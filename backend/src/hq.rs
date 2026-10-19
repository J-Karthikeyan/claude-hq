//! HQ 2.1: visit and customise HQs, ported from `backend/app/hq.py`.
//!
//! Each user may open their 3D HQ to visitors. A visitor sees the building
//! (paint, accent, sign), the owner's level, how many of their crew are
//! working / need them / idle, and their equipped cosmetics. Counts and
//! cosmetics only: no session id, title, project, path, prompt or anything a
//! transcript said may cross this boundary in either direction. That is why the
//! inbound body is validated field by field instead of being merged from
//! free-form JSON -- an unknown key is a 422, never a stored surprise.
//!
//! The level is scored here from daily_stats and game_results on every read. It
//! is never stored (hq_profiles has no level column) and never taken from the
//! client, so a building cannot be grown by editing a request.

use axum::{
    extract::{Path, Request, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use serde::de::{MapAccess, Visitor};
use serde::{Deserialize, Serialize, Serializer};
use serde_json::{json, Value};
use sqlx::{Row, SqlitePool};
use std::collections::HashMap;

/// `LIMIT` on the open list, as in the Python.
const OPEN_LIST_MAX: i64 = 100;
/// Game XP is capped per (user, UTC day) so grinding games stops paying.
const GAME_XP_DAY_CAP: i64 = 400;
const CREW_MAX: i64 = 64;
/// Code points, not bytes, and checked before whitespace is collapsed.
const SIGN_MAX: usize = 18;
/// The nine characters a sign may carry besides letters and digits. The last is
/// MIDDLE DOT U+00B7 -- which the error message below does not mention, and
/// that wording is the exact string the client already sees.
const SIGN_PUNCT: &str = " .,'&!?-·";
/// Bodies here are three fields; anything larger is not a body we would accept.
const MAX_BODY: usize = 64 * 1024;

/// Cosmetics catalog as (id, slot, value), inlined from `app/cosmetics.py`.
/// The `decor` values are keys rather than colours, and three ids share
/// `#d8b34a` in different slots -- which is why the projection below matches on
/// the slot as well as the id.
const CATALOG: &[(&str, &str, &str)] = &[
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

/// Every route here needs a paired device; `main.rs` wraps the whole guarded
/// router in `require_device`, so this adds no layer of its own.
///
/// A server missing these routes answers a bare 404, which the client reads as
/// "this Arena doesn't have visits yet" -- a half-registered router is a
/// silently degraded feature rather than a visible error, so all four go on
/// together or not at all.
pub fn routes() -> Router<crate::AppState> {
    Router::new()
        // `/me` and `/open` are static segments, which matchit prefers over
        // `:user_id` whatever the registration order -- so a user whose id were
        // literally "me" is unreachable, exactly as in the Python.
        .route("/v1/hq/me", get(get_me).put(put_me).fallback(method_not_allowed))
        .route("/v1/hq/open", get(list_open).fallback(method_not_allowed))
        .route("/v1/hq/:user_id", get(get_one).fallback(method_not_allowed))
}

// --- wire shapes ----------------------------------------------------------

/// One HQ as everyone sees it. Field order is the response's key order and is
/// part of the contract, so it matches `HqProfileOut` declaration for
/// declaration. `look` and `crew` always carry all their keys on the way out,
/// even though nulls are dropped on the way in.
#[derive(Debug, Serialize)]
struct HqOut {
    #[serde(rename = "userId")] user_id: String,
    handle: String,
    #[serde(rename = "displayName")] display_name: String,
    #[serde(rename = "trainerName")] trainer_name: Option<String>,
    #[serde(rename = "avatarUrl")] avatar_url: String,
    level: i64,
    open: bool,
    look: LookOut,
    crew: CrewOut,
    #[serde(rename = "updatedAt")] updated_at: Option<String>,
    #[serde(rename = "isYou")] is_you: bool,
    cos: Cos,
}

#[derive(Debug, Default, PartialEq, Serialize)]
struct LookOut {
    paint: Option<String>,
    accent: Option<String>,
    sign: Option<String>,
}

#[derive(Debug, Default, PartialEq, Serialize)]
struct CrewOut {
    working: i64,
    needs: i64,
    idle: i64,
}

/// The `cos` map: slot -> colour or decor key.
///
/// A list of pairs rather than a `serde_json::Map`, because serde_json's
/// `preserve_order` feature is off here and its `Map` is a `BTreeMap` -- so a
/// `Map` would emit the slots alphabetically where the Python emits the stored
/// dict's own order. Serialising through `collect_map` keeps this order.
#[derive(Debug, Clone, Default, PartialEq)]
struct Cos(Vec<(String, String)>);

impl Serialize for Cos {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_map(self.0.iter().map(|(slot, value)| (slot, value)))
    }
}

/// A stored `equipped_cosmetics.slots` object, kept in the row's key order for
/// the same reason: going through `Value` would sort it on the way in.
struct Slots(Vec<(String, Value)>);

impl<'de> Deserialize<'de> for Slots {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Slots;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("an object of slot -> item id")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut m: A) -> Result<Slots, A::Error> {
                let mut out = Vec::new();
                while let Some(pair) = m.next_entry::<String, Value>()? {
                    out.push(pair);
                }
                Ok(Slots(out))
            }
        }
        d.deserialize_map(V)
    }
}

/// The public identity of whoever's HQ is being shown. On `/me` it comes from
/// the `Caller`; on a visit it is read from `users`, because it is the visited
/// user's identity and not the caller's.
struct Public {
    id: String,
    handle: String,
    display_name: String,
    trainer_name: String,
    avatar_url: String,
}

impl Public {
    fn from_caller(c: &crate::Caller) -> Self {
        Self {
            id: c.user_id.clone(),
            handle: c.handle.clone(),
            display_name: c.display_name.clone(),
            trainer_name: c.trainer_name.clone(),
            avatar_url: c.avatar_url.clone(),
        }
    }
}

/// An `hq_profiles` row, kept as the stored JSON text: the write path compares
/// it against what a PUT would store, and reformatting it first would make
/// every row written by the Python look changed.
struct Prof {
    open: bool,
    look: String,
    crew: String,
    updated_at: Option<String>,
}

/// The validated body of `PUT /v1/hq/me`. `None` and absent are the same thing
/// for all three fields: leave that column alone.
#[derive(Debug, Default, PartialEq)]
struct HqUpdate {
    open: Option<bool>,
    look: Option<LookIn>,
    crew: Option<CrewIn>,
}

#[derive(Debug, Default, PartialEq)]
struct LookIn {
    paint: Option<String>,
    accent: Option<String>,
    sign: Option<String>,
}

#[derive(Debug, Default, PartialEq)]
struct CrewIn {
    working: i64,
    needs: i64,
    idle: i64,
}

// --- handlers -------------------------------------------------------------

fn err(code: StatusCode, msg: &str) -> Response {
    (code, Json(json!({ "detail": msg }))).into_response()
}

/// Starlette answers a wrong method with a JSON `detail`; axum's default 405
/// has an empty body, so the method fallback restores the shape. The `Allow`
/// header is still appended by the method router.
async fn method_not_allowed() -> Response {
    err(StatusCode::METHOD_NOT_ALLOWED, "Method Not Allowed")
}

/// One message for all three causes of a refused visit -- no such user, a
/// disabled account, and a closed HQ -- so this endpoint cannot be used to
/// enumerate user ids.
fn not_open() -> Response {
    err(StatusCode::NOT_FOUND, "that HQ is not open to visitors")
}

/// Fold a query failure into the 500 shape `main.rs` uses, so the inner
/// functions can use `?` and read like the Python they came from.
fn db500(r: Result<Response, sqlx::Error>) -> Response {
    match r {
        Ok(res) => res,
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, &format!("db error: {e}")),
    }
}

async fn get_me(State(st): State<crate::AppState>, req: Request) -> Response {
    let c = req.extensions().get::<crate::Caller>().expect("caller set by middleware").clone();
    db500(get_me_inner(&st.pool, &c).await)
}

async fn get_me_inner(pool: &SqlitePool, c: &crate::Caller) -> Result<Response, sqlx::Error> {
    let prof = load_prof(pool, &c.user_id).await?;
    let level = level_of(pool, &c.user_id).await?;
    // `cos` is deliberately empty here: the Python's get_me and update_me never
    // call _with_cos, while the open list and a visit do. The asymmetry is the
    // contract, not an oversight.
    let out = project(&Public::from_caller(c), prof.as_ref(), level, &c.user_id, Cos::default());
    Ok(Json(out).into_response())
}

async fn put_me(State(st): State<crate::AppState>, req: Request) -> Response {
    let c = req.extensions().get::<crate::Caller>().expect("caller set by middleware").clone();
    let (_, body) = req.into_parts();
    let bytes = match axum::body::to_bytes(body, MAX_BODY).await {
        Ok(b) => b,
        Err(_) => return unprocessable(&[at(&["body"], JSON_INVALID)]),
    };
    let update = match read_update(&bytes) {
        Ok(u) => u,
        Err(errs) => return unprocessable(&errs),
    };
    db500(put_me_inner(&st.pool, &c, &update).await)
}

async fn put_me_inner(
    pool: &SqlitePool,
    c: &crate::Caller,
    up: &HqUpdate,
) -> Result<Response, sqlx::Error> {
    write_prof(pool, &c.user_id, up).await?;
    // Re-read instead of trusting what we wrote: the Python's refresh() is what
    // puts the *server* clock in `updatedAt`, and the level queries run after
    // the write, not inside it.
    let prof = load_prof(pool, &c.user_id).await?;
    let level = level_of(pool, &c.user_id).await?;
    let out = project(&Public::from_caller(c), prof.as_ref(), level, &c.user_id, Cos::default());
    Ok(Json(out).into_response())
}

async fn list_open(State(st): State<crate::AppState>, req: Request) -> Response {
    let c = req.extensions().get::<crate::Caller>().expect("caller set by middleware").clone();
    db500(list_open_inner(&st.pool, &c.user_id).await)
}

async fn list_open_inner(pool: &SqlitePool, viewer: &str) -> Result<Response, sqlx::Error> {
    // An INNER JOIN, so a user with no profile row can never appear, and
    // is_active = 0 hides an HQ here as well as on a direct visit. ORDER BY has
    // no tiebreaker on purpose: adding one would make this list stable where
    // the Python's is arbitrary. updated_at is TEXT 'YYYY-MM-DD HH:MM:SS', so
    // lexicographic DESC is newest first.
    let rows = sqlx::query(
        "SELECT u.id AS id, u.handle AS handle, u.display_name AS display_name,
                u.trainer_name AS trainer_name, u.avatar_url AS avatar_url,
                p.open AS open, p.look AS look, p.crew AS crew, p.updated_at AS updated_at
         FROM users u JOIN hq_profiles p ON p.user_id = u.id
         WHERE p.open = 1 AND u.is_active = 1
         ORDER BY p.updated_at DESC LIMIT ?1",
    )
    .bind(OPEN_LIST_MAX)
    .fetch_all(pool)
    .await?;

    let hqs: Vec<(Public, Prof)> = rows
        .iter()
        .map(|r| {
            (
                Public {
                    id: r.get("id"),
                    handle: r.get("handle"),
                    display_name: r.get("display_name"),
                    trainer_name: r.get("trainer_name"),
                    avatar_url: r.get("avatar_url"),
                },
                prof_from_row(r),
            )
        })
        .collect();

    // One pair of level queries and one cosmetics query for the whole page:
    // 100 rows would otherwise be 300 round trips.
    let ids: Vec<String> = hqs.iter().map(|(u, _)| u.id.clone()).collect();
    let levels = levels_for(pool, &ids).await?;
    let cos = equipped(pool, &ids).await?;

    // The viewer's own open HQ is in this list, with isYou true: it is not an
    // "other people's HQs" list.
    let out: Vec<HqOut> = hqs
        .iter()
        .map(|(u, p)| {
            let level = levels.get(&u.id).copied().unwrap_or(1);
            project(u, Some(p), level, viewer, cos.get(&u.id).cloned().unwrap_or_default())
        })
        .collect();
    Ok(Json(json!({ "hqs": out })).into_response())
}

async fn get_one(
    State(st): State<crate::AppState>,
    Path(user_id): Path<String>,
    req: Request,
) -> Response {
    let c = req.extensions().get::<crate::Caller>().expect("caller set by middleware").clone();
    // The Python truncates rather than validating, and a 42-character string
    // whose first 36 code points name an open user does return that user's HQ.
    // Reproducing the truncation keeps those requests answering 200.
    let uid: String = user_id.chars().take(36).collect();
    db500(get_one_inner(&st.pool, &c.user_id, &uid).await)
}

async fn get_one_inner(
    pool: &SqlitePool,
    viewer: &str,
    uid: &str,
) -> Result<Response, sqlx::Error> {
    let row = sqlx::query(
        "SELECT id, handle, display_name, trainer_name, avatar_url, is_active
         FROM users WHERE id = ?1",
    )
    .bind(uid)
    .fetch_optional(pool)
    .await?;
    let Some(r) = row else { return Ok(not_open()) };
    if r.get::<i64, _>("is_active") == 0 {
        return Ok(not_open());
    }
    let who = Public {
        id: r.get("id"),
        handle: r.get("handle"),
        display_name: r.get("display_name"),
        trainer_name: r.get("trainer_name"),
        avatar_url: r.get("avatar_url"),
    };

    let prof = load_prof(pool, uid).await?;
    // Your own HQ is always visible to you, open or not, row or no row.
    if uid != viewer && !prof.as_ref().is_some_and(|p| p.open) {
        return Ok(not_open());
    }
    let level = level_of(pool, uid).await?;
    let cos = equipped(pool, std::slice::from_ref(&who.id)).await?;
    let cos = cos.get(&who.id).cloned().unwrap_or_default();
    Ok(Json(project(&who, prof.as_ref(), level, viewer, cos)).into_response())
}

// --- projection -----------------------------------------------------------

/// The one outbound projection, shared by all four routes.
///
/// Three adjacent fields use three different empty-value conventions, and all
/// three are load-bearing for the client: displayName falls back to the handle,
/// trainerName is null when empty, avatarUrl is "" when empty.
fn project(
    u: &Public,
    p: Option<&Prof>,
    level: i64,
    viewer: &str,
    cos: Cos,
) -> HqOut {
    HqOut {
        user_id: u.id.clone(),
        handle: u.handle.clone(),
        display_name: if u.display_name.is_empty() {
            u.handle.clone()
        } else {
            u.display_name.clone()
        },
        trainer_name: if u.trainer_name.is_empty() { None } else { Some(u.trainer_name.clone()) },
        avatar_url: u.avatar_url.clone(),
        level,
        // Closed by default: an absent row reads as open:false.
        open: p.is_some_and(|p| p.open),
        look: look_out(p.map_or("{}", |p| p.look.as_str())),
        crew: crew_out(p.map_or("{}", |p| p.crew.as_str())),
        updated_at: p.and_then(|p| p.updated_at.as_deref()).map(iso),
        is_you: u.id == viewer,
        cos,
    }
}

/// SQLite's '2026-10-08 10:05:55' as Python's `datetime.isoformat()` renders
/// it: naive, no offset, second resolution. The client string-compares and
/// sorts these, so an RFC 3339 offset or a 'Z' would be a visible change.
fn iso(stored: &str) -> String {
    stored.replacen(' ', "T", 1)
}

/// Unknown keys in a stored `look` are filtered out, as the Python's
/// `model_fields` filter does -- the defence against a row an older or buggier
/// writer polluted. Deliberately lenient: a polluted row is cleaned, not a 500.
fn look_out(text: &str) -> LookOut {
    let v: Value = serde_json::from_str(text).unwrap_or(Value::Null);
    let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
    LookOut { paint: s("paint"), accent: s("accent"), sign: s("sign") }
}

/// Absent crew counts read as 0, so `crew` always carries all three keys.
fn crew_out(text: &str) -> CrewOut {
    let v: Value = serde_json::from_str(text).unwrap_or(Value::Null);
    let n = |k: &str| v.get(k).and_then(Value::as_i64).unwrap_or(0);
    CrewOut { working: n("working"), needs: n("needs"), idle: n("idle") }
}

/// `{slot: value}` from a stored `{slot: item id}`. Both filters are
/// load-bearing: an item id that is no longer in the catalog and an item filed
/// under the wrong slot are both silently dropped.
fn cos_from_slots(text: &str) -> Cos {
    let mut out: Vec<(String, String)> = Vec::new();
    let Ok(Slots(slots)) = serde_json::from_str::<Slots>(text) else { return Cos(out) };
    for (slot, id) in slots {
        let Some(id) = id.as_str() else { continue };
        if let Some(item) = CATALOG.iter().find(|e| e.0 == id && e.1 == slot.as_str()) {
            out.push((slot, item.2.to_string()));
        }
    }
    Cos(out)
}

// --- reads ----------------------------------------------------------------

fn prof_from_row(r: &sqlx::sqlite::SqliteRow) -> Prof {
    Prof {
        open: r.get::<i64, _>("open") != 0,
        look: r.get("look"),
        crew: r.get("crew"),
        updated_at: r.try_get::<Option<String>, _>("updated_at").ok().flatten(),
    }
}

async fn load_prof(pool: &SqlitePool, uid: &str) -> Result<Option<Prof>, sqlx::Error> {
    let row = sqlx::query("SELECT open, look, crew, updated_at FROM hq_profiles WHERE user_id = ?1")
        .bind(uid)
        .fetch_optional(pool)
        .await?;
    Ok(row.as_ref().map(prof_from_row))
}

/// `(?1,?2,...)` -- sqlx has no array binding for SQLite, so an IN clause is
/// built and bound one id at a time, as elsewhere in this codebase.
fn placeholders(n: usize) -> String {
    (1..=n).map(|i| format!("?{i}")).collect::<Vec<_>>().join(",")
}

async fn equipped(pool: &SqlitePool, ids: &[String]) -> Result<HashMap<String, Cos>, sqlx::Error> {
    let mut out: HashMap<String, Cos> = HashMap::new();
    if ids.is_empty() {
        return Ok(out);
    }
    let sql = format!(
        "SELECT user_id, slots FROM equipped_cosmetics WHERE user_id IN ({})",
        placeholders(ids.len())
    );
    let mut q = sqlx::query(&sql);
    for id in ids {
        q = q.bind(id.as_str());
    }
    for r in q.fetch_all(pool).await? {
        let slots: String = r.get("slots");
        out.insert(r.get("user_id"), cos_from_slots(&slots));
    }
    Ok(out)
}

fn xp_for_result(place: i64, players: i64) -> i64 {
    // Finishing is worth 20; winning a game with others 30 more, 2nd 15, 3rd 8.
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

/// Game XP per user from `(user_id, place, players, UTC day)` rows.
///
/// The cap applies to each day's bucket, not to the total, so a heavy day does
/// not eat a quiet one. The Python applies it incrementally per row; every row
/// is worth something positive, so capping the finished bucket is the same sum.
fn game_xp_from_rows(rows: &[(String, i64, i64, String)]) -> HashMap<String, i64> {
    let mut per_day: HashMap<(&str, &str), i64> = HashMap::new();
    for (uid, place, players, day) in rows {
        let bucket = per_day.entry((uid.as_str(), day.as_str())).or_default();
        *bucket = GAME_XP_DAY_CAP.min(*bucket + xp_for_result(*place, *players));
    }
    let mut out: HashMap<String, i64> = HashMap::new();
    for ((uid, _day), xp) in per_day {
        *out.entry(uid.to_string()).or_default() += xp;
    }
    out
}

/// One HQ level per user: session XP (all time, no date window) plus game XP.
///
/// Two queries for any number of users, which is why the open list batches its
/// ids instead of asking per row.
async fn levels_for(pool: &SqlitePool, ids: &[String]) -> Result<HashMap<String, i64>, sqlx::Error> {
    let mut xp: HashMap<String, i64> = ids.iter().map(|i| (i.clone(), 0)).collect();
    if ids.is_empty() {
        return Ok(xp);
    }
    let ph = placeholders(ids.len());

    let sql = format!(
        "SELECT user_id, COALESCE(SUM(prompts),0) AS p, COALESCE(SUM(tools),0) AS t,
                COALESCE(SUM(artifacts),0) AS a
         FROM daily_stats WHERE user_id IN ({ph}) GROUP BY user_id"
    );
    let mut q = sqlx::query(&sql);
    for id in ids {
        q = q.bind(id.as_str());
    }
    for r in q.fetch_all(pool).await? {
        let counted = crate::scoring::xp_from_counts(r.get("p"), r.get("t"), r.get("a"));
        xp.insert(r.get("user_id"), counted);
    }

    // `at` is TEXT 'YYYY-MM-DD HH:MM:SS' in UTC; the bucket key is its UTC
    // date. A local date would move XP across the cap boundary.
    let sql = format!(
        "SELECT user_id, place, players, substr(at, 1, 10) AS day
         FROM game_results WHERE user_id IN ({ph})"
    );
    let mut q = sqlx::query(&sql);
    for id in ids {
        q = q.bind(id.as_str());
    }
    let rows: Vec<(String, i64, i64, String)> = q
        .fetch_all(pool)
        .await?
        .iter()
        .map(|r| (r.get("user_id"), r.get("place"), r.get("players"), r.get("day")))
        .collect();
    for (uid, gained) in game_xp_from_rows(&rows) {
        if let Some(total) = xp.get_mut(&uid) {
            *total += gained;
        }
    }

    Ok(xp.into_iter().map(|(u, total)| (u, crate::scoring::derive_level(total).0)).collect())
}

async fn level_of(pool: &SqlitePool, uid: &str) -> Result<i64, sqlx::Error> {
    let ids = [uid.to_string()];
    let levels = levels_for(pool, &ids).await?;
    Ok(levels.get(uid).copied().unwrap_or(1))
}

// --- the write ------------------------------------------------------------

/// Python's `json.dumps` defaults, which is what SQLAlchemy puts in a JSON
/// column: `", "` between entries, `": "` after a key, and non-ASCII escaped
/// (`ensure_ascii=True`). While both backends write the same file, a diff of
/// these rows should show nothing.
fn py_json_str(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c if (c as u32) < 0x80 => out.push(c),
            c => {
                // ensure_ascii: astral characters become a UTF-16 surrogate
                // pair, exactly as CPython's encoder emits them.
                let mut buf = [0u16; 2];
                for unit in c.encode_utf16(&mut buf) {
                    out.push_str(&format!("\\u{unit:04x}"));
                }
            }
        }
    }
    out.push('"');
}

/// `exclude_none`: a null or omitted field is dropped, so `{"paint": null}`
/// stores `{}`. Key order is the model's field order, not the request's.
fn dump_look(look: &LookIn) -> String {
    let mut out = String::from("{");
    for (key, value) in [("paint", &look.paint), ("accent", &look.accent), ("sign", &look.sign)] {
        if let Some(v) = value {
            if out.len() > 1 {
                out.push_str(", ");
            }
            py_json_str(key, &mut out);
            out.push_str(": ");
            py_json_str(v, &mut out);
        }
    }
    out.push('}');
    out
}

/// No `exclude_none` on the way in, so all three counts are always stored and
/// an omitted one becomes 0.
fn dump_crew(crew: &CrewIn) -> String {
    format!(
        "{{\"working\": {}, \"needs\": {}, \"idle\": {}}}",
        crew.working, crew.needs, crew.idle
    )
}

/// Compare two stored JSON columns the way SQLAlchemy compares the loaded dict
/// with the new one: by value. A text comparison would see every row the Python
/// wrote as changed, because its `json.dumps` spaces entries out and ours does
/// not have to.
fn json_eq(a: &str, b: &str) -> bool {
    match (serde_json::from_str::<Value>(a), serde_json::from_str::<Value>(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => a == b,
    }
}

/// The only write in this module: one row, no transaction, idempotent.
///
/// `look` and `crew` are replaced wholesale, never merged -- a present `look`
/// resets all three of its fields to exactly what it carries.
async fn write_prof(pool: &SqlitePool, uid: &str, up: &HqUpdate) -> Result<(), sqlx::Error> {
    let cur = load_prof(pool, uid).await?;
    // A first PUT creates the row from the model's defaults and then applies
    // only what the body supplied: an omitted field never reads off the body.
    let (open0, look0, crew0) = match cur.as_ref() {
        Some(p) => (p.open, p.look.as_str(), p.crew.as_str()),
        None => (false, "{}", "{}"),
    };
    let open = up.open.unwrap_or(open0);
    let look = match &up.look {
        Some(l) => dump_look(l),
        None => look0.to_string(),
    };
    let crew = match &up.crew {
        Some(c) => dump_crew(c),
        None => crew0.to_string(),
    };

    if cur.is_none() {
        // look and crew are NOT NULL, so '{}' has to be supplied; updated_at
        // comes from the column's CURRENT_TIMESTAMP default, as in the Python.
        sqlx::query("INSERT INTO hq_profiles (user_id, open, look, crew) VALUES (?1,?2,?3,?4)")
            .bind(uid)
            .bind(open)
            .bind(look)
            .bind(crew)
            .execute(pool)
            .await?;
        return Ok(());
    }

    // updated_at must only move when a value actually changed. SQLAlchemy's
    // onupdate fires only if the flush emits an UPDATE, and it value-compares
    // the attributes first -- so PUT {} and a repeated PUT leave the timestamp
    // alone. That matters because GET /v1/hq/open is ordered by updated_at: an
    // unconditional touch would reshuffle the open list on every heartbeat PUT.
    if open == open0 && json_eq(&look, look0) && json_eq(&crew, crew0) {
        return Ok(());
    }
    sqlx::query(
        "UPDATE hq_profiles SET open = ?1, look = ?2, crew = ?3, updated_at = datetime('now')
         WHERE user_id = ?4",
    )
    .bind(open)
    .bind(look)
    .bind(crew)
    .bind(uid)
    .execute(pool)
    .await?;
    Ok(())
}

// --- inbound validation ---------------------------------------------------

/// A pydantic rejection as `(type, msg)`, worded exactly as the client sees it.
type Reject = (&'static str, &'static str);

const MISSING_BODY: Reject = ("missing", "Field required");
const JSON_INVALID: Reject = ("json_invalid", "JSON decode error");
const MODEL_ATTRS: Reject =
    ("model_attributes_type", "Input should be a valid dictionary or object to extract fields from");
const EXTRA_FORBIDDEN: Reject = ("extra_forbidden", "Extra inputs are not permitted");
const BOOL_TYPE: Reject = ("bool_type", "Input should be a valid boolean");
const BOOL_PARSING: Reject =
    ("bool_parsing", "Input should be a valid boolean, unable to interpret input");
const INT_TYPE: Reject = ("int_type", "Input should be a valid integer");
const INT_PARSING: Reject =
    ("int_parsing", "Input should be a valid integer, unable to parse string as an integer");
const INT_FROM_FLOAT: Reject =
    ("int_from_float", "Input should be a valid integer, got a number with a fractional part");
const GE_ZERO: Reject = ("greater_than_equal", "Input should be greater than or equal to 0");
const LE_MAX: Reject = ("less_than_equal", "Input should be less than or equal to 64");
const STR_TYPE: Reject = ("string_type", "Input should be a valid string");
const HEX_MISMATCH: Reject =
    ("string_pattern_mismatch", "String should match pattern '^#[0-9a-fA-F]{6}$'");
const SIGN_TOO_LONG: Reject = ("string_too_long", "String should have at most 18 characters");
const SIGN_CHARSET: Reject = (
    "value_error",
    "Value error, the sign takes letters, numbers, spaces and . , ' & ! ? -",
);

#[derive(Debug)]
struct VErr {
    loc: Vec<Value>,
    kind: &'static str,
    msg: &'static str,
}

fn at(loc: &[&str], r: Reject) -> VErr {
    VErr { loc: loc.iter().map(|s| json!(s)).collect(), kind: r.0, msg: r.1 }
}

/// `detail` is a LIST for 422 and a string for 401/403/404/405, and each entry
/// carries exactly loc/msg/type: `_scrub_422` in the Python drops pydantic's
/// `input` and `url` so a rejected cwd or project name never echoes back.
fn unprocessable(errs: &[VErr]) -> Response {
    let detail: Vec<Value> = errs
        .iter()
        .map(|e| json!({"loc": e.loc, "msg": e.msg, "type": e.kind}))
        .collect();
    (StatusCode::UNPROCESSABLE_ENTITY, Json(json!({ "detail": detail }))).into_response()
}

fn read_update(bytes: &[u8]) -> Result<HqUpdate, Vec<VErr>> {
    // FastAPI reads an empty body as no body at all, and the required model then
    // reports itself missing rather than malformed. axum's Json extractor would
    // have answered 400 here and 415 on a wrong content type, which is why this
    // takes the raw bytes instead.
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Err(vec![at(&["body"], MISSING_BODY)]);
    }
    let body: Value = match serde_json::from_slice(bytes) {
        Ok(v) => v,
        Err(e) => {
            let pos = json!(e.column().saturating_sub(1));
            return Err(vec![VErr {
                loc: vec![json!("body"), pos],
                kind: JSON_INVALID.0,
                msg: JSON_INVALID.1,
            }]);
        }
    };
    parse_update(&body)
}

/// Validate `HqUpdate`, reporting every error pydantic would, in its passes:
/// the model's own fields in declaration order, then whatever is left over.
///
/// One known difference, and only when a body earns several errors at once:
/// the leftover keys come out sorted here (serde_json's object is a BTreeMap)
/// where pydantic walks them in body order. Each entry is identical; a
/// multi-error `detail` list can be ordered differently.
fn parse_update(body: &Value) -> Result<HqUpdate, Vec<VErr>> {
    if body.is_null() {
        return Err(vec![at(&["body"], MISSING_BODY)]);
    }
    let Some(obj) = body.as_object() else {
        return Err(vec![at(&["body"], MODEL_ATTRS)]);
    };

    let mut errs: Vec<VErr> = Vec::new();
    let mut up = HqUpdate::default();
    match obj.get("open") {
        None | Some(Value::Null) => {}
        Some(v) => match coerce_bool(v) {
            Ok(b) => up.open = Some(b),
            Err(r) => errs.push(at(&["body", "open"], r)),
        },
    }
    match obj.get("look") {
        None | Some(Value::Null) => {}
        Some(v) => up.look = Some(parse_look(v, &mut errs)),
    }
    match obj.get("crew") {
        None | Some(Value::Null) => {}
        Some(v) => up.crew = Some(parse_crew(v, &mut errs)),
    }
    // The inbound half of the privacy boundary: there is no `level`, `userId`,
    // `cos` or `handle` field, and an unknown key is refused rather than
    // ignored, so nothing from a transcript can arrive by accident.
    for k in obj.keys() {
        if !matches!(k.as_str(), "open" | "look" | "crew") {
            errs.push(at(&["body", k.as_str()], EXTRA_FORBIDDEN));
        }
    }
    if errs.is_empty() {
        Ok(up)
    } else {
        Err(errs)
    }
}

fn parse_look(v: &Value, errs: &mut Vec<VErr>) -> LookIn {
    let Some(obj) = v.as_object() else {
        errs.push(at(&["body", "look"], MODEL_ATTRS));
        return LookIn::default();
    };
    let mut look = LookIn::default();

    for key in ["paint", "accent"] {
        match obj.get(key) {
            None | Some(Value::Null) => {}
            Some(raw) => match coerce_str(raw) {
                Err(r) => errs.push(at(&["body", "look", key], r)),
                Ok(s) if !is_hex_colour(s) => {
                    errs.push(at(&["body", "look", key], HEX_MISMATCH));
                }
                Ok(s) => {
                    let s = Some(s.to_string());
                    if key == "paint" {
                        look.paint = s;
                    } else {
                        look.accent = s;
                    }
                }
            },
        }
    }
    match obj.get("sign") {
        None | Some(Value::Null) => {}
        Some(raw) => match coerce_str(raw).and_then(clean_sign) {
            Ok(s) => look.sign = s,
            Err(r) => errs.push(at(&["body", "look", "sign"], r)),
        },
    }
    for k in obj.keys() {
        if !matches!(k.as_str(), "paint" | "accent" | "sign") {
            errs.push(at(&["body", "look", k.as_str()], EXTRA_FORBIDDEN));
        }
    }
    look
}

fn parse_crew(v: &Value, errs: &mut Vec<VErr>) -> CrewIn {
    let Some(obj) = v.as_object() else {
        errs.push(at(&["body", "crew"], MODEL_ATTRS));
        return CrewIn::default();
    };
    let mut crew = CrewIn::default();
    {
        let mut read = |key: &'static str| -> Option<i64> {
            let raw = obj.get(key)?;
            match coerce_int(raw) {
                Ok(n) if n < 0 => {
                    errs.push(at(&["body", "crew", key], GE_ZERO));
                    None
                }
                Ok(n) if n > CREW_MAX => {
                    errs.push(at(&["body", "crew", key], LE_MAX));
                    None
                }
                Ok(n) => Some(n),
                Err(r) => {
                    errs.push(at(&["body", "crew", key], r));
                    None
                }
            }
        };
        if let Some(n) = read("working") {
            crew.working = n;
        }
        if let Some(n) = read("needs") {
            crew.needs = n;
        }
        if let Some(n) = read("idle") {
            crew.idle = n;
        }
    }
    for k in obj.keys() {
        if !matches!(k.as_str(), "working" | "needs" | "idle") {
            errs.push(at(&["body", "crew", k.as_str()], EXTRA_FORBIDDEN));
        }
    }
    crew
}

/// Pydantic's lax mode coerces booleans, and the Python's wire contract
/// includes that: "yes", "off", 0 and 1 are all booleans, and only something
/// uninterpretable (2, say) is a 422.
fn coerce_bool(v: &Value) -> Result<bool, Reject> {
    match v {
        Value::Bool(b) => Ok(*b),
        Value::Number(n) => {
            let x = n.as_f64().unwrap_or(f64::NAN);
            if x == 0.0 {
                Ok(false)
            } else if x == 1.0 {
                Ok(true)
            } else {
                Err(BOOL_PARSING)
            }
        }
        Value::String(s) => match s.trim().to_ascii_lowercase().as_str() {
            "0" | "off" | "f" | "false" | "n" | "no" => Ok(false),
            "1" | "on" | "t" | "true" | "y" | "yes" => Ok(true),
            _ => Err(BOOL_PARSING),
        },
        _ => Err(BOOL_TYPE),
    }
}

/// Lax integers too: 1.0 and "3" are 3, but 1.5 is a 422 rather than a 1.
fn coerce_int(v: &Value) -> Result<i64, Reject> {
    match v {
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                return Ok(i);
            }
            whole(n.as_f64().unwrap_or(f64::NAN)).ok_or(INT_FROM_FLOAT)
        }
        Value::String(s) => {
            let t = s.trim();
            if let Ok(i) = t.parse::<i64>() {
                return Ok(i);
            }
            t.parse::<f64>().ok().and_then(whole).ok_or(INT_PARSING)
        }
        _ => Err(INT_TYPE),
    }
}

fn whole(x: f64) -> Option<i64> {
    if x.fract() == 0.0 && x.abs() < 9e18 {
        Some(x as i64)
    } else {
        None
    }
}

/// Pydantic does not turn a number into a string in lax mode either.
fn coerce_str(v: &Value) -> Result<&str, Reject> {
    v.as_str().ok_or(STR_TYPE)
}

/// `^#[0-9a-fA-F]{6}$` by hand (no regex crate here, and none is needed). Note
/// that pydantic's regex engine does not tolerate a trailing newline, so
/// "#224466\n" is a 422 -- and the byte-length check below rejects it too.
fn is_hex_colour(s: &str) -> bool {
    let Some(body) = s.strip_prefix('#') else { return false };
    body.len() == 6 && body.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The stored sign, or the rejection.
///
/// `Ok(None)` means it collapsed to nothing, which is a clear and not an error:
/// the Python's `v or None` plus `exclude_none` drops it from the stored JSON,
/// so sign:"   " reads back as null.
fn clean_sign(raw: &str) -> Result<Option<String>, Reject> {
    // The length cap counts CODE POINTS on the RAW string, before collapsing:
    // 18 letters plus a trailing space is 19 and is refused even though it
    // would have collapsed to 18.
    if raw.chars().count() > SIGN_MAX {
        return Err(SIGN_TOO_LONG);
    }
    // ' '.join(v.split()): any run of whitespace becomes one space, and the
    // ends are stripped. A tab is already a space by the time the charset is
    // checked, so it never trips the allowlist.
    let sign = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    // Python's str.isalnum() is unicode-aware -- "héllo·wörld" is a fine sign --
    // and char::is_alphanumeric is its equivalent. An ASCII-only check would
    // reject signs the Python accepts.
    if !sign.chars().all(|c| c.is_alphanumeric() || SIGN_PUNCT.contains(c)) {
        return Err(SIGN_CHARSET);
    }
    Ok(if sign.is_empty() { None } else { Some(sign) })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn update(j: &str) -> Result<HqUpdate, Vec<VErr>> {
        parse_update(&serde_json::from_str(j).expect("test json"))
    }

    fn first(j: &str) -> (Vec<Value>, &'static str, &'static str) {
        let errs = update(j).expect_err("expected a rejection");
        let e = &errs[0];
        (e.loc.clone(), e.msg, e.kind)
    }

    #[test]
    fn an_empty_body_changes_nothing() {
        assert_eq!(update("{}").expect("valid"), HqUpdate::default());
        // null and absent are the same thing for all three fields.
        let up = update(r##"{"open":null,"look":null,"crew":null}"##).expect("valid");
        assert_eq!(up, HqUpdate::default());
    }

    #[test]
    fn a_client_cannot_send_a_level() {
        assert_eq!(
            first(r##"{"level":99}"##),
            (vec![json!("body"), json!("level")], "Extra inputs are not permitted", "extra_forbidden")
        );
    }

    #[test]
    fn the_privacy_boundary_rejects_transcript_keys() {
        assert_eq!(first(r##"{"open":true,"title":"my secret project"}"##).0,
                   vec![json!("body"), json!("title")]);
        assert_eq!(first(r##"{"look":{"paint":"#224466","cwd":"/Users/me"}}"##).0,
                   vec![json!("body"), json!("look"), json!("cwd")]);
        assert_eq!(first(r##"{"crew":{"working":1,"sessionId":"abc"}}"##).0,
                   vec![json!("body"), json!("crew"), json!("sessionId")]);
        for key in ["userId", "cos", "handle", "updatedAt", "isYou"] {
            assert!(update(&format!(r##"{{"{key}":"x"}}"##)).is_err(), "{key} should be refused");
        }
    }

    #[test]
    fn hex_colours_must_be_exact() {
        assert!(is_hex_colour("#224466") && is_hex_colour("#AbCdEf"));
        assert!(!is_hex_colour("red") && !is_hex_colour("#22446") && !is_hex_colour("224466"));
        // pydantic's regex engine does not tolerate a trailing newline.
        assert!(!is_hex_colour("#224466\n"));
        assert_eq!(
            first(r##"{"look":{"paint":"red"}}"##),
            (
                vec![json!("body"), json!("look"), json!("paint")],
                "String should match pattern '^#[0-9a-fA-F]{6}$'",
                "string_pattern_mismatch"
            )
        );
        assert_eq!(first(r##"{"look":{"paint":7}}"##).2, "string_type");
    }

    #[test]
    fn signs_collapse_whitespace_within_eighteen_code_points() {
        assert_eq!(clean_sign("Ann's  HQ"), Ok(Some("Ann's HQ".into())));
        assert_eq!(clean_sign("ab\tcd"), Ok(Some("ab cd".into())));
        // Whitespace only is a clear, not a 422.
        assert_eq!(clean_sign("   "), Ok(None));
        // 18 letters and a trailing space is 19 RAW code points, so it is
        // refused even though it would have collapsed to 18.
        assert_eq!(clean_sign(&format!("{} ", "a".repeat(18))), Err(SIGN_TOO_LONG));
        assert_eq!(clean_sign(&"a".repeat(18)), Ok(Some("a".repeat(18))));
        // Unicode letters and the middle dot are fine; '%' and '<' are not.
        assert_eq!(clean_sign("héllo·wörld"), Ok(Some("héllo·wörld".into())));
        assert_eq!(clean_sign("50% off"), Err(SIGN_CHARSET));
        assert_eq!(clean_sign("<script>"), Err(SIGN_CHARSET));
        assert_eq!(
            first(r##"{"look":{"sign":"<script>"}}"##),
            (
                vec![json!("body"), json!("look"), json!("sign")],
                "Value error, the sign takes letters, numbers, spaces and . , ' & ! ? -",
                "value_error"
            )
        );
        assert_eq!(first(&format!(r##"{{"look":{{"sign":"{}"}}}}"##, "x".repeat(19))).2,
                   "string_too_long");
    }

    #[test]
    fn crew_counts_are_capped_at_sixty_four() {
        let up = update(r##"{"crew":{"working":3,"needs":1,"idle":2}}"##).expect("valid");
        assert_eq!(up.crew, Some(CrewIn { working: 3, needs: 1, idle: 2 }));
        assert_eq!(first(r##"{"crew":{"working":500}}"##).2, "less_than_equal");
        assert_eq!(first(r##"{"crew":{"working":-1}}"##).2, "greater_than_equal");
        assert_eq!(first(r##"{"crew":{"working":1.5}}"##).2, "int_from_float");
        assert_eq!(first(r##"{"crew":{"working":true}}"##).2, "int_type");
        // Lax coercion, as pydantic does it.
        assert_eq!(update(r##"{"crew":{"working":1.0}}"##).expect("valid").crew,
                   Some(CrewIn { working: 1, needs: 0, idle: 0 }));
        assert_eq!(update(r##"{"crew":{"working":"3"}}"##).expect("valid").crew,
                   Some(CrewIn { working: 3, needs: 0, idle: 0 }));
    }

    #[test]
    fn booleans_are_coerced_the_way_pydantic_coerces_them() {
        assert_eq!(update(r##"{"open":"yes"}"##).expect("valid").open, Some(true));
        assert_eq!(update(r##"{"open":"off"}"##).expect("valid").open, Some(false));
        assert_eq!(update(r##"{"open":1}"##).expect("valid").open, Some(true));
        assert_eq!(first(r##"{"open":2}"##).2, "bool_parsing");
        assert_eq!(first(r##"{"open":[]}"##).2, "bool_type");
    }

    #[test]
    fn a_body_that_is_not_an_object_is_refused_like_fastapi_refuses_it() {
        assert_eq!(first("[]").2, "model_attributes_type");
        assert_eq!(first("null").2, "missing");
        assert_eq!(first(r##"{"look":[]}"##).0, vec![json!("body"), json!("look")]);
        let errs = read_update(b"").expect_err("empty body is missing");
        assert_eq!((errs[0].kind, errs[0].msg), ("missing", "Field required"));
        let errs = read_update(b"{oops").expect_err("malformed json");
        assert_eq!(errs[0].kind, "json_invalid");
    }

    #[test]
    fn stored_json_matches_pythons_json_dumps() {
        // ', ' and ': ' separators, declaration order, nulls absent.
        let look = LookIn { paint: Some("#224466".into()), accent: None, sign: Some("Hi".into()) };
        assert_eq!(dump_look(&look), r##"{"paint": "#224466", "sign": "Hi"}"##);
        // exclude_none: an explicit null stores nothing at all.
        let cleared = update(r##"{"look":{"paint":null}}"##).expect("valid").look.expect("look");
        assert_eq!(dump_look(&cleared), "{}");
        // A present look replaces wholesale: paint and sign are cleared here.
        let accent = update(r##"{"look":{"accent":"#ff0000"}}"##).expect("valid").look.expect("look");
        assert_eq!(dump_look(&accent), r##"{"accent": "#ff0000"}"##);
        // All three counts, always, in working/needs/idle order.
        let crew = update(r##"{"crew":{"needs":2}}"##).expect("valid").crew.expect("crew");
        assert_eq!(dump_crew(&crew), r##"{"working": 0, "needs": 2, "idle": 0}"##);
        // ensure_ascii, as CPython's encoder writes it.
        let sign = LookIn { paint: None, accent: None, sign: Some("héllo·wörld".into()) };
        assert_eq!(dump_look(&sign), "{\"sign\": \"h\\u00e9llo\\u00b7w\\u00f6rld\"}");
    }

    #[test]
    fn a_repeated_put_does_not_move_updated_at() {
        // The guard compares by value, so the Python's spacing is not a change.
        assert!(json_eq(r##"{"working": 1, "needs": 0, "idle": 2}"##,
                        r##"{"working":1,"needs":0,"idle":2}"##));
        assert!(json_eq(r##"{"sign": "Hi", "paint": "#224466"}"##,
                        r##"{"paint": "#224466", "sign": "Hi"}"##));
        assert!(!json_eq(r##"{"working": 1}"##, r##"{"working": 2}"##));
        // A row an older writer polluted really is different, and is rewritten.
        assert!(!json_eq(r##"{"paint": "#224466"}"##, r##"{"paint": "#224466", "junk": 1}"##));
    }

    #[test]
    fn look_and_crew_always_carry_their_keys_on_the_way_out() {
        assert_eq!(look_out("{}"), LookOut::default());
        assert_eq!(crew_out("{}"), CrewOut::default());
        // Unknown keys in a stored row are filtered out rather than echoed.
        let look = look_out(r##"{"paint": "#224466", "cwd": "/Users/me"}"##);
        assert_eq!(look, LookOut { paint: Some("#224466".into()), accent: None, sign: None });
        // An unreadable row is cleaned, not a 500.
        assert_eq!(look_out("not json"), LookOut::default());
        assert_eq!(crew_out(r##"{"working": 3, "needs": 1, "idle": 2}"##),
                   CrewOut { working: 3, needs: 1, idle: 2 });
    }

    #[test]
    fn cos_drops_stale_ids_and_items_in_the_wrong_slot() {
        let cos = cos_from_slots(
            r##"{"kart":"k-neon","decor":"d-flags","frame":"bogus","kart2":"k-gold"}"##,
        );
        // Key order is the row's, not alphabetical: the Python emits the stored
        // dict's order and "decor" would otherwise sort ahead of "kart".
        assert_eq!(
            serde_json::to_string(&cos).expect("serialises"),
            r##"{"kart":"#39ff88","decor":"flags"}"##
        );
        assert_eq!(cos_from_slots("{}"), Cos::default());
        assert_eq!(cos_from_slots(""), Cos::default());
        assert_eq!(cos_from_slots("[]"), Cos::default());
    }

    #[test]
    fn game_xp_is_capped_per_day_not_per_total() {
        let day = |d: &str, n: usize| -> Vec<(String, i64, i64, String)> {
            (0..n).map(|_| ("ann".to_string(), 1, 4, d.to_string())).collect()
        };
        // A win with others is 50; eight of them in one day is 400, capped.
        let mut rows = day("2026-10-08", 20);
        assert_eq!(game_xp_from_rows(&rows).get("ann"), Some(&GAME_XP_DAY_CAP));
        // A second day is its own bucket.
        rows.extend(day("2026-10-07", 20));
        assert_eq!(game_xp_from_rows(&rows).get("ann"), Some(&(2 * GAME_XP_DAY_CAP)));
        assert!(game_xp_from_rows(&[]).is_empty());
        // Finishing alone is worth 20, with no placing bonus.
        assert_eq!(xp_for_result(1, 1), 20);
        assert_eq!(xp_for_result(1, 2), 50);
        assert_eq!(xp_for_result(2, 4), 35);
        assert_eq!(xp_for_result(3, 4), 28);
        assert_eq!(xp_for_result(4, 4), 20);
    }

    #[test]
    fn a_fresh_hq_serialises_byte_for_byte_like_the_python() {
        let who = Public {
            id: "ca72".into(),
            handle: "ann".into(),
            display_name: "Ann".into(),
            trainer_name: String::new(),
            avatar_url: String::new(),
        };
        let out = project(&who, None, 1, "ca72", Cos::default());
        assert_eq!(
            serde_json::to_string(&out).expect("serialises"),
            r##"{"userId":"ca72","handle":"ann","displayName":"Ann","trainerName":null,"avatarUrl":"","level":1,"open":false,"look":{"paint":null,"accent":null,"sign":null},"crew":{"working":0,"needs":0,"idle":0},"updatedAt":null,"isYou":true,"cos":{}}"##
        );
    }

    #[test]
    fn a_visit_carries_a_naive_second_resolution_timestamp() {
        let who = Public {
            id: "ca72".into(),
            handle: "ann".into(),
            display_name: String::new(), // falls back to the handle
            trainer_name: String::new(), // null, not ""
            avatar_url: String::new(),   // "", not null
        };
        let prof = Prof {
            open: true,
            look: r##"{"paint": "#224466", "sign": "Ann's HQ"}"##.into(),
            crew: r##"{"working": 3, "needs": 1, "idle": 2}"##.into(),
            updated_at: Some("2026-10-08 10:05:55".into()),
        };
        let cos = cos_from_slots(r##"{"kart":"k-neon"}"##);
        let out = project(&who, Some(&prof), 1, "other", cos);
        let body = serde_json::to_string(&out).expect("serialises");
        assert!(body.contains(r##""displayName":"ann""##), "{body}");
        assert!(body.contains(r##""trainerName":null,"avatarUrl":"""##), "{body}");
        // No offset, no 'Z', no microseconds: the client sorts these as strings.
        assert!(body.contains(r##""updatedAt":"2026-10-08T10:05:55""##), "{body}");
        assert!(body.contains(r##""isYou":false"##), "{body}");
        assert!(body.contains(r##""cos":{"kart":"#39ff88"}"##), "{body}");
    }

    #[test]
    fn the_in_clause_is_built_the_way_the_rest_of_the_codebase_builds_one() {
        assert_eq!(placeholders(1), "?1");
        assert_eq!(placeholders(3), "?1,?2,?3");
    }
}
