//! Ported from backend/tests/test_platformer.py: level sanity, the referee's rules,
//! 8 runners under simulated lag (race and co-op), and runs through the room + lobby
//! layer (the per-connection queues handle_socket drains).

use super::*;
use crate::realtime::{Registry, MAX_TICKERS, ROOM_BYTES_PER_SEC};
use rand::{rngs::StdRng, Rng, SeedableRng};
use std::collections::BinaryHeap;
use std::time::Duration;
use tokio::sync::mpsc;

// The browser's character (games/platformer.js).
const GRAV: f64 = 26.0;
const JUMP: f64 = 8.6;
const DJUMP: f64 = 7.8;

fn pubv(uid: &str) -> Value {
    json!({"userId": uid, "handle": uid, "displayName": uid, "avatarUrl": ""})
}

fn fr(p: [f64; 3], q: Option<i64>) -> Value {
    let mut m = json!({"x": cm(p[0]), "y": cm(p[1]), "z": cm(p[2]), "r": 0, "a": 1});
    if let Some(q) = q {
        m["q"] = json!(q);
    }
    m
}

fn lv(id: &str) -> &'static Level {
    level(id).unwrap()
}

fn arc_time(d: f64, dh: f64, kind: char) -> f64 {
    let _ = d;
    if kind == 'j' {
        return (JUMP + (JUMP * JUMP - 2.0 * GRAV * dh).sqrt()) / GRAV;
    }
    let s = JUMP / GRAV;
    s + (DJUMP + (DJUMP * DJUMP + 2.0 * GRAV * (JUMP * JUMP / (2.0 * GRAV) - dh)).sqrt()) / GRAV
}

fn arc_lift(t: f64, kind: char) -> f64 {
    if kind == 'j' || t <= JUMP / GRAV {
        return JUMP * t - GRAV * t * t / 2.0;
    }
    let r = t - JUMP / GRAV;
    JUMP * JUMP / (2.0 * GRAV) + DJUMP * r - GRAV * r * r / 2.0
}

/// The level's route as a character runs it from `start` at walking `speed`.
struct Path {
    segs: Vec<(f64, f64, [f64; 3], [f64; 3], char)>,
    total: f64,
}

impl Path {
    fn new(l: &Level, start: [f64; 3], speed: f64) -> Path {
        let mut pts = vec![(start, 'w')];
        pts.extend(l.route.iter().cloned());
        let mut segs = Vec::new();
        let mut t = 0.0;
        for w in pts.windows(2) {
            let (a, b, k) = (w[0].0, w[1].0, w[1].1);
            let d = (b[0] - a[0]).hypot(b[2] - a[2]);
            let tt = if k == 'w' { if d > 1e-9 { d / speed } else { 0.0 } } else { arc_time(d, b[1] - a[1], k) };
            segs.push((t, tt, a, b, k));
            t += tt;
        }
        Path { segs, total: t }
    }

    fn at(&self, tt: f64) -> [f64; 3] {
        let last = self.segs.len() - 1;
        for (i, &(t0, tl, a, b, k)) in self.segs.iter().enumerate() {
            if tt <= t0 + tl || i == last {
                let u = if tl <= 0.0 { 0.0 } else { ((tt - t0) / tl).clamp(0.0, 1.0) };
                let (x, z) = (a[0] + (b[0] - a[0]) * u, a[2] + (b[2] - a[2]) * u);
                let mut y = if k == 'w' { a[1] } else { a[1] + arc_lift(u * tl, k) };
                if k != 'w' && u >= 1.0 {
                    y = b[1];
                }
                return [x, y, z];
            }
        }
        unreachable!()
    }
}

fn dist(a: [f64; 3], b: [f64; 3]) -> f64 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

// ------------------------------------------------------------------ levels --

#[test]
fn levels_are_sane() {
    let ids: Vec<&str> = LEVELS.iter().map(|l| l.id.as_str()).collect();
    assert_eq!(ids, ["meadow", "sky", "fortress"]);
    for l in LEVELS.iter() {
        assert!(!l.name.is_empty() && l.kill < 0.0);
        assert!(l.solids.iter().all(|s| model(&s.m).is_some() && s.y0 >= l.kill));
        assert_eq!(l.spawns.len(), MAX_PLAYERS);
        assert!(l.cps.len() >= 2);
        let on_ground = |p: [f64; 3]| support(l, p[0], p[1], p[2], -PR, 0.01).map(|t| (t - p[1]).abs() < 1e-6).unwrap_or(false);
        for p in l.spawns.iter().chain(l.cps.iter()).chain(std::iter::once(&l.flag)) {
            assert!(on_ground(*p) && !inside(l, p[0], p[1], p[2]), "{} {p:?}", l.id);
        }
        for c in &l.coins {
            assert!(!inside(l, c[0], c[1], c[2]));
            assert!(l.solids.iter().any(|s| s.y1 <= c[1] && c[1] <= s.y1 + 3.2 && in_foot(s, c[0], c[2], 3.5)), "{c:?}");
        }
        assert!(l.goal > 0 && l.goal as usize <= l.coins.len() && l.secs >= 60.0);
        let mut k = 0;
        for (p, kind) in &l.route {
            assert!(support(l, p[0], p[1], p[2], 0.0, 0.01).is_some() && "wjd".contains(*kind));
            if k < l.cps.len() && dist(*p, l.cps[k]) < 0.01 {
                k += 1;
            }
        }
        assert_eq!(k, l.cps.len());
        assert!(dist(l.route.last().unwrap().0, l.flag) < 0.01);
    }
}

#[test]
fn the_envelope_covers_the_browsers_jump() {
    for k in 0..120 {
        let s = k as f64 / 60.0;
        for i in 0..200 {
            let t = i as f64 / 100.0;
            let h = if t <= s {
                JUMP * t - GRAV * t * t / 2.0
            } else {
                JUMP * s - GRAV * s * s / 2.0 + DJUMP * (t - s) - GRAV * (t - s).powi(2) / 2.0
            };
            assert!(h <= lift_bound(t) + 1e-9, "{s} {t}");
        }
    }
    assert!(lift_max(T_APEX) < 3.2 && lift_bound(2.0) < 0.0);
    // the same numbers as the Python
    assert!((lift_max(0.5) - 2.806666666666666).abs() < 1e-9, "{}", lift_max(0.5));
    assert!((lift_bound(1.3) - 3.106666666666666).abs() < 1e-9);
}

#[test]
fn geometry_helpers() {
    let l = lv("meadow");
    let s = &l.solids[0];
    assert!(in_foot(s, s.cx, s.cz, 0.0) && !in_foot(s, s.x1 + 0.2, s.cz, 0.1));
    assert_eq!(support(l, s.cx, s.y1 + 0.3, s.cz, 0.0, 0.6), Some(s.y1));
    assert_eq!(support(l, s.cx, s.y1 + 0.8, s.cz, 0.0, 0.6), None);
    assert!(inside(l, s.cx, s.y1 - 0.3, s.cz) && !inside(l, s.cx, s.y1, s.cz));
    assert_eq!(seg_dist([0.0; 3], [2.0, 0.0, 0.0], [1.0, 1.0, 0.0]), 1.0);
    assert_eq!(seg_dist([0.0; 3], [0.0; 3], [3.0, 4.0, 0.0]), 5.0);
    let r = l.solids.iter().find(|s| s.r > 0.0).unwrap();
    assert!(in_foot(r, r.cx + 2.4, r.cz, 0.0) && !in_foot(r, r.x1 - 0.1, r.z1 - 0.1, 0.0));
}

// ----------------------------------------------------------------- referee --
fn members(n: usize, prefix: &str) -> Vec<(String, Value)> {
    (0..n).map(|i| (format!("{prefix}{i}"), pubv(&format!("{prefix}{i}")))).collect()
}

fn started(n: usize, id: &str, mode: &str) -> (Plat, &'static Level, f64) {
    let mut g = Plat::new();
    assert_eq!(g.start(&members(n, "u"), &json!(id), &json!(mode), 100.0), None);
    g.tick(100.0 + COUNTDOWN, true);
    assert_eq!(g.phase, Phase::Run);
    (g, lv(id), 100.0 + COUNTDOWN)
}

/// Run uid along the route; every frame must be accepted.
fn run(g: &mut Plat, uid: &str, t0: f64, until: Option<f64>) -> f64 {
    let l = g.lv().unwrap();
    let slot = g.player(uid).unwrap().slot;
    let path = Path::new(&l, l.spawns[slot], 5.5);
    let end = until.map(|u| u.min(path.total)).unwrap_or(path.total);
    let mut t = 0.0;
    while g.player(uid).unwrap().fin.is_none() {
        t = end.min(t + 0.05);
        let (ok, fix) = g.pos(uid, &fr(path.at(t), None), t0 + t);
        assert!(ok && fix.is_none(), "{uid} {t}");
        if t >= end {
            break;
        }
    }
    t0 + t
}

fn set_pos(g: &mut Plat, uid: &str, p: [f64; 3]) {
    let pl = g.player_mut(uid).unwrap();
    pl.x = cm(p[0]);
    pl.y = cm(p[1]);
    pl.z = cm(p[2]);
    pl.base = p[1];
    pl.air = None;
}

#[test]
fn start_rules() {
    let mut g = Plat::new();
    assert_eq!(g.start(&[], &json!("nope"), &json!("race"), 0.0).as_deref(), Some("pick a level"));
    assert_eq!(g.start(&members(1, "a"), &json!("meadow"), &json!("golf"), 0.0).as_deref(), Some("pick race or co-op"));
    assert_eq!(g.start(&members(1, "a"), &json!("meadow"), &Value::Null, 0.0), None);
    assert_eq!(g.mode, "race");
    assert!(g.start(&members(1, "a"), &json!("meadow"), &json!("coop"), 0.0).is_some());
    g.end();
    g.start(&members(12, "u"), &json!("sky"), &json!("coop"), 0.0);
    assert_eq!(g.players.len(), MAX_PLAYERS);
    let v = g.view(0.0);
    assert_eq!(v["goInMs"], 4000);
    assert_eq!(v["goal"], lv("sky").goal);
}

#[test]
fn chars_are_picked_and_kept() {
    let mut g = Plat::new();
    assert_eq!(g.char("a0", &json!({"char": 5})), Some(5));
    for bad in [json!({"char": 6}), json!({"char": -1}), json!({"char": 1.5}), json!({"char": true}), json!({})] {
        assert_eq!(g.char("a0", &bad), None);
    }
    g.start(&members(2, "a"), &json!("meadow"), &json!("race"), 0.0);
    assert_eq!((g.player("a0").unwrap().chr, g.player("a1").unwrap().chr), (5, 1));
}

#[test]
fn frames_before_the_start_are_ignored() {
    let mut g = Plat::new();
    g.start(&members(1, "a"), &json!("meadow"), &json!("race"), 0.0);
    assert_eq!(g.pos("a0", &fr(lv("meadow").spawns[0], None), 1.0), (false, None));
    assert_eq!(g.tick(COUNTDOWN, true)[0].0, "go");
}

#[test]
fn a_full_race_run_finishes() {
    let (mut g, l, t) = started(1, "meadow", "race");
    let t = run(&mut g, "u0", t, None);
    let p = g.player("u0").unwrap();
    assert!(p.fin.is_some() && p.cp == l.cps.len() && p.bad == 0 && p.got.len() >= l.coins.len() / 2);
    let evs = g.tick(t + 0.01, true);
    let names: Vec<&str> = evs.iter().map(|e| e.0).collect();
    assert_eq!(names.iter().filter(|n| **n == "cp").count(), l.cps.len());
    assert!(names.contains(&"coin") && names.contains(&"finish") && *names.last().unwrap() == "done");
    assert_eq!(g.results.as_ref().unwrap()[0]["place"], 1);
}

#[test]
fn checkpoints_count_only_in_order_and_the_flag_needs_them_all() {
    let (mut g, l, t) = started(1, "meadow", "race");
    let c1 = l.cps[1];
    set_pos(&mut g, "u0", c1);
    assert!(g.pos("u0", &fr([c1[0] + 0.1, c1[1], c1[2]], None), t + 0.1).0);
    assert_eq!(g.player("u0").unwrap().cp, 0);
    let c0 = l.cps[0];
    set_pos(&mut g, "u0", c0);
    assert!(g.pos("u0", &fr([c0[0], c0[1], c0[2] - 0.1], None), t + 0.2).0);
    assert_eq!(g.player("u0").unwrap().cp, 1);
    let fl = l.flag;
    set_pos(&mut g, "u0", [fl[0], fl[1], fl[2] + 1.0]);
    assert!(g.pos("u0", &fr(fl, None), t + 0.3).0);
    assert!(g.player("u0").unwrap().fin.is_none());
    g.player_mut("u0").unwrap().cp = l.cps.len();
    assert!(g.pos("u0", &fr([fl[0], fl[1], fl[2] + 0.2], None), t + 0.4).0);
    assert!(g.player("u0").unwrap().fin.is_some());
}

#[test]
fn coop_coins_are_taken_once_for_the_room() {
    let (mut g, _, t) = started(2, "meadow", "coop");
    let t1 = run(&mut g, "u0", t, Some(3.0));
    let t2 = run(&mut g, "u1", t, Some(3.0));
    let (a, b) = (g.player("u0").unwrap().got.clone(), g.player("u1").unwrap().got.clone());
    assert!(!a.is_empty() && a.is_disjoint(&b));
    assert_eq!(g.taken.len(), a.len() + b.len());
    let rooms: Vec<Value> = g.tick(t1.max(t2) + 0.01, true).into_iter().filter(|e| e.0 == "coin").map(|e| e.1["room"].clone()).collect();
    assert_eq!(rooms, (1..=g.taken.len()).map(|n| json!(n)).collect::<Vec<_>>());
}

#[test]
fn race_coins_count_once_per_player() {
    let (mut g, _, t) = started(2, "meadow", "race");
    run(&mut g, "u0", t, Some(3.0));
    run(&mut g, "u1", t, Some(3.0));
    let (a, b) = (g.player("u0").unwrap().got.clone(), g.player("u1").unwrap().got.clone());
    assert!(!a.is_empty() && !a.is_disjoint(&b));
    let n = a.len();
    let evs = g.tick(t + 4.0, true);
    assert_eq!(evs.iter().filter(|e| e.0 == "coin" && e.1["user"] == "u0").count(), n);
    assert!(evs.iter().filter(|e| e.0 == "coin").all(|e| e.1.get("room").is_none()));
}

#[test]
fn coop_wins_at_the_goal_or_loses_at_the_time_limit() {
    let (mut g, l, t) = started(1, "meadow", "coop");
    g.taken = (0..l.goal as usize - 1).collect();
    g.tick(t + 1.0, true);
    assert_eq!(g.phase, Phase::Run);
    g.taken.push(l.goal as usize - 1);
    let evs = g.tick(t + 2.0, true);
    assert_eq!(evs.last().unwrap().0, "done");
    assert_eq!(evs.last().unwrap().1["win"], true);
    let (mut g2, l2, t2) = started(1, "meadow", "coop");
    let evs = g2.tick(t2 + l2.secs, true);
    assert_eq!(evs.last().unwrap().1["win"], false);
}

#[test]
fn teleports_flying_and_hovering_are_refused_with_a_fix() {
    let (mut g, l, t) = started(1, "meadow", "race");
    let t = run(&mut g, "u0", t, Some(1.0));
    let p = g.player("u0").unwrap().clone();
    let (ok, fix) = g.pos("u0", &fr([p.x as f64 / 100.0, p.y as f64 / 100.0, p.z as f64 / 100.0 - 6.0], None), t + 0.05);
    assert!(!ok && fix.is_some());
    assert_eq!(fix.unwrap()["x"], cm(p.safe[0]));
    let p = g.player("u0").unwrap().clone();
    let (sx, sy, sz) = (p.x as f64 / 100.0, p.y as f64 / 100.0, p.z as f64 / 100.0);
    let mut caught = false;
    for k in 1..21 {
        if !g.pos("u0", &fr([sx, sy + k as f64 * 0.25, sz], None), t + 0.6 + k as f64 * 0.05).0 {
            caught = true;
            break;
        }
    }
    assert!(caught && g.player("u0").unwrap().bad == 2);
    let s = l.solids[0].clone();
    set_pos(&mut g, "u0", [s.cx, s.y1, s.z1 - 0.1]);
    g.player_mut("u0").unwrap().fix_at = 0.0;
    let mut caught = None;
    for k in 1..60 {
        if !g.pos("u0", &fr([s.cx, s.y1 + 0.4, s.z1 + k as f64 * 0.25], None), t + 5.0 + k as f64 * 0.05).0 {
            caught = Some(k as f64 * 0.05);
            break;
        }
    }
    assert!(caught.unwrap() < 2.0);
}

#[test]
fn inside_a_platform_is_refused() {
    let (mut g, l, t) = started(1, "meadow", "race");
    let s = &l.solids[0];
    let p = g.player("u0").unwrap().clone();
    let (ok, fix) = g.pos("u0", &fr([p.x as f64 / 100.0, s.y1 - 0.3, p.z as f64 / 100.0], None), t + 0.1);
    assert!(!ok && fix.is_some());
}

#[test]
fn a_fast_clock_buys_no_speed() {
    let (mut g, l, mut t) = started(1, "meadow", "race");
    let path = Path::new(l, l.spawns[0], 5.5);
    let (mut q, mut rt) = (10000i64, 0.0);
    for _ in 0..30 {
        rt += 0.05;
        t += 0.05;
        q += 5;
        assert!(g.pos("u0", &fr(path.at(rt), Some(q)), t).0);
    }
    let mut caught = false;
    for _ in 0..60 {
        rt += 0.1;
        t += 0.05;
        q += 10;
        let (ok, fix) = g.pos("u0", &fr(path.at(rt), Some(q)), t);
        if !ok {
            caught = fix.is_some();
            break;
        }
    }
    assert!(caught);
    assert_eq!(g.player("u0").unwrap().bad, 1);
}

#[test]
fn stale_malformed_and_flooding_frames_are_not_cheating() {
    let (mut g, l, t) = started(1, "meadow", "race");
    let s = l.spawns[0];
    assert!(g.pos("u0", &fr([s[0], s[1], s[2] - 0.3], Some(500)), t + 0.05).0);
    assert_eq!(g.pos("u0", &fr([s[0], s[1], s[2] - 0.2], Some(495)), t + 0.06), (false, None));
    assert_eq!(g.pos("u0", &fr([s[0], s[1], s[2] - 0.3], Some(500)), t + 0.07), (false, None));
    for bad in [json!({}), json!({"x": "1", "y": 0, "z": 0, "r": 0}), json!({"x": true, "y": 0, "z": 0, "r": 0})] {
        assert_eq!(g.pos("u0", &bad, t + 0.1), (false, None));
    }
    assert_eq!(g.player("u0").unwrap().bad, 0);
    let took = (0..50).filter(|_| g.pos("u0", &fr(s, None), t + 1.0).0).count();
    assert!(took as f64 <= POS_BURST);
}

#[test]
fn a_stalling_tab_in_slow_motion_is_never_refused() {
    // A tab rendering at a few frames a second clamps its physics step, so its runner
    // moves in slow motion; it sends its simulation clock as q, so every ragged frame is
    // still honest. (With the wall clock as q the descent would outlast the envelope.)
    let mut rng = StdRng::seed_from_u64(3);
    for l in LEVELS.iter() {
        for wall_q in [false, true] {
            let mut g = Plat::new();
            g.start(&members(1, "a"), &json!(l.id), &json!("race"), 0.0);
            g.tick(COUNTDOWN, true);
            let path = Path::new(l, l.spawns[0], 5.5);
            let (mut wall, mut sim) = (COUNTDOWN, 0.0);
            while g.player("a0").unwrap().fin.is_none() && sim < path.total {
                let gap: f64 = rng.gen_range(0.14..0.45);
                wall += gap;
                sim = path.total.min(sim + gap.min(0.1) * rng.gen_range(0.8..1.0));
                let q = 777 + ((if wall_q { wall - COUNTDOWN } else { sim }) * 100.0) as i64;
                let (ok, fix) = g.pos("a0", &fr(path.at(sim), Some(q)), wall);
                if !wall_q {
                    assert!(ok && fix.is_none(), "{} {sim}", l.id);
                }
            }
            let p = g.player("a0").unwrap();
            if wall_q {
                assert!(p.bad > 0, "{}: the wall clock is what made slow motion look like hovering", l.id);
            } else {
                assert!(p.fin.is_some() && p.bad == 0);
            }
        }
    }
}

#[test]
fn respawn_goes_to_the_last_checkpoint() {
    let (mut g, l, t) = started(1, "meadow", "race");
    let sp = g.respawn("u0", t + 0.1).unwrap();
    assert_eq!((sp["x"].as_i64().unwrap(), sp["cp"].as_i64().unwrap()), (cm(l.spawns[0][0]), 0));
    assert!(g.respawn("u0", t + 0.2).is_none());
    let path = Path::new(l, l.spawns[0], 5.5);
    let (mut rt, mut tt) = (0.0, t + 1.0);
    while g.player("u0").unwrap().cp < 1 {
        rt += 0.05;
        tt += 0.05;
        assert!(g.pos("u0", &fr(path.at(rt), None), tt).0);
    }
    let sp = g.respawn("u0", tt + 2.0).unwrap();
    let c = l.cps[0];
    assert_eq!((sp["x"].as_i64().unwrap(), sp["y"].as_i64().unwrap()), (cm(c[0]), cm(c[1])));
    assert!(g.pos("u0", &fr([c[0], c[1], c[2] - 0.2], None), tt + 2.1).0);
}

#[test]
fn standings_and_snapshots_only_carry_what_moved() {
    let (mut g, l, t) = started(3, "meadow", "race");
    run(&mut g, "u2", t, Some(4.0));
    let evs = g.tick(t + 4.1, true);
    let snap = &evs.iter().find(|e| e.0 == "snap").unwrap().1;
    assert_eq!(snap["order"][0], "u2");
    assert_eq!(snap["ps"].as_array().unwrap().len(), 1);
    assert!(!g.tick(t + 4.2, true).iter().any(|e| e.0 == "snap"));
    let s = l.spawns[0];
    g.pos("u0", &fr([s[0], s[1], s[2] - 0.2], None), t + 4.3);
    assert!(!g.tick(t + 4.35, false).iter().any(|e| e.0 == "snap"));
    let evs = g.tick(t + 4.4, true);
    let snap = &evs.iter().find(|e| e.0 == "snap").unwrap().1;
    assert_eq!(snap["ps"][0]["u"], "u0");
    assert!(snap["ps"][0].get("a").is_some() && snap["ps"][0].get("y").is_some());
}

#[test]
fn a_dropped_player_keeps_their_place_for_the_grace_then_dnf() {
    let (mut g, _, t) = started(2, "meadow", "race");
    assert!(g.drop("u1", t, true));
    assert!(g.restore("u1", pubv("u1")));
    g.drop("u1", t, true);
    assert!(g.tick(t + GRACE + 0.1, true).contains(&("dnf", json!({"user": "u1"}))));
    assert!(!g.restore("u1", pubv("u1")));
    g.drop("u0", t + GRACE + 0.2, false);
    assert_eq!(g.tick(t + GRACE + 0.3, true).last().unwrap().0, "done");
}

#[test]
fn finish_grace_and_the_time_limit() {
    let (mut g, _, t) = started(2, "meadow", "race");
    let t = run(&mut g, "u0", t, None);
    g.tick(t, true);
    assert_eq!(g.phase, Phase::Run);
    assert_eq!(g.tick(t + FINISH_GRACE + 0.01, true).last().unwrap().0, "done");
    let res = g.results.as_ref().unwrap();
    assert_eq!((res[0]["user"]["userId"].as_str(), res[1]["dnf"].as_bool()), (Some("u0"), Some(true)));
    let (mut g2, _, t2) = started(2, "meadow", "race");
    g2.tick(t2 + MAX_RUN_SECS - 1.0, true);
    assert_eq!(g2.phase, Phase::Run);
    assert_eq!(g2.tick(t2 + MAX_RUN_SECS, true).last().unwrap().0, "done");
}

// ------------------------------------------ 8 runners under simulated lag --
fn gauss(rng: &mut StdRng, mu: f64, sigma: f64) -> f64 {
    let u1: f64 = 1.0 - rng.gen::<f64>();
    let u2: f64 = rng.gen();
    mu + sigma * (-2.0 * u1.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos()
}

struct InFlight(f64, u64, usize, Value);
impl PartialEq for InFlight {
    fn eq(&self, o: &Self) -> bool {
        self.0 == o.0 && self.1 == o.1
    }
}
impl Eq for InFlight {}
impl PartialOrd for InFlight {
    fn partial_cmp(&self, o: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(o))
    }
}
impl Ord for InFlight {
    fn cmp(&self, o: &Self) -> std::cmp::Ordering {
        o.0.partial_cmp(&self.0).unwrap().then(o.1.cmp(&self.1))
    }
}

/// Eight bots run the route at different speeds through a 120 ms +- 60 ms link.
fn lag_run(id: &str, mode: &str, seed: u64) -> (Plat, Vec<Option<f64>>, Vec<Value>) {
    let mut rng = StdRng::seed_from_u64(seed);
    let l = lv(id);
    let mut g = Plat::new();
    let t0 = 1000.0;
    assert_eq!(g.start(&members(8, "bot"), &json!(id), &json!(mode), t0), None);
    let go = t0 + COUNTDOWN;
    let paths: Vec<Path> = (0..8).map(|i| Path::new(l, l.spawns[i], 5.9 - i as f64 * 0.3)).collect();
    let mut flight: BinaryHeap<InFlight> = BinaryHeap::new();
    let (mut seq, mut t, mut next_tick) = (0u64, t0, t0);
    let mut snap_bytes: Vec<(f64, usize)> = Vec::new();
    let mut true_finish: Vec<Option<f64>> = vec![None; 8];
    let mut coin_evs = Vec::new();
    let mut last_q = [-1i64; 8];
    let mut out_of_order = 0;
    while t < go + 300.0 && g.phase != Phase::Done {
        t = ((t + 0.05) * 1e6).round() / 1e6;
        if t >= go {
            for i in 0..8 {
                if true_finish[i].is_some() {
                    continue;
                }
                let rt = (t - go).min(paths[i].total);
                let p = paths[i].at(rt);
                if rt > paths[i].total / 2.0 && dist(p, l.flag) <= FLAG_R {
                    true_finish[i] = Some(t);
                }
                let delay = gauss(&mut rng, 0.12, 0.06).clamp(0.0, 0.24);
                seq += 1;
                flight.push(InFlight(t + delay, seq, i, fr(p, Some((t * 100.0).round() as i64))));
            }
        }
        while flight.peek().map(|f| f.0 <= t).unwrap_or(false) {
            let InFlight(at, _, i, f) = flight.pop().unwrap();
            let q = f["q"].as_i64().unwrap();
            if q < last_q[i] {
                out_of_order += 1;
            }
            last_q[i] = last_q[i].max(q);
            g.pos(&format!("bot{i}"), &f, at);
        }
        while next_tick <= t {
            for (ev, data) in g.tick(next_tick, true) {
                if ev == "snap" {
                    let mut m = json!({"type": "game", "g": "plat", "ev": ev});
                    m.as_object_mut().unwrap().extend(data.as_object().unwrap().clone());
                    snap_bytes.push((next_tick, m.to_string().len()));
                }
                if ev == "coin" {
                    coin_evs.push(data);
                }
            }
            next_tick += 1.0 / HZ;
        }
    }
    assert_eq!(g.phase, Phase::Done);
    assert!(out_of_order > 0, "the link must reorder some frames");
    assert_eq!(g.players.iter().map(|(_, p)| p.bad).sum::<u32>(), 0, "no false rejections");
    let mut per_sec: HashMap<i64, usize> = HashMap::new();
    for (at, n) in snap_bytes {
        *per_sec.entry(at as i64).or_default() += n * 8;
    }
    assert!(*per_sec.values().max().unwrap() < ROOM_BYTES_PER_SEC);
    (g, true_finish, coin_evs)
}

#[test]
fn eight_runners_race_with_lag_finish_in_order_with_no_false_rejections() {
    for id in ["meadow", "sky", "fortress"] {
        for seed in [7, 8] {
            let (g, tf, _) = lag_run(id, "race", seed);
            let res = g.results.as_ref().unwrap().as_array().unwrap();
            let order: Vec<String> = res.iter().map(|r| r["user"]["userId"].as_str().unwrap().to_string()).collect();
            let mut want: Vec<usize> = (0..8).collect();
            want.sort_by(|a, b| tf[*a].partial_cmp(&tf[*b]).unwrap());
            let want: Vec<String> = want.into_iter().map(|i| format!("bot{i}")).collect();
            assert_eq!(order, want, "{id} {seed}");
            assert!(res.iter().all(|r| r["dnf"] == false));
        }
    }
}

#[test]
fn eight_runners_coop_with_lag_count_each_coin_once() {
    for id in ["meadow", "sky", "fortress"] {
        let (g, _, evs) = lag_run(id, "coop", 9);
        assert_eq!(g.win, Some(true));
        let ids: Vec<i64> = evs.iter().map(|e| e["id"].as_i64().unwrap()).collect();
        let uniq: std::collections::HashSet<i64> = ids.iter().copied().collect();
        assert_eq!((ids.len(), uniq.len()), (g.taken.len(), g.taken.len()));
        let rooms: Vec<i64> = evs.iter().map(|e| e["room"].as_i64().unwrap()).collect();
        assert_eq!(rooms, (1..=ids.len() as i64).collect::<Vec<_>>());
        assert_eq!(g.players.iter().map(|(_, p)| p.got.len()).sum::<usize>(), g.taken.len());
    }
}

#[test]
fn eight_runner_tick_is_cheap() {
    let (mut g, l, t) = started(8, "meadow", "race");
    let paths: Vec<Path> = (0..8).map(|i| Path::new(l, l.spawns[i], 5.0)).collect();
    let start = std::time::Instant::now();
    for f in 0..20 * 30 {
        let tt = t + f as f64 * 0.05;
        for (i, p) in paths.iter().enumerate() {
            g.pos(&format!("u{i}"), &fr(p.at(f as f64 * 0.05), None), tt);
        }
        if f % 2 == 0 {
            g.tick(tt, true);
        }
    }
    let per_second = start.elapsed().as_secs_f64() / 30.0;
    assert!(per_second < 0.05, "{per_second}");
}

// ------------------------------------------- through the room + lobby layer --
struct Env {
    hub: PlatHub,
    rooms: RoomManager,
    clock: Arc<Mutex<f64>>,
}

fn env(max_tickers: usize) -> Env {
    let rooms = RoomManager::new();
    let clock = Arc::new(Mutex::new(1000.0));
    let c = clock.clone();
    let hub = PlatHub::new(rooms.clone(), Registry::new(max_tickers), Arc::new(move || *c.lock().unwrap()),
        crate::results::Recorder::default());
    Env { hub, rooms, clock }
}

impl Env {
    fn advance(&self, dt: f64) {
        *self.clock.lock().unwrap() += dt;
    }
    async fn connect(&self, room: &str, conn: u64, uid: &str) -> (Member, mpsc::Receiver<String>) {
        let m = Member { user_id: uid.into(), handle: uid.into(), display_name: uid.into(), avatar_url: String::new() , cos: serde_json::Value::Null};
        let (_rx, drx, _, _) = self.rooms.join_direct(room, conn, m.clone()).await.unwrap();
        (m, drx)
    }
    async fn send(&self, room: &str, conn: u64, m: &Member, op: &str, extra: Value) {
        let mut msg = json!({"type": "game", "g": "plat", "op": op});
        if let Value::Object(e) = extra {
            msg.as_object_mut().unwrap().extend(e);
        }
        self.hub.handle(room, conn, m, &msg).await;
    }
    async fn disconnect(&self, room: &str, conn: u64, m: &Member) {
        self.hub.on_disconnect(room, conn, m).await;
        self.rooms.leave(room, conn).await;
    }
}

async fn until_where(rx: &mut mpsc::Receiver<String>, ev: &str, pick: impl Fn(&Value) -> bool) -> Value {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let m = tokio::time::timeout_at(deadline, rx.recv()).await.unwrap_or_else(|_| panic!("no {ev} event")).expect("open");
        let v: Value = serde_json::from_str(&m).unwrap();
        if v["type"] == "game" && v["g"] == "plat" && v["ev"] == ev && pick(&v) {
            return v;
        }
    }
}

async fn until(rx: &mut mpsc::Receiver<String>, ev: &str) -> Value {
    until_where(rx, ev, |_| true).await
}

#[tokio::test]
async fn race_through_the_room_and_lobby() {
    let e = env(MAX_TICKERS);
    let (a, mut wa) = e.connect("platroom", 1, "plat-a").await;
    let (b, mut wb) = e.connect("platroom", 2, "plat-b").await;
    e.send("platroom", 1, &a, "join", json!({})).await;
    assert_eq!(until(&mut wa, "lobby").await["members"][0]["host"], true);
    until(&mut wa, "plat").await;
    e.send("platroom", 2, &b, "join", json!({})).await;
    until(&mut wb, "plat").await;
    e.send("platroom", 2, &b, "start", json!({"level": "meadow", "mode": "race"})).await;
    assert_eq!(until(&mut wb, "error").await["error"], "only the host can do that");
    e.send("platroom", 1, &a, "char", json!({"char": 3})).await;
    assert_eq!(until(&mut wb, "char").await["char"], 3);
    e.send("platroom", 1, &a, "start", json!({"level": "meadow", "mode": "race"})).await;
    let run_ = until_where(&mut wb, "plat", |m| m["run"]["phase"] == "grid").await["run"].clone();
    assert_eq!(run_["players"].as_array().unwrap().len(), 2);
    assert!(e.hub.registry().get("plat:platroom").is_some());
    e.advance(COUNTDOWN + 0.01);
    until(&mut wa, "go").await;
    until(&mut wb, "go").await;
    let l = lv("meadow");
    let path = Path::new(l, l.spawns[0], 5.5);
    let mut rt = 0.0;
    while rt < path.total {
        rt = path.total.min(rt + 0.05);
        e.advance(0.05);
        e.send("platroom", 1, &a, "pos", fr(path.at(rt), None)).await;
    }
    until_where(&mut wb, "cp", |m| m["user"] == "plat-a").await;
    until_where(&mut wb, "coin", |m| m["user"] == "plat-a").await;
    assert_eq!(until_where(&mut wb, "finish", |m| m["user"] == "plat-a").await["place"], 1);
    e.send("platroom", 2, &b, "leave", json!({})).await;
    let done = until(&mut wa, "done").await;
    assert_eq!(done["results"][0]["user"]["userId"], "plat-a");
    assert_eq!(done["results"][1]["dnf"], true);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(e.hub.registry().get("plat:platroom").is_none(), "the loop stops with the run");
}

#[tokio::test]
async fn coop_respawn_and_fix_through_the_lobby() {
    let e = env(MAX_TICKERS);
    let (a, mut wa) = e.connect("platco", 1, "plat-c").await;
    e.send("platco", 1, &a, "join", json!({})).await;
    until(&mut wa, "plat").await;
    e.send("platco", 1, &a, "start", json!({"level": "sky", "mode": "coop"})).await;
    until_where(&mut wa, "plat", |m| m["run"]["phase"] == "grid").await;
    e.advance(COUNTDOWN + 0.01);
    until(&mut wa, "go").await;
    let s = lv("sky").spawns[0];
    e.send("platco", 1, &a, "pos", fr([s[0], s[1], s[2] - 9.0], None)).await;
    let fix = until(&mut wa, "fix").await;
    assert!(fix.get("x").is_some() && fix.get("y").is_some() && fix.get("z").is_some());
    e.advance(1.0);
    e.send("platco", 1, &a, "respawn", json!({})).await;
    let sp = until(&mut wa, "spawn").await;
    assert_eq!(sp["x"].as_i64(), Some(cm(s[0])));
    e.send("platco", 1, &a, "end", json!({})).await;
    assert_eq!(until(&mut wa, "plat").await["run"]["phase"], "idle");
    assert!(e.hub.registry().get("plat:platco").is_none());
}

#[tokio::test]
async fn lobby_rules_and_a_dropped_socket() {
    let e = env(MAX_TICKERS);
    let (a, mut wa) = e.connect("plob", 1, "a").await;
    let (b, _wb) = e.connect("plob", 2, "b").await;
    e.send("plob", 1, &a, "start", json!({"level": "meadow"})).await;
    assert_eq!(until(&mut wa, "error").await["error"], "join the lobby first");
    e.send("plob", 1, &a, "join", json!({})).await;
    e.send("plob", 2, &b, "join", json!({})).await;
    e.send("plob", 1, &a, "char", json!({"char": 9})).await;
    assert_eq!(until(&mut wa, "error").await["error"], "pick a character");
    e.send("plob", 1, &a, "start", json!({"level": "meadow", "mode": "coop"})).await;
    e.advance(COUNTDOWN + 0.01);
    until(&mut wa, "go").await;
    e.disconnect("plob", 2, &b).await;
    let left = until_where(&mut wa, "plat", |m| m["left"] == "b").await;
    let pb = left["run"]["players"].as_array().unwrap().iter().find(|p| p["user"]["userId"] == "b").unwrap().clone();
    assert!(pb["away"].is_number());
    let (b2, _wb2) = e.connect("plob", 3, "b").await;
    e.send("plob", 3, &b2, "join", json!({})).await;
    assert_eq!(until(&mut wa, "lobby").await["joined"], Value::Null);
    until_where(&mut wa, "plat", |m| m["back"] == "b").await;
}

#[tokio::test]
async fn a_full_arena_says_so_to_the_host() {
    let e = env(0);
    let (a, mut wa) = e.connect("pbusy", 1, "a").await;
    e.send("pbusy", 1, &a, "join", json!({})).await;
    e.send("pbusy", 1, &a, "start", json!({"level": "meadow"})).await;
    assert_eq!(until(&mut wa, "error").await["error"], "the Arena is busy right now: try again in a minute");
    assert_eq!(e.hub.with_room("pbusy", |v| v.plat.phase), Some(Phase::Idle));
}

// ---------------------------------------------------- custom (editor) levels --
/// Three platforms in a line: walk, jump, walk over the checkpoint, jump, walk to the flag.
fn custom_data() -> Value {
    json!({
        "kill": -6, "coopGoal": 2, "coopSecs": 90,
        "theme": {"sky": "#8FD3FF", "fog": "#cdeeff", "sea": "#5aa9e6", "light": "#fff6e0"},
        "spawns": [[-1, 0, 1], [1, 0, 1]],
        "cps": [[0, 0.5, -6]],
        "flag": [0, 1, -12],
        "coins": [[0, 1.6, -3.5], [0, 2.0, -8.5]],
        "solids": [{"m": "platform-large", "x": 0, "y": -0.5, "z": 0},
                   {"m": "platform-medium", "x": 0, "y": 0, "z": -6},
                   {"m": "platform-large", "x": 0, "y": 0.5, "z": -12}],
        "route": [[0, 0, -2, "w"], [0, 0.5, -5, "j"], [0, 0.5, -7, "w"], [0, 1, -10, "j"], [0, 1, -12, "w"]],
        "deco": [{"m": "cloud", "x": 8, "y": 6, "z": -6, "r": 0, "s": 3.2}],
        "junk": "dropped"
    })
}

fn doc(data: Value) -> Value {
    json!({"kind": "plat", "v": 1, "name": "  Three Hops  ", "data": data})
}

fn custom_started(n: usize, mode: &str) -> (Plat, f64) {
    let mut g = Plat::new();
    assert_eq!(g.start_with(&members(n, "u"), &json!("custom"), &json!(mode), Some(&doc(custom_data())), 100.0), None);
    g.tick(100.0 + COUNTDOWN, true);
    assert_eq!(g.phase, Phase::Run);
    (g, 100.0 + COUNTDOWN)
}

fn with(mut data: Value, f: impl FnOnce(&mut Value)) -> Value {
    f(&mut data);
    data
}

#[test]
fn the_built_in_levels_pass_the_custom_validator() {
    let file: Value = serde_json::from_str(LEVELS_JSON).unwrap();
    for lvj in file["levels"].as_array().unwrap() {
        let c = validate_custom(lvj).unwrap_or_else(|e| panic!("{}: {e}", lvj["id"]));
        let l = compile_custom(&c).unwrap();
        let b = level(lvj["id"].as_str().unwrap()).unwrap();
        assert_eq!((l.solids.len(), l.coins.len(), l.cps.len(), l.route.len()), (b.solids.len(), b.coins.len(), b.cps.len(), b.route.len()));
        assert_eq!(l.bounds, b.bounds);
        // the canonical form is a fixed point
        assert_eq!(validate_custom(&c).unwrap(), c);
    }
}

#[test]
fn custom_data_comes_back_canonical() {
    let c = validate_custom(&custom_data()).unwrap();
    let keys: Vec<&String> = c.as_object().unwrap().keys().collect();
    assert_eq!(keys, ["kill", "coopGoal", "coopSecs", "theme", "spawns", "cps", "flag", "coins", "solids", "route", "deco"]);
    assert_eq!(c["theme"]["sky"], "#8fd3ff");
    assert!(c.get("junk").is_none());
    assert_eq!(c["solids"][0], json!({"m": "platform-large", "x": 0, "y": -0.5, "z": 0}));
    assert_eq!(c["deco"][0]["s"], json!(3.2));
    assert_eq!(c.to_string(), serde_json::to_string(&c).unwrap());
    // 1.004 and 1.0 are the same level, so the same key
    let a = with(custom_data(), |d| d["coins"][0][0] = json!(1.004));
    let b = with(custom_data(), |d| d["coins"][0][0] = json!(1.0));
    let (ca, cb) = (validate_custom(&a).unwrap(), validate_custom(&b).unwrap());
    assert_eq!(ca, cb);
    assert_eq!(custom_key(&ca), custom_key(&cb));
    let k = custom_key(&ca);
    assert!(k.len() == 14 && k.starts_with("c-") && k[2..].chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()), "{k}");
    assert_ne!(custom_key(&c), k);
    // the documented rule: sha256("plat:" + compact canonical JSON)
    use sha2::{Digest, Sha256};
    let hex = hex::encode(Sha256::digest(format!("plat:{}", serde_json::to_string(&ca).unwrap())));
    assert_eq!(k, format!("c-{}", &hex[..12]));
    assert_eq!(round2(-0.125), -0.13);
    assert_eq!(round2(0.125), 0.13);
    assert_eq!(round2(-0.001).to_string(), "0");
}

#[test]
fn bad_custom_levels_are_refused_without_a_panic() {
    let cases: Vec<(&str, Value, &str)> = vec![
        ("unknown model", with(custom_data(), |d| d["solids"][1]["m"] = json!("castle")), "unknown model 'castle'"),
        ("no spawns", with(custom_data(), |d| d["spawns"] = json!([])), "spawns: 1 to 8"),
        ("9 spawns", with(custom_data(), |d| d["spawns"] = json!(vec![json!([0, 0, 1]); 9])), "spawns: 1 to 8"),
        ("spawn in the air", with(custom_data(), |d| d["spawns"][1] = json!([1, 3, 1])), "spawn 2 isn't standing"),
        ("NaN coordinate", with(custom_data(), |d| d["solids"][0]["x"] = json!(f64::NAN)), "not a number"),
        ("1e9 coordinate", with(custom_data(), |d| d["coins"][0][2] = json!(1e9)), "out of range"),
        ("65 solids", with(custom_data(), |d| d["solids"] = json!(vec![json!({"m": "brick", "x": 0, "y": -9, "z": 0}); 65])), "solids: 1 to 64"),
        ("unreachable gap", with(custom_data(), |d| {
            d["solids"][2]["z"] = json!(-16);
            d["route"] = json!([[0, 0, -2, "w"], [0, 0.5, -5, "j"], [0, 0.5, -7, "w"], [0, 1, -14, "j"], [0, 1, -16, "w"]]);
            d["flag"] = json!([0, 1, -16]);
        }), "too far to jump"),
        ("w point floating 3 m up", with(custom_data(), |d| d["route"][2] = json!([0, 3.5, -7, "w"])), "walks onto nothing"),
        ("j landing in the air", with(custom_data(), |d| d["route"][1] = json!([5, 0.5, -5, "j"])), "lands in the air"),
        ("coopGoal > coins", with(custom_data(), |d| d["coopGoal"] = json!(3)), "co-op goal must be"),
        ("no flag", with(custom_data(), |d| { d.as_object_mut().unwrap().remove("flag"); }), "flag is missing"),
        ("route misses the checkpoint", with(custom_data(), |d| d["cps"][0] = json!([2.5, 0, 2])), "misses checkpoint 1"),
        ("route not at the flag", with(custom_data(), |d| d["flag"] = json!([2, 1, -10.5])), "doesn't end at the flag"),
        ("bad route kind", with(custom_data(), |d| d["route"][0][3] = json!("x")), "kind must be"),
        ("one route point", with(custom_data(), |d| d["route"] = json!([[0, 1, -12, "w"]])), "route: 2 to 200"),
        ("walk across a gap", with(custom_data(), |d| d["route"][1][3] = json!("w")), "more than 0.3 m"),
        ("co-op secs", with(custom_data(), |d| d["coopSecs"] = json!(10)), "co-op time"),
        ("theme", with(custom_data(), |d| d["theme"]["sea"] = json!("blue")), "theme sea"),
        ("rotation", with(custom_data(), |d| d["solids"][0]["r"] = json!(45)), "turn must be"),
        ("scale", with(custom_data(), |d| d["solids"][0]["s"] = json!(9)), "scale must be"),
        ("deco model", with(custom_data(), |d| d["deco"][0]["m"] = json!("platform")), "decoration 1"),
        ("bool number", with(custom_data(), |d| d["kill"] = json!(true)), "kill height is not a number"),
        ("kill above the floor", with(custom_data(), |d| d["kill"] = json!(0)), "kill height must be"),
        ("coin in a block", with(custom_data(), |d| d["coins"][0] = json!([0, -0.3, 0])), "coin 1 is inside"),
        ("walk over a flat gap", with(custom_data(), |d| {
            d["solids"][1]["y"] = json!(-0.5);
            d["cps"][0] = json!([0, 0, -6]);
            d["route"][1] = json!([0, 0, -5, "w"]);
            d["route"][2] = json!([0, 0, -7, "w"]);
        }), "walks over a gap"),
        ("walk into a block", with(custom_data(), |d| {
            d["solids"].as_array_mut().unwrap().push(json!({"m": "brick", "x": 0, "y": -0.3, "z": -1}));
        }), "walks into a block"),
        ("jump through a block", with(custom_data(), |d| {
            d["solids"].as_array_mut().unwrap().push(json!({"m": "brick", "x": 0, "y": 0.6, "z": -3.5}));
        }), "jumps through a platform"),
        ("too high even for a double jump", with(custom_data(), |d| {
            d["solids"][1]["y"] = json!(3.5);
            d["cps"][0] = json!([0, 4, -6]);
            d["route"][1] = json!([0, 4, -5, "d"]);
            d["route"][2] = json!([0, 4, -7, "w"]);
        }), "too high even for a double jump"),
        ("not an object", json!([1, 2, 3]), "not an object"),
        ("null", Value::Null, "not an object"),
    ];
    for (what, data, why) in cases {
        let r = std::panic::catch_unwind(|| (validate_custom(&data), compile_custom(&data).map(|_| ())));
        let (v, c) = r.unwrap_or_else(|_| panic!("{what} panicked"));
        assert!(v.is_err() && c.is_err(), "{what} passed");
        let e = v.unwrap_err();
        assert!(e.contains(why), "{what}: {e}");
    }
    // the reasons are in plain words
    let gap = with(custom_data(), |d| {
        d["solids"][2]["z"] = json!(-16);
        d["route"] = json!([[0, 0, -2, "w"], [0, 0.5, -5, "j"], [0, 0.5, -7, "w"], [0, 1, -14, "j"], [0, 1, -16, "w"]]);
        d["flag"] = json!([0, 1, -16]);
    });
    assert_eq!(validate_custom(&gap).unwrap_err(), "route point 4 is too far to jump: try a double jump");
    assert_eq!(validate_custom(&with(custom_data(), |d| d["route"][1] = json!([5, 0.5, -5, "j"]))).unwrap_err(),
               "route point 2 lands in the air");
    // a giant blob is refused before any parsing
    let big = with(custom_data(), |d| d["deco"] = json!(vec![json!({"m": "cloud", "x": 1.23, "y": 4.56, "z": 7.89, "r": 123, "s": 3.21}); 400]));
    assert!(validate_custom(&big).is_err());
}

#[test]
fn custom_level_docs_need_the_right_kind_name_and_version() {
    let d = doc(custom_data());
    let c = custom_of(&d).unwrap();
    assert_eq!(c.name, "Three Hops");
    assert_eq!(c.lv.id, c.key);
    for (bad, why) in [
        (with(d.clone(), |m| m["kind"] = json!("kart")), "that isn't a platformer level"),
        (with(d.clone(), |m| m["v"] = json!(2)), "that level is from a newer editor"),
        (with(d.clone(), |m| m["name"] = json!("   ")), "the name must be 1 to 32 characters"),
        (with(d.clone(), |m| m["name"] = json!("a".repeat(33))), "the name must be 1 to 32 characters"),
        (with(d.clone(), |m| m["name"] = json!("<b>hi</b>")), "the name has characters that aren't allowed"),
        (with(d.clone(), |m| m["name"] = json!("see HTTPS site")), "the name has characters that aren't allowed"),
        (with(d.clone(), |m| m["name"] = json!("tab\there")), "the name has characters that aren't allowed"),
        (Value::Null, "no level sent"),
    ] {
        assert_eq!(custom_of(&bad).unwrap_err(), why);
    }
}

#[test]
fn a_custom_level_races_and_following_its_route_finishes() {
    let (mut g, t) = custom_started(2, "race");
    let key = g.level.clone().unwrap();
    assert!(key.starts_with("c-") && key.len() == 14);
    let v = g.view(t);
    assert_eq!(v["level"], json!(key));
    assert_eq!(v["custom"]["name"], "Three Hops");
    assert_eq!(v["custom"]["data"], validate_custom(&custom_data()).unwrap());
    let l = g.lv().unwrap();
    assert!(matches!(l, Lv::Custom(_)));
    let t1 = run(&mut g, "u0", t, None);
    let p = g.player("u0").unwrap();
    assert!(p.fin.is_some() && p.cp == 1 && p.bad == 0 && p.got.len() == 2, "{:?}", p);
    let t2 = run(&mut g, "u1", t, None);
    let evs = g.tick(t1.max(t2) + 0.01, true);
    assert!(evs.iter().all(|e| e.0 != "snap" || e.1.get("custom").is_none()));
    let done = &evs.last().unwrap().1;
    assert_eq!(evs.last().unwrap().0, "done");
    assert_eq!(done["level"], json!(key));
    let mut d = done.clone();
    d["ev"] = json!("done");
    d["g"] = json!("plat");
    let rows = crate::results::rows_from_done("plat", &d);
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|r| r.key == key && r.mode == "race"));
    // respawn goes back to the custom checkpoint
    let (mut g, t) = custom_started(1, "race");
    run(&mut g, "u0", t, Some(2.0));
    assert_eq!(g.player("u0").unwrap().cp, 1);
    let sp = g.respawn("u0", t + 3.0).unwrap();
    assert_eq!((sp["x"].clone(), sp["y"].clone(), sp["z"].clone()), (json!(0), json!(50), json!(-600)));
}

#[test]
fn a_custom_level_plays_coop_to_its_goal() {
    let (mut g, t) = custom_started(1, "coop");
    assert_eq!(g.limit(), 90.0);
    let t1 = run(&mut g, "u0", t, None);
    let evs = g.tick(t1 + 0.01, true);
    assert_eq!(evs.last().unwrap().0, "done");
    assert_eq!(evs.last().unwrap().1["win"], true);
    assert_eq!(evs.last().unwrap().1["goal"], 2);
}

#[test]
fn custom_starts_are_checked_and_a_built_in_start_clears_the_custom_level() {
    let mut g = Plat::new();
    let gap = with(custom_data(), |d| d["route"][1] = json!([5, 0.5, -5, "j"]));
    assert_eq!(g.start_with(&members(1, "a"), &json!("custom"), &json!("race"), Some(&doc(gap)), 0.0).as_deref(),
               Some("bad level: route point 2 lands in the air"));
    assert_eq!(g.start_with(&members(1, "a"), &json!("custom"), &json!("race"), None, 0.0).as_deref(),
               Some("bad level: no level sent"));
    assert!(g.custom.is_none() && g.level.is_none());
    let nocoins = with(custom_data(), |d| { d["coins"] = json!([]); d["coopGoal"] = json!(0); });
    assert_eq!(g.start_with(&members(1, "a"), &json!("custom"), &json!("coop"), Some(&doc(nocoins.clone())), 0.0).as_deref(),
               Some("this level has no co-op coin goal: race it instead"));
    assert_eq!(g.start_with(&members(1, "a"), &json!("custom"), &json!("race"), Some(&doc(nocoins)), 0.0), None);
    assert!(g.custom.is_some());
    g.end();
    assert!(g.start(&members(1, "a"), &json!("meadow"), &json!("race"), 0.0).is_none());
    assert!(g.custom.is_none());
    let v = g.view(0.0);
    assert_eq!(v["level"], "meadow");
    assert!(v.get("custom").is_none());
}

#[tokio::test]
async fn a_custom_race_through_the_room_records_the_c_key() {
    let e = env(MAX_TICKERS);
    let (a, mut wa) = e.connect("pcustom", 1, "plat-a").await;
    let (b, mut wb) = e.connect("pcustom", 2, "plat-b").await;
    e.send("pcustom", 1, &a, "join", json!({})).await;
    until(&mut wa, "plat").await;
    e.send("pcustom", 2, &b, "join", json!({})).await;
    until(&mut wb, "plat").await;
    let bad = doc(with(custom_data(), |d| d["solids"][0]["m"] = json!("castle")));
    e.send("pcustom", 1, &a, "start", json!({"level": "custom", "mode": "race", "custom": bad})).await;
    assert_eq!(until(&mut wa, "error").await["error"], "bad level: platform 1: unknown model 'castle'");
    e.send("pcustom", 1, &a, "start", json!({"level": "custom", "mode": "race", "custom": doc(custom_data())})).await;
    let started = until_where(&mut wb, "plat", |m| m["run"]["phase"] == "grid").await["run"].clone();
    let key = started["level"].as_str().unwrap().to_string();
    assert!(key.starts_with("c-"));
    assert_eq!(started["custom"]["name"], "Three Hops");
    // a late "view" gets the level too
    e.send("pcustom", 2, &b, "view", json!({})).await;
    assert_eq!(until(&mut wb, "plat").await["run"]["custom"]["data"]["flag"], json!([0, 1, -12]));
    e.advance(COUNTDOWN + 0.01);
    until(&mut wa, "go").await;
    let l = compile_custom(&custom_data()).unwrap();
    let path = Path::new(&l, l.spawns[0], 5.5);
    let mut rt = 0.0;
    while rt < path.total {
        rt = path.total.min(rt + 0.05);
        e.advance(0.05);
        e.send("pcustom", 1, &a, "pos", fr(path.at(rt), None)).await;
    }
    assert_eq!(until_where(&mut wb, "finish", |m| m["user"] == "plat-a").await["place"], 1);
    let snap = until(&mut wb, "snap").await;
    assert!(snap.get("custom").is_none());
    e.send("pcustom", 2, &b, "leave", json!({})).await;
    let done = until(&mut wa, "done").await;
    assert_eq!(done["level"], json!(key));
    assert!(done.get("custom").is_none());
    assert_eq!(crate::results::rows_from_done("plat", &done)[0].key, key);
}

/// tests/test_leveledit.py pins the same keys from games/leveledit.js's canonical form, so the
/// editor and the Arena agree byte for byte on what a level is.
#[test]
fn custom_keys_are_pinned_for_the_editor() {
    let mut got = vec![custom_key(&validate_custom(&custom_data()).unwrap())];
    let file: Value = serde_json::from_str(LEVELS_JSON).unwrap();
    for lvj in file["levels"].as_array().unwrap() {
        got.push(custom_key(&validate_custom(lvj).unwrap()));
    }
    assert_eq!(got, ["c-ca296b942375", "c-afd79d84fac1", "c-80356f2bd64f", "c-b8253b24bae3"]);
}

#[test]
fn the_welcome_tells_the_page_custom_levels_are_taken() {
    // games/platformer.js and games/leveledit.js offer custom levels only when arena.maps is set.
    assert_eq!(crate::protocol::arena_info()["maps"], json!({"v": 1}));
}
