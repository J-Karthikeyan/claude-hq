//! The Strikezone map, the team split, the bots, the shot rules and the round
//! end -- a focused mirror of the fps tests, plus what is new here (teams and
//! server-driven bots).

use super::*;
use std::collections::VecDeque;

fn pubv(uid: &str) -> Value {
    json!({"userId": uid, "handle": uid, "displayName": uid, "avatarUrl": ""})
}

fn members(n: usize, prefix: &str) -> Vec<(String, Value)> {
    (0..n).map(|i| (format!("{prefix}{i}"), pubv(&format!("{prefix}{i}")))).collect()
}

pub(crate) fn started(n: usize, tsize: i64, t0: f64) -> (Strike, f64) {
    let mut s = Strike::new();
    assert_eq!(s.start(&members(n, "u"), &json!(5), &json!(30), &json!(tsize), &json!("normal"), t0), None);
    s.tick(t0 + COUNTDOWN, true);
    assert_eq!(s.phase, Phase::Round);
    (s, t0 + COUNTDOWN)
}

pub(crate) fn put(s: &mut Strike, uid: &str, x: f64, y: f64, z: f64, t: f64, q: Option<i64>) {
    let p = s.player_mut(uid).unwrap();
    p.x = x;
    p.y = y;
    p.z = z;
    p.hist = VecDeque::from([(t, x, y, z)]);
    p.protect = 0.0;
    p.spawn_t = t - 5.0;
    if let Some(q) = q {
        p.q = Some(q);
        p.q0 = q;
        p.t0 = t;
        p.qmax = Some(q);
        p.off = Some(t - q as f64 / 100.0);
    }
}

pub(crate) fn eye(s: &Strike, uid: &str) -> [f64; 3] {
    let p = s.player(uid).unwrap();
    [p.x, p.y + EYE, p.z]
}

fn chest(s: &Strike, uid: &str) -> [f64; 3] {
    let p = s.player(uid).unwrap();
    [p.x, p.y + 1.0, p.z]
}

fn slot(s: &Strike, uid: &str) -> i64 {
    s.player(uid).unwrap().slot
}

// --------------------------------------------------------------------- map --

#[test]
fn map_is_sane() {
    let m = &*MAP;
    assert!(m.spawns.len() >= 8 && m.boxes.len() >= 20);
    for s in &m.spawns {
        assert!(overlaps(m, s[0], s[1], s[2], 0.0).is_none(), "spawn inside a wall {s:?}");
        assert!(overlaps(m, s[0], s[1] - 0.05, s[2], 0.0).is_some(), "spawn not on the ground {s:?}");
        assert_eq!(top_under(m, s[0], s[1] - 0.05, s[2]), Some(s[1]));
    }
    for p in &m.pickups {
        assert!(overlaps(m, p.at[0], p.at[1], p.at[2], 0.0).is_none());
    }
    // the two sides' spawns are on opposite halves of the map
    let half = m.spawns.len() / 2;
    assert!(m.spawns[..half].iter().all(|s| s[2] < 0.0), "amber spawns must be north");
    assert!(m.spawns[half..].iter().all(|s| s[2] > 0.0), "cyan spawns must be south");
    assert_eq!(m.pickups.len(), 7);
    assert!(compile_map(&json!({"boxes": [[0, 0, 0, 0, 1, 1, "x"]]})).is_err());
}

// ------------------------------------------------------------------- teams --
#[test]
fn the_host_sizes_the_sides_and_bots_fill_them() {
    let mut s = Strike::new();
    assert_eq!(s.start(&members(3, "u"), &json!(5), &json!(30), &json!(3), &json!("normal"), 0.0), None);
    for tm in 0..2u8 {
        let side = s.players.iter().filter(|(_, p)| p.team == tm && !p.gone).count();
        assert_eq!(side, 3, "side {tm} must have 3 players");
    }
    let bots = s.players.iter().filter(|(_, p)| p.is_bot()).count();
    assert_eq!(bots, 3, "3 humans + 3 bots = 6 players");
    // a bot leaves when a human arrives
    s.enter("newbie", pubv("newbie"), 1.0);
    let bots_after = s.players.iter().filter(|(_, p)| p.is_bot() && !p.gone).count();
    assert!(bots_after < bots, "a human arriving retires a bot");
}

#[test]
fn a_five_by_five_fills_both_sides_and_a_duel_shrinks_them() {
    // 1..=5 are all legal team sizes now: the host's X tops each side up with
    // bots, and MAX_PLAYERS holds all ten seats at once.
    let mut s = Strike::new();
    assert_eq!(s.start(&members(4, "u"), &json!(5), &json!(30), &json!(5), &json!("normal"), 0.0), None);
    for tm in 0..2u8 {
        let side = s.players.iter().filter(|(_, p)| p.team == tm && !p.gone).count();
        assert_eq!(side, 5, "side {tm} must have 5 players");
    }
    let bots = s.players.iter().filter(|(_, p)| p.is_bot()).count();
    assert_eq!(bots, 6, "4 humans + 6 bots = 10 players");
    let alive = s.players.iter().filter(|(_, p)| !p.gone).count();
    assert_eq!(alive, s.players.len(), "nobody benches to make 5v5 fit");

    let mut d = Strike::new();
    assert_eq!(d.start(&members(2, "u"), &json!(3), &json!(10), &json!(1), &json!("normal"), 0.0), None);
    for tm in 0..2u8 {
        let side = d.players.iter().filter(|(_, p)| p.team == tm && !p.gone).count();
        assert_eq!(side, 1, "side {tm} must have exactly 1 player in a 1v1");
    }
    assert_eq!(d.players.iter().filter(|(_, p)| p.is_bot()).count(), 0, "a 1v1 of two humans adds no bots");
}

#[test]
fn the_host_sets_their_own_scoreline() {
    // minutes and kills are the host's own numbers now (the preset lists are just
    // quick picks): anything inside 1..=60 minutes and 1..=500 kills is honoured,
    // and out-of-range values clamp instead of snapping back to a default.
    let mut s = Strike::new();
    assert_eq!(s.start(&members(2, "u"), &json!(7), &json!(25), &json!(2), &json!("normal"), 0.0), None);
    assert_eq!(s.minutes, 7, "an off-list length is the host's to pick");
    assert_eq!(s.limit, 25, "an off-list target is the host's to pick");
    let mut s = Strike::new();   // a fresh match: start() refuses while one is running
    assert_eq!(s.start(&members(2, "u"), &json!(600), &json!(0), &json!(2), &json!("normal"), 0.0), None);
    assert_eq!(s.minutes, 60, "600 minutes clamps to the 60-minute cap");
    assert_eq!(s.limit, 1, "0 kills clamps up to the 1-kill floor");
    // every preset stays legal, so the menu's quick picks can't fall back either
    for m in MINUTES {
        for k in KILLS {
            let mut p = Strike::new();
            assert_eq!(p.start(&members(2, "u"), &json!(m), &json!(k), &json!(2), &json!("normal"), 0.0), None);
            assert_eq!((p.minutes, p.limit), (m, k), "preset {m} min / {k} kills must survive start");
        }
    }
}

#[test]
fn teams_never_hit_their_own() {
    let (mut s, t) = started(2, 2, 0.0);
    // u0 and u1 are on opposite teams. Find a bot on u0's team and put it
    // directly between u0 and the enemy.
    let mate = s.players.iter().find(|(_, p)| p.is_bot()
        && p.team == s.player("u0").unwrap().team).map(|(u, _)| u.clone()).unwrap();
    put(&mut s, "u0", -6.0, 0.0, -9.0, t, Some(0));
    put(&mut s, "u1", -6.0, 0.0, -12.0, t, Some(0));
    put(&mut s, &mate, -6.0, 0.0, -10.5, t, None);
    let o = eye(&s, "u0");
    let tgt = chest(&s, "u1");
    let yaw = (tgt[0] - o[0]).atan2(-(tgt[2] - o[2])).to_degrees();
    let pitch = (tgt[1] - o[1]).atan2((tgt[0] - o[0]).hypot(tgt[2] - o[2])).to_degrees();
    let (dy, dp) = spread_of(1, 0);
    let msg = json!({"w": 0, "ox": cm(o[0]), "oy": cm(o[1]), "oz": cm(o[2]),
                     "r": ((yaw - dy) * 100.0) as i64, "p": ((pitch - dp) * 100.0) as i64, "q": 10, "ip": 0});
    let (shot, _) = s.fire("u0", &msg, t + 0.1);
    let shot = shot.unwrap();
    // the shot must pass the team-mate and land on the enemy
    assert_eq!(shot[5], json!(slot(&s, "u1")), "the shot hits the enemy, not the mate");
}

// -------------------------------------------------------------------- bots --
#[test]
fn bots_move_and_fight() {
    let (mut s, t) = started(2, 2, 0.0);
    let start: Vec<(String, [f64; 3])> = s.players.iter()
        .filter(|(_, p)| p.is_bot())
        .map(|(u, p)| (u.clone(), [p.x, p.y, p.z])).collect();
    // run 10 s of ticks: the bots must move and someone must have fired
    let mut t = t;
    let mut any_shot = false;
    for _ in 0..200 {
        t += 1.0 / HZ;
        let evs = s.tick(t, true);
        for (ev, data) in evs {
            if ev == "snap" && data["s"].as_array().map(|a| !a.is_empty()).unwrap_or(false) {
                any_shot = true;
            }
        }
    }
    let moved = start.iter().any(|(u, pos0)| {
        s.player(u).map(|p| (p.x - pos0[0]).hypot(p.z - pos0[2]) > 1.0).unwrap_or(false)
    });
    assert!(moved, "bots must move from their spawns");
    assert!(any_shot, "bots must shoot at some point in 10 s");
    assert_eq!(s.phase, Phase::Round, "a 5-minute match is still going at 10 s");
}

#[test]
fn a_blip_past_grace_is_refilled_like_a_drop() {
    // drop() refills the side at once; a blip that never returns must do the
    // same when tick() benches the human, or their side plays one short.
    let (mut s, t) = started(2, 1, 0.0);
    assert!(s.drop("u0", t, true), "a blip keeps the place");
    let tm = s.player("u0").unwrap().team;
    let before = s.players.iter().filter(|(_, p)| p.team == tm && !p.gone).count();
    let evs = s.tick(t + GRACE + 0.05, true);
    assert!(evs.iter().any(|(ev, d)| ev == &"gone" && d["user"] == "u0"));
    assert!(s.player("u0").unwrap().gone);
    let after = s.players.iter().filter(|(_, p)| p.team == tm && !p.gone).count();
    assert_eq!(after, before, "the benched human's slot is refilled by a bot");
    assert!(s.players.iter().any(|(u, p)| u != "u0" && p.team == tm && p.is_bot() && !p.gone));
}

#[test]
fn a_team_score_ends_the_round() {
    let (mut s, t) = started(2, 2, 0.0);
    s.scores[0] = s.limit - 1;
    let killer = "u0";
    let ki = s.players.iter().position(|(u, _)| u == killer).unwrap();
    let vi = s.players.iter().position(|(_, p)| p.team != s.player(killer).unwrap().team).unwrap();
    s.kill(ki, vi, 0, false, t);
    let evs = s.tick(t + 0.05, true);
    assert_eq!(s.phase, Phase::Done);
    assert!(evs.iter().any(|(ev, _)| ev == &"done"), "the done event goes out");
    let res = s.results.clone().unwrap();
    assert_eq!(res.as_array().unwrap().last().unwrap()["teams"], json!(s.scores));
}

// ----------------------------------------------------------------- totems --
#[test]
fn totems_drop_on_the_round_clock_and_the_mole_needs_a_bot() {
    let (mut s, mut t) = started(2, 2, 0.0);
    // both sides field bots, so any kind may drop; run past the first drop
    let mut totems = 0;
    for _ in 0..(TOTEM_FIRST as usize * HZ as usize + 5) {
        t += 1.0 / HZ;
        let evs = s.tick(t, true);
        if evs.iter().any(|(ev, _)| ev == &"totem") {
            totems += 1;
        }
    }
    assert_eq!(totems, 1, "exactly one totem lands TOTEM_FIRST into the round");
    assert_eq!(s.items.len(), MAP.pickups.len() + DROP_SLOTS + PACK_SLOTS,
               "the drop rings exist past the map's packs, all match long");
    assert!(TOTEM_KINDS.contains(&s.kinds[MAP.pickups.len()]), "the kind is one of the five");
    assert!(s.items[MAP.pickups.len()].is_none(), "the fresh totem slot starts live");
    // the next one lands TOTEM_GAP later
    for _ in 0..(TOTEM_GAP as usize * HZ as usize) {
        t += 1.0 / HZ;
        s.tick(t, true);
    }
    assert_eq!(s.items.len(), MAP.pickups.len() + DROP_SLOTS + PACK_SLOTS,
               "the slot list never grows past the rings");
}

#[test]
fn drops_repeat_from_recycled_slots() {
    // totems and loose packs are two independent clocks; each drop replaces the OLDEST
    // slot of its ring (taken or not), so drops keep coming and the map never fills up
    let (mut s, mut t) = started(2, 2, 0.0);
    let n_map = MAP.pickups.len();
    let mut totem_slots = std::collections::HashSet::new();
    let mut pack_slots = std::collections::HashSet::new();
    // run 4 totem gaps + 5 pack gaps, ticking at HZ like the real loop
    let end = t + TOTEM_FIRST + TOTEM_GAP * 4.0 + PACK_GAP * 5.0 + 0.5;
    while t < end {
        t += 1.0 / HZ;
        for (ev, d) in s.tick(t, true) {
            if ev == "totem" {
                totem_slots.insert(d["i"].as_i64().unwrap_or(-1));
            } else if ev == "pack" {
                pack_slots.insert(d["i"].as_i64().unwrap_or(-1));
            }
        }
    }
    assert_eq!(s.items.len(), n_map + DROP_SLOTS + PACK_SLOTS,
               "the slot list never grows past the rings");
    assert!(totem_slots.iter().all(|i| (*i as usize) >= n_map && (*i as usize) < n_map + DROP_SLOTS),
            "totems land only in the totem ring");
    assert!(pack_slots.iter().all(|i| (*i as usize) >= n_map + DROP_SLOTS
                                   && (*i as usize) < n_map + DROP_SLOTS + PACK_SLOTS),
            "loose packs land only in the pack ring");
    assert_eq!(totem_slots.len() + pack_slots.len(), DROP_SLOTS + PACK_SLOTS,
               "every ring slot is used");
}

#[test]
fn a_loose_pack_refills_the_taker() {
    // a loose health/ammo drop behaves like the map's own packs when walked over
    let (mut s, mut t) = started(2, 2, 0.0);
    // the enemy bot would happily shoot u0 while it waits at spawn — borrow the shield
    // totem's bubble for the whole test (it blocks damage, not pickups)
    s.player_mut("u0").unwrap().shield_totem_until = t + 60.0;
    let mut pev = None;
    while t < COUNTDOWN + PACK_FIRST + 0.5 {
        t += 1.0 / HZ;
        for (ev, d) in s.tick(t, true) {
            if ev == "pack" && pev.is_none() {
                pev = Some(d.clone());
            }
        }
    }
    let pev = pev.expect("the first loose pack lands PACK_FIRST into the round");
    let n_map = MAP.pickups.len();
    let kind = pev["kind"].as_str().unwrap();
    assert!(kind == "health" || kind == "ammo");
    let i = pev["i"].as_i64().unwrap() as usize;
    assert!(n_map + DROP_SLOTS <= i && i < n_map + DROP_SLOTS + PACK_SLOTS);
    // stand on it (hurt first so a health pack has room to heal), driving pos() like a client
    let (px, pz) = (pev["x"].as_i64().unwrap() as f64 / 100.0, pev["z"].as_i64().unwrap() as f64 / 100.0);
    s.player_mut("u0").unwrap().hp = 40;
    put(&mut s, "u0", px, 0.0, pz, t, Some(0));
    let _ = s.pos("u0", &json!({"x": cm(px), "y": cm(0.0), "z": cm(pz), "r": 0, "p": 0, "q": 1}), t + 0.1);
    let evs = s.tick(t + 0.2, true);
    let pick = evs.iter().find(|(ev, d)| ev == &"pick" && d["i"].as_i64() == Some(i as i64));
    let (_, d) = pick.expect("walking over the loose pack picks it up");
    if kind == "health" {
        assert_eq!(d["hp"].as_i64(), Some(90), "the loose health pack healed its taker");
    }
    assert!(s.items[i].is_some(), "the taken slot waits out PICKUP_BACK before returning");
}

#[test]
fn a_charmed_kills_points_land_on_the_charmer_side() {
    let (mut s, t) = started(2, 2, 0.0);
    let ki = s.players.iter().position(|(u, _)| u == "u0").unwrap();
    let vi = s.players.iter()
        .position(|(_, p)| p.team != s.player("u0").unwrap().team).unwrap();
    // u0 is amber (team 0), the victim cyan (team 1). u0 charmed by a cyan mole: the point
    // leaves amber and pays cyan — the charmer's side, not the shooter's
    let before = s.scores;
    s.players[ki].1.charm_until = t + CHARM;
    s.kill(ki, vi, 0, false, t);
    assert_eq!(s.scores[1], before[1] + 1, "the mole's kill pays the charmer's (cyan) team");
    assert_eq!(s.scores[0], before[0], "amber does not keep the point its charmed shooter scored");
}

#[test]
fn a_frozen_bot_holds_still() {
    let (mut s, mut t) = started(2, 2, 0.0);
    let bot = s.players.iter().find(|(_, p)| p.is_bot()).map(|(u, _)| u.clone()).unwrap();
    let (x, z) = (s.player(&bot).unwrap().x, s.player(&bot).unwrap().z);
    s.player_mut(&bot).unwrap().freeze_until = t + FREEZE;
    for _ in 0..50 {
        let mut t2 = t;
        t2 += 1.0 / HZ;
        s.tick(t2, true);
    }
    let p = s.player(&bot).unwrap();
    assert_eq!((p.x, p.z), (x, z), "the paralysed bot does not move while frozen");
    // and it thaws: the freeze window lapses and it moves again
    for _ in 0..(FREEZE as usize * HZ as usize + 20) {
        t += 1.0 / HZ;
        s.tick(t, true);
    }
    let p = s.player(&bot).unwrap();
    assert!((p.x - x).hypot(p.z - z) > 0.0 || p.freeze_until <= t, "the freeze ends with the window");
}

#[test]
fn a_cloaked_player_is_invisible_to_bots() {
    let (mut s, t) = started(2, 2, 0.0);
    // u0 and u1 stand eye-to-eye; cloak u0 and no bot may pick them as a target
    let me = s.player("u0").unwrap().team;
    let bot = s.players.iter()
        .find(|(_, p)| p.is_bot() && p.team != me).map(|(u, _)| u.clone()).unwrap();
    put(&mut s, "u0", 0.0, 0.0, 0.0, t, Some(0));
    put(&mut s, &bot, 0.0, 0.0, -6.0, t, None);
    s.player_mut("u0").unwrap().cloak_until = t + CLOAK;
    let mut t2 = t;
    for _ in 0..(BOT_THINK as usize * HZ as usize * 4) {
        t2 += 1.0 / HZ;
        s.tick(t2, true);
    }
    let b = s.player(&bot).unwrap().bot.as_ref().unwrap();
    assert_ne!(b.target.as_deref(), Some("u0"), "a cloaked player is never a bot's target");
}

#[test]
fn a_shield_totem_blocks_damage_but_not_their_own_shots() {
    let (mut s, t) = started(2, 2, 0.0);
    // u1 (human, cyan) stands 6 m downrange of u0 and takes the shield totem
    put(&mut s, "u0", -6.0, 0.0, -9.0, t, Some(0));
    put(&mut s, "u1", -6.0, 0.0, -3.0, t, Some(0));
    s.player_mut("u1").unwrap().shield_totem_until = t + SHIELD_TOTEM;
    // u0 fires straight down the lane at u1: the bubble eats every point of it
    let o = eye(&s, "u0");
    let msg = json!({"w": 0, "ox": cm(o[0]), "oy": cm(o[1]), "oz": cm(o[2]), "r": 18000, "p": 0, "q": 10, "ip": 0});
    let hp0 = s.player("u1").unwrap().hp;
    let _ = s.fire("u0", &msg, t + 0.1);
    assert_eq!(s.player("u1").unwrap().hp, hp0, "nothing hurts the shield totem's taker");
    // but the shielded player shoots back at full damage — u0 has no shield
    let o2 = eye(&s, "u1");
    let hp0u0 = s.player("u0").unwrap().hp;
    let msg2 = json!({"w": 0, "ox": cm(o2[0]), "oy": cm(o2[1]), "oz": cm(o2[2]), "r": 0, "p": 0, "q": 20, "ip": 0});
    let _ = s.fire("u1", &msg2, t + 0.2);
    assert!(s.player("u0").unwrap().hp < hp0u0, "the shielded player still damages others");
    // and the window lapses: after SHIELD_TOTEM the shots land again
    s.player_mut("u1").unwrap().shield_totem_until = t;   // window closed
    let msg3 = json!({"w": 0, "ox": cm(o[0]), "oy": cm(o[1]), "oz": cm(o[2]), "r": 18000, "p": 0, "q": 30, "ip": 0});
    let _ = s.fire("u0", &msg3, t + 0.3);
    assert!(s.player("u1").unwrap().hp < hp0, "the bubble pops when the window ends");
}

// ---------------------------------------------------------------- movement --
#[test]
fn movement_is_checked() {
    let (mut s, t) = started(2, 2, 0.0);
    let p0 = eye(&s, "u0");
    // a teleport is refused and corrected
    let msg = json!({"x": cm(p0[0] + 10.0), "y": cm(p0[1]), "z": cm(p0[2] + 10.0), "r": 0, "p": 0, "q": 10});
    let (ok, fix) = s.pos("u0", &msg, t + 0.05);
    assert!(!ok, "a teleport is refused");
    assert!(fix.is_some(), "a correction comes back");
}

#[test]
fn sprint_needs_the_flag_and_stays_below_its_cap() {
    let (mut s, t) = started(2, 2, 0.0);
    put(&mut s, "u0", 0.0, 0.0, -8.0, t, Some(0));
    // legal walk-speed move first (0.3 s of clock, well within the walk cap) as the baseline
    let (ok, fix) = s.pos("u0", &json!({"x": cm(0.0), "y": cm(0.0), "z": cm(-8.0), "r": 0, "p": 0, "q": 30}), t + 0.3);
    assert!(ok && fix.is_none(), "a walk-speed move is accepted: {fix:?}");
    // 3.4 m in 0.3 s is past the walk cap (7.5 x 0.3 + SLACK = 3.25) but inside the sprint
    // cap (10.5 x 0.3 + SLACK = 4.15): a real sprint frame's burst. Without the flag: refused.
    let p = s.player("u0").unwrap();
    let (x0, z0) = (p.x, p.z);
    let (ok, fix) = s.pos("u0", &json!({"x": cm(x0 + 3.4), "y": cm(0.0), "z": cm(z0), "r": 0, "p": 0, "q": 60}), t + 0.6);
    assert!(!ok, "a sprint-speed burst without the flag is refused");
    assert!(fix.is_some(), "and corrected");
    // the same burst WITH the flag is accepted: the cap rose for this client
    let (ok, fix) = s.pos("u0", &json!({"x": cm(x0 + 3.4), "y": cm(0.0), "z": cm(z0), "r": 0, "p": 0, "q": 90, "sp": true}), t + 0.9);
    assert!(ok && fix.is_none(), "the sprint flag lets the raised cap through: {fix:?}");
    // but the higher cap still caps: a sprint-flag teleport is refused
    let p = s.player("u0").unwrap();
    let (x1, z1) = (p.x, p.z);
    let (ok, fix) = s.pos("u0", &json!({"x": cm(x1 + 20.0), "y": cm(0.0), "z": cm(z1), "r": 0, "p": 0, "q": 120, "sp": true}), t + 1.2);
    assert!(!ok, "the sprint flag is not a teleport license");
    assert!(fix.is_some());
}

// ---------------------------------------------------------------- bot aim wrap --
#[test]
fn bot_wire_rotation_stays_in_range() {
    // a bot's aim_y is an unwrapped accumulator: it is negative (or past 360)
    // whenever the aim sits in quadrants II/III (or swings past 0). Python
    // wraps with sign-free %; the wire value must land in 0..36000 too, so
    // this drives long swings through both and checks every snapshot r.
    let (mut s, t) = started(0, 2, 0.0); // no humans -> bots fill both sides
    let bi = s.players.iter().position(|(_, p)| p.bot.is_some()).unwrap();
    let bteam = s.players[bi].1.team;
    let ei = s.players.iter().position(|(_, p)| p.team != bteam).unwrap();
    let buid = s.players[bi].0.clone();
    let euid = s.players[ei].0.clone();
    put(&mut s, &buid, 0.0, 0.0, -8.0, t, None);
    put(&mut s, &euid, 8.0, 0.0, -8.0, t, None);
    let mut min_raw = f64::INFINITY;
    let mut max_raw = f64::NEG_INFINITY;
    for k in 0..600 {
        let t2 = t + 0.005 * k as f64;
        // walk the enemy around the compass so the aim sweeps every quadrant
        let (ex, ez) = ([8.0, 8.0, -8.0, -8.0][(k / 150) % 4], [-8.0, 8.0, 8.0, -8.0][(k / 150) % 4]);
        s.players[ei].1.x = ex;
        s.players[ei].1.z = ez;
        let _ = s.tick(t2, false);
        let raw = s.players[bi].1.bot.as_ref().unwrap().aim_y;
        min_raw = min_raw.min(raw);
        max_raw = max_raw.max(raw);
        let r = s.players[bi].1.r;
        assert!((0..36000).contains(&r), "wire rotation out of range: {r} (raw aim {raw:.2})");
    }
    assert!(min_raw < 0.0 || max_raw > 360.0,
        "aim stayed in 0..360 the whole run ({min_raw:.1}..{max_raw:.1}) -- the test no longer wraps");
    let _ = euid;
}

// ------------------------------------------------------------------ geometry --
#[test]
fn rays_and_los() {
    let m = &*MAP;
    // through the warehouse wall: the block breaks the line of sight
    let a = [2.0, EYE, -9.0];
    let b = [2.0, EYE, 9.0];
    assert!(!los(m, a, b), "the warehouse block breaks the lane's line of sight");
    // the centre doorway through the warehouse is a 1.6 m chokepoint, open at eye height
    let a = [0.0, EYE, -9.0];
    let b = [0.0, EYE, 9.0];
    assert!(los(m, a, b), "the centre doorway is open");
    // the west lane is open along the wall
    let a = [-17.7, EYE, -8.0];
    let b = [-17.7, EYE, 8.0];
    assert!(los(m, a, b), "the west lane is open along the wall");
    let d = dir_of(90.0, 0.0);
    assert!((d[0] - 1.0).abs() < 1e-12 && d[2].abs() < 1e-12);
    let (t, head) = ray_player([0.0, EYE, 10.0], dir_of(0.0, (1.65f64 - EYE).atan2(10.0).to_degrees()),
                               0.0, 0.0, 0.0, 100.0).unwrap();
    assert!(head && t < 10.0);
}

// ------------------------------------------------------------------ launcher --
#[test]
fn the_launcher_flies_and_blasts() {
    let (mut s, t) = started(2, 2, 0.0);
    put(&mut s, "u0", -6.0, 0.0, -9.0, t, Some(0));
    put(&mut s, "u1", -6.0, 0.0, -3.0, t, Some(0));   // 6 m downrange, same lane
    let o = eye(&s, "u0");
    let p = s.player_mut("u0").unwrap();
    p.w = 3;
    p.ready = 0;
    // u1 sits +z downrange of u0: yaw 180 points the shot there (dir_of: yaw 0 = -z)
    let msg = json!({"w": 3, "ox": cm(o[0]), "oy": cm(o[1]), "oz": cm(o[2]), "r": 18000, "p": 0, "q": 10, "ip": 0});
    let (shot, refuse) = s.fire("u0", &msg, t + 0.1);
    assert!(shot.is_some(), "the launcher shot is accepted (refuse: {refuse:?})");
    assert_eq!(s.rockets.len(), 1, "a rocket is in flight");
    assert_eq!(s.rocket_k, 1);
    // the rocket flies at MISSILE_V; ticks advance it until it reaches u1's body
    let mut t2 = t + 0.1;
    let hp0 = s.player("u1").unwrap().hp;
    for _ in 0..100 {
        t2 += 1.0 / HZ;
        let _ = s.tick(t2, true);
        if s.rockets.is_empty() {
            break;
        }
    }
    assert!(s.rockets.is_empty(), "the rocket must land within 5 s");
    // a direct-ish hit at ~6 m with the body in the lane: u1 takes heavy splash or a direct hit
    let hp1 = s.player("u1").unwrap().hp;
    assert!(hp1 < hp0, "the victim took blast damage ({hp0} -> {hp1})");
    // the shooter is honest too: the rocket left their muzzle, they're out of the radius
    // (6 m downrange > BLAST_R), so u0 keeps their hp
    // the boom was broadcast with a tick snapshot (flushed on send); r/b ride the same snap
    assert!(s.booms.is_empty(), "booms flush with the tick's snapshot");
}

#[test]
fn a_drop_life_rides_the_sky_without_the_two_second_snap() {
    // v4: a parachute spawn is airborne for up to DROP_TIME before the usual MAX_AIR guard
    // would fire — the referee must let it, not fix-slap a glide to the ground mid-fall.
    let (mut s, t) = started(2, 2, 0.0);
    put(&mut s, "u0", 0.0, 0.0, -8.0, t, Some(0));
    // lift the player to DROP_HEIGHT with a drop flag (as a v4 spawn event would), drop_t = now
    {
        let p = s.player_mut("u0").unwrap();
        p.y = DROP_HEIGHT;
        p.drop = true;
        p.drop_t = t;
        p.spawn_t = t;
        p.air_at = t;
        p.hist = VecDeque::from([(t, p.x, p.y, p.z)]);
    }
    // 2.2 s of gliding left (well past MAX_AIR = 2.0 s in the sky) — should be accepted, not
    // snapped. A v1-v3 life in the same state would be corrected.
    let (ok, fix) = s.pos("u0", &json!({"x": cm(0.0 + 2.5), "y": cm(DROP_HEIGHT - 1.5), "z": cm(-8.0), "r": 0, "p": 0, "q": 220}), t + 2.2);
    assert!(ok && fix.is_none(), "a mid-glide pos is accepted while the drop still has legs: {fix:?}");
    // after the drop window closes (past DROP_TIME), the same rules come back
    let (ok, fix) = s.pos("u0", &json!({"x": cm(0.0 + 4.0), "y": cm(DROP_HEIGHT - 2.0), "z": cm(-8.0), "r": 0, "p": 0, "q": 330}), t + 3.3);
    assert!(!ok || fix.is_some(), "past DROP_TIME the air guard fires again");
}

#[test]
fn the_shooters_own_blast_never_scores_a_direct() {
    // Regression: a shot detonating at the shooter's eye judged `dist < DIRECT_R` (0.8) as
    // direct → an instant 120 self-kill every time a bot fired through its own chin (its eye
    // sits ~0.6 m above the torso center the blast measures). The shooter's self-splash must
    // grade by proximity (75 max falloff), never the direct 120 — even at point-blank.
    let (mut s, t) = started(2, 2, 0.0);
    put(&mut s, "u0", -6.0, 0.0, -9.0, t, Some(0));
    put(&mut s, "u1", -6.0, 0.0, -3.0, t, Some(0));     // far away: no splash math on them
    let o = eye(&s, "u0");
    let p = s.player_mut("u0").unwrap();
    p.w = 3;
    p.ready = 0;
    // detonate a rocket AT u0's own eye — the worst-case self-blast
    let r = Rocket { id: 99, shooter: "u0".to_string(), x: o[0], y: o[1], z: o[2],
                     px: o[0], py: o[1], pz: o[2], vx: 0.0, vy: 0.0, vz: 0.0, born: t };
    s.detonate(&r, t + 0.2);
    let hp = s.player("u0").unwrap().hp;
    assert!(hp > 0, "the shooter survives their own point-blank blast (hp {hp})");
    assert!(hp >= 100 - 75, "self splash is falloff at worst, never the direct 120 (hp {hp})");
    assert!(!s.player("u0").unwrap().dead, "the shooter did not die to their own rocket");
}

#[test]
fn a_blast_hurts_the_shooter_too() {
    // an honest rocket: the shooter's own blast bites, with falloff
    let (mut s, t) = started(2, 2, 0.0);
    put(&mut s, "u0", -6.0, 0.0, -9.0, t, Some(0));
    let o = eye(&s, "u0");
    let p = s.player_mut("u0").unwrap();
    p.w = 3;
    p.ready = 0;
    // fire straight down (pitch -90): the rocket hits the ground at the shooter's feet
    let msg = json!({"w": 3, "ox": cm(o[0]), "oy": cm(o[1]), "oz": cm(o[2]), "r": 0, "p": -9000, "q": 10, "ip": 0});
    let (shot, _) = s.fire("u0", &msg, t + 0.1);
    assert!(shot.is_some());
    let mut t2 = t + 0.1;
    for _ in 0..100 {
        t2 += 1.0 / HZ;
        let _ = s.tick(t2, true);
        if s.rockets.is_empty() {
            break;
        }
    }
    assert!(s.rockets.is_empty(), "the rocket landed");
    assert!(s.player("u0").unwrap().hp < 100, "the shooter's own blast damaged them");
}

#[test]
fn bots_draw_weapons_across_the_roster() {
    // every life a bot re-rolls: across enough spawns the roster covers all four
    let (mut s, t) = started(0, 2, 0.0);
    let mut seen = [false; NW];
    let mut t2 = t;
    // watch the whole roster across the run, not just the end state: a weapon seen on
    // a life that later re-rolled still counts as drawn
    for k in 0..3000 {
        t2 += 1.0 / HZ;
        let _ = s.tick(t2, false);
        if k % 20 == 0 {
            for (_, p) in s.players.iter() {
                if p.is_bot() {
                    seen[p.w] = true;
                }
            }
        }
    }
    assert!(seen.iter().all(|x| *x), "bots must draw every weapon across respawns: {seen:?}");
}

#[test]
fn the_drop_shield_holds_off_bullets_and_blasts_while_you_glide() {
    // a shielded glider is untouchable: a clean hit through their chest does 0, and a
    // rocket at their feet does 0 — but the shooter's own blast still hurts the shooter.
    let (mut s, t) = started(2, 2, 0.0);
    put(&mut s, "u0", -6.0, 0.0, -9.0, t, Some(0));
    // u1 is mid-glide: airborne with a live drop flag (as a v4 respawn would be)
    put(&mut s, "u1", -6.0, DROP_HEIGHT, -12.0, t, Some(0));
    {
        let p = s.player_mut("u1").unwrap();
        p.drop = true;
        p.drop_t = t;
        p.air_at = t;
        p.hist = VecDeque::from([(t, p.x, p.y, p.z)]);
    }
    assert!(s.player("u1").unwrap().shielded(t), "a fresh drop life is shielded");
    // the shield is in the snapshot too, so every client can draw the bubble
    s.mark("u1");
    let snap = s.snap(t);
    let row = snap["p"].as_array().unwrap().iter()
        .find(|e| e[0].as_i64() == Some(slot(&s, "u1"))).unwrap();
    assert_ne!(row[8].as_i64().unwrap_or(0) & 32, 0, "the snapshot carries the shield flag");

    // aim straight at the glider's chest: the shot lands but the damage is 0
    let o = eye(&s, "u0");
    let tgt = chest(&s, "u1");
    let yaw = (tgt[0] - o[0]).atan2(-(tgt[2] - o[2])).to_degrees();
    let pitch = (tgt[1] - o[1]).atan2((tgt[0] - o[0]).hypot(tgt[2] - o[2])).to_degrees();
    let (dy, dp) = spread_of(1, 0);
    let msg = json!({"w": 0, "ox": cm(o[0]), "oy": cm(o[1]), "oz": cm(o[2]),
                     "r": ((yaw - dy) * 100.0) as i64, "p": ((pitch - dp) * 100.0) as i64, "q": 10, "ip": 0});
    let (shot, _) = s.fire("u0", &msg, t + 0.1);
    let shot = shot.unwrap();
    assert_eq!(shot[5], json!(slot(&s, "u1")), "the shot connects with the glider");
    assert_eq!(s.player("u1").unwrap().hp, HP, "the shield eats the hit: hp unchanged");

    // a rocket at the glider: the blast must not reach the shielded glider
    assert!(s.weapon("u0", &json!({"w": 3, "q": 15}), t + 0.15), "switch to the launcher");
    let msg = json!({"w": 3, "ox": cm(o[0]), "oy": cm(o[1]), "oz": cm(o[2]),
                     "r": 0, "p": 0, "q": 20, "ip": 0});
    let _ = s.fire("u0", &msg, t + 0.2);
    let mut now = t + 0.21;
    for _ in 0..80 { s.tick(now, true); now += 1.0 / 20.0; }
    assert_eq!(s.player("u1").unwrap().hp, HP, "the shield eats the blast: hp unchanged");

    // touchdown ends it: once grounded, the same chest shot kills like any other
    let (mut s2, t2) = started(2, 2, 0.0);
    put(&mut s2, "u0", -6.0, 0.0, -9.0, t2, Some(0));
    put(&mut s2, "u1", -6.0, 0.0, -12.0, t2, Some(0));
    let o = eye(&s2, "u0");
    let tgt = chest(&s2, "u1");
    let yaw = (tgt[0] - o[0]).atan2(-(tgt[2] - o[2])).to_degrees();
    let pitch = (tgt[1] - o[1]).atan2((tgt[0] - o[0]).hypot(tgt[2] - o[2])).to_degrees();
    let (dy, dp) = spread_of(1, 0);
    let msg = json!({"w": 0, "ox": cm(o[0]), "oy": cm(o[1]), "oz": cm(o[2]),
                     "r": ((yaw - dy) * 100.0) as i64, "p": ((pitch - dp) * 100.0) as i64, "q": 30, "ip": 0});
    let _ = s2.fire("u0", &msg, t2 + 0.1);
    assert_eq!(s2.player("u1").unwrap().hp, 88, "grounded, no shield: a body shot lands 12");
}

// -------------------------------------------------------- movement totems --
#[test]
fn the_movement_totems_relax_only_what_the_server_judged() {
    let (mut s, t) = started(2, 2, 0.0);
    put(&mut s, "u0", 0.0, 0.0, -8.0, t, Some(0));
    // a legal walk-speed move as the baseline
    let (ok, fix) = s.pos("u0", &json!({"x": cm(0.0), "y": cm(0.0), "z": cm(-8.0), "r": 0, "p": 0, "q": 30}), t + 0.3);
    assert!(ok && fix.is_none(), "a walk-speed move is accepted: {fix:?}");

    // --- speed: the dash cap comes from the server's own window, not the wire ---
    // 6.5 m in 0.3 s is past even the sprint cap (4.15) — refused bare, allowed dashing
    let p = s.player("u0").unwrap();
    let (x0, z0) = (p.x, p.z);
    let (ok, _) = s.pos("u0", &json!({"x": cm(x0 + 6.5), "y": cm(0.0), "z": cm(z0), "r": 0, "p": 0, "q": 60}), t + 0.6);
    assert!(!ok, "a dash-speed burst without the speed totem is refused");
    s.player_mut("u0").unwrap().speed_until = t + 10.0;
    let (ok, fix) = s.pos("u0", &json!({"x": cm(x0 + 6.5), "y": cm(0.0), "z": cm(z0), "r": 0, "p": 0, "q": 90}), t + 0.9);
    assert!(ok && fix.is_none(), "the speed totem's window buys the dash cap: {fix:?}");
    // but the dash cap still caps: 10 m in 0.3 s is past even SPEED_MULT x sprint (6.3 + 1)
    let p = s.player("u0").unwrap();
    let (x1, z1) = (p.x, p.z);
    let (ok, _) = s.pos("u0", &json!({"x": cm(x1 + 10.0), "y": cm(0.0), "z": cm(z1), "r": 0, "p": 0, "q": 120}), t + 1.2);
    assert!(!ok, "the speed totem is not a teleport license");

    // --- leap: the tall jump rises past the plain JUMP_V gate ---
    // 4.5 m up in 0.3 s is past the plain gate (6.5 x 0.3 + 0.55 + 1 = 3.5) — refused
    // bare, allowed leaping (19.5 x 0.3 + 1.55 = 7.4)
    put(&mut s, "u0", 0.0, 0.0, -8.0, t + 1.3, Some(150));
    s.player_mut("u0").unwrap().leap_until = t + 10.0;
    let (ok, fix) = s.pos("u0", &json!({"x": cm(0.0), "y": cm(4.5), "z": cm(-8.0), "r": 0, "p": 0, "q": 180}), t + 1.6);
    assert!(ok && fix.is_none(), "the leap totem's window buys the tall jump: {fix:?}");
    // without the window the same rise is refused
    s.player_mut("u0").unwrap().leap_until = 0.0;
    let (ok, _) = s.pos("u0", &json!({"x": cm(0.0), "y": cm(9.0), "z": cm(-8.0), "r": 0, "p": 0, "q": 210}), t + 1.9);
    assert!(!ok, "a plain life cannot rise like a leaper");

    // --- wings: the flight parks the air clock ---
    // rise into the air, then hover past MAX_AIR (2 s of unbroken air): refused bare,
    // allowed while winged. put() refreshes the clock anchor, so the air clock starts
    // fresh at t+2.0 and the hover at t+4.5 is 2.5 s of air — past MAX_AIR.
    put(&mut s, "u0", 0.0, 4.0, -8.0, t + 2.0, Some(240));
    s.player_mut("u0").unwrap().air_at = t + 2.0;
    s.player_mut("u0").unwrap().wings_until = 0.0;
    let tt = t + 4.5;
    let (ok, _) = s.pos("u0", &json!({"x": cm(0.0), "y": cm(4.0), "z": cm(-8.0), "r": 0, "p": 0, "q": 450}), tt);
    assert!(!ok, "a hover past MAX_AIR is refused without the wings totem");
    s.player_mut("u0").unwrap().wings_until = t + 30.0;
    let (ok, fix) = s.pos("u0", &json!({"x": cm(0.0), "y": cm(4.0), "z": cm(-8.0), "r": 0, "p": 0, "q": 460}), tt + 0.1);
    assert!(ok && fix.is_none(), "the wings totem parks the air clock: {fix:?}");
    // the take splits cruise from judge: the shown window is the WINGS cruise, the judged
    // window also covers the automatic glide down (the client flies all three without input)
    let i0 = s.players.iter().position(|(u, _)| u == "u0").unwrap();
    assert!(s.take_totem(i0, "wings", t + 5.0), "wings applies to its taker");
    let p = s.player("u0").unwrap();
    assert_eq!(p.wings_fly, t + 5.0 + WINGS, "cruise = the shown window");
    assert_eq!(p.wings_until, t + 5.0 + WINGS + WINGS_LAND, "judged = cruise + the glide down");
    // the descent past the cruise boundary is still wings flight: a mid-air stay at
    // cruise + 3 s of descent is legal while wings_until runs, refused once it lapses
    put(&mut s, "u0", 0.0, 9.0, -8.0, t + 11.0, Some(1100));
    s.player_mut("u0").unwrap().air_at = t + 11.0;
    let (ok, fix) = s.pos("u0", &json!({"x": cm(0.0), "y": cm(8.0), "z": cm(-8.0), "r": 0, "p": 0, "q": 1410}), t + 14.1);
    assert!(ok && fix.is_none(), "the automatic glide down is still wings flight: {fix:?}");
    let (ok, _) = s.pos("u0", &json!({"x": cm(0.0), "y": cm(7.0), "z": cm(-8.0), "r": 0, "p": 0, "q": 2020}), t + 20.2);
    assert!(!ok, "past wings + WINGS_LAND there is no more flight");
    // and a death pops all three windows (place() resets them with the life)
    let spawn = [0.0, 0.0, -8.0, 0.0];
    s.player_mut("u0").unwrap().dead = true;
    s.player_mut("u0").unwrap().place(spawn, t + 21.0);
    let s2 = s.player("u0").unwrap();
    assert_eq!((s2.wings_until, s2.wings_fly, s2.leap_until, s2.speed_until), (0.0, 0.0, 0.0, 0.0),
               "a respawn starts with every movement window closed");
}

#[test]
fn the_host_picks_one_bot_skill_for_both_sides() {
    // MP lobbies fill short sides with bots on BOTH teams, so one skill level applies to
    // the whole match — never a sharper filler's side against a duller's
    let mut s = Strike::new();
    assert_eq!(s.start(&members(2, "u"), &json!(5), &json!(30), &json!(3), &json!("hard"), 0.0), None);
    assert_eq!(s.bot_diff, "hard");
    assert_eq!(s.bot_aim, 0.5);
    assert_eq!(s.bot_react, 0.35);
    assert_eq!(s.bot_turn, 1.6);
    assert_eq!(s.bot_rate, 0.8);
    // an unknown id — including a crafted wire one — quietly falls back to normal
    let mut s2 = Strike::new();
    s2.start(&members(2, "u"), &json!(5), &json!(30), &json!(3), &json!("nightmare"), 0.0);
    assert_eq!(s2.bot_diff, "normal");
    assert_eq!(s2.bot_aim, 1.0);
    // and a match started without a diff is normal (the host didn't pick one)
    let mut s3 = Strike::new();
    s3.start(&members(2, "u"), &json!(5), &json!(30), &json!(3), &Value::Null, 0.0);
    assert_eq!(s3.bot_diff, "normal");
}

#[test]
fn the_teleport_totem_plants_the_taker_on_open_ground() {
    let (mut s, t) = started(2, 2, 0.0);
    let (x0, z0) = { let p = s.player("u0").unwrap(); (p.x, p.z) };
    let i0 = s.players.iter().position(|(u, _)| u == "u0").unwrap();
    assert!(s.take_totem(i0, "teleport", t + 1.0), "the teleport applies to its taker");
    let p = s.player("u0").unwrap();
    assert!((p.x, p.z) != (x0, z0), "the taker moved somewhere else");
    assert_eq!(p.y, 0.0, "always on the ground, never in the air");
    assert!((-17.5..=17.5).contains(&p.x), "inside the drop scatter's x range");
    assert!((-10.5..=10.5).contains(&p.z), "inside the drop scatter's z range");
    assert!(overlaps(&MAP, p.x, 0.1, p.z, 0.3).is_none(), "never inside a box");
    assert_eq!(p.air_at, t + 1.0, "the air clock restarts at the destination");
    // two hops in a row land somewhere fresh (the seed advances with the take counter)
    let (px, pz) = (p.x, p.z);
    assert!(s.take_totem(i0, "teleport", t + 2.0));
    let p = s.player("u0").unwrap();
    assert!((p.x, p.z) != (px, pz), "the second hop lands somewhere fresh");
    // and a walk-speed pos frame from the destination is accepted (no correction)
    let (tx, tz) = (p.x, p.z);
    let (ok, fix) = s.pos("u0", &json!({"x": cm(tx), "y": 0, "z": cm(tz), "r": 0, "p": 0, "q": 100}), t + 2.1);
    assert!(ok && fix.is_none(), "the next frame starts from the destination: {fix:?}");
}
