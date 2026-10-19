use super::super::testkit::*;
use super::*;
use crate::realtime::MAX_TICKERS;

const FULL: [bool; 10] = [true; 10];
const GOLDEN: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/bowl_golden.json");

fn lane(bumpers: bool) -> Lane {
    Lane { bumpers, standing: FULL }
}

fn card(frames: &[&[u8]]) -> Vec<Vec<u8>> {
    frames.iter().map(|f| f.to_vec()).collect()
}

fn last_score(frames: &[&[u8]]) -> Option<u16> {
    *score_card(&card(frames)).last().unwrap()
}

/// A small LCG so the golden inputs are the same on every machine.
struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.0 >> 33
    }
    fn range(&mut self, lo: i64, hi: i64) -> i64 {
        lo + (self.next() % (hi - lo + 1) as u64) as i64
    }
}

fn mask(b: &[bool; 10]) -> i64 {
    b.iter().enumerate().map(|(i, d)| if *d { 1 << i } else { 0 }).sum()
}

fn golden_inputs() -> Vec<Value> {
    let mut r = Lcg(0x5eed_b0e1);
    (0..240)
        .map(|i| {
            let standing = if i % 3 == 2 { r.range(1, 1022) } else { 1023 };
            json!({"bumpers": i % 5 == 0, "standing": standing, "x": r.range(-X_MAX, X_MAX),
                   "aim": r.range(-AIM_MAX, AIM_MAX), "power": r.range(1, 100),
                   "spin": r.range(-SPIN_MAX, SPIN_MAX), "oil": r.range(0, 2)})
        })
        .collect()
}

fn run(v: &Value) -> Roll {
    let st = v["standing"].as_i64().unwrap();
    let mut standing = [false; 10];
    for (i, s) in standing.iter_mut().enumerate() {
        *s = st & (1 << i) != 0;
    }
    let l = Lane { bumpers: v["bumpers"].as_bool().unwrap(), standing };
    let g = |k: &str| v[k].as_i64().unwrap();
    simulate_roll(&l, g("x"), g("aim"), g("power"), g("spin"), g("oil") as usize)
}

fn fixtures() -> Vec<Vec<Vec<u8>>> {
    let x: &[u8] = &[10];
    vec![
        card(&[x, x, x, x, x, x, x, x, x, &[10, 10, 10]]),
        card(&[&[5, 5], &[5, 5], &[5, 5], &[5, 5], &[5, 5], &[5, 5], &[5, 5], &[5, 5], &[5, 5], &[5, 5, 5]]),
        card(&[&[0u8, 0] as &[u8]; 10]),
        card(&[&[9, 1], x, &[3, 4], &[], &[], &[], &[], &[], &[], &[]]),
        card(&[&[7, 2], &[10], &[10], &[8, 2], &[0, 10], &[3, 3], &[10], &[9, 0], &[10], &[7, 3, 10]]),
        card(&[x, x, x, x, &[10, 2, 8]]),
        card(&[&[3, 7], &[4]]),
    ]
}

#[test]
#[ignore]
fn regenerate_bowl_golden() {
    let rolls: Vec<Value> = golden_inputs()
        .into_iter()
        .map(|mut v| {
            let r = run(&v);
            let end = *r.path_samples.last().unwrap();
            v["down"] = json!(mask(&r.pins_down));
            v["gutter"] = json!(r.gutter);
            v["ticks"] = json!(r.ticks);
            v["end"] = json!([end.0, end.1]);
            v
        })
        .collect();
    let cards: Vec<Value> =
        fixtures().into_iter().map(|f| json!({"frames": f, "scores": score_card(&f)})).collect();
    let body = serde_json::to_string(&json!({"rolls": rolls, "cards": cards})).unwrap();
    std::fs::write(GOLDEN, body + "\n").unwrap();
}

#[test]
fn the_golden_vectors_roll_exactly() {
    let g: Value = serde_json::from_str(&std::fs::read_to_string(GOLDEN).unwrap()).unwrap();
    let rolls = g["rolls"].as_array().unwrap();
    assert!(rolls.len() >= 200, "run the ignored regenerate_bowl_golden test");
    let (mut strikes, mut gutters) = (0, 0);
    for v in rolls {
        let r = run(v);
        let end = *r.path_samples.last().unwrap();
        assert_eq!(json!(mask(&r.pins_down)), v["down"], "{v}");
        assert_eq!(json!(r.gutter), v["gutter"], "{v}");
        assert_eq!(json!(r.ticks), v["ticks"], "{v}");
        assert_eq!(json!([end.0, end.1]), v["end"], "{v}");
        strikes += (v["standing"] == 1023 && v["down"] == 1023) as i32;
        gutters += r.gutter as i32;
    }
    // The vectors exercise the interesting outcomes, not only misses.
    assert!(strikes >= 3, "{strikes} strikes");
    assert!(gutters >= 3, "{gutters} gutters");
    for c in g["cards"].as_array().unwrap() {
        let f: Vec<Vec<u8>> = serde_json::from_value(c["frames"].clone()).unwrap();
        assert_eq!(json!(score_card(&f)), c["scores"]);
    }
}

#[test]
fn a_perfect_game_is_300() {
    let x: &[u8] = &[10];
    assert_eq!(last_score(&[x, x, x, x, x, x, x, x, x, &[10, 10, 10]]), Some(300));
}

#[test]
fn all_five_spares_is_150() {
    let s: &[u8] = &[5, 5];
    assert_eq!(last_score(&[s, s, s, s, s, s, s, s, s, &[5, 5, 5]]), Some(150));
}

#[test]
fn all_gutters_is_0() {
    assert_eq!(last_score(&[&[0u8, 0] as &[u8]; 10]), Some(0));
}

#[test]
fn a_spare_then_a_strike() {
    let sc = score_card(&card(&[&[9, 1], &[10], &[3, 4], &[], &[], &[], &[], &[], &[], &[]]));
    assert_eq!(sc[0], Some(20));
    assert_eq!(sc[1], Some(37));
    assert_eq!(sc[2], Some(44));
    assert_eq!(sc[3], None);
    // A strike waits for its two balls.
    let sc = score_card(&card(&[&[9, 1], &[10], &[], &[], &[]]));
    assert_eq!(sc, vec![Some(20), None, None, None, None]);
}

#[test]
fn the_tenth_frame_takes_its_bonus_balls() {
    let o: &[u8] = &[0, 0];
    assert_eq!(last_score(&[o, o, o, o, o, o, o, o, o, &[7, 3, 10]]), Some(20));
    assert_eq!(last_score(&[o, o, o, o, o, o, o, o, o, &[10, 7, 2]]), Some(19));
    // A ninth-frame strike counts the tenth's first two balls.
    assert_eq!(last_score(&[o, o, o, o, o, o, o, o, &[10], &[10, 10, 10]]), Some(60));
    // Not over until the bonus is rolled.
    assert_eq!(last_score(&[o, o, o, o, o, o, o, o, o, &[7, 3]]), None);
    assert_eq!(last_score(&[o, o, o, o, o, o, o, o, o, &[3, 4]]), Some(7));
    // A five-frame game's last frame is its tenth.
    let x: &[u8] = &[10];
    assert_eq!(last_score(&[x, x, x, x, &[10, 10, 10]]), Some(150));
}

/// The inputs of a strike on a fresh rack (found by search, so a physics tweak
/// that loses every strike fails here).
fn a_strike() -> (i64, i64, i64, i64) {
    for power in [100, 90, 80, 70] {
        for spin in [0, 30, -30, 60, -60] {
            for aim in -20..=20 {
                for x in [0, 300, -300, 600, -600, 900, -900] {
                    let r = simulate_roll(&lane(false), x, aim, power, spin, 1);
                    if r.pins_down == FULL {
                        return (x, aim, power, spin);
                    }
                }
            }
        }
    }
    panic!("no strike anywhere");
}

#[test]
fn the_physics_is_sane() {
    // Straight down the middle knocks pins; hard left without bumpers is a gutter.
    let r = simulate_roll(&lane(false), 0, 0, 80, 0, 1);
    assert!(!r.gutter);
    assert!(r.pins_down[0], "the head pin");
    let g = simulate_roll(&lane(false), -X_MAX, -AIM_MAX, 60, -SPIN_MAX, 1);
    assert!(g.gutter);
    assert_eq!(g.pins_down, [false; 10]);
    // The same roll with bumpers stays on the lane.
    let b = simulate_roll(&lane(true), -X_MAX, -AIM_MAX, 60, -SPIN_MAX, 1);
    assert!(!b.gutter);
    // Pins already down are never counted again.
    let mut standing = FULL;
    standing[0] = false;
    let r = simulate_roll(&Lane { bumpers: false, standing }, 0, 0, 80, 0, 1);
    assert!(!r.pins_down[0]);
    // Deterministic.
    assert_eq!(simulate_roll(&lane(false), 123, 7, 55, -40, 2), simulate_roll(&lane(false), 123, 7, 55, -40, 2));
    a_strike();
}

fn seats(n: usize) -> Vec<(String, Value)> {
    (0..n).map(|i| (format!("u{i}"), json!({"userId": format!("u{i}")}))).collect()
}

fn roll_msg(i: (i64, i64, i64, i64)) -> Value {
    json!({"x": i.0, "aim": i.1, "power": i.2, "spin": i.3})
}

#[test]
fn a_roll_out_of_turn_is_refused_and_turns_alternate() {
    let mut b = Bowling::default();
    b.start(&seats(2), &json!({"frames": 5}), 0, 0.0).unwrap();
    assert_eq!(b.roll("u1", &roll_msg((0, 0, 80, 0)), 1.0).unwrap_err(), ERR_TURN);
    let gutter = (-X_MAX, -AIM_MAX, 60, -SPIN_MAX);
    let (ev, ticks) = b.roll("u0", &roll_msg(gutter), 1.0).unwrap();
    assert_eq!(ev["gutter"], json!(true));
    assert_eq!(ev["fell"], json!(0));
    // Too soon, then u0's second ball, then it is u1's frame.
    let later = 1.0 + ticks as f64 / 120.0 + SETTLE + 0.01;
    assert_eq!(b.roll("u0", &roll_msg(gutter), 1.1).unwrap_err(), "wait for the pins to settle");
    assert!(b.roll("u0", &json!({"x": 0, "aim": 0, "power": 50, "spin": 0, "frame": 0, "ball": 0}), later).is_err());
    let (ev, _) = b.roll("u0", &roll_msg(gutter), later).unwrap();
    assert_eq!(ev["complete"], json!(true));
    assert_eq!(b.turn(), Some("u1"));
    assert_eq!(b.roll("u0", &roll_msg(gutter), later + 20.0).unwrap_err(), ERR_TURN);
}

#[test]
fn a_whole_game_of_strikes_scores_and_records() {
    let s = a_strike();
    for (frames, bumpers, key, perfect) in [(10, false, "f10", 300), (5, true, "f5b", 150)] {
        let mut b = Bowling::default();
        b.start(&seats(1), &json!({"frames": frames, "bumpers": bumpers}), 1, 0.0).unwrap();
        let mut t = 0.0;
        let mut n = 0;
        while b.turn().is_some() {
            let (ev, _) = b.roll("u0", &roll_msg(s), t).unwrap();
            assert_eq!(ev["strike"], json!(true));
            t += 100.0;
            n += 1;
        }
        assert_eq!(n, frames + 2);
        let d = b.finish().unwrap();
        assert_eq!(d["key"], json!(key));
        assert_eq!(d["results"][0]["score"], json!(perfect));
        assert_eq!(d["results"][0]["strikes"], json!(frames + 2));
        assert_eq!(d["results"][0]["place"], json!(1));
        let rows = crate::results::rows_from_done("bowl", &d);
        assert_eq!(rows[0].key, key);
        assert_eq!(rows[0].value, Some(perfect));
    }
    assert_eq!(key_of(10, true), "f10b");
    assert_eq!(key_of(5, false), "f5");
}

#[test]
fn places_are_dense_by_score_and_a_dnf_comes_last() {
    let mut b = Bowling::default();
    b.start(&seats(3), &json!({"frames": 5}), 0, 0.0).unwrap();
    for (uid, f) in [("u0", 9u8), ("u1", 9), ("u2", 3)] {
        let p = b.players.get_mut(uid).unwrap();
        p.rolls = vec![vec![f, 0]; 5];
        p.done = 5;
    }
    b.players.get_mut("u0").unwrap().gone = true;
    let d = b.finish().unwrap();
    let res = d["results"].as_array().unwrap();
    assert_eq!(res[0]["user"]["userId"], json!("u1"));
    assert_eq!(res[0]["place"], json!(1));
    assert_eq!(res[1]["place"], json!(2));
    assert_eq!(res[2]["user"]["userId"], json!("u0"));
    assert_eq!(res[2]["dnf"], json!(true));
    assert_eq!(res[2]["place"], json!(3));
    // A dnf row is recorded without a score.
    let rows = crate::results::rows_from_done("bowl", &d);
    assert_eq!(rows[2].value, None);
}

#[test]
fn skip_zeroes_the_rest_of_the_frame() {
    let mut b = Bowling::default();
    b.start(&seats(2), &json!({"frames": 5}), 0, 0.0).unwrap();
    assert!(b.zero_frame("u0"));
    assert_eq!(b.players.get("u0").unwrap().rolls[0], vec![0, 0]);
    assert_eq!(b.turn(), Some("u1"));
    let p = b.players.get_mut("u1").unwrap();
    p.done = 4;
    p.rolls[4] = vec![10];
    assert!(b.zero_frame("u1"));
    assert_eq!(b.players.get("u1").unwrap().rolls[4], vec![10, 0, 0]);
}

#[tokio::test]
async fn a_bowl_event_goes_only_to_the_lobby() {
    let e = env(MAX_TICKERS, 1);
    let (a, mut wa) = e.connect("r", 1, "a").await;
    let (_c, mut wc) = e.connect("r", 3, "c").await;
    e.send("r", 1, &a, GAME, "join", json!({})).await;
    until(&mut wa, GAME, "bowl").await;
    e.send("r", 1, &a, GAME, "start", json!({"frames": 10, "bumpers": true})).await;
    let m = until_where(&mut wa, GAME, "bowl", |m| m["round"]["phase"] == "playing").await;
    assert_eq!(m["round"]["key"], json!("f10b"));
    assert_eq!(m["by"]["userId"], json!("a"));
    e.send("r", 1, &a, GAME, "roll", json!({"x": 0, "aim": 0, "power": 70, "spin": 0, "seq": 4})).await;
    let r = until(&mut wa, GAME, "roll").await;
    assert_eq!(r["seq"], json!(4));
    assert_eq!(r["standing"], json!(FULL));
    let seen = drain(&mut wc);
    assert!(seen.contains(&(GAME.to_string(), "lobby".to_string())), "{seen:?}");
    assert!(!seen.iter().any(|(_, ev)| ev == "bowl" || ev == "roll"), "{seen:?}");
}

#[tokio::test]
async fn the_host_starts_and_turns_are_enforced() {
    let e = env(MAX_TICKERS, 1);
    let (a, mut wa) = e.connect("r", 1, "a").await;
    let (b, mut wb) = e.connect("r", 2, "b").await;
    e.send("r", 1, &a, GAME, "join", json!({})).await;
    e.send("r", 2, &b, GAME, "join", json!({})).await;
    e.send("r", 2, &b, GAME, "start", json!({"frames": 10})).await;
    assert_eq!(until(&mut wb, GAME, "error").await["error"], json!(ERR_HOST));
    e.send("r", 1, &a, GAME, "start", json!({"frames": 7})).await;
    assert_eq!(until(&mut wa, GAME, "error").await["error"], json!(ERR_FRAMES));
    e.send("r", 1, &a, GAME, "start", json!({"frames": 5})).await;
    until_where(&mut wb, GAME, "bowl", |m| m["round"]["turn"]["user"] == "a").await;
    e.send("r", 2, &b, GAME, "roll", json!({"x": 0, "aim": 0, "power": 70, "spin": 0})).await;
    assert_eq!(until(&mut wb, GAME, "error").await["error"], json!(ERR_TURN));
    e.send("r", 2, &b, GAME, "char", json!({"c": 25})).await;
    let c = until(&mut wa, GAME, "char").await;
    assert_eq!((c["user"].clone(), c["c"].clone()), (json!("b"), json!(25)));
    e.send("r", 2, &b, GAME, "char", json!({"c": 2000})).await;
    until(&mut wb, GAME, "error").await;
}

#[tokio::test]
async fn grace_then_skip() {
    let e = env(MAX_TICKERS, 1);
    let (a, wa) = e.connect("r", 1, "a").await;
    let (b, mut wb) = e.connect("r", 2, "b").await;
    e.send("r", 1, &a, GAME, "join", json!({})).await;
    e.send("r", 2, &b, GAME, "join", json!({})).await;
    e.send("r", 1, &a, GAME, "start", json!({"frames": 5})).await;
    until_where(&mut wb, GAME, "bowl", |m| m["round"]["turn"]["user"] == "a").await;
    // a's socket drops on their own turn: the lane waits for them ...
    e.disconnect("r", 1, &a).await;
    let m = until_where(&mut wb, GAME, "bowl", |m| m["left"] == "a").await;
    assert_eq!(m["round"]["players"][0]["away"], json!(true));
    assert_eq!(m["round"]["turn"]["user"], json!("a"));
    e.send("r", 2, &b, GAME, "roll", json!({"x": 0, "aim": 0, "power": 70, "spin": 0})).await;
    assert_eq!(until(&mut wb, GAME, "error").await["error"], json!(ERR_TURN));
    e.advance(GRACE - 1.0);
    e.hub.recheck("r").await;
    let held = drain(&mut wb);
    assert!(!held.iter().any(|(_, ev)| ev == "bowl"), "{held:?}");
    // ... and after the grace they are out and b bowls.
    e.advance(1.5);
    e.hub.recheck("r").await;
    let m = until_where(&mut wb, GAME, "bowl", |m| m["round"]["turn"]["user"] == "b").await;
    assert_eq!(m["round"]["players"][0]["dnf"], json!(true));
    e.send("r", 2, &b, GAME, "roll", json!({"x": 0, "aim": 0, "power": 70, "spin": 0})).await;
    until(&mut wb, GAME, "roll").await;
    drop(wa);
}

#[tokio::test]
async fn a_quick_rejoin_keeps_the_place_and_skip_moves_on() {
    let e = env(MAX_TICKERS, 1);
    let (a, mut wa) = e.connect("r", 1, "a").await;
    let (b, wb) = e.connect("r", 2, "b").await;
    e.send("r", 1, &a, GAME, "join", json!({})).await;
    e.send("r", 2, &b, GAME, "join", json!({})).await;
    e.send("r", 1, &a, GAME, "start", json!({"frames": 5})).await;
    until_where(&mut wa, GAME, "bowl", |m| m["round"]["phase"] == "playing").await;
    e.disconnect("r", 2, &b).await;
    let (b2, mut wb2) = e.connect("r", 4, "b").await;
    e.send("r", 4, &b2, GAME, "join", json!({})).await;
    let m = until_where(&mut wb2, GAME, "bowl", |m| m["back"] == "b").await;
    assert_eq!(m["round"]["players"][1]["away"], json!(false));
    // The host skips their own turn: a frame of misses, then it is b's.
    e.send("r", 1, &a, GAME, "skip", json!({})).await;
    let m = until_where(&mut wa, GAME, "bowl", |m| m["round"]["turn"]["user"] == "b").await;
    assert_eq!(m["round"]["players"][0]["rolls"][0], json!([0, 0]));
    drop(wb);
}

