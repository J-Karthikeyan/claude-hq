//! The co-op mine run: one shared grid per room, everyone digging the same rock.
//!
//! Ported from `class Mines` in backend/app/valley.py:771-894, the `mines` arm
//! of `handle` (:1125-1131, `start` / `move` / `exit`), the join tail (:1045) and
//! the `mines` arm of `_leave_lobby` (:1549). The constants are valley.py:109.
//!
//! AUDIENCE: the WHOLE ROOM. Every `mines` snapshot and the `minesend` go out
//! through `out.all`, so sockets that never joined this lobby see the run --
//! this is a game people watch. The two exceptions are the join snapshot
//! (`out.to`, the joiner's socket only) and the two loot events: a faint pays
//! out with `out.user` (every socket of that person in this room) and climbing
//! out pays with `out.to` (the socket that asked). That difference is Python's,
//! at valley.py:869 and :877 respectively, and it is pinned below.
//!
//! The shared divergences from Python are listed once in the `valley` module
//! doc; the two that bite here are divergence 2 (no Rust port can reproduce
//! CPython's Mersenne Twister, so the floor LAYOUT is not reproducible and the
//! tests pin the distribution's properties instead of a seeded oracle) and the
//! float-step divergence documented on [`step_axis`], which is this file's own.

use super::{Ctx, Left, Lobby, Out, RoomValley, Seq, I_MINES};
use rand::rngs::StdRng;
use rand::Rng;
use serde_json::{json, Value};

/// The game key on the wire.
pub const GAME: &str = "mines";

/// valley.py:109 -- `MINE_COLS, MINE_ROWS, MINE_MAX_DEPTH = 12, 8, 12`. Signed
/// because every coordinate in here is arithmetic first (`p["x"] + dx` can go to
/// -1) and an index second.
pub const MINE_COLS: i64 = 12;
pub const MINE_ROWS: i64 = 8;
pub const MINE_MAX_DEPTH: i64 = 12;

/// Hearts a digger starts with. Python inlines the 5 (valley.py:805).
const START_HP: i64 = 5;

/// A fresh floor's rock density: `rng.random() < 0.42` per cell
/// (valley.py:776).
const ROCK_CHANCE: f64 = 0.42;

/// The chance a slime takes a step on someone's move (valley.py:855).
const SLIME_MOVE_CHANCE: f64 = 0.5;

/// The cells cleared on every floor so nobody spawns inside rock
/// (valley.py:777). Note it is NOT the spawn list: (1,1) and (10,1) are spawns
/// and are not cleared, so a player can start the run standing in rock -- their
/// first move digs out of it, exactly as in Python.
const CLEARED: [(i64, i64); 5] = [(0, 0), (1, 0), (0, 1), (MINE_COLS - 1, 0), (MINE_COLS - 2, 0)];

/// Where the lobby's nth member starts (valley.py:801), used `i % 8` so a
/// ninth seat (which `MAX_LOBBY` never allows for mines) would double up.
const SPAWNS: [(i64, i64); 8] = [
    (0, 0),
    (MINE_COLS - 1, 0),
    (1, 0),
    (MINE_COLS - 2, 0),
    (0, 1),
    (1, 1),
    (MINE_COLS - 1, 1),
    (MINE_COLS - 2, 1),
];

/// `_loot`'s table (valley.py:816-817), IN PYTHON'S ORDER: (minimum depth,
/// exclusive cut, item). The order is the rule -- the first row whose depth gate
/// passes AND whose cut is beaten wins, so at depth 4 a roll of 0.85 skips gold
/// (which needs depth 5) and falls through to iron, and a roll of 0.96 at depth
/// 3 is quartz rather than amethyst. Sorting this table by cut would change what
/// comes out of the rock.
const LOOT: [(i64, f64, &str); 6] = [
    (8, 0.985, "ruby"),
    (6, 0.97, "emerald"),
    (4, 0.95, "amethyst"),
    (0, 0.9, "quartz"),
    (5, 0.8, "gold"),
    (2, 0.65, "iron"),
];

/// Nothing is drawn for a roll under this (valley.py:812).
const NO_LOOT_BELOW: f64 = 0.45;

// ------------------------------------------------------------------ state --

/// One tile. Python stores the wire strings in the grid itself; this is an enum
/// so the three strings exist in exactly one place, and [`Cell::wire`] is the
/// only thing that may name them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Cell {
    Rock,
    Floor,
    Ladder,
}

impl Cell {
    fn wire(self) -> &'static str {
        match self {
            Cell::Rock => "rock",
            Cell::Floor => "floor",
            Cell::Ladder => "ladder",
        }
    }
}

/// Python's `{"grid", "ladder", "slimes"}` (valley.py:786).
pub struct Floor {
    /// `[MINE_ROWS][MINE_COLS]`, indexed `grid[y][x]` as Python's is.
    pub grid: Vec<Vec<Cell>>,
    /// Where the way down is BURIED. It stays a `rock` on the wire until
    /// somebody digs that exact tile, which is what
    /// `test_mines_coop_loot_goes_to_the_digger` means by
    /// `"ladder" not in str(run["grid"])`.
    pub ladder: (i64, i64),
    pub slimes: Vec<(i64, i64)>,
}

/// One digger. Python's player dict (valley.py:805).
pub struct Player {
    pub x: i64,
    pub y: i64,
    pub hp: i64,
    /// item -> count, in the order the items were first dug: it reaches the wire
    /// through `view()["loot"]` and through the half paid out on a faint, so it
    /// is a [`Seq`] and not a map.
    pub loot: Seq<i64>,
    pub out: bool,
    /// `p.get("displayName") or p.get("handle")` off the lobby's stored profile,
    /// kept as a [`Value`] because Python keeps whatever that expression
    /// produced -- including `None` -- and puts it straight in the snapshot.
    pub name: Value,
}

/// Python's `self.run` dict (valley.py:807).
pub struct Run {
    pub depth: i64,
    pub floor: Floor,
    /// In the lobby's JOIN order, which is load-bearing twice: it decides the
    /// Manhattan tie-break when a slime picks a target (Python's `min` keeps the
    /// first) and it is the key order of `players` and `loot` on the wire.
    pub players: Seq<Player>,
    pub moves: i64,
}

/// Per-room state. One field of [`RoomValley`], in Python's `__init__` order.
#[derive(Default)]
pub struct Mines {
    pub run: Option<Run>,
}

// ------------------------------------------------------- floor generation --

/// Python's `Mines._floor` (valley.py:775-786). THE DRAW ORDER IS THE LAYOUT:
/// `MINE_ROWS * MINE_COLS` rock rolls row-major, then one `choice` over the
/// rocks for the ladder, then `sx` before `sy` for each slime. Reordering any of
/// those gives a different mine for the same generator state.
///
/// Two Python quirks kept on purpose:
///   - the ladder is chosen BEFORE the slimes carve their tiles, so a slime can
///     land on the buried ladder and turn it to `floor`. That ladder can then
///     never be dug (a `floor` tile is walked onto, not mined) and the floor has
///     no way down until someone digs elsewhere -- there is no second chance.
///   - a slime's spawn is not checked against the players' spawns, so a slime
///     can start on top of a digger and bite on their first move.
fn make_floor(depth: i64, rng: &mut StdRng) -> Floor {
    let mut grid: Vec<Vec<Cell>> = Vec::with_capacity(MINE_ROWS as usize);
    for _y in 0..MINE_ROWS {
        let mut row = Vec::with_capacity(MINE_COLS as usize);
        for _x in 0..MINE_COLS {
            // One draw per cell, row-major: Python's nested comprehension.
            row.push(if rng.gen::<f64>() < ROCK_CHANCE { Cell::Rock } else { Cell::Floor });
        }
        grid.push(row);
    }
    for (x, y) in CLEARED {
        grid[y as usize][x as usize] = Cell::Floor;
    }
    // Python builds `[(x, y) for y in rows for x in cols]`, so the list is
    // row-major and `choice`'s index means the same tile it does there.
    let mut rocks = Vec::new();
    for y in 0..MINE_ROWS {
        for x in 0..MINE_COLS {
            if grid[y as usize][x as usize] == Cell::Rock {
                rocks.push((x, y));
            }
        }
    }
    let ladder = pick_ladder(&rocks, rng);
    let mut slimes = Vec::new();
    for _ in 0..slime_count(depth) {
        // `rng.randrange(2, MINE_COLS - 2), rng.randrange(2, MINE_ROWS)`: a
        // Python tuple evaluates left to right, so sx is drawn before sy.
        let sx = rng.gen_range(2..MINE_COLS - 2);
        let sy = rng.gen_range(2..MINE_ROWS);
        grid[sy as usize][sx as usize] = Cell::Floor;
        slimes.push((sx, sy));
    }
    Floor { grid, ladder, slimes }
}

/// `min(1 + depth // 2, 6)` (valley.py:782). `floordiv` rather than `/` only
/// because that is what Python wrote; depth is never negative here, so they
/// agree.
fn slime_count(depth: i64) -> i64 {
    (1 + super::floordiv(depth, 2)).min(6)
}

/// `rng.choice(rocks) if rocks else (MINE_COLS - 1, MINE_ROWS - 1)`
/// (valley.py:780). Its own function so the empty case -- which 96 cells at
/// p = 0.42 will not produce before the heat death of the universe -- is
/// reachable from a test.
fn pick_ladder(rocks: &[(i64, i64)], rng: &mut StdRng) -> (i64, i64) {
    if rocks.is_empty() {
        return (MINE_COLS - 1, MINE_ROWS - 1);
    }
    rocks[rng.gen_range(0..rocks.len())]
}

/// Python's `Mines._loot` (valley.py:811-818), split from its draw so the table
/// itself is testable: one `random()` decides both whether there is anything in
/// the rock and what it is.
fn loot_of(depth: i64, x: f64) -> Option<&'static str> {
    if x < NO_LOOT_BELOW {
        return None;
    }
    for (need, cut, item) in LOOT {
        if depth >= need && x > cut {
            return Some(item);
        }
    }
    Some("copper")
}

fn loot(depth: i64, rng: &mut StdRng) -> Option<&'static str> {
    loot_of(depth, rng.gen::<f64>())
}

// -------------------------------------------------------------- the views --

/// Python's `Mines.view()` (valley.py:788-794) -- `None` when no run is
/// underway, which is what the join snapshot sends. Key order is Python's dict
/// literal order (`depth`, `grid`, `slimes`, `players`, `loot`), and the
/// per-player keys are Python's tuple `("x", "y", "hp", "name", "out")`; both
/// reach the page, and serde_json keeps them only because `preserve_order` is on
/// (Cargo.toml:16).
///
/// Python copies each grid row defensively before handing it out; here the rows
/// are turned into JSON, so there is nothing to alias.
fn view(m: &Mines) -> Value {
    let Some(r) = &m.run else { return Value::Null };
    let grid: Vec<Value> = r
        .floor
        .grid
        .iter()
        .map(|row| Value::Array(row.iter().map(|c| json!(c.wire())).collect()))
        .collect();
    let slimes: Vec<Value> = r.floor.slimes.iter().map(|(x, y)| json!([x, y])).collect();
    json!({
        "depth": r.depth,
        "grid": grid,
        "slimes": slimes,
        "players": r.players.to_object(|p| json!({"x": p.x, "y": p.y, "hp": p.hp,
                                                  "name": p.name, "out": p.out})),
        "loot": r.players.to_object(|p| p.loot.to_object(|n| json!(n))),
    })
}

/// `p.get("displayName") or p.get("handle")` over the lobby's stored
/// `Member::public()` (valley.py:805). Python TRUTHINESS, not `is not None`, so
/// an EMPTY display name falls through to the handle -- `valley::is_true` is the
/// `is True` identity check and is not this. The stored value is always a
/// `public()` dict, whose `displayName` and `handle` are strings, so the only
/// falsy case reachable here is `""`; a missing key gives null, as `.get` does.
fn display_or_handle(p: &Value) -> Value {
    match p.get("displayName") {
        Some(Value::String(s)) if !s.is_empty() => json!(s),
        _ => p.get("handle").cloned().unwrap_or(Value::Null),
    }
}

/// What Python's f-string prints for `p["name"]` in the two notes: a str goes in
/// bare, and a profile with no name at all prints the word "None".
fn name_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => "None".to_string(),
        // Not reachable through Member::public(); Python would print repr-ish
        // text here and so does this.
        other => other.to_string(),
    }
}

// ----------------------------------------------------------------- the ops --

/// Python's `Mines.start` (valley.py:796-809). Seats EVERY member of the lobby,
/// in join order, whether or not they asked to play.
fn start(m: &mut Mines, cx: &mut Ctx, lobby: &Lobby, out: &mut Out) {
    if m.run.is_some() {
        out.err(cx.conn, "a run is already underway");
        return;
    }
    let mut players = Seq::new();
    for (i, (uid, p)) in lobby.members.iter().enumerate() {
        let (x, y) = SPAWNS[i % SPAWNS.len()];
        players.set(uid, Player { x, y, hp: START_HP, loot: Seq::new(), out: false,
                                  name: display_or_handle(p) });
    }
    // The players are built before the floor, so the generator's first draws are
    // the floor's -- see make_floor on why the draw order is the layout.
    m.run = Some(Run { depth: 1, floor: make_floor(1, cx.rng), players, moves: 0 });
    out.all("mines", json!({"run": view(m), "by": cx.me()}));
}

/// One axis of a `move`. Python compares the raw JSON value against the four
/// step tuples, and `==` there is PYTHON equality, so `true` counts as 1 and
/// `false` as 0 (`bool` is an `int` subclass) -- a page sending
/// `{"dx": true, "dy": false}` really does step east, and this accepts it.
///
/// DIVERGENCE, this file's own. Python's membership test also accepts a FLOAT
/// `1.0`, because `1.0 == 1`; two lines later it indexes `f["grid"][ny][nx]`
/// with that float and raises `TypeError: list indices must be integers`, which
/// escapes `valley.handle` and `routes/rooms.py:120`, sends nothing at all and
/// tears the socket down. That is a crash, not a protocol, so a float step is
/// refused here with the same "move one tile" its own branch already gives for a
/// bad step. Nothing else about the step differs: an out-of-range int (`dx: 5`)
/// is refused by the tuple test in both, as are a string, a null and a missing
/// key.
fn step_axis(v: Option<&Value>) -> Option<i64> {
    match v {
        Some(Value::Bool(b)) => Some(*b as i64),
        // `as_i64` is None for a float, which is the divergence above, and for
        // an integer too large for i64, which the tuple test would refuse anyway.
        Some(Value::Number(n)) => n.as_i64(),
        _ => None,
    }
}

/// Python's `(dx, dy) not in ((1, 0), (-1, 0), (0, 1), (0, -1))`
/// (valley.py:826). Note `(0, 0)` is NOT a step: standing still is "move one
/// tile", not a no-op.
fn step_of(msg: &Value) -> Option<(i64, i64)> {
    let d = (step_axis(msg.get("dx"))?, step_axis(msg.get("dy"))?);
    matches!(d, (1, 0) | (-1, 0) | (0, 1) | (0, -1)).then_some(d)
}

/// Python's `Mines.move` (valley.py:820-873).
fn do_move(m: &mut Mines, cx: &mut Ctx, msg: &Value, out: &mut Out) {
    let uid = cx.uid().to_string();
    // `if not p or p["out"] or p["hp"] <= 0` -- and it is checked BEFORE the
    // step, so a fainted player sending a nonsense step hears about the run and
    // not about the step.
    let playing = m
        .run
        .as_ref()
        .and_then(|r| r.players.get(&uid))
        .map(|p| !p.out && p.hp > 0)
        .unwrap_or(false);
    if !playing {
        out.err(cx.conn, "you are not in this run");
        return;
    }
    let Some((dx, dy)) = step_of(msg) else {
        out.err(cx.conn, "move one tile");
        return;
    };
    let r = m.run.as_mut().expect("checked above");
    let (px, py) = {
        let p = r.players.get(&uid).expect("checked above");
        (p.x, p.y)
    };
    let (nx, ny) = (px + dx, py + dy);
    if !(0..MINE_COLS).contains(&nx) || !(0..MINE_ROWS).contains(&ny) {
        return; // off the grid: Python returns, so NOTHING is sent at all
    }
    let cell = r.floor.grid[ny as usize][nx as usize];
    if cell == Cell::Rock {
        // Digging does NOT move you: the rock becomes floor and you stay put.
        r.floor.grid[ny as usize][nx as usize] = Cell::Floor;
        if (nx, ny) == r.floor.ladder {
            r.floor.grid[ny as usize][nx as usize] = Cell::Ladder;
            // The way down pays nothing: Python's `if/else` skips the loot roll,
            // so this is also one fewer draw from the generator.
        } else if let Some(item) = loot(r.depth, cx.rng) {
            let p = r.players.get_mut(&uid).expect("checked above");
            *p.loot.entry_or(item, || 0) += 1;
        }
    } else if cell == Cell::Ladder && r.depth < MINE_MAX_DEPTH {
        r.depth += 1;
        r.floor = make_floor(r.depth, cx.rng);
        for q in r.players.values_mut() {
            if !q.out && q.hp > 0 {
                q.x = 0; // everyone still standing restarts at the entrance
                q.y = 0;
            }
        }
        let note = format!("{} found the way down: floor {}",
                           name_str(&r.players.get(&uid).expect("checked above").name), r.depth);
        // An early return: the descent costs no move and the slimes of the floor
        // just left never get their turn.
        out.all("mines", json!({"run": view(m), "note": note}));
        return;
    } else {
        // `floor`, or the ladder on the deepest floor (depth == MINE_MAX_DEPTH),
        // which is then just a tile you stand on.
        let p = r.players.get_mut(&uid).expect("checked above");
        p.x = nx;
        p.y = ny;
    }
    r.moves += 1;
    slime_turn(r, cx.rng);
    out.all("mines", json!({"run": view(m)}));
    // Python marks the fainted out AFTER that snapshot, so the frame the room
    // sees still has `out: false` for a player on 0 hearts; the `loot` event is
    // what tells them.
    if let Some(r) = m.run.as_mut() {
        for (uid, q) in r.players.iter_mut() {
            if q.hp <= 0 && !q.out {
                q.out = true;
                // `{k: v // 2 for k, v in loot.items() if v // 2}`: half of each
                // stack, in the same order, and a stack of one pays nothing.
                let half = Value::Object(
                    q.loot
                        .iter()
                        .filter(|(_, n)| **n / 2 != 0)
                        .map(|(k, n)| (k.to_string(), json!(n / 2)))
                        .collect(),
                );
                out.user(uid, "loot", json!({"items": half, "fainted": true}));
            }
        }
    }
    maybe_end(m, out);
}

/// The slime half of a move (valley.py:852-865). Each slime rolls, maybe steps
/// one tile toward the nearest player, and then bites everyone standing on it.
///
/// Four Python details that a tidier loop would lose:
///   - `rng.random() < 0.5 and active` evaluates the DRAW first, so a floor with
///     nobody left still burns one number per slime and the next floor's layout
///     depends on it.
///   - `min(active, key=..)` keeps the FIRST on a tie, and `active` is in join
///     order, so the earliest joiner wins a tie-break.
///   - it steps on X first and only moves on Y when already aligned on X.
///   - the bite loop reads the slime's position LIVE, and a bite shoves the
///     slime one tile east, so a second player on the same tile is compared
///     against the SHOVED position and is usually missed.
fn slime_turn(r: &mut Run, rng: &mut StdRng) {
    // A snapshot, as Python's list comprehension is: someone knocked to 0 hearts
    // by an earlier slime is still in it and can be bitten again by a later one.
    let active: Vec<String> = r
        .players
        .iter()
        .filter(|(_, q)| !q.out && q.hp > 0)
        .map(|(u, _)| u.to_string())
        .collect();
    for si in 0..r.floor.slimes.len() {
        let roll = rng.gen::<f64>();
        if roll < SLIME_MOVE_CHANCE && !active.is_empty() {
            let (sx, sy) = r.floor.slimes[si];
            let mut best: Option<(i64, i64, i64)> = None; // (distance, x, y)
            for u in &active {
                let q = r.players.get(u).expect("active came from players");
                let d = (q.x - sx).abs() + (q.y - sy).abs();
                if best.is_none_or(|(bd, _, _)| d < bd) {
                    best = Some((d, q.x, q.y));
                }
            }
            let (_, tx, ty) = best.expect("active is not empty here");
            let step = |from: i64, to: i64| if to > from { 1 } else if to < from { -1 } else { 0 };
            let mx = sx + step(sx, tx);
            let my = if mx != sx { sy } else { sy + step(sy, ty) };
            if (0..MINE_COLS).contains(&mx)
                && (0..MINE_ROWS).contains(&my)
                && r.floor.grid[my as usize][mx as usize] == Cell::Floor
            {
                r.floor.slimes[si] = (mx, my); // slimes never walk into rock
            }
        }
        for u in &active {
            let s = r.floor.slimes[si];
            let q = r.players.get_mut(u).expect("active came from players");
            if (q.x, q.y) == s {
                q.hp -= 1;
                r.floor.slimes[si].0 = (s.0 + 1).min(MINE_COLS - 1);
            }
        }
    }
}

/// The `exit` op -- Python's `Mines.leave` (valley.py:875-883). The op name on
/// the wire and the method name differ; this one is named for the wire.
fn climb_out(m: &mut Mines, cx: &mut Ctx, out: &mut Out) {
    let uid = cx.uid().to_string();
    let Some(r) = m.run.as_mut() else { return };
    let Some(p) = r.players.get_mut(&uid) else { return };
    if p.out {
        return; // already out: not an error, just nothing
    }
    p.out = true;
    // The WHOLE haul, unlike a faint's half -- and to this SOCKET (`out.to`),
    // not to the person's other tabs.
    let items = p.loot.to_object(|n| json!(n));
    let note = format!("{} climbed out", name_str(&p.name));
    out.to(cx.conn, "loot", json!({"items": items, "fainted": false}));
    out.all("mines", json!({"run": view(m), "note": note}));
    maybe_end(m, out);
}

/// Python's `Mines._maybe_end` (valley.py:885-889): the run ends when every
/// seated player is out, whether they climbed out, fainted or left the lobby.
fn maybe_end(m: &mut Mines, out: &mut Out) {
    let Some(r) = &m.run else { return };
    if r.players.values().all(|q| q.out) {
        out.all("minesend", json!({"depth": r.depth}));
        m.run = None;
    }
}

/// Python's `Mines.drop` (valley.py:891-894). Note what it does NOT do: no loot
/// event. Someone who leaves the lobby mid-descent loses the whole haul, where
/// the same person sending `exit` first keeps it.
fn drop_player(m: &mut Mines, uid: &str, out: &mut Out) {
    let seated = match m.run.as_mut().and_then(|r| r.players.get_mut(uid)) {
        Some(p) => {
            p.out = true;
            true
        }
        None => false,
    };
    if seated {
        maybe_end(m, out);
    }
}

// ------------------------------------------------------------- the contract --

/// The join snapshot: `out.to(member.ws, "mines", run=v.mines.view())`
/// (valley.py:1045). `run` is null when no run is underway, and this goes to the
/// JOINER's socket only -- the one thing in this game that is not room-wide.
pub fn joined(v: &mut RoomValley, cx: &mut Ctx, out: &mut Out) {
    out.to(cx.conn, "mines", json!({"run": view(&v.mines)}));
}

/// This game's ops (valley.py:1125-1131). Reached only past the lobby gate. An
/// unrecognised op produces NOTHING, as Python's `else`-less if/elif chain does.
pub fn op(v: &mut RoomValley, cx: &mut Ctx, op: &str, msg: &Value, out: &mut Out) {
    match op {
        // `start` reads the lobby to seat it; the two borrows are disjoint
        // fields of the one RoomValley.
        "start" => start(&mut v.mines, cx, &v.lobbies[I_MINES], out),
        "move" => do_move(&mut v.mines, cx, msg, out),
        "exit" => climb_out(&mut v.mines, cx, out),
        _ => {}
    }
}

/// Someone left this game's lobby, by `leave` or by a dropped socket
/// (valley.py:1549). Mines makes no distinction between the two -- there is no
/// grace period and no held seat, so a socket blip during a descent costs the
/// run -- and it never suppresses the `left` notice.
pub fn dropped(
    v: &mut RoomValley, uid: &str, who: &Value, out: &mut Out, t: f64, disconnected: bool,
) -> Left {
    let _ = (who, t, disconnected); // Python's `mines` arm reads none of these
    drop_player(&mut v.mines, uid, out);
    Left::Notice
}

#[cfg(test)]
mod tests {
    use super::super::testkit::*;
    use super::super::RoomValley;
    use super::*;
    use crate::rooms::Member;
    use rand::SeedableRng;

    /// One socket's queue, as the testkit hands it over.
    type Rx = tokio::sync::mpsc::Receiver<String>;

    const ROOM: &str = "mines-room";
    const SEED: u64 = 7;

    fn dice(seed: u64) -> StdRng {
        StdRng::seed_from_u64(seed)
    }

    /// The per-message generator the harness hands every op, so a test can work
    /// out what its next draw will be.
    fn first_roll(seed: u64) -> f64 {
        dice(seed).gen::<f64>()
    }

    /// Connect a socket and join this lobby with it; the call order is the join
    /// order, which the run's seating and the slimes' tie-break both follow.
    /// Its own queue is drained afterwards, because the join tail leaves a
    /// `mines` frame with a null run on it and [`until`] would hand that to the
    /// next assertion instead of the frame the test is waiting for.
    async fn seat(e: &Env, conn: u64, uid: &str) -> (Member, Rx) {
        let (m, mut rx) = e.connect(ROOM, conn, uid).await;
        e.send(ROOM, conn, &m, GAME, "join", json!({})).await;
        let _ = drain(&mut rx);
        (m, rx)
    }

    /// Reach into the live run, the way the Python tests reach `v.mines.run`.
    fn rig(e: &Env, f: impl FnOnce(&mut Run)) {
        e.hub
            .with_room(ROOM, |v| f(v.mines.run.as_mut().expect("a run is underway")))
            .expect("a room valley");
    }

    fn run_depth(e: &Env) -> Option<i64> {
        e.hub.with_room(ROOM, |v| v.mines.run.as_ref().map(|r| r.depth)).flatten()
    }

    fn moves(e: &Env) -> Option<i64> {
        e.hub.with_room(ROOM, |v| v.mines.run.as_ref().map(|r| r.moves)).flatten()
    }

    /// (x, y, hp, out) of one seated digger.
    fn player(e: &Env, uid: &str) -> (i64, i64, i64, bool) {
        e.hub
            .with_room(ROOM, |v| {
                let p = v.mines.run.as_ref().unwrap().players.get(uid).expect("seated");
                (p.x, p.y, p.hp, p.out)
            })
            .unwrap()
    }

    /// An empty floor with no slimes: every move is then a plain walk, and the
    /// only randomness left is the loot roll.
    fn open_floor(r: &mut Run) {
        for row in r.floor.grid.iter_mut() {
            for c in row.iter_mut() {
                *c = Cell::Floor;
            }
        }
        r.floor.slimes.clear();
    }

    /// A run built by hand, for the slime rules, where driving the hub would put
    /// a seeded floor between the test and the thing it is pinning.
    fn bare_run(seats: &[(&str, i64, i64)], slimes: Vec<(i64, i64)>) -> Run {
        let mut players = Seq::new();
        for (uid, x, y) in seats {
            players.set(uid, Player { x: *x, y: *y, hp: START_HP, loot: Seq::new(), out: false,
                                      name: json!(uid) });
        }
        Run {
            depth: 1,
            floor: Floor { grid: vec![vec![Cell::Floor; MINE_COLS as usize]; MINE_ROWS as usize],
                           ladder: (0, MINE_ROWS - 1), slimes },
            players,
            moves: 0,
        }
    }

    /// A seed whose first draw moves a slime, and one whose first draw does not.
    fn mover() -> u64 {
        (0..64u64).find(|s| first_roll(*s) < SLIME_MOVE_CHANCE).expect("a seed that moves")
    }

    fn stayer() -> u64 {
        (0..64u64).find(|s| first_roll(*s) >= SLIME_MOVE_CHANCE).expect("a seed that stays")
    }


    // ------------------------------------------------------ the floor itself --

    #[test]
    fn the_floor_is_pythons_grid_with_the_entrance_always_dug_out() {
        for seed in 0..40u64 {
            let f = make_floor(1, &mut dice(seed));
            assert_eq!(f.grid.len(), MINE_ROWS as usize);
            assert!(f.grid.iter().all(|row| row.len() == MINE_COLS as usize));
            for (x, y) in CLEARED {
                assert_eq!(f.grid[y as usize][x as usize], Cell::Floor);
            }
            // Nothing is a ladder until it is dug: the Python test asserts
            // `"ladder" not in str(run["grid"])`.
            assert!(f.grid.iter().flatten().all(|c| *c != Cell::Ladder));
            // The ladder is buried in a rock -- unless a slime carved that very
            // tile afterwards, which is the quirk make_floor documents.
            let (lx, ly) = f.ladder;
            assert!(f.grid[ly as usize][lx as usize] == Cell::Rock
                        || f.slimes.contains(&(lx, ly)),
                    "seed {seed}: the ladder is neither rock nor carved by a slime");
            // Neither extreme happens with 96 cells at p = 0.42, so this also
            // proves the per-cell rolls vary rather than one draw being reused.
            let rocks = f.grid.iter().flatten().filter(|c| **c == Cell::Rock).count();
            assert!((10..90).contains(&rocks), "seed {seed}: {rocks} rocks");
        }
    }

    #[test]
    fn a_deeper_floor_has_more_slimes_up_to_six() {
        // min(1 + depth // 2, 6), valley.py:782.
        assert_eq!((1..=12).map(slime_count).collect::<Vec<_>>(),
                   vec![1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 6]);
        for depth in [1i64, 4, 12] {
            let f = make_floor(depth, &mut dice(depth as u64));
            assert_eq!(f.slimes.len(), slime_count(depth) as usize);
            for (sx, sy) in &f.slimes {
                // randrange(2, MINE_COLS - 2) and randrange(2, MINE_ROWS).
                assert!((2..MINE_COLS - 2).contains(sx), "slime x {sx}");
                assert!((2..MINE_ROWS).contains(sy), "slime y {sy}");
                // Its own tile is carved out, so it has somewhere to stand.
                assert_eq!(f.grid[*sy as usize][*sx as usize], Cell::Floor);
            }
        }
    }

    #[test]
    fn a_floor_of_solid_air_puts_the_ladder_in_the_far_corner() {
        // valley.py:780's `else` arm, which no realistic roll reaches.
        assert_eq!(pick_ladder(&[], &mut dice(1)), (MINE_COLS - 1, MINE_ROWS - 1));
        assert_eq!(pick_ladder(&[(3, 4)], &mut dice(1)), (3, 4));
    }

    #[test]
    fn the_loot_table_is_read_in_pythons_order_not_by_value() {
        // valley.py:811-818. The cut is exclusive and the depth gate comes
        // first, so the ROW ORDER decides the item.
        assert_eq!(loot_of(1, 0.0), None);
        assert_eq!(loot_of(12, 0.4499), None);
        assert_eq!(loot_of(12, NO_LOOT_BELOW), Some("copper")); // the floor is `<`
        assert_eq!(loot_of(1, 0.5), Some("copper"));
        assert_eq!(loot_of(1, 0.99), Some("quartz")); // quartz has no depth gate
        assert_eq!(loot_of(2, 0.66), Some("iron"));
        assert_eq!(loot_of(1, 0.66), Some("copper")); // iron needs depth 2
        assert_eq!(loot_of(2, 0.65), Some("copper")); // the cut is `>`, not `>=`
        assert_eq!(loot_of(5, 0.85), Some("gold"));
        assert_eq!(loot_of(4, 0.85), Some("iron")); // gold needs depth 5: fall through
        assert_eq!(loot_of(4, 0.96), Some("amethyst"));
        assert_eq!(loot_of(3, 0.96), Some("quartz")); // amethyst needs depth 4
        assert_eq!(loot_of(6, 0.98), Some("emerald"));
        assert_eq!(loot_of(8, 0.99), Some("ruby"));
        assert_eq!(loot_of(7, 0.99), Some("emerald")); // ruby needs depth 8
    }

    // -------------------------------------------------------------- the ops --

    #[tokio::test]
    async fn a_start_seats_the_whole_lobby_in_join_order_on_pythons_spawns() {
        let e = env(4, SEED);
        let (a, _wa) = seat(&e, 1, "a").await;
        let (_b, _wb) = seat(&e, 2, "b").await;
        let (_c, _wc) = seat(&e, 3, "c").await;
        let (_watch, mut ww) = e.connect(ROOM, 9, "watch").await;
        e.send(ROOM, 1, &a, GAME, "start", json!({})).await;
        let m = until(&mut ww, GAME, "mines").await;
        let run = &m["run"];
        assert_eq!(run["depth"], 1);
        assert_eq!(m["by"]["userId"], "a"); // `by` is whoever pressed start
        // Join order, on spawns[0..3] = (0,0), (11,0), (1,0).
        assert_eq!(run["players"].as_object().unwrap().keys().collect::<Vec<_>>(),
                   vec!["a", "b", "c"]);
        for (uid, x, y) in [("a", 0, 0), ("b", MINE_COLS - 1, 0), ("c", 1, 0)] {
            assert_eq!(run["players"][uid]["x"], x);
            assert_eq!(run["players"][uid]["y"], y);
            assert_eq!(run["players"][uid]["out"], false);
            assert_eq!(run["players"][uid]["name"], uid); // the displayName
            assert_eq!(run["loot"][uid], json!({}));
        }
        // The Python test's own two assertions.
        assert_eq!(run["players"]["a"]["hp"], 5);
        assert!(!run["grid"].to_string().contains("ladder"));
    }

    #[tokio::test]
    async fn a_start_and_every_snapshot_goes_to_the_whole_room() {
        // The audience rule for this engine: `out.all`, not `out.lobby`.
        let e = env(4, SEED);
        let (a, _wa) = seat(&e, 1, "a").await;
        let (_bystander, mut wb) = e.connect(ROOM, 2, "bystander").await; // never joined mines
        e.send(ROOM, 1, &a, GAME, "start", json!({})).await;
        assert_eq!(until(&mut wb, GAME, "mines").await["by"]["userId"], "a");
        rig(&e, open_floor);
        e.send(ROOM, 1, &a, GAME, "move", json!({"dx": 1, "dy": 0})).await;
        assert_eq!(until(&mut wb, GAME, "mines").await["run"]["players"]["a"]["x"], 1);
    }

    #[tokio::test]
    async fn a_second_start_is_refused_while_a_run_is_underway() {
        let e = env(4, SEED);
        let (a, mut wa) = seat(&e, 1, "a").await;
        e.send(ROOM, 1, &a, GAME, "start", json!({})).await;
        until(&mut wa, GAME, "mines").await;
        e.send(ROOM, 1, &a, GAME, "start", json!({})).await;
        assert_eq!(until(&mut wa, GAME, "error").await["error"], "a run is already underway");
    }

    #[tokio::test]
    async fn the_join_snapshot_shows_the_run_or_null_and_only_to_the_joiner() {
        let e = env(4, SEED);
        // Not `seat`: this test wants the join tail itself, which seat drains.
        let (a, mut wa) = e.connect(ROOM, 1, "a").await;
        e.send(ROOM, 1, &a, GAME, "join", json!({})).await;
        assert_eq!(until(&mut wa, GAME, "mines").await["run"], Value::Null);
        e.send(ROOM, 1, &a, GAME, "start", json!({})).await;
        until(&mut wa, GAME, "mines").await;
        let _ = drain(&mut wa);
        // b joining mid-run sees the run, and a is not told again: the join
        // snapshot is `out.to`, the one frame here that is not room-wide.
        let (b, mut wb) = e.connect(ROOM, 2, "b").await;
        e.send(ROOM, 2, &b, GAME, "join", json!({})).await;
        assert_eq!(until(&mut wb, GAME, "mines").await["run"]["depth"], 1);
        assert_eq!(drain(&mut wa), vec![(GAME.to_string(), "lobby".to_string())]);
        // ... and b is a spectator: the run seated only who was in at `start`.
        let seated = e
            .hub
            .with_room(ROOM, |v| v.mines.run.as_ref().unwrap().players.contains_key("b"))
            .unwrap();
        assert!(!seated);
    }

    #[tokio::test]
    async fn an_unrecognised_mines_op_says_nothing_at_all() {
        let e = env(4, SEED);
        let (a, mut wa) = seat(&e, 1, "a").await;
        let _ = drain(&mut wa);
        e.send(ROOM, 1, &a, GAME, "dig", json!({})).await;
        assert!(drain(&mut wa).is_empty());
    }

    // ------------------------------------------------------------ the moves --

    #[tokio::test]
    async fn a_step_must_be_one_tile_and_a_bool_counts_as_one() {
        let e = env(4, SEED);
        let (a, mut wa) = seat(&e, 1, "a").await;
        e.send(ROOM, 1, &a, GAME, "start", json!({})).await;
        until(&mut wa, GAME, "mines").await;
        rig(&e, open_floor);
        // The Python test's own case (dx 5), plus the rest of the refusals:
        // (0,0) is not a step, a diagonal is not a step, a float is this port's
        // divergence, and a missing or non-numeric axis never was one.
        for bad in [json!({"dx": 5, "dy": 0}), json!({"dx": 0, "dy": 0}),
                    json!({"dx": 1, "dy": 1}), json!({"dx": 1.0, "dy": 0}),
                    json!({"dx": 1}), json!({"dx": "1", "dy": 0}),
                    json!({"dx": Value::Null, "dy": 0}), json!({})] {
            e.send(ROOM, 1, &a, GAME, "move", bad.clone()).await;
            assert_eq!(until(&mut wa, GAME, "error").await["error"], "move one tile",
                       "{bad} should not be a step");
        }
        assert_eq!(player(&e, "a"), (0, 0, START_HP, false)); // nothing moved
        // `true == 1` and `false == 0` in Python, so this really is a step east.
        e.send(ROOM, 1, &a, GAME, "move", json!({"dx": true, "dy": false})).await;
        until(&mut wa, GAME, "mines").await;
        assert_eq!(player(&e, "a").0, 1);
    }

    #[tokio::test]
    async fn only_a_live_digger_can_move() {
        let e = env(4, SEED);
        let (a, mut wa) = seat(&e, 1, "a").await;
        let (_b, _wb) = seat(&e, 2, "b").await;
        // No run at all.
        e.send(ROOM, 1, &a, GAME, "move", json!({"dx": 1, "dy": 0})).await;
        assert_eq!(until(&mut wa, GAME, "error").await["error"], "you are not in this run");
        e.send(ROOM, 1, &a, GAME, "start", json!({})).await;
        until(&mut wa, GAME, "mines").await;
        // Out of the run, and on zero hearts: both answer the same thing, and
        // the guard is checked BEFORE the step is even looked at.
        rig(&e, |r| r.players.get_mut("a").unwrap().out = true);
        e.send(ROOM, 1, &a, GAME, "move", json!({"dx": 99, "dy": 0})).await;
        assert_eq!(until(&mut wa, GAME, "error").await["error"], "you are not in this run");
        rig(&e, |r| {
            let p = r.players.get_mut("a").unwrap();
            p.out = false;
            p.hp = 0;
        });
        e.send(ROOM, 1, &a, GAME, "move", json!({"dx": 1, "dy": 0})).await;
        assert_eq!(until(&mut wa, GAME, "error").await["error"], "you are not in this run");
    }

    #[tokio::test]
    async fn a_step_off_the_grid_is_answered_with_nothing() {
        let e = env(4, SEED);
        let (a, mut wa) = seat(&e, 1, "a").await;
        e.send(ROOM, 1, &a, GAME, "start", json!({})).await;
        until(&mut wa, GAME, "mines").await;
        rig(&e, open_floor);
        let _ = drain(&mut wa);
        for off in [json!({"dx": -1, "dy": 0}), json!({"dx": 0, "dy": -1})] {
            e.send(ROOM, 1, &a, GAME, "move", off).await;
            assert!(drain(&mut wa).is_empty()); // no error, no snapshot
        }
        assert_eq!(player(&e, "a"), (0, 0, START_HP, false));
        assert_eq!(moves(&e), Some(0)); // the move was not counted either
    }

    #[tokio::test]
    async fn digging_a_rock_clears_it_without_moving_you_and_pays_the_digger() {
        let e = env(4, SEED);
        let (a, mut wa) = seat(&e, 1, "a").await;
        let (_b, _wb) = seat(&e, 2, "b").await;
        e.send(ROOM, 1, &a, GAME, "start", json!({})).await;
        until(&mut wa, GAME, "mines").await;
        rig(&e, |r| {
            open_floor(r);
            r.floor.grid[0][1] = Cell::Rock;
            r.floor.ladder = (9, 7); // not the tile we are about to dig
        });
        e.send(ROOM, 1, &a, GAME, "move", json!({"dx": 1, "dy": 0})).await;
        let m = until(&mut wa, GAME, "mines").await;
        assert_eq!(m["run"]["grid"][0][1], "floor");
        assert_eq!(m["run"]["players"]["a"]["x"], 0); // digging does not move you
        assert_eq!(m["run"]["players"]["a"]["y"], 0);
        assert_eq!(moves(&e), Some(1));
        // One loot roll, and with no slimes on the floor it is the per-message
        // generator's FIRST draw, so the table says exactly what came out.
        let expect = loot_of(1, first_roll(SEED));
        let got = m["run"]["loot"]["a"].clone();
        match expect {
            Some(item) => assert_eq!(got, json!({item: 1})),
            None => assert_eq!(got, json!({})),
        }
        assert_eq!(m["run"]["loot"]["b"], json!({})); // never the other digger
    }

    #[tokio::test]
    async fn the_way_down_shows_up_only_when_it_is_dug_and_pays_nothing() {
        let e = env(4, SEED);
        let (a, mut wa) = seat(&e, 1, "a").await;
        e.send(ROOM, 1, &a, GAME, "start", json!({})).await;
        until(&mut wa, GAME, "mines").await;
        rig(&e, |r| {
            open_floor(r);
            r.floor.grid[0][1] = Cell::Rock;
            r.floor.ladder = (1, 0); // the tile we are about to dig IS the way down
        });
        e.send(ROOM, 1, &a, GAME, "move", json!({"dx": 1, "dy": 0})).await;
        let m = until(&mut wa, GAME, "mines").await;
        assert_eq!(m["run"]["grid"][0][1], "ladder");
        assert_eq!(m["run"]["loot"]["a"], json!({})); // the way down pays nothing
        assert_eq!(m["run"]["depth"], 1); // digging it is not descending it
    }

    #[tokio::test]
    async fn the_ladder_takes_the_whole_party_down_to_the_entrance_of_a_new_floor() {
        let e = env(4, SEED);
        let (a, mut wa) = seat(&e, 1, "a").await;
        let (_b, _wb) = seat(&e, 2, "b").await;
        let (_c, _wc) = seat(&e, 3, "c").await;
        e.send(ROOM, 1, &a, GAME, "start", json!({})).await;
        until(&mut wa, GAME, "mines").await;
        rig(&e, |r| {
            open_floor(r);
            r.floor.grid[0][1] = Cell::Ladder;
            let b = r.players.get_mut("b").unwrap();
            b.x = 5; // somewhere else on the floor
            b.y = 4;
            let c = r.players.get_mut("c").unwrap();
            c.out = true; // already climbed out
            c.x = 7;
        });
        e.send(ROOM, 1, &a, GAME, "move", json!({"dx": 1, "dy": 0})).await;
        let m = until(&mut wa, GAME, "mines").await;
        assert_eq!(m["note"], "a found the way down: floor 2");
        assert_eq!(m["run"]["depth"], 2);
        assert_eq!(m["run"]["slimes"].as_array().unwrap().len(), 2); // slime_count(2)
        // Everyone still standing restarts at (0, 0) -- including the digger,
        // who never stepped onto the ladder.
        assert_eq!(player(&e, "a"), (0, 0, START_HP, false));
        assert_eq!(player(&e, "b"), (0, 0, START_HP, false));
        assert_eq!(player(&e, "c"), (7, 0, START_HP, true)); // out: left where they were
        // The descent is free: no move counted, and the floor just left never
        // got its slime turn.
        assert_eq!(moves(&e), Some(0));
    }

    #[tokio::test]
    async fn the_deepest_floor_has_no_way_down_and_the_ladder_is_just_a_tile() {
        let e = env(4, SEED);
        let (a, mut wa) = seat(&e, 1, "a").await;
        e.send(ROOM, 1, &a, GAME, "start", json!({})).await;
        until(&mut wa, GAME, "mines").await;
        rig(&e, |r| {
            open_floor(r);
            r.depth = MINE_MAX_DEPTH;
            r.floor.grid[0][1] = Cell::Ladder;
        });
        e.send(ROOM, 1, &a, GAME, "move", json!({"dx": 1, "dy": 0})).await;
        let m = until(&mut wa, GAME, "mines").await;
        assert_eq!(m["run"]["depth"], MINE_MAX_DEPTH);
        assert!(m.get("note").is_none());
        assert_eq!(player(&e, "a"), (1, 0, START_HP, false)); // they stand on it
        assert_eq!(moves(&e), Some(1)); // and it cost a move, unlike a descent
    }

    // ----------------------------------------------------------- the slimes --

    #[tokio::test]
    async fn a_slime_on_your_tile_takes_a_heart_and_is_shoved_east() {
        let e = env(4, SEED);
        let (a, mut wa) = seat(&e, 1, "a").await;
        e.send(ROOM, 1, &a, GAME, "start", json!({})).await;
        until(&mut wa, GAME, "mines").await;
        // The slime sits on the tile a steps onto. Whatever the roll says, its
        // target is then on its own tile, so its step is a no-op and the bite is
        // certain: no seeded oracle needed.
        rig(&e, |r| {
            open_floor(r);
            r.floor.slimes.push((1, 0));
            let p = r.players.get_mut("a").unwrap();
            p.x = 0;
            p.y = 0;
        });
        e.send(ROOM, 1, &a, GAME, "move", json!({"dx": 1, "dy": 0})).await;
        let m = until(&mut wa, GAME, "mines").await;
        assert_eq!(m["run"]["players"]["a"]["hp"], START_HP - 1);
        assert_eq!(m["run"]["slimes"][0], json!([2, 0])); // min(x + 1, MINE_COLS - 1)
    }

    #[test]
    fn a_slime_steps_on_x_first_and_never_into_rock() {
        // Driven directly, so the roll is this test's business: a seed whose
        // first draw is under 0.5 moves, the rest stay.
        let mut r = bare_run(&[("a", 5, 5)], vec![(2, 2)]);
        slime_turn(&mut r, &mut dice(mover()));
        assert_eq!(r.floor.slimes[0], (3, 2)); // x closes first, y waits
        let mut r = bare_run(&[("a", 5, 5)], vec![(2, 2)]);
        slime_turn(&mut r, &mut dice(stayer()));
        assert_eq!(r.floor.slimes[0], (2, 2)); // the roll said no
        // Rock blocks it even when the roll says yes.
        let mut r = bare_run(&[("a", 5, 5)], vec![(2, 2)]);
        r.floor.grid[2][3] = Cell::Rock;
        slime_turn(&mut r, &mut dice(mover()));
        assert_eq!(r.floor.slimes[0], (2, 2));
        // Already aligned on x, it closes on y.
        let mut r = bare_run(&[("a", 5, 5)], vec![(5, 2)]);
        slime_turn(&mut r, &mut dice(mover()));
        assert_eq!(r.floor.slimes[0], (5, 3));
    }

    #[test]
    fn a_slime_rolls_even_when_there_is_nobody_left_to_chase() {
        // `rng.random() < 0.5 and active` draws FIRST, so the number is spent
        // whether or not anyone is standing -- and the next floor's layout
        // depends on it.
        let mut r = bare_run(&[("a", 5, 5)], vec![(2, 2)]);
        r.players.get_mut("a").unwrap().out = true;
        let mut g = dice(mover());
        slime_turn(&mut r, &mut g);
        assert_eq!(r.floor.slimes[0], (2, 2)); // nobody to walk toward
        let mut spent = dice(mover());
        let _: f64 = spent.gen();
        assert_eq!(g.gen::<f64>(), spent.gen::<f64>()); // exactly one draw taken
    }

    #[test]
    fn a_manhattan_tie_goes_to_the_earliest_joiner() {
        // Python's `min(active, key=..)` keeps the FIRST, and `active` follows
        // the players' join order.
        let mut r = bare_run(&[("first", 2, 0), ("second", 0, 2)], vec![(0, 0)]);
        slime_turn(&mut r, &mut dice(mover()));
        assert_eq!(r.floor.slimes[0], (1, 0)); // toward "first", not "second"
    }

    #[test]
    fn one_slime_bites_everyone_on_its_tile_but_the_shove_usually_saves_the_second() {
        // The bite loop re-reads the slime's position, and the first bite shoves
        // it east, so a second player on the same tile is compared against the
        // MOVED slime and goes free.
        let mut r = bare_run(&[("a", 4, 4), ("b", 4, 4)], vec![(4, 4)]);
        slime_turn(&mut r, &mut dice(stayer())); // no step: the bites are the turn
        assert_eq!(r.players.get("a").unwrap().hp, START_HP - 1);
        assert_eq!(r.players.get("b").unwrap().hp, START_HP); // spared by the shove
        assert_eq!(r.floor.slimes[0], (5, 4));
        // On the last column there is nowhere to shove it to, so it keeps
        // biting: min(MINE_COLS - 1, x + 1) is a clamp, not a wrap.
        let mut r = bare_run(&[("a", MINE_COLS - 1, 4), ("b", MINE_COLS - 1, 4)],
                             vec![(MINE_COLS - 1, 4)]);
        slime_turn(&mut r, &mut dice(stayer()));
        assert_eq!(r.players.get("a").unwrap().hp, START_HP - 1);
        assert_eq!(r.players.get("b").unwrap().hp, START_HP - 1);
        assert_eq!(r.floor.slimes[0], (MINE_COLS - 1, 4));
    }

    #[tokio::test]
    async fn a_digger_on_zero_hearts_is_out_with_half_of_each_stack() {
        let e = env(4, SEED);
        let (a, mut wa) = seat(&e, 1, "a").await;
        let (_a2, mut wa2) = e.connect(ROOM, 5, "a").await; // the same person, second tab
        e.send(ROOM, 1, &a, GAME, "start", json!({})).await;
        until(&mut wa, GAME, "mines").await;
        let _ = drain(&mut wa);
        let _ = drain(&mut wa2);
        rig(&e, |r| {
            open_floor(r);
            r.floor.slimes.push((1, 0));
            let p = r.players.get_mut("a").unwrap();
            p.hp = 1;
            for (item, n) in [("iron", 3), ("copper", 1), ("gold", 5)] {
                p.loot.set(item, n);
            }
        });
        e.send(ROOM, 1, &a, GAME, "move", json!({"dx": 1, "dy": 0})).await;
        // The snapshot goes out BEFORE the faint is recorded, so it still says
        // out: false on zero hearts.
        let snap = until(&mut wa, GAME, "mines").await;
        assert_eq!(snap["run"]["players"]["a"]["hp"], 0);
        assert_eq!(snap["run"]["players"]["a"]["out"], false);
        let loot = until(&mut wa, GAME, "loot").await;
        assert_eq!(loot["fainted"], true);
        // Half of each stack, in the order they were dug, and a stack of one
        // pays nothing at all.
        assert_eq!(loot["items"].to_string(), r#"{"iron":1,"gold":2}"#);
        // `out.user`: every socket of that person in this room, this tab too.
        assert_eq!(until(&mut wa2, GAME, "loot").await["fainted"], true);
        // The only digger is out, so the run ends in the same flush -- which is
        // why there is no run left to read `out: true` off.
        assert_eq!(until(&mut wa, GAME, "minesend").await["depth"], 1);
        assert!(run_depth(&e).is_none());
    }

    // ------------------------------------------------ climbing out, leaving --

    #[tokio::test]
    async fn climbing_out_hands_back_the_whole_haul_and_ends_a_solo_run() {
        // The Python test's tail: a loot with fainted false and an items
        // object, then minesend.
        let e = env(4, SEED);
        let (a, mut wa) = seat(&e, 1, "a").await;
        let (_a2, mut wa2) = e.connect(ROOM, 5, "a").await; // the same person, second tab
        e.send(ROOM, 1, &a, GAME, "start", json!({})).await;
        until(&mut wa, GAME, "mines").await;
        let _ = drain(&mut wa);
        let _ = drain(&mut wa2);
        rig(&e, |r| {
            let p = r.players.get_mut("a").unwrap();
            p.loot.set("iron", 3);
            p.loot.set("copper", 1);
        });
        e.send(ROOM, 1, &a, GAME, "exit", json!({})).await;
        assert_eq!(drain(&mut wa),
                   vec![(GAME.to_string(), "loot".to_string()),
                        (GAME.to_string(), "mines".to_string()),
                        (GAME.to_string(), "minesend".to_string())]);
        // `out.to`, so the other tab gets the room-wide frames and NOT the loot.
        assert_eq!(drain(&mut wa2),
                   vec![(GAME.to_string(), "mines".to_string()),
                        (GAME.to_string(), "minesend".to_string())]);
        assert!(run_depth(&e).is_none());
        // A second exit is not an error, it is nothing at all.
        e.send(ROOM, 1, &a, GAME, "exit", json!({})).await;
        assert!(drain(&mut wa).is_empty());
    }

    #[tokio::test]
    async fn the_exit_loot_is_the_whole_haul_and_the_note_names_the_climber() {
        let e = env(4, SEED);
        let (a, mut wa) = seat(&e, 1, "a").await;
        let (_b, _wb) = seat(&e, 2, "b").await;
        e.send(ROOM, 1, &a, GAME, "start", json!({})).await;
        until(&mut wa, GAME, "mines").await;
        rig(&e, |r| {
            let p = r.players.get_mut("a").unwrap();
            p.loot.set("iron", 3);
            p.loot.set("copper", 1);
        });
        e.send(ROOM, 1, &a, GAME, "exit", json!({})).await;
        let loot = until(&mut wa, GAME, "loot").await;
        assert_eq!(loot["fainted"], false);
        assert_eq!(loot["items"].to_string(), r#"{"iron":3,"copper":1}"#); // not halved
        let m = until(&mut wa, GAME, "mines").await;
        assert_eq!(m["note"], "a climbed out");
        assert_eq!(m["run"]["players"]["a"]["out"], true);
        assert!(run_depth(&e).is_some()); // b is still down there
        // Their haul stays on the snapshot; the run just no longer moves them.
        assert_eq!(m["run"]["loot"]["a"].to_string(), r#"{"iron":3,"copper":1}"#);
    }

    #[tokio::test]
    async fn a_digger_who_leaves_the_lobby_mid_descent_loses_the_haul() {
        let e = env(4, SEED);
        let (a, _wa) = seat(&e, 1, "a").await;
        let (b, _wb) = seat(&e, 2, "b").await;
        let (_watch, mut ww) = e.connect(ROOM, 9, "watch").await;
        e.send(ROOM, 1, &a, GAME, "start", json!({})).await;
        until(&mut ww, GAME, "mines").await;
        rig(&e, |r| r.players.get_mut("a").unwrap().loot.set("ruby", 2));
        let _ = drain(&mut ww);
        // An explicit leave: marked out, NO loot event -- the haul is gone --
        // and the run goes on without them.
        e.send(ROOM, 1, &a, GAME, "leave", json!({})).await;
        assert_eq!(drain(&mut ww), vec![(GAME.to_string(), "lobby".to_string())]);
        assert!(player(&e, "a").3);
        assert!(run_depth(&e).is_some());
        // The last one out ends the run, and the engine's event comes BEFORE the
        // shared lobby event.
        e.send(ROOM, 2, &b, GAME, "leave", json!({})).await;
        assert_eq!(drain(&mut ww),
                   vec![(GAME.to_string(), "minesend".to_string()),
                        (GAME.to_string(), "lobby".to_string())]);
        assert!(run_depth(&e).is_none());
    }

    #[tokio::test]
    async fn a_dropped_socket_ends_the_run_the_same_way_as_a_leave() {
        // Mines holds no seat and grants no grace: a socket blip costs the run.
        let e = env(4, SEED);
        let (a, _wa) = seat(&e, 1, "a").await;
        let (b, _wb) = seat(&e, 2, "b").await;
        let (_watch, mut ww) = e.connect(ROOM, 9, "watch").await;
        e.send(ROOM, 1, &a, GAME, "start", json!({})).await;
        until(&mut ww, GAME, "mines").await;
        e.send(ROOM, 2, &b, GAME, "exit", json!({})).await;
        until(&mut ww, GAME, "mines").await;
        let _ = drain(&mut ww);
        e.disconnect(ROOM, 1, &a).await;
        // a was the last one in: minesend, then the lobby notice.
        assert_eq!(drain(&mut ww),
                   vec![(GAME.to_string(), "minesend".to_string()),
                        (GAME.to_string(), "lobby".to_string())]);
        assert!(run_depth(&e).is_none());
    }

    #[test]
    fn leaving_this_lobby_never_suppresses_the_left_notice() {
        // Only duel returns Left::Away; mines always reports the departure.
        let mut v = RoomValley::new(ROOM);
        let mut out = Out::new(GAME);
        let who = json!({"userId": "nobody"});
        assert!(matches!(dropped(&mut v, "nobody", &who, &mut out, 1000.0, true), Left::Notice));
        assert!(out.items.is_empty()); // nobody was in a run, so nothing happened
    }

    // ------------------------------------------------------------- the wire --

    #[tokio::test]
    async fn the_snapshot_keys_are_pythons_in_pythons_order() {
        let e = env(4, SEED);
        let (a, mut wa) = seat(&e, 1, "a").await;
        e.send(ROOM, 1, &a, GAME, "start", json!({})).await;
        let m = until(&mut wa, GAME, "mines").await;
        assert_eq!(m.as_object().unwrap().keys().collect::<Vec<_>>(),
                   vec!["type", "g", "ev", "pv", "run", "by"]);
        assert_eq!(m["run"].as_object().unwrap().keys().collect::<Vec<_>>(),
                   vec!["depth", "grid", "slimes", "players", "loot"]);
        assert_eq!(m["run"]["players"]["a"].as_object().unwrap().keys().collect::<Vec<_>>(),
                   vec!["x", "y", "hp", "name", "out"]);
        assert_eq!(m["g"], GAME);
        assert_eq!(m["pv"], 1);
        // The grid is MINE_ROWS arrays of MINE_COLS wire strings.
        assert_eq!(m["run"]["grid"].as_array().unwrap().len(), MINE_ROWS as usize);
        assert_eq!(m["run"]["grid"][0].as_array().unwrap().len(), MINE_COLS as usize);
        assert!(m["run"]["grid"][0][0].is_string());
    }

    #[test]
    fn a_name_falls_back_to_the_handle_and_prints_bare_in_a_note() {
        // `p.get("displayName") or p.get("handle")`: Python truthiness.
        assert_eq!(display_or_handle(&json!({"displayName": "Ann", "handle": "ash"})),
                   json!("Ann"));
        assert_eq!(display_or_handle(&json!({"displayName": "", "handle": "ash"})), json!("ash"));
        assert_eq!(display_or_handle(&json!({"handle": "ash"})), json!("ash"));
        assert_eq!(display_or_handle(&json!({"displayName": "", "handle": ""})), json!(""));
        assert_eq!(display_or_handle(&json!({})), Value::Null);
        assert_eq!(name_str(&json!("Ann")), "Ann"); // no quotes in the note
        assert_eq!(name_str(&Value::Null), "None"); // what Python's f-string prints
    }

    #[tokio::test]
    async fn a_digger_with_no_display_name_is_noted_by_their_handle() {
        let e = env(4, SEED);
        let (mut a, mut wa) = e.connect(ROOM, 1, "a").await;
        a.display_name = String::new(); // falsy, so `or` falls through
        a.handle = "ash".into();
        e.send(ROOM, 1, &a, GAME, "join", json!({})).await;
        let _ = drain(&mut wa); // the join tail's null-run snapshot
        e.send(ROOM, 1, &a, GAME, "start", json!({})).await;
        assert_eq!(until(&mut wa, GAME, "mines").await["run"]["players"]["a"]["name"], "ash");
        e.send(ROOM, 1, &a, GAME, "exit", json!({})).await;
        assert_eq!(until(&mut wa, GAME, "mines").await["note"], "ash climbed out");
    }

    #[test]
    fn the_constants_are_pythons() {
        // valley.py:109 and the literals inlined around class Mines.
        assert_eq!((MINE_COLS, MINE_ROWS, MINE_MAX_DEPTH), (12, 8, 12));
        assert_eq!(START_HP, 5);
        assert_eq!(ROCK_CHANCE, 0.42);
        assert_eq!(SLIME_MOVE_CHANCE, 0.5);
        assert_eq!(NO_LOOT_BELOW, 0.45);
        assert_eq!(CLEARED, [(0, 0), (1, 0), (0, 1), (11, 0), (10, 0)]);
        assert_eq!(SPAWNS,
                   [(0, 0), (11, 0), (1, 0), (10, 0), (0, 1), (1, 1), (11, 1), (10, 1)]);
        assert_eq!(LOOT.map(|(_, _, i)| i),
                   ["ruby", "emerald", "amethyst", "quartz", "gold", "iron"]);
        assert_eq!(LOOT.map(|(need, _, _)| need), [8, 6, 4, 0, 5, 2]);
        assert_eq!(LOOT.map(|(_, cut, _)| cut), [0.985, 0.97, 0.95, 0.9, 0.8, 0.65]);
        assert_eq!(GAME, "mines");
        assert_eq!([Cell::Rock.wire(), Cell::Floor.wire(), Cell::Ladder.wire()],
                   ["rock", "floor", "ladder"]);
    }
}
