//! World boss (HQ 2.5): one Arena-wide boss per ISO week (UTC).
//!
//! Everyone's merged PRs (`work_events` pr_merged, minted by loot) and
//! multiplayer wins (`game_results` place 1 with 2+ players) drain its HP down
//! to a floor of 15%. From there it is a fight: each player gets 3 attempts a
//! UTC day to send a team of up to six at it, and whoever takes the last HP
//! lands the final blow.
//!
//! HP is never stored; it is computed on every read:
//!
//! ```text
//! drain = Σ_user,day min(prs, 5) × 150  +  Σ_user,day min(wins, 10) × 50
//! hp    = max(0, max_hp − min(drain, floor(0.85·max_hp)) − Σ fights.dmg)
//! ```
//!
//! Trust: the team specs are TRUST-THE-CLIENT (the server cannot see anyone's
//! Pokédex). `clean_spec` clamps them to legal values, and the damage a fight
//! can do is capped at 5% of max HP, three fights a day, so the floor of 15%
//! takes at least three fights from three player-days to clear.
//!
//! Wire: only counts leave the player's machine. A fight carries team specs
//! ({sp, st, br, mg, sh}); nothing here reads or returns a name a player typed.

use crate::valley::pokebattle::{
    self as pb, alive, auto_act, build_mon, calc_stat, clean_spec, needs_replace, new_battle,
    resolve_turn, view_mon, Mon, Spec, DATA,
};
use crate::weeks::{iso_week, week_bounds};
use crate::{err, AppState, Caller};
use axum::{
    extract::{Path, Request, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use chrono::{DateTime, Duration, Utc};
use rand::{rngs::StdRng, Rng, SeedableRng};
use serde::Serialize;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use sqlx::{SqliteConnection, SqlitePool};
use std::collections::HashMap;

/// Beedrill, Scyther, Scizor, Heracross: bugs, for the week the bugs won.
pub const BOSS_DEX: [i64; 4] = [15, 123, 212, 214];
/// max_hp = HP_PER_PLAYER × max(MIN_PLAYERS, last week's active players).
const HP_PER_PLAYER: i64 = 2000;
const MIN_PLAYERS: i64 = 5;
/// Each merged PR drains 150, at most 5 a player a day.
const PR_UNIT: i64 = 150;
const PR_DAY_CAP: i64 = 5;
/// Each multiplayer win drains 50, at most 10 a player a day.
const WIN_UNIT: i64 = 50;
const WIN_DAY_CAP: i64 = 10;
/// Drain alone stops at 85% (the boss keeps 15% for the fight).
const DRAIN_MAX_PCT: i64 = 85;
/// At or below this % of max HP, the boss can be fought.
const FIGHT_AT_PCT: i64 = 15;
const FIGHTS_PER_DAY: i64 = 3;
const TEAM_MAX: usize = 6;
const MAX_TURNS: usize = 60;
const BOSS_LVL: i64 = 70;
/// The battle data's stage-4 level, the base the boss's stats scale from.
const STAGE4_LVL: i64 = 55;
const TOP_N: usize = 10;

const TS: &str = "%Y-%m-%d %H:%M:%S%.6f";

// ------------------------------------------------------------- the boss --

/// The bosses this build can field: [`BOSS_DEX`] filtered to species the battle
/// data has, so a data file without one never picks a boss it cannot build.
fn bosses() -> Vec<i64> {
    BOSS_DEX.iter().copied().filter(|d| DATA.by_dex.contains_key(&d.to_string())).collect()
}

fn sha(s: &str) -> [u8; 32] {
    Sha256::digest(s.as_bytes()).into()
}

/// sha256(week) % len: the same boss for everyone, all week.
fn pick_boss(week: &str) -> i64 {
    let list = bosses();
    if list.is_empty() {
        return BOSS_DEX[0];
    }
    let h = sha(week);
    let n = u64::from_be_bytes(h[..8].try_into().expect("8 bytes"));
    list[(n % list.len() as u64) as usize]
}

/// The boss as a battler: `build_mon` at stage 4 on its evolution line, then
/// its identity and stats overridden to the boss species at level 70 (stats
/// ×70/55, hp ×4). No new constructor: every field set here is pub on [`Mon`].
fn boss_mon(dex: i64) -> Option<Mon> {
    let key = dex.to_string();
    let pid = DATA.by_dex.get(&key)?;
    let sp = DATA.lines.iter().position(|l| l.contains(&dex))?;
    let br = DATA.branches.get(&sp.to_string()).filter(|o| o.contains(&dex)).map(|_| dex);
    let mut m = build_mon(&Spec { sp, st: 4, br, mg: None, sh: false, name: String::new() });
    let e = DATA.pokemon.get(pid)?;
    let scale = |base: i64, hp: bool| calc_stat(base, STAGE4_LVL, hp) * BOSS_LVL / STAGE4_LVL;
    m.name = e.name.clone();
    m.id = pid.clone();
    m.dex = dex;
    m.sprite = e.sprite;
    m.mega = None;
    m.types = e.types.clone();
    m.lvl = BOSS_LVL;
    m.stats.atk = scale(e.bs.atk, false);
    m.stats.def = scale(e.bs.def, false);
    m.stats.spa = scale(e.bs.spa, false);
    m.stats.spd = scale(e.bs.spd, false);
    m.stats.spe = scale(e.bs.spe, false);
    m.stats.hp = 4 * scale(e.bs.hp, true);
    m.hp = m.stats.hp;
    m.max = m.stats.hp;
    if let Some(mv) = e.moves.get(&STAGE4_LVL.to_string()) {
        m.moves = mv
            .iter()
            .map(|id| {
                let pp = pb::move_of(id).pp;
                pb::Slot { id: id.clone(), pp, max: pp }
            })
            .collect();
    }
    Some(m)
}

/// The damage one fight does: the share of the boss battler's HP the team took,
/// scaled so a full knockout is exactly max_hp / 20 (5%), and never more.
fn fight_damage(lost: i64, boss_max: i64, max_hp: i64) -> i64 {
    if boss_max <= 0 || max_hp <= 0 {
        return 0;
    }
    let lost = lost.clamp(0, boss_max);
    (lost * max_hp / (boss_max * 20)).min(max_hp / 20)
}

/// The battle a fight replays. Deterministic: rng = sha256(week|user|requestId),
/// both sides on `auto_act`, at most [`MAX_TURNS`] turns. A fainted battler is
/// replaced by the first one still standing.
struct Fight {
    /// HP the boss battler lost.
    lost: i64,
    boss_max: i64,
    log: Vec<Value>,
    team: Vec<Value>,
    boss: Value,
}

fn simulate(week: &str, uid: &str, rid: &str, dex: i64, team: &[Spec]) -> Option<Fight> {
    let boss = boss_mon(dex)?;
    let mons: Vec<Mon> = team.iter().map(build_mon).collect();
    if mons.is_empty() {
        return None;
    }
    let team_view = mons.iter().map(view_mon).collect();
    let boss_view = view_mon(&boss);
    let boss_max = boss.max;
    let mut st = new_battle(mons, vec![boss]);
    let mut rng = StdRng::from_seed(sha(&format!("{week}|{uid}|{rid}")));
    let mut draw = || rng.gen::<f64>();
    let mut log = Vec::new();
    for _ in 0..MAX_TURNS {
        if st.over {
            break;
        }
        let (a, b) = (auto_act(&st, 0), auto_act(&st, 1));
        log.extend(resolve_turn(&mut st, a, b, &mut draw));
        for side in needs_replace(&st) {
            let to = alive(&st, side)[0];
            log.extend(pb::replace(&mut st, side, to));
        }
    }
    let left = st.sides[1].team[0].hp.max(0);
    Some(Fight { lost: boss_max - left, boss_max, log, team: team_view, boss: boss_view })
}

// ----------------------------------------------------------- the ledger --

fn now_ts(now: DateTime<Utc>) -> String {
    now.format(TS).to_string()
}

fn sql_err(e: sqlx::Error) -> Response {
    err(StatusCode::INTERNAL_SERVER_ERROR, &format!("db error: {e}"))
}

/// The week's row, created on first read. max_hp counts the players active
/// (any game result or work event) in the PREVIOUS week, at least five.
async fn ensure_week(conn: &mut SqliteConnection, week: &str) -> Result<WeekRow, sqlx::Error> {
    if let Some(r) = week_row(conn, week).await? {
        return Ok(r);
    }
    let (start, _) = week_bounds(week);
    let prev = start
        .get(..10)
        .and_then(|d| chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").ok())
        .map(|d| iso_week(&(d - Duration::days(1)).format("%Y-%m-%d").to_string()))
        .unwrap_or_default();
    let (ps, pe) = week_bounds(&prev);
    let players: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM (
           SELECT user_id FROM game_results WHERE substr(at, 1, 10) BETWEEN ?1 AND ?2
           UNION SELECT user_id FROM work_events WHERE day BETWEEN ?1 AND ?2)",
    )
    // UTC days, not the bound strings: a CURRENT_TIMESTAMP row has no
    // fraction, so "… 00:00:00" would sort before "… 00:00:00.000000".
    .bind(ps.get(..10).unwrap_or(""))
    .bind(pe.get(..10).unwrap_or(""))
    .fetch_one(&mut *conn)
    .await?;
    let max_hp = HP_PER_PLAYER * players.max(MIN_PLAYERS);
    sqlx::query("INSERT OR IGNORE INTO boss_weeks (week, boss_dex, max_hp) VALUES (?1, ?2, ?3)")
        .bind(week)
        .bind(pick_boss(week))
        .bind(max_hp)
        .execute(&mut *conn)
        .await?;
    // Whoever won the INSERT race, everyone reads the one stored row.
    Ok(week_row(conn, week).await?.expect("row inserted or already there"))
}

#[derive(Clone, Debug)]
struct WeekRow {
    week: String,
    dex: i64,
    max_hp: i64,
    defeated: bool,
    final_blow: Option<String>,
}

async fn week_row(conn: &mut SqliteConnection, week: &str) -> Result<Option<WeekRow>, sqlx::Error> {
    let r: Option<(String, i64, i64, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT week, boss_dex, max_hp, defeated_at, final_blow_user FROM boss_weeks WHERE week = ?1",
    )
    .bind(week)
    .fetch_optional(&mut *conn)
    .await?;
    Ok(r.map(|(week, dex, max_hp, d, f)| WeekRow { week, dex, max_hp, defeated: d.is_some(), final_blow: f }))
}

/// Per-player damage by source, for one week.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
struct Sources {
    pr: i64,
    win: i64,
    fight: i64,
}

impl Sources {
    fn total(&self) -> i64 {
        self.pr + self.win + self.fight
    }
}

struct Standing {
    hp: i64,
    by_user: HashMap<String, Sources>,
}

/// Everything the HP formula reads, for one week.
async fn standing(conn: &mut SqliteConnection, w: &WeekRow) -> Result<Standing, sqlx::Error> {
    let (start, end) = week_bounds(&w.week);
    let (d0, d1) = (start.get(..10).unwrap_or(""), end.get(..10).unwrap_or(""));
    let mut by: HashMap<String, Sources> = HashMap::new();

    let prs: Vec<(String, i64)> = sqlx::query_as(
        "SELECT user_id, SUM(n) FROM work_events
         WHERE type = 'pr_merged' AND day >= ?1 AND day <= ?2 GROUP BY user_id, day",
    )
    .bind(d0)
    .bind(d1)
    .fetch_all(&mut *conn)
    .await?;
    for (u, n) in prs {
        by.entry(u).or_default().pr += n.clamp(0, PR_DAY_CAP) * PR_UNIT;
    }

    let wins: Vec<(String, i64)> = sqlx::query_as(
        "SELECT user_id, COUNT(*) FROM game_results
         WHERE place = 1 AND players >= 2 AND substr(at, 1, 10) BETWEEN ?1 AND ?2
         GROUP BY user_id, substr(at, 1, 10)",
    )
    .bind(d0)
    .bind(d1)
    .fetch_all(&mut *conn)
    .await?;
    for (u, n) in wins {
        by.entry(u).or_default().win += n.clamp(0, WIN_DAY_CAP) * WIN_UNIT;
    }

    let fights: Vec<(String, i64)> =
        sqlx::query_as("SELECT user_id, SUM(dmg) FROM boss_fights WHERE week = ?1 GROUP BY user_id")
            .bind(&w.week)
            .fetch_all(&mut *conn)
            .await?;
    for (u, n) in fights {
        by.entry(u).or_default().fight += n.max(0);
    }

    let drain: i64 = by.values().map(|s| s.pr + s.win).sum();
    let fought: i64 = by.values().map(|s| s.fight).sum();
    Ok(Standing { hp: hp_of(w.max_hp, drain, fought), by_user: by })
}

fn hp_of(max_hp: i64, drain: i64, fought: i64) -> i64 {
    (max_hp - drain.min(max_hp * DRAIN_MAX_PCT / 100) - fought).max(0)
}

fn phase_of(w: &WeekRow, hp: i64) -> &'static str {
    if w.defeated {
        "down"
    } else if hp * 100 <= w.max_hp * FIGHT_AT_PCT {
        "fight"
    } else {
        "drain"
    }
}

async fn fights_today(conn: &mut SqliteConnection, uid: &str, day: &str) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar("SELECT COUNT(*) FROM boss_fights WHERE user_id = ?1 AND day = ?2")
        .bind(uid)
        .bind(day)
        .fetch_one(&mut *conn)
        .await
}

// ----------------------------------------------------------- the views --

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct Who {
    user_id: String,
    handle: String,
    display_name: String,
    avatar_url: String,
}

async fn who(conn: &mut SqliteConnection, ids: &[String]) -> Result<HashMap<String, Who>, sqlx::Error> {
    let mut out = HashMap::new();
    for id in ids {
        let r: Option<(String, String, String)> = sqlx::query_as(
            "SELECT handle, display_name, avatar_url FROM users WHERE id = ?1 AND is_active = 1",
        )
        .bind(id)
        .fetch_optional(&mut *conn)
        .await?;
        if let Some((handle, display_name, avatar_url)) = r {
            out.insert(id.clone(), Who { user_id: id.clone(), handle, display_name, avatar_url });
        }
    }
    Ok(out)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BossView {
    dex: i64,
    name: String,
    types: Vec<String>,
    max_hp: i64,
    hp: i64,
    phase: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct YouView {
    dmg: i64,
    sources: Sources,
    attempts_left: i64,
    rank: Option<usize>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TopRow {
    #[serde(flatten)]
    who: Who,
    dmg: i64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LastWeek {
    week: String,
    dex: i64,
    name: String,
    defeated: bool,
    final_blow: Option<Who>,
}

#[derive(Serialize)]
struct Rules {
    #[serde(rename = "prUnit")]
    pr_unit: i64,
    #[serde(rename = "prDay")]
    pr_day: i64,
    #[serde(rename = "winUnit")]
    win_unit: i64,
    #[serde(rename = "winDay")]
    win_day: i64,
    #[serde(rename = "fightAt")]
    fight_at: i64,
    #[serde(rename = "fightsPerDay")]
    fights_per_day: i64,
    #[serde(rename = "fightCap")]
    fight_cap: i64,
    #[serde(rename = "teamMax")]
    team_max: usize,
}

const RULES: Rules = Rules {
    pr_unit: PR_UNIT,
    pr_day: PR_DAY_CAP,
    win_unit: WIN_UNIT,
    win_day: WIN_DAY_CAP,
    fight_at: FIGHT_AT_PCT,
    fights_per_day: FIGHTS_PER_DAY,
    fight_cap: 5,
    team_max: TEAM_MAX,
};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BossState {
    week: String,
    ends_at: String,
    boss: BossView,
    you: YouView,
    top: Vec<TopRow>,
    final_blow: Option<Who>,
    last_week: Option<LastWeek>,
    rules: &'static Rules,
}

fn species_name(dex: i64) -> (String, Vec<String>) {
    DATA.by_dex
        .get(&dex.to_string())
        .and_then(|pid| DATA.pokemon.get(pid))
        .map(|e| (e.name.clone(), e.types.clone()))
        .unwrap_or_else(|| (format!("#{dex}"), Vec::new()))
}

async fn state_for(pool: &SqlitePool, uid: &str, now: DateTime<Utc>) -> Result<BossState, sqlx::Error> {
    let mut conn = pool.acquire().await?;
    let ts = now_ts(now);
    let week = iso_week(&ts);
    let w = ensure_week(&mut conn, &week).await?;
    let s = standing(&mut conn, &w).await?;

    let mut ranked: Vec<(&String, i64)> =
        s.by_user.iter().map(|(u, v)| (u, v.total())).filter(|(_, d)| *d > 0).collect();
    // Most damage first; ties by user id so the order never flickers.
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    let mine = s.by_user.get(uid).copied().unwrap_or_default();
    let rank = ranked.iter().position(|(u, _)| u.as_str() == uid).map(|i| i + 1);

    let mut ids: Vec<String> = Vec::new();
    for (u, _) in &ranked {
        if ids.len() >= TOP_N * 2 {
            break; // inactive users are skipped below, so look a little past ten
        }
        ids.push((*u).clone());
    }
    if let Some(f) = &w.final_blow {
        ids.push(f.clone());
    }
    let last_name = {
        let (start, _) = week_bounds(&week);
        start
            .get(..10)
            .and_then(|d| chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").ok())
            .map(|d| iso_week(&(d - Duration::days(1)).format("%Y-%m-%d").to_string()))
            .unwrap_or_default()
    };
    let last = week_row(&mut conn, &last_name).await?;
    if let Some(f) = last.as_ref().and_then(|l| l.final_blow.clone()) {
        ids.push(f);
    }
    let names = who(&mut conn, &ids).await?;

    let top = ranked
        .iter()
        .filter_map(|(u, d)| names.get(*u).map(|w| TopRow { who: w.clone(), dmg: *d }))
        .take(TOP_N)
        .collect();
    let used = fights_today(&mut conn, uid, &ts[..10]).await?;
    let (name, types) = species_name(w.dex);
    let (_, end) = week_bounds(&week);
    Ok(BossState {
        week: week.clone(),
        ends_at: end,
        boss: BossView { dex: w.dex, name, types, max_hp: w.max_hp, hp: s.hp, phase: phase_of(&w, s.hp) },
        you: YouView {
            dmg: mine.total(),
            sources: mine,
            attempts_left: (FIGHTS_PER_DAY - used).max(0),
            rank,
        },
        top,
        final_blow: w.final_blow.as_ref().and_then(|f| names.get(f).cloned()),
        last_week: last.map(|l| {
            let (name, _) = species_name(l.dex);
            LastWeek {
                week: l.week,
                dex: l.dex,
                name,
                defeated: l.defeated,
                final_blow: l.final_blow.as_ref().and_then(|f| names.get(f).cloned()),
            }
        }),
        rules: &RULES,
    })
}

// ------------------------------------------------------------- fighting --

#[derive(Debug, PartialEq)]
enum Refuse {
    Say(StatusCode, &'static str),
    Db(String),
}

impl From<sqlx::Error> for Refuse {
    fn from(e: sqlx::Error) -> Self {
        Refuse::Db(e.to_string())
    }
}

/// What a committed (or replayed) fight did.
#[derive(Debug, PartialEq)]
struct Applied {
    week: String,
    dmg: i64,
    hp: i64,
    final_blow: bool,
}

fn is_unique(e: &sqlx::Error) -> bool {
    matches!(e, sqlx::Error::Database(d) if d.is_unique_violation())
}

/// The stored result of an earlier fight with this request id, if any.
async fn replayed(pool: &SqlitePool, uid: &str, rid: &str) -> Result<Option<Applied>, sqlx::Error> {
    let mut conn = pool.acquire().await?;
    let r: Option<(String, i64)> =
        sqlx::query_as("SELECT week, dmg FROM boss_fights WHERE user_id = ?1 AND request_id = ?2")
            .bind(uid)
            .bind(rid)
            .fetch_optional(&mut *conn)
            .await?;
    let Some((week, dmg)) = r else { return Ok(None) };
    let w = ensure_week(&mut conn, &week).await?;
    let s = standing(&mut conn, &w).await?;
    let final_blow = w.final_blow.as_deref() == Some(uid) && {
        // The final blow is the fight that took the last HP: this user's
        // newest fight of that week.
        let last: Option<String> = sqlx::query_scalar(
            "SELECT request_id FROM boss_fights WHERE week = ?1 AND user_id = ?2 ORDER BY id DESC LIMIT 1",
        )
        .bind(&week)
        .bind(uid)
        .fetch_optional(&mut *conn)
        .await?;
        last.as_deref() == Some(rid)
    };
    Ok(Some(Applied { week, dmg, hp: s.hp, final_blow }))
}

/// One fight, in one transaction, journal row first: the INSERT takes SQLite's
/// write lock and claims UNIQUE(user_id, request_id), so the daily count, the
/// HP read and the final-blow update below all run serialised against any
/// other fight. `raw` is the simulated damage before the remaining-HP clamp.
async fn apply_fight(
    pool: &SqlitePool,
    uid: &str,
    rid: &str,
    week: &str,
    raw: i64,
    now: DateTime<Utc>,
) -> Result<Applied, Refuse> {
    let ts = now_ts(now);
    let day = &ts[..10];
    let mut tx = pool.begin().await?;
    let ins = sqlx::query(
        "INSERT INTO boss_fights (week, user_id, day, dmg, request_id, at) VALUES (?1, ?2, ?3, 0, ?4, ?5)",
    )
    .bind(week)
    .bind(uid)
    .bind(day)
    .bind(rid)
    .bind(&ts)
    .execute(&mut *tx)
    .await;
    let row_id = match ins {
        Ok(r) => r.last_insert_rowid(),
        Err(e) if is_unique(&e) => {
            drop(tx);
            // A concurrent submit of the same request id committed first.
            return replayed(pool, uid, rid).await?.ok_or(Refuse::Db("replay vanished".into()));
        }
        Err(e) => return Err(e.into()),
    };
    // Counted under the lock and including the row just inserted, so the
    // fourth attempt of the day is the one refused, however they race.
    if fights_today(&mut tx, uid, day).await? > FIGHTS_PER_DAY {
        return Err(Refuse::Say(StatusCode::TOO_MANY_REQUESTS, "no boss attempts left today: back tomorrow (UTC)"));
    }
    let w = ensure_week(&mut tx, week).await?;
    let s = standing(&mut tx, &w).await?;
    match phase_of(&w, s.hp) {
        "down" => return Err(Refuse::Say(StatusCode::CONFLICT, "the boss is already down this week")),
        "drain" => return Err(Refuse::Say(StatusCode::CONFLICT, "the boss isn't weak enough to fight yet")),
        _ => {}
    }
    let dmg = raw.clamp(0, s.hp);
    sqlx::query("UPDATE boss_fights SET dmg = ?1 WHERE id = ?2")
        .bind(dmg)
        .bind(row_id)
        .execute(&mut *tx)
        .await?;
    let hp = s.hp - dmg;
    let mut final_blow = false;
    if hp == 0 {
        // Conditional: exactly one fight ever records the final blow.
        let r = sqlx::query(
            "UPDATE boss_weeks SET defeated_at = ?1, final_blow_user = ?2 WHERE week = ?3 AND defeated_at IS NULL",
        )
        .bind(&ts)
        .bind(uid)
        .bind(week)
        .execute(&mut *tx)
        .await?;
        final_blow = r.rows_affected() == 1;
    }
    tx.commit().await?;
    Ok(Applied { week: week.to_string(), dmg, hp, final_blow })
}

// ---------------------------------------------------------- the bodies --

fn is_request_id(s: &str) -> bool {
    (16..=64).contains(&s.len()) && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

const SPEC_KEYS: [&str; 6] = ["sp", "st", "br", "mg", "sh", "name"];

/// {requestId, team: [spec ≤ 6]}. Unknown keys, a bad request id, an empty or
/// oversized team, or any spec `clean_spec` refuses → 422.
fn fight_body(bytes: &[u8]) -> Result<(String, Vec<Spec>), &'static str> {
    let Ok(Value::Object(o)) = serde_json::from_slice::<Value>(bytes) else {
        return Err("body must be a JSON object");
    };
    if o.keys().any(|k| k != "requestId" && k != "team") {
        return Err("unknown field");
    }
    let rid = o.get("requestId").and_then(Value::as_str).filter(|s| is_request_id(s)).ok_or("bad requestId")?;
    let team = o.get("team").and_then(Value::as_array).ok_or("team must be a list")?;
    if team.is_empty() || team.len() > TEAM_MAX {
        return Err("team must have 1 to 6 Pokémon");
    }
    let mut specs = Vec::with_capacity(team.len());
    for t in team {
        let obj: &Map<String, Value> = t.as_object().ok_or("bad team member")?;
        if obj.keys().any(|k| !SPEC_KEYS.contains(&k.as_str())) {
            return Err("bad team member");
        }
        specs.push(clean_spec(t).ok_or("bad team member")?);
    }
    Ok((rid.to_string(), specs))
}

// ------------------------------------------------------------ handlers --

fn caller(req: &Request) -> Caller {
    req.extensions().get::<Caller>().expect("caller set by middleware").clone()
}

async fn get_boss(State(st): State<AppState>, req: Request) -> Response {
    let c = caller(&req);
    match state_for(&st.pool, &c.user_id, Utc::now()).await {
        Ok(s) => Json(s).into_response(),
        Err(e) => sql_err(e),
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct FightOut {
    dmg: i64,
    hp: i64,
    max_hp: i64,
    phase: &'static str,
    final_blow: bool,
    attempts_left: i64,
    team: Vec<Value>,
    boss: Value,
    log: Vec<Value>,
}

async fn fight(State(st): State<AppState>, req: Request) -> Response {
    let c = caller(&req);
    let (_, body) = req.into_parts();
    let Ok(bytes) = axum::body::to_bytes(body, 64 * 1024).await else {
        return err(StatusCode::PAYLOAD_TOO_LARGE, "body too large");
    };
    let (rid, team) = match fight_body(&bytes) {
        Ok(v) => v,
        Err(m) => return err(StatusCode::UNPROCESSABLE_ENTITY, m),
    };
    fight_for(&st.pool, &c.user_id, &rid, &team, Utc::now()).await
}

async fn fight_for(pool: &SqlitePool, uid: &str, rid: &str, team: &[Spec], now: DateTime<Utc>) -> Response {
    // A replay answers from the stored row (and the same seed replays the same log).
    let prior = match replayed(pool, uid, rid).await {
        Ok(p) => p,
        Err(e) => return sql_err(e),
    };
    let week = prior.as_ref().map(|p| p.week.clone()).unwrap_or_else(|| iso_week(&now_ts(now)));
    let w = {
        let mut conn = match pool.acquire().await {
            Ok(c) => c,
            Err(e) => return sql_err(e),
        };
        match ensure_week(&mut conn, &week).await {
            Ok(w) => w,
            Err(e) => return sql_err(e),
        }
    };
    let Some(f) = simulate(&week, uid, rid, w.dex, team) else {
        return err(StatusCode::INTERNAL_SERVER_ERROR, "boss unavailable");
    };
    let applied = match prior {
        Some(p) => p,
        None => match apply_fight(pool, uid, rid, &week, fight_damage(f.lost, f.boss_max, w.max_hp), now).await {
            Ok(a) => a,
            Err(Refuse::Say(code, m)) => return err(code, m),
            Err(Refuse::Db(m)) => return err(StatusCode::INTERNAL_SERVER_ERROR, &format!("db error: {m}")),
        },
    };
    let mut conn = match pool.acquire().await {
        Ok(c) => c,
        Err(e) => return sql_err(e),
    };
    let used = fights_today(&mut conn, uid, &now_ts(now)[..10]).await.unwrap_or(FIGHTS_PER_DAY);
    let w = week_row(&mut conn, &week).await.ok().flatten().unwrap_or(w);
    Json(FightOut {
        dmg: applied.dmg,
        hp: applied.hp,
        max_hp: w.max_hp,
        phase: phase_of(&w, applied.hp),
        final_blow: applied.final_blow,
        attempts_left: (FIGHTS_PER_DAY - used).max(0),
        team: f.team,
        boss: f.boss,
        log: f.log,
    })
    .into_response()
}

#[derive(Serialize)]
struct Badges {
    slayer: Vec<String>,
    participated: i64,
}

async fn badges_for(pool: &SqlitePool, uid: &str) -> Result<Badges, sqlx::Error> {
    let slayer: Vec<String> =
        sqlx::query_scalar("SELECT week FROM boss_weeks WHERE final_blow_user = ?1 ORDER BY week")
            .bind(uid)
            .fetch_all(pool)
            .await?;
    let participated: i64 =
        sqlx::query_scalar("SELECT COUNT(DISTINCT week) FROM boss_fights WHERE user_id = ?1")
            .bind(uid)
            .fetch_one(pool)
            .await?;
    Ok(Badges { slayer, participated })
}

async fn badges(State(st): State<AppState>, Path(user_id): Path<String>, req: Request) -> Response {
    let c = caller(&req);
    let uid = if user_id == "me" {
        c.user_id
    } else {
        let known: Result<Option<i64>, _> =
            sqlx::query_scalar("SELECT 1 FROM users WHERE id = ?1 AND is_active = 1")
                .bind(&user_id)
                .fetch_optional(&st.pool)
                .await;
        match known {
            Ok(Some(_)) => user_id,
            Ok(None) => return err(StatusCode::NOT_FOUND, "no such trainer"),
            Err(e) => return sql_err(e),
        }
    };
    match badges_for(&st.pool, &uid).await {
        Ok(b) => Json(b).into_response(),
        Err(e) => sql_err(e),
    }
}

/// Only this group's routes; main.rs puts them behind `require_device`.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/boss", get(get_boss))
        .route("/v1/boss/fight", post(fight))
        .route("/v1/boss/badges/:user_id", get(badges))
}

#[cfg(test)]
mod tests;
