//! Strikezone for the Valley (g = "strike"): a team FPS for up to 8, two sides
//! of 2-4 players each, with server-driven bots filling the side that is short.
//! A port of `backend/app/strike.py` plus the strike branch of
//! `backend/app/valley.py`, on the same tick and budgets as Blaster Arena
//! (`fps.rs`), with its own hub so each game keeps its own lobby.
//!
//! Each human runs and aims in the browser (games/strike.js) and streams where
//! they are; the server runs a 20 Hz tick that checks every move, judges every
//! shot, keeps the score and sends the lobby one batched snapshot per tick.
//! Shots are judged with the same lag compensation as Blaster Arena (see
//! `fps.rs` for the full description); the only difference is that a shot never
//! hits a team-mate.
//!
//! Teams: players are split amber (0) / cyan (1). The host picks the side size
//! (2-4) at start; both sides are filled with bots until each has that many,
//! and a bot leaves first when a human takes its place. A team scores when any
//! of its members makes a kill; the round ends at the score limit, at the time,
//! or when one side has no active members left.
//!
//! Bots: a bot is a player in the match vector like any other, flagged in the
//! snapshot (flags bit 16); the client draws it exactly like a human. Every
//! tick the referee thinks for each bot: pick a goal (a living enemy when one
//! is in sight, else a waypoint of the patrol graph), walk there with the same
//! step() physics the clients use, and fire through its own adjudication (no
//! rewind is needed - the bot is on the server). Bots never send network
//! frames and never have clocks of their own.
//!
//! The arena is `backend/app/strike_map.json`, embedded at build time with
//! `include_str!` -- the file the Python reads (a byte copy of
//! games/strike/map.json).
//!
//! Units on the wire: x, y, z in centimetres, yaw and pitch in hundredths of a
//! degree, the sender's clock in centiseconds (q).
//!
//! Protocol: in `{"type":"game","g":"strike","op":...}`, out
//! `{"type":"game","g":"strike","ev":...}`. Ops: join, leave, invite, view,
//! char, start, end, pos, fire, reload, weapon. Events: lobby, strike, char,
//! go, snap, fix, ammo, kill, spawn, pick, item, gone, done, invite, invited,
//! error.

// target_at and a few constants are the shared geometry the tests drive (and
// the browser's practice range uses); the hub runtime is exercised through
// main.rs, which the test target does not compile -- hence the wide allow.
#![allow(dead_code)]

use crate::kart::{Lobby, Out, To};
use crate::realtime::{Clock, Registry, Ticker};
use crate::results::Recorder;
use crate::rooms::{Member, RoomManager};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::{Arc, LazyLock, Mutex};

// --- movement (shared with games/strike.js) ---
pub const R: f64 = 0.35;
pub const H: f64 = 1.8;
pub const EYE: f64 = 1.6;
pub const STEP_H: f64 = 0.55;
pub const RUN: f64 = 6.0;
pub const SPRINT: f64 = 1.35;
/// Sprint stamina constants (client-side; the referee only watches raw speed).
pub const STAMINA_MAX: f64 = 100.0;
pub const STAMINA_DRAIN: f64 = 12.5;
pub const STAMINA_REGEN: f64 = 14.0;
pub const SPRINT_SLOW: f64 = 0.015;
/// v4: parachute drops — a spawn may lift the player and they glide down (`drop` hint)
pub const DROP_HEIGHT: f64 = 11.0;
pub const DROP_TIME: f64 = 3.0;
pub const DROP_SINK: f64 = 2.8;
pub const DROP_DRIFT: f64 = 4.5;
pub const ACCEL: f64 = 12.0;
pub const AIR_ACCEL: f64 = 3.0;
pub const GRAVITY: f64 = 18.0;
pub const JUMP_V: f64 = 6.5;
pub const FALL_MAX: f64 = 25.0;
// the movement totems' multipliers (shared with games/strike.js FS)
pub const LEAP_MULT: f64 = 3.0;   // the leap totem: Space-bar jumps this much taller
pub const SPEED_MULT: f64 = 2.0;  // the speed totem: plain WASD at this multiple of sprint pace

// --- weapons (shared with games/strike.js) ---
pub struct Weapon {
    #[allow(dead_code)] // kept in step with games/strike.js weapon ids
    pub id: &'static str,
    pub interval: f64,
    pub damage: i64,
    pub head: i64,
    pub mag: i64,
    pub reserve: i64,
    pub reload: f64,
    pub spread: f64,
    pub range: f64,
    pub pack: i64,
}
// The Arena serves all four, launcher included: its rocket is a real missile the
// server flies in tick() (ROCKETS) and resolves as a blast (blast_damage), matching
// the browser's solo simulation (FS.MISSILE_V & co in games/strike.js).
pub const NW: usize = 4;
pub const WEAPONS: [Weapon; NW] = [
    Weapon { id: "rapid", interval: 0.1, damage: 12, head: 18, mag: 30, reserve: 90, reload: 1.6, spread: 1.2,
             range: 60.0, pack: 60 },
    Weapon { id: "marksman", interval: 0.8, damage: 75, head: 112, mag: 5, reserve: 15, reload: 2.2, spread: 0.3,
             range: 120.0, pack: 10 },
    Weapon { id: "revolver", interval: 0.5, damage: 50, head: 75, mag: 6, reserve: 24, reload: 2.2, spread: 0.5,
             range: 70.0, pack: 12 },
    Weapon { id: "launcher", interval: 1.2, damage: 120, head: 120, mag: 1, reserve: 5, reload: 2.5, spread: 0.0,
             range: 60.0, pack: 2 },
];
// --- the launcher's rocket (shared with games/strike.js) ---
pub const MISSILE_V: f64 = 22.0;   // launch speed (m/s)
pub const MISSILE_G: f64 = 6.0;    // gravity on the rocket (gentler than a body)
pub const MISSILE_TTL: f64 = 3.5;  // s before it fizzles mid-air
pub const BLAST_R: f64 = 4.0;      // splash radius, falloff 75 -> 25
pub const DIRECT_R: f64 = 0.8;     // inside this, a direct hit (120)

/// Splash damage by distance from the explosion (mirrors FS.blastDamage).
pub fn blast_damage(dist: f64, direct: bool) -> i64 {
    if direct {
        return 120;
    }
    if dist >= BLAST_R {
        return 0;
    }
    (75.0 - (75.0 - 25.0) * (dist / BLAST_R)).round() as i64
}
pub const SWITCH: f64 = 0.25;
pub const HP: i64 = 100;
pub const HEAL: i64 = 50;
pub const BODY_R: f64 = 0.4;
pub const BODY_H: f64 = 1.4;
pub const HEAD_R: f64 = 0.25;
pub const HEAD_TOP: f64 = 1.9;

// --- referee ---
pub const HZ: f64 = 20.0;
pub const MAX_SPEED: f64 = 7.5;
pub const SLACK: f64 = 1.0;
pub const SPRINT_MAX_SPEED: f64 = 10.5;
pub const SHRINK: f64 = 0.05;
pub const MAX_AIR: f64 = 2.0;
pub const CLOCK_LEAD: f64 = 0.6;
pub const POS_RATE: f64 = 25.0;
pub const POS_BURST: f64 = 10.0;
pub const FIRE_RATE: f64 = 15.0;
pub const FIRE_BURST: f64 = 6.0;
pub const OP_RATE: f64 = 6.0;
pub const OP_BURST: f64 = 4.0;
pub const FIX_GAP: f64 = 0.5;
pub const ORIGIN_SLACK: f64 = 1.0;
pub const FIRE_LATE: f64 = 0.5;
pub const IP_CAP: f64 = 0.35;
pub const RTT_CAP: f64 = 0.4;
pub const RTT_WINDOW: f64 = 3.0;
pub const HISTORY: f64 = 1.2;
pub const COUNTDOWN: f64 = 4.0;
pub const RESPAWN: f64 = 3.0;
pub const PROTECT: f64 = 1.5;
pub const PICK_R: f64 = 1.1;
pub const PICKUP_BACK: f64 = 15.0;
pub const GRACE: f64 = 15.0;
// --- totems (the "hack" pickups): one drops every TOTEM_GAP during a round; the mole only
// appears when the other side fields a bot (it can't turn a human). Durations mirror
// games/strike.js TOTEMS, which is the client's word for what each gem does.
pub const TOTEM_GAP: f64 = 10.0;
pub const TOTEM_FIRST: f64 = 8.0;
pub const TOTEM_KINDS: [&str; 9] = ["mole", "freeze", "decoy", "cloak", "shield", "wings", "leap", "speed", "teleport"];
// --- the drop rings: past the map's own packs (the first MAP.pickups.len() slots) sit a
// fixed pool of totem slots then pack slots, reused round-robin so slot indices stay
// stable while the drops themselves keep coming — fresh kind, fresh spot, repeats allowed.
// Loose packs (health/ammo) are their own clock, independent of the totems.
pub const DROP_SLOTS: usize = 3;   // recycled totem slots
pub const PACK_SLOTS: usize = 2;   // recycled loose-pack slots
pub const PACK_GAP: f64 = 8.0;     // s between loose-pack drops
pub const PACK_FIRST: f64 = 6.0;   // s into the round before the first loose pack
pub const CHARM: f64 = 8.0;    // s a mole-turned bot shoots its own side
pub const FREEZE: f64 = 5.0;   // s a paralysed bot can't move or fire
pub const DECOY: f64 = 10.0;   // s a decoy ghost stands and eats shots
pub const CLOAK: f64 = 12.0;   // s enemy bots can't see the cloaked player
pub const SHIELD_TOTEM: f64 = 10.0;   // s the shield totem's bubble holds (they still shoot)
pub const WINGS: f64 = 10.0;      // s the wings totem lets the taker fly (auto rise, glide, auto land)
/// s beyond WINGS the descent is still judged as wings flight (9 m at WING_SINK 2.0, rounded up)
pub const WINGS_LAND: f64 = 5.0;
pub const LEAP: f64 = 10.0;       // s the leap totem's boosted Space-bar jumps
pub const SPEED: f64 = 6.0;       // s the speed totem's double-sprint dash on plain WASD
/// The quick presets the menu offers. The host's own minutes/kills (1..=60, 1..=500)
/// are accepted too — `start` clamps, it doesn't check these lists.
pub const MINUTES: [i64; 3] = [3, 5, 10];
pub const KILLS: [i64; 4] = [10, 20, 30, 50];
pub const TEAM_SIZES: [i64; 5] = [1, 2, 3, 4, 5];
pub const MAX_PLAYERS: usize = 10;
pub const CHARS: i64 = 6;

// --- bots ---
pub const BOT_THINK: f64 = 0.25;    // s between a bot's decisions
pub const BOT_TURN: f64 = 9.0;      // how fast a bot swings its aim (radians/s)
pub const BOT_AIM: f64 = 0.04;      // radians of aim error a bot lets itself (they miss)
pub const BOT_RANGE: f64 = 45.0;    // m at which a bot will start shooting
pub const BOT_REACT: f64 = 0.35;    // s a bot takes to open fire once a target is in sight
pub const BOT_NAMES: [&str; 8] = ["Vex", "Rook", "Piston", "Mako", "Juno", "Crow", "Dart", "Onyx"];
/// Per-team colours (amber, cyan) as [r, g, b] in 0..1, for the roster.
pub const BOT_COLORS: [[f64; 3]; 2] = [[0.86, 0.55, 0.20], [0.25, 0.72, 0.80]];
/// Patrol waypoints: (x, z) on the ground plane.
pub const WAYPOINTS: [(f64, f64); 23] = [
    (0.0, -9.0), (0.0, 0.0), (0.0, 9.0), (-7.0, 0.0), (7.0, 0.0),
    (-7.0, -9.0), (7.0, -9.0), (-7.0, 9.0), (7.0, 9.0),
    (-14.2, 0.0), (14.2, 0.0), (-11.0, -6.0), (11.0, 6.0),
    (-9.0, -12.8), (9.0, -12.8), (-9.0, 12.8), (9.0, 12.8),
    (-5.5, -5.2), (5.5, 5.2), (-5.5, 5.2), (5.5, -5.2),
    (0.0, -6.0), (0.0, 6.0),
];

// Lobby (valley.py)
pub const GAME: &str = "strike";
pub const GAME_NAME: &str = "Strikezone";
pub const MAX_LOBBY: usize = 8;
pub const HOST_GRACE: f64 = 15.0;

pub const MAP_JSON: &str = include_str!("../../backend/app/strike_map.json");

pub type Box3 = [f64; 6];

pub struct Pickup {
    #[allow(dead_code)] // mirrors the map JSON; read by tests and the client
    pub id: String,
    pub health: bool,
    pub at: [f64; 3],
}

pub struct Map {
    pub id: String,
    bounds_field: Box3,
    pub boxes: Vec<Box3>,
    pub spawns: Vec<[f64; 4]>,
    pub pickups: Vec<Pickup>,
    #[allow(dead_code)] // practice drones: the client animates them from target_at
    pub targets: Vec<([f64; 3], [f64; 3], f64)>,
}

impl Map {
    pub fn bounds(&self) -> Box3 {
        self.bounds_field
    }
}

fn nums<const N: usize>(v: &Value) -> [f64; N] {
    let mut out = [0.0; N];
    for (i, o) in out.iter_mut().enumerate() {
        *o = v[i].as_f64().expect("a number");
    }
    out
}

pub fn compile_map(d: &Value) -> Result<Map, String> {
    let boxes: Vec<Box3> = d["boxes"].as_array().ok_or("no boxes")?.iter().map(nums::<6>).collect();
    if boxes.iter().any(|b| !(b[0] < b[3] && b[1] < b[4] && b[2] < b[5])) {
        return Err("box with no volume".into());
    }
    Ok(Map {
        id: d["id"].as_str().unwrap_or("").to_string(),
        bounds_field: nums::<6>(&d["bounds"]),
        boxes,
        spawns: d["spawns"].as_array().ok_or("no spawns")?.iter().map(nums::<4>).collect(),
        pickups: d["pickups"].as_array().ok_or("no pickups")?.iter()
            .map(|p| Pickup { id: p["id"].as_str().unwrap_or("").into(), health: p["kind"] == "health",
                              at: nums::<3>(&p["at"]) })
            .collect(),
        targets: d["targets"].as_array().map(|a| a.iter()
            .map(|t| (nums::<3>(&t["a"]), nums::<3>(&t["b"]), t["s"].as_f64().unwrap_or(1.0))).collect())
            .unwrap_or_default(),
    })
}

pub static MAP: LazyLock<Map> = LazyLock::new(|| {
    compile_map(&serde_json::from_str(MAP_JSON).expect("strike_map.json parses"))
        .expect("strike_map.json compiles")
});

// ------------------------------------------------------- shared geometry --
/// Index of the first solid box a player standing at (x, y, z) is inside.
pub fn overlaps(m: &Map, x: f64, y: f64, z: f64, shrink: f64) -> Option<usize> {
    let (x0, x1) = (x - R + shrink, x + R - shrink);
    let (y0, y1) = (y + shrink, y + H - shrink);
    let (z0, z1) = (z - R + shrink, z + R - shrink);
    m.boxes.iter().position(|b| x1 > b[0] && x0 < b[3] && y1 > b[1] && y0 < b[4] && z1 > b[2] && z0 < b[5])
}

/// The highest box top among the boxes a player at (x, y, z) overlaps.
pub fn top_under(m: &Map, x: f64, y: f64, z: f64) -> Option<f64> {
    let mut best: Option<f64> = None;
    for b in &m.boxes {
        if x + R > b[0] && x - R < b[3] && y + H > b[1] && y < b[4] && z + R > b[2] && z - R < b[5]
            && best.map(|v| b[4] > v).unwrap_or(true)
        {
            best = Some(b[4]);
        }
    }
    best
}

/// The runner's state for [`step`]: position, velocity, on the ground.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Body {
    pub x: f64,
    pub y: f64,
    pub z: f64,
    pub vx: f64,
    pub vy: f64,
    pub vz: f64,
    pub g: bool,
}

/// One step of the runner (strike.move): axis by axis against the solid boxes,
/// a low ledge stepped onto while on the ground.
pub fn step(m: &Map, s: &mut Body, wx: f64, wz: f64, jump: bool, dt: f64) {
    let k = (dt * if s.g { ACCEL } else { AIR_ACCEL }).min(1.0);
    s.vx += (wx * RUN - s.vx) * k;
    s.vz += (wz * RUN - s.vz) * k;
    if jump && s.g {
        s.vy = JUMP_V;
        s.g = false;
    }
    s.vy = (s.vy - GRAVITY * dt).max(-FALL_MAX);
    for ax in 0..2 {
        let old = if ax == 0 { s.x } else { s.z };
        let v = if ax == 0 { s.vx } else { s.vz };
        if ax == 0 { s.x = old + v * dt } else { s.z = old + v * dt }
        if let Some(i) = overlaps(m, s.x, s.y, s.z, 0.0) {
            let top = m.boxes[i][4];
            if s.g && top - s.y > 0.0 && top - s.y <= STEP_H && overlaps(m, s.x, top, s.z, 0.0).is_none() {
                s.y = top;
            } else if ax == 0 {
                s.x = old;
                s.vx = 0.0;
            } else {
                s.z = old;
                s.vz = 0.0;
            }
        }
    }
    let ny = s.y + s.vy * dt;
    if overlaps(m, s.x, ny, s.z, 0.0).is_none() {
        s.y = ny;
        s.g = false;
    } else if s.vy <= 0.0 {
        if let Some(top) = top_under(m, s.x, ny, s.z) {
            if top <= s.y + 1e-9 {
                s.y = top;
            }
        }
        s.vy = 0.0;
        s.g = true;
    } else {
        s.vy = 0.0;
    }
}

/// Unit view direction for yaw/pitch in degrees (yaw 0 = -z, 90 = +x; pitch up +).
pub fn dir_of(yaw: f64, pitch: f64) -> [f64; 3] {
    let (y, p) = (yaw.to_radians(), pitch.to_radians());
    let cp = p.cos();
    [y.sin() * cp, p.sin(), -y.cos() * cp]
}

/// The yaw (degrees) that looks along (dx, dz) - dir_of's inverse on the plane.
pub fn yaw_of(dx: f64, dz: f64) -> f64 {
    dx.atan2(-dz).to_degrees()
}

/// mulberry32, one draw.
pub fn mb32(a: u32) -> f64 {
    let a = a.wrapping_add(0x6D2B_79F5);
    let mut t = (a ^ (a >> 15)).wrapping_mul(1 | a);
    t = (t.wrapping_add((t ^ (t >> 7)).wrapping_mul(61 | t))) ^ t;
    (t ^ (t >> 14)) as f64 / 4294967296.0
}

/// The totem drop rng (games/strike.js hashMatch): one salted draw from a seed.
pub fn hash_match(seed: u32, salt: u32) -> f64 {
    let x = seed ^ salt.wrapping_mul(0x9E37_79B9);
    x as f64 / 4294967295.0
}

/// The deterministic spread (degrees of yaw, pitch) of a player's n-th shot.
pub fn spread_of(n: i64, w: usize) -> (f64, f64) {
    let s = WEAPONS[w].spread;
    let seed = ((n as u64).wrapping_mul(2654435761).wrapping_add(w as u64 * 40503) & 0xFFFF_FFFF) as u32;
    ((mb32(seed) * 2.0 - 1.0) * s, (mb32(seed ^ 0x5BD1_E995) * 2.0 - 1.0) * s)
}

/// Distance along o + t d (t >= 0) to the box; 0 if o is inside; None: a miss.
pub fn ray_box(o: [f64; 3], d: [f64; 3], b: &Box3) -> Option<f64> {
    let (mut tmin, mut tmax) = (0.0f64, f64::INFINITY);
    for a in 0..3 {
        if d[a].abs() < 1e-12 {
            if o[a] < b[a] || o[a] > b[a + 3] {
                return None;
            }
        } else {
            let inv = 1.0 / d[a];
            let (mut t1, mut t2) = ((b[a] - o[a]) * inv, (b[a + 3] - o[a]) * inv);
            if t1 > t2 {
                std::mem::swap(&mut t1, &mut t2);
            }
            if t1 > tmin {
                tmin = t1;
            }
            if t2 < tmax {
                tmax = t2;
            }
            if tmin > tmax {
                return None;
            }
        }
    }
    Some(tmin)
}

/// Distance to the first solid box along the ray, or maxd.
pub fn ray_map(m: &Map, o: [f64; 3], d: [f64; 3], maxd: f64) -> f64 {
    m.boxes.iter().filter_map(|b| ray_box(o, d, b)).fold(maxd, f64::min)
}

pub fn hit_boxes(x: f64, y: f64, z: f64) -> (Box3, Box3) {
    ([x - BODY_R, y, z - BODY_R, x + BODY_R, y + BODY_H, z + BODY_R],
     [x - HEAD_R, y + BODY_H, z - HEAD_R, x + HEAD_R, y + HEAD_TOP, z + HEAD_R])
}

/// (distance, head?) where the ray first meets a player at (x, y, z), nearer than maxd.
pub fn ray_player(o: [f64; 3], d: [f64; 3], x: f64, y: f64, z: f64, maxd: f64) -> Option<(f64, bool)> {
    let (body, head) = hit_boxes(x, y, z);
    let mut best = ray_box(o, d, &body).filter(|t| *t < maxd).map(|t| (t, false));
    if let Some(th) = ray_box(o, d, &head).filter(|t| *t < maxd) {
        if best.map(|b| th < b.0).unwrap_or(true) {
            best = Some((th, true));
        }
    }
    best
}

/// A practice drone's centre at time t: back and forth between a and b.
#[allow(dead_code)] // parity with the Python backend; exercised by tests
pub fn target_at(a: [f64; 3], b: [f64; 3], s: f64, t: f64) -> [f64; 3] {
    let ln = ((b[0] - a[0]).powi(2) + (b[1] - a[1]).powi(2) + (b[2] - a[2]).powi(2)).sqrt();
    let ln = if ln == 0.0 { 1.0 } else { ln };
    let ph = (t * s / ln).rem_euclid(2.0);
    let u = if ph <= 1.0 { ph } else { 2.0 - ph };
    [a[0] + (b[0] - a[0]) * u, a[1] + (b[1] - a[1]) * u, a[2] + (b[2] - a[2]) * u]
}

fn dist3(a: [f64; 3], b: [f64; 3]) -> f64 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

/// Line of sight between two eye points: True when no solid box is in the way.
pub fn los(m: &Map, a: [f64; 3], b: [f64; 3]) -> bool {
    let d = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
    let ln = d[0].hypot(d[1]).hypot(d[2]);
    if ln < 1e-9 {
        return true;
    }
    let u = [d[0] / ln, d[1] / ln, d[2] / ln];
    ray_map(m, a, u, ln) >= ln - 1e-6
}

/// A finite JSON number (not a bool), clamped to [lo, hi].
pub fn as_num(v: Option<&Value>, lo: f64, hi: f64) -> Option<f64> {
    let f = v?.as_number()?.as_f64()?;
    if !f.is_finite() {
        return None;
    }
    Some(f.max(lo).min(hi))
}

fn cm(v: f64) -> i64 {
    (v * 100.0).round_ties_even() as i64
}

// ----------------------------------------------------------------- referee --
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Idle,
    Warmup,
    Round,
    Done,
}

impl Phase {
    pub fn as_str(self) -> &'static str {
        match self {
            Phase::Idle => "idle",
            Phase::Warmup => "warmup",
            Phase::Round => "round",
            Phase::Done => "done",
        }
    }
}

/// A bot's working state. None on a human.
#[derive(Clone, Debug, Default)]
pub struct Bot {
    pub goal: Option<(f64, f64)>,
    pub wpi: usize,
    pub think_at: f64,
    pub target: Option<String>,
    pub seen_at: Option<f64>,
    pub aim_y: f64,
    pub aim_p: f64,
    pub err_y: f64,
    pub err_p: f64,
    pub err_at: f64,
    pub fire_at: f64,
    pub body: Body,
    /// The decoy this bot is currently baited by (strike.decoys index); None = a real target.
    pub decoy: Option<usize>,
}

/// A launcher rocket in flight, flown by the server in tick(). The shooter's
/// uid resolves the kill credit; born is the match clock it launched at.
#[derive(Clone, Debug)]
pub struct Rocket {
    pub id: i64,
    pub shooter: String,
    pub x: f64,
    pub y: f64,
    pub z: f64,
    pub px: f64,
    pub py: f64,
    pub pz: f64,
    pub vx: f64,
    pub vy: f64,
    pub vz: f64,
    pub born: f64,
}

/// One decoy ghost (the Doppelganger totem): a bot-bait copy standing where its owner faced.
#[derive(Clone, Debug)]
pub struct Decoy {
    pub id: i64,
    pub x: f64,
    pub y: f64,
    pub z: f64,
    /// Yaw in degrees — the direction the owner faced when they dropped it.
    pub yaw: f64,
    pub owner: String,
    pub until: f64,
}

#[derive(Clone, Debug)]
pub struct Player {
    pub user: Value,
    pub slot: i64,
    pub team: u8,
    pub bot: Option<Bot>,
    pub ch: i64,
    pub x: f64,
    pub y: f64,
    pub z: f64,
    pub vx: f64,
    pub vz: f64,
    pub r: i64,
    pub pt: i64,
    pub hp: i64,
    /// The hp before this tick's blast damage (detonate's echoes report the delta).
    pub hp_original: i64,
    pub w: usize,
    pub mag: [i64; NW],
    pub res: [i64; NW],
    pub last: [VecDeque<i64>; NW],
    pub ready: i64,
    /// (weapon, started at, done at) on the player's clock.
    pub reload: Option<(usize, i64, i64)>,
    pub n: i64,
    pub kills: i64,
    pub deaths: i64,
    pub dead: bool,
    pub respawn_at: f64,
    pub life: i64,
    pub spawn_t: f64,
    pub protect: f64,
    /// v4: this life parachute-drops — the referee lets it ride the sky for DROP_TIME after spawn.
    pub drop: bool,
    pub drop_t: f64,
    pub away: Option<f64>,
    pub gone: bool,
    pub q: Option<i64>,
    pub q0: i64,
    pub t0: f64,
    pub off: Option<f64>,
    pub qmax: Option<i64>,
    pub at: f64,
    pub air_at: f64,
    pub pb: (f64, f64),
    pub fb: (f64, f64),
    pub ob: (f64, f64),
    pub fix_at: f64,
    pub sync_at: f64,
    pub bad: u32,
    /// (server time, x, y, z)
    pub hist: VecDeque<(f64, f64, f64, f64)>,
    pub rtts: VecDeque<(f64, f64)>,
    pub rtt: f64,
    /// Totem windows (server clock): a charmed bot shoots its own side until charm_until,
    /// a paralysed one holds still until freeze_until, enemy bots can't see a cloaked player,
    /// and the shield totem's bubble holds until shield_totem_until (they still shoot).
    pub charm_until: f64,
    pub freeze_until: f64,
    pub cloak_until: f64,
    pub shield_totem_until: f64,
    /// the three movement totems' windows (server clock): wings flight, leap jumps and
    /// the speed dash — all pop on death, like the shield totem's bubble
    pub wings_until: f64,
    /// the wings totem's cruise boundary (rise+hover — the shown window); the totem's
    /// judged window (wings_until) runs past it while the flyer glides back down
    pub wings_fly: f64,
    pub leap_until: f64,
    pub speed_until: f64,
}

fn bucket(b: &mut (f64, f64), rate: f64, burst: f64, t: f64) -> bool {
    b.0 = burst.min(b.0 + (t - b.1) * rate);
    b.1 = t;
    if b.0 < 1.0 {
        return false;
    }
    b.0 -= 1.0;
    true
}

impl Player {
    fn new(user: Value, slot: i64, ch: i64, t: f64, team: u8) -> Self {
        Self {
            user, slot, team, bot: None, ch, x: 0.0, y: 0.0, z: 0.0, vx: 0.0, vz: 0.0, r: 0, pt: 0, hp: HP,
            hp_original: HP, w: 0,
            mag: std::array::from_fn(|i| WEAPONS[i].mag), res: std::array::from_fn(|i| WEAPONS[i].reserve),
            last: std::array::from_fn(|_| VecDeque::new()), ready: -(1 << 30), reload: None, n: 0,
            kills: 0, deaths: 0, dead: false, respawn_at: 0.0, life: 0, spawn_t: t, protect: 0.0,
            drop: false, drop_t: 0.0,
            away: None, gone: false, q: None, q0: 0, t0: t, off: None, qmax: None, at: t, air_at: t,
            pb: (POS_BURST, t), fb: (FIRE_BURST, t), ob: (OP_BURST, t), fix_at: -1e9, sync_at: -1e9, bad: 0,
            hist: VecDeque::new(), rtts: VecDeque::new(), rtt: 0.0,
            charm_until: 0.0, freeze_until: 0.0, cloak_until: 0.0, shield_totem_until: 0.0,
            wings_until: 0.0, wings_fly: 0.0, leap_until: 0.0, speed_until: 0.0,
        }
    }

    fn is_bot(&self) -> bool {
        self.bot.is_some()
    }

    /// The drop shield: a parachute glide in flight. Nothing hurts a glider (they can
    /// still shoot out); landing or the DROP_TIME window closes it. The shield totem
    /// grants the same untouchable bubble for SHIELD_TOTEM seconds.
    fn shielded(&self, t: f64) -> bool {
        (self.drop && self.drop_t > 0.0 && (t - self.drop_t) < DROP_TIME) || self.shield_totem_until > t
    }

    fn place(&mut self, s: [f64; 4], t: f64) {
        self.x = s[0];
        self.y = s[1];
        self.z = s[2];
        // v4: a life starting well above the ground is a parachute drop — the referee lets
        // it ride the sky for DROP_TIME instead of snapping it mid-glide, and clears that
        // flag once it touches down (regular ground respawns land instantly anyway).
        self.drop = s[1] > 0.5;
        self.drop_t = if self.drop { t } else { 0.0 };
        self.r = ((s[3] * 100.0) as i64).rem_euclid(36000);
        self.pt = 0;
        self.hp = HP;
        self.hp_original = HP;
        self.dead = false;
        self.mag = std::array::from_fn(|i| WEAPONS[i].mag);
        self.res = std::array::from_fn(|i| WEAPONS[i].reserve);
        // a bot draws a fresh weapon every life, launcher included (like the solo match)
        if self.bot.is_some() {
            self.w = (mb32((self.slot as u32).wrapping_mul(131)
                .wrapping_add((t * 1000.0) as i64 as u32) & 0xFFFF) * NW as f64) as usize;
        }
        self.reload = None;
        self.spawn_t = t;
        self.protect = t + PROTECT;
        self.air_at = t;
        self.n = 0; // the spread sequence restarts each life
        self.charm_until = 0.0;
        self.freeze_until = 0.0;
        self.shield_totem_until = 0.0;   // a death pops the shield totem's bubble too
        self.wings_until = 0.0;
        self.wings_fly = 0.0;
        self.leap_until = 0.0;
        self.speed_until = 0.0;
        self.life += 1;
        self.hist = VecDeque::from([(t, s[0], s[1], s[2])]);
        if let Some(b) = &mut self.bot {
            b.goal = None;
            b.target = None;
            b.seen_at = None;
            b.fire_at = -1e9;
            b.aim_y = s[3];
            b.aim_p = 0.0;
            b.err_y = 0.0;
            b.err_p = 0.0;
            b.err_at = 0.0;
            b.body = Body { x: s[0], y: s[1], z: s[2], vx: 0.0, vy: 0.0, vz: 0.0, g: true };
            // start the patrol from the waypoint nearest this spawn
            b.wpi = WAYPOINTS.iter().enumerate()
                .min_by(|(_, a), (_, c)| ((a.0 - s[0]).powi(2) + (a.1 - s[2]).powi(2))
                    .total_cmp(&((c.0 - s[0]).powi(2) + (c.1 - s[2]).powi(2))))
                .map(|(i, _)| i).unwrap_or(0);
        }
    }

    /// The sender's clock may never get more than CLOCK_LEAD ahead of ours.
    fn lead_ok(&mut self, q: i64, t: f64) -> bool {
        if self.qmax.is_none() {
            self.q0 = q;
            self.t0 = t;
            self.qmax = Some(q);
            return true;
        }
        (q - self.q0) as f64 / 100.0 <= (t - self.t0) + CLOCK_LEAD
    }

    fn seen(&mut self, q: i64, t: f64) {
        let sample = t - q as f64 / 100.0;
        if self.off.map(|o| sample < o).unwrap_or(true) {
            self.off = Some(sample);
        }
        if self.qmax.map(|m| q > m).unwrap_or(true) {
            self.qmax = Some(q);
        }
    }

    fn this_life(&self, msg: &Value) -> bool {
        as_num(msg.get("e"), 0.0, (1u64 << 30) as f64).map(|e| e as i64 == self.life).unwrap_or(true)
    }

    fn finish_reload(&mut self, q: i64) {
        if let Some((w, _, end)) = self.reload {
            if q >= end {
                let take = (WEAPONS[w].mag - self.mag[w]).min(self.res[w]);
                self.mag[w] += take;
                self.res[w] -= take;
                self.reload = None;
            }
        }
    }

    /// Where this player was at server time `at`, from the history.
    pub fn where_at(&self, at: f64) -> (f64, f64, f64) {
        let h = &self.hist;
        let (Some(first), Some(last)) = (h.front(), h.back()) else { return (self.x, self.y, self.z) };
        if at <= first.0 {
            return (first.1, first.2, first.3);
        }
        if at >= last.0 {
            return (last.1, last.2, last.3);
        }
        let (mut lo, mut hi) = (0usize, h.len() - 1);
        while hi - lo > 1 {
            let mid = (lo + hi) / 2;
            if h[mid].0 <= at {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        let (a, b) = (h[lo], h[hi]);
        let span = b.0 - a.0;
        let u = (at - a.0) / if span == 0.0 { 1.0 } else { span };
        (a.1 + (b.1 - a.1) * u, a.2 + (b.2 - a.2) * u, a.3 + (b.3 - a.3) * u)
    }
}

/// The spawn indices of one side: the first half of the list is amber's, the
/// rest cyan's.
/// v4: a respawn isn't the spawn corner the life started in — it's a parachute drop over a
/// randomly-chosen spawn point's XZ (deterministic via the life/slot seed, never rand()),
/// lifted to DROP_HEIGHT so the player glides down and steers for the DROP_TIME window.
fn drop_spawn(slot: i64, life: i64) -> [f64; 4] {
    let seed = ((slot as u32).wrapping_mul(131).wrapping_add((life * 1000) as u32)) & 0xFFFF;
    let k = (mb32(seed) * MAP.spawns.len() as f64) as usize;
    let mut s = MAP.spawns[k.min(MAP.spawns.len() - 1)];
    s[1] = DROP_HEIGHT;
    s
}

fn side_spawns(team: u8) -> std::ops::Range<usize> {
    let n = MAP.spawns.len();
    let half = n / 2;
    if team == 0 { 0..half } else { half..n }
}

/// One room's match. idle -> warmup (countdown) -> round -> done.
#[derive(Debug)]
pub struct Strike {
    pub phase: Phase,
    pub minutes: i64,
    pub limit: i64,
    /// The host's X in "X vs X".
    pub tsize: i64,
    /// The host-picked MP bot skill. Every bot on BOTH sides plays at this — bot-filled
    /// teams are how uneven humans meet a fair lobby, so one side can't get sharper
    /// filler bots than the other. Resolved at start() into the multipliers below.
    pub bot_diff: &'static str,
    /// aim error multiplier (higher = dumber), reaction multiplier (higher = slower).
    pub bot_aim: f64,
    pub bot_react: f64,
    /// turn multiplier (higher = snappier aim swing) and refire multiplier (higher = longer
    /// gaps between the bot's own shots — plain seconds, the client DIFFS use multipliers
    /// on the weapon interval).
    pub bot_turn: f64,
    pub bot_rate: f64,
    /// Team kill totals [amber, cyan].
    pub scores: [i64; 2],
    pub go_at: f64,
    pub ends_at: f64,
    pub players: Vec<(String, Player)>,
    pub chars: BTreeMap<String, i64>,
    pub results: Option<Value>,
    pub items: Vec<Option<f64>>,
    /// Totem slots appended to the map's pickups this round: kind per slot ("" = a map pack).
    /// Mirrors the client: base slots are health/ammo from the map JSON, totems append at
    /// runtime and reset each match, so the map file itself never changes.
    pub kinds: Vec<&'static str>,
    /// Where each appended totem slot sits (index-aligned with kinds, base slots empty).
    pub spots: Vec<[f64; 3]>,
    /// When the next totem drops (round clock); None until a round starts.
    pub totem_at: Option<f64>,
    /// Totem drop counter: the seed, so every observer derives the same spot and kind.
    pub totem_k: i64,
    /// When the next loose pack drops (its own clock, independent of the totems).
    pub pack_at: Option<f64>,
    /// Loose-pack drop counter: the seed, same derivation as the totems.
    pub pack_k: i64,
    /// Decoy ghosts in flight: id, where it stands, whose view it copied, until when.
    pub decoys: Vec<Decoy>,
    pub dirty: Vec<String>,
    pub shots: Vec<Value>,
    /// Blast points since the last snapshot ("b" rows): [x, y, z] in cm.
    pub booms: Vec<Value>,
    /// detonate()'s pending shot echoes, flushed by flush_echoes().
    detonate_echoes: Vec<(String, [f64; 3], i64, i64)>,
    /// Rockets in flight, flown in tick(); snap() copies them to "r" rows.
    pub rockets: Vec<Rocket>,
    /// Next rocket id (also the index the client reconciles by).
    pub rocket_k: i64,
    pub pending: Vec<(&'static str, Value)>,
    pub k: i64,
    pub sent: VecDeque<(i64, f64)>,
}

impl Default for Strike {
    fn default() -> Self {
        Self::new()
    }
}

impl Strike {
    pub fn new() -> Self {
        Self {
            phase: Phase::Idle, minutes: 5, limit: 30, tsize: 2, scores: [0, 0], go_at: 0.0, ends_at: 0.0,
            bot_diff: "normal", bot_aim: 1.0, bot_react: 1.0, bot_turn: 1.0, bot_rate: 1.0,
            players: Vec::new(), chars: BTreeMap::new(), results: None, items: Vec::new(), dirty: Vec::new(),
            kinds: Vec::new(), spots: Vec::new(), totem_at: None, totem_k: 0, pack_at: None, pack_k: 0,
            decoys: Vec::new(),
            shots: Vec::new(), booms: Vec::new(), detonate_echoes: Vec::new(), rockets: Vec::new(),
            rocket_k: 0, pending: Vec::new(), k: 0, sent: VecDeque::new(),
        }
    }

    /// Apply one MP bot-skill level to every bot the match will field (aim/reaction/turn
    /// are multipliers on the BOT_* constants; rate multiplies the gap between a bot's own
    /// shots). An unknown id — including one a crafted wire message could carry — quietly
    /// falls back to normal, never to a custom super/woozy bot.
    pub fn set_bot_diff(&mut self, id: &str) {
        let (aim, react, turn, rate) = match id {
            "easy" => (3.0, 2.0, 0.5, 1.6),
            "hard" => (0.5, 0.35, 1.6, 0.8),
            "sweaty" => (0.22, 0.22, 2.2, 0.7),
            _ => (1.0, 1.0, 1.0, 1.0),
        };
        self.bot_diff = match id {
            "easy" => "easy",
            "hard" => "hard",
            "sweaty" => "sweaty",
            _ => "normal",
        };
        self.bot_aim = aim;
        self.bot_react = react;
        self.bot_turn = turn;
        self.bot_rate = rate;
    }

    pub fn player(&self, uid: &str) -> Option<&Player> {
        self.players.iter().find(|(u, _)| u == uid).map(|(_, p)| p)
    }

    pub fn player_mut(&mut self, uid: &str) -> Option<&mut Player> {
        self.players.iter_mut().find(|(u, _)| u == uid).map(|(_, p)| p)
    }

    fn idx(&self, uid: &str) -> Option<usize> {
        self.players.iter().position(|(u, _)| u == uid)
    }

    fn mark(&mut self, uid: &str) {
        if !self.dirty.iter().any(|u| u == uid) {
            self.dirty.push(uid.to_string());
        }
    }

    pub fn ms_left(&self, t: f64) -> i64 {
        match self.phase {
            Phase::Round => (((self.ends_at - t) * 1000.0) as i64).max(0),
            Phase::Warmup => self.minutes * 60000,
            _ => 0,
        }
    }

    fn ms(&self, at: f64) -> i64 {
        ((at - self.go_at) * 1000.0).round_ties_even() as i64
    }

    pub fn view(&self, t: f64) -> Value {
        let players: Vec<Value> = self.players.iter().map(|(_, p)| json!({
            "user": p.user, "slot": p.slot, "team": p.team, "bot": if p.is_bot() { 1 } else { 0 },
            "char": p.ch, "kills": p.kills, "deaths": p.deaths, "hp": p.hp,
            "dead": p.dead, "away": p.away.is_some(), "gone": p.gone, "w": p.w, "x": cm(p.x), "y": cm(p.y),
            "z": cm(p.z), "r": p.r, "e": p.life})).collect();
        json!({"map": MAP.id, "phase": self.phase.as_str(), "minutes": self.minutes, "limit": self.limit,
               "tsize": self.tsize, "scores": self.scores,
               "goInMs": if self.phase == Phase::Warmup { (((self.go_at - t) * 1000.0) as i64).max(0) } else { 0 },
               "msLeft": self.ms_left(t), "players": players,
               "items": self.items.iter().map(|a| if a.is_none() { 1 } else { 0 }).collect::<Vec<_>>(),
               "kinds": self.kinds.iter().map(|k| if k.is_empty() { "" } else { *k }).collect::<Vec<_>>(),
               "results": self.results, "chars": self.chars})
    }

    pub fn order(&self) -> Vec<String> {
        let mut v: Vec<&(String, Player)> = self.players.iter().collect();
        v.sort_by_key(|(_, p)| (p.team, -p.kills, p.deaths, p.slot));
        v.into_iter().map(|(u, _)| u.clone()).collect()
    }

    pub fn char(&mut self, uid: &str, msg: &Value) -> Option<i64> {
        let c = as_num(msg.get("c"), -1.0, CHARS as f64)?;
        if c != c.trunc() || !(0.0..CHARS as f64).contains(&c) {
            return None;
        }
        let c = c as i64;
        self.chars.insert(uid.to_string(), c);
        if let Some(p) = self.player_mut(uid) {
            p.ch = c;
        }
        Some(c)
    }

    /// Top each side up to the host's X with bots; retire bots when humans
    /// abound (the most recently added first).
    fn fill_bots(&mut self, t: f64) {
        for team in 0u8..2 {
            let want = self.tsize.max(0) as usize;
            let mut count = self.players.iter().filter(|(_, p)| p.team == team && !p.gone).count();
            while count > want {
                let pick = self.players.iter()
                    .filter(|(_, p)| p.team == team && !p.gone && p.is_bot())
                    .map(|(_, p)| p.slot).max();
                let Some(slot) = pick else { break };
                if let Some(p) = self.players.iter_mut().find(|(_, p)| p.team == team && p.slot == slot) {
                    p.1.gone = true;
                }
                count -= 1;
            }
            let mut k = 0usize;
            while count < want {
                let uid = format!("bot-{}-{}", team, k);
                if self.idx(&uid).is_some() {
                    k += 1;
                    continue;
                }
                let slot = 1 + self.players.iter().map(|(_, p)| p.slot).max().unwrap_or(-1);
                let name = BOT_NAMES[(team as usize * 4 + k) % BOT_NAMES.len()];
                let col = BOT_COLORS[(team % 2) as usize];
                let pubv = json!({"id": uid, "name": name, "color": col, "bot": 1});
                let spawns: Vec<usize> = side_spawns(team).collect();
                let mut p = Player::new(pubv, slot, slot % CHARS, t, team);
                p.bot = Some(Bot { think_at: t + BOT_THINK, fire_at: -1e9, ..Bot::default() });
                p.place(MAP.spawns[spawns[k % spawns.len()]], t);
                self.players.push((uid.clone(), p));
                self.mark(&uid);
                count += 1;
                k += 1;
            }
        }
    }

    /// The side a joining human takes: the short one (bots there get retired).
    fn human_side(&self, uid: &str) -> u8 {
        if let Some(p) = self.player(uid) {
            return p.team;
        }
        let mut counts = [0usize; 2];
        for (_, p) in &self.players {
            if !p.is_bot() && !p.gone {
                counts[p.team as usize] += 1;
            }
        }
        if counts[0] <= counts[1] { 0 } else { 1 }
    }

    pub fn start(&mut self, members: &[(String, Value)], minutes: &Value, kills: &Value, tsize: &Value,
                 diff: &Value, t: f64) -> Option<String> {
        if self.running() {
            return Some("a match is already on: the host can end it first".into());
        }
        // one skill for every bot on BOTH sides — bot-filled teams are how uneven humans
        // meet a fair lobby, so no team may get sharper fillers than the other
        self.set_bot_diff(diff.as_str().unwrap_or("normal"));
        // teams stay on the list (5 is the roster cap); minutes and kills are the host's
        // own scoreline now — any value inside the clamps, the lists are just presets
        let pick = |v: &Value, opts: &[i64], dflt: i64| {
            as_num(Some(v), 0.0, 1000.0).map(|f| f as i64).filter(|f| opts.contains(f)).unwrap_or(dflt)
        };
        let clamp_to = |v: &Value, lo: i64, hi: i64, dflt: i64| {
            // absent or unusable falls back to the default; a present value clamps into [lo, hi]
            as_num(Some(v), lo as f64, hi as f64).map(|f| f as i64).unwrap_or(dflt)
        };
        self.minutes = clamp_to(minutes, 1, 60, 5);
        self.limit = clamp_to(kills, 1, 500, 30);
        self.tsize = pick(tsize, &TEAM_SIZES, 2);
        self.phase = Phase::Warmup;
        self.go_at = t + COUNTDOWN;
        self.results = None;
        self.players = Vec::new();
        self.scores = [0, 0];
        // nothing is on the map at the start: the fixed packs stay dark (1e9 = past any
        // round, so the refill timer never lights them) and every item arrives mid-match
        // from a drop clock — totems every TOTEM_GAP, packs every PACK_GAP, each running
        // regardless of whether the previous drop was taken
        self.items = vec![Some(1e9); MAP.pickups.len()];
        self.kinds = vec![""; MAP.pickups.len()];
        self.spots = vec![[0.0, 0.0, 0.0]; MAP.pickups.len()];
        self.totem_at = None;
        self.totem_k = 0;
        self.pack_at = None;
        self.pack_k = 0;
        self.ensure_drop_slots();   // the recycled drop slots exist (empty) for the whole match
        self.decoys.clear();
        self.shots.clear();
        self.booms.clear();
        self.rockets.clear();
        self.pending.clear();
        self.sent.clear();
        // humans first, split evenly across the two sides
        for (k, (uid, pubv)) in members.iter().take(MAX_PLAYERS).enumerate() {
            let team = (k % 2) as u8;
            let ch = self.chars.get(uid).copied().unwrap_or(k as i64 % CHARS);
            let mut p = Player::new(pubv.clone(), k as i64, ch, t, team);
            let spawns: Vec<usize> = side_spawns(team).collect();
            p.place(MAP.spawns[spawns[(k / 2) % spawns.len()]], t);
            self.players.push((uid.clone(), p));
        }
        self.fill_bots(t);
        self.dirty = self.players.iter().map(|(u, _)| u.clone()).collect();
        None
    }

    pub fn end(&mut self) {
        self.phase = Phase::Idle;
        self.players.clear();
        self.results = None;
        self.shots.clear();
        self.booms.clear();
        self.rockets.clear();
        self.pending.clear();
    }

    pub fn running(&self) -> bool {
        matches!(self.phase, Phase::Warmup | Phase::Round)
    }

    pub fn active(&self) -> usize {
        self.players.iter().filter(|(_, p)| !p.gone).count()
    }

    /// A lobby member (re)joined while a match is on: a dropped player gets
    /// their score back; someone new drops in. True if the match changed.
    pub fn enter(&mut self, uid: &str, pubv: Value, t: f64) -> bool {
        if !self.running() {
            return false;
        }
        let phase = self.phase;
        if let Some(p) = self.player_mut(uid) {
            let was_gone = p.gone;
            p.away = None;
            p.gone = false;
            p.user = pubv;
            p.q = None; // a reloaded page starts a new clock
            p.off = None;
            p.qmax = None;
            p.rtts.clear();
            p.pb = (POS_BURST, t);
            p.fb = (FIRE_BURST, t);
            p.ob = (OP_BURST, t);
            if was_gone && phase == Phase::Round && !p.dead {
                p.dead = true;
                p.respawn_at = t;
            }
            self.mark(uid);
            self.fill_bots(t);
            return true;
        }
        let humans = self.players.iter().filter(|(_, p)| !p.is_bot() && !p.gone).count();
        if humans >= MAX_PLAYERS {
            return false;
        }
        let team = self.human_side(uid);
        let slot = 1 + self.players.iter().map(|(_, p)| p.slot).max().unwrap_or(-1);
        let ch = self.chars.get(uid).copied().unwrap_or(slot % CHARS);
        let mut p = Player::new(pubv, slot, ch, t, team);
        if phase == Phase::Warmup {
            let spawns: Vec<usize> = side_spawns(team).collect();
            p.place(MAP.spawns[spawns[(slot as usize * 5) % spawns.len()]], t);
        } else {
            p.dead = true;
            p.respawn_at = t;
        }
        self.players.push((uid.to_string(), p));
        self.mark(uid);
        self.fill_bots(t);
        true
    }

    /// A player left: a blip keeps their place for GRACE, an explicit leave benches them.
    pub fn drop(&mut self, uid: &str, t: f64, blip: bool) -> bool {
        let running = self.running();
        let Some(p) = self.player_mut(uid) else { return false };
        if !running || p.gone {
            return false;
        }
        if blip {
            p.away = Some(t);
        } else {
            p.gone = true;
        }
        self.mark(uid);
        self.fill_bots(t);
        true
    }

    fn ack(&self, p: &mut Player, msg: &Value, q: Option<i64>, t: f64) {
        let (Some(kk), Some(q), Some(off)) = (as_num(msg.get("k"), 0.0, (1u64 << 30) as f64), q, p.off) else {
            return;
        };
        let kk = kk as i64;
        let arrived = as_num(msg.get("kq"), 0.0, ((1u64 << 30) - 1) as f64)
            .map(|v| v as i64)
            .filter(|kq| q - 100 <= *kq && *kq <= q)
            .unwrap_or(q);
        let mapped = arrived as f64 / 100.0 + off;
        for &(tick, at) in self.sent.iter().rev() {
            if tick == kk {
                p.rtts.push_back((t, (mapped - at).max(0.0)));
                break;
            }
            if tick < kk {
                break;
            }
        }
        while p.rtts.front().map(|r| t - r.0 > RTT_WINDOW).unwrap_or(false) {
            p.rtts.pop_front();
        }
        if !p.rtts.is_empty() {
            p.rtt = p.rtts.iter().map(|r| r.1).fold(f64::INFINITY, f64::min);
        }
    }

    /// A position frame. Returns (accepted, correction to send back).
    pub fn pos(&mut self, uid: &str, msg: &Value, t: f64) -> (bool, Option<Value>) {
        let Some(i) = self.idx(uid) else { return (false, None) };
        if self.phase != Phase::Round || self.players[i].1.is_bot() {
            return (false, None);
        }
        let mut p = std::mem::replace(&mut self.players[i].1, Player::new(Value::Null, 0, 0, t, 0));
        let res = self.pos_inner(&mut p, msg, t);
        self.players[i].1 = p;
        if res.0 {
            self.mark(uid);
            self.pickups(i, t);
        }
        res
    }

    fn pos_inner(&self, p: &mut Player, msg: &Value, t: f64) -> (bool, Option<Value>) {
        if p.dead || p.gone || !bucket(&mut p.pb, POS_RATE, POS_BURST, t) {
            return (false, None);
        }
        let g = |k: &str| as_num(msg.get(k), -1e6, 1e6);
        let (Some(x), Some(y), Some(z), Some(r)) = (g("x"), g("y"), g("z"), g("r")) else { return (false, None) };
        let pt = as_num(msg.get("p"), -9000.0, 9000.0);
        if !p.this_life(msg) {
            return (false, None);
        }
        let q = as_num(msg.get("q"), 0.0, ((1u64 << 30) - 1) as f64).map(|q| q as i64);
        let dt = match (q, p.q) {
            (Some(q), Some(pq)) => {
                if q <= pq {
                    return (false, None); // older than what we have: stale, not cheating
                }
                if p.lead_ok(q, t) { (q - pq) as f64 / 100.0 } else { -1.0 }
            }
            _ => {
                if let Some(q) = q {
                    p.lead_ok(q, t);
                }
                (t - p.at).max(0.0)
            }
        };
        let sprint = msg.get("sp").is_some_and(|v| v.as_bool().unwrap_or(false));
        // the movement totems buy room while their windows run (the server itself judged
        // them, so a raised cap can't be claimed by just sending the flag): the speed
        // totem's dash outranks the sprint flag, the leap totem's jumps rise taller
        let speed_cap = if p.speed_until > t { SPRINT_MAX_SPEED * SPEED_MULT }
            else if sprint { SPRINT_MAX_SPEED } else { MAX_SPEED };
        let up_cap = if p.leap_until > t { JUMP_V * LEAP_MULT } else { JUMP_V };
        let (xm, ym, zm) = (x / 100.0, y / 100.0, z / 100.0);
        let b = MAP.bounds();
        let mut ok = dt >= 0.0 && b[0] <= xm && xm <= b[3] && b[1] <= ym && ym <= b[4] && b[2] <= zm && zm <= b[5];
        if ok {
            let across = (xm - p.x).hypot(zm - p.z);
            let dy = ym - p.y;
            ok = across <= speed_cap * dt + SLACK && dy <= up_cap * dt + STEP_H + SLACK
                && -dy <= FALL_MAX * dt + SLACK && overlaps(&MAP, xm, ym, zm, SHRINK).is_none();
        }
        let clock = match (q, p.off) {
            (Some(q), Some(off)) => q as f64 / 100.0 + off,
            _ => t,
        };
        let mut grounded = false;
        if ok {
            grounded = overlaps(&MAP, xm, ym - 0.1, zm, 0.0).is_some();
            // v4: a parachute-dropping life legitimately rides the sky for DROP_TIME after its
            // spawn — buy it the fall instead of snapping it back to the ground mid-glide.
            let air_let_up = p.drop && p.drop_t > 0.0 && (clock - p.drop_t) < DROP_TIME;
            // the wings totem parks the air clock too: a flyer is off the ground by design
            if !grounded && !air_let_up && p.wings_until <= t && clock - p.air_at > MAX_AIR {
                ok = false;
            }
            if grounded && p.drop && (clock - p.drop_t) >= 0.0 {
                p.drop = false;
                p.drop_t = 0.0;
            }
        }
        if !ok {
            p.bad += 1;
            if t - p.fix_at < FIX_GAP {
                return (false, None);
            }
            p.fix_at = t;
            return (false, Some(json!({"x": cm(p.x), "y": cm(p.y), "z": cm(p.z), "e": p.life})));
        }
        let mapped = match q {
            Some(q) => {
                p.seen(q, t);
                p.q = Some(q);
                q as f64 / 100.0 + p.off.unwrap_or(0.0)
            }
            None => t,
        };
        if grounded {
            p.air_at = mapped;
        }
        p.x = xm;
        p.y = ym;
        p.z = zm;
        p.r = (r as i64).rem_euclid(36000);
        p.pt = pt.map(|v| v as i64).unwrap_or(0);
        p.at = t;
        p.hist.push_back((mapped, xm, ym, zm));
        while p.hist.len() > 2 && mapped - p.hist[0].0 > HISTORY {
            p.hist.pop_front();
        }
        self.ack(p, msg, q, t);
        (true, None)
    }

    fn pickups(&mut self, i: usize, t: f64) {
        let n_map = MAP.pickups.len();
        let uid = self.players[i].0.clone();
        for j in 0..self.items.len() {
            if self.items[j].is_some() {
                continue;
            }
            // a drop slot (past the map's own packs): its spot came with the "totem"/"pack"
            // event. Loose packs refill like the map's own; totems are judged here, exactly
            // like the solo client's local take
            if j >= n_map {
                let kind = self.kinds[j];
                if kind.is_empty() {
                    continue;
                }
                let at = self.spots[j];
                let (x, y, z) = (self.players[i].1.x, self.players[i].1.y, self.players[i].1.z);
                if (x - at[0]).hypot(z - at[2]) > PICK_R || (y - at[1]).abs() > 1.2 {
                    continue;
                }
                if kind == "health" || kind == "ammo" {
                    let p = &mut self.players[i].1;
                    if kind == "health" {
                        if p.hp >= HP {
                            continue;
                        }
                        p.hp = HP.min(p.hp + HEAL);
                    } else {
                        if (0..NW).all(|w| p.res[w] >= WEAPONS[w].reserve * 2) {
                            continue;
                        }
                        for w in 0..NW {
                            p.res[w] = (WEAPONS[w].reserve * 2).min(p.res[w] + WEAPONS[w].pack);
                        }
                    }
                    // a taken ring slot does NOT respawn in place: the drop clock replaces
                    // it with a fresh drop (new kind, new spot) within one gap — push its
                    // return past any round length so no "item" event resurrects it
                    self.items[j] = Some(t + 1e9);
                    let p = &self.players[i].1;
                    self.pending.push(("pick", json!({"i": j, "user": uid, "hp": p.hp, "res": p.res,
                                                       "kind": kind})));
                    continue;
                }
                if !self.take_totem(i, kind, t) {
                    continue;
                }
                // same for a taken totem slot: the ring replaces it, not the refill timer
                self.items[j] = Some(t + 1e9);
                let p = &self.players[i].1;
                let mut ev = json!({"i": j, "user": uid, "hp": p.hp, "res": p.res, "kind": kind});
                if kind == "teleport" {
                    // the destination, so the taker's client snaps its prediction there
                    ev["tx"] = json!(cm(p.x));
                    ev["tz"] = json!(cm(p.z));
                }
                self.pending.push(("pick", ev));
                continue;
            }
            let pk = &MAP.pickups[j];
            let (x, y, z) = (self.players[i].1.x, self.players[i].1.y, self.players[i].1.z);
            let p = &mut self.players[i].1;
            if (x - pk.at[0]).hypot(z - pk.at[2]) > PICK_R || (y - pk.at[1]).abs() > 1.2 {
                continue;
            }
            if pk.health {
                if p.hp >= HP {
                    continue;
                }
                p.hp = HP.min(p.hp + HEAL);
            } else {
                if (0..NW).all(|w| p.res[w] >= WEAPONS[w].reserve * 2) {
                    continue;
                }
                for w in 0..NW {
                    p.res[w] = (WEAPONS[w].reserve * 2).min(p.res[w] + WEAPONS[w].pack);
                }
            }
            let ev = json!({"i": j, "user": uid, "hp": p.hp, "res": p.res});
            self.items[j] = Some(t + PICKUP_BACK);
            self.pending.push(("pick", ev));
        }
    }

    /// Pick up a totem slot (kind != ""): the effect is server-judged in MP, exactly what
    /// the solo client's totemApply() does locally. Returns false when nothing was eligible
    /// (the slot stays for someone it can help).
    fn take_totem(&mut self, i: usize, kind: &str, t: f64) -> bool {
        let me = self.players[i].0.clone();
        let my_team = self.players[i].1.team;
        match kind {
            "mole" => {
                // the first living enemy bot in slot order turns on its own side
                let vid = self.players.iter()
                    .filter(|(u, v)| u.as_str() != me && v.is_bot() && !v.dead && !v.gone && v.team != my_team)
                    .min_by_key(|(_, v)| v.slot).map(|(u, _)| u.clone());
                let Some(vid) = vid else { return false };
                self.player_mut(&vid).unwrap().charm_until = t + CHARM;
                self.mark(&vid);
                true
            }
            "freeze" => {
                // the nearest living enemy bot holds still
                let (x, z) = (self.players[i].1.x, self.players[i].1.z);
                let vid = self.players.iter()
                    .filter(|(u, v)| u.as_str() != me && v.is_bot() && !v.dead && !v.gone && v.team != my_team)
                    .min_by(|a, b| ((a.1.x - x).hypot(a.1.z - z)).total_cmp(&(b.1.x - x).hypot(b.1.z - z)))
                    .map(|(u, _)| u.clone());
                let Some(vid) = vid else { return false };
                self.player_mut(&vid).unwrap().freeze_until = t + FREEZE;
                self.mark(&vid);
                true
            }
            "decoy" => {
                // a ghost of the taker, three metres ahead of where they stand
                let (x, z, yaw) = {
                    let p = &self.players[i].1;
                    (p.x, p.z, p.r as f64 / 100.0)
                };
                let vx = yaw.to_radians().sin();
                let vz = -(yaw.to_radians().cos());
                let (mut gx, mut gz) = (x + vx * 3.0, z + vz * 3.0);
                for k in 0..20 {
                    if overlaps(&MAP, gx, 0.1, gz, 0.2).is_none() {
                        break;
                    }
                    gx = x + vx * (3.0 + k as f64 * 0.3);
                    gz = z + vz * (3.0 + k as f64 * 0.3);
                }
                self.decoys.push(Decoy { id: self.totem_k, x: gx, y: 0.0, z: gz, yaw,
                                         owner: me.clone(), until: t + DECOY });
                true
            }
            "cloak" => {
                self.players[i].1.cloak_until = t + CLOAK;
                true
            }
            "shield" => {
                // the drop shield, bottled: nothing hurts the taker while they keep shooting
                self.players[i].1.shield_totem_until = t + SHIELD_TOTEM;
                true
            }
            "wings" => {
                // the window the referee judges covers the whole flight: rise, cruise, then
                // the automatic glide back down — the client flies all three without input.
                // The shown "wings on" bit and the cruise physics use the shorter cruise
                // boundary; the extra WINGS_LAND only relaxes the air clock while gliding.
                self.players[i].1.wings_until = t + WINGS + WINGS_LAND;
                self.players[i].1.wings_fly = t + WINGS;
                true
            }
            "leap" => {
                self.players[i].1.leap_until = t + LEAP;
                true
            }
            "speed" => {
                self.players[i].1.speed_until = t + SPEED;
                true
            }
            "teleport" => {
                // instant: a random open ground spot, seeded from the take counter so
                // every hop lands somewhere fresh. The referee plants the body there
                // itself (hist rebuilt, air clock reset) so the next pos frame starts
                // from the destination instead of earning a "teleport" correction.
                self.totem_k += 1;
                let (tx, tz) = self.open_spot((self.totem_k as u32).wrapping_mul(2654435761));
                let p = &mut self.players[i].1;
                p.x = tx;
                p.y = 0.0;
                p.z = tz;
                p.hist = VecDeque::from([(t, tx, 0.0, tz)]);
                p.air_at = t;
                p.wings_until = 0.0;
                p.wings_fly = 0.0;
                true
            }
            _ => false,
        }
    }

    /// The drop-slot layout: [first totem slot, first pack slot, end). Past the map's own
    /// packs — fixed all match, so a drop's index means the same thing everywhere.
    fn drop_rings(&self) -> (usize, usize, usize) {
        let n_map = MAP.pickups.len();
        (n_map, n_map + DROP_SLOTS, n_map + DROP_SLOTS + PACK_SLOTS)
    }

    /// The drop-slot entries exist (kind "" = empty) for the whole round; drops rewrite
    /// them round-robin. Called at start and before any drop lands.
    fn ensure_drop_slots(&mut self) {
        let end = self.drop_rings().2;
        while self.kinds.len() < end {
            self.items.push(None);
            self.kinds.push("");
            self.spots.push([0.0, 0.0, 0.0]);
        }
    }

    /// A random open spot for a drop: hashed from the seed, nudged off any box.
    fn open_spot(&self, seed: u32) -> (f64, f64) {
        let mut x = -17.0 + hash_match(seed, 2) * 34.0;
        let mut z = -10.0 + hash_match(seed, 3) * 20.0;
        for k in 0..60 {
            if overlaps(&MAP, x, 0.1, z, 0.3).is_none() {
                break;
            }
            x = -17.0 + hash_match(seed ^ k, 2) * 34.0;
            z = -10.0 + hash_match(seed ^ k, 3) * 20.0;
        }
        (x, z)
    }

    /// Drop one totem: a fresh kind and spot derived from the drop counter, landing in
    /// the OLDEST totem slot of the ring (replacing whatever sat there, taken or not),
    /// announced to the room by the "totem" event tick() emits. (can_mole: the mole only
    /// drops when the other side fields a bot — it can't turn a human. Repetitions
    /// allowed: kinds repeat by chance.)
    fn spawn_totem(&mut self, can_mole: bool) {
        self.ensure_drop_slots();
        let a = self.drop_rings().0;
        self.totem_k += 1;
        let seed = (self.totem_k as u32).wrapping_mul(0x9E37_79B9).wrapping_add(self.k as u32);
        let kinds: Vec<&'static str> = if can_mole { TOTEM_KINDS.to_vec() } else { TOTEM_KINDS[1..].to_vec() };
        let kind = kinds[(hash_match(seed, 1) * kinds.len() as f64) as usize % kinds.len()];
        let (x, z) = self.open_spot(seed);
        let i = a + (self.totem_k as usize - 1) % DROP_SLOTS;
        self.items[i] = None;
        self.kinds[i] = kind;
        self.spots[i] = [x, 0.0, z];
        self.pending.push(("totem", json!({"i": i, "kind": kind, "x": cm(x), "z": cm(z)})));
    }

    /// Drop one loose pack (health or ammo, even odds): its own clock, independent of the
    /// totems — same recycled-slot ring, same fresh random spot each time.
    fn spawn_pack(&mut self) {
        self.ensure_drop_slots();
        let b = self.drop_rings().1;
        self.pack_k += 1;
        let seed = (self.pack_k as u32).wrapping_mul(0x9E37_79B9).wrapping_add(self.k as u32)
            .wrapping_add(0x5F35_6495);
        let kind = if hash_match(seed, 4) >= 0.5 { "ammo" } else { "health" };
        let (x, z) = self.open_spot(seed);
        let i = b + (self.pack_k as usize - 1) % PACK_SLOTS;
        self.items[i] = None;
        self.kinds[i] = kind;
        self.spots[i] = [x, 0.0, z];
        self.pending.push(("pack", json!({"i": i, "kind": kind, "x": cm(x), "z": cm(z)})));
    }

    fn sync(p: &mut Player, t: f64) -> Option<Value> {
        if t - p.sync_at < FIX_GAP {
            return None;
        }
        p.sync_at = t;
        Some(json!({"w": p.w, "mag": p.mag, "res": p.res, "reloading": p.reload.is_some()}))
    }

    fn clock(p: &mut Player, msg: &Value, t: f64) -> Option<i64> {
        let q = as_num(msg.get("q"), 0.0, ((1u64 << 30) - 1) as f64)? as i64;
        if !p.lead_ok(q, t) {
            p.bad += 1;
            return None;
        }
        if p.qmax.map(|m| q < m - (FIRE_LATE * 100.0) as i64).unwrap_or(false) {
            return None;
        }
        p.seen(q, t);
        Some(q)
    }

    /// A shot. Returns (the judged shot, or None if refused; the shooter's ammo to
    /// send back when it was refused for ammo).
    pub fn fire(&mut self, uid: &str, msg: &Value, t: f64) -> (Option<Value>, Option<Value>) {
        let Some(i) = self.idx(uid) else { return (None, None) };
        if self.phase != Phase::Round {
            return (None, None);
        }
        let mut p = std::mem::replace(&mut self.players[i].1, Player::new(Value::Null, 0, 0, t, 0));
        let res = self.fire_inner(&mut p, msg, t);
        self.players[i].1 = p;
        let Some((w, o, d, reach, protect_ended)) = res.0 else { return (None, res.1) };
        if protect_ended {
            self.mark(uid);
        }
        // the launcher: don't judge a ray — spawn a rocket; tick() flies it and resolves the blast
        if w == NW - 1 {
            self.rocket_k += 1;
            let rocket = Rocket {
                id: self.rocket_k, shooter: uid.to_string(),
                x: o[0], y: o[1], z: o[2], px: o[0], py: o[1], pz: o[2],
                vx: d[0] * MISSILE_V, vy: d[1] * MISSILE_V, vz: d[2] * MISSILE_V, born: t,
            };
            self.rockets.push(rocket);
            let shot = json!([self.players[i].1.slot, w, cm(o[0]), cm(o[1]), cm(o[2]), -1, 0]);
            self.shots.push(shot.clone());
            return (Some(shot), None);
        }
        let team = self.players[i].1.team;
        let t_view = res.2;
        let mut best: Option<(usize, f64, bool)> = None;
        for (j, (vid, v)) in self.players.iter().enumerate() {
            if vid == uid || v.dead || v.gone || v.team == team || v.spawn_t > t_view {
                continue;
            }
            let (vx, vy, vz) = v.where_at(t_view);
            if let Some((th, head)) = ray_player(o, d, vx, vy, vz, reach) {
                if best.map(|b| th < b.1).unwrap_or(true) {
                    best = Some((j, th, head));
                }
            }
        }
        let end = best.map(|b| b.1).unwrap_or(reach);
        let shooter_slot = self.players[i].1.slot;
        let mut shot = vec![json!(shooter_slot), json!(w), json!(cm(o[0] + d[0] * end)),
                            json!(cm(o[1] + d[1] * end)), json!(cm(o[2] + d[2] * end)), json!(-1), json!(0)];
        if let Some((j, _, head)) = best {
            let (vid, v) = &mut self.players[j];
            let vid = vid.clone();
            shot[5] = json!(v.slot);
            shot[6] = json!(if head { 1 } else { 0 });
            let dmg = if v.protect > t || v.shielded(t) { 0 } else if head { WEAPONS[w].head } else { WEAPONS[w].damage };
            v.hp = (v.hp - dmg).max(0);
            let killed = dmg > 0 && v.hp <= 0;
            self.mark(&vid);
            if killed {
                self.kill(i, j, w, head, t);
            }
        }
        let shot = Value::Array(shot);
        self.shots.push(shot.clone());
        (Some(shot), None)
    }

    /// The shooter-side rules of a shot. Ok: (weapon, origin, direction, reach,
    /// protection ended) and the rewind time.
    #[allow(clippy::type_complexity)]
    fn fire_inner(&self, p: &mut Player, msg: &Value, t: f64)
        -> (Option<(usize, [f64; 3], [f64; 3], f64, bool)>, Option<Value>, f64)
    {
        let none = (None, None, 0.0);
        if p.dead || p.gone || p.is_bot() || !bucket(&mut p.fb, FIRE_RATE, FIRE_BURST, t) {
            return none;
        }
        let w = as_num(msg.get("w"), -1.0, 99.0);
        let g = |k: &str| as_num(msg.get(k), -1e6, 1e6);
        let (Some(w), Some(ox), Some(oy), Some(oz), Some(yaw), Some(pitch)) =
            (w, g("ox"), g("oy"), g("oz"), g("r"), as_num(msg.get("p"), -9000.0, 9000.0)) else { return none };
        if w != w.trunc() || !(0.0..WEAPONS.len() as f64).contains(&w) {
            return none;
        }
        let w = w as usize;
        if !p.this_life(msg) {
            return none;
        }
        let Some(q) = Self::clock(p, msg, t) else { return none };
        self.ack(p, msg, Some(q), t);
        let wp = &WEAPONS[w];
        p.finish_reload(q);
        let reloading = p.reload.map(|r| r.1 <= q).unwrap_or(false);
        if w != p.w || q < p.ready || reloading || p.mag[w] <= 0 {
            return (None, Self::sync(p, t), 0.0);
        }
        // the fire rate, on the shooter's clock, whatever order shots arrived in
        let gap = (wp.interval * 100.0).round() as i64 - 1;
        if p.last[w].iter().any(|lq| (q - lq).abs() < gap) {
            return none;
        }
        let o = [ox / 100.0, oy / 100.0, oz / 100.0];
        let lag = p.q.map(|pq| (q - pq).abs() as f64 / 100.0).unwrap_or(0.0);
        // SPRINT_MAX_SPEED here (playing can sprint mid-lag-window, and the complaint
        // is only meaningful when the origin is beyond even a sprinter's drift)
        if dist3(o, [p.x, p.y + EYE, p.z]) > ORIGIN_SLACK + SPRINT_MAX_SPEED * lag.min(FIRE_LATE) {
            p.bad += 1;
            return none;
        }
        p.mag[w] -= 1;
        p.last[w].push_back(q);
        while p.last[w].len() > 8 {
            p.last[w].pop_front();
        }
        p.n += 1;
        let protect_ended = p.protect > t;
        if protect_ended {
            p.protect = t;
        }
        let (dy, dp) = spread_of(p.n, w);
        let d = dir_of(yaw / 100.0 + dy, (pitch / 100.0 + dp).clamp(-89.0, 89.0));
        // lag compensation: where was everyone when the shooter pulled the trigger,
        // as the shooter saw them?
        let t_fire = t.min((t - FIRE_LATE).max(q as f64 / 100.0 + p.off.unwrap_or(t - q as f64 / 100.0)));
        let ip = as_num(msg.get("ip"), 0.0, 1e6).unwrap_or(0.0);
        let t_view = (t_fire - p.rtt.min(RTT_CAP) - (ip / 1000.0).min(IP_CAP)).max(t - HISTORY);
        let reach = ray_map(&MAP, o, d, wp.range);
        (Some((w, o, d, reach, protect_ended)), None, t_view)
    }

    /// A rocket's blast at r's position: falloff damage on every living player —
    /// team-mates and the shooter included (an honest rocket) — with a shot echo
    /// per victim so the shooter's screen floats the damage the server applied.
    fn detonate(&mut self, r: &Rocket, t: f64) {
        let at = [r.x, r.y, r.z];
        self.booms.push(json!([cm(at[0]), cm(at[1]), cm(at[2])]));
        let shooter = r.shooter.clone();
        // pass 1 (read-only): who is in range, and how hard it hits them
        let hits: Vec<(usize, i64)> = self.players.iter().enumerate()
            .filter(|(_, (_, v))| !v.dead && !v.gone)
            .map(|(j, (u, v))| {
                let dist = dist3([v.x, v.y + 0.9, v.z], at);
                // honest-but-fair, mirrors the client: the shooter's own blast grades by
                // proximity (75 falloff), never the 120 "direct" — a rocket launched from the
                // eye detonates ~0.6 m above the shooter's torso, which would otherwise be
                // an instant self-kill the moment anyone fires near a wall or their own feet.
                let direct = *u != shooter && dist < DIRECT_R;
                let dmg = if v.protect > t || v.shielded(t) { 0 } else { blast_damage(dist, direct) };
                (j, dmg)
            })
            .filter(|(_, dmg)| *dmg > 0)
            .collect();
        // pass 2 (mutable): apply, mark, credit kills, queue the echoes
        for (j, dmg) in hits {
            let (vid, hp0, vslot) = {
                let (vid, v) = &mut self.players[j];
                let hp0 = v.hp;
                v.hp = (v.hp - dmg).max(0);
                v.hp_original = hp0;
                (vid.clone(), hp0, v.slot)
            };
            let killed = self.players[j].1.hp <= 0;
            self.mark(&vid);
            if killed {
                let si = self.players.iter().position(|(u, _)| *u == shooter);
                if let Some(si) = si {
                    self.kill(si, j, NW - 1, false, t);
                } else {
                    // the shooter left: credit no one, but the death still counts
                    let (vid2, v) = &mut self.players[j];
                    let _ = vid2;
                    v.dead = true;
                    v.deaths += 1;
                    v.respawn_at = t + RESPAWN;
                    v.reload = None;
                    let deaths = v.deaths;
                    let team = v.team;
                    self.pending.push(("kill", json!({"k": null, "v": vid, "w": NW - 1, "hs": 0,
                        "kills": 0, "deaths": deaths, "team": team, "scores": self.scores})));
                }
            }
            let applied = hp0.saturating_sub(self.players[j].1.hp);
            self.detonate_echoes.push((shooter.clone(), at, vslot, applied));
        }
    }

    /// Flush detonate()'s echoes as shot rows once its borrows are done.
    /// [shooter_slot, launcher, x, y, z, victim_slot, head, dmg_applied]
    fn flush_echoes(&mut self) {
        let echoes = std::mem::take(&mut self.detonate_echoes);
        for (shooter, at, vslot, applied) in echoes {
            if let Some((_, p)) = self.players.iter().find(|(u, _)| *u == shooter) {
                self.shots.push(json!([p.slot, NW - 1, cm(at[0]), cm(at[1]), cm(at[2]),
                                       vslot, 0, applied]));
            }
        }
    }

    fn kill(&mut self, i: usize, j: usize, w: usize, head: bool, t: f64) {
        let (vid, deaths) = {
            let (vid, v) = &mut self.players[j];
            v.dead = true;
            v.deaths += 1;
            v.respawn_at = t + RESPAWN;
            v.reload = None;
            (vid.clone(), v.deaths)
        };
        let (uid, p) = &mut self.players[i];
        p.kills += 1;
        // the mole's twist: a charmed killer's point lands on the OTHER side (the charmer's)
        let charmed = p.charm_until > t;
        let team = if charmed { 1 - p.team } else { p.team };
        self.scores[team as usize] += 1;
        let scores = self.scores;
        let ev = json!({"k": uid, "v": vid, "w": w, "hs": if head { 1 } else { 0 }, "kills": p.kills,
                        "deaths": deaths, "team": team, "scores": scores});
        self.pending.push(("kill", ev));
    }

    pub fn reload(&mut self, uid: &str, msg: &Value, t: f64) -> bool {
        let phase = self.phase;
        let Some(p) = self.player_mut(uid) else { return false };
        if p.is_bot() || phase != Phase::Round || p.dead || p.gone
            || !bucket(&mut p.ob, OP_RATE, OP_BURST, t) || !p.this_life(msg) {
            return false;
        }
        let Some(q) = Self::clock(p, msg, t) else { return false };
        p.finish_reload(q);
        let w = p.w;
        if p.reload.is_some() || p.mag[w] >= WEAPONS[w].mag || p.res[w] <= 0 {
            return false;
        }
        p.reload = Some((w, q, q + (WEAPONS[w].reload * 100.0).round() as i64));
        true
    }

    pub fn weapon(&mut self, uid: &str, msg: &Value, t: f64) -> bool {
        let running = self.running();
        let Some(p) = self.player_mut(uid) else { return false };
        if p.is_bot() || !running || p.gone || !bucket(&mut p.ob, OP_RATE, OP_BURST, t) {
            return false;
        }
        let Some(w) = as_num(msg.get("w"), -1.0, 99.0) else { return false };
        if w != w.trunc() || !(0.0..WEAPONS.len() as f64).contains(&w) {
            return false;
        }
        let q = if msg.get("q").map(|v| !v.is_null()).unwrap_or(false) { Self::clock(p, msg, t) } else { None };
        if w as usize == p.w {
            return false;
        }
        p.w = w as usize;
        p.reload = None;
        p.ready = q.or(p.qmax).unwrap_or(0) + (SWITCH * 100.0) as i64;
        self.mark(uid);
        true
    }


    // -- bots --
    /// Think and move for one bot this tick. Movement runs through the same
    /// step() physics the clients run; shooting runs through its own
    /// adjudication (no rewind: the bot is on the server).
    fn bot_step(&mut self, i: usize, dt: f64, t: f64) {
        if self.players[i].1.dead {
            return;
        }
        // the Paralyser: a frozen bot holds still and holds its fire
        if self.players[i].1.freeze_until > t {
            return;
        }
        let team = self.players[i].1.team;
        let (x, y, z) = (self.players[i].1.x, self.players[i].1.y, self.players[i].1.z);
        // -- decide a goal (at most every BOT_THINK) --
        let think = self.players[i].1.bot.as_ref().map(|b| t >= b.think_at).unwrap_or(false);
        if think {
            // the mole: a charmed bot sees its own side as the enemy and its old enemy as home
            let charmed = self.players[i].1.charm_until > t;
            // the closest visible enemy, resolved to its uid without holding a borrow
            let mut closest: Option<String> = None;
            let mut best = f64::INFINITY;
            for (u, v) in self.players.iter() {
                let mut foe = v.team != team;
                if charmed {
                    foe = !foe;
                }
                if !foe || v.dead || v.gone || v.shielded(t) {
                    continue;   // a shielded glider isn't worth a bullet until the canopy's packed
                }
                if v.cloak_until > t {
                    continue;   // the cloak: a cloaked player is invisible to bots
                }
                let d = (v.x - x).hypot(v.z - z);
                if d < best && d <= BOT_RANGE && los(&MAP, [x, y + EYE, z], [v.x, v.y + EYE, v.z]) {
                    best = d;
                    closest = Some(u.clone());
                }
            }
            // the decoy: a standing ghost is just as tempting as the player it copies — the
            // nearest visible one wins the target only when no body is nearer (same rule as
            // the client's pool, weight aside)
            let mut decoy_target: Option<usize> = None;
            for (dj, dc) in self.decoys.iter().enumerate() {
                if dc.until <= t || dc.owner == self.players[i].0 {
                    continue;
                }
                let d = (dc.x - x).hypot(dc.z - z);
                if d < best && d <= BOT_RANGE && los(&MAP, [x, y + EYE, z], [dc.x, 1.0 + EYE, dc.z]) {
                    best = d;
                    decoy_target = Some(dj);
                    closest = None;
                }
            }
            // where the chosen enemy stands (resolved before the mutable borrow on the bot)
            let target_pos = closest.as_ref()
                .and_then(|tu| self.players.iter().find(|(u, _)| u == tu).map(|(_, p)| (p.x, p.z)));
            let b = self.players[i].1.bot.as_mut().unwrap();
            b.think_at = t + BOT_THINK;
            b.decoy = decoy_target;
            match closest {
                Some(ref tu) => {
                    b.target = Some(tu.clone());
                    if b.seen_at.is_none() {
                        b.seen_at = Some(t);
                    }
                }
                None => {
                    b.target = None;
                    b.seen_at = None;
                }
            }
            if decoy_target.is_some() && b.seen_at.is_none() {
                b.seen_at = Some(t);
            }
            if let Some(dj) = decoy_target {
                b.goal = if (self.decoys[dj].x - x).hypot(self.decoys[dj].z - z) > 18.0 {
                    Some((self.decoys[dj].x, self.decoys[dj].z))
                } else {
                    None
                };
            }
            if b.target.is_none() {
                let (wx, wz) = WAYPOINTS[b.wpi % WAYPOINTS.len()];
                if (x - wx).hypot(z - wz) < 1.5 {
                    b.wpi += 1;
                }
                b.goal = Some((wx, wz));
            } else {
                // fight from where we stand: close a little on a far target
                b.goal = match target_pos {
                    Some((vx, vz)) if (vx - x).hypot(vz - z) > 18.0 => Some((vx, vz)),
                    _ => None,
                };
            }
        }
        // -- turn and aim --
        let bait = self.players[i].1.bot.as_ref().and_then(|b| b.decoy)
            .filter(|dj| self.decoys.get(*dj).map(|dc| dc.until > t).unwrap_or(false));
        let ti = self.players[i].1.bot.as_ref().and_then(|b| b.target.as_ref())
            .and_then(|tu| self.players.iter().position(|(u, _)| u == tu));
        let (mut aim_y, mut aim_p) = self.players[i].1.bot.as_ref()
            .map(|b| (b.aim_y, b.aim_p)).unwrap_or((0.0, 0.0));
        let mut target_alive = false;
        if let Some(dj) = bait {
            // aim at the ghost: a still target at chest height, same turn/error rules
            let dc = &self.decoys[dj];
            let tx = dc.x;
            let tz = dc.z;
            let ty = 1.0;
            let mut want_yaw = yaw_of(tx - x, tz - z);
            let flat = (tx - x).hypot(tz - z).max(0.1);
            let mut want_pitch = ((ty - (y + EYE)) / flat).atan().to_degrees();
            let err_due = t - self.players[i].1.bot.as_ref().map(|b| b.err_at).unwrap_or(0.0) >= BOT_THINK;
            if err_due {
                let seed = (t * 1000.0) as i64 as u32 & 0xFFFF;
                let ey = (mb32(seed) * 2.0 - 1.0) * (BOT_AIM * self.bot_aim).to_degrees();
                let ep = (mb32(seed.wrapping_add(7919) & 0xFFFF) * 2.0 - 1.0) * (BOT_AIM * self.bot_aim).to_degrees();
                let b = self.players[i].1.bot.as_mut().unwrap();
                b.err_at = t;
                b.err_y = ey;
                b.err_p = ep;
            }
            let b = self.players[i].1.bot.as_ref().unwrap();
            want_yaw += b.err_y;
            want_pitch = want_pitch.clamp(-89.0, 89.0) + b.err_p;
            let step_deg = (BOT_TURN * self.bot_turn).to_degrees() * dt;
            let dy = (want_yaw - aim_y + 540.0).rem_euclid(360.0) - 180.0;
            aim_y += dy.clamp(-step_deg, step_deg);
            let dp = want_pitch - aim_p;
            aim_p += dp.clamp(-step_deg, step_deg);
            let b = self.players[i].1.bot.as_mut().unwrap();
            b.aim_y = aim_y;
            b.aim_p = aim_p;
            target_alive = true;
        }
        if let Some(ti) = ti {
            let v = &self.players[ti].1;
            if !v.dead && !v.gone && bait.is_none() {
                target_alive = true;
                // lead a little by their speed
                let lead = (v.x - x).hypot(v.z - z) / WEAPONS[self.players[i].1.w].range;
                let lead = 0.25f64.min(lead);
                let tx = v.x + v.vx * lead;
                let tz = v.z + v.vz * lead;
                // alternate head and chest shots (strike.py:902 -- the next shot's index)
                let ty = v.y + if (self.players[i].1.n + 1) % 3 == 0 { 1.35 } else { 1.0 };
                let mut want_yaw = yaw_of(tx - x, tz - z);
                let flat = (tx - x).hypot(tz - z).max(0.1);
                let mut want_pitch = ((ty - (y + EYE)) / flat).atan().to_degrees();
                // a fresh error every BOT_THINK (they are not aimbots)
                let err_due = t - self.players[i].1.bot.as_ref().map(|b| b.err_at).unwrap_or(0.0) >= BOT_THINK;
                if err_due {
                    let seed = (t * 1000.0) as i64 as u32 & 0xFFFF; // strike.py:909's & 0xFFFF
                    let ey = (mb32(seed) * 2.0 - 1.0) * (BOT_AIM * self.bot_aim).to_degrees();
                    let ep = (mb32(seed.wrapping_add(7919) & 0xFFFF) * 2.0 - 1.0) * (BOT_AIM * self.bot_aim).to_degrees();
                    let b = self.players[i].1.bot.as_mut().unwrap();
                    b.err_at = t;
                    b.err_y = ey;
                    b.err_p = ep;
                }
                let b = self.players[i].1.bot.as_ref().unwrap();
                want_yaw += b.err_y;
                want_pitch = want_pitch.clamp(-89.0, 89.0) + b.err_p;
                // swing the aim at BOT_TURN rad/s
                let step_deg = (BOT_TURN * self.bot_turn).to_degrees() * dt;
                let dy = (want_yaw - aim_y + 540.0).rem_euclid(360.0) - 180.0;
                aim_y += dy.clamp(-step_deg, step_deg);
                let dp = want_pitch - aim_p;
                aim_p += dp.clamp(-step_deg, step_deg);
                let b = self.players[i].1.bot.as_mut().unwrap();
                b.aim_y = aim_y;
                b.aim_p = aim_p;
            }
        }
        if !target_alive {
            // walk the patrol: face the way we are going
            let goal = self.players[i].1.bot.as_ref().and_then(|b| b.goal);
            if let Some((gx, gz)) = goal {
                let want_yaw = yaw_of(gx - x, gz - z);
                let step_deg = (BOT_TURN * self.bot_turn).to_degrees() * dt;
                let dy = (want_yaw - aim_y + 540.0).rem_euclid(360.0) - 180.0;
                aim_y += dy.clamp(-step_deg, step_deg);
                let b = self.players[i].1.bot.as_mut().unwrap();
                b.aim_y = aim_y;
            }
        }
        // -- move (same physics as the clients) --
        let goal = self.players[i].1.bot.as_ref().and_then(|b| b.goal);
        let (mut wx, mut wz) = (0.0, 0.0);
        if let Some((gx, gz)) = goal {
            let (dx, dz) = (gx - x, gz - z);
            let ln = dx.hypot(dz);
            if ln > 0.3 {
                wx = dx / ln;
                wz = dz / ln;
            }
        }
        {
            let p = &mut self.players[i].1;
            let b = p.bot.as_mut().unwrap();
            b.body.x = p.x;
            b.body.y = p.y;
            b.body.z = p.z;
            // v4: a parachute-dropping bot doesn't run physics — it floats the same canopy
            // descent a human client gets, then hands control back to step() on landing
            let dropping = p.drop && (t - p.drop_t) < DROP_TIME;
            if dropping {
                let floor = top_under(&MAP, b.body.x, b.body.y, b.body.z).unwrap_or(0.0);
                b.body.y = (b.body.y - DROP_SINK * dt).max(floor);
                if b.body.y <= floor {
                    p.drop = false;
                    p.drop_t = 0.0;
                }
            } else {
                if p.drop {
                    p.drop = false;
                    p.drop_t = 0.0;
                }
                step(&MAP, &mut b.body, wx, wz, false, dt);
            }
            p.x = b.body.x;
            p.y = b.body.y;
            p.z = b.body.z;
            p.vx = b.body.vx;
            p.vz = b.body.vz;
            p.r = ((aim_y * 100.0) as i64).rem_euclid(36000);
            p.pt = (aim_p * 100.0) as i64;
            p.at = t;
            p.hist.push_back((t, p.x, p.y, p.z));
            while p.hist.len() > 2 && t - p.hist[0].0 > HISTORY {
                p.hist.pop_front();
            }
        }
        // -- shoot (its own adjudication: no clock, no rewind) --
        if target_alive {
            let b = self.players[i].1.bot.as_ref().unwrap();
            if b.seen_at.map(|s| t - s >= BOT_REACT * self.bot_react).unwrap_or(false) {
                // baited: the ghost stands where it stands, no need to resolve a target
                let d = match bait {
                    Some(dj) => (self.decoys[dj].x - x).hypot(self.decoys[dj].z - z),
                    None => {
                        let tgt = b.target.as_ref().unwrap();
                        let v = self.players.iter().find(|(u, _)| u == tgt).map(|(_, p)| p).unwrap();
                        (v.x - x).hypot(v.z - z)
                    }
                };
                if d <= BOT_RANGE {
                    self.bot_fire(i, t);
                }
            }
        }
        // -- reload --
        {
            let p = &mut self.players[i].1;
            let w = p.w;
            if let Some((rw, _, end)) = p.reload {
                if (t * 100.0) as i64 >= end {
                    let take = (WEAPONS[rw].mag - p.mag[rw]).min(p.res[rw]);
                    p.mag[rw] += take;
                    p.res[rw] -= take;
                    p.reload = None;
                }
            }
            if p.mag[w] <= 0 && p.reload.is_none() && p.res[w] > 0 {
                p.reload = Some((w, (t * 100.0) as i64,
                                 ((t + WEAPONS[w].reload) * 100.0).round() as i64));
            }
        }
        let uid = self.players[i].0.clone();
        self.mark(&uid);
    }

    /// A bot's shot: the same hit rules as a human's fire op, but with no
    /// sender clock and no lag compensation (the bot is on the server, so what
    /// it sees is what we see). Returns the judged shot, if any.
    fn bot_fire(&mut self, i: usize, t: f64) -> Option<Value> {
        if self.phase != Phase::Round {
            return None;
        }
        let w = self.players[i].1.w;
        let b = self.players[i].1.bot.as_ref()?;
        // bot_rate is the host-picked MP skill stretching (or tightening) a bot's own gap
        // between shots beyond the weapon's interval — plain carry-over from the DIFFS table
        if t < b.fire_at + WEAPONS[w].interval * self.bot_rate - 0.01 {
            return None;
        }
        let (aim_y, aim_p) = (b.aim_y, b.aim_p);
        // a bot with the launcher fires rockets like a player would, one at a time
        if w == NW - 1 {
            if self.rockets.iter().any(|r| r.shooter == self.players[i].0) {
                return None;
            }
            let uid = self.players[i].0.clone();
            let (slot, ok) = {
                let p = &mut self.players[i].1;
                if p.mag[w] <= 0 || p.reload.is_some() {
                    (p.slot, false)
                } else {
                    (p.slot, true)
                }
            };
            if !ok {
                return None;
            }
            let o = {
                let p = &mut self.players[i].1;
                let o = [p.x, p.y + EYE, p.z];
                p.n += 1;
                if p.protect > t {
                    p.protect = t;
                }
                o
            };
            let d = dir_of(aim_y, aim_p.clamp(-89.0, 89.0));
            self.rocket_k += 1;
            self.rockets.push(Rocket {
                id: self.rocket_k, shooter: uid,
                x: o[0], y: o[1], z: o[2], px: o[0], py: o[1], pz: o[2],
                vx: d[0] * MISSILE_V, vy: d[1] * MISSILE_V, vz: d[2] * MISSILE_V, born: t,
            });
            let shot = json!([slot, w, cm(o[0]), cm(o[1]), cm(o[2]), -1, 0]);
            self.shots.push(shot.clone());
            self.players[i].1.mag[w] -= 1;
            self.players[i].1.bot.as_mut().unwrap().fire_at = t;
            return Some(shot);
        }
        let p = &mut self.players[i].1;
        if p.mag[w] <= 0 || p.reload.is_some() {
            return None;
        }
        let o = [p.x, p.y + EYE, p.z];
        p.n += 1;
        let (dy, dp) = spread_of(p.n, w);
        let d = dir_of(aim_y + dy, (aim_p + dp).clamp(-89.0, 89.0));
        if p.protect > t {
            p.protect = t;
        }
        let team = p.team;
        let reach = ray_map(&MAP, o, d, WEAPONS[w].range);
        // the decoy: a ghost standing in the line of fire swallows the shot (bots only fall
        // for it while it stands — the client marks the same dismiss-on-hit)
        let mut decoy_hit: Option<usize> = None;
        let mut decoy_d = reach;
        for (dj, dc) in self.decoys.iter().enumerate() {
            if dc.until <= t {
                continue;
            }
            if let Some(th) = ray_player(o, d, dc.x, dc.y, dc.z, decoy_d).map(|h| h.0) {
                decoy_hit = Some(dj);
                decoy_d = th;
            }
        }
        let mut best: Option<(usize, f64, bool)> = None;
        let charmed = self.players[i].1.charm_until > t;
        for (j, (vid, v)) in self.players.iter().enumerate() {
            if vid == &self.players[i].0 || v.dead || v.gone {
                continue;
            }
            let mut foe = v.team != team;
            if charmed {
                foe = !foe;
            }
            if !foe || v.cloak_until > t {
                continue;
            }
            if let Some((th, head)) = ray_player(o, d, v.x, v.y, v.z, reach) {
                if best.map(|bb| th < bb.1).unwrap_or(true) {
                    best = Some((j, th, head));
                }
            }
        }
        // the ghost wins only when it stands nearer than any body it masks
        if let Some(dj) = decoy_hit {
            let nearer = best.map(|b| b.1 > decoy_d).unwrap_or(true);
            if nearer {
                self.decoys[dj].until = t;   // one hit retires the ghost
                let shooter_slot = self.players[i].1.slot;
                let shot = vec![json!(shooter_slot), json!(w),
                                json!(cm(o[0] + d[0] * decoy_d)), json!(cm(o[1] + d[1] * decoy_d)),
                                json!(cm(o[2] + d[2] * decoy_d)), json!(-1), json!(0)];
                let shot = Value::Array(shot);
                self.shots.push(shot.clone());
                self.players[i].1.bot.as_mut().unwrap().fire_at = t;
                self.players[i].1.mag[w] -= 1;
                return Some(shot);
            }
        }
        let end = best.map(|b| b.1).unwrap_or(reach);
        let shooter_slot = self.players[i].1.slot;
        let mut shot = vec![json!(shooter_slot), json!(w), json!(cm(o[0] + d[0] * end)),
                            json!(cm(o[1] + d[1] * end)), json!(cm(o[2] + d[2] * end)), json!(-1), json!(0)];
        if let Some((j, _, head)) = best {
            let (vid, v) = &mut self.players[j];
            let vid = vid.clone();
            shot[5] = json!(v.slot);
            shot[6] = json!(if head { 1 } else { 0 });
            let dmg = if v.protect > t || v.shielded(t) { 0 } else if head { WEAPONS[w].head } else { WEAPONS[w].damage };
            v.hp = (v.hp - dmg).max(0);
            let killed = dmg > 0 && v.hp <= 0;
            self.mark(&vid);
            if killed {
                self.kill(i, j, w, head, t);
            }
        }
        let shot = Value::Array(shot);
        self.shots.push(shot.clone());
        self.players[i].1.bot.as_mut().unwrap().fire_at = t;
        self.players[i].1.mag[w] -= 1;
        Some(shot)
    }

    // -- the clock --
    /// The spawn point of this player's side farthest from every living opponent.
    pub fn spawn_for(&self, uid: &str) -> usize {
        let team = self.player(uid).map(|p| p.team).unwrap_or(0);
        let foes: Vec<[f64; 3]> = self.players.iter()
            .filter(|(_, v)| v.team != team && !v.dead && !v.gone).map(|(_, v)| [v.x, v.y, v.z]).collect();
        let spawns: Vec<usize> = side_spawns(team).collect();
        let start = self.player(uid).map(|p| p.slot as usize * 5).unwrap_or(0);
        let (mut best, mut best_d) = (spawns[0], -1.0f64);
        for j in 0..spawns.len() {
            let i = spawns[(start + j) % spawns.len()];
            let s = MAP.spawns[i];
            let dmin = foes.iter().map(|f| dist3([s[0], s[1], s[2]], *f)).fold(1e9, f64::min);
            if dmin > best_d + 1e-9 {
                best = i;
                best_d = dmin;
            }
        }
        best
    }

    /// Advance the match clock; returns the events for the lobby, in order.
    pub fn tick(&mut self, t: f64, send: bool) -> Vec<(&'static str, Value)> {
        let mut evs = Vec::new();
        self.k += 1;
        let dt = 1.0 / HZ;
        if self.phase == Phase::Warmup && t >= self.go_at {
            self.phase = Phase::Round;
            self.ends_at = self.go_at + (self.minutes * 60) as f64;
            // the round's own drop clocks: the first totem lands TOTEM_FIRST in, the
            // first loose pack PACK_FIRST in — two independent clocks
            self.totem_at = Some(t + TOTEM_FIRST);
            self.pack_at = Some(t + PACK_FIRST);
            for (_, p) in self.players.iter_mut() {
                p.at = t;
                p.air_at = t;
                p.protect = t + PROTECT;
                p.pb = (POS_BURST, t);
                p.fb = (FIRE_BURST, t);
                p.ob = (OP_BURST, t);
            }
            self.dirty = self.players.iter().map(|(u, _)| u.clone()).collect();
            evs.push(("go", json!({"msLeft": self.ms_left(t)})));
        }
        if self.phase != Phase::Round {
            return evs;
        }
        for i in 0..self.players.len() {
            let uid = self.players[i].0.clone();
            let is_bot = self.players[i].1.is_bot();
            let p = &self.players[i].1;
            if p.gone {
                continue;
            }
            if p.away.map(|a| t - a >= GRACE).unwrap_or(false) {
                self.players[i].1.gone = true;
                self.mark(&uid);
                evs.push(("gone", json!({"user": uid})));
                continue;
            }
            if is_bot {
                if !p.dead {
                    self.bot_step(i, dt, t);
                }
                continue;
            }
            if p.dead && t >= p.respawn_at {
                let life = self.players[i].1.life + 1;
                let slot = self.players[i].1.slot;
                let s = drop_spawn(slot, life);  // v4: parachute drop at a random spawn point
                let p = &mut self.players[i].1;
                p.place(s, t);
                let ev = json!({"user": uid, "x": cm(p.x), "y": cm(p.y), "z": cm(p.z), "r": p.r, "e": p.life, "drop": true});
                self.mark(&uid);
                evs.push(("spawn", ev));
            } else if p.protect != 0.0 && t >= p.protect {
                self.players[i].1.protect = 0.0;
                self.mark(&uid);
            }
        }
        // bots respawn too
        for i in 0..self.players.len() {
            if self.players[i].1.is_bot() && self.players[i].1.dead && !self.players[i].1.gone
                && t >= self.players[i].1.respawn_at {
                let uid = self.players[i].0.clone();
                let life = self.players[i].1.life + 1;
                let slot = self.players[i].1.slot;
                let s = drop_spawn(slot, life);
                self.players[i].1.place(s, t);  // bots glide too, same canopy physics as a human
                self.mark(&uid);
            }
        }
        // the launcher's rockets: fly, hit, blast. One at a time per segment, the same
        // segment test the browser's solo missilesStep runs, and the same falloff.
        for ri in (0..self.rockets.len()).rev() {
            let mut r = self.rockets[ri].clone();
            r.vy -= MISSILE_G * dt;
            let (nx, ny, nz) = (r.px + r.vx * dt, r.py + r.vy * dt, r.pz + r.vz * dt);
            let d3 = [nx - r.px, ny - r.py, nz - r.pz];
            let ln = d3[0].hypot(d3[1]).hypot(d3[2]);
            let mut hit = [nx, ny, nz];
            let mut stopped = false;
            if ln > 1e-9 {
                let u = [d3[0] / ln, d3[1] / ln, d3[2] / ln];
                let reach = ray_map(&MAP, [r.px, r.py, r.pz], u, ln);
                if reach < ln {
                    stopped = true;
                    hit = [r.px + u[0] * reach, r.py + u[1] * reach, r.pz + u[2] * reach];
                } else {
                    // an enemy body blocks it (the shooter's own side is skipped, exactly
                    // like the browser's solo flight: it leaves your own head's box)
                    let steam = self.players.iter().find(|(u, _)| *u == r.shooter).map(|(_, p)| p.team);
                    for (_, v) in self.players.iter() {
                        if v.dead || v.gone || Some(v.team) == steam {
                            continue;
                        }
                        let (body, head) = hit_boxes(v.x, v.y, v.z);
                        if ray_box([r.px, r.py, r.pz], u, &body).map(|h| h <= ln).unwrap_or(false)
                            || ray_box([r.px, r.py, r.pz], u, &head).map(|h| h <= ln).unwrap_or(false) {
                            stopped = true;
                            hit = [r.px + u[0] * ln, r.py + u[1] * ln, r.pz + u[2] * ln];
                            break;
                        }
                    }
                }
            }
            if !stopped && hit[1] <= 0.05 {
                stopped = true;
                hit[1] = 0.05;
            }
            r.x = hit[0];
            r.y = hit[1];
            r.z = hit[2];
            r.px = r.x;
            r.py = r.y;
            r.pz = r.z;
            self.rockets[ri] = r.clone();
            if stopped || t - r.born > MISSILE_TTL {
                self.rockets.remove(ri);
                self.detonate(&r, t);
            }
        }
        self.flush_echoes();
        // the two drop clocks, independent of each other: a fresh random totem every
        // TOTEM_GAP (the mole only when a side fields a bot — it can't turn a human) and
        // a fresh random health/ammo pack every PACK_GAP. Each replaces the oldest slot
        // of its ring, so drops keep coming whether or not anyone took the last one.
        if self.totem_at.map(|at| t >= at).unwrap_or(false) {
            self.totem_at = Some(t + TOTEM_GAP);
            let can_mole = self.players.iter().any(|(_, v)| v.is_bot() && !v.dead && !v.gone);
            self.spawn_totem(can_mole);
        }
        if self.pack_at.map(|at| t >= at).unwrap_or(false) {
            self.pack_at = Some(t + PACK_GAP);
            self.spawn_pack();
        }
        // decoys expire silently (the client's render pass drops the mesh when its window ends)
        self.decoys.retain(|dc| dc.until > t);
        for i in 0..self.items.len() {
            if self.items[i].map(|b| t >= b).unwrap_or(false) {
                self.items[i] = None;
                evs.push(("item", json!({"i": i, "on": 1})));
            }
        }
        evs.append(&mut self.pending);
        // same fill as drop(): a blips-past-GRACE human is replaced too
        self.fill_bots(t);
        // over at the score limit, at the time, or when a side is empty of active players
        let empty0 = !self.players.iter().any(|(_, p)| p.team == 0 && !p.gone
            && p.away.map(|a| t - a < GRACE).unwrap_or(true));
        let empty1 = !self.players.iter().any(|(_, p)| p.team == 1 && !p.gone
            && p.away.map(|a| t - a < GRACE).unwrap_or(true));
        if self.scores[0] >= self.limit || self.scores[1] >= self.limit || t >= self.ends_at || empty0 || empty1 {
            self.phase = Phase::Done;
            let mut results = Vec::new();
            let (mut prev, mut place) = (None, 0usize);
            for (i, u) in self.order().iter().enumerate() {
                let p = self.player(u).expect("ordered player");
                let key = (p.team, p.kills, p.deaths);
                if Some(key) != prev {
                    place = i + 1;
                    prev = Some(key);
                }
                results.push(json!({"user": p.user, "place": place, "team": p.team,
                                    "bot": if p.is_bot() { 1 } else { 0 },
                                    "kills": p.kills, "deaths": p.deaths, "left": p.gone}));
            }
            results.push(json!({"teams": self.scores}));
            self.results = Some(Value::Array(results));
            if !self.shots.is_empty() || !self.booms.is_empty() || !self.rockets.is_empty() {
                evs.push(("snap", self.snap(t)));
            }
            evs.push(("done", json!({"results": self.results})));
            return evs;
        }
        if send && (!self.dirty.is_empty() || !self.shots.is_empty() || !self.booms.is_empty()
                    || !self.rockets.is_empty()) {
            evs.push(("snap", self.snap(t)));
        }
        evs
    }

    /// One snapshot: each changed player as [slot, x, y, z, yaw, pitch, hp,
    /// weapon, flags, when] (when: the server time of that position on the
    /// match clock; flags bit 16 = a bot), the shots judged since the last
    /// one, and ts = when it left.
    fn snap(&mut self, t: f64) -> Value {
        let mut ents = Vec::new();
        for uid in &self.dirty {
            let Some(p) = self.player(uid) else { continue };
            let f = (p.dead as i64) | if p.protect > t { 2 } else { 0 } | if p.away.is_some() { 4 } else { 0 }
                | if p.gone { 8 } else { 0 } | if p.is_bot() { 16 } else { 0 }
                | if p.shielded(t) { 32 } else { 0 }   // the drop shield or the shield totem's bubble
                | if p.charm_until > t { 64 } else { 0 }   // the mole's gold mark
                | if p.freeze_until > t { 128 } else { 0 }   // the paralyser's violet mark
                | if p.wings_fly > t { 256 } else { 0 }   // the wings totem's cruise window (rise+hover — bit off while gliding down)
                | if p.leap_until > t { 512 } else { 0 }   // the leap totem's jump window
                | if p.speed_until > t { 1024 } else { 0 };   // the speed totem's dash window
            let when = p.hist.back().map(|h| self.ms(h.0)).unwrap_or(-1);
            ents.push(json!([p.slot, cm(p.x), cm(p.y), cm(p.z), p.r, p.pt, p.hp, p.w, f, when]));
        }
        let rockets: Vec<Value> = self.rockets.iter()
            .map(|r| json!([r.id, cm(r.x), cm(r.y), cm(r.z), (r.vx * 100.0) as i64,
                            (r.vy * 100.0) as i64, (r.vz * 100.0) as i64])).collect();
        // decoys: the full list while any lives (id, x, y, z, ms left, yaw in centidegrees)
        let decoys: Vec<Value> = self.decoys.iter()
            .map(|dc| json!([dc.id, cm(dc.x), cm(dc.y), cm(dc.z),
                             ((dc.until - t) * 1000.0) as i64, (dc.yaw * 100.0) as i64])).collect();
        let snap = json!({"k": self.k, "ts": self.ms(t), "ms": self.ms_left(t), "sc": self.scores,
                          "p": ents, "s": self.shots, "r": rockets, "b": self.booms, "d": decoys});
        self.dirty.clear();
        self.shots.clear();
        self.booms.clear();
        self.sent.push_back((self.k, t));
        while self.sent.front().map(|s| t - s.1 > RTT_WINDOW + 1.0).unwrap_or(false) {
            self.sent.pop_front();
        }
        snap
    }
}

// --------------------------------------------------------------------- hub --
/// One room's strike state: the lobby and the match.
#[derive(Debug, Default)]
pub struct StrikeRoom {
    pub lobby: Lobby,
    pub strike: Strike,
}

fn lobby_put(l: &mut Lobby, uid: &str, pubv: Value) {
    match l.members.iter_mut().find(|(u, _)| u == uid) {
        Some(slot) => slot.1 = pubv,
        None => l.members.push((uid.to_string(), pubv)),
    }
}

fn lobby_pop(l: &mut Lobby, uid: &str) -> Option<Value> {
    let i = l.members.iter().position(|(u, _)| u == uid)?;
    Some(l.members.remove(i).1)
}

/// The arena game for every room: lobby, referee and the per-room tick loop.
#[derive(Clone)]
pub struct StrikeHub {
    inner: Arc<HubInner>,
}

struct HubInner {
    rooms: RoomManager,
    reg: Arc<Registry>,
    clock: Clock,
    /// Where a finished game is written down. Empty in the unit tests.
    results: Recorder,
    state: Mutex<HashMap<String, StrikeRoom>>,
}

fn tick_key(room_id: &str) -> String {
    format!("strike:{room_id}")
}

impl StrikeHub {
    pub fn new(
        rooms: RoomManager, reg: Arc<Registry>, clock: Clock, results: Recorder,
    ) -> Self {
        Self {
            inner: Arc::new(HubInner {
                rooms, reg, clock, results, state: Mutex::new(HashMap::new()),
            }),
        }
    }

    pub fn now(&self) -> f64 {
        (self.inner.clock)()
    }

    pub fn registry(&self) -> &Arc<Registry> {
        &self.inner.reg
    }

    pub fn with_room<R>(&self, room_id: &str, f: impl FnOnce(&mut StrikeRoom) -> R) -> Option<R> {
        self.inner.state.lock().unwrap().get_mut(room_id).map(f)
    }

    /// A `{"type": "game", "g": "strike", ...}` message from one socket.
    pub async fn handle(&self, room_id: &str, conn: u64, member: &Member, msg: &Value) {
        let Some(op) = msg.get("op").and_then(Value::as_str) else {
            let mut out = Out::new(GAME);
            out.err(conn, "unknown game");
            self.flush(room_id, out).await;
            return;
        };
        let mut out = Out::new(GAME);
        let mut invite: Option<String> = None;
        {
            let t = self.now();
            let uid = member.user_id.as_str();
            let mut st = self.inner.state.lock().unwrap();
            let v = st.entry(room_id.to_string()).or_default();
            match op {
                "join" => {
                    let lobby = &mut v.lobby;
                    if !lobby.has(uid) && lobby.members.len() >= MAX_LOBBY {
                        out.err(conn, "this game's lobby is full");
                    } else {
                        let fresh = !lobby.has(uid);
                        let blip = lobby.blips.remove(uid);
                        let returning = blip.map(|b| t - b < HOST_GRACE).unwrap_or(false);
                        lobby.blips.retain(|_, b| t - *b < HOST_GRACE);
                        lobby_put(lobby, uid, member.public());
                        let ph = lobby.prev_host.clone();
                        let host_gone = lobby.host.as_deref().map(|h| !lobby.has(h)).unwrap_or(true);
                        let host_back = ph.as_ref().map(|(h, at)| h == uid && t - at < HOST_GRACE).unwrap_or(false);
                        if host_gone || host_back {
                            lobby.host = Some(uid.to_string());
                        }
                        if ph.map(|(h, at)| h == uid || t - at >= HOST_GRACE).unwrap_or(false) {
                            lobby.prev_host = None;
                        }
                        let joined = if fresh && !returning { member.public() } else { Value::Null };
                        out.push(To::All, "lobby",
                                 json!({"members": lobby.roster(), "joined": joined, "name": GAME_NAME}));
                        if v.strike.enter(uid, member.public(), t) {
                            out.push(To::Users(v.lobby.ids(), None), "strike",
                                     json!({"match": v.strike.view(t), "back": uid}));
                        } else {
                            out.push(To::Conn(conn), "strike", json!({"match": v.strike.view(t)}));
                        }
                    }
                }
                "leave" => leave_lobby(v, uid, &mut out, false, t),
                "invite" => {
                    let to = msg.get("to").and_then(Value::as_str).unwrap_or("");
                    if !v.lobby.has(uid) {
                        out.err(conn, "join the lobby first");
                    } else if to.is_empty() || to == uid {
                        out.err(conn, "invite someone else");
                    } else {
                        invite = Some(to.to_string());
                    }
                }
                _ if !v.lobby.has(uid) => out.err(conn, "join the lobby first"),
                _ => self.strike_op(room_id, v, conn, member, op, msg, &mut out, t),
            }
        }
        if let Some(to) = invite {
            let payload = json!({"type": "game", "g": GAME, "ev": "invite", "pv": crate::protocol::version(GAME),
                                 "from": member.public(), "room": room_id, "name": GAME_NAME});
            let delivered = self.inner.rooms.deliver_to_user(&to, payload.to_string()).await;
            out.push(To::Conn(conn), "invited", json!({"to": to, "delivered": delivered}));
        }
        self.flush(room_id, out).await;
    }

    #[allow(clippy::too_many_arguments)]
    fn strike_op(&self, room_id: &str, v: &mut StrikeRoom, conn: u64, member: &Member, op: &str, msg: &Value,
                 out: &mut Out, t: f64) {
        let uid = member.user_id.as_str();
        let ids = v.lobby.ids();
        let sm = &mut v.strike;
        match op {
            "pos" => {
                if let (_, Some(fix)) = sm.pos(uid, msg, t) {
                    out.push(To::Conn(conn), "fix", fix);
                }
            }
            "fire" => {
                if let (_, Some(sync)) = sm.fire(uid, msg, t) {
                    out.push(To::Conn(conn), "ammo", sync);
                }
            }
            "reload" => {
                sm.reload(uid, msg, t);
            }
            "weapon" => {
                sm.weapon(uid, msg, t);
            }
            "view" => out.push(To::Conn(conn), "strike", json!({"match": sm.view(t)})),
            "char" => match sm.char(uid, msg) {
                None => out.err(conn, "pick a character"),
                Some(c) => out.push(To::Users(ids, None), "char", json!({"user": uid, "c": c})),
            },
            "start" | "end" => {
                if v.lobby.host.as_deref() != Some(uid) {
                    out.err(conn, "only the host can do that");
                    return;
                }
                v.lobby.prev_host = None;
                let sm = &mut v.strike;
                if op == "start" {
                    let null = Value::Null;
                    let mut err = sm.start(&v.lobby.members, msg.get("minutes").unwrap_or(&null),
                                           msg.get("kills").unwrap_or(&null),
                                           msg.get("tsize").unwrap_or(&null),
                                           msg.get("diff").unwrap_or(&null), t);
                    if err.is_none() && !self.tick_on(room_id) {
                        sm.end();
                        err = Some("the Arena is busy right now: try again in a minute".into());
                    }
                    match err {
                        Some(e) => out.err(conn, &e),
                        None => out.push(To::Users(ids, None), "strike",
                                         json!({"match": sm.view(t), "by": member.public()})),
                    }
                } else {
                    sm.end();
                    self.inner.reg.stop(&tick_key(room_id));
                    out.push(To::Users(ids, None), "strike", json!({"match": sm.view(t)}));
                }
            }
            _ => {}
        }
    }

    fn tick_on(&self, room_id: &str) -> bool {
        let hub = self.clone();
        let rid = room_id.to_string();
        let clock = self.inner.clock.clone();
        let step = move |t: f64, send: bool, tk: Arc<Ticker>| {
            let hub = hub.clone();
            let rid = rid.clone();
            async move { hub.step(&rid, t, send, &tk).await }
        };
        self.inner.reg.start(&tick_key(room_id), HZ, step, clock).is_some()
    }

    async fn step(&self, room_id: &str, t: f64, send: bool, tk: &Ticker) -> bool {
        if !self.inner.rooms.exists(room_id).await {
            return false;
        }
        let (out, ids, running) = {
            let mut st = self.inner.state.lock().unwrap();
            let Some(v) = st.get_mut(room_id) else { return false };
            if !v.strike.running() {
                return false;
            }
            let mut ids = v.lobby.ids();
            for (u, _) in &v.strike.players {
                if !v.lobby.has(u) {
                    ids.push(u.clone());
                }
            }
            let mut out = Out::new(GAME);
            for (ev, data) in v.strike.tick(t, send) {
                out.push(To::Users(ids.clone(), None), ev, data);
            }
            let running = v.strike.running();
            if !running {
                out.push(To::Users(ids.clone(), None), "strike", json!({"match": v.strike.view(t)}));
            }
            (out, ids, running)
        };
        if !out.items.is_empty() {
            let n = self.inner.rooms.count_where(room_id, |m| ids.contains(&m.user_id)).await;
            let bytes: usize = out.items.iter().map(|(_, p)| p.to_string().len()).sum();
            tk.count(bytes * n.max(1));
            self.flush(room_id, out).await;
        }
        running
    }

    /// A socket went away: if that was the user's last socket in the room, leave
    /// the lobby as a blip. Call before `RoomManager::leave`.
    pub async fn on_disconnect(&self, room_id: &str, conn: u64, member: &Member) {
        if self.with_room(room_id, |_| ()).is_none() {
            return;
        }
        let uid = member.user_id.clone();
        if self.inner.rooms.other_conn(room_id, conn, |m| m.user_id == uid).await {
            return;
        }
        let mut out = Out::new(GAME);
        let t = self.now();
        self.with_room(room_id, |v| leave_lobby(v, &uid, &mut out, true, t));
        self.flush(room_id, out).await;
        if !self.inner.rooms.other_conn(room_id, conn, |_| true).await {
            self.inner.state.lock().unwrap().remove(room_id);
            self.inner.reg.stop(&tick_key(room_id));
        }
    }

    async fn flush(&self, room_id: &str, out: Out) {
        let rooms = &self.inner.rooms;
        for (to, payload) in out.items {
            // A finished game is written down from the event the server itself
            // sends the lobby or the room -- once per game, never from a
            // client. The Python records at the same point, in valley._flush.
            if matches!(to, To::All | To::Users(..)) {
                if let Some(g) = crate::results::is_done(&payload) {
                    self.inner.results.record_later(g, &payload);
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
                    rooms.send_where(room_id, |m| ids.contains(&m.user_id)
                                     && skip.as_deref() != Some(m.user_id.as_str()), s).await;
                }
            }
        }
    }
}

fn leave_lobby(v: &mut StrikeRoom, uid: &str, out: &mut Out, disconnected: bool, t: f64) {
    let lobby = &mut v.lobby;
    let Some(who) = lobby_pop(lobby, uid) else { return };
    if disconnected {
        lobby.blips.insert(uid.to_string(), t);
    }
    if lobby.host.as_deref() == Some(uid) {
        lobby.host = lobby.members.first().map(|(u, _)| u.clone());
        lobby.prev_host = if disconnected && lobby.host.is_some() { Some((uid.to_string(), t)) } else { None };
    }
    if v.strike.drop(uid, t, disconnected) {
        out.push(To::Users(v.lobby.ids(), None), "strike", json!({"match": v.strike.view(t), "left": uid}));
    }
    out.push(To::All, "lobby", json!({"members": v.lobby.roster(), "left": who, "name": GAME_NAME}));
}

#[cfg(test)]
mod tests;
