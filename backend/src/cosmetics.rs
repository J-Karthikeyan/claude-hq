//! HQ 2.1 cosmetics, ported from `backend/app/cosmetics.py`.
//!
//! One catalogue. An item is bought with Poke Coins through the pantry journal
//! (owning it is a `poke_balances` row `cos:<id>` at quantity 1) or unlocks at
//! an HQ level, which needs no row at all -- ownership of a level unlock is
//! recomputed from the caller's XP on every read. You wear one per slot.
//!
//! Cosmetic only: nothing here writes to daily_stats, game_results, XP or any
//! leaderboard. It reads the level, and that is the whole interaction with
//! scoring.
//!
//! All three endpoints are strictly self-scoped: no request carries a user id,
//! and there is no path by which one caller sees another's catalogue state or
//! purse. The public surface (room member info, the websocket welcome's
//! `you.cos`, GET /v1/profile/{id}) exposes only slot -> rendered value, and it
//! is built elsewhere -- see the note on `equipped()` at the end of this file.
//!
//! `/v1/market/sell` shares the Python file and service module but is a
//! different router, and is not part of this group.

use crate::scoring::{derive_level, xp_from_counts};
// main.rs's `err` builds the {"detail": "..."} envelope every endpoint shares.
use crate::{err, AppState, Caller};
use axum::{
    extract::{Request, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use chrono::Utc;
use serde::Serialize;
use serde_json::{json, Map, Value};
use sqlx::{Row, SqliteConnection, SqlitePool};
use std::collections::{HashMap, HashSet};

/// The six slots and their labels, in the Python's declaration order.
///
/// A struct rather than a map because serde_json is built without
/// `preserve_order`, so any map it serialises comes out sorted by key -- and
/// the client renders the picker in this order. `SLOT_IDS` repeats the keys for
/// the `unknown slot` check; a test keeps the two in step.
#[derive(Serialize)]
struct Slots {
    kart: &'static str,
    runner: &'static str,
    blaster: &'static str,
    ball: &'static str,
    frame: &'static str,
    decor: &'static str,
}

const SLOTS: Slots = Slots {
    kart: "Kart paint",
    // British spelling, as the Python emits it. The client matches on the key,
    // not the label, but this string is on the wire.
    runner: "Runner colour",
    blaster: "Blaster skin",
    ball: "Golf ball",
    frame: "Name frame",
    decor: "HQ decor",
};

const SLOT_IDS: [&str; 6] = ["kart", "runner", "blaster", "ball", "frame", "decor"];

/// One catalogue row.
///
/// Two sentinels, carried over verbatim: `price == 0` means not for sale, and
/// `level == 0` means no level is needed. The shipped table never sets both,
/// but nothing enforces that -- `locked` tests only the level and `owned`
/// ignores the price -- so a hypothetical priced *and* gated item would be
/// buyable while locked. That is the Python's behaviour and not a bug to fix
/// here.
struct Entry {
    id: &'static str,
    slot: &'static str,
    name: &'static str,
    value: &'static str,
    price: i64,
    level: i64,
}

const fn e(
    id: &'static str,
    slot: &'static str,
    name: &'static str,
    value: &'static str,
    price: i64,
    level: i64,
) -> Entry {
    Entry { id, slot, name, value, price, level }
}

/// Declaration order is response order: `items` iterates this table, never the
/// owned set.
const CATALOG: &[Entry] = &[
    e("k-neon", "kart", "Neon green paint", "#39ff88", 20, 0),
    e("k-midnight", "kart", "Midnight paint", "#1d2b5c", 12, 0),
    e("k-sunset", "kart", "Sunset paint", "#ff7b54", 12, 0),
    e("k-gold", "kart", "Gold paint", "#d8b34a", 0, 20),
    e("r-ember", "runner", "Ember runner", "#ff6b5b", 10, 0),
    e("r-aqua", "runner", "Aqua runner", "#5fd3e6", 10, 0),
    e("r-gold", "runner", "Gold runner", "#d8b34a", 0, 25),
    e("g-chrome", "blaster", "Chrome blaster", "#c0c8d0", 10, 0),
    e("g-ember", "blaster", "Ember blaster", "#ff6b5b", 10, 0),
    e("g-void", "blaster", "Void blaster", "#2c2c34", 15, 0),
    e("b-pink", "ball", "Pink ball", "#ff9ad5", 6, 0),
    e("b-lime", "ball", "Lime ball", "#b6ff5c", 6, 0),
    e("b-gold", "ball", "Gold ball", "#d8b34a", 0, 15),
    e("f-brass", "frame", "Brass frame", "#d8b34a", 8, 0),
    e("f-neon", "frame", "Neon frame", "#5fd3e6", 8, 0),
    e("f-crimson", "frame", "Crimson frame", "#ff6b5b", 8, 0),
    e("f-legend", "frame", "Legend frame", "#9b8cf0", 0, 40),
    e("d-flags", "decor", "Rooftop flags", "flags", 10, 0),
    e("d-gnomes", "decor", "Plaza gnomes", "gnomes", 8, 0),
    e("d-fireworks", "decor", "Fireworks", "fireworks", 25, 0),
    e("d-neon", "decor", "Neon outline", "neon", 0, 30),
    // 2.5 grant-only (loot, prestige)
    e("k-prism", "kart", "Prism paint", "#b388ff", 0, 0),
    e("r-shadow", "runner", "Shadow runner", "#3a3550", 0, 0),
    e("g-plasma", "blaster", "Plasma blaster", "#7df9ff", 0, 0),
    e("b-pokeball", "ball", "Poké Ball", "#e3350d", 0, 0),
    e("f-holo", "frame", "Holo frame", "#a0f0ff", 0, 0),
    e("f-star", "frame", "Prestige star frame", "#f5d76e", 0, 0),
    e("d-crown", "decor", "Rooftop crown", "crown", 0, 0),
];

/// Grant-only: never sold, never level-unlocked, owned only via a cos: row.
const GRANT_ONLY: [&str; 7] = ["k-prism", "r-shadow", "g-plasma", "b-pokeball", "f-holo", "f-star", "d-crown"];
/// Grant-only rows that can never be traded away.
#[allow(dead_code)]
pub(crate) const SOULBOUND: [&str; 2] = ["f-star", "d-crown"];

/// 200 ledger rows per user per UTC day, counted inclusive of the row just
/// written, so the 201st op of the day is refused.
const MAX_OPS_PER_DAY: i64 = 200;
/// Game XP is clamped per UTC day, not once over the total.
const GAME_XP_DAY_CAP: i64 = 400;

const REUSED: &str = "that requestId was already used for a different request";
const BUSY: &str = "busy, try again";
const OWN_ALREADY: &str = "you already own that";

fn find(cid: &str) -> Option<&'static Entry> {
    CATALOG.iter().find(|it| it.id == cid)
}

fn item_key(cid: &str) -> String {
    format!("cos:{cid}")
}

/// The Python slices four characters off the balance key rather than parsing
/// it, and SQLite's LIKE is case-insensitive for ASCII, so a `COS:` row is
/// matched and sliced too. Kept as a slice for exactly that reason.
fn strip_cos(item: &str) -> String {
    item.chars().skip(4).collect()
}

// --- wire shapes ----------------------------------------------------------

#[derive(Serialize)]
struct ItemView {
    id: &'static str,
    slot: &'static str,
    name: &'static str,
    value: &'static str,
    price: i64,
    level: i64,
    owned: bool,
    equipped: bool,
    locked: bool,
}

#[derive(Serialize)]
pub(crate) struct StateView {
    slots: Slots,
    items: Vec<ItemView>,
    coins: i64,
    level: i64,
}

/// One Pydantic error entry, scrubbed to the three keys app/main.py's
/// `_scrub_422` keeps (`input`, `ctx` and `url` are dropped), in its order.
#[derive(Debug, Serialize)]
struct VErr {
    loc: Vec<String>,
    msg: String,
    #[serde(rename = "type")]
    kind: &'static str,
}

fn verr(field: &str, msg: &str, kind: &'static str) -> VErr {
    VErr { loc: vec!["body".into(), field.into()], msg: msg.into(), kind }
}

/// The 422 body is a *list* of those entries, not the single-string envelope
/// `err` produces. The existing JS client distinguishes them.
fn invalid(errs: Vec<VErr>) -> Response {
    (StatusCode::UNPROCESSABLE_ENTITY, Json(json!({ "detail": errs }))).into_response()
}

// --- request validation ---------------------------------------------------
// Hand-rolled rather than derived, because the status code and the body shape
// have to match Pydantic's: the error `type` strings and messages below are
// pydantic-core's own, and the constraint order (min, then max, then pattern,
// one error per field) is the order pydantic-core applies them in.

fn body_object(bytes: &[u8]) -> Result<Map<String, Value>, Vec<VErr>> {
    match serde_json::from_slice::<Value>(bytes) {
        Ok(Value::Object(o)) => Ok(o),
        Ok(_) => Err(vec![VErr {
            loc: vec!["body".into()],
            msg: "Input should be a valid dictionary or object to extract fields from".into(),
            kind: "model_attributes_type",
        }]),
        Err(_) => Err(vec![VErr {
            loc: vec!["body".into()],
            msg: "JSON decode error".into(),
            kind: "json_invalid",
        }]),
    }
}

fn take_str(obj: &Map<String, Value>, field: &str, errs: &mut Vec<VErr>) -> Option<String> {
    match obj.get(field) {
        Some(Value::String(s)) => Some(s.clone()),
        None => {
            errs.push(verr(field, "Field required", "missing"));
            None
        }
        Some(_) => {
            errs.push(verr(field, "Input should be a valid string", "string_type"));
            None
        }
    }
}

/// `true` when the string satisfies every constraint. `min` of 0 is "no
/// minimum", the way the Python's `Field(max_length=...)` leaves it out.
///
/// Lengths are in characters, as Pydantic counts them. Every bound in this
/// module is above one, so the message never needs the singular form Pydantic
/// would use.
fn constrain(
    s: &str,
    field: &str,
    min: usize,
    max: usize,
    pattern: bool,
    errs: &mut Vec<VErr>,
) -> bool {
    let n = s.chars().count();
    if n < min {
        let msg = format!("String should have at least {min} characters");
        errs.push(verr(field, &msg, "string_too_short"));
        return false;
    }
    if n > max {
        let msg = format!("String should have at most {max} characters");
        errs.push(verr(field, &msg, "string_too_long"));
        return false;
    }
    // ^[A-Za-z0-9_-]+$ spelled out: the anchors make it a full match, and the
    // `+` is why the empty string fails.
    if pattern && (n == 0 || !s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'))
    {
        errs.push(verr(field, "String should match pattern '^[A-Za-z0-9_-]+$'", "string_pattern_mismatch"));
        return false;
    }
    true
}

/// `extra="forbid"`. Pydantic reports the declared fields first and the unknown
/// keys afterwards, so this is always called last.
fn forbid_extra(obj: &Map<String, Value>, known: &[&str], errs: &mut Vec<VErr>) {
    for k in obj.keys().filter(|k| !known.contains(&k.as_str())) {
        errs.push(verr(k, "Extra inputs are not permitted", "extra_forbidden"));
    }
}

/// `BuyBody`: requestId (8..=64, `^[A-Za-z0-9_-]+$`) and item (..=16).
/// `item` has no minimum, so `""` passes validation here and is refused by the
/// catalogue check with a 400.
fn buy_body(bytes: &[u8]) -> Result<(String, String), Vec<VErr>> {
    let obj = body_object(bytes)?;
    let mut errs = Vec::new();
    let rid = take_str(&obj, "requestId", &mut errs);
    let rid = rid.filter(|s| constrain(s, "requestId", 8, 64, true, &mut errs));
    let item = take_str(&obj, "item", &mut errs);
    let item = item.filter(|s| constrain(s, "item", 0, 16, false, &mut errs));
    forbid_extra(&obj, &["requestId", "item"], &mut errs);
    match (rid, item) {
        (Some(r), Some(i)) if errs.is_empty() => Ok((r, i)),
        _ => Err(errs),
    }
}

/// `EquipBody`: slot (..=12) and an optional item (..=16). Omitting `item`
/// entirely is identical to sending null -- both unequip the slot.
fn equip_body(bytes: &[u8]) -> Result<(String, Option<String>), Vec<VErr>> {
    let obj = body_object(bytes)?;
    let mut errs = Vec::new();
    let slot = take_str(&obj, "slot", &mut errs);
    let slot = slot.filter(|s| constrain(s, "slot", 0, 12, false, &mut errs));
    let item = match obj.get("item") {
        None | Some(Value::Null) => Some(None),
        // Not a match guard: a guard may not take a mutable borrow.
        Some(Value::String(s)) => {
            if constrain(s, "item", 0, 16, false, &mut errs) {
                Some(Some(s.clone()))
            } else {
                None
            }
        }
        Some(_) => {
            errs.push(verr("item", "Input should be a valid string", "string_type"));
            None
        }
    };
    forbid_extra(&obj, &["slot", "item"], &mut errs);
    match (slot, item) {
        (Some(s), Some(i)) if errs.is_empty() => Ok((s, i)),
        _ => Err(errs),
    }
}

// --- reads ----------------------------------------------------------------

/// Finishing is worth 20; winning a game with others 30 more, 2nd 15, 3rd 8.
fn xp_for_result(place: i64, players: i64) -> i64 {
    let mut xp = 20;
    if players >= 2 {
        xp += match place {
            1 => 30,
            2 => 15,
            3 => 8,
            _ => 0,
        };
    }
    xp
}

/// Game XP, bucketed by the UTC date of `game_results.at` and clamped per day.
///
/// The clamp is per bucket, not over the total: summing first and clamping once
/// gives a heavy player a different level, which would change which items
/// report `locked` and `owned`. Applied as each row lands, exactly as the
/// Python does -- every term is non-negative, so that is the same as clamping
/// the day's total, and keeping the shape makes the two easy to compare.
fn game_xp_from_rows(rows: &[(String, i64, i64)]) -> i64 {
    let mut per_day: HashMap<&str, i64> = HashMap::new();
    for (day, place, players) in rows {
        let running = per_day.entry(day.as_str()).or_insert(0);
        *running = GAME_XP_DAY_CAP.min(*running + xp_for_result(*place, *players));
    }
    per_day.values().sum()
}

/// The caller's HQ level: session XP + game XP, the same walk
/// `results.progress()` does.
async fn hq_level(pool: &SqlitePool, uid: &str) -> Result<i64, sqlx::Error> {
    let row = sqlx::query(
        "SELECT COALESCE(SUM(prompts),0)   AS prompts,
                COALESCE(SUM(tools),0)     AS tools,
                COALESCE(SUM(artifacts),0) AS artifacts
         FROM daily_stats WHERE user_id = ?1 GROUP BY user_id",
    )
    .bind(uid)
    .fetch_optional(pool)
    .await?;
    let session = match row {
        Some(r) => xp_from_counts(r.get("prompts"), r.get("tools"), r.get("artifacts")),
        None => 0,
    };

    let rows = sqlx::query("SELECT DATE(at) AS day, place, players FROM game_results
                            WHERE user_id = ?1")
        .bind(uid)
        .fetch_all(pool)
        .await?;
    // The Python falls back to "now" for a missing timestamp; `at` is NOT NULL,
    // so this only stands in for a value DATE() cannot read.
    let today = Utc::now().date_naive().to_string();
    let games: Vec<(String, i64, i64)> = rows
        .iter()
        .map(|r| {
            (
                r.try_get("day").unwrap_or_else(|_| today.clone()),
                r.get("place"),
                r.get("players"),
            )
        })
        .collect();

    Ok(derive_level(session + game_xp_from_rows(&games)).0)
}

/// The ids a level alone grants, in catalogue order.
///
/// Pure, and deliberately separate from `owned`: a level unlock has no balance
/// row behind it, so this is recomputed from the caller's XP on every read --
/// and could in principle be lost again if XP were removed.
fn level_unlocks(level: i64) -> impl Iterator<Item = &'static str> {
    CATALOG.iter().filter(move |it| it.level != 0 && level >= it.level).map(|it| it.id)
}

/// Every cosmetic id the caller owns.
///
/// Not guaranteed to be a subset of the catalogue: a `cos:` balance row for an
/// id that has since left the catalogue stays in the set. It is never rendered,
/// because `items` iterates the catalogue, but code downstream must not assume
/// membership implies a catalogue entry.
async fn owned(pool: &SqlitePool, uid: &str, level: i64) -> Result<HashSet<String>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT item FROM poke_balances
         WHERE user_id = ?1 AND item LIKE 'cos:%' AND qty > 0",
    )
    .bind(uid)
    .fetch_all(pool)
    .await?;
    let mut have: HashSet<String> =
        rows.iter().map(|r| strip_cos(&r.get::<String, _>("item"))).collect();
    have.extend(level_unlocks(level).map(String::from));
    Ok(have)
}

/// The stored slot -> item-id map, used raw.
async fn equipped_map(pool: &SqlitePool, uid: &str) -> Result<Map<String, Value>, sqlx::Error> {
    let row = sqlx::query("SELECT slots FROM equipped_cosmetics WHERE user_id = ?1")
        .bind(uid)
        .fetch_optional(pool)
        .await?;
    // Absent row, SQL NULL and unreadable text all mean "wearing nothing",
    // which is what the Python's `(row.slots if row else {}) or {}` says.
    let raw: Option<String> = row.and_then(|r| r.try_get("slots").ok());
    Ok(raw
        .and_then(|s| serde_json::from_str::<Map<String, Value>>(&s).ok())
        .unwrap_or_default())
}

async fn qty_of<'e, E>(ex: E, uid: &str, item: &str) -> Result<Option<i64>, sqlx::Error>
where
    E: 'e + sqlx::Executor<'e, Database = sqlx::Sqlite>,
{
    sqlx::query_scalar::<_, i64>("SELECT qty FROM poke_balances
                                  WHERE user_id = ?1 AND item = ?2")
        .bind(uid)
        .bind(item)
        .fetch_optional(ex)
        .await
}

fn item_views(level: i64, have: &HashSet<String>, on: &Map<String, Value>) -> Vec<ItemView> {
    CATALOG
        .iter()
        .filter(|it| !GRANT_ONLY.contains(&it.id) || have.contains(it.id))
        .map(|it| ItemView {
            id: it.id,
            slot: it.slot,
            name: it.name,
            value: it.value,
            price: it.price,
            level: it.level,
            owned: have.contains(it.id),
            // The raw stored map, not the filtered public view: a stale entry
            // whose item left the catalogue or changed slot still reads as
            // equipped here while `equipped()` drops it. The two can disagree,
            // and the Python has the same seam.
            equipped: on.get(it.slot).and_then(Value::as_str) == Some(it.id),
            locked: it.level != 0 && level < it.level,
        })
        .collect()
}

pub(crate) async fn build_state(pool: &SqlitePool, uid: &str) -> Result<StateView, sqlx::Error> {
    let level = hq_level(pool, uid).await?;
    let have = owned(pool, uid, level).await?;
    let on = equipped_map(pool, uid).await?;
    // Absent row and a stored 0 are indistinguishable on the wire; this is
    // always an integer, never null.
    let coins = qty_of(pool, uid, "coins").await?.unwrap_or(0);
    Ok(StateView { slots: SLOTS, items: item_views(level, &have, &on), coins, level })
}

/// Every endpoint in this group answers with the whole state, re-read after
/// whatever it wrote has committed.
async fn state_response(pool: &SqlitePool, uid: &str) -> Response {
    match build_state(pool, uid).await {
        Ok(s) => Json(s).into_response(),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, &format!("{e}")),
    }
}

// --- the spend ------------------------------------------------------------

/// What a refused buy becomes.
///
/// The Python splits these across EconomyError, IntegrityError and
/// OperationalError; sqlx hands every database failure back as one type, so the
/// split happens in `classify`.
enum Refuse {
    /// A guard said no: roll back and surface it with this status.
    Say(StatusCode, String),
    /// This requestId already committed: answer with the state, charge nothing.
    Replay,
    /// A UNIQUE or CHECK conflict -- usually a concurrent submit of the same id.
    Integrity,
    /// SQLite's busy_timeout ran out under a burst of writers.
    Busy,
    Db(sqlx::Error),
}

fn classify(e: sqlx::Error) -> Refuse {
    let seen = e.as_database_error().map(|d| {
        let primary = d.code().and_then(|c| c.parse::<i32>().ok()).map(|c| c & 0xff);
        // UNIQUE(user_id, request_id) is SQLITE_CONSTRAINT_UNIQUE (extended code
        // 2067), which sqlx reports as a unique violation; the message check is
        // belt and braces, because misreading this one would 500 where the
        // Python replays the winner of the race with a 200.
        let integrity = d.is_unique_violation()
            || d.is_check_violation()
            || d.is_foreign_key_violation()
            || d.message().contains("UNIQUE constraint failed");
        (integrity, primary)
    });
    match seen {
        Some((true, _)) => Refuse::Integrity,
        // SQLITE_BUSY and SQLITE_LOCKED, masked out of their extended codes.
        Some((false, Some(5 | 6))) => Refuse::Busy,
        _ => Refuse::Db(e),
    }
}

/// The committed op for this request id. `Some(true)` matches this buy and can
/// be replayed; `Some(false)` means the id was spent on something else.
async fn prior<'e, E>(
    ex: E,
    uid: &str,
    rid: &str,
    key: &str,
    price: i64,
) -> Result<Option<bool>, sqlx::Error>
where
    E: 'e + sqlx::Executor<'e, Database = sqlx::Sqlite>,
{
    let row = sqlx::query(
        "SELECT op, kind, qty, coins, to_user_id FROM poke_ledger
         WHERE user_id = ?1 AND request_id = ?2",
    )
    .bind(uid)
    .bind(rid)
    .fetch_optional(ex)
    .await?;
    Ok(row.map(|r| {
        r.get::<String, _>("op") == "cosmetic"
            && r.try_get::<Option<String>, _>("kind").ok().flatten().as_deref() == Some(key)
            && r.get::<i64, _>("qty") == 1
            && r.get::<i64, _>("coins") == price
            && r.try_get::<Option<String>, _>("to_user_id").ok().flatten().is_none()
    }))
}

/// The replay fast path looks at the request id alone -- NOT the full
/// comparison `prior` makes.
///
/// So reusing an id that was spent on anything else (a pantry food buy, an eat,
/// a gift, a sell) answers 200 with the unchanged state and buys nothing,
/// rather than 409. That is deliberate parity with the Python: the reuse 409 is
/// unreachable on this path and only fires from the race below.
async fn find_op(pool: &SqlitePool, uid: &str, rid: &str) -> Result<bool, sqlx::Error> {
    Ok(sqlx::query_scalar::<_, String>(
        "SELECT id FROM poke_ledger WHERE user_id = ?1 AND request_id = ?2",
    )
    .bind(uid)
    .bind(rid)
    .fetch_optional(pool)
    .await?
    .is_some())
}

/// One buy, in the journal-first order, with no savepoints: any refusal rolls
/// the whole thing back including the journal row, so a retry with the same
/// request id re-executes rather than replaying a failure.
async fn spend_on(
    tx: &mut SqliteConnection,
    uid: &str,
    rid: &str,
    key: &str,
    price: i64,
    note: &str,
    today: &str,
) -> Result<(), Refuse> {
    // The caller already returned for every committed row it could see, so this
    // only fires when a concurrent submit committed between the two reads.
    match prior(&mut *tx, uid, rid, key, price).await.map_err(classify)? {
        Some(true) => return Err(Refuse::Replay),
        Some(false) => return Err(Refuse::Say(StatusCode::CONFLICT, REUSED.into())),
        None => {}
    }

    // The journal row is deliberately the first write: it takes SQLite's write
    // lock here and claims UNIQUE(user_id, request_id), which is what makes the
    // count below exact under concurrency and the idempotency key atomic.
    // `note` is the catalogue display name; delivered_at stays NULL.
    sqlx::query(
        "INSERT INTO poke_ledger
           (id, user_id, request_id, op, op_date, kind, qty, coins, to_user_id, note, created_at)
         VALUES (?1, ?2, ?3, 'cosmetic', ?4, ?5, 1, ?6, NULL, ?7, ?8)",
    )
    .bind(uuid::Uuid::new_v4().to_string())
    .bind(uid)
    .bind(rid)
    .bind(today)
    .bind(key)
    .bind(price)
    .bind(note)
    // The format SQLAlchemy stores a DATETIME in, so both backends write the
    // same bytes and either can read the other's rows.
    .bind(Utc::now().format("%Y-%m-%d %H:%M:%S%.6f").to_string())
    .execute(&mut *tx)
    .await
    .map_err(classify)?;

    // Read under that lock, so it includes the row just inserted: the 201st op
    // of the day is the one that fails. Reordering these two turns the cap into
    // a racy read.
    let ops: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM poke_ledger WHERE user_id = ?1 AND op_date = ?2",
    )
    .bind(uid)
    .bind(today)
    .fetch_one(&mut *tx)
    .await
    .map_err(classify)?;
    if ops > MAX_OPS_PER_DAY {
        return Err(Refuse::Say(
            StatusCode::TOO_MANY_REQUESTS,
            "that's a lot of pantry activity for one day, try again tomorrow".into(),
        ));
    }

    // Coins first, item second. There are no savepoints, so a failed credit
    // below discards this debit too -- nobody is charged for nothing.
    let paid = sqlx::query(
        "UPDATE poke_balances SET qty = qty - ?1, updated_at = CURRENT_TIMESTAMP
         WHERE user_id = ?2 AND item = 'coins' AND qty >= ?3",
    )
    .bind(price)
    .bind(uid)
    .bind(price)
    .execute(&mut *tx)
    .await
    .map_err(classify)?;
    if paid.rows_affected() != 1 {
        // Re-read after the failed UPDATE; 0 when there is no coins row at all.
        let have = qty_of(&mut *tx, uid, "coins").await.map_err(classify)?.unwrap_or(0);
        return Err(Refuse::Say(
            StatusCode::CONFLICT,
            format!("not enough Poke Coins (need {price}, you have {have})"),
        ));
    }

    // The cap of 1 lives in this WHERE clause, not the schema, and is what
    // makes a cosmetic a single-copy item.
    let credited = sqlx::query(
        "UPDATE poke_balances SET qty = qty + 1, updated_at = CURRENT_TIMESTAMP
         WHERE user_id = ?1 AND item = ?2 AND qty + 1 <= 1",
    )
    .bind(uid)
    .bind(key)
    .execute(&mut *tx)
    .await
    .map_err(classify)?;
    if credited.rows_affected() != 1 {
        if qty_of(&mut *tx, uid, key).await.map_err(classify)?.is_some() {
            return Err(Refuse::Say(StatusCode::CONFLICT, OWN_ALREADY.into()));
        }
        sqlx::query("INSERT INTO poke_balances (id, user_id, item, qty) VALUES (?1, ?2, ?3, 1)")
            .bind(uuid::Uuid::new_v4().to_string())
            .bind(uid)
            .bind(key)
            .execute(&mut *tx)
            .await
            .map_err(classify)?;
    }
    Ok(())
}

// --- handlers -------------------------------------------------------------

async fn get_state(State(st): State<AppState>, req: Request) -> Response {
    let c = req.extensions().get::<Caller>().expect("caller set by middleware").clone();
    state_response(&st.pool, &c.user_id).await
}

async fn buy(State(st): State<AppState>, req: Request) -> Response {
    let c = req.extensions().get::<Caller>().expect("caller set by middleware").clone();
    let (_, body) = req.into_parts();
    let bytes = match axum::body::to_bytes(body, 8 * 1024 * 1024).await {
        Ok(b) => b,
        Err(_) => return err(StatusCode::BAD_REQUEST, "body too large"),
    };
    let (rid, cid) = match buy_body(&bytes) {
        Ok(v) => v,
        Err(errs) => return invalid(errs),
    };

    // Guard order is load-bearing. The catalogue check runs BEFORE the
    // idempotency lookup, so a replayed request id that once named an
    // unsaleable item still 400s rather than answering with the cached state.
    // Price 0 is the "not for sale" sentinel: the level unlocks can never be
    // bought, at any request id, and only the server prices an item -- the
    // request carries an id, never an amount.
    let Some(it) = find(&cid).filter(|it| it.price != 0) else {
        return err(StatusCode::BAD_REQUEST, "that item isn't for sale");
    };
    let key = item_key(&cid);

    match find_op(&st.pool, &c.user_id, &rid).await {
        Ok(true) => return state_response(&st.pool, &c.user_id).await,
        Ok(false) => {}
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, &format!("{e}")),
    }

    // A truthiness test in the Python: a row sitting at qty 0 is falsy and
    // falls through, and the conditional credit then succeeds on that same row.
    // "a row exists" is the wrong test here.
    match qty_of(&st.pool, &c.user_id, &key).await {
        Ok(q) if q.unwrap_or(0) != 0 => return err(StatusCode::CONFLICT, OWN_ALREADY),
        Ok(_) => {}
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, &format!("{e}")),
    }

    let today = Utc::now().date_naive().to_string();
    let mut tx = match st.pool.begin().await {
        Ok(t) => t,
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, &format!("{e}")),
    };
    let mut res = spend_on(&mut tx, &c.user_id, &rid, &key, it.price, it.name, &today).await;
    if res.is_ok() {
        // The commit is inside the fallible region: a conflict raised here is
        // the same race as one raised above.
        res = tx.commit().await.map_err(classify);
    } else {
        let _ = tx.rollback().await;
    }

    match res {
        // Buying does not equip; equipped_cosmetics is untouched.
        Ok(()) | Err(Refuse::Replay) => state_response(&st.pool, &c.user_id).await,
        Err(Refuse::Say(code, msg)) => err(code, &msg),
        Err(Refuse::Integrity) => match prior(&st.pool, &c.user_id, &rid, &key, it.price).await {
            // Almost always a concurrent submit of the same request id that won
            // the race: replay it. Anything else is a transient conflict.
            Ok(Some(true)) => state_response(&st.pool, &c.user_id).await,
            Ok(Some(false)) => err(StatusCode::CONFLICT, REUSED),
            Ok(None) => err(StatusCode::SERVICE_UNAVAILABLE, BUSY),
            Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, &format!("{e}")),
        },
        Err(Refuse::Busy) => err(StatusCode::SERVICE_UNAVAILABLE, BUSY),
        Err(Refuse::Db(e)) => err(StatusCode::INTERNAL_SERVER_ERROR, &format!("{e}")),
    }
}

async fn equip(State(st): State<AppState>, req: Request) -> Response {
    let c = req.extensions().get::<Caller>().expect("caller set by middleware").clone();
    let (_, body) = req.into_parts();
    let bytes = match axum::body::to_bytes(body, 8 * 1024 * 1024).await {
        Ok(b) => b,
        Err(_) => return err(StatusCode::BAD_REQUEST, "body too large"),
    };
    let (slot, cid) = match equip_body(&bytes) {
        Ok(v) => v,
        Err(errs) => return invalid(errs),
    };

    // The slot is checked first, before anything about the item.
    if !SLOT_IDS.contains(&slot.as_str()) {
        return err(StatusCode::BAD_REQUEST, "unknown slot");
    }
    if let Some(cid) = cid.as_deref() {
        if find(cid).filter(|it| it.slot == slot).is_none() {
            return err(StatusCode::BAD_REQUEST, "that item doesn't go in that slot");
        }
        // Only this branch walks the XP: unequipping never computes a level.
        let level = match hq_level(&st.pool, &c.user_id).await {
            Ok(l) => l,
            Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, &format!("{e}")),
        };
        // Ownership is validated at equip time only, and never re-checked on
        // read, so the worn set is append-only in practice.
        match owned(&st.pool, &c.user_id, level).await {
            Ok(have) if !have.contains(cid) => {
                return err(StatusCode::CONFLICT, "you don't own that yet")
            }
            Ok(_) => {}
            Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, &format!("{e}")),
        }
    }

    let mut on = match equipped_map(&st.pool, &c.user_id).await {
        Ok(m) => m,
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, &format!("{e}")),
    };
    // Exactly one key moves; the other five slots survive, stale entries
    // included.
    match cid {
        None => {
            on.remove(&slot);
        }
        Some(cid) => {
            on.insert(slot, Value::String(cid));
        }
    }

    // The Python commits unconditionally, even when the map is unchanged, and
    // the updated_at touch is observable -- so no "nothing changed"
    // short-circuit here either.
    let write = sqlx::query(
        "INSERT INTO equipped_cosmetics (user_id, slots, updated_at)
         VALUES (?1, ?2, CURRENT_TIMESTAMP)
         ON CONFLICT(user_id) DO UPDATE SET slots = excluded.slots,
                                            updated_at = CURRENT_TIMESTAMP",
    )
    .bind(&c.user_id)
    .bind(Value::Object(on).to_string())
    .execute(&st.pool)
    .await;
    if let Err(e) = write {
        return err(StatusCode::INTERNAL_SERVER_ERROR, &format!("{e}"));
    }
    state_response(&st.pool, &c.user_id).await
}

/// Only this group's routes. main.rs applies `require_device` to the whole
/// guarded router, so there is no auth layer here. No trailing slashes and no
/// redirects: FastAPI's `prefix + ""` produces the bare path.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/cosmetics", get(get_state))
        .route("/v1/cosmetics/buy", post(buy))
        .route("/v1/cosmetics/equip", post(equip))
}

// The public cosmetics surface -- `cosmetics.equipped()`, which turns the
// stored slot -> item-id map into slot -> rendered value for room member info,
// the websocket welcome's `you.cos` and GET /v1/profile/{id}, exposing no item
// ids, prices, levels, owned set or coin balance -- belongs to whoever ports
// those read paths. It is not here because an unused `pub fn` in a binary crate
// is a dead_code warning, and this crate builds with -D warnings. Two notes for
// that port: it filters each stored entry by `c in CATALOG && CATALOG[c].slot
// == s` (dropping a stale entry from the view while leaving it in the stored
// map), and it never re-checks ownership.

/// The worn cosmetics for one user, as `slot -> value`, for the room roster.
/// Mirrors `cosmetics.equipped(db, [uid])[uid]` in the Python: a slot only
/// counts when the stored key is in the catalogue AND is declared for that
/// slot, so a stale or mismatched key is dropped rather than drawn.
pub async fn equipped_one(pool: &sqlx::SqlitePool, user_id: &str) -> serde_json::Value {
    let row: Option<(String,)> =
        sqlx::query_as("SELECT slots FROM equipped_cosmetics WHERE user_id = ?1")
            .bind(user_id)
            .fetch_optional(pool)
            .await
            .unwrap_or(None);
    let Some((raw,)) = row else { return serde_json::Value::Null };
    let Ok(slots) = serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&raw) else {
        return serde_json::Value::Null;
    };
    let mut out = serde_json::Map::new();
    for (slot, key) in slots {
        let Some(k) = key.as_str() else { continue };
        if let Some(item) = CATALOG.iter().find(|c| c.id == k) {
            if item.slot == slot {
                out.insert(slot, serde_json::Value::String(item.value.to_string()));
            }
        }
    }
    if out.is_empty() { serde_json::Value::Null } else { serde_json::Value::Object(out) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(errs: &[VErr]) -> Vec<&str> {
        errs.iter().map(|e| e.kind).collect()
    }

    #[test]
    fn the_catalogue_matches_the_python() {
        assert_eq!(CATALOG.len(), 28);
        let ids: Vec<&str> = CATALOG.iter().map(|it| it.id).collect();
        assert_eq!(ids[0], "k-neon");
        assert_eq!(ids[3], "k-gold");
        assert_eq!(ids[20], "d-neon");
        for it in CATALOG {
            assert!(SLOT_IDS.contains(&it.slot), "{} sits in no slot: {}", it.id, it.slot);
            // The two sentinels are mutually exclusive in the shipped table.
            // Asserted here, not relied on by the flags.
            assert!((it.price == 0) != (it.level == 0) || GRANT_ONLY.contains(&it.id), "{} sets both sentinels", it.id);
            // poke_balances.item is VARCHAR(16) and the key is "cos:" + id.
            assert!(item_key(it.id).len() <= 16, "{} is too long to store", it.id);
        }
    }

    #[test]
    fn the_slot_labels_keep_the_python_order() {
        let j = serde_json::to_string(&SLOTS).unwrap();
        assert_eq!(
            j,
            r#"{"kart":"Kart paint","runner":"Runner colour","blaster":"Blaster skin","ball":"Golf ball","frame":"Name frame","decor":"HQ decor"}"#
        );
        for s in SLOT_IDS {
            assert!(j.contains(&format!("\"{s}\":")), "{s} is missing a label");
        }
    }

    #[test]
    fn the_state_object_keeps_the_python_key_order() {
        let s = StateView {
            slots: SLOTS,
            items: item_views(1, &HashSet::new(), &Map::new()),
            coins: 0,
            level: 1,
        };
        let j = serde_json::to_string(&s).unwrap();
        assert!(j.starts_with(r#"{"slots":{"kart":"Kart paint""#), "{j}");
        assert!(
            j.contains(
                r##"{"id":"k-neon","slot":"kart","name":"Neon green paint","value":"#39ff88","price":20,"level":0,"owned":false,"equipped":false,"locked":false}"##
            ),
            "{j}"
        );
        assert!(j.ends_with(r#","coins":0,"level":1}"#), "{j}");
    }

    #[test]
    fn the_flags_read_the_level_and_the_raw_worn_map() {
        let have: HashSet<String> = ["k-neon".to_string(), "k-gold".to_string()].into_iter().collect();
        let mut on = Map::new();
        on.insert("kart".into(), Value::String("k-gold".into()));
        // A stale entry: f-legend does not belong in the ball slot.
        on.insert("ball".into(), Value::String("f-legend".into()));
        let views = item_views(20, &have, &on);
        let get = |id: &str| views.iter().find(|v| v.id == id).expect("in the catalogue");

        assert!(get("k-neon").owned && !get("k-neon").equipped && !get("k-neon").locked);
        assert!(get("k-gold").owned && get("k-gold").equipped && !get("k-gold").locked);
        // Level 25 at level 20.
        assert!(get("r-gold").locked && !get("r-gold").owned);
        // Priced items are never locked.
        assert!(!get("d-fireworks").locked && !get("d-fireworks").owned);
        // The stale ball entry is not f-legend's slot, so nothing reads as worn.
        assert!(!get("f-legend").equipped && !get("b-gold").equipped);
    }

    #[test]
    fn a_level_unlock_needs_no_balance_row() {
        // The unlock set is a pure function of the level, with no balance row
        // anywhere in it, and it comes out in catalogue order.
        let at = |l: i64| level_unlocks(l).collect::<Vec<_>>();
        assert!(at(14).is_empty());
        assert_eq!(at(15), ["b-gold"]);
        assert_eq!(at(20), ["k-gold", "b-gold"]);
        assert_eq!(at(40), ["k-gold", "r-gold", "b-gold", "f-legend", "d-neon"]);
        // The five unlocks are exactly the unpriced items, and nothing else.
        assert!(at(999).iter().all(|id| find(id).expect("in the catalogue").price == 0));
        assert_eq!(at(999).len(), CATALOG.iter().filter(|it| it.price == 0 && it.level != 0).count());
        // `item_views` reads only the set it is handed: the union happens in
        // `owned()`, against the database.
        assert!(!item_views(40, &HashSet::new(), &Map::new()).iter().any(|v| v.owned));
    }

    #[test]
    fn the_cos_prefix_is_sliced_not_parsed() {
        assert_eq!(strip_cos("cos:k-neon"), "k-neon");
        // SQLite's LIKE matched this case-insensitively, so it is sliced too.
        assert_eq!(strip_cos("COS:k-neon"), "k-neon");
        assert_eq!(strip_cos("cos:"), "");
        assert_eq!(item_key("d-fireworks"), "cos:d-fireworks");
    }

    #[test]
    fn game_xp_matches_the_python_scoring() {
        assert_eq!(xp_for_result(1, 4), 50);
        assert_eq!(xp_for_result(2, 4), 35);
        assert_eq!(xp_for_result(3, 4), 28);
        assert_eq!(xp_for_result(4, 4), 20);
        // Alone: finishing only, whatever the place says.
        assert_eq!(xp_for_result(1, 1), 20);
    }

    #[test]
    fn game_xp_is_capped_per_day_not_over_the_total() {
        let day = |d: &str, n: usize| -> Vec<(String, i64, i64)> {
            (0..n).map(|_| (d.to_string(), 1, 4)).collect()
        };
        // 50 XP a win: ten wins is 500, clamped to 400 for the day.
        assert_eq!(game_xp_from_rows(&day("2026-10-08", 10)), GAME_XP_DAY_CAP);
        // Two days each earn their own cap.
        let mut two = day("2026-10-08", 10);
        two.extend(day("2026-10-07", 10));
        assert_eq!(game_xp_from_rows(&two), 2 * GAME_XP_DAY_CAP);
        // Under the cap it is just the sum.
        assert_eq!(game_xp_from_rows(&day("2026-10-08", 3)), 150);
        assert_eq!(game_xp_from_rows(&[]), 0);
    }

    #[test]
    fn buy_bodies_are_validated_like_pydantic() {
        assert_eq!(
            buy_body(br#"{"requestId":"abcdefgh","item":"k-neon"}"#).unwrap(),
            ("abcdefgh".to_string(), "k-neon".to_string())
        );
        assert_eq!(
            kinds(&buy_body(br#"{"requestId":"short","item":"k-neon"}"#).unwrap_err()),
            ["string_too_short"]
        );
        assert_eq!(
            kinds(&buy_body(br#"{"requestId":"has space","item":"k-neon"}"#).unwrap_err()),
            ["string_pattern_mismatch"]
        );
        assert_eq!(kinds(&buy_body(br#"{"item":"k-neon"}"#).unwrap_err()), ["missing"]);
        assert_eq!(
            kinds(&buy_body(br#"{"requestId":1234567890,"item":"k-neon"}"#).unwrap_err()),
            ["string_type"]
        );
        assert_eq!(
            kinds(&buy_body(br#"{"requestId":"abcdefgh","item":"k-neon","price":0}"#).unwrap_err()),
            ["extra_forbidden"]
        );
        assert_eq!(kinds(&buy_body(b"[]").unwrap_err()), ["model_attributes_type"]);
        assert_eq!(kinds(&buy_body(b"{").unwrap_err()), ["json_invalid"]);
        // item has no minimum length, so "" is valid here and is refused later
        // by the catalogue check with a 400.
        assert!(buy_body(br#"{"requestId":"abcdefgh","item":""}"#).is_ok());
        // One error per field, in declaration order.
        let both = buy_body(br#"{"requestId":"x","item":"0123456789abcdefg"}"#).unwrap_err();
        assert_eq!(kinds(&both), ["string_too_short", "string_too_long"]);
        assert_eq!(both[0].loc, ["body", "requestId"]);
        assert_eq!(both[0].msg, "String should have at least 8 characters");
        assert_eq!(both[1].msg, "String should have at most 16 characters");
    }

    #[test]
    fn equip_bodies_treat_a_missing_item_as_null() {
        assert_eq!(
            equip_body(br#"{"slot":"kart","item":"k-neon"}"#).unwrap(),
            ("kart".to_string(), Some("k-neon".to_string()))
        );
        assert_eq!(equip_body(br#"{"slot":"kart"}"#).unwrap(), ("kart".to_string(), None));
        assert_eq!(
            equip_body(br#"{"slot":"kart","item":null}"#).unwrap(),
            ("kart".to_string(), None)
        );
        // An unknown slot is accepted by validation and refused with a 400.
        assert_eq!(equip_body(br#"{"slot":"wings"}"#).unwrap(), ("wings".to_string(), None));
        assert_eq!(kinds(&equip_body(br#"{"slot":"kart","item":7}"#).unwrap_err()), ["string_type"]);
        assert_eq!(kinds(&equip_body(br#"{"item":null}"#).unwrap_err()), ["missing"]);
        assert_eq!(
            kinds(&equip_body(br#"{"slot":"0123456789abc"}"#).unwrap_err()),
            ["string_too_long"]
        );
        assert_eq!(
            kinds(&equip_body(br#"{"slot":"kart","wear":true}"#).unwrap_err()),
            ["extra_forbidden"]
        );
    }

    #[test]
    fn only_priced_items_are_for_sale() {
        for it in CATALOG {
            let saleable = find(it.id).filter(|c| c.price != 0).is_some();
            assert_eq!(saleable, it.price != 0, "{}", it.id);
        }
        // The five level unlocks can never be bought.
        assert!(find("k-gold").filter(|c| c.price != 0).is_none());
        assert!(find("r-gold").filter(|c| c.price != 0).is_none());
        assert!(find("b-gold").filter(|c| c.price != 0).is_none());
        assert!(find("f-legend").filter(|c| c.price != 0).is_none());
        assert!(find("d-neon").filter(|c| c.price != 0).is_none());
        assert!(find("nope").is_none());
    }
}

