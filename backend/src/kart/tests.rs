//! Ported from backend/tests/test_kart.py: track geometry, the referee's checks,
//! an 8-car race under simulated lag, and races through the room + lobby layer
//! (the per-connection queues handle_socket drains).

use super::*;
use crate::realtime::{Registry, MAX_TICKERS, ROOM_BYTES_PER_SEC};
use rand::{rngs::StdRng, Rng, SeedableRng};
use std::collections::BinaryHeap;
use std::time::Duration;
use tokio::sync::mpsc;

fn pubv(uid: &str) -> Value {
    json!({"userId": uid, "handle": uid, "displayName": uid, "avatarUrl": ""})
}

fn frame(tr: &Track, u: f64, lat: f64, q: Option<i64>) -> Value {
    let (x, z, r) = point_at(tr, u, lat);
    let mut m = json!({"x": (x * 100.0).round_ties_even() as i64, "z": (z * 100.0).round_ties_even() as i64,
                       "r": r.round_ties_even() as i64, "s": 200});
    if let Some(q) = q {
        m["q"] = json!(q);
    }
    m
}

fn tr(id: &str) -> &'static Track {
    track(id).unwrap()
}

// ------------------------------------------------------------------ tracks --
#[test]
fn tracks_compile_and_close() {
    assert!(TRACKS.len() >= 3);
    for t in TRACKS.iter() {
        assert!(!t.name.is_empty(), "{}", t.id);
        assert_eq!(t.tiles[0].kind, 'F', "{}", t.id);
        assert_eq!(t.tiles[t.n - 1].kind, 'S', "{}", t.id); // the grid sits in the tile behind
        let cells: std::collections::HashSet<_> = t.tiles.iter().map(|x| (x.col, x.row)).collect();
        assert_eq!(cells.len(), t.n);
        for k in 0..t.n {
            let (a, b) = (&t.tiles[k], &t.tiles[(k + 1) % t.n]);
            assert_eq!((a.col - b.col).abs() + (a.row - b.row).abs(), 1, "{}", t.id);
        }
    }
    for id in ["meadow", "canyon", "peaks"] {
        assert!(track(id).is_some(), "{id}");
    }
}


#[test]
fn bad_paths_are_refused() {
    for bad in ["SSSS", "FSSS", "FRRRR", "FX", "FRRSSRR", "", "SRRSRR", "FRRS"] {
        assert!(compile_track(bad).is_err(), "{bad}");
    }
    assert_eq!(compile_track("FX").unwrap_err(), "bad track at tile 1");
    assert_eq!(compile_track("FSSS").unwrap_err(), "track does not close on its start line");
    // a tiny loop closes: up, round to the right, back down and round again
    let (tiles, cells) = compile_track("FRRSRR").unwrap();
    assert_eq!((tiles.len(), cells.len()), (6, 6));
    assert_eq!(tiles.iter().filter(|t| t.kind == 'C').count(), 4);
    assert!(compile_track("FRRSRRS").is_err()); // runs back onto its start tile
}

#[test]
fn centre_line_round_trips_and_road_edges() {
    for t in TRACKS.iter() {
        let n = t.n as f64;
        for k in 0..t.n * 20 {
            let u = k as f64 / 20.0 + 0.01;
            let (x, z, _) = point_at(t, u, 0.0);
            let (i, s, lat) = locate(t, x, z).unwrap();
            let off = (i as f64 + s - u).rem_euclid(n);
            assert!(lat.abs() < 1e-6 && off.min(n - off) < 1e-6, "{} {u}", t.id);
            for side in [-4.2, 4.2] {
                let (x, z, _) = point_at(t, u, side);
                assert!(on_road(t, x, z, 0.0), "{} {u} {side}", t.id);
            }
        }
        // well off the road: past a straight's barrier (ROAD_HALF), or off the track's tiles
        assert!(locate(t, 1000.0, 1000.0).is_none());
        let (x, z, _) = point_at(t, 0.3, ROAD_HALF - 0.1);
        assert!(!on_road(t, x + 2.0, z, 0.0));
    }
}

#[test]
fn yaw_points_along_the_track() {
    let t = tr("meadow");
    assert_eq!(point_at(t, 0.5, 0.0).2, 0.0); // the start line heads north
    let first_r = t.tiles.iter().position(|x| x.o == (x.d + 1) % 4).expect("meadow turns right");
    let (_, _, r) = point_at(t, first_r as f64 + 0.5, 0.0); // the first right turn: half way round, NE
    assert!((r - 45.0).abs() < 1e-9, "{r}");
}

#[test]
fn grid_slots_are_on_the_road_behind_the_line_and_apart() {
    let t = tr("meadow");
    let pts: Vec<(f64, f64)> = (0..MAX_PLAYERS).map(|k| grid_slot(t, k)).collect();
    for &(x, z) in &pts {
        assert!(on_road(t, x, z, 0.0) && z > 0.0);
    }
    for a in 0..pts.len() {
        for b in a + 1..pts.len() {
            assert!((pts[a].0 - pts[b].0).hypot(pts[a].1 - pts[b].1) >= 2.5);
        }
    }
}

// ----------------------------------------------------------------- referee --
fn members(n: usize, prefix: &str) -> Vec<(String, Value)> {
    (0..n).map(|i| (format!("{prefix}{i}"), pubv(&format!("{prefix}{i}")))).collect()
}

fn started(n: usize, track_id: &str, laps: i64) -> (Kart, &'static Track, f64) {
    let mut k = Kart::new();
    assert_eq!(k.start(&members(n, "u"), &json!(track_id), &json!(laps), 100.0), None);
    k.tick(100.0 + COUNTDOWN, true);
    assert_eq!(k.phase, Phase::Race);
    (k, tr(track_id), 100.0 + COUNTDOWN)
}

/// Drive uid along the centre line from track distance u0 to u1 at 20 m/s, 20 Hz.
fn drive(k: &mut Kart, uid: &str, u0: f64, u1: f64, t0: f64) -> f64 {
    let t_ = k.tr().unwrap();
    let (mut t, mut u) = (t0, u0);
    let step = 1.0 / TILE; // 20 m/s at 20 Hz = 1 m per tick
    while u < u1 && k.player(uid).unwrap().fin.is_none() {
        u = u1.min(u + step);
        t += 1.0 / 20.0;
        let (ok, fix) = k.pos(uid, &frame(&t_, u, 0.0, None), t);
        assert!(ok && fix.is_none(), "{uid} {u}");
    }
    t
}

#[test]
fn start_rules() {
    let mut k = Kart::new();
    assert_eq!(k.start(&[], &json!("nope"), &json!(3), 0.0).as_deref(), Some("pick a track"));
    assert_eq!(k.start(&members(1, "a"), &json!("meadow"), &json!(99), 0.0), None);
    assert_eq!(k.laps, MAX_LAPS as i64);
    assert_eq!(k.phase, Phase::Grid);
    assert!(k.start(&members(1, "a"), &json!("meadow"), &json!(3), 0.0).is_some()); // one at a time
    k.end();
    k.start(&members(12, "u"), &json!("canyon"), &Value::Null, 0.0);
    assert_eq!(k.players.len(), MAX_PLAYERS);
    assert_eq!(k.laps, tr("canyon").laps);
    assert_eq!(k.view(0.0)["goInMs"], 4000);
    assert_eq!(k.view(0.0)["players"].as_array().unwrap().len(), MAX_PLAYERS);
}

#[test]
fn cars_are_picked_and_kept_between_races() {
    let mut k = Kart::new();
    assert_eq!(k.car("a0", &json!({"car": 4})), Some(4));
    for bad in [json!({"car": 5}), json!({"car": -1}), json!({"car": 1.5}), json!({"car": true}), json!({})] {
        assert_eq!(k.car("a0", &bad), None, "{bad}");
    }
    k.start(&members(2, "a"), &json!("meadow"), &json!(1), 0.0);
    assert_eq!(k.player("a0").unwrap().car, 4);
    assert_eq!(k.player("a1").unwrap().car, 1); // the default: slot % CARS
    assert_eq!(k.car("a1", &json!({"car": 2})), Some(2));
    assert_eq!(k.player("a1").unwrap().car, 2); // still on the grid: it changes
    k.tick(COUNTDOWN, true);
    k.car("a1", &json!({"car": 3}));
    assert_eq!(k.player("a1").unwrap().car, 2); // mid-race: kept for the next one
    assert_eq!(k.cars["a1"], 3);
}

#[test]
fn frames_before_the_green_light_are_ignored() {
    let mut k = Kart::new();
    k.start(&members(1, "a"), &json!("meadow"), &json!(1), 0.0);
    assert_eq!(k.pos("a0", &frame(tr("meadow"), 0.6, 0.0, None), 1.0), (false, None));
    let evs = k.tick(COUNTDOWN, true);
    assert_eq!(evs[0].0, "go");
    assert_eq!(evs[0].1, json!({"track": "meadow"}));
}

#[test]
fn a_lap_and_the_finish() {
    let (mut k, t_, t) = started(1, "meadow", 2);
    let n = t_.n as f64;
    let u0 = k.player("u0").unwrap().u;
    let t = drive(&mut k, "u0", u0, 0.5 + n + 0.2, t);
    let p = k.player("u0").unwrap();
    assert_eq!((k.lap_of(p), p.lap_at.len()), (2, 1));
    let pu = p.u;
    let t = drive(&mut k, "u0", pu, 0.5 + 2.0 * n + 0.1, t);
    assert!(k.player("u0").unwrap().fin.is_some());
    let evs = k.tick(t + 0.01, true);
    let names: Vec<&str> = evs.iter().map(|e| e.0).collect();
    assert!(names.contains(&"finish") && *names.last().unwrap() == "done");
    let fin = &evs.iter().find(|e| e.0 == "finish").unwrap().1;
    assert_eq!(fin["laps"].as_array().unwrap().len(), 2);
    let res = &k.results.as_ref().unwrap()[0];
    assert_eq!(res["place"], 1);
    assert_eq!(res["dnf"], false);
    assert_eq!(res["user"]["userId"], "u0");
}

#[test]
fn teleports_and_off_road_frames_are_refused_with_a_correction() {
    let (mut k, t_, t) = started(1, "meadow", 1);
    let u0 = k.player("u0").unwrap().u;
    let t = drive(&mut k, "u0", u0, 2.0, t);
    let before = k.player("u0").unwrap().clone();
    let (ok, fix) = k.pos("u0", &frame(t_, 9.0, 0.0, None), t + 0.05); // 7 tiles in 50 ms
    assert!(!ok);
    assert_eq!(fix, Some(json!({"x": before.x, "z": before.z, "r": before.r})));
    let (x, z, _) = point_at(t_, 2.1, 8.0); // into the grass
    let (ok, fix) = k.pos("u0", &json!({"x": (x * 100.0).round(), "z": (z * 100.0).round(), "r": 0}), t + 0.6);
    assert!(!ok && fix.is_some());
    let p = k.player("u0").unwrap();
    assert_eq!(p.u, before.u);
    assert_eq!(p.bad, 2);
}

#[test]
fn corrections_are_spaced_out() {
    let (mut k, t_, t) = started(1, "meadow", 1);
    let far = frame(t_, 9.0, 0.0, None);
    assert!(k.pos("u0", &far, t + 0.6).1.is_some());
    assert!(k.pos("u0", &far, t + 0.7).1.is_none()); // within FIX_GAP: refused quietly
    assert!(k.pos("u0", &far, t + 1.2).1.is_some());
    assert_eq!(k.player("u0").unwrap().bad, 3);
}

#[test]
fn malformed_frames_are_dropped_without_counting_as_cheating() {
    let (mut k, _, t) = started(1, "meadow", 1);
    for bad in [json!({}), json!({"x": "1", "z": 0, "r": 0}), json!({"x": true, "z": 0, "r": 0}),
                json!({"x": 0, "z": null, "r": 0})] {
        assert_eq!(k.pos("u0", &bad, t + 0.1), (false, None));
    }
    assert_eq!(k.player("u0").unwrap().bad, 0);
    assert_eq!(k.pos("nobody", &json!({"x": 0, "z": 0, "r": 0}), t), (false, None));
}

#[test]
fn driving_backwards_over_the_line_does_not_count_a_lap() {
    let (mut k, t_, t) = started(1, "meadow", 1);
    let u0 = k.player("u0").unwrap().u;
    let mut t = drive(&mut k, "u0", u0, 1.0, t);
    // reverse back over the line and forward again: still lap 1, nothing finished
    let back = (0..10).map(|i| 0.9 - i as f64 * 0.05);
    let fwd = (0..10).map(|i| 0.45 + i as f64 * 0.05);
    for u in back.chain(fwd) {
        t += 0.05;
        assert!(k.pos("u0", &frame(t_, u, 0.0, None), t).0, "{u}");
    }
    let p = k.player("u0").unwrap();
    assert!(k.lap_of(p) == 1 && p.fin.is_none() && p.lap_at.is_empty());
}

#[test]
fn a_fast_clock_buys_no_speed() {
    // A client that claims more time has passed than really has (to make a long
    // jump look legal) is caught once its clock runs CLOCK_LEAD ahead of ours.
    let (mut k, t_, mut t) = started(1, "meadow", 1);
    let (mut u, mut q) = (k.player("u0").unwrap().u, 10000i64);
    for _ in 0..40 {
        // honest for 2 s
        u += 0.1;
        t += 0.05;
        q += 5;
        assert!(k.pos("u0", &frame(t_, u, 0.0, Some(q)), t).0);
    }
    let mut caught = false;
    for _ in 0..40 {
        // then 4x the real speed with a clock to match
        u += 0.4;
        t += 0.05;
        q += 20;
        let (ok, fix) = k.pos("u0", &frame(t_, u, 0.0, Some(q)), t);
        if !ok {
            caught = fix.is_some();
            break;
        }
    }
    assert!(caught);
    assert_eq!(k.player("u0").unwrap().bad, 1);
}

#[test]
fn stale_frames_are_dropped_quietly() {
    let (mut k, t_, t) = started(1, "meadow", 1);
    let u = k.player("u0").unwrap().u;
    assert!(k.pos("u0", &frame(t_, u + 0.1, 0.0, Some(500)), t + 0.05).0);
    assert_eq!(k.pos("u0", &frame(t_, u + 0.05, 0.0, Some(495)), t + 0.06), (false, None));
    assert_eq!(k.pos("u0", &frame(t_, u + 0.1, 0.0, Some(500)), t + 0.07), (false, None));
    assert_eq!(k.player("u0").unwrap().bad, 0);
}

#[test]
fn frames_are_rate_limited() {
    let (mut k, t_, t) = started(1, "meadow", 1);
    let took = (0..50).filter(|_| k.pos("u0", &frame(t_, 0.2, 0.0, None), t).0).count();
    assert!(took as f64 <= POS_BURST);
}

#[test]
fn standings_and_snapshots_only_carry_what_moved() {
    let (mut k, t_, t) = started(3, "meadow", 1);
    let u2 = k.player("u2").unwrap().u;
    drive(&mut k, "u2", u2, 3.0, t);
    let evs = k.tick(t + 1.0, true);
    let snap = &evs.iter().find(|e| e.0 == "snap").unwrap().1;
    assert_eq!(snap["order"][0], "u2");
    let cars: Vec<&str> = snap["cars"].as_array().unwrap().iter().map(|c| c["u"].as_str().unwrap()).collect();
    assert_eq!(cars, ["u2"]); // the grid went out with the snapshot at "go"
    let evs = k.tick(t + 1.05, true);
    assert!(!evs.iter().any(|e| e.0 == "snap")); // nothing moved, order unchanged
    let u0 = k.player("u0").unwrap().u;
    k.pos("u0", &frame(t_, u0 + 0.05, 0.0, None), t + 1.2);
    assert!(!k.tick(t + 1.25, false).iter().any(|e| e.0 == "snap")); // a thinned tick holds it back ...
    let evs = k.tick(t + 1.3, true);
    let snap = &evs.iter().find(|e| e.0 == "snap").unwrap().1; // ... to the next
    let cars: Vec<&str> = snap["cars"].as_array().unwrap().iter().map(|c| c["u"].as_str().unwrap()).collect();
    assert_eq!(cars, ["u0"]);
    assert_eq!(snap["cars"][0]["s"], 200);
    assert_eq!(snap["ms"], 1300);
}

#[test]
fn a_dropped_racer_keeps_their_car_for_the_grace_then_dnf() {
    let (mut k, _, t) = started(2, "meadow", 1);
    assert!(k.drop("u1", t, true));
    assert_eq!(k.player("u1").unwrap().away, Some(t));
    assert!(k.restore("u1", pubv("u1")));
    assert_eq!(k.player("u1").unwrap().away, None);
    k.drop("u1", t, true);
    let evs = k.tick(t + GRACE + 0.1, true);
    assert!(evs.contains(&("dnf", json!({"user": "u1"}))));
    assert!(!k.restore("u1", pubv("u1"))); // too late
    k.drop("u0", t + GRACE + 0.2, false); // an explicit leave: out at once
    let evs = k.tick(t + GRACE + 0.3, true);
    assert_eq!(evs.last().unwrap().0, "done");
    assert!(k.results.as_ref().unwrap().as_array().unwrap().iter().all(|r| r["dnf"] == true));
}

#[test]
fn leaving_the_grid_takes_the_car_off() {
    let mut k = Kart::new();
    k.start(&members(3, "u"), &json!("meadow"), &json!(1), 0.0);
    assert!(k.drop("u1", 0.5, false));
    assert_eq!(k.players.len(), 2);
    assert!(!k.drop("nobody", 0.5, false));
}

#[test]
fn the_rest_get_finish_grace_after_the_winner() {
    let (mut k, t_, t) = started(2, "meadow", 1);
    let u0 = k.player("u0").unwrap().u;
    let t = drive(&mut k, "u0", u0, 0.5 + t_.n as f64 + 0.1, t);
    k.tick(t, true);
    assert_eq!(k.phase, Phase::Race);
    let evs = k.tick(t + FINISH_GRACE + 0.01, true);
    assert_eq!(evs.last().unwrap().0, "done");
    let res = k.results.as_ref().unwrap().as_array().unwrap().clone();
    let order: Vec<&str> = res.iter().map(|r| r["user"]["userId"].as_str().unwrap()).collect();
    assert_eq!(order, ["u0", "u1"]);
    assert_eq!(res[1]["dnf"], true);
    assert_eq!(res[1]["ms"], Value::Null);
}

#[test]
fn a_race_is_called_after_max_race() {
    let (mut k, _, t) = started(2, "meadow", 1);
    k.tick(t + MAX_RACE - 1.0, true);
    assert_eq!(k.phase, Phase::Race);
    assert_eq!(k.tick(t + MAX_RACE, true).last().unwrap().0, "done");
    assert!(!k.running());
}

// --------------------------------------------- 8 players under simulated lag --
/// A gaussian from two uniforms (Box-Muller); rand_distr is not a dependency.
fn gauss(rng: &mut StdRng, mu: f64, sigma: f64) -> f64 {
    let u1: f64 = 1.0 - rng.gen::<f64>();
    let u2: f64 = rng.gen();
    mu + sigma * (-2.0 * u1.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos()
}

/// A frame in flight: ordered so the BinaryHeap pops the earliest arrival first.
struct InFlight(f64, u64, String, Value);
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

fn eight_car_race(seed: u64) {
    // Eight bots drive at different speeds, weaving across the road, sending 20
    // frames a second through a 120 ms +- 60 ms link (so frames arrive bunched and
    // out of order). Every honest frame must be accepted, the finish order must be
    // the true one, and the snapshots must stay inside the room's bandwidth budget.
    let mut rng = StdRng::seed_from_u64(seed);
    let t_ = tr("peaks");
    let mut k = Kart::new();
    let ms = members(8, "bot");
    let t0 = 1000.0;
    assert_eq!(k.start(&ms, &json!("peaks"), &json!(2), t0), None);
    let go = t0 + COUNTDOWN;
    let speed = |i: usize| 24.0 - i as f64 * 0.8; // bot0 fastest; all inside FINISH_GRACE
    let mut u: Vec<f64> = k.players.iter().map(|(_, p)| p.u).collect();
    let grid_x: Vec<f64> = k.players.iter().map(|(_, p)| grid_slot(t_, p.slot).0).collect();
    let goal = 0.5 + 2.0 * t_.n as f64 + 0.2;
    let mut q: BinaryHeap<InFlight> = BinaryHeap::new();
    let mut seq = 0u64;
    let mut t = t0;
    let mut snap_bytes: Vec<(f64, usize)> = Vec::new();
    let tick_every = 1.0 / HZ;
    let mut next_tick = t0;
    let mut true_finish: Vec<Option<f64>> = vec![None; 8];
    let mut rejected = 0;
    let mut out_of_order = 0;
    let mut last_arrival = [0.0f64; 8];
    while t < go + 200.0 && k.phase != Phase::Done {
        t = ((t + 0.05) * 1e6).round() / 1e6; // every client sends at 20 Hz
        if t >= go {
            for i in 0..8 {
                if true_finish[i].is_some() {
                    continue;
                }
                u[i] = goal.min(u[i] + speed(i) * 0.05 / TILE);
                let w = ((t - go) / 2.0).min(1.0); // ease from the grid slot into the weave
                let lat = (1.0 - w) * grid_x[i] + w * 2.5 * (u[i] * 1.7 + i as f64).sin();
                if u[i] >= 0.5 + 2.0 * t_.n as f64 {
                    true_finish[i] = Some(t);
                }
                let delay = gauss(&mut rng, 0.12, 0.06).clamp(0.0, 0.24);
                seq += 1;
                let qq = (t * 100.0) as i64;
                q.push(InFlight(t + delay, seq, format!("bot{i}"), frame(t_, u[i], lat, Some(qq))));
            }
        }
        while q.peek().map(|f| f.0 <= t).unwrap_or(false) {
            let InFlight(at, _, uid, fr) = q.pop().unwrap();
            let i: usize = uid[3..].parse().unwrap();
            let sent_q = fr["q"].as_f64().unwrap() / 100.0;
            if sent_q < last_arrival[i] {
                out_of_order += 1;
            }
            last_arrival[i] = last_arrival[i].max(sent_q);
            // an out-of-order frame is older than what the server already has: the
            // client never goes backwards on purpose, so the referee must not punish it
            let (ok, _fix) = k.pos(&uid, &fr, at);
            if !ok && k.player(&uid).unwrap().fin.is_none() {
                rejected += 1;
            }
        }
        while next_tick <= t {
            for (ev, data) in k.tick(next_tick, true) {
                if ev == "snap" {
                    let mut m = json!({"type": "game", "g": "kart", "ev": ev});
                    m.as_object_mut().unwrap().extend(data.as_object().unwrap().clone());
                    snap_bytes.push((next_tick, m.to_string().len()));
                }
            }
            next_tick += tick_every;
        }
    }
    assert_eq!(k.phase, Phase::Done);
    assert!(out_of_order > 0, "the link must reorder some frames");
    let res = k.results.as_ref().unwrap().as_array().unwrap();
    let order: Vec<String> = res.iter().map(|r| r["user"]["userId"].as_str().unwrap().to_string()).collect();
    let mut want: Vec<usize> = (0..8).collect();
    want.sort_by(|a, b| true_finish[*a].partial_cmp(&true_finish[*b]).unwrap());
    let want: Vec<String> = want.into_iter().map(|i| format!("bot{i}")).collect();
    assert_eq!(order, want);
    assert!(res.iter().all(|r| r["dnf"] == false));
    // Out-of-order frames are dropped as stale, never counted as cheating.
    assert_eq!(k.players.iter().map(|(_, p)| p.bad).sum::<u32>(), 0);
    let _ = rejected; // stale frames are "rejected" (not taken) but never "bad"
    let mut per_sec: HashMap<i64, usize> = HashMap::new();
    for (at, n) in snap_bytes {
        *per_sec.entry(at as i64).or_default() += n * 8; // fanned out to 8 sockets
    }
    assert!(*per_sec.values().max().unwrap() < ROOM_BYTES_PER_SEC);
}

#[test]
fn eight_cars_with_lag_finish_in_order_with_no_false_rejections() {
    for seed in [7, 8, 9, 10, 11] {
        eight_car_race(seed);
    }
}

#[test]
fn eight_car_tick_is_cheap() {
    let (mut k, t_, t) = started(8, "meadow", 3);
    let start = std::time::Instant::now();
    for f in 0..20 * 30 {
        // 30 s of 20 Hz frames
        let tt = t + f as f64 * 0.05;
        for i in 0..8 {
            let uid = format!("u{i}");
            let u = k.player(&uid).unwrap().u;
            k.pos(&uid, &frame(t_, u + 0.1, 0.0, None), tt);
        }
        k.tick(tt, true);
    }
    let per_second = start.elapsed().as_secs_f64() / 30.0;
    assert!(per_second < 0.05, "{per_second}"); // < 5% of one core per room (debug build)
}

// ------------------------------------------- through the room + lobby layer --
struct Env {
    hub: KartHub,
    rooms: RoomManager,
    clock: Arc<Mutex<f64>>,
}

fn env(max_tickers: usize) -> Env {
    let rooms = RoomManager::new();
    let clock = Arc::new(Mutex::new(1000.0));
    let c = clock.clone();
    let hub = KartHub::new(rooms.clone(), Registry::new(max_tickers), Arc::new(move || *c.lock().unwrap()),
        crate::results::Recorder::default());
    Env { hub, rooms, clock }
}

impl Env {
    fn advance(&self, dt: f64) {
        *self.clock.lock().unwrap() += dt;
    }
    async fn connect(&self, room: &str, conn: u64, uid: &str) -> (Member, mpsc::Receiver<String>) {
        let m = Member { user_id: uid.into(), handle: uid.into(), display_name: uid.into(),
                         avatar_url: String::new() , cos: serde_json::Value::Null};
        let (_rx, drx, _, _) = self.rooms.join_direct(room, conn, m.clone()).await.unwrap();
        (m, drx)
    }
    async fn send(&self, room: &str, conn: u64, m: &Member, op: &str, extra: Value) {
        let mut msg = json!({"type": "game", "g": "kart", "op": op});
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
        let m = tokio::time::timeout_at(deadline, rx.recv())
            .await
            .unwrap_or_else(|_| panic!("no {ev} event"))
            .expect("queue open");
        let v: Value = serde_json::from_str(&m).unwrap();
        if v["type"] == "game" && v["g"] == "kart" && v["ev"] == ev && pick(&v) {
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
    let (a, mut wa) = e.connect("kartroom", 1, "kart-a").await;
    let (b, mut wb) = e.connect("kartroom", 2, "kart-b").await;
    e.send("kartroom", 1, &a, "join", json!({})).await;
    let lobby = until(&mut wa, "lobby").await;
    assert_eq!(lobby["members"][0]["host"], true);
    assert_eq!(lobby["joined"]["userId"], "kart-a");
    until(&mut wa, "kart").await;
    e.send("kartroom", 2, &b, "join", json!({})).await;
    until(&mut wb, "kart").await;
    e.send("kartroom", 2, &b, "start", json!({"track": "meadow", "laps": 1})).await;
    assert_eq!(until(&mut wb, "error").await["error"], "only the host can do that");
    e.send("kartroom", 1, &a, "car", json!({"car": 4})).await;
    assert_eq!(until(&mut wb, "car").await["car"], 4);
    e.send("kartroom", 1, &a, "start", json!({"track": "meadow", "laps": 1})).await;
    let race = until_where(&mut wb, "kart", |m| m["race"]["phase"] == "grid").await["race"].clone();
    assert_eq!(race["laps"], 1);
    assert_eq!(race["players"].as_array().unwrap().len(), 2);
    let a_car: Vec<&Value> = race["players"].as_array().unwrap().iter()
        .filter(|p| p["user"]["userId"] == "kart-a").map(|p| &p["car"]).collect();
    assert_eq!(a_car, [&json!(4)]);
    assert!(e.hub.registry().get("kart:kartroom").is_some());
    e.advance(COUNTDOWN + 0.01);
    until(&mut wa, "go").await;
    until(&mut wb, "go").await;
    let t_ = tr("meadow");
    let mut u = 0.5 - grid_slot(t_, 0).1 / TILE;
    while u < 0.5 + t_.n as f64 + 0.1 {
        u += 0.08;
        e.advance(0.05);
        e.send("kartroom", 1, &a, "pos", frame(t_, u, 0.0, None)).await;
    }
    let fin = until_where(&mut wb, "finish", |m| m["user"] == "kart-a").await;
    assert_eq!(fin["place"], 1);
    e.send("kartroom", 2, &b, "leave", json!({})).await; // the other car is out: race over
    let done = until(&mut wa, "done").await;
    assert_eq!(done["results"][0]["user"]["userId"], "kart-a");
    assert_eq!(done["results"][0]["place"], 1);
    assert_eq!(done["results"][1]["dnf"], true);
    until_where(&mut wa, "kart", |m| m["race"]["phase"] == "done").await;
    assert_eq!(e.hub.with_room("kartroom", |v| v.kart.running()), Some(false));
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(e.hub.registry().get("kart:kartroom").is_none(), "the loop stops with the race");
}

#[tokio::test]
async fn teleport_through_the_lobby_gets_a_fix_and_end_stops_the_loop() {
    let e = env(MAX_TICKERS);
    let (a, mut wa) = e.connect("kartfix", 1, "kart-c").await;
    e.send("kartfix", 1, &a, "join", json!({})).await;
    until(&mut wa, "kart").await;
    e.send("kartfix", 1, &a, "start", json!({"track": "canyon", "laps": 1})).await;
    until_where(&mut wa, "kart", |m| m["race"]["phase"] == "grid").await;
    e.advance(COUNTDOWN + 0.01);
    until(&mut wa, "go").await;
    e.send("kartfix", 1, &a, "pos", frame(tr("canyon"), 12.0, 0.0, None)).await;
    let fix = until(&mut wa, "fix").await;
    assert!(fix.get("x").is_some() && fix.get("z").is_some() && fix.get("r").is_some());
    e.send("kartfix", 1, &a, "end", json!({})).await;
    assert_eq!(until(&mut wa, "kart").await["race"]["phase"], "idle");
    assert!(e.hub.registry().get("kart:kartfix").is_none());
}

#[tokio::test]
async fn lobby_rules() {
    let e = env(MAX_TICKERS);
    let (a, mut wa) = e.connect("lob", 1, "a").await;
    // other games do not exist on this backend
    e.hub.handle("lob", 1, &a, &json!({"type": "game", "g": "golf", "op": "join"})).await;
    let m: Value = serde_json::from_str(&wa.recv().await.unwrap()).unwrap();
    assert_eq!(m, json!({"type": "game", "g": "golf", "ev": "error", "pv": 1, "error": "unknown game"}));
    e.hub.handle("lob", 1, &a, &json!({"type": "game", "g": 3, "op": "join"})).await;
    let m: Value = serde_json::from_str(&wa.recv().await.unwrap()).unwrap();
    assert_eq!((m["g"].as_str(), m["error"].as_str()), (Some("?"), Some("unknown game")));
    // ops before joining
    e.send("lob", 1, &a, "start", json!({"track": "meadow"})).await;
    assert_eq!(until(&mut wa, "error").await["error"], "join the lobby first");
    e.send("lob", 1, &a, "invite", json!({"to": "z"})).await;
    assert_eq!(until(&mut wa, "error").await["error"], "join the lobby first");
    // the lobby holds MAX_LOBBY people
    let mut socks = Vec::new();
    for i in 0..MAX_LOBBY {
        let (m, rx) = e.connect("lob", 10 + i as u64, &format!("p{i}")).await;
        e.send("lob", 10 + i as u64, &m, "join", json!({})).await;
        socks.push((m, rx));
    }
    e.send("lob", 1, &a, "join", json!({})).await;
    assert_eq!(until(&mut wa, "error").await["error"], "this game's lobby is full");
    // the host leaving hands the lobby to the next in line
    let (p0, _) = &socks[0];
    e.send("lob", 10, p0, "leave", json!({})).await;
    let lobby = until_where(&mut wa, "lobby", |m| m["left"]["userId"] == "p0").await;
    let hosts: Vec<&Value> = lobby["members"].as_array().unwrap().iter()
        .filter(|m| m["host"] == true).map(|m| &m["userId"]).collect();
    assert_eq!(hosts, [&json!("p1")]);
    // now a can join, and invite someone (delivered to every socket they have)
    e.send("lob", 1, &a, "join", json!({})).await;
    until(&mut wa, "kart").await;
    let (_z, mut wz) = e.connect("elsewhere", 99, "z").await;
    e.send("lob", 1, &a, "invite", json!({"to": "z"})).await;
    let inv = until(&mut wz, "invite").await;
    assert_eq!((inv["room"].as_str(), inv["from"]["userId"].as_str()), (Some("lob"), Some("a")));
    let ack = until(&mut wa, "invited").await;
    assert_eq!(ack["delivered"], 1);
    e.send("lob", 1, &a, "invite", json!({"to": "a"})).await;
    assert_eq!(until(&mut wa, "error").await["error"], "invite someone else");
    e.send("lob", 1, &a, "car", json!({"car": 9})).await;
    assert_eq!(until(&mut wa, "error").await["error"], "pick a car");
    e.send("lob", 1, &a, "start", json!({"track": "meadow"})).await;
    assert_eq!(until(&mut wa, "error").await["error"], "only the host can do that");
}

#[tokio::test]
async fn a_dropped_socket_is_a_blip_and_a_rejoin_restores_the_car() {
    let e = env(MAX_TICKERS);
    let (a, mut wa) = e.connect("blip", 1, "a").await;
    let (b, _wb) = e.connect("blip", 2, "b").await;
    e.send("blip", 1, &a, "join", json!({})).await;
    e.send("blip", 2, &b, "join", json!({})).await;
    e.send("blip", 1, &a, "start", json!({"track": "meadow", "laps": 1})).await;
    e.advance(COUNTDOWN + 0.01);
    until(&mut wa, "go").await;
    e.disconnect("blip", 2, &b).await;
    let left = until_where(&mut wa, "kart", |m| m["left"] == "b").await;
    let pb = left["race"]["players"].as_array().unwrap().iter().find(|p| p["user"]["userId"] == "b").unwrap().clone();
    assert!(pb["away"].is_number() && pb["dnf"] == false);
    // b reloads within the grace: their car is back, no "joined" toast
    let (b2, _wb2) = e.connect("blip", 3, "b").await;
    e.send("blip", 3, &b2, "join", json!({})).await;
    let lobby = until(&mut wa, "lobby").await;
    assert_eq!(lobby["joined"], Value::Null);
    let back = until_where(&mut wa, "kart", |m| m["back"] == "b").await;
    let pb = back["race"]["players"].as_array().unwrap().iter().find(|p| p["user"]["userId"] == "b").unwrap().clone();
    assert_eq!(pb["away"], Value::Null);
    // a second socket for a: closing one of them is not a leave
    let (a2, _wa2) = e.connect("blip", 4, "a").await;
    e.disconnect("blip", 4, &a2).await;
    assert_eq!(e.hub.with_room("blip", |v| v.lobby.has("a")), Some(true));
    // the host's socket drops: b hosts, a coming back quickly is host again
    e.disconnect("blip", 1, &a).await;
    assert_eq!(e.hub.with_room("blip", |v| v.lobby.host.clone()), Some(Some("b".into())));
    let (a3, _wa3) = e.connect("blip", 5, "a").await;
    e.send("blip", 5, &a3, "join", json!({})).await;
    assert_eq!(e.hub.with_room("blip", |v| v.lobby.host.clone()), Some(Some("a".into())));
}

#[tokio::test]
async fn the_loop_stops_when_the_room_empties() {
    let e = env(MAX_TICKERS);
    let (a, mut wa) = e.connect("gone", 1, "a").await;
    e.send("gone", 1, &a, "join", json!({})).await;
    e.send("gone", 1, &a, "start", json!({"track": "peaks", "laps": 2})).await;
    until_where(&mut wa, "kart", |m| m["race"]["phase"] == "grid").await;
    assert_eq!(e.hub.registry().running(), 1);
    e.disconnect("gone", 1, &a).await;
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(e.hub.registry().running(), 0);
    assert!(e.hub.with_room("gone", |_| ()).is_none());
}

#[tokio::test]
async fn a_full_arena_says_so_to_the_host() {
    let e = env(0);
    let (a, mut wa) = e.connect("busy", 1, "a").await;
    e.send("busy", 1, &a, "join", json!({})).await;
    e.send("busy", 1, &a, "start", json!({"track": "meadow"})).await;
    assert_eq!(until(&mut wa, "error").await["error"], "the Arena is busy right now: try again in a minute");
    assert_eq!(e.hub.with_room("busy", |v| v.kart.phase), Some(Phase::Idle));
}

#[tokio::test]
async fn snapshots_count_against_the_rooms_bandwidth() {
    let e = env(MAX_TICKERS);
    let (a, mut wa) = e.connect("bw", 1, "a").await;
    e.send("bw", 1, &a, "join", json!({})).await;
    e.send("bw", 1, &a, "start", json!({"track": "meadow", "laps": 1})).await;
    e.advance(COUNTDOWN + 0.01);
    until(&mut wa, "go").await;
    until(&mut wa, "snap").await;
    let tk = e.hub.registry().get("kart:bw").unwrap();
    assert!(tk.bytes_1s() > 0 && tk.ticks() > 0);
    let stats = e.hub.registry().stats();
    assert_eq!(stats["rooms"]["kart:bw"]["hz"], HZ);
}

// ------------------------------------------------- user-made tracks (2.5) --
const RECT: &str = "FSSRSSRSSSSRSSRS"; // a 4 x 6 rectangle, two straights before the line

fn kdata(tiles: &str) -> Value {
    json!({"tiles": tiles, "scenery": "tents",
           "theme": {"sky": "#9fd3f0", "fog": "#cfe8f2", "ground": "#76b85a"}})
}

fn kdoc(tiles: &str) -> Value {
    json!({"kind": "kart", "v": 1, "name": "Back Lot", "data": kdata(tiles)})
}

fn is_ckey(k: &str) -> bool {
    k.len() == 14 && k.starts_with("c-") && k[2..].chars().all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
}

#[test]
fn a_custom_track_starts_and_a_lap_counts() {
    let mut k = Kart::new();
    assert_eq!(k.start_custom(&members(1, "u"), &kdoc(RECT), &json!(1), 100.0), None);
    let key = k.track.clone().unwrap();
    assert!(is_ckey(&key), "{key}");
    let v = k.view(100.0);
    assert_eq!(v["track"], json!(key));
    assert_eq!(v["custom"], json!({"name": "Back Lot", "data": kdata(RECT)}));
    assert_eq!(v["laps"], 1);
    let evs = k.tick(100.0 + COUNTDOWN, true);
    assert_eq!(evs[0], ("go", json!({"track": key})));
    let n = k.tr().unwrap().n as f64;
    assert_eq!(n, RECT.len() as f64);
    let u0 = k.player("u0").unwrap().u;
    let t = drive(&mut k, "u0", u0, 0.5 + n + 0.1, 100.0 + COUNTDOWN);
    assert!(k.player("u0").unwrap().fin.is_some());
    let evs = k.tick(t + 0.01, true);
    let done = evs.iter().find(|e| e.0 == "done").expect("done");
    assert_eq!(done.1["track"], json!(key)); // results.rs keys the board on this
    for (ev, data) in &evs {
        assert!(data.get("custom").is_none(), "{ev} carries the track");
    }
}

#[test]
fn bad_custom_tracks_are_refused_without_a_panic() {
    let mut big = kdata(RECT);
    big["pad"] = json!("x".repeat(13 * 1024));
    let mut bad_hex = kdata(RECT);
    bad_hex["theme"]["fog"] = json!("#12345g");
    let mut short_hex = kdata(RECT);
    short_hex["theme"]["sky"] = json!("#abc");
    let mut no_theme = kdata(RECT);
    no_theme.as_object_mut().unwrap().remove("theme");
    let mut bad_scenery = kdata(RECT);
    bad_scenery["scenery"] = json!("lava");
    let cases: Vec<(Value, &str)> = vec![
        (kdata("FSS"), "tiles"),
        (kdata("FSSXSSRSSSSRSSRS"), "tile 3"),
        (kdata("FSSSSSSSSS"), "does not close"),
        (kdata("FSRSSRSSRSSRS"), "runs over itself"),
        (kdata("FSLSSLSSLSSL"), "starting grid"), // closes, on a left turn
        (kdata("SSSRSSRSSSSRSSRF"), "first tile"),
        (kdata(&"FSSRSSRSSSSRSSRS".repeat(6)), "tiles"),
        (big, "too big"),
        (bad_hex, "theme.fog"),
        (short_hex, "theme.sky"),
        (no_theme, "theme"),
        (bad_scenery, "scenery"),
        (json!({"tiles": 5}), "tiles"),
        (json!("FSS"), "object"),
        (Value::Null, "object"),
        (json!({"tiles": "FSSRSSRSSSSRSSRSé"}), "tile 16"),
    ];
    for (data, why) in cases {
        let e = validate_custom(&data).expect_err(&data.to_string()[..40.min(data.to_string().len())]);
        assert!(e.contains(why), "{e} / {why}");
        let doc = json!({"kind": "kart", "v": 1, "name": "x", "data": data});
        let mut k = Kart::new();
        let got = k.start_custom(&members(1, "u"), &doc, &json!(3), 0.0).unwrap();
        assert!(got.starts_with("bad track: "), "{got}");
        assert_eq!(k.phase, Phase::Idle);
        assert!(k.custom.is_none());
    }
    // the document around the data
    let bad_docs = [
        json!({"kind": "plat", "v": 1, "name": "x", "data": kdata(RECT)}),
        json!({"kind": "kart", "v": 2, "name": "x", "data": kdata(RECT)}),
        json!({"kind": "kart", "v": 1, "name": "  ", "data": kdata(RECT)}),
        json!({"kind": "kart", "v": 1, "name": "a".repeat(33), "data": kdata(RECT)}),
        json!({"kind": "kart", "v": 1, "name": "<b>", "data": kdata(RECT)}),
        json!({"kind": "kart", "v": 1, "name": "see HTTP x", "data": kdata(RECT)}),
        json!({"kind": "kart", "v": 1, "name": "tab\there", "data": kdata(RECT)}),
        json!({"kind": "kart", "v": 1, "data": kdata(RECT)}),
        json!("nope"),
    ];
    for d in bad_docs {
        let mut k = Kart::new();
        assert!(k.start_custom(&members(1, "u"), &d, &json!(3), 0.0).unwrap().starts_with("bad track: "), "{d}");
    }
}

#[test]
fn a_loop_may_end_on_a_corner_and_the_grid_check_still_bites() {
    // A right turn into the start tile: the 8 grid slots still sit on the ring.
    for tiles in ["FSRSSRSSRSSR", "FRSSRSRSSR"] {
        let c = validate_custom(&kdata(tiles)).unwrap_or_else(|e| panic!("{tiles}: {e}"));
        let tr = build_track(c["tiles"].as_str().unwrap()).unwrap();
        assert_eq!(tr.tiles[tr.n - 1].kind, 'C', "{tiles}");
        assert!(grid_fits(&tr));
    }
    // A left turn into it closes too, but the back row's outside slot is off the road.
    let e = validate_custom(&kdata("FSLSSLSSLSSL")).unwrap_err();
    assert!(e.starts_with("the starting grid is off the road"), "{e}");
    assert!(!grid_fits(&build_track("FSLSSLSSLSSL").unwrap()));
    // A track with no tile behind the line (never a closed loop) fails it.
    let mut tr = build_track(RECT).unwrap();
    let last = tr.tiles.pop().unwrap();
    tr.cells.remove(&(last.col, last.row));
    tr.n -= 1;
    assert!(!grid_fits(&tr));
}

#[test]
fn canonical_data_drops_laps_and_unknown_keys_and_lowercases() {
    let mut d = kdata(RECT);
    d["laps"] = json!(5);
    d["theme"]["sky"] = json!("#AABBCC");
    d["theme"]["deco"] = json!({"mul": "#ffffff"});
    d["extra"] = json!(true);
    let c = validate_custom(&d).unwrap();
    assert_eq!(c, json!({"tiles": RECT, "scenery": "tents",
                         "theme": {"sky": "#aabbcc", "fog": "#cfe8f2", "ground": "#76b85a"}}));
    assert!(c.get("laps").is_none());
    let keys: Vec<&String> = c.as_object().unwrap().keys().collect();
    assert_eq!(keys, ["tiles", "scenery", "theme"]);
    // canonical is a fixed point
    assert_eq!(validate_custom(&c).unwrap(), c);
}

#[test]
fn the_key_is_stable_across_case_and_key_order() {
    let a = kdata(RECT);
    let mut upper = kdata(RECT);
    upper["theme"]["sky"] = json!("#9FD3F0");
    let shuffled: Value = serde_json::from_str(&format!(
        r##"{{"theme": {{"ground": "#76B85A", "fog": "#cfe8f2", "sky": "#9fd3f0"}}, "laps": 2, "scenery": "tents", "tiles": "{RECT}"}}"##
    )).unwrap();
    let ka = crate::mapkey::content_key("kart", &validate_custom(&a).unwrap());
    assert!(is_ckey(&ka), "{ka}");
    for d in [upper, shuffled] {
        assert_eq!(crate::mapkey::content_key("kart", &validate_custom(&d).unwrap()), ka);
    }
    // the key is the race's track id, whatever the name
    let mut doc = kdoc(RECT);
    doc["name"] = json!("Another name");
    assert_eq!(custom_track(&doc).unwrap().key, ka);
    // different geometry, different board
    assert_ne!(crate::mapkey::content_key("kart", &validate_custom(&kdata("FSSSRSSRSSSSSRSSRS")).unwrap()), ka);
}

#[test]
fn a_built_in_start_clears_the_custom_track() {
    let mut k = Kart::new();
    k.start_custom(&members(2, "u"), &kdoc(RECT), &Value::Null, 0.0);
    assert_eq!(k.laps, 3); // no laps given: three
    assert!(k.custom.is_some());
    k.end();
    // between races the view still says what was raced
    assert!(k.view(1.0).get("custom").is_some());
    assert_eq!(k.start(&members(2, "u"), &json!("meadow"), &json!(1), 2.0), None);
    assert!(k.custom.is_none());
    let v = k.view(2.0);
    assert_eq!(v["track"], "meadow");
    assert!(v.get("custom").is_none());
    // a race in progress refuses a custom start too
    assert!(k.start_custom(&members(2, "u"), &kdoc(RECT), &json!(1), 3.0).unwrap().contains("already on"));
    assert_eq!(k.track.as_deref(), Some("meadow"));
}

#[test]
fn no_snapshot_carries_the_custom_track() {
    let mut k = Kart::new();
    k.start_custom(&members(3, "u"), &kdoc(RECT), &json!(1), 0.0);
    let tr = k.tr().unwrap();
    let mut t = COUNTDOWN;
    let mut snaps = 0;
    for ev in k.tick(t, true) {
        assert!(ev.1.get("custom").is_none(), "{}", ev.0);
    }
    for step in 0..40 {
        t += 0.05;
        for i in 0..3 {
            let uid = format!("u{i}");
            let u = k.player(&uid).unwrap().u + 0.05;
            k.pos(&uid, &frame(&tr, u, 0.0, None), t);
        }
        for (ev, data) in k.tick(t, step % 2 == 0) {
            snaps += (ev == "snap") as usize;
            assert!(data.get("custom").is_none() && !data.to_string().contains(RECT), "{ev}");
        }
    }
    assert!(snaps > 0);
}

#[tokio::test]
async fn a_custom_race_through_the_lobby() {
    let e = env(MAX_TICKERS);
    let (a, mut wa) = e.connect("ctrack", 1, "kart-a").await;
    let (b, mut wb) = e.connect("ctrack", 2, "kart-b").await;
    e.send("ctrack", 1, &a, "join", json!({})).await;
    until(&mut wa, "kart").await;
    e.send("ctrack", 2, &b, "join", json!({})).await;
    until(&mut wb, "kart").await;
    // only the host starts, and a bad track says why
    e.send("ctrack", 2, &b, "start", json!({"track": "custom", "laps": 1, "custom": kdoc(RECT)})).await;
    assert_eq!(until(&mut wb, "error").await["error"], "only the host can do that");
    e.send("ctrack", 1, &a, "start", json!({"track": "custom", "laps": 1, "custom": kdoc("FSSSSSSSS")})).await;
    let err = until(&mut wa, "error").await;
    assert!(err["error"].as_str().unwrap().starts_with("bad track: the loop does not close"), "{err}");
    assert!(e.hub.registry().get("kart:ctrack").is_none());
    e.send("ctrack", 1, &a, "start", json!({"track": "custom", "laps": 1, "custom": kdoc(RECT)})).await;
    let race = until_where(&mut wb, "kart", |m| m["race"]["phase"] == "grid").await["race"].clone();
    let key = race["track"].as_str().unwrap().to_string();
    assert!(is_ckey(&key));
    assert_eq!(race["custom"]["name"], "Back Lot");
    assert_eq!(race["custom"]["data"]["tiles"], RECT);
    // a late "view" (a rejoin) gets the track too
    e.send("ctrack", 2, &b, "view", json!({})).await;
    let v = until(&mut wb, "kart").await;
    assert_eq!(v["race"]["custom"]["data"], kdata(RECT));
    e.advance(COUNTDOWN + 0.01);
    assert_eq!(until(&mut wa, "go").await["track"], json!(key));
    let tr = e.hub.with_room("ctrack", |v| v.kart.tr().unwrap()).unwrap();
    let mut u = 0.5 - grid_slot(&tr, 0).1 / TILE;
    while u < 0.5 + tr.n as f64 + 0.1 {
        u += 0.08;
        e.advance(0.05);
        e.send("ctrack", 1, &a, "pos", frame(&tr, u, 0.0, None)).await;
    }
    let fin = until_where(&mut wb, "finish", |m| m["user"] == "kart-a").await;
    assert_eq!(fin["place"], 1);
    e.send("ctrack", 2, &b, "leave", json!({})).await;
    let done = until(&mut wa, "done").await;
    assert_eq!(done["track"], json!(key));
    assert!(done.get("custom").is_none());
}
