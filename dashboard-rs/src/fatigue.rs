//! Creature fatigue (local, cosmetic, deterministic): load rises with busy time
//! and drains while resting; it may faint past 3 h of load. Recomputed from
//! the cached busy spans on every build; never stored.

use crate::py;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

pub const WINDOW_SECS: f64 = 86400.0;
pub const TAIL_SECS: f64 = 300.0;
pub const HOLD_SECS: f64 = 600.0;
pub const REST_RATE: f64 = 4.0;
pub const TIRED_SECS: f64 = 3600.0;
pub const FATIGUED_SECS: f64 = 7200.0;
pub const RISK_SECS: f64 = 10800.0;
pub const CERTAIN_SECS: f64 = 14400.0;
pub const CAP_SECS: f64 = 18000.0;
pub const TICK_SECS: f64 = 900.0;
pub const REVIVE_TO_SECS: f64 = 6300.0;
pub const MAX_PAIR_SECS: f64 = 3600.0;
pub const MAX_TURN_SECS: f64 = 21600.0;
pub const MAX_SPANS: usize = 96;
pub const SKIP_TOOLS: [&str; 2] = ["AskUserQuestion", "ExitPlanMode"];

/// kind -> (seconds of load removed, revives). Mirrors the Arena's pantry catalog.
pub const FOOD_EFFECTS: [(&str, (i64, bool)); 21] = [
    ("berry", (1200, false)),
    ("bread", (1200, false)),
    ("riceball", (2700, false)),
    ("coffee", (2700, false)),
    ("bento", (7200, false)),
    ("noodles", (7200, false)),
    ("hotpot", (10800, false)),
    ("tonic", (0, true)),
    ("elixir", (3600, true)),
    ("strawberry", (1500, false)),
    ("dango", (3300, false)),
    ("omelette", (8100, false)),
    ("watermelon", (1500, false)),
    ("shavedice", (3300, false)),
    ("curry", (8100, false)),
    ("apple", (1500, false)),
    ("sweetpotato", (3300, false)),
    ("pumpkinstew", (8100, false)),
    ("chestnuts", (1500, false)),
    ("cocoa", (3300, false)),
    ("oden", (8100, false)),
];

pub fn food_effect(kind: &str) -> Option<(i64, bool)> {
    FOOD_EFFECTS.iter().find(|(k, _)| *k == kind).map(|(_, e)| *e)
}

pub fn is_food(kind: &str) -> bool {
    food_effect(kind).is_some()
}

pub fn rested() -> Value {
    json!({"state": "rested", "energy": 1.0, "loadMins": 0, "mayFaint": false,
           "phase": "resting", "restInMins": 0, "restMins": 0, "streakMins": 0,
           "lastMeal": null})
}

/// Sort (start, end) pairs and merge those at most `gap` seconds apart.
pub fn merge_spans(intervals: &[(f64, f64)], gap: f64) -> Vec<[f64; 2]> {
    let mut v: Vec<(f64, f64)> = intervals.iter().filter(|(a, b)| b >= a).copied().collect();
    v.sort_by(|x, y| x.0.total_cmp(&y.0).then(x.1.total_cmp(&y.1)));
    let mut out: Vec<[f64; 2]> = Vec::new();
    for (s, e) in v {
        if let Some(last) = out.last_mut() {
            if s - last[1] <= gap {
                if e > last[1] {
                    last[1] = e;
                }
                continue;
            }
        }
        out.push([s, e]);
    }
    out
}

/// Deterministic [0, 1) roll for an absolute 15-minute tick.
pub fn faint_roll(sid: &str, k: i64) -> f64 {
    let h = hex::encode(Sha256::digest(format!("hq:faint:{}:{}", sid, k).as_bytes()));
    u32::from_str_radix(&h[..8], 16).unwrap_or(0) as f64 / 4294967296.0
}

/// The creature's fatigue at `now` from its busy spans and meals.
pub fn fatigue_for(sid: &str, spans: &[[f64; 2]], now: f64, open_since: Option<f64>, meals: &[(f64, String)]) -> Value {
    let lo = now - WINDOW_SECS;
    let clipped: Vec<(f64, f64)> =
        spans.iter().filter(|s| s[1] >= lo && s[0] <= now).map(|s| (s[0].max(lo), s[1].min(now))).collect();
    let mut sp = merge_spans(&clipped, TAIL_SECS);
    if let Some(os) = open_since {
        if let Some(last) = sp.last_mut() {
            if last[0] <= os && os <= now {
                let ext = now.min(os + MAX_PAIR_SECS);
                if ext > last[1] {
                    last[1] = ext;
                }
            }
        }
    }
    let mut ml: Vec<(f64, String)> =
        meals.iter().filter(|(t, k)| lo <= *t && *t <= now && is_food(k)).map(|(t, k)| (*t, k.clone())).collect();
    ml.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
    if ml.is_empty() && sp.last().map(|l| now - l[1] > HOLD_SECS + CAP_SECS / REST_RATE).unwrap_or(true) {
        return rested();
    }

    // Segments: (is_work, a, b, recovery_start)
    let mut segs: Vec<(bool, f64, f64, f64)> = Vec::new();
    for (i, s) in sp.iter().enumerate() {
        let nxt = if i + 1 < sp.len() { sp[i + 1][0] } else { now };
        let wend = (s[1] + TAIL_SECS).min(nxt);
        segs.push((true, s[0], wend, 0.0));
        if nxt > wend {
            segs.push((false, wend, nxt, s[1] + HOLD_SECS));
        }
    }

    let mut load = 0.0f64;
    let mut ko = false;
    let mut meal: Option<(f64, String)> = None;

    let work = |a: f64, b: f64, load: &mut f64, ko: &mut bool| {
        let mut x = a;
        while x < b {
            let bnd = ((x / TICK_SECS).floor() + 1.0) * TICK_SECS;
            let y = b.min(bnd);
            *load = CAP_SECS.min(*load + (y - x));
            if y == bnd && !*ko && *load >= RISK_SECS {
                let p = ((*load - RISK_SECS) / (CERTAIN_SECS - RISK_SECS)).min(1.0);
                if faint_roll(sid, (bnd / TICK_SECS).floor() as i64) < p {
                    *ko = true;
                }
            }
            x = y;
        }
    };
    let rest = |a: f64, b: f64, rec: f64, load: &mut f64, ko: &mut bool| {
        let d = b - a.max(rec);
        if d > 0.0 {
            *load = (*load - d * REST_RATE).max(0.0);
        }
        if *ko && *load < FATIGUED_SECS {
            *ko = false;
        }
    };
    let eat = |t: f64, k: &str, load: &mut f64, ko: &mut bool, meal: &mut Option<(f64, String)>| {
        let (credit, revives) = food_effect(k).unwrap_or((0, false));
        let credit = credit as f64;
        if revives {
            *ko = false;
            *load = (load.min(REVIVE_TO_SECS) - credit).max(0.0);
        } else {
            *load = (*load - credit).max(0.0);
            if *ko && *load < FATIGUED_SECS {
                *ko = false;
            }
        }
        *meal = Some((t, k.to_string()));
    };

    let mut mi = 0;
    for &(is_work, a, b, rec) in &segs {
        while mi < ml.len() && ml[mi].0 < a {
            eat(ml[mi].0, &ml[mi].1, &mut load, &mut ko, &mut meal);
            mi += 1;
        }
        let mut cur = a;
        while mi < ml.len() && ml[mi].0 < b {
            let t = ml[mi].0;
            if is_work {
                work(cur, t, &mut load, &mut ko);
            } else {
                rest(cur, t, rec, &mut load, &mut ko);
            }
            eat(t, &ml[mi].1, &mut load, &mut ko, &mut meal);
            mi += 1;
            cur = t;
        }
        if is_work {
            work(cur, b, &mut load, &mut ko);
        } else {
            rest(cur, b, rec, &mut load, &mut ko);
        }
    }
    while mi < ml.len() {
        eat(ml[mi].0, &ml[mi].1, &mut load, &mut ko, &mut meal);
        mi += 1;
    }

    let state = if ko {
        "unconscious"
    } else if load >= FATIGUED_SECS {
        "fatigued"
    } else if load >= TIRED_SECS {
        "tired"
    } else {
        "rested"
    };
    let last_end = sp.last().map(|l| l[1]);
    let p = match last_end {
        Some(le) => now - le,
        None => HOLD_SECS + 1.0,
    };
    let phase = if p <= TAIL_SECS {
        "active"
    } else if p <= HOLD_SECS {
        "pause"
    } else {
        "resting"
    };
    let mut rest_mins = 0i64;
    if state != "rested" {
        let target = if ko { FATIGUED_SECS } else { TIRED_SECS };
        let tail_left = (TAIL_SECS - p).max(0.0);
        let hold_left = (HOLD_SECS - p).max(0.0);
        let need = (load + tail_left - target + 1.0).max(0.0) / REST_RATE;
        rest_mins = ((hold_left + need) / 60.0).ceil() as i64;
    }
    let mut streak = 0i64;
    if let (Some(le), true) = (last_end, phase != "resting") {
        let mut run_start = sp[sp.len() - 1][0];
        for i in (0..sp.len().saturating_sub(1)).rev() {
            if sp[i + 1][0] - sp[i][1] <= HOLD_SECS {
                run_start = sp[i][0];
            } else {
                break;
            }
        }
        streak = ((now.min(le + TAIL_SECS) - run_start) / 60.0).floor() as i64;
    }
    let rest_in = if phase == "resting" { 0 } else { ((HOLD_SECS - p) / 60.0).ceil() as i64 };
    json!({
        "state": state,
        "energy": py::round((1.0 - load / CERTAIN_SECS).clamp(0.0, 1.0), 3),
        "loadMins": (load / 60.0).floor() as i64,
        "mayFaint": !ko && load >= RISK_SECS,
        "phase": phase,
        "restInMins": rest_in,
        "restMins": rest_mins,
        "streakMins": streak,
        "lastMeal": meal.map(|(t, k)| json!({"kind": k, "at": py::iso_from_ts(t)})),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const H: f64 = 3600.0;
    const M: f64 = 60.0;
    const BASE: f64 = 1789999200.0;
    const SID: &str = "00000000-0000-4000-8000-000000000000";

    fn fz(spans: &[(f64, f64)], now: f64, meals: &[(f64, &str)], open_since: Option<f64>, sid: &str) -> Value {
        let sp: Vec<[f64; 2]> = spans.iter().map(|(a, b)| [BASE + a, BASE + b]).collect();
        let ml: Vec<(f64, String)> = meals.iter().map(|(t, k)| (BASE + t, k.to_string())).collect();
        fatigue_for(sid, &sp, BASE + now, open_since.map(|o| BASE + o), &ml)
    }

    #[allow(clippy::type_complexity)]
    fn vectors() -> Vec<(&'static str, Vec<(f64, f64)>, f64, Vec<(f64, &'static str)>, Option<f64>, (&'static str, f64, i64, bool, &'static str, i64, i64, i64), Option<&'static str>)> {
        vec![
            ("V1", vec![(0.0, 2.0 * H)], 2.0 * H, vec![], None, ("fatigued", 0.5, 120, false, "active", 10, 27, 120), None),
            ("V2", vec![(0.0, 2.0 * H)], 2.0 * H + 8.0 * M, vec![], None, ("fatigued", 0.479, 125, false, "pause", 2, 19, 125), None),
            ("V3", vec![(0.0, 2.0 * H)], 2.0 * H + 30.0 * M, vec![], None, ("rested", 0.812, 45, false, "resting", 0, 0, 0), None),
            ("V4", vec![(0.0, 115.0 * M), (127.0 * M, 137.0 * M)], 137.0 * M, vec![], None, ("fatigued", 0.492, 122, false, "active", 10, 27, 10), None),
            ("V5", vec![(0.0, 130.0 * M)], 130.0 * M, vec![(130.0 * M, "riceball")], None, ("tired", 0.646, 85, false, "active", 10, 18, 130), Some("riceball")),
            ("V5b", vec![(0.0, 150.0 * M)], 150.0 * M, vec![(130.0 * M, "riceball")], None, ("tired", 0.562, 105, false, "active", 10, 23, 150), Some("riceball")),
            ("V6", vec![(0.0, 4.0 * H)], 4.0 * H, vec![], None, ("unconscious", 0.0, 240, false, "active", 10, 42, 240), None),
            ("V7", vec![(0.0, 4.0 * H)], 4.0 * H + 30.0 * M, vec![], None, ("unconscious", 0.312, 165, false, "resting", 0, 12, 0), None),
            ("V7b", vec![(0.0, 4.0 * H)], 4.0 * H + 45.0 * M, vec![], None, ("tired", 0.562, 105, false, "resting", 0, 12, 0), None),
            ("V8", vec![(0.0, 4.0 * H)], 4.0 * H, vec![(4.0 * H, "tonic")], None, ("tired", 0.562, 105, false, "active", 10, 23, 240), Some("tonic")),
            ("V8b", vec![(0.0, 6.0 * H)], 6.0 * H, vec![], None, ("unconscious", 0.0, 300, false, "active", 10, 57, 360), None),
            ("V8c", vec![(0.0, 6.0 * H)], 6.0 * H, vec![(6.0 * H, "tonic")], None, ("tired", 0.562, 105, false, "active", 10, 23, 360), Some("tonic")),
            ("V8d", vec![(0.0, 6.0 * H)], 6.0 * H + 55.0 * M, vec![], None, ("unconscious", 0.5, 120, false, "resting", 0, 1, 0), None),
            ("V8e", vec![(0.0, 4.0 * H)], 4.0 * H, vec![(4.0 * H, "elixir")], None, ("rested", 0.812, 45, false, "active", 10, 0, 240), Some("elixir")),
            ("V8f", vec![(0.0, 6.0 * H)], 6.0 * H, vec![(6.0 * H, "elixir")], None, ("rested", 0.812, 45, false, "active", 10, 0, 360), Some("elixir")),
            ("V9", vec![(0.0, 2.0 * H)], 2.0 * H, vec![(2.0 * H, "bento")], None, ("rested", 1.0, 0, false, "active", 10, 0, 120), Some("bento")),
            ("V10", vec![(0.0, 50.0 * M), (70.0 * M, 120.0 * M), (140.0 * M, 190.0 * M), (210.0 * M, 260.0 * M)], 260.0 * M, vec![], None, ("tired", 0.604, 95, false, "active", 10, 21, 50), None),
            ("V12", vec![(0.0, 3.0 * H)], 3.0 * H, vec![], None, ("fatigued", 0.25, 180, true, "active", 10, 42, 180), None),
            ("V12a", vec![(0.0, 3.0 * H + 15.0 * M)], 3.0 * H + 15.0 * M, vec![], None, ("unconscious", 0.188, 195, false, "active", 10, 31, 195), None),
            ("V13", vec![(0.0, 30.0 * M)], 70.0 * M, vec![], Some(30.0 * M), ("tired", 0.708, 70, false, "active", 10, 14, 70), None),
            ("V13b", vec![(10.0 * M, 30.0 * M)], 70.0 * M, vec![], Some(5.0 * M), ("rested", 1.0, 0, false, "resting", 0, 0, 0), None),
            ("V13c", vec![(0.0, 30.0 * M)], 120.0 * M, vec![], Some(30.0 * M), ("rested", 0.938, 15, false, "resting", 0, 0, 0), None),
            ("V14", vec![(0.0, 30.0 * M)], 30.0 * M, vec![(30.0 * M, "berry")], None, ("rested", 0.958, 10, false, "active", 10, 0, 30), Some("berry")),
            ("V16", vec![(0.0, 110.0 * M), (124.0 * M, 125.0 * M)], 125.0 * M, vec![], None, ("tired", 0.583, 100, false, "active", 10, 22, 1), None),
        ]
    }

    #[test]
    fn contract_vectors() {
        for (name, spans, now, meals, osn, want, meal_kind) in vectors() {
            let r = fz(&spans, now, &meals, osn, SID);
            let got = (
                r["state"].as_str().unwrap(),
                r["energy"].as_f64().unwrap(),
                r["loadMins"].as_i64().unwrap(),
                r["mayFaint"].as_bool().unwrap(),
                r["phase"].as_str().unwrap(),
                r["restInMins"].as_i64().unwrap(),
                r["restMins"].as_i64().unwrap(),
                r["streakMins"].as_i64().unwrap(),
            );
            assert_eq!(got, want, "{}", name);
            match meal_kind {
                None => assert!(r["lastMeal"].is_null(), "{}", name),
                Some(k) => assert_eq!(r["lastMeal"]["kind"], k, "{}", name),
            }
            assert_eq!(r.as_object().unwrap().len(), 9);
        }
    }

    #[test]
    fn last_meal_iso_and_idle_rested() {
        let r = fz(&[(0.0, 30.0 * M)], 30.0 * M, &[(30.0 * M, "berry")], None, SID);
        assert_eq!(r["lastMeal"], json!({"kind": "berry", "at": "2026-09-21T14:30:00+00:00"}));
        assert_eq!(fz(&[(0.0, 2.0 * H)], 27.0 * H, &[], None, SID), rested());
        assert_eq!(fz(&[], 0.0, &[], None, SID), rested());
        assert_eq!(fz(&[(0.0, 2.0 * H)], 2.0 * H, &[(-25.0 * H, "bento")], None, SID), fz(&[(0.0, 2.0 * H)], 2.0 * H, &[], None, SID));
        assert_eq!(fz(&[(0.0, 2.0 * H)], 2.0 * H, &[(2.0 * H, "pizza")], None, SID), fz(&[(0.0, 2.0 * H)], 2.0 * H, &[], None, SID));
    }

    #[test]
    fn revive_items() {
        for (spans, now) in [(vec![(0.0, 4.0 * H)], 4.0 * H), (vec![(0.0, 6.0 * H)], 6.0 * H), (vec![(0.0, 6.0 * H)], 6.0 * H + 55.0 * M)] {
            let tonic = fz(&spans, now, &[(now, "tonic")], None, SID);
            let elixir = fz(&spans, now, &[(now, "elixir")], None, SID);
            assert_eq!(tonic["loadMins"], 105);
            assert_eq!(elixir["loadMins"], 45);
            assert_ne!(elixir["state"], "unconscious");
        }
        let r = fz(&[(0.0, 30.0 * M)], 30.0 * M, &[(30.0 * M, "elixir")], None, SID);
        assert_eq!((r["state"].as_str().unwrap(), r["loadMins"].as_i64().unwrap()), ("rested", 0));
    }

    #[test]
    fn continuity_and_no_retroactive_jump() {
        let mut prev: Option<i64> = None;
        let mut sec = 2.0 * H - 60.0;
        while sec < 2.0 * H + 20.0 * M {
            let load = fz(&[(0.0, 2.0 * H)], sec, &[], None, SID)["loadMins"].as_i64().unwrap();
            if let Some(p) = prev {
                assert!((load - p).abs() <= 1, "jump at {}", sec);
            }
            prev = Some(load);
            sec += 1.0;
        }
        for (g, (bw, aw)) in [(4.0, (113, 114)), (8.0, (115, 115)), (12.0, (107, 107)), (14.0, (99, 99)), (16.0, (91, 91)), (30.0, (35, 35))] {
            let resume = 110.0 * M + g * M;
            let before = fz(&[(0.0, 110.0 * M)], resume - 1.0, &[], None, SID)["loadMins"].as_i64().unwrap();
            let after = fz(&[(0.0, 110.0 * M), (resume, resume + 1.0)], resume + 1.0, &[], None, SID)["loadMins"].as_i64().unwrap();
            assert_eq!((before, after), (bw, aw));
        }
    }

    #[test]
    fn faints_between_3h_and_4h_and_is_deterministic() {
        for i in 0..200u128 {
            let n = i * 7919 + 12345;
            let h = format!("{:032x}", n);
            let sid = format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32]);
            assert_ne!(fz(&[(0.0, 3.0 * H - 1.0)], 3.0 * H - 1.0, &[], None, &sid)["state"], "unconscious");
            assert_eq!(fz(&[(0.0, 4.0 * H)], 4.0 * H, &[], None, &sid)["state"], "unconscious");
        }
        let spans = [(0.0, 50.0 * M), (70.0 * M, 120.0 * M), (140.0 * M, 190.0 * M), (210.0 * M, 260.0 * M)];
        let meals = [(100.0 * M, "berry"), (200.0 * M, "riceball")];
        let a = fz(&spans, 260.0 * M, &meals, None, SID);
        let shuffled = [spans[2], spans[0], spans[3], spans[1]];
        let rmeals = [meals[1], meals[0]];
        assert_eq!(a, fz(&shuffled, 260.0 * M, &rmeals, None, SID));
        assert_eq!(faint_roll(SID, 5), faint_roll(SID, 5));
        assert_ne!(faint_roll(SID, 5), faint_roll(SID, 6));
        let r = faint_roll(SID, 7);
        assert!((0.0..1.0).contains(&r));
    }

    #[test]
    fn merge() {
        assert_eq!(merge_spans(&[(500.0, 600.0), (0.0, 100.0)], 300.0), vec![[0.0, 100.0], [500.0, 600.0]]);
        assert_eq!(merge_spans(&[(0.0, 100.0), (400.0, 500.0)], 300.0), vec![[0.0, 500.0]]);
        assert_eq!(merge_spans(&[(0.0, 100.0), (401.0, 500.0)], 300.0), vec![[0.0, 100.0], [401.0, 500.0]]);
        assert_eq!(merge_spans(&[(10.0, 5.0), (0.0, 100.0), (20.0, 30.0)], 300.0), vec![[0.0, 100.0]]);
    }
}
