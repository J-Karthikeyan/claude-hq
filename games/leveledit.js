/* Valley: the Level Editor (HQ 2.5 "Make + compete"). Build a Platformer Rush level from the
 * game's own seven platform models, coins, checkpoints (in order), up to eight spawns and the
 * flag, on a top-down grid with a side profile underneath; a 0.5 m snap, undo / redo, and
 * every action on the keyboard as well as the mouse.
 *
 * The route is the path the bots and the autopilot follow: Auto-route finds one (a greedy
 * search over the platforms), and you can edit it point by point as walks (w), jumps (j) and
 * double jumps (d). The live check below runs the Arena's own rules with its own constants
 * (backend/src/platformer.rs validate_custom; tests/test_leveledit.py keeps them equal):
 * a level the check passes is one the server accepts, and one a runner can finish.
 *
 * Test-run it solo and preview it in 3D (both through games/platformer.js's renderer), keep
 * up to 6 drafts in your Valley save, publish it to the Workshop gallery when that exists,
 * or race it in your Arena room (the host's start op carries it; the server checks it again
 * and boards it under a key made from its content). Nothing transcript-derived is in a level:
 * only numbers, the seven model names, a theme and the name you type.
 */
(function(){
"use strict";
var HQV = window.HQV; if(!HQV || !HQV.api) return;
var api = HQV.api, E = HQV.engine || null;

/* LEVEL-RULES BEGIN */
// The Arena's custom-level rules (backend/src/platformer.rs, "custom levels"). PL is the
// PLAT-LEVEL block of games/platformer.js (support, inside, inFoot, segDist, compileLevel).
function levelRules(PL){
  var K = {LIM: 200, MAX_SOLIDS: 64, MAX_COINS: 80, MAX_CPS: 8, MAX_PLAYERS: 8, MAX_ROUTE: 200, MAX_DECO: 64,
    MAX_DATA: 12288, NAME_MAX: 32, COOP_SECS_MIN: 30, COOP_SECS_MAX: 600, SCALE_MIN: 0.5, SCALE_MAX: 3, DECO_SCALE_MAX: 6,
    ROUTE_JUMP: 8.6, ROUTE_DJUMP: 7.8, ROUTE_GRAV: 26, ROUTE_SPEED: 5.6, ROUTE_STEP_UP: 0.3, ROUTE_SAMPLE: 0.25,
    ROUTE_GROUND: 0.35, ROUTE_LAND: 0.1, ROUTE_ARC_N: 16, ROUTE_FLAG: 2, CP_R: 2, CENTER: 0.45,
    S_JUMP: 9, S_DJUMP: 8.2, S_GRAV: 24, ENV_W: 0.3, ENV_SLACK: 0.5};
  var MODEL_NAMES = ["platform", "platform-medium", "platform-large", "platform-falling", "platform-grass-large-round", "brick", "block-coin"];
  var DECO_MODELS = ["grass", "grass-small", "cloud"];
  var THEME_KEYS = ["sky", "fog", "sea", "light"];
  function r2(n){ var r = (n < 0 ? -1 : 1)*Math.round(Math.abs(n)*100)/100; return r === 0 ? 0 : r; }
  function isNum(v){ return typeof v === "number" && isFinite(v); }
  function Bad(msg){ this.msg = msg; }
  function fail(msg){ throw new Bad(msg); }
  function num(v, what){
    if(typeof v !== "number") fail(what+" is not a number");
    if(!isFinite(v) || Math.abs(v) > K.LIM) fail(what+" is out of range (±"+K.LIM+" m)");
    return r2(v);
  }
  function pt(v, what){
    if(!Array.isArray(v) || v.length !== 3) fail(what+" is not [x, y, z]");
    return [num(v[0], what), num(v[1], what), num(v[2], what)];
  }
  function list(data, key, lo, hi){
    var a = data[key];
    if(!Array.isArray(a)) fail(key+" is missing");
    if(a.length < lo || a.length > hi) fail(key+": "+lo+" to "+hi+" allowed, got "+a.length);
    return a;
  }
  function color(v, what){
    if(typeof v === "string" && /^#[0-9a-fA-F]{6}$/.test(v)) return v.toLowerCase();
    fail("theme "+what+" is not a #rrggbb colour");
  }
  function rot(v, what, right){
    if(v === undefined) return 0;
    if(typeof v !== "number") fail(what+" turn is not a number");
    if(!isFinite(v) || v % 1 !== 0 || v < 0 || v >= 360 || (right && v % 90 !== 0))
      fail(what+" turn must be "+(right ? "0, 90, 180 or 270" : "a whole 0-359"));
    return v;
  }
  function scale(v, what, hi){
    if(v === undefined) return 1;
    var n = num(v, what);
    if(n < K.SCALE_MIN || n > hi) fail(what+" scale must be "+K.SCALE_MIN+" to "+hi);
    return n;
  }
  function obj(v){ return v && typeof v === "object" && !Array.isArray(v) ? v : {}; }
  // the data as the Arena reads it: Bad(reason) on the first thing it refuses
  function parse(data){
    if(!data || typeof data !== "object" || Array.isArray(data)) fail("the level data is not an object");
    var P = {};
    P.kill = num(data.kill, "kill height");
    P.secs = num(data.coopSecs, "co-op time");
    if(P.secs < K.COOP_SECS_MIN || P.secs > K.COOP_SECS_MAX) fail("co-op time must be "+K.COOP_SECS_MIN+" to "+K.COOP_SECS_MAX+" seconds");
    var th = data.theme;
    if(!th || typeof th !== "object" || Array.isArray(th)) fail("theme is missing");
    P.theme = THEME_KEYS.map(function(k){ return color(th[k], k); });
    P.spawns = list(data, "spawns", 1, K.MAX_PLAYERS).map(function(p, i){ return pt(p, "spawn "+(i + 1)); });
    P.cps = list(data, "cps", 0, K.MAX_CPS).map(function(p, i){ return pt(p, "checkpoint "+(i + 1)); });
    if(data.flag === undefined) fail("the flag is missing");
    P.flag = pt(data.flag, "the flag");
    P.coins = list(data, "coins", 0, K.MAX_COINS).map(function(p, i){ return pt(p, "coin "+(i + 1)); });
    var g = data.coopGoal;
    if(typeof g !== "number" || !isFinite(g) || g % 1 !== 0) fail("co-op goal is not a whole number");
    if(g < 0 || g > P.coins.length) fail("co-op goal must be 0 to "+P.coins.length+" (the coins in the level)");
    P.goal = g;
    P.solids = list(data, "solids", 1, K.MAX_SOLIDS).map(function(b, i){
      var what = "platform "+(i + 1), o = obj(b), name = typeof o.m === "string" ? o.m : "";
      if(MODEL_NAMES.indexOf(name) < 0) fail(what+": unknown model '"+name.slice(0, 24)+"'");
      return {m: name, x: num(o.x, what), y: num(o.y, what), z: num(o.z, what), r: rot(o.r, what, true), s: scale(o.s, what, K.SCALE_MAX)};
    });
    P.route = list(data, "route", 2, K.MAX_ROUTE).map(function(p, i){
      var what = "route point "+(i + 1);
      if(!Array.isArray(p) || p.length !== 4) fail(what+" is not [x, y, z, kind]");
      if(p[3] !== "w" && p[3] !== "j" && p[3] !== "d") fail(what+": kind must be w, j or d");
      return [[num(p[0], what), num(p[1], what), num(p[2], what)], p[3]];
    });
    P.deco = list(data, "deco", 0, K.MAX_DECO).map(function(d, i){
      var what = "decoration "+(i + 1), o = obj(d), name = typeof o.m === "string" ? o.m : "";
      if(DECO_MODELS.indexOf(name) < 0) fail(what+": unknown model");
      return {m: name, x: num(o.x, what), y: num(o.y, what), z: num(o.z, what), r: rot(o.r, what, false), s: scale(o.s, what, K.DECO_SCALE_MAX)};
    });
    return P;
  }
  // canonical data: the key order, rounding and colours the Arena stores and keys on
  function canonOf(P){
    function p3(a){ return [a[0], a[1], a[2]]; }
    return {kill: P.kill, coopGoal: P.goal, coopSecs: P.secs,
      theme: {sky: P.theme[0], fog: P.theme[1], sea: P.theme[2], light: P.theme[3]},
      spawns: P.spawns.map(p3), cps: P.cps.map(p3), flag: p3(P.flag), coins: P.coins.map(p3),
      solids: P.solids.map(function(b){ var o = {m: b.m, x: b.x, y: b.y, z: b.z}; if(b.r !== 0) o.r = b.r; if(b.s !== 1) o.s = b.s; return o; }),
      route: P.route.map(function(r){ return [r[0][0], r[0][1], r[0][2], r[1]]; }),
      deco: P.deco.map(function(d){ var o = {m: d.m, x: d.x, y: d.y, z: d.z, r: d.r}; if(d.s !== 1) o.s = d.s; return o; })};
  }
  function liftMax(tau){
    if(tau <= 0) return 0;
    var s = Math.min(tau, Math.max((tau + (K.S_JUMP - K.S_DJUMP)/K.S_GRAV)/2, 0)), r = tau - s;
    return K.S_JUMP*s - K.S_GRAV*s*s/2 + K.S_DJUMP*r - K.S_GRAV*r*r/2;
  }
  var T_APEX = (K.S_JUMP + K.S_DJUMP)/K.S_GRAV;
  function liftBound(tau){ return liftMax(Math.min(Math.max(T_APEX, tau - K.ENV_W), tau + K.ENV_W)) + K.ENV_SLACK; }
  function air(dh, kind){
    if(kind === "j"){
      var disc = K.ROUTE_JUMP*K.ROUTE_JUMP - 2*K.ROUTE_GRAV*dh;
      return disc < 0 ? null : (K.ROUTE_JUMP + Math.sqrt(disc))/K.ROUTE_GRAV;
    }
    var h1 = K.ROUTE_JUMP*K.ROUTE_JUMP/(2*K.ROUTE_GRAV), d2 = K.ROUTE_DJUMP*K.ROUTE_DJUMP + 2*K.ROUTE_GRAV*(h1 - dh);
    return d2 < 0 ? null : K.ROUTE_JUMP/K.ROUTE_GRAV + (K.ROUTE_DJUMP + Math.sqrt(d2))/K.ROUTE_GRAV;
  }
  function lift(t, kind){
    var t1 = K.ROUTE_JUMP/K.ROUTE_GRAV;
    if(kind === "j" || t <= t1) return K.ROUTE_JUMP*t - K.ROUTE_GRAV*t*t/2;
    var r = t - t1;
    return K.ROUTE_JUMP*t1/2 + K.ROUTE_DJUMP*r - K.ROUTE_GRAV*r*r/2;
  }
  function groundNear(L, x, y, z){
    for(var i = 0; i < L.solids.length; i++){ var s = L.solids[i]; if(PL.inFoot(s, x, z, 0) && Math.abs(s.y1 - y) <= K.ROUTE_GROUND) return true; }
    return false;
  }
  function leg(a, b, kind){
    if(kind === "w") return [a, b];
    var t = air(b[1] - a[1], kind) || 0, out = [];
    for(var j = 0; j <= K.ROUTE_ARC_N; j++){
      var u = j/K.ROUTE_ARC_N;
      out.push(j === K.ROUTE_ARC_N ? b : [a[0] + (b[0] - a[0])*u, a[1] + lift(u*t, kind), a[2] + (b[2] - a[2])*u]);
    }
    return out;
  }
  // why the leg a -> b (b's kind) can't be run, or null
  function legProblem(L, a, b, kind){
    var dx = b[0] - a[0], dz = b[2] - a[2], dh = b[1] - a[1], d = Math.sqrt(dx*dx + dz*dz);
    if(PL.support(L, b[0], b[1], b[2], 0, K.ROUTE_LAND) === null) return kind === "w" ? "walks onto nothing" : "lands in the air";
    if(kind === "w"){
      if(Math.abs(dh) > K.ROUTE_STEP_UP) return "is a walk up or down more than 0.3 m: make it a jump";
      var n = Math.max(1, Math.ceil(d/K.ROUTE_SAMPLE));
      for(var j = 0; j <= n; j++){
        var u = j/n, x = a[0] + dx*u, y = a[1] + dh*u, z = a[2] + dz*u;
        if(!groundNear(L, x, y, z)) return "walks over a gap: make it a jump";
        if(PL.inside(L, x, y, z)) return "walks into a block";
      }
      return null;
    }
    var t = air(dh, kind);
    if(t === null) return kind === "j" ? "is too high for a jump: try a double jump" : "is too high even for a double jump";
    if(d > K.ROUTE_SPEED*t) return "is too far to "+(kind === "j" ? "jump: try a double jump" : "reach: the gap is too wide");
    if(dh > liftBound(t)) return "climbs faster than the referee allows";
    var pts = leg(a, b, kind);
    for(var i = 1; i < pts.length - 1; i++) if(PL.inside(L, pts[i][0], pts[i][1], pts[i][2])) return "jumps through a platform";
    return null;
  }
  function onTop(L, p){ return PL.support(L, p[0], p[1], p[2], 0, K.ROUTE_LAND) !== null && !PL.inside(L, p[0], p[1], p[2]); }
  // Everything wrong, in the order the Arena finds it (errors[0] is what the Arena answers),
  // plus per-leg problems for drawing: legs[i] is route point i's leg (-1 - k: spawn k's).
  function check(data){
    var out = {ok: false, errors: [], legs: {}, canon: null, L: null, P: null, cpHit: 0};
    var P;
    try {
      if(JSON.stringify(data).length > K.MAX_DATA*2) fail("the level is too big (12 KiB at most)");
      P = parse(data);
    } catch(e){ if(e instanceof Bad){ out.errors.push(e.msg); return out; } throw e; }
    out.P = P;
    var canon = canonOf(P), L = PL.compileLevel(canon);
    out.canon = canon; out.L = L;
    var low = 1e9; L.solids.forEach(function(s){ low = Math.min(low, s.y0); });
    if(P.kill > low - 1) out.errors.push("the kill height must be at least 1 m below the lowest platform");
    P.spawns.forEach(function(s, i){ if(!onTop(L, s)) out.errors.push("spawn "+(i + 1)+" isn't standing on a platform"); });
    P.cps.forEach(function(c, i){ if(!onTop(L, c)) out.errors.push("checkpoint "+(i + 1)+" isn't on a platform"); });
    if(!onTop(L, P.flag)) out.errors.push("the flag isn't on a platform");
    var b = L.bounds;
    P.coins.forEach(function(c, i){
      if(PL.inside(L, c[0], c[1], c[2]) || !(b[0] <= c[0] && c[0] <= b[1] && P.kill < c[1] && c[1] <= b[3] && b[4] <= c[2] && c[2] <= b[5]))
        out.errors.push("coin "+(i + 1)+" is inside a block or out of reach");
    });
    var R = P.route, first = R[0];
    P.spawns.forEach(function(s, i){
      var why = legProblem(L, s, first[0], first[1]);
      if(why){ out.errors.push("from spawn "+(i + 1)+" to route point 1 "+why); out.legs[-1 - i] = why; }
    });
    var k = 0;
    function up(p){ return [p[0], p[1] + K.CENTER, p[2]]; }
    for(var i = 0; i < R.length; i++){
      var bb = R[i][0], kind = R[i][1], a = i === 0 ? P.spawns[0] : R[i - 1][0];
      if(i > 0){ var w = legProblem(L, a, bb, kind); if(w){ out.errors.push("route point "+(i + 1)+" "+w); out.legs[i] = w; } }
      var pts = leg(a, bb, kind);
      for(var j = 1; j < pts.length; j++){
        if(k < P.cps.length && PL.segDist(up(pts[j - 1]), up(pts[j]), up(P.cps[k])) <= K.CP_R) k++;
      }
    }
    out.cpHit = k;
    if(k < P.cps.length) out.errors.push("the route misses checkpoint "+(k + 1)+" (they count in order)");
    var end = R[R.length - 1][0], fx = end[0] - P.flag[0], fy = end[1] - P.flag[1], fz = end[2] - P.flag[2];
    if(Math.sqrt(fx*fx + fy*fy + fz*fz) > K.ROUTE_FLAG) out.errors.push("the route doesn't end at the flag");
    if(!out.errors.length && JSON.stringify(canon).length > K.MAX_DATA) out.errors.push("the level is too big (12 KiB at most)");
    out.ok = !out.errors.length;
    return out;
  }
  function cleanName(v){
    if(typeof v !== "string") return {error: "the level needs a name"};
    var s = v.trim(), n = Array.from(s).length;
    if(!n || n > K.NAME_MAX) return {error: "the name must be 1 to "+K.NAME_MAX+" characters"};
    for(var i = 0; i < s.length; i++){
      var u = s.charCodeAt(i);
      if(u < 32 || (u >= 127 && u <= 160 && u !== 32) || u === 173 || (u >= 0x2000 && u <= 0x200f) || (u >= 0x2028 && u <= 0x202f) ||
         (u >= 0x205f && u <= 0x206f) || u === 0x3000 || u === 0xfeff || (u >= 0xe000 && u <= 0xf8ff)) return {error: "the name has characters that aren't allowed"};
    }
    if(/[<>]/.test(s) || /http/i.test(s)) return {error: "the name has characters that aren't allowed"};
    return {name: s};
  }
  return {K: K, MODEL_NAMES: MODEL_NAMES, r2: r2, isNum: isNum, check: check, legProblem: legProblem, leg: leg, air: air,
          lift: lift, liftBound: liftBound, cleanName: cleanName};
}
/* LEVEL-RULES END */

var PLv = HQV.platLevel;
if(!PLv) return;                          // needs games/platformer.js (its PLAT-LEVEL block and renderer)
var RU = levelRules(PLv), K = RU.K, r2 = RU.r2;

/* ---------- the palette and starting points ---------- */
var MODEL_UI = [["platform", "Platform 2×2"], ["platform-medium", "Medium 3×3"], ["platform-large", "Large 5×5"],
  ["platform-falling", "Falling 2.2"], ["platform-grass-large-round", "Round island ⌀5"], ["brick", "Brick 1×1"], ["block-coin", "Coin block 1×1"]];
var TOOLS = [["select", "Select / move", "1"], ["solid", "Platform", "2"], ["coin", "Coin", "3"], ["cp", "Checkpoint", "4"],
  ["spawn", "Spawn", "5"], ["flag", "Flag", "6"], ["route", "Route point", "7"], ["erase", "Erase", "8"]];
var KINDS = [["w", "Walk"], ["j", "Jump"], ["d", "Double jump"]];
// themes as the level stores them (hex is level data, like the built-in levels; the page never styles with it)
var THEMES = [["Meadow", {sky: "#8fd3ff", fog: "#cdeeff", sea: "#5aa9e6", light: "#fff6e0"}],
  ["Sky", {sky: "#b8c8ff", fog: "#e4e8ff", sea: "#7d8fd8", light: "#ffffff"}],
  ["Sunset", {sky: "#ffc9a8", fog: "#ffe6d6", sea: "#c98fb8", light: "#ffe9d0"}],
  ["Mint", {sky: "#b6f0d8", fog: "#e2fbf0", sea: "#4fb3a0", light: "#f4fff6"}],
  ["Dusk", {sky: "#5b5f99", fog: "#8a86b8", sea: "#3a4f80", light: "#ffd9a8"}]];
var MAX_DRAFTS = 6, DRAFT_MAX = 8192, SNAP = 0.5, UNDO_MAX = 80;

function starter(){
  return {kind: "plat", v: 1, name: "Three Hops", data: {
    kill: -6, coopGoal: 2, coopSecs: 90, theme: THEMES[0][1],
    spawns: [[-1.5, 0, 1.5], [-0.5, 0, 1.5], [0.5, 0, 1.5], [1.5, 0, 1.5]], cps: [[0, 0.5, -6]], flag: [0, 1, -12],
    coins: [[0, 1.6, -3.5], [0, 2, -8.5]],
    solids: [{m: "platform-large", x: 0, y: -0.5, z: 0}, {m: "platform-medium", x: 0, y: 0, z: -6}, {m: "platform-large", x: 0, y: 0.5, z: -12}],
    route: [[0, 0, -2, "w"], [0, 0.5, -5, "j"], [0, 0.5, -7, "w"], [0, 1, -10, "j"], [0, 1, -12, "w"]],
    deco: [{m: "cloud", x: 9, y: 7, z: -4, r: 0, s: 3.2}, {m: "cloud", x: -10, y: 9, z: -14, r: 90, s: 4}]}};
}
function blank(){
  return {kind: "plat", v: 1, name: "My level", data: {
    kill: -6, coopGoal: 0, coopSecs: 120, theme: THEMES[1][1],
    spawns: [[-1, 0, 1.5], [1, 0, 1.5]], cps: [], flag: [0, 0, -1.5], coins: [],
    solids: [{m: "platform-large", x: 0, y: -0.5, z: 0}],
    route: [[0, 0, 0, "w"], [0, 0, -1.5, "w"]], deco: []}};
}
function clone(o){ return JSON.parse(JSON.stringify(o)); }

/* ---------- drafts in the Valley save: workshop.plat (≤ 6, each < 8 KiB) ---------- */
function wsave(){
  var s = api.save; if(!s) return [];
  if(!s.workshop || typeof s.workshop !== "object" || Array.isArray(s.workshop)) s.workshop = {};
  if(!Array.isArray(s.workshop.plat)) s.workshop.plat = [];
  s.workshop.plat = s.workshop.plat.filter(function(d){ return d && typeof d === "object" && d.kind === "plat" && d.data && typeof d.data === "object"; }).slice(0, MAX_DRAFTS);
  return s.workshop.plat;
}
function docOf(name, data){
  var c = RU.check(data);
  return {kind: "plat", v: 1, name: String(name || "").trim().slice(0, K.NAME_MAX), data: c.canon || data};
}
function mapsOn(){ var A = window.ARENA; return !!(A && A.arena && A.arena.maps); }
function storyNote(ev){ try { if(HQV.story && HQV.story.note) HQV.story.note(ev); } catch(e){} }
function say(t){ if(E && E.say) E.say(t); }
function tokens(){ return E && E.tokens ? E.tokens() : {ink: "currentColor", muted: "gray", line: "gray", panel: "canvas", panel2: "canvas",
  brand: "royalblue", need: "crimson", good: "seagreen", gold: "goldenrod", bg2: "canvas", mono: "monospace"}; }

var CUR = null, OPEN_WITH = null;

/* =============================== the editor =============================== */
function makeEditor(host, first){
  var S = {doc: clone(first && first.doc || starter()), idx: first && first.idx != null ? first.idx : -1, tool: "solid",
    model: "platform", scale: 1, layer: 0, kind: "j", sel: null, cur: [0, -3], cam: {x: 0, z: -5, s: 20}, side: "z",
    undo: [], redo: [], dirty: false, chk: null, game: null, alive: true, drag: null, scope: "public", mode: "race"};
  var D = function(){ return S.doc.data; };
  var root = api.mk("div", "vg-golf vg-ledit"), edit = api.mk("div", "vg-ledit-main"), playBox = api.mk("div", "vg-ledit-play hidden");
  root.appendChild(edit); root.appendChild(playBox); host.appendChild(root);

  /* ----- header: name, drafts, theme ----- */
  var head = api.mk("div", "vg-row");
  var nameL = api.mk("label", "vg-golf-check"), nameIn = api.mk("input", "vg-input");
  nameIn.type = "text"; nameIn.maxLength = K.NAME_MAX; nameIn.value = S.doc.name; nameIn.setAttribute("aria-label", "Level name");
  nameIn.addEventListener("input", function(){ S.doc.name = nameIn.value; S.dirty = true; recheck(); });
  nameL.appendChild(document.createTextNode("Name ")); nameL.appendChild(nameIn); head.appendChild(nameL);
  var draftsSel = api.mk("select", "vg-select"); draftsSel.setAttribute("aria-label", "Open a saved level");
  draftsSel.addEventListener("change", function(){ var v = draftsSel.value; draftsSel.value = ""; openChoice(v); });
  head.appendChild(draftsSel);
  head.appendChild(api.btn("Save", "primary", function(){ saveDraft(); }));
  var pubBtn = api.btn("Publish…", "", function(){ publish(); }); head.appendChild(pubBtn);
  var delBtn = api.btn("Delete draft", "ghost", function(){ deleteDraft(); }); head.appendChild(delBtn);
  edit.appendChild(head);

  var themeRow = api.mk("div", "vg-row"); themeRow.setAttribute("role", "radiogroup"); themeRow.setAttribute("aria-label", "Theme");
  edit.appendChild(themeRow);

  /* ----- tools ----- */
  var toolRow = api.mk("div", "vg-row"); toolRow.setAttribute("role", "toolbar"); toolRow.setAttribute("aria-label", "Tools (keys 1 to 8)");
  edit.appendChild(toolRow);
  var optRow = api.mk("div", "vg-row"); edit.appendChild(optRow);

  /* ----- canvases ----- */
  var topC = api.mk("canvas", "vg-golf-canvas vg-ledit-top"); topC.tabIndex = 0;
  topC.setAttribute("role", "application");
  topC.setAttribute("aria-label", "Level grid, seen from above. Arrow keys move the cursor half a metre (Shift: 2 m), Enter places the tool, "+
    "Delete removes, Shift with arrows moves the selected item, Page Up and Page Down change the height, 1 to 8 pick a tool, "+
    "Ctrl Z undoes, plus and minus zoom.");
  var sideC = api.mk("canvas", "vg-golf-canvas vg-ledit-side"); sideC.setAttribute("aria-hidden", "true");
  var status = api.mk("p", "vg-muted"); status.setAttribute("aria-live", "polite");
  edit.appendChild(topC); edit.appendChild(status); edit.appendChild(sideC);
  var sideRow = api.mk("div", "vg-row"); edit.appendChild(sideRow);

  /* ----- inspector, check, actions ----- */
  var insp = api.mk("div", "vg-golf-score"); edit.appendChild(insp);
  var settings = api.mk("div", "vg-row"); edit.appendChild(settings);
  var chkBox = api.mk("div", "vg-golf-score"); chkBox.setAttribute("aria-live", "polite"); edit.appendChild(chkBox);
  var acts = api.mk("div", "vg-row"); edit.appendChild(acts);
  var keysBox = api.mk("div", "vg-golf-keys"); edit.appendChild(keysBox);
  [["1-8", "pick a tool"], ["Arrows / Shift", "cursor ½ m / 2 m"], ["Enter / Space", "place or select"], ["Shift + arrows", "move the selected item"],
   ["PgUp / PgDn  [ ]", "height ±½ m"], ["Delete", "remove"], ["Ctrl Z / Ctrl Y", "undo / redo"], ["+ / −", "zoom"], ["Right-drag", "pan"]].forEach(function(r){
    var it = api.mk("span", "vg-golf-key"); it.appendChild(api.mk("kbd", null, r[0])); it.appendChild(document.createTextNode(" "+r[1])); keysBox.appendChild(it);
  });

  /* ---------- history ---------- */
  function snap(){ return JSON.stringify(S.doc); }
  function remember(){
    S.undo.push(snap()); if(S.undo.length > UNDO_MAX) S.undo.shift(); S.redo.length = 0; S.dirty = true;
  }
  function undo(){
    if(!S.undo.length){ say("Nothing to undo"); return; }
    S.redo.push(snap()); S.doc = JSON.parse(S.undo.pop()); S.sel = null; nameIn.value = S.doc.name; S.dirty = true; changed("Undone");
  }
  function redo(){
    if(!S.redo.length){ say("Nothing to redo"); return; }
    S.undo.push(snap()); S.doc = JSON.parse(S.redo.pop()); S.sel = null; nameIn.value = S.doc.name; S.dirty = true; changed("Redone");
  }

  /* ---------- geometry helpers ---------- */
  function snapv(v){ return r2(Math.round(v/SNAP)*SNAP); }
  function compiled(){ try { return PLv.compileLevel(D()); } catch(e){ return null; } }
  function topAt(x, z, below){
    var L = compiled(), best = null; if(!L) return null;
    L.solids.forEach(function(s){ if(PLv.inFoot(s, x, z, 0) && (below == null || s.y1 <= below + 0.05) && (best === null || s.y1 > best)) best = s.y1; });
    return best;
  }
  function solidH(b){ var M = PLv.MODELS[b.m]; return M ? M[3]*(b.s != null ? +b.s : 1) : 0.5; }
  function itemsAt(x, z){
    var hits = [], d = D(), R = 0.6;
    function near(p){ var dx = p[0] - x, dz = p[2] - z; return dx*dx + dz*dz <= R*R; }
    (d.route || []).forEach(function(p, i){ if(near(p)) hits.push({t: "route", i: i}); });
    if(d.flag && near(d.flag)) hits.push({t: "flag", i: 0});
    (d.cps || []).forEach(function(p, i){ if(near(p)) hits.push({t: "cp", i: i}); });
    (d.spawns || []).forEach(function(p, i){ if(near(p)) hits.push({t: "spawn", i: i}); });
    (d.coins || []).forEach(function(p, i){ if(near(p)) hits.push({t: "coin", i: i}); });
    var L = compiled();
    if(L) for(var i = L.solids.length - 1; i >= 0; i--) if(PLv.inFoot(L.solids[i], x, z, 0)) hits.push({t: "solid", i: i});
    return hits;
  }
  function itemPos(it){
    var d = D();
    if(!it) return null;
    if(it.t === "solid"){ var b = d.solids[it.i]; return b ? [+b.x, +b.y + solidH(b), +b.z] : null; }
    if(it.t === "flag") return d.flag;
    var arr = {coin: d.coins, cp: d.cps, spawn: d.spawns, route: d.route}[it.t];
    return arr && arr[it.i] ? arr[it.i] : null;
  }
  function label(it){
    if(!it) return "";
    var d = D();
    if(it.t === "solid"){ var b = d.solids[it.i]; return b ? (modelName(b.m)+" "+(it.i + 1)) : "platform"; }
    if(it.t === "flag") return "the flag";
    if(it.t === "route"){ var p = d.route[it.i]; return "route point "+(it.i + 1)+" ("+kindName(p && p[3])+")"; }
    return {coin: "coin ", cp: "checkpoint ", spawn: "spawn "}[it.t]+(it.i + 1);
  }
  function modelName(m){ for(var i = 0; i < MODEL_UI.length; i++) if(MODEL_UI[i][0] === m) return MODEL_UI[i][1]; return m; }
  function kindName(k){ return k === "j" ? "jump" : k === "d" ? "double jump" : "walk"; }
  function fmtP(p){ return p ? "x "+(+p[0]).toFixed(1)+", height "+(+p[1]).toFixed(1)+", z "+(+p[2]).toFixed(1) : ""; }
  function standY(x, z, extra){ var t = topAt(x, z); return r2((t === null ? S.layer : t) + (extra || 0)); }

  /* ---------- edits ---------- */
  function place(x, z){
    var d = D(), t = S.tool, msg = "";
    x = snapv(x); z = snapv(z);
    if(t === "select"){
      var hit = itemsAt(x, z)[0] || null; S.sel = hit; changed(hit ? "Selected "+label(hit)+" at "+fmtP(itemPos(hit)) : "Nothing here"); return;
    }
    if(t === "erase"){ var h = itemsAt(x, z)[0]; if(!h){ say("Nothing to erase here"); return; } removeItem(h); return; }
    remember();
    if(t === "solid"){
      if(d.solids.length >= K.MAX_SOLIDS){ S.undo.pop(); api.toast("64 platforms at most"); return; }
      var b = {m: S.model, x: x, y: r2(S.layer - (PLv.MODELS[S.model][3]*S.scale)), z: z}; if(S.scale !== 1) b.s = S.scale;
      d.solids.push(b); S.sel = {t: "solid", i: d.solids.length - 1}; msg = "Placed "+modelName(S.model)+", top at "+S.layer.toFixed(1)+" m";
    } else if(t === "coin"){
      if(d.coins.length >= K.MAX_COINS){ S.undo.pop(); api.toast("80 coins at most"); return; }
      d.coins.push([x, standY(x, z, 1.2), z]); S.sel = {t: "coin", i: d.coins.length - 1}; msg = "Placed coin "+d.coins.length;
    } else if(t === "cp"){
      if(d.cps.length >= K.MAX_CPS){ S.undo.pop(); api.toast("8 checkpoints at most"); return; }
      d.cps.push([x, standY(x, z), z]); S.sel = {t: "cp", i: d.cps.length - 1}; msg = "Placed checkpoint "+d.cps.length;
    } else if(t === "spawn"){
      if(d.spawns.length >= K.MAX_PLAYERS){ S.undo.pop(); api.toast("8 spawns at most"); return; }
      d.spawns.push([x, standY(x, z), z]); S.sel = {t: "spawn", i: d.spawns.length - 1}; msg = "Placed spawn "+d.spawns.length;
    } else if(t === "flag"){
      d.flag = [x, standY(x, z), z]; S.sel = {t: "flag", i: 0}; msg = "Moved the flag";
    } else if(t === "route"){
      if(d.route.length >= K.MAX_ROUTE){ S.undo.pop(); api.toast("200 route points at most"); return; }
      var at = S.sel && S.sel.t === "route" ? S.sel.i + 1 : d.route.length;
      d.route.splice(at, 0, [x, standY(x, z), z, S.kind]); S.sel = {t: "route", i: at}; msg = "Route point "+(at + 1)+" ("+kindName(S.kind)+")";
    }
    changed(msg+" at "+fmtP(itemPos(S.sel)));
  }
  function removeItem(it){
    var d = D(); if(!it) return;
    if(it.t === "flag"){ say("The flag stays: move it instead"); return; }
    if(it.t === "solid" && d.solids.length <= 1){ say("A level needs at least one platform"); return; }
    if(it.t === "spawn" && d.spawns.length <= 1){ say("A level needs at least one spawn"); return; }
    remember();
    var arr = {solid: d.solids, coin: d.coins, cp: d.cps, spawn: d.spawns, route: d.route}[it.t];
    var name = label(it);
    arr.splice(it.i, 1);
    if(it.t === "coin") d.coopGoal = Math.min(d.coopGoal|0, d.coins.length);
    S.sel = null; changed("Removed "+name);
  }
  function moveSel(dx, dy, dz){
    var it = S.sel, d = D(); if(!it) return;
    remember();
    if(it.t === "solid"){ var b = d.solids[it.i]; b.x = r2(+b.x + dx); b.z = r2(+b.z + dz); b.y = r2(+b.y + dy); }
    else {
      var p = itemPos(it); if(!p) return;
      var nx = r2(+p[0] + dx), nz = r2(+p[2] + dz), ny;
      if(dy) ny = r2(+p[1] + dy);
      else { var tp = topAt(nx, nz, +p[1] + 1.5); ny = it.t === "coin" ? r2(+p[1]) : tp === null ? r2(+p[1]) : r2(tp); }
      p[0] = nx; p[1] = ny; p[2] = nz;
    }
    changed(label(it)+" at "+fmtP(itemPos(it)));
  }
  function setLayer(v){ S.layer = Math.max(-20, Math.min(40, r2(v))); renderOpts(); say("Height "+S.layer.toFixed(1)+" m"); }

  /* ---------- auto-route: a greedy search over the platforms ---------- */
  function autoRoute(){
    var d = D(), L = compiled(); if(!L){ api.toast("Add a platform first"); return; }
    var n = L.solids.length;
    function under(p){
      var best = -1; L.solids.forEach(function(s, i){ if(PLv.inFoot(s, p[0], p[2], 0) && Math.abs(s.y1 - p[1]) <= 0.15 && (best < 0 || s.y1 > L.solids[best].y1)) best = i; });
      return best;
    }
    function inner(s, p, m){
      if(s.r > 0){
        var vx = p[0] - s.cx, vz = p[2] - s.cz, l = Math.sqrt(vx*vx + vz*vz), R = Math.max(0, s.r - m);
        if(l <= R) return [p[0], s.y1, p[2]];
        return [s.cx + vx/l*R, s.y1, s.cz + vz/l*R];
      }
      function cl(v, lo, hi){ return lo > hi ? (lo + hi)/2 : Math.max(lo, Math.min(hi, v)); }
      return [cl(p[0], s.x0 + m, s.x1 - m), s.y1, cl(p[2], s.z0 + m, s.z1 - m)];
    }
    function rnd(p){ return [r2(p[0]), r2(p[1]), r2(p[2])]; }
    // from solid A to solid B: a take-off on A, a landing on B and the kind of leg, or null
    var memo = {};
    function hop(a, b){
      var key = a+">"+b; if(key in memo) return memo[key];
      var A = L.solids[a], B = L.solids[b];
      var tk = rnd(inner(A, [B.cx, 0, B.cz], 0.4)), ld = rnd(inner(B, tk, 0.4)), res = null;
      if(Math.abs(A.y1 - B.y1) <= K.ROUTE_STEP_UP && !RU.legProblem(L, tk, ld, "w")) res = {tk: tk, ld: ld, k: "w"};
      else if(!RU.legProblem(L, tk, ld, "j")) res = {tk: tk, ld: ld, k: "j"};
      else if(!RU.legProblem(L, tk, ld, "d")) res = {tk: tk, ld: ld, k: "d"};
      memo[key] = res; return res;
    }
    function path(a, b){
      if(a === b) return [a];
      var prev = {}, q = [a], seen = {}; seen[a] = 1;
      while(q.length){
        var c = q.shift();
        for(var j = 0; j < n; j++){
          if(seen[j] || !hop(c, j)) continue;
          seen[j] = 1; prev[j] = c; if(j === b){ var out = [b]; while(out[0] !== a) out.unshift(prev[out[0]]); return out; }
          q.push(j);
        }
      }
      return null;
    }
    var goals = (d.cps || []).concat([d.flag]), pos = d.spawns[0], route = [];
    var cur = under(pos);
    if(cur < 0){ api.toast("Spawn 1 isn't on a platform"); say("Auto-route needs spawn 1 on a platform"); return; }
    function walkTo(p){ var last = route.length ? route[route.length - 1] : pos; if(Math.abs(last[0] - p[0]) + Math.abs(last[2] - p[2]) > 0.05 || !route.length) route.push([p[0], p[1], p[2], "w"]); }
    for(var g = 0; g < goals.length; g++){
      var T = goals[g], tgt = under(T);
      if(tgt < 0){ api.toast((g < d.cps.length ? "Checkpoint "+(g + 1) : "The flag")+" isn't on a platform"); return; }
      var ph = path(cur, tgt);
      if(!ph){ var what = g < d.cps.length ? "checkpoint "+(g + 1) : "the flag"; api.toast("No way found to "+what+": add a platform in between"); say("Auto-route found no way to "+what); return; }
      for(var h = 1; h < ph.length; h++){
        var H = hop(ph[h - 1], ph[h]);
        walkTo(H.tk);
        route.push([H.ld[0], H.ld[1], H.ld[2], H.k]);
      }
      walkTo(rnd(T)); cur = tgt;
    }
    if(route.length < 2) route.unshift([r2(pos[0]), r2(pos[1]), r2(pos[2]), "w"]);
    if(route.length > K.MAX_ROUTE){ api.toast("The route needs more than 200 points"); return; }
    remember(); d.route = route; S.sel = null; changed("Auto-route: "+route.length+" points");
  }

  /* ---------- the check ---------- */
  function recheck(){
    S.chk = RU.check(D());
    var nm = RU.cleanName(S.doc.name);
    chkBox.textContent = "";
    var errs = S.chk.errors.slice(); if(nm.error) errs.unshift(nm.error);
    var ok = !errs.length, head = api.mk("b", null, ok ? "✓ Ready: the Arena will take this level and a runner can finish it" : "✗ "+errs.length+" thing"+(errs.length > 1 ? "s" : "")+" to fix");
    chkBox.appendChild(head);
    if(!ok){ var ul = api.mk("ul"); errs.slice(0, 8).forEach(function(e){ ul.appendChild(api.mk("li", null, e.charAt(0).toUpperCase()+e.slice(1))); }); chkBox.appendChild(ul); }
    var bytes = S.chk.canon ? JSON.stringify(S.chk.canon).length : 0;
    chkBox.appendChild(api.mk("span", "vg-muted", (bytes ? (bytes/1024).toFixed(1)+" KiB · " : "")+D().solids.length+" platforms · "+(D().coins || []).length+" coins · "+(D().cps || []).length+" checkpoints · "+(D().route || []).length+" route points"));
    S.okAll = ok;
    renderActs();
    return ok;
  }
  function changed(msg){ recheck(); renderInsp(); draw(); renderDrafts(); if(msg){ status.textContent = msg; say(msg); } }

  /* ---------- panels ---------- */
  function renderThemes(){
    themeRow.textContent = "";
    themeRow.appendChild(api.mk("span", "vg-muted", "Theme"));
    var th = D().theme || {};
    THEMES.forEach(function(t){
      var on = th.sky === t[1].sky && th.sea === t[1].sea;
      var b = api.btn(t[0], on ? "primary" : "", function(){ remember(); D().theme = clone(t[1]); renderThemes(); changed("Theme "+t[0]); });
      b.setAttribute("role", "radio"); b.setAttribute("aria-checked", on ? "true" : "false"); themeRow.appendChild(b);
    });
  }
  function renderTools(){
    toolRow.textContent = "";
    TOOLS.forEach(function(t){
      var b = api.btn(t[2]+" "+t[1], S.tool === t[0] ? "primary" : "", function(){ setTool(t[0]); topC.focus(); });
      b.setAttribute("aria-pressed", S.tool === t[0] ? "true" : "false"); toolRow.appendChild(b);
    });
  }
  function setTool(t){ S.tool = t; renderTools(); renderOpts(); var n = ""; TOOLS.forEach(function(x){ if(x[0] === t) n = x[1]; }); say("Tool: "+n); }
  function renderOpts(){
    optRow.textContent = "";
    if(S.tool === "solid"){
      var sel = api.mk("select", "vg-select"); sel.setAttribute("aria-label", "Platform model");
      MODEL_UI.forEach(function(m){ var o = api.mk("option", null, m[1]); o.value = m[0]; if(m[0] === S.model) o.selected = true; sel.appendChild(o); });
      sel.addEventListener("change", function(){ S.model = sel.value; draw(); });
      optRow.appendChild(sel);
      var sc = api.mk("select", "vg-select"); sc.setAttribute("aria-label", "Platform scale");
      [1, 1.5, 2].forEach(function(v){ var o = api.mk("option", null, "×"+v); o.value = String(v); if(v === S.scale) o.selected = true; sc.appendChild(o); });
      sc.addEventListener("change", function(){ S.scale = +sc.value || 1; });
      optRow.appendChild(sc);
    }
    if(S.tool === "route"){
      var kr = api.mk("span", "vg-row"); kr.setAttribute("role", "radiogroup"); kr.setAttribute("aria-label", "Route point kind");
      KINDS.forEach(function(k){
        var b = api.btn(k[1], S.kind === k[0] ? "primary" : "", function(){ S.kind = k[0]; renderOpts(); });
        b.setAttribute("role", "radio"); b.setAttribute("aria-checked", S.kind === k[0] ? "true" : "false"); kr.appendChild(b);
      });
      optRow.appendChild(kr);
    }
    optRow.appendChild(api.btn("Auto-route", S.tool === "route" ? "primary" : "", function(){ autoRoute(); }));
    optRow.appendChild(api.btn("Clear route", "ghost", function(){ remember(); D().route = []; S.sel = null; changed("Route cleared: add points or press Auto-route"); }));
    var hl = api.mk("span", "vg-row");
    hl.appendChild(api.mk("span", "vg-muted", "New platform top"));
    hl.appendChild(api.btn("−", "", function(){ setLayer(S.layer - SNAP); }));
    hl.appendChild(api.mk("b", null, S.layer.toFixed(1)+" m"));
    hl.appendChild(api.btn("+", "", function(){ setLayer(S.layer + SNAP); }));
    optRow.appendChild(hl);
  }
  function numField(lbl, val, step, fn){
    var l = api.mk("label", "vg-golf-check"), i = api.mk("input", "vg-input"); i.type = "number"; i.step = String(step); i.value = String(val);
    i.style.width = "5.5em";
    i.addEventListener("change", function(){ var v = parseFloat(i.value); if(isFinite(v)) fn(v); });
    l.appendChild(document.createTextNode(lbl+" ")); l.appendChild(i); return l;
  }
  function renderInsp(){
    insp.textContent = "";
    var it = S.sel, d = D();
    if(!it || !itemPos(it)){ insp.appendChild(api.mk("span", "vg-muted", "Nothing selected. Use 1 Select, then click or press Enter on an item.")); return; }
    insp.appendChild(api.mk("b", null, label(it).charAt(0).toUpperCase()+label(it).slice(1)));
    var row = api.mk("div", "vg-row");
    if(it.t === "solid"){
      var b = d.solids[it.i];
      row.appendChild(numField("x", b.x, 0.5, function(v){ remember(); b.x = r2(v); changed(label(it)+" moved"); }));
      row.appendChild(numField("top", r2(+b.y + solidH(b)), 0.5, function(v){ remember(); b.y = r2(v - solidH(b)); changed(label(it)+" top at "+r2(v)+" m"); }));
      row.appendChild(numField("z", b.z, 0.5, function(v){ remember(); b.z = r2(v); changed(label(it)+" moved"); }));
      var ms = api.mk("select", "vg-select"); ms.setAttribute("aria-label", "Model");
      MODEL_UI.forEach(function(m){ var o = api.mk("option", null, m[1]); o.value = m[0]; if(m[0] === b.m) o.selected = true; ms.appendChild(o); });
      ms.addEventListener("change", function(){ remember(); var top = +b.y + solidH(b); b.m = ms.value; b.y = r2(top - solidH(b)); changed("Now a "+modelName(b.m)); });
      row.appendChild(ms);
    } else {
      var p = itemPos(it);
      row.appendChild(numField("x", p[0], 0.5, function(v){ remember(); p[0] = r2(v); changed(label(it)+" moved"); }));
      row.appendChild(numField("height", p[1], 0.1, function(v){ remember(); p[1] = r2(v); changed(label(it)+" at "+fmtP(p)); }));
      row.appendChild(numField("z", p[2], 0.5, function(v){ remember(); p[2] = r2(v); changed(label(it)+" moved"); }));
      if(it.t === "route"){
        KINDS.forEach(function(k){
          var kb = api.btn(k[1], d.route[it.i][3] === k[0] ? "primary" : "", function(){ remember(); d.route[it.i][3] = k[0]; changed(label(it)); });
          kb.setAttribute("aria-pressed", d.route[it.i][3] === k[0] ? "true" : "false"); row.appendChild(kb);
        });
      }
      if(it.t === "cp" && d.cps.length > 1){
        row.appendChild(api.btn("Earlier", "", function(){ if(it.i < 1) return; remember(); var t = d.cps[it.i]; d.cps[it.i] = d.cps[it.i - 1]; d.cps[it.i - 1] = t; S.sel = {t: "cp", i: it.i - 1}; changed("Now "+label(S.sel)); }));
        row.appendChild(api.btn("Later", "", function(){ if(it.i >= d.cps.length - 1) return; remember(); var t = d.cps[it.i]; d.cps[it.i] = d.cps[it.i + 1]; d.cps[it.i + 1] = t; S.sel = {t: "cp", i: it.i + 1}; changed("Now "+label(S.sel)); }));
      }
      if(it.t !== "coin" && it.t !== "solid") row.appendChild(api.btn("Snap onto the platform", "ghost", function(){ var tp = topAt(p[0], p[2], +p[1] + 1.5); if(tp === null){ say("No platform under it"); return; } remember(); p[1] = r2(tp); changed(label(it)+" on the platform"); }));
    }
    if(it.t !== "flag") row.appendChild(api.btn("Delete", "ghost", function(){ removeItem(it); }));
    insp.appendChild(row);
  }
  function renderSettings(){
    settings.textContent = "";
    var d = D();
    settings.appendChild(numField("Co-op goal (coins)", d.coopGoal|0, 1, function(v){ remember(); d.coopGoal = Math.max(0, Math.min(d.coins.length, Math.round(v))); renderSettings(); changed("Co-op goal "+d.coopGoal); }));
    settings.appendChild(numField("Co-op time (s)", +d.coopSecs || 120, 10, function(v){ remember(); d.coopSecs = Math.max(K.COOP_SECS_MIN, Math.min(K.COOP_SECS_MAX, Math.round(v))); renderSettings(); changed("Co-op time "+d.coopSecs+" s"); }));
    settings.appendChild(api.btn("Kill height from the platforms", "ghost", function(){
      var L = compiled(); if(!L) return; var low = 1e9; L.solids.forEach(function(s){ low = Math.min(low, s.y0); });
      remember(); d.kill = Math.max(-K.LIM, Math.floor(low) - 6); renderSettings(); changed("Fall-out height "+d.kill+" m");
    }));
    settings.appendChild(api.mk("span", "vg-muted", "Fall-out height "+(+d.kill)+" m"));
  }
  function renderActs(){
    acts.textContent = "";
    acts.appendChild(api.btn("Undo", "", undo));
    acts.appendChild(api.btn("Redo", "", redo));
    acts.appendChild(api.btn("Preview in 3D", "", function(){ play({preview: true}); }));
    acts.appendChild(api.btn("Test-run", "primary", function(){ play({}); }));
    acts.appendChild(api.btn("Watch the route", "", function(){ play({auto: true}); }));
    var mode = api.mk("select", "vg-select"); mode.setAttribute("aria-label", "Room mode");
    [["race", "Race"], ["coop", "Co-op"]].forEach(function(m){ var o = api.mk("option", null, m[1]); o.value = m[0]; if(S.mode === m[0]) o.selected = true; mode.appendChild(o); });
    mode.addEventListener("change", function(){ S.mode = mode.value; });
    var race = api.btn("Race it in a room", S.okAll ? "primary" : "", function(){ raceInRoom(); });
    if(!mapsOn()) race.title = "Needs an Arena that takes custom levels";
    acts.appendChild(mode); acts.appendChild(race);
    pubBtn.classList.toggle("hidden", typeof window.workshopPublish !== "function");
    delBtn.classList.toggle("hidden", S.idx < 0);
  }
  function renderDrafts(){
    var list = wsave();
    draftsSel.textContent = "";
    var o0 = api.mk("option", null, "My levels ("+list.length+"/"+MAX_DRAFTS+")…"); o0.value = ""; draftsSel.appendChild(o0);
    list.forEach(function(d, i){ var o = api.mk("option", null, (i === S.idx ? "● " : "")+String(d.name || "Untitled").slice(0, 32)); o.value = "d"+i; draftsSel.appendChild(o); });
    var a = api.mk("option", null, "New: three hops (starter)"); a.value = "starter"; draftsSel.appendChild(a);
    var b = api.mk("option", null, "New: empty island"); b.value = "blank"; draftsSel.appendChild(b);
  }
  function load(doc, idx, msg){
    S.doc = clone(doc); S.idx = idx; S.undo = []; S.redo = []; S.sel = null; S.dirty = false; nameIn.value = S.doc.name || "";
    var d = S.doc.data = S.doc.data || {};
    ["spawns", "cps", "coins", "solids", "route", "deco"].forEach(function(k){ if(!Array.isArray(d[k])) d[k] = []; });
    if(!Array.isArray(d.flag)) d.flag = [0, 0, 0];
    if(!d.theme || typeof d.theme !== "object") d.theme = clone(THEMES[0][1]);
    var L = compiled();
    if(L){ var x0 = 1e9, x1 = -1e9, z0 = 1e9, z1 = -1e9; L.solids.forEach(function(s){ x0 = Math.min(x0, s.x0); x1 = Math.max(x1, s.x1); z0 = Math.min(z0, s.z0); z1 = Math.max(z1, s.z1); });
      S.cam.x = (x0 + x1)/2; S.cam.z = (z0 + z1)/2; }
    renderThemes(); renderSettings(); changed(msg || "Opened "+(S.doc.name || "a level"));
  }
  function openChoice(v){
    if(!v) return;
    if(S.dirty && !window.confirm("Leave this level without saving?")) return;
    if(v === "starter") load(starter(), -1, "New level from the starter");
    else if(v === "blank") load(blank(), -1, "New empty level");
    else { var i = parseInt(v.slice(1), 10), list = wsave(); if(list[i]) load(list[i], i); }
  }
  function saveDraft(){
    var nm = RU.cleanName(S.doc.name);
    if(nm.error){ api.toast("Name: "+nm.error); say(nm.error); nameIn.focus(); return false; }
    var doc = docOf(nm.name, D()), size = JSON.stringify(doc).length;
    if(size >= DRAFT_MAX){ api.toast("Too big to keep as a draft ("+(size/1024).toFixed(1)+" KiB; 8 KiB at most)"); say("Too big to save"); return false; }
    var list = wsave();
    if(S.idx >= 0 && S.idx < list.length) list[S.idx] = doc;
    else {
      if(list.length >= MAX_DRAFTS){ api.toast("6 levels at most: delete one first"); say("6 levels at most"); return false; }
      list.push(doc); S.idx = list.length - 1;
    }
    api.persist(); S.dirty = false; storyNote("make-save");
    renderDrafts(); renderActs(); api.toast("💾 Saved “"+doc.name+"”"); say("Saved "+doc.name);
    return true;
  }
  function deleteDraft(){
    var list = wsave(); if(S.idx < 0 || !list[S.idx]) return;
    if(!window.confirm("Delete “"+list[S.idx].name+"” from your levels?")) return;
    list.splice(S.idx, 1); api.persist(); S.idx = -1; S.dirty = true; renderDrafts(); renderActs(); say("Draft deleted");
  }
  function readyDoc(){
    var nm = RU.cleanName(S.doc.name);
    if(nm.error){ api.toast("Name: "+nm.error); return null; }
    if(!recheck()){ api.toast("Fix the check first: "+(S.chk.errors[0] || "")); say("Fix the check first"); return null; }
    return docOf(nm.name, D());
  }
  function publish(){
    if(typeof window.workshopPublish !== "function") return;
    var doc = readyDoc(); if(!doc) return;
    var scope = window.confirm("Publish “"+doc.name+"” to the public gallery?\nOK: public · Cancel: keep it private to you") ? "public" : "private";
    Promise.resolve(window.workshopPublish(doc, scope)).then(function(r){
      if(r && r.map){ api.toast("📤 Published “"+doc.name+"”"); say("Published"); }
      else api.toast("Couldn't publish: "+String((r && r.error) || "the Arena said no").slice(0, 100));
    }, function(){ api.toast("Couldn't publish right now"); });
  }
  function raceInRoom(){
    var doc = readyDoc(); if(!doc) return;
    if(!mapsOn()){ api.toast("This Arena doesn't take custom levels yet (or you aren't paired)"); say("This Arena doesn't take custom levels yet"); return; }
    if(S.dirty && S.idx >= 0) saveDraft();
    HQV.makers.plat.play(doc, {room: true, mode: S.mode});
  }

  /* ---------- test-run and 3D preview in the platformer's own renderer ---------- */
  function play(o){
    if(typeof HQV.platPlay !== "function"){ api.toast("Platformer Rush isn't loaded"); return; }
    var c = RU.check(D());
    if(!c.canon){ api.toast("Fix the check first: "+(c.errors[0] || "")); return; }
    if(!o.preview && !c.ok) api.toast("The check isn't green yet: this run may not be finishable");
    stopPlay();
    edit.classList.add("hidden"); playBox.classList.remove("hidden"); playBox.textContent = "";
    S.game = HQV.platPlay(playBox, {custom: {name: (S.doc.name || "Test").trim() || "Test", data: c.canon}, preview: !!o.preview,
      onExit: function(){ stopPlay(); edit.classList.remove("hidden"); playBox.classList.add("hidden"); topC.focus(); say("Back in the editor"); }});
    if(o.auto && S.game && S.game.autopilot){
      var g = S.game, tries = 0, iv = setInterval(function(){
        tries++;
        if(!g.alive || tries > 100){ clearInterval(iv); return; }
        var P = g.me && g.me();
        if(P && g.level && g.phase === "run"){ clearInterval(iv); g.autopilot(true); say("Watching the autopilot run the route"); }
      }, 100);
    }
    say(o.preview ? "3D preview: Q and E turn the camera" : "Test-run: W A S D to run, Space to jump");
  }
  function stopPlay(){ if(S.game){ try { S.game.destroy(); } catch(e){} S.game = null; } }

  /* ---------- drawing ---------- */
  var TOK = tokens(), tokAt = 0;
  function w2s(x, z){ var W = topC.width, H = topC.height, k = S.cam.s*dpr(); return [W/2 + (x - S.cam.x)*k, H/2 + (z - S.cam.z)*k]; }
  function s2w(px, py){ var W = topC.width, H = topC.height, k = S.cam.s*dpr(); return [S.cam.x + (px - W/2)/k, S.cam.z + (py - H/2)/k]; }
  function dpr(){ return Math.min(window.devicePixelRatio || 1, 2); }
  function size(){
    var w = Math.max(280, edit.clientWidth || host.clientWidth || 640), h = Math.round(Math.min(w*0.56, (window.innerHeight || 800)*0.55));
    topC.style.width = "100%"; topC.style.height = h+"px"; sideC.style.width = "100%"; sideC.style.height = Math.round(h*0.38)+"px";
    var r = dpr(); topC.width = Math.round(w*r); topC.height = Math.round(h*r); sideC.width = Math.round(w*r); sideC.height = Math.round(h*0.38*r);
  }
  function legOf(i){ return S.chk && S.chk.legs ? S.chk.legs[i] : null; }
  function draw(){
    if(!S.alive) return;
    var now = Date.now(); if(now - tokAt > 2000){ TOK = tokens(); tokAt = now; }
    drawTop(); drawSide();
  }
  function drawTop(){
    var g = topC.getContext("2d"), W = topC.width, H = topC.height, T = TOK, d = D(), k = S.cam.s*dpr(), L = compiled();
    g.setTransform(1, 0, 0, 1, 0, 0); g.fillStyle = T.bg2; g.fillRect(0, 0, W, H);
    // grid: 1 m lines, 5 m stronger
    var a = s2w(0, 0), b = s2w(W, H);
    g.lineWidth = 1;
    for(var gx = Math.floor(a[0]); gx <= b[0]; gx++){ var sx = w2s(gx, 0)[0]; g.strokeStyle = T.line; g.globalAlpha = gx % 5 === 0 ? 0.55 : 0.18; g.beginPath(); g.moveTo(sx, 0); g.lineTo(sx, H); g.stroke(); }
    for(var gz = Math.floor(a[1]); gz <= b[1]; gz++){ var sz = w2s(0, gz)[1]; g.strokeStyle = T.line; g.globalAlpha = gz % 5 === 0 ? 0.55 : 0.18; g.beginPath(); g.moveTo(0, sz); g.lineTo(W, sz); g.stroke(); }
    g.globalAlpha = 1;
    if(L){
      var lo = 1e9, hi = -1e9; L.solids.forEach(function(s){ lo = Math.min(lo, s.y1); hi = Math.max(hi, s.y1); });
      L.solids.map(function(s, i){ return [s, i]; }).sort(function(p, q){ return p[0].y1 - q[0].y1; }).forEach(function(pr){
        var s = pr[0], i = pr[1], c = w2s(s.cx, s.cz);
        g.beginPath();
        if(s.r > 0) g.arc(c[0], c[1], s.r*k, 0, Math.PI*2);
        else { var p0 = w2s(s.x0, s.z0); g.rect(p0[0], p0[1], (s.x1 - s.x0)*k, (s.z1 - s.z0)*k); }
        g.fillStyle = T.panel; g.fill();
        g.globalAlpha = 0.15 + 0.45*(s.y1 - lo)/((hi - lo) || 1); g.fillStyle = s.m === "brick" || s.m === "block-coin" ? T.gold : T.brand; g.fill(); g.globalAlpha = 1;
        var sel = S.sel && S.sel.t === "solid" && S.sel.i === i;
        g.lineWidth = sel ? 3*dpr() : 1*dpr(); g.strokeStyle = sel ? T.ink : T.muted; g.stroke();
        g.fillStyle = T.ink; g.font = Math.round(11*dpr())+"px "+T.mono; g.textAlign = "center"; g.textBaseline = "middle";
        g.fillText(s.y1.toFixed(1), c[0], c[1]);
      });
    }
    // route: walks solid, jumps dashed; a leg with a problem in the warning colour
    var R = d.route || [];
    function routeFrom(i){ return i === 0 ? d.spawns[0] : R[i - 1]; }
    R.forEach(function(p, i){
      var a0 = routeFrom(i); if(!a0) return;
      var bad = legOf(i) || (i === 0 && legOf(-1)), s0 = w2s(+a0[0], +a0[2]), s1 = w2s(+p[0], +p[2]);
      g.setLineDash(p[3] === "w" ? [] : p[3] === "j" ? [6*dpr(), 4*dpr()] : [2*dpr(), 3*dpr()]);
      g.lineWidth = 2.5*dpr(); g.strokeStyle = bad ? T.need : T.good; g.beginPath(); g.moveTo(s0[0], s0[1]); g.lineTo(s1[0], s1[1]); g.stroke();
      g.setLineDash([]);
      var sel = S.sel && S.sel.t === "route" && S.sel.i === i;
      g.beginPath(); g.arc(s1[0], s1[1], (sel ? 5 : 3.5)*dpr(), 0, Math.PI*2); g.fillStyle = bad ? T.need : T.good; g.fill();
      if(sel){ g.lineWidth = 2*dpr(); g.strokeStyle = T.ink; g.stroke(); }
    });
    // coins, checkpoints (numbered), spawns, the flag
    (d.coins || []).forEach(function(c, i){ var s = w2s(+c[0], +c[2]), sel = S.sel && S.sel.t === "coin" && S.sel.i === i;
      g.beginPath(); g.arc(s[0], s[1], 0.25*k, 0, Math.PI*2); g.fillStyle = T.gold; g.fill(); if(sel){ g.lineWidth = 2*dpr(); g.strokeStyle = T.ink; g.stroke(); } });
    (d.cps || []).forEach(function(c, i){ var s = w2s(+c[0], +c[2]), sel = S.sel && S.sel.t === "cp" && S.sel.i === i, hit = S.chk && i < S.chk.cpHit;
      g.beginPath(); g.arc(s[0], s[1], K.CP_R*k, 0, Math.PI*2); g.lineWidth = (sel ? 3 : 1.5)*dpr(); g.strokeStyle = hit ? T.good : T.need; g.globalAlpha = 0.8; g.stroke(); g.globalAlpha = 1;
      g.fillStyle = T.ink; g.font = "700 "+Math.round(12*dpr())+"px "+T.mono; g.textAlign = "center"; g.textBaseline = "middle"; g.fillText("CP"+(i + 1), s[0], s[1] - 0.6*k); });
    (d.spawns || []).forEach(function(c, i){ var s = w2s(+c[0], +c[2]), sel = S.sel && S.sel.t === "spawn" && S.sel.i === i, r = 0.35*k;
      g.beginPath(); g.moveTo(s[0], s[1] - r); g.lineTo(s[0] + r, s[1] + r); g.lineTo(s[0] - r, s[1] + r); g.closePath(); g.fillStyle = T.brand; g.fill();
      g.lineWidth = (sel ? 3 : 1)*dpr(); g.strokeStyle = T.ink; g.stroke();
      g.fillStyle = T.ink; g.font = Math.round(10*dpr())+"px "+T.mono; g.fillText(String(i + 1), s[0], s[1] + r + 8*dpr()); });
    if(d.flag){ var f = w2s(+d.flag[0], +d.flag[2]), fs = S.sel && S.sel.t === "flag";
      g.fillStyle = T.need; g.fillRect(f[0] - 1.5*dpr(), f[1] - 0.9*k, 3*dpr(), 0.9*k);
      g.beginPath(); g.moveTo(f[0] + 1.5*dpr(), f[1] - 0.9*k); g.lineTo(f[0] + 0.7*k, f[1] - 0.65*k); g.lineTo(f[0] + 1.5*dpr(), f[1] - 0.4*k); g.fill();
      if(fs){ g.lineWidth = 2*dpr(); g.strokeStyle = T.ink; g.strokeRect(f[0] - 0.5*k, f[1] - k, k, 1.1*k); } }
    // the keyboard cursor and a ghost of the platform to place
    var cs = w2s(S.cur[0], S.cur[1]);
    if(S.tool === "solid" && PLv.MODELS[S.model]){
      var M = PLv.MODELS[S.model], hw = M[1]*S.scale/2, hd = M[2]*S.scale/2;
      g.globalAlpha = 0.35; g.fillStyle = T.brand; g.beginPath();
      if(M[0] === "round") g.arc(cs[0], cs[1], hw*k, 0, Math.PI*2); else g.rect(cs[0] - hw*k, cs[1] - hd*k, 2*hw*k, 2*hd*k);
      g.fill(); g.globalAlpha = 1;
    }
    g.lineWidth = 2*dpr(); g.strokeStyle = T.ink; g.setLineDash([3*dpr(), 3*dpr()]);
    g.strokeRect(cs[0] - 0.25*k, cs[1] - 0.25*k, 0.5*k, 0.5*k); g.setLineDash([]);
    g.fillStyle = T.ink; g.font = "600 "+Math.round(12*dpr())+"px "+T.mono; g.textAlign = "left"; g.textBaseline = "alphabetic";
    g.fillText("x "+S.cur[0].toFixed(1)+"  z "+S.cur[1].toFixed(1)+"  · new top "+S.layer.toFixed(1)+" m · north ↑", 8*dpr(), H - 8*dpr());
  }
  // the side profile: the long axis of the level across, height up; jump arcs as they fly
  function drawSide(){
    var g = sideC.getContext("2d"), W = sideC.width, H = sideC.height, T = TOK, d = D(), L = compiled();
    g.setTransform(1, 0, 0, 1, 0, 0); g.fillStyle = T.bg2; g.fillRect(0, 0, W, H);
    if(!L) return;
    var ax = S.side === "z" ? 2 : 0, lo = 1e9, hi = -1e9, y0 = 1e9, y1 = -1e9;
    L.solids.forEach(function(s){ var a0 = ax === 2 ? s.z0 : s.x0, a1 = ax === 2 ? s.z1 : s.x1; lo = Math.min(lo, a0); hi = Math.max(hi, a1); y0 = Math.min(y0, s.y0); y1 = Math.max(y1, s.y1); });
    y1 += 4; y0 -= 1;
    var k = Math.min((W - 20)/((hi - lo) || 1), (H - 16)/((y1 - y0) || 1));
    function sp(a, y){ return [10 + (a - lo)*k, H - 8 - (y - y0)*k]; }
    g.strokeStyle = T.line; g.globalAlpha = 0.4; g.beginPath(); var zline = sp(lo, 0); g.moveTo(0, zline[1]); g.lineTo(W, zline[1]); g.stroke(); g.globalAlpha = 1;
    L.solids.forEach(function(s, i){
      var a0 = ax === 2 ? s.z0 : s.x0, a1 = ax === 2 ? s.z1 : s.x1, p0 = sp(a0, s.y1), p1 = sp(a1, s.y0);
      g.fillStyle = T.panel; g.fillRect(p0[0], p0[1], p1[0] - p0[0], p1[1] - p0[1]);
      g.globalAlpha = 0.4; g.fillStyle = T.brand; g.fillRect(p0[0], p0[1], p1[0] - p0[0], p1[1] - p0[1]); g.globalAlpha = 1;
      g.lineWidth = (S.sel && S.sel.t === "solid" && S.sel.i === i ? 2.5 : 1)*dpr(); g.strokeStyle = T.muted; g.strokeRect(p0[0], p0[1], p1[0] - p0[0], p1[1] - p0[1]);
    });
    var R = d.route || [];
    R.forEach(function(p, i){
      var a0 = i === 0 ? d.spawns[0] : R[i - 1]; if(!a0) return;
      var pts = RU.leg([+a0[0], +a0[1], +a0[2]], [+p[0], +p[1], +p[2]], p[3]), bad = legOf(i);
      g.beginPath(); pts.forEach(function(q, j){ var s = sp(q[ax], q[1] + K.CENTER); if(j) g.lineTo(s[0], s[1]); else g.moveTo(s[0], s[1]); });
      g.lineWidth = 2*dpr(); g.strokeStyle = bad ? T.need : T.good; g.stroke();
    });
    g.fillStyle = T.gold; (d.coins || []).forEach(function(c){ var s = sp(+c[ax], +c[1]); g.beginPath(); g.arc(s[0], s[1], 3*dpr(), 0, Math.PI*2); g.fill(); });
    g.fillStyle = T.need; if(d.flag){ var f = sp(+d.flag[ax], +d.flag[1]); g.fillRect(f[0] - 1, f[1] - 14*dpr(), 2*dpr(), 14*dpr()); }
    g.strokeStyle = T.ink; (d.cps || []).forEach(function(c){ var s = sp(+c[ax], +c[1]); g.lineWidth = 1.5*dpr(); g.beginPath(); g.arc(s[0], s[1] - 6*dpr(), 6*dpr(), 0, Math.PI*2); g.stroke(); });
    g.fillStyle = T.ink; g.font = Math.round(11*dpr())+"px "+T.mono; g.textAlign = "left"; g.textBaseline = "top";
    g.fillText("Side view along "+(ax === 2 ? "z (north–south)" : "x (west–east)")+" · height up", 6*dpr(), 4*dpr());
  }
  function renderSideRow(){
    sideRow.textContent = "";
    var b = api.btn(S.side === "z" ? "Side view: along z" : "Side view: along x", "ghost", function(){ S.side = S.side === "z" ? "x" : "z"; renderSideRow(); drawSide(); });
    sideRow.appendChild(b);
  }

  /* ---------- input: mouse, touch (pointer) and keyboard ---------- */
  function evPos(e){ var r = topC.getBoundingClientRect(), k = topC.width/(r.width || 1); return s2w((e.clientX - r.left)*k, (e.clientY - r.top)*k); }
  topC.addEventListener("pointerdown", function(e){
    topC.focus();
    var w = evPos(e);
    if(e.button === 2 || e.button === 1){ S.drag = {pan: true, x: e.clientX, y: e.clientY, cx: S.cam.x, cz: S.cam.z}; e.preventDefault(); return; }
    S.cur = [snapv(w[0]), snapv(w[1])];
    if(S.tool === "select"){
      var hit = itemsAt(S.cur[0], S.cur[1])[0] || itemsAt(w[0], w[1])[0] || null;
      S.sel = hit;
      if(hit){ var p = itemPos(hit); S.drag = {it: hit, from: [S.cur[0], S.cur[1]], start: snap(), moved: false, base: p ? [+p[0], +p[2]] : [0, 0]}; }
      changed(hit ? "Selected "+label(hit)+": drag to move" : "Nothing here");
      try { topC.setPointerCapture(e.pointerId); } catch(err){}
      return;
    }
    place(w[0], w[1]);
  });
  topC.addEventListener("pointermove", function(e){
    var Dg = S.drag; if(!Dg) return;
    // canvas pixels are CSS pixels × dpr, and w2s draws S.cam.s canvas pixels per metre per dpr: S.cam.s CSS px a metre
    if(Dg.pan){ S.cam.x = Dg.cx - (e.clientX - Dg.x)/S.cam.s; S.cam.z = Dg.cz - (e.clientY - Dg.y)/S.cam.s; draw(); return; }
    var w = evPos(e), nx = snapv(w[0]), nz = snapv(w[1]), dx = nx - Dg.from[0], dz = nz - Dg.from[1];
    if(!dx && !dz) return;
    if(!Dg.moved){ S.undo.push(Dg.start); if(S.undo.length > UNDO_MAX) S.undo.shift(); S.redo.length = 0; Dg.moved = true; S.dirty = true; }
    Dg.from = [nx, nz]; S.cur = [nx, nz];
    var it = Dg.it, d = D();
    if(it.t === "solid"){ var b = d.solids[it.i]; b.x = r2(+b.x + dx); b.z = r2(+b.z + dz); }
    else { var p = itemPos(it); if(p){ p[0] = r2(+p[0] + dx); p[2] = r2(+p[2] + dz); if(it.t !== "coin"){ var tp = topAt(p[0], p[2], +p[1] + 1.5); if(tp !== null) p[1] = r2(tp); } } }
    recheck(); draw();
  });
  function endDrag(){ var Dg = S.drag; S.drag = null; if(Dg && Dg.moved){ changed(label(Dg.it)+" moved to "+fmtP(itemPos(Dg.it))); } }
  topC.addEventListener("pointerup", endDrag);
  topC.addEventListener("pointercancel", endDrag);
  topC.addEventListener("contextmenu", function(e){ e.preventDefault(); });
  topC.addEventListener("wheel", function(e){ e.preventDefault(); zoom(e.deltaY < 0 ? 1.15 : 1/1.15); }, {passive: false});
  function zoom(f){ S.cam.s = Math.max(6, Math.min(60, S.cam.s*f)); draw(); }
  function follow(){
    var r = topC.getBoundingClientRect(), wv = (r.width || 400)/S.cam.s/2 - 1.5, hv = (r.height || 300)/S.cam.s/2 - 1.5;
    if(S.cur[0] < S.cam.x - wv) S.cam.x = S.cur[0] + wv; if(S.cur[0] > S.cam.x + wv) S.cam.x = S.cur[0] - wv;
    if(S.cur[1] < S.cam.z - hv) S.cam.z = S.cur[1] + hv; if(S.cur[1] > S.cam.z + hv) S.cam.z = S.cur[1] - hv;
  }
  function cursorSay(){
    var hit = itemsAt(S.cur[0], S.cur[1])[0], t = topAt(S.cur[0], S.cur[1]);
    status.textContent = "Cursor x "+S.cur[0].toFixed(1)+", z "+S.cur[1].toFixed(1)+(t !== null ? " · platform top "+t.toFixed(1)+" m" : " · open air")+(hit ? " · "+label(hit) : "");
  }
  var cursorSayT = 0;
  topC.addEventListener("keydown", function(e){
    if(e.altKey) return;
    var key = e.key, mod = e.ctrlKey || e.metaKey, handled = true, step = e.shiftKey ? 2 : SNAP;
    if(mod && (key === "z" || key === "Z")){ if(e.shiftKey) redo(); else undo(); }
    else if(mod && (key === "y" || key === "Y")) redo();
    else if(mod) handled = false;
    else if(key === "ArrowLeft" || key === "ArrowRight" || key === "ArrowUp" || key === "ArrowDown"){
      var dx = key === "ArrowLeft" ? -1 : key === "ArrowRight" ? 1 : 0, dz = key === "ArrowUp" ? -1 : key === "ArrowDown" ? 1 : 0;
      if(e.shiftKey && S.sel) moveSel(dx*SNAP, 0, dz*SNAP);
      else { S.cur = [r2(S.cur[0] + dx*step), r2(S.cur[1] + dz*step)]; follow(); draw();
        clearTimeout(cursorSayT); cursorSayT = setTimeout(function(){ if(S.alive) cursorSay(); }, 150); }
    }
    else if(key === "Enter" || key === " ") place(S.cur[0], S.cur[1]);
    else if(key === "Delete" || key === "Backspace"){ var h = S.sel || itemsAt(S.cur[0], S.cur[1])[0]; if(h) removeItem(h); else say("Nothing to remove here"); }
    else if(key === "PageUp" || key === "]"){ if(S.sel && S.tool === "select") moveSel(0, SNAP, 0); else setLayer(S.layer + SNAP); }
    else if(key === "PageDown" || key === "["){ if(S.sel && S.tool === "select") moveSel(0, -SNAP, 0); else setLayer(S.layer - SNAP); }
    else if(/^[1-8]$/.test(key)) setTool(TOOLS[+key - 1][0]);
    else if(key === "+" || key === "=") zoom(1.2);
    else if(key === "-" || key === "_") zoom(1/1.2);
    else if(key === "Escape"){ if(S.sel){ S.sel = null; changed("Selection cleared"); } else handled = false; }
    else if(S.tool === "route" && (key === "w" || key === "j" || key === "d")){ S.kind = key; renderOpts(); say("Next route point: "+kindName(key)); }
    else if(key === "a" && !e.shiftKey) autoRoute();
    // the page's view keys (0-9, H, J, R, W, C) would switch views mid-edit: this grid owns them
    else if(/^[0-9hjrwcHJRWC]$/.test(key)) handled = true;
    else handled = false;
    if(handled){ e.preventDefault(); e.stopPropagation(); }
  });

  /* ---------- lifecycle ---------- */
  var ro = null;
  function onResize(){ if(!S.alive) return; size(); draw(); }
  window.addEventListener("resize", onResize);
  if(typeof ResizeObserver !== "undefined"){ ro = new ResizeObserver(onResize); ro.observe(host); }
  S.destroy = function(){
    S.alive = false; stopPlay();
    window.removeEventListener("resize", onResize); if(ro) ro.disconnect();
    root.remove();
  };
  S.pause = function(){ if(S.game) S.game.paused = true; };
  S.resume = function(){ if(S.game) S.game.paused = false; };
  S.load = load; S.play = play; S.autoRoute = autoRoute; S.place = place; S.check = function(){ return S.chk; };
  S.saveDraft = saveDraft; S.raceInRoom = raceInRoom;

  renderTools(); renderOpts(); renderSideRow(); size();
  load(S.doc, S.idx, first && first.msg);
  S.dirty = !!(first && first.dirty);
  if(first && first.run) setTimeout(function(){ if(S.alive) play({}); }, 0);
  return S;
}

/* ---------- registration: the Valley card and the Workshop maker ---------- */
HQV.register({id: "make-plat", name: "Level Editor", icon: "🧱", workshop: true,
  desc: "Build a Platformer Rush level, check it, test-run it and race it",
  mount: function(el){
    if(CUR) CUR.destroy();
    var o = OPEN_WITH; OPEN_WITH = null;
    CUR = makeEditor(el, o);
  },
  unmount: function(){ if(CUR){ CUR.destroy(); CUR = null; } },
  pause: function(){ if(CUR) CUR.pause(); },
  resume: function(){ if(CUR) CUR.resume(); }});

function toValley(){ if(typeof window.setView === "function" && window.VIEW !== "valley") window.setView("valley"); }
function okDoc(doc){ return doc && typeof doc === "object" && doc.kind === "plat" && doc.data && typeof doc.data === "object"; }
HQV.makers = HQV.makers || {};
HQV.makers.plat = {game: "make-plat", name: "Level Editor", icon: "🧱",
  // open a level in the editor (a remix starts as a new draft)
  edit: function(doc){
    if(!okDoc(doc)) return;
    toValley();
    OPEN_WITH = {doc: {kind: "plat", v: 1, name: String(doc.name || "Remix").slice(0, K.NAME_MAX), data: clone(doc.data)}, idx: -1, msg: "Opened “"+String(doc.name || "a level")+"”", dirty: true};
    api.open("make-plat");
  },
  // solo: a test-run; {room: true}: open Platformer Rush with friends and host the custom start
  play: function(doc, opts){
    if(!okDoc(doc)) return;
    opts = opts || {};
    var d = {kind: "plat", v: 1, name: String(doc.name || "Custom level").trim().slice(0, K.NAME_MAX), data: clone(doc.data)};
    toValley();
    if(opts.room){
      HQV.platPending = {doc: d, mode: opts.mode === "coop" ? "coop" : "race"};
      api.toast("🏝️ Opening Platformer Rush: you start “"+d.name+"” as the host");
      api.open("mp-plat");
      return;
    }
    OPEN_WITH = {doc: d, idx: -1, run: true, msg: "Test-running “"+d.name+"”", dirty: true};
    api.open("make-plat");
  },
  drafts: function(){ return wsave().map(clone); }};
HQV.levelRules = RU;            // the live check, for smoke tests
HQV.leveleditDebug = function(){ return CUR; };
})();
