//! Blaster Arena for the Valley (g = "fps"): a first-person free-for-all for up
//! to 8, and its referee. A port of `backend/app/fps.py` plus the fps branch of
//! `backend/app/valley.py`, on the same tick and budgets as Kart Racing
//! (`realtime.rs`), with its own hub so each game keeps its own lobby.
//!
//! Each player runs and aims in the browser (games/fps.js) and streams where
//! they are; the server runs a 20 Hz tick that checks every move, judges every
//! shot, keeps the score and sends the lobby one batched snapshot per tick.
//!
//! Lag compensation: a "fire" carries the shooter's clock (q), the snapshot
//! tick it had last seen (k, and when it arrived, kq) and the interpolation
//! delay it was drawing with (ip). The server maps q onto its own clock (the
//! smallest arrival offset seen from that sender), takes off the shooter's
//! round trip (measured here from the snapshot acknowledgements, never
//! claimed) and the interpolation delay (capped at [`IP_CAP`]), and rewinds
//! every target to that moment in the position history it keeps for each
//! player; the ray is tested against the rewound hit boxes and the arena's
//! solid boxes (no hits through walls).
//!
//! The arena is `backend/app/fps_map.json`, embedded at build time with
//! `include_str!` -- the file the Python reads (a byte copy of
//! games/fps/map.json). HQ 2.5: a host may instead start a match on a map made
//! in the Map Editor; it arrives inline on the start op, is checked by
//! [`validate_custom`], and the room holds it as `Fps::map` (an `Arc<Map>`)
//! until a built-in start or the room empties. Its id is the content key
//! (`c-` + 12 hex), which the done event carries as `map` so results.rs can
//! keep a board per user map.
//!
//! Units on the wire: x, y, z in centimetres, yaw and pitch in hundredths of a
//! degree, the sender's clock in centiseconds (q).
//!
//! Protocol: in `{"type":"game","g":"fps","op":...}`, out
//! `{"type":"game","g":"fps","ev":...}`. Ops: join, leave, invite, view, char,
//! start, end, pos, fire, reload, weapon. Events: lobby, fps, char, go, snap,
//! fix, ammo, kill, spawn, pick, item, gone, done, invite, invited, error.

// target_at and a few constants are the shared geometry the tests drive (and
// the browser's practice range uses); the server itself does not need them.
#![cfg_attr(not(test), allow(dead_code))]

use crate::kart::{Lobby, Out, To};
use crate::realtime::{Clock, Registry, Ticker};
use crate::results::Recorder;
use crate::rooms::{Member, RoomManager};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::{Arc, LazyLock, Mutex};

// --- movement (shared with games/fps.js) ---
pub const R: f64 = 0.35;
pub const H: f64 = 1.8;
pub const EYE: f64 = 1.6;
pub const STEP_H: f64 = 0.55;
pub const RUN: f64 = 6.0;
pub const ACCEL: f64 = 12.0;
pub const AIR_ACCEL: f64 = 3.0;
pub const GRAVITY: f64 = 18.0;
pub const JUMP_V: f64 = 6.5;
pub const FALL_MAX: f64 = 25.0;

// --- weapons (shared with games/fps.js) ---
pub struct Weapon {
    #[allow(dead_code)] // kept in step with games/fps.js weapon ids
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
pub const WEAPONS: [Weapon; 2] = [
    Weapon { id: "rapid", interval: 0.1, damage: 12, head: 18, mag: 30, reserve: 90, reload: 1.6, spread: 1.2,
             range: 60.0, pack: 60 },
    Weapon { id: "heavy", interval: 0.8, damage: 55, head: 85, mag: 6, reserve: 18, reload: 2.0, spread: 0.3,
             range: 80.0, pack: 12 },
];
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
pub const MINUTES: [i64; 3] = [3, 5, 10];
pub const KILLS: [i64; 3] = [10, 20, 30];
pub const MAX_PLAYERS: usize = 8;
pub const CHARS: i64 = 6;

// Lobby (valley.py)
pub const GAME: &str = "fps";
pub const GAME_NAME: &str = "Blaster Arena";
pub const MAX_LOBBY: usize = 8;
pub const HOST_GRACE: f64 = 15.0;

pub const MAP_JSON: &str = include_str!("../data/fps_map.json");

pub type Box3 = [f64; 6];

#[derive(Debug)]
pub struct Pickup {
    #[allow(dead_code)] // mirrors the map JSON; read by tests and the client
    pub id: String,
    pub health: bool,
    pub at: [f64; 3],
}

#[derive(Debug)]
pub struct Map {
    pub id: String,
    pub bounds: Box3,
    pub boxes: Vec<Box3>,
    pub spawns: Vec<[f64; 4]>,
    pub pickups: Vec<Pickup>,
    #[allow(dead_code)] // practice drones: the client animates them from target_at
    pub targets: Vec<([f64; 3], [f64; 3], f64)>,
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
        bounds: nums::<6>(&d["bounds"]),
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

/// The built-in arena (the courtyard). Shared: every room that is not playing a
/// user-made map holds a clone of this Arc in [`Fps::map`].
pub static MAP: LazyLock<Arc<Map>> = LazyLock::new(|| {
    Arc::new(compile_map(&serde_json::from_str(MAP_JSON).expect("fps_map.json parses")).expect("fps_map.json compiles"))
});


// ------------------------------------------------------ user-made maps --
// HQ 2.5: a host can start a match on a map made in the Map Editor
// (games/mapedit.js). It arrives inline on the start op as a MapDoc
// {"kind":"fps","v":1,"name":..,"data":{bounds, theme, boxes, spawns, pickups}},
// is checked here without a single panic on hostile input, and is held by the
// room as an `Arc<Map>` for exactly as long as the room plays it (never leaked).

/// The box kinds the client knows how to draw (games/fps.js KIND).
pub const BOX_KINDS: [&str; 6] = ["floor", "wall", "low", "block", "step", "crate"];
pub const MAX_BOXES: usize = 96;
pub const MIN_SPAWNS: usize = 8;
pub const MAX_SPAWNS: usize = 16;
pub const MAX_PICKUPS: usize = 16;
/// Every coordinate of a user map lies within +-COORD_MAX metres.
pub const COORD_MAX: f64 = 200.0;
/// The canonical data, serialised, may be at most this many bytes.
pub const MAX_DATA_BYTES: usize = 12 * 1024;
pub const NAME_MAX: usize = 32;

/// A user-made map, validated: the compiled geometry (its id is the content
/// key), the display name and the canonical data that view events carry.
#[derive(Debug, Clone)]
pub struct Custom {
    pub map: Arc<Map>,
    pub name: String,
    pub data: Value,
}

fn r2(n: f64) -> f64 {
    let r = (n * 100.0).round() / 100.0;
    if r == 0.0 { 0.0 } else { r } // no "-0.0" on the wire
}

/// A canonical number: an integer when it is whole, else the 0.01-rounded float.
fn num_json(n: f64) -> Value {
    if n.fract() == 0.0 && n.abs() < 1e9 { json!(n as i64) } else { json!(n) }
}

/// One finite JSON number (not a bool, not a string) within +-COORD_MAX, rounded to 0.01.
fn coord(v: Option<&Value>, what: &str) -> Result<f64, String> {
    let f = v.and_then(Value::as_number).and_then(|n| n.as_f64())
        .ok_or_else(|| format!("{what} must be a number"))?;
    if !f.is_finite() || f.abs() > COORD_MAX {
        return Err(format!("{what} is out of range"));
    }
    Ok(r2(f))
}

fn coords<const N: usize>(v: &Value, what: &str) -> Result<[f64; N], String> {
    let a = v.as_array().filter(|a| a.len() == N).ok_or_else(|| format!("{what} needs {N} numbers"))?;
    let mut out = [0.0; N];
    for (i, o) in out.iter_mut().enumerate() {
        *o = coord(a.get(i), what)?;
    }
    Ok(out)
}

fn hex_colour(v: Option<&Value>, what: &str) -> Result<String, String> {
    let s = v.and_then(Value::as_str).ok_or_else(|| format!("theme {what} must be a colour"))?;
    let ok = s.len() == 7 && s.starts_with('#') && s[1..].chars().all(|c| c.is_ascii_hexdigit());
    if !ok {
        return Err(format!("theme {what} must be #rrggbb"));
    }
    Ok(s.to_ascii_lowercase())
}

fn inside(b: &Box3, x: f64, y: f64, z: f64) -> bool {
    b[0] <= x && x <= b[3] && b[1] <= y && y <= b[4] && b[2] <= z && z <= b[5]
}

/// Validate a user map's `data` and return it CANONICAL (keys in order, unknown
/// keys dropped, numbers rounded to 0.01, yaw to a whole degree in 0..360, hex
/// lowercased) together with its compiled geometry. Never panics.
fn compile_custom(data: &Value) -> Result<(Map, Value), String> {
    let d = data.as_object().ok_or("the map data must be an object")?;
    let bounds = coords::<6>(d.get("bounds").unwrap_or(&Value::Null), "bounds")?;
    if !(bounds[0] < bounds[3] && bounds[1] < bounds[4] && bounds[2] < bounds[5]) {
        return Err("the bounds have no volume".into());
    }
    let th = d.get("theme").and_then(Value::as_object).ok_or("theme must be an object")?;
    let (sky, fog, ground) = (hex_colour(th.get("sky"), "sky")?, hex_colour(th.get("fog"), "fog")?,
                              hex_colour(th.get("ground"), "ground")?);

    let raw_boxes = d.get("boxes").and_then(Value::as_array).ok_or("boxes must be a list")?;
    if raw_boxes.is_empty() {
        return Err("a map needs at least one box".into());
    }
    if raw_boxes.len() > MAX_BOXES {
        return Err(format!("too many boxes (at most {MAX_BOXES})"));
    }
    let mut boxes: Vec<Box3> = Vec::with_capacity(raw_boxes.len());
    let mut box_json = Vec::with_capacity(raw_boxes.len());
    for (i, rb) in raw_boxes.iter().enumerate() {
        let what = format!("box {}", i + 1);
        let a = rb.as_array().filter(|a| a.len() == 7).ok_or_else(|| format!("{what} needs 6 numbers and a kind"))?;
        let mut b = [0.0; 6];
        for (k, o) in b.iter_mut().enumerate() {
            *o = coord(a.get(k), &what)?;
        }
        let kind = a.get(6).and_then(Value::as_str).filter(|k| BOX_KINDS.contains(k))
            .ok_or_else(|| format!("{what} has an unknown kind"))?;
        if !(b[0] < b[3] && b[1] < b[4] && b[2] < b[5]) {
            return Err(format!("{what} has no volume"));
        }
        if !(inside(&bounds, b[0], b[1], b[2]) && inside(&bounds, b[3], b[4], b[5])) {
            return Err(format!("{what} is outside the bounds"));
        }
        boxes.push(b);
        let mut row: Vec<Value> = b.iter().map(|n| num_json(*n)).collect();
        row.push(json!(kind));
        box_json.push(Value::Array(row));
    }
    let mut map = Map { id: String::new(), bounds, boxes, spawns: Vec::new(), pickups: Vec::new(), targets: Vec::new() };

    let raw_spawns = d.get("spawns").and_then(Value::as_array).ok_or("spawns must be a list")?;
    let n = raw_spawns.len();
    if !(MIN_SPAWNS..=MAX_SPAWNS).contains(&n) || n % 5 == 0 {
        return Err(format!("a map needs {MIN_SPAWNS}-{MAX_SPAWNS} spawns, and not 10 or 15"));
    }
    let mut spawn_json = Vec::with_capacity(n);
    for (i, rs) in raw_spawns.iter().enumerate() {
        let what = format!("spawn {}", i + 1);
        let a = rs.as_array().filter(|a| a.len() == 4).ok_or_else(|| format!("{what} needs x, y, z and a yaw"))?;
        let (x, y, z) = (coord(a.first(), &what)?, coord(a.get(1), &what)?, coord(a.get(2), &what)?);
        let yaw = a.get(3).and_then(Value::as_number).and_then(|v| v.as_f64()).filter(|f| f.is_finite() && f.abs() <= 3600.0)
            .ok_or_else(|| format!("{what} needs a yaw in degrees"))?;
        let yaw = (yaw.round() as i64).rem_euclid(360) as f64;
        if !inside(&bounds, x, y, z) || y + H > bounds[4] {
            return Err(format!("{what} is outside the bounds"));
        }
        if overlaps(&map, x, y, z, 0.0).is_some() {
            return Err(format!("{what} is inside a wall"));
        }
        if top_under(&map, x, y - 0.05, z).map(|t| (t - y).abs() > 1e-6).unwrap_or(true) {
            return Err(format!("{what} is not standing on anything"));
        }
        map.spawns.push([x, y, z, yaw]);
        spawn_json.push(json!([num_json(x), num_json(y), num_json(z), num_json(yaw)]));
    }

    let raw_picks = match d.get("pickups") {
        None | Some(Value::Null) => Vec::new(),
        Some(v) => v.as_array().ok_or("pickups must be a list")?.clone(),
    };
    if raw_picks.len() > MAX_PICKUPS {
        return Err(format!("too many pickups (at most {MAX_PICKUPS})"));
    }
    let mut pick_json = Vec::with_capacity(raw_picks.len());
    for (i, rp) in raw_picks.iter().enumerate() {
        let what = format!("pickup {}", i + 1);
        let o = rp.as_object().ok_or_else(|| format!("{what} must be an object"))?;
        let id = o.get("id").and_then(Value::as_str)
            .filter(|s| s.strip_prefix('p').and_then(|n| n.parse::<usize>().ok())
                .map(|k| (1..=MAX_PICKUPS).contains(&k) && *s == format!("p{k}")).unwrap_or(false))
            .ok_or_else(|| format!("{what} needs an id p1..p16"))?;
        if map.pickups.iter().any(|p| p.id == id) {
            return Err(format!("{what} repeats the id {id}"));
        }
        let kind = o.get("kind").and_then(Value::as_str).filter(|k| *k == "health" || *k == "ammo")
            .ok_or_else(|| format!("{what} must be health or ammo"))?;
        let at = coords::<3>(o.get("at").unwrap_or(&Value::Null), &what)?;
        if !inside(&bounds, at[0], at[1], at[2]) {
            return Err(format!("{what} is outside the bounds"));
        }
        if overlaps(&map, at[0], at[1], at[2], 0.0).is_some() {
            return Err(format!("{what} is inside a wall"));
        }
        map.pickups.push(Pickup { id: id.to_string(), health: kind == "health", at });
        pick_json.push(json!({"id": id, "kind": kind, "at": [num_json(at[0]), num_json(at[1]), num_json(at[2])]}));
    }

    let canon = json!({
        "bounds": bounds.iter().map(|n| num_json(*n)).collect::<Vec<_>>(),
        "theme": {"sky": sky, "fog": fog, "ground": ground},
        "boxes": box_json,
        "spawns": spawn_json,
        "pickups": pick_json,
    });
    if canon.to_string().len() > MAX_DATA_BYTES {
        return Err("the map is too big".into());
    }
    Ok((map, canon))
}

/// The canonical form of a user map's data (global contract section 7), or why
/// it is refused. The map gallery stores exactly this.
#[allow(dead_code)] // also the gallery's validator (maps.rs)
pub fn validate_custom(data: &Value) -> Result<Value, String> {
    compile_custom(data).map(|(_, canon)| canon)
}

/// A MapDoc's name: 1-32 printable characters, trimmed, no angle brackets, no links.
pub fn clean_name(v: Option<&Value>) -> Result<String, String> {
    let s = v.and_then(Value::as_str).ok_or("the map needs a name")?;
    let t = s.trim();
    let n = t.chars().count();
    if n == 0 || n > NAME_MAX {
        return Err(format!("the name must be 1-{NAME_MAX} characters"));
    }
    if t.chars().any(|c| !crate::rooms::py_printable(c) || c == '<' || c == '>') || t.to_ascii_lowercase().contains("http") {
        return Err("the name has characters a map name can't use".into());
    }
    Ok(t.to_string())
}

/// A whole MapDoc from a start op: {"kind":"fps","v":1,"name","data"}.
pub fn custom_from_doc(doc: &Value) -> Result<Custom, String> {
    let o = doc.as_object().ok_or("no map was sent")?;
    if o.get("kind").and_then(Value::as_str) != Some("fps") {
        return Err("that is not a Blaster map".into());
    }
    if o.get("v").and_then(Value::as_i64) != Some(1) {
        return Err("this map was made by a newer editor".into());
    }
    let name = clean_name(o.get("name"))?;
    let (mut map, data) = compile_custom(o.get("data").unwrap_or(&Value::Null))?;
    map.id = crate::mapkey::content_key("fps", &data);
    Ok(Custom { map: Arc::new(map), name, data })
}

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

/// One step of the runner (fps.move): axis by axis against the solid boxes, a
/// low ledge stepped onto while on the ground.
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

/// mulberry32, one draw.
pub fn mb32(a: u32) -> f64 {
    let a = a.wrapping_add(0x6D2B_79F5);
    let mut t = (a ^ (a >> 15)).wrapping_mul(1 | a);
    t = (t.wrapping_add((t ^ (t >> 7)).wrapping_mul(61 | t))) ^ t;
    (t ^ (t >> 14)) as f64 / 4294967296.0
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

#[derive(Clone, Debug)]
pub struct Player {
    pub user: Value,
    pub slot: i64,
    pub ch: i64,
    pub x: f64,
    pub y: f64,
    pub z: f64,
    pub r: i64,
    pub pt: i64,
    pub hp: i64,
    pub w: usize,
    pub mag: [i64; 2],
    pub res: [i64; 2],
    pub last: [VecDeque<i64>; 2],
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
    fn new(user: Value, slot: i64, ch: i64, t: f64) -> Self {
        Self {
            user, slot, ch, x: 0.0, y: 0.0, z: 0.0, r: 0, pt: 0, hp: HP, w: 0,
            mag: [WEAPONS[0].mag, WEAPONS[1].mag], res: [WEAPONS[0].reserve, WEAPONS[1].reserve],
            last: [VecDeque::new(), VecDeque::new()], ready: -(1 << 30), reload: None, n: 0,
            kills: 0, deaths: 0, dead: false, respawn_at: 0.0, life: 0, spawn_t: t, protect: 0.0,
            away: None, gone: false, q: None, q0: 0, t0: t, off: None, qmax: None, at: t, air_at: t,
            pb: (POS_BURST, t), fb: (FIRE_BURST, t), ob: (OP_BURST, t), fix_at: -1e9, sync_at: -1e9, bad: 0,
            hist: VecDeque::new(), rtts: VecDeque::new(), rtt: 0.0,
        }
    }

    fn place(&mut self, s: [f64; 4], t: f64) {
        self.x = s[0];
        self.y = s[1];
        self.z = s[2];
        self.r = ((s[3] * 100.0) as i64).rem_euclid(36000);
        self.pt = 0;
        self.hp = HP;
        self.dead = false;
        self.mag = [WEAPONS[0].mag, WEAPONS[1].mag];
        self.res = [WEAPONS[0].reserve, WEAPONS[1].reserve];
        self.reload = None;
        self.spawn_t = t;
        self.protect = t + PROTECT;
        self.air_at = t;
        self.n = 0; // the spread sequence restarts each life
        self.life += 1;
        self.hist = VecDeque::from([(t, s[0], s[1], s[2])]);
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

/// One room's match. idle -> warmup (countdown) -> round -> done.
#[derive(Debug)]
pub struct Fps {
    pub phase: Phase,
    pub minutes: i64,
    pub limit: i64,
    pub go_at: f64,
    pub ends_at: f64,
    pub players: Vec<(String, Player)>,
    pub chars: BTreeMap<String, i64>,
    pub results: Option<Value>,
    pub items: Vec<Option<f64>>,
    pub dirty: Vec<String>,
    pub shots: Vec<Value>,
    pub pending: Vec<(&'static str, Value)>,
    pub k: i64,
    pub sent: VecDeque<(i64, f64)>,
    /// The arena this room plays: the built-in courtyard, or a user map.
    pub map: Arc<Map>,
    /// (name, canonical data) while a user map is on; None on the built-in one.
    pub custom: Option<(String, Value)>,
}

impl Default for Fps {
    fn default() -> Self {
        Self::new()
    }
}

impl Fps {
    pub fn new() -> Self {
        Self {
            phase: Phase::Idle, minutes: 5, limit: 20, go_at: 0.0, ends_at: 0.0, players: Vec::new(),
            chars: BTreeMap::new(), results: None, items: Vec::new(), dirty: Vec::new(), shots: Vec::new(),
            pending: Vec::new(), k: 0, sent: VecDeque::new(), map: MAP.clone(), custom: None,
        }
    }

    /// Pick the arena for the next match: a user map, or None for the built-in
    /// one. Refused (false) while a match is on, so a running match never
    /// changes ground under anyone.
    pub fn set_map(&mut self, custom: Option<Custom>) -> bool {
        if self.running() {
            return false;
        }
        match custom {
            Some(c) => {
                self.map = c.map;
                self.custom = Some((c.name, c.data));
            }
            None => {
                self.map = MAP.clone();
                self.custom = None;
            }
        }
        true
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
            "user": p.user, "slot": p.slot, "char": p.ch, "kills": p.kills, "deaths": p.deaths, "hp": p.hp,
            "dead": p.dead, "away": p.away.is_some(), "gone": p.gone, "w": p.w, "x": cm(p.x), "y": cm(p.y),
            "z": cm(p.z), "r": p.r, "e": p.life})).collect();
        let mut v = json!({"map": self.map.id, "phase": self.phase.as_str(), "minutes": self.minutes, "limit": self.limit,
               "goInMs": if self.phase == Phase::Warmup { (((self.go_at - t) * 1000.0) as i64).max(0) } else { 0 },
               "msLeft": self.ms_left(t), "players": players,
               "items": self.items.iter().map(|a| if a.is_none() { 1 } else { 0 }).collect::<Vec<_>>(),
               "results": self.results, "chars": self.chars});
        // A user map travels in the view (and so in the start event) only --
        // never in a snapshot -- so a late joiner or a rejoin can build it.
        if let Some((name, data)) = &self.custom {
            v["custom"] = json!({"name": name, "data": data});
        }
        v
    }

    pub fn order(&self) -> Vec<String> {
        let mut v: Vec<&(String, Player)> = self.players.iter().collect();
        v.sort_by_key(|(_, p)| (-p.kills, p.deaths, p.slot));
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

    pub fn start(&mut self, members: &[(String, Value)], minutes: &Value, kills: &Value, t: f64) -> Option<String> {
        if self.running() {
            return Some("a match is already on: the host can end it first".into());
        }
        let pick = |v: &Value, opts: &[i64], dflt: i64| {
            as_num(Some(v), 0.0, 1000.0).map(|f| f as i64).filter(|f| opts.contains(f)).unwrap_or(dflt)
        };
        self.minutes = pick(minutes, &MINUTES, 5);
        self.limit = pick(kills, &KILLS, 20);
        self.phase = Phase::Warmup;
        self.go_at = t + COUNTDOWN;
        self.results = None;
        self.players = Vec::new();
        self.items = vec![None; self.map.pickups.len()];
        self.shots.clear();
        self.pending.clear();
        self.sent.clear();
        let n = self.map.spawns.len();
        for (k, (uid, pubv)) in members.iter().take(MAX_PLAYERS).enumerate() {
            let ch = self.chars.get(uid).copied().unwrap_or(k as i64 % CHARS);
            let mut p = Player::new(pubv.clone(), k as i64, ch, t);
            p.place(self.map.spawns[(k * 5) % n], t);
            self.players.push((uid.clone(), p));
        }
        self.dirty = self.players.iter().map(|(u, _)| u.clone()).collect();
        None
    }

    pub fn end(&mut self) {
        self.phase = Phase::Idle;
        self.players.clear();
        self.results = None;
        self.shots.clear();
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
            return true;
        }
        if self.active() >= MAX_PLAYERS {
            return false;
        }
        let slot = 1 + self.players.iter().map(|(_, p)| p.slot).max().unwrap_or(-1);
        let ch = self.chars.get(uid).copied().unwrap_or(slot % CHARS);
        let mut p = Player::new(pubv, slot, ch, t);
        if phase == Phase::Warmup {
            p.place(self.map.spawns[(slot as usize * 5) % self.map.spawns.len()], t);
        } else {
            p.dead = true;
            p.respawn_at = t;
        }
        self.players.push((uid.to_string(), p));
        self.mark(uid);
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
        if self.phase != Phase::Round {
            return (false, None);
        }
        let mut p = std::mem::replace(&mut self.players[i].1, Player::new(Value::Null, 0, 0, t));
        let res = self.pos_inner(&mut p, uid, msg, t);
        self.players[i].1 = p;
        if res.0 {
            self.mark(uid);
            self.pickups(i, t);
        }
        res
    }

    fn pos_inner(&self, p: &mut Player, _uid: &str, msg: &Value, t: f64) -> (bool, Option<Value>) {
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
        let (xm, ym, zm) = (x / 100.0, y / 100.0, z / 100.0);
        let b = self.map.bounds;
        let mut ok = dt >= 0.0 && b[0] <= xm && xm <= b[3] && b[1] <= ym && ym <= b[4] && b[2] <= zm && zm <= b[5];
        if ok {
            let across = (xm - p.x).hypot(zm - p.z);
            let dy = ym - p.y;
            ok = across <= MAX_SPEED * dt + SLACK && dy <= JUMP_V * dt + STEP_H + SLACK
                && -dy <= FALL_MAX * dt + SLACK && overlaps(&self.map, xm, ym, zm, SHRINK).is_none();
        }
        let clock = match (q, p.off) {
            (Some(q), Some(off)) => q as f64 / 100.0 + off,
            _ => t,
        };
        let mut grounded = false;
        if ok {
            grounded = overlaps(&self.map, xm, ym - 0.1, zm, 0.0).is_some();
            if !grounded && clock - p.air_at > MAX_AIR {
                ok = false;
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
        let map = self.map.clone();
        for (j, pk) in map.pickups.iter().enumerate() {
            if self.items[j].is_some() {
                continue;
            }
            let (uid, p) = &mut self.players[i];
            if (p.x - pk.at[0]).hypot(p.z - pk.at[2]) > PICK_R || (p.y - pk.at[1]).abs() > 1.2 {
                continue;
            }
            if pk.health {
                if p.hp >= HP {
                    continue;
                }
                p.hp = HP.min(p.hp + HEAL);
            } else {
                if (0..2).all(|w| p.res[w] >= WEAPONS[w].reserve * 2) {
                    continue;
                }
                for w in 0..2 {
                    p.res[w] = (WEAPONS[w].reserve * 2).min(p.res[w] + WEAPONS[w].pack);
                }
            }
            let ev = json!({"i": j, "user": uid, "hp": p.hp, "res": p.res});
            self.items[j] = Some(t + PICKUP_BACK);
            self.pending.push(("pick", ev));
        }
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
        let mut p = std::mem::replace(&mut self.players[i].1, Player::new(Value::Null, 0, 0, t));
        let res = self.fire_inner(&mut p, msg, t);
        self.players[i].1 = p;
        let Some((w, o, d, reach, protect_ended)) = res.0 else { return (None, res.1) };
        if protect_ended {
            self.mark(uid);
        }
        let t_view = res.2;
        let mut best: Option<(usize, f64, bool)> = None;
        for (j, (vid, v)) in self.players.iter().enumerate() {
            if vid == uid || v.dead || v.gone || v.spawn_t > t_view {
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
            let dmg = if v.protect > t { 0 } else if head { WEAPONS[w].head } else { WEAPONS[w].damage };
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
        if p.dead || p.gone || !bucket(&mut p.fb, FIRE_RATE, FIRE_BURST, t) {
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
        if dist3(o, [p.x, p.y + EYE, p.z]) > ORIGIN_SLACK + MAX_SPEED * lag.min(FIRE_LATE) {
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
        let reach = ray_map(&self.map, o, d, wp.range);
        (Some((w, o, d, reach, protect_ended)), None, t_view)
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
        let ev = json!({"k": uid, "v": vid, "w": w, "hs": if head { 1 } else { 0 }, "kills": p.kills,
                        "deaths": deaths});
        self.pending.push(("kill", ev));
    }

    pub fn reload(&mut self, uid: &str, msg: &Value, t: f64) -> bool {
        let phase = self.phase;
        let Some(p) = self.player_mut(uid) else { return false };
        if phase != Phase::Round || p.dead || p.gone || !bucket(&mut p.ob, OP_RATE, OP_BURST, t) || !p.this_life(msg) {
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
        if !running || p.gone || !bucket(&mut p.ob, OP_RATE, OP_BURST, t) {
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

    /// The spawn point farthest from every living opponent.
    pub fn spawn_for(&self, uid: &str) -> usize {
        let foes: Vec<[f64; 3]> = self.players.iter()
            .filter(|(u, v)| u != uid && !v.dead && !v.gone).map(|(_, v)| [v.x, v.y, v.z]).collect();
        let n = self.map.spawns.len();
        let start = self.player(uid).map(|p| p.slot as usize * 5).unwrap_or(0);
        let (mut best, mut best_d) = (0usize, -1.0f64);
        for j in 0..n {
            let i = (start + j) % n;
            let s = self.map.spawns[i];
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
        if self.phase == Phase::Warmup && t >= self.go_at {
            self.phase = Phase::Round;
            self.ends_at = self.go_at + (self.minutes * 60) as f64;
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
            if p.dead && t >= p.respawn_at {
                let s = self.map.spawns[self.spawn_for(&uid)];
                let p = &mut self.players[i].1;
                p.place(s, t);
                let ev = json!({"user": uid, "x": cm(p.x), "y": cm(p.y), "z": cm(p.z), "r": p.r, "e": p.life});
                self.mark(&uid);
                evs.push(("spawn", ev));
            } else if p.protect != 0.0 && t >= p.protect {
                self.players[i].1.protect = 0.0;
                self.mark(&uid);
            }
        }
        for i in 0..self.items.len() {
            if self.items[i].map(|b| t >= b).unwrap_or(false) {
                self.items[i] = None;
                evs.push(("item", json!({"i": i, "on": 1})));
            }
        }
        evs.append(&mut self.pending);
        let top = self.players.iter().map(|(_, p)| p.kills).max().unwrap_or(0);
        let alone = self.active() < if self.players.len() >= 2 { 2 } else { 1 };
        if top >= self.limit || t >= self.ends_at || alone {
            self.phase = Phase::Done;
            let mut results = Vec::new();
            let (mut prev, mut place) = (None, 0usize);
            for (i, u) in self.order().iter().enumerate() {
                let p = self.player(u).expect("ordered player");
                let key = (p.kills, p.deaths);
                if Some(key) != prev {
                    place = i + 1;
                    prev = Some(key);
                }
                results.push(json!({"user": p.user, "place": place, "kills": p.kills, "deaths": p.deaths,
                                    "left": p.gone}));
            }
            self.results = Some(Value::Array(results));
            if !self.shots.is_empty() {
                evs.push(("snap", self.snap(t)));
            }
            evs.push(("done", json!({"map": self.map.id, "results": self.results})));
            return evs;
        }
        if send && (!self.dirty.is_empty() || !self.shots.is_empty()) {
            evs.push(("snap", self.snap(t)));
        }
        evs
    }

    /// One snapshot: each changed player as [slot, x, y, z, yaw, pitch, hp,
    /// weapon, flags, when] (when: the server time of that position on the match
    /// clock), the shots judged since the last one, and ts = when it left.
    fn snap(&mut self, t: f64) -> Value {
        let mut ents = Vec::new();
        for uid in &self.dirty {
            let Some(p) = self.player(uid) else { continue };
            let f = (p.dead as i64) | if p.protect > t { 2 } else { 0 } | if p.away.is_some() { 4 } else { 0 }
                | if p.gone { 8 } else { 0 };
            let when = p.hist.back().map(|h| self.ms(h.0)).unwrap_or(-1);
            ents.push(json!([p.slot, cm(p.x), cm(p.y), cm(p.z), p.r, p.pt, p.hp, p.w, f, when]));
        }
        let snap = json!({"k": self.k, "ts": self.ms(t), "ms": self.ms_left(t), "p": ents, "s": self.shots});
        self.dirty.clear();
        self.shots.clear();
        self.sent.push_back((self.k, t));
        while self.sent.front().map(|s| t - s.1 > RTT_WINDOW + 1.0).unwrap_or(false) {
            self.sent.pop_front();
        }
        snap
    }
}

// --------------------------------------------------------------------- hub --
/// One room's fps state: the lobby and the match.
#[derive(Debug, Default)]
pub struct FpsRoom {
    pub lobby: Lobby,
    pub fps: Fps,
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
pub struct FpsHub {
    inner: Arc<HubInner>,
}

struct HubInner {
    rooms: RoomManager,
    reg: Arc<Registry>,
    clock: Clock,
    /// Where a finished game is written down. Empty in the unit tests.
    results: Recorder,
    state: Mutex<HashMap<String, FpsRoom>>,
}

fn tick_key(room_id: &str) -> String {
    format!("fps:{room_id}")
}

impl FpsHub {
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

    pub fn with_room<R>(&self, room_id: &str, f: impl FnOnce(&mut FpsRoom) -> R) -> Option<R> {
        self.inner.state.lock().unwrap().get_mut(room_id).map(f)
    }

    /// A `{"type": "game", "g": "fps", ...}` message from one socket.
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
                        if v.fps.enter(uid, member.public(), t) {
                            out.push(To::Users(v.lobby.ids(), None), "fps",
                                     json!({"match": v.fps.view(t), "back": uid}));
                        } else {
                            out.push(To::Conn(conn), "fps", json!({"match": v.fps.view(t)}));
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
                _ => self.fps_op(room_id, v, conn, member, op, msg, &mut out, t),
            }
        }
        if let Some(to) = invite {
            let payload = json!({"type": "game", "g": GAME, "ev": "invite", "pv": crate::protocol::version(GAME), "from": member.public(),
                                 "room": room_id, "name": GAME_NAME});
            let delivered = self.inner.rooms.deliver_to_user(&to, payload.to_string()).await;
            out.push(To::Conn(conn), "invited", json!({"to": to, "delivered": delivered}));
        }
        self.flush(room_id, out).await;
    }

    #[allow(clippy::too_many_arguments)]
    fn fps_op(&self, room_id: &str, v: &mut FpsRoom, conn: u64, member: &Member, op: &str, msg: &Value,
              out: &mut Out, t: f64) {
        let uid = member.user_id.as_str();
        let ids = v.lobby.ids();
        let fm = &mut v.fps;
        match op {
            "pos" => {
                if let (_, Some(fix)) = fm.pos(uid, msg, t) {
                    out.push(To::Conn(conn), "fix", fix);
                }
            }
            "fire" => {
                if let (_, Some(sync)) = fm.fire(uid, msg, t) {
                    out.push(To::Conn(conn), "ammo", sync);
                }
            }
            "reload" => {
                fm.reload(uid, msg, t);
            }
            "weapon" => {
                fm.weapon(uid, msg, t);
            }
            "view" => out.push(To::Conn(conn), "fps", json!({"match": fm.view(t)})),
            "char" => match fm.char(uid, msg) {
                None => out.err(conn, "pick a character"),
                Some(c) => out.push(To::Users(ids, None), "char", json!({"user": uid, "c": c})),
            },
            "start" | "end" => {
                if v.lobby.host.as_deref() != Some(uid) {
                    out.err(conn, "only the host can do that");
                    return;
                }
                v.lobby.prev_host = None;
                let fm = &mut v.fps;
                if op == "start" {
                    let null = Value::Null;
                    // HQ 2.5: map "custom" carries a MapDoc made in the Map Editor;
                    // anything else is the built-in courtyard.
                    let custom = if msg.get("map").and_then(Value::as_str) == Some("custom") {
                        match custom_from_doc(msg.get("custom").unwrap_or(&null)) {
                            Ok(c) => Some(c),
                            Err(e) => {
                                out.err(conn, &format!("bad map: {e}"));
                                return;
                            }
                        }
                    } else {
                        None
                    };
                    fm.set_map(custom);
                    let mut err = fm.start(&v.lobby.members, msg.get("minutes").unwrap_or(&null),
                                           msg.get("kills").unwrap_or(&null), t);
                    if err.is_none() && !self.tick_on(room_id) {
                        fm.end();
                        err = Some("the Arena is busy right now: try again in a minute".into());
                    }
                    match err {
                        Some(e) => out.err(conn, &e),
                        None => out.push(To::Users(ids, None), "fps",
                                         json!({"match": fm.view(t), "by": member.public()})),
                    }
                } else {
                    fm.end();
                    self.inner.reg.stop(&tick_key(room_id));
                    out.push(To::Users(ids, None), "fps", json!({"match": fm.view(t)}));
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
            if !v.fps.running() {
                return false;
            }
            let mut ids = v.lobby.ids();
            for (u, _) in &v.fps.players {
                if !v.lobby.has(u) {
                    ids.push(u.clone());
                }
            }
            let mut out = Out::new(GAME);
            for (ev, data) in v.fps.tick(t, send) {
                out.push(To::Users(ids.clone(), None), ev, data);
            }
            let running = v.fps.running();
            if !running {
                out.push(To::Users(ids.clone(), None), "fps", json!({"match": v.fps.view(t)}));
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

fn leave_lobby(v: &mut FpsRoom, uid: &str, out: &mut Out, disconnected: bool, t: f64) {
    let lobby = &mut v.lobby;
    let Some(who) = lobby_pop(lobby, uid) else { return };
    if disconnected {
        lobby.blips.insert(uid.to_string(), t);
    }
    if lobby.host.as_deref() == Some(uid) {
        lobby.host = lobby.members.first().map(|(u, _)| u.clone());
        lobby.prev_host = if disconnected && lobby.host.is_some() { Some((uid.to_string(), t)) } else { None };
    }
    if v.fps.drop(uid, t, disconnected) {
        out.push(To::Users(v.lobby.ids(), None), "fps", json!({"match": v.fps.view(t), "left": uid}));
    }
    out.push(To::All, "lobby", json!({"members": v.lobby.roster(), "left": who, "name": GAME_NAME}));
}

#[cfg(test)]
mod tests;
