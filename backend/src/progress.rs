//! HQ 2.1 progression, leaderboards and trainer profiles, ported from
//! `app/results.py` and `app/routes/progress.py`.
//!
//! Three read-only routes. Nothing here is submitted by a client: a level, a
//! streak, a trophy and a board position are all derived on the spot from
//! `daily_stats` (what the stats ingest recorded) and `game_results` (what the
//! Arena itself wrote when a game ended). There is no INSERT, UPDATE or DELETE
//! in this module and so no transaction either -- the only write in the request
//! path is `devices.last_seen_at`, done by main.rs's `require_device`.
//!
//! The privacy boundary: a profile and a board carry derived counts, equipped
//! cosmetics and a crew's tag. Never a cost, a token count, a GitHub id, a
//! device, an email, a crew's invite code, or anything from a transcript.

use crate::scoring::{derive_level, rank_for_level, xp_for_level, xp_from_counts};
use axum::{
    extract::{Path, Query, Request, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use chrono::{NaiveDate, Utc};
use serde::de::{MapAccess, Visitor};
use serde::ser::SerializeMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{json, Map, Value};
use sqlx::{Row, SqlitePool};
use std::collections::{HashMap, HashSet};

/// Response order for `games`, and the only games a result row may count
/// toward. A row naming anything else is skipped by the profile walk.
const GAMES: [&str; 7] = ["kart", "plat", "fps", "golf", "type", "td", "bowl"];
/// Boards where a bigger value is better (waves cleared, pins): keep the max,
/// sort descending. Every other non-fps board keeps the minimum.
const HIGHER_BETTER: [&str; 2] = ["td", "bowl"];
/// The All-rounder trophy stays pinned to the original five games.
const ALL_ROUNDER: usize = 5;
/// Indices into the per-game array, for the three games with their own rules.
const KART: usize = 0;
const FPS: usize = 2;
const TYPE: usize = 4;

/// Game XP is capped per player per **UTC calendar day**, applied before the
/// days are summed. Lifetime game XP is unbounded.
const GAME_XP_DAY_CAP: i64 = 400;
/// At most this many entries per board -- ranks are numbered before the cap.
const BOARD_SIZE: usize = 20;
/// The flex streak: at least 5 active days in every 7, looking back a year-ish.
const STREAK_WINDOW: usize = 7;
const STREAK_MAX_MISSES: usize = 2;
const STREAK_LOOKBACK: i64 = 400;

/// Same envelope as main.rs's `err`. Duplicated rather than shared so this
/// module depends on nothing of main.rs but `AppState` and `Caller`.
fn err(code: StatusCode, msg: &str) -> Response {
    (code, Json(json!({ "detail": msg }))).into_response()
}

// --- ordered JSON objects -------------------------------------------------

/// A JSON object that serialises in insertion order.
///
/// `serde_json::Map` is a `BTreeMap` in this build -- the `preserve_order`
/// feature is off and Cargo.toml is not ours to change -- so a per-game map
/// built from one would come out `{"best:meadow", "bestLap", "played", "wins"}`
/// where the Python emits `{"played", "wins", "best:meadow", "bestLap"}`. The
/// JS client is byte-compared against the Python, so key order is part of the
/// wire format and has to be carried explicitly.
#[derive(Debug, Default)]
struct Obj(Vec<(String, Value)>);

impl Obj {
    /// Python's `d[k] = v`: a key that is already present keeps the position it
    /// was first given, which is why this is not a plain push.
    fn set(&mut self, key: &str, value: Value) {
        match self.0.iter_mut().find(|(k, _)| k.as_str() == key) {
            Some(slot) => slot.1 = value,
            None => self.0.push((key.to_string(), value)),
        }
    }

    fn get(&self, key: &str) -> Option<&Value> {
        self.0.iter().find(|(k, _)| k.as_str() == key).map(|(_, v)| v)
    }

    fn int(&self, key: &str) -> Option<i64> {
        self.get(key).and_then(Value::as_i64)
    }

    fn float(&self, key: &str) -> Option<f64> {
        self.get(key).and_then(Value::as_f64)
    }

    /// `d[k] = d.get(k, 0) + by`.
    fn bump(&mut self, key: &str, by: i64) {
        let total = self.int(key).unwrap_or(0) + by;
        self.set(key, total.into());
    }

    /// `d[k] = v if d.get(k) is None else min(d[k], v)` -- lower is better for
    /// every time, stroke count and lap on a board.
    fn keep_min(&mut self, key: &str, value: i64) {
        let keep = self.int(key).map_or(value, |cur| cur.min(value));
        self.set(key, keep.into());
    }

    /// The same, where more is better (td waves, bowling pins).
    fn keep_max(&mut self, key: &str, value: i64) {
        let keep = self.int(key).map_or(value, |cur| cur.max(value));
        self.set(key, keep.into());
    }
}

impl Serialize for Obj {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut m = s.serialize_map(Some(self.0.len()))?;
        for (k, v) in &self.0 {
            m.serialize_entry(k, v)?;
        }
        m.end()
    }
}

/// `equipped_cosmetics.slots` as stored, in the key order of the stored
/// document: `cos` mirrors that order in the Python, and parsing into a
/// `serde_json::Map` would re-sort it (see `Obj`). A hand-written visitor is
/// the only way to see document order without touching Cargo.toml.
struct StoredSlots(Vec<(String, Value)>);

impl<'de> Deserialize<'de> for StoredSlots {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct AnyObject;
        impl<'de> Visitor<'de> for AnyObject {
            type Value = StoredSlots;

            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("an object of slot -> item id")
            }

            fn visit_map<M: MapAccess<'de>>(self, mut m: M) -> Result<StoredSlots, M::Error> {
                let mut out = Vec::new();
                while let Some(pair) = m.next_entry::<String, Value>()? {
                    out.push(pair);
                }
                Ok(StoredSlots(out))
            }
        }
        d.deserialize_map(AnyObject)
    }
}

// --- cosmetics ------------------------------------------------------------

/// (item id, slot, rendered value) from `app/cosmetics.py`'s catalogue.
///
/// Only those three columns matter here: a profile shows what someone is
/// wearing and never an item id, a price, a level gate or whether it was
/// bought. The full table and the shop belong to the cosmetics group; carrying
/// three columns of it keeps this module from depending on that port's
/// internals, at the cost of having to be updated with it.
const COSMETICS: &[(&str, &str, &str)] = &[
    ("k-neon", "kart", "#39ff88"),
    ("k-midnight", "kart", "#1d2b5c"),
    ("k-sunset", "kart", "#ff7b54"),
    ("k-gold", "kart", "#d8b34a"),
    ("r-ember", "runner", "#ff6b5b"),
    ("r-aqua", "runner", "#5fd3e6"),
    ("r-gold", "runner", "#d8b34a"),
    ("g-chrome", "blaster", "#c0c8d0"),
    ("g-ember", "blaster", "#ff6b5b"),
    ("g-void", "blaster", "#2c2c34"),
    ("b-pink", "ball", "#ff9ad5"),
    ("b-lime", "ball", "#b6ff5c"),
    ("b-gold", "ball", "#d8b34a"),
    ("f-brass", "frame", "#d8b34a"),
    ("f-neon", "frame", "#5fd3e6"),
    ("f-crimson", "frame", "#ff6b5b"),
    ("f-legend", "frame", "#9b8cf0"),
    ("d-flags", "decor", "flags"),
    ("d-gnomes", "decor", "gnomes"),
    ("d-fireworks", "decor", "fireworks"),
    ("d-neon", "decor", "neon"),
    // 2.5 grant-only (loot, prestige)
    ("k-prism", "kart", "#b388ff"),
    ("r-shadow", "runner", "#3a3550"),
    ("g-plasma", "blaster", "#7df9ff"),
    ("b-pokeball", "ball", "#e3350d"),
    ("f-holo", "frame", "#a0f0ff"),
    ("f-star", "frame", "#f5d76e"),
    ("d-crown", "decor", "crown"),
];

/// `app/cosmetics.py::equipped`: slot -> rendered value, dropping any entry
/// whose item is unknown or is stored under a slot it does not belong to.
///
/// The double filter is what keeps a stale or mis-slotted row from breaking a
/// profile: it vanishes from the view and is left untouched in the stored map.
/// Ownership is deliberately not re-checked -- what you wear stays on show.
fn equipped(slots: &[(String, Value)]) -> Obj {
    let mut out = Obj::default();
    for (slot, item) in slots {
        // A non-string item id is not in the catalogue, which is also what
        // Python's `c in CATALOG` concludes about one.
        let Some(id) = item.as_str() else { continue };
        if let Some(entry) = COSMETICS.iter().find(|e| e.0 == id && e.1 == slot.as_str()) {
            out.set(slot, Value::String(entry.2.to_string()));
        }
    }
    out
}

// --- wire shapes ----------------------------------------------------------
//
// `derive(Serialize)` structs rather than JSON maps wherever the keys are
// fixed: field declaration order below is the Python dict's insertion order,
// and a struct is the one thing serde_json cannot re-sort.

/// GET /v1/progress/me's whole body, and a profile's `progress` block.
#[derive(Debug, Serialize)]
pub struct Progress {
    pub level: i64,
    pub xp: i64,
    #[serde(rename = "sessionXp")] pub session_xp: i64,
    #[serde(rename = "gameXp")] pub game_xp: i64,
    #[serde(rename = "xpIntoLevel")] pub xp_into_level: i64,
    #[serde(rename = "xpForLevel")] pub xp_for_level: i64,
    pub rank: &'static str,
    #[serde(rename = "nextLevelAt")] pub next_level_at: i64,
}

#[derive(Debug, Serialize)]
struct UserView {
    #[serde(rename = "userId")] user_id: String,
    handle: String,
    #[serde(rename = "displayName")] display_name: String,
    #[serde(rename = "avatarUrl")] avatar_url: String,
}

/// One player's row on one board. Every key is emitted for every game -- a kart
/// entry still carries `kills: 0`, an fps entry still carries `best: null` --
/// and only the three the Python inserts later are conditional: `kd` on fps,
/// `wpm` and `acc` on type, in that order, all before `rank`.
#[derive(Debug, Serialize)]
struct Entry {
    user: UserView,
    played: i64,
    wins: i64,
    best: Option<i64>,
    #[serde(rename = "bestLap")] best_lap: Option<i64>,
    kills: i64,
    deaths: i64,
    #[serde(rename = "isYou")] is_you: bool,
    #[serde(skip_serializing_if = "Option::is_none")] kd: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")] wpm: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")] acc: Option<f64>,
    rank: i64,
}

#[derive(Debug, Serialize)]
struct BoardView {
    key: String,
    entries: Vec<Entry>,
}

#[derive(Debug, Serialize)]
struct BoardsView {
    game: String,
    boards: Vec<BoardView>,
}

/// The five per-game maps, in GAMES order. Named fields rather than a map
/// because the five keys are fixed even though each game's own keys are not.
#[derive(Debug, Serialize)]
struct GamesView {
    kart: Obj,
    plat: Obj,
    fps: Obj,
    golf: Obj,
    #[serde(rename = "type")] typing: Obj,
    td: Obj,
    bowl: Obj,
}

#[derive(Debug, Serialize)]
struct Totals {
    played: i64,
    wins: i64,
    podiums: i64,
}

#[derive(Debug, Serialize)]
struct TrophyView {
    id: &'static str,
    name: &'static str,
}

/// A crew's public face. The invite code is not a field here and is not
/// selected by the query that fills it.
#[derive(Debug, Serialize)]
struct CrewTag {
    tag: String,
    color: String,
    name: String,
}

#[derive(Debug, Serialize)]
struct ProfileView {
    #[serde(rename = "userId")] user_id: String,
    handle: String,
    #[serde(rename = "displayName")] display_name: String,
    #[serde(rename = "trainerName")] trainer_name: Option<String>,
    #[serde(rename = "avatarUrl")] avatar_url: String,
    progress: Progress,
    streak: i64,
    games: GamesView,
    totals: Totals,
    trophies: Vec<TrophyView>,
    cos: Obj,
    crew: Option<CrewTag>,
    #[serde(rename = "isYou")] is_you: bool,
}

// --- scoring --------------------------------------------------------------

/// Finishing is worth 20; winning a game *with others* 30 more, 2nd 15, 3rd 8.
/// A solo run is a flat 20 even in first place.
fn xp_for_result(place: i64, players: i64) -> i64 {
    let bonus = if players >= 2 {
        match place {
            1 => 30,
            2 => 15,
            3 => 8,
            _ => 0,
        }
    } else {
        0
    };
    20 + bonus
}

/// One `game_results` row reduced to what game XP depends on, so the capping
/// fold below stays a pure function.
struct ResultDay {
    user_id: String,
    day: String,
    place: i64,
    players: i64,
}

/// Game XP per user: bucket by (user, UTC day), clamp each day to the cap, then
/// sum the days. The Python clamps at every accumulation step, which comes to
/// the same thing only because no result is worth negative XP -- summing a day
/// and then clamping it is the clearer way to say it.
fn game_xp_totals(rows: &[ResultDay]) -> HashMap<String, i64> {
    let mut per_day: HashMap<(&str, &str), i64> = HashMap::new();
    for r in rows {
        let slot = per_day.entry((r.user_id.as_str(), r.day.as_str())).or_default();
        *slot = GAME_XP_DAY_CAP.min(*slot + xp_for_result(r.place, r.players));
    }
    let mut out: HashMap<String, i64> = HashMap::new();
    for ((uid, _day), xp) in per_day {
        *out.entry(uid.to_string()).or_default() += xp;
    }
    out
}

/// The eight numbers a level is.
fn progress_from_xp(session_xp: i64, game_xp: i64) -> Progress {
    let xp = session_xp + game_xp;
    let (level, into, need) = derive_level(xp);
    Progress {
        level,
        xp,
        session_xp,
        game_xp,
        xp_into_level: into,
        xp_for_level: need,
        rank: rank_for_level(level),
        // The cost of the level *after* this one, not a cumulative XP target.
        // The quirk is load-bearing: the JS client renders this number as it is.
        next_level_at: xp_for_level(level + 1),
    }
}

/// Python's `round(x, 2)`, which is round-half-to-even on the exact binary
/// value -- `0.125` goes to `0.12`, not `0.13`. Rust's `{:.2}` rounds the same
/// way and reparsing lands on the same double, where `(x * 100).round() / 100`
/// would round half away from zero and disagree on every exact tie.
fn round2(x: f64) -> f64 {
    format!("{x:.2}").parse().unwrap_or(x)
}

/// The flex streak from `app/scoring.py::streak_from_dates`: it survives missed
/// days as long as no 7-day stretch inside it holds more than 2 of them.
///
/// This is **not** `crate::scoring::streak_from_dates`, which is a strict
/// consecutive-day count -- for `{today, today-1, today-3}` the Python says 4
/// and the strict version says 2 (pinned in the tests below). Walking back from
/// yesterday when today is not yet active is what keeps a day in progress from
/// being charged as a miss.
fn flex_streak(active: &HashSet<NaiveDate>, today: NaiveDate) -> i64 {
    let start = if active.contains(&today) {
        today
    } else {
        today - chrono::Duration::days(1)
    };
    let mut span: Vec<bool> = Vec::new();
    let mut day = start;
    for _ in 0..STREAK_LOOKBACK {
        span.push(active.contains(&day));
        let window = &span[span.len().saturating_sub(STREAK_WINDOW)..];
        if window.iter().filter(|&&on| !on).count() > STREAK_MAX_MISSES {
            span.pop();
            break;
        }
        let Some(prev) = day.pred_opt() else { break };
        day = prev;
    }
    // The streak spans the first active day to the last one inside the walk;
    // misses tolerated in between are part of it, trailing ones are not.
    let Some(newest) = span.iter().position(|on| *on) else {
        return 0;
    };
    let oldest = span.iter().rposition(|on| *on).unwrap_or(newest);
    (oldest - newest + 1) as i64
}

// --- reads ----------------------------------------------------------------

fn placeholders(n: usize) -> String {
    (1..=n).map(|i| format!("?{i}")).collect::<Vec<_>>().join(",")
}

/// `r.extra or {}`: a NULL, empty, unparseable `extra`, or one holding anything
/// other than an object, degrades to no extras rather than failing the request.
fn extra_obj(r: &sqlx::sqlite::SqliteRow) -> Map<String, Value> {
    let raw: Option<String> = r.try_get("extra").unwrap_or(None);
    raw.and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .and_then(|v| match v {
            Value::Object(o) => Some(o),
            _ => None,
        })
        .unwrap_or_default()
}

/// Python's `int(ex.get(k, 0))`: a JSON float truncates toward zero, as `int()`
/// does, and anything that is not a number counts as nothing.
fn int_of(extra: &Map<String, Value>, key: &str) -> i64 {
    match extra.get(key) {
        Some(Value::Number(n)) => n.as_i64().unwrap_or_else(|| n.as_f64().unwrap_or(0.0) as i64),
        _ => 0,
    }
}

/// Python's `float(ex.get(k, 0))`.
fn float_of(extra: &Map<String, Value>, key: &str) -> f64 {
    extra.get(key).and_then(Value::as_f64).unwrap_or(0.0)
}

/// `isinstance(lap, int)`: a JSON float such as `20000.0` is not an `int` in
/// Python and is ignored there, so only an integer counts here either.
fn strict_int(extra: &Map<String, Value>, key: &str) -> Option<i64> {
    match extra.get(key) {
        Some(Value::Number(n)) if n.is_i64() || n.is_u64() => n.as_i64(),
        _ => None,
    }
}

/// Session XP plus per-day-capped game XP, as one level per user.
///
/// Every id passed in comes back, at zero if it has no rows. It takes a batch
/// because this is the shared progression helper -- the HQ, cosmetics level
/// unlocks and the crew board all have to show the same level as
/// GET /v1/progress/me, and a per-user loop would re-scan `game_results` once
/// per member of a crew (`ix_game_results_user` is what keeps one scan cheap).
///
/// Both reads are all-time and unfiltered by `is_active`, unlike the windowed
/// `service::build_board`: a level never goes down.
pub async fn progress(
    pool: &SqlitePool,
    user_ids: &[&str],
) -> Result<HashMap<String, Progress>, sqlx::Error> {
    if user_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let ph = placeholders(user_ids.len());

    let sql = format!(
        "SELECT user_id,
                COALESCE(SUM(prompts),0)   AS prompts,
                COALESCE(SUM(tools),0)     AS tools,
                COALESCE(SUM(artifacts),0) AS artifacts
         FROM daily_stats WHERE user_id IN ({ph}) GROUP BY user_id"
    );
    let mut q = sqlx::query(&sql);
    for u in user_ids {
        q = q.bind(*u);
    }
    let mut session: HashMap<String, i64> = HashMap::new();
    for r in q.fetch_all(pool).await? {
        let uid: String = r.get("user_id");
        session.insert(uid, xp_from_counts(r.get("prompts"), r.get("tools"), r.get("artifacts")));
    }

    let sql = format!(
        "SELECT user_id, substr(at, 1, 10) AS day, place, players
         FROM game_results WHERE user_id IN ({ph})"
    );
    let mut q = sqlx::query(&sql);
    for u in user_ids {
        q = q.bind(*u);
    }
    let today = Utc::now().date_naive().to_string();
    let rows: Vec<ResultDay> = q
        .fetch_all(pool)
        .await?
        .iter()
        .map(|r| ResultDay {
            user_id: r.get("user_id"),
            // `at` is written by SQLite's CURRENT_TIMESTAMP as naive UTC text,
            // so its first ten characters are the UTC calendar day -- which is
            // exactly what the Python gets from `.date()` on the datetime
            // SQLAlchemy parses out of the same string. Sliced rather than run
            // through date(), which would answer NULL (and so silently mean
            // "today") for a value it could not parse. A genuinely missing `at`
            // does fall back to today, as the Python's `at or now` does.
            day: r
                .try_get::<Option<String>, _>("day")
                .ok()
                .flatten()
                .unwrap_or_else(|| today.clone()),
            place: r.get("place"),
            players: r.get("players"),
        })
        .collect();
    let game = game_xp_totals(&rows);

    Ok(user_ids
        .iter()
        .map(|u| {
            let session_xp = session.get(*u).copied().unwrap_or(0);
            let game_xp = game.get(*u).copied().unwrap_or(0);
            ((*u).to_string(), progress_from_xp(session_xp, game_xp))
        })
        .collect())
}

/// One user's progression, for the two routes that need exactly one.
async fn progress_of(pool: &SqlitePool, uid: &str) -> Result<Progress, sqlx::Error> {
    let mut all = progress(pool, &[uid]).await?;
    // `progress` answers for every id it is given, so the fallback is
    // unreachable -- and is the right answer anyway for a user with no rows.
    Ok(all.remove(uid).unwrap_or_else(|| progress_from_xp(0, 0)))
}

/// One pass over a board's rows, folded into one entry per player, plus the
/// second pass `type` needs.
fn fold_board(game: &str, rows: &[sqlx::sqlite::SqliteRow], viewer_id: &str) -> Vec<Entry> {
    let mut best: HashMap<String, Entry> = HashMap::new();
    for r in rows {
        let uid: String = r.get("id");
        let handle: String = r.get("handle");
        let display: String = r.get("display_name");
        let avatar: String = r.get("avatar_url");
        let place: i64 = r.get("place");
        let players: i64 = r.get("players");
        let value: Option<i64> = r.try_get("value").unwrap_or(None);
        let extra = extra_obj(r);

        let e = best.entry(uid.clone()).or_insert_with(|| Entry {
            user: UserView {
                user_id: uid.clone(),
                handle: handle.clone(),
                display_name: if display.is_empty() { handle.clone() } else { display.clone() },
                avatar_url: avatar.clone(),
            },
            played: 0,
            wins: 0,
            best: None,
            best_lap: None,
            kills: 0,
            deaths: 0,
            is_you: uid == viewer_id,
            kd: None,
            wpm: None,
            acc: None,
            rank: 0,
        });

        e.played += 1;
        // A solo run is never a win, however it placed.
        if place == 1 && players >= 2 {
            e.wins += 1;
        }
        if game == "fps" {
            e.kills += int_of(&extra, "kills");
            e.deaths += int_of(&extra, "deaths");
        } else if let Some(v) = value {
            // The Python's `elif` only excludes fps, so the `type` board does
            // carry a real `best` (its lowest race time) that nothing sorts by,
            // and an fps entry's `best` stays null forever. Both are kept.
            e.best = Some(if HIGHER_BETTER.contains(&game) {
                e.best.map_or(v, |b| b.max(v))
            } else {
                e.best.map_or(v, |b| b.min(v))
            });
        }
        // Read for every game, not just kart.
        if let Some(lap) = strict_int(&extra, "bestLap") {
            e.best_lap = Some(e.best_lap.map_or(lap, |b| b.min(lap)));
        }
    }

    if game == "type" {
        // A second pass, because `acc` has to come from the row that set the
        // winning `wpm`: tracking the two maxima independently would pair a
        // best speed with someone else's accuracy.
        for r in rows {
            let uid: String = r.get("id");
            let Some(e) = best.get_mut(&uid) else { continue };
            let value: Option<i64> = r.try_get("value").unwrap_or(None);
            let extra = extra_obj(r);
            let w = float_of(&extra, "wpm");
            // `w > (b.get("wpm") or 0)`, so a stored 0 never wins the comparison.
            if value.is_some() && w > e.wpm.unwrap_or(0.0) {
                e.wpm = Some(w);
                e.acc = Some(float_of(&extra, "acc"));
            }
        }
    }

    finalise(game, best.into_values().collect())
}

/// Descending order for an f64 sort key, which has no total order of its own.
fn cmp_desc_f64(a: Option<f64>, b: Option<f64>) -> std::cmp::Ordering {
    b.unwrap_or(0.0)
        .partial_cmp(&a.unwrap_or(0.0))
        .unwrap_or(std::cmp::Ordering::Equal)
}

/// Filter, order, number and cap one board.
///
/// Ranks are assigned *before* the cap, so a returned board carries ranks
/// 1..=BOARD_SIZE and never a 21. Every ordering ends on the handle, which is
/// unique in `users`, so no two entries can compare equal and no row order from
/// SQL can leak into the response.
fn finalise(game: &str, mut entries: Vec<Entry>) -> Vec<Entry> {
    match game {
        "type" => {
            // Python tests truthiness, so a best wpm of exactly 0 is dropped.
            entries.retain(|e| e.wpm.unwrap_or(0.0) != 0.0);
            entries.sort_by(|a, b| {
                cmp_desc_f64(a.wpm, b.wpm).then_with(|| a.user.handle.cmp(&b.user.handle))
            });
        }
        "fps" => {
            // Every entry gets a kd, including a player with no kills at all:
            // the fps board is not filtered, so they stay on it at 0.0.
            for e in entries.iter_mut() {
                e.kd = Some(round2(e.kills as f64 / e.deaths.max(1) as f64));
            }
            entries.sort_by(|a, b| {
                b.kills
                    .cmp(&a.kills)
                    .then_with(|| cmp_desc_f64(a.kd, b.kd))
                    .then_with(|| a.user.handle.cmp(&b.user.handle))
            });
        }
        // td and bowl: the most waves or pins first.
        g if HIGHER_BETTER.contains(&g) => {
            entries.retain(|e| e.best.is_some());
            entries.sort_by(|a, b| {
                b.best.cmp(&a.best).then_with(|| a.user.handle.cmp(&b.user.handle))
            });
        }
        // kart, plat and golf: a player who only ever DNF'd has nothing to rank.
        _ => {
            entries.retain(|e| e.best.is_some());
            entries.sort_by(|a, b| {
                a.best.cmp(&b.best).then_with(|| a.user.handle.cmp(&b.user.handle))
            });
        }
    }
    for (i, e) in entries.iter_mut().enumerate() {
        e.rank = i as i64 + 1;
    }
    entries.truncate(BOARD_SIZE);
    entries
}

async fn build_boards(
    pool: &SqlitePool,
    game: &str,
    key: Option<&str>,
    viewer_id: &str,
) -> Result<BoardsView, sqlx::Error> {
    // No join and no is_active filter here, exactly as the Python: a key that
    // only a deactivated player ever posted on still produces a board, with an
    // empty entry list. Deliberate, and not to be "fixed" by adding the join.
    let rows = sqlx::query(r#"SELECT DISTINCT "key" FROM game_results WHERE game = ?1"#)
        .bind(game)
        .fetch_all(pool)
        .await?;
    let mut keys: Vec<String> = rows
        .iter()
        .filter_map(|r| r.try_get::<Option<String>, _>("key").ok().flatten())
        .filter(|k| !k.is_empty())
        .collect();
    keys.sort();
    match key {
        Some(want) => keys.retain(|k| k.as_str() == want),
        // User-made maps ("c-" content keys) only on request, so they never flood the default boards.
        None => keys.retain(|k| !k.starts_with("c-")),
    }

    let mut boards = Vec::with_capacity(keys.len());
    for k in keys {
        let rows = sqlx::query(
            r#"SELECT g.place, g.players, g.value, g.extra,
                      u.id, u.handle, u.display_name, u.avatar_url
               FROM game_results g JOIN users u ON u.id = g.user_id
               WHERE g.game = ?1 AND g."key" = ?2 AND u.is_active = 1"#,
        )
        .bind(game)
        .bind(&k)
        .fetch_all(pool)
        .await?;
        boards.push(BoardView { key: k, entries: fold_board(game, &rows, viewer_id) });
    }
    Ok(BoardsView { game: game.to_string(), boards })
}

/// Evaluated and emitted in this order; the ids and names are what the client
/// draws, so neither may be reworded.
fn trophies(per: &[Obj; 7], games: i64, wins: i64, podiums: i64, streak: i64) -> Vec<TrophyView> {
    let kills = per[FPS].int("kills").unwrap_or(0);
    let best_wpm = per[TYPE].float("bestWpm").unwrap_or(0.0);
    // `or 10 ** 9` is a truthiness fallback, so a stored lap of 0 misses out too.
    let best_lap = per[KART].int("bestLap").filter(|v| *v != 0).unwrap_or(1_000_000_000);
    let every_game = per[..ALL_ROUNDER].iter().all(|p| p.int("played").unwrap_or(0) != 0);
    [
        ("first-game", "First game", games >= 1),
        ("first-win", "First win", wins >= 1),
        ("ten-wins", "Ten wins", wins >= 10),
        ("podium-5", "Five podiums", podiums >= 5),
        ("all-rounder", "All-rounder: played every game", every_game),
        ("fast-fingers", "Fast fingers: 80 WPM", best_wpm >= 80.0),
        ("sharpshooter", "Sharpshooter: 100 Blaster kills", kills >= 100),
        ("speedster", "Speedster: a lap under 15 s", best_lap < 15_000),
        ("streak-7", "A week-long streak", streak >= 7),
    ]
    .into_iter()
    .filter(|(_, _, won)| *won)
    .map(|(id, name, _)| TrophyView { id, name })
    .collect()
}

/// `Ok(None)` is the 404: there is no such trainer, or there is one who is
/// deactivated, and the two are deliberately indistinguishable.
async fn build_profile(
    pool: &SqlitePool,
    uid: &str,
    viewer_id: &str,
) -> Result<Option<ProfileView>, sqlx::Error> {
    // This runs before anything else is read, so a deactivated account costs
    // one query rather than a whole profile's worth.
    let found = sqlx::query(
        "SELECT id, handle, display_name, trainer_name, avatar_url
         FROM users WHERE id = ?1 AND is_active = 1",
    )
    .bind(uid)
    .fetch_optional(pool)
    .await?;
    let Some(u) = found else {
        return Ok(None);
    };
    let id: String = u.get("id");
    let handle: String = u.get("handle");
    let display: String = u.get("display_name");
    let trainer: String = u.get("trainer_name");
    let avatar: String = u.get("avatar_url");
    let is_you = id == viewer_id;

    // The same helper GET /v1/progress/me answers from, so the two agree.
    let pr = progress_of(pool, &id).await?;

    let today = Utc::now().date_naive();
    let since = (today - chrono::Duration::days(STREAK_LOOKBACK)).to_string();
    // Tools and artifacts alone do not make a day count toward a streak, even
    // though they do earn session XP. Only a prompt or a reply does.
    let days = sqlx::query(
        "SELECT stat_date FROM daily_stats
         WHERE user_id = ?1 AND stat_date >= ?2 AND (prompts > 0 OR replies > 0)",
    )
    .bind(&id)
    .bind(&since)
    .fetch_all(pool)
    .await?;
    let active: HashSet<NaiveDate> = days
        .iter()
        .filter_map(|r| r.try_get::<String, _>("stat_date").ok())
        .filter_map(|d| NaiveDate::parse_from_str(&d, "%Y-%m-%d").ok())
        .collect();
    let streak = flex_streak(&active, today);

    let rows = sqlx::query(
        r#"SELECT game, "key", place, players, value, extra
           FROM game_results WHERE user_id = ?1"#,
    )
    .bind(&id)
    .fetch_all(pool)
    .await?;

    let mut per: [Obj; 7] = std::array::from_fn(|_| {
        let mut o = Obj::default();
        o.set("played", Value::from(0_i64));
        o.set("wins", Value::from(0_i64));
        o
    });
    // One global counter, not one per game.
    let mut podiums: i64 = 0;
    // One running max across the whole loop, as the Python has it. It is only
    // ever written into the `type` map, which is the only place it can grow.
    let mut best_wpm = 0.0_f64;

    for r in &rows {
        let game: String = r.get("game");
        // A row naming a game that is not one of the five counts toward nothing
        // here -- not played, not a win, not a podium -- even though game XP
        // above does not filter by game at all. That asymmetry is the Python's,
        // and is only observable if legacy rows exist.
        let Some(gi) = GAMES.iter().position(|g| *g == game.as_str()) else {
            continue;
        };
        let key: String = r.get("key");
        let place: i64 = r.get("place");
        let players: i64 = r.get("players");
        let value: Option<i64> = r.try_get("value").unwrap_or(None);
        let extra = extra_obj(r);
        let p = &mut per[gi];

        p.bump("played", 1);
        if players >= 2 && place == 1 {
            p.bump("wins", 1);
        }
        // Golf shares places on a tie, so placing is not dense: two players can
        // both be 2nd and nobody 3rd. `<= 3` tolerates that; a check for an
        // exact place would not.
        // A co-op td row stores place 0 ("no placement"): never a podium.
        if players >= 2 && (1..=3).contains(&place) {
            podiums += 1;
        }
        if game == "fps" {
            p.bump("kills", int_of(&extra, "kills"));
            p.bump("deaths", int_of(&extra, "deaths"));
        } else if let Some(v) = value {
            if HIGHER_BETTER.contains(&game.as_str()) {
                p.keep_max(&format!("best:{key}"), v);
            } else {
                p.keep_min(&format!("best:{key}"), v);
            }
        }
        if game == "type" && value.is_some() {
            best_wpm = best_wpm.max(float_of(&extra, "wpm"));
            p.set("bestWpm", best_wpm.into());
        }
        if let Some(lap) = strict_int(&extra, "bestLap") {
            p.keep_min("bestLap", lap);
        }
    }

    // No fps row at all means no `kd` key, not a kd of 0.0. Read out first:
    // the borrow of `per` has to end before the write.
    let fps_kills = per[FPS].int("kills");
    if let Some(kills) = fps_kills {
        let deaths = per[FPS].int("deaths").unwrap_or(0);
        per[FPS].set("kd", round2(kills as f64 / deaths.max(1) as f64).into());
    }

    let played_total: i64 = per.iter().map(|p| p.int("played").unwrap_or(0)).sum();
    let wins_total: i64 = per.iter().map(|p| p.int("wins").unwrap_or(0)).sum();
    let trophies = trophies(&per, played_total, wins_total, podiums, streak);
    let [kart, plat, fps, golf, typing, td, bowl] = per;

    let slots = sqlx::query("SELECT slots FROM equipped_cosmetics WHERE user_id = ?1")
        .bind(&id)
        .fetch_optional(pool)
        .await?;
    let cos = match slots {
        Some(r) => {
            let raw: Option<String> = r.try_get("slots").unwrap_or(None);
            let stored: Vec<(String, Value)> = raw
                .and_then(|s| serde_json::from_str::<StoredSlots>(&s).ok())
                .map(|s| s.0)
                .unwrap_or_default();
            equipped(&stored)
        }
        None => Obj::default(),
    };

    // `crews.code` is not selected: an invite code must never reach a profile.
    let crew = sqlx::query(
        "SELECT c.tag, c.color, c.name FROM crew_members m
         JOIN crews c ON c.id = m.crew_id WHERE m.user_id = ?1",
    )
    .bind(&id)
    .fetch_optional(pool)
    .await?
    .map(|r| CrewTag { tag: r.get("tag"), color: r.get("color"), name: r.get("name") });

    Ok(Some(ProfileView {
        user_id: id,
        handle: handle.clone(),
        display_name: if display.is_empty() { handle } else { display },
        // GET /v1/me emits "" for an unset trainer name where a profile emits
        // null. The two are deliberately not normalised to each other.
        trainer_name: (!trainer.is_empty()).then_some(trainer),
        avatar_url: avatar,
        progress: pr,
        streak,
        games: GamesView { kart, plat, fps, golf, typing, td, bowl },
        totals: Totals { played: played_total, wins: wins_total, podiums },
        trophies,
        cos,
        crew,
        is_you,
    }))
}

// --- routes ---------------------------------------------------------------

async fn my_progress(State(st): State<crate::AppState>, req: Request) -> Response {
    let c = req.extensions().get::<crate::Caller>().expect("caller set by middleware").clone();
    match progress_of(&st.pool, &c.user_id).await {
        Ok(p) => Json(p).into_response(),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, &format!("{e}")),
    }
}

#[derive(Deserialize)]
struct KeyQ {
    #[serde(default)]
    key: Option<String>,
}

/// Pydantic's `max_length=40` on the query parameter, in the body
/// app/main.py's `_scrub_422` leaves behind: three keys per error, and no echo
/// of what the caller sent.
fn key_too_long() -> Response {
    (
        StatusCode::UNPROCESSABLE_ENTITY,
        Json(json!({"detail": [{
            "loc": ["query", "key"],
            "msg": "String should have at most 40 characters",
            "type": "string_too_long"
        }]})),
    )
        .into_response()
}

async fn leaderboards(
    State(st): State<crate::AppState>,
    Path(game): Path<String>,
    Query(q): Query<KeyQ>,
    req: Request,
) -> Response {
    let c = req.extensions().get::<crate::Caller>().expect("caller set by middleware").clone();
    // FastAPI validates the query while it resolves dependencies, before the
    // endpoint body runs, so an over-long key answers 422 even when the game
    // does not exist. (The auth dependency still runs first, so a bad token is
    // 401 either way.)
    if q.key.as_deref().is_some_and(|k| k.chars().count() > 40) {
        return key_too_long();
    }
    if !GAMES.contains(&game.as_str()) {
        return err(StatusCode::NOT_FOUND, "no such game");
    }
    // An empty `?key=` is falsy in Python: it asks for every board, not for the
    // board whose key is "". A key that matches nothing is not an error either.
    let filter = q.key.as_deref().filter(|k| !k.is_empty());
    match build_boards(&st.pool, &game, filter, &c.user_id).await {
        Ok(b) => Json(b).into_response(),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, &format!("{e}")),
    }
}

async fn profile(
    State(st): State<crate::AppState>,
    Path(user_id): Path<String>,
    req: Request,
) -> Response {
    let c = req.extensions().get::<crate::Caller>().expect("caller set by middleware").clone();
    // "me" is the only special case; anything else is truncated to the width of
    // users.id before the lookup, matching app/routes/hq.py's `user_id[:36]`.
    let uid = if user_id == "me" {
        c.user_id.clone()
    } else {
        user_id.chars().take(36).collect::<String>()
    };
    match build_profile(&st.pool, &uid, &c.user_id).await {
        Ok(Some(p)) => Json(p).into_response(),
        Ok(None) => err(StatusCode::NOT_FOUND, "no such trainer"),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, &format!("{e}")),
    }
}

/// Only this group's routes. main.rs applies `require_device` to the whole
/// guarded router, so there is no auth layer here.
pub fn routes() -> Router<crate::AppState> {
    Router::new()
        .route("/v1/progress/me", get(my_progress))
        .route("/v1/leaderboards/:game", get(leaderboards))
        .route("/v1/profile/:user_id", get(profile))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(uid: &str, day: &str, place: i64, players: i64) -> ResultDay {
        ResultDay { user_id: uid.into(), day: day.into(), place, players }
    }

    #[test]
    fn result_xp_matches_the_python_table() {
        assert_eq!(xp_for_result(1, 4), 50);
        assert_eq!(xp_for_result(2, 4), 35);
        assert_eq!(xp_for_result(3, 4), 28);
        assert_eq!(xp_for_result(4, 4), 20);
        // Solo: finishing only, however it placed.
        assert_eq!(xp_for_result(1, 1), 20);
    }

    #[test]
    fn game_xp_is_capped_per_user_per_day() {
        let same_day: Vec<ResultDay> =
            (0..20).map(|_| day("u", "2026-10-08", 1, 2)).collect();
        assert_eq!(game_xp_totals(&same_day).get("u"), Some(&400));

        // The cap is per day, not per lifetime: a second day earns its own 400.
        let two_days: Vec<ResultDay> = (0..20)
            .map(|_| day("u", "2026-10-08", 1, 2))
            .chain((0..20).map(|_| day("u", "2026-10-07", 1, 2)))
            .collect();
        assert_eq!(game_xp_totals(&two_days).get("u"), Some(&800));
    }

    #[test]
    fn game_xp_keeps_users_apart() {
        let rows = vec![day("a", "2026-10-08", 1, 2), day("b", "2026-10-08", 3, 4)];
        let out = game_xp_totals(&rows);
        assert_eq!(out.get("a"), Some(&50));
        assert_eq!(out.get("b"), Some(&28));
    }

    #[test]
    fn progress_is_the_eight_keys_in_the_python_order() {
        // Two kart wins against one other player and no session activity.
        let p = progress_from_xp(0, xp_for_result(1, 2) * 2);
        assert_eq!(
            serde_json::to_string(&p).unwrap(),
            r#"{"level":1,"xp":100,"sessionXp":0,"gameXp":100,"xpIntoLevel":100,"xpForLevel":520,"rank":"Prompt Apprentice","nextLevelAt":640}"#
        );
    }

    #[test]
    fn next_level_at_is_the_next_levels_cost_not_a_total() {
        // Level 2 starts at 520 XP; the quirk is that nextLevelAt is 400+120*3.
        let p = progress_from_xp(520, 0);
        assert_eq!(p.level, 2);
        assert_eq!(p.xp_for_level, 640);
        assert_eq!(p.next_level_at, 760);
    }

    #[test]
    fn round2_rounds_half_to_even_like_python() {
        // The pairs tests/test_results.py asserts literally.
        assert_eq!(round2(60.0 / 1.0), 60.0);
        assert_eq!(round2(50.0 / 2.0), 25.0);
        // Exact ties: Python's round() goes to even, not away from zero.
        assert_eq!(round2(0.125), 0.12);
        assert_eq!(round2(0.375), 0.38);
        assert_eq!(round2(0.625), 0.62);
        assert_eq!(round2(0.875), 0.88);
        // And a value that only looks like a tie.
        assert_eq!(round2(2.675), 2.67);
    }

    fn dates(today: NaiveDate, offsets: &[i64]) -> HashSet<NaiveDate> {
        offsets.iter().map(|d| today - chrono::Duration::days(*d)).collect()
    }

    #[test]
    fn the_streak_is_the_pythons_flex_streak_not_the_strict_one() {
        let today = NaiveDate::from_ymd_opt(2026, 10, 8).unwrap();
        let active = dates(today, &[0, 1, 3]);
        // The gap on today-2 is tolerated: 7 days may hold 2 misses.
        assert_eq!(flex_streak(&active, today), 4);
        // crate::scoring's strict version stops at the gap -- which is exactly
        // why this module carries its own.
        assert_eq!(crate::scoring::streak_from_dates(&active, today), 2);
    }

    #[test]
    fn a_day_in_progress_is_never_charged_as_a_miss() {
        let today = NaiveDate::from_ymd_opt(2026, 10, 8).unwrap();
        assert_eq!(flex_streak(&dates(today, &[1, 2, 3]), today), 3);
    }

    #[test]
    fn the_streak_breaks_on_three_misses_in_a_week() {
        let today = NaiveDate::from_ymd_opt(2026, 10, 8).unwrap();
        // Active today, then a three-day hole: the walk stops inside it and the
        // older run does not join on.
        let active = dates(today, &[0, 4, 5, 6, 7]);
        assert_eq!(flex_streak(&active, today), 1);
    }

    #[test]
    fn the_streak_is_zero_when_cold() {
        let today = NaiveDate::from_ymd_opt(2026, 10, 8).unwrap();
        assert_eq!(flex_streak(&dates(today, &[5]), today), 0);
        assert_eq!(flex_streak(&HashSet::new(), today), 0);
    }

    fn slot(s: &str, item: &str) -> (String, Value) {
        (s.to_string(), Value::String(item.to_string()))
    }

    #[test]
    fn equipped_renders_values_and_drops_bad_entries() {
        let on = vec![slot("kart", "k-neon")];
        assert_eq!(serde_json::to_string(&equipped(&on)).unwrap(), r##"{"kart":"#39ff88"}"##);

        // An unknown id, an id in the wrong slot, and a non-string value all go.
        let bad = vec![
            slot("kart", "k-does-not-exist"),
            slot("frame", "k-neon"),
            ("ball".to_string(), Value::from(7_i64)),
        ];
        assert_eq!(serde_json::to_string(&equipped(&bad)).unwrap(), "{}");
    }

    #[test]
    fn the_catalogue_covers_every_slot() {
        assert_eq!(COSMETICS.len(), 28);
        for s in ["kart", "runner", "blaster", "ball", "frame", "decor"] {
            assert!(COSMETICS.iter().any(|e| e.1 == s), "no item for slot {s}");
        }
    }

    #[test]
    fn stored_slots_keep_the_documents_key_order() {
        let s: StoredSlots =
            serde_json::from_str(r#"{"runner":"r-aqua","kart":"k-neon"}"#).unwrap();
        assert_eq!(s.0[0].0, "runner");
        assert_eq!(
            serde_json::to_string(&equipped(&s.0)).unwrap(),
            r##"{"runner":"#5fd3e6","kart":"#39ff88"}"##
        );
    }

    #[test]
    fn a_per_game_map_serialises_in_insertion_order() {
        let mut p = Obj::default();
        p.set("played", Value::from(2_i64));
        p.set("wins", Value::from(1_i64));
        p.keep_min("best:meadow", 61_000);
        p.keep_min("best:meadow", 70_000);
        p.keep_min("bestLap", 14_900);
        p.bump("played", 1);
        assert_eq!(
            serde_json::to_string(&p).unwrap(),
            r#"{"played":3,"wins":1,"best:meadow":61000,"bestLap":14900}"#
        );
    }

    fn entry(handle: &str, best: Option<i64>, kills: i64, deaths: i64, wpm: Option<f64>) -> Entry {
        Entry {
            user: UserView {
                user_id: handle.into(),
                handle: handle.into(),
                display_name: handle.into(),
                avatar_url: String::new(),
            },
            played: 1,
            wins: 0,
            best,
            best_lap: None,
            kills,
            deaths,
            is_you: false,
            kd: None,
            wpm,
            acc: wpm.map(|_| 97.0),
            rank: 0,
        }
    }

    #[test]
    fn a_timed_board_sorts_by_best_then_handle_and_drops_dnfs() {
        let out = finalise(
            "kart",
            vec![
                entry("zoe", Some(61_000), 0, 0, None),
                entry("ana", Some(61_000), 0, 0, None),
                entry("dnf", None, 0, 0, None),
                entry("bea", Some(60_000), 0, 0, None),
            ],
        );
        let names: Vec<&str> = out.iter().map(|e| e.user.handle.as_str()).collect();
        assert_eq!(names, vec!["bea", "ana", "zoe"]);
        assert_eq!(out.iter().map(|e| e.rank).collect::<Vec<_>>(), vec![1, 2, 3]);
    }

    #[test]
    fn the_td_board_ranks_the_most_waves_first() {
        let out = finalise(
            "td",
            vec![entry("ana", Some(7), 0, 0, None), entry("bea", Some(20), 0, 0, None),
                 entry("cal", None, 0, 0, None)],
        );
        let names: Vec<&str> = out.iter().map(|e| e.user.handle.as_str()).collect();
        assert_eq!(names, vec!["bea", "ana"]);
    }

    #[test]
    fn a_coop_td_row_is_worth_twenty_xp_and_no_win() {
        assert_eq!(xp_for_result(0, 3), 20);
        let xp = game_xp_totals(&[ResultDay { user_id: "u".into(), day: "2026-10-10".into(),
                                              place: 0, players: 3 }]);
        assert_eq!(xp["u"], 20);
    }

    #[test]
    fn the_blaster_board_keeps_everyone_and_carries_a_kd() {
        let out = finalise(
            "fps",
            vec![entry("ana", None, 0, 3, None), entry("bea", None, 9, 2, None)],
        );
        assert_eq!(out[0].user.handle, "bea");
        assert_eq!(out[0].kd, Some(4.5));
        // A player with no kills stays on the board at 0.0, not filtered out.
        assert_eq!(out[1].kd, Some(0.0));
        // fps never gets a `best`, and `best`/`bestLap` are emitted as null.
        assert!(serde_json::to_string(&out[1]).unwrap().contains(r#""best":null"#));
    }

    #[test]
    fn the_typing_board_drops_a_zero_wpm_and_sorts_fastest_first() {
        let out = finalise(
            "type",
            vec![
                entry("ana", Some(30_000), 0, 0, Some(0.0)),
                entry("bea", Some(31_000), 0, 0, Some(74.5)),
                entry("cal", Some(29_000), 0, 0, Some(91.2)),
                entry("dot", Some(29_000), 0, 0, None),
            ],
        );
        let names: Vec<&str> = out.iter().map(|e| e.user.handle.as_str()).collect();
        assert_eq!(names, vec!["cal", "bea"]);
        // A type entry keeps its `best`, which nothing sorted by, and has no kd.
        assert_eq!(out[0].best, Some(29_000));
        assert!(out[0].kd.is_none());
    }

    #[test]
    fn ranks_are_numbered_before_the_board_is_capped() {
        let entries: Vec<Entry> = (0..25)
            .map(|i| entry(&format!("p{i:02}"), Some(60_000 + i), 0, 0, None))
            .collect();
        let out = finalise("plat", entries);
        assert_eq!(out.len(), BOARD_SIZE);
        // 1..=20, never a 21 on a returned row.
        assert_eq!(out[0].rank, 1);
        assert_eq!(out[BOARD_SIZE - 1].rank, 20);
    }

    #[test]
    fn trophies_come_out_in_the_declared_order() {
        let mut per: [Obj; 7] = std::array::from_fn(|_| Obj::default());
        for p in per.iter_mut() {
            p.set("played", Value::from(1_i64));
            p.set("wins", Value::from(0_i64));
        }
        per[FPS].set("kills", Value::from(100_i64));
        per[TYPE].set("bestWpm", Value::from(80.0_f64));
        per[KART].set("bestLap", Value::from(14_999_i64));
        let got: Vec<&str> = trophies(&per, 12, 10, 5, 7).iter().map(|t| t.id).collect();
        assert_eq!(
            got,
            vec![
                "first-game",
                "first-win",
                "ten-wins",
                "podium-5",
                "all-rounder",
                "fast-fingers",
                "sharpshooter",
                "speedster",
                "streak-7",
            ]
        );
    }

    #[test]
    fn an_empty_profile_earns_nothing() {
        let per: [Obj; 7] = std::array::from_fn(|_| Obj::default());
        assert!(trophies(&per, 0, 0, 0, 0).is_empty());
        // One game played is the first trophy and nothing else.
        let got: Vec<&str> = trophies(&per, 1, 0, 0, 0).iter().map(|t| t.id).collect();
        assert_eq!(got, vec!["first-game"]);
    }

    #[test]
    fn extras_degrade_rather_than_fail() {
        let mut extra = Map::new();
        extra.insert("kills".into(), Value::from(7_i64));
        extra.insert("wpm".into(), Value::from(88.5_f64));
        extra.insert("bestLap".into(), Value::from(20_000.0_f64));
        assert_eq!(int_of(&extra, "kills"), 7);
        assert_eq!(int_of(&extra, "deaths"), 0);
        assert_eq!(float_of(&extra, "wpm"), 88.5);
        // isinstance(20000.0, int) is False in Python, so a float lap is ignored.
        assert_eq!(strict_int(&extra, "bestLap"), None);
        extra.insert("bestLap".into(), Value::from(14_900_i64));
        assert_eq!(strict_int(&extra, "bestLap"), Some(14_900));
    }

    #[test]
    fn the_in_clause_is_numbered() {
        assert_eq!(placeholders(1), "?1");
        assert_eq!(placeholders(3), "?1,?2,?3");
    }

    #[tokio::test]
    async fn custom_map_boards_show_only_when_asked_for() {
        let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
        crate::db::migrate(&pool).await.unwrap();
        sqlx::query("INSERT INTO users (id, github_id, handle, display_name, avatar_url,
                     trainer_name, is_active, created_at)
                     VALUES ('u1', 901, 'u1', '', '', '', 1, datetime('now'))")
            .execute(&pool).await.unwrap();
        for key in ["meadow", "c-0123456789ab"] {
            sqlx::query(r#"INSERT INTO game_results (user_id, game, "key", mode, place, players, value, extra)
                           VALUES ('u1', 'kart', ?1, '1 laps', 1, 1, 30000, '{}')"#)
                .bind(key).execute(&pool).await.unwrap();
        }
        let all = build_boards(&pool, "kart", None, "u1").await.unwrap();
        let keys: Vec<&str> = all.boards.iter().map(|b| b.key.as_str()).collect();
        assert_eq!(keys, ["meadow"]);
        let one = build_boards(&pool, "kart", Some("c-0123456789ab"), "u1").await.unwrap();
        assert_eq!(one.boards.len(), 1);
        assert_eq!(one.boards[0].entries.len(), 1);
    }
}
