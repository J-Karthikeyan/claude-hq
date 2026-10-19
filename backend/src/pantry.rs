//! Poke Coins, the pantry and gifts, ported from `app/pantry.py` and
//! `app/routes/pantry.py`.
//!
//! Coins and food are cosmetic: nothing here touches XP, scoring, daily_stats
//! or the board. The store's stock follows the UTC season and one in-stock food
//! a day is the special, a coin off; both are pure functions of the date
//! (`season_of`, `special_of`), so they need no storage and every server and
//! client agrees all day. Every write is one transaction in a fixed order:
//!
//! 1. the journal INSERT into `poke_ledger`. It is the FIRST write, so SQLite's
//!    deferred `BEGIN` takes the database write lock here, and the row claims
//!    the request id under UNIQUE(user_id, request_id);
//! 2. the daily cap counts, which are exact because they run under that lock and
//!    include the row just inserted;
//! 3. conditional debits (`WHERE qty >= n`);
//! 4. conditional credits (`WHERE qty + n <= cap`);
//! 5. commit.
//!
//! There are no savepoints. Any refused guard rolls the whole thing back, the
//! journal row included, so a retry with the same request id re-executes once
//! the caller has topped up; a request id that already committed replays its
//! result instead of spending twice. CHECK(qty >= 0) is only the backstop --
//! the caps live in those WHERE clauses, so they can change without a rebuild.
//!
//! Nothing in this module carries a session id, title, path, URL, command,
//! project name or prompt text, and every request body is `extra="forbid"`, so
//! a stray one is a 422 rather than a quiet accept.

use axum::{
    extract::{Request, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use chrono::{
    DateTime, Datelike, Duration, NaiveDate, NaiveDateTime, NaiveTime, SecondsFormat, Utc,
};
use serde::ser::SerializeMap;
use serde::{Serialize, Serializer};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use sqlx::sqlite::SqliteRow;
use sqlx::{Row, SqliteConnection, SqlitePool};
use std::collections::HashMap;

// --- caps -----------------------------------------------------------------
//
// The comparison operators differ by site and are copied literally from the
// Python. Post-insert sites are strictly-greater (the row just inserted is
// counted); QUEST_REWARDS_PER_DAY is pre-insert and greater-or-equal. In both
// styles the Nth+1 attempt is the one refused.

const DAILY_COINS: i64 = 5;
pub(crate) const COIN_CAP: i64 = 30;
const ITEM_CAP: i64 = 10;
const BUY_MAX_QTY: i64 = 5;
const GIFT_MAX_COINS: i64 = 5;
const GIFT_MAX_QTY: i64 = 3;
const GIFTS_PER_DAY: i64 = 8;
const GIFTS_PER_PAIR_PER_DAY: i64 = 3;
const RECEIVE_COINS_PER_DAY: i64 = 15;
const RECEIVE_ITEMS_PER_DAY: i64 = 10;
/// Counted with no `op` filter, so claim, quest and the cosmetics feature's
/// `cosmetic`/`sell` rows all spend this budget. Enforced only inside `spend`.
const MAX_OPS_PER_DAY: i64 = 200;
const QUEST_REWARDS_PER_DAY: i64 = 10;
const STARTER: (&str, i64) = ("riceball", 1);
const RECENT_GIFTS: i64 = 5;
const RECENT_GIFT_DAYS: i64 = 7;
const DRAIN_LIMIT: i64 = 50;
const SPECIAL_MIN_PRICE: i64 = 2;
const SPECIAL_DISCOUNT: i64 = 1;
const NOTE_MAX: usize = 80;
const QUEST_ID_MAX: usize = 40;
const REWARD_ID_MAX: usize = 100;
const REWARD_COINS_MAX: i64 = 15;

/// Both of these are exact and shared by several endpoints.
const REUSED: &str = "that requestId was already used for a different request";
const BUSY: &str = "busy, try again";

// --- the catalog ----------------------------------------------------------

/// One catalog entry.
///
/// The 21-row order below is load-bearing three times over: it drives `items`'
/// key order, the `catalog` array's order, and `special_of`'s pool index. The
/// client's tests/test_catalog_sync.py AST-parses the Python literal this
/// mirrors, and arena.py FOOD_KINDS / dashboard.py FOOD_EFFECTS mirror it too,
/// so a name, plural, price, restoreMins or season changed here and not there
/// diverges silently.
///
/// `season` is when the *store* stocks it ("all" = year-round); food you
/// already hold can be eaten or gifted in any season. A revive item wakes a
/// fainted creature and then takes restoreMins off on top.
struct Food {
    kind: &'static str,
    name: &'static str,
    plural: &'static str,
    emoji: &'static str,
    price: i64,
    restore_mins: i64,
    revives: bool,
    season: &'static str,
}

/// A `static`, not a `const`: `CATALOG.iter()` and `&CATALOG[i]` have to hand
/// out `&'static Food`, which a const (inlined as a fresh temporary at each use)
/// cannot promise.
static CATALOG: [Food; 21] = [
    Food { kind: "berry",       name: "Berry",              plural: "Berries",              emoji: "\u{1FAD0}", price: 1, restore_mins: 20,  revives: false, season: "all" },
    Food { kind: "bread",       name: "Bread Loaf",         plural: "Bread Loaves",         emoji: "\u{1F35E}", price: 1, restore_mins: 20,  revives: false, season: "all" },
    Food { kind: "riceball",    name: "Rice Ball",          plural: "Rice Balls",           emoji: "\u{1F359}", price: 2, restore_mins: 45,  revives: false, season: "all" },
    Food { kind: "coffee",      name: "Coffee",             plural: "Coffees",              emoji: "\u{2615}",  price: 2, restore_mins: 45,  revives: false, season: "all" },
    Food { kind: "bento",       name: "Bento",              plural: "Bentos",               emoji: "\u{1F371}", price: 3, restore_mins: 120, revives: false, season: "all" },
    Food { kind: "noodles",     name: "Noodle Bowl",        plural: "Noodle Bowls",         emoji: "\u{1F35C}", price: 3, restore_mins: 120, revives: false, season: "all" },
    Food { kind: "hotpot",      name: "Hot Pot",            plural: "Hot Pots",             emoji: "\u{1F372}", price: 4, restore_mins: 180, revives: false, season: "all" },
    Food { kind: "tonic",       name: "Revive Tonic",       plural: "Revive Tonics",        emoji: "\u{1F9C3}", price: 5, restore_mins: 0,   revives: true,  season: "all" },
    Food { kind: "elixir",      name: "Honey Elixir",       plural: "Honey Elixirs",        emoji: "\u{1F36F}", price: 7, restore_mins: 60,  revives: true,  season: "all" },
    Food { kind: "strawberry",  name: "Strawberry",         plural: "Strawberries",         emoji: "\u{1F353}", price: 1, restore_mins: 25,  revives: false, season: "spring" },
    // The dango's plural really is identical to its singular.
    Food { kind: "dango",       name: "Hanami Dango",       plural: "Hanami Dango",         emoji: "\u{1F361}", price: 2, restore_mins: 55,  revives: false, season: "spring" },
    Food { kind: "omelette",    name: "Garden Omelette",    plural: "Garden Omelettes",     emoji: "\u{1F373}", price: 3, restore_mins: 135, revives: false, season: "spring" },
    Food { kind: "watermelon",  name: "Watermelon Slice",   plural: "Watermelon Slices",    emoji: "\u{1F349}", price: 1, restore_mins: 25,  revives: false, season: "summer" },
    Food { kind: "shavedice",   name: "Shaved Ice",         plural: "Shaved Ices",          emoji: "\u{1F367}", price: 2, restore_mins: 55,  revives: false, season: "summer" },
    Food { kind: "curry",       name: "Summer Curry",       plural: "Summer Curries",       emoji: "\u{1F35B}", price: 3, restore_mins: 135, revives: false, season: "summer" },
    Food { kind: "apple",       name: "Apple",              plural: "Apples",               emoji: "\u{1F34E}", price: 1, restore_mins: 25,  revives: false, season: "fall" },
    Food { kind: "sweetpotato", name: "Baked Sweet Potato", plural: "Baked Sweet Potatoes", emoji: "\u{1F360}", price: 2, restore_mins: 55,  revives: false, season: "fall" },
    Food { kind: "pumpkinstew", name: "Pumpkin Stew",       plural: "Pumpkin Stews",        emoji: "\u{1F383}", price: 3, restore_mins: 135, revives: false, season: "fall" },
    // The plural moves the 's' to "Bags".
    Food { kind: "chestnuts",   name: "Bag of Chestnuts",   plural: "Bags of Chestnuts",    emoji: "\u{1F330}", price: 1, restore_mins: 25,  revives: false, season: "winter" },
    Food { kind: "cocoa",       name: "Hot Cocoa",          plural: "Hot Cocoas",           emoji: "\u{1F36B}", price: 2, restore_mins: 55,  revives: false, season: "winter" },
    Food { kind: "oden",        name: "Oden Skewer",        plural: "Oden Skewers",         emoji: "\u{1F362}", price: 3, restore_mins: 135, revives: false, season: "winter" },
];

/// The UTC month (1-12) -> season, northern-hemisphere meteorological.
const SEASON_BY_MONTH: [&str; 12] = [
    "winter", "winter", "spring", "spring", "spring", "summer",
    "summer", "summer", "fall", "fall", "fall", "winter",
];

/// quest id -> coins. A pure literal, as in the Python: the client mirrors it
/// and the server verifies the claimed coins against it before crediting.
const QUEST_REWARDS: &[(&str, i64)] = &[
    ("d_prompts_10", 2), ("d_prompts_25", 3), ("d_prompts_50", 5),
    ("d_tools_50", 2), ("d_tools_150", 3), ("d_tools_300", 5),
    ("d_sessions_3", 2), ("d_sessions_5", 3),
    ("d_active", 1),
    ("d_artifacts_1", 2), ("d_artifacts_3", 3),
    ("d_feed_creature", 1),
    ("w_active_5", 5), ("w_active_7", 8),
    ("w_prompts_100", 5), ("w_prompts_250", 8),
    ("w_tools_500", 5),
    ("w_streak_5", 5), ("w_streak_7", 8),
    ("w_folders_3", 3),
];

/// achievement id -> (bronze, silver, gold) coins. A 0 marks a tier that id
/// does not have, which is a 404 and not a 422: `a_first_prompt` and
/// `a_night_owl` are bronze only, and `a_polyglot` has no gold.
const ACH_REWARDS: &[(&str, i64, i64, i64)] = &[
    ("a_first_prompt", 2, 0, 0),
    ("a_prompts", 3, 5, 10),
    ("a_tools", 3, 5, 10),
    ("a_streak", 3, 8, 15),
    ("a_active_days", 3, 8, 15),
    ("a_catch", 3, 5, 15),
    ("a_shiny", 3, 5, 10),
    ("a_evolve", 3, 8, 15),
    ("a_level", 3, 5, 15),
    ("a_artifacts", 3, 5, 10),
    ("a_night_owl", 3, 0, 0),
    ("a_polyglot", 3, 5, 0),
    ("a_gift", 2, 5, 10),
];

/// A catalog kind, validated the moment the body is parsed: an unknown name
/// cannot be represented, so no code path can reach the database holding one.
/// Carries the CATALOG index, which is also its display position and the
/// position of the name in Pydantic's `FoodKind` Literal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Kind(usize);

impl Kind {
    fn food(self) -> &'static Food {
        &CATALOG[self.0]
    }

    fn as_str(self) -> &'static str {
        CATALOG[self.0].kind
    }
}

// --- clocks and timestamps ------------------------------------------------

/// The two clock seams, as in the Python. Everything is keyed off the UTC date,
/// never the local one, so the store, the claim and every daily cap turn over at
/// the same instant for everybody. The date-dependent logic all takes a
/// `NaiveDate` argument so the unit tests below can pin a day without a seam.
fn today() -> NaiveDate {
    Utc::now().date_naive()
}

fn now() -> DateTime<Utc> {
    Utc::now()
}

/// How SQLAlchemy's SQLite DATETIME bind processor writes a timestamp: a space
/// separator, always six microsecond digits, and no offset. Writing that exact
/// shape is what lets the Python backend keep reading rows this one creates.
fn store_dt(dt: DateTime<Utc>) -> String {
    dt.format("%Y-%m-%d %H:%M:%S%.6f").to_string()
}

/// Read one back. SQLite hands back a naive string and everything here was
/// written in UTC, so attach UTC. Tolerant of the 'T' separator the way
/// main.rs's pair-code reader already is.
fn read_dt(s: &str) -> Option<DateTime<Utc>> {
    NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S%.f")
        .or_else(|_| NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%.f"))
        .ok()
        .map(|n| n.and_utc())
}

/// Python's `datetime.isoformat()`, which is not chrono's `to_rfc3339()`: it
/// emits "+00:00" and never "Z", and either zero fractional digits or exactly
/// six. chrono's automatic precision gives 0/3/6/9, so pick explicitly.
fn iso(dt: DateTime<Utc>) -> String {
    let digits = if dt.timestamp_subsec_nanos() == 0 {
        SecondsFormat::Secs
    } else {
        SecondsFormat::Micros
    };
    dt.to_rfc3339_opts(digits, false)
}

fn next_claim_at(day: NaiveDate) -> String {
    iso((day + Duration::days(1)).and_time(NaiveTime::MIN).and_utc())
}

// --- seasons and today's special (pure, deterministic per UTC day) ---------

fn season_of(day: NaiveDate) -> &'static str {
    SEASON_BY_MONTH[day.month0() as usize]
}

/// Whether the store *sells* `food` on `day`. This gates buying only: off-season
/// food you already hold is still eaten and given.
fn in_stock(food: &Food, day: NaiveDate) -> bool {
    food.season == "all" || food.season == season_of(day)
}

/// The day's special: an in-stock food priced at least SPECIAL_MIN_PRICE, picked
/// by a hash of the date so every server and every client agrees all day.
///
/// The hash must stay byte-identical to the Python's: the first 8 lowercase hex
/// chars of sha256("hq:special:" + the ISO day), modulo the pool length, with
/// the pool taken in CATALOG insertion order. That value reaches 4271878196 on
/// 2026-09-29, which overflows i32, so it is parsed as u64.
fn special_of(day: NaiveDate) -> Option<&'static str> {
    let pool: Vec<&'static Food> = CATALOG
        .iter()
        .filter(|f| f.price >= SPECIAL_MIN_PRICE && in_stock(f, day))
        .collect();
    if pool.is_empty() {
        return None;
    }
    let digest = hex::encode(Sha256::digest(format!("hq:special:{day}").as_bytes()));
    // `digest` is 64 hex chars by construction, so this slice and parse cannot fail.
    let h = u64::from_str_radix(&digest[..8], 16).unwrap_or(0);
    Some(pool[(h % pool.len() as u64) as usize].kind)
}

/// What one `food` costs on `day`: its base price, less the special's discount.
/// The discount applies to the special kind only, and the pool's `price >= 2`
/// filter guarantees a discounted price is never below 1.
fn price_of(food: &Food, day: NaiveDate) -> i64 {
    if special_of(day) == Some(food.kind) {
        food.price - SPECIAL_DISCOUNT
    } else {
        food.price
    }
}

fn quest_reward(id: &str) -> Option<i64> {
    QUEST_REWARDS.iter().find(|q| q.0 == id).map(|q| q.1)
}

/// An achievement's coins. A tier the id does not have is `None`, and so is an
/// achievement with no tier at all: the Python does `tiers.get(None)`, which is
/// a 404 rather than a complaint about the body.
fn ach_reward(id: &str, tier: Option<Tier>) -> Option<i64> {
    let row = ACH_REWARDS.iter().find(|a| a.0 == id)?;
    let coins = match tier? {
        Tier::Bronze => row.1,
        Tier::Silver => row.2,
        Tier::Gold => row.3,
    };
    (coins > 0).then_some(coins)
}

// --- wire shapes ----------------------------------------------------------
//
// These are `derive(Serialize)` structs rather than `serde_json::Value` maps on
// purpose: the JS client talks to both backends, and serde_json's Map only
// preserves insertion order when the `preserve_order` feature is on. Field
// *declaration* order below is the order Pydantic emits.

/// `items` is a JSON object whose 21 keys must come out in CATALOG order, so it
/// serialises from an ordered pair list: both `serde_json::Map` and `BTreeMap`
/// would sort them. Coins and the cosmetics feature's `cos:<id>` balance rows
/// are excluded here -- `items` is exactly the food.
#[derive(Debug)]
struct Items(Vec<(&'static str, i64)>);

impl Serialize for Items {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut m = s.serialize_map(Some(self.0.len()))?;
        for (k, v) in &self.0 {
            m.serialize_entry(k, v)?;
        }
        m.end()
    }
}

#[derive(Debug, Serialize)]
struct CatalogItem {
    kind: &'static str,
    name: &'static str,
    plural: &'static str,
    emoji: &'static str,
    /// Today's price, the special's discount applied.
    price: i64,
    #[serde(rename = "basePrice")] base_price: i64,
    #[serde(rename = "restoreMins")] restore_mins: i64,
    revives: bool,
    season: &'static str,
    #[serde(rename = "inStock")] in_stock: bool,
    special: bool,
}

#[derive(Debug, Serialize)]
struct ClaimInfo {
    #[serde(rename = "claimedToday")] claimed_today: bool,
    claimable: bool,
    amount: i64,
    today: String,
    #[serde(rename = "nextClaimAt")] next_claim_at: String,
}

#[derive(Debug, Serialize)]
struct PantryLimits {
    #[serde(rename = "buyMaxQty")] buy_max_qty: i64,
    #[serde(rename = "giftMaxCoins")] gift_max_coins: i64,
    #[serde(rename = "giftMaxQty")] gift_max_qty: i64,
    #[serde(rename = "giftsLeftToday")] gifts_left_today: i64,
}

#[derive(Debug, Serialize)]
struct GiftItem {
    #[serde(rename = "fromHandle")] from_handle: String,
    #[serde(rename = "fromName")] from_name: String,
    coins: i64,
    kind: Option<String>,
    qty: i64,
    note: String,
    at: String,
}

/// The drain's shape: a GiftItem with the ledger row's id appended. Only the
/// drain carries an id; `recentGifts` does not.
#[derive(Debug, Serialize)]
struct DrainedGift {
    #[serde(flatten)] gift: GiftItem,
    id: String,
}

#[derive(Debug, Serialize)]
struct GiftsResponse {
    gifts: Vec<DrainedGift>,
}

#[derive(Debug, Serialize)]
struct PantryState {
    coins: i64,
    #[serde(rename = "coinCap")] coin_cap: i64,
    items: Items,
    #[serde(rename = "itemCap")] item_cap: i64,
    catalog: Vec<CatalogItem>,
    claim: ClaimInfo,
    limits: PantryLimits,
    #[serde(rename = "recentGifts")] recent_gifts: Vec<GiftItem>,
    season: &'static str,
    special: Option<&'static str>,
}

#[derive(Debug, Serialize)]
struct ClaimResponse {
    #[serde(flatten)] state: PantryState,
    op: &'static str,
    claimed: bool,
    granted: i64,
    starter: bool,
    full: bool,
}

#[derive(Debug, Serialize)]
struct BuyResponse {
    #[serde(flatten)] state: PantryState,
    op: &'static str,
    replayed: bool,
    kind: String,
    qty: i64,
    spent: i64,
}

#[derive(Debug, Serialize)]
struct EatResponse {
    #[serde(flatten)] state: PantryState,
    op: &'static str,
    replayed: bool,
    kind: String,
    #[serde(rename = "restoreMins")] restore_mins: i64,
    revives: bool,
    at: String,
}

#[derive(Debug, Serialize)]
struct SentGift {
    coins: i64,
    kind: Option<String>,
    qty: i64,
}

#[derive(Debug, Serialize)]
struct GiveResponse {
    #[serde(flatten)] state: PantryState,
    op: &'static str,
    replayed: bool,
    #[serde(rename = "toHandle")] to_handle: String,
    sent: SentGift,
    /// A COUNT of sockets reached, not a bool: the page reads 0 as "queued".
    #[serde(rename = "deliveredLive")] delivered_live: i64,
}

/// `reward` answers a completely different shape from every other pantry POST:
/// no PantryState and no "op".
#[derive(Debug, Serialize)]
struct RewardResponse {
    ok: bool,
    coins: i64,
    reward: i64,
}

// --- request shapes -------------------------------------------------------
//
// Parsed by hand out of a `serde_json::Value` rather than derived, because the
// client reads `detail[].loc` to find the offending field: a 422 has to keep
// Pydantic's list-of-{loc, msg, type} shape, and serde's error text would not.
// `Value::as_i64` is None for `true`, `"2"` and `1.5` alike, which is exactly
// what Pydantic's `strict=True` ints refuse.

#[derive(Debug)]
struct BuyRequest {
    request_id: String,
    kind: Kind,
    /// Defaults to 1 when the field is absent.
    qty: i64,
}

#[derive(Debug)]
struct EatRequest {
    request_id: String,
    /// There is no qty: eating is always exactly 1.
    kind: Kind,
}

#[derive(Debug)]
struct GiveRequest {
    request_id: String,
    to_handle: String,
    coins: i64,
    kind: Option<Kind>,
    qty: i64,
    note: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RewardKind {
    Quest,
    Achievement,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tier {
    Bronze,
    Silver,
    Gold,
}

#[derive(Debug)]
struct RewardRequest {
    /// A different namespace from every other endpoint's: up to 100 chars
    /// matching `^(quest|ach):[a-z0-9_]+:.+$`, which *requires* the colons the
    /// others forbid. It is never cross-checked against `questId`.
    request_id: String,
    kind: RewardKind,
    quest_id: String,
    tier: Option<Tier>,
    coins: i64,
}

// --- body validation ------------------------------------------------------
//
// Hand-rolled rather than derived, because the client reads `detail[].loc` to
// find the offending field. FastAPI answers 422 with two DIFFERENT bodies and
// each site here has to pick the right one:
//
//   * a Pydantic validation failure -- a missing field, a wrong type, a failed
//     constraint, a bad Literal, an `extra="forbid"` violation, or a
//     `model_validator` raising -- reaches app/main.py's `_scrub_422` and comes
//     back as a LIST of `{loc, msg, type}`. That is everything below, and the
//     `msg`/`type` strings are pydantic-core's own;
//   * an explicit `raise HTTPException(422, "...")` in the route comes back as
//     a bare string. `do_reward`'s "reward amount does not match the catalog"
//     is the only one in this module, and it stays a string.
//
// The constraint order is Pydantic's too: min_length, then max_length, then
// pattern, at most one error per field; the declared fields are reported before
// the unknown keys; and the `model_validator(mode="after")` cross-field rules
// run only once every field has passed.

const TOKEN_PATTERN: &str = "^[A-Za-z0-9_-]+$";
const REWARD_PATTERN: &str = "^(quest|ach):[a-z0-9_]+:.+$";
const REQUEST_ID_MIN: usize = 16;
const REQUEST_ID_MAX: usize = 64;
const HANDLE_MIN: usize = 1;
const HANDLE_MAX: usize = 64;
const REWARD_KINDS: [&str; 2] = ["quest", "achievement"];
const TIERS: [&str; 3] = ["bronze", "silver", "gold"];

/// One Pydantic error entry, scrubbed to the three keys app/main.py's
/// `_scrub_422` keeps, in its order. `input`, `ctx` and `url` are deliberately
/// absent: the Python handler strips them so a rejected value is never
/// reflected back to the caller, and this must not reintroduce them.
#[derive(Debug, Serialize)]
struct Verr {
    loc: Vec<Value>,
    msg: String,
    #[serde(rename = "type")] kind: &'static str,
}

/// The 422 envelope. A `derive(Serialize)` struct rather than a `json!` map so
/// the three keys keep their declaration order without leaning on serde_json's
/// `preserve_order` feature, which this crate does not enable.
#[derive(Debug, Serialize)]
struct Errors {
    detail: Vec<Verr>,
}

/// An error about the body as a whole, the way a failed decode or a
/// `model_validator` reports one: `loc` is just `["body"]`.
fn body_err(kind: &'static str, msg: impl Into<String>) -> Verr {
    Verr { loc: vec![json!("body")], msg: msg.into(), kind }
}

/// An error about one field of the body.
fn field_err(name: &str, kind: &'static str, msg: impl Into<String>) -> Verr {
    Verr { loc: vec![json!("body"), json!(name)], msg: msg.into(), kind }
}

/// The 422 a *validation* failure answers with: a list of `{loc, msg, type}`
/// and nothing else, exactly as `_scrub_422` builds it. Never echo the
/// submitted value back -- a rejected body is not reflected.
fn err422(detail: Vec<Verr>) -> Response {
    (StatusCode::UNPROCESSABLE_ENTITY, Json(Errors { detail })).into_response()
}

/// Pydantic's singular in the length messages: "at least 1 character".
fn chars_word(n: usize) -> &'static str {
    if n == 1 {
        "character"
    } else {
        "characters"
    }
}

/// `^[A-Za-z0-9_-]+$` spelled out. The anchors make it a full match and the `+`
/// is why the empty string fails.
fn token_ok(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// `^(quest|ach):[a-z0-9_]+:.+$`, checked by hand. Splitting on the first colon
/// matches the regex because `[a-z0-9_]+` can never cover one. The length is
/// NOT checked here: `max_length` is a separate constraint and Pydantic applies
/// it first, so the two must stay separable.
fn reward_id_ok(s: &str) -> bool {
    let Some(rest) = s.strip_prefix("quest:").or_else(|| s.strip_prefix("ach:")) else {
        return false;
    };
    let Some((id, tail)) = rest.split_once(':') else {
        return false;
    };
    !id.is_empty()
        && id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        && !tail.is_empty()
}

/// The two `pattern=` regexes this module validates against. The source text
/// travels with the predicate because Pydantic quotes the pattern back in the
/// message, so the pair must not drift apart.
#[derive(Debug, Clone, Copy)]
enum Pat {
    /// Pydantic's `RequestId` and `Handle` pattern. That it forbids ':' is what
    /// reserves "claim:YYYY-MM-DD" for the server -- a client can never forge
    /// or collide with the daily claim key.
    Token,
    /// The reward namespace, which *requires* the colons `Token` forbids.
    Reward,
}

impl Pat {
    fn text(self) -> &'static str {
        match self {
            Pat::Token => TOKEN_PATTERN,
            Pat::Reward => REWARD_PATTERN,
        }
    }

    fn matches(self, s: &str) -> bool {
        match self {
            Pat::Token => token_ok(s),
            Pat::Reward => reward_id_ok(s),
        }
    }
}

/// Read the body as a JSON object, in FastAPI's own order: an empty body is a
/// missing body field, something that will not decode is a JSON decode error,
/// and a JSON value that is not an object has no fields to extract.
fn body_object(bytes: &[u8]) -> Result<Map<String, Value>, Vec<Verr>> {
    if bytes.is_empty() {
        return Err(vec![body_err("missing", "Field required")]);
    }
    match serde_json::from_slice::<Value>(bytes) {
        Ok(Value::Object(o)) => Ok(o),
        Ok(_) => Err(vec![body_err(
            "model_attributes_type",
            "Input should be a valid dictionary or object to extract fields from",
        )]),
        Err(_) => Err(vec![body_err("json_invalid", "JSON decode error")]),
    }
}

/// A required `str` field with `min_length`, `max_length` and an optional
/// `pattern`, applied in that order: the first failure is the only error
/// reported for the field. A `min` of 0 is "no minimum", the way a Python
/// `Field(max_length=...)` leaves it out.
fn str_field(
    obj: &Map<String, Value>,
    name: &str,
    min: usize,
    max: usize,
    pat: Option<Pat>,
    errs: &mut Vec<Verr>,
) -> Option<String> {
    let s = match obj.get(name) {
        Some(Value::String(s)) => s,
        // An explicit null lands here too, exactly as Pydantic's string_type.
        Some(_) => {
            errs.push(field_err(name, "string_type", "Input should be a valid string"));
            return None;
        }
        None => {
            errs.push(field_err(name, "missing", "Field required"));
            return None;
        }
    };
    let n = s.chars().count();
    if n < min {
        errs.push(field_err(
            name,
            "string_too_short",
            format!("String should have at least {min} {}", chars_word(min)),
        ));
    } else if n > max {
        errs.push(field_err(
            name,
            "string_too_long",
            format!("String should have at most {max} {}", chars_word(max)),
        ));
    } else if let Some(p) = pat.filter(|p| !p.matches(s)) {
        errs.push(field_err(
            name,
            "string_pattern_mismatch",
            format!("String should match pattern '{}'", p.text()),
        ));
    } else {
        return Some(s.clone());
    }
    None
}

/// A `str` field with a `""` default and `max_length` only, so an absent field
/// is the default rather than a "missing". The cap is checked against the *raw*
/// input, which is why an over-long note is a 422 and never truncated quietly.
fn text_field(
    obj: &Map<String, Value>,
    name: &str,
    max: usize,
    errs: &mut Vec<Verr>,
) -> Option<String> {
    match obj.get(name) {
        None => Some(String::new()),
        Some(Value::String(s)) => {
            if s.chars().count() > max {
                errs.push(field_err(
                    name,
                    "string_too_long",
                    format!("String should have at most {max} {}", chars_word(max)),
                ));
                return None;
            }
            Some(s.clone())
        }
        Some(_) => {
            errs.push(field_err(name, "string_type", "Input should be a valid string"));
            None
        }
    }
}

/// A `strict=True` bounded int. A `default` of `None` makes the field required.
/// Rust's types give Pydantic's `isinstance(v, int) and not isinstance(v, bool)`
/// for free: `as_i64` is None for a bool, a float and a numeric string alike.
fn int_field(
    obj: &Map<String, Value>,
    name: &str,
    default: Option<i64>,
    min: i64,
    max: i64,
    errs: &mut Vec<Verr>,
) -> Option<i64> {
    let Some(v) = obj.get(name) else {
        if default.is_none() {
            errs.push(field_err(name, "missing", "Field required"));
        }
        return default;
    };
    let Some(n) = v.as_i64() else {
        errs.push(field_err(name, "int_type", "Input should be a valid integer"));
        return None;
    };
    if n > max {
        errs.push(field_err(
            name,
            "less_than_equal",
            format!("Input should be less than or equal to {max}"),
        ));
        return None;
    }
    if n < min {
        errs.push(field_err(
            name,
            "greater_than_equal",
            format!("Input should be greater than or equal to {min}"),
        ));
        return None;
    }
    Some(n)
}

/// Pydantic's rendering of a `Literal[...]`'s allowed values: each quoted,
/// comma-separated, with " or " before the last.
fn literal_expected(values: &[&str]) -> String {
    let quoted: Vec<String> = values.iter().map(|v| format!("'{v}'")).collect();
    match quoted.split_last() {
        Some((last, [])) => last.clone(),
        Some((last, head)) => format!("{} or {last}", head.join(", ")),
        None => String::new(),
    }
}

/// A required `Literal[...]` of strings; the index into `values` on success.
/// Anything that is not one of them -- another string, a number, a null -- is
/// the one `literal_error`, as pydantic-core's lookup gives it.
fn literal_field(
    obj: &Map<String, Value>,
    name: &str,
    values: &[&str],
    errs: &mut Vec<Verr>,
) -> Option<usize> {
    let Some(v) = obj.get(name) else {
        errs.push(field_err(name, "missing", "Field required"));
        return None;
    };
    let hit = v.as_str().and_then(|s| values.iter().position(|x| *x == s));
    if hit.is_none() {
        errs.push(field_err(
            name,
            "literal_error",
            format!("Input should be {}", literal_expected(values)),
        ));
    }
    hit
}

/// The same field as `Literal[...] | None`, which Pydantic builds as a nullable
/// literal: an absent field and an explicit null are both None, and anything
/// else gets the literal's own error. The OUTER `None` means "that was an
/// error", the inner one "there was no value".
fn optional_literal_field(
    obj: &Map<String, Value>,
    name: &str,
    values: &[&str],
    errs: &mut Vec<Verr>,
) -> Option<Option<usize>> {
    match obj.get(name) {
        None | Some(Value::Null) => Some(None),
        Some(v) => match v.as_str().and_then(|s| values.iter().position(|x| *x == s)) {
            Some(i) => Some(Some(i)),
            None => {
                errs.push(field_err(
                    name,
                    "literal_error",
                    format!("Input should be {}", literal_expected(values)),
                ));
                None
            }
        },
    }
}

/// `extra="forbid"`. Pydantic walks the declared fields first and the unknown
/// keys afterwards, so this is always called last.
fn forbid_extra(obj: &Map<String, Value>, known: &[&str], errs: &mut Vec<Verr>) {
    for k in obj.keys().filter(|k| !known.contains(&k.as_str())) {
        errs.push(field_err(k, "extra_forbidden", "Extra inputs are not permitted"));
    }
}

/// `FoodKind`'s Literal values, in the Python's declaration order -- which is
/// CATALOG order, so the index this yields is a [`Kind`].
fn food_kinds() -> Vec<&'static str> {
    CATALOG.iter().map(|f| f.kind).collect()
}

fn buy_body(bytes: &[u8]) -> Result<BuyRequest, Vec<Verr>> {
    let obj = body_object(bytes)?;
    let mut errs = Vec::new();
    let request_id =
        str_field(&obj, "requestId", REQUEST_ID_MIN, REQUEST_ID_MAX, Some(Pat::Token), &mut errs);
    let kind = literal_field(&obj, "kind", &food_kinds(), &mut errs).map(Kind);
    let qty = int_field(&obj, "qty", Some(1), 1, BUY_MAX_QTY, &mut errs);
    forbid_extra(&obj, &["requestId", "kind", "qty"], &mut errs);
    let (Some(request_id), Some(kind), Some(qty)) = (request_id, kind, qty) else {
        return Err(errs);
    };
    if !errs.is_empty() {
        return Err(errs);
    }
    Ok(BuyRequest { request_id, kind, qty })
}

fn eat_body(bytes: &[u8]) -> Result<EatRequest, Vec<Verr>> {
    let obj = body_object(bytes)?;
    let mut errs = Vec::new();
    let request_id =
        str_field(&obj, "requestId", REQUEST_ID_MIN, REQUEST_ID_MAX, Some(Pat::Token), &mut errs);
    let kind = literal_field(&obj, "kind", &food_kinds(), &mut errs).map(Kind);
    forbid_extra(&obj, &["requestId", "kind"], &mut errs);
    let (Some(request_id), Some(kind)) = (request_id, kind) else {
        return Err(errs);
    };
    if !errs.is_empty() {
        return Err(errs);
    }
    Ok(EatRequest { request_id, kind })
}

fn give_body(bytes: &[u8]) -> Result<GiveRequest, Vec<Verr>> {
    let obj = body_object(bytes)?;
    let mut errs = Vec::new();
    let request_id =
        str_field(&obj, "requestId", REQUEST_ID_MIN, REQUEST_ID_MAX, Some(Pat::Token), &mut errs);
    let to_handle =
        str_field(&obj, "toHandle", HANDLE_MIN, HANDLE_MAX, Some(Pat::Token), &mut errs);
    let coins = int_field(&obj, "coins", Some(0), 0, GIFT_MAX_COINS, &mut errs);
    let kind = optional_literal_field(&obj, "kind", &food_kinds(), &mut errs);
    let qty = int_field(&obj, "qty", Some(0), 0, GIFT_MAX_QTY, &mut errs);
    let note = text_field(&obj, "note", NOTE_MAX, &mut errs);
    forbid_extra(&obj, &["requestId", "toHandle", "coins", "kind", "qty", "note"], &mut errs);
    let (Some(request_id), Some(to_handle), Some(coins), Some(kind), Some(qty), Some(note)) =
        (request_id, to_handle, coins, kind, qty, note)
    else {
        return Err(errs);
    };
    if !errs.is_empty() {
        return Err(errs);
    }
    // `model_validator(mode="after")` runs only once every field has passed,
    // and the ValueError it raises is one body-level error, not a field one.
    if let Err(msg) = give_shape(coins, kind.is_some(), qty) {
        return Err(vec![body_err("value_error", format!("Value error, {msg}"))]);
    }
    Ok(GiveRequest { request_id, to_handle, coins, kind: kind.map(Kind), qty, note })
}

fn reward_body(bytes: &[u8]) -> Result<RewardRequest, Vec<Verr>> {
    let obj = body_object(bytes)?;
    let mut errs = Vec::new();
    // No min_length here, unlike every other requestId: the Python declares
    // this one with max_length and a pattern only.
    let request_id = str_field(&obj, "requestId", 0, REWARD_ID_MAX, Some(Pat::Reward), &mut errs);
    let kind = literal_field(&obj, "kind", &REWARD_KINDS, &mut errs);
    let quest_id = str_field(&obj, "questId", 0, QUEST_ID_MAX, None, &mut errs);
    let tier = optional_literal_field(&obj, "tier", &TIERS, &mut errs);
    let coins = int_field(&obj, "coins", None, 1, REWARD_COINS_MAX, &mut errs);
    forbid_extra(&obj, &["requestId", "kind", "questId", "tier", "coins"], &mut errs);
    let (Some(request_id), Some(kind), Some(quest_id), Some(tier), Some(coins)) =
        (request_id, kind, quest_id, tier, coins)
    else {
        return Err(errs);
    };
    if !errs.is_empty() {
        return Err(errs);
    }
    Ok(RewardRequest {
        request_id,
        kind: if kind == 0 { RewardKind::Quest } else { RewardKind::Achievement },
        quest_id,
        tier: tier.map(|i| [Tier::Bronze, Tier::Silver, Tier::Gold][i]),
        coins,
    })
}

/// GiveRequest's three cross-field rules, which Pydantic runs after the field
/// ranges. Note the second: sending `kind` without a `qty` is an error, not a
/// default-to-1.
fn give_shape(coins: i64, has_kind: bool, qty: i64) -> Result<(), &'static str> {
    if qty > 0 && !has_kind {
        return Err("kind is required when qty > 0");
    }
    if has_kind && qty == 0 {
        return Err("qty is required with kind");
    }
    if coins == 0 && qty == 0 {
        return Err("a gift needs coins or food");
    }
    Ok(())
}

/// Python's note validator: drop every char where `str.isprintable()` is false,
/// collapse whitespace runs to single spaces and trim.
///
/// Rust has no category table, and `char::is_control()` is not the inverse of
/// `isprintable` -- U+00A0 and the other Zs/Zl/Zp spaces are printable=false
/// too -- so those are dropped as well and `split_whitespace` then collapses the
/// plain spaces that remain. This matches on ASCII and on the \x07/\n case the
/// tests pin; it still diverges on Cf (a zero-width joiner survives here).
///
/// The `max_length` check fires *before* this in Pydantic, so an 81-char note is
/// already a 422 and the truncation is belt and braces.
fn clean_note(raw: &str) -> String {
    let printable: String = raw
        .chars()
        .filter(|c| *c == ' ' || (!c.is_control() && !c.is_whitespace()))
        .collect();
    printable
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(NOTE_MAX)
        .collect()
}

// --- failures -------------------------------------------------------------

/// A refused guard, or SQLite saying no. `From<sqlx::Error>` classifies the
/// database's answer once, so `?` can carry it everywhere.
#[derive(Debug)]
enum Fail {
    /// A guard refused: this status and message are the client's whole answer.
    Refused(StatusCode, String),
    /// SQLITE_BUSY: the write lock never came free inside `busy_timeout`.
    Busy,
    /// A UNIQUE(user_id, request_id) conflict. The caller rolls back and looks
    /// for the winner's row to replay; reaching a handler means it never showed
    /// up, which is the same transient answer as a busy lock.
    Unique,
    /// Anything else SQLite said.
    Db(String),
}

impl Fail {
    fn refused(code: StatusCode, msg: &str) -> Self {
        Fail::Refused(code, msg.to_string())
    }

    fn response(self) -> Response {
        match self {
            Fail::Refused(code, msg) => crate::err(code, &msg),
            Fail::Busy | Fail::Unique => crate::err(StatusCode::SERVICE_UNAVAILABLE, BUSY),
            Fail::Db(msg) => {
                crate::err(StatusCode::INTERNAL_SERVER_ERROR, &format!("db error: {msg}"))
            }
        }
    }
}

impl From<sqlx::Error> for Fail {
    fn from(e: sqlx::Error) -> Self {
        if let Some(d) = e.as_database_error() {
            // SQLITE_CONSTRAINT_UNIQUE is the extended code 2067, which sqlx
            // reports as a unique violation; the message check is belt and braces.
            if d.is_unique_violation() || d.message().contains("UNIQUE constraint failed") {
                return Fail::Unique;
            }
            // SQLITE_BUSY and SQLITE_LOCKED, masked out of their extended codes.
            let primary = d.code().and_then(|c| c.parse::<i32>().ok()).map(|c| c & 0xff);
            if matches!(primary, Some(5 | 6)) {
                return Fail::Busy;
            }
        }
        Fail::Db(e.to_string())
    }
}

fn not_enough(label: &str, n: i64, have: i64) -> Fail {
    Fail::refused(
        StatusCode::CONFLICT,
        &format!("not enough {label} (need {n}, you have {have})"),
    )
}

// --- balances and the journal ---------------------------------------------

/// One poke_ledger row, as every response reads it back.
#[derive(Debug, Clone)]
struct Ledger {
    id: String,
    op: String,
    /// The row's own day, which for a replayed buy is the ORIGINAL day.
    op_date: String,
    kind: Option<String>,
    qty: i64,
    coins: i64,
    to_user_id: Option<String>,
    note: String,
    /// Kept as the stored text; `at()` turns it into the wire form.
    created_at: String,
    /// Kept as the stored text too -- only its presence is ever read.
    delivered_at: Option<String>,
}

impl Ledger {
    /// The row's `created_at` on the wire. A row we cannot parse would be
    /// corrupt, and answering "" beats inventing a time.
    fn at(&self) -> String {
        read_dt(&self.created_at).map(iso).unwrap_or_default()
    }
}

const LEDGER_SELECT: &str = "SELECT id, op, op_date, kind, qty, coins, to_user_id, note,
                                    created_at, delivered_at
                             FROM poke_ledger";

const LEDGER_INSERT: &str = "INSERT INTO poke_ledger
                               (id, user_id, request_id, op, op_date, kind, qty, coins,
                                to_user_id, note, created_at, delivered_at)
                             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,NULL)";

fn ledger_from(r: &SqliteRow) -> Ledger {
    Ledger {
        id: r.get("id"),
        op: r.get("op"),
        op_date: r.get("op_date"),
        kind: r.get("kind"),
        qty: r.get("qty"),
        coins: r.get("coins"),
        to_user_id: r.get("to_user_id"),
        note: r.get("note"),
        created_at: r.get("created_at"),
        delivered_at: r.get("delivered_at"),
    }
}

/// The op holding this request id, if any. The idempotency key is
/// (user_id, request_id), UNIQUE on poke_ledger. Always read outside a
/// transaction, which is where the Python reads it too.
async fn find_op(pool: &SqlitePool, uid: &str, rid: &str) -> Result<Option<Ledger>, Fail> {
    let row = sqlx::query(&format!("{LEDGER_SELECT} WHERE user_id = ?1 AND request_id = ?2"))
        .bind(uid)
        .bind(rid)
        .fetch_optional(pool)
        .await?;
    Ok(row.as_ref().map(ledger_from))
}

/// The balance, or `None` when there is no row yet. The difference matters:
/// `credit` inserts only when no row exists.
async fn qty_of(conn: &mut SqliteConnection, uid: &str, item: &str) -> Result<Option<i64>, Fail> {
    Ok(
        sqlx::query_scalar("SELECT qty FROM poke_balances WHERE user_id = ?1 AND item = ?2")
            .bind(uid)
            .bind(item)
            .fetch_optional(&mut *conn)
            .await?,
    )
}

async fn coins_of(pool: &SqlitePool, uid: &str) -> Result<i64, Fail> {
    Ok(
        sqlx::query_scalar("SELECT qty FROM poke_balances WHERE user_id = ?1 AND item = 'coins'")
            .bind(uid)
            .fetch_optional(pool)
            .await?
            .unwrap_or(0),
    )
}

/// Take n, or refuse with 409. `short` replaces the need/have message, which is
/// how eating hides how much you were short by.
async fn debit(
    conn: &mut SqliteConnection,
    uid: &str,
    item: &str,
    n: i64,
    label: &str,
    short: Option<&str>,
) -> Result<(), Fail> {
    // The cap lives in the WHERE clause: this conditional UPDATE *is* the guard.
    let res = sqlx::query(
        "UPDATE poke_balances SET qty = qty - ?3, updated_at = datetime('now')
         WHERE user_id = ?1 AND item = ?2 AND qty >= ?3",
    )
    .bind(uid)
    .bind(item)
    .bind(n)
    .execute(&mut *conn)
    .await?;
    if res.rows_affected() == 1 {
        return Ok(());
    }
    if let Some(msg) = short {
        return Err(Fail::refused(StatusCode::CONFLICT, msg));
    }
    let have = qty_of(&mut *conn, uid, item).await?.unwrap_or(0);
    Err(not_enough(label, n, have))
}

/// Add n without passing cap, or refuse with 409 `err_msg`. Three-way outcome,
/// reproduced exactly: the conditional UPDATE landed; or it did not and either a
/// row already exists or n alone exceeds the cap, which is the refusal; or there
/// is no row at all and n fits, which inserts one.
async fn credit(
    conn: &mut SqliteConnection,
    uid: &str,
    item: &str,
    n: i64,
    cap: i64,
    err_msg: &str,
) -> Result<(), Fail> {
    let res = sqlx::query(
        "UPDATE poke_balances SET qty = qty + ?3, updated_at = datetime('now')
         WHERE user_id = ?1 AND item = ?2 AND qty + ?3 <= ?4",
    )
    .bind(uid)
    .bind(item)
    .bind(n)
    .bind(cap)
    .execute(&mut *conn)
    .await?;
    if res.rows_affected() == 1 {
        return Ok(());
    }
    if qty_of(&mut *conn, uid, item).await?.is_some() || n > cap {
        return Err(Fail::refused(StatusCode::CONFLICT, err_msg));
    }
    // updated_at has a CURRENT_TIMESTAMP default: second precision, no
    // microseconds, which is what SQLAlchemy's func.now() renders here too.
    sqlx::query("INSERT INTO poke_balances (id, user_id, item, qty) VALUES (?1,?2,?3,?4)")
        .bind(uuid::Uuid::new_v4().to_string())
        .bind(uid)
        .bind(item)
        .bind(n)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

/// Refuse as `debit` would unless uid holds n. A read, not a hold: the
/// conditional debit is still the real guard. This exists for its *ordering* --
/// see the privacy note in `Job::apply`.
async fn afford(
    conn: &mut SqliteConnection,
    uid: &str,
    item: &str,
    n: i64,
    label: &str,
) -> Result<(), Fail> {
    let have = qty_of(&mut *conn, uid, item).await?.unwrap_or(0);
    if have < n {
        return Err(not_enough(label, n, have));
    }
    Ok(())
}

// --- the shared state read -------------------------------------------------

fn gift_item(r: &SqliteRow) -> GiftItem {
    let handle: String = r.get("handle");
    let display: String = r.get("display_name");
    let created: String = r.get("created_at");
    GiftItem {
        from_name: if display.is_empty() { handle.clone() } else { display },
        from_handle: handle,
        coins: r.get("coins"),
        kind: r.get("kind"),
        qty: r.get("qty"),
        note: r.get("note"),
        at: read_dt(&created).map(iso).unwrap_or_default(),
    }
}

/// The PantryState every endpoint returns, the claim/buy/eat/give ones re-reading
/// it after their commit. Read-only: it creates no rows and marks nothing
/// delivered, so a GET never claims the daily coins and never drains gifts.
async fn pantry_state(pool: &SqlitePool, uid: &str) -> Result<PantryState, Fail> {
    let day = today();

    let rows = sqlx::query("SELECT item, qty FROM poke_balances WHERE user_id = ?1")
        .bind(uid)
        .fetch_all(pool)
        .await?;
    let held: HashMap<String, i64> = rows
        .iter()
        .map(|r| (r.get::<String, _>("item"), r.get::<i64, _>("qty")))
        .collect();
    let coins = held.get("coins").copied().unwrap_or(0);

    let claimed_today = find_op(pool, uid, &format!("claim:{day}")).await?.is_some();
    let gives_today: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM poke_ledger
         WHERE op = 'give' AND user_id = ?1 AND op_date = ?2",
    )
    .bind(uid)
    .bind(day.to_string())
    .fetch_one(pool)
    .await?;

    // Newest first, over the last 7 days inclusive of the boundary date, and
    // delivered and undelivered alike -- the exact opposite of the drain, which
    // sorts oldest first over undelivered only. Both tie-break on id so the
    // order is total. The INNER join is on the SENDER: poke_ledger.user_id is ON
    // DELETE CASCADE, so a deleted sender takes their gift rows with them.
    let since = (day - Duration::days(RECENT_GIFT_DAYS)).to_string();
    let recent = sqlx::query(
        "SELECT l.coins, l.kind, l.qty, l.note, l.created_at, u.handle, u.display_name
         FROM poke_ledger l JOIN users u ON u.id = l.user_id
         WHERE l.op = 'give' AND l.to_user_id = ?1 AND l.op_date >= ?2
         ORDER BY l.created_at DESC, l.id DESC
         LIMIT ?3",
    )
    .bind(uid)
    .bind(&since)
    .bind(RECENT_GIFTS)
    .fetch_all(pool)
    .await?;

    let special = special_of(day);
    Ok(PantryState {
        coins,
        coin_cap: COIN_CAP,
        items: Items(
            CATALOG
                .iter()
                .map(|f| (f.kind, held.get(f.kind).copied().unwrap_or(0)))
                .collect(),
        ),
        item_cap: ITEM_CAP,
        catalog: CATALOG
            .iter()
            .map(|f| CatalogItem {
                kind: f.kind,
                name: f.name,
                plural: f.plural,
                emoji: f.emoji,
                price: price_of(f, day),
                base_price: f.price,
                restore_mins: f.restore_mins,
                revives: f.revives,
                season: f.season,
                in_stock: in_stock(f, day),
                special: special == Some(f.kind),
            })
            .collect(),
        claim: ClaimInfo {
            claimed_today,
            claimable: !claimed_today && coins < COIN_CAP,
            amount: DAILY_COINS,
            today: day.to_string(),
            next_claim_at: next_claim_at(day),
        },
        limits: PantryLimits {
            buy_max_qty: BUY_MAX_QTY,
            gift_max_coins: GIFT_MAX_COINS,
            gift_max_qty: GIFT_MAX_QTY,
            gifts_left_today: (GIFTS_PER_DAY - gives_today).max(0),
        },
        recent_gifts: recent.iter().map(gift_item).collect(),
        season: season_of(day),
        special,
    })
}

// --- the spend recipe (buy, eat, give) ------------------------------------

/// (op, kind, qty, coins, to_user_id): the tuple a replay must match. A mismatch
/// means the id was reused for something else, which is a 409.
struct Want {
    op: &'static str,
    kind: Option<&'static str>,
    qty: i64,
    coins: i64,
    to_user_id: Option<String>,
}

impl Want {
    fn matches(&self, r: &Ledger) -> bool {
        r.op == self.op
            && r.kind.as_deref() == self.kind
            && r.qty == self.qty
            && r.coins == self.coins
            && r.to_user_id.as_deref() == self.to_user_id.as_deref()
    }
}

/// A gift's resolved target and payload. The target's values are copied out as
/// plain strings before anything can roll back, as the Python does.
struct Gift {
    coins: i64,
    kind: Option<Kind>,
    qty: i64,
    note: String,
    to_id: Option<String>,
    to_handle: String,
    to_active: bool,
}

/// What one spend does beyond writing the journal row. `Give` is boxed so the
/// enum stays small.
enum Job {
    Buy { kind: Kind, qty: i64, cost: i64, day: NaiveDate },
    Eat { kind: Kind },
    Give(Box<Gift>),
}

impl Job {
    fn want(&self) -> Want {
        match self {
            Job::Buy { kind, qty, cost, .. } => Want {
                op: "buy",
                kind: Some(kind.as_str()),
                qty: *qty,
                coins: *cost,
                to_user_id: None,
            },
            Job::Eat { kind } => Want {
                op: "eat",
                kind: Some(kind.as_str()),
                qty: 1,
                coins: 0,
                to_user_id: None,
            },
            Job::Give(g) => Want {
                op: "give",
                kind: g.kind.map(Kind::as_str),
                qty: g.qty,
                coins: g.coins,
                to_user_id: g.to_id.clone(),
            },
        }
    }

    fn note(&self) -> &str {
        match self {
            Job::Give(g) => &g.note,
            _ => "",
        }
    }

    /// The row's own `op_date`. A buy carries the day its price came from;
    /// everything else is today. Every daily cap keys off this, not off today.
    fn op_date(&self, today: NaiveDate) -> NaiveDate {
        match self {
            Job::Buy { day, .. } => *day,
            _ => today,
        }
    }

    /// Refusals that must not fire on a replay. `spend` calls this only when
    /// there is no prior row, which is why a committed buy replays after its
    /// season has turned and a committed give replays even if the recipient has
    /// since been deleted or deactivated.
    fn check(&self, me: &str) -> Result<(), Fail> {
        match self {
            Job::Buy { kind, day, .. } if !in_stock(kind.food(), *day) => Err(Fail::refused(
                // A 409, not a 422, and built from the PLURAL.
                StatusCode::CONFLICT,
                &format!("{} are out of season", kind.food().plural),
            )),
            // No in_stock test here: off-season food you already hold is eatable
            // and giftable.
            Job::Give(g) if g.to_id.is_none() || !g.to_active => {
                // The handle lookup does not filter is_active in SQL, so "no
                // such handle" and "handle exists but deactivated" are
                // deliberately indistinguishable -- and both are 404, not 403.
                Err(Fail::refused(StatusCode::NOT_FOUND, "no such person"))
            }
            // 400, not 409 or 403, and after the existence check, so giving to a
            // nonexistent handle that happens to be your own old one is still 404.
            Job::Give(g) if g.to_id.as_deref() == Some(me) => {
                Err(Fail::refused(StatusCode::BAD_REQUEST, "you can't give to yourself"))
            }
            _ => Ok(()),
        }
    }

    /// The op's caps, debits and credits, inside the transaction and after the
    /// shared ops cap. Returns the `delivered_at` it stamped, if any.
    async fn apply(
        &self,
        conn: &mut SqliteConnection,
        st: &crate::AppState,
        c: &crate::Caller,
        id: &str,
        op_date: &str,
    ) -> Result<Option<String>, Fail> {
        match self {
            Job::Buy { kind, qty, cost, .. } => {
                debit(&mut *conn, &c.user_id, "coins", *cost, "Poke Coins", None).await?;
                // Refuses the WHOLE purchase rather than clamping to the cap,
                // and the coin debit above rolls back with it.
                credit(
                    &mut *conn,
                    &c.user_id,
                    kind.as_str(),
                    *qty,
                    ITEM_CAP,
                    &format!("your pantry holds at most {ITEM_CAP} {}", kind.food().plural),
                )
                .await?;
                Ok(None)
            }
            Job::Eat { kind } => {
                let plural = kind.food().plural;
                // The `short` message deliberately omits need/have.
                debit(
                    &mut *conn,
                    &c.user_id,
                    kind.as_str(),
                    1,
                    plural,
                    Some(&format!("you have no {plural} left")),
                )
                .await?;
                Ok(None)
            }
            Job::Give(g) => give_apply(conn, st, c, g, id, op_date).await,
        }
    }
}

/// Take the goods off the sender. Only the non-zero halves move.
async fn gift_send(conn: &mut SqliteConnection, from: &str, g: &Gift) -> Result<(), Fail> {
    if g.coins > 0 {
        debit(&mut *conn, from, "coins", g.coins, "Poke Coins", None).await?;
    }
    if let Some(k) = g.kind.filter(|_| g.qty > 0) {
        debit(&mut *conn, from, k.as_str(), g.qty, k.food().plural, None).await?;
    }
    Ok(())
}

/// Put them on the recipient. Every refusal here is the one generic string, so
/// the recipient's purse and stacks never leak back to the sender.
async fn gift_receive(
    conn: &mut SqliteConnection,
    to: &str,
    g: &Gift,
    refused: &str,
) -> Result<(), Fail> {
    if g.coins > 0 {
        credit(&mut *conn, to, "coins", g.coins, COIN_CAP, refused).await?;
    }
    if let Some(k) = g.kind.filter(|_| g.qty > 0) {
        credit(&mut *conn, to, k.as_str(), g.qty, ITEM_CAP, refused).await?;
    }
    Ok(())
}

/// give's half of `apply`. The order of the steps is load-bearing; see the
/// comments on each.
async fn give_apply(
    conn: &mut SqliteConnection,
    st: &crate::AppState,
    c: &crate::Caller,
    g: &Gift,
    id: &str,
    op_date: &str,
) -> Result<Option<String>, Fail> {
    let me = c.user_id.as_str();
    // `check` already refused a missing or deactivated target, and a replay never
    // reaches here, so to_id is real.
    let to_id = g.to_id.as_deref().unwrap_or_default();

    // The total daily cap is checked BEFORE the per-pair cap, so the 9th gift of
    // the day reports "you've sent 8..." even when it is also a 4th to the same
    // person. Both counts include the row just inserted.
    let sent_today: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM poke_ledger
         WHERE op = 'give' AND op_date = ?1 AND user_id = ?2",
    )
    .bind(op_date)
    .bind(me)
    .fetch_one(&mut *conn)
    .await?;
    if sent_today > GIFTS_PER_DAY {
        return Err(Fail::refused(
            StatusCode::TOO_MANY_REQUESTS,
            &format!("you've sent {GIFTS_PER_DAY} gifts today, try again tomorrow"),
        ));
    }
    let to_pair: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM poke_ledger
         WHERE op = 'give' AND op_date = ?1 AND user_id = ?2 AND to_user_id = ?3",
    )
    .bind(op_date)
    .bind(me)
    .bind(to_id)
    .fetch_one(&mut *conn)
    .await?;
    if to_pair > GIFTS_PER_PAIR_PER_DAY {
        return Err(Fail::refused(
            StatusCode::TOO_MANY_REQUESTS,
            &format!(
                "you've already sent {} {GIFTS_PER_PAIR_PER_DAY} gifts today",
                g.to_handle
            ),
        ));
    }

    // PRIVACY: a sender who cannot pay is refused BEFORE anything reads the
    // recipient, whatever order the rows are touched in below. Otherwise which
    // 409 came back would tell a broke sender the recipient's balance or today's
    // receipts for free. Coins are checked before food, and a refused gift
    // writes nothing.
    if g.coins > 0 {
        afford(&mut *conn, me, "coins", g.coins, "Poke Coins").await?;
    }
    if let Some(k) = g.kind.filter(|_| g.qty > 0) {
        afford(&mut *conn, me, k.as_str(), g.qty, k.food().plural).await?;
    }

    // PRIVACY: every recipient-side refusal -- daily coin receipts, daily item
    // receipts, a full purse, a full stack -- reads the same, with no digits and
    // no balances in it. The two sums are OR'd into one check, so a gift of coins
    // can be refused because of their ITEM receipts and the sender cannot tell.
    // Both sum over today's gives to them from EVERYONE, this row included.
    let refused = format!("{} can't receive that right now", g.to_handle);
    let coin_sum: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(coins),0) FROM poke_ledger
         WHERE op = 'give' AND op_date = ?1 AND to_user_id = ?2",
    )
    .bind(op_date)
    .bind(to_id)
    .fetch_one(&mut *conn)
    .await?;
    let item_sum: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(qty),0) FROM poke_ledger
         WHERE op = 'give' AND op_date = ?1 AND to_user_id = ?2",
    )
    .bind(op_date)
    .bind(to_id)
    .fetch_one(&mut *conn)
    .await?;
    if coin_sum > RECEIVE_COINS_PER_DAY || item_sum > RECEIVE_ITEMS_PER_DAY {
        return Err(Fail::refused(StatusCode::CONFLICT, &refused));
    }

    // Touch rows in ascending user_id order so A->B and B->A gifts cannot
    // deadlock on Postgres. SQLite holds one database-wide write lock, so there
    // the order is moot -- but the privacy test pins both directions, and the
    // refusal has to read identically either way round.
    if me < to_id {
        gift_send(&mut *conn, me, g).await?;
        gift_receive(&mut *conn, to_id, g, &refused).await?;
    } else {
        gift_receive(&mut *conn, to_id, g, &refused).await?;
        gift_send(&mut *conn, me, g).await?;
    }

    // Pick the delivery channel INSIDE the transaction. A gift that will go live
    // commits already marked delivered, so a drain racing the live send finds
    // nothing and the recipient is never told twice. `_in_lobby` looks only at
    // the room named "lobby", while the live send fans out to every room: someone
    // in a kart room but not the lobby gets no live attempt and falls through to
    // their drain. Preserve that asymmetry.
    if st.rooms.count_where("lobby", |m| m.user_id == to_id).await > 0 {
        let stamp = store_dt(now());
        sqlx::query("UPDATE poke_ledger SET delivered_at = ?1 WHERE id = ?2")
            .bind(&stamp)
            .bind(id)
            .execute(&mut *conn)
            .await?;
        return Ok(Some(stamp));
    }
    Ok(None)
}

/// The committed op for this request id, if any, verified against `want`.
async fn prior(
    pool: &SqlitePool,
    uid: &str,
    rid: &str,
    want: &Want,
) -> Result<Option<Ledger>, Fail> {
    let Some(row) = find_op(pool, uid, rid).await? else {
        return Ok(None);
    };
    if !want.matches(&row) {
        return Err(Fail::refused(StatusCode::CONFLICT, REUSED));
    }
    Ok(Some(row))
}

/// The ops cap and then the job's own work, so `spend` has one place to roll back.
async fn after_insert(
    conn: &mut SqliteConnection,
    st: &crate::AppState,
    c: &crate::Caller,
    job: &Job,
    id: &str,
    op_date: &str,
) -> Result<Option<String>, Fail> {
    // Exact because it runs under the write lock the INSERT took, and it counts
    // the row just inserted. No op filter: a cosmetic or a sell spends it too.
    let ops: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM poke_ledger WHERE user_id = ?1 AND op_date = ?2")
            .bind(&c.user_id)
            .bind(op_date)
            .fetch_one(&mut *conn)
            .await?;
    if ops > MAX_OPS_PER_DAY {
        return Err(Fail::refused(
            StatusCode::TOO_MANY_REQUESTS,
            "that's a lot of pantry activity for one day, try again tomorrow",
        ));
    }
    job.apply(conn, st, c, id, op_date).await
}

/// Run one spend in the journal-first order. Returns the committed ledger row,
/// or the prior one when this request id already ran.
async fn spend(
    st: &crate::AppState,
    c: &crate::Caller,
    rid: &str,
    job: &Job,
) -> Result<(Ledger, bool), Fail> {
    let uid = &c.user_id;
    let want = job.want();
    if let Some(row) = prior(&st.pool, uid, rid, &want).await? {
        // A replay wins over check(), and writes nothing at all.
        return Ok((row, true));
    }
    job.check(uid)?;

    let op_date = job.op_date(today()).to_string();
    let id = uuid::Uuid::new_v4().to_string();
    let created = store_dt(now());
    let mut tx = st.pool.begin().await?;

    // sqlx's `begin()` issues a plain deferred BEGIN, exactly as pysqlite does,
    // so this INSERT is the first write: it takes SQLite's write lock here and
    // claims the request id under UNIQUE(user_id, request_id).
    let inserted = sqlx::query(LEDGER_INSERT)
        .bind(&id)
        .bind(uid)
        .bind(rid)
        .bind(want.op)
        .bind(&op_date)
        .bind(want.kind)
        .bind(want.qty)
        .bind(want.coins)
        .bind(want.to_user_id.as_deref())
        .bind(job.note())
        .bind(&created)
        .execute(&mut *tx)
        .await;
    if let Err(e) = inserted {
        let fail = Fail::from(e);
        let _ = tx.rollback().await;
        // Almost always a concurrent submit of the same request id that won the
        // race: replay it. Anything else is a transient conflict.
        if matches!(fail, Fail::Unique) {
            if let Some(row) = prior(&st.pool, uid, rid, &want).await? {
                return Ok((row, true));
            }
            return Err(Fail::Busy);
        }
        return Err(fail);
    }

    // Bound to a local first: a `match` keeps its scrutinee's temporaries alive
    // for the whole match, and these arms need `tx` and `op_date` back.
    let outcome = after_insert(&mut tx, st, c, job, &id, &op_date).await;
    match outcome {
        Ok(delivered) => {
            tx.commit().await?;
            Ok((
                Ledger {
                    id,
                    op: want.op.to_string(),
                    op_date,
                    kind: want.kind.map(str::to_string),
                    qty: want.qty,
                    coins: want.coins,
                    to_user_id: want.to_user_id,
                    note: job.note().to_string(),
                    created_at: created,
                    delivered_at: delivered,
                },
                false,
            ))
        }
        Err(fail) => {
            // No savepoints: the journal row goes back with everything else, so
            // the same request id can be retried once the caller tops up.
            let _ = tx.rollback().await;
            Err(fail)
        }
    }
}

// --- handlers --------------------------------------------------------------

fn caller(req: &Request) -> crate::Caller {
    req.extensions().get::<crate::Caller>().expect("caller set by middleware").clone()
}

async fn state_response(pool: &SqlitePool, uid: &str) -> Response {
    match pantry_state(pool, uid).await {
        Ok(state) => Json(state).into_response(),
        Err(f) => f.response(),
    }
}

/// Read-only. Creates no rows, claims nothing, marks nothing delivered.
async fn get_pantry(State(st): State<crate::AppState>, req: Request) -> Response {
    let c = caller(&req);
    state_response(&st.pool, &c.user_id).await
}

/// What `claim` did, beyond the state the response re-reads after the commit.
#[derive(Debug, Default)]
struct Claimed {
    granted: i64,
    starter: bool,
    full: bool,
}

/// The granting half, inside the transaction. `Ok(None)` means the purse was
/// already at the cap and the caller must roll back.
async fn grant_claim(
    conn: &mut SqliteConnection,
    uid: &str,
    id: &str,
) -> Result<Option<Claimed>, Fail> {
    let have = qty_of(&mut *conn, uid, "coins").await?.unwrap_or(0);
    let grant = DAILY_COINS.min(COIN_CAP - have);
    if grant <= 0 {
        return Ok(None);
    }
    sqlx::query("UPDATE poke_ledger SET coins = ?1 WHERE id = ?2")
        .bind(grant)
        .bind(id)
        .execute(&mut *conn)
        .await?;
    credit(&mut *conn, uid, "coins", grant, COIN_CAP, "wallet full").await?;

    // The row just inserted is counted, so == 1 means this is the first claim ever.
    let claims: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM poke_ledger WHERE op = 'claim' AND user_id = ?1")
            .bind(uid)
            .fetch_one(&mut *conn)
            .await?;
    let mut starter = false;
    if claims == 1 {
        let (kind, n) = STARTER;
        let snack = credit(&mut *conn, uid, kind, n, ITEM_CAP, "").await;
        match snack {
            Ok(()) => {
                sqlx::query("UPDATE poke_ledger SET kind = ?1, qty = ?2 WHERE id = ?3")
                    .bind(kind)
                    .bind(n)
                    .bind(id)
                    .execute(&mut *conn)
                    .await?;
                starter = true;
            }
            // Already holding a full stack: swallow it. No starter snack, but
            // still a valid claim, and the row keeps kind=NULL/qty=0.
            Err(Fail::Refused(..)) => {}
            Err(f) => return Err(f),
        }
    }
    Ok(Some(Claimed { granted: grant, starter, full: false }))
}

/// +DAILY_COINS once per UTC day, keyed by the reserved request id
/// "claim:YYYY-MM-DD". Lazy: a missed day is simply gone, never banked. Exempt
/// from MAX_OPS_PER_DAY.
async fn do_claim(st: &crate::AppState, uid: &str) -> Result<Claimed, Fail> {
    let day = today();
    let rid = format!("claim:{day}");
    if find_op(&st.pool, uid, &rid).await?.is_some() {
        // No transaction is opened at all.
        return Ok(Claimed::default());
    }

    let id = uuid::Uuid::new_v4().to_string();
    let mut tx = st.pool.begin().await?;
    let inserted = sqlx::query(LEDGER_INSERT)
        .bind(&id)
        .bind(uid)
        .bind(&rid)
        .bind("claim")
        .bind(day.to_string())
        .bind(None::<&str>)
        .bind(0_i64)
        .bind(0_i64)
        .bind(None::<&str>)
        .bind("")
        .bind(store_dt(now()))
        .execute(&mut *tx)
        .await;
    if let Err(e) = inserted {
        let fail = Fail::from(e);
        let _ = tx.rollback().await;
        // Another tab's claim for today won the race: theirs stands.
        if matches!(fail, Fail::Unique) {
            if find_op(&st.pool, uid, &rid).await?.is_some() {
                return Ok(Claimed::default());
            }
            return Err(Fail::Busy);
        }
        return Err(fail);
    }

    // Bound first: these arms need `tx` back, and a match holds its scrutinee's
    // temporaries -- here the future borrowing it -- until the match ends.
    let granted = grant_claim(&mut tx, uid, &id).await;
    match granted {
        Ok(Some(claimed)) => {
            tx.commit().await?;
            Ok(claimed)
        }
        Ok(None) => {
            // A full purse records nothing: the journal row is rolled back, so
            // claimedToday stays false and today's claim stays open for after a
            // spend or a gift.
            let _ = tx.rollback().await;
            Ok(Claimed { granted: 0, starter: false, full: true })
        }
        Err(f) => {
            let _ = tx.rollback().await;
            Err(f)
        }
    }
}

async fn claim(State(st): State<crate::AppState>, req: Request) -> Response {
    // No body is read; the client sends `{}` and anything in it is ignored.
    let c = caller(&req);
    let claimed = match do_claim(&st, &c.user_id).await {
        Ok(v) => v,
        Err(f) => return f.response(),
    };
    match pantry_state(&st.pool, &c.user_id).await {
        Ok(state) => Json(ClaimResponse {
            state,
            op: "claim",
            claimed: claimed.granted > 0,
            granted: claimed.granted,
            starter: claimed.starter,
            full: claimed.full,
        })
        .into_response(),
        Err(f) => f.response(),
    }
}

async fn buy(State(st): State<crate::AppState>, req: Request) -> Response {
    let c = caller(&req);
    let (_, raw) = req.into_parts();
    let bytes = match axum::body::to_bytes(raw, 64 * 1024).await {
        Ok(b) => b,
        Err(_) => return crate::err(StatusCode::BAD_REQUEST, "body too large"),
    };
    let body = match buy_body(&bytes) {
        Ok(b) => b,
        Err(errs) => return err422(errs),
    };

    // A committed buy is priced on ITS day, so a retry that crosses UTC midnight
    // -- when the special rotates, or the season turns -- still REPLAYS instead
    // of computing a different `coins` and failing as a reused id.
    let earlier = match find_op(&st.pool, &c.user_id, &body.request_id).await {
        Ok(p) => p,
        Err(f) => return f.response(),
    };
    let day = earlier
        .filter(|p| p.op == "buy")
        .and_then(|p| NaiveDate::parse_from_str(&p.op_date, "%Y-%m-%d").ok())
        .unwrap_or_else(today);

    let cost = price_of(body.kind.food(), day) * body.qty;
    let job = Job::Buy { kind: body.kind, qty: body.qty, cost, day };
    let (row, replayed) = match spend(&st, &c, &body.request_id, &job).await {
        Ok(v) => v,
        Err(f) => return f.response(),
    };
    match pantry_state(&st.pool, &c.user_id).await {
        // kind/qty/spent come off the COMMITTED row, so a replay echoes the
        // original purchase including the price from its own day.
        Ok(state) => Json(BuyResponse {
            state,
            op: "buy",
            replayed,
            kind: row.kind.unwrap_or_default(),
            qty: row.qty,
            spent: row.coins,
        })
        .into_response(),
        Err(f) => f.response(),
    }
}

async fn eat(State(st): State<crate::AppState>, req: Request) -> Response {
    let c = caller(&req);
    let (_, raw) = req.into_parts();
    let bytes = match axum::body::to_bytes(raw, 64 * 1024).await {
        Ok(b) => b,
        Err(_) => return crate::err(StatusCode::BAD_REQUEST, "body too large"),
    };
    let body = match eat_body(&bytes) {
        Ok(b) => b,
        Err(errs) => return err422(errs),
    };

    let job = Job::Eat { kind: body.kind };
    let (row, replayed) = match spend(&st, &c, &body.request_id, &job).await {
        Ok(v) => v,
        Err(f) => return f.response(),
    };
    // The want-tuple check guarantees a replayed row's kind is this kind, so the
    // effects can be read straight off the catalog. A revive item is eaten by
    // exactly this path; `revives` is just reported.
    let food = body.kind.food();
    match pantry_state(&st.pool, &c.user_id).await {
        Ok(state) => Json(EatResponse {
            state,
            op: "eat",
            replayed,
            kind: row.kind.clone().unwrap_or_default(),
            restore_mins: food.restore_mins,
            revives: food.revives,
            // A replay returns the ORIGINAL time, so a reused id cannot forge a
            // later meal, and eats nothing more.
            at: row.at(),
        })
        .into_response(),
        Err(f) => f.response(),
    }
}

async fn give(State(st): State<crate::AppState>, req: Request) -> Response {
    let c = caller(&req);
    let (_, raw) = req.into_parts();
    let bytes = match axum::body::to_bytes(raw, 64 * 1024).await {
        Ok(b) => b,
        Err(_) => return crate::err(StatusCode::BAD_REQUEST, "body too large"),
    };
    let body = match give_body(&bytes) {
        Ok(b) => b,
        Err(errs) => return err422(errs),
    };

    // Resolved now, case-SENSITIVELY under SQLite's default BINARY collation --
    // no COLLATE NOCASE and no lowercasing. The ledger stores ids, not handles.
    let found = sqlx::query("SELECT id, handle, is_active FROM users WHERE handle = ?1")
        .bind(&body.to_handle)
        .fetch_optional(&st.pool)
        .await;
    let target = match found {
        Ok(t) => t,
        Err(e) => return Fail::from(e).response(),
    };
    let to_id: Option<String> = target.as_ref().map(|r| r.get("id"));
    let to_handle: String = target
        .as_ref()
        .map(|r| r.get("handle"))
        .unwrap_or_else(|| body.to_handle.clone());
    let to_active = target.as_ref().is_some_and(|r| r.get::<i64, _>("is_active") != 0);

    let job = Job::Give(Box::new(Gift {
        coins: body.coins,
        kind: body.kind,
        qty: body.qty,
        note: clean_note(&body.note),
        to_id,
        to_handle: to_handle.clone(),
        to_active,
    }));
    let (row, replayed) = match spend(&st, &c, &body.request_id, &job).await {
        Ok(v) => v,
        Err(f) => return f.response(),
    };

    let live = if replayed {
        // "Send again" after an unconfirmed send must not call a gift that
        // already landed "queued", so report whether it has reached them by now,
        // live or drained. Nothing new is sent.
        i64::from(row.delivered_at.is_some())
    } else if row.delivered_at.is_none() {
        0 // not in the lobby at commit: their drain delivers it
    } else {
        let display = if c.display_name.is_empty() { &c.handle } else { &c.display_name };
        // Exactly these keys: no url, no sessionId, no title, no command.
        let payload = json!({
            "type": "gift",
            "id": &row.id,
            "from": {
                "userId": &c.user_id,
                "handle": &c.handle,
                "displayName": display,
                "avatarUrl": &c.avatar_url,
            },
            "coins": row.coins,
            "kind": &row.kind,
            "qty": row.qty,
            "note": &row.note,
        });
        let to = row.to_user_id.as_deref().unwrap_or_default();
        let n = st.rooms.deliver_to_user(to, payload.to_string()).await as i64;
        if n == 0 {
            // No socket took it: hand it back to their drain. Best-effort -- if
            // this fails too they miss the notification, never the gift.
            let _ = sqlx::query("UPDATE poke_ledger SET delivered_at = NULL WHERE id = ?1")
                .bind(&row.id)
                .execute(&st.pool)
                .await;
        }
        n
    };

    match pantry_state(&st.pool, &c.user_id).await {
        // The SENDER's state, and nothing at all about the recipient's balances.
        // `sent` and `toHandle` come off the committed row and the resolved
        // target, so a replay echoes the ORIGINAL gift.
        Ok(state) => Json(GiveResponse {
            state,
            op: "give",
            replayed,
            to_handle,
            sent: SentGift { coins: row.coins, kind: row.kind, qty: row.qty },
            delivered_live: live,
        })
        .into_response(),
        Err(f) => f.response(),
    }
}

/// Credit coins for a completed quest or achievement, idempotent by requestId.
async fn do_reward(
    st: &crate::AppState,
    uid: &str,
    body: &RewardRequest,
) -> Result<RewardResponse, Fail> {
    let day = today();

    // Catalog validation happens BEFORE the idempotency lookup, so replaying a
    // valid requestId with the wrong `coins` still 422s.
    let expected = match body.kind {
        RewardKind::Quest => quest_reward(&body.quest_id),
        RewardKind::Achievement => ach_reward(&body.quest_id, body.tier),
    };
    let Some(expected) = expected else {
        return Err(Fail::refused(StatusCode::NOT_FOUND, "unknown quest or achievement"));
    };
    if body.coins != expected {
        // The one 422 in this module that is NOT a validation failure: the
        // Python raises HTTPException(422, ...) here, so the body is the bare
        // string envelope and not `err422`'s list. Converting it would diverge.
        return Err(Fail::refused(
            StatusCode::UNPROCESSABLE_ENTITY,
            "reward amount does not match the catalog",
        ));
    }

    // Unlike `prior` there is no want-tuple check here: ANY prior op holding
    // this request id replays, so reward can never 409 on a reused id.
    if find_op(&st.pool, uid, &body.request_id).await?.is_some() {
        return Ok(RewardResponse { ok: true, coins: coins_of(&st.pool, uid).await?, reward: 0 });
    }

    // Counted BEFORE the insert and with `>=`, unlike spend's post-insert `>`:
    // exactly 10 rewards land per UTC day. Reward is exempt from
    // MAX_OPS_PER_DAY, but its rows do spend that shared budget.
    let quests: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM poke_ledger WHERE user_id = ?1 AND op = 'quest' AND op_date = ?2",
    )
    .bind(uid)
    .bind(day.to_string())
    .fetch_one(&st.pool)
    .await?;
    if quests >= QUEST_REWARDS_PER_DAY {
        return Err(Fail::refused(
            StatusCode::TOO_MANY_REQUESTS,
            "too many quest rewards today, try again tomorrow",
        ));
    }

    let mut tx = st.pool.begin().await?;
    let inserted = sqlx::query(LEDGER_INSERT)
        .bind(uuid::Uuid::new_v4().to_string())
        .bind(uid)
        .bind(&body.request_id)
        .bind("quest")
        .bind(day.to_string())
        .bind(None::<&str>)
        .bind(0_i64)
        .bind(body.coins)
        .bind(None::<&str>)
        // The questId goes in `note`.
        .bind(&body.quest_id)
        .bind(store_dt(now()))
        .execute(&mut *tx)
        .await;
    if let Err(e) = inserted {
        let fail = Fail::from(e);
        let _ = tx.rollback().await;
        if matches!(fail, Fail::Unique) {
            if find_op(&st.pool, uid, &body.request_id).await?.is_some() {
                return Ok(RewardResponse {
                    ok: true,
                    coins: coins_of(&st.pool, uid).await?,
                    reward: 0,
                });
            }
            return Err(Fail::Busy);
        }
        return Err(fail);
    }

    // Quest rewards are NOT clamped the way claim is: past the cap this is a 409.
    // Bound first so the borrow of `tx` is over before the rollback needs it.
    let credited = credit(&mut tx, uid, "coins", body.coins, COIN_CAP, "wallet full").await;
    if let Err(f) = credited {
        let _ = tx.rollback().await;
        return Err(f);
    }
    tx.commit().await?;
    Ok(RewardResponse {
        ok: true,
        coins: coins_of(&st.pool, uid).await?,
        reward: body.coins,
    })
}

async fn reward(State(st): State<crate::AppState>, req: Request) -> Response {
    let c = caller(&req);
    let (_, raw) = req.into_parts();
    let bytes = match axum::body::to_bytes(raw, 64 * 1024).await {
        Ok(b) => b,
        Err(_) => return crate::err(StatusCode::BAD_REQUEST, "body too large"),
    };
    let body = match reward_body(&bytes) {
        Ok(b) => b,
        Err(errs) => return err422(errs),
    };
    match do_reward(&st, &c.user_id, &body).await {
        Ok(r) => Json(r).into_response(),
        Err(f) => f.response(),
    }
}

/// Undelivered gifts to me, oldest first, each returned exactly once.
async fn do_drain(pool: &SqlitePool, uid: &str) -> Result<Vec<DrainedGift>, Fail> {
    // Oldest first, the opposite of recentGifts; the tie-break on id makes the
    // order total when two gifts share a timestamp. A gift that went live
    // committed already marked delivered, so this never returns that one too.
    let rows = sqlx::query(
        "SELECT l.id, l.coins, l.kind, l.qty, l.note, l.created_at, u.handle, u.display_name
         FROM poke_ledger l JOIN users u ON u.id = l.user_id
         WHERE l.op = 'give' AND l.to_user_id = ?1 AND l.delivered_at IS NULL
         ORDER BY l.created_at, l.id
         LIMIT ?2",
    )
    .bind(uid)
    .bind(DRAIN_LIMIT)
    .fetch_all(pool)
    .await?;

    // One timestamp for the whole drain.
    let stamp = store_dt(now());
    let mut tx = pool.begin().await?;
    let mut gifts = Vec::new();
    for r in &rows {
        let id: String = r.get("id");
        // The conditional UPDATE is a GUARD, not a convenience: of two drains
        // racing for the same row only one sees delivered_at still NULL, so a
        // row joins the response only when its update landed.
        let res =
            sqlx::query("UPDATE poke_ledger SET delivered_at = ?1 WHERE id = ?2 AND delivered_at IS NULL")
                .bind(&stamp)
                .bind(&id)
                .execute(&mut *tx)
                .await?;
        if res.rows_affected() == 1 {
            gifts.push(DrainedGift { gift: gift_item(r), id });
        }
    }
    tx.commit().await?;
    Ok(gifts)
}

async fn drain(State(st): State<crate::AppState>, req: Request) -> Response {
    // A POST rather than a GET because it marks what it returns as delivered.
    // No body is read; the client sends `{}`.
    let c = caller(&req);
    match do_drain(&st.pool, &c.user_id).await {
        Ok(gifts) => Json(GiftsResponse { gifts }).into_response(),
        Err(f) => f.response(),
    }
}

/// The pantry group. `require_device` is applied by main.rs to the whole guarded
/// router, so there is no auth layer here.
///
/// All seven routes or none: a server missing this feature answers a bare 404
/// "Not Found", which the client reads as "this Arena doesn't have the store
/// yet". Registering only some of them would break that signal.
pub fn routes() -> Router<crate::AppState> {
    Router::new()
        .route("/v1/pantry", get(get_pantry))
        .route("/v1/pantry/claim", post(claim))
        .route("/v1/pantry/buy", post(buy))
        .route("/v1/pantry/eat", post(eat))
        .route("/v1/pantry/give", post(give))
        .route("/v1/pantry/reward", post(reward))
        .route("/v1/pantry/gifts/drain", post(drain))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).expect("a real calendar date")
    }

    fn food(kind: &str) -> &'static Food {
        CATALOG.iter().find(|f| f.kind == kind).expect("a catalog kind")
    }

    /// The `type` strings of a refusal, in the order Pydantic reports them.
    fn kinds(errs: &[Verr]) -> Vec<&'static str> {
        errs.iter().map(|e| e.kind).collect()
    }

    /// A buy body whose requestId is `rid` and nothing else wrong.
    fn rid_errs(rid: &str) -> Vec<&'static str> {
        let body = format!(r#"{{"requestId":{},"kind":"berry","qty":1}}"#, json!(rid));
        buy_body(body.as_bytes()).err().map(|e| kinds(&e)).unwrap_or_default()
    }

    /// A give body whose toHandle is `h` and nothing else wrong.
    fn handle_errs(h: &str) -> Vec<&'static str> {
        let body = format!(
            r#"{{"requestId":"rid-g-0000000000000000","toHandle":{},"coins":1}}"#,
            json!(h)
        );
        give_body(body.as_bytes()).err().map(|e| kinds(&e)).unwrap_or_default()
    }

    #[test]
    fn the_catalog_order_is_the_python_order() {
        let kinds: Vec<&str> = CATALOG.iter().map(|f| f.kind).collect();
        assert_eq!(
            kinds,
            [
                "berry", "bread", "riceball", "coffee", "bento", "noodles", "hotpot", "tonic",
                "elixir", "strawberry", "dango", "omelette", "watermelon", "shavedice", "curry",
                "apple", "sweetpotato", "pumpkinstew", "chestnuts", "cocoa", "oden",
            ]
        );
        let prices: Vec<i64> = CATALOG.iter().map(|f| f.price).collect();
        assert_eq!(prices, [1, 1, 2, 2, 3, 3, 4, 5, 7, 1, 2, 3, 1, 2, 3, 1, 2, 3, 1, 2, 3]);
        let restore: Vec<i64> = CATALOG.iter().map(|f| f.restore_mins).collect();
        assert_eq!(
            restore,
            [20, 20, 45, 45, 120, 120, 180, 0, 60, 25, 55, 135, 25, 55, 135, 25, 55, 135, 25, 55,
             135]
        );
        let revive: Vec<&str> = CATALOG.iter().filter(|f| f.revives).map(|f| f.kind).collect();
        assert_eq!(revive, ["tonic", "elixir"]);
        // The dango's plural really is its singular, and the chestnuts' moves the 's'.
        assert_eq!(food("dango").plural, "Hanami Dango");
        assert_eq!(food("chestnuts").plural, "Bags of Chestnuts");
        // The bare coffee emoji, with no variation selector.
        assert_eq!(food("coffee").emoji, "\u{2615}");
    }

    #[test]
    fn season_boundaries_follow_the_utc_month() {
        for (day, season) in [
            (d(2027, 1, 1), "winter"),
            (d(2027, 2, 28), "winter"),
            (d(2027, 3, 1), "spring"),
            (d(2026, 5, 31), "spring"),
            (d(2026, 6, 1), "summer"),
            (d(2026, 8, 31), "summer"),
            (d(2026, 9, 1), "fall"),
            (d(2026, 11, 30), "fall"),
            (d(2026, 12, 1), "winter"),
        ] {
            assert_eq!(season_of(day), season, "{day}");
        }
    }

    #[test]
    fn in_stock_follows_the_season() {
        let fall = d(2026, 9, 28);
        assert!(in_stock(food("berry"), fall), "year-round food is always in stock");
        assert!(in_stock(food("apple"), fall));
        assert!(!in_stock(food("strawberry"), fall));
        assert!(!in_stock(food("cocoa"), fall));
        assert!(in_stock(food("cocoa"), d(2026, 12, 1)));
    }

    #[test]
    fn the_special_matches_the_python_hash() {
        // Verified against the Python: these four must not drift.
        assert_eq!(special_of(d(2026, 9, 28)), Some("hotpot"));
        assert_eq!(special_of(d(2026, 9, 29)), Some("pumpkinstew"));
        assert_eq!(special_of(d(2026, 11, 30)), Some("pumpkinstew"));
        assert_eq!(special_of(d(2026, 12, 1)), Some("cocoa"));
    }

    #[test]
    fn the_special_is_deterministic_and_in_stock() {
        let start = d(2026, 1, 1);
        let mut seen: Vec<&str> = Vec::new();
        for i in 0..400 {
            let day = start + Duration::days(i);
            let special = special_of(day).expect("this catalog always has a pool");
            assert_eq!(special_of(day), Some(special), "{day} must be stable");
            let f = food(special);
            assert!(in_stock(f, day), "{special} on {day}");
            assert!(f.price >= SPECIAL_MIN_PRICE);
            assert_eq!(price_of(f, day), f.price - SPECIAL_DISCOUNT);
            assert!(price_of(f, day) >= 1, "a discounted price is never below 1");
            for other in CATALOG.iter().filter(|o| o.kind != special) {
                assert_eq!(price_of(other, day), other.price, "{} on {day}", other.kind);
            }
            if !seen.contains(&special) {
                seen.push(special);
            }
        }
        assert!(seen.len() >= 8, "it really rotates: {seen:?}");
        // Seasonal food takes its turn, not just the year-round stock.
        assert!(seen.iter().any(|k| [
            "dango", "omelette", "shavedice", "curry", "sweetpotato", "pumpkinstew", "cocoa",
            "oden"
        ]
        .contains(k)));
    }

    #[test]
    fn todays_special_is_a_coin_off_and_nothing_else_is() {
        let day = d(2026, 9, 28);
        assert_eq!((food("hotpot").price, price_of(food("hotpot"), day)), (4, 3));
        assert_eq!(price_of(food("bento"), day), 3);
        assert_eq!(price_of(food("berry"), day), 1);
    }

    #[test]
    fn timestamps_use_pythons_shapes() {
        // What goes into SQLite: a space, six microsecond digits, no offset.
        let dt = read_dt("2026-09-28 10:02:04.123456").expect("parses");
        assert_eq!(store_dt(dt), "2026-09-28 10:02:04.123456");
        // What goes on the wire: isoformat, never to_rfc3339's "Z".
        assert_eq!(iso(dt), "2026-09-28T10:02:04.123456+00:00");
        // A whole second emits NO fractional digits at all.
        let whole = read_dt("2026-09-29 00:00:00.000000").expect("parses");
        assert_eq!(iso(whole), "2026-09-29T00:00:00+00:00");
        assert_eq!(next_claim_at(d(2026, 9, 28)), "2026-09-29T00:00:00+00:00");
        assert_eq!(next_claim_at(d(2026, 9, 29)), "2026-09-30T00:00:00+00:00");
        // Tolerant of the 'T' form and of a missing fraction.
        assert!(read_dt("2026-09-28T10:02:04").is_some());
        assert!(read_dt("not a time").is_none());
    }

    #[test]
    fn request_ids_are_16_to_64_safe_characters() {
        assert!(rid_errs("rid-buy-0000000000000000").is_empty());
        assert!(rid_errs(&"a".repeat(16)).is_empty());
        assert!(rid_errs(&"a".repeat(64)).is_empty());
        assert_eq!(rid_errs("tooshort"), ["string_too_short"]);
        assert_eq!(rid_errs(&"x".repeat(65)), ["string_too_long"]);
        assert_eq!(rid_errs("has a space 0123456789"), ["string_pattern_mismatch"]);
        // A colon is what reserves the daily claim key for the server.
        assert_eq!(rid_errs("colon:0123456789abcdef"), ["string_pattern_mismatch"]);
    }

    /// The measured divergence this module was fixed for: the 422 is Pydantic's
    /// list of {loc, msg, type}, not a one-line string.
    #[test]
    fn a_refused_request_id_answers_pydantics_own_shape() {
        let errs = buy_body(br#"{"requestId":"tooshort","kind":"berry","qty":1}"#)
            .expect_err("too short");
        assert_eq!(
            serde_json::to_string(&Errors { detail: errs }).expect("serialises"),
            r#"{"detail":[{"loc":["body","requestId"],"msg":"String should have at least 16 characters","type":"string_too_short"}]}"#
        );
        let bad_pattern = buy_body(br#"{"requestId":"colon:0123456789abcdef","kind":"berry"}"#)
            .expect_err("a colon");
        assert_eq!(bad_pattern[0].msg, "String should match pattern '^[A-Za-z0-9_-]+$'");
    }

    #[test]
    fn handles_are_checked_before_the_database_is() {
        assert!(handle_errs("gary").is_empty());
        assert!(handle_errs("a").is_empty());
        assert_eq!(handle_errs(""), ["string_too_short"]);
        assert_eq!(handle_errs("../x"), ["string_pattern_mismatch"]);
        assert_eq!(handle_errs(&"g".repeat(65)), ["string_too_long"]);
        // min_length is 1 here, so Pydantic's message is singular.
        let errs = give_body(br#"{"requestId":"rid-g-0000000000000000","toHandle":"","coins":1}"#)
            .expect_err("an empty handle");
        assert_eq!(errs[0].msg, "String should have at least 1 character");
    }

    #[test]
    fn reward_ids_are_a_different_namespace() {
        assert!(reward_id_ok("quest:d_prompts_10:2026-09-29"));
        assert!(reward_id_ok("ach:a_first_prompt:bronze"));
        // Colons are REQUIRED here, which the other endpoints forbid.
        assert!(!reward_id_ok("bad-no-colons"));
        assert!(!reward_id_ok("quest:d_prompts_10"));
        assert!(!reward_id_ok("quest::2026-09-29"));
        assert!(!reward_id_ok("quest:d_prompts_10:"));
        assert!(!reward_id_ok("other:d_active:x"));
        assert!(!reward_id_ok("quest:D_ACTIVE:x"));
        // The length is max_length's job, not the pattern's: Pydantic checks it
        // first, so an over-long id that still MATCHES is a length error.
        let long = format!("quest:d_active:{}", "x".repeat(REWARD_ID_MAX));
        assert!(reward_id_ok(&long));
        let body = format!(
            r#"{{"requestId":{},"kind":"quest","questId":"d_active","coins":1}}"#,
            json!(long)
        );
        let errs = reward_body(body.as_bytes()).expect_err("too long");
        assert_eq!(kinds(&errs), ["string_too_long"]);
        assert_eq!(errs[0].msg, format!("String should have at most {REWARD_ID_MAX} characters"));
    }

    #[test]
    fn the_reward_catalog_matches_the_python() {
        assert_eq!(quest_reward("d_prompts_10"), Some(2));
        assert_eq!(quest_reward("w_active_7"), Some(8));
        assert_eq!(quest_reward("d_nonexistent"), None);
        assert_eq!(ach_reward("a_first_prompt", Some(Tier::Bronze)), Some(2));
        assert_eq!(ach_reward("a_streak", Some(Tier::Gold)), Some(15));
        // A missing tier is a 404, not a 422.
        assert_eq!(ach_reward("a_first_prompt", Some(Tier::Silver)), None);
        assert_eq!(ach_reward("a_polyglot", Some(Tier::Silver)), Some(5));
        assert_eq!(ach_reward("a_polyglot", Some(Tier::Gold)), None);
        // And so is an achievement with no tier at all: Python does tiers.get(None).
        assert_eq!(ach_reward("a_prompts", None), None);
        assert_eq!(ach_reward("a_unknown", Some(Tier::Bronze)), None);
    }

    #[test]
    fn notes_lose_their_unprintables_and_collapse() {
        assert_eq!(clean_note("  for your\u{7} sleepy \n  Voltkit "), "for your sleepy Voltkit");
        assert_eq!(clean_note(""), "");
        assert_eq!(clean_note("   "), "");
        // A tab is non-printable in Python too, so it is DROPPED, not turned
        // into a space: "a\tb" collapses to "ab" on both backends.
        assert_eq!(clean_note("a\tb"), "ab");
        // Non-space Unicode blanks go the way Python's isprintable sends them.
        assert_eq!(clean_note("a\u{a0}b"), "ab");
        assert_eq!(clean_note(&"x".repeat(200)).chars().count(), NOTE_MAX);
    }

    #[test]
    fn a_gift_needs_coins_or_food() {
        assert_eq!(give_shape(1, false, 0), Ok(()));
        assert_eq!(give_shape(0, true, 1), Ok(()));
        assert_eq!(give_shape(2, true, 3), Ok(()));
        assert_eq!(give_shape(1, false, 1), Err("kind is required when qty > 0"));
        assert_eq!(give_shape(1, true, 0), Err("qty is required with kind"));
        assert_eq!(give_shape(0, false, 0), Err("a gift needs coins or food"));
    }

    #[test]
    fn bodies_refuse_what_pydantic_refuses() {
        let good = r#"{"requestId":"rid-v-0000000000000000","kind":"berry","qty":1}"#;
        assert!(buy_body(good.as_bytes()).is_ok());
        // qty defaults to 1.
        let bare = r#"{"requestId":"rid-v-0000000000000000","kind":"berry"}"#;
        assert_eq!(buy_body(bare.as_bytes()).expect("parses").qty, 1);
        for (bad, want) in [
            (r#"{"requestId":"rid-v-0000000000000000","kind":"pizza","qty":1}"#, "literal_error"),
            (r#"{"requestId":"rid-v-0000000000000000","kind":"berry","qty":true}"#, "int_type"),
            (r#"{"requestId":"rid-v-0000000000000000","kind":"berry","qty":"2"}"#, "int_type"),
            (r#"{"requestId":"rid-v-0000000000000000","kind":"berry","qty":1.5}"#, "int_type"),
            (
                r#"{"requestId":"rid-v-0000000000000000","kind":"berry","qty":0}"#,
                "greater_than_equal",
            ),
            (r#"{"requestId":"rid-v-0000000000000000","kind":"berry","qty":6}"#, "less_than_equal"),
            (
                r#"{"requestId":"rid-v-0000000000000000","kind":"berry","sessionId":"x"}"#,
                "extra_forbidden",
            ),
        ] {
            let errs = buy_body(bad.as_bytes()).expect_err("refused");
            assert_eq!(kinds(&errs), [want], "{bad}");
        }
        // An unknown kind and a stray field are refused on every body shape.
        assert_eq!(
            kinds(
                &eat_body(br#"{"requestId":"rid-e-0000000000000000","kind":"berry","sessionId":"x"}"#)
                    .expect_err("a stray field")
            ),
            ["extra_forbidden"]
        );
        assert_eq!(
            kinds(
                &give_body(
                    br#"{"requestId":"rid-g-0000000000000000","toHandle":"gary","kind":"pizza","qty":1}"#
                )
                .expect_err("an unknown kind")
            ),
            ["literal_error"]
        );
        // An explicit null kind is a coins-only gift, not an error.
        let g = give_body(
            br#"{"requestId":"rid-g-0000000000000000","toHandle":"gary","coins":1,"kind":null}"#,
        )
        .expect("parses");
        assert!(g.kind.is_none() && g.qty == 0 && g.note.is_empty());
        assert_eq!(
            kinds(
                &reward_body(
                    br#"{"requestId":"quest:d_active:x","kind":"quest","questId":"d_active","coins":1,"sessionId":"s"}"#
                )
                .expect_err("a stray field")
            ),
            ["extra_forbidden"]
        );
        let r = reward_body(
            br#"{"requestId":"ach:a_first_prompt:bronze","kind":"achievement","questId":"a_first_prompt","tier":"bronze","coins":2}"#,
        )
        .expect("parses");
        assert_eq!((r.kind, r.tier), (RewardKind::Achievement, Some(Tier::Bronze)));
    }

    #[test]
    fn a_missing_field_is_pydantics_missing() {
        // Both fields at once: Pydantic collects every field error, it does not
        // stop at the first.
        let errs = eat_body(b"{}").expect_err("an empty object");
        assert_eq!(kinds(&errs), ["missing", "missing"]);
        assert_eq!(errs[0].loc, vec![json!("body"), json!("requestId")]);
        assert_eq!(errs[0].msg, "Field required");
        assert_eq!(errs[1].loc, vec![json!("body"), json!("kind")]);
        // `coins` is the only required int, so it is the only "missing" int.
        assert_eq!(
            kinds(
                &reward_body(br#"{"requestId":"quest:d_active:x","kind":"quest","questId":"d"}"#)
                    .expect_err("no coins")
            ),
            ["missing"]
        );
        // A string field given a null is a type error, not a missing one.
        assert_eq!(
            kinds(&eat_body(br#"{"requestId":null,"kind":"berry"}"#).expect_err("null id")),
            ["string_type"]
        );
    }

    #[test]
    fn extra_keys_are_reported_after_the_declared_fields() {
        let errs = buy_body(br#"{"requestId":"tooshort","kind":"berry","sessionId":"x"}"#)
            .expect_err("refused");
        assert_eq!(kinds(&errs), ["string_too_short", "extra_forbidden"]);
        assert_eq!(errs[1].loc, vec![json!("body"), json!("sessionId")]);
        assert_eq!(errs[1].msg, "Extra inputs are not permitted");
    }

    #[test]
    fn the_whole_body_can_fail_on_its_own() {
        // An empty body is a missing body field, not a decode error.
        let empty = body_object(b"").expect_err("an empty body");
        assert_eq!(kinds(&empty), ["missing"]);
        assert_eq!(empty[0].loc, vec![json!("body")]);
        assert_eq!(kinds(&body_object(b"{").expect_err("truncated")), ["json_invalid"]);
        assert_eq!(
            kinds(&body_object(b"[]").expect_err("not an object")),
            ["model_attributes_type"]
        );
    }

    #[test]
    fn literal_messages_list_every_value_the_way_pydantic_does() {
        assert_eq!(literal_expected(&REWARD_KINDS), "'quest' or 'achievement'");
        assert_eq!(literal_expected(&TIERS), "'bronze', 'silver' or 'gold'");
        assert_eq!(literal_expected(&["only"]), "'only'");
        let kind = food_kinds();
        assert_eq!(kind.len(), 21);
        let msg = format!("Input should be {}", literal_expected(&kind));
        assert!(msg.starts_with("Input should be 'berry', 'bread', 'riceball', "), "{msg}");
        assert!(msg.ends_with("'chestnuts', 'cocoa' or 'oden'"), "{msg}");
        let errs = eat_body(br#"{"requestId":"rid-e-0000000000000000","kind":"pizza"}"#)
            .expect_err("an unknown kind");
        assert_eq!(errs[0].loc, vec![json!("body"), json!("kind")]);
        assert_eq!(errs[0].msg, msg);
        // A number where a Literal is expected is the same error.
        assert_eq!(
            kinds(&eat_body(br#"{"requestId":"rid-e-0000000000000000","kind":7}"#).expect_err("7")),
            ["literal_error"]
        );
        assert_eq!(
            reward_body(
                br#"{"requestId":"quest:d_active:x","kind":"quest","questId":"d","tier":"platinum","coins":1}"#
            )
            .expect_err("no such tier")[0]
                .msg,
            "Input should be 'bronze', 'silver' or 'gold'"
        );
    }

    #[test]
    fn the_gifts_cross_field_rules_are_one_body_level_value_error() {
        let base = r#""requestId":"rid-g-0000000000000000","toHandle":"gary""#;
        for (extra, msg) in [
            (r#""coins":1,"qty":1"#, "Value error, kind is required when qty > 0"),
            (r#""coins":1,"kind":"berry""#, "Value error, qty is required with kind"),
            (r#""coins":0"#, "Value error, a gift needs coins or food"),
        ] {
            let body = format!("{{{base},{extra}}}");
            let errs = give_body(body.as_bytes()).expect_err("refused");
            assert_eq!(kinds(&errs), ["value_error"], "{body}");
            assert_eq!(errs[0].loc, vec![json!("body")], "{body}");
            assert_eq!(errs[0].msg, msg);
        }
        // A field error wins: the model validator never runs, so a gift with
        // nothing in it AND a bad id reports only the id.
        let errs = give_body(br#"{"requestId":"short","toHandle":"gary"}"#).expect_err("refused");
        assert_eq!(kinds(&errs), ["string_too_short"]);
    }

    #[test]
    fn the_give_body_keeps_every_field_constraint() {
        let base = r#""requestId":"rid-g-0000000000000000","toHandle":"gary""#;
        for (extra, want) in [
            (r#""coins":1"#, None),
            (r#""coins":0,"kind":"berry","qty":1"#, None),
            (r#""coins":6"#, Some("less_than_equal")),
            (r#""coins":-1"#, Some("greater_than_equal")),
            (r#""coins":true"#, Some("int_type")),
            (r#""coins":1,"kind":"berry","qty":4"#, Some("less_than_equal")),
            (r#""coins":1,"kind":"pizza","qty":1"#, Some("literal_error")),
            (r#""coins":1,"note":7"#, Some("string_type")),
        ] {
            let body = format!("{{{base},{extra}}}");
            match want {
                None => assert!(give_body(body.as_bytes()).is_ok(), "{body}"),
                Some(w) => {
                    let errs = give_body(body.as_bytes()).expect_err("refused");
                    assert_eq!(kinds(&errs), [w], "{body}");
                }
            }
        }
        // max_length runs BEFORE the note cleaner, so an 81-char note is a 422
        // and is never quietly truncated.
        let long = format!(r#"{{{base},"coins":1,"note":{}}}"#, json!("x".repeat(NOTE_MAX + 1)));
        let errs = give_body(long.as_bytes()).expect_err("an over-long note");
        assert_eq!(kinds(&errs), ["string_too_long"]);
        assert_eq!(errs[0].msg, format!("String should have at most {NOTE_MAX} characters"));
        let ok = format!(r#"{{{base},"coins":1,"note":{}}}"#, json!("x".repeat(NOTE_MAX)));
        assert!(give_body(ok.as_bytes()).is_ok());
    }

    #[test]
    fn the_reward_body_keeps_its_own_constraints() {
        let good = r#"{"requestId":"quest:d_active:x","kind":"quest","questId":"d_active","coins":1}"#;
        let parsed = reward_body(good.as_bytes()).expect("parses");
        assert_eq!((parsed.kind, parsed.tier, parsed.coins), (RewardKind::Quest, None, 1));
        for (bad, want) in [
            (
                r#"{"requestId":"bad-no-colons","kind":"quest","questId":"d_active","coins":1}"#,
                "string_pattern_mismatch",
            ),
            (
                r#"{"requestId":"quest:d_active:x","kind":"daily","questId":"d_active","coins":1}"#,
                "literal_error",
            ),
            (
                r#"{"requestId":"quest:d_active:x","kind":"quest","questId":"d_active","coins":0}"#,
                "greater_than_equal",
            ),
            (
                r#"{"requestId":"quest:d_active:x","kind":"quest","questId":"d_active","coins":16}"#,
                "less_than_equal",
            ),
        ] {
            let errs = reward_body(bad.as_bytes()).expect_err("refused");
            assert_eq!(kinds(&errs), [want], "{bad}");
        }
        let long = format!(
            r#"{{"requestId":"quest:d_active:x","kind":"quest","questId":{},"coins":1}}"#,
            json!("q".repeat(QUEST_ID_MAX + 1))
        );
        let errs = reward_body(long.as_bytes()).expect_err("an over-long questId");
        assert_eq!(kinds(&errs), ["string_too_long"]);
        assert_eq!(errs[0].msg, format!("String should have at most {QUEST_ID_MAX} characters"));
    }

    #[test]
    fn a_replay_must_match_the_whole_tuple() {
        let row = Ledger {
            id: "row".into(),
            op: "buy".into(),
            op_date: "2026-09-28".into(),
            kind: Some("riceball".into()),
            qty: 1,
            coins: 2,
            to_user_id: None,
            note: String::new(),
            created_at: "2026-09-28 10:02:04.123456".into(),
            delivered_at: None,
        };
        let same =
            Want { op: "buy", kind: Some("riceball"), qty: 1, coins: 2, to_user_id: None };
        assert!(same.matches(&row));
        assert_eq!(row.at(), "2026-09-28T10:02:04.123456+00:00");
        for other in [
            Want { op: "eat", kind: Some("riceball"), qty: 1, coins: 2, to_user_id: None },
            Want { op: "buy", kind: Some("berry"), qty: 1, coins: 2, to_user_id: None },
            Want { op: "buy", kind: Some("riceball"), qty: 2, coins: 2, to_user_id: None },
            Want { op: "buy", kind: Some("riceball"), qty: 1, coins: 4, to_user_id: None },
            Want {
                op: "buy",
                kind: Some("riceball"),
                qty: 1,
                coins: 2,
                to_user_id: Some("gary".into()),
            },
        ] {
            assert!(!other.matches(&row));
        }
    }

    #[test]
    fn the_state_keeps_pythons_key_order_and_only_food_in_items() {
        let state = PantryState {
            coins: 5,
            coin_cap: COIN_CAP,
            // A coins row and a cosmetic row must not leak into `items`.
            items: Items(CATALOG.iter().map(|f| (f.kind, 0)).collect()),
            item_cap: ITEM_CAP,
            catalog: Vec::new(),
            claim: ClaimInfo {
                claimed_today: false,
                claimable: true,
                amount: DAILY_COINS,
                today: "2026-09-28".into(),
                next_claim_at: next_claim_at(d(2026, 9, 28)),
            },
            limits: PantryLimits {
                buy_max_qty: BUY_MAX_QTY,
                gift_max_coins: GIFT_MAX_COINS,
                gift_max_qty: GIFT_MAX_QTY,
                gifts_left_today: GIFTS_PER_DAY,
            },
            recent_gifts: Vec::new(),
            season: "fall",
            special: Some("hotpot"),
        };
        let claim = ClaimResponse {
            state,
            op: "claim",
            claimed: true,
            granted: 5,
            starter: true,
            full: false,
        };
        let json = serde_json::to_string(&claim).expect("serialises");
        // Field declaration order, not alphabetical: the JS client talks to both
        // backends, so this is the Pydantic order.
        assert!(json.starts_with(r#"{"coins":5,"coinCap":30,"items":{"berry":0,"bread":0,"#), "{json}");
        assert!(json.contains(r#""oden":0},"itemCap":10,"catalog":[],"claim":{"claimedToday":false"#));
        assert!(json.ends_with(
            r#""season":"fall","special":"hotpot","op":"claim","claimed":true,"granted":5,"starter":true,"full":false}"#
        ), "{json}");
        // 21 food keys and nothing else: `"<key>":0` appears only inside `items`.
        assert_eq!(json.matches(r#"":0"#).count(), 21);
    }

    #[test]
    fn a_drained_gift_puts_its_id_last() {
        let drained = DrainedGift {
            gift: GiftItem {
                from_handle: "ash".into(),
                from_name: "Ash".into(),
                coins: 1,
                kind: None,
                qty: 0,
                note: String::new(),
                at: "2026-09-28T10:02:04.123456+00:00".into(),
            },
            id: "row-1".into(),
        };
        assert_eq!(
            serde_json::to_string(&GiftsResponse { gifts: vec![drained] }).expect("serialises"),
            r#"{"gifts":[{"fromHandle":"ash","fromName":"Ash","coins":1,"kind":null,"qty":0,"note":"","at":"2026-09-28T10:02:04.123456+00:00","id":"row-1"}]}"#
        );
        assert_eq!(
            serde_json::to_string(&GiftsResponse { gifts: Vec::new() }).expect("serialises"),
            r#"{"gifts":[]}"#
        );
    }

    #[test]
    fn a_full_purse_grants_nothing_and_a_partial_one_grants_the_difference() {
        // The arithmetic claim leans on: lazy, never banked, and clamped rather
        // than refused.
        for (have, grant) in [(0, 5), (25, 5), (26, 4), (28, 2), (30, 0)] {
            assert_eq!(DAILY_COINS.min(COIN_CAP - have), grant, "holding {have}");
        }
    }

    #[test]
    fn the_out_of_season_refusal_names_the_plural() {
        let job = Job::Buy {
            kind: Kind(CATALOG.iter().position(|f| f.kind == "strawberry").expect("in catalog")),
            qty: 1,
            cost: 1,
            day: d(2026, 9, 28),
        };
        match job.check("me") {
            Err(Fail::Refused(code, msg)) => {
                assert_eq!(code, StatusCode::CONFLICT);
                assert_eq!(msg, "Strawberries are out of season");
            }
            other => panic!("expected a 409, got {other:?}"),
        }
        // In season it passes, and eating is never gated on the season at all.
        let apple = Kind(CATALOG.iter().position(|f| f.kind == "apple").expect("in catalog"));
        assert!(Job::Buy { kind: apple, qty: 1, cost: 1, day: d(2026, 9, 28) }
            .check("me")
            .is_ok());
        let straw =
            Kind(CATALOG.iter().position(|f| f.kind == "strawberry").expect("in catalog"));
        assert!(Job::Eat { kind: straw }.check("me").is_ok());
    }

    #[test]
    fn give_refuses_a_missing_target_before_a_self_gift() {
        let gift = |to_id: Option<&str>, active: bool| {
            Job::Give(Box::new(Gift {
                coins: 1,
                kind: None,
                qty: 0,
                note: String::new(),
                to_id: to_id.map(str::to_string),
                to_handle: "gary".into(),
                to_active: active,
            }))
        };
        for job in [gift(None, true), gift(Some("gary-id"), false)] {
            match job.check("me") {
                // Indistinguishable by design: a deactivated account is a 404.
                Err(Fail::Refused(code, msg)) => {
                    assert_eq!(code, StatusCode::NOT_FOUND);
                    assert_eq!(msg, "no such person");
                }
                other => panic!("expected a 404, got {other:?}"),
            }
        }
        match gift(Some("me"), true).check("me") {
            Err(Fail::Refused(code, msg)) => {
                assert_eq!(code, StatusCode::BAD_REQUEST);
                assert_eq!(msg, "you can't give to yourself");
            }
            other => panic!("expected a 400, got {other:?}"),
        }
        assert!(gift(Some("gary-id"), true).check("me").is_ok());
    }

    #[test]
    fn a_buys_op_date_is_its_own_day() {
        let day = d(2026, 9, 28);
        let kind = Kind(0);
        // A buy is priced and counted on its own day, not today.
        assert_eq!(Job::Buy { kind, qty: 1, cost: 1, day }.op_date(d(2026, 9, 29)), day);
        assert_eq!(Job::Eat { kind }.op_date(d(2026, 9, 29)), d(2026, 9, 29));
    }
}
