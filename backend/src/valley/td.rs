//! Tower Defense (HQ 2.5): up to four people defend one board against twenty
//! waves of bugs with their own Pokemon as towers, in co-op.
//!
//! The rules are integer-only and live in the `TD-RULES` block below, mirrored
//! constant for constant in `games/td.js` (which runs the same rules locally for
//! solo play). `tests/test_td_sync.py` compares the two blocks by regex.
//!
//! AUDIENCE. Everything here goes to THIS GAME'S LOBBY (plus any player of the
//! running game whose socket is still in the room), never to the whole room:
//! `out.to(conn, ..)` for a joiner's or asker's view, `To::Users(ids, None)` for
//! every other event. The shared `lobby` event the dispatcher sends is the only
//! [`super::To::All`] frame a td player sees, and it is not ours.
//!
//! IT TICKS at [`HZ`] = 10, only while a wave runs (or the ready countdown that
//! starts one is pending), and stops itself on `done`, at the end of a wave and
//! when the lobby empties. Its `tsnap` goes out at 5 Hz and IS charged against
//! the room's bandwidth budget (`charge: Some(lobby ids)`), honouring `send`.
//!
//! TRUST. Tower specs are trust-the-client: `pokebattle::clean_spec` only clamps
//! them to legal values. Boards are therefore split per difficulty, and a co-op
//! result carries no placement (place null), so nobody earns a "win" from it.

use super::pokebattle::{build_mon, clean_spec, Mon};
use super::{Ctx, Left, Out, RoomValley, Tick, To, I_TD};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::Arc;

/// The game key on the wire.
pub const GAME: &str = "td";
/// Ticks a second.
pub const HZ: f64 = 10.0;

/* TD-RULES BEGIN */
pub const WAVES: i64 = 20;
pub const LIVES: i64 = 20;
pub const MAX_PLAYERS: usize = 4;
pub const MAX_TOWERS: usize = 6;
pub const START_BERRIES: i64 = 150;
pub const INCOME: [i64; 2] = [20, 3];
pub const SELL_PCT: i64 = 70;
pub const READY_SECS: i64 = 20;
pub const SNAP_EVERY: i64 = 2;
pub const COST: [i64; 5] = [50, 70, 90, 120, 150];
pub const BP: [i64; 5] = [40, 50, 60, 70, 80];
pub const MULT4: [i64; 6] = [0, 1, 2, 4, 8, 16];
pub const SPE_BANDS: [i64; 4] = [60, 90, 120, 150];
pub const CD: [i64; 5] = [12, 10, 8, 7, 6];
pub const RANGE: [i64; 3] = [300, 250, 200];
pub const DIFF_HP4: [i64; 3] = [3, 4, 6];
pub const KIND_HP: [i64; 6] = [60, 50, 110, 45, 70, 1200];
pub const KIND_SPD: [i64; 6] = [8, 11, 5, 10, 13, 4];
pub const KIND_BOUNTY: [i64; 6] = [3, 4, 6, 4, 5, 40];
pub const KIND_LEAK: [i64; 6] = [1, 1, 2, 1, 1, 5];
pub const KIND_ARMOUR: [i64; 6] = [0, 0, 1, 0, 0, 1];
pub const UNLOCK: [i64; 5] = [1, 3, 5, 7, 9];
pub const BOSS_WAVES: [i64; 2] = [10, 20];
pub const FX: [i64; 15] = [30, 5, 8, 80, 4, 2, 150, 20, 60, 10, 80, 2, 40, 5, 12];
#[allow(dead_code)] // the wire names of the kinds, read by games/td.js and the sync test
pub const KINDS: [&str; 6] = ["grub", "stinger", "beetle", "moth", "glitch", "boss"];
pub const KIND_T1: [&str; 6] = ["Bug", "Bug", "Bug", "Bug", "Bug", "Bug"];
pub const KIND_T2: [&str; 6] = ["", "Poison", "Steel", "Flying", "Electric", "Rock"];
pub const DIFFS: [&str; 3] = ["easy", "normal", "hard"];
pub const MODES: [&str; 3] = ["first", "strong", "close"];
pub const MAP_IDS: [&str; 3] = ["garden", "circuit", "datacenter"];
pub const GARDEN_PATH: [(i64, i64); 6] = [(0, 2), (4, 2), (4, 7), (10, 7), (10, 2), (15, 2)];
pub const GARDEN_SLOTS: [(i64, i64); 12] = [(2, 1), (2, 3), (5, 4), (3, 5), (6, 6), (7, 8), (9, 5), (11, 4), (8, 3), (12, 1), (13, 3), (6, 8)];
pub const CIRCUIT_PATH: [(i64, i64); 8] = [(0, 8), (3, 8), (3, 1), (8, 1), (8, 8), (12, 8), (12, 4), (15, 4)];
pub const CIRCUIT_SLOTS: [(i64, i64); 13] = [(1, 7), (2, 4), (4, 3), (4, 6), (6, 2), (5, 5), (7, 4), (9, 3), (9, 6), (10, 7), (11, 5), (13, 5), (14, 3)];
pub const DATACENTER_PATH: [(i64, i64); 6] = [(0, 1), (13, 1), (13, 4), (2, 4), (2, 8), (15, 8)];
pub const DATACENTER_SLOTS: [(i64, i64); 14] = [(3, 2), (6, 2), (9, 2), (12, 2), (14, 3), (4, 5), (7, 5), (10, 5), (1, 6), (3, 7), (6, 7), (9, 7), (12, 7), (14, 6)];
pub const LONG_TYPES: [&str; 8] = ["Flying", "Psychic", "Electric", "Ice", "Water", "Dragon", "Fairy", "Ghost"];
pub const MID_TYPES: [&str; 5] = ["Fire", "Grass", "Poison", "Dark", "Normal"];
/* TD-RULES END */

// Names for the FX table, in its order. One table so the sync test compares
// every effect number at once.
const BURN_TICKS: usize = 0;
const DOT_EVERY: usize = 1;
const BURN_DIV: usize = 2;
const KNOCK: usize = 3;
const KNOCK_EVERY: usize = 4;
const CHAIN: usize = 5;
const CHAIN_R: usize = 6;
const SLOW_TICKS: usize = 7;
const SLOW_PCT: usize = 8;
const ROOT_TICKS: usize = 9;
const SPLASH_R: usize = 10;
const PIERCE: usize = 11;
const PSN_TICKS: usize = 12;
const PSN_MAX: usize = 13;
const PSN_DIV: usize = 14;

const BOSS: usize = 5;
/// Snapshot caps.
pub const MAX_SNAP_BUGS: usize = 120;
pub const MAX_SNAP_HITS: usize = 60;

pub const ERR_HOST: &str = "only the host can do that";
pub const ERR_RUNNING: &str = "a game is already running";
pub const ERR_NO_GAME: &str = "no game is running";
pub const ERR_MAP: &str = "pick garden, circuit or datacenter";
pub const ERR_DIFF: &str = "pick easy, normal or hard";
pub const ERR_SLOT: &str = "no such slot";
pub const ERR_TAKEN: &str = "that slot is taken";
pub const ERR_SPEC: &str = "that creature is not valid";
pub const ERR_CAP: &str = "you have placed all your towers";
pub const ERR_BERRIES: &str = "not enough berries";
pub const ERR_OWNER: &str = "that is not your tower";
pub const ERR_MODE: &str = "pick first, strong or close";
pub const ERR_PLAYER: &str = "you are watching this game";
pub const ERR_BUSY: &str = "the Arena is busy right now: try again in a minute";

// ------------------------------------------------------------------ maps --

pub struct MapDef {
    pub id: &'static str,
    pub path: &'static [(i64, i64)],
    pub slots: &'static [(i64, i64)],
}

pub const MAPS: [MapDef; 3] = [
    MapDef { id: "garden", path: &GARDEN_PATH, slots: &GARDEN_SLOTS },
    MapDef { id: "circuit", path: &CIRCUIT_PATH, slots: &CIRCUIT_SLOTS },
    MapDef { id: "datacenter", path: &DATACENTER_PATH, slots: &DATACENTER_SLOTS },
];

fn map_index(id: &str) -> Option<usize> {
    MAPS.iter().position(|m| m.id == id)
}

/// Cell centre in board units (100 a tile).
fn centre(c: (i64, i64)) -> (i64, i64) {
    (c.0 * 100 + 50, c.1 * 100 + 50)
}

/// The path's length in board units.
pub fn path_len(path: &[(i64, i64)]) -> i64 {
    path.windows(2).map(|w| ((w[1].0 - w[0].0).abs() + (w[1].1 - w[0].1).abs()) * 100).sum()
}

/// Where a bug `d` units along the path stands.
pub fn pos_at(path: &[(i64, i64)], d: i64) -> (i64, i64) {
    let mut left = d.max(0);
    for w in path.windows(2) {
        let (a, b) = (centre(w[0]), centre(w[1]));
        let len = (b.0 - a.0).abs() + (b.1 - a.1).abs();
        if left <= len {
            return (a.0 + (b.0 - a.0).signum() * left, a.1 + (b.1 - a.1).signum() * left);
        }
        left -= len;
    }
    centre(*path.last().expect("a path has cells"))
}

// --------------------------------------------------------------- the rng --

/// mulberry32, the same generator `api.rng` and the page's helpers use.
pub struct Mulberry(u32);

impl Mulberry {
    pub fn next(&mut self) -> u32 {
        self.0 = self.0.wrapping_add(0x6D2B_79F5);
        let mut t = self.0;
        t = (t ^ (t >> 15)).wrapping_mul(t | 1);
        t ^= t.wrapping_add((t ^ (t >> 7)).wrapping_mul(t | 61));
        t ^ (t >> 14)
    }

    /// An index below `n`: floor(next / 2^32 * n).
    pub fn pick(&mut self, n: usize) -> usize {
        ((self.next() as u64 * n as u64) >> 32) as usize
    }
}

/// The wave seed: the first four bytes of sha256("<room>|<startedAt>").
pub fn seed_of(room: &str, started_at: i64) -> u32 {
    let h = Sha256::digest(format!("{room}|{started_at}").as_bytes());
    u32::from_be_bytes([h[0], h[1], h[2], h[3]])
}

/// Twenty waves: each a list of (spawn tick, kind).
pub fn gen_waves(seed: u32) -> Vec<Vec<(i64, usize)>> {
    let mut r = Mulberry(seed);
    (1..=WAVES)
        .map(|w| {
            let open: Vec<usize> =
                (0..UNLOCK.len()).filter(|k| UNLOCK[*k] <= w).collect();
            let gap = (12 - w / 3).max(4);
            let n = 6 + 2 * w;
            let mut v: Vec<(i64, usize)> = (0..n).map(|i| (i * gap, open[r.pick(open.len())])).collect();
            if BOSS_WAVES.contains(&w) {
                v.push((n * gap + 10, BOSS));
            }
            v
        })
        .collect()
}

// ------------------------------------------------------------ the towers --

/// MULT4 for one attacking type against one defending type ("" is no type).
pub fn m4(atk: &str, dfn: &str) -> i64 {
    if dfn.is_empty() {
        return MULT4[3];
    }
    let e = super::pokebattle::eff(atk, dfn);
    let i = match (e * 4.0) as i64 {
        0 => 0,
        1 => 1,
        2 => 2,
        4 => 3,
        8 => 4,
        _ => 5,
    };
    MULT4[i]
}

/// The two-factor multiplier (×16) of a tower type against a bug kind.
pub fn mult16(atk: &str, kind: usize) -> i64 {
    m4(atk, KIND_T1[kind]) * m4(atk, KIND_T2[kind])
}

/// The damage one hit does to one bug kind.
pub fn hit_dmg(base: i64, ttype: &str, kind: usize) -> i64 {
    let m = mult16(ttype, kind);
    if m == 0 {
        return 0;
    }
    let mut d = base * m / 16;
    if ttype == "Fire" && KIND_ARMOUR[kind] == 1 {
        d = d * 3 / 2;
    }
    d.max(1)
}

fn cooldown(spe: i64) -> i64 {
    let band = SPE_BANDS.iter().filter(|b| spe >= **b).count();
    CD[band]
}

fn range_of(t: &str) -> i64 {
    if LONG_TYPES.contains(&t) {
        RANGE[0]
    } else if MID_TYPES.contains(&t) {
        RANGE[1]
    } else {
        RANGE[2]
    }
}

#[derive(Clone, Debug)]
pub struct Tower {
    pub slot: usize,
    pub owner: String,
    pub spec: Value,
    pub mon: Mon,
    pub ttype: String,
    pub base: i64,
    pub cost: i64,
    pub cd_max: i64,
    pub range: i64,
    pub mode: usize,
    pub cd: i64,
    pub hits: i64,
}

impl Tower {
    pub fn new(slot: usize, owner: &str, raw: &Value) -> Option<Tower> {
        let spec = clean_spec(raw)?;
        let mon = build_mon(&spec);
        let ttype = mon.types.first().cloned().unwrap_or_else(|| "Normal".to_string());
        let atk = mon.stats.atk.max(mon.stats.spa);
        let st = spec.st as usize;
        // Only what build_mon actually honoured: a branch or mega it ignored is dropped.
        let br = spec.br.filter(|b| *b == mon.dex);
        let canon = json!({"sp": spec.sp, "st": spec.st, "br": br, "mg": mon.mega, "sh": spec.sh});
        Some(Tower {
            slot,
            owner: owner.to_string(),
            spec: canon,
            base: atk * BP[st] / 50 + 2,
            cost: COST[st],
            cd_max: cooldown(mon.stats.spe),
            range: range_of(&ttype),
            ttype,
            mon,
            mode: 0,
            cd: 0,
            hits: 0,
        })
    }

    fn view(&self) -> Value {
        let m = &self.mon;
        json!({"slot": self.slot, "owner": self.owner, "mode": MODES[self.mode], "cost": self.cost,
               "spec": self.spec, "base": self.base, "cd": self.cd_max, "range": self.range,
               "mon": {"name": m.name, "id": m.id, "dex": m.dex, "sprite": m.sprite, "mega": m.mega,
                       "shiny": m.shiny, "sp": m.sp, "st": m.st, "types": m.types}})
    }
}

// -------------------------------------------------------------- the bugs --

#[derive(Clone, Debug, Default)]
pub struct Bug {
    pub id: i64,
    pub kind: usize,
    pub d: i64,
    pub hp: i64,
    pub max: i64,
    pub slow: i64,
    pub root: i64,
    pub burn: i64,
    pub burn_dmg: i64,
    pub psn: i64,
    pub psn_t: i64,
    pub psn_dmg: i64,
}

// -------------------------------------------------------------- the game --

#[derive(Clone, Debug)]
pub struct Player {
    pub pubv: Value,
    pub berries: i64,
    pub ready: bool,
    pub away: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Phase {
    #[default]
    Idle,
    Build,
    Wave,
    Done,
}

impl Phase {
    pub fn as_str(self) -> &'static str {
        match self {
            Phase::Idle => "idle",
            Phase::Build => "build",
            Phase::Wave => "wave",
            Phase::Done => "done",
        }
    }
}

/// What one game step produced, in send order.
#[derive(Debug, PartialEq)]
pub enum Ev {
    Wave(i64),
    View,
    Done(Value),
}

#[derive(Default)]
pub struct Td {
    pub phase: Phase,
    pub map: usize,
    pub diff: usize,
    /// The wave now running, or the last one cleared between waves.
    pub wave: i64,
    pub cleared: i64,
    pub lives: i64,
    pub players: Vec<(String, Player)>,
    pub towers: Vec<Tower>,
    pub bugs: Vec<Bug>,
    pub waves: Vec<Vec<(i64, usize)>>,
    pub spawn_i: usize,
    pub tick: i64,
    pub next_id: i64,
    /// Monotonic seconds at which the ready countdown starts the next wave.
    pub ready_at: Option<f64>,
    /// (slot, bug id, mult16) since the last snapshot.
    pub hits: Vec<(usize, i64, i64)>,
    pub last: Option<Value>,
}

impl Td {
    pub fn running(&self) -> bool {
        matches!(self.phase, Phase::Build | Phase::Wave)
    }

    fn mapdef(&self) -> &'static MapDef {
        &MAPS[self.map]
    }

    pub fn player(&self, uid: &str) -> Option<&Player> {
        self.players.iter().find(|(u, _)| u == uid).map(|(_, p)| p)
    }

    fn player_mut(&mut self, uid: &str) -> Option<&mut Player> {
        self.players.iter_mut().find(|(u, _)| u == uid).map(|(_, p)| p)
    }

    /// Towers each player may own: min(6, floor(slots / players)).
    pub fn tower_cap(&self) -> usize {
        MAX_TOWERS.min(self.mapdef().slots.len() / self.players.len().max(1))
    }

    pub fn view(&self, t: f64) -> Value {
        let players: Vec<Value> = self
            .players
            .iter()
            .map(|(u, p)| {
                json!({"user": p.pubv, "berries": p.berries, "ready": p.ready, "away": p.away,
                       "towers": self.towers.iter().filter(|x| &x.owner == u).count()})
            })
            .collect();
        let towers: Vec<Value> = self.towers.iter().map(Tower::view).collect();
        let ready_in = self.ready_at.map(|at| ((at - t).max(0.0) * 1000.0) as i64);
        json!({"map": MAP_IDS[self.map], "diff": DIFFS[self.diff], "wave": self.wave,
               "cleared": self.cleared, "waves": WAVES, "lives": self.lives,
               "players": players, "towers": towers, "phase": self.phase.as_str(),
               "cap": self.tower_cap(), "readyInMs": ready_in, "last": self.last})
    }

    /// Start a game on this map and difficulty with these seated players.
    pub fn start(&mut self, map: usize, diff: usize, seats: &[(String, Value)], seed: u32) {
        *self = Td {
            phase: Phase::Build,
            map,
            diff,
            lives: LIVES,
            players: seats
                .iter()
                .take(MAX_PLAYERS)
                .map(|(u, p)| {
                    (u.clone(), Player { pubv: p.clone(), berries: START_BERRIES, ready: false, away: false })
                })
                .collect(),
            waves: gen_waves(seed),
            last: self.last.take(),
            ..Td::default()
        };
    }

    /// Someone joined the lobby: back from away, or a new seat while one is free.
    pub fn seat(&mut self, uid: &str, pubv: Value) -> bool {
        if !self.running() {
            return false;
        }
        if let Some(p) = self.player_mut(uid) {
            p.away = false;
            p.pubv = pubv;
            return true;
        }
        if self.players.len() < MAX_PLAYERS {
            self.players.push((uid.to_string(),
                               Player { pubv, berries: START_BERRIES, ready: false, away: false }));
            return true;
        }
        false
    }

    pub fn place(&mut self, uid: &str, slot: Option<i64>, raw: &Value) -> Result<(), &'static str> {
        if !self.running() {
            return Err(ERR_NO_GAME);
        }
        let Some(p) = self.player(uid) else { return Err(ERR_PLAYER) };
        let slots = self.mapdef().slots.len() as i64;
        let slot = match slot {
            Some(s) if (0..slots).contains(&s) => s as usize,
            _ => return Err(ERR_SLOT),
        };
        if self.towers.iter().any(|x| x.slot == slot) {
            return Err(ERR_TAKEN);
        }
        let Some(tower) = Tower::new(slot, uid, raw) else { return Err(ERR_SPEC) };
        if self.towers.iter().filter(|x| x.owner == uid).count() >= self.tower_cap() {
            return Err(ERR_CAP);
        }
        if p.berries < tower.cost {
            return Err(ERR_BERRIES);
        }
        let cost = tower.cost;
        self.player_mut(uid).expect("checked").berries -= cost;
        self.towers.push(tower);
        self.towers.sort_by_key(|x| x.slot);
        Ok(())
    }

    pub fn sell(&mut self, uid: &str, slot: Option<i64>) -> Result<i64, &'static str> {
        if !self.running() {
            return Err(ERR_NO_GAME);
        }
        let i = self
            .towers
            .iter()
            .position(|x| Some(x.slot as i64) == slot)
            .ok_or(ERR_SLOT)?;
        if self.towers[i].owner != uid {
            return Err(ERR_OWNER);
        }
        let back = self.towers[i].cost * SELL_PCT / 100;
        self.towers.remove(i);
        if let Some(p) = self.player_mut(uid) {
            p.berries += back;
        }
        Ok(back)
    }

    pub fn target(&mut self, uid: &str, slot: Option<i64>, mode: &str) -> Result<(), &'static str> {
        if !self.running() {
            return Err(ERR_NO_GAME);
        }
        let m = MODES.iter().position(|x| *x == mode).ok_or(ERR_MODE)?;
        let tw = self
            .towers
            .iter_mut()
            .find(|x| Some(x.slot as i64) == slot)
            .ok_or(ERR_SLOT)?;
        if tw.owner != uid {
            return Err(ERR_OWNER);
        }
        tw.mode = m;
        Ok(())
    }

    /// Mark a player ready. Returns true when the next wave should start now
    /// (everyone present is ready); the first ready arms the countdown.
    pub fn ready(&mut self, uid: &str, t: f64) -> Result<bool, &'static str> {
        if self.phase != Phase::Build {
            return Err(ERR_NO_GAME);
        }
        let p = self.player_mut(uid).ok_or(ERR_PLAYER)?;
        p.ready = true;
        if self.ready_at.is_none() {
            self.ready_at = Some(t + READY_SECS as f64);
        }
        Ok(self.players.iter().filter(|(_, p)| !p.away).all(|(_, p)| p.ready))
    }

    pub fn begin_wave(&mut self) {
        self.wave = self.cleared + 1;
        self.phase = Phase::Wave;
        self.spawn_i = 0;
        self.tick = 0;
        self.ready_at = None;
        for (_, p) in self.players.iter_mut() {
            p.ready = false;
        }
    }

    fn bug_hp(&self, kind: usize) -> i64 {
        let w = self.wave - 1;
        let np = self.players.len().max(1) as i64;
        (KIND_HP[kind] * (100 + 20 * w + 2 * w * w) * DIFF_HP4[self.diff] * (3 + np) / 1600).max(1)
    }

    /// The `done` payload; the phase becomes Done.
    pub fn finish(&mut self, win: bool) -> Value {
        self.phase = Phase::Done;
        self.ready_at = None;
        let key = format!("{}-{}", MAP_IDS[self.map], DIFFS[self.diff]);
        let results: Vec<Value> = self
            .players
            .iter()
            .map(|(_, p)| json!({"user": p.pubv, "place": null, "waves": self.cleared, "dnf": false}))
            .collect();
        let d = json!({"key": key, "map": MAP_IDS[self.map], "diff": DIFFS[self.diff], "mode": "coop",
                       "waves": self.cleared, "win": win, "results": results});
        self.last = Some(json!({"key": d["key"], "waves": self.cleared, "win": win}));
        self.bugs.clear();
        d
    }

    fn pick_target(&self, tw: &Tower, path: &[(i64, i64)]) -> Option<usize> {
        let tp = centre(self.mapdef().slots[tw.slot]);
        let mut best: Option<(usize, i64)> = None;
        for (i, b) in self.bugs.iter().enumerate() {
            if b.hp <= 0 || mult16(&tw.ttype, b.kind) == 0 {
                continue;
            }
            let p = pos_at(path, b.d);
            let d2 = (p.0 - tp.0).pow(2) + (p.1 - tp.1).pow(2);
            if d2 > tw.range * tw.range {
                continue;
            }
            // Larger is better for every mode: first (furthest along),
            // strong (most hp), close (smallest distance).
            let score = match tw.mode {
                0 => b.d,
                1 => b.hp,
                _ => -d2,
            };
            if best.map(|(_, s)| score > s).unwrap_or(true) {
                best = Some((i, score));
            }
        }
        best.map(|(i, _)| i)
    }

    fn strike(&mut self, slot: usize, i: usize, base: i64, ttype: &str, frac: (i64, i64)) {
        let b = &mut self.bugs[i];
        let d = hit_dmg(base, ttype, b.kind) * frac.0 / frac.1;
        if d <= 0 {
            return;
        }
        b.hp -= d.max(1);
        match ttype {
            "Fire" => {
                b.burn = FX[BURN_TICKS];
                b.burn_dmg = b.burn_dmg.max((d / FX[BURN_DIV]).max(1));
            }
            "Ice" => b.slow = FX[SLOW_TICKS],
            "Grass" if b.kind != BOSS => b.root = FX[ROOT_TICKS],
            "Poison" => {
                b.psn = (b.psn + 1).min(FX[PSN_MAX]);
                b.psn_t = FX[PSN_TICKS];
                b.psn_dmg = b.psn_dmg.max((d / FX[PSN_DIV]).max(1));
            }
            _ => {}
        }
        if self.hits.len() < MAX_SNAP_HITS {
            self.hits.push((slot, b.id, mult16(ttype, b.kind)));
        }
    }

    /// One tower shot with its type's effect.
    fn fire(&mut self, ti: usize, path: &[(i64, i64)]) {
        let tw = self.towers[ti].clone();
        let Some(i) = self.pick_target(&tw, path) else { return };
        self.towers[ti].cd = tw.cd_max;
        self.towers[ti].hits += 1;
        let hits = self.towers[ti].hits;
        let t = tw.ttype.as_str();
        self.strike(tw.slot, i, tw.base, t, (1, 1));
        let at = pos_at(path, self.bugs[i].d);
        let near = |me: &Td, r: i64, skip: &[usize]| -> Vec<usize> {
            let mut v: Vec<(i64, usize)> = me
                .bugs
                .iter()
                .enumerate()
                .filter(|(j, b)| !skip.contains(j) && b.hp > 0)
                .filter_map(|(j, b)| {
                    let p = pos_at(path, b.d);
                    let d2 = (p.0 - at.0).pow(2) + (p.1 - at.1).pow(2);
                    (d2 <= r * r).then_some((d2 * 1000 + b.id.min(999), j))
                })
                .collect();
            v.sort();
            v.into_iter().map(|(_, j)| j).collect()
        };
        match t {
            "Water" if hits % FX[KNOCK_EVERY] == 0 && self.bugs[i].kind != BOSS => {
                self.bugs[i].d = (self.bugs[i].d - FX[KNOCK]).max(0);
            }
            "Electric" => {
                for j in near(self, FX[CHAIN_R], &[i]).into_iter().take(FX[CHAIN] as usize) {
                    self.strike(tw.slot, j, tw.base, t, (1, 2));
                }
            }
            "Rock" | "Ground" => {
                for j in near(self, FX[SPLASH_R], &[i]) {
                    self.strike(tw.slot, j, tw.base, t, (1, 2));
                }
            }
            "Flying" | "Psychic" | "Ghost" => {
                let tp = centre(self.mapdef().slots[tw.slot]);
                let mut more: Vec<(i64, usize)> = self
                    .bugs
                    .iter()
                    .enumerate()
                    .filter(|(j, b)| *j != i && b.hp > 0 && mult16(t, b.kind) > 0)
                    .filter(|(_, b)| {
                        let p = pos_at(path, b.d);
                        (p.0 - tp.0).pow(2) + (p.1 - tp.1).pow(2) <= tw.range * tw.range
                    })
                    .map(|(j, b)| (-b.d, j))
                    .collect();
                more.sort();
                for (_, j) in more.into_iter().take(FX[PIERCE] as usize) {
                    self.strike(tw.slot, j, tw.base, t, (1, 1));
                }
            }
            _ => {}
        }
    }

    /// One 10 Hz step of a running wave (or the countdown before one).
    pub fn step(&mut self, t: f64) -> Vec<Ev> {
        let mut evs = Vec::new();
        if self.phase == Phase::Build {
            if self.ready_at.map(|at| t >= at).unwrap_or(false) {
                self.begin_wave();
                evs.push(Ev::Wave(self.wave));
                evs.push(Ev::View);
            }
            return evs;
        }
        if self.phase != Phase::Wave {
            return evs;
        }
        let path = self.mapdef().path;
        let end = path_len(path);
        // 1. spawn
        let wi = (self.wave - 1) as usize;
        while self.spawn_i < self.waves[wi].len() && self.waves[wi][self.spawn_i].0 <= self.tick {
            let kind = self.waves[wi][self.spawn_i].1;
            self.next_id += 1;
            let hp = self.bug_hp(kind);
            self.bugs.push(Bug { id: self.next_id, kind, hp, max: hp, ..Bug::default() });
            self.spawn_i += 1;
        }
        // 2. move, 3. damage over time
        let dot = self.tick % FX[DOT_EVERY] == 0;
        for b in self.bugs.iter_mut() {
            if b.root > 0 {
                b.root -= 1;
            } else {
                let pct = if b.slow > 0 { FX[SLOW_PCT] } else { 100 };
                b.d += KIND_SPD[b.kind] * pct / 100;
            }
            b.slow = (b.slow - 1).max(0);
            if b.burn > 0 {
                b.burn -= 1;
                if dot {
                    b.hp -= b.burn_dmg;
                }
            }
            if b.psn_t > 0 {
                b.psn_t -= 1;
                if dot {
                    b.hp -= b.psn_dmg * b.psn;
                }
                if b.psn_t == 0 {
                    b.psn = 0;
                }
            }
        }
        // 4. leaks
        let mut leaked = 0;
        self.bugs.retain(|b| {
            if b.d >= end && b.hp > 0 {
                leaked += KIND_LEAK[b.kind];
                false
            } else {
                true
            }
        });
        self.lives = (self.lives - leaked).max(0);
        // 5. towers, in slot order
        for ti in 0..self.towers.len() {
            if self.towers[ti].cd > 0 {
                self.towers[ti].cd -= 1;
            } else {
                self.fire(ti, path);
            }
        }
        // 6. the fallen pay a bounty to everyone present
        let mut bounty = 0;
        self.bugs.retain(|b| {
            if b.hp <= 0 {
                bounty += KIND_BOUNTY[b.kind];
                false
            } else {
                true
            }
        });
        for (_, p) in self.players.iter_mut() {
            if !p.away {
                p.berries += bounty;
            }
        }
        self.tick += 1;
        // 7. the end of the game, or of the wave
        if self.lives <= 0 {
            evs.push(Ev::Done(self.finish(false)));
        } else if self.spawn_i >= self.waves[wi].len() && self.bugs.is_empty() {
            self.cleared = self.wave;
            if self.cleared >= WAVES {
                evs.push(Ev::Done(self.finish(true)));
            } else {
                self.phase = Phase::Build;
                let inc = INCOME[0] + INCOME[1] * self.wave;
                for (_, p) in self.players.iter_mut() {
                    if !p.away {
                        p.berries += inc;
                    }
                }
                evs.push(Ev::View);
            }
        }
        evs
    }

    /// The 5 Hz snapshot: bugs [id, kind, x, y, hp%, fx], hits [slot, bug, mult16].
    pub fn snap(&mut self) -> Value {
        let path = self.mapdef().path;
        let bugs: Vec<Value> = self
            .bugs
            .iter()
            .take(MAX_SNAP_BUGS)
            .map(|b| {
                let (x, y) = pos_at(path, b.d);
                let fx = (b.slow > 0) as i64 | ((b.root > 0) as i64) << 1
                    | ((b.burn > 0) as i64) << 2 | ((b.psn > 0) as i64) << 3;
                json!([b.id, b.kind, x, y, b.hp.max(0) * 100 / b.max.max(1), fx])
            })
            .collect();
        let hits: Vec<Value> =
            self.hits.drain(..).map(|(s, b, m)| json!([s, b, m])).collect();
        let berries: Vec<Value> =
            self.players.iter().map(|(u, p)| json!([u, p.berries])).collect();
        json!({"t": self.tick, "bugs": bugs, "hits": hits, "lives": self.lives, "berries": berries})
    }
}

// -------------------------------------------------------- the dispatcher --

/// Lobby ids plus any player of the running game who left the lobby.
fn audience(v: &RoomValley) -> Vec<String> {
    let lobby = &v.lobbies[I_TD];
    let mut ids = lobby.ids();
    for (u, _) in &v.td.players {
        if !lobby.has(u) {
            ids.push(u.clone());
        }
    }
    ids
}

fn emit(v: &mut RoomValley, evs: Vec<Ev>, t: f64, items: &mut Vec<(To, String, Value)>) {
    let ids = audience(v);
    for e in evs {
        let (ev, data) = match e {
            Ev::Wave(n) => ("wave", json!({"n": n})),
            Ev::View => ("td", json!({"td": v.td.view(t)})),
            Ev::Done(d) => ("done", d),
        };
        items.push((To::Users(ids.clone(), None), ev.to_string(), data));
    }
}

fn flush_items(out: &mut Out, items: Vec<(To, String, Value)>) {
    for (to, ev, data) in items {
        out.push(to, &ev, data);
    }
}

fn want_ticker(td: &Td) -> bool {
    td.phase == Phase::Wave || (td.phase == Phase::Build && td.ready_at.is_some())
}

fn ensure_ticker(v: &mut RoomValley, cx: &mut Ctx, out: &mut Out) {
    if want_ticker(&v.td) && !cx.hub.tick_on(GAME, cx.room_id, HZ, Arc::new(step)) {
        out.err(cx.conn, ERR_BUSY);
    }
}

/// The join snapshot: the full view to this socket only. A returning player is
/// seated again (their towers never left), and a wave in progress gets its
/// ticker back if the lobby had emptied.
pub fn joined(v: &mut RoomValley, cx: &mut Ctx, out: &mut Out) {
    let uid = cx.uid().to_string();
    let seated = v.td.seat(&uid, cx.me());
    out.to(cx.conn, "td", json!({"td": v.td.view(cx.t)}));
    if seated {
        let ids = audience(v);
        out.lobby(ids, "td", json!({"td": v.td.view(cx.t)}));
    }
    ensure_ticker(v, cx, out);
}

pub fn op(v: &mut RoomValley, cx: &mut Ctx, op: &str, msg: &Value, out: &mut Out) {
    let t = cx.t;
    let uid = cx.uid().to_string();
    let slot = msg.get("slot").and_then(Value::as_i64);
    let host = v.lobbies[I_TD].host.as_deref() == Some(uid.as_str());
    let res: Result<bool, &'static str> = match op {
        "view" => {
            out.to(cx.conn, "td", json!({"td": v.td.view(t)}));
            return;
        }
        "start" => {
            let map = msg.get("map").and_then(Value::as_str).and_then(map_index);
            let diff = msg.get("diff").and_then(Value::as_str)
                .and_then(|d| DIFFS.iter().position(|x| *x == d));
            if !host {
                Err(ERR_HOST)
            } else if v.td.running() {
                Err(ERR_RUNNING)
            } else if let (Some(m), Some(d)) = (map, diff) {
                let started = (cx.hub.wall_now() * 1000.0) as i64;
                let seats = v.lobbies[I_TD].members.clone();
                v.td.start(m, d, &seats, seed_of(cx.room_id, started));
                Ok(true)
            } else if map.is_none() {
                Err(ERR_MAP)
            } else {
                Err(ERR_DIFF)
            }
        }
        "place" => v.td.place(&uid, slot, msg.get("mon").unwrap_or(&Value::Null)).map(|_| true),
        "sell" => v.td.sell(&uid, slot).map(|_| true),
        "target" => {
            let mode = msg.get("mode").and_then(Value::as_str).unwrap_or("");
            v.td.target(&uid, slot, mode).map(|_| true)
        }
        "ready" => match v.td.ready(&uid, t) {
            Ok(all) => {
                let mut items = Vec::new();
                if all {
                    v.td.begin_wave();
                    let n = v.td.wave;
                    emit(v, vec![Ev::Wave(n)], t, &mut items);
                }
                flush_items(out, items);
                Ok(true)
            }
            Err(e) => Err(e),
        },
        "end" => {
            if !host {
                Err(ERR_HOST)
            } else if !v.td.running() {
                Err(ERR_NO_GAME)
            } else {
                cx.hub.tick_off(GAME, cx.room_id);
                let mut items = Vec::new();
                if v.td.cleared >= 1 {
                    let d = v.td.finish(false);
                    emit(v, vec![Ev::Done(d)], t, &mut items);
                } else {
                    let last = v.td.last.take();
                    v.td = Td { last, ..Td::default() };
                }
                emit(v, vec![Ev::View], t, &mut items);
                flush_items(out, items);
                return;
            }
        }
        _ => return,
    };
    match res {
        Err(e) => out.err(cx.conn, e),
        Ok(_) => {
            let ids = audience(v);
            let mut data = json!({"td": v.td.view(t)});
            if op == "start" {
                data["by"] = cx.me();
            }
            out.lobby(ids, "td", data);
            ensure_ticker(v, cx, out);
        }
    }
}

/// One tick: the game step, the 5 Hz snapshot, and the stop conditions.
fn step(v: &mut RoomValley, t: f64, send: bool) -> Tick {
    let ids = v.lobbies[I_TD].ids();
    if ids.is_empty() || !want_ticker(&v.td) {
        return Tick { keep: false, items: Vec::new(), charge: None };
    }
    let evs = v.td.step(t);
    let mut items = Vec::new();
    if v.td.phase == Phase::Wave && v.td.tick % SNAP_EVERY == 0 && send {
        let s = v.td.snap();
        items.push((To::Users(audience(v), None), "tsnap".to_string(), s));
    }
    emit(v, evs, t, &mut items);
    Tick { keep: want_ticker(&v.td), items, charge: Some(ids) }
}

/// Someone left the lobby: their towers stay and their berries freeze.
pub fn dropped(
    v: &mut RoomValley, uid: &str, who: &Value, out: &mut Out, t: f64, disconnected: bool,
) -> Left {
    let _ = (who, disconnected);
    if let Some(p) = v.td.player_mut(uid) {
        p.away = true;
        p.ready = false;
        let ids = audience(v);
        out.lobby(ids, "td", json!({"td": v.td.view(t)}));
    }
    Left::Notice
}

#[cfg(test)]
mod tests {
    use super::super::pokebattle::DATA;
    use super::super::testkit::*;
    use super::*;
    use crate::realtime::MAX_TICKERS;
    use std::time::Duration;

    fn seats(n: usize) -> Vec<(String, Value)> {
        (0..n).map(|i| (format!("u{i}"), json!({"userId": format!("u{i}")}))).collect()
    }

    fn line_of(dex: i64) -> i64 {
        DATA.lines.iter().position(|l| l[0] == dex).unwrap() as i64
    }

    fn game(n: usize) -> Td {
        let mut td = Td::default();
        td.start(0, 1, &seats(n), 7);
        td
    }

    #[test]
    fn the_maps_are_sound() {
        for m in MAPS.iter() {
            assert!((10..=14).contains(&m.slots.len()), "{}", m.id);
            for w in m.path.windows(2) {
                assert!(w[0].0 == w[1].0 || w[0].1 == w[1].1, "{} bends diagonally", m.id);
            }
            let mut cells = Vec::new();
            for w in m.path.windows(2) {
                let (mut x, mut y) = w[0];
                cells.push((x, y));
                while (x, y) != w[1] {
                    x += (w[1].0 - x).signum();
                    y += (w[1].1 - y).signum();
                    cells.push((x, y));
                }
            }
            for s in m.slots {
                assert!((0..16).contains(&s.0) && (0..10).contains(&s.1));
                assert!(!cells.contains(s), "{} slot {s:?} is on the path", m.id);
            }
        }
    }

    #[test]
    fn waves_are_deterministic() {
        let a = gen_waves(seed_of("room1", 1_790_000_000_000));
        let b = gen_waves(seed_of("room1", 1_790_000_000_000));
        let c = gen_waves(seed_of("room1", 1_790_000_000_001));
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_eq!(a.len(), 20);
        assert_eq!(a[0].len(), 8);
        assert!(a[0].iter().all(|(_, k)| *k == 0), "wave 1 is all grubs");
        assert_eq!(a[9].last().unwrap().1, BOSS);
        assert_eq!(a[19].last().unwrap().1, BOSS);
        assert!(a[8].iter().all(|(_, k)| *k != BOSS));
    }

    #[test]
    fn fire_burns_a_beetle_and_grass_barely_scratches_it() {
        let ch = Tower::new(0, "u", &json!({"sp": line_of(4), "st": 0})).unwrap();
        let bu = Tower::new(1, "u", &json!({"sp": line_of(1), "st": 0})).unwrap();
        assert_eq!(ch.ttype, "Fire");
        assert_eq!(bu.ttype, "Grass");
        assert_eq!(mult16("Fire", 2), 64);
        assert_eq!(mult16("Grass", 2), 4);
        let (f, g) = (hit_dmg(ch.base, &ch.ttype, 2), hit_dmg(bu.base, &bu.ttype, 2));
        assert!(f >= 4 * g, "fire {f} vs grass {g}");
        // Ground cannot touch a flyer.
        assert_eq!(hit_dmg(100, "Ground", 3), 0);
    }

    #[test]
    fn an_illegal_spec_is_cleaned_or_refused() {
        let t = Tower::new(0, "u", &json!({"sp": 0, "st": 99, "mg": "nope", "x": 1})).unwrap();
        assert_eq!(t.spec, json!({"sp": 0, "st": 4, "br": null, "mg": null, "sh": false}));
        assert_eq!(t.cost, COST[4]);
        assert!(Tower::new(0, "u", &json!({"sp": 99999, "st": 0})).is_none());
        assert!(Tower::new(0, "u", &json!("charizard")).is_none());
    }

    #[test]
    fn placements_need_a_free_real_slot() {
        let mut td = game(1);
        let mon = json!({"sp": 0, "st": 0});
        assert_eq!(td.place("u0", Some(99), &mon), Err(ERR_SLOT));
        assert_eq!(td.place("u0", Some(-1), &mon), Err(ERR_SLOT));
        assert_eq!(td.place("u0", None, &mon), Err(ERR_SLOT));
        assert_eq!(td.place("u0", Some(0), &mon), Ok(()));
        assert_eq!(td.place("u0", Some(0), &mon), Err(ERR_TAKEN));
        assert_eq!(td.place("zz", Some(1), &mon), Err(ERR_PLAYER));
        assert_eq!(td.player("u0").unwrap().berries, START_BERRIES - COST[0]);
        assert_eq!(td.place("u0", Some(1), &json!({"sp": 0, "st": 4})), Err(ERR_BERRIES));
    }

    #[test]
    fn four_players_share_the_slots() {
        let mut td = game(4);
        assert_eq!(td.tower_cap(), 3); // garden: 12 slots / 4
        for (_, p) in td.players.iter_mut() {
            p.berries = 10_000;
        }
        for s in 0..3 {
            assert_eq!(td.place("u0", Some(s), &json!({"sp": 0, "st": 0})), Ok(()));
        }
        assert_eq!(td.place("u0", Some(3), &json!({"sp": 0, "st": 0})), Err(ERR_CAP));
        assert_eq!(td.place("u1", Some(3), &json!({"sp": 0, "st": 0})), Ok(()));
        assert_eq!(game(1).tower_cap(), MAX_TOWERS);
    }

    #[test]
    fn selling_refunds_seventy_percent_to_the_owner_only() {
        let mut td = game(2);
        td.place("u0", Some(2), &json!({"sp": 0, "st": 1})).unwrap();
        assert_eq!(td.sell("u1", Some(2)), Err(ERR_OWNER));
        assert_eq!(td.sell("u0", Some(2)), Ok(COST[1] * 70 / 100));
        assert_eq!(td.player("u0").unwrap().berries, START_BERRIES - COST[1] + 49);
        assert!(td.towers.is_empty());
        assert_eq!(td.sell("u0", Some(2)), Err(ERR_SLOT));
    }

    #[test]
    fn losing_every_life_ends_the_game_unwon() {
        let mut td = game(1);
        td.begin_wave();
        td.lives = 1;
        let end = path_len(MAPS[0].path);
        td.bugs.push(Bug { id: 1, kind: 0, d: end, hp: 5, max: 5, ..Bug::default() });
        let evs = td.step(0.0);
        let Some(Ev::Done(d)) = evs.last() else { panic!("{evs:?}") };
        assert_eq!(d["win"], json!(false));
        assert_eq!(d["mode"], json!("coop"));
        assert_eq!(d["results"][0]["place"], Value::Null);
        assert_eq!(td.phase, Phase::Done);
    }

    #[test]
    fn clearing_wave_twenty_wins() {
        let mut td = Td::default();
        td.start(0, 2, &seats(1), 3);
        td.cleared = 19;
        td.begin_wave();
        td.spawn_i = td.waves[19].len();
        let evs = td.step(0.0);
        let Some(Ev::Done(d)) = evs.last() else { panic!("{evs:?}") };
        assert_eq!(d["win"], json!(true));
        assert_eq!(d["waves"], json!(20));
        assert_eq!(d["key"], json!("garden-hard"));
        assert_eq!(d["results"][0]["user"]["userId"], json!("u0"));
    }

    #[test]
    fn a_wave_clears_and_pays_income() {
        let mut td = game(1);
        td.place("u0", Some(0), &json!({"sp": line_of(4), "st": 4})).ok();
        td.players[0].1.berries = 10_000;
        for s in 1..6 {
            td.place("u0", Some(s), &json!({"sp": line_of(4), "st": 4})).unwrap();
        }
        let before = td.player("u0").unwrap().berries;
        td.begin_wave();
        let mut evs = Vec::new();
        for _ in 0..3000 {
            evs = td.step(0.0);
            if td.phase != Phase::Wave {
                break;
            }
        }
        assert_eq!(evs, vec![Ev::View]);
        assert_eq!(td.phase, Phase::Build);
        assert_eq!(td.cleared, 1);
        assert_eq!(td.lives, LIVES);
        assert!(td.player("u0").unwrap().berries > before + INCOME[0]);
    }

    #[tokio::test]
    async fn a_td_event_goes_only_to_the_lobby() {
        let e = env(MAX_TICKERS, 1);
        let (a, mut wa) = e.connect("r", 1, "a").await;
        let (_b, mut wb) = e.connect("r", 2, "b").await;
        e.send("r", 1, &a, GAME, "join", json!({})).await;
        until(&mut wa, GAME, "td").await;
        e.send("r", 1, &a, GAME, "start", json!({"map": "circuit", "diff": "easy"})).await;
        let m = until_where(&mut wa, GAME, "td", |m| m["td"]["phase"] == "build").await;
        assert_eq!(m["td"]["map"], json!("circuit"));
        assert_eq!(m["by"]["userId"], json!("a"));
        let seen = drain(&mut wb);
        assert!(seen.contains(&(GAME.to_string(), "lobby".to_string())), "{seen:?}");
        assert!(!seen.iter().any(|(_, ev)| ev == "td"), "{seen:?}");
    }

    #[tokio::test]
    async fn the_host_starts_and_a_rejoin_gets_the_view() {
        let e = env(MAX_TICKERS, 1);
        let (a, mut wa) = e.connect("r", 1, "a").await;
        let (b, mut wb) = e.connect("r", 2, "b").await;
        e.send("r", 1, &a, GAME, "join", json!({})).await;
        e.send("r", 2, &b, GAME, "join", json!({})).await;
        e.send("r", 2, &b, GAME, "start", json!({"map": "garden", "diff": "normal"})).await;
        assert_eq!(until(&mut wb, GAME, "error").await["error"], json!(ERR_HOST));
        e.send("r", 1, &a, GAME, "start", json!({"map": "moon", "diff": "normal"})).await;
        assert_eq!(until(&mut wa, GAME, "error").await["error"], json!(ERR_MAP));
        e.send("r", 1, &a, GAME, "start", json!({"map": "garden", "diff": "normal"})).await;
        until_where(&mut wb, GAME, "td", |m| m["td"]["phase"] == "build").await;
        e.send("r", 2, &b, GAME, "place", json!({"slot": 4, "mon": {"sp": 3, "st": 0}})).await;
        until_where(&mut wa, GAME, "td", |m| m["td"]["towers"][0]["slot"] == 4).await;
        e.send("r", 2, &b, GAME, "leave", json!({})).await;
        let m = until_where(&mut wa, GAME, "td", |m| m["td"]["players"][1]["away"] == true).await;
        assert_eq!(m["td"]["towers"].as_array().unwrap().len(), 1, "towers stay");
        e.send("r", 2, &b, GAME, "join", json!({})).await;
        let v = until(&mut wb, GAME, "td").await;
        assert_eq!(v["td"]["players"][1]["away"], json!(false));
        assert_eq!(v["td"]["towers"][0]["owner"], json!("b"));
    }

    #[tokio::test]
    async fn the_ticker_stops_after_done() {
        let e = env(MAX_TICKERS, 1);
        let (a, mut wa) = e.connect("r", 1, "a").await;
        e.send("r", 1, &a, GAME, "join", json!({})).await;
        e.send("r", 1, &a, GAME, "start", json!({"map": "garden", "diff": "normal"})).await;
        e.send("r", 1, &a, GAME, "ready", json!({})).await;
        assert_eq!(until(&mut wa, GAME, "wave").await["n"], json!(1));
        let key = super::super::tick_key(GAME, "r");
        assert!(e.hub.registry().get(&key).is_some());
        // Wave 1 ends: the view (wrapped under "td" like every td event) and the
        // loop stops until someone readies up again.
        e.hub.with_room("r", |v| {
            v.td.spawn_i = v.td.waves[0].len();
            v.td.bugs.clear();
        });
        let v = until_where(&mut wa, GAME, "td", |m| m["td"]["phase"] == "build").await;
        assert_eq!(v["td"]["cleared"], json!(1));
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert!(e.hub.registry().get(&key).is_none());
        e.send("r", 1, &a, GAME, "ready", json!({})).await;
        assert_eq!(until(&mut wa, GAME, "wave").await["n"], json!(2));
        e.hub.with_room("r", |v| {
            v.td.cleared = 19;
            v.td.wave = 20;
            v.td.spawn_i = v.td.waves[19].len();
            v.td.bugs.clear();
        });
        let d = until(&mut wa, GAME, "done").await;
        assert_eq!(d["win"], json!(true));
        assert_eq!(d["key"], json!("garden-normal"));
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert!(e.hub.registry().get(&key).is_none());
    }
}
