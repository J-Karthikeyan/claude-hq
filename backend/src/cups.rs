//! Tournaments (HQ 2.5): a weekly cup per game, a monthly season podium, and
//! trophies on trainer cards.
//!
//!   GET /v1/cups                      this week's cups (top 10 + you), last week's podiums
//!   GET /v1/cups/season?season=YYYY-MM  the season podium (sum of cup points)
//!   GET /v1/cups/trophies/:user_id    a trainer's cup and season trophies ('me' allowed)
//!   GET /v1/cups/:id                  one cup, standings up to 50
//!
//! Nothing here is submitted by a client. A cup is a points race over the
//! `game_results` rows the referees already write (see results.rs): every
//! multiplayer race on the week's cup track/level/course scores points, and
//! the table is computed on read. There is no scheduler either: the schedule is
//! a pure function of the week (`cup_key`), and a finished week is frozen into
//! `cups` / `cup_standings` / `cup_trophies` by the first read after it ends
//! (`finalise_cup`), inside one transaction whose first statement claims the
//! cup row, so two racing readers finalise it exactly once.
//!
//! Points: 1st 10, 2nd 7, 3rd 5, 4th 3, any other finish 1, a DNF 0. Only races
//! with at least two players count. Against farming with a friend (or an alt),
//! at most two rows count per distinct opponent set -- the other user ids in
//! that race -- and then each trainer's best five rows count. Ties go to more
//! wins, then to whoever reached their total first (the earlier last scoring
//! row).
//!
//! Co-op Tower Defense is never a cup: its rows have no places.
//!
//! Privacy: read-only over server-recorded results; the wire carries handles,
//! display names, avatars and numbers, the same as the leaderboards.

use axum::{
    extract::{Path, Query, Request, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use chrono::{Datelike, Duration, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::{Row, SqlitePool};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration as StdDuration, Instant};

use crate::err;
use crate::weeks::{iso_week, week_bounds};

/// The games that run a weekly cup. Tower Defense is co-op (place is null).
pub const CUP_GAMES: [&str; 6] = ["kart", "plat", "golf", "fps", "type", "bowl"];
/// Points for 1st..4th; any other finish scores [`FINISH_POINTS`].
pub const PLACE_POINTS: [i64; 4] = [10, 7, 5, 3];
pub const FINISH_POINTS: i64 = 1;
/// Rows that count per trainer, and per opponent set before that.
pub const BEST_ROWS: usize = 5;
pub const PER_OPPONENT_SET: usize = 2;
/// Standings sizes: the cups list, one cup, and the season table.
pub const TOP_LIST: usize = 10;
pub const TOP_ONE: usize = 50;
pub const TOP_SEASON: usize = 20;
/// How long a live cup's table is reused before it is recomputed.
pub const CACHE_SECS: u64 = 30;
/// Finished weeks a read looks back over to freeze (6 covers a month's gap).
pub const LOOKBACK_WEEKS: i64 = 6;
/// The first season cups can be read for: nothing older is ever frozen, so a
/// request for an ancient week or month cannot make the Arena write rows.
pub const FIRST_SEASON: &str = "2026-01";

/// The cup's display name per game, and how its key reads.
fn game_short(game: &str) -> &'static str {
    match game {
        "kart" => "Kart Cup",
        "plat" => "Platformer Cup",
        "golf" => "Golf Cup",
        "fps" => "Blaster Cup",
        "type" => "Typing Cup",
        "bowl" => "Bowling Cup",
        _ => "Cup",
    }
}

fn game_name(game: &str) -> &'static str {
    match game {
        "kart" => "Kart Racing",
        "plat" => "Platformer Rush",
        "golf" => "Mini Golf",
        "fps" => "Blaster Arena",
        "type" => "Code Typing Race",
        "bowl" => "Bowling",
        _ => "",
    }
}

/// The built-in keys a game's cup may be held on. Never a `c-` user map.
pub fn candidates(game: &str) -> Vec<String> {
    match game {
        "kart" => crate::kart::TRACKS.iter().map(|t| t.id.clone()).collect(),
        "plat" => crate::platformer::LEVELS.iter().map(|l| l.id.clone()).collect(),
        "golf" => crate::valley::golf::COURSES.iter().map(|c| c.id.clone()).collect(),
        "fps" => vec!["match".to_string()],
        "type" => vec!["all".to_string()],
        "bowl" => vec!["f10".to_string()],
        _ => Vec::new(),
    }
}

/// This week's key for a game: sha256("<week>|<game>") picks one candidate,
/// so every Arena agrees on the schedule without storing it.
pub fn cup_key(week: &str, game: &str) -> Option<String> {
    let c = candidates(game);
    if c.is_empty() {
        return None;
    }
    let h = Sha256::digest(format!("{week}|{game}").as_bytes());
    let n = u64::from_be_bytes([h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7]]);
    Some(c[(n % c.len() as u64) as usize].clone())
}

/// What the key is called in the game's own menu.
pub fn key_name(game: &str, key: &str) -> String {
    match game {
        "kart" => crate::kart::track(key).map(|t| t.name.clone()).unwrap_or_else(|| key.to_string()),
        "plat" => crate::platformer::level(key).map(|l| l.name.clone()).unwrap_or_else(|| key.to_string()),
        "golf" => crate::valley::golf::DATA["courses"]
            .as_array()
            .and_then(|cs| cs.iter().find(|c| c["id"].as_str() == Some(key)))
            .and_then(|c| c["name"].as_str())
            .unwrap_or(key)
            .to_string(),
        "fps" => "Courtyard match".to_string(),
        "type" => "Any language".to_string(),
        "bowl" => "10 frames".to_string(),
        _ => key.to_string(),
    }
}

pub fn cup_id(week: &str, game: &str) -> String {
    format!("cup-{week}-{game}")
}

/// A real week ("YYYY-Www" that exists in the ISO calendar).
pub fn valid_week(w: &str) -> bool {
    let b = w.as_bytes();
    if b.len() != 8 || &b[4..6] != b"-W" || !b[..4].iter().chain(&b[6..]).all(u8::is_ascii_digit) {
        return false;
    }
    let (start, _) = week_bounds(w);
    !start.is_empty() && iso_week(&start) == w
}

/// `cup-<week>-<game>` back to its parts, for a cup game and a real week.
pub fn parse_cup_id(id: &str) -> Option<(String, &'static str)> {
    let rest = id.strip_prefix("cup-")?;
    let (week, game) = (rest.get(..8)?, rest.get(8..)?.strip_prefix('-')?);
    let g = CUP_GAMES.iter().find(|g| **g == game)?;
    valid_week(week).then(|| (week.to_string(), *g))
}

/// A real season ("YYYY-MM").
pub fn valid_season(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 7
        && b[4] == b'-'
        && b[..4].iter().chain(&b[5..]).all(u8::is_ascii_digit)
        && matches!(s[5..].parse::<u32>(), Ok(1..=12))
}

/// A week the cups cover: its Sunday falls in [`FIRST_SEASON`] or later.
pub fn in_range(week: &str) -> bool {
    let (_, ends) = week_bounds(week);
    ends.get(..7).is_some_and(|s| s >= FIRST_SEASON)
}

fn day_str(d: NaiveDate) -> String {
    d.format("%Y-%m-%d").to_string()
}

/// The week's first and last UTC day, "YYYY-MM-DD".
fn week_days(week: &str) -> Option<(String, String)> {
    let (s, e) = week_bounds(week);
    Some((s.get(..10)?.to_string(), e.get(..10)?.to_string()))
}

// --- scoring (pure) ------------------------------------------------------

/// One player's line in one race, as `game_results` keeps it.
#[derive(Clone, Debug)]
pub struct RaceRow {
    pub user_id: String,
    pub place: i64,
    pub players: i64,
    pub dnf: bool,
    /// "YYYY-MM-DD HH:MM:SS": a race's rows share it (one insert per game).
    pub at: String,
}

/// A trainer's cup total.
#[derive(Clone, Debug, PartialEq)]
pub struct Score {
    pub user_id: String,
    pub points: i64,
    /// Races entered (players >= 2), counted or not.
    pub races: i64,
    /// 1st places among the counted rows.
    pub wins: i64,
    /// When the last counted row that scored was set; earlier wins a tie.
    pub last_scoring: Option<String>,
    pub place: i64,
}

pub fn points_for(place: i64, dnf: bool) -> i64 {
    if dnf || place < 1 {
        return 0;
    }
    PLACE_POINTS.get((place - 1) as usize).copied().unwrap_or(FINISH_POINTS)
}

/// The points table for a set of race rows (one game and key, one window).
pub fn score(rows: &[RaceRow]) -> Vec<Score> {
    let rows: Vec<&RaceRow> = rows.iter().filter(|r| r.players >= 2).collect();
    // A race is the rows written together: same timestamp, same field size.
    let mut races: HashMap<(&str, i64), Vec<&str>> = HashMap::new();
    for r in &rows {
        races.entry((r.at.as_str(), r.players)).or_default().push(r.user_id.as_str());
    }
    // Per trainer: (points, at, won, opponent set).
    let mut per: BTreeMap<&str, Vec<(i64, &str, bool, String)>> = BTreeMap::new();
    for r in &rows {
        let mut opp: Vec<&str> = races[&(r.at.as_str(), r.players)]
            .iter()
            .copied()
            .filter(|u| *u != r.user_id)
            .collect();
        opp.sort_unstable();
        opp.dedup();
        let pts = points_for(r.place, r.dnf);
        per.entry(r.user_id.as_str()).or_default().push((pts, r.at.as_str(), pts == PLACE_POINTS[0], opp.join(",")));
    }
    // Best first; between equal rows the earlier one, so a total is "reached" early.
    let best = |a: &(i64, &str, bool, String), b: &(i64, &str, bool, String)| b.0.cmp(&a.0).then_with(|| a.1.cmp(b.1));
    let mut out: Vec<Score> = per
        .into_iter()
        .map(|(uid, list)| {
            let races = list.len() as i64;
            let mut by_set: BTreeMap<&str, Vec<&(i64, &str, bool, String)>> = BTreeMap::new();
            for x in &list {
                by_set.entry(x.3.as_str()).or_default().push(x);
            }
            let mut kept: Vec<&(i64, &str, bool, String)> = by_set
                .into_values()
                .flat_map(|mut v| {
                    v.sort_by(|a, b| best(a, b));
                    v.truncate(PER_OPPONENT_SET);
                    v
                })
                .collect();
            kept.sort_by(|a, b| best(a, b));
            kept.truncate(BEST_ROWS);
            Score {
                user_id: uid.to_string(),
                points: kept.iter().map(|x| x.0).sum(),
                races,
                wins: kept.iter().filter(|x| x.2).count() as i64,
                last_scoring: kept.iter().filter(|x| x.0 > 0).map(|x| x.1).max().map(str::to_string),
                place: 0,
            }
        })
        .collect();
    rank(&mut out);
    out
}

/// Sort a table and number it 1..n: points, then wins, then the earlier last
/// scoring row (none sorts last), then the user id so the order is stable.
fn rank(v: &mut [Score]) {
    v.sort_by(|a, b| {
        b.points
            .cmp(&a.points)
            .then(b.wins.cmp(&a.wins))
            .then_with(|| match (&a.last_scoring, &b.last_scoring) {
                (Some(x), Some(y)) => x.cmp(y),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => std::cmp::Ordering::Equal,
            })
            .then_with(|| a.user_id.cmp(&b.user_id))
    });
    for (i, s) in v.iter_mut().enumerate() {
        s.place = i as i64 + 1;
    }
}

fn medal(place: i64) -> &'static str {
    match place {
        1 => "Gold",
        2 => "Silver",
        _ => "Bronze",
    }
}

/// "Gold · Kart Cup · 2026-W41" (fits the 48-character column).
pub fn trophy_label(game: &str, week: &str, place: i64) -> String {
    format!("{} · {} · {}", medal(place), game_short(game), week)
}

pub fn season_label(place: i64) -> &'static str {
    match place {
        1 => "Season champion",
        2 => "Season runner-up",
        _ => "Season third",
    }
}

// --- the database --------------------------------------------------------

/// A trainer as the standings show them.
#[derive(Clone, Debug)]
struct Who {
    handle: String,
    display_name: String,
    avatar_url: String,
}

/// A computed or frozen standing, with the trainer attached.
#[derive(Clone, Debug)]
struct Entrant {
    score: Score,
    who: Who,
}

/// The rows a cup is scored from. Co-op platformer runs have no places worth
/// racing for, so only race-mode rows count there.
async fn fetch_rows(pool: &SqlitePool, game: &str, key: &str, from_day: &str, to_day: &str)
    -> Result<Vec<RaceRow>, sqlx::Error> {
    let rows = sqlx::query(
        r#"SELECT user_id, place, players, value, CAST(extra AS TEXT) AS extra, mode,
                  CAST(at AS TEXT) AS at
           FROM game_results
           WHERE game = ?1 AND "key" = ?2 AND players >= 2
             AND substr(at, 1, 10) >= ?3 AND substr(at, 1, 10) <= ?4
           ORDER BY id"#,
    )
    .bind(game)
    .bind(key)
    .bind(from_day)
    .bind(to_day)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .iter()
        .filter(|r| !(game == "plat" && r.get::<String, _>("mode") == "coop"))
        .map(|r| {
            let extra: Option<String> = r.get("extra");
            let flagged = extra
                .and_then(|e| serde_json::from_str::<Value>(&e).ok())
                .and_then(|e| e.get("dnf").and_then(Value::as_bool))
                .unwrap_or(false);
            RaceRow {
                user_id: r.get("user_id"),
                place: r.get("place"),
                players: r.get("players"),
                dnf: r.get::<Option<i64>, _>("value").is_none() || flagged,
                at: r.get::<Option<String>, _>("at").unwrap_or_default().chars().take(19).collect(),
            }
        })
        .collect())
}

/// Handles and names for active users; deactivated ones are left out.
async fn who_map(pool: &SqlitePool, ids: &[&str]) -> Result<HashMap<String, Who>, sqlx::Error> {
    let mut out = HashMap::new();
    for chunk in ids.chunks(200) {
        let sql = format!(
            "SELECT id, handle, display_name, avatar_url FROM users WHERE is_active = 1 AND id IN ({})",
            vec!["?"; chunk.len()].join(",")
        );
        let mut q = sqlx::query(&sql);
        for id in chunk {
            q = q.bind(*id);
        }
        for r in q.fetch_all(pool).await? {
            out.insert(r.get("id"), Who {
                handle: r.get("handle"),
                display_name: r.get("display_name"),
                avatar_url: r.get("avatar_url"),
            });
        }
    }
    Ok(out)
}

/// A cup's table computed from `game_results` (active trainers only, renumbered).
async fn compute(pool: &SqlitePool, week: &str, game: &str) -> Result<Vec<Entrant>, sqlx::Error> {
    let (Some(key), Some((from, to))) = (cup_key(week, game), week_days(week)) else {
        return Ok(Vec::new());
    };
    let scores = score(&fetch_rows(pool, game, &key, &from, &to).await?);
    let ids: Vec<&str> = scores.iter().map(|s| s.user_id.as_str()).collect();
    let who = who_map(pool, &ids).await?;
    let mut out: Vec<Entrant> = scores
        .into_iter()
        .filter_map(|s| who.get(&s.user_id).cloned().map(|w| Entrant { score: s, who: w }))
        .collect();
    for (i, e) in out.iter_mut().enumerate() {
        e.score.place = i as i64 + 1;
    }
    Ok(out)
}

type Cached = (Instant, Arc<Vec<Entrant>>);
static LIVE: LazyLock<Mutex<HashMap<String, Cached>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// A live cup's table, reused for [`CACHE_SECS`]. The cache is keyed by the
/// database file as well, so two pools in one process never share a table.
async fn live(pool: &SqlitePool, week: &str, game: &str) -> Result<Arc<Vec<Entrant>>, sqlx::Error> {
    let id = format!("{}|{}", pool.connect_options().get_filename().display(), cup_id(week, game));
    if let Some((at, v)) = LIVE.lock().map(|m| m.get(&id).cloned()).ok().flatten() {
        if at.elapsed() < StdDuration::from_secs(CACHE_SECS) {
            return Ok(v);
        }
    }
    let v = Arc::new(compute(pool, week, game).await?);
    if let Ok(mut m) = LIVE.lock() {
        m.retain(|_, (at, _)| at.elapsed() < StdDuration::from_secs(CACHE_SECS));
        m.insert(id, (Instant::now(), v.clone()));
    }
    Ok(v)
}

/// Freeze a finished week's cup: the cup row, every standing, and trophies for
/// the podium (only with two or more entrants, and only for a scoring place).
/// True when this call did it; false when the cup was already final.
///
/// The table is computed first, outside the transaction; the transaction's
/// first statement then claims the cup row, which takes SQLite's write lock,
/// so a second reader waits and finds the row there.
pub async fn finalise_cup(pool: &SqlitePool, week: &str, game: &str) -> Result<bool, sqlx::Error> {
    let Some(key) = cup_key(week, game) else { return Ok(false) };
    let (starts, ends) = week_bounds(week);
    if starts.is_empty() {
        return Ok(false);
    }
    let table = compute(pool, week, game).await?;
    let id = cup_id(week, game);
    let season: String = ends.chars().take(7).collect();
    let mut tx = pool.begin().await?;
    let claimed = sqlx::query(
        "INSERT OR IGNORE INTO cups (id, week, game, key, format, starts_at, ends_at, status)
         VALUES (?1, ?2, ?3, ?4, 'points', ?5, ?6, 'final')",
    )
    .bind(&id)
    .bind(week)
    .bind(game)
    .bind(&key)
    .bind(&starts)
    .bind(&ends)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if claimed == 0 {
        tx.rollback().await?;
        return Ok(false);
    }
    for e in &table {
        sqlx::query(
            "INSERT OR IGNORE INTO cup_standings (cup_id, user_id, points, races, place)
             VALUES (?1, ?2, ?3, ?4, ?5)",
        )
        .bind(&id)
        .bind(&e.score.user_id)
        .bind(e.score.points)
        .bind(e.score.races)
        .bind(e.score.place)
        .execute(&mut *tx)
        .await?;
    }
    if table.len() >= 2 {
        for e in table.iter().take(3).filter(|e| e.score.points > 0) {
            sqlx::query(
                "INSERT OR IGNORE INTO cup_trophies (user_id, cup_id, place, label, season)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
            )
            .bind(&e.score.user_id)
            .bind(&id)
            .bind(e.score.place)
            .bind(trophy_label(game, week, e.score.place))
            .bind(&season)
            .execute(&mut *tx)
            .await?;
        }
    }
    tx.commit().await?;
    Ok(true)
}

/// Freeze every cup of the given (finished) weeks that is not final yet.
async fn finalise_weeks(pool: &SqlitePool, weeks: &[String]) -> Result<(), sqlx::Error> {
    let ids: Vec<(String, &str)> = weeks
        .iter()
        .flat_map(|w| CUP_GAMES.iter().map(move |g| (w.clone(), *g)))
        .collect();
    if ids.is_empty() {
        return Ok(());
    }
    let sql = format!("SELECT id FROM cups WHERE id IN ({})", vec!["?"; ids.len()].join(","));
    let mut q = sqlx::query_scalar::<_, String>(&sql);
    for (w, g) in &ids {
        q = q.bind(cup_id(w, g));
    }
    let done: HashSet<String> = q.fetch_all(pool).await?.into_iter().collect();
    for (w, g) in &ids {
        if !done.contains(&cup_id(w, g)) {
            finalise_cup(pool, w, g).await?;
        }
    }
    Ok(())
}

/// The finished weeks a read on `today` looks back over.
fn recent_weeks(today: NaiveDate) -> Vec<String> {
    (1..=LOOKBACK_WEEKS)
        .map(|k| iso_week(&day_str(today - Duration::days(7 * k))))
        .filter(|w| in_range(w))
        .collect()
}

/// The weeks whose Sunday (so whose cups) fall in a season.
fn season_weeks(season: &str) -> Vec<String> {
    let Some(first) = NaiveDate::parse_from_str(&format!("{season}-01"), "%Y-%m-%d").ok() else {
        return Vec::new();
    };
    (0..31)
        .map(|i| first + Duration::days(i))
        .filter(|d| d.month() == first.month() && d.weekday() == chrono::Weekday::Sun)
        .map(|d| iso_week(&day_str(d)))
        .collect()
}

fn season_of(day: NaiveDate) -> String {
    day.format("%Y-%m").to_string()
}

fn prev_season(today: NaiveDate) -> String {
    let first = today.with_day(1).unwrap_or(today);
    season_of(first - Duration::days(1))
}

/// A season's table: the sum of its finished cups' points, plus the live
/// cups of the current week when they end inside it.
async fn season_table(pool: &SqlitePool, season: &str, today: NaiveDate) -> Result<(Vec<Entrant>, i64), sqlx::Error> {
    let rows = sqlx::query(
        "SELECT s.user_id, s.points, s.place FROM cup_standings s JOIN cups c ON c.id = s.cup_id
         WHERE substr(c.ends_at, 1, 7) = ?1 AND c.id LIKE 'cup-%'",
    )
    .bind(season)
    .fetch_all(pool)
    .await?;
    let mut cups: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM cups WHERE substr(ends_at, 1, 7) = ?1 AND id LIKE 'cup-%'",
    )
    .bind(season)
    .fetch_one(pool)
    .await?;
    // Per trainer: (points, cup wins, cups entered).
    let mut tot: HashMap<String, (i64, i64, i64)> = HashMap::new();
    for r in &rows {
        let e = tot.entry(r.get("user_id")).or_default();
        let pts = r.get::<i64, _>("points");
        e.0 += pts;
        e.1 += (r.get::<i64, _>("place") == 1 && pts > 0) as i64;
        e.2 += 1;
    }
    let week = iso_week(&day_str(today));
    let (_, ends) = week_bounds(&week);
    if ends.get(..7) == Some(season) {
        for g in CUP_GAMES {
            let t = live(pool, &week, g).await?;
            cups += 1;
            for e in t.iter() {
                let x = tot.entry(e.score.user_id.clone()).or_default();
                x.0 += e.score.points;
                x.1 += (e.score.place == 1 && e.score.points > 0) as i64;
                x.2 += 1;
            }
        }
    }
    let mut scores: Vec<Score> = tot
        .into_iter()
        .map(|(u, (p, w, n))| Score { user_id: u, points: p, races: n, wins: w, last_scoring: None, place: 0 })
        .collect();
    rank(&mut scores);
    let ids: Vec<&str> = scores.iter().map(|s| s.user_id.as_str()).collect();
    let who = who_map(pool, &ids).await?;
    let mut out: Vec<Entrant> = scores
        .into_iter()
        .filter_map(|s| who.get(&s.user_id).cloned().map(|w| Entrant { score: s, who: w }))
        .collect();
    for (i, e) in out.iter_mut().enumerate() {
        e.score.place = i as i64 + 1;
    }
    Ok((out, cups))
}

/// Award a finished season's podium, exactly once: every cup ending in it is
/// frozen first, then one transaction claims a 'season-YYYY-MM' marker row in
/// `cups` (so later reads stop at one lookup) and gives the top three with
/// points their 'season-YYYY-MM' trophies, when at least two trainers scored.
/// True when this call did it.
pub async fn finalise_season(pool: &SqlitePool, season: &str, today: NaiveDate) -> Result<bool, sqlx::Error> {
    if !valid_season(season) || season >= season_of(today).as_str() || season < FIRST_SEASON {
        return Ok(false);
    }
    let marker = format!("season-{season}");
    let done: Option<i64> = sqlx::query_scalar("SELECT 1 FROM cups WHERE id = ?1")
        .bind(&marker)
        .fetch_optional(pool)
        .await?;
    if done.is_some() {
        return Ok(false);
    }
    let weeks = season_weeks(season);
    finalise_weeks(pool, &weeks).await?;
    let (table, _) = season_table(pool, season, today).await?;
    let scored: Vec<&Entrant> = table.iter().filter(|e| e.score.points > 0).collect();
    let starts = weeks.first().map(|w| week_bounds(w).0).unwrap_or_default();
    let ends = weeks.last().map(|w| week_bounds(w).1).unwrap_or_default();
    let mut tx = pool.begin().await?;
    let claimed = sqlx::query(
        "INSERT OR IGNORE INTO cups (id, week, game, key, format, starts_at, ends_at, status)
         VALUES (?1, '', 'season', ?2, 'season', ?3, ?4, 'final')",
    )
    .bind(&marker)
    .bind(season)
    .bind(&starts)
    .bind(&ends)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if claimed == 0 {
        tx.rollback().await?;
        return Ok(false);
    }
    if scored.len() >= 2 {
        for e in scored.iter().take(3) {
            sqlx::query(
                "INSERT OR IGNORE INTO cup_trophies (user_id, cup_id, place, label, season)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
            )
            .bind(&e.score.user_id)
            .bind(&marker)
            .bind(e.score.place)
            .bind(season_label(e.score.place))
            .bind(season)
            .execute(&mut *tx)
            .await?;
        }
    }
    tx.commit().await?;
    Ok(true)
}

/// What every read does first: freeze finished weeks and last month's season.
pub async fn catch_up(pool: &SqlitePool, today: NaiveDate) -> Result<(), sqlx::Error> {
    finalise_weeks(pool, &recent_weeks(today)).await?;
    finalise_season(pool, &prev_season(today), today).await.map(|_| ())
}

/// A frozen cup's standings (active trainers only, in their frozen order).
async fn frozen(pool: &SqlitePool, id: &str) -> Result<Vec<Entrant>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT s.user_id, s.points, s.races, s.place, u.handle, u.display_name, u.avatar_url
         FROM cup_standings s JOIN users u ON u.id = s.user_id AND u.is_active = 1
         WHERE s.cup_id = ?1 ORDER BY s.place",
    )
    .bind(id)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .iter()
        .map(|r| Entrant {
            score: Score {
                user_id: r.get("user_id"),
                points: r.get("points"),
                races: r.get("races"),
                wins: 0,
                last_scoring: None,
                place: r.get("place"),
            },
            who: Who {
                handle: r.get("handle"),
                display_name: r.get("display_name"),
                avatar_url: r.get("avatar_url"),
            },
        })
        .collect())
}

// --- the wire ------------------------------------------------------------

#[derive(Serialize, Clone, Debug)]
pub struct StandingView {
    place: i64,
    #[serde(rename = "userId")]
    user_id: String,
    handle: String,
    #[serde(rename = "displayName")]
    display_name: String,
    #[serde(rename = "avatarUrl")]
    avatar_url: String,
    points: i64,
    races: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    wins: Option<i64>,
    #[serde(rename = "isYou")]
    is_you: bool,
}

#[derive(Serialize, Debug)]
pub struct CupView {
    id: String,
    week: String,
    game: String,
    #[serde(rename = "gameName")]
    game_name: String,
    key: String,
    name: String,
    #[serde(rename = "keyName")]
    key_name: String,
    format: String,
    status: String,
    #[serde(rename = "startsAt")]
    starts_at: String,
    #[serde(rename = "endsAt")]
    ends_at: String,
    entrants: usize,
    standings: Vec<StandingView>,
    you: Option<StandingView>,
}

#[derive(Serialize, Debug)]
pub struct CupsView {
    week: String,
    #[serde(rename = "startsAt")]
    starts_at: String,
    #[serde(rename = "endsAt")]
    ends_at: String,
    season: String,
    cups: Vec<CupView>,
    #[serde(rename = "lastWeek")]
    last_week: Vec<CupView>,
}

#[derive(Serialize, Debug)]
pub struct SeasonView {
    season: String,
    status: String,
    cups: i64,
    podium: Vec<StandingView>,
    standings: Vec<StandingView>,
    you: Option<StandingView>,
}

#[derive(Serialize, Debug)]
pub struct TrophyView {
    #[serde(rename = "cupId")]
    cup_id: String,
    kind: String,
    game: Option<String>,
    place: i64,
    label: String,
    season: String,
    at: String,
}

#[derive(Serialize, Debug)]
pub struct TrophiesView {
    #[serde(rename = "userId")]
    user_id: String,
    trophies: Vec<TrophyView>,
    gold: i64,
    silver: i64,
    bronze: i64,
    seasons: i64,
}

fn view(e: &Entrant, me: &str, live: bool) -> StandingView {
    StandingView {
        place: e.score.place,
        user_id: e.score.user_id.clone(),
        handle: e.who.handle.clone(),
        display_name: e.who.display_name.clone(),
        avatar_url: e.who.avatar_url.clone(),
        points: e.score.points,
        races: e.score.races,
        wins: live.then_some(e.score.wins),
        is_you: e.score.user_id == me,
    }
}

/// One cup's view: live this week, frozen once it is over.
pub async fn cup_view(pool: &SqlitePool, week: &str, game: &str, me: &str, top: usize, today: NaiveDate)
    -> Result<CupView, sqlx::Error> {
    let this_week = iso_week(&day_str(today));
    let (starts, ends) = week_bounds(week);
    let id = cup_id(week, game);
    let is_live = week >= this_week.as_str();
    let (table, key) = if is_live {
        (live(pool, week, game).await?, cup_key(week, game).unwrap_or_default())
    } else {
        if in_range(week) {
            finalise_weeks(pool, &[week.to_string()]).await?;
        }
        let key: Option<String> = sqlx::query_scalar("SELECT key FROM cups WHERE id = ?1")
            .bind(&id)
            .fetch_optional(pool)
            .await?;
        (Arc::new(frozen(pool, &id).await?), key.unwrap_or_else(|| cup_key(week, game).unwrap_or_default()))
    };
    let kname = key_name(game, &key);
    Ok(CupView {
        name: format!("{} · {}", game_short(game), kname),
        key_name: kname,
        id,
        week: week.to_string(),
        game: game.to_string(),
        game_name: game_name(game).to_string(),
        key,
        format: "points".to_string(),
        status: if is_live { "live" } else { "final" }.to_string(),
        starts_at: starts,
        ends_at: ends,
        entrants: table.len(),
        standings: table.iter().take(top).map(|e| view(e, me, is_live)).collect(),
        you: table.iter().find(|e| e.score.user_id == me).map(|e| view(e, me, is_live)),
    })
}

pub async fn cups_view(pool: &SqlitePool, me: &str, today: NaiveDate) -> Result<CupsView, sqlx::Error> {
    catch_up(pool, today).await?;
    let week = iso_week(&day_str(today));
    let last = iso_week(&day_str(today - Duration::days(7)));
    let (starts, ends) = week_bounds(&week);
    let mut cups = Vec::new();
    let mut last_week = Vec::new();
    for g in CUP_GAMES {
        cups.push(cup_view(pool, &week, g, me, TOP_LIST, today).await?);
        last_week.push(cup_view(pool, &last, g, me, 3, today).await?);
    }
    Ok(CupsView { season: ends.chars().take(7).collect(), week, starts_at: starts, ends_at: ends, cups, last_week })
}

pub async fn season_view(pool: &SqlitePool, season: &str, me: &str, today: NaiveDate) -> Result<SeasonView, sqlx::Error> {
    catch_up(pool, today).await?;
    let current = season_of(today);
    if season < current.as_str() {
        finalise_season(pool, season, today).await?;
    }
    let (table, cups) = season_table(pool, season, today).await?;
    Ok(SeasonView {
        season: season.to_string(),
        status: if season < current.as_str() { "final" } else { "live" }.to_string(),
        cups,
        podium: table.iter().filter(|e| e.score.points > 0).take(3).map(|e| view(e, me, true)).collect(),
        standings: table.iter().take(TOP_SEASON).map(|e| view(e, me, true)).collect(),
        you: table.iter().find(|e| e.score.user_id == me).map(|e| view(e, me, true)),
    })
}

/// A trainer's trophies, newest first. None for an unknown or inactive user.
pub async fn trophies_view(pool: &SqlitePool, user_id: &str, today: NaiveDate) -> Result<Option<TrophiesView>, sqlx::Error> {
    let known: Option<i64> = sqlx::query_scalar("SELECT 1 FROM users WHERE id = ?1 AND is_active = 1")
        .bind(user_id)
        .fetch_optional(pool)
        .await?;
    if known.is_none() {
        return Ok(None);
    }
    catch_up(pool, today).await?;
    let rows = sqlx::query(
        "SELECT cup_id, place, label, season, CAST(at AS TEXT) AS at FROM cup_trophies
         WHERE user_id = ?1 ORDER BY season DESC, at DESC, cup_id DESC LIMIT 100",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await?;
    let trophies: Vec<TrophyView> = rows
        .iter()
        .map(|r| {
            let cup: String = r.get("cup_id");
            let game = parse_cup_id(&cup).map(|(_, g)| g.to_string());
            TrophyView {
                kind: if cup.starts_with("season-") { "season" } else { "cup" }.to_string(),
                game,
                cup_id: cup,
                place: r.get("place"),
                label: r.get("label"),
                season: r.get("season"),
                at: r.get::<Option<String>, _>("at").unwrap_or_default(),
            }
        })
        .collect();
    let count = |kind: &str, p: i64| trophies.iter().filter(|t| t.kind == kind && t.place == p).count() as i64;
    Ok(Some(TrophiesView {
        user_id: user_id.to_string(),
        gold: count("cup", 1),
        silver: count("cup", 2),
        bronze: count("cup", 3),
        seasons: trophies.iter().filter(|t| t.kind == "season").count() as i64,
        trophies,
    }))
}

// --- routes --------------------------------------------------------------

fn today() -> NaiveDate {
    Utc::now().date_naive()
}

fn caller(req: &Request) -> Option<crate::Caller> {
    req.extensions().get::<crate::Caller>().cloned()
}

fn db_err(e: sqlx::Error) -> Response {
    tracing::warn!("cups: {e}");
    err(StatusCode::INTERNAL_SERVER_ERROR, "db error")
}

async fn list(State(st): State<crate::AppState>, req: Request) -> Response {
    let Some(c) = caller(&req) else { return err(StatusCode::UNAUTHORIZED, "missing bearer token") };
    match cups_view(&st.pool, &c.user_id, today()).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => db_err(e),
    }
}

async fn one(State(st): State<crate::AppState>, Path(id): Path<String>, req: Request) -> Response {
    let Some(c) = caller(&req) else { return err(StatusCode::UNAUTHORIZED, "missing bearer token") };
    let Some((week, game)) = parse_cup_id(&id) else { return err(StatusCode::NOT_FOUND, "no such cup") };
    let t = today();
    if week > iso_week(&day_str(t)) || !in_range(&week) {
        return err(StatusCode::NOT_FOUND, "no such cup");
    }
    if let Err(e) = catch_up(&st.pool, t).await {
        return db_err(e);
    }
    match cup_view(&st.pool, &week, game, &c.user_id, TOP_ONE, t).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => db_err(e),
    }
}

#[derive(Deserialize)]
struct SeasonQ {
    #[serde(default)]
    season: Option<String>,
}

async fn season(State(st): State<crate::AppState>, Query(q): Query<SeasonQ>, req: Request) -> Response {
    let Some(c) = caller(&req) else { return err(StatusCode::UNAUTHORIZED, "missing bearer token") };
    let t = today();
    let s = q.season.filter(|s| !s.is_empty()).unwrap_or_else(|| season_of(t));
    if !valid_season(&s) {
        return err(StatusCode::UNPROCESSABLE_ENTITY, "season must be YYYY-MM");
    }
    if s > season_of(t) || s.as_str() < FIRST_SEASON {
        return err(StatusCode::NOT_FOUND, "no such season");
    }
    match season_view(&st.pool, &s, &c.user_id, t).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => db_err(e),
    }
}

async fn trophies(State(st): State<crate::AppState>, Path(user_id): Path<String>, req: Request) -> Response {
    let Some(c) = caller(&req) else { return err(StatusCode::UNAUTHORIZED, "missing bearer token") };
    let uid = if user_id == "me" { c.user_id.clone() } else { user_id.chars().take(36).collect() };
    match trophies_view(&st.pool, &uid, today()).await {
        Ok(Some(v)) => Json(v).into_response(),
        Ok(None) => err(StatusCode::NOT_FOUND, "no such trainer"),
        Err(e) => db_err(e),
    }
}

/// Only this group's routes; main.rs puts them behind `require_device`.
pub fn routes() -> Router<crate::AppState> {
    Router::new()
        .route("/v1/cups", get(list))
        .route("/v1/cups/season", get(season))
        .route("/v1/cups/trophies/:user_id", get(trophies))
        .route("/v1/cups/:id", get(one))
}

#[cfg(test)]
mod tests;
