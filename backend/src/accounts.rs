//! HQ 2.6 account board: several GitHub users who share one Claude account.
//!
//!   POST /v1/me/account     (device token) body: {"key": 64 hex | ""}
//!   GET  /v1/accounts       (device token) the board
//!   GET  /v1/accounts/mine  (device token) every group the caller is in
//!
//! The local HQ (opt-in, `arenaAccountBoard`) sends
//! `sha256("claude-hq:account:v1:" + accountUuid)`. Only that hash leaves the
//! machine, and it never reaches the database either: what is stored on the
//! device row is `HMAC(ARENA_SECRET_KEY, "acct|" + hash)`, so a copy of the
//! database cannot be matched against a known account id.
//!
//! An account is the set of active users whose live devices (not revoked, seen
//! in the last `ACTIVE_DAYS`) carry the same stored key. The key is per device
//! because one person can use different Claude accounts on different machines.
//! A group shows on the board with `MIN_MEMBERS..=MAX_MEMBERS` people; one
//! person alone is just the personal board, and a crowd past the cap is treated
//! as a shared or leaked id and kept off it.
//!
//! The key is self-reported, so this is a social board, not a proof: anyone who
//! has been signed into that Claude account has the id. Nothing here grants
//! anything beyond appearing in the group.
//!
//! PRIVACY: no response carries the key, its hash, an email, an org or a device.
//! Members are shown as handle, display name, avatar and level, the same public
//! fields as every other board.

use axum::{
    extract::{Request, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use hmac::{Hmac, Mac};
use serde::Serialize;
use serde_json::{json, Value};
use sha2::Sha256;
use sqlx::{Row, SqliteConnection};
use std::collections::{BTreeMap, HashMap};

use crate::crews::{crew_level, progress, Prog};
use crate::scoring::derive_level;

const MIN_MEMBERS: usize = 2;
const MAX_MEMBERS: usize = 20;
const BOARD_SIZE: usize = 50;
/// A device that has not called in for this long stops counting, so people who
/// stopped using the account fall off without anyone removing them.
const ACTIVE_DAYS: i64 = 30;
/// The board shows the first few members; `/mine` shows them all.
const PREVIEW_MEMBERS: usize = 5;
const MAX_BODY_BYTES: usize = 1024;

pub fn routes() -> Router<crate::AppState> {
    Router::new()
        .route("/v1/me/account", post(set_key))
        .route("/v1/accounts", get(board))
        .route("/v1/accounts/mine", get(mine))
}

fn err(code: StatusCode, msg: &str) -> Response {
    (code, Json(json!({ "detail": msg }))).into_response()
}

/// Exactly 64 lowercase hex characters: a sha256 hexdigest and nothing else,
/// so no free text can ride in on this field.
fn valid_key(k: &str) -> bool {
    k.len() == 64 && k.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The stored form. Keyed by the server secret, so the column cannot be joined
/// against hashes computed from a known accountUuid.
fn stored_key(secret: &str, key: &str) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("hmac key");
    mac.update(b"acct|");
    mac.update(key.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

/// `{"key": "..."}` and no other field. `""` clears the key.
fn key_body(bytes: &[u8]) -> Result<String, &'static str> {
    let v: Value = serde_json::from_slice(bytes).map_err(|_| "body must be a JSON object")?;
    let obj = v.as_object().ok_or("body must be a JSON object")?;
    if obj.keys().any(|k| k != "key") {
        return Err("only \"key\" is accepted");
    }
    let key = obj.get("key").and_then(|k| k.as_str()).ok_or("key must be a string")?;
    if !key.is_empty() && !valid_key(key) {
        return Err("key must be 64 lowercase hex characters or empty");
    }
    Ok(key.to_string())
}

// --- wire shapes ----------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
struct MemberOut {
    handle: String,
    #[serde(rename = "displayName")]
    display_name: String,
    #[serde(rename = "avatarUrl")]
    avatar_url: String,
    level: i64,
}

#[derive(Debug, Serialize)]
struct AccountOut {
    rank: i64,
    level: i64,
    xp: i64,
    #[serde(rename = "memberCount")]
    member_count: usize,
    members: Vec<MemberOut>,
    #[serde(rename = "isMine")]
    is_mine: bool,
}

/// One (group, user) row: a user with several devices on one key appears once.
#[derive(Debug, Clone)]
struct GroupRow {
    key: String,
    user_id: String,
    handle: String,
    display_name: String,
    avatar_url: String,
}

// --- pure assembly --------------------------------------------------------

/// Rows arrive ordered by key, then earliest device, so members keep the order
/// they joined in. Groups outside `MIN..=MAX` are dropped here, before any XP is
/// looked up.
fn group(rows: Vec<GroupRow>) -> BTreeMap<String, Vec<GroupRow>> {
    let mut out: BTreeMap<String, Vec<GroupRow>> = BTreeMap::new();
    for r in rows {
        out.entry(r.key.clone()).or_default().push(r);
    }
    out.retain(|_, m| (MIN_MEMBERS..=MAX_MEMBERS).contains(&m.len()));
    out
}

fn prog_of(prog: &HashMap<String, Prog>, user_id: &str) -> Prog {
    prog.get(user_id).copied().unwrap_or(Prog { level: derive_level(0).0, xp: 0 })
}

/// Score every group like a crew, sort by `(-xp, first handle)`, rank over the
/// whole list, then cap. `preview` limits the member list and `cap` the number
/// of groups (the board); `None` keeps all of either (`/mine`).
fn assemble(
    groups: &BTreeMap<String, Vec<GroupRow>>,
    prog: &HashMap<String, Prog>,
    viewer_id: &str,
    preview: Option<usize>,
    cap: Option<usize>,
) -> Vec<AccountOut> {
    let mut out: Vec<(String, AccountOut)> = groups
        .values()
        .map(|mem| {
            let xp: i64 = mem.iter().map(|m| prog_of(prog, &m.user_id).xp).sum();
            let first = mem.first().map(|m| m.handle.to_lowercase()).unwrap_or_default();
            let members = mem
                .iter()
                .take(preview.unwrap_or(usize::MAX))
                .map(|m| MemberOut {
                    display_name: if m.display_name.is_empty() { m.handle.clone() } else { m.display_name.clone() },
                    handle: m.handle.clone(),
                    avatar_url: m.avatar_url.clone(),
                    level: prog_of(prog, &m.user_id).level,
                })
                .collect();
            (
                first,
                AccountOut {
                    rank: 0,
                    level: crew_level(xp, mem.len()),
                    xp,
                    member_count: mem.len(),
                    members,
                    is_mine: mem.iter().any(|m| m.user_id == viewer_id),
                },
            )
        })
        .collect();
    out.sort_by(|a, b| b.1.xp.cmp(&a.1.xp).then(a.0.cmp(&b.0)));
    out.into_iter()
        .enumerate()
        .map(|(i, (_, mut a))| {
            a.rank = i as i64 + 1;
            a
        })
        .take(cap.unwrap_or(usize::MAX))
        .collect()
}

// --- queries --------------------------------------------------------------

async fn group_rows(conn: &mut SqliteConnection) -> Result<Vec<GroupRow>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT d.acct_key AS k, u.id AS user_id, u.handle, u.display_name, u.avatar_url,
                MIN(d.rowid) AS first_dev
           FROM devices d JOIN users u ON u.id = d.user_id
          WHERE d.acct_key IS NOT NULL AND d.revoked = 0 AND u.is_active = 1
            AND d.last_seen_at >= datetime('now', ?1)
          GROUP BY d.acct_key, u.id
          ORDER BY d.acct_key, first_dev",
    )
    .bind(format!("-{ACTIVE_DAYS} days"))
    .fetch_all(conn)
    .await?;
    Ok(rows
        .iter()
        .map(|r| GroupRow {
            key: r.get("k"),
            user_id: r.get("user_id"),
            handle: r.get("handle"),
            display_name: r.get("display_name"),
            avatar_url: r.get("avatar_url"),
        })
        .collect())
}

/// Ranked over every group before any cap, so `/mine` and the board agree on
/// rank.
async fn build(
    conn: &mut SqliteConnection,
    viewer_id: &str,
    preview: Option<usize>,
    cap: Option<usize>,
) -> Result<Vec<AccountOut>, sqlx::Error> {
    let groups = group(group_rows(conn).await?);
    let ids: Vec<String> = {
        let mut v: Vec<String> = groups.values().flatten().map(|m| m.user_id.clone()).collect();
        v.sort();
        v.dedup();
        v
    };
    let prog = progress(conn, &ids).await?;
    Ok(assemble(&groups, &prog, viewer_id, preview, cap))
}

// --- routes ---------------------------------------------------------------

async fn set_key(State(st): State<crate::AppState>, req: Request) -> Response {
    let c = req.extensions().get::<crate::Caller>().expect("caller set by middleware").clone();
    let (_, body) = req.into_parts();
    let Ok(bytes) = axum::body::to_bytes(body, MAX_BODY_BYTES).await else {
        return err(StatusCode::BAD_REQUEST, "body too large");
    };
    let key = match key_body(&bytes) {
        Ok(k) => k,
        Err(msg) => return err(StatusCode::UNPROCESSABLE_ENTITY, msg),
    };
    let stored = if key.is_empty() { None } else { Some(stored_key(&st.cfg.secret_key, &key)) };
    match sqlx::query("UPDATE devices SET acct_key = ?1 WHERE id = ?2")
        .bind(&stored)
        .bind(&c.device_id)
        .execute(&st.pool)
        .await
    {
        Ok(_) => Json(json!({ "ok": true, "linked": stored.is_some() })).into_response(),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, &format!("{e}")),
    }
}

async fn board(State(st): State<crate::AppState>, req: Request) -> Response {
    let c = req.extensions().get::<crate::Caller>().expect("caller set by middleware").clone();
    let mut conn = match st.pool.acquire().await {
        Ok(conn) => conn,
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, &format!("{e}")),
    };
    match build(&mut conn, &c.user_id, Some(PREVIEW_MEMBERS), Some(BOARD_SIZE)).await {
        Ok(accounts) => Json(json!({ "accounts": accounts })).into_response(),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, &format!("{e}")),
    }
}

/// A list, not one group: a person on two machines signed into two Claude
/// accounts is in both. Ranked over the whole board, cap ignored, so a group
/// past the top 50 still learns its rank.
async fn mine(State(st): State<crate::AppState>, req: Request) -> Response {
    let c = req.extensions().get::<crate::Caller>().expect("caller set by middleware").clone();
    let mut conn = match st.pool.acquire().await {
        Ok(conn) => conn,
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, &format!("{e}")),
    };
    match build(&mut conn, &c.user_id, None, None).await {
        Ok(all) => {
            let accounts: Vec<AccountOut> = all.into_iter().filter(|a| a.is_mine).collect();
            Json(json!({ "accounts": accounts })).into_response()
        }
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, &format!("{e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::SqlitePool;

    const K1: &str = "1111111111111111111111111111111111111111111111111111111111111111";
    const K2: &str = "2222222222222222222222222222222222222222222222222222222222222222";

    #[test]
    fn only_a_sha256_hexdigest_or_empty_is_accepted() {
        assert_eq!(key_body(format!(r#"{{"key":"{K1}"}}"#).as_bytes()), Ok(K1.to_string()));
        assert_eq!(key_body(br#"{"key":""}"#), Ok(String::new()));
        assert!(key_body(br#"{"key":"ABC"}"#).is_err());
        assert!(key_body(format!(r#"{{"key":"{}"}}"#, "AB".repeat(32)).as_bytes()).is_err());
        assert!(key_body(format!(r#"{{"key":"{}"}}"#, "ab".repeat(32)).as_bytes()).is_ok());
        assert!(key_body(format!(r#"{{"key":"{K1}0"}}"#).as_bytes()).is_err());
        assert!(key_body(format!(r#"{{"key":"{K1}","email":"a@b.c"}}"#).as_bytes()).is_err());
        assert!(key_body(br#"{"key":5}"#).is_err());
        assert!(key_body(b"[]").is_err());
    }

    #[test]
    fn the_stored_key_is_secret_keyed_and_never_the_client_hash() {
        let a = stored_key("s1", K1);
        assert_eq!(a.len(), 64);
        assert_ne!(a, K1);
        assert_eq!(a, stored_key("s1", K1));
        assert_ne!(a, stored_key("s2", K1));
    }

    async fn pool() -> SqlitePool {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        crate::db::migrate(&pool).await.unwrap();
        pool
    }

    async fn user(pool: &SqlitePool, id: &str, gh: i64, prompts: i64) {
        sqlx::query("INSERT INTO users (id, github_id, handle, display_name, avatar_url,
                     trainer_name, is_active, created_at)
                     VALUES (?1, ?2, ?1, '', '', '', 1, datetime('now'))")
            .bind(id).bind(gh).execute(pool).await.unwrap();
        if prompts > 0 {
            sqlx::query("INSERT INTO daily_stats (id, user_id, stat_date, prompts, tools, artifacts, replies,
                         tokens_input, tokens_output, tokens_cache_read, tokens_cache_creation)
                         VALUES (?1 || '-d', ?1, date('now'), ?2, 0, 0, 0, 0, 0, 0, 0)")
                .bind(id).bind(prompts).execute(pool).await.unwrap();
        }
    }

    async fn device(pool: &SqlitePool, user_id: &str, key: Option<&str>, seen: &str, revoked: bool) {
        sqlx::query(&format!(
            "INSERT INTO devices (id, user_id, token_hash, label, created_at, last_seen_at, revoked, acct_key)
             VALUES (?1, ?2, ?3, 'x', datetime('now'), datetime('now', '{seen}'), ?4, ?5)"
        ))
        .bind(uuid::Uuid::new_v4().to_string())
        .bind(user_id)
        .bind(uuid::Uuid::new_v4().to_string())
        .bind(revoked)
        .bind(key.map(|k| stored_key("s", k)))
        .execute(pool)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn people_sharing_a_key_form_one_ranked_group() {
        let pool = pool().await;
        for (i, (u, p)) in [("ann", 40), ("bob", 10), ("cy", 5), ("dee", 0), ("eve", 0), ("old", 0)].iter().enumerate() {
            user(&pool, u, i as i64 + 1, *p).await;
        }
        device(&pool, "ann", Some(K1), "-1 hours", false).await;
        device(&pool, "ann", Some(K1), "-1 hours", false).await; // a second machine: still one member
        device(&pool, "bob", Some(K1), "-2 days", false).await;
        device(&pool, "cy", Some(K2), "-1 hours", false).await; // alone on K2: hidden
        device(&pool, "dee", Some(K2), "-1 hours", true).await; // revoked: does not count
        device(&pool, "old", Some(K2), "-60 days", false).await; // stale: does not count
        device(&pool, "eve", None, "-1 hours", false).await;

        let mut conn = pool.acquire().await.unwrap();
        let board = build(&mut conn, "eve", Some(PREVIEW_MEMBERS), Some(BOARD_SIZE)).await.unwrap();
        assert_eq!(board.len(), 1);
        let g = &board[0];
        assert_eq!((g.rank, g.member_count, g.is_mine), (1, 2, false));
        let handles: Vec<&str> = g.members.iter().map(|m| m.handle.as_str()).collect();
        assert_eq!(handles, ["ann", "bob"]);
        let prog = progress(&mut conn, &["ann".to_string(), "bob".to_string()]).await.unwrap();
        let xp = prog["ann"].xp + prog["bob"].xp;
        assert_eq!((g.xp, g.level), (xp, crew_level(xp, 2)));

        let mine = build(&mut conn, "bob", None, None).await.unwrap();
        assert!(mine.iter().any(|a| a.is_mine));

        let blob = serde_json::to_string(&board).unwrap();
        assert!(!blob.contains(K1) && !blob.contains(&stored_key("s", K1)));
    }

    #[tokio::test]
    async fn a_crowd_past_the_cap_is_kept_off_the_board() {
        let pool = pool().await;
        for i in 0..(MAX_MEMBERS + 1) {
            let id = format!("u{i}");
            user(&pool, &id, i as i64 + 1, 0).await;
            device(&pool, &id, Some(K1), "-1 hours", false).await;
        }
        let mut conn = pool.acquire().await.unwrap();
        assert!(build(&mut conn, "u0", None, None).await.unwrap().is_empty());
    }
}
