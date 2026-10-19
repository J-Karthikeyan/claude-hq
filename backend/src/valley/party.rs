//! Party Mode (HQ 2.1): a playlist of the room's games with one combined score.
//!
//! Ported from backend/app/party.py in full, plus the party short-circuit at the
//! top of `valley.handle` (valley.py:1000) and `partymod.on_done` as `_flush`
//! calls it (valley.py:1610).
//!
//! Someone in a room starts a party; the room then plays Kart Racing, Platformer
//! Rush, Blaster Arena and Mini Golf in that order. Each finished game (its
//! "done" event, the same one the results are kept from) gives points by place,
//! 10-8-6-5-4-3-2-1, and moves the party on to the next game. The Arena keeps
//! the score; the page only shows it. In memory, per room.
//!
//! AUDIENCE: `start` and `stop` and every `on_done` message go to the WHOLE
//! ROOM (`out.all`) -- party has no lobby at all, so there is no lobby audience;
//! `view` and the three errors answer the asking PERSON (see the divergence note
//! on [`op`]). `a_party_start_goes_to_the_whole_room` pins the broadcast.
//!
//! THIS FILE'S SHAPE IS NOT THE OTHER EIGHT ENGINES'. party is not in
//! [`super::GAMES`], has no lobby, needs no join, and its state is
//! MODULE-LEVEL ([`PARTIES`], Python's `_parties` at party.py:17) rather than a
//! [`super::RoomValley`] field, because Python keys it by room id in a global
//! dict and `handle` reaches it before any room state is created.
//!
//! The shared divergences are listed once at the top of `valley.rs`; this file
//! adds four of its own, each commented where it bites: the single-socket
//! audience ([`op`]), the monotonic clock ([`now`]), the unreachable
//! negative-place index ([`on_done`]) and golf's missing results branch
//! ([`golf_rows`]).

use super::{Out, Seq};
use crate::rooms::Member;
use serde_json::{json, Value};
use std::cmp::Ordering;
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::Instant;

/// Not a [`super::GAMES`] member: Python short-circuits party ahead of every
/// other check in `handle`, and its state is module-level, like party.py's
/// `_parties` -- not a [`super::RoomValley`] field.
pub const GAME: &str = "party";

/// The playlist, in play order (party.py:13). Reaches the wire in every `view`
/// AND in the room welcome's `arena.party.order` (valley.py:72), which is the
/// key the page's whole party panel is gated on (ui/app/27-play.js:104).
pub const ORDER: [&str; 4] = ["kart", "plat", "fps", "golf"];

/// Points by place, 1st first (party.py:14). Place 9 and beyond score 1, as
/// `min(place, len(POINTS))` does.
pub const POINTS: [i64; 8] = [10, 8, 6, 5, 4, 3, 2, 1];

/// A party untouched for this long is forgotten (party.py:15). Two hours.
pub const TTL: f64 = 2.0 * 3600.0;

/// One room's party. Python's party dict (party.py:53), field for field.
struct Party {
    /// The starter's `member.public()`. Only this person may end it early.
    by: Value,
    /// How many games of [`ORDER`] are done. `idx >= ORDER.len()` is a finished
    /// party, which still answers `view` until the TTL takes it.
    idx: usize,
    /// user_id -> points. A [`Seq`] because Python's dict order is the final
    /// tie-break under `sorted()` (equal score AND equal handle), and because
    /// `names` beside it is read in the same order.
    scores: Seq<i64>,
    /// user_id -> the three-key profile the standings show. Insertion-ordered
    /// for the same reason.
    names: Seq<Value>,
    /// One `{"game", "got"}` per scored game, oldest first.
    rounds: Vec<Value>,
    /// When this party was last touched, for the [`TTL`] sweep.
    at: f64,
}

/// Python's module-level `_parties` (party.py:17): room_id -> party, for the
/// whole process. Its ORDER never reaches the wire (only `.get(room_id)` ever
/// reads it), so a `HashMap` is right here where [`Seq`] is right inside
/// [`Party`].
static PARTIES: LazyLock<Mutex<HashMap<String, Party>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Python's `_now()` (party.py:21), which is `time.monotonic()` called DIRECTLY
/// -- not `valley.now`, so the test clock the other eight engines are driven by
/// does not reach it in either port. Hence an [`Instant`] here rather than
/// [`super::Clock`]: the frozen [`op`] and [`on_done`] signatures carry no `t`,
/// matching party.py, which takes none either.
///
/// Divergence: the process start is this `LazyLock`'s first touch rather than
/// boot, so the absolute value differs from Python's. Only differences are ever
/// read (`now() - p.at > TTL`), so it is unobservable.
fn now() -> f64 {
    static START: LazyLock<Instant> = LazyLock::new(Instant::now);
    START.elapsed().as_secs_f64()
}

/// Python's `_get(room_id)` (party.py:24): the room's party, FORGETTING it first
/// if it has gone stale. Strictly greater than [`TTL`], as Python's `>`.
fn get<'a>(
    st: &'a mut HashMap<String, Party>, room_id: &str, t: f64,
) -> Option<&'a mut Party> {
    if st.get(room_id).is_some_and(|p| t - p.at > TTL) {
        st.remove(room_id);
        return None;
    }
    st.get_mut(room_id)
}

/// The standings handle a party is sorted by: `p["names"].get(uid,
/// {}).get("handle", "")` (party.py:36).
///
/// Divergence: Python would raise TypeError comparing a `None` handle with a
/// `str` mid-sort; a profile without a string `handle` sorts as "" here. Every
/// real profile comes from `Member::public()`, which always carries one.
fn handle_of(names: &Seq<Value>, uid: &str) -> String {
    names
        .get(uid)
        .and_then(|u| u.get("handle"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

/// Python's `view(room_id)` (party.py:32). Takes the map because Python's `view`
/// calls `_get` itself, so it sees the TTL sweep and the `start`/`stop` the
/// caller just made -- `stop` pops the party and then asks for this, which is
/// how the `stopped` message carries `on: false`.
///
/// The key order is the wire order: `on`, `order`, `idx`, `done`, `next`, `by`,
/// `rounds`, `standings` (visible only because serde_json's `preserve_order` is
/// on, Cargo.toml:16).
fn view(st: &mut HashMap<String, Party>, room_id: &str, t: f64) -> Value {
    let Some(p) = get(st, room_id, t) else {
        return json!({"on": false, "order": ORDER});
    };
    // `sorted(..., key=lambda kv: (-kv[1], handle))`: points DESCENDING, then
    // handle ascending, and Rust's `sort_by` is stable like Python's `sorted`,
    // so a dead-equal pair keeps `scores` insertion order.
    let mut board: Vec<(&str, i64)> = p.scores.iter().map(|(u, pts)| (u, *pts)).collect();
    board.sort_by(|a, b| match b.1.cmp(&a.1) {
        Ordering::Equal => handle_of(&p.names, a.0).cmp(&handle_of(&p.names, b.0)),
        other => other,
    });
    let standings: Vec<Value> = board
        .iter()
        .map(|(uid, pts)| {
            // `p["names"].get(uid, {"userId": uid})`.
            let user = p.names.get(uid).cloned().unwrap_or_else(|| json!({"userId": uid}));
            json!({"user": user, "points": pts})
        })
        .collect();
    json!({
        "on": true, "order": ORDER, "idx": p.idx,
        "done": p.idx >= ORDER.len(),
        "next": ORDER.get(p.idx).map(|g| json!(g)).unwrap_or(Value::Null),
        "by": p.by, "rounds": p.rounds,
        "standings": standings,
    })
}

/// Python's `out.err(member.ws, msg)`. See the divergence on [`op`]: the frozen
/// signature carries no conn id, so this answers the PERSON rather than the one
/// socket.
fn err(out: &mut Out, uid: &str, msg: &str) {
    out.user(uid, "error", json!({"error": msg}));
}

/// Python's `partymod.op(room_id, member, op, out)` (party.py:45).
///
/// DIVERGENCE, and the one thing in this file the integrator should fix rather
/// than live with: Python answers `view` and all three errors with
/// `out.to(member.ws, ..)`, ONE SOCKET. The frozen signature has no conn id
/// (valley.rs:1089 calls `party::op(room_id, member, op, &mut out)`), so those
/// four replies go to `out.user`, every socket THIS PERSON has in THIS ROOM.
/// Observable only with two tabs open, where the quiet tab also gets the answer
/// it did not ask for. Pass `conn` down from the short-circuit and the four
/// calls become `out.to(conn, ..)` / `out.err(conn, ..)` unchanged.
pub fn op(room_id: &str, member: &Member, op: &str, out: &mut Out) {
    let t = now();
    let uid = member.user_id.as_str();
    let mut st = PARTIES.lock().unwrap();
    // Python reads the party BEFORE the branch, so the TTL sweep runs even for
    // `view` and even for an op nobody recognises. Only `idx` and `by`'s user id
    // are read from it, and holding a borrow across the arms would lock out
    // `view`, so they are copied out here.
    let cur = get(&mut st, room_id, t).map(|p| {
        (p.idx, p.by.get("userId").and_then(Value::as_str).map(str::to_string))
    });
    match op {
        "view" => {
            let party = view(&mut st, room_id, t);
            out.user(uid, "state", json!({"party": party}));
        }
        "start" => {
            // A FINISHED party does not block a new one: the test is
            // `p and p["idx"] < len(ORDER)`, so `start` on a done playlist
            // silently replaces it.
            if cur.is_some_and(|(idx, _)| idx < ORDER.len()) {
                err(out, uid, "a party is already on in this room");
                return;
            }
            st.insert(
                room_id.to_string(),
                Party { by: member.public(), idx: 0, scores: Seq::new(), names: Seq::new(),
                        rounds: Vec::new(), at: t },
            );
            let party = view(&mut st, room_id, t);
            out.all("state", json!({"party": party, "started": true}));
        }
        "stop" => {
            // `if not p: return` -- no party, no error, no message at all.
            let Some((idx, by_uid)) = cur else { return };
            // Once the playlist is over anyone may clear it away; while it is
            // running, only whoever started it.
            if by_uid.as_deref() != Some(uid) && idx < ORDER.len() {
                err(out, uid, "only whoever started the party can end it early");
                return;
            }
            st.remove(room_id);
            // `view` after the pop, so this carries `on: false`.
            let party = view(&mut st, room_id, t);
            out.all("state", json!({"party": party, "stopped": true}));
        }
        _ => err(out, uid, "unknown party op"),
    }
}

/// What party needs out of one finished game's rows: who, where they placed and
/// whether they posted a time at all. Python reads the same three fields off
/// `results.rows_from_done`'s dicts.
struct Scored {
    user_id: String,
    place: i64,
    value: Option<i64>,
}

/// `resultsmod.rows_from_done(game, data)` (party.py:74), narrowed to the three
/// fields above.
///
/// `results.rs` ports only kart, plat and fps, and party's playlist ENDS with
/// golf -- so without [`golf_rows`] below, the fourth round would never score
/// and no party could ever reach its final standings.
fn scored_rows(game: &str, data: &Value) -> Vec<Scored> {
    let rows = crate::results::rows_from_done(game, data);
    if rows.is_empty() && game == "golf" {
        return golf_rows(data); // the shim; it retires itself, see below
    }
    rows.into_iter()
        .map(|r| Scored { user_id: r.user_id, place: r.place, value: r.value })
        .collect()
}

/// TEMPORARY, AND THE ONE THING IN THIS FILE THAT BELONGS SOMEWHERE ELSE: the
/// golf branch of `results.rows_from_done` (results.py:66-74), which
/// `backend/src/results.rs` has not ported (its header says "Port the branch
/// with the game"). It lives here because party.rs is the only file this agent
/// owns and because a party that cannot score its last game is a party that
/// never finishes -- `the_points_add_up_over_the_playlist`, the Python's own
/// test, needs it.
///
/// DELETE THIS, and the `game == "golf"` arm of [`scored_rows`], the moment
/// `results::GAMES` grows "golf": [`scored_rows`] tries `results.rs` first, so
/// the real branch wins automatically and this one goes dead.
///
/// Python, verbatim:
/// ```text
/// totals = data.get("totals") or {}
/// ranked = sorted((v, uid) for uid, v in totals.items()
///                 if isinstance(uid, str) and isinstance(v, (int, float)))
/// n = len(ranked)
/// place, prev = 0, None
/// for i, (v, uid) in enumerate(ranked):
///     if v != prev:
///         place, prev = i + 1, v
/// ```
/// Fewest strokes wins, and players on the same total SHARE a place (so 18, 20,
/// 20, 21 places 1, 2, 2, 4) -- which is what party's points then follow.
///
/// Divergence: Python's `isinstance(v, (int, float))` also accepts a BOOL,
/// because `True` is an `int`; `Value::as_f64` does not. No `done` event the
/// Arena builds puts a bool in `totals`.
fn golf_rows(data: &Value) -> Vec<Scored> {
    let mut ranked: Vec<(f64, &str)> = data
        .get("totals")
        .and_then(Value::as_object)
        .map(|t| t.iter().filter_map(|(u, v)| Some((v.as_f64()?, u.as_str()))).collect())
        .unwrap_or_default();
    // `sorted()` over (value, uid) tuples: by strokes, then by user id. JSON
    // carries no NaN, so the partial_cmp never falls back.
    ranked.sort_by(|a, b| {
        a.0.partial_cmp(&b.0).unwrap_or(Ordering::Equal).then_with(|| a.1.cmp(b.1))
    });
    let mut out = Vec::with_capacity(ranked.len());
    let (mut place, mut prev) = (0i64, None);
    for (i, (v, uid)) in ranked.iter().enumerate() {
        if prev != Some(*v) {
            place = i as i64 + 1;
            prev = Some(*v);
        }
        out.push(Scored { user_id: uid.to_string(), place, value: Some(*v as i64) });
    }
    out
}

/// Python's `{k: u.get(k) for k in ("userId", "handle", "displayName")}`
/// (party.py:77) for one member, with Python's `names.get(uid, {"userId": uid})`
/// fallback (party.py:85) folded in: EXACTLY those three keys in that order,
/// `null` for one the profile does not carry. `avatarUrl` and `cos` are
/// deliberately dropped -- the standings are a name list, not a roster.
fn name_of(members: &Value, uid: &str) -> Value {
    match members.get(uid) {
        Some(u) => json!({"userId": u.get("userId"), "handle": u.get("handle"),
                          "displayName": u.get("displayName")}),
        None => json!({"userId": uid}),
    }
}

/// Python's `partymod.on_done(room_id, game, data, who)` (party.py:67): a game
/// in this room finished, so score it if it is the party's CURRENT game and
/// move the playlist on. Returns the message the flush sends the room, or None.
///
/// `members` is Python's `who`: `{m.user_id: m.public() for m in room.members}`,
/// the WHOLE ROOM keyed by user id -- not any game's lobby.
///
/// Nothing happens unless this is the exact next game: a `done` for the wrong
/// game, for a game outside [`ORDER`], after the playlist is over, or one that
/// produces no rows, all return None and leave `idx` alone.
pub fn on_done(room_id: &str, g: &str, payload: &Value, members: &Value) -> Option<Value> {
    let t = now();
    let mut st = PARTIES.lock().unwrap();
    {
        let p = get(&mut st, room_id, t)?;
        if p.idx >= ORDER.len() || ORDER[p.idx] != g {
            return None;
        }
        let mut rows = scored_rows(g, payload);
        if rows.is_empty() {
            return None;
        }
        // `sorted(rows, key=lambda r: r["place"])`, stable, so two players
        // sharing a place keep the results' own order.
        rows.sort_by_key(|r| r.place);
        // `data.get("mode") != "coop"`: a co-op platformer run has no individual
        // time for ANYONE, so nobody is treated as having not finished.
        let coop = payload.get("mode").and_then(Value::as_str) == Some("coop");
        let mut got = Vec::with_capacity(rows.len());
        for r in &rows {
            // `POINTS[min(place, len(POINTS)) - 1]`. Divergence: Python would
            // index from the END for place <= 0 (place 0 scores POINTS[-1] = 1)
            // and raise IndexError below -8. Every `done` event the Arena builds
            // places from 1, and `rows_from_done` turns a 0 into the field size,
            // so this clamps rather than porting a crash.
            let mut pts = POINTS[(r.place.clamp(1, POINTS.len() as i64) - 1) as usize];
            if (g == "kart" || g == "plat") && r.value.is_none() && !coop {
                pts = 1; // did not finish: a point for turning up
            }
            *p.scores.entry_or(&r.user_id, || 0) += pts;
            // `setdefault`: the first round someone appears in names them, and a
            // later round never overwrites it.
            p.names.entry_or(&r.user_id, || name_of(members, &r.user_id));
            got.push(json!({"userId": r.user_id, "place": r.place, "points": pts}));
        }
        p.rounds.push(json!({"game": g, "got": got}));
        p.idx += 1;
        p.at = t;
    }
    // Hand-built, NOT through `Out::msg`, exactly as party.py:90 is: the flush
    // appends this payload itself. `pv` is hardcoded 1 there, which is also what
    // `protocol::version("party")` answers for a game outside its table.
    Some(json!({"type": "game", "g": GAME, "ev": "state", "pv": 1,
                "party": view(&mut st, room_id, t), "scored": g}))
}

#[cfg(test)]
mod tests {
    use super::super::testkit::*;
    use super::*;
    use serde_json::json;

    // [`PARTIES`] is process-global and `cargo test` runs these in parallel, so
    // every test below uses a room id of its own instead of Python's autouse
    // `party.reset()` fixture. NOTHING HERE CALLS `reset()`: it clears the whole
    // process, so one test tidying up after itself would wipe a neighbour's room
    // mid-run -- which is exactly how this suite first failed. `reset()` exists
    // because party.py:93 does, for a caller that owns the process.

    fn member(uid: &str) -> Member {
        Member { user_id: uid.into(), handle: uid.into(), display_name: uid.into(),
                 avatar_url: String::new(), cos: Value::Null }
    }

    /// test_party.py's `res(*uids, dnf=())`: places 1..n in order, and a DNF is
    /// a null `ms`.
    fn res(uids: &[&str], dnf: &[&str]) -> Value {
        Value::Array(
            uids.iter()
                .enumerate()
                .map(|(i, u)| {
                    let ms = if dnf.contains(u) { Value::Null } else { json!(1000 + i) };
                    json!({"user": {"userId": u}, "place": i + 1, "ms": ms})
                })
                .collect(),
        )
    }

    /// test_party.py's `who = {u: M(u).public() for u in "abc"}`.
    fn who(uids: &[&str]) -> Value {
        let mut m = serde_json::Map::new();
        for u in uids {
            m.insert((*u).into(), json!({"userId": u, "handle": u, "displayName": u}));
        }
        Value::Object(m)
    }

    fn sent(out: &Out) -> Vec<Value> {
        out.items.iter().map(|(_, p)| p.clone()).collect()
    }

    // ------------------------------------------------------- the constants --

    #[test]
    fn the_playlist_and_the_points_are_pythons() {
        // party.py:13-15, byte for byte.
        assert_eq!(ORDER, ["kart", "plat", "fps", "golf"]);
        assert_eq!(POINTS, [10, 8, 6, 5, 4, 3, 2, 1]);
        assert_eq!(TTL, 7200.0);
        assert_eq!(GAME, "party");
        // party is not a Valley game and has no lobby.
        assert_eq!(super::super::game_index(GAME), None);
        assert!(!super::super::ours(GAME));
    }

    // ------------------------------------------------------------- the view --

    #[test]
    fn a_room_with_no_party_sees_only_the_playlist() {
        let mut out = Out::new(GAME);
        op("view_off", &member("a"), "view", &mut out);
        let m = &sent(&out)[0];
        // Both keys, in Python's order, and nothing else.
        assert_eq!(m["party"].to_string(),
                   r#"{"on":false,"order":["kart","plat","fps","golf"]}"#);
        assert_eq!(m["ev"], "state");
        assert_eq!(m["pv"], 1);
    }

    #[test]
    fn a_fresh_party_is_on_at_the_first_game_with_nobody_scored() {
        let mut out = Out::new(GAME);
        op("view_fresh", &member("a"), "start", &mut out);
        let v = &sent(&out)[0]["party"];
        assert_eq!(v["on"], true);
        assert_eq!(v["idx"], 0);
        assert_eq!(v["done"], false);
        assert_eq!(v["next"], "kart");
        assert_eq!(v["by"]["userId"], "a");
        assert_eq!(v["rounds"], json!([]));
        assert_eq!(v["standings"], json!([]));
        // The wire order of the eight keys (party.py:37-42).
        let keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(keys, ["on", "order", "idx", "done", "next", "by", "rounds", "standings"]);
    }

    // --------------------------------------------------------------- the ops --

    #[test]
    fn starting_a_party_tells_the_room_and_naming_it_again_does_not() {
        let mut out = Out::new(GAME);
        op("start_twice", &member("a"), "start", &mut out);
        assert_eq!(sent(&out)[0]["started"], true);
        assert!(matches!(out.items[0].0, super::super::To::All));
        let mut out = Out::new(GAME);
        op("start_twice", &member("b"), "start", &mut out);
        let m = &sent(&out)[0];
        assert_eq!(m["ev"], "error");
        assert_eq!(m["error"], "a party is already on in this room");
    }

    #[test]
    fn only_whoever_started_it_can_end_it_early() {
        // test_party.py::test_only_the_starter_ends_it_early.
        let mut out = Out::new(GAME);
        op("stop_rights", &member("a"), "start", &mut out);
        let mut out = Out::new(GAME);
        op("stop_rights", &member("b"), "stop", &mut out);
        assert_eq!(sent(&out)[0]["error"], "only whoever started the party can end it early");
        let mut out = Out::new(GAME);
        op("stop_rights", &member("a"), "stop", &mut out);
        let m = &sent(&out)[0];
        assert_eq!(m["party"]["on"], false);
        assert_eq!(m["stopped"], true);
        assert!(matches!(out.items[0].0, super::super::To::All));
    }

    #[test]
    fn stopping_a_room_with_no_party_says_nothing_at_all() {
        // `if not p: return` -- not even an error.
        let mut out = Out::new(GAME);
        op("stop_quiet", &member("a"), "stop", &mut out);
        assert!(out.items.is_empty());
    }

    #[test]
    fn anyone_may_clear_away_a_finished_party() {
        // The `and p["idx"] < len(ORDER)` half of the starter test: once the
        // playlist is over the lock comes off.
        let mut out = Out::new(GAME);
        op("stop_done", &member("a"), "start", &mut out);
        PARTIES.lock().unwrap().get_mut("stop_done").unwrap().idx = ORDER.len();
        let mut out = Out::new(GAME);
        op("stop_done", &member("b"), "stop", &mut out);
        assert_eq!(sent(&out)[0]["stopped"], true);
    }

    #[test]
    fn a_finished_party_does_not_block_the_next_one() {
        let mut out = Out::new(GAME);
        op("start_again", &member("a"), "start", &mut out);
        PARTIES.lock().unwrap().get_mut("start_again").unwrap().idx = ORDER.len();
        let mut out = Out::new(GAME);
        op("start_again", &member("b"), "start", &mut out);
        let v = &sent(&out)[0]["party"];
        assert_eq!(v["idx"], 0);
        assert_eq!(v["by"]["userId"], "b");
    }

    #[test]
    fn an_op_nobody_recognises_is_an_error_unlike_every_other_game() {
        // party.py:64 has the `else` the eleven Valley arms do not.
        let mut out = Out::new(GAME);
        op("unknown_op", &member("a"), "dance", &mut out);
        assert_eq!(sent(&out)[0]["error"], "unknown party op");
    }

    #[test]
    fn a_stale_party_is_forgotten_on_the_next_touch() {
        let mut out = Out::new(GAME);
        op("stale", &member("a"), "start", &mut out);
        // Back-date it past the TTL, the one way to reach the sweep: party.py
        // reads time.monotonic() directly, so no test clock drives it.
        PARTIES.lock().unwrap().get_mut("stale").unwrap().at = now() - TTL - 1.0;
        let mut out = Out::new(GAME);
        op("stale", &member("a"), "view", &mut out);
        assert_eq!(sent(&out)[0]["party"]["on"], false);
        assert!(!PARTIES.lock().unwrap().contains_key("stale"));
    }

    // ------------------------------------------------------------- on_done --

    #[test]
    fn the_points_add_up_over_the_playlist() {
        // test_party.py::test_points_add_up_over_the_playlist, round for round.
        let w = who(&["a", "b", "c"]);
        let mut out = Out::new(GAME);
        op("playlist", &member("a"), "start", &mut out);
        assert_eq!(sent(&out)[0]["party"]["next"], "kart");
        // Not the current game: nothing scored, nothing moved.
        assert!(on_done("playlist", "plat",
                        &json!({"results": res(&["a", "b"], &[])}), &w).is_none());
        let m = on_done("playlist", "kart",
                        &json!({"track": "oval",
                                "results": res(&["a", "b", "c"], &["c"])}), &w)
            .unwrap();
        assert_eq!(m["party"]["next"], "plat");
        assert_eq!(m["scored"], "kart");
        on_done("playlist", "plat",
                &json!({"level": "1", "mode": "race", "results": res(&["b", "a"], &[])}), &w);
        on_done("playlist", "fps",
                &json!({"results": [{"user": {"userId": "c"}, "place": 1, "kills": 5},
                                    {"user": {"userId": "a"}, "place": 2, "kills": 1}]}), &w);
        let m = on_done("playlist", "golf",
                        &json!({"totals": {"a": 20, "b": 18}, "course": "x"}), &w).unwrap();
        let v = &m["party"];
        assert_eq!(v["done"], true);
        assert_eq!(v["next"], Value::Null);
        // a: 10+8+8+8, b: 8+10+0+10, c: 1 (dnf kart) + 10
        let pts: Vec<(&str, i64)> = v["standings"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| (s["user"]["userId"].as_str().unwrap(), s["points"].as_i64().unwrap()))
            .collect();
        assert_eq!(pts, [("a", 34), ("b", 28), ("c", 11)]);
        assert_eq!(v["standings"][0]["user"]["displayName"], "a");
        // The playlist is over: a fifth game scores nothing.
        assert!(on_done("playlist", "kart", &json!({"results": res(&["a"], &[])}), &w).is_none());
    }

    #[test]
    fn the_message_the_flush_appends_is_hand_built_in_pythons_key_order() {
        let w = who(&["a"]);
        let mut out = Out::new(GAME);
        op("envelope", &member("a"), "start", &mut out);
        let m = on_done("envelope", "kart", &json!({"results": res(&["a"], &[])}), &w).unwrap();
        let keys: Vec<&str> = m.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(keys, ["type", "g", "ev", "pv", "party", "scored"]);
        assert_eq!(m["type"], "game");
        assert_eq!(m["g"], "party");
        assert_eq!(m["ev"], "state");
        assert_eq!(m["pv"], 1);
        assert_eq!(m["pv"].as_i64(), Some(crate::protocol::version(GAME)));
    }

    #[test]
    fn a_game_with_no_rows_scores_nothing_and_moves_nothing() {
        let mut out = Out::new(GAME);
        op("no_rows", &member("a"), "start", &mut out);
        // A kart done with no usable results: `if not rows: return None`.
        assert!(on_done("no_rows", "kart", &json!({"results": []}), &json!({})).is_none());
        assert!(on_done("no_rows", "kart", &json!({}), &json!({})).is_none());
        assert_eq!(PARTIES.lock().unwrap().get("no_rows").unwrap().idx, 0);
        // A game outside the playlist never scores, even at the right moment.
        assert!(on_done("no_rows", "type", &json!({"results": res(&["a"], &[])}),
                        &json!({})).is_none());
    }

    #[test]
    fn a_done_for_a_room_with_no_party_is_ignored() {
        assert!(on_done("no_party", "kart", &json!({"results": res(&["a"], &[])}),
                        &json!({})).is_none());
    }

    #[test]
    fn a_coop_platformer_run_places_everyone_properly_anyway() {
        // Nobody has an individual time in co-op, so the DNF rule must not fire:
        // `data.get("mode") != "coop"`.
        let w = who(&["a", "b"]);
        let mut out = Out::new(GAME);
        op("coop", &member("a"), "start", &mut out);
        PARTIES.lock().unwrap().get_mut("coop").unwrap().idx = 1; // on to plat
        let m = on_done("coop", "plat",
                        &json!({"level": "1", "mode": "coop",
                                "results": res(&["a", "b"], &[])}), &w).unwrap();
        let got = &m["party"]["rounds"][0]["got"];
        assert_eq!(got[0], json!({"userId": "a", "place": 1, "points": 10}));
        assert_eq!(got[1], json!({"userId": "b", "place": 2, "points": 8}));
    }

    #[test]
    fn a_dnf_outside_coop_is_worth_one_point_for_turning_up() {
        let w = who(&["a", "b"]);
        let mut out = Out::new(GAME);
        op("dnf", &member("a"), "start", &mut out);
        let m = on_done("dnf", "kart",
                        &json!({"results": res(&["a", "b"], &["b"])}), &w).unwrap();
        assert_eq!(m["party"]["rounds"][0]["got"][1],
                   json!({"userId": "b", "place": 2, "points": 1}));
        // fps has no time at all, so its places always score by place.
        PARTIES.lock().unwrap().get_mut("dnf").unwrap().idx = 2;
        let m = on_done("dnf", "fps",
                        &json!({"results": [{"user": {"userId": "b"}, "place": 9}]}), &w)
            .unwrap();
        // Place 9 falls off the end of POINTS: `min(place, len(POINTS))` -> 1.
        assert_eq!(m["party"]["rounds"][1]["got"][0]["points"], 1);
    }

    #[test]
    fn golf_scores_by_strokes_and_a_tie_shares_a_place() {
        // results.rs has no golf branch; golf_rows stands in for it. Fewest
        // strokes first, and 18/20/20/21 places 1, 2, 2, 4.
        let w = who(&["a", "b", "c", "d"]);
        let mut out = Out::new(GAME);
        op("golf_rank", &member("a"), "start", &mut out);
        PARTIES.lock().unwrap().get_mut("golf_rank").unwrap().idx = 3; // on to golf
        let m = on_done("golf_rank", "golf",
                        &json!({"course": "x",
                                "totals": {"a": 21, "b": 20, "c": 18, "d": 20}}), &w).unwrap();
        let got = m["party"]["rounds"][0]["got"].clone();
        assert_eq!(got, json!([{"userId": "c", "place": 1, "points": 10},
                               {"userId": "b", "place": 2, "points": 8},
                               {"userId": "d", "place": 2, "points": 8},
                               {"userId": "a", "place": 4, "points": 5}]));
    }

    #[test]
    fn equal_scores_break_on_the_handle_then_on_the_scoring_order() {
        // `sorted(key=lambda kv: (-kv[1], handle))`: "ann" before "bob" on a tie.
        let w = json!({"zed": {"userId": "zed", "handle": "ann", "displayName": "Zed"},
                       "abe": {"userId": "abe", "handle": "bob", "displayName": "Abe"}});
        let mut out = Out::new(GAME);
        op("tie_break", &member("a"), "start", &mut out);
        let m = on_done("tie_break", "kart",
                        &json!({"results": [{"user": {"userId": "zed"}, "place": 1, "ms": 1},
                                            {"user": {"userId": "abe"}, "place": 1, "ms": 2}]}),
                        &w).unwrap();
        let board = &m["party"]["standings"];
        assert_eq!(board[0]["user"]["userId"], "zed"); // handle "ann"
        assert_eq!(board[1]["user"]["userId"], "abe");
        assert_eq!(board[0]["points"], 10);
        assert_eq!(board[1]["points"], 10);
    }

    #[test]
    fn a_player_the_room_does_not_know_is_named_by_user_id_alone() {
        // `names.get(uid, {"userId": uid})`, and the three-key projection keeps
        // a null for a profile that carries no handle.
        let w = json!({"b": {"userId": "b"}});
        let mut out = Out::new(GAME);
        op("names", &member("a"), "start", &mut out);
        let m = on_done("names", "kart",
                        &json!({"results": res(&["a", "b"], &[])}), &w).unwrap();
        let board = &m["party"]["standings"];
        assert_eq!(board[0]["user"], json!({"userId": "a"}));
        assert_eq!(board[1]["user"],
                   json!({"userId": "b", "handle": null, "displayName": null}));
    }

    #[test]
    fn the_name_a_player_was_first_seen_under_sticks() {
        // `setdefault`: a later round never rewrites a profile.
        let mut out = Out::new(GAME);
        op("setdefault", &member("a"), "start", &mut out);
        on_done("setdefault", "kart", &json!({"results": res(&["a"], &[])}),
                &who(&["a"])).unwrap();
        let m = on_done("setdefault", "plat",
                        &json!({"mode": "race", "results": res(&["a"], &[])}),
                        &json!({"a": {"userId": "a", "handle": "renamed",
                                      "displayName": "Renamed"}})).unwrap();
        assert_eq!(m["party"]["standings"][0]["user"]["handle"], "a");
    }

    // ------------------------------------------------- through the dispatcher --

    #[tokio::test]
    async fn a_party_start_goes_to_the_whole_room() {
        // test_party.py::test_party_over_the_room_socket. party has no lobby, so
        // the second socket -- which has joined nothing -- must still see it.
        let e = env(4, 1);
        let (a, mut arx) = e.connect("hub_all", 1, "a").await;
        let (_b, mut brx) = e.connect("hub_all", 2, "b").await;
        e.send("hub_all", 1, &a, GAME, "start", json!({})).await;
        let m = until(&mut arx, GAME, "state").await;
        assert_eq!(m["party"]["on"], true);
        assert_eq!(m["started"], true);
        assert_eq!(m["party"]["order"], json!(["kart", "plat", "fps", "golf"]));
        let seen = until(&mut brx, GAME, "state").await;
        assert_eq!(seen["party"]["by"]["userId"], "a");
    }

    #[tokio::test]
    async fn a_view_answers_only_the_person_who_asked() {
        let e = env(4, 1);
        let (a, mut arx) = e.connect("hub_view", 1, "a").await;
        let (_b, mut brx) = e.connect("hub_view", 2, "b").await;
        e.send("hub_view", 1, &a, GAME, "view", json!({})).await;
        let m = until(&mut arx, GAME, "state").await;
        assert_eq!(m["party"]["on"], false);
        assert!(drain(&mut brx).is_empty(), "the view answer reached the wrong socket");
    }

    #[tokio::test]
    async fn a_party_frame_needs_no_lobby_and_no_hq_room() {
        // The short-circuit runs before the GAMES test, the room gate and the
        // "join the lobby first" gate, so a party op works from a bare socket.
        let e = env(4, 1);
        let (a, mut arx) = e.connect("hub_gate", 1, "a").await;
        e.send("hub_gate", 1, &a, GAME, "dance", json!({})).await;
        let m = until(&mut arx, GAME, "error").await;
        assert_eq!(m["error"], "unknown party op"); // not "unknown game"
    }

    #[tokio::test]
    async fn a_party_frame_without_a_string_op_is_an_unknown_game() {
        // `g == "party" and isinstance(op, str)`: a non-string op falls past the
        // short-circuit into the GAMES test, which party is not in.
        let e = env(4, 1);
        let (a, mut arx) = e.connect("hub_badop", 1, "a").await;
        e.send_raw("hub_badop", 1, &a, json!({"type": "game", "g": GAME, "op": 7})).await;
        let m = until(&mut arx, GAME, "error").await;
        assert_eq!(m["error"], "unknown game");
    }
}
