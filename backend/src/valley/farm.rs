//! The shared farm. The only Valley game that touches the database.
//!
//! A port of `backend/app/valley.py`'s farm half: `CROPS` (:102-106),
//! `FARM_PLOTS`/`FARM_SEEDS_PER_DAY` (:107-108), `_farm_load` (:900),
//! `_water` (:915), `_farm_view` (:924) and `farm_op` (:942). `_today` and
//! `season_of` are already in the shared layer as [`super::today`] and
//! [`super::season_of`], reached here through `hub.today()`.
//!
//! The farm keeps NO [`super::RoomValley`] state -- Python's `RoomValley` has no
//! farm attribute either. One JSON document per room lives in `room_farms.data`
//! and the water is read out of `daily_stats` on every view, never stored. So
//! there is no `Farm` struct, no `joined`, no `op` and no `dropped`: the shared
//! layer routes the join snapshot (as `{"op": "view"}`) and every farm op to the
//! one entry point [`run`] through [`super::After::Farm`], with the state guard
//! dropped, which is the only reason this file may await.
//!
//! AUDIENCE. `note` and `farm` go to [`super::To::All`] -- the WHOLE ROOM,
//! including sockets that never joined the farm lobby. Only the four errors and
//! the `harvest` receipt go to the one socket. Pinned by
//! `the_farm_view_goes_to_the_whole_room`.
//!
//! The shared divergences from Python are listed once at the top of
//! `backend/src/valley.rs`; this file's own are marked DIVERGENCE below.

use super::{py_round_to, season_of, Out, Seq, ValleyHub};
use crate::rooms::Member;
use chrono::NaiveDate;
use rand::Rng;
use serde_json::{json, Value};
use sqlx::{Row, SqlitePool};

/// The game key on the wire.
pub const GAME: &str = "farm";

/// Python's `CROPS` (valley.py:102): `id -> (hours, water per gardener,
/// season)`, IN PYTHON'S DICT ORDER. The order reaches the wire: the seed box
/// filters this table for the season and draws from the result, so the key order
/// of the `seeds` object a page receives descends from it, and
/// `test_farm_is_watered_by_published_prompts_and_persists` plants
/// `next(iter(farm["seeds"]))`.
pub const CROPS: [(&str, i64, i64, &str); 9] = [
    ("radish", 4, 6, "spring"),
    ("pea", 6, 8, "spring"),
    ("tulip", 8, 10, "spring"),
    ("tomato", 6, 10, "summer"),
    ("melon", 12, 16, "summer"),
    ("corn", 10, 12, "summer"),
    ("pumpkin", 12, 16, "autumn"),
    ("grape", 8, 12, "autumn"),
    ("kale", 6, 8, "winter"),
];

/// Python's `FARM_PLOTS` (valley.py:107): the plot array is always this long,
/// truncated or null-padded on load.
pub const FARM_PLOTS: usize = 9;

/// Python's `FARM_SEEDS_PER_DAY` (valley.py:108): one `seeds` op draws this many
/// seeds, with replacement, so six draws can land on fewer than six crops.
pub const FARM_SEEDS_PER_DAY: usize = 6;

/// Python's `CROPS[id]`, or None for `crop not in CROPS`.
fn crop_of(id: &str) -> Option<(i64, i64, &'static str)> {
    CROPS.iter().find(|(c, ..)| *c == id).map(|(_, h, n, s)| (*h, *n, *s))
}

/// Python's `[c for c, v in CROPS.items() if v[2] == season_of(_today())]`, in
/// [`CROPS`] order. Never empty for any of the four seasons -- pinned by
/// `every_season_has_at_least_one_crop_to_draw`, which is what makes the
/// unguarded index in the `seeds` arm safe (Python's `rng.choice([])` raises).
fn season_crops(season: &str) -> Vec<&'static str> {
    CROPS.iter().filter(|(.., s)| *s == season).map(|(c, ..)| *c).collect()
}

// ------------------------------------------------------------ the document --

/// `room_farms.data`, normalised the way `_farm_load` normalises it
/// (valley.py:900-913).
// No PartialEq: `Seq` does not derive it and valley.rs is not this file's to
// change. The tests compare documents through `to_json`, which is total.
#[derive(Debug, Default)]
struct Data {
    /// Exactly [`FARM_PLOTS`] entries, `Value::Null` for an empty plot. Kept as
    /// raw JSON rather than a parsed struct so a round trip through the column
    /// preserves whatever a plot object holds, as Python's `dict` does.
    plots: Vec<Value>,
    /// crop id -> count. A [`Seq`] because the insertion order reaches the wire
    /// (see [`CROPS`]); a `HashMap` would randomise it and a `BTreeMap` would
    /// sort it.
    seeds: Seq<i64>,
    /// The ISO day the seed box was last opened. "" for never.
    seed_day: String,
    /// Everyone who has ever sent a farm op in this room, in first-touch order.
    /// Both the water divisor and the `IN (..)` list of [`water`].
    gardeners: Vec<String>,
}

impl Data {
    /// Python's `_farm_load` tail (valley.py:906-912): truncate/pad `plots` to
    /// [`FARM_PLOTS`], then `setdefault` the other three.
    ///
    /// DIVERGENCE: Python's `dict(row.data or {})` raises on a stored value that
    /// is neither falsey nor a mapping (`dict("x")` is a ValueError,
    /// `dict(5)` a TypeError), and `CROPS[p["crop"]]` raises on a plot naming a
    /// crop the catalog dropped. Nothing but this code writes the column, so
    /// neither is reachable from the wire; where Python would 500 we degrade the
    /// offending part to its default -- an unreadable document to an empty farm,
    /// an unreadable plot to an empty plot -- because a half-parsed garden is
    /// better than a room that can never open its farm again.
    fn parse(raw: &Value) -> Self {
        let get = |k: &str| raw.get(k);
        let mut plots: Vec<Value> = match get("plots") {
            Some(Value::Array(a)) => a.iter().take(FARM_PLOTS).cloned().collect(),
            _ => Vec::new(),
        };
        plots.resize(FARM_PLOTS, Value::Null);
        let mut seeds = Seq::new();
        if let Some(Value::Object(o)) = get("seeds") {
            for (k, v) in o {
                seeds.set(k, v.as_i64().unwrap_or(0));
            }
        }
        Self {
            plots,
            seeds,
            seed_day: get("seedDay").and_then(Value::as_str).unwrap_or("").to_string(),
            gardeners: match get("gardeners") {
                Some(Value::Array(a)) => {
                    a.iter().filter_map(Value::as_str).map(str::to_string).collect()
                }
                _ => Vec::new(),
            },
        }
    }

    /// What goes back into the column. The key order is the one `_farm_load`
    /// leaves behind: the fresh row's literal is
    /// `{"plots", "seeds", "seedDay", "gardeners"}` (valley.py:903) and every
    /// later write re-reads and re-writes that same dict, so the order is
    /// stable. Nothing reads it; it is here so a stored document is diffable.
    fn to_json(&self) -> Value {
        json!({"plots": self.plots, "seeds": self.seeds.to_object(|n| json!(n)),
               "seedDay": self.seed_day, "gardeners": self.gardeners})
    }
}

/// One planted plot, as `_farm_view` reads it (valley.py:931-937). None for an
/// empty plot (Python's falsey `if not p`) and for a malformed one (see the
/// DIVERGENCE on [`Data::parse`]).
struct Plot {
    crop: String,
    /// `wall()` when it was planted, UTC unix seconds.
    at: f64,
    /// The day it was planted: the floor of the water window, not a timestamp.
    day: NaiveDate,
    /// `p.get("by", "")` -- the planter's display name, absent on a plot written
    /// before the field existed.
    by: String,
}

fn plot_of(v: &Value) -> Option<Plot> {
    // Python's `if not p: continue` is a truthiness test, so null, 0, "", [] and
    // an EMPTY OBJECT are all empty plots.
    if !is_truthy(v) {
        return None;
    }
    let crop = v.get("crop").and_then(Value::as_str)?;
    crop_of(crop)?; // a crop the catalog no longer has is an unreadable plot
    let at = v.get("at").and_then(Value::as_f64).filter(|f| f.is_finite())?;
    let day = NaiveDate::parse_from_str(v.get("day")?.as_str()?, "%Y-%m-%d").ok()?;
    Some(Plot {
        crop: crop.to_string(),
        at,
        day,
        by: v.get("by").and_then(Value::as_str).unwrap_or("").to_string(),
    })
}

/// Python truthiness, for the `if not p` of `_farm_view` and the
/// `data["plots"][i]` of the plant arm. Not [`super::is_true`], which is an
/// identity check against `True`.
fn is_truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().map(|f| f != 0.0).unwrap_or(true),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// `msg["plot"]` as Python's plant and harvest arms read it:
/// `isinstance(i, int) and 0 <= i < FARM_PLOTS` (valley.py:963, :978).
///
/// THE BOOL ARM IS DELIBERATE, not a leftover. `bool` is a subclass of `int` in
/// Python, so `isinstance(True, int)` is True and `0 <= True < 9` holds: a frame
/// carrying `"plot": true` plants in plot ONE and `"plot": false` in plot ZERO.
/// A float is NOT an int, so `0.0` is refused where `0` is accepted -- which is
/// also why this cannot be [`super::int_exact`], whose whole point is that
/// `10.0` passes. Pinned by `a_json_true_is_plot_one_because_a_python_bool_is_an_int`.
fn plot_index(v: Option<&Value>) -> Option<usize> {
    let i = match v {
        Some(Value::Bool(b)) => i64::from(*b),
        // serde_json keeps ints and floats apart, so `as_i64` is None for 0.0
        // and for an integer literal too wide for i64 (which Python would hold
        // exactly and then reject on range -- same answer).
        Some(Value::Number(n)) => n.as_i64()?,
        _ => return None,
    };
    (0..FARM_PLOTS as i64).contains(&i).then_some(i as usize)
}

// ------------------------------------------------------------------- the db --

/// Python's `_farm_load` (valley.py:900): the room's document, or a fresh
/// default when the row does not exist yet. Python `db.add`s the new row so the
/// commit inserts it; [`store`] upserts unconditionally, which is the same.
///
/// `pool` is None in the unit tests that have no database. Then every room's
/// farm is a fresh default that nothing persists -- the op still runs and the
/// view still goes out, rather than panicking.
async fn load(pool: Option<&SqlitePool>, room_id: &str) -> Data {
    let Some(pool) = pool else { return Data::parse(&Value::Null) };
    let row = sqlx::query("SELECT data FROM room_farms WHERE room_id = ?1")
        .bind(room_id)
        .fetch_optional(pool)
        .await;
    match row {
        Ok(Some(r)) => {
            let raw: String = r.try_get("data").unwrap_or_default();
            Data::parse(&serde_json::from_str(&raw).unwrap_or(Value::Null))
        }
        Ok(None) => Data::parse(&Value::Null),
        Err(e) => {
            // DIVERGENCE: Python lets the error out of the handler (a closed
            // socket). An empty garden that is not written back is quieter and
            // loses nothing, because `store` fails the same way.
            tracing::warn!("room_farms: load failed: {e}");
            Data::parse(&Value::Null)
        }
    }
}

/// Python's `row.data = data; await db.commit()` (valley.py:987-988), which runs
/// on EVERY farm op -- including an unrecognised one and one that only errored,
/// because the gardener list may have grown. `updated_at` is
/// `datetime('now')`, matching the model's `onupdate=func.now()`.
async fn store(pool: Option<&SqlitePool>, room_id: &str, data: &Data) {
    let Some(pool) = pool else { return };
    let q = sqlx::query(
        "INSERT INTO room_farms (room_id, data, updated_at)
         VALUES (?1, ?2, datetime('now'))
         ON CONFLICT(room_id) DO UPDATE SET data = excluded.data,
                                            updated_at = datetime('now')",
    )
    .bind(room_id)
    .bind(data.to_json().to_string())
    .execute(pool)
    .await;
    if let Err(e) = q {
        tracing::warn!("room_farms: store failed: {e}");
    }
}

/// Python's `_water` (valley.py:915): "prompts the room's gardeners published
/// since the plot was planted" -- `sum(prompts)` over `daily_stats` for those
/// users with `stat_date >= since`.
///
/// `since` is a DAY, the plot's planting day, so a prompt published earlier on
/// the same day counts in full; the comparison is on `stat_date`, which this
/// Arena stores as an ISO "YYYY-MM-DD" string (service.rs:235), so `>=` is the
/// lexicographic order and that is also the date order.
async fn water(pool: Option<&SqlitePool>, gardeners: &[String], since: NaiveDate) -> i64 {
    if gardeners.is_empty() {
        return 0; // Python's `if not gardeners: return 0`
    }
    let Some(pool) = pool else { return 0 };
    let holes =
        (1..=gardeners.len()).map(|i| format!("?{i}")).collect::<Vec<_>>().join(",");
    let sql = format!(
        "SELECT COALESCE(SUM(prompts), 0) AS w FROM daily_stats
         WHERE user_id IN ({holes}) AND stat_date >= ?{}",
        gardeners.len() + 1
    );
    let mut q = sqlx::query(&sql);
    for g in gardeners {
        q = q.bind(g);
    }
    match q.bind(since.to_string()).fetch_one(pool).await {
        Ok(r) => r.try_get::<i64, _>("w").unwrap_or(0),
        Err(e) => {
            tracing::warn!("daily_stats: water read failed: {e}");
            0
        }
    }
}

// ----------------------------------------------------------------- the view --

/// Python's `_farm_view` (valley.py:924). The key order is Python's and the page
/// reads it: `plots`, `seeds`, `seedBoxOpen`, `gardeners`, `season`, and per
/// plot `crop`, `by`, `water`, `need`, `hoursLeft`, `ripe`.
///
/// `wall` and `today` are hoisted once per message where Python calls `wall()`
/// twice per plot and `_today()` twice per view (shared divergence 1).
async fn view(
    pool: Option<&SqlitePool>, data: &Data, wall: f64, today: NaiveDate,
) -> Value {
    let mut plots = Vec::with_capacity(FARM_PLOTS);
    for raw in &data.plots {
        let Some(p) = plot_of(raw) else {
            plots.push(Value::Null);
            continue;
        };
        let (hours, per, _) = crop_of(&p.crop).expect("plot_of rejects an unknown crop");
        // `need *= max(1, len(gardeners))`: a bigger room needs more prompts.
        let need = per * data.gardeners.len().max(1) as i64;
        let w = water(pool, &data.gardeners, p.day).await;
        let elapsed = (wall - p.at) / 3600.0;
        let grown = elapsed >= hours as f64;
        // `max(0.0, round(hours - elapsed, 1))`, in that order: the rounding is
        // Python's banker's decimal rounding (super::py_round_to), and the max
        // is written the way Python's two-argument `max` resolves a tie, so a
        // rounded -0.0 comes out as 0.0 and not as "-0.0" on the wire.
        let left = py_round_to(hours as f64 - elapsed, 1);
        let hours_left = if left > 0.0 { left } else { 0.0 };
        plots.push(json!({"crop": p.crop, "by": p.by, "water": w.min(need), "need": need,
                          "hoursLeft": hours_left, "ripe": grown && w >= need}));
    }
    json!({"plots": plots, "seeds": data.seeds.to_object(|n| json!(n)),
           // A string compare, as Python's is: a seedDay that is not today's
           // ISO date -- including "" and a malformed one -- opens the box.
           "seedBoxOpen": data.seed_day != today.to_string(),
           "gardeners": data.gardeners.len(), "season": season_of(today)})
}

// ------------------------------------------------------------------- the op --

/// Python's `await farm_op(room_id, member, msg, out, rng)` (valley.py:942).
/// Runs with the state guard DROPPED, so it may await: it keeps no
/// [`super::RoomValley`] state and does the whole op -- load, mutate, commit,
/// view -- against `hub.pool()`.
///
/// Every path ends in the same two statements Python ends on: the document is
/// written back and `farm` goes to the whole room. That holds for an
/// unrecognised op too (the join snapshot arrives as `{"op": "view"}`, which
/// matches no arm), because merely touching the farm makes you a gardener and
/// that has to be stored.
///
/// DIVERGENCE: Python hands `farm_op` the rng the dispatcher made for this
/// message; [`super::After::Farm`] carries only the message, so `run` asks
/// `hub.rng()` for its own. Equivalent: `hub.rng()` is a factory of fresh seeded
/// generators (valley.rs's [`super::Dice`]) and nothing in Python's `handle`
/// draws from that rng before the farm arm, so both sides draw from an undrawn
/// generator. `wall()` and `_today()` are likewise read here rather than passed
/// in, which is where Python reads them too.
pub async fn run(
    hub: &ValleyHub, room_id: &str, conn: u64, member: &Member, msg: &Value, out: &mut Out,
) {
    let op = msg.get("op").and_then(Value::as_str).unwrap_or("");
    let pool = hub.pool();
    let wall = hub.wall_now();
    let today = hub.today();
    let iso = today.to_string();

    let mut data = load(pool, room_id).await;
    // `if m.user_id not in data["gardeners"]`: first touch makes you a gardener
    // of this room's farm, for good, whatever the op was.
    if !data.gardeners.iter().any(|g| g == &member.user_id) {
        data.gardeners.push(member.user_id.clone());
    }

    if op == "seeds" {
        if data.seed_day == iso {
            out.err(conn, "the seed box is empty until tomorrow");
        } else {
            let crops = season_crops(season_of(today));
            let mut rng = hub.rng();
            for _ in 0..FARM_SEEDS_PER_DAY {
                // `rng.choice(crops)`: uniform over the season's crops, WITH
                // replacement. Shared divergence 2 -- CPython's Mersenne
                // Twister cannot be reproduced here, so the distribution ports
                // and the seeded sequence does not; the tests assert that six
                // seeds of this season's crops arrived, not which ones.
                let c = crops[rng.gen_range(0..crops.len())];
                // `seeds[c] = seeds.get(c, 0) + 1`: a crop already in the box
                // keeps its position, a new one is appended -- Seq::set, not
                // Seq::bump.
                let n = data.seeds.get(c).copied().unwrap_or(0);
                data.seeds.set(c, n + 1);
            }
            data.seed_day = iso.clone();
            out.all("note", json!({"text": format!("{} opened the seed box", member.display_name)}));
        }
    } else if op == "plant" {
        let i = plot_index(msg.get("plot"));
        let crop = msg.get("crop").and_then(Value::as_str).unwrap_or("");
        // Python's one condition, in its order: a bad index, a plot that is
        // already taken, or a crop outside CROPS all give the same message.
        let taken = i.map(|i| is_truthy(&data.plots[i])).unwrap_or(false);
        match i {
            None => out.err(conn, "can't plant there"),
            Some(_) if taken || crop_of(crop).is_none() => out.err(conn, "can't plant there"),
            Some(_) if data.seeds.get(crop).copied().unwrap_or(0) < 1 => {
                out.err(conn, "no seeds of that kind")
            }
            Some(i) => {
                let n = data.seeds.get(crop).copied().unwrap_or(0) - 1;
                // `if not seeds[crop]: del seeds[crop]` -- a spent crop leaves
                // the box entirely rather than sitting there as a 0.
                if n == 0 {
                    data.seeds.remove(crop);
                } else {
                    data.seeds.set(crop, n);
                }
                // The stored plot's four keys, in Python's order (valley.py:973).
                // `by` is the raw display name: not clipped, not the handle.
                data.plots[i] = json!({"crop": crop, "at": wall, "day": iso,
                                       "by": member.display_name});
            }
        }
    } else if op == "harvest" {
        // The view is computed BEFORE the check, as Python does, because
        // ripeness is the check.
        let v = view(pool, &data, wall, today).await;
        let i = plot_index(msg.get("plot"));
        let ripe = i.map(|i| v["plots"][i]["ripe"] == json!(true)).unwrap_or(false);
        match i.filter(|_| ripe) {
            None => out.err(conn, "not ripe yet"),
            Some(i) => {
                let crop = data.plots[i]["crop"].as_str().unwrap_or("").to_string();
                data.plots[i] = Value::Null;
                // The receipt is for the harvester alone; the note is the room's.
                out.to(conn, "harvest", json!({"crop": crop, "n": 2}));
                out.all("note",
                        json!({"text": format!("{} harvested the {}", member.display_name, crop)}));
            }
        }
    }

    store(pool, room_id, &data).await;
    // Recomputed after the mutation, as Python's second `_farm_view` call is.
    out.all("farm", json!({"farm": view(pool, &data, wall, today).await}));
}

#[cfg(test)]
mod tests {
    use super::super::testkit::*;
    use super::*;

    /// The test fixture's wall clock, 1_790_000_000 = 2026-09-21T14:13:20Z, is
    /// in AUTUMN, so the seed box deals from pumpkin and grape.
    const AUTUMN: [&str; 2] = ["pumpkin", "grape"];

    /// `data["seeds"]` as a plain map: what a test looks a count up in.
    fn seed_counts(view: &Value) -> serde_json::Map<String, Value> {
        view["seeds"].as_object().cloned().unwrap_or_default()
    }

    /// The seeds the box dealt, totalled.
    fn seed_total(view: &Value) -> i64 {
        seed_counts(view).values().map(|v| v.as_i64().unwrap()).sum()
    }

    /// `daily_stats` has a foreign key to `users`, and `env_db`'s pool has
    /// foreign keys on, so water needs a real user row behind it.
    async fn user(pool: &SqlitePool, id: &str, github_id: i64) {
        sqlx::query(
            "INSERT INTO users (id, github_id, handle, display_name, avatar_url,
             trainer_name, is_active, created_at)
             VALUES (?1, ?2, ?3, '', '', '', 1, datetime('now'))",
        )
        .bind(id)
        .bind(github_id)
        .bind(id)
        .execute(pool)
        .await
        .unwrap();
    }

    /// One `daily_stats` row: `prompts` published on `day`, which is what the
    /// farm reads as water.
    async fn prompts(pool: &SqlitePool, uid: &str, day: NaiveDate, n: i64) {
        sqlx::query(
            "INSERT INTO daily_stats (id, user_id, stat_date, prompts, tools, artifacts,
             replies, tokens_input, tokens_output, tokens_cache_read, tokens_cache_creation,
             updated_at)
             VALUES (?1, ?2, ?3, ?4, 0, 0, 0, 0, 0, 0, 0, datetime('now'))",
        )
        .bind(uuid::Uuid::new_v4().to_string())
        .bind(uid)
        .bind(day.to_string())
        .bind(n)
        .execute(pool)
        .await
        .unwrap();
    }

    // ----------------------------------------------------------- the catalog --

    #[test]
    fn the_crop_catalog_is_pythons_in_pythons_order() {
        // valley.py:102-108, byte for byte.
        assert_eq!(
            CROPS,
            [("radish", 4, 6, "spring"), ("pea", 6, 8, "spring"), ("tulip", 8, 10, "spring"),
             ("tomato", 6, 10, "summer"), ("melon", 12, 16, "summer"), ("corn", 10, 12, "summer"),
             ("pumpkin", 12, 16, "autumn"), ("grape", 8, 12, "autumn"), ("kale", 6, 8, "winter")]
        );
        assert_eq!(FARM_PLOTS, 9);
        assert_eq!(FARM_SEEDS_PER_DAY, 6);
        assert_eq!(crop_of("melon"), Some((12, 16, "summer")));
        assert_eq!(crop_of("durian"), None);
    }

    #[test]
    fn every_season_has_at_least_one_crop_to_draw() {
        // What makes the unguarded `crops[rng.gen_range(..)]` safe: Python's
        // `rng.choice([])` would raise IndexError and the handler would die.
        assert_eq!(season_crops("spring"), ["radish", "pea", "tulip"]);
        assert_eq!(season_crops("summer"), ["tomato", "melon", "corn"]);
        assert_eq!(season_crops("autumn"), AUTUMN);
        assert_eq!(season_crops("winter"), ["kale"]);
    }

    // --------------------------------------------------------- the document --

    #[test]
    fn a_loaded_document_is_always_nine_plots_long() {
        // Truncated ...
        let long = json!({"plots": [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11]});
        assert_eq!(Data::parse(&long).plots.len(), FARM_PLOTS);
        // ... and null-padded.
        let short = Data::parse(&json!({"plots": [Value::Null]}));
        assert_eq!(short.plots, vec![Value::Null; FARM_PLOTS]);
        assert_eq!(short.seed_day, "");
        assert!(short.seeds.is_empty() && short.gardeners.is_empty());
    }

    #[test]
    fn a_malformed_document_degrades_to_an_empty_farm() {
        // Where Python's `dict(row.data or {})` raises. See the DIVERGENCE on
        // Data::parse.
        let empty = Data::parse(&Value::Null).to_json();
        for raw in [json!(null), json!({}), json!([]), json!(5), json!("x"), json!(false)] {
            assert_eq!(Data::parse(&raw).to_json(), empty, "{raw}");
        }
        // Each field degrades on its own: a non-array plots, a non-object seeds,
        // a non-string seedDay and a gardener that is not a string.
        let junk = Data::parse(&json!({"plots": "nine", "seeds": 7, "seedDay": 3,
                                       "gardeners": ["a", 5, null, "b"]}));
        assert_eq!(junk.plots, vec![Value::Null; FARM_PLOTS]);
        assert!(junk.seeds.is_empty());
        assert_eq!(junk.seed_day, "");
        assert_eq!(junk.gardeners, ["a", "b"]);
    }

    #[test]
    fn a_stored_document_keeps_pythons_key_order() {
        let mut d = Data::parse(&Value::Null);
        d.seeds.set("grape", 2);
        d.gardeners.push("u1".into());
        d.seed_day = "2026-09-21".into();
        assert_eq!(
            d.to_json().to_string(),
            r#"{"plots":[null,null,null,null,null,null,null,null,null],"seeds":{"grape":2},"seedDay":"2026-09-21","gardeners":["u1"]}"#
        );
    }

    #[test]
    fn an_unreadable_plot_reads_as_an_empty_one() {
        assert!(plot_of(&Value::Null).is_none());
        assert!(plot_of(&json!({})).is_none()); // an empty dict is falsey in Python
        let ok = json!({"crop": "grape", "at": 1.0, "day": "2026-09-21", "by": "ash"});
        assert!(plot_of(&ok).is_some());
        for bad in [json!({"crop": "durian", "at": 1.0, "day": "2026-09-21"}),
                    json!({"crop": "grape", "day": "2026-09-21"}),
                    json!({"crop": "grape", "at": 1.0, "day": "21/09/2026"}),
                    json!({"crop": "grape", "at": "soon", "day": "2026-09-21"})] {
            assert!(plot_of(&bad).is_none(), "{bad}");
        }
        // `by` is optional, as Python's `p.get("by", "")` has it.
        let no_by = plot_of(&json!({"crop": "grape", "at": 1.0, "day": "2026-09-21"})).unwrap();
        assert_eq!(no_by.by, "");
    }

    #[test]
    fn a_plot_index_is_an_int_in_range_and_a_bool_counts_as_one() {
        assert_eq!(plot_index(Some(&json!(0))), Some(0));
        assert_eq!(plot_index(Some(&json!(8))), Some(8));
        assert_eq!(plot_index(Some(&json!(9))), None);
        assert_eq!(plot_index(Some(&json!(-1))), None);
        assert_eq!(plot_index(Some(&json!(0.0))), None); // a float is not an int
        assert_eq!(plot_index(Some(&json!("0"))), None);
        assert_eq!(plot_index(None), None);
        // Python: isinstance(True, int) is True, and True == 1.
        assert_eq!(plot_index(Some(&json!(true))), Some(1));
        assert_eq!(plot_index(Some(&json!(false))), Some(0));
    }

    // ------------------------------------------------------------ the rules --

    #[tokio::test]
    async fn joining_the_farm_sends_a_fresh_garden_for_one_gardener() {
        let e = env_db(8, 1).await;
        let (m, mut rx) = e.connect("farmroom", 1, "u1").await;
        e.send("farmroom", 1, &m, GAME, "join", json!({})).await;
        let farm = until(&mut rx, GAME, "farm").await["farm"].clone();
        // test_farm_is_watered_by_published_prompts_and_persists' first two
        // assertions, plus the shape the page reads.
        assert_eq!(farm["seedBoxOpen"], json!(true));
        assert_eq!(farm["gardeners"], json!(1));
        assert_eq!(farm["season"], json!("autumn")); // 1_790_000_000 is 2026-09-21
        assert_eq!(farm["plots"], json!([null, null, null, null, null, null, null, null, null]));
        assert_eq!(farm["seeds"], json!({}));
        // The key order the page reads.
        assert_eq!(farm.as_object().unwrap().keys().collect::<Vec<_>>(),
                   ["plots", "seeds", "seedBoxOpen", "gardeners", "season"]);
    }

    #[tokio::test]
    async fn the_join_snapshot_creates_the_row_even_though_nothing_was_planted() {
        // The join tail is `{"op": "view"}`, which matches no arm -- and still
        // commits, because the gardener list grew (valley.py:947).
        let e = env_db(8, 1).await;
        let (m, mut rx) = e.connect("farmroom", 1, "u1").await;
        e.send("farmroom", 1, &m, GAME, "join", json!({})).await;
        until(&mut rx, GAME, "farm").await;
        let raw: String = sqlx::query("SELECT data FROM room_farms WHERE room_id = ?1")
            .bind("farmroom")
            .fetch_one(e.hub.pool().unwrap())
            .await
            .unwrap()
            .get("data");
        let stored = Data::parse(&serde_json::from_str::<Value>(&raw).unwrap());
        assert_eq!(stored.gardeners, ["u1"]);
    }

    #[tokio::test]
    async fn the_seed_box_deals_six_seeds_of_the_season_and_then_closes() {
        let e = env_db(8, 1).await;
        let (m, mut rx) = e.connect("farmroom", 1, "u1").await;
        e.send("farmroom", 1, &m, GAME, "join", json!({})).await;
        until(&mut rx, GAME, "farm").await;
        e.send("farmroom", 1, &m, GAME, "seeds", json!({})).await;
        assert_eq!(until(&mut rx, GAME, "note").await["text"], json!("u1 opened the seed box"));
        let farm = until(&mut rx, GAME, "farm").await["farm"].clone();
        let seeds = seed_counts(&farm);
        // The distribution, not the sequence: shared divergence 2.
        assert_eq!(seed_total(&farm), FARM_SEEDS_PER_DAY as i64);
        assert!(seeds.keys().all(|c| AUTUMN.contains(&c.as_str())), "{seeds:?}");
        assert_eq!(farm["seedBoxOpen"], json!(false));
        // Twice in one day is refused, and the box still reads closed.
        e.send("farmroom", 1, &m, GAME, "seeds", json!({})).await;
        assert_eq!(until(&mut rx, GAME, "error").await["error"],
                   json!("the seed box is empty until tomorrow"));
        assert_eq!(until(&mut rx, GAME, "farm").await["farm"]["seedBoxOpen"], json!(false));
    }

    #[tokio::test]
    async fn the_seed_box_reopens_at_utc_midnight_and_not_twenty_four_hours_later() {
        // The off-by-one day this file exists to get right: seedDay is a DATE,
        // so the box reopens at the next UTC midnight -- 34_000 s after
        // 2026-09-21T14:13:20Z -- not a full day after it was opened.
        let e = env_db(8, 1).await;
        let (m, mut rx) = e.connect("farmroom", 1, "u1").await;
        e.send("farmroom", 1, &m, GAME, "join", json!({})).await;
        until(&mut rx, GAME, "farm").await;
        e.send("farmroom", 1, &m, GAME, "seeds", json!({})).await;
        until(&mut rx, GAME, "farm").await;
        // 13:13:20 later: same UTC day, still closed.
        e.advance_wall(9.0 * 3600.0);
        e.send("farmroom", 1, &m, GAME, "seeds", json!({})).await;
        assert_eq!(until(&mut rx, GAME, "error").await["error"],
                   json!("the seed box is empty until tomorrow"));
        until(&mut rx, GAME, "farm").await;
        // One more hour crosses midnight: open again, and the season still reads
        // off the new day.
        e.advance_wall(3600.0);
        e.send("farmroom", 1, &m, GAME, "seeds", json!({})).await;
        let farm = until(&mut rx, GAME, "farm").await["farm"].clone();
        assert_eq!(seed_total(&farm), 2 * FARM_SEEDS_PER_DAY as i64);
        assert_eq!(farm["seedBoxOpen"], json!(false));
    }

    #[tokio::test]
    async fn a_planted_plot_starts_dry_and_unripe_and_cannot_be_harvested() {
        let e = env_db(8, 1).await;
        let (m, mut rx) = e.connect("farmroom", 1, "u1").await;
        e.send("farmroom", 1, &m, GAME, "join", json!({})).await;
        until(&mut rx, GAME, "farm").await;
        e.send("farmroom", 1, &m, GAME, "seeds", json!({})).await;
        let farm = until(&mut rx, GAME, "farm").await["farm"].clone();
        // The Python test's `next(iter(farm["seeds"]))`: the first key in
        // insertion order, which is why `seeds` is a Seq.
        let crop = seed_counts(&farm).keys().next().unwrap().clone();
        e.send("farmroom", 1, &m, GAME, "plant", json!({"plot": 0, "crop": crop})).await;
        let plot = until(&mut rx, GAME, "farm").await["farm"]["plots"][0].clone();
        assert_eq!(plot["crop"], json!(crop));
        assert_eq!(plot["water"], json!(0));
        assert_eq!(plot["ripe"], json!(false));
        assert_eq!(plot["by"], json!("u1"));
        let (hours, per, _) = crop_of(&crop).unwrap();
        assert_eq!(plot["need"], json!(per)); // one gardener, so need is unmultiplied
        assert_eq!(plot["hoursLeft"], json!(hours as f64));
        assert_eq!(plot.as_object().unwrap().keys().collect::<Vec<_>>(),
                   ["crop", "by", "water", "need", "hoursLeft", "ripe"]);
        e.send("farmroom", 1, &m, GAME, "harvest", json!({"plot": 0})).await;
        assert_eq!(until(&mut rx, GAME, "error").await["error"], json!("not ripe yet"));
        // The spent seed left the box rather than sitting there as a 0.
        let seeds = seed_counts(&until(&mut rx, GAME, "farm").await["farm"]);
        assert!(seeds.get(&crop).map(|n| n.as_i64().unwrap() > 0).unwrap_or(true));
    }

    #[tokio::test]
    async fn water_comes_from_published_prompts_and_the_farm_survives_a_reconnect() {
        // test_farm_is_watered_by_published_prompts_and_persists, end to end.
        let e = env_db(8, 1).await;
        let pool = e.hub.pool().unwrap().clone();
        user(&pool, "u1", 912).await;
        let (m, mut rx) = e.connect("farmroom", 1, "u1").await;
        e.send("farmroom", 1, &m, GAME, "join", json!({})).await;
        until(&mut rx, GAME, "farm").await;
        e.send("farmroom", 1, &m, GAME, "seeds", json!({})).await;
        let crop = seed_counts(&until(&mut rx, GAME, "farm").await["farm"])
            .keys()
            .next()
            .unwrap()
            .clone();
        e.send("farmroom", 1, &m, GAME, "plant", json!({"plot": 0, "crop": crop})).await;
        until(&mut rx, GAME, "farm").await;
        // Water is read out of daily_stats, never stored; time comes from the
        // clock. 500 prompts is over any crop's need, and a full day is over
        // any crop's hours.
        prompts(&pool, "u1", e.hub.today(), 500).await;
        e.advance_wall(24.0 * 3600.0);
        // A new socket in the same room: the garden survived because it is in
        // the database, not in RoomValley.
        e.disconnect("farmroom", 1, &m).await;
        let (m2, mut rx2) = e.connect("farmroom", 2, "u1").await;
        e.send("farmroom", 2, &m2, GAME, "join", json!({})).await;
        let plot = until(&mut rx2, GAME, "farm").await["farm"]["plots"][0].clone();
        assert_eq!(plot["ripe"], json!(true));
        assert_eq!(plot["hoursLeft"], json!(0.0));
        let (_, per, _) = crop_of(&crop).unwrap();
        assert_eq!(plot["water"], json!(per)); // min(500, need)
        e.send("farmroom", 2, &m2, GAME, "harvest", json!({"plot": 0})).await;
        let h = until(&mut rx2, GAME, "harvest").await;
        assert_eq!(h["crop"], json!(crop));
        assert_eq!(h["n"], json!(2));
        assert_eq!(until(&mut rx2, GAME, "note").await["text"],
                   json!(format!("u1 harvested the {crop}")));
        assert_eq!(until(&mut rx2, GAME, "farm").await["farm"]["plots"][0], json!(null));
    }

    #[tokio::test]
    async fn prompts_published_before_the_plot_was_planted_are_not_its_water() {
        // The water-since-date arithmetic: `stat_date >= since` where `since` is
        // the PLANTING DAY. A day earlier does not count; the planting day
        // itself counts in full, however early in the day the prompt landed.
        let e = env_db(8, 1).await;
        let pool = e.hub.pool().unwrap().clone();
        user(&pool, "u1", 913).await;
        let today = e.hub.today();
        prompts(&pool, "u1", today.pred_opt().unwrap(), 500).await;
        let (m, mut rx) = e.connect("farmroom", 1, "u1").await;
        e.send("farmroom", 1, &m, GAME, "join", json!({})).await;
        until(&mut rx, GAME, "farm").await;
        e.send("farmroom", 1, &m, GAME, "seeds", json!({})).await;
        let crop = seed_counts(&until(&mut rx, GAME, "farm").await["farm"])
            .keys()
            .next()
            .unwrap()
            .clone();
        e.send("farmroom", 1, &m, GAME, "plant", json!({"plot": 0, "crop": crop})).await;
        e.advance_wall(24.0 * 3600.0); // grown, but still dry
        e.send("farmroom", 1, &m, GAME, "harvest", json!({"plot": 0})).await;
        let farm = until(&mut rx, GAME, "farm").await["farm"].clone();
        assert_eq!(farm["plots"][0]["water"], json!(0));
        assert_eq!(farm["plots"][0]["ripe"], json!(false));
        // Yesterday's prompts stay out even after the day boundary moved.
        prompts(&pool, "u1", today, 500).await;
        e.send("farmroom", 1, &m, GAME, "harvest", json!({"plot": 0})).await;
        assert_eq!(until(&mut rx, GAME, "harvest").await["crop"], json!(crop));
    }

    #[tokio::test]
    async fn a_second_gardener_doubles_the_water_a_plot_needs() {
        // `need *= max(1, len(gardeners))` (valley.py:932), and the gardener
        // count is everyone who ever touched this room's farm.
        let e = env_db(8, 1).await;
        let (a, mut ra) = e.connect("farmroom", 1, "u1").await;
        e.send("farmroom", 1, &a, GAME, "join", json!({})).await;
        until(&mut ra, GAME, "farm").await;
        e.send("farmroom", 1, &a, GAME, "seeds", json!({})).await;
        let crop = seed_counts(&until(&mut ra, GAME, "farm").await["farm"])
            .keys()
            .next()
            .unwrap()
            .clone();
        e.send("farmroom", 1, &a, GAME, "plant", json!({"plot": 0, "crop": crop})).await;
        let (_, per, _) = crop_of(&crop).unwrap();
        assert_eq!(until(&mut ra, GAME, "farm").await["farm"]["plots"][0]["need"], json!(per));
        // u2 arrives only now, so their own queue holds just their own view.
        let (b, mut rb) = e.connect("farmroom", 2, "u2").await;
        e.send("farmroom", 2, &b, GAME, "join", json!({})).await;
        let farm = until(&mut rb, GAME, "farm").await["farm"].clone();
        assert_eq!(farm["gardeners"], json!(2));
        assert_eq!(farm["plots"][0]["need"], json!(per * 2));
    }

    #[tokio::test]
    async fn planting_refuses_a_bad_plot_a_taken_plot_and_a_crop_outside_the_catalog() {
        let e = env_db(8, 1).await;
        let (m, mut rx) = e.connect("farmroom", 1, "u1").await;
        e.send("farmroom", 1, &m, GAME, "join", json!({})).await;
        until(&mut rx, GAME, "farm").await;
        e.send("farmroom", 1, &m, GAME, "seeds", json!({})).await;
        let crop = seed_counts(&until(&mut rx, GAME, "farm").await["farm"])
            .keys()
            .next()
            .unwrap()
            .clone();
        // Every one of these is the same message, byte for byte.
        for bad in [json!({"plot": 9, "crop": crop}), json!({"plot": -1, "crop": crop}),
                    json!({"plot": 0.0, "crop": crop}), json!({"crop": crop}),
                    json!({"plot": 0, "crop": "durian"}), json!({"plot": 0})] {
            e.send("farmroom", 1, &m, GAME, "plant", bad.clone()).await;
            assert_eq!(until(&mut rx, GAME, "error").await["error"], json!("can't plant there"),
                       "{bad}");
            until(&mut rx, GAME, "farm").await;
        }
        e.send("farmroom", 1, &m, GAME, "plant", json!({"plot": 0, "crop": crop})).await;
        until(&mut rx, GAME, "farm").await;
        // A plot that is already planted is "can't plant there", not "taken".
        e.send("farmroom", 1, &m, GAME, "plant", json!({"plot": 0, "crop": crop})).await;
        assert_eq!(until(&mut rx, GAME, "error").await["error"], json!("can't plant there"));
    }

    #[tokio::test]
    async fn planting_a_crop_the_box_does_not_hold_says_no_seeds_of_that_kind() {
        let e = env_db(8, 1).await;
        let (m, mut rx) = e.connect("farmroom", 1, "u1").await;
        e.send("farmroom", 1, &m, GAME, "join", json!({})).await;
        until(&mut rx, GAME, "farm").await;
        // A real crop, an empty box: the second branch, not the first.
        e.send("farmroom", 1, &m, GAME, "plant", json!({"plot": 0, "crop": "kale"})).await;
        assert_eq!(until(&mut rx, GAME, "error").await["error"], json!("no seeds of that kind"));
        assert_eq!(until(&mut rx, GAME, "farm").await["farm"]["plots"][0], json!(null));
    }

    #[tokio::test]
    async fn a_json_true_is_plot_one_because_a_python_bool_is_an_int() {
        // isinstance(True, int) is True and True == 1, so `"plot": true` plants
        // in plot ONE. Ported deliberately; see plot_index.
        let e = env_db(8, 1).await;
        let (m, mut rx) = e.connect("farmroom", 1, "u1").await;
        e.send("farmroom", 1, &m, GAME, "join", json!({})).await;
        until(&mut rx, GAME, "farm").await;
        e.send("farmroom", 1, &m, GAME, "seeds", json!({})).await;
        let crop = seed_counts(&until(&mut rx, GAME, "farm").await["farm"])
            .keys()
            .next()
            .unwrap()
            .clone();
        e.send("farmroom", 1, &m, GAME, "plant", json!({"plot": true, "crop": crop})).await;
        let plots = until(&mut rx, GAME, "farm").await["farm"]["plots"].clone();
        assert_eq!(plots[0], json!(null));
        assert_eq!(plots[1]["crop"], json!(crop));
    }

    #[tokio::test]
    async fn harvesting_an_unplanted_or_out_of_range_plot_is_not_ripe_yet() {
        let e = env_db(8, 1).await;
        let (m, mut rx) = e.connect("farmroom", 1, "u1").await;
        e.send("farmroom", 1, &m, GAME, "join", json!({})).await;
        until(&mut rx, GAME, "farm").await;
        for bad in [json!({"plot": 0}), json!({"plot": 9}), json!({"plot": "0"}), json!({})] {
            e.send("farmroom", 1, &m, GAME, "harvest", bad.clone()).await;
            assert_eq!(until(&mut rx, GAME, "error").await["error"], json!("not ripe yet"),
                       "{bad}");
            until(&mut rx, GAME, "farm").await;
        }
    }

    #[tokio::test]
    async fn hours_left_rounds_to_one_decimal_and_never_goes_negative() {
        let d = NaiveDate::from_ymd_opt(2026, 9, 21).unwrap();
        let mut data = Data::parse(&Value::Null);
        data.gardeners.push("u1".into());
        data.plots[0] = json!({"crop": "grape", "at": 0.0, "day": "2026-09-21", "by": "ash"});
        // grape: 8 hours. 5.75 h in leaves exactly 2.25, and Python's round is
        // BANKER'S, so that is 2.2 and not the 2.3 a half-up rounding gives.
        let v = view(None, &data, 5.75 * 3600.0, d).await;
        assert_eq!(v["plots"][0]["hoursLeft"], json!(2.2));
        // Past ripeness the figure floors at 0.0 and never prints -0.0, which is
        // why the max is written the way Python's `max(0.0, x)` resolves a tie.
        let v = view(None, &data, 8.01 * 3600.0, d).await;
        assert_eq!(v["plots"][0]["hoursLeft"].to_string(), "0.0");
        // Exactly on the hour counts as grown: `elapsed >= hours`.
        let v = view(None, &data, 8.0 * 3600.0, d).await;
        assert_eq!(v["plots"][0]["hoursLeft"], json!(0.0));
        assert_eq!(v["plots"][0]["ripe"], json!(false)); // grown, but no water
    }

    #[tokio::test]
    async fn the_farm_view_goes_to_the_whole_room() {
        // `out.all`, not `out.lobby`: a socket that never joined the farm still
        // sees the garden and the notes. Only the errors and the harvest
        // receipt are for one socket.
        let e = env_db(8, 1).await;
        let (a, mut ra) = e.connect("farmroom", 1, "u1").await;
        let (_b, mut rb) = e.connect("farmroom", 2, "u2").await;
        e.send("farmroom", 1, &a, GAME, "join", json!({})).await;
        until(&mut ra, GAME, "farm").await;
        e.send("farmroom", 1, &a, GAME, "seeds", json!({})).await;
        // u2 joined no lobby and still gets both.
        assert_eq!(until(&mut rb, GAME, "note").await["text"], json!("u1 opened the seed box"));
        assert_eq!(until(&mut rb, GAME, "farm").await["farm"]["seedBoxOpen"], json!(false));
        // The error went to u1's socket alone.
        e.send("farmroom", 1, &a, GAME, "seeds", json!({})).await;
        until(&mut ra, GAME, "error").await;
        assert!(!drain(&mut rb).iter().any(|(_, ev)| ev == "error"));
    }

    #[tokio::test]
    async fn the_farm_needs_the_lobby_like_every_other_game() {
        // The shared gate, not this file's: a farm op from someone who never
        // joined never reaches `run`, so no row is created either.
        let e = env_db(8, 1).await;
        let (m, mut rx) = e.connect("farmroom", 1, "u1").await;
        e.send("farmroom", 1, &m, GAME, "seeds", json!({})).await;
        assert_eq!(until(&mut rx, GAME, "error").await["error"], json!("join the lobby first"));
        let n: i64 = sqlx::query("SELECT COUNT(*) AS n FROM room_farms")
            .fetch_one(e.hub.pool().unwrap())
            .await
            .unwrap()
            .get("n");
        assert_eq!(n, 0);
    }

    #[tokio::test]
    async fn a_farm_with_no_database_still_answers_a_view() {
        // `env` has no pool, as the other eight engines' tests do. The op runs,
        // nothing persists, and nothing panics.
        let e = env(8, 1);
        let (m, mut rx) = e.connect("farmroom", 1, "u1").await;
        e.send("farmroom", 1, &m, GAME, "join", json!({})).await;
        let farm = until(&mut rx, GAME, "farm").await["farm"].clone();
        assert_eq!(farm["gardeners"], json!(1));
        assert_eq!(farm["seedBoxOpen"], json!(true));
        e.send("farmroom", 1, &m, GAME, "seeds", json!({})).await;
        let farm = until(&mut rx, GAME, "farm").await["farm"].clone();
        assert_eq!(seed_total(&farm), FARM_SEEDS_PER_DAY as i64);
        // Nothing was stored, so the next op starts over.
        e.send("farmroom", 1, &m, GAME, "seeds", json!({})).await;
        let farm = until(&mut rx, GAME, "farm").await["farm"].clone();
        assert_eq!(seed_total(&farm), FARM_SEEDS_PER_DAY as i64);
    }

    #[tokio::test]
    async fn an_unrecognised_op_only_sends_the_view() {
        // Python's if/elif chain has no else, so "dig" is not an error -- but
        // the tail still commits and still broadcasts.
        let e = env_db(8, 1).await;
        let (m, mut rx) = e.connect("farmroom", 1, "u1").await;
        e.send("farmroom", 1, &m, GAME, "join", json!({})).await;
        until(&mut rx, GAME, "farm").await;
        e.send("farmroom", 1, &m, GAME, "dig", json!({})).await;
        until(&mut rx, GAME, "farm").await;
        assert!(drain(&mut rx).is_empty());
    }

    #[tokio::test]
    async fn a_plot_naming_a_crop_the_catalog_dropped_reads_as_empty_and_can_be_replanted() {
        // The DIVERGENCE on Data::parse, over the wire: Python's
        // `CROPS[p["crop"]]` would KeyError and close the socket.
        let e = env_db(8, 1).await;
        let plots = json!([{"crop": "durian", "at": 0.0, "day": "2026-09-21", "by": "ash"},
                           null, null, null, null, null, null, null, null]);
        sqlx::query("INSERT INTO room_farms (room_id, data, updated_at)
                     VALUES ('farmroom', ?1, datetime('now'))")
            .bind(json!({"plots": plots, "seeds": {"kale": 1}, "seedDay": "",
                         "gardeners": []})
                .to_string())
            .execute(e.hub.pool().unwrap())
            .await
            .unwrap();
        let (m, mut rx) = e.connect("farmroom", 1, "u1").await;
        e.send("farmroom", 1, &m, GAME, "join", json!({})).await;
        assert_eq!(until(&mut rx, GAME, "farm").await["farm"]["plots"][0], json!(null));
        // It reads empty but it is still THERE, so planting over it is refused
        // -- the plant arm tests the raw stored value's truthiness, as Python's
        // `data["plots"][i]` does.
        e.send("farmroom", 1, &m, GAME, "plant", json!({"plot": 0, "crop": "kale"})).await;
        assert_eq!(until(&mut rx, GAME, "error").await["error"], json!("can't plant there"));
    }
}
