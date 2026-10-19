//! Music: what everyone is listening to, and listen-along rooms.
//!
//! Two things, both memory only (nothing here touches the database, and all of
//! it goes when the server restarts):
//!
//! **Now Playing.** A paired HQ reads the track its own machine is playing
//! (Spotify, Apple Music, a YouTube Music tab) and `PUT`s it to
//! `/v1/music/now`; `GET` lists everyone listening right now. A track is a
//! title, an artist, an album, which app, an optional Spotify or YouTube id and
//! a position. Every field is validated one by one -- an unknown key is a 422,
//! never a stored surprise -- and no URL is accepted: links are rebuilt by the
//! page from the ids. An entry no one refreshes for [`NOW_TTL_MS`] is gone.
//!
//! **Listen along.** Any Arena room can play a shared queue of YouTube videos.
//! The server keeps one [`RoomMusic`] per room and a clock: the current item
//! "started" at `startAt` (server epoch ms), so every page computes the same
//! position as `now - startAt` and seeks its own embedded player to it. The
//! audio never touches this server; each page plays the video from YouTube
//! itself. Messages ride the room socket as `{"type": "music", "op": ...}`:
//!
//!   sync                      answer me (only me) with the room's music state
//!   add   {v, title?}         queue a video (11-char YouTube id)
//!   remove {id}               drop a queued item
//!   play / pause              resume / pause the current item for everyone
//!   seek  {ms}                move the current item
//!   next  {id}                skip or "it ended": advances only while `id` is
//!                             still current, so ten pages saying "ended" at
//!                             once advance the queue exactly once
//!   dur   {id, ms}            the first page to learn an item's length says so
//!   swap  {id, v, title?}     the current item's upload won't play: put another
//!                             upload of the same song in its place, from the top
//!   clear                     empty the queue (the current item keeps playing)
//!   viz   {b: [16 x 0..255]}  the DJ's live spectrum, relayed to everyone else
//!                             in the room and never stored
//!
//! Every change is broadcast as `{"type": "music", "op": "state", "state",
//! "now", "by", "what"}`; `now` lets a page correct its clock against ours.

use axum::{
    extract::{Request, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::realtime::Bucket;
use crate::rooms::{py_printable, Member, RoomManager};

/// A Now Playing entry lives this long without a refresh. Pages refresh every
/// 30 s and on every change, so a closed laptop drops off within two minutes.
pub const NOW_TTL_MS: i64 = 120_000;
/// Listening entries kept at once; past this the stalest goes first.
pub const NOW_MAX: usize = 5_000;
/// A page may update its track this often (ms); faster is a 429.
pub const NOW_MIN_GAP_MS: i64 = 1_000;
/// The longest title / artist / album, in characters.
pub const TEXT_MAX: usize = 150;
/// Queue length per room, and how much of it one person may hold.
pub const QUEUE_MAX: usize = 50;
pub const QUEUE_PER_USER: usize = 10;
/// The longest a track or video may claim to be (a day), in ms.
pub const DUR_MAX_MS: i64 = 86_400_000;
/// Spectrum bands in a `viz` frame.
pub const VIZ_BANDS: usize = 16;
const BODY_MAX: usize = 4 * 1024;
const SOURCES: &[&str] = &["spotify", "apple", "ytmusic"];

pub fn routes() -> Router<crate::AppState> {
    Router::new().route("/v1/music/now", get(get_now).put(put_now).delete(delete_now))
}

/// Server time in epoch ms: the clock both features are measured on.
pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

// --- validation -----------------------------------------------------------

/// Clean free text the way room chat is cleaned: anything invisible or a
/// control character becomes a space, runs of whitespace fold to one, clipped.
pub fn clean_text(raw: &str, max: usize) -> String {
    let cleaned: String = raw.chars().map(|c| if py_printable(c) { c } else { ' ' }).collect();
    cleaned.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(max).collect()
}

pub fn is_youtube_id(s: &str) -> bool {
    s.len() == 11 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

pub fn is_spotify_id(s: &str) -> bool {
    s.len() == 22 && s.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// A whole number in `0..=max` (a float with no fraction counts, as JS sends them).
fn ms_in(v: &Value, max: i64) -> Option<i64> {
    let n = v.as_i64().or_else(|| v.as_f64().filter(|f| f.fract() == 0.0).map(|f| f as i64))?;
    (0..=max).contains(&n).then_some(n)
}

/// One validated track, as stored and as shown.
#[derive(Clone, Debug, PartialEq)]
pub struct Track {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub source: String,
    pub spotify_id: Option<String>,
    pub youtube_id: Option<String>,
    pub duration_ms: Option<i64>,
    pub position_ms: Option<i64>,
    pub playing: bool,
}

/// Validate a `PUT /v1/music/now` body field by field. Errors name the field.
pub fn read_track(v: &Value) -> Result<Track, String> {
    let obj = v.as_object().ok_or("body must be an object")?;
    const KEYS: &[&str] = &["title", "artist", "album", "source", "spotifyId", "youtubeId",
                            "durationMs", "positionMs", "playing"];
    if let Some(k) = obj.keys().find(|k| !KEYS.contains(&k.as_str())) {
        return Err(format!("{k}: unknown field"));
    }
    let text = |k: &str, required: bool| -> Result<String, String> {
        match obj.get(k) {
            None | Some(Value::Null) if !required => Ok(String::new()),
            Some(Value::String(s)) => {
                let t = clean_text(s, TEXT_MAX);
                if required && t.is_empty() { Err(format!("{k}: must not be empty")) } else { Ok(t) }
            }
            _ => Err(format!("{k}: must be a string")),
        }
    };
    let title = text("title", true)?;
    let artist = text("artist", false)?;
    let album = text("album", false)?;
    let source = match obj.get("source").and_then(|s| s.as_str()) {
        Some(s) if SOURCES.contains(&s) => s.to_string(),
        _ => return Err(format!("source: must be one of {}", SOURCES.join(", "))),
    };
    let id = |k: &str, ok: fn(&str) -> bool| -> Result<Option<String>, String> {
        match obj.get(k) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(s)) if ok(s) => Ok(Some(s.clone())),
            _ => Err(format!("{k}: not a valid id")),
        }
    };
    let spotify_id = id("spotifyId", is_spotify_id)?;
    let youtube_id = id("youtubeId", is_youtube_id)?;
    let num = |k: &str| -> Result<Option<i64>, String> {
        match obj.get(k) {
            None | Some(Value::Null) => Ok(None),
            Some(v) => ms_in(v, DUR_MAX_MS).map(Some).ok_or(format!("{k}: must be 0..{DUR_MAX_MS}")),
        }
    };
    let duration_ms = num("durationMs")?;
    let mut position_ms = num("positionMs")?;
    if let (Some(p), Some(d)) = (position_ms, duration_ms) {
        position_ms = Some(p.min(d));
    }
    let playing = match obj.get("playing") {
        None | Some(Value::Null) => true,
        Some(Value::Bool(b)) => *b,
        _ => return Err("playing: must be a boolean".into()),
    };
    Ok(Track { title, artist, album, source, spotify_id, youtube_id, duration_ms, position_ms, playing })
}

// --- the hub --------------------------------------------------------------

#[derive(Clone, Debug)]
struct Listening {
    who: Member,
    track: Track,
    /// Server ms of the last PUT: entries expire from here, and a playing
    /// track's position is projected forward from here.
    at: i64,
}

/// One queued (or current) video.
#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    pub id: String,
    pub v: String,
    pub title: String,
    pub by_id: String,
    pub by_name: String,
    pub dur_ms: Option<i64>,
}

impl Item {
    fn public(&self) -> Value {
        json!({"id": self.id, "v": self.v, "title": self.title,
               "by": {"userId": self.by_id, "displayName": self.by_name},
               "durMs": self.dur_ms})
    }
}

/// A room's listen-along state. Positions are in ms; `start_at` is the server
/// time at which the current item was at position 0.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RoomMusic {
    pub queue: Vec<Item>,
    pub cur: Option<Item>,
    pub playing: bool,
    pub start_at: i64,
    pub paused_pos: i64,
    pub rev: u64,
    seq: u64,
    /// Who last sent a spectrum, and when: pages label the DJ from this.
    pub dj: Option<(String, String, i64)>,
}

/// What an op did: changed the state (broadcast it), only needs answering to
/// the sender, or was refused with a reason.
#[derive(Debug, PartialEq)]
pub enum Outcome {
    Changed,
    Unchanged,
    Refused(String),
}

impl RoomMusic {
    /// Where the current item is right now.
    pub fn position(&self, now: i64) -> i64 {
        if self.cur.is_none() {
            return 0;
        }
        let p = if self.playing { now - self.start_at } else { self.paused_pos };
        let p = p.max(0);
        match self.cur.as_ref().and_then(|c| c.dur_ms) {
            Some(d) => p.min(d),
            None => p,
        }
    }

    pub fn public(&self, now: i64) -> Value {
        json!({
            "rev": self.rev,
            "cur": self.cur.as_ref().map(Item::public),
            "queue": self.queue.iter().map(Item::public).collect::<Vec<_>>(),
            "playing": self.playing && self.cur.is_some(),
            "startAt": self.start_at,
            "pos": self.position(now),
            "dj": self.dj.as_ref().filter(|d| now - d.2 < 10_000)
                     .map(|d| json!({"userId": d.0, "displayName": d.1})),
        })
    }

    fn start(&mut self, item: Item, now: i64) {
        self.cur = Some(item);
        self.playing = true;
        self.start_at = now;
        self.paused_pos = 0;
    }

    fn advance(&mut self, now: i64) {
        if self.queue.is_empty() {
            self.cur = None;
            self.playing = false;
            self.paused_pos = 0;
        } else {
            let next = self.queue.remove(0);
            self.start(next, now);
        }
    }

    /// Apply one op from `me`. Pure: the clock comes in as `now`.
    pub fn apply(&mut self, op: &str, msg: &Value, me: &Member, now: i64) -> Outcome {
        let out = self.apply_inner(op, msg, me, now);
        if out == Outcome::Changed {
            self.rev += 1;
        }
        out
    }

    fn apply_inner(&mut self, op: &str, msg: &Value, me: &Member, now: i64) -> Outcome {
        let cur_is = |s: &RoomMusic| {
            msg.get("id").and_then(|i| i.as_str())
                .is_some_and(|id| s.cur.as_ref().is_some_and(|c| c.id == id))
        };
        match op {
            "add" => {
                let Some(v) = msg.get("v").and_then(|v| v.as_str()).filter(|v| is_youtube_id(v)) else {
                    return Outcome::Refused("that isn't a YouTube video id".into());
                };
                if self.queue.len() >= QUEUE_MAX {
                    return Outcome::Refused(format!("the queue is full ({QUEUE_MAX})"));
                }
                if self.queue.iter().filter(|i| i.by_id == me.user_id).count() >= QUEUE_PER_USER {
                    return Outcome::Refused(format!("you already have {QUEUE_PER_USER} songs queued"));
                }
                let title = msg.get("title").and_then(|t| t.as_str())
                    .map(|t| clean_text(t, TEXT_MAX)).filter(|t| !t.is_empty())
                    .unwrap_or_else(|| "YouTube video".to_string());
                self.seq += 1;
                let item = Item {
                    id: format!("m{}", self.seq), v: v.to_string(), title,
                    by_id: me.user_id.clone(), by_name: clean_text(&me.display_name, 40),
                    dur_ms: None,
                };
                if self.cur.is_none() { self.start(item, now) } else { self.queue.push(item) }
                Outcome::Changed
            }
            "remove" => {
                let id = msg.get("id").and_then(|i| i.as_str()).unwrap_or("");
                let before = self.queue.len();
                self.queue.retain(|i| i.id != id);
                if self.queue.len() == before { Outcome::Unchanged } else { Outcome::Changed }
            }
            "clear" => {
                if self.queue.is_empty() { return Outcome::Unchanged; }
                self.queue.clear();
                Outcome::Changed
            }
            "play" => {
                if self.cur.is_none() || self.playing { return Outcome::Unchanged; }
                self.start_at = now - self.paused_pos;
                self.playing = true;
                Outcome::Changed
            }
            "pause" => {
                if self.cur.is_none() || !self.playing { return Outcome::Unchanged; }
                self.paused_pos = self.position(now);
                self.playing = false;
                Outcome::Changed
            }
            "seek" => {
                if self.cur.is_none() { return Outcome::Unchanged; }
                let Some(ms) = msg.get("ms").and_then(|m| ms_in(m, DUR_MAX_MS)) else {
                    return Outcome::Refused("seek: ms must be a whole number of milliseconds".into());
                };
                let ms = match self.cur.as_ref().and_then(|c| c.dur_ms) { Some(d) => ms.min(d), None => ms };
                if self.playing { self.start_at = now - ms } else { self.paused_pos = ms }
                Outcome::Changed
            }
            "next" => {
                if !cur_is(self) { return Outcome::Unchanged; }
                self.advance(now);
                Outcome::Changed
            }
            "swap" => {
                let Some(v) = msg.get("v").and_then(|v| v.as_str()).filter(|v| is_youtube_id(v)) else {
                    return Outcome::Refused("that isn't a YouTube video id".into());
                };
                if !cur_is(self) { return Outcome::Unchanged; }
                let title = msg.get("title").and_then(|t| t.as_str())
                    .map(|t| clean_text(t, TEXT_MAX)).filter(|t| !t.is_empty());
                let mut item = self.cur.take().expect("cur_is checked it");
                if item.v == v { self.cur = Some(item); return Outcome::Unchanged; }
                item.v = v.to_string();
                item.dur_ms = None;
                if let Some(t) = title { item.title = t; }
                self.start(item, now);
                Outcome::Changed
            }
            "dur" => {
                let Some(ms) = msg.get("ms").and_then(|m| ms_in(m, DUR_MAX_MS)).filter(|m| *m > 0) else {
                    return Outcome::Unchanged;
                };
                match self.cur.as_mut() {
                    Some(c) if c.dur_ms.is_none() && cur_is_id(msg, &c.id) => {
                        c.dur_ms = Some(ms);
                        Outcome::Changed
                    }
                    _ => Outcome::Unchanged,
                }
            }
            _ => Outcome::Refused(format!("music: unknown op {}", clean_text(op, 20))),
        }
    }
}

fn cur_is_id(msg: &Value, id: &str) -> bool {
    msg.get("id").and_then(|i| i.as_str()) == Some(id)
}

/// A spectrum frame: exactly [`VIZ_BANDS`] whole numbers 0..=255.
pub fn read_viz(msg: &Value) -> Option<Vec<u8>> {
    let b = msg.get("b")?.as_array()?;
    if b.len() != VIZ_BANDS {
        return None;
    }
    b.iter().map(|x| x.as_u64().filter(|n| *n <= 255).map(|n| n as u8)).collect()
}

#[derive(Default)]
struct Inner {
    now: HashMap<String, Listening>,
    rooms: HashMap<String, RoomMusic>,
    reaped_at: i64,
}

/// Process-wide music state, shared through `AppState`.
#[derive(Clone, Default)]
pub struct MusicHub {
    inner: Arc<Mutex<Inner>>,
}

/// Per-socket limits, held by the socket's own inbound task (like chat's).
pub struct ConnLimits {
    ops: Bucket,
    viz: Bucket,
}

impl ConnLimits {
    pub fn new() -> Self {
        let t = now_ms() as f64 / 1000.0;
        // Controls: 4 a second, bursts of 8. Spectrum: 15 frames a second.
        Self { ops: Bucket::new(4.0, 8.0, t), viz: Bucket::new(15.0, 15.0, t) }
    }
}

impl MusicHub {
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        // A panic while holding this lock cannot leave a half-applied op that
        // matters more than staying up: carry on with what is there.
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn set_now(&self, who: Member, track: Track, now: i64) -> Result<(), &'static str> {
        let mut g = self.lock();
        if let Some(prev) = g.now.get(&who.user_id) {
            if now - prev.at < NOW_MIN_GAP_MS && prev.track == track {
                return Err("slow down");
            }
        }
        g.now.retain(|_, l| now - l.at < NOW_TTL_MS);
        if g.now.len() >= NOW_MAX && !g.now.contains_key(&who.user_id) {
            if let Some(oldest) = g.now.iter().min_by_key(|(_, l)| l.at).map(|(k, _)| k.clone()) {
                g.now.remove(&oldest);
            }
        }
        g.now.insert(who.user_id.clone(), Listening { who, track, at: now });
        Ok(())
    }

    pub fn clear_now(&self, user_id: &str) {
        self.lock().now.remove(user_id);
    }

    /// Everyone listening, freshest first.
    pub fn list_now(&self, viewer: &str, now: i64) -> Value {
        let mut g = self.lock();
        g.now.retain(|_, l| now - l.at < NOW_TTL_MS);
        let mut rows: Vec<&Listening> = g.now.values().collect();
        rows.sort_by(|a, b| b.at.cmp(&a.at).then_with(|| a.who.user_id.cmp(&b.who.user_id)));
        let listening: Vec<Value> = rows.iter().map(|l| {
            let t = &l.track;
            let pos = t.position_ms.map(|p| {
                let p = if t.playing { p + (now - l.at) } else { p };
                t.duration_ms.map_or(p, |d| p.min(d))
            });
            let mut who = l.who.public();
            who["isYou"] = json!(l.who.user_id == viewer);
            json!({"user": who, "updatedAt": l.at, "track": {
                "title": t.title, "artist": t.artist, "album": t.album, "source": t.source,
                "spotifyId": t.spotify_id, "youtubeId": t.youtube_id,
                "durationMs": t.duration_ms, "positionMs": pos, "playing": t.playing}})
        }).collect();
        json!({"now": now, "ttlMs": NOW_TTL_MS, "listening": listening})
    }

    /// The room's state for one page, as a `music` message.
    pub fn state_msg(&self, room_id: &str, by: Option<&Member>, what: &str, now: i64) -> String {
        let g = self.lock();
        let st = g.rooms.get(room_id).cloned().unwrap_or_default();
        json!({"type": "music", "op": "state", "what": what, "now": now,
               "by": by.map(Member::public), "state": st.public(now)}).to_string()
    }

    /// Drop the music of rooms that no longer exist (at most once a minute).
    async fn reap(&self, rooms: &RoomManager, now: i64) {
        let ids: Vec<String> = {
            let mut g = self.lock();
            if now - g.reaped_at < 60_000 { return; }
            g.reaped_at = now;
            g.rooms.keys().cloned().collect()
        };
        let mut gone = Vec::new();
        for id in ids {
            if !rooms.exists(&id).await { gone.push(id); }
        }
        if !gone.is_empty() {
            let mut g = self.lock();
            for id in gone { g.rooms.remove(&id); }
        }
    }

    /// One `{"type": "music"}` frame from `me` on socket `conn_id` in `room_id`.
    pub async fn handle(&self, rooms: &RoomManager, room_id: &str, conn_id: u64, me: &Member,
                        lim: &mut ConnLimits, v: &Value) {
        let now = now_ms();
        let t = now as f64 / 1000.0;
        let op = v.get("op").and_then(|o| o.as_str()).unwrap_or("");
        let refuse = |why: String| json!({"type": "music", "op": "error", "error": why}).to_string();
        match op {
            "sync" => {
                rooms.send_conn(room_id, conn_id, self.state_msg(room_id, None, "sync", now)).await;
            }
            "viz" => {
                if !lim.viz.take(t) { return; }
                let Some(bands) = read_viz(v) else {
                    rooms.send_conn(room_id, conn_id, refuse("viz: 16 numbers 0..255".into())).await;
                    return;
                };
                let first = {
                    let mut g = self.lock();
                    let st = g.rooms.entry(room_id.to_string()).or_default();
                    let fresh = st.dj.as_ref().is_none_or(|d| d.0 != me.user_id || now - d.2 >= 10_000);
                    st.dj = Some((me.user_id.clone(), clean_text(&me.display_name, 40), now));
                    fresh
                };
                let frame = json!({"type": "music", "op": "viz", "from": me.user_id, "b": bands});
                rooms.send_where_except(room_id, conn_id, |_| true, frame.to_string()).await;
                // A new DJ: everyone's state shows who is spinning.
                if first {
                    rooms.broadcast(room_id, self.state_msg(room_id, Some(me), "dj", now)).await;
                }
            }
            _ => {
                if !lim.ops.take(t) {
                    rooms.send_conn(room_id, conn_id, refuse("music: slow down".into())).await;
                    return;
                }
                let out = {
                    let mut g = self.lock();
                    g.rooms.entry(room_id.to_string()).or_default().apply(op, v, me, now)
                };
                match out {
                    Outcome::Changed => {
                        rooms.broadcast(room_id, self.state_msg(room_id, Some(me), op, now)).await;
                    }
                    Outcome::Unchanged => {
                        rooms.send_conn(room_id, conn_id, self.state_msg(room_id, None, op, now)).await;
                    }
                    Outcome::Refused(why) => {
                        rooms.send_conn(room_id, conn_id, refuse(why)).await;
                    }
                }
                self.reap(rooms, now).await;
            }
        }
    }
}

// --- HTTP -----------------------------------------------------------------

fn caller(req: &Request) -> crate::Caller {
    req.extensions().get::<crate::Caller>().expect("caller set by middleware").clone()
}

fn member_of(c: &crate::Caller) -> Member {
    Member {
        user_id: c.user_id.clone(),
        handle: c.handle.clone(),
        display_name: if c.display_name.is_empty() { c.handle.clone() } else { c.display_name.clone() },
        avatar_url: c.avatar_url.clone(),
        cos: Value::Null,
    }
}

async fn get_now(State(st): State<crate::AppState>, req: Request) -> Response {
    let c = caller(&req);
    Json(st.music.list_now(&c.user_id, now_ms())).into_response()
}

async fn put_now(State(st): State<crate::AppState>, req: Request) -> Response {
    let c = caller(&req);
    let (_, body) = req.into_parts();
    let Ok(bytes) = axum::body::to_bytes(body, BODY_MAX).await else {
        return crate::err(StatusCode::PAYLOAD_TOO_LARGE, "body too large");
    };
    let Ok(v) = serde_json::from_slice::<Value>(&bytes) else {
        return crate::err(StatusCode::UNPROCESSABLE_ENTITY, "body must be JSON");
    };
    let track = match read_track(&v) {
        Ok(t) => t,
        Err(e) => return crate::err(StatusCode::UNPROCESSABLE_ENTITY, &e),
    };
    match st.music.set_now(member_of(&c), track, now_ms()) {
        Ok(()) => Json(json!({"ok": true, "ttlMs": NOW_TTL_MS})).into_response(),
        Err(e) => crate::err(StatusCode::TOO_MANY_REQUESTS, e),
    }
}

async fn delete_now(State(st): State<crate::AppState>, req: Request) -> Response {
    let c = caller(&req);
    st.music.clear_now(&c.user_id);
    Json(json!({"ok": true})).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn who(id: &str) -> Member {
        Member { user_id: id.into(), handle: id.into(), display_name: id.to_uppercase(),
                 avatar_url: String::new(), cos: Value::Null }
    }

    #[test]
    fn a_track_is_validated_field_by_field() {
        let ok = read_track(&json!({"title": "  Song\u{202E}  Name ", "artist": "A", "source": "spotify",
                                    "spotifyId": "4uLU6hMCjMI75M1A2tKUQC", "durationMs": 1000,
                                    "positionMs": 5000})).unwrap();
        assert_eq!(ok.title, "Song Name");
        assert_eq!(ok.position_ms, Some(1000)); // clamped to the duration
        assert!(ok.playing);
        assert!(read_track(&json!({"title": "x", "source": "napster"})).is_err());
        assert!(read_track(&json!({"title": "", "source": "apple"})).is_err());
        assert!(read_track(&json!({"title": "x", "source": "apple", "url": "https://evil"})).is_err());
        assert!(read_track(&json!({"title": "x", "source": "ytmusic", "youtubeId": "bad"})).is_err());
        assert!(read_track(&json!({"title": "x", "source": "apple", "durationMs": -1})).is_err());
        assert!(read_track(&json!({"title": "x".repeat(400), "source": "apple"})).unwrap().title.len() == TEXT_MAX);
    }

    #[test]
    fn now_playing_lists_fresh_entries_and_projects_position() {
        let hub = MusicHub::default();
        let t = read_track(&json!({"title": "T", "source": "apple", "durationMs": 100_000,
                                   "positionMs": 1000})).unwrap();
        hub.set_now(who("a"), t.clone(), 10_000).unwrap();
        let l = hub.list_now("a", 15_000);
        assert_eq!(l["listening"][0]["track"]["positionMs"], 6000);
        assert_eq!(l["listening"][0]["user"]["isYou"], true);
        // the same track again inside a second is refused; a change is not
        assert!(hub.set_now(who("a"), t.clone(), 10_500).is_err());
        let mut t2 = t.clone();
        t2.title = "Other".into();
        assert!(hub.set_now(who("a"), t2, 10_600).is_ok());
        // expired after the TTL
        assert_eq!(hub.list_now("b", 10_600 + NOW_TTL_MS)["listening"].as_array().unwrap().len(), 0);
        hub.set_now(who("b"), t, 1).unwrap();
        hub.clear_now("b");
        assert_eq!(hub.list_now("b", 2)["listening"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn the_first_add_plays_and_the_rest_queue() {
        let mut m = RoomMusic::default();
        let a = who("a");
        assert_eq!(m.apply("add", &json!({"v": "dQw4w9WgXcQ", "title": "One"}), &a, 1000), Outcome::Changed);
        assert_eq!(m.apply("add", &json!({"v": "aaaaaaaaaaa"}), &a, 2000), Outcome::Changed);
        assert!(matches!(m.apply("add", &json!({"v": "nope"}), &a, 2000), Outcome::Refused(_)));
        assert_eq!(m.cur.as_ref().unwrap().title, "One");
        assert_eq!(m.queue.len(), 1);
        assert_eq!(m.queue[0].title, "YouTube video");
        assert_eq!(m.position(4000), 3000);
        assert_eq!(m.rev, 2);
    }

    #[test]
    fn pause_play_and_seek_keep_one_clock() {
        let mut m = RoomMusic::default();
        let a = who("a");
        m.apply("add", &json!({"v": "dQw4w9WgXcQ"}), &a, 0);
        m.apply("pause", &json!({}), &a, 5000);
        assert_eq!(m.position(60_000), 5000);
        m.apply("play", &json!({}), &a, 10_000);
        assert_eq!(m.position(12_000), 7000);
        m.apply("seek", &json!({"ms": 1000}), &a, 20_000);
        assert_eq!(m.position(21_000), 2000);
        assert_eq!(m.apply("play", &json!({}), &a, 21_000), Outcome::Unchanged);
    }

    #[test]
    fn next_advances_once_however_many_pages_say_ended() {
        let mut m = RoomMusic::default();
        let a = who("a");
        m.apply("add", &json!({"v": "dQw4w9WgXcQ"}), &a, 0);
        m.apply("add", &json!({"v": "bbbbbbbbbbb"}), &a, 0);
        let first = m.cur.as_ref().unwrap().id.clone();
        assert_eq!(m.apply("next", &json!({"id": first}), &a, 9000), Outcome::Changed);
        assert_eq!(m.apply("next", &json!({"id": first}), &a, 9001), Outcome::Unchanged);
        assert_eq!(m.cur.as_ref().unwrap().v, "bbbbbbbbbbb");
        assert_eq!(m.position(9500), 500);
        let second = m.cur.as_ref().unwrap().id.clone();
        m.apply("next", &json!({"id": second}), &a, 10_000);
        assert!(m.cur.is_none());
        assert!(!m.public(10_000)["playing"].as_bool().unwrap());
    }

    #[test]
    fn queue_limits_and_duration() {
        let mut m = RoomMusic::default();
        let a = who("a");
        for _ in 0..=QUEUE_PER_USER {
            m.apply("add", &json!({"v": "dQw4w9WgXcQ"}), &a, 0);
        }
        assert!(matches!(m.apply("add", &json!({"v": "dQw4w9WgXcQ"}), &a, 0), Outcome::Refused(_)));
        assert_eq!(m.apply("add", &json!({"v": "dQw4w9WgXcQ"}), &who("b"), 0), Outcome::Changed);
        let id = m.cur.as_ref().unwrap().id.clone();
        assert_eq!(m.apply("dur", &json!({"id": id, "ms": 3000}), &a, 0), Outcome::Changed);
        assert_eq!(m.apply("dur", &json!({"id": id, "ms": 9000}), &a, 0), Outcome::Unchanged);
        assert_eq!(m.position(50_000), 3000);
        let q = m.queue[0].id.clone();
        assert_eq!(m.apply("remove", &json!({"id": q}), &a, 0), Outcome::Changed);
        assert_eq!(m.apply("clear", &json!({}), &a, 0), Outcome::Changed);
        assert!(m.queue.is_empty());
        assert!(matches!(m.apply("dance", &json!({}), &a, 0), Outcome::Refused(_)));
    }

    #[test]
    fn swap_replaces_the_current_upload_in_place() {
        let mut m = RoomMusic::default();
        let a = who("a");
        m.apply("add", &json!({"v": "dQw4w9WgXcQ", "title": "Song"}), &a, 0);
        m.apply("add", &json!({"v": "bbbbbbbbbbb"}), &a, 0);
        let id = m.cur.as_ref().unwrap().id.clone();
        m.apply("dur", &json!({"id": id, "ms": 5000}), &a, 0);
        assert_eq!(m.apply("swap", &json!({"id": id, "v": "ccccccccccc", "title": "Song (lyrics)"}), &a, 7000), Outcome::Changed);
        let cur = m.cur.as_ref().unwrap();
        assert_eq!((cur.id.as_str(), cur.v.as_str(), cur.title.as_str(), cur.dur_ms), (id.as_str(), "ccccccccccc", "Song (lyrics)", None));
        assert_eq!(m.position(8000), 1000);   // from the top
        assert_eq!(m.queue.len(), 1);         // the queue keeps its order
        assert_eq!(m.apply("swap", &json!({"id": "m99", "v": "ddddddddddd"}), &a, 0), Outcome::Unchanged);
        assert!(matches!(m.apply("swap", &json!({"id": id, "v": "bad"}), &a, 0), Outcome::Refused(_)));
    }

    #[test]
    fn viz_frames_are_sixteen_bytes() {
        assert_eq!(read_viz(&json!({"b": vec![0; 16]})).unwrap().len(), 16);
        assert!(read_viz(&json!({"b": vec![0; 15]})).is_none());
        assert!(read_viz(&json!({"b": vec![256; 16]})).is_none());
        assert!(read_viz(&json!({"b": "x"})).is_none());
    }

    #[tokio::test]
    async fn a_room_socket_sees_state_and_relayed_viz() {
        let rooms = RoomManager::new();
        let hub = MusicHub::default();
        let (a, b) = (who("a"), who("b"));
        let (_rx_a, mut da, _, _) = rooms.join_direct("r1", 1, a.clone()).await.unwrap();
        let (mut rx_b, mut db, _, _) = rooms.join_direct("r1", 2, b.clone()).await.unwrap();
        let mut lim = ConnLimits::new();
        hub.handle(&rooms, "r1", 1, &a, &mut lim, &json!({"op": "add", "v": "dQw4w9WgXcQ", "title": "Hi"})).await;
        let got: Value = serde_json::from_str(&rx_b.recv().await.unwrap()).unwrap();
        assert_eq!(got["op"], "state");
        assert_eq!(got["state"]["cur"]["title"], "Hi");
        assert_eq!(got["by"]["userId"], "a");
        hub.handle(&rooms, "r1", 1, &a, &mut lim, &json!({"op": "viz", "b": vec![7; 16]})).await;
        let relayed: Value = serde_json::from_str(&db.recv().await.unwrap()).unwrap();
        assert_eq!(relayed["op"], "viz");
        assert_eq!(relayed["b"][0], 7);
        // the sender does not get its own spectrum back
        hub.handle(&rooms, "r1", 2, &b, &mut lim, &json!({"op": "sync"})).await;
        let synced: Value = serde_json::from_str(&db.recv().await.unwrap()).unwrap();
        assert_eq!(synced["what"], "sync");
        assert_eq!(synced["state"]["dj"]["userId"], "a");
        assert!(da.try_recv().is_err());
    }
}
