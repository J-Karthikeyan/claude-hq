//! HQ 2.5 loot: chests from real work.
//!
//! The user's own HQ notices a merged PR, a green test run or a long focus
//! session and reports only the event TYPE, a small count and the UTC day
//! (`POST /v1/loot/events`). Each accepted unit mints one chest; a chest opens
//! once into Poke Coins, a grant-only cosmetic or a Pokemon card.
//!
//! TRUST-THE-CLIENT within caps. The server cannot see transcripts or GitHub,
//! so nothing here verifies that the work happened. What bounds it:
//!   * per-user, per-UTC-day caps on the summed units of each type
//!     ([`CAPS`]); the excess is accepted (200) but mints nothing;
//!   * `day` must be today or yesterday (UTC);
//!   * loot coins have their own cap, [`LOOT_COINS_PER_DAY`], counted from the
//!     day's opened rewards, AND never push a wallet past the pantry's
//!     `COIN_CAP`; any coin roll that would not fit becomes a card instead.
//!
//! Chest ids are derived from the server secret, the user, the requestId and
//! the unit index, so a replayed requestId finds the very chests it minted --
//! and a client cannot pick a requestId offline that rolls an epic, because it
//! does not know the secret. Rarity comes from sha256(chest id); the opening
//! roll from sha256(chest id + "open"), so opening is deterministic per chest.
//!
//! Loot coins bypass `poke_ledger` on purpose: its CHECK constraint allows no
//! new op value, and a table rebuild would break the add-only migration rule.
//! `loot_chests.reward` is the journal of record for every coin minted here,
//! and LOOT_COINS_PER_DAY replaces the pantry's DAILY_COINS and MAX_OPS_PER_DAY
//! for this path.
//!
//! Every route is self-scoped: no request names another user.

use crate::{err, AppState, Caller};
use axum::{
    extract::{Request, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use chrono::{Duration, NaiveDate, Utc};
use serde::Serialize;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use sqlx::{Row, SqliteConnection, SqlitePool};

/// The three work signals, with their per-user per-UTC-day unit caps.
const CAPS: [(&str, i64); 3] = [("pr_merged", 5), ("tests_green", 3), ("focus_long", 2)];
/// Rarity odds in percent, common / rare / epic, per source.
const ODDS: [(&str, [u32; 3]); 3] = [
    ("pr_merged", [60, 30, 10]),
    ("tests_green", [80, 18, 2]),
    ("focus_long", [70, 25, 5]),
];
/// Loot coins a user can be paid per UTC day, summed over that day's openings.
const LOOT_COINS_PER_DAY: i64 = 15;
/// The five grant-only cosmetics a chest can hold. `f-star` and `d-crown` are
/// prestige's and never drop.
const LOOT_COS: [&str; 5] = ["k-prism", "r-shadow", "g-plasma", "b-pokeball", "f-holo"];
/// A card stack stops at this many copies.
const CARD_CAP: i64 = 99;
/// At most this many units per report.
const MAX_N: i64 = 5;
/// The unopened list is bounded; the caps make this unreachable in practice.
const MAX_LISTED: i64 = 200;
const OPENED_LISTED: i64 = 20;

const REUSED: &str = "that requestId was already used for a different request";
const BUSY: &str = "busy, try again";

fn cap_of(kind: &str) -> Option<i64> {
    CAPS.iter().find(|(k, _)| *k == kind).map(|(_, c)| *c)
}

fn now_ts() -> String {
    Utc::now().format("%Y-%m-%d %H:%M:%S%.6f").to_string()
}

fn sha(parts: &[&str]) -> [u8; 32] {
    let mut h = Sha256::new();
    for (i, p) in parts.iter().enumerate() {
        if i > 0 {
            h.update(b"|");
        }
        h.update(p.as_bytes());
    }
    h.finalize().into()
}

fn u16_at(b: &[u8; 32], i: usize) -> u32 {
    (u32::from(b[i]) << 8) | u32::from(b[i + 1])
}

/// `ch-` + 32 hex. Secret-keyed, so a replay can find its chests again and a
/// client cannot grind requestIds for rarity.
fn chest_id(secret: &str, uid: &str, rid: &str, i: i64) -> String {
    let b = sha(&["loot-chest", secret, uid, rid, &i.to_string()]);
    format!("ch-{}", &hex::encode(b)[..32])
}

fn rarity_for(source: &str, id: &str) -> &'static str {
    let odds = ODDS.iter().find(|(k, _)| *k == source).map(|(_, o)| *o).unwrap_or([100, 0, 0]);
    let roll = u16_at(&sha(&[id]), 0) % 100;
    if roll < odds[0] {
        "common"
    } else if roll < odds[0] + odds[1] {
        "rare"
    } else {
        "epic"
    }
}

/// What opening a chest will try to give, before the caps have their say.
#[derive(Debug, PartialEq, Eq)]
enum Plan {
    Coins(i64),
    Cosmetic(usize),
    Card { line: usize, holo: bool },
}

/// The deterministic opening roll. Pure: the same chest always rolls the same.
fn roll(id: &str, rarity: &str, lines: usize) -> Plan {
    let b = Sha256::digest(format!("{id}open").as_bytes());
    let b: [u8; 32] = b.into();
    let pick = u16_at(&b, 0) % 100;
    let amt = i64::from(b[2] % 3);
    let line = (u16_at(&b, 4) as usize) % lines.max(1);
    match rarity {
        "epic" if pick < 30 => Plan::Coins(8 + amt),
        "epic" if pick < 65 => Plan::Cosmetic(usize::from(b[3]) % LOOT_COS.len()),
        "epic" => Plan::Card { line, holo: true },
        "rare" if pick < 40 => Plan::Coins(4 + amt),
        "rare" if pick < 65 => Plan::Cosmetic(usize::from(b[3]) % LOOT_COS.len()),
        "rare" => Plan::Card { line, holo: false },
        _ if pick < 55 => Plan::Coins(1 + amt),
        _ => Plan::Card { line, holo: false },
    }
}

/// The base dex of every evolution line, in `pokebattle` DATA order.
fn line_dexes() -> Vec<i64> {
    crate::valley::pokebattle::DATA.lines.iter().filter_map(|l| l.first().copied()).collect()
}

fn card_id(dex: i64, holo: bool) -> String {
    format!("{}{:03}", if holo { 'h' } else { 'p' }, dex)
}

// --- wire shapes ----------------------------------------------------------

#[derive(Serialize, Debug, Clone, PartialEq)]
struct Chest {
    id: String,
    source: String,
    rarity: String,
    created_at: String,
}

#[derive(Serialize, Debug)]
struct EventsOut {
    accepted: i64,
    chests: Vec<Chest>,
}

#[derive(Serialize, Debug, Clone, PartialEq)]
struct Reward {
    kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<String>,
    qty: i64,
}

#[derive(Serialize, Debug)]
struct OpenOut {
    reward: Reward,
}

#[derive(Serialize, Debug)]
struct Opened {
    id: String,
    source: String,
    rarity: String,
    reward: Option<Reward>,
    opened_at: String,
}

#[derive(Serialize, Debug)]
struct LootView {
    chests: Vec<Chest>,
    opened: Vec<Opened>,
    cards: Map<String, Value>,
}

/// A refusal: the status and the `detail` it carries.
#[derive(Debug)]
struct Fail(StatusCode, String);

impl Fail {
    fn db(e: sqlx::Error) -> Self {
        let busy = e
            .as_database_error()
            .and_then(|d| d.code())
            .and_then(|c| c.parse::<i32>().ok())
            .map(|c| matches!(c & 0xff, 5 | 6))
            .unwrap_or(false);
        if busy {
            Fail(StatusCode::SERVICE_UNAVAILABLE, BUSY.into())
        } else {
            Fail(StatusCode::INTERNAL_SERVER_ERROR, format!("db error: {e}"))
        }
    }
    fn into_response(self) -> Response {
        err(self.0, &self.1)
    }
}

fn is_unique(e: &sqlx::Error) -> bool {
    e.as_database_error()
        .map(|d| d.is_unique_violation() || d.message().contains("UNIQUE constraint failed"))
        .unwrap_or(false)
}

fn bad(msg: &str) -> Fail {
    Fail(StatusCode::UNPROCESSABLE_ENTITY, msg.into())
}

// --- request bodies (by hand: extra keys and bad fields are 422) ----------

fn body_object(bytes: &[u8]) -> Result<Map<String, Value>, Fail> {
    match serde_json::from_slice::<Value>(bytes) {
        Ok(Value::Object(o)) => Ok(o),
        _ => Err(bad("body must be a JSON object")),
    }
}

fn forbid_extra(obj: &Map<String, Value>, known: &[&str]) -> Result<(), Fail> {
    match obj.keys().find(|k| !known.contains(&k.as_str())) {
        Some(k) => Err(bad(&format!("unknown field: {k}"))),
        None => Ok(()),
    }
}

/// 16-64 chars of `^[A-Za-z0-9_-]+$`.
fn request_id(obj: &Map<String, Value>) -> Result<String, Fail> {
    let s = obj.get("requestId").and_then(Value::as_str).ok_or_else(|| bad("requestId is required"))?;
    let ok = (16..=64).contains(&s.len())
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if ok {
        Ok(s.to_string())
    } else {
        Err(bad("requestId must be 16-64 of A-Z a-z 0-9 _ -"))
    }
}

#[derive(Debug, PartialEq)]
struct EventBody {
    rid: String,
    kind: String,
    n: i64,
    day: String,
}

fn event_body(bytes: &[u8], today: NaiveDate) -> Result<EventBody, Fail> {
    let obj = body_object(bytes)?;
    forbid_extra(&obj, &["requestId", "type", "n", "day"])?;
    let rid = request_id(&obj)?;
    let kind = obj.get("type").and_then(Value::as_str).unwrap_or("");
    if cap_of(kind).is_none() {
        return Err(bad("type must be pr_merged, tests_green or focus_long"));
    }
    let n = match obj.get("n") {
        Some(Value::Number(n)) if n.is_i64() => n.as_i64().unwrap_or(0),
        _ => 0,
    };
    if !(1..=MAX_N).contains(&n) {
        return Err(bad("n must be an integer 1-5"));
    }
    let day = obj.get("day").and_then(Value::as_str).unwrap_or("");
    let parsed = NaiveDate::parse_from_str(day, "%Y-%m-%d").ok().filter(|_| day.len() == 10);
    if parsed != Some(today) && parsed != Some(today - Duration::days(1)) {
        return Err(bad("day must be today or yesterday (UTC)"));
    }
    Ok(EventBody { rid, kind: kind.to_string(), n, day: day.to_string() })
}

fn is_chest_id(s: &str) -> bool {
    s.len() == 35
        && s.starts_with("ch-")
        && s[3..].bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn open_body(bytes: &[u8]) -> Result<(String, String), Fail> {
    let obj = body_object(bytes)?;
    forbid_extra(&obj, &["chestId", "requestId"])?;
    let id = obj.get("chestId").and_then(Value::as_str).unwrap_or("");
    if !is_chest_id(id) {
        return Err(bad("chestId must look like ch-<32 hex>"));
    }
    Ok((id.to_string(), request_id(&obj)?))
}

// --- events ---------------------------------------------------------------

async fn chests_by_ids(pool: &SqlitePool, uid: &str, ids: &[String]) -> Result<Vec<Chest>, Fail> {
    let mut out = Vec::new();
    for id in ids {
        let row = sqlx::query(
            "SELECT id, source, rarity, created_at FROM loot_chests WHERE id = ?1 AND user_id = ?2",
        )
        .bind(id)
        .bind(uid)
        .fetch_optional(pool)
        .await
        .map_err(Fail::db)?;
        if let Some(r) = row {
            out.push(chest_of(&r));
        }
    }
    Ok(out)
}

fn chest_of(r: &sqlx::sqlite::SqliteRow) -> Chest {
    Chest {
        id: r.get("id"),
        source: r.get("source"),
        rarity: r.get("rarity"),
        created_at: r.try_get::<String, _>("created_at").unwrap_or_default(),
    }
}

/// The original answer for a replayed requestId, or 409 when it was spent on
/// a different report.
async fn replay(pool: &SqlitePool, secret: &str, uid: &str, b: &EventBody) -> Result<Option<EventsOut>, Fail> {
    let row = sqlx::query("SELECT type, day, n FROM work_events WHERE user_id = ?1 AND request_id = ?2")
        .bind(uid)
        .bind(&b.rid)
        .fetch_optional(pool)
        .await
        .map_err(Fail::db)?;
    let Some(r) = row else { return Ok(None) };
    let same = r.get::<String, _>("type") == b.kind
        && r.get::<String, _>("day") == b.day
        && r.get::<i64, _>("n") == b.n;
    if !same {
        return Err(Fail(StatusCode::CONFLICT, REUSED.into()));
    }
    let ids: Vec<String> = (0..b.n).map(|i| chest_id(secret, uid, &b.rid, i)).collect();
    let chests = chests_by_ids(pool, uid, &ids).await?;
    Ok(Some(EventsOut { accepted: chests.len() as i64, chests }))
}

async fn mint(tx: &mut SqliteConnection, secret: &str, uid: &str, b: &EventBody) -> Result<Vec<Chest>, sqlx::Error> {
    let at = now_ts();
    // The journal row first: it takes the write lock and claims
    // UNIQUE(user_id, request_id), so the sum below is exact under concurrency.
    sqlx::query(
        "INSERT INTO work_events (user_id, type, day, n, request_id, at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
    )
    .bind(uid)
    .bind(&b.kind)
    .bind(&b.day)
    .bind(b.n)
    .bind(&b.rid)
    .bind(&at)
    .execute(&mut *tx)
    .await?;
    let total: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(n), 0) FROM work_events WHERE user_id = ?1 AND type = ?2 AND day = ?3",
    )
    .bind(uid)
    .bind(&b.kind)
    .bind(&b.day)
    .fetch_one(&mut *tx)
    .await?;
    let cap = cap_of(&b.kind).unwrap_or(0);
    let before = total - b.n;
    let units = (cap - before).clamp(0, b.n);
    let mut out = Vec::new();
    for i in 0..units {
        let id = chest_id(secret, uid, &b.rid, i);
        let rarity = rarity_for(&b.kind, &id);
        sqlx::query(
            "INSERT INTO loot_chests (id, user_id, source, rarity, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
        )
        .bind(&id)
        .bind(uid)
        .bind(&b.kind)
        .bind(rarity)
        .bind(&at)
        .execute(&mut *tx)
        .await?;
        out.push(Chest { id, source: b.kind.clone(), rarity: rarity.into(), created_at: at.clone() });
    }
    Ok(out)
}

async fn record_events(pool: &SqlitePool, secret: &str, uid: &str, b: &EventBody) -> Result<EventsOut, Fail> {
    if let Some(out) = replay(pool, secret, uid, b).await? {
        return Ok(out);
    }
    let mut tx = pool.begin().await.map_err(Fail::db)?;
    let res = mint(&mut tx, secret, uid, b).await;
    let res = match res {
        Ok(chests) => tx.commit().await.map(|_| chests),
        Err(e) => {
            let _ = tx.rollback().await;
            Err(e)
        }
    };
    match res {
        Ok(chests) => Ok(EventsOut { accepted: chests.len() as i64, chests }),
        // A concurrent submit of the same requestId won the race: replay it.
        Err(e) if is_unique(&e) => replay(pool, secret, uid, b)
            .await?
            .ok_or_else(|| Fail(StatusCode::SERVICE_UNAVAILABLE, BUSY.into())),
        Err(e) => Err(Fail::db(e)),
    }
}

// --- opening --------------------------------------------------------------

/// Give one copy of a card, or `false` when the stack is already full.
async fn credit_card(tx: &mut SqliteConnection, uid: &str, key: &str) -> Result<bool, sqlx::Error> {
    let up = sqlx::query(
        "UPDATE poke_balances SET qty = qty + 1, updated_at = CURRENT_TIMESTAMP
         WHERE user_id = ?1 AND item = ?2 AND qty + 1 <= ?3",
    )
    .bind(uid)
    .bind(key)
    .bind(CARD_CAP)
    .execute(&mut *tx)
    .await?;
    if up.rows_affected() == 1 {
        return Ok(true);
    }
    let ins = sqlx::query(
        "INSERT INTO poke_balances (id, user_id, item, qty)
         SELECT ?1, ?2, ?3, 1 WHERE NOT EXISTS
           (SELECT 1 FROM poke_balances WHERE user_id = ?2 AND item = ?3)",
    )
    .bind(uuid::Uuid::new_v4().to_string())
    .bind(uid)
    .bind(key)
    .execute(&mut *tx)
    .await?;
    Ok(ins.rows_affected() == 1)
}

/// Grant a cosmetic unless it is already owned (`false`).
async fn credit_cosmetic(tx: &mut SqliteConnection, uid: &str, cid: &str) -> Result<bool, sqlx::Error> {
    let key = format!("cos:{cid}");
    // A row left at qty 0 (e.g. traded away) is revived rather than duplicated:
    // UNIQUE(user_id, item) would refuse a second row.
    let up = sqlx::query(
        "UPDATE poke_balances SET qty = 1, updated_at = CURRENT_TIMESTAMP
         WHERE user_id = ?1 AND item = ?2 AND qty = 0",
    )
    .bind(uid)
    .bind(&key)
    .execute(&mut *tx)
    .await?;
    if up.rows_affected() == 1 {
        return Ok(true);
    }
    let ins = sqlx::query(
        "INSERT INTO poke_balances (id, user_id, item, qty)
         SELECT ?1, ?2, ?3, 1 WHERE NOT EXISTS
           (SELECT 1 FROM poke_balances WHERE user_id = ?2 AND item = ?3)",
    )
    .bind(uuid::Uuid::new_v4().to_string())
    .bind(uid)
    .bind(&key)
    .execute(&mut *tx)
    .await?;
    Ok(ins.rows_affected() == 1)
}

/// Pay coins when they fit under BOTH the loot day cap and the wallet cap.
async fn credit_coins(tx: &mut SqliteConnection, uid: &str, amt: i64, today: &str) -> Result<bool, sqlx::Error> {
    let paid_today: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(json_extract(reward, '$.qty')), 0) FROM loot_chests
         WHERE user_id = ?1 AND substr(opened_at, 1, 10) = ?2
           AND json_extract(reward, '$.kind') = 'coins'",
    )
    .bind(uid)
    .bind(today)
    .fetch_one(&mut *tx)
    .await?;
    if paid_today + amt > LOOT_COINS_PER_DAY {
        return Ok(false);
    }
    let cap = crate::pantry::COIN_CAP;
    let up = sqlx::query(
        "UPDATE poke_balances SET qty = qty + ?1, updated_at = CURRENT_TIMESTAMP
         WHERE user_id = ?2 AND item = 'coins' AND qty + ?1 <= ?3",
    )
    .bind(amt)
    .bind(uid)
    .bind(cap)
    .execute(&mut *tx)
    .await?;
    if up.rows_affected() == 1 {
        return Ok(true);
    }
    if amt > cap {
        return Ok(false);
    }
    let ins = sqlx::query(
        "INSERT INTO poke_balances (id, user_id, item, qty)
         SELECT ?1, ?2, 'coins', ?3 WHERE NOT EXISTS
           (SELECT 1 FROM poke_balances WHERE user_id = ?2 AND item = 'coins')",
    )
    .bind(uuid::Uuid::new_v4().to_string())
    .bind(uid)
    .bind(amt)
    .execute(&mut *tx)
    .await?;
    Ok(ins.rows_affected() == 1)
}

/// A card from `line` onward: the first stack that is not full.
async fn give_card(tx: &mut SqliteConnection, uid: &str, line: usize, holo: bool) -> Result<Reward, sqlx::Error> {
    let dexes = line_dexes();
    for k in 0..dexes.len() {
        let id = card_id(dexes[(line + k) % dexes.len()], holo);
        if credit_card(tx, uid, &format!("card:{id}")).await? {
            return Ok(Reward { kind: "card".into(), id: Some(id), qty: 1 });
        }
    }
    // Every one of the 48 stacks at 99: the chest is spent and gives nothing.
    Ok(Reward { kind: "card".into(), id: None, qty: 0 })
}

async fn apply(tx: &mut SqliteConnection, uid: &str, id: &str, rarity: &str, today: &str) -> Result<Reward, sqlx::Error> {
    let lines = line_dexes().len();
    let plan = roll(id, rarity, lines);
    let fallback_line = (u16_at(&sha(&[id, "card"]), 0) as usize) % lines.max(1);
    let holo = rarity == "epic";
    match plan {
        Plan::Coins(n) => {
            if credit_coins(tx, uid, n, today).await? {
                return Ok(Reward { kind: "coins".into(), id: None, qty: n });
            }
            // Over a cap: the overflow becomes a card.
            give_card(tx, uid, fallback_line, holo).await
        }
        Plan::Cosmetic(i) => {
            let cid = LOOT_COS[i];
            if credit_cosmetic(tx, uid, cid).await? {
                return Ok(Reward { kind: "cosmetic".into(), id: Some(cid.into()), qty: 1 });
            }
            // Already owned: a holo card instead.
            give_card(tx, uid, fallback_line, true).await
        }
        Plan::Card { line, holo } => give_card(tx, uid, line, holo).await,
    }
}

fn reward_of(raw: Option<String>) -> Option<(Reward, Option<String>)> {
    let v: Value = serde_json::from_str(&raw?).ok()?;
    let kind = v.get("kind")?.as_str()?.to_string();
    let id = v.get("id").and_then(Value::as_str).map(String::from);
    let qty = v.get("qty").and_then(Value::as_i64).unwrap_or(0);
    let rid = v.get("rid").and_then(Value::as_str).map(String::from);
    Some((Reward { kind, id, qty }, rid))
}

enum Opening {
    Gave(Reward),
    NotFound,
    Already(Option<String>, Option<Reward>),
}

async fn open_tx(tx: &mut SqliteConnection, uid: &str, id: &str, rid: &str, today: &str) -> Result<Opening, sqlx::Error> {
    let now = now_ts();
    // The claim is the first write: zero rows means it was opened already (or
    // is not yours), and two racing opens cannot both get here.
    let row = sqlx::query(
        "UPDATE loot_chests SET opened_at = ?1 WHERE id = ?2 AND user_id = ?3 AND opened_at IS NULL
         RETURNING rarity",
    )
    .bind(&now)
    .bind(id)
    .bind(uid)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(row) = row else {
        let prev = sqlx::query("SELECT reward FROM loot_chests WHERE id = ?1 AND user_id = ?2")
            .bind(id)
            .bind(uid)
            .fetch_optional(&mut *tx)
            .await?;
        return Ok(match prev {
            None => Opening::NotFound,
            Some(p) => {
                let got = reward_of(p.try_get::<Option<String>, _>("reward").ok().flatten());
                match got {
                    Some((r, prid)) => Opening::Already(prid, Some(r)),
                    None => Opening::Already(None, None),
                }
            }
        });
    };
    let rarity: String = row.get("rarity");
    let reward = apply(tx, uid, id, &rarity, today).await?;
    let mut stored = json!({ "kind": reward.kind, "qty": reward.qty, "rid": rid });
    if let Some(i) = &reward.id {
        stored["id"] = json!(i);
    }
    sqlx::query("UPDATE loot_chests SET reward = ?1 WHERE id = ?2")
        .bind(stored.to_string())
        .bind(id)
        .execute(&mut *tx)
        .await?;
    Ok(Opening::Gave(reward))
}

async fn open_chest(pool: &SqlitePool, uid: &str, id: &str, rid: &str, today: &str) -> Result<Reward, Fail> {
    let mut tx = pool.begin().await.map_err(Fail::db)?;
    let res = open_tx(&mut tx, uid, id, rid, today).await;
    match res {
        Ok(Opening::Gave(r)) => {
            tx.commit().await.map_err(Fail::db)?;
            Ok(r)
        }
        Ok(Opening::NotFound) => {
            let _ = tx.rollback().await;
            Err(Fail(StatusCode::NOT_FOUND, "no such chest".into()))
        }
        Ok(Opening::Already(prid, prev)) => {
            let _ = tx.rollback().await;
            // The same requestId again is a replay of the opening it made.
            match (prid, prev) {
                (Some(p), Some(r)) if p == rid => Ok(r),
                _ => Err(Fail(StatusCode::CONFLICT, "already opened".into())),
            }
        }
        Err(e) => {
            let _ = tx.rollback().await;
            Err(Fail::db(e))
        }
    }
}

// --- reads ----------------------------------------------------------------

async fn loot_view(pool: &SqlitePool, uid: &str) -> Result<LootView, Fail> {
    let rows = sqlx::query(
        "SELECT id, source, rarity, created_at FROM loot_chests
         WHERE user_id = ?1 AND opened_at IS NULL ORDER BY created_at, rowid LIMIT ?2",
    )
    .bind(uid)
    .bind(MAX_LISTED)
    .fetch_all(pool)
    .await
    .map_err(Fail::db)?;
    let chests = rows.iter().map(chest_of).collect();
    let rows = sqlx::query(
        "SELECT id, source, rarity, opened_at, reward FROM loot_chests
         WHERE user_id = ?1 AND opened_at IS NOT NULL ORDER BY opened_at DESC, id LIMIT ?2",
    )
    .bind(uid)
    .bind(OPENED_LISTED)
    .fetch_all(pool)
    .await
    .map_err(Fail::db)?;
    let opened = rows
        .iter()
        .map(|r| Opened {
            id: r.get("id"),
            source: r.get("source"),
            rarity: r.get("rarity"),
            reward: reward_of(r.try_get::<Option<String>, _>("reward").ok().flatten()).map(|(rw, _)| rw),
            opened_at: r.try_get::<String, _>("opened_at").unwrap_or_default(),
        })
        .collect();
    let rows = sqlx::query(
        "SELECT item, qty FROM poke_balances WHERE user_id = ?1 AND item LIKE 'card:%' AND qty > 0
         ORDER BY item",
    )
    .bind(uid)
    .fetch_all(pool)
    .await
    .map_err(Fail::db)?;
    let mut cards = Map::new();
    for r in rows {
        let item: String = r.get("item");
        cards.insert(item.chars().skip(5).collect(), json!(r.get::<i64, _>("qty")));
    }
    Ok(LootView { chests, opened, cards })
}

// --- handlers -------------------------------------------------------------

async fn read_body(req: Request) -> Result<(Caller, axum::body::Bytes), Fail> {
    let c = req.extensions().get::<Caller>().cloned();
    let Some(c) = c else { return Err(Fail(StatusCode::UNAUTHORIZED, "missing caller".into())) };
    let (_, body) = req.into_parts();
    match axum::body::to_bytes(body, 16 * 1024).await {
        Ok(b) => Ok((c, b)),
        Err(_) => Err(Fail(StatusCode::PAYLOAD_TOO_LARGE, "body too large".into())),
    }
}

async fn get_loot(State(st): State<AppState>, req: Request) -> Response {
    let Some(c) = req.extensions().get::<Caller>().cloned() else {
        return err(StatusCode::UNAUTHORIZED, "missing caller");
    };
    match loot_view(&st.pool, &c.user_id).await {
        Ok(v) => Json(v).into_response(),
        Err(f) => f.into_response(),
    }
}

async fn post_events(State(st): State<AppState>, req: Request) -> Response {
    let (c, bytes) = match read_body(req).await {
        Ok(v) => v,
        Err(f) => return f.into_response(),
    };
    let body = match event_body(&bytes, Utc::now().date_naive()) {
        Ok(b) => b,
        Err(f) => return f.into_response(),
    };
    match record_events(&st.pool, &st.cfg.secret_key, &c.user_id, &body).await {
        Ok(out) => Json(out).into_response(),
        Err(f) => f.into_response(),
    }
}

async fn post_open(State(st): State<AppState>, req: Request) -> Response {
    let (c, bytes) = match read_body(req).await {
        Ok(v) => v,
        Err(f) => return f.into_response(),
    };
    let (id, rid) = match open_body(&bytes) {
        Ok(v) => v,
        Err(f) => return f.into_response(),
    };
    let today = Utc::now().date_naive().to_string();
    match open_chest(&st.pool, &c.user_id, &id, &rid, &today).await {
        Ok(reward) => Json(OpenOut { reward }).into_response(),
        Err(f) => f.into_response(),
    }
}

/// All three routes together, so an Arena either has loot or answers a bare
/// 404 the client reads as "too old". main.rs applies `require_device`.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/loot", get(get_loot))
        .route("/v1/loot/events", post(post_events))
        .route("/v1/loot/open", post(post_open))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "test-secret";

    /// A FILE-backed temp database (WAL, several connections), so the race
    /// tests below really run on separate connections.
    async fn pool() -> (SqlitePool, std::path::PathBuf) {
        let path = std::env::temp_dir().join(format!("loot-test-{}.db", uuid::Uuid::new_v4().simple()));
        let pool = crate::db::connect(&format!("sqlite:{}", path.display())).await.unwrap();
        crate::db::migrate(&pool).await.unwrap();
        for (gh, id, h) in [(1, "u1", "ann"), (2, "u2", "bob")] {
            sqlx::query(
                "INSERT INTO users (id, github_id, handle, display_name, avatar_url, trainer_name,
                 is_active, created_at) VALUES (?1, ?3, ?2, ?2, '', '', 1, datetime('now'))",
            )
            .bind(id)
            .bind(h)
            .bind(gh)
            .execute(&pool)
            .await
            .unwrap();
        }
        (pool, path)
    }

    fn cleanup(path: std::path::PathBuf) {
        for ext in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{ext}", path.display()));
        }
    }

    fn today() -> String {
        Utc::now().date_naive().to_string()
    }

    fn ev(rid: &str, kind: &str, n: i64) -> EventBody {
        EventBody { rid: rid.into(), kind: kind.into(), n, day: today() }
    }

    async fn coins(pool: &SqlitePool, uid: &str) -> i64 {
        sqlx::query_scalar("SELECT qty FROM poke_balances WHERE user_id = ?1 AND item = 'coins'")
            .bind(uid)
            .fetch_optional(pool)
            .await
            .unwrap()
            .unwrap_or(0)
    }

    async fn set_coins(pool: &SqlitePool, uid: &str, n: i64) {
        sqlx::query(
            "INSERT INTO poke_balances (id, user_id, item, qty) VALUES (?1, ?2, 'coins', ?3)
             ON CONFLICT(user_id, item) DO UPDATE SET qty = excluded.qty",
        )
        .bind(uuid::Uuid::new_v4().to_string())
        .bind(uid)
        .bind(n)
        .execute(pool)
        .await
        .unwrap();
    }

    /// Chests of a given rarity, minted directly (as the events route would).
    async fn seed_chest(pool: &SqlitePool, uid: &str, id: &str, rarity: &str) {
        sqlx::query("INSERT INTO loot_chests (id, user_id, source, rarity) VALUES (?1, ?2, 'pr_merged', ?3)")
            .bind(id)
            .bind(uid)
            .bind(rarity)
            .execute(pool)
            .await
            .unwrap();
    }

    /// A chest id whose roll is the wanted kind for that rarity.
    fn find_id(rarity: &str, want: fn(&Plan) -> bool, skip: usize) -> String {
        let mut seen = 0;
        for i in 0..100_000 {
            let id = format!("ch-{}", &hex::encode(sha(&["find", &i.to_string()]))[..32]);
            if want(&roll(&id, rarity, 48)) {
                if seen == skip {
                    return id;
                }
                seen += 1;
            }
        }
        panic!("no such id");
    }

    #[test]
    fn bodies_are_parsed_strictly() {
        let t = Utc::now().date_naive();
        let y = (t - Duration::days(1)).to_string();
        let ok = format!(r#"{{"requestId":"loot-0123456789abcdef","type":"pr_merged","n":2,"day":"{t}"}}"#);
        assert_eq!(event_body(ok.as_bytes(), t).unwrap().n, 2);
        let yday = format!(r#"{{"requestId":"loot-0123456789abcdef","type":"focus_long","n":1,"day":"{y}"}}"#);
        assert!(event_body(yday.as_bytes(), t).is_ok());
        let old = (t - Duration::days(2)).to_string();
        let tomorrow = (t + Duration::days(1)).to_string();
        for bad_body in [
            format!(r#"{{"requestId":"loot-0123456789abcdef","type":"pr_merged","n":1,"day":"{old}"}}"#),
            format!(r#"{{"requestId":"loot-0123456789abcdef","type":"pr_merged","n":1,"day":"{tomorrow}"}}"#),
            format!(r#"{{"requestId":"loot-0123456789abcdef","type":"pr_merged","n":6,"day":"{t}"}}"#),
            format!(r#"{{"requestId":"loot-0123456789abcdef","type":"pr_merged","n":0,"day":"{t}"}}"#),
            format!(r#"{{"requestId":"loot-0123456789abcdef","type":"pr_merged","n":1.5,"day":"{t}"}}"#),
            format!(r#"{{"requestId":"loot-0123456789abcdef","type":"commits","n":1,"day":"{t}"}}"#),
            format!(r#"{{"requestId":"short","type":"pr_merged","n":1,"day":"{t}"}}"#),
            format!(r#"{{"requestId":"has space in it 0123","type":"pr_merged","n":1,"day":"{t}"}}"#),
            format!(r#"{{"requestId":"loot-0123456789abcdef","type":"pr_merged","n":1,"day":"{t}","title":"x"}}"#),
            "[]".to_string(),
        ] {
            let e = event_body(bad_body.as_bytes(), t).unwrap_err();
            assert_eq!(e.0, StatusCode::UNPROCESSABLE_ENTITY, "{bad_body}");
        }
        let id = format!("ch-{}", "a".repeat(32));
        assert!(open_body(format!(r#"{{"chestId":"{id}","requestId":"open-0123456789abcd"}}"#).as_bytes()).is_ok());
        assert!(open_body(br#"{"chestId":"ch-xyz","requestId":"open-0123456789abcd"}"#).is_err());
        assert!(open_body(format!(r#"{{"chestId":"{id}","requestId":"open-0123456789abcd","qty":9}}"#).as_bytes()).is_err());
    }

    #[test]
    fn rolls_are_deterministic_and_in_range() {
        let mut seen = [0usize; 3];
        for i in 0..3000 {
            let id = chest_id(SECRET, "u1", "rid-0123456789abcd", i);
            assert!(is_chest_id(&id), "{id}");
            assert_eq!(rarity_for("pr_merged", &id), rarity_for("pr_merged", &id));
            let r = rarity_for("pr_merged", &id);
            seen[["common", "rare", "epic"].iter().position(|x| *x == r).unwrap()] += 1;
            for rar in ["common", "rare", "epic"] {
                let p = roll(&id, rar, 48);
                assert_eq!(p, roll(&id, rar, 48));
                match p {
                    Plan::Coins(n) => assert!(match rar {
                        "common" => (1..=3).contains(&n),
                        "rare" => (4..=6).contains(&n),
                        _ => (8..=10).contains(&n),
                    }),
                    Plan::Cosmetic(i) => assert!(rar != "common" && i < LOOT_COS.len()),
                    Plan::Card { line, holo } => {
                        assert!(line < 48);
                        assert_eq!(holo, rar == "epic");
                    }
                }
            }
        }
        // 60/30/10 within a few points over 3000 draws.
        assert!((1650..1950).contains(&seen[0]), "{seen:?}");
        assert!((750..1050).contains(&seen[1]), "{seen:?}");
        assert!((200..400).contains(&seen[2]), "{seen:?}");
        // The secret keys the id: without it a client cannot predict rarity.
        assert_ne!(chest_id("a", "u1", "r", 0), chest_id("b", "u1", "r", 0));
    }

    #[test]
    fn cards_name_the_base_of_a_line() {
        let d = line_dexes();
        assert_eq!(d.len(), 48);
        assert!(d.contains(&25) || d.contains(&172), "Pichu or Pikachu heads a line");
        assert_eq!(card_id(4, false), "p004");
        assert_eq!(card_id(633, true), "h633");
        // poke_balances.item is VARCHAR(16).
        assert!(format!("card:{}", card_id(999, true)).len() <= 16);
        for c in LOOT_COS {
            assert!(format!("cos:{c}").len() <= 16);
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn the_same_request_id_mints_once() {
        let (pool, path) = pool().await;
        let b = ev("loot-aaaaaaaaaaaaaaaa", "pr_merged", 2);
        let first = record_events(&pool, SECRET, "u1", &b).await.unwrap();
        assert_eq!(first.accepted, 2);
        assert_eq!(first.chests.len(), 2);
        let again = record_events(&pool, SECRET, "u1", &b).await.unwrap();
        assert_eq!(again.accepted, 2);
        assert_eq!(again.chests, first.chests);
        // Concurrent replays of a fresh id: still one set of chests.
        let c = ev("loot-bbbbbbbbbbbbbbbb", "tests_green", 1);
        let (x, y) = tokio::join!(
            record_events(&pool, SECRET, "u1", &c),
            record_events(&pool, SECRET, "u1", &c)
        );
        assert_eq!(x.unwrap().chests, y.unwrap().chests);
        let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM loot_chests WHERE user_id = 'u1'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(n, 3);
        // The id spent on something else is refused.
        let other = ev("loot-aaaaaaaaaaaaaaaa", "pr_merged", 3);
        assert_eq!(record_events(&pool, SECRET, "u1", &other).await.unwrap_err().0, StatusCode::CONFLICT);
        cleanup(path);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn caps_are_enforced_per_type_and_day() {
        let (pool, path) = pool().await;
        let a = record_events(&pool, SECRET, "u1", &ev("loot-cap-000000000001", "pr_merged", 4)).await.unwrap();
        assert_eq!(a.accepted, 4);
        let b = record_events(&pool, SECRET, "u1", &ev("loot-cap-000000000002", "pr_merged", 3)).await.unwrap();
        assert_eq!(b.accepted, 1, "only 1 of the 5/day left");
        let c = record_events(&pool, SECRET, "u1", &ev("loot-cap-000000000003", "pr_merged", 1)).await.unwrap();
        assert_eq!((c.accepted, c.chests.len()), (0, 0), "accepted but mints nothing");
        let t = record_events(&pool, SECRET, "u1", &ev("loot-cap-000000000004", "tests_green", 5)).await.unwrap();
        assert_eq!(t.accepted, 3);
        let f = record_events(&pool, SECRET, "u1", &ev("loot-cap-000000000005", "focus_long", 5)).await.unwrap();
        assert_eq!(f.accepted, 2);
        // Yesterday has its own budget; another user has theirs.
        let mut y = ev("loot-cap-000000000006", "pr_merged", 5);
        y.day = (Utc::now().date_naive() - Duration::days(1)).to_string();
        assert_eq!(record_events(&pool, SECRET, "u1", &y).await.unwrap().accepted, 5);
        assert_eq!(record_events(&pool, SECRET, "u2", &ev("loot-cap-000000000001", "pr_merged", 5)).await.unwrap().accepted, 5);
        // Racing reports with different ids cannot together beat the cap.
        let (e7, e8) = (ev("loot-cap-000000000007", "focus_long", 2), ev("loot-cap-000000000008", "focus_long", 2));
        let (p, q) = tokio::join!(
            record_events(&pool, SECRET, "u2", &e7),
            record_events(&pool, SECRET, "u2", &e8)
        );
        assert_eq!(p.unwrap().accepted + q.unwrap().accepted, 2);
        cleanup(path);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn two_concurrent_opens_give_one_reward() {
        let (pool, path) = pool().await;
        let out = record_events(&pool, SECRET, "u1", &ev("loot-race-0000000001", "pr_merged", 1)).await.unwrap();
        let id = out.chests[0].id.clone();
        let d = today();
        let (a, b) = tokio::join!(
            open_chest(&pool, "u1", &id, "open-race-00000000a", &d),
            open_chest(&pool, "u1", &id, "open-race-00000000b", &d)
        );
        let oks = [a.is_ok(), b.is_ok()];
        assert_eq!(oks.iter().filter(|x| **x).count(), 1, "{a:?} {b:?}");
        let fail = if a.is_ok() { b.unwrap_err() } else { a.unwrap_err() };
        assert_eq!(fail.0, StatusCode::CONFLICT);
        assert_eq!(fail.1, "already opened");
        // The winner's id replays its own reward; another user's open is a 404.
        let rid = if oks[0] { "open-race-00000000a" } else { "open-race-00000000b" };
        assert!(open_chest(&pool, "u1", &id, rid, &d).await.is_ok());
        assert_eq!(open_chest(&pool, "u2", &id, rid, &d).await.unwrap_err().0, StatusCode::NOT_FOUND);
        let v = loot_view(&pool, "u1").await.unwrap();
        assert!(v.chests.is_empty());
        assert_eq!(v.opened.len(), 1);
        assert!(v.opened[0].reward.is_some());
        cleanup(path);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn loot_coins_respect_both_caps() {
        let (pool, path) = pool().await;
        let d = today();
        let is_coins = |p: &Plan| matches!(p, Plan::Coins(_));
        // Ten epic coin chests roll 8-10 each: the day stops at 15 coins.
        let mut paid = 0;
        for k in 0..10 {
            let id = find_id("epic", is_coins, k);
            seed_chest(&pool, "u1", &id, "epic").await;
            let r = open_chest(&pool, "u1", &id, &format!("open-coins-{k:08}"), &d).await.unwrap();
            if r.kind == "coins" {
                paid += r.qty;
            } else {
                assert_eq!(r.kind, "card", "overflow becomes a card");
                assert!(r.id.as_deref().unwrap_or("").starts_with('h'));
            }
        }
        assert!((8..=LOOT_COINS_PER_DAY).contains(&paid), "{paid}");
        assert_eq!(coins(&pool, "u1").await, paid);
        // A near-full wallet: coins never push it past COIN_CAP.
        set_coins(&pool, "u2", crate::pantry::COIN_CAP - 2).await;
        for k in 0..6 {
            let id = find_id("rare", is_coins, 20 + k);
            seed_chest(&pool, "u2", &id, "rare").await;
            let r = open_chest(&pool, "u2", &id, &format!("open-wallet-{k:07}"), &d).await.unwrap();
            assert_eq!(r.kind, "card", "4-6 coins never fit in 2: {r:?}");
        }
        assert_eq!(coins(&pool, "u2").await, crate::pantry::COIN_CAP - 2);
        cleanup(path);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_cosmetic_is_granted_once_then_turns_holo() {
        let (pool, path) = pool().await;
        let d = today();
        let is_cos0 = |p: &Plan| matches!(p, Plan::Cosmetic(0));
        let a = find_id("rare", is_cos0, 0);
        let b = find_id("rare", is_cos0, 1);
        seed_chest(&pool, "u1", &a, "rare").await;
        seed_chest(&pool, "u1", &b, "rare").await;
        let r = open_chest(&pool, "u1", &a, "open-cosmetic-0001", &d).await.unwrap();
        assert_eq!(r, Reward { kind: "cosmetic".into(), id: Some(LOOT_COS[0].into()), qty: 1 });
        // Owned exactly the way cosmetics.rs reads ownership: a cos: row, qty > 0.
        let q: i64 = sqlx::query_scalar(
            "SELECT qty FROM poke_balances WHERE user_id = 'u1' AND item LIKE 'cos:%' AND qty > 0",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(q, 1);
        let r2 = open_chest(&pool, "u1", &b, "open-cosmetic-0002", &d).await.unwrap();
        assert_eq!(r2.kind, "card");
        assert!(r2.id.unwrap().starts_with('h'), "already owned -> a holo card");
        let v = loot_view(&pool, "u1").await.unwrap();
        assert_eq!(v.cards.values().map(|q| q.as_i64().unwrap()).sum::<i64>(), 1);
        cleanup(path);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn card_stacks_stop_at_99() {
        let (pool, path) = pool().await;
        let d = today();
        let is_card = |p: &Plan| matches!(p, Plan::Card { .. });
        let id = find_id("common", is_card, 0);
        let Plan::Card { line, .. } = roll(&id, "common", 48) else { unreachable!() };
        let full = format!("card:{}", card_id(line_dexes()[line], false));
        sqlx::query("INSERT INTO poke_balances (id, user_id, item, qty) VALUES ('x', 'u1', ?1, 99)")
            .bind(&full)
            .execute(&pool)
            .await
            .unwrap();
        seed_chest(&pool, "u1", &id, "common").await;
        let r = open_chest(&pool, "u1", &id, "open-cards-0000001", &d).await.unwrap();
        assert_eq!(r.kind, "card");
        assert_ne!(format!("card:{}", r.id.unwrap()), full, "the full stack is skipped");
        let q: i64 = sqlx::query_scalar("SELECT qty FROM poke_balances WHERE item = ?1")
            .bind(&full)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(q, 99);
        cleanup(path);
    }

    #[test]
    fn the_wire_shapes_keep_their_order() {
        let r = Reward { kind: "coins".into(), id: None, qty: 3 };
        assert_eq!(serde_json::to_string(&OpenOut { reward: r }).unwrap(), r#"{"reward":{"kind":"coins","qty":3}}"#);
        let c = Chest { id: "ch-1".into(), source: "pr_merged".into(), rarity: "rare".into(), created_at: "t".into() };
        assert_eq!(
            serde_json::to_string(&c).unwrap(),
            r#"{"id":"ch-1","source":"pr_merged","rarity":"rare","created_at":"t"}"#
        );
        // `rid` lives in the stored journal only, never on the wire.
        let (rw, rid) = reward_of(Some(r#"{"kind":"card","qty":1,"rid":"x","id":"p004"}"#.into())).unwrap();
        assert_eq!(rid.as_deref(), Some("x"));
        assert!(!serde_json::to_string(&rw).unwrap().contains("rid"));
    }
}
