//! The HTTP surface (dashboard.Handler): one fallback handler that walks the
//! same route list, in the same order, with the same gates -- Host check on
//! every request, CSRF token + same-site Origin on every POST, allowlisted
//! paths and ids, reads on GET and mutations on POST.

use crate::app::{is_uuid, App, APP_VERSION};
use crate::ext;
use crate::handlers::{self, ARENA_ROOM_POSTS};
use crate::py;
use crate::pyjson;
use axum::body::{Body, Bytes};
use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode};
use axum::response::Response;
use axum::Router;
use regex::Regex;
use serde_json::{json, Value};
use sha1::{Digest, Sha1};
use std::sync::{Arc, LazyLock};

const ICON_SVG: &str = include_str!("../assets/icon.svg");
const CSRF_PLACEHOLDER: &str = "__HQ_CSRF__";
pub const MAX_POST_BODY: usize = 1024 * 1024;
pub const MAX_SOUND_POST_BODY: usize = 8 * 1024 * 1024;
const STREAM_HEARTBEAT_SECS: f64 = 15.0;
const JSON: &str = "application/json; charset=utf-8";
const TEXT: &str = "text/plain; charset=utf-8";
const SW_JS: &str = concat!(
    "const C=\"claude-hq-v7\";",
    "self.addEventListener(\"install\",e=>self.skipWaiting());",
    "self.addEventListener(\"activate\",e=>e.waitUntil(self.clients.claim()));",
    "self.addEventListener(\"fetch\",e=>{",
    "const u=new URL(e.request.url);",
    "if(e.request.method!==\"GET\"||u.pathname.startsWith(\"/api/\"))return;",
    "e.respondWith(fetch(e.request).then(r=>{",
    "if(u.pathname===\"/\"){const c=r.clone();caches.open(C).then(x=>x.put(\"/\",c));}",
    "return r;}).catch(()=>caches.match(u.pathname===\"/\"?\"/\":e.request)));",
    "});"
);

pub fn post_body_limit(path: &str) -> usize {
    if path == "/api/arena/sounds" {
        MAX_SOUND_POST_BODY
    } else {
        MAX_POST_BODY
    }
}

/// Every state-changing route; anything else 404s locally.
pub fn post_paths() -> Vec<&'static str> {
    let mut v = vec![
        "/api/update", "/api/action", "/api/config", "/api/meta", "/api/evo-choice", "/api/arena/pair", "/api/arena/unpair", "/api/arena/publish",
        "/api/arena/ticket", "/api/arena/nudge", "/api/arena/pantry/claim", "/api/arena/pantry/buy", "/api/arena/pantry/eat",
        "/api/arena/pantry/give", "/api/arena/pantry/reward", "/api/arena/hq/me", "/api/arena/portrait", "/api/arena/cosmetics/buy",
        "/api/arena/cosmetics/equip", "/api/arena/market/sell", "/api/arena/crews/create", "/api/arena/crews/join",
        "/api/arena/crews/leave", "/api/arena/quickplay/join", "/api/arena/quickplay/leave", "/api/arena/cali/order",
        "/api/arena/sounds", "/api/games/state", "/api/vial/spend", "/api/notch/install",
    ];
    v.extend(ARENA_ROOM_POSTS);
    v.extend(ext::POST.iter().map(|(p, _)| *p));
    v
}

/// One response: status, body, headers (Content-Type first).
pub struct Reply {
    pub code: u16,
    pub body: Vec<u8>,
    pub headers: Vec<(&'static str, String)>,
}

impl Reply {
    pub fn new(code: u16, body: impl Into<Vec<u8>>, ctype: &str) -> Reply {
        Reply { code, body: body.into(), headers: vec![("Content-Type", ctype.to_string()), ("Cache-Control", "no-store".into())] }
    }

    pub fn json(code: u16, v: &Value) -> Reply {
        Reply::new(if code == 0 { 502 } else { code }, pyjson::dumps(v), JSON)
    }

    fn download(body: impl Into<Vec<u8>>, ctype: &str, filename: &str) -> Reply {
        let mut r = Reply::new(200, body, ctype);
        r.headers.push(("Content-Disposition", format!("attachment; filename=\"{}\"", filename)));
        r
    }
}

fn into_response(r: Reply, head: bool) -> Response {
    let mut b = Response::builder().status(StatusCode::from_u16(r.code).unwrap_or(StatusCode::BAD_GATEWAY));
    b = b.header("Server", format!("ClaudeHQ/{} Rust", APP_VERSION));
    for (k, v) in &r.headers {
        if let Ok(hv) = HeaderValue::from_str(v) {
            b = b.header(*k, hv);
        }
    }
    let body = if head { Body::empty() } else { Body::from(r.body) };
    b.body(body).unwrap()
}

fn host_ok(h: &HeaderMap) -> bool {
    let host = h.get("host").and_then(|v| v.to_str().ok()).unwrap_or("");
    let name = py::strip(host.split(':').next().unwrap_or("")).to_lowercase();
    matches!(name.as_str(), "127.0.0.1" | "localhost" | "")
}

/// Reject cross-site POSTs even if they carry a valid Host header.
fn origin_ok(h: &HeaderMap) -> bool {
    if let Some(origin) = h.get("origin").and_then(|v| v.to_str().ok()).filter(|o| !o.is_empty()) {
        let host = match origin.split_once("://") {
            Some((_, rest)) => {
                let netloc = rest.split(['/', '?', '#']).next().unwrap_or("");
                let hp = netloc.rsplit('@').next().unwrap_or("");
                let host = if hp.starts_with('[') { hp.split(']').next().unwrap_or("").trim_start_matches('[') } else { hp.split(':').next().unwrap_or("") };
                host.to_lowercase()
            }
            None => String::new(),
        };
        if host != "127.0.0.1" && host != "localhost" {
            return false;
        }
    }
    let sfs = h.get("sec-fetch-site").and_then(|v| v.to_str().ok()).map(|s| py::strip(s).to_lowercase()).unwrap_or_default();
    sfs != "cross-site"
}

pub fn router(app: Arc<App>) -> Router {
    Router::new().fallback(handle).with_state(app)
}

/// The raw request target, like BaseHTTPRequestHandler.path.
fn raw_target(req: &Request) -> String {
    let t = req.uri().path_and_query().map(|p| p.as_str().to_string()).unwrap_or_else(|| "/".into());
    if t.starts_with("//") {
        format!("/{}", t.trim_start_matches('/'))
    } else {
        t
    }
}

async fn handle(State(app): State<Arc<App>>, req: Request) -> Response {
    let method = req.method().clone();
    let headers = req.headers().clone();
    let target = raw_target(&req);
    let (path, query) = match target.split_once('?') {
        Some((p, q)) => (p.to_string(), q.to_string()),
        None => (target.clone(), String::new()),
    };
    if method == Method::HEAD {
        return head_reply(&headers, &path);
    }
    if method == Method::GET {
        if !host_ok(&headers) {
            return into_response(Reply::new(403, "Forbidden: local access only\n", TEXT), false);
        }
        if path == "/api/stream" {
            return stream(app);
        }
        let h = headers.clone();
        return match tokio::task::spawn_blocking(move || do_get(&app, &path, &query, &h)).await {
            Ok(r) => into_response(r, false),
            Err(e) => into_response(Reply::new(500, format!("internal error: {}\n", e), TEXT), false),
        };
    }
    if method == Method::POST {
        return do_post(app, headers, path, req.into_body()).await;
    }
    into_response(
        Reply::new(501, format!("Unsupported method ('{}')\n", method), TEXT),
        false,
    )
}

/// Header-only answers (so `curl -I` works for exports); never a body.
fn head_reply(headers: &HeaderMap, path: &str) -> Response {
    if !host_ok(headers) {
        let mut b = Response::builder().status(403).header("Content-Type", TEXT);
        b = b.header("Server", format!("ClaudeHQ/{} Rust", APP_VERSION));
        return b.body(Body::empty()).unwrap();
    }
    let mut r = Reply { code: 200, body: Vec::new(), headers: vec![("Cache-Control", "no-store".into())] };
    match path {
        "/api/export.csv" => {
            r.headers.push(("Content-Type", "text/csv; charset=utf-8".into()));
            r.headers.push(("Content-Disposition", "attachment; filename=\"claude-hq-sessions.csv\"".into()));
        }
        "/api/export.json" => {
            r.headers.push(("Content-Type", JSON.into()));
            r.headers.push(("Content-Disposition", "attachment; filename=\"claude-hq-export.json\"".into()));
        }
        "/" => r.headers.push(("Content-Type", "text/html; charset=utf-8".into())),
        _ => r.headers.push(("Content-Type", JSON.into())),
    }
    into_response(r, true)
}

/// The live payload as server-sent events: a frame when something the page
/// shows changed, else one per heartbeat; keepalives in between.
fn stream(app: Arc<App>) -> Response {
    struct St {
        app: Arc<App>,
        last_sig: Option<Arc<String>>,
        last_sent: Option<std::time::Instant>,
        first: bool,
    }
    let st = St { app, last_sig: None, last_sent: None, first: true };
    let s = futures::stream::unfold(st, |mut st| async move {
        if !st.first {
            tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
        }
        st.first = false;
        let app = st.app.clone();
        let (blob, sig) = tokio::task::spawn_blocking(move || app.build_payload_blob()).await.ok()?;
        let due = st.last_sent.map(|t| t.elapsed().as_secs_f64() >= STREAM_HEARTBEAT_SECS).unwrap_or(true);
        let frame = if st.last_sig.as_ref() != Some(&sig) || due {
            st.last_sig = Some(sig);
            st.last_sent = Some(std::time::Instant::now());
            format!("data: {}\n\n", blob)
        } else {
            ":keepalive\n\n".to_string()
        };
        Some((Ok::<Bytes, std::io::Error>(Bytes::from(frame)), st))
    });
    Response::builder()
        .status(200)
        .header("Server", format!("ClaudeHQ/{} Rust", APP_VERSION))
        .header("Content-Type", "text/event-stream; charset=utf-8")
        .header("Cache-Control", "no-store")
        .body(Body::from_stream(s))
        .unwrap()
}

// ---------------------------------------------------------------------------
// index.html assembly and static files
// ---------------------------------------------------------------------------

static INCLUDE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^[ \t]*(?://@include (ui/[\w./-]+)|/\*@include (ui/[\w./-]+)\*/)[ \t]*$").unwrap());

pub enum IndexErr {
    NotFound,
    Other(String),
}

impl App {
    /// The ui/ file an @include line names, or NotFound (no traversal).
    pub fn include_path(&self, rel: &str) -> Result<std::path::PathBuf, IndexErr> {
        let path = crate::state::realpath(&self.paths.here.join(rel));
        let root = crate::state::realpath(&self.paths.ui_dir);
        if rel.split('/').any(|p| p == "..") || !path.starts_with(&root) || path == root || !path.is_file() {
            return Err(IndexErr::NotFound);
        }
        Ok(path)
    }

    /// index.html with every @include line replaced by that ui/ file.
    pub fn assemble_index(&self) -> Result<Arc<String>, IndexErr> {
        let tpl = std::fs::read(&self.paths.index_html).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                IndexErr::NotFound
            } else {
                IndexErr::Other(e.to_string())
            }
        })?;
        let tpl = String::from_utf8(tpl).map_err(|e| IndexErr::Other(e.to_string()))?;
        let rels: Vec<String> = INCLUDE_RE
            .captures_iter(&tpl)
            .map(|c| c.get(1).or_else(|| c.get(2)).unwrap().as_str().to_string())
            .collect();
        let mut paths = Vec::new();
        for r in &rels {
            paths.push(self.include_path(r)?);
        }
        let sig: Vec<i128> = paths.iter().map(|p| crate::app::file_key(p).map(|k| k.0).unwrap_or(0)).collect();
        if let Some((s, t, html)) = self.index_memo.lock().unwrap().as_ref() {
            if *s == sig && *t == tpl {
                return Ok(html.clone());
            }
        }
        let mut parts = std::collections::HashMap::new();
        for (r, p) in rels.iter().zip(&paths) {
            let body = std::fs::read_to_string(p).map_err(|e| IndexErr::Other(e.to_string()))?;
            parts.insert(r.clone(), body);
        }
        let html = INCLUDE_RE
            .replace_all(&tpl, |c: &regex::Captures| {
                let body = &parts[c.get(1).or_else(|| c.get(2)).unwrap().as_str()];
                body.strip_suffix('\n').unwrap_or(body).to_string()
            })
            .into_owned();
        let html = Arc::new(html);
        *self.index_memo.lock().unwrap() = Some((sig, tpl, html.clone()));
        Ok(html)
    }
}

static GAME_FILE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[a-z][a-z0-9_-]{0,40}\.(js|css)$").unwrap());
static GAME_ASSET_RE: LazyLock<Regex> = LazyLock::new(|| {
    // Valley Rally (games/rally/) is all generated code: its physics, stage and scene scripts
    // are classic scripts in that folder, so it alone may serve .js beside its data.
    Regex::new(r"^(vendor/[a-z][a-z0-9-]{0,40}\.js|(golf|kart|platformer|fps|strike)/[a-z][a-z0-9-]{0,40}\.(glb|json|png)|rally/[a-z][a-z0-9-]{0,40}\.(js|json|png))$").unwrap()
});

fn game_type(ext: &str) -> &'static str {
    match ext {
        "js" => "application/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "glb" => "model/gltf-binary",
        "json" => "application/json; charset=utf-8",
        _ => "image/png",
    }
}

pub fn game_cache_control(name: &str) -> &'static str {
    if GAME_ASSET_RE.is_match(name) {
        "no-cache"
    } else {
        "no-store"
    }
}

pub fn game_etag(body: &[u8]) -> String {
    format!("\"{}\"", &hex::encode(Sha1::digest(body))[..20])
}

impl App {
    /// (bytes, content type) for an allowlisted file under games/, else None.
    pub fn game_file(&self, name: &str) -> Option<(Vec<u8>, &'static str)> {
        let root = crate::state::realpath(&self.paths.games_dir);
        let folder = if GAME_FILE_RE.is_match(name) {
            root.clone()
        } else if GAME_ASSET_RE.is_match(name) {
            root.join(name.split('/').next().unwrap())
        } else {
            return None;
        };
        let path = crate::state::realpath(&root.join(name));
        if path.parent() != Some(folder.as_path()) || !path.is_file() {
            return None;
        }
        let body = std::fs::read(&path).ok()?;
        Some((body, game_type(name.rsplit('.').next().unwrap())))
    }
}

// ---------------------------------------------------------------------------
// GET
// ---------------------------------------------------------------------------

fn code_or(code: u16, dflt: u16) -> u16 {
    if code == 0 {
        dflt
    } else {
        code
    }
}

fn do_get(app: &App, path: &str, query: &str, headers: &HeaderMap) -> Reply {
    let arg = |k: &str| py::strip(&py::qs_first(query, k).unwrap_or_default()).to_string();
    for (p, f) in ext::GET {
        if *p == path {
            let (code, resp) = f(app, &arg);
            return Reply::json(code_or(code, 502), &resp);
        }
    }
    match path {
        "/" => {
            return match app.assemble_index() {
                Ok(html) => Reply::new(200, html.replace(CSRF_PLACEHOLDER, &app.csrf_token), "text/html; charset=utf-8"),
                Err(IndexErr::NotFound) => Reply::new(
                    200,
                    "<!doctype html><meta charset=utf-8><title>Claude Dashboard</title><body style='font-family:system-ui;padding:2rem'><h1>Claude Dashboard</h1><p>index.html not found next to dashboard.py. API is live at <a href='/api/sessions'>/api/sessions</a>.</p>",
                    "text/html; charset=utf-8",
                ),
                Err(IndexErr::Other(e)) => Reply::new(500, format!("error reading index.html: {}\n", e), TEXT),
            };
        }
        "/manifest.webmanifest" => {
            let m = json!({
                "name": "Claude HQ", "short_name": "Claude HQ", "start_url": "/", "scope": "/", "display": "standalone",
                "background_color": "#0c0d10", "theme_color": "#0c0d10",
                "description": "Local dashboard for your Claude Code sessions.",
                "icons": [{"src": "/icon.svg", "sizes": "any", "type": "image/svg+xml", "purpose": "any maskable"}],
            });
            return Reply::new(200, pyjson::dumps(&m), "application/manifest+json; charset=utf-8");
        }
        "/icon.svg" => return Reply::new(200, ICON_SVG, "image/svg+xml; charset=utf-8"),
        "/sw.js" => return Reply::new(200, SW_JS, "application/javascript; charset=utf-8"),
        _ => {}
    }
    if let Some(name) = path.strip_prefix("/games/") {
        return match app.game_file(name) {
            None => Reply::new(404, "not found\n", TEXT),
            Some((body, ctype)) => {
                let cache = game_cache_control(name);
                let tag = (cache != "no-store").then(|| game_etag(&body));
                let inm = headers.get("if-none-match").and_then(|v| v.to_str().ok()).unwrap_or("");
                let (code, body) = match &tag {
                    Some(t) if inm == t => (304, Vec::new()),
                    _ => (200, body),
                };
                let mut r = Reply { code, body, headers: vec![("Content-Type", ctype.into()), ("Cache-Control", cache.into())] };
                if let Some(t) = tag {
                    r.headers.push(("ETag", t));
                }
                r
            }
        };
    }
    match path {
        "/api/games/state" => return Reply::json(200, &json!({"state": app.load_games_save()})),
        "/api/sessions" => return Reply::new(200, pyjson::dumps(&app.build_payload_memo()), JSON),
        "/api/arena/status" => return Reply::json(200, &app.arena_status()),
        "/api/arena/sounds" => {
            let local = app.local_sounds();
            if !local.is_empty() {
                return Reply::json(200, &json!({"sounds": local, "source": "local"}));
            }
            let (code, resp) = app.arena_list_sounds();
            return Reply::json(code_or(code, 502), &resp);
        }
        _ => {}
    }
    if let Some(raw) = path.strip_prefix("/api/arena/sounds/") {
        let fn_ = py::unquote(raw);
        let ext = handlers::splitext(&fn_).1.to_lowercase();
        if fn_.is_empty() || fn_.contains('/') || fn_.contains('\\') || fn_.contains("..") || fn_.starts_with('.') || handlers::sound_type(&ext).is_none() {
            return Reply::json(404, &json!({"error": "no such sound"}));
        }
        let ctype = handlers::sound_type(&ext).unwrap();
        if let Some(lp) = app.local_sound_file(&fn_) {
            return match std::fs::read(&lp) {
                Ok(data) => Reply::new(200, data, ctype),
                Err(e) => Reply::json(500, &json!({"error": e.to_string()})),
            };
        }
        let (code, ct, data) = app.arena_get_sound(&fn_);
        if code != 200 || data.is_empty() {
            return Reply::json(code_or(code, 502), &json!({"error": "no such sound"}));
        }
        return Reply::new(200, data, if ct.is_empty() { ctype } else { &ct });
    }
    let qs = |k: &str| py::qs_first(query, k);
    match path {
        "/api/arena/preview" => {
            let (code, resp) = app.arena_preview();
            return Reply::json(code_or(code, 200), &resp);
        }
        "/api/arena/rooms" => {
            let (c, r) = app.arena_rooms_directory();
            return Reply::json(c, &r);
        }
        "/api/arena/rooms/members" => {
            let rid = qs("roomId").unwrap_or_default();
            if !crate::arena::ROOM_ID_RE.is_match(&rid) {
                return Reply::json(400, &json!({"error": "roomId required"}));
            }
            let (c, r) = app.arena_room_members(&rid);
            return Reply::json(c, &r);
        }
        "/api/arena/board" => {
            let w = qs("window").unwrap_or_else(|| "season".into());
            let w = if ["season", "30d", "7d", "all"].contains(&w.as_str()) { w } else { "season".into() };
            let (c, r) = app.arena_board(&w);
            return Reply::json(c, &r);
        }
        "/api/arena/pantry" => {
            let (c, r) = app.arena_pantry(None, None);
            return Reply::json(c, &if c == 200 { handlers::overlay_food_effects(r) } else { r });
        }
        "/api/arena/quickplay/status" => {
            let (c, r) = app.arena_quickplay("status", &json!({}));
            return Reply::json(c, &r);
        }
        "/api/arena/accounts/standings" => {
            let w = qs("window").unwrap_or_else(|| "season".into());
            let w = if ["season", "30d", "7d", "all"].contains(&w.as_str()) { w } else { "season".into() };
            let (c, r) = app.arena_account_standings(&w);
            return Reply::json(c, &r);
        }
        "/api/arena/accounts" | "/api/arena/accounts/mine" => {
            let (c, r) = app.arena_accounts(if path.ends_with("/mine") { "mine" } else { "board" });
            return Reply::json(c, &r);
        }
        "/api/arena/crews" | "/api/arena/crews/mine" => {
            let (c, r) = app.arena_crews(if path.ends_with("/mine") { "mine" } else { "board" }, &json!({}));
            return Reply::json(c, &r);
        }
        "/api/arena/cosmetics" => {
            let (c, r) = app.arena_cosmetics(None, &json!({}));
            return Reply::json(c, &r);
        }
        "/api/server-stats" => return Reply::json(200, &app.server_stats()),
        "/api/arena/server-stats" => {
            let (c, r) = app.arena_server_stats();
            return Reply::json(c, &r);
        }
        "/api/arena/progress" => {
            let (c, r) = app.arena_progress();
            return Reply::json(c, &r);
        }
        "/api/arena/leaderboards" => {
            let key = arg("key");
            let (c, r) = app.arena_leaderboards(&arg("game"), Some(key.as_str()).filter(|k| !k.is_empty()));
            return Reply::json(c, &r);
        }
        "/api/arena/profile" => {
            let u = Some(arg("u")).filter(|s| !s.is_empty()).unwrap_or_else(|| "me".into());
            let (c, r) = app.arena_profile(&u);
            return Reply::json(c, &r);
        }
        _ => {}
    }
    if let Some(vid) = path.strip_prefix("/api/music/audio/") {
        return audio_reply(app, vid, headers);
    }
    match path {
        "/api/music/now" | "/api/music/search" | "/api/music/oembed" | "/api/music/audio-state" | "/api/arena/music/now" => {
            let (c, r) = music_get(app, path, &arg);
            return Reply::json(c, &r);
        }
        "/api/arena/hq/me" => {
            let (c, r) = app.arena_hq_me();
            return Reply::json(c, &r);
        }
        "/api/arena/hq/open" => {
            let (c, r) = app.arena_hq_open();
            return Reply::json(c, &r);
        }
        "/api/arena/hq/visit" => {
            let (c, r) = app.arena_hq_visit(&arg("u"));
            return Reply::json(c, &r);
        }
        "/api/arena/cali/board" => {
            let (c, r) = app.arena_cali_board(&qs("window").unwrap_or_else(|| "season".into()));
            return Reply::json(c, &r);
        }
        "/api/arena/cali/orders" => {
            let (c, r) = app.arena_cali_orders();
            return Reply::json(c, &r);
        }
        "/api/update" => return Reply::json(200, &app.update_status(query.contains("force=1"))),
        "/api/config" => return Reply::json(200, &Value::Object(app.load_config())),
        "/api/meta" => return Reply::json(200, &Value::Object(app.load_meta())),
        _ => {}
    }
    if let Some(sid) = path.strip_prefix("/api/transcript/") {
        return transcript_page(app, sid, query);
    }
    if let Some(sid) = path.strip_prefix("/api/session/").and_then(|s| s.strip_suffix("/export.md")) {
        if !is_uuid(sid) {
            return Reply::json(404, &json!({"error": "unknown session"}));
        }
        let Some(tp) = app.find_transcript(sid) else { return Reply::json(404, &json!({"error": "unknown session"})) };
        let md = app.build_session_markdown(sid, &tp);
        let short = py::take_chars(sid, 8);
        return Reply::download(md, "text/markdown; charset=utf-8", &format!("claude-hq-session-{}.md", if short.is_empty() { "session".into() } else { short }));
    }
    if let Some(sid) = path.strip_prefix("/api/session/") {
        if !is_uuid(sid) {
            return Reply::json(404, &json!({"error": "unknown session"}));
        }
        return match app.build_session_detail(sid) {
            Some(d) => Reply::json(200, &d),
            None => Reply::json(404, &json!({"error": "unknown session"})),
        };
    }
    match path {
        "/api/search" => {
            let raw = qs("q").unwrap_or_default();
            let q = py::strip(&raw).to_string();
            if q.is_empty() {
                return Reply::json(200, &json!({"query": raw, "count": 0, "results": []}));
            }
            let results = app.search_transcripts(&q, 40);
            Reply::json(200, &json!({"query": q, "count": results.len(), "results": results}))
        }
        "/api/project" => {
            let slug = py::strip(&qs("folder").unwrap_or_default()).to_string();
            match app.compute_project(&slug) {
                Some(d) => Reply::json(200, &d),
                None => Reply::json(404, &json!({"error": "unknown folder"})),
            }
        }
        "/api/history" => Reply::json(200, &app.compute_history()),
        "/api/insights" => Reply::json(200, &app.compute_insights()),
        "/api/pokedex" => Reply::json(200, &app.compute_pokedex()),
        "/api/export.json" => {
            let p = app.build_payload_memo();
            let sessions = p["sessions"].as_array().cloned().unwrap_or_default();
            let body = json!({"generated": py::iso(&py::now_utc()), "season": p.get("season").cloned().unwrap_or(Value::Null),
                              "sessions": app.full_text_sessions(&sessions)});
            Reply::download(pyjson::dumps(&body), JSON, "claude-hq-export.json")
        }
        "/api/export.csv" => {
            Reply::download(crate::analytics::build_export_csv(&app.build_payload_memo()), "text/csv; charset=utf-8", "claude-hq-sessions.csv")
        }
        "/api/digest" => {
            static DATE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\d{4}-\d{2}-\d{2}$").unwrap());
            let mut diso = py::strip(&qs("date").unwrap_or_default()).to_string();
            if !DATE_RE.is_match(&diso) {
                diso = py::date_iso(&py::local_today());
            }
            let days = qs("days").map(|d| py::py_int(&Value::from(d)).unwrap_or(1)).unwrap_or(1).clamp(1, 31);
            let data = app.compute_digest(&diso, days);
            if matches!(qs("download").as_deref(), Some("1" | "true" | "yes")) {
                return Reply::download(data["markdown"].as_str().unwrap_or("").to_string(), "text/markdown; charset=utf-8", &format!("claude-hq-digest-{}.md", diso));
            }
            Reply::json(200, &data)
        }
        _ => Reply::json(404, &json!({"error": "not found"})),
    }
}

fn transcript_page(app: &App, sid: &str, query: &str) -> Reply {
    if !is_uuid(sid) {
        return Reply::json(404, &json!({"error": "unknown session"}));
    }
    let Some(tp) = app.find_transcript(sid) else { return Reply::json(404, &json!({"error": "unknown session"})) };
    let qs = |k: &str| py::qs_first(query, k);
    let offset = qs("offset").map(|v| py::py_int(&Value::from(v)).unwrap_or(0)).unwrap_or(0).max(0) as usize;
    let limit = qs("limit").map(|v| py::py_int(&Value::from(v)).unwrap_or(60)).unwrap_or(60).clamp(1, 200) as usize;
    let q = py::strip(&qs("q").unwrap_or_default()).to_lowercase();
    let mut events: Vec<Value> = app.get_transcript_events(&tp).as_ref().clone();
    let agg = app.scan_file(&tp);
    let title = app.session_heading(agg.as_deref(), sid);
    let mut matched = Value::Null;
    if !q.is_empty() {
        events.retain(|e| {
            e["text"].as_str().unwrap_or("").to_lowercase().contains(&q) || e["tool"].as_str().unwrap_or("").to_lowercase().contains(&q)
        });
        matched = json!(events.len());
    }
    let page: Vec<Value> = events.iter().skip(offset).take(limit).cloned().collect();
    Reply::json(200, &json!({
        "sessionId": sid, "title": title, "total": events.len(), "matched": matched, "query": q,
        "offset": offset, "limit": limit, "events": page,
        "source": agg.map(|a| a.source()).unwrap_or("claude"),
    }))
}

/// A fetched room song, with Range so the page can seek in it.
fn audio_reply(app: &App, vid: &str, headers: &HeaderMap) -> Reply {
    static RANGE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^bytes=(\d*)-(\d*)$").unwrap());
    let Some((fpath, ctype)) = app.music.audio_file(&app.paths.music_cache, vid) else {
        return Reply::json(404, &json!({"error": "not fetched"}));
    };
    let size = std::fs::metadata(&fpath).map(|m| m.len()).unwrap_or(0);
    let (mut start, mut end) = (0u64, size.saturating_sub(1));
    let range = py::strip(headers.get("range").and_then(|v| v.to_str().ok()).unwrap_or("")).to_string();
    let m = RANGE_RE.captures(&range);
    let partial = m.as_ref().map(|c| !c[1].is_empty() || !c[2].is_empty()).unwrap_or(false);
    if partial {
        let c = m.unwrap();
        if !c[1].is_empty() {
            start = c[1].parse().unwrap_or(0);
            end = if !c[2].is_empty() { c[2].parse::<u64>().unwrap_or(u64::MAX).min(size.saturating_sub(1)) } else { size.saturating_sub(1) };
        } else {
            start = size.saturating_sub(c[2].parse().unwrap_or(0));
        }
        if start > end || start >= size {
            return Reply { code: 416, body: Vec::new(), headers: vec![("Content-Range", format!("bytes */{}", size))] };
        }
    }
    let data: Vec<u8> = std::fs::read(&fpath).unwrap_or_default();
    let lo = (start as usize).min(data.len());
    let hi = ((end + 1) as usize).min(data.len()).max(lo);
    let mut r = Reply {
        code: if partial { 206 } else { 200 },
        body: data[lo..hi].to_vec(),
        headers: vec![("Content-Type", ctype.into()), ("Accept-Ranges", "bytes".into())],
    };
    if partial {
        r.headers.push(("Content-Range", format!("bytes {}-{}/{}", start, end, size)));
    }
    r.headers.push(("Cache-Control", "no-store".into()));
    r
}

fn music_get(app: &App, path: &str, arg: &dyn Fn(&str) -> String) -> (u16, Value) {
    match path {
        "/api/music/now" => {
            let (t, age) = app.music.current(1.5);
            let share = app.music.share.lock().unwrap();
            (200, json!({
                "track": t, "ageMs": (age * 1000.0) as i64,
                "share": py::opt_truthy(app.load_config().get("musicShare")),
                "paired": app.load_link().get("token").map(py::truthy).unwrap_or(false),
                "shared": share.as_ref().map(|s| s.sent.is_some()).unwrap_or(false),
                "shareError": share.as_ref().and_then(|s| s.last_error),
                "platform": if cfg!(target_os = "macos") { "darwin" } else { std::env::consts::OS },
            }))
        }
        "/api/music/search" => {
            let q = arg("q");
            if let Some(vid) = crate::music::youtube_id_from_url(&q) {
                let one = app.music.oembed(&vid).unwrap_or_else(|| json!({"v": vid, "title": "", "author": ""}));
                if one.get("embeddable") == Some(&Value::Bool(false)) {
                    return (200, json!({"results": [], "error": "That video's owner only lets it play on YouTube itself. Try another upload of the song."}));
                }
                return (200, json!({"results": [one]}));
            }
            (200, json!({"results": app.music.search(&q, 6, arg("quick") != "1")}))
        }
        "/api/music/audio-state" => {
            let cookies = app.load_config().get("musicCookies").and_then(|v| v.as_str()).map(String::from);
            let mut st = app.music.prepare(&app.paths.music_cache, &arg("v"), cookies);
            st.insert("ffmpeg".into(), json!(crate::music::tool("ffmpeg").is_some()));
            (200, Value::Object(st))
        }
        "/api/music/oembed" => match app.music.oembed(&arg("v")) {
            Some(one) => (200, one),
            None => (404, json!({"error": "not found"})),
        },
        _ => {
            let (c, r) = app.arena_music_now();
            (code_or(c, 502), r)
        }
    }
}

// ---------------------------------------------------------------------------
// POST
// ---------------------------------------------------------------------------

async fn do_post(app: Arc<App>, headers: HeaderMap, path: String, body: Body) -> Response {
    let reply = |r: Reply| into_response(r, false);
    if !host_ok(&headers) {
        return reply(Reply::json(403, &json!({"error": "local access only"})));
    }
    let token = headers.get("x-hq-token").and_then(|v| v.to_str().ok()).unwrap_or("");
    if token.is_empty() || !constant_eq(token.as_bytes(), app.csrf_token.as_bytes()) {
        return reply(Reply::json(403, &json!({"error": "bad or missing CSRF token"})));
    }
    if !origin_ok(&headers) {
        return reply(Reply::json(403, &json!({"error": "cross-site request rejected"})));
    }
    if !post_paths().contains(&path.as_str()) {
        return reply(Reply::json(404, &json!({"error": "not found"})));
    }
    let cl = headers.get("content-length").and_then(|v| v.to_str().ok()).map(|s| py::strip(s).to_string()).unwrap_or_default();
    if cl.is_empty() || !cl.bytes().all(|b| b.is_ascii_digit()) {
        let mut r = Reply::json(if cl.is_empty() { 411 } else { 400 }, &json!({"error": "a valid Content-Length is required"}));
        r.headers.push(("Connection", "close".into()));
        return reply(r);
    }
    let length: usize = cl.parse().unwrap_or(usize::MAX);
    if length > post_body_limit(&path) {
        let mut r = Reply::json(413, &json!({"error": "request body too large"}));
        r.headers.push(("Connection", "close".into()));
        return reply(r);
    }
    let raw = match axum::body::to_bytes(body, length).await {
        Ok(b) => b,
        Err(e) => return reply(Reply::json(400, &json!({"error": format!("bad JSON body: {}", e)}))),
    };
    let parsed: Result<Value, String> = if raw.is_empty() {
        Ok(json!({}))
    } else {
        std::str::from_utf8(&raw).map_err(|e| e.to_string()).and_then(|s| serde_json::from_str::<Value>(s).map_err(|e| e.to_string()))
    };
    let body = match parsed {
        Ok(v @ Value::Object(_)) => v,
        Ok(_) => return reply(Reply::json(400, &json!({"error": "bad JSON body: body must be an object"}))),
        Err(e) => return reply(Reply::json(400, &json!({"error": format!("bad JSON body: {}", e)}))),
    };
    match tokio::task::spawn_blocking(move || post_route(&app, &path, &body)).await {
        Ok(r) => reply(r),
        Err(e) => reply(Reply::json(500, &json!({"error": format!("internal error: {}", e)}))),
    }
}

fn constant_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn post_route(app: &Arc<App>, path: &str, body: &Value) -> Reply {
    let b = body.as_object().unwrap();
    if path.starts_with("/api/arena/") {
        let (code, resp) = arena_post(app, path, body);
        return Reply::json(code, &resp);
    }
    match path {
        "/api/update" => {
            let (c, r) = app.update_and_restart();
            Reply::json(c, &r)
        }
        "/api/config" => match app.save_config(body) {
            Ok(cfg) => Reply::json(200, &Value::Object(cfg)),
            Err(e) => Reply::json(500, &json!({"error": format!("could not save config: {}", e)})),
        },
        "/api/games/state" => match app.save_games_save(b) {
            Ok(v) => Reply::json(200, &v),
            Err(crate::app::SaveError::Bad(e)) => Reply::json(400, &json!({"error": format!("bad save: {}", e)})),
            Err(crate::app::SaveError::Io(e)) => Reply::json(500, &json!({"error": format!("could not write save: {}", e)})),
        },
        "/api/notch/install" => {
            // HQ Notch from HQ (ext_notch.py): the same install as `python3 notch/tools/install.py`
            let (c, r) = app.notch_install();
            Reply::json(c, &r)
        }
        "/api/vial/spend" => {
            let sid = b.get("sessionId").unwrap_or(&Value::Null);
            let stg = b.get("fromStage").unwrap_or(&Value::Null);
            if !matches!(sid, Value::String(s) if is_uuid(s)) {
                return Reply::json(400, &json!({"error": "invalid sessionId"}));
            }
            if !matches!(py::strict_int(stg), Some(0..=3)) {
                return Reply::json(400, &json!({"error": "invalid fromStage"}));
            }
            Reply::json(200, &app.vial_spend(sid, stg, py::now_ts()))
        }
        "/api/evo-choice" => match app.save_evo_choice(b.get("sessionId"), b) {
            Ok(entry) => {
                app.invalidate_payload_memo(); // the next /api/sessions carries it
                Reply::json(200, &json!({"sessionId": b.get("sessionId"), "evo": entry}))
            }
            Err(crate::evo::EvoError::Bad(e)) => Reply::json(400, &json!({"error": e})),
            Err(crate::evo::EvoError::Io(e)) => Reply::json(500, &json!({"error": format!("could not save the pick: {}", e)})),
        },
        "/api/meta" => {
            let sid = b.get("sessionId").and_then(|v| v.as_str()).filter(|s| crate::app::is_uuid_match(s));
            let Some(sid) = sid else { return Reply::json(400, &json!({"error": "invalid sessionId"})) };
            match app.save_meta(sid, b) {
                Ok(e) => Reply::json(200, &e),
                Err(e) => Reply::json(500, &json!({"error": format!("could not save meta: {}", e)})),
            }
        }
        _ => {
            let (c, r) = match b.get("action").and_then(|v| v.as_str()) {
                Some("resume") => app.action_resume(b.get("sessionId")),
                Some("reveal") => app.action_reveal(b.get("sessionId")),
                Some("close") => app.action_close(b.get("pid")),
                _ => (400, json!({"error": "unknown action"})),
            };
            Reply::json(c, &r)
        }
    }
}

/// Arena actions. The device token never crosses back to the page.
fn arena_post(app: &App, path: &str, body: &Value) -> (u16, Value) {
    let b = body.as_object().unwrap();
    for (p, f) in ext::POST {
        if *p == path {
            let (c, r) = f(app, body);
            return (code_or(c, 502), r);
        }
    }
    if ARENA_ROOM_POSTS.contains(&path) {
        return handlers::room_post(app, path, b);
    }
    let r = match path {
        "/api/arena/pair" => match b.get("code").and_then(|v| v.as_str()).filter(|c| !py::strip(c).is_empty()) {
            Some(code) => app.arena_pair(code, b.get("label")),
            None => (400, json!({"error": "pairing code required"})),
        },
        "/api/arena/unpair" => {
            app.clear_link();
            (200, json!({"ok": true}))
        }
        "/api/arena/publish" => app.arena_publish(),
        "/api/arena/ticket" => app.arena_ws_ticket(),
        "/api/arena/nudge" => match b.get("toHandle").and_then(|v| v.as_str()).filter(|t| !py::strip(t).is_empty()) {
            Some(to) => app.arena_send_nudge(py::strip(to), b.get("note")),
            None => (400, json!({"error": "toHandle required"})),
        },
        "/api/arena/quickplay/join" | "/api/arena/quickplay/leave" => app.arena_quickplay(path.rsplit('/').next().unwrap(), body),
        "/api/arena/crews/create" | "/api/arena/crews/join" | "/api/arena/crews/leave" => app.arena_crews(path.rsplit('/').next().unwrap(), body),
        "/api/arena/cosmetics/buy" | "/api/arena/cosmetics/equip" => app.arena_cosmetics(path.rsplit('/').next(), body),
        "/api/arena/market/sell" => app.arena_market_sell(b.get("cat"), b.get("qty")),
        "/api/arena/portrait" => {
            if b.get("remove") == Some(&Value::Bool(true)) {
                app.arena_portrait_delete()
            } else {
                match crate::arena::portrait_png(b.get("png")) {
                    Some(png) => app.arena_portrait_put(&png),
                    None => (400, json!({"error": "png must be a data:image/png;base64 URL of a small square PNG"})),
                }
            }
        }
        "/api/arena/hq/me" => {
            let crew = py::opt_truthy(b.get("crew")).then(|| app.hq_crew_counts());
            app.arena_hq_update(b.get("open"), b.get("look"), crew.as_ref())
        }
        "/api/arena/cali/order" => match handlers::cali_body(body) {
            Ok(clean) => {
                let (c, r) = app.arena_cali_log_order(&clean);
                (code_or(c, 502), r)
            }
            Err(e) => (400, json!({"error": e})),
        },
        "/api/arena/sounds" => app.receive_sound_upload(b),
        _ => {
            if let Some(action) = path.strip_prefix("/api/arena/pantry/") {
                if action == "reward" {
                    return handlers::quest_reward(app, body);
                }
                if action == "eat" {
                    return app.pantry_eat(body);
                }
                return match handlers::pantry_body(action, body) {
                    Ok(clean) => {
                        let (c, r) = app.arena_pantry(Some(action), Some(&clean));
                        (code_or(c, 502), if c == 200 { handlers::overlay_food_effects(r) } else { r })
                    }
                    Err(e) => (400, json!({"error": e})),
                };
            }
            (404, json!({"error": "not found"}))
        }
    };
    r
}
