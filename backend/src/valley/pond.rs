//! The fishing pond: a shared dock where the SERVER picks every bite, times
//! every reel and scores every catch. A room goal, and a boss fish the whole
//! lobby reels in together.
//!
//! Ported from `backend/app/valley.py`: the catalogs (:84-101), `class Pond`
//! (:328-476), the pond tail of the join arm (:1036-1038), the pond arm of
//! `handle` (:1092-1105) and the pond branch of `_leave_lobby` (:1542-1545).
//! The shared divergences from Python are listed once in valley.rs's module
//! doc; this file notes only its own.
//!
//! AUDIENCE: almost everything here goes to [`Out::all`] -- the WHOLE ROOM,
//! including sockets that never joined the pond lobby, which is how a page shows
//! the dock to a bystander. Exactly three things go to the one socket:
//! the `cast` reply (it carries the anti-cheat `token`, which nobody else may
//! see), the `loot` from a chest, and the `pond` snapshot a joiner gets.
//! `..._goes_to_the_whole_room` below pins both halves.
//!
//! WHY THIS ENGINE EXISTS SERVER-SIDE AT ALL. The reel runs on the client, so
//! the client could claim anything. Three rules make a claim checkable, and
//! each rejects differently -- which matters, because turning a drop into an
//! error turns a flood guard into an error storm:
//!   - [`POND_OP_RATE`]: at most 6 line ops per second per player. Over that the
//!     frame is SILENTLY DROPPED -- no error, no state change, no answer at all
//!     (valley.py:1093-1094). `pull` is not a line op; it has its own
//!     [`PULL_RATE`], and over THAT limit it is likewise silently dropped.
//!   - [`MIN_REEL_MS`]: a landing sooner than bite + the rarity's floor is
//!     REFUSED with "too fast — that wasn't a real reel", and the line stays in
//!     the water so an honest client can land it a moment later.
//!   - [`HOOK_EARLY_MS`]: striking more than 250 ms before the bite is REFUSED
//!     with "not yet". A hook with the wrong token, or a second hook on a line
//!     already hooked, is answered with NOTHING (`hook` is cosmetic, so a
//!     stale frame is not worth an error).
//!
//! A `land` or `lose` naming a cast that is not yours, or a token that does not
//! match, is "no such cast" for `land` and silence for `lose`.

use super::{as_num, is_true, py_round, py_round_to, Ctx, Left, Out, RoomValley, Seq, I_POND};
use rand::rngs::StdRng;
use rand::Rng;
use serde_json::{json, Value};

/// The game key on the wire.
pub const GAME: &str = "pond";

// ----------------------------------------------------------------- catalogs --
// valley.py:83: "Mirrors games/core.js (fish rarity); keep in sync."

/// Python's `FISH` (valley.py:84), name -> rarity. A pair array rather than a
/// map because `list(FISH)` reaches the weighting in INSERTION ORDER, and the
/// draw is over that order.
pub const FISH: [(&str, i64); 16] = [
    ("minnow", 1), ("perch", 1), ("carp", 1), ("mudcat", 1), ("bream", 2), ("trout", 2),
    ("pike", 2), ("eel", 2), ("sunfish", 2), ("koi", 3), ("sturgeon", 3), ("angler", 3),
    ("glowfin", 3), ("lumen", 4), ("leviathan", 4), ("ghostfish", 4),
];

// The three rarity tables. Python keys them by rarity 1..4; here they are
// INDEXED BY `rarity - 1`, which is total because every rarity in [`FISH`] is
// 1..=4 and a test pins that. An out-of-range rarity panics on the index, where
// Python's dict lookup raises KeyError -- the same bug surfacing the same way.

/// Python's `FISH_WEIGHT = {1: 10, 2: 5, 3: 2, 4: 0.6}`: how often that rarity
/// bites. f64 because rarity 4 is fractional.
pub const FISH_WEIGHT: [f64; 4] = [10.0, 5.0, 2.0, 0.6];

/// Python's `FISH_POINTS = {1: 1, 2: 3, 3: 8, 4: 20}`.
pub const FISH_POINTS: [i64; 4] = [1, 3, 8, 20];

/// Python's `MIN_REEL_MS` -- "faster than this is not a real reel".
pub const MIN_REEL_MS: [i64; 4] = [1500, 2200, 3000, 4000];

/// A reel longer than this (past the bite) is a lost fish, not a catch.
pub const MAX_REEL_MS: i64 = 90_000;

/// Room catches that pay out a `goal`.
pub const POND_GOAL: i64 = 20;

/// "a boss surfaces every 8th room catch".
pub const BOSS_EVERY: i64 = 8;

/// Seconds a boss stays up. Python's `BOSS_SECS` is an int added to
/// `now()`; f64 here because the clock is.
pub const BOSS_SECS: f64 = 60.0;

pub const BOSS_HP_PER_PLAYER: i64 = 40;

/// "pulls per second per player, at most". `usize` because it is only ever
/// compared against a list length.
pub const PULL_RATE: usize = 8;

/// "cast / hook / land / lose per second per player, at most".
pub const POND_OP_RATE: usize = 6;

/// "a cast with a treasure chest on the reel".
pub const CHEST_CHANCE: f64 = 0.15;

/// Python's `CHEST_LOOT`, item -> weight (the weights are ints there; f64 here
/// because the weighted draw is in floats, and 50/25/12/8/4/1 are exact).
pub const CHEST_LOOT: [(&str, f64); 6] = [
    ("quartz", 50.0), ("copper", 25.0), ("amethyst", 12.0), ("gold", 8.0), ("emerald", 4.0),
    ("ruby", 1.0),
];

/// "a hook this much before the bite still counts (latency)".
pub const HOOK_EARLY_MS: i64 = 250;

/// The boss is always a leviathan (valley.py:444).
const BOSS_FISH: &str = "leviathan";

/// Python's `FISH[name]`. Panics on a fish outside the catalog, which is
/// Python's `KeyError` in the same place; only a fish this module drew can get
/// here.
fn rarity_of(fish: &str) -> i64 {
    FISH.iter().find(|(f, _)| *f == fish).map(|(_, r)| *r).expect("a fish outside FISH")
}

/// Python's `rng.choices(population, weights=w)[0]` (CPython `random.py`):
/// accumulate the weights, draw `random() * total`, and take the FIRST index
/// whose running total is strictly greater than the draw -- which is what
/// `bisect.bisect` returns, being the RIGHT-hand insertion point. CPython caps
/// the index at `len - 1`, so a draw that reaches the total through float
/// rounding takes the last item; the fallthrough does the same.
///
/// Only the distribution ports, not the sequence (valley.rs divergence 2).
fn choices<'a>(rng: &mut StdRng, pop: &[(&'a str, f64)]) -> &'a str {
    let total: f64 = pop.iter().map(|(_, w)| w).sum();
    let x = rng.gen::<f64>() * total;
    let mut cum = 0.0;
    for (name, w) in pop {
        cum += w;
        if cum > x {
            return name;
        }
    }
    pop[pop.len() - 1].0
}

/// `secrets.token_urlsafe(8)` (valley.py:385): 8 random bytes as unpadded
/// url-safe base64, which is 11 characters.
///
/// Drawn from the OS generator and NOT from [`Ctx::rng`], exactly as Python
/// draws it from `secrets` and not from the patched `_rng`. This token is the
/// anti-cheat nonce that ties a `land` to a `cast`, so a seeded test -- or
/// anyone who guesses the seed -- must not be able to predict it. (auth.rs:39
/// has the same encoder, private to that module; duplicated in these eleven
/// lines rather than reaching into a file this port does not own.)
fn token() -> String {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut bytes = [0u8; 8];
    rand::thread_rng().fill(&mut bytes);
    let mut out = String::with_capacity(11);
    for chunk in bytes.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        for i in 0..chunk.len() + 1 {
            out.push(T[((n >> (18 - 6 * i)) & 0x3F) as usize] as char);
        }
    }
    out
}

// -------------------------------------------------------------------- state --

/// One line in the water. Python's cast dict (valley.py:385), key for key.
#[derive(Debug, Clone)]
pub struct Cast {
    /// The anti-cheat nonce. Only the caster's own socket ever sees it.
    pub token: String,
    /// "public cast id: lets every page match events to a line".
    pub id: i64,
    pub fish: &'static str,
    pub bite_ms: i64,
    /// `now()` at the cast, monotonic seconds.
    pub at: f64,
    /// Where along the dock, 0..1 at 3 decimal places.
    pub aim: f64,
    pub hooked: bool,
    /// Rolled by the SERVER at cast time; a client claiming a chest it was not
    /// given gets nothing.
    pub chest: bool,
}

/// The boss fish. Python's boss dict (valley.py:444).
#[derive(Debug, Clone)]
pub struct Boss {
    pub fish: &'static str,
    /// Fractional: two people pulling at once take 1.5 off, so the hp really
    /// can land on .5 and [`py_round`] (banker's) is what the wire sees.
    pub hp: f64,
    pub max: i64,
    /// `now()` the boss dives, monotonic seconds.
    pub until: f64,
}

/// Per-room pond state. One field of [`RoomValley`], in Python's `__init__`
/// order (valley.py:329-339).
#[derive(Default)]
pub struct Pond {
    /// user_id -> their line. [`Seq`] because `snapshot()["casting"]` is this
    /// map's key order on the wire, and a recast MOVES a player to the end of
    /// it (Python pops then reassigns) -- [`Seq::bump`], not [`Seq::set`].
    pub casts: Seq<Cast>,
    /// user_id -> points. [`Seq`] because the whole table is sent with every
    /// `caught` and `bossdown`.
    pub scores: Seq<i64>,
    pub goal: i64,
    pub catches: i64,
    pub boss: Option<Boss>,
    /// user_id -> recent pull times. [`Seq`] because `bossdown`'s `helpers`
    /// list is this map's key order and a Python test pins it exactly.
    pub pulls: Seq<Vec<f64>>,
    /// "recent line ops per player (rate limit)". Never serialised, but a
    /// [`Seq`] like its two neighbours so no reader has to work out which of
    /// the three maps may be a std map.
    pub ops: Seq<Vec<f64>>,
    pub seq: i64,
    pub last_boss_push: f64,
    /// "catch count that last spawned a boss", so one catch cannot spawn two.
    pub boss_at: i64,
}

impl Pond {
    /// Python's `snapshot()` (valley.py:341). "casting" stays for older pages;
    /// "lines" lets a late joiner draw every line.
    pub fn snapshot(&self, t: f64) -> Value {
        json!({
            "casting": self.casts.keys().collect::<Vec<_>>(),
            "scores": self.scores.to_object(|n| json!(n)),
            "goal": self.goal,
            "goalTarget": POND_GOAL,
            "boss": self.boss_view(t),
            "lines": self.casts.iter()
                .map(|(uid, c)| json!({"userId": uid, "id": c.id, "aim": c.aim,
                                       "hooked": c.hooked}))
                .collect::<Vec<_>>(),
        })
    }

    /// Python's `_boss_view()` (valley.py:348), or `None` -- which is JSON
    /// `null`, a key the page reads, so [`Value::Null`] and never an omission.
    /// Both numbers are `max(0, round(..))`: Python's banker's `round`, not
    /// `f64::round`.
    fn boss_view(&self, t: f64) -> Value {
        let Some(b) = &self.boss else { return Value::Null };
        json!({"fish": b.fish, "hp": py_round(b.hp).max(0), "max": b.max,
               "left": py_round(b.until - t).max(0)})
    }

    /// Python's `_expire_boss` (valley.py:355): a boss nobody finished in
    /// [`BOSS_SECS`] dives, and the pull book is wiped. Strictly `t > until`,
    /// so a frame landing exactly on the deadline still finds the boss up.
    fn expire_boss(&mut self, out: &mut Out, t: f64) {
        let gone = self.boss.as_ref().filter(|b| t > b.until).map(|b| b.fish);
        if let Some(fish) = gone {
            out.all("bossgone", json!({"fish": fish}));
            self.boss = None;
            self.pulls.clear();
        }
    }

    /// Python's `throttled` (valley.py:361): at most [`POND_OP_RATE`] line ops
    /// per second per player; the rest are dropped by the caller.
    ///
    /// Note what it does NOT do. It prunes only THIS player's window, and when
    /// the window is full it keeps the pruned list and adds nothing -- so a
    /// flood does not push its own limit out of reach, and a player who is
    /// being dropped stops accumulating. `ops[uid]` is written back on both
    /// paths, which is why the Python test can read its length.
    fn throttled(&mut self, uid: &str, t: f64) -> bool {
        let mine = self.ops.entry_or(uid, Vec::new);
        mine.retain(|x| t - *x < 1.0);
        if mine.len() >= POND_OP_RATE {
            return true;
        }
        mine.push(t);
        false
    }

    /// Python's `cast` (valley.py:372).
    fn cast(&mut self, cx: &mut Ctx, msg: &Value, out: &mut Out) {
        self.expire_boss(out, cx.t);
        if let Some(old) = self.casts.remove(cx.uid()) {
            // a line left out by an earlier socket or tab: reel it in for everyone
            out.all("lost", json!({"user": cx.me(), "id": old.id, "fish": old.fish}));
        }
        // Python: a number that is not a bool and not a NaN, clamped into
        // [0, 1] and rounded to 3dp; anything else is a random spot. That is
        // exactly `as_num` + `py_round_to`.
        //
        // Divergence: Python's guard is `aim == aim`, so an INFINITY passes and
        // clamps (+inf -> 1.0), where `as_num` rejects every non-finite. It is
        // unreachable -- Python's `json.loads` accepts the `Infinity` literal
        // and serde_json refuses it at parse, so such a frame never gets this
        // far in the Rust Arena.
        let aim = py_round_to(
            as_num(msg.get("aim"), 0.0, 1.0).unwrap_or_else(|| cx.rng.gen::<f64>()), 3);
        // Python's `rng.choices(list(FISH), weights=[FISH_WEIGHT[FISH[f]] ..])`:
        // the same 16-long list, built in FISH order, per cast.
        let pop: Vec<(&str, f64)> =
            FISH.iter().map(|(f, r)| (*f, FISH_WEIGHT[(*r - 1) as usize])).collect();
        let fish = choices(cx.rng, &pop);
        self.seq += 1;
        // The field order is the draw order: `bite_ms` then `chest`, as the
        // Python dict literal evaluates them (valley.py:385-386).
        let c = Cast {
            token: token(),
            id: self.seq,
            fish,
            bite_ms: cx.rng.gen_range(800..=3000), // randint is inclusive at both ends
            at: cx.t,
            aim,
            hooked: false,
            chest: cx.rng.gen::<f64>() < CHEST_CHANCE,
        };
        self.casts.bump(cx.uid(), c);
        let c = self.casts.get(cx.uid()).expect("just inserted");
        // The token goes to the caster ALONE; the lobby only learns where the
        // bobber landed.
        out.to(cx.conn, "cast", json!({"token": c.token, "id": c.id, "fish": c.fish,
                                       "rarity": rarity_of(c.fish), "biteIn": c.bite_ms,
                                       "chest": c.chest}));
        let id = c.id;
        out.all("casting", json!({"user": cx.me(), "id": id, "aim": aim}));
    }

    /// Python's `hook` (valley.py:392). Cosmetic: the angler struck on the
    /// bite, so the others see the fight start. A missing line, a token that
    /// does not match, or a second hook is answered with NOTHING.
    fn hook(&mut self, cx: &Ctx, msg: &Value, out: &mut Out) {
        let tok = msg.get("token").and_then(Value::as_str);
        let Some(c) = self.casts.get_mut(cx.uid()) else { return };
        if tok != Some(c.token.as_str()) || c.hooked {
            return;
        }
        if (cx.t - c.at) * 1000.0 < (c.bite_ms - HOOK_EARLY_MS) as f64 {
            out.err(cx.conn, "not yet");
            return;
        }
        c.hooked = true;
        let id = c.id;
        out.all("hooked", json!({"user": cx.me(), "id": id}));
    }

    /// Python's `land` (valley.py:403): the anti-cheat core.
    fn land(&mut self, cx: &mut Ctx, msg: &Value, out: &mut Out) {
        self.expire_boss(out, cx.t);
        let tok = msg.get("token").and_then(Value::as_str);
        // Python's one condition: `not c or msg.get("token") != c["token"]`. A
        // non-string token cannot equal the stored one, so it lands here too.
        if self.casts.get(cx.uid()).is_none_or(|c| tok != Some(c.token.as_str())) {
            out.err(cx.conn, "no such cast");
            return;
        }
        let c = self.casts.get(cx.uid()).expect("checked just above");
        let r = rarity_of(c.fish);
        let elapsed = (cx.t - c.at) * 1000.0;
        if elapsed < (c.bite_ms + MIN_REEL_MS[(r - 1) as usize]) as f64 {
            // REFUSED, and the line stays in the water: an honest client whose
            // frame was early can land the same cast a moment later.
            out.err(cx.conn, "too fast — that wasn't a real reel");
            return;
        }
        // Past this point the line is gone either way -- Python deletes it
        // BEFORE deciding between a catch and a lost fish (valley.py:414).
        let c = self.casts.remove(cx.uid()).expect("checked just above");
        if elapsed > (c.bite_ms + MAX_REEL_MS) as f64 {
            out.all("lost", json!({"user": cx.me(), "id": c.id, "fish": c.fish}));
            return;
        }
        // Cosmetic only: the reel runs on the client, so it can't score. An
        // IDENTITY check, so `1` and `"true"` are not a perfect reel.
        let perfect = is_true(msg.get("perfect"));
        let pts = FISH_POINTS[(r - 1) as usize];
        // The client may only OPEN the chest the server rolled at cast time.
        if is_true(msg.get("chest")) && c.chest {
            out.to(cx.conn, "loot", json!({"item": choices(cx.rng, &CHEST_LOOT)}));
        }
        *self.scores.entry_or(cx.uid(), || 0) += pts;
        self.goal += 1;
        self.catches += 1;
        out.all("caught", json!({"user": cx.me(), "id": c.id, "fish": c.fish, "points": pts,
                                 "perfect": perfect,
                                 "scores": self.scores.to_object(|n| json!(n)),
                                 "goal": self.goal, "goalTarget": POND_GOAL}));
        if self.goal >= POND_GOAL {
            out.all("goal", json!({"reward": "gold"}));
            self.goal = 0;
        }
    }

    /// Python's `lose` (valley.py:432): the client gave up on the line. Wrong
    /// token or no line: nothing at all, not even an error.
    fn lose(&mut self, cx: &Ctx, msg: &Value, out: &mut Out) {
        let tok = msg.get("token").and_then(Value::as_str);
        if self.casts.get(cx.uid()).is_some_and(|c| tok == Some(c.token.as_str())) {
            let c = self.casts.remove(cx.uid()).expect("checked just above");
            out.all("lost", json!({"user": cx.me(), "id": c.id, "fish": c.fish}));
        }
    }

    /// Python's `maybe_boss` (valley.py:438), called after every landing. Four
    /// reasons not to: one is already up, nothing has been caught, this is not
    /// an 8th catch, or this very catch count already spawned one.
    ///
    /// `players` is `len(lobby.members)`, read at the call site because the
    /// lobby and the pond are two disjoint borrows of one [`RoomValley`].
    fn maybe_boss(&mut self, players: usize, out: &mut Out, t: f64) {
        if self.boss.is_some()
            || self.catches == 0
            || self.catches % BOSS_EVERY != 0
            || self.catches == self.boss_at
        {
            return;
        }
        self.boss_at = self.catches;
        let hp = BOSS_HP_PER_PLAYER * players.max(1) as i64;
        self.boss = Some(Boss { fish: BOSS_FISH, hp: hp as f64, max: hp, until: t + BOSS_SECS });
        self.pulls.clear();
        out.all("boss", json!({"boss": self.boss_view(t)}));
    }

    /// Python's `pull` (valley.py:448): the co-op fight. Over [`PULL_RATE`] in
    /// a second the frame is silently dropped, like a throttled line op --
    /// `pull` is deliberately NOT one of the four line ops, so the two limits
    /// are independent.
    fn pull(&mut self, cx: &Ctx, out: &mut Out) {
        self.expire_boss(out, cx.t);
        if self.boss.is_none() {
            return;
        }
        let t = cx.t;
        let mine = self.pulls.entry_or(cx.uid(), Vec::new);
        mine.retain(|x| t - *x < 1.0);
        if mine.len() >= PULL_RATE {
            return;
        }
        mine.push(t);
        // How many DIFFERENT people pulled in the last second. Note Python
        // prunes only the caller's own window, so another player's stale times
        // stay in the map and this `any` is what decides who counts.
        let together =
            self.pulls.values().filter(|xs| xs.iter().any(|x| t - x < 1.0)).count() as i64;
        let hp = {
            let b = self.boss.as_mut().expect("a boss, checked above");
            b.hp -= 1.0 + 0.5 * (together - 1) as f64; // more people at once = stronger
            b.hp
        };
        if hp <= 0.0 {
            let fish = self.boss.as_ref().expect("a boss, checked above").fish;
            // Insertion order, and a Python test pins this list exactly.
            let helpers: Vec<String> = self
                .pulls
                .iter()
                .filter(|(_, xs)| !xs.is_empty())
                .map(|(u, _)| u.to_string())
                .collect();
            for uid in &helpers {
                *self.scores.entry_or(uid, || 0) += 10;
            }
            out.all("bossdown", json!({"fish": fish, "helpers": helpers,
                                       "scores": self.scores.to_object(|n| json!(n)),
                                       "reward": BOSS_FISH}));
            self.boss = None;
            self.pulls.clear();
        } else if t - self.last_boss_push > 0.15 {
            // At most ~7 hp frames a second however hard the lobby pulls.
            self.last_boss_push = t;
            out.all("bosshp", json!({"boss": self.boss_view(t), "pulling": together}));
        }
    }

    /// Python's `Pond.drop` (valley.py:472): forget a player who left; returns
    /// the line they still had out, if any. Renamed because an inherent `drop`
    /// reads as `std::mem::drop` at every call site.
    fn drop_player(&mut self, uid: &str) -> Option<Cast> {
        self.pulls.remove(uid);
        self.ops.remove(uid);
        self.casts.remove(uid)
    }
}

// ---------------------------------------------------------- the three hooks --

/// The join snapshot (valley.py:1036-1038), pushed after the shared `lobby`
/// event: expire a boss nobody touched, then hand this ONE socket the whole
/// dock.
pub fn joined(v: &mut RoomValley, cx: &mut Ctx, out: &mut Out) {
    v.pond.expire_boss(out, cx.t); // a boss nobody touched may have run out
    out.to(cx.conn, "pond", json!({"pond": v.pond.snapshot(cx.t)}));
}

/// The pond ops (valley.py:1092-1105). Reached only past the lobby gate.
pub fn op(v: &mut RoomValley, cx: &mut Ctx, op: &str, msg: &Value, out: &mut Out) {
    // valley.py:1093-1094: the four LINE ops share one rate limit, and over it
    // the frame is SILENTLY DROPPED -- `pass`, not an error. `pull` is not in
    // this list on purpose: it has its own limit inside `pull`.
    if matches!(op, "cast" | "hook" | "land" | "lose") && v.pond.throttled(cx.uid(), cx.t) {
        return;
    }
    match op {
        "cast" => v.pond.cast(cx, msg, out),
        "hook" => v.pond.hook(cx, msg, out),
        "land" => {
            v.pond.land(cx, msg, out);
            // valley.py:1101 -- the boss check runs on the lobby as it is AFTER
            // the landing, and only `land` ever triggers it.
            let players = v.lobbies[I_POND].members.len();
            v.pond.maybe_boss(players, out, cx.t);
        }
        "lose" => v.pond.lose(cx, msg, out),
        "pull" => v.pond.pull(cx, out),
        // An unrecognised op produces NOTHING, as Python's else-less if/elif
        // chain does. Note it is also NOT throttled, so it cannot be used to
        // exhaust someone's line-op budget.
        _ => {}
    }
}

/// Someone left the pond lobby, by `leave` or by a dropped socket
/// (valley.py:1542-1545). The pond keeps no grace period -- unlike duel and
/// golf, a blip costs you the line -- so neither `t` nor `disconnected` is
/// read, and the answer is always [`Left::Notice`].
pub fn dropped(
    v: &mut RoomValley, uid: &str, who: &Value, out: &mut Out, t: f64, disconnected: bool,
) -> Left {
    let _ = (t, disconnected);
    if let Some(c) = v.pond.drop_player(uid) {
        // their bobber leaves everyone else's water too. `who` is the profile
        // the LOBBY held, not `member.public()`: the socket may already be gone.
        out.all("lost", json!({"user": who, "id": c.id, "fish": c.fish}));
    }
    Left::Notice
}

#[cfg(test)]
mod tests {
    use super::super::testkit::*;
    use super::super::HOST_GRACE;
    use super::*;
    use serde_json::json;

    const ROOM: &str = "lobby";

    /// A `land` that is late enough for any rarity: the slowest floor is
    /// 4000 ms and the longest bite 3000 ms.
    const ANY_REEL: f64 = 10.0;

    /// `{"type":"game","g":"pond","op":op, ..}` through the real dispatcher.
    async fn pond_op(
        e: &Env, conn: u64, m: &crate::rooms::Member, op: &str, extra: Value,
    ) {
        e.send(ROOM, conn, m, GAME, op, extra).await;
    }

    /// Join the pond lobby and swallow the snapshot.
    async fn join(e: &Env, conn: u64, m: &crate::rooms::Member,
                  rx: &mut tokio::sync::mpsc::Receiver<String>) {
        pond_op(e, conn, m, "join", json!({})).await;
        until(rx, GAME, "pond").await;
    }

    // ----------------------------------------------------------- the tables --

    #[test]
    fn the_catalogs_are_pythons_in_pythons_order() {
        // valley.py:84-101, byte for byte.
        assert_eq!(FISH,
                   [("minnow", 1), ("perch", 1), ("carp", 1), ("mudcat", 1), ("bream", 2),
                    ("trout", 2), ("pike", 2), ("eel", 2), ("sunfish", 2), ("koi", 3),
                    ("sturgeon", 3), ("angler", 3), ("glowfin", 3), ("lumen", 4),
                    ("leviathan", 4), ("ghostfish", 4)]);
        assert_eq!(FISH_WEIGHT, [10.0, 5.0, 2.0, 0.6]);
        assert_eq!(FISH_POINTS, [1, 3, 8, 20]);
        assert_eq!(MIN_REEL_MS, [1500, 2200, 3000, 4000]);
        assert_eq!(MAX_REEL_MS, 90_000);
        assert_eq!(POND_GOAL, 20);
        assert_eq!(BOSS_EVERY, 8);
        assert_eq!(BOSS_SECS, 60.0);
        assert_eq!(BOSS_HP_PER_PLAYER, 40);
        assert_eq!(PULL_RATE, 8);
        assert_eq!(POND_OP_RATE, 6);
        assert_eq!(CHEST_CHANCE, 0.15);
        assert_eq!(CHEST_LOOT,
                   [("quartz", 50.0), ("copper", 25.0), ("amethyst", 12.0), ("gold", 8.0),
                    ("emerald", 4.0), ("ruby", 1.0)]);
        assert_eq!(HOOK_EARLY_MS, 250);
        // Every rarity indexes the three tables, which is what makes `r - 1`
        // total rather than a lurking panic.
        for (_, r) in FISH {
            assert!((1..=4).contains(&r));
        }
    }

    #[test]
    fn the_weighted_draw_follows_the_table() {
        // Only the distribution ports (valley.rs divergence 2), so this asserts
        // the shape Python's own pond test asserts: a fish from the catalog,
        // whose rarity is in the table, and the common ones far more often.
        use rand::SeedableRng;
        let mut rng = StdRng::seed_from_u64(7);
        let pop: Vec<(&str, f64)> =
            FISH.iter().map(|(f, r)| (*f, FISH_WEIGHT[(*r - 1) as usize])).collect();
        let mut by_rarity = [0; 4];
        for _ in 0..20_000 {
            let f = choices(&mut rng, &pop);
            by_rarity[(rarity_of(f) - 1) as usize] += 1;
        }
        // Expected shares of the 74.8 total weight: 40, 25, 8, 1.8.
        assert!(by_rarity[0] > by_rarity[1], "{by_rarity:?}");
        assert!(by_rarity[1] > by_rarity[2], "{by_rarity:?}");
        assert!(by_rarity[2] > by_rarity[3], "{by_rarity:?}");
        assert!(by_rarity[3] > 0, "{by_rarity:?}");
        // Every loot item is reachable and nothing outside the table is.
        let items: Vec<&str> = CHEST_LOOT.iter().map(|(i, _)| *i).collect();
        for _ in 0..200 {
            assert!(items.contains(&choices(&mut rng, &CHEST_LOOT)));
        }
    }

    #[test]
    fn a_cast_token_is_eleven_url_safe_characters() {
        // `secrets.token_urlsafe(8)`'s shape, and never twice the same.
        let a = token();
        assert_eq!(a.chars().count(), 11);
        assert!(a.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'), "{a}");
        assert_ne!(a, token());
    }

    // ----------------------------------------------------- the anti-cheat --

    #[tokio::test]
    async fn a_fast_reel_is_refused_and_a_real_one_scores() {
        // test_pond_refuses_fast_reels_and_scores_real_ones (test_valley.py:78).
        let e = env(4, 1);
        let (a, mut wa) = e.connect(ROOM, 1, "a").await;
        join(&e, 1, &a, &mut wa).await;
        pond_op(&e, 1, &a, "cast", json!({})).await;
        let cast = until(&mut wa, GAME, "cast").await;
        pond_op(&e, 1, &a, "land", json!({"token": cast["token"]})).await;
        // Byte for byte, em-dash and all (valley.py:412).
        assert_eq!(until(&mut wa, GAME, "error").await["error"],
                   "too fast — that wasn't a real reel");
        // ... and the line is STILL in the water: a refusal is not a forfeit.
        assert_eq!(e.hub.with_room(ROOM, |v| v.pond.casts.len()), Some(1));
        let rarity = cast["rarity"].as_i64().unwrap();
        e.advance(
            (cast["biteIn"].as_f64().unwrap() + MIN_REEL_MS[(rarity - 1) as usize] as f64)
                / 1000.0
                + 0.1,
        );
        pond_op(&e, 1, &a, "land", json!({"token": cast["token"]})).await;
        let got = until(&mut wa, GAME, "caught").await;
        assert_eq!(got["fish"], cast["fish"]);
        assert_eq!(got["points"], FISH_POINTS[(rarity - 1) as usize]);
        assert_eq!(got["goal"], 1);
        assert_eq!(got["goalTarget"], 20);
        assert_eq!(got["scores"]["a"], FISH_POINTS[(rarity - 1) as usize]);
        // The same token twice is not a second fish.
        pond_op(&e, 1, &a, "land", json!({"token": cast["token"]})).await;
        assert_eq!(until(&mut wa, GAME, "error").await["error"], "no such cast");
    }

    #[tokio::test]
    async fn a_land_with_the_wrong_token_or_no_line_is_no_such_cast() {
        let e = env(4, 1);
        let (a, mut wa) = e.connect(ROOM, 1, "a").await;
        join(&e, 1, &a, &mut wa).await;
        // No line at all.
        pond_op(&e, 1, &a, "land", json!({"token": "nope"})).await;
        assert_eq!(until(&mut wa, GAME, "error").await["error"], "no such cast");
        pond_op(&e, 1, &a, "cast", json!({})).await;
        until(&mut wa, GAME, "cast").await;
        e.advance(ANY_REEL);
        // A token of the wrong TYPE cannot match either -- Python compares the
        // raw value, so 7 != "<token>".
        for tok in [json!("wrong"), json!(7), Value::Null] {
            pond_op(&e, 1, &a, "land", json!({"token": tok})).await;
            assert_eq!(until(&mut wa, GAME, "error").await["error"], "no such cast");
        }
        assert_eq!(e.hub.with_room(ROOM, |v| v.pond.casts.len()), Some(1)); // still out
    }

    #[tokio::test]
    async fn a_hook_waits_for_the_bite_and_shows_in_the_snapshot() {
        // test_pond_hook_waits_for_the_bite_and_shows_in_snapshot (:148).
        let e = env(4, 1);
        let (a, mut wa) = e.connect(ROOM, 1, "a").await;
        let (b, mut wb) = e.connect(ROOM, 2, "b").await;
        join(&e, 1, &a, &mut wa).await;
        join(&e, 2, &b, &mut wb).await;
        pond_op(&e, 1, &a, "cast", json!({"aim": 0.3})).await;
        let cast = until(&mut wa, GAME, "cast").await;
        pond_op(&e, 1, &a, "hook", json!({"token": cast["token"]})).await;
        assert_eq!(until(&mut wa, GAME, "error").await["error"], "not yet");
        // HOOK_EARLY_MS of latency is forgiven, so 240 ms early is a hook.
        e.advance((cast["biteIn"].as_f64().unwrap() - HOOK_EARLY_MS as f64 + 10.0) / 1000.0);
        pond_op(&e, 1, &a, "hook", json!({"token": cast["token"]})).await;
        let hooked = until(&mut wb, GAME, "hooked").await;
        assert_eq!(hooked["user"]["handle"], "a");
        assert_eq!(hooked["id"], cast["id"]);
        // A second hook on the same line says nothing at all.
        pond_op(&e, 1, &a, "hook", json!({"token": cast["token"]})).await;
        // A late joiner gets every line with its aim and its state.
        pond_op(&e, 2, &b, "join", json!({})).await;
        let snap = until(&mut wb, GAME, "pond").await;
        assert_eq!(snap["pond"]["casting"], json!(["a"]));
        assert_eq!(snap["pond"]["lines"],
                   json!([{"userId": "a", "id": cast["id"], "aim": 0.3, "hooked": true}]));
        assert_eq!(snap["pond"]["boss"], Value::Null); // present as null, not omitted
        assert_eq!(snap["pond"]["goalTarget"], 20);
    }

    #[tokio::test]
    async fn a_hook_with_the_wrong_token_is_answered_with_nothing() {
        let e = env(4, 1);
        let (a, mut wa) = e.connect(ROOM, 1, "a").await;
        join(&e, 1, &a, &mut wa).await;
        pond_op(&e, 1, &a, "cast", json!({})).await;
        until(&mut wa, GAME, "cast").await;
        let _ = drain(&mut wa); // the `casting` the whole room got
        e.advance(ANY_REEL);
        pond_op(&e, 1, &a, "hook", json!({"token": "nope"})).await;
        // `hook` is cosmetic, so a stale frame earns silence, not "not yet".
        assert!(drain(&mut wa).is_empty());
        // And a hook with NO line at all is equally quiet.
        pond_op(&e, 1, &a, "lose", json!({"token": "nope"})).await;
        pond_op(&e, 1, &a, "hook", json!({})).await;
        assert!(drain(&mut wa).is_empty());
    }

    #[tokio::test]
    async fn line_ops_are_rate_limited_and_the_extra_ones_are_dropped_in_silence() {
        // test_pond_line_ops_are_rate_limited (:245). The point of the test is
        // that the throttle DROPS: an error per flooded frame would be an
        // amplifier, not a guard.
        let e = env(4, 1);
        let (a, mut wa) = e.connect(ROOM, 1, "a").await;
        join(&e, 1, &a, &mut wa).await;
        for _ in 0..POND_OP_RATE + 4 {
            pond_op(&e, 1, &a, "land", json!({"token": "nope"})).await;
        }
        // Exactly POND_OP_RATE of the ten were answered; the other four are gone.
        assert_eq!(drain(&mut wa).iter().filter(|(g, ev)| g == GAME && ev == "error").count(),
                   POND_OP_RATE);
        // `join` is not a line op, so it is always answered even mid-flood.
        pond_op(&e, 1, &a, "join", json!({})).await;
        until(&mut wa, GAME, "pond").await;
        // The window keeps the cap and does not grow past it.
        assert_eq!(e.hub.with_room(ROOM, |v| v.pond.ops.get("a").map(Vec::len)),
                   Some(Some(POND_OP_RATE)));
        // A second later the budget is back.
        e.advance(1.1);
        pond_op(&e, 1, &a, "cast", json!({})).await;
        until(&mut wa, GAME, "cast").await;
    }

    #[tokio::test]
    async fn an_unknown_pond_op_produces_nothing_and_spends_no_budget() {
        let e = env(4, 1);
        let (a, mut wa) = e.connect(ROOM, 1, "a").await;
        join(&e, 1, &a, &mut wa).await;
        for _ in 0..20 {
            pond_op(&e, 1, &a, "reel", json!({})).await;
        }
        assert!(drain(&mut wa).is_empty());
        assert_eq!(e.hub.with_room(ROOM, |v| v.pond.ops.contains_key("a")), Some(false));
    }

    #[tokio::test]
    async fn a_reel_longer_than_ninety_seconds_is_a_lost_fish_not_a_catch() {
        let e = env(4, 1);
        let (a, mut wa) = e.connect(ROOM, 1, "a").await;
        join(&e, 1, &a, &mut wa).await;
        pond_op(&e, 1, &a, "cast", json!({})).await;
        let cast = until(&mut wa, GAME, "cast").await;
        e.advance((cast["biteIn"].as_f64().unwrap() + MAX_REEL_MS as f64) / 1000.0 + 0.1);
        pond_op(&e, 1, &a, "land", json!({"token": cast["token"]})).await;
        let lost = until(&mut wa, GAME, "lost").await;
        assert_eq!(lost["id"], cast["id"]);
        assert_eq!(lost["fish"], cast["fish"]);
        // No points, no goal, no catch -- so no boss can be counted towards.
        let (goal, catches, scores) =
            e.hub.with_room(ROOM, |v| (v.pond.goal, v.pond.catches, v.pond.scores.len())).unwrap();
        assert_eq!((goal, catches, scores), (0, 0, 0));
    }

    // ------------------------------------------------------------- the cast --

    #[tokio::test]
    async fn the_cast_aim_is_clamped_and_shared() {
        // test_pond_cast_aim_is_clamped_and_shared (:129).
        let e = env(4, 1);
        let (a, mut wa) = e.connect(ROOM, 1, "a").await;
        let (b, mut wb) = e.connect(ROOM, 2, "b").await;
        join(&e, 1, &a, &mut wa).await;
        join(&e, 2, &b, &mut wb).await;
        let cases: [(Value, Option<f64>); 6] = [
            (json!(5), Some(1.0)),         // clamped up
            (json!(-2), Some(0.0)),        // clamped down
            (json!(0.42), Some(0.42)),     // kept
            (json!("x"), None),            // not a number: a random spot
            (json!(true), None),           // a bool is not a number in Python either
            (Value::Null, None),           // absent
        ];
        for (aim, want) in cases {
            let extra = if aim.is_null() { json!({}) } else { json!({"aim": aim}) };
            pond_op(&e, 1, &a, "cast", extra).await;
            let cast = until(&mut wa, GAME, "cast").await;
            let seen = until(&mut wb, GAME, "casting").await;
            assert_eq!(seen["user"]["handle"], "a");
            assert_eq!(seen["id"], cast["id"]);
            // Always a JSON float, never an int, because Python rounds to 3dp.
            assert!(seen["aim"].is_f64(), "{aim} -> {}", seen["aim"]);
            let got = seen["aim"].as_f64().unwrap();
            match want {
                Some(x) => assert_eq!(got, x, "{aim}"),
                None => assert!((0.0..=1.0).contains(&got), "{aim} -> {got}"),
            }
            assert!(cast["chest"].is_boolean());
            assert!(FISH.iter().any(|(f, _)| *f == cast["fish"].as_str().unwrap()));
            assert_eq!(cast["rarity"], rarity_of(cast["fish"].as_str().unwrap()));
            assert!((800..=3000).contains(&cast["biteIn"].as_i64().unwrap()));
            pond_op(&e, 1, &a, "lose", json!({"token": cast["token"]})).await;
            assert_eq!(until(&mut wb, GAME, "lost").await["id"], cast["id"]);
            e.advance(1.1); // stay under the per-player op rate
        }
    }

    #[tokio::test]
    async fn a_second_cast_replaces_the_old_line() {
        // test_pond_second_cast_replaces_the_old_line (:224).
        let e = env(4, 1);
        let (a, mut wa) = e.connect(ROOM, 1, "a").await;
        let (b, mut wb) = e.connect(ROOM, 2, "b").await;
        join(&e, 1, &a, &mut wa).await;
        join(&e, 2, &b, &mut wb).await;
        pond_op(&e, 1, &a, "cast", json!({})).await;
        let old = until(&mut wa, GAME, "cast").await;
        until(&mut wb, GAME, "casting").await;
        e.advance(1.1);
        pond_op(&e, 1, &a, "cast", json!({})).await;
        // The old line is reeled in for everyone BEFORE the new one is shown.
        assert_eq!(drain(&mut wb).iter().filter(|(g, _)| g == GAME).map(|(_, ev)| ev.as_str())
                       .collect::<Vec<_>>(),
                   vec!["lost", "casting"]);
        let new = until(&mut wa, GAME, "cast").await;
        assert_ne!(new["id"], old["id"]);
        // The old token is dead, and the cast id never repeats.
        pond_op(&e, 1, &a, "land", json!({"token": old["token"]})).await;
        assert_eq!(until(&mut wa, GAME, "error").await["error"], "no such cast");
    }

    #[tokio::test]
    async fn a_recast_moves_a_player_to_the_end_of_the_casting_order() {
        // Python pops then reassigns, so the dict order changes and
        // `snapshot()["casting"]` is the proof -- Seq::bump, not Seq::set.
        let e = env(4, 1);
        let (a, mut wa) = e.connect(ROOM, 1, "a").await;
        let (b, mut wb) = e.connect(ROOM, 2, "b").await;
        join(&e, 1, &a, &mut wa).await;
        join(&e, 2, &b, &mut wb).await;
        pond_op(&e, 1, &a, "cast", json!({})).await;
        pond_op(&e, 2, &b, "cast", json!({})).await;
        pond_op(&e, 2, &b, "join", json!({})).await;
        assert_eq!(until(&mut wb, GAME, "pond").await["pond"]["casting"], json!(["a", "b"]));
        e.advance(1.1);
        pond_op(&e, 1, &a, "cast", json!({})).await;
        pond_op(&e, 2, &b, "join", json!({})).await;
        assert_eq!(until(&mut wb, GAME, "pond").await["pond"]["casting"], json!(["b", "a"]));
    }

    // -------------------------------------------------------------- scoring --

    #[tokio::test]
    async fn the_room_goal_pays_gold_at_twenty_and_starts_again() {
        let e = env(4, 1);
        let (a, mut wa) = e.connect(ROOM, 1, "a").await;
        join(&e, 1, &a, &mut wa).await;
        // Python's own goal test drives 20 catches through the websocket; one
        // catch off a primed counter pins the same two lines (valley.py:428-430)
        // without 20 round trips.
        e.hub.with_room(ROOM, |v| v.pond.goal = POND_GOAL - 1);
        pond_op(&e, 1, &a, "cast", json!({})).await;
        let cast = until(&mut wa, GAME, "cast").await;
        e.advance(ANY_REEL);
        pond_op(&e, 1, &a, "land", json!({"token": cast["token"]})).await;
        assert_eq!(until(&mut wa, GAME, "caught").await["goal"], POND_GOAL);
        assert_eq!(until(&mut wa, GAME, "goal").await["reward"], "gold");
        assert_eq!(e.hub.with_room(ROOM, |v| v.pond.goal), Some(0));
    }

    /// The seeds whose per-message RNG puts the chest draw either side of
    /// [`CHEST_CHANCE`]. Python's test monkeypatches `valley.CHEST_CHANCE` to 1
    /// and 0; a Rust `const` cannot be patched, so the two outcomes are reached
    /// by choosing the dice instead -- `testkit::env` reseeds identically for
    /// every message, so a chest is deterministic within one Env.
    const SEED_CHEST: u64 = 10;
    const SEED_NO_CHEST: u64 = 1;

    #[tokio::test]
    async fn perfect_is_cosmetic_and_the_chest_is_server_rolled() {
        // test_pond_perfect_is_cosmetic_and_chest_is_server_rolled (:169), which
        // reads frames until the `caught` and counts the `loot` among them --
        // the loot is pushed FIRST, so it cannot be found by looking for the
        // catch and then draining.
        for (seed, want_chest) in [(SEED_CHEST, true), (SEED_NO_CHEST, false)] {
            let e = env(4, seed);
            let (a, mut wa) = e.connect(ROOM, 1, "a").await;
            join(&e, 1, &a, &mut wa).await;
            pond_op(&e, 1, &a, "cast", json!({})).await;
            let cast = until(&mut wa, GAME, "cast").await;
            assert_eq!(cast["chest"], want_chest, "seed {seed}");
            e.advance(ANY_REEL);
            pond_op(&e, 1, &a, "land", json!({"token": cast["token"], "perfect": true,
                                              "chest": true})).await;
            let mut loot = 0;
            let got = loop {
                let m: Value =
                    serde_json::from_str(&wa.recv().await.expect("queue open")).unwrap();
                if m["g"] != GAME {
                    continue;
                }
                if m["ev"] == "loot" {
                    loot += 1;
                } else if m["ev"] == "caught" {
                    break m;
                }
            };
            // Relayed for the feed and the card ...
            assert_eq!(got["perfect"], true);
            // ... but never worth points: the reel runs on the client.
            let rarity = cast["rarity"].as_i64().unwrap();
            assert_eq!(got["points"], FISH_POINTS[(rarity - 1) as usize]);
            // Exactly one chest pays out, and only when the server rolled one.
            assert_eq!(loot, if want_chest { 1 } else { 0 }, "seed {seed}");
        }
    }

    #[tokio::test]
    async fn loot_needs_both_a_server_chest_and_an_identity_true() {
        let e = env(4, SEED_CHEST);
        let (a, mut wa) = e.connect(ROOM, 1, "a").await;
        join(&e, 1, &a, &mut wa).await;
        pond_op(&e, 1, &a, "cast", json!({})).await;
        let cast = until(&mut wa, GAME, "cast").await;
        assert_eq!(cast["chest"], true);
        e.advance(ANY_REEL);
        // `is True` is an IDENTITY check, so a truthy 1 opens nothing.
        pond_op(&e, 1, &a, "land", json!({"token": cast["token"], "chest": 1,
                                          "perfect": 1})).await;
        let seen: Vec<String> =
            drain(&mut wa).into_iter().filter(|(g, _)| g == GAME).map(|(_, ev)| ev).collect();
        assert!(!seen.contains(&"loot".to_string()), "{seen:?}");
        assert!(seen.contains(&"caught".to_string()), "{seen:?}");

        // The same frame against a cast the server rolled WITHOUT a chest is
        // equally empty -- a client cannot grant itself one.
        let e = env(4, SEED_NO_CHEST);
        let (a, mut wa) = e.connect(ROOM, 1, "a").await;
        join(&e, 1, &a, &mut wa).await;
        pond_op(&e, 1, &a, "cast", json!({})).await;
        let cast = until(&mut wa, GAME, "cast").await;
        assert_eq!(cast["chest"], false);
        e.advance(ANY_REEL);
        pond_op(&e, 1, &a, "land", json!({"token": cast["token"], "chest": true})).await;
        let seen: Vec<String> =
            drain(&mut wa).into_iter().filter(|(g, _)| g == GAME).map(|(_, ev)| ev).collect();
        assert!(!seen.contains(&"loot".to_string()), "{seen:?}");
    }

    #[tokio::test]
    async fn a_chest_pays_one_item_from_the_loot_table_to_the_opener_alone() {
        let e = env(4, SEED_CHEST);
        let (a, mut wa) = e.connect(ROOM, 1, "a").await;
        let (b, mut wb) = e.connect(ROOM, 2, "b").await;
        join(&e, 1, &a, &mut wa).await;
        join(&e, 2, &b, &mut wb).await;
        pond_op(&e, 1, &a, "cast", json!({})).await;
        let cast = until(&mut wa, GAME, "cast").await;
        e.advance(ANY_REEL);
        pond_op(&e, 1, &a, "land", json!({"token": cast["token"], "chest": true})).await;
        let loot = until(&mut wa, GAME, "loot").await;
        assert!(CHEST_LOOT.iter().any(|(i, _)| *i == loot["item"].as_str().unwrap()),
                "{}", loot["item"]);
        // b sees the catch but never the loot: it is `out.to`, one socket.
        let seen: Vec<String> =
            drain(&mut wb).into_iter().filter(|(g, _)| g == GAME).map(|(_, ev)| ev).collect();
        assert!(seen.contains(&"caught".to_string()), "{seen:?}");
        assert!(!seen.contains(&"loot".to_string()), "{seen:?}");
    }

    // ----------------------------------------------------------- the boss --

    #[tokio::test]
    async fn the_boss_needs_everyone_pulling() {
        // test_pond_boss_needs_everyone_pulling (:98).
        let e = env(4, 1);
        let (a, mut wa) = e.connect(ROOM, 1, "a").await;
        join(&e, 1, &a, &mut wa).await;
        e.hub.with_room(ROOM, |v| v.pond.catches = BOSS_EVERY - 1);
        pond_op(&e, 1, &a, "cast", json!({})).await;
        let cast = until(&mut wa, GAME, "cast").await;
        e.advance(ANY_REEL);
        pond_op(&e, 1, &a, "land", json!({"token": cast["token"]})).await;
        let boss = until(&mut wa, GAME, "boss").await;
        assert_eq!(boss["boss"]["hp"], BOSS_HP_PER_PLAYER); // one player in the lobby
        assert_eq!(boss["boss"]["max"], BOSS_HP_PER_PLAYER);
        assert_eq!(boss["boss"]["fish"], "leviathan");
        assert_eq!(boss["boss"]["left"], BOSS_SECS as i64);
        // Only PULL_RATE pulls count per second, and `pull` is NOT a line op,
        // so POND_OP_RATE does not apply to it.
        for _ in 0..PULL_RATE + 5 {
            pond_op(&e, 1, &a, "pull", json!({})).await;
        }
        let hp = until(&mut wa, GAME, "bosshp").await;
        assert_eq!(hp["pulling"], 1);
        assert_eq!(e.hub.with_room(ROOM, |v| v.pond.boss.as_ref().unwrap().hp),
                   Some(BOSS_HP_PER_PLAYER as f64 - PULL_RATE as f64));
        for _ in 0..10 {
            e.advance(1.1);
            for _ in 0..PULL_RATE {
                pond_op(&e, 1, &a, "pull", json!({})).await;
            }
        }
        let down = until(&mut wa, GAME, "bossdown").await;
        assert_eq!(down["helpers"], json!(["a"])); // insertion order, pinned exactly
        assert_eq!(down["fish"], "leviathan");
        assert_eq!(down["reward"], "leviathan");
        assert!(down["scores"]["a"].as_i64().unwrap() >= 10);
        assert_eq!(e.hub.with_room(ROOM, |v| v.pond.boss.is_some()), Some(false));
        // With no boss up, a pull is answered with nothing at all.
        pond_op(&e, 1, &a, "pull", json!({})).await;
        assert!(drain(&mut wa).is_empty());
    }

    #[tokio::test]
    async fn two_pulling_together_hit_harder_than_two_taking_turns() {
        // hp -= 1 + 0.5 * (together - 1), which is why the hp is a float and
        // py_round (banker's) is what the wire sees.
        let e = env(4, 1);
        let (a, mut wa) = e.connect(ROOM, 1, "a").await;
        let (b, mut wb) = e.connect(ROOM, 2, "b").await;
        join(&e, 1, &a, &mut wa).await;
        join(&e, 2, &b, &mut wb).await;
        e.hub.with_room(ROOM, |v| {
            v.pond.boss = Some(Boss { fish: "leviathan", hp: 40.0, max: 80, until: 1060.0 });
        });
        pond_op(&e, 1, &a, "pull", json!({})).await; // alone: -1.0
        assert_eq!(e.hub.with_room(ROOM, |v| v.pond.boss.as_ref().unwrap().hp), Some(39.0));
        pond_op(&e, 2, &b, "pull", json!({})).await; // together: -1.5
        assert_eq!(e.hub.with_room(ROOM, |v| v.pond.boss.as_ref().unwrap().hp), Some(37.5));
        // 37.5 on the wire is 38, not 37: Python's round() is banker's and
        // rounds .5 to the EVEN neighbour.
        assert_eq!(py_round(37.5), 38);
        pond_op(&e, 2, &b, "join", json!({})).await;
        assert_eq!(until(&mut wb, GAME, "pond").await["pond"]["boss"]["hp"], 38);
    }

    #[tokio::test]
    async fn the_boss_dives_after_a_minute_and_the_whole_room_hears_it() {
        let e = env(4, 1);
        let (a, mut wa) = e.connect(ROOM, 1, "a").await;
        join(&e, 1, &a, &mut wa).await;
        e.hub.with_room(ROOM, |v| {
            v.pond.boss = Some(Boss { fish: "leviathan", hp: 40.0, max: 40, until: 1000.0 + BOSS_SECS });
            v.pond.pulls.set("a", vec![1000.0]);
        });
        // Strictly `now() > until`: exactly on the deadline the boss is still up.
        e.advance(BOSS_SECS);
        pond_op(&e, 1, &a, "pull", json!({})).await;
        assert_eq!(until(&mut wa, GAME, "bosshp").await["pulling"], 1);
        e.advance(0.001);
        pond_op(&e, 1, &a, "pull", json!({})).await;
        assert_eq!(until(&mut wa, GAME, "bossgone").await["fish"], "leviathan");
        let (boss, pulls) =
            e.hub.with_room(ROOM, |v| (v.pond.boss.is_some(), v.pond.pulls.len())).unwrap();
        assert_eq!((boss, pulls), (false, 0));
    }

    #[tokio::test]
    async fn one_catch_cannot_spawn_two_bosses() {
        // `catches == boss_at` is the guard: a boss that dives does not come
        // back on the next frame just because the counter is still a multiple
        // of eight.
        let e = env(4, 1);
        let (a, mut wa) = e.connect(ROOM, 1, "a").await;
        join(&e, 1, &a, &mut wa).await;
        e.hub.with_room(ROOM, |v| v.pond.catches = BOSS_EVERY - 1);
        pond_op(&e, 1, &a, "cast", json!({})).await;
        let cast = until(&mut wa, GAME, "cast").await;
        e.advance(ANY_REEL);
        pond_op(&e, 1, &a, "land", json!({"token": cast["token"]})).await;
        until(&mut wa, GAME, "boss").await;
        let (catches, boss_at) =
            e.hub.with_room(ROOM, |v| (v.pond.catches, v.pond.boss_at)).unwrap();
        assert_eq!((catches, boss_at), (BOSS_EVERY, BOSS_EVERY));
        // Let it dive, then land a fish that does NOT move the counter onto a
        // multiple of eight: no second boss.
        e.advance(BOSS_SECS + 1.0);
        pond_op(&e, 1, &a, "cast", json!({})).await;
        // `expire_boss` runs at the TOP of `cast`, so the dive is pushed before
        // the cast reply and a reader looking for the reply first would skip it.
        until(&mut wa, GAME, "bossgone").await;
        let cast = until(&mut wa, GAME, "cast").await;
        e.advance(ANY_REEL);
        pond_op(&e, 1, &a, "land", json!({"token": cast["token"]})).await;
        until(&mut wa, GAME, "caught").await;
        assert!(drain(&mut wa).iter().all(|(g, ev)| !(g == GAME && ev == "boss")));
        assert_eq!(e.hub.with_room(ROOM, |v| v.pond.boss.is_some()), Some(false));
    }

    #[tokio::test]
    async fn the_boss_hp_scales_with_the_lobby_not_the_room() {
        let e = env(4, 1);
        let (a, mut wa) = e.connect(ROOM, 1, "a").await;
        let (b, mut wb) = e.connect(ROOM, 2, "b").await;
        // In the ROOM but never in the pond lobby; `_wc` is bound rather than
        // dropped so the socket stays joined.
        let (_c, _wc) = e.connect(ROOM, 3, "c").await;
        join(&e, 1, &a, &mut wa).await;
        join(&e, 2, &b, &mut wb).await;
        e.hub.with_room(ROOM, |v| v.pond.catches = BOSS_EVERY - 1);
        pond_op(&e, 1, &a, "cast", json!({})).await;
        let cast = until(&mut wa, GAME, "cast").await;
        e.advance(ANY_REEL);
        pond_op(&e, 1, &a, "land", json!({"token": cast["token"]})).await;
        let boss = until(&mut wa, GAME, "boss").await;
        assert_eq!(boss["boss"]["max"], BOSS_HP_PER_PLAYER * 2); // two in the LOBBY
    }

    // ------------------------------------------------------------- leaving --

    #[tokio::test]
    async fn a_line_left_out_is_lost_for_everyone_on_leave_and_on_a_dropped_socket() {
        // test_pond_line_left_out_is_lost_for_everyone_on_leave_and_disconnect (:199).
        let e = env(4, 1);
        let (a, mut wa) = e.connect(ROOM, 1, "a").await;
        let (b, mut wb) = e.connect(ROOM, 2, "b").await;
        join(&e, 1, &a, &mut wa).await;
        join(&e, 2, &b, &mut wb).await;
        pond_op(&e, 1, &a, "cast", json!({})).await;
        let cast = until(&mut wa, GAME, "cast").await;
        until(&mut wb, GAME, "casting").await;
        pond_op(&e, 1, &a, "leave", json!({})).await;
        // The lost line comes FIRST, then the shared lobby notice: the order is
        // load-bearing (valley.py:1542-1568) and the page depends on it.
        let lost = until(&mut wb, GAME, "lost").await;
        assert_eq!(lost["id"], cast["id"]);
        assert_eq!(lost["user"]["userId"], "a");
        assert_eq!(lost["fish"], cast["fish"]);
        assert_eq!(until(&mut wb, GAME, "lobby").await["left"]["userId"], "a");
        // Leaving also forgets their rate-limit window and their pulls.
        assert_eq!(e.hub.with_room(ROOM, |v| v.pond.ops.contains_key("a")), Some(false));

        // Now the same thing on a dropped socket.
        e.advance(1.1);
        join(&e, 1, &a, &mut wa).await;
        pond_op(&e, 1, &a, "cast", json!({})).await;
        let cast = until(&mut wa, GAME, "cast").await;
        until(&mut wb, GAME, "casting").await;
        e.disconnect(ROOM, 1, &a).await;
        let lost = until(&mut wb, GAME, "lost").await;
        assert_eq!(lost["id"], cast["id"]);
        assert_eq!(lost["user"]["userId"], "a");
    }

    #[tokio::test]
    async fn leaving_with_no_line_out_says_nothing_but_the_lobby_notice() {
        let e = env(4, 1);
        let (a, mut wa) = e.connect(ROOM, 1, "a").await;
        let (b, mut wb) = e.connect(ROOM, 2, "b").await;
        join(&e, 1, &a, &mut wa).await;
        join(&e, 2, &b, &mut wb).await;
        let _ = drain(&mut wb);
        pond_op(&e, 1, &a, "leave", json!({})).await;
        assert_eq!(drain(&mut wb).iter().filter(|(g, _)| g == GAME).map(|(_, ev)| ev.as_str())
                       .collect::<Vec<_>>(),
                   vec!["lobby"]);
    }

    #[tokio::test]
    async fn a_quick_rejoin_is_not_a_fresh_join_but_the_pond_keeps_no_grace() {
        // test_a_quick_rejoin_after_a_socket_drop_is_not_a_fresh_join (:718) is
        // driven through the pond lobby. The shared layer owns the blip rule;
        // what is the POND's is that a blip still costs you the line.
        let e = env(4, 1);
        let (a, mut wa) = e.connect(ROOM, 1, "a").await;
        let (b, mut wb) = e.connect(ROOM, 2, "b").await;
        join(&e, 1, &a, &mut wa).await;
        join(&e, 2, &b, &mut wb).await;
        pond_op(&e, 2, &b, "cast", json!({})).await;
        until(&mut wb, GAME, "cast").await;
        e.disconnect(ROOM, 2, &b).await;
        until(&mut wa, GAME, "lost").await; // the line went with the socket
        let (b, mut wb) = e.connect(ROOM, 3, "b").await;
        pond_op(&e, 3, &b, "join", json!({})).await;
        let m = until_where(&mut wa, GAME, "lobby",
                            |m| m["members"].as_array().is_some_and(|a| a.len() == 2)).await;
        assert_eq!(m["joined"], Value::Null); // a blip, not a fresh join
        // and the pond they come back to has no line of theirs in it.
        assert_eq!(until(&mut wb, GAME, "pond").await["pond"]["casting"], json!([]));
        // Past HOST_GRACE the same return IS a fresh join.
        e.disconnect(ROOM, 3, &b).await;
        e.advance(HOST_GRACE + 1.0);
        let (b, _wb) = e.connect(ROOM, 4, "b").await;
        pond_op(&e, 4, &b, "join", json!({})).await;
        let m = until_where(&mut wa, GAME, "lobby",
                            |m| m["members"].as_array().is_some_and(|a| a.len() == 2)).await;
        assert_eq!(m["joined"]["userId"], "b");
    }

    // ------------------------------------------------------------ audience --

    #[tokio::test]
    async fn a_pond_event_goes_to_the_whole_room() {
        // The pond broadcasts with out.all, so a socket that never joined the
        // lobby still sees the dock -- and the three out.to events do not reach
        // it. Getting this backwards fails silently.
        let e = env(4, SEED_CHEST);
        let (a, mut wa) = e.connect(ROOM, 1, "a").await;
        let (bystander, mut wz) = e.connect(ROOM, 9, "z").await;
        join(&e, 1, &a, &mut wa).await;
        pond_op(&e, 1, &a, "cast", json!({})).await;
        let cast = until(&mut wa, GAME, "cast").await;
        assert_eq!(until(&mut wz, GAME, "casting").await["id"], cast["id"]);
        e.advance(ANY_REEL);
        pond_op(&e, 1, &a, "land", json!({"token": cast["token"], "chest": true})).await;
        assert_eq!(until(&mut wz, GAME, "caught").await["id"], cast["id"]);
        // z never joined, so it saw neither the token nor the loot nor a snapshot.
        let seen: Vec<String> =
            drain(&mut wz).into_iter().filter(|(g, _)| g == GAME).map(|(_, ev)| ev).collect();
        for private in ["cast", "loot", "pond"] {
            assert!(!seen.contains(&private.to_string()), "{private} leaked: {seen:?}");
        }
        // And the bystander still cannot ACT: the lobby gate is the shared
        // layer's, checked before this engine is reached.
        pond_op(&e, 9, &bystander, "cast", json!({})).await;
        assert_eq!(until(&mut wz, GAME, "error").await["error"], "join the lobby first");
        assert_eq!(e.hub.with_room(ROOM, |v| v.lobbies[I_POND].members.len()), Some(1));
    }
}
