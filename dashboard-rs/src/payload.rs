//! The live payload: session cards (live + archived), the 30-day season, the
//! activity feed, health, and the session detail drawer -- build_payload and
//! friends in dashboard.py.

use crate::app::App;
use crate::creature;
use crate::fatigue;
use crate::py::{self, Obj};
use crate::pyjson;
use crate::scan::{self, Agg};
use chrono::{Datelike, Duration, NaiveDate};
use indexmap::IndexMap;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Instant;

pub const LIVE_PROMPT_MAX: usize = 280;
pub const LIVE_REPLY_MAX: usize = 400;
pub const LIVE_WAIT_STATES: [&str; 5] = ["waiting", "blocked", "needs_input", "awaiting_input", "permission"];
const LIKELY_AWAIT_SECS: f64 = 20.0;
const ARCHIVED_CAP: usize = 150;
pub const CURSOR_WORKING_SECS: f64 = 120.0;
pub const CURSOR_IDLE_SECS: f64 = 12.0 * 3600.0;
const STREAK_WINDOW: usize = 7;
const STREAK_MAX_MISSES: usize = 2;

/// _epoch(iso): parse_ts(iso).timestamp() or 0.0
/// _recent_unrecovered_error: (ts, sig) of the newest error inside the 6 h
/// window that the session has not answered past (a good reply strictly
/// after it clears it), else None.
pub fn recent_unrecovered_error(errors: &[(f64, String)], last_good_reply: Option<f64>, now_ts: f64) -> Option<(f64, &str)> {
    let cutoff = now_ts - 6.0 * 3600.0;
    let mut recent: Option<(f64, &str)> = None;
    for (ts, sig) in errors {
        if *ts >= cutoff && recent.map(|r| *ts > r.0).unwrap_or(true) {
            recent = Some((*ts, sig));
        }
    }
    match (recent, last_good_reply) {
        (Some(r), Some(good)) if good > r.0 => None,
        _ => recent,
    }
}

pub fn epoch(v: Option<&Value>) -> f64 {
    v.and_then(|v| v.as_str()).and_then(py::parse_iso_utc).map(|d| py::ts(&d)).unwrap_or(0.0)
}

/// Bucket activity timestamps over the last span_secs into n buckets.
pub fn buckets_from_ts(activity_ts: &[f64], n: usize, span_secs: f64) -> Vec<i64> {
    let now = py::now_ts();
    let start = now - span_secs;
    let width = span_secs / n as f64;
    let mut buckets = vec![0i64; n];
    for &ts in activity_ts {
        if ts < start || ts > now {
            continue;
        }
        let idx = ((ts - start) / width) as i64;
        let idx = idx.clamp(0, n as i64 - 1) as usize;
        buckets[idx] += 1;
    }
    buckets
}

/// Length in days of the flexible streak ending at `end`.
pub fn flex_streak(active: &HashSet<NaiveDate>, end: NaiveDate) -> i64 {
    let start = if active.contains(&end) { end } else { end - Duration::days(1) };
    let mut span: Vec<bool> = Vec::new();
    let mut d = start;
    for _ in 0..400 {
        span.push(active.contains(&d));
        let tail = &span[span.len().saturating_sub(STREAK_WINDOW)..];
        if tail.iter().filter(|x| !**x).count() > STREAK_MAX_MISSES {
            span.pop();
            break;
        }
        d -= Duration::days(1);
    }
    let Some(newest) = span.iter().position(|x| *x) else { return 0 };
    let oldest = span.len() - 1 - span.iter().rev().position(|x| *x).unwrap();
    (oldest - newest + 1) as i64
}

fn ach(id: &str, name: &str, desc: &str, icon: &str, actual: f64, target: f64) -> Value {
    let progress = if target != 0.0 { (actual / target).min(1.0) } else { 0.0 };
    json!({"id": id, "name": name, "desc": desc, "icon": icon, "unlocked": progress >= 1.0, "progress": py::round(progress, 3)})
}

fn heat_level(c: i64, mx: i64) -> i64 {
    if c <= 0 || mx <= 0 {
        return 0;
    }
    let frac = c as f64 / mx as f64;
    if frac >= 0.75 {
        4
    } else if frac >= 0.5 {
        3
    } else if frac >= 0.25 {
        2
    } else {
        1
    }
}

pub fn empty_season(err: Option<String>) -> Value {
    let today = py::utc_today();
    let calendar: Vec<Value> = (0..14)
        .rev()
        .map(|i| {
            let d = today - Duration::days(i);
            json!({"date": py::date_iso(&d), "count": 0, "heat": 0, "today": d == today})
        })
        .collect();
    let daily_cost: Vec<Value> = (0..14).rev().map(|i| json!({"date": py::date_iso(&(today - Duration::days(i))), "usd": 0.0})).collect();
    let mut s = json!({
        "level": 1, "xp": 0, "xpIntoLevel": 0, "xpForLevel": creature::xp_for_level(1),
        "pct": 0.0, "rank": creature::rank_for_level(1), "streak": 0, "bestStreak": 0,
        "totals": {"prompts": 0, "tools": 0, "artifacts": 0, "activeDays": 0},
        "calendar": calendar, "achievements": [],
        "tokens": {"output": 0, "input": 0, "cacheRead": 0, "total": 0, "estCostUSD": 0.0},
        "toolBreakdown": [], "folderLeaderboard": [], "hourly": vec![0; 24],
        "dailyCost": daily_cost,
    });
    if let Some(e) = err {
        s["error"] = Value::from(e);
    }
    s
}

pub fn pretty_folder(slug: &str) -> String {
    static RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| regex::Regex::new(r"^-?Users-[^-]+-?").unwrap());
    if slug.is_empty() {
        return "~".into();
    }
    let m = RE.replacen(slug, 1, "");
    if m.is_empty() {
        "~ (home)".into()
    } else {
        m.into_owned()
    }
}

fn find_claude() -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    let cands = [
        format!("{}/.claude/local/claude", home),
        "/opt/homebrew/bin/claude".to_string(),
        "/usr/local/bin/claude".to_string(),
        format!("{}/.local/bin/claude", home),
        format!("{}/.npm-global/bin/claude", home),
    ];
    for c in &cands {
        if Path::new(c).exists() {
            return c.clone();
        }
    }
    let aug = format!("{}:/opt/homebrew/bin:/usr/local/bin:{}/.local/bin", std::env::var("PATH").unwrap_or_default(), home);
    crate::actions::which("claude", &aug).unwrap_or_else(|| "claude".into())
}

/// Run a command with a timeout; (exit code, stdout, stderr) or an error kind.
pub fn run_timeout(cmd: &mut Command, secs: u64) -> Result<(i32, String, String), RunErr> {
    let mut child = cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            RunErr::NotFound
        } else {
            RunErr::Other(e.to_string())
        }
    })?;
    let mut out = child.stdout.take().unwrap();
    let mut err = child.stderr.take().unwrap();
    let t_out = std::thread::spawn(move || {
        let mut s = Vec::new();
        use std::io::Read;
        let _ = out.read_to_end(&mut s);
        s
    });
    let t_err = std::thread::spawn(move || {
        let mut s = Vec::new();
        use std::io::Read;
        let _ = err.read_to_end(&mut s);
        s
    });
    let deadline = Instant::now() + std::time::Duration::from_secs(secs);
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break st,
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(RunErr::Timeout);
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Err(e) => return Err(RunErr::Other(e.to_string())),
        }
    };
    let so = String::from_utf8_lossy(&t_out.join().unwrap_or_default()).into_owned();
    let se = String::from_utf8_lossy(&t_err.join().unwrap_or_default()).into_owned();
    Ok((status.code().unwrap_or(-1), so, se))
}

pub enum RunErr {
    NotFound,
    Timeout,
    Other(String),
}

impl App {
    /// `claude agents --json` -> (agents, error).
    pub fn get_live_agents(&self) -> (Vec<Value>, Option<String>) {
        if let Some(h) = self.agents_hook.read().unwrap().clone() {
            return h();
        }
        let home = std::env::var("HOME").unwrap_or_default();
        let path = format!("{}:/opt/homebrew/bin:/usr/local/bin:{}/.local/bin", std::env::var("PATH").unwrap_or_default(), home);
        let mut cmd = Command::new(find_claude());
        cmd.args(["agents", "--json"]).env("PATH", path);
        match run_timeout(&mut cmd, 10) {
            Ok((code, out, err)) => {
                if code != 0 {
                    return (Vec::new(), Some(format!("claude agents exited {}: {}", code, py::take_chars(py::strip(&err), 200))));
                }
                match serde_json::from_str::<Value>(&out) {
                    Ok(Value::Array(a)) => (a, None),
                    Ok(_) => (Vec::new(), Some("unexpected output shape from claude agents --json".into())),
                    Err(e) => (Vec::new(), Some(format!("claude agents error: {}", e))),
                }
            }
            Err(RunErr::NotFound) => (Vec::new(), Some("claude CLI not found on PATH".into())),
            Err(RunErr::Timeout) => (Vec::new(), Some("claude agents --json timed out".into())),
            Err(RunErr::Other(e)) => (Vec::new(), Some(format!("claude agents error: {}", e))),
        }
    }

    fn fatigue_safe(&self, sid: &str, agg: Option<&Agg>, open_since: Option<f64>, meals: &[(f64, String)]) -> Value {
        let spans: &[[f64; 2]] = agg.map(|a| a.busy_spans.as_slice()).unwrap_or(&[]);
        fatigue::fatigue_for(sid, spans, py::now_ts(), open_since, meals)
    }

    /// creature.fatigue for a transcript outside the payload, or None when
    /// creature energy is off.
    pub fn offpayload_fatigue(&self, sid: &str, agg: Option<&Agg>) -> Option<Value> {
        if !py::opt_truthy(self.load_config().get("creatureFatigue")) {
            return None;
        }
        let meals = self.load_meals(None).remove(sid).unwrap_or_default();
        Some(self.fatigue_safe(sid, agg, None, &meals))
    }

    /// Fresh fatigue for the session a snack was for.
    pub fn session_fatigue_now(&self, sess: &Obj) -> Value {
        let sid = py::str_or_empty(sess, "sessionId").to_string();
        let agg = self.find_transcript(&sid).and_then(|p| self.scan_file(&p));
        let busy = py::str_or_empty(sess, "kind") == "interactive" && py::str_or_empty(sess, "rawStatus") == "busy";
        let ext = if busy { agg.as_ref().and_then(|a| a.open_tool_since) } else { None };
        let meals = self.load_meals(None).remove(&sid).unwrap_or_default();
        self.fatigue_safe(&sid, agg.as_deref(), ext, &meals)
    }

    /// Build one session card from a live agent + its transcript.
    pub fn build_session(&self, agent: &Obj, meals: &HashMap<String, Vec<(f64, String)>>, fatigue_on: bool) -> Value {
        let str_of = |k: &str| agent.get(k).filter(|v| py::truthy(v)).and_then(|v| v.as_str());
        let session_id = str_of("sessionId").or_else(|| str_of("id")).unwrap_or("").to_string();
        let short = py::take_chars(&session_id, 8);
        let short_id = if short.is_empty() { "unknown".to_string() } else { short };
        let cwd = str_of("cwd").unwrap_or("").to_string();
        let folder = if cwd.is_empty() {
            String::new()
        } else {
            Path::new(cwd.trim_end_matches('/')).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
        };
        let kind = str_of("kind").unwrap_or("interactive").to_string();
        let raw_status = if kind == "background" { str_of("state") } else { str_of("status") }.unwrap_or("").to_string();
        let mut status = if kind == "interactive" {
            if raw_status == "busy" {
                "working"
            } else if LIVE_WAIT_STATES.contains(&raw_status.as_str()) {
                "needs"
            } else {
                "idle"
            }
        } else if raw_status == "blocked" {
            "needs"
        } else {
            "idle"
        };

        let path = self.find_transcript(&session_id);
        let agg = path.as_ref().and_then(|p| self.scan_file(p));
        let a = agg.as_deref();
        let mut last_activity = a.and_then(|a| a.last_activity);
        if last_activity.is_none() {
            if let Some(started) = agent.get("startedAt").and_then(py::number) {
                last_activity = py::from_ts(started / 1000.0);
            }
        }
        let now = py::now_utc();
        let (age_secs, last_activity_iso) = match last_activity {
            Some(la) => (((now - la).num_microseconds().unwrap_or(0) as f64 / 1e6) as i64, py::iso(&la)),
            None => (0, String::new()),
        };
        let stale = age_secs > 86400;
        if stale && status != "working" {
            status = "stale";
        }

        let mut alert: Option<String> = None;
        let mut alert_kind: Option<&str> = None;
        let mut waiting_since: Option<String> = None;
        let recent = a.and_then(|a| recent_unrecovered_error(&a.errors, a.last_good_reply, py::now_ts()));
        let ask = a.and_then(|a| a.pending_ask.as_ref());
        if let (Some(ask), true) = (ask, status != "stale") {
            status = "needs";
            alert_kind = Some(ask.kind);
            alert = Some(if ask.kind == "plan" {
                "Waiting for you to approve a plan".to_string()
            } else if ask.text.is_empty() {
                "Asked you a question".to_string()
            } else {
                format!("Asked you a question: {}", ask.text)
            });
            waiting_since = py::iso_from_ts(ask.since);
        } else if let Some((_, sig)) = recent {
            alert = Some(format!("Recent error detected: matched \"{}\" in session output", sig));
            alert_kind = Some("error");
            status = "needs";
        } else if status == "needs" && kind == "interactive" {
            alert = Some("Waiting for your input".into());
            alert_kind = Some("waiting");
        } else if status == "needs" {
            alert = Some("Background agent is blocked / awaiting input".into());
            alert_kind = Some("blocked");
        }

        let mut likely: Option<String> = None;
        if let Some((since, name)) = a.and_then(|a| a.gated_tool_open.as_ref()) {
            if status == "working" && kind == "interactive" && py::now_ts() - since >= LIKELY_AWAIT_SECS {
                likely = Some(format!("May be waiting for permission to run {}", if name.is_empty() { "a tool" } else { name }));
            }
        }
        let now_label = if status == "working" { a.and_then(|a| a.now_label.clone()) } else { None };
        let name = agent.get("name").filter(|v| py::truthy(v)).cloned().unwrap_or_else(|| Value::from(short_id.clone()));
        let title = a.and_then(|a| a.ai_title.clone()).filter(|t| !t.is_empty()).unwrap_or_else(|| "Untitled session".into());
        let (tokens, spark) = match a {
            Some(a) => (a.tokens_json(), buckets_from_ts(&a.activity_ts, 12, 86400.0)),
            None => (scan::empty_tokens(), vec![0; 12]),
        };
        let prompt_count = a.map(|a| a.prompt_count).unwrap_or(0);
        let mut creature = self.creature_for(&session_id, prompt_count);
        if fatigue_on {
            let ext = if kind == "interactive" && raw_status == "busy" { a.and_then(|a| a.open_tool_since) } else { None };
            let m = meals.get(&session_id).cloned().unwrap_or_default();
            creature.insert("fatigue".into(), self.fatigue_safe(&session_id, a, ext, &m));
        }
        let first = a.and_then(|a| a.first_prompt.clone()).unwrap_or_default();
        let last_prompt = a.and_then(|a| a.last_prompt.clone()).filter(|s| !s.is_empty()).unwrap_or_else(|| first.clone());
        let pid = agent.get("pid").and_then(py::strict_int).map(Value::from).unwrap_or(Value::Null);
        json!({
            "id": short_id,
            "sessionId": session_id,
            "name": name,
            "title": title,
            "cwd": cwd,
            "folder": folder,
            "kind": kind,
            "pid": pid,
            "status": status,
            "rawStatus": raw_status,
            "creature": creature,
            "firstPrompt": py::truncate(&first, LIVE_PROMPT_MAX),
            "lastPrompt": py::truncate(&last_prompt, LIVE_PROMPT_MAX),
            "lastReply": py::truncate(&a.and_then(|a| a.last_reply.clone()).unwrap_or_default(), LIVE_REPLY_MAX),
            "now": now_label,
            "promptCount": prompt_count,
            "lastActivity": last_activity_iso,
            "ageSecs": age_secs,
            "stale": stale,
            "links": a.map(|a| a.links_json()).unwrap_or_else(|| json!([])),
            "alert": alert,
            "alertKind": alert_kind,
            "waitingSince": waiting_since,
            "likelyAwaiting": likely,
            "tokens": tokens,
            "spark": spark,
            "source": "claude",
            "pinned": false,
            "tags": [],
            "note": "",
            "stuck": false,
            "stuckReason": null,
        })
    }

    /// Cursor's own chat titles, cached until state.vscdb changes.
    pub fn cursor_composer_names(&self) -> Arc<HashMap<String, String>> {
        let path = self.paths.cursor_state_db.clone();
        let Some(k) = crate::app::file_key(&path) else { return Arc::new(HashMap::new()) };
        let key = (path.clone(), k);
        {
            let g = self.cursor_titles.lock().unwrap();
            if g.0.as_ref() == Some(&key) {
                return g.1.clone();
            }
        }
        let names = Arc::new(read_cursor_composer_names(&path));
        *self.cursor_titles.lock().unwrap() = (Some(key), names.clone());
        names
    }

    /// Heading for a card. Cursor uses the chat title it stored, when it has one.
    pub fn session_heading(&self, agg: Option<&Agg>, sid: &str) -> String {
        let mut fallback = "Untitled session".to_string();
        if let Some(a) = agg {
            if let Some(t) = &a.ai_title {
                if !py::strip(t).is_empty() {
                    fallback = t.clone();
                }
            }
            if a.cursor && !sid.is_empty() {
                if let Some(n) = self.cursor_composer_names().get(sid) {
                    return n.clone();
                }
            }
        }
        fallback
    }

    /// A card for a transcript that is not a live `claude agents` session.
    pub fn build_archived_session(&self, path: &Path, sid: &str, meals: &HashMap<String, Vec<(f64, String)>>, fatigue_on: bool, status: Option<&str>) -> Value {
        let agg = self.scan_file(path);
        let a = agg.as_deref();
        let la = a.and_then(|a| a.last_activity);
        let age = la.map(|la| ((py::now_utc() - la).num_microseconds().unwrap_or(0) as f64 / 1e6) as i64).unwrap_or(0);
        let (tokens, spark) = match a {
            Some(a) => (a.tokens_json(), buckets_from_ts(&a.activity_ts, 12, 86400.0)),
            None => (scan::empty_tokens(), vec![0; 12]),
        };
        let mut creature = self.creature_for(sid, a.map(|a| a.prompt_count).unwrap_or(0));
        if fatigue_on {
            let m = meals.get(sid).cloned().unwrap_or_default();
            creature.insert("fatigue".into(), self.fatigue_safe(sid, a, None, &m));
        }
        let cursor = a.map(|a| a.cursor).unwrap_or(false);
        let source = if cursor { "cursor" } else { "claude" };
        let fresh = cursor && matches!(status, Some("working" | "idle"));
        let folder = a.map(|a| a.folder.clone()).unwrap_or_default();
        let cwd = if cursor { scan::path_from_project_slug(&folder) } else { String::new() };
        let short = py::take_chars(sid, 8);
        let first = a.and_then(|a| a.first_prompt.clone()).unwrap_or_default();
        let last_prompt = a.and_then(|a| a.last_prompt.clone()).filter(|s| !s.is_empty()).unwrap_or_else(|| first.clone());
        json!({
            "id": if short.is_empty() { "unknown".to_string() } else { short.clone() },
            "sessionId": sid,
            "name": if cursor { "cursor".to_string() } else if short.is_empty() { "archived".to_string() } else { short },
            "title": self.session_heading(a, sid),
            "cwd": cwd, "folder": folder,
            "kind": if fresh { "cursor" } else { "archived" }, "pid": null,
            "status": if fresh { status.unwrap() } else { "stale" },
            "rawStatus": if cursor { "cursor" } else { "archived" },
            "creature": creature,
            "firstPrompt": py::truncate(&first, LIVE_PROMPT_MAX),
            "lastPrompt": py::truncate(&last_prompt, LIVE_PROMPT_MAX),
            "lastReply": py::truncate(&a.and_then(|a| a.last_reply.clone()).unwrap_or_default(), LIVE_REPLY_MAX),
            "now": if status == Some("working") { a.and_then(|a| a.now_label.clone()) } else { None },
            "promptCount": a.map(|a| a.prompt_count).unwrap_or(0),
            "lastActivity": la.map(|d| py::iso(&d)).unwrap_or_default(), "ageSecs": age,
            "stale": !fresh, "links": a.map(|a| a.links_json()).unwrap_or_else(|| json!([])),
            "alert": null, "alertKind": null, "tokens": tokens, "spark": spark,
            "archived": !fresh,
            "source": source,
        })
    }

    /// The merged recent-activity feed across the payload sessions.
    pub fn build_feed(&self, sessions: &[Value], limit: usize) -> Vec<Value> {
        let mut events: Vec<Value> = Vec::new();
        for s in sessions {
            let sid = s["sessionId"].as_str().unwrap_or("");
            if sid.is_empty() {
                continue;
            }
            let Some(agg) = self.find_transcript(sid).and_then(|p| self.scan_file(&p)) else { continue };
            let title = s.get("title").filter(|v| py::truthy(v)).cloned().unwrap_or_else(|| {
                Value::from(agg.ai_title.clone().filter(|t| !t.is_empty()).unwrap_or_else(|| "Untitled session".into()))
            });
            let source = s.get("source").filter(|v| py::truthy(v)).cloned().unwrap_or_else(|| Value::from("claude"));
            let n = agg.timeline.len();
            for ev in &agg.timeline[n.saturating_sub(12)..] {
                events.push(json!({
                    "t": ev.t, "sessionId": sid, "title": title, "kind": ev.kind,
                    "text": ev.text, "tool": ev.tool, "source": source,
                }));
            }
            if s.get("alert").map(py::truthy).unwrap_or(false) {
                events.push(json!({
                    "t": s.get("lastActivity").filter(|v| py::truthy(v)).cloned().unwrap_or_else(|| Value::from("")),
                    "sessionId": sid, "title": title, "kind": "needs",
                    "text": s["alert"], "tool": null, "source": source,
                }));
            }
        }
        let mut keyed: Vec<(f64, Value)> = events.into_iter().map(|e| (epoch(e.get("t")), e)).collect();
        keyed.sort_by(|a, b| b.0.total_cmp(&a.0));
        keyed.into_iter().take(limit).map(|(_, e)| e).collect()
    }

    /// Scan all transcripts for the 30-day season: XP, level, streak, calendar,
    /// achievements and the extended insights.
    pub fn compute_season(&self) -> Value {
        let today = py::utc_today();
        let window_start = today - Duration::days(29);
        let mut day_prompts: HashMap<NaiveDate, i64> = HashMap::new();
        let mut day_tools: HashMap<NaiveDate, i64> = HashMap::new();
        let mut day_artifacts: HashMap<NaiveDate, i64> = HashMap::new();
        let mut day_replies: HashMap<NaiveDate, i64> = HashMap::new();
        let mut day_cost: HashMap<NaiveDate, f64> = HashMap::new();
        let mut active: HashSet<NaiveDate> = HashSet::new();
        let mut folders: HashSet<String> = HashSet::new();
        let (mut tok_out, mut tok_in, mut tok_cr, mut tok_cc) = (0i64, 0i64, 0i64, 0i64);
        let mut total_cost = 0.0f64;
        let mut tool_counts: IndexMap<String, i64> = IndexMap::new();
        let mut folder_prompts: IndexMap<String, i64> = IndexMap::new();
        let mut folder_tools: IndexMap<String, i64> = IndexMap::new();
        let mut hourly = [0i64; 24];
        let mut night_owl = false;

        for path in self.iter_transcript_paths() {
            let Some(agg) = self.scan_file(&path) else { continue };
            let fol = agg.folder.clone();
            folders.insert(fol.clone());
            for (diso, dd) in &agg.per_day {
                let Some(d) = py::parse_date(diso) else { continue };
                if d < window_start || d > today {
                    continue;
                }
                if dd.prompts != 0 {
                    *day_prompts.entry(d).or_insert(0) += dd.prompts;
                    active.insert(d);
                    *folder_prompts.entry(fol.clone()).or_insert(0) += dd.prompts;
                }
                if dd.tools != 0 {
                    *day_tools.entry(d).or_insert(0) += dd.tools;
                    *folder_tools.entry(fol.clone()).or_insert(0) += dd.tools;
                }
                if dd.artifacts != 0 {
                    *day_artifacts.entry(d).or_insert(0) += dd.artifacts;
                }
                if dd.replies != 0 {
                    *day_replies.entry(d).or_insert(0) += dd.replies;
                    active.insert(d);
                }
                *day_cost.entry(d).or_insert(0.0) += dd.cost;
                tok_out += dd.output;
                tok_in += dd.input;
                tok_cr += dd.cache_read;
                tok_cc += dd.cache_creation;
                total_cost += dd.cost;
                for (nm, c) in &dd.tools_by_name {
                    *tool_counts.entry(nm.clone()).or_insert(0) += c;
                }
                for (h, c) in &dd.hours {
                    if *h < 24 {
                        hourly[*h as usize] += c;
                        if *h <= 5 && *c > 0 {
                            night_owl = true;
                        }
                    }
                }
            }
        }

        let total_prompts: i64 = day_prompts.values().sum();
        let total_tools: i64 = day_tools.values().sum();
        let total_artifacts: i64 = day_artifacts.values().sum();
        let active_days = active.len() as i64;
        let xp = total_prompts * 10 + total_tools * 3 + total_artifacts * 40;
        let (level, xp_into, xp_for) = creature::derive_level(xp);
        let pct = if xp_for != 0 { py::round(xp_into as f64 / xp_for as f64 * 100.0, 1) } else { 0.0 };
        let streak = flex_streak(&active, today);
        let best = active.iter().map(|d| flex_streak(&active, *d)).fold(streak, i64::max);

        let counts: Vec<i64> =
            (0..14).rev().map(|i| { let d = today - Duration::days(i); day_prompts.get(&d).unwrap_or(&0) + day_replies.get(&d).unwrap_or(&0) }).collect();
        let maxc = counts.iter().copied().max().unwrap_or(0);
        let calendar: Vec<Value> = (0..14)
            .rev()
            .enumerate()
            .map(|(idx, i)| {
                let d = today - Duration::days(i);
                json!({"date": py::date_iso(&d), "count": counts[idx], "heat": heat_level(counts[idx], maxc), "today": d == today})
            })
            .collect();

        let achievements = vec![
            ach("century", "Century", "100+ prompts in 30 days", "💯", total_prompts as f64, 100.0),
            ach("artificer", "Artificer", "10+ artifacts in 30 days", "🎨", total_artifacts as f64, 10.0),
            ach("streak_keeper", "Streak Keeper", "5+ day streak", "🔥", streak as f64, 5.0),
            ach("tool_smith", "Tool Smith", "500+ tool calls in 30 days", "🛠️", total_tools as f64, 500.0),
            ach("polyglot", "Polyglot", "Worked in 5+ distinct folders", "🌐", folders.len() as f64, 5.0),
            ach("marathon", "Marathoner", "Active on 20+ days", "🏃", active_days as f64, 20.0),
            json!({"id": "night_owl", "name": "Night Owl", "desc": "Coded between midnight and 5am", "icon": "🦉",
                   "unlocked": night_owl, "progress": if night_owl { 1.0 } else { 0.0 }}),
            ach("power_user", "Power User", "1000+ XP in 30 days", "⚡", xp as f64, 1000.0),
        ];
        let mut tb: Vec<(String, i64)> = tool_counts.into_iter().collect();
        tb.sort_by_key(|a| std::cmp::Reverse(a.1));
        let tool_breakdown: Vec<Value> = tb.into_iter().take(8).map(|(n, c)| json!({"name": n, "count": c})).collect();
        let mut fnames: Vec<String> = folder_prompts.keys().cloned().collect();
        for f in folder_tools.keys() {
            if !folder_prompts.contains_key(f) {
                fnames.push(f.clone());
            }
        }
        let mut fl: Vec<(i64, Value)> = fnames
            .into_iter()
            .map(|f| {
                let pp = folder_prompts.get(&f).copied().unwrap_or(0);
                let tt = folder_tools.get(&f).copied().unwrap_or(0);
                (pp * 10 + tt, json!({"folder": f, "prompts": pp, "tools": tt, "score": pp * 10 + tt}))
            })
            .collect();
        fl.sort_by_key(|a| std::cmp::Reverse(a.0));
        let daily_cost: Vec<Value> = (0..14)
            .rev()
            .map(|i| {
                let d = today - Duration::days(i);
                json!({"date": py::date_iso(&d), "usd": py::round(day_cost.get(&d).copied().unwrap_or(0.0), 4)})
            })
            .collect();
        json!({
            "level": level, "xp": xp, "xpIntoLevel": xp_into, "xpForLevel": xp_for, "pct": pct,
            "rank": creature::rank_for_level(level), "streak": streak, "bestStreak": best,
            "totals": {"prompts": total_prompts, "tools": total_tools, "artifacts": total_artifacts, "activeDays": active_days},
            "calendar": calendar, "achievements": achievements,
            "tokens": {"output": tok_out, "input": tok_in, "cacheRead": tok_cr, "total": tok_out + tok_in + tok_cr + tok_cc,
                       "estCostUSD": py::round(total_cost, 2)},
            "toolBreakdown": tool_breakdown,
            "folderLeaderboard": fl.into_iter().take(6).map(|(_, v)| v).collect::<Vec<_>>(),
            "hourly": hourly.to_vec(),
            "dailyCost": daily_cost,
        })
    }

    pub fn build_payload(&self) -> Value {
        let config = self.load_config();
        let fz_on = py::opt_truthy(config.get("creatureFatigue"));
        let meals = if fz_on { self.load_meals(None) } else { HashMap::new() };
        let (agents, error) = self.get_live_agents();
        let mut sessions: Vec<Value> = Vec::new();
        for a in &agents {
            if let Value::Object(a) = a {
                sessions.push(self.build_session(a, &meals, fz_on));
            }
        }

        let mut live_ids: HashSet<String> = sessions.iter().filter_map(|s| s["sessionId"].as_str().map(String::from)).collect();
        let mut arch: Vec<(f64, PathBuf, String)> = Vec::new();
        for p in self.iter_transcript_paths() {
            let sid = scan::session_id_from_path(&p);
            if sid.is_empty() || live_ids.contains(&sid) {
                continue;
            }
            // prompt-less transcripts are listed too (Python 2.5.2): the Overflow Vial
            // needs every creature reachable, so the cap (not the floor) is the gate.
            let Some(agg) = self.scan_file(&p) else { continue };
            if agg.cursor {
                let touched = std::fs::metadata(&p)
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| py::now_ts() - d.as_secs_f64())
                    .unwrap_or(CURSOR_IDLE_SECS + 1.0);
                if touched <= CURSOR_IDLE_SECS {
                    let st = if touched <= CURSOR_WORKING_SECS { "working" } else { "idle" };
                    sessions.push(self.build_archived_session(&p, &sid, &meals, fz_on, Some(st)));
                    live_ids.insert(sid);
                    continue;
                }
            }
            let la = agg.last_activity.map(|d| py::ts(&d)).unwrap_or(0.0);
            arch.push((la, p, sid));
        }
        arch.sort_by(|a, b| b.0.total_cmp(&a.0));
        for (_, p, sid) in arch.into_iter().take(ARCHIVED_CAP) {
            sessions.push(self.build_archived_session(&p, &sid, &meals, fz_on, None));
        }

        let order = |s: &str| match s {
            "working" => 0,
            "needs" => 1,
            "idle" => 2,
            "stale" => 3,
            _ => 9,
        };
        let mut keyed: Vec<((i32, f64), Value)> =
            sessions.into_iter().map(|s| ((order(s["status"].as_str().unwrap_or("")), -epoch(s.get("lastActivity"))), s)).collect();
        keyed.sort_by(|a, b| a.0 .0.cmp(&b.0 .0).then(a.0 .1.total_cmp(&b.0 .1)));
        let mut sessions: Vec<Value> = keyed.into_iter().map(|(_, s)| s).collect();

        let season = self.compute_season();
        let feed = self.build_feed(&sessions, 25);
        let meta = self.load_meta();
        crate::evo::merge_evo_choices(&mut sessions, &self.load_evo_choices());
        let stuck_secs = config.get("stuckMinutes").and_then(py::py_int).unwrap_or(15) * 60;
        let mut stuck_ids: Vec<Value> = Vec::new();
        for s in sessions.iter_mut() {
            let sid = s["sessionId"].as_str().unwrap_or("").to_string();
            if let Some(Value::Object(m)) = meta.get(&sid) {
                s["pinned"] = Value::Bool(py::opt_truthy(m.get("pinned")));
                s["tags"] = m.get("tags").cloned().unwrap_or_else(|| json!([]));
                s["note"] = m.get("note").cloned().unwrap_or_else(|| json!(""));
                let alias = py::strip(py::str_or_empty(m, "name")).to_string();
                if !alias.is_empty() {
                    let title = s["title"].clone();
                    s["alias"] = Value::from(alias.clone());
                    s["autoTitle"] = title;
                    s["title"] = Value::from(alias);
                }
            }
            let age = s.get("ageSecs").and_then(|v| v.as_i64()).unwrap_or(0);
            if s["status"] == "working" && age > stuck_secs {
                s["stuck"] = Value::Bool(true);
                s["stuckReason"] = Value::from(format!("No activity for {} min while working", age.div_euclid(60)));
                if !sid.is_empty() {
                    stuck_ids.push(Value::from(sid));
                }
            }
        }

        let daily_cost = season["dailyCost"].as_array().and_then(|a| a.last()).and_then(|d| d["usd"].as_f64()).unwrap_or(0.0);
        let budget = config.get("dailyBudgetUSD").filter(|v| py::truthy(v)).and_then(py::py_float).unwrap_or(0.0);
        let over_budget = budget > 0.0 && daily_cost > budget;
        let budget_pct = if budget > 0.0 { py::round(daily_cost / budget, 4) } else { 0.0 };
        let mut warnings: Vec<Value> = Vec::new();
        if !stuck_ids.is_empty() {
            warnings.push(Value::from(format!("{} tab{} may be stuck", stuck_ids.len(), if stuck_ids.len() == 1 { "" } else { "s" })));
        }
        if over_budget {
            warnings.push(Value::from("over daily budget"));
        }
        let health = json!({
            "stuck": stuck_ids.clone(), "stuckCount": stuck_ids.len(),
            "dailyCostUSD": py::round(daily_cost, 4), "dailyBudgetUSD": budget,
            "overBudget": over_budget, "budgetPct": budget_pct, "warnings": warnings,
        });
        let vial = self.payload_vial(&mut sessions);
        let mut payload = json!({
            "updated": py::iso(&py::now_utc()),
            "version": crate::app::APP_VERSION,
            "boot": self.boot_id,
            "season": season,
            "sessions": sessions,
            "feed": feed,
            "health": health,
            "config": config,
            "vial": vial,
        });
        if let Some(e) = error {
            payload["error"] = Value::from(e);
        }
        payload
    }

    pub fn invalidate_payload_memo(&self) {
        let mut g = self.payload_memo.lock().unwrap();
        g.at = None;
        g.gen += 1;
    }

    /// build_payload, memoized for 1.5 s so bursts of requests share one build.
    pub fn build_payload_memo(&self) -> Arc<Value> {
        let gen0 = {
            let g = self.payload_memo.lock().unwrap();
            if let (Some(d), Some(at)) = (&g.data, g.at) {
                if at.elapsed().as_secs_f64() < 1.5 {
                    return d.clone();
                }
            }
            g.gen
        };
        let data = Arc::new(self.build_payload());
        let mut g = self.payload_memo.lock().unwrap();
        g.at = if g.gen == gen0 { Some(Instant::now()) } else { None };
        g.data = Some(data.clone());
        data
    }

    /// (serialized payload, change signature), shared by every stream client.
    pub fn build_payload_blob(&self) -> (Arc<String>, Arc<String>) {
        let data = self.build_payload_memo();
        if let Some((d, b, s)) = self.blob_memo.lock().unwrap().as_ref() {
            if Arc::ptr_eq(d, &data) {
                return (b.clone(), s.clone());
            }
        }
        let blob = Arc::new(pyjson::dumps(&data));
        let sig = Arc::new(stream_sig(&data));
        *self.blob_memo.lock().unwrap() = Some((data, blob.clone(), sig.clone()));
        (blob, sig)
    }

    /// Copies of the payload sessions with untruncated prompt/reply text.
    pub fn full_text_sessions(&self, sessions: &[Value]) -> Vec<Value> {
        sessions
            .iter()
            .map(|s| {
                let mut s = s.clone();
                let sid = s["sessionId"].as_str().unwrap_or("").to_string();
                if let Some(agg) = (!sid.is_empty()).then(|| self.find_transcript(&sid)).flatten().and_then(|p| self.scan_file(&p)) {
                    let first = agg.first_prompt.clone().unwrap_or_default();
                    s["firstPrompt"] = Value::from(first.clone());
                    s["lastPrompt"] = Value::from(agg.last_prompt.clone().filter(|x| !x.is_empty()).unwrap_or(first));
                    s["lastReply"] = Value::from(agg.last_reply.clone().unwrap_or_default());
                }
                s
            })
            .collect()
    }

    pub fn payload_session(&self, sid: &str) -> Option<Value> {
        self.build_payload_memo()["sessions"].as_array()?.iter().find(|s| s["sessionId"] == sid).cloned()
    }

    /// The single-session detail payload, or None if unknown.
    pub fn build_session_detail(&self, sid: &str) -> Option<Value> {
        let path = self.find_transcript(sid)?;
        let agg = self.scan_file(&path)?;
        let sess = self.payload_session(sid);
        let (folder, cwd, kind, status, links, title, creature);
        if let Some(s) = &sess {
            folder = s.get("folder").filter(|v| py::truthy(v)).cloned().unwrap_or_else(|| Value::from(agg.folder.clone()));
            cwd = s.get("cwd").cloned().unwrap_or_else(|| json!(""));
            kind = s.get("kind").cloned().unwrap_or_else(|| json!("interactive"));
            status = s.get("status").cloned().unwrap_or_else(|| json!("idle"));
            links = s.get("links").filter(|v| py::truthy(v)).cloned().unwrap_or_else(|| agg.links_json());
            title = s.get("title").filter(|v| py::truthy(v)).cloned().unwrap_or_else(|| Value::from(self.session_heading(Some(&agg), sid)));
            creature = s.get("creature").filter(|v| py::truthy(v)).cloned().unwrap_or_else(|| Value::Object(self.creature_for(sid, agg.prompt_count)));
        } else {
            folder = Value::from(agg.folder.clone());
            cwd = json!("");
            kind = json!("interactive");
            links = agg.links_json();
            title = Value::from(self.session_heading(Some(&agg), sid));
            let age = agg.last_activity.map(|la| ((py::now_utc() - la).num_microseconds().unwrap_or(0) as f64 / 1e6) as i64).unwrap_or(0);
            let mut st = if age > 86400 { "stale" } else { "idle" };
            // as in the session list: an error the session already answered past stops flagging
            if recent_unrecovered_error(&agg.errors, agg.last_good_reply, py::now_ts()).is_some() {
                st = "needs";
            }
            status = json!(st);
            let mut c = self.creature_for(sid, agg.prompt_count);
            if let Some(fz) = self.offpayload_fatigue(sid, Some(&agg)) {
                c.insert("fatigue".into(), fz);
            }
            creature = Value::Object(c);
        }
        let mut files: Vec<&scan::FileTouch> = agg.files.values().collect();
        files.sort_by_key(|a| std::cmp::Reverse(a.count));
        let files: Vec<Value> = files.into_iter().take(15).map(|f| json!({"path": f.path, "action": f.action, "count": f.count})).collect();
        let (mut first_iso, mut span_days) = (String::new(), 0i64);
        if !agg.activity_ts.is_empty() {
            let first = agg.activity_ts.iter().copied().fold(f64::INFINITY, f64::min);
            let last = agg.activity_ts.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            first_iso = py::iso_from_ts(first).unwrap_or_default();
            span_days = ((last - first) / 86400.0).floor() as i64;
        }
        let tl = &agg.timeline[agg.timeline.len().saturating_sub(20)..];
        let first_prompt = agg.first_prompt.clone().unwrap_or_default();
        let source = sess
            .as_ref()
            .and_then(|s| s.get("source").filter(|v| py::truthy(v)).cloned())
            .unwrap_or_else(|| Value::from(agg.source()));
        let short = py::take_chars(sid, 8);
        Some(json!({
            "id": if short.is_empty() { "unknown".to_string() } else { short },
            "sessionId": sid,
            "title": title,
            "folder": folder,
            "cwd": cwd,
            "kind": kind,
            "status": status,
            "model": agg.model,
            "tokens": agg.tokens_json(),
            "timeline": tl.iter().map(|e| e.to_json()).collect::<Vec<_>>(),
            "files": files,
            "links": links,
            "sparkHourly": buckets_from_ts(&agg.activity_ts, 24, 86400.0),
            "resumeCmd": if agg.cursor { String::new() } else { format!("claude --resume {}", sid) },
            "lastReplyFull": agg.last_reply.clone().unwrap_or_default(),
            "firstPromptFull": first_prompt.clone(),
            "lastPromptFull": agg.last_prompt.clone().filter(|x| !x.is_empty()).unwrap_or(first_prompt),
            "firstActivity": first_iso,
            "lastActivity": agg.last_activity.map(|d| py::iso(&d)).unwrap_or_default(),
            "spanDays": span_days,
            "activeDays": agg.per_day.len(),
            "promptCount": agg.prompt_count,
            "creature": creature,
            "source": source,
        }))
    }

    /// How many sessions are working / need you / are idle right now.
    pub fn hq_crew_counts(&self) -> Value {
        let (mut w, mut n, mut i) = (0, 0, 0);
        if let Some(ss) = self.build_payload_memo()["sessions"].as_array() {
            for s in ss {
                match s["status"].as_str() {
                    Some("working") => w += 1,
                    Some("needs") => n += 1,
                    Some("idle") => i += 1,
                    _ => {}
                }
            }
        }
        json!({"working": w, "needs": n, "idle": i})
    }
}

/// Everything a stream client must see change: the payload minus the clock
/// and the per-tick ages.
pub fn stream_sig(payload: &Value) -> String {
    let mut slim = payload.clone();
    if let Value::Object(o) = &mut slim {
        o.shift_remove("updated");
        if let Some(Value::Array(ss)) = o.get_mut("sessions") {
            for s in ss.iter_mut() {
                if let Value::Object(so) = s {
                    so.shift_remove("ageSecs");
                }
            }
        }
    }
    pyjson::dumps_sorted(&slim)
}

fn sqlite_names(path: &Path) -> rusqlite::Result<HashMap<String, String>> {
    let con = rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
    con.busy_timeout(std::time::Duration::from_secs(1))?;
    let mut names = HashMap::new();
    let mut stmt = con.prepare("SELECT composerId, value FROM composerHeaders")?;
    let rows = stmt.query_map([], |r| {
        let cid: rusqlite::types::Value = r.get(0)?;
        let val: rusqlite::types::Value = r.get(1)?;
        Ok((cid, val))
    })?;
    for row in rows.flatten() {
        let cid = match row.0 {
            rusqlite::types::Value::Text(s) => s,
            _ => continue,
        };
        let val = match row.1 {
            rusqlite::types::Value::Text(s) => s,
            rusqlite::types::Value::Blob(b) => String::from_utf8_lossy(&b).into_owned(),
            _ => continue,
        };
        if cid.is_empty() || val.is_empty() {
            continue;
        }
        let Ok(Value::Object(o)) = serde_json::from_str::<Value>(&val) else { continue };
        let Some(Value::String(name)) = o.get("name") else { continue };
        let name = py::collapse_ws(name);
        if !name.is_empty() {
            names.insert(cid, name);
        }
    }
    Ok(names)
}

/// composerId -> chat title from Cursor's composerHeaders table (read-only).
pub fn read_cursor_composer_names(path: &Path) -> HashMap<String, String> {
    sqlite_names(path).unwrap_or_default()
}

/// The weekday index Python's date.weekday() gives (Monday = 0).
pub fn weekday(d: &NaiveDate) -> usize {
    d.weekday().num_days_from_monday() as usize
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::testutil::*;

    fn days(t: NaiveDate, offs: &[i64]) -> HashSet<NaiveDate> {
        offs.iter().map(|o| t - Duration::days(*o)).collect()
    }

    #[test]
    fn flexible_streak() {
        let t = NaiveDate::from_ymd_opt(2026, 10, 6).unwrap();
        assert_eq!(flex_streak(&days(t, &[0, 1, 2, 3]), t), 4);
        assert_eq!(flex_streak(&days(t, &[1, 2, 3]), t), 3);
        assert_eq!(flex_streak(&days(t, &[0, 1, 2, 4, 5, 6, 7, 8, 9]), t), 10);
        assert_eq!(flex_streak(&days(t, &[0, 2, 4, 5, 6, 7]), t), 8);
        assert_eq!(flex_streak(&days(t, &[0, 2, 4, 6, 7, 8, 9]), t), 5);
        assert_eq!(flex_streak(&HashSet::new(), t), 0);
        assert_eq!(flex_streak(&days(t, &[0, 1, 5, 6, 7]), t), 2);
        let start = NaiveDate::from_ymd_opt(2026, 9, 23).unwrap();
        let mut active: HashSet<NaiveDate> = (0..14).map(|i| start + Duration::days(i)).collect();
        for d in [(2026, 9, 26), (2026, 9, 27), (2026, 10, 3)] {
            active.remove(&NaiveDate::from_ymd_opt(d.0, d.1, d.2).unwrap());
        }
        assert_eq!(flex_streak(&active, t), 14);
    }

    #[test]
    fn stream_sig_ignores_clock_and_ages() {
        let p = |u: &str, age: i64, title: &str| json!({"updated": u, "sessions": [{"sessionId": "a", "ageSecs": age, "title": title}]});
        assert_eq!(stream_sig(&p("t1", 5, "t")), stream_sig(&p("t2", 9, "t")));
        assert_ne!(stream_sig(&p("t1", 5, "t")), stream_sig(&p("t1", 5, "new")));
    }

    const SID: &str = "44444444-4444-4444-8444-444444444444";

    #[test]
    fn cards_trim_text_and_detail_keeps_it() {
        let (_d, app) = app();
        let long_prompt = format!("please analyse {}", "x".repeat(20000));
        let long_reply = format!("done: {}", "y".repeat(5000));
        let path = app.paths.projects_dir.join("proj").join(format!("{}.jsonl", SID));
        write_jsonl(&path, &[
            json!({"type": "user", "timestamp": "2026-09-01T10:00:00.000Z", "message": {"content": long_prompt}}),
            json!({"type": "assistant", "timestamp": "2026-09-01T10:00:00.000Z", "message": {"id": "m1", "model": "claude-opus-5-5",
                   "content": [{"type": "text", "text": long_reply}], "usage": {"input_tokens": 1, "output_tokens": 1}}}),
        ]);
        let card = app.build_archived_session(&path, SID, &HashMap::new(), false, None);
        assert!(card["firstPrompt"].as_str().unwrap().chars().count() <= LIVE_PROMPT_MAX);
        assert!(card["lastReply"].as_str().unwrap().chars().count() <= LIVE_REPLY_MAX);
        assert!(card["firstPrompt"].as_str().unwrap().ends_with('…'));
        assert!(pyjson::dumps(&card).len() < 4000);
        let full = app.full_text_sessions(&[card]);
        assert_eq!(full[0]["firstPrompt"], long_prompt);
        assert_eq!(full[0]["lastReply"], long_reply);
        let agent = match json!({"sessionId": SID, "cwd": "/tmp/proj", "kind": "interactive", "status": "idle"}) {
            Value::Object(o) => o,
            _ => unreachable!(),
        };
        let live = app.build_session(&agent, &HashMap::new(), false);
        assert!(live["lastReply"].as_str().unwrap().chars().count() <= LIVE_REPLY_MAX);
        let d = app.build_session_detail(SID).unwrap();
        assert_eq!(d["firstPromptFull"], long_prompt);
        assert_eq!(d["lastReplyFull"], long_reply);
        assert!(!card_has_fatigue(&live));
    }

    fn card_has_fatigue(c: &Value) -> bool {
        c["creature"].get("fatigue").is_some()
    }

    #[test]
    fn needs_you_detection() {
        let (_d, app) = app();
        let path = app.paths.projects_dir.join("proj").join(format!("{}.jsonl", SID));
        let ago = |s: i64| py::iso(&(py::now_utc() - chrono::Duration::seconds(s))).replace("+00:00", "Z");
        let session = |recs: &[Value], status: &str| {
            write_jsonl(&path, recs);
            let agent = match json!({"sessionId": SID, "cwd": "/tmp/proj", "kind": "interactive", "pid": 4242, "status": status}) {
                Value::Object(o) => o,
                _ => unreachable!(),
            };
            app.build_session(&agent, &HashMap::new(), false)
        };
        let ask = json!({"type": "tool_use", "id": "ask1", "name": "AskUserQuestion", "input": {"questions": [{"question": "Which   table should I use?"}]}});
        let asst = |blocks: Value, ts: String, mid: &str| json!({"type": "assistant", "timestamp": ts, "message": {"id": mid, "model": "claude-opus-5-5", "content": blocks, "usage": {"input_tokens": 1, "output_tokens": 1}}});
        let s = session(&[json!({"type": "user", "timestamp": ago(60), "message": {"content": "help me"}, "permissionMode": "default"}),
                          asst(json!([ask]), ago(50), "m1")], "busy");
        assert_eq!(s["status"], "needs");
        assert_eq!(s["alertKind"], "question");
        assert_eq!(s["alert"], "Asked you a question: Which table should I use?");
        let s = session(&[json!({"type": "user", "timestamp": ago(60), "message": {"content": "help me"}}),
                          asst(json!([ask]), ago(50), "m1"),
                          json!({"type": "user", "timestamp": ago(40), "message": {"content": [{"type": "tool_result", "tool_use_id": "ask1", "content": "ok"}]}})], "idle");
        assert_eq!(s["status"], "idle");
        // a Bash call open for a minute in default mode is only a hint
        let s = session(&[json!({"type": "user", "timestamp": ago(65), "message": {"content": "run it"}, "permissionMode": "default"}),
                          asst(json!([{"type": "tool_use", "id": "b1", "name": "Bash", "input": {"command": "ls"}}]), ago(60), "m1")], "busy");
        assert_eq!(s["status"], "working");
        assert_eq!(s["likelyAwaiting"], "May be waiting for permission to run Bash");
        assert!(s["alert"].is_null());
        for st in LIVE_WAIT_STATES {
            let s = session(&[json!({"type": "user", "timestamp": ago(30), "message": {"content": "hi"}})], st);
            assert_eq!((s["status"].as_str(), s["alertKind"].as_str()), (Some("needs"), Some("waiting")));
        }
        assert_eq!(session(&[json!({"type": "user", "timestamp": ago(30), "message": {"content": "hi"}})], "mystery")["status"], "idle");
    }

    #[test]
    fn payload_reads_the_fatigue_toggle() {
        let (_d, app) = app();
        let path = app.paths.projects_dir.join("proj").join(format!("{}.jsonl", SID));
        let now = py::now_ts();
        write_jsonl(&path, &[
            json!({"type": "user", "timestamp": py::iso_from_ts(now - 70.0 * 60.0), "message": {"content": "build it"}}),
            json!({"type": "assistant", "timestamp": py::iso_from_ts(now - 40.0 * 60.0), "message": {"content": [{"type": "tool_use", "id": "t1", "name": "Bash", "input": {}}]}}),
        ]);
        for on in [true, false] {
            app.save_config(&json!({"creatureFatigue": on})).unwrap();
            let p = app.build_payload();
            assert_eq!(p["config"]["creatureFatigue"], on);
            assert_eq!(p["sessions"].as_array().unwrap().len(), 1);
            assert_eq!(card_has_fatigue(&p["sessions"][0]), on);
        }
        let mut busy = match json!({"sessionId": SID, "kind": "interactive", "status": "busy"}) {
            Value::Object(o) => o,
            _ => unreachable!(),
        };
        let c = app.build_session(&busy, &HashMap::new(), true);
        assert_eq!(c["creature"]["fatigue"]["phase"], "active");
        assert!(c["creature"]["fatigue"]["loadMins"].as_i64().unwrap() >= 39);
        busy.insert("status".into(), json!("idle"));
        let c = app.build_session(&busy, &HashMap::new(), true);
        assert_eq!(c["creature"]["fatigue"]["phase"], "resting");
    }

    #[test]
    fn memo_invalidation_mid_build_is_not_fresh() {
        let (_d, app) = app();
        app.invalidate_payload_memo();
        let a = app.build_payload_memo();
        let b = app.build_payload_memo();
        assert!(Arc::ptr_eq(&a, &b));
        let (b1, s1) = app.build_payload_blob();
        let (b2, s2) = app.build_payload_blob();
        assert!(Arc::ptr_eq(&b1, &b2));
        assert_eq!(s1, s2);
        app.invalidate_payload_memo();
        assert!(!Arc::ptr_eq(&app.build_payload_memo(), &a));
    }

    #[test]
    fn cursor_heading_from_state_db() {
        let (_d, app) = app();
        let sid = "33333333-3333-4333-8333-333333333333";
        let path = app.paths.cursor_projects_dir.join("Users-ada-demo/agent-transcripts").join(sid).join(format!("{}.jsonl", sid));
        write_jsonl(&path, &[json!({"role": "user", "message": {"content": [{"type": "text", "text": "<user_query>You're reading it exactly right and the first message is long</user_query>"}]}})]);
        let con = rusqlite::Connection::open(&app.paths.cursor_state_db).unwrap();
        con.execute("CREATE TABLE composerHeaders (composerId TEXT PRIMARY KEY, value TEXT)", []).unwrap();
        con.execute("INSERT INTO composerHeaders VALUES (?1, ?2)", [sid, "{\"name\": \"  Extend chat data support  \"}"]).unwrap();
        drop(con);
        let card = app.build_archived_session(&path, sid, &HashMap::new(), true, Some("idle"));
        assert_eq!(card["title"], "Extend chat data support");
        assert_eq!(card["kind"], "cursor");
        let agg = app.scan_file(&path).unwrap();
        assert_eq!(agg.folder, "Users-ada-demo");
        assert_eq!(agg.prompt_count, 1);
    }
}
