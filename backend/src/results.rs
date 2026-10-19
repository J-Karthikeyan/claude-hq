//! Finished games, written down: `backend/app/results.py`'s write half.
//!
//! Every multiplayer game the Arena referees leaves one row per player in
//! `game_results`, minted from the game's own "done" event and never from a
//! client -- which is the whole reason the levels, leaderboards and trainer
//! cards `progress.rs` serves can be trusted. Python intercepts the event in
//! `valley._flush`, the one place every outbound game payload passes through;
//! here each hub's `flush` does it, which is the same chokepoint three times
//! over because the hubs do not share one.
//!
//! The read half -- game XP, boards, profiles -- is already in `progress.rs`,
//! and until this module existed it was reading a table nothing ever wrote.
//!
//! All five of Python's games are here. Golf is the odd one: it is scored from
//! a `totals` MAP rather than a `results` list, and its places are DENSE --
//! two players tied on strokes share a place and the next is not skipped.

use serde_json::{json, Value};
use sqlx::SqlitePool;

/// The games this module knows how to write down. Python's `results.GAMES`
/// (results.py:25), in its order.
pub const GAMES: [&str; 7] = ["kart", "plat", "fps", "golf", "type", "td", "bowl"];

/// One `game_results` row, before it reaches the database.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub user_id: String,
    pub game: String,
    pub key: String,
    pub mode: String,
    pub place: i64,
    pub players: i64,
    /// Milliseconds, strokes or kills. Lower is better except kills; NULL for a
    /// player who did not finish.
    pub value: Option<i64>,
    pub extra: Value,
}

/// `entry["user"]["userId"]`, when it is a string. Python's `_uid`.
fn uid(entry: &Value) -> Option<&str> {
    entry.get("user")?.get("userId")?.as_str()
}

/// Python's `int(x or default)` over a JSON number: a missing, null, zero or
/// non-numeric value falls back, and a float truncates toward zero.
fn int_or(v: Option<&Value>, default: i64) -> i64 {
    match v.and_then(Value::as_f64) {
        Some(n) if n != 0.0 => n as i64,
        _ => default,
    }
}

/// Python's `int(x or 0)`.
fn int_or_zero(v: Option<&Value>) -> i64 {
    int_or(v, 0)
}

/// Python's `float(x or 0)`.
fn float_or_zero(v: Option<&Value>) -> f64 {
    v.and_then(Value::as_f64).unwrap_or(0.0)
}

/// Python's `str(x or "")[:n]` -- a *character* clip, and a number or null
/// becomes the empty string rather than "0" or "None", because `or` tests
/// truthiness before `str` ever runs.
fn clipped_str(v: Option<&Value>, n: usize) -> String {
    match v.and_then(Value::as_str) {
        Some(s) => s.chars().take(n).collect(),
        None => String::new(),
    }
}

/// Truthy in Python's sense: present, and not null / false / 0 / "".
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0) != 0.0,
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

/// The rows a game's "done" event becomes. Pure, like the Python's, so the
/// shape of every column is testable without a database or a game.
pub fn rows_from_done(game: &str, data: &Value) -> Vec<Row> {
    if !GAMES.contains(&game) {
        return Vec::new();
    }
    // Golf never goes near the `results` list: it is ranked out of `totals`.
    if game == "golf" {
        return golf_rows(data);
    }
    let empty: Vec<Value> = Vec::new();
    let res: Vec<&Value> = data
        .get("results")
        .and_then(Value::as_array)
        .unwrap_or(&empty)
        .iter()
        .filter(|r| uid(r).is_some())
        .collect();
    let n = res.len() as i64;

    res.iter()
        .map(|r| {
            let dnf = truthy(r.get("dnf"));
            let mut row = Row {
                user_id: uid(r).expect("filtered above").to_string(),
                game: game.to_string(),
                key: String::new(),
                mode: String::new(),
                place: int_or(r.get("place"), n),
                players: n,
                value: None,
                extra: json!({}),
            };
            match game {
                "kart" => {
                    let laps: Vec<f64> = r
                        .get("laps")
                        .and_then(Value::as_array)
                        .map(|l| l.iter().filter_map(Value::as_f64).collect())
                        .unwrap_or_default();
                    // Cumulative lap stamps to per-lap splits: the first lap is
                    // measured from zero, each later one from the one before.
                    let splits: Vec<f64> = laps
                        .iter()
                        .scan(0.0, |prev, &l| {
                            let d = l - *prev;
                            *prev = l;
                            Some(d)
                        })
                        .collect();
                    row.key = clipped_str(data.get("track"), 40);
                    row.mode = if laps.is_empty() {
                        String::new()
                    } else {
                        format!("{} laps", laps.len())
                    };
                    row.value = if dnf { None } else { r.get("ms").and_then(Value::as_f64) }
                        .map(|ms| ms as i64);
                    if !dnf {
                        if let Some(best) = splits.iter().cloned().reduce(f64::min) {
                            row.extra = json!({"bestLap": best as i64});
                        }
                    }
                }
                "plat" => {
                    let coop = data.get("mode").and_then(Value::as_str) == Some("coop");
                    row.key = clipped_str(data.get("level"), 40);
                    row.mode = clipped_str(data.get("mode"), 16);
                    // A co-op run has no individual time to rank.
                    row.value = if dnf || coop { None } else { r.get("ms").and_then(Value::as_f64) }
                        .map(|ms| ms as i64);
                    row.extra = json!({"coins": int_or_zero(r.get("coins"))});
                }
                // The typing race: one "all" board, scored on time, with the
                // words-per-minute and accuracy kept beside it.
                "type" => {
                    row.key = "all".to_string();
                    row.mode = clipped_str(data.get("lang"), 16);
                    row.value = if dnf { None } else { r.get("ms").and_then(Value::as_f64) }
                        .map(|ms| ms as i64);
                    row.extra = json!({"wpm": float_or_zero(r.get("wpm")),
                                       "acc": float_or_zero(r.get("acc"))});
                }
                // Tower Defense (HQ 2.5): one board per "<map>-<diff>", scored on
                // waves cleared, where higher wins. Co-op places nobody, and the
                // NOT NULL column stores that as 0, which no win or podium counts.
                "td" => {
                    let waves = int_or_zero(data.get("waves"));
                    row.key = clipped_str(data.get("key"), 40);
                    row.mode = "coop".to_string();
                    row.place = if r.get("place").and_then(Value::as_i64).is_some() {
                        int_or(r.get("place"), n)
                    } else {
                        0
                    };
                    row.value = Some(waves);
                    row.extra = json!({"waves": waves, "win": truthy(data.get("win")),
                                       "diff": clipped_str(data.get("diff"), 16)});
                }
                // Bowling (HQ 2.5): one board per variant key, scored on pins.
                "bowl" => {
                    row.key = clipped_str(data.get("key"), 40);
                    row.mode = "bowl".to_string();
                    // A game someone dropped out of keeps its row but never a score.
                    row.value = if dnf { None } else { Some(int_or_zero(r.get("score"))) };
                    row.extra = json!({"strikes": int_or_zero(r.get("strikes")),
                                       "spares": int_or_zero(r.get("spares"))});
                }
                // Blaster: one "match" board, scored on kills, where higher wins.
                // HQ 2.5: a match on a user map keeps its own board, keyed by
                // the map's content key (done.map = "c-" + 12 hex).
                _ => {
                    let kills = int_or_zero(r.get("kills"));
                    row.key = custom_map_key(data.get("map")).unwrap_or("match").to_string();
                    row.value = Some(kills);
                    row.extra = json!({"kills": kills, "deaths": int_or_zero(r.get("deaths"))});
                }
            }
            row
        })
        .collect()
}

/// A user map's content key ("c-" + 12 lowercase hex), when `v` is one.
fn custom_map_key(v: Option<&Value>) -> Option<&str> {
    let s = v?.as_str()?;
    let hex = s.strip_prefix("c-")?;
    (hex.len() == 12 && hex.chars().all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))).then_some(s)
}

/// Golf's rows, from Python's `elif game == "golf"` (results.py:66-75).
///
/// Three things here that a rewrite gets wrong. The sort key is `(value, uid)`,
/// so a tie on strokes breaks on the user id and the row order is stable.
/// The place is DENSE: `place = i + 1` only when the value CHANGES, so two
/// players tied for first are both 1 and the next is 3 -- the same shape as a
/// real golf card, and not what `enumerate` alone gives. And `players` is the
/// number of RANKED entries, not the size of `totals`, so a malformed entry
/// lowers it.
fn golf_rows(data: &Value) -> Vec<Row> {
    let Some(totals) = data.get("totals").and_then(Value::as_object) else {
        return Vec::new();
    };
    // Python's `isinstance(v, (int, float))`. DIVERGENCE, declared: Python also
    // admits a JSON `true`/`false` there, because `bool` is a subclass of `int`
    // and `int(True)` is 1. The Arena mints `totals` itself and never puts a
    // bool in it, so this takes numbers only rather than carrying a branch that
    // can only be reached by hand-editing the payload.
    let mut ranked: Vec<(f64, &String)> = totals
        .iter()
        .filter_map(|(uid, v)| v.as_f64().map(|n| (n, uid)))
        .collect();
    // `sorted()` on (value, uid) tuples: value first, then the id.
    ranked.sort_by(|a, b| a.0.total_cmp(&b.0).then_with(|| a.1.cmp(b.1)));
    let n = ranked.len() as i64;
    let key = clipped_str(data.get("course"), 40);
    let mut place = 0i64;
    let mut prev: Option<f64> = None;
    ranked
        .iter()
        .enumerate()
        .map(|(i, (v, uid))| {
            if prev != Some(*v) {
                place = i as i64 + 1;
                prev = Some(*v);
            }
            Row {
                user_id: (*uid).clone(),
                game: "golf".to_string(),
                key: key.clone(),
                mode: String::new(),
                place,
                players: n,
                value: Some(*v as i64),
                extra: json!({}),
            }
        })
        .collect()
}

/// Writes finished games down, off the game's own tick.
///
/// `Default` is a recorder with no database: the game plays exactly the same
/// and nothing is stored, which is what the hubs' unit tests want.
#[derive(Clone, Default)]
pub struct Recorder {
    pool: Option<SqlitePool>,
}

impl Recorder {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool: Some(pool) }
    }

    /// Record without holding up the game's tick. Python spawns a task from
    /// `_flush` for the same reason: a results hiccup must not disturb a game
    /// in progress, so this never reports back and never fails.
    pub fn record_later(&self, game: &str, data: &Value) {
        let Some(pool) = self.pool.clone() else { return };
        let (game, data) = (game.to_string(), data.clone());
        tokio::spawn(async move {
            record(&pool, &game, &data).await;
        });
    }
}

/// Store a finished game's results. Never raises -- a failure here is logged
/// and swallowed. Returns how many rows the event produced, as Python does,
/// which is not the same as how many were inserted: a row for a user this
/// Arena has never seen is dropped but still counted.
pub async fn record(pool: &SqlitePool, game: &str, data: &Value) -> usize {
    let rows = rows_from_done(game, data);
    if rows.is_empty() {
        return 0;
    }
    let ph = vec!["?"; rows.len()].join(",");
    let sql = format!("SELECT id FROM users WHERE id IN ({ph})");
    let mut q = sqlx::query_scalar::<_, String>(&sql);
    for r in &rows {
        q = q.bind(r.user_id.clone());
    }
    let known: Vec<String> = match q.fetch_all(pool).await {
        Ok(ids) => ids,
        Err(e) => {
            tracing::warn!("game_results: user lookup failed: {e}");
            return 0;
        }
    };
    // One transaction for the whole game, as Python's single commit.
    let mut tx = match pool.begin().await {
        Ok(tx) => tx,
        Err(e) => {
            tracing::warn!("game_results: begin failed: {e}");
            return 0;
        }
    };
    for r in rows.iter().filter(|r| known.contains(&r.user_id)) {
        // `at` is left to the column's CURRENT_TIMESTAMP default, which is
        // where SQLAlchemy's server_default leaves it too -- so both ports
        // stamp the same UTC "YYYY-MM-DD HH:MM:SS" that `substr(at, 1, 10)`
        // reads back as a day. `extra` goes in compact; every reader parses it.
        let w = sqlx::query(
            "INSERT INTO game_results (user_id, game, \"key\", mode, place, players, value, extra)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        )
        .bind(&r.user_id)
        .bind(&r.game)
        .bind(&r.key)
        .bind(&r.mode)
        .bind(r.place)
        .bind(r.players)
        .bind(r.value)
        .bind(r.extra.to_string())
        .execute(&mut *tx)
        .await;
        if let Err(e) = w {
            tracing::warn!("game_results: insert failed: {e}");
            return 0;
        }
    }
    if let Err(e) = tx.commit().await {
        tracing::warn!("game_results: commit failed: {e}");
        return 0;
    }
    rows.len()
}

/// A finished game's own "done" event, as the hubs' `flush` sees it: the one
/// the server sends the lobby or the whole room, so it is recorded once.
/// `ev`, `g` and the payload all ride in the same object the client gets.
pub fn is_done(payload: &Value) -> Option<&str> {
    let ev = payload.get("ev").and_then(Value::as_str)?;
    let g = payload.get("g").and_then(Value::as_str)?;
    (ev == "done" && GAMES.contains(&g)).then_some(g)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Map;

    fn done(game: &str, data: Value) -> Vec<Row> {
        rows_from_done(game, &data)
    }

    #[test]
    fn a_td_done_records_coop_rows_with_no_placement() {
        let rows = done("td", json!({"key": "garden-normal", "map": "garden", "diff": "normal",
            "mode": "coop", "waves": 7, "win": false,
            "results": [{"user": {"userId": "a"}, "place": null, "waves": 7, "dnf": false},
                        {"user": {"userId": "b"}, "place": null, "waves": 7, "dnf": false}]}));
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].key, "garden-normal");
        assert_eq!(rows[0].mode, "coop");
        assert_eq!(rows[0].place, 0);
        assert_eq!(rows[0].players, 2);
        assert_eq!(rows[0].value, Some(7));
        assert_eq!(rows[0].extra, json!({"waves": 7, "win": false, "diff": "normal"}));
        assert!(is_done(&json!({"g": "td", "ev": "done"})).is_some());
    }

    #[test]
    fn a_bowl_done_keeps_its_places_and_pins() {
        let rows = done("bowl", json!({"key": "f10", "results": [
            {"user": {"userId": "a"}, "place": 1, "score": 187, "strikes": 5, "spares": 2}]}));
        assert_eq!(rows[0].key, "f10");
        assert_eq!(rows[0].mode, "bowl");
        assert_eq!(rows[0].place, 1);
        assert_eq!(rows[0].value, Some(187));
        assert_eq!(rows[0].extra, json!({"strikes": 5, "spares": 2}));
    }

    fn player(id: &str, extra: Value) -> Value {
        let mut m = Map::new();
        m.insert("user".into(), json!({"userId": id}));
        if let Value::Object(e) = extra {
            m.extend(e);
        }
        Value::Object(m)
    }

    #[test]
    fn kart_rows_carry_the_track_the_lap_count_and_the_best_split() {
        let rows = done(
            "kart",
            json!({"track": "dunes", "results": [
                player("u1", json!({"place": 1, "ms": 61234, "laps": [20000, 40500, 61234]})),
                player("u2", json!({"place": 2, "ms": 70000.9, "laps": [25000, 48000, 70000]})),
            ]}),
        );
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].key, "dunes");
        assert_eq!(rows[0].mode, "3 laps");
        assert_eq!(rows[0].place, 1);
        assert_eq!(rows[0].players, 2);
        assert_eq!(rows[0].value, Some(61234));
        // 20000, then 20500, then 20734 -- the first lap is measured from zero.
        assert_eq!(rows[0].extra, json!({"bestLap": 20000}));
        // A float millisecond truncates toward zero, as Python's int() does.
        assert_eq!(rows[1].value, Some(70000));
        assert_eq!(rows[1].extra, json!({"bestLap": 22000}));
    }

    #[test]
    fn a_dnf_has_no_time_and_no_best_lap() {
        let rows = done(
            "kart",
            json!({"track": "dunes", "results": [
                player("u1", json!({"place": 1, "ms": 61234, "laps": [61234]})),
                player("u2", json!({"dnf": true, "ms": 90000, "laps": [30000]})),
            ]}),
        );
        assert_eq!(rows[1].value, None);
        assert_eq!(rows[1].extra, json!({}));
        // Still placed and still counted among the players.
        assert_eq!(rows[1].place, 2);
        assert_eq!(rows[1].players, 2);
    }

    #[test]
    fn a_missing_place_becomes_last() {
        let rows = done(
            "kart",
            json!({"results": [player("u1", json!({})), player("u2", json!({"place": 0}))]}),
        );
        // `int(r.get("place") or n)`: absent and zero both fall back to n.
        assert_eq!((rows[0].place, rows[1].place), (2, 2));
        // No track and no laps: the empty string and no mode, not "None".
        assert_eq!(rows[0].key, "");
        assert_eq!(rows[0].mode, "");
    }

    #[test]
    fn a_result_without_a_user_id_is_not_a_row_and_not_a_player() {
        let rows = done(
            "kart",
            json!({"results": [
                player("u1", json!({"ms": 1000})),
                json!({"ms": 2000}),
                json!({"user": {"userId": 7}}),
            ]}),
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].players, 1);
    }

    #[test]
    fn a_coop_platformer_run_has_no_individual_time() {
        let solo = done(
            "plat",
            json!({"level": "caves", "mode": "rush",
                   "results": [player("u1", json!({"ms": 42000, "coins": 7}))]}),
        );
        assert_eq!(solo[0].key, "caves");
        assert_eq!(solo[0].mode, "rush");
        assert_eq!(solo[0].value, Some(42000));
        assert_eq!(solo[0].extra, json!({"coins": 7}));

        let coop = done(
            "plat",
            json!({"level": "caves", "mode": "coop",
                   "results": [player("u1", json!({"ms": 42000}))]}),
        );
        assert_eq!(coop[0].value, None);
        assert_eq!(coop[0].extra, json!({"coins": 0}));
    }

    #[test]
    fn blaster_scores_on_kills_and_keeps_deaths_beside_them() {
        let rows = done(
            "fps",
            json!({"results": [player("u1", json!({"place": 1, "kills": 9, "deaths": 3}))]}),
        );
        assert_eq!(rows[0].key, "match");
        assert_eq!(rows[0].mode, "");
        assert_eq!(rows[0].value, Some(9));
        assert_eq!(rows[0].extra, json!({"kills": 9, "deaths": 3}));
    }

    #[test]
    fn blaster_on_a_user_map_keeps_its_own_board() {
        let p = || json!({"results": [player("u1", json!({"place": 1, "kills": 4, "deaths": 1}))]});
        let mut custom = p();
        custom["map"] = json!("c-0123456789ab");
        assert_eq!(done("fps", custom)[0].key, "c-0123456789ab");
        // the built-in courtyard (and anything that is not a content key) is "match"
        for m in [json!("courtyard"), json!("c-XYZ"), json!("c-0123456789abc"), json!(7), Value::Null] {
            let mut d = p();
            d["map"] = m;
            assert_eq!(done("fps", d)[0].key, "match");
        }
    }

    #[test]
    fn long_keys_and_modes_are_clipped_where_the_columns_end() {
        let rows = done(
            "plat",
            json!({"level": "l".repeat(60), "mode": "m".repeat(30),
                   "results": [player("u1", json!({}))]}),
        );
        assert_eq!(rows[0].key.chars().count(), 40);
        assert_eq!(rows[0].mode.chars().count(), 16);
    }

    #[test]
    fn a_game_with_no_results_branch_writes_nothing() {
        // The pond, the race, the duel, the mines and the farm raise no `done`
        // event and have no branch in Python's rows_from_done either.
        for g in ["pond", "race", "duel", "mines", "farm", "hq", "party", ""] {
            assert!(done(g, json!({"results": [player("u1", json!({}))]})).is_empty());
        }
        assert!(done("kart", json!({})).is_empty());
        assert!(done("golf", json!({})).is_empty());
    }

    #[test]
    fn the_typing_race_keeps_the_time_the_speed_and_the_accuracy() {
        let rows = done(
            "type",
            json!({"lang": "rust", "results": [
                player("u1", json!({"place": 1, "ms": 30500, "wpm": 82.4, "acc": 0.97})),
                player("u2", json!({"dnf": true, "ms": 60000})),
            ]}),
        );
        assert_eq!(rows[0].key, "all");
        assert_eq!(rows[0].mode, "rust");
        assert_eq!(rows[0].value, Some(30500));
        assert_eq!(rows[0].extra, json!({"wpm": 82.4, "acc": 0.97}));
        assert_eq!((rows[0].place, rows[0].players), (1, 2));
        // A player who gave up has no time, and no speed to show.
        assert_eq!(rows[1].value, None);
        assert_eq!(rows[1].extra, json!({"wpm": 0.0, "acc": 0.0}));
    }

    #[test]
    fn golf_ranks_from_totals_with_dense_places() {
        let rows = done(
            "golf",
            json!({"course": "seaside", "totals": {"u3": 41, "u1": 38, "u2": 38, "u4": 45}}),
        );
        // Sorted by strokes, then by id: the two 38s are BOTH first, and the
        // next player is third, not second.
        assert_eq!(rows.iter().map(|r| r.user_id.as_str()).collect::<Vec<_>>(),
                   ["u1", "u2", "u3", "u4"]);
        assert_eq!(rows.iter().map(|r| r.place).collect::<Vec<_>>(), [1, 1, 3, 4]);
        assert_eq!(rows[0].key, "seaside");
        assert_eq!(rows[0].mode, "");
        assert_eq!(rows[0].value, Some(38));
        assert_eq!(rows[0].extra, json!({}));
        assert!(rows.iter().all(|r| r.players == 4));
    }

    #[test]
    fn golf_counts_only_the_entries_it_could_rank() {
        // `players` is the number of RANKED entries, not the size of `totals`:
        // a non-numeric score is dropped and lowers the field.
        let rows = done("golf", json!({"totals": {"u1": 30, "u2": "nope", "u3": 31.7}}));
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|r| r.players == 2));
        // int() truncates toward zero, as Python's int(31.7) does.
        assert_eq!(rows[1].value, Some(31));
        // No course named is the empty string, not "None".
        assert_eq!(rows[0].key, "");
    }

    #[test]
    fn only_a_done_event_for_a_known_game_records() {
        assert_eq!(is_done(&json!({"ev": "done", "g": "kart"})), Some("kart"));
        assert_eq!(is_done(&json!({"ev": "snap", "g": "kart"})), None);
        assert_eq!(is_done(&json!({"ev": "done", "g": "golf"})), Some("golf"));
        assert_eq!(is_done(&json!({"ev": "done", "g": "type"})), Some("type"));
        // The pond and the duel end, but not with a `done` -- nothing to record.
        assert_eq!(is_done(&json!({"ev": "done", "g": "pond"})), None);
        assert_eq!(is_done(&json!({"ev": "done"})), None);
    }

    #[test]
    fn a_recorder_with_no_database_is_a_no_op() {
        // The hubs' unit tests build one of these; it must not panic.
        Recorder::default().record_later("kart", &json!({"results": []}));
    }

    #[tokio::test]
    async fn record_writes_one_row_per_known_player() {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        crate::db::migrate(&pool).await.unwrap();
        for (i, id) in ["u1", "u2"].iter().enumerate() {
            sqlx::query("INSERT INTO users (id, github_id, handle, display_name, avatar_url,
                         trainer_name, is_active, created_at)
                         VALUES (?1, ?2, ?3, '', '', '', 1, datetime('now'))")
                .bind(id)
                .bind(900 + i as i64)
                .bind(id)
                .execute(&pool)
                .await
                .unwrap();
        }
        let data = json!({"track": "dunes", "results": [
            player("u1", json!({"place": 1, "ms": 1000, "laps": [1000]})),
            player("u2", json!({"place": 2, "dnf": true})),
            player("ghost", json!({"place": 3})),
        ]});
        // Three rows in the event, three counted, two stored: the Arena has
        // never seen "ghost", and the foreign key would refuse it anyway.
        assert_eq!(record(&pool, "kart", &data).await, 3);
        let rows: Vec<(String, i64, i64, Option<i64>, String, String)> = sqlx::query_as(
            "SELECT user_id, place, players, value, extra, substr(at, 1, 4)
             FROM game_results ORDER BY user_id",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].0, "u1");
        assert_eq!((rows[0].1, rows[0].2, rows[0].3), (1, 3, Some(1000)));
        assert_eq!(rows[0].4, r#"{"bestLap":1000}"#);
        // `at` defaulted to CURRENT_TIMESTAMP, so progress.rs's substr(at,1,10)
        // has a day to read.
        assert!(rows[0].5.starts_with("20"), "at was not stamped: {:?}", rows[0].5);
        assert_eq!(rows[1].0, "u2");
        assert_eq!(rows[1].3, None);

        // A game with nobody in it is not a write.
        assert_eq!(record(&pool, "kart", &json!({"results": []})).await, 0);
    }
}
