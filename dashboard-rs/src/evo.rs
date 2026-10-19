//! Evolution picks (which branch / Mega form a session's Pokemon becomes):
//! dashboard.py's evo_tables / clean_evo_choice / merge_evo_choices /
//! load_evo_choices / save_evo_choice.
//!
//! The page POSTs each pick to /api/evo-choice; they live in evo-choices.json
//! (0600, git-ignored, never leaves the machine) and ride in /api/sessions as
//! `creature.evo`. The allowed values are read from ui/app/06-trainer-packs.js
//! itself (POKE_EVO / POKE_BRANCH / MEGA_FORMS), so the server and the page
//! cannot disagree on what is valid.

use crate::app::{is_uuid, App};
use crate::creature::session_hash_mod;
use crate::py::Obj;
use crate::state;
use regex::Regex;
use serde_json::{Map, Value};
use std::collections::{BTreeSet, HashMap};
use std::io;
use std::sync::LazyLock;

/// Sessions kept (the oldest picks drop beyond this).
pub const EVO_MAX: usize = 2000;

/// ({species: final dexes it may become}, {final dex: mega slugs}).
#[derive(Debug, Default, Clone)]
pub struct EvoTables {
    pub finals: HashMap<i64, BTreeSet<i64>>,
    pub megas: HashMap<i64, BTreeSet<String>>,
}

static BRANCH_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(\d+):\[(\{.*?\})\]").unwrap());
static DEX_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"dex:(\d+)").unwrap());
static MEGA_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(\d+):\[(\{name:.*?\})\]").unwrap());
static SLUG_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"slug:"([a-z0-9-]+)""#).unwrap());

/// Parse the tables out of the page script. Like Python, a parse error stops
/// the parse but keeps what was read before it.
pub fn parse_tables(js: &str) -> EvoTables {
    let mut t = EvoTables::default();
    let var = |name: &str| -> String {
        let re = Regex::new(&format!(r"(?s)var {}=(.*?);\n", regex::escape(name))).unwrap();
        re.captures(js).map(|c| c[1].to_string()).unwrap_or_default()
    };
    let _ = (|| -> Result<(), ()> {
        let evo = var("POKE_EVO");
        let lines: Value = serde_json::from_str(if evo.is_empty() { "[]" } else { &evo }).map_err(|_| ())?;
        if let Value::Array(lines) = lines {
            for (i, line) in lines.iter().enumerate() {
                if let Some(last) = line.as_array().and_then(|l| l.last()) {
                    let d = last.as_i64().or_else(|| last.as_f64().map(|f| f.trunc() as i64)).ok_or(())?;
                    t.finals.insert(i as i64, BTreeSet::from([d]));
                }
            }
        }
        for c in BRANCH_RE.captures_iter(&var("POKE_BRANCH")) {
            let idx: i64 = c[1].parse().map_err(|_| ())?;
            let set = t.finals.entry(idx).or_default();
            for d in DEX_RE.captures_iter(&c[2]) {
                set.insert(d[1].parse().map_err(|_| ())?);
            }
        }
        for c in MEGA_RE.captures_iter(&var("MEGA_FORMS")) {
            let key: i64 = c[1].parse().map_err(|_| ())?;
            t.megas.insert(key, SLUG_RE.captures_iter(&c[2]).map(|s| s[1].to_string()).collect());
        }
        Ok(())
    })();
    t
}

fn is_int(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) if n.is_i64() || n.is_u64() => n.as_i64(),
        _ => None,
    }
}

impl App {
    /// evo_tables(): parsed once from ui/app/06-trainer-packs.js.
    pub fn evo_tables(&self) -> &EvoTables {
        self.evo_tables.get_or_init(|| {
            let js = std::fs::read(self.paths.ui_dir.join("app").join("06-trainer-packs.js")).ok().and_then(|b| String::from_utf8(b).ok());
            js.map(|s| parse_tables(&s)).unwrap_or_default()
        })
    }

    /// clean_evo_choice: this session's pick, keeping only what is valid for
    /// its species: {branchDex?, megaSlug?}; {} when nothing in it is.
    pub fn clean_evo_choice(&self, sid: &str, entry: &Value) -> Obj {
        let mut out = Map::new();
        let Value::Object(entry) = entry else { return out };
        if !is_uuid(sid) {
            return out;
        }
        let t = self.evo_tables();
        let empty = BTreeSet::new();
        let allowed = t.finals.get(&(session_hash_mod(sid, 48) as i64)).unwrap_or(&empty);
        if let Some(b) = entry.get("branchDex").and_then(is_int) {
            if allowed.contains(&b) && allowed.len() > 1 {
                out.insert("branchDex".into(), Value::from(b));
            }
        }
        // a Mega must belong to the chosen final, or (no branch picked) to any final it may become
        let finals_now: Vec<i64> = match out.get("branchDex").and_then(|v| v.as_i64()) {
            Some(b) => vec![b],
            None => allowed.iter().copied().collect(),
        };
        let pool: BTreeSet<&String> = finals_now.iter().filter_map(|d| t.megas.get(d)).flatten().collect();
        if let Some(Value::String(m)) = entry.get("megaSlug") {
            if pool.contains(m) {
                out.insert("megaSlug".into(), Value::from(m.clone()));
            }
        }
        out
    }

    /// load_evo_choices: evo-choices.json -> {sessionId: {branchDex?, megaSlug?}} (validated).
    pub fn load_evo_choices(&self) -> Obj {
        let mut out = Map::new();
        if let Some(Value::Object(raw)) = state::load_json_guarded(&self.paths.evo) {
            for (sid, entry) in raw {
                let e = self.clean_evo_choice(&sid, &entry);
                if !e.is_empty() {
                    out.insert(sid, Value::Object(e));
                }
            }
        }
        out
    }

    /// save_evo_choice: merge a pick into this session's entry (a null field
    /// clears it), persist, return the entry. Err(Bad) for a bad session id or
    /// a pick that is not valid for this session's Pokemon (nothing is written
    /// then); Err(Io) when the file cannot be written.
    pub fn save_evo_choice(&self, sid: Option<&Value>, patch: &Obj) -> Result<Obj, EvoError> {
        let sid = match sid {
            Some(Value::String(s)) if is_uuid(s) => s.as_str(),
            _ => return Err(EvoError::Bad("invalid sessionId".into())),
        };
        let _g = self.evo_lock.lock().unwrap();
        let mut data = self.load_evo_choices();
        let mut cur = match data.get(sid) {
            Some(Value::Object(o)) => o.clone(),
            _ => Map::new(),
        };
        for k in ["branchDex", "megaSlug"] {
            match patch.get(k) {
                None => {}
                Some(Value::Null) => {
                    cur.shift_remove(k);
                }
                Some(v) => {
                    cur.insert(k.into(), v.clone());
                }
            }
        }
        let entry = self.clean_evo_choice(sid, &Value::Object(cur));
        for k in ["branchDex", "megaSlug"] {
            if let Some(p) = patch.get(k).filter(|v| !v.is_null()) {
                if entry.get(k) != Some(p) {
                    return Err(EvoError::Bad(format!("{} is not a valid pick for this session's Pokemon", k)));
                }
            }
        }
        data.shift_remove(sid);
        if !entry.is_empty() {
            data.insert(sid.to_string(), Value::Object(entry.clone())); // newest last, so a trim keeps it
        }
        if data.len() > EVO_MAX {
            let drop = data.len() - EVO_MAX;
            data = data.into_iter().skip(drop).collect();
        }
        state::atomic_write_json(&self.paths.evo, &Value::Object(data), 0o600).map_err(EvoError::Io)?;
        Ok(entry)
    }
}

#[derive(Debug)]
pub enum EvoError {
    Bad(String),
    Io(io::Error),
}

/// merge_evo_choices: put each session's pick on its creature as
/// `creature.evo` (only where one exists, so every other card is unchanged).
pub fn merge_evo_choices(sessions: &mut [Value], evo: &Obj) {
    for s in sessions.iter_mut() {
        let Some(sid) = s.get("sessionId").and_then(|v| v.as_str()) else { continue };
        let Some(pick) = evo.get(sid).cloned() else { continue };
        if let Some(Value::Object(c)) = s.get_mut("creature") {
            c.insert("evo".into(), pick);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::testutil::app;
    use crate::app::Paths;
    use serde_json::json;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;
    use std::sync::Arc;

    /// uuid.UUID(int=n) as a string.
    fn uuid_of(n: u128) -> String {
        let h = format!("{:032x}", n);
        format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32])
    }

    /// A session id whose Pokemon is this species (species = sha256(sid) % 48),
    /// found the same way test_evo_choice.py's sid_for does.
    pub fn sid_for(species: u64) -> String {
        let mut i: u128 = 0;
        loop {
            let sid = uuid_of(i.wrapping_mul(2654435761));
            if session_hash_mod(&sid, 48) == species {
                return sid;
            }
            i += 1;
        }
    }

    /// An App over a temp dir that reads the real page tables.
    fn evo_app() -> (tempfile::TempDir, Arc<App>) {
        let (d, base) = app();
        let mut p: Paths = base.paths.clone();
        p.ui_dir = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().join("ui");
        (d, Arc::new(App::new(p)))
    }

    fn obj(v: Value) -> Obj {
        v.as_object().unwrap().clone()
    }

    #[test]
    fn tables_come_from_the_page() {
        let (_d, a) = evo_app();
        let t = a.evo_tables();
        assert_eq!(t.finals.len(), 48);
        assert_eq!(t.finals[&4], BTreeSet::from([134, 135, 136, 196, 197, 470, 471, 700])); // Eevee's eight
        assert_eq!(t.finals[&1], BTreeSet::from([6])); // Charizard's line
        assert_eq!(t.megas[&6], BTreeSet::from(["charizard-megax".to_string(), "charizard-megay".to_string()]));
    }

    #[test]
    fn sid_for_matches_python() {
        // test_evo_choice.py: str(uuid.UUID(int=i * 2654435761 % (1 << 128)))
        assert_eq!(uuid_of(2654435761), "00000000-0000-0000-0000-00009e3779b1");
    }

    #[test]
    fn clean() {
        let (_d, a) = evo_app();
        let (eevee, zard, pika) = (sid_for(4), sid_for(1), sid_for(0));
        let c = |sid: &str, v: Value| Value::Object(a.clean_evo_choice(sid, &v));
        assert_eq!(c(&eevee, json!({"branchDex": 197})), json!({"branchDex": 197}));
        assert_eq!(c(&eevee, json!({"branchDex": 25})), json!({})); // not one of Eevee's
        assert_eq!(c(&eevee, json!({"branchDex": true})), json!({})); // a bool is not a dex
        assert_eq!(c(&eevee, json!({"branchDex": 197.0})), json!({})); // nor a float
        assert_eq!(c(&zard, json!({"megaSlug": "charizard-megay"})), json!({"megaSlug": "charizard-megay"}));
        assert_eq!(c(&zard, json!({"megaSlug": "gengar-mega"})), json!({})); // not Charizard's
        assert_eq!(c(&zard, json!({"branchDex": 6})), json!({})); // no branch to pick
        assert_eq!(c(&pika, json!({"megaSlug": "charizard-megax"})), json!({}));
        assert_eq!(c("not-a-uuid", json!({"branchDex": 197})), json!({}));
        assert_eq!(c(&eevee, json!("x")), json!({}));
    }

    #[test]
    fn round_trip_and_clear() {
        let (_d, a) = evo_app();
        let eevee = sid_for(4);
        let sid = json!(eevee);
        let save = |p: Value| a.save_evo_choice(Some(&sid), &obj(p)).unwrap();
        assert_eq!(Value::Object(save(json!({"branchDex": 197}))), json!({"branchDex": 197}));
        assert_eq!(std::fs::metadata(&a.paths.evo).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(Value::Object(a.load_evo_choices()), json!({eevee.clone(): {"branchDex": 197}}));
        assert_eq!(Value::Object(save(json!({"branchDex": 700}))), json!({"branchDex": 700}));
        assert_eq!(Value::Object(save(json!({"branchDex": null}))), json!({}));
        assert_eq!(Value::Object(a.load_evo_choices()), json!({}));
        assert_eq!(std::fs::read_to_string(&a.paths.evo).unwrap(), "{}");
    }

    #[test]
    fn a_bad_pick_writes_nothing() {
        let (_d, a) = evo_app();
        let (eevee, zard) = (sid_for(4), sid_for(1));
        a.save_evo_choice(Some(&json!(zard)), &obj(json!({"megaSlug": "charizard-megax"}))).unwrap();
        let cases = [
            (json!(zard), json!({"megaSlug": "nope"}), "megaSlug is not a valid pick for this session's Pokemon"),
            (json!(eevee), json!({"branchDex": 1}), "branchDex is not a valid pick for this session's Pokemon"),
            (json!("x"), json!({"branchDex": 197}), "invalid sessionId"),
            (json!(5), json!({"branchDex": 197}), "invalid sessionId"),
        ];
        for (sid, patch, msg) in cases {
            match a.save_evo_choice(Some(&sid), &obj(patch)) {
                Err(EvoError::Bad(e)) => assert_eq!(e, msg),
                other => panic!("{:?}", other),
            }
        }
        assert_eq!(Value::Object(a.load_evo_choices()), json!({zard: {"megaSlug": "charizard-megax"}}));
    }

    #[test]
    fn a_tampered_file_is_cleaned_on_read() {
        let (_d, a) = evo_app();
        let (eevee, zard) = (sid_for(4), sid_for(1));
        std::fs::write(&a.paths.evo, json!({eevee: {"branchDex": 25}, zard.clone(): {"megaSlug": "charizard-megay", "x": 1}, "junk": {}}).to_string()).unwrap();
        assert_eq!(Value::Object(a.load_evo_choices()), json!({zard: {"megaSlug": "charizard-megay"}}));
    }

    #[test]
    fn newest_last_and_trimmed() {
        let (_d, a) = evo_app();
        let eevee = sid_for(4);
        let mut seed = Map::new();
        let mut n = 0u128;
        while seed.len() < EVO_MAX {
            let sid = uuid_of(n.wrapping_mul(2654435761));
            n += 1;
            if session_hash_mod(&sid, 48) == 4 && sid != eevee {
                seed.insert(sid, json!({"branchDex": 134}));
            }
        }
        let first = seed.keys().next().unwrap().clone();
        std::fs::write(&a.paths.evo, Value::Object(seed).to_string()).unwrap();
        a.save_evo_choice(Some(&json!(eevee)), &obj(json!({"branchDex": 471}))).unwrap();
        let data = a.load_evo_choices();
        assert_eq!(data.len(), EVO_MAX);
        assert!(!data.contains_key(&first)); // the oldest pick dropped
        assert_eq!(data.keys().next_back().unwrap(), &eevee);
    }

    #[test]
    fn write_failure_is_an_io_error() {
        let (d, base) = evo_app();
        let mut p = base.paths.clone();
        p.evo = d.path().join("missing-dir").join("evo-choices.json");
        let a = App::new(p);
        assert!(matches!(a.save_evo_choice(Some(&json!(sid_for(4))), &obj(json!({"branchDex": 197}))), Err(EvoError::Io(_))));
    }

    #[test]
    fn payload_carries_it_only_where_picked() {
        let (eevee, pika) = (sid_for(4), sid_for(0));
        let mut sessions = vec![
            json!({"sessionId": eevee, "creature": {"species": 4}}),
            json!({"sessionId": pika, "creature": {"species": 0}}),
            json!({"sessionId": eevee, "creature": "not a dict"}),
        ];
        merge_evo_choices(&mut sessions, &obj(json!({eevee: {"branchDex": 197}})));
        assert_eq!(sessions[0]["creature"], json!({"species": 4, "evo": {"branchDex": 197}}));
        assert!(sessions[1]["creature"].get("evo").is_none());
        assert_eq!(sessions[2]["creature"], "not a dict");
    }

    #[test]
    fn a_broken_page_script_gives_empty_tables() {
        let t = parse_tables("var POKE_EVO=[[1,2],oops];\nvar MEGA_FORMS={6:[{name:\"X\",slug:\"x-mega\"}]};\n");
        assert!(t.finals.is_empty() && t.megas.is_empty());
        let t = parse_tables("var POKE_EVO=[[1,2],[3]];\nvar POKE_BRANCH={\n  0:[{name:\"A\",dex:9},{name:\"B\",dex:10}]\n};\n");
        assert_eq!(t.finals[&0], BTreeSet::from([2, 9, 10]));
        assert_eq!(t.finals[&1], BTreeSet::from([3]));
    }
}
