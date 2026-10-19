//! Quick Play (HQ 2.1): matchmaking across rooms, per game. A port of
//! `backend/app/quickplay.py` and `backend/app/routes/quickplay.py`.
//!
//! Queue for a game; the Arena groups people into a fresh room ("qp_<id>") and
//! tells each of them where to go. Crewmates are put together first, then
//! whoever has waited longest. A match forms as soon as [`MAX_GROUP`] are
//! waiting, or once the oldest has waited [`WAIT_SECS`] with at least one other
//! person in the queue.
//!
//! Three things are worth knowing before changing anything here.
//!
//! *Matchmaking is poll-driven, not timer-driven.* [`expire`] and [`form`] run
//! only inside `status` -- and `join` ends by calling `status`, which is why the
//! eighth joiner for a game is matched by their own POST. Any one user's poll
//! advances the state machine for all five games and every waiting user. There
//! is deliberately no background sweeper: adding one would change *when* groups
//! form relative to the Python and break the clock-stepping tests.
//!
//! *All the state is in memory and process-local*, exactly as in the Python:
//! module globals in one Arena process, no table, no migration. A restart drops
//! every queue, match and room entitlement, and clients fall back to `idle` on
//! their next poll. A second Arena process would be a second, disjoint
//! matchmaker. It lives in a static rather than on `AppState` because it is
//! this module's own business and nothing else reads it.
//!
//! *The responses carry counts, never people.* No handle, display name, user
//! id, avatar or crew appears in any of the three bodies -- only a queue size,
//! a group size, the opaque room id and the game key. The crew id read from the
//! database is used to sort the grouping and is then discarded. The lobby
//! websocket is where identities legitimately appear, after admission.

// admits/room_info are the websocket route's half of this feature; main.rs has
// not been wired to call them yet (another agent owns that file), and the
// constants are the vocabulary the tests and that route read.
#![cfg_attr(not(test), allow(dead_code))]

use axum::{
    extract::{Request, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use rand::Rng;
use serde::Serialize;
use serde_json::{json, Value};
use sqlx::Row;
use std::collections::{HashMap, HashSet};
use std::sync::{LazyLock, Mutex};
use std::time::Instant;

/// Group size, and also the queue size that forces an immediate match.
pub const MAX_GROUP: usize = 8;
/// The oldest entry's wait that forces a match, given at least two queued.
pub const WAIT_SECS: f64 = 8.0;
/// No poll for longer than this and the queue entry is dropped.
pub const STALE_SECS: f64 = 15.0;
/// How long `status` keeps telling you where your match is.
pub const MATCH_TTL: f64 = 90.0;
/// How long the room itself keeps admitting the people matched into it.
pub const ROOM_TTL: f64 = 3.0 * 3600.0;
/// Pydantic's `Field(max_length=8)` on `game`: a *string length* cap, not an
/// allowlist. "chess" passes this and fails the allowlist; "platformer" 422s.
const MAX_GAME_LEN: usize = 8;
/// The five queues. A closed set, so an array beats a map: `_queues[g]` can
/// never be a missing key and the GAMES iteration order is a compile-time fact.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Game {
    Kart,
    Plat,
    Fps,
    Golf,
    Type,
}

/// Iteration order is load-bearing: `status` scans the queues in this order, so
/// even a corrupted double-entry resolves the same way every time.
pub const GAMES: [Game; 5] = [Game::Kart, Game::Plat, Game::Fps, Game::Golf, Game::Type];

/// What Quick Play may offer: the catalog, minus whatever this Arena's engine
/// cannot actually referee. Matching two people into a room where no game ever
/// starts is worse than telling them the game is not available, so the queue
/// stays shut for a game this build does not have.
///
/// Driven off `protocol::GAMES`, the same table the room welcome advertises, so
/// porting a game opens its queue with no second edit here. When the Rust Arena
/// runs all five this returns the whole catalog and [`bad_game`] becomes
/// [`BAD_GAME`] again, byte for byte.
fn runnable() -> Vec<Game> {
    GAMES.into_iter().filter(|g| crate::protocol::runs(g.as_str())).collect()
}

/// The unknown-game error. Over all of [`GAMES`] this is the Python's literal
/// byte for byte -- `quickplay_lists_what_this_arena_runs` pins that -- and the
/// client renders whatever comes back verbatim.
fn game_list(games: &[Game]) -> String {
    let names: Vec<&str> = games.iter().map(|g| g.short_name()).collect();
    match names.split_last() {
        Some((last, [])) => format!("Quick Play has {last}"),
        Some((last, rest)) => format!("Quick Play has {} and {last}", rest.join(", ")),
        None => "Quick Play is not available on this Arena".to_string(),
    }
}

/// ...narrowed to the games this Arena really runs.
fn bad_game() -> String {
    game_list(&runnable())
}

impl Game {
    /// No trimming and no lowercasing, matching `game not in GAMES`: " kart",
    /// "Kart" and "KART" all fall through to the 200 error envelope.
    fn parse(s: &str) -> Option<Game> {
        match s {
            "kart" => Some(Game::Kart),
            "plat" => Some(Game::Plat),
            "fps" => Some(Game::Fps),
            "golf" => Some(Game::Golf),
            "type" => Some(Game::Type),
            _ => None,
        }
    }

    /// The wire key.
    pub fn as_str(self) -> &'static str {
        match self {
            Game::Kart => "kart",
            Game::Plat => "plat",
            Game::Fps => "fps",
            Game::Golf => "golf",
            Game::Type => "type",
        }
    }

    /// How [`bad_game`] lists the game -- the short label the Python's literal
    /// uses, not the room's display name ("Platformer", not "Platformer Rush").
    fn short_name(self) -> &'static str {
        match self {
            Game::Kart => "Kart",
            Game::Plat => "Platformer",
            Game::Fps => "Blaster",
            Game::Golf => "Golf",
            Game::Type => "Code Typing Race",
        }
    }

    /// `NAMES`, only ever read by [`room_info`] -- this is where the display
    /// name of a Quick Play room is minted, and nowhere else.
    fn display_name(self) -> &'static str {
        match self {
            Game::Kart => "Kart Racing",
            Game::Plat => "Platformer Rush",
            Game::Fps => "Blaster Arena",
            Game::Golf => "Mini Golf",
            Game::Type => "Code Typing Race",
        }
    }
}

#[derive(Clone, Debug)]
struct Entry {
    /// When this user joined the queue. Never reset while they stay queued.
    at: f64,
    /// Last poll. The liveness heartbeat.
    seen: f64,
    /// Captured once, on first enqueue, and discarded after grouping.
    crew: Option<String>,
}

#[derive(Clone, Debug)]
struct MatchRec {
    room: String,
    game: Game,
    at: f64,
    /// `len(group)` frozen at match time -- NOT the live room occupancy.
    with: usize,
}

#[derive(Clone, Debug)]
struct RoomRec {
    game: Game,
    users: HashSet<String>,
    at: f64,
}

#[derive(Default)]
struct Inner {
    /// Indexed by `Game as usize`. A `Vec` rather than a map because the
    /// matchmaker sorts by `at` and Python's `sorted` is stable over a dict's
    /// *insertion-ordered* items: with a stepped clock a whole burst of joiners
    /// shares one `at`, and the tie has to break the way they joined.
    queues: [Vec<(String, Entry)>; 5],
    matched: HashMap<String, MatchRec>,
    rooms: HashMap<String, RoomRec>,
}

static STATE: LazyLock<Mutex<Inner>> = LazyLock::new(|| Mutex::new(Inner::default()));

/// A monotonic clock in seconds, offset like main.rs's game clock. Monotonic
/// and never wall-clock: no NTP step, no timezone, and none of these stamps are
/// persisted or returned -- only the derived `waitedSecs` leaves the process.
/// The pure logic below all takes `t` as an argument, which is how the tests
/// step the clock (the Python monkeypatches `_now` for the same reason).
fn now() -> f64 {
    static STARTED: LazyLock<Instant> = LazyLock::new(Instant::now);
    1000.0 + STARTED.elapsed().as_secs_f64()
}

/// Drop timed-out rooms, then timed-out match records. Both comparisons are
/// strictly greater, so a record exactly at its TTL survives.
///
/// The two TTLs disagree on purpose: after 90 seconds `status` says `idle`
/// while the room still admits you for the rest of its three hours. The
/// entitlement is the room; the match record is only the hand-off notification.
fn expire(inner: &mut Inner, t: f64) {
    inner.rooms.retain(|_, r| t - r.at <= ROOM_TTL);
    inner.matched.retain(|_, m| t - m.at <= MATCH_TTL);
}

/// The matchmaker for one game. May form several rooms in one call: the guard
/// is re-evaluated against the new oldest each time round, so a backlog of 17
/// drains as 8 + 8 with one left over rather than one room per poll.
fn form(inner: &mut Inner, game: Game, t: f64) {
    let gi = game as usize;
    // Stale sweep first, so an entry that stopped polling can never be matched.
    inner.queues[gi].retain(|(_, e)| t - e.seen <= STALE_SECS);

    while inner.queues[gi].len() >= 2 {
        let group: Vec<String> = {
            let q = &inner.queues[gi];
            let mut order: Vec<&(String, Entry)> = q.iter().collect();
            // Stable, so equal `at` keeps join order -- see the Inner::queues note.
            order.sort_by(|a, b| a.1.at.total_cmp(&b.1.at));
            let oldest = &order[0].1;
            // Not `break`: the Python returns, abandoning this game for the
            // tick rather than trying the second-oldest as a seed.
            if q.len() < MAX_GROUP && t - oldest.at < WAIT_SECS {
                return;
            }
            let mut group = vec![order[0].0.clone()];
            // Friends first: the oldest's crewmates, in `at` order, at most
            // MAX_GROUP - 1 of them. Truthiness, as in the Python: an empty
            // crew id counts as no crew (unreachable -- crew ids are uuids).
            if let Some(crew) = oldest.crew.as_deref().filter(|c| !c.is_empty()) {
                for (u, e) in order.iter().skip(1) {
                    if group.len() >= MAX_GROUP {
                        break;
                    }
                    if e.crew.as_deref() == Some(crew) {
                        group.push(u.clone());
                    }
                }
            }
            for (u, _) in order.iter().skip(1) {
                if group.len() >= MAX_GROUP {
                    break;
                }
                if !group.contains(u) {
                    group.push(u.clone());
                }
            }
            group
        };

        // The room id is the capability: a CSPRNG over 6 bytes, lowercase hex,
        // never a counter or a timestamp. The "qp_" prefix is what the
        // websocket route and the client's arenaIsQp() dispatch on.
        let mut bytes = [0u8; 6];
        rand::thread_rng().fill(&mut bytes);
        let room = format!("qp_{}", hex::encode(bytes));
        // Room row before the per-user match rows: the thing being handed out
        // exists before anyone is told to go there.
        inner.rooms.insert(
            room.clone(),
            RoomRec { game, users: group.iter().cloned().collect(), at: t },
        );
        for u in &group {
            inner.queues[gi].retain(|(k, _)| k != u);
            inner.matched.insert(
                u.clone(),
                MatchRec { room: room.clone(), game, at: t, with: group.len() },
            );
        }
    }
}

/// `status(uid)`: sweep, run every game's matchmaker, then answer for `uid`.
///
/// The order is the whole mechanism of "stopped polling -> dropped": the stale
/// sweep inside `form` runs *before* the caller's own `seen` is refreshed
/// below, so a caller who paused for longer than [`STALE_SECS`] is evicted by
/// their own request and gets `idle`. Refreshing first would make a lone queued
/// player immortal.
fn status_in(inner: &mut Inner, uid: &str, t: f64) -> Value {
    expire(inner, t);
    for g in GAMES {
        form(inner, g, t);
    }
    if let Some(m) = inner.matched.get(uid) {
        // Matched wins over waiting, and refreshes no `seen`.
        return json!({"state": "matched", "room": m.room,
                      "game": m.game.as_str(), "players": m.with});
    }
    for g in GAMES {
        let gi = g as usize;
        let waiting = inner.queues[gi].len();
        if let Some((_, e)) = inner.queues[gi].iter_mut().find(|(u, _)| u == uid) {
            e.seen = t;
            // Truncating seconds, as `int()` does: 7.9 reports 7. Rounding
            // would be off by one for most of every second, and the client
            // prints this verbatim.
            return json!({"state": "waiting", "game": g.as_str(),
                          "waiting": waiting, "waitedSecs": (t - e.at) as i64});
        }
    }
    json!({"state": "idle"})
}

/// Steps 3 and 4 of `join`, which run *before* the crew read: drop any match
/// this user had (the "re-queue me, I'm done with that match" path -- note it
/// does not touch `rooms`, so the old room still admits them) and take them out
/// of the other four queues. One user is in at most one queue, ever.
fn dequeue_elsewhere(inner: &mut Inner, uid: &str, game: Game) {
    inner.matched.remove(uid);
    for g in GAMES {
        if g != game {
            inner.queues[g as usize].retain(|(u, _)| u != uid);
        }
    }
}

/// Steps 6 and 7 of `join`: `setdefault` then an unconditional `seen` bump.
///
/// The insert is conditional and that is load-bearing. Re-joining the same game
/// keeps the existing entry, so `at` is not reset and `crew` is not refreshed:
/// a client that re-POSTs join instead of polling status cannot restart its own
/// queue position, and cannot game its way to the front either. The flip side
/// is that joining a crew while already queued needs a `leave` first.
fn enqueue(inner: &mut Inner, uid: &str, game: Game, crew: Option<String>, t: f64) {
    let q = &mut inner.queues[game as usize];
    if let Some((_, e)) = q.iter_mut().find(|(u, _)| u == uid) {
        e.seen = t;
    } else {
        q.push((uid.to_string(), Entry { at: t, seen: t, crew }));
    }
}

/// `leave(uid)`: forget the queue entry and the match, keep the room.
///
/// Fully idempotent -- no 404 for "you weren't queued", no 409 for "you're
/// already matched". It deliberately does not clock, sweep, match, or remove
/// the user from `rooms`: leaving after a match forgets the match but keeps the
/// room entitlement for the rest of [`ROOM_TTL`].
fn leave_in(inner: &mut Inner, uid: &str) -> Value {
    for g in GAMES {
        inner.queues[g as usize].retain(|(u, _)| u != uid);
    }
    inner.matched.remove(uid);
    json!({"state": "idle"})
}

fn admits_in(inner: &mut Inner, room_id: &str, uid: &str, t: f64) -> bool {
    expire(inner, t);
    inner.rooms.get(room_id).is_some_and(|r| r.users.contains(uid))
}

fn room_info_in(inner: &Inner, room_id: &str) -> Option<Value> {
    let r = inner.rooms.get(room_id)?;
    Some(json!({"kind": "quickplay", "id": room_id, "game": r.game.as_str(),
                "name": format!("Quick Play: {}", r.game.display_name())}))
}

/// The admission guard for a `qp_` room, and it is fail-closed: an unknown room
/// id, an expired room and a non-member all answer false.
///
/// The websocket route must `accept()` *first* and only then close with code
/// 4403 and reason "that Quick Play match isn't yours" -- accepting before
/// closing is what makes the browser report 4403 rather than a bare 1006.
pub fn admits(room_id: &str, uid: &str) -> bool {
    let mut inner = STATE.lock().expect("quickplay state");
    admits_in(&mut inner, room_id, uid, now())
}

/// The `roomInfo` the welcome frame carries for a `qp_` room, or None if the
/// room is unknown. Unlike [`admits`], this does not expire anything.
pub fn room_info(room_id: &str) -> Option<Value> {
    let inner = STATE.lock().expect("quickplay state");
    room_info_in(&inner, room_id)
}

// ------------------------------------------------------ inbound validation --

/// One pydantic-shaped error entry, carrying exactly the three keys
/// `_scrub_422` in app/main.py keeps, in its order. Pydantic's `input`, `ctx`
/// and `url` are deliberately absent: a rejected body is never reflected back.
#[derive(Debug, Serialize)]
struct VErr {
    loc: Vec<Value>,
    msg: String,
    #[serde(rename = "type")]
    kind: &'static str,
}

fn verr(loc: Vec<Value>, kind: &'static str, msg: impl Into<String>) -> VErr {
    VErr { loc, msg: msg.into(), kind }
}

/// The 422 body for a *validation* failure: a list of `{loc, msg, type}` under
/// `detail`, never the single-string envelope [`crate::err`] produces. Only an
/// explicit `raise HTTPException(422, "...")` is a string, and this group's
/// three routes contain none -- every 422 here comes from the model.
fn err422(errs: Vec<VErr>) -> Response {
    (StatusCode::UNPROCESSABLE_ENTITY, Json(json!({ "detail": errs }))).into_response()
}

/// The index pydantic puts beside "body" in a `json_invalid` `loc`.
///
/// Pydantic's parser counts one absolute offset in *characters*; serde reports
/// a 1-based line and byte column instead. So this walks to the start of the
/// reported line, adds the column back -- minus one, because the column points
/// *at* the byte that was not expected, except at end of input where there is
/// no such byte and it already points one past the last -- and then converts
/// the byte offset to a character count.
fn decode_offset(bytes: &[u8], e: &serde_json::Error) -> usize {
    let mut start = 0usize;
    for _ in 1..e.line() {
        match bytes[start..].iter().position(|&b| b == b'\n') {
            Some(i) => start += i + 1,
            None => break,
        }
    }
    let at = if matches!(e.classify(), serde_json::error::Category::Eof) {
        start + e.column()
    } else {
        start + e.column().saturating_sub(1)
    };
    // Every byte that is not a UTF-8 continuation byte starts a character.
    // Counting them is safe even if `at` landed mid-character.
    bytes[..at.min(bytes.len())].iter().filter(|b| *b & 0xC0 != 0x80).count()
}

/// `JoinBody`: one required `game: str = Field(max_length=8)` under
/// `extra="forbid"`. Returns the raw string -- the *allowlist* is not part of
/// validation and its failure is a 200, not a 422.
///
/// Hand-rolled rather than derived because the `type` and `msg` strings below
/// are pydantic-core's own and no derive produces them, and because the passes
/// have to run in pydantic's order: the declared field first, then whatever
/// keys are left over. serde_json's `preserve_order` keeps those leftovers in
/// body order, which is the order pydantic walks them in.
fn join_body(bytes: &[u8]) -> Result<String, Vec<VErr>> {
    // FastAPI reads a *zero-length* body as no body at all, and the required
    // model then reports itself missing rather than malformed. Taking the raw
    // bytes instead of axum's Json extractor is what makes that reachable: the
    // extractor would have answered 400 here and 415 on a wrong content type.
    // A body of only whitespace is NOT empty -- it is unparseable JSON, and
    // measurably 422s as `json_invalid` rather than `missing`.
    if bytes.is_empty() {
        return Err(vec![verr(vec![json!("body")], "missing", "Field required")]);
    }
    let body: Value = match serde_json::from_slice(bytes) {
        Ok(v) => v,
        Err(e) => {
            let pos = json!(decode_offset(bytes, &e));
            return Err(vec![verr(vec![json!("body"), pos], "json_invalid", "JSON decode error")]);
        }
    };
    // A literal `null` is a present-but-absent body, and reports as missing.
    if body.is_null() {
        return Err(vec![verr(vec![json!("body")], "missing", "Field required")]);
    }
    let Some(obj) = body.as_object() else {
        return Err(vec![verr(
            vec![json!("body")],
            "model_attributes_type",
            "Input should be a valid dictionary or object to extract fields from",
        )]);
    };

    let mut errs: Vec<VErr> = Vec::new();
    let game = match obj.get("game") {
        Some(Value::String(s)) => {
            // Characters, as pydantic counts them. There is no minimum, so ""
            // validates and falls through to the 200 error envelope.
            if s.chars().count() > MAX_GAME_LEN {
                errs.push(verr(
                    vec![json!("body"), json!("game")],
                    "string_too_long",
                    format!("String should have at most {MAX_GAME_LEN} characters"),
                ));
                None
            } else {
                Some(s.clone())
            }
        }
        None => {
            errs.push(verr(vec![json!("body"), json!("game")], "missing", "Field required"));
            None
        }
        // null included: `game` is required and untyped-None is a type error,
        // not a missing field.
        Some(_) => {
            errs.push(verr(
                vec![json!("body"), json!("game")],
                "string_type",
                "Input should be a valid string",
            ));
            None
        }
    };
    for k in obj.keys().filter(|k| k.as_str() != "game") {
        errs.push(verr(
            vec![json!("body"), json!(k)],
            "extra_forbidden",
            "Extra inputs are not permitted",
        ));
    }
    match game {
        Some(g) if errs.is_empty() => Ok(g),
        _ => Err(errs),
    }
}

// ------------------------------------------------------------------- routes --

/// POST /v1/quickplay/join. Answers 200 in every outcome, including an unknown
/// game: there is no 4xx for a bad game name, only an error envelope.
async fn join(State(st): State<crate::AppState>, req: Request) -> Response {
    let c = req.extensions().get::<crate::Caller>().expect("caller set by middleware").clone();
    let (_, body) = req.into_parts();
    let bytes = match axum::body::to_bytes(body, 8 * 1024 * 1024).await {
        Ok(b) => b,
        Err(_) => return crate::err(StatusCode::BAD_REQUEST, "body too large"),
    };
    // The whole model runs here, and before the allowlist: Pydantic validates
    // before the service does, so an over-long name is a 422 (a *list* of
    // entries, as every pydantic rejection is) and an unknown name is a 200.
    let raw_game = match join_body(&bytes) {
        Ok(g) => g,
        Err(errs) => return err422(errs),
    };
    // Both rejections are returned before anything is clocked or touched: the
    // caller is not dequeued, their match is not dropped, their `seen` is not
    // refreshed. A game this Arena cannot referee is rejected the same way as
    // one that does not exist -- the alternative is a match into a room where
    // nothing ever starts.
    let game = match Game::parse(&raw_game) {
        Some(g) if crate::protocol::runs(g.as_str()) => g,
        _ => return Json(json!({"state": "error", "error": bad_game()})).into_response(),
    };

    // One timestamp for the whole call, as the Python's single `_now()`.
    let t = now();
    // The lock is taken, dropped for the await, and taken again: holding it
    // across the database read would serialise every joiner behind one query.
    {
        let mut inner = STATE.lock().expect("quickplay state");
        dequeue_elsewhere(&mut inner, &c.user_id, game);
    }
    // The group's only database access, one primary-key read. It happens
    // unconditionally even though `enqueue` throws the answer away for someone
    // already queued -- keeping it here keeps join's observable order intact.
    let read = sqlx::query("SELECT crew_id FROM crew_members WHERE user_id = ?1")
        .bind(&c.user_id)
        .fetch_optional(&st.pool)
        .await;
    let crew: Option<String> = match read {
        Ok(row) => row.map(|r| r.get("crew_id")),
        Err(e) => return crate::err(StatusCode::INTERNAL_SERVER_ERROR, &format!("db error: {e}")),
    };

    let out = {
        let mut inner = STATE.lock().expect("quickplay state");
        enqueue(&mut inner, &c.user_id, game, crew, t);
        // join ends by asking status, so it is not a write-only endpoint: the
        // eighth joiner is matched by their own POST and never sees "waiting",
        // and a joiner whose clock has jumped can even be swept back to "idle".
        status_in(&mut inner, &c.user_id, t)
    };
    Json(out).into_response()
}

/// GET /v1/quickplay/status. No parameters; the client polls it every 2 s while
/// it believes it is waiting.
async fn status(req: Request) -> Response {
    let c = req.extensions().get::<crate::Caller>().expect("caller set by middleware").clone();
    let out = {
        let mut inner = STATE.lock().expect("quickplay state");
        status_in(&mut inner, &c.user_id, now())
    };
    Json(out).into_response()
}

/// POST /v1/quickplay/leave. The route declares no body and the client always
/// sends `{}`, so the body is read and discarded -- parsing it would turn an
/// endpoint that cannot fail into one that 422s.
async fn leave(req: Request) -> Response {
    let c = req.extensions().get::<crate::Caller>().expect("caller set by middleware").clone();
    let out = {
        let mut inner = STATE.lock().expect("quickplay state");
        leave_in(&mut inner, &c.user_id)
    };
    Json(out).into_response()
}

/// The group's routes. main.rs merges this into the guarded router, which is
/// where `require_device` is applied -- not here.
pub fn routes() -> Router<crate::AppState> {
    Router::new()
        .route("/v1/quickplay/join", post(join))
        .route("/v1/quickplay/status", get(status))
        .route("/v1/quickplay/leave", post(leave))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(inner: &Inner, game: Game) -> Vec<String> {
        inner.queues[game as usize].iter().map(|(u, _)| u.clone()).collect()
    }

    #[test]
    fn quickplay_lists_what_this_arena_runs() {
        // Over the whole catalog, the Python's literal, byte for byte.
        assert_eq!(
            game_list(&GAMES),
            "Quick Play has Kart, Platformer, Blaster, Golf and Code Typing Race"
        );
        // And the queue is only open for games the engine can referee, so
        // nobody is matched into a room where nothing starts.
        let open = runnable();
        assert!(open.iter().all(|g| crate::protocol::runs(g.as_str())));
        assert!(open.contains(&Game::Kart), "kart is refereed, so it must be offered");
        for g in GAMES {
            if !crate::protocol::runs(g.as_str()) {
                assert!(!open.contains(&g), "{} is offered but cannot run", g.as_str());
                assert!(!bad_game().contains(g.short_name()));
            }
        }
        // English, not a comma-spliced list, however many are left.
        assert_eq!(game_list(&[Game::Kart]), "Quick Play has Kart");
        assert_eq!(game_list(&[Game::Kart, Game::Fps]), "Quick Play has Kart and Blaster");
        assert_eq!(game_list(&[]), "Quick Play is not available on this Arena");
    }

    #[test]
    fn the_three_routes_build_a_router() {
        // axum panics on a bad path or a duplicate route, so building it is
        // the whole assertion.
        let _router: Router<crate::AppState> = routes();
    }

    #[test]
    fn the_length_cap_and_the_allowlist_are_different_failures() {
        // "chess" is 5 characters: it passes validation and fails the allowlist.
        assert!("chess".chars().count() <= MAX_GAME_LEN);
        assert_eq!(Game::parse("chess"), None);
        // "platformer" is 10: it never reaches the allowlist at all.
        assert!("platformer".chars().count() > MAX_GAME_LEN);
        // No trimming and no case folding anywhere.
        assert_eq!(Game::parse(" kart"), None);
        assert_eq!(Game::parse("Kart"), None);
        assert_eq!(Game::parse("KART"), None);
        assert_eq!(Game::parse("kart"), Some(Game::Kart));
        assert_eq!(Game::parse("plat"), Some(Game::Plat));
    }

    fn bad(bytes: &[u8]) -> Vec<VErr> {
        join_body(bytes).expect_err("should not validate")
    }

    fn kinds(errs: &[VErr]) -> Vec<&'static str> {
        errs.iter().map(|e| e.kind).collect()
    }

    #[test]
    fn the_join_body_refuses_extras_missing_and_non_strings() {
        assert_eq!(join_body(br#"{"game":"kart"}"#).expect("valid"), "kart");
        assert_eq!(kinds(&bad(br#"{"game":"kart","room":"qp_x"}"#)), ["extra_forbidden"]);
        assert_eq!(kinds(&bad(br#"{}"#)), ["missing"]);
        assert_eq!(kinds(&bad(br#"{"game":123}"#)), ["string_type"]);
        // null is a type error, not a missing field: `game` is required.
        assert_eq!(kinds(&bad(br#"{"game":null}"#)), ["string_type"]);
        assert_eq!(kinds(&bad(b"[]")), ["model_attributes_type"]);
        // No minimum on the field, so the empty string validates and is only
        // then refused by the allowlist, with a 200.
        assert_eq!(join_body(br#"{"game":""}"#).expect("no minimum"), "");
        assert_eq!(Game::parse(""), None);
    }

    #[test]
    fn a_422_is_a_list_of_loc_msg_type_entries_not_a_string() {
        // The over-long name: pydantic's own wording, and loc down to the field.
        assert_eq!(
            serde_json::to_value(bad(br#"{"game":"platformer"}"#)).expect("json"),
            json!([{"loc": ["body", "game"],
                    "msg": "String should have at most 8 characters",
                    "type": "string_too_long"}])
        );
        // Several errors at once, the declared field ahead of the leftovers.
        assert_eq!(
            kinds(&bad(br#"{"game":"platformer","room":"qp_x"}"#)),
            ["string_too_long", "extra_forbidden"]
        );
        // No body at all reports the model missing, at loc ["body"].
        assert_eq!(
            serde_json::to_value(bad(b"")).expect("json"),
            json!([{"loc": ["body"], "msg": "Field required", "type": "missing"}])
        );
        // A literal null is a present body that reports the model missing;
        // whitespace is not an empty body at all, it is unparseable.
        assert_eq!(kinds(&bad(b"null")), ["missing"]);
        assert_eq!(kinds(&bad(b"   ")), ["json_invalid"]);
    }

    /// Every offset below was measured against FastAPI's own 422 for the same
    /// body -- see the `loc` second element, which is pydantic's absolute index
    /// into the body and not a line or a column.
    #[test]
    fn malformed_json_carries_pydantics_offset_beside_body() {
        let off = |b: &[u8]| {
            let e = bad(b);
            assert_eq!(e.len(), 1);
            assert_eq!(e[0].kind, "json_invalid");
            assert_eq!(e[0].msg, "JSON decode error");
            assert_eq!(e[0].loc[0], json!("body"));
            e[0].loc[1].as_u64().expect("an offset")
        };
        // Truncated: the offset is the length, one past the last byte.
        assert_eq!(off(b" "), 1);
        assert_eq!(off(b"   "), 3);
        assert_eq!(off(b"{"), 1);
        assert_eq!(off(br#"{"game""#), 7);
        assert_eq!(off(br#"{"game":"#), 8);
        assert_eq!(off(br#"{"game":"kart""#), 14);
        assert_eq!(off(b"[1,"), 3);
        // An unexpected byte: the offset points at it.
        assert_eq!(off(br#"{"game":}"#), 8);
        assert_eq!(off(b"xyz"), 0);
        assert_eq!(off(br#"{"a":1}{"b":2}"#), 7);
        // Several lines: one absolute index, so the line offset is added back.
        assert_eq!(off(b"{\n\"game\":\n}"), 10);
        assert_eq!(off(b"{\n  \"game\": \"kart\",\n  \"x\"\n}"), 26);
        // Characters, not bytes: the two-byte e-acute counts once.
        assert_eq!(off("{\"g\u{e9}ame\":}".as_bytes()), 9);
    }

    #[test]
    fn two_queued_match_once_the_oldest_has_waited_exactly_the_wait() {
        let mut s = Inner::default();
        enqueue(&mut s, "ann", Game::Kart, None, 1000.0);
        enqueue(&mut s, "bob", Game::Kart, None, 1001.0);
        // Under the wait: still waiting, and waitedSecs truncates rather than rounds.
        let w = status_in(&mut s, "ann", 1007.9);
        assert_eq!(w, json!({"state": "waiting", "game": "kart",
                             "waiting": 2, "waitedSecs": 7}));
        // Exactly WAIT_SECS matches: the guard is a strict `<`.
        let m = status_in(&mut s, "ann", 1000.0 + WAIT_SECS);
        assert_eq!(m["state"], "matched");
        assert_eq!(m["game"], "kart");
        assert_eq!(m["players"], 2);
        let room = m["room"].as_str().expect("room id").to_string();
        assert!(room.starts_with("qp_"));
        assert_eq!(room.len(), 15);
        assert!(room[3..].chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
        // Both of them are told the same room, and nobody is left queued.
        assert_eq!(status_in(&mut s, "bob", 1008.0)["room"], m["room"]);
        assert!(q(&s, Game::Kart).is_empty());
    }

    #[test]
    fn a_full_group_matches_at_once_without_waiting() {
        let mut s = Inner::default();
        for i in 0..9 {
            enqueue(&mut s, &format!("p{i}"), Game::Type, None, 5000.0);
        }
        // Nine queued at the same instant: eight match immediately because the
        // queue is at MAX_GROUP, and the ninth is left waiting on the clock.
        let m = status_in(&mut s, "p0", 5000.0);
        assert_eq!(m["state"], "matched");
        assert_eq!(m["players"], 8);
        assert_eq!(q(&s, Game::Type), ["p8"]);
        assert_eq!(status_in(&mut s, "p8", 5000.0),
                   json!({"state": "waiting", "game": "type",
                          "waiting": 1, "waitedSecs": 0}));
    }

    #[test]
    fn a_caller_who_stopped_polling_is_swept_by_their_own_request() {
        let mut s = Inner::default();
        enqueue(&mut s, "ann", Game::Golf, None, 1000.0);
        assert_eq!(status_in(&mut s, "ann", 1000.0)["state"], "waiting");
        // The sweep runs before the caller's own `seen` is refreshed, so
        // polling late does not revive them.
        assert_eq!(status_in(&mut s, "ann", 1000.0 + STALE_SECS + 1.0),
                   json!({"state": "idle"}));
        assert!(q(&s, Game::Golf).is_empty());
    }

    #[test]
    fn a_poll_that_stays_inside_the_stale_window_keeps_the_entry_alive() {
        let mut s = Inner::default();
        enqueue(&mut s, "ann", Game::Golf, None, 1000.0);
        let mut t = 1000.0;
        for _ in 0..5 {
            t += STALE_SECS - 1.0;
            assert_eq!(status_in(&mut s, "ann", t)["state"], "waiting");
        }
        // Alone in the queue, so 70 seconds of waiting still forms nothing.
        assert_eq!(status_in(&mut s, "ann", t)["waitedSecs"], 70);
    }

    #[test]
    fn crewmates_take_the_last_seats_ahead_of_people_who_queued_earlier() {
        let mut s = Inner::default();
        let owls = Some("owls".to_string());
        enqueue(&mut s, "ann", Game::Fps, owls.clone(), 1000.0);
        for i in 0..7 {
            enqueue(&mut s, &format!("n{i}"), Game::Fps, None, 1001.0 + i as f64);
        }
        enqueue(&mut s, "zed", Game::Fps, owls, 1008.0);
        // Nine queued: the oldest seeds the group, her one crewmate follows
        // even though he joined last, and the fill stops at eight.
        let m = status_in(&mut s, "ann", 1008.0);
        assert_eq!(m["players"], 8);
        assert_eq!(status_in(&mut s, "zed", 1008.0)["room"], m["room"]);
        // n6 queued before zed and is the one left behind.
        assert_eq!(q(&s, Game::Fps), ["n6"]);
    }

    #[test]
    fn one_form_call_drains_a_backlog_into_several_rooms() {
        let mut s = Inner::default();
        for i in 0..17 {
            enqueue(&mut s, &format!("p{i}"), Game::Kart, None, 1000.0);
        }
        // 17 waiting at once: 8 + 8, then one left because a queue of one
        // cannot match. Forming a single group per poll would drain far slower.
        form(&mut s, Game::Kart, 1000.0);
        assert_eq!(q(&s, Game::Kart).len(), 1);
        let rooms: HashSet<String> =
            s.matched.values().map(|m| m.room.clone()).collect();
        assert_eq!(rooms.len(), 2);
        assert_eq!(s.matched.len(), 16);
        assert!(s.matched.values().all(|m| m.with == 8));
    }

    #[test]
    fn join_keeps_the_original_at_and_crew() {
        let mut s = Inner::default();
        enqueue(&mut s, "ann", Game::Kart, Some("owls".to_string()), 1000.0);
        // Re-joining the same game refreshes only the heartbeat.
        enqueue(&mut s, "ann", Game::Kart, None, 1005.0);
        let (_, e) = &s.queues[Game::Kart as usize][0];
        assert_eq!(s.queues[Game::Kart as usize].len(), 1);
        assert_eq!(e.at, 1000.0);
        assert_eq!(e.seen, 1005.0);
        assert_eq!(e.crew.as_deref(), Some("owls"));
    }

    #[test]
    fn a_user_is_only_ever_in_one_queue() {
        let mut s = Inner::default();
        enqueue(&mut s, "ann", Game::Kart, None, 1000.0);
        dequeue_elsewhere(&mut s, "ann", Game::Golf);
        enqueue(&mut s, "ann", Game::Golf, None, 1001.0);
        assert!(q(&s, Game::Kart).is_empty());
        assert_eq!(q(&s, Game::Golf), ["ann"]);
        // leave clears all five, whichever one you were in.
        assert_eq!(leave_in(&mut s, "ann"), json!({"state": "idle"}));
        assert!(GAMES.into_iter().all(|g| q(&s, g).is_empty()));
    }

    #[test]
    fn re_joining_drops_the_match_but_not_the_room_entitlement() {
        let mut s = Inner::default();
        enqueue(&mut s, "ann", Game::Golf, None, 1000.0);
        enqueue(&mut s, "bob", Game::Golf, None, 1000.0);
        let room = status_in(&mut s, "ann", 1000.0 + WAIT_SECS)["room"]
            .as_str()
            .expect("room id")
            .to_string();
        dequeue_elsewhere(&mut s, "ann", Game::Kart);
        assert!(!s.matched.contains_key("ann"));
        assert!(admits_in(&mut s, &room, "ann", 1008.0));
        // And so does leaving.
        leave_in(&mut s, "bob");
        assert!(admits_in(&mut s, &room, "bob", 1008.0));
    }

    #[test]
    fn the_match_record_expires_long_before_the_room_does() {
        let mut s = Inner::default();
        enqueue(&mut s, "ann", Game::Golf, None, 1000.0);
        enqueue(&mut s, "bob", Game::Golf, None, 1000.0);
        let room = status_in(&mut s, "ann", 1008.0)["room"].as_str().expect("room").to_string();
        // Past MATCH_TTL: status forgets, the room does not.
        let t = 1008.0 + MATCH_TTL + 1.0;
        assert_eq!(status_in(&mut s, "ann", t), json!({"state": "idle"}));
        assert!(admits_in(&mut s, &room, "ann", t));
        // Past ROOM_TTL the entitlement goes too, and it is fail-closed.
        let t = 1008.0 + ROOM_TTL + 1.0;
        assert!(!admits_in(&mut s, &room, "ann", t));
        assert!(!admits_in(&mut s, "qp_000000000000", "ann", t));
    }

    #[test]
    fn a_room_is_only_for_the_people_matched_into_it() {
        let mut s = Inner::default();
        enqueue(&mut s, "ann", Game::Golf, None, 1000.0);
        enqueue(&mut s, "bob", Game::Golf, None, 1000.0);
        let room = status_in(&mut s, "ann", 1008.0)["room"].as_str().expect("room").to_string();
        assert!(admits_in(&mut s, &room, "ann", 1008.0));
        assert!(admits_in(&mut s, &room, "bob", 1008.0));
        assert!(!admits_in(&mut s, &room, "cat", 1008.0));
        assert_eq!(
            room_info_in(&s, &room),
            Some(json!({"kind": "quickplay", "id": room.as_str(),
                        "game": "golf", "name": "Quick Play: Mini Golf"}))
        );
        assert_eq!(room_info_in(&s, "qp_000000000000"), None);
    }

    /// The only test that touches the process-wide state, to cover the two
    /// functions the websocket route calls. Its room id is unique, so it
    /// cannot disturb (or be disturbed by) anything running beside it.
    #[test]
    fn the_shared_state_answers_the_websocket_route() {
        let room = "qp_aaaaaaaaaaaa";
        {
            let mut inner = STATE.lock().expect("quickplay state");
            inner.rooms.insert(
                room.to_string(),
                RoomRec {
                    game: Game::Type,
                    users: ["ws-member".to_string()].into_iter().collect(),
                    at: now(),
                },
            );
        }
        assert!(admits(room, "ws-member"));
        assert!(!admits(room, "ws-stranger"));
        assert_eq!(
            room_info(room),
            Some(json!({"kind": "quickplay", "id": room, "game": "type",
                        "name": "Quick Play: Code Typing Race"}))
        );
        assert_eq!(room_info("qp_bbbbbbbbbbbb"), None);
    }
}
