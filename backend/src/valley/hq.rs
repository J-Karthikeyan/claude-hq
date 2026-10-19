//! Live presence in an HQ and in Arena City (`g = "hq"`): who is in the room
//! right now, and where they stand.
//!
//! A port of backend/app/hqpresence.py in full, plus the four places
//! backend/app/valley.py reaches into it -- the join tail (:1067-1070), the
//! `pos` op (:1136-1138), the loop `_hq_tick_on` (:1271-1287) and the leave
//! branch (:1558-1559).
//!
//! Each HQ has its own Arena room, `hq_<owner user id>`; Arena City, the street
//! of open HQs, is one more room, `hq_city`. Everyone in either joins the "hq"
//! lobby and sends where they stand: which floor, x/z in centimetres, facing in
//! degrees, and whether they walk. The room's loop sends the whole list 8 times
//! a second, so everyone sees everyone else move. Positions only: no session
//! data, and nothing is stored (hqpresence.py:1-9).
//!
//! THE AUDIENCE RULE, in one line (see the audience note at the top of
//! `super`): the join `snap` goes to the ONE socket that joined (`out.to`,
//! valley.py:1069), and every ticked `snap` goes only to the hq LOBBY
//! (`out.lobby(list(v.lobbies["hq"].members), ..)`, valley.py:1283) -- never to
//! the whole room, so a socket sitting in an `hq_` room that never sent `join`
//! learns nothing about anybody.
//!
//! hq emits NO error of its own. The two a client can provoke are the
//! dispatcher's, not this file's: "HQ presence lives in an HQ room" at
//! [`super::room_gate`] and "this game's lobby is full" at [`super::cap`]. A
//! `pos` that fails every rule is answered with silence -- the next snapshot
//! simply still shows the old position.
//!
//! WHAT THE SHARED LAYER ALREADY DOES, so this file does not do it twice:
//! [`super::cap`] answers 16 in an HQ and 40 in `hq_city` from
//! [`super::HQ_MAX_PEOPLE`] / [`super::HQ_MAX_CITY`] / [`super::HQ_CITY_ROOM`],
//! and [`super::room_gate`] answers "HQ presence lives in an HQ room" for an hq
//! frame outside an `hq_` room, BEFORE any room state is created.
//!
//! DIVERGENCES FROM PYTHON, beyond the shared list in `super`'s module doc:
//!   1. `a` (the walk flag) is kept as a RAW JSON value rather than an integer,
//!      because Python's membership test `a not in (0, 1, 2)` is `==` and
//!      `True == 1`, so `a: true` passes and is then re-sent verbatim as
//!      `true`. An `i64` would quietly turn that into `1`. See [`anim_ok`].
//!   2. hqpresence.py's `MAX_PEOPLE`, `CITY_ROOM` and `MAX_CITY` are NOT
//!      redeclared here: they already live in valley.rs beside [`super::cap`],
//!      which is their only caller, and two copies of a cap is how a cap drifts.
//!      valley.rs:203 says it means to hand them over; that is the integrator's
//!      move, not this file's.
//!   3. `_hq_tick_on`'s `v is None or room is None` half is
//!      [`super::ValleyHub::step`]'s job in this port, so [`step`] tests only
//!      what is left of that line: "nobody is in the HQ any more".

use super::{chars, num_in, Ctx, Left, Out, RoomValley, Seq, StepFn, Tick, To, I_HQ};
use serde_json::{json, Map, Value};
use std::sync::Arc;

/// The game key on the wire.
pub const GAME: &str = "hq";

/// hqpresence.py:13: snapshots a second, while anyone is in the room.
pub const HZ: f64 = 8.0;

/// hqpresence.py:17, in order. A `w` outside this list drops the whole frame.
pub const FLOORS: [&str; 4] = ["base", "lobby", "mission", "city"];

/// hqpresence.py:18: inbound position frames a second per person, 24 deep.
/// Clients send 8 (ui/app/22-hq.js:402 runs every 125 ms), so the headroom is
/// for jitter, not for a flood.
pub const POS_RATE: f64 = 12.0;
pub const POS_BURST: f64 = 24.0;

/// hqpresence.py:19: resend the list this often even when nobody moved, so a
/// page that joined mid-stillness is not left with an empty street.
pub const KEEPALIVE: f64 = 2.0;

/// The cut on a name tag, in CHARACTERS (hqpresence.py:78's `[:24]`).
const NAME_CHARS: usize = 24;

/// One person standing somewhere. Python's entry in `Presence.people`
/// (hqpresence.py:48).
struct Person {
    /// `member.public()` AS IT WAS WHEN THEY FIRST ENTERED. Python's `enter`
    /// only builds the record when `p is None`, so a rejoin with new cosmetics
    /// does NOT refresh it -- the name tag and the frame keep the values from
    /// the first join of this room session. Faithful, and pinned by a test.
    user: Value,
    w: String,
    x: i64,
    z: i64,
    r: i64,
    /// Raw, for the `True == 1` reason in divergence 1.
    a: Value,
    /// Their 3D character (the `look` op, "look" capability): a short array of
    /// small integers the page turns into a model, colours and add-ons. None
    /// until they send one; older pages never do, and get the hashed look.
    look: Option<Vec<u8>>,
    /// On a skateboard (a = 4, the "skate" capability): the trick they are doing
    /// (`k`, 0..=SKATE_K_MAX) and how high they are (`y`, decimetres, 0..=SKATE_Y_MAX),
    /// so others see the board flip and the air. None whenever a is not 4.
    skate: Option<(u8, u8)>,
    /// (tokens, when) -- Python's `[POS_BURST, t]` list.
    bucket: (f64, f64),
}

/// Per-room state. One field of [`RoomValley`], in Python's `__init__` order.
/// Python's `Presence` (hqpresence.py:39).
#[derive(Default)]
pub struct Hq {
    /// user_id -> where they stand, IN ENTRY ORDER, because that order is the
    /// order of the `ps` array on the wire. A [`Seq`], never a std map.
    people: Seq<Person>,
    /// Somebody moved, entered or left since the last snapshot.
    dirty: bool,
    /// When the last snapshot went out. Python starts it at 0.0, so the first
    /// tick of a fresh room is always past [`KEEPALIVE`] -- harmless, because
    /// `enter` has already set `dirty`.
    sent_at: f64,
}

impl Hq {
    /// hqpresence.py:45. Note `dirty` is set even for someone already here:
    /// a second socket's join still earns everyone a fresh snapshot.
    fn enter(&mut self, uid: &str, pubv: Value, t: f64) {
        if self.people.get(uid).is_none() {
            self.people.set(uid, Person {
                user: pubv,
                // hqpresence.py:48: you start outside, 14 m up the path, facing
                // the door.
                w: "base".to_string(),
                x: 0,
                z: 1400,
                r: 0,
                a: json!(0),
                look: None,
                skate: None,
                bucket: (POS_BURST, t),
            });
        }
        self.dirty = true;
    }

    /// hqpresence.py:52. False (and NOT dirty) for someone who was not here.
    fn drop_person(&mut self, uid: &str) -> bool {
        if self.people.remove(uid).is_none() {
            return false;
        }
        self.dirty = true;
        true
    }

    /// hqpresence.py:58: spend a token, then take the frame whole or drop it
    /// whole. Order is load-bearing twice over:
    ///   - the token is spent BEFORE the fields are checked, so a flood of
    ///     nonsense costs the sender its budget exactly as a flood of good
    ///     frames would (hqpresence.py:67 precedes :71);
    ///   - every field is parsed before any is tested, and one bad field drops
    ///     the whole frame, so you cannot move in x while sending a junk z.
    fn pos(&mut self, uid: &str, msg: &Value, t: f64) -> bool {
        let Some(p) = self.people.get_mut(uid) else { return false };
        p.bucket.0 = POS_BURST.min(p.bucket.0 + (t - p.bucket.1) * POS_RATE);
        p.bucket.1 = t;
        if p.bucket.0 < 1.0 {
            return false;
        }
        p.bucket.0 -= 1.0;
        let w = msg.get("w").and_then(Value::as_str);
        // `num_in`, which REJECTS out of range -- not `as_num`, which clamps.
        // An x of 10^9 must be dropped whole (test_hq_presence.py:44), not
        // pinned to the wall at 6000.
        let x = num_in(msg.get("x"), -6000.0, 6000.0);
        let z = num_in(msg.get("z"), -6000.0, 6000.0);
        let r = num_in(msg.get("r"), -720.0, 720.0);
        // Python's `msg.get("a", 0)`: an ABSENT key defaults to 0, an explicit
        // null does not and fails the membership test below.
        let a = msg.get("a").cloned().unwrap_or_else(|| json!(0));
        let (Some(w), Some(x), Some(z), Some(r)) = (w, x, z, r) else { return false };
        if !FLOORS.contains(&w) || !anim_ok(&a) {
            return false;
        }
        // On a board, its trick and height ride along: absent means 0, and a bad
        // one drops the whole frame like any other field. Off a board both are
        // ignored (an older page never sends them).
        let skate = if a.as_f64() == Some(4.0) {
            let (Some(k), Some(y)) = (small_int(msg.get("k"), SKATE_K_MAX), small_int(msg.get("y"), SKATE_Y_MAX)) else { return false };
            Some((k, y))
        } else {
            None
        };
        p.w = w.to_string();
        p.x = super::py_trunc(x); // Python's int(x): toward zero
        p.z = super::py_trunc(z);
        p.r = super::deg360(r); // int(r) % 360, so -90 is 270
        p.a = a;
        p.skate = skate;
        self.dirty = true;
        true
    }

    /// The `look` op: spend a token like `pos`, then keep the character only
    /// if it is an array of 1..=LOOK_LEN whole numbers in 0..=LOOK_MAX. A bad one
    /// is dropped whole and changes nothing.
    fn look(&mut self, uid: &str, msg: &Value, t: f64) -> bool {
        let Some(p) = self.people.get_mut(uid) else { return false };
        p.bucket.0 = POS_BURST.min(p.bucket.0 + (t - p.bucket.1) * POS_RATE);
        p.bucket.1 = t;
        if p.bucket.0 < 1.0 {
            return false;
        }
        p.bucket.0 -= 1.0;
        let Some(c) = read_look(msg.get("c")) else { return false };
        if p.look.as_ref() != Some(&c) {
            p.look = Some(c);
            self.dirty = true;
        }
        true
    }

    /// hqpresence.py:77: the whole street, one object per person, keys in
    /// Python's order -- `u`, `n`, `w`, `x`, `z`, `r`, `a`, and `f` LAST and
    /// only when they wear one.
    fn listing(&self) -> Value {
        Value::Array(
            self.people
                .iter()
                .map(|(uid, p)| {
                    let mut m = Map::new();
                    m.insert("u".into(), json!(uid));
                    m.insert("n".into(), json!(name_of(&p.user)));
                    m.insert("w".into(), json!(p.w));
                    m.insert("x".into(), json!(p.x));
                    m.insert("z".into(), json!(p.z));
                    m.insert("r".into(), json!(p.r));
                    m.insert("a".into(), p.a.clone());
                    if let Some(f) = frame_of(&p.user) {
                        m.insert("f".into(), json!(f));
                    }
                    if let Some(c) = &p.look {
                        m.insert("c".into(), json!(c));
                    }
                    // only for someone on a board, so every other listing is unchanged
                    if let Some((k, y)) = p.skate {
                        m.insert("k".into(), json!(k));
                        m.insert("y".into(), json!(y));
                    }
                    Value::Object(m)
                })
                .collect(),
        )
    }
}

/// Python's `a not in (0, 1, 2)`, inverted. That is `==`, not `isinstance`, and
/// in Python `True == 1` and `False == 0`, so a JSON bool PASSES and is stored
/// and re-sent as a bool (divergence 1). `2.0` passes too, for the same reason.
/// A string, an array, an object and an explicit `null` do not.
fn anim_ok(a: &Value) -> bool {
    match a {
        Value::Bool(_) => true,
        // 0 still, 1 walk, 2 run -- and 3, riding a bike in Arena City (the "ride"
        // capability in protocol.rs; a page only sends 3 to an Arena that lists it),
        // 4, on a skateboard (the "skate" capability, likewise), and 5 / 6, driving a
        // car / riding a motorbike (the "drive" capability).
        Value::Number(n) => n.as_f64().is_some_and(|f| [0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0].contains(&f)),
        _ => false,
    }
}

/// A skater's trick id (games/hqskate.js `K`) and height (decimetres) top out here.
const SKATE_K_MAX: u8 = 15;
const SKATE_Y_MAX: u8 = 60;

/// A whole number in 0..=max, or 0 when absent; None (drop the frame) for
/// anything else -- a fraction, a bool, a string, out of range.
fn small_int(v: Option<&Value>, max: u8) -> Option<u8> {
    let Some(v) = v else { return Some(0) };
    let f = super::num_in(Some(v), 0.0, f64::from(max))?;
    (f.fract() == 0.0).then_some(f as u8)
}

/// The longest character array and the largest value in it (the page's spec is
/// ten small numbers today; room to grow, append-only).
pub const LOOK_LEN: usize = 16;
pub const LOOK_MAX: u64 = 31;

fn read_look(v: Option<&Value>) -> Option<Vec<u8>> {
    let a = v?.as_array()?;
    if a.is_empty() || a.len() > LOOK_LEN {
        return None;
    }
    a.iter().map(|x| x.as_u64().filter(|n| *n <= LOOK_MAX).map(|n| n as u8)).collect()
}

/// hqpresence.py:78's `(displayName or handle or "")[:24]`. An `or` chain, so an
/// EMPTY display name falls through to the handle -- not just a missing one.
/// Cut in CHARACTERS: display names are user-supplied and may be non-ASCII.
fn name_of(pubv: &Value) -> String {
    let pick = |k: &str| pubv.get(k).and_then(Value::as_str).filter(|s| !s.is_empty());
    chars(pick("displayName").or_else(|| pick("handle")).unwrap_or(""), NAME_CHARS)
}

/// hqpresence.py:33: the name frame they wear (a colour), so everyone draws
/// their name tag in it. `(pub.get("cos") or {}).get("frame")`, kept only when
/// it is a string of exactly 7 CHARACTERS starting with `#` -- the resolved hex
/// the cosmetics layer puts in `cos`, never the slot id.
fn frame_of(pubv: &Value) -> Option<&str> {
    let f = pubv.get("cos")?.get("frame")?.as_str()?;
    (f.chars().count() == 7 && f.starts_with('#')).then_some(f)
}

/// Python's `_hq_tick_on`'s inner `step` (valley.py:1275). The `v is None or
/// room is None` half of its first line is the shared loop's (divergence 3);
/// what is left is "nobody is in the HQ any more", which stops the loop.
///
/// It HONOURS `send`, unlike type: a tick the room's bandwidth budget thinned
/// out does nothing at all and does not touch `dirty`, so the snapshot it
/// skipped goes out on the next tick that is allowed to send. `charge` is None
/// because Python never calls `tk.count(..)` here, so these snapshots sit
/// outside `ROOM_BYTES_PER_SEC` and are never themselves thinned.
fn step(v: &mut RoomValley, t: f64, send: bool) -> Tick {
    if v.hq.people.is_empty() {
        return Tick { keep: false, items: Vec::new(), charge: None };
    }
    let mut items = Vec::new();
    if send && (v.hq.dirty || t - v.hq.sent_at >= KEEPALIVE) {
        v.hq.dirty = false;
        v.hq.sent_at = t;
        items.push((To::Users(v.lobbies[I_HQ].ids(), None), "snap".to_string(),
                    json!({"ps": v.hq.listing()})));
    }
    Tick { keep: true, items, charge: None }
}

/// Python's `_hq_tick_on(room.room_id)` (valley.py:1070). The return value is
/// IGNORED, exactly as Python's join arm ignores it: at
/// `realtime::MAX_TICKERS` the joiner keeps their one `snap` and then hears
/// nothing, where type turns the same refusal into "the Arena is busy right
/// now". Faithful, and the reason this returns a bool nobody reads.
fn tick_on(cx: &Ctx) -> bool {
    let s: StepFn = Arc::new(step);
    cx.hub.tick_on(GAME, cx.room_id, HZ, s)
}

/// The join snapshot: Python's hq tail of the join arm (valley.py:1067-1070) --
/// enter, answer this one socket with the whole street, start the room's loop.
pub fn joined(v: &mut RoomValley, cx: &mut Ctx, out: &mut Out) {
    v.hq.enter(cx.uid(), cx.me(), cx.t);
    out.to(cx.conn, "snap", json!({"ps": v.hq.listing()}));
    tick_on(cx);
}

/// This game's ops (valley.py:1136-1138): `pos`, and nothing else. It sends
/// NOTHING, accepted or rejected -- the answer is the loop's next snapshot, and
/// Python likewise throws away `pos`'s return value.
pub fn op(v: &mut RoomValley, cx: &mut Ctx, op: &str, msg: &Value, out: &mut Out) {
    let _ = out; // hq answers a position frame with silence, never an event
    if op == "pos" {
        v.hq.pos(cx.uid(), msg, cx.t);
    } else if op == "look" {
        v.hq.look(cx.uid(), msg, cx.t);
    }
}

/// Someone left this lobby, by `leave` or by a dropped socket. Python's whole
/// branch is `v.hq.drop(user_id)` (valley.py:1558): no event of its own, and
/// [`Left::Notice`], so the shared `lobby` event carries them in `left`.
/// Everyone else learns they are gone from the next snapshot, which the drop
/// made dirty.
///
/// `t` and `disconnected` are unused ON PURPOSE: presence has no grace and no
/// blip concept, so a dropped socket and an explicit leave are the same thing
/// here -- unlike kart, plat, fps and golf, which all hold a seat.
///
/// The loop is NOT stopped here either. [`step`] stops itself on the first tick
/// after the last person leaves, which is what Python does; calling
/// `tick_off` would also be wrong from inside a leave triggered BY that loop's
/// own room.
pub fn dropped(
    v: &mut RoomValley, uid: &str, who: &Value, out: &mut Out, t: f64, disconnected: bool,
) -> Left {
    let _ = (who, out, t, disconnected);
    v.hq.drop_person(uid);
    Left::Notice
}

#[cfg(test)]
mod tests {
    use super::super::testkit::*;
    use super::super::{cap, room_gate, HQ_CITY_ROOM, HQ_MAX_CITY, HQ_MAX_PEOPLE};
    use super::*;
    use crate::rooms::Member;

    const HQ: &str = "hq_ann";

    // ------------------------------------------------------- the catalogues --

    #[test]
    fn the_rates_and_the_floors_are_pythons() {
        // hqpresence.py:13-19, byte for byte and in order.
        assert_eq!(HZ, 8.0);
        assert_eq!(FLOORS, ["base", "lobby", "mission", "city"]);
        assert_eq!(POS_RATE, 12.0);
        assert_eq!(POS_BURST, 24.0);
        assert_eq!(KEEPALIVE, 2.0);
        assert_eq!(NAME_CHARS, 24);
        // The three caps live in valley.rs beside their only caller
        // (divergence 2); pinned here so this file notices if they move.
        assert_eq!(HQ_MAX_PEOPLE, 16);
        assert_eq!(HQ_MAX_CITY, 40);
        assert_eq!(HQ_CITY_ROOM, "hq_city");
        assert_eq!(cap(GAME, HQ), 16);
        assert_eq!(cap(GAME, HQ_CITY_ROOM), 40);
    }

    #[test]
    fn hq_emits_no_error_of_its_own_so_the_dispatcher_owns_both_strings() {
        // Byte for byte: valley.py:1009 and :1026.
        assert_eq!(room_gate(GAME, "lobby"), Some("HQ presence lives in an HQ room"));
        assert_eq!(room_gate(GAME, HQ), None);
        assert_eq!(room_gate(GAME, HQ_CITY_ROOM), None); // a prefix test
        // A rejected `pos` produces no message at all.
        let mut v = RoomValley::new(HQ);
        v.hq.enter("u", json!({"handle": "u"}), 0.0);
        let mut out = Out::new(GAME);
        assert!(!v.hq.pos("u", &json!({"w": "roof", "x": 0, "z": 0, "r": 0}), 0.1));
        assert!(out.items.is_empty());
        out.err(1, "unused"); // the only way an hq Out ever gets an error
        assert_eq!(out.items[0].1["error"], "unused");
    }

    // ------------------------------------------------------ the position rule --

    /// test_hq_presence.py:69's `test_position_rules`, line for line.
    #[test]
    fn a_position_is_stored_only_when_every_field_passes() {
        let mut p = Hq::default();
        p.enter("u", json!({"handle": "u"}), 0.0);
        assert!(p.pos("u", &json!({"w": "mission", "x": 100, "z": -200, "r": -90, "a": 2}), 0.1));
        assert_eq!(p.listing()[0]["r"], 270); // int(-90) % 360, not Rust's -90
        // a bool x is not a number in Python either
        assert!(!p.pos("u", &json!({"w": "mission", "x": true, "z": 0, "r": 0}), 0.2));
        // a is 7, which is not in (0, 1, 2)
        assert!(!p.pos("u", &json!({"w": "mission", "x": 0, "z": 0, "r": 0, "a": 7}), 0.3));
        // nobody by that name is standing anywhere
        assert!(!p.pos("nobody", &json!({"w": "base", "x": 0, "z": 0, "r": 0}), 0.4));
        // Every rejection above left the stored position alone.
        let first = &p.listing()[0];
        assert_eq!((&first["w"], &first["x"], &first["z"], &first["a"]),
                   (&json!("mission"), &json!(100), &json!(-200), &json!(2)));
    }

    #[test]
    fn a_look_is_kept_only_when_valid_and_rides_in_the_listing() {
        let mut p = Hq::default();
        p.enter("u", json!({"handle": "u"}), 0.0);
        // No look yet: the listing carries no `c`, as for an older page.
        assert!(p.listing()[0].get("c").is_none());
        assert!(p.look("u", &json!({"c": [2, 7, 9, 9, 2, 5, 3, 1, 4, 1]}), 0.1));
        assert_eq!(p.listing()[0]["c"], json!([2, 7, 9, 9, 2, 5, 3, 1, 4, 1]));
        p.dirty = false;
        for bad in [json!({"c": []}), json!({"c": [32]}), json!({"c": [-1]}), json!({"c": [1.5]}),
                    json!({"c": "1,2"}), json!({"c": vec![1; 17]}), json!({}), json!({"c": [true]})] {
            assert!(!p.look("u", &bad, 1.0), "accepted {bad}");
        }
        assert!(!p.dirty);
        assert_eq!(p.listing()[0]["c"][1], 7);   // unchanged by the rejects
        // The same look again is not news.
        assert!(p.look("u", &json!({"c": [2, 7, 9, 9, 2, 5, 3, 1, 4, 1]}), 2.0));
        assert!(!p.dirty);
        assert!(!p.look("nobody", &json!({"c": [1]}), 2.0));
    }

    #[test]
    fn a_skater_carries_a_trick_and_a_height_and_nobody_else_does() {
        let mut p = Hq::default();
        p.enter("u", json!({"handle": "u"}), 0.0);
        // Walking: k and y are ignored, and the listing has neither.
        assert!(p.pos("u", &json!({"w": "city", "x": 0, "z": 0, "r": 0, "a": 1, "k": 2, "y": 9}), 0.1));
        assert!(p.listing()[0].get("k").is_none() && p.listing()[0].get("y").is_none());
        // On a board: both ride in the listing, after everything else.
        assert!(p.pos("u", &json!({"w": "city", "x": 0, "z": 0, "r": 0, "a": 4, "k": 2, "y": 21}), 0.2));
        let l = p.listing();
        assert_eq!((l[0]["a"].clone(), l[0]["k"].clone(), l[0]["y"].clone()), (json!(4), json!(2), json!(21)));
        let keys: Vec<&String> = l[0].as_object().unwrap().keys().collect();
        assert_eq!(keys.last().map(|k| k.as_str()), Some("y"));
        // Absent means 0 (rolling, on the ground).
        assert!(p.pos("u", &json!({"w": "city", "x": 0, "z": 0, "r": 0, "a": 4}), 0.3));
        assert_eq!((p.listing()[0]["k"].clone(), p.listing()[0]["y"].clone()), (json!(0), json!(0)));
        // A bad trick or height drops the whole frame: the position does not move either.
        for bad in [json!({"k": 16}), json!({"k": -1}), json!({"k": 1.5}), json!({"k": true}), json!({"k": "2"}),
                    json!({"y": 61}), json!({"y": -1}), json!({"y": 0.5}), json!({"y": null})] {
            let mut m = json!({"w": "city", "x": 500, "z": 0, "r": 0, "a": 4});
            for (k, v) in bad.as_object().unwrap() { m[k] = v.clone(); }
            assert!(!p.pos("u", &m, 1.0), "accepted {m}");
        }
        assert_eq!(p.listing()[0]["x"], json!(0));
        // Stepping off clears them.
        assert!(p.pos("u", &json!({"w": "city", "x": 0, "z": 0, "r": 0, "a": 0}), 2.0));
        assert!(p.listing()[0].get("k").is_none());
    }

    #[test]
    fn a_bad_floor_or_a_missing_field_drops_the_whole_frame() {
        let mut p = Hq::default();
        p.enter("u", json!({"handle": "u"}), 0.0);
        let bad = [
            json!({"w": "roof", "x": 1, "z": 1, "r": 0}),          // not a floor
            json!({"x": 1, "z": 1, "r": 0}),                       // no floor at all
            json!({"w": 1, "x": 1, "z": 1, "r": 0}),               // a non-string floor
            json!({"w": "base", "x": 1e9, "z": 0, "r": 0}),        // rejected, NOT clamped
            json!({"w": "base", "x": -6000.5, "z": 0, "r": 0}),    // just outside
            json!({"w": "base", "x": "1", "z": 0, "r": 0}),        // a string is not a number
            json!({"w": "base", "z": 0, "r": 0}),                  // no x
            json!({"w": "base", "x": 0, "r": 0}),                  // no z
            json!({"w": "base", "x": 0, "z": 0}),                  // no r
            json!({"w": "base", "x": 0, "z": 0, "r": 721}),        // r out of range
            json!({"w": "base", "x": 0, "z": 0, "r": 0, "a": null}), // explicit null is not 0
            json!({"w": "base", "x": 0, "z": 0, "r": 0, "a": "1"}),
        ];
        for m in &bad {
            assert!(!p.pos("u", m, 10.0), "accepted {m}");
        }
        // Still on the doorstep, where `enter` put them, and nothing is dirty.
        p.dirty = false;
        assert!(!p.pos("u", &bad[0], 11.0));
        assert!(!p.dirty);
        assert_eq!(p.listing()[0], json!({"u": "u", "n": "u", "w": "base", "x": 0, "z": 1400,
                                          "r": 0, "a": 0}));
    }

    #[test]
    fn the_edges_of_every_range_are_inclusive() {
        let mut p = Hq::default();
        p.enter("u", json!({"handle": "u"}), 0.0);
        assert!(p.pos("u", &json!({"w": "city", "x": -6000, "z": 6000, "r": -720, "a": 1}), 0.1));
        assert_eq!(p.listing()[0], json!({"u": "u", "n": "u", "w": "city", "x": -6000,
                                          "z": 6000, "r": 0, "a": 1})); // -720 % 360 == 0
        // int() truncates toward zero, so -0.9 cm is 0 and not -1.
        assert!(p.pos("u", &json!({"w": "base", "x": -0.9, "z": 0.9, "r": 359.9}), 0.2));
        let l = &p.listing()[0];
        assert_eq!((&l["x"], &l["z"], &l["r"], &l["a"]), (&json!(0), &json!(0), &json!(359),
                                                          &json!(0)));
    }

    /// Divergence 1: Python's `a not in (0, 1, 2)` is `==`, so `True` is `1`.
    #[test]
    fn the_walk_flag_is_compared_by_value_the_way_python_compares_it() {
        let mut p = Hq::default();
        p.enter("u", json!({"handle": "u"}), 0.0);
        for (a, keep) in [(json!(0), true), (json!(1), true), (json!(2), true),
                          (json!(2.0), true), (json!(true), true), (json!(false), true),
                          (json!(3), true), (json!(4), true), (json!(5), true), (json!(6), true), (json!(7), false),
                          (json!(-1), false), (json!(1.5), false),
                          (json!("1"), false), (json!([1]), false)] {
            let m = json!({"w": "base", "x": 0, "z": 0, "r": 0, "a": a});
            assert_eq!(p.pos("u", &m, 1.0), keep, "a = {a}");
        }
        // And a bool reaches the wire AS A BOOL, which an i64 field would have
        // turned into 1.
        assert!(p.pos("u", &json!({"w": "base", "x": 0, "z": 0, "r": 0, "a": true}), 2.0));
        assert_eq!(p.listing()[0]["a"], json!(true));
    }

    // ------------------------------------------------------------ the bucket --

    #[test]
    fn the_burst_runs_dry_and_a_nonsense_frame_costs_a_token_too() {
        let mut p = Hq::default();
        p.enter("u", json!({"handle": "u"}), 0.0);
        let good = json!({"w": "base", "x": 1, "z": 2, "r": 3, "a": 0});
        // 24 tokens at entry, no time passing, so exactly 24 go through.
        for i in 0..24 {
            assert!(p.pos("u", &good, 0.0), "frame {i}");
        }
        assert!(!p.pos("u", &good, 0.0));
        // A frame that fails every rule still spends one: the limiter runs
        // before the parser (hqpresence.py:67 before :71).
        let junk = json!({"w": "roof"});
        p.people.get_mut("u").unwrap().bucket.0 = 1.0;
        assert!(!p.pos("u", &junk, 0.0));
        assert!(!p.pos("u", &good, 0.0)); // the junk ate the last token
        // 12 a second, so half a second buys exactly six back.
        for i in 0..6 {
            assert!(p.pos("u", &good, 0.5), "refilled frame {i}");
        }
        assert!(!p.pos("u", &good, 0.5));
        // The refill is capped at the burst however long you wait.
        assert!(p.pos("u", &good, 1_000.0));
        assert_eq!(p.people.get("u").unwrap().bucket.0, POS_BURST - 1.0);
    }

    // -------------------------------------------------------- the name tag --

    #[test]
    fn a_name_falls_back_to_the_handle_and_is_cut_at_twenty_four_characters() {
        let name = |pubv: Value| {
            let mut p = Hq::default();
            p.enter("u", pubv, 0.0);
            p.listing()[0]["n"].as_str().unwrap().to_string()
        };
        assert_eq!(name(json!({"displayName": "Bob", "handle": "bob"})), "Bob");
        // An EMPTY display name is falsy in Python, so the `or` chain goes on.
        assert_eq!(name(json!({"displayName": "", "handle": "bob"})), "bob");
        assert_eq!(name(json!({"handle": "bob"})), "bob");
        assert_eq!(name(json!({})), "");
        assert_eq!(name(json!({"displayName": "", "handle": ""})), "");
        // Twenty-four CHARACTERS, never bytes: 30 of these are 60 bytes.
        assert_eq!(name(json!({"displayName": "é".repeat(30)})), "é".repeat(24));
        assert_eq!(name(json!({"displayName": "a".repeat(30)})), "a".repeat(24));
    }

    #[test]
    fn only_a_seven_character_hash_colour_is_worn_as_a_frame() {
        let has_f = |cos: Value| {
            let mut p = Hq::default();
            let mut pubv = json!({"handle": "u"});
            if !cos.is_null() {
                pubv["cos"] = cos;
            }
            p.enter("u", pubv, 0.0);
            p.listing()[0].get("f").cloned()
        };
        // test_hq_presence.py:93: a worn frame resolves to a hex before it gets
        // here, and the key is ABSENT for anyone not wearing one.
        assert_eq!(has_f(json!({"frame": "#d8b34a"})), Some(json!("#d8b34a")));
        assert_eq!(has_f(json!({"frame": "#d8b34"})), None); // six, not seven
        assert_eq!(has_f(json!({"frame": "#d8b34aa"})), None);
        assert_eq!(has_f(json!({"frame": "d8b34ab"})), None); // no leading hash
        assert_eq!(has_f(json!({"frame": "f-brass"})), None); // the slot id, not a colour
        assert_eq!(has_f(json!({"frame": 7})), None);
        assert_eq!(has_f(json!({"badge": "#d8b34a"})), None);
        assert_eq!(has_f(json!({})), None);
        assert_eq!(has_f(Value::Null), None); // no cos key at all
    }

    // ---------------------------------------------------------- the listing --

    #[test]
    fn the_snapshot_keys_are_pythons_in_pythons_order() {
        let mut p = Hq::default();
        p.enter("u", json!({"displayName": "Ann", "cos": {"frame": "#d8b34a"}}), 0.0);
        assert!(p.pos("u", &json!({"w": "lobby", "x": -1250, "z": 300, "r": 90, "a": 1}), 0.1));
        // hqpresence.py:78-80: u, n, w, x, z, r, a, then f LAST.
        assert_eq!(p.listing()[0].to_string(),
                   r##"{"u":"u","n":"Ann","w":"lobby","x":-1250,"z":300,"r":90,"a":1,"f":"#d8b34a"}"##);
    }

    #[test]
    fn the_listing_is_in_entry_order_and_entering_twice_changes_nothing() {
        let mut p = Hq::default();
        p.enter("a", json!({"handle": "a"}), 0.0);
        p.enter("b", json!({"handle": "b"}), 0.0);
        p.enter("c", json!({"handle": "c"}), 0.0);
        assert!(p.pos("b", &json!({"w": "city", "x": 5, "z": 5, "r": 0}), 0.1));
        // Python's `enter` builds the record only `if p is None`, so a second
        // join keeps the first profile AND the position already walked to ...
        p.enter("b", json!({"handle": "b", "displayName": "Bee"}), 0.2);
        let l = p.listing();
        assert_eq!(l.as_array().unwrap().iter().map(|e| e["u"].as_str().unwrap())
                       .collect::<Vec<_>>(),
                   ["a", "b", "c"]); // and their place in the order
        assert_eq!(l[1]["n"], "b"); // not "Bee"
        assert_eq!(l[1]["x"], 5);
        // ... but it still earns everyone a snapshot.
        assert!(p.dirty);
    }

    #[test]
    fn dropping_someone_who_is_not_here_changes_nothing() {
        let mut p = Hq::default();
        p.enter("a", json!({"handle": "a"}), 0.0);
        p.dirty = false;
        assert!(!p.drop_person("b"));
        assert!(!p.dirty); // Python returns False before touching dirty
        assert!(p.drop_person("a"));
        assert!(p.dirty);
        assert_eq!(p.listing(), json!([]));
    }

    // --------------------------------------------------------------- the loop --

    fn with_one(uid: &str) -> RoomValley {
        let mut v = RoomValley::new(HQ);
        v.lobbies[I_HQ].put(uid, json!({"userId": uid, "handle": uid}));
        v.hq.enter(uid, json!({"handle": uid}), 1000.0);
        v
    }

    #[test]
    fn a_tick_sends_a_snapshot_only_when_something_changed() {
        let mut v = with_one("a");
        // enter() set dirty, so the first tick sends and clears it.
        let r = step(&mut v, 1000.0, true);
        assert!(r.keep);
        assert_eq!(r.items.len(), 1);
        assert_eq!(r.items[0].1, "snap");
        assert_eq!(r.items[0].2["ps"][0]["u"], "a");
        assert!(r.charge.is_none()); // hq never calls tk.count
        assert!(!v.hq.dirty);
        assert_eq!(v.hq.sent_at, 1000.0);
        // Nobody moved and the keepalive has not come round: a quiet tick.
        let r = step(&mut v, 1001.9, true);
        assert!(r.keep && r.items.is_empty());
        // KEEPALIVE is 2.0 and the test is `>=`, so exactly 2 s later it resends.
        let r = step(&mut v, 1002.0, true);
        assert_eq!(r.items.len(), 1);
        assert_eq!(v.hq.sent_at, 1002.0);
        // A move makes it dirty again at once.
        assert!(v.hq.pos("a", &json!({"w": "lobby", "x": 1, "z": 2, "r": 3}), 1002.1));
        assert_eq!(step(&mut v, 1002.1, true).items.len(), 1);
    }

    #[test]
    fn a_thinned_tick_keeps_the_snapshot_for_the_next_one() {
        let mut v = with_one("a");
        // Unlike type, hq HONOURS send: nothing goes out and dirty is kept, so
        // the frame is not lost, only late.
        let r = step(&mut v, 1000.0, false);
        assert!(r.keep && r.items.is_empty());
        assert!(v.hq.dirty);
        assert_eq!(v.hq.sent_at, 0.0);
        assert_eq!(step(&mut v, 1000.1, true).items.len(), 1);
    }

    #[test]
    fn the_loop_stops_when_the_last_person_leaves() {
        let mut v = with_one("a");
        assert!(step(&mut v, 1000.0, true).keep);
        v.hq.drop_person("a");
        let r = step(&mut v, 1000.1, true);
        assert!(!r.keep); // Python's `not v.hq.people: return False`
        assert!(r.items.is_empty()); // and it says nothing on the way out
    }

    #[test]
    fn a_snapshot_is_addressed_to_the_hq_lobby_and_not_to_the_room() {
        let mut v = with_one("a");
        // Somebody in the room who never sent join is in `people`? No -- but
        // somebody in the LOBBY who is not in `people` is possible (the lobby
        // and the presence are separate maps), and the audience is the LOBBY.
        v.lobbies[I_HQ].put("b", json!({"userId": "b"}));
        let r = step(&mut v, 1000.0, true);
        match &r.items[0].0 {
            To::Users(ids, skip) => {
                assert_eq!(ids, &["a".to_string(), "b".to_string()]);
                assert!(skip.is_none());
            }
            other => panic!("a ticked snap must go to the lobby, not {other:?}"),
        }
    }

    // ------------------------------------------------- through the dispatch --

    fn renamed(m: &Member, display_name: &str, cos: Value) -> Member {
        let mut m = m.clone();
        m.display_name = display_name.to_string();
        m.cos = cos;
        m
    }

    /// test_hq_presence.py:27's `test_visitors_see_each_other_move`, end to end
    /// through the real dispatcher and the real 8 Hz loop.
    #[tokio::test]
    async fn visitors_see_each_other_move() {
        let e = env(4, 1);
        let (a, mut wa) = e.connect(HQ, 1, "ann").await;
        let (b0, _wb) = e.connect(HQ, 2, "bob").await;
        let b = renamed(&b0, "Bob", Value::Null);
        e.send(HQ, 1, &a, GAME, "join", json!({})).await;
        let first = until(&mut wa, GAME, "snap").await;
        assert_eq!(first["ps"].as_array().unwrap().iter().map(|p| p["u"].as_str().unwrap())
                       .collect::<Vec<_>>(),
                   ["ann"]);
        e.send(HQ, 2, &b, GAME, "join", json!({})).await;
        e.send(HQ, 2, &b, GAME, "pos",
               json!({"w": "lobby", "x": -1250, "z": 300, "r": 90, "a": 1})).await;
        let snap = until_where(&mut wa, GAME, "snap", |m| {
            m["ps"].as_array().unwrap().iter().any(|p| p["u"] == "bob" && p["w"] == "lobby")
        })
        .await;
        let bob = snap["ps"].as_array().unwrap().iter().find(|p| p["u"] == "bob").unwrap();
        assert_eq!(*bob, json!({"u": "bob", "n": "Bob", "w": "lobby", "x": -1250, "z": 300,
                                "r": 90, "a": 1}));
        // Nonsense is ignored, not stored (test_hq_presence.py:43-45).
        for m in [json!({"w": "roof", "x": 1, "z": 1, "r": 0}),
                  json!({"w": "base", "x": 1e9, "z": 0, "r": 0}),
                  json!({"w": "base", "x": "1", "z": 0, "r": 0})] {
            e.send(HQ, 2, &b, GAME, "pos", m).await;
        }
        e.send(HQ, 2, &b, GAME, "leave", json!({})).await;
        let gone = until_where(&mut wa, GAME, "snap", |m| m["ps"].as_array().unwrap().len() == 1)
            .await;
        assert_eq!(gone["ps"][0]["u"], "ann");
        assert_eq!(gone["ps"][0]["w"], "base"); // ann never moved
        e.hub.tick_off(GAME, HQ);
    }

    /// The module's audience test: a ticked `snap` goes only to the lobby.
    #[tokio::test]
    async fn a_ticked_snapshot_goes_only_to_the_lobby() {
        let e = env(4, 1);
        let (a, mut wa) = e.connect(HQ, 1, "ann").await;
        let (_b, mut wb) = e.connect(HQ, 2, "bob").await; // in the room, never joins hq
        e.send(HQ, 1, &a, GAME, "join", json!({})).await;
        until(&mut wa, GAME, "snap").await; // the join snap, to this socket only
        let _ = drain(&mut wa);
        let _ = drain(&mut wb);
        e.send(HQ, 1, &a, GAME, "pos", json!({"w": "mission", "x": 7, "z": 8, "r": 9})).await;
        let snap = until(&mut wa, GAME, "snap").await;
        assert_eq!(snap["ps"][0]["w"], "mission");
        // bob is in the room but not in the lobby, so nothing reached him.
        assert!(!drain(&mut wb).iter().any(|(g, ev)| g == GAME && ev == "snap"));
        e.hub.tick_off(GAME, HQ);
    }

    /// test_hq_presence.py:79: Arena City is open to everyone, and name tags
    /// wear their frame.
    #[tokio::test]
    async fn arena_city_name_tags_wear_their_frame() {
        let e = env(4, 1);
        let (a, mut wa) = e.connect(HQ_CITY_ROOM, 1, "ann").await;
        let (b0, _wb) = e.connect(HQ_CITY_ROOM, 2, "bob").await;
        let b = renamed(&b0, "Bob", json!({"frame": "#d8b34a"}));
        e.send(HQ_CITY_ROOM, 1, &a, GAME, "join", json!({})).await;
        e.send(HQ_CITY_ROOM, 2, &b, GAME, "join", json!({})).await;
        e.send(HQ_CITY_ROOM, 2, &b, GAME, "pos",
               json!({"w": "city", "x": 500, "z": -2400, "r": 0, "a": 1})).await;
        let snap = until_where(&mut wa, GAME, "snap", |m| {
            m["ps"].as_array().unwrap().iter().any(|p| p["u"] == "bob" && p["w"] == "city")
        })
        .await;
        let ps = snap["ps"].as_array().unwrap();
        assert_eq!(ps.iter().find(|p| p["u"] == "bob").unwrap()["f"], "#d8b34a");
        // ann wears none, so the key is absent rather than null.
        let ann = ps.iter().find(|p| p["u"] == "ann").unwrap();
        assert!(!ann.as_object().unwrap().contains_key("f"));
        e.hub.tick_off(GAME, HQ_CITY_ROOM);
    }

    #[tokio::test]
    async fn a_dropped_socket_leaves_the_street_with_no_event_of_its_own() {
        let e = env(4, 1);
        let (a, mut wa) = e.connect(HQ, 1, "ann").await;
        let (b, _wb) = e.connect(HQ, 2, "bob").await;
        e.send(HQ, 1, &a, GAME, "join", json!({})).await;
        e.send(HQ, 2, &b, GAME, "join", json!({})).await;
        assert_eq!(e.hub.with_room(HQ, |v| v.hq.people.len()), Some(2));
        let _ = drain(&mut wa);
        e.disconnect(HQ, 2, &b).await;
        // hq's own leave branch sends nothing; the shared `lobby` event carries
        // them in `left`, and the next tick drops them from the street.
        let seen = drain(&mut wa);
        assert!(seen.iter().any(|(g, ev)| g == GAME && ev == "lobby"));
        assert!(!seen.iter().any(|(g, ev)| g == GAME && ev == "snap"));
        let gone = until_where(&mut wa, GAME, "snap", |m| m["ps"].as_array().unwrap().len() == 1)
            .await;
        assert_eq!(gone["ps"][0]["u"], "ann");
        e.hub.tick_off(GAME, HQ);
    }

    #[tokio::test]
    async fn an_unknown_op_is_answered_with_silence() {
        let e = env(4, 1);
        let (a, mut wa) = e.connect(HQ, 1, "ann").await;
        e.send(HQ, 1, &a, GAME, "join", json!({})).await;
        until(&mut wa, GAME, "snap").await;
        let _ = drain(&mut wa);
        e.send(HQ, 1, &a, GAME, "teleport", json!({"w": "mission"})).await;
        // Python's if/elif chain has no else, so an op hq does not know produces
        // nothing at all -- not an error, and no state change.
        assert!(drain(&mut wa).is_empty());
        assert_eq!(e.hub.with_room(HQ, |v| v.hq.people.get("ann").unwrap().w.clone()),
                   Some("base".to_string()));
        e.hub.tick_off(GAME, HQ);
    }

    #[tokio::test]
    async fn a_pos_without_a_seat_is_refused_by_the_dispatcher() {
        let e = env(4, 1);
        let (a, mut wa) = e.connect(HQ, 1, "ann").await;
        e.send(HQ, 1, &a, GAME, "pos", json!({"w": "base", "x": 0, "z": 0, "r": 0})).await;
        assert_eq!(until(&mut wa, GAME, "error").await["error"], "join the lobby first");
        assert_eq!(e.hub.with_room(HQ, |v| v.hq.people.len()), Some(0));
    }
}
