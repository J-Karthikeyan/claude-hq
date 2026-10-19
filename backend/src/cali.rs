//! California Burrito taco Tuesdays, ported from `app/tacos.py`.
//!
//! The deal is **buy 1 get 1, pooled across the whole table**, so the price is
//! a property of the *order*, not of any one diner:
//!
//!     paid = ceil(TT / 2)
//!
//! and it is computed here, never submitted. Same rule as the XP board: a
//! client sends what it saw (who ate what), the server decides what it means.
//! The receipt stored on the order row is what every later read reports -- the
//! board never re-derives the table totals from the diner rows.
//!
//! The board ranks by **Tuesdays attended**, with TT as the tiebreak: turning
//! up is the score, appetite only settles ties. The rest of the menu is
//! recorded per diner and reported back, but never priced and never scored.
//!
//! Two parity notes that are easy to get wrong in Rust, both of them visible on
//! the wire:
//!
//!   * `round(x, 2)` is decimal rounding of the exact binary value. Scaling by
//!     100 and rounding back disagrees with CPython on 1330 of the ~2.0M
//!     (tacos, people) pairs this board can produce, so `round2` formats to two
//!     places and re-parses, which agrees on all of them.
//!   * `serde_json`'s map is a BTreeMap, i.e. alphabetical, but Python emits
//!     `items` in menu order. `Items` therefore keeps menu *indexes* and has a
//!     hand-written `Serialize` -- for the response and for the stored JSON
//!     column, which the Python backend also reads.
//!
//! One approximation is deliberate: Python's `str.casefold()` and
//! `str.isprintable()` need Unicode tables std does not carry, so `casefold`
//! is `to_lowercase` and `printable` covers Cc, Zs/Zl/Zp and the common Cf
//! code points. Both differ from CPython only on exotic input.

use axum::{
    extract::{Query, Request, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use chrono::{DateTime, Datelike, NaiveDate, NaiveDateTime, NaiveTime, SecondsFormat, Timelike, Utc};
use serde::ser::{SerializeMap, Serializer};
use serde::Serialize;
use serde_json::{json, Value};
use sqlx::{Row, SqlitePool};
use std::collections::{HashMap, HashSet};

/// menu key -> display name, in display order. Mirrors `tacos.CALI_MENU`.
const CALI_MENU: [(&str, &str); 11] = [
    ("burrito", "Burrito"),
    ("ricebowl", "Rice Bowl"),
    ("saladbowl", "Salad Bowl"),
    ("quesadilla", "Quesadilla"),
    ("nachos", "Nachos"),
    ("tostada", "Tostada"),
    ("chips", "Chips & Salsa"),
    ("guac", "Guacamole"),
    ("churros", "Churros"),
    ("soda", "Soda"),
    ("icedtea", "Iced Tea"),
];

/// The four mild/wild x hard/soft variants, in schema-field order.
const COUNT_KEYS: [&str; 4] = ["mildHard", "mildSoft", "wildHard", "wildSoft"];

/// Every key a favourite can be, in tie-break order: the taco variants first,
/// then the menu in display order. `favorite` walks it with a strict `>`, so a
/// tie goes to the earlier key and a variant always beats a menu item.
const FAVORITE_ORDER: [&str; 15] = [
    "mildHard", "mildSoft", "wildHard", "wildSoft", "burrito", "ricebowl", "saladbowl",
    "quesadilla", "nachos", "tostada", "chips", "guac", "churros", "soda", "icedtea",
];

const DINER_NAME_MAX: usize = 40;
const NOTE_MAX: usize = 80;
const MAX_DINERS: usize = 20;
const MAX_PER_VARIANT: i64 = 50;
const MAX_PER_ITEM: i64 = 20;
const RECENT_ORDERS: i64 = 50;
const REQUEST_ID_MIN: usize = 16;
const REQUEST_ID_MAX: usize = 64;
const HANDLE_MAX: usize = 64;
const TOKEN_PATTERN: &str = "^[A-Za-z0-9_-]+$";
const BUSY: &str = "busy, try again";

/// The XP board's windows plus the whole previous calendar month, so last
/// season's champion can be crowned after the 1st. Derived from
/// `service::WINDOWS` rather than copied, because "lastseason" must not leak
/// back the other way.
fn cali_windows() -> Vec<&'static str> {
    let mut w = crate::service::WINDOWS.to_vec();
    w.push("lastseason");
    w
}

pub fn routes() -> Router<crate::AppState> {
    Router::new()
        .route("/v1/cali/board", get(get_board))
        .route("/v1/cali/orders", get(list_orders).post(log_order))
}

// --- pure helpers ---------------------------------------------------------

/// What the table actually pays for under a pooled buy-1-get-1.
///
/// Every second taco is free regardless of mild/wild or hard/soft, so an odd
/// total pays for the odd one out and nothing else. This is the one place the
/// deal is encoded. Totals are never negative, so `/` is Python's `//` here.
fn paid_tacos(total: i64) -> i64 {
    (total + 1) / 2
}

/// Python's `round(value, 2)`. See the module header for why this is a format
/// and re-parse rather than arithmetic.
fn round2(v: f64) -> f64 {
    format!("{v:.2}").parse().unwrap_or(v)
}

/// Tacos per person. Zero people is not a dinner, but never divide by it.
fn tpp(tacos: i64, people: usize) -> f64 {
    if people == 0 {
        0.0
    } else {
        round2(tacos as f64 / people as f64)
    }
}

/// The key eaten most, ties to the earlier key in `FAVORITE_ORDER`; `None`
/// when nothing was eaten (rather than the first key).
fn favorite(eaten: &[i64; 15]) -> Option<&'static str> {
    let mut best = None;
    let mut most = 0;
    for (i, n) in eaten.iter().enumerate() {
        if *n > most {
            best = Some(FAVORITE_ORDER[i]);
            most = *n;
        }
    }
    best
}

/// `window_range`, plus "lastseason": the whole previous calendar month.
fn cali_window_range(window: &str, today: NaiveDate) -> (NaiveDate, NaiveDate) {
    if window == "lastseason" {
        // Back to the 1st, then one day earlier is the previous month's last
        // day -- which wraps the year in January for free.
        let last = today.with_day(1).unwrap_or(today) - chrono::Duration::days(1);
        return (last.with_day(1).unwrap_or(last), last);
    }
    crate::service::window_range(window, today)
}

/// Python's `str.casefold()`, as closely as std can manage. Used for the
/// board's name key and the board's sort, where the Python casefolds.
fn casefold(s: &str) -> String {
    s.to_lowercase()
}

/// Python's `str.isprintable()` without a Unicode category table: a space
/// stays, control characters (Cc) and every other whitespace (Zs/Zl/Zp, so
/// U+00A0 too) go, and so do the format (Cf) code points a paste is likely to
/// carry. Exotic Cf/Co/Cn survive here where CPython would drop them.
fn printable(c: char) -> bool {
    if c == ' ' {
        return true;
    }
    if c.is_control() || c.is_whitespace() {
        return false;
    }
    !matches!(c,
        '\u{00ad}' | '\u{0600}'..='\u{0605}' | '\u{200b}'..='\u{200f}'
        | '\u{202a}'..='\u{202e}' | '\u{2060}'..='\u{2064}'
        | '\u{206a}'..='\u{206f}' | '\u{feff}' | '\u{fff9}'..='\u{fffb}')
}

/// The `note` / `name` cleaner: non-printables are deleted (not replaced), runs
/// of spaces collapse and the ends are trimmed. The trailing cap can never
/// fire, because `max_length` is checked against the raw input first -- it is
/// kept so the two sides of the port read the same.
fn clean_text(v: &str, cap: usize) -> String {
    let kept: String = v.chars().filter(|c| printable(*c)).collect();
    kept.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(cap).collect()
}

/// Python's `datetime.isoformat()` on a UTC-aware value: the offset is spelled
/// "+00:00" (never "Z") and the fraction is six digits or absent entirely.
/// `to_rfc3339()` would print nanoseconds, so the precision is pinned by hand.
fn iso_utc(dt: DateTime<Utc>) -> String {
    if dt.timestamp_subsec_micros() == 0 {
        dt.to_rfc3339_opts(SecondsFormat::Secs, false)
    } else {
        dt.to_rfc3339_opts(SecondsFormat::Micros, false)
    }
}

/// SQLite hands back SQLAlchemy's naive "YYYY-MM-DD HH:MM:SS.ffffff"; it was
/// written in UTC, so stamp it UTC rather than guessing a zone.
fn created_iso(stored: &str) -> String {
    match NaiveDateTime::parse_from_str(stored, "%Y-%m-%d %H:%M:%S%.f")
        .or_else(|_| NaiveDateTime::parse_from_str(stored, "%Y-%m-%dT%H:%M:%S%.f"))
    {
        Ok(n) => iso_utc(n.and_utc()),
        // A hand-edited row is not worth a 500; pass the text through.
        Err(_) => stored.to_string(),
    }
}

/// SQLAlchemy's SQLite DATETIME text form, with always six fractional digits.
/// Both backends read the same file and `ORDER BY created_at DESC` is a text
/// sort, so the format has to be exact.
fn stored_datetime(dt: DateTime<Utc>) -> String {
    dt.format("%Y-%m-%d %H:%M:%S%.6f").to_string()
}

fn placeholders(n: usize) -> String {
    vec!["?"; n].join(",")
}

// --- wire shapes ----------------------------------------------------------

/// Menu counts, held as `CALI_MENU` indexes so that menu order -- for the wire
/// *and* for the `eaten` tally -- falls out of construction order.
struct Items(Vec<(usize, i64)>);

impl Items {
    fn total(&self) -> i64 {
        self.0.iter().map(|(_, n)| *n).sum()
    }
}

impl Serialize for Items {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut m = s.serialize_map(Some(self.0.len()))?;
        for (i, n) in &self.0 {
            m.serialize_entry(CALI_MENU[*i].0, n)?;
        }
        m.end()
    }
}

#[derive(Default, Clone, Copy, Serialize)]
struct Counts {
    #[serde(rename = "mildHard")] mild_hard: i64,
    #[serde(rename = "mildSoft")] mild_soft: i64,
    #[serde(rename = "wildHard")] wild_hard: i64,
    #[serde(rename = "wildSoft")] wild_soft: i64,
}

impl Counts {
    fn total(self) -> i64 {
        self.mild_hard + self.mild_soft + self.wild_hard + self.wild_soft
    }
}

#[derive(Serialize)]
struct OrderDinerOut {
    handle: Option<String>,
    name: String,
    #[serde(rename = "avatarUrl")] avatar_url: String,
    tacos: Counts,
    total: i64,
    items: Items,
}

#[derive(Serialize)]
struct OrderOut {
    id: String,
    date: String,
    people: i64,
    #[serde(rename = "totalTacos")] total_tacos: i64,
    #[serde(rename = "paidTacos")] paid_tacos: i64,
    #[serde(rename = "freeTacos")] free_tacos: i64,
    #[serde(rename = "totalItems")] total_items: i64,
    #[serde(rename = "tacosPerPerson")] tacos_per_person: f64,
    note: String,
    #[serde(rename = "loggedByHandle")] logged_by_handle: String,
    #[serde(rename = "createdAt")] created_at: String,
    diners: Vec<OrderDinerOut>,
}

#[derive(Serialize)]
struct BoardEntry {
    rank: i64,
    handle: Option<String>,
    name: String,
    #[serde(rename = "avatarUrl")] avatar_url: String,
    tuesdays: i64,
    #[serde(rename = "totalTacos")] total_tacos: i64,
    #[serde(rename = "tacosPerPerson")] tacos_per_person: f64,
    mild: i64,
    wild: i64,
    hard: i64,
    soft: i64,
    #[serde(rename = "isYou")] is_you: bool,
    items: i64,
    favorite: Option<&'static str>,
}

#[derive(Serialize)]
struct MenuOut {
    kind: &'static str,
    name: &'static str,
}

#[derive(Serialize)]
struct BoardResponse {
    window: String,
    #[serde(rename = "startsOn")] starts_on: NaiveDate,
    #[serde(rename = "endsOn")] ends_on: NaiveDate,
    #[serde(rename = "generatedAt")] generated_at: String,
    orders: i64,
    #[serde(rename = "totalTacos")] total_tacos: i64,
    #[serde(rename = "paidTacos")] paid_tacos: i64,
    #[serde(rename = "freeTacos")] free_tacos: i64,
    #[serde(rename = "totalItems")] total_items: i64,
    entries: Vec<BoardEntry>,
    menu: Vec<MenuOut>,
}

#[derive(Serialize)]
struct LogOrderResponse {
    order: OrderOut,
    /// True when this requestId already logged this dinner; nothing was written.
    replayed: bool,
}

#[derive(Serialize)]
struct OrdersResponse {
    orders: Vec<OrderOut>,
}

/// One pydantic-shaped validation error. `input` and `url` are deliberately
/// absent: the Python's 422 handler strips them so a rejected value is never
/// reflected back to the caller, and this must not reintroduce them.
#[derive(Debug, Serialize)]
struct Verr {
    loc: Vec<Value>,
    msg: String,
    #[serde(rename = "type")] kind: &'static str,
}

#[derive(Serialize)]
struct Errors {
    detail: Vec<Verr>,
}

fn err(code: StatusCode, msg: &str) -> Response {
    (code, Json(json!({ "detail": msg }))).into_response()
}

/// The 422 body: a list of `{loc, msg, type}` and nothing else, matching the
/// Python's `_scrub_422`. Never echo the submitted value back -- this group's
/// whole privacy argument is that a rejected body is not reflected.
fn err422(detail: Vec<Verr>) -> Response {
    (StatusCode::UNPROCESSABLE_ENTITY, Json(Errors { detail })).into_response()
}

fn ve(loc: Vec<Value>, kind: &'static str, msg: impl Into<String>) -> Verr {
    Verr { loc, msg: msg.into(), kind }
}

fn child(at: &[Value], key: &str) -> Vec<Value> {
    let mut v = at.to_vec();
    v.push(json!(key));
    v
}

fn dloc(i: usize, rest: &[Value]) -> Vec<Value> {
    let mut v = vec![json!("body"), json!("diners"), json!(i)];
    v.extend_from_slice(rest);
    v
}

fn chars_word(n: usize) -> &'static str {
    if n == 1 { "character" } else { "characters" }
}

fn items_word(n: usize) -> &'static str {
    if n == 1 { "item" } else { "items" }
}

// --- request validation ---------------------------------------------------
//
// Hand-rolled rather than derived, because the client reads `detail[].loc` to
// highlight the offending field: the 422 body has to keep pydantic's
// {loc, msg, type} shape, and serde's error text would not.

struct Diner {
    handle: Option<String>,
    name: String,
    tacos: Counts,
    items: Items,
}

struct LogOrder {
    request_id: String,
    date: Option<NaiveDate>,
    diners: Vec<Diner>,
    note: String,
}

/// `min_length`, `max_length` then `pattern`, in pydantic-core's order: the
/// first failure is the only error reported for that field.
fn check_token(errs: &mut Vec<Verr>, at: Vec<Value>, s: &str, min: usize, max: usize) -> bool {
    let n = s.chars().count();
    if n < min {
        errs.push(ve(at, "string_too_short",
            format!("String should have at least {min} {}", chars_word(min))));
    } else if n > max {
        errs.push(ve(at, "string_too_long",
            format!("String should have at most {max} {}", chars_word(max))));
    } else if !s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
        errs.push(ve(at, "string_pattern_mismatch",
            format!("String should match pattern '{TOKEN_PATTERN}'")));
    } else {
        return true;
    }
    false
}

/// A `strict=True` bounded integer. Rust's types give the Python's
/// `isinstance(v, int) and not isinstance(v, bool)` for free: `as_i64` is
/// `None` for a bool, a float and a numeric string alike.
fn strict_int(errs: &mut Vec<Verr>, at: Vec<Value>, v: &Value, min: i64, max: i64) -> Option<i64> {
    let Some(n) = v.as_i64() else {
        errs.push(ve(at, "int_type", "Input should be a valid integer"));
        return None;
    };
    if n > max {
        errs.push(ve(at, "less_than_equal", format!("Input should be less than or equal to {max}")));
        return None;
    }
    if n < min {
        errs.push(ve(at, "greater_than_equal",
            format!("Input should be greater than or equal to {min}")));
        return None;
    }
    Some(n)
}

/// A capped, cleaned free-text field. The cap is checked against the *raw*
/// input, so an over-long value is a 422 and is never silently truncated.
fn parse_text(errs: &mut Vec<Verr>, at: Vec<Value>, v: Option<&Value>, cap: usize) -> String {
    match v {
        None => String::new(),
        Some(Value::String(s)) => {
            if s.chars().count() > cap {
                errs.push(ve(at, "string_too_long",
                    format!("String should have at most {cap} {}", chars_word(cap))));
                String::new()
            } else {
                clean_text(s, cap)
            }
        }
        Some(_) => {
            errs.push(ve(at, "string_type", "Input should be a valid string"));
            String::new()
        }
    }
}

fn parse_counts(errs: &mut Vec<Verr>, at: &[Value], v: Option<&Value>) -> Counts {
    let Some(v) = v else { return Counts::default() };
    let Some(obj) = v.as_object() else {
        // An explicit null lands here too, exactly as pydantic's model_type.
        errs.push(ve(at.to_vec(), "model_type",
            "Input should be a valid dictionary or instance of TacoCounts"));
        return Counts::default();
    };
    let mut n = [0i64; 4];
    for (i, key) in COUNT_KEYS.iter().enumerate() {
        let Some(raw) = obj.get(*key) else { continue };
        if let Some(x) = strict_int(errs, child(at, key), raw, 0, MAX_PER_VARIANT) {
            n[i] = x;
        }
    }
    for key in obj.keys() {
        if !COUNT_KEYS.contains(&key.as_str()) {
            errs.push(ve(child(at, key), "extra_forbidden", "Extra inputs are not permitted"));
        }
    }
    Counts { mild_hard: n[0], mild_soft: n[1], wild_hard: n[2], wild_soft: n[3] }
}

fn menu_literals() -> String {
    let quoted: Vec<String> = CALI_MENU.iter().map(|(k, _)| format!("'{k}'")).collect();
    match quoted.split_last() {
        Some((last, head)) => format!("{} or {last}", head.join(", ")),
        None => String::new(),
    }
}

fn parse_items(errs: &mut Vec<Verr>, at: &[Value], v: Option<&Value>) -> Items {
    let Some(v) = v else { return Items(Vec::new()) };
    let Some(obj) = v.as_object() else {
        errs.push(ve(at.to_vec(), "dict_type", "Input should be a valid dictionary"));
        return Items(Vec::new());
    };
    let mut counts = [0i64; 11];
    for (k, raw) in obj {
        match CALI_MENU.iter().position(|(key, _)| *key == k.as_str()) {
            None => {
                // pydantic reports a bad dict *key* under the key plus "[key]".
                let mut loc = child(at, k);
                loc.push(json!("[key]"));
                errs.push(ve(loc, "literal_error",
                    format!("Input should be {}", menu_literals())));
            }
            Some(i) => {
                if let Some(n) = strict_int(errs, child(at, k), raw, 0, MAX_PER_ITEM) {
                    counts[i] = n;
                }
            }
        }
    }
    // A zero is accepted at validation and dropped here, same as _clean_items.
    Items(counts.iter().enumerate().filter(|(_, n)| **n > 0).map(|(i, n)| (i, *n)).collect())
}

fn parse_diner(errs: &mut Vec<Verr>, i: usize, v: &Value) -> Option<Diner> {
    const KEYS: [&str; 4] = ["handle", "name", "tacos", "items"];
    let Some(obj) = v.as_object() else {
        errs.push(ve(dloc(i, &[]), "model_type",
            "Input should be a valid dictionary or instance of DinerOrder"));
        return None;
    };
    let before = errs.len();

    let mut handle = None;
    match obj.get("handle") {
        None | Some(Value::Null) => {}
        Some(Value::String(s)) => {
            if check_token(errs, dloc(i, &[json!("handle")]), s, 1, HANDLE_MAX) {
                handle = Some(s.clone());
            }
        }
        Some(_) => errs.push(ve(dloc(i, &[json!("handle")]), "string_type",
            "Input should be a valid string")),
    }
    let name = parse_text(errs, dloc(i, &[json!("name")]), obj.get("name"), DINER_NAME_MAX);
    let tacos = parse_counts(errs, &dloc(i, &[json!("tacos")]), obj.get("tacos"));
    let items = parse_items(errs, &dloc(i, &[json!("items")]), obj.get("items"));
    for k in obj.keys() {
        if !KEYS.contains(&k.as_str()) {
            errs.push(ve(dloc(i, &[json!(k)]), "extra_forbidden",
                "Extra inputs are not permitted"));
        }
    }

    // The after-validator only runs when this diner's own fields all passed,
    // which is why a whitespace-only name fails here rather than earlier: it
    // cleans to "" and then has no identity to stand on.
    if errs.len() != before {
        return None;
    }
    if handle.is_none() && name.is_empty() {
        errs.push(ve(dloc(i, &[]), "value_error",
            "Value error, a diner needs a handle or a name"));
        return None;
    }
    Some(Diner { handle, name, tacos, items })
}

/// Pydantic's lax `date`: "YYYY-MM-DD", or a datetime string whose time is
/// exactly midnight. An integer unix timestamp is *not* accepted here -- see
/// the module's deviations.
fn parse_date(s: &str) -> Result<NaiveDate, (&'static str, &'static str)> {
    const BAD: (&str, &str) =
        ("date_from_datetime_parsing", "Input should be a valid date or datetime");
    // Pydantic wants a zero-padded date, where chrono would also take
    // "2026-9-1", so the length is the gate. Nothing shorter can be a datetime.
    if s.len() == 10 {
        return NaiveDate::parse_from_str(s, "%Y-%m-%d").map_err(|_| BAD);
    }
    for fmt in ["%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%d %H:%M:%S%.f"] {
        if let Ok(dt) = NaiveDateTime::parse_from_str(s, fmt) {
            if dt.time() == NaiveTime::MIN {
                return Ok(dt.date());
            }
            return Err(("date_from_datetime_inexact",
                "Datetimes provided to dates should have zero time - e.g. be exact dates"));
        }
    }
    Err(BAD)
}

fn parse_body(v: &Value) -> Result<LogOrder, Vec<Verr>> {
    const KEYS: [&str; 4] = ["requestId", "date", "diners", "note"];
    let Some(obj) = v.as_object() else {
        return Err(vec![ve(vec![json!("body")], "model_attributes_type",
            "Input should be a valid dictionary or instance of LogOrderRequest")]);
    };
    let mut errs: Vec<Verr> = Vec::new();
    let at = |f: &str| vec![json!("body"), json!(f)];

    // Fields in definition order, because pydantic reports them that way.
    let mut request_id = String::new();
    match obj.get("requestId") {
        None => errs.push(ve(at("requestId"), "missing", "Field required")),
        Some(Value::String(s)) => {
            if check_token(&mut errs, at("requestId"), s, REQUEST_ID_MIN, REQUEST_ID_MAX) {
                request_id = s.clone();
            }
        }
        Some(_) => errs.push(ve(at("requestId"), "string_type", "Input should be a valid string")),
    }

    // Absent *and* an explicit null both mean tonight, not an error.
    let mut date = None;
    match obj.get("date") {
        None | Some(Value::Null) => {}
        Some(Value::String(s)) => match parse_date(s) {
            Ok(d) => date = Some(d),
            Err((kind, msg)) => errs.push(ve(at("date"), kind, msg)),
        },
        Some(_) => errs.push(ve(at("date"), "date_type", "Input should be a valid date")),
    }

    let mut diners = Vec::new();
    match obj.get("diners") {
        None => errs.push(ve(at("diners"), "missing", "Field required")),
        Some(Value::Array(raw)) => {
            for (i, item) in raw.iter().enumerate() {
                if let Some(d) = parse_diner(&mut errs, i, item) {
                    diners.push(d);
                }
            }
            // "after validation", so the length check follows the items.
            if raw.is_empty() {
                errs.push(ve(at("diners"), "too_short",
                    "List should have at least 1 item after validation, not 0"));
            } else if raw.len() > MAX_DINERS {
                errs.push(ve(at("diners"), "too_long",
                    format!("List should have at most {MAX_DINERS} {} after validation, not {}",
                            items_word(MAX_DINERS), raw.len())));
            }
        }
        Some(_) => errs.push(ve(at("diners"), "list_type", "Input should be a valid list")),
    }

    let note = parse_text(&mut errs, at("note"), obj.get("note"), NOTE_MAX);

    for k in obj.keys() {
        if !KEYS.contains(&k.as_str()) {
            errs.push(ve(child(&[json!("body")], k), "extra_forbidden",
                "Extra inputs are not permitted"));
        }
    }

    if !errs.is_empty() {
        return Err(errs);
    }

    // A model validator, so it only runs once every field is valid. It keys
    // handles with lower() but names with casefold(), which is why
    // [{handle:"ana",name:"Ana"},{name:"Ana"}] passes: "@ana" vs "#ana".
    let mut seen = HashSet::new();
    for d in &diners {
        let key = match &d.handle {
            Some(h) => format!("@{}", h.to_lowercase()),
            None => format!("#{}", casefold(&d.name)),
        };
        if !seen.insert(key) {
            return Err(vec![ve(vec![json!("body")], "value_error",
                "Value error, the same diner is listed twice")]);
        }
    }

    Ok(LogOrder { request_id, date, diners, note })
}

// --- rows -----------------------------------------------------------------

struct UserRow {
    handle: String,
    display_name: String,
    avatar_url: String,
}

impl UserRow {
    /// The Python's `u.display_name or u.handle`.
    fn name(&self) -> String {
        if self.display_name.is_empty() { self.handle.clone() } else { self.display_name.clone() }
    }
}

struct OrderRow {
    id: String,
    user_id: String,
    request_id: String,
    order_date: String,
    total_tacos: i64,
    paid_tacos: i64,
    note: String,
    created_at: String,
}

struct DinerRow {
    id: String,
    user_id: Option<String>,
    diner_name: String,
    counts: Counts,
    /// The stored JSON text. `_clean_items` runs on read as well as on write,
    /// so a legacy or garbage key is dropped at render time either way.
    items: String,
}

fn clean_items(stored: &str) -> Items {
    let v: Value = serde_json::from_str(stored).unwrap_or(Value::Null);
    let mut out = Vec::new();
    if let Some(obj) = v.as_object() {
        for (i, (k, _)) in CALI_MENU.iter().enumerate() {
            // `as_i64` is None for a float and for a bool, which is Python's
            // `isinstance(v, int) and not isinstance(v, bool)` for free.
            let Some(n) = obj.get(*k).and_then(Value::as_i64) else { continue };
            if n > 0 {
                out.push((i, n));
            }
        }
    }
    Items(out)
}

/// Batched account hydration. No `is_active = 1` filter: the XP board has one,
/// this deliberately does not, so a deactivated account still ranks and still
/// shows up in the dinner log.
async fn users_by_id(
    pool: &SqlitePool,
    ids: &HashSet<String>,
) -> Result<HashMap<String, UserRow>, sqlx::Error> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let sql = format!(
        "SELECT id, handle, display_name, avatar_url FROM users WHERE id IN ({})",
        placeholders(ids.len())
    );
    let mut q = sqlx::query(&sql);
    for id in ids {
        q = q.bind(id);
    }
    let rows = q.fetch_all(pool).await?;
    Ok(rows
        .iter()
        .map(|r| {
            (r.get::<String, _>("id"), UserRow {
                handle: r.get("handle"),
                display_name: r.get("display_name"),
                avatar_url: r.get("avatar_url"),
            })
        })
        .collect())
}

fn order_row(r: &sqlx::sqlite::SqliteRow) -> OrderRow {
    OrderRow {
        id: r.get("id"),
        user_id: r.get("user_id"),
        request_id: r.get("request_id"),
        order_date: r.get("order_date"),
        total_tacos: r.get("total_tacos"),
        paid_tacos: r.get("paid_tacos"),
        note: r.get("note"),
        created_at: r.get("created_at"),
    }
}

const ORDER_COLS: &str =
    "id, user_id, request_id, order_date, total_tacos, paid_tacos, note, created_at";

async fn find_order(
    pool: &SqlitePool,
    user_id: &str,
    request_id: &str,
) -> Result<Option<OrderRow>, sqlx::Error> {
    let sql = format!("SELECT {ORDER_COLS} FROM taco_orders WHERE user_id = ?1 AND request_id = ?2");
    Ok(sqlx::query(&sql)
        .bind(user_id)
        .bind(request_id)
        .fetch_optional(pool)
        .await?
        .as_ref()
        .map(order_row))
}

/// The diners of several orders, grouped and kept in insertion order.
///
/// `ORDER BY order_id, rowid` is required, not cosmetic: the primary key is a
/// random uuid4, while the Python's `lazy="selectin"` relationship has no
/// order_by and so yields the order the rows were written in -- which is the
/// request order the clients and tests expect.
async fn diners_for(
    pool: &SqlitePool,
    order_ids: &[String],
) -> Result<HashMap<String, Vec<DinerRow>>, sqlx::Error> {
    let mut out: HashMap<String, Vec<DinerRow>> = HashMap::new();
    if order_ids.is_empty() {
        return Ok(out);
    }
    let sql = format!(
        "SELECT order_id, id, user_id, diner_name, mild_hard, mild_soft, wild_hard, wild_soft,
                items
         FROM taco_diners WHERE order_id IN ({}) ORDER BY order_id, rowid",
        placeholders(order_ids.len())
    );
    let mut q = sqlx::query(&sql);
    for id in order_ids {
        q = q.bind(id);
    }
    for r in q.fetch_all(pool).await? {
        out.entry(r.get("order_id")).or_default().push(DinerRow {
            id: r.get("id"),
            user_id: r.get("user_id"),
            diner_name: r.get("diner_name"),
            counts: Counts {
                mild_hard: r.get("mild_hard"),
                mild_soft: r.get("mild_soft"),
                wild_hard: r.get("wild_hard"),
                wild_soft: r.get("wild_soft"),
            },
            items: r.try_get("items").unwrap_or_default(),
        });
    }
    Ok(out)
}

fn order_out(
    o: &OrderRow,
    diners: &[DinerRow],
    users: &HashMap<String, UserRow>,
    logged_by: &str,
) -> OrderOut {
    let out: Vec<OrderDinerOut> = diners
        .iter()
        .map(|d| {
            let u = d.user_id.as_ref().and_then(|id| users.get(id));
            OrderDinerOut {
                handle: u.map(|u| u.handle.clone()),
                name: u.map_or_else(|| d.diner_name.clone(), UserRow::name),
                avatar_url: u.map(|u| u.avatar_url.clone()).unwrap_or_default(),
                tacos: d.counts,
                total: d.counts.total(),
                items: clean_items(&d.items),
            }
        })
        .collect();
    let people = out.len();
    OrderOut {
        id: o.id.clone(),
        date: o.order_date.clone(),
        people: people as i64,
        // The receipt as priced at the time, never re-summed from the diners.
        total_tacos: o.total_tacos,
        paid_tacos: o.paid_tacos,
        free_tacos: o.total_tacos - o.paid_tacos,
        total_items: out.iter().map(|d| d.items.total()).sum(),
        tacos_per_person: tpp(o.total_tacos, people),
        note: o.note.clone(),
        logged_by_handle: logged_by.to_string(),
        created_at: created_iso(&o.created_at),
        diners: out,
    }
}

// --- GET /v1/cali/orders --------------------------------------------------

/// The last value wins and unknown parameters are ignored, which is what
/// FastAPI does with a repeated query parameter.
fn last_param(pairs: &[(String, String)], key: &str) -> Option<String> {
    pairs.iter().rev().find(|(k, _)| k.as_str() == key).map(|(_, v)| v.clone())
}

async fn list_orders(
    State(st): State<crate::AppState>,
    Query(q): Query<Vec<(String, String)>>,
) -> Response {
    let limit = match last_param(&q, "limit") {
        None => RECENT_ORDERS,
        Some(s) => match s.parse::<i64>() {
            Err(_) => {
                return err422(vec![ve(vec![json!("query"), json!("limit")], "int_parsing",
                    "Input should be a valid integer, unable to parse string as an integer")])
            }
            Ok(n) if n < 1 => {
                return err422(vec![ve(vec![json!("query"), json!("limit")],
                    "greater_than_equal", "Input should be greater than or equal to 1")])
            }
            Ok(n) if n > RECENT_ORDERS => {
                return err422(vec![ve(vec![json!("query"), json!("limit")], "less_than_equal",
                    format!("Input should be less than or equal to {RECENT_ORDERS}"))])
            }
            Ok(n) => n,
        },
    };

    match recent_orders(&st.pool, limit).await {
        Ok(orders) => Json(OrdersResponse { orders }).into_response(),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, &format!("db error: {e}")),
    }
}

/// Recent dinners, newest first. There is no per-caller filter: everyone with a
/// paired device sees the same log, because it is one shared table and not a
/// per-user diary.
async fn recent_orders(pool: &SqlitePool, limit: i64) -> Result<Vec<OrderOut>, sqlx::Error> {
    // No third tiebreak, same as the Python: two rows with an identical
    // (order_date, created_at) have undefined relative order in both.
    let sql = format!(
        "SELECT {ORDER_COLS} FROM taco_orders
         ORDER BY order_date DESC, created_at DESC LIMIT ?1"
    );
    let orders: Vec<OrderRow> =
        sqlx::query(&sql).bind(limit).fetch_all(pool).await?.iter().map(order_row).collect();

    let ids: Vec<String> = orders.iter().map(|o| o.id.clone()).collect();
    let diners = diners_for(pool, &ids).await?;

    // The Python runs two lookups -- the order owners and the diners -- both
    // unfiltered and both keyed by id, so one query over the union is the same
    // two maps.
    let mut user_ids: HashSet<String> = orders.iter().map(|o| o.user_id.clone()).collect();
    for rows in diners.values() {
        user_ids.extend(rows.iter().filter_map(|d| d.user_id.clone()));
    }
    let users = users_by_id(pool, &user_ids).await?;

    Ok(orders
        .iter()
        .map(|o| {
            // "" when the owner row is missing: defensive only, since
            // taco_orders.user_id is ON DELETE CASCADE. Not a null, not an error.
            let logged_by = users.get(&o.user_id).map(|u| u.handle.as_str()).unwrap_or("");
            let rows = diners.get(&o.id).map(|v| v.as_slice()).unwrap_or(&[]);
            order_out(o, rows, &users, logged_by)
        })
        .collect())
}

// --- POST /v1/cali/orders -------------------------------------------------

async fn log_order(State(st): State<crate::AppState>, req: Request) -> Response {
    let c = req.extensions().get::<crate::Caller>().expect("caller set by middleware").clone();
    let (_, body) = req.into_parts();
    let bytes = match axum::body::to_bytes(body, 8 * 1024 * 1024).await {
        Ok(b) => b,
        Err(_) => return err(StatusCode::BAD_REQUEST, "body too large"),
    };
    let raw: Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(_) => return err422(vec![ve(vec![json!("body")], "json_invalid", "JSON decode error")]),
    };
    let body = match parse_body(&raw) {
        Ok(b) => b,
        Err(e) => return err422(e),
    };

    // The date check happens before any database read, so a typo cannot cost a
    // round trip -- and it is the whole point of the endpoint being refusable.
    let today = Utc::now().date_naive();
    let when = body.date.unwrap_or(today);
    if when > today {
        return err(StatusCode::BAD_REQUEST, "that dinner hasn't happened yet");
    }

    // Idempotency read, before handle resolution and before any write: the
    // phone that lost its connection mid-tap can retry without logging Tuesday
    // twice. A replay returns the original order unchanged and writes nothing;
    // the resubmitted body is never compared against it.
    match find_order(&st.pool, &c.user_id, &body.request_id).await {
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, &format!("db error: {e}")),
        Ok(Some(prior)) => return replay(&st.pool, &prior, &c.handle).await,
        Ok(None) => {}
    }

    // Resolve every handle before writing, so an unknown one fails the whole
    // order rather than silently demoting that person to a bare name. The
    // lookup is byte-exact: users.handle has no NOCASE collation, so "Ana"
    // against the account "ana" is a 404 that echoes the handle as submitted.
    let handles: Vec<&str> = body.diners.iter().filter_map(|d| d.handle.as_deref()).collect();
    let mut found: HashMap<String, (String, UserRow)> = HashMap::new();
    if !handles.is_empty() {
        let sql = format!(
            "SELECT id, handle, display_name, avatar_url FROM users WHERE handle IN ({})",
            placeholders(handles.len())
        );
        let mut q = sqlx::query(&sql);
        for h in &handles {
            q = q.bind(*h);
        }
        match q.fetch_all(&st.pool).await {
            Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, &format!("db error: {e}")),
            Ok(rows) => {
                for r in &rows {
                    found.insert(r.get::<String, _>("handle"), (r.get::<String, _>("id"), UserRow {
                        handle: r.get("handle"),
                        display_name: r.get("display_name"),
                        avatar_url: r.get("avatar_url"),
                    }));
                }
            }
        }
    }
    for h in &handles {
        if !found.contains_key(*h) {
            return err(StatusCode::NOT_FOUND, &format!("no such person: {h}"));
        }
    }

    let total: i64 = body.diners.iter().map(|d| d.tacos.total()).sum();
    // Stamped here rather than by datetime('now') so a replay can return it.
    // Truncated to microseconds, because that is all the stored text carries.
    let now = Utc::now();
    let now = now.with_nanosecond(now.timestamp_subsec_micros() * 1000).unwrap_or(now);
    let order = OrderRow {
        id: uuid::Uuid::new_v4().to_string(),
        user_id: c.user_id.clone(),
        request_id: body.request_id.clone(),
        order_date: when.to_string(),
        total_tacos: total,
        paid_tacos: paid_tacos(total),
        note: body.note.clone(),
        created_at: stored_datetime(now),
    };
    let rows: Vec<DinerRow> = body
        .diners
        .iter()
        .map(|d| {
            let u = d.handle.as_ref().and_then(|h| found.get(h));
            DinerRow {
                id: uuid::Uuid::new_v4().to_string(),
                user_id: u.map(|(id, _)| id.clone()),
                // Always written, so the row keeps a human identity after the
                // FK's ON DELETE SET NULL. When a handle resolves, the
                // submitted name is discarded and the account's name wins.
                diner_name: u.map_or_else(|| d.name.clone(), |(_, u)| u.name()),
                counts: d.tacos,
                items: serde_json::to_string(&d.items).unwrap_or_else(|_| "{}".into()),
            }
        })
        .collect();

    if let Err(e) = insert_order(&st.pool, &order, &rows).await {
        // Almost always a concurrent submit of the same requestId that won the
        // race on uq_taco_orders_user_request: replay it. A busy database that
        // never got the row falls through to the same 503, which is what the
        // Python's separate IntegrityError/OperationalError arms add up to.
        tracing::warn!("cali: order insert failed, re-reading for a replay: {e}");
        return match find_order(&st.pool, &c.user_id, &body.request_id).await {
            Ok(Some(prior)) => replay(&st.pool, &prior, &c.handle).await,
            _ => err(StatusCode::SERVICE_UNAVAILABLE, BUSY),
        };
    }

    // Rendered from the rows just written rather than re-read: identical data,
    // and the users were already hydrated for the handle resolution.
    let users: HashMap<String, UserRow> = found.into_values().collect();
    Json(LogOrderResponse {
        order: order_out(&order, &rows, &users, &c.handle),
        replayed: false,
    })
    .into_response()
}

/// One transaction: the order row first, then the diners in submission order.
/// A partially written dinner must never be visible, and dropping the
/// transaction on any error rolls the whole thing back.
async fn insert_order(
    pool: &SqlitePool,
    o: &OrderRow,
    diners: &[DinerRow],
) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;
    sqlx::query(
        "INSERT INTO taco_orders (id, user_id, request_id, order_date, total_tacos,
                                  paid_tacos, note, created_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
    )
    .bind(&o.id)
    .bind(&o.user_id)
    .bind(&o.request_id)
    .bind(&o.order_date)
    .bind(o.total_tacos)
    .bind(o.paid_tacos)
    .bind(&o.note)
    .bind(&o.created_at)
    .execute(&mut *tx)
    .await?;

    for d in diners {
        sqlx::query(
            "INSERT INTO taco_diners (id, order_id, user_id, diner_name, mild_hard,
                                      mild_soft, wild_hard, wild_soft, items)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
        )
        .bind(&d.id)
        .bind(&o.id)
        // NULL for a diner with no account: the FK is ON DELETE SET NULL, and
        // diner_name below is what keeps the row identifiable.
        .bind(d.user_id.as_deref())
        .bind(&d.diner_name)
        .bind(d.counts.mild_hard)
        .bind(d.counts.mild_soft)
        .bind(d.counts.wild_hard)
        .bind(d.counts.wild_soft)
        .bind(&d.items)
        .execute(&mut *tx)
        .await?;
    }

    tx.commit().await
}

/// Return a stored order untouched. A replay writes nothing at all, and the
/// resubmitted body is never compared against what is already there -- there is
/// no 409 in this group.
async fn replay(pool: &SqlitePool, prior: &OrderRow, logged_by: &str) -> Response {
    let ids = [prior.id.clone()];
    let diners = match diners_for(pool, &ids).await {
        Ok(mut by_order) => by_order.remove(&prior.id).unwrap_or_default(),
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, &format!("db error: {e}")),
    };
    let user_ids: HashSet<String> = diners.iter().filter_map(|d| d.user_id.clone()).collect();
    let users = match users_by_id(pool, &user_ids).await {
        Ok(u) => u,
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, &format!("db error: {e}")),
    };
    Json(LogOrderResponse {
        order: order_out(prior, &diners, &users, logged_by),
        replayed: true,
    })
    .into_response()
}

// --- GET /v1/cali/board ---------------------------------------------------

/// One person on the board, accumulated across the window.
struct Person {
    handle: Option<String>,
    name: String,
    /// `name.casefold()`, kept so the sort does not refold on every comparison.
    fold: String,
    avatar_url: String,
    /// Distinct dinner dates: the ranking key.
    dates: HashSet<String>,
    tacos: i64,
    mild: i64,
    wild: i64,
    hard: i64,
    soft: i64,
    items: i64,
    eaten: [i64; 15],
    is_you: bool,
}

async fn get_board(
    State(st): State<crate::AppState>,
    Query(q): Query<Vec<(String, String)>>,
    req: Request,
) -> Response {
    let c = req.extensions().get::<crate::Caller>().expect("caller set by middleware").clone();
    let windows = cali_windows();
    let window = last_param(&q, "window").unwrap_or_else(|| "season".into());
    // A bare string with no framework validation, so this is a 400 and not a 422.
    if !windows.contains(&window.as_str()) {
        return err(StatusCode::BAD_REQUEST,
                   &format!("window must be one of {}", windows.join(", ")));
    }
    match build_board(&st.pool, &window, &c.user_id).await {
        Ok(b) => Json(b).into_response(),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, &format!("db error: {e}")),
    }
}

/// The cali-leaderboard: Tuesdays attended, TT as the tiebreak.
///
/// Aggregated here rather than in SQL because a person is keyed by account when
/// they have one and by name when they don't, which is a COALESCE over two
/// columns of different provenance. A founders' dinner log is a few hundred
/// rows a year, so the clarity is worth more than the pushdown.
///
/// One consequence of that key: someone logged by handle one week and by a bare
/// name the next is two people on the board.
async fn build_board(
    pool: &SqlitePool,
    window: &str,
    viewer_id: &str,
) -> Result<BoardResponse, sqlx::Error> {
    let today = Utc::now().date_naive();
    let (starts_on, ends_on) = cali_window_range(window, today);

    // Both bounds are inclusive. order_date is DATE stored as 'YYYY-MM-DD'
    // text, so the lexicographic comparison *is* the date comparison.
    let rows = sqlx::query(
        r#"
        SELECT d.user_id, d.diner_name, d.mild_hard, d.mild_soft, d.wild_hard, d.wild_soft,
               d.items, o.id AS order_id, o.order_date, o.total_tacos, o.paid_tacos
        FROM taco_diners d JOIN taco_orders o ON o.id = d.order_id
        WHERE o.order_date >= ?1 AND o.order_date <= ?2
        "#,
    )
    .bind(starts_on.to_string())
    .bind(ends_on.to_string())
    .fetch_all(pool)
    .await?;

    let ids: HashSet<String> =
        rows.iter().filter_map(|r| r.get::<Option<String>, _>("user_id")).collect();
    let users = users_by_id(pool, &ids).await?;

    let mut people: Vec<Person> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    let mut seen_orders: HashSet<String> = HashSet::new();
    let (mut total, mut paid, mut total_items) = (0i64, 0i64, 0i64);

    for r in &rows {
        let order_id: String = r.get("order_id");
        let order_date: String = r.get("order_date");
        // Deduped per order, from the stored receipt columns.
        if seen_orders.insert(order_id) {
            total += r.get::<i64, _>("total_tacos");
            paid += r.get::<i64, _>("paid_tacos");
        }
        let counts = Counts {
            mild_hard: r.get("mild_hard"),
            mild_soft: r.get("mild_soft"),
            wild_hard: r.get("wild_hard"),
            wild_soft: r.get("wild_soft"),
        };
        let items = clean_items(&r.try_get::<String, _>("items").unwrap_or_default());
        // Tacos are deduped per order; items are counted per diner row.
        total_items += items.total();

        let user_id: Option<String> = r.get("user_id");
        let diner_name: String = r.get("diner_name");
        let u = user_id.as_ref().and_then(|id| users.get(id));
        let key = match &user_id {
            Some(id) => format!("@{id}"),
            None => format!("#{}", casefold(&diner_name)),
        };
        let slot = *index.entry(key).or_insert_with(|| {
            // Seeded from the first row seen. The query has no ORDER BY, so for
            // a name-keyed person the displayed spelling is whichever row
            // SQLite hands back first -- non-deterministic in the Python too,
            // and only the label varies: the final sort is deterministic.
            let name = u.map_or_else(|| diner_name.clone(), UserRow::name);
            people.push(Person {
                handle: u.map(|u| u.handle.clone()),
                fold: casefold(&name),
                name,
                avatar_url: u.map(|u| u.avatar_url.clone()).unwrap_or_default(),
                dates: HashSet::new(),
                tacos: 0,
                mild: 0,
                wild: 0,
                hard: 0,
                soft: 0,
                items: 0,
                eaten: [0; 15],
                is_you: user_id.as_deref() == Some(viewer_id),
            });
            people.len() - 1
        });

        let p = &mut people[slot];
        p.dates.insert(order_date);
        p.tacos += counts.total();
        p.mild += counts.mild_hard + counts.mild_soft;
        p.wild += counts.wild_hard + counts.wild_soft;
        p.hard += counts.mild_hard + counts.wild_hard;
        p.soft += counts.mild_soft + counts.wild_soft;
        p.items += items.total();
        p.eaten[0] += counts.mild_hard;
        p.eaten[1] += counts.mild_soft;
        p.eaten[2] += counts.wild_hard;
        p.eaten[3] += counts.wild_soft;
        for (i, n) in &items.0 {
            p.eaten[4 + *i] += *n;
        }
    }

    // Tuesdays first, then TT, then the folded name so the order is stable
    // across calls. Items never influence the ranking.
    people.sort_by(|a, b| {
        b.dates
            .len()
            .cmp(&a.dates.len())
            .then(b.tacos.cmp(&a.tacos))
            .then_with(|| a.fold.cmp(&b.fold))
    });

    // Ranks are dense 1..n; a tie does not share a rank.
    let entries = people
        .iter()
        .enumerate()
        .map(|(i, p)| BoardEntry {
            rank: i as i64 + 1,
            handle: p.handle.clone(),
            name: p.name.clone(),
            avatar_url: p.avatar_url.clone(),
            tuesdays: p.dates.len() as i64,
            total_tacos: p.tacos,
            tacos_per_person: tpp(p.tacos, p.dates.len()),
            mild: p.mild,
            wild: p.wild,
            hard: p.hard,
            soft: p.soft,
            is_you: p.is_you,
            items: p.items,
            favorite: favorite(&p.eaten),
        })
        .collect();

    Ok(BoardResponse {
        window: window.to_string(),
        starts_on,
        ends_on,
        generated_at: iso_utc(Utc::now()),
        orders: seen_orders.len() as i64,
        total_tacos: total,
        paid_tacos: paid,
        free_tacos: total - paid,
        total_items,
        entries,
        // Always all 11, in menu order: the client uses Array.isArray(b.menu)
        // as the flag for "this Arena records items" and silently drops the
        // user's item counts without it, which is user-visible data loss.
        menu: CALI_MENU.iter().map(|&(kind, name)| MenuOut { kind, name }).collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).expect("test date")
    }

    #[test]
    fn pooling_is_the_whole_point() {
        for (total, want) in
            [(0, 0), (1, 1), (2, 1), (3, 2), (4, 2), (5, 3), (8, 4), (9, 5), (100, 50)]
        {
            assert_eq!(paid_tacos(total), want, "total {total}");
        }
        // Two people wanting three each pay for three between them, not four.
        assert_eq!(paid_tacos(3 + 3), 3);
        assert_eq!(paid_tacos(3) + paid_tacos(3), 4);
    }

    #[test]
    fn round2_matches_cpython_not_arithmetic() {
        // Each of these is a case where (v * 100.0).round() / 100.0 disagrees.
        for (v, want) in [
            (0.625, 0.62), (0.375, 0.38), (1.125, 1.12), (1.375, 1.38), (3.125, 3.12),
        ] {
            assert_eq!(round2(v), want, "round2({v})");
        }
        // 1/40: the binary value sits just above the tie, so it rounds up.
        assert_eq!(round2(1.0 / 40.0), 0.03);
        assert_eq!(tpp(5, 2), 2.5);
        assert_eq!(tpp(10, 3), 3.33);
        // Zero people is not a dinner, but never divide by it.
        assert_eq!(tpp(7, 0), 0.0);
    }

    #[test]
    fn lastseason_is_the_whole_previous_month() {
        assert_eq!(cali_window_range("lastseason", d(2026, 10, 5)),
                   (d(2026, 9, 1), d(2026, 9, 30)));
        assert_eq!(cali_window_range("lastseason", d(2026, 3, 1)),
                   (d(2026, 2, 1), d(2026, 2, 28)));
        // January wraps the year.
        assert_eq!(cali_window_range("lastseason", d(2027, 1, 12)),
                   (d(2026, 12, 1), d(2026, 12, 31)));
    }

    #[test]
    fn the_other_windows_are_the_xp_boards() {
        let t = d(2026, 9, 26);
        assert_eq!(cali_window_range("7d", t), (d(2026, 9, 20), t));
        assert_eq!(cali_window_range("30d", t), (d(2026, 8, 28), t));
        assert_eq!(cali_window_range("season", t), (d(2026, 9, 1), t));
        assert_eq!(cali_window_range("all", t), (d(2020, 1, 1), t));
    }

    #[test]
    fn the_window_error_names_them_in_order() {
        let w = cali_windows();
        assert_eq!(format!("window must be one of {}", w.join(", ")),
                   "window must be one of season, 30d, 7d, all, lastseason");
        // "lastseason" must not leak back into the XP board's windows.
        assert!(!crate::service::WINDOWS.contains(&"lastseason"));
    }

    #[test]
    fn favorite_order_tracks_the_menu() {
        assert_eq!(&FAVORITE_ORDER[..4], &COUNT_KEYS[..]);
        let menu: Vec<&str> = CALI_MENU.iter().map(|(k, _)| *k).collect();
        assert_eq!(&FAVORITE_ORDER[4..], &menu[..]);
    }

    #[test]
    fn favorite_ties_go_to_the_earlier_key() {
        let mut eaten = [0i64; 15];
        assert_eq!(favorite(&eaten), None, "an empty tally has no favourite");

        // 4 wildSoft beats 3 nachos.
        eaten[3] = 4;
        eaten[8] = 3;
        assert_eq!(favorite(&eaten), Some("wildSoft"));

        // A taco variant wins an equal count against a menu item.
        let mut tie = [0i64; 15];
        tie[0] = 2;
        tie[13] = 2;
        assert_eq!(favorite(&tie), Some("mildHard"));

        // {soda: 2, ricebowl: 2} goes to the earlier menu position.
        let mut menu_tie = [0i64; 15];
        menu_tie[4 + 1] = 2;
        menu_tie[4 + 9] = 2;
        assert_eq!(favorite(&menu_tie), Some("ricebowl"));
    }

    #[test]
    fn items_serialise_in_menu_order_not_alphabetically() {
        let items = clean_items(r#"{"nachos":1,"ricebowl":2}"#);
        assert_eq!(serde_json::to_string(&items).expect("items"),
                   r#"{"ricebowl":2,"nachos":1}"#);
    }

    #[test]
    fn clean_items_refilters_on_read() {
        // Unknown keys, non-positive counts, floats and booleans all go.
        let items = clean_items(
            r#"{"pizza":4,"soda":0,"guac":-1,"chips":2.5,"churros":true,"burrito":3}"#,
        );
        assert_eq!(serde_json::to_string(&items).expect("items"), r#"{"burrito":3}"#);
        assert_eq!(items.total(), 3);
        // A garbage column is an empty object, not a panic.
        assert_eq!(clean_items("not json").0.len(), 0);
        assert_eq!(clean_items("[]").0.len(), 0);
    }

    #[test]
    fn text_is_cleaned_not_escaped() {
        // Non-printables are deleted, not replaced by a space.
        assert_eq!(clean_text("bogo\tnight", NOTE_MAX), "bogonight");
        // U+00A0 is Zs, so it goes too.
        assert_eq!(clean_text("bogo\u{00a0}night", NOTE_MAX), "bogonight");
        assert_eq!(clean_text("  two   words  ", NOTE_MAX), "two words");
        assert_eq!(clean_text("   ", DINER_NAME_MAX), "");
    }

    #[test]
    fn isoformat_spells_the_offset_the_python_way() {
        let exact = d(2026, 9, 29).and_hms_opt(19, 30, 1).expect("time").and_utc();
        assert_eq!(iso_utc(exact), "2026-09-29T19:30:01+00:00");
        let frac = d(2026, 9, 29)
            .and_hms_micro_opt(19, 30, 1, 123_456)
            .expect("time")
            .and_utc();
        assert_eq!(iso_utc(frac), "2026-09-29T19:30:01.123456+00:00");
        // Round-trips through SQLAlchemy's stored text form.
        assert_eq!(stored_datetime(frac), "2026-09-29 19:30:01.123456");
        assert_eq!(created_iso("2026-09-29 19:30:01.123456"),
                   "2026-09-29T19:30:01.123456+00:00");
        assert_eq!(created_iso("2026-09-29 19:30:01.000000"), "2026-09-29T19:30:01+00:00");
    }

    fn body(j: &str) -> Result<LogOrder, Vec<Verr>> {
        parse_body(&serde_json::from_str(j).expect("test json"))
    }

    fn kinds(j: &str) -> Vec<(Vec<Value>, &'static str)> {
        match body(j) {
            Ok(_) => Vec::new(),
            Err(e) => e.into_iter().map(|v| (v.loc, v.kind)).collect(),
        }
    }

    const OK: &str = r#"{"requestId":"abcdefghijklmnop","diners":[{"handle":"ana"}]}"#;

    #[test]
    fn a_minimal_order_validates() {
        let b = body(OK).expect("minimal order");
        assert_eq!(b.request_id, "abcdefghijklmnop");
        assert_eq!(b.date, None);
        assert_eq!(b.note, "");
        assert_eq!(b.diners.len(), 1);
        assert_eq!(b.diners[0].tacos.total(), 0, "a zero-taco diner still attended");
    }

    #[test]
    fn request_id_bounds_are_exact() {
        assert_eq!(kinds(r#"{"requestId":"123456789012345","diners":[{"name":"Bo"}]}"#),
                   vec![(vec![json!("body"), json!("requestId")], "string_too_short")]);
        let long = "a".repeat(65);
        let j = format!(r#"{{"requestId":"{long}","diners":[{{"name":"Bo"}}]}}"#);
        assert_eq!(kinds(&j), vec![(vec![json!("body"), json!("requestId")], "string_too_long")]);
        assert_eq!(kinds(r#"{"requestId":"abcdefghijklmno.","diners":[{"name":"Bo"}]}"#),
                   vec![(vec![json!("body"), json!("requestId")], "string_pattern_mismatch")]);
        assert_eq!(kinds(r#"{"diners":[{"name":"Bo"}]}"#),
                   vec![(vec![json!("body"), json!("requestId")], "missing")]);
    }

    #[test]
    fn a_stray_field_is_rejected_at_every_level() {
        let at_body = r#"{"requestId":"abcdefghijklmnop","diners":[{"handle":"ana"}],
                          "sessionId":"/Users/me/secret"}"#;
        assert_eq!(kinds(at_body),
                   vec![(vec![json!("body"), json!("sessionId")], "extra_forbidden")]);
        let at_diner = r#"{"requestId":"abcdefghijklmnop",
                           "diners":[{"handle":"ana","prompt":"hi"}]}"#;
        assert_eq!(kinds(at_diner),
                   vec![(vec![json!("body"), json!("diners"), json!(0), json!("prompt")],
                         "extra_forbidden")]);
        let at_tacos = r#"{"requestId":"abcdefghijklmnop",
                           "diners":[{"handle":"ana","tacos":{"paidTacos":0}}]}"#;
        assert_eq!(kinds(at_tacos),
                   vec![(vec![json!("body"), json!("diners"), json!(0), json!("tacos"),
                              json!("paidTacos")], "extra_forbidden")]);
    }

    #[test]
    fn a_client_cannot_price_its_own_dinner() {
        let j = r#"{"requestId":"abcdefghijklmnop","diners":[{"handle":"ana"}],
                    "totalTacos":99,"paidTacos":0}"#;
        let errs = kinds(j);
        assert_eq!(errs.len(), 2, "both totals rejected: {errs:?}");
        assert!(errs.iter().all(|(_, k)| *k == "extra_forbidden"));
    }

    #[test]
    fn the_diners_list_is_capped() {
        let one = r#"{"handle":"a","tacos":{"mildHard":1}}"#;
        assert_eq!(kinds(r#"{"requestId":"abcdefghijklmnop","diners":[]}"#),
                   vec![(vec![json!("body"), json!("diners")], "too_short")]);
        let twenty: Vec<String> =
            (0..20).map(|i| format!(r#"{{"name":"P{i}"}}"#)).collect();
        let ok = format!(r#"{{"requestId":"abcdefghijklmnop","diners":[{}]}}"#,
                         twenty.join(","));
        assert!(body(&ok).is_ok(), "20 diners is the cap, not over it");
        let twentyone: Vec<String> =
            (0..21).map(|i| format!(r#"{{"name":"P{i}"}}"#)).collect();
        let over = format!(r#"{{"requestId":"abcdefghijklmnop","diners":[{}]}}"#,
                           twentyone.join(","));
        assert_eq!(kinds(&over), vec![(vec![json!("body"), json!("diners")], "too_long")]);
        assert!(body(&format!(r#"{{"requestId":"abcdefghijklmnop","diners":[{one}]}}"#)).is_ok());
    }

    #[test]
    fn taco_counts_are_strict_and_bounded() {
        let j = |t: &str| {
            format!(r#"{{"requestId":"abcdefghijklmnop","diners":[{{"handle":"a","tacos":{t}}}]}}"#)
        };
        let at = vec![json!("body"), json!("diners"), json!(0), json!("tacos"), json!("mildHard")];
        assert_eq!(kinds(&j(r#"{"mildHard":51}"#)), vec![(at.clone(), "less_than_equal")]);
        assert_eq!(kinds(&j(r#"{"mildHard":-1}"#)), vec![(at.clone(), "greater_than_equal")]);
        assert_eq!(kinds(&j(r#"{"mildHard":"3"}"#)), vec![(at.clone(), "int_type")]);
        assert_eq!(kinds(&j(r#"{"mildHard":1.0}"#)), vec![(at.clone(), "int_type")]);
        assert_eq!(kinds(&j(r#"{"mildHard":true}"#)), vec![(at, "int_type")]);
        assert_eq!(kinds(&j("null")),
                   vec![(vec![json!("body"), json!("diners"), json!(0), json!("tacos")],
                         "model_type")]);
        assert!(body(&j(r#"{"mildHard":50}"#)).is_ok(), "50 is the cap, not over it");
    }

    #[test]
    fn menu_items_are_checked_by_key_and_by_value() {
        let j = |i: &str| {
            format!(r#"{{"requestId":"abcdefghijklmnop","diners":[{{"handle":"a","items":{i}}}]}}"#)
        };
        assert_eq!(kinds(&j(r#"{"pizza":1}"#)),
                   vec![(vec![json!("body"), json!("diners"), json!(0), json!("items"),
                              json!("pizza"), json!("[key]")], "literal_error")]);
        let soda = vec![json!("body"), json!("diners"), json!(0), json!("items"), json!("soda")];
        assert_eq!(kinds(&j(r#"{"soda":21}"#)), vec![(soda.clone(), "less_than_equal")]);
        assert_eq!(kinds(&j(r#"{"soda":-1}"#)), vec![(soda.clone(), "greater_than_equal")]);
        assert_eq!(kinds(&j(r#"{"soda":"2"}"#)), vec![(soda.clone(), "int_type")]);
        assert_eq!(kinds(&j(r#"{"soda":true}"#)), vec![(soda, "int_type")]);
        assert_eq!(kinds(&j("[]")),
                   vec![(vec![json!("body"), json!("diners"), json!(0), json!("items")],
                         "dict_type")]);
        // A zero is accepted and dropped.
        let kept = body(&j(r#"{"soda":0,"guac":2}"#)).expect("zero is legal");
        assert_eq!(serde_json::to_string(&kept.diners[0].items).expect("items"),
                   r#"{"guac":2}"#);
        assert_eq!(menu_literals(),
                   "'burrito', 'ricebowl', 'saladbowl', 'quesadilla', 'nachos', 'tostada', \
                    'chips', 'guac', 'churros', 'soda' or 'icedtea'");
    }

    #[test]
    fn a_diner_needs_a_handle_or_a_name() {
        let bare = r#"{"requestId":"abcdefghijklmnop","diners":[{"tacos":{"mildHard":1}}]}"#;
        assert_eq!(kinds(bare),
                   vec![(vec![json!("body"), json!("diners"), json!(0)], "value_error")]);
        // Whitespace cleans to "" and so has no identity either...
        let blank = r#"{"requestId":"abcdefghijklmnop","diners":[{"name":"   "}]}"#;
        assert_eq!(kinds(blank),
                   vec![(vec![json!("body"), json!("diners"), json!(0)], "value_error")]);
        // ...but it is fine alongside a handle.
        let with_handle = r#"{"requestId":"abcdefghijklmnop","diners":[{"handle":"ana",
                              "name":"   "}]}"#;
        assert!(body(with_handle).is_ok());
        assert_eq!(kinds(r#"{"requestId":"abcdefghijklmnop","diners":[{"handle":""}]}"#),
                   vec![(vec![json!("body"), json!("diners"), json!(0), json!("handle")],
                         "string_too_short")]);
    }

    #[test]
    fn names_and_notes_are_never_silently_truncated() {
        let long = "n".repeat(41);
        let j = format!(r#"{{"requestId":"abcdefghijklmnop","diners":[{{"name":"{long}"}}]}}"#);
        assert_eq!(kinds(&j),
                   vec![(vec![json!("body"), json!("diners"), json!(0), json!("name")],
                         "string_too_long")]);
        let note = "x".repeat(81);
        let j = format!(r#"{{"requestId":"abcdefghijklmnop","diners":[{{"handle":"a"}}],
                            "note":"{note}"}}"#);
        assert_eq!(kinds(&j), vec![(vec![json!("body"), json!("note")], "string_too_long")]);
        let j = r#"{"requestId":"abcdefghijklmnop","diners":[{"handle":"a"}],"note":null}"#;
        assert_eq!(kinds(j), vec![(vec![json!("body"), json!("note")], "string_type")]);
        let j = r#"{"requestId":"abcdefghijklmnop","diners":[{"handle":"a"}],
                    "note":"bogo\tnight"}"#;
        assert_eq!(body(j).expect("note").note, "bogonight");
    }

    #[test]
    fn duplicate_diners_are_refused_but_a_handle_and_a_name_are_not_the_same_person() {
        let dup = r#"{"requestId":"abcdefghijklmnop",
                      "diners":[{"handle":"Ana"},{"handle":"ana"}]}"#;
        assert_eq!(kinds(dup), vec![(vec![json!("body")], "value_error")]);
        let dup_name = r#"{"requestId":"abcdefghijklmnop",
                           "diners":[{"name":"Bo"},{"name":"bo"}]}"#;
        assert_eq!(kinds(dup_name), vec![(vec![json!("body")], "value_error")]);
        // "@ana" and "#ana" are different keys, so this is two people.
        let both = r#"{"requestId":"abcdefghijklmnop",
                       "diners":[{"handle":"ana","name":"Ana"},{"name":"Ana"}]}"#;
        assert_eq!(body(both).expect("two keys").diners.len(), 2);
    }

    #[test]
    fn the_dinner_date_is_optional_and_exact() {
        let j = |v: &str| {
            format!(r#"{{"requestId":"abcdefghijklmnop","diners":[{{"handle":"a"}}],"date":{v}}}"#)
        };
        assert_eq!(body(&j("null")).expect("null is tonight").date, None);
        assert_eq!(body(&j(r#""2026-09-29""#)).expect("date").date, Some(d(2026, 9, 29)));
        assert_eq!(body(&j(r#""2026-09-29T00:00:00""#)).expect("midnight").date,
                   Some(d(2026, 9, 29)));
        assert_eq!(kinds(&j(r#""2026-09-29T19:30:00""#)),
                   vec![(vec![json!("body"), json!("date")], "date_from_datetime_inexact")]);
        assert_eq!(kinds(&j(r#""not a date""#)),
                   vec![(vec![json!("body"), json!("date")], "date_from_datetime_parsing")]);
        assert_eq!(kinds(&j("true")), vec![(vec![json!("body"), json!("date")], "date_type")]);
    }

    #[test]
    fn a_non_object_body_is_one_error() {
        assert_eq!(kinds("[]"), vec![(vec![json!("body")], "model_attributes_type")]);
    }

    #[test]
    fn the_last_query_value_wins() {
        let q = vec![
            ("window".to_string(), "a".to_string()),
            ("window".to_string(), "b".to_string()),
            ("unknown".to_string(), "x".to_string()),
        ];
        assert_eq!(last_param(&q, "window").as_deref(), Some("b"));
        assert_eq!(last_param(&q, "limit"), None);
    }

    #[test]
    fn placeholders_match_the_bind_count() {
        assert_eq!(placeholders(1), "?");
        assert_eq!(placeholders(3), "?,?,?");
    }
}
