//! HQ 2.1 crews, ported from `app/crews.py` and `app/routes/crews.py`.
//!
//! Create a crew (name, short tag, banner colour), share its private invite
//! code, and climb the crew board together. A crew's XP is its members'
//! combined XP -- session XP plus game XP, scored exactly as progression does
//! -- one crew per person, up to `MAX_MEMBERS`. When the owner leaves, the
//! longest-standing remaining member takes over; the last one out closes the
//! crew.
//!
//! PRIVACY: the invite code is the one field here that is not public. It goes
//! out only to someone who is already inside the crew (`/mine`) or who just
//! got in (`/create`, `/join`), which is why `CrewOut.code` is an `Option` that
//! is *omitted* rather than nulled -- the Python asserts `"code" not in
//! board[0]`, and an explicit `null` would already be a leak of shape. The
//! lookup in `/join` is gated on the code's shape before any query runs, and a
//! malformed code and an unknown one return the identical 404, so nothing in
//! this module tells a prober which codes exist.

use axum::{
    extract::{Request, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::Serialize;
use serde_json::{json, Map, Value};
use sqlx::{Row, SqliteConnection, SqlitePool};
use std::collections::HashMap;

use crate::scoring::{derive_level, xp_from_counts};

/// A crew tops out at 20 rows in `crew_members`: the guard is `count >= 20` on
/// the count taken *before* the insert.
const MAX_MEMBERS: i64 = 20;
/// The crew board is capped, and `rank` is assigned over the whole sorted list
/// before the cap is applied.
const BOARD_SIZE: usize = 50;
/// Game XP is capped per (user, calendar day), and the capped per-day figures
/// are then summed -- not one cap over a whole history.
const GAME_XP_DAY_CAP: i64 = 400;

// Pydantic `max_length` on the request bodies. These are *not* the business
// rules below; see `fits`.
const MAX_NAME_CHARS: usize = 40;
const MAX_TAG_CHARS: usize = 4;
const MAX_COLOR_CHARS: usize = 7;
const MAX_CODE_CHARS: usize = 12;

/// Three short strings. Anything near this is already a client bug, so there is
/// no reason to buffer megabytes for it.
const MAX_BODY_BYTES: usize = 64 * 1024;

/// Reproduced character for character, trailing " -" included: the client shows
/// it verbatim as the field's help text.
const NAME_HELP: &str = "a crew name is 3 to 32 letters, numbers, spaces and . ' & ! -";
const TAG_HELP: &str = "a tag is 2 to 4 capital letters or numbers";
/// British, and lower-case c. Not "color".
const COLOR_HELP: &str = "pick a banner colour";
/// The same answer for a malformed code and for a well-formed unknown one.
const NO_SUCH_CODE: &str = "no crew has that invite code";
const ALREADY_CREWED: &str = "leave your crew first";

/// `/mine` and `/create` and `/join` reveal the invite code; the board does not.
/// `main.rs` puts the whole guarded router behind `require_device`, so this adds
/// no layer of its own.
///
/// All four are literal paths. `/v1/crews` is the bare prefix with no trailing
/// slash (`@router.get("")`), and there is no `GET /v1/crews/{id}` in this
/// group, so nothing shadows `/v1/crews/mine`.
pub fn routes() -> Router<crate::AppState> {
    Router::new()
        .route("/v1/crews/mine", get(mine))
        .route("/v1/crews", get(board))
        .route("/v1/crews/create", post(create))
        .route("/v1/crews/join", post(join))
        .route("/v1/crews/leave", post(leave))
}

fn err(code: StatusCode, msg: &str) -> Response {
    (code, Json(json!({ "detail": msg }))).into_response()
}

/// One Pydantic error entry, scrubbed to the three keys `app/main.py`'s
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
/// `err` produces.
///
/// Which shape a 422 takes is not a style choice: a Pydantic failure -- a
/// missing field, a wrong type, an over-long value, an extra key -- never
/// reaches a route, so `_scrub_422` answers it with this array. Every explicit
/// `raise HTTPException` in `app/crews.py` is a 400, a 404 or a 409 and keeps
/// the single string, which is why `err` stays exactly as it was.
fn invalid(errs: Vec<VErr>) -> Response {
    (StatusCode::UNPROCESSABLE_ENTITY, Json(json!({ "detail": errs }))).into_response()
}

// --- wire shapes ----------------------------------------------------------
//
// Structs rather than `json!` literals because field order *is* the emitted
// key order and the Python's dict insertion order pins it. `serde_json::Value`
// would not do: its object is a BTreeMap and would re-sort the keys.

/// Not `Deserialize`: see `create_body`, which validates by hand so the 422 can
/// carry Pydantic's entries instead of serde's error text.
#[derive(Debug)]
struct CreateBody {
    name: String,
    tag: String,
    color: String,
}

/// Exactly these five keys, in this order. No avatar, no trainer name, no XP:
/// a crew's member list is a roster, not a leaderboard.
#[derive(Debug, Serialize)]
struct MemberOut {
    #[serde(rename = "userId")]
    user_id: String,
    handle: String,
    #[serde(rename = "displayName")]
    display_name: String,
    level: i64,
    owner: bool,
}

/// `code` and `rank` are both last-appended in the Python -- `with_code` adds
/// `code` after the dict literal, `board()` adds `rank` after `describe()`
/// returned -- and a crew object never carries both, so one declaration order
/// serves both shapes.
#[derive(Debug, Serialize)]
struct CrewOut {
    id: String,
    name: String,
    tag: String,
    color: String,
    xp: i64,
    level: i64,
    members: Vec<MemberOut>,
    #[serde(rename = "isMine")]
    is_mine: bool,
    /// PRIVACY: omitted, not nulled, for a non-member. See the module note.
    #[serde(skip_serializing_if = "Option::is_none")]
    code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    rank: Option<i64>,
}

/// The envelope key is always present; `null` is what a caller in no crew gets.
/// There is no 404 here.
#[derive(Debug, Serialize)]
struct MineResponse {
    crew: Option<CrewOut>,
}

#[derive(Debug, Serialize)]
struct BoardResponse {
    /// `[]` when there are no crews -- never null, never omitted.
    crews: Vec<CrewOut>,
}

#[derive(Debug, Serialize)]
struct CrewResponse {
    crew: CrewOut,
}

/// `/leave` answers with this and nothing else: no "crew" envelope, and no hint
/// of whether the crew was closed or handed over.
#[derive(Debug, Serialize)]
struct OkResponse {
    ok: bool,
}

// --- rows -----------------------------------------------------------------

struct CrewRow {
    id: String,
    name: String,
    tag: String,
    color: String,
    code: String,
    owner_id: String,
}

struct MemberRow {
    user_id: String,
    handle: String,
    display_name: String,
}

/// One user's progression figure. `Copy` so the lookup helper can hand back a
/// value rather than a borrow that would pin the map.
#[derive(Clone, Copy)]
pub(crate) struct Prog {
    pub(crate) level: i64,
    pub(crate) xp: i64,
}

const CREW_SELECT: &str = "SELECT id, name, tag, color, code, owner_id FROM crews";

fn crew_from_row(r: &sqlx::sqlite::SqliteRow) -> CrewRow {
    CrewRow {
        id: r.get("id"),
        name: r.get("name"),
        tag: r.get("tag"),
        color: r.get("color"),
        code: r.get("code"),
        owner_id: r.get("owner_id"),
    }
}

fn member_from_row(r: &sqlx::sqlite::SqliteRow) -> MemberRow {
    MemberRow {
        user_id: r.get("user_id"),
        handle: r.get("handle"),
        display_name: r.get("display_name"),
    }
}

// --- pure logic -----------------------------------------------------------

/// Python's whitespace table for `str.split()` and `str.strip()`: Unicode
/// White_Space *plus* the four C0 separators U+001C..U+001F, which Rust's
/// `char::is_whitespace` leaves out. Including them keeps "ab\x1ccd"
/// normalising to "ab cd" here as it does there, instead of failing NAME_RE.
fn py_is_space(c: char) -> bool {
    c.is_whitespace() || matches!(c, '\u{1c}'..='\u{1f}')
}

/// Python's `" ".join(name.split())` -- not a trim. `str.split()` with no
/// argument splits on *runs* of whitespace and drops the empty pieces, so this
/// collapses internal runs as well as stripping the ends: "  Night   Owls  "
/// becomes "Night Owls".
fn collapse_whitespace(raw: &str) -> String {
    raw.split(py_is_space).filter(|p| !p.is_empty()).collect::<Vec<_>>().join(" ")
}

/// `^[A-Za-z0-9 .'&!-]{3,32}$`. The class is letters, digits, space, period,
/// apostrophe, ampersand, exclamation mark and hyphen -- nothing else, so
/// "<b>" fails. Lengths are code points, the way the regex counts.
fn name_ok(s: &str) -> bool {
    let n = s.chars().count();
    (3..=32).contains(&n)
        && s.chars().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, ' ' | '.' | '\'' | '&' | '!' | '-')
        })
}

/// `^[A-Z0-9]{2,4}$`, checked after the uppercasing.
fn tag_ok(s: &str) -> bool {
    let n = s.chars().count();
    (2..=4).contains(&n) && s.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
}

/// `^#[0-9a-fA-F]{6}$`. Both cases are accepted and neither is normalised: the
/// colour is stored and returned exactly as it arrived.
fn color_ok(s: &str) -> bool {
    match s.strip_prefix('#') {
        Some(hex) => hex.chars().count() == 6 && hex.chars().all(|c| c.is_ascii_hexdigit()),
        None => false,
    }
}

/// `[0-9A-F]{8}`: the shape gate that keeps a malformed invite code away from
/// the database entirely. Note `[A-F]`, not `is_ascii_hexdigit` -- the code has
/// already been uppercased, and "abcdef12" must not match before that.
fn code_ok(s: &str) -> bool {
    s.len() == 8 && s.bytes().all(|b| b.is_ascii_digit() || matches!(b, b'A'..=b'F'))
}

// --- request validation ---------------------------------------------------
//
// Hand-rolled rather than derived, because the 422 body has to keep Pydantic's
// {loc, msg, type} entries and serde's error text would not. Pydantic-core
// collects every field's error in one pass, so these return a Vec and not just
// the first failure, and the messages below are pydantic-core's own.

/// An empty body is `Field required` at `["body"]`, not a decode error:
/// FastAPI only calls `json.loads` when the request actually carried bytes. A
/// whitespace-only body did carry bytes and so is a decode error.
fn body_object(bytes: &[u8]) -> Result<Map<String, Value>, Vec<VErr>> {
    if bytes.is_empty() {
        return Err(vec![VErr {
            loc: vec!["body".into()],
            msg: "Field required".into(),
            kind: "missing",
        }]);
    }
    match serde_json::from_slice::<Value>(bytes) {
        Ok(Value::Object(o)) => Ok(o),
        Ok(_) => Err(vec![VErr {
            loc: vec!["body".into()],
            msg: "Input should be a valid dictionary or object to extract fields from".into(),
            kind: "model_attributes_type",
        }]),
        // KNOWN GAP: FastAPI appends `json.JSONDecodeError.pos` to this loc, so
        // its `["body", 1]` is our `["body"]`. serde_json reports a line and a
        // column rather than an absolute offset, and the two parsers do not even
        // give up at the same character, so there is nothing faithful to put
        // there. cali.rs and cosmetics.rs make the same call.
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
        // An explicit null lands here too, exactly as Pydantic's string_type:
        // neither field is Optional, so `null` is a wrong type and not a
        // missing value.
        Some(_) => {
            errs.push(verr(field, "Input should be a valid string", "string_type"));
            None
        }
    }
}

/// Pydantic's `max_length`, and nothing else: neither body declares a minimum
/// or a pattern, so `string_too_long` is the only constraint error these fields
/// can carry -- the name, tag and colour *shapes* are the route's own checks and
/// are 400s.
///
/// The cap is a core-schema constraint, so it fires *before* any of
/// `crews.create`'s checks and with a different status: a 41-character name is a
/// 422 while a 33-character one reaches NAME_RE and is a 400, and a 5-character
/// tag is a 422 while "a b" is a 400. Lengths are code points and are measured
/// on the RAW value -- before the whitespace collapse and before `.upper()` --
/// so a 42-character name that would collapse to 30 is still a 422.
///
/// Every cap here is above one, so the message never needs Pydantic's singular
/// "character".
fn fits(s: &str, field: &str, max: usize, errs: &mut Vec<VErr>) -> bool {
    if s.chars().count() > max {
        let msg = format!("String should have at most {max} characters");
        errs.push(verr(field, &msg, "string_too_long"));
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

/// `CreateBody`: name (..=40), tag (..=4), color (..=7), `extra="forbid"`.
/// Fields are reported in declaration order, as Pydantic validates them.
fn create_body(bytes: &[u8]) -> Result<CreateBody, Vec<VErr>> {
    let obj = body_object(bytes)?;
    let mut errs = Vec::new();
    let name = take_str(&obj, "name", &mut errs);
    let name = name.filter(|s| fits(s, "name", MAX_NAME_CHARS, &mut errs));
    let tag = take_str(&obj, "tag", &mut errs);
    let tag = tag.filter(|s| fits(s, "tag", MAX_TAG_CHARS, &mut errs));
    let color = take_str(&obj, "color", &mut errs);
    let color = color.filter(|s| fits(s, "color", MAX_COLOR_CHARS, &mut errs));
    forbid_extra(&obj, &["name", "tag", "color"], &mut errs);
    // The `errs.is_empty()` guard is not redundant: an extra key leaves all
    // three fields present and still has to fail.
    match (name, tag, color) {
        (Some(name), Some(tag), Some(color)) if errs.is_empty() => {
            Ok(CreateBody { name, tag, color })
        }
        _ => Err(errs),
    }
}

/// `JoinBody`: a single `code` (..=12), `extra="forbid"`. The string comes back
/// raw -- `join` strips and uppercases it afterwards, where the Python does, so
/// the cap is measured before the strip and a 13-character code padded with
/// spaces is a 422 rather than a 404.
fn join_body(bytes: &[u8]) -> Result<String, Vec<VErr>> {
    let obj = body_object(bytes)?;
    let mut errs = Vec::new();
    let code = take_str(&obj, "code", &mut errs);
    let code = code.filter(|s| fits(s, "code", MAX_CODE_CHARS, &mut errs));
    forbid_extra(&obj, &["code"], &mut errs);
    match code {
        Some(code) if errs.is_empty() => Ok(code),
        _ => Err(errs),
    }
}

/// Finishing is worth 20; winning a game *with others* 30 more, 2nd 15, 3rd 8.
/// A solo game is a flat 20 even for place 1, because the bonus needs
/// `players >= 2`.
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

/// NOT `derive_level(total_xp)`: the average member's level, plus one per extra
/// member. Forgetting the `+ n - 1` is the easiest silent drift here -- a crew
/// of twenty level-1 players reports level 20.
///
/// Integer floor division, with `max(1, n)` in the divisor. A zero-member crew
/// is unreachable in normal flow but the Python guards for it, and the guard's
/// answer is level 0 (`derive_level(0).0 + 0 - 1`), deliberately not clamped
/// to 1.
pub(crate) fn crew_level(total_xp: i64, member_count: usize) -> i64 {
    let n = member_count as i64;
    derive_level(total_xp / n.max(1)).0 + n - 1
}

/// Every member id was passed to `progress`, so the fallback is unreachable. It
/// is `derive_level(0)` rather than a zero so that a future miss cannot invent a
/// level nobody can reach.
fn prog_of(prog: &HashMap<String, Prog>, user_id: &str) -> Prog {
    prog.get(user_id).copied().unwrap_or(Prog { level: derive_level(0).0, xp: 0 })
}

/// Accumulate game XP per user from `(user_id, day, place, players)` rows.
/// Pure, so the per-day cap is testable without a database.
///
/// The Python clamps at every accumulation step (`min(CAP, running + xp)`),
/// which is the same thing as clamping the day's total only because
/// `xp_for_result` is never negative.
fn fold_game_xp(rows: &[(String, String, i64, i64)], out: &mut HashMap<String, i64>) {
    let mut per_day: HashMap<(&str, &str), i64> = HashMap::new();
    for (user_id, day, place, players) in rows {
        *per_day.entry((user_id.as_str(), day.as_str())).or_insert(0) +=
            xp_for_result(*place, *players);
    }
    for ((user_id, _day), xp) in per_day {
        if let Some(total) = out.get_mut(user_id) {
            *total += xp.min(GAME_XP_DAY_CAP);
        }
    }
}

/// Pure assembly of the wire object, kept apart from the queries so the board
/// can compute everyone's XP in one pass and still emit byte-identical crews.
fn assemble(
    crew: &CrewRow,
    mem: &[MemberRow],
    prog: &HashMap<String, Prog>,
    viewer_id: Option<&str>,
    with_code: bool,
) -> CrewOut {
    let xp: i64 = mem.iter().map(|m| prog_of(prog, &m.user_id).xp).sum();
    CrewOut {
        id: crew.id.clone(),
        name: crew.name.clone(),
        tag: crew.tag.clone(),
        color: crew.color.clone(),
        xp,
        level: crew_level(xp, mem.len()),
        members: mem
            .iter()
            .map(|m| MemberOut {
                user_id: m.user_id.clone(),
                // Python's `display_name or handle`: the column defaults to ""
                // and the empty string is falsy there.
                display_name: if m.display_name.is_empty() {
                    m.handle.clone()
                } else {
                    m.display_name.clone()
                },
                handle: m.handle.clone(),
                level: prog_of(prog, &m.user_id).level,
                owner: m.user_id == crew.owner_id,
            })
            .collect(),
        is_mine: mem.iter().any(|m| Some(m.user_id.as_str()) == viewer_id),
        code: if with_code { Some(crew.code.clone()) } else { None },
        rank: None,
    }
}

/// Sort by `(-xp, name)`, then rank over the FULL list, then cap.
///
/// Rank-then-slice, never slice-then-rank: the two agree today only because the
/// list is already sorted, and keeping the order means a future change to the
/// sort cannot silently start handing out non-contiguous ranks.
fn rank_and_cap(mut out: Vec<CrewOut>) -> Vec<CrewOut> {
    out.sort_by(|a, b| b.xp.cmp(&a.xp).then(a.name.cmp(&b.name)));
    for (i, crew) in out.iter_mut().enumerate() {
        crew.rank = Some(i as i64 + 1);
    }
    out.truncate(BOARD_SIZE);
    out
}

// --- queries --------------------------------------------------------------

/// `?1,?2,...` for an IN list: sqlx binds SQLite parameters one at a time, so
/// the placeholders have to be generated to match the id count.
fn placeholders(n: usize) -> String {
    (1..=n).map(|i| format!("?{i}")).collect::<Vec<_>>().join(",")
}

/// All-time, with NO date window -- unlike `service::build_board`, which filters
/// by the board's window. Reusing the windowed query here would understate crew
/// XP on the first of the month.
async fn session_xp(
    conn: &mut SqliteConnection,
    ids: &[String],
) -> Result<HashMap<String, i64>, sqlx::Error> {
    let mut out: HashMap<String, i64> = ids.iter().map(|u| (u.clone(), 0)).collect();
    if ids.is_empty() {
        return Ok(out);
    }
    let sql = format!(
        "SELECT user_id,
                COALESCE(SUM(prompts),0)   AS prompts,
                COALESCE(SUM(tools),0)     AS tools,
                COALESCE(SUM(artifacts),0) AS artifacts
         FROM daily_stats WHERE user_id IN ({}) GROUP BY user_id",
        placeholders(ids.len())
    );
    let mut q = sqlx::query(&sql);
    for id in ids {
        q = q.bind(id);
    }
    for r in q.fetch_all(conn).await? {
        let user_id: String = r.get("user_id");
        out.insert(user_id, xp_from_counts(r.get("prompts"), r.get("tools"), r.get("artifacts")));
    }
    Ok(out)
}

/// `substr(at, 1, 10)` is the calendar day of the result. `at` is NOT NULL and
/// SQLite stores it date-first in both "YYYY-MM-DD HH:MM:SS" (our
/// CURRENT_TIMESTAMP) and SQLAlchemy's ".ffffff" form, so the prefix is exactly
/// Python's `at.date()` without parsing anything.
async fn game_xp(
    conn: &mut SqliteConnection,
    ids: &[String],
) -> Result<HashMap<String, i64>, sqlx::Error> {
    let mut out: HashMap<String, i64> = ids.iter().map(|u| (u.clone(), 0)).collect();
    if ids.is_empty() {
        return Ok(out);
    }
    let sql = format!(
        "SELECT user_id, place, players, substr(at, 1, 10) AS day
         FROM game_results WHERE user_id IN ({})",
        placeholders(ids.len())
    );
    let mut q = sqlx::query(&sql);
    for id in ids {
        q = q.bind(id);
    }
    let rows: Vec<(String, String, i64, i64)> = q
        .fetch_all(conn)
        .await?
        .iter()
        .map(|r| {
            (
                r.get("user_id"),
                r.try_get("day").unwrap_or_default(),
                r.get("place"),
                r.get("players"),
            )
        })
        .collect();
    fold_game_xp(&rows, &mut out);
    Ok(out)
}

/// One HQ level per user: session XP plus game XP, the same figure progression
/// shows, so a member's level on their crew card matches their profile.
pub(crate) async fn progress(
    conn: &mut SqliteConnection,
    ids: &[String],
) -> Result<HashMap<String, Prog>, sqlx::Error> {
    let session = session_xp(conn, ids).await?;
    let game = game_xp(conn, ids).await?;
    Ok(ids
        .iter()
        .map(|u| {
            let xp = session.get(u).copied().unwrap_or(0) + game.get(u).copied().unwrap_or(0);
            (u.clone(), Prog { level: derive_level(xp).0, xp })
        })
        .collect())
}

/// Deliberately NO `u.is_active` filter, unlike `service::build_board`: a
/// deactivated member still appears on the roster and their XP still counts
/// toward the crew total.
///
/// The `m.rowid` tiebreak is new. `joined_at` is CURRENT_TIMESTAMP, which has
/// one-second resolution, so two people joining in the same second tie and both
/// the roster order and the owner hand-over target become nondeterministic.
/// Ordering by insertion after that tightens the Python's behaviour rather than
/// changing it (`crew_members` is a rowid table -- a VARCHAR primary key does
/// not make it WITHOUT ROWID).
async fn members(
    conn: &mut SqliteConnection,
    crew_id: &str,
) -> Result<Vec<MemberRow>, sqlx::Error> {
    Ok(sqlx::query(
        "SELECT u.id AS user_id, u.handle, u.display_name
           FROM users u JOIN crew_members m ON m.user_id = u.id
          WHERE m.crew_id = ?1
          ORDER BY m.joined_at, m.rowid",
    )
    .bind(crew_id)
    .fetch_all(conn)
    .await?
    .iter()
    .map(member_from_row)
    .collect())
}

async fn crew_by_id(
    conn: &mut SqliteConnection,
    crew_id: &str,
) -> Result<Option<CrewRow>, sqlx::Error> {
    Ok(sqlx::query(&format!("{CREW_SELECT} WHERE id = ?1"))
        .bind(crew_id)
        .fetch_optional(conn)
        .await?
        .as_ref()
        .map(crew_from_row))
}

async fn crew_by_code(
    conn: &mut SqliteConnection,
    code: &str,
) -> Result<Option<CrewRow>, sqlx::Error> {
    Ok(sqlx::query(&format!("{CREW_SELECT} WHERE code = ?1 LIMIT 1"))
        .bind(code)
        .fetch_optional(conn)
        .await?
        .as_ref()
        .map(crew_from_row))
}

async fn is_crewed(conn: &mut SqliteConnection, user_id: &str) -> Result<bool, sqlx::Error> {
    Ok(sqlx::query("SELECT 1 FROM crew_members WHERE user_id = ?1")
        .bind(user_id)
        .fetch_optional(conn)
        .await?
        .is_some())
}

async fn describe(
    conn: &mut SqliteConnection,
    crew: &CrewRow,
    viewer_id: Option<&str>,
    with_code: bool,
) -> Result<CrewOut, sqlx::Error> {
    let mem = members(conn, &crew.id).await?;
    let ids: Vec<String> = mem.iter().map(|m| m.user_id.clone()).collect();
    let prog = progress(conn, &ids).await?;
    Ok(assemble(crew, &mem, &prog, viewer_id, with_code))
}

async fn load_mine(
    conn: &mut SqliteConnection,
    user_id: &str,
) -> Result<Option<CrewOut>, sqlx::Error> {
    let Some(row) = sqlx::query("SELECT crew_id FROM crew_members WHERE user_id = ?1")
        .bind(user_id)
        .fetch_optional(&mut *conn)
        .await?
    else {
        return Ok(None);
    };
    let crew_id: String = row.get("crew_id");
    // `mine()` has an explicit "no crew row, no crew" guard that `leave()` does
    // not: an orphaned member row answers `null` here rather than erroring.
    let Some(crew) = crew_by_id(conn, &crew_id).await? else {
        return Ok(None);
    };
    Ok(Some(describe(conn, &crew, Some(user_id), true).await?))
}

async fn build_board(
    conn: &mut SqliteConnection,
    viewer_id: Option<&str>,
) -> Result<Vec<CrewOut>, sqlx::Error> {
    // No ORDER BY, exactly as `select(Crew)`: `rank_and_cap` re-sorts anyway.
    let crews: Vec<CrewRow> = sqlx::query(CREW_SELECT)
        .fetch_all(&mut *conn)
        .await?
        .iter()
        .map(crew_from_row)
        .collect();
    if crews.is_empty() {
        return Ok(Vec::new());
    }

    // One pass over every crew's membership, then one XP computation over the
    // union of their ids. The Python calls describe() per crew, so it rescans
    // daily_stats and game_results once per crew; batching here is invisible on
    // the wire. A user belongs to at most one crew (crew_members.user_id is the
    // primary key), so `ids` cannot contain a duplicate.
    let rows = sqlx::query(
        "SELECT m.crew_id, u.id AS user_id, u.handle, u.display_name
           FROM users u JOIN crew_members m ON m.user_id = u.id
          ORDER BY m.joined_at, m.rowid",
    )
    .fetch_all(&mut *conn)
    .await?;
    let mut by_crew: HashMap<String, Vec<MemberRow>> = HashMap::new();
    let mut ids: Vec<String> = Vec::with_capacity(rows.len());
    for r in &rows {
        let crew_id: String = r.get("crew_id");
        let m = member_from_row(r);
        ids.push(m.user_id.clone());
        by_crew.entry(crew_id).or_default().push(m);
    }
    let prog = progress(conn, &ids).await?;

    let empty: Vec<MemberRow> = Vec::new();
    let out = crews
        .iter()
        .map(|c| {
            assemble(c, by_crew.get(&c.id).unwrap_or(&empty), &prog, viewer_id, false)
        })
        .collect();
    Ok(rank_and_cap(out))
}

/// Eight uppercase hex characters from four CSPRNG bytes -- never a sequential
/// id and never derived from the crew id, so holding one code tells you nothing
/// about any other.
///
/// There is no collision retry, matching the Python: a duplicate collides with
/// UNIQUE(code) and surfaces as a 500. At 2^32 codes that is the rarer failure
/// than the retry loop's own bugs, and the constraint -- not this function -- is
/// what guarantees codes stay unique.
fn new_code() -> String {
    let bytes: [u8; 4] = rand::random();
    hex::encode(bytes).to_uppercase()
}

// --- handlers -------------------------------------------------------------

async fn mine(State(st): State<crate::AppState>, req: Request) -> Response {
    let c = req.extensions().get::<crate::Caller>().expect("caller set by middleware").clone();
    let mut conn = match st.pool.acquire().await {
        Ok(conn) => conn,
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, &format!("{e}")),
    };
    match load_mine(&mut conn, &c.user_id).await {
        Ok(crew) => Json(MineResponse { crew }).into_response(),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, &format!("{e}")),
    }
}

async fn board(State(st): State<crate::AppState>, req: Request) -> Response {
    let c = req.extensions().get::<crate::Caller>().expect("caller set by middleware").clone();
    let mut conn = match st.pool.acquire().await {
        Ok(conn) => conn,
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, &format!("{e}")),
    };
    // `build_board` takes an Option because the Python's does, but every route
    // in this group is behind require_device: there is no anonymous viewer.
    match build_board(&mut conn, Some(&c.user_id)).await {
        Ok(crews) => Json(BoardResponse { crews }).into_response(),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, &format!("{e}")),
    }
}

async fn create(State(st): State<crate::AppState>, req: Request) -> Response {
    let c = req.extensions().get::<crate::Caller>().expect("caller set by middleware").clone();
    let (_, body) = req.into_parts();
    let bytes = match axum::body::to_bytes(body, MAX_BODY_BYTES).await {
        Ok(b) => b,
        Err(_) => return err(StatusCode::BAD_REQUEST, "body too large"),
    };
    // One pass: an extra key, a missing key, a wrong type and an over-long
    // value are all Pydantic failures and all come back together, in the array
    // shape its handler emits.
    let body = match create_body(&bytes) {
        Ok(b) => b,
        Err(errs) => return invalid(errs),
    };

    // The first failure wins, and this is the order the Python checks in.
    let name = collapse_whitespace(&body.name);
    let tag = body.tag.to_uppercase();
    if !name_ok(&name) {
        return err(StatusCode::BAD_REQUEST, NAME_HELP);
    }
    if !tag_ok(&tag) {
        return err(StatusCode::BAD_REQUEST, TAG_HELP);
    }
    // `color` is never normalised -- not trimmed, not lowercased.
    if !color_ok(&body.color) {
        return err(StatusCode::BAD_REQUEST, COLOR_HELP);
    }

    // One transaction: the crew row and its first member land together or
    // neither does. A committed crew with no members would be an unjoinable
    // ghost sitting on the board at level 0.
    let mut tx = match st.pool.begin().await {
        Ok(t) => t,
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, &format!("{e}")),
    };

    // Checked AFTER all field validation, never before: an already-crewed
    // caller who sends a bad name sees the 400, not the 409, and that ordering
    // is observable.
    match is_crewed(&mut tx, &c.user_id).await {
        Ok(true) => return err(StatusCode::CONFLICT, ALREADY_CREWED),
        Ok(false) => {}
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, &format!("{e}")),
    }

    // Case-insensitive, which is stricter than the schema's case-sensitive
    // UNIQUE(name): "night owls" is taken once "Night Owls" exists. SQLite's
    // LOWER() is ASCII-only, which is exactly what Python's .lower() does over
    // NAME_RE's ASCII-only character class.
    let taken = sqlx::query("SELECT 1 FROM crews WHERE LOWER(name) = LOWER(?1) LIMIT 1")
        .bind(&name)
        .fetch_optional(&mut *tx)
        .await;
    match taken {
        Ok(Some(_)) => return err(StatusCode::CONFLICT, "that name is taken"),
        Ok(None) => {}
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, &format!("{e}")),
    }

    // created_at and joined_at are left to the column defaults, not stamped by
    // this process, so both backends write the same shape.
    let crew = CrewRow {
        id: uuid::Uuid::new_v4().to_string(),
        name,
        tag,
        color: body.color,
        code: new_code(),
        owner_id: c.user_id.clone(),
    };
    let inserted = sqlx::query(
        "INSERT INTO crews (id, name, tag, color, code, owner_id) VALUES (?1,?2,?3,?4,?5,?6)",
    )
    .bind(&crew.id)
    .bind(&crew.name)
    .bind(&crew.tag)
    .bind(&crew.color)
    .bind(&crew.code)
    .bind(&crew.owner_id)
    .execute(&mut *tx)
    .await;
    if let Err(e) = inserted {
        return err(StatusCode::INTERNAL_SERVER_ERROR, &format!("{e}"));
    }
    // The crew row exists within the transaction by now, so the FK on
    // crew_members is satisfiable -- this is the Python's flush.
    let joined = sqlx::query("INSERT INTO crew_members (user_id, crew_id) VALUES (?1,?2)")
        .bind(&c.user_id)
        .bind(&crew.id)
        .execute(&mut *tx)
        .await;
    if let Err(e) = joined {
        return err(StatusCode::INTERNAL_SERVER_ERROR, &format!("{e}"));
    }
    if let Err(e) = tx.commit().await {
        return err(StatusCode::INTERNAL_SERVER_ERROR, &format!("{e}"));
    }

    respond_with_crew(&st.pool, &crew, &c.user_id).await
}

async fn join(State(st): State<crate::AppState>, req: Request) -> Response {
    let c = req.extensions().get::<crate::Caller>().expect("caller set by middleware").clone();
    let (_, body) = req.into_parts();
    let bytes = match axum::body::to_bytes(body, MAX_BODY_BYTES).await {
        Ok(b) => b,
        Err(_) => return err(StatusCode::BAD_REQUEST, "body too large"),
    };
    // `c` is already the caller, so the parsed code gets its own name.
    let raw_code = match join_body(&bytes) {
        Ok(s) => s,
        Err(errs) => return invalid(errs),
    };

    // A lower-case code works: the client may be pasting what a member typed.
    let code = raw_code.trim_matches(py_is_space).to_uppercase();
    // PRIVACY: the shape gate comes before any query, so "", "abc", a 12-char
    // code and an 8-char code containing G-Z never touch the database. That is
    // the cheap half of the anti-enumeration boundary; the other half is that
    // this 404 is byte-identical to the unknown-code one below.
    if !code_ok(&code) {
        return err(StatusCode::NOT_FOUND, NO_SUCH_CODE);
    }

    // The Python reads the member count and inserts in two statements, so two
    // concurrent joins at 19 members can both pass. Doing it in one transaction
    // lets SQLite's write serialisation close that: the count is re-taken inside
    // the same write transaction that does the insert.
    let mut tx = match st.pool.begin().await {
        Ok(t) => t,
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, &format!("{e}")),
    };

    let found = match crew_by_code(&mut tx, &code).await {
        Ok(f) => f,
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, &format!("{e}")),
    };
    // Before the "already in a crew" check, never after: a crewed caller who
    // sends an unknown code gets the 404, not the 409.
    let Some(crew) = found else {
        return err(StatusCode::NOT_FOUND, NO_SUCH_CODE);
    };

    // Not idempotent, by design: re-joining the crew you are already in is a
    // 409, not a no-op that hands the crew back.
    match is_crewed(&mut tx, &c.user_id).await {
        Ok(true) => return err(StatusCode::CONFLICT, ALREADY_CREWED),
        Ok(false) => {}
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, &format!("{e}")),
    }

    let count: Result<(i64,), _> =
        sqlx::query_as("SELECT COUNT(*) FROM crew_members WHERE crew_id = ?1")
            .bind(&crew.id)
            .fetch_one(&mut *tx)
            .await;
    match count {
        Ok((n,)) if n >= MAX_MEMBERS => return err(StatusCode::CONFLICT, "that crew is full"),
        Ok(_) => {}
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, &format!("{e}")),
    }

    let joined = sqlx::query("INSERT INTO crew_members (user_id, crew_id) VALUES (?1,?2)")
        .bind(&c.user_id)
        .bind(&crew.id)
        .execute(&mut *tx)
        .await;
    if let Err(e) = joined {
        return err(StatusCode::INTERNAL_SERVER_ERROR, &format!("{e}"));
    }
    if let Err(e) = tx.commit().await {
        return err(StatusCode::INTERNAL_SERVER_ERROR, &format!("{e}"));
    }

    respond_with_crew(&st.pool, &crew, &c.user_id).await
}

/// `with_code = true` on both create and join: joining REVEALS the invite code
/// to the new member, which is the point -- they are inside now and need it to
/// pull a friend in.
///
/// Read back AFTER the commit, as the Python's `describe()` re-queries, so the
/// new member row and its XP are the same ones a later GET /v1/crews/mine
/// returns. Building the response from in-memory state instead would let the
/// two drift.
async fn respond_with_crew(pool: &SqlitePool, crew: &CrewRow, viewer_id: &str) -> Response {
    let mut conn = match pool.acquire().await {
        Ok(conn) => conn,
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, &format!("{e}")),
    };
    match describe(&mut conn, crew, Some(viewer_id), true).await {
        Ok(crew) => Json(CrewResponse { crew }).into_response(),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, &format!("{e}")),
    }
}

/// No body model in the Python route, so nothing here reads or requires one: the
/// client POSTs with no body at all and a `Json<T>` extractor would answer that
/// with a 422.
async fn leave(State(st): State<crate::AppState>, req: Request) -> Response {
    let c = req.extensions().get::<crate::Caller>().expect("caller set by middleware").clone();
    match do_leave(&st.pool, &c.user_id).await {
        Ok(()) => Json(OkResponse { ok: true }).into_response(),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, &format!("{e}")),
    }
}

async fn do_leave(pool: &SqlitePool, user_id: &str) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;
    let Some(row) = sqlx::query("SELECT crew_id FROM crew_members WHERE user_id = ?1")
        .bind(user_id)
        .fetch_optional(&mut *tx)
        .await?
    else {
        // The idempotency guarantee: no member row means zero writes and a
        // success, so leaving twice and leaving when never joined both answer
        // 200 {"ok": true}. Dropping `tx` rolls back a transaction that wrote
        // nothing.
        return Ok(());
    };
    let crew_id: String = row.get("crew_id");
    // The Python does not null-check this and would raise on an orphaned member
    // row. ON DELETE CASCADE plus foreign_keys=ON makes that unreachable, but
    // here it stays a None and the member row is still cleaned up -- never a
    // panic.
    let crew = crew_by_id(&mut tx, &crew_id).await?;

    // Order matters, and not as an optimisation: the DELETE has to be visible to
    // the roster read below, or the leaver counts himself, the crew is never
    // closed and ownership never moves.
    sqlx::query("DELETE FROM crew_members WHERE user_id = ?1")
        .bind(user_id)
        .execute(&mut *tx)
        .await?;
    let rest = members(&mut tx, &crew_id).await?;

    // if/elif, not two independent branches: when the leaver was the only
    // member the crew is deleted and no owner UPDATE is attempted.
    match rest.first() {
        // The last one out closes the crew. Nothing is left to cascade.
        None => {
            sqlx::query("DELETE FROM crews WHERE id = ?1")
                .bind(&crew_id)
                .execute(&mut *tx)
                .await?;
        }
        // Hand-over is by seniority -- the longest-standing REMAINING member,
        // not the next to join and not an arbitrary one -- and only when the
        // leaver was the owner.
        Some(next) if crew.as_ref().map(|c| c.owner_id.as_str()) == Some(user_id) => {
            sqlx::query("UPDATE crews SET owner_id = ?1 WHERE id = ?2")
                .bind(&next.user_id)
                .bind(&crew_id)
                .execute(&mut *tx)
                .await?;
        }
        Some(_) => {}
    }
    tx.commit().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn crew(id: &str, name: &str, owner: &str) -> CrewRow {
        CrewRow {
            id: id.into(),
            name: name.into(),
            tag: "AB".into(),
            color: "#9b8cf0".into(),
            code: "KX81B9QZ".into(),
            owner_id: owner.into(),
        }
    }

    fn member(id: &str, handle: &str, display: &str) -> MemberRow {
        MemberRow {
            user_id: id.into(),
            handle: handle.into(),
            display_name: display.into(),
        }
    }

    fn progs(entries: &[(&str, i64)]) -> HashMap<String, Prog> {
        entries
            .iter()
            .map(|&(u, xp)| (u.to_string(), Prog { level: derive_level(xp).0, xp }))
            .collect()
    }

    #[test]
    fn names_match_the_python_regex() {
        assert!(name_ok("Night Owls"));
        assert!(name_ok("O'Brien & Co."));
        assert!(name_ok("a-b!"));
        assert!(!name_ok("ab")); // under 3
        assert!(!name_ok(&"x".repeat(33))); // over 32
        assert!(!name_ok("<b>hi</b>")); // angle brackets are not in the class
        assert!(!name_ok("Night_Owls")); // nor underscores
        assert!(!name_ok("Nuit Chouette é")); // nor non-ASCII letters
    }

    #[test]
    fn tags_are_checked_after_uppercasing() {
        assert!(tag_ok(&"ab".to_uppercase()));
        assert!(tag_ok("OWL1"));
        assert!(!tag_ok("A")); // under 2
        assert!(!tag_ok("ABCDE")); // over 4 -- but Pydantic 422s this first
        assert!(!tag_ok(&"a b".to_uppercase())); // a space is not in the class
        assert!(!tag_ok("ab")); // lower case only matches once uppercased
    }

    #[test]
    fn colours_keep_their_case_and_need_six_hex_digits() {
        assert!(color_ok("#9b8cf0"));
        assert!(color_ok("#9B8CF0"));
        assert!(!color_ok("red"));
        assert!(!color_ok("#fff"));
        assert!(!color_ok("#ffffffff"));
        assert!(!color_ok("9b8cf0"));
        assert!(!color_ok("#9b8cfg"));
    }

    #[test]
    fn the_invite_code_gate_rejects_everything_but_eight_upper_hex() {
        assert!(code_ok("0123ABCD"));
        assert!(code_ok("FFFFFFFF"));
        assert!(!code_ok("")); // empty
        assert!(!code_ok("abc")); // too short
        assert!(!code_ok("ZZZZZZZZ")); // right length, not hex
        assert!(!code_ok("0123abcd")); // lower case (already uppercased by then)
        assert!(!code_ok("0123ABCDE")); // too long
    }

    /// `(loc, type)` per entry: the pair a client keys off, and the pair the
    /// Python's tests pin.
    fn kinds(errs: &[VErr]) -> Vec<(Vec<String>, &'static str)> {
        errs.iter().map(|e| (e.loc.clone(), e.kind)).collect()
    }

    fn at(field: &str) -> Vec<String> {
        vec!["body".into(), field.into()]
    }

    /// Built through `json!` rather than a raw literal: a `"#9b8cf0"` colour
    /// would otherwise close a `r#"..."#` string early.
    fn create_json(name: &str, tag: &str, color: &str) -> String {
        json!({ "name": name, "tag": tag, "color": color }).to_string()
    }

    #[test]
    fn pydantic_lengths_fire_before_the_regexes() {
        // 33..40 characters passes max_length and then fails NAME_RE with a 400.
        let long_name = create_json(&"x".repeat(33), "AB", "#9b8cf0");
        let parsed = create_body(long_name.as_bytes()).expect("33 characters is within the cap");
        assert!(!name_ok(&parsed.name));
        // 41 characters is a 422 and never reaches the regex.
        let too_much = create_json(&"x".repeat(41), "AB", "#9b8cf0");
        let errs = create_body(too_much.as_bytes()).expect_err("41 characters is a 422");
        assert_eq!(kinds(&errs), vec![(at("name"), "string_too_long")]);
        assert_eq!(errs[0].msg, "String should have at most 40 characters");
        // "red" is 3 characters: a 400, not a 422.
        let bad_colour = create_json("Night Owls", "AB", "red");
        let parsed = create_body(bad_colour.as_bytes()).expect("a short colour is not a 422");
        assert!(!color_ok(&parsed.color));
        // 5-character tag is a 422, checked on the RAW value before .upper().
        let long_tag = create_json("Night Owls", "abcde", "#9b8cf0");
        let errs = create_body(long_tag.as_bytes()).expect_err("five characters is a 422");
        assert_eq!(kinds(&errs), vec![(at("tag"), "string_too_long")]);
        assert_eq!(errs[0].msg, "String should have at most 4 characters");
    }

    #[test]
    fn the_length_check_runs_on_the_raw_name_not_the_collapsed_one() {
        // 42 characters that would collapse to 30 is still a 422.
        let raw = format!("{}{}", "a".repeat(30), " ".repeat(12));
        assert_eq!(raw.chars().count(), 42);
        assert_eq!(collapse_whitespace(&raw).chars().count(), 30);
        let body = create_json(&raw, "AB", "#9b8cf0");
        let errs = create_body(body.as_bytes()).expect_err("42 raw characters is a 422");
        assert_eq!(kinds(&errs), vec![(at("name"), "string_too_long")]);
    }

    #[test]
    fn a_create_body_reports_every_field_in_declaration_order() {
        let errs = create_body(json!({ "tag": 7 }).to_string().as_bytes())
            .expect_err("a missing name, a non-string tag and a missing colour");
        assert_eq!(
            kinds(&errs),
            vec![(at("name"), "missing"), (at("tag"), "string_type"), (at("color"), "missing")]
        );
        assert_eq!(errs[0].msg, "Field required");
        assert_eq!(errs[1].msg, "Input should be a valid string");
    }

    #[test]
    fn an_explicit_null_is_a_wrong_type_and_not_a_missing_field() {
        let body = json!({ "name": null, "tag": "AB", "color": "#9b8cf0" }).to_string();
        let errs = create_body(body.as_bytes()).expect_err("null is not a string");
        assert_eq!(kinds(&errs), vec![(at("name"), "string_type")]);
    }

    #[test]
    fn an_extra_key_is_forbidden_even_when_every_field_is_valid() {
        let body =
            json!({ "name": "Night Owls", "tag": "AB", "color": "#9b8cf0", "banner": "x" })
                .to_string();
        let errs = create_body(body.as_bytes()).expect_err("extra='forbid'");
        assert_eq!(kinds(&errs), vec![(at("banner"), "extra_forbidden")]);
        assert_eq!(errs[0].msg, "Extra inputs are not permitted");
    }

    #[test]
    fn a_body_that_is_not_an_object_never_reaches_the_fields() {
        let body = vec!["body".to_string()];
        assert_eq!(
            kinds(&create_body(b"[]").expect_err("a list is not a model")),
            vec![(body.clone(), "model_attributes_type")]
        );
        assert_eq!(
            kinds(&create_body(b"{").expect_err("truncated JSON")),
            vec![(body.clone(), "json_invalid")]
        );
        // Whitespace still counts as a body, so it is a decode error.
        assert_eq!(
            kinds(&create_body(b"  ").expect_err("whitespace is not JSON")),
            vec![(body.clone(), "json_invalid")]
        );
        // No body at all is the one case Pydantic calls missing.
        assert_eq!(
            kinds(&create_body(b"").expect_err("no body at all")),
            vec![(body, "missing")]
        );
    }

    #[test]
    fn a_join_body_keeps_the_code_raw_and_caps_it_at_twelve() {
        let ok = join_body(json!({ "code": "kx81b9qz" }).to_string().as_bytes())
            .expect("a lower-case code parses");
        assert_eq!(ok, "kx81b9qz");
        let over = json!({ "code": "x".repeat(13) }).to_string();
        let errs = join_body(over.as_bytes()).expect_err("thirteen characters is a 422");
        assert_eq!(kinds(&errs), vec![(at("code"), "string_too_long")]);
        assert_eq!(errs[0].msg, "String should have at most 12 characters");
        // A 404 is the route's own answer; a missing field never gets there.
        assert_eq!(
            kinds(&join_body(b"{}").expect_err("code is required")),
            vec![(at("code"), "missing")]
        );
        assert_eq!(
            kinds(&join_body(json!({ "invite": "KX81B9QZ" }).to_string().as_bytes())
                .expect_err("extra='forbid'")),
            vec![(at("code"), "missing"), (at("invite"), "extra_forbidden")]
        );
    }

    #[test]
    fn the_422_body_is_an_array_of_loc_msg_type_and_nothing_else() {
        let errs = create_body(json!({ "tag": "AB", "color": "#9b8cf0" }).to_string().as_bytes())
            .expect_err("a missing name is a 422");
        // PRIVACY: `input`, `ctx` and `url` are what `_scrub_422` strips, so a
        // rejected value is never reflected back to the caller.
        let text = serde_json::to_string(&json!({ "detail": errs }))
            .expect("the error envelope serialises");
        assert_eq!(
            text,
            r#"{"detail":[{"loc":["body","name"],"msg":"Field required","type":"missing"}]}"#
        );
    }

    #[test]
    fn whitespace_collapses_rather_than_being_trimmed() {
        assert_eq!(collapse_whitespace("Night  Owls"), "Night Owls");
        assert_eq!(collapse_whitespace("  Night   Owls  "), "Night Owls");
        assert_eq!(collapse_whitespace("Night\tOwls"), "Night Owls");
        assert_eq!(collapse_whitespace("Night\n\rOwls"), "Night Owls");
        // The C0 separators Python's str.split() treats as whitespace and
        // char::is_whitespace does not.
        assert_eq!(collapse_whitespace("ab\u{1c}cd"), "ab cd");
        assert_eq!(collapse_whitespace("   "), "");
    }

    #[test]
    fn placing_pays_only_in_a_game_with_others() {
        assert_eq!(xp_for_result(1, 1), 20); // solo: flat 20 even for first
        assert_eq!(xp_for_result(1, 4), 50);
        assert_eq!(xp_for_result(2, 4), 35);
        assert_eq!(xp_for_result(3, 4), 28);
        assert_eq!(xp_for_result(4, 4), 20);
        assert_eq!(xp_for_result(9, 9), 20);
    }

    #[test]
    fn game_xp_is_capped_per_day_and_the_days_are_summed() {
        let row = |u: &str, d: &str| ((u).to_string(), (d).to_string(), 1i64, 4i64);
        // 50 XP a win, so 20 wins in one day would be 1000 without the cap.
        let mut rows: Vec<_> = (0..20).map(|_| row("u1", "2026-10-07")).collect();
        rows.extend((0..3).map(|_| row("u1", "2026-10-08")));
        let mut out: HashMap<String, i64> = [("u1".to_string(), 0)].into_iter().collect();
        fold_game_xp(&rows, &mut out);
        // 400 (capped) + 150, not min(400, 1150) and not 1150.
        assert_eq!(out["u1"], GAME_XP_DAY_CAP + 150);
    }

    #[test]
    fn game_xp_ignores_users_that_were_not_asked_for() {
        let rows = vec![("stranger".to_string(), "2026-10-08".to_string(), 1i64, 4i64)];
        let mut out: HashMap<String, i64> = [("u1".to_string(), 0)].into_iter().collect();
        fold_game_xp(&rows, &mut out);
        assert_eq!(out["u1"], 0);
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn crew_level_is_the_average_members_level_plus_one_per_extra_member() {
        // One member: exactly their own level.
        assert_eq!(crew_level(0, 1), derive_level(0).0);
        assert_eq!(crew_level(5_000, 1), derive_level(5_000).0);
        // Twenty level-1 players report level 20.
        assert_eq!(crew_level(0, 20), 20);
        // Floor division on the average, not derive_level of the total.
        assert_eq!(crew_level(2_080, 4), derive_level(520).0 + 3);
        assert_ne!(crew_level(2_080, 4), derive_level(2_080).0);
        // The empty-crew guard yields level 0 and is deliberately not clamped.
        assert_eq!(crew_level(0, 0), 0);
    }

    #[test]
    fn a_crew_object_carries_the_members_in_roster_order() {
        let c = crew("c1", "Night Owls", "u2");
        let mem = vec![member("u1", "ann", ""), member("u2", "bob", "Bob B")];
        let prog = progs(&[("u1", 520), ("u2", 0)]);
        let out = assemble(&c, &mem, &prog, Some("u1"), true);
        assert_eq!(out.xp, 520);
        assert_eq!(out.level, crew_level(520, 2));
        assert!(out.is_mine);
        assert_eq!(out.code.as_deref(), Some("KX81B9QZ"));
        assert_eq!(out.rank, None);
        let handles: Vec<_> = out.members.iter().map(|m| m.handle.as_str()).collect();
        assert_eq!(handles, vec!["ann", "bob"]);
        // display_name falls back to the handle when the column is "".
        assert_eq!(out.members[0].display_name, "ann");
        assert_eq!(out.members[1].display_name, "Bob B");
        // Exactly one owner, and it is the crew's owner_id.
        assert!(!out.members[0].owner);
        assert!(out.members[1].owner);
    }

    #[test]
    fn a_stranger_sees_no_code_and_is_not_mine() {
        let c = crew("c1", "Night Owls", "u1");
        let mem = vec![member("u1", "ann", "")];
        let prog = progs(&[("u1", 0)]);
        let out = assemble(&c, &mem, &prog, Some("u9"), false);
        assert!(!out.is_mine);
        // PRIVACY: the key must be ABSENT, not null.
        let text = serde_json::to_string(&out).expect("a crew object serialises");
        assert!(!text.contains("code"), "got: {text}");
        assert!(!text.contains("rank"), "got: {text}");
    }

    #[test]
    fn a_viewer_with_no_crew_is_never_mine() {
        let c = crew("c1", "Night Owls", "u1");
        let mem = vec![member("u1", "ann", "")];
        let out = assemble(&c, &mem, &progs(&[("u1", 0)]), None, false);
        assert!(!out.is_mine);
    }

    #[test]
    fn the_key_order_is_the_pythons_dict_order() {
        let c = crew("c1", "Night Owls", "u1");
        let out = assemble(&c, &[member("u1", "ann", "")], &progs(&[("u1", 0)]), Some("u1"), true);
        let text = serde_json::to_string(&out).expect("a crew object serialises");
        // Each key must appear no earlier than the one before it.
        let keys = [
            "\"id\"", "\"name\"", "\"tag\"", "\"color\"", "\"xp\"", "\"level\"", "\"members\"",
            "\"isMine\"", "\"code\"",
        ];
        let mut at = 0;
        for k in keys {
            let found = text[at..].find(k).map(|i| i + at);
            assert!(found.is_some(), "{k} missing from {text}");
            at = found.expect("checked just above");
        }
        assert!(text.contains(r#""userId":"u1","handle":"ann","displayName":"ann","level":1,"owner":true"#),
                "member key order drifted: {text}");
    }

    #[test]
    fn the_board_sorts_by_xp_then_name_and_ranks_before_capping() {
        let prog = progs(&[("u1", 0)]);
        let one = |id: &str, name: &str, xp: i64| {
            let mut o = assemble(&crew(id, name, "u1"), &[], &prog, None, false);
            o.xp = xp;
            o
        };
        let out = rank_and_cap(vec![
            one("b", "Bats", 100),
            one("a", "Aardvarks", 100),
            one("c", "Cats", 900),
        ]);
        let order: Vec<_> = out.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(order, vec!["Cats", "Aardvarks", "Bats"]);
        assert_eq!(out.iter().map(|c| c.rank).collect::<Vec<_>>(),
                   vec![Some(1), Some(2), Some(3)]);
    }

    #[test]
    fn the_board_is_capped_at_fifty_with_contiguous_ranks() {
        let prog = progs(&[("u1", 0)]);
        let all: Vec<CrewOut> = (0..60)
            .map(|i| {
                let mut o = assemble(
                    &crew(&format!("c{i}"), &format!("crew {i:02}"), "u1"),
                    &[],
                    &prog,
                    None,
                    false,
                );
                // Descending XP, so the sort keeps insertion order.
                o.xp = 1_000 - i;
                o
            })
            .collect();
        let out = rank_and_cap(all);
        assert_eq!(out.len(), BOARD_SIZE);
        assert_eq!(out[0].rank, Some(1));
        assert_eq!(out[BOARD_SIZE - 1].rank, Some(BOARD_SIZE as i64));
    }

    #[test]
    fn an_empty_board_is_an_empty_array() {
        let text = serde_json::to_string(&BoardResponse { crews: Vec::new() })
            .expect("the envelope serialises");
        assert_eq!(text, r#"{"crews":[]}"#);
    }

    #[test]
    fn mine_answers_null_rather_than_omitting_the_key() {
        let text =
            serde_json::to_string(&MineResponse { crew: None }).expect("the envelope serialises");
        assert_eq!(text, r#"{"crew":null}"#);
    }

    #[test]
    fn leave_answers_ok_and_only_ok() {
        let text = serde_json::to_string(&OkResponse { ok: true }).expect("it serialises");
        assert_eq!(text, r#"{"ok":true}"#);
    }

    #[test]
    fn an_invite_code_is_eight_upper_hex_characters() {
        for _ in 0..64 {
            let code = new_code();
            assert_eq!(code.len(), 8, "got: {code}");
            assert!(code_ok(&code), "got: {code}");
        }
    }

    #[test]
    fn in_lists_get_one_placeholder_each() {
        assert_eq!(placeholders(1), "?1");
        assert_eq!(placeholders(3), "?1,?2,?3");
    }
}
