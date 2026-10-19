//! The server's shared state: where everything lives, the per-file caches, and
//! the small local JSON state files (config, session meta, meals, game save,
//! the dex salt). One `App` replaces dashboard.py's module globals.

use crate::creature;
use crate::fatigue;
use crate::py::{self, Obj};
use crate::pyjson;
use crate::scan::{self, Agg};
use crate::state;
use regex::Regex;
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::{Arc, LazyLock, Mutex, RwLock};

pub const APP_VERSION: &str = "2.8.2";

pub static UUID_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[0-9a-fA-F-]{36}$").unwrap());
pub static RID_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z0-9_-]{16,64}$").unwrap());
pub static HANDLE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z0-9_-]{1,64}$").unwrap());

/// _UUID_RE.fullmatch(s)
pub fn is_uuid(s: &str) -> bool {
    UUID_RE.is_match(s)
}

/// _UUID_RE.match(s): Python's "$" also matches before one trailing "\n".
pub fn is_uuid_match(s: &str) -> bool {
    is_uuid(s) || s.strip_suffix('\n').map(is_uuid).unwrap_or(false)
}

pub fn is_rid(s: &str) -> bool {
    RID_RE.is_match(s)
}

/// The files and folders the server reads and writes.
#[derive(Clone, Debug)]
pub struct Paths {
    pub here: PathBuf,
    pub projects_dir: PathBuf,
    pub cursor_projects_dir: PathBuf,
    pub cursor_state_db: PathBuf,
    pub config: PathBuf,
    pub meta: PathBuf,
    pub meals: PathBuf,
    pub games_save: PathBuf,
    pub dex_seed: PathBuf,
    pub index_html: PathBuf,
    pub ui_dir: PathBuf,
    pub games_dir: PathBuf,
    pub sounds_dir: PathBuf,
    pub music_cache: PathBuf,
    pub arena_link: PathBuf,
    /// Claude Code's own state file. Only `oauthAccount.accountUuid` is ever
    /// read from it, and only to hash (arena.rs `account_key`).
    pub claude_json: PathBuf,
    pub loot_state: PathBuf,
    pub evo: PathBuf,
    pub vial: PathBuf,
    pub notch_installer: PathBuf,
    pub notch_app: PathBuf,
}

impl Paths {
    /// Everything under `here` (the repo folder, like dashboard.py's HERE) and
    /// the transcript folders under `home`.
    pub fn new(here: &Path, home: &Path) -> Paths {
        Paths {
            here: here.to_path_buf(),
            projects_dir: home.join(".claude/projects"),
            cursor_projects_dir: home.join(".cursor/projects"),
            cursor_state_db: home.join("Library/Application Support/Cursor/User/globalStorage/state.vscdb"),
            config: here.join("config.json"),
            meta: here.join("sessions-meta.json"),
            meals: here.join("meals.json"),
            games_save: here.join("games-save.json"),
            dex_seed: here.join(".dex-seed"),
            index_html: here.join("index.html"),
            ui_dir: here.join("ui"),
            games_dir: here.join("games"),
            sounds_dir: here.join("sounds"),
            music_cache: here.join(".music-cache"),
            arena_link: here.join("arena-link.json"),
            claude_json: match std::env::var_os("CLAUDE_CONFIG_DIR").filter(|v| !v.is_empty()) {
                Some(dir) => PathBuf::from(dir).join(".claude.json"),
                None => home.join(".claude.json"),
            },
            loot_state: here.join("loot-state.json"),
            evo: here.join("evo-choices.json"),
            vial: here.join("vial.json"),
            notch_installer: here.join("notch").join("tools").join("install.py"),
            notch_app: home.join("Applications").join("HQ Notch.app"),
        }
    }
}

/// (mtime_ns, size): the cache key dashboard.py takes from os.stat.
pub type FileKey = (i128, u64);

pub fn file_key(p: &Path) -> Option<FileKey> {
    let m = fs::metadata(p).ok()?;
    use std::os::unix::fs::MetadataExt;
    Some((m.mtime() as i128 * 1_000_000_000 + m.mtime_nsec() as i128, m.size()))
}

pub type AgentsFn = Arc<dyn Fn() -> (Vec<Value>, Option<String>) + Send + Sync>;

#[derive(Default)]
pub struct PayloadMemo {
    pub at: Option<std::time::Instant>,
    pub data: Option<Arc<Value>>,
    pub gen: u64,
}

pub struct App {
    pub paths: Paths,
    pub csrf_token: String,
    pub boot_id: String,
    pub port: AtomicU16,
    pub proc_started: f64,
    pub boot_code_sig: Option<i128>,
    pub(crate) scan_cache: Mutex<HashMap<PathBuf, (FileKey, Arc<Agg>)>>,
    pub(crate) search_cache: Mutex<HashMap<PathBuf, (FileKey, Arc<crate::analytics::SearchEntry>)>>,
    pub(crate) events_cache: Mutex<HashMap<PathBuf, (FileKey, Arc<Vec<Value>>)>>,
    pub(crate) cursor_titles: Mutex<(Option<(PathBuf, FileKey)>, Arc<HashMap<String, String>>)>,
    pub(crate) index_memo: Mutex<Option<(Vec<i128>, String, Arc<String>)>>,
    pub(crate) payload_memo: Mutex<PayloadMemo>,
    pub(crate) blob_memo: Mutex<Option<(Arc<Value>, Arc<String>, Arc<String>)>>,
    pub(crate) config_lock: Mutex<()>,
    pub(crate) meta_lock: Mutex<()>,
    /// Guards meals.json; holds the requestIds with an eat in flight.
    pub(crate) meals_lock: Mutex<HashSet<String>>,
    pub(crate) games_lock: Mutex<()>,
    pub(crate) evo_lock: Mutex<()>,
    pub(crate) vial_lock: Mutex<()>,
    pub(crate) evo_tables: std::sync::OnceLock<crate::evo::EvoTables>,
    pub(crate) dex_salt: Mutex<Option<String>>,
    pub(crate) update_cache: Mutex<(f64, Option<Value>)>,
    pub arena: crate::arena::ArenaState,
    pub music: Arc<crate::music::MusicState>,
    pub ext: crate::ext::ExtState,
    pub notch: crate::notch::NotchState,
    /// Replaces `claude agents --json` (tests).
    pub agents_hook: RwLock<Option<AgentsFn>>,
    /// Replaces the self-restart after an update (tests).
    pub restart_hook: RwLock<Option<Arc<dyn Fn() + Send + Sync>>>,
    /// Replace the running binary's path and the cargo the self-update rebuilds with (tests).
    pub exe_hook: RwLock<Option<PathBuf>>,
    pub cargo_hook: RwLock<Option<PathBuf>>,
}

impl App {
    pub fn new(paths: Paths) -> App {
        App {
            paths,
            csrf_token: py::token_hex(16),
            boot_id: py::token_hex(8),
            port: AtomicU16::new(8765),
            proc_started: py::now_ts(),
            boot_code_sig: crate::actions::code_sig(),
            scan_cache: Mutex::new(HashMap::new()),
            search_cache: Mutex::new(HashMap::new()),
            events_cache: Mutex::new(HashMap::new()),
            cursor_titles: Mutex::new((None, Arc::new(HashMap::new()))),
            index_memo: Mutex::new(None),
            payload_memo: Mutex::new(PayloadMemo::default()),
            blob_memo: Mutex::new(None),
            config_lock: Mutex::new(()),
            meta_lock: Mutex::new(()),
            meals_lock: Mutex::new(HashSet::new()),
            games_lock: Mutex::new(()),
            evo_lock: Mutex::new(()),
            vial_lock: Mutex::new(()),
            evo_tables: std::sync::OnceLock::new(),
            dex_salt: Mutex::new(None),
            update_cache: Mutex::new((0.0, None)),
            arena: crate::arena::ArenaState::default(),
            music: Arc::new(crate::music::MusicState::default()),
            ext: crate::ext::ExtState::default(),
            notch: crate::notch::NotchState::default(),
            agents_hook: RwLock::new(None),
            restart_hook: RwLock::new(None),
            exe_hook: RwLock::new(None),
            cargo_hook: RwLock::new(None),
        }
    }

    pub fn port(&self) -> u16 {
        self.port.load(Ordering::Relaxed)
    }

    // ----------------------------------------------------------------------
    // Transcripts
    // ----------------------------------------------------------------------

    pub fn is_cursor(&self, p: &Path) -> bool {
        scan::is_cursor_transcript(p, &self.paths.cursor_projects_dir)
    }

    /// The cached per-file aggregate, re-read only when (mtime, size) changed.
    pub fn scan_file(&self, path: &Path) -> Option<Arc<Agg>> {
        let key = file_key(path)?;
        if let Some((k, a)) = self.scan_cache.lock().unwrap().get(path) {
            if *k == key {
                return Some(a.clone());
            }
        }
        let agg = Arc::new(if self.is_cursor(path) { scan::scan_cursor(path) } else { scan::scan_claude(path) });
        self.scan_cache.lock().unwrap().insert(path.to_path_buf(), (key, agg.clone()));
        Some(agg)
    }

    /// Claude Code transcripts plus Cursor parent agent sessions, in glob order.
    pub fn iter_transcript_paths(&self) -> Vec<PathBuf> {
        let mut out = Vec::new();
        for d in glob_dirs(&self.paths.projects_dir) {
            out.extend(glob_files(&d, ".jsonl"));
        }
        for d in glob_dirs(&self.paths.cursor_projects_dir) {
            for s in glob_dirs(&d.join("agent-transcripts")) {
                out.extend(glob_files(&s, ".jsonl"));
            }
        }
        out
    }

    /// Claude transcripts only (the Arena publish reads these).
    pub fn claude_transcript_paths(&self) -> Vec<PathBuf> {
        glob_dirs(&self.paths.projects_dir).iter().flat_map(|d| glob_files(d, ".jsonl")).collect()
    }

    /// Locate the JSONL for a UUID-shaped session id.
    pub fn find_transcript(&self, sid: &str) -> Option<PathBuf> {
        if !is_uuid(sid) {
            return None;
        }
        let name = format!("{}.jsonl", sid);
        for d in glob_dirs(&self.paths.projects_dir) {
            let p = d.join(&name);
            if fs::symlink_metadata(&p).is_ok() {
                return Some(p);
            }
        }
        for d in glob_dirs(&self.paths.cursor_projects_dir) {
            let p = d.join("agent-transcripts").join(sid).join(&name);
            if fs::symlink_metadata(&p).is_ok() {
                return Some(p);
            }
        }
        None
    }

    // ----------------------------------------------------------------------
    // Dex salt
    // ----------------------------------------------------------------------

    /// A stable per-install salt for the shiny roll (.dex-seed).
    pub fn dex_salt(&self) -> String {
        let mut g = self.dex_salt.lock().unwrap();
        if let Some(s) = g.as_ref() {
            return s.clone();
        }
        static SALT_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[0-9a-f]{8,64}$").unwrap());
        let s = fs::read(&self.paths.dex_seed).ok().map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default();
        let mut s = py::strip(&s).to_string();
        if !SALT_RE.is_match(&s) {
            s = py::token_hex(8);
            let _ = state::atomic_write_text(&self.paths.dex_seed, &s, 0o600);
        }
        *g = Some(s.clone());
        s
    }

    pub fn creature_for(&self, sid: &str, prompt_count: i64) -> Obj {
        creature::creature_for(sid, prompt_count, &self.dex_salt())
    }

    // ----------------------------------------------------------------------
    // Config
    // ----------------------------------------------------------------------

    pub fn load_config(&self) -> Obj {
        validate_config(&state::load_json_guarded(&self.paths.config).unwrap_or_else(|| json!({})), None)
    }

    /// Merge `patch` into the current config, validate, persist, return it.
    pub fn save_config(&self, patch: &Value) -> io::Result<Obj> {
        let _g = self.config_lock.lock().unwrap();
        let cur = self.load_config();
        let cfg = validate_config(patch, Some(&cur));
        state::atomic_write_json(&self.paths.config, &Value::Object(cfg.clone()), 0o600)?;
        Ok(cfg)
    }

    // ----------------------------------------------------------------------
    // Session meta
    // ----------------------------------------------------------------------

    pub fn load_meta(&self) -> Obj {
        let mut out = Map::new();
        if let Some(Value::Object(raw)) = state::load_json_guarded(&self.paths.meta) {
            for (sid, entry) in raw {
                if is_uuid_match(&sid) {
                    out.insert(sid, clean_meta_entry(&entry));
                }
            }
        }
        out
    }

    pub fn save_meta(&self, sid: &str, patch: &Obj) -> io::Result<Value> {
        let _g = self.meta_lock.lock().unwrap();
        let mut data = self.load_meta();
        let mut merged = match data.get(sid) {
            Some(Value::Object(o)) => o.clone(),
            _ => match json!({"pinned": false, "tags": [], "note": "", "name": ""}) {
                Value::Object(o) => o,
                _ => unreachable!(),
            },
        };
        for k in ["pinned", "tags", "note", "name"] {
            if let Some(v) = patch.get(k) {
                merged.insert(k.to_string(), v.clone());
            }
        }
        let entry = clean_meta_entry(&Value::Object(merged));
        data.insert(sid.to_string(), entry.clone());
        state::atomic_write_json(&self.paths.meta, &Value::Object(data), 0o600)?;
        Ok(entry)
    }

    // ----------------------------------------------------------------------
    // Meal ledger
    // ----------------------------------------------------------------------

    fn read_meal_entries(&self, now: f64) -> Vec<Meal> {
        let Some(Value::Object(raw)) = state::load_json_plain(&self.paths.meals) else { return Vec::new() };
        let Some(Value::Array(items)) = raw.get("meals") else { return Vec::new() };
        let mut out = Vec::new();
        for m in items {
            let Value::Object(m) = m else { continue };
            let (Some(sid), Some(kind), Some(rid)) = (py::get_str(m, "sessionId"), py::get_str(m, "kind"), py::get_str(m, "requestId")) else {
                continue;
            };
            if !is_uuid(sid) || !fatigue::is_food(kind) || !is_rid(rid) {
                continue;
            }
            let Some(at) = m.get("at").and_then(py::number) else { continue };
            if !at.is_finite() || !(now - MEALS_KEEP_SECS <= at && at <= now + 60.0) {
                continue;
            }
            out.push(Meal { request_id: rid.into(), session_id: sid.into(), kind: kind.into(), at });
        }
        out
    }

    /// {sessionId: [(at, kind), ...]} sorted by time, for the fatigue walk.
    pub fn load_meals(&self, now: Option<f64>) -> HashMap<String, Vec<(f64, String)>> {
        let mut out: HashMap<String, Vec<(f64, String)>> = HashMap::new();
        for m in self.read_meal_entries(now.unwrap_or_else(py::now_ts)) {
            out.entry(m.session_id).or_default().push((m.at, m.kind));
        }
        for v in out.values_mut() {
            v.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
        }
        out
    }

    pub fn find_meal(&self, rid: &str) -> Option<Meal> {
        self.read_meal_entries(py::now_ts()).into_iter().find(|m| m.request_id == rid)
    }

    /// Record one meal (idempotent by requestId across all sessions).
    pub fn record_meal(&self, rid: &str, sid: &str, kind: &str, at: f64, now: Option<f64>) -> io::Result<Meal> {
        let _g = self.meals_lock.lock().unwrap();
        self.record_meal_locked(rid, sid, kind, at, now)
    }

    pub(crate) fn record_meal_locked(&self, rid: &str, sid: &str, kind: &str, at: f64, now: Option<f64>) -> io::Result<Meal> {
        let now = now.unwrap_or_else(py::now_ts);
        let mut entries = self.read_meal_entries(now);
        if let Some(m) = entries.iter().find(|m| m.request_id == rid) {
            return Ok(m.clone());
        }
        let entry = Meal { request_id: rid.into(), session_id: sid.into(), kind: kind.into(), at };
        entries.push(entry.clone());
        let fz_lo = now - fatigue::WINDOW_SECS;
        let mut sorted = entries.clone();
        sorted.sort_by(|a, b| b.at.total_cmp(&a.at));
        let mut kept: Vec<Meal> = Vec::new();
        let mut per_sid: HashMap<String, usize> = HashMap::new();
        for m in sorted {
            if !(now - MEALS_KEEP_SECS <= m.at && m.at <= now + 60.0) {
                continue;
            }
            let n = per_sid.get(&m.session_id).copied().unwrap_or(0);
            if n >= MEALS_PER_SID && m.at < fz_lo {
                continue;
            }
            per_sid.insert(m.session_id.clone(), n + 1);
            kept.push(m);
            if kept.len() >= MEALS_MAX {
                break;
            }
        }
        kept.reverse();
        let body = json!({"version": 1, "meals": kept.iter().map(Meal::to_json).collect::<Vec<_>>()});
        let dir = self.paths.meals.parent().unwrap_or(Path::new(".")).to_path_buf();
        state::atomic_write_prefixed(&self.paths.meals, &dir, ".meals-", &pyjson::dumps(&body))?;
        Ok(entry)
    }

    // ----------------------------------------------------------------------
    // Valley game save
    // ----------------------------------------------------------------------

    pub fn load_games_save(&self) -> Value {
        match state::load_json_plain(&self.paths.games_save) {
            Some(v @ Value::Object(_)) => v,
            _ => json!({}),
        }
    }

    /// Replace the whole save (a JSON object of bounded size), atomically.
    pub fn save_games_save(&self, body: &Obj) -> Result<Value, SaveError> {
        let Some(st @ Value::Object(_)) = body.get("state") else {
            return Err(SaveError::Bad("state must be an object".into()));
        };
        let blob = pyjson::dumps_with(st, pyjson::COMPACT);
        if blob.chars().count() > GAMES_SAVE_MAX {
            return Err(SaveError::Bad("save too large".into()));
        }
        let _g = self.games_lock.lock().unwrap();
        state::atomic_write_prefixed(&self.paths.games_save, &self.paths.here, ".games-save.", &blob).map_err(SaveError::Io)?;
        Ok(json!({"ok": true, "bytes": blob.chars().count()}))
    }
}

pub enum SaveError {
    Bad(String),
    Io(io::Error),
}

pub const GAMES_SAVE_MAX: usize = 256 * 1024;
pub const MEALS_KEEP_SECS: f64 = 172800.0;
pub const MEALS_MAX: usize = 500;
pub const MEALS_PER_SID: usize = 20;

#[derive(Clone, Debug, PartialEq)]
pub struct Meal {
    pub request_id: String,
    pub session_id: String,
    pub kind: String,
    pub at: f64,
}

impl Meal {
    pub fn to_json(&self) -> Value {
        json!({"requestId": self.request_id, "sessionId": self.session_id, "kind": self.kind, "at": self.at})
    }
}

/// Non-hidden subdirectories of `dir`, in directory order (glob "*/").
pub fn glob_dirs(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = fs::read_dir(dir) else { return Vec::new() };
    rd.filter_map(|e| e.ok())
        .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect()
}

/// Non-hidden entries of `dir` whose name ends with `suffix` (glob "*<suffix>").
pub fn glob_files(dir: &Path, suffix: &str) -> Vec<PathBuf> {
    let Ok(rd) = fs::read_dir(dir) else { return Vec::new() };
    rd.filter_map(|e| e.ok())
        .filter(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            !n.starts_with('.') && n.ends_with(suffix)
        })
        .map(|e| e.path())
        .collect()
}

// --------------------------------------------------------------------------
// Config validation
// --------------------------------------------------------------------------

pub const KNOWN_THEMES: [&str; 4] = ["aurora", "midnight", "forest", "mono"];
pub const KNOWN_CREATURE_PACKS: [&str; 7] = ["monsters", "pokemon", "pokemon3d", "aniimo", "village", "animals", "faces"];
pub const TRAINER_MAX: [i64; 9] = [6, 8, 8, 8, 8, 7, 5, 8, 4];
pub const CHARACTER_MAX: [i64; 10] = [5, 7, 9, 9, 2, 5, 3, 1, 4, 1];

pub fn default_config() -> Obj {
    match json!({
        "theme": "aurora",
        "creaturePack": "pokemon3d",
        "refreshMs": 5000,
        "stuckMinutes": 15,
        "dailyBudgetUSD": 0,
        "trainerName": "",
        "trainerAvatar": null,
        "character": null,
        "arenaUrl": "",
        "arenaEnabled": false,
        "arenaShareCost": false,
        "arenaAccountBoard": false,
        "creatureFatigue": true,
        "musicShare": true,
        "musicCookies": "",
        "workSignals": false,
        "workSignalsPRs": false,
    }) {
        Value::Object(o) => o,
        _ => unreachable!(),
    }
}

fn spec_list(v: &Value, maxes: &[i64]) -> Value {
    let a = v.as_array().unwrap();
    Value::Array(
        maxes
            .iter()
            .enumerate()
            .map(|(i, m)| {
                let x = a.get(i).and_then(py::py_int).unwrap_or(0);
                Value::from(x.rem_euclid(m + 1))
            })
            .collect(),
    )
}

/// A clean config: known keys only, validated types/ranges; anything invalid
/// falls back to `base` (defaults, or the current config).
pub fn validate_config(raw: &Value, base: Option<&Obj>) -> Obj {
    let mut cfg = base.cloned().unwrap_or_else(default_config);
    let Value::Object(raw) = raw else { return cfg };
    if let Some(Value::String(t)) = raw.get("theme") {
        if KNOWN_THEMES.contains(&t.as_str()) {
            cfg.insert("theme".into(), Value::from(t.clone()));
        }
    }
    if let Some(Value::String(t)) = raw.get("creaturePack") {
        if KNOWN_CREATURE_PACKS.contains(&t.as_str()) {
            cfg.insert("creaturePack".into(), Value::from(t.clone()));
        }
    }
    if let Some(rm) = raw.get("refreshMs").and_then(py::py_int) {
        if (1000..=60000).contains(&rm) {
            cfg.insert("refreshMs".into(), Value::from(rm));
        }
    }
    if let Some(sm) = raw.get("stuckMinutes").and_then(py::py_int) {
        if (1..=240).contains(&sm) {
            cfg.insert("stuckMinutes".into(), Value::from(sm));
        }
    }
    if let Some(db) = raw.get("dailyBudgetUSD").and_then(py::py_float) {
        if db >= 0.0 && db.is_finite() {
            cfg.insert("dailyBudgetUSD".into(), Value::from(db));
        }
    }
    if let Some(Value::String(tn)) = raw.get("trainerName") {
        cfg.insert("trainerName".into(), Value::from(py::take_chars(&py::collapse_ws(&py::printable_only(tn)), 32)));
    }
    if let Some(ta) = raw.get("trainerAvatar") {
        match ta {
            Value::Null => {
                cfg.insert("trainerAvatar".into(), Value::Null);
            }
            Value::Array(_) => {
                cfg.insert("trainerAvatar".into(), spec_list(ta, &TRAINER_MAX));
            }
            _ => {}
        }
    }
    if let Some(ch) = raw.get("character") {
        match ch {
            Value::Null => {
                cfg.insert("character".into(), Value::Null);
            }
            Value::Array(_) => {
                cfg.insert("character".into(), spec_list(ch, &CHARACTER_MAX));
            }
            _ => {}
        }
    }
    if let Some(Value::String(au)) = raw.get("arenaUrl") {
        let au = py::strip(au);
        let v = if au.starts_with("http://") || au.starts_with("https://") { py::take_chars(au, 256) } else { String::new() };
        cfg.insert("arenaUrl".into(), Value::from(v));
    }
    if let Some(Value::String(mc)) = raw.get("musicCookies") {
        if mc.is_empty() || crate::music::COOKIE_BROWSERS.contains(&mc.as_str()) {
            cfg.insert("musicCookies".into(), Value::from(mc.clone()));
        }
    }
    for key in ["arenaEnabled", "arenaShareCost", "arenaAccountBoard", "creatureFatigue", "musicShare"] {
        if let Some(v) = raw.get(key) {
            cfg.insert(key.into(), Value::Bool(py::truthy(v)));
        }
    }
    for key in ["workSignals", "workSignalsPRs"] {
        if let Some(Value::Bool(b)) = raw.get(key) {
            cfg.insert(key.into(), Value::Bool(*b));
        }
    }
    cfg
}

/// Normalise one session-meta entry: pinned bool, tags list, note capped.
pub fn clean_meta_entry(entry: &Value) -> Value {
    let empty = Map::new();
    let e = entry.as_object().unwrap_or(&empty);
    let pinned = py::opt_truthy(e.get("pinned"));
    let mut tags: Vec<Value> = Vec::new();
    if let Some(Value::Array(raw)) = e.get("tags") {
        for t in raw {
            let Value::String(t) = t else { continue };
            let t = py::take_chars(py::strip(t), 24);
            if !t.is_empty() {
                tags.push(Value::from(t));
            }
            if tags.len() >= 12 {
                break;
            }
        }
    }
    let note = match e.get("note") {
        Some(Value::String(s)) => py::take_chars(s, 2000),
        _ => String::new(),
    };
    let name = match e.get("name") {
        Some(Value::String(s)) => py::take_chars(py::strip(s), 80),
        _ => String::new(),
    };
    json!({"pinned": pinned, "tags": tags, "note": note, "name": name})
}

#[cfg(test)]
pub(crate) mod testutil {
    use super::*;

    /// An App rooted in a temp dir, with empty transcript folders.
    pub fn app() -> (tempfile::TempDir, Arc<App>) {
        let d = tempfile::tempdir().unwrap();
        let mut p = Paths::new(d.path(), &d.path().join("home"));
        p.projects_dir = d.path().join("projects");
        p.cursor_projects_dir = d.path().join("cursor");
        p.cursor_state_db = d.path().join("state.vscdb");
        // Never the developer's real file, whatever CLAUDE_CONFIG_DIR says.
        p.claude_json = d.path().join("home").join(".claude.json");
        fs::create_dir_all(&p.projects_dir).unwrap();
        fs::create_dir_all(&p.cursor_projects_dir).unwrap();
        let app = Arc::new(App::new(p));
        *app.agents_hook.write().unwrap() = Some(Arc::new(|| (Vec::new(), None)));
        (d, app)
    }

    pub fn write_jsonl(path: &Path, recs: &[Value]) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut s = String::new();
        for r in recs {
            s.push_str(&serde_json::to_string(r).unwrap());
            s.push('\n');
        }
        fs::write(path, s).unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::testutil::*;
    use super::*;

    fn v(raw: Value) -> Obj {
        validate_config(&raw, None)
    }

    #[test]
    fn defaults_for_garbage() {
        assert_eq!(v(json!("not a dict")), default_config());
        assert_eq!(v(json!({})), default_config());
        assert!(pyjson::dumps(&Value::Object(default_config())).len() > 10);
    }

    #[test]
    fn allowlists_and_ranges() {
        let c = v(json!({"theme": "forest", "creaturePack": "animals"}));
        assert_eq!((c["theme"].as_str(), c["creaturePack"].as_str()), (Some("forest"), Some("animals")));
        let c = v(json!({"theme": "neon", "creaturePack": "dragons"}));
        assert_eq!((c["theme"].as_str(), c["creaturePack"].as_str()), (Some("aurora"), Some("pokemon3d")));
        let c = v(json!({"refreshMs": 999, "stuckMinutes": 999, "dailyBudgetUSD": -5}));
        assert_eq!((c["refreshMs"].clone(), c["stuckMinutes"].clone(), c["dailyBudgetUSD"].clone()), (json!(5000), json!(15), json!(0)));
        let c = v(json!({"refreshMs": 3000, "stuckMinutes": 30, "dailyBudgetUSD": 12.5}));
        assert_eq!((c["refreshMs"].clone(), c["stuckMinutes"].clone(), c["dailyBudgetUSD"].clone()), (json!(3000), json!(30), json!(12.5)));
        assert_eq!(pyjson::dumps(&v(json!({"dailyBudgetUSD": 25}))["dailyBudgetUSD"]), "25.0");
    }

    #[test]
    fn trainer_name_and_urls() {
        assert_eq!(v(json!({"trainerName": "  Swastik \u{7}  Tripathi  "}))["trainerName"], "Swastik Tripathi");
        assert_eq!(v(json!({"trainerName": "x".repeat(50)}))["trainerName"].as_str().unwrap().len(), 32);
        assert_eq!(v(json!({"trainerName": 123}))["trainerName"], "");
        assert_eq!(v(json!({"arenaUrl": "https://example.fly.dev"}))["arenaUrl"], "https://example.fly.dev");
        assert_eq!(v(json!({"arenaUrl": "file:///etc/passwd"}))["arenaUrl"], "");
        assert_eq!(v(json!({"arenaUrl": "javascript:alert(1)"}))["arenaUrl"], "");
    }

    #[test]
    fn flags() {
        let c = v(json!({"arenaEnabled": 1, "arenaShareCost": ""}));
        assert_eq!((c["arenaEnabled"].clone(), c["arenaShareCost"].clone()), (json!(true), json!(false)));
        assert_eq!(v(json!({"creatureFatigue": 0}))["creatureFatigue"], false);
        let mut off = default_config();
        off.insert("creatureFatigue".into(), json!(false));
        assert_eq!(validate_config(&json!({"theme": "forest"}), Some(&off))["creatureFatigue"], false);
        assert_eq!(validate_config(&json!({"creatureFatigue": "yes"}), Some(&off))["creatureFatigue"], true);
        assert!(!v(json!({"fatigue": true})).contains_key("fatigue"));
        let on = v(json!({"workSignals": true, "workSignalsPRs": true}));
        let kept = validate_config(&json!({"workSignals": "yes", "workSignalsPRs": 1}), Some(&on));
        assert_eq!((kept["workSignals"].clone(), kept["workSignalsPRs"].clone()), (json!(true), json!(true)));
        assert_eq!(v(json!({"musicShare": false}))["musicShare"], false);
        assert_eq!(v(json!({"musicShare": "yes"}))["musicShare"], true);
        assert_eq!(v(json!({"musicCookies": "safari"}))["musicCookies"], "safari");
        assert_eq!(v(json!({"musicCookies": "evil"}))["musicCookies"], "");
    }

    #[test]
    fn character_and_trainer_specs() {
        assert!(default_config()["character"].is_null());
        assert_eq!(v(json!({"character": [5, 7, 9, 9, 2, 5, 3, 1, 4, 1]}))["character"], json!([5, 7, 9, 9, 2, 5, 3, 1, 4, 1]));
        assert_eq!(v(json!({"character": [6, -1, "x"]}))["character"], json!([0, 7, 0, 0, 0, 0, 0, 0, 0, 0]));
        assert!(v(json!({"character": null}))["character"].is_null());
        assert!(v(json!({"character": "evil"}))["character"].is_null());
        assert_eq!(v(json!({"trainerAvatar": [7, -1]}))["trainerAvatar"], json!([0, 8, 0, 0, 0, 0, 0, 0, 0]));
    }

    #[test]
    fn config_and_meta_round_trip() {
        let (_d, app) = app();
        let saved = app.save_config(&json!({"dailyBudgetUSD": 25})).unwrap();
        assert_eq!(app.load_config(), saved);
        fs::write(&app.paths.config, "{oops").unwrap();
        assert_eq!(app.load_config(), default_config());
        let sid = "11111111-1111-4111-8111-111111111111";
        let patch = match json!({"pinned": true, "note": "hi"}) {
            Value::Object(o) => o,
            _ => unreachable!(),
        };
        let entry = app.save_meta(sid, &patch).unwrap();
        assert_eq!(app.load_meta()[sid], entry);
        fs::write(&app.paths.meta, "[[[").unwrap();
        assert_eq!(app.load_meta(), Map::new());
    }

    #[test]
    fn meta_entries_are_cleaned() {
        let e = clean_meta_entry(&json!({"pinned": 1, "tags": [" a ", 5, "", "b".repeat(30)], "note": 7, "name": "  x  "}));
        assert_eq!(e, json!({"pinned": true, "tags": ["a", "b".repeat(24)], "note": "", "name": "x"}));
    }

    const SID_A: &str = "11111111-1111-4111-8111-111111111111";
    const SID_B: &str = "22222222-2222-4222-8222-222222222222";
    const NOW: f64 = 1790000000.0;

    fn rid(n: usize) -> String {
        format!("req-{:016}", n)
    }

    #[test]
    fn meals_are_idempotent_private_and_bounded() {
        let (d, app) = app();
        let first = app.record_meal(&rid(1), SID_A, "berry", NOW - 10.0, Some(NOW)).unwrap();
        let again = app.record_meal(&rid(1), SID_B, "bento", NOW - 5.0, Some(NOW)).unwrap();
        assert_eq!(first, again);
        assert_eq!(app.load_meals(Some(NOW)).get(SID_A).unwrap(), &vec![(NOW - 10.0, "berry".to_string())]);
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(fs::metadata(&app.paths.meals).unwrap().permissions().mode() & 0o777, 0o600);
        let names: Vec<String> = fs::read_dir(d.path()).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        assert!(!names.iter().any(|n| n.starts_with(".meals-")));
        // corrupt / invalid entries read as empty
        fs::write(&app.paths.meals, "{not json").unwrap();
        assert!(app.load_meals(Some(NOW)).is_empty());
        let good = json!({"requestId": rid(1), "sessionId": SID_A, "kind": "berry", "at": NOW - 60.0});
        let mut bad = vec![good.clone()];
        for (k, val) in [("sessionId", json!("../etc/passwd")), ("sessionId", json!(format!("{}\n", SID_A))), ("kind", json!("pizza")),
                         ("requestId", json!("short")), ("at", json!(true)), ("at", json!("1790000000")),
                         ("at", json!(NOW - MEALS_KEEP_SECS - 1.0)), ("at", json!(NOW + 61.0))] {
            let mut x = good.clone();
            x[k] = val;
            if k != "requestId" {
                x["requestId"] = json!(rid(bad.len() + 10));
            }
            bad.push(x);
        }
        fs::write(&app.paths.meals, pyjson::dumps(&json!({"version": 1, "meals": bad}))).unwrap();
        assert_eq!(app.load_meals(Some(NOW)).get(SID_A).unwrap(), &vec![(NOW - 60.0, "berry".to_string())]);
        assert_eq!(app.load_meals(Some(NOW)).len(), 1);
    }

    #[test]
    fn meal_caps_keep_the_fatigue_window() {
        let (_d, app) = app();
        let old = NOW - fatigue::WINDOW_SECS - 1000.0;
        let entries: Vec<Value> = (0..30).map(|i| json!({"requestId": rid(i), "sessionId": SID_A, "kind": "berry", "at": old + i as f64})).collect();
        fs::write(&app.paths.meals, pyjson::dumps(&json!({"version": 1, "meals": entries}))).unwrap();
        app.record_meal(&rid(99), SID_A, "bento", NOW, Some(NOW)).unwrap();
        let meals = app.load_meals(Some(NOW))[SID_A].clone();
        assert_eq!(meals.len(), MEALS_PER_SID);
        assert_eq!(meals.last().unwrap(), &(NOW, "bento".to_string()));
        assert_eq!(meals[0], (old + 11.0, "berry".to_string()));

        let lo = NOW - fatigue::WINDOW_SECS;
        let mut entries: Vec<Value> = (0..5).map(|i| json!({"requestId": rid(i), "sessionId": SID_A, "kind": "berry", "at": lo - 5000.0 + i as f64})).collect();
        entries.push(json!({"requestId": rid(10), "sessionId": SID_A, "kind": "tonic", "at": lo + 60.0}));
        entries.extend((0..MEALS_PER_SID + 4).map(|i| json!({"requestId": rid(20 + i), "sessionId": SID_A, "kind": "berry", "at": lo + 120.0 + i as f64})));
        fs::write(&app.paths.meals, pyjson::dumps(&json!({"version": 1, "meals": entries}))).unwrap();
        app.record_meal(&rid(99), SID_A, "berry", NOW, Some(NOW)).unwrap();
        let meals = app.load_meals(Some(NOW))[SID_A].clone();
        assert_eq!(meals.len(), MEALS_PER_SID + 6);
        assert_eq!(meals[0], (lo + 60.0, "tonic".to_string()));

        let many: Vec<Value> = (0..600).map(|i| json!({"requestId": rid(1000 + i), "sessionId": format!("{:08}-0000-4000-8000-000000000000", i), "kind": "berry", "at": NOW - 600.0 + i as f64})).collect();
        fs::write(&app.paths.meals, pyjson::dumps(&json!({"version": 1, "meals": many}))).unwrap();
        app.record_meal(&rid(5000), SID_B, "tonic", NOW, Some(NOW)).unwrap();
        let raw = state::load_json_plain(&app.paths.meals).unwrap();
        let data = raw["meals"].as_array().unwrap();
        assert_eq!(data.len(), MEALS_MAX);
        assert_eq!(data.last().unwrap()["requestId"], rid(5000));
    }

    #[test]
    fn dex_salt_is_written_once() {
        let (_d, app) = app();
        let s = app.dex_salt();
        assert_eq!(fs::read_to_string(&app.paths.dex_seed).unwrap(), s);
        assert_eq!(s.len(), 16);
        assert_eq!(app.dex_salt(), s);
    }

    #[test]
    fn games_save_round_trip() {
        let (_d, app) = app();
        assert_eq!(app.load_games_save(), json!({}));
        let body = match json!({"state": {"v": 1, "inv": {"koi": 2}}}) {
            Value::Object(o) => o,
            _ => unreachable!(),
        };
        assert!(app.save_games_save(&body).is_ok());
        assert_eq!(app.load_games_save(), json!({"v": 1, "inv": {"koi": 2}}));
        let bad = match json!({"state": [1]}) {
            Value::Object(o) => o,
            _ => unreachable!(),
        };
        assert!(matches!(app.save_games_save(&bad), Err(SaveError::Bad(_))));
    }

    #[test]
    fn find_transcript_validates_ids() {
        let (_d, app) = app();
        let sid = "22222222-2222-4222-8222-222222222222";
        let p = app.paths.projects_dir.join("proj").join(format!("{}.jsonl", sid));
        write_jsonl(&p, &[json!({"type": "user"})]);
        assert_eq!(app.find_transcript(sid), Some(p));
        for bad in ["*", "2222*", "../proj/x", &format!("{}\n", sid), ""] {
            assert_eq!(app.find_transcript(bad), None);
        }
    }
}
