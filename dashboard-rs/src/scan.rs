//! The single per-file transcript pass (dashboard._scan_file_uncached and the
//! Cursor variant). Everything downstream -- cards, season, analytics, search
//! snippets aside -- reads the aggregate built here; App::scan_file caches it by
//! (mtime, size).

use crate::cost;
use crate::fatigue::{self, MAX_PAIR_SECS};
use crate::py::{self, Dt};
use crate::worksignals::{self, WsDays, WsState};
use indexmap::{IndexMap, IndexSet};
use regex::Regex;
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

/// Python's \s (str.isspace) as a regex class body.
pub const WS: &str = r"\s\x1c-\x1f";

static PASTED_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)<pasted_content\b[^>]*>.*?</pasted_content>").unwrap());
pub static ARTIFACT_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"https://claude\.ai/[^\s)"']*artifact[^\s)"']*"#).unwrap());
static PR_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"https://github\.com/[^\s)"']+/pull/(\d+)"#).unwrap());
pub static WS_RUN_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(&format!("[{}]+", WS)).unwrap());
static MD_LINK_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\[([^\]]+)\]\([^)]*\)").unwrap());
static MD_IMG_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"!\[([^\]]*)\]\([^)]*\)").unwrap());
static MD_HEAD_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(&format!(r"(?m)^[{}]{{0,3}}#{{1,6}}[{}]*", WS, WS)).unwrap());
static MD_LIST_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(&format!(r"(?m)^[{}]{{0,3}}[-*+][{}]+", WS, WS)).unwrap());
static MD_BOLD_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\*\*([^*]+)\*\*").unwrap());
static MD_EM_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\*([^*]+)\*").unwrap());
static MD_UND_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"__([^_]+)__").unwrap());

const NON_HUMAN_PREFIXES: [&str; 6] = [
    "<command-",
    "<local-command",
    "<system-reminder",
    "Caveat:",
    "This session is being continued",
    "[Request interrupted",
];

pub const ERROR_SIGS: [&str; 14] = [
    "organization has disabled", "disabled claude", "rate limit", "overloaded",
    "invalid api key", "credit balance", "billing", "quota", "insufficient",
    "authentication_error", "spend limit", "login expired", "/login",
    "prompt is too long",
];
const GENERIC_ERROR_SIG: &str = "api_error";
const ASK_TEXT_MAX: usize = 140;
const EDIT_TOOLS: [&str; 4] = ["Edit", "Write", "NotebookEdit", "MultiEdit"];

/// Reduce markdown to readable plain text.
pub fn strip_markdown(text: &str) -> String {
    if text.is_empty() {
        return String::new();
    }
    let t = MD_LINK_RE.replace_all(text, "${1}");
    let t = MD_IMG_RE.replace_all(&t, "${1}");
    let t = t.replace("```", " ").replace('`', "");
    let t = MD_HEAD_RE.replace_all(&t, "");
    let t = MD_LIST_RE.replace_all(&t, "");
    let t = MD_BOLD_RE.replace_all(&t, "${1}");
    let t = MD_EM_RE.replace_all(&t, "${1}");
    let t = MD_UND_RE.replace_all(&t, "${1}");
    let t = t.replace('>', "");
    py::strip(&WS_RUN_RE.replace_all(&t, " ")).to_string()
}

/// Human-authored text from a user message's content (a string, or text blocks
/// of a list). None where Python would raise (a truthy non-string text field).
pub fn human_text(content: Option<&Value>) -> Option<String> {
    match content {
        Some(Value::String(s)) => Some(s.clone()),
        Some(Value::Array(blocks)) => {
            let mut parts: Vec<String> = Vec::new();
            for b in blocks {
                match b {
                    Value::String(s) => parts.push(s.clone()),
                    Value::Object(o) if o.get("type") == Some(&Value::from("text")) => match o.get("text") {
                        Some(Value::String(s)) => parts.push(s.clone()),
                        Some(v) if py::truthy(v) => return None,
                        _ => parts.push(String::new()),
                    },
                    _ => {}
                }
            }
            let joined = parts.into_iter().filter(|p| !p.is_empty()).collect::<Vec<_>>().join(" ");
            Some(py::strip(&joined).to_string())
        }
        _ => Some(String::new()),
    }
}

/// Strip pasted-content noise and normalise whitespace for a human prompt.
pub fn clean_prompt(content: Option<&Value>) -> Option<String> {
    let text = match content {
        Some(Value::String(s)) => s.clone(),
        other => human_text(other)?,
    };
    if text.is_empty() {
        return Some(String::new());
    }
    let t = PASTED_RE.replace_all(&text, " [pasted content] ");
    Some(py::strip(&WS_RUN_RE.replace_all(&t, " ")).to_string())
}

pub fn is_real_human_prompt(content: Option<&Value>) -> Option<bool> {
    let s = human_text(content)?;
    let s = py::lstrip(&s);
    if s.is_empty() {
        return Some(false);
    }
    Some(!NON_HUMAN_PREFIXES.iter().any(|p| s.starts_with(p)))
}

fn is_interrupt_marker(content: Option<&Value>) -> Option<bool> {
    Some(py::lstrip(&human_text(content)?).starts_with("[Request interrupted"))
}

/// A short human label for an in-flight tool_use block.
pub fn tool_label(b: &Map<String, Value>) -> String {
    let name = tool_name(b);
    let mut detail = String::new();
    if let Some(Value::Object(inp)) = b.get("input") {
        for key in ["description", "command", "file_path", "path", "pattern", "query", "url"] {
            if let Some(Value::String(v)) = inp.get(key) {
                if !py::strip(v).is_empty() {
                    detail = py::strip(v).to_string();
                    break;
                }
            }
        }
    }
    if detail.is_empty() {
        py::truncate(&name, 80)
    } else {
        py::truncate(&format!("{}: {}", name, detail), 80)
    }
}

/// b.get("name") or "Tool" (a non-string name is shown as its JSON text).
fn tool_name(b: &Map<String, Value>) -> String {
    match b.get("name") {
        Some(Value::String(s)) if !s.is_empty() => s.clone(),
        Some(v) if py::truthy(v) => v.to_string(),
        _ => "Tool".to_string(),
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Link {
    pub kind: &'static str,
    pub url: String,
    pub label: String,
}

impl Link {
    pub fn to_json(&self) -> Value {
        json!({"type": self.kind, "url": self.url, "label": self.label})
    }
}

fn extract_links(text: &str, links: &mut Vec<Link>, seen: &mut IndexSet<String>) {
    if text.is_empty() {
        return;
    }
    for m in ARTIFACT_RE.find_iter(text) {
        let url = m.as_str().to_string();
        if seen.insert(url.clone()) {
            links.push(Link { kind: "artifact", url, label: "artifact".into() });
        }
    }
    for c in PR_RE.captures_iter(text) {
        let url = c[0].to_string();
        if seen.insert(url.clone()) {
            links.push(Link { kind: "pr", url, label: format!("PR #{}", &c[1]) });
        }
    }
}

fn match_error(text: &str) -> Option<&'static str> {
    if text.is_empty() {
        return None;
    }
    let low = text.to_lowercase();
    ERROR_SIGS.iter().find(|s| low.contains(*s)).copied()
}

/// Error signature for a transcript record that IS an API error, else None.
pub fn record_error_sig(o: &Value) -> Option<String> {
    let Value::Object(o) = o else { return None };
    let mut parts: Vec<String> = Vec::new();
    match o.get("type").and_then(|v| v.as_str()) {
        Some("system") => {
            if o.get("subtype") != Some(&Value::from("api_error")) {
                return None;
            }
            if let (Some(ra), Some(mr)) = (o.get("retryAttempt").and_then(py::strict_int), o.get("maxRetries").and_then(py::strict_int)) {
                if ra < mr {
                    return None;
                }
            }
            if let Some(Value::String(c)) = o.get("content") {
                parts.push(c.clone());
            }
            match o.get("error") {
                None | Some(Value::Null) => {}
                Some(Value::String(s)) => parts.push(s.clone()),
                Some(v) => parts.push(crate::pyjson::dumps(v)),
            }
        }
        Some("assistant") => {
            if o.get("isApiErrorMessage") != Some(&Value::Bool(true)) {
                return None;
            }
            let content = match o.get("message") {
                Some(Value::Object(m)) => m.get("content"),
                _ => None,
            };
            parts.push(human_text(content).unwrap_or_default());
            if let Some(Value::String(s)) = o.get("error") {
                parts.push(s.clone());
            }
        }
        _ => return None,
    }
    let joined = parts.into_iter().filter(|p| !p.is_empty()).collect::<Vec<_>>().join(" ");
    Some(match_error(&joined).unwrap_or(GENERIC_ERROR_SIG).to_string())
}

fn opens_turn(content: Option<&Value>) -> bool {
    match content {
        Some(Value::String(s)) => !py::strip(s).is_empty(),
        Some(Value::Array(a)) => a.iter().any(|b| matches!(b, Value::Object(o) if o.get("type") != Some(&Value::from("tool_result")))),
        _ => false,
    }
}

fn permission_gated(name: &str, mode: Option<&str>) -> bool {
    if name.is_empty() || matches!(mode, None | Some("auto" | "bypassPermissions" | "dontAsk")) {
        return false;
    }
    if name.starts_with("mcp__") || name == "Bash" || name == "WebFetch" {
        return true;
    }
    mode == Some("default") && EDIT_TOOLS.contains(&name)
}

fn ask_kind(name: &str) -> &'static str {
    if name == "ExitPlanMode" {
        "plan"
    } else {
        "question"
    }
}

fn ask_text(b: &Map<String, Value>) -> String {
    if let Some(Value::Object(inp)) = b.get("input") {
        if let Some(Value::Array(qs)) = inp.get("questions") {
            if let Some(Value::Object(q0)) = qs.first() {
                if let Some(Value::String(q)) = q0.get("question") {
                    return py::truncate(&py::collapse_ws(q), ASK_TEXT_MAX);
                }
            }
        }
    }
    String::new()
}

// --------------------------------------------------------------------------
// The aggregate
// --------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub struct Day {
    pub prompts: i64,
    pub tools: i64,
    pub artifacts: i64,
    pub replies: i64,
    pub tools_by_name: IndexMap<String, i64>,
    pub hours: IndexMap<u32, i64>,
    pub output: i64,
    pub input: i64,
    pub cache_read: i64,
    pub cache_creation: i64,
    pub cost: f64,
}

#[derive(Debug, Clone)]
pub struct FileTouch {
    pub path: String,
    pub action: String,
    pub count: i64,
}

#[derive(Debug, Clone)]
pub struct TimelineEv {
    pub t: String,
    pub kind: &'static str,
    pub text: String,
    pub tool: Option<String>,
}

impl TimelineEv {
    pub fn to_json(&self) -> Value {
        json!({"t": self.t, "kind": self.kind, "text": self.text, "tool": self.tool})
    }
}

#[derive(Debug, Clone)]
pub struct PendingAsk {
    pub kind: &'static str,
    pub since: f64,
    pub text: String,
}

#[derive(Debug, Clone, Default)]
pub struct Agg {
    pub ai_title: Option<String>,
    pub last_prompt: Option<String>,
    pub last_reply: Option<String>,
    pub now_label: Option<String>,
    pub first_prompt: Option<String>,
    pub prompt_count: i64,
    pub last_activity: Option<Dt>,
    pub links: Vec<Link>,
    pub folder: String,
    pub per_day: IndexMap<String, Day>,
    pub activity_ts: Vec<f64>,
    pub errors: Vec<(f64, String)>,
    /// Epoch of the newest assistant record that is not itself an API-error
    /// reply: an error alert clears once a good reply landed after it.
    pub last_good_reply: Option<f64>,
    pub timeline: Vec<TimelineEv>,
    pub files: IndexMap<String, FileTouch>,
    pub model: String,
    pub tok_output: i64,
    pub tok_input: i64,
    pub tok_cache_read: i64,
    pub tok_cache_creation: i64,
    pub cost: f64,
    pub busy_spans: Vec<[f64; 2]>,
    pub open_tool_since: Option<f64>,
    pub pending_ask: Option<PendingAsk>,
    pub gated_tool_open: Option<(f64, String)>,
    pub permission_mode: Option<String>,
    pub cursor: bool,
    pub ws_days: Option<WsDays>,
}

impl Agg {
    /// agg.get("source") or "claude"
    pub fn source(&self) -> &'static str {
        if self.cursor {
            "cursor"
        } else {
            "claude"
        }
    }

    pub fn tokens_json(&self) -> Value {
        let total = self.tok_output + self.tok_input + self.tok_cache_read + self.tok_cache_creation;
        json!({
            "output": self.tok_output, "input": self.tok_input, "cacheRead": self.tok_cache_read,
            "total": total, "estCostUSD": py::round(self.cost, 4), "model": self.model,
        })
    }

    pub fn links_json(&self) -> Value {
        Value::Array(self.links.iter().take(4).map(Link::to_json).collect())
    }
}

pub fn empty_tokens() -> Value {
    json!({"output": 0, "input": 0, "cacheRead": 0, "total": 0, "estCostUSD": 0.0, "model": ""})
}

/// The JSON-object records of a transcript, streamed line by line like
/// dashboard.py's `for line in f` (universal newlines, UTF-8 with
/// errors="replace", each line stripped; blank, unparsable and non-object
/// lines skipped). Yields (stripped line, record).
pub struct Records {
    reader: std::io::BufReader<std::fs::File>,
    buf: Vec<u8>,
    pending: std::collections::VecDeque<String>,
}

pub fn open_records(path: &Path) -> Option<Records> {
    let f = std::fs::File::open(path).ok()?;
    Some(Records { reader: std::io::BufReader::with_capacity(1 << 16, f), buf: Vec::new(), pending: Default::default() })
}

impl Iterator for Records {
    type Item = (String, Value);

    fn next(&mut self) -> Option<Self::Item> {
        use std::io::BufRead;
        loop {
            if let Some(line) = self.pending.pop_front() {
                let line = py::strip(&line);
                if line.is_empty() {
                    continue;
                }
                if let Ok(v @ Value::Object(_)) = serde_json::from_str::<Value>(line) {
                    return Some((line.to_string(), v));
                }
                continue;
            }
            self.buf.clear();
            match self.reader.read_until(b'\n', &mut self.buf) {
                Ok(0) | Err(_) => return None,
                Ok(_) => {}
            }
            let s = String::from_utf8_lossy(&self.buf);
            self.pending.extend(py::universal_lines(&s).map(String::from));
        }
    }
}

fn basename(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// The folder a Claude transcript sits in (its project dir slug).
pub fn parent_name(p: &Path) -> String {
    p.parent().map(basename).unwrap_or_default()
}

fn obj(v: &Value) -> Option<&Map<String, Value>> {
    v.as_object()
}

/// (o.get("message") or {}) -- None where Python would raise on .get.
fn message_of(o: &Map<String, Value>) -> Option<Map<String, Value>> {
    match o.get("message") {
        Some(Value::Object(m)) => Some(m.clone()),
        Some(v) if py::truthy(v) => None,
        _ => Some(Map::new()),
    }
}

struct ScanCtx {
    agg: Agg,
    busy: Vec<(f64, f64)>,
    open_tools: IndexMap<String, f64>,
    open_tool_names: IndexMap<String, String>,
    pending_ask: IndexMap<String, (&'static str, f64, String)>,
    last_busy: Option<f64>,
    rest_until: Option<f64>,
}

impl ScanCtx {
    fn mark_busy(&mut self, t: f64) {
        if self.last_busy.map(|lb| t > lb).unwrap_or(true) {
            if let Some(lb) = self.last_busy {
                if t - lb > fatigue::HOLD_SECS {
                    let mut cover = lb;
                    if let Some(mx) = self.open_tools.values().copied().reduce(f64::max) {
                        cover = cover.max(mx + MAX_PAIR_SECS);
                    }
                    if t - cover > fatigue::HOLD_SECS {
                        self.rest_until = Some(t);
                    }
                }
            }
            self.last_busy = Some(t);
        }
        self.busy.push((t, t));
    }
}

fn new_day() -> Day {
    Day::default()
}

/// Read one Claude Code transcript once, producing everything downstream needs.
pub fn scan_claude(path: &Path) -> Agg {
    let mut cx = ScanCtx {
        agg: Agg { folder: parent_name(path), ..Default::default() },
        busy: Vec::new(),
        open_tools: IndexMap::new(),
        open_tool_names: IndexMap::new(),
        pending_ask: IndexMap::new(),
        last_busy: None,
        rest_until: None,
    };
    let Some(recs) = open_records(path) else { return cx.agg };
    let mut seen_links: IndexSet<String> = IndexSet::new();
    let mut seen_usage: IndexSet<String> = IndexSet::new();
    let mut last_assistant_text: Option<String> = None;
    let mut last_assistant_tool: Option<Map<String, Value>> = None;
    let mut perm_mode: Option<String> = None;
    let mut turn_floor: Option<f64> = None;
    let mut turn_ended = true;
    let mut ws_state = WsState::default();

    for (line, rec) in recs {
        worksignals::observe(&rec, &mut cx.agg.ws_days, &mut ws_state);
        let o = obj(&rec).unwrap();
        // A closure so a record that would raise in Python stops here (`?`),
        // keeping whatever it already changed, like the Python try/except.
        let _ = (|| -> Option<()> {
            let typ = o.get("type").and_then(|v| v.as_str());
            let ts = o.get("timestamp").and_then(|v| v.as_str()).and_then(py::parse_iso_utc);
            let diso = ts.as_ref().map(|d| py::date_iso(&d.date_naive()));
            let lhour = ts.as_ref().map(py::local_hour);
            let t = ts.as_ref().map(py::ts);
            match typ {
                Some("ai-title") => {
                    if let Some(at) = o.get("aiTitle") {
                        if py::truthy(at) {
                            cx.agg.ai_title = Some(at.as_str().map(|s| s.to_string()).unwrap_or_else(|| at.to_string()));
                        }
                    }
                }
                Some("last-prompt") => {
                    if let Some(lp) = o.get("lastPrompt") {
                        if py::truthy(lp) {
                            cx.agg.last_prompt = Some(clean_prompt(Some(lp))?);
                        }
                    }
                }
                Some("user") => {
                    let msg = message_of(o)?;
                    let content = msg.get("content");
                    if let Some(t) = t {
                        cx.mark_busy(t);
                        if let Some(Value::Array(blocks)) = content {
                            for blk in blocks {
                                let Value::Object(blk) = blk else { continue };
                                if blk.get("type") != Some(&Value::from("tool_result")) {
                                    continue;
                                }
                                let tid = blk.get("tool_use_id").and_then(|v| v.as_str());
                                if let Some(tid) = tid {
                                    cx.pending_ask.shift_remove(tid);
                                    cx.open_tool_names.shift_remove(tid);
                                    if let Some(u) = cx.open_tools.shift_remove(tid) {
                                        if t >= u {
                                            cx.busy.push((u, t.min(u + MAX_PAIR_SECS)));
                                        }
                                    }
                                }
                            }
                        }
                        if turn_ended || (!py::opt_truthy(o.get("isMeta")) && opens_turn(content)) {
                            turn_floor = Some(t);
                            turn_ended = false;
                        }
                    }
                    if let Some(Value::String(pm)) = o.get("permissionMode") {
                        perm_mode = Some(pm.clone());
                    }
                    if is_interrupt_marker(content)? {
                        cx.open_tools.clear();
                        cx.open_tool_names.clear();
                        cx.pending_ask.clear();
                    }
                    if is_real_human_prompt(content)? {
                        cx.open_tools.clear();
                        cx.open_tool_names.clear();
                        cx.pending_ask.clear();
                        let cleaned = clean_prompt(content)?;
                        cx.agg.prompt_count += 1;
                        if cx.agg.first_prompt.is_none() {
                            cx.agg.first_prompt = Some(cleaned.clone());
                        }
                        if let Some(ts) = ts {
                            if cx.agg.last_activity.map(|la| ts > la).unwrap_or(true) {
                                cx.agg.last_activity = Some(ts);
                            }
                        }
                        let text = human_text(content)?;
                        extract_links(&text, &mut cx.agg.links, &mut seen_links);
                        if let Some(diso) = &diso {
                            let d = cx.agg.per_day.entry(diso.clone()).or_insert_with(new_day);
                            d.prompts += 1;
                            d.artifacts += ARTIFACT_RE.find_iter(&text).count() as i64;
                            if let Some(h) = lhour {
                                *d.hours.entry(h).or_insert(0) += 1;
                            }
                        }
                        if let Some(ts) = ts {
                            cx.agg.activity_ts.push(py::ts(&ts));
                            cx.agg.timeline.push(TimelineEv { t: py::iso(&ts), kind: "you", text: py::truncate(&cleaned, 200), tool: None });
                        }
                    }
                }
                Some("assistant") => {
                    if let Some(t) = t {
                        cx.mark_busy(t);
                    }
                    let msg = message_of(o)?;
                    let blocks = msg.get("content");
                    let model = match msg.get("model") {
                        Some(Value::String(s)) => s.clone(),
                        Some(v) if py::truthy(v) => return None,
                        _ => String::new(),
                    };
                    if !model.is_empty() {
                        cx.agg.model = model.clone();
                    }
                    match (record_error_sig(&rec).filter(|s| !s.is_empty()), t) {
                        (Some(sig), Some(t)) => cx.agg.errors.push((t, sig)),
                        // a real reply (this record isn't the synthetic error message
                        // itself): the session recovered, so its error alert may clear
                        (None, Some(t)) => cx.agg.last_good_reply = Some(t),
                        _ => {}
                    }
                    if let Some(diso) = &diso {
                        cx.agg.per_day.entry(diso.clone()).or_insert_with(new_day);
                    }
                    let ukey = msg.get("id").filter(|v| py::truthy(v)).or_else(|| o.get("requestId"));
                    let ukey = match ukey {
                        Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
                        _ => None,
                    };
                    let count_it = match &ukey {
                        None => true,
                        Some(k) => seen_usage.insert(k.clone()),
                    };
                    if count_it {
                        let usage = msg.get("usage").filter(|v| py::truthy(v));
                        let u = cost::usage_cost(&model, usage);
                        cx.agg.cost += u.cost;
                        cx.agg.tok_output += u.output;
                        cx.agg.tok_input += u.input;
                        cx.agg.tok_cache_read += u.cache_read;
                        cx.agg.tok_cache_creation += u.cache_creation;
                        if let Some(diso) = &diso {
                            let d = cx.agg.per_day.get_mut(diso).unwrap();
                            d.cost += u.cost;
                            d.output += u.output;
                            d.input += u.input;
                            d.cache_read += u.cache_read;
                            d.cache_creation += u.cache_creation;
                        }
                    }
                    if let Some(Value::Array(blocks)) = blocks {
                        let mut texts: Vec<String> = Vec::new();
                        let mut last_tool: Option<&Map<String, Value>> = None;
                        for b in blocks {
                            let Value::Object(b) = b else { continue };
                            match b.get("type").and_then(|v| v.as_str()) {
                                Some("text") => {
                                    let txt = match b.get("text") {
                                        Some(Value::String(s)) => s.clone(),
                                        Some(v) if py::truthy(v) => return None,
                                        _ => String::new(),
                                    };
                                    extract_links(&txt, &mut cx.agg.links, &mut seen_links);
                                    if let Some(diso) = &diso {
                                        cx.agg.per_day.get_mut(diso).unwrap().artifacts += ARTIFACT_RE.find_iter(&txt).count() as i64;
                                    }
                                    texts.push(txt);
                                }
                                Some("tool_use") => {
                                    last_tool = Some(b);
                                    if matches!(b.get("name"), Some(v) if py::truthy(v) && !v.is_string()) {
                                        return None;
                                    }
                                    let name = tool_name(b);
                                    let tuid = b.get("id").and_then(|v| v.as_str()).filter(|s| !s.is_empty());
                                    if let (Some(t), Some(tuid)) = (t, tuid) {
                                        if !fatigue::SKIP_TOOLS.contains(&name.as_str()) {
                                            cx.open_tools.insert(tuid.to_string(), t);
                                            cx.open_tool_names.insert(tuid.to_string(), name.clone());
                                        } else {
                                            cx.pending_ask.insert(tuid.to_string(), (ask_kind(&name), t, ask_text(b)));
                                        }
                                    }
                                    if let Some(diso) = &diso {
                                        let d = cx.agg.per_day.get_mut(diso).unwrap();
                                        d.tools += 1;
                                        *d.tools_by_name.entry(name.clone()).or_insert(0) += 1;
                                        if let Some(h) = lhour {
                                            *d.hours.entry(h).or_insert(0) += 1;
                                        }
                                    }
                                    if let Some(ts) = ts {
                                        cx.agg.activity_ts.push(py::ts(&ts));
                                        cx.agg.timeline.push(TimelineEv { t: py::iso(&ts), kind: "tool", text: tool_label(b), tool: Some(name.clone()) });
                                    }
                                    let p = match b.get("input") {
                                        Some(Value::Object(inp)) => {
                                            let fp = inp.get("file_path");
                                            if fp.map(py::truthy).unwrap_or(false) {
                                                fp
                                            } else {
                                                inp.get("path")
                                            }
                                        }
                                        _ => None,
                                    };
                                    if let Some(Value::String(p)) = p {
                                        if !py::strip(p).is_empty() {
                                            let nl = name.to_lowercase();
                                            let action = if nl == "write" {
                                                "write"
                                            } else if nl.contains("edit") {
                                                "edit"
                                            } else if nl == "read" {
                                                "read"
                                            } else {
                                                "other"
                                            };
                                            let fe = cx.agg.files.entry(p.clone()).or_insert_with(|| FileTouch { path: p.clone(), action: action.into(), count: 0 });
                                            fe.count += 1;
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                        if !texts.is_empty() {
                            let joined = texts.join("\n");
                            if let Some(diso) = &diso {
                                let d = cx.agg.per_day.get_mut(diso).unwrap();
                                d.replies += 1;
                                if let Some(h) = lhour {
                                    *d.hours.entry(h).or_insert(0) += 1;
                                }
                            }
                            if let Some(ts) = ts {
                                cx.agg.activity_ts.push(py::ts(&ts));
                                cx.agg.timeline.push(TimelineEv {
                                    t: py::iso(&ts),
                                    kind: "claude",
                                    text: py::truncate(&strip_markdown(&joined), 200),
                                    tool: None,
                                });
                            }
                            last_assistant_text = Some(joined);
                        }
                        if let Some(lt) = last_tool {
                            last_assistant_tool = Some(lt.clone());
                        }
                    }
                    if let Some(ts) = ts {
                        if cx.agg.last_activity.map(|la| ts > la).unwrap_or(true) {
                            cx.agg.last_activity = Some(ts);
                        }
                    }
                }
                Some("system") => {
                    if let (Some(sig), Some(t)) = (record_error_sig(&rec), t) {
                        cx.agg.errors.push((t, sig));
                    }
                    if let Some(Value::String(content)) = o.get("content") {
                        if content.contains("claude.ai") || content.contains("github.com") {
                            extract_links(content, &mut cx.agg.links, &mut seen_links);
                        }
                    }
                    let st = o.get("subtype").and_then(|v| v.as_str());
                    if let Some(t) = t {
                        let dms = match o.get("durationMs") {
                            Some(Value::Number(n)) => n.as_f64(),
                            _ => None,
                        };
                        if st == Some("turn_duration") && dms.map(|d| d > 0.0).unwrap_or(false) {
                            cx.mark_busy(t);
                            let mut start = t - (dms.unwrap() / 1000.0).min(fatigue::MAX_TURN_SECS);
                            for floor in [turn_floor, cx.rest_until].into_iter().flatten() {
                                if floor <= t {
                                    start = start.max(floor);
                                }
                            }
                            cx.busy.push((start, t));
                            turn_floor = Some(t);
                            turn_ended = true;
                        } else if matches!(st, Some("local_command" | "api_error" | "compact_boundary")) {
                            cx.mark_busy(t);
                        }
                    }
                }
                _ => {
                    if line.contains("claude.ai") || line.contains("github.com") {
                        extract_links(&line, &mut cx.agg.links, &mut seen_links);
                    }
                }
            }
            Some(())
        })();
    }

    worksignals::finish(&mut cx.agg.ws_days, &mut ws_state);
    let mut agg = cx.agg;
    if let Some(t) = last_assistant_text.filter(|t| !t.is_empty()) {
        agg.last_reply = Some(strip_markdown(&t));
    }
    if let Some(lt) = &last_assistant_tool {
        agg.now_label = Some(tool_label(lt));
    }
    agg.links.truncate(4);
    if agg.timeline.len() > 60 {
        agg.timeline.drain(..agg.timeline.len() - 60);
    }
    let merged = fatigue::merge_spans(&cx.busy, fatigue::TAIL_SECS);
    agg.busy_spans = merged[merged.len().saturating_sub(fatigue::MAX_SPANS)..].to_vec();
    agg.open_tool_since = cx.open_tools.values().copied().reduce(f64::min);
    agg.pending_ask = cx
        .pending_ask
        .values()
        .fold(None::<&(&'static str, f64, String)>, |best, v| match best {
            Some(b) if b.1 >= v.1 => Some(b),
            _ => Some(v),
        })
        .map(|(k, s, t)| PendingAsk { kind: k, since: *s, text: t.clone() });
    let mut gated: Vec<(f64, String)> = cx
        .open_tools
        .iter()
        .filter_map(|(k, since)| {
            let name = cx.open_tool_names.get(k).cloned().unwrap_or_default();
            permission_gated(&name, perm_mode.as_deref()).then_some((*since, name))
        })
        .collect();
    gated.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
    agg.gated_tool_open = gated.into_iter().next();
    agg.permission_mode = perm_mode;
    agg
}

// --------------------------------------------------------------------------
// Cursor agent transcripts
// --------------------------------------------------------------------------

static CURSOR_QUERY_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(&format!(r"(?s)<user_query>[{}]*(.*?)[{}]*</user_query>", WS, WS)).unwrap());
static CURSOR_TS_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(&format!(r"(?s)<timestamp>[{}]*(.*?)[{}]*</timestamp>", WS, WS)).unwrap());
static CURSOR_CLOCK_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"(?i)([A-Za-z]+)[{ws}]+(\d{{1,2}}),[{ws}]*(\d{{4}}),[{ws}]*(\d{{1,2}}):(\d{{2}})[{ws}]*(AM|PM)\b",
        ws = WS
    ))
    .unwrap()
});
static CURSOR_TZ_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(&format!(r"\((?:UTC|GMT)?([+-])(\d{{1,2}}):(\d{{2}})\)[{}]*$", WS)).unwrap());
static CURSOR_DAYNAME_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(&format!(r"^[A-Za-z]+,[{}]*", WS)).unwrap());
const CURSOR_SKIP_QUERIES: [&str; 1] = ["briefly inform the user about the task result"];

/// True for a file under the Cursor projects dir in an agent-transcripts tree.
pub fn is_cursor_transcript(path: &Path, cursor_root: &Path) -> bool {
    if cursor_root.as_os_str().is_empty() {
        return false;
    }
    let root = crate::state::realpath(cursor_root);
    let real = crate::state::realpath(path);
    if real != root && !real.starts_with(&root) {
        return false;
    }
    let norm = normpath(path);
    format!("/{}", norm).contains("/agent-transcripts/")
}

fn normpath(p: &Path) -> String {
    let s = p.to_string_lossy();
    let abs = s.starts_with('/');
    let mut parts: Vec<&str> = Vec::new();
    for c in s.split('/') {
        match c {
            "" | "." => {}
            ".." => {
                if parts.last().map(|l| *l != "..").unwrap_or(false) {
                    parts.pop();
                } else if !abs {
                    parts.push("..");
                }
            }
            x => parts.push(x),
        }
    }
    let j = parts.join("/");
    if abs {
        format!("/{}", j)
    } else if j.is_empty() {
        ".".into()
    } else {
        j
    }
}

/// The project-dir slug: the folder directly above agent-transcripts.
pub fn cursor_project_slug(path: &Path) -> String {
    let norm = normpath(path);
    let parts: Vec<&str> = norm.split('/').collect();
    match parts.iter().position(|p| *p == "agent-transcripts") {
        Some(i) if i > 0 => parts[i - 1].to_string(),
        Some(_) => String::new(),
        None => parent_name(path),
    }
}

pub fn cursor_message_text(o: &Map<String, Value>) -> String {
    match o.get("message") {
        Some(Value::Object(m)) => human_text(m.get("content")).unwrap_or_default(),
        _ => String::new(),
    }
}

/// The human prompt inside a Cursor user record, or None for system turns.
pub fn cursor_user_query(text: &str) -> Option<String> {
    if text.is_empty() {
        return None;
    }
    let last = CURSOR_QUERY_RE.captures_iter(text).last()?;
    let q = py::strip(&WS_RUN_RE.replace_all(&last[1], " ")).to_string();
    if q.is_empty() {
        return None;
    }
    let low = q.to_lowercase();
    if CURSOR_SKIP_QUERIES.iter().any(|p| low.starts_with(p)) {
        return None;
    }
    Some(q)
}

fn month_num(m: &str) -> Option<u32> {
    let k: String = m.chars().take(3).collect::<String>().to_lowercase();
    ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"]
        .iter()
        .position(|x| *x == k)
        .map(|i| i as u32 + 1)
}

/// <timestamp>Thursday, Sep 24, 2026, 1:44 PM (UTC+5:30)</timestamp> -> UTC.
pub fn parse_cursor_clock(text: &str) -> Option<Dt> {
    let m = CURSOR_TS_RE.captures(text)?;
    let raw_full = py::strip(&m[1]).to_string();
    let raw = CURSOR_DAYNAME_RE.replace(&raw_full, "").into_owned();
    let mut rest = raw.clone();
    let mut offset: Option<i32> = None;
    if let Some(z) = CURSOR_TZ_RE.captures(&raw) {
        rest = py::strip(&raw[..z.get(0).unwrap().start()]).to_string();
        let sign = if &z[1] == "+" { 1 } else { -1 };
        let h: i32 = z[2].parse().ok()?;
        let mi: i32 = z[3].parse().ok()?;
        let secs = sign * (h * 3600 + mi * 60);
        if secs.abs() < 86400 {
            offset = Some(secs);
        }
    }
    let cm = CURSOR_CLOCK_RE.captures(&rest)?;
    let mon = month_num(&cm[1])?;
    let mut hour: u32 = cm[4].parse::<u32>().ok()? % 12;
    if cm[6].to_uppercase() == "PM" {
        hour += 12;
    }
    let date = chrono::NaiveDate::from_ymd_opt(cm[3].parse().ok()?, mon, cm[2].parse().ok()?)?;
    let naive = date.and_hms_opt(hour, cm[5].parse().ok()?, 0)?;
    match offset {
        Some(secs) => {
            use chrono::TimeZone;
            let tz = chrono::FixedOffset::east_opt(secs)?;
            Some(tz.from_local_datetime(&naive).single()?.with_timezone(&chrono::Utc))
        }
        None => py::local_to_utc(&naive),
    }
}

/// Best-effort absolute path for a Cursor project slug (longest existing
/// directory at each step, since '-' is ambiguous).
pub fn path_from_project_slug(slug: &str) -> String {
    if slug.is_empty() || slug.contains('/') || slug.contains('\\') || slug.contains("..") {
        return String::new();
    }
    let raw = slug.strip_prefix('-').unwrap_or(slug);
    let parts: Vec<&str> = raw.split('-').filter(|p| !p.is_empty()).collect();
    if parts.is_empty() {
        return String::new();
    }
    let mut cur = PathBuf::from("/");
    let mut i = 0;
    while i < parts.len() {
        let mut found = None;
        for j in (i + 1..=parts.len()).rev() {
            let cand = cur.join(parts[i..j].join("-"));
            if cand.is_dir() {
                found = Some((cand, j));
                break;
            }
        }
        match found {
            Some((c, j)) => {
                cur = c;
                i = j;
            }
            None => break,
        }
    }
    if i == parts.len() && cur != Path::new("/") && cur.is_dir() {
        cur.to_string_lossy().into_owned()
    } else {
        String::new()
    }
}

enum CursorAct {
    You(String),
    Tool { label: String, name: String, file: Option<String>, action: &'static str },
    Reply(String),
    Nothing,
}

/// One Cursor agent transcript, in the same aggregate shape as Claude's.
pub fn scan_cursor(path: &Path) -> Agg {
    let mut agg = Agg { folder: cursor_project_slug(path), cursor: true, ..Default::default() };
    let Some(recs) = open_records(path) else { return agg };
    let mut seen_links: IndexSet<String> = IndexSet::new();
    let mut last_assistant_text: Option<String> = None;
    let mut last_assistant_tool: Option<Map<String, Value>> = None;
    let mut busy: Vec<(f64, f64)> = Vec::new();
    let mut turn_ts: Option<Dt> = None;
    let mut pending: Vec<CursorAct> = Vec::new();

    let mut apply = |agg: &mut Agg, ts: Dt, act: &CursorAct, last_text: &mut Option<String>| {
        if matches!(act, CursorAct::Nothing) {
            return; // a clock-only user record just moves the turn time
        }
        let t = py::ts(&ts);
        busy.push((t, t));
        if agg.last_activity.map(|la| ts > la).unwrap_or(true) {
            agg.last_activity = Some(ts);
        }
        let diso = py::date_iso(&ts.date_naive());
        let lhour = py::local_hour(&ts);
        agg.per_day.entry(diso.clone()).or_insert_with(new_day);
        agg.activity_ts.push(t);
        match act {
            CursorAct::You(cleaned) => {
                agg.prompt_count += 1;
                if agg.first_prompt.is_none() {
                    agg.first_prompt = Some(cleaned.clone());
                }
                agg.last_prompt = Some(cleaned.clone());
                extract_links(cleaned, &mut agg.links, &mut seen_links);
                let d = agg.per_day.get_mut(&diso).unwrap();
                d.prompts += 1;
                d.artifacts += ARTIFACT_RE.find_iter(cleaned).count() as i64;
                *d.hours.entry(lhour).or_insert(0) += 1;
                agg.timeline.push(TimelineEv { t: py::iso(&ts), kind: "you", text: py::truncate(cleaned, 200), tool: None });
            }
            CursorAct::Tool { label, name, file, action } => {
                let d = agg.per_day.get_mut(&diso).unwrap();
                d.tools += 1;
                *d.tools_by_name.entry(name.clone()).or_insert(0) += 1;
                *d.hours.entry(lhour).or_insert(0) += 1;
                agg.timeline.push(TimelineEv { t: py::iso(&ts), kind: "tool", text: py::truncate(label, 200), tool: Some(name.clone()) });
                if let Some(fp) = file {
                    if !py::strip(fp).is_empty() {
                        let fe = agg.files.entry(fp.clone()).or_insert_with(|| FileTouch { path: fp.clone(), action: action.to_string(), count: 0 });
                        fe.count += 1;
                    }
                }
            }
            CursorAct::Reply(txt) => {
                *last_text = Some(txt.clone());
                let d = agg.per_day.get_mut(&diso).unwrap();
                d.replies += 1;
                *d.hours.entry(lhour).or_insert(0) += 1;
                extract_links(txt, &mut agg.links, &mut seen_links);
                d.artifacts += ARTIFACT_RE.find_iter(txt).count() as i64;
                agg.timeline.push(TimelineEv { t: py::iso(&ts), kind: "claude", text: py::truncate(&strip_markdown(txt), 200), tool: None });
            }
            CursorAct::Nothing => {}
        }
    };

    for (_line, rec) in recs {
        let o = rec.as_object().unwrap();
        let mut emit = |agg: &mut Agg, ts: Option<Dt>, act: CursorAct, last_text: &mut Option<String>| {
            if let Some(ts) = ts {
                if turn_ts.map(|tt| ts >= tt).unwrap_or(true) {
                    if turn_ts.is_none() && !pending.is_empty() {
                        let queued: Vec<CursorAct> = std::mem::take(&mut pending);
                        turn_ts = Some(ts);
                        for a in &queued {
                            apply(agg, ts, a, last_text);
                        }
                    }
                    turn_ts = Some(ts);
                }
            }
            match ts.or(turn_ts) {
                None => pending.push(act),
                Some(use_ts) => apply(agg, use_ts, &act, last_text),
            }
        };
        match o.get("role").and_then(|v| v.as_str()) {
            Some("user") => {
                let text = cursor_message_text(o);
                let ts = parse_cursor_clock(&text);
                if let Some(q) = cursor_user_query(&text) {
                    emit(&mut agg, ts, CursorAct::You(q), &mut last_assistant_text);
                } else if ts.is_some() {
                    emit(&mut agg, ts, CursorAct::Nothing, &mut last_assistant_text);
                }
            }
            Some("assistant") => {
                let content = match o.get("message") {
                    Some(Value::Object(m)) => m.get("content").cloned(),
                    Some(v) if py::truthy(v) => continue,
                    _ => None,
                };
                let blocks = match content {
                    Some(Value::String(s)) => vec![json!({"type": "text", "text": s})],
                    Some(Value::Array(a)) => a,
                    _ => continue,
                };
                let mut texts: Vec<String> = Vec::new();
                for b in &blocks {
                    let Value::Object(b) = b else { continue };
                    match b.get("type").and_then(|v| v.as_str()) {
                        Some("text") => {
                            let txt = match b.get("text") {
                                Some(Value::String(s)) => s.clone(),
                                _ => String::new(),
                            };
                            if !py::strip(&txt).is_empty() {
                                texts.push(txt);
                            }
                        }
                        Some("tool_use") => {
                            if !texts.is_empty() {
                                let joined = texts.join("\n");
                                texts.clear();
                                emit(&mut agg, None, CursorAct::Reply(joined), &mut last_assistant_text);
                            }
                            let name = tool_name(b);
                            let label = tool_label(b);
                            let fp = match b.get("input") {
                                Some(Value::Object(inp)) => ["path", "file_path", "target_file"]
                                    .iter()
                                    .filter_map(|k| inp.get(*k))
                                    .find(|v| py::truthy(v))
                                    .and_then(|v| v.as_str().map(|s| s.to_string())),
                                _ => None,
                            };
                            let nl = name.to_lowercase();
                            let action = if nl == "write" {
                                "write"
                            } else if nl.contains("edit") || nl == "strreplace" || nl == "delete" {
                                "edit"
                            } else if matches!(nl.as_str(), "read" | "grep" | "glob") {
                                "read"
                            } else {
                                "other"
                            };
                            last_assistant_tool = Some(b.clone());
                            emit(&mut agg, None, CursorAct::Tool { label, name, file: fp, action }, &mut last_assistant_text);
                        }
                        _ => {}
                    }
                }
                if !texts.is_empty() {
                    emit(&mut agg, None, CursorAct::Reply(texts.join("\n")), &mut last_assistant_text);
                }
            }
            _ => {}
        }
    }

    let mt = std::fs::metadata(path).ok().and_then(|m| m.modified().ok()).and_then(|t| {
        let d = t.duration_since(std::time::UNIX_EPOCH).ok()?;
        py::from_ts(d.as_secs_f64())
    });
    if let Some(mt) = mt {
        if !pending.is_empty() {
            for a in std::mem::take(&mut pending) {
                apply(&mut agg, mt, &a, &mut last_assistant_text);
            }
        }
        if agg.last_activity.map(|la| mt > la).unwrap_or(true) {
            agg.last_activity = Some(mt);
        }
    }
    if let Some(t) = last_assistant_text.filter(|t| !t.is_empty()) {
        agg.last_reply = Some(strip_markdown(&t));
    }
    if let Some(lt) = &last_assistant_tool {
        agg.now_label = Some(tool_label(lt));
    }
    if let Some(fp) = agg.first_prompt.clone().filter(|f| !f.is_empty()) {
        if agg.ai_title.as_deref().map(|t| t.is_empty()).unwrap_or(true) {
            agg.ai_title = Some(py::truncate(&fp, 80));
        }
    }
    agg.links.truncate(4);
    if agg.timeline.len() > 60 {
        agg.timeline.drain(..agg.timeline.len() - 60);
    }
    let merged = fatigue::merge_spans(&busy, fatigue::TAIL_SECS);
    agg.busy_spans = merged[merged.len().saturating_sub(fatigue::MAX_SPANS)..].to_vec();
    agg
}

/// Session id from a transcript path (<sid>.jsonl).
pub fn session_id_from_path(path: &Path) -> String {
    let b = basename(path);
    b.strip_suffix(".jsonl").map(|s| s.to_string()).unwrap_or(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown() {
        assert_eq!(strip_markdown("# Title\n- **bold** and [link](http://x) `code`"), "Title bold and link code");
        assert_eq!(strip_markdown(""), "");
    }

    #[test]
    fn human_prompts() {
        assert_eq!(is_real_human_prompt(Some(&json!("[Request interrupted by user]"))), Some(false));
        assert_eq!(is_real_human_prompt(Some(&json!([{"type": "text", "text": "[Request interrupted by user for tool use]"}]))), Some(false));
        assert_eq!(is_real_human_prompt(Some(&json!("please continue"))), Some(true));
        assert_eq!(clean_prompt(Some(&json!("a <pasted_content x>stuff</pasted_content>  b"))).unwrap(), "a [pasted content] b");
    }

    #[test]
    fn error_sigs() {
        assert_eq!(record_error_sig(&json!(null)), None);
        assert_eq!(record_error_sig(&json!({"type": "user"})), None);
        assert_eq!(record_error_sig(&json!({"type": "assistant", "isApiErrorMessage": "yes", "message": {"content": "rate limit"}})), None);
        assert!(!ERROR_SIGS.contains(&"permission denied by user"));
    }

    #[test]
    fn cursor_clock() {
        let d = parse_cursor_clock("<timestamp>Thursday, Sep 24, 2026, 1:44 PM (UTC+5:30)</timestamp>").unwrap();
        assert_eq!(py::iso(&d), "2026-09-24T08:14:00+00:00");
        assert!(parse_cursor_clock("no clock").is_none());
    }

    #[test]
    fn opens_turn_rules() {
        for c in [json!("hi"), json!("<task-notification>x"), json!([{"type": "text", "text": "look"}, {"type": "image"}])] {
            assert!(opens_turn(Some(&c)));
        }
        for c in [json!(""), json!("   "), json!(null), json!(7), json!([]), json!([{"type": "tool_result", "tool_use_id": "t"}]), json!(["junk"])] {
            assert!(!opens_turn(Some(&c)));
        }
    }
}
