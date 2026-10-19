//! Ported from tests/test_scanner.py, test_fatigue.py (scan collector) and
//! test_worksignals.py: the single transcript pass, on temp files only.

use chrono::{Duration, TimeZone, Utc};
use claudehq::app::{App, Paths};
use claudehq::{cost, fatigue, py, scan, worksignals};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::Arc;

const TS: &str = "2026-09-01T10:00:00.000Z";
const DAY: &str = "2026-09-01";

fn write(path: &Path, recs: &[Value]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let s: String = recs.iter().map(|r| format!("{}\n", r)).collect();
    std::fs::write(path, s).unwrap();
}

fn scan(recs: &[Value]) -> scan::Agg {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("22222222-2222-4222-8222-222222222222.jsonl");
    write(&p, recs);
    scan::scan_claude(&p)
}

fn usage() -> Value {
    json!({"input_tokens": 1000, "output_tokens": 500, "cache_read_input_tokens": 0, "cache_creation_input_tokens": 0})
}

fn asst(blocks: Value, mid: Option<&str>, rid: Option<&str>, ts: &str) -> Value {
    let mut msg = json!({"model": "claude-sonnet-4-6", "content": blocks, "usage": usage()});
    if let Some(m) = mid {
        msg["id"] = json!(m);
    }
    let mut r = json!({"type": "assistant", "timestamp": ts, "message": msg});
    if let Some(r2) = rid {
        r["requestId"] = json!(r2);
    }
    r
}

fn text(t: &str) -> Value {
    json!({"type": "text", "text": t})
}

fn user(content: Value, ts: &str) -> Value {
    json!({"type": "user", "timestamp": ts, "message": {"content": content}})
}

fn one_record_cost() -> f64 {
    cost::usage_cost("claude-sonnet-4-6", Some(&usage())).cost
}

#[test]
fn records_sharing_a_message_id_count_once() {
    let agg = scan(&[
        asst(json!([text("thinking about it")]), Some("msg_1"), Some("req_1"), TS),
        asst(json!([{"type": "tool_use", "id": "t1", "name": "Read", "input": {}}]), Some("msg_1"), Some("req_1"), TS),
        asst(json!([text("done")]), Some("msg_1"), Some("req_1"), TS),
    ]);
    assert_eq!((agg.tok_output, agg.tok_input), (500, 1000));
    assert!((agg.cost - one_record_cost()).abs() < 1e-12);
    let day = &agg.per_day[DAY];
    assert_eq!((day.output, day.tools, day.replies), (500, 1, 2));
}

#[test]
fn usage_keys() {
    assert_eq!(scan(&[asst(json!([text("a")]), Some("msg_1"), None, TS), asst(json!([text("b")]), Some("msg_2"), None, TS)]).tok_output, 1000);
    let agg = scan(&[asst(json!([text("a")]), None, Some("req_1"), TS), asst(json!([text("b")]), None, Some("req_1"), TS), asst(json!([text("c")]), None, Some("req_2"), TS)]);
    assert_eq!(agg.tok_output, 1000);
    assert_eq!(scan(&[asst(json!([text("a")]), None, None, TS), asst(json!([text("b")]), None, None, TS)]).tok_output, 1000);
    let agg = scan(&[asst(json!([text("a")]), Some("m"), None, TS), asst(json!([text("b")]), Some("m"), None, "2026-09-02T00:30:00.000Z")]);
    assert_eq!(agg.per_day["2026-09-02"].output, 0);
    assert_eq!(agg.per_day["2026-09-02"].replies, 1);
}

#[test]
fn image_prompts_count() {
    let art = "https://claude.ai/artifact/abc123";
    let agg = scan(&[user(json!([{"type": "image", "source": {"type": "base64", "data": "xx"}}, text(&format!("look at this {}", art))]), TS)]);
    assert_eq!(agg.prompt_count, 1);
    let day = &agg.per_day[DAY];
    assert_eq!((day.prompts, day.artifacts, day.hours.values().sum::<i64>()), (1, 1, 1));
    assert_eq!(agg.timeline.iter().map(|t| t.kind).collect::<Vec<_>>(), vec!["you"]);
    assert_eq!(agg.links.iter().map(|l| l.url.as_str()).collect::<Vec<_>>(), vec![art]);
}

#[test]
fn error_signatures() {
    let sigs = |a: &scan::Agg| a.errors.iter().map(|e| e.1.clone()).collect::<Vec<_>>();
    assert!(scan(&[asst(json!([text("Your billing quota and rate limit look fine.")]), Some("m1"), None, TS)]).errors.is_empty());
    let mut r = asst(json!([text("API Error: Credit balance is too low")]), Some("m1"), None, TS);
    r["isApiErrorMessage"] = json!(true);
    assert_eq!(sigs(&scan(&[r])), vec!["credit balance"]);
    let err = json!({"message": "529 {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"Overloaded\"}}", "status": 529});
    assert_eq!(sigs(&scan(&[json!({"type": "system", "subtype": "api_error", "level": "error", "timestamp": TS, "error": err, "retryAttempt": 10, "maxRetries": 10})])), vec!["overloaded"]);
    assert!(scan(&[json!({"type": "system", "subtype": "api_error", "timestamp": TS, "error": {"message": "Overloaded"}, "retryAttempt": 1, "maxRetries": 10})]).errors.is_empty());
    assert!(scan(&[json!({"type": "system", "subtype": "away_summary", "timestamp": TS, "content": "We discussed the billing quota and rate limits."})]).errors.is_empty());
    for (msg, sig) in [("You've hit your monthly spend limit", "spend limit"), ("Login expired \u{b7} Please run /login", "login expired"), ("Prompt is too long", "prompt is too long")] {
        let mut r = asst(json!([text(msg)]), Some("m1"), None, TS);
        r["isApiErrorMessage"] = json!(true);
        assert_eq!(sigs(&scan(&[r])), vec![sig]);
    }
    let mut r = asst(json!([text("API Error: Unable to connect to API (ENOTFOUND)")]), Some("m1"), None, TS);
    r["isApiErrorMessage"] = json!(true);
    assert_eq!(sigs(&scan(&[r])), vec!["api_error"]);
    assert_eq!(sigs(&scan(&[json!({"type": "system", "subtype": "api_error", "timestamp": TS, "error": {"message": "socket hang up"}})])), vec!["api_error"]);
}

/// test_scanner.py RecoveryTests: an error alert clears on a good reply after it.
#[test]
fn recovery_from_errors() {
    const LATER: &str = "2026-09-01T10:05:00.000Z";
    let api_err = |mid: &str, ts: &str| {
        let mut r = asst(json!([text("API Error: Overloaded")]), Some(mid), None, ts);
        r["isApiErrorMessage"] = json!(true);
        r
    };
    let recent = |a: &scan::Agg, now: f64| claudehq::payload::recent_unrecovered_error(&a.errors, a.last_good_reply, now).is_some();
    // a good reply sets last_good_reply
    let agg = scan(&[asst(json!([text("hello")]), Some("m1"), None, TS)]);
    assert_eq!(agg.last_good_reply, Some(Utc.with_ymd_and_hms(2026, 9, 1, 10, 0, 0).unwrap().timestamp() as f64));
    // the error record itself does not recover it
    assert_eq!(scan(&[api_err("m1", TS)]).last_good_reply, None);
    // an error after the last reply stays flagged
    let agg = scan(&[asst(json!([text("working on it")]), Some("m1"), None, TS), api_err("m2", LATER)]);
    assert_eq!(agg.errors.len(), 1);
    assert!(recent(&agg, agg.errors[0].0 + 60.0));
    // a good reply after the error marks recovery
    let agg = scan(&[api_err("m1", TS), asst(json!([text("sorry, here is the answer")]), Some("m2"), None, LATER)]);
    assert_eq!(agg.errors.len(), 1);
    assert!(!recent(&agg, agg.errors[0].0 + 60.0));
    // recovery does not reach back in time
    let agg = scan(&[asst(json!([text("earlier good turn")]), Some("m0"), None, TS), api_err("m1", LATER)]);
    assert!(recent(&agg, agg.errors[0].0 + 60.0));
    // the six-hour window still expires an unanswered error
    let agg = scan(&[api_err("m1", TS)]);
    assert!(!recent(&agg, agg.errors[0].0 + 6.0 * 3600.0 + 1.0));
}

#[test]
fn interrupts_are_not_prompts() {
    let agg = scan(&[
        user(json!("build it"), TS),
        user(json!([text("[Request interrupted by user]")]), TS),
        user(json!("[Request interrupted by user for tool use]"), TS),
    ]);
    assert_eq!(agg.prompt_count, 1);
    assert_eq!(agg.per_day[DAY].prompts, 1);
    assert_eq!(agg.first_prompt.as_deref(), Some("build it"));
}

// ---- fatigue: the scan-time busy-span collector ----------------------------

const BASE: f64 = 1789999200.0; // 2026-09-21T14:00:00Z
const H: f64 = 3600.0;
const M: f64 = 60.0;
const SID: &str = "00000000-0000-4000-8000-000000000000";

fn t(secs: f64) -> String {
    let dt = Utc.timestamp_opt(BASE as i64, 0).unwrap() + Duration::milliseconds((secs * 1000.0) as i64);
    py::iso(&dt).replace("+00:00", "Z")
}

fn fixture() -> Vec<Value> {
    vec![
        json!({"type": "user", "timestamp": t(0.0), "message": {"role": "user", "content": "fix the tests"}}),
        json!({"type": "user", "timestamp": t(-13.0 * 86400.0), "message": {"content": "old copied prompt"}}),
        json!({"type": "assistant", "timestamp": t(10.0), "message": {"content": [{"type": "tool_use", "id": "tu1", "name": "Bash", "input": {}}]}}),
        json!({"type": "user", "timestamp": t(50.0 * M), "message": {"content": [{"type": "tool_result", "tool_use_id": "tu1", "content": "ok"}]}}),
        json!({"type": "assistant", "timestamp": t(50.0 * M + 20.0), "message": {"content": [{"type": "text", "text": "done"}]}}),
        json!({"type": "system", "subtype": "turn_duration", "durationMs": 3030000, "timestamp": t(50.0 * M + 30.0)}),
        json!({"type": "system", "subtype": "away_summary", "timestamp": t(58.0 * M), "content": "x"}),
        json!({"type": "assistant", "timestamp": t(60.0 * M), "message": {"content": [{"type": "tool_use", "id": "tu2", "name": "AskUserQuestion", "input": {}}]}}),
        json!({"type": "attachment", "timestamp": t(70.0 * M)}),
        json!({"type": "user", "timestamp": t(100.0 * M), "message": {"content": [{"type": "tool_result", "tool_use_id": "tu2", "content": "yes"}]}}),
        json!({"type": "assistant", "timestamp": t(100.0 * M + 5.0), "message": {"content": [{"type": "text", "text": "ok"}]}}),
        json!({"type": "assistant", "timestamp": t(130.0 * M), "message": {"content": [{"type": "tool_use", "id": "tu3", "name": "Bash", "input": {}}]}}),
    ]
}

fn mins(agg: &scan::Agg) -> Vec<[f64; 2]> {
    agg.busy_spans.iter().map(|s| [py::round((s[0] - BASE) / 60.0, 3), py::round((s[1] - BASE) / 60.0, 3)]).collect()
}

#[test]
fn busy_spans_and_open_tool() {
    let agg = scan(&fixture());
    assert_eq!(mins(&agg), vec![[-18720.0, -18720.0], [0.0, 50.5], [60.0, 60.0], [100.0, 100.083], [130.0, 130.0]]);
    assert_eq!(agg.open_tool_since, Some(BASE + 130.0 * M));
    assert_eq!(agg.prompt_count, 2);
    assert_eq!(agg.activity_ts, vec![BASE, BASE - 13.0 * 86400.0, BASE + 10.0, BASE + 3020.0, BASE + 3600.0, BASE + 6005.0, BASE + 7800.0]);
    let mut days: Vec<&String> = agg.per_day.keys().collect();
    days.sort();
    assert_eq!(days, vec!["2026-09-08", "2026-09-21"]);
    let day = &agg.per_day["2026-09-21"];
    assert_eq!((day.prompts, day.tools, day.replies), (1, 3, 2));
    assert_eq!(day.tools_by_name.iter().map(|(k, v)| (k.as_str(), *v)).collect::<Vec<_>>(), vec![("Bash", 2), ("AskUserQuestion", 1)]);
}

#[test]
fn a_real_prompt_closes_orphaned_tools() {
    let mut recs = fixture();
    recs.push(json!({"type": "user", "timestamp": t(140.0 * M), "message": {"content": "next task"}}));
    let agg = scan(&recs);
    assert_eq!(agg.open_tool_since, None);
    let n = agg.busy_spans.len();
    assert_eq!(agg.busy_spans[n - 2..], [[BASE + 130.0 * M, BASE + 130.0 * M], [BASE + 140.0 * M, BASE + 140.0 * M]]);
}

#[test]
fn turn_durations() {
    let agg = scan(&[
        json!({"type": "system", "subtype": "turn_duration", "durationMs": 30.0 * 3600.0 * 1000.0, "timestamp": t(10.0 * H)}),
        json!({"type": "system", "subtype": "turn_duration", "durationMs": true, "timestamp": t(20.0 * H)}),
    ]);
    assert_eq!(agg.busy_spans, vec![[BASE + 4.0 * H, BASE + 10.0 * H]]);
    // A task-notification turn reports durationMs from the first turn's start;
    // only its own 40 s are work.
    let agg = scan(&[
        json!({"type": "user", "timestamp": t(0.0), "message": {"content": "build it"}}),
        json!({"type": "user", "isMeta": true, "timestamp": t(10.0 * M), "message": {"content": [{"type": "text", "text": "skill body"}]}}),
        json!({"type": "assistant", "timestamp": t(19.0 * M), "message": {"content": [{"type": "tool_use", "id": "w1", "name": "Workflow", "input": {}}]}}),
        json!({"type": "user", "timestamp": t(19.0 * M + 2.0), "message": {"content": [{"type": "tool_result", "tool_use_id": "w1", "content": "launched in background"}]}}),
        json!({"type": "system", "subtype": "turn_duration", "durationMs": 20 * 60 * 1000, "timestamp": t(20.0 * M)}),
        json!({"type": "user", "timestamp": t(3.0 * H), "message": {"content": "<task-notification> <task-id>w1</task-id> done"}}),
        json!({"type": "assistant", "timestamp": t(3.0 * H + 30.0), "message": {"content": [{"type": "text", "text": "it finished"}]}}),
        json!({"type": "system", "subtype": "turn_duration", "durationMs": (3.0 * H + 40.0) * 1000.0, "timestamp": t(3.0 * H + 40.0)}),
    ]);
    assert_eq!(agg.busy_spans, vec![[BASE, BASE + 20.0 * M], [BASE + 3.0 * H, BASE + 3.0 * H + 40.0]]);
    // A human wait inside the turn stays rest.
    let agg = scan(&[
        json!({"type": "user", "timestamp": t(0.0), "message": {"content": "improve it"}}),
        json!({"type": "assistant", "timestamp": t(54.0), "message": {"content": [{"type": "tool_use", "id": "q1", "name": "AskUserQuestion", "input": {}}]}}),
        json!({"type": "user", "timestamp": t(15.0 * M), "message": {"content": [{"type": "tool_result", "tool_use_id": "q1", "content": "yes"}]}}),
        json!({"type": "assistant", "timestamp": t(15.0 * M + 30.0), "message": {"content": [{"type": "text", "text": "done"}]}}),
        json!({"type": "system", "subtype": "turn_duration", "durationMs": 89 * 1000, "timestamp": t(15.0 * M + 35.0)}),
    ]);
    assert_eq!(agg.busy_spans, vec![[BASE, BASE + 54.0], [BASE + 15.0 * M, BASE + 15.0 * M + 35.0]]);
}

#[test]
fn another_sessions_message_after_a_slash_command() {
    let agg = scan(&[
        json!({"type": "user", "timestamp": t(0.0), "message": {"content": "ship it"}}),
        json!({"type": "system", "subtype": "turn_duration", "durationMs": 5 * 60 * 1000, "timestamp": t(5.0 * M)}),
        json!({"type": "user", "isMeta": true, "timestamp": t(6.0 * M), "message": {"content": "<local-command-caveat>Caveat</local-command-caveat>"}}),
        json!({"type": "user", "timestamp": t(6.0 * M), "message": {"content": "<command-name>/model</command-name>"}}),
        json!({"type": "user", "timestamp": t(6.0 * M), "message": {"content": "<local-command-stdout>Set model</local-command-stdout>"}}),
        json!({"type": "user", "isMeta": true, "timestamp": t(2.0 * H), "message": {"content": "Another Claude session sent a message: hi"}}),
        json!({"type": "system", "subtype": "turn_duration", "durationMs": (2.0 * H + 20.0) * 1000.0, "timestamp": t(2.0 * H + 20.0)}),
    ]);
    assert_eq!(agg.busy_spans, vec![[BASE, BASE + 6.0 * M], [BASE + 2.0 * H, BASE + 2.0 * H + 20.0]]);
    let f = fatigue::fatigue_for(SID, &agg.busy_spans, BASE + 2.0 * H + 21.0, None, &[]);
    assert_eq!(f["state"], "rested");
}

#[test]
fn a_tool_past_the_pair_cap_never_jumps() {
    for run_mins in [150.0, 240.0] {
        let recs = [
            json!({"type": "user", "timestamp": t(0.0), "message": {"content": "audit"}}),
            json!({"type": "assistant", "timestamp": t(10.0), "message": {"content": [{"type": "tool_use", "id": "a1", "name": "Agent", "input": {}}]}}),
            json!({"type": "user", "timestamp": t(run_mins * M), "message": {"content": [{"type": "tool_result", "tool_use_id": "a1", "content": "report"}]}}),
            json!({"type": "assistant", "timestamp": t(run_mins * M + 10.0), "message": {"content": [{"type": "text", "text": "done"}]}}),
            json!({"type": "system", "subtype": "turn_duration", "durationMs": (run_mins * 60.0 + 20.0) * 1000.0, "timestamp": t(run_mins * M + 20.0)}),
        ];
        let stamps: Vec<f64> = recs.iter().map(|r| py::ts(&py::parse_iso_utc(r["timestamp"].as_str().unwrap()).unwrap())).collect();
        let (mut prev, mut written) = (None::<i64>, usize::MAX);
        let mut agg = scan(&recs[..0]);
        let mut sec = 0.0;
        while sec < (run_mins + 5.0) * 60.0 {
            let now = BASE + sec;
            let n = stamps.iter().filter(|s| **s <= now).count();
            if n != written {
                agg = scan(&recs[..n]);
                written = n;
            }
            let ext = if n < recs.len() { agg.open_tool_since } else { None };
            let load = fatigue::fatigue_for(SID, &agg.busy_spans, now, ext, &[])["loadMins"].as_i64().unwrap();
            if let Some(p) = prev {
                assert!(load - p <= 1, "load jumped at {} s", sec);
            }
            prev = Some(load);
            sec += 30.0;
        }
        assert_eq!(agg.busy_spans, vec![[BASE, BASE + 10.0 + fatigue::MAX_PAIR_SECS], [BASE + run_mins * M, BASE + run_mins * M + 20.0]]);
    }
}

#[test]
fn spans_are_capped_and_missing_files_have_the_keys() {
    let recs: Vec<Value> = (0..150).map(|i| json!({"type": "assistant", "timestamp": t(10.0 * M * i as f64), "message": {"content": [text("x")]}})).collect();
    let agg = scan(&recs);
    assert_eq!(agg.busy_spans.len(), fatigue::MAX_SPANS);
    assert_eq!(*agg.busy_spans.last().unwrap(), [BASE + 1490.0 * M, BASE + 1490.0 * M]);
    let missing = scan::scan_claude(Path::new("/nonexistent/x.jsonl"));
    assert!(missing.busy_spans.is_empty() && missing.open_tool_since.is_none());
}

// ---- work signals -------------------------------------------------------------

fn ws_ts(h: u32, m: u32, s: u32, day: &str) -> String {
    format!("{}T{:02}:{:02}:{:02}.000Z", day, h, m, s)
}

fn use_(tid: &str, name: &str, inp: Value, ts: String) -> Value {
    json!({"type": "assistant", "timestamp": ts, "message": {"content": [{"type": "tool_use", "id": tid, "name": name, "input": inp}]}})
}

fn result(tid: &str, out: &str, ts: String, err: bool) -> Value {
    let mut b = json!({"type": "tool_result", "tool_use_id": tid, "content": out});
    if err {
        b["is_error"] = json!(true);
    }
    json!({"type": "user", "timestamp": ts, "message": {"content": [b]}})
}

fn synthetic() -> Vec<Value> {
    let d = DAY;
    vec![
        use_("t1", "Bash", json!({"command": "cd app && python3 -m pytest -q"}), ws_ts(10, 0, 0, d)),
        result("t1", "12 passed in 0.4s", ws_ts(10, 0, 5, d), false),
        use_("t2", "Bash", json!({"command": "cargo test"}), ws_ts(10, 1, 0, d)),
        result("t2", "test result: FAILED. 3 passed; 1 failed", ws_ts(10, 1, 9, d), false),
        use_("t3", "Edit", json!({"file_path": "/r/tests/test_x.py", "old_string": "a", "new_string": "b"}), ws_ts(10, 2, 0, d)),
        result("t3", "ok", ws_ts(10, 2, 1, d), false),
        use_("d1", "Write", json!({"file_path": "/r/README.md", "content": "# hi"}), ws_ts(10, 3, 0, d)),
        result("d1", "ok", ws_ts(10, 3, 1, d), false),
        use_("d2", "Edit", json!({"file_path": "/r/docs/guide.html", "old_string": "a", "new_string": "b"}), ws_ts(10, 4, 0, d)),
        result("d2", "ok", ws_ts(10, 4, 1, d), false),
        use_("r1", "MultiEdit", json!({"file_path": "/r/src/a.py", "edits": [{"old_string": "a", "new_string": "b"}, {"old_string": "c", "new_string": "d"}]}), ws_ts(10, 5, 0, d)),
        result("r1", "ok", ws_ts(10, 5, 1, d), false),
        use_("r2", "Edit", json!({"file_path": "/r/src/a.py", "old_string": "x = 1\ny = 2\nz = 3\n", "new_string": "x = 9\ny = 2\nz = 8\n"}), ws_ts(10, 6, 0, d)),
        result("r2", "ok", ws_ts(10, 6, 1, d), false),
        use_("b1", "Edit", json!({"file_path": "/r/src/b.rs", "old_string": "fn a", "new_string": "fn b"}), ws_ts(10, 7, 0, d)),
        result("b1", "ok", ws_ts(10, 7, 1, d), false),
        use_("b2", "Write", json!({"file_path": "/r/src/c.js", "content": "var a;"}), ws_ts(10, 8, 0, d)),
        result("b2", "ok", ws_ts(10, 8, 1, d), false),
        use_("v1", "Bash", json!({"command": "git diff HEAD~1"}), ws_ts(10, 9, 0, d)),
        result("v1", "diff --git", ws_ts(10, 9, 1, d), false),
        use_("v2", "Bash", json!({"command": "gh pr view 12"}), ws_ts(10, 10, 0, d)),
        result("v2", "title", ws_ts(10, 10, 1, d), false),
        use_("e0", "Grep", json!({"pattern": "x"}), ws_ts(10, 11, 0, d)),
        result("e0", "found", ws_ts(10, 11, 1, d), false),
        use_("e1", "Glob", json!({"pattern": "x"}), ws_ts(10, 12, 0, d)),
        result("e1", "found", ws_ts(10, 12, 1, d), false),
        use_("e2", "Read", json!({"pattern": "x"}), ws_ts(10, 13, 0, d)),
        result("e2", "found", ws_ts(10, 13, 1, d), false),
        use_("g1", "Bash", json!({"command": "make it"}), ws_ts(10, 20, 0, d)),
        result("g1", "boom", ws_ts(10, 20, 1, d), true),
        use_("g2", "Bash", json!({"command": "make it go"}), ws_ts(10, 21, 0, d)),
        result("g2", "ok", ws_ts(10, 21, 1, d), false),
        use_("g3", "Read", json!({"file_path": "/r/log.txt"}), ws_ts(10, 22, 0, d)),
        result("g3", "Traceback (most recent call last):\n  File x", ws_ts(10, 22, 1, d), false),
        use_("n1", "Bash", json!({"command": "echo pytest"}), ws_ts(10, 23, 0, d)),
        result("n1", "pytest", ws_ts(10, 23, 1, d), false),
    ]
}

fn ws_counts(aggs: &[&scan::Agg], days: &[&str]) -> indexmap::IndexMap<String, indexmap::IndexMap<String, i64>> {
    let days: Vec<String> = days.iter().map(|d| d.to_string()).collect();
    worksignals::merge_days(aggs.iter().map(|a| a.ws_days.as_ref()), &days)
}

#[test]
fn synthetic_entries_give_the_expected_counts() {
    let agg = scan(&synthetic());
    let got = &ws_counts(&[&agg], &[DAY])[DAY];
    let want = [("tests", 3), ("refactor", 2), ("docs", 2), ("review", 2), ("debug", 2), ("explore", 4), ("build", 2), ("tests_green", 1), ("focus_long", 0)];
    assert_eq!(got.iter().map(|(k, v)| (k.as_str(), *v)).collect::<Vec<_>>(), want.to_vec());
    let blob = claudehq::pyjson::dumps(&serde_json::to_value(agg.ws_days.as_ref().map(|d| format!("{:?}", d))).unwrap());
    for leak in ["README", "pytest", "git", "Grep", "/r/", ".py", "Traceback"] {
        assert!(!blob.contains(leak), "{}", leak);
    }
    let two = scan(&synthetic());
    let got = &ws_counts(&[&agg, &two], &[DAY])[DAY];
    assert_eq!((got["explore"], got["tests_green"]), (8, 2));
}

#[test]
fn focus_spans_credit_the_end_day_and_break_on_gaps() {
    let run = |sh: u32, sm: u32, minutes: u32, step: u32| -> Vec<Value> {
        (0..=minutes)
            .step_by(step as usize)
            .map(|i| {
                let mut tot = sh * 60 + sm + i;
                let mut day = DAY;
                if tot >= 24 * 60 {
                    tot -= 24 * 60;
                    day = "2026-09-02";
                }
                json!({"type": "user", "timestamp": ws_ts(tot / 60, tot % 60, 0, day), "message": {"content": "go"}})
            })
            .collect()
    };
    let agg = scan(&run(23, 40, 50, 5));
    let got = ws_counts(&[&agg], &[DAY, "2026-09-02"]);
    assert_eq!((got[DAY]["focus_long"], got["2026-09-02"]["focus_long"]), (0, 1));
    let mut gap = run(9, 0, 30, 5);
    gap.extend(run(9, 45, 30, 5));
    assert_eq!(ws_counts(&[&scan(&gap)], &[DAY])[DAY]["focus_long"], 0);
    assert_eq!(ws_counts(&[&scan(&run(9, 0, 100, 10))], &[DAY])[DAY]["focus_long"], 1);
}

#[test]
fn a_warm_cache_and_an_mtime_bump_give_identical_totals() {
    let d = tempfile::tempdir().unwrap();
    let mut p = Paths::new(d.path(), &d.path().join("home"));
    p.projects_dir = d.path().join("projects");
    p.cursor_projects_dir = d.path().join("cursor");
    let app = Arc::new(App::new(p));
    let path: PathBuf = app.paths.projects_dir.join("proj").join("a.jsonl");
    write(&path, &synthetic());
    let a1 = app.scan_file(&path).unwrap();
    let a2 = app.scan_file(&path).unwrap();
    assert!(Arc::ptr_eq(&a1, &a2));
    let f = std::fs::File::options().write(true).open(&path).unwrap();
    f.set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(5)).unwrap();
    let a3 = app.scan_file(&path).unwrap();
    assert!(!Arc::ptr_eq(&a1, &a3));
    assert_eq!(ws_counts(&[&a1], &[DAY]), ws_counts(&[&a3], &[DAY]));
}
