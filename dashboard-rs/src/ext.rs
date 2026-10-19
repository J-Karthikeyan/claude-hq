//! HQ 2.5 local proxy extensions (ext_skills / ext_loot / ext_prestige /
//! ext_maps / ext_cups / ext_boss). Each forwards only a validated id or an
//! allowlisted body to the paired Arena; the skill and loot reporters send
//! counts only, and only when the user opted in (config workSignals).

use crate::app::{is_rid, App};
use crate::arena::{valid_user_id, USER_ID_RE};
use crate::py::{self, Obj};
use crate::pyjson;
use crate::worksignals::{self, SKILL_CATS};
use chrono::Duration;
use indexmap::IndexMap;
use regex::Regex;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, LazyLock, Mutex, RwLock};

pub type Resp = (u16, Value);
pub type GetFn = fn(&App, &dyn Fn(&str) -> String) -> Resp;
pub type PostFn = fn(&App, &Value) -> Resp;

/// GET routes in dashboard.py's EXT order (skills, loot, prestige, maps, cups, boss, skate, notch).
pub const GET: &[(&str, GetFn)] = &[
    ("/api/arena/skills", get_skills),
    ("/api/skills/local", get_local),
    ("/api/arena/loot", get_loot),
    ("/api/loot/status", get_loot_status),
    ("/api/arena/prestige", get_prestige),
    ("/api/arena/maps", list_maps),
    ("/api/arena/maps/one", one_map),
    ("/api/arena/maps/featured", featured_maps),
    ("/api/arena/cups", cups),
    ("/api/arena/cups/one", cup_one),
    ("/api/arena/cups/season", cup_season),
    ("/api/arena/cups/trophies", cup_trophies),
    ("/api/arena/boss", boss_state),
    ("/api/arena/boss/badges", boss_badges),
    ("/api/arena/skate", skate_board),
    ("/api/notch/status", crate::notch::get_status),
];

pub const POST: &[(&str, PostFn)] = &[
    ("/api/arena/skills/title", post_title),
    ("/api/arena/loot/open", post_loot_open),
    ("/api/arena/prestige/claim", post_prestige_claim),
    ("/api/arena/maps/save", save_map),
    ("/api/arena/maps/like", like_map),
    ("/api/arena/maps/report", report_map),
    ("/api/arena/maps/delete", delete_map),
    ("/api/arena/maps/hide", hide_map),
    ("/api/arena/boss/fight", boss_fight),
    ("/api/arena/skate/combo", skate_combo),
];

pub type DayCountsFn = Arc<dyn Fn(i64) -> IndexMap<String, IndexMap<String, i64>> + Send + Sync>;
pub type GhFn = Arc<dyn Fn(&[String]) -> Option<(i32, String)> + Send + Sync>;

#[derive(Default)]
pub struct ExtState {
    pub skills_last: Mutex<Option<Value>>,
    pub skills_sent: Mutex<HashMap<String, IndexMap<String, i64>>>,
    pub loot_last: Mutex<Option<String>>,
    pub loot_lock: Mutex<()>,
    /// Test seams: worksignals.day_counts and the `gh` runner / which.
    pub day_counts_hook: RwLock<Option<DayCountsFn>>,
    pub gh_hook: RwLock<Option<GhFn>>,
    pub gh_which_hook: RwLock<Option<bool>>,
}

fn not_paired() -> Resp {
    (400, json!({"error": "not paired"}))
}

fn bad(msg: &str) -> Resp {
    (400, json!({"error": msg}))
}

impl App {
    /// worksignals.day_counts(n): the last n UTC days across every transcript.
    pub fn day_counts(&self, n: i64) -> IndexMap<String, IndexMap<String, i64>> {
        if let Some(h) = self.ext.day_counts_hook.read().unwrap().clone() {
            return h(n);
        }
        let want = worksignals::last_days(n, py::utc_today());
        let oldest = py::now_ts() - (want.len() as f64 + 2.0) * 86400.0;
        let mut aggs = Vec::new();
        for p in self.iter_transcript_paths() {
            let mt = std::fs::metadata(&p).ok().and_then(|m| m.modified().ok()).and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok());
            if let Some(mt) = mt {
                if mt.as_secs_f64() < oldest {
                    continue;
                }
            }
            if let Some(a) = self.scan_file(&p) {
                aggs.push(a);
            }
        }
        worksignals::merge_days(aggs.iter().map(|a| a.ws_days.as_ref()), &want)
    }

    fn work_signals_on(&self) -> bool {
        self.load_config().get("workSignals") == Some(&Value::Bool(true))
    }
}

fn to_value(m: &IndexMap<String, IndexMap<String, i64>>) -> Value {
    Value::Object(m.iter().map(|(d, c)| (d.clone(), Value::Object(c.iter().map(|(k, v)| (k.clone(), json!(v))).collect()))).collect())
}

// ---------------------------------------------------------------------------
// skills
// ---------------------------------------------------------------------------

static TITLE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(&format!(r"^(?:{})-(?:3|5)$", SKILL_CATS.join("|"))).unwrap());

fn get_skills(app: &App, arg: &dyn Fn(&str) -> String) -> Resp {
    let who = Some(arg("u")).filter(|s| !s.is_empty()).unwrap_or_else(|| "me".into());
    if app.authed().is_none() {
        return not_paired();
    }
    if who != "me" && !USER_ID_RE.is_match(&who) {
        return bad("bad user id");
    }
    let (t, b) = app.authed().unwrap();
    app.arena_request("GET", &format!("{}/v1/skills/{}", b, who), Some(&t), None)
}

fn post_title(app: &App, body: &Value) -> Resp {
    let Some(Value::Object(body)) = Some(body) else { return bad("title required") };
    let Some(title) = body.get("title") else { return bad("title required") };
    if !title.is_null() && !title.as_str().map(|s| TITLE_RE.is_match(s)).unwrap_or(false) {
        return bad("unknown title");
    }
    let Some((t, b)) = app.authed() else { return not_paired() };
    app.arena_request("POST", &format!("{}/v1/skills/title", b), Some(&t), Some(&json!({"title": title})))
}

fn get_local(app: &App, _arg: &dyn Fn(&str) -> String) -> Resp {
    let days = worksignals::report_counts(&app.day_counts(7));
    let last = app.ext.skills_last.lock().unwrap().clone();
    (200, json!({"enabled": app.work_signals_on(), "days": to_value(&days), "lastReport": last}))
}

/// Send the last seven days' skill counts when paired and opted in.
pub fn skills_report_once(app: &App) -> usize {
    if !app.work_signals_on() {
        return 0;
    }
    let Some((token, base)) = app.authed() else { return 0 };
    let days = worksignals::report_counts(&app.day_counts(7));
    let mut posted = 0;
    let mut ok = true;
    let mut code_seen = 0u16;
    let mut keys: Vec<&String> = days.keys().collect();
    keys.sort();
    for day in keys {
        let counts = &days[day];
        if counts.values().all(|v| *v == 0) {
            continue;
        }
        if app.ext.skills_sent.lock().unwrap().get(day) == Some(counts) {
            continue;
        }
        let body = json!({"day": day, "counts": counts.iter().map(|(k, v)| (k.clone(), json!(v))).collect::<Map<String, Value>>()});
        let (code, _) = app.arena_request("POST", &format!("{}/v1/skills/report", base), Some(&token), Some(&body));
        posted += 1;
        code_seen = code;
        if (200..300).contains(&code) {
            app.ext.skills_sent.lock().unwrap().insert(day.clone(), counts.clone());
        } else {
            ok = false;
        }
    }
    let keep: HashSet<&String> = days.keys().collect();
    app.ext.skills_sent.lock().unwrap().retain(|d, _| keep.contains(d));
    if posted > 0 {
        *app.ext.skills_last.lock().unwrap() = Some(json!({
            "at": py::now_utc().format("%Y-%m-%dT%H:%M:%SZ").to_string(), "ok": ok, "days": posted, "code": code_seen,
        }));
    }
    posted
}

pub fn start_skills_reporter(app: Arc<App>) {
    std::thread::Builder::new()
        .name("skills-reporter".into())
        .spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(45));
            let mut last = 0.0;
            let mut was_on = false;
            loop {
                let on = app.work_signals_on() && app.authed().is_some();
                if on && (!was_on || py::now_ts() - last >= 900.0) {
                    last = py::now_ts();
                    skills_report_once(&app);
                }
                was_on = on;
                std::thread::sleep(std::time::Duration::from_secs(60));
            }
        })
        .ok();
}

// ---------------------------------------------------------------------------
// loot
// ---------------------------------------------------------------------------

pub const LOOT_TYPES: [&str; 3] = ["pr_merged", "tests_green", "focus_long"];
const LOOT_GH_LIMIT: usize = 50;
const LOOT_PR_WINDOW_DAYS: i64 = 7;
const LOOT_SEEN_MAX: usize = 500;
const LOOT_KEEP_DAYS: i64 = 3;
const LOOT_MAX_N: i64 = 5;
static CHEST_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^ch-[0-9a-f]{32}$").unwrap());

impl App {
    /// The loot reporter's state, self-healing (fresh salt on a bad file).
    pub fn loot_load_state(&self) -> Obj {
        let mut st = match crate::state::load_json_plain(&self.paths.loot_state) {
            Some(Value::Object(o)) => o,
            _ => Map::new(),
        };
        if st.get("salt").and_then(|v| v.as_str()).map(|s| s.chars().count() < 16).unwrap_or(true) {
            st = Map::new();
            st.insert("salt".into(), Value::from(py::token_hex(16)));
        }
        if st.get("seen").map(|v| !v.is_array()).unwrap_or(false) {
            st.shift_remove("seen");
        }
        for k in ["prs", "sent"] {
            if !st.get(k).map(|v| v.is_object()).unwrap_or(false) {
                st.insert(k.into(), json!({}));
            }
        }
        if let Some(Value::Object(sent)) = st.get_mut("sent") {
            sent.retain(|t, v| LOOT_TYPES.contains(&t.as_str()) && v.is_object());
        }
        st
    }

    pub fn loot_save_state(&self, st: &Obj) -> std::io::Result<()> {
        use std::io::Write;
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let path = &self.paths.loot_state;
        let tmp = path.with_file_name(format!("{}.tmp", path.file_name().unwrap().to_string_lossy()));
        let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp)?;
        f.write_all(pyjson::dumps(&Value::Object(st.clone())).as_bytes())?;
        drop(f);
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
        std::fs::rename(&tmp, path)
    }

    fn gh_available(&self) -> bool {
        if let Some(b) = *self.ext.gh_which_hook.read().unwrap() {
            return b;
        }
        crate::actions::which("gh", &std::env::var("PATH").unwrap_or_default()).is_some()
    }

    fn run_gh(&self, argv: &[String]) -> Option<(i32, String)> {
        if let Some(h) = self.ext.gh_hook.read().unwrap().clone() {
            return h(argv);
        }
        let mut cmd = std::process::Command::new(&argv[0]);
        cmd.args(&argv[1..]);
        crate::payload::run_timeout(&mut cmd, 20).ok().map(|(c, out, _)| (c, out))
    }
}

fn loot_hash(salt: &str, url: &str) -> String {
    hex::encode(Sha256::digest(format!("{}|{}", salt, url).as_bytes()))[..24].to_string()
}

/// Count newly merged PRs (by salted url hash) onto today's tally.
fn scan_prs(app: &App, st: &mut Obj, today: chrono::NaiveDate) -> Option<usize> {
    let since = py::date_iso(&(today - Duration::days(LOOT_PR_WINDOW_DAYS)));
    let argv: Vec<String> = ["gh", "search", "prs", "--author=@me", "--merged", "--merged-at", &format!(">={}", since), "--json", "url", "--limit", &LOOT_GH_LIMIT.to_string()]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let (code, out) = app.run_gh(&argv)?;
    if code != 0 {
        return None;
    }
    let rows: Value = serde_json::from_str(if out.is_empty() { "[]" } else { &out }).ok()?;
    let Value::Array(rows) = rows else { return None };
    let salt = st["salt"].as_str().unwrap_or("").to_string();
    let hashes: Vec<String> = rows.iter().filter_map(|r| r.get("url").and_then(|u| u.as_str()).filter(|u| !u.is_empty())).map(|u| loot_hash(&salt, u)).collect();
    if !st.contains_key("seen") {
        let start = hashes.len().saturating_sub(LOOT_SEEN_MAX);
        st.insert("seen".into(), json!(hashes[start..].to_vec()));
        return Some(0);
    }
    let seen: HashSet<String> = st["seen"].as_array().unwrap().iter().filter_map(|v| v.as_str().map(String::from)).collect();
    let mut uniq: Vec<String> = Vec::new();
    for h in hashes {
        if !uniq.contains(&h) && !seen.contains(&h) {
            uniq.push(h);
        }
    }
    if !uniq.is_empty() {
        let mut all: Vec<Value> = st["seen"].as_array().unwrap().clone();
        all.extend(uniq.iter().map(|h| json!(h)));
        let start = all.len().saturating_sub(LOOT_SEEN_MAX);
        st.insert("seen".into(), Value::Array(all[start..].to_vec()));
        let day = py::date_iso(&today);
        let prs = st.get_mut("prs").unwrap().as_object_mut().unwrap();
        let cur = prs.get(&day).and_then(py::py_int).unwrap_or(0);
        prs.insert(day, json!(cur + uniq.len() as i64));
    }
    Some(uniq.len())
}

/// {type: {day: cumulative count}} for today and yesterday.
fn loot_local_counts(app: &App, st: &Obj, today: chrono::NaiveDate) -> IndexMap<String, IndexMap<String, i64>> {
    let days = [py::date_iso(&(today - Duration::days(1))), py::date_iso(&today)];
    let ws = app.day_counts(days.len() as i64);
    let mut out: IndexMap<String, IndexMap<String, i64>> = LOOT_TYPES.iter().map(|t| (t.to_string(), IndexMap::new())).collect();
    for d in &days {
        if let Some(row) = ws.get(d) {
            for t in ["tests_green", "focus_long"] {
                let n = row.get(t).copied().unwrap_or(0);
                if n > 0 {
                    out.get_mut(t).unwrap().insert(d.clone(), n);
                }
            }
        }
        let n = st["prs"].get(d).filter(|v| py::truthy(v)).and_then(py::py_int).unwrap_or(0);
        if n > 0 {
            out.get_mut("pr_merged").unwrap().insert(d.clone(), n);
        }
    }
    out
}

fn sent_count(st: &Obj, t: &str, d: &str) -> i64 {
    st["sent"].get(t).and_then(|m| m.get(d)).filter(|v| py::truthy(v)).and_then(py::py_int).unwrap_or(0)
}

/// {type: units detected but not yet sent}.
fn loot_pending(st: &Obj, counts: &IndexMap<String, IndexMap<String, i64>>) -> Map<String, Value> {
    let mut out: IndexMap<String, i64> = IndexMap::new();
    for t in LOOT_TYPES {
        for (d, n) in counts.get(t).into_iter().flatten() {
            let delta = n - sent_count(st, t, d);
            if delta > 0 {
                *out.entry(t.to_string()).or_insert(0) += delta;
            }
        }
    }
    out.into_iter().map(|(k, v)| (k, json!(v))).collect()
}

pub fn loot_request_id(salt: &str, uid: &str, kind: &str, day: &str, cumulative: i64) -> String {
    let raw = format!("{}|{}|{}|{}|{}", salt, uid, kind, day, cumulative);
    format!("loot-{}", &hex::encode(Sha256::digest(raw.as_bytes()))[..40])
}

fn loot_report(app: &App, st: &mut Obj, counts: &IndexMap<String, IndexMap<String, i64>>, token: &str, base: &str, uid: &str) -> Vec<Value> {
    let mut chests = Vec::new();
    let salt = st["salt"].as_str().unwrap_or("").to_string();
    for t in LOOT_TYPES {
        let Some(per) = counts.get(t) else { continue };
        let mut ds: Vec<&String> = per.keys().collect();
        ds.sort();
        for d in ds {
            let target = per[d];
            loop {
                let have = sent_count(st, t, d);
                if target <= have {
                    break;
                }
                let n = LOOT_MAX_N.min(target - have);
                let body = json!({"requestId": loot_request_id(&salt, uid, t, d, have + n), "type": t, "n": n, "day": d});
                let (code, resp) = app.arena_request("POST", &format!("{}/v1/loot/events", base), Some(token), Some(&body));
                if code == 0 || code >= 500 {
                    break;
                }
                let sent = st.get_mut("sent").unwrap().as_object_mut().unwrap();
                let m = sent.entry(t.to_string()).or_insert_with(|| json!({}));
                m.as_object_mut().unwrap().insert(d.clone(), json!(have + n));
                if (200..300).contains(&code) {
                    if let Some(Value::Array(cs)) = resp.get("chests") {
                        chests.extend(cs.iter().filter(|c| c.is_object()).cloned());
                    }
                }
            }
        }
    }
    chests
}

fn loot_prune(st: &mut Obj, today: chrono::NaiveDate) {
    let keep = py::date_iso(&(today - Duration::days(LOOT_KEEP_DAYS)));
    if let Some(Value::Object(prs)) = st.get_mut("prs") {
        prs.retain(|d, _| d.as_str() >= keep.as_str());
    }
    if let Some(Value::Object(sent)) = st.get_mut("sent") {
        for (_, v) in sent.iter_mut() {
            if let Value::Object(m) = v {
                m.retain(|d, _| d.as_str() >= keep.as_str());
            }
        }
    }
}

/// One loot reporter cycle; nothing at all unless paired and opted in.
pub fn loot_run_once(app: &App) -> Value {
    let cfg = app.load_config();
    if !py::opt_truthy(cfg.get("workSignals")) {
        return json!({"skipped": "workSignals is off"});
    }
    let Some((token, base)) = app.authed() else { return json!({"skipped": "not paired"}) };
    let _g = app.ext.loot_lock.lock().unwrap();
    let mut st = app.loot_load_state();
    let today = py::utc_today();
    if py::opt_truthy(cfg.get("workSignalsPRs")) && app.gh_available() {
        scan_prs(app, &mut st, today);
    }
    let counts = loot_local_counts(app, &st, today);
    let uid = app.load_link().get("handle").filter(|v| py::truthy(v)).map(crate::arena::py_str).unwrap_or_default();
    let chests = loot_report(app, &mut st, &counts, &token, &base, &uid);
    loot_prune(&mut st, today);
    let _ = app.loot_save_state(&st);
    *app.ext.loot_last.lock().unwrap() = Some(py::now_utc().format("%Y-%m-%dT%H:%M:%S+00:00").to_string());
    json!({"chests": chests.len()})
}

pub fn start_loot_reporter(app: Arc<App>) {
    std::thread::Builder::new()
        .name("loot-reporter".into())
        .spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(90));
            loop {
                loot_run_once(&app);
                std::thread::sleep(std::time::Duration::from_secs(1800));
            }
        })
        .ok();
}

fn get_loot(app: &App, _arg: &dyn Fn(&str) -> String) -> Resp {
    match app.authed() {
        Some((t, b)) => app.arena_request("GET", &format!("{}/v1/loot", b), Some(&t), None),
        None => not_paired(),
    }
}

fn get_loot_status(app: &App, _arg: &dyn Fn(&str) -> String) -> Resp {
    let cfg = app.load_config();
    let enabled = py::opt_truthy(cfg.get("workSignals")) && app.authed().is_some();
    let prs = enabled && py::opt_truthy(cfg.get("workSignalsPRs")) && app.gh_available();
    let mut pend = Map::new();
    if enabled {
        let _g = app.ext.loot_lock.lock().unwrap();
        let st = app.loot_load_state();
        pend = loot_pending(&st, &loot_local_counts(app, &st, py::utc_today()));
    }
    let last = app.ext.loot_last.lock().unwrap().clone();
    (200, json!({"enabled": enabled, "prs": prs, "pending": pend, "lastReport": last}))
}

/// re.match with a `$`-anchored pattern also accepts one trailing "\n".
fn py_match(re: &Regex, s: &str) -> bool {
    re.is_match(s) || s.strip_suffix('\n').map(|x| re.is_match(x)).unwrap_or(false)
}

fn post_loot_open(app: &App, body: &Value) -> Resp {
    let cid = body.get("chestId").and_then(|v| v.as_str());
    let Some(cid) = cid.filter(|c| py_match(&CHEST_RE, c)) else { return bad("bad chest id") };
    let rid = match body.get("requestId").and_then(|v| v.as_str()) {
        Some(r) if py_match(&crate::app::RID_RE, r) => r.to_string(),
        _ => format!("open-{}", &py::uuid4_hex()[..24]),
    };
    let Some((t, b)) = app.authed() else { return not_paired() };
    app.arena_request("POST", &format!("{}/v1/loot/open", b), Some(&t), Some(&json!({"chestId": cid, "requestId": rid})))
}

// ---------------------------------------------------------------------------
// prestige
// ---------------------------------------------------------------------------

fn get_prestige(app: &App, arg: &dyn Fn(&str) -> String) -> Resp {
    let u = Some(arg("u")).filter(|s| !s.is_empty()).unwrap_or_else(|| "me".into());
    if u != "me" && !valid_user_id(&u) {
        return bad("bad user id");
    }
    let Some((t, b)) = app.authed() else { return not_paired() };
    app.arena_request("GET", &format!("{}/v1/prestige/{}", b, u), Some(&t), None)
}

fn post_prestige_claim(app: &App, body: &Value) -> Resp {
    let rid = match body.get("requestId").and_then(|v| v.as_str()) {
        Some(r) if py_match(&crate::app::RID_RE, r) => r.to_string(),
        _ => format!("prestige-{}", &py::uuid4_hex()[..24]),
    };
    let Some((t, b)) = app.authed() else { return not_paired() };
    app.arena_request("POST", &format!("{}/v1/prestige", b), Some(&t), Some(&json!({"requestId": rid})))
}

// ---------------------------------------------------------------------------
// maps
// ---------------------------------------------------------------------------

static MAP_ID_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^m-[0-9a-f]{12}$").unwrap());
static CKEY_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^c-[0-9a-f]{12}$").unwrap());
static MAP_ROOM_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(hq_[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}|r_[A-Za-z0-9_-]{22})$").unwrap()
});
static CURSOR_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[0-9a-f]{2,400}$").unwrap());
const MAP_KINDS: [&str; 3] = ["kart", "plat", "fps"];
const MAP_SCOPES: [&str; 3] = ["private", "room", "public"];
const MAP_SORTS: [&str; 3] = ["new", "top", "week"];
const MAP_REASONS: [&str; 4] = ["spam", "offensive", "broken", "other"];
const MAP_MAX_DATA: usize = 12 * 1024;
const MAP_MAX_NAME: usize = 32;
pub const MAP_SAVE_KEYS: [&str; 6] = ["id", "kind", "name", "data", "scope", "roomId"];
const MAP_FORBIDDEN: [&str; 16] = [
    "prompt", "prompts_text", "reply", "text", "content", "path", "paths", "cwd", "folder", "project", "projectName",
    "sessionId", "sessionTitle", "title", "file", "files",
];

fn map_keys(v: &Value, depth: usize, out: &mut HashSet<String>) {
    if depth > 12 {
        out.insert("<deep>".into());
        return;
    }
    match v {
        Value::Object(o) => {
            for (k, x) in o {
                out.insert(k.clone());
                map_keys(x, depth + 1, out);
            }
        }
        Value::Array(a) => a.iter().for_each(|x| map_keys(x, depth + 1, out)),
        _ => {}
    }
}

/// The map name as the Arena will store it, or None when it would be refused.
pub fn clean_map_name(raw: &Value) -> Option<String> {
    let s = py::strip(raw.as_str()?);
    if s.is_empty() || s.chars().count() > MAP_MAX_NAME {
        return None;
    }
    let low = s.to_lowercase();
    if s.contains('<') || s.contains('>') || low.contains("http") || low.contains("www.") {
        return None;
    }
    if s.chars().any(|c| (c as u32) < 32 || (127..160).contains(&(c as u32))) {
        return None;
    }
    Some(s.to_string())
}

fn maps_authed(app: &App) -> Option<(String, String)> {
    app.authed().map(|(t, b)| (t, b.trim_end_matches('/').to_string()))
}

fn list_maps(app: &App, arg: &dyn Fn(&str) -> String) -> Resp {
    let Some((t, b)) = maps_authed(app) else { return not_paired() };
    let mut q: Vec<(&str, String)> = Vec::new();
    let kind = arg("kind");
    if !kind.is_empty() {
        if !MAP_KINDS.contains(&kind.as_str()) {
            return bad("bad kind");
        }
        q.push(("kind", kind));
    }
    let sort = arg("sort");
    if !sort.is_empty() {
        if !MAP_SORTS.contains(&sort.as_str()) {
            return bad("bad sort");
        }
        q.push(("sort", sort));
    }
    let room = arg("room");
    if !room.is_empty() {
        if !MAP_ROOM_RE.is_match(&room) {
            return bad("bad room");
        }
        q.push(("room", room));
    }
    if arg("mine") == "1" {
        q.push(("mine", "1".into()));
    }
    let ckey = arg("ckey");
    if !ckey.is_empty() {
        if !CKEY_RE.is_match(&ckey) {
            return bad("bad ckey");
        }
        q.push(("ckey", ckey));
    }
    let cursor = arg("cursor");
    if !cursor.is_empty() {
        if !CURSOR_RE.is_match(&cursor) {
            return bad("bad cursor");
        }
        q.push(("cursor", cursor));
    }
    let limit = arg("limit");
    if !limit.is_empty() {
        let n: Option<u64> = if limit.bytes().all(|c| c.is_ascii_digit()) { limit.parse().ok() } else { None };
        match n {
            Some(n) if (1..=30).contains(&n) => q.push(("limit", n.to_string())),
            _ => return bad("bad limit"),
        }
    }
    if q.iter().any(|(k, _)| *k == "mine") && q.iter().any(|(k, _)| *k == "room") {
        return bad("pick mine or room");
    }
    let qs = q.iter().map(|(k, v)| format!("{}={}", py::quote_plus(k), py::quote_plus(v))).collect::<Vec<_>>().join("&");
    let url = format!("{}/v1/maps{}", b, if qs.is_empty() { String::new() } else { format!("?{}", qs) });
    app.arena_request("GET", &url, Some(&t), None)
}

fn one_map(app: &App, arg: &dyn Fn(&str) -> String) -> Resp {
    let Some((t, b)) = maps_authed(app) else { return not_paired() };
    let mid = arg("id");
    if !MAP_ID_RE.is_match(&mid) {
        return bad("bad map id");
    }
    app.arena_request("GET", &format!("{}/v1/maps/{}", b, mid), Some(&t), None)
}

fn featured_maps(app: &App, _arg: &dyn Fn(&str) -> String) -> Resp {
    let Some((t, b)) = maps_authed(app) else { return not_paired() };
    app.arena_request("GET", &format!("{}/v1/maps/featured", b), Some(&t), None)
}

/// The exact body forwarded for a save, or an error.
pub fn map_save_body(body: &Value) -> Result<Value, &'static str> {
    let Value::Object(b) = body else { return Err("bad body") };
    if b.keys().any(|k| !MAP_SAVE_KEYS.contains(&k.as_str())) {
        return Err("unexpected field");
    }
    let mut out = Map::new();
    if let Some(mid) = b.get("id").filter(|v| !v.is_null()) {
        match mid.as_str() {
            Some(m) if MAP_ID_RE.is_match(m) => {
                out.insert("id".into(), json!(m));
            }
            _ => return Err("bad map id"),
        }
    }
    let kind = b.get("kind").and_then(|v| v.as_str()).filter(|k| MAP_KINDS.contains(k)).ok_or("bad kind")?;
    out.insert("kind".into(), json!(kind));
    let name = clean_map_name(b.get("name").unwrap_or(&Value::Null)).ok_or("names are 1-32 characters, with no links or < >")?;
    out.insert("name".into(), json!(name));
    let data = b.get("data").filter(|v| v.is_object()).ok_or("data must be an object")?;
    let size = pyjson::dumps_with(data, pyjson::Style { indent: None, compact: true, ensure_ascii: false }).len();
    if size > MAP_MAX_DATA {
        return Err("map data is over 12 KiB");
    }
    let mut keys = HashSet::new();
    map_keys(data, 0, &mut keys);
    if keys.contains("<deep>") || MAP_FORBIDDEN.iter().any(|k| keys.contains(*k)) {
        return Err("map data has a field maps don't use");
    }
    out.insert("data".into(), data.clone());
    let scope = b.get("scope").and_then(|v| v.as_str()).filter(|s| MAP_SCOPES.contains(s)).ok_or("bad scope")?;
    out.insert("scope".into(), json!(scope));
    let room = b.get("roomId").filter(|v| !v.is_null());
    if scope == "room" {
        match room.and_then(|v| v.as_str()) {
            Some(r) if MAP_ROOM_RE.is_match(r) => {
                out.insert("roomId".into(), json!(r));
            }
            _ => return Err("bad room"),
        }
    } else if room.is_some() {
        return Err("roomId goes with scope room");
    }
    Ok(Value::Object(out))
}

fn save_map(app: &App, body: &Value) -> Resp {
    let Some((t, b)) = maps_authed(app) else { return not_paired() };
    match map_save_body(body) {
        Ok(clean) => app.arena_request("POST", &format!("{}/v1/maps", b), Some(&t), Some(&clean)),
        Err(e) => bad(e),
    }
}

fn map_id(body: &Value) -> Option<String> {
    body.get("id").and_then(|v| v.as_str()).filter(|m| MAP_ID_RE.is_match(m)).map(String::from)
}

fn like_map(app: &App, body: &Value) -> Resp {
    let Some((t, b)) = maps_authed(app) else { return not_paired() };
    match (map_id(body), body.get("on").and_then(|v| v.as_bool())) {
        (Some(mid), Some(on)) => app.arena_request("POST", &format!("{}/v1/maps/{}/like", b, mid), Some(&t), Some(&json!({"on": on}))),
        _ => bad("bad like"),
    }
}

fn report_map(app: &App, body: &Value) -> Resp {
    let Some((t, b)) = maps_authed(app) else { return not_paired() };
    let reason = body.get("reason").and_then(|v| v.as_str()).filter(|r| MAP_REASONS.contains(r));
    match (map_id(body), reason) {
        (Some(mid), Some(r)) => app.arena_request("POST", &format!("{}/v1/maps/{}/report", b, mid), Some(&t), Some(&json!({"reason": r}))),
        _ => bad("bad report"),
    }
}

fn delete_map(app: &App, body: &Value) -> Resp {
    let Some((t, b)) = maps_authed(app) else { return not_paired() };
    match map_id(body) {
        Some(mid) => app.arena_request("POST", &format!("{}/v1/maps/{}/delete", b, mid), Some(&t), Some(&json!({}))),
        None => bad("bad map id"),
    }
}

fn hide_map(app: &App, body: &Value) -> Resp {
    let Some((t, b)) = maps_authed(app) else { return not_paired() };
    match (map_id(body), body.get("hidden").and_then(|v| v.as_bool())) {
        (Some(mid), Some(h)) => app.arena_request("POST", &format!("{}/v1/maps/{}/hide", b, mid), Some(&t), Some(&json!({"hidden": h}))),
        _ => bad("bad hide"),
    }
}

// ---------------------------------------------------------------------------
// cups
// ---------------------------------------------------------------------------

const CUP_GAMES: [&str; 6] = ["kart", "plat", "golf", "fps", "type", "bowl"];
static CUP_ID_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(&format!(r"^cup-\d{{4}}-W(0[1-9]|[1-4]\d|5[0-3])-({})$", CUP_GAMES.join("|"))).unwrap());
static SEASON_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\d{4}-(0[1-9]|1[0-2])$").unwrap());

fn cups_get(app: &App, tail: &str) -> Resp {
    match app.authed() {
        Some((t, b)) => app.arena_request("GET", &format!("{}{}", b, tail), Some(&t), None),
        None => not_paired(),
    }
}

fn cups(app: &App, _arg: &dyn Fn(&str) -> String) -> Resp {
    cups_get(app, "/v1/cups")
}

fn cup_one(app: &App, arg: &dyn Fn(&str) -> String) -> Resp {
    let cid = arg("id");
    if !CUP_ID_RE.is_match(&cid) {
        return bad("bad cup id");
    }
    cups_get(app, &format!("/v1/cups/{}", cid))
}

fn cup_season(app: &App, arg: &dyn Fn(&str) -> String) -> Resp {
    let s = arg("season");
    if !s.is_empty() && !SEASON_RE.is_match(&s) {
        return bad("season must be YYYY-MM");
    }
    cups_get(app, &format!("/v1/cups/season{}", if s.is_empty() { String::new() } else { format!("?season={}", s) }))
}

fn cup_trophies(app: &App, arg: &dyn Fn(&str) -> String) -> Resp {
    let u = Some(arg("u")).filter(|s| !s.is_empty()).unwrap_or_else(|| "me".into());
    if u != "me" && !USER_ID_RE.is_match(&u) {
        return bad("bad user id");
    }
    cups_get(app, &format!("/v1/cups/trophies/{}", u))
}

// ---------------------------------------------------------------------------
// boss
// ---------------------------------------------------------------------------

const BOSS_TEAM_MAX: usize = 6;
const SPEC_KEYS: [&str; 5] = ["sp", "st", "br", "mg", "sh"];
static MEGA_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[a-z0-9-]{1,32}$").unwrap());

/// 1..6 battle specs with exactly {sp, st, br, mg, sh}, or None.
pub fn clean_team(team: Option<&Value>) -> Option<Value> {
    let Some(Value::Array(team)) = team else { return None };
    if team.is_empty() || team.len() > BOSS_TEAM_MAX {
        return None;
    }
    let mut out = Vec::new();
    for t in team {
        let Value::Object(t) = t else { return None };
        let keys: HashSet<&str> = t.keys().map(|k| k.as_str()).collect();
        if keys != SPEC_KEYS.iter().copied().collect::<HashSet<_>>() {
            return None;
        }
        let sp = py::strict_int(&t["sp"]).filter(|v| (0..1000).contains(v))?;
        let st = py::strict_int(&t["st"]).filter(|v| (0..=4).contains(v))?;
        let br = &t["br"];
        if !br.is_null() && py::strict_int(br).filter(|v| *v > 0 && *v < 2000).is_none() {
            return None;
        }
        let mg = &t["mg"];
        if !mg.is_null() && !mg.as_str().map(|s| MEGA_RE.is_match(s)).unwrap_or(false) {
            return None;
        }
        let sh = match &t["sh"] {
            Value::Bool(b) => *b,
            v => match py::strict_int(v) {
                Some(0) => false,
                Some(1) => true,
                _ => return None,
            },
        };
        out.push(json!({"sp": sp, "st": st, "br": br, "mg": mg, "sh": sh}));
    }
    Some(Value::Array(out))
}

fn boss_state(app: &App, _arg: &dyn Fn(&str) -> String) -> Resp {
    match app.authed() {
        Some((t, b)) => app.arena_request("GET", &format!("{}/v1/boss", b), Some(&t), None),
        None => not_paired(),
    }
}

fn boss_badges(app: &App, arg: &dyn Fn(&str) -> String) -> Resp {
    let Some((t, b)) = app.authed() else { return not_paired() };
    let u = Some(arg("u")).filter(|s| !s.is_empty()).unwrap_or_else(|| "me".into());
    if u != "me" && !valid_user_id(&u) {
        return bad("bad user id");
    }
    app.arena_request("GET", &format!("{}/v1/boss/badges/{}", b, u), Some(&t), None)
}

fn boss_fight(app: &App, body: &Value) -> Resp {
    let Some((t, b)) = app.authed() else { return not_paired() };
    let Some(team) = clean_team(body.get("team")) else { return bad("pick 1 to 6 Pokémon") };
    let rid = match body.get("requestId") {
        None | Some(Value::Null) => format!("boss-{}", py::uuid4_hex()),
        Some(Value::String(s)) if is_rid(s) => s.clone(),
        _ => return bad("bad request id"),
    };
    app.arena_request("POST", &format!("{}/v1/boss/fight", b), Some(&t), Some(&json!({"requestId": rid, "team": team})))
}

// ---------------------------------------------------------------------------
// skate (ext_skate.py): the Arena City skatepark's weekly board. The only
// thing that leaves the machine is a combo's score: two whole numbers.
// ---------------------------------------------------------------------------

pub const SKATE_MAX_TOTAL: i64 = 200000;
pub const SKATE_MAX_TRICKS: i64 = 10;

/// _whole(v, lo, hi): an int, or an integral float, inside [lo, hi]; never a bool.
pub fn whole(v: Option<&Value>, lo: i64, hi: i64) -> Option<i64> {
    let Some(Value::Number(n)) = v else { return None };
    let x = if let Some(i) = n.as_i64() {
        i as f64
    } else {
        let f = n.as_f64()?;
        if f.is_nan() || f.fract() != 0.0 {
            return None;
        }
        f
    };
    (lo as f64 <= x && x <= hi as f64).then_some(x as i64)
}

fn skate_board(app: &App, _arg: &dyn Fn(&str) -> String) -> Resp {
    let Some((t, b)) = app.authed() else { return not_paired() };
    app.arena_request("GET", &format!("{}/v1/skate", b), Some(&t), None)
}

fn skate_combo(app: &App, body: &Value) -> Resp {
    let total = whole(body.get("total"), 1, SKATE_MAX_TOTAL);
    let tricks = whole(body.get("tricks"), 1, SKATE_MAX_TRICKS);
    let (Some(total), Some(tricks)) = (total, tricks) else {
        return bad("a combo is a total (1-200000) and a trick count (1-10)");
    };
    let Some((t, b)) = app.authed() else { return not_paired() };
    app.arena_request("POST", &format!("{}/v1/skate", b), Some(&t), Some(&json!({"total": total, "tricks": tricks})))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::testutil::*;
    use crate::arena::tests::{leaks, stub};

    fn arg(m: &[(&str, &str)]) -> impl Fn(&str) -> String {
        let m: HashMap<String, String> = m.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        move |k| m.get(k).cloned().unwrap_or_default()
    }

    const UID: &str = "0f8fad5b-d9cb-469f-a165-70867728950e";

    #[test]
    fn skills_report_is_exactly_seven_counts() {
        let (_d, app) = app();
        let sent = stub(&app, "https://arena.example", Arc::new(Mutex::new((200, json!({"ok": true})))));
        let full: IndexMap<String, i64> = worksignals::KEYS.iter().map(|k| (k.to_string(), 3)).collect();
        let mut two = full.clone();
        two.insert("tests".into(), 7);
        let days: IndexMap<String, IndexMap<String, i64>> = [("2026-10-09".to_string(), full), ("2026-10-10".to_string(), two)].into_iter().collect();
        *app.ext.day_counts_hook.write().unwrap() = Some(Arc::new(move |_| days.clone()));
        assert_eq!(skills_report_once(&app), 0); // workSignals off
        app.save_config(&json!({"workSignals": true})).unwrap();
        assert_eq!(skills_report_once(&app), 2);
        {
            let s = sent.lock().unwrap();
            for (m, url, tok, body) in s.iter() {
                assert_eq!((m.as_str(), url.as_str(), tok.as_deref()), ("POST", "https://arena.example/v1/skills/report", Some("T")));
                let body = body.as_ref().unwrap();
                let keys: Vec<&String> = body["counts"].as_object().unwrap().keys().collect();
                assert_eq!(keys, SKILL_CATS.iter().collect::<Vec<_>>());
            }
            assert_eq!(s[1].3.as_ref().unwrap()["counts"]["tests"], 7);
        }
        assert_eq!(skills_report_once(&app), 0); // unchanged days are not resent
        assert_eq!(app.ext.skills_last.lock().unwrap().as_ref().unwrap()["days"], 2);
    }

    #[test]
    fn skills_routes() {
        let (_d, app) = app();
        let sent = stub(&app, "https://arena.example", Arc::new(Mutex::new((200, json!({"ok": true})))));
        get_skills(&app, &arg(&[]));
        get_skills(&app, &arg(&[("u", UID)]));
        for b in ["../me", "x", "me?x=1"] {
            assert_eq!(get_skills(&app, &arg(&[("u", b)])).0, 400);
        }
        assert_eq!(post_title(&app, &json!({"title": "tests-3"})).0, 200);
        post_title(&app, &json!({"title": null}));
        for b in [json!({}), json!({"title": "tests-4"}), json!({"title": "magic-3"}), json!({"title": 3}), json!({"title": "tests-3 "}), json!(null)] {
            assert_eq!(post_title(&app, &b).0, 400);
        }
        let s = sent.lock().unwrap();
        assert_eq!(s.iter().map(|x| x.1.as_str()).collect::<Vec<_>>(), vec![
            "https://arena.example/v1/skills/me", &format!("https://arena.example/v1/skills/{}", UID),
            "https://arena.example/v1/skills/title", "https://arena.example/v1/skills/title"]);
        assert_eq!(s[3].3, Some(json!({"title": null})));
        let (code, local) = get_local(&app, &arg(&[]));
        assert_eq!(code, 200);
        assert_eq!(local.as_object().unwrap().keys().collect::<Vec<_>>(), vec!["enabled", "days", "lastReport"]);
    }

    fn loot_setup() -> (tempfile::TempDir, Arc<App>, crate::arena::tests::Sent, Arc<Mutex<Vec<Value>>>, Arc<Mutex<(u16, Value)>>) {
        let (d, app) = app();
        let reply = Arc::new(Mutex::new((200, json!({"accepted": 1, "chests": [{"id": format!("ch-{}", "a".repeat(32))}]}))));
        let sent = stub(&app, "https://arena.example", reply.clone());
        app.save_config(&json!({"workSignals": true, "workSignalsPRs": true})).unwrap();
        let rows = Arc::new(Mutex::new((0..3).map(|i| json!({"url": format!("https://github.com/acme/secret-repo/pull/{}", i + 1)})).collect::<Vec<_>>()));
        let r2 = rows.clone();
        *app.ext.gh_which_hook.write().unwrap() = Some(true);
        *app.ext.gh_hook.write().unwrap() = Some(Arc::new(move |argv| {
            assert_eq!(&argv[..6], &["gh", "search", "prs", "--author=@me", "--merged", "--merged-at"]);
            Some((0, pyjson::dumps(&Value::Array(r2.lock().unwrap().clone()))))
        }));
        *app.ext.day_counts_hook.write().unwrap() = Some(Arc::new(|_| IndexMap::new()));
        (d, app, sent, rows, reply)
    }

    #[test]
    fn loot_baseline_then_one_new_merge() {
        let (_d, app, sent, rows, reply) = loot_setup();
        loot_run_once(&app);
        assert!(sent.lock().unwrap().is_empty(), "no backfill burst");
        let raw = std::fs::read_to_string(&app.paths.loot_state).unwrap();
        assert!(!raw.contains("github") && !raw.contains("secret-repo"));
        loot_run_once(&app);
        assert!(sent.lock().unwrap().is_empty());
        rows.lock().unwrap().push(json!({"url": "https://github.com/acme/secret-repo/pull/4"}));
        loot_run_once(&app);
        {
            let s = sent.lock().unwrap();
            assert_eq!(s.len(), 1);
            let body = s[0].3.as_ref().unwrap();
            assert_eq!(body.as_object().unwrap().keys().collect::<Vec<_>>(), vec!["requestId", "type", "n", "day"]);
            assert_eq!((body["type"].as_str(), body["n"].as_i64()), (Some("pr_merged"), Some(1)));
            assert!(body["requestId"].as_str().unwrap().starts_with("loot-"));
            assert!(!pyjson::dumps(body).contains("github.com"));
        }
        loot_run_once(&app);
        assert_eq!(sent.lock().unwrap().len(), 1);
        // a 4xx marks the delta sent; a 5xx is retried with the same requestId
        rows.lock().unwrap().push(json!({"url": "https://github.com/acme/secret-repo/pull/5"}));
        *reply.lock().unwrap() = (503, json!({"error": "busy"}));
        loot_run_once(&app);
        *reply.lock().unwrap() = (200, json!({"accepted": 1, "chests": []}));
        loot_run_once(&app);
        loot_run_once(&app);
        let s = sent.lock().unwrap();
        assert_eq!(s.len(), 3);
        assert_eq!(s[1].3.as_ref().unwrap()["requestId"], s[2].3.as_ref().unwrap()["requestId"]);
    }

    #[test]
    fn loot_off_or_unpaired_sends_nothing() {
        let (_d, app, sent, _rows, _reply) = loot_setup();
        app.save_config(&json!({"workSignals": false})).unwrap();
        assert!(loot_run_once(&app).get("skipped").is_some());
        assert!(!app.paths.loot_state.exists());
        app.save_config(&json!({"workSignals": true})).unwrap();
        app.save_link(&json!({"url": "https://arena.example"})).unwrap();
        loot_run_once(&app);
        assert_eq!(get_loot(&app, &arg(&[])).0, 400);
        assert!(sent.lock().unwrap().is_empty());
    }

    #[test]
    fn loot_worksignals_deltas_split_by_five() {
        let (_d, app, sent, _rows, _reply) = loot_setup();
        app.save_config(&json!({"workSignalsPRs": false})).unwrap();
        let today = py::date_iso(&py::utc_today());
        let yday = py::date_iso(&(py::utc_today() - Duration::days(1)));
        let counts = Arc::new(Mutex::new(IndexMap::<String, IndexMap<String, i64>>::new()));
        let mk = |pairs: &[(&str, i64)]| pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect::<IndexMap<String, i64>>();
        counts.lock().unwrap().insert(today.clone(), mk(&[("tests_green", 2), ("focus_long", 1), ("tests", 40)]));
        counts.lock().unwrap().insert(yday.clone(), mk(&[("tests_green", 1)]));
        let c2 = counts.clone();
        *app.ext.day_counts_hook.write().unwrap() = Some(Arc::new(move |_| c2.lock().unwrap().clone()));
        let (_, st) = get_loot_status(&app, &arg(&[]));
        assert_eq!(st["pending"], json!({"tests_green": 3, "focus_long": 1}));
        loot_run_once(&app);
        let mut got: Vec<(String, String, i64)> = sent.lock().unwrap().iter().map(|x| {
            let b = x.3.as_ref().unwrap();
            (b["type"].as_str().unwrap().to_string(), b["day"].as_str().unwrap().to_string(), b["n"].as_i64().unwrap())
        }).collect();
        got.sort();
        let mut want = vec![("focus_long".to_string(), today.clone(), 1), ("tests_green".to_string(), today.clone(), 2), ("tests_green".to_string(), yday, 1)];
        want.sort();
        assert_eq!(got, want);
        counts.lock().unwrap().get_mut(&today).unwrap().insert("tests_green".into(), 9);
        loot_run_once(&app);
        let s = sent.lock().unwrap();
        assert_eq!(s[s.len() - 2..].iter().map(|x| x.3.as_ref().unwrap()["n"].as_i64().unwrap()).collect::<Vec<_>>(), vec![5, 2]);
        drop(s);
        let (_, st) = get_loot_status(&app, &arg(&[]));
        assert_eq!(st["pending"], json!({}));
        assert_eq!((st["enabled"].clone(), st["prs"].clone()), (json!(true), json!(false)));
    }

    #[test]
    fn loot_open_forwards_only_chest_and_request_id() {
        let (_d, app, sent, _r, _rep) = loot_setup();
        let cid = format!("ch-{}", "0123456789abcdef".repeat(2));
        assert_eq!(post_loot_open(&app, &json!({"chestId": cid, "title": "x", "path": "/etc"})).0, 200);
        assert_eq!(post_loot_open(&app, &json!({"chestId": cid, "requestId": "open-mine-0123456789"})).0, 200);
        for b in [json!(""), json!("ch-xyz"), json!(format!("ch-{}", "A".repeat(32))), json!(null), json!(7)] {
            assert_eq!(post_loot_open(&app, &json!({"chestId": b})).0, 400);
        }
        let s = sent.lock().unwrap();
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].3.as_ref().unwrap().as_object().unwrap().keys().collect::<Vec<_>>(), vec!["chestId", "requestId"]);
        assert_eq!(s[1].3.as_ref().unwrap()["requestId"], "open-mine-0123456789");
    }

    #[test]
    fn prestige_routes() {
        let (_d, app) = app();
        let sent = stub(&app, "https://arena.test", Arc::new(Mutex::new((200, json!({"stars": 1})))));
        assert_eq!(get_prestige(&app, &arg(&[])).0, 200);
        for u in ["../me", "bob", "me/../x"] {
            assert_eq!(get_prestige(&app, &arg(&[("u", u)])).0, 400);
        }
        post_prestige_claim(&app, &json!({"requestId": "prestige-abcdef0123456789", "stars": 9}));
        post_prestige_claim(&app, &json!({"requestId": "bad id"}));
        post_prestige_claim(&app, &json!(null));
        let s = sent.lock().unwrap();
        assert_eq!(s[0].1, "https://arena.test/v1/prestige/me");
        assert_eq!(s[1].3, Some(json!({"requestId": "prestige-abcdef0123456789"})));
        assert!(app.paths.here.exists());
        for x in &s[1..] {
            assert!(crate::app::RID_RE.is_match(x.3.as_ref().unwrap()["requestId"].as_str().unwrap()));
        }
    }

    #[test]
    fn maps_wire_and_validation() {
        let (_d, app) = app();
        let sent = stub(&app, "https://arena.example/", Arc::new(Mutex::new((200, json!({"ok": true})))));
        let kart = json!({"tiles": "FSSSSSRSSSRSSSSSSSSRSSSRSS", "scenery": "forest", "theme": {"sky": "#9fd3f0"}});
        assert_eq!(save_map(&app, &json!({"kind": "kart", "name": "  Loop  ", "data": kart, "scope": "public"})).0, 200);
        assert_eq!(sent.lock().unwrap()[0].1, "https://arena.example/v1/maps");
        assert_eq!(sent.lock().unwrap()[0].3, Some(json!({"kind": "kart", "name": "Loop", "data": kart, "scope": "public"})));
        let n = sent.lock().unwrap().len();
        let mut big = kart.clone();
        big["pad"] = json!("x".repeat(13 * 1024));
        let mut texty = kart.clone();
        texty["deco"] = json!([{"text": "hi"}]);
        for b in [json!({"kind": "kart", "name": "X", "data": kart, "scope": "public", "title": "x"}),
                  json!({"kind": "golf", "name": "X", "data": kart, "scope": "public"}),
                  json!({"kind": "kart", "name": "<b>", "data": kart, "scope": "public"}),
                  json!({"kind": "kart", "name": "go to http://x", "data": kart, "scope": "public"}),
                  json!({"kind": "kart", "name": "a\u{7}b", "data": kart, "scope": "public"}),
                  json!({"kind": "kart", "name": "X", "data": big, "scope": "public"}),
                  json!({"kind": "kart", "name": "X", "data": texty, "scope": "public"}),
                  json!({"kind": "kart", "name": "X", "data": kart, "scope": "room", "roomId": "lobby"}),
                  json!({"kind": "kart", "name": "X", "data": kart, "scope": "public", "roomId": format!("hq_{}", UID)}),
                  json!({"id": "m-XYZ", "kind": "kart", "name": "X", "data": kart, "scope": "public"}),
                  json!(null), json!([])] {
            assert_eq!(save_map(&app, &b).0, 400, "{}", b);
        }
        assert_eq!(sent.lock().unwrap().len(), n);
        list_maps(&app, &arg(&[("kind", "kart"), ("sort", "week"), ("limit", "12"), ("cursor", "ab12")]));
        assert_eq!(sent.lock().unwrap().last().unwrap().1, "https://arena.example/v1/maps?kind=kart&sort=week&cursor=ab12&limit=12");
        for d in [vec![("kind", "golf")], vec![("limit", "31")], vec![("cursor", "zz")], vec![("mine", "1"), ("room", "r_AAAAAAAAAAAAAAAAAAAAAA")]] {
            assert_eq!(list_maps(&app, &arg(&d)).0, 400);
        }
        like_map(&app, &json!({"id": "m-0123456789ab", "on": true}));
        assert_eq!(sent.lock().unwrap().last().unwrap().1, "https://arena.example/v1/maps/m-0123456789ab/like");
        assert_eq!(like_map(&app, &json!({"id": "m-0123456789ab", "on": 1})).0, 400);
        for (_, _, _, b) in sent.lock().unwrap().iter() {
            assert!(leaks(b.as_ref().unwrap_or(&json!({}))).is_empty());
        }
    }

    #[test]
    fn map_size_line_is_twelve_kib() {
        let kart = json!({"tiles": "FSS", "pad": ""});
        let base = pyjson::dumps_with(&kart, pyjson::COMPACT).len();
        let fits = json!({"tiles": "FSS", "pad": "x".repeat(12 * 1024 - base)});
        assert!(map_save_body(&json!({"kind": "kart", "name": "Big", "data": fits, "scope": "private"})).is_ok());
        let over = json!({"tiles": "FSS", "pad": "x".repeat(12 * 1024 - base + 1)});
        assert!(map_save_body(&json!({"kind": "kart", "name": "Big", "data": over, "scope": "private"})).is_err());
    }

    #[test]
    fn cups_validate_ids() {
        let (_d, app) = app();
        let sent = stub(&app, "https://arena.example", Arc::new(Mutex::new((200, json!({})))));
        assert_eq!(cup_one(&app, &arg(&[("id", "cup-2026-W41-kart")])).0, 200);
        for b in ["", "cup-2026-W41-td", "cup-2026-W54-kart", "cup-2026-W00-golf", "cup-2026-W41-kart?x=1", "cup-2026-W41-kart\n", "CUP-2026-W41-kart"] {
            assert_eq!(cup_one(&app, &arg(&[("id", b)])).0, 400, "{}", b);
        }
        cup_season(&app, &arg(&[("season", "2026-09")]));
        assert_eq!(cup_season(&app, &arg(&[("season", "2026-13")])).0, 400);
        cup_trophies(&app, &arg(&[]));
        assert_eq!(cup_trophies(&app, &arg(&[("u", &UID.to_uppercase())])).0, 400);
        let s = sent.lock().unwrap();
        assert_eq!(s.iter().map(|x| x.1.as_str()).collect::<Vec<_>>(), vec![
            "https://arena.example/v1/cups/cup-2026-W41-kart", "https://arena.example/v1/cups/season?season=2026-09",
            "https://arena.example/v1/cups/trophies/me"]);
    }

    #[test]
    fn boss_teams() {
        let (_d, app) = app();
        let sent = stub(&app, "https://arena.example", Arc::new(Mutex::new((200, json!({"ok": true})))));
        let spec = json!({"sp": 1, "st": 4, "br": null, "mg": null, "sh": false});
        let mut mega = spec.clone();
        mega["sp"] = json!(3);
        mega["mg"] = json!("charizard-mega-x");
        mega["sh"] = json!(1);
        assert_eq!(boss_fight(&app, &json!({"requestId": "boss-0123456789abcdef", "team": [spec, mega]})).0, 200);
        assert_eq!(sent.lock().unwrap()[0].3.as_ref().unwrap()["team"][1], json!({"sp": 3, "st": 4, "br": null, "mg": "charizard-mega-x", "sh": true}));
        boss_fight(&app, &json!({"team": [spec], "title": "x"}));
        let rid = sent.lock().unwrap()[1].3.as_ref().unwrap()["requestId"].as_str().unwrap().to_string();
        assert!(rid.starts_with("boss-") && rid.len() == 37);
        let mut s7 = vec![];
        for _ in 0..7 {
            s7.push(spec.clone());
        }
        let with = |k: &str, v: Value| {
            let mut x = spec.clone();
            x[k] = v;
            json!([x])
        };
        for team in [json!(null), json!("x"), json!([]), Value::Array(s7), json!([1]), with("sp", json!(true)), with("sp", json!(1.0)),
                     with("st", json!(5)), with("sp", json!(-1)), with("br", json!("x")), with("mg", json!("<b>")), with("sh", json!(2)), with("path", json!("/x"))] {
            assert_eq!(boss_fight(&app, &json!({"team": team})).0, 400);
        }
        for rid in [json!("short"), json!("x".repeat(65)), json!(12345678901234567i64)] {
            assert_eq!(boss_fight(&app, &json!({"team": [spec], "requestId": rid})).0, 400);
        }
        for b in ["../x", "abc", "ME"] {
            assert_eq!(boss_badges(&app, &arg(&[("u", b)])).0, 400);
        }
        assert_eq!(sent.lock().unwrap().len(), 2);
    }

    #[test]
    fn skate_sends_two_whole_numbers_only() {
        let (_d, app) = app();
        assert_eq!(skate_board(&app, &arg(&[])), (400, json!({"error": "not paired"})));
        assert_eq!(skate_combo(&app, &json!({"total": 800, "tricks": 2})), (400, json!({"error": "not paired"})));
        let sent = stub(&app, "https://arena.example", Arc::new(Mutex::new((200, json!({"ok": true})))));
        assert_eq!(skate_board(&app, &arg(&[])).0, 200);
        assert_eq!(skate_combo(&app, &json!({"total": 800.0, "tricks": 2, "where": "/secret", "sessionId": UID})).0, 200);
        assert_eq!(skate_combo(&app, &json!({"total": SKATE_MAX_TOTAL, "tricks": SKATE_MAX_TRICKS})).0, 200);
        let err = json!({"error": "a combo is a total (1-200000) and a trick count (1-10)"});
        for b in [json!({}), json!(null), json!({"total": 0, "tricks": 1}), json!({"total": 200001, "tricks": 1}), json!({"total": 5, "tricks": 11}),
                  json!({"total": 5.5, "tricks": 1}), json!({"total": true, "tricks": 1}), json!({"total": "5", "tricks": 1}), json!({"total": 5, "tricks": 0})] {
            assert_eq!(skate_combo(&app, &b), (400, err.clone()), "{}", b);
        }
        let s = sent.lock().unwrap();
        assert_eq!(s.len(), 3);
        assert_eq!((s[0].0.as_str(), s[0].1.as_str(), s[0].3.clone()), ("GET", "https://arena.example/v1/skate", None));
        assert_eq!((s[1].0.as_str(), s[1].1.as_str()), ("POST", "https://arena.example/v1/skate"));
        assert_eq!(s[1].3, Some(json!({"total": 800, "tricks": 2})));
        assert!(GET.iter().any(|(p, _)| *p == "/api/arena/skate"));
        assert!(POST.iter().any(|(p, _)| *p == "/api/arena/skate/combo"));
    }
}
