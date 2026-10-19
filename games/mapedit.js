/* Valley: Map Editor (HQ 2.5 "Make + compete"). Build a Blaster Arena map on a top-down
 * grid: walls, low cover, blocks, steps, crates and floors stacked in height layers,
 * 8-16 spawns with a facing, and health/ammo pickups. Every change is checked live with
 * the same geometry the Arena referee uses (HQV.fpsShared, the FPS-SHARED block of
 * games/fps.js): a spawn inside a wall or standing on nothing gets a red flag, spawn
 * pairs that can see each other are drawn (amber when closer than 12 m), and the spawn
 * count, the bounds and the size limits are checked as backend/src/fps.rs
 * validate_custom checks them.
 *
 * Walk a map alone (fps.js: a walk-through, no drones, no clock), save up to 6 drafts in
 * the Valley save (api.save.workshop.fps), publish to the Workshop gallery when it is
 * there (window.workshopPublish), or host a match on it in an Arena room. What leaves
 * the machine (only when you play in a room or publish) is the map itself:
 * {kind:"fps", v:1, name, data:{bounds, theme, boxes, spawns, pickups}}.
 */
(function(){
"use strict";
var HQV = window.HQV; if(!HQV || !HQV.api) return;
var api = HQV.api, E = HQV.engine || null;

/* ---------- the rules, as backend/src/fps.rs checks them ---------- */
var MAX_BOXES = 96, MIN_SPAWNS = 8, MAX_SPAWNS = 16, MAX_PICKUPS = 16, COORD_MAX = 200;
var MAX_DATA = 12*1024, DRAFT_MAX = 8*1024, DRAFTS = 6, NAME_MAX = 32, LOS_WARN = 12, SNAP = 0.5;
var TOP = 12, CANVAS = 640;
var KINDS = [{id: "wall", name: "Wall", h: 3}, {id: "low", name: "Low cover", h: 1.1}, {id: "block", name: "Block", h: 1.5},
             {id: "step", name: "Step", h: 0.5}, {id: "crate", name: "Crate", h: 1.2}, {id: "floor", name: "Floor", h: 0.5}];
var KIND_IDS = KINDS.map(function(k){ return k.id; });
var TOOLS = [{id: "select", name: "Select", key: "1"}, {id: "box", name: "Box", key: "2"}, {id: "spawn", name: "Spawn", key: "3"},
             {id: "health", name: "Health", key: "4"}, {id: "ammo", name: "Ammo", key: "5"}, {id: "erase", name: "Erase", key: "6"}];
var SIZES = [24, 32, 40, 48];
// Theme presets: colours for the 3D sky, fog and ground (data, not CSS).
var THEMES = [{id: "day", name: "Sunny day", sky: "#9fd3f0", fog: "#cfe8f2", ground: "#6fb35a"},
              {id: "dusk", name: "Dusk", sky: "#f2a65a", fog: "#f6d1a8", ground: "#7a8f4a"},
              {id: "night", name: "Night", sky: "#1d2a4a", fog: "#2b3a5e", ground: "#3a5a3a"},
              {id: "desert", name: "Desert", sky: "#f4d9a0", fog: "#f7e7c4", ground: "#d8b36a"},
              {id: "snow", name: "Snowfield", sky: "#cfe3f2", fog: "#eef5fb", ground: "#e8eef2"},
              {id: "ember", name: "Ember", sky: "#3a1f1f", fog: "#5a2a20", ground: "#4a3a30"}];
var LAYERS = []; for(var li = 0; li <= 20; li++) LAYERS.push(li/2);
var HEIGHTS = []; for(var hi = 1; hi <= 16; hi++) HEIGHTS.push(hi/2);

function FSH(){ return HQV.fpsShared || null; }
function sayIt(t){ if(E && E.say) E.say(t); else if(typeof window.announce === "function"){ try { window.announce(t); } catch(e){} } }
function r2(n){ var r = Math.round(n*100)/100; return r === 0 ? 0 : r; }
function snap(n){ var r = Math.round(n/SNAP)*SNAP; return r === 0 ? 0 : r; }
function yawOf(n){ return ((Math.round(+n || 0) % 360) + 360) % 360; }
function clone(o){ return JSON.parse(JSON.stringify(o)); }
function isHex(s){ return typeof s === "string" && /^#[0-9a-fA-F]{6}$/.test(s); }
function storyNote(ev){ try { if(HQV.story && typeof HQV.story.note === "function") HQV.story.note(ev); } catch(e){} }
function mapsOn(){ var A = window.ARENA; return !!(A && A.arena && A.arena.maps); }
function fmtM(n){ return (Math.round(n*10)/10).toFixed(1)+" m"; }
function toast(t){ api.toast(t); }

/* ---------- the MapDoc ---------- */
// Canonical data (backend validate_custom): keys in order, numbers to 0.01, yaw a whole
// degree in 0..359, hex lowercased, pickups named p1..pN in order.
function canon(d){
  d = d || {};
  var th = d.theme || {};
  return {
    bounds: (d.bounds || []).map(function(n){ return r2(+n); }),
    theme: {sky: String(th.sky || "").toLowerCase(), fog: String(th.fog || "").toLowerCase(), ground: String(th.ground || "").toLowerCase()},
    boxes: (d.boxes || []).map(function(b){ return [r2(+b[0]), r2(+b[1]), r2(+b[2]), r2(+b[3]), r2(+b[4]), r2(+b[5]), String(b[6])]; }),
    spawns: (d.spawns || []).map(function(s){ return [r2(+s[0]), r2(+s[1]), r2(+s[2]), yawOf(s[3])]; }),
    pickups: (d.pickups || []).map(function(p, i){ return {id: "p"+(i + 1), kind: p.kind === "health" ? "health" : "ammo", at: (p.at || []).slice(0, 3).map(function(n){ return r2(+n); })}; })
  };
}
function docOf(name, data){ return {kind: "fps", v: 1, name: String(name || "").trim().slice(0, NAME_MAX), data: canon(data)}; }
// A stored or handed-over doc, cleaned up enough to edit (the checks report the rest).
function adopt(doc){
  if(!doc || typeof doc !== "object" || doc.kind !== "fps" || !doc.data || typeof doc.data !== "object") return null;
  var d = doc.data;
  if(!Array.isArray(d.bounds) || d.bounds.length !== 6 || !Array.isArray(d.boxes) || !Array.isArray(d.spawns)) return null;
  var ok = function(a, n){ return Array.isArray(a) && a.length >= n && a.slice(0, n).every(function(v){ return typeof v === "number" && isFinite(v); }); };
  if(!ok(d.bounds, 6)) return null;
  var data = {bounds: d.bounds.slice(0, 6), theme: d.theme && typeof d.theme === "object" ? d.theme : {},
              boxes: d.boxes.filter(function(b){ return ok(b, 6) && KIND_IDS.indexOf(b[6]) >= 0; }),
              spawns: d.spawns.filter(function(s){ return ok(s, 4); }),
              pickups: (Array.isArray(d.pickups) ? d.pickups : []).filter(function(p){ return p && ok(p.at, 3); })};
  if(!isHex(data.theme.sky) || !isHex(data.theme.fog) || !isHex(data.theme.ground)) data.theme = {sky: THEMES[0].sky, fog: THEMES[0].fog, ground: THEMES[0].ground};
  return docOf(typeof doc.name === "string" && doc.name.trim() ? doc.name : "Untitled map", data);
}
function nameErr(s){
  var t = String(s || "").trim();
  if(!t.length) return "Give the map a name.";
  if(t.length > NAME_MAX) return "The name can be at most "+NAME_MAX+" characters.";
  if(/[<>]/.test(t) || /http/i.test(t) || /[\u0000-\u001f\u007f-\u009f]/.test(t)) return "The name can't use < >, links or control characters.";
  return "";
}

/* ---------- starter maps ---------- */
function facing(x, z){ return yawOf(Math.atan2(-x, z)*180/Math.PI); }
function openYard(size){
  var h = size/2, w = 0.5, ring = h - 3, c = h - 4.5;
  var boxes = [[-h, -1, -h, h, 0, h, "floor"],
               [-h, 0, -h, h, 4, -h + w, "wall"], [-h, 0, h - w, h, 4, h, "wall"],
               [-h, 0, -h + w, -h + w, 4, h - w, "wall"], [h - w, 0, -h + w, h, 4, h - w, "wall"],
               [-2, 0, -2, 2, 1.5, 2, "block"], [2, 0, -1, 3, 0.5, 1, "step"], [-3, 0, -1, -2, 0.5, 1, "step"],
               [-6, 0, -c, -2, 1.1, -c + 0.6, "low"], [2, 0, c - 0.6, 6, 1.1, c, "low"],
               [c - 2, 0, -c + 1, c - 0.8, 1.2, -c + 2.2, "crate"], [-c + 0.8, 0, c - 2.2, -c + 2, 1.2, c - 1, "crate"],
               [-c + 1, 0, -2, -c + 2, 3, 2, "wall"], [c - 2, 0, -2, c - 1, 3, 2, "wall"]];
  // screens between each side spawn and the corner spawns next to it
  [-1, 1].forEach(function(sx){ [-1, 1].forEach(function(sz){
    var m = c/2, a = ring - 3, e = h - w;
    boxes.push(sz > 0 ? [sx*m - 0.5, 0, a, sx*m + 0.5, 3, e, "wall"] : [sx*m - 0.5, 0, -e, sx*m + 0.5, 3, -a, "wall"]);
    boxes.push(sx > 0 ? [a, 0, sz*m - 0.5, e, 3, sz*m + 0.5, "wall"] : [-e, 0, sz*m - 0.5, -a, 3, sz*m + 0.5, "wall"]);
  }); });
  var pts = [[0, ring], [0, -ring], [ring, 0], [-ring, 0], [c, c], [-c, -c], [c, -c - 1], [-c, c + 1]];
  return {bounds: [-h, -1, -h, h, TOP, h], theme: {sky: THEMES[0].sky, fog: THEMES[0].fog, ground: THEMES[0].ground}, boxes: boxes,
          spawns: pts.map(function(p){ return [p[0], 0, p[1], facing(p[0], p[1])]; }),
          pickups: [{kind: "health", at: [0, 1.5, 0]}, {kind: "ammo", at: [-c + 3, 0, 0]}, {kind: "ammo", at: [c - 3, 0, 0]}]};
}
// The built-in courtyard as a starting point: its edge walls move just inside the bounds.
function fromCourtyard(j){
  var bd = (j.bounds || [-22, -1, -22, 22, 12, 22]).slice(0, 6);
  var boxes = (j.boxes || []).map(function(b){
    var o = b.slice(0, 6).map(Number), k = String(b[6] || "block");
    for(var a = 0; a < 3; a++){
      o[a] = Math.max(bd[a], Math.min(bd[a + 3], o[a])); o[a + 3] = Math.max(bd[a], Math.min(bd[a + 3], o[a + 3]));
      if(o[a] >= o[a + 3]){ if(o[a + 3] >= bd[a + 3]) o[a] = o[a + 3] - SNAP; else o[a + 3] = o[a] + SNAP; }
    }
    o.push(KIND_IDS.indexOf(k) >= 0 ? k : "block"); return o;
  });
  return {bounds: bd, theme: j.theme || {}, boxes: boxes, spawns: (j.spawns || []).slice(0, MAX_SPAWNS),
          pickups: (j.pickups || []).slice(0, MAX_PICKUPS).map(function(p){ return {kind: p.kind, at: p.at}; })};
}

/* ---------- the live checks ---------- */
function inside(b, x, y, z){ return b[0] <= x && x <= b[3] && b[1] <= y && y <= b[4] && b[2] <= z && z <= b[5]; }
// -> {errors:[{msg, sel}], warns:[...], spawnBad:{i:why}, boxBad:{}, pickBad:{}, pairs:[{i,j,d,warn}], bytes}
function check(doc){
  var FS = FSH(), d = doc.data, out = {errors: [], warns: [], spawnBad: {}, boxBad: {}, pickBad: {}, pairs: [], bytes: 0};
  var bd = d.bounds, m = {boxes: d.boxes};
  function err(msg, sel){ out.errors.push({msg: msg, sel: sel || null}); }
  var ne = nameErr(doc.name); if(ne) err(ne);
  if(!(bd[0] < bd[3] && bd[1] < bd[4] && bd[2] < bd[5])) err("The arena's bounds have no volume.");
  if(!d.boxes.length) err("A map needs at least one box.");
  if(d.boxes.length > MAX_BOXES) err("Too many boxes: "+d.boxes.length+" of "+MAX_BOXES+".");
  d.boxes.forEach(function(b, i){
    var why = "";
    if(!(b[0] < b[3] && b[1] < b[4] && b[2] < b[5])) why = "has no volume";
    else if(!(inside(bd, b[0], b[1], b[2]) && inside(bd, b[3], b[4], b[5]))) why = "is outside the arena";
    else if(b.slice(0, 6).some(function(n){ return !isFinite(n) || Math.abs(n) > COORD_MAX; })) why = "is out of range";
    if(why){ out.boxBad[i] = why; err("Box "+(i + 1)+" ("+b[6]+") "+why+".", {t: "box", i: i}); }
  });
  var n = d.spawns.length;
  if(n < MIN_SPAWNS || n > MAX_SPAWNS || n % 5 === 0)
    err("A map needs "+MIN_SPAWNS+"-"+MAX_SPAWNS+" spawns, but not 10 or 15 (this one has "+n+").");
  d.spawns.forEach(function(s, i){
    var why = "";
    if(!inside(bd, s[0], s[1], s[2]) || (FS && s[1] + FS.H > bd[4])) why = "is outside the arena";
    else if(FS && FS.overlaps(m, s[0], s[1], s[2], 0) >= 0) why = "is inside a wall";
    else if(FS){ var t = FS.topUnder(m, s[0], s[1] - 0.05, s[2]); if(t === null || Math.abs(t - s[1]) > 1e-6) why = "isn't standing on anything"; }
    if(why){ out.spawnBad[i] = why; err("Spawn "+(i + 1)+" "+why+".", {t: "spawn", i: i}); }
  });
  if(d.pickups.length > MAX_PICKUPS) err("Too many pickups: "+d.pickups.length+" of "+MAX_PICKUPS+".");
  d.pickups.forEach(function(p, i){
    var why = "";
    if(!inside(bd, p.at[0], p.at[1], p.at[2])) why = "is outside the arena";
    else if(FS && FS.overlaps(m, p.at[0], p.at[1], p.at[2], 0) >= 0) why = "is inside a wall";
    if(why){ out.pickBad[i] = why; err((p.kind === "health" ? "Health" : "Ammo")+" pickup "+(i + 1)+" "+why+".", {t: "pick", i: i}); }
  });
  // Spawn pairs with a clear line of sight eye to eye, through the referee's own ray test.
  if(FS){
    for(var i = 0; i < n; i++) for(var j = i + 1; j < n; j++){
      if(out.spawnBad[i] || out.spawnBad[j]) continue;
      var a = d.spawns[i], b = d.spawns[j], o = [a[0], a[1] + FS.EYE, a[2]];
      var dx = b[0] - a[0], dy = b[1] - a[1], dz = b[2] - a[2], dist = Math.sqrt(dx*dx + dy*dy + dz*dz);
      if(dist < 1e-6) continue;
      var dir = [dx/dist, dy/dist, dz/dist];
      if(FS.rayMap(m, o, dir, dist) >= dist - 1e-3){
        var warn = dist < LOS_WARN;
        out.pairs.push({i: i, j: j, d: dist, warn: warn});
        if(warn) out.warns.push({msg: "Spawns "+(i + 1)+" and "+(j + 1)+" can see each other only "+fmtM(dist)+" apart.", sel: {t: "spawn", i: j}});
      }
    }
  }
  out.bytes = JSON.stringify(canon(d)).length;
  if(out.bytes > MAX_DATA) err("The map is too big to send: "+Math.ceil(out.bytes/1024)+" KiB of "+MAX_DATA/1024+".");
  return out;
}

/* ---------- drafts in the Valley save ---------- */
function drafts(){
  var s = api.save; if(!s) return null;
  if(!s.workshop || typeof s.workshop !== "object" || Array.isArray(s.workshop)) s.workshop = {};
  if(!Array.isArray(s.workshop.fps)) s.workshop.fps = [];
  s.workshop.fps = s.workshop.fps.filter(function(x){ return x && typeof x === "object" && x.kind === "fps"; }).slice(0, DRAFTS);
  return s.workshop.fps;
}

/* ---------- the editor ---------- */
var CUR = null, PENDING = null;
function makeEditor(host){
  var FS = FSH();
  var S = {alive: true, doc: null, idx: -1, tool: "select", kind: "wall", layer: 0, height: 3, sel: null, cur: [0, 0],
           corner: null, drag: null, undo: [], redo: [], los: true, chk: null, lastSay: "", saved: "", paused: false};
  var root = api.mk("div", "vg-mapedit");
  root.style.display = "flex"; root.style.flexWrap = "wrap"; root.style.gap = "14px"; root.style.alignItems = "flex-start";
  host.appendChild(root);
  if(!FS){ root.appendChild(api.mk("p", "vg-muted", "The Map Editor needs Blaster Arena, which couldn't load in this browser.")); return {destroy: function(){ S.alive = false; }}; }

  var left = api.mk("div"); left.style.flex = "1 1 420px"; left.style.minWidth = "280px"; left.style.maxWidth = CANVAS+"px";
  var right = api.mk("div"); right.style.flex = "1 1 300px"; right.style.minWidth = "260px"; right.style.display = "grid"; right.style.gap = "10px";
  root.appendChild(left); root.appendChild(right);
  var canvas = api.canvas(CANVAS, CANVAS);
  canvas.style.imageRendering = "auto"; canvas.style.touchAction = "none"; canvas.style.cursor = "crosshair";
  canvas.setAttribute("role", "application");
  canvas.setAttribute("aria-roledescription", "map grid");
  canvas.setAttribute("aria-label", "Map grid, seen from above. Arrow keys move the cursor, Enter uses the tool, keys 1 to 6 pick a tool.");
  left.appendChild(canvas);
  var cursorLine = api.mk("p", "vg-muted", ""); cursorLine.setAttribute("aria-live", "polite"); left.appendChild(cursorLine);
  left.appendChild(api.mk("p", "vg-muted", "Keys: arrows move the cursor (or the selection) · Enter/Space use the tool · 1-6 tools · [ ] turn a spawn · - = lower/raise · Delete removes · Ctrl+Z / Ctrl+Y undo/redo · L sight lines · Esc cancels"));

  var head = api.mk("div", "vg-row"), body = api.mk("div"), checksBox = api.mk("div"), actions = api.mk("div");
  right.appendChild(head); right.appendChild(body); right.appendChild(checksBox); right.appendChild(actions);
  var status = api.mk("p", "vg-msg"); status.setAttribute("role", "status"); status.setAttribute("aria-live", "polite");

  /* ----- model helpers ----- */
  function D(){ return S.doc.data; }
  function bounds(){ return D().bounds; }
  function size(){ var b = bounds(); return Math.max(b[3] - b[0], b[5] - b[2]); }
  function sc(){ return CANVAS/size(); }
  function toPx(x, z){ var b = bounds(), k = sc(); return [(x - b[0])*k, (z - b[2])*k]; }
  function clampXZ(x, z){ var b = bounds(); return [Math.max(b[0], Math.min(b[3], x)), Math.max(b[2], Math.min(b[5], z))]; }
  // The highest box top under a player's footprint at (x, z) that a runner at height y could stand on.
  function ground(x, z, y){
    var best = null, R = FS.R;
    D().boxes.forEach(function(b){
      if(x + R > b[0] && x - R < b[3] && z + R > b[2] && z - R < b[5] && b[4] <= y + FS.STEP_H + 1e-9 && (best === null || b[4] > best)) best = b[4];
    });
    return best === null ? y : best;
  }
  function push(){ S.undo.push(JSON.stringify(D())); if(S.undo.length > 60) S.undo.shift(); S.redo = []; }
  function changed(msg){ S.chk = check(S.doc); draw(); renderBody(); renderChecks(); if(msg) sayIt(msg); }
  function undo(){ if(!S.undo.length) return; S.redo.push(JSON.stringify(D())); S.doc.data = JSON.parse(S.undo.pop()); S.sel = null; changed("Undone"); }
  function redo(){ if(!S.redo.length) return; S.undo.push(JSON.stringify(D())); S.doc.data = JSON.parse(S.redo.pop()); S.sel = null; changed("Redone"); }
  function selItem(){
    if(!S.sel) return null;
    var d = D(), a = S.sel.t === "box" ? d.boxes : S.sel.t === "spawn" ? d.spawns : d.pickups;
    return a[S.sel.i] || null;
  }
  function isBase(b){ var bd = bounds(); return b[6] === "floor" && b[0] <= bd[0] && b[2] <= bd[2] && b[3] >= bd[3] && b[5] >= bd[5]; }
  function hit(x, z){
    var d = D(), best = null, bd2 = 0.6*0.6;
    d.spawns.forEach(function(s, i){ var q = (s[0] - x)*(s[0] - x) + (s[2] - z)*(s[2] - z); if(q < bd2){ bd2 = q; best = {t: "spawn", i: i}; } });
    if(best) return best;
    bd2 = 0.6*0.6;
    d.pickups.forEach(function(p, i){ var q = (p.at[0] - x)*(p.at[0] - x) + (p.at[2] - z)*(p.at[2] - z); if(q < bd2){ bd2 = q; best = {t: "pick", i: i}; } });
    if(best) return best;
    var top = -Infinity, base = null;
    d.boxes.forEach(function(b, i){
      if(x < b[0] || x > b[3] || z < b[2] || z > b[5]) return;
      if(isBase(b)){ if(!base) base = {t: "box", i: i}; return; }
      if(b[4] > top){ top = b[4]; best = {t: "box", i: i}; }
    });
    return best || base;
  }
  function describe(sel){
    if(!sel) return "nothing";
    var it = selItem();
    if(!it) return "nothing";
    if(sel.t === "box") return "box "+(sel.i + 1)+", "+it[6]+", "+fmtM(it[3] - it[0])+" by "+fmtM(it[5] - it[2])+", from "+fmtM(it[1])+" up to "+fmtM(it[4]);
    if(sel.t === "spawn") return "spawn "+(sel.i + 1)+" at "+it[0]+", "+it[2]+", height "+it[1]+", facing "+it[3]+"°"+(S.chk && S.chk.spawnBad[sel.i] ? " — it "+S.chk.spawnBad[sel.i] : "");
    return it.kind+" pickup "+(sel.i + 1)+" at "+it.at[0]+", "+it.at[2];
  }

  /* ----- edits ----- */
  function addBox(x0, z0, x1, z1){
    var d = D();
    if(d.boxes.length >= MAX_BOXES){ toast("A map holds at most "+MAX_BOXES+" boxes."); return; }
    var a = Math.min(x0, x1), b = Math.max(x0, x1), c = Math.min(z0, z1), e = Math.max(z0, z1);
    if(b - a < SNAP){ b = a + SNAP; } if(e - c < SNAP){ e = c + SNAP; }
    var y0 = S.layer, y1 = Math.min(TOP, y0 + S.height);
    if(y1 <= y0){ toast("No room for a box this high."); return; }
    push(); d.boxes.push([r2(a), r2(y0), r2(c), r2(b), r2(y1), r2(e), S.kind]);
    S.sel = {t: "box", i: d.boxes.length - 1};
    changed("Added a "+S.kind+" box, "+fmtM(b - a)+" by "+fmtM(e - c)+", "+fmtM(y1 - y0)+" tall");
  }
  function addSpawn(x, z){
    var d = D();
    if(d.spawns.length >= MAX_SPAWNS){ toast("A map holds at most "+MAX_SPAWNS+" spawns."); return; }
    push(); var y = ground(x, z, S.layer);
    d.spawns.push([x, y, z, facing(x, z)]);
    S.sel = {t: "spawn", i: d.spawns.length - 1};
    changed();
    sayIt("Spawn "+d.spawns.length+" placed"+(S.chk.spawnBad[d.spawns.length - 1] ? ", but it "+S.chk.spawnBad[d.spawns.length - 1] : ""));
  }
  function addPickup(kind, x, z){
    var d = D();
    if(d.pickups.length >= MAX_PICKUPS){ toast("A map holds at most "+MAX_PICKUPS+" pickups."); return; }
    push(); d.pickups.push({id: "p"+(d.pickups.length + 1), kind: kind, at: [x, ground(x, z, S.layer), z]});
    S.sel = {t: "pick", i: d.pickups.length - 1};
    changed((kind === "health" ? "Health" : "Ammo")+" pickup placed");
  }
  function remove(sel){
    if(!sel) return;
    var d = D(), a = sel.t === "box" ? d.boxes : sel.t === "spawn" ? d.spawns : d.pickups;
    if(!a[sel.i]) return;
    var what = describe(sel);
    push(); a.splice(sel.i, 1);
    if(sel.t === "pick") d.pickups.forEach(function(p, i){ p.id = "p"+(i + 1); });
    S.sel = null; changed("Removed "+what);
  }
  // Move the selection by (dx, dz) metres from its original state `from`.
  function moveSel(from, dx, dz){
    var it = selItem(); if(!it) return;
    var bd = bounds();
    if(S.sel.t === "box"){
      var w = from[3] - from[0], dd = from[5] - from[2];
      var x0 = Math.max(bd[0], Math.min(bd[3] - w, from[0] + dx)), z0 = Math.max(bd[2], Math.min(bd[5] - dd, from[2] + dz));
      it[0] = r2(x0); it[3] = r2(x0 + w); it[2] = r2(z0); it[5] = r2(z0 + dd);
    } else if(S.sel.t === "spawn"){
      var p = clampXZ(from[0] + dx, from[2] + dz); it[0] = r2(p[0]); it[2] = r2(p[1]); it[1] = r2(ground(it[0], it[2], from[1]));
    } else {
      var q = clampXZ(from.at[0] + dx, from.at[2] + dz); it.at[0] = r2(q[0]); it.at[2] = r2(q[1]); it.at[1] = r2(ground(it.at[0], it.at[2], from.at[1]));
    }
  }
  function nudge(dx, dz, resize){
    var it = selItem(); if(!it) return;
    push();
    if(resize && S.sel.t === "box"){
      var bd = bounds();
      it[3] = r2(Math.max(it[0] + SNAP, Math.min(bd[3], it[3] + dx))); it[5] = r2(Math.max(it[2] + SNAP, Math.min(bd[5], it[5] + dz)));
    } else moveSel(clone(it), dx, dz);
    changed(); sayIt(describe(S.sel));
  }
  function raise(dy){
    var it = selItem();
    if(!it){ var li = LAYERS.indexOf(S.layer) + (dy > 0 ? 1 : -1); if(li >= 0 && li < LAYERS.length){ S.layer = LAYERS[li]; renderBody(); draw(); sayIt("Layer "+fmtM(S.layer)); } return; }
    push();
    if(S.sel.t === "box"){
      var h = it[4] - it[1], y0 = Math.max(bounds()[1], Math.min(TOP - h, it[1] + dy)); it[1] = r2(y0); it[4] = r2(y0 + h);
    } else if(S.sel.t === "spawn"){ it[1] = r2(Math.max(bounds()[1], Math.min(TOP - FS.H, it[1] + dy))); }
    else { it.at[1] = r2(Math.max(bounds()[1], Math.min(TOP, it.at[1] + dy))); }
    changed(); sayIt(describe(S.sel));
  }
  function turn(deg){
    if(!S.sel || S.sel.t !== "spawn") return;
    var it = selItem(); if(!it) return;
    push(); it[3] = yawOf(it[3] + deg); changed(); sayIt("Spawn "+(S.sel.i + 1)+" faces "+it[3]+"°");
  }
  function useTool(x, z){
    if(S.tool === "select"){ S.sel = hit(x, z); draw(); renderBody(); sayIt("Selected "+describe(S.sel)); }
    else if(S.tool === "box"){
      if(!S.corner){ S.corner = [x, z]; draw(); sayIt("Corner set. Move to the opposite corner and press Enter."); }
      else { var c = S.corner; S.corner = null; addBox(c[0], c[1], x, z); }
    }
    else if(S.tool === "spawn") addSpawn(x, z);
    else if(S.tool === "health" || S.tool === "ammo") addPickup(S.tool, x, z);
    else if(S.tool === "erase"){ var h = hit(x, z); if(h && !(h.t === "box" && isBase(D().boxes[h.i]))) remove(h); else sayIt("Nothing to erase here"); }
  }

  /* ----- drawing ----- */
  function kindColour(T, k){ return k === "wall" ? T.ink : k === "low" ? T.muted : k === "block" ? T.brand : k === "step" ? T.good : k === "crate" ? T.gold : T.panel2; }
  function draw(){
    if(!S.alive) return;
    var T = E && E.tokens ? E.tokens() : {ink: "currentColor", muted: "gray", line: "gray", panel: "canvas", panel2: "canvas", brand: "royalblue", need: "crimson", good: "seagreen", gold: "goldenrod", bg2: "canvas"};
    var g = canvas.getContext("2d"), d = D(), bd = d.bounds, k = sc(), chk = S.chk || check(S.doc);
    g.setTransform(1, 0, 0, 1, 0, 0); g.clearRect(0, 0, CANVAS, CANVAS);
    g.fillStyle = T.bg2; g.fillRect(0, 0, CANVAS, CANVAS);
    // boxes, lowest tops first; anything above the edit layer is a dashed outline
    var order = d.boxes.map(function(b, i){ return i; }).sort(function(a, b){ return d.boxes[a][4] - d.boxes[b][4] || a - b; });
    order.forEach(function(i){
      var b = d.boxes[i], p = toPx(b[0], b[2]), w = (b[3] - b[0])*k, h = (b[5] - b[2])*k, above = b[1] > S.layer + 1e-6;
      if(above){ g.setLineDash([5, 4]); g.strokeStyle = kindColour(T, b[6]); g.lineWidth = 1.5; g.strokeRect(p[0] + 0.5, p[1] + 0.5, w - 1, h - 1); g.setLineDash([]); }
      else {
        g.globalAlpha = isBase(b) ? 1 : Math.max(0.4, Math.min(1, 0.45 + b[4]/6)); g.fillStyle = kindColour(T, b[6]); g.fillRect(p[0], p[1], w, h);
        g.globalAlpha = 1; g.strokeStyle = T.line; g.lineWidth = 1; g.strokeRect(p[0] + 0.5, p[1] + 0.5, w - 1, h - 1);
      }
      if(chk.boxBad[i]){ g.strokeStyle = T.need; g.lineWidth = 3; g.strokeRect(p[0], p[1], w, h); }
      if(!isBase(b) && w > 22 && h > 14){
        g.fillStyle = above ? T.ink : (b[6] === "wall" || b[6] === "block" ? T.bg2 : T.ink);
        g.font = "11px sans-serif"; g.textAlign = "center"; g.textBaseline = "middle";
        g.fillText(String(r2(b[4])), p[0] + w/2, p[1] + h/2);
      }
    });
    // the 1 m grid on top, every 4 m stronger
    g.lineWidth = 1;
    for(var x = Math.ceil(bd[0]); x <= bd[3]; x++){
      var px = (x - bd[0])*k; g.strokeStyle = T.line; g.globalAlpha = x % 4 === 0 ? 0.55 : 0.18;
      g.beginPath(); g.moveTo(px + 0.5, 0); g.lineTo(px + 0.5, (bd[5] - bd[2])*k); g.stroke();
    }
    for(var z = Math.ceil(bd[2]); z <= bd[5]; z++){
      var pz = (z - bd[2])*k; g.strokeStyle = T.line; g.globalAlpha = z % 4 === 0 ? 0.55 : 0.18;
      g.beginPath(); g.moveTo(0, pz + 0.5); g.lineTo((bd[3] - bd[0])*k, pz + 0.5); g.stroke();
    }
    g.globalAlpha = 1;
    // lines of sight between spawns
    if(S.los) chk.pairs.forEach(function(pr){
      var a = d.spawns[pr.i], b = d.spawns[pr.j], pa = toPx(a[0], a[2]), pb = toPx(b[0], b[2]);
      g.strokeStyle = pr.warn ? T.need : T.muted; g.globalAlpha = pr.warn ? 0.9 : 0.35; g.lineWidth = pr.warn ? 2.5 : 1;
      if(pr.warn) g.setLineDash([6, 4]);
      g.beginPath(); g.moveTo(pa[0], pa[1]); g.lineTo(pb[0], pb[1]); g.stroke(); g.setLineDash([]); g.globalAlpha = 1;
    });
    // pickups
    d.pickups.forEach(function(pk, i){
      var p = toPx(pk.at[0], pk.at[2]), r = Math.max(6, 0.45*k);
      g.fillStyle = pk.kind === "health" ? T.good : T.gold; g.beginPath(); g.arc(p[0], p[1], r, 0, Math.PI*2); g.fill();
      g.strokeStyle = chk.pickBad[i] ? T.need : T.ink; g.lineWidth = chk.pickBad[i] ? 3 : 1; g.stroke();
      g.fillStyle = T.bg2; g.font = "bold 10px sans-serif"; g.textAlign = "center"; g.textBaseline = "middle";
      g.fillText(pk.kind === "health" ? "+" : "A", p[0], p[1] + 0.5);
    });
    // spawns: a disc, a facing arrow, the number; a red flag when it is in a wall or floating
    d.spawns.forEach(function(s, i){
      var p = toPx(s[0], s[2]), r = Math.max(7, FS.R*k*1.3), bad = chk.spawnBad[i], y = s[3]*Math.PI/180;
      var fx = Math.sin(y), fz = -Math.cos(y);
      g.strokeStyle = bad ? T.need : T.brand; g.lineWidth = 3;
      g.beginPath(); g.moveTo(p[0], p[1]); g.lineTo(p[0] + fx*r*2.2, p[1] + fz*r*2.2); g.stroke();
      g.fillStyle = bad ? T.need : T.brand; g.beginPath(); g.arc(p[0], p[1], r, 0, Math.PI*2); g.fill();
      g.strokeStyle = T.bg2; g.lineWidth = 1.5; g.stroke();
      g.fillStyle = T.bg2; g.font = "bold 11px sans-serif"; g.textAlign = "center"; g.textBaseline = "middle"; g.fillText(String(i + 1), p[0], p[1] + 0.5);
      if(bad){
        g.strokeStyle = T.need; g.fillStyle = T.need; g.lineWidth = 2;
        g.beginPath(); g.moveTo(p[0] + r*0.6, p[1] - r*0.6); g.lineTo(p[0] + r*0.6, p[1] - r*2.6); g.stroke();
        g.beginPath(); g.moveTo(p[0] + r*0.6, p[1] - r*2.6); g.lineTo(p[0] + r*2, p[1] - r*2.1); g.lineTo(p[0] + r*0.6, p[1] - r*1.6); g.closePath(); g.fill();
      }
    });
    // selection, box preview, keyboard cursor, arena edge
    var it = selItem();
    if(it){
      g.strokeStyle = T.brand; g.lineWidth = 2; g.setLineDash([4, 3]);
      if(S.sel.t === "box"){ var q = toPx(it[0], it[2]); g.strokeRect(q[0] - 2, q[1] - 2, (it[3] - it[0])*k + 4, (it[5] - it[2])*k + 4); }
      else { var at = S.sel.t === "spawn" ? it : it.at, c = toPx(at[0], at[2]); g.beginPath(); g.arc(c[0], c[1], Math.max(12, 0.9*k), 0, Math.PI*2); g.stroke(); }
      g.setLineDash([]);
    }
    var pv = S.drag && S.drag.box ? S.drag.box : S.corner ? [S.corner[0], S.corner[1], S.cur[0], S.cur[1]] : null;
    if(pv){
      var a0 = toPx(Math.min(pv[0], pv[2]), Math.min(pv[1], pv[3])), a1 = toPx(Math.max(pv[0], pv[2]), Math.max(pv[1], pv[3]));
      g.fillStyle = kindColour(T, S.kind); g.globalAlpha = 0.45; g.fillRect(a0[0], a0[1], Math.max(2, a1[0] - a0[0]), Math.max(2, a1[1] - a0[1])); g.globalAlpha = 1;
      g.strokeStyle = T.brand; g.lineWidth = 2; g.strokeRect(a0[0], a0[1], Math.max(2, a1[0] - a0[0]), Math.max(2, a1[1] - a0[1]));
    }
    var cp = toPx(S.cur[0], S.cur[1]);
    g.strokeStyle = T.brand; g.lineWidth = 1.5; g.beginPath();
    g.moveTo(cp[0] - 8, cp[1]); g.lineTo(cp[0] + 8, cp[1]); g.moveTo(cp[0], cp[1] - 8); g.lineTo(cp[0], cp[1] + 8); g.stroke();
    g.strokeStyle = T.ink; g.lineWidth = 2; g.strokeRect(1, 1, (bd[3] - bd[0])*k - 2, (bd[5] - bd[2])*k - 2);
    cursorLine.textContent = "Cursor "+S.cur[0]+", "+S.cur[1]+" · layer "+fmtM(S.layer)+" · tool "+S.tool+(S.corner ? " · corner set" : "");
  }

  /* ----- the side panel ----- */
  function field(label, input){ var l = api.mk("label", "vg-golf-check"); l.appendChild(document.createTextNode(label+" ")); l.appendChild(input); return l; }
  function select(label, opts, val, fn){
    var s = api.mk("select", "vg-select"); s.setAttribute("aria-label", label);
    opts.forEach(function(o){ var op = api.mk("option", null, o[1]); op.value = String(o[0]); if(String(o[0]) === String(val)) op.selected = true; s.appendChild(op); });
    s.addEventListener("change", function(){ fn(s.value); });
    return field(label, s);
  }
  function numIn(label, v, fn){
    var n = api.mk("input", "vg-select"); n.type = "number"; n.step = "0.5"; n.value = String(v); n.style.width = "5.5em"; n.setAttribute("aria-label", label);
    n.addEventListener("change", function(){ var f = parseFloat(n.value); if(!isFinite(f) || Math.abs(f) > COORD_MAX){ n.value = String(v); return; } fn(r2(f)); });
    return field(label, n);
  }
  function renderHead(){
    head.textContent = "";
    var nm = api.mk("input", "vg-select"); nm.type = "text"; nm.maxLength = NAME_MAX; nm.value = S.doc.name; nm.setAttribute("aria-label", "Map name");
    nm.style.minWidth = "12em";
    nm.addEventListener("input", function(){ S.doc.name = nm.value; S.chk = check(S.doc); renderChecks(); });
    head.appendChild(field("Name", nm));
    var th = S.doc.data.theme, cur = "";
    THEMES.forEach(function(t){ if(t.sky === th.sky && t.fog === th.fog && t.ground === th.ground) cur = t.id; });
    var opts = THEMES.map(function(t){ return [t.id, t.name]; }); if(!cur) opts.unshift(["", "Custom colours"]);
    head.appendChild(select("Look", opts, cur, function(v){
      THEMES.forEach(function(t){ if(t.id === v){ push(); D().theme = {sky: t.sky, fog: t.fog, ground: t.ground}; changed("Look: "+t.name); } });
    }));
    var b = bounds(); head.appendChild(api.mk("span", "vg-muted", "Arena "+(b[3] - b[0])+" × "+(b[5] - b[2])+" m"));
  }
  function renderBody(){
    body.textContent = "";
    var tools = api.mk("div", "vg-row vg-tabs"); tools.setAttribute("role", "toolbar"); tools.setAttribute("aria-label", "Tools");
    TOOLS.forEach(function(t){
      var b = api.btn(t.key+" "+t.name, S.tool === t.id ? "on" : "", function(){ setTool(t.id); });
      b.setAttribute("aria-pressed", S.tool === t.id ? "true" : "false"); tools.appendChild(b);
    });
    body.appendChild(tools);
    var kinds = api.mk("div", "vg-row vg-tabs"); kinds.setAttribute("role", "toolbar"); kinds.setAttribute("aria-label", "Box kind");
    KINDS.forEach(function(kd){
      var b = api.btn(kd.name, S.kind === kd.id ? "on" : "", function(){ S.kind = kd.id; S.height = kd.h; if(S.tool !== "box") S.tool = "box"; renderBody(); draw(); sayIt(kd.name+" boxes, "+fmtM(kd.h)+" tall"); });
      b.setAttribute("aria-pressed", S.kind === kd.id ? "true" : "false"); kinds.appendChild(b);
    });
    body.appendChild(kinds);
    var lay = api.mk("div", "vg-row");
    lay.appendChild(select("Layer", LAYERS.map(function(v){ return [v, v+" m"]; }), S.layer, function(v){ S.layer = +v; draw(); }));
    lay.appendChild(select("Box height", HEIGHTS.map(function(v){ return [v, v+" m"]; }), HEIGHTS.indexOf(S.height) >= 0 ? S.height : 3, function(v){ S.height = +v; }));
    var los = api.mk("input"); los.type = "checkbox"; los.checked = S.los;
    los.addEventListener("change", function(){ S.los = los.checked; draw(); });
    lay.appendChild(field("Sight lines", los));
    body.appendChild(lay);
    // the inspector
    var it = selItem(), ins = api.mk("div", "vg-row");
    if(it){
      ins.appendChild(api.mk("b", null, S.sel.t === "box" ? "Box "+(S.sel.i + 1) : S.sel.t === "spawn" ? "Spawn "+(S.sel.i + 1) : "Pickup "+(S.sel.i + 1)));
      var setv = function(fn){ return function(v){ push(); fn(v); changed(describe(S.sel)); }; };
      if(S.sel.t === "box"){
        ins.appendChild(select("Kind", KINDS.map(function(kd){ return [kd.id, kd.name]; }), it[6], setv(function(v){ it[6] = v; })));
        [["West", 0], ["Bottom", 1], ["North", 2], ["East", 3], ["Top", 4], ["South", 5]].forEach(function(f){
          ins.appendChild(numIn(f[0], it[f[1]], setv(function(v){ it[f[1]] = v; })));
        });
      } else if(S.sel.t === "spawn"){
        ins.appendChild(numIn("x", it[0], setv(function(v){ it[0] = v; })));
        ins.appendChild(numIn("z", it[2], setv(function(v){ it[2] = v; })));
        ins.appendChild(numIn("Height", it[1], setv(function(v){ it[1] = v; })));
        ins.appendChild(numIn("Facing °", it[3], setv(function(v){ it[3] = yawOf(v); })));
        ins.appendChild(api.btn("⟲", "", function(){ turn(-45); }));
        ins.appendChild(api.btn("⟳", "", function(){ turn(45); }));
        ins.appendChild(api.btn("Drop to the ground", "", function(){ push(); it[1] = r2(ground(it[0], it[2], it[1])); changed(describe(S.sel)); }));
      } else {
        ins.appendChild(select("Kind", [["health", "Health"], ["ammo", "Ammo"]], it.kind, setv(function(v){ it.kind = v; })));
        ins.appendChild(numIn("x", it.at[0], setv(function(v){ it.at[0] = v; })));
        ins.appendChild(numIn("z", it.at[2], setv(function(v){ it.at[2] = v; })));
        ins.appendChild(numIn("Height", it.at[1], setv(function(v){ it.at[1] = v; })));
      }
      if(!(S.sel.t === "box" && isBase(it))) ins.appendChild(api.btn("Delete", "", function(){ remove(S.sel); }));
    } else ins.appendChild(api.mk("span", "vg-muted", "Pick the Select tool and click something to change it. Drag to move."));
    body.appendChild(ins);
    var d = D();
    body.appendChild(api.mk("p", "vg-muted", d.boxes.length+"/"+MAX_BOXES+" boxes · "+d.spawns.length+" spawns ("+MIN_SPAWNS+"-"+MAX_SPAWNS+", not 10 or 15) · "+d.pickups.length+"/"+MAX_PICKUPS+" pickups"));
  }
  function renderChecks(){
    checksBox.textContent = "";
    var c = S.chk || (S.chk = check(S.doc));
    checksBox.appendChild(api.mk("h4", "vg-golf-h", "Checks"));
    var line = c.errors.length ? c.errors.length+(c.errors.length === 1 ? " problem" : " problems")+" to fix before it can be played in a room"
             : "All checks pass"+(c.warns.length ? " · "+c.warns.length+(c.warns.length === 1 ? " warning" : " warnings") : "");
    status.textContent = line; checksBox.appendChild(status);
    if(line !== S.lastSay){ S.lastSay = line; }
    var ul = api.mk("ul"); ul.style.margin = "0"; ul.style.paddingLeft = "1.2em"; ul.style.maxHeight = "12em"; ul.style.overflow = "auto";
    c.errors.concat(c.warns).forEach(function(e, n){
      var li = api.mk("li"), warn = n >= c.errors.length;
      li.style.color = warn ? "var(--gold)" : "var(--need)";
      if(e.sel){ var b = api.btn((warn ? "⚠ " : "✖ ")+e.msg, "ghost", function(){ S.sel = e.sel; S.tool = "select"; draw(); renderBody(); canvas.focus(); sayIt("Selected "+describe(S.sel)); }); b.style.textAlign = "left"; li.appendChild(b); }
      else li.textContent = (warn ? "⚠ " : "✖ ")+e.msg;
      ul.appendChild(li);
    });
    if(c.errors.length || c.warns.length) checksBox.appendChild(ul);
    checksBox.appendChild(api.mk("p", "vg-muted", Math.ceil(c.bytes/102.4)/10+" KiB of "+MAX_DATA/1024+" KiB"));
    renderActions();
  }
  function renderActions(){
    actions.textContent = "";
    var c = S.chk, ok = !c.errors.length, row = api.mk("div", "vg-row");
    row.appendChild(api.btn("Undo", "", undo)); row.appendChild(api.btn("Redo", "", redo));
    row.appendChild(api.btn(S.idx >= 0 ? "Save" : "Save draft", "primary", function(){ save(false); }));
    if(S.idx >= 0) row.appendChild(api.btn("Save as new", "", function(){ save(true); }));
    actions.appendChild(row);
    var play = api.mk("div", "vg-row");
    var walk = api.btn("Walk it (solo)", "", function(){ walkIt(); });
    var room = api.btn("Play in a room", "primary", function(){ roomIt(); });
    if(!ok){ room.disabled = true; room.title = "Fix the problems first"; }
    if(!mapsOn()){ room.disabled = true; room.title = "Your Arena doesn't take user maps yet"; }
    play.appendChild(walk); play.appendChild(room);
    if(typeof window.workshopPublish === "function"){
      var scope = api.mk("select", "vg-select"); scope.setAttribute("aria-label", "Publish to");
      [["public", "Public gallery"], ["private", "Only me"]].forEach(function(o){ var op = api.mk("option", null, o[1]); op.value = o[0]; scope.appendChild(op); });
      var pub = api.btn("Publish", "", function(){ publish(scope.value, pub); });
      if(!ok){ pub.disabled = true; pub.title = "Fix the problems first"; }
      play.appendChild(scope); play.appendChild(pub);
    }
    actions.appendChild(play);
    if(!mapsOn()) actions.appendChild(api.mk("p", "vg-muted", "Pair with an Arena that takes user maps to play this with friends."));
    // new map + your drafts
    actions.appendChild(api.mk("h4", "vg-golf-h", "New map"));
    var nw = api.mk("div", "vg-row");
    SIZES.forEach(function(sz){ nw.appendChild(api.btn("Open yard "+sz+" m", "", function(){ fresh(openYard(sz), "My arena"); })); });
    nw.appendChild(api.btn("Copy of Sky Courtyard", "", function(){
      fetch("/games/fps/map.json").then(function(r){ if(!r.ok) throw new Error(String(r.status)); return r.json(); })
        .then(function(j){ if(S.alive) fresh(fromCourtyard(j), "Courtyard remix"); }, function(){ toast("Couldn't load the Sky Courtyard."); });
    }));
    actions.appendChild(nw);
    var list = drafts();
    actions.appendChild(api.mk("h4", "vg-golf-h", "Your maps ("+(list ? list.length : 0)+"/"+DRAFTS+")"));
    if(!list) actions.appendChild(api.mk("p", "vg-muted", "Your Valley save is still loading."));
    else if(!list.length) actions.appendChild(api.mk("p", "vg-muted", "No saved maps yet."));
    else list.forEach(function(dr, i){
      var r = api.mk("div", "vg-row");
      r.appendChild(api.mk("span", null, (i === S.idx ? "▸ " : "")+String(dr.name || "Untitled").slice(0, NAME_MAX)));
      r.appendChild(api.btn("Open", "", function(){ var a = adopt(dr); if(!a){ toast("That draft is damaged."); return; } load(a, i); sayIt("Opened "+a.name); }));
      var del = api.btn("Delete", "", function(){
        if(del.dataset.sure !== "1"){ del.dataset.sure = "1"; del.textContent = "Delete for good?"; return; }
        var l = drafts(); if(!l) return; l.splice(i, 1); api.persist();
        if(S.idx === i) S.idx = -1; else if(S.idx > i) S.idx--;
        renderActions(); sayIt("Deleted the draft");
      });
      r.appendChild(del); actions.appendChild(r);
    });
  }
  function setTool(id){ S.tool = id; S.corner = null; renderBody(); draw(); sayIt("Tool: "+id); }

  /* ----- whole-map actions ----- */
  function load(doc, idx){
    S.doc = doc; S.idx = typeof idx === "number" ? idx : -1; S.sel = null; S.corner = null; S.undo = []; S.redo = [];
    var b = bounds(); S.cur = [snap((b[0] + b[3])/2), snap((b[2] + b[5])/2)];
    S.chk = check(S.doc); renderHead(); renderBody(); renderChecks(); draw();
  }
  function fresh(data, name){ load(docOf(name, data), -1); sayIt("New map: "+name); }
  function current(){ return docOf(S.doc.name, D()); }
  function save(asNew){
    var list = drafts();
    if(!list){ toast("Your Valley save hasn't loaded yet."); return false; }
    var ne = nameErr(S.doc.name); if(ne){ toast(ne); return false; }
    var doc = current();
    if(JSON.stringify(doc).length >= DRAFT_MAX){ toast("This map is too big to keep as a draft (8 KiB). Remove some boxes."); return false; }
    if(asNew || S.idx < 0 || S.idx >= list.length){
      if(list.length >= DRAFTS){ toast("You already keep "+DRAFTS+" maps. Delete one first."); return false; }
      list.push(doc); S.idx = list.length - 1;
    } else list[S.idx] = doc;
    api.persist(); storyNote("make-save");
    toast("Saved "+doc.name); sayIt("Saved "+doc.name); renderActions();
    return true;
  }
  function walkIt(){
    var doc = current();
    if(!doc.data.spawns.length || Object.keys(S.chk.spawnBad).length === doc.data.spawns.length){ toast("Place a spawn on the ground first."); return; }
    if(typeof HQV.fpsPlay !== "function"){ toast("Blaster Arena isn't loaded."); return; }
    if(S.idx >= 0) save(false);
    HQV.fpsPlay(doc, false);
  }
  function roomIt(){
    if(S.chk.errors.length){ toast("Fix the problems first: "+S.chk.errors[0].msg); return; }
    if(!mapsOn()){ toast("Your Arena doesn't take user maps yet."); return; }
    if(typeof HQV.fpsPlay !== "function"){ toast("Blaster Arena isn't loaded."); return; }
    if(S.idx >= 0) save(false);
    HQV.fpsPlay(current(), true);
  }
  function publish(scope, b){
    if(S.chk.errors.length){ toast("Fix the problems first."); return; }
    if(typeof window.workshopPublish !== "function") return;
    b.disabled = true;
    var p; try { p = window.workshopPublish(current(), scope); } catch(e){ p = null; }
    Promise.resolve(p).then(function(r){
      b.disabled = false;
      if(r && r.map){ toast("Published "+S.doc.name); sayIt("Published "+S.doc.name); }
      else toast("Couldn't publish: "+String((r && r.error) || "no answer").slice(0, 120));
    }, function(){ b.disabled = false; toast("Couldn't publish."); });
  }

  /* ----- input: pointer (mouse, pen, touch) ----- */
  function world(e){
    var r = canvas.getBoundingClientRect(), b = bounds(), k = sc();
    var x = b[0] + (e.clientX - r.left)*(CANVAS/r.width)/k, z = b[2] + (e.clientY - r.top)*(CANVAS/r.height)/k;
    var c = clampXZ(snap(x), snap(z)); return c;
  }
  function onDown(e){
    if(!S.alive || e.button > 0) return;
    canvas.focus(); var w = world(e); S.cur = w;
    try { canvas.setPointerCapture(e.pointerId); } catch(er){}
    if(S.tool === "select"){
      S.sel = hit(w[0], w[1]); var it = selItem();
      if(it && !(S.sel.t === "box" && isBase(it))) S.drag = {at: w, from: clone(it), undo: JSON.stringify(D()), moved: false};
      renderBody(); draw(); if(S.sel) sayIt("Selected "+describe(S.sel));
    } else if(S.tool === "box"){ S.corner = null; S.drag = {box: [w[0], w[1], w[0], w[1]]}; draw(); }
    else if(S.tool === "spawn"){ var nb = D().spawns.length; addSpawn(w[0], w[1]); S.drag = {aim: D().spawns.length > nb ? nb : -1}; }
    else useTool(w[0], w[1]);
    e.preventDefault();
  }
  function onMove(e){
    if(!S.alive) return;
    var w = world(e);
    if(!S.drag){ if(w[0] !== S.cur[0] || w[1] !== S.cur[1]){ S.cur = w; draw(); } return; }
    S.cur = w;
    if(S.drag.box){ S.drag.box[2] = w[0]; S.drag.box[3] = w[1]; draw(); }
    else if(S.drag.from){
      var dx = w[0] - S.drag.at[0], dz = w[1] - S.drag.at[1];
      if(dx || dz || S.drag.moved){ S.drag.moved = true; moveSel(S.drag.from, dx, dz); S.chk = check(S.doc); draw(); }
    } else if(S.drag.aim >= 0){
      var s = D().spawns[S.drag.aim]; if(!s) return;
      var ax = w[0] - s[0], az = w[1] - s[2];
      if(ax*ax + az*az >= 0.25){ s[3] = yawOf(Math.atan2(ax, -az)*180/Math.PI); draw(); }
    }
  }
  function onUp(){
    if(!S.drag) return;
    var dr = S.drag; S.drag = null;
    if(dr.box){ addBox(dr.box[0], dr.box[1], dr.box[2], dr.box[3]); return; }
    if(dr.from && dr.moved){ S.undo.push(dr.undo); S.redo = []; changed(describe(S.sel)); return; }
    if(dr.aim >= 0){ changed(); }
  }
  canvas.addEventListener("pointerdown", onDown);
  canvas.addEventListener("pointermove", onMove);
  canvas.addEventListener("pointerup", onUp);
  canvas.addEventListener("pointercancel", function(){ S.drag = null; draw(); });

  /* ----- input: keyboard (only while the grid has focus) ----- */
  canvas.addEventListener("keydown", function(e){
    if(!S.alive || e.altKey) return;
    var k = e.key, mod = e.ctrlKey || e.metaKey, done = true;
    if(mod && (k === "z" || k === "Z")){ if(e.shiftKey) redo(); else undo(); }
    else if(mod && (k === "y" || k === "Y")) redo();
    else if(mod) done = false;
    else if(k === "ArrowLeft" || k === "ArrowRight" || k === "ArrowUp" || k === "ArrowDown"){
      var dx = k === "ArrowLeft" ? -SNAP : k === "ArrowRight" ? SNAP : 0, dz = k === "ArrowUp" ? -SNAP : k === "ArrowDown" ? SNAP : 0;
      if(S.sel && S.tool === "select") nudge(dx, dz, e.shiftKey);
      else { S.cur = clampXZ(S.cur[0] + dx, S.cur[1] + dz); draw(); }
    }
    else if(k === "Enter" || k === " ") useTool(S.cur[0], S.cur[1]);
    else if(k === "Delete" || k === "Backspace"){ if(S.sel && !(S.sel.t === "box" && isBase(selItem()))) remove(S.sel); }
    else if(k === "Escape"){ if(S.corner || S.sel){ S.corner = null; S.sel = null; renderBody(); draw(); sayIt("Cancelled"); } else done = false; }
    else if(k >= "1" && k <= "6") setTool(TOOLS[+k - 1].id);
    else if(k === "[") turn(-45);
    else if(k === "]") turn(45);
    else if(k === "-" || k === "_") raise(-SNAP);
    else if(k === "=" || k === "+") raise(SNAP);
    else if(k === "l" || k === "L"){ S.los = !S.los; renderBody(); draw(); sayIt(S.los ? "Sight lines on" : "Sight lines off"); }
    else if(k === "h" || k === "H" || k === "j" || k === "J" || k === "r" || k === "R" || k === "w" || k === "W" || k === "c" || k === "C" || k === "0" ||
            k === "7" || k === "8" || k === "9") {}  // keep the view shortcuts from switching away mid-edit
    else done = false;
    if(done){ e.preventDefault(); e.stopPropagation(); }
  });

  var ro = null;
  function onTheme(){ if(S.alive) draw(); }
  if(window.matchMedia){ try { var mq = window.matchMedia("(prefers-color-scheme: dark)"); if(mq.addEventListener) mq.addEventListener("change", onTheme); ro = mq; } catch(e){ ro = null; } }

  var start = PENDING; PENDING = null;
  var adopted = start && adopt(start.doc);
  if(adopted) load(adopted, typeof start.idx === "number" ? start.idx : -1);
  else {
    var l = drafts();
    var first = l && l.length ? adopt(l[0]) : null;
    if(first) load(first, 0); else load(docOf("My arena", openYard(32)), -1);
  }

  return {
    destroy: function(){ S.alive = false; if(ro && ro.removeEventListener) ro.removeEventListener("change", onTheme); },
    // test hooks (the browser smoke test)
    state: function(){ return S; }, check: function(){ return S.chk; }, doc: current,
    place: function(tool, x, z){ setTool(tool); useTool(snap(x), snap(z)); return S.chk; },
    box: function(x0, z0, x1, z1, kind){ if(kind) S.kind = kind; addBox(x0, z0, x1, z1); return S.chk; },
    save: save, walk: walkIt, room: roomIt, canvas: canvas
  };
}

/* ---------- registration: the Workshop card and the maker hook ---------- */
HQV.register({id: "make-fps", name: "Map Editor", icon: "🧱", workshop: true,
  desc: "Build a Blaster Arena map: walls, cover, spawns and pickups, checked as you go",
  badge: function(){ var l = drafts(); return l && l.length ? l.length+(l.length === 1 ? " map" : " maps") : ""; },
  mount: function(el){ if(CUR) CUR.destroy(); CUR = makeEditor(el); },
  unmount: function(){ if(CUR){ CUR.destroy(); CUR = null; } },
  pause: function(){}, resume: function(){}});

HQV.makers = HQV.makers || {};
HQV.makers.fps = {
  game: "make-fps", name: "Map Editor", icon: "🧱",
  // Open the editor on a MapDoc (a gallery remix, a story map...).
  edit: function(doc){ var a = adopt(doc); if(!a) return false; PENDING = {doc: a, idx: -1}; api.open("make-fps"); return true; },
  // Play a MapDoc: opts.room hosts it in the Blaster room card, else a solo walk-through.
  play: function(doc, opts){ var a = adopt(doc); if(!a || typeof HQV.fpsPlay !== "function") return false; return HQV.fpsPlay(a, !!(opts && opts.room)); },
  drafts: function(){ var l = drafts(); return l ? l.map(function(d){ return clone(d); }) : []; }
};
HQV.mapEditDebug = function(){ return CUR; };
// The pure pieces, for tests/test_mapedit.py (run under node) and the gallery's thumbnails.
HQV.mapEdit = {check: check, canon: canon, docOf: docOf, adopt: adopt, nameErr: nameErr, openYard: openYard, fromCourtyard: fromCourtyard,
               limits: {boxes: MAX_BOXES, minSpawns: MIN_SPAWNS, maxSpawns: MAX_SPAWNS, pickups: MAX_PICKUPS, data: MAX_DATA, draft: DRAFT_MAX,
                        drafts: DRAFTS, name: NAME_MAX, los: LOS_WARN, coord: COORD_MAX, kinds: KIND_IDS.slice()}};
})();
