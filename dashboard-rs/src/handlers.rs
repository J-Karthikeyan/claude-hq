//! The request validators and flows dashboard.py runs before anything reaches
//! the Arena: pantry bodies and the eat flow (with the local meal ledger),
//! quest rewards, cali orders, room bodies, and the soundboard.

use crate::app::{is_rid, is_uuid, App, HANDLE_RE};
use crate::arena::{CALI_ITEM_KEYS, CALI_TACO_KEYS, PANTRY_ACTIONS, ROOM_ID_RE, USER_ID_RE};
use crate::fatigue::{self, food_effect, is_food};
use crate::payload::epoch;
use crate::py::{self, Obj};
use base64::Engine;
use regex::Regex;
use serde_json::{json, Map, Value};
use std::collections::HashSet;
use std::path::Path;
use std::sync::LazyLock;

pub type Resp = (u16, Value);

fn int_in(v: Option<&Value>, lo: i64, hi: i64) -> Option<i64> {
    v.and_then(py::strict_int).filter(|n| (lo..=hi).contains(n))
}

fn food_kind(v: Option<&Value>) -> Option<&str> {
    v.and_then(|v| v.as_str()).filter(|k| is_food(k))
}

/// Validate a page pantry request -> the clean body for the Arena, or an error.
/// The clean body never contains a sessionId.
pub fn pantry_body(action: &str, body: &Value) -> Result<Value, &'static str> {
    if !PANTRY_ACTIONS.contains(&action) {
        return Err("unknown pantry action");
    }
    if action == "claim" {
        return Ok(json!({}));
    }
    let empty = Map::new();
    let b = body.as_object().unwrap_or(&empty);
    let rid = b.get("requestId").and_then(|v| v.as_str()).filter(|r| is_rid(r)).ok_or("invalid requestId")?;
    let kind = b.get("kind");
    match action {
        "eat" => {
            let k = food_kind(kind).ok_or("unknown food")?;
            Ok(json!({"requestId": rid, "kind": k}))
        }
        "buy" => {
            let k = food_kind(kind).ok_or("unknown food")?;
            let qty = match b.get("qty") {
                None => Some(1),
                q => int_in(q, 1, 5),
            }
            .ok_or("qty must be a whole number from 1 to 5")?;
            Ok(json!({"requestId": rid, "kind": k, "qty": qty}))
        }
        _ => {
            let to = b.get("toHandle").and_then(|v| v.as_str()).map(py::strip).unwrap_or("");
            if !HANDLE_RE.is_match(to) {
                return Err("invalid handle");
            }
            let coins = match b.get("coins") {
                None => Some(0),
                c => int_in(c, 0, 5),
            }
            .ok_or("coins must be a whole number from 0 to 5")?;
            let qty = match b.get("qty") {
                None => Some(0),
                q => int_in(q, 0, 3),
            }
            .ok_or("qty must be a whole number from 0 to 3")?;
            let kind = kind.filter(|k| !k.is_null());
            if kind.is_some() && food_kind(kind).is_none() {
                return Err("unknown food");
            }
            if qty > 0 && kind.is_none() {
                return Err("pick a food for that amount");
            }
            if kind.is_some() && qty == 0 {
                return Err("pick an amount for that food");
            }
            if coins == 0 && qty == 0 {
                return Err("a gift needs Poke Coins or food");
            }
            let note = b.get("note").and_then(|v| v.as_str()).map(|n| py::clean_note(n, 80)).unwrap_or_default();
            let mut clean = json!({"requestId": rid, "toHandle": to, "coins": coins, "qty": qty, "note": note});
            if let Some(k) = kind {
                clean["kind"] = k.clone();
            }
            Ok(clean)
        }
    }
}

/// Stamp the LOCAL effect of each food onto a server catalog.
pub fn overlay_food_effects(resp: Value) -> Value {
    let Value::Object(mut r) = resp else { return resp };
    let Some(Value::Array(items)) = r.get("catalog") else { return Value::Object(r) };
    let mut catalog = Vec::new();
    for item in items {
        let Value::Object(item) = item else { continue };
        let Some(kind) = food_kind(item.get("kind")) else { continue };
        let (secs, revives) = food_effect(kind).unwrap();
        let mut it = item.clone();
        it.insert("restoreMins".into(), json!(secs / 60));
        it.insert("revives".into(), json!(revives));
        it.insert(
            "wakeToMins".into(),
            if revives { json!((fatigue::REVIVE_TO_SECS as i64 - secs).max(0) / 60) } else { Value::Null },
        );
        catalog.push(Value::Object(it));
    }
    r.insert("catalog".into(), Value::Array(catalog));
    Value::Object(r)
}

pub fn meal_view(kind: &str, at: f64) -> Value {
    let (secs, revives) = food_effect(kind).unwrap_or((0, false));
    json!({"kind": kind, "at": py::iso_from_ts(at), "restoreMins": secs / 60, "revives": revives})
}

static QUEST_RID_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(quest|ach):[a-z0-9_]+:.{1,60}$").unwrap());

/// Validate + forward a quest/achievement reward claim.
pub fn quest_reward(app: &App, body: &Value) -> Resp {
    let rid = body.get("requestId").and_then(|v| v.as_str()).filter(|r| QUEST_RID_RE.is_match(r));
    let Some(rid) = rid else { return (400, json!({"error": "invalid requestId"})) };
    let Some(kind) = body.get("kind").and_then(|v| v.as_str()).filter(|k| ["quest", "achievement"].contains(k)) else {
        return (400, json!({"error": "kind must be quest or achievement"}));
    };
    let Some(qid) = body.get("questId").and_then(|v| v.as_str()).filter(|q| (1..=40).contains(&q.chars().count())) else {
        return (400, json!({"error": "invalid questId"}));
    };
    let tier = match body.get("tier") {
        None | Some(Value::Null) => None,
        Some(Value::String(t)) if ["bronze", "silver", "gold"].contains(&t.as_str()) => Some(t.as_str()),
        _ => return (400, json!({"error": "invalid tier"})),
    };
    let Some(coins) = int_in(body.get("coins"), 1, 15) else { return (400, json!({"error": "coins must be 1-15"})) };
    let (code, resp) = app.arena_quest_reward(rid, kind, qid, tier, coins);
    (if code == 0 { 502 } else { code }, resp)
}

const CALI_MAX_DINERS: usize = 20;
const CALI_MAX_PER_VARIANT: i64 = 50;
pub const CALI_MAX_PER_ITEM: i64 = 20;
const CALI_NAME_MAX: usize = 40;
static CALI_DATE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\d{4}-\d{2}-\d{2}$").unwrap());

/// Validate a page cali order -> the clean body, or an error.
pub fn cali_body(body: &Value) -> Result<Value, String> {
    let empty = Map::new();
    let b = body.as_object().unwrap_or(&empty);
    let rid = b.get("requestId").and_then(|v| v.as_str()).filter(|r| is_rid(r)).ok_or("invalid requestId")?;
    let diners = match b.get("diners") {
        Some(Value::Array(d)) if (1..=CALI_MAX_DINERS).contains(&d.len()) => d,
        _ => return Err(format!("an order needs 1 to {} diners", CALI_MAX_DINERS)),
    };
    let mut clean_diners = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for d in diners {
        let Value::Object(d) = d else { return Err("each diner must be an object".into()) };
        let handle = d.get("handle").and_then(|v| v.as_str()).map(py::strip).unwrap_or("").to_string();
        if !handle.is_empty() && !HANDLE_RE.is_match(&handle) {
            return Err("invalid handle".into());
        }
        let name = d.get("name").and_then(|v| v.as_str()).map(|n| py::clean_note(n, CALI_NAME_MAX)).unwrap_or_default();
        if handle.is_empty() && name.is_empty() {
            return Err("every diner needs a handle or a name".into());
        }
        let key = if !handle.is_empty() { format!("@{}", handle.to_lowercase()) } else { format!("#{}", name.to_lowercase()) };
        if !seen.insert(key) {
            return Err("the same diner is listed twice".into());
        }
        let empty_t = Map::new();
        let tacos = d.get("tacos").and_then(|v| v.as_object()).unwrap_or(&empty_t);
        let mut counts = Map::new();
        for k in CALI_TACO_KEYS {
            let v = match tacos.get(k) {
                None => Some(0),
                x => int_in(x, 0, CALI_MAX_PER_VARIANT),
            };
            let Some(v) = v else { return Err(format!("each taco count must be a whole number from 0 to {}", CALI_MAX_PER_VARIANT)) };
            counts.insert(k.into(), json!(v));
        }
        let mut row = Map::new();
        row.insert("tacos".into(), Value::Object(counts));
        let items = match d.get("items") {
            None | Some(Value::Null) => None,
            Some(Value::Object(o)) => Some(o),
            _ => return Err("items must be an object of menu item counts".into()),
        };
        let mut kept: Map<String, Value> = Map::new();
        for (k, v) in items.into_iter().flatten() {
            if !CALI_ITEM_KEYS.contains(&k.as_str()) {
                return Err("unknown menu item".into());
            }
            let Some(n) = int_in(Some(v), 0, CALI_MAX_PER_ITEM) else {
                return Err(format!("each menu item count must be a whole number from 0 to {}", CALI_MAX_PER_ITEM));
            };
            if n > 0 {
                kept.insert(k.clone(), json!(n));
            }
        }
        if !kept.is_empty() {
            let ordered: Map<String, Value> = CALI_ITEM_KEYS.iter().filter_map(|k| kept.get(*k).map(|v| (k.to_string(), v.clone()))).collect();
            row.insert("items".into(), Value::Object(ordered));
        }
        if !handle.is_empty() {
            row.insert("handle".into(), json!(handle));
        }
        if !name.is_empty() {
            row.insert("name".into(), json!(name));
        }
        clean_diners.push(Value::Object(row));
    }
    let mut clean = json!({"requestId": rid, "diners": clean_diners});
    match b.get("date") {
        None | Some(Value::Null) => {}
        Some(Value::String(w)) if CALI_DATE_RE.is_match(w) => clean["date"] = json!(w),
        _ => return Err("date must look like YYYY-MM-DD".into()),
    }
    clean["note"] = json!(b.get("note").and_then(|v| v.as_str()).map(|n| py::clean_note(n, 80)).unwrap_or_default());
    Ok(clean)
}

pub const ARENA_ROOM_POSTS: [&str; 8] = [
    "/api/arena/rooms/create", "/api/arena/rooms/join", "/api/arena/rooms/leave", "/api/arena/rooms/rename",
    "/api/arena/rooms/password", "/api/arena/rooms/kick", "/api/arena/rooms/unban", "/api/arena/rooms/delete",
];

fn pw_ok(v: Option<&Value>) -> bool {
    v.and_then(|v| v.as_str()).map(|p| (1..=1024).contains(&p.chars().count())).unwrap_or(false)
}

fn name_ok(v: Option<&Value>) -> bool {
    v.and_then(|v| v.as_str()).map(|n| n.chars().count() <= 200 && !py::strip(n).is_empty()).unwrap_or(false)
}

/// The first problem with a room request body, or None.
pub fn room_body_error(path: &str, body: &Obj) -> Option<&'static str> {
    let op = path.rsplit_once('/').map(|(_, o)| o).unwrap_or("");
    if op == "create" {
        if !name_ok(body.get("name")) {
            return Some("room name required");
        }
        if !pw_ok(body.get("password")) {
            return Some("password required");
        }
        return None;
    }
    if !["join", "leave", "rename", "password", "kick", "unban", "delete"].contains(&op) {
        return Some("not found");
    }
    if !body.get("roomId").and_then(|v| v.as_str()).map(|r| ROOM_ID_RE.is_match(r)).unwrap_or(false) {
        return Some("roomId required");
    }
    if op == "join" && !pw_ok(body.get("password")) {
        return Some("password required");
    }
    if op == "rename" && !name_ok(body.get("name")) {
        return Some("room name required");
    }
    if op == "password" {
        if !pw_ok(body.get("password")) {
            return Some("password required");
        }
        if !matches!(body.get("signOutOthers"), None | Some(Value::Null) | Some(Value::Bool(_))) {
            return Some("signOutOthers must be true or false");
        }
    }
    if ["kick", "unban"].contains(&op) && !body.get("userId").and_then(|v| v.as_str()).map(|u| USER_ID_RE.is_match(u)).unwrap_or(false) {
        return Some("userId required");
    }
    None
}

pub fn room_post(app: &App, path: &str, body: &Obj) -> Resp {
    match room_body_error(path, body) {
        Some("not found") => return (404, json!({"error": "not found"})),
        Some(e) => return (400, json!({"error": e})),
        None => {}
    }
    let op = path.rsplit_once('/').map(|(_, o)| o).unwrap_or("");
    let (code, resp) = app.arena_room_post(op, body);
    (if code == 0 { 502 } else { code }, resp)
}

impl App {
    /// The session a snack is for: its payload entry, else a stub carrying the
    /// fatigue the drawer showed. None if no transcript has that id.
    fn eat_target(&self, sid: &str) -> Option<Value> {
        if let Some(s) = self.payload_session(sid) {
            return Some(s);
        }
        let agg = self.find_transcript(sid).and_then(|p| self.scan_file(&p))?;
        let fz = self.offpayload_fatigue(sid, Some(&agg));
        Some(json!({"sessionId": sid, "creature": match fz { Some(f) => json!({"fatigue": f}), None => json!({}) }}))
    }

    /// Feed one session's creature.
    pub fn pantry_eat(&self, body: &Value) -> Resp {
        let sid = body.get("sessionId").and_then(|v| v.as_str()).filter(|s| is_uuid(s));
        let Some(sid) = sid else { return (400, json!({"error": "invalid sessionId"})) };
        let clean = match pantry_body("eat", body) {
            Ok(c) => c,
            Err(e) => return (400, json!({"error": e})),
        };
        let rid = clean["requestId"].as_str().unwrap().to_string();
        let kind = clean["kind"].as_str().unwrap().to_string();
        let retry = body.get("retry") == Some(&Value::Bool(true));
        let sess_obj = |s: Option<Value>| -> Obj { s.and_then(|v| v.as_object().cloned()).unwrap_or_else(|| json!({"sessionId": sid}).as_object().unwrap().clone()) };

        if let Some(m) = self.find_meal(&rid) {
            if m.session_id == sid && m.kind == kind {
                let sess = sess_obj(self.payload_session(sid));
                return (200, json!({"op": "eat", "replayed": true, "kind": kind, "sessionId": sid,
                                    "meal": meal_view(&kind, m.at), "fatigue": self.session_fatigue_now(&sess)}));
            }
            return (409, json!({"error": "that requestId was already used for a different meal", "code": "rid_reused"}));
        }
        {
            let mut inflight = self.meals_lock.lock().unwrap();
            if !inflight.insert(rid.clone()) {
                return (202, json!({"pending": true, "requestId": rid}));
            }
        }
        let out = self.pantry_eat_inner(sid, &rid, &kind, retry, &clean);
        self.meals_lock.lock().unwrap().remove(&rid);
        out
    }

    fn pantry_eat_inner(&self, sid: &str, rid: &str, kind: &str, retry: bool, clean: &Value) -> Resp {
        let Some(sess) = self.eat_target(sid) else { return (404, json!({"error": "unknown session"})) };
        let fz = sess.get("creature").and_then(|c| c.get("fatigue")).cloned();
        let Some(fz) = fz.filter(|f| !f.is_null()) else {
            return (409, json!({"error": "Creature energy is turned off in Settings", "code": "fatigue_off"}));
        };
        if !retry {
            let state = fz.get("state").and_then(|v| v.as_str()).unwrap_or("");
            let revives = food_effect(kind).map(|e| e.1).unwrap_or(false);
            if state == "rested" {
                return (409, json!({"error": "This creature is full of energy: no snack needed", "code": "not_hungry"}));
            }
            if state == "unconscious" && !revives {
                return (409, json!({"error": "This creature has fainted: only a revive item like a Revive Tonic (or a break) can wake it", "code": "fainted"}));
            }
            if revives && state != "unconscious" {
                return (409, json!({"error": "Revive items only work on a fainted creature", "code": "not_fainted"}));
            }
        }
        let (code, resp) = self.arena_pantry(Some("eat"), Some(clean));
        if code == 200 && resp.get("kind").and_then(|k| k.as_str()) == Some(kind) {
            let now = py::now_ts();
            let e = epoch(resp.get("at"));
            let at = (if e != 0.0 { e } else { now }).min(now);
            let entry = {
                let _g = self.meals_lock.lock().unwrap();
                self.record_meal_locked(rid, sid, kind, at, None)
            };
            let Ok(entry) = entry else {
                return (202, json!({"pending": true, "requestId": rid, "error": "Ate it, but couldn't save the meal on this machine. Retrying…"}));
            };
            if entry.session_id != sid || entry.kind != kind {
                return (409, json!({"error": "that requestId was already used for a different meal", "code": "rid_reused"}));
            }
            self.invalidate_payload_memo();
            let mut out = overlay_food_effects(resp);
            let sess_o = sess.as_object().cloned().unwrap_or_default();
            out["sessionId"] = json!(sid);
            out["meal"] = meal_view(kind, entry.at);
            out["fatigue"] = self.session_fatigue_now(&sess_o);
            return (200, out);
        }
        if code == 0 || code >= 500 {
            return (202, json!({"pending": true, "requestId": rid, "error": "The Arena didn't answer. Your snack is safe: Claude HQ will retry it."}));
        }
        if code == 200 {
            return (502, json!({"error": "the Arena sent an unexpected answer"}));
        }
        (code, resp)
    }

    // ---- soundboard ----------------------------------------------------------

    /// [{name,file,size}] for the audio files in ./sounds.
    pub fn local_sounds(&self) -> Vec<Value> {
        let Ok(rd) = std::fs::read_dir(&self.paths.sounds_dir) else { return Vec::new() };
        let mut names: Vec<String> = rd.filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        names
            .into_iter()
            .filter_map(|fn_| {
                let (stem, ext) = splitext(&fn_);
                sound_type(&ext.to_lowercase())?;
                let p = self.paths.sounds_dir.join(&fn_);
                let m = std::fs::metadata(&p).ok().filter(|m| m.is_file())?;
                Some(json!({"name": stem, "file": fn_, "size": m.len()}))
            })
            .collect()
    }

    /// Accept a browser upload ({name, data:<base64>}) and store the clip.
    pub fn receive_sound_upload(&self, body: &Obj) -> Resp {
        let name = body.get("name").and_then(|v| v.as_str()).filter(|n| !py::strip(n).is_empty());
        let Some(name) = name else { return (400, json!({"error": "name required"})) };
        let data = body.get("data").and_then(|v| v.as_str()).filter(|d| !d.is_empty());
        let Some(data) = data else { return (400, json!({"error": "file data required"})) };
        let base = py::strip(name.rsplit('/').next().unwrap_or("")).to_string();
        let ext = splitext(&base).1.to_lowercase();
        if base.is_empty() || base.starts_with('.') || base.contains('/') || base.contains('\\') || base.contains("..") || sound_type(&ext).is_none() {
            let mut exts: Vec<&str> = SOUND_TYPES.iter().map(|(e, _)| *e).collect();
            exts.sort();
            return (400, json!({"error": format!("must be an audio file ({})", exts.join(", "))}));
        }
        let Ok(raw) = base64::engine::general_purpose::STANDARD.decode(data) else {
            return (400, json!({"error": "invalid file encoding"}));
        };
        if raw.is_empty() {
            return (400, json!({"error": "empty file"}));
        }
        if raw.len() > MAX_SOUND_UPLOAD {
            return (413, json!({"error": format!("file too large (max {} MB)", MAX_SOUND_UPLOAD / (1024 * 1024))}));
        }
        if self.is_paired() {
            let (code, resp) = self.arena_upload_sound(&base, &raw, sound_type(&ext).unwrap());
            return (if code == 0 { 502 } else { code }, resp);
        }
        let dir = &self.paths.sounds_dir;
        if let Err(e) = std::fs::create_dir_all(dir) {
            return (500, json!({"error": format!("could not save: {}", e)}));
        }
        let dest = dir.join(&base);
        if crate::state::realpath(dest.parent().unwrap()) != crate::state::realpath(dir) {
            return (400, json!({"error": "bad name"}));
        }
        if dest.exists() {
            return (409, json!({"error": "a sound with that name already exists"}));
        }
        let tmp = dir.join(format!("{}.part", base));
        if let Err(e) = std::fs::write(&tmp, &raw).and_then(|_| std::fs::rename(&tmp, &dest)) {
            return (500, json!({"error": format!("could not save: {}", e)}));
        }
        (201, json!({"name": splitext(&base).0, "file": base, "size": raw.len(), "source": "local"}))
    }

    /// A local clip's bytes for GET /api/arena/sounds/<file>, if it is one.
    pub fn local_sound_file(&self, fn_: &str) -> Option<std::path::PathBuf> {
        let lp = self.paths.sounds_dir.join(fn_);
        let ok = lp.is_file()
            && crate::state::realpath(&lp).parent().map(Path::to_path_buf) == Some(crate::state::realpath(&self.paths.sounds_dir));
        ok.then_some(lp)
    }
}

pub const SOUND_TYPES: [(&str, &str); 5] =
    [(".ogg", "audio/ogg"), (".mp3", "audio/mpeg"), (".wav", "audio/wav"), (".m4a", "audio/mp4"), (".webm", "audio/webm")];
pub const MAX_SOUND_UPLOAD: usize = 5 * 1024 * 1024;

pub fn sound_type(ext: &str) -> Option<&'static str> {
    SOUND_TYPES.iter().find(|(e, _)| *e == ext).map(|(_, t)| *t)
}

/// os.path.splitext: (root, ext) where a leading dot is not an extension.
pub fn splitext(name: &str) -> (String, String) {
    let base_start = name.rfind('/').map(|i| i + 1).unwrap_or(0);
    let base = &name[base_start..];
    let lead = base.len() - base.trim_start_matches('.').len();
    match base[lead..].rfind('.') {
        Some(i) => {
            let cut = base_start + lead + i;
            (name[..cut].to_string(), name[cut..].to_string())
        }
        None => (name.to_string(), String::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::testutil::*;
    use crate::arena::tests::{leaks, stub};
    use std::sync::{Arc, Mutex};

    const SID: &str = "11111111-1111-4111-8111-111111111111";
    const OTHER: &str = "22222222-2222-4222-8222-222222222222";
    const RID: &str = "a1b2c3d4-e5f6-4a7b-8c9d-0e1f2a3b4c5d";

    #[test]
    fn pantry_bodies() {
        assert_eq!(pantry_body("claim", &json!({"requestId": RID, "sessionId": SID})), Ok(json!({})));
        assert_eq!(pantry_body("steal", &json!({"requestId": RID})), Err("unknown pantry action"));
        for bad in [json!(null), json!(7), json!("short"), json!("has a space here!!"), json!("x".repeat(65)), json!(format!("{}\n", RID)), json!("claim:2026-09-28")] {
            assert_eq!(pantry_body("buy", &json!({"requestId": bad, "kind": "berry"})), Err("invalid requestId"));
        }
        assert_eq!(pantry_body("buy", &json!({"requestId": RID, "kind": "bento", "sessionId": SID, "title": "x"})), Ok(json!({"requestId": RID, "kind": "bento", "qty": 1})));
        for bad in [json!(true), json!("2"), json!(0), json!(6), json!(1.0), json!(null)] {
            assert_eq!(pantry_body("buy", &json!({"requestId": RID, "kind": "berry", "qty": bad})), Err("qty must be a whole number from 1 to 5"));
        }
        assert_eq!(pantry_body("eat", &json!({"requestId": RID})), Err("unknown food"));
        assert_eq!(
            pantry_body("give", &json!({"requestId": RID, "toHandle": "  gary ", "coins": 2, "kind": "berry", "qty": 1, "note": "hi", "sessionId": SID})),
            Ok(json!({"requestId": RID, "toHandle": "gary", "coins": 2, "qty": 1, "note": "hi", "kind": "berry"}))
        );
        let base = |extra: Value| {
            let mut b = json!({"requestId": RID, "toHandle": "gary"});
            for (k, v) in extra.as_object().unwrap() {
                b[k] = v.clone();
            }
            pantry_body("give", &b)
        };
        for (extra, want) in [
            (json!({}), "a gift needs Poke Coins or food"),
            (json!({"qty": 1}), "pick a food for that amount"),
            (json!({"kind": "berry"}), "pick an amount for that food"),
            (json!({"kind": "pizza", "qty": 1}), "unknown food"),
            (json!({"coins": 6}), "coins must be a whole number from 0 to 5"),
            (json!({"coins": true}), "coins must be a whole number from 0 to 5"),
            (json!({"kind": "berry", "qty": 4}), "qty must be a whole number from 0 to 3"),
            (json!({"toHandle": "ga ry", "coins": 1}), "invalid handle"),
            (json!({"toHandle": null, "coins": 1}), "invalid handle"),
        ] {
            assert_eq!(base(extra.clone()), Err(want), "{}", extra);
        }
        let note = format!("  for   your \u{0}sleepy\tVoltkit \u{1b}{}", "z".repeat(200));
        let c = pantry_body("give", &json!({"requestId": RID, "toHandle": "gary", "coins": 1, "note": note})).unwrap();
        assert_eq!(c["note"].as_str().unwrap().chars().count(), 80);
        assert!(c["note"].as_str().unwrap().starts_with("for your sleepyVoltkit zz"));
    }

    #[test]
    fn overlay() {
        let server = json!({"coins": 3, "catalog": [{"kind": "berry", "restoreMins": 999, "price": 1}, {"kind": "tonic", "price": 5},
                                                     {"kind": "pizza", "price": 1}, "junk"]});
        let out = overlay_food_effects(server.clone());
        assert_eq!(out["catalog"].as_array().unwrap().len(), 2);
        assert_eq!((out["catalog"][0]["restoreMins"].clone(), out["catalog"][0]["wakeToMins"].clone()), (json!(20), json!(null)));
        assert_eq!((out["catalog"][1]["revives"].clone(), out["catalog"][1]["wakeToMins"].clone()), (json!(true), json!(105)));
        assert_eq!(out["coins"], 3);
        let e = overlay_food_effects(json!({"catalog": [{"kind": "elixir", "price": 7}]}));
        assert_eq!((e["catalog"][0]["restoreMins"].clone(), e["catalog"][0]["wakeToMins"].clone()), (json!(60), json!(45)));
        for r in [json!({"error": "not paired"}), json!({"catalog": "x"}), json!([]), json!(null)] {
            assert_eq!(overlay_food_effects(r.clone()), r);
        }
    }

    #[test]
    fn cali_orders() {
        let counts = |mh: i64| json!({"mildHard": mh, "mildSoft": 0, "wildHard": 0, "wildSoft": 0});
        let order = |diners: Value| json!({"requestId": RID, "diners": diners});
        let c = cali_body(&order(json!([{"name": "Ana", "tacos": {"mildHard": 3}}]))).unwrap();
        assert_eq!(c, json!({"requestId": RID, "diners": [{"tacos": counts(3), "name": "Ana"}], "note": ""}));
        assert_eq!(cali_body(&order(json!([{"name": "Ana"}]))).unwrap()["diners"][0]["tacos"], counts(0));
        assert_eq!(cali_body(&order(json!([{"tacos": counts(1)}]))).unwrap_err(), "every diner needs a handle or a name");
        assert!(cali_body(&order(json!([]))).unwrap_err().contains("1 to 20 diners"));
        assert_eq!(cali_body(&order(json!([{"name": "Ana"}, {"name": "ana"}]))).unwrap_err(), "the same diner is listed twice");
        assert_eq!(cali_body(&order(json!([{"handle": "a b"}]))).unwrap_err(), "invalid handle");
        for bad in [json!(true), json!("3"), json!(51), json!(-1)] {
            assert!(cali_body(&order(json!([{"name": "Ana", "tacos": {"mildHard": bad}}]))).unwrap_err().contains("whole number"));
        }
        let mut o = order(json!([{"name": "Ana"}]));
        o["date"] = json!("next tuesday");
        assert_eq!(cali_body(&o).unwrap_err(), "date must look like YYYY-MM-DD");
        o["date"] = json!("2026-09-29");
        o["note"] = json!("bogo\tnight");
        let c = cali_body(&o).unwrap();
        assert_eq!((c["date"].as_str(), c["note"].as_str()), (Some("2026-09-29"), Some("bogonight")));
        let c = cali_body(&order(json!([{"name": "Ana", "items": {"soda": 1, "burrito": 0, "nachos": 2}}]))).unwrap();
        assert_eq!(c["diners"][0]["items"].as_object().unwrap().keys().collect::<Vec<_>>(), vec!["nachos", "soda"]);
        assert!(cali_body(&order(json!([{"name": "Ana", "items": {"burrito": 0}}]))).unwrap()["diners"][0].get("items").is_none());
        assert_eq!(cali_body(&order(json!([{"name": "Ana", "items": {"pizza": 1}}]))).unwrap_err(), "unknown menu item");
        assert!(cali_body(&order(json!([{"name": "Ana", "items": ["burrito"]}]))).unwrap_err().contains("object"));
        let c = cali_body(&json!({"requestId": RID, "diners": [{"name": "Ana", "sessionId": SID}], "sessionId": SID, "cwd": "/x", "totalTacos": 9})).unwrap();
        assert!(leaks(&c).is_empty());
        assert!(c.get("totalTacos").is_none());
    }

    #[test]
    fn room_bodies() {
        let e = |p: &str, b: Value| room_body_error(p, b.as_object().unwrap());
        let r = "r_ABCDEFGHIJKLMNOPQRSTUv";
        assert!(e("/api/arena/rooms/create", json!({})).unwrap().contains("name"));
        assert!(e("/api/arena/rooms/create", json!({"name": "hi"})).unwrap().contains("password"));
        assert!(e("/api/arena/rooms/create", json!({"name": "a".repeat(201), "password": "x"})).unwrap().contains("name"));
        assert!(e("/api/arena/rooms/create", json!({"name": "   ", "password": "x"})).unwrap().contains("name"));
        assert_eq!(e("/api/arena/rooms/create", json!({"name": "My Room", "password": "secret123"})), None);
        assert!(e("/api/arena/rooms/join", json!({"roomId": r})).unwrap().contains("password"));
        assert!(e("/api/arena/rooms/leave", json!({"roomId": "bad"})).unwrap().contains("roomId"));
        assert_eq!(e("/api/arena/rooms/leave", json!({"roomId": r})), None);
        assert!(e("/api/arena/rooms/kick", json!({"roomId": r, "userId": "not-uuid"})).unwrap().contains("userId"));
        assert!(e("/api/arena/rooms/password", json!({"roomId": r, "password": "x", "signOutOthers": "yes"})).unwrap().contains("signOutOthers"));
        assert_eq!(e("/api/arena/rooms/password", json!({"roomId": r, "password": "x", "signOutOthers": true})), None);
    }

    fn eat_setup(state: &'static str) -> (tempfile::TempDir, Arc<App>, crate::arena::tests::Sent, Arc<Mutex<(u16, Value)>>) {
        let (d, app) = app();
        let reply = Arc::new(Mutex::new((200, json!({}))));
        let sent = stub(&app, "https://arena.example", reply.clone());
        // a live session in the memo with the given fatigue state
        {
            let mut g = app.payload_memo.lock().unwrap();
            g.data = Some(Arc::new(json!({"sessions": [{"sessionId": SID, "kind": "interactive", "rawStatus": "idle",
                                                        "creature": {"species": 3, "fatigue": {"state": state}}}]})));
            g.at = Some(std::time::Instant::now() + std::time::Duration::from_secs(3600));
        }
        (d, app, sent, reply)
    }

    fn server_eat(kind: &str, at: f64) -> (u16, Value) {
        (200, json!({"op": "eat", "replayed": false, "kind": kind, "at": py::iso_from_ts(at), "coins": 4,
                     "catalog": [{"kind": "berry", "price": 1}, {"kind": "tonic", "price": 5}]}))
    }

    #[test]
    fn eat_gate_and_ledger() {
        let (_d, app, sent, reply) = eat_setup("tired");
        assert_eq!(app.pantry_eat(&json!({"sessionId": "nope", "kind": "berry", "requestId": RID})).0, 400);
        assert_eq!(app.pantry_eat(&json!({"sessionId": OTHER, "kind": "berry", "requestId": RID})), (404, json!({"error": "unknown session"})));
        assert_eq!(app.pantry_eat(&json!({"sessionId": SID, "kind": "tonic", "requestId": RID})).1["code"], "not_fainted");
        assert!(sent.lock().unwrap().is_empty());
        let at = (py::now_ts() as i64 - 42) as f64;
        *reply.lock().unwrap() = server_eat("berry", at);
        let (code, resp) = app.pantry_eat(&json!({"sessionId": SID, "kind": "berry", "requestId": RID, "title": "secret"}));
        assert_eq!(code, 200, "{}", resp);
        assert_eq!(resp["meal"], json!({"kind": "berry", "at": py::iso_from_ts(at), "restoreMins": 20, "revives": false}));
        assert_eq!(resp["catalog"][1]["wakeToMins"], 105);
        assert_eq!(app.load_meals(None)[SID], vec![(at, "berry".to_string())]);
        assert_eq!(sent.lock().unwrap()[0].3, Some(json!({"requestId": RID, "kind": "berry"})));
        // a replay is local; a reused id for another meal is refused
        let (code, resp) = app.pantry_eat(&json!({"sessionId": SID, "kind": "berry", "requestId": RID}));
        assert_eq!((code, resp["replayed"].clone()), (200, json!(true)));
        assert_eq!(app.pantry_eat(&json!({"sessionId": SID, "kind": "bento", "requestId": RID})).1["code"], "rid_reused");
        assert_eq!(sent.lock().unwrap().len(), 1);
    }

    #[test]
    fn eat_pending_and_passthrough() {
        let (_d, app, _sent, reply) = eat_setup("fatigued");
        for code in [0u16, 500, 503] {
            *reply.lock().unwrap() = (code, json!({"error": "boom"}));
            let (status, resp) = app.pantry_eat(&json!({"sessionId": SID, "kind": "berry", "requestId": RID}));
            assert_eq!(status, 202);
            assert!(resp["error"].as_str().unwrap().contains("retry"));
        }
        *reply.lock().unwrap() = (409, json!({"detail": "no Berries", "error": "no Berries"}));
        assert_eq!(app.pantry_eat(&json!({"sessionId": SID, "kind": "berry", "requestId": RID})).0, 409);
        *reply.lock().unwrap() = server_eat("bento", py::now_ts());
        assert_eq!(app.pantry_eat(&json!({"sessionId": SID, "kind": "berry", "requestId": RID})).0, 502);
        assert!(app.load_meals(None).is_empty());
        app.meals_lock.lock().unwrap().insert(RID.to_string());
        assert_eq!(app.pantry_eat(&json!({"sessionId": SID, "kind": "berry", "requestId": RID})), (202, json!({"pending": true, "requestId": RID})));
    }

    #[test]
    fn eat_gate_states() {
        for (state, kind, want) in [("rested", "berry", "not_hungry"), ("unconscious", "berry", "fainted"), ("unconscious", "hotpot", "fainted"),
                                    ("tired", "tonic", "not_fainted"), ("fatigued", "elixir", "not_fainted"), ("rested", "elixir", "not_hungry")] {
            let (_d, app, sent, _r) = eat_setup(state);
            let (code, resp) = app.pantry_eat(&json!({"sessionId": SID, "kind": kind, "requestId": RID}));
            assert_eq!((code, resp["code"].as_str().unwrap()), (409, want));
            assert!(sent.lock().unwrap().is_empty());
        }
    }

    #[test]
    fn sounds_local_upload() {
        let (_d, app) = app();
        let data = base64::engine::general_purpose::STANDARD.encode(b"RIFFdata");
        let body = |n: &str, d: &str| json!({"name": n, "data": d}).as_object().unwrap().clone();
        assert_eq!(app.receive_sound_upload(&body("ding.wav", &data)).0, 201);
        assert_eq!(app.receive_sound_upload(&body("ding.wav", &data)).0, 409);
        assert_eq!(app.receive_sound_upload(&body("x.exe", &data)).1["error"], "must be an audio file (.m4a, .mp3, .ogg, .wav, .webm)");
        assert_eq!(app.receive_sound_upload(&body("a.mp3", "!!!")).1["error"], "invalid file encoding");
        assert_eq!(app.local_sounds(), vec![json!({"name": "ding", "file": "ding.wav", "size": 8})]);
        assert!(app.local_sound_file("ding.wav").is_some());
        assert_eq!(splitext(".hidden"), (".hidden".to_string(), String::new()));
        assert_eq!(splitext("a.b.mp3"), ("a.b".to_string(), ".mp3".to_string()));
    }
}
