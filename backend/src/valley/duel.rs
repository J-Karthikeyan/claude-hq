//! The creature duel: a turn-based match with timed choices.
//!
//! A port of `class Duel` plus `_team`, `_anim_allowance` and the `DUEL_*`
//! constants from `backend/app/valley.py` (:541-770), with the battle rules
//! themselves in [`super::pokebattle`]. The most stateful game in the Arena, and
//! all of its state is in one `Option<Match>` per room.
//!
//! Audience: the whole room (`out.all`) for `duel`, `turn`, `waiting`,
//! `duelend`, `away` and `back`, so spectators who never joined this lobby
//! follow the battle -- `test_duel_non_participants_cannot_act` depends on a
//! third socket seeing the match. The exceptions are `challenge`/`declined`
//! (`out.user`, to one person) and `challenged`/`late`/`duel`/`error`
//! (`out.to`, to one socket).
//!
//! THERE IS NO SERVER TICK. Golf aside, this is the only Valley game with
//! deadlines, and it has no [`super::StepFn`]: every deadline is enforced
//! LAZILY, inside [`Duel::tick`], on the next message from anyone in the match,
//! and the page sends `poke` when a countdown it is showing runs out. So a match
//! whose players both go silent simply sits there until someone pokes it, in
//! both ports.
//!
//! THE FIVE TIMERS AND WHAT EACH ONE DOES WHEN IT EXPIRES:
//!   - [`DUEL_FIRST_SECS`] / [`DUEL_CHOOSE_SECS`] / [`DUEL_REPLACE_SECS`] set
//!     `deadline`. Past it (plus [`DUEL_SLACK_SECS`]), the server CHOOSES for
//!     whoever has not, bumps their `auto` count, and resolves the turn.
//!   - [`DUEL_AFK_PICKS`] consecutive server picks for one side ends the match
//!     as a forfeit with `timeout: true`. Any real choice resets that side's
//!     count to 0.
//!   - [`DUEL_GRACE_SECS`] is written into `away[uid]` as an ABSOLUTE deadline
//!     when a duelist's last socket drops. Past it, the first `tick` ends the
//!     match as a forfeit with `left: true`.
//!   - [`DUEL_RESULT_SECS`] keeps an ending for someone who was mid-grace when
//!     it happened, replayed ONCE on their next join.
//!
//! The two hooks the shared layer asks for by name are [`Duel::seated`] (the
//! reserved lobby seat, valley.py:1017 -- the only game whose cap can be
//! exceeded) and [`Duel::back`] (the reconnect that suppresses the "joined"
//! toast, valley.py:1023). [`dropped`] is the only [`Left::Away`] in the Arena.
//!
//! Divergences from Python live beside the code that makes them; the ones every
//! engine shares are listed once in `super`'s module doc.

use super::pokebattle as pb;
use super::{py_round, py_trunc, Ctx, Left, Lobby, Out, RoomValley, Seq, I_DUEL};
use rand::Rng;
use serde_json::{json, Map, Value};

/// The game key on the wire.
pub const GAME: &str = "duel";

/// The first choice: time to read the screen.
pub const DUEL_FIRST_SECS: i64 = 60;
/// Each later move-or-switch choice; then the server picks for you.
pub const DUEL_CHOOSE_SECS: i64 = 30;
/// To send in the next creature after a faint.
pub const DUEL_REPLACE_SECS: i64 = 30;
/// A dropped connection may come back within this and play on.
pub const DUEL_GRACE_SECS: i64 = 20;
/// Consecutive server picks for one side = that side forfeits.
pub const DUEL_AFK_PICKS: i64 = 2;
/// A pick sent as a visible countdown ends still counts (network trip).
pub const DUEL_SLACK_SECS: i64 = 2;
/// Extra choosing time per animated move/switch/faint of the last turn.
pub const DUEL_ANIM_SECS: i64 = 2;
/// ...capped, so a long turn never stalls the match.
pub const DUEL_ANIM_CAP: i64 = 12;
/// An ending a dropped player missed is replayed when they rejoin.
pub const DUEL_RESULT_SECS: i64 = 600;

/// Python's `_anim_allowance`: seconds the client spends animating a turn before
/// it shows the menu again. Only `move`, `switch` and `faint` are animated; a
/// `dmg` or a `boost` rides along with the move that caused it.
pub fn anim_allowance(events: &[Value]) -> i64 {
    let n = events
        .iter()
        .filter(|e| matches!(e.get("t").and_then(Value::as_str), Some("move" | "switch" | "faint")))
        .count() as i64;
    (DUEL_ANIM_SECS * n).min(DUEL_ANIM_CAP)
}

/// Python's `_team`: 1-6 creatures as `{sp, st, br, mg, sh, name}`, or None.
/// ONE bad creature rejects the whole team -- the Python's
/// `if any(c is None for c in specs)` -- rather than silently dropping it, so a
/// client cannot sneak a short team past the 1..6 check.
pub fn team(raw: Option<&Value>) -> Option<Vec<pb::Mon>> {
    let arr = raw?.as_array()?;
    if !(1..=6).contains(&arr.len()) {
        return None;
    }
    let specs: Option<Vec<pb::Spec>> = arr.iter().map(pb::clean_spec).collect();
    Some(specs?.iter().map(pb::build_mon).collect())
}

/// Which half of a turn the match is in. Python's `mt["phase"]`, a string on the
/// wire.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Choose,
    Replace,
}

impl Phase {
    pub fn as_str(self) -> &'static str {
        match self {
            Phase::Choose => "choose",
            Phase::Replace => "replace",
        }
    }
}

/// An offered challenge: who sent it and the team they will fight with.
/// The challenger's team is built AT CHALLENGE TIME and held here, so a client
/// cannot swap it after seeing what they are up against.
pub struct Pending {
    pub from: String,
    pub team: Vec<pb::Mon>,
}

/// One live match. Python's `self.match` dict.
pub struct Match {
    /// `secrets.token_hex(4)`: eight hex characters, the page's handle on this
    /// match. Every broadcast carries it so a stale frame can be told apart.
    pub mid: String,
    /// The two duelists' STORED public profiles, captured at accept.
    pub a: Value,
    pub b: Value,
    pub ids: [String; 2],
    pub state: pb::Battle,
    pub phase: Phase,
    /// Each side's choice for this turn, or None while it is still waiting.
    /// Python's `mt["choices"]` is a dict keyed by side, and `len(choices) == 2`
    /// is "both are in"; a two-slot array says the same thing without a map.
    pub choices: [Option<pb::Act>; 2],
    /// The sides owing a replacement, ascending. Empty in the choose phase.
    pub need: Vec<usize>,
    pub deadline: f64,
    /// Consecutive server picks per side.
    pub auto: [i64; 2],
    /// user_id -> the absolute monotonic time their grace runs out. Insertion
    /// ordered because it reaches the wire as `view()["away"]`.
    pub away: Seq<f64>,
    /// Bumped on every resolved turn, so the page can drop a replay.
    pub seq: i64,
}

/// Per-room state. One field of [`RoomValley`], in Python's `__init__` order.
#[derive(Default)]
pub struct Duel {
    /// challenged user_id -> the offer. Never serialised, but kept in insertion
    /// order anyway so two offers to the same person behave as the Python's
    /// `self.pending[to] = ..` does.
    pending: Seq<Pending>,
    /// Named with a trailing underscore because `match` is a Rust keyword;
    /// Python's attribute is `self.match`.
    match_: Option<Match>,
    /// user_id -> (when, the `duelend` payload they missed).
    missed: Seq<(f64, Value)>,
}

impl Duel {
    /// Python's `Duel.view()`: the whole match as the page needs it, or None.
    /// `t` is the hoisted `now()` (divergence 1).
    pub fn view(&self, t: f64) -> Value {
        let Some(mt) = &self.match_ else { return Value::Null };
        json!({
            "mid": mt.mid,
            "a": mt.a,
            "b": mt.b,
            "ids": mt.ids,
            "turn": mt.state.turn,
            "phase": mt.phase.as_str(),
            // `max(0, round(..))` and `max(0, int(..))`: a deadline already past
            // reads as 0, never as a negative countdown. py_round is Python's
            // banker's rounding, py_trunc its `int()`.
            "deadline_in": py_round(mt.deadline - t).max(0),
            "deadline_ms": py_trunc((mt.deadline - t) * 1000.0).max(0),
            "slack_ms": DUEL_SLACK_SECS * 1000,
            "waiting": self.waiting(),
            "away": mt.away.to_object(|d| json!(py_trunc((d - t) * 1000.0).max(0))),
            "sides": sides_view(mt),
        })
    }

    /// Python's `_waiting()`: who still owes a choice. In the replace phase only
    /// the sides that owe a REPLACEMENT are waited on, which is why a healthy
    /// side cannot hold the match up.
    pub fn waiting(&self) -> Vec<String> {
        let Some(mt) = &self.match_ else { return Vec::new() };
        if mt.phase == Phase::Replace {
            mt.need.iter().filter(|i| mt.choices[**i].is_none()).map(|i| mt.ids[*i].clone()).collect()
        } else {
            (0..2).filter(|i| mt.choices[*i].is_none()).map(|i| mt.ids[i].clone()).collect()
        }
    }

    /// Python's `challenge`. The challenger's team is validated and BUILT here;
    /// only the other player's seat in this lobby is checked.
    pub fn challenge(
        &mut self, uid: &str, me: &Value, conn: u64, msg: &Value, out: &mut Out, lobby: &Lobby,
    ) {
        if self.match_.is_some() {
            out.err(conn, "a duel is already on");
            return;
        }
        // Python: `to not in lobby.members or to == m.user_id`. A non-string
        // `to` collapses to "" here, which is never a user id and so is never a
        // member key -- the same branch rejects it.
        let to = msg.get("to").and_then(Value::as_str).unwrap_or("");
        let team = team(msg.get("team")); // pure, so computing it where Python does costs nothing
        if !lobby.has(to) || to == uid {
            out.err(conn, "they need to be in the duel lobby");
            return;
        }
        let Some(team) = team else {
            out.err(conn, "bring a team of 1 to 6 creatures");
            return;
        };
        self.pending.set(to, Pending { from: uid.to_string(), team });
        out.user(to, "challenge", json!({"from": me}));
        // The target's STORED profile, not their user id: `lobby.members[to]`.
        // `has(to)` above makes this Some.
        out.to(conn, "challenged", json!({"to": lobby.profile(to)}));
    }

    /// Python's `decline`: the challenger is told, and nothing else happens.
    /// (Python takes a `lobby` here and never reads it.)
    pub fn decline(&mut self, uid: &str, me: &Value, out: &mut Out) {
        if let Some(p) = self.pending.remove(uid) {
            out.user(&p.from, "declined", json!({"by": me}));
        }
    }

    /// Python's `accept`. The offer is POPPED before anything else can fail, so
    /// a refused accept also consumes the challenge -- deliberate in the Python
    /// and pinned by a test.
    pub fn accept(
        &mut self, uid: &str, conn: u64, msg: &Value, out: &mut Out, lobby: &Lobby, t: f64,
    ) {
        if self.match_.is_some() {
            out.err(conn, "a duel is already on");
            return;
        }
        let p = self.pending.remove(uid);
        let team_b = team(msg.get("team"));
        // `not p or p["from"] not in lobby.members`: the challenger has to still
        // be here.
        let gone = match &p {
            None => true,
            Some(p) => !lobby.has(&p.from),
        };
        if gone {
            out.err(conn, "that challenge is gone");
            return;
        }
        let p = p.expect("gone covers the None case");
        let Some(team_b) = team_b else {
            out.err(conn, "bring a team of 1 to 6 creatures");
            return;
        };
        let (a, b) = (p.from, uid.to_string());
        // A fresh match clears either player's unwatched ending.
        self.missed.remove(&a);
        self.missed.remove(&b);
        // Python's `secrets.token_hex(4)`: OS entropy, NOT the per-message
        // `_rng()`, so it is deliberately not the injectable [`super::Dice`] and
        // a test reads the mid off the wire rather than predicting it.
        let mut raw = [0u8; 4];
        rand::thread_rng().fill(&mut raw);
        self.match_ = Some(Match {
            mid: hex::encode(raw),
            a: lobby.profile(&a).cloned().unwrap_or(Value::Null),
            b: lobby.profile(&b).cloned().unwrap_or(Value::Null),
            ids: [a, b],
            state: pb::new_battle(p.team, team_b),
            phase: Phase::Choose,
            choices: [None, None],
            need: Vec::new(),
            deadline: t + DUEL_FIRST_SECS as f64,
            auto: [0, 0],
            away: Seq::new(),
            seq: 0, // Python has no "seq" yet; `mt.get("seq", 0) + 1` makes the first 1
        });
        out.all("duel", json!({"duel": self.view(t)}));
    }

    /// Python's `act`: one duelist's choice for this turn.
    pub fn act(
        &mut self, uid: &str, conn: u64, msg: &Value, out: &mut Out, draw: pb::Draw, t: f64,
    ) {
        if !self.seated(uid) {
            out.err(conn, "you are not in this duel");
            return;
        }
        let was = self.match_.as_ref().expect("seated means there is a match").mid.clone();
        if self.tick(out, draw, t) {
            // Python's `if self.match is mt`: a tick can only END this match,
            // never swap it for another, so "the same object" is "still here
            // under the same mid".
            if let Some(mt) = &self.match_ {
                if mt.mid == was {
                    out.to(conn, "late", json!({"turn": mt.state.turn}));
                }
            }
            return;
        }
        let mt = self.match_.as_mut().expect("tick returned false, so the match is live");
        let side = mt.ids.iter().position(|u| u == uid).expect("seated");
        if !same_turn(msg.get("turn"), mt.state.turn) {
            // A stale or replayed choice: ignored quietly, and told the real turn.
            out.to(conn, "late", json!({"turn": mt.state.turn}));
            return;
        }
        if mt.phase == Phase::Replace && !mt.need.contains(&side) {
            out.err(conn, "wait for the other player");
            return;
        }
        let act = pb::legal(&mt.state, side, msg.get("a"));
        // In the replace phase the only legal action is a switch, even though
        // `legal` would happily hand back a move for a healthy side.
        let refused = match act {
            None => true,
            Some(a) => mt.phase == Phase::Replace && !matches!(a, pb::Act::Switch(_)),
        };
        if refused {
            out.err(conn, "you can't do that now");
            return;
        }
        let fresh = mt.choices[side].is_none();
        mt.choices[side] = act; // a resend or a changed mind overwrites until both are in
        mt.auto[side] = 0; // choosing anything at all clears the idle count
        let both = mt.choices.iter().all(Option::is_some);
        // `fresh or len(choices) == 2`: a resend that changes nothing still
        // resolves the turn once the other side is in, but does not re-announce
        // "waiting" on its own.
        if fresh || both {
            self.advance(out, draw, t);
        }
    }

    /// Python's `tick`: enforce the lazy deadlines. True if something happened
    /// (a forfeit or a forced turn), which is the caller's signal that whatever
    /// it was about to do is now stale.
    pub fn tick(&mut self, out: &mut Out, draw: pb::Draw, t: f64) -> bool {
        // A side whose grace has run out. `away.get(uid, t + 1)` means a side
        // with no grace entry is NEVER gone -- the default is in the future.
        let gone: Vec<usize> = {
            let Some(mt) = &self.match_ else { return false };
            (0..2).filter(|&i| mt.away.get(&mt.ids[i]).copied().unwrap_or(t + 1.0) < t).collect()
        };
        if !gone.is_empty() {
            let winner = (gone.len() != 2).then(|| 1 - gone[0]);
            self.end(out, winner, &[("forfeit", json!(true)), ("left", json!(true))], t);
            return true;
        }
        {
            let mt = self.match_.as_ref().expect("checked above");
            // Slack: a pick sent as the countdown ended is still in flight.
            if t <= mt.deadline + DUEL_SLACK_SECS as f64 {
                return false;
            }
        }
        let mut afk: Vec<usize> = Vec::new();
        {
            let mt = self.match_.as_mut().expect("checked above");
            let sides = if mt.phase == Phase::Replace { mt.need.clone() } else { vec![0, 1] };
            for side in sides {
                if mt.choices[side].is_none() {
                    mt.choices[side] = Some(if mt.phase == Phase::Replace {
                        // The first creature with hp left. `need` only holds
                        // sides that HAVE one, so this cannot be empty.
                        pb::Act::Switch(pb::alive(&mt.state, side)[0])
                    } else {
                        pb::auto_act(&mt.state, side)
                    });
                    mt.auto[side] += 1;
                    if mt.auto[side] >= DUEL_AFK_PICKS {
                        afk.push(side);
                    }
                }
            }
        }
        if !afk.is_empty() {
            let winner = (afk.len() != 2).then(|| 1 - afk[0]);
            self.end(out, winner, &[("forfeit", json!(true)), ("timeout", json!(true))], t);
            return true;
        }
        self.advance(out, draw, t);
        true
    }

    /// Python's `_advance`: resolve the turn (or say who is still missing), then
    /// re-arm the deadline and broadcast. Called only with a live match.
    fn advance(&mut self, out: &mut Out, draw: pb::Draw, t: f64) {
        let (replacing, incomplete, mid, turn) = {
            let mt = self.match_.as_ref().expect("advance with a live match");
            let incomplete = if mt.phase == Phase::Replace {
                mt.need.iter().any(|i| mt.choices[*i].is_none())
            } else {
                !mt.choices.iter().all(Option::is_some)
            };
            (mt.phase == Phase::Replace, incomplete, mt.mid.clone(), mt.state.turn)
        };
        if incomplete {
            let waiting = self.waiting();
            out.all("waiting", json!({"mid": mid, "turn": turn, "waiting": waiting}));
            return;
        }
        let events = {
            let mt = self.match_.as_mut().expect("advance with a live match");
            if replacing {
                // Both replacements happen in `need` order, each its own
                // `pb::replace`, and their events concatenate.
                let mut ev = Vec::new();
                for side in mt.need.clone() {
                    let to = match mt.choices[side] {
                        Some(pb::Act::Switch(to)) => to,
                        _ => panic!("a replace choice is always a switch"),
                    };
                    ev.extend(pb::replace(&mut mt.state, side, to));
                }
                ev
            } else {
                let (a, b) = (mt.choices[0].unwrap(), mt.choices[1].unwrap());
                pb::resolve_turn(&mut mt.state, a, b, draw)
            }
        };
        let (mid, seq, over, winner) = {
            let mt = self.match_.as_mut().expect("advance with a live match");
            mt.choices = [None, None];
            mt.need = pb::needs_replace(&mt.state);
            mt.phase = if mt.need.is_empty() { Phase::Choose } else { Phase::Replace };
            let secs = if mt.need.is_empty() { DUEL_CHOOSE_SECS } else { DUEL_REPLACE_SECS };
            // The animation allowance is added on top, so a long turn does not
            // eat the next choice's clock.
            mt.deadline = t + secs as f64 + anim_allowance(&events) as f64;
            mt.seq += 1;
            (mt.mid.clone(), mt.seq, mt.state.over, mt.state.winner)
        };
        let view = self.view(t);
        out.all("turn", json!({"mid": mid, "seq": seq, "events": events, "duel": view}));
        if over {
            self.end(out, winner, &[], t);
        }
    }

    /// Python's `_end`: broadcast the ending, keep it for anyone mid-grace who
    /// cannot hear it, and forget the match. `extra` is Python's `**extra`, and
    /// its keys land AFTER `duel` and `winner` in the payload.
    fn end(&mut self, out: &mut Out, winner_side: Option<usize>, extra: &[(&str, Value)], t: f64) {
        let mt = self.match_.as_ref().expect("end with a live match");
        let winner = match winner_side {
            None => Value::Null, // a double knockout has no winner
            Some(0) => mt.a.clone(),
            Some(_) => mt.b.clone(),
        };
        let view = self.view(t);
        let mut data = json!({"duel": view, "winner": winner});
        for (k, v) in extra {
            data[*k] = v.clone(); // appended in order, which preserve_order keeps
        }
        out.all("duelend", data.clone());
        // Whoever is mid-grace (socket down) cannot hear that broadcast.
        let away: Vec<String> =
            self.match_.as_ref().expect("still live").away.keys().map(str::to_string).collect();
        for uid in away {
            self.missed.set(&uid, (t, data.clone()));
        }
        self.match_ = None;
    }

    /// Python's `replay_missed`: a rejoining player whose duel ended while they
    /// were away gets that ending, ONCE. The sweep of everyone else's stale
    /// entries happens on the same call, exactly as the Python's dict
    /// comprehension does -- and AFTER this user's was popped, so their own
    /// entry is judged by the `got` test below and not by the sweep.
    pub fn replay_missed(&mut self, uid: &str, conn: u64, out: &mut Out, t: f64) {
        let got = self.missed.remove(uid);
        self.missed.retain(|_, v| t - v.0 < DUEL_RESULT_SECS as f64);
        if let Some((at, payload)) = got {
            if t - at < DUEL_RESULT_SECS as f64 {
                out.to(conn, "duelend", payload);
            }
        }
    }

    /// Python's `away`: the user's last socket dropped. True if they are
    /// mid-duel and get a grace period, which is what makes this the Arena's
    /// only [`Left::Away`]. Their outstanding challenge goes either way.
    pub fn away(&mut self, uid: &str, out: &mut Out, t: f64) -> bool {
        self.pending.remove(uid);
        let Some(mt) = self.match_.as_mut() else { return false };
        if !mt.ids.iter().any(|u| u == uid) {
            return false;
        }
        mt.away.set(uid, t + DUEL_GRACE_SECS as f64);
        let mid = mt.mid.clone();
        out.all("away", json!({"mid": mid, "user": uid, "ms": DUEL_GRACE_SECS * 1000}));
        true
    }

    /// Python's `back`: true if the user was away mid-duel, which makes their
    /// join a reconnect rather than a fresh one. Called from the shared join arm
    /// BEFORE the blip test, and it pushes its own `back` event ahead of the
    /// shared `lobby` event (valley.py:1023).
    // Same as [`Duel::seated`]: the join arm's `returning` is blip-only until
    // the integrator calls this (valley.rs:1134-1138).
    #[allow(dead_code)]
    pub fn back(&mut self, uid: &str, out: &mut Out) -> bool {
        let Some(mt) = self.match_.as_mut() else { return false };
        if mt.away.remove(uid).is_none() {
            return false;
        }
        let mid = mt.mid.clone();
        out.all("back", json!({"mid": mid, "user": uid}));
        true
    }

    /// Python's `seated`: a duelist of the live match. Their lobby seat is
    /// RESERVED while they reconnect, so the join arm lets them past the cap --
    /// the one exception to `MAX_LOBBY` in the whole dispatcher
    /// (valley.py:1017).
    // NOTHING IN THE BINARY CALLS THIS YET: `super`'s join arm still has
    // `let reserved = false;` with a note saying duel.rs lands later
    // (valley.rs:1127), and this file may not edit that one. Allowed rather
    // than deleted, because the name is the contract the integrator wires to.
    #[allow(dead_code)]
    pub fn seated(&self, uid: &str) -> bool {
        self.match_.as_ref().map(|mt| mt.ids.iter().any(|u| u == uid)).unwrap_or(false)
    }

    /// Python's `Duel.drop`: an immediate forfeit, with no grace. Named apart
    /// from `Drop::drop`. Reached by the `forfeit` op and by an explicit lobby
    /// `leave`.
    pub fn drop_user(&mut self, uid: &str, out: &mut Out, t: f64) {
        self.pending.remove(uid);
        let side = {
            let Some(mt) = &self.match_ else { return };
            match mt.ids.iter().position(|u| u == uid) {
                Some(i) => i,
                None => return,
            }
        };
        self.end(out, Some(1 - side), &[("forfeit", json!(true))], t);
    }

    /// Whether a match is on.
    // Read only by this file's tests, which reach the hub through
    // `with_room` and so cannot touch the private `match_` directly from
    // outside this module tree. Python's tests read `v.duel.match is not None`
    // the same way.
    #[allow(dead_code)]
    pub fn live(&self) -> bool {
        self.match_.is_some()
    }

    /// The live match, for a test that needs to look inside it (the Python's
    /// tests read `v.duel.match["auto"]`, which has no event to assert on).
    #[allow(dead_code)]
    pub fn current(&self) -> Option<&Match> {
        self.match_.as_ref()
    }
}

/// Python's `{uid: {"active": .., "team": [..]} for i, uid in enumerate(ids)}`:
/// keyed by USER ID, in `ids` order, so the page can look its own side up
/// without knowing which index it is.
fn sides_view(mt: &Match) -> Value {
    let mut o = Map::new();
    for (i, uid) in mt.ids.iter().enumerate() {
        let s = &mt.state.sides[i];
        o.insert(
            uid.clone(),
            json!({"active": s.active,
                   "team": s.team.iter().map(pb::view_mon).collect::<Vec<_>>()}),
        );
    }
    Value::Object(o)
}

/// Python's `msg.get("turn") != mt["state"]["turn"]`, which is a Python `==`:
/// `2.0 == 2` and `True == 1` are both true, so a float or a bool that happens
/// to equal the turn number is accepted exactly as the Python accepts it. A
/// missing or non-numeric `turn` never matches, which is how the page's first
/// frame after a reconnect gets answered "late" rather than acted on.
fn same_turn(v: Option<&Value>, turn: i64) -> bool {
    match v {
        Some(Value::Bool(b)) => i64::from(*b) == turn,
        Some(Value::Number(n)) => n.as_f64() == Some(turn as f64),
        _ => false,
    }
}

/// The join snapshot: Python's tail of the join arm for this game
/// (valley.py:1041), pushed after the shared `lobby` event. Three steps in this
/// order: enforce the deadlines the joiner may have slept through, replay an
/// ending they missed, then the match view.
pub fn joined(v: &mut RoomValley, cx: &mut Ctx, out: &mut Out) {
    let (uid, conn, t) = (cx.uid().to_string(), cx.conn, cx.t);
    let rng = &mut *cx.rng;
    let mut draw = || rng.gen::<f64>();
    v.duel.tick(out, &mut draw, t);
    v.duel.replay_missed(&uid, conn, out, t);
    out.to(conn, "duel", json!({"duel": v.duel.view(t)}));
}

/// This game's ops. Reached only past the lobby gate. An unrecognised op must
/// produce NOTHING, as Python's `else`-less if/elif chains do.
pub fn op(v: &mut RoomValley, cx: &mut Ctx, op: &str, msg: &Value, out: &mut Out) {
    let (uid, conn, t, me) = (cx.uid().to_string(), cx.conn, cx.t, cx.me());
    let rng = &mut *cx.rng;
    let mut draw = || rng.gen::<f64>();
    // Disjoint field borrows: the engine and its own lobby at once.
    let (duel, lobby) = (&mut v.duel, &v.lobbies[I_DUEL]);
    match op {
        "challenge" => duel.challenge(&uid, &me, conn, msg, out, lobby),
        "accept" => duel.accept(&uid, conn, msg, out, lobby, t),
        "act" => duel.act(&uid, conn, msg, out, &mut draw, t),
        "poke" => {
            duel.tick(out, &mut draw, t);
        }
        "sync" => {
            duel.tick(out, &mut draw, t);
            out.to(conn, "duel", json!({"duel": duel.view(t)}));
        }
        "decline" => duel.decline(&uid, &me, out),
        "forfeit" => duel.drop_user(&uid, out, t),
        _ => {}
    }
}

/// Someone left this game's lobby, by `leave` or by a dropped socket. Python's
/// duel branch of `_leave_lobby` (valley.py:1546): a DROPPED socket mid-match
/// buys a grace period, and anything else forfeits at once.
///
/// [`Left::Away`] is returned only for the grace case, and it is the only
/// `Away` in the Arena: the shared layer then suppresses the `lobby` event's
/// `left` field entirely (`left=None if away else who`, valley.py:1573), because
/// a duelist on a grace period has not left.
pub fn dropped(
    v: &mut RoomValley, uid: &str, who: &Value, out: &mut Out, t: f64, disconnected: bool,
) -> Left {
    let _ = who; // the profile goes into the shared `lobby` event, not into ours
    let away = disconnected && v.duel.away(uid, out, t);
    if !away {
        v.duel.drop_user(uid, out, t);
    }
    if away {
        Left::Away
    } else {
        Left::Notice
    }
}

#[cfg(test)]
mod tests {
    use super::super::testkit::*;
    use super::super::{leave_lobby, Out, RoomValley};
    use crate::rooms::Member;
    use super::*;
    use tokio::sync::mpsc;

    const ROOM: &str = "duel-room";

    // The creatures the Python duel tests use. Picked here for how PREDICTABLE
    // the battle is, because this hub's rng is a real `StdRng` and no Rust port
    // can be handed Python's constant-0.5 `FixedRng` through
    // [`super::super::Dice`] (see the report's "needed from shared"). So:
    //   - MAGIKARP knows only Tackle (acc 100, 3-5 damage out of 23 hp), so two
    //     of them trade blows for turns on end and the phase never moves.
    //   - CHARIZARD's Flare Blitz one-shots a MAGIKARP on every roll and every
    //     crit, so a faint is certain.
    // The battle NUMBERS are pinned against the Python in pokebattle.rs's
    // `a_whole_turn_matches_the_python_event_for_event`; these tests pin the
    // state machine around them.
    /// Charizard, level 55: Flare Blitz, Dragon Claw, Air Slash, Scary Face.
    fn zard() -> Value {
        json!({"sp": 1, "st": 4})
    }

    /// Magikarp, level 8: Tackle and nothing else.
    fn karp() -> Value {
        json!({"sp": 14, "st": 0})
    }

    /// A one-Magikarp team, the default on both sides.
    fn karps() -> Value {
        json!([karp()])
    }

    /// Every frame already queued, in order, as whole payloads. [`drain`] gives
    /// only (g, ev) pairs, and these tests pin the `left` field of an event
    /// whose POSITION in the sequence also matters, which needs both at once.
    fn frames(rx: &mut mpsc::Receiver<String>) -> Vec<Value> {
        let mut seen = Vec::new();
        while let Ok(m) = rx.try_recv() {
            seen.push(serde_json::from_str(&m).unwrap());
        }
        seen
    }

    fn evs(m: &Value, t: &str) -> Vec<Value> {
        m["events"].as_array().unwrap().iter().filter(|e| e["t"] == t).cloned().collect()
    }

    /// Both sockets joined and each has seen the other -- `both_in` in
    /// test_valley.py.
    async fn both_in(e: &Env, a: (u64, &Member), b: (u64, &Member),
                     wa: &mut mpsc::Receiver<String>, wb: &mut mpsc::Receiver<String>) {
        e.send(ROOM, a.0, a.1, GAME, "join", json!({})).await;
        e.send(ROOM, b.0, b.1, GAME, "join", json!({})).await;
        for rx in [wa, wb] {
            until_where(rx, GAME, "lobby", |m| m["members"].as_array().unwrap().len() == 2).await;
        }
    }

    /// test_valley.py's `start_duel`: challenge, accept, and the opening view.
    async fn start(
        e: &Env, a: (u64, &Member), b: (u64, &Member), wa: &mut mpsc::Receiver<String>,
        wb: &mut mpsc::Receiver<String>, team_a: Value, team_b: Value,
    ) -> Value {
        both_in(e, a, b, wa, wb).await;
        e.send(ROOM, a.0, a.1, GAME, "challenge",
               json!({"to": b.1.user_id, "team": team_a})).await;
        // the `challenge` event names the CHALLENGER, not its recipient
        assert_eq!(until(wb, GAME, "challenge").await["from"]["userId"], a.1.user_id.as_str());
        e.send(ROOM, b.0, b.1, GAME, "accept", json!({"team": team_b})).await;
        let duel = until_where(wa, GAME, "duel", |m| !m["duel"].is_null()).await["duel"].clone();
        until_where(wb, GAME, "duel", |m| !m["duel"].is_null()).await;
        duel
    }

    /// `{"type":"game","g":"duel","op":"act","turn":turn,"a":a}`.
    async fn act(e: &Env, conn: u64, m: &Member, turn: i64, a: Value) {
        e.send(ROOM, conn, m, GAME, "act", json!({"turn": turn, "a": a})).await;
    }

    // ------------------------------------------------------- the constants --

    #[test]
    fn the_constants_are_pythons() {
        // valley.py:542-550, byte for byte.
        assert_eq!(DUEL_FIRST_SECS, 60);
        assert_eq!(DUEL_CHOOSE_SECS, 30);
        assert_eq!(DUEL_REPLACE_SECS, 30);
        assert_eq!(DUEL_GRACE_SECS, 20);
        assert_eq!(DUEL_AFK_PICKS, 2);
        assert_eq!(DUEL_SLACK_SECS, 2);
        assert_eq!(DUEL_ANIM_SECS, 2);
        assert_eq!(DUEL_ANIM_CAP, 12);
        assert_eq!(DUEL_RESULT_SECS, 600);
        assert_eq!(GAME, "duel");
        assert_eq!(Phase::Choose.as_str(), "choose");
        assert_eq!(Phase::Replace.as_str(), "replace");
    }

    #[test]
    fn the_animation_allowance_is_two_seconds_an_animated_event_capped_at_twelve() {
        let ev = |ts: &[&str]| -> Vec<Value> { ts.iter().map(|t| json!({"t": t})).collect() };
        assert_eq!(anim_allowance(&[]), 0);
        assert_eq!(anim_allowance(&ev(&["move"])), 2);
        assert_eq!(anim_allowance(&ev(&["move", "switch", "faint"])), 6);
        // dmg, boost, residual, status, end and the rest ride along for free.
        assert_eq!(anim_allowance(&ev(&["move", "dmg", "residual", "boost", "end"])), 2);
        assert_eq!(anim_allowance(&ev(&["move"; 6])), 12);
        assert_eq!(anim_allowance(&ev(&["move"; 99])), 12); // the cap holds
        assert_eq!(anim_allowance(&[json!({"x": 1}), json!("move")]), 0); // not an event shape
    }

    #[test]
    fn a_team_is_one_to_six_creatures_and_each_one_has_to_be_real() {
        // test_duel_team_validation_mega_and_branch's first three lines.
        assert!(team(Some(&json!([]))).is_none());
        assert!(team(Some(&json!([zard(), zard(), zard(), zard(), zard(),
                                  zard(), zard()]))).is_none());
        assert!(team(Some(&json!([{"sp": true, "st": 4}]))).is_none());
        assert!(team(Some(&json!([{"sp": 1, "st": 4.0}]))).is_none());
        assert!(team(Some(&json!([{"sp": 48, "st": 0}]))).is_none());
        assert!(team(Some(&json!({"sp": 1, "st": 4}))).is_none()); // not a list
        assert!(team(None).is_none());
        // ONE bad creature rejects the whole team, rather than being dropped.
        assert!(team(Some(&json!([zard(), {"sp": 999, "st": 0}]))).is_none());
        let t = team(Some(&json!([zard(), karp()]))).unwrap();
        assert_eq!(t.len(), 2);
        assert_eq!((t[0].name.as_str(), t[1].name.as_str()), ("Charizard", "Magikarp"));
        assert_eq!(team(Some(&json!([zard(), zard(), zard(), zard(), zard(), zard()]))).unwrap().len(), 6);
    }

    #[test]
    fn a_turn_number_is_compared_the_way_python_compares_it() {
        assert!(same_turn(Some(&json!(2)), 2));
        assert!(same_turn(Some(&json!(2.0)), 2)); // Python: 2.0 == 2
        assert!(same_turn(Some(&json!(true)), 1)); // Python: True == 1
        assert!(!same_turn(Some(&json!(3)), 2));
        assert!(!same_turn(Some(&json!("2")), 2));
        assert!(!same_turn(None, 2));
        assert!(!same_turn(Some(&Value::Null), 2));
    }

    // -------------------------------------------- challenge, accept, decline --

    #[tokio::test]
    async fn a_challenge_needs_someone_else_who_is_in_the_duel_lobby() {
        let e = env(4, 1);
        let (a, mut wa) = e.connect(ROOM, 1, "a").await;
        let (b, mut wb) = e.connect(ROOM, 2, "b").await;
        both_in(&e, (1, &a), (2, &b), &mut wa, &mut wb).await;
        for to in [json!("nobody"), json!("a"), json!(7), Value::Null] {
            e.send(ROOM, 1, &a, GAME, "challenge", json!({"to": to, "team": karps()})).await;
            assert_eq!(until(&mut wa, GAME, "error").await["error"],
                       "they need to be in the duel lobby");
        }
        // The seat check comes BEFORE the team check, even with a bad team.
        e.send(ROOM, 1, &a, GAME, "challenge", json!({"to": "a", "team": json!([])})).await;
        assert_eq!(until(&mut wa, GAME, "error").await["error"],
                   "they need to be in the duel lobby");
    }

    #[tokio::test]
    async fn a_challenge_needs_a_team_of_one_to_six_creatures() {
        let e = env(4, 1);
        let (a, mut wa) = e.connect(ROOM, 1, "a").await;
        let (b, mut wb) = e.connect(ROOM, 2, "b").await;
        both_in(&e, (1, &a), (2, &b), &mut wa, &mut wb).await;
        for t in [json!([]), json!([zard(), zard(), zard(), zard(), zard(), zard(), zard()]), json!("charizard"), Value::Null] {
            e.send(ROOM, 1, &a, GAME, "challenge", json!({"to": "b", "team": t})).await;
            assert_eq!(until(&mut wa, GAME, "error").await["error"],
                       "bring a team of 1 to 6 creatures");
        }
        // ... and so does an accept.
        e.send(ROOM, 1, &a, GAME, "challenge", json!({"to": "b", "team": karps()})).await;
        until(&mut wb, GAME, "challenge").await;
        e.send(ROOM, 2, &b, GAME, "accept", json!({"team": json!([])})).await;
        assert_eq!(until(&mut wb, GAME, "error").await["error"],
                   "bring a team of 1 to 6 creatures");
        // The offer was POPPED on the way in, so retrying is now too late.
        e.send(ROOM, 2, &b, GAME, "accept", json!({"team": karps()})).await;
        assert_eq!(until(&mut wb, GAME, "error").await["error"], "that challenge is gone");
    }

    #[tokio::test]
    async fn a_challenge_tells_the_target_and_echoes_their_profile_to_the_sender() {
        let e = env(4, 1);
        let (a, mut wa) = e.connect(ROOM, 1, "a").await;
        let (b, mut wb) = e.connect(ROOM, 2, "b").await;
        let (_c, mut wc) = e.connect(ROOM, 3, "c").await;
        both_in(&e, (1, &a), (2, &b), &mut wa, &mut wb).await;
        let _ = frames(&mut wc);
        e.send(ROOM, 1, &a, GAME, "challenge", json!({"to": "b", "team": karps()})).await;
        // `challenge` goes to ONE person and `challenged` to ONE socket, which
        // is why the bystander hears nothing at all.
        assert_eq!(until(&mut wb, GAME, "challenge").await["from"]["userId"], "a");
        assert_eq!(until(&mut wa, GAME, "challenged").await["to"]["userId"], "b");
        assert!(frames(&mut wc).is_empty());
    }

    #[tokio::test]
    async fn declining_tells_the_challenger_and_the_offer_is_gone() {
        // test_duel_decline_tells_the_challenger.
        let e = env(4, 1);
        let (a, mut wa) = e.connect(ROOM, 1, "a").await;
        let (b, mut wb) = e.connect(ROOM, 2, "b").await;
        both_in(&e, (1, &a), (2, &b), &mut wa, &mut wb).await;
        e.send(ROOM, 1, &a, GAME, "challenge", json!({"to": "b", "team": karps()})).await;
        until(&mut wb, GAME, "challenge").await;
        e.send(ROOM, 2, &b, GAME, "decline", json!({})).await;
        assert_eq!(until(&mut wa, GAME, "declined").await["by"]["userId"], "b");
        e.send(ROOM, 2, &b, GAME, "accept", json!({"team": karps()})).await;
        assert_eq!(until(&mut wb, GAME, "error").await["error"], "that challenge is gone");
        // A decline with nothing pending says nothing at all.
        let _ = frames(&mut wa);
        e.send(ROOM, 2, &b, GAME, "decline", json!({})).await;
        assert!(frames(&mut wa).is_empty());
    }

    #[tokio::test]
    async fn an_accepted_challenge_opens_the_match_to_the_whole_room() {
        let e = env(4, 1);
        let (a, mut wa) = e.connect(ROOM, 1, "a").await;
        let (b, mut wb) = e.connect(ROOM, 2, "b").await;
        let (_c, mut wc) = e.connect(ROOM, 3, "c").await; // never joined the duel lobby
        let duel = start(&e, (1, &a), (2, &b), &mut wa, &mut wb, karps(), karps()).await;
        // ... and the spectator who never joined gets it too: `out.all`.
        let seen = until_where(&mut wc, GAME, "duel", |m| !m["duel"].is_null()).await;
        assert_eq!(seen["duel"]["mid"], duel["mid"]);
        assert_eq!(duel["mid"].as_str().unwrap().len(), 8); // token_hex(4)
        assert_eq!(duel["ids"], json!(["a", "b"]));
        assert_eq!(duel["a"]["userId"], "a");
        assert_eq!(duel["b"]["userId"], "b");
        assert_eq!(duel["turn"], 1);
        assert_eq!(duel["phase"], "choose");
        assert_eq!(duel["deadline_in"], DUEL_FIRST_SECS);
        assert_eq!(duel["deadline_ms"], DUEL_FIRST_SECS * 1000);
        assert_eq!(duel["slack_ms"], DUEL_SLACK_SECS * 1000);
        assert_eq!(duel["waiting"], json!(["a", "b"]));
        assert_eq!(duel["away"], json!({}));
        // The view's own key order, which the page reads.
        assert_eq!(duel.as_object().unwrap().keys().map(String::as_str).collect::<Vec<_>>(),
                   vec!["mid", "a", "b", "ids", "turn", "phase", "deadline_in", "deadline_ms",
                        "slack_ms", "waiting", "away", "sides"]);
        // `sides` is keyed by user id, in ids order.
        assert_eq!(duel["sides"].as_object().unwrap().keys().map(String::as_str)
                       .collect::<Vec<_>>(), vec!["a", "b"]);
        assert_eq!(duel["sides"]["a"]["active"], 0);
        assert_eq!(duel["sides"]["a"]["team"][0]["name"], "Magikarp");
    }

    #[tokio::test]
    async fn a_second_challenge_or_accept_during_a_match_is_refused() {
        let e = env(4, 1);
        let (a, mut wa) = e.connect(ROOM, 1, "a").await;
        let (b, mut wb) = e.connect(ROOM, 2, "b").await;
        start(&e, (1, &a), (2, &b), &mut wa, &mut wb, karps(), karps()).await;
        e.send(ROOM, 1, &a, GAME, "challenge", json!({"to": "b", "team": karps()})).await;
        assert_eq!(until(&mut wa, GAME, "error").await["error"], "a duel is already on");
        e.send(ROOM, 2, &b, GAME, "accept", json!({"team": karps()})).await;
        assert_eq!(until(&mut wb, GAME, "error").await["error"], "a duel is already on");
    }

    // ---------------------------------------------------------- acting out --

    #[tokio::test]
    async fn only_the_two_duelists_may_act_and_a_spectator_still_sees_the_match() {
        // test_duel_non_participants_cannot_act.
        let e = env(4, 1);
        let (a, mut wa) = e.connect(ROOM, 1, "a").await;
        let (b, mut wb) = e.connect(ROOM, 2, "b").await;
        let (c, mut wc) = e.connect(ROOM, 3, "c").await;
        let duel = start(&e, (1, &a), (2, &b), &mut wa, &mut wb, karps(), karps()).await;
        e.send(ROOM, 3, &c, GAME, "join", json!({})).await;
        let seen = until_where(&mut wc, GAME, "duel", |m| !m["duel"].is_null()).await;
        assert_eq!(seen["duel"]["mid"], duel["mid"]);
        act(&e, 3, &c, 1, json!({"k": "move", "i": 0})).await;
        assert_eq!(until(&mut wc, GAME, "error").await["error"], "you are not in this duel");
        // ... and their stray choice did not count: the turn is still waiting on
        // both duelists.
        e.send(ROOM, 1, &a, GAME, "sync", json!({})).await;
        assert_eq!(until(&mut wa, GAME, "duel").await["duel"]["waiting"], json!(["a", "b"]));
    }

    #[tokio::test]
    async fn nothing_resolves_on_one_choice_and_a_stale_turn_is_answered_late() {
        let e = env(4, 1);
        let (a, mut wa) = e.connect(ROOM, 1, "a").await;
        let (b, mut wb) = e.connect(ROOM, 2, "b").await;
        start(&e, (1, &a), (2, &b), &mut wa, &mut wb, karps(), karps()).await;
        act(&e, 1, &a, 1, json!({"k": "move", "i": 0})).await;
        assert_eq!(until(&mut wb, GAME, "waiting").await["waiting"], json!(["b"]));
        act(&e, 2, &b, 1, json!({"k": "move", "i": 0})).await;
        let turn = until(&mut wa, GAME, "turn").await;
        assert_eq!(turn["seq"], 1);
        assert_eq!(turn["duel"]["turn"], 2);
        assert_eq!(turn["duel"]["phase"], "choose");
        assert_eq!(turn["duel"]["waiting"], json!(["a", "b"]));
        assert_eq!(evs(&turn, "move").len(), 2);
        // The choosing clock is re-armed to CHOOSE + the animation allowance.
        let want = DUEL_CHOOSE_SECS + anim_allowance(turn["events"].as_array().unwrap());
        assert_eq!(turn["duel"]["deadline_in"], want);
        assert!(want > DUEL_CHOOSE_SECS && want <= DUEL_CHOOSE_SECS + DUEL_ANIM_CAP);
        // A choice for a turn that has already resolved is told the real turn.
        act(&e, 1, &a, 1, json!({"k": "move", "i": 0})).await;
        assert_eq!(until(&mut wa, GAME, "late").await["turn"], 2);
        // ... and so is one with no turn at all.
        e.send(ROOM, 1, &a, GAME, "act", json!({"a": {"k": "move", "i": 0}})).await;
        assert_eq!(until(&mut wa, GAME, "late").await["turn"], 2);
    }

    #[tokio::test]
    async fn an_illegal_choice_is_refused_and_pp_only_comes_off_a_real_one() {
        let e = env(4, 1);
        let (a, mut wa) = e.connect(ROOM, 1, "a").await;
        let (b, mut wb) = e.connect(ROOM, 2, "b").await;
        start(&e, (1, &a), (2, &b), &mut wa, &mut wb, karps(), karps()).await;
        // Magikarp knows exactly one move, so slot 1 does not exist.
        for bad in [json!({"k": "move", "i": 1}), json!({"k": "move", "i": -1}),
                    json!({"k": "switch", "to": 0}), json!({"k": "nope"}), json!("move"),
                    Value::Null] {
            act(&e, 1, &a, 1, bad).await;
            assert_eq!(until(&mut wa, GAME, "error").await["error"], "you can't do that now");
        }
        // No PP went anywhere and nobody is waiting on a phantom choice.
        e.send(ROOM, 1, &a, GAME, "sync", json!({})).await;
        let d = until(&mut wa, GAME, "duel").await["duel"].clone();
        assert_eq!(d["sides"]["a"]["team"][0]["moves"][0]["pp"], 35);
        assert_eq!(d["waiting"], json!(["a", "b"]));
    }

    #[tokio::test]
    async fn changing_your_mind_overwrites_until_both_are_in() {
        // test_duel_changing_your_mind_overwrites_until_both_are_in.
        let e = env(4, 1);
        let (a, mut wa) = e.connect(ROOM, 1, "a").await;
        let (b, mut wb) = e.connect(ROOM, 2, "b").await;
        start(&e, (1, &a), (2, &b), &mut wa, &mut wb, json!([zard()]), karps()).await;
        act(&e, 1, &a, 1, json!({"k": "move", "i": 0})).await; // Flare Blitz
        until(&mut wb, GAME, "waiting").await;
        act(&e, 1, &a, 1, json!({"k": "move", "i": 0})).await; // a resend is idempotent
        act(&e, 1, &a, 1, json!({"k": "move", "i": 3})).await; // Scary Face instead
        act(&e, 2, &b, 1, json!({"k": "move", "i": 0})).await;
        let turn = until(&mut wa, GAME, "turn").await;
        let mine: Vec<_> = evs(&turn, "move").iter().filter(|m| m["side"] == 0)
            .map(|m| m["move"].as_str().unwrap().to_string()).collect();
        assert_eq!(mine, vec!["scaryface"]);
        // Scary Face does no damage, so the Magikarp is still standing.
        assert_eq!(turn["duel"]["phase"], "choose");
    }

    #[tokio::test]
    async fn a_faint_opens_the_replace_phase_for_that_side_only_then_the_win() {
        // test_duel_replace_phase_then_win.
        let e = env(4, 1);
        let (a, mut wa) = e.connect(ROOM, 1, "a").await;
        let (b, mut wb) = e.connect(ROOM, 2, "b").await;
        start(&e, (1, &a), (2, &b), &mut wa, &mut wb,
              json!([zard()]), json!([karp(), karp()])).await;
        act(&e, 1, &a, 1, json!({"k": "move", "i": 0})).await;
        act(&e, 2, &b, 1, json!({"k": "move", "i": 0})).await;
        let turn = until(&mut wb, GAME, "turn").await;
        assert!(evs(&turn, "faint").iter().any(|f| f["side"] == 1));
        let view = turn["duel"].clone();
        assert_eq!(view["phase"], "replace");
        assert_eq!(view["waiting"], json!(["b"]));
        assert_eq!(view["deadline_in"],
                   DUEL_REPLACE_SECS + anim_allowance(turn["events"].as_array().unwrap()));
        let t2 = view["turn"].as_i64().unwrap();
        // The healthy side may not act at all while a replacement is owed.
        act(&e, 1, &a, t2, json!({"k": "move", "i": 0})).await;
        assert_eq!(until(&mut wa, GAME, "error").await["error"], "wait for the other player");
        // ... and the fainted side may only switch.
        act(&e, 2, &b, t2, json!({"k": "move", "i": 0})).await;
        assert_eq!(until(&mut wb, GAME, "error").await["error"], "you can't do that now");
        act(&e, 2, &b, t2, json!({"k": "switch", "to": 1})).await;
        let swap = until(&mut wa, GAME, "turn").await;
        assert_eq!(swap["events"],
                   json!([{"t": "switch", "side": 1, "slot": 1, "name": "Magikarp"}]));
        assert_eq!(swap["duel"]["phase"], "choose");
        assert_eq!(swap["duel"]["turn"], t2); // a replacement is not a new turn
        // The second Magikarp goes the same way, and that is the match.
        act(&e, 1, &a, t2, json!({"k": "move", "i": 0})).await;
        act(&e, 2, &b, t2, json!({"k": "move", "i": 0})).await;
        let end = until(&mut wa, GAME, "duelend").await;
        assert_eq!(end["winner"]["userId"], "a");
        assert!(end.get("forfeit").is_none()); // won, not forfeited
        assert_eq!(end.as_object().unwrap().keys().map(String::as_str).collect::<Vec<_>>(),
                   vec!["type", "g", "ev", "pv", "duel", "winner"]);
        assert!(!e.hub.with_room(ROOM, |v| v.duel.live()).unwrap());
    }

    // ---------------------------------------------------- deadlines and afk --

    #[tokio::test]
    async fn past_the_deadline_the_server_picks_for_whoever_is_missing() {
        // test_duel_timeout_picks_for_you_and_forfeit's first half.
        let e = env(4, 1);
        let (a, mut wa) = e.connect(ROOM, 1, "a").await;
        let (b, mut wb) = e.connect(ROOM, 2, "b").await;
        start(&e, (1, &a), (2, &b), &mut wa, &mut wb, karps(), karps()).await;
        act(&e, 1, &a, 1, json!({"k": "move", "i": 0})).await;
        until(&mut wa, GAME, "waiting").await; // processed before the clock moves
        e.advance((DUEL_FIRST_SECS + DUEL_SLACK_SECS + 1) as f64);
        e.send(ROOM, 1, &a, GAME, "poke", json!({})).await;
        let turn = until(&mut wb, GAME, "turn").await;
        // b idled once, so the server used its first move with PP for them.
        let theirs: Vec<_> = evs(&turn, "move").iter().filter(|m| m["side"] == 1)
            .map(|m| m["move"].as_str().unwrap().to_string()).collect();
        assert_eq!(theirs, vec!["tackle"]);
        assert_eq!(e.hub.with_room(ROOM, |v| v.duel.current().unwrap().auto).unwrap(), [0, 1]);
    }

    #[tokio::test]
    async fn two_server_picks_in_a_row_forfeit_that_side() {
        // test_duel_two_server_picks_in_a_row_forfeit.
        let e = env(4, 1);
        let (a, mut wa) = e.connect(ROOM, 1, "a").await;
        let (b, mut wb) = e.connect(ROOM, 2, "b").await;
        start(&e, (1, &a), (2, &b), &mut wa, &mut wb, karps(), karps()).await;
        act(&e, 1, &a, 1, json!({"k": "move", "i": 0})).await;
        until(&mut wa, GAME, "waiting").await;
        e.advance((DUEL_FIRST_SECS + DUEL_SLACK_SECS + 1) as f64);
        e.send(ROOM, 1, &a, GAME, "poke", json!({})).await;
        let view = until(&mut wa, GAME, "turn").await["duel"].clone();
        let t2 = view["turn"].as_i64().unwrap();
        act(&e, 1, &a, t2, json!({"k": "move", "i": 0})).await;
        until_where(&mut wa, GAME, "waiting", |m| m["turn"] == t2).await;
        e.advance(view["deadline_in"].as_f64().unwrap() + (DUEL_SLACK_SECS + 1) as f64);
        e.send(ROOM, 1, &a, GAME, "poke", json!({})).await;
        let end = until(&mut wa, GAME, "duelend").await;
        assert_eq!(end["winner"]["userId"], "a");
        assert_eq!(end["timeout"], true);
        assert_eq!(end["forfeit"], true);
        // `**extra` lands after duel and winner, in the order the Python passes it.
        assert_eq!(end.as_object().unwrap().keys().map(String::as_str).collect::<Vec<_>>(),
                   vec!["type", "g", "ev", "pv", "duel", "winner", "forfeit", "timeout"]);
        // The ending carries the match it is ending, not null.
        assert!(!end["duel"].is_null());
        assert!(!e.hub.with_room(ROOM, |v| v.duel.live()).unwrap());
        let _ = &mut wb;
    }

    #[tokio::test]
    async fn choosing_anything_resets_that_sides_idle_count() {
        // test_duel_choosing_resets_the_idle_count.
        let e = env(4, 1);
        let (a, mut wa) = e.connect(ROOM, 1, "a").await;
        let (b, mut wb) = e.connect(ROOM, 2, "b").await;
        start(&e, (1, &a), (2, &b), &mut wa, &mut wb, karps(), karps()).await;
        act(&e, 1, &a, 1, json!({"k": "move", "i": 0})).await;
        until(&mut wa, GAME, "waiting").await;
        e.advance((DUEL_FIRST_SECS + DUEL_SLACK_SECS + 1) as f64);
        e.send(ROOM, 1, &a, GAME, "poke", json!({})).await;
        let view = until(&mut wb, GAME, "turn").await["duel"].clone();
        let t2 = view["turn"].as_i64().unwrap();
        act(&e, 2, &b, t2, json!({"k": "move", "i": 0})).await; // b is back
        act(&e, 1, &a, t2, json!({"k": "move", "i": 0})).await;
        let view = until_where(&mut wb, GAME, "turn", |m| m["duel"]["turn"] == t2 + 1).await
            ["duel"].clone();
        assert_eq!(e.hub.with_room(ROOM, |v| v.duel.current().unwrap().auto).unwrap(), [0, 0]);
        // So the NEXT server pick is b's first again, and nobody forfeits.
        let t3 = view["turn"].as_i64().unwrap();
        act(&e, 1, &a, t3, json!({"k": "move", "i": 0})).await;
        until_where(&mut wa, GAME, "waiting", |m| m["turn"] == t3).await;
        e.advance(view["deadline_in"].as_f64().unwrap() + (DUEL_SLACK_SECS + 1) as f64);
        e.send(ROOM, 1, &a, GAME, "poke", json!({})).await;
        until_where(&mut wa, GAME, "turn", |m| m["duel"]["turn"] == t3 + 1).await;
        assert!(e.hub.with_room(ROOM, |v| v.duel.live()).unwrap());
    }

    #[tokio::test]
    async fn a_pick_sent_inside_the_slack_is_still_yours() {
        // test_duel_a_pick_just_after_the_deadline_is_still_yours.
        let e = env(4, 1);
        let (a, mut wa) = e.connect(ROOM, 1, "a").await;
        let (b, mut wb) = e.connect(ROOM, 2, "b").await;
        let duel = start(&e, (1, &a), (2, &b), &mut wa, &mut wb, karps(), karps()).await;
        assert_eq!(duel["slack_ms"], DUEL_SLACK_SECS * 1000);
        act(&e, 1, &a, 1, json!({"k": "move", "i": 0})).await;
        until(&mut wb, GAME, "waiting").await;
        // In flight as b's countdown ended: one second past, inside the slack.
        e.advance((DUEL_FIRST_SECS + 1) as f64);
        act(&e, 2, &b, 1, json!({"k": "move", "i": 0})).await;
        let turn = until(&mut wa, GAME, "turn").await;
        assert_eq!(evs(&turn, "move").len(), 2);
        // Nobody was picked for: b's own choice counted.
        assert_eq!(e.hub.with_room(ROOM, |v| v.duel.current().unwrap().auto).unwrap(), [0, 0]);
        // The deadline is a `<=`, so exactly at deadline + slack is still fine.
        let t2 = turn["duel"]["turn"].as_i64().unwrap();
        act(&e, 1, &a, t2, json!({"k": "move", "i": 0})).await;
        until_where(&mut wa, GAME, "waiting", |m| m["turn"] == t2).await;
        e.advance(turn["duel"]["deadline_in"].as_f64().unwrap() + DUEL_SLACK_SECS as f64);
        act(&e, 2, &b, t2, json!({"k": "move", "i": 0})).await;
        until_where(&mut wa, GAME, "turn", |m| m["duel"]["turn"] == t2 + 1).await;
        assert_eq!(e.hub.with_room(ROOM, |v| v.duel.current().unwrap().auto).unwrap(), [0, 0]);
    }

    #[tokio::test]
    async fn a_countdown_already_past_reads_as_zero_and_never_as_a_negative() {
        let e = env(4, 1);
        let (a, mut wa) = e.connect(ROOM, 1, "a").await;
        let (b, mut wb) = e.connect(ROOM, 2, "b").await;
        start(&e, (1, &a), (2, &b), &mut wa, &mut wb, karps(), karps()).await;
        e.advance((DUEL_FIRST_SECS + 100) as f64);
        // `sync` ticks first, and the tick picks for BOTH sides -- one idle pick
        // each, which is under the forfeit threshold -- then re-arms the clock.
        e.send(ROOM, 1, &a, GAME, "sync", json!({})).await;
        let d = until(&mut wa, GAME, "duel").await["duel"].clone();
        assert_eq!(d["turn"], 2);
        assert_eq!(e.hub.with_room(ROOM, |v| v.duel.current().unwrap().auto).unwrap(), [1, 1]);
        // Pin the clamp itself on a deadline deliberately in the past.
        e.hub.with_room(ROOM, |v| {
            v.duel.match_.as_mut().unwrap().deadline = e.hub.now() - 7.5;
        });
        let view = e.hub.with_room(ROOM, |v| v.duel.view(e.hub.now())).unwrap();
        assert_eq!(view["deadline_in"], 0);
        assert_eq!(view["deadline_ms"], 0);
    }

    // ------------------------------------------- dropping, grace, reconnect --

    #[tokio::test]
    async fn a_dropped_duelist_is_away_not_left_and_comes_back_mid_battle() {
        // test_duel_survives_a_dropped_connection_and_resumes.
        let e = env(4, 1);
        let (a, mut wa) = e.connect(ROOM, 1, "a").await;
        let (b, mut wb) = e.connect(ROOM, 2, "b").await;
        let duel = start(&e, (1, &a), (2, &b), &mut wa, &mut wb, karps(), karps()).await;
        act(&e, 1, &a, 1, json!({"k": "move", "i": 0})).await;
        act(&e, 2, &b, 1, json!({"k": "move", "i": 0})).await;
        let hp = until(&mut wb, GAME, "turn").await["duel"]["sides"]["b"]["team"][0]["hp"].clone();
        let _ = frames(&mut wa);
        e.disconnect(ROOM, 2, &b).await;
        // `away` goes to the whole room and THEN the shared `lobby` event, whose
        // `left` is suppressed: a duelist on a grace period has not left.
        let seen = frames(&mut wa);
        let order: Vec<_> = seen.iter().map(|m| m["ev"].as_str().unwrap()).collect();
        assert_eq!(order, vec!["away", "lobby"]);
        assert_eq!(seen[0]["user"], "b");
        assert_eq!(seen[0]["ms"], DUEL_GRACE_SECS * 1000);
        assert_eq!(seen[0]["mid"], duel["mid"]);
        assert_eq!(seen[1]["left"], Value::Null);
        assert!(seen[1].as_object().unwrap().contains_key("left")); // null, not omitted
        assert!(e.hub.with_room(ROOM, |v| v.duel.live()).unwrap());
        // The seat is reserved: `seated` is what the join arm asks past the cap.
        assert!(e.hub.with_room(ROOM, |v| v.duel.seated("b")).unwrap());

        // A page refresh inside the grace resumes the battle.
        e.advance((DUEL_GRACE_SECS - 1) as f64);
        let (b2, mut wb2) = e.connect(ROOM, 3, "b").await;
        e.send(ROOM, 3, &b2, GAME, "join", json!({})).await;
        // The join itself cancels the hold: the shared arm runs
        // `v.duel.back(uid, out)` before it consults the blip
        // (valley.py:1026), so by the time anyone sees a frame, b is back.
        let back = until(&mut wa, GAME, "back").await;
        assert_eq!(back["user"], "b");
        assert_eq!(back["mid"], duel["mid"]);
        let view = until_where(&mut wb2, GAME, "duel", |m| !m["duel"].is_null()).await["duel"]
            .clone();
        assert_eq!(view["mid"], duel["mid"]);
        assert_eq!(view["turn"], 2);
        assert_eq!(view["sides"]["b"]["team"][0]["hp"], hp);
        // Nobody is away any more, so the match carries on.
        assert_eq!(view["away"], json!({}));
        assert!(!e.hub.with_room(ROOM, |v| v.duel.seated("b")).unwrap_or(true)
                || e.hub.with_room(ROOM, |v| v.duel.live()).unwrap());
        let mut out = Out::new(GAME);
        // Asked a second time it is false and silent: the hold is already gone.
        assert!(!e.hub.with_room(ROOM, |v| v.duel.back("b", &mut out)).unwrap());
        assert!(out.items.is_empty());
        act(&e, 1, &a, 2, json!({"k": "move", "i": 0})).await;
        act(&e, 3, &b2, 2, json!({"k": "move", "i": 0})).await;
        assert_eq!(until(&mut wb2, GAME, "turn").await["duel"]["turn"], 3);
    }

    #[tokio::test]
    async fn back_is_false_for_anyone_who_was_not_away() {
        let e = env(4, 1);
        let (a, mut wa) = e.connect(ROOM, 1, "a").await;
        let (b, mut wb) = e.connect(ROOM, 2, "b").await;
        // No match at all: nothing to come back to, and no event either.
        let mut out = Out::new(GAME);
        let mut v = RoomValley::new(ROOM);
        assert!(!v.duel.back("a", &mut out));
        assert!(out.items.is_empty());
        // A live match, but nobody is on a grace period.
        start(&e, (1, &a), (2, &b), &mut wa, &mut wb, karps(), karps()).await;
        assert!(!e.hub.with_room(ROOM, |v| v.duel.back("a", &mut out)).unwrap());
        assert!(!e.hub.with_room(ROOM, |v| v.duel.back("nobody", &mut out)).unwrap());
        assert!(out.items.is_empty());
        assert!(!e.hub.with_room(ROOM, |v| v.duel.seated("nobody")).unwrap());
    }

    #[tokio::test]
    async fn a_duelist_who_never_comes_back_forfeits_on_the_next_tick() {
        // test_duel_forfeits_when_the_dropped_player_never_returns.
        let e = env(4, 1);
        let (a, mut wa) = e.connect(ROOM, 1, "a").await;
        let (b, mut wb) = e.connect(ROOM, 2, "b").await;
        start(&e, (1, &a), (2, &b), &mut wa, &mut wb, karps(), karps()).await;
        e.disconnect(ROOM, 2, &b).await;
        until(&mut wa, GAME, "away").await;
        // The grace is lazy: nothing happens until someone pokes it.
        e.advance((DUEL_GRACE_SECS + 1) as f64);
        assert!(e.hub.with_room(ROOM, |v| v.duel.live()).unwrap());
        e.send(ROOM, 1, &a, GAME, "poke", json!({})).await;
        let end = until(&mut wa, GAME, "duelend").await;
        assert_eq!(end["winner"]["userId"], "a");
        assert_eq!(end["left"], true);
        assert_eq!(end["forfeit"], true);
        assert_eq!(end.as_object().unwrap().keys().map(String::as_str).collect::<Vec<_>>(),
                   vec!["type", "g", "ev", "pv", "duel", "winner", "forfeit", "left"]);
        assert!(!e.hub.with_room(ROOM, |v| v.duel.seated("b")).unwrap()); // the seat is free
    }

    #[tokio::test]
    async fn leaving_the_lobby_on_purpose_forfeits_at_once() {
        // test_duel_leaving_the_lobby_on_purpose_forfeits_at_once, plus the
        // `forfeit` op, which goes down the same path.
        for op in ["leave", "forfeit"] {
            let e = env(4, 1);
            let (a, mut wa) = e.connect(ROOM, 1, "a").await;
            let (b, mut wb) = e.connect(ROOM, 2, "b").await;
            start(&e, (1, &a), (2, &b), &mut wa, &mut wb, karps(), karps()).await;
            e.send(ROOM, 2, &b, GAME, op, json!({})).await;
            let end = until(&mut wa, GAME, "duelend").await;
            assert_eq!(end["winner"]["userId"], "a");
            assert_eq!(end["forfeit"], true);
            assert!(end.get("left").is_none()); // only a grace that ran out is "left"
            assert!(!e.hub.with_room(ROOM, |v| v.duel.live()).unwrap());
        }
    }

    #[tokio::test]
    async fn an_explicit_leave_mid_grace_still_says_left_in_the_lobby_event() {
        // The two conditions of `away = disconnected and v.duel.away(..)` are
        // separate: an explicit `leave` is never a grace, however live the match.
        let e = env(4, 1);
        let (a, mut wa) = e.connect(ROOM, 1, "a").await;
        let (b, mut wb) = e.connect(ROOM, 2, "b").await;
        start(&e, (1, &a), (2, &b), &mut wa, &mut wb, karps(), karps()).await;
        let _ = frames(&mut wa);
        e.send(ROOM, 2, &b, GAME, "leave", json!({})).await;
        let seen = frames(&mut wa);
        let order: Vec<_> = seen.iter().map(|m| m["ev"].as_str().unwrap()).collect();
        assert_eq!(order, vec!["duelend", "lobby"]);
        assert_eq!(seen[1]["left"]["userId"], "b");
    }

    #[tokio::test]
    async fn a_dropped_bystander_is_a_plain_left_and_their_offer_is_dropped() {
        let e = env(4, 1);
        let (a, mut wa) = e.connect(ROOM, 1, "a").await;
        let (b, mut wb) = e.connect(ROOM, 2, "b").await;
        both_in(&e, (1, &a), (2, &b), &mut wa, &mut wb).await;
        e.send(ROOM, 1, &a, GAME, "challenge", json!({"to": "b", "team": karps()})).await;
        until(&mut wb, GAME, "challenge").await;
        let _ = frames(&mut wb);
        e.disconnect(ROOM, 1, &a).await;
        // No match, so no "away": just the shared `lobby` event with `left`.
        let seen = frames(&mut wb);
        let order: Vec<_> = seen.iter().map(|m| m["ev"].as_str().unwrap()).collect();
        assert_eq!(order, vec!["lobby"]);
        assert_eq!(seen[0]["left"]["userId"], "a");
        // Their outstanding offer went with them, so b cannot accept it.
        e.send(ROOM, 2, &b, GAME, "accept", json!({"team": karps()})).await;
        assert_eq!(until(&mut wb, GAME, "error").await["error"], "that challenge is gone");
    }

    #[test]
    fn a_drop_during_a_match_is_away_not_left_without_any_sockets() {
        // test_duel_drop_during_a_match_is_away_not_left, which drives
        // `_leave_lobby` directly on a hand-built valley.
        let mut v = RoomValley::new("x");
        for uid in ["a", "b"] {
            v.lobbies[I_DUEL].put(uid, json!({"userId": uid}));
        }
        // a challenged b, b accepted: a live match with no sockets behind it.
        v.duel.pending.set("b", Pending { from: "a".into(), team: team(Some(&karps())).unwrap() });
        let (duel, lobby) = (&mut v.duel, &v.lobbies[I_DUEL]);
        let mut out = Out::new(GAME);
        duel.accept("b", 9, &json!({"team": karps()}), &mut out, lobby, 1000.0);
        assert!(v.duel.live());

        let mut out = Out::new(GAME);
        leave_lobby(&mut v, GAME, "b", &mut out, true, 1000.0);
        let evs: Vec<_> = out.items.iter()
            .map(|(_, p)| (p["ev"].as_str().unwrap(), p["left"].clone()))
            .collect();
        assert_eq!(evs, vec![("away", Value::Null), ("lobby", Value::Null)]);
        assert!(v.duel.live());
        assert!(v.duel.seated("b"));
        // ... and the roster really did lose them, which is what makes the
        // reserved seat necessary at all.
        assert!(!v.lobbies[I_DUEL].has("b"));
    }

    // ---------------------------------------------------- the missed ending --

    #[tokio::test]
    async fn an_ending_missed_while_offline_is_replayed_once_on_rejoin() {
        // test_duel_ending_missed_while_offline_is_replayed_on_rejoin.
        let e = env(4, 1);
        let (a, mut wa) = e.connect(ROOM, 1, "a").await;
        let (b, mut wb) = e.connect(ROOM, 2, "b").await;
        let duel = start(&e, (1, &a), (2, &b), &mut wa, &mut wb, karps(), karps()).await;
        e.disconnect(ROOM, 1, &a).await; // a's socket dropped
        until(&mut wb, GAME, "away").await;
        e.send(ROOM, 2, &b, GAME, "forfeit", json!({})).await;
        let end = until(&mut wb, GAME, "duelend").await;
        assert_eq!(end["winner"]["userId"], "a"); // b forfeited to the absent a

        let (a2, mut wa2) = e.connect(ROOM, 3, "a").await;
        e.send(ROOM, 3, &a2, GAME, "join", json!({})).await;
        // The ending comes BEFORE the snapshot, which is now empty.
        let seen = frames(&mut wa2);
        let order: Vec<_> = seen.iter().map(|m| m["ev"].as_str().unwrap()).collect();
        assert_eq!(order, vec!["lobby", "duelend", "duel"]);
        assert_eq!(seen[1]["duel"]["mid"], duel["mid"]);
        assert_eq!(seen[1]["winner"]["userId"], "a");
        assert_eq!(seen[1]["forfeit"], true);
        assert_eq!(seen[2]["duel"], Value::Null);
        // Replayed ONCE only.
        e.send(ROOM, 3, &a2, GAME, "join", json!({})).await;
        let order: Vec<_> = frames(&mut wa2).iter().map(|m| m["ev"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(order, vec!["lobby", "duel"]);
    }

    #[tokio::test]
    async fn an_ending_older_than_the_replay_window_is_dropped() {
        let e = env(4, 1);
        let (a, mut wa) = e.connect(ROOM, 1, "a").await;
        let (b, mut wb) = e.connect(ROOM, 2, "b").await;
        start(&e, (1, &a), (2, &b), &mut wa, &mut wb, karps(), karps()).await;
        e.disconnect(ROOM, 1, &a).await;
        until(&mut wb, GAME, "away").await;
        e.send(ROOM, 2, &b, GAME, "forfeit", json!({})).await;
        until(&mut wb, GAME, "duelend").await;
        e.advance(DUEL_RESULT_SECS as f64); // the test is a STRICT `<`
        let (a2, mut wa2) = e.connect(ROOM, 3, "a").await;
        e.send(ROOM, 3, &a2, GAME, "join", json!({})).await;
        let order: Vec<_> = frames(&mut wa2).iter().map(|m| m["ev"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(order, vec!["lobby", "duel"]); // no duelend
        // A fresh match also clears an unwatched ending for either player.
        assert!(e.hub.with_room(ROOM, |v| v.duel.missed.is_empty()).unwrap());
    }

    #[tokio::test]
    async fn accepting_a_new_challenge_clears_either_players_unwatched_ending() {
        let e = env(4, 1);
        let (a, mut wa) = e.connect(ROOM, 1, "a").await;
        let (b, mut wb) = e.connect(ROOM, 2, "b").await;
        start(&e, (1, &a), (2, &b), &mut wa, &mut wb, karps(), karps()).await;
        e.disconnect(ROOM, 1, &a).await;
        until(&mut wb, GAME, "away").await;
        e.send(ROOM, 2, &b, GAME, "forfeit", json!({})).await;
        until(&mut wb, GAME, "duelend").await;
        assert!(!e.hub.with_room(ROOM, |v| v.duel.missed.is_empty()).unwrap());
        let (a2, mut wa2) = e.connect(ROOM, 3, "a").await;
        // Rejoin WITHOUT reading it, then start again: the stored ending goes.
        e.hub.with_room(ROOM, |v| v.lobbies[I_DUEL].put("a", a2.public()));
        e.send(ROOM, 3, &a2, GAME, "challenge", json!({"to": "b", "team": karps()})).await;
        until(&mut wb, GAME, "challenge").await;
        e.send(ROOM, 2, &b, GAME, "accept", json!({"team": karps()})).await;
        until_where(&mut wb, GAME, "duel", |m| !m["duel"].is_null()).await;
        assert!(e.hub.with_room(ROOM, |v| v.duel.missed.is_empty()).unwrap());
        let _ = (&mut wa, &mut wa2);
    }

    // ------------------------------------------------------- odds and ends --

    #[tokio::test]
    async fn an_unrecognised_op_says_nothing_at_all() {
        let e = env(4, 1);
        let (a, mut wa) = e.connect(ROOM, 1, "a").await;
        e.send(ROOM, 1, &a, GAME, "join", json!({})).await;
        until(&mut wa, GAME, "lobby").await;
        let _ = frames(&mut wa);
        for op in ["quit", "surrender", "", "ACT"] {
            e.send(ROOM, 1, &a, GAME, op, json!({})).await;
        }
        assert!(frames(&mut wa).is_empty());
    }

    #[tokio::test]
    async fn poke_and_sync_with_no_match_say_only_what_they_must() {
        let e = env(4, 1);
        let (a, mut wa) = e.connect(ROOM, 1, "a").await;
        e.send(ROOM, 1, &a, GAME, "join", json!({})).await;
        assert_eq!(until(&mut wa, GAME, "duel").await["duel"], Value::Null);
        let _ = frames(&mut wa);
        e.send(ROOM, 1, &a, GAME, "poke", json!({})).await;
        assert!(frames(&mut wa).is_empty()); // a tick with no match does nothing
        e.send(ROOM, 1, &a, GAME, "sync", json!({})).await;
        let seen = frames(&mut wa);
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0]["ev"], "duel");
        assert_eq!(seen[0]["duel"], Value::Null);
        // `act` with no match is the same error as acting in someone else's.
        act(&e, 1, &a, 1, json!({"k": "move", "i": 0})).await;
        assert_eq!(until(&mut wa, GAME, "error").await["error"], "you are not in this duel");
    }

    #[tokio::test]
    async fn the_six_error_strings_are_pythons_byte_for_byte() {
        // One place a reviewer can read every string this engine can answer with.
        let e = env(4, 1);
        let (a, mut wa) = e.connect(ROOM, 1, "a").await;
        let (b, mut wb) = e.connect(ROOM, 2, "b").await;
        both_in(&e, (1, &a), (2, &b), &mut wa, &mut wb).await;
        let say = |m: &Value| m["error"].as_str().unwrap().to_string();

        act(&e, 1, &a, 1, json!({"k": "move", "i": 0})).await;
        assert_eq!(say(&until(&mut wa, GAME, "error").await), "you are not in this duel");
        e.send(ROOM, 1, &a, GAME, "challenge", json!({"to": "zz", "team": karps()})).await;
        assert_eq!(say(&until(&mut wa, GAME, "error").await), "they need to be in the duel lobby");
        e.send(ROOM, 1, &a, GAME, "challenge", json!({"to": "b", "team": []})).await;
        assert_eq!(say(&until(&mut wa, GAME, "error").await), "bring a team of 1 to 6 creatures");
        e.send(ROOM, 2, &b, GAME, "accept", json!({"team": karps()})).await;
        assert_eq!(say(&until(&mut wb, GAME, "error").await), "that challenge is gone");

        start(&e, (1, &a), (2, &b), &mut wa, &mut wb,
              json!([zard()]), json!([karp(), karp()])).await;
        e.send(ROOM, 1, &a, GAME, "challenge", json!({"to": "b", "team": karps()})).await;
        assert_eq!(say(&until(&mut wa, GAME, "error").await), "a duel is already on");
        act(&e, 1, &a, 1, json!({"k": "move", "i": 9})).await;
        assert_eq!(say(&until(&mut wa, GAME, "error").await), "you can't do that now");
        act(&e, 1, &a, 1, json!({"k": "move", "i": 0})).await;
        act(&e, 2, &b, 1, json!({"k": "move", "i": 0})).await;
        let t2 = until(&mut wa, GAME, "turn").await["duel"]["turn"].as_i64().unwrap();
        act(&e, 1, &a, t2, json!({"k": "move", "i": 0})).await;
        assert_eq!(say(&until(&mut wa, GAME, "error").await), "wait for the other player");
    }

    #[tokio::test]
    async fn every_match_broadcast_goes_to_the_whole_room() {
        // The audience rule in the module header, as a test: a socket in the
        // room that never joined the duel lobby still follows the battle.
        let e = env(4, 1);
        let (a, mut wa) = e.connect(ROOM, 1, "a").await;
        let (b, mut wb) = e.connect(ROOM, 2, "b").await;
        let (_c, mut wc) = e.connect(ROOM, 3, "c").await;
        start(&e, (1, &a), (2, &b), &mut wa, &mut wb, karps(), karps()).await;
        let _ = frames(&mut wc);
        act(&e, 1, &a, 1, json!({"k": "move", "i": 0})).await;
        act(&e, 2, &b, 1, json!({"k": "move", "i": 0})).await;
        e.send(ROOM, 2, &b, GAME, "forfeit", json!({})).await;
        let seen: Vec<_> = frames(&mut wc).iter()
            .map(|m| m["ev"].as_str().unwrap().to_string()).collect();
        assert!(!e.hub.with_room(ROOM, |v| v.lobbies[I_DUEL].has("c")).unwrap());
        assert_eq!(seen, vec!["waiting", "turn", "duelend"]);
    }
}
