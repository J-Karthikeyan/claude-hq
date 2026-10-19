//! HQ 2.5 prestige: at shown level 50, reset the SHOWN level for a star.
//!
//! Nothing a player earned is touched. XP, level unlocks, boards and
//! `progress::progress` all keep reading the true all-time XP; prestige only
//! stores an offset (`xp_base`, the XP total at the last claim) and a star
//! count. The shown level is `derive_level(xp - xp_base)`.
//!
//! Rewards are grant-only cosmetics (a `cos:` balance row, insert-if-absent):
//! the star frame at star 1 and the rooftop crown at star 3. Both are soulbound
//! (cosmetics::SOULBOUND), so they cannot be traded away.
//!
//! Routes (all behind `require_device`):
//! - GET  /v1/prestige/me        -> the caller's full view
//! - POST /v1/prestige {requestId}
//! - GET  /v1/prestige/:user_id  -> {stars, shownLevel} for any active user, else 404

use axum::{
    extract::{Path, Request, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::Serialize;
use serde_json::Value;
use sqlx::{Row, SqlitePool};

use crate::{err, AppState, Caller};

/// The shown level at which a star can be claimed.
pub const PRESTIGE_AT: i64 = 50;
/// Star at which each grant-only reward lands.
const REWARDS: [(i64, &str); 2] = [(1, "cos:f-star"), (3, "cos:d-crown")];

#[derive(Debug, Serialize, PartialEq)]
pub struct MeView {
    stars: i64,
    #[serde(rename = "trueLevel")]
    true_level: i64,
    xp: i64,
    #[serde(rename = "shownLevel")]
    shown_level: i64,
    into: i64,
    need: i64,
    #[serde(rename = "canPrestige")]
    can_prestige: bool,
    at: i64,
}

#[derive(Debug, Serialize)]
struct PublicView {
    stars: i64,
    #[serde(rename = "shownLevel")]
    shown_level: i64,
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/prestige", post(claim))
        // "me" is handled inside: one route per segment keeps matching simple.
        .route("/v1/prestige/:user_id", get(get_one))
}

/// Pure: the view from a true XP total, a level and the stored prestige row.
fn view(xp: i64, true_level: i64, stars: i64, xp_base: i64) -> MeView {
    let (shown, into, need) = crate::scoring::derive_level((xp - xp_base).max(0));
    MeView {
        stars,
        true_level,
        xp,
        shown_level: shown,
        into,
        need,
        can_prestige: shown >= PRESTIGE_AT,
        at: PRESTIGE_AT,
    }
}

/// The stored (stars, xp_base); (0, 0) for someone who never prestiged.
async fn row(pool: &SqlitePool, uid: &str) -> Result<(i64, i64), sqlx::Error> {
    Ok(sqlx::query("SELECT stars, xp_base FROM prestige WHERE user_id = ?1")
        .bind(uid)
        .fetch_optional(pool)
        .await?
        .map(|r| (r.get::<i64, _>("stars"), r.get::<i64, _>("xp_base")))
        .unwrap_or((0, 0)))
}

/// (xp, true level) from the shared progression walk.
async fn true_xp(pool: &SqlitePool, uid: &str) -> Result<(i64, i64), sqlx::Error> {
    let all = crate::progress::progress(pool, &[uid]).await?;
    Ok(all.get(uid).map(|p| (p.xp, p.level)).unwrap_or((0, 1)))
}

pub async fn me(pool: &SqlitePool, uid: &str) -> Result<MeView, sqlx::Error> {
    let (xp, lvl) = true_xp(pool, uid).await?;
    let (stars, base) = row(pool, uid).await?;
    Ok(view(xp, lvl, stars, base))
}

/// Outcome of a claim, kept apart from HTTP so the tests can drive it.
#[derive(Debug, PartialEq)]
pub enum Claim {
    /// A new star (or a lost race against a concurrent claim): the state now.
    Done(MeView),
    /// The shown level is below PRESTIGE_AT.
    TooLow(MeView),
}

pub async fn claim_for(pool: &SqlitePool, uid: &str) -> Result<Claim, sqlx::Error> {
    let (xp, lvl) = true_xp(pool, uid).await?;
    let mut tx = pool.begin().await?;
    // Write first: this takes SQLite's write lock before anything is read, so
    // two claims serialise here instead of both reading the same star count.
    sqlx::query("INSERT INTO prestige (user_id) VALUES (?1) ON CONFLICT(user_id) DO NOTHING")
        .bind(uid)
        .execute(&mut *tx)
        .await?;
    let r = sqlx::query("SELECT stars, xp_base FROM prestige WHERE user_id = ?1")
        .bind(uid)
        .fetch_one(&mut *tx)
        .await?;
    let (stars, base): (i64, i64) = (r.get("stars"), r.get("xp_base"));
    let before = view(xp, lvl, stars, base);
    if !before.can_prestige {
        tx.rollback().await?;
        return Ok(Claim::TooLow(before));
    }
    let star = stars + 1;
    let logged = sqlx::query(
        "INSERT INTO prestige_log (user_id, star, xp_at) VALUES (?1, ?2, ?3)
         ON CONFLICT(user_id, star) DO NOTHING",
    )
    .bind(uid)
    .bind(star)
    .bind(xp)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if logged == 0 {
        // Someone else claimed this star first: answer with the state as it is.
        tx.rollback().await?;
        return Ok(Claim::Done(me(pool, uid).await?));
    }
    sqlx::query(
        "UPDATE prestige SET stars = ?2, xp_base = ?3, updated_at = CURRENT_TIMESTAMP
         WHERE user_id = ?1",
    )
    .bind(uid)
    .bind(star)
    .bind(xp)
    .execute(&mut *tx)
    .await?;
    for (at, item) in REWARDS {
        if star >= at {
            sqlx::query(
                "INSERT INTO poke_balances (id, user_id, item, qty) VALUES (?1, ?2, ?3, 1)
                 ON CONFLICT(user_id, item) DO UPDATE SET qty = 1, updated_at = CURRENT_TIMESTAMP
                 WHERE poke_balances.qty < 1",
            )
            .bind(uuid::Uuid::new_v4().to_string())
            .bind(uid)
            .bind(item)
            .execute(&mut *tx)
            .await?;
        }
    }
    tx.commit().await?;
    Ok(Claim::Done(view(xp, lvl, star, xp)))
}

fn request_id_ok(s: &str) -> bool {
    (16..=64).contains(&s.len()) && s.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
}

/// `{requestId}` and nothing else.
fn parse_claim(bytes: &[u8]) -> Result<(), &'static str> {
    let v: Value = serde_json::from_slice(bytes).map_err(|_| "body must be a JSON object")?;
    let obj = v.as_object().ok_or("body must be a JSON object")?;
    if obj.keys().any(|k| k != "requestId") {
        return Err("unexpected field");
    }
    match obj.get("requestId").and_then(Value::as_str) {
        Some(r) if request_id_ok(r) => Ok(()),
        _ => Err("requestId must be 16-64 letters, digits, _ or -"),
    }
}

async fn claim(State(st): State<AppState>, req: Request) -> Response {
    let c = req.extensions().get::<Caller>().expect("caller set by middleware").clone();
    let (_, body) = req.into_parts();
    let Ok(bytes) = axum::body::to_bytes(body, 4 * 1024).await else {
        return err(StatusCode::PAYLOAD_TOO_LARGE, "body too large");
    };
    if let Err(m) = parse_claim(&bytes) {
        return err(StatusCode::UNPROCESSABLE_ENTITY, m);
    }
    match claim_for(&st.pool, &c.user_id).await {
        Ok(Claim::Done(v)) => Json(v).into_response(),
        Ok(Claim::TooLow(_)) => err(StatusCode::CONFLICT, "reach level 50 to prestige"),
        Err(_) => err(StatusCode::INTERNAL_SERVER_ERROR, "db error"),
    }
}

async fn get_one(State(st): State<AppState>, Path(user_id): Path<String>, req: Request) -> Response {
    let c = req.extensions().get::<Caller>().expect("caller set by middleware").clone();
    if user_id == "me" || user_id == c.user_id {
        return match me(&st.pool, &c.user_id).await {
            Ok(v) if user_id == "me" => Json(v).into_response(),
            Ok(v) => Json(PublicView { stars: v.stars, shown_level: v.shown_level }).into_response(),
            Err(_) => err(StatusCode::INTERNAL_SERVER_ERROR, "db error"),
        };
    }
    if user_id.is_empty() || user_id.len() > 64 {
        return err(StatusCode::NOT_FOUND, "no such user");
    }
    match sqlx::query("SELECT 1 FROM users WHERE id = ?1 AND is_active = 1")
        .bind(&user_id)
        .fetch_optional(&st.pool)
        .await
    {
        Ok(Some(_)) => {}
        Ok(None) => return err(StatusCode::NOT_FOUND, "no such user"),
        Err(_) => return err(StatusCode::INTERNAL_SERVER_ERROR, "db error"),
    }
    match me(&st.pool, &user_id).await {
        Ok(v) => Json(PublicView { stars: v.stars, shown_level: v.shown_level }).into_response(),
        Err(_) => err(StatusCode::INTERNAL_SERVER_ERROR, "db error"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// XP that puts someone exactly at the start of `level`.
    fn xp_at(level: i64) -> i64 {
        (1..level).map(|l| 400 + 120 * l).sum()
    }

    async fn pool() -> (SqlitePool, std::path::PathBuf) {
        let path = std::env::temp_dir().join(format!("prestige-test-{}.db", uuid::Uuid::new_v4().simple()));
        let pool = crate::db::connect(&format!("sqlite:{}", path.display())).await.unwrap();
        crate::db::migrate(&pool).await.unwrap();
        for (gh, id, h) in [(1, "u1", "ann"), (2, "u2", "bob")] {
            sqlx::query(
                "INSERT INTO users (id, github_id, handle, display_name, avatar_url, trainer_name,
                 is_active, created_at) VALUES (?1, ?3, ?2, ?2, '', '', 1, datetime('now'))",
            )
            .bind(id)
            .bind(h)
            .bind(gh)
            .execute(&pool)
            .await
            .unwrap();
        }
        (pool, path)
    }

    /// Give `uid` session XP worth `prompts * 10` on a day of its own.
    async fn add_xp(pool: &SqlitePool, uid: &str, day: &str, xp: i64) {
        assert_eq!(xp % 10, 0);
        sqlx::query(
            "INSERT INTO daily_stats (id, user_id, stat_date, prompts, tools, artifacts, replies,
             tokens_input, tokens_output, tokens_cache_read, tokens_cache_creation)
             VALUES (?1, ?2, ?3, ?4, 0, 0, 0, 0, 0, 0, 0)",
        )
        .bind(uuid::Uuid::new_v4().to_string())
        .bind(uid)
        .bind(day)
        .bind(xp / 10)
        .execute(pool)
        .await
        .unwrap();
    }

    async fn has(pool: &SqlitePool, uid: &str, item: &str) -> bool {
        sqlx::query("SELECT 1 FROM poke_balances WHERE user_id = ?1 AND item = ?2 AND qty > 0")
            .bind(uid)
            .bind(item)
            .fetch_optional(pool)
            .await
            .unwrap()
            .is_some()
    }

    fn done(c: Claim) -> MeView {
        match c {
            Claim::Done(v) => v,
            Claim::TooLow(v) => panic!("refused at shown level {}", v.shown_level),
        }
    }

    #[test]
    fn the_view_offsets_only_the_shown_level() {
        let v = view(xp_at(50) + 10, 50, 0, 0);
        assert_eq!((v.shown_level, v.true_level, v.can_prestige, v.at), (50, 50, true, 50));
        let v = view(xp_at(50) + 10, 50, 1, xp_at(50));
        assert_eq!((v.shown_level, v.into, v.stars, v.can_prestige), (1, 10, 1, false));
        assert!(!view(xp_at(49), 49, 0, 0).can_prestige);
        // A base above the total (it never should be) still reads level 1.
        assert_eq!(view(10, 1, 1, 500).shown_level, 1);
    }

    #[test]
    fn a_claim_body_is_a_request_id_and_nothing_else() {
        assert!(parse_claim(br#"{"requestId":"prestige-0123456789ab"}"#).is_ok());
        assert!(parse_claim(br#"{"requestId":"short"}"#).is_err());
        assert!(parse_claim(br#"{"requestId":"prestige 0123456789ab"}"#).is_err());
        assert!(parse_claim(br#"{"requestId":"prestige-0123456789ab","stars":9}"#).is_err());
        assert!(parse_claim(br#"[]"#).is_err());
        assert!(parse_claim(b"{").is_err());
    }

    #[tokio::test]
    async fn below_fifty_there_is_nothing_to_claim() {
        let (p, path) = pool().await;
        add_xp(&p, "u1", "2026-10-01", xp_at(49) / 10 * 10 + 100).await;
        let v = me(&p, "u1").await.unwrap();
        assert!(!v.can_prestige);
        assert!(matches!(claim_for(&p, "u1").await.unwrap(), Claim::TooLow(_)));
        assert_eq!(row(&p, "u1").await.unwrap().0, 0);
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn a_claim_resets_the_shown_level_and_keeps_everything_else() {
        let (p, path) = pool().await;
        let xp = xp_at(52) / 10 * 10;
        add_xp(&p, "u1", "2026-10-01", xp).await;
        let before = crate::progress::progress(&p, &["u1"]).await.unwrap()["u1"].level;
        let cos_before = serde_json::to_value(crate::cosmetics::build_state(&p, "u1").await.unwrap()).unwrap();
        let v = done(claim_for(&p, "u1").await.unwrap());
        assert_eq!(v.stars, 1);
        assert_eq!(row(&p, "u1").await.unwrap(), (1, xp));
        assert_eq!(v.shown_level, crate::scoring::derive_level(0).0);
        assert_eq!(me(&p, "u1").await.unwrap(), v);
        // The true level, and so every unlock, is untouched.
        assert_eq!(crate::progress::progress(&p, &["u1"]).await.unwrap()["u1"].level, before);
        let cos = serde_json::to_value(crate::cosmetics::build_state(&p, "u1").await.unwrap()).unwrap();
        let unlocked = |s: &Value| -> Vec<String> {
            s["items"].as_array().unwrap().iter()
                .filter(|i| i["level"].as_i64().unwrap() > 0 && i["owned"].as_bool().unwrap())
                .map(|i| i["id"].as_str().unwrap().to_string()).collect()
        };
        assert_eq!(unlocked(&cos), unlocked(&cos_before));
        assert_eq!(unlocked(&cos).len(), 5, "every level unlock is still listed and owned");
        assert_eq!(cos["level"], cos_before["level"]);
        // The star frame is now owned and listed; the crown is not yet.
        let ids: Vec<&str> = cos["items"].as_array().unwrap().iter().map(|i| i["id"].as_str().unwrap()).collect();
        assert!(ids.contains(&"f-star") && !ids.contains(&"d-crown"));
        // Another claim right away is refused: the shown level is 1.
        assert!(matches!(claim_for(&p, "u1").await.unwrap(), Claim::TooLow(_)));
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn the_frame_lands_once_and_the_crown_at_star_three() {
        let (p, path) = pool().await;
        let step = xp_at(50) / 10 * 10 + 10;
        for (n, day) in ["2026-09-01", "2026-09-02", "2026-09-03"].iter().enumerate() {
            add_xp(&p, "u1", day, step).await;
            let v = done(claim_for(&p, "u1").await.unwrap());
            assert_eq!(v.stars, n as i64 + 1);
            assert!(has(&p, "u1", "cos:f-star").await);
            assert_eq!(has(&p, "u1", "cos:d-crown").await, n == 2);
        }
        let n: i64 = sqlx::query("SELECT COUNT(*) AS n FROM poke_balances WHERE user_id = 'u1' AND item = 'cos:f-star'")
            .fetch_one(&p).await.unwrap().get("n");
        let q: i64 = sqlx::query("SELECT qty FROM poke_balances WHERE user_id = 'u1' AND item = 'cos:f-star'")
            .fetch_one(&p).await.unwrap().get("qty");
        assert_eq!((n, q), (1, 1));
        let logs: i64 = sqlx::query("SELECT COUNT(*) AS n FROM prestige_log WHERE user_id = 'u1'")
            .fetch_one(&p).await.unwrap().get("n");
        assert_eq!(logs, 3);
        assert!(!has(&p, "u2", "cos:f-star").await);
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn two_concurrent_claims_earn_exactly_one_star() {
        let (p, path) = pool().await;
        add_xp(&p, "u1", "2026-10-01", xp_at(51) / 10 * 10).await;
        let (a, b, c) = tokio::join!(claim_for(&p, "u1"), claim_for(&p, "u1"), claim_for(&p, "u1"));
        let ok = [a.unwrap(), b.unwrap(), c.unwrap()];
        assert!(ok.iter().any(|c| matches!(c, Claim::Done(v) if v.stars == 1)));
        assert_eq!(row(&p, "u1").await.unwrap().0, 1);
        let logs: i64 = sqlx::query("SELECT COUNT(*) AS n FROM prestige_log WHERE user_id = 'u1'")
            .fetch_one(&p).await.unwrap().get("n");
        assert_eq!(logs, 1);
        let _ = std::fs::remove_file(path);
    }
}
