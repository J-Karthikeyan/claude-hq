//! The opt-in Arena client -- THE PRIVACY BOUNDARY (port of arena.py). It sends
//! daily counts only, never prompt text, replies, file paths, project/folder
//! names, session ids or titles. Tool names outside the built-in allowlist are
//! bucketed as "Other"; pantry, cali, HQ and music bodies are rebuilt from key
//! allowlists. The device token lives in arena-link.json, never in config.json.

use crate::app::App;
use crate::music;
use crate::py::{self, Obj};
use crate::state;
use base64::Engine;
use chrono::Duration;
use indexmap::IndexMap;
use regex::Regex;
use serde_json::{json, Map, Value};
use std::sync::{Arc, LazyLock, Mutex, RwLock};

pub const KNOWN_TOOLS: [&str; 23] = [
    "Bash", "BashOutput", "KillShell", "Read", "Write", "Edit", "NotebookEdit",
    "Glob", "Grep", "Task", "Agent", "WebFetch", "WebSearch", "TodoWrite",
    "ExitPlanMode", "EnterPlanMode", "SlashCommand", "Skill", "AskUserQuestion",
    "Artifact", "Workflow", "Monitor", "ToolSearch",
];
pub const SCHEMA_VERSION: i64 = 1;
pub const PUBLISH_WINDOW_DAYS: i64 = 30;
pub const PUBLISH_INTERVAL_SECS: u64 = 300;
pub const NUDGE_POLL_SECS: u64 = 30;
const HTTP_TIMEOUT: u64 = 20;
pub const GIFT_DRAIN_BACKOFF_SECS: f64 = 1800.0;

pub const FOOD_KINDS: [&str; 21] = [
    "berry", "bread", "riceball", "coffee", "bento", "noodles", "hotpot", "tonic", "elixir", "strawberry", "dango",
    "omelette", "watermelon", "shavedice", "curry", "apple", "sweetpotato", "pumpkinstew", "chestnuts", "cocoa", "oden",
];
pub const FOOD_LABELS: [(&str, (&str, &str)); 21] = [
    ("berry", ("Berry", "Berries")),
    ("bread", ("Bread Loaf", "Bread Loaves")),
    ("riceball", ("Rice Ball", "Rice Balls")),
    ("coffee", ("Coffee", "Coffees")),
    ("bento", ("Bento", "Bentos")),
    ("noodles", ("Noodle Bowl", "Noodle Bowls")),
    ("hotpot", ("Hot Pot", "Hot Pots")),
    ("tonic", ("Revive Tonic", "Revive Tonics")),
    ("elixir", ("Honey Elixir", "Honey Elixirs")),
    ("strawberry", ("Strawberry", "Strawberries")),
    ("dango", ("Hanami Dango", "Hanami Dango")),
    ("omelette", ("Garden Omelette", "Garden Omelettes")),
    ("watermelon", ("Watermelon Slice", "Watermelon Slices")),
    ("shavedice", ("Shaved Ice", "Shaved Ices")),
    ("curry", ("Summer Curry", "Summer Curries")),
    ("apple", ("Apple", "Apples")),
    ("sweetpotato", ("Baked Sweet Potato", "Baked Sweet Potatoes")),
    ("pumpkinstew", ("Pumpkin Stew", "Pumpkin Stews")),
    ("chestnuts", ("Bag of Chestnuts", "Bags of Chestnuts")),
    ("cocoa", ("Hot Cocoa", "Hot Cocoas")),
    ("oden", ("Oden Skewer", "Oden Skewers")),
];
pub const PANTRY_ACTIONS: [&str; 4] = ["claim", "buy", "eat", "give"];
pub const PANTRY_KEYS: [&str; 6] = ["requestId", "kind", "qty", "coins", "toHandle", "note"];

pub static ROOM_ID_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^r_[A-Za-z0-9_-]{22}$").unwrap());
pub static USER_ID_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$").unwrap());

pub const CALI_WINDOWS: [&str; 5] = ["season", "30d", "7d", "all", "lastseason"];
const CALI_ORDER_KEYS: [&str; 4] = ["requestId", "date", "diners", "note"];
const CALI_DINER_KEYS: [&str; 4] = ["handle", "name", "tacos", "items"];
pub const CALI_TACO_KEYS: [&str; 4] = ["mildHard", "mildSoft", "wildHard", "wildSoft"];
pub const CALI_ITEM_KEYS: [&str; 11] =
    ["burrito", "ricebowl", "saladbowl", "quesadilla", "nachos", "tostada", "chips", "guac", "churros", "soda", "icedtea"];
const HQ_LOOK_KEYS: [&str; 3] = ["paint", "accent", "sign"];
const HQ_CREW_KEYS: [&str; 3] = ["working", "needs", "idle"];
pub const PORTRAIT_MAX_BYTES: usize = 64 * 1024;
const GAMES: [&str; 8] = ["kart", "plat", "fps", "golf", "type", "td", "bowl", "rally"];
const COS_SLOTS: [&str; 6] = ["kart", "runner", "blaster", "ball", "frame", "decor"];
const MARKET_CATS: [&str; 5] = ["fish", "crop", "ore", "gem", "misc"];
pub const QUICKPLAY_GAMES: [&str; 6] = ["kart", "plat", "fps", "golf", "type", "rally"];

pub fn valid_room_id(v: &Value) -> bool {
    v.as_str().map(|s| ROOM_ID_RE.is_match(s)).unwrap_or(false)
}

pub fn valid_user_id(s: &str) -> bool {
    USER_ID_RE.is_match(s)
}

/// (method, url, token, json body) -> (status, body). 0 = no answer.
pub type Transport = Arc<dyn Fn(&str, &str, Option<&str>, Option<&Value>) -> (u16, Value) + Send + Sync>;

pub struct ArenaState {
    pub last: Mutex<Value>,
    pub gift_off_until: Mutex<f64>,
    pub link_lock: Mutex<()>,
    /// Replaces the network (tests).
    pub transport: RwLock<Option<Transport>>,
}

impl Default for ArenaState {
    fn default() -> Self {
        ArenaState {
            last: Mutex::new(json!({"at": null, "ok": null, "error": null, "accepted": 0})),
            gift_off_until: Mutex::new(0.0),
            link_lock: Mutex::new(()),
            transport: RwLock::new(None),
        }
    }
}

/// FastAPI says {"detail": ...}; the page reads "error".
pub fn with_error(body: Value) -> Value {
    let Value::Object(mut o) = body else { return body };
    if o.contains_key("error") || !o.contains_key("detail") {
        return Value::Object(o);
    }
    let detail = match &o["detail"] {
        Value::Array(items) => items
            .iter()
            .map(|i| match i {
                Value::Object(io) => match io.get("msg") {
                    Some(Value::String(s)) => s.clone(),
                    Some(v) => v.to_string(),
                    None => py_str(i),
                },
                other => py_str(other),
            })
            .collect::<Vec<_>>()
            .join("; "),
        Value::String(s) => s.clone(),
        other => py_str(other),
    };
    o.insert("error".into(), Value::from(detail));
    Value::Object(o)
}

/// str(v) for a decoded JSON value (close to Python's str()).
pub fn py_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => "None".into(),
        Value::Bool(b) => if *b { "True" } else { "False" }.into(),
        Value::Number(n) => {
            if n.is_f64() {
                py::float_repr(n.as_f64().unwrap())
            } else {
                n.to_string()
            }
        }
        other => other.to_string(),
    }
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new().timeout(std::time::Duration::from_secs(HTTP_TIMEOUT)).build()
}

fn read_json_response(r: ureq::Response) -> Result<Value, String> {
    let raw = r.into_string().map_err(|e| e.to_string())?;
    if raw.is_empty() {
        return Ok(json!({}));
    }
    serde_json::from_str(&raw).map_err(|e| e.to_string())
}

/// The real HTTP transport (arena._request).
pub fn http_request(method: &str, url: &str, token: Option<&str>, body: Option<&Value>) -> (u16, Value) {
    let mut req = agent().request(method, url).set("Accept", "application/json");
    if let Some(t) = token {
        req = req.set("Authorization", &format!("Bearer {}", t));
    }
    let res = match body {
        Some(b) => req.set("Content-Type", "application/json").send_string(&crate::pyjson::dumps(b)),
        None => req.call(),
    };
    handle_json(res)
}

fn handle_json(res: Result<ureq::Response, ureq::Error>) -> (u16, Value) {
    match res {
        Ok(r) => {
            let status = r.status();
            match read_json_response(r) {
                Ok(v) => (status, v),
                Err(e) => (0, json!({"error": e})),
            }
        }
        Err(ureq::Error::Status(code, r)) => {
            let reason = r.status_text().to_string();
            match r.into_string().ok().and_then(|s| serde_json::from_str::<Value>(&s).ok()) {
                Some(body) => (code, with_error(body)),
                None => (code, json!({"error": reason})),
            }
        }
        Err(e) => (0, json!({"error": e.to_string()})),
    }
}

/// Send raw bytes (sound uploads, portraits) and parse a JSON answer.
fn http_bytes(method: &str, url: &str, token: &str, ctype: &str, data: &[u8]) -> (u16, Value) {
    let res = agent()
        .request(method, url)
        .set("Accept", "application/json")
        .set("Content-Type", ctype)
        .set("Authorization", &format!("Bearer {}", token))
        .send_bytes(data);
    handle_json(res)
}

/// (status, content type, bytes) for a non-JSON GET.
fn http_raw(url: &str, token: &str) -> (u16, String, Vec<u8>) {
    match agent().get(url).set("Authorization", &format!("Bearer {}", token)).call() {
        Ok(r) => {
            let status = r.status();
            let ct = r.header("Content-Type").unwrap_or("").to_string();
            let mut buf = Vec::new();
            use std::io::Read;
            if r.into_reader().read_to_end(&mut buf).is_err() {
                return (0, String::new(), Vec::new());
            }
            (status, ct, buf)
        }
        Err(ureq::Error::Status(code, _)) => (code, String::new(), Vec::new()),
        Err(_) => (0, String::new(), Vec::new()),
    }
}

fn count(v: Option<&Value>) -> i64 {
    v.and_then(py::strict_int).filter(|n| *n > 0).unwrap_or(0)
}

/// (title, body) for the OS notification of one gift.
pub fn gift_notice(g: &Obj) -> (String, String) {
    let who = g.get("fromName").filter(|v| py::truthy(v)).or_else(|| g.get("fromHandle").filter(|v| py::truthy(v)));
    let who = match who {
        Some(Value::String(s)) => s.clone(),
        _ => "Someone".to_string(),
    };
    let (c, q) = (count(g.get("coins")), count(g.get("qty")));
    let mut parts: Vec<String> = Vec::new();
    if c > 0 {
        parts.push(format!("{} Poke Coin{}", c, if c == 1 { "" } else { "s" }));
    }
    if q > 0 {
        if let Some(Value::String(kind)) = g.get("kind") {
            if let Some((_, (name, plural))) = FOOD_LABELS.iter().find(|(k, _)| k == kind) {
                parts.push(format!("{} {}", q, if q == 1 { name } else { plural }));
            }
        }
    }
    let mut body = if parts.is_empty() { "a gift".to_string() } else { parts.join(" and ") };
    if let Some(Value::String(note)) = g.get("note") {
        if !note.is_empty() {
            body.push_str(": ");
            body.push_str(note);
        }
    }
    (format!("\u{1F381} {} sent you a gift", who), body)
}

/// A server-sent field as display text (str as-is, numbers stringified).
fn text_of(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => py_str(&Value::Number(n.clone())),
        _ => String::new(),
    }
}

/// The PNG bytes of a data:image/png;base64 URL, or None if it isn't one we
/// would send (not a PNG, too big, not square 32..256 px).
pub fn portrait_png(data_url: Option<&Value>) -> Option<Vec<u8>> {
    let s = data_url?.as_str()?;
    let b64 = s.strip_prefix("data:image/png;base64,")?;
    let b = base64::engine::general_purpose::STANDARD.decode(b64).ok()?;
    if b.len() > PORTRAIT_MAX_BYTES || b.len() < 33 || &b[..8] != b"\x89PNG\r\n\x1a\n" || &b[12..16] != b"IHDR" {
        return None;
    }
    let w = u32::from_be_bytes([b[16], b[17], b[18], b[19]]);
    let h = u32::from_be_bytes([b[20], b[21], b[22], b[23]]);
    (w == h && (32..=256).contains(&w)).then_some(b)
}

/// Rebuild a cali order from allowlisted keys only, one level at a time.
pub fn cali_order(body: &Value) -> Value {
    let empty = Map::new();
    let body = body.as_object().unwrap_or(&empty);
    let mut clean = Map::new();
    for k in CALI_ORDER_KEYS {
        if k != "diners" {
            if let Some(v) = body.get(k) {
                clean.insert(k.into(), v.clone());
            }
        }
    }
    let mut out = Vec::new();
    if let Some(Value::Array(diners)) = body.get("diners") {
        for d in diners {
            let Value::Object(d) = d else { continue };
            let mut row = Map::new();
            for k in CALI_DINER_KEYS {
                if k != "tacos" && k != "items" {
                    if let Some(v) = d.get(k) {
                        row.insert(k.into(), v.clone());
                    }
                }
            }
            let mut tacos = Map::new();
            if let Some(Value::Object(t)) = d.get("tacos") {
                for k in CALI_TACO_KEYS {
                    if let Some(v) = t.get(k) {
                        tacos.insert(k.into(), v.clone());
                    }
                }
            }
            row.insert("tacos".into(), Value::Object(tacos));
            let mut items = Map::new();
            if let Some(Value::Object(it)) = d.get("items") {
                for k in CALI_ITEM_KEYS {
                    if let Some(n) = it.get(k).and_then(py::strict_int) {
                        if n > 0 {
                            items.insert(k.into(), Value::from(n));
                        }
                    }
                }
            }
            if !items.is_empty() {
                row.insert("items".into(), Value::Object(items));
            }
            out.push(Value::Object(row));
        }
    }
    clean.insert("diners".into(), Value::Array(out));
    Value::Object(clean)
}

fn not_paired() -> (u16, Value) {
    (400, json!({"error": "not paired"}))
}

impl App {
    /// arena._request through the transport (stubbed in tests).
    pub fn arena_request(&self, method: &str, url: &str, token: Option<&str>, body: Option<&Value>) -> (u16, Value) {
        if let Some(t) = self.arena.transport.read().unwrap().clone() {
            return t(method, url, token, body);
        }
        http_request(method, url, token, body)
    }

    pub fn load_link(&self) -> Obj {
        match state::load_json_guarded(&self.paths.arena_link) {
            Some(Value::Object(o)) => o,
            _ => Map::new(),
        }
    }

    pub fn save_link(&self, data: &Value) -> std::io::Result<()> {
        let _g = self.arena.link_lock.lock().unwrap();
        state::atomic_write_json(&self.paths.arena_link, data, 0o600)
    }

    pub fn clear_link(&self) {
        let _g = self.arena.link_lock.lock().unwrap();
        let _ = std::fs::remove_file(&self.paths.arena_link);
    }

    fn base_url(&self) -> String {
        py::str_or_empty(&self.load_config(), "arenaUrl").trim_end_matches('/').to_string()
    }

    /// (token, base) when paired.
    pub fn authed(&self) -> Option<(String, String)> {
        let link = self.load_link();
        let token = link.get("token").filter(|v| py::truthy(v)).and_then(|v| v.as_str())?.to_string();
        let base = match link.get("url").filter(|v| py::truthy(v)).and_then(|v| v.as_str()) {
            Some(u) => u.to_string(),
            None => self.base_url(),
        };
        if base.is_empty() {
            return None;
        }
        Some((token, base))
    }

    pub fn is_paired(&self) -> bool {
        self.authed().is_some()
    }

    fn authed_get(&self, tail: &str) -> (u16, Value) {
        match self.authed() {
            Some((t, b)) => self.arena_request("GET", &format!("{}{}", b, tail), Some(&t), None),
            None => not_paired(),
        }
    }

    fn authed_send(&self, method: &str, tail: &str, body: Value) -> (u16, Value) {
        match self.authed() {
            Some((t, b)) => self.arena_request(method, &format!("{}{}", b, tail), Some(&t), Some(&body)),
            None => not_paired(),
        }
    }

    /// The last `days` days of activity as the wire allowlist (counts only).
    pub fn arena_build_payload(&self, share_cost: bool, trainer_name: &str, days: i64) -> Value {
        let today = py::utc_today();
        let window_start = today - Duration::days(days - 1);
        struct Acc {
            v: [i64; 8],
            cost: f64,
            tools: IndexMap<String, i64>,
        }
        let mut per_day: IndexMap<chrono::NaiveDate, Acc> = IndexMap::new();
        for path in self.claude_transcript_paths() {
            let Some(agg) = self.scan_file(&path) else { continue };
            for (diso, dd) in &agg.per_day {
                let Some(d) = py::parse_date(diso) else { continue };
                if d < window_start || d > today {
                    continue;
                }
                let acc = per_day.entry(d).or_insert_with(|| Acc { v: [0; 8], cost: 0.0, tools: IndexMap::new() });
                for (i, x) in [dd.prompts, dd.tools, dd.artifacts, dd.replies, dd.input, dd.output, dd.cache_read, dd.cache_creation].iter().enumerate() {
                    acc.v[i] += x;
                }
                acc.cost += dd.cost;
                for (name, c) in &dd.tools_by_name {
                    // Bucket here, before the name can reach the network.
                    let safe = if KNOWN_TOOLS.contains(&name.as_str()) { name.as_str() } else { "Other" };
                    *acc.tools.entry(safe.to_string()).or_insert(0) += c;
                }
            }
        }
        per_day.sort_keys();
        let mut out_days = Vec::new();
        for (d, acc) in per_day {
            let mut tb: Vec<(String, i64)> = acc.tools.into_iter().collect();
            tb.sort_by_key(|a| std::cmp::Reverse(a.1));
            let mut day = json!({
                "date": py::date_iso(&d),
                "prompts": acc.v[0], "tools": acc.v[1], "artifacts": acc.v[2], "replies": acc.v[3],
                "tokens": {"input": acc.v[4], "output": acc.v[5], "cacheRead": acc.v[6], "cacheCreation": acc.v[7]},
                "toolBreakdown": tb.into_iter().take(32).map(|(n, c)| json!({"name": n, "count": c})).collect::<Vec<_>>(),
            });
            if share_cost {
                day["costUSD"] = Value::from(py::round(acc.cost, 4));
            }
            out_days.push(day);
        }
        json!({"schemaVersion": SCHEMA_VERSION, "trainerName": trainer_name, "days": out_days})
    }

    fn payload_from_config(&self) -> Value {
        let cfg = self.load_config();
        self.arena_build_payload(py::opt_truthy(cfg.get("arenaShareCost")), py::str_or_empty(&cfg, "trainerName"), PUBLISH_WINDOW_DAYS)
    }

    /// Redeem a pairing code for a device token and remember it.
    pub fn arena_pair(&self, code: &str, label: Option<&Value>) -> (u16, Value) {
        let base = self.base_url();
        if base.is_empty() {
            return (400, json!({"error": "set the Arena server URL first"}));
        }
        let label = label.filter(|v| py::truthy(v)).cloned().unwrap_or_else(|| json!("claude-hq"));
        let (status, body) =
            self.arena_request("POST", &format!("{}/v1/auth/pair", base), None, Some(&json!({"code": py::strip(code).to_uppercase(), "label": label})));
        let b = body.as_object().cloned().unwrap_or_default();
        if status == 200 && b.get("token").map(py::truthy).unwrap_or(false) {
            let g = |k: &str| b.get(k).cloned().unwrap_or_else(|| json!(""));
            let link = json!({"url": base, "token": b["token"], "handle": g("handle"), "displayName": g("displayName"), "avatarUrl": g("avatarUrl")});
            if let Err(e) = self.save_link(&link) {
                return (500, json!({"error": format!("paired, but could not save the device token: {}", e)}));
            }
            return (200, json!({"ok": true, "handle": g("handle")}));
        }
        let err = b.get("detail").filter(|v| py::truthy(v)).or_else(|| b.get("error").filter(|v| py::truthy(v))).cloned().unwrap_or_else(|| json!("pairing failed"));
        (if status == 0 { 502 } else { status }, json!({"error": err}))
    }

    /// Push the current window. Returns (status, body).
    pub fn arena_publish(&self) -> (u16, Value) {
        let Some((token, base)) = self.authed() else { return (400, json!({"error": "not paired"})) };
        let payload = self.payload_from_config();
        let (status, body) = self.arena_request("POST", &format!("{}/v1/stats", base), Some(&token), Some(&payload));
        let b = body.as_object().cloned().unwrap_or_default();
        let err = if status == 200 {
            Value::Null
        } else {
            b.get("detail").filter(|v| py::truthy(v)).or_else(|| b.get("error").filter(|v| py::truthy(v))).cloned().unwrap_or_else(|| json!(format!("HTTP {}", status)))
        };
        *self.arena.last.lock().unwrap() = json!({
            "at": py::iso(&py::now_utc()), "ok": status == 200, "error": err,
            "accepted": if status == 200 { b.get("accepted").cloned().unwrap_or(json!(0)) } else { json!(0) },
        });
        if status == 200 {
            // Sent every publish, "" when the setting is off, so switching it off
            // unlinks this device on the next publish. Best effort: an Arena
            // without the route answers 404 and the publish still counts.
            let _ = self.arena_request("POST", &format!("{}/v1/me/account", base), Some(&token), Some(&json!({"key": self.account_key_if_enabled()})));
        }
        (status, body)
    }

    /// The account-board key: sha256 over a fixed prefix and the local Claude
    /// `oauthAccount.accountUuid`, or "" if there is none. Nothing else in
    /// `.claude.json` is read, and the file is never written or quarantined:
    /// it belongs to Claude Code.
    pub fn account_key(&self) -> String {
        use sha2::{Digest, Sha256};
        let uuid = state::load_json_plain(&self.paths.claude_json)
            .and_then(|v| v.get("oauthAccount")?.get("accountUuid")?.as_str().map(str::to_lowercase))
            .filter(|u| crate::app::is_uuid(u));
        match uuid {
            Some(u) => hex::encode(Sha256::digest(format!("claude-hq:account:v1:{}", u).as_bytes())),
            None => String::new(),
        }
    }

    fn account_key_if_enabled(&self) -> String {
        if py::opt_truthy(self.load_config().get("arenaAccountBoard")) { self.account_key() } else { String::new() }
    }

    /// The exact payload publish() would send, without sending it, plus the
    /// account key when that is on (it goes in its own request).
    pub fn arena_preview(&self) -> (u16, Value) {
        let mut p = self.payload_from_config();
        let key = self.account_key_if_enabled();
        if let (false, Some(o)) = (key.is_empty(), p.as_object_mut()) {
            o.insert("accountKey".into(), json!(key));
        }
        (200, p)
    }

    /// Connection state for the UI. Deliberately excludes the token.
    pub fn arena_status(&self) -> Value {
        let link = self.load_link();
        let cfg = self.load_config();
        let url = link
            .get("url")
            .filter(|v| py::truthy(v))
            .cloned()
            .or_else(|| cfg.get("arenaUrl").filter(|v| py::truthy(v)).cloned())
            .unwrap_or_else(|| json!(""));
        json!({
            "paired": link.get("token").map(py::truthy).unwrap_or(false),
            "url": url,
            "handle": link.get("handle").cloned().unwrap_or_else(|| json!("")),
            "displayName": link.get("displayName").cloned().unwrap_or_else(|| json!("")),
            "avatarUrl": link.get("avatarUrl").cloned().unwrap_or_else(|| json!("")),
            "enabled": py::opt_truthy(cfg.get("arenaEnabled")),
            "shareCost": py::opt_truthy(cfg.get("arenaShareCost")),
            "accountBoard": py::opt_truthy(cfg.get("arenaAccountBoard")),
            "lastPublish": self.arena.last.lock().unwrap().clone(),
        })
    }

    pub fn arena_board(&self, window: &str) -> (u16, Value) {
        self.authed_get(&format!("/v1/board?window={}", window))
    }

    pub fn arena_list_sounds(&self) -> (u16, Value) {
        self.authed_get("/v1/sounds")
    }

    pub fn arena_get_sound(&self, file: &str) -> (u16, String, Vec<u8>) {
        match self.authed() {
            Some((t, b)) => http_raw(&format!("{}/v1/sounds/{}", b, py::quote(file, "/")), &t),
            None => (400, String::new(), Vec::new()),
        }
    }

    pub fn arena_upload_sound(&self, filename: &str, data: &[u8], ctype: &str) -> (u16, Value) {
        match self.authed() {
            Some((t, b)) => http_bytes("POST", &format!("{}/v1/sounds?name={}", b, py::quote(filename, "/")), &t, ctype, data),
            None => not_paired(),
        }
    }

    pub fn arena_ws_ticket(&self) -> (u16, Value) {
        let Some((t, b)) = self.authed() else { return not_paired() };
        let (status, mut body) = self.arena_request("POST", &format!("{}/v1/auth/ticket", b), Some(&t), None);
        if status == 200 {
            if let Value::Object(o) = &mut body {
                o.insert("wsUrl".into(), Value::from(b.replace("https://", "wss://").replace("http://", "ws://")));
            }
        }
        (status, body)
    }

    pub fn arena_send_nudge(&self, to: &str, note: Option<&Value>) -> (u16, Value) {
        let note = note.filter(|v| py::truthy(v)).cloned().unwrap_or_else(|| json!(""));
        self.authed_send("POST", "/v1/nudge", json!({"toHandle": to, "note": note}))
    }

    pub fn arena_drain_nudges(&self) -> Vec<Value> {
        let Some((t, b)) = self.authed() else { return Vec::new() };
        let (code, body) = self.arena_request("GET", &format!("{}/v1/nudges", b), Some(&t), None);
        match (code, body.get("nudges")) {
            (200, Some(Value::Array(n))) => n.clone(),
            _ => Vec::new(),
        }
    }

    /// GET the pantry (action None) or POST one allowlisted pantry action.
    pub fn arena_pantry(&self, action: Option<&str>, body: Option<&Value>) -> (u16, Value) {
        let Some((t, b)) = self.authed() else { return not_paired() };
        let Some(action) = action else {
            return self.arena_request("GET", &format!("{}/v1/pantry", b), Some(&t), None);
        };
        if !PANTRY_ACTIONS.contains(&action) {
            return (400, json!({"error": "unknown pantry action"}));
        }
        let mut clean = Map::new();
        if let Some(Value::Object(o)) = body {
            for k in PANTRY_KEYS {
                if let Some(v) = o.get(k) {
                    clean.insert(k.into(), v.clone());
                }
            }
        }
        self.arena_request("POST", &format!("{}/v1/pantry/{}", b, action), Some(&t), Some(&Value::Object(clean)))
    }

    pub fn arena_quest_reward(&self, rid: &str, kind: &str, quest_id: &str, tier: Option<&str>, coins: i64) -> (u16, Value) {
        let mut body = json!({"requestId": rid, "kind": kind, "questId": quest_id, "coins": coins});
        if let Some(t) = tier {
            body["tier"] = Value::from(t);
        }
        self.authed_send("POST", "/v1/pantry/reward", body)
    }

    pub fn arena_drain_gifts(&self) -> Vec<Value> {
        if py::now_ts() < *self.arena.gift_off_until.lock().unwrap() {
            return Vec::new();
        }
        let Some((t, b)) = self.authed() else { return Vec::new() };
        let (code, body) = self.arena_request("POST", &format!("{}/v1/pantry/gifts/drain", b), Some(&t), Some(&json!({})));
        if code == 404 {
            *self.arena.gift_off_until.lock().unwrap() = py::now_ts() + GIFT_DRAIN_BACKOFF_SECS;
            return Vec::new();
        }
        match (code, body.get("gifts")) {
            (200, Some(Value::Array(g))) => g.iter().filter(|x| x.is_object()).cloned().collect(),
            _ => Vec::new(),
        }
    }

    /// One poller pass: nudges ping, gifts arrive silently.
    pub fn arena_poll_once(&self, notify: &dyn Fn(&str, &str, bool) -> Result<(), String>) {
        for n in self.arena_drain_nudges() {
            let Value::Object(n) = n else { continue };
            let mut who = text_of(n.get("fromName"));
            if who.is_empty() {
                who = text_of(n.get("fromHandle"));
            }
            if who.is_empty() {
                who = "Someone".into();
            }
            let note = text_of(n.get("note"));
            let body = if note.is_empty() { format!("{} nudged you", who) } else { format!("{} nudged you: {}", who, note) };
            let _ = notify(&format!("👋 {} nudged you", who), &body, true);
        }
        for g in self.arena_drain_gifts() {
            if let Value::Object(g) = g {
                let (t, b) = gift_notice(&g);
                let _ = notify(&t, &b, false);
            }
        }
    }

    // ---- private rooms -----------------------------------------------------

    pub fn arena_rooms_directory(&self) -> (u16, Value) {
        self.authed_get("/v1/rooms/directory")
    }

    pub fn arena_room_members(&self, rid: &str) -> (u16, Value) {
        if !ROOM_ID_RE.is_match(rid) {
            return (400, json!({"error": "bad room id"}));
        }
        self.authed_get(&format!("/v1/rooms/members?roomId={}", py::quote(rid, "")))
    }

    /// The room POSTs (body already validated by the dashboard layer).
    pub fn arena_room_post(&self, op: &str, body: &Obj) -> (u16, Value) {
        let s = |k: &str| body.get(k).cloned().unwrap_or(Value::Null);
        let rid_ok = valid_room_id(&s("roomId"));
        let uid_ok = body.get("userId").and_then(|v| v.as_str()).map(valid_user_id).unwrap_or(false);
        let (tail, payload) = match op {
            "create" => ("create", json!({"name": s("name"), "password": s("password")})),
            "join" => ("join", json!({"roomId": s("roomId"), "password": s("password")})),
            "leave" => ("leave", json!({"roomId": s("roomId")})),
            "rename" => ("rename", json!({"roomId": s("roomId"), "name": s("name")})),
            "password" => ("password", json!({"roomId": s("roomId"), "password": s("password"),
                                              "signOutOthers": body.get("signOutOthers") == Some(&Value::Bool(true))})),
            "kick" => ("kick", json!({"roomId": s("roomId"), "userId": s("userId")})),
            "unban" => ("unban", json!({"roomId": s("roomId"), "userId": s("userId")})),
            "delete" => ("delete", json!({"roomId": s("roomId")})),
            _ => return (404, json!({"error": "not found"})),
        };
        if op != "create" && !rid_ok {
            return (400, json!({"error": "bad room id"}));
        }
        if matches!(op, "kick" | "unban") && !uid_ok {
            return (400, json!({"error": "bad user id"}));
        }
        self.authed_send("POST", &format!("/v1/rooms/{}", tail), payload)
    }

    // ---- cali --------------------------------------------------------------

    pub fn arena_cali_board(&self, window: &str) -> (u16, Value) {
        let Some((t, b)) = self.authed() else { return not_paired() };
        let w = if CALI_WINDOWS.contains(&window) { window } else { "season" };
        self.arena_request("GET", &format!("{}/v1/cali/board?window={}", b, w), Some(&t), None)
    }

    pub fn arena_cali_orders(&self) -> (u16, Value) {
        self.authed_get("/v1/cali/orders")
    }

    pub fn arena_cali_log_order(&self, body: &Value) -> (u16, Value) {
        self.authed_send("POST", "/v1/cali/orders", cali_order(body))
    }

    // ---- HQ 2.1 ------------------------------------------------------------

    pub fn arena_hq_me(&self) -> (u16, Value) {
        self.authed_get("/v1/hq/me")
    }

    pub fn arena_hq_update(&self, open: Option<&Value>, look: Option<&Value>, crew: Option<&Value>) -> (u16, Value) {
        let Some((t, b)) = self.authed() else { return not_paired() };
        let mut body = Map::new();
        if let Some(Value::Bool(o)) = open {
            body.insert("open".into(), Value::Bool(*o));
        }
        if let Some(Value::Object(l)) = look {
            let mut m = Map::new();
            for k in HQ_LOOK_KEYS {
                if let Some(Value::String(s)) = l.get(k) {
                    m.insert(k.into(), Value::from(s.clone()));
                }
            }
            body.insert("look".into(), Value::Object(m));
        }
        if let Some(Value::Object(c)) = crew {
            let mut m = Map::new();
            for k in HQ_CREW_KEYS {
                let n = c.get(k).filter(|v| py::truthy(v)).and_then(py::py_int).unwrap_or(0);
                m.insert(k.into(), Value::from(n.clamp(0, 64)));
            }
            body.insert("crew".into(), Value::Object(m));
        }
        self.arena_request("PUT", &format!("{}/v1/hq/me", b), Some(&t), Some(&Value::Object(body)))
    }

    pub fn arena_hq_open(&self) -> (u16, Value) {
        self.authed_get("/v1/hq/open")
    }

    pub fn arena_hq_visit(&self, uid: &str) -> (u16, Value) {
        if self.authed().is_none() {
            return not_paired();
        }
        if !valid_user_id(uid) {
            return (400, json!({"error": "bad user id"}));
        }
        self.authed_get(&format!("/v1/hq/{}", uid))
    }

    pub fn arena_portrait_put(&self, png: &[u8]) -> (u16, Value) {
        match self.authed() {
            Some((t, b)) => http_bytes("PUT", &format!("{}/v1/me/portrait", b), &t, "image/png", png),
            None => not_paired(),
        }
    }

    pub fn arena_portrait_delete(&self) -> (u16, Value) {
        match self.authed() {
            Some((t, b)) => self.arena_request("DELETE", &format!("{}/v1/me/portrait", b), Some(&t), None),
            None => not_paired(),
        }
    }

    // ---- music ---------------------------------------------------------------

    pub fn arena_music_now(&self) -> (u16, Value) {
        self.authed_get("/v1/music/now")
    }

    pub fn arena_music_now_put(&self, track: &Map<String, Value>) -> (u16, Value) {
        if self.authed().is_none() {
            return not_paired();
        }
        match music::wire_track(&Value::Object(track.clone())) {
            Some(body) => self.authed_send("PUT", "/v1/music/now", Value::Object(body)),
            None => (400, json!({"error": "no track"})),
        }
    }

    pub fn arena_music_now_clear(&self) -> (u16, Value) {
        match self.authed() {
            Some((t, b)) => self.arena_request("DELETE", &format!("{}/v1/music/now", b), Some(&t), None),
            None => not_paired(),
        }
    }

    // ---- progression, boards, profiles ---------------------------------------

    pub fn arena_progress(&self) -> (u16, Value) {
        self.authed_get("/v1/progress/me")
    }

    pub fn arena_leaderboards(&self, game: &str, key: Option<&str>) -> (u16, Value) {
        static KEY_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[a-z0-9_-]{1,40}$").unwrap());
        if self.authed().is_none() {
            return not_paired();
        }
        if !GAMES.contains(&game) {
            return (400, json!({"error": "unknown game"}));
        }
        let q = key.filter(|k| KEY_RE.is_match(k)).map(|k| format!("?key={}", k)).unwrap_or_default();
        self.authed_get(&format!("/v1/leaderboards/{}{}", game, q))
    }

    pub fn arena_profile(&self, uid: &str) -> (u16, Value) {
        if self.authed().is_none() {
            return not_paired();
        }
        if uid != "me" && !valid_user_id(uid) {
            return (400, json!({"error": "bad user id"}));
        }
        self.authed_get(&format!("/v1/profile/{}", uid))
    }

    pub fn arena_server_stats(&self) -> (u16, Value) {
        self.authed_get("/v1/server/stats")
    }

    pub fn arena_cosmetics(&self, action: Option<&str>, body: &Value) -> (u16, Value) {
        static ITEM_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[a-z]-[a-z]{2,12}$").unwrap());
        if self.authed().is_none() {
            return not_paired();
        }
        let Some(action) = action else { return self.authed_get("/v1/cosmetics") };
        let item = body.get("item").cloned().unwrap_or(Value::Null);
        if !item.is_null() && !item.as_str().map(|s| ITEM_RE.is_match(s)).unwrap_or(false) {
            return (400, json!({"error": "bad item"}));
        }
        match action {
            "buy" => self.authed_send("POST", "/v1/cosmetics/buy", json!({"requestId": format!("cos-{}", &py::uuid4_hex()[..24]), "item": item})),
            "equip" => {
                let slot = body.get("slot").and_then(|v| v.as_str()).filter(|s| COS_SLOTS.contains(s));
                match slot {
                    Some(s) => self.authed_send("POST", "/v1/cosmetics/equip", json!({"slot": s, "item": item})),
                    None => (400, json!({"error": "unknown slot"})),
                }
            }
            _ => (400, json!({"error": "unknown cosmetics action"})),
        }
    }

    pub fn arena_market_sell(&self, cat: Option<&Value>, qty: Option<&Value>) -> (u16, Value) {
        if self.authed().is_none() {
            return not_paired();
        }
        let cat = cat.and_then(|v| v.as_str()).filter(|c| MARKET_CATS.contains(c));
        let qty = qty.and_then(py::strict_int).filter(|q| (1..=10).contains(q));
        match (cat, qty) {
            (Some(c), Some(q)) => self.authed_send("POST", "/v1/market/sell", json!({"requestId": format!("sell-{}", &py::uuid4_hex()[..24]), "cat": c, "qty": q})),
            _ => (400, json!({"error": "sell 1 to 10 of one kind"})),
        }
    }

    pub fn arena_crews(&self, action: &str, body: &Value) -> (u16, Value) {
        if self.authed().is_none() {
            return not_paired();
        }
        let field = |k: &str, cap: usize| -> String {
            match body.get(k).filter(|v| py::truthy(v)) {
                Some(v) => py::take_chars(&py_str(v), cap),
                None => String::new(),
            }
        };
        match action {
            "board" => self.authed_get("/v1/crews"),
            "mine" => self.authed_get("/v1/crews/mine"),
            "create" => self.authed_send("POST", "/v1/crews/create", json!({"name": field("name", 40), "tag": field("tag", 40), "color": field("color", 40)})),
            "join" => self.authed_send("POST", "/v1/crews/join", json!({"code": field("code", 12)})),
            "leave" => self.authed_send("POST", "/v1/crews/leave", json!({})),
            _ => (400, json!({"error": "unknown crews action"})),
        }
    }

    /// The account board, or the groups you are in. Read-only.
    pub fn arena_accounts(&self, action: &str) -> (u16, Value) {
        match action {
            "board" => self.authed_get("/v1/accounts"),
            "mine" => self.authed_get("/v1/accounts/mine"),
            _ => (400, json!({"error": "unknown accounts action"})),
        }
    }

    /// Standings grouped by Claude account, for one window. Read-only; the
    /// window is already allowlisted by the route.
    ///
    /// An Arena from before `/v1/accounts/standings` answers 404: then the same
    /// shape is assembled here from routes it does have, the window's board
    /// (`/v1/board`) and the account groups (`/v1/accounts`, `/v1/accounts/mine`),
    /// joined on handle. Nothing extra is sent; these are the same reads the
    /// Arena tab already makes.
    pub fn arena_account_standings(&self, window: &str) -> (u16, Value) {
        let (code, body) = self.authed_get(&format!("/v1/accounts/standings?window={}", window));
        if code != 404 {
            return (code, body);
        }
        let (bc, board) = self.authed_get(&format!("/v1/board?window={}", window));
        let (ac, accounts) = self.authed_get("/v1/accounts");
        if bc != 200 || ac != 200 {
            return (code, body);
        }
        let (_, mine) = self.authed_get("/v1/accounts/mine");
        (200, account_standings_from(&board, &accounts, &mine))
    }

    pub fn arena_quickplay(&self, action: &str, body: &Value) -> (u16, Value) {
        if self.authed().is_none() {
            return not_paired();
        }
        match action {
            "status" => self.authed_get("/v1/quickplay/status"),
            "join" => match body.get("game").and_then(|v| v.as_str()).filter(|g| QUICKPLAY_GAMES.contains(g)) {
                Some(g) => self.authed_send("POST", "/v1/quickplay/join", json!({"game": g})),
                None => (400, json!({"error": "unknown game"})),
            },
            "leave" => self.authed_send("POST", "/v1/quickplay/leave", json!({})),
            _ => (400, json!({"error": "unknown quickplay action"})),
        }
    }
}

/// Background: publish every PUBLISH_INTERVAL_SECS while enabled and paired.
pub fn start_publisher(app: Arc<App>) {
    std::thread::Builder::new()
        .name("arena-publisher".into())
        .spawn(move || loop {
            std::thread::sleep(std::time::Duration::from_secs(PUBLISH_INTERVAL_SECS));
            let cfg = app.load_config();
            if py::opt_truthy(cfg.get("arenaEnabled")) && app.load_link().get("token").map(py::truthy).unwrap_or(false) {
                app.arena_publish();
            }
        })
        .ok();
}

/// Background: nudges and gifts become native notifications while enabled.
pub fn start_nudge_poller(app: Arc<App>, notify: fn(&str, &str, bool) -> Result<(), String>) {
    std::thread::Builder::new()
        .name("arena-nudge-poller".into())
        .spawn(move || loop {
            std::thread::sleep(std::time::Duration::from_secs(NUDGE_POLL_SECS));
            let cfg = app.load_config();
            if py::opt_truthy(cfg.get("arenaEnabled")) && app.load_link().get("token").map(py::truthy).unwrap_or(false) {
                app.arena_poll_once(&notify);
            }
        })
        .ok();
}

/// The `/v1/accounts/standings` shape, built from an older Arena's window
/// board and account groups. Scored like the server: a member's window numbers
/// are their Standings row (zero if they have none), an account sums them
/// (best member for days and streak), and its level is the crew formula, the
/// average member's level plus one per extra member. `/v1/accounts` lists only
/// the first five members of each group, so a larger group is completed from
/// `/v1/accounts/mine` when it is yours, and otherwise marked `partial`.
pub fn account_standings_from(board: &Value, accounts: &Value, mine: &Value) -> Value {
    use crate::creature::{derive_level, rank_for_level};
    let num = |v: &Value, k: &str| v.get(k).and_then(Value::as_i64).unwrap_or(0);
    let text = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let entries: Vec<&Value> = board.get("entries").and_then(Value::as_array).map(|a| a.iter().collect()).unwrap_or_default();
    let by_handle: std::collections::HashMap<String, &Value> = entries.iter().map(|e| (text(e, "handle").to_lowercase(), *e)).collect();
    let handles_of = |a: &Value| -> Vec<String> {
        a.get("members").and_then(Value::as_array).map(|m| m.iter().map(|x| text(x, "handle")).collect()).unwrap_or_default()
    };
    let mine_groups: Vec<&Value> = mine.get("accounts").and_then(Value::as_array).map(|a| a.iter().collect()).unwrap_or_default();

    let mut out: Vec<Value> = Vec::new();
    for a in accounts.get("accounts").and_then(Value::as_array).into_iter().flatten() {
        let preview = handles_of(a);
        // Your own group: the full list, from the /mine entry that contains this preview.
        let full = mine_groups.iter().find(|g| {
            let hs = handles_of(g);
            !preview.is_empty() && preview.iter().all(|h| hs.contains(h))
        });
        let roster: Vec<&Value> = full.or(Some(&a))
            .and_then(|g| g.get("members").and_then(Value::as_array))
            .map(|m| m.iter().collect())
            .unwrap_or_default();
        let count = (num(a, "memberCount") as usize).max(roster.len());
        let mut members: Vec<Value> = roster.iter().map(|m| {
            let h = text(m, "handle");
            match by_handle.get(&h.to_lowercase()) {
                Some(e) => json!({
                    "handle": h, "displayName": text(e, "displayName"), "trainerName": text(e, "trainerName"),
                    "avatarUrl": text(e, "avatarUrl"), "xp": num(e, "xp"), "level": num(e, "level"),
                    "rankTitle": text(e, "rankTitle"), "prompts": num(e, "prompts"), "tools": num(e, "tools"),
                    "artifacts": num(e, "artifacts"), "activeDays": num(e, "activeDays"), "streak": num(e, "streak"),
                    "tokensTotal": num(e, "tokensTotal"), "share": 0, "isYou": e.get("isYou").and_then(Value::as_bool).unwrap_or(false),
                }),
                None => {
                    let level = derive_level(0).0;
                    let dn = text(m, "displayName");
                    json!({
                        "handle": h.clone(), "displayName": if dn.is_empty() { h } else { dn }, "trainerName": "",
                        "avatarUrl": text(m, "avatarUrl"), "xp": 0, "level": level, "rankTitle": rank_for_level(level),
                        "prompts": 0, "tools": 0, "artifacts": 0, "activeDays": 0, "streak": 0, "tokensTotal": 0,
                        "share": 0, "isYou": false,
                    })
                }
            }
        }).collect();
        let sum = |k: &str, ms: &[Value]| ms.iter().map(|m| num(m, k)).sum::<i64>();
        let best = |k: &str, ms: &[Value]| ms.iter().map(|m| num(m, k)).max().unwrap_or(0);
        let xp = sum("xp", &members);
        for m in members.iter_mut() {
            let mx = num(m, "xp");
            m["share"] = json!(if xp > 0 { (mx * 100 + xp / 2) / xp } else { 0 });
        }
        members.sort_by(|x, y| num(y, "xp").cmp(&num(x, "xp")).then(text(x, "handle").cmp(&text(y, "handle"))));
        let n = members.len() as i64;
        let is_mine = full.is_some() || a.get("isMine").and_then(Value::as_bool).unwrap_or(false) || members.iter().any(|m| m["isYou"] == json!(true));
        out.push(json!({
            "rank": 0, "xp": xp, "level": derive_level(xp / n.max(1)).0 + n - 1,
            "prompts": sum("prompts", &members), "tools": sum("tools", &members), "artifacts": sum("artifacts", &members),
            "activeDays": best("activeDays", &members), "streak": best("streak", &members),
            "tokensTotal": sum("tokensTotal", &members), "memberCount": count, "isMine": is_mine,
            "partial": members.len() < count, "members": members,
        }));
    }
    out.sort_by(|x, y| {
        let first = |v: &Value| v["members"].get(0).map(|m| text(m, "handle").to_lowercase()).unwrap_or_default();
        num(y, "xp").cmp(&num(x, "xp")).then(first(x).cmp(&first(y)))
    });
    for (i, a) in out.iter_mut().enumerate() {
        a["rank"] = json!(i as i64 + 1);
    }
    json!({
        "window": board.get("window").cloned().unwrap_or(Value::Null),
        "startsOn": board.get("startsOn").cloned().unwrap_or(Value::Null),
        "endsOn": board.get("endsOn").cloned().unwrap_or(Value::Null),
        "seasonName": board.get("seasonName").cloned().unwrap_or(Value::Null),
        "accounts": out,
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::app::testutil::*;

    pub const FORBIDDEN_KEYS: [&str; 16] = [
        "prompt", "prompts_text", "reply", "text", "content", "path", "paths", "cwd", "folder", "project", "projectName",
        "sessionId", "sessionTitle", "title", "file", "files",
    ];

    pub fn collect_keys(v: &Value, out: &mut std::collections::HashSet<String>) {
        match v {
            Value::Object(o) => {
                for (k, x) in o {
                    out.insert(k.clone());
                    collect_keys(x, out);
                }
            }
            Value::Array(a) => a.iter().for_each(|x| collect_keys(x, out)),
            _ => {}
        }
    }

    pub fn leaks(v: &Value) -> Vec<String> {
        let mut ks = std::collections::HashSet::new();
        collect_keys(v, &mut ks);
        FORBIDDEN_KEYS.iter().filter(|k| ks.contains(**k)).map(|k| k.to_string()).collect()
    }

    pub type Sent = Arc<Mutex<Vec<(String, String, Option<String>, Option<Value>)>>>;

    /// Pair the app (a link file) and capture every request.
    pub fn stub(app: &App, url: &str, reply: Arc<Mutex<(u16, Value)>>) -> Sent {
        app.save_link(&json!({"token": "T", "url": url, "handle": "ann"})).unwrap();
        let sent: Sent = Arc::new(Mutex::new(Vec::new()));
        let s2 = sent.clone();
        *app.arena.transport.write().unwrap() = Some(Arc::new(move |m, u, t, b| {
            s2.lock().unwrap().push((m.to_string(), u.to_string(), t.map(String::from), b.cloned()));
            reply.lock().unwrap().clone()
        }));
        sent
    }

    #[test]
    fn payload_buckets_mcp_tools_and_cost_is_opt_in() {
        let (_d, app) = app();
        let p = app.paths.projects_dir.join("some-project").join("session.jsonl");
        let today = py::iso(&py::now_utc()).replace("+00:00", "Z");
        write_jsonl(&p, &[
            json!({"type": "user", "timestamp": today, "message": {"content": "go"}}),
            json!({"type": "assistant", "timestamp": today, "message": {"id": "m", "model": "x", "usage": {"input_tokens": 100, "output_tokens": 200},
                "content": [{"type": "tool_use", "id": "a", "name": "Bash", "input": {}}, {"type": "tool_use", "id": "b", "name": "mcp__acme_corp__deploy", "input": {"path": "/secret"}}]}}),
        ]);
        let payload = app.arena_build_payload(false, "Ash", 30);
        let blob = crate::pyjson::dumps(&payload);
        assert!(!blob.contains("acme_corp") && !blob.contains("/secret"));
        let names: Vec<&str> = payload["days"][0]["toolBreakdown"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert!(names.contains(&"Bash") && names.contains(&"Other"));
        assert!(leaks(&payload).is_empty());
        assert!(payload["days"][0].get("costUSD").is_none());
        assert!(app.arena_build_payload(true, "", 30)["days"][0].get("costUSD").is_some());
        assert_eq!(payload["trainerName"], "Ash");
        assert_eq!(payload["days"][0]["tokens"]["output"], 200);
        assert_eq!(payload["days"][0]["prompts"], 1);
    }

    #[test]
    fn pantry_wire_is_allowlisted() {
        let (_d, app) = app();
        let sent = stub(&app, "https://arena.example", Arc::new(Mutex::new((200, json!({"coins": 0})))));
        assert_eq!(app.arena_pantry(None, None).0, 200);
        let rid = "a1b2c3d4-e5f6-4a7b-8c9d-0e1f2a3b4c5d";
        app.arena_pantry(Some("eat"), Some(&json!({"requestId": rid, "kind": "berry", "sessionId": "abc", "title": "Secret plan",
            "path": "/Users/me/client", "cwd": "/x", "fatigue": {"state": "tired"}, "retry": true})));
        app.arena_pantry(Some("claim"), None);
        assert_eq!(app.arena_pantry(Some("steal"), Some(&json!({}))), (400, json!({"error": "unknown pantry action"})));
        let s = sent.lock().unwrap();
        assert_eq!(s[0].0, "GET");
        assert_eq!(s[0].1, "https://arena.example/v1/pantry");
        assert_eq!(s[1].3, Some(json!({"requestId": rid, "kind": "berry"})));
        assert_eq!(s[2].3, Some(json!({})));
        assert_eq!(s.len(), 3);
    }

    #[test]
    fn rally_boards_and_quickplay() {
        let (_d, app) = app();
        let sent = stub(&app, "https://arena.example", Arc::new(Mutex::new((200, json!({"entries": []})))));
        assert_eq!(app.arena_leaderboards("rally", Some("c-0123456789ab")).0, 200);
        assert_eq!(app.arena_leaderboards("rally", Some("../x")).0, 200);
        assert_eq!(app.arena_leaderboards("rallyx", None).0, 400);
        assert_eq!(app.arena_quickplay("join", &json!({"game": "rally"})).0, 200);
        let s = sent.lock().unwrap();
        assert_eq!(s[0].1, "https://arena.example/v1/leaderboards/rally?key=c-0123456789ab");
        assert_eq!(s[1].1, "https://arena.example/v1/leaderboards/rally");
        assert_eq!(s[2].3, Some(json!({"game": "rally"})));
        assert_eq!(s.len(), 3);
    }

    #[test]
    fn not_paired_makes_no_request() {
        let (_d, app) = app();
        let sent = stub(&app, "https://arena.example", Arc::new(Mutex::new((200, json!({})))));
        app.clear_link();
        assert_eq!(app.arena_pantry(None, None), (400, json!({"error": "not paired"})));
        assert_eq!(app.arena_send_nudge("gary", None).0, 400);
        assert!(app.arena_drain_gifts().is_empty());
        assert!(sent.lock().unwrap().is_empty());
    }

    #[test]
    fn link_url_wins_over_config() {
        let (_d, app) = app();
        let sent = stub(&app, "https://arena.example", Arc::new(Mutex::new((200, json!({})))));
        app.save_config(&json!({"arenaUrl": "https://other.example"})).unwrap();
        app.arena_pantry(None, None);
        app.save_link(&json!({"token": "T"})).unwrap();
        app.arena_pantry(None, None);
        let s = sent.lock().unwrap();
        assert_eq!(s[0].1, "https://arena.example/v1/pantry");
        assert_eq!(s[1].1, "https://other.example/v1/pantry");
    }

    #[test]
    fn gift_drain_backs_off_after_404() {
        let (_d, app) = app();
        let reply = Arc::new(Mutex::new((404, json!({"detail": "Not Found", "error": "Not Found"}))));
        let sent = stub(&app, "https://arena.example", reply.clone());
        assert!(app.arena_drain_gifts().is_empty());
        assert!(*app.arena.gift_off_until.lock().unwrap() >= py::now_ts() + GIFT_DRAIN_BACKOFF_SECS - 5.0);
        *reply.lock().unwrap() = (200, json!({"gifts": [{"id": "g1"}]}));
        assert!(app.arena_drain_gifts().is_empty());
        assert_eq!(sent.lock().unwrap().len(), 1);
        *app.arena.gift_off_until.lock().unwrap() = py::now_ts() - 1.0;
        assert_eq!(app.arena_drain_gifts(), vec![json!({"id": "g1"})]);
        *reply.lock().unwrap() = (200, json!({"gifts": [{"id": "g1"}, "junk"]}));
        assert_eq!(app.arena_drain_gifts(), vec![json!({"id": "g1"})]);
    }

    #[test]
    fn gift_notices() {
        let o = |v: Value| v.as_object().unwrap().clone();
        assert_eq!(gift_notice(&o(json!({"fromName": "Gary", "fromHandle": "gary", "coins": 2, "kind": "riceball", "qty": 1, "note": "for your sleepy Voltkit"}))),
                   ("\u{1F381} Gary sent you a gift".to_string(), "2 Poke Coins and 1 Rice Ball: for your sleepy Voltkit".to_string()));
        assert_eq!(gift_notice(&o(json!({"fromHandle": "ash", "coins": 1, "qty": 0, "kind": null}))).1, "1 Poke Coin");
        assert_eq!(gift_notice(&o(json!({"fromName": "Ash", "kind": "berry", "qty": 2}))).1, "2 Berries");
        assert_eq!(gift_notice(&o(json!({"fromName": "Ash", "kind": "tonic", "qty": 3}))).1, "3 Revive Tonics");
        assert_eq!(gift_notice(&o(json!({}))), ("\u{1F381} Someone sent you a gift".to_string(), "a gift".to_string()));
        assert_eq!(gift_notice(&o(json!({"fromName": 7, "coins": true, "qty": "2", "kind": ["berry"], "note": 5}))).1, "a gift");
        assert_eq!(gift_notice(&o(json!({"kind": "pizza", "qty": 2}))).1, "a gift");
    }

    #[test]
    fn poller_tolerates_bad_items() {
        let (_d, app) = app();
        let replies = Arc::new(Mutex::new(Vec::<(String, String, bool)>::new()));
        app.save_link(&json!({"token": "T", "url": "https://arena.example"})).unwrap();
        *app.arena.transport.write().unwrap() = Some(Arc::new(|_m, u, _t, _b| {
            if u.ends_with("/v1/nudges") {
                (200, json!({"nudges": [null, "junk", {"fromName": {"x": 1}, "fromHandle": 7, "note": ["n"]}, {"fromName": "Ash", "note": 42}]}))
            } else {
                (200, json!({"gifts": [null, {"fromName": "Gary", "coins": 1}]}))
            }
        }));
        let r2 = replies.clone();
        app.arena_poll_once(&move |t, b, s| {
            r2.lock().unwrap().push((t.into(), b.into(), s));
            Ok(())
        });
        assert_eq!(*replies.lock().unwrap(), vec![
            ("👋 7 nudged you".to_string(), "7 nudged you".to_string(), true),
            ("👋 Ash nudged you".to_string(), "Ash nudged you: 42".to_string(), true),
            ("\u{1F381} Gary sent you a gift".to_string(), "1 Poke Coin".to_string(), false),
        ]);
    }

    #[test]
    fn errors_and_ids() {
        assert_eq!(with_error(json!({"detail": "no such person"})), json!({"detail": "no such person", "error": "no such person"}));
        assert_eq!(with_error(json!({"detail": [{"msg": "field required"}, {"msg": "too long"}]}))["error"], "field required; too long");
        assert_eq!(with_error(json!({"error": "not paired"})), json!({"error": "not paired"}));
        assert_eq!(with_error(json!([1, 2])), json!([1, 2]));
        assert!(valid_room_id(&json!("r_ABCDEFGHIJKLMNOPQRSTUv")));
        assert!(!valid_room_id(&json!("r_short")) && !valid_room_id(&json!("R_ABCDEFGHIJKLMNOPQRSTUv")) && !valid_room_id(&json!(123)));
        assert!(valid_user_id("01234567-89ab-cdef-0123-456789abcdef"));
        assert!(!valid_user_id("01234567-89AB-CDEF-0123-456789ABCDEF"));
    }

    #[test]
    fn cali_allowlist() {
        let rid = "a1b2c3d4-e5f6-4a7b-8c9d-0e1f2a3b4c5d";
        let clean = cali_order(&json!({"requestId": rid, "note": "bogo night", "date": "2026-09-29", "sessionId": "x", "projectName": "stealth",
            "diners": [{"handle": "ana", "name": "Ana", "tacos": {"mildHard": 3}, "cwd": "/Users/ana", "title": "secret",
                        "items": {"burrito": 2, "pizza": 9, "soda": "1", "guac": true, "nachos": 0}}, "ana", null]}));
        assert!(leaks(&clean).is_empty());
        let keys: Vec<&String> = clean.as_object().unwrap().keys().collect();
        assert_eq!(keys, vec!["requestId", "date", "note", "diners"]);
        assert_eq!(clean["diners"].as_array().unwrap().len(), 1);
        assert_eq!(clean["diners"][0]["items"], json!({"burrito": 2}));
        assert_eq!(cali_order(&json!(null)), json!({"diners": []}));
    }

    #[test]
    fn portraits() {
        fn png(w: u32, h: u32) -> Vec<u8> {
            let mut b = b"\x89PNG\r\n\x1a\n".to_vec();
            b.extend_from_slice(&13u32.to_be_bytes());
            b.extend_from_slice(b"IHDR");
            b.extend_from_slice(&w.to_be_bytes());
            b.extend_from_slice(&h.to_be_bytes());
            b.extend_from_slice(&[8, 6, 0, 0, 0, 0, 0, 0, 0]);
            b
        }
        let url = |b: &[u8]| json!(format!("data:image/png;base64,{}", base64::engine::general_purpose::STANDARD.encode(b)));
        assert_eq!(portrait_png(Some(&url(&png(128, 128)))), Some(png(128, 128)));
        assert!(portrait_png(Some(&url(&png(16, 16)))).is_none());
        assert!(portrait_png(Some(&url(&png(128, 64)))).is_none());
        assert!(portrait_png(Some(&json!("data:image/jpeg;base64,AAAA"))).is_none());
        assert!(portrait_png(Some(&json!("data:image/png;base64,!!!"))).is_none());
        assert!(portrait_png(None).is_none());
    }

    #[test]
    fn pair_reports_a_failed_save() {
        let (d, app) = app();
        app.save_config(&json!({"arenaUrl": "https://arena.example"})).unwrap();
        *app.arena.transport.write().unwrap() = Some(Arc::new(|_, _, _, _| (200, json!({"token": "T", "handle": "ash"}))));
        let (code, resp) = app.arena_pair("abcd", None);
        assert_eq!((code, resp["ok"].clone()), (200, json!(true)));
        assert_eq!(app.load_link()["handle"], "ash");
        let mut p = app.paths.clone();
        p.arena_link = d.path().join("missing-dir/arena-link.json");
        let app2 = App::new(p);
        *app2.arena.transport.write().unwrap() = Some(Arc::new(|_, _, _, _| (200, json!({"token": "T", "handle": "ash"}))));
        let (code, resp) = app2.arena_pair("abcd", None);
        assert_eq!(code, 500);
        assert!(resp["error"].as_str().unwrap().contains("could not save"));
    }

    const ACCT: &str = "0f1e2d3c-4b5a-6978-8796-a5b4c3d2e1f0";

    fn write_claude_json(app: &App) {
        let p = &app.paths.claude_json;
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, crate::pyjson::dumps(&json!({"oauthAccount": {
            "accountUuid": ACCT.to_uppercase(), "emailAddress": "ann@acme.example",
            "organizationUuid": "99999999-8888-7777-6666-555555555555", "organizationName": "Acme Corp",
            "displayName": "Ann", "fullName": "Ann Example"}}))).unwrap();
    }

    /// Like `stub`, but `/v1/me/account` answers `acct` and everything else 200.
    fn stub_split(app: &App, acct: (u16, Value)) -> Sent {
        app.save_link(&json!({"token": "T", "url": "https://arena.example", "handle": "ann"})).unwrap();
        let sent: Sent = Arc::new(Mutex::new(Vec::new()));
        let s2 = sent.clone();
        *app.arena.transport.write().unwrap() = Some(Arc::new(move |m, u, t, b| {
            s2.lock().unwrap().push((m.to_string(), u.to_string(), t.map(String::from), b.cloned()));
            if u.ends_with("/v1/me/account") { acct.clone() } else { (200, json!({"accepted": 1})) }
        }));
        sent
    }

    #[test]
    fn account_key_is_a_hash_of_the_account_uuid_only() {
        let (_d, app) = app();
        assert_eq!(app.account_key(), "");
        write_claude_json(&app);
        let k = app.account_key();
        use sha2::{Digest, Sha256};
        assert_eq!(k, hex::encode(Sha256::digest(format!("claude-hq:account:v1:{}", ACCT).as_bytes())));
        assert_eq!(k, app.account_key());
        std::fs::write(&app.paths.claude_json, r#"{"oauthAccount": {"accountUuid": "not-a-uuid"}}"#).unwrap();
        assert_eq!(app.account_key(), "");
        std::fs::write(&app.paths.claude_json, "{corrupt").unwrap();
        assert_eq!(app.account_key(), "");
        // Claude Code's file is never renamed or rewritten.
        assert_eq!(std::fs::read_to_string(&app.paths.claude_json).unwrap(), "{corrupt");
    }

    #[test]
    fn publish_sends_only_the_account_hash_and_only_when_opted_in() {
        let (_d, app) = app();
        write_claude_json(&app);
        let sent = stub_split(&app, (200, json!({"ok": true})));
        app.arena_publish();
        {
            let s = sent.lock().unwrap();
            assert_eq!(s.len(), 2);
            assert_eq!(s[1].1, "https://arena.example/v1/me/account");
            assert_eq!(s[1].3, Some(json!({"key": ""})));
        }
        assert!(app.arena_preview().1.get("accountKey").is_none());

        app.save_config(&json!({"arenaAccountBoard": true})).unwrap();
        sent.lock().unwrap().clear();
        app.arena_publish();
        let s = sent.lock().unwrap();
        assert_eq!(s[1].3, Some(json!({"key": app.account_key()})));
        let mut blob = String::new();
        for (_, _, _, b) in s.iter() {
            blob.push_str(&crate::pyjson::dumps(b.as_ref().unwrap_or(&Value::Null)));
        }
        blob.push_str(&crate::pyjson::dumps(&app.arena_preview().1));
        for secret in [ACCT, &ACCT.to_uppercase(), "ann@acme.example", "Acme Corp", "99999999-8888", "Ann Example"] {
            assert!(!blob.contains(secret), "{secret} left the machine");
        }
        assert_eq!(app.arena_preview().1["accountKey"], json!(app.account_key()));
        assert_eq!(app.arena_status()["accountBoard"], json!(true));
    }

    #[test]
    fn an_arena_without_the_account_route_still_publishes() {
        let (_d, app) = app();
        let _sent = stub_split(&app, (404, json!({"detail": "Not Found"})));
        assert_eq!(app.arena_publish().0, 200);
        assert_eq!(app.arena.last.lock().unwrap()["ok"], json!(true));
    }

    #[test]
    fn the_account_board_proxies_two_reads_only() {
        let (_d, app) = app();
        let sent = stub(&app, "https://arena.example", Arc::new(Mutex::new((200, json!({"accounts": []})))));
        assert_eq!(app.arena_accounts("board").0, 200);
        assert_eq!(app.arena_accounts("mine").0, 200);
        assert_eq!(app.arena_accounts("drop").0, 400);
        let s = sent.lock().unwrap();
        let got: Vec<(&str, &str)> = s.iter().map(|r| (r.0.as_str(), r.1.as_str())).collect();
        assert_eq!(got, [("GET", "https://arena.example/v1/accounts"), ("GET", "https://arena.example/v1/accounts/mine")]);
        drop(s);
        assert_eq!(app.arena_account_standings("7d").0, 200);
        assert_eq!(sent.lock().unwrap().last().unwrap().1, "https://arena.example/v1/accounts/standings?window=7d");
    }

    #[test]
    fn an_older_arena_gets_account_standings_built_locally() {
        let (_d, app) = app();
        app.save_link(&json!({"token": "T", "url": "https://arena.example", "handle": "bob"})).unwrap();
        let board = json!({"window": "7d", "startsOn": "2026-10-13", "endsOn": "2026-10-19", "seasonName": "October 2026", "entries": [
            {"handle": "ann", "displayName": "Ann", "trainerName": "", "avatarUrl": "a.png", "xp": 300, "level": 1, "rankTitle": "Prompt Apprentice",
             "prompts": 30, "tools": 0, "artifacts": 0, "activeDays": 3, "streak": 2, "tokensTotal": 10, "isYou": false},
            {"handle": "Bob", "displayName": "Bob", "trainerName": "Bo", "avatarUrl": "", "xp": 100, "level": 1, "rankTitle": "Prompt Apprentice",
             "prompts": 10, "tools": 0, "artifacts": 0, "activeDays": 5, "streak": 1, "tokensTotal": 5, "isYou": true},
            {"handle": "zed", "displayName": "Zed", "xp": 900, "isYou": false}]});
        let accounts = json!({"accounts": [{"rank": 1, "level": 9, "xp": 1, "memberCount": 3, "isMine": true,
            "members": [{"handle": "ann", "displayName": "Ann", "avatarUrl": "", "level": 1}, {"handle": "bob", "displayName": "Bob", "avatarUrl": "", "level": 1}]}]});
        let mine = json!({"accounts": [{"rank": 1, "memberCount": 3, "isMine": true, "members": [
            {"handle": "ann", "displayName": "Ann", "avatarUrl": "", "level": 1}, {"handle": "bob", "displayName": "Bob", "avatarUrl": "", "level": 1},
            {"handle": "cy", "displayName": "", "avatarUrl": "c.png", "level": 1}]}]});
        let routes: Vec<(&str, (u16, Value))> = vec![
            ("/v1/accounts/standings?window=7d", (404, json!({"detail": "Not Found"}))),
            ("/v1/board?window=7d", (200, board)),
            ("/v1/accounts/mine", (200, mine)),
            ("/v1/accounts", (200, accounts)),
        ];
        let sent: Sent = Arc::new(Mutex::new(Vec::new()));
        let s2 = sent.clone();
        *app.arena.transport.write().unwrap() = Some(Arc::new(move |m, u, t, b| {
            s2.lock().unwrap().push((m.to_string(), u.to_string(), t.map(String::from), b.cloned()));
            routes.iter().find(|(tail, _)| u.ends_with(tail)).map(|(_, r)| r.clone()).unwrap_or((500, json!({})))
        }));
        let (code, out) = app.arena_account_standings("7d");
        assert_eq!(code, 200);
        assert_eq!(out["seasonName"], json!("October 2026"));
        let a = &out["accounts"][0];
        assert_eq!((a["xp"].clone(), a["prompts"].clone(), a["activeDays"].clone(), a["memberCount"].clone()), (json!(400), json!(40), json!(5), json!(3)));
        assert_eq!((a["isMine"].clone(), a["partial"].clone()), (json!(true), json!(false)));
        let ms: Vec<(String, i64, bool)> = a["members"].as_array().unwrap().iter()
            .map(|m| (m["handle"].as_str().unwrap().to_string(), m["share"].as_i64().unwrap(), m["isYou"].as_bool().unwrap())).collect();
        assert_eq!(ms, [("ann".to_string(), 75, false), ("bob".to_string(), 25, true), ("cy".to_string(), 0, false)]);
        assert_eq!(a["members"][2]["avatarUrl"], json!("c.png"));
        assert_eq!(a["level"], json!(crate::creature::derive_level(400 / 3).0 + 2));
        // Only GETs, all to the paired Arena.
        assert!(sent.lock().unwrap().iter().all(|r| r.0 == "GET" && r.1.starts_with("https://arena.example/") && r.3.is_none()));
    }

    #[test]
    fn a_large_group_that_is_not_yours_is_marked_partial() {
        let board = json!({"entries": []});
        let accounts = json!({"accounts": [{"memberCount": 7, "isMine": false, "members": [{"handle": "a"}, {"handle": "b"}]}]});
        let out = account_standings_from(&board, &accounts, &json!({"accounts": []}));
        assert_eq!(out["accounts"][0]["partial"], json!(true));
        assert_eq!(out["accounts"][0]["xp"], json!(0));
    }
}
