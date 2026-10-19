//! 3D character portraits: one small PNG per user, rendered on their own machine
//! by their HQ from the character they built, so everyone's lists can show it
//! without loading a 3D engine.
//!
//!   PUT    /v1/me/portrait        (device token) body: the PNG itself
//!   DELETE /v1/me/portrait        (device token) back to the GitHub picture
//!   GET    /v1/portraits/:user_id (public)       the PNG, for <img src>
//!
//! The GET is public because an `<img>` cannot send a bearer token -- the same
//! exposure as the GitHub avatar URL it stands in for. A portrait is generated
//! art from the user's own choices: no session or transcript data.
//!
//! While a portrait exists, `users.avatar_url` is pointed at its GET URL (with a
//! `?v=` version, so caches refresh on change), which is how the leaderboard,
//! room members, chat, lobby chips and the rest show it without each learning a
//! new field. The GitHub picture is kept in `portraits.github_avatar_url` and put
//! back on DELETE; a GitHub re-login updates that copy, not `avatar_url`.

use axum::{
    extract::{Path, Request, State},
    http::{header, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, put},
    Json, Router,
};
use serde_json::json;
use sqlx::{Row, SqlitePool};

/// Largest accepted PNG, and the square size range (px) a portrait may be.
pub const MAX_BYTES: usize = 64 * 1024;
pub const MIN_PX: u32 = 32;
pub const MAX_PX: u32 = 256;
/// One upload per user this often (ms); faster is a 429.
pub const MIN_GAP_MS: i64 = 3_000;

pub fn routes() -> Router<crate::AppState> {
    Router::new().route("/v1/me/portrait", put(put_portrait).delete(delete_portrait))
}

pub fn public_routes() -> Router<crate::AppState> {
    Router::new().route("/v1/portraits/:user_id", get(get_portrait))
}

/// A PNG we are willing to store and serve: the signature, an IHDR chunk first,
/// a square size in range, and nothing larger than [`MAX_BYTES`].
pub fn check_png(b: &[u8]) -> Result<(u32, u32), &'static str> {
    if b.len() > MAX_BYTES {
        return Err("portrait too large");
    }
    if b.len() < 33 || &b[..8] != b"\x89PNG\r\n\x1a\n" || &b[12..16] != b"IHDR" {
        return Err("not a PNG");
    }
    let w = u32::from_be_bytes([b[16], b[17], b[18], b[19]]);
    let h = u32::from_be_bytes([b[20], b[21], b[22], b[23]]);
    if w != h || !(MIN_PX..=MAX_PX).contains(&w) {
        return Err("portrait must be square, 32 to 256 px");
    }
    if !b.windows(4).any(|x| x == b"IEND") {
        return Err("not a PNG");
    }
    Ok((w, h))
}

pub fn url_for(base: &str, user_id: &str, version: i64) -> String {
    format!("{}/v1/portraits/{}?v={}", base.trim_end_matches('/'), user_id, version)
}

fn is_user_id(s: &str) -> bool {
    !s.is_empty() && s.len() <= 64 && s.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
}

/// Store (or replace) a portrait and point `avatar_url` at it. Returns the URL.
pub async fn store(pool: &SqlitePool, base: &str, user_id: &str, png: &[u8], now_ms: i64)
    -> Result<String, (StatusCode, &'static str)> {
    check_png(png).map_err(|e| (StatusCode::UNPROCESSABLE_ENTITY, e))?;
    let db = |_| (StatusCode::INTERNAL_SERVER_ERROR, "db error");
    let mut tx = pool.begin().await.map_err(db)?;
    let prev = sqlx::query("SELECT version, github_avatar_url FROM portraits WHERE user_id = ?1")
        .bind(user_id).fetch_optional(&mut *tx).await.map_err(db)?;
    let github = match &prev {
        Some(r) => {
            let v: i64 = r.get("version");
            if now_ms - v < MIN_GAP_MS {
                return Err((StatusCode::TOO_MANY_REQUESTS, "slow down"));
            }
            r.get::<String, _>("github_avatar_url")
        }
        None => sqlx::query("SELECT avatar_url FROM users WHERE id = ?1")
            .bind(user_id).fetch_optional(&mut *tx).await.map_err(db)?
            .map(|r| r.get::<String, _>("avatar_url")).unwrap_or_default(),
    };
    let url = url_for(base, user_id, now_ms);
    sqlx::query("INSERT INTO portraits (user_id, png, version, github_avatar_url, updated_at)
                 VALUES (?1, ?2, ?3, ?4, datetime('now'))
                 ON CONFLICT(user_id) DO UPDATE SET png = excluded.png, version = excluded.version,
                     updated_at = excluded.updated_at")
        .bind(user_id).bind(png).bind(now_ms).bind(&github)
        .execute(&mut *tx).await.map_err(db)?;
    sqlx::query("UPDATE users SET avatar_url = ?1 WHERE id = ?2")
        .bind(&url).bind(user_id).execute(&mut *tx).await.map_err(db)?;
    tx.commit().await.map_err(db)?;
    Ok(url)
}

/// Remove a portrait and put the GitHub picture back. False when there was none.
pub async fn remove(pool: &SqlitePool, user_id: &str) -> Result<bool, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let Some(r) = sqlx::query("SELECT github_avatar_url FROM portraits WHERE user_id = ?1")
        .bind(user_id).fetch_optional(&mut *tx).await? else { return Ok(false) };
    let github: String = r.get("github_avatar_url");
    sqlx::query("UPDATE users SET avatar_url = ?1 WHERE id = ?2").bind(&github).bind(user_id)
        .execute(&mut *tx).await?;
    sqlx::query("DELETE FROM portraits WHERE user_id = ?1").bind(user_id).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(true)
}

/// A GitHub re-login: refresh the kept GitHub picture instead of `avatar_url`
/// when the user has a portrait. True when it did (the caller then leaves
/// `avatar_url` alone).
pub async fn keep_github_avatar(pool: &SqlitePool, user_id: &str, avatar: &str) -> bool {
    sqlx::query("UPDATE portraits SET github_avatar_url = ?1 WHERE user_id = ?2")
        .bind(avatar).bind(user_id).execute(pool).await
        .map(|r| r.rows_affected() > 0).unwrap_or(false)
}

fn err(code: StatusCode, msg: &str) -> Response {
    (code, Json(json!({"detail": msg}))).into_response()
}

async fn put_portrait(State(st): State<crate::AppState>, req: Request) -> Response {
    let c = req.extensions().get::<crate::Caller>().expect("caller set by middleware").clone();
    let Ok(body) = axum::body::to_bytes(req.into_body(), MAX_BYTES + 1).await else {
        return err(StatusCode::PAYLOAD_TOO_LARGE, "portrait too large");
    };
    match store(&st.pool, &st.cfg.public_base_url, &c.user_id, &body,
                chrono::Utc::now().timestamp_millis()).await {
        Ok(url) => Json(json!({"ok": true, "avatarUrl": url})).into_response(),
        Err((code, msg)) => err(code, msg),
    }
}

async fn delete_portrait(State(st): State<crate::AppState>, req: Request) -> Response {
    let c = req.extensions().get::<crate::Caller>().expect("caller set by middleware").clone();
    match remove(&st.pool, &c.user_id).await {
        Ok(had) => Json(json!({"ok": true, "removed": had})).into_response(),
        Err(_) => err(StatusCode::INTERNAL_SERVER_ERROR, "db error"),
    }
}

async fn get_portrait(State(st): State<crate::AppState>, Path(user_id): Path<String>) -> Response {
    if !is_user_id(&user_id) {
        return err(StatusCode::NOT_FOUND, "no portrait");
    }
    let row = sqlx::query("SELECT png FROM portraits WHERE user_id = ?1")
        .bind(&user_id).fetch_optional(&st.pool).await;
    match row {
        Ok(Some(r)) => {
            let png: Vec<u8> = r.get("png");
            let mut res = png.into_response();
            let h = res.headers_mut();
            h.insert(header::CONTENT_TYPE, HeaderValue::from_static("image/png"));
            // The URL carries ?v=<version>, so a changed portrait is a new URL.
            h.insert(header::CACHE_CONTROL, HeaderValue::from_static("public, max-age=86400"));
            h.insert("x-content-type-options", HeaderValue::from_static("nosniff"));
            h.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, HeaderValue::from_static("*"));
            res
        }
        Ok(None) => err(StatusCode::NOT_FOUND, "no portrait"),
        Err(_) => err(StatusCode::INTERNAL_SERVER_ERROR, "db error"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal valid-looking PNG of `px` x `px`.
    fn png(px: u32) -> Vec<u8> {
        let mut b = b"\x89PNG\r\n\x1a\n\x00\x00\x00\x0dIHDR".to_vec();
        b.extend_from_slice(&px.to_be_bytes());
        b.extend_from_slice(&px.to_be_bytes());
        b.extend_from_slice(&[8, 6, 0, 0, 0, 0, 0, 0, 0]);
        b.extend_from_slice(b"\x00\x00\x00\x00IEND\xaeB`\x82");
        b
    }

    async fn pool() -> SqlitePool {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        crate::db::migrate(&pool).await.unwrap();
        sqlx::query("INSERT INTO users (id, github_id, handle, display_name, avatar_url, trainer_name,
                     is_active, created_at) VALUES ('u1', 1, 'ann', 'Ann', 'https://gh/a.png', '', 1,
                     datetime('now'))").execute(&pool).await.unwrap();
        pool
    }

    async fn avatar(pool: &SqlitePool) -> String {
        sqlx::query("SELECT avatar_url FROM users WHERE id = 'u1'").fetch_one(pool).await.unwrap()
            .get("avatar_url")
    }

    #[test]
    fn only_small_square_pngs_pass() {
        assert_eq!(check_png(&png(128)), Ok((128, 128)));
        assert!(check_png(&png(16)).is_err());
        assert!(check_png(&png(512)).is_err());
        assert!(check_png(b"GIF89a.....................................").is_err());
        let mut big = png(128);
        big.resize(MAX_BYTES + 1, 0);
        assert_eq!(check_png(&big), Err("portrait too large"));
        let mut not_square = png(128);
        not_square[23] = 64;
        assert!(check_png(&not_square).is_err());
        let mut no_end = png(128);
        no_end.truncate(33);
        assert!(check_png(&no_end).is_err());
    }

    #[tokio::test]
    async fn a_portrait_takes_over_the_avatar_and_gives_it_back() {
        let pool = pool().await;
        let url = store(&pool, "https://arena.example/", "u1", &png(128), 10_000).await.unwrap();
        assert_eq!(url, "https://arena.example/v1/portraits/u1?v=10000");
        assert_eq!(avatar(&pool).await, url);
        // Too soon, then a replacement keeps the ORIGINAL GitHub picture.
        assert_eq!(store(&pool, "https://arena.example", "u1", &png(128), 11_000).await.unwrap_err().0,
                   StatusCode::TOO_MANY_REQUESTS);
        store(&pool, "https://arena.example", "u1", &png(64), 20_000).await.unwrap();
        // A GitHub re-login refreshes the kept copy, not the shown avatar.
        assert!(keep_github_avatar(&pool, "u1", "https://gh/b.png").await);
        assert!(avatar(&pool).await.ends_with("?v=20000"));
        assert!(remove(&pool, "u1").await.unwrap());
        assert_eq!(avatar(&pool).await, "https://gh/b.png");
        assert!(!remove(&pool, "u1").await.unwrap());
        assert!(!keep_github_avatar(&pool, "u1", "https://gh/c.png").await);
        // A bad upload stores nothing.
        assert_eq!(store(&pool, "x", "u1", b"nope", 99_000).await.unwrap_err().0,
                   StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(avatar(&pool).await, "https://gh/b.png");
    }

    #[test]
    fn user_ids_are_checked_before_the_database() {
        assert!(is_user_id("ae1a7051-d9d9-4f65-a88f-e087ff9cf09b"));
        assert!(!is_user_id("../x"));
        assert!(!is_user_id(""));
    }
}
