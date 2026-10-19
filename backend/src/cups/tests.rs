use super::*;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};

// --- pure scoring --------------------------------------------------------

fn row(u: &str, place: i64, players: i64, at: &str) -> RaceRow {
    RaceRow { user_id: u.into(), place, players, dnf: false, at: at.into() }
}

/// One race: users in finishing order, at a timestamp.
fn race(at: &str, order: &[&str]) -> Vec<RaceRow> {
    order.iter().enumerate().map(|(i, u)| row(u, i as i64 + 1, order.len() as i64, at)).collect()
}

fn pts(t: &[Score], u: &str) -> i64 {
    t.iter().find(|s| s.user_id == u).map(|s| s.points).unwrap_or(-1)
}

#[test]
fn the_points_table() {
    assert_eq!(points_for(1, false), 10);
    assert_eq!(points_for(2, false), 7);
    assert_eq!(points_for(3, false), 5);
    assert_eq!(points_for(4, false), 3);
    assert_eq!(points_for(5, false), 1);
    assert_eq!(points_for(8, false), 1);
    assert_eq!(points_for(1, true), 0);
    assert_eq!(points_for(0, false), 0);
}

#[test]
fn places_score_and_only_the_best_five_rows_count() {
    // ann races 7 times against 7 different fields, so no opponent set repeats.
    let fields = ["b", "c", "d", "e", "f", "g", "h"];
    let mut rows = Vec::new();
    for (i, f) in fields.iter().enumerate() {
        let at = format!("2026-10-0{} 10:00:00", i + 1);
        // ann wins the first three, is 2nd in the next two, 3rd in the last two.
        let order: Vec<&str> = match i { 0..=2 => vec!["ann", f], 3 | 4 => vec![f, "ann"], _ => vec![f, "x", "ann"] };
        rows.extend(race(&at, &order));
    }
    let t = score(&rows);
    // best five of 10,10,10,7,7,5,5 = 44; seven races entered.
    let ann = t.iter().find(|s| s.user_id == "ann").unwrap();
    assert_eq!((ann.points, ann.races, ann.wins), (44, 7, 3));
    assert_eq!(t[0].user_id, "ann");
    assert_eq!(t[0].place, 1);
    // x: 2nd twice, against two different fields (7 + 7). b: one 2nd (7).
    assert_eq!(pts(&t, "x"), 14);
    assert_eq!(pts(&t, "b"), 7);
    assert_eq!(pts(&t, "d"), 7);
    assert_eq!(pts(&t, "e"), 10);
}

#[test]
fn ties_go_to_more_wins_then_to_the_earlier_total() {
    // p: 10 + 3 (a win). q: 7 + 5 + 1 = 13 (no win). p ranks first on wins.
    let mut rows = race("2026-10-05 10:00:00", &["p", "z1"]);
    rows.extend(race("2026-10-05 11:00:00", &["z2", "z3", "z4", "p"]));
    rows.extend(race("2026-10-05 12:00:00", &["z5", "q"]));
    rows.extend(race("2026-10-05 13:00:00", &["z6", "z7", "q"]));
    rows.extend(race("2026-10-05 14:00:00", &["z8", "z9", "za", "zb", "q"]));
    let t = score(&rows);
    assert_eq!(pts(&t, "p"), 13);
    assert_eq!(pts(&t, "q"), 13);
    let pi = t.iter().position(|s| s.user_id == "p").unwrap();
    let qi = t.iter().position(|s| s.user_id == "q").unwrap();
    assert!(pi < qi, "more wins first");

    // r and s: same points, same wins; r reached it on Monday, s on Tuesday.
    let mut rows = race("2026-10-06 09:00:00", &["s", "o1"]);
    rows.extend(race("2026-10-05 09:00:00", &["r", "o2"]));
    let t = score(&rows);
    assert_eq!(t[0].user_id, "r");
    assert_eq!(t[1].user_id, "s");
    assert_eq!(t[0].last_scoring.as_deref(), Some("2026-10-05 09:00:00"));
}

#[test]
fn solo_rows_are_ignored_and_a_dnf_scores_nothing() {
    let mut rows = vec![row("solo", 1, 1, "2026-10-05 09:00:00")];
    let mut r = race("2026-10-05 10:00:00", &["a", "b"]);
    r[0].dnf = true; // a crashed out: 0, though the row says place 1
    rows.extend(r);
    let t = score(&rows);
    assert!(t.iter().all(|s| s.user_id != "solo"));
    assert_eq!(pts(&t, "a"), 0);
    assert_eq!(pts(&t, "b"), 7);
    let a = t.iter().find(|s| s.user_id == "a").unwrap();
    assert_eq!((a.races, a.wins, a.last_scoring.clone()), (1, 0, None));
}

#[test]
fn the_same_two_racing_six_times_count_twice_each() {
    let mut rows = Vec::new();
    for i in 0..6 {
        rows.extend(race(&format!("2026-10-05 1{i}:00:00"), &["alice", "bob"]));
    }
    let t = score(&rows);
    let a = t.iter().find(|s| s.user_id == "alice").unwrap();
    let b = t.iter().find(|s| s.user_id == "bob").unwrap();
    assert_eq!((a.points, a.races, a.wins), (20, 6, 2));
    assert_eq!((b.points, b.races), (14, 6));
    // A third player in the race is a different opponent set: it counts again.
    rows.extend(race("2026-10-06 10:00:00", &["alice", "bob", "cara"]));
    let t = score(&rows);
    assert_eq!(pts(&t, "alice"), 30);
    assert_eq!(pts(&t, "bob"), 21);
}

#[test]
fn the_key_is_deterministic_and_never_tower_defense() {
    for g in CUP_GAMES {
        let k = cup_key("2026-W41", g).expect("every cup game has a key");
        assert_eq!(cup_key("2026-W41", g).as_deref(), Some(k.as_str()));
        assert!(!k.starts_with("c-"));
        assert!(candidates(g).contains(&k));
    }
    assert_eq!(cup_key("2026-W41", "fps").as_deref(), Some("match"));
    assert_eq!(cup_key("2026-W41", "type").as_deref(), Some("all"));
    assert_eq!(cup_key("2026-W41", "bowl").as_deref(), Some("f10"));
    // The kart rotation actually rotates over a season of weeks.
    let picks: HashSet<String> = (1..=20).map(|w| cup_key(&format!("2026-W{w:02}"), "kart").unwrap()).collect();
    assert!(picks.len() >= 3, "{picks:?}");
    // Tower Defense is co-op: no places, no cup.
    assert!(!CUP_GAMES.contains(&"td"));
    assert_eq!(cup_key("2026-W41", "td"), None);
    assert_eq!(parse_cup_id("cup-2026-W41-td"), None);
}

#[test]
fn ids_weeks_and_seasons_are_checked() {
    assert_eq!(parse_cup_id("cup-2026-W41-kart"), Some(("2026-W41".to_string(), "kart")));
    assert_eq!(parse_cup_id("cup-2026-W53-golf"), Some(("2026-W53".to_string(), "golf")));
    for bad in ["cup-2027-W53-golf", "cup-2026-W41", "cup-2026-W41-", "cup-2026-W41-chess",
                "cup-2026-41-kart", "x-2026-W41-kart", "cup-2026-W00-kart", "cup-２０２６-W41-kart"] {
        assert_eq!(parse_cup_id(bad), None, "{bad}");
    }
    assert!(valid_season("2026-09"));
    assert!(!valid_season("2026-13"));
    assert!(!valid_season("2026-9"));
    assert!(!valid_season("26-09-01"));
    assert_eq!(season_weeks("2026-09"), vec!["2026-W36", "2026-W37", "2026-W38", "2026-W39"]);
    assert_eq!(prev_season(NaiveDate::from_ymd_opt(2026, 1, 15).unwrap()), "2025-12");
    assert!(trophy_label("plat", "2026-W41", 3).chars().count() <= 48);
}

// --- the database --------------------------------------------------------

/// A file-backed SQLite (WAL, several connections) so concurrent readers are
/// real, never `sqlite::memory:`.
async fn pool(tag: &str) -> (SqlitePool, std::path::PathBuf) {
    let path = std::env::temp_dir().join(format!("arena-cups-{tag}-{}.db", uuid::Uuid::new_v4().simple()));
    let opts = SqliteConnectOptions::new()
        .filename(&path)
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .busy_timeout(std::time::Duration::from_secs(10));
    let pool = SqlitePoolOptions::new().max_connections(4).connect_with(opts).await.unwrap();
    crate::db::migrate(&pool).await.unwrap();
    for (i, (id, active)) in [("alice", 1), ("bob", 1), ("cara", 1), ("dan", 1), ("gone", 0)].iter().enumerate() {
        sqlx::query("INSERT INTO users (id, github_id, handle, display_name, avatar_url, trainer_name,
                     is_active, created_at) VALUES (?1, ?2, ?1, ?3, '', '', ?4, datetime('now'))")
            .bind(id).bind(i as i64 + 1).bind(id.to_uppercase()).bind(active)
            .execute(&pool).await.unwrap();
    }
    (pool, path)
}

fn drop_db(path: std::path::PathBuf) {
    for ext in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{ext}", path.display()));
    }
}

/// Write one race the way results.rs does, at a fixed timestamp. `None` place = DNF.
async fn seed(pool: &SqlitePool, game: &str, key: &str, at: &str, order: &[(&str, Option<i64>)]) {
    let n = order.len() as i64;
    for (u, place) in order {
        sqlx::query("INSERT INTO game_results (user_id, game, \"key\", mode, place, players, value, extra, at)
                     VALUES (?1, ?2, ?3, 'race', ?4, ?5, ?6, '{}', ?7)")
            .bind(u).bind(game).bind(key).bind(place.unwrap_or(n)).bind(n)
            .bind(place.map(|p| 60_000 + p)).bind(at)
            .execute(pool).await.unwrap();
    }
}

fn d(s: &str) -> NaiveDate {
    NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
}

async fn count(pool: &SqlitePool, sql: &str) -> i64 {
    sqlx::query_scalar(sql).fetch_one(pool).await.unwrap()
}

const W40: &str = "2026-W40"; // Mon 2026-09-28 .. Sun 2026-10-04

#[tokio::test]
async fn a_live_cup_reads_the_weeks_results_on_its_key_only() {
    let (p, path) = pool("live").await;
    let today = d("2026-10-10"); // 2026-W41
    let key = cup_key("2026-W41", "kart").unwrap();
    let other = candidates("kart").into_iter().find(|k| *k != key).unwrap();
    seed(&p, "kart", &key, "2026-10-06 10:00:00", &[("alice", Some(1)), ("bob", Some(2))]).await;
    seed(&p, "kart", &key, "2026-10-07 10:00:00", &[("bob", Some(1)), ("alice", Some(2)), ("cara", None)]).await;
    // Wrong key, last week, a solo run and a deactivated rival: none of them count.
    seed(&p, "kart", &other, "2026-10-06 11:00:00", &[("cara", Some(1)), ("dan", Some(2))]).await;
    seed(&p, "kart", &key, "2026-10-04 23:59:59", &[("dan", Some(1)), ("cara", Some(2))]).await;
    seed(&p, "kart", &key, "2026-10-08 10:00:00", &[("dan", Some(1))]).await;
    seed(&p, "kart", &key, "2026-10-08 11:00:00", &[("gone", Some(1)), ("dan", Some(2))]).await;

    let v = cups_view(&p, "alice", today).await.unwrap();
    assert_eq!(v.week, "2026-W41");
    assert_eq!(v.ends_at, "2026-10-11 23:59:59.999999");
    assert_eq!(v.season, "2026-10");
    assert_eq!(v.cups.len(), CUP_GAMES.len());
    let k = v.cups.iter().find(|c| c.game == "kart").unwrap();
    assert_eq!(k.status, "live");
    assert_eq!(k.key, key);
    let got: Vec<(&str, i64, i64)> = k.standings.iter().map(|s| (s.user_id.as_str(), s.points, s.place)).collect();
    // alice 10+7 = 17, bob 7+10 = 17 (tie: equal wins, alice's last score was earlier: Tue 2nd
    // vs bob's Wed win... both last scored on Wed; alice < bob by id), dan 7, cara 0.
    assert_eq!(got, vec![("alice", 17, 1), ("bob", 17, 2), ("dan", 7, 3), ("cara", 0, 4)]);
    assert!(k.you.as_ref().unwrap().is_you);
    let json = serde_json::to_value(&v).unwrap();
    assert!(json["cups"][0]["standings"][0].get("displayName").is_some());
    // The current week is never frozen.
    assert_eq!(count(&p, "SELECT COUNT(*) FROM cups WHERE week = '2026-W41'").await, 0);
    // Last week's cups were frozen by the same read.
    assert_eq!(count(&p, "SELECT COUNT(*) FROM cups WHERE week = '2026-W40'").await, CUP_GAMES.len() as i64);
    drop_db(path);
}

#[tokio::test]
async fn two_concurrent_reads_after_the_deadline_finalise_exactly_once() {
    let (p, path) = pool("race").await;
    let key = cup_key(W40, "golf").unwrap();
    seed(&p, "golf", &key, "2026-09-29 10:00:00", &[("alice", Some(1)), ("bob", Some(2)), ("cara", Some(3))]).await;
    seed(&p, "golf", &key, "2026-09-30 10:00:00", &[("bob", Some(1)), ("alice", Some(2))]).await;
    let (a, b) = tokio::join!(finalise_cup(&p, W40, "golf"), finalise_cup(&p, W40, "golf"));
    let (a, b) = (a.unwrap(), b.unwrap());
    assert!(a ^ b, "exactly one finalises: {a} {b}");
    assert_eq!(count(&p, "SELECT COUNT(*) FROM cups").await, 1);
    assert_eq!(count(&p, "SELECT COUNT(*) FROM cup_standings").await, 3);
    assert_eq!(count(&p, "SELECT COUNT(*) FROM cup_trophies").await, 3);
    // And reads racing through the whole read path do not double anything either.
    let today = d("2026-10-10");
    let (x, y, z) = tokio::join!(cups_view(&p, "alice", today), cups_view(&p, "bob", today),
                                 trophies_view(&p, "cara", today));
    x.unwrap(); y.unwrap(); z.unwrap();
    assert_eq!(count(&p, "SELECT COUNT(*) FROM cups WHERE week = '2026-W40'").await, CUP_GAMES.len() as i64);
    assert_eq!(count(&p, "SELECT COUNT(*) FROM cup_standings").await, 3);
    assert_eq!(count(&p, "SELECT COUNT(*) FROM cup_trophies").await, 3);
    // alice 10+7=17 (one win), bob 7+10=17 (one win), alice's last scoring row is
    // later (Wed) than... both scored last on Wed 30th: tie on time -> id order.
    let rows: Vec<(String, i64, i64)> = sqlx::query_as(
        "SELECT user_id, points, place FROM cup_standings ORDER BY place").fetch_all(&p).await.unwrap();
    assert_eq!(rows, vec![("alice".into(), 17, 1), ("bob".into(), 17, 2), ("cara".into(), 5, 3)]);
    let labels: Vec<String> = sqlx::query_scalar("SELECT label FROM cup_trophies ORDER BY place")
        .fetch_all(&p).await.unwrap();
    assert_eq!(labels[0], "Gold · Golf Cup · 2026-W40");
    let season: String = sqlx::query_scalar("SELECT season FROM cup_trophies LIMIT 1").fetch_one(&p).await.unwrap();
    assert_eq!(season, "2026-10");
    drop_db(path);
}

#[tokio::test]
async fn trophies_need_two_entrants() {
    let (p, path) = pool("lonely").await;
    let key = cup_key(W40, "plat").unwrap();
    // alice raced a guest the Arena does not know: two players, one entrant.
    seed(&p, "plat", &key, "2026-09-29 10:00:00", &[("alice", Some(1))]).await;
    sqlx::query("UPDATE game_results SET players = 2").execute(&p).await.unwrap();
    assert!(finalise_cup(&p, W40, "plat").await.unwrap());
    assert_eq!(count(&p, "SELECT COUNT(*) FROM cup_standings").await, 1);
    assert_eq!(count(&p, "SELECT COUNT(*) FROM cup_trophies").await, 0);
    // Co-op runs never count for the platformer cup.
    let key41 = cup_key("2026-W41", "plat").unwrap();
    seed(&p, "plat", &key41, "2026-10-06 10:00:00", &[("alice", Some(1)), ("bob", Some(2))]).await;
    sqlx::query("UPDATE game_results SET mode = 'coop' WHERE substr(at,1,10) = '2026-10-06'").execute(&p).await.unwrap();
    let v = cup_view(&p, "2026-W41", "plat", "alice", TOP_ONE, d("2026-10-10")).await.unwrap();
    assert_eq!(v.entrants, 0);
    drop_db(path);
}

#[tokio::test]
async fn a_past_cup_reads_frozen_and_trophies_show_on_the_card() {
    let (p, path) = pool("past").await;
    let key = cup_key(W40, "type").unwrap();
    seed(&p, "type", &key, "2026-10-01 10:00:00", &[("cara", Some(1)), ("dan", Some(2))]).await;
    let today = d("2026-10-10");
    let v = cup_view(&p, W40, "type", "dan", TOP_ONE, today).await.unwrap();
    assert_eq!(v.status, "final");
    assert_eq!(v.standings.len(), 2);
    assert_eq!(v.you.as_ref().map(|s| s.place), Some(2));
    // A frozen table does not move when a late row appears.
    seed(&p, "type", &key, "2026-10-02 10:00:00", &[("dan", Some(1)), ("cara", Some(2))]).await;
    let again = cup_view(&p, W40, "type", "dan", TOP_ONE, today).await.unwrap();
    assert_eq!(again.standings[0].user_id, "cara");
    assert_eq!(again.standings[0].points, 10);
    let t = trophies_view(&p, "cara", today).await.unwrap().unwrap();
    assert_eq!((t.gold, t.silver, t.bronze, t.seasons), (1, 0, 0, 0));
    assert_eq!(t.trophies[0].game.as_deref(), Some("type"));
    assert_eq!(t.trophies[0].kind, "cup");
    assert!(trophies_view(&p, "nobody", today).await.unwrap().is_none());
    assert!(trophies_view(&p, "gone", today).await.unwrap().is_none());
    drop_db(path);
}

#[tokio::test]
async fn the_season_podium_sums_cup_points_and_crowns_once() {
    let (p, path) = pool("season").await;
    // September 2026: weeks 36-39. kart in W36 and W37, golf in W38.
    let k36 = cup_key("2026-W36", "kart").unwrap();
    let k37 = cup_key("2026-W37", "kart").unwrap();
    let g38 = cup_key("2026-W38", "golf").unwrap();
    seed(&p, "kart", &k36, "2026-09-01 10:00:00", &[("alice", Some(1)), ("bob", Some(2)), ("cara", Some(3))]).await;
    seed(&p, "kart", &k37, "2026-09-08 10:00:00", &[("bob", Some(1)), ("alice", Some(2))]).await;
    seed(&p, "golf", &g38, "2026-09-15 10:00:00", &[("bob", Some(1)), ("dan", Some(2))]).await;
    // W40 ends on 2026-10-04: an October cup, not September.
    let k40 = cup_key(W40, "kart").unwrap();
    seed(&p, "kart", &k40, "2026-09-29 10:00:00", &[("cara", Some(1)), ("dan", Some(2))]).await;

    let today = d("2026-10-10");
    let s = season_view(&p, "2026-09", "alice", today).await.unwrap();
    assert_eq!(s.status, "final");
    let got: Vec<(&str, i64)> = s.standings.iter().map(|x| (x.user_id.as_str(), x.points)).collect();
    // bob 7+10+10 = 27, alice 10+7 = 17, dan 7, cara 5.
    assert_eq!(got, vec![("bob", 27), ("alice", 17), ("dan", 7), ("cara", 5)]);
    assert_eq!(s.podium.len(), 3);
    assert_eq!(s.you.as_ref().map(|y| y.place), Some(2));
    assert_eq!(s.cups, (4 * CUP_GAMES.len()) as i64);
    let crowns: Vec<(String, i64, String)> = sqlx::query_as(
        "SELECT user_id, place, label FROM cup_trophies WHERE cup_id = 'season-2026-09' ORDER BY place")
        .fetch_all(&p).await.unwrap();
    assert_eq!(crowns, vec![("bob".into(), 1, "Season champion".into()),
                            ("alice".into(), 2, "Season runner-up".into()),
                            ("dan".into(), 3, "Season third".into())]);
    // A second read changes nothing; and the GET /v1/cups catch-up awards it too.
    season_view(&p, "2026-09", "bob", today).await.unwrap();
    cups_view(&p, "bob", today).await.unwrap();
    assert_eq!(count(&p, "SELECT COUNT(*) FROM cup_trophies WHERE cup_id LIKE 'season-%'").await, 3);
    let t = trophies_view(&p, "bob", today).await.unwrap().unwrap();
    assert_eq!(t.seasons, 1);
    assert!(t.trophies.iter().any(|x| x.kind == "season" && x.game.is_none() && x.label == "Season champion"));

    // October is still running: live, no crowns, and it counts the W40 cup plus live W41.
    let oct = season_view(&p, "2026-10", "alice", today).await.unwrap();
    assert_eq!(oct.status, "live");
    assert_eq!(oct.standings[0].user_id, "cara");
    assert_eq!(count(&p, "SELECT COUNT(*) FROM cup_trophies WHERE cup_id = 'season-2026-10'").await, 0);
    drop_db(path);
}

#[tokio::test]
async fn a_season_with_one_scorer_crowns_nobody() {
    let (p, path) = pool("quiet").await;
    assert!(finalise_season(&p, "2026-09", d("2026-10-10")).await.unwrap());
    assert_eq!(count(&p, "SELECT COUNT(*) FROM cup_trophies").await, 0);
    assert_eq!(count(&p, "SELECT COUNT(*) FROM cups WHERE id LIKE 'cup-%'").await, (4 * CUP_GAMES.len()) as i64);
    // The marker row means the next read stops at one lookup.
    assert_eq!(count(&p, "SELECT COUNT(*) FROM cups WHERE id = 'season-2026-09'").await, 1);
    assert!(!finalise_season(&p, "2026-09", d("2026-10-10")).await.unwrap());
    // The current month, a future month and anything before the first season never freeze.
    assert!(!finalise_season(&p, "2026-10", d("2026-10-10")).await.unwrap());
    assert!(!finalise_season(&p, "2025-12", d("2026-10-10")).await.unwrap());
    assert!(!in_range("2025-W52"));
    assert!(in_range("2026-W01")); // ends Sun 2026-01-04
    assert!(recent_weeks(d("2026-01-10")).iter().all(|w| in_range(w)));
    drop_db(path);
}

#[tokio::test]
async fn two_concurrent_season_reads_crown_exactly_once() {
    let (p, path) = pool("crown").await;
    let k = cup_key("2026-W37", "kart").unwrap();
    seed(&p, "kart", &k, "2026-09-08 10:00:00", &[("alice", Some(1)), ("bob", Some(2))]).await;
    let today = d("2026-10-10");
    let (a, b) = tokio::join!(finalise_season(&p, "2026-09", today), finalise_season(&p, "2026-09", today));
    let (a, b) = (a.unwrap(), b.unwrap());
    assert!(a ^ b, "exactly one crowns: {a} {b}");
    assert_eq!(count(&p, "SELECT COUNT(*) FROM cup_trophies WHERE cup_id = 'season-2026-09'").await, 2);
    drop_db(path);
}
