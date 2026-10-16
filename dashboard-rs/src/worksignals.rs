//! Work signals (HQ 2.5): classify how you work, locally, from transcripts, into
//! per-UTC-day COUNTS. Nothing here keeps or returns a command, a file name, a
//! path or any text -- only integers per category (port of worksignals.py).

use crate::difflib;
use crate::py;
use chrono::{Duration, NaiveDate};
use indexmap::IndexMap;
use regex::Regex;
use serde_json::Value;
use std::sync::LazyLock;

pub const SKILL_CATS: [&str; 7] = ["tests", "refactor", "docs", "review", "debug", "explore", "build"];
pub const KEYS: [&str; 9] = ["tests", "refactor", "docs", "review", "debug", "explore", "build", "tests_green", "focus_long"];
pub const MAX_N: i64 = 200;
pub const FOCUS_MIN_SECS: f64 = 45.0 * 60.0;
pub const FOCUS_GAP_SECS: f64 = 10.0 * 60.0;

const EDIT_TOOLS: [&str; 4] = ["Edit", "Write", "MultiEdit", "NotebookEdit"];
const EXPLORE_TOOLS: [&str; 4] = ["Grep", "Glob", "Read", "LS"];

static TEST_CMD_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"^(?:(?:env\s+)?(?:[A-Z_][A-Z0-9_]*=\S*\s+)*)",
        r"(?:(?:uv|poetry|pipenv|hatch|pdm)\s+run\s+)?",
        r"(?:",
        r"pytest\b|py\.test\b|tox\b|nox\b|",
        r"python[0-9.]*\s+(?:-[a-zA-Z]\s+)*(?:-m\s+(?:pytest|unittest|nose2?)\b|\S*test\S*\.py\b|manage\.py\s+test\b)|",
        r"cargo\s+(?:\+\S+\s+)?(?:test|nextest)\b|",
        r"go\s+test\b|",
        r"(?:npm|pnpm|yarn|bun)\s+(?:run\s+)?test(?::\S+)?\b|",
        r"(?:npx\s+|bunx\s+)?(?:jest|vitest|mocha|ava|karma|playwright\s+test|cypress\s+run)\b|",
        r"deno\s+test\b|bun\s+test\b|",
        r"(?:\./)?gradlew?\s+(?:\S+\s+)*test\b|mvn\s+(?:\S+\s+)*(?:test|verify)\b|",
        r"rspec\b|bundle\s+exec\s+(?:rspec|rake\s+test)\b|rake\s+test\b|",
        r"phpunit\b|(?:\./)?vendor/bin/phpunit\b|",
        r"dotnet\s+test\b|swift\s+test\b|mix\s+test\b|ctest\b|",
        r"make\s+(?:\S+\s+)*(?:test|check)\b",
        r")"
    ))
    .unwrap()
});
static SEG_SPLIT_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s*(?:&&|\|\||;|\||\n)\s*").unwrap());
static LEAD_STRIP_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(?:\(|\{|time\s+|timeout\s+\d+\S*\s+|sudo\s+|exec\s+)+").unwrap());
static REVIEW_CMD_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:gh\s+pr\s+(?:view|diff|review|checks)\b|git\s+(?:-C\s+\S+\s+|--no-pager\s+|-c\s+\S+\s+)*(?:diff|log|show|blame|range-diff)\b)").unwrap()
});
static DEBUG_CMD_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:pdb\b|gdb\b|lldb\b|python[0-9.]*\s+-m\s+pdb\b|node\s+inspect\b|strace\b|dtruss\b)").unwrap()
});
static DEBUG_ENV_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b(?:RUST_BACKTRACE=(?:1|full)|PYTHONFAULTHANDLER=1|NODE_DEBUG=)").unwrap());
static FAIL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"\bFAILED\b|\bFAIL\b|\bFAILURES?\b|",
        r"\b[1-9]\d*\s+(?:failed|failing|failures?|errors?)\b|",
        r"\bfailures=[1-9]|\berrors=[1-9]|",
        r"test result: FAILED|Tests?:\s+[1-9]\d*\s+failed|",
        r"Traceback \(most recent call last\)|panicked at|\bERROR:\s"
    ))
    .unwrap()
});
static TRACE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"Traceback \(most recent call last\)|panicked at |stack backtrace:|",
        r"Uncaught \w*Error|Segmentation fault|\bat \S+ \(\S+:\d+:\d+\)|Exception in thread "
    ))
    .unwrap()
});
const DOC_EXT: [&str; 5] = [".md", ".mdx", ".rst", ".txt", ".adoc"];
static TEST_FILE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"(?:^|/)(?:tests?|__tests__|spec|specs|testing)/|",
        r"(?:^|/)test_[^/]+$|_test\.[a-z0-9]+$|_tests\.[a-z0-9]+$|",
        r"\.(?:test|spec)\.[a-z0-9]+$|(?:^|/)[^/]*Tests?\.[a-zA-Z0-9]+$|conftest\.py$"
    ))
    .unwrap()
});

fn segments(cmd: &str) -> Vec<String> {
    SEG_SPLIT_RE
        .split(cmd)
        .map(|seg| LEAD_STRIP_RE.replacen(py::strip(seg), 1, "").into_owned())
        .filter(|s| !s.is_empty())
        .collect()
}

pub fn is_test_command(cmd: &str) -> bool {
    if py::strip(cmd).is_empty() {
        return false;
    }
    segments(cmd).iter().any(|s| TEST_CMD_RE.is_match(s))
}

pub fn is_review_command(cmd: &str) -> bool {
    segments(cmd).iter().any(|s| REVIEW_CMD_RE.is_match(s))
}

pub fn is_debug_command(cmd: &str) -> bool {
    DEBUG_ENV_RE.is_match(cmd) || segments(cmd).iter().any(|s| DEBUG_CMD_RE.is_match(s))
}

/// "test", "doc" or "code" for an edited file path ("" when unknown).
pub fn file_kind(path: Option<&Value>) -> &'static str {
    let Some(Value::String(p)) = path else { return "" };
    if py::strip(p).is_empty() {
        return "";
    }
    let p = py::strip(p).replace('\\', "/");
    let low = p.to_lowercase();
    if TEST_FILE_RE.is_match(&p) {
        return "test";
    }
    if DOC_EXT.iter().any(|e| low.ends_with(e)) || low.contains("/docs/") || low.starts_with("docs/") || low.contains("/doc/") {
        return "doc";
    }
    "code"
}

/// (old or "").splitlines() -- Err where Python would raise (a truthy non-str).
fn lines_of(v: Option<&Value>) -> Result<Vec<&str>, ()> {
    match v {
        Some(Value::String(s)) => Ok(py::splitlines(s)),
        Some(x) if py::truthy(x) => Err(()),
        _ => Ok(Vec::new()),
    }
}

fn is_refactor_edit(name: &str, inp: &serde_json::Map<String, Value>) -> Result<bool, ()> {
    if name == "MultiEdit" {
        return Ok(matches!(inp.get("edits"), Some(Value::Array(e)) if e.len() >= 2));
    }
    if name == "Edit" {
        if inp.get("replace_all") == Some(&Value::Bool(true)) {
            return Ok(true);
        }
        let a = lines_of(inp.get("old_string"))?;
        let b = lines_of(inp.get("new_string"))?;
        return Ok(difflib::changed_regions(&a, &b) >= 2);
    }
    Ok(false)
}

fn result_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(a)) => {
            let mut parts = Vec::new();
            for b in a {
                match b {
                    Value::Object(o) => {
                        if let Some(Value::String(t)) = o.get("text") {
                            parts.push(t.as_str());
                        }
                    }
                    Value::String(s) => parts.push(s.as_str()),
                    _ => {}
                }
            }
            parts.join("\n")
        }
        _ => String::new(),
    }
}

pub type WsDays = IndexMap<String, IndexMap<String, i64>>;

/// Per-transcript scratch state; never stored past the scan.
#[derive(Default)]
pub struct WsState {
    pending: IndexMap<String, (String, String, bool)>,
    errored: IndexMap<String, bool>,
    ts: Vec<f64>,
}

fn bump(days: &mut WsDays, day: &str, key: &str) {
    *days.entry(day.to_string()).or_default().entry(key.to_string()).or_insert(0) += 1;
}

/// Classify one record into days[day]. Tool results are credited to the day of
/// the call they answer. Never panics.
pub fn classify_entry(entry: &Value, day: &str, days: &mut WsDays, state: &mut WsState) {
    let Value::Object(e) = entry else { return };
    let _ = (|| -> Result<(), ()> {
        let typ = e.get("type");
        let content = match e.get("message") {
            Some(Value::Object(m)) => m.get("content"),
            _ => None,
        };
        let Some(Value::Array(blocks)) = content else { return Ok(()) };
        if typ == Some(&Value::from("assistant")) {
            for b in blocks {
                if let Value::Object(b) = b {
                    if b.get("type") == Some(&Value::from("tool_use")) {
                        classify_use(b, day, days, state)?;
                    }
                }
            }
        } else if typ == Some(&Value::from("user")) {
            for b in blocks {
                if let Value::Object(b) = b {
                    if b.get("type") == Some(&Value::from("tool_result")) {
                        classify_result(b, days, state);
                    }
                }
            }
        }
        Ok(())
    })();
}

fn classify_use(b: &serde_json::Map<String, Value>, day: &str, days: &mut WsDays, state: &mut WsState) -> Result<(), ()> {
    let name = b.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let empty = serde_json::Map::new();
    let inp = match b.get("input") {
        Some(Value::Object(o)) => o,
        _ => &empty,
    };
    let tid = b.get("id").and_then(|v| v.as_str());
    if !name.is_empty() && state.errored.shift_remove(name).unwrap_or(false) {
        bump(days, day, "debug");
    }
    state.errored.clear();
    let mut is_test_run = false;
    if name == "Bash" {
        let cmd = inp.get("command").and_then(|v| v.as_str()).unwrap_or("");
        if is_test_command(cmd) {
            bump(days, day, "tests");
            is_test_run = true;
        }
        if is_review_command(cmd) {
            bump(days, day, "review");
        }
        if is_debug_command(cmd) {
            bump(days, day, "debug");
        }
    } else if EXPLORE_TOOLS.contains(&name) {
        bump(days, day, "explore");
    } else if EDIT_TOOLS.contains(&name) {
        // inp.get("file_path") or inp.get("notebook_path") or inp.get("path")
        let p = ["file_path", "notebook_path", "path"]
            .iter()
            .filter_map(|k| inp.get(*k))
            .find(|v| py::truthy(v))
            .or_else(|| inp.get("path"));
        match file_kind(p) {
            "test" => bump(days, day, "tests"),
            "doc" => bump(days, day, "docs"),
            "code" => {
                let refactor = is_refactor_edit(name, inp)?;
                bump(days, day, if refactor { "refactor" } else { "build" });
            }
            _ => {}
        }
    }
    if let Some(tid) = tid {
        if state.pending.len() > 500 {
            state.pending.clear();
        }
        state.pending.insert(tid.to_string(), (name.to_string(), day.to_string(), is_test_run));
    }
    Ok(())
}

fn classify_result(b: &serde_json::Map<String, Value>, days: &mut WsDays, state: &mut WsState) {
    let Some(tid) = b.get("tool_use_id").and_then(|v| v.as_str()) else { return };
    let Some((name, day, is_test_run)) = state.pending.shift_remove(tid) else { return };
    let text = result_text(b.get("content"));
    let is_err = b.get("is_error") == Some(&Value::Bool(true));
    if is_err && !name.is_empty() {
        state.errored.insert(name.clone(), true);
    }
    if TRACE_RE.is_match(&py::take_chars(&text, 20000)) {
        bump(days, &day, "debug");
    }
    if is_test_run && !is_err && !FAIL_RE.is_match(py::last_chars(&text, 20000)) {
        bump(days, &day, "tests_green");
    }
}

/// [(start, end)] runs of >= 45 min with no gap over 10 min.
pub fn focus_spans(timestamps: &[f64]) -> Vec<(f64, f64)> {
    let mut ts: Vec<f64> = timestamps.to_vec();
    ts.sort_by(|a, b| a.total_cmp(b));
    let mut out = Vec::new();
    if ts.is_empty() {
        return out;
    }
    let (mut start, mut prev) = (ts[0], ts[0]);
    for &t in &ts[1..] {
        if t - prev > FOCUS_GAP_SECS {
            if prev - start >= FOCUS_MIN_SECS {
                out.push((start, prev));
            }
            start = t;
        }
        prev = t;
    }
    if prev - start >= FOCUS_MIN_SECS {
        out.push((start, prev));
    }
    out
}

fn utc_day(t: f64) -> String {
    py::from_ts(t).map(|d| py::date_iso(&d.date_naive())).unwrap_or_default()
}

/// The scan hook: classify one record into ws_days[utc_day] and remember its
/// time for focus_long.
pub fn observe(entry: &Value, ws_days: &mut Option<WsDays>, state: &mut WsState) {
    let Value::Object(e) = entry else { return };
    let Some(Value::String(t)) = e.get("timestamp") else { return };
    let Some(dt) = py::parse_iso_utc(t) else { return };
    if matches!(e.get("type").and_then(|v| v.as_str()), Some("user" | "assistant")) {
        state.ts.push(py::ts(&dt));
    }
    let day = py::date_iso(&dt.date_naive());
    let days = ws_days.get_or_insert_with(IndexMap::new);
    days.entry(day.clone()).or_default();
    classify_entry(entry, &day, days, state);
}

/// End of one transcript's scan: credit focus_long and drop empty days.
pub fn finish(ws_days: &mut Option<WsDays>, state: &mut WsState) {
    let days = ws_days.get_or_insert_with(IndexMap::new);
    for (_start, end) in focus_spans(&state.ts) {
        bump(days, &utc_day(end), "focus_long");
    }
    days.retain(|_, ws| !ws.is_empty());
    *state = WsState::default();
}

/// The last n UTC days, oldest first, ending today.
pub fn last_days(n: i64, today: NaiveDate) -> Vec<String> {
    let n = n.clamp(1, 60);
    (0..n).rev().map(|i| py::date_iso(&(today - Duration::days(i)))).collect()
}

/// {day: {key: n}} summed over per-file ws_days, every KEY present.
pub fn merge_days<'a>(aggs: impl Iterator<Item = Option<&'a WsDays>>, days: &[String]) -> IndexMap<String, IndexMap<String, i64>> {
    let mut out: IndexMap<String, IndexMap<String, i64>> =
        days.iter().map(|d| (d.clone(), KEYS.iter().map(|k| (k.to_string(), 0)).collect())).collect();
    for wd in aggs.flatten() {
        for d in days {
            if let Some(ws) = wd.get(d) {
                for k in KEYS {
                    if let Some(v) = ws.get(k) {
                        if *v > 0 {
                            *out.get_mut(d).unwrap().get_mut(k).unwrap() += v;
                        }
                    }
                }
            }
        }
    }
    out
}

fn clean7(counts: &IndexMap<String, i64>) -> IndexMap<String, i64> {
    SKILL_CATS.iter().map(|k| (k.to_string(), counts.get(*k).copied().unwrap_or(0).clamp(0, MAX_N))).collect()
}

/// EXACTLY the seven skill categories per day, each clamped to 0..MAX_N.
pub fn report_counts(days: &IndexMap<String, IndexMap<String, i64>>) -> IndexMap<String, IndexMap<String, i64>> {
    days.iter().map(|(d, c)| (d.clone(), clean7(c))).collect()
}

/// report_counts for one day's raw JSON counts (bools and non-ints read as 0).
pub fn report_counts_value(counts: &Value) -> IndexMap<String, i64> {
    let mut m = IndexMap::new();
    if let Value::Object(o) = counts {
        for (k, v) in o {
            let n = match v {
                Value::Bool(_) => 0,
                _ => py::py_int(v).unwrap_or(0),
            };
            m.insert(k.clone(), n);
        }
    }
    clean7(&m)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn helpers() {
        assert!(is_test_command("FOO=1 npm test"));
        assert!(is_test_command("cd backend-rs && cargo test && cargo clippy"));
        assert!(is_test_command("python3 -m unittest discover -s tests"));
        assert!(!is_test_command("grep pytest setup.cfg"));
        assert!(!is_test_command(""));
        assert!(is_review_command("git --no-pager log --oneline"));
        assert!(!is_review_command("git commit -m diff"));
        assert_eq!(file_kind(Some(&json!("src/foo.test.ts"))), "test");
        assert_eq!(file_kind(Some(&json!("notes.TXT"))), "doc");
        assert_eq!(file_kind(Some(&json!("a/b.py"))), "code");
        assert_eq!(file_kind(Some(&json!(""))), "");
        assert_eq!(file_kind(None), "");
    }

    #[test]
    fn focus() {
        assert_eq!(focus_spans(&[]), vec![]);
        assert_eq!(focus_spans(&[0.0, 600.0, 1200.0, 1800.0, 2400.0, 2700.0]), vec![(0.0, 2700.0)]);
        assert_eq!(focus_spans(&[0.0, 2600.0]), vec![]);
    }

    #[test]
    fn report_keeps_exactly_seven() {
        let one: IndexMap<String, i64> = KEYS.iter().map(|k| (k.to_string(), 4)).collect();
        let mut two = IndexMap::new();
        two.insert("tests".to_string(), 999);
        two.insert("focus_long".to_string(), 3);
        let mut days = IndexMap::new();
        days.insert("2026-09-01".to_string(), one);
        days.insert("2026-09-02".to_string(), two);
        let many = report_counts(&days);
        for d in many.values() {
            assert_eq!(d.keys().cloned().collect::<Vec<_>>(), SKILL_CATS.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        }
        assert_eq!(many["2026-09-02"]["tests"], MAX_N);
        assert_eq!(report_counts_value(&json!({"tests": -3, "docs": "x", "build": true}))["tests"], 0);
    }

    #[test]
    fn last_days_window() {
        let today = NaiveDate::from_ymd_opt(2026, 10, 10).unwrap();
        assert_eq!(last_days(3, today), vec!["2026-10-08", "2026-10-09", "2026-10-10"]);
        assert_eq!(last_days(7, today).len(), 7);
    }
}
