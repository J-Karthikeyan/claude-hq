//! Directed "come to the Arena" nudges, ported from `app/routes/nudges.py`.
//!
//! A nudge is persisted so it reaches a friend whose Arena tab is closed: their
//! Claude HQ drains GET /v1/nudges on a timer and shows a native notification.
//! When they do have a socket open we also write a live copy so it lands
//! instantly.
//!
//! By design a nudge carries no URL, no path, no room id and no command -- only
//! who it is from and an optional short note. That is the whole safety story:
//! it cannot make the recipient's machine open or run anything, the recipient
//! decides whether to act. Do not add an id, a deep link or a "join" field for
//! convenience; `extra="forbid"` refuses one on the way in -- with the field
//! named in the 422's `detail[].loc` -- and the tests on both backends assert
//! none comes back out.

use axum::{
    extract::{Request, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::Serialize;
use serde_json::{json, Value};
use sqlx::Row;

/// A friendly cap so a nudge is a tap, not a spam cannon.
///
/// It counts undelivered rows for the ORDERED pair (sender -> recipient), so it
/// is directional, per-pair, and a *backlog* cap rather than a rate limit: the
/// budget re-opens the instant the recipient drains, with no cooldown.
const MAX_UNDELIVERED_PER_PAIR: i64 = 5;

/// A note is capped at 120 characters, so a body anywhere near this is not a
/// nudge. Python has no such ceiling and would answer a huge body with a 422
/// instead; nothing a client sends legitimately comes close either way.
const MAX_BODY: usize = 64 * 1024;

/// `main.rs::err` is private and this port owns exactly one file, so the body
/// shape is repeated here rather than main.rs being widened. Same
/// `{"detail": ...}` envelope as every other error in the service.
fn err(code: StatusCode, msg: &str) -> Response {
    (code, Json(serde_json::json!({ "detail": msg }))).into_response()
}

// --- 422 ------------------------------------------------------------------
//
// FastAPI answers with TWO different bodies under the same status code, and the
// difference is observable: an explicit `raise HTTPException(code, "text")`
// carries `detail` as a bare string, while anything pydantic rejects goes
// through `_scrub_422` in app/main.py and carries `detail` as a LIST of
// `{loc, msg, type}`. Every 422 this module can produce is the second kind --
// the request model is the only thing that rejects with 422 here -- so the 400,
// 404, 429 and 500 paths keep `err` and the string form.

/// A pydantic rejection as `(type, msg)`, worded exactly as the client sees it.
type Reject = (&'static str, &'static str);

const MISSING_BODY: Reject = ("missing", "Field required");
const JSON_INVALID: Reject = ("json_invalid", "JSON decode error");
const MODEL_ATTRS: Reject =
    ("model_attributes_type", "Input should be a valid dictionary or object to extract fields from");
const EXTRA_FORBIDDEN: Reject = ("extra_forbidden", "Extra inputs are not permitted");
const STR_TYPE: Reject = ("string_type", "Input should be a valid string");
const HANDLE_TOO_SHORT: Reject = ("string_too_short", "String should have at least 1 character");
const HANDLE_TOO_LONG: Reject = ("string_too_long", "String should have at most 64 characters");
const NOTE_TOO_LONG: Reject = ("string_too_long", "String should have at most 120 characters");

#[derive(Debug)]
struct VErr {
    loc: Vec<Value>,
    kind: &'static str,
    msg: &'static str,
}

fn at(loc: &[&str], r: Reject) -> VErr {
    VErr { loc: loc.iter().map(|s| json!(s)).collect(), kind: r.0, msg: r.1 }
}

/// Each entry carries exactly loc/msg/type and in that order: `_scrub_422`
/// rebuilds the dict from those three keys and drops pydantic's `input` and
/// `url`, so a rejected handle or note is never echoed back to the sender.
fn detail(errs: &[VErr]) -> Vec<Value> {
    errs.iter().map(|e| json!({ "loc": e.loc, "msg": e.msg, "type": e.kind })).collect()
}

fn err422(errs: &[VErr]) -> Response {
    (StatusCode::UNPROCESSABLE_ENTITY, Json(json!({ "detail": detail(errs) }))).into_response()
}

// --- wire shapes ----------------------------------------------------------

/// The validated body, after pydantic's field constraints and the note cleaner.
///
/// Hand-parsed rather than derived: the client reads `detail[].loc` to point at
/// the offending field, and serde's error text carries no `loc` at all.
struct SendNudgeRequest {
    to_handle: String,
    /// Already cleaned, because `_clean_note` is an *after* validator and so is
    /// what the Python route sees. Never `null` on any hop: absent means "",
    /// and an explicit null is a 422.
    note: String,
}

#[derive(Serialize)]
struct SendNudgeResponse {
    /// Always true -- every non-queued outcome is an HTTP error. The client
    /// branches on `deliveredLive` to pick "delivered" over "queued", so that
    /// one stays a number and this one stays a bool.
    queued: bool,
    #[serde(rename = "deliveredLive")] delivered_live: usize,
}

/// The HTTP drain is a narrower privacy boundary than the live socket frame,
/// and the asymmetry is deliberate: no sender userId, no avatarUrl, no row id,
/// no toHandle, no deliveredAt. Do not unify this with `NudgeFrom`.
#[derive(Serialize)]
struct NudgeItem {
    #[serde(rename = "fromHandle")] from_handle: String,
    #[serde(rename = "fromName")] from_name: String,
    note: String,
    /// The nudge's CREATION time, never its delivery time.
    at: String,
}

#[derive(Serialize)]
struct NudgesResponse {
    nudges: Vec<NudgeItem>,
}

/// The sender as the live frame carries them. Wider than `NudgeItem` because a
/// socket already knows the room roster.
///
/// Not `rooms::Member::public()`: that sends the raw `display_name`, which can
/// be "", where this path falls back to the handle, and it may carry fields a
/// nudge must not (no trainerName, no cosmetics).
#[derive(Serialize)]
struct NudgeFrom<'a> {
    #[serde(rename = "userId")] user_id: &'a str,
    handle: &'a str,
    #[serde(rename = "displayName")] display_name: String,
    #[serde(rename = "avatarUrl")] avatar_url: &'a str,
}

#[derive(Serialize)]
struct NudgeLive<'a> {
    #[serde(rename = "type")] kind: &'static str,
    from: NudgeFrom<'a>,
    note: &'a str,
}

// --- pure logic -----------------------------------------------------------

/// Python's `str.isprintable()`, which is what the note cleaner is specified in
/// terms of: everything *except* Unicode categories Cc, Cf, Cs, Co, Cn, Zs, Zl
/// and Zp, with ASCII space U+0020 as the single exception.
///
/// Worth spelling out rather than reaching for `is_control()`, which only
/// covers Cc: NO-BREAK SPACE, ZWJ, the bidi marks and the line/paragraph
/// separators are all non-printable to Python and would otherwise survive,
/// while emoji and other astral characters must be kept.
///
/// Checked against CPython over every code point: this never drops one Python
/// keeps. The one gap is general Cn, i.e. code points unassigned in whatever
/// Unicode version the interpreter was built against -- a table std does not
/// expose and no dependency here carries, so an unassigned character survives
/// the clean in Rust and would not in Python. It renders as tofu either way.
fn is_printable(ch: char) -> bool {
    if ch == ' ' {
        return true;
    }
    if ch.is_control() {
        return false; // Cc
    }
    let c = ch as u32;
    // Noncharacters (Cn): the pair at the top of every plane, plus the Arabic
    // presentation-form hole. Cheap, and exact where general Cn is not.
    if (c & 0xFFFE) == 0xFFFE || (0xFDD0..=0xFDEF).contains(&c) {
        return false;
    }
    !matches!(c,
        // Zs other than U+0020, then Zl and Zp.
        0x00A0 | 0x1680 | 0x2000..=0x200A | 0x2028 | 0x2029 | 0x202F | 0x205F | 0x3000
        // Cf: soft hyphen, the Arabic and Syriac number signs, the joiners and
        // bidi controls, interlinear annotation, the music and tag blocks.
        // U+2060..=U+206F is taken whole: U+2065 inside it is unassigned, and
        // Python refuses that too.
        | 0x00AD | 0x0600..=0x0605 | 0x061C | 0x06DD | 0x070F | 0x0890..=0x0891
        | 0x08E2 | 0x180E | 0x200B..=0x200F | 0x202A..=0x202E | 0x2060..=0x206F
        | 0xFEFF | 0xFFF9..=0xFFFB | 0x110BD | 0x110CD | 0x13430..=0x1343F
        | 0x1BCA0..=0x1BCA3 | 0x1D173..=0x1D17A | 0xE0001 | 0xE0020..=0xE007F
        // Co, the three private-use ranges.
        | 0xE000..=0xF8FF | 0xF0000..=0xFFFFD | 0x100000..=0x10FFFD
    )
}

/// `"".join(ch for ch in v if ch.isprintable()).strip()`.
///
/// The filter DELETES rather than replaces, and it runs BEFORE the strip, so
/// "a\tb" is "ab" -- not "a b" and not "a\tb". Python's trailing `[:120]` is
/// unreachable (cleaning can only shorten an already-validated note) and is
/// therefore not reproduced. `trim()` matches `str.strip()` here because the
/// only whitespace that survives the filter is U+0020.
fn clean_note(raw: &str) -> String {
    raw.chars().filter(|c| is_printable(*c)).collect::<String>().trim().to_string()
}

/// Decode the raw body the way FastAPI decodes it, before any field is looked
/// at.
///
/// Taking the bytes rather than axum's `Json` extractor is what makes this
/// possible: `Json` answers 400 on malformed JSON and 415 on a wrong content
/// type, where FastAPI answers 422 with a `detail` list in both cases.
///
/// Only a zero-length body is "no body at all" to FastAPI (`if body_bytes:` in
/// routing.py), and the required model then reports itself missing. A body of
/// whitespace is truthy, so it reaches `json.loads` and comes back malformed --
/// not missing.
fn read_send(bytes: &[u8]) -> Result<SendNudgeRequest, Vec<VErr>> {
    if bytes.is_empty() {
        return Err(vec![at(&["body"], MISSING_BODY)]);
    }
    let body: Value = match serde_json::from_slice(bytes) {
        Ok(v) => v,
        Err(e) => {
            // FastAPI appends `JSONDecodeError.pos`, a 0-based character
            // offset, after "body". serde_json reports a 1-based column
            // instead, so it is one more -- except at EOF, where Python points
            // one PAST the last character and serde_json points at it, and the
            // two already agree.
            let pos = if e.is_eof() { e.column() } else { e.column().saturating_sub(1) };
            return Err(vec![VErr {
                loc: vec![json!("body"), json!(pos)],
                kind: JSON_INVALID.0,
                msg: JSON_INVALID.1,
            }]);
        }
    };
    parse_send(&body)
}

/// The pydantic field constraints, in pydantic's order, then the cleaner.
///
/// The order within a field is observable and load-bearing: `max_length=120` is
/// a core-schema constraint while `_clean_note` is an *after* validator, so a
/// 121-character raw note is a 422 and is never silently truncated. Clamping
/// with `chars().take(120)` would accept requests the Python rejects. Lengths
/// are in code points because that is what Python counts.
///
/// The order *between* fields matters too, because every error is reported at
/// once: pydantic-core walks the model's declared fields first, in declaration
/// order, and only then the leftover keys. `preserve_order` on serde_json keeps
/// those leftovers in body order, so a multi-error `detail` list matches
/// entry for entry.
fn parse_send(body: &Value) -> Result<SendNudgeRequest, Vec<VErr>> {
    // An explicit `null` body reads as no body, the same as an empty one.
    if body.is_null() {
        return Err(vec![at(&["body"], MISSING_BODY)]);
    }
    let Some(obj) = body.as_object() else {
        return Err(vec![at(&["body"], MODEL_ATTRS)]);
    };

    let mut errs: Vec<VErr> = Vec::new();
    let mut to_handle = String::new();
    match obj.get("toHandle") {
        // Only an ABSENT key is `missing`. An explicit null is a value, and a
        // non-optional `str` field rejects it as a type error (verified against
        // pydantic 2.13) -- do not fold the two together.
        None => errs.push(at(&["body", "toHandle"], MISSING_BODY)),
        Some(Value::String(s)) => {
            let n = s.chars().count();
            if n == 0 {
                errs.push(at(&["body", "toHandle"], HANDLE_TOO_SHORT));
            } else if n > 64 {
                errs.push(at(&["body", "toHandle"], HANDLE_TOO_LONG));
            } else {
                to_handle = s.clone();
            }
        }
        // Lax mode does not stringify a number or a bool for a `str` field.
        Some(_) => errs.push(at(&["body", "toHandle"], STR_TYPE)),
    }

    let mut note = String::new();
    match obj.get("note") {
        // `note: str = Field("", ...)` has a default but is NOT optional: an
        // absent note is "", while an explicit null is a type error and must
        // never fall back to the default.
        None => {}
        Some(Value::String(s)) => {
            if s.chars().count() > 120 {
                errs.push(at(&["body", "note"], NOTE_TOO_LONG));
            } else {
                note = clean_note(s);
            }
        }
        Some(_) => errs.push(at(&["body", "note"], STR_TYPE)),
    }

    // extra="forbid": a url, a room id or any other actionable extra is named
    // in `loc` and refused, never ignored.
    for k in obj.keys() {
        if !matches!(k.as_str(), "toHandle" | "note") {
            errs.push(at(&["body", k.as_str()], EXTRA_FORBIDDEN));
        }
    }

    if errs.is_empty() {
        Ok(SendNudgeRequest { to_handle, note })
    } else {
        Err(errs)
    }
}

/// Python's `display_name or handle`: the column defaults to "", and the empty
/// string falls back.
fn display_or_handle(display: &str, handle: &str) -> String {
    if display.is_empty() { handle.to_string() } else { display.to_string() }
}

fn backlog_is_full(pending: i64) -> bool {
    pending >= MAX_UNDELIVERED_PER_PAIR
}

/// SQLite hands back "YYYY-MM-DD HH:MM:SS"; Python parses that into a NAIVE
/// datetime and calls `.isoformat()`, i.e. the same string with a 'T'.
///
/// Swapping the space is the whole port. Parsing into a `DateTime<Utc>` and
/// formatting would append "Z" or "+00:00" and break the client's rendering,
/// and a legacy row carrying a ".ffffff" fraction keeps it this way too.
fn wire_at(stored: &str) -> String {
    stored.replacen(' ', "T", 1)
}

// --- handlers -------------------------------------------------------------

async fn send_nudge(State(st): State<crate::AppState>, req: Request) -> Response {
    let c = req.extensions().get::<crate::Caller>().expect("caller set by middleware").clone();
    let (_, body) = req.into_parts();
    let bytes = match axum::body::to_bytes(body, MAX_BODY).await {
        Ok(b) => b,
        Err(_) => return err(StatusCode::BAD_REQUEST, "body too large"),
    };
    // Decode and validate in one pass, so a url, a room id or any other
    // actionable extra fails here -- Python's extra="forbid", enforced earlier.
    // Every rejection this returns is a pydantic one, hence the `detail` list.
    let SendNudgeRequest { to_handle, note } = match read_send(&bytes) {
        Ok(p) => p,
        Err(errs) => return err422(&errs),
    };

    // IMMEDIATE, not the default deferred: the backlog count and the INSERT
    // must not interleave with another send from the same person, or two
    // concurrent nudges each see four pending and both land, taking the backlog
    // to six. Taking the write lock up front also keeps this read-then-write
    // out of WAL's snapshot-conflict case, which would surface as a 500.
    let mut tx = match st.pool.begin_with("BEGIN IMMEDIATE").await {
        Ok(t) => t,
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, &format!("{e}")),
    };

    // Exact, case-sensitive equality: ix_users_handle is a plain UNIQUE index
    // with no NOCASE collation, so "GARY" does not find "gary". No trim, no
    // case folding, no handle normalisation.
    //
    // `is_active = 1` is folded into the lookup the way require_device folds
    // it: Python checks the flag after fetching the row and answers the same
    // 404 either way, deliberately, so that a deactivated account is
    // indistinguishable from a missing one and handle existence is not leaked.
    let found = sqlx::query("SELECT id FROM users WHERE handle = ?1 AND is_active = 1")
        .bind(&to_handle)
        .fetch_optional(&mut *tx)
        .await;
    let found = match found {
        Ok(r) => r,
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, &format!("{e}")),
    };
    let Some(row) = found else {
        return err(StatusCode::NOT_FOUND, "no such person");
    };
    let target_id: String = row.get("id");
    // After the lookup, so nudging your own handle is a 400 and not a 404.
    if target_id == c.user_id {
        return err(StatusCode::BAD_REQUEST, "you cannot nudge yourself");
    }

    let pending = sqlx::query(
        "SELECT COUNT(*) FROM nudges
          WHERE from_user_id = ?1 AND to_user_id = ?2 AND delivered_at IS NULL",
    )
    .bind(&c.user_id)
    .bind(&target_id)
    .fetch_one(&mut *tx)
    .await;
    let pending: i64 = match pending {
        Ok(r) => r.get(0),
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, &format!("{e}")),
    };
    if backlog_is_full(pending) {
        return err(StatusCode::TOO_MANY_REQUESTS, "they already have pending nudges from you");
    }

    // created_at comes from SQLite so it is UTC at second precision in
    // "YYYY-MM-DD HH:MM:SS" form. That string is wire-visible through the GET's
    // `at`, so it must not be written by chrono.
    let ins = sqlx::query(
        "INSERT INTO nudges (id, from_user_id, to_user_id, note, created_at, delivered_at)
         VALUES (?1, ?2, ?3, ?4, CURRENT_TIMESTAMP, NULL)",
    )
    .bind(uuid::Uuid::new_v4().to_string())
    .bind(&c.user_id)
    .bind(&target_id)
    .bind(&note)
    .execute(&mut *tx)
    .await;
    if let Err(e) = ins {
        return err(StatusCode::INTERNAL_SERVER_ERROR, &format!("{e}"));
    }
    // Commit BEFORE the socket write, so a socket error can never lose a queued
    // nudge.
    if let Err(e) = tx.commit().await {
        return err(StatusCode::INTERNAL_SERVER_ERROR, &format!("{e}"));
    }

    let live = NudgeLive {
        kind: "nudge",
        from: NudgeFrom {
            user_id: &c.user_id,
            handle: &c.handle,
            display_name: display_or_handle(&c.display_name, &c.handle),
            avatar_url: &c.avatar_url,
        },
        note: &note,
    };
    // Best effort, and it counts SOCKETS rather than people: every socket of
    // that user in every room the manager holds, private and game rooms too.
    // A dead socket is swallowed and never fails the request.
    //
    // The live copy deliberately does NOT stamp delivered_at: the frame is
    // unacknowledged and may be lost, so the recipient gets it again on their
    // next drain. A duplicate notification is the accepted cost -- do not
    // "fix" it by marking the row delivered here.
    let delivered_live = st
        .rooms
        .deliver_to_user(&target_id, serde_json::to_string(&live).unwrap_or_default())
        .await;
    Json(SendNudgeResponse { queued: true, delivered_live }).into_response()
}

async fn drain_nudges(State(st): State<crate::AppState>, req: Request) -> Response {
    let c = req.extensions().get::<crate::Caller>().expect("caller set by middleware").clone();

    // One transaction for the read and every stamp: if it fails, nothing is
    // marked delivered and the client's retry still sees the whole queue.
    // IMMEDIATE for the same reason as the send -- two concurrent drains must
    // not both hand out the same nudge.
    let mut tx = match st.pool.begin_with("BEGIN IMMEDIATE").await {
        Ok(t) => t,
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, &format!("{e}")),
    };

    // An INNER JOIN, so a nudge whose sender row vanished is skipped (the FK
    // cascades, so in practice it went with them). No `u.is_active` filter: a
    // nudge already queued by a since-deactivated sender is still delivered.
    //
    // No LIMIT either. The per-pair cap bounds one sender, not the queue: N
    // senders can leave 5N pending and the drain owes the recipient all of
    // them. A LIMIT here would also be one careless edit away from stamping
    // rows it never returned and destroying them silently.
    //
    // Python orders by created_at alone, which is second-precision and so ties
    // for a sender who lands all five inside one second; rowid breaks the tie
    // the way insertion order already did in practice. Never by id -- it is a
    // random uuid4.
    let rows = sqlx::query(
        "SELECT n.id, n.note, n.created_at, u.handle, u.display_name
           FROM nudges n
           JOIN users u ON u.id = n.from_user_id
          WHERE n.to_user_id = ?1 AND n.delivered_at IS NULL
          ORDER BY n.created_at, n.rowid",
    )
    .bind(&c.user_id)
    .fetch_all(&mut *tx)
    .await;
    let rows = match rows {
        Ok(r) => r,
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, &format!("{e}")),
    };

    // One timestamp for the whole drain, as Python takes `now` once outside the
    // loop. delivered_at never reaches the wire -- only IS NULL is ever read --
    // so the second-precision form is fine here.
    let now = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
    let mut nudges: Vec<NudgeItem> = Vec::with_capacity(rows.len());
    for r in &rows {
        let id: String = r.get("id");
        // `AND delivered_at IS NULL` is what makes the drain one-shot. If a
        // racing drain already claimed this row we must not return it as well.
        let claimed = sqlx::query(
            "UPDATE nudges SET delivered_at = ?1 WHERE id = ?2 AND delivered_at IS NULL",
        )
        .bind(&now)
        .bind(&id)
        .execute(&mut *tx)
        .await;
        match claimed {
            Ok(res) if res.rows_affected() == 0 => continue,
            Ok(_) => {}
            Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, &format!("{e}")),
        }
        let handle: String = r.get("handle");
        let display: String = r.get("display_name");
        let created: String = r.get("created_at");
        nudges.push(NudgeItem {
            from_name: display_or_handle(&display, &handle),
            note: r.get("note"),
            at: wire_at(&created),
            from_handle: handle,
        });
    }
    // Stamped before the response is written, so a second poll returns []. That
    // makes delivery at-most-once by design: a response lost in flight loses
    // the nudges with it.
    if let Err(e) = tx.commit().await {
        return err(StatusCode::INTERNAL_SERVER_ERROR, &format!("{e}"));
    }
    Json(NudgesResponse { nudges }).into_response()
}

/// Both routes are device-token only; main.rs puts the whole guarded router
/// behind `require_device`, so no auth layer is applied here.
///
/// The websocket-native `{"type":"nudge","to":...}` message handled in the room
/// loop is a different, same-room, unpersisted path and is deliberately not
/// duplicated here.
pub fn routes() -> Router<crate::AppState> {
    Router::new()
        .route("/v1/nudge", post(send_nudge))
        .route("/v1/nudges", get(drain_nudges))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `(loc, type, msg)` per entry -- exactly the triple `_scrub_422` keeps.
    fn errs(body: &str) -> Vec<(Vec<Value>, &'static str, &'static str)> {
        read_send(body.as_bytes())
            .err()
            .unwrap_or_default()
            .into_iter()
            .map(|e| (e.loc, e.kind, e.msg))
            .collect()
    }

    fn accepted(body: &str) -> SendNudgeRequest {
        read_send(body.as_bytes()).expect("valid body")
    }

    fn loc(parts: &[&str]) -> Vec<Value> {
        parts.iter().map(|s| json!(s)).collect()
    }

    #[test]
    fn the_cleaner_deletes_control_chars_before_stripping() {
        // The easy case from the Python test.
        assert_eq!(clean_note("  come play\u{07}  "), "come play");
        // The order is only visible on an interior control char: the tab is
        // deleted, not turned into a space.
        assert_eq!(clean_note("a\tb"), "ab");
        assert_eq!(clean_note("a\nb\r"), "ab");
    }

    #[test]
    fn the_cleaner_follows_python_isprintable_not_is_control() {
        // NO-BREAK SPACE is Zs-but-not-U+0020, so Python drops it.
        assert_eq!(clean_note("a\u{00A0}b"), "ab");
        assert_eq!(clean_note("a\u{00A0}b\u{07} c"), "ab c");
        // Zero-width joiner, RTL mark, line and paragraph separators.
        assert_eq!(clean_note("a\u{200D}b\u{200F}c\u{2028}d\u{2029}e"), "abcde");
        // Emoji and other astral characters are printable and kept.
        assert_eq!(clean_note(" hi \u{1F44B} "), "hi \u{1F44B}");
        // Plain ASCII space is the one separator that survives.
        assert_eq!(clean_note("come  play"), "come  play");
    }

    #[test]
    fn an_over_long_note_is_rejected_not_truncated() {
        let fits = "x".repeat(120);
        assert_eq!(accepted(&format!(r#"{{"toHandle":"gary","note":"{fits}"}}"#)).note, fits);
        let too_long = "x".repeat(121);
        assert_eq!(
            errs(&format!(r#"{{"toHandle":"gary","note":"{too_long}"}}"#)),
            [(
                loc(&["body", "note"]),
                "string_too_long",
                "String should have at most 120 characters"
            )]
        );
        // Counted in code points, as Python counts them: 121 emoji is too long
        // even though it is far more than 121 bytes.
        let astral = "\u{1F44B}".repeat(121);
        assert_eq!(
            errs(&format!(r#"{{"toHandle":"gary","note":"{astral}"}}"#))[0].1,
            "string_too_long"
        );
        let at_cap = "\u{1F44B}".repeat(120);
        assert_eq!(accepted(&format!(r#"{{"toHandle":"gary","note":"{at_cap}"}}"#)).note, at_cap);
    }

    #[test]
    fn the_length_check_runs_before_the_cleaner() {
        // 121 raw characters that would clean down to 2 is still a 422: the
        // constraint sees the raw string. "\\u0007" here is the JSON escape, so
        // the decoded note really is 121 code points.
        let raw = format!("a{}b", "\\u0007".repeat(119));
        assert_eq!(
            errs(&format!(r#"{{"toHandle":"gary","note":"{raw}"}}"#)),
            [(
                loc(&["body", "note"]),
                "string_too_long",
                "String should have at most 120 characters"
            )]
        );
    }

    #[test]
    fn handle_constraints_match_pydantic() {
        assert_eq!(
            errs(r#"{"toHandle":""}"#),
            [(
                loc(&["body", "toHandle"]),
                "string_too_short",
                "String should have at least 1 character"
            )]
        );
        let long = "g".repeat(65);
        assert_eq!(
            errs(&format!(r#"{{"toHandle":"{long}"}}"#)),
            [(
                loc(&["body", "toHandle"]),
                "string_too_long",
                "String should have at most 64 characters"
            )]
        );
        let at_cap = "g".repeat(64);
        assert_eq!(accepted(&format!(r#"{{"toHandle":"{at_cap}"}}"#)).to_handle, at_cap);
        // An absent note is "", never null.
        assert_eq!(accepted(r#"{"toHandle":"gary"}"#).note, "");
    }

    #[test]
    fn nothing_actionable_can_be_sent_in() {
        // A url, a room id, a command -- extra="forbid" on both backends. The
        // key is named in `loc`; its value is never echoed back.
        assert_eq!(
            errs(r#"{"toHandle":"gary","url":"https://evil.example/x"}"#),
            [(loc(&["body", "url"]), "extra_forbidden", "Extra inputs are not permitted")]
        );
        assert_eq!(
            errs(r#"{"toHandle":"gary","note":"hi","cmd":"rm -rf /"}"#),
            [(loc(&["body", "cmd"]), "extra_forbidden", "Extra inputs are not permitted")]
        );
        // Type strictness: a null note and a numeric handle are both refused.
        assert_eq!(
            errs(r#"{"toHandle":"g","note":null}"#),
            [(loc(&["body", "note"]), "string_type", "Input should be a valid string")]
        );
        assert_eq!(
            errs(r#"{"toHandle":5}"#),
            [(loc(&["body", "toHandle"]), "string_type", "Input should be a valid string")]
        );
        assert_eq!(
            errs(r#"{"note":"hi"}"#),
            [(loc(&["body", "toHandle"]), "missing", "Field required")]
        );
        // An explicit null handle is a TYPE error, not `missing`: the key is
        // present, so pydantic validates null against `str`.
        assert_eq!(
            errs(r#"{"toHandle":null}"#),
            [(loc(&["body", "toHandle"]), "string_type", "Input should be a valid string")]
        );
        // The happy shape, with note defaulted.
        let p = accepted(r#"{"toHandle":"gary"}"#);
        assert_eq!(p.to_handle, "gary");
        assert_eq!(p.note, "");
    }

    #[test]
    fn the_422_envelope_is_a_list_of_loc_msg_type() {
        // The whole point of this shape: `detail` is an ARRAY for anything
        // pydantic rejects, where the 400/404/429 paths keep a bare string.
        let body = serde_json::to_string(&json!({
            "detail": detail(&[at(&["body", "note"], NOTE_TOO_LONG)]),
        }))
        .unwrap();
        assert_eq!(
            body,
            r#"{"detail":[{"loc":["body","note"],"msg":"String should have at most 120 characters","type":"string_too_long"}]}"#
        );
        // ...and the string form is still what an HTTPException carries.
        assert_eq!(
            serde_json::to_string(&serde_json::json!({ "detail": "you cannot nudge yourself" }))
                .unwrap(),
            r#"{"detail":"you cannot nudge yourself"}"#
        );
    }

    #[test]
    fn a_body_that_is_not_an_object_is_refused_like_fastapi_refuses_it() {
        // No body at all is "missing"; an explicit JSON null reads the same
        // way. Whitespace is NOT -- it is a truthy body that fails to decode.
        assert_eq!(errs(""), [(loc(&["body"]), "missing", "Field required")]);
        assert_eq!(errs("null"), [(loc(&["body"]), "missing", "Field required")]);
        assert_eq!(
            errs("   "),
            [(vec![json!("body"), json!(3)], "json_invalid", "JSON decode error")]
        );
        assert_eq!(
            errs("[]"),
            [(
                loc(&["body"]),
                "model_attributes_type",
                "Input should be a valid dictionary or object to extract fields from"
            )]
        );
        assert_eq!(errs(r#""gary""#)[0].1, "model_attributes_type");
        assert_eq!(errs("7")[0].1, "model_attributes_type");
        // Malformed JSON is reported against the body with the character
        // offset appended, matching `JSONDecodeError.pos`.
        assert_eq!(
            errs("{oops"),
            [(vec![json!("body"), json!(1)], "json_invalid", "JSON decode error")]
        );
        assert_eq!(errs("{")[0].0, vec![json!("body"), json!(1)]);
    }

    #[test]
    fn several_errors_come_out_in_pydantics_order() {
        // Declared fields first, in declaration order, then the leftover keys
        // in body order -- which is what pydantic-core does and what
        // serde_json's `preserve_order` feature buys us.
        let too_long = "x".repeat(121);
        let got: Vec<(Vec<Value>, &str)> =
            errs(&format!(r#"{{"zz":1,"note":"{too_long}","aa":2}}"#))
                .into_iter()
                .map(|(l, k, _)| (l, k))
                .collect();
        assert_eq!(
            got,
            vec![
                (loc(&["body", "toHandle"]), "missing"),
                (loc(&["body", "note"]), "string_too_long"),
                (loc(&["body", "zz"]), "extra_forbidden"),
                (loc(&["body", "aa"]), "extra_forbidden"),
            ]
        );
    }

    #[test]
    fn an_empty_display_name_falls_back_to_the_handle() {
        assert_eq!(display_or_handle("", "gary"), "gary");
        assert_eq!(display_or_handle("Gary Oak", "gary"), "Gary Oak");
    }

    #[test]
    fn the_fifth_nudge_fits_and_the_sixth_does_not() {
        assert_eq!(MAX_UNDELIVERED_PER_PAIR, 5);
        assert!(!backlog_is_full(0));
        assert!(!backlog_is_full(4)); // the 5th send
        assert!(backlog_is_full(5)); // the 6th is the 429
        assert!(backlog_is_full(6));
    }

    #[test]
    fn at_is_naive_with_no_timezone_suffix() {
        assert_eq!(wire_at("2026-10-08 10:05:22"), "2026-10-08T10:05:22");
        // A row written by SQLAlchemy keeps all six fractional digits.
        assert_eq!(wire_at("2026-10-08 10:05:22.123456"), "2026-10-08T10:05:22.123456");
        // Only the first space is the date/time separator.
        assert_eq!(wire_at("2026-10-08T10:05:22"), "2026-10-08T10:05:22");
    }

    #[test]
    fn the_drain_shape_leaks_nothing_extra() {
        let body = serde_json::to_string(&NudgesResponse {
            nudges: vec![NudgeItem {
                from_handle: "ash".into(),
                from_name: "Ash".into(),
                note: "hi".into(),
                at: "2026-10-08T10:05:22".into(),
            }],
        })
        .unwrap();
        assert_eq!(
            body,
            r#"{"nudges":[{"fromHandle":"ash","fromName":"Ash","note":"hi","at":"2026-10-08T10:05:22"}]}"#
        );
        // An empty queue is [], never null and never omitted.
        assert_eq!(
            serde_json::to_string(&NudgesResponse { nudges: vec![] }).unwrap(),
            r#"{"nudges":[]}"#
        );
    }

    #[test]
    fn the_send_response_and_live_frame_match_the_python_bytes() {
        assert_eq!(
            serde_json::to_string(&SendNudgeResponse { queued: true, delivered_live: 2 }).unwrap(),
            r#"{"queued":true,"deliveredLive":2}"#
        );
        let live = NudgeLive {
            kind: "nudge",
            from: NudgeFrom {
                user_id: "u1",
                handle: "ash",
                display_name: display_or_handle("", "ash"),
                avatar_url: "https://cdn.example/a.png",
            },
            note: "",
        };
        assert_eq!(
            serde_json::to_string(&live).unwrap(),
            r#"{"type":"nudge","from":{"userId":"u1","handle":"ash","displayName":"ash","avatarUrl":"https://cdn.example/a.png"},"note":""}"#
        );
    }
}
