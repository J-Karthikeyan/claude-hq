/* Valley: Strikezone. A team shooter (amber vs cyan) on a tight harbour dockyard:
 * container lanes, a warehouse with a doorway chokepoint, catwalks along the walls.
 * The host picks the side size (X vs X); when a side is short of players the Arena
 * server fills it with bots that patrol, chase and shoot.
 *
 * Rendering is the vendored three.js r186 (games/vendor/), imported only when this game
 * opens; without WebGL2 the same match runs as a top-down map. Players are Kenney's Mini
 * Characters (games/golf/); the arena blocks are Kenney's CC0 Starter Kit FPS pieces
 * (games/fps/), recoloured per box for the dockyard. An original re-implementation in
 * the spirit of the kit's (no Godot code is used).
 *
 * Multiplayer: you run and aim here (your own movement is predicted, your shots show at
 * once) and send where you are ~20 times a second; the Arena server (backend/app/strike.py)
 * runs a 20 Hz tick that checks every move and judges every shot with lag compensation.
 * Team-mates can't be hit. The team score is any member's kill; the round ends at the
 * kill limit, the clock, or when a side empties. Only numbers, a weapon and a character
 * id travel; nothing transcript-derived.
 *
 * The STRIKE-SHARED block mirrors backend/app/strike.py (tests/test_strike_sync.py runs
 * both under node and checks they agree).
 */
(function(){
"use strict";
var HQV = window.HQV; if(!HQV || !HQV.api || !HQV.engine) return;
// Shared helpers, three.js loading and snapshot timing: games/engine.js.
var E = HQV.engine;
var now = E.now, clamp = E.clamp, hex = E.hex, angLerp = E.angLerp, calm = E.calm, say = E.say,
    tokens = E.tokens, myClock = E.myClock, hasWebGL2 = E.hasWebGL2;
var api = HQV.api, MP = HQV.mp || null;

/* STRIKE-SHARED BEGIN */
var FS = (function(){
  var R = 0.35, H = 1.8, EYE = 1.6, STEP_H = 0.55, RUN = 6.0, ACCEL = 12.0, AIR_ACCEL = 3.0, GRAVITY = 18.0;
  var SPRINT = 1.35;   // hold Shift (or stick click): RUN × SPRINT while a movement key is down (v3)
  // sprint stamina (client-side; the server only watches raw speed): a full tank covers
  // STAMINA_MAX/STAMINA_DRAIN = 8 s of sprint, drained Shift still moves — barely (SLOW)
  var STAMINA_MAX = 100, STAMINA_DRAIN = 12.5, STAMINA_REGEN = 14, SPRINT_SLOW = 0.015;
  // v4: respawns are parachute drops — a spawn lifts you to this height and you steer a
  // gentle glide down for up to DROP_TIME seconds (the Arena too accepts "drop" hints and
  // relaxes its action-air guard while a drop still has legs)
  var DROP_HEIGHT = 11.0, DROP_TIME = 3.0, DROP_SINK = 2.8, DROP_DRIFT = 4.5;
  var JUMP_V = 6.5, FALL_MAX = 25.0, BODY_R = 0.4, BODY_H = 1.4, HEAD_R = 0.25, HEAD_TOP = 1.9;
  // the wings/leap/speed totems' movement numbers, shared with both Arena referees so
  // the caps they enforce and the physics the browser runs stay the same numbers
  var LEAP_MULT = 3.0, SPEED_MULT = 2.0, WING_TOP = 9.0, WING_RISE = 4.0, WING_SINK = 2.0, WING_DRIFT = 6.0;
  var WEAPONS = [
    {id: "rapid", name: "Rapid smg", interval: 0.1, damage: 12, head: 18, mag: 30, reserve: 90, reload: 1.6,
     spread: 1.2, range: 60.0, auto: true, pack: 60},
    {id: "marksman", name: "Marksman rifle", interval: 0.8, damage: 75, head: 112, mag: 5, reserve: 15, reload: 2.2,
     spread: 0.3, range: 120.0, auto: false, pack: 10},
    {id: "revolver", name: "Revolver", interval: 0.5, damage: 50, head: 75, mag: 6, reserve: 24, reload: 2.2,
     spread: 0.5, range: 70.0, auto: false, pack: 12},
    {id: "launcher", name: "Launcher", interval: 1.2, damage: 120, head: 120, mag: 1, reserve: 5, reload: 2.5,
     spread: 0.0, range: 60.0, auto: false, pack: 2}];
  // the launcher's rocket: a real missile that flies and blasts — the Arena server flies and
  // judges it in multiplayer (snapshots carry it), the browser flies it solo. All impls agree.
  var LAUNCH = WEAPONS[3];
  var MISSILE_V = 22.0, MISSILE_G = 6.0, MISSILE_TTL = 3.5, BLAST_R = 4.0, DIRECT_R = 0.8;
  // blast damage by distance from the explosion (direct hit / falloff to BLAST_R)
  function blastDamage(dist, direct){
    if(direct) return 120;
    if(dist >= BLAST_R) return 0;
    return Math.round(75 - (75 - 25)*(dist/BLAST_R));
  }
  function compileMap(d){
    d = d || {};
    var boxes = (d.boxes || []).map(function(b){ return [+b[0], +b[1], +b[2], +b[3], +b[4], +b[5], String(b[6] || "block")]; });
    boxes.forEach(function(b){ if(!(b[0] < b[3] && b[1] < b[4] && b[2] < b[5])) throw new Error("box with no volume"); });
    return {id: d.id, name: d.name, bounds: (d.bounds || []).map(Number), boxes: boxes, theme: d.theme || {},
            spawns: (d.spawns || []).map(function(s){ return s.map(Number); }), pickups: d.pickups || [], targets: d.targets || []};
  }
  // index of the first solid box a player standing at (x, y, z) is inside, else -1
  function overlaps(m, x, y, z, shrink){
    shrink = shrink || 0;
    var x0 = x - R + shrink, x1 = x + R - shrink, y0 = y + shrink, y1 = y + H - shrink, z0 = z - R + shrink, z1 = z + R - shrink;
    for(var i = 0; i < m.boxes.length; i++){
      var b = m.boxes[i];
      if(x1 > b[0] && x0 < b[3] && y1 > b[1] && y0 < b[4] && z1 > b[2] && z0 < b[5]) return i;
    }
    return -1;
  }
  function topUnder(m, x, y, z){
    var best = null;
    for(var i = 0; i < m.boxes.length; i++){
      var b = m.boxes[i];
      if(x + R > b[0] && x - R < b[3] && y + H > b[1] && y < b[4] && z + R > b[2] && z - R < b[5] && (best === null || b[4] > best)) best = b[4];
    }
    return best;
  }
  // one step of the runner: s = {x, y, z, vx, vy, vz, g}; (wx, wz) the wished direction.
  // sprint scales the speed you accelerate toward (omitted/false everywhere but the v3 hold).
  // topMult/jumpMult: the speed and leap totems' multipliers (default 1 — every other
  // caller passes nothing, so the shared numbers below stay the referee's word).
  function move(m, s, wx, wz, jump, dt, sprint, topMult, jumpMult){
    var top = RUN*(sprint ? SPRINT : 1)*(topMult || 1);
    var k = Math.min(1, dt*(s.g ? ACCEL : AIR_ACCEL));
    s.vx += (wx*top - s.vx)*k;
    s.vz += (wz*top - s.vz)*k;
    if(jump && s.g){ s.vy = JUMP_V*(jumpMult || 1); s.g = false; }
    s.vy = Math.max(-FALL_MAX, s.vy - GRAVITY*dt);
    ["x", "z"].forEach(function(ax){
      var old = s[ax];
      s[ax] = old + s["v"+ax]*dt;
      var i = overlaps(m, s.x, s.y, s.z, 0);
      if(i >= 0){
        var top = m.boxes[i][4];
        if(s.g && top - s.y > 0 && top - s.y <= STEP_H && overlaps(m, s.x, top, s.z, 0) < 0) s.y = top;
        else { s[ax] = old; s["v"+ax] = 0; }
      }
    });
    var ny = s.y + s.vy*dt;
    if(overlaps(m, s.x, ny, s.z, 0) < 0){ s.y = ny; s.g = false; }
    else if(s.vy <= 0){
      var top = topUnder(m, s.x, ny, s.z);
      if(top !== null && top <= s.y + 1e-9) s.y = top;
      s.vy = 0; s.g = true;
    } else s.vy = 0;
    return s;
  }
  function dirOf(yaw, pitch){
    var y = yaw*Math.PI/180, p = pitch*Math.PI/180, cp = Math.cos(p);
    return [Math.sin(y)*cp, Math.sin(p), -Math.cos(y)*cp];
  }
  function mb32(a){
    a = (a + 0x6D2B79F5)|0;
    var t = Math.imul(a ^ (a >>> 15), 1 | a);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0)/4294967296;
  }
  // the deterministic spread (degrees of yaw, pitch) of a player's n-th shot this life
  function spreadOf(n, w){
    var s = WEAPONS[w].spread, seed = (n*2654435761 + w*40503) % 4294967296;
    return [(mb32(seed)*2 - 1)*s, (mb32(seed ^ 0x5BD1E995)*2 - 1)*s];
  }
  // distance along o + t d (t >= 0) to box b; 0 if o is inside; null: a miss
  function rayBox(o, d, b){
    var tmin = 0, tmax = Infinity;
    for(var a = 0; a < 3; a++){
      if(Math.abs(d[a]) < 1e-12){ if(o[a] < b[a] || o[a] > b[a+3]) return null; }
      else {
        var inv = 1/d[a], t1 = (b[a] - o[a])*inv, t2 = (b[a+3] - o[a])*inv;
        if(t1 > t2){ var tt = t1; t1 = t2; t2 = tt; }
        if(t1 > tmin) tmin = t1;
        if(t2 < tmax) tmax = t2;
        if(tmin > tmax) return null;
      }
    }
    return tmin;
  }
  function rayMap(m, o, d, maxd){
    var best = maxd;
    for(var i = 0; i < m.boxes.length; i++){ var t = rayBox(o, d, m.boxes[i]); if(t !== null && t < best) best = t; }
    return best;
  }
  function hitBoxes(x, y, z){
    return [[x - BODY_R, y, z - BODY_R, x + BODY_R, y + BODY_H, z + BODY_R],
            [x - HEAD_R, y + BODY_H, z - HEAD_R, x + HEAD_R, y + HEAD_TOP, z + HEAD_R]];
  }
  // [distance, head?] where the ray first meets a player at (x, y, z) nearer than maxd, else null
  function rayPlayer(o, d, x, y, z, maxd){
    var hb = hitBoxes(x, y, z), tb = rayBox(o, d, hb[0]), th = rayBox(o, d, hb[1]), best = null;
    if(tb !== null && tb < maxd) best = [tb, false];
    if(th !== null && th < maxd && (best === null || th < best[0])) best = [th, true];
    return best;
  }
  // a practice drone's centre at time t: back and forth between a and b at s m/s
  function targetAt(tg, t){
    var a = tg.a, b = tg.b, s = +tg.s, ln = Math.sqrt((b[0]-a[0])*(b[0]-a[0]) + (b[1]-a[1])*(b[1]-a[1]) + (b[2]-a[2])*(b[2]-a[2])) || 1;
    var ph = ((t*s/ln) % 2 + 2) % 2, u = ph <= 1 ? ph : 2 - ph;
    return [a[0] + (b[0]-a[0])*u, a[1] + (b[1]-a[1])*u, a[2] + (b[2]-a[2])*u];
  }
  return {R: R, H: H, EYE: EYE, STEP_H: STEP_H, RUN: RUN, SPRINT: SPRINT, ACCEL: ACCEL, AIR_ACCEL: AIR_ACCEL, GRAVITY: GRAVITY, JUMP_V: JUMP_V,
          LEAP_MULT: LEAP_MULT, SPEED_MULT: SPEED_MULT, WING_TOP: WING_TOP, WING_RISE: WING_RISE, WING_SINK: WING_SINK, WING_DRIFT: WING_DRIFT,
          STAMINA_MAX: STAMINA_MAX, STAMINA_DRAIN: STAMINA_DRAIN, STAMINA_REGEN: STAMINA_REGEN, SPRINT_SLOW: SPRINT_SLOW,
          DROP_HEIGHT: DROP_HEIGHT, DROP_TIME: DROP_TIME, DROP_SINK: DROP_SINK, DROP_DRIFT: DROP_DRIFT,
          FALL_MAX: FALL_MAX, BODY_R: BODY_R, BODY_H: BODY_H, HEAD_R: HEAD_R, HEAD_TOP: HEAD_TOP, WEAPONS: WEAPONS,
          compileMap: compileMap, overlaps: overlaps, topUnder: topUnder, move: move, dirOf: dirOf, mb32: mb32,
          spreadOf: spreadOf, rayBox: rayBox, rayMap: rayMap, hitBoxes: hitBoxes, rayPlayer: rayPlayer, targetAt: targetAt,
          LAUNCH: LAUNCH, MISSILE_V: MISSILE_V, MISSILE_G: MISSILE_G, MISSILE_TTL: MISSILE_TTL,
          BLAST_R: BLAST_R, DIRECT_R: DIRECT_R, blastDamage: blastDamage};
})();
/* STRIKE-SHARED END */

/* ---------- tuning (browser only) ---------- */
var STEP = 1/120, SEND_EVERY = 0.05, KEEPALIVE = 0.5, SWITCH = 0.25, RESPAWN = 3, PRACTICE_SECS = 60;
// offline bot brains (solo bot match) — mirrored from backend-rs/src/strike.rs
var BOT_THINK = 0.25, BOT_TURN = 9.0, BOT_AIM = 0.04, BOT_RANGE = 45.0, BOT_REACT = 0.35;
// solo difficulty: scales the offline bots' aim error, reaction, turn speed, refire pause and
// close-range mercy (the Arena server keeps its own tuned bots — these only shape the match
// you play alone). `rate` > 1 forces a gap BETWEEN shots (beyond the weapon's own interval).
// `cqc` is a guaranteed-miss radius in metres: inside it the bot's aim is pushed PAST the
// target's angular size, so even a point-blank Easy bot whiffs instead of shredding you —
// at close range a body spans several degrees of arc, so a wide random error alone still hits.
var DIFFS = [
  {id: "easy", n: "Easy", aim: 10.0, react: 2.5, turn: 0.30, rate: 3.0, cqc: 9.0},
  {id: "normal", n: "Normal", aim: 3.2, react: 1.2, turn: 0.70, rate: 1.8, cqc: 6.0},
  {id: "hard", n: "Hard", aim: 2.0, react: 0.5, turn: 1.8, rate: 1.2, cqc: 1.5},
  {id: "sweaty", n: "Sweaty", aim: 0.10, react: 0.12, turn: 3.5, rate: 1.0, cqc: 0.0}];
function diffOf(){
  var s = null; try { s = ssave(); } catch(e){}
  var id = (s && s.diff) || "sweaty";
  for(var i = 0; i < DIFFS.length; i++) if(DIFFS[i].id === id) return DIFFS[i];
  return DIFFS[1];
}
var WAYPOINTS = [
  [0.0, -9.0], [0.0, 0.0], [0.0, 9.0], [-7.0, 0.0], [7.0, 0.0],
  [-7.0, -9.0], [7.0, -9.0], [-7.0, 9.0], [7.0, 9.0],
  [-14.2, 0.0], [14.2, 0.0], [-11.0, -6.0], [11.0, 6.0],
  [-9.0, -12.8], [9.0, -12.8], [-9.0, 12.8], [9.0, 12.8],
  [-5.5, -5.2], [5.5, 5.2], [-5.5, 5.2], [5.5, -5.2],
  [0.0, -6.0], [0.0, 6.0]];
var INTERP = 0.1, JIT_MAX = 0.25, SNAPS = 12, LOOK = 0.0022, PITCH_MAX = 89;
var CHARS = [{f: "character-female-a", n: "Ada"}, {f: "character-male-a", n: "Abe"}, {f: "character-female-c", n: "Cleo"},
             {f: "character-male-c", n: "Cal"}, {f: "character-female-e", n: "Eve"}, {f: "character-male-e", n: "Eli"}];
var CHAR_SWATCH = ["#e0574a", "#3a6fd8", "#3aa86a", "#e0b325", "#8a3fd8", "#e07b25"];
// amber (team 0) and cyan (team 1): tags, boards, the 2D map and the feed
var TEAMS = [{n: "Amber", c: "#dc8c33", rgb: 0xdc8c33}, {n: "Cyan", c: "#3fb8c9", rgb: 0x3fb8c9}];
var MINUTES = [3, 5, 10], KILLS = [10, 20, 30, 50], TEAM_SIZES = [1, 2, 3, 4, 5];
// map pickups: the permanent health/ammo packs come from the map file; totem slots are
// added per match once a bot exists so they never clutter a pure-human duel. Each slot is
// an index into MAP.pickups AND into V.kinds (same layout across server & solo client).
var TOTEMS = {mole:   {hue: "#e0b325", n: "Mole",      dur: 8,  c: 0xe0b325},
              freeze: {hue: "#a15dde", n: "Paralyser", dur: 5,  c: 0xa15dde},
              decoy:  {hue: "#bfeef7", n: "Decoy",     dur: 10, c: 0xbfeef7},
              cloak:  {hue: "#a8b4c2", n: "Cloak",     dur: 12, c: 0xa8b4c2},
              // the same light blue as SHIELD_COLOR (defined below) so the totem's beam,
              // gem and guide row read as "the drop shield, bottled"
              shield: {hue: "#7fd4ff", n: "Shield",    dur: 10, c: 0x7fd4ff},
              // the three movement totems: wings owns the sky, leap owns the Space bar,
              // speed owns the ground — each for its own window, each no defence at all
              wings:  {hue: "#b2f2bb", n: "Wings",     dur: 10, c: 0xb2f2bb},
              leap:   {hue: "#ff922b", n: "Leap",      dur: 10, c: 0xff922b},
              speed:  {hue: "#ffe066", n: "Speed",     dur: 6,  c: 0xffe066},
              // the hop is the whole effect: no window, the taker is just somewhere else
              teleport: {hue: "#ff7ab6", n: "Teleport", dur: 0, c: 0xff7ab6}};
// the item guide card (press I) reads these — one line per totem, kept next to TOTEMS
var TOTEM_HELP = {mole:   "Charms an enemy bot for 8s: it shoots its own team, and its kills pay YOUR team. Only drops when a side has a bot.",
                  freeze: "Pins the nearest enemy bot for 5s: it can't move or fire.",
                  decoy:  "Drops a ghost copy of you 3m ahead for 10s: enemy bots shoot the ghost instead of you.",
                  cloak:  "Enemy bots can't see you for 12s. Real players still can.",
                  shield: "Nothing can hurt you for 10s — but you can still shoot back.",
                  wings:  "Fly for 10s: the totem lifts you up to a rooftop height by itself; WASD glides you, then it lands you automatically. You're easy to spot from below.",
                  leap:   "For 10s your Space-bar jumps launch you three times as high.",
                  speed:  "For 6s plain WASD runs at twice sprint pace — no Shift, no stamina spent.",
                  teleport: "Instant: you're somewhere else on the map — always on the ground, always somewhere open."};
// the deterministic rng used by the server + solo client so everyone sees the same drop
function hashMatch(seed, salt){ var x = (seed ^ (salt*0x9e3779b9))|0; return ((x >>> 0) / 4294967295); }
// the live-drop rings, shared by all three impls: past the map's own packs sit a fixed
// pool of totem slots and pack slots. Both drop clocks REUSE them round-robin — a new
// drop (fresh kind, fresh spot, repetitions allowed) replaces the oldest one, taken or
// not — so slot indices stay stable all match while the drops themselves never stop.
var DROP_SLOTS = 3, PACK_SLOTS = 2, PACK_GAP = 8, TOTEM_GAP = 10, TOTEM_FIRST = 8, PACK_FIRST = 6, BASE_PACKS = 7;
var KEYS = [["W A S D / stick", "move"], ["mouse / right stick", "look"], ["click / RT / F", "fire"], ["R / X", "reload"],
            ["Shift / stick click", "sprint"], ["Q / E", "lean to peek"], ["right-click or `~ / LT", "scope in"], ["1 2 3 4 / wheel / Tab", "weapon"], ["Space / A", "jump"],
            ["B / Back", "scores"], ["I", "item guide"], ["H", "hide these keys"], ["Esc", "release the mouse"]];
var BUTTONS = {w1: {keys: ["Digit1"]}, w2: {keys: ["Digit2"]}, w3: {keys: ["Digit3"]}, w4: {keys: ["Digit4"]}, swap: {keys: [], pad: [3]}, shoot: {keys: ["KeyF"]},
               trig: {keys: [], pad: [7]}, peekL: {keys: ["KeyQ"], pad: [14]}, peekR: {keys: ["KeyE"], pad: [15]}, score: {keys: ["KeyB"], pad: [8]},
               items: {keys: ["KeyI"], pad: [2]},
               sprint: {keys: ["ShiftLeft", "ShiftRight"], pad: [10]},
               wheel: {keys: ["Tab"]},
               // scope in: hold the right mouse button, or (trackpad) the `~ key above Tab; gamepad LT works too
               aim: {keys: ["Backquote"], pad: [6], mouse: 2}};
// weapon "wheel" (hold Tab): emoji glyphs for the 4 slots, in FS.WEAPONS order
// (cannot hotlink real art: IP rule; these are system emoji the OS renders).
var WHEEL_ICONS = ["\u{1F4A8}", "\u{1F3AF}", "\u{1F52B}", "\u{1F680}"];
// lean: how far your eyes slide sideways, how much the camera rolls (radians), how fast it settles
var PEEK_SIDE = 0.55, PEEK_ROLL = 0.16, PEEK_SPEED = 9.0;
// the drop shield's light blue: one constant for the 3D bubble, the 2D-map halo and the
// first-person rim (CSS reads it back through --fps-shield-rim, which keeps games.css hex-free)
var SHIELD_COLOR = "#7fd4ff";
// slide: pure visual — a ground sprint's camera dips then settles; no hitbox or physics change
var SLIDE_DIP = 0.22, SLIDE_RISE = 5.0, SLIDE_FALL = 3.0;
// ADS per weapon: SMG = holographic red-dot sight (gun + sight housing visible);
// Marksman = true sniper scope (gun hidden, world through the lens + crosshair);
// Revolver = plain iron-sight aim (no optics — no ring, no dot, no zoom).
var ADS = [{fov: 63, cls: "reddot"}, {fov: 14, cls: "sniper"}, {fov: 75, cls: ""}];

function nameOf(p){ return String((p && (p.displayName || p.handle || p.name)) || "Bot").slice(0, 24); }
function mmss(ms){ ms = Math.max(0, ms|0); var s = Math.ceil(ms/1000), m = Math.floor(s/60); s %= 60; return m+":"+(s < 10 ? "0" : "")+s; }
function ssave(){
  var s = api.save; if(!s) return {best: 0};
  if(!s.strike || typeof s.strike !== "object" || Array.isArray(s.strike)) s.strike = {};
  return s.strike;
}

/* ---------- map + three.js, loaded on demand ---------- */
var MAP = null, MAP_P = null;
function loadMap(){
  if(MAP_P) return MAP_P;
  MAP_P = fetch("/games/strike/map.json").then(function(r){ if(!r.ok) throw new Error("map "+r.status); return r.json(); })
    .then(function(j){ MAP = FS.compileMap(j); return MAP; }, function(e){ MAP_P = null; throw e; });
  return MAP_P;
}

/* =============================== the game view =============================== */
var CUR = null;

function makeGame(host, opts){
  var sv = ssave();
  var V = {mode: opts.mode, ctx: opts.ctx || null, alive: true, paused: false, phase: "idle", round: null, gotView: false,
    unsupported: false, map: !!sv.map, low: !!sv.low, sens: clamp(+sv.sens || 1, 0.2, 3), offline: false, note: "",
    me: {x: 0, y: 0, z: 0, vx: 0, vy: 0, vz: 0, g: true}, yaw: 0, pitch: 0, hp: 100, dead: false, life: 0, prot: false, team: 0,
    w: 0, mag: [30, 5, 6], res: [90, 15, 24], magX: [FS.LAUNCH.mag], resX: [FS.LAUNCH.reserve], reloadAt: 0, readyAt: 0, nextShot: 0, n: 0, kills: 0, deaths: 0,
    missiles: [], shakeAt: -10,
    players: {}, order: [], slots: {}, mySlot: -1, items: [], feed: [], results: null, minutes: 5, limit: 30, tsize: 2, scores: [0, 0],
    goAt: 0, endsAt: 0, deadAt: 0, k: null, kq: 0, off: null, sendAt: 0, sentKey: "", fixAt: 0, score: 0, shots: 0,
    drones: [], startAt: 0, kick: 0, bob: 0, flashAt: -9, hitAt: -9, headAt: -9, hurtAt: -9, hurtDir: 0, board: false, ads: false,
    tracers: [], trigWas: false, protUntil: 0, itemBack: null, dmgNums: [], peek: 0, nextLaunchAt: 0, sprinting: false, slideK: 0, slideRoll: 0,
    wheelOpen: false, wheelSel: -1, wheelLook: null, stamina: 100, drag: false, dropUntil: 0, shieldTotemUntil: 0, killCam: null,
    wingsUntil: 0, leapUntil: 0, speedUntil: 0,
    totemAt: null, packAt: null, dropRing: 0};
  function myId(){ return V.mode === "mp" && MP ? MP.me() : "me"; }
  var LAUNCH_W = 3;  // the launcher's index in the 4-strong roster every impl now shares
  // the chosen tier belongs to the ENEMIES you want to beat — your own bots are always Easy,
  // so Sweaty is "carry these potatoes", never "my team demos the lobby for me" (V lives in
  // makeGame's scope, so this helper must too, not out by DIFFS/diffOf)
  function diffFor(P){ return P && P.team === V.team ? DIFFS[0] : diffOf(); }
  function W(){ return FS.WEAPONS[V.w] || FS.LAUNCH; }
  function magOf(w){ return w === LAUNCH_W ? V.magX : V.mag; }
  function resOf(w){ return w === LAUNCH_W ? V.resX : V.res; }
  // protocol v2 servers fly the launcher too; a v1 Arena serves the first three only
  function weaponCount(){
    if(V.mode !== "mp") return FS.WEAPONS.length;
    var gi = window.ARENA && window.ARENA.arena && window.ARENA.arena.games && window.ARENA.arena.games.strike;
    return (gi && (gi.v|0) >= 2) ? FS.WEAPONS.length : 3;
  }
  // protocol v3 servers accept the sprint flag ("sp" on pos, raised speed cap); a v1/v2
  // Arena serves walk-only, like the v1 launcher clamp above. Solo always sprints.
  function sprintOK(){
    if(V.mode !== "mp") return true;
    var gi = window.ARENA && window.ARENA.arena && window.ARENA.arena.games && window.ARENA.arena.games.strike;
    return !!(gi && (gi.v|0) >= 3);
  }
  // v4 arenas accept parachute drops; solo always does (local physics are our own)
  function dropOK(){
    if(V.mode !== "mp") return true;
    var gi = window.ARENA && window.ARENA.arena && window.ARENA.arena.games && window.ARENA.arena.games.strike;
    return !!(gi && (gi.v|0) >= 4);
  }
  // the drop shield: while a parachute glide is actually running, NOTHING can hurt the
  // glider (bullets, blasts, bot aim all bounce) — but they can shoot out freely. Landing
  // (or the DROP_TIME window closing) ends it. Solo: V.dropUntil / P.ai.dropUntil are the
  // glide clocks; MP: the snapshot's shield flag (bit 32) is the server's word.
  function meShielded(t){
    if(V.mode === "mp") return !!(V.prot & 32);
    return !!((V.dropUntil && t < V.dropUntil && !V.me.g) || (V.shieldTotemUntil && t < V.shieldTotemUntil));
  }
  function botShielded(P, t){
    if(!P) return false;
    if(V.mode === "mp") return !!(P.prot & 32);
    return !!(P.ai && P.ai.dropUntil && t < P.ai.dropUntil);
  }
  function mate(uid){ var P = V.players[uid]; return !!P && P.team === V.team; }

  var root = api.mk("div", "vg-golf vg-fps vg-strike"), menu = api.mk("div", "vg-golf-menu"), stage = api.mk("div", "vg-golf-stage hidden");
  root.style.setProperty("--fps-shield-rim", "color-mix(in srgb, "+SHIELD_COLOR+" 35%, transparent)");
  root.style.setProperty("--fps-shield", SHIELD_COLOR);
  root.appendChild(menu); root.appendChild(stage); host.appendChild(root);
  var wrap = api.mk("div", "vg-golf-view vg-fps-view");
  var hud = api.mk("div", "vg-fps-hud"); hud.setAttribute("aria-hidden", "true");
  var cross = api.mk("div", "vg-fps-cross"), crossRing = api.mk("i"), crossDot = api.mk("b");
  cross.appendChild(crossRing); cross.appendChild(crossDot);
  var hitm = api.mk("div", "vg-fps-hitm"), hurt = api.mk("div", "vg-fps-hurt");
  var hpBox = api.mk("div", "vg-fps-hp"), hpTrack = api.mk("span", "vg-fps-hpt"), hpBar = api.mk("i"), hpNum = api.mk("b");
  hpTrack.appendChild(hpBar); hpBox.appendChild(hpTrack); hpBox.appendChild(hpNum);
  var ammoBox = api.mk("div", "vg-fps-ammo"), ammoNum = api.mk("b"), ammoName = api.mk("span"), reloadBar = api.mk("i", "vg-fps-rl");
  ammoBox.appendChild(ammoNum); ammoBox.appendChild(ammoName); ammoBox.appendChild(reloadBar);
  var topBox = api.mk("div", "vg-fps-top"), feedBox = api.mk("ol", "vg-fps-feed"), center = api.mk("div", "vg-fps-center hidden");
  var vignette = api.mk("div", "vg-fps-vig"), scope = api.mk("div", "vg-fps-scope");
  var stamBox = api.mk("div", "vg-fps-stam hidden"), stamBar = api.mk("i");
  stamBox.appendChild(stamBar);
  // your own drop shield, seen from inside: a rim glow over the view (an overlay ABOVE the
  // canvas — an inset shadow on the canvas wrapper itself paints underneath the canvas)
  // plus a small pill with a draining bar, so the moment the canopy packs is unmistakable
  var shOv = api.mk("div", "vg-fps-shield-ov"), shPill = api.mk("div", "vg-fps-shld hidden"), shBar = api.mk("b");
  shPill.appendChild(document.createTextNode("🛡 Shield "));
  var shTrack = api.mk("i"); shTrack.appendChild(shBar); shPill.appendChild(shTrack);
  // cloak (unseen by enemy bots) reads the same way: a grey rim over the view, colour from TOTEMS
  var clOv = api.mk("div", "vg-fps-cloak-ov");
  root.style.setProperty("--fps-cloak-rim", "color-mix(in srgb, "+TOTEMS.cloak.hue+" 30%, transparent)");
  [vignette, shOv, clOv, scope, cross, hitm, hurt, hpBox, ammoBox, topBox, feedBox, center, stamBox, shPill].forEach(function(n){ hud.appendChild(n); });
  var board = api.mk("div", "vg-golf-card vg-fps-board hidden"); board.setAttribute("role", "dialog"); board.setAttribute("aria-label", "Scoreboard");
  // the item guide (press I): what each light column on the map hands you
  var guide = api.mk("div", "vg-golf-card vg-fps-board hidden"); guide.setAttribute("role", "dialog"); guide.setAttribute("aria-label", "Item guide");
  var cardBox = api.mk("div", "vg-golf-card vg-fps-card hidden"); cardBox.setAttribute("role", "dialog"); cardBox.setAttribute("aria-label", "Results");
  var pauseBox = api.mk("div", "vg-fps-pause hidden");
  var badge = api.mk("span", "vg-reconnecting vg-golf-badge hidden", "Reconnecting…"); badge.setAttribute("role", "status");
  var load = api.mk("div", "vg-meter vg-golf-load hidden"), loadFill = api.mk("i"); load.appendChild(loadFill); load.setAttribute("aria-hidden", "true");
  var hint = api.mk("p", "vg-golf-hint"), tools = api.mk("div", "vg-row vg-golf-tools"), keysBox = api.mk("div", "vg-golf-keys");
  stage.appendChild(wrap); stage.appendChild(hint); stage.appendChild(tools); stage.appendChild(keysBox);
  function renderKeys(){
    keysBox.textContent = "";
    if(ssave().keysHidden){ keysBox.appendChild(api.mk("span", "vg-muted", "Keys hidden — press H to show them")); return; }
    KEYS.forEach(function(r){ var it = api.mk("span", "vg-golf-key"); it.appendChild(api.mk("kbd", null, r[0])); it.appendChild(document.createTextNode(" "+r[1])); keysBox.appendChild(it); });
  }
  var canvas = null, R3 = null, R2 = null, IN = null, raf = 0, lastT = 0, acc = 0, ro = null, TOK = tokens(), tokAt = 0;

  /* ---------- players ---------- */
  function ensure(uid, info){
    var P = V.players[uid];
    if(!P){
      P = V.players[uid] = {uid: uid, slot: -1, team: 0, bot: false, name: "", ch: 0, x: 0, y: 0, z: 0, yaw: 0, pitch: 0, hp: 100, w: 0, dead: false,
        prot: false, away: false, gone: false, kills: 0, deaths: 0, snaps: [], last: -1, jit: 0, seen: 0, mesh: null, moving: 0};
      V.order.push(uid);
    }
    if(info){ for(var k in info) P[k] = info[k]; }
    if(P.slot >= 0) V.slots[P.slot] = uid;
    return P;
  }
  function drop(uid){ var P = V.players[uid]; if(!P) return; if(R3) R3.removePlayer(P); delete V.players[uid]; V.order = V.order.filter(function(u){ return u !== uid; }); }
  function clearPlayers(){ V.order.slice().forEach(drop); V.slots = {}; }
  function others(){ return V.order.filter(function(u){ return u !== myId(); }).map(function(u){ return V.players[u]; }); }

  /* ---------- screens ---------- */
  var WB = null;    // HQ 2.1 spectate: follow any player, or stop watching
  function showStage(on){ stage.classList.toggle("hidden", !on); menu.classList.toggle("hidden", on); if(on){ ensureRenderer(); renderTools(); renderKeys(); }
    if(!WB) WB = E.watchBar(stage, {list: function(){ return Object.keys(V.players).filter(function(u){ return u !== myId() && !V.players[u].gone; }).map(function(u){ return {id: u, name: V.players[u].name}; }); },
      get: function(){ return V.watch; }, set: function(id){ V.watch = id; },
      stop: function(){ V.watch = null; resetMatch(); showStage(false); renderMenu(); }});
    WB.update(); }
  function isHost(){
    var s = MP ? MP.st("strike") : null, id = myId(), h = false;
    ((s && s.lobby) || []).forEach(function(p){ if(p.userId === id && p.host) h = true; });
    return h;
  }
  function select(label, opts, val, fn){
    var l = api.mk("label", "vg-golf-check"), s = api.mk("select", "vg-select"); s.setAttribute("aria-label", label);
    opts.forEach(function(o){ var op = api.mk("option", null, o[1]); op.value = String(o[0]); if(o[0] === val) op.selected = true; s.appendChild(op); });
    // numeric values arrive as numbers ("3" -> 3) but ids like "easy" must stay strings —
    // a blind +value turned them into NaN, which silently fell back to the default
    s.addEventListener("change", function(){ var v = s.value; fn(v !== "" && !isNaN(+v) ? +v : v); api.persist(); });
    l.appendChild(document.createTextNode(label+" ")); l.appendChild(s); return l;
  }
  // a small clamped number field, styled like the selects (vg-select is all semantic tokens,
  // so it themes with the rest of the dock in every mode). Empty or junk falls back to dflt.
  function numField(label, lo, hi, val, fn){
    var l = api.mk("label", "vg-golf-check"), f = api.mk("input", "vg-select vg-fps-num");
    f.type = "number"; f.min = String(lo); f.max = String(hi); f.value = String(val);
    f.setAttribute("aria-label", label);
    f.addEventListener("change", function(){ var n = clamp(Math.round(+f.value || 0), lo, hi); f.value = String(n); fn(n); api.persist(); });
    l.appendChild(document.createTextNode(label+" ")); l.appendChild(f); return l;
  }
  // the pre-game card: what Strikezone is, what's in it and what to press — visible before
  // anyone reaches for the I key mid-match. Data-driven: weapons from FS.WEAPONS, totems
  // from TOTEMS/TOTEM_HELP, teams from TEAMS, keys from KEYS, so the card can't drift.
  function ico(hue){
    var b = api.mk("i", "vg-sz-ic"); b.style.background = hue || "var(--panel2)"; b.setAttribute("aria-hidden", "true");
    return b;
  }
  function wIcon(w){
    // reuse the weapon-wheel emoji glyph (system emoji, no art to license), one per
    // FS.WEAPONS slot — index lookup keeps the card in lockstep with the wheel
    var i = FS.WEAPONS.indexOf(w), g = document.createTextNode(WHEEL_ICONS[i > -1 ? i : 0]);
    var s = api.mk("span", "vg-sz-wic"); s.setAttribute("aria-hidden", "true"); s.appendChild(g);
    return s;
  }
  function welcomeCard(){
    var card = api.mk("div", "vg-sz-card");
    card.appendChild(api.mk("p", "vg-sz-kick", "Arena mode · team deathmatch"));
    card.appendChild(api.mk("h3", "vg-sz-h", "Strikezone on "+MAP.name));
    card.appendChild(api.mk("p", "vg-muted", "Two teams — "+TEAMS[0].n+" and "+TEAMS[1].n+" — fight to the kill target or the clock. You run, aim and shoot on your own screen; the referee judges every shot (team-mates can't be hit). Short sides are filled with bots that patrol and shoot."));
    // the arena
    var ar = api.mk("div", "vg-sz-sec");
    ar.appendChild(api.mk("b", null, "The rules"));
    [["🎯", "First to the kill target wins — or whichever side is ahead when the clock runs out."],
     ["🛡", "Respawns fall from the sky under a parachute shield: steer the glide with WASD, untouchable until your boots touch."],
     ["⚡", "Fresh drops keep landing mid-match — totems and loose health/ammo packs, somewhere random, on their own clocks."]]
      .forEach(function(r){ var l = api.mk("div", "vg-sz-lin"); l.appendChild(ico("transparent")); l.children[0].textContent = r[0];
        var s = api.mk("span"); s.appendChild(document.createTextNode(r[1])); l.appendChild(s); ar.appendChild(l); });
    card.appendChild(ar);
    // the arsenal
    var al = api.mk("div", "vg-sz-sec");
    al.appendChild(api.mk("b", null, "The arsenal"));
    var wrow = api.mk("div", "vg-sz-grid");
    FS.WEAPONS.forEach(function(w){
      var t = api.mk("div", "vg-sz-pill");
      t.appendChild(wIcon(w));
      var b = api.mk("span", "vg-sz-pn");
      b.appendChild(document.createTextNode(w.name));
      b.appendChild(api.mk("small", "vg-muted", w.mag+"-round mag"+(w.auto ? " · auto" : "")));
      t.appendChild(b); wrow.appendChild(t);
    });
    al.appendChild(wrow);
    card.appendChild(al);
    // the drops — every totem in its true beacon colour
    var dr = api.mk("div", "vg-sz-sec");
    dr.appendChild(api.mk("b", null, "Drops on the map"));
    var msg = api.mk("p", "vg-muted");
    msg.appendChild(document.createTextNode("Every "+TOTEM_GAP+" s a fresh totem lands somewhere random; every "+PACK_GAP+" s a loose health or ammo pack does the same — repetitions allowed, watch the light columns in the sky."));
    dr.appendChild(msg);
    var drow = api.mk("div", "vg-sz-grid");
    Object.keys(TOTEMS).forEach(function(k){
      var tm = TOTEMS[k], t2 = api.mk("div", "vg-sz-pill");
      var gem = api.mk("i", "vg-sz-gem"); gem.style.background = tm.hue; gem.setAttribute("aria-hidden", "true");
      t2.appendChild(gem);
      var b = api.mk("span", "vg-sz-pn");
      var nm = api.mk("span"); nm.style.color = tm.hue; nm.appendChild(document.createTextNode(tm.n));
      b.appendChild(nm);
      b.appendChild(api.mk("small", "vg-muted", (tm.dur > 0 ? tm.dur+" s · " : "")+TOTEM_HELP[k].split(/[:.]/)[0].toLowerCase()));
      t2.appendChild(b); drow.appendChild(t2);
    });
    dr.appendChild(drow);
    card.appendChild(dr);
    // the controls, straight from the same KEYS table the in-game overlay reads
    var ks = api.mk("div", "vg-sz-sec");
    ks.appendChild(api.mk("b", null, "The controls"));
    var krow = api.mk("div", "vg-sz-keys");
    KEYS.forEach(function(r){
      var l = api.mk("div", "vg-sz-key");
      var kc = api.mk("kbd"); kc.textContent = r[0].split(" / ")[0];
      l.appendChild(kc); l.appendChild(document.createTextNode(r[1])); krow.appendChild(l);
    });
    ks.appendChild(krow);
    card.appendChild(ks);
    return card;
  }
  function renderMenu(){
    if(!V.alive) return;
    menu.textContent = "";
    if(!MAP){
      menu.appendChild(api.mk("p", "vg-muted", V.note || "Loading the dock…"));
      if(V.note) menu.appendChild(api.btn("Try again", "primary", function(){ V.note = ""; renderMenu();
        loadMap().then(function(){ if(V.alive) renderMenu(); }, function(){ if(V.alive){ V.note = "Couldn't load the dock."; renderMenu(); } }); }));
      return;
    }
    var sv = ssave(), mp = V.mode === "mp", host = mp && isHost();
    if(mp && V.unsupported) menu.appendChild(api.mk("p", "vg-msg", "This Arena server doesn't host Strikezone yet. You can still use the target range."));
    var top = api.mk("div", "vg-row");
    if(mp) top.appendChild(api.btn("Target range (solo)", "", function(){ V.mode = "practice"; renderMenu(); }));
    else if(V.ctx) top.appendChild(api.btn("Back to the lobby", "", function(){ V.mode = "mp"; resetMatch(); requestView(); renderMenu(); }));
    function check(label, val, fn){
      var l = api.mk("label", "vg-golf-check"), cb = api.mk("input"); cb.type = "checkbox"; cb.checked = val;
      cb.addEventListener("change", function(){ fn(cb.checked); api.persist(); });
      l.appendChild(cb); l.appendChild(document.createTextNode(" "+label)); top.appendChild(l);
    }
    check("Map view (2D)", V.map, function(v){ V.map = v; sv.map = v; });
    check("Low detail (faster on older laptops)", V.low, function(v){ V.low = v; sv.low = v; });
    var sl = api.mk("label", "vg-golf-check"), sr = api.mk("input"); sr.type = "range"; sr.min = "0.2"; sr.max = "3"; sr.step = "0.1"; sr.value = String(V.sens);
    sr.setAttribute("aria-label", "Mouse sensitivity");
    sr.addEventListener("change", function(){ V.sens = clamp(+sr.value || 1, 0.2, 3); sv.sens = V.sens; api.persist(); if(IN) IN.sens = V.sens; });
    sl.appendChild(document.createTextNode("Mouse sensitivity ")); sl.appendChild(sr); top.appendChild(sl);
    menu.appendChild(top);
    // character
    menu.appendChild(api.mk("h4", "vg-golf-h", "Your character"));
    var chars = api.mk("div", "vg-golf-chars"); chars.setAttribute("role", "group"); chars.setAttribute("aria-label", "Pick a character");
    var mine = clamp(sv.ch|0, 0, CHARS.length - 1);
    CHARS.forEach(function(c, i){
      var b = api.btn("", "vg-golf-char vg-fps-char"+(i === mine ? " on" : ""), function(){
        sv.ch = i; api.persist(); if(mp && MP) MP.send("strike", "char", {c: i}); renderMenu();
      });
      var sw = api.mk("i", "vg-fps-sw"); sw.style.background = CHAR_SWATCH[i]; sw.setAttribute("aria-hidden", "true"); b.appendChild(sw);
      b.appendChild(api.mk("span", null, c.n)); b.setAttribute("aria-pressed", i === mine ? "true" : "false");
      chars.appendChild(b);
    });
    menu.appendChild(chars);
    // HQ 2.1 spectate: a match is on and you're not in it
    if(mp && V.round && (V.round.phase === "warmup" || V.round.phase === "round") && !inMatch(V.round)){
      menu.appendChild(api.btn("Watch the match", "primary", function(){
        var first = (V.round.players || []).filter(function(p){ return p.user && !p.gone; })[0];
        if(first){ V.watch = first.user.id || first.user.userId; applyView(V.round); }
      }));
    }
    if(mp){
      menu.appendChild(api.mk("h4", "vg-golf-h", host ? "Start a match" : "Match"));
      var row = api.mk("div", "vg-row");
      var mins = MINUTES.indexOf(sv.minutes|0) >= 0 ? sv.minutes|0 : 5, kl = KILLS.indexOf(sv.kills|0) >= 0 ? sv.kills|0 : 30,
          ts = TEAM_SIZES.indexOf(sv.tsize|0) >= 0 ? sv.tsize|0 : 2;
      if(host){
        // the host sizes the sides: X vs X, bots fill whichever side is short of players.
        // Length/target: the custom field (1..60 min, 1..500 kills) wins over the preset
        // selects when it holds something the server's list doesn't have.
        row.appendChild(select("Teams", TEAM_SIZES.map(function(t){ return [t, t+" vs "+t]; }), ts, function(v){ sv.tsize = v; }));
        row.appendChild(select("Length", MINUTES.map(function(m){ return [m, m+" minutes"]; }), mins, function(v){ sv.minutes = v; }));
        row.appendChild(select("First to", KILLS.map(function(k){ return [k, k+" kills"]; }), kl, function(v){ sv.kills = v; }));
        // one skill for every bot on both sides (bot-filled teams must be fair); the server
        // applies it to its fillers when the match starts
        row.appendChild(select("Bot skill", DIFFS.map(function(d){ return [d.id, d.n]; }), (diffOf() || DIFFS[1]).id,
          function(v){ sv.diff = v; }));
        row.appendChild(numField("Custom min", 1, 60, sv.minutes|0 || 5, function(v){ sv.minutes = v; }));
        row.appendChild(numField("Custom kills", 1, 500, sv.kills|0 || 30, function(v){ sv.kills = v; }));
        row.appendChild(api.btn("Start the match", "primary", function(){
          // the server takes the host's word for minutes/kills inside its clamps (any value,
          // not just MINUTES/KILLS) — the lists above are just quick presets
          var s = ssave(), m = clamp(Math.round(s.minutes|0 || 5), 1, 60), k = clamp(Math.round(s.kills|0 || 30), 1, 500);
          if(MP) MP.send("strike", "start", {minutes: m, kills: k, tsize: TEAM_SIZES.indexOf(s.tsize|0) >= 0 ? s.tsize|0 : 2,
            diff: DIFFS.some(function(d){ return d.id === s.diff; }) ? s.diff : "normal"});
        }));
      } else row.appendChild(api.mk("span", "vg-muted", "Waiting for the host (★ in the lobby) to start a match — any side short of humans is filled with bots at the host's chosen skill, the same on both teams."));
      menu.appendChild(row);
      if(V.results) menu.appendChild(resultsTable(V.results, "Last match"));
      if(V.round && (V.round.phase === "warmup" || V.round.phase === "round"))
        menu.appendChild(api.btn("Join the match", "primary", function(){ requestView(); }));
      menu.appendChild(welcomeCard());
    } else {
      // solo: a full offline bot match by default; the classic drone range is the warm-up
      menu.appendChild(api.mk("h4", "vg-golf-h", "Solo"));
      var soloRow = api.mk("div", "vg-row");
      var soloTs = TEAM_SIZES.indexOf(sv.tsize|0) >= 0 ? sv.tsize|0 : 3;   // 3v3 unless you picked otherwise
      soloRow.appendChild(api.btn("Play a bot match ("+soloTs+"v"+soloTs+")", "primary", function(){ startBotMatch(); }));
      soloRow.appendChild(select("Teams", TEAM_SIZES.map(function(t){ return [t, t+" vs "+t]; }), soloTs, function(v){ sv.tsize = v; }));
      soloRow.appendChild(select("Bot skill", DIFFS.map(function(d){ return [d.id, d.n]; }), (diffOf() || DIFFS[DIFFS.length - 1]).id,
        function(v){ sv.diff = v; }));
      menu.appendChild(soloRow);
      // the scoreline is yours too: any length and any target, not just the server's list
      var soloCfg = api.mk("div", "vg-row");
      soloCfg.appendChild(numField("Minutes", 1, 60, sv.minutes|0 || 3, function(v){ sv.minutes = v; }));
      soloCfg.appendChild(numField("First to", 1, 500, sv.kills|0 || 10, function(v){ sv.kills = v; }));
      menu.appendChild(soloCfg);
      var cf = api.mk("details", "vg-golf-check"), sum = api.mk("summary", null, "Classic target range (60 s)");
      cf.appendChild(sum);
      var cfRow = api.mk("div", "vg-row");
      cfRow.appendChild(api.btn("Start the range", "", function(){ startPractice(); }));
      var best = sv.best|0; if(best) cfRow.appendChild(api.mk("span", "vg-muted", "Best: "+best+" drones"));
      cf.appendChild(cfRow); menu.appendChild(cf);
      menu.appendChild(welcomeCard());
    }
  }
  function renderTools(){
    tools.textContent = "";
    tools.appendChild(api.btn(V.mode === "mp" ? "Leave the match" : "Back to the menu", "", function(){
      if(V.mode === "mp" && MP && isHost() && (V.phase === "warmup" || V.phase === "round")){
        if(!window.confirm("End the match for everyone?")) return;
        MP.send("strike", "end");
      }
      if(IN) IN.unlock();
      resetMatch(); showStage(false); renderMenu();
    }));
    tools.appendChild(api.btn("Scores", "", function(){ V.board = !V.board; renderBoard(); }));
    tools.appendChild(api.btn("Items", "", function(){ V.guide = !V.guide; renderGuide(); }));
  }
  function resetMatch(){
    // drop slots appended to MAP.pickups during a match are match-local: trim them so the
    // next match (and the server's shorter slot list) sees only the map's own packs
    MAP.pickups.length = 7;
    totemCount = 0; packCount = 0; V.totemAt = null; V.packAt = null;
    (V.missiles || []).forEach(function(m){ if(m.o && R3) R3.rocket(null, m); }); V.missiles = [];
    clearPlayers(); V.phase = "idle"; V.results = null; V.feed = []; V.drones = []; V.tracers = []; V.dmgNums = []; V.killCam = null;
    cardBox.classList.add("hidden"); board.classList.add("hidden"); guide.classList.add("hidden"); center.classList.add("hidden"); V.board = false; V.guide = false;
    if(R3) R3.clearDynamic();
    // totems die with the old match; the slot "kind" memory resets too
    V.kinds = []; V.decoys = []; V.cloakUntil = 0; V.shieldTotemUntil = 0;
    V.wingsUntil = 0; V.leapUntil = 0; V.speedUntil = 0;
    others().forEach(function(P){ P.charmUntil = 0; P.freezeUntil = 0; clearMarks(P); });
  }

  /* ---------- me ---------- */
  function placeMe(x, y, z, yawDeg){
    var M = V.me; M.x = x; M.y = y; M.z = z; M.vx = 0; M.vy = 0; M.vz = 0; M.g = true;
    if(yawDeg != null){ V.yaw = yawDeg*Math.PI/180; V.pitch = 0; }
    V.dropUntil = 0;  // a plain ground spawn isn't a drop; dropSpawns set this explicitly
  }
  function randomDropSpot(t, slot, team){
    // one deterministic random-looking spawn point per life (slot ∘ life-seeded): not
    // Math.random() — a player sees the same spot given the same seeds. Own side only:
    // the spawn list is amber's half then cyan's, and dropping into the enemy's half
    // puts you in their lap with your spawn cover already burnt mid-air.
    var n = MAP.spawns.length || 1, half = Math.floor(n/2);
    var pool = team === 1 ? MAP.spawns.slice(half) : MAP.spawns.slice(0, half);
    if(!pool.length) pool = MAP.spawns;
    var seed = ((slot || 0) * 131 + ((t * 1000) | 0)) & 0xFFFF;
    var k = Math.floor(FS.mb32(seed) * pool.length);
    return pool[Math.min(k, pool.length - 1)] || [0, 0, 0, 0];
  }
  // a parachute-drop spawn: pick a random spawn point's XZ and lift the Y. The referee's
  // 4-second spawn-t window (MAX_AIR-let-up) is what makes an 11 m fall legal in v4 arenas.
  function dropSpawn(slot){
    var sp = randomDropSpot(now(), slot, V.team);
    placeMe(sp[0], FS.DROP_HEIGHT, sp[2], sp[3]);
    V.dropUntil = now() + FS.DROP_TIME;
    V.me.g = false;
    return sp;
  }
  function fullAmmo(){ V.mag = FS.WEAPONS.map(function(wp){ return wp.mag; }); V.res = FS.WEAPONS.map(function(wp){ return wp.reserve; }); V.magX = [FS.LAUNCH.mag]; V.resX = [FS.LAUNCH.reserve]; V.reloadAt = 0; V.n = 0; if(V.w >= weaponCount()) V.w = 0; }

  /* ---------- practice: the target range ---------- */
  function startPractice(){
    if(!MAP) return;
    resetMatch(); V.mode = "practice"; V.score = 0; V.shots = 0; V.team = 0; V.kinds = [];
    var s = MAP.spawns[0]; placeMe(s[0], s[1], s[2], s[3]); fullAmmo(); V.hp = 100; V.dead = false;
    V.drones = MAP.targets.map(function(tg, i){ return {tg: tg, i: i, downUntil: 0, phase: i*1.7}; });
    V.items = MAP.pickups.map(function(){ return 1; });
    V.phase = "warmup"; V.goAt = now() + 3; V.endsAt = V.goAt + PRACTICE_SECS;
    showStage(true); if(R3) R3.buildMap();
    // same as the bot match: enter the range already holding the mouse
    if(canvas) canvas.focus();
    if(IN) IN.lock();
    say("Target range: "+PRACTICE_SECS+" seconds.");
  }
  function dronePos(D, t){ var c = FS.targetAt(D.tg, t + D.phase); return c; }

  /* ---------- solo bot match: the same teams/bots as the server, judged locally ---------- */
  var BOT_NAMES = ["Bolt", "Havoc", "Rook", "Vista", "Onyx", "Slate", "Drift", "Crane"];
  function botPlace(P, sp, t){
    P.x = sp[0]; P.y = sp[1]; P.z = sp[2]; P.yaw = (sp[3]|0)*Math.PI/180; P.pitch = 0;
    P.hp = 100; P.dead = false;
    // solo bots can draw the launcher too — local only, never leaves your machine.
    // deterministic by slot+spawn (never Math.random(): every player sees the same bot twice)
    var seed = ((t*1000)|0) + (P.slot|0)*131;
    P.w = Math.floor(FS.mb32(seed & 0xFFFF)*4);
    P.mag = FS.WEAPONS.map(function(wp){ return wp.mag; });
    P.res = FS.WEAPONS.map(function(wp){ return wp.reserve; });
    P.reloadAt = 0; P.protUntil = t + 1.5; // PROTECT (strike.py): 1.5 s of spawn cover, gone once they fire
    P.ai.body = {x: sp[0], y: sp[1], z: sp[2], vx: 0, vy: 0, vz: 0, g: true};
    // v4: a spawn from the sky (initial join comes from a ground spawn list, respawns don't)
    P.ai.dropUntil = sp[1] > 0.5 ? t + FS.DROP_TIME : 0;
    P.ai.aim_y = sp[3]|0; P.ai.aim_p = 0; P.ai.goal = null; P.ai.target = null; P.ai.seenAt = null; P.ai.fireAt = -1e9; P.ai.thinkAt = t + BOT_THINK;
    P.ai.errAt = 0; P.ai.err_y = 0; P.ai.err_p = 0;
  }
  function botSpawnFor(P){
    var half = Math.floor(MAP.spawns.length/2);
    return MAP.spawns[P.team === 0 ? (P.slot % Math.max(1, half)) : half + (P.slot % Math.max(1, MAP.spawns.length - half))];
  }
  function addBot(team, slot, t){
    var uid = "bot-"+team+"-"+slot;
    var P = ensure(uid, {slot: slot, team: team, name: BOT_NAMES[(team*4 + slot) % BOT_NAMES.length], bot: true,
      ch: clamp(slot + team*3, 0, CHARS.length - 1)});
    P.ai = {body: {x: 0, y: 0, z: 0, vx: 0, vy: 0, vz: 0, g: true}};
    P.n = 0;
    P.charmUntil = 0; P.freezeUntil = 0; P.decoyAt = 0;
    botPlace(P, botSpawnFor(P), t);
    return P;
  }
  // the floating charm/freeze marks over a player's head: one mesh per kind, removed when
  // the effect ends (the server's snapshot flags drive this in MP, the clock in solo)
  function clearMarks(P, kind){
    if(!P || !P.marks) return;
    var frz = kind === "freeze";
    for(var i = P.marks.length - 1; i >= 0; i--){
      if(!kind || P.marks[i].__freeze === frz){
        if(P.mesh && P.mesh.g) P.mesh.g.remove(P.marks[i]);
        P.marks.splice(i, 1);
      }
    }
  }
  function botEnemies(P){
    var px = Math.cos(V.yaw)*PEEK_SIDE*(V.peek || 0), pz = Math.sin(V.yaw)*PEEK_SIDE*(V.peek || 0);
    var out = others().concat([{__me: 1}]).filter(function(q){
      if(q.__me){
        // cloak hides me from every bot in solo; MP goes through the server
        if(V.mode === "bots" && V.cloakUntil && now() < V.cloakUntil) return false;
        return V.team !== P.team && !V.dead;
      }
      // mole: the charmed bot sees its own side as the foe and its old foe as home
      var charm = P.charmUntil && P.charmUntil > now();
      var foe = charm ? q.team === P.team : q.team !== P.team;
      return q !== P && foe && !q.dead && !q.gone;
    }).map(function(q){ return q.__me ? {x: V.me.x + px, y: V.me.y, z: V.me.z + pz, vx: V.me.vx || 0, vz: V.me.vz || 0, __me: 1} : q; });
    // decoy: the shooter sees a ghost standing where I dropped it, and it looks free to hit
    V.decoys && V.decoys.forEach(function(dc){ if(dc && dc.until > now() && !P.decoyAt){ out.push({x: dc.x, y: dc.y, z: dc.z, vx: 0, vz: 0, __decoy: dc}); } });
    return out;
  }
  function los(a, b){
    var d = [b[0]-a[0], b[1]-a[1], b[2]-a[2]], ln = Math.sqrt(d[0]*d[0] + d[1]*d[1] + d[2]*d[2]);
    if(ln < 1e-6) return true;
    d = [d[0]/ln, d[1]/ln, d[2]/ln];
    return FS.rayMap(MAP, a, d, ln - 0.001) >= ln - 0.001;
  }
  function botJudge(P, t, e, d, reach){
    var foes = botEnemies(P), end = reach, vic = null, head = false;
    foes.forEach(function(V2){
      var h = FS.rayPlayer(e, d, V2.x, V2.y, V2.z, end);
      if(h && h[0] < end){ end = h[0]; vic = V2; head = h[1]; }
    });
    addTracer(muzzleOf(P), [e[0] + d[0]*end, e[1] + d[1]*end, e[2] + d[2]*end], P.w, t, false);
    if(!vic) return;
    if(vic.__decoy){                          // a ghost swallows every tracer until its time runs out
      vic.__decoy.until = 0;                  // bots only shoot at it once it's been spotted
      return;
    }
    var dmg = head ? FS.WEAPONS[P.w].head : FS.WEAPONS[P.w].damage;
    if(vic.__me){
      if(V.protUntil && t < V.protUntil) return;
      if(meShielded(t)) return;  // the drop shield: bullets can't reach a glider
      V.hp -= dmg; V.hurtAt = t; V.hurtDir = Math.atan2(P.x - V.me.x, -(P.z - V.me.z)) - V.yaw;
      say((P.name || "A bot")+(head ? " landed a headshot" : " hit you")+" ("+Math.max(0, V.hp|0)+" hp)");
      if(V.hp <= 0){ endBotLife(P, null, t, head); }
      return;
    }
    if(vic.protUntil && t < vic.protUntil) return; // spawn cover still up
    if(botShielded(vic, t)) return;  // the drop shield: bullets can't reach a glider
    vic.hp -= dmg;
    if(vic.hp <= 0 && !vic.dead) endBotLife(P, vic, t, head);
  }
  /* ---------- launcher: a slow missile that flies to where it hits and blasts ----------
   * Solo (bots/practice): the browser flies it (missilesStep). MP: the server flies it in its
   * 20 Hz tick and the snapshot's "r" rows drive the same meshes; "b" rows are booms. */
  function spawnMissile(t, e, d, owner, fromServer){
    V.missiles.push({id: V.life*100 + (V.n|0), at: t, ttl: FS.MISSILE_TTL, px: e[0], py: e[1], pz: e[2],
                     vx: d[0]*FS.MISSILE_V, vy: d[1]*FS.MISSILE_V, vz: d[2]*FS.MISSILE_V, o: null, x: e[0], y: e[1], z: e[2],
                     owner: owner || null, srv: !!fromServer});   // owner null: the local player
    if(V.missiles.length > 2){ var old = V.missiles.shift(); if(old.o && R3) R3.rocket(null, old); }
  }
  function detonate(miss, t){
    if(miss.srv) return;  // server-flown: the "b" snapshot rows bring the boom and the damage
    var x = miss.x, y = miss.y, z = miss.z;
    if(R3) R3.boom(x, y, z, t);
    E.sfx("boom");
    V.shakeAt = t;
    var shooter = miss.owner; // null: the local player; else a bot
    // every living thing in radius takes falloff; the shooter is NOT safe (honest rockets, friendlies included)
    others().forEach(function(P){
      if(P.gone || P.dead) return;
      var dx = P.x - x, dy = (P.y + 0.9) - y, dz = P.z - z, dist = Math.sqrt(dx*dx + dy*dy + dz*dz);
      // the shooter is honest-but-fair: self blast grades by proximity (75 falloff), never the
      // 120 "direct" reserved for a rocket actually lodged in you — an eye-firing bot's own
      // throat is ~0.6 m above its torso center, which would otherwise be an instant suicide
      var dmg = FS.blastDamage(dist, P === shooter ? false : dist < FS.DIRECT_R);
      if(dmg <= 0) return;
      if(P.protUntil && t < P.protUntil) return;
      if(botShielded(P, t)) return;  // the drop shield: blasts can't reach a glider
      P.hp -= dmg;
      if(miss.srv) return;
      if(!shooter) V.hitAt = t;
      V.dmgNums.push({x: P.x, y: P.y + 1.2, z: P.z, n: dmg, head: dist < FS.DIRECT_R, at: t, o: null});
      if(V.dmgNums.length > 12) V.dmgNums.shift();
      var killer = shooter && P !== shooter ? {uid: shooter.uid, kills: shooter.kills|0, team: shooter.team, w: LAUNCH_W, name: shooter.name}
                           : shooter ? null : {uid: myId(), kills: V.kills, team: V.team, w: LAUNCH_W, name: "You"};
      if(P.hp <= 0 && !P.dead) endBotLife(killer, P, t, dist < FS.DIRECT_R);
    });
    if(V.mode === "practice") V.drones.forEach(function(D){
      if(D.downUntil > t) return;
      var c = dronePos(D, t - V.startAt), dx = c[0] - x, dy = c[1] - y, dz = c[2] - z;
      if(dx*dx + dy*dy + dz*dz < 16){ D.downUntil = t + 1.5; V.score++; V.hitAt = t; }
    });
    // your own rockets are honest: full falloff on you (not through spawn cover you dropped by firing)
    if(!shooter || shooter.team !== V.team){
      var mdx = V.me.x - x, mdy = (V.me.y + 0.9) - y, mdz = V.me.z - z, md = Math.sqrt(mdx*mdx + mdy*mdy + mdz*mdz);
      var self = FS.blastDamage(md, false);
      if(self > 0 && !V.dead && !meShielded(t)){  // the drop shield stops blasts reaching a glider too
        V.hp -= self; V.hurtAt = t; V.hurtDir = Math.atan2(x - V.me.x, -(z - V.me.z)) - V.yaw;
        if(V.hp <= 0){ V.hp = 0; V.dead = true; V.deadAt = t; V.deaths = (V.deaths|0) + 1; V.wingsUntil = 0; V.leapUntil = 0; V.speedUntil = 0;
          if(shooter && shooter.uid !== myId()) snapKillCam(shooter, LAUNCH_W, false);
          onEvent({ev: "kill", k: shooter ? shooter.uid : null, v: myId(), w: LAUNCH_W, hs: false,
                   kills: shooter ? (shooter.kills|0) : V.kills, deaths: V.deaths, scores: V.scores}); }
      }
    }
  }
  function missilesStep(t, dt){
    for(var i = V.missiles.length - 1; i >= 0; i--){
      var m = V.missiles[i], stopped = false;
      if(m.srv) continue;  // the server flies this one; the snapshot places it
      m.vy -= FS.MISSILE_G*dt;
      var sp = Math.sqrt(m.vx*m.vx + m.vy*m.vy + m.vz*m.vz) || 1;
      var nx = m.px + m.vx*dt, ny = m.py + m.vy*dt, nz = m.pz + m.vz*dt;
      var d3 = [nx - m.px, ny - m.py, nz - m.pz], ln = Math.sqrt(d3[0]*d3[0] + d3[1]*d3[1] + d3[2]*d3[2]) || 1e-6;
      // wall?
      var reach = FS.rayMap(MAP, [m.px, m.py, m.pz], [d3[0]/ln, d3[1]/ln, d3[2]/ln], ln);
      if(reach < ln){ stopped = true; m.x = m.px + d3[0]*reach/ln; m.y = m.py + d3[1]*reach/ln; m.z = m.pz + d3[2]*reach/ln; }
      else { m.x = nx; m.y = ny; m.z = nz; }
      // a body?
      if(!stopped) others().forEach(function(P){
        if(stopped || P.gone || P.dead || P.team === V.team) return;
        if(botShielded(P, t)) return;  // the drop shield: a rocket passes a glider by
        var hb = FS.hitBoxes(P.x, P.y, P.z);
        for(var b = 0; b < 2; b++){
          var h = FS.rayBox([m.px, m.py, m.pz], [d3[0]/ln, d3[1]/ln, d3[2]/ln], hb[b]);
          if(h !== null && h <= ln){ stopped = true; m.x = m.px + d3[0]*h/ln; m.y = m.py + d3[1]*h/ln; m.z = m.pz + d3[2]*h/ln; break; }
        }
      });
      // the ground (the map's floor is implicit: no box under you, y hits 0)
      if(!stopped && m.y <= 0.05){ stopped = true; m.y = 0.05; }
      m.px = m.x; m.py = m.y; m.pz = m.z;
      if(stopped || t - m.at > m.ttl){ detonate(m, t); if(m.o && R3) R3.rocket(null, m); V.missiles.splice(i, 1); }
      else if(R3) R3.rocket(m, m);
    }
  }

  // the kill cam: a frozen snapshot of the killer (taken the instant you died) the camera
  // flies to while "Respawning in X" counts down. Not a live follow — in MP their snapshots
  // keep interpolating anyway; here we keep the reference so the tag can use their live name.
  function snapKillCam(K, w, head){
    if(!K || K.gone) return;
    V.killCam = {uid: K.uid, name: K.name || "Killer", team: K.team|0, w: w|0, hs: !!head,
                 x: K.x, y: K.y, z: K.z, yaw: K.yaw || 0, pitch: K.pitch || 0, ref: K};
  }
  function endBotLife(killer, vic, t, head){
    // kill cam: your own death is worth one snapshot of the shooter — where they stood,
    // where they looked — replayed from their eyes while "Respawning in X" counts down
    if(!vic && killer && killer.uid !== myId()) snapKillCam(killer, killer.w|0, !!head);
    if(killer) killer.kills = (killer.kills|0) + 1;
    if(killer && killer.team != null){
      // a mole kill still pays the real shooter's side? no — the charm DELIBERATELY turns
      // friend\-to\-foe aim points back at the charmer's team, so the credit goes there
      var charm = killer.charmUntil && killer.charmUntil > t;
      V.scores[(charm ? 1 - killer.team : killer.team) === 0 ? 0 : 1]++;
    }
    if(vic){ vic.dead = true; vic.deaths = (vic.deaths|0) + 1; vic.deadAt = t; vic.hp = 0; }
    if(!vic){ // the local player died
      if(killer) V.deaths = (V.deaths|0) + 1;
      V.dead = true; V.deadAt = t; V.hp = 0;   // endBotLife is also the bots-mode death site
      V.wingsUntil = 0; V.leapUntil = 0; V.speedUntil = 0;   // a death pops the movement totems too
      onEvent({ev: "kill", k: killer.uid, v: myId(), w: killer.w, hs: !!head, kills: killer.kills, deaths: V.deaths, scores: V.scores});
      return;
    }
    onEvent({ev: "kill", k: killer ? killer.uid : null, v: vic.uid, w: killer ? killer.w : 0, hs: !!head,
             kills: killer ? killer.kills|0 : 0, deaths: vic.deaths|0, scores: V.scores});
  }
  // the four totem effects, shared shape between solo (judge local) and MP (mirror the server)
  function totemApply(kind, uid, t){
    if(!TOTEMS[kind]) return null;
    var foes = others().filter(function(P){ return !P.bot || P.dead || P.gone ? false : P.team !== V.team; });
    if(!foes.length && kind === "mole") return "Mole — no enemy bots to turn";
    if(kind === "mole"){
      var tar = foes[0];  // deterministic: first in the slot order
      foes.forEach(function(P){ if(P.slot != null && P.slot < tar.slot) tar = P; });
      tar.charmUntil = t + TOTEMS.mole.dur;
      if(R3) R3.charm(tar);
      return "Mole: "+tar.name+" turned against their own team for "+TOTEMS.mole.dur+"s";
    }
    if(kind === "freeze"){
      var best = null, bd = 1e9;
      foes.forEach(function(P){ var d = Math.hypot(P.x - V.me.x, P.z - V.me.z); if(d < bd){ bd = d; best = P; } });
      if(!best) return "Paralyser — no enemy bots nearby";
      best.freezeUntil = t + TOTEMS.freeze.dur;
      if(R3) R3.charm(best, "freeze");
      return "Paralyser: "+best.name+" frozen for "+TOTEMS.freeze.dur+"s";
    }
    if(kind === "decoy"){
      // drop a ghost copy of me three metres ahead of where I face
      var vx = Math.sin(V.yaw), vz = -Math.cos(V.yaw);
      var x = V.me.x + vx*3, z = V.me.z + vz*3, i;
      for(i = 0; i < 20 && FS.overlaps(MAP, x, 0.1, z, 0.2) >= 0; i++){ x = V.me.x + vx*(3 + i*0.3); z = V.me.z + vz*(3 + i*0.3); }
      V.decoys = V.decoys || [];
      var dc = {x: x, y: 0, z: z, hp: 60, until: t + TOTEMS.decoy.dur};
      V.decoys.push(dc);
      if(R3) R3.addDecoy(dc);
      return "Decoy: a ghost of me drops in "+["front","left","right","behind"][(i/6)|0]+" and draws bot fire for "+TOTEMS.decoy.dur+"s";
    }
    if(kind === "cloak"){
      V.cloakUntil = t + TOTEMS.cloak.dur;
      root.classList.add("vg-fps-cloak");
      return "Cloak: every enemy bot stops seeing me for "+TOTEMS.cloak.dur+"s";
    }
    if(kind === "shield"){
      // the drop shield, bottled: nothing hurts the taker while they keep shooting
      V.shieldTotemUntil = t + TOTEMS.shield.dur;
      return "Shield: nothing can hurt me for "+TOTEMS.shield.dur+"s";
    }
    if(kind === "wings"){
      V.wingsUntil = t + TOTEMS.wings.dur;
      return "Wings: up you go for "+TOTEMS.wings.dur+"s — WASD glides, landing is automatic. Easy to shoot from below";
    }
    if(kind === "leap"){
      V.leapUntil = t + TOTEMS.leap.dur;
      return "Leap: huge Space-bar jumps for "+TOTEMS.leap.dur+"s";
    }
    if(kind === "speed"){
      V.speedUntil = t + TOTEMS.speed.dur;
      return "Speed: plain W dashes at double sprint pace for "+TOTEMS.speed.dur+"s";
    }
    if(kind === "teleport"){
      // instant: a random open spot on the ground (the same hashed scatter drops use,
      // seeded from this take so it's never Math.random()) — pop any flight/drop state,
      // the body arrives standing. The time runs through the golden-ratio multiplier
      // first: a low-bit time XOR alone barely moves the scatter (sub-mm), the full-range
      // mix makes each take land somewhere genuinely fresh.
      var tk = ((hashStr(myId() + ":" + ((V.life|0) + "." + (V.n|0))) ^ Math.imul(Math.floor(t*1000) + 1, 2654435761)) >>> 0);
      var to = dropSpot(tk);
      V.me.x = to[0]; V.me.y = 0; V.me.z = to[2];
      V.me.vx = 0; V.me.vy = 0; V.me.vz = 0; V.me.g = true;
      V.dropUntil = 0; V.wingsUntil = 0;   // a hop cancels a glide/flight — you land standing
      return "Teleport: you're somewhere else now";
    }
    return null;
  }
  // drop slots: past the map's own packs (BASE_PACKS) sit DROP_SLOTS totem slots then
  // PACK_SLOTS pack slots — one fixed layout, so a drop's index means the same thing to
  // the server, a mid-match joiner and the solo client. Both drop clocks reuse them
  // round-robin. (BASE_PACKS, not MAP.pickups.length: the slots append past it.)
  function dropPlan(){
    return {base: BASE_PACKS, totems: [BASE_PACKS, BASE_PACKS + DROP_SLOTS], packs: [BASE_PACKS + DROP_SLOTS, BASE_PACKS + DROP_SLOTS + PACK_SLOTS]};
  }
  function ensureDropSlots(){
    // the slot entries exist (kind "" = empty) for the whole match; drops rewrite them
    var pl = dropPlan();
    while(MAP.pickups.length < pl.packs[1]){
      var j = MAP.pickups.length;
      MAP.pickups.push({id: j < pl.packs[0] ? "totem-"+j : "pack-"+j, kind: "", at: [0, 0, 0]});
    }
  }
  // the drop rng: a fresh kind and a fresh open spot from the drop counter (not the wall
  // clock — epoch-ms seeds differ only in their low bits between drops, and hashMatch's
  // /2^32 output reads the top ones, which stacked every drop on the same spot)
  var totemCount = 0, packCount = 0;
  function dropSpot(tok){
    var x = -17 + hashMatch(tok, 1)*34, z = -10 + hashMatch(tok, 2)*20;
    for(var j = 0; j < 60 && FS.overlaps(MAP, x, 0.1, z, 0.3) >= 0; j++){
      x = -17 + hashMatch(tok ^ (j*2654435761), 1)*34; z = -10 + hashMatch(tok ^ (j*2654435761), 2)*20;
    }
    return [x, 0, z];
  }
  function totemSpawn(seed, canMole){
    ensureDropSlots();
    var pl = dropPlan();
    var tok = ((seed ^ (totemCount = (totemCount + 1)) * 0x9e3779b9) >>> 0);
    var toks = canMole ? ["mole", "freeze", "decoy", "cloak", "shield", "wings", "leap", "speed", "teleport"]
                       : ["freeze", "decoy", "cloak", "shield", "wings", "leap", "speed", "teleport"];
    var kind = toks[((tok % toks.length) + toks.length) % toks.length];
    var idx = pl.totems[0] + (totemCount - 1) % DROP_SLOTS;
    MAP.pickups[idx] = {id: kind+"-"+((tok >>> 0)%99991), kind: kind, at: dropSpot(tok)};
    return idx;
  }
  function packSpawn(seed){
    ensureDropSlots();
    var pl = dropPlan();
    var tok = ((seed ^ (packCount = (packCount + 1)) * 0x9e3779b9 + 0x5f356495) >>> 0);
    var kind = tok % 2 ? "ammo" : "health";
    var idx = pl.packs[0] + (packCount - 1) % PACK_SLOTS;
    MAP.pickups[idx] = {id: kind+"-"+((tok >>> 0)%99991), kind: kind, at: dropSpot(tok)};
    return idx;
  }
  function botStep(P, t){
    var ai = P.ai, x = P.ai.body.x, z = P.ai.body.z, i;
    if(P.freezeUntil && t < P.freezeUntil) return;   // the Paralyser pins a bot in place
    // -- decide a goal (at most every BOT_THINK) --
    if(t >= ai.thinkAt){
      var foes = botEnemies(P), pick = null, pickPos = null, pool = [], wts = [], wt = 0, cur = ai.target;
      // a stable handle for "me" this tick, so a bot chasing the player keeps the same object
      // to compare targets against — a fresh wrapper every call would reset its lock every frame
      for(i = 0; i < foes.length; i++){ if(foes[i].__me){ foes[i].__uid = "me"; } else foes[i].__uid = foes[i].uid; }
      for(i = 0; i < foes.length; i++){
        var V2 = foes[i];
        // a shielded glider isn't worth a bullet — bots hold fire until the canopy's packed
        if(V2.__me ? meShielded(t) : botShielded(V2, t)) continue;
        var dd = Math.hypot(V2.x - x, (V2.y - P.ai.body.y) || 0, V2.z - z);  // 3D: a ledge above is genuinely far
        if(dd <= BOT_RANGE && los([x, P.ai.body.y + FS.EYE, z], [V2.x, V2.y + FS.EYE, V2.z])){
          // every visible foe is a candidate, nearer likelier; whoever we already chase gets a bonus
          var w2 = 1/(0.2 + dd) * (cur && foes[i].__uid === cur.__uid ? 2.2 : 1);
          pool.push(V2); wts.push(w2); wt += w2;
        }
      }
      if(wt > 0){
        var roll = FS.mb32((P.slot*131 + ((t*1000)|0)) & 0xFFFF)*wt;
        for(i = 0; i < pool.length; i++){ roll -= wts[i]; if(roll <= 0){ pick = pool[i]; break; } }
        if(!pick) pick = pool[pool.length - 1];
        pickPos = [pick.x, pick.z];
      }
      ai.thinkAt = t + BOT_THINK;
      if(pick){
        var same = cur && pick.__uid === cur.__uid;
        if(!same) ai.seenAt = t; ai.target = pick;
        ai.targetPos = pickPos.slice();        // remember where I last saw them, for a chase
        ai.lostPos = null;
      } else {
        if(ai.target){ ai.lostPos = ai.targetPos ? ai.targetPos.slice() : null; ai.target = null; ai.seenAt = null; }
      }
      if(!ai.target){
        // hunt: walk to where the last target went; only fall back to patrol once that's searched
        if(ai.lostPos && Math.hypot(ai.lostPos[0] - x, ai.lostPos[1] - z) > 2.0){
          ai.goal = ai.lostPos.slice();
        } else {
          ai.lostPos = null;
          // push toward the middle of the map when idle, not the spawn corner
          var w = WAYPOINTS[ai.wpi % WAYPOINTS.length];
          if(Math.hypot(x - w[0], z - w[1]) < 1.5) ai.wpi++;
          ai.goal = w.slice();
        }
      } else {
        // stalkers close on a seen target — do NOT freeze at 18 m and wait under fire like a
        // shooting-gallery duck. Easy/Normal hold ground until ~18 m; Hard presses to ~10 m.
        var pressTo = diffFor(P).id === "hard" ? 10 : diffFor(P).id === "sweaty" ? 6 : 18;
        ai.goal = pickPos && Math.hypot(pickPos[0] - x, pickPos[1] - z) > pressTo ? pickPos.slice() : null;
      }
    }
    // -- turn and aim: track the target's current position, not the last think tick's --
    var aim_y = ai.aim_y, aim_p = ai.aim_p, tgt2 = ai.target, targetAlive = false, txNow = 0, tyNow = 0, tzNow = 0, tvx = 0, tvz = 0;
    if(tgt2){
      targetAlive = !!(tgt2.__me ? !V.dead : !tgt2.dead);
      var cur = tgt2.__me ? {x: V.me.x + Math.cos(V.yaw)*PEEK_SIDE*(V.peek || 0), y: V.me.y, z: V.me.z + Math.sin(V.yaw)*PEEK_SIDE*(V.peek || 0), vx: V.me.vx || 0, vz: V.me.vz || 0} : tgt2;
      txNow = cur.x; tyNow = cur.y; tzNow = cur.z; tvx = cur.vx || 0; tvz = cur.vz || 0;
    }
    if(targetAlive){
      var df = diffFor(P);
      var wp = FS.WEAPONS[P.w] || FS.LAUNCH;
      // lead and pitch must both see the true 3D range — a target on a ledge above is
      // genuinely farther than its shadow on the floor, so it earns more lead time
      var lead = Math.min(0.25, Math.hypot(txNow - x, (tyNow - P.ai.body.y) || 0, tzNow - z)/wp.range);
      var tx = txNow + tvx*lead, tz = tzNow + tvz*lead, ty = tyNow + ((P.n + 1) % 3 === 0 ? 1.35 : 0.9);
      var want_yaw = Math.atan2(tx - x, -(tz - z))*180/Math.PI;
      var flat = Math.max(0.1, Math.hypot(tx - x, tz - z));
      var want_pitch = Math.atan2(ty - (P.ai.body.y + FS.EYE), flat)*180/Math.PI;
      if(t - ai.errAt >= BOT_THINK){
        ai.errAt = t;
        var seed = ((t*1000)|0) & 0xFFFF;
        ai.err_y = (FS.mb32(seed)*2 - 1)*BOT_AIM*df.aim*180/Math.PI;
        ai.err_p = (FS.mb32((seed + 7919) & 0xFFFF)*2 - 1)*BOT_AIM*df.aim*180/Math.PI;
        // close-range mercy, by distance: inside df.cqc metres the aim is pushed PAST the
        // target's angular size (a body spans several degrees up close), so the shot
        // physically cannot hit; fades out by cqc+12 m. Easy whiffs point-blank,
        // Sweaty (cqc 0) never does.
        if(df.cqc > 0){
          var flat2 = Math.hypot(txNow - x, tzNow - z);
          if(flat2 < df.cqc + 12){
            // the target's half-extent in degrees at this range (body ~0.6 m wide)
            var bodyHalf = flat2 > 0.1 ? Math.atan(0.3/flat2)*180/Math.PI : 90;
            var push = (bodyHalf + 1.5)*Math.max(0, Math.min(1, (df.cqc + 12 - flat2)/12));
            if(push > 0.2){
              if(Math.abs(ai.err_y) < push) ai.err_y = (ai.err_y >= 0 ? 1 : -1)*push;
              if(Math.abs(ai.err_p) < push) ai.err_p = (ai.err_p >= 0 ? 1 : -1)*push;
            }
          }
        }
      }
      want_yaw += ai.err_y; want_pitch = clamp(want_pitch, -89, 89) + ai.err_p;
      var stepDeg = BOT_TURN*df.turn*180/Math.PI*(1/60);
      var dy = ((want_yaw - aim_y + 540) % 360) - 180; aim_y += clamp(dy, -stepDeg, stepDeg);
      aim_p += clamp(want_pitch - aim_p, -stepDeg, stepDeg);
      ai.aim_y = aim_y; ai.aim_p = aim_p;
      // -- fire: only when the target is actually in the sights right now (visible this frame) --
      // Aim the body, not the eyes (EYE is your head): eye-level LOS false-negatives when a
      // target is crouched or over a slope — the bot thinks it can't fire, plainly staring at you
      var clearNow = los([x, P.ai.body.y + FS.EYE, z], [txNow, tyNow + 0.9, tzNow]);
      // launcher bots: a real missile with their aim; one in the air at a time, like the player
      if(P.w === 3){
        if(clearNow && ai.seenAt != null && t - ai.seenAt >= BOT_REACT*df.react && t >= ai.fireAt + 1.2 - 0.01 &&
           (P.mag[3]|0) > 0 && !P.reloadAt && !V.missiles.some(function(m){ return m.owner === P; })){
          ai.fireAt = t; P.mag[3]--; P.n++; P.protUntil = 0;
          var ldir = FS.dirOf(aim_y, clamp(aim_p, -89, 89));
          spawnMissile(t, [x, P.ai.body.y + FS.EYE, z], ldir, P);
          E.sfx("shot", {w: 3});
          if(P.mag[3] <= 0 && (P.res[3]|0) > 0) P.reloadAt = t + FS.LAUNCH.reload;
        }
      } else if(clearNow && ai.seenAt != null && t - ai.seenAt >= BOT_REACT*df.react && t >= ai.fireAt + Math.max(wp.interval, wp.interval*(df.rate || 1)) - 0.01 && P.mag[P.w] > 0 && !P.reloadAt){
        ai.fireAt = t; P.mag[P.w]--; P.n++; P.protUntil = 0;
        var sp = FS.spreadOf(P.n, P.w), dir = FS.dirOf(aim_y + sp[0], clamp(aim_p + sp[1], -89, 89));
        var eyeAt = [x, P.ai.body.y + FS.EYE, z];
        var reach = FS.rayMap(MAP, eyeAt, dir, wp.range);
        botJudge(P, t, eyeAt, dir, reach);
        if(P.mag[P.w] <= 0 && P.res[P.w] > 0 && !P.reloadAt) P.reloadAt = t + wp.reload;
      }
      if(clearNow) ai.targetPos = [txNow, tzNow];  // keep the trail fresh while they're still exposed
    } else if(ai.goal){
      var want_yaw2 = Math.atan2(ai.goal[0] - x, -(ai.goal[1] - z))*180/Math.PI;
      var step2 = BOT_TURN*diffFor(P).turn*180/Math.PI*(1/60);
      var dy2 = ((want_yaw2 - aim_y + 540) % 360) - 180; aim_y += clamp(dy2, -step2, step2); ai.aim_y = aim_y;
    }
    // -- reload --
    if(P.reloadAt && t >= P.reloadAt){
      var rw = P.w, magW = (FS.WEAPONS[rw] || FS.LAUNCH).mag;
      var take = Math.min(magW - P.mag[rw], P.res[rw]);
      P.mag[rw] += take; P.res[rw] -= take; P.reloadAt = 0;
    }
    // -- move (same physics as the client, stepped in update; a descending glide is drift) --
    if(ai.dropUntil && t < ai.dropUntil){
      ai.wx = 0; ai.wz = 0;  // a gliding bot rides the wind down; movement resumes on the ground
    } else if(ai.goal){
      var dx = ai.goal[0] - x, dz = ai.goal[1] - z, ln2 = Math.hypot(dx, dz);
      ai.wx = ln2 > 0.3 ? dx/ln2 : 0; ai.wz = ln2 > 0.3 ? dz/ln2 : 0;
    } else { ai.wx = 0; ai.wz = 0; }
    P.yaw = ai.aim_y*Math.PI/180; P.pitch = ai.aim_p*Math.PI/180;
    // write-through to the snap-sampled mesh position
    P.snaps.length = 0; P.snaps.push({t: t, x: ai.body.x, y: ai.body.y, z: ai.body.z, yaw: P.yaw, pitch: P.pitch});
  }
  function botsStep(dt, t){
    others().forEach(function(P){
      if(!P.bot || P.gone || V.phase !== "round") return;
      if(P.dead) return;
      var steps = Math.max(1, Math.round(dt/(1/60)));
      for(var s = 0; s < steps; s++) botStep(P, t - dt + dt*(s + 1)/steps);
      // physics at the client's STEP — or glide it: a descending bot sinks gently with the
      // ground's own collide-and-slide absent (no FS.move during a drop, by design)
      var ai = P.ai, acc = dt;
      if(ai.dropUntil && t < ai.dropUntil){
        var floor = FS.topUnder(MAP, ai.body.x, ai.body.z, ai.body.y);
        ai.body.y = Math.max(floor, ai.body.y - FS.DROP_SINK*dt);
        if(ai.body.y <= floor){ ai.dropUntil = 0; P.protUntil = t + 1.5; }
      } else {
        if(ai.dropUntil) ai.dropUntil = 0;
        while(acc >= STEP){ FS.move(MAP, ai.body, ai.wx || 0, ai.wz || 0, false, STEP); acc -= STEP; }
      }
      P.x = ai.body.x; P.y = ai.body.y; P.z = ai.body.z; P.moving = 1;
    });
  }
  function botsRespawn(t){
    others().forEach(function(P){
      if(!P.bot || !P.dead || P.gone) return;
      if(t >= (P.deadAt || 0) + RESPAWN){
        // v4: a bot respawn is a parachute drop too — the same random point the player would
        // get, on the bot's own side (randomDropSpot keeps the halves apart)
        var dsp = dropOK() ? randomDropSpot(t, P.slot, P.team) : botSpawnFor(P);
        var sp = dsp.slice(); sp[1] = dropOK() ? FS.DROP_HEIGHT : sp[1];
        botPlace(P, sp, t);
        onEvent({ev: "spawn", user: P.uid, x: Math.round(P.x*100), y: Math.round(P.y*100), z: Math.round(P.z*100), r: Math.round(P.yaw*18000/Math.PI), e: 0, drop: true});
      }
    });
  }
  function botsRoundEnd(t){
    var lim = V.limit, time = V.endsAt && t >= V.endsAt;
    var a0 = [], a1 = [];
    [{__me: 1}].concat(others()).forEach(function(P){
      var team = P.__me ? V.team : P.team, dead = P.__me ? V.dead : P.dead, gone = P.__me ? false : P.gone;
      if(gone) return;
      (team === 0 ? a0 : a1).push(P);
    });
    if(V.scores[0] >= lim || V.scores[1] >= lim || time || !a0.length || !a1.length){
      endBotMatch(t);
    }
  }
  function endBotMatch(t){
    V.phase = "done";
    var rows = [{user: {id: myId(), name: "You"}, place: 0, team: V.team, bot: 0, kills: V.kills|0, deaths: V.deaths|0}];
    others().forEach(function(P){ rows.push({user: {id: P.uid, name: P.name}, place: 0, team: P.team, bot: 1, kills: P.kills|0, deaths: P.deaths|0}); });
    rows.sort(function(a, b){ return a.team - b.team || b.kills - a.kills || a.deaths - b.deaths; });
    var teamThen = -1, place = 0;
    rows.forEach(function(r, i){ if(r.team !== teamThen){ teamThen = r.team; place = i + 1; } r.place = place; });
    rows.push({teams: [V.scores[0], V.scores[1]]});
    V.results = rows;
    showResults(rows);
    if(IN) IN.unlock();
  }
  function startBotMatch(){
    if(!MAP) return;
    var svb = ssave(), ts = TEAM_SIZES.indexOf(svb.tsize|0) >= 0 ? svb.tsize|0 : 3,
        mins = clamp(Math.round(svb.minutes|0 || 3), 1, 60), lim = clamp(Math.round(svb.kills|0 || 10), 1, 500);
    resetMatch(); V.mode = "bots"; V.off = null; V.k = null; V.watch = null;
    V.minutes = mins; V.limit = lim; V.tsize = ts; V.scores = [0, 0]; V.kills = 0; V.deaths = 0; V.team = 0;
    // nothing is on the map at the start: every item arrives mid-match from a drop clock
    // (totems every TOTEM_GAP, health/ammo packs every PACK_GAP — each regardless of
    // whether the previous drop was taken). Practice keeps the range's fixed packs.
    V.items = MAP.pickups.map(function(){ return 0; }); V.itemBack = null;
    ensureDropSlots();
    while(V.items.length < MAP.pickups.length) V.items.push(0);   // the drop slots exist, empty, all match
    var s = botSpawnFor({team: 0, slot: 0});
    placeMe(s[0], s[1], s[2], s[3]); fullAmmo(); V.hp = 100; V.dead = false; V.life = 1; V.protUntil = now() + 1.5; V.mySlot = 0;
    var t = now();
    for(var b = 1; b < ts; b++) addBot(0, b, t);
    for(var e = 1; e <= ts; e++) addBot(1, e, t);
    renderBoard();
    V.phase = "warmup"; V.goAt = t + 3; V.endsAt = V.goAt + V.minutes*60;
    V.totemAt = t + 3; V.packAt = t + 3; V.kinds = [];  // set at warmup; the round's start re-anchors both (+8 s / +6 s)
    showStage(true); if(R3) R3.buildMap();
    // straight in: the Play click is the gesture pointer lock needs, so grab the mouse now
    // instead of leaving the player on the "click here to play" pause card
    if(canvas) canvas.focus();
    if(IN) IN.lock();
    say("You're on team "+TEAMS[0].n+". "+ts+"v"+ts+" on "+MAP.name+": "+(ts === 1 ? "just you" : "you and "+(ts === 2 ? "one teammate" : (ts - 1)+" teammates"))+" vs "+(ts === 1 ? "one bot" : ts+" bots")+", "+mins+" minute"+(mins === 1 ? "" : "s")+", first to "+lim+". Enemy names are red. Esc gives the mouse back.");
  }
  function endPractice(){
    V.phase = "done";
    var sv = ssave(), best = sv.best|0;
    if(V.score > best){ sv.best = V.score; api.persist(); api.toast("🎯 New best on the range: "+V.score+" drones"); }
    var acc2 = V.shots ? Math.round(100*V.score/V.shots) : 0;
    cardBox.textContent = "";
    cardBox.appendChild(api.mk("b", null, "Time! "+V.score+" drones · "+acc2+"% of shots"));
    var row = api.mk("div", "vg-row");
    row.appendChild(api.btn("Again", "primary", function(){ startPractice(); }));
    row.appendChild(api.btn("Menu", "", function(){ resetMatch(); showStage(false); renderMenu(); }));
    cardBox.appendChild(row); cardBox.classList.remove("hidden");
    if(IN) IN.unlock();
    say("Time is up: "+V.score+" drones hit, best "+Math.max(best, V.score)+".");
  }

  /* ---------- multiplayer: the server's view ---------- */
  function requestView(){ if(MP) MP.send("strike", "view"); }
  function inMatch(view){ return (view.players || []).some(function(p){ var id = p.user && (p.user.userId || p.user.id); return id === myId() && !p.gone; }); }
  function uidOf(p){ return p.user && (p.user.userId || p.user.id); }
  function applyView(view){
    V.round = view; V.gotView = true;
    if(V.mode !== "mp" || !view) { renderMenu(); return; }
    V.minutes = view.minutes|0 || 5; V.limit = view.limit|0 || 30; V.tsize = view.tsize|0 || 2;
    if(Array.isArray(view.scores)) V.scores = [view.scores[0]|0, view.scores[1]|0];
    if(view.phase === "idle"){ V.watch = null; if(V.phase !== "idle"){ resetMatch(); showStage(false); } V.results = view.results || V.results; renderMenu(); return; }
    if(view.phase === "done"){ V.results = view.results; if(V.phase === "warmup" || V.phase === "round") showResults(view.results); V.watch = null; V.killCam = null; V.phase = "done"; renderMenu(); return; }
    if(inMatch(view)) V.watch = null;
    else if(!V.watch){ renderMenu(); return; }
    else if(!(view.players || []).some(function(p){ return uidOf(p) === V.watch && !p.gone; })){
      var first = (view.players || []).filter(function(p){ return p.user && !p.gone; })[0]; V.watch = first ? uidOf(first) : null;
      if(!V.watch){ renderMenu(); return; }
    }
    var fresh = V.phase === "idle" || V.phase === "done";
    if(fresh){ resetMatch(); V.off = null; V.k = null; }
    var seen = {};
    (view.players || []).forEach(function(p){
      var uid = uidOf(p); if(!uid) return; seen[uid] = 1;
      var P = ensure(uid, {name: nameOf(p.user), slot: p.slot|0, team: clamp(p.team|0, 0, 1), bot: !!p.bot, ch: clamp(p.char|0, 0, CHARS.length - 1),
        kills: p.kills|0, deaths: p.deaths|0, hp: p.hp|0, dead: !!p.dead, away: !!p.away, gone: !!p.gone, w: p.w|0,
        cos: (p.user && p.user.cos) || null});
      if(uid === myId()){
        V.mySlot = p.slot|0; V.team = clamp(p.team|0, 0, 1); V.kills = p.kills|0; V.deaths = p.deaths|0; V.hp = p.hp|0;
        if(fresh || V.life !== (p.e|0) || view.phase === "warmup"){
          placeMe((+p.x || 0)/100, (+p.y || 0)/100, (+p.z || 0)/100, (+p.r || 0)/100);
          V.life = p.e|0; fullAmmo(); V.w = p.w|0;
        }
        V.dead = !!p.dead;
      } else if(fresh || P.snaps.length === 0){
        P.x = (+p.x || 0)/100; P.y = (+p.y || 0)/100; P.z = (+p.z || 0)/100; P.yaw = (+p.r || 0)*Math.PI/18000;
      }
    });
    V.order.slice().forEach(function(uid){ if(!seen[uid]) drop(uid); });
    ensureDropSlots();   // the server's slot list includes the drop rings; match its layout
    V.items = Array.isArray(view.items) ? view.items.slice() : MAP.pickups.map(function(){ return 1; });
    while(V.items.length < MAP.pickups.length) V.items.push(0);
    V.kinds = Array.isArray(view.kinds) ? view.kinds.slice() : [];
    while(V.kinds.length < MAP.pickups.length) V.kinds.push("");
    // a joiner mid-round can't rebuild meshes for live drops from the view alone (spots
    // aren't in it), so the drop slots start dark; the next totem/pack event lights them
    for(var di = dropPlan().totems[0]; fresh && di < MAP.pickups.length; di++){
      V.items[di] = 0;
    }
    if(view.phase === "warmup"){ V.phase = "warmup"; V.goAt = now() + (view.goInMs|0)/1000; V.endsAt = V.goAt + V.minutes*60; }
    else if(view.phase === "round"){ V.phase = "round"; V.endsAt = now() + (view.msLeft|0)/1000; }
    showStage(true);
    if(fresh && R3) R3.buildMap();
    renderBoard();
  }
  function slotUid(s){ return V.slots[s]; }
  function onEvent(m){
    if(m.ev === "strike"){
      if(!MAP){ V.round = m.match; V.gotView = true; return; }
      applyView(m.match);
      if(m.by && m.match && m.match.phase === "warmup" && canvas) canvas.focus();
      return;
    }
    if(m.ev === "char"){ var C = V.players[m.user]; if(C){ C.ch = clamp(m.c|0, 0, CHARS.length - 1); if(R3) R3.removePlayer(C); } return; }
    if((V.mode !== "mp" && V.mode !== "bots") || V.phase === "idle") return;
    var t = now();
    // the go whistle also names your side once per match (the lobby assigns you silently,
    // so the match must announce it in the very first moment you regain control)
    if(m.ev === "go"){ V.phase = "round"; V.endsAt = t + (m.msLeft|0)/1000; V.off = null; center.classList.add("hidden");
      flashCenter("Fight! Team "+TEAMS[V.team|0].n);
      say("Fight! You're on team "+TEAMS[V.team|0].n+" — enemy names are red."); return; }
    if(m.ev === "snap"){ onSnap(m, t); return; }
    if(m.ev === "kill"){
      var K = V.players[m.k], D = V.players[m.v];
      if(K) K.kills = m.kills|0; if(D){ D.deaths = m.deaths|0; D.dead = true; D.deadAt = t; }
      if(Array.isArray(m.scores)) V.scores = [m.scores[0]|0, m.scores[1]|0];
      V.feed.unshift({k: K ? K.name : (m.k === myId() ? "You" : "?"), kt: K ? K.team : V.team,
        v: D ? D.name : (m.v === myId() ? "You" : "?"), vt: D ? D.team : V.team, w: m.w|0, hs: !!m.hs, at: t,
        mine: m.k === myId() || m.v === myId()});
      if(m.k === myId()) E.sfx("hit");
      if(V.feed.length > 5) V.feed.length = 5;
      if(m.v === myId()){ V.dead = true; V.deadAt = t; V.deaths = m.deaths|0; V.hp = 0;
        V.wingsUntil = 0; V.leapUntil = 0; V.speedUntil = 0;   // a death pops the movement totems too
        if(K && K.uid !== myId()) snapKillCam(K, m.w|0, !!m.hs);
        say((K ? K.name : "Someone")+" got you. Respawning in "+RESPAWN+" seconds."); }
      if(m.k === myId()){ V.kills = m.kills|0; say("You got "+(D ? D.name : "someone")+(m.hs ? " with a headshot" : "")+". "+TEAMS[0].n+" "+V.scores[0]+" — "+V.scores[1]+" "+TEAMS[1].n+"."); }
      renderFeed(); renderBoard(); return;
    }
    if(m.ev === "spawn"){
      var S = V.players[m.user];
      if(m.user === myId()){
        placeMe((+m.x || 0)/100, (+m.y || 0)/100, (+m.z || 0)/100, (+m.r || 0)/100); V.life = m.e|0; V.dead = false; V.hp = 100; V.killCam = null;
        if(m.drop && dropOK()){ V.dropUntil = t + FS.DROP_TIME; V.me.g = false; }
        fullAmmo(); center.classList.add("hidden"); say("You're back in.");
      } else if(S){ S.dead = false; S.snaps.length = 0; S.last = -1; S.x = (+m.x || 0)/100; S.y = (+m.y || 0)/100; S.z = (+m.z || 0)/100; S.yaw = (+m.r || 0)*Math.PI/18000; S.hp = 100; }
      return;
    }
    if(m.ev === "fix"){
      if((m.e|0) !== V.life) return;
      placeMe((+m.x || 0)/100, (+m.y || 0)/100, (+m.z || 0)/100, null); V.fixAt = t; return;
    }
    if(m.ev === "ammo"){
      if(Array.isArray(m.mag)) V.mag = m.mag.map(function(n){ return n|0; });
      if(Array.isArray(m.res)) V.res = m.res.map(function(n){ return n|0; });
      if(!m.reloading) V.reloadAt = 0;
      return;
    }
    if(m.ev === "pick"){
      V.items[m.i|0] = 0;
      var knd = m.kind || (V.kinds && V.kinds[m.i|0]) || (MAP.pickups[m.i|0] && MAP.pickups[m.i|0].id[0] === "h" ? "health" : "ammo");
      var picker = V.players[m.user], pname = m.user === myId() ? "You" : (picker ? picker.name : "?");
      feedItem(pname, picker ? picker.team : V.team, knd, t);
      if(m.user === myId()){
        V.hp = m.hp|0; if(Array.isArray(m.res)) V.res = m.res.map(function(n){ return n|0; });
        if(knd === "health") say("Health pack: "+V.hp);
        else if(knd === "ammo") say("Ammo pack");
        else if(TOTEMS[knd]){
          // MP: the server judged the effect; the local apply is only the cloak/shield's
          // local clock + the say (charm/freeze marks and the decoy ride the snapshot)
          if(knd === "cloak"){ V.cloakUntil = t + TOTEMS.cloak.dur; }
          if(knd === "shield"){ V.shieldTotemUntil = t + TOTEMS.shield.dur; }
          if(knd === "wings"){ V.wingsUntil = t + TOTEMS.wings.dur; }
          if(knd === "leap"){ V.leapUntil = t + TOTEMS.leap.dur; }
          if(knd === "speed"){ V.speedUntil = t + TOTEMS.speed.dur; }
          if(knd === "teleport" && m.tx != null && m.tz != null){
            // the server moved us: snap the prediction to where it put us (ground, standing)
            placeMe((+m.tx)/100, 0, (+m.tz)/100, null);
            V.dropUntil = 0; V.wingsUntil = 0;
          }
          say(TOTEMS[knd].n+" taken.");
        }
        else say(TOTEMS[knd] ? TOTEMS[knd].n : "Pack");
      }
      return;
    }
    if(m.ev === "totem"){
      // a fresh totem the server dropped (a recycled slot): same bookkeeping the solo
      // spawner does — rewrite the slot, kill any pending refill, show the new gem
      var ti = m.i|0, tk = String(m.kind || "");
      if(!TOTEMS[tk]) return;
      ensureDropSlots();
      MAP.pickups[ti] = {id: tk+"-"+ti, kind: tk, at: [(+m.x || 0)/100, 0, (+m.z || 0)/100]};
      V.kinds = V.kinds || [];
      while(V.kinds.length < MAP.pickups.length) V.kinds.push("");
      V.kinds[ti] = tk;
      V.items = V.items || [];
      while(V.items.length < MAP.pickups.length) V.items.push(0);
      V.items[ti] = 1; if(V.itemBack) delete V.itemBack[ti];
      if(R3) R3.addTotem(ti, MAP.pickups[ti]);
      return;
    }
    if(m.ev === "pack"){
      // a fresh loose health/ammo pack the server dropped (a recycled slot of its own ring)
      var pi = m.i|0, pkd = String(m.kind || "") === "health" ? "health" : "ammo";
      ensureDropSlots();
      MAP.pickups[pi] = {id: pkd+"-"+pi, kind: pkd, at: [(+m.x || 0)/100, 0, (+m.z || 0)/100]};
      V.kinds = V.kinds || [];
      while(V.kinds.length < MAP.pickups.length) V.kinds.push("");
      V.kinds[pi] = "";
      V.items = V.items || [];
      while(V.items.length < MAP.pickups.length) V.items.push(0);
      V.items[pi] = 1; if(V.itemBack) delete V.itemBack[pi];
      if(R3) R3.addPack(pi, MAP.pickups[pi]);
      return;
    }
    if(m.ev === "item"){ V.items[m.i|0] = 1; return; }
    if(m.ev === "gone"){ var G = V.players[m.user]; if(G){ G.gone = true; renderBoard(); } return; }
    if(m.ev === "done"){ V.results = m.results; V.phase = "done"; showResults(m.results); if(IN) IN.unlock(); return; }
  }
  function onSnap(m, t){
    if(typeof m.ts === "number"){ var o = t - m.ts/1000; if(V.off === null || o < V.off) V.off = o; }
    if(typeof m.k === "number" && (V.k === null || m.k > V.k)){ V.k = m.k; V.kq = myClock(); }
    if(typeof m.ms === "number") V.endsAt = t + m.ms/1000;
    if(Array.isArray(m.sc)) V.scores = [m.sc[0]|0, m.sc[1]|0];
    (m.p || []).forEach(function(e){
      if(!Array.isArray(e) || e.length < 10) return;
      var uid = slotUid(e[0]); if(!uid) return;
      var P = V.players[uid]; if(!P) return;
      var f = e[8]|0;
      P.hp = e[6]|0; P.w = e[7]|0; P.prot = !!(f & 2); P.away = !!(f & 4); P.gone = !!(f & 8); P.bot = !!(f & 16);
      // bit 32: the drop shield — the server's word on who's an untouchable glider
      if(f & 32){ P.prot = (P.prot|0) | 32; } else { P.prot = (P.prot|0) & ~32; }
      // bits 64/128: the totem marks the server adjudicates in MP (mole gold, paralyser
      // violet) — Infinity while flagged, so the solo clock checks read it as "still on"
      if(f & 64){ if(!P.charmUntil && R3) R3.charm(P); P.charmUntil = Infinity; }
      else if(P.charmUntil){ clearMarks(P); P.charmUntil = 0; }
      if(f & 128){ if(!P.freezeUntil && R3) R3.charm(P, "freeze"); P.freezeUntil = Infinity; }
      else if(P.freezeUntil){ clearMarks(P, "freeze"); P.freezeUntil = 0; }
      // bits 256/512/1024: the movement totems' windows (wings/leap/speed) — Infinity while
      // flagged so the solo clock checks read "still on", exactly like charm/freeze above
      if(uid === myId()){
        if(f & 256) V.wingsUntil = Infinity; else if(V.wingsUntil === Infinity) V.wingsUntil = 1; // not 0: the glide-down after cruise still flies
        if(f & 512) V.leapUntil = Infinity; else if(V.leapUntil === Infinity) V.leapUntil = 0;
        if(f & 1024) V.speedUntil = Infinity; else if(V.speedUntil === Infinity) V.speedUntil = 0;
      }
      if(uid === myId()){ V.hp = e[6]|0; V.prot = P.prot; if(f & 1) V.dead = true; return; }
      if(f & 1){ if(!P.dead) P.deadAt = t; P.dead = true; } else if(P.dead){ P.dead = false; P.snaps.length = 0; P.last = -1; }
      pushSnap(P, e, t);
    });
    (m.s || []).forEach(function(s){
      if(!Array.isArray(s) || s.length < 7) return;
      var shooter = slotUid(s[0]), end = [s[2]/100, s[3]/100, s[4]/100];
      // a launcher shot: no tracer — the "r" rows fly the rocket; one echo per victim carries the damage
      if((s[1]|0) === LAUNCH_W){
        if(shooter === myId()){
          if(s[5] >= 0){
            var LV = V.players[slotUid(s[5])];
            if(LV && s[7] > 0){   // s[7]: the damage the server actually applied (0 through spawn cover)
              V.hitAt = t;
              V.dmgNums.push({x: LV.x, y: LV.y + 1.2, z: LV.z, n: s[7]|0, head: false, at: t, o: null});
              if(V.dmgNums.length > 12) V.dmgNums.shift();
            }
          }
          return;
        }
        if(s[5] === V.mySlot){ V.hurtAt = t; var LP = V.players[shooter]; if(LP) V.hurtDir = Math.atan2(LP.x - V.me.x, -(LP.z - V.me.z)) - V.yaw; }
        return;
      }
      if(shooter === myId()){
        if(s[5] >= 0){
          V.hitAt = t; if(s[6]) V.headAt = t;
          // the server confirmed the hit: float the damage it applied, at the victim.
          // (suppressed while the server still flags the victim spawn-protected — that hit did 0)
          var VIC = V.players[slotUid(s[5])], WP2 = FS.WEAPONS[s[1]|0];
          if(VIC && WP2 && !(VIC.prot & 2)){
            V.dmgNums.push({x: VIC.x, y: VIC.y + (s[6] ? 1.75 : 1.2), z: VIC.z, n: s[6] ? WP2.head : WP2.damage, head: !!s[6], at: t, o: null});
            if(V.dmgNums.length > 12) V.dmgNums.shift();
          }
        }
        return;
      }
      var P = V.players[shooter];
      if(P) addTracer(muzzleOf(P), end, s[1]|0, t, false);
      if(s[5] === V.mySlot && P){
        V.hurtAt = t; V.hurtDir = Math.atan2(P.x - V.me.x, -(P.z - V.me.z)) - V.yaw;
      }
    });
    // "r": the launcher rockets the server is flying — same meshes the solo match uses,
    // positioned straight from the row (no local physics: the server already judged it)
    (m.r || []).forEach(function(r){
      if(!Array.isArray(r) || r.length < 7) return;
      var key = "srv-"+(r[0]|0);
      var mis = null;
      for(var i = 0; i < V.missiles.length; i++) if(V.missiles[i].srvKey === key){ mis = V.missiles[i]; break; }
      if(!mis){
        mis = spawnMissile(t, [r[1]/100, r[2]/100, r[3]/100], [0, 0, 1], null, true);
        mis.srvKey = key; mis.id = "srv"+(r[0]|0);
      }
      mis.x = r[1]/100; mis.y = r[2]/100; mis.z = r[3]/100;
      var ln = Math.sqrt(r[4]*r[4] + r[5]*r[5] + r[6]*r[6]) || 1;   // velocity: aim the nose
      mis.vx = r[4]/ln*FS.MISSILE_V; mis.vy = r[5]/ln*FS.MISSILE_V; mis.vz = r[6]/ln*FS.MISSILE_V;
      mis.px = mis.x; mis.py = mis.y; mis.pz = mis.z;
      mis.lastSrv = t;
      if(R3) R3.rocket(mis, mis);
    });
    // gone from the snapshot: the rocket landed (the "b" rows bring the boom) or expired
    for(var ri = V.missiles.length - 1; ri >= 0; ri--){
      var rm = V.missiles[ri];
      if(rm.srv && t - (rm.lastSrv || t) > 0.3){ if(rm.o && R3) R3.rocket(null, rm); V.missiles.splice(ri, 1); }
    }
    // "b": a blast the server resolved — the boom, the shake and the hurt flash, minus the damage
    // (hp already came in the same snapshot's "p" rows / shot echoes)
    (m.b || []).forEach(function(bm){
      if(!Array.isArray(bm) || bm.length < 3) return;
      var x = bm[0]/100, y = bm[1]/100, z = bm[2]/100;
      if(R3) R3.boom(x, y, z, t);
      E.sfx("boom");
      var near = Math.sqrt((V.me.x - x)*(V.me.x - x) + (V.me.y + 0.9 - y)*(V.me.y + 0.9 - y) + (V.me.z - z)*(V.me.z - z));
      if(!V.dead && near < FS.BLAST_R + 6){
        V.shakeAt = t;
        if(near < FS.BLAST_R){ V.hurtAt = t; V.hurtDir = Math.atan2(x - V.me.x, -(z - V.me.z)) - V.yaw; }
      }
    });
    // "d": the decoy ghosts the server owns in MP — one row per live ghost, reconciled by
    // id; the render pass drops a ghost whose window closed (same meshes a solo drop uses)
    if(Array.isArray(m.d)){
      V.decoys = V.decoys || [];
      for(var di = V.decoys.length - 1; di >= 0; di--){ if(V.decoys[di].srv) V.decoys.splice(di, 1); }
      m.d.forEach(function(dc){
        if(!Array.isArray(dc) || dc.length < 5) return;
        var found = null;
        V.decoys.forEach(function(q){ if(q.srvId === (dc[0]|0)) found = q; });
        if(found){ found.x = dc[1]/100; found.y = dc[2]/100; found.z = dc[3]/100; found.until = t + dc[4]/1000; found.yaw = (dc[5]|0)/100; return; }
        var nd = {x: dc[1]/100, y: dc[2]/100, z: dc[3]/100, until: t + dc[4]/1000,
                  yaw: (dc[5]|0)/100, srv: true, srvId: dc[0]|0};
        V.decoys.push(nd); if(R3) R3.addDecoy(nd);
      });
    }
  }
  function onError(m){
    var e = String(m.error || "");
    if(/unknown game/.test(e)){ V.unsupported = true; renderMenu(); return true; }
    return false;
  }

  /* ---------- remote players: snapshot interpolation on the server's clock ---------- */
  function pushSnap(P, e, t){
    var key = e[9]|0;
    if(key < 0 || (P.last >= 0 && key <= P.last)) return;
    P.last = key;
    var st = key/1000;
    E.jitter(P, V.off === null ? 0 : clamp(t - (st + V.off), 0, JIT_MAX));
    var sn = P.snaps, x = e[1]/100, y = e[2]/100, z = e[3]/100, L = sn[sn.length - 1];
    var tp = L && (Math.abs(x - L.x) > 6 || Math.abs(z - L.z) > 6);
    if(tp) sn.length = 0;
    sn.push({t: st, x: x, y: y, z: z, yaw: (e[4]|0)*Math.PI/18000, pitch: (e[5]|0)*Math.PI/18000});
    if(sn.length > SNAPS) sn.shift();
    if(tp || sn.length === 1){ P.x = x; P.y = y; P.z = z; P.yaw = sn[sn.length - 1].yaw; }
  }
  function sampleSnaps(P, t, dt){
    var sn = P.snaps, n = sn.length; if(!n || V.off === null) return;
    var rt = t - V.off - INTERP - P.jit, x, y, z, yaw, pitch;
    P.seen = rt;
    if(rt <= sn[0].t){ x = sn[0].x; y = sn[0].y; z = sn[0].z; yaw = sn[0].yaw; pitch = sn[0].pitch; }
    else if(rt >= sn[n-1].t){ var L = sn[n-1]; x = L.x; y = L.y; z = L.z; yaw = L.yaw; pitch = L.pitch; P.seen = L.t; }
    else {
      for(var i = n - 1; i > 0 && sn[i-1].t > rt; i--){}
      var A = sn[i-1], B = sn[i], u = (rt - A.t)/((B.t - A.t) || 1);
      x = A.x + (B.x - A.x)*u; y = A.y + (B.y - A.y)*u; z = A.z + (B.z - A.z)*u; yaw = angLerp(A.yaw, B.yaw, u); pitch = A.pitch + (B.pitch - A.pitch)*u;
    }
    var sp = dt > 0 ? Math.sqrt((x - P.x)*(x - P.x) + (z - P.z)*(z - P.z))/dt : 0;
    P.moving += ((sp > 0.6 ? 1 : 0) - P.moving)*Math.min(1, dt*8);
    P.x = x; P.y = y; P.z = z; P.yaw = yaw; P.pitch = pitch;
  }
  function muzzleOf(P){ var f = FS.dirOf(P.yaw*180/Math.PI, 0); return [P.x + f[0]*0.5 - f[2]*0.2, P.y + 1.25, P.z + f[2]*0.5 + f[0]*0.2]; }

  /* ---------- shooting ---------- */
  // your eyes, slid sideways by the lean (PEEK_SIDE at full lean); shots come from here, and the
  // bots aim at the same spot — peeking shows your head, it doesn't move your feet
  function eye(){
    var px = Math.cos(V.yaw)*PEEK_SIDE*(V.peek || 0), pz = Math.sin(V.yaw)*PEEK_SIDE*(V.peek || 0);
    return [V.me.x + px, V.me.y + FS.EYE, V.me.z + pz];
  }
  function myMuzzle(){
    var f = FS.dirOf(V.yaw*180/Math.PI, V.pitch*180/Math.PI), r = [Math.cos(V.yaw), 0, Math.sin(V.yaw)], e = eye();
    return [e[0] + f[0]*0.55 + r[0]*0.22, e[1] + f[1]*0.55 - 0.2, e[2] + f[2]*0.55 + r[2]*0.22];
  }
  function canShoot(t){
    if(V.dead || V.paused) return false;
    if(V.mode === "mp" && V.phase !== "round") return false;
    if((V.mode === "practice" || V.mode === "bots") && V.phase !== "round") return false;
    return t >= V.readyAt && t >= V.nextShot && !V.reloadAt && magOf(V.w)[V.w === LAUNCH_W ? 0 : V.w] > 0;
  }
  function startReload(t){
    var w = V.w, wp = W(), mag = magOf(w), res = resOf(w), i = w === LAUNCH_W ? 0 : w;
    if(V.reloadAt || mag[i] >= wp.mag || res[i] <= 0 || V.dead) return;
    V.reloadAt = t + wp.reload;
    if(V.mode === "mp" && MP) MP.send("strike", "reload", {q: myClock(), e: V.life});
    say("Reloading");
  }
  function finishReload(t){
    if(V.reloadAt && t >= V.reloadAt){
      var w = V.w, wp = W(), mag = magOf(w), res = resOf(w), i = w === LAUNCH_W ? 0 : w;
      var take = Math.min(wp.mag - mag[i], res[i]);
      mag[i] += take; res[i] -= take; V.reloadAt = 0;
    }
  }
  function switchTo(w, t){
    w = clamp(w|0, 0, weaponCount() - 1);
    if(w === V.w || V.dead) return;
    V.w = w; V.reloadAt = 0; V.readyAt = t + SWITCH;
    if(V.mode === "mp" && MP) MP.send("strike", "weapon", {w: w, q: myClock()});  // 0-3 (a v1 Arena clamps to its 3)
    say(W().name);
  }
  // Fire one shot: drawn at once; in a match the server says whether it hit.
  function fire(t){
    var wp = W(), e = eye();
    E.sfx("shot", {w: V.w});
    var mag = magOf(V.w); mag[V.w === LAUNCH_W ? 0 : V.w]--; V.nextShot = t + wp.interval; V.n++; V.shots++;
    V.flashAt = t; V.kick = calm() ? 0.15 : 1;
    V.protUntil = V.protUntil || 0; if(V.protUntil) V.protUntil = 0; // any shot drops your own spawn cover (strike.py:772)
    if(V.w === LAUNCH_W){
      // lob a real missile that flies and blasts — the server flies it in mp, we do solo
      var ld = FS.dirOf(V.yaw*180/Math.PI, V.pitch*180/Math.PI);
      if(V.mode === "mp"){
        var lmsg = {w: LAUNCH_W, ox: Math.round(e[0]*100), oy: Math.round(e[1]*100), oz: Math.round(e[2]*100),
                    r: Math.round(((V.yaw*180/Math.PI % 360) + 360) % 360*100), p: Math.round(V.pitch*180/Math.PI),
                    q: myClock(), ip: 0, e: V.life};
        if(V.k !== null){ lmsg.k = V.k; lmsg.kq = V.kq; }
        if(MP) MP.send("strike", "fire", lmsg);
        if(magOf(V.w)[0] <= 0) startReload(t);
        return;   // the "r" snapshot rows draw the rocket
      }
      spawnMissile(t, e, ld);
      if(R3 && R3.kick) R3.kick();
      if(mag[0] <= 0) startReload(t);
      return;
    }
    var sp = FS.spreadOf(V.n, V.w), yawD = V.yaw*180/Math.PI, pitD = V.pitch*180/Math.PI;
    var d = FS.dirOf(yawD + sp[0], clamp(pitD + sp[1], -PITCH_MAX, PITCH_MAX));
    var reach = FS.rayMap(MAP, e, d, wp.range), end = reach, aimed = null, aimedHead = false;
    if(V.mode === "practice"){
      V.drones.forEach(function(D){
        if(D.downUntil > t) return;
        var c = dronePos(D, t - V.startAt), b = [c[0]-0.45, c[1]-0.45, c[2]-0.45, c[0]+0.45, c[1]+0.45, c[2]+0.45], h = FS.rayBox(e, d, b);
        if(h !== null && h < end){ end = h; aimed = D; }
      });
      if(aimed){ aimed.downUntil = t + 1.5; V.score++; V.hitAt = t; }
    } else if(V.mode === "bots"){
      if(V.protUntil) V.protUntil = 0; // firing drops your own spawn cover (strike.py:772)
      others().forEach(function(P){
        if(!P.bot || P.dead || P.gone || P.team === V.team) return;
        if(botShielded(P, t)) return;  // the drop shield: your bullets bounce off a glider
        var h = FS.rayPlayer(e, d, P.x, P.y, P.z, end);
        if(h && h[0] < end){ end = h[0]; aimed = P; aimedHead = h[1]; }
      });
      if(aimed){
        var dmg = aimedHead ? wp.head : wp.damage;
        aimed.hp -= dmg;
        V.hitAt = t; if(aimedHead) V.headAt = t;
        // the number floats where the shot landed: white for body, red and bigger for a headshot
        V.dmgNums.push({x: aimed.x, y: aimed.y + (aimedHead ? 1.75 : 1.2), z: aimed.z, n: dmg, head: aimedHead, at: t, o: null});
        if(V.dmgNums.length > 12) V.dmgNums.shift();
        if(aimed.hp <= 0 && !aimed.dead) endBotLife({uid: myId(), kills: V.kills, team: V.team, w: V.w, name: "You"}, aimed, t, aimedHead);
        else say((aimedHead ? "Headshot" : "Hit")+" — "+aimed.name+" ("+Math.max(0, aimed.hp|0)+" hp)");
      }
    } else {
      others().forEach(function(P){
        if(P.dead || P.gone || P.team === V.team) return;
        var h = FS.rayPlayer(e, d, P.x, P.y, P.z, end);
        if(h){ end = h[0]; aimed = P; }
      });
      // the delay you were seeing the others with: the target's if you aimed at one
      var avg = 0, n = 0; if(V.off !== null) others().forEach(function(P){ if(P.snaps.length){ avg += t - V.off - P.seen; n++; } });
      var ip = aimed && V.off !== null ? t - V.off - aimed.seen : n ? avg/n : INTERP;
      var msg = {w: V.w, ox: Math.round(e[0]*100), oy: Math.round(e[1]*100), oz: Math.round(e[2]*100),
                 r: Math.round(((yawD % 360) + 360) % 360*100), p: Math.round(pitD*100), q: myClock(), ip: Math.round(clamp(ip, 0, 1)*1000), e: V.life};
      if(V.k !== null){ msg.k = V.k; msg.kq = V.kq; }
      if(MP) MP.send("strike", "fire", msg);
    }
    addTracer(myMuzzle(), [e[0] + d[0]*end, e[1] + d[1]*end, e[2] + d[2]*end], V.w, t, true);
    if(magOf(V.w)[V.w === LAUNCH_W ? 0 : V.w] <= 0) startReload(t);
  }
  function addTracer(a, b, w, t, mine){
    V.tracers.push({a: a, b: b, w: w, at: t, mine: mine});
    if(V.tracers.length > 40) V.tracers.shift();
    if(R3) R3.tracer(a, b, w, mine);
  }

  /* ---------- the loop ---------- */
  var sendKeyAt = 0;
  function update(dt, t){
    if(!MAP) return;
    if(IN) IN.poll();
    var locked = !!(IN && IN.locked), focused = !!(canvas && document.activeElement === canvas);
    // the weapon wheel (hold Tab): turning consumes this frame's look so your aim stays put
    var wheelNow = !!(IN && !V.paused && !V.dead && !V.watch && IN.down("wheel") && (V.phase === "round" || (V.phase === "warmup" && V.mode !== "mp")));
    if(wheelNow !== V.wheelOpen){
      V.wheelOpen = wheelNow;
      if(wheelNow){ V.wheelSel = V.w; V.wheelLook = null; }
      else if(V.wheelSel >= 0) switchTo(V.wheelSel, t);
      renderWheel(t);
    }
    var playing = V.phase === "round" || V.phase === "warmup";
    pauseBox.classList.toggle("hidden", !(playing && !locked && !focused));
    V.paused = (V.mode === "practice" || V.mode === "bots") && ((playing && !locked && !focused) || !!V.hqPaused);
    if(IN && IN.pressed("help")){ var s = ssave(); s.keysHidden = !s.keysHidden; api.persist(); renderKeys(); }
    if(IN && IN.pressed("score")){ V.board = !V.board; renderBoard(); }
    if(IN && IN.pressed("items")){ V.guide = !V.guide; renderGuide(); }
    if(V.phase === "warmup"){
      var left = Math.ceil(V.goAt - t);
      if(left > 0 && left <= 5 && center.textContent !== String(left)){ flashCenter(String(left)); say(String(left)); }
      if((V.mode === "practice" || V.mode === "bots") && t >= V.goAt){
        V.phase = "round"; V.startAt = t;
        var goTxt = (V.mode === "bots" ? "Fight! " : "Go! ") + "Team " + TEAMS[V.team|0].n;
        flashCenter(goTxt); say(goTxt + ". Enemy names are red.");
        if(V.mode === "bots" && V.totemAt != null) V.totemAt = t + TOTEM_FIRST;   // the round's own clock: first totem in 8 s
        if(V.mode === "bots" && V.packAt != null) V.packAt = t + PACK_FIRST;      // …and the first loose pack in 6 s
      }
    }
    if((V.mode === "practice" || V.mode === "bots") && V.phase === "round"){
      if(V.paused){ V.endsAt += dt; V.startAt += dt; }
      else if(V.mode === "practice" && t >= V.endsAt) endPractice();
      else if(V.mode === "bots") botsRoundEnd(t);
    }
    if(V.mode === "bots"){ if(!V.paused){ botsStep(dt, t); botsRespawn(t); } } // pause freezes the bots too (offline only)
    // scope in: pure camera zoom (spread stays server-replayed — no desync). Hold right-click or the `~ key.
    V.ads = !!(IN && IN.down("aim") && !V.dead && !V.watch);
    // look (slower while scoped: a steadier aim)
    var look = LOOK*(V.ads ? 0.55 : 1);
    var lk = IN && !V.paused ? IN.look() : null;
    if(wheelNow && lk){
      // the mouse becomes the wedge selector; moving little stays on the current one,
      // the neutral center picks "cancel" (GTA's dead zone), anything else picks a wedge
      V.wheelLook = V.wheelLook || {x: 0, y: 0};
      V.wheelLook.x += lk.dx*2.2; V.wheelLook.y += lk.dy*2.2;
      var len = Math.hypot(V.wheelLook.x, V.wheelLook.y);
      if(len > 60){
        // the wedges sit centered on up/right/down/left; atan2's 0 is up, and a wedge's
        // center is its quadrant diagonal — the +π/4 nudges the diagonals into the right wedge
        var a = Math.atan2(V.wheelLook.x, -V.wheelLook.y) + Math.PI/4, seg = Math.floor(a/(Math.PI/2));
        V.wheelSel = ((seg % 4) + 4) % 4;
      }
      renderWheel(t);
    } else if(lk){
      V.yaw += lk.dx*look; V.pitch = clamp(V.pitch - lk.dy*look, -PITCH_MAX*Math.PI/180, PITCH_MAX*Math.PI/180);
    }
    // lean: Q/E slide your eyes (and your gun) sideways out of cover — the camera rolls, and your
    // head goes with it, so what you can see and what can see you are the same thing
    var wantPeek = ((V.phase === "round" || ((V.mode === "practice" || V.mode === "bots") && V.phase === "warmup")) && IN && !V.paused && !V.dead && !V.watch
      ? (IN.down("peekR") ? 1 : 0) - (IN.down("peekL") ? 1 : 0) : 0);
    V.peek = V.peek || 0;
    V.peek += clamp(wantPeek - V.peek, -PEEK_SPEED*dt, PEEK_SPEED*dt);
    var canMove = !V.watch && !V.dead && !V.paused && (V.phase === "round" || ((V.mode === "practice" || V.mode === "bots") && V.phase === "warmup"));
    var ax = canMove && IN ? IN.axis("x") : 0, ay = canMove && IN ? IN.axis("y") : 0;
    var fx = Math.sin(V.yaw), fz = -Math.cos(V.yaw), rx = Math.cos(V.yaw), rz = Math.sin(V.yaw);
    var wx = fx*ay + rx*ax, wz = fz*ay + rz*ax, wl = Math.sqrt(wx*wx + wz*wz); if(wl > 1){ wx /= wl; wz /= wl; }
    var jump = canMove && IN && IN.down("jump");
    var leaping = !!(V.leapUntil && t < V.leapUntil);
    var winged = !!(V.wingsUntil && t < V.wingsUntil);
    // speed totem: plain WASD dashes at SPEED_MULT x sprint pace — no Shift held, no stamina
    // spent (the boost is its own window; the tank keeps refilling underneath it). v1/v2
    // arenas never granted the raised cap, so the boost stays off there like the sprint is
    var boosting = !!(V.speedUntil && t < V.speedUntil && sprintOK());
    // sprint: Shift (or stick click) while a movement key is held, only if this Arena allows it.
    // Stamina drains while sprinting; an empty tank keeps Shift moving at a crawl (SPRINT_SLOW)
    // and refills while you're not sprinting. Client-side only — the server sees raw speed.
    var moving = wl > 0.01;
    var sprintHold = canMove && !V.watch && IN && IN.down("sprint") && sprintOK();
    var sprinting = !!(sprintHold && moving && V.stamina > 0);
    if(sprinting) V.stamina = Math.max(0, V.stamina - FS.STAMINA_DRAIN*dt);
    else V.stamina = Math.min(FS.STAMINA_MAX, V.stamina + FS.STAMINA_REGEN*dt);
    // the speed totem needs none of the tank: while it runs, plain movement key IS a dash
    if(boosting){ sprinting = true; V.stamina = Math.min(FS.STAMINA_MAX, V.stamina + FS.STAMINA_REGEN*dt); }
    // latch the drag: at a 0.09 m/s crawl wl must not flicker the state with stick noise
    V.drag = !!(V.drag || (sprintHold && moving && V.stamina <= 0)) && !!(sprintHold && V.stamina <= 0);
    var dragging = V.drag;
    V.sprinting = sprinting;
    // slide: a grounded sprint's camera dips then settles; pure cosmetics, decays on landing or stop
    V.slideK = clamp(V.slideK + (sprinting && V.me.g ? SLIDE_RISE*dt : -SLIDE_FALL*dt), 0, 1);
    V.slideRoll = V.slideK*0.05;
    if(V.phase !== "idle" && V.phase !== "done" && !V.dead){
      // v4: a drop owns the body's descent — glide left/right at DROP_DRIFT and sink gently,
      // no FS.move gravity/sprint/jump. Touching down (or the window closing) lands you.
      var dropping = !!(dropOK() && V.dropUntil && t < V.dropUntil && !V.me.g);
      // the wings totem flies on its own tallies up: takeoff is automatic the moment you
      // take the totem, cruising is WASD drift at WING_DRIFT with no rise/sink input, and
      // the window's end starts a gentle automatic glide to the ground (no Space needed).
      // Server-side the window covers all of it: rise + cruise + the descent to floor.
      // the body is "flying" whenever the window runs OR the window ended and you're still
      // midair working your way back down from the cruise altitude.
      var flying = !!((winged || (V.wingsUntil && t >= V.wingsUntil && !V.me.g)) && !dropping);
      if(flying){
        var floor = FS.topUnder(MAP, V.me.x, V.me.z, V.me.y); if(floor == null) floor = 0;
        // rise automatically toward WING_TOP while the window runs; glide back down to the
        // floor when it's over. drift horizontally with WASD the whole time.
        var fy = winged ? Math.min(V.me.y + FS.WING_RISE*dt, FS.WING_TOP)
                        : Math.max(floor, V.me.y - FS.WING_SINK*dt);
        var fx = V.me.x + (wx * FS.WING_DRIFT)*dt, fz = V.me.z + (wz * FS.WING_DRIFT)*dt;
        if(FS.overlaps(MAP, fx, fy, fz, 0) < 0){ V.me.x = fx; V.me.z = fz; }   // walls stop a flyer
        if(!winged && fy <= floor){ V.me.y = floor; V.me.vy = 0; V.me.g = true; V.wingsUntil = 0; } // landed — all the way down
        else V.me.y = fy;
        V.me.g = false;   // a flyer is airborne by definition, even at takeoff floor
        V.me.vy = 0;
        acc = 0;
      }
      if(dropping){
        var floor = FS.topUnder(MAP, V.me.x, V.me.z, V.me.y);
        V.me.y = Math.max(floor, V.me.y - FS.DROP_SINK*dt);
        if(V.me.y <= floor){ V.me.g = true; V.dropUntil = 0; V.protUntil = t + 1.5; }
        else {
          V.me.x += (wx * FS.DROP_DRIFT)*dt;
          V.me.z += (wz * FS.DROP_DRIFT)*dt;
        }
        acc = 0;
      } else if(V.dropUntil && t >= V.dropUntil){ V.dropUntil = 0; }
      if(!dropping && !flying){
        acc += dt;
        // the drag must bite at once: scaling only the wish leaves a drained 8.1 m/s sprint
        // coasting at ~walk speed for half a second. Clamp velocity to the crawl cap directly.
        if(dragging){
          var dTop = FS.RUN*FS.SPRINT_SLOW, dSpd = Math.sqrt(V.me.vx*V.me.vx + V.me.vz*V.me.vz);
          if(dSpd > dTop){ V.me.vx *= dTop/dSpd; V.me.vz *= dTop/dSpd; }  // bite: drop to the crawl AT the drain
        }
        var mwx = dragging ? wx*FS.SPRINT_SLOW : wx, mwz = dragging ? wz*FS.SPRINT_SLOW : wz;
        while(acc >= STEP){ FS.move(MAP, V.me, mwx, mwz, jump, STEP, sprinting, boosting ? FS.SPEED_MULT : 1, leaping ? FS.LEAP_MULT : 1); acc -= STEP; }
      }
    } else acc = 0;
    var spd = Math.sqrt(V.me.vx*V.me.vx + V.me.vz*V.me.vz);
    V.bob += spd*dt*(sprinting ? 2.1 : 1.6);
    // weapons
    finishReload(t);
    if(IN && !V.paused && !V.dead && !V.watch && !wheelNow){
      if(IN.pressed("w1")) switchTo(0, t);
      if(IN.pressed("w2")) switchTo(1, t);
      if(IN.pressed("w3")) switchTo(2, t);
      if(IN.pressed("w4")) switchTo(3, t);  // solo-only: no-op in mp (switchTo clamps to the shared roster)
      // cycle: next, wrapping past the end (wheel is directional: down = next, up = previous)
      if(IN.pressed("swap")) switchTo((V.w + 1) % weaponCount(), t);
      var wh = IN.wheel(); if(wh){ switchTo((V.w + (wh > 0 ? 1 : weaponCount() - 1)) % weaponCount(), t); }
      if(IN.pressed("reload")) startReload(t);
      var trig = (IN.mouse[0] && locked) || IN.down("shoot") || IN.down("trig");
      var want = (W().auto ? trig : trig && !V.trigWas)
        && !(V.w === LAUNCH_W && (V.missiles.length > 0 || (V.mode === "mp" && t < V.nextLaunchAt)));  // one rocket in the air at a time
      if(V.w === LAUNCH_W && want && canShoot(t)) V.nextLaunchAt = t + 0.9;   // covers the launch gap before the snapshot carries it
      V.trigWas = trig;
      if(want && canShoot(t)) fire(t);
      else if(want && magOf(V.w)[V.w === LAUNCH_W ? 0 : V.w] <= 0 && !V.reloadAt) startReload(t);
    }
    V.kick = Math.max(0, V.kick - dt*8);
    missilesStep(t, dt);   // solo: flies them; mp: only renders the server's "r" rows
    // pickups (offline: refilled locally; online the server decides)
    if((V.mode === "practice" || V.mode === "bots") && V.phase === "round") MAP.pickups.forEach(function(pk, i){
      if(!V.items[i]) return;
      if(Math.abs(V.me.x - pk.at[0]) > 1.1 || Math.abs(V.me.z - pk.at[2]) > 1.1 || Math.abs(V.me.y - pk.at[1]) > 1.2) return;
      var knd = (V.kinds && V.kinds[i]) || (pk.kind === "health" || pk.kind === "ammo" ? pk.kind : (pk.id && pk.id[0] === "h" ? "health" : "ammo"));
      if(knd === "ammo"){
        var full = FS.WEAPONS.every(function(wp, wi){ return resOf(wi)[wi === LAUNCH_W ? 0 : wi] >= wp.reserve*2; }); if(full) return;
        V.res = FS.WEAPONS.map(function(wp, wi){ return Math.min(wp.reserve*2, V.res[wi] + wp.pack); });
        V.resX = [Math.min(FS.LAUNCH.reserve*2, V.resX[0] + FS.LAUNCH.pack)]; say("Ammo pack");
      } else if(knd === "health"){
        if(V.hp >= 100) return;
        V.hp = Math.min(100, V.hp + 50); say("Health pack: "+V.hp); // HEAL = 50 (strike.py)
      } else if(TOTEMS[knd]){
        // totem effects are one-shot; the solo client applies them right here, MP comes
        // via the server's "pick" event (same shape, judged server-side)
        var msg = totemApply(knd, myId(), t);
        if(!msg) return; // nothing eligible (e.g. a mole with no enemy bots left)
        say(msg);
      } else return;
      feedItem("You", V.team, knd, t);
      // the map's own packs respawn at their spot; a taken drop slot doesn't (the drop
      // clock replaces it with a fresh drop within one gap)
      V.items[i] = 0;
      if(i < BASE_PACKS){ V.itemBack = V.itemBack || {}; V.itemBack[i] = t + 15; }
    });
    // taken packs come back: practice refills them all, bot matches refill the map's own
    // packs (the drop-ring slots never respawn in place — the drop clocks replace them)
    if(V.itemBack) Object.keys(V.itemBack).forEach(function(i){
      if(t >= V.itemBack[i] && (V.mode === "practice" || (+i < BASE_PACKS))){ V.items[i] = 1; delete V.itemBack[i]; }
    });
    // the two drop clocks, independent of each other: a fresh random totem every TOTEM_GAP
    // (mole only with bots) and a fresh random health/ammo pack every PACK_GAP. Each new
    // drop replaces the oldest slot of its ring — taken or not, drops keep coming.
    if(V.mode === "bots" && V.phase === "round" && V.totemAt != null && t >= V.totemAt){
      V.totemAt = t + TOTEM_GAP;
      var anyBot = others().some(function(P){ return P.bot && !P.gone; });
      var seed = ((Math.floor(t*1000) ^ (V.mySlot|0)) >>> 0);
      var idx = totemSpawn(seed, anyBot);
      V.items[idx] = 1; if(V.itemBack) delete V.itemBack[idx];
      V.kinds[idx] = MAP.pickups[idx].kind;
      if(R3) R3.addTotem(idx, MAP.pickups[idx]);
    }
    if(V.mode === "bots" && V.phase === "round" && V.packAt != null && t >= V.packAt){
      V.packAt = t + PACK_GAP;
      var pseed = ((Math.floor(t*1000)*31 ^ (V.mySlot|0)) >>> 0);
      var pidx = packSpawn(pseed);
      V.items[pidx] = 1; if(V.itemBack) delete V.itemBack[pidx];
      V.kinds[pidx] = "";
      if(R3) R3.addPack(pidx, MAP.pickups[pidx]);
    }
    others().forEach(function(P){ sampleSnaps(P, t, dt); });
    if(!V.watch) sendPos(t);
    if(V.dead && (V.mode === "mp" || V.mode === "bots")){
      if(V.killCam && (!V.killCam.ref || V.killCam.ref.gone || V.killCam.ref.dead)) V.killCam = null;
      var rl = Math.max(0, Math.ceil(V.deadAt + RESPAWN - t)); var txt = rl > 0 ? "Respawning in "+rl : "Respawning…";
      if(V.killCam && V.killCam.name) txt = V.killCam.name+"'s view — "+txt;
      if(center.textContent !== txt){ center.textContent = txt; center.classList.remove("hidden"); center.dataset.until = ""; } }
    if(V.dead && V.mode === "bots" && t >= (V.deadAt || 0) + RESPAWN){
      var dropping = false;
      if(dropOK()){ dropping = !!dropSpawn(V.mySlot != null ? V.mySlot : 0); }
      else { var sp = botSpawnFor({team: V.team, slot: V.mySlot != null ? V.mySlot : 0}); placeMe(sp[0], sp[1], sp[2], sp[3]); }
      V.dead = false; V.hp = 100; fullAmmo(); V.killCam = null;
      // PROTECT (strike.py): 1.5 s of ground cover — but a glide takes DROP_TIME, so the
      // clock starts only once your boots touch, or you'd land with the cover already gone
      V.protUntil = dropping ? 0 : t + 1.5;
      center.classList.add("hidden"); say(dropOK() && V.dropUntil > t ? "Glide down: WASD to steer, soft landing at the bottom." : "You're back in.");
    }
    if(center.dataset.until && t > +center.dataset.until){ center.classList.add("hidden"); center.dataset.until = ""; }
  }
  function sendPos(t){
    if(V.mode !== "mp" || !MP || V.phase !== "round" || V.dead || V.offline) return;
    var M = V.me, x = Math.round(M.x*100), y = Math.round(M.y*100), z = Math.round(M.z*100);
    var r = Math.round((((V.yaw*180/Math.PI) % 360) + 360) % 360*100), p = Math.round(V.pitch*18000/Math.PI);
    var key = x+","+y+","+z+","+r+","+p;
    if(t - V.sendAt < SEND_EVERY) return;
    if(key === V.sentKey && t - V.sendAt < KEEPALIVE) return;
    V.sendAt = t; V.sentKey = key;
    var msg = {x: x, y: y, z: z, r: r, p: p, q: myClock(), e: V.life};
    // v3+: the sprint flag that earns this position the raised speed cap. Omit it
    // off so a v1/v2 Arena's referee sees exactly the shape it already knows.
    if(V.sprinting && sprintOK()) msg.sp = true;
    if(V.k !== null){ msg.k = V.k; msg.kq = V.kq; }
    MP.send("strike", "pos", msg);
  }
  function flashCenter(txt){ center.textContent = txt; center.classList.remove("hidden"); center.dataset.until = String(now() + 1.0); }

  /* ---------- HUD ---------- */
  function hudUpdate(t){
    if(WB) WB.update();
    var wp = W(), hp = V.mode === "practice" ? 100 : clamp(V.hp, 0, 100); // bots mode uses V.hp too (damage taken)
    hpBar.style.width = hp+"%"; hpBox.classList.toggle("low", hp <= 30);
    var ht = String(hp); if(hpNum.textContent !== ht) hpNum.textContent = ht;
    var at = V.reloadAt ? "…" : String(magOf(V.w)[V.w === LAUNCH_W ? 0 : V.w]); at += " / "+resOf(V.w)[V.w === LAUNCH_W ? 0 : V.w];
    if(ammoNum.textContent !== at) ammoNum.textContent = at;
    if(ammoName.textContent !== wp.name) ammoName.textContent = wp.name;
    reloadBar.style.width = V.reloadAt ? Math.round(100*clamp(1 - (V.reloadAt - t)/wp.reload, 0, 1))+"%" : "0";
    var tt;
    if(V.mode === "practice") tt = (V.phase === "round" ? mmss((V.endsAt - t)*1000) : V.phase === "warmup" ? mmss(PRACTICE_SECS*1000) : "0:00")+" · "+V.score+(V.score === 1 ? " drone" : " drones");
    else {
      var mine = V.mode === "bots" ? 1 : 0, foes = 0; // bots mode: V.order holds only the bots, you are the +1
      V.order.forEach(function(u){ var P = V.players[u]; if(!P || P.gone) return; if(P.team === V.team) mine++; else foes++; });
      tt = (V.phase === "round" ? mmss((V.endsAt - t)*1000) : V.phase === "done" ? "0:00" : mmss(V.minutes*60000))
        +" · "+TEAMS[0].n+" "+V.scores[0]+" — "+V.scores[1]+" "+TEAMS[1].n+" · "+mine+"v"+foes+" · first to "+V.limit;
    }
    if(topBox.textContent !== tt) topBox.textContent = tt;
    hitm.classList.toggle("on", t - V.hitAt < 0.15); hitm.classList.toggle("head", t - V.headAt < 0.15);
    var hurtOn = t - V.hurtAt < 0.8;
    hurt.classList.toggle("on", hurtOn);
    if(hurtOn) hurt.style.transform = "translate(-50%,-50%) rotate("+Math.round(V.hurtDir*180/Math.PI)+"deg)";
    vignette.classList.toggle("on", !calm() && t - V.hurtAt < 0.25);
    var stamOn = !V.watch && !V.dead && !V.paused && V.stamina < FS.STAMINA_MAX - 0.5;
    stamBox.classList.toggle("hidden", !stamOn);
    if(stamOn){
      stamBar.style.width = Math.round(V.stamina)+"%";
      stamBox.classList.toggle("low", V.stamina <= 0);
    }
    cross.classList.toggle("wide", V.w === 0 && !V.ads);
    var adsCls = V.ads && ADS[V.w] ? ADS[V.w].cls : "";
    cross.classList.toggle("ads", adsCls === "reddot");
    cross.classList.toggle("sniper", adsCls === "sniper");
    scope.classList.toggle("on", adsCls !== "");
    scope.classList.toggle("reddot", adsCls === "reddot");
    scope.classList.toggle("sniper", adsCls === "sniper");
    root.classList.toggle("vg-fps-dead", V.dead);
    // the cloak veil: bots stop seeing me for the window; I see the same rim so it reads
    root.classList.toggle("vg-fps-cloak", !!(V.cloakUntil && t < V.cloakUntil));
    // your own shield, from the inside: rim + pill while the glide runs, the bar drains
    // over DROP_TIME so the packing moment is visible, not just inferable
    var shMe = !V.dead && meShielded(t);
    root.classList.toggle("vg-fps-shield", shMe);
    shOv.classList.toggle("on", shMe);
    shPill.classList.toggle("hidden", !shMe);
    if(shMe){
      var shLeft = V.mode === "mp"
        ? 1   // the server's window: the flag rides the snapshot, no local clock to drain
        : clamp((V.shieldTotemUntil > t ? (V.shieldTotemUntil - t)/TOTEMS.shield.dur : (V.dropUntil - t)/FS.DROP_TIME), 0, 1);
      shBar.style.width = Math.round(shLeft*100)+"%";
    }
    if(V.feed.length && t - V.feed[V.feed.length - 1].at > 6){ V.feed = V.feed.filter(function(f){ return t - f.at <= 6; }); renderFeed(); }
  }
  function renderFeed(){
    feedBox.textContent = "";
    V.feed.forEach(function(f){
      var li = api.mk("li", f.mine ? "me" : null);
      if(f.item){   // a pickup: "Bolt grabbed the Mole" — so the map's items read as contested
        li.appendChild(api.mk("b", null, f.k));
        li.appendChild(api.mk("span", "vg-fps-w", " grabbed "));
        var it = api.mk("b", null, f.item);
        it.style.color = f.hue; li.appendChild(it); feedBox.appendChild(li);
        li.style.borderLeft = "3px solid "+f.hue;
        return;
      }
      li.appendChild(api.mk("b", null, f.k));
      li.appendChild(api.mk("span", "vg-fps-w", " "+(f.w ? "rifle" : "smg")+(f.hs ? " · head" : "")+" "));
      li.appendChild(api.mk("b", null, f.v)); feedBox.appendChild(li);
      li.style.borderLeft = "3px solid "+TEAMS[f.vt|0].c;
    });
  }
  // one pickup into the top-right feed: name (their team colour in the dot) + the item,
  // tinted with the exact hue of its sky column so the feed doubles as the beacon legend
  function feedItem(name, team, kind, t){
    var hue = kind === "health" ? "#f4f4f4" : kind === "ammo" ? "#e0b325" : (TOTEMS[kind] && TOTEMS[kind].hue) || TEAMS[team|0].c;
    var label = kind === "health" ? "a health kit" : kind === "ammo" ? "an ammo box" : (TOTEMS[kind] && TOTEMS[kind].n) || kind;
    V.feed.unshift({k: name, item: label, hue: hue, at: t, mine: name === "You"});
    if(V.feed.length > 5) V.feed.length = 5;
    renderFeed();
  }
  // the item guide card (press I): one row per thing that drops on the map, each tinted with
  // the exact colour of its light column so the card reads as a legend for the sky beacons
  function renderGuide(){
    guide.classList.toggle("hidden", !V.guide);
    if(!V.guide) return;
    guide.textContent = "";
    guide.appendChild(api.mk("b", null, "Items on the map"));
    guide.appendChild(api.mk("p", null, "Fresh totems drop every "+TOTEM_GAP+"s and fresh health/ammo packs every "+PACK_GAP+"s — each somewhere random on the map. Repetitions allowed."));
    var tb = api.mk("table", "vg-golf-table"), hr = api.mk("tr");
    ["", "Item", "Effect"].forEach(function(h){ hr.appendChild(api.mk("th", null, h)); }); tb.appendChild(hr);
    // rows: the map's base packs first, then the totems; colours come straight from the
    // constants the 3D gems/beams use, so the legend can never drift from what you see
    var rows = [{c: "#f4f4f4", n: "Health kit", d: "Pick up to raise your health by 50."},
                {c: "#e0b325", n: "Ammo box", d: "Pick up to reload any gun that is running empty."}]
      .concat(Object.keys(TOTEMS).map(function(k){ return {c: TOTEMS[k].hue, n: TOTEMS[k].n, d: TOTEM_HELP[k]}; }));
    rows.forEach(function(r){
      var tr = api.mk("tr");
      var sw = api.mk("td"); sw.style.color = r.c; sw.appendChild(document.createTextNode("●"));
      tr.appendChild(sw);
      var nm = api.mk("td", null, r.n); nm.style.color = r.c; tr.appendChild(nm);
      tr.appendChild(api.mk("td", null, r.d));
      tb.appendChild(tr);
    });
    guide.appendChild(tb);
    guide.appendChild(api.btn("Close", "", function(){ V.guide = false; renderGuide(); if(canvas) canvas.focus(); }));
  }
  function renderBoard(){
    board.classList.toggle("hidden", !V.board);
    if(!V.board) return;
    board.textContent = "";
    board.appendChild(api.mk("b", null, V.mode === "practice" ? "Target range" :
      TEAMS[0].n+" "+V.scores[0]+" — "+V.scores[1]+" "+TEAMS[1].n+" · first to "+V.limit));
    var tb = api.mk("table", "vg-golf-table"), hr = api.mk("tr");
    (V.mode === "practice" ? ["", "Drones", "Shots"] : ["Player", "Team", "Kills", "Deaths"]).forEach(function(h){ hr.appendChild(api.mk("th", null, h)); }); tb.appendChild(hr);
    var rows = V.mode === "practice" ? [{name: "You", kills: V.score, deaths: V.shots, uid: "me"}] :
      (V.mode === "bots" ? [{name: "You", uid: myId(), team: V.team, kills: V.kills|0, deaths: V.deaths|0, slot: -1}] : []).concat(V.order.map(function(u){ return V.players[u]; }))
        .sort(function(a, b){ return (a.team - b.team) || (b.kills - a.kills) || (a.deaths - b.deaths) || (a.slot - b.slot); });
    rows.forEach(function(P){
      var tr = api.mk("tr");
      tr.appendChild(api.mk("td", P.uid === myId() ? "tot" : null, P.name+(P.bot ? " (bot)" : "")+(P.gone ? " (left)" : P.away ? " (away)" : "")));
      if(V.mode !== "practice"){
        var td = api.mk("td", null, TEAMS[P.team|0].n); td.style.color = TEAMS[P.team|0].c; tr.appendChild(td);
      }
      tr.appendChild(api.mk("td", null, String(P.kills|0))); tr.appendChild(api.mk("td", null, String(P.deaths|0)));
      tb.appendChild(tr);
    });
    board.appendChild(tb);
    board.appendChild(api.btn("Close", "", function(){ V.board = false; renderBoard(); if(canvas) canvas.focus(); }));
  }
  function resultsTable(rows, title){
    var box = api.mk("div", "vg-golf-score");
    box.appendChild(api.mk("b", null, title));
    var teams = (rows || []).filter(function(r){ return r && r.teams; })[0];
    if(teams){
      var tt = api.mk("p", "vg-strike-final");
      tt.appendChild(api.mk("span", null, TEAMS[0].n+" "+teams.teams[0]));
      tt.appendChild(api.mk("b", null, " — "));
      tt.appendChild(api.mk("span", null, teams.teams[1]+" "+TEAMS[1].n));
      if(teams.teams[0] !== teams.teams[1]){
        var win = teams.teams[0] > teams.teams[1] ? 0 : 1;
        tt.appendChild(api.mk("b", "vg-strike-win", "  "+TEAMS[win].n+" wins"));
        tt.lastChild.style.color = TEAMS[win].c;
      } else tt.appendChild(api.mk("b", null, "  Draw"));
      box.appendChild(tt);
    }
    var tb = api.mk("table", "vg-golf-table"), hr = api.mk("tr");
    ["#", "Player", "Team", "Kills", "Deaths"].forEach(function(h){ hr.appendChild(api.mk("th", null, h)); }); tb.appendChild(hr);
    (rows || []).forEach(function(r){
      if(!r || !r.place) return;
      var tr = api.mk("tr");
      tr.appendChild(api.mk("td", null, String(r.place|0)));
      tr.appendChild(api.mk("td", r.user && (r.user.userId || r.user.id) === myId() ? "tot" : null,
        nameOf(r.user)+(r.bot ? " (bot)" : "")+(r.left ? " (left)" : "")));
      var td = api.mk("td", null, TEAMS[clamp(r.team|0, 0, 1)].n); td.style.color = TEAMS[clamp(r.team|0, 0, 1)].c; tr.appendChild(td);
      tr.appendChild(api.mk("td", null, String(r.kills|0))); tr.appendChild(api.mk("td", null, String(r.deaths|0)));
      tb.appendChild(tr);
    });
    box.appendChild(tb); return box;
  }
  function showResults(rows){
    cardBox.textContent = "";
    cardBox.appendChild(resultsTable(rows, "Match over · "+MAP.name));
    var row = api.mk("div", "vg-row");
    if(V.mode === "bots"){
      row.appendChild(api.btn("Play again", "primary", function(){ startBotMatch(); }));
      row.appendChild(api.btn("Menu", "", function(){ resetMatch(); showStage(false); renderMenu(); }));
    } else row.appendChild(api.btn(isHost() ? "Set up the next match" : "Back to the lobby", "primary", function(){ resetMatch(); showStage(false); renderMenu(); }));
    cardBox.appendChild(row); cardBox.classList.remove("hidden");
    var teams = (rows || []).filter(function(r){ return r && r.teams; })[0];
    if(teams){
      var mine = teams.teams[V.team]|0, theirs = teams.teams[1 - V.team]|0;
      say(mine > theirs ? "Your side wins "+mine+" to "+theirs+"." : mine < theirs ? "Their side wins "+theirs+" to "+mine+". " : "A draw: "+mine+" each.");
    }
  }

  /* ---------- renderers ---------- */
  function ensureRenderer(){
    if(canvas) return;
    wrap.textContent = "";
    pauseBox.textContent = "";
    pauseBox.appendChild(api.mk("b", null, "Paused"));
    pauseBox.appendChild(api.mk("span", null, "Click here to play (the mouse aims; Esc gives it back), or Tab here to play with the keyboard."));
    [hud, board, guide, cardBox, pauseBox, badge, load].forEach(function(n){ wrap.appendChild(n); });
    canvas = api.mk("canvas", "vg-golf-canvas vg-fps-canvas"); canvas.tabIndex = 0;
    canvas.setAttribute("aria-label", "Strikezone, first person team shooter. Click to aim with the mouse, Escape releases it. W A S D move, Q and E lean to peek, F or click fires, R reloads, 1 2 3 4 pick a weapon, hold Tab for the weapon wheel, Space jumps. Hold right-click or the backquote key to scope in.");
    wrap.insertBefore(canvas, wrap.firstChild);
    if(IN) IN.destroy();
    IN = HQV.input ? HQV.input.create(wrap, {look: true, wheel: true, sensitivity: V.sens, buttons: BUTTONS}) : null;
    canvas.addEventListener("pointerdown", function(){ canvas.focus(); });
    pauseBox.addEventListener("click", function(){ if(canvas){ canvas.focus(); if(IN) IN.lock(); } });
    resize();
    if(!V.map && hasWebGL2()){
      R2 = make2d(null);
      load.classList.remove("hidden"); loadFill.style.width = "10%";
      make3d(canvas).then(function(r){
        load.classList.add("hidden");
        if(!V.alive){ r.dispose(); return; }
        R3 = r; R2 = null; R3.buildMap(); resize();
      }, function(e){
        load.classList.add("hidden");
        if(!V.alive) return;
        if(window.console) console.warn("strike 3d", e);
        var why = e && e.message ? ": " + e.message : "";
        V.note = "3D unavailable" + why + " — showing the map view."; api.toast(V.note); fallback2d();
      });
    } else R2 = make2d(canvas);
    if(!raf) raf = requestAnimationFrame(frame);
  }
  function fallback2d(){
    if(R3){ R3.dispose(); R3 = null; }
    if(canvas){ canvas.remove(); canvas = null; }
    var m = V.map; V.map = true; ensureRenderer(); V.map = m;
  }
  function resize(){
    if(!canvas) return;
    var w = Math.max(280, wrap.clientWidth || host.clientWidth || 640), h = Math.round(Math.min(w*0.6, (window.innerHeight || 800)*0.72));
    canvas.style.height = h+"px";
    if(R3) R3.size(w, h);
    else if(R2 && R2.canvas){ var dpr = Math.min(window.devicePixelRatio || 1, 2); canvas.width = Math.round(w*dpr); canvas.height = Math.round(h*dpr); }
  }
  var hudAt = 0;
  function frame(ts){
    if(!V.alive) return;
    raf = requestAnimationFrame(frame);
    var t = ts/1000, dt = lastT ? clamp(t - lastT, 0, 0.1) : 0; lastT = t;
    try {
      if(t - tokAt > 2){ TOK = tokens(); tokAt = t; }
      var tn = now();
      update(dt, tn);
      if(R3) R3.render(dt, tn); else if(R2) R2.render(tn);
      if(t - hudAt > 0.05){ hudAt = t; hudUpdate(tn); }
    } catch(e){ if(window.console) console.error("strike frame", e); }
  }
  // the weapon wheel: hold Tab, steer with the mouse, release to pick (the selection
  // takes effect as the key comes up, GTA-style; Esc closes it without picking)
  var wheelBox = (function(){
    var b = api.mk("div", "vg-fps-wheel hidden"); b.setAttribute("role", "dialog"); b.setAttribute("aria-label", "Pick a weapon");
    var ring = api.mk("div", "vg-fps-wheel-ring"), lab = api.mk("div", "vg-fps-wheel-lab");
    b.appendChild(ring); b.appendChild(lab); hud.appendChild(b);
    var segs = FS.WEAPONS.map(function(wp, i){
      // the wedge positions are pure CSS (:nth-child(1..4), each square rotated so the
      // ring's border-radius clips it to a quarter sector; seg 0 sits centered on up)
      var s = api.mk("div", "vg-fps-wheel-seg");
      var c = api.mk("div", "vg-fps-wheel-cnt"); s.appendChild(c);
      var ic = api.mk("span", "vg-fps-wheel-ic", WHEEL_ICONS[i]);
      var nm = api.mk("b", null, wp.name), am = api.mk("i");
      ic.setAttribute("aria-hidden", "true");
      c.appendChild(ic); c.appendChild(nm); c.appendChild(am); ring.appendChild(s);
      return {el: s, ammo: am};
    });
    return {box: b, lab: lab, segs: segs};
  })();
  function renderWheel(t){
    if(!V.wheelOpen){ wheelBox.box.classList.add("hidden"); return; }
    wheelBox.box.classList.remove("hidden");
    var n = weaponCount();
    wheelBox.segs.forEach(function(sg, i){
      sg.el.classList.toggle("sel", i === V.wheelSel);
      sg.el.classList.toggle("off", i >= n);
      sg.el.classList.toggle("cur", i === V.w);
      sg.ammo.textContent = magOf(i)[i === LAUNCH_W ? 0 : i] + " / " + resOf(i)[i === LAUNCH_W ? 0 : i];
    });
    var txt = V.wheelSel >= 0 && V.wheelSel < n ? FS.WEAPONS[V.wheelSel].name : "Cancel";
    if(wheelBox.lab.textContent !== txt) wheelBox.lab.textContent = txt;
  }

  // 2D map view: north-up, centred on you, your view as a wedge.
  function make2d(cv){
    var R = {canvas: cv};
    R.render = function(t){
      if(!cv) return;
      var g = cv.getContext("2d"), Wd = cv.width, Ht = cv.height, T = TOK;
      g.setTransform(1, 0, 0, 1, 0, 0); g.fillStyle = T.bg2; g.fillRect(0, 0, Wd, Ht);
      if(!MAP){ g.fillStyle = T.muted; g.font = "14px sans-serif"; g.fillText("Loading…", 16, 24); return; }
      var sc = Math.min(Wd, Ht)/34, M = V.me;
      g.save(); g.translate(Wd/2, Ht/2); g.scale(sc, sc); g.translate(-M.x, -M.z);
      MAP.boxes.forEach(function(b){
        if(b[4] <= 0) { g.fillStyle = T.panel2; g.fillRect(b[0], b[2], b[3] - b[0], b[5] - b[2]); return; }
        g.globalAlpha = clamp(0.35 + b[4]/5, 0.35, 1); g.fillStyle = b[6] === "wall" ? T.ink : T.muted;
        g.fillRect(b[0], b[2], b[3] - b[0], b[5] - b[2]); g.globalAlpha = 1;
      });
      MAP.pickups.forEach(function(pk, i){ if(!V.items[i] || !pk.kind) return; g.fillStyle = pk.kind === "health" ? T.need : T.gold; g.beginPath(); g.arc(pk.at[0], pk.at[2], 0.4, 0, Math.PI*2); g.fill(); });
      if(V.mode === "practice") V.drones.forEach(function(D){ if(D.downUntil > t) return; var c = dronePos(D, (V.phase === "round" ? t - V.startAt : 0)); g.fillStyle = T.need; g.fillRect(c[0] - 0.45, c[2] - 0.45, 0.9, 0.9); });
      V.tracers.forEach(function(tr){ var age = t - tr.at; if(age > 0.12) return; g.strokeStyle = tr.mine ? T.gold : T.need; g.lineWidth = 0.08; g.beginPath(); g.moveTo(tr.a[0], tr.a[2]); g.lineTo(tr.b[0], tr.b[2]); g.stroke(); });
      others().forEach(function(P){
        if(P.gone) return;
        if(!P.dead && botShielded(P, t)){  // the drop shield reads on the map too: a light-blue halo
          g.globalAlpha = 0.5; g.strokeStyle = SHIELD_COLOR; g.lineWidth = 0.18;
          g.beginPath(); g.arc(P.x, P.z, 0.8, 0, Math.PI*2); g.stroke(); g.globalAlpha = 1;
        }
        g.globalAlpha = P.dead ? 0.3 : 1; g.fillStyle = TEAMS[P.team|0].c;
        g.beginPath(); g.arc(P.x, P.z, 0.45, 0, Math.PI*2); g.fill();
        g.strokeStyle = T.ink; g.lineWidth = 0.08; g.beginPath(); g.moveTo(P.x, P.z); g.lineTo(P.x + Math.sin(P.yaw)*0.8, P.z - Math.cos(P.yaw)*0.8); g.stroke();
        g.globalAlpha = 1; g.fillStyle = T.ink; g.font = "0.6px sans-serif"; g.textAlign = "center"; g.fillText(P.name, P.x, P.z - 0.7);
      });
      // me + the view wedge
      g.fillStyle = TEAMS[V.team].c; g.globalAlpha = 0.18; g.beginPath(); g.moveTo(M.x, M.z);
      g.arc(M.x, M.z, 12, V.yaw - Math.PI/2 - (V.ads ? 0.22 : 0.5), V.yaw - Math.PI/2 + (V.ads ? 0.22 : 0.5)); g.closePath(); g.fill(); g.globalAlpha = 1;
      g.beginPath(); g.arc(M.x, M.z, 0.45, 0, Math.PI*2); g.fill();
      // kill cam on the 2D map: their wedge and name, from where they shot you
      var KC2 = V.dead && V.killCam && V.killCam.ref && !V.killCam.ref.gone ? V.killCam : null;
      if(KC2){
        var ky = KC2.ref.yaw || 0;
        g.fillStyle = TEAMS[KC2.team].c; g.globalAlpha = 0.3; g.beginPath(); g.moveTo(KC2.ref.x, KC2.ref.z);
        g.arc(KC2.ref.x, KC2.ref.z, 9, ky - Math.PI/2 - 0.5, ky - Math.PI/2 + 0.5); g.closePath(); g.fill(); g.globalAlpha = 1;
        g.beginPath(); g.arc(KC2.ref.x, KC2.ref.z, 0.45, 0, Math.PI*2); g.fill();
        g.fillStyle = T.ink; g.font = "0.6px sans-serif"; g.textAlign = "center"; g.fillText("killer", KC2.ref.x, KC2.ref.z - 0.7);
      }
      g.restore();
    };
    return R;
  }

  function make3d(cv){
    return E.lib().then(function(lib){
      var names = [["fps", "platform-large-grass"], ["fps", "platform"], ["fps", "wall-high"], ["fps", "wall-low"], ["fps", "blaster"],
                   ["fps", "blaster-repeater"], ["fps", "enemy-flying"], ["fps", "grass"]]
        .concat(CHARS.map(function(c){ return ["golf", c.f]; }));
      var done = 0, GL = {};
      function tex(n){ return new Promise(function(res){ lib.tex.load("/games/fps/"+n+".png", res, null, function(){ res(null); }); }); }
      return Promise.all(names.map(function(n){ return E.loadGlb(lib, n[0], n[1]).then(function(g){ GL[n[1]] = g; done++; loadFill.style.width = Math.round(10 + 85*done/names.length)+"%"; }); }))
        .then(function(){ return Promise.all([tex("burst"), tex("hit")]); })
        .then(function(tx){ return build3d(lib, lib.THREE, cv, GL, tx[0], tx[1]); });
    });
  }
  function build3d(lib, THREE, cv, GL, burstTex, hitTex){
    var renderer = new THREE.WebGLRenderer({canvas: cv, antialias: !V.low});
    renderer.setPixelRatio(V.low ? 1 : Math.min(window.devicePixelRatio || 1, 1.5));
    renderer.outputColorSpace = THREE.SRGBColorSpace;
    renderer.shadowMap.enabled = !V.low; renderer.shadowMap.type = THREE.PCFShadowMap;
    renderer.autoClear = false;
    // Graphics quality: Low keeps the cheap path; Balanced/Ultra add tone mapping + PBR + soft shadows
    // (carried over from the Blaster Arena graphics work, tuned for the dock's dusk).
    var HQ = (function(){ try { return (localStorage.getItem("hq_strike_gfx") || "balanced"); } catch(e){ return "balanced"; } })();
    var ULTRA = !V.low && HQ !== "low", PBR = ULTRA && !!THREE.MeshStandardMaterial;
    if(ULTRA && THREE.ACESFilmicToneMapping != null){ renderer.toneMapping = THREE.ACESFilmicToneMapping; renderer.toneMappingExposure = 1.05; }
    var FOV = 75, FOV_ADS = 40;                                  // FOV zoom while scoped (visual only; spread unchanged; per-weapon zooms in ADS above)
    var scene = new THREE.Scene(), camera = new THREE.PerspectiveCamera(FOV, 1.6, 0.05, 300);
    var hemi = new THREE.HemisphereLight(0xdfeffc, 0x4a5a44, ULTRA ? 1.1 : 2.1); scene.add(hemi);
    var sun = new THREE.DirectionalLight(0xfff2d8, ULTRA ? 2.4 : 1.8); sun.position.set(14, 30, 10);
    sun.castShadow = !V.low; sun.shadow.mapSize.set(ULTRA ? 2048 : 1024, ULTRA ? 2048 : 1024);
    sun.shadow.bias = -0.0005; sun.shadow.normalBias = 0.03;
    var sc = sun.shadow.camera; sc.left = -26; sc.right = 26; sc.top = 26; sc.bottom = -26; sc.near = 1; sc.far = 80;
    scene.add(sun); scene.add(sun.target);
    // cool fill from the opposite side so shadowed faces keep depth instead of going flat
    var fill = null; if(ULTRA){ fill = new THREE.DirectionalLight(0x7a9fd8, 0.5); fill.position.set(-12, 18, -14); scene.add(fill); }
    // the first-person weapon: its own scene and camera, drawn over the world
    var vScene = new THREE.Scene(), vCam = new THREE.PerspectiveCamera(50, 1.6, 0.01, 10);
    vScene.add(new THREE.HemisphereLight(0xffffff, 0x667755, 2.3));
    var vSun = new THREE.DirectionalLight(0xffffff, 1.4); vSun.position.set(2, 3, 2); vScene.add(vSun);
    var GUN_S = 0.2, guns, gunHold = new THREE.Group(); vScene.add(gunHold);
    var mySkin = window.HQ_MYCOS && hex(window.HQ_MYCOS.blaster, null);       // HQ 2.1: the blaster skin you wear
    // Simple, readable silhouettes of our own making: what you hold should tell you what you hold.
    function gmat(c, r){ return new THREE.MeshLambertMaterial({color: c}); }
    function box(w, h, d, x, y, z, c){
      var m = new THREE.Mesh(new THREE.BoxGeometry(w, h, d), gmat(c));
      m.position.set(x || 0, y || 0, z || 0); return m;
    }
    function tubeL(r, len, x, y, z, c){ // a barrel: a cylinder lying along -z
      var m = new THREE.Mesh(new THREE.CylinderGeometry(r, r, len, 12), gmat(c));
      m.rotation.x = Math.PI/2; m.position.set(x || 0, y || 0, z || 0); return m;
    }
    function gunMarksman(){ // long thin barrel, a scope tube on top, a stock — the sniper outline
      var g = new THREE.Group();
      g.add(box(0.10, 0.12, 0.55, 0, 0, -0.05, 0x3a3f45));        // receiver
      g.add(tubeL(0.028, 0.62, 0, 0.030, -0.62, 0x2c3036));       // the long barrel
      g.add(tubeL(0.045, 0.22, 0, 0.105, -0.18, 0x22262b));       // scope
      g.add(box(0.09, 0.16, 0.26, 0, -0.06, 0.34, 0x4a3b2a));     // wooden stock
      g.add(box(0.05, 0.12, 0.10, 0, -0.11, 0.05, 0x3a3f45));     // grip
      return g;
    }
    function gunRevolver(){ // your current blaster model — chunky pistol frame (it already reads as a hand cannon)
      return model("blaster");
    }
    function gunLauncher(){ // a fat tube with a back vent and a front ring — the rocket outline
      var g = new THREE.Group();
      g.add(tubeL(0.13, 0.95, 0, 0, -0.1, 0x3e4a3a));             // the launch tube
      g.add(tubeL(0.155, 0.16, 0, 0, 0.40, 0x2e382c));            // rear vent bell
      g.add(tubeL(0.145, 0.10, 0, 0, -0.58, 0x50604a));           // front ring
      g.add(box(0.06, 0.14, 0.12, 0, -0.14, 0.02, 0x2c3328));     // grip underneath
      g.add(box(0.04, 0.08, 0.30, 0, 0.15, -0.05, 0x2e382c));     // sight rail on top
      return g;
    }
    guns = [model("blaster-repeater"), gunMarksman(), gunRevolver(), gunLauncher()];
    // measure each gun BEFORE rotation, in its own local space (setFromObject bakes rotation in
    // and the children's matrices haven't re-synced after rotation.y flips — that misreads the span).
    // Normalise our thin rifle/launcher to the chunky SMG's on-screen length, with a width
    // compensation so they read the same visual weight.
    var refB = new THREE.Box3().setFromObject(guns[0]), refL = (refB.max.z - refB.min.z) || 1;
    var BOOST = 2.6; // thin silhouette -> reads much smaller than the chunky repeater at equal length: boost
    guns.forEach(function(g, i){
      g.traverse(function(n){ if(n.isMesh){ n.castShadow = false; n.receiveShadow = false;
        if(i === 0 && mySkin != null){ n.material = n.material.clone(); n.material.color.setHex(mySkin); } } });
      var k = 1;
      if(i === 1 || i === 3){
        var b = new THREE.Box3().setFromObject(g), L = (b.max.z - b.min.z) || refL;
        k = (refL/L)*(i === 1 ? 5.0 : BOOST);   // rifle: a bit more than the launcher
      }
      g.scale.setScalar(GUN_S*k); g.rotation.y = Math.PI; g.visible = i === 0; gunHold.add(g);
    });
    var flash = null, muzzle = null, seatFlash = null;
    if(burstTex){
      burstTex.colorSpace = THREE.SRGBColorSpace; burstTex.repeat.set(0.5, 1);
      flash = new THREE.Sprite(new THREE.SpriteMaterial({map: burstTex, transparent: true, depthTest: false, blending: THREE.AdditiveBlending}));
      // at the muzzle: the front face of the weapon's bounds (it looks down -z here); re-seated
      // per weapon so every gun flashes at its own muzzle
      gunHold.add(flash);
      seatFlash = function(g){
        if(!flash || !g) return;
        gunHold.updateMatrixWorld(true);
        var gb = new THREE.Box3().setFromObject(g);
        flash.position.set((gb.min.x + gb.max.x)/2, gb.min.y + (gb.max.y - gb.min.y)*0.62, gb.min.z - 0.02);
        if(muzzle) muzzle.position.copy(flash.position);
      };
      if(ULTRA){ muzzle = new THREE.PointLight(0xffc36a, 0, 9); gunHold.add(muzzle); }
      seatFlash(guns[0]);
    }
    var mapGroup = null, dyn = new THREE.Group(); scene.add(dyn);
    // your own canopy, seen from inside: a sphere around your body — from eye height you
    // look down THROUGH it (a soft blue haze over your feet), which is the "I'm shielded"
    // cue the rim glow alone can't give. Hidden the moment the shield drops.
    var myShield = new THREE.Mesh(new THREE.SphereGeometry(1.05, 20, 14),
      new THREE.MeshBasicMaterial({color: hex(SHIELD_COLOR, 0x7fd4ff), transparent: true, opacity: 0.16, depthWrite: false, side: THREE.DoubleSide}));
    myShield.position.y = 0.95; myShield.visible = false; scene.add(myShield);
    var lost = function(e){ e.preventDefault(); if(V.alive) setTimeout(fallback2d, 0); };
    cv.addEventListener("webglcontextlost", lost);
    function model(name){
      var g = GL[name]; if(!g) return new THREE.Group();
      var o = lib.clone(g.scene);
      o.traverse(function(m){ if(m.isMesh){ m.castShadow = !V.low; m.receiveShadow = !V.low; } });
      return o;
    }
    function bbox(name){ var g = GL[name]; return g ? new THREE.Box3().setFromObject(g.scene) : new THREE.Box3(new THREE.Vector3(-0.5, 0, -0.5), new THREE.Vector3(0.5, 1, 0.5)); }
    var BB = {};
    // A solid box drawn as kit pieces, tiled so they keep their proportions roughly.
    var KIND = {floor: ["platform-large-grass", 8.8], block: ["platform", 2.2], step: ["platform", 1.0], crate: ["platform", 1.2],
                low: ["wall-low", 2.2], wall: ["wall-high", 1.5], catwalk: ["wall-low", 2.2]};
    // One material finish everywhere so the dock reads as one place: PBR when we have it, Lambert
    // when we don't, with an optional emissive pulse over the theme's accent. Colors come from the
    // theme and the box kind; only the surface response changes.
    var pulseMats = [];
    function mat(opts){
      opts = opts || {};
      var m;
      if(PBR) m = new THREE.MeshStandardMaterial({color: opts.color != null ? opts.color : 0xffffff, roughness: opts.rough != null ? opts.rough : 0.82, metalness: opts.metal != null ? opts.metal : 0.04});
      else m = new THREE.MeshLambertMaterial({color: opts.color != null ? opts.color : 0xffffff});
      if(opts.em){ m.emissive = new THREE.Color(opts.em); m.emissiveIntensity = 0; pulseMats.push({m: m, max: opts.emMax != null ? opts.emMax : 0.9}); }
      return m;
    }
    function kindColor(k, i){
      var th = MAP.theme || {};
      if(k === "container"){ var cs = th.container || ["#a8534a", "#4a7d8c"]; return hex(cs[i % cs.length], 0x4a7d8c); }
      if(k === "catwalk") return hex(th.catwalk || "#5d6873", 0x5d6873);
      if(k === "crate") return hex(th.crate || "#8c734a", 0x8c734a);
      return null;    // the rest keeps the kit's own look
    }
    function boxModel(b, idx){
      var k = KIND[b[6]] || KIND.block, name = k[0], bb = BB[name] || (BB[name] = bbox(name));
      var sx = b[3] - b[0], sy = b[4] - b[1], sz = b[5] - b[2], g = new THREE.Group();
      var along = name.indexOf("wall") === 0 && sz > sx;           // walls: the long side runs along x in the model
      var L = along ? sz : sx, D = along ? sx : sz;
      var nL = Math.max(1, Math.round(L/k[1])), nD = name.indexOf("wall") === 0 ? 1 : Math.max(1, Math.round(D/k[1]));
      var mw = bb.max.x - bb.min.x, mh = bb.max.y - bb.min.y, md = bb.max.z - bb.min.z;
      // containers, catwalks and crates get one flat colour so the lanes read at a glance
      var kc = kindColor(b[6], idx);
      for(var i = 0; i < nL; i++) for(var j = 0; j < nD; j++){
        var o = model(name), tl = L/nL, td = D/nD, ov = name.indexOf("wall") === 0 ? 1.12 : 1.04;
        if(kc != null) o.traverse(function(n){ if(n.isMesh){ n.material = n.material.clone(); n.material.color.setHex(kc); } });
        o.scale.set(tl*ov/mw, sy/mh, td*(name.indexOf("wall") === 0 ? 1 : ov)/md);
        var cl = -L/2 + tl*(i + 0.5), cd = -D/2 + td*(j + 0.5);
        o.position.set(along ? cd : cl, -bb.min.y*sy/mh, along ? cl : cd);
        if(along) o.rotation.y = Math.PI/2;
        o.position.x -= (bb.min.x + bb.max.x)/2*(along ? 0 : tl/mw); o.position.z -= (bb.min.z + bb.max.z)/2*(along ? 0 : td/md);
        g.add(o);
      }
      g.position.set((b[0] + b[3])/2, b[1], (b[2] + b[5])/2);
      return g;
    }
    function hash(a, b){ var h = (a*73856093) ^ (b*19349663); h = (h ^ (h >>> 13))*1274126177; return ((h ^ (h >>> 16)) >>> 0)/4294967296; }
    var pickMeshes = [], droneMeshes = [];
    // a beacon so every live pickup is findable from anywhere: a tall, thin, additive light
    // column soaring above the fog (fog:false so it stays legible across the whole map),
    // brightest at the base and fading to nothing at the top
    function beacon(c){
      var b = new THREE.Mesh(new THREE.CylinderGeometry(0.16, 0.30, 26, 10, 1, true),
        new THREE.MeshBasicMaterial({color: c, transparent: true, opacity: 0.5, fog: false,
                                     side: THREE.DoubleSide, depthWrite: false, blending: THREE.AdditiveBlending}));
      b.position.y = 13; b.renderOrder = 4; return b;
    }
    function buildMap(){
      if(mapGroup){ scene.remove(mapGroup); }
      mapGroup = new THREE.Group(); scene.add(mapGroup);
      var th = MAP.theme || {};
      scene.background = new THREE.Color(hex(th.sky, 0x33415c));
      scene.fog = new THREE.Fog(hex(th.fog, 0x3d4d63), V.low ? 25 : (ULTRA ? 42 : 36), V.low ? 70 : (ULTRA ? 170 : 120));
      var under = new THREE.Mesh(new THREE.PlaneGeometry(420, 420).rotateX(-Math.PI/2), mat({color: hex(th.ground, 0x5b6670), rough: 0.95, metal: 0}));
      under.position.y = -1.2; mapGroup.add(under);
      // a simple sky backdrop: a big gradient shell behind the fog, tinted by the theme (no skybox texture → IP-safe)
      if(ULTRA){
        var cvs = document.createElement("canvas"); cvs.width = 4; cvs.height = 128; var gg = cvs.getContext("2d");
        var grd = gg.createLinearGradient(0, 0, 0, 128);
        grd.addColorStop(0, th.skyTop || th.sky || "#22304a"); grd.addColorStop(0.55, th.sky || "#33415c"); grd.addColorStop(1, th.skyBottom || th.ground || "#5b6670");
        gg.fillStyle = grd; gg.fillRect(0, 0, 4, 128);
        var skyt = new THREE.CanvasTexture(cvs); skyt.colorSpace = THREE.SRGBColorSpace;
        var skyd = new THREE.Mesh(new THREE.SphereGeometry(240, 24, 16), new THREE.MeshBasicMaterial({map: skyt, side: THREE.BackSide, fog: false, depthWrite: false}));
        skyd.renderOrder = -1; mapGroup.add(skyd);
      }
      MAP.boxes.forEach(function(b, i){ mapGroup.add(boxModel(b, i)); });
      // one glowing strip per container top edge (the theme's accent): harbour cranes and deck lights
      var acc = hex(th.accent || "#ffb86b", 0xffb86b);
      MAP.boxes.forEach(function(b, idx){
        if(b[6] !== "container" && b[6] !== "catwalk") return;
        var sx = b[3] - b[0], sz = b[5] - b[2], horiz = sx >= sz;
        var strip = new THREE.Mesh(new THREE.BoxGeometry(horiz ? sx : 0.08, 0.1, horiz ? 0.08 : sz), mat({color: 0x0a0e14, em: acc, emMax: 1.05}));
        strip.position.set((b[0] + b[3])/2, b[4] + 0.06, (b[2] + b[5])/2); mapGroup.add(strip);
      });
      if(!V.low){
        // grass tufts as dock weeds between the boxes, placed by a fixed hash so everyone sees the same dock
        for(var i = 0; i < 60; i++){
          var x = -20 + hash(i, 7)*40, z = -14 + hash(i, 13)*28;
          if(FS.overlaps(MAP, x, 0.05, z, 0.2) >= 0) continue;
          var gr = model("grass"); gr.position.set(x, 0, z); gr.rotation.y = hash(i, 3)*6.28; gr.scale.setScalar(1.2); mapGroup.add(gr);
        }
      }
      pickMeshes.forEach(function(p){ scene.remove(p); }); pickMeshes = [];
      MAP.pickups.forEach(function(pk, i){
        if(!pk.kind && i >= BASE_PACKS) return;   // an empty drop slot: no crate until a drop lands
        var gp = new THREE.Group(), base = new THREE.Mesh(new THREE.BoxGeometry(0.5, 0.5, 0.5),
          mat({color: pk.kind === "health" ? 0xf4f4f4 : 0x3c7a3a, rough: 0.4, metal: 0.25}));
        gp.add(base);
        if(pk.kind === "health"){
          var red = mat({color: 0xe0574a, em: PBR ? 0xe0574a : null, emMax: 0.7, rough: 0.5});
          gp.add(new THREE.Mesh(new THREE.BoxGeometry(0.52, 0.14, 0.36), red)); gp.add(new THREE.Mesh(new THREE.BoxGeometry(0.14, 0.36, 0.52), red).rotateY(0));
          gp.add(new THREE.Mesh(new THREE.BoxGeometry(0.36, 0.14, 0.52), red)); gp.add(new THREE.Mesh(new THREE.BoxGeometry(0.14, 0.52, 0.36), red));
        } else {
          var gold = mat({color: 0xe0b325, em: PBR ? 0xe0b325 : null, emMax: 0.7, rough: 0.35, metal: 0.6});
          gp.add(new THREE.Mesh(new THREE.BoxGeometry(0.54, 0.12, 0.54), gold));
        }
        gp.add(beacon(pk.kind === "health" ? 0xf4f4f4 : 0xe0b325));
        gp.position.set(pk.at[0], pk.at[1] + 0.55, pk.at[2]); scene.add(gp); pickMeshes.push(gp);
      });
      droneMeshes.forEach(function(d){ scene.remove(d); }); droneMeshes = [];
      MAP.targets.forEach(function(){ var d = model("enemy-flying"); d.scale.setScalar(1.5); d.visible = false; scene.add(d); droneMeshes.push(d); });
    }
    // the name tag: the dot keeps the side's own colour, the NAME carries friend/foe —
    // enemies read red (shoot on sight), team-mates read your side's colour (hold fire)
    function tag(text, dot, nameCol, bot){
      var c = document.createElement("canvas"), g = c.getContext("2d"); c.width = 256; c.height = 64;
      g.font = "700 30px sans-serif"; g.textAlign = "center"; g.textBaseline = "middle";
      var w = Math.min(250, g.measureText(text).width + 40);
      g.fillStyle = "rgba(20,24,32,0.72)"; g.beginPath(); if(g.roundRect) g.roundRect(128 - w/2, 8, w, 48, 18); else g.rect(128 - w/2, 8, w, 48); g.fill();
      g.fillStyle = dot; g.beginPath(); g.arc(128 - w/2 + 18, 32, 7, 0, Math.PI*2); g.fill();
      g.fillStyle = nameCol; g.fillText(text + (bot ? " ▦" : ""), 136, 33);
      var tx = new THREE.CanvasTexture(c); tx.colorSpace = THREE.SRGBColorSpace;
      var sp = new THREE.Sprite(new THREE.SpriteMaterial({map: tx, depthTest: true, transparent: true})); sp.scale.set(1.6, 0.4, 1); sp.renderOrder = 10;
      return sp;
    }
    // a floating damage number: ONE entry per hit — sprite + state on the same object.
    // There is no separate descriptor list to keep in step: fire() drops
    //   {x,y,z, at, o:null} and this render block owns .o (the sprite) from creation
    //   to disposal. An entry with o === null is always "needs a sprite this frame".
    function dmgSprite(n, head){
      var c = document.createElement("canvas"), g = c.getContext("2d"); c.width = 128; c.height = 64;
      g.font = "900 " + (head ? 44 : 32) + "px sans-serif"; g.textAlign = "center"; g.textBaseline = "middle";
      g.strokeStyle = "rgba(0,0,0,0.65)"; g.lineWidth = 5; g.strokeText(String(n), 64, 32);
      g.fillStyle = head ? "#ff453a" : "#ffe9c9"; g.fillText(String(n), 64, 32);
      var tx = new THREE.CanvasTexture(c); tx.colorSpace = THREE.SRGBColorSpace;
      var sp = new THREE.Sprite(new THREE.SpriteMaterial({map: tx, depthTest: false, transparent: true})); sp.renderOrder = 20;
      sp.scale.set(head ? 1.6 : 1.1, head ? 0.8 : 0.55, 1);
      return sp;
    }
    var CH_H = null;
    // the body meshes carry no usable texture in three (their GLTF baseColorTexture rides
    // on KHR_texture_transform, which the vendored loader ignores), so they shade off
    // material.color alone. Painting the kit is therefore setting a flat team colour on a
    // per-clone copy of the body material — clothes read Amber/Cyan outright, faces and
    // hair keep their own shading, nothing has to be blitted or re-uploaded.
    var KITS = [{}, {}];
    function kitMat(ch, team){
      team = team ? 1 : 0; ch = ch|0;
      if(KITS[team][ch]) return KITS[team][ch];
      var src = null;
      GL[CHARS[ch].f] && GL[CHARS[ch].f].scene.traverse(function(n){ if(!src && n.isMesh && n.name === "body-mesh" && n.material) src = n.material; });
      if(!src) return null;
      src = src.clone();
      src.color = src.color.clone().lerp(new THREE.Color(TEAMS[team].rgb), 0.85);
      KITS[team][ch] = src; return src;
    }
    function addPlayer(P){
      var g = GL[CHARS[P.ch|0].f]; if(!g) return;
      if(CH_H === null){ var hb = new THREE.Box3().setFromObject(g.scene); CH_H = Math.max(0.3, hb.max.y - hb.min.y); }
      var o = lib.clone(g.scene), holder = new THREE.Group(); holder.add(o);
      o.scale.setScalar(1.75/CH_H);
      // the team kit swaps the whole-body tint in on the body mesh only — the head
      // (face, hair) keeps its own colours
      var teamKit = kitMat(P.ch|0, P.team|0);
      o.traverse(function(n){ if(n.isMesh){
        n.castShadow = !V.low; n.frustumCulled = false;
        if(teamKit && n.name === "body-mesh") n.material = teamKit;
      } });
      var mixer = new THREE.AnimationMixer(o), acts = {};
      ["idle", "walk", "die"].forEach(function(n){ var cl = THREE.AnimationClip.findByName(g.animations, n); if(cl) acts[n] = mixer.clipAction(cl); });
      if(acts.die){ acts.die.setLoop(THREE.LoopOnce, 1); acts.die.clampWhenFinished = true; }
      // the weapon is held out in front at the right hand (barrel along the model's +z, its facing)
      var gun = [model("blaster-repeater"), gunMarksman(), gunRevolver(), gunLauncher()];
      var skin = P.cos && hex(P.cos.blaster, null);
      gun.forEach(function(gn, i){ gn.scale.setScalar(0.34); gn.position.set(-0.36, 0.74, 0.3); gn.visible = i === 0; holder.add(gn);
        if(skin != null) gn.traverse(function(n){ if(n.isMesh){ n.material = n.material.clone(); n.material.color.setHex(skin); } }); });
      // the team ring at the feet: amber or cyan, always readable
      var ring = new THREE.Mesh(new THREE.RingGeometry(0.42, 0.55, 24).rotateX(-Math.PI/2),
        new THREE.MeshBasicMaterial({color: TEAMS[P.team|0].rgb, transparent: true, opacity: 0.85}));
      ring.position.y = 0.03; holder.add(ring);
      // the drop shield: a light-blue bubble that wraps a glider for their descent —
      // nothing gets in, they still shoot out (hidden unless the shield is up)
      var shield = new THREE.Mesh(new THREE.SphereGeometry(1.05, 20, 14),
        new THREE.MeshBasicMaterial({color: hex(SHIELD_COLOR, 0x7fd4ff), transparent: true, opacity: 0.22, depthWrite: false, side: THREE.DoubleSide}));
      shield.position.y = 0.95; shield.visible = false; holder.add(shield);
      // friend or foe at a glance: the name reads red on enemies, your side's colour on
      // team-mates (the dot stays the side's own colour either way)
      var foe = P.team !== V.team;
      var tg = tag(P.name.slice(0, 18), TEAMS[P.team|0].c, foe ? "#ff5544" : TEAMS[V.team|0].c, P.bot); tg.position.set(0, 2.15, 0); holder.add(tg);
      P.mesh = {g: holder, mixer: mixer, acts: acts, cur: null, gun: gun, tag: tg, ring: ring, shield: shield,
                ch: P.ch, team: P.team, bot: P.bot, foe: foe}; scene.add(holder);
    }
    function setAnim(m, n){
      if(m.cur === n || !m.acts[n]) return;
      var next = m.acts[n], prev = m.acts[m.cur]; next.reset(); next.play();
      if(prev) prev.crossFadeTo(next, calm() ? 0.01 : 0.15, false);
      m.cur = n;
    }
    function removePlayer(P){ if(P && P.mesh){ scene.remove(P.mesh.g); P.mesh.mixer.stopAllAction(); P.mesh = null; } }
    var trMat = [new THREE.LineBasicMaterial({color: 0xffe08a, transparent: true}), new THREE.LineBasicMaterial({color: 0xff9a5a, transparent: true})];
    var tracers = [], puffs = [];
    var hitMat = hitTex ? new THREE.SpriteMaterial({map: hitTex, transparent: true, depthWrite: false}) : null;
    function tracer(a, b, w, mine){
      var geo = new THREE.BufferGeometry().setFromPoints([new THREE.Vector3(a[0], a[1], a[2]), new THREE.Vector3(b[0], b[1], b[2])]);
      var ln = new THREE.Line(geo, trMat[w ? 1 : 0].clone()); dyn.add(ln); tracers.push({o: ln, at: now()});
      if(hitMat && !V.low){ var p = new THREE.Sprite(hitMat.clone()); p.position.set(b[0], b[1], b[2]); p.scale.setScalar(0.35); dyn.add(p); puffs.push({o: p, at: now()}); }
      if(tracers.length > 30){ var x = tracers.shift(); dyn.remove(x.o); x.o.geometry.dispose(); }
    }
    function clearDynamic(){ tracers.concat(puffs).forEach(function(x){ dyn.remove(x.o); }); tracers = []; puffs = [];
      (V.dmgNums || []).forEach(function(D){ if(D.o){ dyn.remove(D.o); D.o.material.map.dispose(); D.o.material.dispose(); D.o = null; } }); V.dmgNums = [];
      V.order.forEach(function(u){ removePlayer(V.players[u]); }); }
    var R = {};
    /* launcher visuals: the flying rocket and its blast, both correlations of the missiles list */
    var rockets = [], booms = [];
    var flameTex = (function(){
      var c = document.createElement("canvas"), g = c.getContext("2d"); c.width = 64; c.height = 64;
      var gr = g.createRadialGradient(32, 32, 2, 32, 32, 30);
      gr.addColorStop(0, "rgba(255,240,200,1)"); gr.addColorStop(0.35, "rgba(255,150,60,0.85)"); gr.addColorStop(1, "rgba(255,80,20,0)");
      g.fillStyle = gr; g.fillRect(0, 0, 64, 64);
      var t = new THREE.CanvasTexture(c); t.colorSpace = THREE.SRGBColorSpace; return t;
    })();
    R.rocket = function(m, miss){  // (m, m) updates/spawns; (null, m) disposes
      if(!miss) return;
      if(!m){ var k = rockets.indexOf(miss.o); if(k >= 0){ dyn.remove(miss.o); rockets.splice(k, 1); } miss.o = null; return; }
      if(!m.o){
        var g = new THREE.Group();
        var body = new THREE.Mesh(new THREE.CylinderGeometry(0.07, 0.07, 0.42, 10), new THREE.MeshLambertMaterial({color: 0x4a5a46}));
        body.rotation.x = Math.PI/2; g.add(body);
        var nose = new THREE.Mesh(new THREE.ConeGeometry(0.07, 0.16, 10), new THREE.MeshLambertMaterial({color: 0xb03a2c}));
        nose.rotation.x = -Math.PI/2; nose.position.z = -0.28; g.add(nose);
        var flame = new THREE.Sprite(new THREE.SpriteMaterial({map: flameTex, transparent: true, depthWrite: false, blending: THREE.AdditiveBlending}));
        flame.position.z = 0.30; flame.scale.setScalar(0.5); g.add(flame);
        g.userData.flame = flame; m.o = g; dyn.add(g); rockets.push(g);
      }
      m.o.position.set(m.x, m.y, m.z);
      m.o.lookAt(m.x - m.vx, m.y - m.vy, m.z - m.vz);  // nose (-z local) points along travel
      var fl = m.o.userData.flame; if(fl) fl.scale.setScalar(0.4 + FS.mb32(((now() * 977)|0) & 0xFFFF)*0.35);
    };
    var boomTex = (function(){
      var c = document.createElement("canvas"), g = c.getContext("2d"); c.width = 128; c.height = 128;
      var gr = g.createRadialGradient(64, 64, 4, 64, 64, 62);
      gr.addColorStop(0, "rgba(255,236,190,1)"); gr.addColorStop(0.3, "rgba(255,160,70,0.95)"); gr.addColorStop(0.62, "rgba(220,80,30,0.55)"); gr.addColorStop(1, "rgba(120,40,20,0)");
      g.fillStyle = gr; g.fillRect(0, 0, 128, 128);
      var t = new THREE.CanvasTexture(c); t.colorSpace = THREE.SRGBColorSpace; return t;
    })();
    R.boom = function(x, y, z, t){
      var sp = new THREE.Sprite(new THREE.SpriteMaterial({map: boomTex, transparent: true, depthWrite: false}));
      sp.position.set(x, y, z); sp.scale.setScalar(1.2); dyn.add(sp);
      booms.push({o: sp, at: t});
      if(booms.length > 6){ var b0 = booms.shift(); dyn.remove(b0.o); }
    };
    function boomsTick(t, quiet){
      for(var i = booms.length - 1; i >= 0; i--){
        var a = t - booms[i].at;
        if(a > 0.45){ dyn.remove(booms[i].o); booms.splice(i, 1); continue; }
        booms[i].o.material.opacity = 1 - a/0.45;
        if(!quiet) booms[i].o.scale.setScalar(1.2 + a*16);
      }
    }
    R.buildMap = buildMap; R.removePlayer = removePlayer; R.tracer = tracer; R.clearDynamic = clearDynamic;
    // a totem slot is one small rotating gem: gold mole, violet paralyser, pale-cyan decoy,
    // grey cloak. Kept together with pickups under pickMeshes ([] never shrinks this match,
    // totems just claim the next index) so the bob/spin/mipmap loop covers them too
    R.addTotem = function(idx, pk){
      if(!TOTEMS[pk.kind]) return;
      var old = pickMeshes[idx]; if(old) scene.remove(old);
      var gp = new THREE.Group(), c = TOTEMS[pk.kind].c;
      var gem = new THREE.Mesh(new THREE.OctahedronGeometry(0.32),
        mat({color: c, em: PBR ? c : null, emMax: 0.8, rough: 0.25, metal: 0.4}));
      gem.position.y = 0.28; gp.add(gem);
      var ring = new THREE.Mesh(new THREE.RingGeometry(0.34, 0.44, 20).rotateX(-Math.PI/2),
        new THREE.MeshBasicMaterial({color: c, transparent: true, opacity: 0.55, side: THREE.DoubleSide}));
      ring.position.y = 0.03; gp.add(ring);
      gp.add(beacon(c));
      gp.position.set(pk.at[0], pk.at[1] + 0.55, pk.at[2]); scene.add(gp); pickMeshes[idx] = gp;
    };
    // a loose pack (the PACK_GAP drop ring) is the same crate the map's fixed packs use,
    // rebuilt in place each time the ring replaces the slot
    R.addPack = function(idx, pk){
      var old = pickMeshes[idx]; if(old) scene.remove(old);
      var gp = new THREE.Group();
      var base = new THREE.Mesh(new THREE.BoxGeometry(0.5, 0.5, 0.5),
        mat({color: pk.kind === "health" ? 0xf4f4f4 : 0x3c7a3a, rough: 0.4, metal: 0.25}));
      gp.add(base);
      if(pk.kind === "health"){
        var red = mat({color: 0xe0574a, em: PBR ? 0xe0574a : null, emMax: 0.7, rough: 0.5});
        gp.add(new THREE.Mesh(new THREE.BoxGeometry(0.52, 0.14, 0.36), red));
        gp.add(new THREE.Mesh(new THREE.BoxGeometry(0.14, 0.36, 0.52), red).rotateY(0));
        gp.add(new THREE.Mesh(new THREE.BoxGeometry(0.36, 0.14, 0.52), red));
        gp.add(new THREE.Mesh(new THREE.BoxGeometry(0.14, 0.52, 0.36), red));
      } else {
        var gold = mat({color: 0xe0b325, em: PBR ? 0xe0b325 : null, emMax: 0.7, rough: 0.35, metal: 0.6});
        gp.add(new THREE.Mesh(new THREE.BoxGeometry(0.54, 0.12, 0.54), gold));
      }
      gp.add(beacon(pk.kind === "health" ? 0xf4f4f4 : 0xe0b325));
      gp.position.set(pk.at[0], pk.at[1] + 0.55, pk.at[2]); scene.add(gp); pickMeshes[idx] = gp;
    };
    R.charm = function(P, kind){
      if(!P || !P.mesh || !P.mesh.g) return;
      var frz = kind === "freeze";
      if(P.marks && P.marks.some(function(mk){ return mk.__freeze === frz; })) return;   // one per kind
      var c = frz ? TOTEMS.freeze.c : TOTEMS.mole.c;
      if(!P.marks) P.marks = [];
      var tag = new THREE.Mesh(new THREE.PlaneGeometry(0.18, 0.18),
        new THREE.MeshBasicMaterial({color: c, transparent: true, opacity: 0.85, side: THREE.DoubleSide}));
      tag.position.set(0, 2.6, 0); tag.__freeze = frz; P.mesh.g.add(tag); P.marks.push(tag);
    };
    R.addDecoy = function(dc){
      // the decoy is a copy of my character standing where I faced, its albedo tipped toward
      // pale cyan so it's clearly not the real me (and so a friendly doesn't shoot at it)
      var o = GL[CHARS[clamp(V.myCh|0, 0, CHARS.length - 1)].f]; if(!o) return;
      var cl = lib.clone(o.scene); cl.scale.setScalar(1.75/CH_H);
      cl.position.set(dc.x, dc.y, dc.z);
      cl.rotation.y = dc.yaw != null ? dc.yaw : V.yaw;
      cl.traverse(function(n){
        if(!n.isMesh) return;
        n.material = n.material.clone();
        if(n.material.color) n.material.color = n.material.color.clone().lerp(new THREE.Color(0xbfeef7), 0.5);
        n.castShadow = !V.low;
      });
      scene.add(cl); dc.mesh = cl;
    };

    R.boomsTick = boomsTick;
    R.size = function(w, h){ renderer.setSize(w, h, false); camera.aspect = w/h; camera.updateProjectionMatrix(); vCam.aspect = w/h; vCam.updateProjectionMatrix(); };
    R.camera = camera;
    R.render = function(dt, t){
      var quiet = calm();
      // others (a new look or team re-renders the whole mesh)
      others().forEach(function(P){
        // rebuild also when the friend/foe relation flips: the red-vs-ally name tag is baked
        // into the tag sprite, and in MP my own team arrives with the first view/snapshot
        if(P.mesh && (P.mesh.ch !== P.ch || P.mesh.team !== P.team || P.mesh.bot !== P.bot || P.mesh.foe !== (P.team !== V.team))) removePlayer(P);
        if(!P.mesh) addPlayer(P);
        var m = P.mesh; if(!m) return;
        m.g.visible = !P.gone && !(P.dead && t - (P.deadAt || 0) > 1.6);
        m.g.position.set(P.x, P.y, P.z); m.g.rotation.y = Math.PI - P.yaw;
        setAnim(m, P.dead ? "die" : P.moving > 0.5 ? "walk" : "idle");
        if(m.gun){ m.gun[0].visible = (P.w|0) === 0; m.gun[1].visible = (P.w|0) === 1; m.gun[2].visible = (P.w|0) === 2; m.gun[3].visible = (P.w|0) === 3; }
        m.mixer.update(dt);
        m.tag.visible = !P.dead;
        if(m.shield){
          m.shield.visible = !P.dead && botShielded(P, t);
          if(m.shield.visible && !quiet) m.shield.rotation.y = t*0.8;   // a slow spin so it reads as a live shield
        }
      });
      // pickups and drones
      pickMeshes.forEach(function(p, i){
        p.visible = !!V.items[i] && !!(MAP.pickups[i] && MAP.pickups[i].kind);
        if(!quiet) p.rotation.y = t*1.5;
        p.position.y = (MAP.pickups[i] ? MAP.pickups[i].at[1] : 0) + 0.55 + (quiet ? 0 : Math.sin(t*2 + i)*0.08);
      });
      // totem slots: the old map-drop plays out the same but the gem colours say what it is
      // and a fading decoy must clear its mesh the moment the window ends
      for(var dci = (V.decoys || []).length - 1; dci >= 0; dci--){
        var dc = V.decoys[dci];
        if(dc.until > t){ if(dc.mesh && !quiet) dc.mesh.rotation.y = t*0.4; continue; }
        if(dc.mesh){ scene.remove(dc.mesh); dc.mesh = null; }
        V.decoys.splice(dci, 1);
      }
      droneMeshes.forEach(function(d, i){
        var D = V.drones[i]; d.visible = !!D && V.mode === "practice" && D.downUntil <= t;
        if(!D) return; var c = dronePos(D, V.phase === "round" ? t - V.startAt : 0); d.position.set(c[0], c[1] - 0.3, c[2]);
        d.rotation.y = Math.atan2(V.me.x - c[0], V.me.z - c[2]);
      });
      // effects fade
      for(var i = tracers.length - 1; i >= 0; i--){ var a = t - tracers[i].at; if(a > 0.09){ dyn.remove(tracers[i].o); tracers[i].o.geometry.dispose(); tracers.splice(i, 1); } else tracers[i].o.material.opacity = 1 - a/0.09; }
      for(var j = puffs.length - 1; j >= 0; j--){ var b = t - puffs[j].at; if(b > 0.25){ dyn.remove(puffs[j].o); puffs.splice(j, 1); } else { puffs[j].o.material.opacity = 1 - b/0.25; if(!quiet) puffs[j].o.scale.setScalar(0.35 + b*1.6); } }
      if(R3.boomsTick) R3.boomsTick(t, quiet);
      // damage numbers: one pass, in-place removal, o===null means "spawn this frame"
      for(var dn = V.dmgNums.length - 1; dn >= 0; dn--){
        var D2 = V.dmgNums[dn], age = t - D2.at;
        if(age > 0.8){
          if(D2.o){ dyn.remove(D2.o); D2.o.material.map.dispose(); D2.o.material.dispose(); D2.o = null; }
          V.dmgNums.splice(dn, 1); continue;
        }
        if(!D2.o){ D2.o = dmgSprite(D2.n, D2.head); D2.o.position.set(D2.x, D2.y, D2.z); dyn.add(D2.o); }
        D2.o.position.y = D2.y + age*0.9;
        D2.o.material.opacity = age < 0.5 ? 1 : 1 - (age - 0.5)/0.3;
      }
      // camera: your eyes (lower when down); spectating, a chase camera behind whoever you watch
      var M = V.me, ey = V.dead ? 0.5 : FS.EYE, WP = V.watch && V.players[V.watch];
      // your own shield bubble: rides your feet, spinning gently — visible only while it holds
      var meSh = !V.dead && !WP && meShielded(t);
      myShield.visible = meSh;
      if(meSh){ myShield.position.set(V.me.x, V.me.y + 0.95, V.me.z); if(!quiet) myShield.rotation.y = t*0.8; }
      // kill cam: dead, and we know who did it — a third-person chase on them for the respawn
      // wait (same seat the spectate bar uses: behind and above their shoulder, looking where they look)
      var KC = !WP && V.dead && V.killCam && V.killCam.ref && !V.killCam.ref.gone ? V.killCam : null;
      if(KC && (KC.ref.dead || KC.ref.team == null)){ KC = null; }
      if(WP){
        var fwx = Math.sin(WP.yaw), fwz = -Math.cos(WP.yaw);
        camera.position.set(WP.x - fwx*3.6, WP.y + 2.3, WP.z - fwz*3.6);
        camera.rotation.order = "YXZ"; camera.lookAt(WP.x + fwx*2, WP.y + 1.2, WP.z + fwz*2);
        M = WP;
      } else if(KC){
        // the reference tracks the live player (MP snapshots / solo botStep keep it moving),
        // so this is where they are NOW, not where they shot from
        var kx = Math.sin(KC.ref.yaw || 0), kz = -Math.cos(KC.ref.yaw || 0);
        camera.position.set(KC.ref.x - kx*3.6, KC.ref.y + 2.3, KC.ref.z - kz*3.6);
        camera.rotation.order = "YXZ"; camera.lookAt(KC.ref.x + kx*2, KC.ref.y + 1.2, KC.ref.z + kz*2);
        M = KC.ref;
      } else {
        // lean: slide the camera sideways (the same offset your shots come from) and roll it a touch
        var pk = V.peek || 0, plx = Math.cos(V.yaw)*PEEK_SIDE*pk, plz = Math.sin(V.yaw)*PEEK_SIDE*pk;
        camera.position.set(M.x + plx, M.y + ey, M.z + plz);
        camera.rotation.order = "YXZ"; camera.rotation.set(V.pitch + (V.dead ? -0.3 : 0), -V.yaw, V.dead && !quiet ? 0.35 : PEEK_ROLL*pk);
      }
      // slide: a grounded sprint's camera dips then settles (Calm stills it; never while you watch someone)
      if(!quiet && V.slideK > 0 && !WP && !KC){
        camera.position.y -= V.slideK*SLIDE_DIP;
        camera.rotation.z += V.slideRoll;
      }
      // launcher blast: the world shudders for a moment (Calm stills it)
      if(!quiet && t - V.shakeAt < 0.35){
        var st = 1 - (t - V.shakeAt)/0.35, st2 = st*st*0.05;
        camera.position.x += (FS.mb32((t*397)|0) - 0.5)*2*st2;
        camera.position.y += (FS.mb32((t*397)|0 ^ 5917) - 0.5)*2*st2;
        camera.rotation.z += (FS.mb32((t*397)|0 ^ 1301) - 0.5)*2*st2*0.8;
      }
      sun.position.set(M.x + 14, 30, M.z + 10); sun.target.position.set(M.x, 0, M.z);
      // scoped in: ease to this weapon's ADS FOV and slow the bob so the sight picture settles (visual only)
      V.fov = V.fov || camera.fov;
      var wantFov = V.ads ? ((ADS[V.w] || {fov: FOV_ADS}).fov) : FOV;
      V.fov += (wantFov - V.fov)*clamp(dt*10, 0, 1);
      if(Math.abs(camera.fov - V.fov) > 0.01){ camera.fov = V.fov; camera.updateProjectionMatrix(); }
      // the weapon in your hands: bob, recoil, a muzzle flash
      guns.forEach(function(g, gi){ g.visible = V.w === gi; });
      if(seatFlash && guns[V.w] && V.w !== V.seatedW){ V.seatedW = V.w; seatFlash(guns[V.w]); }
      var spd = Math.sqrt((M.vx || 0)*(M.vx || 0) + (M.vz || 0)*(M.vz || 0)), bobA = quiet ? 0 : clamp(spd/FS.RUN, 0, 1)*0.012*(V.ads ? 0.3 : 1);
      var rl = V.reloadAt ? clamp((V.reloadAt - t)/W().reload, 0, 1) : 0, dip = V.reloadAt ? Math.sin(rl*Math.PI)*0.25 : 0;
      // scoped: the gun rises to your eye — centred under the dot, the scope ring blacks out the rest
      var adsIn = V.ads ? 1 : 0;
      var adsCfg = ADS[V.w] || {fov: FOV_ADS, cls: ""};  // the launcher (index 3) takes the generic: a light zoom, no sight art
      var reddot = V.ads && adsCfg.cls === "reddot";
      var revAds = V.ads && adsCfg.cls === "";  // revolver/launcher: no sight — gun big by the eye, sitting below the aim point
      var ax = 0.2*(1 - adsIn), ay = (-0.25 + 0.09*adsIn) - Math.abs(Math.cos(V.bob*2))*bobA - dip*0.3 - (V.dead ? 1 : 0), az = -0.42 + 0.10*adsIn;
      if(reddot){ ay = -0.41; }  // SMG sighted: dropped clear of the sight's bottom border (size stays 1 — scale grows it into view)
      if(revAds){ ay = -0.27; }  // revolver/launcher sighted: big by the eye, sitting just below the aim point
      var gsc = reddot ? 1.4 : revAds ? 1.45 : 1;  // SMG z stays put (scaling z distorts the pose); revolver/launcher come close
      gunHold.scale.set(gsc, gsc, gsc);
      gunHold.position.set(ax + Math.sin(V.bob*2)*bobA, ay, ((V.ads ? (revAds ? -0.52 : -0.8) : az)) + V.kick*(V.w ? 0.06 : 0.03));
      gunHold.rotation.set(V.kick*(V.w ? 0.12 : 0.05) - dip - (reddot ? 0 : 0.18*adsIn), 0, 0);
      var sniper = V.ads && adsCfg.cls === "sniper";
      gunHold.visible = !V.dead && !sniper;
      if(flash){ var fa = t - V.flashAt; flash.visible = fa < (quiet ? 0.03 : 0.05) && !V.dead; flash.scale.setScalar((quiet ? 0.07 : 0.13)*(V.w ? 1.3 : 1)); flash.material.opacity = quiet ? 0.5 : 0.95; flash.material.rotation = V.n*1.3; }
      if(muzzle){ muzzle.intensity = (t - V.flashAt) < 0.06 && !V.dead && !quiet ? (V.w ? 26 : 15) : 0; }
      // taking a hit: a small shake (the red vignette itself is the existing .vg-fps-vig overlay)
      var hurtA = clamp(1 - (now() - V.hurtAt)/0.5, 0, 1);
      if(!quiet && hurtA > 0){ var sh = hurtA*0.05; camera.position.x += Math.sin(t*47)*sh; camera.position.y += Math.sin(t*61)*sh; camera.rotation.z += Math.sin(t*53)*sh*0.4; }
      // the dock breathes: emissive accents pulse gently with the round (off when calm)
      for(var pi = 0; pi < pulseMats.length; pi++){ pulseMats[pi].m.emissiveIntensity = quiet ? pulseMats[pi].max*0.4 : pulseMats[pi].max*(0.55 + 0.45*Math.sin(t*1.4 + pi)); }
      renderer.clear();
      renderer.render(scene, camera);
      renderer.clearDepth();
      if(!V.watch && !KC) renderer.render(vScene, vCam);  // no weapon in your hands while you watch — or ride a kill cam
    };
    R.dispose = function(){
      cv.removeEventListener("webglcontextlost", lost);
      clearDynamic(); renderer.dispose();
      try { var ext = renderer.getContext().getExtension("WEBGL_lose_context"); if(ext) ext.loseContext(); } catch(e){}
    };
    return R;
  }
  function onResize(){ resize(); }
  window.addEventListener("resize", onResize);
  if(typeof ResizeObserver !== "undefined"){ ro = new ResizeObserver(onResize); ro.observe(host); }
  renderKeys();
  renderMenu();
  loadMap().then(function(){ if(!V.alive) return; renderMenu(); if(V.mode === "mp" && V.gotView) applyView(V.round); else if(V.mode === "mp") requestView(); },
    function(){ V.note = "Couldn't load the dock — refresh the page and try again."; renderMenu(); });
  if(!raf) raf = requestAnimationFrame(frame);
  V.destroy = function(){
    if(WB){ WB.destroy(); WB = null; }
    V.alive = false;
    if(raf) cancelAnimationFrame(raf); raf = 0;
    window.removeEventListener("resize", onResize); if(ro) ro.disconnect();
    window.removeEventListener("keydown", onTab, true); window.removeEventListener("keyup", onTab, true);
    if(IN){ IN.destroy(); IN = null; }
    if(R3){ R3.dispose(); R3 = null; }
    root.remove();
  };
  V.onEvent = onEvent; V.onError = onError;
  V.onConn = function(on){
    V.offline = !on; badge.classList.toggle("hidden", on);
    if(!on){ V.off = null; V.k = null; others().forEach(function(P){ P.last = -1; P.snaps.length = 0; }); }
  };
  // Test hooks (the browser smoke tests; headless browsers can't lock the pointer).
  V.startPractice = startPractice;
  V.startBotMatch = startBotMatch;
  V.rendererKind = function(){ return R3 ? "3d" : R2 && R2.canvas ? "2d" : ""; };
  V.look = function(yawDeg, pitchDeg){ V.yaw = (+yawDeg || 0)*Math.PI/180; V.pitch = clamp(+pitchDeg || 0, -PITCH_MAX, PITCH_MAX)*Math.PI/180; };
  V.aimAt = function(uid){
    var P = V.players[uid]; if(!P) return null;
    var e = eye(), dx = P.x - e[0], dy = P.y + 1.0 - e[1], dz = P.z - e[2];
    V.look(Math.atan2(dx, -dz)*180/Math.PI, Math.atan2(dy, Math.sqrt(dx*dx + dz*dz))*180/Math.PI);
    return [P.x, P.y, P.z];
  };
  V.aimDrone = function(i){
    var D = V.drones[i|0]; if(!D) return null;
    var c = dronePos(D, V.phase === "round" ? now() - V.startAt : 0), e = eye(), dx = c[0] - e[0], dy = c[1] - e[1], dz = c[2] - e[2];
    V.look(Math.atan2(dx, -dz)*180/Math.PI, Math.atan2(dy, Math.sqrt(dx*dx + dz*dz))*180/Math.PI); return c;
  };
  V.shoot = function(){ var t = now(); if(canShoot(t)){ fire(t); return true; } return false; };
  V.reload = function(){ startReload(now()); };
  V.weapon = function(w){ switchTo(w, now()); };
  V.place = function(x, y, z){ placeMe(x, y, z, null); };
  V.showBoard = function(on){ V.board = !!on; renderBoard(); };
  return V;
}

/* ---------- registration: a solo card and a "with friends" card ---------- */
HQV.register({id: "strike", name: "Strikezone", icon: "💀", desc: "Team shootout: play 1v1 to 5v5 with AI teammates and enemies offline, or match up in the Arena",
  mount: function(el){ if(CUR) CUR.destroy(); CUR = makeGame(el, {mode: "practice"}); },
  unmount: function(){ if(CUR){ CUR.destroy(); CUR = null; } },
  pause: function(){ if(CUR) CUR.hqPaused = true; },
  resume: function(){ if(CUR) CUR.hqPaused = false; }});

if(MP){
  MP.handlers.strike = {
    on: function(m){ if(CUR && CUR.ctx) CUR.onEvent(m); },
    onError: function(m){ return CUR && CUR.ctx ? CUR.onError(m) : false; },
    render: function(){}
  };
  MP.register("strike", "💀", "Team shootout for up to 8 (X vs X, bots fill in): the server judges every shot", function(ctx){
    if(CUR) CUR.destroy();
    var g = CUR = makeGame(ctx.box, {mode: "mp", ctx: ctx});
    ctx.onConn = function(on){ if(g.alive) g.onConn(on); };
    ctx.onRejoin = function(){ if(g.alive && MP) MP.send("strike", "view"); };
    ctx.stop = function(){ g.destroy(); if(CUR === g) CUR = null; };
  });
}
HQV.strikeShared = FS;    // for tests/test_strike_sync.py
HQV.strikeDebug = function(){ return CUR; };
})();
