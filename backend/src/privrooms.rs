//! Named, password-protected private rooms -- the Rust port of
//! `backend/app/routes/private_rooms.py` plus `backend/app/private_rooms.py`.
//!
//! Three things here are load-bearing and easy to tidy away by accident, so
//! they are spelled out instead:
//!
//! * The stored password is `scrypt$N$r$p$salt$dk`, written by Python's
//!   `hashlib.scrypt`. Cargo.toml has no scrypt crate and this port may not add
//!   one, so RFC 7914 is implemented below over `sha2`/`hmac` and checked in the
//!   tests against vectors taken from `hashlib`. Without it, a room created on
//!   the Python backend could not be joined here at all.
//! * Every mutating half runs inside a per-room gate and re-checks what the
//!   cheap pre-checks already found. The pre-checks exist only to avoid paying
//!   for scrypt; the re-checks inside the gate are the actual guard.
//! * Writes commit before any socket is touched. The database is the source of
//!   truth, and nobody is disconnected on behalf of a write that may roll back.

use axum::{
    extract::{Query, Request, State},
    http::{header, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use chrono::{DateTime, Duration, NaiveDateTime, Utc};
use hmac::{Hmac, Mac};
use rand::Rng;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sha2::Sha256;
use sqlx::sqlite::SqliteRow;
use sqlx::{Row, SqliteConnection, SqlitePool};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Instant;

use crate::rooms::RoomManager;
use crate::{AppState, Caller};

type HmacSha256 = Hmac<Sha256>;

// --- constants -------------------------------------------------------------
// Named rather than inlined because each one appears twice: once in the guard
// and once interpolated into the message, so a changed cap changes the text.

const NAME_MAX: usize = 40;
const PW_MIN: usize = 6;
const PW_MAX: usize = 128;

const MAX_OWNED: i64 = 5;
const MAX_JOINED: i64 = 25;
const MAX_MEMBERS: i64 = 50;
const DIRECTORY_MAX: usize = 200;

/// Pydantic's field-level `max_length`, which is the *first* gate: a 500-char
/// name is a schema 422, a 100-char one is the "1-40 characters" 422.
const FIELD_NAME_MAX: usize = 200;
const FIELD_PW_MAX: usize = 1024;

const SCRYPT_N: usize = 1 << 14;
const SCRYPT_R: usize = 8;
const SCRYPT_P: usize = 5;
const SCRYPT_DKLEN: usize = 32;
/// Widest N `verify_password` will accept from a stored string.
const SCRYPT_N_MAX: usize = 1 << 20;

const KDF_CONCURRENCY: usize = 2;
const KDF_OPS_PER_MIN: usize = 10;

const FAILS_PER_PAIR: i64 = 5;
const FAILS_PER_USER: i64 = 20;
const FAILS_PER_ROOM: i64 = 50;
const PAIR_WINDOW_MIN: i64 = 15;
const USER_WINDOW_MIN: i64 = 60;
const ROOM_WINDOW_MIN: i64 = 60;
const ATTEMPT_RETENTION_HOURS: i64 = 24;

const KDF_BUSY: &str = "slow down — too many room requests; try again in a minute";
const NO_SUCH_ROOM: &str = "no such room";
const NOT_OWNER: &str = "only the room owner can do that";
const DUPLICATE_NAME: &str = "a room with that name already exists";
const BAD_ROOM_ID: &str = "roomId must be r_ followed by 22 url-safe characters";

/// The two `Field(pattern=...)` sources, spelled the way the schema spells
/// them: pydantic quotes the pattern verbatim into its message, so these are
/// wire contract and not documentation. `valid_room_id` and `valid_user_id`
/// below are the same two patterns as predicates.
const ROOM_ID_PATTERN: &str = r"^r_[A-Za-z0-9_-]{22}$";
const USER_ID_PATTERN: &str =
    r"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$";

// --- small helpers ---------------------------------------------------------

fn err(code: StatusCode, msg: &str) -> Response {
    (code, Json(json!({ "detail": msg }))).into_response()
}

/// One pydantic error entry, scrubbed to the three keys `app/main.py`'s
/// `_scrub_422` keeps, in its order. `input`, `ctx` and `url` are deliberately
/// absent: `input` would carry the rejected password straight back out.
#[derive(Debug, Serialize)]
struct Verr {
    loc: Vec<String>,
    msg: String,
    #[serde(rename = "type")]
    kind: &'static str,
}

/// The 422 body a *pydantic* failure produces: a list of entries, where `err`
/// builds the single-string envelope. An explicit `raise HTTPException(422,
/// "...")` in the Python route keeps the string, so both shapes answer with
/// this status and the client tells them apart by the type of `detail`.
#[derive(Serialize)]
struct Errors {
    detail: Vec<Verr>,
}

fn err422(detail: Vec<Verr>) -> Response {
    (StatusCode::UNPROCESSABLE_ENTITY, Json(Errors { detail })).into_response()
}

fn ve(loc: Vec<String>, kind: &'static str, msg: impl Into<String>) -> Verr {
    Verr { loc, msg: msg.into(), kind }
}

/// `["body", <field>]`, where all but one of this module's pydantic errors sit.
fn bloc(field: &str) -> Vec<String> {
    vec!["body".to_string(), field.to_string()]
}

/// A throttle refusal carries the seconds to wait in a header as well as the
/// rounded-up minutes in the message; the client reads both.
fn err_retry(code: StatusCode, msg: &str, retry_secs: i64) -> Response {
    let mut res = err(code, msg);
    let value = HeaderValue::from_str(&retry_secs.to_string()).expect("digits are a header value");
    res.headers_mut().insert(header::RETRY_AFTER, value);
    res
}

fn db_err(e: sqlx::Error) -> Response {
    err(StatusCode::INTERNAL_SERVER_ERROR, &format!("db error: {e}"))
}

/// SQLite holds these naive, in UTC, the way SQLAlchemy wrote them.
fn sqlite_dt(t: DateTime<Utc>) -> String {
    t.format("%Y-%m-%d %H:%M:%S%.6f").to_string()
}

fn parse_dt(raw: &str) -> Option<NaiveDateTime> {
    NaiveDateTime::parse_from_str(raw, "%Y-%m-%d %H:%M:%S%.f")
        .or_else(|_| NaiveDateTime::parse_from_str(raw, "%Y-%m-%dT%H:%M:%S%.f"))
        .ok()
}

/// The wire form: seconds precision with an explicit offset, never a "Z" and
/// never microseconds (Python's `isoformat(timespec="seconds")` on a UTC value).
fn iso(raw: &str) -> String {
    match parse_dt(raw) {
        Some(dt) => format!("{}+00:00", dt.format("%Y-%m-%dT%H:%M:%S")),
        // A row we cannot parse is not worth a 500: pass the stored text
        // through rather than invent a timestamp.
        None => raw.to_string(),
    }
}

/// SQLite reports a broken UNIQUE at statement time; SQLAlchemy raised it at
/// commit. Both paths are checked at the call sites, so the handler answers 409
/// wherever the race lands.
fn is_unique_violation(e: &sqlx::Error) -> bool {
    let Some(d) = e.as_database_error() else { return false };
    // SQLITE_CONSTRAINT_UNIQUE is the extended code 2067, which sqlx reports as
    // a unique violation; the message check is belt and braces, because
    // misreading this one would 500 where the Python answers 409.
    d.is_unique_violation() || d.message().contains("UNIQUE constraint failed")
}

fn valid_room_id(s: &str) -> bool {
    s.len() == 24
        && s.starts_with("r_")
        && s[2..].bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// Lowercase hyphenated hex only: an uppercase uuid is a 422, not a 404.
fn valid_user_id(s: &str) -> bool {
    let mut parts = s.split('-');
    for width in [8, 4, 4, 4, 12] {
        match parts.next() {
            Some(p)
                if p.len() == width
                    && p.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) => {}
            _ => return false,
        }
    }
    parts.next().is_none()
}

/// "r_" + 22 url-safe characters, the shape `secrets.token_urlsafe(16)` makes.
fn new_room_id() -> String {
    let mut bytes = [0u8; 16];
    rand::thread_rng().fill(&mut bytes);
    format!("r_{}", b64e(&bytes))
}

// --- unicode seams ---------------------------------------------------------
// Cargo.toml has no normalisation crate and this port may not add one, so the
// three places Python reaches for `unicodedata` are named functions here: every
// one of them is exact for ASCII, which is what the name and password rules
// were written for, and approximate above it. The port notes carry the detail.

/// Python's `unicodedata.normalize("NFC", s)`. Every scalar below U+0080 is
/// already NFC, so this is exact for ASCII and a no-op elsewhere.
fn nfc(s: &str) -> String {
    s.to_string()
}

/// Python's `unicodedata.normalize("NFKC", name).casefold()`. `to_lowercase`
/// is full Unicode lowercase and agrees with casefold across ASCII; the NFKC
/// folding of compatibility forms (full-width, ligatures) is not available
/// here, so those variants do not collide the way they do in Python.
fn room_name_key(name: &str) -> String {
    name.to_lowercase()
}

/// Python's `str.isprintable()`: false for control and format characters, and
/// for any separator other than a plain space.
fn printable(c: char) -> bool {
    c == ' ' || !(c.is_control() || c.is_whitespace() || is_format(c))
}

/// General category Cf, as far as a pasted name carries it.
fn is_format(c: char) -> bool {
    matches!(c as u32,
        0x00AD | 0x0600..=0x0605 | 0x061C | 0x06DD | 0x070F | 0x180E
        | 0x200B..=0x200F | 0x202A..=0x202E | 0x2060..=0x2064 | 0x206A..=0x206F
        | 0xFEFF | 0xFFF9..=0xFFFB)
}

/// General categories Mn/Mc/Me, as far as the Zalgo filter needs them: a list
/// of the combining ranges names actually arrive with, not a table lookup. An
/// exotic mark can slip through, which costs a less tidy name and never a
/// wrong decision.
fn is_mark(c: char) -> bool {
    matches!(c as u32,
        0x0300..=0x036F | 0x0483..=0x0489 | 0x0591..=0x05BD | 0x05BF
        | 0x05C1..=0x05C2 | 0x05C4..=0x05C5 | 0x05C7 | 0x0610..=0x061A
        | 0x064B..=0x065F | 0x0670 | 0x06D6..=0x06DC | 0x06DF..=0x06E4
        | 0x06E7..=0x06E8 | 0x06EA..=0x06ED | 0x0711 | 0x0730..=0x074A
        | 0x07A6..=0x07B0 | 0x07EB..=0x07F3 | 0x0816..=0x0819 | 0x081B..=0x0823
        | 0x0825..=0x0827 | 0x0829..=0x082D | 0x0900..=0x0903 | 0x093A..=0x094F
        | 0x0951..=0x0957 | 0x0E31 | 0x0E34..=0x0E3A | 0x0E47..=0x0E4E
        | 0x1AB0..=0x1AFF | 0x1DC0..=0x1DFF | 0x20D0..=0x20F0 | 0xFE00..=0xFE0F
        | 0xFE20..=0xFE2F)
}

/// Python's "at least one character whose category starts with L, N, P or S".
/// The complement of that set is separators, marks, and the control and format
/// characters the step above has already turned into spaces -- so this is that
/// test, inverted.
fn real_character(c: char) -> bool {
    !c.is_whitespace() && !c.is_control() && !is_mark(c) && !is_format(c)
}

/// The five-step pipeline, in Python's order. Order matters: folding whitespace
/// before the length check is what makes "  a  " a one-character name.
fn clean_room_name(raw: &str) -> Option<String> {
    let normalised = nfc(raw);
    // Non-printables become a SPACE rather than disappearing, so "a\x07b" is
    // two words and not "ab".
    let printable_only: String =
        normalised.chars().map(|c| if printable(c) { c } else { ' ' }).collect();

    // Drop combining marks past the second in a run. The counter resets on a
    // non-mark, and a skipped mark does not reset it.
    let mut kept = String::with_capacity(printable_only.len());
    let mut combo = 0;
    for c in printable_only.chars() {
        if is_mark(c) {
            combo += 1;
            if combo > 2 {
                continue;
            }
        } else {
            combo = 0;
        }
        kept.push(c);
    }

    let folded = kept.split_whitespace().collect::<Vec<&str>>().join(" ");
    if folded.is_empty() || folded.chars().count() > NAME_MAX {
        return None;
    }
    if !folded.chars().any(real_character) {
        return None;
    }
    // NFKC can expand, so a 40-character name can still overflow the key
    // column. Unreachable without NFKC, kept so the rule stays visible.
    if room_name_key(&folded).chars().count() > 255 {
        return None;
    }
    Some(folded)
}

/// Length in Unicode scalars after normalisation -- not bytes, or a six
/// character non-ASCII password would be refused.
fn password_ok(pw: &str) -> bool {
    (PW_MIN..=PW_MAX).contains(&pw.chars().count())
}

// --- base64url -------------------------------------------------------------
// Unpadded url-safe base64, the shape `base64.urlsafe_b64encode().rstrip("=")`
// writes. auth.rs has the encoder already but keeps it private, and this port
// may not edit that file.

fn b64e(bytes: &[u8]) -> String {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        let take = chunk.len() + 1;
        for i in 0..take {
            out.push(T[((n >> (18 - 6 * i)) & 0x3F) as usize] as char);
        }
    }
    out
}

/// None for anything Python's `urlsafe_b64decode` would raise on, which the
/// caller turns into "this stored hash does not verify" rather than an error.
fn b64d(s: &str) -> Option<Vec<u8>> {
    let mut acc: u32 = 0;
    let mut bits: u32 = 0;
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    for c in s.chars() {
        if c == '=' {
            break;
        }
        let v = match c {
            'A'..='Z' => c as u32 - 'A' as u32,
            'a'..='z' => c as u32 - 'a' as u32 + 26,
            '0'..='9' => c as u32 - '0' as u32 + 52,
            '-' => 62,
            '_' => 63,
            _ => return None,
        };
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((acc >> bits) & 0xFF) as u8);
        }
    }
    // A trailing group of a single character encodes no whole byte.
    if bits >= 6 {
        return None;
    }
    Some(out)
}

// --- scrypt (RFC 7914) -----------------------------------------------------
// Only needed because the hashes in the database were written by Python and the
// dependency set has no scrypt. The tests pin it to `hashlib.scrypt` output.

/// PBKDF2-HMAC-SHA256 with a single iteration, which is all scrypt asks for.
fn pbkdf2_sha256_once(pw: &[u8], salt: &[u8], out: &mut [u8]) {
    let mut block: u32 = 1;
    let mut off = 0;
    while off < out.len() {
        let mut mac = HmacSha256::new_from_slice(pw).expect("hmac takes any key length");
        mac.update(salt);
        mac.update(&block.to_be_bytes());
        let t = mac.finalize().into_bytes();
        let take = (out.len() - off).min(t.len());
        out[off..off + take].copy_from_slice(&t[..take]);
        off += take;
        block += 1;
    }
}

/// The Salsa20/8 core: eight rounds, then add the input back.
fn salsa20_8(block: &mut [u8; 64]) {
    let mut x = [0u32; 16];
    for (i, chunk) in block.chunks(4).enumerate() {
        x[i] = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
    }
    let input = x;
    for _ in 0..4 {
        x[4] ^= x[0].wrapping_add(x[12]).rotate_left(7);
        x[8] ^= x[4].wrapping_add(x[0]).rotate_left(9);
        x[12] ^= x[8].wrapping_add(x[4]).rotate_left(13);
        x[0] ^= x[12].wrapping_add(x[8]).rotate_left(18);
        x[9] ^= x[5].wrapping_add(x[1]).rotate_left(7);
        x[13] ^= x[9].wrapping_add(x[5]).rotate_left(9);
        x[1] ^= x[13].wrapping_add(x[9]).rotate_left(13);
        x[5] ^= x[1].wrapping_add(x[13]).rotate_left(18);
        x[14] ^= x[10].wrapping_add(x[6]).rotate_left(7);
        x[2] ^= x[14].wrapping_add(x[10]).rotate_left(9);
        x[6] ^= x[2].wrapping_add(x[14]).rotate_left(13);
        x[10] ^= x[6].wrapping_add(x[2]).rotate_left(18);
        x[3] ^= x[15].wrapping_add(x[11]).rotate_left(7);
        x[7] ^= x[3].wrapping_add(x[15]).rotate_left(9);
        x[11] ^= x[7].wrapping_add(x[3]).rotate_left(13);
        x[15] ^= x[11].wrapping_add(x[7]).rotate_left(18);
        x[1] ^= x[0].wrapping_add(x[3]).rotate_left(7);
        x[2] ^= x[1].wrapping_add(x[0]).rotate_left(9);
        x[3] ^= x[2].wrapping_add(x[1]).rotate_left(13);
        x[0] ^= x[3].wrapping_add(x[2]).rotate_left(18);
        x[6] ^= x[5].wrapping_add(x[4]).rotate_left(7);
        x[7] ^= x[6].wrapping_add(x[5]).rotate_left(9);
        x[4] ^= x[7].wrapping_add(x[6]).rotate_left(13);
        x[5] ^= x[4].wrapping_add(x[7]).rotate_left(18);
        x[11] ^= x[10].wrapping_add(x[9]).rotate_left(7);
        x[8] ^= x[11].wrapping_add(x[10]).rotate_left(9);
        x[9] ^= x[8].wrapping_add(x[11]).rotate_left(13);
        x[10] ^= x[9].wrapping_add(x[8]).rotate_left(18);
        x[12] ^= x[15].wrapping_add(x[14]).rotate_left(7);
        x[13] ^= x[12].wrapping_add(x[15]).rotate_left(9);
        x[14] ^= x[13].wrapping_add(x[12]).rotate_left(13);
        x[15] ^= x[14].wrapping_add(x[13]).rotate_left(18);
    }
    for (i, chunk) in block.chunks_mut(4).enumerate() {
        chunk.copy_from_slice(&x[i].wrapping_add(input[i]).to_le_bytes());
    }
}

/// scryptBlockMix on 2r 64-byte blocks: the even outputs first, then the odd.
fn block_mix(r: usize, b: &[u8], out: &mut [u8]) {
    let two_r = 2 * r;
    let mut x = [0u8; 64];
    x.copy_from_slice(&b[(two_r - 1) * 64..two_r * 64]);
    for i in 0..two_r {
        for (j, xj) in x.iter_mut().enumerate() {
            *xj ^= b[i * 64 + j];
        }
        salsa20_8(&mut x);
        let dst = if i % 2 == 0 { (i / 2) * 64 } else { (r + i / 2) * 64 };
        out[dst..dst + 64].copy_from_slice(&x);
    }
}

/// Integerify: the last 64-byte block read as a little-endian integer.
fn integerify(r: usize, block: &[u8]) -> u64 {
    let off = (2 * r - 1) * 64;
    let mut n = [0u8; 8];
    n.copy_from_slice(&block[off..off + 8]);
    u64::from_le_bytes(n)
}

/// scryptROMix in place. `v` is the N-block scratch array (the memory hardness)
/// and `tmp` one block; both are passed in so the p iterations reuse them.
fn romix(r: usize, n: usize, block: &mut [u8], v: &mut [u8], tmp: &mut [u8]) {
    let bl = 128 * r;
    for i in 0..n {
        v[i * bl..(i + 1) * bl].copy_from_slice(block);
        block_mix(r, block, tmp);
        block.copy_from_slice(tmp);
    }
    for _ in 0..n {
        let j = (integerify(r, block) % n as u64) as usize;
        for k in 0..bl {
            tmp[k] = block[k] ^ v[j * bl + k];
        }
        block_mix(r, tmp, block);
    }
}

fn scrypt_raw(pw: &[u8], salt: &[u8], n: usize, r: usize, p: usize, dklen: usize) -> Vec<u8> {
    let bl = 128 * r;
    let mut b = vec![0u8; p * bl];
    pbkdf2_sha256_once(pw, salt, &mut b);
    let mut v = vec![0u8; n * bl];
    let mut tmp = vec![0u8; bl];
    for i in 0..p {
        romix(r, n, &mut b[i * bl..(i + 1) * bl], &mut v, &mut tmp);
    }
    let mut dk = vec![0u8; dklen];
    pbkdf2_sha256_once(pw, &b, &mut dk);
    dk
}

/// `==` on slices short-circuits; a timing difference here would leak how much
/// of a guess was right. Python uses `hmac.compare_digest` for the same reason.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= *x ^ *y;
    }
    diff == 0
}

/// At most this many scrypt runs at once, process-wide: each wants 16 MiB and a
/// core, so an unbounded fan-in would be a memory bomb. Python's
/// `threading.BoundedSemaphore(2)` around the same call.
static KDF_SLOTS: LazyLock<tokio::sync::Semaphore> =
    LazyLock::new(|| tokio::sync::Semaphore::new(KDF_CONCURRENCY));

/// Runs scrypt on a blocking thread behind the concurrency gate. None only if
/// that thread died, which must never read as a correct password.
async fn kdf(pw: Vec<u8>, salt: Vec<u8>, n: usize, r: usize, p: usize) -> Option<Vec<u8>> {
    let _permit = KDF_SLOTS.acquire().await;
    tokio::task::spawn_blocking(move || scrypt_raw(&pw, &salt, n, r, p, SCRYPT_DKLEN))
        .await
        .ok()
}

async fn hash_password(pw: &str) -> Option<String> {
    let mut salt = [0u8; 16];
    rand::thread_rng().fill(&mut salt);
    let dk = kdf(nfc(pw).into_bytes(), salt.to_vec(), SCRYPT_N, SCRYPT_R, SCRYPT_P).await?;
    Some(format!(
        "scrypt${SCRYPT_N}${SCRYPT_R}${SCRYPT_P}${}${}",
        b64e(&salt),
        b64e(&dk)
    ))
}

/// Parses the stored string and compares. Any malformed field is `false`, not
/// an error: a corrupt row means nobody can join, not that the endpoint 500s.
async fn verify_password(pw: &str, stored: &str) -> bool {
    let parts: Vec<&str> = stored.split('$').collect();
    if parts.len() != 6 || parts[0] != "scrypt" {
        return false;
    }
    let (Ok(n), Ok(r), Ok(p)) = (
        parts[1].parse::<usize>(),
        parts[2].parse::<usize>(),
        parts[3].parse::<usize>(),
    ) else {
        return false;
    };
    if !(2..=SCRYPT_N_MAX).contains(&n) || !n.is_power_of_two() {
        return false;
    }
    if !(1..=16).contains(&r) || !(1..=16).contains(&p) {
        return false;
    }
    let (Some(salt), Some(expected)) = (b64d(parts[4]), b64d(parts[5])) else {
        return false;
    };
    if expected.len() != SCRYPT_DKLEN {
        return false;
    }
    match kdf(nfc(pw).into_bytes(), salt, n, r, p).await {
        Some(actual) => constant_time_eq(&actual, &expected),
        None => false,
    }
}

// --- in-process budgets ----------------------------------------------------

/// Per-user scrypt budget: at most `KDF_OPS_PER_MIN` accepted calls a minute,
/// counted on a monotonic clock. In-process and not shared between instances,
/// exactly like Python's deque-per-user -- it is a cost brake, not an
/// authorisation decision, so a restart forgetting it is fine.
static KDF_BUDGET: LazyLock<Mutex<HashMap<String, VecDeque<Instant>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// True if the caller may spend a KDF slot; the token is consumed only then.
fn kdf_budget_ok(user_id: &str) -> bool {
    let now = Instant::now();
    let Ok(mut budget) = KDF_BUDGET.lock() else {
        // Poisoned means another request panicked holding it. Refusing the
        // budget is the safe side of that.
        return false;
    };
    let dq = budget.entry(user_id.to_string()).or_default();
    while dq.front().is_some_and(|t| now.saturating_duration_since(*t).as_secs_f64() > 60.0) {
        dq.pop_front();
    }
    if dq.len() >= KDF_OPS_PER_MIN {
        return false;
    }
    dq.push_back(now);
    // Only ever pruned once the map is large, so the common path stays O(1).
    if budget.len() > 10_000 {
        budget.retain(|_, v| !v.is_empty());
    }
    true
}

/// Per-room gates, serialising the mutating half of join, leave, password, kick
/// and delete. rename and unban deliberately take none. Python keeps these in a
/// WeakValueDictionary; here an unused gate is swept when the map grows.
static GATES: LazyLock<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn gate(room_id: &str) -> Arc<tokio::sync::Mutex<()>> {
    // A poisoned map still holds usable gates, and refusing to hand one out
    // would fail every write for the life of the process.
    let mut gates = match GATES.lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };
    if gates.len() > 1024 {
        gates.retain(|_, g| Arc::strong_count(g) > 1);
    }
    gates
        .entry(room_id.to_string())
        .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
        .clone()
}

/// Closing someone else's websocket is the one thing this module cannot do:
/// `rooms::RoomManager` exposes send, count and broadcast but no `evict`, and
/// this port may not edit that file. Each call below marks where Python closes
/// sockets, so adding `RoomManager::evict(room_id, code, reason, user_id,
/// keep_user_id)` is the only change left. Until then a kicked member keeps a
/// live socket in a room they are no longer in -- the database is still right,
/// and the next reconnect is refused.
fn evict_unavailable(room_id: &str, code: u16, reason: &str, whose: &str) {
    tracing::warn!(
        "private room {room_id}: would close {whose} socket(s) with {code} \"{reason}\", \
         but RoomManager has no evict yet"
    );
}

// --- wire shapes -----------------------------------------------------------
// Structs rather than json! because serde writes fields in declaration order
// and serde_json's maps sort: these are the Python's key orders, kept.

/// What the Python's `room_out()` builds. There is deliberately no field for
/// `password_hash`, `name_key` or `updated_at`, so none of them can reach the
/// wire however the row was loaded.
#[derive(Serialize)]
struct RoomOut {
    id: String,
    name: String,
    #[serde(rename = "ownerUserId")]
    owner_user_id: String,
    #[serde(rename = "ownerHandle")]
    owner_handle: String,
    #[serde(rename = "ownerName")]
    owner_name: String,
    online: i64,
    #[serde(rename = "memberCount")]
    member_count: i64,
    /// Always present, `null` when the caller has no membership row -- and it
    /// does report "banned", which the directory is meant to show.
    role: Option<String>,
    #[serde(rename = "createdAt")]
    created_at: String,
}

#[derive(Serialize)]
struct Lobby {
    id: &'static str,
    name: &'static str,
    online: i64,
}

#[derive(Serialize)]
struct Limits {
    #[serde(rename = "nameMax")]
    name_max: usize,
    #[serde(rename = "passwordMin")]
    password_min: usize,
    #[serde(rename = "passwordMax")]
    password_max: usize,
    #[serde(rename = "maxOwned")]
    max_owned: i64,
    #[serde(rename = "maxJoined")]
    max_joined: i64,
    #[serde(rename = "maxMembers")]
    max_members: i64,
}

#[derive(Serialize)]
struct DirectoryResponse {
    lobby: Lobby,
    rooms: Vec<RoomOut>,
    limits: Limits,
}

#[derive(Serialize)]
struct RoomResponse {
    room: RoomOut,
    already: bool,
}

#[derive(Serialize)]
struct LeaveResponse {
    ok: bool,
    deleted: bool,
    #[serde(rename = "newOwnerHandle")]
    new_owner_handle: Option<String>,
}

#[derive(Serialize)]
struct MemberOut {
    #[serde(rename = "userId")]
    user_id: String,
    handle: String,
    #[serde(rename = "displayName")]
    display_name: String,
    #[serde(rename = "avatarUrl")]
    avatar_url: String,
    role: String,
    online: bool,
    #[serde(rename = "joinedAt")]
    joined_at: String,
}

#[derive(Serialize)]
struct BannedOut {
    #[serde(rename = "userId")]
    user_id: String,
    handle: String,
    #[serde(rename = "displayName")]
    display_name: String,
    #[serde(rename = "avatarUrl")]
    avatar_url: String,
}

#[derive(Serialize)]
struct MembersResponse {
    #[serde(rename = "roomId")]
    room_id: String,
    members: Vec<MemberOut>,
    /// `[]` for a member, populated only for the owner -- never omitted.
    banned: Vec<BannedOut>,
}

#[derive(Deserialize)]
struct MembersQuery {
    #[serde(rename = "roomId", default)]
    room_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateRequest {
    name: String,
    password: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JoinRequest {
    #[serde(rename = "roomId")]
    room_id: String,
    password: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RoomRefRequest {
    #[serde(rename = "roomId")]
    room_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RenameRequest {
    #[serde(rename = "roomId")]
    room_id: String,
    name: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PasswordRequest {
    #[serde(rename = "roomId")]
    room_id: String,
    password: String,
    #[serde(rename = "signOutOthers", default)]
    sign_out_others: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RoomUserRequest {
    #[serde(rename = "roomId")]
    room_id: String,
    #[serde(rename = "userId")]
    user_id: String,
}

// --- request validation ----------------------------------------------------
//
// Hand-rolled rather than derived. A schema failure has to come back in
// pydantic's `{loc, msg, type}` shape, and serde's one opaque error cannot
// carry it: the client reads `detail[].loc` to highlight the offending field.
// `deny_unknown_fields` and the renames on the structs above are Python's
// `extra="forbid"` and its camelCase keys; the derived impl is now only
// exercised by the unit tests, which pin it against the readers below.
//
// Field order is wire contract. pydantic-core reports the declared fields in
// declaration order and the `extra="forbid"` keys after all of them, so every
// reader collects each field in turn -- never stopping at the first failure --
// and calls `deny_extra` last.

/// The body as a JSON object, in pydantic's voice.
///
/// An empty body and a literal `null` are both "Field required": FastAPI turns
/// both into `body = None` and then validates the model against that. Valid
/// JSON that is not an object is `model_attributes_type`.
fn body_object(bytes: &[u8]) -> Result<Map<String, Value>, Vec<Verr>> {
    let at_body = || vec!["body".to_string()];
    if bytes.is_empty() {
        return Err(vec![ve(at_body(), "missing", "Field required")]);
    }
    match serde_json::from_slice::<Value>(bytes) {
        Ok(Value::Object(o)) => Ok(o),
        Ok(Value::Null) => Err(vec![ve(at_body(), "missing", "Field required")]),
        Ok(_) => Err(vec![ve(
            at_body(),
            "model_attributes_type",
            "Input should be a valid dictionary or object to extract fields from",
        )]),
        // FastAPI puts the character offset of the fault in `loc` here; serde
        // reports a line and column that CPython's decoder does not agree with
        // anyway, so this stops at `["body"]` -- the one known gap in the shape.
        Err(_) => Err(vec![ve(at_body(), "json_invalid", "JSON decode error")]),
    }
}

/// Reads a request body and hands it to one of the readers below.
///
/// The 64 KiB cap has no counterpart in the Python, so its refusal keeps the
/// single-string envelope rather than inventing a pydantic entry for it.
// The Err arm carries a fully-built axum Response (~128 bytes), which trips
// clippy::result_large_err. Boxing it would churn every `?` call site in this
// module to buy nothing: the Err path is a request that is already being
// rejected, so one extra move on a cold path is not worth the indirection.
#[allow(clippy::result_large_err)]
async fn read_request<T>(
    req: Request,
    parse: fn(&Map<String, Value>) -> Result<T, Vec<Verr>>,
) -> Result<T, Response> {
    let bytes = match axum::body::to_bytes(req.into_body(), 64 * 1024).await {
        Ok(b) => b,
        Err(_) => return Err(err(StatusCode::UNPROCESSABLE_ENTITY, "body too large")),
    };
    let obj = body_object(&bytes).map_err(err422)?;
    parse(&obj).map_err(err422)
}

/// Pydantic's lax `str`: a string and nothing else. A number, a boolean and an
/// explicit `null` are all `string_type` -- lax mode coerces none of them.
fn take_str(obj: &Map<String, Value>, field: &str, errs: &mut Vec<Verr>) -> Option<String> {
    match obj.get(field) {
        Some(Value::String(s)) => Some(s.clone()),
        None => {
            errs.push(ve(bloc(field), "missing", "Field required"));
            None
        }
        Some(_) => {
            errs.push(ve(bloc(field), "string_type", "Input should be a valid string"));
            None
        }
    }
}

/// `Field(max_length=N)`. Counted in characters, as pydantic counts them: a
/// byte count would refuse a legal non-ASCII name.
fn take_capped(
    obj: &Map<String, Value>,
    field: &str,
    max: usize,
    errs: &mut Vec<Verr>,
) -> Option<String> {
    let s = take_str(obj, field, errs)?;
    if s.chars().count() > max {
        errs.push(ve(
            bloc(field),
            "string_too_long",
            format!("String should have at most {max} characters"),
        ));
        return None;
    }
    Some(s)
}

/// `Field(pattern=...)`. `ok` is the pattern as a predicate and `pattern` is
/// the source text pydantic quotes back; the two must describe the same set.
fn take_pattern(
    obj: &Map<String, Value>,
    field: &str,
    pattern: &str,
    ok: fn(&str) -> bool,
    errs: &mut Vec<Verr>,
) -> Option<String> {
    let s = take_str(obj, field, errs)?;
    if !ok(&s) {
        errs.push(ve(
            bloc(field),
            "string_pattern_mismatch",
            format!("String should match pattern '{pattern}'"),
        ));
        return None;
    }
    Some(s)
}

fn take_room_id(obj: &Map<String, Value>, errs: &mut Vec<Verr>) -> Option<String> {
    take_pattern(obj, "roomId", ROOM_ID_PATTERN, valid_room_id, errs)
}

fn take_user_id(obj: &Map<String, Value>, errs: &mut Vec<Verr>) -> Option<String> {
    take_pattern(obj, "userId", USER_ID_PATTERN, valid_user_id, errs)
}

/// Pydantic's lax `bool`, which is wider than serde's. Absent is the model
/// default; a JSON boolean is itself; pydantic-core also reads its own set of
/// words (case-insensitively, with no trimming) and an integral 0 or 1. A value
/// of the right *kind* but the wrong value is `bool_parsing`; a null, a
/// fractional or unrepresentable number, a list or an object is `bool_type`.
fn take_bool(obj: &Map<String, Value>, field: &str, errs: &mut Vec<Verr>) -> bool {
    const TRUE: [&str; 6] = ["1", "on", "t", "true", "y", "yes"];
    const FALSE: [&str; 6] = ["0", "off", "f", "false", "n", "no"];
    const PARSING: (&str, &str) =
        ("bool_parsing", "Input should be a valid boolean, unable to interpret input");
    const TYPE: (&str, &str) = ("bool_type", "Input should be a valid boolean");

    let Some(v) = obj.get(field) else { return false };
    let (kind, msg) = match v {
        Value::Bool(b) => return *b,
        Value::String(s) => {
            let lower = s.to_lowercase();
            if TRUE.contains(&lower.as_str()) {
                return true;
            }
            if FALSE.contains(&lower.as_str()) {
                return false;
            }
            PARSING
        }
        // A whole float is read as the integer it is; a fraction, or a
        // magnitude no `int` could hold, is not a boolean at all.
        Value::Number(_) => {
            let n = v.as_i64().or_else(|| {
                v.as_f64().filter(|f| f.fract() == 0.0 && f.abs() < 9.2e18).map(|f| f as i64)
            });
            match n {
                Some(1) => return true,
                Some(0) => return false,
                Some(_) => PARSING,
                None => TYPE,
            }
        }
        _ => TYPE,
    };
    errs.push(ve(bloc(field), kind, msg));
    false
}

/// `extra="forbid"`. Always called last, because pydantic reports the declared
/// fields before the unknown keys.
fn deny_extra(obj: &Map<String, Value>, known: &[&str], errs: &mut Vec<Verr>) {
    for k in obj.keys() {
        if !known.contains(&k.as_str()) {
            errs.push(ve(bloc(k), "extra_forbidden", "Extra inputs are not permitted"));
        }
    }
}

/// Every reader ends this way: no errors means every field produced a value, so
/// the `expect`s below are the invariant written down rather than a fallback.
const PARSED: &str = "a body with no errors has every field";

fn create_body(obj: &Map<String, Value>) -> Result<CreateRequest, Vec<Verr>> {
    let mut errs = Vec::new();
    let name = take_capped(obj, "name", FIELD_NAME_MAX, &mut errs);
    let password = take_capped(obj, "password", FIELD_PW_MAX, &mut errs);
    deny_extra(obj, &["name", "password"], &mut errs);
    if !errs.is_empty() {
        return Err(errs);
    }
    Ok(CreateRequest { name: name.expect(PARSED), password: password.expect(PARSED) })
}

fn join_body(obj: &Map<String, Value>) -> Result<JoinRequest, Vec<Verr>> {
    let mut errs = Vec::new();
    let room_id = take_room_id(obj, &mut errs);
    let password = take_capped(obj, "password", FIELD_PW_MAX, &mut errs);
    deny_extra(obj, &["roomId", "password"], &mut errs);
    if !errs.is_empty() {
        return Err(errs);
    }
    Ok(JoinRequest { room_id: room_id.expect(PARSED), password: password.expect(PARSED) })
}

fn room_ref_body(obj: &Map<String, Value>) -> Result<RoomRefRequest, Vec<Verr>> {
    let mut errs = Vec::new();
    let room_id = take_room_id(obj, &mut errs);
    deny_extra(obj, &["roomId"], &mut errs);
    if !errs.is_empty() {
        return Err(errs);
    }
    Ok(RoomRefRequest { room_id: room_id.expect(PARSED) })
}

fn rename_body(obj: &Map<String, Value>) -> Result<RenameRequest, Vec<Verr>> {
    let mut errs = Vec::new();
    let room_id = take_room_id(obj, &mut errs);
    let name = take_capped(obj, "name", FIELD_NAME_MAX, &mut errs);
    deny_extra(obj, &["roomId", "name"], &mut errs);
    if !errs.is_empty() {
        return Err(errs);
    }
    Ok(RenameRequest { room_id: room_id.expect(PARSED), name: name.expect(PARSED) })
}

fn password_body(obj: &Map<String, Value>) -> Result<PasswordRequest, Vec<Verr>> {
    let mut errs = Vec::new();
    let room_id = take_room_id(obj, &mut errs);
    let password = take_capped(obj, "password", FIELD_PW_MAX, &mut errs);
    let sign_out_others = take_bool(obj, "signOutOthers", &mut errs);
    deny_extra(obj, &["roomId", "password", "signOutOthers"], &mut errs);
    if !errs.is_empty() {
        return Err(errs);
    }
    Ok(PasswordRequest {
        room_id: room_id.expect(PARSED),
        password: password.expect(PARSED),
        sign_out_others,
    })
}

fn room_user_body(obj: &Map<String, Value>) -> Result<RoomUserRequest, Vec<Verr>> {
    let mut errs = Vec::new();
    let room_id = take_room_id(obj, &mut errs);
    let user_id = take_user_id(obj, &mut errs);
    deny_extra(obj, &["roomId", "userId"], &mut errs);
    if !errs.is_empty() {
        return Err(errs);
    }
    Ok(RoomUserRequest { room_id: room_id.expect(PARSED), user_id: user_id.expect(PARSED) })
}

// --- database helpers ------------------------------------------------------

/// A row of `private_rooms`. It carries `password_hash` because Python's
/// `select(PrivateRoom)` does; `RoomOut` is built field by field, so the hash
/// has no route to the wire.
struct RoomRow {
    id: String,
    name: String,
    owner_user_id: String,
    password_hash: String,
    created_at: String,
}

const ROOM_COLUMNS: &str = "SELECT id, name, owner_user_id, password_hash, created_at
                            FROM private_rooms";

fn room_row(r: &SqliteRow) -> RoomRow {
    RoomRow {
        id: r.get("id"),
        name: r.get("name"),
        owner_user_id: r.get("owner_user_id"),
        password_hash: r.get("password_hash"),
        created_at: r.try_get("created_at").unwrap_or_default(),
    }
}

async fn load_room(pool: &SqlitePool, id: &str) -> Result<Option<RoomRow>, sqlx::Error> {
    let sql = format!("{ROOM_COLUMNS} WHERE id = ?1");
    let row = sqlx::query(&sql).bind(id).fetch_optional(pool).await?;
    Ok(row.as_ref().map(room_row))
}

async fn member_role(
    pool: &SqlitePool,
    room_id: &str,
    user_id: &str,
) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar("SELECT role FROM private_room_members WHERE room_id = ?1 AND user_id = ?2")
        .bind(room_id)
        .bind(user_id)
        .fetch_optional(pool)
        .await
}

/// Counted from the membership table, not from `private_rooms.owner_user_id`:
/// the two agree only because create and the owner transfer write both.
async fn count_owned(pool: &SqlitePool, user_id: &str) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM private_room_members WHERE user_id = ?1 AND role = 'owner'",
    )
    .bind(user_id)
    .fetch_one(pool)
    .await
}

async fn count_joined(pool: &SqlitePool, user_id: &str) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM private_room_members
         WHERE user_id = ?1 AND role IN ('owner','member')",
    )
    .bind(user_id)
    .fetch_one(pool)
    .await
}

/// Banned rows are excluded, so a ban does not hold a seat.
async fn count_members(pool: &SqlitePool, room_id: &str) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM private_room_members
         WHERE room_id = ?1 AND role IN ('owner','member')",
    )
    .bind(room_id)
    .fetch_one(pool)
    .await
}

/// `online` counts people, not sockets: `summary()` already collapses one
/// person's sockets into a single roster entry, which is what the Python's
/// `len(Room.roster())` returns.
async fn online_map(rooms: &RoomManager) -> HashMap<String, i64> {
    rooms
        .summary()
        .await
        .iter()
        .filter_map(|v| {
            let id = v.get("roomId")?.as_str()?.to_string();
            Some((id, v.get("members")?.as_i64()?))
        })
        .collect()
}

async fn online_of(rooms: &RoomManager, room_id: &str) -> i64 {
    online_map(rooms).await.get(room_id).copied().unwrap_or(0)
}

async fn room_out(
    st: &AppState,
    room: &RoomRow,
    caller_id: &str,
    online: i64,
) -> Result<RoomOut, sqlx::Error> {
    let owner = sqlx::query("SELECT handle, display_name FROM users WHERE id = ?1")
        .bind(&room.owner_user_id)
        .fetch_optional(&st.pool)
        .await?;
    // A missing owner row is near-impossible (the FK cascades), but the wire
    // contract for it is empty strings, not null.
    let (owner_handle, owner_name) = match owner {
        Some(r) => {
            let handle: String = r.get("handle");
            let display: String = r.get("display_name");
            let name = if display.is_empty() { handle.clone() } else { display };
            (handle, name)
        }
        None => (String::new(), String::new()),
    };
    Ok(RoomOut {
        id: room.id.clone(),
        name: room.name.clone(),
        owner_user_id: room.owner_user_id.clone(),
        owner_handle,
        owner_name,
        online,
        member_count: count_members(&st.pool, &room.id).await?,
        // Reads straight through, so the directory does report "banned".
        role: member_role(&st.pool, &room.id, caller_id).await?,
        created_at: iso(&room.created_at),
    })
}

async fn room_response(st: &AppState, room: &RoomRow, caller_id: &str, already: bool) -> Response {
    let online = online_of(&st.rooms, &room.id).await;
    match room_out(st, room, caller_id, online).await {
        Ok(room) => Json(RoomResponse { room, already }).into_response(),
        Err(e) => db_err(e),
    }
}

fn role_rank(role: Option<&str>) -> u8 {
    match role {
        Some("owner") => 0,
        Some("member") => 1,
        // Covers both "no membership row" and "banned".
        _ => 2,
    }
}

/// (rank, -online, casefolded name). Stable, so rooms tied on all three keep
/// the order the unordered SELECT returned them in (rowid, in practice).
fn sort_directory(rooms: &mut [RoomOut]) {
    rooms.sort_by(|a, b| {
        role_rank(a.role.as_deref())
            .cmp(&role_rank(b.role.as_deref()))
            .then(b.online.cmp(&a.online))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
}

/// Children before parent, so this works even with foreign keys off (the
/// SQLite default, and what the Python relied on). Note it does NOT touch
/// `room_farms`: deleting a room orphans its Valley garden row, which is a
/// leak and not a correctness bug, and is left exactly as the Python has it.
async fn delete_room_rows(tx: &mut SqliteConnection, room_id: &str) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM private_room_join_attempts WHERE room_id = ?1")
        .bind(room_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM private_room_members WHERE room_id = ?1")
        .bind(room_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM private_rooms WHERE id = ?1")
        .bind(room_id)
        .execute(&mut *tx)
        .await?;
    Ok(())
}

/// The websocket admission check, for main.rs's socket handler: the room's name
/// and the caller's role, or None when there is no such room. It lives here
/// because the membership rules do, and the ws route is main.rs's to register.
#[allow(dead_code)]
/// Python's `rooms.is_private_id`: a private room is any id with the `r_`
/// prefix. The websocket admission check keys off this.
pub fn is_private_id(room_id: &str) -> bool {
    room_id.starts_with("r_")
}

pub async fn admission(
    pool: &SqlitePool,
    room_id: &str,
    user_id: &str,
) -> Result<Option<(String, Option<String>)>, sqlx::Error> {
    let Some(room) = load_room(pool, room_id).await? else {
        return Ok(None);
    };
    let role = member_role(pool, room_id, user_id).await?;
    Ok(Some((room.name, role)))
}

// --- join throttle ---------------------------------------------------------

#[derive(Clone, Copy)]
enum Window {
    Pair,
    User,
    Room,
}

impl Window {
    fn cap(self) -> i64 {
        match self {
            Window::Pair => FAILS_PER_PAIR,
            Window::User => FAILS_PER_USER,
            Window::Room => FAILS_PER_ROOM,
        }
    }

    fn span(self) -> Duration {
        match self {
            Window::Pair => Duration::minutes(PAIR_WINDOW_MIN),
            Window::User => Duration::minutes(USER_WINDOW_MIN),
            Window::Room => Duration::minutes(ROOM_WINDOW_MIN),
        }
    }

    fn message(self, minutes: i64) -> String {
        match self {
            Window::Pair => {
                format!("too many wrong passwords for this room — try again in {minutes} min")
            }
            Window::User => format!("too many wrong passwords — try again in {minutes} min"),
            Window::Room => {
                format!("this room has had too many wrong passwords — try again in {minutes} min")
            }
        }
    }
}

struct Throttled {
    detail: String,
    retry: i64,
}

/// `Retry-After` keeps the raw seconds while the message rounds up to whole
/// minutes, so a five-second wait still reads "try again in 1 min".
fn throttle_minutes(retry_secs: i64) -> i64 {
    // Python's `max(1, math.ceil(retry_secs / 60))`: true division, then ceil.
    std::cmp::max(1, (retry_secs as f64 / 60.0).ceil() as i64)
}

fn ceil_secs(d: Duration) -> i64 {
    (d.num_milliseconds() as f64 / 1000.0).ceil() as i64
}

/// Book one join attempt, then test the three windows.
///
/// The attempt is committed BEFORE the password is checked, so a crash mid
/// verify still counts against the budget. Successful joins are booked too and
/// are only cleared inside the success transaction.
async fn reserve_attempt(
    pool: &SqlitePool,
    room_id: &str,
    user_id: &str,
) -> Result<Result<(), Throttled>, sqlx::Error> {
    let now = Utc::now();
    let id = uuid::Uuid::new_v4().to_string();

    let mut tx = pool.begin().await?;
    sqlx::query(
        "INSERT INTO private_room_join_attempts (id, room_id, user_id, created_at)
         VALUES (?1, ?2, ?3, ?4)",
    )
    .bind(&id)
    .bind(room_id)
    .bind(user_id)
    .bind(sqlite_dt(now))
    .execute(&mut *tx)
    .await?;
    sqlx::query("DELETE FROM private_room_join_attempts WHERE user_id = ?1 AND created_at < ?2")
        .bind(user_id)
        .bind(sqlite_dt(now - Duration::hours(ATTEMPT_RETENTION_HOURS)))
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;

    for w in [Window::Pair, Window::User, Window::Room] {
        let cutoff = sqlite_dt(now - w.span());
        // The count includes the row just inserted, and the test is STRICTLY
        // greater: the pair window therefore allows 5 password checks in 15
        // minutes and rejects the 6th.
        let count: i64 = match w {
            Window::Pair => {
                sqlx::query_scalar(
                    "SELECT COUNT(*) FROM private_room_join_attempts
                     WHERE created_at >= ?1 AND user_id = ?2 AND room_id = ?3",
                )
                .bind(&cutoff)
                .bind(user_id)
                .bind(room_id)
                .fetch_one(pool)
                .await?
            }
            Window::User => {
                sqlx::query_scalar(
                    "SELECT COUNT(*) FROM private_room_join_attempts
                     WHERE created_at >= ?1 AND user_id = ?2",
                )
                .bind(&cutoff)
                .bind(user_id)
                .fetch_one(pool)
                .await?
            }
            Window::Room => {
                sqlx::query_scalar(
                    "SELECT COUNT(*) FROM private_room_join_attempts
                     WHERE created_at >= ?1 AND room_id = ?2",
                )
                .bind(&cutoff)
                .bind(room_id)
                .fetch_one(pool)
                .await?
            }
        };
        if count <= w.cap() {
            continue;
        }

        // Over cap: give the attempt back before refusing, so a throttled
        // request does not also deepen the hole it is in.
        sqlx::query("DELETE FROM private_room_join_attempts WHERE id = ?1")
            .bind(&id)
            .execute(pool)
            .await?;

        // `id != ?` is redundant after that delete; Python has it and it costs
        // nothing, so the two queries stay comparable.
        let oldest: Option<String> = match w {
            Window::Pair => {
                sqlx::query_scalar(
                    "SELECT created_at FROM private_room_join_attempts
                     WHERE created_at >= ?1 AND user_id = ?2 AND room_id = ?3 AND id != ?4
                     ORDER BY created_at LIMIT 1",
                )
                .bind(&cutoff)
                .bind(user_id)
                .bind(room_id)
                .bind(&id)
                .fetch_optional(pool)
                .await?
            }
            Window::User => {
                sqlx::query_scalar(
                    "SELECT created_at FROM private_room_join_attempts
                     WHERE created_at >= ?1 AND user_id = ?2 AND id != ?3
                     ORDER BY created_at LIMIT 1",
                )
                .bind(&cutoff)
                .bind(user_id)
                .bind(&id)
                .fetch_optional(pool)
                .await?
            }
            Window::Room => {
                sqlx::query_scalar(
                    "SELECT created_at FROM private_room_join_attempts
                     WHERE created_at >= ?1 AND room_id = ?2 AND id != ?3
                     ORDER BY created_at LIMIT 1",
                )
                .bind(&cutoff)
                .bind(room_id)
                .bind(&id)
                .fetch_optional(pool)
                .await?
            }
        };

        let retry = match oldest.as_deref().and_then(parse_dt) {
            // When the oldest attempt in the window ages out, a slot frees.
            Some(oldest) => std::cmp::max(1, ceil_secs(oldest + w.span() - now.naive_utc())),
            None => std::cmp::max(1, w.span().num_seconds()),
        };
        return Ok(Err(Throttled {
            detail: w.message(throttle_minutes(retry)),
            retry,
        }));
    }

    // The id is not returned: the success path clears every attempt for this
    // (room, user) pair, not just this one row, so nobody needs it back.
    Ok(Ok(()))
}

/// Counts attempts, not failures, and includes the one just reserved -- which
/// is why the first wrong password says "4 tries left" and not 5.
async fn pair_failures(pool: &SqlitePool, room_id: &str, user_id: &str) -> Result<i64, sqlx::Error> {
    let cutoff = sqlite_dt(Utc::now() - Duration::minutes(PAIR_WINDOW_MIN));
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM private_room_join_attempts
         WHERE user_id = ?1 AND room_id = ?2 AND created_at >= ?3",
    )
    .bind(user_id)
    .bind(room_id)
    .bind(&cutoff)
    .fetch_one(pool)
    .await
}

fn wrong_password_message(tries_left: i64) -> String {
    // The "15-minute" here is the pair window; Python writes it into the string
    // the same way, so the two stay greppable by the client.
    match tries_left {
        k if k > 1 => format!("wrong password — {k} tries left before a 15-minute pause"),
        1 => "wrong password — 1 try left before a 15-minute pause".to_string(),
        _ => "wrong password — no tries left; wait 15 minutes".to_string(),
    }
}

// --- handlers --------------------------------------------------------------

fn caller(req: &Request) -> Caller {
    req.extensions().get::<Caller>().expect("caller set by middleware").clone()
}

/// Every room, to every authenticated caller. Private rooms are password-gated,
/// not hidden, so there is deliberately no visibility filter here.
async fn directory(State(st): State<AppState>, req: Request) -> Response {
    let c = caller(&req);
    let online = online_map(&st.rooms).await;

    // No ORDER BY: the ordering is decided below, where a tie must keep
    // SQLite's natural order.
    let rows = match sqlx::query(ROOM_COLUMNS).fetch_all(&st.pool).await {
        Ok(r) => r,
        Err(e) => return db_err(e),
    };

    // N+1 by design, as in the Python: owner, member count and the caller's
    // role per room. The derived values are what the client reads, so they are
    // computed the same way rather than folded into one clever join.
    let mut rooms: Vec<RoomOut> = Vec::with_capacity(rows.len());
    for r in &rows {
        let room = room_row(r);
        let here = online.get(&room.id).copied().unwrap_or(0);
        match room_out(&st, &room, &c.user_id, here).await {
            Ok(out) => rooms.push(out),
            Err(e) => return db_err(e),
        }
    }
    sort_directory(&mut rooms);
    // Truncated AFTER sorting, so the caller's own rooms always survive the
    // cut; a LIMIT 200 in SQL would not guarantee that.
    rooms.truncate(DIRECTORY_MAX);

    Json(DirectoryResponse {
        lobby: Lobby {
            id: "lobby",
            name: "Lobby",
            online: online.get("lobby").copied().unwrap_or(0),
        },
        rooms,
        limits: Limits {
            name_max: NAME_MAX,
            password_min: PW_MIN,
            password_max: PW_MAX,
            max_owned: MAX_OWNED,
            max_joined: MAX_JOINED,
            max_members: MAX_MEMBERS,
        },
    })
    .into_response()
}

async fn members(
    State(st): State<AppState>,
    Query(q): Query<MembersQuery>,
    req: Request,
) -> Response {
    let c = caller(&req);
    // roomId arrives in the query string here and in the body everywhere else.
    // An absent one is the framework's to refuse, so it answers in pydantic's
    // shape. A malformed one is NOT: FastAPI drops the `Annotated[..., Field(
    // pattern=...)]` constraint for a query parameter -- the generated OpenAPI
    // schema for it is a bare string -- so the Python reaches the route with
    // "lobby" in hand and answers 404. This still refuses it here instead,
    // which is the one status code in the module the two backends disagree on.
    let Some(room_id) = q.room_id else {
        return err422(vec![ve(
            vec!["query".to_string(), "roomId".to_string()],
            "missing",
            "Field required",
        )]);
    };
    if !valid_room_id(&room_id) {
        return err(StatusCode::UNPROCESSABLE_ENTITY, BAD_ROOM_ID);
    }
    match load_room(&st.pool, &room_id).await {
        Ok(Some(_)) => {}
        Ok(None) => return err(StatusCode::NOT_FOUND, NO_SUCH_ROOM),
        Err(e) => return db_err(e),
    }
    let role = match member_role(&st.pool, &room_id, &c.user_id).await {
        Ok(r) => r,
        Err(e) => return db_err(e),
    };
    // A banned caller gets the stranger's message on purpose: this endpoint is
    // not an oracle for "you are banned". The directory is where they see it.
    if !matches!(role.as_deref(), Some("owner") | Some("member")) {
        return err(StatusCode::FORBIDDEN, "join this room first");
    }

    // ORDER BY role, joined_at, id -- and 'member' sorts before 'owner' as a
    // string, so the OWNER COMES LAST. The Python's comment claims otherwise;
    // the SQL is the contract the client's roster order was built against.
    let rows = match sqlx::query(
        "SELECT m.user_id, m.role, m.joined_at, u.handle, u.display_name, u.avatar_url
         FROM private_room_members m JOIN users u ON u.id = m.user_id
         WHERE m.room_id = ?1 AND m.role IN ('owner','member')
         ORDER BY m.role, m.joined_at, m.id",
    )
    .bind(&room_id)
    .fetch_all(&st.pool)
    .await
    {
        Ok(r) => r,
        Err(e) => return db_err(e),
    };

    let mut members = Vec::with_capacity(rows.len());
    for r in &rows {
        let user_id: String = r.get("user_id");
        let handle: String = r.get("handle");
        let display: String = r.get("display_name");
        let joined_at: String = r.try_get("joined_at").unwrap_or_default();
        // Presence is per person: any live socket of theirs counts once.
        let online = st.rooms.count_where(&room_id, |m| m.user_id == user_id).await > 0;
        members.push(MemberOut {
            display_name: if display.is_empty() { handle.clone() } else { display },
            handle,
            avatar_url: r.get("avatar_url"),
            role: r.get("role"),
            online,
            joined_at: iso(&joined_at),
            user_id,
        });
    }

    // The banned list is the owner's alone.
    let mut banned = Vec::new();
    if role.as_deref() == Some("owner") {
        // joined_at only, with no id tiebreak: it is the original join time,
        // which the kick left untouched.
        let rows = match sqlx::query(
            "SELECT m.user_id, u.handle, u.display_name, u.avatar_url
             FROM private_room_members m JOIN users u ON u.id = m.user_id
             WHERE m.room_id = ?1 AND m.role = 'banned'
             ORDER BY m.joined_at",
        )
        .bind(&room_id)
        .fetch_all(&st.pool)
        .await
        {
            Ok(r) => r,
            Err(e) => return db_err(e),
        };
        for r in &rows {
            let handle: String = r.get("handle");
            let display: String = r.get("display_name");
            banned.push(BannedOut {
                user_id: r.get("user_id"),
                display_name: if display.is_empty() { handle.clone() } else { display },
                handle,
                avatar_url: r.get("avatar_url"),
            });
        }
    }

    Json(MembersResponse { room_id, members, banned }).into_response()
}

async fn create_room(State(st): State<AppState>, req: Request) -> Response {
    let c = caller(&req);
    let body = match read_request(req, create_body).await {
        Ok(b) => b,
        Err(r) => return r,
    };

    // The checks below run in exactly this order and the first failure wins.
    let Some(name) = clean_room_name(&body.name) else {
        return err(StatusCode::UNPROCESSABLE_ENTITY, "room names are 1–40 characters");
    };
    let key = room_name_key(&name);
    if key == "lobby" {
        return err(
            StatusCode::UNPROCESSABLE_ENTITY,
            "\"Lobby\" is reserved — pick another name",
        );
    }
    let pw = nfc(&body.password);
    if !password_ok(&pw) {
        return err(StatusCode::UNPROCESSABLE_ENTITY, "passwords are 6–128 characters");
    }

    match count_owned(&st.pool, &c.user_id).await {
        Ok(n) if n >= MAX_OWNED => {
            return err(
                StatusCode::CONFLICT,
                &format!("you already own {MAX_OWNED} rooms — delete one first"),
            )
        }
        Err(e) => return db_err(e),
        Ok(_) => {}
    }
    match count_joined(&st.pool, &c.user_id).await {
        Ok(n) if n >= MAX_JOINED => {
            return err(
                StatusCode::CONFLICT,
                &format!("you're in {MAX_JOINED} rooms — leave one first"),
            )
        }
        Err(e) => return db_err(e),
        Ok(_) => {}
    }
    let clash: Option<String> =
        match sqlx::query_scalar("SELECT id FROM private_rooms WHERE name_key = ?1")
            .bind(&key)
            .fetch_optional(&st.pool)
            .await
        {
            Ok(v) => v,
            Err(e) => return db_err(e),
        };
    if clash.is_some() {
        return err(StatusCode::CONFLICT, DUPLICATE_NAME);
    }

    // Charged only once every cheap check has passed, so a typo in the name
    // never costs a KDF slot.
    if !kdf_budget_ok(&c.user_id) {
        return err_retry(StatusCode::TOO_MANY_REQUESTS, KDF_BUSY, 60);
    }
    let Some(hashed) = hash_password(&pw).await else {
        return err(StatusCode::INTERNAL_SERVER_ERROR, "could not hash the password");
    };

    let now = Utc::now();
    let now_s = sqlite_dt(now);
    let room = RoomRow {
        id: new_room_id(),
        name,
        owner_user_id: c.user_id.clone(),
        password_hash: hashed,
        created_at: now_s.clone(),
    };

    // One transaction: a room with no owner membership row would be
    // unreachable, and MAX_OWNED is counted from that very row.
    let mut tx = match st.pool.begin().await {
        Ok(t) => t,
        Err(e) => return db_err(e),
    };
    if let Err(e) = sqlx::query(
        "INSERT INTO private_rooms
         (id, name, name_key, owner_user_id, password_hash, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
    )
    .bind(&room.id)
    .bind(&room.name)
    .bind(&key)
    .bind(&room.owner_user_id)
    .bind(&room.password_hash)
    .bind(&now_s)
    .execute(&mut *tx)
    .await
    {
        // Dropping the transaction rolls both inserts back.
        return if is_unique_violation(&e) {
            err(StatusCode::CONFLICT, DUPLICATE_NAME)
        } else {
            db_err(e)
        };
    }
    if let Err(e) = sqlx::query(
        "INSERT INTO private_room_members (id, room_id, user_id, role, joined_at)
         VALUES (?1, ?2, ?3, 'owner', ?4)",
    )
    .bind(uuid::Uuid::new_v4().to_string())
    .bind(&room.id)
    .bind(&room.owner_user_id)
    .bind(&now_s)
    .execute(&mut *tx)
    .await
    {
        return db_err(e);
    }
    if let Err(e) = tx.commit().await {
        return if is_unique_violation(&e) {
            err(StatusCode::CONFLICT, DUPLICATE_NAME)
        } else {
            db_err(e)
        };
    }

    room_response(&st, &room, &c.user_id, false).await
}

async fn join_room(State(st): State<AppState>, req: Request) -> Response {
    let c = caller(&req);
    let body = match read_request(req, join_body).await {
        Ok(b) => b,
        Err(r) => return r,
    };

    // PHASE 1: cheap pre-checks, no gate held. They exist to avoid paying for
    // scrypt, not to decide anything -- phase 2 re-checks all of it.
    let room = match load_room(&st.pool, &body.room_id).await {
        Ok(Some(r)) => r,
        Ok(None) => return err(StatusCode::NOT_FOUND, NO_SUCH_ROOM),
        Err(e) => return db_err(e),
    };
    match member_role(&st.pool, &body.room_id, &c.user_id).await {
        // Idempotent re-join: no password check, no attempt row, no KDF spend
        // and no write. This is the client's retry-safety contract.
        Ok(Some(role)) if role == "owner" || role == "member" => {
            return room_response(&st, &room, &c.user_id, true).await
        }
        // Checked before any cap or password work: a ban is not a wrong
        // password, and the kick left the row behind precisely so this fires.
        Ok(Some(role)) if role == "banned" => {
            return err(StatusCode::FORBIDDEN, "you were removed from this room")
        }
        Err(e) => return db_err(e),
        Ok(_) => {}
    }
    match count_joined(&st.pool, &c.user_id).await {
        Ok(n) if n >= MAX_JOINED => {
            return err(
                StatusCode::CONFLICT,
                &format!("you're in {MAX_JOINED} rooms — leave one first"),
            )
        }
        Err(e) => return db_err(e),
        Ok(_) => {}
    }
    match count_members(&st.pool, &body.room_id).await {
        Ok(n) if n >= MAX_MEMBERS => {
            return err(
                StatusCode::CONFLICT,
                &format!("this room is full ({MAX_MEMBERS} members)"),
            )
        }
        Err(e) => return db_err(e),
        Ok(_) => {}
    }
    if !kdf_budget_ok(&c.user_id) {
        return err_retry(StatusCode::TOO_MANY_REQUESTS, KDF_BUSY, 60);
    }

    // Booked and committed before the password is checked, so a crash mid
    // verify still counts against the budget.
    match reserve_attempt(&st.pool, &body.room_id, &c.user_id).await {
        Ok(Ok(())) => {}
        Ok(Err(t)) => return err_retry(StatusCode::TOO_MANY_REQUESTS, &t.detail, t.retry),
        Err(e) => return db_err(e),
    }

    let pw = nfc(&body.password);
    // A password outside 6..=128 counts as WRONG rather than as a 422: an
    // unknown room must not become a length oracle. It still burns the attempt
    // it just booked. /create and /password do answer 422 for the same input.
    let ok = if password_ok(&pw) {
        verify_password(&pw, &room.password_hash).await
    } else {
        false
    };
    if !ok {
        let failures = match pair_failures(&st.pool, &body.room_id, &c.user_id).await {
            Ok(n) => n,
            Err(e) => return db_err(e),
        };
        // pair_failures includes the attempt just booked, so the ladder reads
        // 4, 3, 2, "1 try left", "no tries left" and then the 429.
        let message = wrong_password_message(FAILS_PER_PAIR - failures);
        return err(StatusCode::FORBIDDEN, &message);
    }

    // PHASE 2: the password is right; the gate serialises the actual join.
    let room = {
        let lock = gate(&body.room_id);
        let _held = lock.lock().await;

        let room = match load_room(&st.pool, &body.room_id).await {
            Ok(Some(r)) => r,
            Ok(None) => return err(StatusCode::NOT_FOUND, NO_SUCH_ROOM),
            Err(e) => return db_err(e),
        };
        match member_role(&st.pool, &body.room_id, &c.user_id).await {
            Ok(Some(role)) if role == "banned" => {
                return err(StatusCode::FORBIDDEN, "you were removed from this room")
            }
            // Someone else's request got there first, or a kick landed; either
            // way the caller is already in and this is not an error.
            Ok(Some(_)) => return room_response(&st, &room, &c.user_id, true).await,
            Err(e) => return db_err(e),
            Ok(None) => {}
        }
        match count_joined(&st.pool, &c.user_id).await {
            Ok(n) if n >= MAX_JOINED => {
                return err(
                    StatusCode::CONFLICT,
                    &format!("you're in {MAX_JOINED} rooms — leave one first"),
                )
            }
            Err(e) => return db_err(e),
            Ok(_) => {}
        }
        match count_members(&st.pool, &body.room_id).await {
            Ok(n) if n >= MAX_MEMBERS => {
                return err(
                    StatusCode::CONFLICT,
                    &format!("this room is full ({MAX_MEMBERS} members)"),
                )
            }
            Err(e) => return db_err(e),
            Ok(_) => {}
        }

        let mut tx = match st.pool.begin().await {
            Ok(t) => t,
            Err(e) => return db_err(e),
        };
        // The attempt purge and the membership INSERT are ONE transaction: if
        // the insert loses the race, the purge rolls back with it and the
        // throttle accounting stays honest.
        if let Err(e) =
            sqlx::query("DELETE FROM private_room_join_attempts WHERE room_id = ?1 AND user_id = ?2")
                .bind(&body.room_id)
                .bind(&c.user_id)
                .execute(&mut *tx)
                .await
        {
            return db_err(e);
        }
        let insert = sqlx::query(
            "INSERT INTO private_room_members (id, room_id, user_id, role, joined_at)
             VALUES (?1, ?2, ?3, 'member', ?4)",
        )
        .bind(uuid::Uuid::new_v4().to_string())
        .bind(&body.room_id)
        .bind(&c.user_id)
        .bind(sqlite_dt(Utc::now()))
        .execute(&mut *tx)
        .await;
        let committed = match insert {
            Ok(_) => tx.commit().await,
            Err(e) => Err(e),
        };
        if let Err(e) = committed {
            // Lost the UNIQUE(room_id, user_id) race: they are in, which is
            // what they asked for.
            return if is_unique_violation(&e) {
                room_response(&st, &room, &c.user_id, true).await
            } else {
                db_err(e)
            };
        }
        room
    };

    room_response(&st, &room, &c.user_id, false).await
}

async fn leave_room(State(st): State<AppState>, req: Request) -> Response {
    let c = caller(&req);
    let body = match read_request(req, room_ref_body).await {
        Ok(b) => b,
        Err(r) => return r,
    };
    // Leaving a room you are not in, or are banned from, is a silent 200.
    let quiet = || {
        Json(LeaveResponse { ok: true, deleted: false, new_owner_handle: None }).into_response()
    };

    let room = match load_room(&st.pool, &body.room_id).await {
        Ok(Some(r)) => r,
        Ok(None) => return err(StatusCode::NOT_FOUND, NO_SUCH_ROOM),
        Err(e) => return db_err(e),
    };
    match member_role(&st.pool, &body.room_id, &c.user_id).await {
        // A banned row is NOT deleted by leaving: the ban outlives the exit.
        Ok(None) => return quiet(),
        Ok(Some(role)) if role == "banned" => return quiet(),
        Err(e) => return db_err(e),
        Ok(Some(_)) => {}
    }

    let lock = gate(&body.room_id);
    let _held = lock.lock().await;

    let role = match member_role(&st.pool, &body.room_id, &c.user_id).await {
        Ok(Some(role)) if role != "banned" => role,
        Ok(_) => return quiet(),
        Err(e) => return db_err(e),
    };

    if role == "member" {
        if let Err(e) =
            sqlx::query("DELETE FROM private_room_members WHERE room_id = ?1 AND user_id = ?2")
                .bind(&body.room_id)
                .bind(&c.user_id)
                .execute(&st.pool)
                .await
        {
            return db_err(e);
        }
        evict_unavailable(&body.room_id, 4406, "you left this room", "the caller's");
        return quiet();
    }

    // The owner is leaving. The earliest member by (joined_at, id) inherits;
    // banned rows are never heirs.
    let heir: Option<String> = match sqlx::query_scalar(
        "SELECT user_id FROM private_room_members
         WHERE room_id = ?1 AND role = 'member'
         ORDER BY joined_at, id LIMIT 1",
    )
    .bind(&body.room_id)
    .fetch_optional(&st.pool)
    .await
    {
        Ok(v) => v,
        Err(e) => return db_err(e),
    };

    let Some(heir_id) = heir else {
        // Alone, or only banned rows left: the room goes.
        let mut tx = match st.pool.begin().await {
            Ok(t) => t,
            Err(e) => return db_err(e),
        };
        if let Err(e) = delete_room_rows(&mut tx, &body.room_id).await {
            return db_err(e);
        }
        if let Err(e) = tx.commit().await {
            return db_err(e);
        }
        evict_unavailable(&body.room_id, 4404, "room deleted", "every");
        return Json(LeaveResponse { ok: true, deleted: true, new_owner_handle: None })
            .into_response();
    };

    let heir_handle: Option<String> =
        match sqlx::query_scalar("SELECT handle FROM users WHERE id = ?1")
            .bind(&heir_id)
            .fetch_optional(&st.pool)
            .await
        {
            Ok(v) => v,
            Err(e) => return db_err(e),
        };

    // One transaction for all four changes. Ownership is authorised from
    // private_rooms.owner_user_id while the MAX_OWNED cap is counted from the
    // membership role, so both must move together or the two disagree forever.
    let mut tx = match st.pool.begin().await {
        Ok(t) => t,
        Err(e) => return db_err(e),
    };
    if let Err(e) =
        sqlx::query("UPDATE private_rooms SET owner_user_id = ?1, updated_at = ?2 WHERE id = ?3")
            .bind(&heir_id)
            .bind(sqlite_dt(Utc::now()))
            .bind(&body.room_id)
            .execute(&mut *tx)
            .await
    {
        return db_err(e);
    }
    if let Err(e) = sqlx::query(
        "UPDATE private_room_members SET role = 'owner' WHERE room_id = ?1 AND user_id = ?2",
    )
    .bind(&body.room_id)
    .bind(&heir_id)
    .execute(&mut *tx)
    .await
    {
        return db_err(e);
    }
    if let Err(e) =
        sqlx::query("DELETE FROM private_room_members WHERE room_id = ?1 AND user_id = ?2")
            .bind(&body.room_id)
            .bind(&c.user_id)
            .execute(&mut *tx)
            .await
    {
        return db_err(e);
    }
    if let Err(e) = tx.commit().await {
        return db_err(e);
    }

    // Commit, then sockets, then the broadcast -- never the other way round.
    evict_unavailable(&body.room_id, 4406, "you left this room", "the caller's");
    if st.rooms.exists(&body.room_id).await {
        st.rooms
            .broadcast(
                &body.room_id,
                json!({"type": "room", "op": "updated",
                       "room": {"id": &room.id, "name": &room.name, "ownerUserId": &heir_id}})
                .to_string(),
            )
            .await;
    }
    Json(LeaveResponse { ok: true, deleted: false, new_owner_handle: heir_handle }).into_response()
}

/// Ownership is read from `private_rooms.owner_user_id`, not from the
/// membership role -- the Python's `_owner` helper does the same.
fn not_owner(room: &RoomRow, c: &Caller) -> Option<Response> {
    if room.owner_user_id == c.user_id {
        None
    } else {
        Some(err(StatusCode::FORBIDDEN, NOT_OWNER))
    }
}

async fn rename_room(State(st): State<AppState>, req: Request) -> Response {
    let c = caller(&req);
    let body = match read_request(req, rename_body).await {
        Ok(b) => b,
        Err(r) => return r,
    };
    let room = match load_room(&st.pool, &body.room_id).await {
        Ok(Some(r)) => r,
        Ok(None) => return err(StatusCode::NOT_FOUND, NO_SUCH_ROOM),
        Err(e) => return db_err(e),
    };
    if let Some(r) = not_owner(&room, &c) {
        return r;
    }
    let Some(name) = clean_room_name(&body.name) else {
        return err(StatusCode::UNPROCESSABLE_ENTITY, "room names are 1–40 characters");
    };
    let key = room_name_key(&name);
    if key == "lobby" {
        return err(
            StatusCode::UNPROCESSABLE_ENTITY,
            "\"Lobby\" is reserved — pick another name",
        );
    }
    // Excluding this room's own id means re-casing its own name is allowed.
    let clash: Option<String> =
        match sqlx::query_scalar("SELECT id FROM private_rooms WHERE name_key = ?1 AND id != ?2")
            .bind(&key)
            .bind(&body.room_id)
            .fetch_optional(&st.pool)
            .await
        {
            Ok(v) => v,
            Err(e) => return db_err(e),
        };
    if clash.is_some() {
        return err(StatusCode::CONFLICT, DUPLICATE_NAME);
    }

    // No gate here, deliberately: a rename races only with another rename, and
    // UNIQUE(name_key) is the referee.
    if let Err(e) =
        sqlx::query("UPDATE private_rooms SET name = ?1, name_key = ?2, updated_at = ?3 WHERE id = ?4")
            .bind(&name)
            .bind(&key)
            .bind(sqlite_dt(Utc::now()))
            .bind(&body.room_id)
            .execute(&st.pool)
            .await
    {
        return if is_unique_violation(&e) {
            err(StatusCode::CONFLICT, DUPLICATE_NAME)
        } else {
            db_err(e)
        };
    }

    let room = RoomRow { name, ..room };
    if st.rooms.exists(&body.room_id).await {
        st.rooms
            .broadcast(
                &body.room_id,
                json!({"type": "room", "op": "updated",
                       "room": {"id": &room.id, "name": &room.name,
                                "ownerUserId": &room.owner_user_id}})
                .to_string(),
            )
            .await;
    }
    room_response(&st, &room, &c.user_id, false).await
}

async fn change_password(State(st): State<AppState>, req: Request) -> Response {
    let c = caller(&req);
    let body = match read_request(req, password_body).await {
        Ok(b) => b,
        Err(r) => return r,
    };
    let room = match load_room(&st.pool, &body.room_id).await {
        Ok(Some(r)) => r,
        Ok(None) => return err(StatusCode::NOT_FOUND, NO_SUCH_ROOM),
        Err(e) => return db_err(e),
    };
    if let Some(r) = not_owner(&room, &c) {
        return r;
    }
    let pw = nfc(&body.password);
    // Unlike /join, this endpoint does answer 422 on a bad length: the caller
    // already owns the room, so there is nothing to probe for.
    if !password_ok(&pw) {
        return err(StatusCode::UNPROCESSABLE_ENTITY, "passwords are 6–128 characters");
    }
    if !kdf_budget_ok(&c.user_id) {
        return err_retry(StatusCode::TOO_MANY_REQUESTS, KDF_BUSY, 60);
    }
    let Some(hashed) = hash_password(&pw).await else {
        return err(StatusCode::INTERNAL_SERVER_ERROR, "could not hash the password");
    };

    let lock = gate(&body.room_id);
    let _held = lock.lock().await;

    let mut tx = match st.pool.begin().await {
        Ok(t) => t,
        Err(e) => return db_err(e),
    };
    if let Err(e) =
        sqlx::query("UPDATE private_rooms SET password_hash = ?1, updated_at = ?2 WHERE id = ?3")
            .bind(&hashed)
            .bind(sqlite_dt(Utc::now()))
            .bind(&body.room_id)
            .execute(&mut *tx)
            .await
    {
        return db_err(e);
    }
    // No user filter: a new password resets the pair and room throttles for
    // everybody, because the thing they were failing against is gone.
    if let Err(e) = sqlx::query("DELETE FROM private_room_join_attempts WHERE room_id = ?1")
        .bind(&body.room_id)
        .execute(&mut *tx)
        .await
    {
        return db_err(e);
    }
    let mut signed_out = 0u64;
    if body.sign_out_others {
        // Only role='member' rows: a banned user stays banned across a password
        // change, and a signed-out member becomes a stranger, not a ban.
        match sqlx::query("DELETE FROM private_room_members WHERE room_id = ?1 AND role = 'member'")
            .bind(&body.room_id)
            .execute(&mut *tx)
            .await
        {
            Ok(r) => signed_out = r.rows_affected(),
            Err(e) => return db_err(e),
        }
    }
    if let Err(e) = tx.commit().await {
        return db_err(e);
    }
    if body.sign_out_others {
        evict_unavailable(
            &body.room_id,
            4406,
            "room password changed",
            "everyone but the caller's",
        );
    }

    Json(json!({"ok": true, "signedOut": signed_out})).into_response()
}

async fn kick_member(State(st): State<AppState>, req: Request) -> Response {
    let c = caller(&req);
    let body = match read_request(req, room_user_body).await {
        Ok(b) => b,
        Err(r) => return r,
    };
    let room = match load_room(&st.pool, &body.room_id).await {
        Ok(Some(r)) => r,
        Ok(None) => return err(StatusCode::NOT_FOUND, NO_SUCH_ROOM),
        Err(e) => return db_err(e),
    };
    if let Some(r) = not_owner(&room, &c) {
        return r;
    }
    // After the owner check, so a non-owner kicking themselves gets the 403.
    if body.user_id == c.user_id {
        return err(StatusCode::BAD_REQUEST, "you can't remove yourself — use Leave");
    }
    // Kicking a stranger and kicking an already-banned user answer the same.
    match member_role(&st.pool, &body.room_id, &body.user_id).await {
        Ok(Some(role)) if role == "member" => {}
        Err(e) => return db_err(e),
        Ok(_) => return err(StatusCode::NOT_FOUND, "they're not in this room"),
    }

    let lock = gate(&body.room_id);
    let _held = lock.lock().await;

    // The row is kept, not deleted, and joined_at is untouched: that surviving
    // row is the ban, and /join checks it before anything else. Join attempts
    // are deliberately not cleared.
    if let Err(e) = sqlx::query(
        "UPDATE private_room_members SET role = 'banned' WHERE room_id = ?1 AND user_id = ?2",
    )
    .bind(&body.room_id)
    .bind(&body.user_id)
    .execute(&st.pool)
    .await
    {
        return db_err(e);
    }
    evict_unavailable(&body.room_id, 4406, "removed from this room", "the target's");

    Json(json!({"ok": true})).into_response()
}

async fn unban_member(State(st): State<AppState>, req: Request) -> Response {
    let c = caller(&req);
    let body = match read_request(req, room_user_body).await {
        Ok(b) => b,
        Err(r) => return r,
    };
    let room = match load_room(&st.pool, &body.room_id).await {
        Ok(Some(r)) => r,
        Ok(None) => return err(StatusCode::NOT_FOUND, NO_SUCH_ROOM),
        Err(e) => return db_err(e),
    };
    if let Some(r) = not_owner(&room, &c) {
        return r;
    }
    match member_role(&st.pool, &body.room_id, &body.user_id).await {
        Ok(Some(role)) if role == "banned" => {}
        Err(e) => return db_err(e),
        Ok(_) => return err(StatusCode::NOT_FOUND, "they're not banned here"),
    }
    // No gate and no eviction: lifting a ban only removes the row. They still
    // have to join again with the password.
    if let Err(e) =
        sqlx::query("DELETE FROM private_room_members WHERE room_id = ?1 AND user_id = ?2")
            .bind(&body.room_id)
            .bind(&body.user_id)
            .execute(&st.pool)
            .await
    {
        return db_err(e);
    }
    Json(json!({"ok": true})).into_response()
}

async fn delete_room(State(st): State<AppState>, req: Request) -> Response {
    let c = caller(&req);
    let body = match read_request(req, room_ref_body).await {
        Ok(b) => b,
        Err(r) => return r,
    };
    let room = match load_room(&st.pool, &body.room_id).await {
        Ok(Some(r)) => r,
        Ok(None) => return err(StatusCode::NOT_FOUND, NO_SUCH_ROOM),
        Err(e) => return db_err(e),
    };
    if let Some(r) = not_owner(&room, &c) {
        return r;
    }

    let lock = gate(&body.room_id);
    let _held = lock.lock().await;

    let mut tx = match st.pool.begin().await {
        Ok(t) => t,
        Err(e) => return db_err(e),
    };
    if let Err(e) = delete_room_rows(&mut tx, &body.room_id).await {
        return db_err(e);
    }
    if let Err(e) = tx.commit().await {
        return db_err(e);
    }
    evict_unavailable(&body.room_id, 4404, "room deleted", "every");

    Json(json!({"ok": true})).into_response()
}

/// Only this group's routes. main.rs applies `require_device` to the whole
/// guarded router, so there is deliberately no auth layer here.
pub fn routes() -> Router<crate::AppState> {
    Router::new()
        .route("/v1/rooms/directory", get(directory))
        .route("/v1/rooms/members", get(members))
        .route("/v1/rooms/create", post(create_room))
        .route("/v1/rooms/join", post(join_room))
        .route("/v1/rooms/leave", post(leave_room))
        .route("/v1/rooms/rename", post(rename_room))
        .route("/v1/rooms/password", post(change_password))
        .route("/v1/rooms/kick", post(kick_member))
        .route("/v1/rooms/unban", post(unban_member))
        .route("/v1/rooms/delete", post(delete_room))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn out(name: &str, online: i64, role: Option<&str>) -> RoomOut {
        RoomOut {
            id: format!("r_{name}"),
            name: name.to_string(),
            owner_user_id: "u1".into(),
            owner_handle: "ash".into(),
            owner_name: "Ash".into(),
            online,
            member_count: 1,
            role: role.map(str::to_string),
            created_at: "2026-10-08T12:34:56+00:00".into(),
        }
    }

    // --- scrypt ---
    // Both vectors come from hashlib, which wrote every hash in the database.

    #[test]
    fn scrypt_matches_the_rfc_7914_vector() {
        // hashlib.scrypt(b"", salt=b"", n=16, r=1, p=1, dklen=64)
        assert_eq!(
            hex::encode(scrypt_raw(b"", b"", 16, 1, 1, 64)),
            "77d6576238657b203b19ca42c18a0497f16b4844e3074ae8dfdffa3fede21442\
             fcd0069ded0948f8326a753a0fc81f17e8d3e0fb2e0d3628cf35e20c38d18906"
        );
    }

    #[test]
    fn scrypt_matches_hashlib_at_the_r_and_p_this_backend_uses() {
        // hashlib.scrypt(b"hunter22", salt=b"0123456789abcdef", n=256, r=8, p=5, dklen=32).
        // N is small only to keep the test quick; r and p are production.
        assert_eq!(
            hex::encode(scrypt_raw(b"hunter22", b"0123456789abcdef", 256, 8, 5, 32)),
            "d3fc611a4bd6d22048cb5ecbb018e0f1ca113fc4bd267bcfce103334cfa1d976"
        );
    }

    #[tokio::test]
    async fn a_hash_written_by_the_python_verifies_here() {
        let stored = "scrypt$256$8$5$MDEyMzQ1Njc4OWFiY2RlZg$0_xhGkvW0iBIy17LsBjg8coRP8S9JnvPzhAzNM-h2XY";
        assert!(verify_password("hunter22", stored).await);
        assert!(!verify_password("hunter23", stored).await);
    }

    #[tokio::test]
    async fn a_malformed_stored_hash_is_a_refusal_not_an_error() {
        let salt = "MDEyMzQ1Njc4OWFiY2RlZg";
        let rejected = vec![
            String::new(),
            "scrypt".to_string(),
            format!("scrypt$256$8$5${salt}"),         // five fields
            format!("bcrypt$256$8$5${salt}$MDEy"),    // not scrypt
            format!("scrypt$255$8$5${salt}$MDEy"),    // N not a power of two
            format!("scrypt$1$8$5${salt}$MDEy"),      // N below 2
            format!("scrypt$256$17$5${salt}$MDEy"),   // r out of range
            format!("scrypt$256$8$0${salt}$MDEy"),    // p out of range
            "scrypt$256$8$5$!!!!$MDEy".to_string(),   // salt is not base64url
            format!("scrypt$256$8$5${salt}$MDEy"),    // dk is not 32 bytes
        ];
        for bad in &rejected {
            assert!(!verify_password("hunter22", bad).await, "accepted {bad:?}");
        }
    }

    #[test]
    fn base64url_is_unpadded_and_round_trips() {
        assert_eq!(b64e(b"0123456789abcdef"), "MDEyMzQ1Njc4OWFiY2RlZg");
        assert_eq!(b64d("MDEyMzQ1Njc4OWFiY2RlZg").as_deref(), Some(&b"0123456789abcdef"[..]));
        // Padding is tolerated on the way in, and '+'/'/' are not the alphabet.
        assert_eq!(b64d("MDEyMzQ1Njc4OWFiY2RlZg=="), b64d("MDEyMzQ1Njc4OWFiY2RlZg"));
        assert_eq!(b64d("a+b/"), None);
        assert_eq!(b64d("A"), None);
        for n in 1..40usize {
            let bytes: Vec<u8> = (0..n).map(|i| (i * 7 + 3) as u8).collect();
            assert_eq!(b64d(&b64e(&bytes)).as_deref(), Some(&bytes[..]), "{n} bytes");
        }
    }

    // --- ids ---

    #[test]
    fn room_ids_are_the_python_shape() {
        let id = new_room_id();
        assert_eq!(id.len(), 24, "{id}");
        assert!(id.starts_with("r_") && valid_room_id(&id), "{id}");
        assert!(!valid_room_id("lobby"));
        assert!(!valid_room_id("r_short"));
        // Python's rule is ^r_[A-Za-z0-9_-]{22}$ -- exactly 22 after the prefix.
        assert!(valid_room_id("r_0123456789012345678901")); // 22 after the prefix: valid
        assert!(!valid_room_id("r_01234567890123456789012")); // 23: too long
        assert!(valid_room_id("r_0123456789012345678-_a"));
        assert!(!valid_room_id("r_0123456789012345678-_!"));
    }

    #[test]
    fn user_ids_must_be_lowercase_uuids() {
        assert!(valid_user_id("3f2504e0-4f89-11d3-9a0c-0305e82c3301"));
        // An uppercase uuid is a 422, not a 404 -- do not relax this.
        assert!(!valid_user_id("3F2504E0-4F89-11D3-9A0C-0305E82C3301"));
        assert!(!valid_user_id("3f2504e0-4f89-11d3-9a0c-0305e82c330"));
        assert!(!valid_user_id("3f2504e0-4f89-11d3-9a0c-0305e82c3301-x"));
        assert!(!valid_user_id("not-a-uuid"));
        assert!(!valid_user_id(""));
    }

    // --- names ---

    #[test]
    fn names_fold_whitespace_and_cap_at_forty() {
        assert_eq!(clean_room_name("  Hello   World  ").as_deref(), Some("Hello World"));
        assert_eq!(clean_room_name("\tTabbed\nName ").as_deref(), Some("Tabbed Name"));
        assert_eq!(clean_room_name(""), None);
        assert_eq!(clean_room_name("    "), None);
        let forty = "a".repeat(NAME_MAX);
        assert_eq!(clean_room_name(&forty).as_deref(), Some(forty.as_str()));
        assert_eq!(clean_room_name(&"a".repeat(NAME_MAX + 1)), None);
    }

    #[test]
    fn non_printables_become_spaces_rather_than_vanishing() {
        assert_eq!(clean_room_name("a\u{7}b").as_deref(), Some("a b"));
        assert_eq!(clean_room_name("a\u{200b}b").as_deref(), Some("a b"));
    }

    #[test]
    fn zalgo_keeps_two_marks_per_run() {
        // U+0333 composes with nothing, so NFC is not in play here.
        let name = clean_room_name("x\u{333}\u{333}\u{333}\u{333}\u{333}");
        assert_eq!(name.as_deref(), Some("x\u{333}\u{333}"));
        // A name with no letter, number, punctuation or symbol is refused.
        assert_eq!(clean_room_name("\u{333}\u{333}"), None);
    }

    #[test]
    fn the_name_key_is_case_folded_so_two_casings_collide() {
        assert_eq!(room_name_key("Dup"), room_name_key("dup"));
        assert_eq!(room_name_key("LOBBY"), "lobby");
        // Which is also what makes "Lobby" reserved whatever case it arrives in.
        assert_eq!(room_name_key(&clean_room_name(" lObBy ").unwrap()), "lobby");
    }

    #[test]
    fn password_length_counts_characters_not_bytes() {
        assert!(!password_ok("12345"));
        assert!(password_ok("123456"));
        assert!(password_ok(&"a".repeat(PW_MAX)));
        assert!(!password_ok(&"a".repeat(PW_MAX + 1)));
        // Six characters, eighteen bytes: len() would wrongly accept this as
        // long enough, and wrongly reject a long non-ASCII password.
        assert!(password_ok("日本語です！！"));
        assert!(!password_ok("日本語で"));
    }

    // --- throttle arithmetic ---

    #[test]
    fn retry_after_seconds_round_up_to_whole_minutes_in_the_message() {
        assert_eq!(throttle_minutes(1), 1);
        assert_eq!(throttle_minutes(5), 1);
        assert_eq!(throttle_minutes(60), 1);
        assert_eq!(throttle_minutes(61), 2);
        assert_eq!(throttle_minutes(900), 15);
    }

    #[test]
    fn the_window_messages_name_their_window() {
        assert_eq!(
            Window::Pair.message(15),
            "too many wrong passwords for this room — try again in 15 min"
        );
        assert_eq!(Window::User.message(7), "too many wrong passwords — try again in 7 min");
        assert_eq!(
            Window::Room.message(1),
            "this room has had too many wrong passwords — try again in 1 min"
        );
        assert_eq!((Window::Pair.cap(), Window::User.cap(), Window::Room.cap()), (5, 20, 50));
        assert_eq!(Window::Pair.span().num_minutes(), 15);
    }

    #[test]
    fn the_tries_left_ladder_starts_at_four() {
        // pair_failures includes the attempt just booked, so the first wrong
        // password reports four, never five.
        assert_eq!(
            wrong_password_message(FAILS_PER_PAIR - 1),
            "wrong password — 4 tries left before a 15-minute pause"
        );
        assert_eq!(
            wrong_password_message(2),
            "wrong password — 2 tries left before a 15-minute pause"
        );
        assert_eq!(
            wrong_password_message(1),
            "wrong password — 1 try left before a 15-minute pause"
        );
        assert_eq!(wrong_password_message(0), "wrong password — no tries left; wait 15 minutes");
        assert_eq!(wrong_password_message(-3), "wrong password — no tries left; wait 15 minutes");
    }

    #[test]
    fn the_kdf_budget_allows_ten_a_minute_then_refuses() {
        let user = "budget-test-user";
        for i in 0..KDF_OPS_PER_MIN {
            assert!(kdf_budget_ok(user), "refused call {i}");
        }
        assert!(!kdf_budget_ok(user));
        // A refusal does not consume a token, and another user is unaffected.
        assert!(!kdf_budget_ok(user));
        assert!(kdf_budget_ok("budget-test-other"));
    }

    // --- directory ---

    #[test]
    fn the_directory_sorts_owned_then_joined_then_the_rest() {
        let mut rooms = vec![
            out("zeta", 0, None),
            out("alpha", 5, Some("banned")),
            out("beta", 0, Some("member")),
            out("gamma", 9, Some("owner")),
        ];
        sort_directory(&mut rooms);
        let names: Vec<&str> = rooms.iter().map(|r| r.name.as_str()).collect();
        // banned ranks with "no membership", and inside a rank the busier room
        // comes first.
        assert_eq!(names, vec!["gamma", "beta", "alpha", "zeta"]);
        assert_eq!(role_rank(None), role_rank(Some("banned")));
    }

    #[test]
    fn ties_keep_the_order_the_select_returned() {
        let mut rooms = vec![out("same", 0, None), out("Same", 0, None), out("sAme", 0, None)];
        sort_directory(&mut rooms);
        let names: Vec<&str> = rooms.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["same", "Same", "sAme"]);
    }

    #[test]
    fn the_directory_cut_happens_after_the_sort() {
        // One owned room buried past the cut must survive it.
        let mut rooms: Vec<RoomOut> =
            (0..DIRECTORY_MAX + 5).map(|i| out(&format!("room{i:03}"), 0, None)).collect();
        rooms.push(out("mine", 0, Some("owner")));
        sort_directory(&mut rooms);
        rooms.truncate(DIRECTORY_MAX);
        assert_eq!(rooms.len(), DIRECTORY_MAX);
        assert_eq!(rooms[0].name, "mine");
    }

    // --- wire shapes ---

    #[test]
    fn timestamps_are_seconds_precision_with_an_explicit_offset() {
        assert_eq!(iso("2026-10-08 12:34:56.123456"), "2026-10-08T12:34:56+00:00");
        assert_eq!(iso("2026-10-08 12:34:56"), "2026-10-08T12:34:56+00:00");
        assert_eq!(iso("2026-10-08T12:34:56.000001"), "2026-10-08T12:34:56+00:00");
        // Unparseable text passes through rather than becoming a 500.
        assert_eq!(iso("not a date"), "not a date");
    }

    #[test]
    fn room_out_keeps_the_python_key_order_and_a_null_role() {
        let json = serde_json::to_string(&out("Lab", 2, None)).expect("RoomOut serialises");
        assert_eq!(
            json,
            r#"{"id":"r_Lab","name":"Lab","ownerUserId":"u1","ownerHandle":"ash","ownerName":"Ash","online":2,"memberCount":1,"role":null,"createdAt":"2026-10-08T12:34:56+00:00"}"#
        );
        // role is present and reports a ban; the hash has no field to live in.
        let banned = serde_json::to_string(&out("Lab", 0, Some("banned"))).expect("serialises");
        assert!(banned.contains(r#""role":"banned""#));
        assert!(!banned.contains("scrypt") && !banned.contains("password"));
    }

    #[test]
    fn an_unknown_field_is_refused_the_way_extra_forbid_is() {
        assert!(serde_json::from_str::<CreateRequest>(r#"{"name":"Lab","password":"hunter22"}"#).is_ok());
        assert!(serde_json::from_str::<CreateRequest>(
            r#"{"name":"Lab","password":"hunter22","role":"owner"}"#
        )
        .is_err());
        assert!(serde_json::from_str::<CreateRequest>(r#"{"name":"Lab"}"#).is_err());
        // signOutOthers defaults to false, every other field is required.
        let p: PasswordRequest =
            serde_json::from_str(r#"{"roomId":"r_0123456789012345678901","password":"hunter22"}"#)
                .expect("optional signOutOthers");
        assert!(!p.sign_out_others);
    }

    // --- the 422 shape ---
    //
    // Every string below was read off FastAPI 0.141 / pydantic 2.13 running the
    // real schemas, not reconstructed from the docs. A pydantic failure answers
    // with a LIST of {loc, msg, type}; the route's own `raise HTTPException(422,
    // "...")` answers with a string, and those sites stay strings here.

    fn obj(raw: &str) -> Map<String, Value> {
        body_object(raw.as_bytes()).expect("an object body")
    }

    /// Written as a match rather than `unwrap_err`, so the request structs need
    /// no `Debug`: nothing should make a password easy to print by accident.
    fn rejected<T>(r: Result<T, Vec<Verr>>) -> Vec<Verr> {
        match r {
            Err(errs) => errs,
            Ok(_) => panic!("expected a rejection"),
        }
    }

    fn kinds(errs: &[Verr]) -> Vec<&'static str> {
        errs.iter().map(|e| e.kind).collect()
    }

    fn locs(errs: &[Verr]) -> Vec<String> {
        errs.iter().map(|e| e.loc.join("/")).collect()
    }

    const ROOM: &str = "r_0123456789012345678901";

    #[test]
    fn a_pydantic_422_is_a_list_of_three_key_entries() {
        let body = Errors { detail: vec![ve(bloc("roomId"), "string_pattern_mismatch", "nope")] };
        assert_eq!(
            serde_json::to_string(&body).expect("Errors serialises"),
            r#"{"detail":[{"loc":["body","roomId"],"msg":"nope","type":"string_pattern_mismatch"}]}"#
        );
    }

    #[test]
    fn a_body_that_is_not_an_object_answers_the_way_fastapi_does() {
        // An empty body and a literal null are both "the body is missing".
        assert_eq!(kinds(&rejected(body_object(b""))), ["missing"]);
        assert_eq!(kinds(&rejected(body_object(b"null"))), ["missing"]);
        assert_eq!(locs(&rejected(body_object(b"null"))), ["body"]);
        assert_eq!(rejected(body_object(b""))[0].msg, "Field required");
        for scalar in [&b"[]"[..], &b"5"[..], &b"true"[..], &b"\"abc\""[..]] {
            assert_eq!(kinds(&rejected(body_object(scalar))), ["model_attributes_type"]);
        }
        assert_eq!(
            rejected(body_object(b"[]"))[0].msg,
            "Input should be a valid dictionary or object to extract fields from"
        );
        assert_eq!(kinds(&rejected(body_object(b"{"))), ["json_invalid"]);
        assert_eq!(rejected(body_object(b"{"))[0].msg, "JSON decode error");
        assert!(body_object(b"{}").is_ok());
    }

    #[test]
    fn fields_are_reported_in_declaration_order_and_the_extras_last() {
        let errs = rejected(create_body(&obj("{}")));
        assert_eq!(locs(&errs), ["body/name", "body/password"]);
        assert_eq!(kinds(&errs), ["missing", "missing"]);
        assert_eq!(errs[0].msg, "Field required");

        let errs = rejected(room_user_body(&obj(r#"{"roomId":"x","nope":1}"#)));
        assert_eq!(locs(&errs), ["body/roomId", "body/userId", "body/nope"]);
        assert_eq!(kinds(&errs), ["string_pattern_mismatch", "missing", "extra_forbidden"]);
        assert_eq!(errs[2].msg, "Extra inputs are not permitted");
    }

    #[test]
    fn the_pattern_messages_quote_the_schema_verbatim() {
        let errs = rejected(join_body(&obj(r#"{"roomId":"lobby","password":"hunter22"}"#)));
        assert_eq!(kinds(&errs), ["string_pattern_mismatch"]);
        assert_eq!(errs[0].msg, "String should match pattern '^r_[A-Za-z0-9_-]{22}$'");

        // An uppercase uuid fails the pattern rather than reaching the database.
        let raw = format!(r#"{{"roomId":"{ROOM}","userId":"3F2504E0-4F89-11D3-9A0C-0305E82C3301"}}"#);
        let errs = rejected(room_user_body(&obj(&raw)));
        assert_eq!(locs(&errs), ["body/userId"]);
        assert_eq!(
            errs[0].msg,
            "String should match pattern \
             '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$'"
        );
    }

    #[test]
    fn a_wrong_type_is_never_coerced_and_a_cap_reads_as_a_length() {
        let errs = rejected(create_body(&obj(r#"{"name":5,"password":"hunter22"}"#)));
        assert_eq!(kinds(&errs), ["string_type"]);
        assert_eq!(errs[0].msg, "Input should be a valid string");
        // An explicit null is the wrong type, not a missing field.
        let errs = rejected(join_body(&obj(r#"{"roomId":null,"password":"x"}"#)));
        assert_eq!(kinds(&errs), ["string_type"]);

        let long = "a".repeat(FIELD_NAME_MAX + 1);
        let errs = rejected(rename_body(&obj(&format!(r#"{{"roomId":"{ROOM}","name":"{long}"}}"#))));
        assert_eq!(locs(&errs), ["body/name"]);
        assert_eq!(kinds(&errs), ["string_too_long"]);
        assert_eq!(errs[0].msg, "String should have at most 200 characters");

        let long = "a".repeat(FIELD_PW_MAX + 1);
        let errs = rejected(create_body(&obj(&format!(r#"{{"name":"Lab","password":"{long}"}}"#))));
        assert_eq!(errs[0].msg, "String should have at most 1024 characters");
    }

    #[test]
    fn the_names_and_passwords_the_route_refuses_itself_stay_strings() {
        // Pydantic only caps these at 200 / 1024, so "   " and "hi" are valid
        // as far as the schema goes and the route's own 422 -- a string -- is
        // what answers them. Converting these would CREATE a divergence.
        let body = create_body(&obj(r#"{"name":"   ","password":"hi"}"#))
            .expect("the schema accepts both");
        assert!(clean_room_name(&body.name).is_none());
        assert!(!password_ok(&body.password));
        let lobby = create_body(&obj(r#"{"name":" lObBy ","password":"hunter22"}"#))
            .expect("the schema accepts a reserved name");
        assert_eq!(room_name_key(&clean_room_name(&lobby.name).expect("a name")), "lobby");
    }

    #[test]
    fn sign_out_others_reads_pydantics_lax_booleans() {
        let p = |v: &str| {
            password_body(&obj(&format!(
                r#"{{"roomId":"{ROOM}","password":"hunter22","signOutOthers":{v}}}"#
            )))
        };
        for t in ["true", "\"yes\"", "\"ON\"", "\"t\"", "\"Y\"", "1", "1.0"] {
            assert!(p(t).expect("a lax true").sign_out_others, "{t}");
        }
        for f in ["false", "\"no\"", "\"OFF\"", "\"f\"", "0", "-0.0"] {
            assert!(!p(f).expect("a lax false").sign_out_others, "{f}");
        }
        // The right kind of scalar with an uninterpretable value, then values
        // that are not booleans at all.
        for (v, kind) in [
            ("\"\"", "bool_parsing"),
            ("\"2\"", "bool_parsing"),
            ("\" true\"", "bool_parsing"),
            ("2", "bool_parsing"),
            ("2.0", "bool_parsing"),
            ("0.5", "bool_type"),
            ("1e308", "bool_type"),
            ("null", "bool_type"),
            ("[]", "bool_type"),
            ("{}", "bool_type"),
        ] {
            let errs = rejected(p(v));
            assert_eq!(kinds(&errs), [kind], "{v}");
            assert_eq!(locs(&errs), ["body/signOutOthers"], "{v}");
        }
        assert_eq!(
            rejected(p("2"))[0].msg,
            "Input should be a valid boolean, unable to interpret input"
        );
        assert_eq!(rejected(p("null"))[0].msg, "Input should be a valid boolean");
        // Absent is the model default, and not an error.
        let kept = password_body(&obj(&format!(r#"{{"roomId":"{ROOM}","password":"hunter22"}}"#)))
            .expect("signOutOthers is optional");
        assert!(!kept.sign_out_others);
    }

    #[test]
    fn a_valid_body_round_trips_through_every_reader() {
        let ref_body = room_ref_body(&obj(&format!(r#"{{"roomId":"{ROOM}"}}"#)))
            .expect("a bare room reference");
        assert_eq!(ref_body.room_id, ROOM);
        let joined = join_body(&obj(&format!(r#"{{"roomId":"{ROOM}","password":"hunter22"}}"#)))
            .expect("a join");
        assert_eq!((joined.room_id.as_str(), joined.password.as_str()), (ROOM, "hunter22"));
        let uid = "3f2504e0-4f89-11d3-9a0c-0305e82c3301";
        let kick = room_user_body(&obj(&format!(r#"{{"roomId":"{ROOM}","userId":"{uid}"}}"#)))
            .expect("a kick");
        assert_eq!(kick.user_id, uid);
    }
}
