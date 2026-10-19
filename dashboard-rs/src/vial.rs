//! The Overflow Vial (dashboard.py 2.5.2): past Apex a creature's session keeps
//! earning XP no one spends, so half of that surplus drips into a shared bank
//! with a 24 h fuse. The ledger is per-deposit (each drop expires on its own
//! clock) and a spend CONSUMES deposits oldest-first, rewriting the ledger, so
//! an expired deposit can never drag a spend onto fresh XP.
//!
//! vial.json: {"version": 1, "deposits": [{at, xp}], "spent": [{at, xp, sid, stage}], "seenPrompts": n}.
//! "spent" is a pure audit log; it never feeds the balance. Never leaves the machine.

use crate::app::{is_uuid, App};
use crate::creature::STAGE_THRESHOLDS;
use crate::py;
use crate::pyjson;
use crate::state;
use serde_json::{json, Map, Value};

/// A deposit dies one day after it lands.
pub const VIAL_WINDOW: f64 = 86400.0;
/// Hard ceiling so a forgotten bank can't grow forever.
pub const VIAL_MAX: i64 = 10000;
/// Cost to lift a creature one stage from stage i (the "half-efficiency tax").
pub const VIAL_STAGE_COST: [i64; 4] = [30, 90, 180, 400];

#[derive(Clone, Debug, PartialEq)]
pub struct Deposit {
    pub at: f64,
    pub xp: i64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Spent {
    pub at: f64,
    pub xp: i64,
    pub sid: Value,
    pub stage: i64,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct VialState {
    pub deposits: Vec<Deposit>,
    pub spent: Vec<Spent>,
    pub seen_prompts: i64,
}

/// isinstance(v, (int, float)) -- bools included, as in Python.
fn num(v: Option<&Value>) -> Option<f64> {
    match v? {
        Value::Bool(b) => Some(*b as i64 as f64),
        Value::Number(n) => n.as_f64(),
        _ => None,
    }
}

/// int(v) of a number that passed num().
fn int_of(v: &Value) -> i64 {
    match v {
        Value::Bool(b) => *b as i64,
        Value::Number(n) => n.as_i64().unwrap_or_else(|| n.as_f64().unwrap_or(0.0).trunc() as i64),
        _ => 0,
    }
}

/// Python's len() for the shapes a hand-edited ledger may hold.
fn py_len(v: Option<&Value>) -> usize {
    match v {
        None => 0,
        Some(Value::Array(a)) => a.len(),
        Some(Value::Object(o)) => o.len(),
        Some(Value::String(s)) => s.chars().count(),
        Some(_) => 0,
    }
}

fn live(v: &Map<String, Value>, now: f64) -> Option<(f64, i64)> {
    let xp = num(v.get("xp"))?;
    let at = num(v.get("at"))?;
    let xp_v = v.get("xp").unwrap();
    (0.0 < xp && xp <= VIAL_MAX as f64 && now - VIAL_WINDOW < at && at <= now + 60.0).then(|| (at, int_of(xp_v)))
}

/// _vial_read: only live entries; a missing or corrupt file is an empty vial.
pub fn parse_state(raw: Option<&Value>, now: f64) -> VialState {
    let mut out = VialState::default();
    let Some(raw) = raw else { return out };
    let Value::Object(o) = raw else { return out };
    if let Some(sn) = o.get("seenPrompts") {
        if let Some(n) = num(Some(sn)) {
            if n >= 0.0 {
                out.seen_prompts = int_of(sn);
            }
        }
    }
    if let Some(Value::Array(dep)) = o.get("deposits") {
        for d in dep {
            if let Some((at, xp)) = d.as_object().and_then(|d| live(d, now)) {
                out.deposits.push(Deposit { at, xp });
            }
        }
    }
    if let Some(Value::Array(sp)) = o.get("spent") {
        for s in sp {
            let Some(m) = s.as_object() else { continue };
            if let Some((at, xp)) = live(m, now) {
                let sid = m.get("sid").filter(|v| py::truthy(v)).cloned().unwrap_or_else(|| json!(""));
                let stage = m.get("stage").filter(|v| py::truthy(v)).and_then(py::py_int).unwrap_or(0);
                out.spent.push(Spent { at, xp, sid, stage });
            }
        }
    }
    out.deposits.sort_by(|a, b| a.at.total_cmp(&b.at));
    out.spent.sort_by(|a, b| a.at.total_cmp(&b.at));
    out
}

impl VialState {
    /// vial_balance: the sum of the live deposits.
    pub fn balance(&self) -> i64 {
        self.deposits.iter().map(|d| d.xp).sum()
    }

    /// vial_sources: live deposits as {xp, ttl}, last expiry first.
    pub fn sources(&self, now: f64) -> Value {
        let mut rows: Vec<(f64, i64)> = self.deposits.iter().map(|d| ((d.at + VIAL_WINDOW - now).max(0.0), d.xp)).collect();
        rows.sort_by(|a, b| b.0.total_cmp(&a.0)); // stable, like sorted(key=-ttl)
        Value::Array(rows.into_iter().map(|(ttl, xp)| json!({"xp": xp, "ttl": ttl})).collect())
    }

    /// _vial_cap: trim the deposits to VIAL_MAX total XP, oldest-first.
    pub fn cap(&mut self) {
        let mut total: i64 = self.balance();
        while total > VIAL_MAX && !self.deposits.is_empty() {
            let old = self.deposits[0].clone();
            if old.xp <= total - VIAL_MAX {
                total -= old.xp;
                self.deposits.remove(0);
            } else {
                self.deposits[0] = Deposit { at: old.at, xp: old.xp - (total - VIAL_MAX) };
                total = VIAL_MAX;
            }
        }
    }

    /// The file body: {"version": 1, deposits, spent, seenPrompts}.
    pub fn to_json(&self) -> Value {
        json!({
            "version": 1,
            "deposits": self.deposits.iter().map(|d| json!({"at": d.at, "xp": d.xp})).collect::<Vec<_>>(),
            "spent": self.spent.iter().map(|s| json!({"at": s.at, "xp": s.xp, "sid": s.sid, "stage": s.stage})).collect::<Vec<_>>(),
            "seenPrompts": self.seen_prompts,
        })
    }
}

impl App {
    fn vial_raw(&self) -> Option<Value> {
        let b = std::fs::read(&self.paths.vial).ok()?;
        pyjson::loads_lenient(std::str::from_utf8(&b).ok()?)
    }

    pub fn vial_read(&self, now: f64) -> VialState {
        parse_state(self.vial_raw().as_ref(), now)
    }

    fn vial_write(&self, st: &VialState) -> std::io::Result<()> {
        state::atomic_write_json(&self.paths.vial, &st.to_json(), 0o600)
    }

    /// vial_tick: expire old deposits and spends on disk (a no-op if nothing
    /// aged out) and return the live balance.
    pub fn vial_tick(&self, now: f64) -> i64 {
        let _g = self.vial_lock.lock().unwrap();
        let st = self.vial_read(now);
        let raw = self.vial_raw();
        let live_n = st.deposits.len() + st.spent.len();
        let (is_dict, raw_n) = match &raw {
            Some(Value::Object(o)) => (true, py_len(o.get("deposits")) + py_len(o.get("spent"))),
            _ => (false, live_n),
        };
        if raw_n != live_n || !is_dict {
            let _ = self.vial_write(&st);
        }
        st.balance()
    }

    /// vial_grant: deposit XP.
    pub fn vial_grant(&self, xp: i64, at: f64) {
        if xp <= 0 {
            return;
        }
        let _g = self.vial_lock.lock().unwrap();
        let mut st = self.vial_read(at);
        st.deposits.push(Deposit { at, xp });
        st.cap();
        let _ = self.vial_write(&st);
    }

    /// vial_drip: turn prompts past Apex into half-XP deposits, once (the
    /// seenPrompts watermark). Returns the XP just deposited.
    pub fn vial_drip(&self, surplus_prompts: i64, at: f64) -> i64 {
        if surplus_prompts <= 0 {
            return 0;
        }
        let _g = self.vial_lock.lock().unwrap();
        let mut st = self.vial_read(at);
        let seen = st.seen_prompts;
        if surplus_prompts <= seen {
            return 0;
        }
        let delta = ((surplus_prompts - seen) * 10).div_euclid(2);
        if delta != 0 {
            st.deposits.push(Deposit { at, xp: delta });
            st.cap();
        }
        st.seen_prompts = surplus_prompts;
        if self.vial_write(&st).is_err() {
            return 0;
        }
        delta
    }

    /// vial_spend: lift a creature one stage. {"ok", "cost", "balance"} or
    /// {"ok": false, "error", ...}.
    pub fn vial_spend(&self, session_id: &Value, from_stage: &Value, at: f64) -> Value {
        if !matches!(session_id, Value::String(s) if is_uuid(s)) {
            return json!({"ok": false, "error": "bad session id"});
        }
        let stage = match py::strict_int(from_stage) {
            Some(s) if (0..=3).contains(&s) => s,
            _ => return json!({"ok": false, "error": "already Apex"}),
        };
        let cost = VIAL_STAGE_COST[stage as usize];
        let _g = self.vial_lock.lock().unwrap();
        let mut st = self.vial_read(at);
        if st.balance() < cost {
            return json!({"ok": false, "error": "the vial is short", "cost": cost, "balance": st.balance()});
        }
        // consume oldest-first (the same fuse order the deposits expire in)
        let mut need = cost;
        while need > 0 && !st.deposits.is_empty() {
            let old = st.deposits[0].clone();
            if old.xp <= need {
                need -= old.xp;
                st.deposits.remove(0);
            } else {
                st.deposits[0] = Deposit { at: old.at, xp: old.xp - need };
                need = 0;
            }
        }
        st.spent.push(Spent { at, xp: cost, sid: session_id.clone(), stage: stage + 1 });
        if self.vial_write(&st).is_err() {
            return json!({"ok": false, "error": "the vial ledger is unwritable"});
        }
        self.invalidate_payload_memo();
        json!({"ok": true, "cost": cost, "balance": st.balance()})
    }

    /// The vial part of build_payload: drip the fresh Apex surplus, tick, set
    /// each card's `vialPoured`, and return payload["vial"].
    pub fn payload_vial(&self, sessions: &mut [Value]) -> Value {
        let now = py::now_ts();
        let apex = STAGE_THRESHOLDS[STAGE_THRESHOLDS.len() - 1];
        let pc = |s: &Value| s.get("promptCount").filter(|v| py::truthy(v)).and_then(py::py_int).unwrap_or(0);
        let surplus: i64 = sessions.iter().map(|s| (pc(s) - apex).max(0)).sum();
        self.vial_drip(surplus, now);
        let xp = self.vial_tick(now);
        for s in sessions.iter_mut() {
            let poured = (pc(s) - apex).max(0) * 5;
            if let Value::Object(o) = s {
                o.insert("vialPoured".into(), json!(poured));
            }
        }
        let st = {
            let _g = self.vial_lock.lock().unwrap();
            self.vial_read(now)
        };
        let costs: Map<String, Value> = VIAL_STAGE_COST.iter().enumerate().map(|(i, c)| (i.to_string(), json!(c))).collect();
        json!({"xp": xp, "expiresIn": VIAL_WINDOW, "costs": costs, "src": st.sources(now)})
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::testutil::app;
    use std::os::unix::fs::PermissionsExt;

    const SID: &str = "11111111-1111-4111-8111-111111111111";
    const NOW: f64 = 1790000000.0;

    fn raw(a: &App) -> Value {
        serde_json::from_slice(&std::fs::read(&a.paths.vial).unwrap()).unwrap()
    }

    #[test]
    fn missing_file_is_an_empty_vial() {
        let (_d, a) = app();
        assert_eq!(a.vial_tick(NOW), 0);
        assert_eq!(a.vial_read(NOW), VialState::default());
        assert!(a.paths.vial.exists()); // the tick wrote the empty ledger, as Python does
    }

    #[test]
    fn grant_deposits_and_balance_is_the_live_sum() {
        let (_d, a) = app();
        a.vial_grant(50, NOW - 10.0);
        a.vial_grant(30, NOW - 5.0);
        assert_eq!(a.vial_tick(NOW), 80);
    }

    #[test]
    fn a_deposit_dies_exactly_one_window_after_it_lands() {
        let (_d, a) = app();
        a.vial_grant(100, NOW - VIAL_WINDOW + 1.0);
        assert_eq!(a.vial_tick(NOW), 100);
        assert_eq!(a.vial_tick(NOW + 1.0), 0);
    }

    #[test]
    fn spend_consumes_oldest_first_and_leaves_the_split() {
        let (_d, a) = app();
        a.vial_grant(50, NOW - 20.0);
        a.vial_grant(50, NOW - 5.0);
        let r = a.vial_spend(&json!(SID), &json!(1), NOW);
        assert_eq!(r, json!({"ok": true, "cost": 90, "balance": 10}));
        let st = a.vial_read(NOW);
        assert_eq!(st.deposits, vec![Deposit { at: NOW - 5.0, xp: 10 }]);
        assert_eq!((st.spent[0].sid.clone(), st.spent[0].stage), (json!(SID), 2));
    }

    #[test]
    fn an_expired_deposit_never_drags_a_spend_onto_fresh_xp() {
        let (_d, a) = app();
        a.vial_grant(40, NOW - VIAL_WINDOW + 2.0);
        a.vial_grant(100, NOW - 1.0);
        let r = a.vial_spend(&json!(SID), &json!(0), NOW);
        assert_eq!((r["ok"].clone(), r["balance"].clone()), (json!(true), json!(110)));
        assert_eq!(a.vial_tick(NOW + 3.0), 100);
    }

    #[test]
    fn spend_refused_when_the_vial_is_short() {
        let (_d, a) = app();
        a.vial_grant(20, NOW);
        assert_eq!(a.vial_spend(&json!(SID), &json!(0), NOW), json!({"ok": false, "error": "the vial is short", "cost": 30, "balance": 20}));
        assert!(a.vial_read(NOW).spent.is_empty());
    }

    #[test]
    fn apex_and_bad_ids_are_refused() {
        let (_d, a) = app();
        a.vial_grant(5000, NOW);
        assert_eq!(a.vial_spend(&json!(SID), &json!(4), NOW), json!({"ok": false, "error": "already Apex"}));
        assert_eq!(a.vial_spend(&json!("../etc/passwd"), &json!(1), NOW), json!({"ok": false, "error": "bad session id"}));
        assert_eq!(a.vial_spend(&json!(SID), &json!(true), NOW)["ok"], false);
        assert_eq!(a.vial_spend(&json!(SID), &json!(1.0), NOW)["ok"], false);
        assert_eq!(a.vial_tick(NOW), 5000);
    }

    #[test]
    fn ledger_ceiling_trims_the_oldest_deposits() {
        let (_d, a) = app();
        a.vial_grant(VIAL_MAX - 10, NOW - 50.0);
        a.vial_grant(20, NOW);
        assert_eq!(a.vial_tick(NOW), VIAL_MAX);
        let st = a.vial_read(NOW);
        assert_eq!(st.deposits, vec![Deposit { at: NOW - 50.0, xp: VIAL_MAX - 20 }, Deposit { at: NOW, xp: 20 }]);
    }

    #[test]
    fn file_is_valid_private_json_with_no_temp_left() {
        let (d, a) = app();
        a.vial_grant(10, NOW);
        assert_eq!(raw(&a), json!({"version": 1, "deposits": [{"at": NOW, "xp": 10}], "spent": [], "seenPrompts": 0}));
        let text = std::fs::read_to_string(&a.paths.vial).unwrap();
        assert!(text.contains("\"at\": 1790000000.0"), "{}", text); // float repr, like Python
        assert_eq!(std::fs::metadata(&a.paths.vial).unwrap().permissions().mode() & 0o777, 0o600);
        let temps = std::fs::read_dir(d.path()).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().ends_with(".tmp")).count();
        assert_eq!(temps, 0);
    }

    #[test]
    fn corrupt_file_reads_empty_and_bad_entries_are_dropped() {
        let (_d, a) = app();
        std::fs::write(&a.paths.vial, "{not json").unwrap();
        assert_eq!(a.vial_tick(NOW), 0);
        // what Python's json.dumps writes for float("inf"): a bare Infinity literal
        let body = format!(
            r#"{{"version": 1, "deposits": [{{"at": {n:?}, "xp": 10}}, {{"at": {n:?}, "xp": -5}}, {{"at": {n:?}, "xp": Infinity}}, {{"at": {old:?}, "xp": 10}}, "not a dict"], "spent": [{{"at": {n:?}, "xp": 3, "sid": "{SID}", "stage": 1}}, {{"at": true, "xp": 3}}], "seenPrompts": -4}}"#,
            n = NOW,
            old = NOW - VIAL_WINDOW - 1.0
        );
        std::fs::write(&a.paths.vial, body).unwrap();
        let st = a.vial_read(NOW);
        assert_eq!(st.deposits, vec![Deposit { at: NOW, xp: 10 }]);
        assert_eq!(st.spent.len(), 1);
        assert_eq!(st.seen_prompts, 0);
    }

    #[test]
    fn surplus_drips_in_once_and_only_the_fresh_surplus_lands() {
        let (_d, a) = app();
        assert_eq!(a.vial_drip(5, NOW), 25);
        assert_eq!(a.vial_tick(NOW + 1.0), 25);
        assert_eq!(a.vial_drip(5, NOW + 2.0), 0);
        assert_eq!(a.vial_drip(15, NOW + 3.0), 50);
        assert_eq!(a.vial_tick(NOW + 4.0), 75);
        let st = a.vial_read(NOW + 5.0);
        assert_eq!(st.seen_prompts, 15);
        assert_eq!(st.deposits[0], Deposit { at: NOW, xp: 25 });
    }

    #[test]
    fn no_surplus_no_deposit() {
        let (_d, a) = app();
        assert_eq!(a.vial_drip(0, NOW), 0);
        assert_eq!(a.vial_drip(-3, NOW), 0);
        assert_eq!(a.vial_tick(NOW + 1.0), 0);
        assert_eq!(a.vial_read(NOW).seen_prompts, 0);
    }

    #[test]
    fn spend_debits_the_ledger_and_invalidates_the_payload() {
        let (_d, a) = app();
        a.vial_grant(200, NOW);
        a.invalidate_payload_memo();
        let gen0 = a.payload_memo.lock().unwrap().gen;
        assert_eq!(a.vial_spend(&json!(SID), &json!(0), NOW), json!({"ok": true, "cost": 30, "balance": 170}));
        assert!(a.payload_memo.lock().unwrap().gen > gen0);
        assert_eq!(VIAL_STAGE_COST, [30, 90, 180, 400]);
    }

    #[test]
    fn sources_list_the_live_drops_last_expiry_first() {
        let (_d, a) = app();
        a.vial_grant(50, NOW - 82800.0);
        a.vial_grant(400, NOW - 7200.0);
        a.vial_grant(200, NOW - 3600.0);
        a.vial_drip(6, NOW);
        let src = a.vial_read(NOW).sources(NOW);
        assert_eq!(
            src,
            json!([{"xp": 30, "ttl": 86400.0}, {"xp": 200, "ttl": 82800.0}, {"xp": 400, "ttl": 79200.0}, {"xp": 50, "ttl": 3600.0}])
        );
    }

    #[test]
    fn a_partial_spend_keeps_the_die_time_of_the_leftover() {
        let (_d, a) = app();
        a.vial_grant(100, NOW - 3600.0);
        a.vial_spend(&json!(SID), &json!(0), NOW);
        assert_eq!(a.vial_read(NOW).sources(NOW), json!([{"xp": 70, "ttl": 82800.0}]));
    }

    #[test]
    fn payload_pours_apex_surplus() {
        let (_d, a) = app();
        let mut sessions = vec![json!({"sessionId": "a", "promptCount": 75}), json!({"sessionId": "b", "promptCount": 3}), json!({"sessionId": "c"})];
        let v = a.payload_vial(&mut sessions);
        assert_eq!(sessions.iter().map(|s| s["vialPoured"].clone()).collect::<Vec<_>>(), vec![json!(25), json!(0), json!(0)]);
        assert_eq!(v["xp"], 25); // 5 prompts past Apex -> (5 * 10) // 2
        assert_eq!(v["costs"], json!({"0": 30, "1": 90, "2": 180, "3": 400}));
        assert_eq!(v["expiresIn"], json!(86400.0));
        let keys: Vec<&String> = v.as_object().unwrap().keys().collect();
        assert_eq!(keys, ["xp", "expiresIn", "costs", "src"]);
        assert_eq!(a.payload_vial(&mut sessions)["xp"], 25); // the watermark: no double drip
    }
}
