//! The live leaderboard stream, ported from `app/routes/board.py`.
//!
//! One endpoint, `GET /v1/board/stream`, which pushes the same body as
//! `GET /v1/board` over Server-Sent Events. A frame is the *whole* board every
//! time -- no diffing, no caching -- so a client never has to reconcile a patch
//! against state it might have missed.
//!
//! Two things about the cadence are easy to get wrong by reading the Python
//! loop too quickly. A keepalive is never the last thing on the wire: a full
//! board follows it immediately, because the Python emits the keepalive from
//! its `except` and then falls back to the top of the `while`. So an idle arena
//! still sees `board, 25s, keepalive, board, 25s, keepalive, ...`, and streaks
//! and window rollovers keep refreshing with nothing happening. And `today` is
//! recomputed every tick, so a stream held across UTC midnight silently rolls
//! its own window -- `startsOn`/`endsOn` ride in every frame for exactly that
//! reason, and a client must not treat them as fixed for the life of a stream.
//!
//! Privacy: a frame is `service::BoardResponse` and nothing else. No user id,
//! github id, device id, device label, token or token hash is ever on this
//! wire, entries are keyed by `handle`, and `viewer_id` is used only to set
//! `isYou` -- never to filter, hide or reorder anyone. The board is
//! public-within-the-arena by design.

use axum::{
    body::Body,
    extract::{Query, Request, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use serde::Deserialize;
use serde_json::json;
use sqlx::SqlitePool;
use std::convert::Infallible;
use std::sync::OnceLock;
use std::time::Duration;
use tokio::sync::broadcast;

/// Long enough to stay quiet, short enough that proxies do not drop the stream.
///
/// It doubles as the worst-case staleness of the whole feature: the hub is
/// in-process, so an ingest handled by the *other* backend (Python and Rust run
/// side by side) never wakes a stream held here. The heartbeat is the only
/// thing keeping that a latency bug rather than a correctness one -- a second
/// reason not to lengthen it. Changing it also changes how often every
/// connected client re-renders.
const HEARTBEAT_SECS: u64 = 25;

/// Mirrors the Python queue's `maxsize=8`. Small on purpose: see [`publish_if_accepted`].
const HUB_CAPACITY: usize = 8;

/// Same `{"detail": ...}` envelope as main.rs's `err`. Duplicated rather than
/// widening main.rs, so this module depends on nothing of it but `AppState`
/// and `Caller`.
fn err(code: StatusCode, msg: &str) -> Response {
    (code, Json(json!({ "detail": msg }))).into_response()
}

/// Only this route. main.rs puts the whole guarded router behind
/// `require_device`, so a layer here would run the middleware twice and double
/// the `last_seen_at` write.
pub fn routes() -> Router<crate::AppState> {
    Router::new().route("/v1/board/stream", get(stream_board))
}

// --- the hub --------------------------------------------------------------

/// Process-global fan-out, matching the Python's module-level `board_hub`.
///
/// It cannot live on `AppState` -- that struct is main.rs's and this module may
/// not touch it -- and it does not need to: the hub is a singleton in the
/// Python too, not request state.
static HUB: OnceLock<broadcast::Sender<()>> = OnceLock::new();

fn hub() -> &'static broadcast::Sender<()> {
    // The receiver from `channel` is dropped at once; `Sender::subscribe`
    // works with no listeners, which is the normal state of an idle arena.
    HUB.get_or_init(|| broadcast::channel(HUB_CAPACITY).0)
}

/// Wake every open stream after an ingest, the way `POST /v1/stats` does.
///
/// Three rules are baked into this signature. Publish only when a day was
/// actually accepted -- a submission whose every day was rejected (future
/// date, too old, over a cap) changed no board, and `accepted == 0` is falsy
/// in the Python, so the test is a strict `> 0`. Publish only *after* the
/// ingest transaction has committed, or a woken stream reads the pre-commit
/// board and renders stale numbers as fresh. And the nudge carries no payload:
/// the Python's `"stats"` string is read and discarded by the reader, so there
/// is nothing here to describe what changed and nothing to trust -- every tick
/// rebuilds the whole board from the database regardless.
///
/// Dropping a nudge is correct, not a bug to fix. `broadcast` never blocks and
/// never errors a publisher because a subscriber is slow, exactly as
/// `put_nowait` swallowed `QueueFull`: a lagging reader misses a wake-up and
/// the next tick, at most [`HEARTBEAT_SECS`] later, carries the current state.
///
/// Nothing calls this yet -- `POST /v1/stats` lives in main.rs, which is not
/// this module's file to edit, hence the `allow`. Until someone wires it, Rust
/// streams refresh on the heartbeat only and never instantly on ingest.
pub fn publish_if_accepted(accepted: i64) {
    if accepted > 0 {
        // Err only means nobody is streaming right now.
        let _ = hub().send(());
    }
}

// --- framing --------------------------------------------------------------

/// A board frame: one `data:` line and a blank line. No `event:`, no `id:`, no
/// `retry:` -- the existing client reads exactly this shape.
fn board_frame(json: &str) -> String {
    format!("data: {json}\n\n")
}

/// An SSE *comment*, so an `EventSource` reader sees nothing at all.
///
/// Written by hand rather than with `axum::response::sse::Event::comment`,
/// which serialises as `:keepalive` with no space after the colon. The Python
/// sends the space, and these bytes are the contract.
const KEEPALIVE_FRAME: &str = ": keepalive\n\n";

/// What the loop owes the client next.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Due {
    /// Emit a board immediately, without waiting.
    Board,
    /// Wait for a nudge or the heartbeat before emitting anything.
    Wait,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Frame {
    Board,
    Keepalive,
}

/// The cadence, as a pure function so it can be tested without a clock or a
/// database. `woken` is "the hub nudged us" as opposed to "the heartbeat
/// expired".
///
/// The timeout arm is the subtle one: it emits the keepalive and leaves
/// `Due::Board` behind, which is how the Python returns to the top of its
/// `while` and re-sends a full board right after every keepalive. An
/// implementation that only re-sent on a nudge would stop refreshing streaks
/// and window rollovers on an idle arena.
fn advance(due: Due, woken: bool) -> (Due, Frame) {
    match due {
        Due::Board => (Due::Wait, Frame::Board),
        Due::Wait if woken => (Due::Wait, Frame::Board),
        Due::Wait => (Due::Board, Frame::Keepalive),
    }
}

// --- the stream -----------------------------------------------------------

struct StreamState {
    pool: SqlitePool,
    window: String,
    /// The only thing kept from the Caller. Everything on the wire -- handles,
    /// display names, avatars -- is re-read from the database each tick.
    user_id: String,
    /// The subscription lives in the stream's state so it is dropped when the
    /// response body is dropped: client disconnect, cancellation and error all
    /// unsubscribe, which is what the Python's `finally` guarantees. Leaking it
    /// would cost every future publish a little more work, forever.
    ///
    /// `None` until the first pull -- see [`next_frame`].
    rx: Option<broadcast::Receiver<()>>,
    due: Due,
}

/// Wait for a nudge; `true` means one arrived, `false` means the heartbeat won.
async fn wait_for_nudge(rx: &mut broadcast::Receiver<()>) -> bool {
    let beat = Duration::from_secs(HEARTBEAT_SECS);
    match tokio::time::timeout(beat, rx.recv()).await {
        // A nudge and a lagged subscription mean the same thing: refresh now.
        // Lagging is not an error here -- the payload never said what changed,
        // so there is nothing to have lost.
        Ok(Ok(())) | Ok(Err(broadcast::error::RecvError::Lagged(_))) => true,
        // The sender is a `'static` singleton, so this is unreachable. Were it
        // ever possible, `recv()` would return instantly forever and spin the
        // loop, so fall back to a plain heartbeat rather than hot-looping.
        Ok(Err(broadcast::error::RecvError::Closed)) => {
            tokio::time::sleep(beat).await;
            false
        }
        Err(_) => false,
    }
}

/// Rebuild the whole board and render its frame. `None` ends the stream.
///
/// There is deliberately no error status here. The Python's generator first
/// touches the database after `StreamingResponse` has already committed the 200
/// and the headers, so a query failure truncates the body rather than becoming
/// a 5xx: the client sees a cut stream and reconnects. Reproducing that is the
/// whole reason the window check happens eagerly in the handler instead.
///
/// Each call finishes its database work before anything waits, so no pooled
/// connection is held across the heartbeat. That matters: the pool is five
/// connections, and a connection parked per stream would deadlock the server
/// at five concurrent viewers.
///
/// Takes the three fields it needs rather than `&StreamState`, so no borrow of
/// the whole state -- and in particular none of the subscription -- is held
/// across the `.await`.
async fn board_frame_for(pool: &SqlitePool, window: &str, user_id: &str) -> Option<String> {
    let board = match crate::service::build_board(pool, window, Some(user_id)).await {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!("board stream ended: {e}");
            return None;
        }
    };
    match serde_json::to_string(&board) {
        Ok(json) => Some(board_frame(&json)),
        Err(e) => {
            tracing::warn!("board stream ended: could not serialise board: {e}");
            None
        }
    }
}

async fn next_frame(mut s: StreamState) -> Option<(Result<String, Infallible>, StreamState)> {
    // Subscribe on the first pull, not at request time: `queue =
    // board_hub.subscribe()` is the first line of the Python's generator, so a
    // nudge published between the 200 and this moment is missed. Reproduced
    // rather than "fixed" so the frame counts match, and harmless anyway --
    // the frame this pull is about to build is a fresh board regardless.
    let rx = s.rx.get_or_insert_with(|| hub().subscribe());
    // Only `Due::Wait` blocks. `Due::Board` emits at once, which is what puts
    // a board right after every keepalive with no 25-second gap between them.
    let woken = match s.due {
        Due::Board => false,
        Due::Wait => wait_for_nudge(rx).await,
    };
    let (next, frame) = advance(s.due, woken);
    s.due = next;
    let text = match frame {
        Frame::Board => board_frame_for(&s.pool, &s.window, &s.user_id).await?,
        Frame::Keepalive => KEEPALIVE_FRAME.to_string(),
    };
    Some((Ok(text), s))
}

/// Same shape as main.rs's `/v1/board` query: FastAPI ignores unknown and
/// extra query params, and an absent `window` defaults to `"season"`.
#[derive(Deserialize)]
struct WindowQ {
    #[serde(default)]
    window: Option<String>,
}

async fn stream_board(
    State(st): State<crate::AppState>,
    Query(q): Query<WindowQ>,
    req: Request,
) -> Response {
    let c = req
        .extensions()
        .get::<crate::Caller>()
        .expect("caller set by middleware")
        .clone();

    // Eagerly, before a single byte of the stream exists. A bad window has to
    // be a real 400 with a JSON body -- the client tells an error from a board
    // by status code, and once the 200 is out there is no way to report one.
    // It is also the only thing standing between `window=garbage` and a
    // quietly month-to-date board, because `window_range` treats every
    // unrecognised string as "season".
    let window = q.window.unwrap_or_else(|| "season".into());
    if !crate::service::WINDOWS.contains(&window.as_str()) {
        return err(
            StatusCode::BAD_REQUEST,
            &format!("window must be one of {}", crate::service::WINDOWS.join(", ")),
        );
    }

    // The caller is authorised exactly once, here. A device revoked or a user
    // deactivated mid-stream keeps receiving boards until it disconnects; that
    // is the Python's behaviour, and re-authorising per tick would change the
    // wire timing and add a query to every tick.
    let state = StreamState {
        pool: st.pool,
        window,
        user_id: c.user_id,
        rx: None,
        due: Due::Board,
    };

    let body = Body::from_stream(futures::stream::unfold(state, next_frame));
    // Built by hand instead of with axum's `Sse`, which sends
    // `cache-control: no-cache` and no `x-accel-buffering`. The latter is what
    // stops nginx buffering the stream into uselessness.
    match Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream; charset=utf-8")
        .header(header::CACHE_CONTROL, "no-store")
        .header("x-accel-buffering", "no")
        .body(body)
    {
        Ok(r) => r,
        // Every header above is a literal, so this is unreachable; a 500 still
        // beats a panic for the one case where it is not.
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, &format!("{e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_error_matches_python() {
        // f"window must be one of {', '.join(WINDOWS)}", in declared order.
        assert_eq!(
            format!("window must be one of {}", crate::service::WINDOWS.join(", ")),
            "window must be one of season, 30d, 7d, all"
        );
    }

    #[test]
    fn frames_are_byte_exact() {
        assert_eq!(board_frame(r#"{"window":"7d"}"#), "data: {\"window\":\"7d\"}\n\n");
        // The space after the colon is the Python's; `Event::comment` omits it.
        assert_eq!(KEEPALIVE_FRAME, ": keepalive\n\n");
        assert_eq!(KEEPALIVE_FRAME.len(), 13);
    }

    #[test]
    fn idle_cadence_resends_a_board_after_every_keepalive() {
        let mut due = Due::Board;
        let seen: Vec<Frame> = (0..5)
            .map(|_| {
                let (next, frame) = advance(due, false);
                due = next;
                frame
            })
            .collect();
        assert_eq!(
            seen,
            vec![Frame::Board, Frame::Keepalive, Frame::Board, Frame::Keepalive, Frame::Board]
        );
    }

    #[test]
    fn a_nudge_sends_a_board_and_keeps_waiting() {
        assert_eq!(advance(Due::Wait, true), (Due::Wait, Frame::Board));
    }

    #[test]
    fn only_an_accepted_ingest_wakes_a_stream() {
        // The hub is process-global, so this is the only test that publishes.
        let mut rx = hub().subscribe();
        publish_if_accepted(0);
        assert!(rx.try_recv().is_err(), "a fully rejected ingest must not wake a stream");
        publish_if_accepted(1);
        assert!(rx.try_recv().is_ok(), "one accepted day must wake every stream");
    }
}
