//! Mini Golf for the Valley (g = "golf"): deterministic integer putting physics
//! and the round referee. A port of `backend/app/golf.py` in full plus the golf
//! branch of `backend/app/valley.py` (`golf_op` :1429-1479, `_golf_advance`
//! :1482-1492, the join tail :1049-1054 and the `_leave_lobby` arm :1562-1566).
//!
//! AUDIENCE: every golf event goes to THIS GAME'S LOBBY (`out.lobby`), or to the
//! one asking socket (`view`, every error, and a join that did not restore a
//! parked player). Never [`super::To::All`] -- unlike pond, race, duel, mines
//! and farm, a socket in the room that never joined golf sees no golf traffic at
//! all, which `test_round_over_websockets` depends on (test_golf.py:318-325) and
//! `a_golf_event_goes_only_to_the_lobby` pins here.
//!
//! The physics is integer-only so the server and every browser compute exactly
//! the same roll: the server simulates each shot from {ax, az, power} and
//! broadcasts the result, and the clients replay the same simulation
//! (games/golf.js, between the GOLF-SIM markers) only to animate it. Keep the
//! three in lock-step. `backend/tests/golf_golden.json` pins 350 shots across
//! all five courses and `the_golden_vectors_roll_exactly_as_python_does` drives
//! that same file, so a divergence of one unit in one substep fails here.
//!
//! Units: 1 tile = 10000. Velocity is units/tick x [`VS`], [`TICK`] ticks per
//! second. Courses come from `backend/app/golf_courses.json`, embedded with
//! `include_str!` -- the same file the Python reads (itself a byte copy of
//! games/golf/courses.json), so there is no third copy to drift, exactly as
//! kart.rs:75 and fps.rs:121 do it.
//!
//! Rules: [`tdiv`] truncates toward zero, [`super::isqrt`] floors; no floats
//! anywhere in [`simulate`]. Moving obstacles (windmill blades, sliding gates)
//! are functions of the shot clock: the client sends the phase it putted at
//! ("clk", 0..[`CLOCK`]-1) and both sides roll from it.
//!
//! Golf is TURN-BASED and never reaches realtime.rs: its only timer is
//! [`super::After::GolfRecheck`], the one-shot the shared layer arms from
//! `on_disconnect` and from itself while [`grace_left`] is Some.
//!
//! The shared divergences from Python (one clock read per message, no seeded
//! RNG oracle, `SEND_TIMEOUT`, the deferred tail) are listed once at the top of
//! `backend/src/valley.rs`; this file does not repeat them. Its own are
//! marked DIVERGENCE at the site.

use super::{
    as_int_clamped, deg360, floordiv, isqrt, py_trunc, Ctx, Left, Out, RoomValley, Seq, I_GOLF,
};
use rand::rngs::StdRng;
use rand::seq::SliceRandom;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

/// The game key on the wire.
pub const GAME: &str = "golf";

// ------------------------------------------------------------------ physics --
pub const TILE: i64 = 10000;
pub const HALF: i64 = TILE / 2;
/// Ball radius.
pub const R: i64 = 350;
/// Cup radius: the ball centre must be inside it.
pub const CUP: i64 = 670;
/// Velocity scale.
pub const VS: i64 = 256;
/// Ticks per second.
pub const TICK: i64 = 120;
pub const VMIN: i64 = 30 * VS;
pub const VMAX: i64 = 360 * VS;
/// Proportional drag: 25/10000 of the speed per tick.
pub const DRAG_NUM: i64 = 25;
pub const DRAG_DEN: i64 = 10000;
/// Constant rolling resistance per tick.
pub const ROLL: i64 = 90;
pub const STOP: i64 = 60;
/// A wall keeps 3/4 of the normal speed.
pub const REST_NUM: i64 = 3;
pub const REST_DEN: i64 = 4;
/// Faster balls roll over the cup.
pub const CAPTURE: i64 = 160 * VS;
/// At most 150 units of travel per collision substep.
pub const SUBSTEP: i64 = 150 * VS;
pub const MAX_TICKS: i64 = 1800;
pub const MAX_STROKES: i64 = 8;
pub const OOB_PENALTY: i64 = 1;
pub const AIM_MAX: i64 = 4096;
// Surfaces, bumpers and the shot clock that drives every moving obstacle.
/// Sand: much more drag and rolling resistance.
pub const SAND_DRAG: i64 = 150;
pub const SAND_ROLL: i64 = 420;
/// Ice: almost none.
pub const ICE_DRAG: i64 = 6;
pub const ICE_ROLL: i64 = 22;
/// A bumper sends the ball off at 5/4 of its normal speed.
pub const BUMP_NUM: i64 = 5;
pub const BUMP_DEN: i64 = 4;
/// The shot clock wraps every 24 s; every mover's period divides it.
pub const CLOCK: i64 = 2880;
/// A windmill blade passes the doorway every 144 ticks ...
pub const BLADE_GAP: i64 = 144;
/// ... and blocks it for 20 ticks either side of straight down.
pub const BLADE_HIT: i64 = 20;
pub const Z_SAND: i64 = 1;
pub const Z_ICE: i64 = 2;
pub const Z_WATER: i64 = 3;
/// Python's `ZONES` dict, as a lookup by the course file's surface name. A name
/// the table does not know is zone 0 (plain grass), which is `ZONES.get(z[0], 0)`.
pub fn zone_kind(name: &str) -> i64 {
    match name {
        "sand" => Z_SAND,
        "ice" => Z_ICE,
        "water" => Z_WATER,
        _ => 0,
    }
}
/// The eight bumper facets, as thousandths of the radius (golf.py:63).
pub const OCT: [(i64, i64); 8] = [
    (1000, 0), (707, 707), (0, 1000), (-707, 707), (-1000, 0), (-707, -707), (0, -1000),
    (707, -707),
];

/// Python's `tdiv(a, b)`: integer division TRUNCATED TOWARD ZERO. Rust's `/`
/// already truncates toward zero, which is the whole reason golf.py:70 has to
/// spell out `abs(a) // abs(b)` and a sign while this is one operator -- Python's
/// `//` floors. Do NOT reach for [`floordiv`] here: `tdiv(-7, 2)` is -3 and
/// `-7 // 2` is -4, and the golden vectors see the difference on every bounce
/// that pushes the ball back along -x or -z.
pub fn tdiv(a: i64, b: i64) -> i64 {
    a / b
}

/// three.js `rotation.y = k * 90 degrees`, on integers (golf.py:75).
pub fn rot(x: i64, z: i64, k: i64) -> (i64, i64) {
    // Python's `k & 3` on a negative int is its two's-complement low bits, which
    // is what Rust's `&` gives too, so -1 & 3 == 3 in both. Every k in the
    // course file is 0..3 regardless.
    match k & 3 {
        1 => (z, -x),
        2 => (-x, -z),
        3 => (-z, x),
        _ => (x, z),
    }
}

/// Which tile column (or row) a world coordinate is in. FLOOR division, not
/// Rust's truncating `/`: `cell_of(-5001)` must be -1, and a truncating divide
/// would say 0 and quietly take the ball off the course on the -x/-z half of
/// every hole.
pub fn cell_of(v: i64) -> i64 {
    floordiv(v + HALF, TILE)
}

/// One wall: its start, its delta, its length (never 0, so `tdiv` by it is
/// safe) and its kind -- 0 a wall, 1 a bumper facet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Seg {
    pub x1: i64,
    pub z1: i64,
    pub dx: i64,
    pub dz: i64,
    pub l: i64,
    pub kind: i64,
}

/// Python's `seg(..)` (golf.py:91), including its `isqrt(..) or 1`.
pub fn seg(x1: i64, z1: i64, x2: i64, z2: i64, kind: i64) -> Seg {
    let (dx, dz) = (x2 - x1, z2 - z1);
    let l = isqrt(dx * dx + dz * dz);
    Seg { x1, z1, dx, dz, l: if l == 0 { 1 } else { l }, kind }
}

/// Python's `box(..)` (golf.py:96): the axis-aligned (lo x, lo z, hi x, hi z).
/// Renamed because `box` is a reserved word in Rust.
pub fn bbox_of(x1: i64, z1: i64, x2: i64, z2: i64) -> (i64, i64, i64, i64) {
    (x1.min(x2), z1.min(z2), x1.max(x2), z1.max(z2))
}

/// JS `v|0` for the small integers in the course file -- Python's `_i`. The
/// shared layer's [`super::js_int`] is exactly this; named here because the
/// Python has a local and a reader will look for it.
fn i_of(v: Option<&Value>) -> i64 {
    super::js_int(v)
}

// ------------------------------------------------------------------ courses --
/// The course file, embedded at build time: the same bytes the Python reads
/// (golf.py:106-108), so the two cannot drift.
pub const COURSES_JSON: &str = include_str!("../../data/golf_courses.json");

/// Python's module-level `DATA`.
pub static DATA: LazyLock<Value> =
    LazyLock::new(|| serde_json::from_str(COURSES_JSON).expect("golf_courses.json parses"));

/// A gravity box: (lo x, lo z, hi x, hi z, gx, gz). The ball inside it is pulled
/// along (gx, gz) every tick.
pub type Slope = (i64, i64, i64, i64, i64, i64);
/// A surface patch: (kind, lo x, lo z, hi x, hi z).
pub type Zone = (i64, i64, i64, i64, i64);

/// A moving obstacle. Python stores two differently-shaped tuples in one list
/// discriminated by `m[0]`; the field names are that tuple's positions, so
/// `period` is `m[7]` and `phase` is `m[8]`.
#[derive(Clone, Debug)]
pub enum Mover {
    /// `(0, seg)`: a windmill's blade wall, which exists only while
    /// [`blades_down`].
    Blade(Seg),
    /// `(1, x, z, axis, hw, hd, travel, period, phase)`: a gate sliding along
    /// `axis` (0 = x, 1 = z).
    Slider {
        x: i64,
        z: i64,
        axis: i64,
        hw: i64,
        hd: i64,
        travel: i64,
        period: i64,
        phase: i64,
    },
}

/// One compiled hole: every tile placed into world space.
pub struct Hole {
    /// The hole's display name. Python's `compile_hole` returns it and the page
    /// shows it; nothing on this side of the wire reads it except the tests'
    /// assertion messages, so it is kept for parity and the allow says why.
    #[allow(dead_code)] // part of the compiled hole the Python returns
    pub name: String,
    pub par: i64,
    pub segs: Vec<Seg>,
    /// Broad phase: tile cell -> the indices into `segs` that reach into it, in
    /// ASCENDING index order. The order is load-bearing: [`hit`] mutates the
    /// ball, so two walls resolved in the other order give another bounce.
    pub grid: HashMap<(i64, i64), Vec<usize>>,
    pub floor: HashSet<(i64, i64)>,
    pub voids: Vec<(i64, i64, i64, i64)>,
    pub slopes: Vec<Slope>,
    pub zones: Vec<Zone>,
    pub movers: Vec<Mover>,
    /// (x, z, radius) per bumper. The PHYSICS reads the octagon facets in
    /// `segs`, not this; Python builds the list for `games/golf.js` to draw from
    /// and never reads it back either.
    #[allow(dead_code)] // write-only in the Python too; the page draws from it
    pub bumpers: Vec<(i64, i64, i64)>,
    pub tee: (i64, i64),
    pub cup: (i64, i64),
    pub bbox: (i64, i64, i64, i64),
}

/// Place every tile: world wall segments, a per-cell broad phase, floor cells,
/// voids, slopes, surfaces, bumpers, moving obstacles, the tee and the cup.
/// Python's `compile_hole` (golf.py:112), reading the hole off a JSON value
/// exactly as the Python reads it off a dict, so a test can hand in an ad-hoc
/// hole the way `test_golf.py`'s `_lane` does.
///
/// DIVERGENCE: Python's optional `pieces` parameter is not ported -- no caller
/// in the repo passes it, and the one reader (`DATA["pieces"]`) is global.
///
/// DIVERGENCE: a hole with no `start` tile or no `hole-*` tile panics HERE,
/// where Python returns `tee: None` / `cup: None` and raises later, in
/// `simulate` or `on_floor`. Every hole in the file has exactly one of each
/// (`every_hole_compiles` pins it), so the only way to reach this is a
/// hand-written hole in a test, and failing at the build is the clearer place.
pub fn compile_hole(hole: &Value) -> Hole {
    let pieces = &DATA["pieces"];
    let mut segs: Vec<Seg> = Vec::new();
    let mut seen: HashSet<(i64, i64, i64, i64)> = HashSet::new();
    let mut floor: HashSet<(i64, i64)> = HashSet::new();
    let mut voids: Vec<(i64, i64, i64, i64)> = Vec::new();
    let mut slopes: Vec<Slope> = Vec::new();
    let mut zones: Vec<Zone> = Vec::new();
    let mut movers: Vec<Mover> = Vec::new();
    let mut bumpers: Vec<(i64, i64, i64)> = Vec::new();
    let (mut tee, mut cup) = (None, None);
    let (mut cols, mut rows) = (Vec::new(), Vec::new());
    let empty: Vec<Value> = Vec::new();
    let arr = |v: &Value, k: &str| -> Vec<Value> {
        v.get(k).and_then(Value::as_array).cloned().unwrap_or_default()
    };
    for tile in hole["tiles"].as_array().unwrap_or(&empty) {
        let name = tile[0].as_str().expect("a tile names its piece");
        // DIVERGENCE: Python unpacks col/row/k RAW (`for name, col, row, k in
        // ..`) where `_i` is applied only to the zone/bumper/mover fields, so a
        // float column would stay a float there and put floats in the floor set.
        // Every tile in the file is integral; truncating here keeps the whole
        // compiler on i64 and cannot change a value that exists.
        let (col, row, k) = (i_of(tile.get(1)), i_of(tile.get(2)), i_of(tile.get(3)));
        // Python's `pieces[name]` -- a KeyError on an unknown piece, here a panic
        // in the same place.
        let p = pieces.get(name).unwrap_or_else(|| panic!("no golf piece {name:?}"));
        let (cx, cz) = (col * TILE, row * TILE);
        floor.insert((col, row));
        cols.push(col);
        rows.push(row);
        for s in arr(p, "segs") {
            let (ax, az) = rot(i_of(s.get(0)), i_of(s.get(1)), k);
            let (bx, bz) = rot(i_of(s.get(2)), i_of(s.get(3)), k);
            let s = (cx + ax, cz + az, cx + bx, cz + bz);
            // The same shared wall placed by two neighbouring tiles is one wall:
            // Python's `min(s, reversed)` keys the pair by its unordered ends,
            // and tuple comparison is lexicographic in both languages.
            let key = s.min((s.2, s.3, s.0, s.1));
            if !seen.insert(key) {
                continue;
            }
            segs.push(seg(s.0, s.1, s.2, s.3, 0));
        }
        for s in arr(p, "voids") {
            let (ax, az) = rot(i_of(s.get(0)), i_of(s.get(1)), k);
            let (bx, bz) = rot(i_of(s.get(2)), i_of(s.get(3)), k);
            voids.push(bbox_of(cx + ax, cz + az, cx + bx, cz + bz));
        }
        // a slope: a box where gravity pulls the ball along (gx, gz) every tick
        for s in arr(p, "slopes") {
            let (ax, az) = rot(i_of(s.get(0)), i_of(s.get(1)), k);
            let (bx, bz) = rot(i_of(s.get(2)), i_of(s.get(3)), k);
            let g = rot(i_of(s.get(4)), i_of(s.get(5)), k);
            let b = bbox_of(cx + ax, cz + az, cx + bx, cz + bz);
            slopes.push((b.0, b.1, b.2, b.3, g.0, g.1));
        }
        if let Some(bl) = p.get("blades") {
            let (ax, az) = rot(i_of(bl.get(0)), i_of(bl.get(1)), k);
            let (bx, bz) = rot(i_of(bl.get(2)), i_of(bl.get(3)), k);
            movers.push(Mover::Blade(seg(cx + ax, cz + az, cx + bx, cz + bz, 0)));
        }
        if let Some(tv) = p.get("tee") {
            let (tx, tz) = rot(i_of(tv.get(0)), i_of(tv.get(1)), k);
            tee = Some((cx + tx, cz + tz));
        }
        if let Some(uv) = p.get("cup") {
            let (ux, uz) = rot(i_of(uv.get(0)), i_of(uv.get(1)), k);
            cup = Some((cx + ux, cz + uz));
        }
    }
    // per-hole surfaces: [kind, col, row, x1, z1, x2, z2] (tile-local, not rotated)
    for z in arr(hole, "zones") {
        let (c, r) = (i_of(z.get(1)) * TILE, i_of(z.get(2)) * TILE);
        let b = bbox_of(
            c + i_of(z.get(3)),
            r + i_of(z.get(4)),
            c + i_of(z.get(5)),
            r + i_of(z.get(6)),
        );
        zones.push((zone_kind(z[0].as_str().unwrap_or("")), b.0, b.1, b.2, b.3));
    }
    // bumpers: [col, row, x, z, radius], an octagon that kicks the ball back
    // harder than a wall. These segs are appended AFTER every tile wall and are
    // NOT deduplicated, so two bumpers that touch keep both facets.
    for b in arr(hole, "bumpers") {
        let (x, z, rad) = (
            i_of(b.get(0)) * TILE + i_of(b.get(2)),
            i_of(b.get(1)) * TILE + i_of(b.get(3)),
            i_of(b.get(4)),
        );
        let pts: Vec<(i64, i64)> =
            OCT.iter().map(|o| (x + tdiv(rad * o.0, 1000), z + tdiv(rad * o.1, 1000))).collect();
        for (i, p0) in pts.iter().enumerate() {
            let q = pts[(i + 1) % 8];
            segs.push(seg(p0.0, p0.1, q.0, q.1, 1));
        }
        bumpers.push((x, z, rad));
    }
    // sliders: ["slider", col, row, axis (0 = x, 1 = z), half width, half depth,
    // travel, period, phase]. DIVERGENCE: Python slices `m[3:9]` and a short
    // entry would build a short tuple that only fails later, in `slide_off`;
    // here a missing field reads 0 through `_i`, and a 0 period still divides by
    // zero in `slide_off` exactly as Python's does.
    for m in arr(hole, "movers") {
        if m[0].as_str() == Some("slider") {
            movers.push(Mover::Slider {
                x: i_of(m.get(1)) * TILE,
                z: i_of(m.get(2)) * TILE,
                axis: i_of(m.get(3)),
                hw: i_of(m.get(4)),
                hd: i_of(m.get(5)),
                travel: i_of(m.get(6)),
                period: i_of(m.get(7)),
                phase: i_of(m.get(8)),
            });
        }
    }
    let mut grid: HashMap<(i64, i64), Vec<usize>> = HashMap::new();
    for (i, sg) in segs.iter().enumerate() {
        let (lo_x, hi_x) = (sg.x1.min(sg.x1 + sg.dx) - R, sg.x1.max(sg.x1 + sg.dx) + R);
        let (lo_z, hi_z) = (sg.z1.min(sg.z1 + sg.dz) - R, sg.z1.max(sg.z1 + sg.dz) + R);
        for c in cell_of(lo_x)..=cell_of(hi_x) {
            for r in cell_of(lo_z)..=cell_of(hi_z) {
                grid.entry((c, r)).or_default().push(i);
            }
        }
    }
    Hole {
        // DIVERGENCE: a hole spec with no `name` or no `par` reads "" / 0 where
        // Python raises KeyError. Every hole in the file has both, and a test
        // pins that (`every_hole_compiles`); a hand-written hole missing one is
        // a test's own mistake and not worth a panic in the compiler.
        name: hole["name"].as_str().unwrap_or("").to_string(),
        par: hole["par"].as_i64().unwrap_or(0),
        segs,
        grid,
        floor,
        voids,
        slopes,
        zones,
        movers,
        bumpers,
        tee: tee.expect("a hole has a tee"),
        cup: cup.expect("a hole has a cup"),
        bbox: (
            cols.iter().min().copied().unwrap_or(0) * TILE - HALF,
            rows.iter().min().copied().unwrap_or(0) * TILE - HALF,
            cols.iter().max().copied().unwrap_or(0) * TILE + HALF,
            rows.iter().max().copied().unwrap_or(0) * TILE + HALF,
        ),
    }
}

/// "Play random": a round of N holes drawn (no repeats) from every hole of every
/// course.
pub const RANDOM: &str = "random";
pub const RANDOM_SIZES: [i64; 3] = [5, 10, 15];

/// One course, compiled. Python keeps `COURSES` (the raw dicts, keyed by id, in
/// file order) and `_COMPILED` (the lazily built holes) apart; here they are one
/// list in file order, because `list(golf.COURSES)` order reaches a test and
/// nothing needs the two halves separately.
pub struct Course {
    pub id: String,
    pub holes: Vec<Hole>,
}

/// Every course, compiled once on first use -- Python's `COURSES` plus the
/// `_COMPILED` cache `course_holes` fills. Python compiles one course the first
/// time it is asked for; 25 holes is a few milliseconds, so this does all of
/// them at once and the cache is the `LazyLock` itself.
pub static COURSES: LazyLock<Vec<Course>> = LazyLock::new(|| {
    DATA["courses"]
        .as_array()
        .expect("golf_courses.json has courses")
        .iter()
        .map(|c| Course {
            id: c["id"].as_str().expect("course id").to_string(),
            holes: c["holes"]
                .as_array()
                .expect("course holes")
                .iter()
                .map(compile_hole)
                .collect(),
        })
        .collect()
});

/// Python's `ALL_HOLES`: `[(course_id, hole_index), ..]` in file order.
pub static ALL_HOLES: LazyLock<Vec<(String, usize)>> = LazyLock::new(|| {
    COURSES.iter().flat_map(|c| (0..c.holes.len()).map(|i| (c.id.clone(), i))).collect()
});

/// Python's `course_holes(course_id)`. None for an id no course has, where
/// Python raises `KeyError` -- every caller has already checked membership
/// through [`course_exists`], and the one that has not (`Golf::holes` on a
/// course set by [`Golf::start`]) could only reach it with a course that passed
/// that check.
pub fn course_holes(course_id: &str) -> Option<&'static [Hole]> {
    COURSES.iter().find(|c| c.id == course_id).map(|c| c.holes.as_slice())
}

/// Python's `course in COURSES`.
pub fn course_exists(course_id: &str) -> bool {
    COURSES.iter().any(|c| c.id == course_id)
}

/// n distinct [course_id, hole_index] pairs in play order (capped at every hole
/// there is). Python's `pick_mix` (golf.py:199).
///
/// DIVERGENCE (shared divergence 2): `random.Random.sample` cannot be
/// reproduced, so this ports the DISTRIBUTION -- `partial_shuffle` is a partial
/// Fisher-Yates, which like `sample` gives a uniformly random subset in a
/// uniformly random order. The tests assert the properties the Python test
/// asserts (n distinct pairs, all of them real holes, more than one course over
/// several rounds) and no seeded oracle.
pub fn pick_mix(n: i64, rng: &mut StdRng) -> Vec<(String, usize)> {
    let mut all = ALL_HOLES.clone();
    let k = (n.max(0) as usize).min(all.len());
    all.partial_shuffle(rng, k).0.to_vec()
}

/// Python's `on_floor` (golf.py:210): on a tile, and not over one of its voids.
/// Note both the void and the zone tests are STRICT (`x1 < x < x2`), so a ball
/// exactly on a boundary is outside.
pub fn on_floor(h: &Hole, x: i64, z: i64) -> bool {
    if !h.floor.contains(&(cell_of(x), cell_of(z))) {
        return false;
    }
    !h.voids.iter().any(|v| v.0 < x && x < v.2 && v.1 < z && z < v.3)
}

/// The surface under (x, z), 0 for plain grass. FIRST match in course-file
/// order, which is why `zones` is a Vec.
pub fn zone_at(h: &Hole, x: i64, z: i64) -> i64 {
    for &(kind, x1, z1, x2, z2) in &h.zones {
        if x1 < x && x < x2 && z1 < z && z < z2 {
            return kind;
        }
    }
    0
}

/// The gravity box under (x, z), or None. First match in order, as above.
pub fn slope_at(h: &Hole, x: i64, z: i64) -> Option<&Slope> {
    h.slopes.iter().find(|v| v.0 < x && x < v.2 && v.1 < z && z < v.3)
}

/// A slider's centre offset along its axis at clock phase ph (a triangle wave).
/// Python's `slide_off` (golf.py:233).
pub fn slide_off(m: &Mover, ph: i64) -> i64 {
    let Mover::Slider { travel, period, phase, .. } = *m else {
        // Python indexes m[8] on a blade tuple and raises IndexError; nothing
        // calls this with one.
        panic!("slide_off on a blade");
    };
    // Python's `%` floors; every phase in the file is non-negative, so
    // `rem_euclid` and `%` agree, and `rem_euclid` is the one that keeps
    // agreeing if a course file ever carries a negative phase.
    let u = (ph + phase).rem_euclid(period);
    let half = tdiv(period, 2);
    let tri = if u < half { u } else { period - u };
    -travel + tdiv(2 * travel * tri, half)
}

/// Windmill blades block their doorway while one sweeps past the bottom.
pub fn blades_down(ph: i64) -> bool {
    (ph + BLADE_HIT).rem_euclid(BLADE_GAP) < 2 * BLADE_HIT
}

/// The obstacle walls that exist at clock phase ph. Python's `mover_segs`
/// (golf.py:246); the four slider walls are pushed in Python's order, and
/// [`hit`] resolves them in it.
pub fn mover_segs(h: &Hole, ph: i64) -> Vec<Seg> {
    let mut out = Vec::new();
    for m in &h.movers {
        match m {
            Mover::Blade(sg) => {
                if blades_down(ph) {
                    out.push(*sg);
                }
            }
            Mover::Slider { x, z, axis, hw, hd, .. } => {
                let off = slide_off(m, ph);
                // `0 if m[3] else off` -- Python truthiness on the axis: axis 1
                // (z) leaves x alone, axis 0 (x) leaves z alone.
                let (x, z) = (x + if *axis != 0 { 0 } else { off },
                              z + if *axis != 0 { off } else { 0 });
                let (x1, z1, x2, z2) = (x - hw, z - hd, x + hw, z + hd);
                out.push(seg(x1, z1, x2, z1, 0));
                out.push(seg(x2, z1, x2, z2, 0));
                out.push(seg(x2, z2, x1, z2, 0));
                out.push(seg(x1, z2, x1, z1, 0));
            }
        }
    }
    out
}

// ----------------------------------------------------------------- simulate --
/// Python's `launch` (golf.py:262): the aim vector scaled to the power's speed.
pub fn launch(ax: i64, az: i64, power: i64) -> (i64, i64) {
    let m = isqrt(ax * ax + az * az);
    let m = if m == 0 { 1 } else { m };
    let sp = VMIN + tdiv((power - 1) * (VMAX - VMIN), 99);
    (tdiv(ax * sp, m), tdiv(az * sp, m))
}

/// One shot's outcome. Python's `{"end", "holed", "oob", "water", "ticks"}`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shot {
    pub end: (i64, i64),
    pub holed: bool,
    pub oob: bool,
    pub water: bool,
    pub ticks: i64,
}

/// Python's inner `hit(sg)` (golf.py:277), which mutates the ball and the
/// velocity through `nonlocal`. Taken as `&mut` here for the same reason: the
/// substep loop resolves several walls in sequence and each one sees the
/// previous one's push.
fn hit(sg: &Seg, b: &mut (i64, i64), v: &mut (i64, i64)) {
    let Seg { x1, z1, dx, dz, l, kind } = *sg;
    let p = tdiv((b.0 - x1) * dx + (b.1 - z1) * dz, l);
    let p = if p < 0 { 0 } else if p > l { l } else { p };
    let (px, pz) = (x1 + tdiv(dx * p, l), z1 + tdiv(dz * p, l));
    // ox/oz are the offset from the OLD ball centre, and the velocity reflection
    // below still uses them after the ball has been pushed out.
    let (ox, oz) = (b.0 - px, b.1 - pz);
    let d2 = ox * ox + oz * oz;
    if d2 < R * R {
        let d = isqrt(d2);
        let d = if d == 0 { 1 } else { d };
        b.0 = px + tdiv(ox * R, d);
        b.1 = pz + tdiv(oz * R, d);
        let vn = tdiv(v.0 * ox + v.1 * oz, d);
        let (num, den) = if kind != 0 { (BUMP_NUM, BUMP_DEN) } else { (REST_NUM, REST_DEN) };
        if vn < 0 {
            v.0 -= tdiv((den + num) * vn * ox, den * d);
            v.1 -= tdiv((den + num) * vn * oz, den * d);
        }
    }
}

/// Roll one shot from the shot clock's phase clk. Python's `simulate`
/// (golf.py:268), statement for statement and in the same order -- the
/// integration order, the substep count, the collision pass, the slope read
/// before the clamp and the rest threshold are all load-bearing, and
/// `golf_golden.json` is what proves they stayed that way.
pub fn simulate(h: &Hole, bx: i64, bz: i64, ax: i64, az: i64, power: i64, clk: i64) -> Shot {
    let (sx, sz) = (bx, bz);
    let mut b = (bx, bz);
    let mut v = launch(ax, az, power);
    let (cx, cz) = h.cup;
    let clk = clk.rem_euclid(CLOCK);
    for t in 1..=MAX_TICKS {
        let dyn_segs =
            if !h.movers.is_empty() { mover_segs(h, (clk + t).rem_euclid(CLOCK)) } else { Vec::new() };
        let mut s = isqrt(v.0 * v.0 + v.1 * v.1);
        let n = 1.max(tdiv(s + SUBSTEP - 1, SUBSTEP));
        for _ in 0..n {
            b.0 += tdiv(v.0, n * VS);
            b.1 += tdiv(v.1, n * VS);
            if let Some(ids) = h.grid.get(&(cell_of(b.0), cell_of(b.1))) {
                for &i in ids {
                    hit(&h.segs[i], &mut b, &mut v);
                }
            }
            for sg in &dyn_segs {
                hit(sg, &mut b, &mut v);
            }
        }
        let sl = slope_at(h, b.0, b.1);
        if let Some(g) = sl {
            v.0 += g.4;
            v.1 += g.5;
        }
        s = isqrt(v.0 * v.0 + v.1 * v.1);
        if s > VMAX {
            v = (tdiv(v.0 * VMAX, s), tdiv(v.1 * VMAX, s));
            s = isqrt(v.0 * v.0 + v.1 * v.1);
        }
        let (ddx, ddz) = (b.0 - cx, b.1 - cz);
        if ddx * ddx + ddz * ddz < CUP * CUP && s <= CAPTURE {
            return Shot { end: (cx, cz), holed: true, oob: false, water: false, ticks: t };
        }
        if !on_floor(h, b.0, b.1) {
            return Shot { end: (sx, sz), holed: false, oob: true, water: false, ticks: t };
        }
        let zk = zone_at(h, b.0, b.1);
        if zk == Z_WATER {
            return Shot { end: (sx, sz), holed: false, oob: true, water: true, ticks: t };
        }
        let dn = if zk == Z_SAND {
            SAND_DRAG
        } else if zk == Z_ICE {
            ICE_DRAG
        } else {
            DRAG_NUM
        };
        let rl = if zk == Z_SAND {
            SAND_ROLL
        } else if zk == Z_ICE {
            ICE_ROLL
        } else {
            ROLL
        };
        let mut ns = s - tdiv(s * dn, DRAG_DEN) - rl;
        if ns <= STOP {
            if sl.is_none() {
                return Shot { end: b, holed: false, oob: false, water: false, ticks: t };
            }
            if ns < 0 {
                ns = 0; // on a slope the ball never rests: gravity takes it next tick
            }
        }
        if s != 0 {
            v = (tdiv(v.0 * ns, s), tdiv(v.1 * ns, s));
        }
    }
    Shot { end: b, holed: false, oob: false, water: false, ticks: MAX_TICKS }
}

// ------------------------------------------------------------------ referee --
/// Scorecard cut-scene between holes (seconds).
pub const HOLE_PAUSE: f64 = 4.0;
pub const SHOT_GRACE: f64 = 0.3;
/// Walking updates: a token bucket per player, 3 deep, ...
pub const POS_BURST: f64 = 3.0;
/// ... refilled 12 per second (clients send <= 10/s).
pub const POS_RATE: f64 = 12.0;
/// A dropped player's card is kept this long for a rejoin.
pub const PARK_SECS: f64 = 120.0;
/// A socket drop holds the hole open this long for a rejoin. This is where
/// `valley::HOST_GRACE` comes from (`HOST_GRACE = golfmod.GRACE`, valley.py:79).
pub const GRACE: f64 = 15.0;
pub const CHARS: i64 = 6;
pub const COLORS: i64 = 8;
/// A round seats the first eight of the lobby (golf.py:423, golf.py:585).
pub const MAX_PLAYERS: usize = 8;

/// `idle | playing | done`, which are also the three strings the `phase` field
/// carries on the wire.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Phase {
    #[default]
    Idle,
    Playing,
    Done,
}

impl Phase {
    pub fn as_str(self) -> &'static str {
        match self {
            Phase::Idle => "idle",
            Phase::Playing => "playing",
            Phase::Done => "done",
        }
    }
}

/// One player's card. Python's `self.players[uid]` dict.
#[derive(Clone, Debug)]
pub struct Player {
    /// Their public profile, refreshed on a restore.
    pub user: Value,
    pub c: i64,
    pub color: i64,
    pub ball: (i64, i64),
    pub done: bool,
    pub strokes: Vec<i64>,
    pub busy_until: f64,
}

/// A dropped player's card, kept for [`PARK_SECS`] so a reconnect restores it.
#[derive(Clone, Debug)]
struct Parked {
    p: Player,
    hole: usize,
    at: f64,
    course: Option<String>,
    mix: Option<Vec<(String, usize)>>,
    /// The socket dropped rather than an explicit leave, so the hole also waits
    /// [`GRACE`] seconds for them.
    blip: bool,
}

/// One room's Mini Golf round. Everybody plays at once (balls are ghosts); the
/// hole advances when every player has holed out or picked up. Python's
/// `Golf.__init__` (golf.py:356), field for field and in its order.
#[derive(Default)]
pub struct Golf {
    pub course: Option<String>,
    /// A random round: [[course_id, hole_index], ..].
    pub mix: Option<Vec<(String, usize)>>,
    pub hole: usize,
    pub phase: Phase,
    /// In join order: it decides the `players` list, the card, the totals and
    /// the winners list, all of which reach the wire.
    pub players: Seq<Player>,
    /// user_id -> character, kept between rounds. Reaches the wire as the view's
    /// `chars` object, so insertion-ordered.
    pub chars: Seq<i64>,
    /// user_id -> [tokens, last refill time]. Never serialised, so a plain map.
    bucket: HashMap<String, (f64, f64)>,
    /// user_id -> their parked card. Only ever reached by key or scanned with
    /// `any`/`max`, so a plain map.
    parked: HashMap<String, Parked>,
    pub hole_ready_at: f64,
    /// Python keeps the last shot per player and NOTHING EVER READS IT -- it is
    /// set in `shot`, cleared in `start`/`advance`/`end`/`expire` and popped in
    /// `drop`, and no view, event or rule looks at it. Ported because it is
    /// state the Python holds and a reader comparing the two files will look
    /// for it; the allow says what the port cannot show.
    #[allow(dead_code)] // write-only in the Python too; see above
    last_shot: HashMap<String, (Value, f64)>,
}

impl Golf {
    // -- views --
    /// Python's `holes()`: a random round's drawn holes, else the course's, else
    /// nothing.
    pub fn holes(&self) -> Vec<&'static Hole> {
        if let Some(mix) = self.mix.as_ref().filter(|m| !m.is_empty()) {
            // `if self.mix:` -- an empty mix is falsy in Python, so it falls
            // through to the course.
            return mix
                .iter()
                .filter_map(|(cid, i)| course_holes(cid).and_then(|hs| hs.get(*i)))
                .collect();
        }
        match self.course.as_deref().filter(|c| !c.is_empty()) {
            Some(c) => course_holes(c).map(|hs| hs.iter().collect()).unwrap_or_default(),
            None => Vec::new(),
        }
    }

    /// Python's `view(t)`, which is `None` before the first round -- that
    /// reaches the page as `"round": null`, never an omitted key.
    pub fn view(&self, t: f64) -> Value {
        let Some(course) = self.course.as_deref() else { return Value::Null };
        let holes = self.holes();
        let mut out = json!({
            "course": course,
            "hole": self.hole,
            "phase": self.phase.as_str(),
            "par": holes.iter().map(|h| h.par).collect::<Vec<_>>(),
            "players": self.players.values().map(|p| json!({
                "user": p.user,
                "c": p.c,
                "color": p.color,
                "ball": [p.ball.0, p.ball.1],
                "done": p.done,
                "strokes": p.strokes,
            })).collect::<Vec<_>>(),
            "readyInMs": py_trunc((self.hole_ready_at - t) * 1000.0).max(0),
            "chars": self.chars.to_object(|c| json!(c)),
        });
        if let Some(mix) = self.mix.as_ref().filter(|m| !m.is_empty()) {
            // every client builds the same holes from this
            out["mix"] = mix_value(mix);
        }
        out
    }

    /// Python's `card()`: user_id -> that player's per-hole strokes, in player
    /// order.
    pub fn card(&self) -> Seq<Vec<i64>> {
        let mut c = Seq::new();
        for (uid, p) in self.players.iter() {
            c.set(uid, p.strokes.clone());
        }
        c
    }

    // -- ops --
    /// Python's `char(uid, msg)`: pick a character, kept across rounds, and
    /// applied to a card already in play.
    pub fn char(&mut self, uid: &str, msg: &Value) -> Option<i64> {
        let c = as_int_clamped(msg.get("c"), 0, CHARS - 1)?;
        self.chars.set(uid, c);
        if let Some(p) = self.players.get_mut(uid) {
            p.c = c;
        }
        Some(c)
    }

    /// Start a round on one course, or (course "random", holes 5|10|15) on holes
    /// the server draws from every course with its own rng. The error string, or
    /// None. Python's `start` (golf.py:400).
    ///
    /// `members` is the lobby in join order; the first [`MAX_PLAYERS`] play.
    ///
    /// DIVERGENCE: Python's `rng` is optional and falls back to
    /// `random.Random(secrets.randbits(64))`. The hub draws a fresh generator
    /// per message and always passes it (valley.py:1130 does the same fallback
    /// and never needs it), so there is no None case to port.
    pub fn start(
        &mut self, members: &[(String, Value)], course: Option<&Value>, t: f64,
        holes_arg: Option<&Value>, rng: &mut StdRng,
    ) -> Option<&'static str> {
        if self.phase == Phase::Playing {
            return Some("a round is already on: the host can end it first");
        }
        let mut mix = None;
        let course_str = course.and_then(Value::as_str);
        if course_str == Some(RANDOM) {
            // `isinstance(holes, bool) or not isinstance(holes, int)`: a JSON
            // 5.0 is a Python float and is REFUSED, so `as_i64` (which is None
            // for serde_json's f64 variant) is the right test and
            // `int_exact` -- which accepts 10.0 -- is not.
            let n = holes_arg.filter(|v| !v.is_boolean()).and_then(Value::as_i64);
            match n {
                Some(n) if RANDOM_SIZES.contains(&n) => mix = Some(pick_mix(n, rng)),
                _ => return Some("pick 5, 10 or 15 holes"),
            }
        } else if !course_str.map(course_exists).unwrap_or(false) {
            return Some("pick a course");
        }
        self.course = course_str.map(str::to_string);
        self.mix = mix;
        self.hole = 0;
        self.phase = Phase::Playing;
        self.hole_ready_at = t;
        self.last_shot.clear();
        self.parked.clear();
        let holes = self.holes();
        let tee = holes[0].tee;
        // `self.chars` and `self.bucket` deliberately survive a start: the
        // chosen character is kept between rounds and the rate bucket is a
        // network property, not a round's.
        self.players = Seq::new();
        for (i, (uid, pubv)) in members.iter().take(MAX_PLAYERS).enumerate() {
            let i = i as i64;
            self.players.set(
                uid,
                Player {
                    user: pubv.clone(),
                    c: self.chars.get(uid).copied().unwrap_or(i % CHARS),
                    color: i % COLORS,
                    ball: tee,
                    done: false,
                    strokes: vec![0; holes.len()],
                    busy_until: 0.0,
                },
            );
        }
        None
    }

    /// Validate and roll one shot: the `shot` event, or the error. Python's
    /// `shot` (golf.py:429), which returns the pair `(ev, err)`.
    pub fn shot(&mut self, uid: &str, msg: &Value, t: f64) -> Result<Value, &'static str> {
        if self.phase != Phase::Playing {
            return Err("no round is being played");
        }
        // Python reads `p` before the phase test but only uses it after, so the
        // order of these two checks is the order of the `return`s, not the gets.
        let Some(p) = self.players.get(uid) else { return Err("you're not in this round") };
        if p.done {
            return Err("you've finished this hole");
        }
        // `msg.get("hole") is not None`: a JSON null is Python's None, so an
        // explicit `"hole": null` is "no hole given", not "hole 0".
        let hole_no = msg.get("hole").filter(|v| !v.is_null());
        if hole_no.is_some() && as_int_clamped(hole_no, -1, 1 << 16) != Some(self.hole as i64) {
            // An unparseable hole lands here too: `None != self.hole`.
            return Err("that hole is over");
        }
        if t < p.busy_until.max(self.hole_ready_at) {
            return Err("wait for the ball to stop");
        }
        let ax = as_int_clamped(msg.get("ax"), -AIM_MAX, AIM_MAX);
        let az = as_int_clamped(msg.get("az"), -AIM_MAX, AIM_MAX);
        let power = as_int_clamped(msg.get("power"), 1, 100);
        let (Some(ax), Some(az), Some(power)) = (ax, az, power) else {
            return Err("bad shot");
        };
        if ax == 0 && az == 0 {
            return Err("bad shot");
        }
        // `as_int(..) or 0`: both a missing field and a 0 give 0.
        let seq = as_int_clamped(msg.get("seq"), 0, 1 << 30).unwrap_or(0);
        // the shot clock's phase at the putt
        let clk = as_int_clamped(msg.get("clk"), 0, CLOCK - 1).unwrap_or(0);
        let h = self.holes()[self.hole];
        let frm = p.ball;
        let res = simulate(h, frm.0, frm.1, ax, az, power, clk);
        let n = self.hole;
        let p = self.players.get_mut(uid).expect("checked above");
        p.strokes[n] += 1 + if res.oob { OOB_PENALTY } else { 0 };
        p.ball = res.end;
        p.busy_until = t + res.ticks as f64 / TICK as f64 + SHOT_GRACE;
        if res.holed {
            p.done = true;
        } else if p.strokes[n] >= MAX_STROKES {
            p.strokes[n] = MAX_STROKES;
            p.done = true;
        }
        let ev = json!({
            "user": p.user,
            "seq": seq,
            "hole": n,
            "from": [frm.0, frm.1],
            "ax": ax,
            "az": az,
            "power": power,
            "clk": clk,
            "end": [res.end.0, res.end.1],
            "holed": res.holed,
            "oob": res.oob,
            "water": res.water,
            "ticks": res.ticks,
            "strokes": p.strokes[n],
            "done": p.done,
        });
        self.last_shot.insert(uid.to_string(), (ev.clone(), t));
        Ok(ev)
    }

    /// Everyone listed who hasn't finished takes [`MAX_STROKES`] for the hole.
    /// The ids that did, in the order given.
    pub fn pick_up(&mut self, uids: &[String]) -> Vec<String> {
        let mut took = Vec::new();
        let (phase, hole) = (self.phase, self.hole);
        for uid in uids {
            if phase != Phase::Playing {
                continue;
            }
            if let Some(p) = self.players.get_mut(uid) {
                if !p.done {
                    p.strokes[hole] = MAX_STROKES;
                    p.done = true;
                    took.push(uid.clone());
                }
            }
        }
        took
    }

    /// If everyone is done with the hole: ("hole", data) or ("done", data). A
    /// player whose socket dropped mid-hole less than [`GRACE`] ago still counts
    /// as playing it. Python's `advance` (golf.py:479).
    pub fn advance(&mut self, t: f64, last_ticks: i64) -> Option<(&'static str, Value)> {
        self.expire(t);
        if self.phase != Phase::Playing
            || self.players.is_empty()
            || !self.players.values().all(|p| p.done)
            || self.holding(t)
        {
            return None;
        }
        let holes = self.holes();
        let card = self.card();
        if self.hole + 1 >= holes.len() {
            self.phase = Phase::Done;
            let mut totals: Seq<i64> = Seq::new();
            for (uid, s) in card.iter() {
                totals.set(uid, s.iter().sum());
            }
            let best = totals.values().copied().min().expect("players is not empty");
            let winners: Vec<&str> =
                totals.iter().filter(|(_, v)| **v == best).map(|(uid, _)| uid).collect();
            let mut data = json!({
                "card": card.to_object(|s| json!(s)),
                "totals": totals.to_object(|v| json!(v)),
                "par": holes.iter().map(|h| h.par).collect::<Vec<_>>(),
                "winners": winners,
                "course": self.course,
            });
            if let Some(mix) = self.mix.as_ref().filter(|m| !m.is_empty()) {
                data["mix"] = mix_value(mix);
            }
            return Some(("done", data));
        }
        self.hole += 1;
        let tee = holes[self.hole].tee;
        for p in self.players.values_mut() {
            p.ball = tee;
            p.done = false;
            p.busy_until = 0.0;
        }
        self.hole_ready_at = t + last_ticks as f64 / TICK as f64 + HOLE_PAUSE;
        self.last_shot.clear();
        Some((
            "hole",
            json!({
                "hole": self.hole,
                "delayMs": py_trunc((self.hole_ready_at - t) * 1000.0),
                "card": card.to_object(|s| json!(s)),
            }),
        ))
    }

    /// The host ended the round. Note `course` and `chars` SURVIVE, so the view
    /// after an end is the idle course, not `null`.
    pub fn end(&mut self) {
        self.phase = Phase::Idle;
        self.players = Seq::new();
        self.last_shot.clear();
        self.parked.clear();
    }

    /// Someone dropped off the network mid-hole and may still come back to it.
    pub fn holding(&self, t: f64) -> bool {
        self.parked
            .values()
            .any(|k| k.blip && k.hole == self.hole && !k.p.done && t - k.at < GRACE)
    }

    /// Seconds until the last hold on this hole runs out (None: nothing held).
    pub fn grace_left(&self, t: f64) -> Option<f64> {
        self.parked
            .values()
            .filter(|k| k.blip && k.hole == self.hole && !k.p.done && t - k.at < GRACE)
            .map(|k| GRACE - (t - k.at))
            // `max(left) if left else None` over finite floats.
            .fold(None, |acc: Option<f64>, x| Some(acc.map_or(x, |a: f64| a.max(x))))
    }

    /// The host skipped the hole: nobody parked holds it any more.
    pub fn release(&mut self) {
        for k in self.parked.values_mut() {
            k.blip = false;
        }
    }

    /// With nobody left in the round and no hold, it ends. True if it just did.
    pub fn expire(&mut self, t: f64) -> bool {
        if self.phase == Phase::Playing && self.players.is_empty() && !self.holding(t) {
            self.phase = Phase::Idle;
            self.parked.clear();
            self.last_shot.clear();
            return true;
        }
        false
    }

    /// A walking update to relay, or None (over the rate, not playing, or
    /// malformed). Rate: a token bucket ([`POS_BURST`] deep, [`POS_RATE`]/s), so
    /// network bunching of a 10 Hz sender never drops the final "stopped here"
    /// frame. No storage, no DB. Python's `pos` (golf.py:537).
    pub fn pos(&mut self, uid: &str, msg: &Value, t: f64) -> Option<Value> {
        if !self.players.contains_key(uid) || self.phase != Phase::Playing {
            return None;
        }
        let b = self.bucket.entry(uid.to_string()).or_insert((POS_BURST, t));
        b.0 = POS_BURST.min(b.0 + (t - b.1) * POS_RATE);
        b.1 = t;
        if b.0 < 1.0 {
            return None;
        }
        let (x0, z0, x1, z1) = self.holes()[self.hole].bbox;
        let x = as_int_clamped(msg.get("x"), x0 - 2 * TILE, x1 + 2 * TILE);
        let z = as_int_clamped(msg.get("z"), z0 - 2 * TILE, z1 + 2 * TILE);
        let r = as_int_clamped(msg.get("r"), -100000, 100000);
        let a = as_int_clamped(msg.get("a"), 0, 7);
        let (Some(x), Some(z), Some(r), Some(a)) = (x, z, r, a) else { return None };
        // Only a frame that is actually relayed costs a token.
        self.bucket.get_mut(uid).expect("just inserted").0 -= 1.0;
        let q = as_int_clamped(msg.get("q"), 0, (1 << 30) - 1);
        // `r % 360` on an int that the clamp has already pinned into
        // +-100000: deg360 is that same non-negative modulo, so -90 relays as
        // 270 and the f64 round trip is exact at this magnitude.
        let mut out = json!({"u": uid, "x": x, "z": z, "r": deg360(r as f64), "a": a});
        if let Some(q) = q {
            // A q of 0 IS sent: Python tests `is not None`, not truthiness.
            out["q"] = json!(q);
        }
        Some(out)
    }

    /// Remove a player; True if they were in the round. Mid-round their card is
    /// parked for [`PARK_SECS`] so a reconnect restores it. `blip`: the socket
    /// dropped (not an explicit leave), so the hole also waits [`GRACE`] seconds
    /// for them.
    pub fn drop(&mut self, uid: &str, t: f64, blip: bool) -> bool {
        self.bucket.remove(uid);
        self.last_shot.remove(uid);
        let Some(p) = self.players.remove(uid) else { return false };
        if self.phase == Phase::Playing {
            self.parked.insert(
                uid.to_string(),
                Parked {
                    p,
                    hole: self.hole,
                    at: t,
                    course: self.course.clone(),
                    mix: self.mix.clone(),
                    blip,
                },
            );
            self.expire(t);
        }
        true
    }

    /// A parked player rejoined the lobby: put them back in the round. Holes
    /// that finished without them count as picked up. Python's `restore`
    /// (golf.py:579).
    ///
    /// Note the park entry is POPPED FIRST, so a refused restore still forgets
    /// it -- a stale card never comes back on a later join.
    pub fn restore(&mut self, uid: &str, pubv: Value, t: f64) -> bool {
        let Some(k) = self.parked.remove(uid) else { return false };
        if self.phase != Phase::Playing
            || k.course != self.course
            || k.mix != self.mix
            || t - k.at > PARK_SECS
            || self.players.contains_key(uid)
            || self.players.len() >= MAX_PLAYERS
        {
            return false;
        }
        let mut p = k.p;
        p.user = pubv;
        if k.hole != self.hole {
            for i in k.hole..self.hole {
                if i == k.hole && p.done {
                    continue; // they did finish that one before they went
                }
                p.strokes[i] = MAX_STROKES;
            }
            p.ball = self.holes()[self.hole].tee;
            p.done = false;
            p.busy_until = 0.0;
        }
        // Appended at the END of the player order, as Python's
        // `self.players[uid] = p` does after the drop popped them.
        self.players.set(uid, p);
        true
    }
}

/// `[[course_id, hole_index], ..]` as the page reads it.
fn mix_value(mix: &[(String, usize)]) -> Value {
    Value::Array(mix.iter().map(|(cid, i)| json!([cid, i])).collect())
}

// -------------------------------------------------------------- the valley --

/// Python's `_golf_advance` (valley.py:1482): if the hole (or the round) moved
/// on, the step event and then the fresh view; if only the PHASE moved -- the
/// round's last, dropped player never came back -- just the view.
fn advance_into(gm: &mut Golf, ids: &[String], out: &mut Out, t: f64, ticks: i64) {
    let was = gm.phase;
    match gm.advance(t, ticks) {
        Some((ev, data)) => {
            out.lobby(ids.to_vec(), ev, data);
            out.lobby(ids.to_vec(), GAME, json!({"round": gm.view(t)}));
        }
        None if gm.phase != was => {
            out.lobby(ids.to_vec(), GAME, json!({"round": gm.view(t)}));
        }
        None => {}
    }
}

/// The join snapshot (valley.py:1049-1054). A parked player's card comes back
/// and the whole LOBBY is told with `back`; anyone else just gets the round on
/// their own socket, which is how a spectator sees a game in progress.
pub fn joined(v: &mut RoomValley, cx: &mut Ctx, out: &mut Out) {
    let gm = &mut v.golf;
    let t = cx.t;
    if gm.restore(cx.uid(), cx.me(), t) {
        // back from a dropped socket: everyone sees them return to the round
        let ids = v.lobbies[I_GOLF].ids();
        out.lobby(ids, GAME, json!({"round": gm.view(t), "back": cx.uid()}));
    } else {
        out.to(cx.conn, GAME, json!({"round": gm.view(t)}));
    }
}

/// This game's ops (valley.py:1429). Reached only past the lobby gate.
///
/// Every op first runs [`advance_into`], before even looking at `op`: a dropped
/// player's hold may have run out meanwhile, and the Python does this at the top
/// of `golf_op` whether the op is one it knows or not. An op it does not know
/// then produces NOTHING else, as the `else`-less if/elif chain does.
pub fn op(v: &mut RoomValley, cx: &mut Ctx, op: &str, msg: &Value, out: &mut Out) {
    // Two disjoint field borrows of the same RoomValley, which is how an engine
    // reaches its lobby and its state at once.
    let gm = &mut v.golf;
    let lobby = &mut v.lobbies[I_GOLF];
    let t = cx.t;
    let conn = cx.conn;
    let uid = cx.uid().to_string();
    // `ids = list(lobby.members)` is read ONCE, at the top, and every event in
    // this dispatch goes to that same list.
    let ids = lobby.ids();
    advance_into(gm, &ids, out, t, 0);
    match op {
        "view" => out.to(conn, GAME, json!({"round": gm.view(t)})),
        "pos" => {
            if let Some(rel) = gm.pos(&uid, msg, t) {
                out.lobby_skip(ids.clone(), &uid, "pos", rel);
            }
        }
        "char" => match gm.char(&uid, msg) {
            None => out.err(conn, "pick a character"),
            Some(c) => out.lobby(ids.clone(), "char", json!({"user": uid, "c": c})),
        },
        "shot" => match gm.shot(&uid, msg, t) {
            Err(e) => out.err(conn, e),
            Ok(ev) => {
                let ticks = ev["ticks"].as_i64().unwrap_or(0);
                out.lobby(ids.clone(), "shot", ev);
                advance_into(gm, &ids, out, t, ticks);
            }
        },
        "concede" => {
            // Python's `gm.pick_up([uid])`: a one-element list.
            if gm.pick_up(std::slice::from_ref(&uid)).is_empty() {
                out.err(conn, "nothing to pick up");
            } else {
                out.lobby(ids.clone(), GAME, json!({"round": gm.view(t)}));
                advance_into(gm, &ids, out, t, 0);
            }
        }
        "start" | "skip" | "end" => {
            if lobby.host.as_deref() != Some(uid.as_str()) {
                out.err(conn, "only the host can do that");
                return;
            }
            // the new host acted: a returning old host stays a player
            lobby.prev_host = None;
            match op {
                "start" => {
                    let err = gm.start(
                        &lobby.members,
                        msg.get("course"),
                        t,
                        msg.get("holes"),
                        &mut *cx.rng,
                    );
                    match err {
                        Some(e) => out.err(conn, e),
                        None => out.lobby(
                            ids.clone(),
                            GAME,
                            json!({"round": gm.view(t), "by": cx.me()}),
                        ),
                    }
                }
                "skip" => {
                    gm.release(); // nobody parked holds the hole open any more
                    let all: Vec<String> = gm.players.keys().map(str::to_string).collect();
                    gm.pick_up(&all);
                    out.lobby(ids.clone(), GAME, json!({"round": gm.view(t)}));
                    advance_into(gm, &ids, out, t, 0);
                }
                _ => {
                    gm.end();
                    out.lobby(ids.clone(), GAME, json!({"round": gm.view(t)}));
                }
            }
        }
        _ => {}
    }
}

/// Someone left this game's lobby, by `leave` or by a dropped socket
/// (valley.py:1562). The `left` field here is the USER ID, not the profile the
/// shared `lobby` event carries. An explicit leave advances the hole at once; a
/// socket drop does not, because the hole waits [`GRACE`] for them and the
/// shared layer arms [`super::After::GolfRecheck`] off [`grace_left`].
pub fn dropped(
    v: &mut RoomValley, uid: &str, who: &Value, out: &mut Out, t: f64, disconnected: bool,
) -> Left {
    let _ = who; // golf's own event carries the id; `who` is the shared event's
    let gm = &mut v.golf;
    if gm.drop(uid, t, disconnected) {
        let ids = v.lobbies[I_GOLF].ids();
        out.lobby(ids.clone(), GAME, json!({"round": gm.view(t), "left": uid}));
        if !disconnected {
            advance_into(gm, &ids, out, t, 0);
        }
    }
    Left::Notice
}

/// The longest outstanding grace hold, or None. Python's `Golf.grace_left(t)`,
/// asked for by name at valley.py:1585 and :1518.
pub fn grace_left(v: &RoomValley, t: f64) -> Option<f64> {
    v.golf.grace_left(t)
}

/// The body of Python's `golf_recheck` that runs under the lock
/// (valley.py:1517-1518): advance the round into `out` for the golf lobby, then
/// report the grace still outstanding so the shared layer can re-arm.
pub fn recheck_step(v: &mut RoomValley, out: &mut Out, t: f64) -> Option<f64> {
    let gm = &mut v.golf;
    let ids = v.lobbies[I_GOLF].ids();
    advance_into(gm, &ids, out, t, 0);
    gm.grace_left(t)
}

#[cfg(test)]
mod tests {
    use super::super::testkit::*;
    use super::*;
    use rand::SeedableRng;
    use serde_json::Map;

    /// A JSON object with these keys IN THIS ORDER, for the tests that pin a
    /// card or a totals table -- `serde_json`'s `preserve_order` means the
    /// comparison would fail if the engine emitted them in another order, which
    /// is the point.
    fn obj_of(pairs: &[(&str, Value)]) -> Value {
        let mut m = Map::new();
        for (k, v) in pairs {
            m.insert((*k).to_string(), v.clone());
        }
        Value::Object(m)
    }

    fn dice(seed: u64) -> StdRng {
        StdRng::seed_from_u64(seed)
    }

    /// `golf.course_holes(cid)[i]`, the way every physics test reaches a hole.
    fn hole(cid: &str, i: usize) -> &'static Hole {
        &course_holes(cid).expect("a real course")[i]
    }

    /// test_golf.py's `_lane`: start, the given middle tiles (straight lanes
    /// heading -z), a cup. `extra` is merged into the hole spec, so a test can
    /// add zones, bumpers or movers.
    fn lane(mid: &[&str], extra: Value) -> Hole {
        let mut tiles = vec![json!(["start", 0, 0, 0])];
        for (i, name) in mid.iter().enumerate() {
            tiles.push(json!([name, 0, -(i as i64) - 1, 0]));
        }
        tiles.push(json!(["hole-round", 0, -(mid.len() as i64) - 1, 2]));
        let mut spec = json!({"name": "t", "par": 3, "tiles": tiles});
        if let Value::Object(e) = extra {
            spec.as_object_mut().unwrap().extend(e);
        }
        compile_hole(&spec)
    }

    fn members(n: usize) -> Vec<(String, Value)> {
        (0..n).map(|i| (format!("u{i}"), json!({"userId": format!("u{i}"), "handle": format!("h{i}")}))).collect()
    }

    /// test_golf.py's `_engine`: a started round with n players on `course`.
    fn engine(n: usize, course: &str) -> Golf {
        let mut gm = Golf::default();
        assert_eq!(gm.start(&members(n), Some(&json!(course)), 0.0, None, &mut dice(1)), None);
        gm
    }

    // ------------------------------------------------------------ physics --

    /// THE port's proof: `backend/tests/golf_golden.json`, the same 350 vectors
    /// `test_golden_vectors_match` feeds the Python, read at run time from the
    /// repo rather than copied into this file so the two cannot drift.
    #[test]
    fn the_golden_vectors_roll_exactly_as_python_does() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/golf_golden.json");
        let raw = std::fs::read_to_string(path).expect("golf_golden.json is in backend/tests");
        let vecs: Vec<Value> = serde_json::from_str(&raw).unwrap();
        assert!(vecs.len() >= 150, "the Python asserts at least 150 vectors");
        for v in &vecs {
            let h = hole(v["course"].as_str().unwrap(), v["hole"].as_u64().unwrap() as usize);
            let r = simulate(
                h,
                v["from"][0].as_i64().unwrap(),
                v["from"][1].as_i64().unwrap(),
                v["ax"].as_i64().unwrap(),
                v["az"].as_i64().unwrap(),
                v["power"].as_i64().unwrap(),
                v["clk"].as_i64().unwrap(),
            );
            let got = json!({"end": [r.end.0, r.end.1], "holed": r.holed, "oob": r.oob,
                             "water": r.water, "ticks": r.ticks});
            let want = json!({"end": v["end"], "holed": v["holed"], "oob": v["oob"],
                              "water": v["water"], "ticks": v["ticks"]});
            assert_eq!(got, want, "{v}");
        }
    }

    #[test]
    fn tdiv_truncates_toward_zero_and_isqrt_floors() {
        assert_eq!((tdiv(7, 2), tdiv(-7, 2), tdiv(7, -2), tdiv(-7, -2)), (3, -3, -3, 3));
        let got: Vec<i64> = [0, 1, 3, 4, 99, 100, 1_000_000_000_001].iter().map(|n| isqrt(*n)).collect();
        assert_eq!(got, [0, 1, 1, 2, 9, 10, 1_000_000]);
        // The trap this function exists for: Python's `//` would floor to -4.
        assert_eq!(tdiv(-7, 2), -3);
        assert_eq!(floordiv(-7, 2), -4);
    }

    #[test]
    fn cell_of_floors_so_the_minus_x_half_of_a_hole_stays_on_the_course() {
        assert_eq!(cell_of(0), 0);
        assert_eq!(cell_of(4999), 0);
        assert_eq!(cell_of(5000), 1);
        assert_eq!(cell_of(-5000), 0);
        assert_eq!(cell_of(-5001), -1); // a truncating divide would say 0
        assert_eq!(cell_of(-15001), -2);
    }

    #[test]
    fn rot_is_a_quarter_turn_about_the_tile_centre() {
        assert_eq!(rot(100, 200, 0), (100, 200));
        assert_eq!(rot(100, 200, 1), (200, -100));
        assert_eq!(rot(100, 200, 2), (-100, -200));
        assert_eq!(rot(100, 200, 3), (-200, 100));
        assert_eq!(rot(100, 200, 4), (100, 200)); // k & 3
        assert_eq!(rot(100, 200, -1), (-200, 100)); // -1 & 3 == 3, as in Python
    }

    #[test]
    fn a_shot_is_deterministic_and_integral() {
        let h = hole("windmill", 3);
        let a = simulate(h, h.tee.0, h.tee.1, 1234, -3000, 77, 0);
        let b = simulate(h, h.tee.0, h.tee.1, 1234, -3000, 77, 0);
        assert_eq!(a, b);
    }

    #[test]
    fn full_power_rolls_about_seven_tiles() {
        let mut tiles = vec![json!(["start", 0, 0, 0])];
        for i in 1..12 {
            tiles.push(json!(["straight", 0, -i, 0]));
        }
        tiles.push(json!(["hole-round", 0, -12, 2]));
        let h = compile_hole(&json!({"name": "t", "par": 2, "tiles": tiles}));
        let r = simulate(&h, h.tee.0, h.tee.1, 0, -4096, 100, 0);
        let dist = (h.tee.1 - r.end.1) as f64 / TILE as f64;
        assert!(6.5 < dist && dist < 8.0, "{dist}");
        assert!(!r.oob);
    }

    #[test]
    fn every_hole_compiles() {
        assert!(COURSES.len() >= 3);
        for (ci, c) in COURSES.iter().enumerate() {
            assert!(c.holes.len() >= 3);
            let specs = DATA["courses"][ci]["holes"].as_array().unwrap();
            for (spec, h) in specs.iter().zip(&c.holes) {
                let names: Vec<&str> =
                    spec["tiles"].as_array().unwrap().iter().map(|t| t[0].as_str().unwrap()).collect();
                assert_eq!(names.iter().filter(|n| **n == "start").count(), 1, "{}", h.name);
                assert_eq!(
                    names.iter().filter(|n| n.starts_with("hole-")).count(),
                    1,
                    "{}",
                    h.name
                );
                assert!(on_floor(h, h.tee.0, h.tee.1), "{}", h.name);
                assert!(on_floor(h, h.cup.0, h.cup.1), "{}", h.name);
                for sg in &h.segs {
                    assert!(sg.dx * sg.dx + sg.dz * sg.dz <= 15000 * 15000);
                }
                assert!((2..=5).contains(&h.par), "{}", h.name);
                let cells: HashSet<(i64, i64)> = spec["tiles"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|t| (t[1].as_i64().unwrap(), t[2].as_i64().unwrap()))
                    .collect();
                assert_eq!(cells.len(), spec["tiles"].as_array().unwrap().len(), "overlapping tiles");
            }
        }
    }

    #[test]
    fn five_courses_with_themes() {
        let ids: Vec<&str> = COURSES.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, ["meadow", "windmill", "keep", "desert", "snow"]);
        let sizes: Vec<usize> = COURSES.iter().map(|c| c.holes.len()).collect();
        assert_eq!(sizes, [4, 5, 6, 5, 5]);
        for c in DATA["courses"].as_array().unwrap() {
            assert!(c["theme"]["props"].is_array());
            assert_eq!(c["theme"]["sky"].as_array().unwrap().len(), 2);
        }
    }

    #[test]
    fn every_mover_period_divides_the_clock() {
        for c in COURSES.iter() {
            for h in &c.holes {
                for m in &h.movers {
                    if let Mover::Slider { travel, period, .. } = *m {
                        assert_eq!(CLOCK % period, 0, "{} {}", c.id, h.name);
                        assert_eq!(period % 2, 0, "{} {}", c.id, h.name);
                        assert!(
                            2 * travel / (period / 2) < R,
                            "a gate must move slower than a ball radius per tick"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn sand_slows_and_ice_speeds_the_roll() {
        let zones = |kind: &str| {
            Value::Array(
                (1..7).map(|i| json!([kind, 0, -i, -4000, -5000, 4000, 5000])).collect(),
            )
        };
        let straights = ["straight"; 6];
        let plain = lane(&straights, json!({}));
        let sand = lane(&straights, json!({"zones": zones("sand")}));
        let ice = lane(&straights, json!({"zones": zones("ice")}));
        let d: Vec<f64> = [&sand, &plain, &ice]
            .iter()
            .map(|h| (h.tee.1 - simulate(h, h.tee.0, h.tee.1, 0, -4096, 30, 0).end.1) as f64)
            .collect();
        assert!(0.0 < d[0] && d[0] < d[1] / 2.0, "{d:?}");
        assert!(d[2] > d[1] * 1.5, "{d:?}");
    }

    #[test]
    fn water_is_a_hazard_back_to_the_lie() {
        let h = lane(
            &["straight", "straight", "straight"],
            json!({"zones": [["water", 0, -2, -4000, -3000, 4000, 3000]]}),
        );
        let r = simulate(&h, h.tee.0, h.tee.1, 0, -4096, 50, 0);
        assert!(r.oob && r.water && r.end == h.tee);
        // ... and the referee charges the penalty stroke for it (Oasis: the pond
        // sits right in the line).
        let mut gm = Golf::default();
        gm.start(&members(1), Some(&json!("desert")), 0.0, None, &mut dice(1));
        gm.hole = 1;
        let h = hole("desert", 1);
        for p in gm.players.values_mut() {
            p.ball = h.tee;
        }
        let ev = gm
            .shot("u0", &json!({"ax": 0, "az": -4096, "power": 55, "clk": 5}), 1.0)
            .expect("a legal shot");
        assert_eq!(ev["water"], json!(true));
        assert_eq!(ev["strokes"], json!(2)); // 1 + OOB_PENALTY
        assert_eq!(ev["end"], json!([h.tee.0, h.tee.1]));
        assert_eq!(ev["clk"], json!(5));
    }

    #[test]
    fn a_bumper_kicks_harder_than_a_wall() {
        let h = lane(
            &["straight", "straight", "straight"],
            json!({"bumpers": [[0, -2, 0, 0, 900]]}),
        );
        let r = simulate(&h, h.tee.0, h.tee.1, 0, -4096, 45, 0);
        assert!(!r.holed);
        assert!(r.end.1 > -2 * TILE, "bounced back toward the tee");
        let wall = lane(&["straight", "straight", "straight"], json!({}));
        assert_ne!(simulate(&wall, wall.tee.0, wall.tee.1, 0, -4096, 45, 0).end, r.end);
    }

    #[test]
    fn a_hill_rolls_a_weak_putt_back() {
        let h = lane(&["straight", "hill-round", "straight"], json!({}));
        let weak = simulate(&h, h.tee.0, h.tee.1, 0, -4096, 12, 0);
        assert!(weak.end.1 > -2 * TILE + 3000, "never got over the crest");
        let strong = simulate(&h, h.tee.0, h.tee.1, 0, -4096, 60, 0);
        assert!(strong.end.1 < -2 * TILE, "rolled over it");
        assert!(slope_at(&h, 0, -2 * TILE + 1000).is_some());
    }

    #[test]
    fn windmill_blades_block_by_the_clock() {
        let h = hole("windmill", 0); // start, straight, windmill, straight, cup
        let mut outs = HashSet::new();
        for clk in (0..576).step_by(8) {
            let r = simulate(h, h.tee.0, h.tee.1, 0, -4096, 70, clk);
            outs.insert(r.holed || r.end.1 < -2 * TILE);
        }
        assert_eq!(outs.len(), 2, "some phases get through the door and some are blocked");
        for clk in [0, 100, 2879] {
            // the same phase always rolls the same
            assert_eq!(
                simulate(h, h.tee.0, h.tee.1, 0, -4096, 70, clk),
                simulate(h, h.tee.0, h.tee.1, 0, -4096, 70, clk + CLOCK)
            );
        }
        assert!(blades_down(0));
        assert!(blades_down(BLADE_GAP));
        assert!(!blades_down(BLADE_GAP / 2));
    }

    #[test]
    fn a_slider_moves_with_the_clock() {
        let h = hole("desert", 2); // Canyon Gates
        let m = h.movers.iter().find(|m| matches!(m, Mover::Slider { .. })).unwrap();
        let Mover::Slider { travel, period, .. } = *m else { unreachable!() };
        let offs: HashSet<i64> = (0..period).map(|ph| slide_off(m, ph)).collect();
        assert_eq!(offs.iter().min(), Some(&-travel));
        assert_eq!(offs.iter().max(), Some(&travel));
        let ends: HashSet<(i64, i64)> = (0..720)
            .step_by(24)
            .map(|clk| simulate(h, h.tee.0, h.tee.1, 0, -4096, 55, clk).end)
            .collect();
        assert!(ends.len() > 1);
    }

    #[test]
    fn a_gap_is_out_of_bounds_and_returns_to_the_lie() {
        let h = hole("keep", 0); // Moat: start, straight, gap, straight, cup
        for pw in 1..=100 {
            let r = simulate(h, h.tee.0, h.tee.1, 0, -4096, pw, 0);
            if r.oob {
                assert_eq!(r.end, h.tee);
                return;
            }
        }
        panic!("no power drops the ball into the moat");
    }

    #[test]
    fn backwards_off_the_tee_ramp_is_out_of_bounds() {
        let h = hole("meadow", 0);
        let r = simulate(h, h.tee.0, h.tee.1, 0, 4096, 60, 0);
        assert!(r.oob);
        assert_eq!(r.end, h.tee);
    }

    // ------------------------------------------------------------- engine --

    #[test]
    fn max_strokes_a_pickup_and_the_card() {
        let mut gm = Golf::default();
        assert_eq!(gm.start(&members(2), Some(&json!("meadow")), 0.0, None, &mut dice(1)), None);
        let mut t = 0.0;
        let mut ev = Value::Null;
        for _ in 0..4 {
            // backwards off the ramp: 2 strokes each
            t += 30.0;
            ev = gm.shot("u0", &json!({"ax": 0, "az": 4096, "power": 60}), t).expect("legal");
            assert_eq!(ev["oob"], json!(true));
        }
        assert_eq!(ev["strokes"], json!(MAX_STROKES));
        assert_eq!(ev["done"], json!(true));
        assert_eq!(
            gm.shot("u0", &json!({"ax": 0, "az": -4096, "power": 50}), t + 30.0),
            Err("you've finished this hole")
        );
        assert!(gm.advance(t, 0).is_none(), "u1 still playing");
        assert_eq!(gm.pick_up(&["u1".into()]), ["u1"]);
        let (kind, data) = gm.advance(t, 120).expect("the hole moves on");
        assert_eq!(kind, "hole");
        assert_eq!(data["hole"], json!(1));
        assert_eq!(data["card"]["u0"][0], json!(MAX_STROKES));
        assert!((gm.hole_ready_at - (t + 1.0 + HOLE_PAUSE)).abs() < 1e-9);
        assert_eq!(
            gm.shot("u0", &json!({"ax": 0, "az": -4096, "power": 50}), t + 1.0),
            Err("wait for the ball to stop")
        );
    }

    #[test]
    fn the_engine_rejects_bad_shots_with_pythons_words() {
        let mut gm = Golf::default();
        assert_eq!(
            gm.shot("u0", &json!({"ax": 1, "az": 0, "power": 5}), 0.0),
            Err("no round is being played")
        );
        gm.start(&members(1), Some(&json!("meadow")), 0.0, None, &mut dice(1));
        assert_eq!(
            gm.shot("x", &json!({"ax": 1, "az": 0, "power": 5}), 1.0),
            Err("you're not in this round")
        );
        for bad in [
            json!({"ax": 0, "az": 0, "power": 5}),
            json!({"ax": "1", "az": 0, "power": 5}),
            json!({"ax": 1, "az": 0}),
            json!({"ax": true, "az": 0, "power": 5}),
            // Python's `v != v` NaN test; JSON cannot carry a NaN, and
            // serde_json decodes neither, so a null stands in for the same
            // "not a number" rejection.
            json!({"ax": 1, "az": 0, "power": null}),
        ] {
            assert_eq!(gm.shot("u0", &bad, 1.0), Err("bad shot"), "{bad}");
        }
        let ev = gm
            .shot("u0", &json!({"ax": 99999, "az": -99999, "power": 500}), 1.0)
            .expect("clamped, not refused");
        assert_eq!((&ev["ax"], &ev["az"], &ev["power"]), (&json!(4096), &json!(-4096), &json!(100)));
        assert_eq!(
            gm.shot("u0", &json!({"ax": 1, "az": 0, "power": 5}), 1.01),
            Err("wait for the ball to stop")
        );
    }

    #[test]
    fn a_shot_is_refused_while_busy_after_done_and_for_the_wrong_hole() {
        let mut gm = engine(1, "meadow");
        let h = gm.holes()[0];
        assert_eq!(
            gm.shot("u0", &json!({"ax": 0, "az": -4096, "power": 20, "hole": 1}), 1.0),
            Err("that hole is over")
        );
        let ev = gm
            .shot("u0", &json!({"ax": 0, "az": -4096, "power": 20, "hole": 0, "seq": 3}), 1.0)
            .expect("legal");
        assert_eq!(ev["seq"], json!(3));
        assert_eq!(ev["from"], json!([h.tee.0, h.tee.1]));
        assert_eq!(
            gm.shot("u0", &json!({"ax": 0, "az": -4096, "power": 20}), 1.0),
            Err("wait for the ball to stop")
        );
        gm.pick_up(&["u0".into()]);
        assert_eq!(
            gm.shot("u0", &json!({"ax": 0, "az": -4096, "power": 20}), 99.0),
            Err("you've finished this hole")
        );
    }

    #[test]
    fn an_explicit_null_hole_is_no_hole_at_all() {
        // Python's `msg.get("hole") is not None` skips the check for a JSON null,
        // where a naive port would read it as "hole 0" -- harmless here, but as
        // "not hole 1" after the hole moved on, which would wrongly refuse.
        let mut gm = engine(1, "meadow");
        gm.pick_up(&["u0".into()]);
        gm.advance(1.0, 0);
        assert_eq!(gm.hole, 1);
        gm.hole_ready_at = 0.0;
        assert!(gm
            .shot("u0", &json!({"ax": 0, "az": -4096, "power": 20, "hole": null}), 99.0)
            .is_ok());
    }

    #[test]
    fn the_pos_token_bucket_bursts_three_then_refills_at_twelve_per_second() {
        let mut gm = engine(2, "meadow");
        let msg = json!({"x": 0, "z": 0, "r": 0, "a": 0});
        let got: Vec<bool> = (0..5).map(|_| gm.pos("u0", &msg, 10.0).is_some()).collect();
        assert_eq!(got, [true, true, true, false, false]);
        // one second of 10 Hz sending after an empty bucket: every frame passes
        let passed = (1..=10).filter(|i| gm.pos("u0", &msg, 10.0 + 0.1 * *i as f64).is_some()).count();
        assert_eq!(passed, 10);
        // sustained 30 Hz spam is held to ~12/s
        let t0 = 20.0;
        let passed =
            (0..90).filter(|i| gm.pos("u0", &msg, t0 + *i as f64 / 30.0).is_some()).count();
        assert!((36..=40).contains(&passed), "{passed}");
        // buckets are per player
        assert!(gm.pos("u1", &msg, t0 + 3.0).is_some());
    }

    #[test]
    fn pos_relays_the_sender_sequence_and_ignores_strangers() {
        let mut gm = engine(2, "meadow");
        assert_eq!(
            gm.pos("u0", &json!({"x": 1, "z": 2, "r": -90, "a": 9, "q": 41}), 1.0),
            Some(json!({"u": "u0", "x": 1, "z": 2, "r": 270, "a": 7, "q": 41}))
        );
        let relay = gm.pos("u0", &json!({"x": 1, "z": 2, "r": 0, "a": 0}), 2.0).unwrap();
        assert!(relay.get("q").is_none(), "no q in, no q out");
        assert_eq!(gm.pos("zz", &json!({"x": 1, "z": 2, "r": 0, "a": 0}), 3.0), None);
        assert_eq!(gm.pos("u0", &json!({"x": "1", "z": 2, "r": 0, "a": 0}), 4.0), None);
        // A q of zero still goes out: Python tests `is not None`, not truthiness.
        let z = gm.pos("u0", &json!({"x": 1, "z": 2, "r": 0, "a": 0, "q": 0}), 5.0).unwrap();
        assert_eq!(z["q"], json!(0));
    }

    #[test]
    fn a_pos_is_clamped_to_two_tiles_outside_the_hole() {
        let mut gm = engine(1, "windmill");
        let (_, _, x1, _) = gm.holes()[0].bbox;
        let rel = gm.pos("u0", &json!({"x": 1_000_000_000i64, "z": -5, "r": 725, "a": 1, "q": 1}), 1.0);
        assert_eq!(
            rel,
            Some(json!({"u": "u0", "x": x1 + 2 * TILE, "z": -5, "r": 5, "a": 1, "q": 1}))
        );
    }

    #[test]
    fn a_dropped_player_is_parked_and_restored_on_rejoin() {
        let mut gm = engine(2, "meadow");
        gm.shot("u1", &json!({"ax": 0, "az": -4096, "power": 20}), 1.0).expect("legal");
        let ball = gm.players.get("u1").unwrap().ball;
        assert!(gm.drop("u1", 5.0, false));
        assert!(!gm.players.contains_key("u1"));
        assert_eq!(gm.phase, Phase::Playing);
        // same hole: strokes and lie come back
        assert!(gm.restore("u1", json!({"userId": "u1", "handle": "h1"}), 10.0));
        assert_eq!(gm.players.get("u1").unwrap().strokes[0], 1);
        assert_eq!(gm.players.get("u1").unwrap().ball, ball);
        // the hole moved on while they were away: that hole counts as picked up,
        // they start at the next tee
        gm.drop("u1", 20.0, false);
        gm.pick_up(&["u0".into()]);
        assert!(gm.advance(21.0, 0).is_some());
        assert_eq!(gm.hole, 1);
        assert!(gm.restore("u1", json!({"userId": "u1", "handle": "h1"}), 22.0));
        let p = gm.players.get("u1").unwrap();
        assert_eq!(p.strokes[0], MAX_STROKES);
        assert_eq!(p.ball, gm.holes()[1].tee);
        assert!(!p.done);
        // too late, or a new round: no restore
        gm.drop("u1", 30.0, false);
        assert!(!gm.restore("u1", json!({"userId": "u1"}), 30.0 + PARK_SECS + 1.0));
        gm.drop("u0", 40.0, false);
        assert_eq!(gm.phase, Phase::Idle);
        assert!(gm.parked.is_empty());
    }

    #[test]
    fn a_refused_restore_still_forgets_the_parked_card() {
        // Python pops the park entry BEFORE every test, so a card refused once
        // can never come back. A port that checked first would resurrect it.
        let mut gm = engine(2, "meadow");
        gm.drop("u1", 1.0, false);
        assert!(!gm.restore("u1", json!({"userId": "u1"}), 1.0 + PARK_SECS + 1.0));
        assert!(!gm.restore("u1", json!({"userId": "u1"}), 2.0));
    }

    #[test]
    fn a_socket_drop_holds_the_hole_for_the_grace_period() {
        let mut gm = engine(2, "meadow");
        gm.pick_up(&["u0".into()]); // u0 finished the hole
        gm.shot("u1", &json!({"ax": 0, "az": -4096, "power": 20}), 1.0).expect("legal");
        assert!(gm.drop("u1", 5.0, true));
        assert!(gm.advance(6.0, 0).is_none(), "u1 may still come back");
        assert_eq!(gm.hole, 0);
        assert!((gm.grace_left(6.0).unwrap() - (GRACE - 1.0)).abs() < 1e-9);
        assert!(gm.restore("u1", json!({"userId": "u1", "handle": "h1"}), 7.0));
        assert_eq!(gm.players.get("u1").unwrap().strokes[0], 1);
        assert_eq!(gm.hole, 0);
        // a second drop that never comes back: the hole moves on once the grace
        // runs out
        gm.drop("u1", 10.0, true);
        assert!(gm.advance(10.0 + GRACE - 0.1, 0).is_none());
        assert_eq!(gm.advance(10.0 + GRACE + 0.1, 0).unwrap().0, "hole");
        assert_eq!(gm.hole, 1);
        // an explicit leave never holds the hole; neither does a host skip
        let mut gm2 = engine(2, "meadow");
        gm2.pick_up(&["u0".into()]);
        gm2.drop("u1", 5.0, false);
        assert!(gm2.advance(5.0, 0).is_some());
        let mut gm3 = engine(2, "meadow");
        gm3.pick_up(&["u0".into()]);
        gm3.drop("u1", 5.0, true);
        gm3.release();
        assert!(gm3.advance(5.0, 0).is_some());
    }

    #[test]
    fn a_lone_players_round_survives_a_blip_and_ends_if_they_never_return() {
        let mut gm = engine(1, "meadow");
        gm.drop("u0", 5.0, true);
        assert_eq!(gm.phase, Phase::Playing);
        assert!(gm.restore("u0", json!({"userId": "u0"}), 6.0));
        assert_eq!(gm.phase, Phase::Playing);
        gm.drop("u0", 10.0, true);
        assert!(gm.advance(10.0 + GRACE + 0.1, 0).is_none());
        assert_eq!(gm.phase, Phase::Idle);
        assert!(gm.parked.is_empty());
    }

    #[test]
    fn a_round_already_on_refuses_a_second_start() {
        let mut gm = engine(1, "meadow");
        assert_eq!(
            gm.start(&members(1), Some(&json!("keep")), 1.0, None, &mut dice(1)),
            Some("a round is already on: the host can end it first")
        );
        gm.end();
        assert_eq!(gm.start(&members(1), Some(&json!("keep")), 1.0, None, &mut dice(1)), None);
    }

    #[test]
    fn a_start_refuses_a_course_nobody_has() {
        let mut gm = Golf::default();
        for bad in [json!("nope"), json!(null), json!(7), json!(true), json!(["meadow"])] {
            assert_eq!(
                gm.start(&members(1), Some(&bad), 0.0, None, &mut dice(1)),
                Some("pick a course"),
                "{bad}"
            );
        }
        // a missing `course` key at all
        assert_eq!(gm.start(&members(1), None, 0.0, None, &mut dice(1)), Some("pick a course"));
        assert_eq!(gm.phase, Phase::Idle);
        assert!(gm.view(0.0).is_null());
    }

    #[test]
    fn a_round_seats_only_the_first_eight_of_the_lobby() {
        let mut gm = Golf::default();
        gm.start(&members(11), Some(&json!("meadow")), 0.0, None, &mut dice(1));
        let seated: Vec<&str> = gm.players.keys().collect();
        assert_eq!(seated, ["u0", "u1", "u2", "u3", "u4", "u5", "u6", "u7"]);
        // c cycles over CHARS and color over COLORS, by lobby position
        assert_eq!(gm.players.get("u6").unwrap().c, 0); // 6 % 6
        assert_eq!(gm.players.get("u7").unwrap().color, 7);
    }

    #[test]
    fn a_chosen_character_sticks_to_the_player_and_to_the_room() {
        let mut gm = engine(2, "meadow");
        assert_eq!(gm.char("u0", &json!({"c": 4})), Some(4));
        assert_eq!(gm.players.get("u0").unwrap().c, 4);
        assert_eq!(gm.char("u0", &json!({"c": 99})), Some(CHARS - 1)); // clamped
        assert_eq!(gm.char("u0", &json!({"c": "x"})), None);
        assert_eq!(gm.char("nobody", &json!({"c": 2})), Some(2)); // kept for a later round
        gm.end();
        gm.start(&members(2), Some(&json!("meadow")), 1.0, None, &mut dice(1));
        assert_eq!(gm.players.get("u0").unwrap().c, CHARS - 1);
        assert_eq!(gm.view(1.0)["chars"]["u0"], json!(CHARS - 1));
    }

    #[test]
    fn the_view_counts_down_to_the_next_tee_and_never_below_zero() {
        let mut gm = engine(1, "meadow");
        gm.pick_up(&["u0".into()]);
        gm.advance(10.0, 120);
        assert_eq!(gm.view(10.0)["readyInMs"], json!(5000)); // 1 s of roll + 4 s pause
        assert_eq!(gm.view(100.0)["readyInMs"], json!(0));
    }

    #[test]
    fn the_done_payload_ranks_every_card() {
        // meadow is four holes: u0 takes 2 a hole, u1 picks every one up.
        let mut gm = engine(2, "meadow");
        let n = gm.holes().len();
        let mut last = None;
        for i in 0..n {
            gm.pick_up(&["u1".into()]);
            let p = gm.players.get_mut("u0").unwrap();
            p.strokes[i] = 2;
            p.done = true;
            last = gm.advance(100.0 * (i + 1) as f64, 0);
            assert_eq!(last.as_ref().unwrap().0, if i + 1 < n { "hole" } else { "done" });
        }
        let data = last.unwrap().1;
        assert_eq!(data["card"], obj_of(&[("u0", json!([2, 2, 2, 2])),
                                          ("u1", json!(vec![MAX_STROKES; 4]))]));
        assert_eq!(data["totals"], obj_of(&[("u0", json!(8)), ("u1", json!(32))]));
        // One winner, and the key order is the player order both times.
        assert_eq!(data["winners"], json!(["u0"]));
        assert_eq!(data["course"], json!("meadow"));
        assert!(data.get("mix").is_none(), "a fixed-course round carries no mix");
        assert_eq!(gm.phase, Phase::Done);
    }

    // -------------------------------------------------------- play random --

    #[test]
    fn a_random_round_draws_distinct_holes_from_every_course() {
        assert_eq!(ALL_HOLES.len(), COURSES.iter().map(|c| c.holes.len()).sum::<usize>());
        for n in RANDOM_SIZES {
            let mut gm = Golf::default();
            assert_eq!(
                gm.start(&members(1), Some(&json!(RANDOM)), 0.0, Some(&json!(n)), &mut dice(n as u64)),
                None
            );
            let mix = gm.mix.clone().unwrap();
            let want = (n as usize).min(ALL_HOLES.len());
            assert_eq!(mix.len(), want);
            assert_eq!(gm.holes().len(), want);
            assert_eq!(gm.players.get("u0").unwrap().strokes.len(), want);
            let distinct: HashSet<&(String, usize)> = mix.iter().collect();
            assert_eq!(distinct.len(), mix.len());
            assert!(mix.iter().all(|m| ALL_HOLES.contains(m)));
            // every drawn hole IS that course's compiled hole, not a copy
            for ((cid, i), h) in mix.iter().zip(gm.holes()) {
                assert!(std::ptr::eq(h, &course_holes(cid).unwrap()[*i]));
            }
            let v = gm.view(0.0);
            assert_eq!(v["course"], json!(RANDOM));
            assert_eq!(v["mix"], mix_value(&mix));
            let pars: Vec<i64> =
                mix.iter().map(|(c, i)| course_holes(c).unwrap()[*i].par).collect();
            assert_eq!(v["par"], json!(pars));
            assert_eq!(gm.players.get("u0").unwrap().ball, gm.holes()[0].tee);
        }
        // 15 holes over enough rounds touch more than one course
        let seen: HashSet<String> =
            (0..5).flat_map(|s| pick_mix(15, &mut dice(s)).into_iter().map(|(c, _)| c)).collect();
        assert!(seen.len() > 1);
        // a fixed-course round carries no mix
        let gm = engine(2, "meadow");
        assert!(gm.mix.is_none());
        assert!(gm.view(0.0).get("mix").is_none());
    }

    #[test]
    fn a_random_round_caps_at_every_hole_there_is() {
        // Python monkeypatches ALL_HOLES to 7 entries; a Rust static cannot be
        // patched, so this drives pick_mix's own `min(n, len)` instead -- which
        // is the line the Python test is about.
        let drawn = pick_mix(999, &mut dice(1));
        assert_eq!(drawn.len(), ALL_HOLES.len());
        let distinct: HashSet<&(String, usize)> = drawn.iter().collect();
        assert_eq!(distinct.len(), drawn.len());
    }

    #[test]
    fn a_random_round_refuses_bad_hole_counts() {
        let mut gm = Golf::default();
        for bad in [
            Value::Null,
            json!(0),
            json!(3),
            json!(7),
            json!(20),
            json!(-5),
            json!(5.0), // a float is NOT an int: Python's isinstance test
            json!("5"),
            json!(true),
            json!([5]),
        ] {
            assert_eq!(
                gm.start(&members(1), Some(&json!(RANDOM)), 0.0, Some(&bad), &mut dice(1)),
                Some("pick 5, 10 or 15 holes"),
                "{bad}"
            );
            assert_eq!(gm.phase, Phase::Idle);
            assert!(gm.mix.is_none());
        }
        // and a missing `holes` key
        assert_eq!(
            gm.start(&members(1), Some(&json!(RANDOM)), 0.0, None, &mut dice(1)),
            Some("pick 5, 10 or 15 holes")
        );
    }

    #[test]
    fn a_random_round_plays_to_done_with_the_mix_on_the_card() {
        let mut gm = Golf::default();
        assert_eq!(
            gm.start(&members(2), Some(&json!(RANDOM)), 0.0, Some(&json!(5)), &mut dice(7)),
            None
        );
        let mix = gm.mix.clone().unwrap();
        let (mut t, mut step) = (1.0, None);
        for i in 0..5 {
            let h = gm.holes()[gm.hole];
            assert!(std::ptr::eq(h, &course_holes(&mix[i].0).unwrap()[mix[i].1]));
            // u0 putts once from the tee (rolled on that hole's own course),
            // then everybody picks up
            let ev = gm
                .shot("u0", &json!({"ax": 0, "az": -4096, "power": 30, "hole": i}), t + 10.0)
                .expect("legal");
            let ref_end = simulate(h, h.tee.0, h.tee.1, 0, -4096, 30, 0).end;
            assert_eq!(ev["end"], json!([ref_end.0, ref_end.1]));
            gm.pick_up(&["u0".into(), "u1".into()]);
            t += 60.0;
            step = gm.advance(t, 0);
            let (kind, _) = step.clone().expect("the round moves on");
            assert_eq!(kind, if i < 4 { "hole" } else { "done" });
            if i < 4 {
                assert_eq!(
                    gm.players.get("u0").unwrap().ball,
                    course_holes(&mix[i + 1].0).unwrap()[mix[i + 1].1].tee
                );
            }
        }
        let data = step.unwrap().1;
        assert_eq!(data["course"], json!(RANDOM));
        assert_eq!(data["mix"], mix_value(&mix));
        let pars: Vec<i64> = mix.iter().map(|(c, i)| course_holes(c).unwrap()[*i].par).collect();
        assert_eq!(data["par"], json!(pars));
        assert_eq!(data["card"]["u1"], json!(vec![MAX_STROKES; 5]));
        assert_eq!(gm.phase, Phase::Done);
    }

    #[test]
    fn a_random_round_restores_only_into_the_same_mix() {
        let mut gm = Golf::default();
        assert_eq!(
            gm.start(&members(2), Some(&json!(RANDOM)), 0.0, Some(&json!(5)), &mut dice(3)),
            None
        );
        gm.drop("u1", 1.0, false);
        assert!(gm.restore("u1", json!({"userId": "u1"}), 2.0));
        gm.drop("u1", 3.0, false);
        // parked from some other random round
        gm.parked.get_mut("u1").unwrap().mix = Some(vec![("meadow".into(), 0)]);
        assert!(!gm.restore("u1", json!({"userId": "u1"}), 4.0));
    }

    // ---------------------------------------------------- over the valley --

    const ROOM: &str = "lobby";

    #[tokio::test]
    async fn a_round_runs_from_join_to_done_over_the_hub() {
        let e = env(4, 1);
        let (a, mut ra) = e.connect(ROOM, 1, "a").await;
        let (b, mut rb) = e.connect(ROOM, 2, "b").await;
        e.send(ROOM, 1, &a, GAME, "join", json!({})).await;
        assert!(until(&mut ra, GAME, GAME).await["round"].is_null());
        e.send(ROOM, 2, &b, GAME, "join", json!({})).await;
        until(&mut rb, GAME, GAME).await;
        // only the host may start
        e.send(ROOM, 2, &b, GAME, "start", json!({"course": "windmill"})).await;
        assert_eq!(until(&mut rb, GAME, "error").await["error"], json!("only the host can do that"));
        e.send(ROOM, 1, &a, GAME, "start", json!({"course": "nope"})).await;
        assert_eq!(until(&mut ra, GAME, "error").await["error"], json!("pick a course"));
        e.send(ROOM, 2, &b, GAME, "char", json!({"c": 4})).await;
        assert_eq!(until(&mut ra, GAME, "char").await["c"], json!(4));
        e.send(ROOM, 1, &a, GAME, "start", json!({"course": "windmill"})).await;
        let rnd = until_where(&mut rb, GAME, GAME, |m| m["round"]["phase"] == "playing").await;
        let rnd = &rnd["round"];
        let seated: Vec<&str> =
            rnd["players"].as_array().unwrap().iter().map(|p| p["user"]["userId"].as_str().unwrap()).collect();
        assert_eq!(seated, ["a", "b"]);
        assert_eq!(rnd["players"][1]["c"], json!(4));
        assert_eq!(rnd["course"], json!("windmill"));
        assert_eq!(rnd["par"].as_array().unwrap().len(), 5);
        // a shot is rolled by the server and reaches the lobby
        e.send(ROOM, 1, &a, GAME, "shot", json!({"ax": 0, "az": -4096, "power": 40, "seq": 7})).await;
        let shot = until(&mut rb, GAME, "shot").await;
        let h = hole("windmill", 0);
        let r = simulate(h, h.tee.0, h.tee.1, 0, -4096, 40, 0);
        assert_eq!(shot["end"], json!([r.end.0, r.end.1]));
        assert_eq!(shot["ticks"], json!(r.ticks));
        assert_eq!(shot["seq"], json!(7));
        assert_eq!(shot["user"]["userId"], json!("a"));
        assert_eq!(shot["strokes"], json!(1));
        assert_eq!(shot["from"], json!([h.tee.0, h.tee.1]));
        until(&mut ra, GAME, "shot").await;
        // busy until the ball stops
        e.send(ROOM, 1, &a, GAME, "shot", json!({"ax": 0, "az": -4096, "power": 40})).await;
        assert_eq!(until(&mut ra, GAME, "error").await["error"], json!("wait for the ball to stop"));
        // the host skips every hole to the end
        let mut done = Value::Null;
        for i in 0..5 {
            e.advance(30.0);
            e.send(ROOM, 1, &a, GAME, "skip", json!({})).await;
            if i < 4 {
                let hl = until(&mut rb, GAME, "hole").await;
                assert_eq!(hl["hole"], json!(i + 1));
                assert!(hl["delayMs"].as_i64().unwrap() >= 4000);
            } else {
                done = until(&mut rb, GAME, "done").await;
            }
        }
        assert_eq!(
            done["totals"],
            obj_of(&[("a", json!(MAX_STROKES * 5)), ("b", json!(MAX_STROKES * 5))])
        );
        let mut winners: Vec<&str> =
            done["winners"].as_array().unwrap().iter().map(|w| w.as_str().unwrap()).collect();
        winners.sort();
        assert_eq!(winners, ["a", "b"]);
        assert_eq!(done["card"]["b"], json!(vec![MAX_STROKES; 5]));
        let pars: Vec<i64> = course_holes("windmill").unwrap().iter().map(|h| h.par).collect();
        assert_eq!(done["par"], json!(pars));
        until_where(&mut ra, GAME, GAME, |m| m["round"]["phase"] == "done").await;
        e.send(ROOM, 1, &a, GAME, "end", json!({})).await;
        until_where(&mut rb, GAME, GAME, |m| m["round"]["phase"] == "idle").await;
    }

    #[tokio::test]
    async fn a_golf_event_goes_only_to_the_lobby() {
        let e = env(4, 1);
        let (a, mut ra) = e.connect(ROOM, 1, "a").await;
        let (c, mut rc) = e.connect(ROOM, 2, "c").await; // in the room, never joined golf
        e.send(ROOM, 1, &a, GAME, "join", json!({})).await;
        until(&mut ra, GAME, GAME).await;
        e.send(ROOM, 1, &a, GAME, "start", json!({"course": "meadow"})).await;
        until_where(&mut ra, GAME, GAME, |m| m["round"]["phase"] == "playing").await;
        e.send(ROOM, 1, &a, GAME, "shot", json!({"ax": 0, "az": -4096, "power": 30})).await;
        until(&mut ra, GAME, "shot").await;
        // c saw the shared `lobby` roster -- that one IS To::All -- and nothing
        // else; no `golf`, no `shot`.
        let seen = drain(&mut rc);
        assert!(!seen.is_empty(), "the shared lobby event does reach the room");
        assert!(seen.iter().all(|(g, ev)| g == GAME && ev == "lobby"), "{seen:?}");
        // ... and they cannot play
        e.send(ROOM, 2, &c, GAME, "shot", json!({"ax": 0, "az": -4096, "power": 30})).await;
        assert_eq!(until(&mut rc, GAME, "error").await["error"], json!("join the lobby first"));
    }

    #[tokio::test]
    async fn a_pos_reaches_the_lobby_but_never_the_sender() {
        let e = env(4, 1);
        let (a, mut ra) = e.connect(ROOM, 1, "a").await;
        let (b, mut rb) = e.connect(ROOM, 2, "b").await;
        e.send(ROOM, 1, &a, GAME, "join", json!({})).await;
        e.send(ROOM, 2, &b, GAME, "join", json!({})).await;
        e.send(ROOM, 1, &a, GAME, "start", json!({"course": "windmill"})).await;
        until_where(&mut ra, GAME, GAME, |m| m["round"]["phase"] == "playing").await;
        drain(&mut rb);
        e.send(ROOM, 2, &b, GAME, "pos", json!({"x": 1, "z": 2, "r": 725, "a": 1, "q": 1})).await;
        let p = until(&mut ra, GAME, "pos").await;
        // the exact frame test_golf.py:330 pins, key for key
        assert_eq!(
            p,
            json!({"type": "game", "g": "golf", "ev": "pos", "pv": 1, "u": "b", "x": 1, "z": 2,
                   "r": 5, "a": 1, "q": 1})
        );
        // b never sees their own pos echoed
        e.send(ROOM, 2, &b, GAME, "view", json!({})).await;
        let seen = drain(&mut rb);
        assert!(!seen.iter().any(|(_, ev)| ev == "pos"), "{seen:?}");
        assert!(seen.iter().any(|(_, ev)| ev == GAME), "the view round-trip arrived");
    }

    #[tokio::test]
    async fn an_unknown_golf_op_answers_nothing() {
        let e = env(4, 1);
        let (a, mut ra) = e.connect(ROOM, 1, "a").await;
        e.send(ROOM, 1, &a, GAME, "join", json!({})).await;
        until(&mut ra, GAME, GAME).await;
        drain(&mut ra);
        e.send(ROOM, 1, &a, GAME, "teleport", json!({})).await;
        e.send(ROOM, 1, &a, GAME, "view", json!({})).await; // a round trip
        let seen = drain(&mut ra);
        assert_eq!(seen, [(GAME.to_string(), GAME.to_string())], "only the view answered");
    }

    #[tokio::test]
    async fn concede_and_char_answer_pythons_words() {
        let e = env(4, 1);
        let (a, mut ra) = e.connect(ROOM, 1, "a").await;
        e.send(ROOM, 1, &a, GAME, "join", json!({})).await;
        until(&mut ra, GAME, GAME).await;
        e.send(ROOM, 1, &a, GAME, "concede", json!({})).await;
        assert_eq!(until(&mut ra, GAME, "error").await["error"], json!("nothing to pick up"));
        e.send(ROOM, 1, &a, GAME, "char", json!({"c": "banana"})).await;
        assert_eq!(until(&mut ra, GAME, "error").await["error"], json!("pick a character"));
    }

    #[tokio::test]
    async fn a_late_joiner_spectates_and_a_rejoin_returns_to_the_round() {
        let e = env(4, 1);
        let (a, mut ra) = e.connect(ROOM, 1, "a").await;
        let (b, rb) = e.connect(ROOM, 2, "b").await;
        e.send(ROOM, 1, &a, GAME, "join", json!({})).await;
        e.send(ROOM, 2, &b, GAME, "join", json!({})).await;
        e.send(ROOM, 1, &a, GAME, "start", json!({"course": "meadow"})).await;
        until_where(&mut ra, GAME, GAME, |m| m["round"]["phase"] == "playing").await;
        e.send(ROOM, 2, &b, GAME, "shot", json!({"ax": 0, "az": -4096, "power": 25, "seq": 1, "hole": 0}))
            .await;
        until(&mut ra, GAME, "shot").await;
        drop(rb);
        e.disconnect(ROOM, 2, &b).await;
        until_where(&mut ra, GAME, GAME, |m| m["left"] == "b").await;
        // a late joiner gets the full round view at once (spectator, not player)
        let (c, mut rc) = e.connect(ROOM, 3, "c").await;
        e.send(ROOM, 3, &c, GAME, "join", json!({})).await;
        let view = until(&mut rc, GAME, GAME).await;
        assert_eq!(view["round"]["phase"], json!("playing"));
        let seated: Vec<&str> = view["round"]["players"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["user"]["userId"].as_str().unwrap())
            .collect();
        assert_eq!(seated, ["a"]);
        // b reconnects: back in the round with their stroke, and the whole lobby
        // is told
        e.advance(1.0);
        let (b2, mut rb2) = e.connect(ROOM, 4, "b").await;
        e.send(ROOM, 4, &b2, GAME, "join", json!({})).await;
        let back = until(&mut rb2, GAME, GAME).await;
        assert_eq!(back["back"], json!("b"));
        let me = back["round"]["players"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["user"]["userId"] == "b")
            .unwrap()
            .clone();
        assert_eq!(me["strokes"][0], json!(1));
        until_where(&mut rc, GAME, GAME, |m| m["back"] == "b").await;
        // leaving mid-hole lets the hole advance once everyone else is done
        e.send(ROOM, 1, &a, GAME, "concede", json!({})).await;
        until(&mut ra, GAME, GAME).await;
        e.send(ROOM, 4, &b2, GAME, "leave", json!({})).await;
        assert_eq!(until(&mut ra, GAME, "hole").await["hole"], json!(1));
    }

    #[tokio::test]
    async fn the_grace_recheck_advances_the_hole_and_tells_the_lobby() {
        // The port of test_grace_timer_recheck_advances_the_hole_and_tells_the
        // _lobby: u1's socket dropped mid-hole, so the hole is held; once the
        // grace runs out the recheck moves it on with nobody asking.
        let e = env(4, 1);
        let (a, mut ra) = e.connect(ROOM, 1, "a").await;
        let (b, rb) = e.connect(ROOM, 2, "b").await;
        e.send(ROOM, 1, &a, GAME, "join", json!({})).await;
        e.send(ROOM, 2, &b, GAME, "join", json!({})).await;
        e.send(ROOM, 1, &a, GAME, "start", json!({"course": "meadow"})).await;
        until_where(&mut ra, GAME, GAME, |m| m["round"]["phase"] == "playing").await;
        e.send(ROOM, 1, &a, GAME, "concede", json!({})).await; // a is done with hole 1
        until(&mut ra, GAME, GAME).await;
        drop(rb);
        e.disconnect(ROOM, 2, &b).await; // b blipped: the hole waits
        until_where(&mut ra, GAME, GAME, |m| m["left"] == "b").await;
        assert_eq!(e.hub.with_room(ROOM, |v| v.golf.hole), Some(0));
        drain(&mut ra);
        e.hub.recheck(ROOM).await;
        assert!(drain(&mut ra).is_empty(), "still inside the grace period");
        assert_eq!(e.hub.with_room(ROOM, |v| v.golf.hole), Some(0));
        e.advance(GRACE + 0.1);
        e.hub.recheck(ROOM).await;
        assert_eq!(drain(&mut ra), [("golf".into(), "hole".into()), ("golf".into(), "golf".into())]);
        assert_eq!(e.hub.with_room(ROOM, |v| v.golf.hole), Some(1));
    }

    #[tokio::test]
    async fn a_socket_drop_keeps_the_hole_and_the_host_on_a_quick_rejoin() {
        let e = env(4, 1);
        let (a, mut ra) = e.connect(ROOM, 1, "a").await;
        let (b, rb) = e.connect(ROOM, 2, "b").await;
        e.send(ROOM, 1, &a, GAME, "join", json!({})).await;
        e.send(ROOM, 2, &b, GAME, "join", json!({})).await;
        e.send(ROOM, 1, &a, GAME, "start", json!({"course": "meadow"})).await;
        until_where(&mut ra, GAME, GAME, |m| m["round"]["phase"] == "playing").await;
        e.send(ROOM, 1, &a, GAME, "concede", json!({})).await;
        until(&mut ra, GAME, GAME).await;
        e.send(ROOM, 2, &b, GAME, "shot", json!({"ax": 0, "az": -4096, "power": 25, "hole": 0})).await;
        until(&mut ra, GAME, "shot").await;
        drop(rb);
        e.disconnect(ROOM, 2, &b).await;
        let gone = until_where(&mut ra, GAME, GAME, |m| m["left"] == "b").await;
        assert_eq!(gone["round"]["hole"], json!(0), "the hole waits for them");
        e.advance(1.0);
        let (b2, mut rb2) = e.connect(ROOM, 3, "b").await;
        e.send(ROOM, 3, &b2, GAME, "join", json!({})).await;
        let back = until(&mut rb2, GAME, GAME).await;
        assert_eq!(back["back"], json!("b"));
        assert_eq!(back["round"]["hole"], json!(0));
        let me = back["round"]["players"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["user"]["userId"] == "b")
            .unwrap()
            .clone();
        assert_eq!(me["strokes"][0], json!(1));
        assert_eq!(me["done"], json!(false));
        // now the host (a) blips: b holds the host meanwhile, a gets it back on
        // a quick return
        drop(ra);
        e.disconnect(ROOM, 1, &a).await;
        let roster = until_where(&mut rb2, GAME, "lobby", |m| m["members"].as_array().unwrap().len() == 1).await;
        assert_eq!(roster["members"][0]["userId"], json!("b"));
        assert_eq!(roster["members"][0]["host"], json!(true));
        e.advance(1.0);
        let (a2, mut ra2) = e.connect(ROOM, 4, "a").await;
        e.send(ROOM, 4, &a2, GAME, "join", json!({})).await;
        let roster = until_where(&mut ra2, GAME, "lobby", |m| m["members"].as_array().unwrap().len() == 2).await;
        let hosts: Vec<(&str, bool)> = roster["members"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| (m["userId"].as_str().unwrap(), m["host"].as_bool().unwrap()))
            .collect();
        assert_eq!(hosts, [("b", false), ("a", true)]);
        e.send(ROOM, 4, &a2, GAME, "skip", json!({})).await; // host controls work again
        assert_eq!(until(&mut rb2, GAME, "hole").await["hole"], json!(1));
        // a blip after the grace period: the new host keeps it
        drop(ra2);
        e.disconnect(ROOM, 4, &a2).await;
        until_where(&mut rb2, GAME, "lobby", |m| m["members"].as_array().unwrap().len() == 1).await;
        e.advance(GRACE + 1.0);
        let (a3, mut ra3) = e.connect(ROOM, 5, "a").await;
        e.send(ROOM, 5, &a3, GAME, "join", json!({})).await;
        let roster = until_where(&mut ra3, GAME, "lobby", |m| m["members"].as_array().unwrap().len() == 2).await;
        let hosts: Vec<(&str, bool)> = roster["members"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| (m["userId"].as_str().unwrap(), m["host"].as_bool().unwrap()))
            .collect();
        assert_eq!(hosts, [("b", true), ("a", false)]);
    }

    #[tokio::test]
    async fn a_concede_that_finishes_the_hole_sends_golf_then_hole_then_golf() {
        // The order is the Python's statement order in `golf_op`'s concede arm:
        // the fresh view FIRST (so the page can grey the player out), then
        // `_golf_advance`'s step event, then the view again. `until` would pass
        // on any order, so this drains.
        let e = env(4, 1);
        let (a, mut ra) = e.connect(ROOM, 1, "a").await;
        e.send(ROOM, 1, &a, GAME, "join", json!({})).await;
        e.send(ROOM, 1, &a, GAME, "start", json!({"course": "meadow"})).await;
        let started = until_where(&mut ra, GAME, GAME, |m| m["round"]["phase"] == "playing").await;
        assert_eq!(started["by"]["userId"], json!("a"), "the start names who called it");
        drain(&mut ra);
        e.send(ROOM, 1, &a, GAME, "concede", json!({})).await;
        assert_eq!(
            drain(&mut ra),
            [("golf".into(), "golf".into()),
             ("golf".into(), "hole".into()),
             ("golf".into(), "golf".into())]
        );
    }

    #[tokio::test]
    async fn an_op_advances_a_held_hole_before_it_answers() {
        // `golf_op` runs `_golf_advance` at the TOP, before it even looks at the
        // op: a dropped player's hold may have run out meanwhile. So a plain
        // `view` sent after the grace expires moves the hole on first and
        // answers second.
        let e = env(4, 1);
        let (a, mut ra) = e.connect(ROOM, 1, "a").await;
        let (b, rb) = e.connect(ROOM, 2, "b").await;
        e.send(ROOM, 1, &a, GAME, "join", json!({})).await;
        e.send(ROOM, 2, &b, GAME, "join", json!({})).await;
        e.send(ROOM, 1, &a, GAME, "start", json!({"course": "meadow"})).await;
        until_where(&mut ra, GAME, GAME, |m| m["round"]["phase"] == "playing").await;
        e.send(ROOM, 1, &a, GAME, "concede", json!({})).await;
        drop(rb);
        e.disconnect(ROOM, 2, &b).await;
        until_where(&mut ra, GAME, GAME, |m| m["left"] == "b").await;
        assert_eq!(e.hub.with_room(ROOM, |v| v.golf.hole), Some(0));
        drain(&mut ra);
        e.advance(GRACE + 0.1);
        e.send(ROOM, 1, &a, GAME, "view", json!({})).await;
        assert_eq!(
            drain(&mut ra),
            [("golf".into(), "hole".into()),
             ("golf".into(), "golf".into()),  // _golf_advance's view
             ("golf".into(), "golf".into())]  // the view op's own answer
        );
        assert_eq!(e.hub.with_room(ROOM, |v| v.golf.hole), Some(1));
    }

    #[tokio::test]
    async fn an_explicit_leave_ends_a_lone_players_round() {
        let e = env(4, 1);
        let (a, mut ra) = e.connect(ROOM, 1, "a").await;
        e.send(ROOM, 1, &a, GAME, "join", json!({})).await;
        until(&mut ra, GAME, GAME).await;
        e.send(ROOM, 1, &a, GAME, "start", json!({"course": "meadow"})).await;
        until_where(&mut ra, GAME, GAME, |m| m["round"]["phase"] == "playing").await;
        e.send(ROOM, 1, &a, GAME, "leave", json!({})).await;
        // test_golf.py:396 -- the room's valley is gone or the round is idle
        assert_eq!(e.hub.with_room(ROOM, |v| v.golf.phase), Some(Phase::Idle));
    }
}
