use super::*;
use chrono::TimeZone;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};

/// A file-backed temp database (WAL, 4 connections) that deletes itself.
struct Db {
    pool: SqlitePool,
    path: std::path::PathBuf,
}

impl Drop for Db {
    fn drop(&mut self) {
        for ext in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{ext}", self.path.display()));
        }
    }
}

async fn db() -> Db {
    let path = std::env::temp_dir().join(format!("boss-test-{}.db", uuid::Uuid::new_v4().simple()));
    let opts = SqliteConnectOptions::new()
        .filename(&path)
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .busy_timeout(std::time::Duration::from_secs(10))
        .foreign_keys(true);
    let pool = SqlitePoolOptions::new().max_connections(4).connect_with(opts).await.unwrap();
    crate::db::migrate(&pool).await.unwrap();
    for i in 1..=8 {
        sqlx::query(
            "INSERT INTO users (id, github_id, handle, display_name, avatar_url, trainer_name, is_active, created_at)
             VALUES (?1, ?2, ?3, ?4, '', '', 1, datetime('now'))",
        )
        .bind(format!("u{i}"))
        .bind(1000 + i)
        .bind(format!("h{i}"))
        .bind(format!("Player {i}"))
        .execute(&pool)
        .await
        .unwrap();
    }
    Db { pool, path }
}

/// Wednesday of 2026-W41 (Mon 2026-10-05 .. Sun 2026-10-11).
fn now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 7, 12, 0, 0).unwrap()
}
const WEEK: &str = "2026-W41";

async fn pr(p: &SqlitePool, user: &str, day: &str, n: i64) {
    sqlx::query("INSERT INTO work_events (user_id, type, day, n, request_id) VALUES (?1, 'pr_merged', ?2, ?3, ?4)")
        .bind(user)
        .bind(day)
        .bind(n)
        .bind(uuid::Uuid::new_v4().simple().to_string())
        .execute(p)
        .await
        .unwrap();
}

async fn result(p: &SqlitePool, user: &str, at: &str, place: i64, players: i64) {
    sqlx::query(
        "INSERT INTO game_results (user_id, game, \"key\", mode, place, players, value, extra, at)
         VALUES (?1, 'kart', 'dunes', '', ?2, ?3, 1000, '{}', ?4)",
    )
    .bind(user)
    .bind(place)
    .bind(players)
    .bind(at)
    .execute(p)
    .await
    .unwrap();
}

async fn state(p: &SqlitePool, uid: &str) -> Value {
    serde_json::to_value(state_for(p, uid, now()).await.unwrap()).unwrap()
}

async fn week(p: &SqlitePool) -> WeekRow {
    let mut c = p.acquire().await.unwrap();
    ensure_week(&mut c, WEEK).await.unwrap()
}

/// 12 player-days of 5 PRs: 9000 of drain, so the boss (max 10000) sits on its
/// 15% floor at 1500, in the fight phase.
async fn to_fight_phase(p: &SqlitePool) {
    for (u, d) in [("u5", 5), ("u5", 6), ("u5", 7), ("u6", 5), ("u6", 6), ("u6", 7)] {
        pr(p, u, &format!("2026-10-{d:02}"), 5).await;
    }
    for (u, d) in [("u7", 5), ("u7", 6), ("u7", 7), ("u8", 5), ("u8", 6), ("u8", 7)] {
        pr(p, u, &format!("2026-10-{d:02}"), 9).await;
    }
}

fn rid(s: &str) -> String {
    format!("boss-test-{s:0>8}")
}

async fn body(r: Response) -> (StatusCode, Value) {
    let code = r.status();
    let b = axum::body::to_bytes(r.into_body(), usize::MAX).await.unwrap();
    (code, serde_json::from_slice(&b).unwrap())
}

fn team() -> Vec<Spec> {
    // Charizard, Venusaur, Gengar at stage 4.
    [(1, 4), (3, 4), (5, 4)]
        .iter()
        .map(|&(sp, st)| Spec { sp, st, br: None, mg: None, sh: false, name: String::new() })
        .collect()
}

// ------------------------------------------------------------- the boss --

#[test]
fn boss_dex_resolves_to_at_least_one_species_and_every_week_picks_one() {
    let list = bosses();
    assert!(!list.is_empty());
    for w in ["2026-W41", "2026-W42", "2027-W01"] {
        let d = pick_boss(w);
        assert!(list.contains(&d), "{w} picked {d}");
        assert_eq!(pick_boss(w), d, "the pick is stable");
        let m = boss_mon(d).expect("boss builds");
        assert_eq!(m.dex, d);
        assert_eq!(m.lvl, 70);
        assert!(!m.moves.is_empty());
    }
}

#[test]
fn the_boss_is_its_own_species_at_level_70_with_four_times_the_hp() {
    // Scyther is the line's FIRST stage: the override, not the stage-4 build
    // (Scizor), decides who the boss is.
    let m = boss_mon(123).unwrap();
    assert_eq!((m.name.as_str(), m.dex), ("Scyther", 123));
    let e = &DATA.pokemon[&DATA.by_dex["123"]];
    assert_eq!(m.stats.atk, calc_stat(e.bs.atk, 55, false) * 70 / 55);
    assert_eq!(m.max, 4 * (calc_stat(e.bs.hp, 55, true) * 70 / 55));
    assert_eq!(m.hp, m.max);
    assert_eq!(m.types, e.types);
}

#[test]
fn a_full_knockout_is_exactly_five_percent_and_never_more() {
    for max_hp in [10_000, 12_000, 2_000_000] {
        assert_eq!(fight_damage(1234, 1234, max_hp), max_hp / 20);
        assert_eq!(fight_damage(9999, 1234, max_hp), max_hp / 20, "overkill is capped");
        assert_eq!(fight_damage(617, 1234, max_hp), max_hp / 40);
        assert_eq!(fight_damage(0, 1234, max_hp), 0);
        assert_eq!(fight_damage(-5, 1234, max_hp), 0);
    }
    assert_eq!(fight_damage(10, 0, 10_000), 0);
}

#[test]
fn a_fight_replays_identically_from_its_seed() {
    let a = simulate(WEEK, "u1", &rid("a"), 214, &team()).unwrap();
    let b = simulate(WEEK, "u1", &rid("a"), 214, &team()).unwrap();
    assert_eq!(a.log, b.log);
    assert_eq!(a.lost, b.lost);
    assert!(a.lost > 0, "three stage-4 Pokémon dent the boss");
    assert!(a.log.len() <= MAX_TURNS * 40);
    assert_eq!(a.team.len(), 3);
    assert_eq!(a.boss["dex"], 214);
    let c = simulate(WEEK, "u2", &rid("a"), 214, &team()).unwrap();
    // Another seed may or may not change the outcome, but must still be valid.
    assert!(c.lost >= 0 && c.lost <= c.boss_max);
}

#[test]
fn the_fight_body_is_strict() {
    let ok = br#"{"requestId":"abcdefghijklmnop","team":[{"sp":1,"st":4,"br":null,"mg":null,"sh":false}]}"#;
    let (r, t) = fight_body(ok).unwrap();
    assert_eq!((r.as_str(), t.len()), ("abcdefghijklmnop", 1));
    for bad in [
        &br#"[]"#[..],
        br#"{"requestId":"short","team":[{"sp":1,"st":4}]}"#,
        br#"{"requestId":"abcdefghijklmnop","team":[]}"#,
        br#"{"requestId":"abcdefghijklmnop","team":[{"sp":1,"st":4}],"x":1}"#,
        br#"{"requestId":"abcdefghijklmnop","team":[{"sp":99,"st":4}]}"#,
        br#"{"requestId":"abcdefghijklmnop","team":[{"sp":1,"st":4.0}]}"#,
        br#"{"requestId":"abcdefghijklmnop","team":[{"sp":1,"st":4,"path":"x"}]}"#,
        br#"{"requestId":"abcdefghijklmnop","team":[1,2]}"#,
        br#"{"requestId":"abc defghijklmnop","team":[{"sp":1,"st":4}]}"#,
        br#"{"requestId":"abcdefghijklmnop","team":[{"sp":1,"st":4},{"sp":1,"st":4},{"sp":1,"st":4},{"sp":1,"st":4},{"sp":1,"st":4},{"sp":1,"st":4},{"sp":1,"st":4}]}"#,
    ] {
        assert!(fight_body(bad).is_err(), "{}", String::from_utf8_lossy(bad));
    }
    // A stage out of range is clamped, not refused (clean_spec's contract).
    let (_, t) = fight_body(br#"{"requestId":"abcdefghijklmnop","team":[{"sp":1,"st":9}]}"#).unwrap();
    assert_eq!(t[0].st, 4);
}

// ------------------------------------------------------------- the ledger --

#[tokio::test]
async fn max_hp_counts_last_weeks_players_with_a_floor_of_five() {
    let d = db().await;
    assert_eq!(week(&d.pool).await.max_hp, 10_000, "nobody played: five-player floor");

    let d = db().await;
    for u in 1..=6 {
        result(&d.pool, &format!("u{u}"), "2026-09-30 10:00:00", 2, 2).await;
    }
    pr(&d.pool, "u7", "2026-10-04", 1).await; // Sunday of W40
    pr(&d.pool, "u8", "2026-10-05", 1).await; // this week: not counted
    result(&d.pool, "u8", "2026-09-28 00:00:00", 1, 1).await; // Monday of W40
    let w = week(&d.pool).await;
    assert_eq!(w.max_hp, 2000 * 8);
    assert_eq!(w.dex, pick_boss(WEEK));
    // Created once: later activity never moves it.
    result(&d.pool, "u5", "2026-09-30 11:00:00", 1, 2).await;
    sqlx::query("INSERT INTO users (id, github_id, handle, display_name, avatar_url, trainer_name, is_active, created_at) VALUES ('u9', 9, 'h9', '', '', '', 1, datetime('now'))").execute(&d.pool).await.unwrap();
    result(&d.pool, "u9", "2026-09-30 11:00:00", 1, 2).await;
    assert_eq!(week(&d.pool).await.max_hp, 16_000);
}

#[tokio::test]
async fn seeded_events_and_results_give_the_exact_hp_with_daily_caps() {
    let d = db().await;
    let p = &d.pool;
    // u1: 3 + 4 PRs on Monday (7 -> capped at 5 = 750), 2 on Tuesday (300).
    pr(p, "u1", "2026-10-05", 3).await;
    pr(p, "u1", "2026-10-05", 4).await;
    pr(p, "u1", "2026-10-06", 2).await;
    // Not counted: last week, and another event type.
    pr(p, "u1", "2026-10-04", 5).await;
    sqlx::query("INSERT INTO work_events (user_id, type, day, n, request_id) VALUES ('u1','tests_green','2026-10-05',3,'tg-1')")
        .execute(p)
        .await
        .unwrap();
    // u2: 12 multiplayer wins on Monday (capped at 10 = 500), one on Sunday (50).
    for i in 0..12 {
        result(p, "u2", &format!("2026-10-05 10:{i:02}:00"), 1, 3).await;
    }
    result(p, "u2", "2026-10-11 23:59:59.999999", 1, 2).await;
    // Not counted: solo, second place, next week.
    result(p, "u2", "2026-10-06 10:00:00", 1, 1).await;
    result(p, "u2", "2026-10-06 10:00:00", 2, 4).await;
    result(p, "u2", "2026-10-12 00:00:00", 1, 2).await;

    let s = state(p, "u1").await;
    assert_eq!(s["week"], WEEK);
    assert_eq!(s["endsAt"], "2026-10-11 23:59:59.999999");
    assert_eq!(s["boss"]["maxHp"], 10_000);
    assert_eq!(s["boss"]["hp"], 10_000 - 750 - 300 - 500 - 50);
    assert_eq!(s["boss"]["phase"], "drain");
    assert_eq!(s["you"]["sources"], serde_json::json!({"pr": 1050, "win": 0, "fight": 0}));
    assert_eq!(s["you"]["dmg"], 1050);
    assert_eq!(s["you"]["rank"], 1);
    assert_eq!(s["you"]["attemptsLeft"], 3);
    assert_eq!(s["top"][1]["userId"], "u2");
    assert_eq!(s["top"][1]["dmg"], 550);
    assert_eq!(s["top"][1]["handle"], "h2");
    assert_eq!(s["finalBlow"], Value::Null);
    assert_eq!(s["lastWeek"], Value::Null);
    let s3 = state(p, "u3").await;
    assert_eq!(s3["you"]["rank"], Value::Null);
    assert_eq!(s3["you"]["dmg"], 0);
}

#[tokio::test]
async fn drain_alone_stops_at_exactly_fifteen_percent() {
    let d = db().await;
    to_fight_phase(&d.pool).await;
    let s = state(&d.pool, "u1").await;
    assert_eq!(s["boss"]["hp"], 1500);
    assert_eq!(s["boss"]["phase"], "fight");
    // More drain changes nothing.
    pr(&d.pool, "u1", "2026-10-08", 5).await;
    assert_eq!(state(&d.pool, "u1").await["boss"]["hp"], 1500);
}

#[tokio::test]
async fn fights_are_refused_until_the_fight_phase() {
    let d = db().await;
    pr(&d.pool, "u1", "2026-10-05", 5).await;
    let r = apply_fight(&d.pool, "u1", &rid("early"), WEEK, 500, now()).await;
    assert_eq!(r, Err(Refuse::Say(StatusCode::CONFLICT, "the boss isn't weak enough to fight yet")));
    // The refusal rolled back: no row, no attempt used.
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM boss_fights").fetch_one(&d.pool).await.unwrap();
    assert_eq!(n, 0);
}

#[tokio::test]
async fn one_full_ko_fight_removes_exactly_five_percent() {
    let d = db().await;
    to_fight_phase(&d.pool).await;
    let w = week(&d.pool).await;
    let a = apply_fight(&d.pool, "u1", &rid("ko"), WEEK, fight_damage(777, 777, w.max_hp), now()).await.unwrap();
    assert_eq!(a.dmg, 500);
    assert_eq!(a.hp, 1000);
    assert!(!a.final_blow);
    let s = state(&d.pool, "u1").await;
    assert_eq!(s["boss"]["hp"], 1000);
    assert_eq!(s["you"]["sources"]["fight"], 500);
    assert_eq!(s["you"]["attemptsLeft"], 2);
}

#[tokio::test]
async fn the_fourth_attempt_in_a_day_is_429_and_tomorrow_is_fine() {
    let d = db().await;
    to_fight_phase(&d.pool).await;
    for i in 0..3 {
        apply_fight(&d.pool, "u1", &rid(&format!("d{i}")), WEEK, 10, now()).await.unwrap();
    }
    let r = apply_fight(&d.pool, "u1", &rid("d3"), WEEK, 10, now()).await;
    assert!(matches!(r, Err(Refuse::Say(StatusCode::TOO_MANY_REQUESTS, _))));
    let r = fight_for(&d.pool, "u1", &rid("d4"), &team(), now()).await;
    assert_eq!(r.status(), StatusCode::TOO_MANY_REQUESTS);
    // Another player is unaffected, and so is u1 tomorrow.
    apply_fight(&d.pool, "u2", &rid("o1"), WEEK, 10, now()).await.unwrap();
    apply_fight(&d.pool, "u1", &rid("t1"), WEEK, 10, now() + Duration::days(1)).await.unwrap();
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM boss_fights WHERE user_id = 'u1'")
        .fetch_one(&d.pool)
        .await
        .unwrap();
    assert_eq!(n, 4);
}

#[tokio::test]
async fn the_same_request_id_replays_the_same_result_without_a_second_row() {
    let d = db().await;
    to_fight_phase(&d.pool).await;
    let (c1, a) = body(fight_for(&d.pool, "u1", &rid("same"), &team(), now()).await).await;
    assert_eq!(c1, StatusCode::OK, "{a}");
    let (c2, b) = body(fight_for(&d.pool, "u1", &rid("same"), &team(), now()).await).await;
    assert_eq!(c2, StatusCode::OK);
    assert_eq!(a, b);
    assert!(a["dmg"].as_i64().unwrap() > 0);
    assert_eq!(a["hp"].as_i64().unwrap(), 1500 - a["dmg"].as_i64().unwrap());
    assert!(!a["log"].as_array().unwrap().is_empty());
    assert_eq!(a["boss"]["lvl"], 70);
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM boss_fights").fetch_one(&d.pool).await.unwrap();
    assert_eq!(n, 1);
    // Only the fight counts against the day, once.
    assert_eq!(a["attemptsLeft"], 2);
    // The wire never carries a forbidden key.
    let forbidden = ["prompt", "text", "content", "path", "paths", "title", "file", "files", "cwd", "folder", "project", "sessionId"];
    fn keys(v: &Value, out: &mut Vec<String>) {
        match v {
            Value::Object(o) => o.iter().for_each(|(k, v)| {
                out.push(k.clone());
                keys(v, out)
            }),
            Value::Array(a) => a.iter().for_each(|v| keys(v, out)),
            _ => {}
        }
    }
    let mut all = Vec::new();
    keys(&a, &mut all);
    keys(&state(&d.pool, "u1").await, &mut all);
    for k in forbidden {
        assert!(!all.iter().any(|x| x == k), "wire carries {k}");
    }
}

#[tokio::test]
async fn two_concurrent_killing_fights_record_exactly_one_final_blow() {
    let d = db().await;
    to_fight_phase(&d.pool).await;
    apply_fight(&d.pool, "u1", &rid("k1"), WEEK, 500, now()).await.unwrap();
    apply_fight(&d.pool, "u2", &rid("k2"), WEEK, 500, now()).await.unwrap();
    // 500 left: either of the next two full KOs finishes it.
    let (r3, r4) = (rid("k3"), rid("k4"));
    let (a, b) = tokio::join!(
        apply_fight(&d.pool, "u3", &r3, WEEK, 500, now()),
        apply_fight(&d.pool, "u4", &r4, WEEK, 500, now()),
    );
    let wins: Vec<&Applied> = [&a, &b].into_iter().filter_map(|r| r.as_ref().ok()).collect();
    assert_eq!(wins.len(), 1, "{a:?} {b:?}");
    assert!(wins[0].final_blow);
    assert_eq!(wins[0].hp, 0);
    let lost = if a.is_ok() { &b } else { &a };
    assert_eq!(lost, &Err(Refuse::Say(StatusCode::CONFLICT, "the boss is already down this week")));
    let blows: Vec<(String,)> =
        sqlx::query_as("SELECT final_blow_user FROM boss_weeks WHERE final_blow_user IS NOT NULL")
            .fetch_all(&d.pool)
            .await
            .unwrap();
    assert_eq!(blows.len(), 1);
    let s = state(&d.pool, "u1").await;
    assert_eq!(s["boss"]["phase"], "down");
    assert_eq!(s["boss"]["hp"], 0);
    assert_eq!(s["finalBlow"]["userId"], blows[0].0.as_str());
    // A replay of the killing request still says it was the final blow.
    let killer = if a.is_ok() { ("u3", rid("k3")) } else { ("u4", rid("k4")) };
    let r = replayed(&d.pool, killer.0, &killer.1).await.unwrap().unwrap();
    assert!(r.final_blow);
    let r = replayed(&d.pool, "u1", &rid("k1")).await.unwrap().unwrap();
    assert!(!r.final_blow);
}

#[tokio::test]
async fn badges_list_the_slayer_week_and_last_week_shows_on_the_next() {
    let d = db().await;
    to_fight_phase(&d.pool).await;
    for (u, r) in [("u1", "b1"), ("u2", "b2"), ("u3", "b3")] {
        apply_fight(&d.pool, u, &rid(r), WEEK, 500, now()).await.unwrap();
    }
    let b = badges_for(&d.pool, "u3").await.unwrap();
    assert_eq!(b.slayer, vec![WEEK.to_string()]);
    assert_eq!(b.participated, 1);
    let b = badges_for(&d.pool, "u1").await.unwrap();
    assert!(b.slayer.is_empty());
    assert_eq!(b.participated, 1);
    let b = badges_for(&d.pool, "u7").await.unwrap();
    assert_eq!((b.slayer.len(), b.participated), (0, 0));

    let next = serde_json::to_value(state_for(&d.pool, "u1", now() + Duration::days(7)).await.unwrap()).unwrap();
    assert_eq!(next["week"], "2026-W42");
    assert_eq!(next["boss"]["phase"], "drain");
    assert_eq!(next["lastWeek"]["week"], WEEK);
    assert_eq!(next["lastWeek"]["defeated"], true);
    assert_eq!(next["lastWeek"]["finalBlow"]["handle"], "h3");
    // W41 had four players with work events (fights alone don't count): the floor.
    assert_eq!(next["boss"]["maxHp"], 10_000);
}
