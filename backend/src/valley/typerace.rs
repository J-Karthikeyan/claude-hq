//! The Code Typing Race: up to eight people race to type the same real code
//! snippet. A port of `backend/app/typerace.py` plus `type_op` and
//! `_type_tick_on` (`backend/app/valley.py:1223-1269`).
//!
//! The server picks the snippet, runs the countdown and judges progress: a
//! position can only move FORWARD, never past the snippet, and never faster than
//! [`MAX_CPS`] characters a second with a [`BURST`] allowance. Finishing time,
//! words per minute and accuracy make the result, and the "done" event feeds the
//! leaderboards through `results.rs`.
//!
//! AUDIENCE. Everything here goes to THIS GAME'S LOBBY, never to the whole room:
//! `out.to(member.ws, ..)` for the join snapshot and `view`, `out.lobby(ids, ..)`
//! for `start`, `end` and every tick event. type is on kart/plat/fps's side of
//! the audience split described in `valley.rs`, not pond/race/duel's. The shared
//! `lobby` event the dispatcher sends around it is the only [`super::To::All`]
//! frame a type player sees, and it is not ours.
//!
//! IT TICKS, at [`HZ`] = 5, and its step is the shape `_type_tick_on` sets:
//!   - `charge: None`, because `_type_tick_on` never calls `tk.count(..)`. Its
//!     snapshots sit outside `realtime::ROOM_BYTES_PER_SEC` and are never
//!     thinned, unlike kart's.
//!   - the `send` flag is IGNORED entirely -- `_type_tick_on`'s step takes it and
//!     never reads it, so a tick the budget would have thinned still sends.
//!     (Both deliberate; hq honours `send` where we do not.)
//!   - the loop's own liveness test is `not v.type.running()`, inside the step,
//!     so it stops itself the tick after a race finishes or is ended.
//!
//! The shared divergences from Python are listed once in `valley.rs`; this file
//! adds the three noted at [`TypeRace::start`], [`TypeRace::tick`] and
//! [`super::int_exact`].

use super::{int_exact, py_round_to, py_trunc, Ctx, Left, Out, RoomValley, Seq, Tick, To, I_TYPE};
use rand::rngs::StdRng;
use rand::Rng;
use serde_json::{json, Value};
use std::sync::Arc;

/// The game key on the wire.
pub const GAME: &str = "type";

/// Ticks a second. Python's `HZ = 5` is an int; `tick_on` wants a rate.
pub const HZ: f64 = 5.0;
/// Seconds between `start` and "go".
pub const COUNTDOWN: f64 = 3.0;
/// A race is over this long after "go" whatever anyone has typed.
pub const MAX_SECS: f64 = 180.0;
/// 300 WPM: faster than anyone types.
pub const MAX_CPS: f64 = 25.0;
/// Characters of credit a typist may spend at once.
pub const BURST: i64 = 12;
/// After the first finisher, the rest have this long.
pub const FINISH_GRACE: f64 = 20.0;

/// Seats in a race. Python writes the literal `[:8]` in `TypeRace.start`
/// (typerace.py:76) rather than reading `valley.MAX_LOBBY`, and typerace.py
/// imports nothing from valley, so the two numbers are equal by coincidence and
/// not by construction. Kept as its own constant for that reason.
pub const SEATS: usize = 8;

/// The ceiling `_int` puts on a reported error count (typerace.py:80). A
/// position is bounded by the snippet's length; mistakes are not, so they get
/// their own bound.
pub const MAX_ERRS: i64 = 100_000;

/// `(language, source)`, Python's `SNIPPETS` (typerace.py:17-30) byte for byte
/// and in order -- the order is not load-bearing (the pick is uniform), but the
/// table is what a test compares against.
pub const SNIPPETS: [(&str, &str); 12] = [
    ("python", "def fib(n):\n    a, b = 0, 1\n    for _ in range(n):\n        a, b = b, a + b\n    return a"),
    ("python", "with open(path, encoding=\"utf-8\") as f:\n    rows = [line.split(\",\") for line in f if line.strip()]"),
    ("python", "counts = {}\nfor word in text.split():\n    counts[word] = counts.get(word, 0) + 1"),
    ("javascript", "const debounce = (fn, ms) => {\n  let t;\n  return (...a) => { clearTimeout(t); t = setTimeout(() => fn(...a), ms); };\n};"),
    ("javascript", "fetch(\"/api/sessions\").then(r => r.json()).then(d => console.log(d.sessions.length));"),
    ("javascript", "const sum = xs => xs.reduce((a, b) => a + b, 0);\nconsole.log(sum([3, 4, 5]));"),
    ("rust", "fn main() {\n    let v: Vec<i32> = (1..=10).filter(|x| x % 2 == 0).collect();\n    println!(\"{:?}\", v);\n}"),
    ("rust", "impl Point {\n    fn dist(&self, o: &Point) -> f64 {\n        ((self.x - o.x).powi(2) + (self.y - o.y).powi(2)).sqrt()\n    }\n}"),
    ("sql", "SELECT city, count(*) AS rides\nFROM rides\nWHERE status = 'completed'\nGROUP BY city\nORDER BY rides DESC;"),
    ("sql", "UPDATE users SET last_seen = now() WHERE id = $1 RETURNING id, last_seen;"),
    ("shell", "git log --oneline -n 20 | grep -i fix | wc -l"),
    ("shell", "find . -name \"*.py\" -not -path \"./.venv/*\" | xargs wc -l | sort -n | tail -5"),
];

/// A race already running (typerace.py:71).
pub const ERR_ON: &str = "a race is already on";
/// `TypeRace.start` with nobody seated (typerace.py:73). The same words the
/// dispatcher's own lobby gate uses, which is why it is a constant here and not
/// a literal in two places.
pub const ERR_EMPTY: &str = "join the lobby first";
/// Only the lobby's host may `start` or `end` (valley.py:1233).
pub const ERR_HOST: &str = "only the host can do that";
/// `realtime.start` refused: the process is at `realtime::MAX_TICKERS`
/// (valley.py:1240).
pub const ERR_BUSY: &str = "the Arena is busy right now: try again in a minute";

/// Python's `self.phase` string, which reaches the wire through
/// [`TypeRace::view`]. An enum rather than a `String` so a typo cannot
/// mis-compare; [`Phase::as_str`] is the only thing the page ever sees.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Phase {
    #[default]
    Idle,
    Countdown,
    Run,
    Done,
}

impl Phase {
    pub fn as_str(self) -> &'static str {
        match self {
            Phase::Idle => "idle",
            Phase::Countdown => "countdown",
            Phase::Run => "run",
            Phase::Done => "done",
        }
    }
}

/// One typist. Python's `self.players[uid]` dict (typerace.py:76).
#[derive(Debug, Clone)]
pub struct Player {
    /// Their public profile, as stored at `start`. The results carry this, not a
    /// user id, which is why `results.rs` reads `r["user"]["userId"]`.
    pub user: Value,
    pub pos: i64,
    pub err: i64,
    /// Milliseconds after "go", once they typed the last character.
    pub fin: Option<i64>,
    /// When their budget was last settled.
    pub at: f64,
    /// Characters of credit left. NOT floored at zero -- see [`TypeRace::prog`].
    pub budget: f64,
}

/// Per-room state. One field of [`RoomValley`], in Python's `__init__` order.
/// Python's `TypeRace` (typerace.py:46).
#[derive(Debug, Default)]
pub struct TypeRace {
    pub phase: Phase,
    /// NOT cleared by [`TypeRace::end`], exactly as Python's `end` leaves it, so
    /// an idle view still names the last snippet's language.
    pub lang: String,
    pub text: String,
    pub go_at: f64,
    pub first_at: Option<f64>,
    /// Insertion-ordered: the order decides the `prog` event's `ps` list and the
    /// results' tie-break, and it reaches the wire. A `HashMap` would randomise
    /// it. See [`Seq`].
    pub players: Seq<Player>,
    pub results: Option<Vec<Value>>,
    pub dirty: bool,
}

impl TypeRace {
    /// Python's `running()` (typerace.py:56): countdown or run. The loop's
    /// liveness test and `start`'s refusal both read it.
    pub fn running(&self) -> bool {
        matches!(self.phase, Phase::Countdown | Phase::Run)
    }

    /// `len(self.text)` -- a CHARACTER count, because Python's `len` over a
    /// `str` counts code points. Every snippet in [`SNIPPETS`] is ASCII, so this
    /// equals the byte length today; it is written this way so a non-ASCII
    /// snippet would still bound `pos` the way Python bounds it.
    fn text_len(&self) -> i64 {
        self.text.chars().count() as i64
    }

    /// Python's `view(t)` (typerace.py:59), key for key and in order.
    pub fn view(&self, t: f64) -> Value {
        json!({
            "phase": self.phase.as_str(),
            "lang": self.lang,
            // The snippet is withheld while idle so nobody can pre-read the next
            // race -- except there is no next race yet, so this is really just
            // "nothing to type".
            "text": if self.phase != Phase::Idle { self.text.as_str() } else { "" },
            // `max(0, int(..))`: int() TRUNCATES toward zero before the clamp.
            "goInMs": if self.phase == Phase::Countdown {
                py_trunc((self.go_at - t) * 1000.0).max(0)
            } else {
                0
            },
            "players": Value::Array(self.players.values()
                .map(|p| json!({"user": p.user, "pos": p.pos, "fin": p.fin}))
                .collect()),
            "results": self.results,
        })
    }

    /// Python's `start(members, t, rng)` (typerace.py:69): the error string, or
    /// None when the countdown is on. `members` is the lobby in JOIN ORDER, and
    /// only the first [`SEATS`] of them get a seat.
    ///
    /// Divergence (shared divergence 2 in `valley.rs`): no Rust port reproduces
    /// CPython's Mersenne Twister, so `rng.choice(SNIPPETS)` becomes a uniform
    /// draw over the same table. The DISTRIBUTION ports; the seeded sequence
    /// does not, so the tests assert "a snippet from the table" rather than
    /// which one.
    pub fn start(
        &mut self, members: &[(String, Value)], t: f64, rng: &mut StdRng,
    ) -> Option<&'static str> {
        if self.running() {
            return Some(ERR_ON);
        }
        if members.is_empty() {
            return Some(ERR_EMPTY);
        }
        let (lang, text) = SNIPPETS[rng.gen_range(0..SNIPPETS.len())];
        self.lang = lang.to_string();
        self.text = text.to_string();
        self.phase = Phase::Countdown;
        self.go_at = t + COUNTDOWN;
        self.first_at = None;
        self.results = None;
        // A FRESH table, as Python's dict comprehension builds one: last race's
        // typists are gone even if they never left the lobby.
        self.players = Seq::new();
        for (uid, pubv) in members.iter().take(SEATS) {
            self.players.set(uid, Player { user: pubv.clone(), pos: 0, err: 0, fin: None,
                                           // Nobody may spend budget before "go".
                                           at: t + COUNTDOWN, budget: BURST as f64 });
        }
        self.dirty = true;
        None
    }

    /// Python's `prog(uid, msg, t)` (typerace.py:82): judge one progress frame.
    /// True when it moved the race, which only decides whether the next tick
    /// sends a `prog` event -- the caller discards it.
    ///
    /// THE TWO TRAPS, both pinned by tests:
    ///   - the budget AND `at` are settled BEFORE the step is checked, so a
    ///     frame rejected as too fast still charges the clock. With time moving
    ///     forward that is unobservable (the refill is capped and monotone);
    ///     with time moving BACKWARDS, which the Python's own unit test does, it
    ///     is not, and the budget can go NEGATIVE because `min(BURST, ..)` only
    ///     caps it from above.
    ///   - `p["err"] = max(p["err"], err)`: the error count only ever rises, so
    ///     a client cannot type badly and then claim a clean run.
    pub fn prog(&mut self, uid: &str, msg: &Value, t: f64) -> bool {
        let n = self.text_len();
        // Hoisted because `p` borrows `self.players` for the rest of the body.
        let (phase, go_at) = (self.phase, self.go_at);
        let Some(p) = self.players.get_mut(uid) else { return false };
        if phase != Phase::Run || p.fin.is_some() {
            return false;
        }
        // `_int` is the shared `int_exact`: rejects a bool, requires `v ==
        // int(v)` so 10.0 passes and 10.5 does not, and REJECTS out of range
        // rather than clamping.
        let (pos, err) = (int_exact(msg.get("pos"), 0, n), int_exact(msg.get("err"), 0, MAX_ERRS));
        let (Some(pos), Some(err)) = (pos, err) else { return false };
        if pos < p.pos {
            return false; // never backwards
        }
        p.budget = (BURST as f64).min(p.budget + (t - p.at) * MAX_CPS);
        p.at = t;
        let step = pos - p.pos;
        if step as f64 > p.budget {
            return false; // faster than anyone types: ignored
        }
        p.budget -= step as f64;
        p.pos = pos;
        p.err = p.err.max(err);
        let finished = pos == n;
        if finished {
            p.fin = Some(py_trunc((t - go_at) * 1000.0));
        }
        // `p`'s borrow ends above, so `self.first_at` is reachable again.
        if finished && self.first_at.is_none() {
            self.first_at = Some(t);
        }
        self.dirty = true;
        true
    }

    /// Python's `drop(uid)` (typerace.py:104). Named apart from `drop` because
    /// that is `Drop::drop`. The dispatcher discards the answer: leaving a type
    /// race raises no event of its own, only the shared `lobby` one.
    pub fn drop_player(&mut self, uid: &str) -> bool {
        self.players.remove(uid).is_some()
    }

    /// Python's `end()` (typerace.py:107): back to idle, nobody seated, no
    /// results. Note it clears neither `dirty` nor `lang` nor `text` -- `view`
    /// hides the text while idle, and the next `start` overwrites all three.
    pub fn end(&mut self) {
        self.phase = Phase::Idle;
        self.players.clear();
        self.results = None;
    }

    /// Python's `tick(t)` (typerace.py:110): the events one tick produces, in
    /// order. `(ev, data)` pairs, as Python's list of tuples.
    ///
    /// Divergence: none in behaviour, but note the shape. The "done" branch
    /// `return`s, so a tick that finishes the race sends NO `prog` event even
    /// when `dirty` is set, and `dirty` STAYS set. That is Python's early
    /// return; nothing observes the stale flag, because `start` sets it anyway
    /// and `running()` is false until then.
    pub fn tick(&mut self, t: f64) -> Vec<(&'static str, Value)> {
        let mut evs: Vec<(&'static str, Value)> = Vec::new();
        if self.phase == Phase::Countdown && t >= self.go_at {
            self.phase = Phase::Run;
            evs.push(("go", json!({})));
            // Python falls straight into the `run` block in the SAME tick, so a
            // race nobody is left in can go and finish at once.
        }
        if self.phase == Phase::Run {
            // `self.players and all(..)`: an EMPTY table is falsy, so "everyone
            // finished" is false when there is nobody. `not self.players` is the
            // separate fourth reason below.
            let everyone =
                !self.players.is_empty() && self.players.values().all(|p| p.fin.is_some());
            let late = self.first_at.map(|f| t - f >= FINISH_GRACE).unwrap_or(false);
            if everyone || late || t - self.go_at >= MAX_SECS || self.players.is_empty() {
                self.phase = Phase::Done;
                let n = self.text_len();
                // `sorted` is STABLE, and `Vec::sort_by_key` is too: finishers
                // first by time, then the rest by how far they got, and ties
                // keep seat order. `kv[1]["fin"] or 0` and `unwrap_or(0)` agree
                // on every value, Some(0) included.
                let mut order: Vec<&Player> = self.players.values().collect();
                order.sort_by_key(|p| (p.fin.is_none(), p.fin.unwrap_or(0), -p.pos));
                let mut results = Vec::with_capacity(order.len());
                for (i, p) in order.iter().enumerate() {
                    let ms = p.fin;
                    // Python's `if ms` is TRUTHINESS, not `is not None`: a
                    // finisher clocked at exactly 0 ms takes the unfinished
                    // branch and has their WPM measured over elapsed time.
                    let wpm = match ms {
                        Some(m) if m != 0 => {
                            py_round_to((n as f64 / 5.0) / (m as f64 / 60000.0), 1)
                        }
                        _ => py_round_to(
                            (p.pos as f64 / 5.0) / 1e-6f64.max((t - self.go_at) / 60.0), 1),
                    };
                    results.push(json!({
                        "user": p.user,
                        "place": i + 1,
                        "ms": ms,
                        "dnf": ms.is_none(),
                        "pos": p.pos,
                        "wpm": wpm,
                        // A snippet of length 0 cannot happen through `start`;
                        // Python guards it anyway and so do we, and 100.0 is a
                        // float on the wire, not 100.
                        "acc": if n != 0 {
                            py_round_to((100 * n) as f64 / (n + p.err) as f64, 1)
                        } else {
                            100.0
                        },
                    }));
                }
                self.results = Some(results.clone());
                evs.push(("done", json!({"results": results, "lang": self.lang})));
                return evs;
            }
        }
        if self.dirty {
            self.dirty = false;
            evs.push(("prog", json!({"ps": Value::Array(self.players.iter()
                .map(|(uid, p)| json!({"u": uid, "pos": p.pos, "fin": p.fin}))
                .collect())})));
        }
        evs
    }
}

/// The join snapshot: Python's `out.to(member.ws, "type", race=v.type.view(now()))`
/// (valley.py:1066). One socket, not the lobby -- a joiner's own catch-up is
/// nobody else's business. It does NOT start the loop: a race already running
/// has one, and an idle lobby needs none.
pub fn joined(v: &mut RoomValley, cx: &mut Ctx, out: &mut Out) {
    out.to(cx.conn, "type", json!({"race": v.type_race.view(cx.t)}));
}

/// This game's ops. Python's `type_op` (valley.py:1223). Reached only past the
/// lobby gate. An unrecognised op produces NOTHING, as Python's `else`-less
/// if/elif chain does.
pub fn op(v: &mut RoomValley, cx: &mut Ctx, op: &str, msg: &Value, out: &mut Out) {
    let t = cx.t;
    // Python takes `ids = list(lobby.members)` ONCE, at the top, before the op
    // runs. No type op changes the lobby, so this is the same list either way.
    let ids = v.lobbies[I_TYPE].ids();
    match op {
        // The answer is discarded: progress reaches the room on the next tick,
        // batched, never as its own frame. A rejected frame is silent -- no
        // error, because a client that is merely ahead of the speed cap is not
        // misbehaving.
        "prog" => {
            v.type_race.prog(cx.uid(), msg, t);
        }
        "view" => out.to(cx.conn, "type", json!({"race": v.type_race.view(t)})),
        "start" | "end" => {
            if v.lobbies[I_TYPE].host.as_deref() != Some(cx.uid()) {
                out.err(cx.conn, ERR_HOST);
                return;
            }
            // Note type_op does NOT clear `lobby.prev_host` here, where kart's
            // and plat's start/end arms do (kart.rs:914). Faithful to
            // valley.py:1236.
            if op == "start" {
                // Disjoint field borrows of `*v`: the lobby's members read-only,
                // the race mutably.
                let members = &v.lobbies[I_TYPE].members;
                let mut err = v.type_race.start(members, t, cx.rng);
                if err.is_none() && !cx.hub.tick_on(GAME, cx.room_id, HZ, Arc::new(step)) {
                    // No loop, no race: roll the start back before answering, or
                    // the lobby would sit in a countdown nothing advances.
                    v.type_race.end();
                    err = Some(ERR_BUSY);
                }
                match err {
                    Some(e) => out.err(cx.conn, e),
                    None => out.lobby(ids, "type",
                                      json!({"race": v.type_race.view(t), "by": cx.me()})),
                }
            } else {
                v.type_race.end();
                cx.hub.tick_off(GAME, cx.room_id);
                out.lobby(ids, "type", json!({"race": v.type_race.view(t)}));
            }
        }
        _ => {}
    }
}

/// One tick of the race. Python's `_type_tick_on`'s inner `step`
/// (valley.py:1254). The shared [`super::ValleyHub::step`] has already checked
/// the room exists and taken the lock; this is the varying half.
///
/// `send` is ignored, and `charge` is None: see the header.
fn step(v: &mut RoomValley, t: f64, _send: bool) -> Tick {
    // Python's `if v is None or room is None or not v.type.running(): return
    // False`. The first two are the shared half's job; this is the third.
    if !v.type_race.running() {
        return Tick { keep: false, items: Vec::new(), charge: None };
    }
    // The lobby, plus any typist who has since left it -- so someone who closed
    // the tab mid-race still sees the race they were in finish, as long as
    // another socket of theirs is in the room. Order: lobby first, Python's
    // `list(members) + [u for u in players if u not in members]`.
    let lobby = &v.lobbies[I_TYPE];
    let mut ids = lobby.ids();
    for u in v.type_race.players.keys() {
        if !lobby.has(u) {
            ids.push(u.to_string());
        }
    }
    let items = v
        .type_race
        .tick(t)
        .into_iter()
        .map(|(ev, data)| (To::Users(ids.clone(), None), ev.to_string(), data))
        .collect();
    // Python returns `v.type.running()` AFTER the tick, so the tick that
    // finishes a race is the last one.
    Tick { keep: v.type_race.running(), items, charge: None }
}

/// Someone left this game's lobby, by `leave` or by a dropped socket. Python:
/// `elif g == "type": v.type.drop(user_id)` (valley.py:1560) -- the answer is
/// discarded and no type event is sent, so the only frame is the dispatcher's
/// shared `lobby` one. [`Left::Notice`] always: nobody here gets a grace period.
///
/// Dropping the last typist mid-race leaves `players` empty, which the next tick
/// reads as `not self.players` and finishes the race with an EMPTY results list.
pub fn dropped(
    v: &mut RoomValley, uid: &str, who: &Value, out: &mut Out, t: f64, disconnected: bool,
) -> Left {
    let _ = (who, out, t, disconnected);
    v.type_race.drop_player(uid);
    Left::Notice
}

#[cfg(test)]
mod tests {
    use super::super::testkit::*;
    use super::*;
    use crate::realtime::MAX_TICKERS;
    use rand::SeedableRng;
    use std::time::Duration;

    fn seat(id: &str) -> (String, Value) {
        (id.to_string(), json!({"userId": id}))
    }

    fn dice() -> StdRng {
        StdRng::seed_from_u64(7)
    }

    // --------------------------------------------------------- the catalogs --

    #[test]
    fn the_constants_are_pythons() {
        // typerace.py:7-15, byte for byte.
        assert_eq!(GAME, "type");
        assert_eq!(HZ, 5.0);
        assert_eq!(COUNTDOWN, 3.0);
        assert_eq!(MAX_SECS, 180.0);
        assert_eq!(MAX_CPS, 25.0);
        assert_eq!(BURST, 12);
        assert_eq!(FINISH_GRACE, 20.0);
        assert_eq!(SEATS, 8);
        assert_eq!(MAX_ERRS, 100_000);
    }

    #[test]
    fn the_error_strings_are_pythons_byte_for_byte() {
        assert_eq!(ERR_ON, "a race is already on");
        assert_eq!(ERR_EMPTY, "join the lobby first");
        assert_eq!(ERR_HOST, "only the host can do that");
        assert_eq!(ERR_BUSY, "the Arena is busy right now: try again in a minute");
    }

    #[test]
    fn the_snippet_catalog_is_pythons_in_pythons_order() {
        assert_eq!(SNIPPETS.len(), 12);
        let langs: Vec<&str> = SNIPPETS.iter().map(|(l, _)| *l).collect();
        assert_eq!(langs, ["python", "python", "python", "javascript", "javascript", "javascript",
                           "rust", "rust", "sql", "sql", "shell", "shell"]);
        // The two with embedded quotes and the two shortest, in full -- the ones
        // a transcription slip would mangle.
        assert_eq!(SNIPPETS[1].1,
                   "with open(path, encoding=\"utf-8\") as f:\n    rows = [line.split(\",\") \
                    for line in f if line.strip()]");
        assert_eq!(SNIPPETS[6].1,
                   "fn main() {\n    let v: Vec<i32> = (1..=10).filter(|x| x % 2 == 0).collect();\
                    \n    println!(\"{:?}\", v);\n}");
        assert_eq!(SNIPPETS[10].1, "git log --oneline -n 20 | grep -i fix | wc -l");
        assert_eq!(SNIPPETS[11].1,
                   "find . -name \"*.py\" -not -path \"./.venv/*\" | xargs wc -l | sort -n \
                    | tail -5");
        // Every snippet is typeable inside MAX_SECS at a human rate, and none is
        // empty (which `acc`'s n == 0 branch would need).
        for (lang, text) in SNIPPETS {
            assert!(!text.is_empty(), "{lang} snippet is empty");
            assert!(text.chars().count() < (MAX_SECS * MAX_CPS) as usize);
        }
    }

    // ------------------------------------------------------------ the rules --

    /// A port of backend/tests/test_typerace.py::test_rules, line for line. The
    /// snippet is whichever one the draw picks (shared divergence 2), so every
    /// assertion is about `n = len(r.text)` rather than a fixed string.
    #[test]
    fn the_rules_judge_progress_and_rank_the_finishers() {
        let mut r = TypeRace::default();
        assert_eq!(r.start(&[], 0.0, &mut dice()), Some(ERR_EMPTY));
        assert_eq!(r.start(&[seat("a"), seat("b")], 0.0, &mut dice()), None);
        let n = r.text.chars().count() as i64;
        assert!(SNIPPETS.iter().any(|(l, t)| *l == r.lang && *t == r.text));
        // Still counting down: prog is refused outright.
        assert!(!r.prog("a", &json!({"pos": 3, "err": 0}), 1.0));
        assert_eq!(r.tick(3.0)[0].0, "go");
        assert!(r.prog("a", &json!({"pos": 10, "err": 1}), 3.5));
        assert!(!r.prog("a", &json!({"pos": 5, "err": 1}), 3.6)); // never backwards
        assert!(!r.prog("a", &json!({"pos": n, "err": 1}), 3.7)); // far too fast
        // A string and a bool are both rejected by `_int`.
        assert!(!r.prog("a", &json!({"pos": "20", "err": 0}), 4.0));
        assert!(!r.prog("a", &json!({"pos": true, "err": 0}), 4.0));
        let mut t = 3.5;
        let mut pos = 10;
        while pos < n {
            // A fast but human typist: two characters every 100 ms.
            t += 0.1;
            pos = n.min(pos + 2);
            assert!(r.prog("a", &json!({"pos": pos, "err": 1}), t));
        }
        assert!(r.players.get("a").unwrap().fin.is_some());
        let evs = r.tick(t + FINISH_GRACE + 1.0);
        let done = evs.iter().find(|(ev, _)| *ev == "done").unwrap().1.clone();
        let first = &done["results"][0];
        assert_eq!(first["user"]["userId"], "a");
        assert_eq!(first["place"], 1);
        assert!(first["wpm"].as_f64().unwrap() > 0.0);
        assert!(first["acc"].as_f64().unwrap() < 100.0);
        // "b" never typed a character: last, and a DNF.
        assert_eq!(done["results"][1]["dnf"], json!(true));
        assert_eq!(done["results"][1]["ms"], Value::Null);
        assert_eq!(done["lang"], json!(r.lang));
        assert_eq!(r.phase, Phase::Done);
    }

    #[test]
    fn a_second_start_while_a_race_is_on_is_refused() {
        let mut r = TypeRace::default();
        assert_eq!(r.start(&[seat("a")], 0.0, &mut dice()), None);
        assert_eq!(r.start(&[seat("a")], 0.1, &mut dice()), Some(ERR_ON));
        // A finished race is not "on", so the host may start the next one.
        r.tick(3.0);
        r.tick(3.0 + MAX_SECS);
        assert_eq!(r.phase, Phase::Done);
        assert_eq!(r.start(&[seat("a")], 200.0, &mut dice()), None);
    }

    #[test]
    fn only_the_first_eight_in_the_lobby_get_a_seat() {
        let mut r = TypeRace::default();
        let seats: Vec<(String, Value)> =
            ["a", "b", "c", "d", "e", "f", "g", "h", "i", "j"].iter().map(|u| seat(u)).collect();
        assert_eq!(r.start(&seats, 0.0, &mut dice()), None);
        assert_eq!(r.players.len(), SEATS);
        assert_eq!(r.players.keys().collect::<Vec<_>>(), ["a", "b", "c", "d", "e", "f", "g", "h"]);
        // A fresh table each race, in the new lobby's order.
        r.end();
        assert_eq!(r.start(&[seat("j"), seat("a")], 0.0, &mut dice()), None);
        assert_eq!(r.players.keys().collect::<Vec<_>>(), ["j", "a"]);
    }

    #[test]
    fn a_position_must_be_an_exact_integer_inside_the_snippet() {
        let mut r = TypeRace::default();
        r.start(&[seat("a")], 0.0, &mut dice());
        r.tick(3.0);
        let n = r.text.chars().count() as i64;
        for bad in [json!({"pos": 10.5, "err": 0}),          // not an integer
                    json!({"pos": -1, "err": 0}),            // below the floor
                    json!({"pos": n + 1, "err": 0}),         // past the snippet
                    json!({"pos": 1, "err": -1}),            // errors below zero
                    json!({"pos": 1, "err": MAX_ERRS + 1}),  // errors over the cap
                    json!({"pos": 1}),                       // no err at all
                    json!({"err": 0})] {                     // no pos at all
            assert!(!r.prog("a", &bad, 3.1), "accepted {bad}");
        }
        // 10.0 is an exact integer and passes.
        assert!(r.prog("a", &json!({"pos": 10.0, "err": 0}), 3.1));
        assert_eq!(r.players.get("a").unwrap().pos, 10);
        // Somebody who never got a seat is not a typist.
        assert!(!r.prog("ghost", &json!({"pos": 1, "err": 0}), 3.2));
    }

    #[test]
    fn a_rejected_step_still_charges_the_clock() {
        let mut r = TypeRace::default();
        r.start(&[seat("a")], 0.0, &mut dice());
        r.tick(3.0);
        let n = r.text.chars().count() as i64;
        assert!(n > 2 * BURST, "every snippet is long enough to outrun the burst");
        // Spend the whole burst: budget 12 -> 0, at = 3.0.
        assert!(r.prog("a", &json!({"pos": 12, "err": 0}), 3.0));
        assert_eq!(r.players.get("a").unwrap().budget, 0.0);
        // Refused as too fast at t = 4.0 -- and `pos` must stay inside the
        // snippet, or `_int` would reject it before the budget is ever settled.
        // The rejection still settles the budget to the BURST cap and moves
        // `at` to 4.0.
        assert!(!r.prog("a", &json!({"pos": n, "err": 0}), 4.0));
        let p = r.players.get("a").unwrap();
        assert_eq!((p.budget, p.at), (BURST as f64, 4.0));
        // Now time goes BACKWARDS, as the Python's own unit test lets it: the
        // refill is negative, the budget goes below zero, and even one character
        // is refused. This is the only way the pre-check settle is observable,
        // and `min(BURST, ..)` has no floor to stop it.
        assert!(!r.prog("a", &json!({"pos": 13, "err": 0}), 3.1));
        // 12 - 0.9 * 25, give or take the usual binary dust.
        assert!((r.players.get("a").unwrap().budget - -10.5).abs() < 1e-9);
    }

    #[test]
    fn the_error_count_only_ever_rises() {
        let mut r = TypeRace::default();
        r.start(&[seat("a")], 0.0, &mut dice());
        r.tick(3.0);
        assert!(r.prog("a", &json!({"pos": 5, "err": 9}), 3.0));
        assert!(r.prog("a", &json!({"pos": 6, "err": 0}), 3.1));
        assert_eq!(r.players.get("a").unwrap().err, 9);
    }

    #[test]
    fn a_finisher_sends_nothing_more() {
        let mut r = TypeRace::default();
        r.start(&[seat("a")], 0.0, &mut dice());
        r.tick(3.0);
        let n = r.text.chars().count() as i64;
        let mut t = 3.0;
        let mut pos = 0;
        while pos < n {
            t += 0.5;
            pos = n.min(pos + BURST);
            assert!(r.prog("a", &json!({"pos": pos, "err": 0}), t));
        }
        let fin = r.players.get("a").unwrap().fin.unwrap();
        // int((t - go_at) * 1000), truncated toward zero.
        assert_eq!(fin, py_trunc((t - 3.0) * 1000.0));
        // `p["fin"] is not None` short-circuits every later frame, so the stamp
        // cannot be rewritten by a second claim.
        assert!(!r.prog("a", &json!({"pos": n, "err": 0}), t + 1.0));
        assert_eq!(r.players.get("a").unwrap().fin, Some(fin));
    }

    // ------------------------------------------------------------- the tick --

    #[test]
    fn the_countdown_sends_the_roster_then_go() {
        let mut r = TypeRace::default();
        r.start(&[seat("a"), seat("b")], 0.0, &mut dice());
        // `start` set `dirty`, so the first tick of the countdown carries the
        // starting roster -- and `ps` is in seat order, keyed by "u".
        let evs = r.tick(0.2);
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].0, "prog");
        assert_eq!(evs[0].1, json!({"ps": [{"u": "a", "pos": 0, "fin": null},
                                           {"u": "b", "pos": 0, "fin": null}]}));
        // Nothing changed since: a quiet tick sends nothing at all.
        assert!(r.tick(0.4).is_empty());
        // "go" carries no data of its own.
        let evs = r.tick(3.0);
        assert_eq!(evs.len(), 1);
        assert_eq!((evs[0].0, evs[0].1.clone()), ("go", json!({})));
        assert_eq!(r.phase, Phase::Run);
    }

    #[test]
    fn the_last_tick_of_a_race_sends_done_and_no_prog() {
        let mut r = TypeRace::default();
        r.start(&[seat("a")], 0.0, &mut dice());
        r.tick(3.0);
        let n = r.text.chars().count() as i64;
        let mut t = 3.0;
        let mut pos = 0;
        while pos < n {
            t += 0.5;
            pos = n.min(pos + BURST);
            r.prog("a", &json!({"pos": pos, "err": 0}), t);
        }
        // Everybody finished, so the race is over on the next tick -- and the
        // "done" branch RETURNS, so the pending `prog` is never sent.
        let evs = r.tick(t);
        assert_eq!(evs.iter().map(|(ev, _)| *ev).collect::<Vec<_>>(), ["done"]);
        assert!(r.dirty, "Python's early return leaves the flag set");
        assert!(!r.running());
        // And the view now carries the same results the event did.
        assert_eq!(r.view(t)["results"], evs[0].1["results"]);
        assert_eq!(r.view(t)["phase"], json!("done"));
    }

    #[test]
    fn the_finish_grace_ends_the_race_for_everyone_else() {
        let mut r = TypeRace::default();
        r.start(&[seat("a"), seat("b")], 0.0, &mut dice());
        r.tick(3.0);
        let n = r.text.chars().count() as i64;
        let mut t = 3.0;
        let mut pos = 0;
        while pos < n {
            t += 0.5;
            pos = n.min(pos + BURST);
            r.prog("a", &json!({"pos": pos, "err": 0}), t);
        }
        assert!(r.prog("b", &json!({"pos": 3, "err": 0}), t));
        // One finisher: "b" is not done, so the race runs on...
        assert_eq!(r.tick(t + FINISH_GRACE - 0.01).iter().map(|(e, _)| *e).collect::<Vec<_>>(),
                   ["prog"]);
        // ...until the grace is up, measured from the FIRST finish, inclusively.
        let evs = r.tick(t + FINISH_GRACE);
        assert_eq!(evs[0].0, "done");
        let res = &evs[0].1["results"];
        assert_eq!(res[0]["place"], 1);
        assert_eq!(res[1]["dnf"], json!(true));
        assert_eq!(res[1]["pos"], 3);
    }

    #[test]
    fn a_race_nobody_finishes_times_out_at_max_secs() {
        let mut r = TypeRace::default();
        r.start(&[seat("a")], 0.0, &mut dice());
        r.tick(3.0);
        assert!(r.prog("a", &json!({"pos": 10, "err": 0}), 4.0));
        r.tick(4.0); // drains the dirty flag
        assert!(r.tick(3.0 + MAX_SECS - 0.01).is_empty());
        let evs = r.tick(3.0 + MAX_SECS);
        assert_eq!(evs[0].0, "done");
        let only = &evs[0].1["results"][0];
        assert_eq!(only["dnf"], json!(true));
        assert_eq!(only["ms"], Value::Null);
        // A DNF's WPM is measured over the whole elapsed race: 10 chars / 5 over
        // 180 s / 60, to one decimal.
        assert_eq!(only["wpm"], json!(py_round_to((10.0 / 5.0) / (MAX_SECS / 60.0), 1)));
        assert_eq!(only["wpm"], json!(0.7));
    }

    #[test]
    fn the_last_typist_leaving_finishes_the_race_with_no_results() {
        let mut r = TypeRace::default();
        r.start(&[seat("a")], 0.0, &mut dice());
        r.tick(3.0);
        assert!(r.drop_player("a"));
        assert!(!r.drop_player("a"));
        // `not self.players` is its own reason to finish, and the results list
        // is empty -- which `results::rows_from_done` must turn into no rows.
        let evs = r.tick(3.1);
        assert_eq!(evs[0].0, "done");
        assert_eq!(evs[0].1["results"], json!([]));
        assert!(!r.running());
    }

    #[test]
    fn the_results_rank_finishers_by_time_and_the_rest_by_distance() {
        let mut r = TypeRace::default();
        r.start(&[seat("a"), seat("b"), seat("c"), seat("d")], 0.0, &mut dice());
        r.tick(3.0);
        let n = r.text.chars().count() as i64;
        // "b" finishes first, then "a"; "d" got further than "c".
        for (uid, delay) in [("b", 0.0), ("a", 1.0)] {
            let mut t = 3.0 + delay;
            let mut pos = 0;
            while pos < n {
                t += 0.5;
                pos = n.min(pos + BURST);
                assert!(r.prog(uid, &json!({"pos": pos, "err": 0}), t));
            }
        }
        assert!(r.prog("c", &json!({"pos": 2, "err": 0}), 3.0));
        assert!(r.prog("d", &json!({"pos": 9, "err": 0}), 3.0));
        let evs = r.tick(3.0 + MAX_SECS);
        let res = &evs[0].1["results"];
        let who: Vec<&str> =
            (0..4).map(|i| res[i]["user"]["userId"].as_str().unwrap()).collect();
        assert_eq!(who, ["b", "a", "d", "c"]);
        assert_eq!((res[0]["place"].as_i64(), res[3]["place"].as_i64()), (Some(1), Some(4)));
        assert!(res[0]["ms"].as_i64().unwrap() < res[1]["ms"].as_i64().unwrap());
    }

    #[test]
    fn wpm_and_accuracy_are_pythons_numbers_to_one_decimal() {
        let mut r = TypeRace::default();
        r.start(&[seat("a")], 0.0, &mut dice());
        // A snippet of a known length, so the arithmetic is checkable: 100
        // characters typed in exactly 30 s is 40.0 WPM, and 3 mistakes over 100
        // characters is 100 * 100 / 103 = 97.1.
        r.text = "x".repeat(100);
        r.tick(3.0);
        let p = r.players.get_mut("a").unwrap();
        p.pos = 100;
        p.err = 3;
        p.fin = Some(30_000);
        r.first_at = Some(33.0);
        let evs = r.tick(33.0 + FINISH_GRACE);
        let only = &evs[0].1["results"][0];
        assert_eq!(only["wpm"], json!(40.0));
        assert_eq!(only["acc"], json!(97.1));
        assert_eq!(only["ms"], json!(30_000));
        assert_eq!(only["dnf"], json!(false));
        assert_eq!(only["pos"], json!(100));
    }

    #[test]
    fn a_finish_clocked_at_zero_takes_the_unfinished_wpm_branch() {
        // Python writes `if ms`, not `if ms is not None`, so a 0 ms finish is
        // falsy and its WPM comes from pos over elapsed time. Unreachable in a
        // real race (the countdown is 3 s) and faithfully ported anyway.
        let mut r = TypeRace::default();
        r.start(&[seat("a")], 0.0, &mut dice());
        r.text = "x".repeat(100);
        r.tick(3.0);
        let p = r.players.get_mut("a").unwrap();
        p.pos = 100;
        p.fin = Some(0);
        // 100 chars / 5 over 60 s / 60 = 20.0 WPM, not the 1/0 a real finish
        // time would have given.
        let evs = r.tick(63.0);
        assert_eq!(evs[0].1["results"][0]["wpm"], json!(20.0));
        // Still not a DNF: `dnf` is `ms is None`, which 0 is not.
        assert_eq!(evs[0].1["results"][0]["dnf"], json!(false));
    }

    #[test]
    fn an_empty_snippet_scores_full_accuracy() {
        // `round(100 * n / (n + err), 1) if n else 100.0`. Only reachable by
        // poking the state, as Python's guard is equally unreachable.
        let mut r = TypeRace::default();
        r.start(&[seat("a")], 0.0, &mut dice());
        r.text = String::new();
        r.tick(3.0);
        r.players.get_mut("a").unwrap().err = 5;
        let evs = r.tick(3.0 + MAX_SECS);
        assert_eq!(evs[0].1["results"][0]["acc"], json!(100.0));
    }

    #[test]
    fn the_view_hides_the_snippet_while_idle_and_counts_down_in_whole_ms() {
        let mut r = TypeRace::default();
        let idle = r.view(1000.0);
        assert_eq!(idle, json!({"phase": "idle", "lang": "", "text": "", "goInMs": 0,
                                "players": [], "results": null}));
        r.start(&[seat("a")], 1000.0, &mut dice());
        let v = r.view(1000.5);
        assert_eq!(v["phase"], json!("countdown"));
        assert_eq!(v["text"], json!(r.text));
        assert_eq!(v["goInMs"], json!(2500));
        assert_eq!(v["players"], json!([{"user": {"userId": "a"}, "pos": 0, "fin": null}]));
        // int() truncates toward zero before max(0, ..): 2499.9 ms is 2499.
        assert_eq!(r.view(1000.5001)["goInMs"], json!(2499));
        // Past the deadline the countdown floors at zero rather than going
        // negative, and outside the countdown it is always zero.
        assert_eq!(r.view(1099.0)["goInMs"], json!(0));
        r.tick(1003.0);
        assert_eq!(r.view(1003.0)["goInMs"], json!(0));
        // `end` leaves `lang` behind, as Python's does: an idle view still names
        // the last snippet's language.
        r.end();
        let after = r.view(1003.0);
        assert_eq!(after["phase"], json!("idle"));
        assert_eq!(after["text"], json!(""));
        assert_eq!(after["lang"], json!(r.lang));
        assert_eq!(after["players"], json!([]));
        assert_eq!(after["results"], Value::Null);
    }

    #[test]
    fn the_view_keys_are_pythons_in_pythons_order() {
        let r = TypeRace::default();
        let v = r.view(0.0);
        let keys: Vec<&str> = v.as_object().unwrap().keys().map(|k| k.as_str()).collect();
        assert_eq!(keys, ["phase", "lang", "text", "goInMs", "players", "results"]);
    }

    #[test]
    fn the_result_keys_are_pythons_in_pythons_order() {
        let mut r = TypeRace::default();
        r.start(&[seat("a")], 0.0, &mut dice());
        r.tick(3.0);
        let evs = r.tick(3.0 + MAX_SECS);
        let keys: Vec<&str> =
            evs[0].1["results"][0].as_object().unwrap().keys().map(|k| k.as_str()).collect();
        assert_eq!(keys, ["user", "place", "ms", "dnf", "pos", "wpm", "acc"]);
        let done: Vec<&str> = evs[0].1.as_object().unwrap().keys().map(|k| k.as_str()).collect();
        assert_eq!(done, ["results", "lang"]);
    }

    // ------------------------------------------------------------ the wire --

    #[tokio::test]
    async fn joining_sends_that_socket_the_race_and_nothing_else() {
        let e = env(MAX_TICKERS, 1);
        let (a, mut wa) = e.connect("r", 1, "a").await;
        e.send("r", 1, &a, GAME, "join", json!({})).await;
        let m = until(&mut wa, GAME, "type").await;
        assert_eq!(m["pv"], json!(1)); // PROTOCOL["type"]["v"]
        assert_eq!(m["race"]["phase"], json!("idle"));
        // No loop is started by a join.
        assert!(e.hub.registry().get(&super::super::tick_key(GAME, "r")).is_none());
    }

    #[tokio::test]
    async fn only_the_host_can_start_or_end() {
        let e = env(MAX_TICKERS, 1);
        let (a, _wa) = e.connect("r", 1, "a").await;
        let (b, mut wb) = e.connect("r", 2, "b").await;
        e.send("r", 1, &a, GAME, "join", json!({})).await;
        e.send("r", 2, &b, GAME, "join", json!({})).await;
        for op in ["start", "end"] {
            e.send("r", 2, &b, GAME, op, json!({})).await;
            assert_eq!(until(&mut wb, GAME, "error").await["error"], json!(ERR_HOST));
        }
    }

    #[tokio::test]
    async fn a_start_goes_only_to_the_lobby() {
        // type's audience rule: `out.lobby(ids, ..)`, not `out.all`. "b" is in
        // the ROOM but never joined the type lobby, so it sees the dispatcher's
        // shared `lobby` event and no `type` event at all.
        let e = env(MAX_TICKERS, 1);
        let (a, mut wa) = e.connect("r", 1, "a").await;
        let (_b, mut wb) = e.connect("r", 2, "b").await;
        e.send("r", 1, &a, GAME, "join", json!({})).await;
        e.send("r", 1, &a, GAME, "start", json!({})).await;
        let m = until_where(&mut wa, GAME, "type", |m| m["race"]["phase"] == "countdown").await;
        assert_eq!(m["by"]["userId"], json!("a"));
        assert!(!m["race"]["text"].as_str().unwrap().is_empty());
        assert!(SNIPPETS.iter().any(|(l, _)| json!(l) == m["race"]["lang"]));
        let seen = drain(&mut wb);
        assert!(seen.contains(&(GAME.to_string(), "lobby".to_string())), "{seen:?}");
        assert!(!seen.iter().any(|(_, ev)| ev == "type"), "{seen:?}");
    }

    #[tokio::test]
    async fn a_full_arena_says_so_to_the_host_and_rolls_the_start_back() {
        let e = env(0, 1);
        let (a, mut wa) = e.connect("busy", 1, "a").await;
        e.send("busy", 1, &a, GAME, "join", json!({})).await;
        e.send("busy", 1, &a, GAME, "start", json!({})).await;
        assert_eq!(until(&mut wa, GAME, "error").await["error"], json!(ERR_BUSY));
        // `end()` ran, so the lobby is idle rather than stuck in a countdown
        // nothing advances.
        assert_eq!(e.hub.with_room("busy", |v| v.type_race.phase), Some(Phase::Idle));
    }

    #[tokio::test]
    async fn the_loop_runs_the_countdown_and_finishes_the_race() {
        let e = env(MAX_TICKERS, 1);
        let (a, mut wa) = e.connect("r", 1, "a").await;
        e.send("r", 1, &a, GAME, "join", json!({})).await;
        e.send("r", 1, &a, GAME, "start", json!({})).await;
        // The loop's first tick carries the starting roster.
        let m = until(&mut wa, GAME, "prog").await;
        assert_eq!(m["ps"], json!([{"u": "a", "pos": 0, "fin": null}]));
        // Past the countdown, on the shared test clock.
        e.advance(COUNTDOWN);
        until(&mut wa, GAME, "go").await;
        let n = e.hub.with_room("r", |v| v.type_race.text.chars().count() as i64).unwrap();
        let mut pos = 0;
        while pos < n {
            // 0.5 s of credit refills the whole burst, so BURST characters a
            // frame is inside the cap.
            e.advance(0.5);
            pos = n.min(pos + BURST);
            e.send("r", 1, &a, GAME, "prog", json!({"pos": pos, "err": 2})).await;
        }
        let done = until(&mut wa, GAME, "done").await;
        assert_eq!(done["results"][0]["place"], json!(1));
        assert_eq!(done["results"][0]["dnf"], json!(false));
        assert!(done["results"][0]["wpm"].as_f64().unwrap() > 0.0);
        assert!(done["results"][0]["acc"].as_f64().unwrap() < 100.0);
        // The loop stops itself the tick after the race ends.
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert!(e.hub.registry().get(&super::super::tick_key(GAME, "r")).is_none());
    }

    #[tokio::test]
    async fn the_host_can_end_a_race_and_the_loop_stops() {
        let e = env(MAX_TICKERS, 1);
        let (a, mut wa) = e.connect("r", 1, "a").await;
        e.send("r", 1, &a, GAME, "join", json!({})).await;
        e.send("r", 1, &a, GAME, "start", json!({})).await;
        until(&mut wa, GAME, "prog").await;
        e.send("r", 1, &a, GAME, "end", json!({})).await;
        let m = until_where(&mut wa, GAME, "type", |m| m["race"]["phase"] == "idle").await;
        assert_eq!(m["race"]["players"], json!([]));
        assert!(m.get("by").is_none(), "only a start names who did it");
        assert!(e.hub.registry().get(&super::super::tick_key(GAME, "r")).is_none());
    }

    #[tokio::test]
    async fn a_view_answers_only_the_asker() {
        let e = env(MAX_TICKERS, 1);
        let (a, mut wa) = e.connect("r", 1, "a").await;
        let (b, mut wb) = e.connect("r", 2, "b").await;
        e.send("r", 1, &a, GAME, "join", json!({})).await;
        e.send("r", 2, &b, GAME, "join", json!({})).await;
        drain(&mut wb);
        e.send("r", 1, &a, GAME, "view", json!({})).await;
        assert_eq!(until(&mut wa, GAME, "type").await["race"]["phase"], json!("idle"));
        assert!(drain(&mut wb).is_empty());
    }

    #[tokio::test]
    async fn an_unknown_type_op_answers_nothing() {
        // Python's if/elif chain has no `else`, so only an unknown GAME is an
        // error. A nonsense op on a joined lobby is silence.
        let e = env(MAX_TICKERS, 1);
        let (a, mut wa) = e.connect("r", 1, "a").await;
        e.send("r", 1, &a, GAME, "join", json!({})).await;
        until(&mut wa, GAME, "type").await;
        drain(&mut wa);
        e.send("r", 1, &a, GAME, "shrug", json!({})).await;
        assert!(drain(&mut wa).is_empty());
    }

    #[tokio::test]
    async fn an_op_before_joining_is_refused_by_the_shared_gate() {
        let e = env(MAX_TICKERS, 1);
        let (a, mut wa) = e.connect("r", 1, "a").await;
        e.send("r", 1, &a, GAME, "start", json!({})).await;
        assert_eq!(until(&mut wa, GAME, "error").await["error"], json!("join the lobby first"));
    }

    #[tokio::test]
    async fn leaving_mid_race_sends_no_type_event_of_its_own() {
        let e = env(MAX_TICKERS, 1);
        let (a, mut wa) = e.connect("r", 1, "a").await;
        let (b, _wb) = e.connect("r", 2, "b").await;
        e.send("r", 1, &a, GAME, "join", json!({})).await;
        e.send("r", 2, &b, GAME, "join", json!({})).await;
        e.send("r", 1, &a, GAME, "start", json!({})).await;
        until(&mut wa, GAME, "prog").await;
        drain(&mut wa);
        e.send("r", 2, &b, GAME, "leave", json!({})).await;
        // Only the shared `lobby` event, with "b" named as having left.
        let m = until(&mut wa, GAME, "lobby").await;
        assert_eq!(m["left"]["userId"], json!("b"));
        assert_eq!(e.hub.with_room("r", |v| v.type_race.players.len()), Some(1));
        // And "b" is no longer in the race's roster.
        assert_eq!(e.hub.with_room("r", |v| v.type_race.players.contains_key("b")), Some(false));
    }

    #[tokio::test]
    async fn a_typist_who_left_the_lobby_still_hears_the_race_finish() {
        // `_type_tick_on`'s ids are the lobby PLUS any typist no longer in it.
        let e = env(MAX_TICKERS, 1);
        let (a, _wa) = e.connect("r", 1, "a").await;
        let (b, mut wb) = e.connect("r", 2, "b").await;
        e.send("r", 1, &a, GAME, "join", json!({})).await;
        e.send("r", 2, &b, GAME, "join", json!({})).await;
        e.send("r", 1, &a, GAME, "start", json!({})).await;
        until(&mut wb, GAME, "prog").await;
        // Take "b" out of the LOBBY without taking them out of the RACE, which
        // is what a `drop` the engine never sees looks like.
        e.hub.with_room("r", |v| v.lobbies[I_TYPE].pop("b"));
        e.advance(COUNTDOWN);
        // Still addressed: "b" is in `v.type_race.players`.
        until(&mut wb, GAME, "go").await;
    }
}
