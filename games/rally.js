/* Valley: Valley Rally. Point-to-point off-road stages in a low-poly world, solo time
 * trials against your own ghost, a three-stage rally event, or live races of up to 8 in
 * an Arena room (the others are translucent ghost cars: no collisions).
 *
 * Code lives in games/rally/ (all original, all generated, no files):
 *   physics.js  the car: rigid body, raycast suspension, Pacejka tyres, drivetrain (pure, node-tested)
 *   stage.js    seeded stages: road, heightfield, banking, gates, surfaces, scenery (pure, node-tested)
 *   scene.js    the three.js world: terrain, road, trees, cars, dust, tyre tracks, sky, camera
 * This file is the Valley game: menus, HUD, input, ghosts, saves and the room protocol.
 *
 * Multiplayer (refereed by the Arena, backend/src/rally.rs): the host sends the stage as a
 * MapDoc {kind:"rally", v:1, name, data:{seed, biome, night, length, road, gates}}; everyone
 * rebuilds the world from seed/biome/night/length and the Arena checks the road and gates.
 * You drive your own car and send {x, z, y (cm), yaw (deg), v (dm/s), q (your clock, cs),
 * dr} ~16 times a second; the Arena checks speed and gates and answers with snaps (15 Hz),
 * splits, finishes and results. Only those numbers and a car id travel; nothing
 * transcript-derived ever does.
 */
(function(){
"use strict";
var HQV = window.HQV; if(!HQV || !HQV.api || !HQV.engine) return;
var E = HQV.engine, api = HQV.api, MP = HQV.mp || null;
var now = E.now, clamp = E.clamp, calm = E.calm, say = E.say, tokens = E.tokens, myClock = E.myClock, hasWebGL2 = E.hasWebGL2;

var LIBS = null;
// games/rally/*.js, loaded once, in order, the first time the game opens
function loadLibs(){
  if(LIBS) return LIBS;
  LIBS = ["physics", "stage", "scene"].reduce(function(p, n){
    return p.then(function(){
      var have = n === "physics" ? HQV.rallyPhysics : n === "stage" ? HQV.rallyStage : HQV.rallyScene;
      if(have) return null;
      return new Promise(function(res, rej){
        var s = document.createElement("script"); s.src = "/games/rally/" + n + ".js"; s.async = false;
        s.onload = res; s.onerror = function(){ rej(new Error(n + ".js failed to load")); };
        document.head.appendChild(s);
      });
    });
  }, Promise.resolve()).catch(function(e){ LIBS = null; throw e; });
  return LIBS;
}

/* ---------- formatting ---------- */
function fmtMs(ms){
  if(ms == null || !isFinite(ms)) return "–";
  ms = Math.max(0, Math.round(ms)); var m = Math.floor(ms/60000), s = Math.floor((ms % 60000)/1000), r = ms % 1000;
  return m + ":" + (s < 10 ? "0" : "") + s + "." + ("00" + r).slice(-3);
}
function fmtDelta(ms){ if(ms == null || !isFinite(ms)) return ""; var a = Math.abs(ms)/1000; return (ms < 0 ? "−" : "+") + a.toFixed(2); }
function nameOf(p){ return (p && (p.name || p.displayName || p.handle)) || "Racer"; }

/* ---------- the save: api.save.rally ---------- */
function rsave(){
  var s = api.save; if(!s) return {best: {}, splits: {}, ghosts: {}, opts: {}};
  if(!s.rally || typeof s.rally !== "object" || Array.isArray(s.rally)) s.rally = {};
  var r = s.rally;
  ["best", "splits", "ghosts"].forEach(function(k){ if(!r[k] || typeof r[k] !== "object" || Array.isArray(r[k])) r[k] = {}; });
  return r;
}
function opt(name, fb){ var r = rsave(); return r[name] != null ? r[name] : fb; }
function setOpt(name, v){ rsave()[name] = v; api.persist(); }
// stage keys seen in rooms (for the leaderboard picker in the Arena view): a per-viewer convenience
function rememberKey(key, name){
  if(!/^c-[0-9a-f]{12}$/.test(String(key || ""))) return;
  try {
    var list = JSON.parse(localStorage.getItem("hq_rally_keys") || "[]"); if(!Array.isArray(list)) list = [];
    list = list.filter(function(e){ return Array.isArray(e) && e[0] !== key; });
    list.unshift([key, String(name || "Stage").slice(0, 40)]);
    localStorage.setItem("hq_rally_keys", JSON.stringify(list.slice(0, 12)));
  } catch(e){}
}

/* ---------- ghosts: ~10 Hz samples packed into a short string ---------- */
var B64 = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_", B64I = {};
for(var bi = 0; bi < 64; bi++) B64I[B64.charAt(bi)] = bi;
var GHOST_HZ = 10, GHOST_MAX = 3, GHOST_MAX_CHARS = 26000;
function ghostRecorder(x, y, z){
  var R = {x0: Math.round(x*10), y0: Math.round(y*10), z0: Math.round(z*10), lx: 0, ly: 0, lz: 0, out: []};
  R.lx = R.x0; R.ly = R.y0; R.lz = R.z0;
  R.push = function(x, y, z, yaw, pitch, roll){
    var qx = Math.round(x*10), qy = Math.round(y*10), qz = Math.round(z*10);
    var dx = clamp(qx - R.lx, -2047, 2047), dz = clamp(qz - R.lz, -2047, 2047), dy = clamp(qy - R.ly, -31, 31);
    R.lx += dx; R.lz += dz; R.ly += dy;
    var yw = Math.round(((yaw % (Math.PI*2)) + Math.PI*2)/(Math.PI*2)*4096) & 4095;
    var pt = clamp(Math.round(pitch/(Math.PI/2)*31), -31, 31) + 32, rl = clamp(Math.round(roll/(Math.PI/2)*31), -31, 31) + 32;
    var a = dx + 2048, b = dz + 2048;
    R.out.push(B64.charAt(a >> 6) + B64.charAt(a & 63) + B64.charAt(b >> 6) + B64.charAt(b & 63) + B64.charAt(dy + 32) +
               B64.charAt(yw >> 6) + B64.charAt(yw & 63) + B64.charAt(pt & 63) + B64.charAt(rl & 63));
  };
  R.pack = function(ms){ return {v: 1, hz: GHOST_HZ, x: R.x0, y: R.y0, z: R.z0, ms: Math.round(ms), d: R.out.join("")}; };
  return R;
}
function ghostDecode(g){
  if(!g || typeof g !== "object" || typeof g.d !== "string" || g.d.length % 9) return null;
  var x = g.x|0, y = g.y|0, z = g.z|0, out = [];
  for(var i = 0; i < g.d.length; i += 9){
    var c = function(k){ var v = B64I[g.d.charAt(i + k)]; return v == null ? 0 : v; };
    x += ((c(0) << 6) | c(1)) - 2048; z += ((c(2) << 6) | c(3)) - 2048; y += c(4) - 32;
    var yw = ((c(5) << 6) | c(6))/4096*Math.PI*2, pt = (c(7) - 32)/31*Math.PI/2, rl = (c(8) - 32)/31*Math.PI/2;
    out.push({x: x/10, y: y/10, z: z/10, yaw: yw, pitch: pt, roll: rl});
  }
  return {hz: g.hz || GHOST_HZ, ms: g.ms|0, s: out};
}

/* ---------- input: keyboard, gamepad ---------- */
var RALLY_BUTTONS = {hb: {keys: ["Space"], pad: [0, 2]}, rset: {keys: ["KeyR"], pad: [3]}, cam: {keys: ["KeyC"], pad: [8]},
                     esc: {keys: ["Escape", "KeyP"], pad: [9]}, keyshelp: {keys: ["KeyH"], pad: []}};
function padValue(I, i){ var p = I && I.pad, b = p && p.buttons && p.buttons[i]; return b ? (typeof b === "object" ? b.value : +b) : 0; }
function padAxis(I, i){ var p = I && I.pad, v = p && p.axes ? +p.axes[i] || 0 : 0; var D = 0.12; return Math.abs(v) < D ? 0 : (v - (v > 0 ? D : -D))/(1 - D); }

var CAR_SWATCH = ["#e9eef2", "#2e6a4f", "#f0b23a"];
var DIFFS = {awd: "AWD", rwd: "RWD", fwd: "FWD"};
var INTERP = 0.12, JIT_MAX = 0.25, EXTRAP = 0.3, SNAPS = 12, SEND_EVERY = 1/16, CD_SECS = 3;

/* =============================== the game view =============================== */
var CUR = null;
function makeGame(host, opts){
  opts = opts || {};
  var P = null, S = null, SC = null;        // physics, stage, scene modules (after loadLibs)
  var V = {mode: opts.mode || "solo", ctx: opts.ctx || null, alive: true, paused: false, phase: "menu", note: "",
    stage: null, info: null, key: null, car: null, handle: null, ghost: null, gate: 0, splits: [], raceT: 0, finishMs: null,
    event: null, cam: clamp(opt("cam", 0)|0, 0, 2), units: opt("units", "kmh") === "mph" ? "mph" : "kmh", assist: opt("assist", true) !== false,
    low: !!opt("low", false), lobby: [], host: null, roomPhase: "idle", roomStage: null, others: {}, offset: null, sv: {lastQ: -1, off: null, jit: 0},
    goAt: null, greenLocal: null, sendAt: 0, results: null, prog: {}, leaderAt: {}, fps: 60, frames: 0, fpsT: 0, padSeen: 0,
    thr: 0, brk: 0, hb: 0, rec: null, recT: 0, upsideT: 0, lastGatePos: null, hint: 0, unsupported: false, showMenu: false, pendingStart: null};
  V.myId = function(){ return V.mode === "mp" && MP ? MP.me() : "me"; };

  var root = api.mk("div", "vg-golf vg-rally"), menu = api.mk("div", "vg-golf-menu vg-rally-menu"), stageBox = api.mk("div", "vg-golf-stage hidden");
  root.appendChild(menu); root.appendChild(stageBox); host.appendChild(root);
  var wrap = api.mk("div", "vg-golf-view vg-rally-view");
  // HUD
  var hudTop = api.mk("div", "vg-rally-top"), hudTime = api.mk("b", "vg-rally-time", "0:00.000"), hudGate = api.mk("span", "vg-rally-gate", "GATE 1 OF 20");
  var strip = api.mk("canvas", "vg-rally-strip"), splitBox = api.mk("div", "vg-rally-split hidden");
  hudTop.appendChild(hudTime); hudTop.appendChild(hudGate); hudTop.appendChild(strip); hudTop.appendChild(splitBox);
  var hudBL = api.mk("div", "vg-rally-bl"), hudDist = api.mk("b", "vg-rally-dist", ""), bars = api.mk("canvas", "vg-rally-bars");
  hudBL.appendChild(hudDist); hudBL.appendChild(bars);
  var gauge = api.mk("canvas", "vg-rally-gauge");
  var standBox = api.mk("ol", "vg-rally-stand");
  var cdBox = api.mk("div", "vg-kart-cd vg-rally-cd hidden"), cardBox = api.mk("div", "vg-golf-card vg-rally-card hidden");
  cardBox.setAttribute("role", "dialog"); cardBox.setAttribute("aria-label", "Stage results");
  var pauseBox = api.mk("div", "vg-rally-pause hidden"); pauseBox.setAttribute("role", "dialog"); pauseBox.setAttribute("aria-label", "Paused");
  pauseBox.addEventListener("keydown", function(e){ if(e.key === "Escape"){ e.preventDefault(); e.stopPropagation(); togglePause(false); } });
  var badge = api.mk("span", "vg-reconnecting vg-golf-badge hidden", "Reconnecting…"); badge.setAttribute("role", "status");
  var load = api.mk("div", "vg-meter vg-golf-load hidden"), loadFill = api.mk("i"); load.appendChild(loadFill);
  [hudTop, hudBL, gauge, standBox, cdBox].forEach(function(n){ n.setAttribute("aria-hidden", "true"); });
  load.setAttribute("aria-hidden", "true");
  var hint = api.mk("p", "vg-golf-hint"), tools = api.mk("div", "vg-row vg-golf-tools"), keysBox = api.mk("div", "vg-golf-keys");
  stageBox.appendChild(wrap); stageBox.appendChild(hint); stageBox.appendChild(tools); stageBox.appendChild(keysBox);
  var KEYS = [["W / ↑ / RT", "throttle"], ["S / ↓ / LT", "brake, then reverse"], ["A D / ← → / stick", "steer"], ["Space / A", "handbrake"],
              ["R / Y", "back to the last gate"], ["C / View", "camera"], ["Esc / Start", "pause"], ["H", "hide these keys"]];
  function renderKeys(){
    keysBox.textContent = "";
    if(opt("keysHidden", false)){ keysBox.appendChild(api.mk("span", "vg-muted", "Keys hidden — press H to show them")); return; }
    KEYS.forEach(function(r){ var it = api.mk("span", "vg-golf-key"); it.appendChild(api.mk("kbd", null, r[0])); it.appendChild(document.createTextNode(" " + r[1])); keysBox.appendChild(it); });
  }
  var canvas = null, R3 = null, IN = null, raf = 0, lastT = 0, ro = null, TOK = tokens(), tokAt = 0, eng = null, skid = null, gravel = null;

  /* ---------- menu ---------- */
  function isHost(){ return V.mode === "mp" && V.host != null && V.host === V.myId(); }
  function stageCard(info, onPick, disabled){
    var card = api.mk("button", "vg-card vg-golf-course vg-rally-stagecard"); card.type = "button";
    var ic = api.mk("span", "vg-card-ic", info.night ? "🌙" : info.biome === "snow" ? "❄️" : info.biome === "hills" ? "🏜️" : "🌲"); ic.setAttribute("aria-hidden", "true"); card.appendChild(ic);
    var tt = api.mk("span", "vg-card-t"); tt.appendChild(api.mk("b", null, info.name));
    var key = S ? S.localKey({data: info}) : "", best = rsave().best[key];
    tt.appendChild(api.mk("span", null, (S ? S.BIOME_NAMES[info.biome] : info.biome) + (info.night ? " · night" : "") + " · " + (info.length/1000).toFixed(1) + " km" + (best ? " · best " + fmtMs(best) : "")));
    card.appendChild(tt);
    if(disabled) card.disabled = true;
    card.addEventListener("click", function(){ onPick(info); });
    return card;
  }
  function renderMenu(){
    if(!V.alive) return;
    menu.textContent = "";
    if(!S){ menu.appendChild(api.mk("p", "vg-muted", V.note || "Loading Valley Rally…")); return; }
    var mp = V.mode === "mp", host = isHost();
    menu.appendChild(api.mk("p", "vg-muted", mp
      ? "Up to 8 drivers on one stage, each against the clock. The others are ghost cars: no collisions. The Arena times every gate."
      : "Point-to-point stages against the clock and your best ghost. Gravel, dirt, snow and tarmac; jumps, crests and banked corners."));
    if(mp && V.unsupported) menu.appendChild(api.mk("p", "vg-msg", "This Arena server doesn't run Valley Rally yet. You can still drive solo."));
    var top = api.mk("div", "vg-row");
    if(mp) top.appendChild(api.btn("Drive solo", "", function(){ V.mode = "solo"; renderMenu(); }));
    else if(V.ctx) top.appendChild(api.btn("Back to the room", "", function(){ V.mode = "mp"; renderMenu(); if(MP) MP.send("rally", "view"); }));
    top.appendChild(api.btn("Settings", "", function(){ settingsDialog(menu); }));
    menu.appendChild(top);
    // cars
    menu.appendChild(api.mk("h4", "vg-golf-h", "Your car"));
    var cars = api.mk("div", "vg-golf-chars vg-rally-cars"); cars.setAttribute("role", "group"); cars.setAttribute("aria-label", "Pick a car");
    var mine = clamp(opt("car", 0)|0, 0, P.CARS.length - 1);
    P.CARS.forEach(function(c, i){
      var b = api.btn("", "vg-golf-char vg-rally-car" + (i === mine ? " on" : ""), function(){
        setOpt("car", i); if(mp && MP) MP.send("rally", "car", {car: i}); renderMenu();
      });
      var sw = api.mk("i", "vg-kart-sw"); sw.style.background = CAR_SWATCH[i]; sw.setAttribute("aria-hidden", "true"); b.appendChild(sw);
      var t = api.mk("span", "vg-rally-carname"); t.appendChild(api.mk("b", null, c.name)); t.appendChild(api.mk("small", null, c.kind + " · " + DIFFS[c.drive] + " · " + Math.round(maxPower(c)) + " hp"));
      b.appendChild(t); b.setAttribute("aria-pressed", i === mine ? "true" : "false");
      cars.appendChild(b);
    });
    menu.appendChild(cars);
    if(mp && !host){
      menu.appendChild(api.mk("p", "vg-muted", V.roomPhase === "racing" || V.roomPhase === "countdown"
        ? "A stage is running" + (V.roomStage ? ": " + V.roomStage.name : "") + ". You'll be on the next one."
        : "Waiting for the host (★ in the lobby) to pick a stage."));
      if(V.results) menu.appendChild(resultsTable(V.results, "Last stage"));
      return;
    }
    // stages
    var daily = S.daily(new Date());
    menu.appendChild(api.mk("h4", "vg-golf-h", mp ? "Pick a stage to start" : "Stages"));
    var grid = api.mk("div", "vg-golf-courses");
    grid.appendChild(stageCard(daily, pickStage));
    S.PRESETS.forEach(function(p){ grid.appendChild(stageCard(p, pickStage)); });
    menu.appendChild(grid);
    // random seed / biome / night
    var rr = api.mk("div", "vg-row vg-rally-random");
    var seedIn = api.mk("input", "vg-input"); seedIn.type = "text"; seedIn.maxLength = 32; seedIn.placeholder = "seed (any word)"; seedIn.setAttribute("aria-label", "Stage seed");
    seedIn.value = opt("seed", "");
    var bsel = api.mk("select", "vg-select"); bsel.setAttribute("aria-label", "Biome");
    S.BIOMES.forEach(function(b){ var o = api.mk("option", null, S.BIOME_NAMES[b]); o.value = b; if(b === opt("biome", "forest")) o.selected = true; bsel.appendChild(o); });
    var nl = api.mk("label", "vg-golf-check"), nc = api.mk("input"); nc.type = "checkbox"; nc.checked = !!opt("night", false);
    nl.appendChild(nc); nl.appendChild(document.createTextNode(" Night"));
    var lsel = api.mk("select", "vg-select"); lsel.setAttribute("aria-label", "Stage length");
    [2500, 3000, 3500, 4000].forEach(function(n){ var o = api.mk("option", null, (n/1000).toFixed(1) + " km"); o.value = String(n); if(n === (opt("len", 3000)|0)) o.selected = true; lsel.appendChild(o); });
    function custom(){
      var seed = String(seedIn.value || "").trim().replace(/[^A-Za-z0-9 _.:-]/g, "").slice(0, 32);
      if(!seed){ var r = new Uint32Array(1); try { crypto.getRandomValues(r); } catch(e){ r[0] = Date.now() >>> 0; } seed = "r" + r[0].toString(36); seedIn.value = seed; }
      var rsv = rsave(); rsv.seed = seed; rsv.biome = bsel.value; rsv.night = nc.checked; rsv.len = +lsel.value; api.persist();
      return {id: "custom", name: S.nameFor(seed), seed: seed, biome: bsel.value, night: nc.checked, length: +lsel.value};
    }
    rr.appendChild(seedIn); rr.appendChild(bsel); rr.appendChild(lsel); rr.appendChild(nl);
    rr.appendChild(api.btn("🎲 Random stage", "", function(){ seedIn.value = ""; pickStage(custom()); }));
    rr.appendChild(api.btn("Drive this seed", "", function(){ pickStage(custom()); }));
    menu.appendChild(rr);
    // rally event: three stages, one total
    var ev = api.mk("div", "vg-rally-event");
    ev.appendChild(api.mk("h4", "vg-golf-h", "Rally event · 3 stages back to back"));
    var evSeed = "cup-" + S.utcDay(new Date()), evList = S.event(evSeed, 3), eb = rsave().eventBest;
    ev.appendChild(api.mk("p", "vg-muted", evList.map(function(e){ return e.name + " (" + S.BIOME_NAMES[e.biome].toLowerCase() + ")"; }).join(" → ") + (eb && eb.seed === evSeed ? " · your best total " + fmtMs(eb.ms) : "")));
    ev.appendChild(api.btn("Start the event", "primary", function(){ startEvent(evSeed, 3); }));
    menu.appendChild(ev);
    if(mp && V.results) menu.appendChild(resultsTable(V.results, "Last stage"));
  }
  function maxPower(c){ var best = 0; c.engine.curve.forEach(function(p){ best = Math.max(best, p[0]*p[1]/9549); }); return best*1.341; }
  function pickStage(info){
    if(V.mode === "mp"){ startRoomStage(info, null); return; }
    V.event = null; startSolo(info);
  }
  function startEvent(seed, count){
    var list = S.event(seed, count);
    if(V.mode === "mp"){ V.mpEvent = {seed: seed, count: count, index: 0, list: list}; startRoomStage(list[0], {index: 0, count: count}); return; }
    V.event = {seed: seed, list: list, index: 0, times: []};
    startSolo(list[0]);
  }
  function settingsDialog(where){
    var box = api.mk("div", "vg-rally-settings"); box.setAttribute("role", "group"); box.setAttribute("aria-label", "Rally settings");
    function check(label, val, fn){
      var l = api.mk("label", "vg-golf-check"), cb = api.mk("input"); cb.type = "checkbox"; cb.checked = val;
      cb.addEventListener("change", function(){ fn(cb.checked); });
      l.appendChild(cb); l.appendChild(document.createTextNode(" " + label)); box.appendChild(l); return cb;
    }
    check("Countersteer + stability assist (recommended on a keyboard)", V.assist, function(v){ V.assist = v; setOpt("assist", v); if(V.car) V.car.assist = v; });
    check("Speed in mph", V.units === "mph", function(v){ V.units = v ? "mph" : "kmh"; setOpt("units", V.units); });
    check("Low detail (faster on older laptops)", V.low, function(v){ V.low = v; setOpt("low", v); api.toast("Low detail applies from the next stage"); });
    var cr = api.mk("div", "vg-row"); cr.appendChild(api.mk("span", "vg-muted", "Camera"));
    ["Chase", "Bumper", "Hood"].forEach(function(n, i){ var b = api.btn(n, V.cam === i ? "primary" : "", function(){ V.cam = i; setOpt("cam", i); box.remove(); settingsDialog(where); }); b.setAttribute("aria-pressed", V.cam === i ? "true" : "false"); cr.appendChild(b); });
    box.appendChild(cr);
    var vr = api.mk("label", "vg-row vg-rally-vol"), vi = api.mk("input"); vi.type = "range"; vi.min = "0"; vi.max = "1"; vi.step = "0.05"; vi.value = String(E.sfxVolume());
    vi.setAttribute("aria-label", "Sound volume"); vi.addEventListener("input", function(){ E.sfxSetVolume(+vi.value); });
    vr.appendChild(api.mk("span", "vg-muted", "Sound")); vr.appendChild(vi); box.appendChild(vr);
    box.appendChild(api.btn("Done", "", function(){ box.remove(); }));
    var old = where.querySelector(".vg-rally-settings"); if(old) old.remove();
    where.appendChild(box);
    var f = box.querySelector("input"); if(f) f.focus();
  }

  /* ---------- the stage view ---------- */
  function showStage(on){ stageBox.classList.toggle("hidden", !on); menu.classList.toggle("hidden", on); if(on){ ensureRenderer(); renderTools(); renderKeys(); } }
  function renderTools(){
    tools.textContent = "";
    tools.appendChild(api.btn(V.mode === "mp" ? "Leave the stage" : "Back to stages", "", function(){ quitStage(); }));
    tools.appendChild(api.btn("Camera", "", function(){ V.cam = (V.cam + 1) % 3; setOpt("cam", V.cam); if(canvas) canvas.focus(); }));
    tools.appendChild(api.btn("Pause", "", function(){ togglePause(true); }));
  }
  function quitStage(){
    if(V.mode === "mp" && MP && isHost() && (V.phase === "countdown" || V.phase === "race")){
      if(!window.confirm("End the stage for everyone?")) return;
      MP.send("rally", "end");
    }
    resetRun(); showStage(false); renderMenu();
  }
  function resetRun(){
    V.phase = "menu"; V.paused = false; pauseBox.classList.add("hidden"); cardBox.classList.add("hidden"); cdBox.classList.add("hidden");
    splitBox.classList.add("hidden");
    if(R3){ if(V.handle){ R3.removeCar(V.handle); V.handle = null; } if(V.ghost && V.ghost.handle){ R3.removeCar(V.ghost.handle); } Object.keys(V.others).forEach(function(u){ var o = V.others[u]; if(o.handle) R3.removeCar(o.handle); o.handle = null; }); }
    V.ghost = null; V.car = null; V.rec = null;
    if(eng){ eng.stop(); eng = null; } if(skid){ skid.stop(); skid = null; } if(gravel){ gravel.stop(); gravel = null; }
  }
  function buildStageFor(info, doc){
    var st = doc ? S.build({seed: doc.data.seed, biome: doc.data.biome, night: doc.data.night, length: doc.data.length, name: doc.name}, doc)
                 : S.build({seed: info.seed, biome: info.biome, night: info.night, length: info.length, name: info.name});
    return st;
  }
  function startSolo(info){
    resetRun();
    V.info = info; V.stage = buildStageFor(info, null); V.key = S.localKey(V.stage.doc);
    beginStage();
  }
  // common start: the car on the grid, the world built, the countdown
  function beginStage(){
    var st = V.stage;
    V.car = P.create(clamp(opt("car", 0)|0, 0, P.CARS.length - 1), {assist: V.assist});
    P.place(V.car, st.start.x, st.start.z, st.start.h*Math.PI/180, st.env);
    V.gate = 0; V.splits = []; V.raceT = 0; V.finishMs = null; V.hint = 0; V.lastGatePos = {x: st.start.x, z: st.start.z, h: st.start.h};
    V.prog = {}; V.leaderAt = {}; V.results = null; V.upsideT = 0; V.thr = V.brk = V.hb = 0;
    // ghost: your best run on this stage (solo)
    V.ghost = null;
    if(V.mode === "solo"){
      var g = ghostDecode(rsave().ghosts[V.key]);
      if(g && g.s.length > 4) V.ghost = {data: g, handle: null, t: 0};
    }
    V.rec = ghostRecorder(V.car.p[0], V.car.p[1], V.car.p[2]); V.recT = 0;
    showStage(true);
    V.phase = "loading";
    load.classList.remove("hidden"); loadFill.style.width = "30%";
    whenRenderer(function(){
      if(!V.alive || !V.stage) return;
      if(R3){
        R3.setStage(st);
        V.handle = R3.addCar(V.car.spec, {number: 1 + (opt("car", 0)|0)});
        R3.carLights(V.handle, st.night); V.handle.lit = st.night;
        if(V.ghost) V.ghost.handle = R3.addCar(V.car.spec, {ghost: true, name: "Best " + fmtMs(V.ghost.data.ms)});
        R3.resetCamera();
      }
      load.classList.add("hidden");
      if(V.mode === "solo"){ V.phase = "countdown"; V.goAt = now() + CD_SECS; }
      else V.phase = V.greenLocal != null ? "countdown" : "grid";
      hint.textContent = st.name + " · " + S.BIOME_NAMES[st.biome] + (st.night ? " at night" : "") + " · " + (st.length/1000).toFixed(2) + " km · " + st.gates.length + " gates";
      say("Stage " + st.name + ". " + (st.length/1000).toFixed(1) + " kilometres.");
      if(canvas) canvas.focus();
    });
  }

  /* ---------- multiplayer ---------- */
  function startRoomStage(info, event){
    if(!MP) return;
    var st = buildStageFor(info, null);
    var msg = {stage: st.doc}; if(event && event.count > 1) msg.event = {index: event.index|0, count: event.count|0};
    if(MP.send("rally", "start", msg)) V.pendingStart = info; else api.toast("Not connected to the Arena");
  }
  function onEvent(m){
    if(!S){ V.queued = V.queued || []; V.queued.push(m); return; }
    var me = V.myId();
    switch(m.ev){
      case "lobby":
        V.lobby = Array.isArray(m.players) ? m.players.slice(0, 8) : []; V.host = m.host || null; V.roomPhase = m.phase || "idle";
        V.roomStage = m.stage && typeof m.stage === "object" ? m.stage : null;
        V.lobby.forEach(function(p){ var o = V.others[p.uid]; if(o) o.name = nameOf(p); });
        // the host ended the stage (or it was never ours): back to the lobby
        if(V.mode === "mp" && V.roomPhase === "idle" && (V.phase === "grid" || V.phase === "countdown" || V.phase === "race" || V.phase === "loading")){
          resetRun(); showStage(false); api.toast("The stage was ended");
        }
        if(V.phase === "menu") renderMenu();
        standDirty = true;
        return;
      case "rally": {
        // A stage. It runs only if a green light follows ("go": a start, or a rejoin mid-stage);
        // when results follow instead ("done"), it was over before we came.
        var doc = S.cleanDoc(m.stage); if(!doc){ api.toast("The Arena sent a stage this client can't read"); return; }
        if(m.key) rememberKey(m.key, m.name || doc.name);
        V.roomStage = {key: m.key, name: m.name || doc.name, gates: m.gates};
        V.pendingRally = {doc: doc, m: m};
        return;
      }
      case "go": {
        if(V.pendingRally){
          var pr2 = V.pendingRally, d2 = pr2.doc; V.pendingRally = null;
          V.mode = "mp"; resetRun();
          V.key = pr2.m.key;
          V.info = {name: pr2.m.name || d2.name, seed: d2.data.seed, biome: d2.data.biome, night: d2.data.night, length: d2.data.length};
          V.roomEvent = pr2.m.event && typeof pr2.m.event === "object" ? {index: pr2.m.event.index|0, count: pr2.m.event.count|0} : null;
          V.stage = buildStageFor(V.info, d2);
          V.greenLocal = null; V.goAt = null; V.offset = null; V.goServer = 0;
          beginStage();
        }
        V.goServer = +m.at || 0;
        var cd = clamp(+m.countdown || 3, 0, 10);
        // until a snap tells us the Arena's clock, assume the start was just sent
        if(V.offset == null) V.greenLocal = now() + cd; else V.greenLocal = (V.goServer + V.offset)/1000;
        if(V.phase === "grid" || V.phase === "loading") V.phase = V.phase === "loading" ? "loading" : "countdown";
        V.goAt = V.greenLocal;
        return;
      }
      case "snap": {
        var t = +m.t || 0, lt = now()*1000;
        // the Arena's clock on ours: the least-delayed frame wins, drifting slowly to newer ones
        var off = lt - t;
        if(V.offset == null || off < V.offset) V.offset = off; else V.offset += (off - V.offset)*0.01;
        if(V.goServer){ V.greenLocal = (V.goServer + V.offset)/1000; V.goAt = V.greenLocal; }
        var st2 = E.senderTime(V.sv, Math.round(t/10), now(), JIT_MAX);
        if(st2 === null) return;
        (m.cars || []).forEach(function(c){
          if(!c || c.uid === me) return;
          var o = other(c.uid);
          pushSnap(o, st2, c);
        });
        return;
      }
      case "split": {
        var uid = m.uid, gi = m.gate|0, ms = +m.ms || 0;
        var pr = V.prog[uid] = V.prog[uid] || {gate: -1, ms: 0, fin: null, dnf: false};
        if(gi > pr.gate){ pr.gate = gi; pr.ms = ms; }
        if(V.leaderAt[gi] == null || ms < V.leaderAt[gi]) V.leaderAt[gi] = ms;
        if(uid === me){
          V.splits[gi] = ms;
          // the Arena's word is final: after a reconnect it may be ahead of what we saw
          if(V.stage && gi + 1 > V.gate && gi < V.stage.gates.length - 1){ var gg = V.stage.gates[gi]; V.gate = gi + 1; V.lastGatePos = {x: gg.x, z: gg.z, h: gg.h}; }
          if(V.flash && V.flash.gate === gi) showSplit(gi, ms, true);
        }
        standDirty = true;
        return;
      }
      case "finish": {
        var pf = V.prog[m.uid] = V.prog[m.uid] || {gate: -1, ms: 0};
        pf.fin = +m.ms || 0; pf.gate = V.stage ? V.stage.gates.length - 1 : pf.gate;
        if(m.uid === me){ V.finishMs = pf.fin; finishLocal(true); }
        else { var on = V.others[m.uid]; api.toast("🏁 " + (on ? on.name : "A driver") + " finished · " + fmtMs(pf.fin)); }
        standDirty = true;
        return;
      }
      case "dnf": {
        var pd = V.prog[m.uid] = V.prog[m.uid] || {gate: -1, ms: 0}; pd.dnf = true; pd.reason = String(m.reason || "");
        standDirty = true;
        return;
      }
      case "done": {
        V.pendingRally = null;
        V.results = {rows: Array.isArray(m.results) ? m.results : [], key: m.key, name: m.name, event: m.event || null};
        if(V.phase !== "menu") showResults();
        else renderMenu();
        return;
      }
      case "car": {
        var oc = V.others[m.uid]; if(oc){ oc.car = clamp(m.car|0, 0, P.CARS.length - 1); }
        return;
      }
      case "fix": {
        if(m.uid && m.uid !== me) return;
        if(!V.car || !V.stage) return;
        P.place(V.car, (+m.x || 0)/100, (+m.z || 0)/100, ((+m.yaw || 0)*Math.PI/180), V.stage.env);
        V.fixAt = now(); api.toast("The Arena put your car back");
        return;
      }
      case "error": return;
    }
  }
  function onError(m){
    var e = String(m.msg || m.error || "");
    if(/unknown game/.test(e)){ V.unsupported = true; renderMenu(); return true; }
    if(/lobby is full/.test(e)){ api.toast("This rally is full (8 drivers)"); return true; }
    if(/bad stage|bad event|already running|only the host|busy/.test(e)){ api.toast("⚠ " + e.slice(0, 120)); V.pendingStart = null; return true; }
    return false;
  }
  function other(uid){
    var o = V.others[uid];
    if(!o){
      var lp = V.lobby.filter(function(p){ return p.uid === uid; })[0];
      o = V.others[uid] = {uid: uid, name: lp ? nameOf(lp) : "Racer", car: lp ? clamp(lp.car|0, 0, P.CARS.length - 1) : 0, snaps: [], x: 0, y: 0, z: 0, yaw: 0, v: 0, dr: false, handle: null, ang: 0, q: [0, 0, 0, 1]};
    }
    return o;
  }
  function pushSnap(o, t, c){
    var sn = o.snaps, x = (+c.x || 0)/100, z = (+c.z || 0)/100, y = c.y != null ? (+c.y || 0)/100 : (V.stage ? V.stage.env.height(x, z) + 0.5 : 0);
    var last = sn[sn.length - 1], tp = last && (Math.abs(x - last.x) > 25 || Math.abs(z - last.z) > 25);
    if(tp) sn.length = 0;
    E.pushSnap(sn, {t: t, x: x, y: y, z: z, yaw: (+c.yaw || 0)*Math.PI/180, v: (+c.v || 0)/10, dr: !!c.dr}, SNAPS);
    if(tp || sn.length === 1){ o.x = x; o.y = y; o.z = z; o.yaw = sn[sn.length - 1].yaw; }
  }
  function sampleOther(o, t, dt){
    var b = E.bracket(o.snaps, t - INTERP - V.sv.jit); if(!b) return;
    var A = b.A, x, y, z, yaw, v;
    if(b.B){ var u = b.u; x = A.x + (b.B.x - A.x)*u; y = A.y + (b.B.y - A.y)*u; z = A.z + (b.B.z - A.z)*u; yaw = E.angLerp(A.yaw, b.B.yaw, u); v = A.v + (b.B.v - A.v)*u; }
    else { var ex = Math.min(b.after || 0, EXTRAP); x = A.x + Math.sin(A.yaw)*A.v*ex; z = A.z - Math.cos(A.yaw)*A.v*ex; y = A.y; yaw = A.yaw; v = A.v; }
    var k = calm() ? 1 : 1 - Math.exp(-18*dt);
    if(Math.abs(x - o.x) > 25 || Math.abs(z - o.z) > 25) k = 1;
    o.x += (x - o.x)*k; o.y += (y - o.y)*k; o.z += (z - o.z)*k; o.yaw = E.angLerp(o.yaw, yaw, Math.min(1, k*1.5)); o.v = v; o.dr = A.dr;
    o.ang += v*dt/0.31;
  }
  function sendPos(t){
    var C = V.car; if(V.mode !== "mp" || !MP || !C || V.phase !== "race" || V.finishMs != null || V.offline) return;
    if(t - V.sendAt < SEND_EVERY) return;
    V.sendAt = t;
    var hd = Math.round(P.headingOf(C.M)*180/Math.PI) % 360;
    var msg = {x: Math.round(C.p[0]*100), z: Math.round(C.p[2]*100), y: Math.round(C.p[1]*100), yaw: (hd + 360) % 360, v: Math.round(C.tel.speed*10), q: myClock()};
    if(C.tel.drift) msg.dr = 1;
    MP.send("rally", "pos", msg);
  }

  /* ---------- driving ---------- */
  function controls(dt){
    var I = IN; if(!I) return {throttle: 0, brake: 0, steer: 0, handbrake: 0};
    var kThr = I.down("up") ? 1 : 0, kBrk = I.down("down") ? 1 : 0, kSteer = (I.down("right") ? 1 : 0) - (I.down("left") ? 1 : 0);
    var pThr = padValue(I, 7), pBrk = padValue(I, 6), pSteer = padAxis(I, 0), pHb = Math.max(padValue(I, 0), padValue(I, 2));
    var padActive = pThr > 0.05 || pBrk > 0.05 || Math.abs(pSteer) > 0.05;
    if(padActive) V.padSeen = now();
    var analog = now() - V.padSeen < 1.5 && kSteer === 0;
    // keys ramp in (a stab of brake should not lock the wheels at once); triggers pass through
    V.thr += (kThr - V.thr)*Math.min(1, dt*(kThr > V.thr ? 9 : 14));
    V.brk += (kBrk - V.brk)*Math.min(1, dt*(kBrk > V.brk ? 5 : 14));
    var thr = Math.max(V.thr, pThr), brk = Math.max(V.brk, pBrk);
    var hb = I.down("hb") ? 1 : pHb > 0.5 ? 1 : 0;
    return {throttle: thr, brake: brk, steer: analog ? pSteer : clamp(kSteer + pSteer, -1, 1), handbrake: hb, analog: analog};
  }
  function resetToGate(){
    var C = V.car, st = V.stage; if(!C || !st) return;
    var g = V.lastGatePos, x = g.x, z = g.z, h = g.h;
    if(V.mode === "mp"){
      // the Arena allows at most 50 m per update: from far away, go back to the nearest road instead
      var dx = x - C.p[0], dz = z - C.p[2];
      if(dx*dx + dz*dz > 44*44){ var L = st.locate(C.p[0], C.p[2], V.hint), pt = st.pointAtS(Math.max(0, L.s - 2)); x = pt.x; z = pt.z; h = pt.h*180/Math.PI; }
    }
    P.place(C, x, z, h*Math.PI/180, st.env);
    V.upsideT = 0; say("Back on the road");
  }
  function update(dt, t){
    if(!V.car || !V.stage) return;
    if(IN) IN.poll();
    if(IN && IN.pressed("keyshelp")){ setOpt("keysHidden", !opt("keysHidden", false)); renderKeys(); }
    if(IN && IN.pressed("cam")){ V.cam = (V.cam + 1) % 3; setOpt("cam", V.cam); }
    if(IN && IN.pressed("esc") && V.phase !== "done") togglePause(!V.paused);
    if(V.paused && V.mode === "solo") return;
    var C = V.car, st = V.stage;
    if(V.phase === "countdown" && V.goAt != null){
      var left = Math.ceil(V.goAt - t);
      if(left > 0 && left <= 3 && cdBox.textContent !== String(left)){ cdShow(String(left)); say(String(left)); }
      if(t >= V.goAt){ V.phase = "race"; V.raceT = V.mode === "mp" && V.greenLocal != null ? Math.max(0, t - V.greenLocal) : 0; cdShow("GO!"); say("Go!"); }
    }
    var racing = V.phase === "race" && V.finishMs == null && !(V.paused && V.mode === "solo");
    var ctl = racing ? controls(dt) : {throttle: 0, brake: 0, steer: 0, handbrake: 0, hold: V.phase !== "race" && V.phase !== "done"};
    if(V.paused && V.mode === "mp") ctl = {throttle: 0, brake: 0.6, steer: 0, handbrake: 0};
    if(V.phase === "done" && V.finishMs != null) ctl = {throttle: 0, brake: 0.5, steer: 0, handbrake: 0};
    if(racing && IN && IN.pressed("rset")) resetToGate();
    C.assist = V.assist;
    var px = C.p[0], pz = C.p[2], t0 = V.raceT;
    var n = P.advance(C, ctl, st.env, dt);
    if(V.phase === "race" && V.finishMs == null) V.raceT += n*P.DT;
    // gates: the segment this frame drove, timed between the fixed steps
    if(V.phase === "race" && V.gate < st.gates.length){
      var u = S.crossesGate(st.gates[V.gate], px, pz, C.p[0], C.p[2]);
      if(u !== false) passGate(V.gate, (t0 + (V.raceT - t0)*u)*1000);
    }
    // upside down for 3 s: put it back
    if(C.upsideT > 3 && V.phase === "race") resetToGate();
    // ghost recording
    if(V.phase === "race" && V.finishMs == null && V.rec){
      V.recT += n*P.DT;
      while(V.recT >= 1/GHOST_HZ){
        V.recT -= 1/GHOST_HZ;
        var M = C.M, f = [-M[2], -M[5], -M[8]], r = [M[0], M[3], M[6]];
        V.rec.push(C.p[0], C.p[1], C.p[2], P.headingOf(M), Math.asin(clamp(f[1], -1, 1)), Math.asin(clamp(r[1], -1, 1)));
      }
    }
    V.hint = st.locate(C.p[0], C.p[2], V.hint).i;
    // remote cars
    Object.keys(V.others).forEach(function(u2){ sampleOther(V.others[u2], t, dt); });
    sendPos(t);
    audioUpdate(C);
    if(cdBox.dataset.until && t > +cdBox.dataset.until){ cdBox.classList.add("hidden"); cdBox.dataset.until = ""; }
    if(V.flashUntil && t > V.flashUntil){ splitBox.classList.add("hidden"); V.flashUntil = 0; }
  }
  function passGate(gi, ms){
    var st = V.stage, g = st.gates[gi];
    V.gate = gi + 1;
    V.lastGatePos = {x: g.x, z: g.z, h: g.h};
    var last = gi === st.gates.length - 1;
    if(V.mode === "solo" || V.splits[gi] == null) V.splits[gi] = Math.round(ms);
    if(last){ V.finishMs = Math.round(ms); finishLocal(false); return; }
    showSplit(gi, V.splits[gi], false);
  }
  function showSplit(gi, ms, server){
    var ref = null;
    if(V.mode === "mp"){ var la = V.leaderAt[gi]; if(la != null && la < ms - 1) ref = la; else if(la != null) ref = la; }
    else { var bs = rsave().splits[V.key]; if(Array.isArray(bs) && bs[gi] != null) ref = bs[gi]; }
    var d = ref != null ? ms - ref : null;
    splitBox.textContent = "";
    splitBox.appendChild(api.mk("b", null, "GATE " + (gi + 1)));
    splitBox.appendChild(api.mk("span", null, fmtMs(ms)));
    if(d != null && !(V.mode === "mp" && Math.abs(d) < 1 && ref === ms)) splitBox.appendChild(api.mk("i", d <= 0 ? "good" : "bad", fmtDelta(d)));
    splitBox.className = "vg-rally-split" + (d == null ? "" : d <= 0 ? " good" : " bad");
    V.flash = {gate: gi}; V.flashUntil = now() + 2.8;
    if(!server) say("Gate " + (gi + 1) + ", " + fmtMs(ms) + (d != null ? ", " + (d <= 0 ? "ahead by " : "behind by ") + (Math.abs(d)/1000).toFixed(2) : ""));
    E.sfx("hit");
  }
  function finishLocal(server){
    if(V.phase === "done" && !server) return;
    V.phase = "done";
    cdShow("FINISH");
    if(V.mode === "solo") soloFinish();
    else { say("Finished: " + fmtMs(V.finishMs) + (server ? "" : ". Waiting for the Arena")); }
  }
  function soloFinish(){
    var ms = V.finishMs, sv = rsave(), prev = sv.best[V.key], newBest = !prev || ms < prev;
    V.prevSplits = Array.isArray(sv.splits[V.key]) ? sv.splits[V.key].slice() : null;
    if(newBest){
      sv.best[V.key] = ms; sv.splits[V.key] = V.splits.slice();
      var gh = V.rec ? V.rec.pack(ms) : null;
      V.ghostSaved = false;
      if(gh && gh.d.length <= GHOST_MAX_CHARS){
        gh.at = Date.now(); sv.ghosts[V.key] = gh; V.ghostSaved = true;
        // keep the few most recent ghosts only (the save file is shared with every Valley game)
        var keys = Object.keys(sv.ghosts).sort(function(a, b){ return (sv.ghosts[b].at || 0) - (sv.ghosts[a].at || 0); });
        keys.slice(GHOST_MAX).forEach(function(k){ delete sv.ghosts[k]; });
      }
      api.persist();
    }
    if(V.event){
      V.event.times[V.event.index] = ms;
      if(V.event.index === V.event.list.length - 1){
        var tot = V.event.times.reduce(function(a, b){ return a + b; }, 0), eb = sv.eventBest;
        if(!eb || eb.seed !== V.event.seed || tot < eb.ms){ sv.eventBest = {seed: V.event.seed, ms: tot}; api.persist(); }
      }
    }
    if(HQV.story && HQV.story.note) try { HQV.story.note("rally-finish"); } catch(e){}
    showResults(newBest, prev);
  }

  /* ---------- results ---------- */
  function resultsTable(res, title){
    var box = api.mk("div", "vg-golf-score");
    box.appendChild(api.mk("b", null, title + (res.name ? " · " + res.name : "")));
    var ev = res.event && res.event.count > 1;
    var tb = api.mk("table", "vg-golf-table"), hr = api.mk("tr");
    ["#", "Driver", "Car", "Time"].concat(ev ? ["Event total"] : []).forEach(function(h){ hr.appendChild(api.mk("th", null, h)); }); tb.appendChild(hr);
    (res.rows || []).forEach(function(r){
      var tr = api.mk("tr");
      tr.appendChild(api.mk("td", null, r.dnf || r.ms == null ? "–" : String(r.place || "")));
      tr.appendChild(api.mk("td", r.uid === V.myId() ? "tot" : null, nameOf(r)));
      tr.appendChild(api.mk("td", null, (P.CARS[clamp(r.car|0, 0, P.CARS.length - 1)] || {}).name || ""));
      tr.appendChild(api.mk("td", null, r.ms == null ? "DNF" : fmtMs(r.ms)));
      if(ev) tr.appendChild(api.mk("td", null, r.total == null ? "–" : fmtMs(r.total)));
      tb.appendChild(tr);
    });
    box.appendChild(tb); return box;
  }
  function splitsTable(){
    var best = rsave().splits[V.key], tb = api.mk("table", "vg-golf-table vg-rally-splits"), hr = api.mk("tr");
    ["Gate", "Time", "vs best"].forEach(function(h){ hr.appendChild(api.mk("th", null, h)); }); tb.appendChild(hr);
    V.splits.forEach(function(ms, i){
      if(ms == null) return;
      var tr = api.mk("tr"), d = Array.isArray(V.prevSplits) && V.prevSplits[i] != null ? ms - V.prevSplits[i] : null;
      tr.appendChild(api.mk("td", null, i === V.stage.gates.length - 1 ? "Finish" : String(i + 1)));
      tr.appendChild(api.mk("td", null, fmtMs(ms)));
      tr.appendChild(api.mk("td", d == null ? null : d <= 0 ? "good" : "bad", d == null ? "–" : fmtDelta(d)));
      tb.appendChild(tr);
    });
    var wrapT = api.mk("div", "vg-rally-splitwrap"); wrapT.appendChild(tb); return wrapT;
  }
  function showResults(newBest, prev){
    cardBox.textContent = "";
    var row = api.mk("div", "vg-row");
    if(V.mode === "solo"){
      cardBox.appendChild(api.mk("b", "vg-rally-big", fmtMs(V.finishMs)));
      cardBox.appendChild(api.mk("p", newBest ? "vg-rally-good" : "vg-muted", newBest ? (prev ? "New best! " + fmtDelta(V.finishMs - prev) + " on " + fmtMs(prev) : (V.ghostSaved ? "First time on this stage: your ghost is saved" : "First time on this stage")) : "Best " + fmtMs(prev) + " (" + fmtDelta(V.finishMs - prev) + ")"));
      cardBox.appendChild(splitsTable());
      if(V.event){
        var done = V.event.index === V.event.list.length - 1, tot = V.event.times.reduce(function(a, b){ return a + (b || 0); }, 0);
        cardBox.appendChild(api.mk("p", "vg-muted", "Event: stage " + (V.event.index + 1) + " of " + V.event.list.length + " · total " + fmtMs(tot)));
        if(!done) row.appendChild(api.btn("Next stage →", "primary", function(){ V.event.index++; var ev = V.event; startSolo(ev.list[ev.index]); V.event = ev; }));
        else row.appendChild(api.btn("Run the event again", "primary", function(){ startEvent(V.event.seed, V.event.list.length); }));
      } else row.appendChild(api.btn("Drive again", "primary", function(){ var i = V.info; startSolo(i); }));
      row.appendChild(api.btn("Stages", "", function(){ resetRun(); showStage(false); renderMenu(); }));
      say("Finished in " + fmtMs(V.finishMs) + (newBest ? ". New best." : ""));
    } else {
      var res = V.results || {rows: []};
      cardBox.appendChild(resultsTable(res, "Results"));
      var ev = res.event || V.roomEvent;
      if(isHost()){
        if(ev && ev.count > 1 && ev.index < ev.count - 1 && V.mpEvent){
          row.appendChild(api.btn("Next stage →", "primary", function(){ var me2 = V.mpEvent; me2.index = (ev.index|0) + 1; resetRun(); showStage(false); renderMenu(); startRoomStage(me2.list[me2.index], {index: me2.index, count: me2.count}); }));
        }
        row.appendChild(api.btn("Pick the next stage", ev && ev.count > 1 ? "" : "primary", function(){ resetRun(); showStage(false); renderMenu(); }));
      } else row.appendChild(api.btn("Back to the lobby", "", function(){ resetRun(); showStage(false); renderMenu(); }));
      var mine = (res.rows || []).filter(function(r){ return r.uid === V.myId(); })[0];
      if(mine) say(mine.ms == null ? "Stage over: did not finish" : "Stage over: place " + mine.place + ", " + fmtMs(mine.ms));
    }
    cardBox.appendChild(row); cardBox.classList.remove("hidden");
    var b = cardBox.querySelector("button"); if(b) b.focus();
  }
  function togglePause(on){
    if(V.phase === "menu" || !V.stage) return;
    V.paused = !!on;
    pauseBox.textContent = "";
    pauseBox.classList.toggle("hidden", !on);
    if(!on){ if(canvas) canvas.focus(); return; }
    var box = api.mk("div", "vg-rally-pausebox");
    box.appendChild(api.mk("b", null, V.mode === "mp" ? "Menu (the stage keeps running)" : "Paused"));
    var row = api.mk("div", "vg-row");
    row.appendChild(api.btn("Resume", "primary", function(){ togglePause(false); }));
    if(V.mode === "solo") row.appendChild(api.btn("Restart stage", "", function(){ togglePause(false); var ev = V.event, i = V.info; startSolo(i); V.event = ev; }));
    row.appendChild(api.btn(V.mode === "mp" ? "Leave the stage" : "Quit to stages", "", function(){ togglePause(false); quitStage(); }));
    box.appendChild(row);
    settingsDialog(box);
    pauseBox.appendChild(box);
    var b = box.querySelector("button"); if(b) b.focus();
  }
  function cdShow(t){ E.sfx(t === "GO!" ? "go" : /^\d$/.test(t) ? "count" : "finish"); cdBox.textContent = t; cdBox.classList.remove("hidden"); cdBox.dataset.until = String(now() + 1.1); }

  /* ---------- audio: engine note, tyre scrub, gravel ---------- */
  function audioUpdate(C){
    if(!C) return;
    var kind = C.spec.eng || "turbo";
    if(eng && V.engKind !== kind){ eng.stop(); eng = null; }
    if(!eng){ eng = E.engineSound(kind); V.engKind = kind; }
    var lim = C.spec.engine.limiter;
    eng.set(900 + (C.rpm - C.spec.engine.idle)/(lim - C.spec.engine.idle)*6700);
    if(E.noiseLoop){
      if(!skid) skid = E.noiseLoop(1400, 2);
      if(!gravel) gravel = E.noiseLoop(380, 0.8);
      var T = C.tel, tar = T.surface === 0, slip = Math.max(0, T.slip - 1.05);
      skid.set(tar ? Math.min(0.09, slip*0.05) : Math.min(0.04, slip*0.02), tar ? 1300 : 900);
      gravel.set(!tar && !T.air ? Math.min(0.08, T.rumble*Math.min(1, T.speed/20)*0.1 + slip*0.01) : 0, 300 + T.speed*8);
    }
  }

  /* ---------- HUD ---------- */
  var standDirty = true, hudAt = 0, gaugeAt = 0;
  function hudUpdate(){
    if(!V.stage || !V.car) return;
    var st = V.stage, C = V.car, ms = V.phase === "race" || V.phase === "done" ? (V.finishMs != null ? V.finishMs : V.raceT*1000) : 0;
    var tt = fmtMs(ms); if(hudTime.textContent !== tt) hudTime.textContent = tt;
    var gt = V.finishMs != null ? "FINISHED" : "GATE " + Math.min(st.gates.length, V.gate + 1) + " OF " + st.gates.length;
    if(V.event) gt = "STAGE " + (V.event.index + 1) + "/" + V.event.list.length + " · " + gt;
    if(hudGate.textContent !== gt) hudGate.textContent = gt;
    var L = st.locate(C.p[0], C.p[2], V.hint), left = Math.max(0, st.length - L.s);
    var dt2 = V.finishMs != null ? "Stage complete" : left >= 1000 ? (left/1000).toFixed(2) + " km to finish" : Math.round(left) + " m to finish";
    if(hudDist.textContent !== dt2) hudDist.textContent = dt2;
    if(V.mode === "mp" && standDirty){
      standDirty = false; standBox.textContent = "";
      var me = V.myId(), list = V.lobby.map(function(p){ var pr = V.prog[p.uid] || {gate: -1, ms: 0}; return {uid: p.uid, name: nameOf(p), car: p.car|0, pr: pr}; });
      list.sort(function(a, b){
        var fa = a.pr.fin != null, fb = b.pr.fin != null;
        if(fa !== fb) return fa ? -1 : 1;
        if(fa) return a.pr.fin - b.pr.fin;
        return (b.pr.gate - a.pr.gate) || (a.pr.ms - b.pr.ms);
      });
      list.forEach(function(e, i){
        var li = api.mk("li", e.uid === me ? "me" : null), sw = api.mk("i", "vg-kart-sw"); sw.style.background = CAR_SWATCH[clamp(e.car, 0, 2)];
        li.appendChild(sw);
        var lead = V.leaderAt[e.pr.gate], gap = e.pr.fin != null ? fmtMs(e.pr.fin) : e.pr.dnf ? "out" : e.pr.gate >= 0 ? "G" + (e.pr.gate + 1) + (lead != null && e.pr.ms > lead ? " " + fmtDelta(e.pr.ms - lead) : "") : "–";
        li.appendChild(document.createTextNode(" " + (i + 1) + ". " + e.name + "  " + gap));
        standBox.appendChild(li);
      });
    }
    standBox.classList.toggle("hidden", V.mode !== "mp");
  }
  function sizeCanvas(cv, W, H){
    var dpr = Math.min(window.devicePixelRatio || 1, 2);
    if(cv.width !== Math.round(W*dpr) || cv.height !== Math.round(H*dpr)){ cv.width = Math.round(W*dpr); cv.height = Math.round(H*dpr); }
    var g = cv.getContext("2d"); g.setTransform(dpr, 0, 0, dpr, 0, 0); g.clearRect(0, 0, W, H); return g;
  }
  function drawGauge(){
    var C = V.car; if(!C) return;
    var W = 210, H = 170, g = sizeCanvas(gauge, W, H), T = TOK, tel = C.tel;
    var spd = V.units === "mph" ? tel.speed*2.23694 : tel.speed*3.6, max = V.units === "mph" ? 140 : 220;
    var cx = 108, cy = 98, r = 76, a0 = Math.PI*0.78, a1 = Math.PI*2.22;
    function ang(v, m){ return a0 + (a1 - a0)*clamp(v/m, 0, 1); }
    g.beginPath(); g.arc(cx, cy, r + 8, 0, Math.PI*2); g.fillStyle = T.panel; g.globalAlpha = 0.86; g.fill(); g.globalAlpha = 1;
    g.lineWidth = 1.5; g.strokeStyle = T.line; g.stroke();
    // speed ticks
    g.lineCap = "round";
    var major = V.units === "mph" ? 20 : 20;
    for(var v = 0; v <= max + 1e-6; v += major/2){
      var isMaj = v % major === 0, a = ang(v, max), r1 = r + 2, r2 = r - (isMaj ? 10 : 6);
      g.beginPath(); g.moveTo(cx + Math.cos(a)*r1, cy + Math.sin(a)*r1); g.lineTo(cx + Math.cos(a)*r2, cy + Math.sin(a)*r2);
      g.lineWidth = isMaj ? 2 : 1; g.strokeStyle = T.muted; g.stroke();
      if(isMaj && v % (major*2) === 0){ g.fillStyle = T.muted; g.font = "600 9px " + T.mono; g.textAlign = "center"; g.textBaseline = "middle"; g.fillText(String(v), cx + Math.cos(a)*(r - 19), cy + Math.sin(a)*(r - 19)); }
    }
    // RPM arc inside, red from the shift point
    var e = C.spec.engine, rmax = Math.ceil(e.limiter/1000)*1000, rr = r - 30;
    g.beginPath(); g.arc(cx, cy, rr, a0, a1); g.lineWidth = 7; g.strokeStyle = T.bg2; g.stroke();
    g.beginPath(); g.arc(cx, cy, rr, ang(C.spec.shiftUp, rmax), a1); g.lineWidth = 7; g.strokeStyle = T.need; g.globalAlpha = 0.45; g.stroke(); g.globalAlpha = 1;
    g.beginPath(); g.arc(cx, cy, rr, a0, ang(C.rpm, rmax)); g.lineWidth = 7; g.strokeStyle = C.rpm > C.spec.shiftUp ? T.need : T.brand; g.stroke();
    // gear (above the pivot), big digital speed (below it), then the needle over both
    var gear = C.gear < 0 ? "R" : C.shiftT > 0 ? String(C.pend > 0 ? C.pend : "N") : String(C.gear);
    g.fillStyle = T.panel2; g.fillRect(cx - 15, cy - 44, 30, 30); g.strokeStyle = T.line; g.lineWidth = 1; g.strokeRect(cx - 15, cy - 44, 30, 30);
    g.fillStyle = C.shiftT > 0 ? T.muted : T.ink; g.font = "800 22px " + T.mono; g.textAlign = "center"; g.textBaseline = "middle"; g.fillText(gear, cx, cy - 28);
    g.fillStyle = T.ink; g.font = "800 30px " + T.mono; g.textBaseline = "alphabetic";
    g.fillText(String(Math.round(spd)), cx, cy + 38);
    g.fillStyle = T.muted; g.font = "600 10px " + T.mono; g.fillText(V.units === "mph" ? "MPH" : "KM/H", cx, cy + 51);
    g.font = "600 9px " + T.mono; g.fillText("RPM ×1000  " + (C.rpm/1000).toFixed(1), cx, cy + 63);
    var an = ang(spd, max);
    g.beginPath(); g.moveTo(cx - Math.cos(an)*8, cy - Math.sin(an)*8); g.lineTo(cx + Math.cos(an)*(r - 4), cy + Math.sin(an)*(r - 4));
    g.lineWidth = 3; g.strokeStyle = T.need; g.stroke();
    g.beginPath(); g.arc(cx, cy, 5, 0, Math.PI*2); g.fillStyle = T.ink; g.fill();
  }
  function drawBars(){
    var C = V.car; if(!C) return;
    var W = 170, H = 54, g = sizeCanvas(bars, W, H), T = TOK, tel = C.tel;
    function bar(y, label, v, col, centred){
      g.fillStyle = T.muted; g.font = "600 9px " + T.mono; g.textAlign = "left"; g.textBaseline = "middle"; g.fillText(label, 0, y + 4);
      g.fillStyle = T.bg2; g.fillRect(44, y, 120, 8);
      g.fillStyle = col;
      if(centred){ var w = v*60; g.fillRect(104 + Math.min(0, w), y, Math.abs(w), 8); g.fillStyle = T.ink; g.fillRect(103.5, y - 1, 1, 10); }
      else g.fillRect(44, y, 120*clamp(v, 0, 1), 8);
    }
    bar(2, "STEER", clamp(C.steerIn, -1, 1), T.brand, true);
    bar(16, "THROT", tel.throttle, T.good, false);
    bar(30, "BRAKE", tel.brake, T.need, false);
    g.fillStyle = tel.hand ? T.need : T.muted; g.font = "700 9px " + T.mono; g.fillText(tel.hand ? "HANDBRAKE" : (tel.drift ? "DRIFT " + Math.round(Math.abs(tel.beta)*57.3) + "°" : (P.SURFACES[tel.surface] || {}).name || ""), 44, 48);
  }
  // the next stretch of road, straightened out ahead of the car (travel to the right)
  function drawStrip(){
    var st = V.stage, C = V.car; if(!st || !C) return;
    var W = 300, H = 58, g = sizeCanvas(strip, W, H), T = TOK;
    var L = st.locate(C.p[0], C.p[2], V.hint), s0 = L.s, AHEAD = 600, sx = (W - 30)/AHEAD;
    g.fillStyle = T.panel; g.globalAlpha = 0.82; g.fillRect(0, 0, W, H); g.globalAlpha = 1;
    // project the road ahead into the car's heading frame at the strip's scale
    var P0 = st.pointAtS(s0), h0 = P0.h, ch = Math.cos(h0), sh = Math.sin(h0);
    var maxA = 30, pts = [];
    for(var d = 0; d <= AHEAD; d += 10){
      var p = st.pointAtS(s0 + d), dx = p.x - P0.x, dz = p.z - P0.z;
      var along = dx*sh - dz*ch, lat = dx*ch + dz*sh;
      pts.push([along, lat]); maxA = Math.max(maxA, Math.abs(lat));
      if(s0 + d >= st.length) break;
    }
    var sy = (H/2 - 7)/maxA;
    g.beginPath();
    pts.forEach(function(q, i){ var x = 18 + q[0]*sx, y = H/2 + q[1]*sy; if(i) g.lineTo(x, y); else g.moveTo(x, y); });
    g.lineJoin = "round"; g.lineCap = "round"; g.lineWidth = 6; g.strokeStyle = T.line; g.stroke(); g.lineWidth = 2.5; g.strokeStyle = T.ink; g.stroke();
    // gates ahead (the next one in the accent colour, the finish in gold)
    st.gates.forEach(function(gt, i){
      if(i < V.gate) return;
      var gs = st.gateS[i], d2 = gs - s0; if(d2 < 0 || d2 > AHEAD) return;
      var p = st.pointAtS(gs), dx = p.x - P0.x, dz = p.z - P0.z, x = 18 + (dx*sh - dz*ch)*sx, y = H/2 + (dx*ch + dz*sh)*sy;
      g.fillStyle = i === st.gates.length - 1 ? T.gold : i === V.gate ? T.brand : T.muted;
      g.fillRect(x - 1.5, y - 7, 3, 14);
    });
    // the car, and how far off the line it is
    var cy = H/2 + clamp(L.lat, -maxA, maxA)*sy;
    g.beginPath(); g.arc(18, cy, 4.5, 0, Math.PI*2); g.fillStyle = T.need; g.fill(); g.lineWidth = 1.5; g.strokeStyle = T.ink; g.stroke();
    g.fillStyle = T.muted; g.font = "600 8px " + T.mono; g.textAlign = "right"; g.textBaseline = "top"; g.fillText(AHEAD + " m", W - 4, 3);
  }

  /* ---------- renderer ---------- */
  var renderWait = [];
  function whenRenderer(fn){ if(R3 || R2) fn(); else renderWait.push(fn); }
  var R2 = null;
  function ensureRenderer(){
    if(canvas) return;
    wrap.textContent = "";
    [hudTop, hudBL, gauge, standBox, cdBox, cardBox, pauseBox, badge, load].forEach(function(n){ wrap.appendChild(n); });
    canvas = api.mk("canvas", "vg-golf-canvas vg-rally-canvas"); canvas.tabIndex = 0;
    canvas.setAttribute("aria-label", "Rally stage. W or up arrow for throttle, S to brake and reverse, A and D steer, Space is the handbrake, R goes back to the last gate, C changes the camera, Escape pauses.");
    wrap.insertBefore(canvas, wrap.firstChild);
    if(IN) IN.destroy();
    // keys count only while the stage canvas has focus, so the pause menu and the results card keep Enter, Space and Esc
    IN = HQV.input ? HQV.input.create(canvas, {buttons: RALLY_BUTTONS}) : null;
    canvas.addEventListener("pointerdown", function(){ canvas.focus(); });
    resize();
    if(hasWebGL2()){
      load.classList.remove("hidden"); loadFill.style.width = "15%";
      E.lib().then(function(lib){
        if(!V.alive) return;
        R3 = HQV.rallyScene.create(lib.THREE, canvas, {low: V.low});
        resize();
        var w = renderWait; renderWait = []; w.forEach(function(f){ f(); });
      }, function(){
        if(!V.alive) return;
        V.note = "3D unavailable: showing the map view."; api.toast(V.note); R2 = make2d(canvas);
        var w = renderWait; renderWait = []; w.forEach(function(f){ f(); });
      });
    } else { R2 = make2d(canvas); }
    if(!raf) raf = requestAnimationFrame(frame);
  }
  function resize(){
    if(!canvas) return;
    var w = Math.max(300, wrap.clientWidth || host.clientWidth || 800), h = Math.round(Math.min(w*0.58, (window.innerHeight || 800)*0.74));
    canvas.style.height = h + "px";
    if(R3) R3.size(w, h);
    else if(R2){ var dpr = Math.min(window.devicePixelRatio || 1, 2); canvas.width = Math.round(w*dpr); canvas.height = Math.round(h*dpr); }
  }
  // 2D map view (no WebGL2): north-up, centred on the car
  function make2d(cv){
    var R = {};
    R.render = function(){
      var g = cv.getContext("2d"), W = cv.width, H = cv.height, T = TOK;
      g.setTransform(1, 0, 0, 1, 0, 0); g.fillStyle = T.bg2; g.fillRect(0, 0, W, H);
      if(!V.stage || !V.car) return;
      var sc = Math.min(W, H)/120, C = V.car;
      g.save(); g.translate(W/2, H/2); g.scale(sc, sc); g.translate(-C.p[0], -C.p[2]);
      g.beginPath(); V.stage.pts.forEach(function(p, i){ if(i % 2) return; if(i) g.lineTo(p.x, p.z); else g.moveTo(p.x, p.z); });
      g.lineJoin = "round"; g.lineWidth = 8; g.strokeStyle = T.line; g.stroke(); g.lineWidth = 7; g.strokeStyle = T.panel2; g.stroke();
      V.stage.gates.forEach(function(gt, i){ var h = gt.h*Math.PI/180; g.beginPath(); g.moveTo(gt.x - Math.cos(h)*gt.w, gt.z - Math.sin(h)*gt.w); g.lineTo(gt.x + Math.cos(h)*gt.w, gt.z + Math.sin(h)*gt.w); g.lineWidth = 0.6; g.strokeStyle = i === V.gate ? T.brand : T.muted; g.stroke(); });
      function car(x, z, yaw, col){ g.save(); g.translate(x, z); g.rotate(yaw); g.fillStyle = col; g.fillRect(-0.85, -2, 1.7, 4); g.fillStyle = T.ink; g.fillRect(-0.7, -2, 1.4, 0.6); g.restore(); }
      Object.keys(V.others).forEach(function(u){ var o = V.others[u]; car(o.x, o.z, o.yaw, T.muted); });
      car(C.p[0], C.p[2], P.headingOf(C.M), T.need);
      g.restore();
    };
    return R;
  }
  var POSE = {p: [0, 0, 0], q: [0, 0, 0, 1]}, GPOSE = {p: [0, 0, 0], q: [0, 0, 0, 1]}, QT = [0, 0, 0, 1];
  function eulerQuat(yaw, pitch, roll, out){
    // three.js Euler(pitch, -yaw, roll, "YXZ") as a quaternion
    var x = pitch/2, y = -yaw/2, z = roll/2, c1 = Math.cos(x), c2 = Math.cos(y), c3 = Math.cos(z), s1 = Math.sin(x), s2 = Math.sin(y), s3 = Math.sin(z);
    out[0] = s1*c2*c3 + c1*s2*s3; out[1] = c1*s2*c3 - s1*c2*s3; out[2] = c1*c2*s3 - s1*s2*c3; out[3] = c1*c2*c3 + s1*s2*s3;
    return out;
  }
  var FAKE = {wheels: [{len: 0.22, steer: 0, ang: 0}, {len: 0.22, steer: 0, ang: 0}, {len: 0.22, steer: 0, ang: 0}, {len: 0.22, steer: 0, ang: 0}], tel: {brake: 0}};
  function renderScene(dt, t){
    var C = V.car;
    if(!C || !V.stage || !R3 || !R3.stage){ if(R3) R3.renderer.render(R3.scene, R3.camera); return; }
    P.pose(C, POSE);
    if(V.handle) R3.poseCar(V.handle, POSE, C);
    // the solo ghost: your best run, replayed by race time
    if(V.ghost && V.ghost.handle){
      var gd = V.ghost.data, gt = (V.phase === "race" || V.phase === "done") ? V.raceT : 0, f = gt*gd.hz, i0 = Math.floor(f), u = f - i0;
      var A = gd.s[Math.min(gd.s.length - 1, i0)], B = gd.s[Math.min(gd.s.length - 1, i0 + 1)];
      if(i0 >= gd.s.length - 1) u = 0;
      var yaw = A.yaw + Math.atan2(Math.sin(B.yaw - A.yaw), Math.cos(B.yaw - A.yaw))*u;
      GPOSE.p[0] = A.x + (B.x - A.x)*u; GPOSE.p[1] = A.y + (B.y - A.y)*u; GPOSE.p[2] = A.z + (B.z - A.z)*u;
      eulerQuat(yaw, A.pitch + (B.pitch - A.pitch)*u, A.roll + (B.roll - A.roll)*u, GPOSE.q);
      var sp = Math.sqrt(Math.pow(B.x - A.x, 2) + Math.pow(B.z - A.z, 2))*gd.hz;
      FAKE.wheels.forEach(function(w){ w.ang = (w.ang + sp*dt/0.31) % (Math.PI*2); w.len = 0.22; w.steer = 0; });
      R3.poseCar(V.ghost.handle, GPOSE, FAKE);
      V.ghost.handle.g.visible = !(V.phase === "race" && Math.abs(GPOSE.p[0] - C.p[0]) < 0.01 && false);
    }
    // the other drivers, as ghosts
    Object.keys(V.others).forEach(function(u2){
      var o = V.others[u2];
      if(!o.snaps.length) return;
      if(!o.handle){ o.handle = R3.addCar(P.CARS[clamp(o.car|0, 0, P.CARS.length - 1)], {ghost: true, name: o.name}); }
      var n3 = [0, 1, 0]; V.stage.env.normal(o.x, o.z, n3);
      GPOSE.p[0] = o.x; GPOSE.p[1] = o.y; GPOSE.p[2] = o.z;
      eulerQuat(o.yaw, Math.asin(clamp(n3[0]*Math.sin(o.yaw) - n3[2]*Math.cos(o.yaw), -1, 1)), -Math.asin(clamp(n3[0]*Math.cos(o.yaw) + n3[2]*Math.sin(o.yaw), -1, 1)), GPOSE.q);
      FAKE.wheels.forEach(function(w){ w.ang = (o.ang) % (Math.PI*2); });
      R3.poseCar(o.handle, GPOSE, FAKE);
    });
    R3.fx(C, dt, calm());
    R3.render(dt, {mode: V.phase === "done" ? 3 : V.cam, pose: POSE, car: C, calm: calm()});
  }
  var perf = {t: 0, n: 0, slow: 0, pr: null};
  function frame(ts){
    if(!V.alive) return;
    raf = requestAnimationFrame(frame);
    var t = ts/1000, dt = lastT ? clamp(t - lastT, 0, 0.1) : 0; lastT = t;
    try {
      if(t - tokAt > 2){ TOK = tokens(); tokAt = t; }
      update(dt, now());
      if(R3) renderScene(dt, now()); else if(R2) R2.render();
      if(t - hudAt > 0.08){ hudAt = t; hudUpdate(); }
      if(t - gaugeAt > (V.low ? 1/15 : 1/30)){ gaugeAt = t; drawGauge(); drawBars(); drawStrip(); }
      // frame rate, and a lower resolution when frames run long
      V.frames++; perf.n++; perf.t += dt;
      if(perf.t > 2){
        V.fps = perf.n/perf.t; perf.n = 0; perf.t = 0;
        if(R3 && V.fps < 28 && V.car){ perf.pr = Math.max(0.6, (perf.pr || Math.min(window.devicePixelRatio || 1, 1.5)) - 0.2); R3.setPixelRatio(perf.pr); }
      }
    } catch(e){ if(window.console) console.error("rally frame", e); }
  }
  function onResize(){ resize(); }
  window.addEventListener("resize", onResize);
  if(typeof ResizeObserver !== "undefined"){ ro = new ResizeObserver(onResize); ro.observe(host); }
  renderKeys();
  renderMenu();
  loadLibs().then(function(){
    if(!V.alive) return;
    P = HQV.rallyPhysics; S = HQV.rallyStage; SC = HQV.rallyScene;
    renderMenu();
    if(V.queued){ var q = V.queued; V.queued = null; q.forEach(onEvent); }
    if(V.mode === "mp" && MP) MP.send("rally", "view");
    if(opts.doc){ var d = S.cleanDoc(opts.doc); if(d) { V.info = {name: d.name, seed: d.data.seed, biome: d.data.biome, night: d.data.night, length: d.data.length}; resetRun(); V.stage = buildStageFor(V.info, d); V.key = S.localKey(d); beginStage(); } }
  }, function(e){ V.note = "Couldn't load Valley Rally: " + e.message; renderMenu(); });
  if(!raf) raf = requestAnimationFrame(frame);

  V.destroy = function(){
    V.alive = false;
    if(eng){ eng.stop(); eng = null; } if(skid){ skid.stop(); skid = null; } if(gravel){ gravel.stop(); gravel = null; }
    if(raf) cancelAnimationFrame(raf); raf = 0;
    window.removeEventListener("resize", onResize); if(ro) ro.disconnect();
    if(IN){ IN.destroy(); IN = null; }
    if(R3){ R3.dispose(); R3 = null; }
    root.remove();
  };
  V.onEvent = onEvent; V.onError = onError;
  V.onConn = function(on){ V.offline = !on; badge.classList.toggle("hidden", on); if(!on){ V.sv.lastQ = -1; } };
  // for the browser smoke test
  V.startSolo = function(info){ if(!S) return false; startSolo(info || S.PRESETS[0]); return true; };
  // a pure-pursuit driver for the headless test (it only suggests keys; the test presses them)
  V.autopilot = function(){
    if(!V.stage || !V.car) return null;
    var C = V.car, st = V.stage, L = st.locate(C.p[0], C.p[2], V.hint), A = st.pointAtS(L.s + Math.max(9, C.tel.speed*0.85)), kmax = 0;
    for(var q = 0; q < 70; q += 7) kmax = Math.max(kmax, Math.abs(st.pointAtS(L.s + q).k));
    var vT = Math.min(32, Math.sqrt(0.5*9.81*0.75/Math.max(kmax, 1e-3))), fx = -C.M[2], fz = -C.M[8], dx = A.x - C.p[0], dz = A.z - C.p[2];
    var ang = Math.atan2(fx*dz - fz*dx, fx*dx + fz*dz), e = vT - C.tel.speed;
    return {thr: e > -1 ? 1 : 0, brk: e < -3 ? 1 : 0, steer: clamp(ang*2.5, -1, 1), hb: kmax > 1/32 && C.tel.speed > 13 && Math.abs(ang) > 0.25 ? 1 : 0};
  };
  V.scene = function(){ return R3; };      // for the browser smoke test
  // (smoke test) put the car 40 m before gate gi, as if the earlier gates were passed
  V.jumpTo = function(gi){
    if(!V.stage || !V.car || V.mode !== "solo") return false;
    var st = V.stage; gi = clamp(gi|0, 0, st.gates.length - 1);
    for(var i = V.gate; i < gi; i++) V.splits[i] = Math.round(V.raceT*1000*(i + 1)/gi);
    V.gate = gi; V.rec = null; var p = st.pointAtS(Math.max(0, st.gateS[gi] - 40));
    P.place(V.car, p.x, p.z, p.h, st.env); V.hint = st.locate(p.x, p.z, null).i;
    return true;
  };
  V.debug = function(){ return {phase: V.phase, gate: V.gate, gates: V.stage ? V.stage.gates.length : 0, fps: V.fps, raceMs: V.raceT*1000, speed: V.car ? V.car.tel.kmh : 0,
    drift: V.car ? V.car.tel.drift : false, surface: V.car ? V.car.tel.surface : -1, renderer: R3 ? "3d" : R2 ? "2d" : "", info: R3 ? R3.info() : null, finishMs: V.finishMs}; };
  return V;
}

/* ---------- registration: a solo card and a room card, like Kart Racing ---------- */
HQV.register({id: "rally", name: "Valley Rally", icon: "🏁", desc: "Off-road stages against the clock and your ghost: gravel, snow, jumps",
  badge: function(){ var s = api.save && api.save.rally, n = s && s.best ? Object.keys(s.best).length : 0; return n ? n + " stage" + (n === 1 ? "" : "s") + " timed" : ""; },
  mount: function(el){ if(CUR) CUR.destroy(); CUR = makeGame(el, {mode: "solo"}); },
  unmount: function(){ if(CUR){ CUR.destroy(); CUR = null; } },
  pause: function(){ if(CUR && CUR.mode === "solo") CUR.paused = true; },
  resume: function(){ if(CUR) CUR.paused = false; }});

if(MP){
  MP.handlers.rally = {
    on: function(m, s){
      // the shared lobby panel reads s.lobby as {userId, displayName, host}
      if(m.ev === "lobby" && Array.isArray(m.players)){
        s.lobby = m.players.map(function(p){ return {userId: p.uid, displayName: p.name, host: p.uid === m.host, car: p.car|0, ready: !!p.ready}; });
      }
      if(CUR && CUR.ctx) CUR.onEvent(m);
    },
    onError: function(m){ return CUR && CUR.ctx ? CUR.onError(m) : false; },
    render: function(){}
  };
  MP.register("rally", "🏁", "Race up to 8 friends stage by stage: the Arena times every gate", function(ctx){
    if(CUR) CUR.destroy();
    var g = CUR = makeGame(ctx.box, {mode: "mp", ctx: ctx});
    ctx.onConn = function(on){ if(g.alive) g.onConn(on); };
    ctx.onRejoin = function(){ if(g.alive && MP) MP.send("rally", "view"); };
    ctx.stop = function(){ g.destroy(); if(CUR === g) CUR = null; };
  });
}
HQV.rallyDebug = function(){ return CUR; };
})();
