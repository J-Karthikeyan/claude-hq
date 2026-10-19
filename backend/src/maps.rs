//! The map gallery (HQ 2.5): kart tracks, platformer levels and blaster maps
//! people make in the Valley editors, kept private, shared to a room, or
//! published to the whole Arena; with likes, reports and a weekly featured pick.
//!
//!   GET  /v1/maps?kind&sort=new|top|week&room&mine=1&ckey&cursor&limit  {maps, next, admin}
//!   GET  /v1/maps/featured                       {week, thisWeek:{kart,plat,fps}, lastWeek:{..}}
//!   GET  /v1/maps/:id                            {map: MapCard + data}
//!   POST /v1/maps        {id?, kind, name, data, scope, roomId?}   create or update (owner)
//!   POST /v1/maps/:id/like    {on}               {likes, liked}
//!   POST /v1/maps/:id/report  {reason}           {ok}
//!   POST /v1/maps/:id/delete  {}                 {ok}   owner or admin
//!   POST /v1/maps/:id/hide    {hidden}           {ok, hidden}   admin only
//!
//! What is stored is the CANONICAL map data the game's own validator returns,
//! keyed by `mapkey::content_key`, so a map that goes through the gallery and
//! back races on the same leaderboard (`game_results.key` = the c- key) as the
//! copy that never left the editor.
//!
//! Visibility: `private` maps are the owner's alone. `room` maps are listed and
//! readable only by the owner of that HQ room (`hq_<uid>`) or a non-banned
//! member of that private room (`r_...`): visitors to an open HQ and Quick
//! Play rooms get 403 on purpose, since neither is a group anyone chose.
//! `public` maps are everyone's unless hidden. Three distinct reports hide a
//! map; an admin (a handle in ARENA_ADMIN_HANDLES) can hide or unhide it, and
//! an unhide sticks (more reports never re-hide it automatically).
//!
//! Caps: 50 live maps per owner (409; a delete frees a slot), and 10 creates or
//! changes to public per UTC day (429; deletes do not give those back).
//!
//! Privacy: a map carries only what its maker typed and drew -- a name and
//! geometry. Names are refused if they look like markup or a link.

use axum::{
    extract::{Path, Query, Request, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::Serialize;
use serde_json::{json, Map, Value};
use sqlx::{Row, Sqlite, SqlitePool};
use std::collections::HashMap;

pub const KINDS: [&str; 3] = ["kart", "plat", "fps"];
pub const SCOPES: [&str; 3] = ["private", "room", "public"];
pub const REASONS: [&str; 4] = ["spam", "offensive", "broken", "other"];
/// Largest serialized `data` accepted (bytes).
pub const MAX_DATA: usize = 12 * 1024;
/// Largest request body read at all (the data plus its envelope).
const MAX_BODY: usize = 32 * 1024;
pub const MAX_NAME: usize = 32;
/// Live maps one owner may keep; a delete frees a slot.
pub const MAX_LIVE: i64 = 50;
/// Creates plus changes-to-public per owner per UTC day; deletes do not refund.
pub const PUBLISH_PER_DAY: i64 = 10;
/// Distinct reports that hide a map.
pub const HIDE_AT: i64 = 3;
pub const PAGE_DEFAULT: i64 = 20;
pub const PAGE_MAX: i64 = 30;
/// A like is worth this many racers in the weekly score.
pub const LIKE_WEIGHT: i64 = 3;

pub fn routes() -> Router<crate::AppState> {
    Router::new()
        .route("/v1/maps", get(list_h).post(save_h))
        .route("/v1/maps/featured", get(featured_h))
        .route("/v1/maps/:id", get(one_h))
        .route("/v1/maps/:id/like", post(like_h))
        .route("/v1/maps/:id/report", post(report_h))
        .route("/v1/maps/:id/delete", post(delete_h))
        .route("/v1/maps/:id/hide", post(hide_h))
}

// ------------------------------------------------------------------ errors --

/// A refusal: status plus the {"detail": msg} text.
#[derive(Debug, PartialEq)]
pub struct Refusal(pub StatusCode, pub String);

fn no(code: StatusCode, msg: &str) -> Refusal {
    Refusal(code, msg.to_string())
}

fn bad(msg: &str) -> Refusal {
    no(StatusCode::UNPROCESSABLE_ENTITY, msg)
}

fn db(_: sqlx::Error) -> Refusal {
    no(StatusCode::INTERNAL_SERVER_ERROR, "db error")
}

fn not_found() -> Refusal {
    no(StatusCode::NOT_FOUND, "no such map")
}

fn reply<T: Serialize>(r: Result<T, Refusal>) -> Response {
    match r {
        Ok(v) => Json(v).into_response(),
        Err(Refusal(code, msg)) => crate::err(code, &msg),
    }
}

// --------------------------------------------------------------- the viewer --

/// Who is asking: their user id, and whether they moderate.
#[derive(Clone, Debug)]
pub struct Viewer {
    pub user_id: String,
    pub admin: bool,
}

fn viewer_of(st: &crate::AppState, req: &Request) -> Viewer {
    let c = req.extensions().get::<crate::Caller>().expect("caller set by middleware");
    Viewer { user_id: c.user_id.clone(), admin: st.cfg.is_admin(&c.handle) }
}

// ------------------------------------------------------------- wire shapes --

#[derive(Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Owner {
    pub user_id: String,
    pub handle: String,
    pub display_name: String,
    pub avatar_url: String,
}

#[derive(Serialize, Debug, Clone)]
pub struct MapCard {
    pub id: String,
    pub kind: String,
    pub name: String,
    pub ckey: String,
    pub scope: String,
    pub owner: Owner,
    pub likes: i64,
    pub liked: bool,
    pub mine: bool,
    /// Only for the owner or an admin.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hidden: Option<bool>,
    /// Distinct people with a recorded race on this map's key.
    pub races: i64,
    pub created_at: String,
    pub updated_at: String,
    /// The weekly score, on featured picks only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub score: Option<i64>,
    /// The map itself, on single reads and saves only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

#[derive(Serialize, Debug)]
pub struct ListView {
    pub maps: Vec<MapCard>,
    pub next: Option<String>,
    /// Does the viewer moderate (the page shows Hide only then).
    pub admin: bool,
}

#[derive(Serialize, Debug)]
pub struct OneView {
    pub map: MapCard,
}

#[derive(Serialize, Debug, Default)]
pub struct Picks {
    pub kart: Option<MapCard>,
    pub plat: Option<MapCard>,
    pub fps: Option<MapCard>,
}

impl Picks {
    fn set(&mut self, kind: &str, c: Option<MapCard>) {
        match kind {
            "kart" => self.kart = c,
            "plat" => self.plat = c,
            _ => self.fps = c,
        }
    }
}

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct FeaturedView {
    pub week: String,
    pub this_week: Picks,
    pub last_week: Picks,
}

#[derive(Serialize, Debug)]
pub struct LikeView {
    pub likes: i64,
    pub liked: bool,
}

#[derive(Serialize, Debug)]
pub struct OkView {
    pub ok: bool,
}

#[derive(Serialize, Debug)]
pub struct HideView {
    pub ok: bool,
    pub hidden: bool,
}

// ---------------------------------------------------------------- helpers --

pub fn now_ts() -> String {
    chrono::Utc::now().format("%Y-%m-%d %H:%M:%S%.6f").to_string()
}

pub fn is_map_id(s: &str) -> bool {
    s.len() == 14
        && s.starts_with("m-")
        && s[2..].bytes().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

fn new_id() -> String {
    format!("m-{}", &uuid::Uuid::new_v4().simple().to_string()[..12])
}

/// A map name as stored: trimmed, 1-32 printable characters, no markup and no
/// link. Refused rather than cleaned, so what you typed is what others see.
pub fn clean_name(raw: &str) -> Result<String, String> {
    let s = raw.trim();
    let n = s.chars().count();
    if n == 0 {
        return Err("give your map a name".into());
    }
    if n > MAX_NAME {
        return Err("names are at most 32 characters".into());
    }
    if s.chars().any(|c| c.is_control() || c == '<' || c == '>') {
        return Err("names can't contain < > or control characters".into());
    }
    if s.to_ascii_lowercase().contains("http") || s.to_ascii_lowercase().contains("www.") {
        return Err("names can't contain links".into());
    }
    Ok(s.to_string())
}

/// The canonical form of `data` for `kind`, or the reason it is refused: the
/// games' own referee validators, so a gallery map is exactly what a room
/// would accept and its c- key is the one the room's board uses.
pub fn canonical(kind: &str, data: &Value) -> Result<Value, String> {
    match kind {
        "kart" => crate::kart::validate_custom(data),
        "plat" => crate::platformer::validate_custom(data),
        "fps" => crate::fps::validate_custom(data),
        _ => Err("unknown kind".into()),
    }
}

/// May `user_id` see and share into `room`? The owner of an HQ room, or an
/// owner/member (not banned) of a private room. Nothing else.
pub async fn room_ok(pool: &SqlitePool, room: &str, user_id: &str) -> Result<bool, Refusal> {
    if room.len() > 64 || room.is_empty() {
        return Ok(false);
    }
    if let Some(owner) = room.strip_prefix("hq_") {
        return Ok(room != "hq_city" && owner == user_id);
    }
    if crate::privrooms::is_private_id(room) {
        let a = crate::privrooms::admission(pool, room, user_id).await.map_err(db)?;
        return Ok(matches!(a, Some((_, Some(r))) if r == "owner" || r == "member"));
    }
    Ok(false)
}

/// One stored row, before it is shaped for a viewer.
#[derive(Debug, Clone)]
struct RowMap {
    id: String,
    owner_id: String,
    kind: String,
    name: String,
    ckey: String,
    scope: String,
    room_id: Option<String>,
    likes: i64,
    hidden: bool,
    created_at: String,
    updated_at: String,
    handle: String,
    display_name: String,
    avatar_url: String,
    liked: bool,
    races: i64,
    data: String,
    sort_n: i64,
}

/// The columns every read selects; ?1 is always the viewer's id.
const SELECT: &str = r#"SELECT m.id, m.owner_id, m.kind, m.name, m.ckey, m.scope, m.room_id,
    m.likes, m.hidden, m.created_at, m.updated_at, m.data,
    u.handle, u.display_name, u.avatar_url,
    EXISTS(SELECT 1 FROM user_map_likes l WHERE l.map_id = m.id AND l.user_id = ?1) AS liked,
    (SELECT COUNT(DISTINCT g.user_id) FROM game_results g
       WHERE g.game = m.kind AND g."key" = m.ckey) AS races"#;

fn row_of(r: &sqlx::sqlite::SqliteRow) -> RowMap {
    RowMap {
        id: r.get("id"),
        owner_id: r.get("owner_id"),
        kind: r.get("kind"),
        name: r.get("name"),
        ckey: r.get("ckey"),
        scope: r.get("scope"),
        room_id: r.get("room_id"),
        likes: r.get("likes"),
        hidden: r.get::<i64, _>("hidden") != 0,
        created_at: r.get("created_at"),
        updated_at: r.get("updated_at"),
        handle: r.get("handle"),
        display_name: r.get("display_name"),
        avatar_url: r.get("avatar_url"),
        liked: r.get::<i64, _>("liked") != 0,
        races: r.get("races"),
        data: r.get("data"),
        sort_n: r.try_get::<i64, _>("sort_n").unwrap_or(0),
    }
}

fn card(m: &RowMap, v: &Viewer, with_data: bool) -> MapCard {
    let mine = m.owner_id == v.user_id;
    MapCard {
        id: m.id.clone(),
        kind: m.kind.clone(),
        name: m.name.clone(),
        ckey: m.ckey.clone(),
        scope: m.scope.clone(),
        owner: Owner {
            user_id: m.owner_id.clone(),
            handle: m.handle.clone(),
            display_name: m.display_name.clone(),
            avatar_url: m.avatar_url.clone(),
        },
        likes: m.likes,
        liked: m.liked,
        mine,
        hidden: (mine || v.admin).then_some(m.hidden),
        races: m.races,
        created_at: m.created_at.clone(),
        updated_at: m.updated_at.clone(),
        score: None,
        data: if with_data { serde_json::from_str(&m.data).ok() } else { None },
    }
}

async fn load(pool: &SqlitePool, id: &str, v: &Viewer) -> Result<Option<RowMap>, Refusal> {
    let sql = format!(
        "{SELECT} FROM user_maps m JOIN users u ON u.id = m.owner_id AND u.is_active = 1
         WHERE m.id = ?2"
    );
    let r = sqlx::query(&sql).bind(&v.user_id).bind(id).fetch_optional(pool).await.map_err(db)?;
    Ok(r.as_ref().map(row_of))
}

/// Can `v` read this map at all? (Owner always; admin for anything shared;
/// otherwise public-and-visible, or room-and-visible to a member.)
async fn can_see(pool: &SqlitePool, m: &RowMap, v: &Viewer) -> Result<bool, Refusal> {
    if m.owner_id == v.user_id {
        return Ok(true);
    }
    match m.scope.as_str() {
        "private" => Ok(false),
        "public" => Ok(!m.hidden || v.admin),
        _ => {
            if m.hidden && !v.admin {
                return Ok(false);
            }
            match &m.room_id {
                Some(room) => Ok(v.admin || room_ok(pool, room, &v.user_id).await?),
                None => Ok(v.admin),
            }
        }
    }
}

/// A visible map or 404 -- a map you may not see does not exist for you.
async fn visible(pool: &SqlitePool, id: &str, v: &Viewer) -> Result<RowMap, Refusal> {
    if !is_map_id(id) {
        return Err(not_found());
    }
    let m = load(pool, id, v).await?.ok_or_else(not_found)?;
    if !can_see(pool, &m, v).await? {
        return Err(not_found());
    }
    Ok(m)
}

// ------------------------------------------------------------------ cursor --

/// Opaque page cursor: hex of the JSON [sortN, created_at, id] of the last row.
pub fn encode_cursor(n: i64, created_at: &str, id: &str) -> String {
    hex::encode(json!([n, created_at, id]).to_string())
}

pub fn decode_cursor(s: &str) -> Option<(i64, String, String)> {
    if s.len() > 400 {
        return None;
    }
    let raw = hex::decode(s).ok()?;
    let v: Value = serde_json::from_slice(&raw).ok()?;
    let a = v.as_array()?;
    if a.len() != 3 {
        return None;
    }
    let id = a[2].as_str()?;
    let at = a[1].as_str()?;
    if !is_map_id(id) || at.len() > 40 {
        return None;
    }
    Some((a[0].as_i64()?, at.to_string(), id.to_string()))
}

// -------------------------------------------------------------------- list --

/// The parsed GET /v1/maps query.
#[derive(Debug, Default, Clone)]
pub struct ListQ {
    pub kind: Option<String>,
    pub sort: String,
    pub room: Option<String>,
    pub mine: bool,
    pub ckey: Option<String>,
    pub cursor: Option<(i64, String, String)>,
    pub limit: i64,
}

pub fn parse_list_q(q: &HashMap<String, String>) -> Result<ListQ, Refusal> {
    for k in q.keys() {
        if !["kind", "sort", "room", "mine", "ckey", "cursor", "limit"].contains(&k.as_str()) {
            return Err(bad(&format!("unknown parameter {k}")));
        }
    }
    let get = |k: &str| q.get(k).map(|s| s.trim()).filter(|s| !s.is_empty());
    let kind = match get("kind") {
        None => None,
        Some(k) if KINDS.contains(&k) => Some(k.to_string()),
        Some(_) => return Err(bad("kind must be kart, plat or fps")),
    };
    let sort = match get("sort").unwrap_or("new") {
        s @ ("new" | "top" | "week") => s.to_string(),
        _ => return Err(bad("sort must be new, top or week")),
    };
    let room = get("room").map(str::to_string);
    if room.as_deref().is_some_and(|r| r.len() > 64) {
        return Err(bad("bad room"));
    }
    let mine = match get("mine") {
        None | Some("0") => false,
        Some("1") => true,
        Some(_) => return Err(bad("mine must be 1")),
    };
    let ckey = get("ckey").map(str::to_string);
    if ckey.as_deref().is_some_and(|c| {
        c.len() != 14 || !c.starts_with("c-") || !c[2..].bytes().all(|b| b.is_ascii_hexdigit())
    }) {
        return Err(bad("bad ckey"));
    }
    let cursor = match get("cursor") {
        None => None,
        Some(c) => Some(decode_cursor(c).ok_or_else(|| bad("bad cursor"))?),
    };
    let limit = match get("limit") {
        None => PAGE_DEFAULT,
        Some(l) => match l.parse::<i64>() {
            Ok(n) if (1..=PAGE_MAX).contains(&n) => n,
            _ => return Err(bad("limit must be 1 to 30")),
        },
    };
    if mine && room.is_some() {
        return Err(bad("pick mine or room, not both"));
    }
    Ok(ListQ { kind, sort, room, mine, ckey, cursor, limit })
}

/// A bound SQL argument, numbered ?1, ?2, ... in push order.
enum Arg {
    S(String),
    I(i64),
}

fn push(args: &mut Vec<Arg>, a: Arg) -> String {
    args.push(a);
    format!("?{}", args.len())
}

pub async fn list(pool: &SqlitePool, v: &Viewer, q: &ListQ) -> Result<ListView, Refusal> {
    if let Some(room) = &q.room {
        if !room_ok(pool, room, &v.user_id).await? {
            return Err(no(StatusCode::FORBIDDEN, "room maps are for that room's members"));
        }
    }
    let (start, end) = crate::weeks::week_bounds(&crate::weeks::iso_week(&now_ts()));
    // The sort number: 0 for new, likes for top, this week's score for week.
    let sort_n = match q.sort.as_str() {
        "top" => "m.likes".to_string(),
        "week" => week_score_sql("?2", "?3"),
        _ => "0".to_string(),
    };
    // ?1 viewer, ?2/?3 the week window (always bound so the numbering is fixed).
    let mut args: Vec<Arg> = vec![Arg::S(v.user_id.clone()), Arg::S(start), Arg::S(end)];
    let mut sql = format!(
        "SELECT * FROM ({SELECT}, {sort_n} AS sort_n FROM user_maps m
           JOIN users u ON u.id = m.owner_id AND u.is_active = 1 WHERE 1 = 1");
    if q.mine {
        sql += &format!(" AND m.owner_id = {}", push(&mut args, Arg::S(v.user_id.clone())));
    } else if let Some(room) = &q.room {
        sql += &format!(" AND m.scope = 'room' AND m.room_id = {} AND (m.hidden = 0 OR m.owner_id = ?1)",
                        push(&mut args, Arg::S(room.clone())));
    } else {
        sql += " AND m.scope = 'public' AND m.hidden = 0";
    }
    if let Some(k) = &q.kind {
        sql += &format!(" AND m.kind = {}", push(&mut args, Arg::S(k.clone())));
    }
    if let Some(c) = &q.ckey {
        sql += &format!(" AND m.ckey = {}", push(&mut args, Arg::S(c.clone())));
    }
    sql += ") t";
    // week: score high to low, then the older map first; else newest first.
    let asc = q.sort == "week";
    let cmp = if asc { ">" } else { "<" };
    if let Some((n, at, id)) = &q.cursor {
        let (pn, pa, pi) = (push(&mut args, Arg::I(*n)), push(&mut args, Arg::S(at.clone())),
                            push(&mut args, Arg::S(id.clone())));
        sql += &format!(" WHERE (t.sort_n < {pn} OR (t.sort_n = {pn} AND (t.created_at {cmp} {pa}
                          OR (t.created_at = {pa} AND t.id {cmp} {pi}))))");
    }
    let dir = if asc { "ASC" } else { "DESC" };
    sql += &format!(" ORDER BY t.sort_n DESC, t.created_at {dir}, t.id {dir} LIMIT {}", q.limit + 1);
    let mut qq = sqlx::query(&sql);
    for a in &args {
        qq = match a {
            Arg::S(s) => qq.bind(s.clone()),
            Arg::I(i) => qq.bind(*i),
        };
    }
    let rows = qq.fetch_all(pool).await.map_err(db)?;
    let mut all: Vec<RowMap> = rows.iter().map(row_of).collect();
    let more = all.len() as i64 > q.limit;
    all.truncate(q.limit as usize);
    let next = if more {
        all.last().map(|m| encode_cursor(m.sort_n, &m.created_at, &m.id))
    } else {
        None
    };
    Ok(ListView { maps: all.iter().map(|m| card(m, v, false)).collect(), next, admin: v.admin })
}

/// SQL for a map's score in a [start, end] window: 3 x likes given in it plus
/// distinct people with a result on its key in it.
fn week_score_sql(start: &str, end: &str) -> String {
    format!(
        r#"({LIKE_WEIGHT} * (SELECT COUNT(*) FROM user_map_likes wl WHERE wl.map_id = m.id
              AND wl.at >= {start} AND wl.at <= {end})
           + (SELECT COUNT(DISTINCT wg.user_id) FROM game_results wg WHERE wg.game = m.kind
              AND wg."key" = m.ckey AND wg.at >= {start} AND wg.at <= {end}))"#
    )
}

pub async fn one(pool: &SqlitePool, v: &Viewer, id: &str) -> Result<OneView, Refusal> {
    let m = visible(pool, id, v).await?;
    Ok(OneView { map: card(&m, v, true) })
}

// ---------------------------------------------------------------- featured --

/// The week before a "YYYY-Www" week.
pub fn prev_week(week: &str) -> String {
    let (start, _) = crate::weeks::week_bounds(week);
    let d = chrono::NaiveDate::parse_from_str(start.get(..10).unwrap_or(""), "%Y-%m-%d")
        .unwrap_or_default() - chrono::Duration::days(7);
    crate::weeks::iso_week(&d.format("%Y-%m-%d").to_string())
}

/// The best public, visible map of `kind` in `week` (score > 0), ties to the
/// earlier map: (id, score).
async fn winner(pool: &SqlitePool, kind: &str, week: &str) -> Result<Option<(String, i64)>, Refusal> {
    let (start, end) = crate::weeks::week_bounds(week);
    let sql = format!(
        "SELECT m.id, {} AS score FROM user_maps m JOIN users u ON u.id = m.owner_id AND u.is_active = 1
         WHERE m.kind = ?3 AND m.scope = 'public' AND m.hidden = 0
         ORDER BY score DESC, m.created_at ASC, m.id ASC LIMIT 1",
        week_score_sql("?1", "?2")
    );
    let r = sqlx::query(&sql).bind(&start).bind(&end).bind(kind)
        .fetch_optional(pool).await.map_err(db)?;
    Ok(r.map(|r| (r.get::<String, _>("id"), r.get::<i64, _>("score"))).filter(|(_, s)| *s > 0))
}

/// The card for a featured pick, if the map is still there and public.
async fn pick(pool: &SqlitePool, v: &Viewer, id: &str, score: i64) -> Result<Option<MapCard>, Refusal> {
    let Some(m) = load(pool, id, v).await? else { return Ok(None) };
    if m.scope != "public" || m.hidden {
        return Ok(None);
    }
    let mut c = card(&m, v, false);
    c.score = Some(score);
    Ok(Some(c))
}

pub async fn featured(pool: &SqlitePool, v: &Viewer, now: &str) -> Result<FeaturedView, Refusal> {
    let week = crate::weeks::iso_week(now);
    let last = prev_week(&week);
    let mut this_week = Picks::default();
    let mut last_week = Picks::default();
    for kind in KINDS {
        if let Some((id, score)) = winner(pool, kind, &week).await? {
            this_week.set(kind, pick(pool, v, &id, score).await?);
        }
        // Last week is frozen on its first read, so a later unlike, delete or
        // hide can't rewrite who won it.
        let frozen = sqlx::query("SELECT map_id, score FROM map_features WHERE week = ?1 AND kind = ?2")
            .bind(&last).bind(kind).fetch_optional(pool).await.map_err(db)?;
        let got = match frozen {
            Some(r) => Some((r.get::<String, _>("map_id"), r.get::<i64, _>("score"))),
            None => match winner(pool, kind, &last).await? {
                Some((id, score)) => {
                    sqlx::query("INSERT OR IGNORE INTO map_features (week, kind, map_id, score, at)
                                 VALUES (?1, ?2, ?3, ?4, ?5)")
                        .bind(&last).bind(kind).bind(&id).bind(score).bind(now_ts())
                        .execute(pool).await.map_err(db)?;
                    // Whoever's insert won a race is the one everyone sees.
                    sqlx::query("SELECT map_id, score FROM map_features WHERE week = ?1 AND kind = ?2")
                        .bind(&last).bind(kind).fetch_optional(pool).await.map_err(db)?
                        .map(|r| (r.get::<String, _>("map_id"), r.get::<i64, _>("score")))
                }
                None => None,
            },
        };
        if let Some((id, score)) = got {
            last_week.set(kind, pick(pool, v, &id, score).await?);
        }
    }
    Ok(FeaturedView { week, this_week, last_week })
}

// -------------------------------------------------------------------- save --

/// A parsed, validated POST /v1/maps body.
#[derive(Debug, Clone)]
pub struct SaveBody {
    pub id: Option<String>,
    pub kind: String,
    pub name: String,
    pub data: Value,
    pub scope: String,
    pub room_id: Option<String>,
}

fn body_object(bytes: &[u8]) -> Result<Map<String, Value>, Refusal> {
    match serde_json::from_slice::<Value>(bytes) {
        Ok(Value::Object(o)) => Ok(o),
        Ok(_) => Err(bad("body must be an object")),
        Err(_) => Err(bad("bad JSON body")),
    }
}

fn forbid_extra(o: &Map<String, Value>, known: &[&str]) -> Result<(), Refusal> {
    match o.keys().find(|k| !known.contains(&k.as_str())) {
        Some(k) => Err(bad(&format!("unexpected field {k}"))),
        None => Ok(()),
    }
}

fn want_str<'a>(o: &'a Map<String, Value>, k: &str) -> Result<&'a str, Refusal> {
    o.get(k).and_then(Value::as_str).ok_or_else(|| bad(&format!("{k} must be a string")))
}

pub fn parse_save(bytes: &[u8]) -> Result<SaveBody, Refusal> {
    let o = body_object(bytes)?;
    forbid_extra(&o, &["id", "kind", "name", "data", "scope", "roomId"])?;
    let id = match o.get("id") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) if is_map_id(s) => Some(s.clone()),
        Some(_) => return Err(bad("bad map id")),
    };
    let kind = want_str(&o, "kind")?;
    if !KINDS.contains(&kind) {
        return Err(bad("kind must be kart, plat or fps"));
    }
    let name = clean_name(want_str(&o, "name")?).map_err(|e| bad(&e))?;
    let scope = want_str(&o, "scope")?;
    if !SCOPES.contains(&scope) {
        return Err(bad("scope must be private, room or public"));
    }
    let room_id = match o.get("roomId") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) if !s.is_empty() && s.len() <= 64 => Some(s.clone()),
        Some(_) => return Err(bad("bad roomId")),
    };
    if (scope == "room") != room_id.is_some() {
        return Err(bad("roomId goes with scope room, and only with it"));
    }
    let data = match o.get("data") {
        Some(d @ Value::Object(_)) => d,
        _ => return Err(bad("data must be an object")),
    };
    if serde_json::to_string(data).map(|s| s.len()).unwrap_or(usize::MAX) > MAX_DATA {
        return Err(bad("map data is over 12 KiB"));
    }
    let canon = canonical(kind, data).map_err(|e| bad(&format!("bad map: {e}")))?;
    if serde_json::to_string(&canon).map(|s| s.len()).unwrap_or(usize::MAX) > MAX_DATA {
        return Err(bad("map data is over 12 KiB"));
    }
    Ok(SaveBody { id, kind: kind.into(), name, data: canon, scope: scope.into(), room_id })
}

/// Count one create or change-to-public against today's quota, inside `tx`.
/// This is the transaction's first write, so it takes the write lock before
/// anything is counted: two racing saves cannot both see room under a cap.
async fn spend_publish(tx: &mut sqlx::Transaction<'_, Sqlite>, user_id: &str, day: &str)
    -> Result<(), Refusal> {
    let r = sqlx::query(
        "INSERT INTO map_publishes (user_id, day, n) VALUES (?1, ?2, 1)
         ON CONFLICT(user_id, day) DO UPDATE SET n = n + 1 WHERE n < ?3")
        .bind(user_id).bind(day).bind(PUBLISH_PER_DAY)
        .execute(&mut **tx).await.map_err(db)?;
    if r.rows_affected() == 0 {
        return Err(no(StatusCode::TOO_MANY_REQUESTS,
                      "that's 10 new or published maps today; try again tomorrow"));
    }
    Ok(())
}

pub async fn save(pool: &SqlitePool, v: &Viewer, b: SaveBody) -> Result<OneView, Refusal> {
    if let Some(room) = &b.room_id {
        if !room_ok(pool, room, &v.user_id).await? {
            return Err(no(StatusCode::FORBIDDEN, "you can only share to your HQ or a room you're in"));
        }
    }
    let ckey = crate::mapkey::content_key(&b.kind, &b.data);
    let data = serde_json::to_string(&b.data).map_err(|_| bad("bad map"))?;
    let now = now_ts();
    let day = now[..10].to_string();
    let id = match &b.id {
        Some(id) => {
            // Read first, outside the transaction: the write below re-checks
            // ownership in its WHERE, so a lost race is a 404, never a takeover.
            let prev = sqlx::query("SELECT owner_id, kind, scope FROM user_maps WHERE id = ?1")
                .bind(id).fetch_optional(pool).await.map_err(db)?.ok_or_else(not_found)?;
            if prev.get::<String, _>("owner_id") != v.user_id {
                return Err(no(StatusCode::FORBIDDEN, "that map isn't yours"));
            }
            if prev.get::<String, _>("kind") != b.kind {
                return Err(bad("a map can't change kind"));
            }
            let to_public = b.scope == "public" && prev.get::<String, _>("scope") != "public";
            let mut tx = pool.begin().await.map_err(db)?;
            if to_public {
                spend_publish(&mut tx, &v.user_id, &day).await?;
            }
            let r = sqlx::query(
                "UPDATE user_maps SET name = ?1, data = ?2, ckey = ?3, scope = ?4, room_id = ?5,
                     updated_at = ?6 WHERE id = ?7 AND owner_id = ?8")
                .bind(&b.name).bind(&data).bind(&ckey).bind(&b.scope).bind(&b.room_id)
                .bind(&now).bind(id).bind(&v.user_id)
                .execute(&mut *tx).await.map_err(db)?;
            if r.rows_affected() == 0 {
                return Err(not_found());
            }
            tx.commit().await.map_err(db)?;
            id.clone()
        }
        None => {
            let id = new_id();
            let mut tx = pool.begin().await.map_err(db)?;
            spend_publish(&mut tx, &v.user_id, &day).await?;
            let live: i64 = sqlx::query("SELECT COUNT(*) AS n FROM user_maps WHERE owner_id = ?1")
                .bind(&v.user_id).fetch_one(&mut *tx).await.map_err(db)?.get("n");
            if live >= MAX_LIVE {
                // Dropping the transaction rolls the quota spend back too.
                return Err(no(StatusCode::CONFLICT,
                              "you have 50 maps; delete one to make room"));
            }
            sqlx::query(
                "INSERT INTO user_maps (id, owner_id, kind, name, data, ckey, scope, room_id,
                     created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9)")
                .bind(&id).bind(&v.user_id).bind(&b.kind).bind(&b.name).bind(&data).bind(&ckey)
                .bind(&b.scope).bind(&b.room_id).bind(&now)
                .execute(&mut *tx).await.map_err(db)?;
            tx.commit().await.map_err(db)?;
            id
        }
    };
    let m = load(pool, &id, v).await?.ok_or_else(not_found)?;
    Ok(OneView { map: card(&m, v, true) })
}

// ---------------------------------------------------- like / report / mod --

pub fn parse_flag(bytes: &[u8], key: &str) -> Result<bool, Refusal> {
    let o = body_object(bytes)?;
    forbid_extra(&o, &[key])?;
    o.get(key).and_then(Value::as_bool).ok_or_else(|| bad(&format!("{key} must be true or false")))
}

pub fn parse_reason(bytes: &[u8]) -> Result<String, Refusal> {
    let o = body_object(bytes)?;
    forbid_extra(&o, &["reason"])?;
    let r = want_str(&o, "reason")?;
    if !REASONS.contains(&r) {
        return Err(bad("reason must be spam, offensive, broken or other"));
    }
    Ok(r.to_string())
}

pub fn parse_empty(bytes: &[u8]) -> Result<(), Refusal> {
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Ok(());
    }
    let o = body_object(bytes)?;
    forbid_extra(&o, &[])
}

pub async fn like(pool: &SqlitePool, v: &Viewer, id: &str, on: bool) -> Result<LikeView, Refusal> {
    let m = visible(pool, id, v).await?;
    if m.owner_id == v.user_id {
        return Err(no(StatusCode::FORBIDDEN, "you can't like your own map"));
    }
    let mut tx = pool.begin().await.map_err(db)?;
    if on {
        let r = sqlx::query("INSERT OR IGNORE INTO user_map_likes (map_id, user_id, at) VALUES (?1, ?2, ?3)")
            .bind(id).bind(&v.user_id).bind(now_ts()).execute(&mut *tx).await.map_err(db)?;
        if r.rows_affected() > 0 {
            sqlx::query("UPDATE user_maps SET likes = likes + 1 WHERE id = ?1")
                .bind(id).execute(&mut *tx).await.map_err(db)?;
        }
    } else {
        let r = sqlx::query("DELETE FROM user_map_likes WHERE map_id = ?1 AND user_id = ?2")
            .bind(id).bind(&v.user_id).execute(&mut *tx).await.map_err(db)?;
        if r.rows_affected() > 0 {
            sqlx::query("UPDATE user_maps SET likes = MAX(0, likes - 1) WHERE id = ?1")
                .bind(id).execute(&mut *tx).await.map_err(db)?;
        }
    }
    let likes: i64 = sqlx::query("SELECT likes FROM user_maps WHERE id = ?1")
        .bind(id).fetch_optional(&mut *tx).await.map_err(db)?
        .map(|r| r.get("likes")).unwrap_or(0);
    tx.commit().await.map_err(db)?;
    Ok(LikeView { likes, liked: on })
}

pub async fn report(pool: &SqlitePool, v: &Viewer, id: &str, reason: &str) -> Result<OkView, Refusal> {
    let m = visible(pool, id, v).await?;
    if m.owner_id == v.user_id {
        return Err(no(StatusCode::FORBIDDEN, "you can't report your own map"));
    }
    let mut tx = pool.begin().await.map_err(db)?;
    let r = sqlx::query("INSERT OR IGNORE INTO user_map_reports (map_id, user_id, reason, at)
                         VALUES (?1, ?2, ?3, ?4)")
        .bind(id).bind(&v.user_id).bind(reason).bind(now_ts())
        .execute(&mut *tx).await.map_err(db)?;
    if r.rows_affected() > 0 {
        // Hidden exactly when the count reaches HIDE_AT: once an admin unhides
        // a map, later reports don't take it down again by themselves.
        sqlx::query("UPDATE user_maps SET reports = reports + 1,
                         hidden = CASE WHEN reports + 1 = ?2 THEN 1 ELSE hidden END
                     WHERE id = ?1")
            .bind(id).bind(HIDE_AT).execute(&mut *tx).await.map_err(db)?;
    }
    tx.commit().await.map_err(db)?;
    Ok(OkView { ok: true })
}

pub async fn delete(pool: &SqlitePool, v: &Viewer, id: &str) -> Result<OkView, Refusal> {
    if !is_map_id(id) {
        return Err(not_found());
    }
    let owner: Option<String> = sqlx::query("SELECT owner_id FROM user_maps WHERE id = ?1")
        .bind(id).fetch_optional(pool).await.map_err(db)?.map(|r| r.get("owner_id"));
    let Some(owner) = owner else { return Err(not_found()) };
    if owner != v.user_id && !v.admin {
        return Err(no(StatusCode::FORBIDDEN, "that map isn't yours"));
    }
    let mut tx = pool.begin().await.map_err(db)?;
    let r = sqlx::query("DELETE FROM user_maps WHERE id = ?1").bind(id)
        .execute(&mut *tx).await.map_err(db)?;
    if r.rows_affected() == 0 {
        return Err(not_found());
    }
    sqlx::query("DELETE FROM user_map_likes WHERE map_id = ?1").bind(id)
        .execute(&mut *tx).await.map_err(db)?;
    sqlx::query("DELETE FROM user_map_reports WHERE map_id = ?1").bind(id)
        .execute(&mut *tx).await.map_err(db)?;
    tx.commit().await.map_err(db)?;
    Ok(OkView { ok: true })
}

pub async fn hide(pool: &SqlitePool, v: &Viewer, id: &str, hidden: bool) -> Result<HideView, Refusal> {
    if !v.admin {
        return Err(no(StatusCode::FORBIDDEN, "admins only"));
    }
    if !is_map_id(id) {
        return Err(not_found());
    }
    let r = sqlx::query("UPDATE user_maps SET hidden = ?1 WHERE id = ?2")
        .bind(i64::from(hidden)).bind(id).execute(pool).await.map_err(db)?;
    if r.rows_affected() == 0 {
        return Err(not_found());
    }
    Ok(HideView { ok: true, hidden })
}

// ---------------------------------------------------------------- handlers --

async fn body_bytes(req: Request) -> Result<axum::body::Bytes, Refusal> {
    axum::body::to_bytes(req.into_body(), MAX_BODY)
        .await
        .map_err(|_| no(StatusCode::PAYLOAD_TOO_LARGE, "map data is over 12 KiB"))
}

async fn list_h(State(st): State<crate::AppState>, Query(q): Query<HashMap<String, String>>,
                req: Request) -> Response {
    let v = viewer_of(&st, &req);
    reply(match parse_list_q(&q) {
        Ok(q) => list(&st.pool, &v, &q).await,
        Err(e) => Err(e),
    })
}

async fn featured_h(State(st): State<crate::AppState>, req: Request) -> Response {
    let v = viewer_of(&st, &req);
    reply(featured(&st.pool, &v, &now_ts()).await)
}

async fn one_h(State(st): State<crate::AppState>, Path(id): Path<String>, req: Request) -> Response {
    let v = viewer_of(&st, &req);
    reply(one(&st.pool, &v, &id).await)
}

async fn save_h(State(st): State<crate::AppState>, req: Request) -> Response {
    let v = viewer_of(&st, &req);
    let r = async {
        let b = parse_save(&body_bytes(req).await?)?;
        save(&st.pool, &v, b).await
    }.await;
    reply(r)
}

async fn like_h(State(st): State<crate::AppState>, Path(id): Path<String>, req: Request) -> Response {
    let v = viewer_of(&st, &req);
    let r = async {
        let on = parse_flag(&body_bytes(req).await?, "on")?;
        like(&st.pool, &v, &id, on).await
    }.await;
    reply(r)
}

async fn report_h(State(st): State<crate::AppState>, Path(id): Path<String>, req: Request) -> Response {
    let v = viewer_of(&st, &req);
    let r = async {
        let reason = parse_reason(&body_bytes(req).await?)?;
        report(&st.pool, &v, &id, &reason).await
    }.await;
    reply(r)
}

async fn delete_h(State(st): State<crate::AppState>, Path(id): Path<String>, req: Request) -> Response {
    let v = viewer_of(&st, &req);
    let r = async {
        parse_empty(&body_bytes(req).await?)?;
        delete(&st.pool, &v, &id).await
    }.await;
    reply(r)
}

async fn hide_h(State(st): State<crate::AppState>, Path(id): Path<String>, req: Request) -> Response {
    let v = viewer_of(&st, &req);
    let r = async {
        let h = parse_flag(&body_bytes(req).await?, "hidden")?;
        hide(&st.pool, &v, &id, h).await
    }.await;
    reply(r)
}


#[cfg(test)]
mod tests;
