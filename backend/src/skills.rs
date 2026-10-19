//! HQ 2.5 skill tree: perks and titles earned from how you work.
//!
//!   POST /v1/skills/report   {day, counts:{cat:n}}   one UTC day's category counts
//!   GET  /v1/skills/me       your tree: points, tiers, perks, titles, worn title
//!   GET  /v1/skills/:user_id the public view: tiers and title only
//!   POST /v1/skills/title    {title: id | null}      wear an unlocked title
//!
//! The HQ classifies the user's own transcripts on their machine (tool use and
//! file kinds) and, only when they opted in (`workSignals`), reports SEVEN
//! integers per UTC day. Nothing else reaches this module: no tool names,
//! commands, file names, paths, repos or text. The server cannot verify the
//! counts, so this is trust-the-client within caps: a day is at most 7 x 200
//! points, a report can only raise a day's count (MAX), and only the last week
//! can be reported, so a replayed or reordered report never inflates anything.
//!
//! The public view carries tiers and the worn title, never counts or days.

use crate::{err, AppState, Caller};
use axum::{
    extract::{Path, Request, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use chrono::{NaiveDate, Utc};
use serde::Serialize;
use serde_json::{Map, Value};
use sqlx::{Row, SqlitePool};
use std::collections::HashMap;

/// The seven categories, in display order. A report with any other key is a 422.
pub const CATS: [&str; 7] = ["tests", "refactor", "docs", "review", "debug", "explore", "build"];

/// Cumulative points needed for tiers 1..=5.
pub const TIERS: [i64; 5] = [10, 50, 150, 400, 1000];

/// Largest count one category may report for one day.
pub const MAX_N: i64 = 200;

/// How far back a day may be (in whole UTC days). The HQ sends the last seven
/// (today and six before); one more day of slack keeps a report built at
/// 23:59:59 UTC valid when it lands after midnight.
pub const MAX_AGE_DAYS: i64 = 7;

/// Perk names per category, tiers 1..=5. Tiers 3 and 5 are also titles. Ids are
/// `<cat>-<tier>`. ui/app/35-skills.js mirrors this table (a test keeps them in step).
pub const PERKS: [(&str, [&str; 5]); 7] = [
    ("tests", ["Smoke Tester", "Assertive", "Test Pilot", "Coverage Hound", "Green Machine"]),
    ("refactor", ["Tidy Up", "Renamer", "Untangler", "Pattern Weaver", "Architect"]),
    ("docs", ["Note Taker", "Readme Writer", "Scribe", "Chronicler", "Loremaster"]),
    ("review", ["Skimmer", "Nitpicker", "Second Pair of Eyes", "Sharp Eye", "Gatekeeper"]),
    ("debug", ["Bug Spotter", "Stack Reader", "Bug Hunter", "Root Causer", "Exterminator"]),
    ("explore", ["Wanderer", "Pathfinder", "Scout", "Surveyor", "Cartographer"]),
    ("build", ["Tinkerer", "Builder", "Maker", "Engineer", "Forgemaster"]),
];

/// The tiers whose perk is also a title you can wear.
pub const TITLE_TIERS: [i64; 2] = [3, 5];

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/skills/report", post(report))
        .route("/v1/skills/me", get(me))
        .route("/v1/skills/title", post(set_title))
        .route("/v1/skills/:user_id", get(public_view))
}

// --- pure rules -------------------------------------------------------------

/// Tier 0..=5 for a cumulative point total.
pub fn tier_for(points: i64) -> i64 {
    TIERS.iter().filter(|t| points >= **t).count() as i64
}

/// Points needed for the next tier, or None at tier 5.
pub fn next_for(points: i64) -> Option<i64> {
    TIERS.iter().copied().find(|t| points < *t)
}

fn perk_name(cat: &str, tier: i64) -> Option<&'static str> {
    if !(1..=5).contains(&tier) {
        return None;
    }
    PERKS.iter().find(|(c, _)| *c == cat).map(|(_, names)| names[(tier - 1) as usize])
}

/// Title ids unlocked by these per-category points, in catalogue order.
pub fn unlocked_titles(points: &HashMap<String, i64>) -> Vec<String> {
    let mut out = Vec::new();
    for cat in CATS {
        let t = tier_for(points.get(cat).copied().unwrap_or(0));
        for tt in TITLE_TIERS {
            if t >= tt {
                out.push(format!("{cat}-{tt}"));
            }
        }
    }
    out
}

/// True for a well-formed title id (`<cat>-3` or `<cat>-5`), unlocked or not.
pub fn is_title_id(id: &str) -> bool {
    let Some((cat, tier)) = id.rsplit_once('-') else { return false };
    CATS.contains(&cat) && TITLE_TIERS.iter().any(|t| t.to_string() == tier)
}

/// A strict YYYY-MM-DD that is a real date.
fn parse_day(s: &str) -> Option<NaiveDate> {
    if s.len() != 10 {
        return None;
    }
    NaiveDate::parse_from_str(s, "%Y-%m-%d").ok().filter(|d| d.format("%Y-%m-%d").to_string() == s)
}

/// A validated report: the day plus (cat, n) pairs in CATS order.
#[derive(Debug, PartialEq)]
pub struct Report {
    pub day: String,
    pub counts: Vec<(&'static str, i64)>,
}

/// Parse and validate a report body against `today` (UTC). Every failure is a
/// 422 message: unknown or extra keys, a bad day, a day older than
/// MAX_AGE_DAYS or in the future, an unknown category, or n outside 0..=200.
pub fn parse_report(bytes: &[u8], today: NaiveDate) -> Result<Report, String> {
    let obj: Map<String, Value> = match serde_json::from_slice::<Value>(bytes) {
        Ok(Value::Object(o)) => o,
        Ok(_) => return Err("body must be an object".into()),
        Err(_) => return Err("invalid JSON".into()),
    };
    if let Some(k) = obj.keys().find(|k| *k != "day" && *k != "counts") {
        return Err(format!("unknown field: {}", k.chars().take(24).collect::<String>()));
    }
    let day_s = match obj.get("day") {
        Some(Value::String(s)) => s.as_str(),
        Some(_) => return Err("day must be a string".into()),
        None => return Err("day is required".into()),
    };
    let day = parse_day(day_s).ok_or("day must be YYYY-MM-DD")?;
    let age = (today - day).num_days();
    if age < 0 {
        return Err("day is in the future".into());
    }
    if age > MAX_AGE_DAYS {
        return Err("day is too old".into());
    }
    let counts = match obj.get("counts") {
        Some(Value::Object(c)) => c,
        Some(_) => return Err("counts must be an object".into()),
        None => return Err("counts is required".into()),
    };
    if counts.len() > CATS.len() {
        return Err("too many categories".into());
    }
    let mut out: Vec<(&'static str, i64)> = Vec::new();
    for cat in CATS {
        if let Some(v) = counts.get(cat) {
            let n = v.as_i64().filter(|n| (0..=MAX_N).contains(n)).ok_or("counts must be integers 0..200")?;
            out.push((cat, n));
        }
    }
    if out.len() != counts.len() {
        return Err("unknown category".into());
    }
    Ok(Report { day: day_s.to_string(), counts: out })
}

/// `{title: id | null}`: Ok(None) clears the title.
pub fn parse_title(bytes: &[u8]) -> Result<Option<String>, String> {
    let obj: Map<String, Value> = match serde_json::from_slice::<Value>(bytes) {
        Ok(Value::Object(o)) => o,
        Ok(_) => return Err("body must be an object".into()),
        Err(_) => return Err("invalid JSON".into()),
    };
    if obj.keys().any(|k| k != "title") {
        return Err("unknown field".into());
    }
    match obj.get("title") {
        None => Err("title is required".into()),
        Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if s.len() <= 24 && is_title_id(s) => Ok(Some(s.clone())),
        Some(_) => Err("unknown title".into()),
    }
}

// --- storage ----------------------------------------------------------------

/// Upsert one report: each (day, cat) keeps the larger of the stored and the
/// reported count, so the call is idempotent and monotone.
pub async fn store_report(pool: &SqlitePool, user_id: &str, r: &Report) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;
    for (cat, n) in &r.counts {
        sqlx::query(
            "INSERT INTO skill_days (user_id, day, cat, n) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(user_id, day, cat) DO UPDATE SET n = MAX(skill_days.n, excluded.n)",
        )
        .bind(user_id)
        .bind(&r.day)
        .bind(*cat)
        .bind(*n)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await
}

/// Cumulative points per category (every category present, 0 when none).
pub async fn points(pool: &SqlitePool, user_id: &str) -> Result<HashMap<String, i64>, sqlx::Error> {
    let rows = sqlx::query("SELECT cat, SUM(n) AS p FROM skill_days WHERE user_id = ?1 GROUP BY cat")
        .bind(user_id)
        .fetch_all(pool)
        .await?;
    let mut out: HashMap<String, i64> = CATS.iter().map(|c| (c.to_string(), 0)).collect();
    for r in rows {
        let cat: String = r.get("cat");
        let p: i64 = r.try_get("p").unwrap_or(0);
        if let Some(v) = out.get_mut(&cat) {
            *v = p.max(0);
        }
    }
    Ok(out)
}

/// The worn title, only while it is still unlocked.
async fn worn_title(pool: &SqlitePool, user_id: &str, unlocked: &[String]) -> Result<Option<String>, sqlx::Error> {
    let t: Option<String> = sqlx::query("SELECT title FROM skill_titles WHERE user_id = ?1")
        .bind(user_id)
        .fetch_optional(pool)
        .await?
        .map(|r| r.get("title"));
    Ok(t.filter(|t| unlocked.contains(t)))
}

// --- wire shapes ---------------------------------------------------------------

#[derive(Serialize, Debug)]
pub struct CatView {
    pub points: i64,
    pub tier: i64,
    pub next: Option<i64>,
}

#[derive(Serialize, Debug)]
pub struct Perk {
    pub id: String,
    pub name: &'static str,
    pub cat: &'static str,
    pub tier: i64,
}

#[derive(Serialize, Debug)]
pub struct MeView {
    pub cats: Map<String, Value>,
    pub perks: Vec<Perk>,
    pub titles: Vec<String>,
    pub title: Option<String>,
}

#[derive(Serialize, Debug)]
pub struct PublicTier {
    pub tier: i64,
}

#[derive(Serialize, Debug)]
pub struct PublicView {
    pub cats: Map<String, Value>,
    pub title: Option<String>,
}

pub async fn me_view(pool: &SqlitePool, user_id: &str) -> Result<MeView, sqlx::Error> {
    let pts = points(pool, user_id).await?;
    let titles = unlocked_titles(&pts);
    let title = worn_title(pool, user_id, &titles).await?;
    let mut cats = Map::new();
    let mut perks = Vec::new();
    for cat in CATS {
        let p = pts.get(cat).copied().unwrap_or(0);
        let tier = tier_for(p);
        let v = CatView { points: p, tier, next: next_for(p) };
        cats.insert(cat.to_string(), serde_json::to_value(v).unwrap_or(Value::Null));
        for t in 1..=tier {
            if let Some(name) = perk_name(cat, t) {
                perks.push(Perk { id: format!("{cat}-{t}"), name, cat, tier: t });
            }
        }
    }
    Ok(MeView { cats, perks, titles, title })
}

/// Tiers and title only: no points, counts or days. None for an unknown or
/// inactive user.
pub async fn public_of(pool: &SqlitePool, user_id: &str) -> Result<Option<PublicView>, sqlx::Error> {
    let found = sqlx::query("SELECT 1 FROM users WHERE id = ?1 AND is_active = 1")
        .bind(user_id)
        .fetch_optional(pool)
        .await?;
    if found.is_none() {
        return Ok(None);
    }
    let pts = points(pool, user_id).await?;
    let titles = unlocked_titles(&pts);
    let title = worn_title(pool, user_id, &titles).await?;
    let mut cats = Map::new();
    for cat in CATS {
        let tier = tier_for(pts.get(cat).copied().unwrap_or(0));
        cats.insert(cat.to_string(), serde_json::to_value(PublicTier { tier }).unwrap_or(Value::Null));
    }
    Ok(Some(PublicView { cats, title }))
}

/// Wear (Some) or clear (None) a title. Err(msg) when it is not unlocked.
pub async fn wear(pool: &SqlitePool, user_id: &str, title: Option<&str>) -> Result<Result<(), &'static str>, sqlx::Error> {
    match title {
        None => {
            sqlx::query("DELETE FROM skill_titles WHERE user_id = ?1").bind(user_id).execute(pool).await?;
            Ok(Ok(()))
        }
        Some(t) => {
            let pts = points(pool, user_id).await?;
            if !unlocked_titles(&pts).iter().any(|u| u == t) {
                return Ok(Err("that title is not unlocked yet"));
            }
            let now = Utc::now().format("%Y-%m-%d %H:%M:%S%.6f").to_string();
            sqlx::query(
                "INSERT INTO skill_titles (user_id, title, at) VALUES (?1, ?2, ?3)
                 ON CONFLICT(user_id) DO UPDATE SET title = excluded.title, at = excluded.at",
            )
            .bind(user_id)
            .bind(t)
            .bind(now)
            .execute(pool)
            .await?;
            Ok(Ok(()))
        }
    }
}

// --- handlers -----------------------------------------------------------------

#[derive(Serialize)]
struct Ok_ {
    ok: bool,
}

#[derive(Serialize)]
struct TitleOk {
    ok: bool,
    title: Option<String>,
}

fn caller(req: &Request) -> Caller {
    req.extensions().get::<Caller>().expect("caller set by middleware").clone()
}

/// Reports are tiny; anything bigger is refused before it is parsed.
const MAX_BODY: usize = 4 * 1024;

async fn report(State(st): State<AppState>, req: Request) -> Response {
    let c = caller(&req);
    let Ok(bytes) = axum::body::to_bytes(req.into_body(), MAX_BODY).await else {
        return err(StatusCode::PAYLOAD_TOO_LARGE, "report too large");
    };
    let r = match parse_report(&bytes, Utc::now().date_naive()) {
        Ok(r) => r,
        Err(m) => return err(StatusCode::UNPROCESSABLE_ENTITY, &m),
    };
    match store_report(&st.pool, &c.user_id, &r).await {
        Ok(()) => Json(Ok_ { ok: true }).into_response(),
        Err(_) => err(StatusCode::INTERNAL_SERVER_ERROR, "db error"),
    }
}

async fn me(State(st): State<AppState>, req: Request) -> Response {
    let c = caller(&req);
    match me_view(&st.pool, &c.user_id).await {
        Ok(v) => Json(v).into_response(),
        Err(_) => err(StatusCode::INTERNAL_SERVER_ERROR, "db error"),
    }
}

async fn public_view(State(st): State<AppState>, Path(user_id): Path<String>, req: Request) -> Response {
    let c = caller(&req);
    let uid = if user_id == "me" { c.user_id } else { user_id.chars().take(36).collect() };
    match public_of(&st.pool, &uid).await {
        Ok(Some(v)) => Json(v).into_response(),
        Ok(None) => err(StatusCode::NOT_FOUND, "no such trainer"),
        Err(_) => err(StatusCode::INTERNAL_SERVER_ERROR, "db error"),
    }
}

async fn set_title(State(st): State<AppState>, req: Request) -> Response {
    let c = caller(&req);
    let Ok(bytes) = axum::body::to_bytes(req.into_body(), MAX_BODY).await else {
        return err(StatusCode::PAYLOAD_TOO_LARGE, "body too large");
    };
    let t = match parse_title(&bytes) {
        Ok(t) => t,
        Err(m) => return err(StatusCode::UNPROCESSABLE_ENTITY, &m),
    };
    match wear(&st.pool, &c.user_id, t.as_deref()).await {
        Ok(Ok(())) => Json(TitleOk { ok: true, title: t }).into_response(),
        Ok(Err(m)) => err(StatusCode::UNPROCESSABLE_ENTITY, m),
        Err(_) => err(StatusCode::INTERNAL_SERVER_ERROR, "db error"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
    use std::str::FromStr;

    fn today() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 10, 10).unwrap()
    }

    fn body(v: serde_json::Value) -> Vec<u8> {
        serde_json::to_vec(&v).unwrap()
    }

    /// A file-backed temp database (WAL, several connections), so concurrent
    /// writers really race on separate connections.
    async fn file_pool(tag: &str) -> (SqlitePool, std::path::PathBuf) {
        let path = std::env::temp_dir().join(format!(
            "skills-{tag}-{}-{}.db",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        let opts = SqliteConnectOptions::from_str(&format!("sqlite:{}", path.display()))
            .unwrap()
            .journal_mode(SqliteJournalMode::Wal)
            .busy_timeout(std::time::Duration::from_secs(10))
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new().max_connections(4).connect_with(opts).await.unwrap();
        crate::db::migrate(&pool).await.unwrap();
        for (id, gh, handle, active) in [("u1", 1, "ann", 1), ("u2", 2, "bob", 0)] {
            sqlx::query(
                "INSERT INTO users (id, github_id, handle, display_name, avatar_url, trainer_name,
                 is_active, created_at) VALUES (?1, ?2, ?3, ?3, '', '', ?4, datetime('now'))",
            )
            .bind(id)
            .bind(gh)
            .bind(handle)
            .bind(active)
            .execute(&pool)
            .await
            .unwrap();
        }
        (pool, path)
    }

    async fn done(pool: SqlitePool, path: std::path::PathBuf) {
        pool.close().await;
        for ext in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{ext}", path.display()));
        }
    }

    fn rep(day: &str, counts: &[(&'static str, i64)]) -> Report {
        Report { day: day.into(), counts: counts.to_vec() }
    }

    #[test]
    fn tier_thresholds_are_10_50_150_400_1000() {
        assert_eq!(tier_for(0), 0);
        assert_eq!(tier_for(9), 0);
        assert_eq!(tier_for(10), 1);
        assert_eq!(tier_for(49), 1);
        assert_eq!(tier_for(50), 2);
        assert_eq!(tier_for(150), 3);
        assert_eq!(tier_for(399), 3);
        assert_eq!(tier_for(400), 4);
        assert_eq!(tier_for(999), 4);
        assert_eq!(tier_for(1000), 5);
        assert_eq!(tier_for(1_000_000), 5);
        assert_eq!(next_for(0), Some(10));
        assert_eq!(next_for(10), Some(50));
        assert_eq!(next_for(400), Some(1000));
        assert_eq!(next_for(1000), None);
    }

    #[test]
    fn the_catalogue_has_a_title_at_tiers_3_and_5_per_category() {
        assert_eq!(PERKS.len(), CATS.len());
        for (i, (cat, _)) in PERKS.iter().enumerate() {
            assert_eq!(*cat, CATS[i]);
        }
        assert_eq!(perk_name("tests", 3), Some("Test Pilot"));
        assert_eq!(perk_name("tests", 5), Some("Green Machine"));
        assert_eq!(perk_name("review", 3), Some("Second Pair of Eyes"));
        assert_eq!(perk_name("build", 5), Some("Forgemaster"));
        assert_eq!(perk_name("build", 6), None);
        assert!(is_title_id("docs-3") && is_title_id("explore-5"));
        assert!(!is_title_id("docs-4") && !is_title_id("magic-3") && !is_title_id("docs"));
        let mut p = HashMap::new();
        p.insert("tests".to_string(), 1000);
        p.insert("docs".to_string(), 150);
        p.insert("debug".to_string(), 149);
        assert_eq!(unlocked_titles(&p), vec!["tests-3", "tests-5", "docs-3"]);
    }

    #[test]
    fn a_report_parses_in_category_order() {
        let r = parse_report(&body(serde_json::json!({"day": "2026-10-09", "counts": {"build": 3, "tests": 2}})), today())
            .unwrap();
        assert_eq!(r, rep("2026-10-09", &[("tests", 2), ("build", 3)]));
        // Today, and a week back, are fine.
        assert!(parse_report(&body(serde_json::json!({"day": "2026-10-10", "counts": {}})), today()).is_ok());
        assert!(parse_report(&body(serde_json::json!({"day": "2026-10-03", "counts": {"docs": 0}})), today()).is_ok());
    }

    #[test]
    fn a_bad_report_is_refused() {
        let bad = |v: serde_json::Value| parse_report(&body(v), today()).unwrap_err();
        // More than seven days old, or in the future.
        assert_eq!(bad(serde_json::json!({"day": "2026-10-02", "counts": {"tests": 1}})), "day is too old");
        assert_eq!(bad(serde_json::json!({"day": "2026-10-11", "counts": {"tests": 1}})), "day is in the future");
        // n out of range, or not an integer.
        assert!(bad(serde_json::json!({"day": "2026-10-09", "counts": {"tests": 201}})).contains("0..200"));
        assert!(bad(serde_json::json!({"day": "2026-10-09", "counts": {"tests": -1}})).contains("0..200"));
        assert!(bad(serde_json::json!({"day": "2026-10-09", "counts": {"tests": 1.5}})).contains("0..200"));
        assert!(bad(serde_json::json!({"day": "2026-10-09", "counts": {"tests": "3"}})).contains("0..200"));
        // An unknown category, even alongside known ones.
        assert_eq!(bad(serde_json::json!({"day": "2026-10-09", "counts": {"tests": 1, "naps": 2}})), "unknown category");
        // Extra keys, a missing field, a malformed day.
        assert!(bad(serde_json::json!({"day": "2026-10-09", "counts": {}, "path": "/x"})).starts_with("unknown field"));
        assert_eq!(bad(serde_json::json!({"counts": {}})), "day is required");
        assert_eq!(bad(serde_json::json!({"day": "2026-10-09"})), "counts is required");
        assert_eq!(bad(serde_json::json!({"day": "2026-1-9", "counts": {}})), "day must be YYYY-MM-DD");
        assert_eq!(bad(serde_json::json!({"day": "2026-02-30", "counts": {}})), "day must be YYYY-MM-DD");
        assert_eq!(bad(serde_json::json!(["day"])), "body must be an object");
        assert_eq!(parse_report(b"{", today()).unwrap_err(), "invalid JSON");
    }

    #[test]
    fn a_title_body_is_an_id_or_null() {
        assert_eq!(parse_title(br#"{"title":null}"#), Ok(None));
        assert_eq!(parse_title(br#"{"title":"tests-3"}"#), Ok(Some("tests-3".into())));
        assert!(parse_title(br#"{"title":"tests-4"}"#).is_err());
        assert!(parse_title(br#"{"title":3}"#).is_err());
        assert!(parse_title(br#"{}"#).is_err());
        assert!(parse_title(br#"{"title":null,"x":1}"#).is_err());
    }

    #[tokio::test]
    async fn the_upsert_keeps_the_max_and_is_idempotent() {
        let (pool, path) = file_pool("max").await;
        store_report(&pool, "u1", &rep("2026-10-09", &[("tests", 5), ("docs", 2)])).await.unwrap();
        // A lower count (an older report arriving late) never lowers the day.
        store_report(&pool, "u1", &rep("2026-10-09", &[("tests", 3), ("docs", 4)])).await.unwrap();
        // The same report again changes nothing.
        store_report(&pool, "u1", &rep("2026-10-09", &[("tests", 3), ("docs", 4)])).await.unwrap();
        store_report(&pool, "u1", &rep("2026-10-08", &[("tests", 7)])).await.unwrap();
        let p = points(&pool, "u1").await.unwrap();
        assert_eq!(p["tests"], 12);
        assert_eq!(p["docs"], 4);
        assert_eq!(p["build"], 0);
        done(pool, path).await;
    }

    #[tokio::test]
    async fn concurrent_reports_on_separate_connections_keep_the_max() {
        let (pool, path) = file_pool("race").await;
        let (r1, r2) = (rep("2026-10-09", &[("tests", 9), ("build", 1)]), rep("2026-10-09", &[("tests", 4), ("build", 8)]));
        let (r3, r4) = (rep("2026-10-09", &[("tests", 6)]), rep("2026-10-09", &[("build", 3)]));
        let (a, b, c, d) = tokio::join!(
            store_report(&pool, "u1", &r1),
            store_report(&pool, "u1", &r2),
            store_report(&pool, "u1", &r3),
            store_report(&pool, "u1", &r4),
        );
        for r in [a, b, c, d] {
            r.unwrap();
        }
        let p = points(&pool, "u1").await.unwrap();
        assert_eq!(p["tests"], 9);
        assert_eq!(p["build"], 8);
        done(pool, path).await;
    }

    #[tokio::test]
    async fn the_tree_lists_perks_titles_and_the_worn_title() {
        let (pool, path) = file_pool("tree").await;
        // 150 tests points over three days -> tier 3; 12 review -> tier 1.
        for (d, n) in [("2026-10-07", 50), ("2026-10-08", 50), ("2026-10-09", 50)] {
            store_report(&pool, "u1", &rep(d, &[("tests", n)])).await.unwrap();
        }
        store_report(&pool, "u1", &rep("2026-10-09", &[("review", 12)])).await.unwrap();
        let v = me_view(&pool, "u1").await.unwrap();
        assert_eq!(v.cats["tests"]["points"], 150);
        assert_eq!(v.cats["tests"]["tier"], 3);
        assert_eq!(v.cats["tests"]["next"], 400);
        assert_eq!(v.cats["review"]["tier"], 1);
        assert_eq!(v.cats["docs"]["tier"], 0);
        assert_eq!(v.cats["docs"]["next"], 10);
        let keys: Vec<&str> = v.cats.keys().map(String::as_str).collect();
        assert_eq!(keys, CATS.to_vec());
        let ids: Vec<&str> = v.perks.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, vec!["tests-1", "tests-2", "tests-3", "review-1"]);
        assert_eq!(v.perks[2].name, "Test Pilot");
        assert_eq!(v.titles, vec!["tests-3"]);
        assert_eq!(v.title, None);
        done(pool, path).await;
    }

    #[tokio::test]
    async fn a_title_must_be_unlocked() {
        let (pool, path) = file_pool("title").await;
        assert_eq!(wear(&pool, "u1", Some("docs-3")).await.unwrap(), Err("that title is not unlocked yet"));
        store_report(&pool, "u1", &rep("2026-10-09", &[("docs", 150)])).await.unwrap();
        assert_eq!(wear(&pool, "u1", Some("docs-5")).await.unwrap(), Err("that title is not unlocked yet"));
        assert_eq!(wear(&pool, "u1", Some("docs-3")).await.unwrap(), Ok(()));
        assert_eq!(me_view(&pool, "u1").await.unwrap().title.as_deref(), Some("docs-3"));
        assert_eq!(public_of(&pool, "u1").await.unwrap().unwrap().title.as_deref(), Some("docs-3"));
        // Clearing it.
        assert_eq!(wear(&pool, "u1", None).await.unwrap(), Ok(()));
        assert_eq!(me_view(&pool, "u1").await.unwrap().title, None);
        done(pool, path).await;
    }

    #[tokio::test]
    async fn the_public_view_has_tiers_and_title_but_no_counts() {
        let (pool, path) = file_pool("public").await;
        store_report(&pool, "u1", &rep("2026-10-09", &[("explore", 60), ("build", 7)])).await.unwrap();
        let v = public_of(&pool, "u1").await.unwrap().unwrap();
        let wire = serde_json::to_value(&v).unwrap();
        assert_eq!(wire["cats"]["explore"], serde_json::json!({"tier": 2}));
        assert_eq!(wire["cats"]["build"], serde_json::json!({"tier": 0}));
        assert_eq!(wire["cats"].as_object().unwrap().len(), 7);
        let s = wire.to_string();
        for leak in ["points", "next", "\"n\"", "day", "2026", "perks", "60", "\"7\""] {
            assert!(!s.contains(leak), "public view leaks {leak}: {s}");
        }
        assert_eq!(wire.as_object().unwrap().keys().collect::<Vec<_>>(), vec!["cats", "title"]);
        // Inactive and unknown users are not found.
        assert!(public_of(&pool, "u2").await.unwrap().is_none());
        assert!(public_of(&pool, "nobody").await.unwrap().is_none());
        done(pool, path).await;
    }
}
