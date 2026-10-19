//! Ingest and leaderboard queries, ported from `app/service.py`.
//!
//! Scoring stays server-side: clients submit raw activity and never a score,
//! so the formula can change without a client release.

use crate::schemas::*;
use crate::scoring::*;
use chrono::{Datelike, NaiveDate, Utc};
use serde::Serialize;
use sqlx::{Row, SqlitePool};
use std::collections::HashSet;

pub const WINDOWS: &[&str] = &["season", "30d", "7d", "all"];

pub fn window_range(window: &str, today: NaiveDate) -> (NaiveDate, NaiveDate) {
    match window {
        "7d" => (today - chrono::Duration::days(6), today),
        "30d" => (today - chrono::Duration::days(29), today),
        "all" => (NaiveDate::from_ymd_opt(2020, 1, 1).unwrap(), today),
        // "season" -- the calendar month, which is what people reset around.
        _ => (today.with_day(1).unwrap(), today),
    }
}

#[derive(Debug, Serialize)]
pub struct BoardEntry {
    pub rank: i64,
    pub handle: String,
    #[serde(rename = "displayName")] pub display_name: String,
    #[serde(rename = "trainerName")] pub trainer_name: String,
    #[serde(rename = "avatarUrl")] pub avatar_url: String,
    pub xp: i64,
    pub level: i64,
    #[serde(rename = "rankTitle")] pub rank_title: String,
    pub prompts: i64,
    pub tools: i64,
    pub artifacts: i64,
    #[serde(rename = "activeDays")] pub active_days: i64,
    pub streak: i64,
    #[serde(rename = "tokensTotal")] pub tokens_total: i64,
    #[serde(rename = "costUSD")] pub cost_usd: Option<f64>,
    #[serde(rename = "isYou")] pub is_you: bool,
}

#[derive(Debug, Serialize)]
pub struct BoardResponse {
    pub window: String,
    #[serde(rename = "startsOn")] pub starts_on: NaiveDate,
    #[serde(rename = "endsOn")] pub ends_on: NaiveDate,
    #[serde(rename = "seasonName")] pub season_name: String,
    #[serde(rename = "generatedAt")] pub generated_at: String,
    pub entries: Vec<BoardEntry>,
}

#[derive(Debug, Serialize)]
pub struct IngestResponse {
    pub accepted: i64,
    pub rejected: i64,
    pub notes: Vec<String>,
}

pub async fn build_board(
    pool: &SqlitePool,
    window: &str,
    viewer_id: Option<&str>,
) -> Result<BoardResponse, sqlx::Error> {
    let today = Utc::now().date_naive();
    let (starts, ends) = window_range(window, today);

    let rows = sqlx::query(
        r#"
        SELECT u.id, u.handle, u.display_name, u.trainer_name, u.avatar_url,
               COALESCE(SUM(d.prompts),0)    AS prompts,
               COALESCE(SUM(d.tools),0)      AS tools,
               COALESCE(SUM(d.artifacts),0)  AS artifacts,
               COALESCE(SUM(d.tokens_input + d.tokens_output
                          + d.tokens_cache_read + d.tokens_cache_creation),0) AS tokens,
               SUM(d.cost_usd)               AS cost,
               COUNT(d.id)                   AS active_days
        FROM users u
        JOIN daily_stats d ON d.user_id = u.id
        WHERE d.stat_date >= ?1 AND d.stat_date <= ?2 AND u.is_active = 1
        GROUP BY u.id
        "#,
    )
    .bind(starts.to_string())
    .bind(ends.to_string())
    .fetch_all(pool)
    .await?;

    // Streaks read the full history, not just the window, so a streak does not
    // appear to reset on the first of the month.
    let since = (today - chrono::Duration::days(400)).to_string();
    let act = sqlx::query(
        "SELECT user_id, stat_date FROM daily_stats
         WHERE stat_date >= ?1 AND (prompts > 0 OR replies > 0)",
    )
    .bind(&since)
    .fetch_all(pool)
    .await?;

    let mut active: std::collections::HashMap<String, HashSet<NaiveDate>> = Default::default();
    for r in &act {
        let uid: String = r.get("user_id");
        let d: String = r.get("stat_date");
        if let Ok(date) = NaiveDate::parse_from_str(&d, "%Y-%m-%d") {
            active.entry(uid).or_default().insert(date);
        }
    }

    let mut entries: Vec<BoardEntry> = rows
        .iter()
        .map(|r| {
            let id: String = r.get("id");
            let prompts: i64 = r.get("prompts");
            let tools: i64 = r.get("tools");
            let artifacts: i64 = r.get("artifacts");
            let xp = xp_from_counts(prompts, tools, artifacts);
            let (level, _, _) = derive_level(xp);
            let handle: String = r.get("handle");
            let display: String = r.get("display_name");
            BoardEntry {
                rank: 0,
                display_name: if display.is_empty() { handle.clone() } else { display },
                trainer_name: r.get("trainer_name"),
                avatar_url: r.get("avatar_url"),
                xp,
                level,
                rank_title: rank_for_level(level).to_string(),
                prompts,
                tools,
                artifacts,
                active_days: r.get("active_days"),
                streak: active.get(&id).map(|s| streak_from_dates(s, today)).unwrap_or(0),
                tokens_total: r.get("tokens"),
                cost_usd: r.get::<Option<f64>, _>("cost").map(|c| (c * 100.0).round() / 100.0),
                is_you: viewer_id == Some(id.as_str()),
                handle,
            }
        })
        .collect();

    entries.sort_by(|a, b| b.xp.cmp(&a.xp).then(a.handle.cmp(&b.handle)));
    for (i, e) in entries.iter_mut().enumerate() {
        e.rank = i as i64 + 1;
    }

    // Python prefers the active season's name and only falls back to the month:
    //     seasonName = season.name if season else today.strftime("%B %Y")
    // `.first()` there means no ORDER BY, so with several active seasons the row
    // is whichever SQLite yields first; LIMIT 1 with no ORDER BY matches that.
    let season_name: Option<(String,)> =
        sqlx::query_as("SELECT name FROM seasons WHERE is_active = 1 LIMIT 1")
            .fetch_optional(pool)
            .await?;

    Ok(BoardResponse {
        window: window.to_string(),
        starts_on: starts,
        ends_on: ends,
        season_name: season_name
            .map(|r| r.0)
            .unwrap_or_else(|| today.format("%B %Y").to_string()),
        generated_at: Utc::now().to_rfc3339(),
        entries,
    })
}

pub async fn ingest(
    pool: &SqlitePool,
    user_id: &str,
    device_id: &str,
    payload: &StatPayload,
    max_prompts: i64,
    max_tools: i64,
    max_backfill: i64,
) -> Result<IngestResponse, sqlx::Error> {
    let today = Utc::now().date_naive();
    let oldest = today - chrono::Duration::days(max_backfill);

    sqlx::query("INSERT INTO stat_snapshots (id, user_id, device_id, received_at, payload)
                 VALUES (?1, ?2, ?3, datetime('now'), ?4)")
        .bind(uuid::Uuid::new_v4().to_string())
        .bind(user_id)
        .bind(device_id)
        .bind(serde_json::to_string(payload).unwrap_or_default())
        .execute(pool)
        .await?;

    if !payload.trainer_name.is_empty() {
        sqlx::query("UPDATE users SET trainer_name = ?1 WHERE id = ?2")
            .bind(&payload.trainer_name).bind(user_id).execute(pool).await?;
    }

    let (mut accepted, mut rejected) = (0i64, 0i64);
    let mut notes: Vec<String> = Vec::new();
    let note = |n: String, notes: &mut Vec<String>| {
        if !notes.contains(&n) { notes.push(n); }
    };

    for day in &payload.days {
        if day.date > today {
            rejected += 1; note("future dates ignored".into(), &mut notes); continue;
        }
        if day.date < oldest {
            rejected += 1;
            note(format!("dates older than {max_backfill} days ignored"), &mut notes); continue;
        }
        if day.prompts > max_prompts {
            rejected += 1;
            note(format!("daily prompts above {max_prompts} rejected"), &mut notes); continue;
        }
        if day.tools > max_tools {
            rejected += 1;
            note(format!("daily tool calls above {max_tools} rejected"), &mut notes); continue;
        }

        // A re-scan of a day is authoritative, so replace rather than accumulate.
        sqlx::query(
            r#"INSERT INTO daily_stats
               (id, user_id, stat_date, prompts, tools, artifacts, replies,
                tokens_input, tokens_output, tokens_cache_read, tokens_cache_creation,
                cost_usd, updated_at)
               VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12, datetime('now'))
               ON CONFLICT(user_id, stat_date) DO UPDATE SET
                 prompts=excluded.prompts, tools=excluded.tools,
                 artifacts=excluded.artifacts, replies=excluded.replies,
                 tokens_input=excluded.tokens_input, tokens_output=excluded.tokens_output,
                 tokens_cache_read=excluded.tokens_cache_read,
                 tokens_cache_creation=excluded.tokens_cache_creation,
                 cost_usd=excluded.cost_usd, updated_at=datetime('now')"#,
        )
        .bind(uuid::Uuid::new_v4().to_string()).bind(user_id).bind(day.date.to_string())
        .bind(day.prompts).bind(day.tools).bind(day.artifacts).bind(day.replies)
        .bind(day.tokens.input).bind(day.tokens.output)
        .bind(day.tokens.cache_read).bind(day.tokens.cache_creation)
        .bind(day.cost_usd)
        .execute(pool).await?;

        for tc in &day.tool_breakdown {
            sqlx::query(
                r#"INSERT INTO daily_tool_stats (id, user_id, stat_date, tool_name, count)
                   VALUES (?1,?2,?3,?4,?5)
                   ON CONFLICT(user_id, stat_date, tool_name)
                   DO UPDATE SET count = excluded.count"#,
            )
            .bind(uuid::Uuid::new_v4().to_string()).bind(user_id)
            .bind(day.date.to_string()).bind(tc.name.as_str()).bind(tc.count)
            .execute(pool).await?;
        }
        accepted += 1;
    }

    Ok(IngestResponse { accepted, rejected, notes })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_match_the_python_ranges() {
        let t = NaiveDate::from_ymd_opt(2026, 9, 26).unwrap();
        assert_eq!(window_range("7d", t).0, NaiveDate::from_ymd_opt(2026, 9, 20).unwrap());
        assert_eq!(window_range("30d", t).0, NaiveDate::from_ymd_opt(2026, 8, 28).unwrap());
        assert_eq!(window_range("season", t).0, NaiveDate::from_ymd_opt(2026, 9, 1).unwrap());
        assert_eq!(window_range("all", t).0, NaiveDate::from_ymd_opt(2020, 1, 1).unwrap());
    }
}
