//! Soundboard clips served from a directory on the Arena host, ported from
//! `backend/app/routes/sounds.py`.
//!
//! The audio files live on the server (and are git-ignored), so the public repo
//! never carries them. Every route here -- the two reads included -- sits behind
//! the shared device gate, so a clip is only reachable by an authenticated,
//! non-revoked device belonging to an active user. main.rs applies
//! `require_device` to the whole guarded router; this module must not add its
//! own layer and must not register any of these three routes as public.
//!
//! Zero database access: all three handlers touch only the filesystem. The one
//! write in the request path is the middleware's `UPDATE devices SET
//! last_seen_at`, which main.rs owns.
//!
//! `Caller` is never read. The caller is a pure gate, so the sounds dir is a
//! single global namespace: any paired member can upload a clip every other
//! member hears, nothing is attributed to an uploader, and there is no delete
//! route. Extracting a `Caller` and then not using it would trip
//! `unused_variables` under `-D warnings`, so the handlers take only what they
//! read from the request.
//!
//! The parity-critical, easy-to-get-wrong bits, all of them visible on the wire:
//!
//!   * `_AUDIO_TYPES` is the single source of truth for three decisions --
//!     which files the listing shows, which names POST accepts, and which MIME
//!     type GET-one returns -- and it is consulted on a lowercased suffix.
//!   * The name sanitiser substitutes per *codepoint*, and `.strip()` runs
//!     *after* substitution. Byte iteration or the other order changes the
//!     stored filename and the 201 body.
//!   * The 80-character cap truncates silently and, because it re-appends the
//!     lowercased suffix, silently lowercases the extension.
//!   * `Path.resolve()` is non-strict where `fs::canonicalize` is not, so a
//!     missing sounds dir must fall back to a lexical absolute path or a fresh
//!     host 500s where Python returns `{"sounds": []}`.
//!   * FastAPI's unhandled-exception 500 is a `text/plain` body, not the
//!     `{"detail": ...}` envelope every 4xx here uses.
//!
//! Key order matters and `serde_json`'s map is a `BTreeMap` (alphabetical), so
//! the response bodies are structs -- serde emits struct fields in declaration
//! order, which is the order Python's dict literals emit.

use axum::{
    body::Body,
    extract::{DefaultBodyLimit, Path as UrlPath, Request},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use serde::Serialize;
use serde_json::json;
use std::path::{Component, Path, PathBuf};
use tokio::io::AsyncReadExt;

/// Extensions we are willing to list and serve, mapped to their MIME type.
/// An array rather than a map: the order is load-bearing for [`ALLOWED_EXTS`],
/// and five linear comparisons are cheaper than hashing.
const AUDIO_TYPES: [(&str, &str); 5] = [
    (".ogg", "audio/ogg"),
    (".mp3", "audio/mpeg"),
    (".wav", "audio/wav"),
    (".m4a", "audio/mp4"),
    (".webm", "audio/webm"),
];

/// `", ".join(sorted(_AUDIO_TYPES))` -- the keys *with* their leading dots,
/// ASCII sorted, so `.m4a` precedes `.mp3` (`'4' < 'p'`) and `.wav` precedes
/// `.webm` (`'a' < 'e'`). Hardcoded because a hash-map walk or a different
/// sort would scramble a string the clients show verbatim; the test below pins
/// it back to the map so the two cannot drift.
const ALLOWED_EXTS: &str = ".m4a, .mp3, .ogg, .wav, .webm";

/// `_MAX_NAME_LEN`: the cap on a stored basename, *including* the extension.
const MAX_NAME_LEN: usize = 80;

pub fn routes() -> axum::Router<crate::AppState> {
    Router::new()
        .route("/v1/sounds", get(list_sounds).post(upload_sound))
        .route("/v1/sounds/:file", get(get_sound))
        // axum's DefaultBodyLimit is 2 MiB and would pre-empt the handler's own
        // 413: a 3 MB upload Python accepts would come back as axum's
        // `text/plain` "length limit exceeded" instead of
        // `{"detail": "file too large (max 5 MB)"}`. [`upload_sound`] reads the
        // body itself with an explicit ceiling, which already sidesteps the
        // default (it is only consulted by body-consuming *extractors*), but
        // disabling it here keeps that true if anyone later reaches for `Bytes`.
        .layer(DefaultBodyLimit::disable())
}

// --- responses ------------------------------------------------------------

/// `{"name": ..., "file": ..., "size": ...}`, in that order: one listing entry,
/// and also the whole 201 body of an upload.
///
/// `name` is `Path.stem`, which strips only the *last* extension -- so
/// `mix.final.mp3` has the name `mix.final`. The browser labels its buttons
/// with `s.name || s.file`, making that user-visible text.
#[derive(Serialize)]
struct SoundEntry {
    name: String,
    file: String,
    size: u64,
}

#[derive(Serialize)]
struct SoundList {
    sounds: Vec<SoundEntry>,
}

/// FastAPI's `HTTPException` envelope, same shape as main.rs's `err()`.
fn err(code: StatusCode, msg: &str) -> Response {
    (code, Json(json!({ "detail": msg }))).into_response()
}

/// FastAPI's *unhandled* exception response: status 500 with a `text/plain`
/// `Internal Server Error` body, **not** a JSON `detail`. Reproduced rather
/// than improved into a clean 403/503, because the live production failure mode
/// lands here -- docker-compose bind-mounts `./sounds:/srv/sounds:ro`, so every
/// deployed upload fails EROFS -- and the in-repo UI already branches on it.
fn internal() -> Response {
    (StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error").into_response()
}

/// The one 404 on GET-one. Uniform by design: it covers "escaped the sounds
/// dir", "wrong extension" and "not there" without saying which, so the
/// response cannot be used to probe for arbitrary host paths. Resist adding a
/// more helpful message for the extension case.
fn not_found() -> Response {
    err(StatusCode::NOT_FOUND, "no such sound")
}

/// `?name=` omitted entirely. The app's `_scrub_422` handler rebuilds every
/// validation error as exactly `loc`, `msg`, `type` in that order, deliberately
/// dropping FastAPI's stock `input` and `url` keys. Those three names are
/// already in alphabetical order, so `serde_json`'s `BTreeMap` emits them
/// correctly without a struct.
fn missing_name() -> Response {
    (
        StatusCode::UNPROCESSABLE_ENTITY,
        Json(json!({
            "detail": [{"loc": ["query", "name"], "msg": "Field required", "type": "missing"}]
        })),
    )
        .into_response()
}

/// `f"file too large (max {max_sound_bytes // (1024 * 1024)} MB)"` -- FLOOR
/// division, so `ARENA_MAX_SOUND_BYTES=500000` really does read `(max 0 MB)`
/// and a 6,000,000-byte cap reads `(max 5 MB)`. Do not round or format a
/// decimal.
fn too_large(max: usize) -> String {
    format!("file too large (max {} MB)", max / (1024 * 1024))
}

// --- settings -------------------------------------------------------------

/// `Settings` in config.rs carries neither of this group's keys and rule 1
/// forbids editing it, so both are read straight from the process environment.
/// Python's pydantic-settings also reads `backend/.env`; neither key appears
/// there or in `.env.example`, and docker-compose sets `ARENA_SOUNDS_DIR` in the
/// `environment:` block shared by both images, so production parity holds.
fn sounds_dir_setting() -> String {
    std::env::var("ARENA_SOUNDS_DIR").unwrap_or_else(|_| "./sounds".into())
}

fn max_sound_bytes() -> usize {
    std::env::var("ARENA_MAX_SOUND_BYTES")
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(5 * 1024 * 1024)
}

/// `Path(get_settings().sounds_dir).expanduser().resolve()`.
///
/// Python's `get_settings()` is `@lru_cache`d but `_sounds_dir()` runs *per
/// request*, so the configured string is frozen at first use while its
/// resolution is redone every time: a directory created, remounted or
/// re-symlinked after boot is picked up with no restart. Caching the resolved
/// `PathBuf` in process state would lose that half of the behaviour.
async fn sounds_dir() -> PathBuf {
    let raw = sounds_dir_setting();
    let home = std::env::var("HOME").unwrap_or_default();
    resolve(&expanduser(&raw, &home)).await
}

/// `Path.expanduser()`, limited to a bare leading `~`. The `~user` form needs
/// the passwd database, which std cannot read and rule 5 forbids a crate for,
/// so `~user/...` is left alone where Python would expand it. Skipping the
/// expansion entirely would make `ARENA_SOUNDS_DIR=~/sounds` create a directory
/// literally named `~` beside the binary.
fn expanduser(raw: &str, home: &str) -> PathBuf {
    if home.is_empty() {
        return PathBuf::from(raw);
    }
    if raw == "~" {
        return PathBuf::from(home);
    }
    match raw.strip_prefix("~/") {
        Some(rest) => Path::new(home).join(rest),
        None => PathBuf::from(raw),
    }
}

/// `Path.resolve()`, which is NON-strict: it canonicalises the components that
/// exist, follows symlinks, collapses `..`, makes the path absolute against the
/// CWD -- and does *not* fail when the path is missing. `fs::canonicalize` does
/// fail (`ErrorKind::NotFound`), so a missing path falls back to
/// `cwd.join(p)` plus lexical normalisation. Without that fallback
/// `GET /v1/sounds` would 500 on a fresh host instead of returning
/// `{"sounds": []}`, and the default `./sounds` is relative, so the fallback is
/// taken whenever the process starts somewhere that has no sounds dir yet.
async fn resolve(p: &Path) -> PathBuf {
    match tokio::fs::canonicalize(p).await {
        Ok(c) => c,
        Err(_) => lexical_absolute(p),
    }
}

/// Absolutise and collapse `.`/`..` without touching the filesystem. `getcwd`
/// is already symlink-free, so for the paths this module builds the result is
/// canonical wherever it matters (both sides of the containment check).
fn lexical_absolute(p: &Path) -> PathBuf {
    let mut out = if p.is_absolute() {
        PathBuf::new()
    } else {
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"))
    };
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Python's `d in target.parents`: `d` is a *strict* ancestor of `target`.
///
/// `Path.parents` is the whole ancestor chain, so this is a CONTAINMENT check,
/// not a depth-1 one -- `d/sub/clip.mp3` satisfies it too. Do not narrow this to
/// `target.parent() == Some(d)`: that is strictly stricter than the Python and
/// would reject a nested path the moment anyone adds folder support.
/// `Path`'s `PartialEq` compares components, so a trailing slash cannot fool it.
fn inside(dir: &Path, target: &Path) -> bool {
    target.starts_with(dir) && target != dir
}

// --- pathlib and name handling --------------------------------------------

/// `Path(name).name`. pathlib drops empty and `"."` segments while parsing, so
/// `a/./` is `a` and `"."` is `""`, but a trailing `..` survives as `".."` --
/// which `Path::file_name` would instead report as `None`. (Every `..` case
/// ends in the same 400 either way, but matching pathlib keeps the reasoning
/// local.) On POSIX only `/` separates, so a backslash is an ordinary
/// character: `a\b.mp3` is one filename.
fn basename(raw: &str) -> &str {
    raw.rsplit('/').find(|s| !s.is_empty() && *s != ".").unwrap_or("")
}

/// `Path.suffix`, as CPython 3.14 computes it -- the rule changed in 3.12 and
/// the backend's venv is 3.14, so this follows the current source:
///
/// ```python
/// name = self.name.lstrip('.'); i = name.rfind('.')
/// return name[i:] if i != -1 else ''
/// ```
///
/// Leading dots are stripped FIRST, so `.mp3` and `..mp3` have no suffix at all
/// -- which is why a file literally named `..mp3` is neither listed nor served.
/// A trailing dot, by contrast, *is* a suffix: `a.` has the suffix `.`, which
/// is simply never in [`AUDIO_TYPES`]. Indexing by byte rather than character
/// is safe and equivalent: an ASCII `.` is always a UTF-8 boundary.
fn suffix(name: &str) -> &str {
    let stripped = name.trim_start_matches('.');
    match stripped.rfind('.') {
        Some(i) => &stripped[i..],
        None => "",
    }
}

/// `Path.stem`: the name minus [`suffix`], so only the LAST extension is
/// stripped and `mix.final.mp3` stems to `mix.final`.
///
/// pathlib spells this out longhand with a "the stem must contain at least one
/// non-dot character" guard, but that guard can only fire when [`suffix`] is
/// already empty, so subtracting the suffix is the same function.
fn stem(name: &str) -> &str {
    &name[..name.len() - suffix(name).len()]
}

/// Look up an already-lowercased suffix. The lowercasing is the caller's job
/// because Python writes `suffix.lower()` at each of the three call sites.
fn audio_mime(suffix_lower: &str) -> Option<&'static str> {
    AUDIO_TYPES.iter().find(|(e, _)| *e == suffix_lower).map(|(_, m)| *m)
}

/// `_SAFE_NAME = re.compile(r"[^A-Za-z0-9 ._()-]")`, inverted: the characters a
/// stored filename may keep. An allowlist, not a denylist -- which is what makes
/// every stored name pure ASCII, traversal-safe and shell-safe, instead of
/// relying on spotting bad patterns.
fn is_safe(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, ' ' | '.' | '_' | '(' | ')' | '-')
}

/// POST's whole name pipeline (basename, substitute, strip, validate, cap) as
/// one pure function: `Some(stored filename)`, or `None` for the
/// `name must be an audio file (...)` 400. Split out from the handler so the
/// parity-critical parts are unit-testable with no filesystem.
fn stored_name(raw: &str) -> Option<String> {
    // `re.sub` replaces one CODEPOINT with one `_`, so iterate `chars()`:
    // `café.mp3` must become `caf_.mp3` (one underscore). Byte iteration would
    // emit `caf__.mp3`, changing the stored name, the 201 body, and whether the
    // 80-character cap trips.
    let subbed: String = basename(raw).chars().map(|c| if is_safe(c) { c } else { '_' }).collect();
    // `.strip()` runs AFTER substitution, which makes whitespace asymmetric: a
    // trailing SPACE survives the substitution and is then stripped, but a
    // trailing TAB has already become `_` and therefore survives the strip --
    // turning `clip.mp3<TAB>` into `clip.mp3_`, whose suffix `.mp3_` is not in
    // the map, so it 400s. Keep these two operations in this order.
    // (By this point the string is pure ASCII and the only whitespace that can
    // remain is the space, so `trim` and `strip` agree.)
    let safe = subbed.trim();

    let ext = suffix(safe).to_lowercase();
    if safe.is_empty() || safe.starts_with('.') || audio_mime(&ext).is_none() {
        return None;
    }
    if safe.len() > MAX_NAME_LEN {
        // Silent truncation, never a rejection -- and because the re-appended
        // `ext` is the LOWERCASED suffix while `safe` keeps the sender's case,
        // an 81-character `AAA....A.MP3` is stored with a lowercase `.mp3`
        // while a 70-character `A.MP3` keeps `.MP3`. Easy to "fix" by accident;
        // the 201 body reports the stored name, so don't.
        // `safe` is ASCII by now, so a byte slice is the character slice.
        return Some(format!("{}{ext}", &safe[..MAX_NAME_LEN - ext.len()]));
    }
    Some(safe.to_string())
}

// --- query string ---------------------------------------------------------

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// `application/x-www-form-urlencoded` decoding, the way Starlette's
/// `parse_qsl` does it: `+` is a space, `%XX` is a byte, and a malformed escape
/// is left alone rather than raising. Bytes that are not valid UTF-8 reach
/// Python as surrogates; here they are replaced, which only ever shows up in
/// the sanitiser, and the sanitiser would have turned them into `_` anyway.
fn form_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < b.len() => match (hex_val(b[i + 1]), hex_val(b[i + 2])) {
                (Some(h), Some(l)) => {
                    out.push((h << 4) | l);
                    i += 3;
                }
                _ => {
                    out.push(b'%');
                    i += 1;
                }
            },
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// One query parameter, read by hand rather than with `Query<T>` so that a
/// missing or odd value produces Python's exact 422 (or falls through to the
/// 400) instead of axum's own rejection body.
///
/// Last-wins, like Starlette's `QueryParams` dict comprehension and FastAPI's
/// `.get()`. `keep_blank_values=True`, so `?name=` and a bare `?name` are both
/// present-and-empty -- an empty string is a valid `str` and is NOT a 422; it
/// falls through to the `name must be an audio file (...)` 400.
fn query_param(query: &str, key: &str) -> Option<String> {
    let mut found = None;
    for pair in query.split('&').filter(|p| !p.is_empty()) {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        if form_decode(k) == key {
            found = Some(form_decode(v));
        }
    }
    found
}

/// One unfold step of the streamed clip: a chunk and the file back, or `None`
/// at EOF. A read error ends the body mid-flight, which is all a Rust server
/// can do once the headers are on the wire -- Starlette is in the same
/// position.
async fn read_chunk(
    mut f: tokio::fs::File,
) -> std::io::Result<Option<(Vec<u8>, tokio::fs::File)>> {
    let mut buf = vec![0u8; 64 * 1024];
    let n = f.read(&mut buf).await?;
    if n == 0 {
        return Ok(None);
    }
    buf.truncate(n);
    Ok(Some((buf, f)))
}

/// Starlette sets `last-modified` with `formatdate(st_mtime, usegmt=True)`, an
/// RFC 7231 IMF-fixdate. chrono's weekday and month names are English and
/// locale-independent, which is exactly what the format requires.
fn http_date(t: std::time::SystemTime) -> String {
    chrono::DateTime::<chrono::Utc>::from(t).format("%a, %d %b %Y %H:%M:%S GMT").to_string()
}

// --- handlers -------------------------------------------------------------

/// `GET /v1/sounds` -- list the available clips. `name` is the filename without
/// its last extension (the label the soundboard shows); `file` is the token
/// `GET /v1/sounds/{file}` wants.
///
/// Takes no extractors: the device gate is applied by main.rs and no field of
/// the caller is read.
async fn list_sounds() -> Response {
    let d = sounds_dir().await;

    // `d.is_dir()` follows symlinks. A missing path, a regular file, a dangling
    // symlink or EACCES on the parent all mean an empty list and a 200 -- a
    // fresh host with no clips yet is a working soundboard, not a broken
    // endpoint.
    if !tokio::fs::metadata(&d).await.is_ok_and(|m| m.is_dir()) {
        return Json(SoundList { sounds: Vec::new() }).into_response();
    }

    let mut rd = match tokio::fs::read_dir(&d).await {
        Ok(rd) => rd,
        // Python lets a PermissionError out of `iterdir()` as a bare 500.
        Err(_) => return internal(),
    };
    let mut names: Vec<String> = Vec::new();
    loop {
        let Ok(entry) = rd.next_entry().await else { return internal() };
        let Some(entry) = entry else { break };
        // Python surfaces a non-UTF-8 name via surrogateescape and lets
        // FastAPI's encoder emit it; std cannot hand us a `&str` at all, so such
        // an entry is skipped rather than lossily converted. Unreachable for a
        // file this API created -- POST sanitises every stored name to pure
        // ASCII -- but reachable for one dropped in by hand or over scp.
        if let Some(n) = entry.file_name().to_str() {
            names.push(n.to_string());
        }
    }

    // `sorted()` over `Path` objects compares `str(path)` case-SENSITIVELY, and
    // every entry shares the same parent, so it is a codepoint-wise sort of the
    // filenames -- `Zap.mp3` before `apple.mp3`. UTF-8 byte order *is* codepoint
    // order, so `String`'s `Ord` is the same comparison. `read_dir` order is
    // unordered on both ext4 and APFS and must never reach the response.
    names.sort_unstable();

    let mut sounds: Vec<SoundEntry> = Vec::new();
    for name in names {
        let p = d.join(&name);
        // `p.is_file()` follows symlinks (so a symlink to a real file IS
        // listed) and swallows any OSError, so a failure here is a skip. Python
        // then stats a second time for the size and lets *that* failure out as a
        // 500; one metadata call collapses that microsecond race into the skip.
        let Ok(md) = tokio::fs::metadata(&p).await else { continue };
        if !md.is_file() {
            continue;
        }
        // Subdirectories are excluded, a directory literally named `beep.mp3`
        // is excluded, and a half-finished `beep.mp3.part` is excluded because
        // `.part` is not in the map -- which is precisely why POST writes to a
        // `.part` sibling. Dotfiles like `.foo.mp3` ARE listed even though POST
        // refuses to create them; that asymmetry is deliberate, so no dotfile
        // filter here.
        if audio_mime(&suffix(&name).to_lowercase()).is_none() {
            continue;
        }
        sounds.push(SoundEntry { name: stem(&name).to_string(), file: name, size: md.len() });
    }

    Json(SoundList { sounds }).into_response()
}

/// `POST /v1/sounds?name=<filename>` -- store an uploaded clip so it joins the
/// soundboard, the web equivalent of SSHing in to drop a file in the sounds
/// dir. The raw audio is the request body (no multipart, no JSON, no base64,
/// and `Content-Type` is ignored); `name` is a query parameter whose extension
/// decides the type.
///
/// The order of operations is observable and load-bearing: gate -> `name`
/// presence (422) -> name shape (400) -> truncation -> body read -> empty (400)
/// -> size (413) -> mkdir -> containment (400) -> existence (409) -> write ->
/// rename -> 201.
async fn upload_sound(req: Request) -> Response {
    let max = max_sound_bytes();

    let Some(raw) = query_param(req.uri().query().unwrap_or(""), "name") else {
        return missing_name();
    };

    // Validated before the body is read, so a bad name short-circuits without
    // consuming the upload.
    let Some(safe) = stored_name(&raw) else {
        let detail = format!("name must be an audio file ({ALLOWED_EXTS})");
        return err(StatusCode::BAD_REQUEST, &detail);
    };

    // Python buffers the whole body and only then compares lengths. The `+ 1`
    // ceiling lets a body one byte over the cap through to the handler's own
    // 413 rather than being cut off by the reader; anything larger could only
    // produce the same 413, and a genuine mid-stream read error means the client
    // is already gone and will never see this response.
    let data = match axum::body::to_bytes(req.into_body(), max.saturating_add(1)).await {
        Ok(b) => b,
        Err(_) => return err(StatusCode::PAYLOAD_TOO_LARGE, &too_large(max)),
    };
    if data.is_empty() {
        return err(StatusCode::BAD_REQUEST, "empty upload");
    }
    if data.len() > max {
        return err(StatusCode::PAYLOAD_TOO_LARGE, &too_large(max));
    }

    let d = sounds_dir().await;
    // Created only after every validation above has passed, so a rejected
    // request must never bring the sounds directory into existence.
    if tokio::fs::create_dir_all(&d).await.is_err() {
        return internal();
    }

    let target = resolve(&d.join(&safe)).await;
    if !inside(&d, &target) {
        // Dead code in practice: the sanitiser has already turned every `/`
        // into `_` and taken the basename, and the leading-dot check has already
        // rejected `..`. Ported anyway, exactly as the Python keeps it --
        // defence in depth against a future change to the sanitiser.
        return err(StatusCode::BAD_REQUEST, "bad name");
    }
    // Check-then-write with no lock, exactly as the Python. Two concurrent
    // uploads of the same name can both pass this check, both write the SAME
    // `.part` path (interleaving their bytes), and both rename -- so the
    // survivor can be a splice of two clips and both callers get a 201. Narrow,
    // and unfixable without changing the wire contract: `O_EXCL` on the `.part`
    // would turn the loser into a 500.
    if tokio::fs::try_exists(&target).await.unwrap_or(false) {
        return err(StatusCode::CONFLICT, "a sound with that name already exists");
    }

    // Write a `.part` sibling, then rename. `.part` is absent from AUDIO_TYPES,
    // so an in-flight or abandoned partial upload is invisible to the listing
    // and unfetchable via GET-one; and `rename(2)` is atomic, so a reader only
    // ever observes the complete file or no file.
    let tmp = target.with_file_name(format!("{safe}.part"));
    if tokio::fs::write(&tmp, &data).await.is_err() {
        // The read-only bind mount in docker-compose.yml lands here (EROFS), so
        // every upload to the deployed container is a 500. A failed write also
        // leaves the `.part` behind forever -- there is no cleanup and no
        // sweeper, and a retry of the same name reuses that path. Python does
        // neither; do not add either, or the clients' error handling diverges.
        return internal();
    }
    if tokio::fs::rename(&tmp, &target).await.is_err() {
        return internal();
    }

    // `safe` *is* `target.name`: it has no separator and the target did not
    // exist, so resolution could not have changed the final component. A client
    // that sent `my song!.mp3` gets back `file: "my song_.mp3"` -- the stored
    // name, not the one it sent. No `ok` flag, no `sounds` wrapper, and never a
    // `source` key (that is the local dashboard proxy's, not the backend's).
    (
        StatusCode::CREATED,
        // `name` is evaluated before `file`, so the borrow ends before the move.
        Json(SoundEntry { name: stem(&safe).to_string(), file: safe, size: data.len() as u64 }),
    )
        .into_response()
}

/// `GET /v1/sounds/{file}` -- the raw clip bytes.
///
/// `file` is always a single path segment: axum will not match a `/` against
/// `:file`, and the server has already percent-decoded it.
async fn get_sound(UrlPath(file): UrlPath<String>) -> Response {
    let d = sounds_dir().await;

    // Resolve FIRST, check SECOND. Canonicalising before any filesystem access
    // is what blocks `../` and absolute-path escapes before a byte is read --
    // and because resolution follows symlinks, a symlink *inside* the sounds dir
    // that points outside it resolves outside and is rejected. The
    // canonicalise-then-prefix-check on the FINAL path is mandatory; merely
    // rejecting a `/` would not stop that symlink escape.
    let target = resolve(&d.join(&file)).await;

    // All three conditions collapse into the same 404, short-circuiting left to
    // right, exactly as the Python's single `if` does. `file == "."` or `""`
    // resolves to `d` itself, which is not among its own parents; `".."`
    // resolves to `d.parent()`.
    if !inside(&d, &target) {
        return not_found();
    }
    // A non-UTF-8 stored name yields an empty suffix here and so 404s where
    // Python would serve it -- the same `OsStr` limitation as the listing.
    let name = target.file_name().and_then(|s| s.to_str()).unwrap_or("");
    let Some(mime) = audio_mime(&suffix(name).to_lowercase()) else {
        return not_found();
    };
    let Ok(md) = tokio::fs::metadata(&target).await else {
        return not_found(); // `is_file()` swallows the error and returns False
    };
    if !md.is_file() {
        return not_found();
    }

    let Ok(f) = tokio::fs::File::open(&target).await else {
        // The file vanished or lost read permission between the stat and the
        // open: Starlette re-stats inside FileResponse and raises, so this is
        // Python's `text/plain` 500.
        return internal();
    };

    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(mime));
    // The only header Python sets explicitly, and it is `public` on a route that
    // requires a bearer token -- so a shared proxy could in principle cache an
    // authenticated clip for five minutes. Deliberate upstream (clips are
    // immutable-by-name and shared across the whole paired group, and Caddy does
    // not cache); kept as-is rather than quietly tightened to `private`.
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("public, max-age=300"));
    headers.insert(header::CONTENT_LENGTH, HeaderValue::from(md.len()));
    if let Ok(t) = md.modified() {
        if let Ok(v) = HeaderValue::from_str(&http_date(t)) {
            headers.insert(header::LAST_MODIFIED, v);
        }
    }
    // Deliberately no `accept-ranges`: this does not honour `Range`, and
    // Starlette's `etag` is an md5 we cannot compute without a new dependency.
    // Advertising byte ranges we would then ignore is worse than not offering
    // them -- serving the whole representation for a `Range` request is legal,
    // and the in-repo clients (`arrayBuffer()` and the CLI's `_request_raw`)
    // never send one.

    // Streamed in 64 KiB chunks like Starlette, so a large hand-dropped clip is
    // not buffered whole. `content-length` above keeps the response unchunked.
    (headers, Body::from_stream(futures::stream::try_unfold(f, read_chunk))).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basename_matches_pathlib() {
        assert_eq!(basename("../../etc/evil.mp3"), "evil.mp3");
        assert_eq!(basename("/abs/x.mp3"), "x.mp3");
        assert_eq!(basename("a/"), "a");
        assert_eq!(basename("a/./"), "a");
        assert_eq!(basename(""), "");
        assert_eq!(basename("/"), "");
        assert_eq!(basename("."), "");
        assert_eq!(basename(".."), "..");
        assert_eq!(basename("a/.."), "..");
        // POSIX: a backslash is an ordinary character, not a separator.
        assert_eq!(basename("a\\b.mp3"), "a\\b.mp3");
    }

    #[test]
    fn suffix_and_stem_match_pathlib() {
        assert_eq!(suffix("mix.final.mp3"), ".mp3");
        assert_eq!(stem("mix.final.mp3"), "mix.final");
        assert_eq!(suffix("boo.ya.mp3"), ".mp3");
        assert_eq!(stem("boo.ya.mp3"), "boo.ya");
        // Leading dots are stripped first, so these have no suffix at all --
        // which is what keeps a file named `..mp3` out of the listing.
        assert_eq!(suffix(".mp3"), "");
        assert_eq!(stem(".mp3"), ".mp3");
        assert_eq!(suffix("..mp3"), "");
        assert_eq!(stem("..mp3"), "..mp3");
        assert_eq!(suffix("..a"), "");
        assert_eq!(stem("..a"), "..a");
        // A trailing dot IS a suffix; it is just never an audio type.
        assert_eq!(suffix("a."), ".");
        assert_eq!(stem("a."), "a");
        assert_eq!(suffix("a.."), ".");
        assert_eq!(stem("a.."), "a.");
        assert_eq!(suffix("a.b."), ".");
        assert_eq!(stem("a.b."), "a.b");
        assert_eq!(suffix("plain"), "");
        assert_eq!(stem("plain"), "plain");
        assert_eq!(suffix(".."), "");
        assert_eq!(stem(".."), "..");
        // Byte indexing must land on a UTF-8 boundary.
        assert_eq!(suffix("café."), ".");
        assert_eq!(stem("café."), "café");
        assert_eq!(suffix("café.mp3"), ".mp3");
        assert_eq!(stem("café.mp3"), "café");
    }

    #[test]
    fn sanitiser_substitutes_one_underscore_per_codepoint() {
        // `re.sub` works on codepoints: one `_` for `é`, not two for its bytes.
        assert_eq!(stored_name("café.mp3").unwrap(), "caf_.mp3");
        assert_eq!(stored_name("my song!.mp3").unwrap(), "my song_.mp3");
        assert_eq!(stored_name("a(1)-b_c.mp3").unwrap(), "a(1)-b_c.mp3");
        // Separators and shell surprises all collapse to `_` after the basename.
        assert_eq!(stored_name("../../etc/evil.mp3").unwrap(), "evil.mp3");
        assert_eq!(stored_name("/abs/x.mp3").unwrap(), "x.mp3");
        assert_eq!(stored_name("a\\b.mp3").unwrap(), "a_b.mp3");
        assert_eq!(stored_name("x;$'\".mp3").unwrap(), "x____.mp3");
    }

    #[test]
    fn strip_runs_after_substitution() {
        // A trailing space survives substitution and is then stripped...
        assert_eq!(stored_name("  clip.mp3  ").unwrap(), "clip.mp3");
        // ...but a trailing tab or newline became `_` first, so it survives the
        // strip, making the suffix `.mp3_`, which is not an audio type.
        assert!(stored_name("clip.mp3\t").is_none());
        assert!(stored_name("clip.mp3\n").is_none());
    }

    #[test]
    fn rejects_exactly_what_python_rejects() {
        assert!(stored_name("").is_none());
        assert!(stored_name("/").is_none());
        assert!(stored_name(".").is_none());
        assert!(stored_name("..").is_none());
        // Dotfiles can never be created, including an extension-only name.
        assert!(stored_name(".mp3").is_none());
        assert!(stored_name(".hidden.mp3").is_none());
        assert!(stored_name("clip.txt").is_none());
        assert!(stored_name("clip").is_none());
        assert!(stored_name("clip.mp3.part").is_none());
        // The extension check is case-insensitive; the stored case is kept.
        assert_eq!(stored_name("CLIP.MP3").unwrap(), "CLIP.MP3");
        assert_eq!(stored_name("clip.WeBm").unwrap(), "clip.WeBm");
    }

    #[test]
    fn cap_truncates_to_eighty_and_lowercases_the_extension() {
        let long = format!("{}.MP3", "A".repeat(81));
        let stored = stored_name(&long).unwrap();
        assert_eq!(stored.len(), MAX_NAME_LEN);
        // Truncated to 76 kept characters plus the LOWERCASED `.mp3`.
        assert_eq!(stored, format!("{}.mp3", "A".repeat(76)));

        // Exactly at the cap: untouched, so the extension keeps its case.
        let at_cap = format!("{}.MP3", "A".repeat(76));
        assert_eq!(at_cap.len(), MAX_NAME_LEN);
        assert_eq!(stored_name(&at_cap).unwrap(), at_cap);
    }

    #[test]
    fn too_large_uses_floor_division() {
        assert_eq!(too_large(5 * 1024 * 1024), "file too large (max 5 MB)");
        // Under 1 MiB the message really does read "(max 0 MB)".
        assert_eq!(too_large(500_000), "file too large (max 0 MB)");
        assert_eq!(too_large(6_000_000), "file too large (max 5 MB)");
    }

    #[test]
    fn allowed_extension_list_stays_in_step_with_the_map() {
        let mut keys: Vec<&str> = AUDIO_TYPES.iter().map(|(e, _)| *e).collect();
        keys.sort_unstable();
        assert_eq!(ALLOWED_EXTS, keys.join(", "));
    }

    #[test]
    fn audio_mime_is_the_single_source_of_truth() {
        assert_eq!(audio_mime(".ogg"), Some("audio/ogg"));
        assert_eq!(audio_mime(".mp3"), Some("audio/mpeg"));
        assert_eq!(audio_mime(".wav"), Some("audio/wav"));
        assert_eq!(audio_mime(".m4a"), Some("audio/mp4"));
        assert_eq!(audio_mime(".webm"), Some("audio/webm"));
        assert_eq!(audio_mime(".part"), None);
        // The lookup expects an already-lowercased suffix.
        assert_eq!(audio_mime(".MP3"), None);
    }

    #[test]
    fn containment_is_strict_ancestry_not_depth() {
        let d = Path::new("/srv/sounds");
        assert!(inside(d, Path::new("/srv/sounds/clip.mp3")));
        // `d in target.parents` is containment, so a nested path passes too.
        assert!(inside(d, Path::new("/srv/sounds/sub/clip.mp3")));
        // `d` is not among its own parents, nor is an ancestor or a sibling
        // whose name merely starts with the same characters.
        assert!(!inside(d, Path::new("/srv/sounds")));
        assert!(!inside(d, Path::new("/srv/sounds/")));
        assert!(!inside(d, Path::new("/srv")));
        assert!(!inside(d, Path::new("/srv/sounds-evil/clip.mp3")));
        assert!(!inside(d, Path::new("/etc/passwd")));
    }

    #[test]
    fn query_is_parsed_the_way_starlette_parses_it() {
        assert_eq!(query_param("name=clip.mp3", "name").as_deref(), Some("clip.mp3"));
        assert_eq!(query_param("name=my%20clip.mp3", "name").as_deref(), Some("my clip.mp3"));
        assert_eq!(query_param("name=my+clip.mp3", "name").as_deref(), Some("my clip.mp3"));
        // keep_blank_values=True: present-and-empty, which is a 400, not a 422.
        assert_eq!(query_param("name=", "name").as_deref(), Some(""));
        assert_eq!(query_param("name", "name").as_deref(), Some(""));
        // Absent: the 422.
        assert_eq!(query_param("other=1", "name"), None);
        assert_eq!(query_param("", "name"), None);
        // Last wins, like the dict comprehension behind QueryParams.
        assert_eq!(query_param("name=a.mp3&name=b.mp3", "name").as_deref(), Some("b.mp3"));
        // A malformed escape is left alone rather than raising.
        assert_eq!(query_param("name=a%zz.mp3", "name").as_deref(), Some("a%zz.mp3"));
        assert_eq!(query_param("name=a%4", "name").as_deref(), Some("a%4"));
    }

    #[test]
    fn lexical_absolute_collapses_without_touching_the_filesystem() {
        assert_eq!(
            lexical_absolute(Path::new("/srv/sounds/../sounds/x.mp3")),
            PathBuf::from("/srv/sounds/x.mp3")
        );
        assert_eq!(lexical_absolute(Path::new("/srv/./sounds")), PathBuf::from("/srv/sounds"));
        // The default `./sounds` is relative, which is the case that matters.
        assert!(lexical_absolute(Path::new("./sounds")).is_absolute());
    }

    #[test]
    fn expanduser_handles_only_a_bare_leading_tilde() {
        assert_eq!(expanduser("~", "/home/t"), PathBuf::from("/home/t"));
        assert_eq!(expanduser("~/sounds", "/home/t"), PathBuf::from("/home/t/sounds"));
        assert_eq!(expanduser("./sounds", "/home/t"), PathBuf::from("./sounds"));
        assert_eq!(expanduser("/srv/sounds", "/home/t"), PathBuf::from("/srv/sounds"));
        // No $HOME, or a `~user` form we cannot resolve: left untouched.
        assert_eq!(expanduser("~/sounds", ""), PathBuf::from("~/sounds"));
        assert_eq!(expanduser("~bob/sounds", "/home/t"), PathBuf::from("~bob/sounds"));
    }
}
