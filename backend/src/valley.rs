//! The shared Valley layer: the catalogs, the outbox, the per-game lobby, the
//! one room-state object, the dispatcher, the flush, the disconnect path and the
//! per-room tick loops. A port of the game-agnostic half of
//! `backend/app/valley.py` -- GAMES/PROTOCOL/GAME_NAMES (:59-80),
//! `season_of`/`_clip` (:249-255), `Out` (:258-284), `Lobby` (:287-298),
//! `RoomValley`/`_rooms`/`valley_for` (:301-325), `_today` (:896), `_rng`
//! (:993), `handle` (:997-1151), `_golf_recheck_later`/`golf_recheck`
//! (:1495-1525), `_leave_lobby` (:1527-1573), `on_disconnect` (:1575-1589),
//! `_send_quiet` (:1592-1597) and `_flush` (:1599-1633).
//!
//! WHY THIS IS ONE MODULE AND NOT NINE HUBS. The Python has one dispatcher and
//! one `RoomValley` per room holding all eleven lobbies. kart.rs, platformer.rs
//! and fps.rs each reimplemented that layer, and the three copies are the same
//! code three times (`flush` is byte-identical in all three, comment text
//! included). Nine more games would make twelve copies and twelve state maps.
//! More importantly, the only reason this port exists is wire parity, and the
//! only cheap way to audit wire parity is for a reviewer to put valley.py and
//! valley.rs side by side and see the same branches in the same order. So the
//! shapes here are Python's shapes, even where idiomatic Rust would differ:
//! [`Lobby::members`] is an insertion-ordered `Vec` because Python's dict order
//! reaches the wire; [`Out`] is a list of (audience, payload) pairs rather than
//! a typed event enum; [`RoomValley`] is one wide struct rather than per-game
//! maps; the dispatcher is one long `match` rather than a registry of handlers;
//! and Python's `if g == "hq"` special cases stay AT the dispatch site
//! ([`cap`], [`room_gate`]) instead of moving into the engine, so a reviewer can
//! still find them.
//!
//! WHAT IS NOT HERE YET, and who adds it. This file is phase 0 of the fan-out:
//! the shared layer alone, with nothing wired to it. Nine engines (pond, race,
//! duel, mines, farm, golf, hq, type and party) land next, each as its own
//! `src/valley/<game>.rs`, and each touches this file in exactly three places:
//! [`ValleyHub::joined_tail`] (the join snapshot), [`ValleyHub::engine_op`] (the
//! per-game ops) and [`engine_dropped`] (the leave branch). Those three
//! signatures are the frozen contract; everything else here is finished.
//! `RoomValley` grows one named field per engine, in Python's `__init__` order.
//! main.rs's wiring (an `AppState` field, the routing arms and the
//! `on_disconnect` call) is the integrator's commit, not this one.
//!
//! kart, plat and fps keep their own hubs and their own private copies of this
//! layer, including their own `Lobby`/`Out`/`To`, `MAX_LOBBY`, `HOST_GRACE`,
//! `as_num` and `cm`. That duplication is deliberate and temporary: those three
//! work, are covered by 2,536 lines of tests that pin event ORDER as well as
//! contents, and are the Arena's only live real-time games. They migrate one at
//! a time, each its own commit with `cargo test <game>::` green before and
//! after, once these nine have exercised this dispatcher. Deleting their copies
//! belongs to those commits. See the note on [`ValleyHub`].
//!
//! THE SHARED DIVERGENCES FROM PYTHON, LISTED ONCE SO NINE ENGINE FILES CAN
//! POINT AT THIS INSTEAD OF EACH INVENTING A WORDING:
//!   1. `now()` is read ONCE per message and passed down. Python calls `now()`
//!      up to six times inside the join arm alone and could in principle see
//!      different values; with a monotonic clock the difference is
//!      unobservable. kart.rs already made this choice.
//!   2. No Rust port can reproduce CPython's Mersenne Twister, so no seeded
//!      Python sequence is reproducible. Port the DISTRIBUTION, drop the
//!      seeded-oracle tests, assert the properties the Python test asserts (a
//!      5-letter word from the list, a fish whose rarity is in the table, n
//!      distinct holes in play order), and say so at the call site. See [`Dice`].
//!   3. [`SEND_TIMEOUT`] has no equivalent. Python caps one socket at 0.5 s for
//!      a lobby fan-out and gathers the rest concurrently under a shield;
//!      `rooms::send_where` is a non-blocking `try_send` on bounded queues, so a
//!      socket that cannot keep up drops the frame instead of being waited on.
//!   4. [`ValleyHub::flush`] uses `Recorder::record_later` (which spawns) where
//!      Python `await`s `resultsmod.record`. Pre-existing, documented at
//!      results.rs.
//!   5. Python appends party's message to `out.items` WHILE iterating that same
//!      list (valley.py:1610), so it goes out after everything already queued.
//!      A Rust by-value `for` cannot pick that up, so when party lands its
//!      message is collected during the loop and sent after it -- "last in this
//!      flush", not "right after the done".
//!   6. The three places Python awaits mid-dispatch are deferred to after the
//!      state guard drops. See [`After`].
//!   7. `on_disconnect`'s early return compares conn ids where Python compares
//!      WebSocket identity (valley.py:1577), and its room teardown stops this
//!      room's tick loops outright where Python lets each step notice the
//!      valley is gone and return False on its next tick.
//!   8. Python walks all eleven games in GAMES order in `on_disconnect`, putting
//!      kart/plat/fps 7th/8th/9th; here main.rs will call the three hubs first
//!      and this one after, so a socket watching several games sees the GAMES in
//!      a different inter-game order. Nothing within a game moves, because each
//!      flush addresses different sockets. It resolves itself when those three
//!      migrate.
//!
//! AUDIENCE IS THE EASIEST THING TO GET WRONG IN THESE NINE PORTS, and it fails
//! silently: no error, no panic, a passing engine test, and frames that simply
//! stop reaching people. kart, plat and fps -- the only Rust models an author
//! can copy from -- send almost everything to `To::Users(lobby_ids, None)`.
//! pond, race, duel, mines, farm and party send almost everything to [`To::All`]:
//! the WHOLE ROOM, including sockets that never joined that game's lobby, and
//! `test_duel_non_participants_cannot_act` depends on it. Each engine module's
//! header must state its own rule in one line, and each test file must carry one
//! `..._goes_to_the_whole_room` or `..._goes_only_to_the_lobby` test.

// Nothing calls this module yet. main.rs only declares it (`mod valley;`); the
// hub's wiring and the nine engine files land in later commits, and until then
// the whole file is unreachable from the binary. Rather than 40 targeted allows
// that all say the same thing, one module-wide allow with this comment, the way
// kart.rs:30 and realtime.rs:23 justify theirs. It comes off when main.rs
// constructs the hub; the engine agents never need to touch it.
// Deliberately NO module-wide `#![allow(dead_code)]`. It covered the nine
// engine files too (a lint attribute nests), which would have hidden an
// unreachable function in any of 12,000 new lines. Everything unused here is
// allowed item by item, with its reason.

// The nine engines. Each is one file and touches nothing outside it; the three
// hooks below, the party short-circuit, the farm tail and golf's two grace
// questions are the whole of their contact with this file.
pub mod bowling;
pub mod duel;
pub mod farm;
pub mod golf;
pub mod hq;
pub mod mines;
pub mod party;
pub mod pokebattle;
pub mod pond;
pub mod race;
pub mod td;
pub mod typerace;

use crate::realtime::{Clock, Registry, Ticker};
use crate::results::Recorder;
use crate::rooms::{Member, RoomManager};
use chrono::{DateTime, Datelike, NaiveDate};
use rand::rngs::StdRng;
use rand::SeedableRng;
use serde_json::{json, Map, Value};
use sqlx::SqlitePool;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

// ---------------------------------------------------------------- catalogs --

/// Every Valley game, in Python's `GAMES` order (valley.py:60) with the
/// Rust-first td/bowl appended (the frozen Python never learned them). The
/// order is load-bearing: [`ValleyHub::on_disconnect`] leaves the lobbies in
/// it, and `protocol::arena_info` emits the welcome's `games` keys in it
/// (visible only because serde_json's `preserve_order` is on, Cargo.toml:16).
pub const GAMES: [&str; 14] = [
    "pond", "race", "duel", "mines", "farm", "golf", "kart", "plat", "fps", "strike", "hq", "type",
    "td", "bowl",
];

/// Python's `GAME_NAMES` (valley.py:74), in [`GAMES`] order. The human name in
/// every `lobby` and `invite` event.
pub const GAME_NAMES: [&str; 14] = [
    "Fishing Pond", "Puzzle Race", "Creature Duel", "Co-op Mines", "Shared Farm", "Mini Golf",
    "Kart Racing", "Platformer Rush", "Blaster Arena", "Strikezone", "HQ", "Code Typing Race",
    "Tower Defense", "Bowling",
];

/// The eight games of [`GAMES`] this hub routes. kart, plat and fps keep their
/// own hubs for now, so main.rs matches them first and `handle` never sees them;
/// a frame for one of them reaching here is a routing bug, and answering
/// "unknown game" is better than opening a second, phantom kart lobby that
/// nothing ever writes to. That is the one dispatcher divergence from Python,
/// and it disappears when those three migrate (add them to this list, add the
/// three engine fields and the three arms, delete the three hubs).
///
/// `party` is NOT here and NOT in [`GAMES`]: Python short-circuits it ahead of
/// every other check (valley.py:1000), so it needs no lobby and no join. Until
/// `valley/party.rs` lands, a party frame falls through to "unknown game" --
/// which is what the Rust Arena answers today. See [`ValleyHub::handle`] step 2.
pub const OURS: [&str; 10] =
    ["pond", "race", "duel", "mines", "farm", "golf", "hq", "type", "td", "bowl"];

/// Index into [`RoomValley::lobbies`] for each game. Pinned against
/// [`GAMES`] by a test, so they cannot drift.
// The complete set, so [`GAMES`] order and the lobby array stay in step and a
// reader can find any game's slot. An engine that reaches its lobby through
// [`RoomValley::lobby_mut`] rather than by index never names its own constant.
#[allow(dead_code)]
pub const I_POND: usize = 0;
#[allow(dead_code)]
pub const I_RACE: usize = 1;
#[allow(dead_code)]
pub const I_DUEL: usize = 2;
#[allow(dead_code)]
pub const I_MINES: usize = 3;
#[allow(dead_code)]
pub const I_FARM: usize = 4;
#[allow(dead_code)]
pub const I_GOLF: usize = 5;
#[allow(dead_code)]
pub const I_KART: usize = 6;
#[allow(dead_code)]
pub const I_PLAT: usize = 7;
#[allow(dead_code)]
pub const I_FPS: usize = 8;
#[allow(dead_code)]
pub const I_STRIKE: usize = 9;
#[allow(dead_code)]
pub const I_HQ: usize = 10;
#[allow(dead_code)]
pub const I_TYPE: usize = 11;
pub const I_TD: usize = 12;
#[allow(dead_code)]
pub const I_BOWL: usize = 13;

/// [`GAMES`] position of `g`, or None for a game this Valley does not define --
/// Python's `g not in GAMES` test.
pub fn game_index(g: &str) -> Option<usize> {
    GAMES.iter().position(|n| *n == g)
}

/// Does this hub route `g`? See [`OURS`].
pub fn ours(g: &str) -> bool {
    OURS.contains(&g)
}

/// Python's `GAME_NAMES[g]`. Panics on a game outside [`GAMES`], which the
/// dispatcher has already rejected -- Python's `KeyError` in the same place.
pub fn game_name(g: &str) -> &'static str {
    GAME_NAMES[game_index(g).expect("game_name on a game outside GAMES")]
}

/// Seats in a game's lobby, unless the game overrides it. Python's `MAX_LOBBY`.
pub const MAX_LOBBY: usize = 8;

/// Seconds one socket may take to accept a lobby fan-out. Python's
/// `SEND_TIMEOUT`. Documentation only: see divergence 3 in the module doc.
// Nothing reads it: a bounded per-socket queue replaces the per-send timeout
// (the shared divergences note). Kept because the number is Python's and a
// reader comparing the two files will look for it.
#[allow(dead_code)]
pub const SEND_TIMEOUT: f64 = 0.5;

/// A host back from a socket drop this soon is host again. Python derives it
/// (`HOST_GRACE = golfmod.GRACE`, valley.py:79) from golf.py:347, which is
/// literally where the number comes from; when `valley/golf.rs` lands this
/// becomes `golf::GRACE` and golf owns it, rather than kart.rs:73,
/// platformer.rs:83 and fps.rs:119 each hardcoding 15.0 as they do today.
pub const HOST_GRACE: f64 = 15.0;

/// hqpresence.py:14-17: the HQ lobby holds 16, or 40 in Arena City. Here rather
/// than in `valley/hq.rs` only because that file does not exist yet; when it
/// does, it owns these three and [`cap`] calls `hq::cap(room_id)` -- Python's
/// `cap = hqmod.cap(room.room_id) if g == "hq" else MAX_LOBBY` (valley.py:1021).
pub const HQ_MAX_PEOPLE: usize = 16;
pub const HQ_CITY_ROOM: &str = "hq_city";
pub const HQ_MAX_CITY: usize = 40;

/// The lobby cap for one game in one room. Only hq overrides it. This is the
/// only per-game cap in the whole dispatcher, and the hook all three existing
/// Rust copies lack (they hardcode `MAX_LOBBY`).
pub fn cap(g: &str, room_id: &str) -> usize {
    if g == "hq" {
        if room_id == HQ_CITY_ROOM { HQ_MAX_CITY } else { HQ_MAX_PEOPLE }
    } else if g == td::GAME {
        td::MAX_PLAYERS
    } else if g == bowling::GAME {
        8
    } else if g == crate::strike::GAME {
        // 5v5 needs all ten seats in one lobby (bots don't take lobby slots)
        crate::strike::MAX_PLAYERS
    } else {
        MAX_LOBBY
    }
}

/// A game's room-id gate, checked BEFORE [`ValleyHub::valley_for`] creates any
/// state so a refused frame leaves no room behind. One arm today: hq outside an
/// `hq_` room (valley.py:1010). Note it is a PREFIX test, so "hq_city" passes;
/// which HQ you may enter is enforced earlier, in main.rs, with close code 4403.
pub fn room_gate(g: &str, room_id: &str) -> Option<&'static str> {
    (g == "hq" && !room_id.starts_with("hq_")).then_some("HQ presence lives in an HQ room")
}

// ------------------------------------------------------------------- seams --

/// Python's `wall = time.time` (valley.py:80): UTC unix seconds, patched by name
/// in tests. Only the farm reads it (its UTC day boundary and crop ripeness).
/// Separate from [`Clock`], which is `time.monotonic`, exactly as the Python
/// keeps two aliases. Kept here rather than in realtime.rs so that file, which
/// every hub shares, did not have to change.
pub type Wall = Arc<dyn Fn() -> f64 + Send + Sync>;

/// Python's `_rng()` (valley.py:993): a FRESH seeded generator per inbound
/// message, created unconditionally before the dispatch. A factory, not a
/// stream, so one message's draws cannot leak into the next and a test can hand
/// in `Arc::new(|| StdRng::seed_from_u64(1))` and get the same message twice.
/// Injected the way [`Clock`] already is, because Python's tests replace
/// `valley._rng` wholesale (test_valley.py:297) and a Rust `const` cannot be
/// patched.
///
/// See divergence 2 in the module doc: the distributions port, the seeded
/// oracles do not.
pub type Dice = Arc<dyn Fn() -> StdRng + Send + Sync>;

/// A [`Dice`] seeded from the OS, for the real server -- Python's
/// `random.Random(secrets.randbits(64))`.
pub fn entropy_dice() -> Dice {
    Arc::new(StdRng::from_entropy)
}

/// A [`Wall`] reading the real clock: UTC unix seconds, as `time.time` does.
pub fn system_wall() -> Wall {
    Arc::new(|| chrono::Utc::now().timestamp_millis() as f64 / 1000.0)
}

// ----------------------------------------------- python numbers on the wire --
// Every one of these exists because the naive Rust translation diverges on a
// value the page actually receives. Each is pinned by a test below, and they
// live here so no engine writes its own.

/// Python's one-argument `round()`: to an integer, HALF TO EVEN.
/// `py_round(2.5) == 2`, `py_round(37.5) == 38`, `py_round(38.5) == 38`. Rust's
/// `f64::round` rounds half away from zero and is wrong here. Pond's boss hp
/// really can land on .5 (two pullers decrement by 1.5); also race's join replay
/// `secs`, duel's `deadline_in`, hq.
pub fn py_round(x: f64) -> i64 {
    x.round_ties_even() as i64
}

/// Python's `round(x, n)`: correctly-rounded DECIMAL rounding of the exact
/// double, ties to even. Done through `format!` + `parse`, NOT
/// multiply-round-divide, which diverges in both directions:
/// `round(0.4425, 3)` is 0.443 but naive gives 0.442, and `round(0.1235, 3)` is
/// 0.123 but naive gives 0.124. Rust's float `Display` with a precision does the
/// same correct decimal rounding CPython's `float.__round__` does, which is why
/// this is a round trip through a string rather than arithmetic. Pond's `aim`
/// (3dp), farm's `hoursLeft` (1dp), typerace's `wpm`/`acc` (1dp) all reach the
/// wire through this.
pub fn py_round_to(x: f64, n: usize) -> f64 {
    if !x.is_finite() {
        return x; // Python's round() on a nan/inf returns it unchanged
    }
    format!("{:.*}", n, x).parse().unwrap_or(x)
}

/// Python's `int(x)` over a float: truncate TOWARD ZERO. `py_trunc(-0.9) == 0`,
/// not -1. hq's x/z/r, golf's `delayMs`/`readyInMs`, duel's `deadline_ms`,
/// typerace's `goInMs`.
pub fn py_trunc(x: f64) -> i64 {
    x as i64 // Rust's float -> int cast truncates toward zero, as Python's int()
}

/// Python's `int(v) % 360` for a validated float: truncate toward zero, then a
/// NON-NEGATIVE modulo. `deg360(-90.0) == 270`. Rust's `%` would give -90, so
/// this is a truncating cast then `rem_euclid`, neither a floor nor a round.
/// hq's heading (test_hq_presence pins -90 -> 270) and golf's `pos` relay.
pub fn deg360(x: f64) -> i64 {
    (x as i64).rem_euclid(360)
}

/// `math.floor(x + 0.5)` -- pokebattle's `half_up`, which exists precisely
/// because Python's `round` is banker's and JavaScript's `Math.round` is not.
/// Every rounding in the battle rules goes through it. It matches Rust's
/// `f64::round` for NON-NEGATIVE input only; do not substitute.
pub fn half_up(x: f64) -> i64 {
    (x + 0.5).floor() as i64
}

/// Python's `math.isqrt(n) if n > 0 else 0`: a FLOOR integer square root, 0 for
/// n <= 0. Golf's physics needs it; `(n as f64).sqrt() as i64` rounds
/// differently near perfect squares and the golden vectors diverge.
pub fn isqrt(n: i64) -> i64 {
    if n > 0 { n.isqrt() } else { 0 }
}

/// Python's floor division `a // b` on ints. Golf's `cell_of(v) = (v + HALF) //
/// TILE` needs it: Rust's `/` truncates, so `cell_of(-5001)` would come out 0
/// instead of -1 and the ball leaves the course on the -x/-z half of every hole.
///
/// `div_euclid` is floor division only for a POSITIVE divisor (it keeps the
/// remainder non-negative, so `7.div_euclid(-2)` is -3 where Python's `7 // -2`
/// is -4). Every caller in the port divides by TILE, which is positive; nothing
/// here should be used with a negative `b`.
pub fn floordiv(a: i64, b: i64) -> i64 {
    a.div_euclid(b)
}

/// kart.py:146's `as_num(v, lo, hi)`: reject a bool, require a finite JSON
/// number, then CLAMP into [lo, hi]. Identical to the three copies at
/// kart.rs:221, platformer.rs:252 and fps.rs:340.
///
/// NOTE it clamps, where hqpresence.py's look-alike `_num` REJECTS out of range
/// -- that one is [`num_in`]. Picking the wrong one is silent: an hq position of
/// x = 10^9 must be dropped whole (test_hq_presence pins it), not pinned to the
/// wall at 6000.
pub fn as_num(v: Option<&Value>, lo: f64, hi: f64) -> Option<f64> {
    let f = v?.as_number()?.as_f64()?;
    if !f.is_finite() {
        return None;
    }
    Some(f.max(lo).min(hi))
}

/// hqpresence.py:24's `_num(v, lo, hi)`: reject a bool, require a JSON number,
/// then an INCLUSIVE range check -- None for anything outside it. Contrast
/// [`as_num`], which clamps.
pub fn num_in(v: Option<&Value>, lo: f64, hi: f64) -> Option<f64> {
    let f = v?.as_number()?.as_f64()?;
    (lo <= f && f <= hi).then_some(f)
}

/// Golf's `as_int(v, lo, hi)` (golf.py:334): reject a bool, require a number,
/// reject NaN (`v != v`) and the infinities, truncate toward zero, THEN CLAMP
/// into range. Power 500 becomes 100; aim 99999 becomes 4096. It does not reject
/// an out-of-range number -- unlike [`int_exact`].
pub fn as_int_clamped(v: Option<&Value>, lo: i64, hi: i64) -> Option<i64> {
    let f = v?.as_number()?.as_f64()?;
    if !f.is_finite() {
        return None;
    }
    Some((f as i64).clamp(lo, hi))
}

/// typerace's `_int(v, lo, hi)` (typerace.py:32): reject a bool, require a
/// number, require `v == int(v)` (so 10.0 passes and 10.5 does not), then
/// range-check inclusively and REJECT out of range. Named apart from
/// [`as_int_clamped`] on purpose: Python has parsers that look alike and
/// respectively clamp and reject, and a single `as_int` would be a bug in eight
/// of nine engines.
///
/// Divergence: Python's `v != int(v)` RAISES OverflowError on an infinity, which
/// would propagate out of the handler; here an infinity is simply rejected.
pub fn int_exact(v: Option<&Value>, lo: i64, hi: i64) -> Option<i64> {
    let f = v?.as_number()?.as_f64()?;
    if !f.is_finite() || f.trunc() != f {
        return None;
    }
    let i = f as i64;
    (lo <= i && i <= hi).then_some(i)
}

/// JS `v|0` -- golf's `_i(v)` (golf.py:100): truncate an int or float toward
/// zero, 0 for ANYTHING else including a bool. Reads the course file's fields.
/// (Python's `int(nan)` raises where Rust's cast gives 0; no course file has
/// one, and a JSON document cannot carry a NaN in the first place.)
pub fn js_int(v: Option<&Value>) -> i64 {
    v.and_then(Value::as_number).and_then(|n| n.as_f64()).map(|f| f as i64).unwrap_or(0)
}

/// Python's `msg.get(k) is True`: an IDENTITY check. `1`, `1.0` and `"true"` do
/// NOT count. Pond's `perfect` and `chest`.
pub fn is_true(v: Option<&Value>) -> bool {
    matches!(v, Some(Value::Bool(true)))
}

/// Centimetres on the wire: `py_round(v * 100.0)`. Identical to the two copies
/// at platformer.rs:260 and fps.rs:348.
#[allow(dead_code)] // for kart/plat/fps, which still carry their own copies
pub fn cm(v: f64) -> i64 {
    py_round(v * 100.0)
}

/// Python's `_clip(v, n)` (valley.py:254): strip everything `str.isprintable()`
/// rejects (via `rooms::py_printable`), `.strip()`, THEN the first `n`
/// CHARACTERS; "" for anything that is not a string, including a bool or a
/// number. The strip comes before the cut, so " ab " clipped to 2 is "ab".
pub fn clip(v: Option<&Value>, n: usize) -> String {
    let Some(Value::String(s)) = v else { return String::new() };
    let kept: String = s.chars().filter(|c| crate::rooms::py_printable(*c)).collect();
    chars(kept.trim(), n)
}

/// The first `n` CHARACTERS of a `&str`, never bytes. Display names are
/// user-supplied and may be non-ASCII; hq's 24-character name cut and golf's
/// 40-character course key both need this.
pub fn chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// Python's `season_of(d)` (valley.py:249): winter for month 12/1/2, spring for
/// <= 5, summer for <= 8, else autumn. Takes a date, as Python does, so the farm
/// composes it with [`today`] the way `season_of(_today())` does.
pub fn season_of(d: NaiveDate) -> &'static str {
    match d.month() {
        12 | 1 | 2 => "winter",
        m if m <= 5 => "spring",
        m if m <= 8 => "summer",
        _ => "autumn",
    }
}

/// Python's `_today()` (valley.py:896): the UTC date of `wall` unix seconds.
/// The farm's day boundary is UTC midnight, not local. `.to_string()` gives the
/// "YYYY-MM-DD" Python's `.isoformat()` writes.
pub fn today(wall: f64) -> NaiveDate {
    let secs = wall.floor() as i64;
    DateTime::from_timestamp(secs, 0).unwrap_or_default().date_naive()
}

// -------------------------------------------------------- insertion order --

/// A Python dict whose insertion order reaches the wire, because the pages read
/// it: pond's `casts`/`scores`/`pulls` (the `helpers` list is pinned exactly),
/// race's `wins`/`guesses`, mines' `players` (it decides the Manhattan
/// tie-break) and `loot`, farm's `seeds` (a Python test does
/// `next(iter(seeds))`), golf's `chars`/`players`/`winners`, hq's `people`,
/// type's `players`, party's `names`. A `HashMap` randomises it and a `BTreeMap`
/// sorts it; both are wrong, and `BTreeMap` is wrong QUIETLY -- kart.rs uses one
/// elsewhere, so the wrong instinct is already in the file an author will copy
/// from. No engine should use a std map for anything it serialises.
///
/// The two insert rules are separate methods ON PURPOSE, because the engines
/// disagree and getting it backwards is silent.
#[derive(Debug, Clone)]
pub struct Seq<V> {
    items: Vec<(String, V)>,
}

impl<V> Default for Seq<V> {
    fn default() -> Self {
        Self { items: Vec::new() }
    }
}

impl<V> Seq<V> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn contains_key(&self, k: &str) -> bool {
        self.items.iter().any(|(n, _)| n == k)
    }

    pub fn get(&self, k: &str) -> Option<&V> {
        self.items.iter().find(|(n, _)| n == k).map(|(_, v)| v)
    }

    pub fn get_mut(&mut self, k: &str) -> Option<&mut V> {
        self.items.iter_mut().find(|(n, _)| n == k).map(|(_, v)| v)
    }

    /// Python's `d[k] = v`: insert, or overwrite IN PLACE keeping the original
    /// position. What [`Lobby::put`] and hq's `enter` want.
    pub fn set(&mut self, k: &str, v: V) {
        match self.items.iter_mut().find(|(n, _)| n == k) {
            Some(slot) => slot.1 = v,
            None => self.items.push((k.to_string(), v)),
        }
    }

    /// Python's `v = d.pop(k); d[k] = v`: insert, moving an existing key to the
    /// END of the order. What pond's recast does, and pond's
    /// `snapshot()["casting"]` order is the proof.
    pub fn bump(&mut self, k: &str, v: V) {
        self.remove(k);
        self.items.push((k.to_string(), v));
    }

    pub fn remove(&mut self, k: &str) -> Option<V> {
        let i = self.items.iter().position(|(n, _)| n == k)?;
        Some(self.items.remove(i).1)
    }

    /// Python's `d.setdefault(k, default)` reached by reference: the existing
    /// value, keeping its position, or a new one appended at the end.
    pub fn entry_or(&mut self, k: &str, default: impl FnOnce() -> V) -> &mut V {
        if let Some(i) = self.items.iter().position(|(n, _)| n == k) {
            return &mut self.items[i].1;
        }
        self.items.push((k.to_string(), default()));
        &mut self.items.last_mut().unwrap().1
    }

    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.items.iter().map(|(k, _)| k.as_str())
    }

    pub fn values(&self) -> impl Iterator<Item = &V> {
        self.items.iter().map(|(_, v)| v)
    }

    pub fn values_mut(&mut self) -> impl Iterator<Item = &mut V> {
        self.items.iter_mut().map(|(_, v)| v)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &V)> {
        self.items.iter().map(|(k, v)| (k.as_str(), v))
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = (&str, &mut V)> {
        self.items.iter_mut().map(|(k, v)| (k.as_str(), v))
    }

    pub fn retain(&mut self, mut f: impl FnMut(&str, &mut V) -> bool) {
        self.items.retain_mut(|(k, v)| f(k.as_str(), v));
    }

    pub fn clear(&mut self) {
        self.items.clear();
    }

    /// The JSON object `json.dumps(dict)` produces, keys in this order (visible
    /// only because serde_json's `preserve_order` is on, Cargo.toml:16).
    pub fn to_object(&self, f: impl Fn(&V) -> Value) -> Value {
        let mut m = Map::new();
        for (k, v) in &self.items {
            m.insert(k.clone(), f(v));
        }
        Value::Object(m)
    }
}

// -------------------------------------------------------------- Out and To --

/// Who a message goes to. The four Python `Out` kinds map onto three variants:
/// [`To::All`] is `out.all`; [`To::Conn`] is `out.to(ws, ..)`;
/// `Users(ids, None)` is BOTH `out.lobby(ids, ..)` and `out.user(uid, ..)`
/// (Python's "user" kind also only reaches that user's sockets in this room);
/// `Users(ids, Some(uid))` is `out.lobby(.., skip_user=uid)`.
///
/// One known consequence of collapsing "user" and "lobby" onto one variant:
/// [`ValleyHub::flush`]'s results interception fires for `Users`, so a
/// hypothetical `out.user(uid, "done", ..)` would be recorded here and not in
/// Python, which gates on `kind in ("lobby", "all")` (valley.py:1602). No engine
/// sends one; if one ever does, split the variant.
#[derive(Debug, Clone)]
pub enum To {
    /// Every socket in the room (Python `out.all`).
    All,
    /// One socket (Python `out.to`).
    Conn(u64),
    /// The sockets of these users, optionally not back to one user.
    Users(Vec<String>, Option<String>),
}

/// Messages to send once a rule has run. Python's `Out` (valley.py:259).
/// Nothing is sent until [`ValleyHub::flush`], which is why the deferred tail
/// can keep appending to it.
#[derive(Debug, Default)]
pub struct Out {
    pub g: String,
    pub items: Vec<(To, Value)>,
}

impl Out {
    pub fn new(g: &str) -> Self {
        Self { g: g.to_string(), items: Vec::new() }
    }

    /// Python's `Out(g if isinstance(g, str) else "?")` (valley.py:999): the
    /// unknown-game path echoes the client's own `g` back, or "?" when they did
    /// not send a string at all.
    pub fn unknown(g: Option<&str>) -> Self {
        Self::new(g.unwrap_or("?"))
    }

    /// Python's `Out._m`: `{"type", "g", "ev", "pv", **data}` IN THAT KEY ORDER.
    /// A data key that collides overwrites the VALUE and keeps the ORIGINAL
    /// POSITION, which serde_json reproduces only because `preserve_order` is on
    /// (Cargo.toml:16). `pv` is `protocol::version(self.g)`, which is 1 for an
    /// unknown g -- so an `Out::unknown(..)` error still carries `pv: 1`, and
    /// kart/tests.rs:655 pins exactly
    /// `{"type":"game","g":"golf","ev":"error","pv":1,"error":"unknown game"}`.
    ///
    /// Public (it is private at kart.rs:752) because party hand-builds the
    /// message it appends in `flush`, exactly as party.py:90 does.
    pub fn msg(&self, ev: &str, data: Value) -> Value {
        let mut m = Map::new();
        m.insert("type".into(), json!("game"));
        m.insert("g".into(), json!(self.g));
        m.insert("ev".into(), json!(ev));
        m.insert("pv".into(), json!(crate::protocol::version(&self.g)));
        if let Value::Object(d) = data {
            m.extend(d);
        }
        Value::Object(m)
    }

    pub fn push(&mut self, to: To, ev: &str, data: Value) {
        let m = self.msg(ev, data);
        self.items.push((to, m));
    }

    /// `out.all(ev, **data)` -- the WHOLE ROOM, including sockets that never
    /// joined this game's lobby. Pond, race, duel, mines, farm and party
    /// broadcast almost everything this way where kart, plat and fps use the
    /// lobby. See the audience note in the module doc.
    pub fn all(&mut self, ev: &str, data: Value) {
        self.push(To::All, ev, data);
    }

    /// `out.to(ws, ev, **data)` -- one socket.
    pub fn to(&mut self, conn: u64, ev: &str, data: Value) {
        self.push(To::Conn(conn), ev, data);
    }

    /// `out.user(uid, ev, **data)` -- every socket of one user IN THIS ROOM.
    pub fn user(&mut self, uid: &str, ev: &str, data: Value) {
        self.push(To::Users(vec![uid.to_string()], None), ev, data);
    }

    /// `out.lobby(ids, ev, **data)` -- only this game's lobby. Python stores a
    /// frozenset, so duplicate ids collapse; a Vec is equivalent because the
    /// fan-out tests membership per socket.
    pub fn lobby(&mut self, ids: Vec<String>, ev: &str, data: Value) {
        self.push(To::Users(ids, None), ev, data);
    }

    /// `out.lobby(ids, ev, skip_user=skip, **data)`. The skip arm has existed
    /// unused since kart.rs; golf's `pos` relay (valley.py:1441) is its first
    /// caller in either port.
    pub fn lobby_skip(&mut self, ids: Vec<String>, skip: &str, ev: &str, data: Value) {
        self.push(To::Users(ids, Some(skip.to_string())), ev, data);
    }

    /// `out.err(ws, msg)` -> `out.to(ws, "error", error=msg)`.
    pub fn err(&mut self, conn: u64, error: &str) {
        self.to(conn, "error", json!({"error": error}));
    }
}

// ------------------------------------------------------------------- lobby --

/// One game's lobby in one room. Python's `Lobby` (valley.py:288).
#[derive(Debug, Default)]
pub struct Lobby {
    /// user_id -> public profile, IN JOIN ORDER. The order reaches the wire
    /// through [`Lobby::roster`], decides host succession on leave (Python's
    /// `next(iter(lobby.members), None)`) and decides who is cut when a game
    /// seats only the first eight -- so this is a Vec, not a map.
    pub members: Vec<(String, Value)>,
    pub host: Option<String>,
    /// (user_id, when) of a host whose socket dropped: a quick rejoin takes the
    /// host back.
    pub prev_host: Option<(String, f64)>,
    /// user_id -> when their socket dropped: a quick rejoin is a reconnect, not
    /// a "joined" toast.
    pub blips: HashMap<String, f64>,
}

impl Lobby {
    pub fn has(&self, uid: &str) -> bool {
        self.members.iter().any(|(u, _)| u == uid)
    }

    pub fn ids(&self) -> Vec<String> {
        self.members.iter().map(|(u, _)| u.clone()).collect()
    }

    /// Python's `roster()`: each profile plus `host: bool`, in join order.
    pub fn roster(&self) -> Value {
        Value::Array(
            self.members
                .iter()
                .map(|(uid, p)| {
                    let mut p = p.clone();
                    if let Some(o) = p.as_object_mut() {
                        o.insert("host".into(), json!(self.host.as_deref() == Some(uid)));
                    }
                    p
                })
                .collect(),
        )
    }

    /// The public profile of one member, for duel's `challenged` event, which
    /// sends the target's STORED profile rather than a user id.
    pub fn profile(&self, uid: &str) -> Option<&Value> {
        self.members.iter().find(|(u, _)| u == uid).map(|(_, p)| p)
    }

    /// Python's `lobby.members[uid] = member.public()`: insert, or refresh an
    /// existing member's cosmetics IN PLACE, keeping their join position -- the
    /// reassignment the join arm runs even for someone already in the lobby
    /// (valley.py:1030). Contrast [`Seq::bump`], which moves to the end.
    pub fn put(&mut self, uid: &str, pubv: Value) {
        match self.members.iter_mut().find(|(u, _)| u == uid) {
            Some(slot) => slot.1 = pubv,
            None => self.members.push((uid.to_string(), pubv)),
        }
    }

    pub fn pop(&mut self, uid: &str) -> Option<Value> {
        let i = self.members.iter().position(|(u, _)| u == uid)?;
        Some(self.members.remove(i).1)
    }
}

// -------------------------------------------------------------- RoomValley --

/// Everything one room's Valley holds. Python's `RoomValley` (valley.py:301):
/// eleven lobbies built eagerly and one instance of every engine. Created lazily
/// per room by [`ValleyHub::valley_for`] and dropped when the room loses its last
/// socket -- except that an outstanding golf grace holds it (see
/// [`ValleyHub::recheck`]).
///
/// The engine fields land with the engines, one named field per game in Python's
/// `__init__` order (`pond`, `race`, `duel`, `mines`, `golf`, `hq`, `type_`).
/// Two games never get one: the FARM, which has no in-process state at all (it
/// lives in `room_farms.data`, and Python's `RoomValley` likewise has no farm
/// attribute), and PARTY, which is a process-global store keyed by room id
/// (party.py:19) and is not in [`GAMES`]. kart, plat and fps get theirs only
/// when they migrate -- adding them now would give every room a second,
/// never-written `Kart`, `Plat` and `Fps` beside the ones KartHub/PlatHub/FpsHub
/// already hold (kart.rs:788), which is dead state a reviewer cannot tell apart
/// from a routing bug.
///
/// The lobby slots DO exist for all eleven, so [`GAMES`] order, the index
/// constants and `on_disconnect`'s loop all hold, and [`leave_lobby`] returns
/// early on an empty one.
///
/// Engines reach their own lobby by index, so a dispatcher arm can take two
/// disjoint `&mut` borrows (`&mut v.lobbies[I_POND]`, `&mut v.pond`).
pub struct RoomValley {
    /// Python's `RoomValley.room_id`. Nothing reads it -- every entry point is
    /// handed the room id already -- but it is what the Python holds, and a
    /// future engine that needs its own room id will look here first.
    #[allow(dead_code)]
    pub room_id: String,
    pub lobbies: [Lobby; 14],
    // Python's `RoomValley.__init__` order (valley.py:305-314), minus kart,
    // plat and fps, which their own hubs still hold.
    pub pond: pond::Pond,
    pub race: race::Race,
    pub duel: duel::Duel,
    pub mines: mines::Mines,
    pub golf: golf::Golf,
    pub hq: hq::Hq,
    pub type_race: typerace::TypeRace,
    pub td: td::Td,
    pub bowl: bowling::Bowling,
}

impl RoomValley {
    pub fn new(room_id: &str) -> Self {
        Self {
            room_id: room_id.to_string(),
            lobbies: std::array::from_fn(|_| Lobby::default()),
            pond: pond::Pond::default(),
            race: race::Race::default(),
            duel: duel::Duel::default(),
            mines: mines::Mines::default(),
            golf: golf::Golf::default(),
            hq: hq::Hq::default(),
            type_race: typerace::TypeRace::default(),
            td: td::Td::default(),
            bowl: bowling::Bowling::default(),
        }
    }

    /// Python's `v.lobbies[g]`. Panics on a game outside [`GAMES`], as Python's
    /// dict lookup does; the dispatcher has already rejected one.
    #[allow(dead_code)] // the named accessor Python's `v.lobbies[g]` maps to
    pub fn lobby(&self, g: &str) -> &Lobby {
        &self.lobbies[game_index(g).expect("lobby for a game outside GAMES")]
    }

    #[allow(dead_code)] // ditto; engines index instead, for disjoint borrows
    pub fn lobby_mut(&mut self, g: &str) -> &mut Lobby {
        &mut self.lobbies[game_index(g).expect("lobby for a game outside GAMES")]
    }
}

// ------------------------------------------------------- engine call shapes --

/// What the shared layer hands every engine entry point. It carries exactly the
/// hub capabilities an engine may use from INSIDE the state lock -- which means
/// nothing that awaits.
///
/// `t` is hoisted once per message (divergence 1). There is no `wall` here:
/// the farm is the only game that wants one, and it runs OUTSIDE this lock
/// through [`After::Farm`], where it asks [`ValleyHub::wall_now`] itself.
pub struct Ctx<'a> {
    pub room_id: &'a str,
    pub conn: u64,
    pub member: &'a Member,
    /// `valley.now()`, monotonic seconds.
    pub t: f64,
    /// Python's per-message `_rng()`.
    pub rng: &'a mut StdRng,
    /// For [`ValleyHub::tick_on`] / [`ValleyHub::tick_off`] only -- both are
    /// synchronous, as `realtime::Registry::start` is, so they work from inside
    /// the critical section exactly as kart.rs already calls its own `tick_on`
    /// from inside `kart_op`.
    pub hub: &'a ValleyHub,
    /// Async follow-ups to run once the lock is dropped. See [`After`].
    pub after: &'a mut Vec<After>,
}

impl Ctx<'_> {
    pub fn uid(&self) -> &str {
        self.member.user_id.as_str()
    }

    pub fn me(&self) -> Value {
        self.member.public()
    }
}

/// What [`engine_dropped`] tells [`leave_lobby`]. Only duel ever returns
/// `Away`: Python suppresses the `left` field entirely while a duelist is on a
/// grace period (`left=None if away else who`, valley.py:1573).
pub enum Left {
    Notice,
    Away,
}

/// Work Python does with an `await` in the middle of `handle`, which a Rust
/// dispatch holding a `std::sync::Mutex` cannot do (a `MutexGuard` is not
/// `Send`, and even a `Send` guard held across a yield point would race the tick
/// tasks that lock the same map). The sync dispatch queues these;
/// [`ValleyHub::handle`] drains them IN ORDER after dropping the guard and
/// BEFORE the single `flush`, appending to the same [`Out`].
///
/// This generalises the `invite: Option<String>` that kart.rs:830 already sets
/// inside the lock and kart.rs:886 awaits outside it. A `Vec` rather than one
/// value, because a single slot would be silently wrong the first time a path
/// queues two.
///
/// IT IS ORDER-PRESERVING ON EVERY REACHABLE PATH, which is the only thing that
/// could break. Each one checked in valley.py: the invite arm's
/// `out.to(ws, "invited", ..)` is the last statement of its branch (:1088); the
/// `g == "farm"` arm is `await farm_op(..)` and nothing else (:1135); the join
/// arm's farm case is the whole tail, pushed after `out.all("lobby", ..)`
/// (:1047). So the invariant is "nothing in
/// `handle` pushes to `out` after an await". That is a property of today's
/// Python, not a promise it makes.
pub enum After {
    /// Python: `delivered = await manager.deliver_to_user(to, {..})` then
    /// `out.to(ws, "invited", to=to, delivered=delivered)` (valley.py:1085).
    /// The payload is hand-built, NOT through [`Out::msg`]. `delivered` is a
    /// SOCKET COUNT across every room, not a bool, so a user with two tabs
    /// gives 2.
    Invite { to: String },
    /// Python: `await farm_op(room_id, member, msg, out, rng)`. The farm holds
    /// no [`RoomValley`] state, so the entire op -- load, mutate, commit, view
    /// -- runs out here with its own database handle and never re-takes the
    /// lock.
    Farm { msg: Value },
}

/// Python's `loop.call_later(max(0.0, delay) + 0.05, ..)` (valley.py:1506): the
/// extra 50 ms so the recheck lands just after the grace, never on it.
pub const RECHECK_SLACK: f64 = 0.05;

/// What one tick of a ticked game produced, returned by a [`StepFn`].
pub struct Tick {
    /// False stops the loop (Python's `return False` from the step).
    pub keep: bool,
    /// (audience, ev, data) in send order.
    pub items: Vec<(To, String, Value)>,
    /// The lobby ids to charge the room's bandwidth budget against, or None to
    /// skip the charge. hq and type pass None, because Python's `_hq_tick_on`
    /// and `_type_tick_on` never call `tk.count(..)` -- their snapshots sit
    /// outside `ROOM_BYTES_PER_SEC` and are never thinned, unlike
    /// kart.rs:978-980.
    pub charge: Option<Vec<String>>,
}

/// One tick of a ticked game, run with the state lock held. `send` is false on a
/// tick the bandwidth budget thinned out: hq honours it, type ignores it
/// entirely. The generic loop in [`ValleyHub::step`] does the invariant part
/// (room-exists check, lock, liveness, build the [`Out`], unlock, optional
/// `tk.count`, flush, return `keep`); the varying part is this closure, which
/// lives in the engine's own file.
///
/// Only hq (8 Hz) and type (5 Hz) of the nine tick at all. Golf is turn-based
/// and must NEVER reach realtime.rs -- its only timer is
/// [`ValleyHub::arm_recheck`], a one-shot, not a `Ticker`. pond, race, duel, mines,
/// farm and party have no loop in the Python and must not be given one.
pub type StepFn = Arc<dyn Fn(&mut RoomValley, f64, bool) -> Tick + Send + Sync>;

/// Python's `"<g>:" + room_id` ticker key.
pub fn tick_key(g: &str, room_id: &str) -> String {
    format!("{g}:{room_id}")
}

// ----------------------------------------------------------------- the hub --

/// The Valley for every room: the lobbies, the engines, the one dispatcher, the
/// per-room tick loops and golf's grace timers. Python's module-level `_rooms`
/// plus `handle`.
///
/// WHY THIS IS A SECOND HUB BESIDE KartHub/PlatHub/FpsHub RATHER THAN THEIR
/// REPLACEMENT. Those three work and are covered by 2,536 lines of tests that
/// pin event ORDER as well as contents -- strong evidence of behaviour and weak
/// evidence of ordering, the worst combination for a mechanical dispatch swap,
/// because `until(rx, ev)` SKIPS frames until it finds the one it wants, so
/// reordering two pushes within a flush can leave every assertion green while
/// changing what the page receives. Folding them in also puts eleven games
/// behind one lock per room where there are three today, which changes
/// contention for the 15/20 Hz loops and deserves its own measurement. They
/// migrate one at a time, each its own commit, once these nine have exercised
/// this dispatcher.
#[derive(Clone)]
pub struct ValleyHub {
    inner: Arc<HubInner>,
}

struct HubInner {
    rooms: RoomManager,
    reg: Arc<Registry>,
    clock: Clock,
    wall: Wall,
    dice: Dice,
    /// Where a finished game is written down. Empty in the unit tests.
    results: Recorder,
    /// None in the unit tests that need no database; the farm then writes
    /// nothing rather than panicking.
    pool: Option<SqlitePool>,
    /// Python's module-level `_rooms` (valley.py:317).
    state: Mutex<HashMap<String, RoomValley>>,
}

impl ValleyHub {
    /// `wall` and `dice` are the two seams that did not exist in backend
    /// before: Python patches `valley.wall` and `valley._rng` by name in its
    /// tests and so must we.
    #[allow(clippy::too_many_arguments)] // one per seam the Python patches, plus the two stores
    pub fn new(
        rooms: RoomManager, reg: Arc<Registry>, clock: Clock, wall: Wall, dice: Dice,
        results: Recorder, pool: Option<SqlitePool>,
    ) -> Self {
        Self {
            inner: Arc::new(HubInner {
                rooms, reg, clock, wall, dice, results, pool,
                state: Mutex::new(HashMap::new()),
            }),
        }
    }

    /// `valley.now()`, monotonic seconds.
    pub fn now(&self) -> f64 {
        (self.inner.clock)()
    }

    /// `valley.wall()`, UTC unix seconds.
    pub fn wall_now(&self) -> f64 {
        (self.inner.wall)()
    }

    /// `_today()`: the UTC date of [`ValleyHub::wall_now`].
    pub fn today(&self) -> NaiveDate {
        today(self.wall_now())
    }

    /// A fresh per-message generator, Python's `_rng()`.
    pub fn rng(&self) -> StdRng {
        (self.inner.dice)()
    }


    #[cfg_attr(not(test), allow(dead_code))] // the tests assert on the tickers
    pub fn registry(&self) -> &Arc<Registry> {
        &self.inner.reg
    }

    pub fn pool(&self) -> Option<&SqlitePool> {
        self.inner.pool.as_ref()
    }

    /// Look at (or change) a room's Valley, if it has one. The tests' way in,
    /// the way the Python tests reach `v.pond.catches` and `v.pond.ops[a]`.
    pub fn with_room<R>(&self, room_id: &str, f: impl FnOnce(&mut RoomValley) -> R) -> Option<R> {
        self.inner.state.lock().unwrap().get_mut(room_id).map(f)
    }

    /// Python's `valley_for(room_id)` (valley.py:320): the room's Valley,
    /// created lazily. Only `handle` calls it; everything else uses
    /// [`ValleyHub::with_room`], so a frame refused before this point leaves no
    /// room state behind.
    fn valley_for<'a>(
        st: &'a mut HashMap<String, RoomValley>, room_id: &str,
    ) -> &'a mut RoomValley {
        st.entry(room_id.to_string()).or_insert_with(|| RoomValley::new(room_id))
    }

    /// A `{"type": "game", ...}` frame from one socket. Python's `handle`
    /// (valley.py:997), branch for branch and in the same order:
    ///
    ///   1. `g`, `op` off the wire; `out = Out(g if isinstance(g, str) else "?")`
    ///   2. PARTY SHORT-CIRCUIT, before every other check: `g == "party"` with a
    ///      string op goes straight to `party::op` and flushes. party is not in
    ///      [`GAMES`], has no lobby, requires no join and requires no room
    ///      prefix. NOT HERE YET: see [`OURS`]. The party agent adds the arm
    ///      between steps 1 and 3, and `protocol::arena_info` must not advertise
    ///      a "party" key before it does (that key switches on a whole UI panel,
    ///      ui/app/27-play.js:106).
    ///   3. `g` not routed here, or a non-string op -> err "unknown game". A
    ///      non-string op on a VALID g lands here too. The client's own `g` is
    ///      echoed back in the envelope.
    ///   4. [`room_gate`]: hq outside an `hq_` room -> err
    ///      "HQ presence lives in an HQ room", checked BEFORE `valley_for` so no
    ///      room state is created.
    ///   5. `valley_for`, the game's lobby by index, a fresh rng.
    ///   6. join / leave / invite -- the only ops you may send without a seat.
    ///   7. every other op: err "join the lobby first" unless seated, then the
    ///      per-game arm. An unrecognised op on a valid g produces NOTHING --
    ///      Python's if/elif chains have no `else`, so only an unknown GAME is
    ///      answered.
    ///   8. drain `after` in order, then ONE `flush`, on every path.
    ///
    /// Engines are reached only past step 7, so an engine can never be called
    /// for someone who has not joined. In Python that guarantee is the ordering
    /// of an if/elif chain, re-trusted by eleven arms; here it is one place, and
    /// no engine re-checks it.
    ///
    /// THE JOIN ARM'S SIX TRAPS, each with a test below:
    ///   - a reserved seat PAST the cap (duel only), so the lobby can
    ///     momentarily hold `MAX_LOBBY + 1`.
    ///   - `cap(g, room_id)`; over cap, not already in, not reserved -> err
    ///     "this game's lobby is full".
    ///   - `fresh = !lobby.has(uid)`; `returning` (duel's `back`, which pushes
    ///     its own `back` event BEFORE the shared `lobby` event).
    ///   - pop THIS user's blip, then OR in `t - blip < HOST_GRACE` (strictly
    ///     less, so a blip exactly at `HOST_GRACE` is not returning), and only
    ///     THEN sweep the other stale blips.
    ///   - `lobby.put(uid, member.public())`, which refreshes an existing
    ///     member's profile in place. Then host election, reading
    ///     `host not in members` AFTER the insert (so a host rejoining keeps
    ///     it), with `lobby.host is None` encoded as `unwrap_or(true)`.
    ///   - the prev_host clear is a SEPARATE condition
    ///     (`ph.0 == uid || t - ph.1 >= HOST_GRACE`) and must not be merged with
    ///     the host-back one: the old host returning too late still clears it,
    ///     and a different person joining after the grace also clears it.
    ///
    /// Then [`To::All`] `lobby` with `joined` = the profile or `Value::Null` --
    /// NEVER an omitted key -- then the per-game snapshot tail.
    pub async fn handle(&self, room_id: &str, conn: u64, member: &Member, msg: &Value) {
        // 1. and 3. Python reads both off the wire first and builds the Out from
        // the client's own `g`, so even "unknown game" comes back on their g.
        let gv = msg.get("g").and_then(Value::as_str);
        let opv = msg.get("op").and_then(Value::as_str);
        // 2. Python short-circuits party before every other check, including the
        // GAMES membership test (valley.py:1000): it is not a Valley game and has
        // no lobby, so none of the lobby gate applies to it.
        if let (Some(party::GAME), Some(op)) = (gv, opv) {
            let mut out = Out::new(party::GAME);
            party::op(room_id, member, op, &mut out);
            self.flush(room_id, out).await;
            return;
        }
        let (g, op) = match (gv, opv) {
            (Some(g), Some(op)) if ours(g) => (g, op),
            _ => {
                let mut out = Out::unknown(gv);
                out.err(conn, "unknown game");
                self.flush(room_id, out).await;
                return;
            }
        };
        // 4. Before valley_for, so a refused hq frame creates no room state.
        if let Some(why) = room_gate(g, room_id) {
            let mut out = Out::new(g);
            out.err(conn, why);
            self.flush(room_id, out).await;
            return;
        }
        let mut out = Out::new(g);
        let mut after: Vec<After> = Vec::new();
        {
            // 5. One clock read per message (divergence 1), one fresh rng, as
            // Python draws one whether the op uses it or not.
            let t = self.now();
            let mut rng = self.rng();
            let uid = member.user_id.as_str();
            let gi = game_index(g).expect("OURS is a subset of GAMES");
            let mut st = self.inner.state.lock().unwrap();
            let v = Self::valley_for(&mut st, room_id);
            match op {
                // 6.
                "join" => {
                    // Python: `reserved = g == "duel" and v.duel.seated(uid)` --
                    // a seat held past the cap for a duelist mid-match, so
                    // someone who dropped mid-duel is never shut out of their
                    // own match by eight spectators.
                    let reserved = g == duel::GAME && v.duel.seated(uid);
                    let cap = cap(g, room_id);
                    let lobby = &mut v.lobbies[gi];
                    if !lobby.has(uid) && lobby.members.len() >= cap && !reserved {
                        out.err(conn, "this game's lobby is full");
                    } else {
                        let fresh = !lobby.has(uid);
                        // Python: `returning = g == "duel" and v.duel.back(uid, out)`
                        // and THEN `returning = returning or (blip ..)`. `back`
                        // is not a predicate -- it cancels the grace hold and
                        // pushes the duel's own "back" frame -- so it must run
                        // on every duel join, before the blip is consulted, and
                        // its `out` comes first. Hence the split borrow here.
                        let back = if g == duel::GAME {
                            v.duel.back(uid, &mut out)
                        } else {
                            false
                        };
                        let lobby = &mut v.lobbies[gi];
                        let blip = lobby.blips.remove(uid);
                        let returning =
                            back || blip.map(|b| t - b < HOST_GRACE).unwrap_or(false);
                        lobby.blips.retain(|_, b| t - *b < HOST_GRACE);
                        lobby.put(uid, member.public());
                        let ph = lobby.prev_host.clone();
                        let host_gone =
                            lobby.host.as_deref().map(|h| !lobby.has(h)).unwrap_or(true);
                        let host_back = ph
                            .as_ref()
                            .map(|(h, at)| h == uid && t - at < HOST_GRACE)
                            .unwrap_or(false);
                        if host_gone || host_back {
                            lobby.host = Some(uid.to_string()); // first in, or back from a blip
                        }
                        if ph.map(|(h, at)| h == uid || t - at >= HOST_GRACE).unwrap_or(false) {
                            lobby.prev_host = None;
                        }
                        let joined =
                            if fresh && !returning { member.public() } else { Value::Null };
                        out.push(To::All, "lobby",
                                 json!({"members": lobby.roster(), "joined": joined,
                                        "name": game_name(g)}));
                        let mut cx = Ctx { room_id, conn, member, t, rng: &mut rng,
                                           hub: self, after: &mut after };
                        Self::joined_tail(v, &mut cx, g, &mut out);
                    }
                }
                "leave" => leave_lobby(v, g, uid, &mut out, false, t),
                "invite" => {
                    // Python: `not isinstance(to, str) or not to or to == uid`.
                    // A non-string `to` collapses to "" here, which the same
                    // branch rejects.
                    let to = msg.get("to").and_then(Value::as_str).unwrap_or("");
                    if !v.lobbies[gi].has(uid) {
                        out.err(conn, "join the lobby first");
                    } else if to.is_empty() || to == uid {
                        out.err(conn, "invite someone else");
                    } else {
                        after.push(After::Invite { to: to.to_string() });
                    }
                }
                // 7.
                _ if !v.lobbies[gi].has(uid) => out.err(conn, "join the lobby first"),
                _ => {
                    let mut cx = Ctx { room_id, conn, member, t, rng: &mut rng,
                                       hub: self, after: &mut after };
                    Self::engine_op(v, &mut cx, g, op, msg, &mut out);
                }
            }
        }
        // 8. The deferred tail, in order, then exactly one flush.
        self.run_after(room_id, conn, member, g, after, &mut out).await;
        self.flush(room_id, out).await;
    }

    /// Python's three mid-dispatch awaits, run with the state guard dropped and
    /// before the single flush. See [`After`] for why that is order-preserving.
    async fn run_after(
        &self, room_id: &str, conn: u64, member: &Member, g: &str, after: Vec<After>,
        out: &mut Out,
    ) {
        for a in after {
            match a {
                After::Invite { to } => {
                    // Hand-built, NOT through Out::msg: the frame leaves this
                    // room entirely (valley.py:1083).
                    let payload = json!({"type": "game", "g": g, "ev": "invite",
                                         "pv": crate::protocol::version(g),
                                         "from": member.public(), "room": room_id,
                                         "name": game_name(g)});
                    let delivered =
                        self.inner.rooms.deliver_to_user(&to, payload.to_string()).await;
                    // A SOCKET COUNT, not a bool: two tabs give 2.
                    out.to(conn, "invited", json!({"to": to, "delivered": delivered}));
                }
                // `await farm_op(room_id, member, msg, out, rng)`, with the guard
                // dropped so it can reach the database.
                After::Farm { msg } => {
                    farm::run(self, room_id, conn, member, &msg, out).await
                }
            }
        }
    }

    /// The per-game snapshot tail of Python's join arm (valley.py:1036-1075),
    /// pushed AFTER the shared `lobby` event. Each engine agent adds one arm:
    /// `pond::GAME => pond::joined(v, cx, out)`, and so on for the nine.
    fn joined_tail(v: &mut RoomValley, cx: &mut Ctx, g: &str, out: &mut Out) {
        match g {
            pond::GAME => pond::joined(v, cx, out),
            race::GAME => race::joined(v, cx, out),
            duel::GAME => duel::joined(v, cx, out),
            mines::GAME => mines::joined(v, cx, out),
            // The farm's join tail is `await farm_op(.., {"op": "view"}, ..)`
            // (valley.py:1047), so it defers like every other farm op.
            farm::GAME => cx.after.push(After::Farm { msg: json!({"op": "view"}) }),
            golf::GAME => golf::joined(v, cx, out),
            hq::GAME => hq::joined(v, cx, out),
            typerace::GAME => typerace::joined(v, cx, out),
            td::GAME => td::joined(v, cx, out),
            bowling::GAME => bowling::joined(v, cx, out),
            _ => {}
        }
    }

    /// The per-game op arms of Python's `handle` (valley.py:1092-1151), reached
    /// ONLY past the lobby gate -- so an engine can never be called for someone
    /// who has not joined. Each engine agent adds one arm:
    /// `pond::GAME => pond::op(v, cx, op, msg, out)`, and so on for the nine.
    /// An unrecognised op on a valid g must produce NOTHING, as Python's
    /// `else`-less if/elif chains do.
    fn engine_op(
        v: &mut RoomValley, cx: &mut Ctx, g: &str, op: &str, msg: &Value, out: &mut Out,
    ) {
        match g {
            pond::GAME => pond::op(v, cx, op, msg, out),
            race::GAME => race::op(v, cx, op, msg, out),
            duel::GAME => duel::op(v, cx, op, msg, out),
            mines::GAME => mines::op(v, cx, op, msg, out),
            // `await farm_op(room_id, member, msg, out, rng)` (valley.py:1135):
            // the whole arm is the await, so the whole arm defers.
            farm::GAME => cx.after.push(After::Farm { msg: msg.clone() }),
            golf::GAME => golf::op(v, cx, op, msg, out),
            hq::GAME => hq::op(v, cx, op, msg, out),
            typerace::GAME => typerace::op(v, cx, op, msg, out),
            td::GAME => td::op(v, cx, op, msg, out),
            bowling::GAME => bowling::op(v, cx, op, msg, out),
            _ => {}
        }
    }

    /// Golf's `grace_left(t)` (valley.py:1585): the longest outstanding grace
    /// hold, or None. Asked for BY NAME in `on_disconnect` and in
    /// [`ValleyHub::recheck`] because that is where Python asks -- not a hook
    /// every engine is offered and one answers. valley/golf.rs fills it in.
    fn golf_grace_left(v: &RoomValley, t: f64) -> Option<f64> {
        // HQ 2.5: bowling holds a dropped bowler's place on the same one-shot
        // timer, so the recheck lands at the sooner of the two holds.
        match (golf::grace_left(v, t), bowling::grace_left(v, t)) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// The body of Python's `golf_recheck` that runs under the lock
    /// (valley.py:1512-1515): re-run golf's advance into `out`, then report the
    /// grace still outstanding. valley/golf.rs fills it in.
    fn golf_recheck_step(v: &mut RoomValley, out: &mut Out, t: f64) -> Option<f64> {
        golf::recheck_step(v, out, t)
    }

    /// Python's `_golf_recheck_later` (valley.py:1495): one shot at
    /// `max(0.0, delay) + RECHECK_SLACK`, so the hole moves on even if nobody
    /// else does anything meanwhile.
    ///
    /// Divergence: Python keeps a strong ref in a module-level `_timers` set and
    /// discards it on completion; `tokio::spawn` already owns the task, so there
    /// is nothing to hold. A shutdown drops the runtime and the pending recheck
    /// with it, exactly as closing the event loop does.
    fn arm_recheck(&self, room_id: &str, delay: f64) {
        let hub = self.clone();
        let rid = room_id.to_string();
        let wait = delay.max(0.0) + RECHECK_SLACK;
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs_f64(wait)).await;
            hub.recheck(&rid).await;
        });
    }

    /// Golf's grace recheck. Python's `golf_recheck` (valley.py:1510): re-run
    /// golf's advance, re-arm while `grace_left` is Some, flush if the room still
    /// has members, and otherwise drop the [`RoomValley`]. Armed only from
    /// `on_disconnect` and from itself -- an explicit `leave` holds nothing and
    /// arms nothing.
    pub async fn recheck(&self, room_id: &str) {
        let t = self.now();
        let mut out = Out::new("golf");
        let mut bowl = Out::new(bowling::GAME);
        let left = {
            let mut st = self.inner.state.lock().unwrap();
            let Some(v) = st.get_mut(room_id) else { return }; // Python's `if v is None`
            let g = Self::golf_recheck_step(v, &mut out, t);
            let b = bowling::recheck_step(v, &mut bowl, t);
            match (g, b) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            }
        };
        out.items.extend(bowl.items);
        if let Some(d) = left {
            self.arm_recheck(room_id, d);
        }
        if self.inner.rooms.count_where(room_id, |_| true).await > 0 {
            self.flush(room_id, out).await;
        } else if left.is_none() {
            // Nobody came back to the room. Python guards the pop with
            // `_rooms.get(room_id) is v`; we re-take the lock instead, which
            // leaves the same narrow window and the same outcome.
            self.inner.state.lock().unwrap().remove(room_id);
            for g in GAMES {
                self.inner.reg.stop(&tick_key(g, room_id));
            }
        }
    }

    /// A socket went away. Python's `on_disconnect` (valley.py:1575): return
    /// early if the same person is still here on another socket, then leave
    /// EVERY lobby in [`GAMES`] order, FLUSHING ONCE PER GAME INSIDE THE LOOP --
    /// eleven separate flushes, not one at the end. Then, if golf still holds a
    /// grace, arm the recheck; otherwise, if the room is empty, forget its
    /// Valley and stop its loops.
    ///
    /// Note Python's teardown test is `all(ws is member.ws for ws in
    /// room.members)`, and `all()` over an empty dict is True, which
    /// `!other_conn(.., |_| true)` reproduces.
    ///
    /// Call before `RoomManager::leave`, beside the three existing calls at
    /// main.rs:592-594.
    pub async fn on_disconnect(&self, room_id: &str, conn: u64, member: &Member) {
        if self.with_room(room_id, |_| ()).is_none() {
            return; // Python's `if v is None: return` -- this room never played
        }
        let uid = member.user_id.clone();
        if self.inner.rooms.other_conn(room_id, conn, |m| m.user_id == uid).await {
            return; // the same person is still here on another socket
        }
        let t = self.now();
        for g in GAMES {
            let mut out = Out::new(g);
            self.with_room(room_id, |v| leave_lobby(v, g, &uid, &mut out, true, t));
            // Python skips the flush when the room is already gone; a flush to a
            // room that no longer exists sends nothing, so this is the same.
            self.flush(room_id, out).await;
        }
        let left = self.with_room(room_id, |v| Self::golf_grace_left(v, t)).flatten();
        if let Some(d) = left {
            self.arm_recheck(room_id, d); // the recheck also tidies an emptied room
        } else if !self.inner.rooms.other_conn(room_id, conn, |_| true).await {
            // Nobody else is in the room: forget its games and stop its loops.
            // Python only drops the state and lets each step notice the valley
            // is gone on its next tick; stopping them here is kart.rs's choice
            // (kart.rs:1020) and saves one tick of work per loop.
            self.inner.state.lock().unwrap().remove(room_id);
            for g in GAMES {
                self.inner.reg.stop(&tick_key(g, room_id));
            }
        }
    }

    /// Python's `_flush` (valley.py:1599). For each item, in order: if the
    /// payload is a `done` event of a game `results::is_done` knows AND the
    /// audience is [`To::All`] or [`To::Users`] (Python's "all" or "lobby" -- a
    /// `done` to a single socket is never recorded), record it with
    /// `Recorder::record_later`; then send it.
    ///
    /// Party's hook goes at the same line, right after the record: Python calls
    /// `partymod.on_done(room_id, g, payload, {m.user_id: m.public() for m in
    /// room.members})` -- the ROOM's members keyed by user_id, not any game's
    /// lobby -- and appends the returned message to `out.items` WHILE iterating
    /// it. A Rust by-value `for` cannot pick that up, so the party agent must
    /// collect the message here and send it after this loop: "last in this
    /// flush", not "right after the done" (divergence 5).
    ///
    /// [`To::All`] goes through `send_where(.., |_| true, ..)`, the per-socket
    /// direct queues, where Python's `out.all` uses the room broadcast;
    /// equivalent because main.rs joins every socket with `join_direct`.
    pub async fn flush(&self, room_id: &str, out: Out) {
        let rooms = &self.inner.rooms;
        for (to, payload) in out.items {
            // A finished game is written down from the event the server itself
            // sends the lobby or the room -- once per game, never from a client.
            if matches!(to, To::All | To::Users(..)) {
                if let Some(g) = crate::results::is_done(&payload) {
                    self.inner.results.record_later(g, &payload);
                    // Python appends party's message to `out.items` WHILE
                    // iterating that same list (valley.py:1612), so it goes out
                    // later in the same flush. This loop took `out.items` by
                    // value and cannot grow, so the message is sent right here
                    // instead -- which puts it BEFORE the `done` event rather
                    // than after it. DIVERGENCE, declared: the page reads the
                    // two independently (a party standings panel and a game's
                    // own end screen), and a party frame can only follow a
                    // `done` it was triggered by, so nothing orders them.
                    let who = rooms.members_public(room_id).await;
                    if let Some(pm) = party::on_done(room_id, g, &payload, &who) {
                        rooms.send_where(room_id, |_| true, pm.to_string()).await;
                    }
                }
            }
            let s = payload.to_string();
            match to {
                To::All => {
                    rooms.send_where(room_id, |_| true, s).await;
                }
                To::Conn(c) => {
                    rooms.send_conn(room_id, c, s).await;
                }
                To::Users(ids, skip) => {
                    rooms
                        .send_where(room_id, |m| {
                            ids.contains(&m.user_id)
                                && skip.as_deref() != Some(m.user_id.as_str())
                        }, s)
                        .await;
                }
            }
        }
    }

    /// Start (or keep) the room's loop for game `g` at `hz`. False means the
    /// process is at `realtime::MAX_TICKERS`. type turns that into "the Arena is
    /// busy right now: try again in a minute" AND rolls the race back with
    /// `end()` first; hq ignores it, which leaves a joiner with one `snap` and
    /// then silence -- faithful, and to be commented at that call site.
    ///
    /// Synchronous, like `realtime::Registry::start`, so it is callable from
    /// inside the state lock. The spawned loop's first poll wants the same
    /// mutex; on a multi-threaded runtime that can block one worker thread for
    /// the microseconds we still hold it. Not a deadlock -- we are not awaiting
    /// -- and kart has shipped this since it was written.
    pub fn tick_on(&self, g: &'static str, room_id: &str, hz: f64, step: StepFn) -> bool {
        let hub = self.clone();
        let rid = room_id.to_string();
        let clock = self.inner.clock.clone();
        let runner = move |t: f64, send: bool, tk: Arc<Ticker>| {
            let hub = hub.clone();
            let rid = rid.clone();
            let step = step.clone();
            async move { hub.step(g, &rid, t, send, &tk, &step).await }
        };
        self.inner.reg.start(&tick_key(g, room_id), hz, runner, clock).is_some()
    }

    /// Python's `realtime.stop("<g>:" + room_id)`.
    pub fn tick_off(&self, g: &str, room_id: &str) {
        self.inner.reg.stop(&tick_key(g, room_id));
    }

    /// The invariant half of every ticked game's step, shared by hq and type the
    /// way `_hq_tick_on` and `_type_tick_on` share their shape. Returns whether
    /// to keep the loop running.
    async fn step(
        &self, g: &str, room_id: &str, t: f64, send: bool, tk: &Ticker, step: &StepFn,
    ) -> bool {
        if !self.inner.rooms.exists(room_id).await {
            return false;
        }
        let (out, charge, keep) = {
            let mut st = self.inner.state.lock().unwrap();
            let Some(v) = st.get_mut(room_id) else { return false };
            let r = step(v, t, send);
            let mut out = Out::new(g);
            for (to, ev, data) in r.items {
                out.push(to, &ev, data);
            }
            (out, r.charge, r.keep)
        };
        // Python flushes only `if out.items` -- a quiet tick sends nothing.
        if !out.items.is_empty() {
            if let Some(ids) = charge {
                let n = self.inner.rooms.count_where(room_id, |m| ids.contains(&m.user_id)).await;
                let bytes: usize = out.items.iter().map(|(_, p)| p.to_string().len()).sum();
                tk.count(bytes * n.max(1));
            }
            self.flush(room_id, out).await;
        }
        keep
    }
}

/// The game-specific branch of [`leave_lobby`] (valley.py:1541-1570), called
/// after the member is popped and the host handed on, and BEFORE the shared
/// `lobby` event. Each engine agent adds one arm; returning [`Left::Away`]
/// suppresses that event's `left` field (duel only).
///
/// Python's `_leave_lobby` contains no `await`, so no engine needs to defer
/// anything from here and this takes no `Vec<After>` -- a correction to the
/// judged contract, which gave `dropped` one. golf's explicit-leave path calls
/// its own `advance` synchronously, inside this branch, exactly as
/// valley.py:1567 does.
fn engine_dropped(
    v: &mut RoomValley, g: &str, uid: &str, who: &Value, out: &mut Out, t: f64,
    disconnected: bool,
) -> Left {
    match g {
        pond::GAME => pond::dropped(v, uid, who, out, t, disconnected),
        race::GAME => race::dropped(v, uid, who, out, t, disconnected),
        duel::GAME => duel::dropped(v, uid, who, out, t, disconnected),
        mines::GAME => mines::dropped(v, uid, who, out, t, disconnected),
        // The farm keeps no per-room state, so nobody can leave it.
        golf::GAME => golf::dropped(v, uid, who, out, t, disconnected),
        hq::GAME => hq::dropped(v, uid, who, out, t, disconnected),
        typerace::GAME => typerace::dropped(v, uid, who, out, t, disconnected),
        td::GAME => td::dropped(v, uid, who, out, t, disconnected),
        bowling::GAME => bowling::dropped(v, uid, who, out, t, disconnected),
        _ => Left::Notice,
    }
}

/// Leave one game's lobby. Python's `_leave_lobby` (valley.py:1527). Returns
/// immediately if the user was not in it, so a stray `leave` emits NOTHING, not
/// even an error. `disconnected`: the socket dropped rather than an explicit
/// leave, so the host is handed back on a quick return and a game may hold a
/// seat.
///
/// Order is load-bearing: the game-specific event goes to that game's LOBBY
/// first, then `lobby` goes to the WHOLE ROOM, and the page depends on that.
/// `prev_host` is set only when there IS a successor
/// (`disconnected && lobby.host.is_some()`), so the last person out of a lobby
/// does not become it.
pub fn leave_lobby(
    v: &mut RoomValley, g: &str, uid: &str, out: &mut Out, disconnected: bool, t: f64,
) {
    let gi = game_index(g).expect("leave_lobby for a game outside GAMES");
    let lobby = &mut v.lobbies[gi];
    let Some(who) = lobby.pop(uid) else { return };
    if disconnected {
        lobby.blips.insert(uid.to_string(), t);
    }
    if lobby.host.as_deref() == Some(uid) {
        lobby.host = lobby.members.first().map(|(u, _)| u.clone());
        lobby.prev_host =
            if disconnected && lobby.host.is_some() { Some((uid.to_string(), t)) } else { None };
    }
    let left = engine_dropped(v, g, uid, &who, out, t, disconnected);
    // A duelist on a grace period has not left: the "away" event says so, no
    // "left" notice.
    let left_field = match left {
        Left::Away => Value::Null,
        Left::Notice => who,
    };
    out.push(To::All, "lobby",
             json!({"members": v.lobbies[gi].roster(), "left": left_field, "name": game_name(g)}));
}

// --------------------------------------------------------------- test kit --

/// The harness every engine's tests drive the hub through, written ONCE here so
/// nine test modules do not each reinvent kart/tests.rs:527-560's `Env` and nine
/// agents do not each add a near-copy to a shared file.
#[cfg(test)]
pub mod testkit {
    use super::*;
    use tokio::sync::mpsc;

    pub struct Env {
        pub hub: ValleyHub,
        pub rooms: RoomManager,
        pub clock: Arc<Mutex<f64>>,
        pub wall: Arc<Mutex<f64>>,
    }

    /// `now()` starts at 1000.0 and `wall()` at 1_790_000_000.0 -- the two values
    /// backend/tests/test_valley.py's `clock` fixture uses, so a ported test's
    /// arithmetic carries over unchanged. `seed` fixes the per-message RNG.
    pub fn env(max_tickers: usize, seed: u64) -> Env {
        env_with(max_tickers, seed, None)
    }

    /// With a migrated in-memory SQLite, for the farm.
    pub async fn env_db(max_tickers: usize, seed: u64) -> Env {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        crate::db::migrate(&pool).await.unwrap();
        env_with(max_tickers, seed, Some(pool))
    }

    fn env_with(max_tickers: usize, seed: u64, pool: Option<SqlitePool>) -> Env {
        let rooms = RoomManager::new();
        let clock = Arc::new(Mutex::new(1000.0));
        let wall = Arc::new(Mutex::new(1_790_000_000.0));
        let c = clock.clone();
        let w = wall.clone();
        let hub = ValleyHub::new(
            rooms.clone(),
            Registry::new(max_tickers),
            Arc::new(move || *c.lock().unwrap()),
            Arc::new(move || *w.lock().unwrap()),
            Arc::new(move || StdRng::seed_from_u64(seed)),
            Recorder::default(),
            pool,
        );
        Env { hub, rooms, clock, wall }
    }

    impl Env {
        pub fn advance(&self, dt: f64) {
            *self.clock.lock().unwrap() += dt;
        }

        pub fn advance_wall(&self, dt: f64) {
            *self.wall.lock().unwrap() += dt;
        }

        pub async fn connect(
            &self, room: &str, conn: u64, uid: &str,
        ) -> (Member, mpsc::Receiver<String>) {
            let m = Member { user_id: uid.into(), handle: uid.into(), display_name: uid.into(),
                             avatar_url: String::new(), cos: Value::Null };
            let (_rx, drx, _, _) = self.rooms.join_direct(room, conn, m.clone()).await.unwrap();
            (m, drx)
        }

        /// `{"type":"game","g":g,"op":op, ..extra}` through [`ValleyHub::handle`].
        pub async fn send(
            &self, room: &str, conn: u64, m: &Member, g: &str, op: &str, extra: Value,
        ) {
            let mut msg = json!({"type": "game", "g": g, "op": op});
            if let Value::Object(e) = extra {
                msg.as_object_mut().unwrap().extend(e);
            }
            self.hub.handle(room, conn, m, &msg).await;
        }

        /// A frame whose `g` or `op` is not a string, for the unknown-game path.
        pub async fn send_raw(&self, room: &str, conn: u64, m: &Member, msg: Value) {
            self.hub.handle(room, conn, m, &msg).await;
        }

        pub async fn disconnect(&self, room: &str, conn: u64, m: &Member) {
            self.hub.on_disconnect(room, conn, m).await;
            self.rooms.leave(room, conn).await;
        }
    }

    /// Read frames off one socket's queue until the first `ev` for `g` arrives.
    pub async fn until(rx: &mut mpsc::Receiver<String>, g: &str, ev: &str) -> Value {
        until_where(rx, g, ev, |_| true).await
    }

    pub async fn until_where(
        rx: &mut mpsc::Receiver<String>, g: &str, ev: &str, pick: impl Fn(&Value) -> bool,
    ) -> Value {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let m = tokio::time::timeout_at(deadline, rx.recv())
                .await
                .unwrap_or_else(|_| panic!("no {g}/{ev} event"))
                .expect("queue open");
            let v: Value = serde_json::from_str(&m).unwrap();
            if v["type"] == "game" && v["g"] == g && v["ev"] == ev && pick(&v) {
                return v;
            }
        }
    }

    /// Every frame already queued, in order, as (g, ev) -- for the tests that
    /// pin a SEQUENCE rather than one event (`["away","lobby"]`,
    /// `["hole","golf"]`, `["loot","mines"]`). [`until`] skips frames, so it
    /// cannot express those.
    pub fn drain(rx: &mut mpsc::Receiver<String>) -> Vec<(String, String)> {
        let mut seen = Vec::new();
        while let Ok(m) = rx.try_recv() {
            let v: Value = serde_json::from_str(&m).unwrap();
            seen.push((v["g"].as_str().unwrap_or("").to_string(),
                       v["ev"].as_str().unwrap_or("").to_string()));
        }
        seen
    }
}

#[cfg(test)]
mod tests {
    use super::testkit::*;
    use super::*;

    // --------------------------------------------------------- the catalogs --

    #[test]
    fn the_catalogs_are_pythons_in_pythons_order() {
        // valley.py:60 and :74, byte for byte and in order; td and bowl are
        // Rust-first (never ported back to the frozen Python) and sit at the end.
        assert_eq!(GAMES,
                   ["pond", "race", "duel", "mines", "farm", "golf", "kart", "plat", "fps",
                    "strike", "hq", "type", "td", "bowl"]);
        assert_eq!(game_name("strike"), "Strikezone");
        assert_eq!(game_name("td"), "Tower Defense");
        assert_eq!(game_name("bowl"), "Bowling");
        assert_eq!(cap("td", "r"), 4);
        assert_eq!(cap("bowl", "r"), 8);
        assert_eq!(game_name("pond"), "Fishing Pond");
        assert_eq!(game_name("race"), "Puzzle Race");
        assert_eq!(game_name("duel"), "Creature Duel");
        assert_eq!(game_name("mines"), "Co-op Mines");
        assert_eq!(game_name("farm"), "Shared Farm");
        assert_eq!(game_name("golf"), "Mini Golf");
        assert_eq!(game_name("kart"), "Kart Racing");
        assert_eq!(game_name("plat"), "Platformer Rush");
        assert_eq!(game_name("fps"), "Blaster Arena");
        assert_eq!(game_name("hq"), "HQ");
        assert_eq!(game_name("type"), "Code Typing Race");
        assert_eq!(MAX_LOBBY, 8);
        assert_eq!(SEND_TIMEOUT, 0.5);
        assert_eq!(HOST_GRACE, 15.0); // = golf.GRACE (golf.py:347)
    }

    #[test]
    fn the_lobby_indexes_match_the_games_table() {
        for (i, g) in [(I_POND, "pond"), (I_RACE, "race"), (I_DUEL, "duel"), (I_MINES, "mines"),
                       (I_FARM, "farm"), (I_GOLF, "golf"), (I_KART, "kart"), (I_PLAT, "plat"),
                       (I_FPS, "fps"), (I_STRIKE, "strike"), (I_HQ, "hq"), (I_TYPE, "type"),
                       (I_TD, "td"), (I_BOWL, "bowl")] {
            assert_eq!(GAMES[i], g);
            assert_eq!(game_index(g), Some(i));
        }
        assert_eq!(game_index("party"), None); // party is not in GAMES
        assert_eq!(game_index("nope"), None);
    }

    #[test]
    fn only_hq_overrides_the_lobby_cap() {
        // valley.py:1021 + hqpresence.py:31.
        assert_eq!(cap("pond", "lobby"), 8);
        assert_eq!(cap("hq", "hq_ann"), 16);
        assert_eq!(cap("hq", "hq_city"), 40);
        assert_eq!(cap("golf", "hq_city"), 8); // the room id only matters for hq
    }

    #[test]
    fn hq_presence_lives_in_an_hq_room() {
        assert_eq!(room_gate("hq", "lobby"), Some("HQ presence lives in an HQ room"));
        assert_eq!(room_gate("hq", "hq_ann"), None);
        assert_eq!(room_gate("hq", "hq_city"), None); // a prefix test, so the city passes
        assert_eq!(room_gate("pond", "lobby"), None);
    }

    // ------------------------------------------------ python numbers pinned --

    #[test]
    fn py_round_is_bankers_rounding() {
        assert_eq!(py_round(2.5), 2); // Rust's f64::round would say 3
        assert_eq!(py_round(37.5), 38);
        assert_eq!(py_round(38.5), 38);
        assert_eq!(py_round(-2.5), -2);
        assert_eq!(py_round(38.4), 38);
        assert_eq!(cm(1.005), 100); // 1.005 is under 1.005 as a double
        assert_eq!(cm(-12.5), -1250);
    }

    #[test]
    fn py_round_to_rounds_the_exact_double_not_the_product() {
        // The two cases a multiply-round-divide gets wrong, in both directions.
        assert_eq!(py_round_to(0.4425, 3), 0.443);
        assert_eq!(py_round_to(0.1235, 3), 0.123);
        assert_eq!(py_round_to(0.0005, 3), 0.001);
        assert_eq!(py_round_to(2.25, 1), 2.2); // ties to even
        assert_eq!(py_round_to(100.0, 1), 100.0);
    }

    #[test]
    fn py_trunc_and_deg360_truncate_toward_zero() {
        assert_eq!(py_trunc(-0.9), 0); // not -1
        assert_eq!(py_trunc(0.9), 0);
        assert_eq!(py_trunc(-1250.9), -1250);
        assert_eq!(deg360(-90.0), 270); // test_hq_presence pins exactly this
        assert_eq!(deg360(90.0), 90);
        assert_eq!(deg360(720.0), 0);
        assert_eq!(deg360(-0.5), 0);
    }

    #[test]
    fn half_up_is_not_bankers_rounding() {
        assert_eq!(half_up(0.5), 1); // where py_round says 0
        assert_eq!(half_up(1.5), 2);
        assert_eq!(half_up(2.5), 3);
        // pokebattle's evo_pos over a 2-long line: the parity suite's note is
        // "round(0.5) would say 0".
        let evo = |stage: i64, len: i64| half_up(stage as f64 / 4.0 * (len - 1) as f64).clamp(0, len - 1);
        assert_eq!((0..=4).map(|s| evo(s, 2)).collect::<Vec<_>>(), vec![0, 0, 1, 1, 1]);
    }

    #[test]
    fn isqrt_floors_and_floordiv_floors() {
        assert_eq!([0, 1, 3, 4, 99, 100, 1_000_000_000_001].map(isqrt),
                   [0, 1, 1, 2, 9, 10, 1_000_000]);
        assert_eq!(isqrt(-9), 0);
        // golf's cell_of(v) = (v + HALF) // TILE, with HALF = 5000, TILE = 10000.
        assert_eq!(floordiv(-5000 + 5000, 10000), 0);
        assert_eq!(floordiv(-5001 + 5000, 10000), -1); // a truncating / says 0
    }

    #[test]
    fn the_four_int_parsers_disagree_the_way_pythons_do() {
        let n = |x: f64| json!(x);
        let i = |x: i64| json!(x);
        // as_num clamps (kart.py:146) ...
        assert_eq!(as_num(Some(&n(1e9)), -6000.0, 6000.0), Some(6000.0));
        assert_eq!(as_num(Some(&json!(true)), 0.0, 1.0), None); // never a bool
        assert_eq!(as_num(Some(&json!("1")), 0.0, 1.0), None);
        assert_eq!(as_num(None, 0.0, 1.0), None);
        // ... where hq's _num rejects out of range.
        assert_eq!(num_in(Some(&n(1e9)), -6000.0, 6000.0), None);
        assert_eq!(num_in(Some(&n(-6000.0)), -6000.0, 6000.0), Some(-6000.0)); // inclusive
        assert_eq!(num_in(Some(&json!(true)), 0.0, 1.0), None);
        // golf's as_int clamps after truncating toward zero.
        assert_eq!(as_int_clamped(Some(&i(500)), 1, 100), Some(100));
        assert_eq!(as_int_clamped(Some(&i(99999)), -4096, 4096), Some(4096));
        assert_eq!(as_int_clamped(Some(&n(-0.9)), -10, 10), Some(0));
        assert_eq!(as_int_clamped(Some(&json!(true)), 0, 10), None);
        // typerace's _int rejects a fractional float and an out-of-range one.
        assert_eq!(int_exact(Some(&n(10.0)), 0, 20), Some(10));
        assert_eq!(int_exact(Some(&n(10.5)), 0, 20), None);
        assert_eq!(int_exact(Some(&i(21)), 0, 20), None);
        assert_eq!(int_exact(Some(&json!("20")), 0, 20), None);
        assert_eq!(int_exact(Some(&json!(true)), 0, 20), None);
        // golf's _i is JS `v|0`: 0 for anything that is not a number.
        assert_eq!(js_int(Some(&n(-2.7))), -2);
        assert_eq!(js_int(Some(&json!(true))), 0);
        assert_eq!(js_int(None), 0);
    }

    #[test]
    fn is_true_is_an_identity_check() {
        assert!(is_true(Some(&json!(true))));
        assert!(!is_true(Some(&json!(1))));
        assert!(!is_true(Some(&json!(1.0))));
        assert!(!is_true(Some(&json!("true"))));
        assert!(!is_true(None));
    }

    #[test]
    fn clip_strips_then_cuts_characters() {
        assert_eq!(clip(Some(&json!("  hello  ")), 4), "hell");
        assert_eq!(clip(Some(&json!("a\tb\nc")), 10), "abc"); // tab and newline are not printable
        assert_eq!(clip(Some(&json!(" \u{200b} ab ")), 8), "ab"); // zero-width space too
        assert_eq!(clip(Some(&json!(true)), 8), ""); // a non-str is ""
        assert_eq!(clip(Some(&json!(5)), 8), "");
        assert_eq!(clip(None, 8), "");
        assert_eq!(chars("héllo wörld", 5), "héllo"); // characters, never bytes
    }

    #[test]
    fn the_season_turns_on_pythons_months() {
        let d = |y, m, day| NaiveDate::from_ymd_opt(y, m, day).unwrap();
        assert_eq!(season_of(d(2026, 12, 1)), "winter");
        assert_eq!(season_of(d(2026, 1, 31)), "winter");
        assert_eq!(season_of(d(2026, 2, 28)), "winter");
        assert_eq!(season_of(d(2026, 3, 1)), "spring");
        assert_eq!(season_of(d(2026, 5, 31)), "spring");
        assert_eq!(season_of(d(2026, 6, 1)), "summer");
        assert_eq!(season_of(d(2026, 8, 31)), "summer");
        assert_eq!(season_of(d(2026, 9, 1)), "autumn");
        assert_eq!(season_of(d(2026, 11, 30)), "autumn");
    }

    #[test]
    fn today_is_the_utc_day_of_the_wall_clock() {
        // 1_790_000_000 = 2026-09-21T14:13:20Z, the test fixture's wall start.
        assert_eq!(today(1_790_000_000.0).to_string(), "2026-09-21");
        // The boundary is UTC midnight, not local: that day began 51_200 s before.
        assert_eq!(today(1_790_000_000.0 - 51_200.0).to_string(), "2026-09-21");
        assert_eq!(today(1_790_000_000.0 - 51_200.5).to_string(), "2026-09-20");
        assert_eq!(today(1_790_000_000.0 + 24.0 * 3600.0).to_string(), "2026-09-22");
    }

    // ------------------------------------------------------- insertion order --

    #[test]
    fn set_keeps_a_keys_position_and_bump_moves_it_to_the_end() {
        let mut s: Seq<i64> = Seq::new();
        s.set("a", 1);
        s.set("b", 2);
        s.set("c", 3);
        s.set("a", 9); // Python's d[k] = v: in place
        assert_eq!(s.keys().collect::<Vec<_>>(), vec!["a", "b", "c"]);
        assert_eq!(s.get("a"), Some(&9));
        s.bump("a", 10); // Python's pop then d[k] = v: to the end
        assert_eq!(s.keys().collect::<Vec<_>>(), vec!["b", "c", "a"]);
        assert_eq!(s.to_object(|v| json!(v)).to_string(), r#"{"b":2,"c":3,"a":10}"#);
        assert_eq!(s.remove("c"), Some(3));
        assert_eq!(s.len(), 2);
        s.retain(|k, _| k == "a");
        assert_eq!(s.keys().collect::<Vec<_>>(), vec!["a"]);
        *s.entry_or("new", || 7) += 1;
        assert_eq!(s.get("new"), Some(&8));
        s.clear();
        assert!(s.is_empty());
    }

    // ----------------------------------------------------------- the outbox --

    #[test]
    fn the_envelope_keys_come_before_the_data_keys() {
        let out = Out::new("pond");
        let m = out.msg("caught", json!({"points": 3, "goal": 1}));
        assert_eq!(m.to_string(),
                   r#"{"type":"game","g":"pond","ev":"caught","pv":1,"points":3,"goal":1}"#);
        // A colliding data key overwrites the VALUE and keeps the POSITION.
        let m = out.msg("x", json!({"g": "spoofed", "z": 1}));
        assert_eq!(m.to_string(), r#"{"type":"game","g":"spoofed","ev":"x","pv":1,"z":1}"#);
    }

    #[test]
    fn an_unknown_game_still_carries_pv_one() {
        let mut out = Out::unknown(Some("golf"));
        out.err(7, "unknown game");
        assert_eq!(out.items[0].1.to_string(),
                   r#"{"type":"game","g":"golf","ev":"error","pv":1,"error":"unknown game"}"#);
        assert_eq!(Out::unknown(None).g, "?"); // a non-string g becomes "?"
    }

    #[test]
    fn the_four_out_kinds_map_onto_three_audiences() {
        let mut out = Out::new("duel");
        out.all("turn", json!({}));
        out.to(3, "late", json!({}));
        out.user("u1", "challenge", json!({}));
        out.lobby(vec!["u1".into(), "u2".into()], "pos", json!({}));
        out.lobby_skip(vec!["u1".into(), "u2".into()], "u1", "pos", json!({}));
        assert!(matches!(out.items[0].0, To::All));
        assert!(matches!(out.items[1].0, To::Conn(3)));
        assert!(matches!(&out.items[2].0, To::Users(ids, None) if ids == &["u1".to_string()]));
        assert!(matches!(&out.items[3].0, To::Users(_, None)));
        assert!(matches!(&out.items[4].0, To::Users(_, Some(s)) if s == "u1"));
    }

    // ------------------------------------------------------------ the lobby --

    fn pubv(uid: &str) -> Value {
        json!({"userId": uid, "handle": uid, "displayName": uid, "avatarUrl": ""})
    }

    #[test]
    fn the_roster_is_each_profile_plus_host_in_join_order() {
        let mut l = Lobby::default();
        l.put("a", pubv("a"));
        l.put("b", pubv("b"));
        l.host = Some("b".into());
        let r = l.roster();
        assert_eq!(r[0]["userId"], "a");
        assert_eq!(r[0]["host"], false);
        assert_eq!(r[1]["userId"], "b");
        assert_eq!(r[1]["host"], true);
        assert_eq!(r.as_array().unwrap().len(), 2);
        assert_eq!(l.ids(), vec!["a".to_string(), "b".to_string()]);
        assert_eq!(l.profile("a").unwrap()["handle"], "a");
        assert!(l.profile("zz").is_none());
    }

    #[test]
    fn put_refreshes_a_member_in_place_and_keeps_their_join_position() {
        let mut l = Lobby::default();
        l.put("a", pubv("a"));
        l.put("b", pubv("b"));
        let mut fresh = pubv("a");
        fresh["displayName"] = json!("Ann");
        l.put("a", fresh);
        assert_eq!(l.ids(), vec!["a".to_string(), "b".to_string()]); // not moved to the back
        assert_eq!(l.roster()[0]["displayName"], "Ann");
        assert_eq!(l.pop("a").unwrap()["displayName"], "Ann");
        assert!(l.pop("a").is_none());
        assert!(!l.has("a"));
    }

    // ------------------------------------------------- through the dispatch --

    const ROOM: &str = "valley-room";

    #[tokio::test]
    async fn an_unknown_game_is_answered_with_the_clients_own_g() {
        let e = env(4, 1);
        let (a, mut wa) = e.connect(ROOM, 1, "a").await;
        e.send(ROOM, 1, &a, "quoits", "join", json!({})).await;
        let m = until(&mut wa, "quoits", "error").await;
        assert_eq!(m["error"], "unknown game");
        assert_eq!(m["pv"], 1);
        // kart, plat and fps have their own hubs: reaching this one is a routing
        // bug, and it is answered rather than given a phantom lobby.
        e.send(ROOM, 1, &a, "kart", "join", json!({})).await;
        assert_eq!(until(&mut wa, "kart", "error").await["error"], "unknown game");
        // Nothing above created a room valley.
        assert!(e.hub.with_room(ROOM, |_| ()).is_none());
    }

    #[tokio::test]
    async fn a_non_string_op_on_a_real_game_is_also_an_unknown_game() {
        let e = env(4, 1);
        let (a, mut wa) = e.connect(ROOM, 1, "a").await;
        e.send_raw(ROOM, 1, &a, json!({"type": "game", "g": "pond", "op": 7})).await;
        assert_eq!(until(&mut wa, "pond", "error").await["error"], "unknown game");
        // A non-string g echoes back as "?".
        e.send_raw(ROOM, 1, &a, json!({"type": "game", "g": 5, "op": "join"})).await;
        assert_eq!(until(&mut wa, "?", "error").await["error"], "unknown game");
    }

    #[tokio::test]
    async fn hq_presence_is_refused_outside_an_hq_room_and_leaves_no_room_behind() {
        let e = env(4, 1);
        let (a, mut wa) = e.connect("lobby", 1, "a").await;
        e.send("lobby", 1, &a, "hq", "join", json!({})).await;
        assert_eq!(until(&mut wa, "hq", "error").await["error"],
                   "HQ presence lives in an HQ room");
        assert!(e.hub.with_room("lobby", |_| ()).is_none());

        let (b, mut wb) = e.connect("hq_city", 2, "b").await;
        e.send("hq_city", 2, &b, "hq", "join", json!({})).await;
        assert_eq!(until(&mut wb, "hq", "lobby").await["name"], "HQ");
    }

    #[tokio::test]
    async fn join_leave_and_invite_are_the_only_ops_without_a_seat() {
        let e = env(4, 1);
        let (a, mut wa) = e.connect(ROOM, 1, "a").await;
        for op in ["cast", "start", "guess", "pull"] {
            e.send(ROOM, 1, &a, "pond", op, json!({})).await;
            assert_eq!(until(&mut wa, "pond", "error").await["error"], "join the lobby first");
        }
        // invite says the same thing before it looks at `to`.
        e.send(ROOM, 1, &a, "pond", "invite", json!({"to": "b"})).await;
        assert_eq!(until(&mut wa, "pond", "error").await["error"], "join the lobby first");
        // leave for someone who never joined says NOTHING at all, not an error.
        e.send(ROOM, 1, &a, "pond", "leave", json!({})).await;
        assert!(drain(&mut wa).is_empty());
    }

    #[tokio::test]
    async fn invite_someone_else_and_delivered_counts_sockets_not_people() {
        let e = env(4, 1);
        let (a, mut wa) = e.connect(ROOM, 1, "a").await;
        let (b, _wb) = e.connect(ROOM, 2, "b").await;
        let (_b2, _wb2) = e.connect(ROOM, 3, "b").await; // b's second tab
        e.send(ROOM, 1, &a, "race", "join", json!({})).await;
        until(&mut wa, "race", "lobby").await;
        for to in [json!("a"), json!(""), json!(7), Value::Null] {
            e.send(ROOM, 1, &a, "race", "invite", json!({"to": to})).await;
            assert_eq!(until(&mut wa, "race", "error").await["error"], "invite someone else");
        }
        e.send(ROOM, 1, &a, "race", "invite", json!({"to": b.user_id})).await;
        let m = until(&mut wa, "race", "invited").await;
        assert_eq!(m["to"], "b");
        assert_eq!(m["delivered"], 2); // two tabs, one person
    }

    #[tokio::test]
    async fn the_lobby_is_full_at_the_cap() {
        let e = env(4, 1);
        let mut socks = Vec::new();
        for i in 0..MAX_LOBBY {
            let (m, rx) = e.connect(ROOM, i as u64 + 1, &format!("u{i}")).await;
            e.send(ROOM, i as u64 + 1, &m, "mines", "join", json!({})).await;
            socks.push((m, rx));
        }
        let (z, mut wz) = e.connect(ROOM, 99, "z").await;
        e.send(ROOM, 99, &z, "mines", "join", json!({})).await;
        assert_eq!(until(&mut wz, "mines", "error").await["error"], "this game's lobby is full");
        // Someone already in is never refused, cap or no cap.
        let (m0, _) = &socks[0];
        e.send(ROOM, 1, m0, "mines", "join", json!({})).await;
        let n = e.hub.with_room(ROOM, |v| v.lobbies[I_MINES].members.len()).unwrap();
        assert_eq!(n, MAX_LOBBY);
    }

    #[tokio::test]
    async fn an_hq_lobby_holds_sixteen_and_arena_city_forty() {
        let e = env(4, 1);
        assert_eq!(cap("hq", "hq_ann"), 16);
        // Drive the real dispatcher for the 17th in an owner's HQ.
        for i in 0..HQ_MAX_PEOPLE {
            let (m, _rx) = e.connect("hq_ann", i as u64 + 1, &format!("u{i}")).await;
            e.send("hq_ann", i as u64 + 1, &m, "hq", "join", json!({})).await;
        }
        let (z, mut wz) = e.connect("hq_ann", 99, "z").await;
        e.send("hq_ann", 99, &z, "hq", "join", json!({})).await;
        assert_eq!(until(&mut wz, "hq", "error").await["error"], "this game's lobby is full");
        // The same person in Arena City is let in: a different cap at the same
        // dispatch site.
        let (z2, mut wz2) = e.connect(HQ_CITY_ROOM, 100, "z").await;
        e.send(HQ_CITY_ROOM, 100, &z2, "hq", "join", json!({})).await;
        assert_eq!(until(&mut wz2, "hq", "lobby").await["joined"]["userId"], "z");
    }

    #[tokio::test]
    async fn joined_and_left_are_present_and_null_rather_than_omitted() {
        let e = env(4, 1);
        let (a, mut wa) = e.connect(ROOM, 1, "a").await;
        e.send(ROOM, 1, &a, "golf", "join", json!({})).await;
        let m = until(&mut wa, "golf", "lobby").await;
        assert_eq!(m["joined"]["userId"], "a");
        assert_eq!(m["name"], "Mini Golf");
        assert!(m.as_object().unwrap().contains_key("joined"));
        // A second join from the same socket is not fresh, so joined is null --
        // the KEY is still there.
        e.send(ROOM, 1, &a, "golf", "join", json!({})).await;
        let m = until(&mut wa, "golf", "lobby").await;
        assert_eq!(m["joined"], Value::Null);
        assert!(m.as_object().unwrap().contains_key("joined"));
        e.send(ROOM, 1, &a, "golf", "leave", json!({})).await;
        let m = until(&mut wa, "golf", "lobby").await;
        assert_eq!(m["left"]["userId"], "a");
        assert_eq!(m["members"].as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn a_reconnect_keeps_its_place_in_the_roster_and_refreshes_its_cosmetics() {
        let e = env(4, 1);
        let (a, mut wa) = e.connect(ROOM, 1, "a").await;
        let (b, _wb) = e.connect(ROOM, 2, "b").await;
        e.send(ROOM, 1, &a, "race", "join", json!({})).await;
        e.send(ROOM, 2, &b, "race", "join", json!({})).await;
        let mut a2 = a.clone();
        a2.display_name = "Ann".into();
        a2.cos = json!({"frame": "#d8b34a"});
        e.send(ROOM, 1, &a2, "race", "join", json!({})).await;
        let m = until_where(&mut wa, "race", "lobby", |m| m["joined"] == Value::Null).await;
        assert_eq!(m["members"][0]["userId"], "a"); // still first
        assert_eq!(m["members"][0]["displayName"], "Ann");
        assert_eq!(m["members"][0]["cos"]["frame"], "#d8b34a");
        assert_eq!(m["members"][1]["userId"], "b");
    }

    // ------------------------------------------------- hosts, blips, graces --

    fn host_of(e: &Env, g: &str) -> Option<String> {
        e.hub.with_room(ROOM, |v| v.lobby(g).host.clone()).flatten()
    }

    fn prev_host_of(e: &Env, g: &str) -> Option<(String, f64)> {
        e.hub.with_room(ROOM, |v| v.lobby(g).prev_host.clone()).flatten()
    }

    #[tokio::test]
    async fn the_first_in_hosts_and_the_next_one_inherits_on_leave() {
        let e = env(4, 1);
        let (a, _wa) = e.connect(ROOM, 1, "a").await;
        let (b, _wb) = e.connect(ROOM, 2, "b").await;
        e.send(ROOM, 1, &a, "duel", "join", json!({})).await;
        assert_eq!(host_of(&e, "duel").as_deref(), Some("a"));
        e.send(ROOM, 2, &b, "duel", "join", json!({})).await;
        assert_eq!(host_of(&e, "duel").as_deref(), Some("a")); // a keeps it
        // An explicit leave hands the host to the next member in JOIN order and
        // sets no prev_host (the page is not coming back).
        e.send(ROOM, 1, &a, "duel", "leave", json!({})).await;
        assert_eq!(host_of(&e, "duel").as_deref(), Some("b"));
        assert!(prev_host_of(&e, "duel").is_none());
    }

    #[tokio::test]
    async fn the_host_comes_back_from_a_blip_inside_the_grace() {
        let e = env(4, 1);
        let (a, mut wa) = e.connect(ROOM, 1, "a").await;
        let (b, _wb) = e.connect(ROOM, 2, "b").await;
        e.send(ROOM, 1, &a, "pond", "join", json!({})).await;
        e.send(ROOM, 2, &b, "pond", "join", json!({})).await;
        e.disconnect(ROOM, 1, &a).await;
        assert_eq!(host_of(&e, "pond").as_deref(), Some("b"));
        assert_eq!(prev_host_of(&e, "pond").map(|(u, _)| u).as_deref(), Some("a"));
        e.advance(HOST_GRACE - 0.1);
        let (a2, mut wa2) = e.connect(ROOM, 3, "a").await;
        e.send(ROOM, 3, &a2, "pond", "join", json!({})).await;
        assert_eq!(host_of(&e, "pond").as_deref(), Some("a"));
        assert!(prev_host_of(&e, "pond").is_none()); // the old host returning clears it
        // Inside the grace it is a reconnect, so nobody is told anyone "joined".
        let m = until(&mut wa2, "pond", "lobby").await;
        assert_eq!(m["joined"], Value::Null);
        let _ = &mut wa;
    }

    #[tokio::test]
    async fn a_blip_exactly_at_the_host_grace_is_not_returning() {
        let e = env(4, 1);
        let (a, _wa) = e.connect(ROOM, 1, "a").await;
        let (b, _wb) = e.connect(ROOM, 2, "b").await;
        e.send(ROOM, 1, &a, "pond", "join", json!({})).await;
        e.send(ROOM, 2, &b, "pond", "join", json!({})).await;
        e.disconnect(ROOM, 1, &a).await;
        e.advance(HOST_GRACE); // the keep test is a STRICT `<`
        let (a2, mut wa2) = e.connect(ROOM, 3, "a").await;
        e.send(ROOM, 3, &a2, "pond", "join", json!({})).await;
        assert_eq!(host_of(&e, "pond").as_deref(), Some("b")); // too late to reclaim
        // ... but the old host returning still clears prev_host, which is a
        // SEPARATE condition from the host-back one.
        assert!(prev_host_of(&e, "pond").is_none());
        assert_eq!(until(&mut wa2, "pond", "lobby").await["joined"]["userId"], "a");
    }

    #[tokio::test]
    async fn a_different_joiner_after_the_grace_clears_prev_host() {
        let e = env(4, 1);
        let (a, _wa) = e.connect(ROOM, 1, "a").await;
        let (b, _wb) = e.connect(ROOM, 2, "b").await;
        e.send(ROOM, 1, &a, "mines", "join", json!({})).await;
        e.send(ROOM, 2, &b, "mines", "join", json!({})).await;
        e.disconnect(ROOM, 1, &a).await;
        assert!(prev_host_of(&e, "mines").is_some());
        // Inside the grace a stranger joining leaves it alone ...
        e.advance(1.0);
        let (c, _wc) = e.connect(ROOM, 4, "c").await;
        e.send(ROOM, 4, &c, "mines", "join", json!({})).await;
        assert!(prev_host_of(&e, "mines").is_some());
        assert_eq!(host_of(&e, "mines").as_deref(), Some("b"));
        // ... and past it, anyone's join clears it.
        e.advance(HOST_GRACE);
        let (d, _wd) = e.connect(ROOM, 5, "d").await;
        e.send(ROOM, 5, &d, "mines", "join", json!({})).await;
        assert!(prev_host_of(&e, "mines").is_none());
    }

    #[tokio::test]
    async fn the_last_person_out_of_a_lobby_does_not_become_prev_host() {
        let e = env(4, 1);
        let (a, _wa) = e.connect(ROOM, 1, "a").await;
        let (_keep, _wk) = e.connect(ROOM, 2, "keep").await; // holds the room open
        e.send(ROOM, 1, &a, "type", "join", json!({})).await;
        e.disconnect(ROOM, 1, &a).await;
        assert!(host_of(&e, "type").is_none());
        assert!(prev_host_of(&e, "type").is_none()); // no successor, so no prev_host
        // The blip is still recorded, so a quick return is a reconnect.
        let blip = e.hub.with_room(ROOM, |v| v.lobby("type").blips.get("a").copied()).flatten();
        assert_eq!(blip, Some(1000.0));
    }

    #[tokio::test]
    async fn a_stale_blip_is_swept_by_the_next_joiner() {
        let e = env(4, 1);
        let (a, _wa) = e.connect(ROOM, 1, "a").await;
        let (b, _wb) = e.connect(ROOM, 2, "b").await;
        e.send(ROOM, 1, &a, "race", "join", json!({})).await;
        e.send(ROOM, 2, &b, "race", "join", json!({})).await;
        e.disconnect(ROOM, 1, &a).await;
        assert_eq!(e.hub.with_room(ROOM, |v| v.lobby("race").blips.len()), Some(1));
        e.advance(HOST_GRACE + 1.0);
        // b rejoining pops its own blip (it has none) and then sweeps a's.
        e.send(ROOM, 2, &b, "race", "join", json!({})).await;
        assert_eq!(e.hub.with_room(ROOM, |v| v.lobby("race").blips.len()), Some(0));
    }

    // --------------------------------------------------- rooms and teardown --

    #[tokio::test]
    async fn a_second_tab_keeps_every_lobby_alive() {
        let e = env(4, 1);
        let (a, _wa) = e.connect(ROOM, 1, "a").await;
        let (a2, _wa2) = e.connect(ROOM, 2, "a").await; // the same person, second socket
        e.send(ROOM, 1, &a, "pond", "join", json!({})).await;
        e.disconnect(ROOM, 1, &a).await;
        // Python returns before leaving any lobby when another socket is theirs.
        assert!(e.hub.with_room(ROOM, |v| v.lobby("pond").has("a")).unwrap());
        assert_eq!(host_of(&e, "pond").as_deref(), Some("a"));
        e.disconnect(ROOM, 2, &a2).await;
        assert!(e.hub.with_room(ROOM, |_| ()).is_none()); // the last socket forgets the room
    }

    #[tokio::test]
    async fn an_emptied_room_forgets_its_games_and_a_disconnect_leaves_every_lobby() {
        let e = env(4, 1);
        let (a, mut wa) = e.connect(ROOM, 1, "a").await;
        let (b, mut wb) = e.connect(ROOM, 2, "b").await;
        for g in ["pond", "race", "golf"] {
            e.send(ROOM, 1, &a, g, "join", json!({})).await;
        }
        let _ = drain(&mut wa);
        let _ = drain(&mut wb);
        e.disconnect(ROOM, 1, &a).await;
        // One `lobby` per game, in GAMES order, each its own flush.
        let seen: Vec<_> = drain(&mut wb).into_iter().filter(|(_, ev)| ev == "lobby").collect();
        assert_eq!(seen,
                   vec![("pond".to_string(), "lobby".to_string()),
                        ("race".to_string(), "lobby".to_string()),
                        ("golf".to_string(), "lobby".to_string())]);
        assert!(e.hub.with_room(ROOM, |v| v.lobby("pond").members.is_empty()).unwrap());
        e.disconnect(ROOM, 2, &b).await;
        assert!(e.hub.with_room(ROOM, |_| ()).is_none());
    }

    #[tokio::test]
    async fn a_disconnect_in_a_room_that_never_played_does_nothing() {
        let e = env(4, 1);
        let (a, _wa) = e.connect(ROOM, 1, "a").await;
        e.disconnect(ROOM, 1, &a).await; // Python's `if v is None: return`
        assert!(e.hub.with_room(ROOM, |_| ()).is_none());
    }

    // ------------------------------------------------------------ the flush --

    #[tokio::test]
    async fn a_done_sent_to_one_socket_is_never_recorded() {
        // Python's `_flush` gates the record on `kind in ("lobby", "all")`; the
        // Recorder here has no pool, so this pins the GATE, not the write.
        let e = env(4, 1);
        let (_a, mut wa) = e.connect(ROOM, 1, "a").await;
        let mut out = Out::new("kart");
        out.to(1, "done", json!({"results": []}));
        out.all("done", json!({"results": []}));
        assert!(crate::results::is_done(&out.items[0].1).is_some());
        assert!(matches!(out.items[0].0, To::Conn(_)));
        assert!(matches!(out.items[1].0, To::All));
        e.hub.flush(ROOM, out).await;
        assert_eq!(drain(&mut wa).len(), 2); // both still SENT, only one recordable
    }

    #[tokio::test]
    async fn a_lobby_fanout_reaches_only_its_members_and_skips_one() {
        let e = env(4, 1);
        let (_a, mut wa) = e.connect(ROOM, 1, "a").await;
        let (_b, mut wb) = e.connect(ROOM, 2, "b").await;
        let (_c, mut wc) = e.connect(ROOM, 3, "c").await;
        let mut out = Out::new("golf");
        out.lobby(vec!["a".into(), "b".into()], "hole", json!({"hole": 1}));
        out.lobby_skip(vec!["a".into(), "b".into()], "a", "pos", json!({"u": "a"}));
        out.all("golf", json!({"round": Value::Null}));
        e.hub.flush(ROOM, out).await;
        assert_eq!(drain(&mut wa), vec![("golf".into(), "hole".into()), ("golf".into(), "golf".into())]);
        assert_eq!(drain(&mut wb),
                   vec![("golf".into(), "hole".into()), ("golf".into(), "pos".into()),
                        ("golf".into(), "golf".into())]);
        assert_eq!(drain(&mut wc), vec![("golf".into(), "golf".into())]); // the room, not the lobby
    }

    // ---------------------------------------------------------- the tickers --

    #[tokio::test]
    async fn a_tick_loop_sends_its_items_and_stops_when_the_step_says_so() {
        // Deliberately NOT hq: joining hq arms hq's own loop on the same key
        // (Python's `_hq_tick_on(room.room_id)`, valley.py:1070), and
        // `Registry::start` answers Some for a key already running -- so this
        // test's step would be silently dropped and never counted. type arms
        // its loop on `start`, not on `join`, so "type:r1" is free here.
        let e = env(4, 1);
        let (a, mut wa) = e.connect("r1", 1, "a").await;
        e.send("r1", 1, &a, "type", "join", json!({})).await;
        let n = Arc::new(Mutex::new(0usize));
        let seen = n.clone();
        let step: StepFn = Arc::new(move |_v, _t, _send| {
            let mut c = seen.lock().unwrap();
            *c += 1;
            Tick {
                keep: *c < 2,
                items: vec![(To::Users(vec!["a".into()], None), "snap".into(), json!({"ps": []}))],
                charge: None, // hq and type never call tk.count
            }
        });
        assert!(e.hub.tick_on("type", "r1", 50.0, step));
        until(&mut wa, "type", "snap").await;
        until(&mut wa, "type", "snap").await;
        // The second tick returned keep = false, so the loop unregistered itself.
        for _ in 0..50 {
            if e.hub.registry().get(&tick_key("type", "r1")).is_none() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert_eq!(*n.lock().unwrap(), 2);
        e.hub.tick_off("type", "r1");
    }

    #[tokio::test]
    async fn a_tick_loop_is_refused_when_the_process_is_at_its_budget() {
        let e = env(1, 1); // one slot in the whole registry
        let quiet: StepFn =
            Arc::new(|_v, _t, _send| Tick { keep: true, items: Vec::new(), charge: None });
        let (a, _wa) = e.connect("r1", 1, "a").await;
        e.send("r1", 1, &a, "type", "join", json!({})).await;
        let (b, _wb) = e.connect("r2", 2, "b").await;
        e.send("r2", 2, &b, "type", "join", json!({})).await;
        assert!(e.hub.tick_on("type", "r1", 5.0, quiet.clone()));
        assert!(!e.hub.tick_on("type", "r2", 5.0, quiet)); // the caller says "busy"
        e.hub.tick_off("type", "r1");
    }

    #[test]
    fn a_ticker_key_is_the_games_name_and_the_room() {
        assert_eq!(tick_key("hq", "hq_city"), "hq:hq_city");
        assert_eq!(tick_key("type", "lobby"), "type:lobby");
    }

    // ---------------------------------------------------------- the deferred --

    #[test]
    fn the_recheck_lands_just_after_the_grace_never_on_it() {
        assert_eq!(RECHECK_SLACK, 0.05); // valley.py:1506
    }

    #[tokio::test]
    async fn the_seams_read_the_values_the_python_fixture_uses() {
        let e = env(4, 1);
        assert_eq!(e.hub.now(), 1000.0);
        assert_eq!(e.hub.wall_now(), 1_790_000_000.0);
        assert_eq!(e.hub.today().to_string(), "2026-09-21");
        e.advance(2.5);
        e.advance_wall(24.0 * 3600.0);
        assert_eq!(e.hub.now(), 1002.5);
        assert_eq!(e.hub.today().to_string(), "2026-09-22");
        // A Dice is a FACTORY: the same message twice draws the same numbers.
        use rand::Rng;
        let first: u32 = e.hub.rng().gen();
        let again: u32 = e.hub.rng().gen();
        assert_eq!(first, again);
        assert!(e.hub.pool().is_none());
    }
}
