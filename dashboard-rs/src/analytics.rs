//! Read-only views over every transcript: full-history search, history /
//! analytics, the Pokédex, per-project rollups, the daily digest, insights,
//! the paged transcript view and the Markdown / CSV exports.

use crate::app::{file_key, App};
use crate::cost;
use crate::creature;
use crate::payload::{epoch, pretty_folder, weekday};
use crate::py;
use crate::scan::{self, FileTouch};
use chrono::{Duration, NaiveDate};
use indexmap::{IndexMap, IndexSet};
use regex::Regex;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, LazyLock};

const SEARCH_MAX_BLOB: usize = 200_000;
static TERM_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[a-z0-9_]+").unwrap());

#[derive(Debug, Clone)]
pub struct SearchEntry {
    pub session_id: String,
    pub title: String,
    pub folder: String,
    pub last_activity: String,
    pub text: String,
    pub blob: String,
    pub cursor: bool,
}

fn build_claude_search_entry(path: &Path) -> Option<SearchEntry> {
    let recs = scan::open_records(path)?;
    let mut title: Option<String> = None;
    let mut last: Option<py::Dt> = None;
    let mut parts: Vec<String> = Vec::new();
    let mut total = 0usize;
    for (_line, rec) in recs {
        let o = rec.as_object().unwrap();
        let _ = (|| -> Option<()> {
            let ts = o.get("timestamp").and_then(|v| v.as_str()).and_then(py::parse_iso_utc);
            if let Some(ts) = ts {
                if last.map(|l| ts > l).unwrap_or(true) {
                    last = Some(ts);
                }
            }
            if total >= SEARCH_MAX_BLOB {
                return Some(());
            }
            let msg = match o.get("message") {
                Some(Value::Object(m)) => Some(m),
                Some(v) if py::truthy(v) => None,
                _ => None,
            };
            match o.get("type").and_then(|v| v.as_str()) {
                Some("ai-title") => {
                    if let Some(at) = o.get("aiTitle").filter(|v| py::truthy(v)) {
                        let at = at.as_str().map(String::from).unwrap_or_else(|| at.to_string());
                        total += py::char_len(&at);
                        title = Some(at.clone());
                        parts.push(at);
                    }
                }
                Some("user") => {
                    if matches!(o.get("message"), Some(v) if py::truthy(v) && !v.is_object()) {
                        return None;
                    }
                    let content = msg.and_then(|m| m.get("content"));
                    if scan::is_real_human_prompt(content)? {
                        let c = scan::clean_prompt(content)?;
                        total += py::char_len(&c);
                        parts.push(c);
                    }
                }
                Some("assistant") => {
                    if matches!(o.get("message"), Some(v) if py::truthy(v) && !v.is_object()) {
                        return None;
                    }
                    if let Some(Value::Array(blocks)) = msg.and_then(|m| m.get("content")) {
                        for b in blocks {
                            if let Value::Object(b) = b {
                                if b.get("type") == Some(&Value::from("text")) {
                                    let t = match b.get("text") {
                                        Some(Value::String(s)) => s.clone(),
                                        Some(v) if py::truthy(v) => return None,
                                        _ => String::new(),
                                    };
                                    if !t.is_empty() {
                                        let st = scan::strip_markdown(&t);
                                        total += py::char_len(&st);
                                        parts.push(st);
                                    }
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
            Some(())
        })();
    }
    let text = py::take_chars(&parts.join(" \n "), SEARCH_MAX_BLOB);
    Some(SearchEntry {
        session_id: scan::session_id_from_path(path),
        title: title.unwrap_or_else(|| "Untitled session".into()),
        folder: scan::parent_name(path),
        last_activity: last.map(|d| py::iso(&d)).unwrap_or_default(),
        blob: text.to_lowercase(),
        text,
        cursor: false,
    })
}

fn build_cursor_search_entry(path: &Path) -> Option<SearchEntry> {
    let recs = scan::open_records(path)?;
    let mut title: Option<String> = None;
    let mut last: Option<py::Dt> = None;
    let mut parts: Vec<String> = Vec::new();
    let mut total = 0usize;
    for (_line, rec) in recs {
        let o = rec.as_object().unwrap();
        match o.get("role").and_then(|v| v.as_str()) {
            Some("user") => {
                let t = scan::cursor_message_text(o);
                if let Some(ts) = scan::parse_cursor_clock(&t) {
                    if last.map(|l| ts > l).unwrap_or(true) {
                        last = Some(ts);
                    }
                }
                if let Some(q) = scan::cursor_user_query(&t) {
                    if total < SEARCH_MAX_BLOB {
                        if title.is_none() {
                            title = Some(py::truncate(&q, 80));
                        }
                        total += py::char_len(&q);
                        parts.push(q);
                    }
                }
            }
            Some("assistant") if total < SEARCH_MAX_BLOB => {
                let blocks = match o.get("message") {
                    Some(Value::Object(m)) => match m.get("content") {
                        Some(Value::String(s)) => vec![json!({"type": "text", "text": s})],
                        Some(Value::Array(a)) => a.clone(),
                        _ => Vec::new(),
                    },
                    _ => Vec::new(),
                };
                for b in &blocks {
                    if let Value::Object(b) = b {
                        if b.get("type") == Some(&Value::from("text")) {
                            if let Some(Value::String(t)) = b.get("text") {
                                if !t.is_empty() {
                                    let st = scan::strip_markdown(t);
                                    total += py::char_len(&st);
                                    parts.push(st);
                                }
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
    if last.is_none() {
        last = std::fs::metadata(path)
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .and_then(|d| py::from_ts(d.as_secs_f64()));
    }
    let text = py::take_chars(&parts.join(" \n "), SEARCH_MAX_BLOB);
    Some(SearchEntry {
        session_id: scan::session_id_from_path(path),
        title: title.unwrap_or_else(|| "Untitled session".into()),
        folder: scan::cursor_project_slug(path),
        last_activity: last.map(|d| py::iso(&d)).unwrap_or_default(),
        blob: text.to_lowercase(),
        text,
        cursor: true,
    })
}

/// The text around char index `idx`, like dashboard._snippet_around.
fn snippet_around(text: &str, idx: usize, term_len: usize) -> String {
    let len = py::char_len(text);
    let start = idx.saturating_sub(70);
    let end = (idx + term_len + 90).min(len);
    let slice = py::char_slice(text, start.min(end), end);
    let mut snip = py::strip(&scan::WS_RUN_RE.replace_all(&slice, " ")).to_string();
    if start > 0 {
        snip = format!("…{}", snip);
    }
    if end < len {
        snip.push('…');
    }
    snip
}

/// Non-overlapping occurrences of `t` in `s` (str.count).
fn count_sub(s: &str, t: &str) -> usize {
    s.matches(t).count()
}

fn char_index_of(s: &str, t: &str) -> Option<usize> {
    s.find(t).map(|b| s[..b].chars().count())
}

fn heatmap(day_heat: &HashMap<NaiveDate, i64>, start: NaiveDate, n: i64) -> Vec<Value> {
    let counts: Vec<i64> = (0..n).map(|i| day_heat.get(&(start + Duration::days(i))).copied().unwrap_or(0)).collect();
    let mx = counts.iter().copied().max().unwrap_or(0);
    (0..n)
        .map(|i| {
            let c = counts[i as usize];
            let level = if c <= 0 || mx <= 0 {
                0
            } else {
                let frac = c as f64 / mx as f64;
                if frac >= 0.75 {
                    4
                } else if frac >= 0.5 {
                    3
                } else if frac >= 0.25 {
                    2
                } else {
                    1
                }
            };
            json!({"date": py::date_iso(&(start + Duration::days(i))), "count": c, "level": level})
        })
        .collect()
}

impl App {
    /// Cached search entry for one file, rebuilt only when (mtime, size) changed.
    pub fn get_search_entry(&self, path: &Path) -> Option<Arc<SearchEntry>> {
        let key = file_key(path)?;
        if let Some((k, e)) = self.search_cache.lock().unwrap().get(path) {
            if *k == key {
                return Some(e.clone());
            }
        }
        let e = Arc::new(if self.is_cursor(path) { build_cursor_search_entry(path)? } else { build_claude_search_entry(path)? });
        self.search_cache.lock().unwrap().insert(path.to_path_buf(), (key, e.clone()));
        Some(e)
    }

    fn live_status_map(&self) -> HashMap<String, Value> {
        let mut out = HashMap::new();
        if let Some(ss) = self.build_payload_memo()["sessions"].as_array() {
            for s in ss {
                if let Some(sid) = s["sessionId"].as_str().filter(|x| !x.is_empty()) {
                    out.insert(sid.to_string(), s.get("status").cloned().unwrap_or_else(|| json!("idle")));
                }
            }
        }
        out
    }

    /// TF-IDF ranked search across all transcripts.
    pub fn search_transcripts(&self, q: &str, limit: usize) -> Vec<Value> {
        let ql = q.to_lowercase();
        let mut terms: Vec<String> = TERM_RE.find_iter(&ql).map(|m| m.as_str().to_string()).collect();
        if terms.is_empty() {
            let s = py::strip(&ql);
            if !s.is_empty() {
                terms.push(s.to_string());
            }
        }
        let terms: Vec<String> = terms.into_iter().collect::<IndexSet<_>>().into_iter().collect();
        if terms.is_empty() {
            return Vec::new();
        }
        let live = self.live_status_map();
        let mut entries: Vec<(Arc<SearchEntry>, IndexMap<String, usize>)> = Vec::new();
        let mut df: HashMap<String, usize> = terms.iter().map(|t| (t.clone(), 0)).collect();
        for path in self.iter_transcript_paths() {
            let Some(e) = self.get_search_entry(&path) else { continue };
            let mut tf: IndexMap<String, usize> = IndexMap::new();
            for t in &terms {
                let c = count_sub(&e.blob, t);
                if c > 0 {
                    tf.insert(t.clone(), c);
                    *df.get_mut(t).unwrap() += 1;
                }
            }
            if !tf.is_empty() {
                entries.push((e, tf));
            }
        }
        let n_docs = entries.len().max(1) as f64;
        let idf: HashMap<&String, f64> = terms.iter().map(|t| (t, (1.0 + n_docs / (1.0 + df[t] as f64)).ln())).collect();
        let names = self.cursor_composer_names();
        let mut results: Vec<(f64, f64, Value)> = Vec::new();
        for (e, tf) in &entries {
            let mut score = 0.0;
            let mut matches = 0usize;
            for (t, c) in tf {
                score += (1.0 + (*c as f64).ln()) * idf[t];
                matches += c;
            }
            score *= 1.0 + 0.5 * (tf.len() as f64 - 1.0);
            let best = tf
                .keys()
                .fold(None::<&String>, |best, t| match best {
                    Some(b) if (df[b], -(py::char_len(b) as i64)) <= (df[t], -(py::char_len(t) as i64)) => Some(b),
                    _ => Some(t),
                })
                .unwrap();
            let idx = char_index_of(&e.blob, best).unwrap_or(0);
            let mut title = e.title.clone();
            if e.cursor {
                if let Some(n) = names.get(&e.session_id) {
                    title = n.clone();
                }
            }
            let la = Value::from(e.last_activity.clone());
            let ep = epoch(Some(&la));
            results.push((
                score,
                ep,
                json!({
                    "sessionId": e.session_id, "title": title, "folder": e.folder,
                    "lastActivity": e.last_activity, "live": live.contains_key(&e.session_id),
                    "status": live.get(&e.session_id).cloned().unwrap_or_else(|| json!("archived")),
                    "snippet": snippet_around(&e.text, idx, py::char_len(best)),
                    "matches": matches, "source": if e.cursor { "cursor" } else { "claude" },
                }),
            ));
        }
        results.sort_by(|a, b| b.0.total_cmp(&a.0).then(b.1.total_cmp(&a.1)));
        results.into_iter().take(limit).map(|(_, _, v)| v).collect()
    }

    /// 91-day heatmap, 30-day daily series, hour/weekday profile, hall of fame.
    pub fn compute_history(&self) -> Value {
        let today = py::utc_today();
        let start91 = today - Duration::days(90);
        let start30 = today - Duration::days(29);
        let mut day_heat: HashMap<NaiveDate, i64> = HashMap::new();
        let mut day_daily: HashMap<NaiveDate, (i64, f64, i64, i64)> = HashMap::new();
        let mut byhour = [0i64; 24];
        let mut bydow = [0i64; 7];
        let (mut tot_output, mut tot_prompts, mut tot_tools) = (0i64, 0i64, 0i64);
        let mut tot_cost = 0.0f64;
        let mut active: HashSet<NaiveDate> = HashSet::new();
        let mut hall: Vec<(i64, Value)> = Vec::new();
        let mut model_out: IndexMap<String, i64> = IndexMap::new();
        let mut model_cost: IndexMap<String, f64> = IndexMap::new();
        let mut model_sess: IndexMap<String, i64> = IndexMap::new();
        let mut folder_cost: IndexMap<String, f64> = IndexMap::new();
        let mut folder_out: IndexMap<String, i64> = IndexMap::new();
        let files = self.iter_transcript_paths();
        for path in &files {
            let Some(agg) = self.scan_file(path) else { continue };
            let tools_all: i64 = agg.per_day.values().map(|d| d.tools).sum();
            let fam = cost::model_family(&agg.model);
            *model_out.entry(fam.clone()).or_insert(0) += agg.tok_output;
            *model_cost.entry(fam.clone()).or_insert(0.0) += agg.cost;
            *model_sess.entry(fam).or_insert(0) += 1;
            *folder_cost.entry(agg.folder.clone()).or_insert(0.0) += agg.cost;
            *folder_out.entry(agg.folder.clone()).or_insert(0) += agg.tok_output;
            let sid = scan::session_id_from_path(path);
            hall.push((
                agg.tok_output,
                json!({
                    "sessionId": sid, "title": self.session_heading(Some(&agg), &sid), "folder": agg.folder,
                    "output": agg.tok_output, "estCostUSD": py::round(agg.cost, 4), "tools": tools_all,
                }),
            ));
            for (diso, dd) in &agg.per_day {
                let Some(d) = py::parse_date(diso) else { continue };
                if d < start91 || d > today {
                    continue;
                }
                *day_heat.entry(d).or_insert(0) += dd.prompts + dd.replies;
                tot_output += dd.output;
                tot_prompts += dd.prompts;
                tot_tools += dd.tools;
                tot_cost += dd.cost;
                if dd.prompts != 0 || dd.replies != 0 || dd.tools != 0 {
                    active.insert(d);
                }
                let dow = weekday(&d);
                for (h, hc) in &dd.hours {
                    if *h < 24 {
                        byhour[*h as usize] += hc;
                        bydow[dow] += hc;
                    }
                }
                if d >= start30 {
                    let e = day_daily.entry(d).or_insert((0, 0.0, 0, 0));
                    e.0 += dd.output;
                    e.1 += dd.cost;
                    e.2 += dd.prompts;
                    e.3 += dd.tools;
                }
            }
        }
        let daily: Vec<Value> = (0..30)
            .map(|i| {
                let d = start30 + Duration::days(i);
                match day_daily.get(&d) {
                    Some(e) => json!({"date": py::date_iso(&d), "output": e.0, "cost": py::round(e.1, 4), "prompts": e.2, "tools": e.3}),
                    None => json!({"date": py::date_iso(&d), "output": 0, "cost": 0.0, "prompts": 0, "tools": 0}),
                }
            })
            .collect();
        hall.sort_by_key(|a| std::cmp::Reverse(a.0));
        let mut mb: Vec<(f64, Value)> = model_out
            .keys()
            .map(|m| {
                let c = py::round(model_cost[m], 2);
                (c, json!({"model": m, "output": model_out[m], "estCostUSD": c, "sessions": model_sess[m]}))
            })
            .collect();
        mb.sort_by(|a, b| b.0.total_cmp(&a.0));
        let mut cf: Vec<(f64, Value)> = folder_cost
            .keys()
            .map(|f| {
                let c = py::round(folder_cost[f], 2);
                (c, json!({"folder": f, "estCostUSD": c, "output": folder_out[f]}))
            })
            .collect();
        cf.sort_by(|a, b| b.0.total_cmp(&a.0));
        json!({
            "heatmap": heatmap(&day_heat, start91, 91),
            "daily": daily,
            "byHour": byhour.to_vec(),
            "byDow": bydow.to_vec(),
            "totals": {
                "transcripts": files.len(), "activeDays": active.len(), "output": tot_output,
                "estCostUSD": py::round(tot_cost, 2), "prompts": tot_prompts, "tools": tot_tools,
            },
            "hallOfFame": hall.into_iter().take(10).map(|(_, v)| v).collect::<Vec<_>>(),
            "modelBreakdown": mb.into_iter().map(|(_, v)| v).collect::<Vec<_>>(),
            "costByFolder": cf.into_iter().take(8).map(|(_, v)| v).collect::<Vec<_>>(),
        })
    }

    /// The collection view across all transcripts (species by hash % 48).
    pub fn compute_pokedex(&self) -> Value {
        let salt = self.dex_salt();
        struct Sp {
            count: i64,
            max_stage: i64,
            total_output: i64,
            shiny: bool,
            example: Option<String>,
            best_out: i64,
        }
        let mut sp: Vec<Sp> = (0..48).map(|_| Sp { count: 0, max_stage: 0, total_output: 0, shiny: false, example: None, best_out: -1 }).collect();
        for path in self.iter_transcript_paths() {
            let sid = scan::session_id_from_path(&path);
            let Some(agg) = self.scan_file(&path) else { continue };
            let s = creature::session_hash_mod(&sid, 48) as usize;
            let shiny = creature::shiny_for_species(s as i64, &salt);
            let stage = creature::stage_for(agg.prompt_count).0 as i64;
            let out = agg.tok_output;
            let d = &mut sp[s];
            d.count += 1;
            d.total_output += out;
            if stage > d.max_stage {
                d.max_stage = stage;
            }
            if shiny {
                d.shiny = true;
            }
            if out > d.best_out || d.example.is_none() {
                d.best_out = out;
                d.example = Some(sid);
            }
        }
        let caught = sp.iter().filter(|d| d.count > 0).count();
        let shiny = sp.iter().filter(|d| d.shiny).count();
        let species: Vec<Value> = sp
            .iter()
            .enumerate()
            .map(|(i, d)| {
                json!({
                    "species": i, "name": creature::SPECIES_NAMES[i], "type": creature::SPECIES_TYPES[i],
                    "seed": creature::species_seed(i as i64), "typeHue": creature::type_hue(creature::SPECIES_TYPES[i]),
                    "caught": d.count > 0, "count": d.count, "maxStage": d.max_stage, "totalOutput": d.total_output,
                    "shiny": d.shiny, "exampleSessionId": d.example,
                })
            })
            .collect();
        json!({"caughtCount": caught, "total": 48, "shinyCount": shiny, "species": species})
    }

    fn folder_of(&self, p: &Path) -> String {
        if self.is_cursor(p) {
            scan::cursor_project_slug(p)
        } else {
            scan::parent_name(p)
        }
    }

    /// The real set of project-dir slugs that contain transcripts.
    pub fn known_folders(&self) -> HashSet<String> {
        self.iter_transcript_paths().iter().map(|p| self.folder_of(p)).filter(|s| !s.is_empty()).collect()
    }

    /// Per-project rollup, or None if the slug is not a known folder.
    pub fn compute_project(&self, slug: &str) -> Option<Value> {
        if slug.is_empty() || !self.known_folders().contains(slug) {
            return None;
        }
        let today = py::utc_today();
        let start91 = today - Duration::days(90);
        let mut day_heat: HashMap<NaiveDate, i64> = HashMap::new();
        let mut active: HashSet<NaiveDate> = HashSet::new();
        let mut files_agg: IndexMap<String, FileTouch> = IndexMap::new();
        let mut model_out: IndexMap<String, i64> = IndexMap::new();
        let mut model_cost: IndexMap<String, f64> = IndexMap::new();
        let mut model_sess: IndexMap<String, i64> = IndexMap::new();
        let mut sessions: Vec<(f64, Value)> = Vec::new();
        let (mut tot_prompts, mut tot_tools, mut tot_output) = (0i64, 0i64, 0i64);
        let mut tot_cost = 0.0f64;
        let mut n = 0i64;
        for path in self.iter_transcript_paths() {
            if self.folder_of(&path) != slug {
                continue;
            }
            let Some(agg) = self.scan_file(&path) else { continue };
            n += 1;
            let sess_tools: i64 = agg.per_day.values().map(|d| d.tools).sum();
            for (diso, dd) in &agg.per_day {
                tot_prompts += dd.prompts;
                tot_tools += dd.tools;
                tot_output += dd.output;
                tot_cost += dd.cost;
                let Some(d) = py::parse_date(diso) else { continue };
                if dd.prompts != 0 || dd.replies != 0 || dd.tools != 0 {
                    active.insert(d);
                }
                if start91 <= d && d <= today {
                    *day_heat.entry(d).or_insert(0) += dd.prompts + dd.replies;
                }
            }
            for fe in agg.files.values() {
                if fe.path.is_empty() {
                    continue;
                }
                match files_agg.get_mut(&fe.path) {
                    Some(cur) => cur.count += fe.count,
                    None => {
                        files_agg.insert(fe.path.clone(), fe.clone());
                    }
                }
            }
            let fam = cost::model_family(&agg.model);
            *model_out.entry(fam.clone()).or_insert(0) += agg.tok_output;
            *model_cost.entry(fam.clone()).or_insert(0.0) += agg.cost;
            *model_sess.entry(fam).or_insert(0) += 1;
            let sid = scan::session_id_from_path(&path);
            let la = agg.last_activity.map(|d| py::iso(&d)).unwrap_or_default();
            let ep = epoch(Some(&Value::from(la.clone())));
            sessions.push((
                ep,
                json!({
                    "sessionId": sid, "title": self.session_heading(Some(&agg), &sid),
                    "output": agg.tok_output, "estCostUSD": py::round(agg.cost, 4),
                    "tools": sess_tools, "lastActivity": la,
                }),
            ));
        }
        let mut top: Vec<&FileTouch> = files_agg.values().collect();
        top.sort_by_key(|a| std::cmp::Reverse(a.count));
        let top_files: Vec<Value> = top.into_iter().take(15).map(|f| json!({"path": f.path, "action": f.action, "count": f.count})).collect();
        let mut models: Vec<(f64, Value)> = model_out
            .keys()
            .map(|m| {
                let c = py::round(model_cost[m], 4);
                (c, json!({"model": m, "output": model_out[m], "estCostUSD": c, "sessions": model_sess[m]}))
            })
            .collect();
        models.sort_by(|a, b| b.0.total_cmp(&a.0));
        sessions.sort_by(|a, b| b.0.total_cmp(&a.0));
        Some(json!({
            "folder": slug,
            "prettyFolder": pretty_folder(slug),
            "totals": {
                "sessions": n, "prompts": tot_prompts, "tools": tot_tools, "output": tot_output,
                "estCostUSD": py::round(tot_cost, 4), "activeDays": active.len(),
            },
            "heatmap": heatmap(&day_heat, start91, 91),
            "topFiles": top_files,
            "models": models.into_iter().map(|(_, v)| v).collect::<Vec<_>>(),
            "sessions": sessions.into_iter().take(40).map(|(_, v)| v).collect::<Vec<_>>(),
        }))
    }

    /// First human prompt, last reply and touched file basenames on the days.
    fn digest_range_detail(&self, path: &Path, days: &HashSet<String>) -> (Option<String>, Option<String>, Vec<String>) {
        let (mut first, mut last, mut files) = (None, None, Vec::new());
        let mut seen: HashSet<String> = HashSet::new();
        let Some(recs) = scan::open_records(path) else { return (first, last, files) };
        for (_line, rec) in recs {
            let o = rec.as_object().unwrap();
            let _ = (|| -> Option<()> {
                let ts = o.get("timestamp").and_then(|v| v.as_str()).and_then(py::parse_iso_utc)?;
                if !days.contains(&py::date_iso(&ts.date_naive())) {
                    return Some(());
                }
                if matches!(o.get("message"), Some(v) if py::truthy(v) && !v.is_object()) {
                    return None;
                }
                let content = o.get("message").and_then(|m| m.as_object()).and_then(|m| m.get("content"));
                match o.get("type").and_then(|v| v.as_str()) {
                    Some("user") => {
                        if scan::is_real_human_prompt(content)? && first.is_none() {
                            first = Some(scan::clean_prompt(content)?);
                        }
                    }
                    Some("assistant") => {
                        if let Some(Value::Array(blocks)) = content {
                            let mut texts: Vec<String> = Vec::new();
                            for b in blocks {
                                let Value::Object(b) = b else { continue };
                                match b.get("type").and_then(|v| v.as_str()) {
                                    Some("text") => texts.push(b.get("text").and_then(|v| v.as_str()).unwrap_or("").to_string()),
                                    Some("tool_use") => {
                                        if let Some(Value::Object(inp)) = b.get("input") {
                                            let fp = inp.get("file_path");
                                            let p = if fp.map(py::truthy).unwrap_or(false) { fp } else { inp.get("path") };
                                            if let Some(Value::String(p)) = p {
                                                if !py::strip(p).is_empty() {
                                                    let bn = Path::new(p.trim_end_matches('/'))
                                                        .file_name()
                                                        .map(|n| n.to_string_lossy().into_owned())
                                                        .unwrap_or_default();
                                                    if !bn.is_empty() && seen.insert(bn.clone()) {
                                                        files.push(bn);
                                                    }
                                                }
                                            }
                                        }
                                    }
                                    _ => {}
                                }
                            }
                            if !texts.is_empty() {
                                last = Some(scan::strip_markdown(&texts.join("\n")));
                            }
                        }
                    }
                    _ => {}
                }
                Some(())
            })();
        }
        (first, last, files)
    }

    /// The digest payload for one day or the `days` days ending at `diso`.
    pub fn compute_digest(&self, diso: &str, days: i64) -> Value {
        let days = days.clamp(1, 31);
        let (end_d, diso) = match py::parse_date(diso) {
            Some(d) => (d, diso.to_string()),
            None => {
                let d = py::local_today();
                (d, py::date_iso(&d))
            }
        };
        let start_d = end_d - Duration::days(days - 1);
        let date_list: Vec<String> = (0..days).map(|i| py::date_iso(&(start_d + Duration::days(i)))).collect();
        let date_set: HashSet<String> = date_list.iter().cloned().collect();
        let mut entries: Vec<(i64, i64, Value)> = Vec::new();
        let (mut tp, mut tt, mut to) = (0i64, 0i64, 0i64);
        let mut tc = 0.0f64;
        for path in self.iter_transcript_paths() {
            let Some(agg) = self.scan_file(&path) else { continue };
            let (mut sp, mut st, mut so) = (0i64, 0i64, 0i64);
            let mut sc = 0.0f64;
            let mut present = false;
            for di in &date_list {
                let Some(day) = agg.per_day.get(di) else { continue };
                present = true;
                sp += day.prompts;
                st += day.tools;
                so += day.output;
                sc += day.cost;
            }
            if !present {
                continue;
            }
            let (fp, lr, files) = self.digest_range_detail(&path, &date_set);
            let sid = scan::session_id_from_path(&path);
            let links: Vec<Value> = agg.links.iter().take(4).map(|l| Value::from(l.url.clone())).collect();
            entries.push((
                sp + st,
                so,
                json!({
                    "sessionId": sid, "title": self.session_heading(Some(&agg), &sid),
                    "folder": pretty_folder(&agg.folder), "prompts": sp, "tools": st, "output": so,
                    "cost": py::round(sc, 4),
                    "firstPrompt": fp.filter(|s| !s.is_empty()).or_else(|| agg.first_prompt.clone()).unwrap_or_default(),
                    "lastReply": lr.filter(|s| !s.is_empty()).or_else(|| agg.last_reply.clone()).unwrap_or_default(),
                    "files": files.into_iter().take(8).collect::<Vec<_>>(),
                    "links": links,
                }),
            ));
            tp += sp;
            tt += st;
            to += so;
            tc += sc;
        }
        entries.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));
        let entries: Vec<Value> = entries.into_iter().map(|(_, _, v)| v).collect();
        let (mut lines, empty_span) = if days == 1 {
            (vec![format!("# Claude HQ — Daily Digest — {}", diso), String::new()], diso.clone())
        } else {
            let s = py::date_iso(&start_d);
            (vec![format!("# Claude HQ — Digest — {} → {}", s, diso), String::new()], format!("{} → {}", s, diso))
        };
        if entries.is_empty() {
            lines.push(format!("_No session activity on {}._", empty_span));
        } else {
            lines.push(format!(
                "**{} session{} active** · {} prompts · {} tool calls · ~{} output tokens · ~${:.2} list-price est.",
                entries.len(),
                if entries.len() == 1 { "" } else { "s" },
                tp,
                tt,
                py::thousands(to),
                tc
            ));
            lines.push(String::new());
            for e in &entries {
                lines.push(format!("## {}  ({})", e["title"].as_str().unwrap_or(""), e["folder"].as_str().unwrap_or("")));
                let fp = e["firstPrompt"].as_str().unwrap_or("");
                if !fp.is_empty() {
                    lines.push(format!("- Started with: {}", py::truncate(fp, 220)));
                }
                let lr = e["lastReply"].as_str().unwrap_or("");
                if !lr.is_empty() {
                    lines.push(format!("- Last outcome: {}", py::truncate(lr, 220)));
                }
                lines.push(format!(
                    "- {} prompts · {} tools · ~{} output tok · ~${:.2}",
                    e["prompts"],
                    e["tools"],
                    py::thousands(e["output"].as_i64().unwrap_or(0)),
                    e["cost"].as_f64().unwrap_or(0.0)
                ));
                let files: Vec<&str> = e["files"].as_array().unwrap().iter().filter_map(|f| f.as_str()).collect();
                if !files.is_empty() {
                    lines.push(format!("- Files touched: {}", files.join(", ")));
                }
                let links: Vec<&str> = e["links"].as_array().unwrap().iter().filter_map(|f| f.as_str()).collect();
                if !links.is_empty() {
                    lines.push(format!("- Links: {}", links.join(" ")));
                }
                lines.push(String::new());
            }
        }
        json!({
            "date": diso, "days": days, "markdown": lines.join("\n"), "sessionCount": entries.len(),
            "totals": {"prompts": tp, "tools": tt, "output": to, "estCostUSD": py::round(tc, 2)},
        })
    }

    /// A handful of observations from the scan cache and the live payload.
    pub fn compute_insights(&self) -> Value {
        let today = py::utc_today();
        let d7 = today - Duration::days(6);
        let d14 = today - Duration::days(13);
        let lw_end = today - Duration::days(7);
        let d30 = today - Duration::days(29);
        let mut day_cost: IndexMap<NaiveDate, f64> = IndexMap::new();
        let mut day_activity: IndexMap<NaiveDate, i64> = IndexMap::new();
        let mut day_output: IndexMap<NaiveDate, i64> = IndexMap::new();
        let mut folder_last: IndexMap<String, NaiveDate> = IndexMap::new();
        let mut folder_week: IndexMap<String, i64> = IndexMap::new();
        let mut hourly = [0i64; 24];
        for path in self.iter_transcript_paths() {
            let Some(agg) = self.scan_file(&path) else { continue };
            let fol = agg.folder.clone();
            if let Some(la) = agg.last_activity {
                let ld = la.date_naive();
                if folder_last.get(&fol).map(|x| ld > *x).unwrap_or(true) {
                    folder_last.insert(fol.clone(), ld);
                }
            }
            for (diso, dd) in &agg.per_day {
                let Some(d) = py::parse_date(diso) else { continue };
                if d > today {
                    continue;
                }
                *day_cost.entry(d).or_insert(0.0) += dd.cost;
                *day_activity.entry(d).or_insert(0) += dd.prompts + dd.replies;
                *day_output.entry(d).or_insert(0) += dd.output;
                if d7 <= d && d <= today {
                    *folder_week.entry(fol.clone()).or_insert(0) += dd.prompts + dd.tools;
                }
                if d30 <= d && d <= today {
                    for (h, hc) in &dd.hours {
                        if *h < 24 {
                            hourly[*h as usize] += hc;
                        }
                    }
                }
            }
        }
        let mut insights: Vec<Value> = Vec::new();
        let this_week: f64 = day_cost.iter().filter(|(d, _)| d7 <= **d && **d <= today).map(|(_, v)| *v).fold(0.0, |a, b| a + b);
        let last_week: f64 = day_cost.iter().filter(|(d, _)| d14 <= **d && **d <= lw_end).map(|(_, v)| *v).fold(0.0, |a, b| a + b);
        if this_week > 0.0 || last_week > 0.0 {
            let (detail, kind) = if last_week > 0.0 {
                let pct = (this_week - last_week) / last_week * 100.0;
                let arrow = if pct >= 0.0 { "▲" } else { "▼" };
                (
                    format!("Spend ${:.2} this week ({} {}% vs last week's ${:.2})", this_week, arrow, py::round_int(pct).abs(), last_week),
                    if pct > 25.0 { "warn" } else { "info" },
                )
            } else {
                (format!("Spend ${:.2} this week (nothing last week)", this_week), "info")
            };
            insights.push(json!({"icon": "💸", "title": "Weekly spend", "detail": detail, "kind": kind}));
        }
        if let Some((top, score)) = folder_week.iter().fold(None::<(&String, i64)>, |best, (k, v)| match best {
            Some((_, b)) if b >= *v => best,
            _ => Some((k, *v)),
        }) {
            if score > 0 {
                insights.push(json!({"icon": "🔥", "title": "Busiest project",
                    "detail": format!("{} — {} prompts + tool calls in the last 7 days", pretty_folder(top), score), "kind": "good"}));
            }
        }
        let mut dormant: Vec<(i64, String)> =
            folder_last.iter().map(|(f, ld)| ((today - *ld).num_days(), f.clone())).filter(|(age, _)| *age > 14).collect();
        dormant.sort_by(|a, b| b.cmp(a));
        for (age, fol) in dormant.into_iter().take(2) {
            insights.push(json!({"icon": "💤", "title": "Dormant project", "detail": format!("{} untouched {} days", pretty_folder(&fol), age), "kind": "warn"}));
        }
        let mut biggest: Option<(NaiveDate, i64)> = None;
        for (d, v) in &day_activity {
            if d30 <= *d && *d <= today && *v > 0 && biggest.map(|b| *v > b.1).unwrap_or(true) {
                biggest = Some((*d, *v));
            }
        }
        if let Some((d, v)) = biggest {
            insights.push(json!({"icon": "📈", "title": "Biggest day",
                "detail": format!("{} was your busiest — {} prompts + replies", py::date_iso(&d), v), "kind": "info"}));
        }
        let out_week: i64 = day_output.iter().filter(|(d, _)| d7 <= **d && **d <= today).map(|(_, v)| *v).sum();
        if out_week > 0 {
            insights.push(json!({"icon": "✍️", "title": "Output this week",
                "detail": format!("~{} output tokens in the last 7 days", py::thousands(out_week)), "kind": "info"}));
        }
        let p = self.build_payload_memo();
        let ss = p["sessions"].as_array().cloned().unwrap_or_default();
        let needs = ss.iter().filter(|s| s["status"] == "needs").count();
        let stuck = ss.iter().filter(|s| s.get("stuck").map(py::truthy).unwrap_or(false)).count();
        let attn = needs + stuck;
        if attn > 0 {
            insights.push(json!({"icon": "⚠️", "title": "Needs attention",
                "detail": format!("{} tab{} stuck or awaiting input right now", attn, if attn == 1 { "" } else { "s" }), "kind": "warn"}));
        }
        if hourly.iter().any(|h| *h != 0) {
            let peak = (0..24).fold(0usize, |best, h| if hourly[h] > hourly[best] { h } else { best });
            if hourly[peak] > 0 {
                let ampm = if peak < 12 { "am" } else { "pm" };
                let h12 = if peak % 12 == 0 { 12 } else { peak % 12 };
                insights.push(json!({"icon": "🕒", "title": "Peak hour",
                    "detail": format!("Most active around {}{} local — {} actions over 30 days", h12, ampm, hourly[peak]), "kind": "info"}));
            }
        }
        json!({"generated": py::iso(&py::now_utc()), "insights": insights})
    }

    /// Cached ordered conversation events for one transcript.
    pub fn get_transcript_events(&self, path: &Path) -> Arc<Vec<Value>> {
        let Some(key) = file_key(path) else { return Arc::new(Vec::new()) };
        if let Some((k, e)) = self.events_cache.lock().unwrap().get(path) {
            if *k == key {
                return e.clone();
            }
        }
        let ev = Arc::new(if self.is_cursor(path) { cursor_events(path) } else { claude_events(path) });
        self.events_cache.lock().unwrap().insert(path.to_path_buf(), (key, ev.clone()));
        ev
    }

    /// The whole session as a readable Markdown transcript.
    pub fn build_session_markdown(&self, sid: &str, path: &Path) -> String {
        let agg = self.scan_file(path);
        let events = self.get_transcript_events(path);
        let title = self.session_heading(agg.as_deref(), sid);
        let folder = agg.as_ref().map(|a| pretty_folder(&a.folder)).unwrap_or_else(|| "~".into());
        let who = if agg.as_ref().map(|a| a.cursor).unwrap_or(false) { "Cursor" } else { "Claude" };
        let ts: Vec<&str> = events.iter().filter_map(|e| e["t"].as_str()).filter(|t| !t.is_empty()).collect();
        let mut lines = vec![format!("# {}", title), String::new(), format!("- Folder: {}", folder), format!("- Source: {}", who), format!("- Session: {}", sid)];
        if !ts.is_empty() {
            let f = |s: &str| py::take_chars(s, 19).replace('T', " ");
            lines.push(format!("- Range: {} → {}", f(ts[0]), f(ts[ts.len() - 1])));
        }
        lines.push(String::new());
        for e in events.iter() {
            let text = e["text"].as_str().unwrap_or("");
            match e["role"].as_str() {
                Some("you") => lines.push(format!("**You:** {}", text)),
                Some("claude") => lines.push(format!("**{}:** {}", who, text)),
                Some("tool") => lines.push(format!("`{}` {}", e["tool"].as_str().filter(|s| !s.is_empty()).unwrap_or("tool"), text)),
                Some("system") => lines.push(format!("_system:_ {}", text)),
                _ => {}
            }
            lines.push(String::new());
        }
        lines.join("\n")
    }
}

fn ev(i: usize, t: &str, role: &str, text: &str, tool: Option<&str>) -> Value {
    json!({"i": i, "t": t, "role": role, "text": py::truncate(text, 1200), "tool": tool})
}

fn claude_events(path: &Path) -> Vec<Value> {
    let mut events = Vec::new();
    let Some(recs) = scan::open_records(path) else { return events };
    let mut i = 0usize;
    for (_line, rec) in recs {
        let o = rec.as_object().unwrap();
        let _ = (|| -> Option<()> {
            let ts = o.get("timestamp").and_then(|v| v.as_str()).and_then(py::parse_iso_utc);
            let tiso = ts.map(|d| py::iso(&d)).unwrap_or_default();
            let msg = || -> Option<Option<&serde_json::Map<String, Value>>> {
                match o.get("message") {
                    Some(Value::Object(m)) => Some(Some(m)),
                    Some(v) if py::truthy(v) => None,
                    _ => Some(None),
                }
            };
            match o.get("type").and_then(|v| v.as_str()) {
                Some("user") => {
                    let content = msg()?.and_then(|m| m.get("content"));
                    if scan::is_real_human_prompt(content)? {
                        events.push(ev(i, &tiso, "you", &scan::clean_prompt(content)?, None));
                        i += 1;
                    }
                }
                Some("assistant") => {
                    if let Some(Value::Array(blocks)) = msg()?.and_then(|m| m.get("content")) {
                        let mut texts: Vec<String> = Vec::new();
                        for b in blocks {
                            let Value::Object(b) = b else { continue };
                            match b.get("type").and_then(|v| v.as_str()) {
                                Some("text") => {
                                    let txt = match b.get("text") {
                                        Some(Value::String(s)) => s.clone(),
                                        Some(v) if py::truthy(v) => return None,
                                        _ => String::new(),
                                    };
                                    if !py::strip(&txt).is_empty() {
                                        texts.push(txt);
                                    }
                                }
                                Some("tool_use") => {
                                    if !texts.is_empty() {
                                        events.push(ev(i, &tiso, "claude", &scan::strip_markdown(&texts.join("\n")), None));
                                        i += 1;
                                        texts.clear();
                                    }
                                    let name = match b.get("name") {
                                        Some(Value::String(s)) if !s.is_empty() => s.clone(),
                                        _ => "Tool".into(),
                                    };
                                    events.push(ev(i, &tiso, "tool", &scan::tool_label(b), Some(&name)));
                                    i += 1;
                                }
                                _ => {}
                            }
                        }
                        if !texts.is_empty() {
                            events.push(ev(i, &tiso, "claude", &scan::strip_markdown(&texts.join("\n")), None));
                            i += 1;
                        }
                    }
                }
                Some("system")
                    if scan::record_error_sig(&rec).is_some() => {
                        let content = match o.get("content") {
                            Some(Value::String(s)) if !py::strip(s).is_empty() => s.clone(),
                            _ => match o.get("error") {
                                Some(Value::Object(e)) if e.get("message").map(|m| m.is_string()).unwrap_or(false) => {
                                    e["message"].as_str().unwrap().to_string()
                                }
                                Some(Value::String(s)) => s.clone(),
                                Some(Value::Null) | None => "API error".into(),
                                Some(v) => crate::pyjson::dumps(v),
                            },
                        };
                        events.push(ev(i, &tiso, "system", &scan::strip_markdown(&content), None));
                        i += 1;
                    }
                _ => {}
            }
            Some(())
        })();
    }
    events
}

fn cursor_events(path: &Path) -> Vec<Value> {
    let mut events: Vec<Value> = Vec::new();
    let Some(recs) = scan::open_records(path) else { return events };
    let mut turn_ts: Option<py::Dt> = None;
    let mut pending: Vec<(String, String, Option<String>)> = Vec::new();
    let add = |events: &mut Vec<Value>, ts: Option<py::Dt>, role: &str, text: &str, tool: Option<&str>| {
        let n = events.len();
        events.push(ev(n, &ts.map(|d| py::iso(&d)).unwrap_or_default(), role, text, tool));
    };
    for (_line, rec) in recs {
        let o = rec.as_object().unwrap();
        let mut emit = |events: &mut Vec<Value>, ts: Option<py::Dt>, role: &str, text: String, tool: Option<String>| {
            if let Some(ts) = ts {
                if turn_ts.map(|tt| ts >= tt).unwrap_or(true) {
                    if turn_ts.is_none() && !pending.is_empty() {
                        let queued = std::mem::take(&mut pending);
                        turn_ts = Some(ts);
                        for (r, t, tl) in queued {
                            add(events, Some(ts), &r, &t, tl.as_deref());
                        }
                    }
                    turn_ts = Some(ts);
                }
            }
            match ts.or(turn_ts) {
                None => pending.push((role.to_string(), text, tool)),
                Some(u) => add(events, Some(u), role, &text, tool.as_deref()),
            }
        };
        match o.get("role").and_then(|v| v.as_str()) {
            Some("user") => {
                let t = scan::cursor_message_text(o);
                if let Some(q) = scan::cursor_user_query(&t) {
                    emit(&mut events, scan::parse_cursor_clock(&t), "you", q, None);
                }
            }
            Some("assistant") => {
                let blocks = match o.get("message") {
                    Some(Value::Object(m)) => match m.get("content") {
                        Some(Value::String(s)) => vec![json!({"type": "text", "text": s})],
                        Some(Value::Array(a)) => a.clone(),
                        _ => continue,
                    },
                    Some(v) if py::truthy(v) => continue,
                    _ => continue,
                };
                let mut texts: Vec<String> = Vec::new();
                for b in &blocks {
                    let Value::Object(b) = b else { continue };
                    let ty = b.get("type").and_then(|v| v.as_str());
                    if ty == Some("text") {
                        let t = b.get("text").and_then(|v| v.as_str()).unwrap_or("");
                        if !py::strip(t).is_empty() {
                            texts.push(t.to_string());
                        }
                    } else if ty == Some("tool_use") {
                        if !texts.is_empty() {
                            emit(&mut events, None, "claude", scan::strip_markdown(&texts.join("\n")), None);
                            texts.clear();
                        }
                        let name = match b.get("name") {
                            Some(Value::String(s)) if !s.is_empty() => s.clone(),
                            _ => "Tool".into(),
                        };
                        emit(&mut events, None, "tool", scan::tool_label(b), Some(name));
                    }
                }
                if !texts.is_empty() {
                    emit(&mut events, None, "claude", scan::strip_markdown(&texts.join("\n")), None);
                }
            }
            _ => {}
        }
    }
    if !pending.is_empty() {
        let mt = std::fs::metadata(path)
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .and_then(|d| py::from_ts(d.as_secs_f64()));
        if mt.is_some() {
            for (r, t, tl) in std::mem::take(&mut pending) {
                add(&mut events, mt, &r, &t, tl.as_deref());
            }
        }
    }
    events
}

/// CSV: one header + one row per payload session (csv.writer rules).
pub fn build_export_csv(payload: &Value) -> String {
    fn field(v: &Value) -> String {
        let s = match v {
            Value::String(s) => s.clone(),
            Value::Null => String::new(),
            Value::Bool(b) => if *b { "True" } else { "False" }.to_string(),
            Value::Number(n) => {
                if n.is_f64() {
                    py::float_repr(n.as_f64().unwrap())
                } else {
                    n.to_string()
                }
            }
            x => x.to_string(),
        };
        if s.contains([',', '"', '\r', '\n']) {
            format!("\"{}\"", s.replace('"', "\"\""))
        } else {
            s
        }
    }
    let mut out = String::from("id,title,folder,status,promptCount,ageSecs,outputTokens,estCostUSD,model,lastActivity\r\n");
    let empty = Vec::new();
    for s in payload["sessions"].as_array().unwrap_or(&empty) {
        let tok = s.get("tokens").filter(|t| py::truthy(t)).cloned().unwrap_or_else(|| json!({}));
        let get = |o: &Value, k: &str, d: Value| o.get(k).cloned().unwrap_or(d);
        let model = tok.get("model").filter(|m| py::truthy(m)).cloned().unwrap_or_else(|| json!(""));
        let row = [
            get(s, "sessionId", json!("")),
            get(s, "title", json!("")),
            get(s, "folder", json!("")),
            get(s, "status", json!("")),
            get(s, "promptCount", json!(0)),
            get(s, "ageSecs", json!(0)),
            get(&tok, "output", json!(0)),
            get(&tok, "estCostUSD", json!(0.0)),
            model,
            get(s, "lastActivity", json!("")),
        ];
        out.push_str(&row.iter().map(field).collect::<Vec<_>>().join(","));
        out.push_str("\r\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::testutil::*;

    const SID: &str = "22222222-2222-4222-8222-222222222222";

    #[test]
    fn transcript_error_events() {
        let (_d, app) = app();
        let p = app.paths.projects_dir.join("proj").join(format!("{}.jsonl", SID));
        write_jsonl(&p, &[
            json!({"type": "system", "subtype": "away_summary", "timestamp": "2026-09-01T10:00:00.000Z", "content": "We discussed billing and the rate limit."}),
            json!({"type": "system", "subtype": "api_error", "timestamp": "2026-09-01T10:00:00.000Z", "error": {"message": "Overloaded"}, "retryAttempt": 1, "maxRetries": 10}),
            json!({"type": "system", "subtype": "api_error", "timestamp": "2026-09-01T10:00:00.000Z", "error": {"message": "Overloaded"}, "retryAttempt": 10, "maxRetries": 10}),
        ]);
        let ev = app.get_transcript_events(&p);
        let sys: Vec<&Value> = ev.iter().filter(|e| e["role"] == "system").collect();
        assert_eq!(sys.len(), 1);
        assert!(sys[0]["text"].as_str().unwrap().contains("Overloaded"));
    }

    #[test]
    fn search_ranks_and_snippets() {
        let (_d, app) = app();
        let p = app.paths.projects_dir.join("proj").join(format!("{}.jsonl", SID));
        write_jsonl(&p, &[
            json!({"type": "ai-title", "aiTitle": "Fix the flux capacitor"}),
            json!({"type": "user", "timestamp": "2026-09-01T10:00:00.000Z", "message": {"content": "the flux capacitor is broken"}}),
        ]);
        let r = app.search_transcripts("Flux", 40);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0]["title"], "Fix the flux capacitor");
        assert_eq!(r[0]["matches"], 2);
        assert_eq!(r[0]["status"], "stale"); // archived cards are in the payload too
        assert!(r[0]["snippet"].as_str().unwrap().contains("flux"));
        assert!(app.search_transcripts("zzzz", 40).is_empty());
    }

    #[test]
    fn csv_quotes_like_python() {
        let p = json!({"sessions": [{"sessionId": "a", "title": "x, \"y\"", "folder": "f", "status": "idle", "promptCount": 1, "ageSecs": 2,
                                     "tokens": {"output": 3, "estCostUSD": 0.0, "model": ""}, "lastActivity": ""}]});
        assert_eq!(build_export_csv(&p), "id,title,folder,status,promptCount,ageSecs,outputTokens,estCostUSD,model,lastActivity\r\na,\"x, \"\"y\"\"\",f,idle,1,2,3,0.0,,\r\n");
    }

    #[test]
    fn cursor_transcript_roles() {
        let (_d, app) = app();
        let sid = "33333333-3333-4333-8333-333333333333";
        let path = app.paths.cursor_projects_dir.join("Users-ada-demo/agent-transcripts").join(sid).join(format!("{}.jsonl", sid));
        let stamp = "<timestamp>Thursday, Sep 24, 2026, 1:44 PM (UTC+5:30)</timestamp>\n<user_query>fix the milestone bug</user_query>";
        write_jsonl(&path, &[
            json!({"role": "user", "message": {"content": [{"type": "text", "text": "<git_status>snapshot only</git_status>"}]}}),
            json!({"role": "user", "message": {"content": [{"type": "text", "text": stamp}]}}),
            json!({"role": "assistant", "message": {"content": [{"type": "text", "text": "Looking now."}, {"type": "tool_use", "name": "Read", "input": {"path": "/tmp/a.py"}}]}}),
            json!({"role": "user", "message": {"content": [{"type": "text", "text": "<user_query>Briefly inform the user about the task result and perform any follow-up actions.</user_query>"}]}}),
            json!({"role": "assistant", "message": {"content": [{"type": "text", "text": "Fixed the consumer."}]}}),
            json!({"type": "turn_ended", "status": "success"}),
        ]);
        let sub = path.parent().unwrap().join("subagents").join("44444444-4444-4444-8444-444444444444.jsonl");
        write_jsonl(&sub, &[json!({})]);
        let agg = app.scan_file(&path).unwrap();
        assert!(agg.cursor);
        assert_eq!(agg.folder, "Users-ada-demo");
        assert_eq!(agg.prompt_count, 1);
        assert_eq!(agg.first_prompt.as_deref(), Some("fix the milestone bug"));
        assert_eq!(agg.ai_title.as_deref(), Some("fix the milestone bug"));
        assert!(agg.last_reply.as_deref().unwrap().contains("Fixed the consumer"));
        assert_eq!(agg.per_day["2026-09-24"].prompts, 1);
        assert_eq!(agg.per_day["2026-09-24"].tools, 1);
        assert_eq!(agg.files["/tmp/a.py"].action, "read");
        assert_eq!(app.find_transcript(sid), Some(path.clone()));
        assert_eq!(app.iter_transcript_paths(), vec![path.clone()]);
        let roles: Vec<String> = app.get_transcript_events(&path).iter().map(|e| e["role"].as_str().unwrap().to_string()).collect();
        assert_eq!(roles, vec!["you", "claude", "tool", "claude"]);
    }

    #[test]
    fn digest_and_project() {
        let (_d, app) = app();
        let p = app.paths.projects_dir.join("-Users-ada-proj").join(format!("{}.jsonl", SID));
        write_jsonl(&p, &[
            json!({"type": "user", "timestamp": "2026-09-01T10:00:00.000Z", "message": {"content": "ship it"}}),
            json!({"type": "assistant", "timestamp": "2026-09-01T10:01:00.000Z", "message": {"id": "m", "model": "claude-sonnet-4-6",
                   "content": [{"type": "text", "text": "done"}, {"type": "tool_use", "id": "t", "name": "Edit", "input": {"file_path": "/r/a.py"}}],
                   "usage": {"input_tokens": 1000, "output_tokens": 1500}}}),
        ]);
        let d = app.compute_digest("2026-09-01", 1);
        assert_eq!(d["sessionCount"], 1);
        let md = d["markdown"].as_str().unwrap();
        assert!(md.starts_with("# Claude HQ — Daily Digest — 2026-09-01"));
        assert!(md.contains("1 session active** · 1 prompts · 1 tool calls · ~1,500 output tokens"));
        assert!(md.contains("- Files touched: a.py"));
        let pr = app.compute_project("-Users-ada-proj").unwrap();
        assert_eq!(pr["prettyFolder"], "proj");
        assert_eq!(pr["totals"]["sessions"], 1);
        assert_eq!(pr["topFiles"][0]["action"], "edit");
        assert!(app.compute_project("../etc").is_none());
    }
}
