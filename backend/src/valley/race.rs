//! The puzzle word race: the server holds a five-letter word and marks each
//! guess Wordle-style, privately.
//!
//! Ported from backend/app/valley.py: `RACE_GUESSES`/`RACE_SECS` (:103),
//! `RACE_WORDS` (:104-240), `mark` (:480-492), `class Race` (:494-540), the
//! `race` arm of `handle` (:1106-1110) and the join tail (:1039-1040). The
//! shared divergences from Python are listed once at the top of
//! `backend/src/valley.rs`; this file adds none of its own.
//!
//! AUDIENCE. Everything the race says goes to [`To::All`], the WHOLE ROOM,
//! including sockets that never joined the race lobby -- `start`, `progress`,
//! `timeout` and `win`. The one exception is the `mark` itself, which goes to
//! the guesser's SOCKET alone, because the marks say which letters are in the
//! word and the room must not learn them (valley.py:533: the `progress` event
//! carries only a hit COUNT, "how close, never which letters").
//!
//! RACE HAS NO `_leave_lobby` BRANCH in the Python, so [`dropped`] does nothing
//! and a player who leaves keeps their guess count and their wins.

use super::{py_round, Ctx, Left, Out, RoomValley, Seq};
use rand::seq::SliceRandom;
use serde_json::{json, Value};

/// The game key on the wire.
pub const GAME: &str = "race";

/// Python's `RACE_GUESSES, RACE_SECS = 6, 180` (valley.py:103). Ints, because
/// `secs` reaches the wire as the integer 180.
pub const RACE_GUESSES: i64 = 6;
pub const RACE_SECS: i64 = 180;

/// Python's `RACE_WORDS` (valley.py:104): 134 five-letter words of the trade,
/// in the Python's order. The order is not on the wire -- only
/// `rng.choice(RACE_WORDS)` reads it -- but it is kept anyway so a reviewer can
/// put the two tables side by side, and a test pins the length, the ends and
/// the shape of every entry.
pub const RACE_WORDS: [&str; 134] = [
    "admin", "agent", "alias", "array", "async", "audit", "await", "batch", "bench", "blobs",
    "brand", "build", "bytes", "cache", "catch", "chain", "chmod", "class", "clone", "close",
    "cloud", "codec", "const", "count", "crash", "crate", "cycle", "dates", "debit", "debug",
    "defer", "delta", "draft", "drive", "embed", "entry", "epoch", "error", "event", "fetch",
    "field", "flags", "float", "flush", "frame", "graph", "group", "guard", "heaps", "hooks",
    "index", "infer", "input", "istio", "items", "joins", "kafka", "label", "latch", "layer",
    "limit", "lines", "linux", "local", "logic", "loops", "macro", "merge", "model", "mount",
    "mutex", "nginx", "nodes", "order", "owner", "paged", "parse", "patch", "pivot", "pixel",
    "popup", "ports", "print", "proxy", "qubit", "query", "queue", "ratio", "react", "redis",
    "regex", "rerun", "reset", "route", "rules", "scope", "serde", "shape", "shard", "shell",
    "sigma", "sleep", "slice", "spawn", "split", "stack", "state", "stdin", "store", "style",
    "swift", "table", "tasks", "tests", "timer", "token", "tools", "trace", "trees", "tuple",
    "types", "union", "unzip", "utils", "value", "vault", "views", "watch", "where", "while",
    "write", "xpath", "yield", "zones",
];

/// Python's `mark(guess, word)` (valley.py:480): the Wordle scorer, and the only
/// hard thing in this game. TWO PASSES over five slots, because repeated
/// letters are what a one-pass reimplementation gets wrong:
///
///   1. every exact match is a "hit" and its letter is STRUCK OUT of `left`;
///   2. only then, each remaining slot whose letter is still somewhere in
///      `left` is a "near", and THAT occurrence is struck out too.
///
/// So each letter of the word is credited at most once, hits first. The two
/// ways a rewrite breaks, both pinned by tests below:
///   - no striking out at all (a plain `guess[i] in word`) double-credits: for
///     word "state" and guess "tests" it answers `[near, near, near, hit, near]`
///     where Python answers `[near, near, near, hit, miss]`, because by the
///     fifth slot both of the word's s's are already spoken for.
///   - nears before hits steals the hit's letter: for word "float" and guess
///     "alias" it answers `[near, hit, miss, hit, miss]` where Python answers
///     `[miss, hit, miss, hit, miss]`, because the leading 'a' would take the
///     very 'a' that scores the hit at index 3.
///
/// Indexes 0..5 unconditionally, as the Python does. Both sides are exactly
/// five characters on every reachable path (the word comes from [`RACE_WORDS`]
/// and the guess is validated in [`Race::guess`]); a shorter one panics here
/// exactly where Python raises IndexError.
pub fn mark(guess: &str, word: &str) -> Vec<&'static str> {
    let g: Vec<char> = guess.chars().collect();
    let w: Vec<char> = word.chars().collect();
    let mut res = vec!["miss"; 5];
    // Python's `left: list[str | None] = list(word)`: the word's letters, each
    // struck out (None) once something has been credited against it.
    let mut left: Vec<Option<char>> = w.iter().map(|c| Some(*c)).collect();
    for i in 0..5 {
        if g[i] == w[i] {
            res[i] = "hit";
            left[i] = None;
        }
    }
    for i in 0..5 {
        // Python's `guess[i] in left` then `left[left.index(guess[i])] = None`:
        // membership over the STRUCK-OUT list, and the FIRST surviving
        // occurrence is the one consumed.
        if res[i] != "hit" {
            if let Some(j) = left.iter().position(|c| *c == Some(g[i])) {
                res[i] = "near";
                left[j] = None;
            }
        }
    }
    res
}

/// Python's `msg.get("round") != self.round` -- an `==` between a JSON value and
/// an int, so it is true for `1` and for `1.0`, and ALSO for `true` when the
/// round is 1, because in Python `True == 1`. Anything else (a string, null, a
/// missing key) is not equal.
fn is_round(v: Option<&Value>, round: i64) -> bool {
    match v {
        Some(Value::Number(n)) => match n.as_i64() {
            Some(i) => i == round,
            None => n.as_f64() == Some(round as f64),
        },
        Some(Value::Bool(b)) => i64::from(*b) == round,
        _ => false,
    }
}

/// Python's `isinstance(g, str) and re.fullmatch(r"[a-z]{5}", g)`. Hand-rolled
/// rather than pulled through a regex crate: `[a-z]` in a Python `str` pattern
/// is ASCII-only, so "exactly five ASCII lowercase bytes" is the same
/// predicate -- and because every byte must be ASCII, a 5-BYTE length test is a
/// 5-CHARACTER length test here.
fn is_guess(v: Option<&Value>) -> Option<&str> {
    let s = v?.as_str()?;
    (s.len() == 5 && s.bytes().all(|b| b.is_ascii_lowercase())).then_some(s)
}

/// Per-room state. One field of [`RoomValley`], in Python's `__init__` order
/// (valley.py:495-500).
#[derive(Default)]
pub struct Race {
    pub round: i64,
    /// The word in play, or None between rounds. Python's `self.word`, whose
    /// None-ness is half of `running()`.
    pub word: Option<String>,
    /// user_id -> guesses used this round. A [`Seq`] rather than a `HashMap`
    /// because every dict in this port that a page could ever see keeps its
    /// insertion order; this one does not reach the wire today.
    pub guesses: Seq<i64>,
    pub until: f64,
    /// user_id -> rounds won, IN THE ORDER PEOPLE FIRST WON ONE. This one DOES
    /// reach the wire, whole, in every `win` event.
    pub wins: Seq<i64>,
}

impl Race {
    /// Python's `running()` (valley.py:502): a word is out AND the clock has not
    /// run out. `t` is the hub's one `now()` read for this message (shared
    /// divergence 1).
    pub fn running(&self, t: f64) -> bool {
        self.word.is_some() && t < self.until
    }

    /// Python's `start` (valley.py:505).
    pub fn start(&mut self, cx: &mut Ctx, out: &mut Out) {
        if self.running(cx.t) {
            out.err(cx.conn, "a race is already running");
            return;
        }
        self.round += 1;
        // Python's `rng.choice(RACE_WORDS)` off the per-message `_rng()`. No
        // Rust port can reproduce CPython's Mersenne Twister, so the DRAW is
        // ported and not the sequence: the tests assert the word is one of
        // RACE_WORDS (shared divergence 2) and fix it by choosing a seed for
        // what it draws.
        self.word = Some(RACE_WORDS.choose(cx.rng).expect("RACE_WORDS is not empty").to_string());
        self.guesses.clear();
        self.until = cx.t + RACE_SECS as f64;
        out.all("start", json!({"round": self.round, "by": cx.me(), "secs": RACE_SECS}));
    }

    /// Python's `guess` (valley.py:518).
    pub fn guess(&mut self, cx: &mut Ctx, msg: &Value, out: &mut Out) {
        if !self.running(cx.t) || !is_round(msg.get("round"), self.round) {
            // A guess that arrives after the clock ran out is how the room
            // learns the round is over -- there is no race timer on the server.
            // `self.word` is Python's truthiness test on an `str | None`; a word
            // from RACE_WORDS is never "", so this is just "is there one". Note
            // the race must NOT be running: a guess for the wrong round
            // mid-race is only an error, never a timeout.
            if self.word.is_some() && !self.running(cx.t) {
                out.all("timeout", json!({"round": self.round, "word": self.word}));
                self.word = None; // said once; the next late guess only gets the error
            }
            out.err(cx.conn, "no race running");
            return;
        }
        let Some(g) = is_guess(msg.get("word")) else {
            out.err(cx.conn, "five letters, a-z");
            return;
        };
        let uid = cx.uid();
        let n = self.guesses.get(uid).copied().unwrap_or(0);
        if n >= RACE_GUESSES {
            out.err(cx.conn, "out of guesses");
            return;
        }
        self.guesses.set(uid, n + 1);
        let word = self.word.clone().expect("running() means there is a word");
        let marks = mark(g, &word);
        // The marks go to the guesser's socket ALONE ...
        out.to(cx.conn, "mark",
               json!({"round": self.round, "word": g, "marks": marks, "n": n + 1}));
        // ... and the room hears only how close they got, never which letters
        // (valley.py:533).
        let hits = marks.iter().filter(|m| **m == "hit").count();
        out.all("progress",
                json!({"round": self.round, "user": cx.me(), "n": n + 1, "hits": hits}));
        if g == word {
            let w = self.wins.get(uid).copied().unwrap_or(0) + 1;
            self.wins.set(uid, w);
            out.all("win", json!({"round": self.round, "user": cx.me(), "word": word,
                                  "n": n + 1, "wins": self.wins.to_object(|v| json!(v))}));
            self.word = None; // the round is over; `round` only moves on the next start
        }
    }
}

/// The join snapshot: Python's `elif g == "race" and v.race.running()`
/// (valley.py:1039). A joiner mid-round is re-sent the `start` so their page can
/// put the clock up -- and ONLY then; between rounds the join tail is silent.
///
/// `secs` is `max(0, round(until - now()))`: Python's banker's `round`, so
/// 178.5 seconds left is sent as 178, not 179. The `max(0, ..)` is defensive --
/// `running()` already means `t < until` -- and is kept because Python keeps it.
pub fn joined(v: &mut RoomValley, cx: &mut Ctx, out: &mut Out) {
    let r = &v.race;
    if r.running(cx.t) {
        out.to(cx.conn, "start",
               json!({"round": r.round, "secs": py_round(r.until - cx.t).max(0)}));
    }
}

/// This game's ops. Reached only past the lobby gate. An unrecognised op must
/// produce NOTHING, as Python's `else`-less if/elif chains do (valley.py:1106).
pub fn op(v: &mut RoomValley, cx: &mut Ctx, op: &str, msg: &Value, out: &mut Out) {
    match op {
        "start" => v.race.start(cx, out),
        "guess" => v.race.guess(cx, msg, out),
        _ => {}
    }
}

/// Someone left this game's lobby, by `leave` or by a dropped socket. Python's
/// `_leave_lobby` has NO `race` branch (valley.py:1541-1569), so nothing
/// happens: the word stays out, the clock keeps running, and the leaver's guess
/// count and wins survive a leave and a rejoin.
pub fn dropped(
    v: &mut RoomValley, uid: &str, who: &Value, out: &mut Out, t: f64, disconnected: bool,
) -> Left {
    let _ = (v, uid, who, out, t, disconnected);
    Left::Notice
}

#[cfg(test)]
mod tests {
    use super::super::testkit::*;
    use super::super::{game_name, ours, GAMES, I_RACE};
    use super::*;
    use rand::rngs::StdRng;
    use rand::SeedableRng;

    const ROOM: &str = "lobby";

    // ------------------------------------------------------------ the scorer --

    #[test]
    fn mark_handles_repeated_letters() {
        // backend/tests/test_valley.py:282, byte for byte.
        assert_eq!(mark("eerie", "there"), ["near", "miss", "near", "miss", "hit"]);
    }

    #[test]
    fn a_letter_of_the_word_is_credited_at_most_once() {
        // The case a strike-out-free `guess[i] in word` gets wrong. Word "state"
        // has two s's and one t; guess "tests" spends the t on the hit at index
        // 3 and both s's on the nears at 0 and 2, so the fifth slot has nothing
        // left and is a MISS where the naive scorer says "near".
        assert_eq!(mark("tests", "state"), ["near", "near", "near", "hit", "miss"]);
        // Five of a letter the word holds once: exactly one of them scores, and
        // the hit takes it.
        assert_eq!(mark("aaaaa", "cache"), ["miss", "hit", "miss", "miss", "miss"]);
        // Both of "trees"'s e's are struck out by hits, so the leading 's' finds
        // only the trailing 's' and the 'p' finds nothing.
        assert_eq!(mark("sleep", "trees"), ["near", "miss", "hit", "hit", "miss"]);
        // Three s's against "class"'s two: two score, the third does not.
        assert_eq!(mark("sassy", "class"), ["near", "near", "miss", "hit", "miss"]);
    }

    #[test]
    fn a_hit_is_struck_out_before_any_near_is_looked_for() {
        // Nears-before-hits would credit the leading 'a' of "alias" against
        // "float"'s only 'a' -- the one that scores the hit at index 3 -- and
        // answer "near" for index 0.
        assert_eq!(mark("alias", "float"), ["miss", "hit", "miss", "hit", "miss"]);
        assert_eq!(mark("array", "merge"), ["miss", "miss", "hit", "miss", "miss"]);
        assert_eq!(mark("alias", "local"), ["miss", "near", "miss", "hit", "miss"]);
        // The ends: the word itself, and a guess sharing one letter.
        assert_eq!(mark("cache", "cache"), ["hit", "hit", "hit", "hit", "hit"]);
        assert_eq!(mark("pivot", "sleep"), ["near", "miss", "miss", "miss", "miss"]);
        // The e2e case backend/tests/test_valley.py:274 pins.
        assert_eq!(mark("catch", "cache"), ["hit", "hit", "miss", "near", "near"]);
    }

    // ----------------------------------------------------------- the catalog --

    #[test]
    fn the_catalog_is_pythons_in_pythons_order() {
        assert_eq!(RACE_WORDS.len(), 134);
        assert_eq!(RACE_WORDS[0], "admin");
        assert_eq!(RACE_WORDS[133], "zones");
        assert_eq!(&RACE_WORDS[13..15], ["cache", "catch"]);
        // Every entry is exactly what mark() and the guess predicate assume.
        for w in RACE_WORDS {
            assert_eq!(w.len(), 5, "{w}");
            assert!(w.bytes().all(|b| b.is_ascii_lowercase()), "{w}");
        }
        let mut sorted = RACE_WORDS;
        sorted.sort_unstable();
        assert_eq!(sorted, RACE_WORDS); // the Python literal is alphabetical
        assert_eq!(RACE_GUESSES, 6);
        assert_eq!(RACE_SECS, 180);
    }

    /// The seed whose first `rng.choice(RACE_WORDS)` is `want`, SEARCHED rather
    /// than hardcoded: the Python test monkeypatches `RACE_WORDS` to `("cache",)`
    /// and a Rust `const` cannot be patched, so the only way to fix the word is
    /// through the dice the testkit injects. Searching keeps these tests honest
    /// if the `rand` version ever changes the stream.
    fn seed_for(want: &str) -> u64 {
        (0..10_000u64)
            .find(|s| *RACE_WORDS.choose(&mut StdRng::seed_from_u64(*s)).unwrap() == want)
            .unwrap_or_else(|| panic!("no seed in 10k draws gives {want}"))
    }

    fn word_of(e: &Env) -> Option<String> {
        e.hub.with_room(ROOM, |v| v.race.word.clone()).flatten()
    }

    #[tokio::test]
    async fn a_start_draws_a_five_letter_word_from_the_catalog() {
        // Shared divergence 2: the distribution ports, the seeded oracle does
        // not, so this asserts the property the Python test asserts.
        let e = env(4, 7);
        let (a, mut wa) = e.connect(ROOM, 1, "a").await;
        e.send(ROOM, 1, &a, GAME, "join", json!({})).await;
        e.send(ROOM, 1, &a, GAME, "start", json!({})).await;
        let m = until(&mut wa, GAME, "start").await;
        assert_eq!(m["round"], 1);
        assert_eq!(m["secs"], 180);
        assert_eq!(m["by"]["userId"], "a");
        let w = word_of(&e).unwrap();
        assert!(RACE_WORDS.contains(&w.as_str()), "{w} is not a race word");
        assert_eq!(e.hub.with_room(ROOM, |v| v.race.until), Some(1180.0));
    }

    // --------------------------------------------------------- a whole round --

    #[tokio::test]
    async fn a_guess_is_marked_privately_and_the_room_only_hears_how_close() {
        // backend/tests/test_valley.py:262, with the word fixed through the dice
        // instead of through a monkeypatched catalog.
        let e = env(4, seed_for("cache"));
        let (a, mut wa) = e.connect(ROOM, 1, "a").await;
        let (b, mut wb) = e.connect(ROOM, 2, "b").await;
        e.send(ROOM, 1, &a, GAME, "join", json!({})).await;
        e.send(ROOM, 2, &b, GAME, "join", json!({})).await;
        e.send(ROOM, 1, &a, GAME, "start", json!({})).await;
        let rnd = until(&mut wb, GAME, "start").await["round"].clone();
        assert_eq!(word_of(&e).as_deref(), Some("cache"));

        e.send(ROOM, 2, &b, GAME, "guess", json!({"round": rnd, "word": "catch"})).await;
        let mk = until(&mut wb, GAME, "mark").await;
        assert_eq!(mk["marks"], json!(["hit", "hit", "miss", "near", "near"]));
        assert_eq!(mk["word"], "catch");
        assert_eq!(mk["n"], 1);
        assert_eq!(mk["round"], 1);
        // a hears the progress and nothing that gives the word away.
        let prog = until(&mut wa, GAME, "progress").await;
        assert_eq!(prog["hits"], 2);
        assert_eq!(prog["n"], 1);
        assert_eq!(prog["user"]["userId"], "b");
        let keys = prog.as_object().unwrap();
        assert!(!keys.contains_key("word"));
        assert!(!keys.contains_key("marks"));
        // a never saw a `mark` for somebody else's guess.
        assert!(!drain(&mut wa).iter().any(|(_, ev)| ev == "mark"));

        e.send(ROOM, 1, &a, GAME, "guess", json!({"round": rnd, "word": "cache"})).await;
        let win = until(&mut wb, GAME, "win").await;
        assert_eq!(win["user"]["userId"], "a");
        assert_eq!(win["word"], "cache");
        assert_eq!(win["n"], 1);
        assert_eq!(win["round"], 1);
        assert_eq!(win["wins"], json!({"a": 1}));
        // The word is taken off the table, but the round number stays put until
        // somebody starts the next one.
        assert!(word_of(&e).is_none());
        assert_eq!(e.hub.with_room(ROOM, |v| v.race.round), Some(1));
    }

    #[tokio::test]
    async fn the_wins_table_keeps_the_order_people_first_won_in() {
        let e = env(4, seed_for("admin"));
        let (a, mut wa) = e.connect(ROOM, 1, "a").await;
        let (b, _wb) = e.connect(ROOM, 2, "b").await;
        e.send(ROOM, 1, &a, GAME, "join", json!({})).await;
        e.send(ROOM, 2, &b, GAME, "join", json!({})).await;
        // Every message gets a fresh rng from the same factory, so every round
        // draws "admin".
        for (conn, m) in [(2u64, &b), (1, &a), (2, &b)] {
            e.send(ROOM, 1, &a, GAME, "start", json!({})).await;
            let rnd = e.hub.with_room(ROOM, |v| v.race.round).unwrap();
            e.send(ROOM, conn, m, GAME, "guess", json!({"round": rnd, "word": "admin"})).await;
        }
        let win = until_where(&mut wa, GAME, "win", |m| m["round"] == 3).await;
        // b won first, so b stays first in the table even though a has won since.
        assert_eq!(win["wins"].to_string(), r#"{"b":2,"a":1}"#);
        assert_eq!(win["n"], 1);
    }

    #[tokio::test]
    async fn a_race_goes_to_the_whole_room_and_the_mark_to_one_socket() {
        // The audience rule for this engine: start/progress/win are out.all, so
        // a socket that never joined the lobby still sees them; only the mark is
        // private. (The shared layer's note: getting this wrong fails silently.)
        let e = env(4, seed_for("cache"));
        let (a, _wa) = e.connect(ROOM, 1, "a").await;
        let (_z, mut wz) = e.connect(ROOM, 9, "z").await; // in the room, not the lobby
        e.send(ROOM, 1, &a, GAME, "join", json!({})).await;
        e.send(ROOM, 1, &a, GAME, "start", json!({})).await;
        e.send(ROOM, 1, &a, GAME, "guess", json!({"round": 1, "word": "cache"})).await;
        let seen: Vec<String> = drain(&mut wz).into_iter().map(|(_, ev)| ev).collect();
        assert_eq!(seen, ["lobby", "start", "progress", "win"]);
    }

    // ----------------------------------------------------- the error strings --

    #[tokio::test]
    async fn a_second_start_while_one_runs_is_refused() {
        let e = env(4, 1);
        let (a, mut wa) = e.connect(ROOM, 1, "a").await;
        e.send(ROOM, 1, &a, GAME, "join", json!({})).await;
        e.send(ROOM, 1, &a, GAME, "start", json!({})).await;
        until(&mut wa, GAME, "start").await;
        e.send(ROOM, 1, &a, GAME, "start", json!({})).await;
        assert_eq!(until(&mut wa, GAME, "error").await["error"], "a race is already running");
        assert_eq!(e.hub.with_room(ROOM, |v| v.race.round), Some(1));
        // Once the clock is out, a start is allowed again and the round moves on.
        e.advance(RACE_SECS as f64);
        e.send(ROOM, 1, &a, GAME, "start", json!({})).await;
        assert_eq!(until_where(&mut wa, GAME, "start", |m| m["round"] == 2).await["secs"], 180);
    }

    #[tokio::test]
    async fn a_guess_for_the_wrong_round_is_refused_without_timing_the_race_out() {
        let e = env(4, 1);
        let (a, mut wa) = e.connect(ROOM, 1, "a").await;
        e.send(ROOM, 1, &a, GAME, "join", json!({})).await;
        e.send(ROOM, 1, &a, GAME, "start", json!({})).await;
        until(&mut wa, GAME, "start").await;
        for round in [json!(2), json!("1"), Value::Null, json!(1.5)] {
            e.send(ROOM, 1, &a, GAME, "guess", json!({"round": round, "word": "cache"})).await;
            assert_eq!(until(&mut wa, GAME, "error").await["error"], "no race running");
        }
        // The race is still running: no timeout went out and the word is still up.
        assert!(!drain(&mut wa).iter().any(|(_, ev)| ev == "timeout"));
        assert!(word_of(&e).is_some());
        // Python compares with `!=` against an int, so 1.0 is round 1 ... and so
        // is `true`, because in Python `True == 1`.
        assert!(is_round(Some(&json!(1.0)), 1));
        assert!(is_round(Some(&json!(true)), 1));
        assert!(is_round(Some(&json!(false)), 0));
        assert!(!is_round(Some(&json!("1")), 1));
        assert!(!is_round(None, 1));
    }

    #[tokio::test]
    async fn the_room_is_told_the_word_once_when_the_clock_has_run_out() {
        let e = env(4, 1);
        let (a, mut wa) = e.connect(ROOM, 1, "a").await;
        e.send(ROOM, 1, &a, GAME, "join", json!({})).await;
        e.send(ROOM, 1, &a, GAME, "start", json!({})).await;
        until(&mut wa, GAME, "start").await;
        let word = word_of(&e).unwrap();
        e.advance(RACE_SECS as f64); // `running()` is a strict `t < until`
        e.send(ROOM, 1, &a, GAME, "guess", json!({"round": 1, "word": &word})).await;
        let t = until(&mut wa, GAME, "timeout").await;
        assert_eq!(t["round"], 1);
        assert_eq!(t["word"], word);
        assert_eq!(until(&mut wa, GAME, "error").await["error"], "no race running");
        // Said once: the word is cleared, so a second late guess is only an error.
        e.send(ROOM, 1, &a, GAME, "guess", json!({"round": 1, "word": &word})).await;
        assert_eq!(drain(&mut wa), vec![(GAME.to_string(), "error".to_string())]);
    }

    #[tokio::test]
    async fn a_guess_after_a_win_is_no_race_running_and_times_nothing_out() {
        let e = env(4, seed_for("cache"));
        let (a, mut wa) = e.connect(ROOM, 1, "a").await;
        e.send(ROOM, 1, &a, GAME, "join", json!({})).await;
        e.send(ROOM, 1, &a, GAME, "start", json!({})).await;
        e.send(ROOM, 1, &a, GAME, "guess", json!({"round": 1, "word": "cache"})).await;
        until(&mut wa, GAME, "win").await;
        let _ = drain(&mut wa);
        e.send(ROOM, 1, &a, GAME, "guess", json!({"round": 1, "word": "cache"})).await;
        // The clock has NOT run out, but the word is gone, so there is nothing
        // to time out -- just the error.
        assert_eq!(drain(&mut wa), vec![(GAME.to_string(), "error".to_string())]);
    }

    #[tokio::test]
    async fn only_five_lowercase_letters_are_a_guess() {
        let e = env(4, 1);
        let (a, mut wa) = e.connect(ROOM, 1, "a").await;
        e.send(ROOM, 1, &a, GAME, "join", json!({})).await;
        e.send(ROOM, 1, &a, GAME, "start", json!({})).await;
        until(&mut wa, GAME, "start").await;
        for w in [json!("cat"), json!("caches"), json!("CACHE"), json!("cach3"), json!("ca he"),
                  json!("café"), json!("cachä"), json!(12345), json!(true), Value::Null] {
            e.send(ROOM, 1, &a, GAME, "guess", json!({"round": 1, "word": w})).await;
            assert_eq!(until(&mut wa, GAME, "error").await["error"], "five letters, a-z");
        }
        // A refused guess costs nothing: the counter never moved.
        assert_eq!(e.hub.with_room(ROOM, |v| v.race.guesses.len()), Some(0));
        // "cachä" is 6 BYTES and 5 characters; the predicate is ASCII-only, so
        // both readings refuse it.
        assert!(is_guess(Some(&json!("cache"))).is_some());
        assert!(is_guess(Some(&json!("cachä"))).is_none());
    }

    #[tokio::test]
    async fn six_guesses_and_then_you_are_out() {
        let e = env(4, seed_for("cache"));
        let (a, mut wa) = e.connect(ROOM, 1, "a").await;
        e.send(ROOM, 1, &a, GAME, "join", json!({})).await;
        e.send(ROOM, 1, &a, GAME, "start", json!({})).await;
        for i in 1..=RACE_GUESSES {
            e.send(ROOM, 1, &a, GAME, "guess", json!({"round": 1, "word": "zones"})).await;
            assert_eq!(until(&mut wa, GAME, "mark").await["n"], i);
        }
        e.send(ROOM, 1, &a, GAME, "guess", json!({"round": 1, "word": "zones"})).await;
        assert_eq!(until(&mut wa, GAME, "error").await["error"], "out of guesses");
        assert_eq!(e.hub.with_room(ROOM, |v| v.race.guesses.get("a").copied()), Some(Some(6)));
        // The allowance is per round: a new start wipes the counters.
        e.advance(RACE_SECS as f64);
        e.send(ROOM, 1, &a, GAME, "start", json!({})).await;
        assert_eq!(e.hub.with_room(ROOM, |v| v.race.guesses.len()), Some(0));
    }

    #[tokio::test]
    async fn an_unknown_race_op_is_answered_with_nothing_at_all() {
        // Python's if/elif chain has no `else`, so only an unknown GAME is
        // answered (valley.py:1106-1110).
        let e = env(4, 1);
        let (a, mut wa) = e.connect(ROOM, 1, "a").await;
        e.send(ROOM, 1, &a, GAME, "join", json!({})).await;
        let _ = drain(&mut wa);
        for op in ["mark", "stop", "win", "Start"] {
            e.send(ROOM, 1, &a, GAME, op, json!({})).await;
        }
        assert!(drain(&mut wa).is_empty());
    }

    // ------------------------------------------------------- join and leave --

    #[tokio::test]
    async fn joining_mid_round_resends_the_start_with_the_seconds_left() {
        let e = env(4, 1);
        let (a, _wa) = e.connect(ROOM, 1, "a").await;
        e.send(ROOM, 1, &a, GAME, "join", json!({})).await;
        e.send(ROOM, 1, &a, GAME, "start", json!({})).await;
        e.advance(1.5);
        let (b, mut wb) = e.connect(ROOM, 2, "b").await;
        e.send(ROOM, 2, &b, GAME, "join", json!({})).await;
        let m = until(&mut wb, GAME, "start").await;
        assert_eq!(m["round"], 1);
        // 178.5 seconds left: Python's banker's `round` says 178 where Rust's
        // `f64::round` would say 179.
        assert_eq!(m["secs"], 178);
        // The replay is the joiner's alone and carries no `by`.
        assert!(!m.as_object().unwrap().contains_key("by"));
        // The shared `lobby` came first and the tail after it; nothing else.
        assert_eq!(drain(&mut wb), Vec::new());
    }

    #[tokio::test]
    async fn joining_between_rounds_replays_nothing() {
        let e = env(4, 1);
        let (a, _wa) = e.connect(ROOM, 1, "a").await;
        // Before any round ...
        let (b, mut wb) = e.connect(ROOM, 2, "b").await;
        e.send(ROOM, 2, &b, GAME, "join", json!({})).await;
        assert_eq!(drain(&mut wb), vec![(GAME.to_string(), "lobby".to_string())]);
        // ... and after one has run out.
        e.send(ROOM, 1, &a, GAME, "join", json!({})).await;
        e.send(ROOM, 1, &a, GAME, "start", json!({})).await;
        e.advance(RACE_SECS as f64);
        let (c, mut wc) = e.connect(ROOM, 3, "c").await;
        e.send(ROOM, 3, &c, GAME, "join", json!({})).await;
        assert!(!drain(&mut wc).iter().any(|(_, ev)| ev == "start"));
    }

    #[tokio::test]
    async fn leaving_the_race_keeps_the_round_and_your_guess_count() {
        // Python's `_leave_lobby` has no `race` branch: the round survives
        // everyone walking out, and so does what they spent.
        let e = env(4, seed_for("cache"));
        let (a, mut wa) = e.connect(ROOM, 1, "a").await;
        let (keep, _wk) = e.connect(ROOM, 2, "keep").await; // holds the room open
        e.send(ROOM, 2, &keep, GAME, "join", json!({})).await;
        e.send(ROOM, 1, &a, GAME, "join", json!({})).await;
        e.send(ROOM, 1, &a, GAME, "start", json!({})).await;
        e.send(ROOM, 1, &a, GAME, "guess", json!({"round": 1, "word": "zones"})).await;
        until(&mut wa, GAME, "mark").await;
        let _ = drain(&mut wa);
        e.send(ROOM, 1, &a, GAME, "leave", json!({})).await;
        // The leave says only the shared `lobby`: the race itself has nothing to
        // say about it.
        assert_eq!(drain(&mut wa), vec![(GAME.to_string(), "lobby".to_string())]);
        assert_eq!(word_of(&e).as_deref(), Some("cache"));
        // Back in, and still on guess two of six.
        e.send(ROOM, 1, &a, GAME, "join", json!({})).await;
        e.send(ROOM, 1, &a, GAME, "guess", json!({"round": 1, "word": "zones"})).await;
        assert_eq!(until(&mut wa, GAME, "mark").await["n"], 2);
    }

    #[tokio::test]
    async fn a_dropped_socket_leaves_the_race_running() {
        let e = env(4, seed_for("cache"));
        let (a, _wa) = e.connect(ROOM, 1, "a").await;
        let (b, mut wb) = e.connect(ROOM, 2, "b").await;
        e.send(ROOM, 1, &a, GAME, "join", json!({})).await;
        e.send(ROOM, 2, &b, GAME, "join", json!({})).await;
        e.send(ROOM, 1, &a, GAME, "start", json!({})).await;
        let _ = drain(&mut wb);
        e.disconnect(ROOM, 1, &a).await;
        assert_eq!(drain(&mut wb), vec![(GAME.to_string(), "lobby".to_string())]);
        assert_eq!(word_of(&e).as_deref(), Some("cache"));
        // b can still win the round a started.
        e.send(ROOM, 2, &b, GAME, "guess", json!({"round": 1, "word": "cache"})).await;
        assert_eq!(until(&mut wb, GAME, "win").await["wins"], json!({"b": 1}));
    }

    #[test]
    fn the_race_is_the_second_lobby_and_puzzle_race_on_the_wire() {
        assert_eq!(GAME, "race");
        assert_eq!(GAMES[I_RACE], GAME);
        assert_eq!(game_name(GAME), "Puzzle Race");
        assert!(ours(GAME));
    }
}
