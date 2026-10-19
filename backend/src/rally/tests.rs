//! Valley Rally: stage validation and canonical keys, gate geometry, the
//! referee's checks (speed, teleport, clock), split and finish timing, DNFs,
//! events, and stages through the room + lobby layer (the per-connection
//! queues handle_socket drains). Modelled on kart/tests.rs.

use super::*;
use crate::realtime::{Registry, MAX_TICKERS};
use std::time::Duration;
use tokio::sync::mpsc;

fn pubv(uid: &str) -> Value {
    json!({"userId": uid, "handle": uid, "displayName": format!("{uid} name"), "avatarUrl": ""})
}

fn members(n: usize, prefix: &str) -> Vec<(String, Value)> {
    (0..n).map(|i| (format!("{prefix}{i}"), pubv(&format!("{prefix}{i}")))).collect()
}

/// A straight road north (-z) from the origin, a point every 10 m.
fn road(n: usize) -> Vec<Value> {
    (0..n).map(|i| json!([0, -(i as i64) * 10, 0])).collect()
}

/// Gates across that road at these distances (m) from the start.
fn gates(ds: &[f64]) -> Vec<Value> {
    ds.iter().map(|d| json!({"x": 0, "z": -d, "h": 0, "w": 8})).collect()
}

const GATES: [f64; 6] = [50.0, 250.0, 450.0, 650.0, 850.0, 990.0];

fn sdata() -> Value {
    json!({"seed": "daily-2026-10-19", "biome": "forest", "night": false, "length": 1000,
           "road": road(101), "gates": gates(&GATES)})
}

fn sdoc() -> Value {
    json!({"kind": "rally", "v": 1, "name": "Daily Stage", "data": sdata()})
}

fn with(mut v: Value, f: impl FnOnce(&mut Value)) -> Value {
    f(&mut v);
    v
}

/// A position frame `d` metres up the road, `x` metres east of it.
fn frame(d: f64, x: f64, q: Option<i64>) -> Value {
    let mut m = json!({"x": (x * 100.0).round() as i64, "z": (-d * 100.0).round() as i64, "y": 0,
                       "yaw": 0, "v": 300});
    if let Some(q) = q {
        m["q"] = json!(q);
    }
    m
}

fn started(n: usize) -> (Rally, f64) {
    let mut r = Rally::new();
    assert_eq!(r.start(&members(n, "u"), &sdoc(), None, 100.0), None);
    assert_eq!(r.phase, Phase::Countdown);
    assert!(r.tick(100.0 + COUNTDOWN - 0.01, true).is_empty());
    r.tick(100.0 + COUNTDOWN, true);
    assert_eq!(r.phase, Phase::Racing);
    (r, 100.0 + COUNTDOWN)
}

/// Drive uid from d0 to d1 (m along the road) at 30 m/s, 20 Hz, `x` m east;
/// the first frame is at d0. Every frame must be accepted.
fn drive(r: &mut Rally, uid: &str, d0: f64, d1: f64, x: f64, t0: f64) -> (f64, Vec<(&'static str, Value)>) {
    let (mut t, mut d) = (t0 + 0.05, d0);
    let mut evs = Vec::new();
    let back = d1 < d0;
    loop {
        let (fix, e) = r.pos(uid, &frame(d, x, None), t);
        assert!(fix.is_none(), "{uid} at {d}: {fix:?}");
        evs.extend(e);
        if d == d1 || r.player(uid).unwrap().fin.is_some() {
            return (t, evs);
        }
        d = if back { d1.max(d - 1.5) } else { d1.min(d + 1.5) };
        t += 0.05;
    }
}

fn names(evs: &[(&'static str, Value)]) -> Vec<&'static str> {
    evs.iter().map(|e| e.0).collect()
}

fn is_ckey(k: &str) -> bool {
    k.len() == 14 && k.starts_with("c-") && k[2..].chars().all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
}

fn keys(v: &Value) -> Vec<String> {
    v.as_object().unwrap().keys().cloned().collect()
}

// --------------------------------------------------------------- validation --
#[test]
fn a_good_stage_validates_to_its_canonical_form() {
    let c = validate_custom(&sdata()).unwrap();
    assert_eq!(keys(&c), ["seed", "biome", "night", "length", "road", "gates"]);
    assert_eq!(c["road"][3], json!([0.0, -30.0, 0.0]));
    assert_eq!(c["gates"][0], json!({"x": 0.0, "z": -50.0, "h": 0, "w": 8.0}));
    assert_eq!(c["length"], 1000);
    // canonical is a fixed point
    assert_eq!(validate_custom(&c).unwrap(), c);
    let s = stage_from_doc(&sdoc()).unwrap();
    assert!(is_ckey(&s.key), "{}", s.key);
    assert_eq!((s.road.len(), s.gates.len(), s.name.as_str()), (101, 6, "Daily Stage"));
    assert_eq!(s.doc(), json!({"kind": "rally", "v": 1, "name": "Daily Stage", "data": c}));
    assert_eq!(s.start(), (0.0, 0.0, 0.0, 0));
}

#[test]
fn canonical_rounds_normalises_and_drops_unknown_keys() {
    let mut d = sdata();
    d["road"][1] = json!([0.04, -10.04, 3.06]);
    d["road"][2] = json!([-0.04, -20.0, 0]);
    d["gates"][1] = json!({"w": 7.96, "h": -360, "z": -250.01, "x": 0.0, "colour": "red"});
    d["gates"][2]["h"] = json!(359.6);
    d["length"] = json!(1000.4);
    d["extra"] = json!({"x": 1});
    d["theme"] = json!("#fff");
    let c = validate_custom(&d).unwrap();
    assert_eq!(keys(&c), ["seed", "biome", "night", "length", "road", "gates"]);
    assert_eq!(c["road"][1], json!([0.0, -10.0, 3.1]));
    // -0.04 rounds to 0.0, never -0.0 (which would change the key)
    assert_eq!(serde_json::to_string(&c["road"][2]).unwrap(), "[0.0,-20.0,0.0]");
    assert_eq!(c["gates"][1], json!({"x": 0.0, "z": -250.0, "h": 0, "w": 8.0}));
    assert_eq!(c["gates"][2]["h"], 0); // 359.6 -> 360 -> 0
    assert_eq!(c["length"], 1000);
    assert!(c.get("extra").is_none() && c.get("theme").is_none());
    assert_eq!(validate_custom(&c).unwrap(), c);
}

#[test]
fn the_key_is_stable_across_key_order_rounding_and_name() {
    let base = crate::mapkey::content_key("rally", &validate_custom(&sdata()).unwrap());
    assert!(is_ckey(&base));
    // shuffled keys, integer vs float, sub-rounding noise, extra keys
    let shuffled: Value = serde_json::from_str(&format!(
        r#"{{"gates": {}, "road": {}, "length": 1000.0, "night": false, "biome": "forest",
             "seed": "daily-2026-10-19", "v": 9}}"#,
        serde_json::to_string(&gates(&GATES)).unwrap(),
        serde_json::to_string(&road(101)).unwrap()
    ))
    .unwrap();
    let noisy = with(sdata(), |d| {
        d["road"][5] = json!([0.01, -50.04, -0.0]);
        d["gates"][0]["w"] = json!(8.04);
    });
    for d in [shuffled, noisy] {
        assert_eq!(crate::mapkey::content_key("rally", &validate_custom(&d).unwrap()), base);
    }
    // the name is not geometry: same stage, same board
    let renamed = with(sdoc(), |d| d["name"] = json!("Another name"));
    assert_eq!(stage_from_doc(&renamed).unwrap().key, base);
    assert_eq!(stage_from_doc(&sdoc()).unwrap().key, base);
    // a different seed, biome or night is a different stage
    for d in [with(sdata(), |d| d["seed"] = json!("daily-2026-10-20")),
              with(sdata(), |d| d["biome"] = json!("snow")),
              with(sdata(), |d| d["night"] = json!(true))] {
        assert_ne!(crate::mapkey::content_key("rally", &validate_custom(&d).unwrap()), base);
    }
    // and rally keys never collide with another kind's on the same data
    let c = validate_custom(&sdata()).unwrap();
    assert_ne!(crate::mapkey::content_key("kart", &c), base);
}

#[test]
fn bad_stage_data_is_refused_with_a_reason_and_no_panic() {
    let cases: Vec<(Value, &str)> = vec![
        (Value::Null, "object"),
        (json!([1, 2]), "object"),
        (with(sdata(), |d| d["seed"] = json!(5)), "seed"),
        (with(sdata(), |d| d["seed"] = json!("x".repeat(65))), "seed"),
        (with(sdata(), |d| d["seed"] = json!("a\nb")), "seed"),
        (with(sdata(), |d| d["biome"] = json!("desert")), "biome"),
        (with(sdata(), |d| d["night"] = json!(1)), "night"),
        (with(sdata(), |d| d["length"] = json!(999)), "length"),
        (with(sdata(), |d| d["length"] = json!(6001)), "length"),
        (with(sdata(), |d| d["length"] = json!("long")), "length"),
        (with(sdata(), |d| d["road"] = json!("north")), "road"),
        (with(sdata(), |d| d["road"] = json!(road(49))), "50 to 400 points"),
        (with(sdata(), |d| d["road"] = json!(road(401))), "50 to 400 points"),
        (with(sdata(), |d| d["road"][7] = json!([0, -70])), "road point 7"),
        (with(sdata(), |d| d["road"][7] = json!([0, "x", 0])), "road point 7"),
        (with(sdata(), |d| d["road"][7] = json!({"x": 0})), "road point 7"),
        (with(sdata(), |d| d["road"][100] = json!([0, -5001, 0])), "5000"),
        (with(sdata(), |d| d["road"][0] = json!([0, 0, 5000.1])), "5000"),
        (with(sdata(), |d| d["road"][7] = json!([0, -61.5, 0])), "1.5 m apart"),
        (with(sdata(), |d| d["road"][7] = json!([35, -70, 0])), "apart"),
        (with(sdata(), |d| d["gates"] = json!(gates(&[50.0, 60.0, 70.0, 80.0]))), "5 to 40 gates"),
        (with(sdata(), |d| d["gates"] = json!(gates(&(1..=41).map(|i| i as f64 * 20.0).collect::<Vec<_>>()))),
         "5 to 40 gates"),
        (with(sdata(), |d| d["gates"][2] = json!(3)), "gate 2"),
        (with(sdata(), |d| d["gates"][2]["x"] = json!("0")), "gate 2"),
        (with(sdata(), |d| d["gates"][2].as_object_mut().unwrap().remove("h").map(|_| ()).unwrap()), "heading"),
        (with(sdata(), |d| d["gates"][2]["w"] = json!(3.9)), "half-width"),
        (with(sdata(), |d| d["gates"][2]["w"] = json!(20.1)), "half-width"),
        (with(sdata(), |d| d["gates"][2]["x"] = json!(15.2)), "within 15 m"),
        (with(sdata(), |d| d["gates"][2]["z"] = json!(-6000)), "5000"),
        // out of order along the road
        (with(sdata(), |d| d["gates"] = json!(gates(&[50.0, 450.0, 250.0, 650.0, 990.0]))), "gate 2"),
        (with(sdata(), |d| d["pad"] = json!("x".repeat(33 * 1024))), "too big"),
    ];
    for (data, why) in cases {
        let e = validate_custom(&data).expect_err(why);
        assert!(e.contains(why), "{e} / {why}");
        let mut r = Rally::new();
        let doc = json!({"kind": "rally", "v": 1, "name": "x", "data": data});
        let got = r.start(&members(1, "u"), &doc, None, 0.0).unwrap();
        assert!(got.starts_with("bad stage: "), "{got}");
        assert_eq!(r.phase, Phase::Idle);
        assert!(r.stage.is_none());
    }
    // a gate exactly 15 m off the road is in; the edges of every range are in
    let ok = with(sdata(), |d| {
        d["gates"][2]["x"] = json!(15);
        d["gates"][1]["w"] = json!(4);
        d["gates"][3]["w"] = json!(20);
        d["length"] = json!(6000);
        d["seed"] = json!("s".repeat(64));
        d["road"][7] = json!([0, -62, 0]); // 8 m then 12 m: inside 2..30
    });
    validate_custom(&ok).unwrap();
}

#[test]
fn the_document_around_the_data_is_checked() {
    let bad_docs = [
        (json!("nope"), "map document"),
        (with(sdoc(), |d| d["kind"] = json!("kart")), "not a rally stage"),
        (with(sdoc(), |d| d["v"] = json!(2)), "newer editor"),
        (with(sdoc(), |d| d["name"] = json!("   ")), "name"),
        (with(sdoc(), |d| d["name"] = json!("a".repeat(41))), "at most 40"),
        (with(sdoc(), |d| d["name"] = json!("<b>stage</b>")), "< >"),
        (with(sdoc(), |d| d["name"] = json!("see www.x.io")), "links"),
        (with(sdoc(), |d| d["name"] = json!("tab\there")), "control"),
        (with(sdoc(), |d| { d.as_object_mut().unwrap().remove("name"); }), "needs a name"),
        (with(sdoc(), |d| { d.as_object_mut().unwrap().remove("data"); }), "object"),
    ];
    for (d, why) in bad_docs {
        let e = stage_from_doc(&d).unwrap_err();
        assert!(e.contains(why), "{e} / {why}");
    }
    // 40 characters, trimmed, is a fine name
    let s = stage_from_doc(&with(sdoc(), |d| d["name"] = json!(format!("  {}  ", "é".repeat(40))))).unwrap();
    assert_eq!(s.name.chars().count(), 40);
}

#[test]
fn a_full_size_stage_fits_the_byte_budget() {
    // 400 points at the far corner of the map, 40 gates: the largest the
    // rounding allows still fits 12 KiB (the gallery's and a frame's budget).
    let road: Vec<Value> = (0..400).map(|i| json!([-4999.9 + i as f64 * 10.0, -4999.9, -999.9])).collect();
    let gates: Vec<Value> = (0..40)
        .map(|i| json!({"x": -4999.9 + 50.0 + i as f64 * 98.0, "z": -4987.9, "h": 270, "w": 19.9}))
        .collect();
    let d = json!({"seed": "daily-2026-10-19", "biome": "snow", "night": true, "length": 4000,
                   "road": road, "gates": gates});
    let c = validate_custom(&d).unwrap();
    assert!(serde_json::to_string(&c).unwrap().len() <= MAX_BYTES);
}

#[test]
fn a_curving_road_places_gates_by_distance_along_it() {
    // A road that loops back past itself: a hairpin. The return leg runs 12 m
    // beside the outbound one, so "near the road" alone can't order the gates.
    let mut pts = Vec::new();
    for i in 0..30 {
        pts.push(json!([0, -(i as i64) * 10, 0])); // north 290 m
    }
    for i in 0..30 {
        pts.push(json!([12, -290 + (i as i64) * 10, 0])); // back south, 12 m east
    }
    let d = json!({"seed": "hairpin", "biome": "hills", "night": false, "length": 1000, "road": pts,
                   "gates": [{"x": 0, "z": -50, "h": 0, "w": 5}, {"x": 0, "z": -250, "h": 0, "w": 5},
                             {"x": 12, "z": -250, "h": 180, "w": 5}, {"x": 12, "z": -100, "h": 180, "w": 5},
                             {"x": 12, "z": -10, "h": 180, "w": 5}]});
    validate_custom(&d).expect("gates on the return leg come later along the road");
}

// ------------------------------------------------------------ gate geometry --
#[test]
fn a_gate_is_crossed_forward_inside_its_half_width() {
    let g = Gate { x: 0.0, z: -50.0, h: 0, w: 8.0 }; // across the road, facing north
    assert_eq!(g.crossed((0.0, -45.0), (0.0, -55.0)), Some(0.5));
    // touching the line from behind counts; leaving it does not count again
    assert_eq!(g.crossed((0.0, -45.0), (0.0, -50.0)), Some(1.0));
    assert_eq!(g.crossed((0.0, -50.0), (0.0, -55.0)), None);
    // backwards never counts
    assert_eq!(g.crossed((0.0, -55.0), (0.0, -45.0)), None);
    // not reaching the line
    assert_eq!(g.crossed((0.0, -40.0), (0.0, -49.9)), None);
    // outside the half-width, either side; the edge is in
    assert_eq!(g.crossed((8.5, -45.0), (8.5, -55.0)), None);
    assert_eq!(g.crossed((-8.5, -45.0), (-8.5, -55.0)), None);
    assert!(g.crossed((8.0, -45.0), (8.0, -55.0)).is_some());
    // a diagonal through the line: where it crosses is what matters
    assert!(g.crossed((-12.0, -46.0), (4.0, -54.0)).is_some()); // crosses at x = -4
    assert_eq!(g.crossed((4.0, -46.0), (20.0, -54.0)), None); // crosses at x = 12
    // a gate facing east (h = 90): its line runs north-south
    let e = Gate { x: 100.0, z: 0.0, h: 90, w: 5.0 };
    assert!(e.crossed((95.0, 2.0), (105.0, 2.0)).is_some());
    assert_eq!(e.crossed((105.0, 2.0), (95.0, 2.0)), None);
    assert_eq!(e.crossed((95.0, 6.0), (105.0, 6.0)), None);
    // h = 225: driving south-west
    let sw = Gate { x: 0.0, z: 0.0, h: 225, w: 6.0 };
    assert!(sw.crossed((3.0, -3.0), (-3.0, 3.0)).is_some());
    assert_eq!(sw.crossed((-3.0, 3.0), (3.0, -3.0)), None);
}

// ------------------------------------------------------------------ referee --
#[test]
fn gates_count_in_order_and_the_last_one_finishes() {
    let (mut r, t) = started(1);
    let (t, evs) = drive(&mut r, "u0", 0.0, 1000.0, 0.0, t);
    assert_eq!(names(&evs), ["split", "split", "split", "split", "split", "finish"]);
    for (i, (_, v)) in evs.iter().take(5).enumerate() {
        assert_eq!(v["gate"], i);
        assert_eq!(v["uid"], "u0");
    }
    let p = r.player("u0").unwrap();
    assert_eq!(p.splits.len(), 6);
    assert_eq!(p.fin, Some(*p.splits.last().unwrap()));
    assert!(p.splits.windows(2).all(|w| w[0] < w[1]));
    // finished: frames are ignored, the stage is done on the next tick
    assert_eq!(r.pos("u0", &frame(995.0, 0.0, None), t + 0.05), (None, vec![]));
    let evs = r.tick(t + 0.1, true);
    assert_eq!(names(&evs), ["done"]);
    assert_eq!(r.phase, Phase::Done);
    assert!(!r.running());
}

#[test]
fn splits_and_the_finish_are_timed_on_the_server_clock_from_go() {
    let (mut r, t) = started(1);
    let (_, evs) = drive(&mut r, "u0", 0.0, 1000.0, 0.0, t);
    // anchored at d = 0 at go + 0.05 s, then 30 m/s: gate d is crossed at go + 0.05 + d / 30
    for (i, d) in GATES.iter().enumerate() {
        let want = ((0.05 + d / 30.0) * 1000.0).round() as i64;
        let got = evs[i].1["ms"].as_i64().unwrap();
        assert!((got - want).abs() <= 1, "gate {i}: {got} vs {want}");
    }
    assert_eq!(evs[5], ("finish", json!({"uid": "u0", "ms": evs[5].1["ms"]})));
}

#[test]
fn skipping_a_gate_means_later_gates_do_not_count_until_it_is_taken() {
    let (mut r, t) = started(1);
    let (t, evs) = drive(&mut r, "u0", 0.0, 200.0, 0.0, t);
    assert_eq!(names(&evs), ["split"]); // gate 0
    // round gate 1 off-road (off-road is allowed), then through gate 2
    let (t, evs) = drive(&mut r, "u0", 200.0, 200.0, 0.0, t);
    assert!(evs.is_empty());
    let mut t = t;
    for k in 1..=10 {
        t += 0.05;
        let (fix, _) = r.pos("u0", &frame(200.0, k as f64 * 1.5, None), t);
        assert!(fix.is_none());
    }
    let (t, evs) = drive(&mut r, "u0", 200.0, 470.0, 15.0, t);
    assert!(evs.is_empty(), "gate 1 was skipped, so gate 2 does not count: {evs:?}");
    assert_eq!(r.player("u0").unwrap().next, 1);
    // back to the road behind gate 1 and through it: now 1, then 2 counts on the way past
    let (t, _) = drive(&mut r, "u0", 470.0, 240.0, 15.0, t);
    let mut t = t;
    for k in 1..=10 {
        t += 0.05;
        r.pos("u0", &frame(240.0, 15.0 - k as f64 * 1.5, None), t);
    }
    let (_, evs) = drive(&mut r, "u0", 240.0, 460.0, 0.0, t);
    assert_eq!(evs.iter().map(|e| e.1["gate"].as_i64().unwrap()).collect::<Vec<_>>(), [1, 2]);
}

#[test]
fn crossing_backwards_does_not_count_and_does_not_undo() {
    // gate 0 20 m up the road, inside the start radius
    let doc = with(sdoc(), |d| d["data"]["gates"] = json!(gates(&[20.0, 250.0, 450.0, 650.0, 990.0])));
    let mut r = Rally::new();
    assert_eq!(r.start(&members(1, "u"), &doc, None, 0.0), None);
    r.tick(COUNTDOWN, true);
    // anchored just past gate 0: no split for the anchor
    let (t, evs) = drive(&mut r, "u0", 25.0, 25.0, 0.0, COUNTDOWN);
    assert!(evs.is_empty());
    // reverse back over gate 0: nothing
    let (t, evs) = drive(&mut r, "u0", 25.0, 10.0, 0.0, t);
    assert!(evs.is_empty());
    // forward over it: that is the crossing
    let (t, evs) = drive(&mut r, "u0", 10.0, 30.0, 0.0, t);
    assert_eq!(names(&evs), ["split"]);
    // back and forth again: gate 0 is behind us for good
    let (t, _) = drive(&mut r, "u0", 30.0, 10.0, 0.0, t);
    let (t, evs) = drive(&mut r, "u0", 10.0, 30.0, 0.0, t);
    assert!(evs.is_empty());
    assert_eq!(r.player("u0").unwrap().next, 1);
    // gate 1 driven through backwards from beyond (off-road round it first) is no crossing
    let mut t = t;
    for k in 1..=10 {
        t += 0.05;
        r.pos("u0", &frame(30.0, k as f64 * 1.5, None), t);
    }
    let (t, _) = drive(&mut r, "u0", 30.0, 260.0, 15.0, t);
    let mut t = t;
    for k in 1..=10 {
        t += 0.05;
        r.pos("u0", &frame(260.0, 15.0 - k as f64 * 1.5, None), t);
    }
    let (_, evs) = drive(&mut r, "u0", 260.0, 240.0, 0.0, t);
    assert!(evs.is_empty());
    assert_eq!(r.player("u0").unwrap().next, 1);
}

#[test]
fn the_first_frame_must_be_near_the_start() {
    let (mut r, t) = started(2);
    // 35 m up the road is past the start radius: back to the start
    let (fix, evs) = r.pos("u0", &frame(35.0, 0.0, None), t + 0.05);
    assert_eq!(fix, Some(json!({"uid": "u0", "x": 0, "z": 0, "y": 0, "yaw": 0})));
    assert!(evs.is_empty() && !r.player("u0").unwrap().anchored);
    // 20 m behind the start, 10 m to the side: fine (a grid spot)
    let (fix, _) = r.pos("u1", &frame(-20.0, 10.0, None), t + 0.05);
    assert!(fix.is_none() && r.player("u1").unwrap().anchored);
    // before the green light every frame is ignored
    let mut r = Rally::new();
    r.start(&members(1, "u"), &sdoc(), None, 0.0);
    assert_eq!(r.pos("u0", &frame(0.0, 0.0, None), 1.0), (None, vec![]));
    assert!(!r.player("u0").unwrap().anchored);
    // a frame from someone not in the stage is ignored
    let (mut r, t) = started(1);
    assert_eq!(r.pos("ghost", &frame(0.0, 0.0, None), t + 0.1), (None, vec![]));
}

#[test]
fn too_fast_or_a_teleport_is_fixed_back_to_the_last_accepted_point() {
    let (mut r, t) = started(1);
    let (t, _) = drive(&mut r, "u0", 0.0, 30.0, 0.0, t);
    // 10 m in 0.05 s is 200 m/s
    let (fix, evs) = r.pos("u0", &frame(40.0, 0.0, None), t + 0.05);
    assert_eq!(fix, Some(json!({"uid": "u0", "x": 0, "z": -3000, "y": 0, "yaw": 0})));
    assert!(evs.is_empty());
    assert_eq!(r.player("u0").unwrap().z, -30.0);
    // a second bad frame inside FIX_GAP is dropped without another fix
    assert_eq!(r.pos("u0", &frame(45.0, 0.0, None), t + 0.1), (None, vec![]));
    assert_eq!(r.player("u0").unwrap().bad, 2);
    // after the gap the car may carry on from where it was accepted
    let t = t + 0.6;
    assert_eq!(r.pos("u0", &frame(31.5, 0.0, None), t).0, None);
    // 75 m/s is the limit, with slack: 3.75 + 2.5 m in 0.05 s passes, 6.5 + 2.5... fails
    assert_eq!(r.pos("u0", &frame(31.5 + 6.2, 0.0, None), t + 0.05).0, None);
    assert!(r.pos("u0", &frame(31.5 + 6.2 + 6.4, 0.0, None), t + 0.1).0.is_some());
    // a teleport: a long gap by the sender's clock still caps one step at MAX_STEP
    let (mut r, t) = started(1);
    assert!(r.pos("u0", &frame(0.0, 0.0, Some(100)), t + 0.05).0.is_none());
    assert!(r.pos("u0", &frame(10.0, 0.0, Some(400)), t + 3.0).0.is_none()); // 10 m in 3 s
    let (fix, _) = r.pos("u0", &frame(65.0, 0.0, Some(700)), t + 6.0); // 55 m in 3 s: under 75 m/s, over MAX_STEP
    assert!(fix.is_some());
    // nor can the sender's clock run ahead of ours to buy speed
    let (mut r, t) = started(1);
    assert!(r.pos("u0", &frame(0.0, 0.0, Some(1000)), t + 0.05).0.is_none());
    let (fix, _) = r.pos("u0", &frame(20.0, 0.0, Some(1100)), t + 0.10); // claims 1 s passed in 0.05
    assert!(fix.is_some());
    // stale (older-q) frames are ignored, not fixed
    let (mut r, t) = started(1);
    r.pos("u0", &frame(0.0, 0.0, Some(500)), t + 0.05);
    r.pos("u0", &frame(1.5, 0.0, Some(505)), t + 0.10);
    assert_eq!(r.pos("u0", &frame(0.5, 0.0, Some(503)), t + 0.15), (None, vec![]));
    assert_eq!(r.player("u0").unwrap().bad, 0);
    // way off the map is never a position
    assert!(r.pos("u0", &frame(1.5, 6000.0, Some(900)), t + 5.0).0.is_some());
}

#[test]
fn off_road_driving_is_allowed() {
    let (mut r, t) = started(1);
    let (t, _) = drive(&mut r, "u0", 0.0, 20.0, 0.0, t);
    let mut t = t;
    for k in 1..=40 {
        t += 0.05;
        assert!(r.pos("u0", &frame(20.0, k as f64 * 1.5, None), t).0.is_none(), "60 m into the trees is fine");
    }
    assert_eq!(r.player("u0").unwrap().x, 60.0);
}

#[test]
fn position_frames_are_rate_limited() {
    let (mut r, t) = started(1);
    let mut taken = 0;
    for i in 0..30 {
        // 30 frames at one instant: the burst allows 10
        let before = r.player("u0").unwrap().at;
        r.pos("u0", &frame(i as f64 * 0.01, 0.0, None), t + 0.05);
        if r.player("u0").unwrap().at != before || i == 0 {
            taken += 1;
        }
    }
    assert!(r.player("u0").unwrap().bucket.0 < 1.0);
    assert!(taken <= POS_BURST as usize + 1, "{taken}");
}

#[test]
fn snapshots_carry_only_the_cars_that_moved() {
    let (mut r, t) = started(3);
    r.pos("u0", &frame(0.0, 0.0, Some(7)), t + 0.05);
    r.pos("u1", &with(frame(0.0, 3.0, None), |f| f["dr"] = json!(1)), t + 0.05);
    let evs = r.tick(t + 0.066, true);
    assert_eq!(names(&evs), ["snap"]);
    let snap = &evs[0].1;
    assert_eq!(snap["t"], wire_ms(t + 0.066));
    assert_eq!(snap["cars"], json!([
        {"uid": "u0", "x": 0, "z": 0, "y": 0, "yaw": 0, "v": 300},
        {"uid": "u1", "x": 300, "z": 0, "y": 0, "yaw": 0, "v": 300, "dr": 1}]));
    // nothing moved: no snapshot; a thinned tick (send = false) holds it back
    assert!(r.tick(t + 0.13, true).is_empty());
    r.pos("u2", &frame(0.0, -3.0, None), t + 0.15);
    assert!(r.tick(t + 0.2, false).is_empty());
    assert_eq!(r.tick(t + 0.26, true)[0].1["cars"].as_array().unwrap().len(), 1);
}

// ----------------------------------------------------------- dnf and done --
#[test]
fn a_leave_is_a_dnf_and_done_comes_when_everyone_is_in() {
    let (mut r, t) = started(3);
    let (t, _) = drive(&mut r, "u0", 0.0, 1000.0, 0.0, t);
    assert_eq!(r.drop_player("u1", t, false), Some(json!({"uid": "u1", "reason": "left"})));
    assert!(names(&r.tick(t + 0.07, true)).iter().all(|e| *e != "done"), "u2 is still driving");
    // u2's socket drops: the run waits GRACE, then it's a DNF
    assert_eq!(r.drop_player("u2", t + 0.1, true), None);
    assert!(r.player("u2").unwrap().away.is_some());
    assert!(r.tick(t + 0.1 + GRACE - 0.5, true).is_empty());
    let evs = r.tick(t + 0.1 + GRACE, true);
    assert_eq!(evs[0], ("dnf", json!({"uid": "u2", "reason": "disconnect"})));
    let done = &evs.iter().find(|e| e.0 == "done").expect("done").1;
    let res = done["results"].as_array().unwrap();
    assert_eq!(res.len(), 3);
    assert_eq!(keys(&res[0]), ["uid", "name", "ms", "car", "place", "dnf", "splits"]);
    assert_eq!((res[0]["uid"].as_str(), res[0]["name"].as_str()), (Some("u0"), Some("u0 name")));
    assert!(res[0]["ms"].is_i64() && res[0]["dnf"] == false);
    for (i, uid) in ["u1", "u2"].iter().enumerate() {
        assert_eq!(res[i + 1]["uid"], *uid);
        assert_eq!(res[i + 1]["ms"], Value::Null);
        assert_eq!(res[i + 1]["dnf"], true);
        assert_eq!(res[i + 1]["place"], i + 2);
    }
    assert!(done["key"].as_str().is_some_and(is_ckey));
    assert_eq!(done["name"], "Daily Stage");
    assert!(done.get("event").is_none() && res[0].get("total").is_none());
    assert_eq!(r.results.as_ref(), Some(done));
}

#[test]
fn a_dropped_racer_back_in_time_carries_on_from_where_they_were() {
    let (mut r, t) = started(2);
    let (t, _) = drive(&mut r, "u0", 0.0, 60.0, 0.0, t);
    r.drop_player("u0", t, true);
    assert_eq!(r.pos("u0", &frame(61.5, 0.0, None), t + 0.05), (None, vec![]), "away: ignored");
    let fix = r.restore("u0", pubv("u0"), t + 3.0).unwrap();
    assert_eq!(fix, json!({"uid": "u0", "x": 0, "z": -6000, "y": 0, "yaw": 0}));
    assert!(r.pos("u0", &frame(61.5, 0.0, Some(3)), t + 3.05).0.is_none());
    // nobody to restore once the DNF is in
    r.drop_player("u1", t, false);
    assert!(r.restore("u1", pubv("u1"), t + 3.0).is_none());
    // in the countdown, leaving just takes the car off the grid
    let mut r = Rally::new();
    r.start(&members(3, "u"), &sdoc(), None, 0.0);
    assert_eq!(r.drop_player("u1", 1.0, false), None);
    assert_eq!(r.players.iter().map(|(u, _)| u.as_str()).collect::<Vec<_>>(), ["u0", "u2"]);
}

#[test]
fn the_stage_is_called_after_ten_minutes_or_three_times_the_leaders_time() {
    let (mut r, _) = started(2);
    let go = r.go_at;
    assert_eq!(r.deadline(), go + MIN_STAGE);
    assert!(r.tick(go + MIN_STAGE - 1.0, true).iter().all(|e| e.0 != "dnf"));
    let evs = r.tick(go + MIN_STAGE, true);
    assert_eq!(names(&evs), ["dnf", "dnf", "done"]);
    assert_eq!(evs[0].1, json!({"uid": "u0", "reason": "timeout"}));
    // a slow leader stretches it: 300 s x 3 = 900 s
    let (mut r, _) = started(2);
    let go = r.go_at;
    r.players[0].1.fin = Some(300_000);
    assert_eq!(r.deadline(), go + 900.0);
    assert!(r.tick(go + 899.0, true).iter().all(|e| e.0 != "dnf"));
    let evs = r.tick(go + 900.0, true);
    assert_eq!(names(&evs), ["dnf", "done"]);
    assert_eq!(evs[1].1["results"][0]["ms"], 300_000);
    // a quick leader doesn't shorten it below ten minutes
    let (mut r, t) = started(2);
    drive(&mut r, "u0", 0.0, 1000.0, 0.0, t);
    assert_eq!(r.deadline(), r.go_at + MIN_STAGE);
}

#[test]
fn results_rank_finishers_by_time() {
    let (mut r, t) = started(3);
    // u2 starts later than u1 but drives the same: u1 first, u2 second; u0 never moves
    let (_, _) = drive(&mut r, "u1", 0.0, 1000.0, 0.0, t);
    let (t2, _) = drive(&mut r, "u2", 0.0, 1000.0, 0.0, t + 1.0);
    r.drop_player("u0", t2, false);
    let done = r.tick(t2 + 0.07, true).into_iter().find(|e| e.0 == "done").unwrap().1;
    let order: Vec<&str> = done["results"].as_array().unwrap().iter().map(|x| x["uid"].as_str().unwrap()).collect();
    assert_eq!(order, ["u1", "u2", "u0"]);
    let rows = crate::results::rows_from_done("rally", &done);
    assert_eq!(rows.iter().map(|x| (x.place, x.value.is_some())).collect::<Vec<_>>(),
               [(1, true), (2, true), (3, false)]);
    assert_eq!(rows[0].key, done["key"].as_str().unwrap());
}

// ------------------------------------------------------------------ events --
fn run_stage(r: &mut Rally, n: usize, index: i64, count: i64, finishers: &[usize], t0: f64) -> Value {
    let ev = json!({"index": index, "count": count});
    assert_eq!(r.start(&members(n, "u"), &sdoc(), Some(&ev), t0), None);
    let t = t0 + COUNTDOWN;
    r.tick(t, true);
    let mut end = t;
    for (k, &i) in finishers.iter().enumerate() {
        // the k-th finisher starts k seconds late
        end = end.max(drive(r, &format!("u{i}"), 0.0, 1000.0, 0.0, t + k as f64).0);
    }
    for i in 0..n {
        if !finishers.contains(&i) {
            r.drop_player(&format!("u{i}"), end, false);
        }
    }
    r.tick(end + 0.07, true).into_iter().find(|e| e.0 == "done").expect("done").1
}

fn by_uid<'a>(done: &'a Value, uid: &str) -> &'a Value {
    done["results"].as_array().unwrap().iter().find(|x| x["uid"] == uid).unwrap()
}

#[test]
fn an_event_carries_cumulative_time_across_its_stages() {
    let mut r = Rally::new();
    let d0 = run_stage(&mut r, 3, 0, 3, &[0, 1, 2], 100.0);
    assert_eq!(d0["event"], json!({"index": 0, "count": 3}));
    for u in ["u0", "u1", "u2"] {
        assert_eq!(by_uid(&d0, u)["total"], by_uid(&d0, u)["ms"]);
    }
    let d1 = run_stage(&mut r, 3, 1, 3, &[1, 0], 1000.0); // u2 DNFs stage 2
    let sum = |u: &str| by_uid(&d0, u)["ms"].as_i64().unwrap() + by_uid(&d1, u)["ms"].as_i64().unwrap();
    assert_eq!(by_uid(&d1, "u0")["total"], sum("u0"));
    assert_eq!(by_uid(&d1, "u1")["total"], sum("u1"));
    assert_eq!(by_uid(&d1, "u2")["total"], Value::Null);
    let d2 = run_stage(&mut r, 3, 2, 3, &[0, 1, 2], 2000.0);
    let t0 = sum("u0") + by_uid(&d2, "u0")["ms"].as_i64().unwrap();
    assert_eq!(by_uid(&d2, "u0")["total"], t0);
    assert_eq!(by_uid(&d2, "u2")["total"], Value::Null, "a DNF anywhere in the event has no total");
    // the stage's own rows are what results record: one board per stage
    assert!(crate::results::rows_from_done("rally", &d2).iter().all(|x| x.mode == "event"));
    // a new event starting at index 0 forgets the old totals
    let d = run_stage(&mut r, 3, 0, 2, &[2], 3000.0);
    assert_eq!(by_uid(&d, "u2")["total"], by_uid(&d, "u2")["ms"]);
}

#[test]
fn an_event_stage_out_of_sequence_has_no_totals_and_a_lone_stage_none_at_all() {
    // index 1 with no index 0 run in this room: nobody has a complete total
    let mut r = Rally::new();
    let d = run_stage(&mut r, 2, 1, 2, &[0, 1], 100.0);
    assert_eq!(by_uid(&d, "u0")["total"], Value::Null);
    // a re-run of an aborted stage keeps the event's totals
    let mut r = Rally::new();
    run_stage(&mut r, 1, 0, 2, &[0], 100.0);
    r.start(&members(1, "u"), &sdoc(), Some(&json!({"index": 1, "count": 2})), 500.0);
    r.end();
    let d = run_stage(&mut r, 1, 1, 2, &[0], 600.0);
    assert!(by_uid(&d, "u0")["total"].is_i64());
    // count 1 is a plain stage: no event, no total
    let mut r = Rally::new();
    let d = run_stage(&mut r, 1, 0, 1, &[0], 100.0);
    assert!(d.get("event").is_none() && by_uid(&d, "u0").get("total").is_none());
    // a bad event is refused
    for ev in [json!({"index": 5, "count": 5}), json!({"index": 2, "count": 2}), json!({"index": 0, "count": 6}),
               json!({"index": 0}), json!({"index": "0", "count": 2}), json!(3)] {
        let mut r = Rally::new();
        let e = r.start(&members(1, "u"), &sdoc(), Some(&ev), 0.0).unwrap();
        assert!(e.starts_with("bad event"), "{ev}: {e}");
        assert_eq!(r.phase, Phase::Idle);
    }
}

#[test]
fn start_end_and_cars() {
    let mut r = Rally::new();
    r.cars.insert("u1".into(), 6);
    assert_eq!(r.start(&members(10, "u"), &sdoc(), None, 0.0), None);
    assert_eq!(r.players.len(), MAX_PLAYERS, "eight cars at most");
    assert_eq!(r.player("u1").unwrap().car, 6);
    assert_eq!(r.player("u3").unwrap().car, 3);
    assert!(r.start(&members(2, "u"), &sdoc(), None, 1.0).unwrap().contains("already running"));
    for bad in [json!({"car": 8}), json!({"car": -1}), json!({"car": 1.5}), json!({"car": "2"}), json!({})] {
        assert_eq!(r.car("u0", &bad), None, "{bad}");
    }
    assert_eq!(r.car("u0", &json!({"car": 7})), Some(7));
    assert_eq!(r.player("u0").unwrap().car, 7, "the car can change on the grid");
    r.tick(COUNTDOWN, true);
    assert_eq!(r.car("u0", &json!({"car": 2})), Some(2));
    assert_eq!(r.player("u0").unwrap().car, 7, "but not mid-stage");
    assert_eq!(r.cars["u0"], 2, "the choice is kept for the next stage");
    r.end();
    assert_eq!(r.phase, Phase::Idle);
    assert!(r.players.is_empty() && r.stage.is_some());
}

#[test]
fn an_eight_car_tick_is_cheap() {
    let (mut r, t) = started(8);
    let start = std::time::Instant::now();
    for f in 0..20 * 30 {
        let tt = t + 0.05 + f as f64 * 0.05;
        for i in 0..8 {
            r.pos(&format!("u{i}"), &frame(f as f64 * 1.5, i as f64, None), tt);
        }
        r.tick(tt, f % 4 != 3);
    }
    let per_second = start.elapsed().as_secs_f64() / 30.0;
    assert!(per_second < 0.05, "{per_second}");
}

// ------------------------------------------- through the room + lobby layer --
struct Env {
    hub: RallyHub,
    rooms: RoomManager,
    clock: Arc<Mutex<f64>>,
}

fn env(max_tickers: usize) -> Env {
    let rooms = RoomManager::new();
    let clock = Arc::new(Mutex::new(1000.0));
    let c = clock.clone();
    let hub = RallyHub::new(rooms.clone(), Registry::new(max_tickers), Arc::new(move || *c.lock().unwrap()),
                            crate::results::Recorder::default());
    Env { hub, rooms, clock }
}

impl Env {
    fn advance(&self, dt: f64) {
        *self.clock.lock().unwrap() += dt;
    }
    async fn connect(&self, room: &str, conn: u64, uid: &str) -> (Member, mpsc::Receiver<String>) {
        let m = Member { user_id: uid.into(), handle: uid.into(), display_name: format!("{uid} name"),
                         avatar_url: String::new(), cos: Value::Null };
        let (_rx, drx, _, _) = self.rooms.join_direct(room, conn, m.clone()).await.unwrap();
        (m, drx)
    }
    async fn send(&self, room: &str, conn: u64, m: &Member, op: &str, extra: Value) {
        let mut msg = json!({"type": "game", "g": "rally", "op": op});
        if let Value::Object(e) = extra {
            msg.as_object_mut().unwrap().extend(e);
        }
        self.hub.handle(room, conn, m, &msg).await;
    }
    async fn disconnect(&self, room: &str, conn: u64, m: &Member) {
        self.hub.on_disconnect(room, conn, m).await;
        self.rooms.leave(room, conn).await;
    }
    async fn wait_phase(&self, room: &str, ph: Phase) {
        for _ in 0..250 {
            if self.hub.with_room(room, |v| v.rally.phase) == Some(ph) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("never reached {ph:?}");
    }
    /// Drive up the road at 30 m/s from d0 to d1 through the hub.
    async fn drive(&self, room: &str, conn: u64, m: &Member, d0: f64, d1: f64) {
        let mut d = d0;
        loop {
            self.advance(0.05);
            self.send(room, conn, m, "pos", frame(d, 0.0, None)).await;
            if d >= d1 {
                return;
            }
            d = d1.min(d + 1.5);
        }
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
        if v["type"] == "game" && v["g"] == "rally" && v["ev"] == ev && pick(&v) {
            return v;
        }
    }
}

async fn until(rx: &mut mpsc::Receiver<String>, ev: &str) -> Value {
    until_where(rx, ev, |_| true).await
}

/// The event's payload without the envelope.
fn body(v: &Value) -> Vec<String> {
    keys(v).into_iter().filter(|k| !["type", "g", "ev", "pv"].contains(&k.as_str())).collect()
}

#[tokio::test]
async fn a_stage_through_the_room_and_lobby() {
    let e = env(MAX_TICKERS);
    let (a, mut wa) = e.connect("rr", 1, "ra").await;
    let (b, mut wb) = e.connect("rr", 2, "rb").await;
    e.send("rr", 1, &a, "join", json!({})).await;
    let lobby = until(&mut wa, "lobby").await;
    assert_eq!(body(&lobby), ["players", "host", "phase"]);
    assert_eq!(lobby["players"], json!([{"uid": "ra", "name": "ra name", "car": 0, "ready": true}]));
    assert_eq!((lobby["host"].as_str(), lobby["phase"].as_str()), (Some("ra"), Some("idle")));
    assert_eq!(lobby["pv"], 1);
    e.send("rr", 2, &b, "join", json!({})).await;
    let l2 = until_where(&mut wb, "lobby", |m| m["players"].as_array().unwrap().len() == 2).await;
    assert_eq!(l2["host"], "ra");
    // host-only
    e.send("rr", 2, &b, "start", json!({"stage": sdoc()})).await;
    let er = until(&mut wb, "error").await;
    assert_eq!((er["msg"].as_str(), er["error"].as_str()),
               (Some("only the host can do that"), Some("only the host can do that")));
    e.send("rr", 2, &b, "end", json!({})).await;
    assert_eq!(until(&mut wb, "error").await["msg"], "only the host can do that");
    e.send("rr", 1, &a, "car", json!({"car": 5})).await;
    assert_eq!(body(&until(&mut wb, "car").await), ["uid", "car"]);
    // a bad stage says why
    e.send("rr", 1, &a, "start", json!({"stage": with(sdoc(), |d| d["data"]["biome"] = json!("lava"))})).await;
    assert_eq!(until(&mut wa, "error").await["msg"], "bad stage: biome must be forest, hills or snow");
    assert!(e.hub.registry().get("rally:rr").is_none());
    // start: rally, go and the lobby to everyone
    e.send("rr", 1, &a, "start", json!({"stage": sdoc()})).await;
    let rally = until(&mut wb, "rally").await;
    assert_eq!(body(&rally), ["key", "name", "gates", "stage"]);
    assert!(is_ckey(rally["key"].as_str().unwrap()));
    assert_eq!((rally["name"].as_str(), rally["gates"].as_i64()), (Some("Daily Stage"), Some(6)));
    assert_eq!(rally["stage"], stage_from_doc(&sdoc()).unwrap().doc());
    let go = until(&mut wb, "go").await;
    assert_eq!(body(&go), ["at", "countdown"]);
    assert_eq!(go["at"], wire_ms(1000.0 + COUNTDOWN));
    assert_eq!(go["countdown"], 3);
    let lobby = until(&mut wb, "lobby").await;
    assert_eq!(lobby["phase"], "countdown");
    assert_eq!(lobby["stage"], json!({"key": rally["key"], "name": "Daily Stage", "gates": 6}));
    assert_eq!(lobby["players"][0]["car"], 5);
    assert!(e.hub.registry().get("rally:rr").is_some());
    e.advance(COUNTDOWN);
    e.wait_phase("rr", Phase::Racing).await;
    e.drive("rr", 1, &a, 0.0, 1000.0).await;
    let split = until(&mut wb, "split").await;
    assert_eq!(body(&split), ["uid", "gate", "ms"]);
    assert_eq!((split["uid"].as_str(), split["gate"].as_i64()), (Some("ra"), Some(0)));
    let fin = until(&mut wb, "finish").await;
    assert_eq!(body(&fin), ["uid", "ms"]);
    let snap = until(&mut wb, "snap").await;
    assert_eq!(body(&snap), ["t", "cars"]);
    assert_eq!(body(&snap["cars"][0]), ["uid", "x", "z", "y", "yaw", "v"]);
    // the other car leaves: a DNF, and the stage is done
    e.send("rr", 2, &b, "leave", json!({})).await;
    let dnf = until(&mut wa, "dnf").await;
    assert_eq!((body(&dnf), dnf["reason"].as_str()), (vec!["uid".to_string(), "reason".to_string()], Some("left")));
    let done = until(&mut wa, "done").await;
    assert_eq!(body(&done), ["results", "key", "name"]);
    assert_eq!(done["results"][0]["uid"], "ra");
    assert_eq!(done["results"][0]["car"], 5);
    assert_eq!(done["results"][0]["ms"], fin["ms"]);
    assert_eq!(done["results"][1]["ms"], Value::Null);
    until_where(&mut wa, "lobby", |m| m["phase"] == "done").await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(e.hub.registry().get("rally:rr").is_none(), "the loop stops with the stage");
    // a late view gets the lobby, the stage and the last results
    e.send("rr", 1, &a, "view", json!({})).await;
    until(&mut wa, "lobby").await;
    until(&mut wa, "rally").await;
    assert_eq!(until(&mut wa, "done").await["results"], done["results"]);
}

#[tokio::test]
async fn a_bad_move_through_the_lobby_gets_a_fix_and_end_stops_the_loop() {
    let e = env(MAX_TICKERS);
    let (a, mut wa) = e.connect("rfix", 1, "a").await;
    e.send("rfix", 1, &a, "join", json!({})).await;
    e.send("rfix", 1, &a, "start", json!({"stage": sdoc(), "event": {"index": 0, "count": 2}})).await;
    let rally = until(&mut wa, "rally").await;
    assert_eq!(rally["event"], json!({"index": 0, "count": 2}));
    e.advance(COUNTDOWN);
    e.wait_phase("rfix", Phase::Racing).await;
    e.advance(0.05);
    e.send("rfix", 1, &a, "pos", frame(0.0, 0.0, None)).await;
    e.advance(0.05);
    e.send("rfix", 1, &a, "pos", frame(300.0, 0.0, None)).await;
    let fix = until(&mut wa, "fix").await;
    assert_eq!(body(&fix), ["uid", "x", "z", "y", "yaw"]);
    assert_eq!((fix["uid"].as_str(), fix["z"].as_i64()), (Some("a"), Some(0)));
    e.send("rfix", 1, &a, "end", json!({})).await;
    assert_eq!(until_where(&mut wa, "lobby", |m| m["phase"] == "idle").await["host"], "a");
    assert!(e.hub.registry().get("rally:rfix").is_none());
}

#[tokio::test]
async fn lobby_rules_max_players_and_invites() {
    let e = env(MAX_TICKERS);
    let (a, mut wa) = e.connect("rlob", 1, "a").await;
    e.hub.handle("rlob", 1, &a, &json!({"type": "game", "g": "kart", "op": "join"})).await;
    let m: Value = serde_json::from_str(&wa.recv().await.unwrap()).unwrap();
    assert_eq!((m["g"].as_str(), m["msg"].as_str()), (Some("kart"), Some("unknown game")));
    // ops before joining
    e.send("rlob", 1, &a, "start", json!({"stage": sdoc()})).await;
    assert_eq!(until(&mut wa, "error").await["msg"], "join the lobby first");
    e.send("rlob", 1, &a, "invite", json!({"uid": "z"})).await;
    assert_eq!(until(&mut wa, "error").await["msg"], "join the lobby first");
    // the lobby holds 8
    let mut socks = Vec::new();
    for i in 0..MAX_LOBBY {
        let (m, rx) = e.connect("rlob", 10 + i as u64, &format!("p{i}")).await;
        e.send("rlob", 10 + i as u64, &m, "join", json!({})).await;
        socks.push((m, rx));
    }
    e.send("rlob", 1, &a, "join", json!({})).await;
    assert_eq!(until(&mut wa, "error").await["msg"], "this game's lobby is full");
    // the host leaving hands the lobby to the next in line
    let (p0, _) = &socks[0];
    e.send("rlob", 10, p0, "leave", json!({})).await;
    let lobby = until_where(&mut wa, "lobby", |m| m["players"].as_array().unwrap().len() == 7).await;
    assert_eq!(lobby["host"], "p1");
    e.send("rlob", 1, &a, "join", json!({})).await;
    until_where(&mut wa, "lobby", |m| m["players"].as_array().unwrap().len() == 8).await;
    // invites go to every socket the invitee has
    let (_z, mut wz) = e.connect("elsewhere", 99, "z").await;
    e.send("rlob", 1, &a, "invite", json!({"uid": "z"})).await;
    let inv = until(&mut wz, "invite").await;
    assert_eq!((inv["room"].as_str(), inv["from"]["userId"].as_str(), inv["name"].as_str()),
               (Some("rlob"), Some("a"), Some("Valley Rally")));
    assert_eq!(until(&mut wa, "invited").await["delivered"], 1);
    e.send("rlob", 1, &a, "invite", json!({"uid": "a"})).await;
    assert_eq!(until(&mut wa, "error").await["msg"], "invite someone else");
    e.send("rlob", 1, &a, "car", json!({"car": 8})).await;
    assert_eq!(until(&mut wa, "error").await["msg"], "pick a car");
    // the host starts: all 8 are on the grid and ready
    let (p1, _) = &socks[1];
    e.send("rlob", 11, p1, "start", json!({"stage": sdoc()})).await;
    let lobby = until_where(&mut wa, "lobby", |m| m["phase"] == "countdown").await;
    assert!(lobby["players"].as_array().unwrap().iter().all(|p| p["ready"] == true));
    assert_eq!(e.hub.with_room("rlob", |v| v.rally.players.len()), Some(MAX_PLAYERS));
    // a start while one runs
    e.send("rlob", 11, p1, "start", json!({"stage": sdoc()})).await;
    let (_, rx1) = &mut socks[1];
    assert!(until(rx1, "error").await["msg"].as_str().unwrap().contains("already running"));
}

#[tokio::test]
async fn a_dropped_socket_waits_for_a_rejoin_and_gets_its_car_back() {
    let e = env(MAX_TICKERS);
    let (a, mut wa) = e.connect("rblip", 1, "a").await;
    let (b, _wb) = e.connect("rblip", 2, "b").await;
    e.send("rblip", 1, &a, "join", json!({})).await;
    e.send("rblip", 2, &b, "join", json!({})).await;
    e.send("rblip", 1, &a, "start", json!({"stage": sdoc()})).await;
    e.advance(COUNTDOWN);
    e.wait_phase("rblip", Phase::Racing).await;
    e.drive("rblip", 2, &b, 0.0, 30.0).await;
    e.disconnect("rblip", 2, &b).await;
    assert!(e.hub.with_room("rblip", |v| v.rally.player("b").unwrap().away.is_some()).unwrap());
    // b reloads within the grace: back on the stage, with a fix to where the car was
    let (b2, mut wb2) = e.connect("rblip", 3, "b").await;
    e.send("rblip", 3, &b2, "join", json!({})).await;
    until(&mut wb2, "rally").await;
    until(&mut wb2, "go").await;
    let fix = until(&mut wb2, "fix").await;
    assert_eq!((fix["uid"].as_str(), fix["z"].as_i64()), (Some("b"), Some(-3000)));
    assert!(e.hub.with_room("rblip", |v| v.rally.player("b").unwrap().away.is_none()).unwrap());
    // a drop for longer than the grace is a DNF
    e.disconnect("rblip", 3, &b2).await;
    e.advance(GRACE + 0.1);
    let dnf = until(&mut wa, "dnf").await;
    assert_eq!((dnf["uid"].as_str(), dnf["reason"].as_str()), (Some("b"), Some("disconnect")));
    // a closing second socket of a is not a leave
    let (a2, _wa2) = e.connect("rblip", 4, "a").await;
    e.disconnect("rblip", 4, &a2).await;
    assert_eq!(e.hub.with_room("rblip", |v| v.lobby.has("a")), Some(true));
}

#[tokio::test]
async fn the_loop_stops_when_the_room_empties_and_a_full_arena_says_so() {
    let e = env(MAX_TICKERS);
    let (a, mut wa) = e.connect("rgone", 1, "a").await;
    e.send("rgone", 1, &a, "join", json!({})).await;
    e.send("rgone", 1, &a, "start", json!({"stage": sdoc()})).await;
    until(&mut wa, "go").await;
    assert_eq!(e.hub.registry().running(), 1);
    e.disconnect("rgone", 1, &a).await;
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(e.hub.registry().running(), 0);
    assert!(e.hub.with_room("rgone", |_| ()).is_none());

    let e = env(0);
    let (a, mut wa) = e.connect("rbusy", 1, "a").await;
    e.send("rbusy", 1, &a, "join", json!({})).await;
    e.send("rbusy", 1, &a, "start", json!({"stage": sdoc()})).await;
    assert_eq!(until(&mut wa, "error").await["msg"], "the Arena is busy right now: try again in a minute");
    assert_eq!(e.hub.with_room("rbusy", |v| v.rally.phase), Some(Phase::Idle));
}

#[tokio::test]
async fn snapshots_run_at_15_hz_and_count_against_the_rooms_bandwidth() {
    let e = env(MAX_TICKERS);
    let (a, mut wa) = e.connect("rbw", 1, "a").await;
    e.send("rbw", 1, &a, "join", json!({})).await;
    e.send("rbw", 1, &a, "start", json!({"stage": sdoc()})).await;
    e.advance(COUNTDOWN);
    e.wait_phase("rbw", Phase::Racing).await;
    e.drive("rbw", 1, &a, 0.0, 3.0).await;
    until(&mut wa, "snap").await;
    let tk = e.hub.registry().get("rally:rbw").unwrap();
    assert!(tk.bytes_1s() > 0 && tk.ticks() > 0);
    assert_eq!(e.hub.registry().stats()["rooms"]["rally:rbw"]["hz"], HZ);
}

// ------------------------------------------------------------------ gallery --
#[test]
fn the_gallery_takes_rally_stages_under_the_same_key() {
    let save = |name: &str, data: &Value| {
        crate::maps::parse_save(&serde_json::to_vec(
            &json!({"kind": "rally", "name": name, "data": data, "scope": "private"})).unwrap())
    };
    let b = save(&"n".repeat(40), &sdata()).expect("40-character rally names are fine");
    assert_eq!(b.data, validate_custom(&sdata()).unwrap());
    assert_eq!(crate::maps::canonical("rally", &sdata()).unwrap(), b.data);
    assert_eq!(crate::mapkey::content_key("rally", &b.data), stage_from_doc(&sdoc()).unwrap().key);
    assert!(save(&"n".repeat(41), &sdata()).is_err());
    assert!(save("Bad", &with(sdata(), |d| d["biome"] = json!("lava"))).is_err());
    // the other kinds keep their 32
    let k = crate::maps::parse_save(&serde_json::to_vec(
        &json!({"kind": "kart", "name": "n".repeat(33), "data": {}, "scope": "private"})).unwrap());
    assert!(k.is_err());
}
