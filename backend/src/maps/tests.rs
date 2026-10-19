//! Map gallery tests: CRUD, likes, reports and hiding, room scope, caps, body
//! validation, canonical keys, the weekly pick and paging.

use super::*;
use serde_json::json;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use std::str::FromStr;

const KART_JSON: &str = include_str!("../../data/kart_tracks.json");
const PLAT_JSON: &str = include_str!("../../data/platformer_levels.json");
const FPS_JSON: &str = include_str!("../../data/fps_map.json");

async fn pool() -> SqlitePool {
    let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
    crate::db::migrate(&pool).await.unwrap();
    seed_users(&pool).await;
    pool
}

/// A file-backed WAL database with several connections, for the race tests.
async fn file_pool(tag: &str) -> (SqlitePool, std::path::PathBuf) {
    let path = std::env::temp_dir().join(format!(
        "hq-maps-{tag}-{}-{}.db", std::process::id(), uuid::Uuid::new_v4().simple()));
    let opts = SqliteConnectOptions::from_str(&format!("sqlite://{}", path.display()))
        .unwrap()
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .busy_timeout(std::time::Duration::from_secs(10));
    let pool = SqlitePoolOptions::new().max_connections(6).connect_with(opts).await.unwrap();
    crate::db::migrate(&pool).await.unwrap();
    seed_users(&pool).await;
    (pool, path)
}

fn drop_file(path: &std::path::Path) {
    for ext in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{ext}", path.display()));
    }
}

const USERS: [&str; 8] = ["alice", "bob", "cara", "dave", "erin", "fred", "gail", "hank"];

async fn seed_users(pool: &SqlitePool) {
    for (i, id) in USERS.iter().enumerate() {
        sqlx::query("INSERT INTO users (id, github_id, handle, display_name, avatar_url,
                     trainer_name, is_active, created_at)
                     VALUES (?1, ?2, ?1, ?3, '', '', 1, datetime('now'))")
            .bind(id).bind(700 + i as i64).bind(id.to_uppercase())
            .execute(pool).await.unwrap();
    }
}

fn who(id: &str) -> Viewer {
    Viewer { user_id: id.into(), admin: false }
}

fn admin(id: &str) -> Viewer {
    Viewer { user_id: id.into(), admin: true }
}

fn kart_tiles(i: usize) -> String {
    let v: Value = serde_json::from_str(KART_JSON).unwrap();
    v["tracks"][i]["path"].as_str().unwrap().to_string()
}

fn kart_data(i: usize) -> Value {
    json!({"tiles": kart_tiles(i), "scenery": "forest",
           "theme": {"sky": "#9FD3F0", "fog": "#cfe8f2", "ground": "#76b85a"}})
}

fn body(kind: &str, name: &str, data: &Value, scope: &str, room: Option<&str>) -> Vec<u8> {
    let mut o = json!({"kind": kind, "name": name, "data": data, "scope": scope});
    if let Some(r) = room {
        o["roomId"] = json!(r);
    }
    serde_json::to_vec(&o).unwrap()
}

async fn make(pool: &SqlitePool, v: &Viewer, name: &str, scope: &str) -> MapCard {
    make_i(pool, v, name, scope, 0).await
}

async fn make_i(pool: &SqlitePool, v: &Viewer, name: &str, scope: &str, track: usize) -> MapCard {
    let b = parse_save(&body("kart", name, &kart_data(track), scope, None)).unwrap();
    save(pool, v, b).await.unwrap().map
}

fn q(pairs: &[(&str, &str)]) -> ListQ {
    let m: HashMap<String, String> = pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    parse_list_q(&m).unwrap()
}

fn ids(l: &ListView) -> Vec<String> {
    l.maps.iter().map(|m| m.id.clone()).collect()
}

// ---------------------------------------------------------------- crud --

#[tokio::test]
async fn create_read_update_list_and_delete() {
    let pool = pool().await;
    let (a, b) = (who("alice"), who("bob"));
    let m = make(&pool, &a, "  Loop One ", "private").await;
    assert!(is_map_id(&m.id));
    assert_eq!(m.name, "Loop One");
    assert!(m.mine && m.ckey.starts_with("c-"));
    assert_eq!(m.hidden, Some(false));
    // Stored canonical: the colour came in upper case and goes out lower.
    assert_eq!(m.data.as_ref().unwrap()["theme"]["sky"], "#9fd3f0");

    // Private: the owner reads it, nobody else even learns it exists.
    assert_eq!(one(&pool, &a, &m.id).await.unwrap().map.name, "Loop One");
    assert_eq!(one(&pool, &b, &m.id).await.unwrap_err().0, StatusCode::NOT_FOUND);
    assert!(list(&pool, &b, &q(&[])).await.unwrap().maps.is_empty());
    assert_eq!(ids(&list(&pool, &a, &q(&[("mine", "1")])).await.unwrap()), vec![m.id.clone()]);

    // Publish it under a new name: everyone sees it, without the hidden flag.
    let mut up = parse_save(&body("kart", "Loop Two", &kart_data(1), "public", None)).unwrap();
    up.id = Some(m.id.clone());
    let m2 = save(&pool, &a, up).await.unwrap().map;
    assert_eq!(m2.id, m.id);
    assert_ne!(m2.ckey, m.ckey, "new geometry, new key");
    let seen = list(&pool, &b, &q(&[("kind", "kart")])).await.unwrap();
    assert_eq!(ids(&seen), vec![m.id.clone()]);
    assert_eq!(seen.maps[0].hidden, None);
    assert!(!seen.maps[0].mine);
    assert!(seen.maps[0].data.is_none(), "lists carry no map data");
    assert!(list(&pool, &b, &q(&[("kind", "plat")])).await.unwrap().maps.is_empty());

    // Only the owner may change it.
    let mut steal = parse_save(&body("kart", "Mine now", &kart_data(0), "public", None)).unwrap();
    steal.id = Some(m.id.clone());
    assert_eq!(save(&pool, &b, steal).await.unwrap_err().0, StatusCode::FORBIDDEN);
    // ...and it can't change kind.
    let mut morph = parse_save(&body("fps", "Morph", &fps_data(), "public", None)).unwrap();
    morph.id = Some(m.id.clone());
    assert_eq!(save(&pool, &a, morph).await.unwrap_err().0, StatusCode::UNPROCESSABLE_ENTITY);

    // Delete: not by bob, yes by alice, and its likes and reports go too.
    like(&pool, &b, &m.id, true).await.unwrap();
    report(&pool, &who("cara"), &m.id, "other").await.unwrap();
    assert_eq!(delete(&pool, &b, &m.id).await.unwrap_err().0, StatusCode::FORBIDDEN);
    delete(&pool, &a, &m.id).await.unwrap();
    assert_eq!(one(&pool, &a, &m.id).await.unwrap_err().0, StatusCode::NOT_FOUND);
    assert_eq!(delete(&pool, &a, &m.id).await.unwrap_err().0, StatusCode::NOT_FOUND);
    for t in ["user_map_likes", "user_map_reports"] {
        let n: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {t}")).fetch_one(&pool).await.unwrap();
        assert_eq!(n, 0, "{t}");
    }
}

#[tokio::test]
async fn an_admin_may_delete_anyones_map() {
    let pool = pool().await;
    let m = make(&pool, &who("alice"), "Doomed", "public").await;
    delete(&pool, &admin("dave"), &m.id).await.unwrap();
    assert!(list(&pool, &who("bob"), &q(&[])).await.unwrap().maps.is_empty());
}

// -------------------------------------------------------------- likes --

#[tokio::test]
async fn likes_are_idempotent_and_never_on_your_own_map() {
    let pool = pool().await;
    let m = make(&pool, &who("alice"), "Likeable", "public").await;
    assert_eq!(like(&pool, &who("alice"), &m.id, true).await.unwrap_err().0, StatusCode::FORBIDDEN);
    let r = like(&pool, &who("bob"), &m.id, true).await.unwrap();
    assert_eq!((r.likes, r.liked), (1, true));
    let r = like(&pool, &who("bob"), &m.id, true).await.unwrap();
    assert_eq!((r.likes, r.liked), (1, true), "a second like is a no-op");
    like(&pool, &who("cara"), &m.id, true).await.unwrap();
    let seen = one(&pool, &who("bob"), &m.id).await.unwrap().map;
    assert_eq!((seen.likes, seen.liked), (2, true));
    assert!(!one(&pool, &who("erin"), &m.id).await.unwrap().map.liked);
    let r = like(&pool, &who("bob"), &m.id, false).await.unwrap();
    assert_eq!((r.likes, r.liked), (1, false));
    let r = like(&pool, &who("bob"), &m.id, false).await.unwrap();
    assert_eq!((r.likes, r.liked), (1, false), "a second unlike is a no-op");
    // You can't like what you can't see.
    let p = make(&pool, &who("alice"), "Secret", "private").await;
    assert_eq!(like(&pool, &who("bob"), &p.id, true).await.unwrap_err().0, StatusCode::NOT_FOUND);
    assert_eq!(like(&pool, &who("bob"), "m-zzz", true).await.unwrap_err().0, StatusCode::NOT_FOUND);
}

// ---------------------------------------------------- reports and hide --

#[tokio::test]
async fn three_reports_hide_a_map_but_its_owner_still_sees_it() {
    let pool = pool().await;
    let m = make(&pool, &who("alice"), "Reported", "public").await;
    assert_eq!(report(&pool, &who("alice"), &m.id, "spam").await.unwrap_err().0, StatusCode::FORBIDDEN);
    report(&pool, &who("bob"), &m.id, "spam").await.unwrap();
    report(&pool, &who("bob"), &m.id, "offensive").await.unwrap(); // one per user
    report(&pool, &who("cara"), &m.id, "broken").await.unwrap();
    assert_eq!(ids(&list(&pool, &who("erin"), &q(&[])).await.unwrap()), vec![m.id.clone()],
               "two distinct reports are not enough");
    report(&pool, &who("dave"), &m.id, "other").await.unwrap();
    assert!(list(&pool, &who("erin"), &q(&[])).await.unwrap().maps.is_empty());
    assert_eq!(one(&pool, &who("erin"), &m.id).await.unwrap_err().0, StatusCode::NOT_FOUND);
    let mine = list(&pool, &who("alice"), &q(&[("mine", "1")])).await.unwrap();
    assert_eq!(mine.maps[0].hidden, Some(true));
    assert_eq!(one(&pool, &who("alice"), &m.id).await.unwrap().map.hidden, Some(true));
    // An admin sees it (flagged) and can bring it back; more reports then don't re-hide it.
    assert_eq!(one(&pool, &admin("hank"), &m.id).await.unwrap().map.hidden, Some(true));
    hide(&pool, &admin("hank"), &m.id, false).await.unwrap();
    report(&pool, &who("fred"), &m.id, "spam").await.unwrap();
    assert_eq!(list(&pool, &who("erin"), &q(&[])).await.unwrap().maps.len(), 1);
    assert!(parse_reason(br#"{"reason":"meh"}"#).is_err());
    assert!(parse_reason(br#"{"reason":"spam","note":"x"}"#).is_err());
}

#[tokio::test]
async fn only_an_admin_may_hide() {
    let pool = pool().await;
    let m = make(&pool, &who("alice"), "Hideable", "public").await;
    assert_eq!(hide(&pool, &who("bob"), &m.id, true).await.unwrap_err().0, StatusCode::FORBIDDEN);
    assert_eq!(hide(&pool, &who("alice"), &m.id, true).await.unwrap_err().0, StatusCode::FORBIDDEN);
    let r = hide(&pool, &admin("dave"), &m.id, true).await.unwrap();
    assert!(r.hidden);
    assert!(list(&pool, &who("bob"), &q(&[])).await.unwrap().maps.is_empty());
    assert!(list(&pool, &admin("dave"), &q(&[])).await.unwrap().admin);
    hide(&pool, &admin("dave"), &m.id, false).await.unwrap();
    assert_eq!(list(&pool, &who("bob"), &q(&[])).await.unwrap().maps.len(), 1);
    assert_eq!(hide(&pool, &admin("dave"), "m-000000000000", true).await.unwrap_err().0,
               StatusCode::NOT_FOUND);
}

// --------------------------------------------------------- room scope --

async fn private_room(pool: &SqlitePool, room: &str, owner: &str, members: &[(&str, &str)]) {
    sqlx::query("INSERT INTO private_rooms (id, name, name_key, owner_user_id, password_hash)
                 VALUES (?1, ?1, ?1, ?2, 'x')")
        .bind(room).bind(owner).execute(pool).await.unwrap();
    for (u, role) in [(owner, "owner")].iter().chain(members.iter()) {
        sqlx::query("INSERT INTO private_room_members (id, room_id, user_id, role) VALUES (?1, ?2, ?3, ?4)")
            .bind(uuid::Uuid::new_v4().to_string()).bind(room).bind(u).bind(role)
            .execute(pool).await.unwrap();
    }
}

#[tokio::test]
async fn room_maps_are_for_that_rooms_people_only() {
    let pool = pool().await;
    let a = who("alice");
    // Alice shares to her own HQ: she sees it; a visitor to her open HQ gets 403.
    let b = parse_save(&body("kart", "Home Loop", &kart_data(0), "room", Some("hq_alice"))).unwrap();
    let m = save(&pool, &a, b).await.unwrap().map;
    assert_eq!(m.scope, "room");
    let mine = list(&pool, &a, &q(&[("room", "hq_alice")])).await.unwrap();
    assert_eq!(ids(&mine), vec![m.id.clone()]);
    assert_eq!(list(&pool, &who("bob"), &q(&[("room", "hq_alice")])).await.unwrap_err().0,
               StatusCode::FORBIDDEN);
    assert_eq!(one(&pool, &who("bob"), &m.id).await.unwrap_err().0, StatusCode::NOT_FOUND);
    // Room maps never show in the public gallery.
    assert!(list(&pool, &who("bob"), &q(&[])).await.unwrap().maps.is_empty());
    // Bob can't share into Alice's HQ.
    let b = parse_save(&body("kart", "Intruder", &kart_data(0), "room", Some("hq_alice"))).unwrap();
    assert_eq!(save(&pool, &who("bob"), b).await.unwrap_err().0, StatusCode::FORBIDDEN);
    // Quick Play and the city are no one's room.
    for r in ["qp_abc", "hq_city", "lobby"] {
        assert_eq!(list(&pool, &a, &q(&[("room", r)])).await.unwrap_err().0, StatusCode::FORBIDDEN, "{r}");
    }

    // A private room: owner and members share and see; banned and outsiders don't.
    private_room(&pool, "r_crew", "cara", &[("bob", "member"), ("erin", "banned")]).await;
    let b = parse_save(&body("kart", "Crew Loop", &kart_data(1), "room", Some("r_crew"))).unwrap();
    let c = save(&pool, &who("bob"), b).await.unwrap().map;
    for u in ["cara", "bob"] {
        assert_eq!(ids(&list(&pool, &who(u), &q(&[("room", "r_crew")])).await.unwrap()), vec![c.id.clone()]);
        assert!(one(&pool, &who(u), &c.id).await.is_ok());
    }
    for u in ["erin", "alice"] {
        assert_eq!(list(&pool, &who(u), &q(&[("room", "r_crew")])).await.unwrap_err().0,
                   StatusCode::FORBIDDEN, "{u}");
        assert_eq!(one(&pool, &who(u), &c.id).await.unwrap_err().0, StatusCode::NOT_FOUND, "{u}");
    }
    // roomId only with scope room.
    assert!(parse_save(&body("kart", "X", &kart_data(0), "public", Some("hq_alice"))).is_err());
    assert!(parse_save(&body("kart", "X", &kart_data(0), "room", None)).is_err());
}

// --------------------------------------------------------------- caps --

#[tokio::test]
async fn fifty_live_maps_then_a_delete_makes_room() {
    let pool = pool().await;
    let a = who("alice");
    // 49 old maps (made on earlier days), then today's 50th and 51st.
    for i in 0..49 {
        sqlx::query("INSERT INTO user_maps (id, owner_id, kind, name, data, ckey, scope)
                     VALUES (?1, 'alice', 'kart', 'old', '{}', 'c-000000000000', 'private')")
            .bind(format!("m-{i:012x}")).execute(&pool).await.unwrap();
    }
    make(&pool, &a, "Fiftieth", "private").await;
    let b = parse_save(&body("kart", "Fifty-first", &kart_data(0), "private", None)).unwrap();
    let e = save(&pool, &a, b.clone()).await.unwrap_err();
    assert_eq!(e.0, StatusCode::CONFLICT);
    // The refused create did not spend today's quota.
    let n: i64 = sqlx::query_scalar("SELECT n FROM map_publishes WHERE user_id = 'alice'")
        .fetch_one(&pool).await.unwrap();
    assert_eq!(n, 1);
    delete(&pool, &a, "m-000000000000").await.unwrap();
    assert!(save(&pool, &a, b).await.is_ok());
}

#[tokio::test]
async fn ten_publishes_a_day_and_deletes_dont_give_them_back() {
    let pool = pool().await;
    let a = who("alice");
    let mut made = Vec::new();
    for i in 0..10 {
        made.push(make(&pool, &a, &format!("Map {i}"), "private").await);
    }
    for m in &made {
        delete(&pool, &a, &m.id).await.unwrap();
    }
    let b = parse_save(&body("kart", "Eleventh", &kart_data(0), "private", None)).unwrap();
    assert_eq!(save(&pool, &a, b).await.unwrap_err().0, StatusCode::TOO_MANY_REQUESTS);
    // Bob's quota is his own.
    make(&pool, &who("bob"), "Bobs", "public").await;
}

#[tokio::test]
async fn editing_is_free_but_going_public_counts() {
    let pool = pool().await;
    let a = who("alice");
    let m = make(&pool, &a, "Draft", "private").await; // 1
    for i in 0..5 {
        let mut up = parse_save(&body("kart", &format!("Draft {i}"), &kart_data(i % 3), "private", None)).unwrap();
        up.id = Some(m.id.clone());
        save(&pool, &a, up).await.unwrap();
    }
    let mut up = parse_save(&body("kart", "Live", &kart_data(0), "public", None)).unwrap();
    up.id = Some(m.id.clone());
    save(&pool, &a, up.clone()).await.unwrap(); // 2
    save(&pool, &a, up).await.unwrap(); // already public: free
    let n: i64 = sqlx::query_scalar("SELECT n FROM map_publishes WHERE user_id = 'alice'")
        .fetch_one(&pool).await.unwrap();
    assert_eq!(n, 2);
}

// ------------------------------------------------------------- bodies --

#[test]
fn bodies_are_checked_strictly() {
    let ok = body("kart", "Fine", &kart_data(0), "public", None);
    assert!(parse_save(&ok).is_ok());
    // A top-level title (a privacy-forbidden key) is refused, as is any extra key.
    let mut o: Value = serde_json::from_slice(&ok).unwrap();
    o["title"] = json!("x");
    assert_eq!(parse_save(&serde_json::to_vec(&o).unwrap()).unwrap_err().0, StatusCode::UNPROCESSABLE_ENTITY);
    // 13 KiB of data is refused before it is even looked at.
    let mut d = kart_data(0);
    d["pad"] = json!("x".repeat(13 * 1024));
    let e = parse_save(&body("kart", "Big", &d, "public", None)).unwrap_err();
    assert_eq!(e.0, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(e.1.contains("12 KiB"));
    // Names.
    for n in ["", "   ", "<b>hi</b>", "see http://x", "HTTPS fun", "www.example", "tab\there",
              "a name that is far too long to keep ok"] {
        assert!(parse_save(&body("kart", n, &kart_data(0), "public", None)).is_err(), "{n:?}");
    }
    assert!(parse_save(&body("kart", "Ünïcode ok — 32 chars max fine!", &kart_data(0), "public", None)).is_ok());
    // Kinds, scopes, ids and data shapes.
    assert!(parse_save(&body("golf", "X", &kart_data(0), "public", None)).is_err());
    assert!(parse_save(&body("kart", "X", &kart_data(0), "friends", None)).is_err());
    assert!(parse_save(&body("kart", "X", &json!([1, 2]), "public", None)).is_err());
    let mut o: Value = serde_json::from_slice(&ok).unwrap();
    o["id"] = json!("m-NOTHEX000000");
    assert!(parse_save(&serde_json::to_vec(&o).unwrap()).is_err());
    assert!(parse_save(b"[1]").is_err());
    assert!(parse_save(b"nope").is_err());
    // Broken geometry gets the game's reason.
    let mut d = kart_data(0);
    d["tiles"] = json!("FSSSSSSSS");
    assert!(parse_save(&body("kart", "Open", &d, "public", None)).unwrap_err().1.starts_with("bad map"));
    assert!(parse_flag(br#"{"on":1}"#, "on").is_err());
    assert!(parse_flag(br#"{"on":true}"#, "on").unwrap());
    assert!(parse_empty(b"").is_ok() && parse_empty(b"{}").is_ok() && parse_empty(br#"{"x":1}"#).is_err());
}

#[test]
fn reordered_keys_give_the_same_key() {
    let a = json!({"tiles": kart_tiles(2), "scenery": "tents",
                   "theme": {"sky": "#AABBCC", "fog": "#ddeeff", "ground": "#112233"}});
    let b: Value = serde_json::from_str(&format!(
        r##"{{"theme": {{"ground": "#112233", "fog": "#DDEEFF", "sky": "#aabbcc"}}, "junk": 1,
             "scenery": "tents", "tiles": "{}"}}"##, kart_tiles(2))).unwrap();
    let (ca, cb) = (canonical("kart", &a).unwrap(), canonical("kart", &b).unwrap());
    assert_eq!(serde_json::to_string(&ca).unwrap(), serde_json::to_string(&cb).unwrap());
    assert_eq!(crate::mapkey::content_key("kart", &ca), crate::mapkey::content_key("kart", &cb));
    // Canonical is a fixed point: a gallery round trip never forks a board.
    assert_eq!(canonical("kart", &ca).unwrap(), ca);
}

/// The built-in Blaster map as a user map. Its perimeter walls sit one unit
/// outside its play bounds, which the referee's validator (rightly) refuses
/// for user maps, so the bounds are widened to enclose them.
fn fps_data() -> Value {
    let mut v: Value = serde_json::from_str(FPS_JSON).unwrap();
    v["bounds"] = json!([-23, -1, -23, 23, 12, 23]);
    // User-map pickups are named p1..p16 (the built-in map uses descriptive ids).
    if let Some(ps) = v["pickups"].as_array_mut() {
        for (i, p) in ps.iter_mut().enumerate() {
            p["id"] = json!(format!("p{}", i + 1));
        }
    }
    v
}

#[test]
fn every_built_in_map_passes_and_canonicalises_to_a_fixed_point() {
    let k: Value = serde_json::from_str(KART_JSON).unwrap();
    for t in k["tracks"].as_array().unwrap() {
        let d = json!({"tiles": t["path"], "scenery": t["scenery"], "theme": {
            "sky": t["theme"]["sky"], "fog": t["theme"]["fog"], "ground": t["theme"]["ground"]}});
        let c = canonical("kart", &d).unwrap_or_else(|e| panic!("{}: {e}", t["id"]));
        assert_eq!(canonical("kart", &c).unwrap(), c);
    }
    let p: Value = serde_json::from_str(PLAT_JSON).unwrap();
    for l in p["levels"].as_array().unwrap() {
        let c = canonical("plat", l).unwrap_or_else(|e| panic!("{}: {e}", l["id"]));
        assert!(c.get("id").is_none() && c.get("name").is_none(), "unknown keys are dropped");
        assert_eq!(canonical("plat", &c).unwrap(), c);
        let keys: Vec<&String> = c.as_object().unwrap().keys().collect();
        assert_eq!(keys, ["kill", "coopGoal", "coopSecs", "theme", "spawns", "cps", "flag", "coins",
                          "solids", "route", "deco"]);
    }
    let c = canonical("fps", &fps_data()).unwrap();
    assert_eq!(canonical("fps", &c).unwrap(), c);
    let keys: Vec<&String> = c.as_object().unwrap().keys().collect();
    assert_eq!(keys, ["bounds", "theme", "boxes", "spawns", "pickups"]);
    assert!(serde_json::to_string(&c).unwrap().len() <= MAX_DATA);
}

#[test]
fn validators_refuse_bad_shapes_without_panicking() {
    let mut f = fps_data();
    f["spawns"] = json!(f["spawns"].as_array().unwrap()[..10]);
    assert!(canonical("fps", &f).is_err(), "10 spawns repeat under k*5 % n");
    let mut f = fps_data();
    f["boxes"][0] = json!([0, 0, 0, 0, 1, 1, "wall"]);
    assert!(canonical("fps", &f).is_err());
    let mut f = fps_data();
    f["boxes"][0][6] = json!("lava");
    assert!(canonical("fps", &f).is_err());
    let p: Value = serde_json::from_str(PLAT_JSON).unwrap();
    let mut l = p["levels"][0].clone();
    l["solids"][0]["m"] = json!("castle");
    assert!(canonical("plat", &l).is_err());
    let mut l = p["levels"][0].clone();
    l["kill"] = json!(1e9);
    assert!(canonical("plat", &l).is_err());
    let mut l = p["levels"][0].clone();
    l["coopGoal"] = json!(999);
    assert!(canonical("plat", &l).is_err());
    for junk in [json!(null), json!(1), json!("x"), json!({}), json!({"tiles": 5})] {
        for k in KINDS {
            assert!(canonical(k, &junk).is_err());
        }
    }
}

// ----------------------------------------------------------- featured --

async fn at(pool: &SqlitePool, id: &str, ts: &str) {
    sqlx::query("UPDATE user_maps SET created_at = ?2 WHERE id = ?1").bind(id).bind(ts)
        .execute(pool).await.unwrap();
}

async fn like_at(pool: &SqlitePool, map: &str, user: &str, ts: &str) {
    sqlx::query("INSERT INTO user_map_likes (map_id, user_id, at) VALUES (?1, ?2, ?3)")
        .bind(map).bind(user).bind(ts).execute(pool).await.unwrap();
    sqlx::query("UPDATE user_maps SET likes = likes + 1 WHERE id = ?1").bind(map)
        .execute(pool).await.unwrap();
}

async fn raced(pool: &SqlitePool, game: &str, key: &str, user: &str, ts: &str) {
    sqlx::query("INSERT INTO game_results (user_id, game, \"key\", mode, place, players, value, extra, at)
                 VALUES (?1, ?2, ?3, 'race', 1, 2, 1000, '{}', ?4)")
        .bind(user).bind(game).bind(key).bind(ts).execute(pool).await.unwrap();
}

#[tokio::test]
async fn the_weekly_pick_counts_likes_thrice_and_racers_once() {
    let pool = pool().await;
    // Now: Wednesday of 2026-W41 (Mon 10-05 .. Sun 10-11). Last week: W40.
    let now = "2026-10-07 12:00:00";
    let x = make_i(&pool, &who("alice"), "Liked", "public", 0).await;
    let y = make_i(&pool, &who("bob"), "Raced", "public", 1).await;
    let z = make_i(&pool, &who("cara"), "Hidden", "public", 2).await;
    at(&pool, &x.id, "2026-09-01 00:00:00").await;
    at(&pool, &y.id, "2026-09-02 00:00:00").await;
    // x: 1 like this week (3) + 1 like last week. y: 4 racers this week (4), one twice.
    like_at(&pool, &x.id, "erin", "2026-10-05 00:00:00").await;
    like_at(&pool, &x.id, "fred", "2026-10-04 23:59:59").await;
    for u in ["alice", "cara", "dave", "erin", "erin"] {
        raced(&pool, "kart", &y.ckey, u, "2026-10-06 10:00:00").await;
    }
    // A plat result on the same key doesn't count for a kart map; z is hidden.
    raced(&pool, "plat", &x.ckey, "gail", "2026-10-06 10:00:00").await;
    for u in ["erin", "fred", "gail", "hank"] {
        like_at(&pool, &z.id, u, "2026-10-06 00:00:00").await;
    }
    hide(&pool, &admin("hank"), &z.id, true).await.unwrap();

    let f = featured(&pool, &who("hank"), now).await.unwrap();
    assert_eq!(f.week, "2026-W41");
    let k = f.this_week.kart.as_ref().unwrap();
    assert_eq!((k.id.as_str(), k.score), (y.id.as_str(), Some(4)));
    assert!(f.this_week.plat.is_none() && f.this_week.fps.is_none());
    // Last week: only x had anything (one like = 3).
    let lw = f.last_week.kart.as_ref().unwrap();
    assert_eq!((lw.id.as_str(), lw.score), (x.id.as_str(), Some(3)));
    // A tie goes to the older map: give x one more like this week -> 6? no: make it 4+... tie at 6.
    like_at(&pool, &x.id, "gail", "2026-10-06 00:00:00").await; // x = 6
    raced(&pool, "kart", &y.ckey, "fred", "2026-10-06 11:00:00").await;
    raced(&pool, "kart", &y.ckey, "gail", "2026-10-06 11:00:00").await; // y = 6
    let f = featured(&pool, &who("hank"), now).await.unwrap();
    assert_eq!(f.this_week.kart.as_ref().unwrap().id, x.id, "tie: the earlier map");

    // Last week is frozen on first read: new old-week likes for y don't move it.
    for u in ["cara", "dave", "erin", "gail"] {
        like_at(&pool, &y.id, u, "2026-10-01 00:00:00").await;
    }
    let f = featured(&pool, &who("hank"), now).await.unwrap();
    assert_eq!(f.last_week.kart.as_ref().unwrap().id, x.id);
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM map_features").fetch_one(&pool).await.unwrap();
    assert_eq!(n, 1, "one frozen row (kart); empty kinds store nothing");

    // sort=week orders by this week's score.
    let l = list(&pool, &who("hank"), &q(&[("sort", "week")])).await.unwrap();
    assert_eq!(ids(&l)[..2], [x.id.clone(), y.id.clone()]);
    // races counts distinct racers on the key, all time.
    assert_eq!(one(&pool, &who("hank"), &y.id).await.unwrap().map.races, 6);
}

#[tokio::test]
async fn a_frozen_pick_that_was_deleted_shows_nothing() {
    let pool = pool().await;
    let x = make(&pool, &who("alice"), "Gone", "public").await;
    like_at(&pool, &x.id, "bob", "2026-10-01 00:00:00").await;
    let f = featured(&pool, &who("bob"), "2026-10-07 00:00:00").await.unwrap();
    assert!(f.last_week.kart.is_some());
    delete(&pool, &who("alice"), &x.id).await.unwrap();
    let f = featured(&pool, &who("bob"), "2026-10-07 00:00:00").await.unwrap();
    assert!(f.last_week.kart.is_none());
}

// ------------------------------------------------------------- paging --

#[tokio::test]
async fn cursor_pages_are_stable_and_complete() {
    let pool = pool().await;
    let mut made = Vec::new();
    for i in 0..7 {
        let m = make_i(&pool, &who(USERS[i % 3]), &format!("Map {i}"), "public", i % 3).await;
        // Two pairs share a timestamp, so the id tiebreak is exercised.
        at(&pool, &m.id, &format!("2026-10-0{} 00:00:00", 1 + i / 2)).await;
        made.push(m);
    }
    for (i, m) in made.iter().enumerate() {
        for u in USERS.iter().skip(3).take(i % 3) {
            like(&pool, &who(u), &m.id, true).await.unwrap();
        }
    }
    for sort in ["new", "top", "week"] {
        let full = ids(&list(&pool, &who("hank"), &q(&[("sort", sort), ("limit", "30")])).await.unwrap());
        assert_eq!(full.len(), 7);
        let mut paged = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let mut p = vec![("sort", sort), ("limit", "3")];
            if let Some(c) = cursor.as_deref() {
                p.push(("cursor", c));
            }
            let page = list(&pool, &who("hank"), &q(&p)).await.unwrap();
            paged.extend(ids(&page));
            match page.next {
                Some(c) => cursor = Some(c),
                None => break,
            }
        }
        assert_eq!(paged, full, "{sort}");
    }
    // "new" is newest first.
    let l = list(&pool, &who("hank"), &q(&[])).await.unwrap();
    assert!(l.maps.windows(2).all(|w| w[0].created_at >= w[1].created_at));
    let m: HashMap<String, String> = [("cursor".to_string(), "zz".to_string())].into();
    assert!(parse_list_q(&m).is_err());
    let m: HashMap<String, String> = [("limit".to_string(), "31".to_string())].into();
    assert!(parse_list_q(&m).is_err());
    let m: HashMap<String, String> = [("bogus".to_string(), "1".to_string())].into();
    assert!(parse_list_q(&m).is_err());
    // ckey filter.
    let k = list(&pool, &who("hank"), &q(&[("ckey", &made[0].ckey)])).await.unwrap();
    assert!(k.maps.iter().all(|c| c.ckey == made[0].ckey) && k.maps.len() == 3);
}

// -------------------------------------------------------------- races --

#[tokio::test]
async fn racing_likes_and_reports_count_each_person_once() {
    let (pool, path) = file_pool("likes").await;
    let m = make(&pool, &who("alice"), "Popular", "public").await;
    let mut set = tokio::task::JoinSet::new();
    for u in USERS.iter().skip(1) {
        for _ in 0..3 {
            let (pool, id, u) = (pool.clone(), m.id.clone(), u.to_string());
            set.spawn(async move { like(&pool, &who(&u), &id, true).await.map(|_| ()) });
        }
    }
    while let Some(r) = set.join_next().await {
        r.unwrap().unwrap();
    }
    // Three reporters, each three times at once: exactly three reports land and
    // the map hides once; a repeat that arrives after the hide finds no map.
    for u in ["bob", "cara", "dave"] {
        for _ in 0..3 {
            let (pool, id, u) = (pool.clone(), m.id.clone(), u.to_string());
            set.spawn(async move { report(&pool, &who(&u), &id, "spam").await.map(|_| ()) });
        }
    }
    while let Some(r) = set.join_next().await {
        if let Err(e) = r.unwrap() {
            assert_eq!(e.0, StatusCode::NOT_FOUND);
        }
    }
    let (likes, reports, hidden): (i64, i64, i64) =
        sqlx::query_as("SELECT likes, reports, hidden FROM user_maps WHERE id = ?1")
            .bind(&m.id).fetch_one(&pool).await.unwrap();
    assert_eq!((likes, reports, hidden), (7, 3, 1));
    pool.close().await;
    drop_file(&path);
}

#[tokio::test]
async fn racing_creates_never_pass_the_daily_cap() {
    let (pool, path) = file_pool("caps").await;
    let (r1, r2, r3, r4) = tokio::join!(
        async { let mut n = 0; for i in 0..4 { if try_make(&pool, i).await { n += 1 } } n },
        async { let mut n = 0; for i in 4..8 { if try_make(&pool, i).await { n += 1 } } n },
        async { let mut n = 0; for i in 8..12 { if try_make(&pool, i).await { n += 1 } } n },
        async { let mut n = 0; for i in 12..16 { if try_make(&pool, i).await { n += 1 } } n },
    );
    assert_eq!(r1 + r2 + r3 + r4, PUBLISH_PER_DAY);
    let live: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM user_maps").fetch_one(&pool).await.unwrap();
    assert_eq!(live, PUBLISH_PER_DAY);
    pool.close().await;
    drop_file(&path);
}

async fn try_make(pool: &SqlitePool, i: usize) -> bool {
    let b = parse_save(&body("kart", &format!("Racer {i}"), &kart_data(i % 3), "public", None)).unwrap();
    match save(pool, &who("alice"), b).await {
        Ok(_) => true,
        Err(e) => {
            assert_eq!(e.0, StatusCode::TOO_MANY_REQUESTS, "{e:?}");
            false
        }
    }
}

// -------------------------------------------------------------- misc --

#[test]
fn ids_cursors_and_weeks() {
    assert!(is_map_id("m-0123456789ab"));
    for bad in ["m-0123456789AB", "m-0123", "x-0123456789ab", "m-0123456789abc", ""] {
        assert!(!is_map_id(bad), "{bad}");
    }
    let c = encode_cursor(5, "2026-10-01 00:00:00", "m-0123456789ab");
    assert_eq!(decode_cursor(&c), Some((5, "2026-10-01 00:00:00".into(), "m-0123456789ab".into())));
    assert_eq!(decode_cursor(&hex::encode("[1,\"x\",\"nope\"]")), None);
    assert_eq!(decode_cursor("not hex"), None);
    assert_eq!(prev_week("2026-W41"), "2026-W40");
    assert_eq!(prev_week("2027-W01"), "2026-W53");
    assert!(new_id().len() == 14 && is_map_id(&new_id()));
}
