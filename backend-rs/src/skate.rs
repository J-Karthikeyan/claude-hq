//! Arena City skatepark: the weekly skate board (best combo per player per ISO week, UTC).
//!
//! Only a score leaves a player's machine: the combo's total and how many tricks it chained
//! (games/hqskate.js banks `total = points x tricks`). Nothing about what they did, where, or
//! when beyond the week.
//!
//! Routes (both behind `require_device`):
//! - POST /v1/skate {total, tricks} -> {week, best, rank}   (keeps your best; one post per MIN_GAP_MS)
//! - GET  /v1/skate                 -> {week, top: [{userId, handle, displayName, best, tricks}], me: {best, rank} | null}

use axum::{
    extract::{Request, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;
use sqlx::{Row, SqlitePool};

use crate::{err, AppState, Caller};

/// A combo's total: at least one point, at most this.
pub const MAX_TOTAL: i64 = 200_000;
/// Tricks in a combo (games/hqskate.js MAX_MULT).
pub const MAX_TRICKS: i64 = 10;
/// The most one trick can be worth (a long grind or a big air), so `total` must fit
/// `PER_TRICK x tricks x tricks` (points times the multiplier).
const PER_TRICK: i64 = 5_000;
/// One post per player this often; a faster one is refused, not queued.
const MIN_GAP_MS: i64 = 3_000;
/// How many the board lists.
pub const TOP: i64 = 20;

pub fn routes() -> Router<AppState> {
    Router::new().route("/v1/skate", get(board).post(post_combo))
}

/// The ISO week a moment falls in: the same key cups and the world boss use.
pub fn week_of(now: DateTime<Utc>) -> String {
    crate::weeks::iso_week(&now.format("%Y-%m-%d").to_string())
}

/// A whole number from JSON (not a bool, not a fraction).
fn whole(v: Option<&Value>) -> Option<i64> {
    let n = v?.as_number()?;
    if let Some(i) = n.as_i64() {
        return Some(i);
    }
    let f = n.as_f64()?;
    (f.fract() == 0.0 && f.abs() < 1e12).then_some(f as i64)
}

/// `{total, tricks}` and nothing else, both in range and consistent with each other.
pub fn parse_post(bytes: &[u8]) -> Result<(i64, i64), &'static str> {
    let v: Value = serde_json::from_slice(bytes).map_err(|_| "body must be a JSON object")?;
    let obj = v.as_object().ok_or("body must be a JSON object")?;
    if obj.keys().any(|k| k != "total" && k != "tricks") {
        return Err("unexpected field");
    }
    let total = whole(obj.get("total")).ok_or("total must be a whole number")?;
    let tricks = whole(obj.get("tricks")).ok_or("tricks must be a whole number")?;
    if !(1..=MAX_TRICKS).contains(&tricks) {
        return Err("tricks must be 1-10");
    }
    if !(1..=MAX_TOTAL).contains(&total) {
        return Err("total must be 1-200000");
    }
    if total > PER_TRICK * tricks * tricks {
        return Err("total is more than that many tricks can score");
    }
    Ok((total, tricks))
}

#[derive(Debug, Serialize, PartialEq)]
pub struct Posted {
    week: String,
    best: i64,
    rank: i64,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct Entry {
    #[serde(rename = "userId")]
    user_id: String,
    handle: String,
    #[serde(rename = "displayName")]
    display_name: String,
    best: i64,
    tricks: i64,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct Mine {
    best: i64,
    rank: i64,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct Board {
    week: String,
    top: Vec<Entry>,
    me: Option<Mine>,
}

/// Your rank this week: one more than the players with a strictly better best.
async fn rank_of(pool: &SqlitePool, week: &str, best: i64) -> Result<i64, sqlx::Error> {
    let r = sqlx::query(
        "SELECT COUNT(*) AS n FROM skate_best s JOIN users u ON u.id = s.user_id
         WHERE s.week = ?1 AND s.total > ?2 AND u.is_active = 1",
    )
    .bind(week)
    .bind(best)
    .fetch_one(pool)
    .await?;
    Ok(r.get::<i64, _>("n") + 1)
}

#[derive(Debug, PartialEq)]
pub enum Post {
    Done(Posted),
    TooSoon,
}

/// Keep `total` if it beats the caller's best this week. Rate-limited per player.
pub async fn post_for(pool: &SqlitePool, uid: &str, total: i64, tricks: i64, now: DateTime<Utc>)
    -> Result<Post, sqlx::Error> {
    let week = week_of(now);
    let ms = now.timestamp_millis();
    let mut tx = pool.begin().await?;
    let prev = sqlx::query("SELECT total, last_post_ms FROM skate_best WHERE week = ?1 AND user_id = ?2")
        .bind(&week)
        .bind(uid)
        .fetch_optional(&mut *tx)
        .await?
        .map(|r| (r.get::<i64, _>("total"), r.get::<i64, _>("last_post_ms")));
    if let Some((_, last)) = prev {
        if ms - last < MIN_GAP_MS {
            tx.rollback().await?;
            return Ok(Post::TooSoon);
        }
    }
    sqlx::query(
        "INSERT INTO skate_best (week, user_id, total, tricks, last_post_ms) VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(week, user_id) DO UPDATE SET
           tricks = CASE WHEN excluded.total > skate_best.total THEN excluded.tricks ELSE skate_best.tricks END,
           at = CASE WHEN excluded.total > skate_best.total THEN CURRENT_TIMESTAMP ELSE skate_best.at END,
           total = MAX(skate_best.total, excluded.total),
           last_post_ms = excluded.last_post_ms",
    )
    .bind(&week)
    .bind(uid)
    .bind(total)
    .bind(tricks)
    .bind(ms)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    let best = prev.map_or(total, |(t, _)| t.max(total));
    let rank = rank_of(pool, &week, best).await?;
    Ok(Post::Done(Posted { week, best, rank }))
}

/// This week's top TOP, best first (ties: whoever got there first), and the caller's own place.
pub async fn board_for(pool: &SqlitePool, uid: &str, now: DateTime<Utc>) -> Result<Board, sqlx::Error> {
    let week = week_of(now);
    let rows = sqlx::query(
        "SELECT s.user_id AS user_id, u.handle AS handle, u.display_name AS display_name,
                s.total AS total, s.tricks AS tricks
         FROM skate_best s JOIN users u ON u.id = s.user_id
         WHERE s.week = ?1 AND u.is_active = 1
         ORDER BY s.total DESC, s.at ASC, s.user_id ASC LIMIT ?2",
    )
    .bind(&week)
    .bind(TOP)
    .fetch_all(pool)
    .await?;
    let top = rows
        .iter()
        .map(|r| Entry {
            user_id: r.get("user_id"),
            handle: r.get("handle"),
            display_name: r.get("display_name"),
            best: r.get("total"),
            tricks: r.get("tricks"),
        })
        .collect();
    let mine = sqlx::query("SELECT total FROM skate_best WHERE week = ?1 AND user_id = ?2")
        .bind(&week)
        .bind(uid)
        .fetch_optional(pool)
        .await?
        .map(|r| r.get::<i64, _>("total"));
    let me = match mine {
        Some(best) => Some(Mine { best, rank: rank_of(pool, &week, best).await? }),
        None => None,
    };
    Ok(Board { week, top, me })
}

async fn post_combo(State(st): State<AppState>, req: Request) -> Response {
    let c = req.extensions().get::<Caller>().expect("caller set by middleware").clone();
    let (_, body) = req.into_parts();
    let Ok(bytes) = axum::body::to_bytes(body, 1024).await else {
        return err(StatusCode::PAYLOAD_TOO_LARGE, "body too large");
    };
    let (total, tricks) = match parse_post(&bytes) {
        Ok(v) => v,
        Err(m) => return err(StatusCode::UNPROCESSABLE_ENTITY, m),
    };
    match post_for(&st.pool, &c.user_id, total, tricks, Utc::now()).await {
        Ok(Post::Done(v)) => Json(v).into_response(),
        Ok(Post::TooSoon) => err(StatusCode::TOO_MANY_REQUESTS, "slow down"),
        Err(_) => err(StatusCode::INTERNAL_SERVER_ERROR, "db error"),
    }
}

async fn board(State(st): State<AppState>, req: Request) -> Response {
    let c = req.extensions().get::<Caller>().expect("caller set by middleware").clone();
    match board_for(&st.pool, &c.user_id, Utc::now()).await {
        Ok(b) => Json(b).into_response(),
        Err(_) => err(StatusCode::INTERNAL_SERVER_ERROR, "db error"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, mo, d, h, mi, s).unwrap()
    }

    async fn pool() -> (SqlitePool, std::path::PathBuf) {
        let path = std::env::temp_dir().join(format!("skate-test-{}.db", uuid::Uuid::new_v4().simple()));
        let pool = crate::db::connect(&format!("sqlite:{}", path.display())).await.unwrap();
        crate::db::migrate(&pool).await.unwrap();
        for (gh, id, h, active) in [(1, "u1", "ann", 1), (2, "u2", "bob", 1), (3, "u3", "cyd", 1), (4, "u4", "gone", 0)] {
            sqlx::query(
                "INSERT INTO users (id, github_id, handle, display_name, avatar_url, trainer_name,
                 is_active, created_at) VALUES (?1, ?3, ?2, ?2, '', '', ?4, datetime('now'))",
            )
            .bind(id)
            .bind(h)
            .bind(gh)
            .bind(active)
            .execute(&pool)
            .await
            .unwrap();
        }
        (pool, path)
    }

    #[test]
    fn a_post_is_two_whole_numbers_in_range_and_nothing_else() {
        assert_eq!(parse_post(br#"{"total": 800, "tricks": 2}"#), Ok((800, 2)));
        assert_eq!(parse_post(br#"{"total": 800.0, "tricks": 2}"#), Ok((800, 2)));
        assert_eq!(parse_post(br#"{"total": 200000, "tricks": 10}"#), Ok((200_000, 10)));
        for bad in [
            &br#"{"total": 0, "tricks": 1}"#[..],
            br#"{"total": 200001, "tricks": 10}"#,
            br#"{"total": 100, "tricks": 0}"#,
            br#"{"total": 100, "tricks": 11}"#,
            br#"{"total": 1.5, "tricks": 1}"#,
            br#"{"total": true, "tricks": 1}"#,
            br#"{"total": "100", "tricks": 1}"#,
            br#"{"total": 100}"#,
            br#"{"total": 100, "tricks": 1, "where": "rail"}"#,
            br#"{"total": 6000, "tricks": 1}"#,
            br#"[100, 1]"#,
            b"nope",
        ] {
            assert!(parse_post(bad).is_err(), "accepted {}", String::from_utf8_lossy(bad));
        }
    }

    #[test]
    fn the_week_turns_on_monday_utc() {
        assert_eq!(week_of(at(2026, 10, 18, 23, 59, 59)), "2026-W42");
        assert_eq!(week_of(at(2026, 10, 19, 0, 0, 0)), "2026-W43");
        assert_eq!(week_of(at(2027, 1, 3, 12, 0, 0)), "2026-W53");
    }

    #[tokio::test]
    async fn the_best_is_kept_posts_are_rate_limited_and_weeks_are_separate() {
        let (p, path) = pool().await;
        let t0 = at(2026, 10, 14, 12, 0, 0);
        let Post::Done(a) = post_for(&p, "u1", 800, 2, t0).await.unwrap() else { panic!() };
        assert_eq!((a.week.as_str(), a.best, a.rank), ("2026-W42", 800, 1));
        // Too soon: refused, nothing changes.
        assert_eq!(post_for(&p, "u1", 5000, 3, t0 + chrono::Duration::milliseconds(500)).await.unwrap(), Post::TooSoon);
        // A lower combo later keeps the best.
        let Post::Done(b) = post_for(&p, "u1", 300, 1, t0 + chrono::Duration::seconds(5)).await.unwrap() else { panic!() };
        assert_eq!(b.best, 800);
        // A higher one replaces it.
        let Post::Done(c) = post_for(&p, "u1", 2400, 3, t0 + chrono::Duration::seconds(10)).await.unwrap() else { panic!() };
        assert_eq!(c.best, 2400);
        post_for(&p, "u2", 3000, 3, t0).await.unwrap();
        post_for(&p, "u3", 100, 1, t0).await.unwrap();
        post_for(&p, "u4", 99_000, 10, t0).await.unwrap();       // deactivated: never listed or counted
        let board = board_for(&p, "u1", t0).await.unwrap();
        let order: Vec<(&str, i64)> = board.top.iter().map(|e| (e.handle.as_str(), e.best)).collect();
        assert_eq!(order, vec![("bob", 3000), ("ann", 2400), ("cyd", 100)]);
        assert_eq!(board.top[1].tricks, 3);
        assert_eq!(board.me, Some(Mine { best: 2400, rank: 2 }));
        // Next week starts empty, and someone who never posted has no place.
        let next = board_for(&p, "u1", at(2026, 10, 19, 0, 0, 1)).await.unwrap();
        assert_eq!((next.week.as_str(), next.top.len(), next.me), ("2026-W43", 0, None));
        assert_eq!(board_for(&p, "nobody", t0).await.unwrap().me, None);
        drop(p);
        let _ = std::fs::remove_file(path);
    }
}
