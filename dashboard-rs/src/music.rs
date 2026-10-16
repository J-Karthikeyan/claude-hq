//! Now Playing (what this Mac plays, asked of the music apps through
//! AppleScript), the share loop to the paired Arena, YouTube lookups for the
//! listening rooms, and the local yt-dlp audio cache. Nothing here reads a
//! transcript (port of music.py).

use crate::payload::{run_timeout, RunErr};
use crate::py;
use regex::Regex;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, LazyLock, Mutex};

pub const TRACK_KEYS: [&str; 9] = ["title", "artist", "album", "source", "spotifyId", "youtubeId", "durationMs", "positionMs", "playing"];
pub const SOURCES: [&str; 3] = ["spotify", "apple", "ytmusic"];
pub static YT_ID_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z0-9_-]{11}$").unwrap());
static SPOTIFY_ID_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z0-9]{22}$").unwrap());
const TEXT_MAX: usize = 150;
const DUR_MAX_MS: f64 = 86400000.0;
const UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126 Safari/537.36";

const SPOTIFY: &str = r#"
tell application id "com.spotify.client"
  set ps to player state as string
  if ps is "stopped" then return "stopped"
  set t to current track
  return ps & tab & (name of t) & tab & (artist of t) & tab & (album of t) & tab & (id of t) & tab & ((duration of t) as string) & tab & ((player position) as string)
end tell
"#;

const APPLE: &str = r#"
tell application id "com.apple.Music"
  set ps to player state as string
  if ps is "stopped" then return "stopped"
  try
    set t to current track
    return ps & tab & (name of t) & tab & (artist of t) & tab & (album of t) & tab & "" & tab & (((duration of t) * 1000) as integer as string) & tab & ((player position) as string)
  on error
    return "stopped"
  end try
end tell
"#;

const CHROMIUM: &str = r#"
tell application "%s"
  repeat with w in windows
    repeat with t in tabs of w
      set u to URL of t
      if u starts with "https://music.youtube.com/" then return (title of t) & tab & u
    end repeat
  end repeat
end tell
return ""
"#;

const SAFARI: &str = r#"
tell application "Safari"
  repeat with w in windows
    repeat with t in tabs of w
      set u to URL of t
      if u starts with "https://music.youtube.com/" then return (name of t) & tab & u
    end repeat
  end repeat
end tell
return ""
"#;

const CHROMIUM_APPS: [(&str, &str); 6] = [
    ("Google Chrome", "Google Chrome"),
    ("Arc", "Arc"),
    ("Brave Browser", "Brave Browser"),
    ("Microsoft Edge", "Microsoft Edge"),
    ("Vivaldi", "Vivaldi"),
    ("Chromium", "Chromium"),
];

/// Browsers whose YouTube login yt-dlp may borrow (opt-in, config musicCookies).
pub const COOKIE_BROWSERS: [&str; 9] = ["operagx", "opera", "chrome", "brave", "edge", "arc", "vivaldi", "firefox", "safari"];

pub fn cookie_arg(name: &str) -> Option<String> {
    let home = std::env::var("HOME").unwrap_or_default();
    Some(match name {
        "operagx" => format!("opera:{}/Library/Application Support/com.operasoftware.OperaGX/Default", home),
        "arc" => format!("chrome:{}/Library/Application Support/Arc/User Data/Default", home),
        "opera" | "chrome" | "brave" | "edge" | "vivaldi" | "firefox" | "safari" => name.to_string(),
        _ => return None,
    })
}

pub const AUDIO_EXTS: [(&str, &str); 5] =
    [("m4a", "audio/mp4"), ("mp4", "audio/mp4"), ("webm", "audio/webm"), ("opus", "audio/ogg"), ("mp3", "audio/mpeg")];

fn running(proc: &str) -> bool {
    matches!(run_timeout(Command::new("pgrep").args(["-xq", proc]), 2), Ok((0, _, _)))
}

fn osa(script: &str) -> String {
    match run_timeout(Command::new("osascript").args(["-e", script]), 4) {
        Ok((0, out, _)) => py::strip(&out).to_string(),
        _ => String::new(),
    }
}

/// Printable characters only, whitespace folded, clipped.
pub fn clean_text(s: &str, cap: usize) -> String {
    let s: String = s.chars().map(|c| if py::is_printable(c) { c } else { ' ' }).collect();
    py::take_chars(&py::collapse_ws(&s), cap)
}

fn num(s: &str) -> Option<f64> {
    py::py_float(&Value::from(py::strip(s).replace(',', ".")))
}

fn obj(v: Value) -> Map<String, Value> {
    match v {
        Value::Object(o) => o,
        _ => Map::new(),
    }
}

/// Parse the tab-separated answer of the Spotify / Music scripts.
pub fn parse_player(out: &str, source: &str) -> Option<Map<String, Value>> {
    if out.is_empty() || out == "stopped" {
        return None;
    }
    let parts: Vec<&str> = out.split('\t').collect();
    if parts.len() < 7 {
        return None;
    }
    let (state, name, artist, album, tid, dur, pos) = (parts[0], parts[1], parts[2], parts[3], parts[4], parts[5], parts[6]);
    let title = clean_text(name, TEXT_MAX);
    if title.is_empty() {
        return None;
    }
    let mut t = obj(json!({"title": title, "artist": clean_text(artist, TEXT_MAX), "album": clean_text(album, TEXT_MAX),
                           "source": source, "playing": py::strip(state) == "playing"}));
    if let Some(d) = num(dur) {
        if d > 0.0 && d <= DUR_MAX_MS {
            t.insert("durationMs".into(), Value::from(d as i64));
        }
    }
    if let Some(p) = num(pos) {
        if p >= 0.0 {
            let ms = (p * 1000.0) as i64;
            let cap = t.get("durationMs").and_then(|v| v.as_i64()).unwrap_or(DUR_MAX_MS as i64);
            t.insert("positionMs".into(), Value::from(ms.min(cap)));
        }
    }
    if source == "spotify" {
        let sid = if tid.starts_with("spotify:track:") { tid.rsplit(':').next().unwrap_or("") } else { "" };
        if SPOTIFY_ID_RE.is_match(sid) {
            t.insert("spotifyId".into(), Value::from(sid));
        }
    }
    Some(t)
}

/// (scheme, netloc, path, query) of a URL, like urlsplit.
fn urlsplit(url: &str) -> (String, String, String, String) {
    let (scheme, rest) = match url.find("://") {
        Some(i) if url[..i].chars().all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c)) => (url[..i].to_lowercase(), &url[i + 3..]),
        _ => (String::new(), url),
    };
    let rest = rest.split('#').next().unwrap_or("");
    let (before_q, query) = match rest.split_once('?') {
        Some((a, b)) => (a, b.to_string()),
        None => (rest, String::new()),
    };
    let (netloc, path) = match before_q.find('/') {
        Some(i) => (&before_q[..i], before_q[i..].to_string()),
        None => (before_q, String::new()),
    };
    (scheme, netloc.to_string(), path, query)
}

fn hostname(netloc: &str) -> String {
    let h = netloc.rsplit('@').next().unwrap_or("");
    let h = if h.starts_with('[') {
        h.split(']').next().unwrap_or("").trim_start_matches('[')
    } else {
        h.split(':').next().unwrap_or("")
    };
    h.to_lowercase()
}

/// The video id of a YouTube / YouTube Music / youtu.be link, else None.
pub fn youtube_id_from_url(url: &str) -> Option<String> {
    static PATH_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^/(?:embed|shorts|live|v)/([^/?#]+)").unwrap());
    let url = py::strip(url);
    if YT_ID_RE.is_match(url) {
        return Some(url.to_string());
    }
    let full = if url.contains("://") { url.to_string() } else { format!("https://{}", url) };
    let (_, netloc, path, query) = urlsplit(&full);
    let mut host = hostname(&netloc);
    if let Some(h) = host.strip_prefix("www.") {
        host = h.to_string();
    }
    if let Some(h) = host.strip_prefix("m.") {
        host = h.to_string();
    }
    let v = if host == "youtu.be" {
        Some(path.trim_matches('/').split('/').next().unwrap_or("").to_string())
    } else if matches!(host.as_str(), "youtube.com" | "music.youtube.com" | "youtube-nocookie.com") {
        if path == "/watch" {
            Some(py::qs_first(&query, "v").unwrap_or_default())
        } else {
            PATH_RE.captures(&path).map(|c| c[1].to_string())
        }
    } else {
        None
    };
    v.filter(|v| !v.is_empty() && YT_ID_RE.is_match(v))
}

/// Parse "title<TAB>url" of a YouTube Music tab into a track or None.
pub fn parse_ytmusic_tab(out: &str) -> Option<Map<String, Value>> {
    let (title, url) = out.split_once('\t')?;
    let mut title = py::strip(title).to_string();
    let mut found = false;
    for suffix in [" - YouTube Music", " – YouTube Music"] {
        if let Some(t) = title.strip_suffix(suffix) {
            title = t.to_string();
            found = true;
            break;
        }
    }
    if !found || title.is_empty() || title == "YouTube Music" {
        return None;
    }
    let mut artist = String::new();
    if let Some((t, a)) = title.clone().split_once(" • ") {
        title = t.to_string();
        artist = a.to_string();
    }
    let mut t = obj(json!({"title": clean_text(&title, TEXT_MAX), "artist": clean_text(&artist, TEXT_MAX), "album": "",
                           "source": "ytmusic", "playing": true}));
    if t["title"].as_str().unwrap_or("").is_empty() {
        return None;
    }
    if let Some(vid) = youtube_id_from_url(url) {
        t.insert("youtubeId".into(), Value::from(vid));
    }
    Some(t)
}

/// What this Mac is playing: the first playing source, else the first paused.
pub fn detect() -> Option<Map<String, Value>> {
    if std::env::consts::OS != "macos" {
        return None;
    }
    let mut found: Vec<Map<String, Value>> = Vec::new();
    if running("Spotify") {
        if let Some(t) = parse_player(&osa(SPOTIFY), "spotify") {
            found.push(t);
        }
    }
    if running("Music") {
        if let Some(t) = parse_player(&osa(APPLE), "apple") {
            found.push(t);
        }
    }
    if !found.iter().any(|t| t["playing"] == true) {
        let mut broke = false;
        for (proc, app) in CHROMIUM_APPS {
            if running(proc) {
                if let Some(t) = parse_ytmusic_tab(&osa(&CHROMIUM.replace("%s", app))) {
                    found.push(t);
                    broke = true;
                    break;
                }
            }
        }
        if !broke && running("Safari") {
            if let Some(t) = parse_ytmusic_tab(&osa(SAFARI)) {
                found.push(t);
            }
        }
    }
    found.iter().find(|t| t["playing"] == true).cloned().or_else(|| found.into_iter().next())
}

/// Only TRACK_KEYS: the boundary of what leaves this machine.
pub fn wire_track(t: &Value) -> Option<Map<String, Value>> {
    let Value::Object(t) = t else { return None };
    let mut out = Map::new();
    for k in TRACK_KEYS {
        if let Some(v) = t.get(k) {
            if !v.is_null() {
                out.insert(k.into(), v.clone());
            }
        }
    }
    let src_ok = out.get("source").and_then(|v| v.as_str()).map(|s| SOURCES.contains(&s)).unwrap_or(false);
    if !src_ok || !out.get("title").map(py::truthy).unwrap_or(false) {
        return None;
    }
    Some(out)
}

/// Same track, same play state.
pub fn same_song(a: Option<&Map<String, Value>>, b: Option<&Map<String, Value>>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) if !a.is_empty() && !b.is_empty() => {
            ["title", "artist", "source", "playing"].iter().all(|k| a.get(*k) == b.get(*k))
        }
        (None, None) => true,
        _ => false,
    }
}

/// Puts the playing track on the Arena when it changes (and every `refresh`
/// seconds), takes it off when the music stops or sharing is turned off.
pub struct ShareLoop {
    pub enabled: Box<dyn Fn() -> bool + Send + Sync>,
    pub put: Box<dyn Fn(&Map<String, Value>) -> u16 + Send + Sync>,
    pub clear: Box<dyn Fn() -> u16 + Send + Sync>,
    pub refresh: f64,
    pub sent: Option<Map<String, Value>>,
    pub sent_at: f64,
    pub last_error: Option<u16>,
    pub err_at: f64,
}

impl ShareLoop {
    pub fn new(
        enabled: Box<dyn Fn() -> bool + Send + Sync>,
        put: Box<dyn Fn(&Map<String, Value>) -> u16 + Send + Sync>,
        clear: Box<dyn Fn() -> u16 + Send + Sync>,
    ) -> ShareLoop {
        ShareLoop { enabled, put, clear, refresh: 45.0, sent: None, sent_at: 0.0, last_error: None, err_at: 0.0 }
    }

    pub fn step(&mut self, now: f64, track: Option<Value>, detect_fn: &dyn Fn() -> Option<Map<String, Value>>) {
        if !(self.enabled)() {
            if self.sent.is_some() {
                (self.clear)();
                self.sent = None;
            }
            return;
        }
        let t = track.or_else(|| detect_fn().map(Value::Object));
        let t = t.filter(|t| t.get("playing").map(py::truthy).unwrap_or(false)).and_then(|t| wire_track(&t));
        let Some(t) = t else {
            if self.sent.is_some() {
                (self.clear)();
                self.sent = None;
            }
            return;
        };
        if same_song(Some(&t), self.sent.as_ref()) && now - self.sent_at < self.refresh {
            return;
        }
        if self.last_error == Some(404) && now - self.err_at < 600.0 {
            return;
        }
        let code = (self.put)(&t);
        if code == 200 {
            self.sent = Some(t);
            self.sent_at = now;
            self.last_error = None;
        } else {
            self.last_error = Some(code);
            self.err_at = now;
        }
    }
}

#[derive(Default)]
pub struct MusicState {
    current: Mutex<(f64, Option<Map<String, Value>>)>,
    oembed: Mutex<HashMap<String, Value>>,
    search: Mutex<HashMap<String, (f64, Vec<Value>)>>,
    audio: Mutex<HashMap<String, Value>>,
    /// The share loop's (sent?, last error) as the Now Playing route reports them.
    pub share: Mutex<Option<ShareLoop>>,
}

impl MusicState {
    /// detect(), cached for `max_age` seconds; also how old the reading is.
    pub fn current(&self, max_age: f64) -> (Option<Map<String, Value>>, f64) {
        {
            let g = self.current.lock().unwrap();
            let now = py::now_ts();
            if now - g.0 < max_age {
                return (g.1.clone(), now - g.0);
            }
        }
        let t = detect();
        *self.current.lock().unwrap() = (py::now_ts(), t.clone());
        (t, 0.0)
    }

    /// Title and channel of one video (YouTube's public oEmbed), or None.
    pub fn oembed(&self, vid: &str) -> Option<Value> {
        if !YT_ID_RE.is_match(vid) {
            return None;
        }
        if let Some(v) = self.oembed.lock().unwrap().get(vid) {
            return Some(v.clone());
        }
        let url = format!("https://www.youtube.com/oembed?format=json&url={}", py::quote(&format!("https://www.youtube.com/watch?v={}", vid), ""));
        let out = match http_get(&url) {
            Ok(raw) => {
                let j: Value = serde_json::from_str(&raw).ok()?;
                let s = |k: &str| j.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
                json!({"v": vid, "title": clean_text(&s("title"), TEXT_MAX), "author": clean_text(&s("author_name"), TEXT_MAX), "embeddable": true})
            }
            Err(Some(401 | 403)) => json!({"v": vid, "title": "", "author": "", "embeddable": false}),
            Err(_) => return None,
        };
        let mut g = self.oembed.lock().unwrap();
        if g.len() > 500 {
            g.clear();
        }
        g.insert(vid.to_string(), out.clone());
        Some(out)
    }

    /// Search YouTube for `q`, leaving out videos that can't be embedded.
    pub fn search(self: &Arc<Self>, q: &str, limit: usize, verify: bool) -> Vec<Value> {
        let q = clean_text(q, 120);
        if q.is_empty() {
            return Vec::new();
        }
        let key = q.to_lowercase();
        let hit = self.search.lock().unwrap().get(&key).cloned();
        let found = match hit {
            Some((at, f)) if py::now_ts() - at < 600.0 => f,
            _ => {
                let Ok(html) = http_get(&format!("https://www.youtube.com/results?search_query={}", py::quote_plus(&q))) else {
                    return Vec::new();
                };
                let f = parse_search(&html, limit * 2);
                let mut g = self.search.lock().unwrap();
                if g.len() > 200 {
                    g.clear();
                }
                g.insert(key, (py::now_ts(), f.clone()));
                f
            }
        };
        if !verify {
            return found.into_iter().take(limit).collect();
        }
        let handles: Vec<_> = found
            .iter()
            .map(|r| {
                let me = self.clone();
                let v = r["v"].as_str().unwrap_or("").to_string();
                std::thread::spawn(move || me.oembed(&v))
            })
            .collect();
        let checks: Vec<Option<Value>> = handles.into_iter().map(|h| h.join().ok().flatten()).collect();
        found
            .into_iter()
            .zip(checks)
            .filter(|(_, c)| !matches!(c, Some(c) if c.get("embeddable") == Some(&Value::Bool(false))))
            .map(|(r, _)| r)
            .take(limit)
            .collect()
    }

    // ---- local room audio (yt-dlp) -----------------------------------------

    fn existing(folder: &Path, vid: &str) -> Option<(PathBuf, &'static str)> {
        for (ext, _) in AUDIO_EXTS {
            let p = folder.join(format!("{}.{}", vid, ext));
            if std::fs::metadata(&p).map(|m| m.is_file() && m.len() > 0).unwrap_or(false) {
                return Some((p, ext));
            }
        }
        None
    }

    /// (path, content type) of a fetched song, or None. Bad ids never touch disk.
    pub fn audio_file(&self, folder: &Path, vid: &str) -> Option<(PathBuf, &'static str)> {
        if !YT_ID_RE.is_match(vid) {
            return None;
        }
        let (p, ext) = Self::existing(folder, vid)?;
        let now = std::time::SystemTime::now();
        if let Ok(f) = std::fs::File::options().write(true).open(&p) {
            let _ = f.set_times(std::fs::FileTimes::new().set_accessed(now).set_modified(now));
        }
        Some((p, AUDIO_EXTS.iter().find(|(e, _)| *e == ext).unwrap().1))
    }

    /// Starts fetching the song if it isn't here yet; its state.
    pub fn prepare(self: &Arc<Self>, folder: &Path, vid: &str, cookies: Option<String>) -> Map<String, Value> {
        if tool("yt-dlp").is_none() {
            return obj(json!({"state": "unavailable"}));
        }
        if !YT_ID_RE.is_match(vid) {
            return obj(json!({"state": "error", "error": "bad video id"}));
        }
        if let Some((_, ext)) = Self::existing(folder, vid) {
            return obj(json!({"state": "ready", "ext": ext}));
        }
        {
            let mut g = self.audio.lock().unwrap();
            if let Some(st) = g.get(vid) {
                let s = st["state"].as_str().unwrap_or("");
                if s == "loading" || (s == "error" && py::now_ts() - st["at"].as_f64().unwrap_or(0.0) < 300.0) {
                    return obj(st.clone());
                }
            }
            g.insert(vid.to_string(), json!({"state": "loading", "at": py::now_ts()}));
        }
        let me = self.clone();
        let folder = folder.to_path_buf();
        let vid = vid.to_string();
        std::thread::Builder::new()
            .name("hq-music-fetch".into())
            .spawn(move || me.fetch(&folder, &vid, cookies))
            .ok();
        obj(json!({"state": "loading"}))
    }

    fn fetch(&self, folder: &Path, vid: &str, cookies: Option<String>) {
        let _ = std::fs::create_dir_all(folder);
        let ytdlp = tool("yt-dlp").unwrap_or_else(|| "yt-dlp".into());
        let mut cmd = Command::new(ytdlp);
        cmd.args(["-f", "bestaudio[ext=m4a]/bestaudio", "--no-playlist", "--no-progress", "--quiet", "--no-warnings", "--no-mtime", "-o"]);
        cmd.arg(folder.join("%(id)s.%(ext)s"));
        if let Some(b) = cookies.as_deref().and_then(cookie_arg) {
            cmd.args(["--cookies-from-browser", &b]);
        }
        if let Some(ff) = tool("ffmpeg") {
            cmd.args(["--ffmpeg-location", &ff, "-x", "--audio-format", "m4a"]);
        }
        cmd.arg(format!("https://www.youtube.com/watch?v={}", vid));
        let mut path = std::env::var("PATH").unwrap_or_default();
        for d in tool_dirs() {
            path.push(':');
            path.push_str(&d);
        }
        cmd.env("PATH", path);
        let mut err = String::new();
        match run_timeout(&mut cmd, 180) {
            Ok((code, out, stderr)) => {
                if code != 0 {
                    let src = if !stderr.is_empty() { stderr } else if !out.is_empty() { out } else { "yt-dlp failed".into() };
                    err = py::take_chars(py::splitlines(py::strip(&src)).last().copied().unwrap_or(""), 200);
                }
            }
            Err(RunErr::Timeout) => err = "timed out".into(),
            Err(RunErr::NotFound) => err = "yt-dlp not found".into(),
            Err(RunErr::Other(e)) => err = py::take_chars(&e, 200),
        }
        let got = Self::existing(folder, vid);
        let st = match (got, err.is_empty()) {
            (Some((_, ext)), true) => json!({"state": "ready", "ext": ext, "at": py::now_ts()}),
            _ => json!({"state": "error", "error": if err.is_empty() { "no audio".to_string() } else { err }, "at": py::now_ts()}),
        };
        self.audio.lock().unwrap().insert(vid.to_string(), st);
        evict(folder, 400 * 1024 * 1024);
    }
}

fn evict(folder: &Path, cap: u64) {
    let Ok(rd) = std::fs::read_dir(folder) else { return };
    use std::os::unix::fs::MetadataExt;
    let mut files: Vec<(i64, u64, PathBuf)> = rd
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let m = e.metadata().ok()?;
            m.is_file().then(|| (m.atime(), m.len(), e.path()))
        })
        .collect();
    let mut total: u64 = files.iter().map(|f| f.1).sum();
    files.sort();
    for (_, size, f) in files {
        if total <= cap {
            break;
        }
        if std::fs::remove_file(&f).is_ok() {
            total -= size;
        }
    }
}

fn tool_dirs() -> Vec<String> {
    let home = std::env::var("HOME").unwrap_or_default();
    vec!["/opt/homebrew/bin".into(), "/usr/local/bin".into(), format!("{}/.local/bin", home)]
}

/// A command-line tool's path (PATH, then the usual install places).
pub fn tool(name: &str) -> Option<String> {
    if let Some(p) = crate::actions::which(name, &std::env::var("PATH").unwrap_or_default()) {
        return Some(p);
    }
    for d in tool_dirs() {
        let c = Path::new(&d).join(name);
        if crate::actions::is_executable(&c) {
            return Some(c.to_string_lossy().into_owned());
        }
    }
    None
}

/// GET with the browser UA (YouTube lookups). Err(Some(code)) for an HTTP error.
fn http_get(url: &str) -> Result<String, Option<u16>> {
    let agent = ureq::AgentBuilder::new().timeout(std::time::Duration::from_secs(6)).build();
    match agent.get(url).set("User-Agent", UA).set("Accept-Language", "en").call() {
        Ok(r) => {
            use std::io::Read;
            let mut buf = Vec::new();
            r.into_reader().take(2_000_000).read_to_end(&mut buf).map_err(|_| None)?;
            Ok(String::from_utf8_lossy(&buf).into_owned())
        }
        Err(ureq::Error::Status(code, _)) => Err(Some(code)),
        Err(_) => Err(None),
    }
}

/// Video results out of a YouTube results page's ytInitialData.
pub fn parse_search(html: &str, limit: usize) -> Vec<Value> {
    static VR: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#""videoRenderer":\{"videoId":"([A-Za-z0-9_-]{11})""#).unwrap());
    static TM: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#""title":\{"runs":\[\{"text":"((?:[^"\\]|\\.)*)""#).unwrap());
    static AM: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#""ownerText":\{"runs":\[\{"text":"((?:[^"\\]|\\.)*)""#).unwrap());
    static LM: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#""lengthText":\{"accessibility":\{"accessibilityData":\{"label":"(?:[^"\\]|\\.)*"\}\},"simpleText":"([0-9:]+)""#).unwrap()
    });
    let unq = |s: &str| serde_json::from_str::<String>(&format!("\"{}\"", s)).unwrap_or_else(|_| s.to_string());
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for c in VR.captures_iter(html) {
        let vid = c[1].to_string();
        if !seen.insert(vid.clone()) {
            continue;
        }
        let end = c.get(0).unwrap().end();
        let chunk: String = html[end..].chars().take(4000).collect();
        let title = TM.captures(&chunk).map(|m| clean_text(&unq(&m[1]), TEXT_MAX)).unwrap_or_default();
        if title.is_empty() {
            continue;
        }
        out.push(json!({
            "v": vid, "title": title,
            "author": AM.captures(&chunk).map(|m| clean_text(&unq(&m[1]), TEXT_MAX)).unwrap_or_default(),
            "length": LM.captures(&chunk).map(|m| m[1].to_string()).unwrap_or_default(),
        }));
        if out.len() >= limit {
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[test]
    fn parses_players() {
        let t = parse_player("playing\tOWA OWA\tLil Tecca\tDOPAMINE\tspotify:track:1lfO0bqThTLimzHsvk3LrN\t132381\t41,869", "spotify").unwrap();
        assert_eq!(t["title"], "OWA OWA");
        assert_eq!(t["spotifyId"], "1lfO0bqThTLimzHsvk3LrN");
        assert_eq!(t["durationMs"], 132381);
        assert_eq!(t["positionMs"], 41869);
        assert_eq!(t["playing"], true);
        let t = parse_player("paused\tSong\tArtist\tAlbum\t\t200000\t12.5", "apple").unwrap();
        assert_eq!(t["playing"], false);
        assert!(!t.contains_key("spotifyId"));
        assert_eq!(t["positionMs"], 12500);
        assert!(parse_player("stopped", "apple").is_none());
        assert!(parse_player("", "spotify").is_none());
        assert!(parse_player("playing\t\tA\tB\t\t1\t1", "spotify").is_none());
        assert_eq!(parse_player("playing\tA\u{202e}B\u{0}  C\tX\tY\t\t1000\t0", "apple").unwrap()["title"], "A B C");
    }

    #[test]
    fn youtube_links() {
        assert_eq!(youtube_id_from_url("https://www.youtube.com/watch?v=dQw4w9WgXcQ&t=3").as_deref(), Some("dQw4w9WgXcQ"));
        assert_eq!(youtube_id_from_url("https://music.youtube.com/watch?v=dQw4w9WgXcQ&list=x").as_deref(), Some("dQw4w9WgXcQ"));
        assert_eq!(youtube_id_from_url("youtu.be/dQw4w9WgXcQ").as_deref(), Some("dQw4w9WgXcQ"));
        assert_eq!(youtube_id_from_url("https://youtube.com/shorts/dQw4w9WgXcQ").as_deref(), Some("dQw4w9WgXcQ"));
        assert_eq!(youtube_id_from_url("dQw4w9WgXcQ").as_deref(), Some("dQw4w9WgXcQ"));
        assert_eq!(youtube_id_from_url("https://evil.example/watch?v=dQw4w9WgXcQ"), None);
        assert_eq!(youtube_id_from_url("some song name"), None);
        let t = parse_ytmusic_tab("Blinding Lights • The Weeknd - YouTube Music\thttps://music.youtube.com/watch?v=4NRXx6U8ABQ").unwrap();
        assert_eq!((t["title"].as_str(), t["artist"].as_str(), t["youtubeId"].as_str()), (Some("Blinding Lights"), Some("The Weeknd"), Some("4NRXx6U8ABQ")));
        assert!(parse_ytmusic_tab("YouTube Music\thttps://music.youtube.com/").is_none());
        assert!(parse_ytmusic_tab("Inbox - Gmail\thttps://mail.google.com/").is_none());
    }

    #[test]
    fn search_page() {
        let html = concat!(r#"..."videoRenderer":{"videoId":"Uu43993zDMA","thumbnail":{},"title":{"runs":[{"text":"F3miii - FROM YOUR EYES & more"}]},"#,
            r#""ownerText":{"runs":[{"text":"f3miii"}]},"lengthText":{"accessibility":{"accessibilityData":{"label":"2 minutes"}},"simpleText":"2:37"}}"#,
            r#"..."videoRenderer":{"videoId":"Uu43993zDMA"}..."#);
        assert_eq!(parse_search(html, 6), vec![json!({"v": "Uu43993zDMA", "title": "F3miii - FROM YOUR EYES & more", "author": "f3miii", "length": "2:37"})]);
    }

    #[test]
    fn wire_boundary() {
        let t = json!({"title": "T", "source": "spotify", "playing": true, "cwd": "/Users/x/secret", "sessionId": "abc"});
        assert_eq!(Value::Object(wire_track(&t).unwrap()), json!({"title": "T", "source": "spotify", "playing": true}));
        assert!(wire_track(&json!({"title": "T", "source": "winamp"})).is_none());
        assert!(wire_track(&json!({"source": "apple"})).is_none());
    }

    fn share(on: Arc<AtomicBool>, calls: Arc<Mutex<Vec<String>>>, code: Arc<Mutex<Vec<u16>>>) -> ShareLoop {
        let c1 = calls.clone();
        let c2 = calls;
        ShareLoop::new(
            Box::new(move || on.load(Ordering::SeqCst)),
            Box::new(move |t| {
                c1.lock().unwrap().push(format!("put {}", t["title"].as_str().unwrap()));
                let mut g = code.lock().unwrap();
                if g.len() > 1 { g.remove(0) } else { g[0] }
            }),
            Box::new(move || {
                c2.lock().unwrap().push("clear".into());
                200
            }),
        )
    }

    #[test]
    fn share_loop_puts_refreshes_and_clears() {
        let on = Arc::new(AtomicBool::new(true));
        let calls = Arc::new(Mutex::new(Vec::new()));
        let mut lp = share(on.clone(), calls.clone(), Arc::new(Mutex::new(vec![200])));
        let none = || None;
        let a = json!({"title": "A", "artist": "x", "source": "spotify", "playing": true, "positionMs": 1});
        let with = |k: &str, v: Value| {
            let mut x = a.clone();
            x[k] = v;
            x
        };
        lp.step(0.0, Some(a.clone()), &none);
        lp.step(10.0, Some(with("positionMs", json!(9000))), &none);
        lp.step(50.0, Some(a.clone()), &none);
        lp.step(55.0, Some(with("title", json!("B"))), &none);
        let mut paused = with("title", json!("B"));
        paused["playing"] = json!(false);
        lp.step(60.0, Some(paused), &none);
        assert_eq!(*calls.lock().unwrap(), vec!["put A", "put A", "put B", "clear"]);
        calls.lock().unwrap().clear();
        lp.step(61.0, Some(a.clone()), &none);
        on.store(false, Ordering::SeqCst);
        lp.step(65.0, None, &none);
        lp.step(70.0, None, &none);
        assert_eq!(*calls.lock().unwrap(), vec!["put A", "clear"]);
    }

    #[test]
    fn share_loop_retries_and_backs_off_404() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let mut lp = share(Arc::new(AtomicBool::new(true)), calls.clone(), Arc::new(Mutex::new(vec![0, 200])));
        let t = json!({"title": "A", "source": "apple", "playing": true});
        lp.step(0.0, Some(t.clone()), &|| None);
        assert_eq!(lp.last_error, Some(0));
        lp.step(10.0, Some(t.clone()), &|| None);
        assert_eq!(calls.lock().unwrap().len(), 2);
        assert_eq!(lp.last_error, None);
        let calls = Arc::new(Mutex::new(Vec::new()));
        let mut lp = share(Arc::new(AtomicBool::new(true)), calls.clone(), Arc::new(Mutex::new(vec![404])));
        for now in [0.0, 10.0, 20.0, 300.0] {
            lp.step(now, Some(t.clone()), &|| None);
        }
        assert_eq!(calls.lock().unwrap().len(), 1);
        lp.step(601.0, Some(t), &|| None);
        assert_eq!(calls.lock().unwrap().len(), 2);
    }

    #[test]
    fn audio_cache_ids_never_touch_disk() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("dQw4w9WgXcQ.m4a"), b"xxxxxxxxxx").unwrap();
        let m = MusicState::default();
        assert_eq!(m.audio_file(d.path(), "dQw4w9WgXcQ").unwrap().1, "audio/mp4");
        assert!(m.audio_file(d.path(), "../../etc/passwd").is_none());
        assert!(cookie_arg("operagx").unwrap().starts_with("opera:"));
        assert_eq!(cookie_arg("chrome").as_deref(), Some("chrome"));
        assert!(cookie_arg("rm -rf").is_none());
        assert!(cookie_arg("").is_none());
    }
}
