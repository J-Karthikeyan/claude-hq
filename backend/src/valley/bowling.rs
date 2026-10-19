//! Bowling for the Valley (g = "bowl", HQ 2.5): ten-pin on the golf physics
//! model, and the round referee.
//!
//! PHYSICS. Integer-only, in golf's units ([`TILE`] = 1 m = 10000, velocity in
//! units/tick x [`VS`], [`TICK`] ticks a second, [`tdiv`] truncating and
//! [`isqrt`] flooring), so the server and every browser roll exactly the same
//! ball: the server simulates each roll from {x, aim, power, spin} and
//! broadcasts the inputs and the pins that fell, and the clients replay the same
//! simulation (games/bowling.js, between the BOWL-SIM markers) only to animate
//! it. `backend/tests/bowl_golden.json` pins a few hundred seeded rolls plus
//! the scoring fixtures, and tests/test_bowl_sync.py runs the JS block against
//! the same file. Keep the two in lock-step.
//!
//! One straight lane: a gutter on each side (or bumpers), ten pins in the usual
//! triangle, a ball that skids on the oil and hooks with its spin once it
//! reaches the dry back end, and ball-pin then pin-pin impulse chains resolved
//! in a fixed order every tick. A standing pin never moves: it is either hit
//! hard enough to fall (and then slides) or it stays put.
//!
//! REFEREE. TURN-BASED like golf, with no ticker. Players bowl a whole frame
//! each in join order; the bowler whose turn it is is the one with the fewest
//! finished frames (ties in seat order), so a player who drops out never stalls
//! the round. The only timer is the shared golf recheck
//! ([`super::ValleyHub::recheck`]), which asks [`grace_left`] and runs
//! [`recheck_step`]: a socket drop holds that player's place for [`GRACE`]
//! seconds, then they are out (dnf) and the round moves on.
//!
//! AUDIENCE: every bowl event goes to THIS GAME'S LOBBY, or to the one asking
//! socket (`view` and every error), never the whole room
//! (`a_bowl_event_goes_only_to_the_lobby`).

use super::golf::{tdiv, TICK, VS};
use super::{as_int_clamped, isqrt, py_trunc, Ctx, Left, Out, RoomValley, Seq, I_BOWL};
use rand::Rng;
use serde_json::{json, Value};

/// The game key on the wire.
pub const GAME: &str = "bowl";

// ------------------------------------------------------------------ physics --
/// Half the lane's width (a real lane is 1.05 m).
pub const HALF_W: i64 = 5270;
/// Ball and pin radii.
pub const BR: i64 = 1090;
pub const PR: i64 = 610;
/// The furthest a ball may start from the middle.
pub const X_MAX: i64 = HALF_W - BR;
/// The head pin's distance from the foul line (18.29 m).
pub const HEAD_Z: i64 = 182900;
/// Pin spacing: half the 30.5 cm gap across, and the row depth.
pub const PIN_DX: i64 = 1524;
pub const ROW_DZ: i64 = 2640;
/// Past this the ball and any pin are in the pit.
pub const PIT_Z: i64 = 197820;
/// Launch speed for power 1 and power 100 (units/tick x VS).
pub const V_MIN: i64 = 120000;
pub const V_MAX: i64 = 220000;
/// aim is -AIM_MAX..AIM_MAX; the sideways speed is vz * aim / AIM_DIV.
pub const AIM_MAX: i64 = 100;
pub const AIM_DIV: i64 = 1500;
pub const SPIN_MAX: i64 = 100;
/// Sideways pull per tick at full spin on the dry back end; an eighth on oil.
pub const HOOK: i64 = 300;
pub const OIL_DIV: i64 = 50;
/// The oil patterns a round can draw (where the hook starts).
pub const OIL: [i64; 3] = [110000, 125000, 140000];
/// Rolling drag on the ball, per 10000 a tick.
pub const BALL_DRAG: i64 = 6;
/// A ball slower than this has stopped on the lane.
pub const V_STALL: i64 = 5000;
/// Masses (ball, pin) and the restitution EN/ED.
pub const BALL_M: i64 = 14;
pub const PIN_M: i64 = 3;
pub const EN: i64 = 3;
pub const ED: i64 = 4;
/// A sliding pin's drag per 1000 a tick, and the speed below which it rests.
pub const PIN_DRAG: i64 = 12;
pub const PIN_STOP: i64 = 600;
/// A toppling pin reaches this far from its base (about half its height).
pub const FALL_R: i64 = 900;
/// The least impulse that topples a standing pin.
pub const PIN_TIP: i64 = 2000;
pub const MAX_TICKS: i64 = 900;
/// A path sample every this many ticks.
pub const SAMPLE: i64 = 4;

/// The rack, pins 1-10, as (x, z). Pin 2 is on the bowler's left (-x).
pub const PINS: [(i64, i64); 10] = [
    (0, HEAD_Z),
    (-PIN_DX, HEAD_Z + ROW_DZ),
    (PIN_DX, HEAD_Z + ROW_DZ),
    (-2 * PIN_DX, HEAD_Z + 2 * ROW_DZ),
    (0, HEAD_Z + 2 * ROW_DZ),
    (2 * PIN_DX, HEAD_Z + 2 * ROW_DZ),
    (-3 * PIN_DX, HEAD_Z + 3 * ROW_DZ),
    (-PIN_DX, HEAD_Z + 3 * ROW_DZ),
    (PIN_DX, HEAD_Z + 3 * ROW_DZ),
    (3 * PIN_DX, HEAD_Z + 3 * ROW_DZ),
];

/// The lane a ball is rolled on: bumpers or gutters, and which pins stand.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Lane {
    pub bumpers: bool,
    pub standing: [bool; 10],
}

/// One roll's outcome.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Roll {
    /// The ball's centre every [`SAMPLE`] ticks, and where it ended.
    pub path_samples: Vec<(i64, i64)>,
    /// Pins that were standing and fell.
    pub pins_down: [bool; 10],
    pub gutter: bool,
    pub ticks: i64,
}

#[derive(Clone, Copy)]
struct Pin {
    x: i64,
    z: i64,
    vx: i64,
    vz: i64,
    down: bool,
    off: bool,
}

/// Roll one ball. `x0` is the start across the lane, `aim` the line
/// (-[`AIM_MAX`]..), `power` 1..100, `spin` -[`SPIN_MAX`].. (+ hooks to +x),
/// `oil` an index into [`OIL`]. Inputs are trusted: the referee clamps them.
pub fn simulate_roll(lane: &Lane, x0: i64, aim: i64, power: i64, spin: i64, oil: usize) -> Roll {
    let oil_len = OIL[oil.min(OIL.len() - 1)];
    let mut pins = [Pin { x: 0, z: 0, vx: 0, vz: 0, down: false, off: false }; 10];
    for (i, p) in pins.iter_mut().enumerate() {
        p.x = PINS[i].0;
        p.z = PINS[i].1;
        p.off = !lane.standing[i];
    }
    let (mut bx, mut bz) = (x0, 0i64);
    let mut bvz = V_MIN + tdiv((power - 1) * (V_MAX - V_MIN), 99);
    let mut bvx = tdiv(bvz * aim, AIM_DIV);
    let mut alive = true;
    let mut gutter = false;
    let mut path = vec![(bx, bz)];
    let r_bp = BR + PR;
    let r_pp = PR + FALL_R;
    let mut ticks = MAX_TICKS;
    for t in 1..=MAX_TICKS {
        // 1. the ball
        if alive {
            let hook = if bz > oil_len { tdiv(spin * HOOK, 100) } else { tdiv(spin * HOOK, 100 * OIL_DIV) };
            if !gutter {
                bvx += hook;
            }
            bvx -= tdiv(bvx * BALL_DRAG, 10000);
            bvz -= tdiv(bvz * BALL_DRAG, 10000);
            bx += tdiv(bvx, VS);
            bz += tdiv(bvz, VS);
            if !gutter {
                if lane.bumpers {
                    let lim = HALF_W - BR;
                    if bx > lim {
                        bx = 2 * lim - bx;
                        bvx = -tdiv(bvx * EN, ED);
                    } else if bx < -lim {
                        bx = -2 * lim - bx;
                        bvx = -tdiv(bvx * EN, ED);
                    }
                } else if !(-HALF_W..=HALF_W).contains(&bx) {
                    gutter = true;
                    bx = if bx > 0 { HALF_W + BR } else { -HALF_W - BR };
                    bvx = 0;
                }
            }
            if bz > PIT_Z + BR || bvz < V_STALL {
                alive = false;
            }
        }
        // 2. the sliding pins
        for p in pins.iter_mut() {
            if p.off || (p.vx == 0 && p.vz == 0) {
                continue;
            }
            p.x += tdiv(p.vx, VS);
            p.z += tdiv(p.vz, VS);
            p.vx -= tdiv(p.vx * PIN_DRAG, 1000);
            p.vz -= tdiv(p.vz * PIN_DRAG, 1000);
            if p.vx.abs() + p.vz.abs() < PIN_STOP {
                p.vx = 0;
                p.vz = 0;
            }
            if p.z > PIT_Z || p.x > HALF_W || p.x < -HALF_W {
                p.off = true;
                p.vx = 0;
                p.vz = 0;
            }
        }
        // 3. the ball against each pin, in rack order
        if alive && !gutter {
            for p in pins.iter_mut() {
                if p.off {
                    continue;
                }
                let (dx, dz) = (p.x - bx, p.z - bz);
                let d2 = dx * dx + dz * dz;
                if d2 >= r_bp * r_bp {
                    continue;
                }
                let d = isqrt(d2).max(1);
                let vn = tdiv((bvx - p.vx) * dx + (bvz - p.vz) * dz, d);
                if vn > 0 {
                    let imp = (EN + ED) * vn;
                    let dp = tdiv(imp * BALL_M, ED * (BALL_M + PIN_M));
                    let db = tdiv(imp * PIN_M, ED * (BALL_M + PIN_M));
                    p.vx += tdiv(dp * dx, d);
                    p.vz += tdiv(dp * dz, d);
                    bvx -= tdiv(db * dx, d);
                    bvz -= tdiv(db * dz, d);
                    p.down = true;
                }
                if p.down {
                    p.x = bx + tdiv(dx * r_bp, d);
                    p.z = bz + tdiv(dz * r_bp, d);
                }
            }
        }
        // 4. pin against pin, in (i, j) order
        for i in 0..10 {
            for j in (i + 1)..10 {
                let (a, b) = (pins[i], pins[j]);
                if a.off || b.off || (!a.down && !b.down) {
                    continue;
                }
                let (dx, dz) = (b.x - a.x, b.z - a.z);
                let d2 = dx * dx + dz * dz;
                if d2 >= r_pp * r_pp {
                    continue;
                }
                let d = isqrt(d2).max(1);
                let vn = tdiv((a.vx - b.vx) * dx + (a.vz - b.vz) * dz, d);
                let (mut a, mut b) = (a, b);
                if vn > 0 {
                    let half = tdiv((EN + ED) * vn, 2 * ED);
                    if half >= PIN_TIP || (a.down && b.down) {
                        a.vx -= tdiv(half * dx, d);
                        a.vz -= tdiv(half * dz, d);
                        b.vx += tdiv(half * dx, d);
                        b.vz += tdiv(half * dz, d);
                        a.down = true;
                        b.down = true;
                    } else {
                        let full = tdiv((EN + ED) * vn, ED);
                        if a.down {
                            a.vx -= tdiv(full * dx, d);
                            a.vz -= tdiv(full * dz, d);
                        } else {
                            b.vx += tdiv(full * dx, d);
                            b.vz += tdiv(full * dz, d);
                        }
                    }
                }
                let ov = r_pp - d;
                if a.down && b.down {
                    let h = ov / 2 + 1;
                    a.x -= tdiv(dx * h, d);
                    a.z -= tdiv(dz * h, d);
                    b.x += tdiv(dx * h, d);
                    b.z += tdiv(dz * h, d);
                } else if a.down {
                    a.x -= tdiv(dx * ov, d);
                    a.z -= tdiv(dz * ov, d);
                } else {
                    b.x += tdiv(dx * ov, d);
                    b.z += tdiv(dz * ov, d);
                }
                pins[i] = a;
                pins[j] = b;
            }
        }
        if t % SAMPLE == 0 {
            path.push((bx, bz));
        }
        if !alive && pins.iter().all(|p| p.off || (p.vx == 0 && p.vz == 0)) {
            ticks = t;
            break;
        }
    }
    if path.last() != Some(&(bx, bz)) {
        path.push((bx, bz));
    }
    let mut pins_down = [false; 10];
    for i in 0..10 {
        pins_down[i] = lane.standing[i] && (pins[i].down || pins[i].off);
    }
    Roll { path_samples: path, pins_down, gutter, ticks }
}

// ------------------------------------------------------------------ scoring --
/// Standard ten-pin scoring. `frames` is every frame of the game (unplayed ones
/// empty), each the pins per ball; the last frame takes its bonus balls. The
/// running total per frame, None from the first frame not yet decided.
pub fn score_card(frames: &[Vec<u8>]) -> Vec<Option<u16>> {
    let n = frames.len();
    let mut out = vec![None; n];
    let mut total: u16 = 0;
    for i in 0..n {
        let f = &frames[i];
        let sum2 = |f: &Vec<u8>| f.iter().take(2).map(|&x| x as u16).sum::<u16>();
        let s = if i + 1 == n {
            let done = f.len() == 3 || (f.len() == 2 && sum2(f) < 10);
            done.then(|| f.iter().map(|&x| x as u16).sum())
        } else if f.first() == Some(&10) {
            bonus(frames, i, 2).map(|b| 10 + b)
        } else if f.len() >= 2 {
            if sum2(f) == 10 { bonus(frames, i, 1).map(|b| 10 + b) } else { Some(sum2(f)) }
        } else {
            None
        };
        match s {
            Some(v) => {
                total += v;
                out[i] = Some(total);
            }
            None => break,
        }
    }
    out
}

/// The next `k` balls after frame `i`, summed, once they have all been rolled.
fn bonus(frames: &[Vec<u8>], i: usize, k: usize) -> Option<u16> {
    let next: Vec<u16> = frames[i + 1..].iter().flatten().take(k).map(|&x| x as u16).collect();
    (next.len() == k).then(|| next.iter().sum())
}

// ------------------------------------------------------------------ referee --
/// A dropped socket keeps its place this long.
pub const GRACE: f64 = 15.0;
/// After a roll, the lane is busy for the roll plus this.
pub const SETTLE: f64 = 0.3;
pub const MAX_PLAYERS: usize = 8;
pub const DEX_MAX: i64 = 1025;

pub const ERR_HOST: &str = "only the host can do that";
pub const ERR_TURN: &str = "it's not your turn";
pub const ERR_NONE: &str = "no game is being bowled";
pub const ERR_FRAMES: &str = "pick 10 or 5 frames";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Phase {
    #[default]
    Idle,
    Playing,
    Done,
}

impl Phase {
    fn as_str(self) -> &'static str {
        match self {
            Phase::Idle => "idle",
            Phase::Playing => "playing",
            Phase::Done => "done",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Player {
    pub user: Value,
    pub rolls: Vec<Vec<u8>>,
    pub standing: [bool; 10],
    /// Frames finished.
    pub done: usize,
    pub strikes: i64,
    pub spares: i64,
    /// When their socket dropped, while they may still come back.
    pub away: Option<f64>,
    /// Out of this game (left, conceded or never came back).
    pub gone: bool,
}

impl Player {
    fn total(&self) -> u16 {
        score_card(&self.rolls).iter().rev().find_map(|s| *s).unwrap_or(0)
    }
}

#[derive(Default)]
pub struct Bowling {
    pub phase: Phase,
    pub frames: usize,
    pub bumpers: bool,
    pub oil: usize,
    pub players: Seq<Player>,
    /// user_id -> bowler dex, kept between games (cosmetic only).
    pub chars: Seq<i64>,
    pub busy_until: f64,
    /// The last finished game's summary, for the idle view.
    last: Option<Value>,
}

/// The board key: "f10", "f5", and a "b" with bumpers.
pub fn key_of(frames: usize, bumpers: bool) -> String {
    format!("f{}{}", frames, if bumpers { "b" } else { "" })
}

impl Bowling {
    /// Whose turn: the player still in with the fewest frames done, seat order
    /// breaking ties. None when nobody has a frame left.
    pub fn turn(&self) -> Option<&str> {
        self.players
            .iter()
            .enumerate()
            .filter(|(_, (_, p))| !p.gone && p.done < self.frames)
            .min_by_key(|(i, (_, p))| (p.done, *i))
            .map(|(_, (uid, _))| uid)
    }

    pub fn view(&self, t: f64) -> Value {
        let turn = self.turn().filter(|_| self.phase == Phase::Playing);
        let turn_v = match turn.and_then(|u| self.players.get(u).map(|p| (u, p))) {
            Some((u, p)) => json!({"user": u, "frame": p.done, "ball": p.rolls[p.done].len()}),
            None => Value::Null,
        };
        json!({
            "phase": self.phase.as_str(),
            "frames": self.frames,
            "bumpers": self.bumpers,
            "oil": self.oil,
            "key": key_of(if self.frames == 0 { 10 } else { self.frames }, self.bumpers),
            "players": self.players.values().map(|p| json!({
                "user": p.user,
                "rolls": p.rolls,
                "scores": score_card(&p.rolls),
                "total": p.total(),
                "standing": p.standing,
                "strikes": p.strikes,
                "spares": p.spares,
                "away": p.away.is_some(),
                "dnf": p.gone,
            })).collect::<Vec<_>>(),
            "turn": turn_v,
            "readyInMs": py_trunc((self.busy_until - t) * 1000.0).max(0),
            "chars": self.chars.to_object(|c| json!(c)),
            "last": self.last,
        })
    }

    pub fn start(&mut self, members: &[(String, Value)], msg: &Value, oil: usize, t: f64) -> Result<(), &'static str> {
        if self.phase == Phase::Playing {
            return Err("a game is already on: the host can end it first");
        }
        let frames = match msg.get("frames").filter(|v| !v.is_boolean()).and_then(Value::as_i64) {
            Some(10) => 10,
            Some(5) => 5,
            _ => return Err(ERR_FRAMES),
        };
        self.frames = frames;
        self.bumpers = msg.get("bumpers").and_then(Value::as_bool).unwrap_or(false);
        self.oil = oil.min(OIL.len() - 1);
        self.phase = Phase::Playing;
        self.busy_until = t;
        self.players = Seq::new();
        for (uid, pubv) in members.iter().take(MAX_PLAYERS) {
            self.players.set(
                uid,
                Player {
                    user: pubv.clone(),
                    rolls: vec![Vec::new(); frames],
                    standing: [true; 10],
                    done: 0,
                    strikes: 0,
                    spares: 0,
                    away: None,
                    gone: false,
                },
            );
        }
        Ok(())
    }

    /// Validate and roll one ball: the `roll` event, or the error.
    pub fn roll(&mut self, uid: &str, msg: &Value, t: f64) -> Result<(Value, i64), &'static str> {
        if self.phase != Phase::Playing {
            return Err(ERR_NONE);
        }
        let Some(p) = self.players.get(uid) else { return Err("you're not in this game") };
        if p.gone {
            return Err("you're out of this game");
        }
        if self.turn() != Some(uid) {
            return Err(ERR_TURN);
        }
        let (frame, ball) = (p.done, p.rolls[p.done].len());
        for (k, want) in [("frame", frame), ("ball", ball)] {
            let given = msg.get(k).filter(|v| !v.is_null());
            if given.is_some() && as_int_clamped(given, -1, 1 << 16) != Some(want as i64) {
                return Err("that ball is already rolled");
            }
        }
        if t < self.busy_until {
            return Err("wait for the pins to settle");
        }
        let x = as_int_clamped(msg.get("x"), -X_MAX, X_MAX);
        let aim = as_int_clamped(msg.get("aim"), -AIM_MAX, AIM_MAX);
        let power = as_int_clamped(msg.get("power"), 1, 100);
        let spin = as_int_clamped(msg.get("spin"), -SPIN_MAX, SPIN_MAX);
        let (Some(x), Some(aim), Some(power), Some(spin)) = (x, aim, power, spin) else {
            return Err("bad roll");
        };
        let seq = as_int_clamped(msg.get("seq"), 0, 1 << 30).unwrap_or(0);
        let before = p.standing;
        let lane = Lane { bumpers: self.bumpers, standing: before };
        let res = simulate_roll(&lane, x, aim, power, spin, self.oil);
        let last_frame = frame + 1 == self.frames;
        let p = self.players.get_mut(uid).expect("checked above");
        let fell = res.pins_down.iter().filter(|d| **d).count() as u8;
        for (s, d) in p.standing.iter_mut().zip(res.pins_down) {
            *s = *s && !d;
        }
        let full = before.iter().all(|s| *s);
        let cleared = p.standing.iter().all(|s| !*s);
        let strike = full && cleared;
        let spare = !full && cleared;
        p.strikes += strike as i64;
        p.spares += spare as i64;
        p.rolls[frame].push(fell);
        let complete = if !last_frame {
            strike || ball >= 1
        } else {
            ball >= 2 || (ball == 1 && !cleared && p.rolls[frame][0] != 10)
        };
        if complete {
            p.done += 1;
            p.standing = [true; 10];
        } else if cleared {
            p.standing = [true; 10];
        }
        self.busy_until = t + res.ticks as f64 / TICK as f64 + SETTLE;
        let end = res.path_samples.last().copied().unwrap_or((x, 0));
        let ev = json!({
            "user": p.user,
            "seq": seq,
            "frame": frame,
            "ball": ball,
            "input": {"x": x, "aim": aim, "power": power, "spin": spin},
            "pins_down": res.pins_down,
            "standing": before,
            "fell": fell,
            "strike": strike,
            "spare": spare,
            "gutter": res.gutter,
            "end": [end.0, end.1],
            "ticks": res.ticks,
            "complete": complete,
        });
        Ok((ev, res.ticks))
    }

    /// Fill the rest of this player's frame with misses.
    fn zero_frame(&mut self, uid: &str) -> bool {
        let last = self.frames;
        let Some(p) = self.players.get_mut(uid) else { return false };
        if p.gone || p.done >= last {
            return false;
        }
        let is_last = p.done + 1 == last;
        let f = &mut p.rolls[p.done];
        loop {
            let full = if is_last {
                f.len() >= 3 || (f.len() == 2 && (f[0] as u16 + f[1] as u16) < 10)
            } else {
                f.first() == Some(&10) || f.len() >= 2
            };
            if full {
                break;
            }
            f.push(0);
        }
        p.done += 1;
        p.standing = [true; 10];
        true
    }

    /// Out of the game; their score so far stands as a dnf.
    pub fn knock_out(&mut self, uid: &str) -> bool {
        match self.players.get_mut(uid) {
            Some(p) if !p.gone && self.phase == Phase::Playing => {
                p.gone = true;
                p.away = None;
                true
            }
            _ => false,
        }
    }

    /// Players whose grace ran out are out. True if anyone was.
    pub fn expire(&mut self, t: f64) -> bool {
        let mut any = false;
        for p in self.players.values_mut() {
            if let Some(at) = p.away {
                if !p.gone && t - at >= GRACE {
                    p.gone = true;
                    p.away = None;
                    any = true;
                }
            }
        }
        any
    }

    /// The soonest a held place runs out, or None.
    pub fn grace_left(&self, t: f64) -> Option<f64> {
        if self.phase != Phase::Playing {
            return None;
        }
        self.players
            .values()
            .filter(|p| !p.gone)
            .filter_map(|p| p.away.map(|at| GRACE - (t - at)))
            .fold(None, |acc: Option<f64>, x| Some(acc.map_or(x, |a: f64| a.min(x))))
    }

    /// If nobody has a frame left, the game ends: the `done` data, or (when
    /// every player dropped out) the game just goes idle with nothing recorded.
    pub fn finish(&mut self) -> Option<Value> {
        if self.phase != Phase::Playing || self.turn().is_some() {
            return None;
        }
        if self.players.values().all(|p| p.gone) {
            self.phase = Phase::Idle;
            return None;
        }
        self.phase = Phase::Done;
        let key = key_of(self.frames, self.bumpers);
        let mut rows: Vec<(bool, u16, usize, &str, &Player)> = self
            .players
            .iter()
            .enumerate()
            .map(|(i, (uid, p))| (p.gone, p.total(), i, uid, p))
            .collect();
        rows.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)).then(a.2.cmp(&b.2)));
        let mut results = Vec::new();
        let mut place = 0;
        let mut prev: Option<(bool, u16)> = None;
        for (gone, score, _, _, p) in &rows {
            if prev != Some((*gone, *score)) {
                place += 1;
                prev = Some((*gone, *score));
            }
            results.push(json!({"user": p.user, "place": place, "score": score,
                                "strikes": p.strikes, "spares": p.spares, "dnf": gone}));
        }
        let mut totals = Seq::new();
        let mut card = Seq::new();
        for (uid, p) in self.players.iter() {
            totals.set(uid, p.total());
            card.set(uid, p.rolls.clone());
        }
        let data = json!({
            "key": key,
            "frames": self.frames,
            "bumpers": self.bumpers,
            "totals": totals.to_object(|v| json!(v)),
            "card": card.to_object(|r| json!(r)),
            "results": results,
        });
        self.last = Some(json!({"key": data["key"], "totals": data["totals"], "results": data["results"]}));
        Some(data)
    }

    pub fn end(&mut self) {
        self.phase = Phase::Idle;
        self.players = Seq::new();
    }
}

// -------------------------------------------------------------- the valley --

fn view_to_lobby(gm: &Bowling, ids: &[String], out: &mut Out, t: f64) {
    out.lobby(ids.to_vec(), GAME, json!({"round": gm.view(t)}));
}

/// After anything that can end the game: `done` to the lobby, then the view.
fn settle(gm: &mut Bowling, ids: &[String], out: &mut Out, t: f64) {
    let was = gm.phase;
    if let Some(d) = gm.finish() {
        out.lobby(ids.to_vec(), "done", d);
    }
    if gm.phase != was {
        view_to_lobby(gm, ids, out, t);
    }
}

pub fn joined(v: &mut RoomValley, cx: &mut Ctx, out: &mut Out) {
    let gm = &mut v.bowl;
    let t = cx.t;
    let uid = cx.uid().to_string();
    let back = match gm.players.get_mut(&uid) {
        Some(p) if p.away.is_some() && !p.gone && gm.phase == Phase::Playing => {
            p.away = None;
            p.user = cx.me();
            true
        }
        _ => false,
    };
    if back {
        let ids = v.lobbies[I_BOWL].ids();
        out.lobby(ids, GAME, json!({"round": gm.view(t), "back": uid}));
    } else {
        out.to(cx.conn, GAME, json!({"round": gm.view(t)}));
    }
}

pub fn op(v: &mut RoomValley, cx: &mut Ctx, op: &str, msg: &Value, out: &mut Out) {
    let gm = &mut v.bowl;
    let lobby = &mut v.lobbies[I_BOWL];
    let t = cx.t;
    let conn = cx.conn;
    let uid = cx.uid().to_string();
    let ids = lobby.ids();
    if gm.expire(t) {
        settle(gm, &ids, out, t);
        if gm.phase == Phase::Playing {
            view_to_lobby(gm, &ids, out, t);
        }
    }
    match op {
        "view" => out.to(conn, GAME, json!({"round": gm.view(t)})),
        "char" => match msg.get("c").and_then(Value::as_i64).filter(|c| (1..=DEX_MAX).contains(c)) {
            None => out.err(conn, "pick a Pokémon"),
            Some(c) => {
                gm.chars.set(&uid, c);
                out.lobby(ids.clone(), "char", json!({"user": uid, "c": c}));
            }
        },
        "roll" => match gm.roll(&uid, msg, t) {
            Err(e) => out.err(conn, e),
            Ok((ev, _)) => {
                let complete = ev["complete"].as_bool().unwrap_or(false);
                let frame = ev["frame"].clone();
                out.lobby(ids.clone(), "roll", ev);
                if complete {
                    if let Some(p) = gm.players.get(&uid) {
                        out.lobby(ids.clone(), "frame", json!({"user": uid, "frame": frame,
                            "rolls": p.rolls, "scores": score_card(&p.rolls), "total": p.total()}));
                    }
                }
                let was = gm.phase;
                settle(gm, &ids, out, t);
                if gm.phase == was {
                    view_to_lobby(gm, &ids, out, t);
                }
            }
        },
        "concede" => {
            if gm.knock_out(&uid) {
                view_to_lobby(gm, &ids, out, t);
                settle(gm, &ids, out, t);
            } else {
                out.err(conn, "nothing to concede");
            }
        }
        "start" | "skip" | "end" => {
            if lobby.host.as_deref() != Some(uid.as_str()) {
                out.err(conn, ERR_HOST);
                return;
            }
            lobby.prev_host = None;
            match op {
                "start" => {
                    let oil = cx.rng.gen_range(0..OIL.len());
                    match gm.start(&lobby.members, msg, oil, t) {
                        Err(e) => out.err(conn, e),
                        Ok(()) => out.lobby(ids.clone(), GAME, json!({"round": gm.view(t), "by": cx.me()})),
                    }
                }
                "skip" => {
                    if gm.phase != Phase::Playing {
                        out.err(conn, ERR_NONE);
                        return;
                    }
                    let cur = gm.turn().map(str::to_string);
                    if let Some(cur) = cur {
                        let away = gm.players.get(&cur).is_some_and(|p| p.away.is_some());
                        if away {
                            gm.knock_out(&cur);
                        } else {
                            gm.zero_frame(&cur);
                        }
                        gm.busy_until = t;
                    }
                    view_to_lobby(gm, &ids, out, t);
                    settle(gm, &ids, out, t);
                }
                _ => {
                    gm.end();
                    view_to_lobby(gm, &ids, out, t);
                }
            }
        }
        _ => {}
    }
}

/// Someone left the lobby. A socket drop holds their place for [`GRACE`]; an
/// explicit leave puts them out at once.
pub fn dropped(
    v: &mut RoomValley, uid: &str, who: &Value, out: &mut Out, t: f64, disconnected: bool,
) -> Left {
    let _ = who;
    let gm = &mut v.bowl;
    if gm.phase != Phase::Playing {
        return Left::Notice;
    }
    let changed = match gm.players.get_mut(uid) {
        Some(p) if !p.gone => {
            if disconnected {
                p.away = Some(t);
            } else {
                p.gone = true;
                p.away = None;
            }
            true
        }
        _ => false,
    };
    if changed {
        let ids = v.lobbies[I_BOWL].ids();
        out.lobby(ids.clone(), GAME, json!({"round": gm.view(t), "left": uid}));
        settle(gm, &ids, out, t);
    }
    Left::Notice
}

/// The soonest a dropped bowler's held place runs out, or None.
pub fn grace_left(v: &RoomValley, t: f64) -> Option<f64> {
    v.bowl.grace_left(t)
}

/// The recheck's bowling half: put out whoever's grace ran out, move the game
/// on, and report the grace still outstanding.
pub fn recheck_step(v: &mut RoomValley, out: &mut Out, t: f64) -> Option<f64> {
    let gm = &mut v.bowl;
    let ids = v.lobbies[I_BOWL].ids();
    if gm.expire(t) {
        view_to_lobby(gm, &ids, out, t);
        settle(gm, &ids, out, t);
    }
    gm.grace_left(t)
}

#[cfg(test)]
mod tests;
