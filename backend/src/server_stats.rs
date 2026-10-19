//! The Arena's own numbers for the HQ Settings panel, ported from
//! `backend/app/routes/server.py`.
//!
//! Counts, never who. The room list and the ticker registry are read for their
//! lengths and sums and then dropped: `realtime::Registry::stats()` keys its
//! `rooms` map by `"<game>:<room_id>"`, so serialising that map would publish
//! every live room id -- including the private ones -- to anyone with a device
//! token. Only aggregates leave this module.

use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use serde::Serialize;
use serde_json::{json, Value};
use sqlx::Row;
use std::sync::{LazyLock, Mutex};
use std::time::Instant;

/// Python stamps `STARTED` at import, so uptime is measured from process
/// start. [`routes`] forces this at boot for the same reason -- left lazy, the
/// first request would be the zero point and every later one would under-report.
static STARTED: LazyLock<Instant> = LazyLock::new(Instant::now);

/// A room id prefix that marks a room as private (`PRIVATE_PREFIX`,
/// app/rooms.py:23).
const PRIVATE_PREFIX: &str = "r_";

struct CpuMark {
    t: f64,
    cpu: f64,
}

/// Process-global, exactly like Python's `_cpu_mark`: the window is "since the
/// last request", not "since this request began". Two concurrent polls race --
/// the first consumes the whole interval and the second hits the 1ms floor --
/// which is accepted upstream, because the Settings panel is a single 5s
/// poller and it wants "since my last poll". `t` counts seconds from
/// [`STARTED`], so the zero seeded here *is* boot, and the first reading is
/// mean CPU since process start, as Python's `{"cpu": 0.0}` seed makes it.
static CPU_MARK: Mutex<CpuMark> = Mutex::new(CpuMark { t: 0.0, cpu: 0.0 });

/// `round(x, 1)`. Python rounds half-to-even and this rounds half-away-from
/// zero, which only differs on an exact `.x5` -- unreachable for a measured
/// float in binary.
fn round1(x: f64) -> f64 {
    (x * 10.0).round() / 10.0
}

/// main.rs has a private `arena_version()`; the default must stay in step
/// with it, and with Python's `os.environ.get("ARENA_VERSION", "dev")`.
fn arena_version() -> String {
    std::env::var("ARENA_VERSION").unwrap_or_else(|_| "dev".into())
}

/// Count the public rooms and the people in them.
///
/// The filter lives here rather than in `RoomManager::summary()`: Python's
/// `manager.summary()` drops private rooms itself, the Rust one does not, and
/// rooms.rs belongs to another port. Leaving it out is silent -- the shape
/// still matches and private rooms just quietly inflate both numbers.
///
/// `online` sums per-room distinct-user counts, so one person sitting in two
/// rooms counts twice. That is Python's number and not a distinct-people
/// count, despite the "Online" label the UI puts on it.
fn count_rooms(snapshot: &[Value]) -> (usize, u64) {
    snapshot
        .iter()
        .filter(|r| !r["roomId"].as_str().unwrap_or("").starts_with(PRIVATE_PREFIX))
        .fold((0, 0), |(n, online), r| (n + 1, online + r["members"].as_u64().unwrap_or(0)))
}

/// `VmRSS` from `/proc/self/status`, in MB. Current RSS, not peak.
fn parse_vm_rss(status: &str) -> Option<f64> {
    let kb: f64 = status
        .lines()
        .find_map(|l| l.strip_prefix("VmRSS:"))?
        .split_whitespace()
        .next()?
        .parse()
        .ok()?;
    Some(round1(kb / 1024.0))
}

/// `/proc/self/stat` fields 14 (utime) and 15 (stime) as seconds of CPU.
///
/// Field 2 is the process name and may itself contain spaces and brackets, so
/// the scan starts after its closing bracket. `sysconf(_SC_CLK_TCK)` would
/// need libc, which is not a declared dependency -- the kernel reports these
/// in USER_HZ, which is 100 on every Linux target Rust supports whatever
/// CONFIG_HZ the kernel was built with.
fn parse_cpu_ticks(stat: &str) -> Option<f64> {
    let fields: Vec<&str> = stat[stat.rfind(')')? + 1..].split_whitespace().collect();
    let utime: u64 = fields.get(11)?.parse().ok()?;
    let stime: u64 = fields.get(12)?.parse().ok()?;
    Some((utime + stime) as f64 / 100.0)
}

/// Python's `_cpu_pct` arithmetic, split out so the window logic is testable
/// without `/proc`. Advances the mark, so it must be called exactly once per
/// request: a second call halves the figure and resets the window.
fn cpu_pct_from(cpu: f64, now: f64, mark: &mut CpuMark) -> f64 {
    let dt = (now - mark.t).max(1e-3);
    let pct = (cpu - mark.cpu) / dt * 100.0;
    mark.t = now;
    mark.cpu = cpu;
    round1(pct.max(0.0))
}

/// Current RSS in MB, or `None` off Linux.
///
/// Python falls back to `getrusage`'s *peak* RSS when `/proc` is unreadable
/// (bytes on macOS, kB on Linux). With no libc dependency there is no such
/// call here, so a macOS dev box reports `null` rather than a different
/// measurement under the same key. Production is Linux, where this is exact
/// parity.
fn rss_mb() -> Option<f64> {
    parse_vm_rss(&std::fs::read_to_string("/proc/self/status").ok()?)
}

/// CPU used since the previous call, as % of one core. `None` off Linux, for
/// the same reason as [`rss_mb`].
fn cpu_pct() -> Option<f64> {
    let cpu = parse_cpu_ticks(&std::fs::read_to_string("/proc/self/stat").ok()?)?;
    let mut mark = CPU_MARK.lock().expect("cpu mark mutex");
    Some(cpu_pct_from(cpu, STARTED.elapsed().as_secs_f64(), &mut mark))
}

/// Python's `database_url.split("///", 1)[-1]` -- the whole string when there
/// is no `///`.
///
/// Deliberately not `db::normalise_url`: that rewrites `sqlite:///./arena.db`
/// to `sqlite:./arena.db`, after which there is no `///` left to split and
/// `dbMb` would be permanently null.
fn sqlite_path(url: &str) -> &str {
    url.split_once("///").map(|(_, p)| p).unwrap_or(url)
}

/// Size of the main database file in MB.
///
/// The `-wal` and `-shm` sidecars are *not* added, so a database with a large
/// uncheckpointed log under-reports -- that is Python's number and the client
/// is calibrated to it. A relative path resolves against the process CWD, as
/// `os.path.getsize` does; canonicalising would change which file is measured.
fn db_mb(url: &str) -> Option<f64> {
    std::fs::metadata(sqlite_path(url))
        .ok()
        .map(|m| round1(m.len() as f64 / 1_048_576.0))
}

/// Field order is the wire order: serde_json writes a struct in declaration
/// order, which is what Python's dict literal emits. All thirteen keys are
/// always present -- the UI tests `a.rssMb != null`, so a missing key is not
/// the same thing as a null one and `skip_serializing_if` is wrong here.
#[derive(Serialize)]
struct ServerStats {
    #[serde(rename = "impl")]
    impl_: &'static str,
    version: String,
    python: String,
    #[serde(rename = "uptimeSecs")]
    uptime_secs: i64,
    #[serde(rename = "rssMb")]
    rss_mb: Option<f64>,
    #[serde(rename = "cpuPct")]
    cpu_pct: Option<f64>,
    rooms: usize,
    online: u64,
    #[serde(rename = "gameLoops")]
    game_loops: u64,
    #[serde(rename = "gameLoopsMax")]
    game_loops_max: u64,
    overruns: u64,
    db: String,
    #[serde(rename = "dbMb")]
    db_mb: Option<f64>,
}

/// Signed-in devices only -- the route is registered inside main.rs's guarded
/// router, which resolves the caller and refreshes `devices.last_seen_at`
/// once. The caller is deliberately not read here: nothing in the response
/// depends on who is asking, and `list_rooms` takes the same shape.
async fn stats(State(st): State<crate::AppState>) -> Response {
    // Snapshot the in-memory counters first and query the database last, as
    // Python does: `summary()` hands back an owned Vec and releases the room
    // mutex, so no lock is held across the await.
    let snapshot = st.rooms.summary().await;
    let (rooms, online) = count_rooms(&snapshot);

    // One registry serves every real-time game (main.rs clones one Arc into
    // all three hubs), so `kart` is as good as any -- and taking `running`
    // and `max` from the same snapshot keeps the pair the UI prints as
    // "N of M" consistent. `overruns` is summed over every ticker, private
    // rooms included: that is Python's figure, and an aggregate integer
    // names nobody.
    let rt = st.kart.registry().stats();
    let game_loops = rt["running"].as_u64().unwrap_or(0);
    let game_loops_max = rt["max"].as_u64().unwrap_or(0);
    let overruns: u64 = rt["rooms"]
        .as_object()
        .map(|m| m.values().map(|r| r["overruns"].as_u64().unwrap_or(0)).sum())
        .unwrap_or(0);

    // Order is load-bearing: rss_mb() is a pure read, cpu_pct() advances the
    // process-global window, and Python evaluates them in that order, once each.
    let rss = rss_mb();
    let cpu = cpu_pct();

    let db = match sqlx::query("PRAGMA journal_mode").fetch_one(&st.pool).await {
        Ok(r) => {
            let mode: String = r.try_get(0).unwrap_or_else(|_| "?".into());
            format!("sqlite (journal_mode={mode})")
        }
        // Unlike /health, this endpoint fails loudly rather than degrading to
        // a 200 that says the database is unreachable: the Settings panel
        // tells "the Arena didn't answer" apart from a stats payload, and the
        // 500 is that signal. Python's routes/server.py wraps nothing here.
        // The CPU window has already advanced -- a 500 still costs a reading,
        // which is Python's behaviour too.
        Err(_) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "detail": "Internal Server Error" })),
            )
                .into_response()
        }
    };

    Json(ServerStats {
        // Python hardcodes "py" here. Reporting which backend is actually
        // running is the whole point of the field -- main.rs's /health already
        // says "rust" -- and the UI renders an unknown value verbatim as the
        // card title (`a.impl==="py" ? "Python" : a.impl`).
        impl_: "rust",
        version: arena_version(),
        // No analogue, and the Arena card never reads it. Kept so the shape
        // does not change; empty says "no interpreter" rather than inventing one.
        python: String::new(),
        uptime_secs: STARTED.elapsed().as_secs() as i64,
        rss_mb: rss,
        cpu_pct: cpu,
        rooms,
        online,
        game_loops,
        game_loops_max,
        overruns,
        db,
        db_mb: db_mb(&st.cfg.database_url),
    })
    .into_response()
}

/// Per-room tick counts, overruns and bytes/s, for `scripts/loadtest.py`. Only
/// the loop registry -- the heavy `/v1/server/stats` numbers are not wanted
/// while a load test is running.
async fn realtime_stats(State(st): State<crate::AppState>) -> Response {
    // Any hub would do: the three share one registry.
    Json(st.kart.registry().stats()).into_response()
}

pub fn routes() -> Router<crate::AppState> {
    // Stamp the uptime origin at boot. main.rs calls this while wiring the
    // router, which is the closest thing to Python's import-time `STARTED`.
    LazyLock::force(&STARTED);
    Router::new().route("/v1/server/stats", get(stats))
}

/// The loadtest route, which takes no device token -- Python declares it on the
/// bare app, outside the `require_device` router, and `scripts/loadtest.py`
/// polls it without one. It is registered inside Python's
/// `if os.environ.get(...) == "1"`, so an Arena without the flag answers 404
/// rather than 403; same here, the route only exists when the flag is set.
pub fn public_routes() -> Router<crate::AppState> {
    let r = Router::new();
    if std::env::var("ARENA_EXPOSE_REALTIME_STATS").as_deref() == Ok("1") {
        return r.route("/v1/realtime/stats", get(realtime_stats));
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn rounds_to_one_decimal() {
        // Non-tie values on purpose: Python rounds half-to-even and Rust
        // half-away-from-zero, so an exact .x5 would encode the wrong rule.
        assert!(close(round1(123.456), 123.5));
        assert!(close(round1(7.84), 7.8));
        assert!(close(round1(0.0), 0.0));
    }

    #[test]
    fn private_rooms_are_not_counted() {
        let snapshot = vec![
            json!({"roomId": "valley", "members": 3}),
            json!({"roomId": "r_abc123", "members": 2}),
            json!({"roomId": "lobby", "members": 1}),
        ];
        assert_eq!(count_rooms(&snapshot), (2, 4));
    }

    #[test]
    fn an_all_private_arena_looks_empty() {
        let snapshot = vec![json!({"roomId": "r_one", "members": 8})];
        assert_eq!(count_rooms(&snapshot), (0, 0));
    }

    #[test]
    fn the_sqlite_path_matches_pythons_split() {
        assert_eq!(sqlite_path("sqlite:///./arena.db"), "./arena.db");
        assert_eq!(sqlite_path("sqlite+aiosqlite:////data/arena.db"), "/data/arena.db");
        // No "///" at all: Python's [-1] hands back the whole string.
        assert_eq!(sqlite_path("sqlite:./arena.db"), "sqlite:./arena.db");
    }

    #[test]
    fn vm_rss_is_read_in_megabytes() {
        let status = "Name:\tarena\nVmPeak:\t  204800 kB\nVmRSS:\t   30720 kB\nThreads:\t9\n";
        assert_eq!(parse_vm_rss(status), Some(30.0));
        assert_eq!(parse_vm_rss("Name:\tarena\nThreads:\t9\n"), None);
    }

    #[test]
    fn cpu_ticks_survive_a_process_name_with_spaces() {
        // Fields 14 and 15 are 250 and 125 ticks: 3.75s at 100 Hz.
        let stat = "42 ((are na)) S 1 42 42 0 -1 4194304 1000 0 0 0 250 125 0 0 20 0 9 0 100";
        assert_eq!(parse_cpu_ticks(stat), Some(3.75));
        assert_eq!(parse_cpu_ticks("42 (arena) S 1 2"), None);
    }

    #[test]
    fn the_first_reading_is_mean_cpu_since_boot() {
        let mut mark = CpuMark { t: 0.0, cpu: 0.0 };
        assert!(close(cpu_pct_from(1.5, 30.0, &mut mark), 5.0));
        // and every later one is since the previous call
        assert!(close(cpu_pct_from(1.8, 40.0, &mut mark), 3.0));
    }

    #[test]
    fn two_near_simultaneous_polls_hit_the_dt_floor() {
        let mut mark = CpuMark { t: 30.0, cpu: 1.5 };
        // Same instant, no CPU burned: the 1ms floor keeps this finite.
        assert!(close(cpu_pct_from(1.5, 30.0, &mut mark), 0.0));
        // The documented race: the second of two back-to-back polls divides a
        // real CPU delta by 1ms and reports an absurd figure. Python does the
        // same, and the single 5s poller never triggers it.
        let mut mark = CpuMark { t: 30.0, cpu: 1.5 };
        assert!(close(cpu_pct_from(1.6, 30.0, &mut mark), 10000.0));
    }

    #[test]
    fn a_backward_step_cannot_report_negative_cpu() {
        let mut mark = CpuMark { t: 0.0, cpu: 5.0 };
        assert!(close(cpu_pct_from(4.0, 10.0, &mut mark), 0.0));
    }

    #[test]
    fn the_wire_keeps_pythons_key_order_and_nulls() {
        let body = serde_json::to_string(&ServerStats {
            impl_: "rust",
            version: "dev".into(),
            python: String::new(),
            uptime_secs: 1234,
            rss_mb: None,
            cpu_pct: Some(0.3),
            rooms: 2,
            online: 5,
            game_loops: 1,
            game_loops_max: 48,
            overruns: 0,
            db: "sqlite (journal_mode=wal)".into(),
            db_mb: None,
        })
        .expect("serialises");
        assert_eq!(
            body,
            r#"{"impl":"rust","version":"dev","python":"","uptimeSecs":1234,"rssMb":null,"cpuPct":0.3,"rooms":2,"online":5,"gameLoops":1,"gameLoopsMax":48,"overruns":0,"db":"sqlite (journal_mode=wal)","dbMb":null}"#
        );
    }
}
