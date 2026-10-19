/* Bowling (HQ 2.5): ten-pin on the Mini Golf physics model, with your buddy Pokémon bowling.
 *
 * Solo in the Valley (card "bowl") rolls right here; in an Arena room (card "mp-bowl", up to
 * 8) the Rust Arena referees every ball (backend/src/valley/bowling.rs): it simulates the
 * roll from {x, aim, power, spin}, keeps the turn order and the score, and sends 'bowl' (the
 * round view), 'roll', 'frame', 'char' and 'done'. The physics is integer-only and lives in
 * the BOWL-SIM block below, mirrored constant for constant from bowling.rs; every browser
 * replays the same roll from the inputs only to animate it, so everyone sees the same pins
 * (tests/test_bowl_sync.py runs this block against backend/tests/bowl_golden.json).
 *
 * The lane is three.js with procedural pins, or a top-down 2D canvas without WebGL2. The
 * bowler is your buddy Pokémon (HQV.pk.spriteEl, whose onerror chain ends at
 * window.paintCreature); the ball wears your 'ball' cosmetic, and the Poké Ball one looks
 * like a Poké Ball. Keyboard: arrows move and aim, Space/Enter runs the aim, power and spin
 * meters and rolls. Under calm the meters hold still, Space rolls at once and the pins are
 * shown where they fell instead of scattering. Every ball and frame is also announced.
 */
(function(){
"use strict";
var HQV = window.HQV; if(!HQV || !HQV.api || !HQV.engine) return;
var api = HQV.api, E = HQV.engine, MP = HQV.mp || null;

/* BOWL-SIM BEGIN */
var VS = 256;
var TICK = 120;
var HALF_W = 5270;
var BR = 1090;
var PR = 610;
var HEAD_Z = 182900;
var PIN_DX = 1524;
var ROW_DZ = 2640;
var PIT_Z = 197820;
var V_MIN = 120000;
var V_MAX = 220000;
var AIM_MAX = 100;
var AIM_DIV = 1500;
var SPIN_MAX = 100;
var HOOK = 300;
var OIL_DIV = 50;
var OIL = [110000, 125000, 140000];
var BALL_DRAG = 6;
var V_STALL = 5000;
var BALL_M = 14;
var PIN_M = 3;
var EN = 3;
var ED = 4;
var PIN_DRAG = 12;
var PIN_STOP = 600;
var FALL_R = 900;
var PIN_TIP = 2000;
var MAX_TICKS = 900;
var SAMPLE = 4;
var X_MAX = HALF_W - BR;
var PINS = [[0, HEAD_Z], [-PIN_DX, HEAD_Z + ROW_DZ], [PIN_DX, HEAD_Z + ROW_DZ],
  [-2*PIN_DX, HEAD_Z + 2*ROW_DZ], [0, HEAD_Z + 2*ROW_DZ], [2*PIN_DX, HEAD_Z + 2*ROW_DZ],
  [-3*PIN_DX, HEAD_Z + 3*ROW_DZ], [-PIN_DX, HEAD_Z + 3*ROW_DZ], [PIN_DX, HEAD_Z + 3*ROW_DZ], [3*PIN_DX, HEAD_Z + 3*ROW_DZ]];
function tdiv(a, b){ return Math.trunc(a/b); }
function isqrt(n){ if(n <= 0) return 0; var x = n, y = Math.floor((x + 1)/2); while(y < x){ x = y; y = Math.floor((x + Math.floor(n/x))/2); } return x; }
// One ball, statement for statement as bowling.rs's simulate_roll. rec (optional) sees every
// tick for the animation only and never changes the roll.
function simulateRoll(lane, x0, aim, power, spin, oil, rec){
  var oilLen = OIL[Math.min(oil, OIL.length - 1)], pins = [], i, j;
  for(i = 0; i < 10; i++) pins.push({x:PINS[i][0], z:PINS[i][1], vx:0, vz:0, down:false, off:!lane.standing[i]});
  var bx = x0, bz = 0, bvz = V_MIN + tdiv((power - 1)*(V_MAX - V_MIN), 99), bvx = tdiv(bvz*aim, AIM_DIV);
  var alive = true, gutter = false, path = [[bx, bz]], rbp = BR + PR, rpp = PR + FALL_R, ticks = MAX_TICKS;
  for(var t = 1; t <= MAX_TICKS; t++){
    if(alive){
      var hook = bz > oilLen ? tdiv(spin*HOOK, 100) : tdiv(spin*HOOK, 100*OIL_DIV);
      if(!gutter) bvx += hook;
      bvx -= tdiv(bvx*BALL_DRAG, 10000);
      bvz -= tdiv(bvz*BALL_DRAG, 10000);
      bx += tdiv(bvx, VS);
      bz += tdiv(bvz, VS);
      if(!gutter){
        if(lane.bumpers){
          var lim = HALF_W - BR;
          if(bx > lim){ bx = 2*lim - bx; bvx = -tdiv(bvx*EN, ED); }
          else if(bx < -lim){ bx = -2*lim - bx; bvx = -tdiv(bvx*EN, ED); }
        } else if(bx > HALF_W || bx < -HALF_W){
          gutter = true; bx = bx > 0 ? HALF_W + BR : -HALF_W - BR; bvx = 0;
        }
      }
      if(bz > PIT_Z + BR || bvz < V_STALL) alive = false;
    }
    for(i = 0; i < 10; i++){
      var p = pins[i];
      if(p.off || (p.vx === 0 && p.vz === 0)) continue;
      p.x += tdiv(p.vx, VS); p.z += tdiv(p.vz, VS);
      p.vx -= tdiv(p.vx*PIN_DRAG, 1000); p.vz -= tdiv(p.vz*PIN_DRAG, 1000);
      if(Math.abs(p.vx) + Math.abs(p.vz) < PIN_STOP){ p.vx = 0; p.vz = 0; }
      if(p.z > PIT_Z || p.x > HALF_W || p.x < -HALF_W){ p.off = true; p.vx = 0; p.vz = 0; }
    }
    if(alive && !gutter){
      for(i = 0; i < 10; i++){
        var q = pins[i];
        if(q.off) continue;
        var dx = q.x - bx, dz = q.z - bz, d2 = dx*dx + dz*dz;
        if(d2 >= rbp*rbp) continue;
        var d = Math.max(isqrt(d2), 1), vn = tdiv((bvx - q.vx)*dx + (bvz - q.vz)*dz, d);
        if(vn > 0){
          var imp = (EN + ED)*vn, dp = tdiv(imp*BALL_M, ED*(BALL_M + PIN_M)), db = tdiv(imp*PIN_M, ED*(BALL_M + PIN_M));
          q.vx += tdiv(dp*dx, d); q.vz += tdiv(dp*dz, d);
          bvx -= tdiv(db*dx, d); bvz -= tdiv(db*dz, d);
          q.down = true;
        }
        if(q.down){ q.x = bx + tdiv(dx*rbp, d); q.z = bz + tdiv(dz*rbp, d); }
      }
    }
    for(i = 0; i < 10; i++){
      for(j = i + 1; j < 10; j++){
        var a = pins[i], b = pins[j];
        if(a.off || b.off || (!a.down && !b.down)) continue;
        var ex = b.x - a.x, ez = b.z - a.z, e2 = ex*ex + ez*ez;
        if(e2 >= rpp*rpp) continue;
        var e = Math.max(isqrt(e2), 1), wn = tdiv((a.vx - b.vx)*ex + (a.vz - b.vz)*ez, e);
        if(wn > 0){
          var half = tdiv((EN + ED)*wn, 2*ED);
          if(half >= PIN_TIP || (a.down && b.down)){
            a.vx -= tdiv(half*ex, e); a.vz -= tdiv(half*ez, e);
            b.vx += tdiv(half*ex, e); b.vz += tdiv(half*ez, e);
            a.down = true; b.down = true;
          } else {
            var full = tdiv((EN + ED)*wn, ED);
            if(a.down){ a.vx -= tdiv(full*ex, e); a.vz -= tdiv(full*ez, e); }
            else { b.vx += tdiv(full*ex, e); b.vz += tdiv(full*ez, e); }
          }
        }
        var ov = rpp - e;
        if(a.down && b.down){
          var h = Math.trunc(ov/2) + 1;
          a.x -= tdiv(ex*h, e); a.z -= tdiv(ez*h, e);
          b.x += tdiv(ex*h, e); b.z += tdiv(ez*h, e);
        } else if(a.down){ a.x -= tdiv(ex*ov, e); a.z -= tdiv(ez*ov, e); }
        else { b.x += tdiv(ex*ov, e); b.z += tdiv(ez*ov, e); }
      }
    }
    if(t % SAMPLE === 0) path.push([bx, bz]);
    if(rec) rec(t, bx, bz, pins, alive);
    var still = true;
    for(i = 0; i < 10; i++) if(!(pins[i].off || (pins[i].vx === 0 && pins[i].vz === 0))) still = false;
    if(!alive && still){ ticks = t; break; }
  }
  var last = path[path.length - 1];
  if(last[0] !== bx || last[1] !== bz) path.push([bx, bz]);
  var down = [];
  for(i = 0; i < 10; i++) down.push(!!lane.standing[i] && (pins[i].down || pins[i].off));
  return {path:path, pinsDown:down, gutter:gutter, ticks:ticks};
}
// Standard ten-pin scoring, as bowling.rs's score_card: frames is every frame (unplayed ones
// empty); the running total per frame, null from the first frame not yet decided.
function scoreCard(frames){
  var n = frames.length, out = [], total = 0, i;
  for(i = 0; i < n; i++) out.push(null);
  function bonus(i, k){
    var next = [];
    for(var j = i + 1; j < n && next.length < k; j++) for(var q = 0; q < frames[j].length && next.length < k; q++) next.push(frames[j][q]);
    return next.length === k ? next.reduce(function(s, x){ return s + x; }, 0) : null;
  }
  for(i = 0; i < n; i++){
    var f = frames[i], s = null, two = (f.length > 0 ? f[0] : 0) + (f.length > 1 ? f[1] : 0);
    if(i + 1 === n){
      if(f.length === 3 || (f.length === 2 && two < 10)) s = f.reduce(function(a, x){ return a + x; }, 0);
    } else if(f[0] === 10){
      var b2 = bonus(i, 2); if(b2 !== null) s = 10 + b2;
    } else if(f.length >= 2){
      if(two === 10){ var b1 = bonus(i, 1); if(b1 !== null) s = 10 + b1; } else s = two;
    }
    if(s === null) break;
    total += s; out[i] = total;
  }
  return out;
}
/* BOWL-SIM END */

var POKEBALL = "#e3350d";          // the b-pokeball cosmetic's value
var M = 10000;                     // units per metre (golf's TILE)
var DEFAULT_DEX = 25;

function fullRack(){ return [true, true, true, true, true, true, true, true, true, true]; }
function note(ev){ try { if(HQV.story && HQV.story.note) HQV.story.note(ev); } catch(e){} }
function clamp(v, a, b){ return v < a ? a : v > b ? b : v; }
function totalOf(rolls){ var sc = scoreCard(rolls), t = 0; sc.forEach(function(s){ if(s != null) t = s; }); return t; }
function keyOf(frames, bumpers){ return "f" + frames + (bumpers ? "b" : ""); }

/* ------------------------------ the rules ------------------------------ */
// One ball into a player's card, as bowling.rs's Bowling::roll (used solo; the Arena does it in a room).
function newCard(frames){ var r = []; for(var i = 0; i < frames; i++) r.push([]); return {rolls:r, standing:fullRack(), done:0, strikes:0, spares:0}; }
function applyRoll(p, nFrames, res){
  var frame = p.done, ball = p.rolls[frame].length, last = frame + 1 === nFrames, before = p.standing.slice(), fell = 0;
  res.pinsDown.forEach(function(d){ if(d) fell++; });
  p.standing = before.map(function(s, i){ return s && !res.pinsDown[i]; });
  var full = before.every(function(s){ return s; }), cleared = p.standing.every(function(s){ return !s; });
  var strike = full && cleared, spare = !full && cleared;
  if(strike) p.strikes++;
  if(spare) p.spares++;
  p.rolls[frame].push(fell);
  var complete = !last ? (strike || ball >= 1) : (ball >= 2 || (ball === 1 && !cleared && p.rolls[frame][0] !== 10));
  if(complete){ p.done++; p.standing = fullRack(); } else if(cleared) p.standing = fullRack();
  return {frame:frame, ball:ball, fell:fell, strike:strike, spare:spare, complete:complete, gutter:res.gutter, standing:before};
}
// What a score sheet shows for one frame's balls.
function marks(f, last){
  var out = [], i;
  if(!last){
    if(f[0] === 10) return ["", "X"];
    for(i = 0; i < f.length; i++) out.push(i === 1 && f[0] + f[1] === 10 ? "/" : f[i] === 0 ? "-" : String(f[i]));
    return out;
  }
  var fresh = true, prev = 0;
  for(i = 0; i < f.length; i++){
    var v = f[i];
    if(fresh && v === 10){ out.push("X"); continue; }
    if(!fresh && prev + v === 10){ out.push("/"); fresh = true; prev = 0; continue; }
    out.push(v === 0 ? "-" : String(v));
    if(fresh){ fresh = false; prev = v; } else { fresh = true; prev = 0; }
  }
  return out;
}
function rollWords(r){ return r.strike ? "Strike!" : r.spare ? "Spare!" : r.gutter ? "Gutter ball" : r.fell === 1 ? "1 pin" : r.fell + " pins"; }

/* ------------------------------ the save ------------------------------ */
function bowlSave(){
  var s = api.save; if(!s) return {best:{}, games:0, strikes:0};
  if(!s.bowl || typeof s.bowl !== "object") s.bowl = {best:{}, games:0, strikes:0};
  if(!s.bowl.best || typeof s.bowl.best !== "object") s.bowl.best = {};
  s.bowl.games = s.bowl.games|0; s.bowl.strikes = s.bowl.strikes|0;
  return s.bowl;
}
function recordGame(key, score, strikes){
  var sv = bowlSave();
  sv.games += 1; sv.strikes += strikes|0;
  if(!(sv.best[key] >= score)) sv.best[key] = score;
  api.persist();
  note("bowl-game");
}

/* --------------------------- Pokémon bowlers --------------------------- */
function pkd(){ return HQV.pk && HQV.pk.data && HQV.pk.data() ? HQV.pk.data() : null; }
// A sprite-ready creature for a national dex number (base form), or null.
function monOfDex(dex){
  var d = pkd(); if(!d || !d.byDex) return null;
  var pid = d.byDex[String(dex)], e = pid != null ? d.pokemon[pid] : null; if(!e) return null;
  var sp = -1, st = 0;
  d.lines.forEach(function(l, i){ if(sp < 0 && l.indexOf(dex) >= 0) sp = i; });
  if(sp >= 0 && HQV.pk.evoPos){ var pos = d.lines[sp].indexOf(dex); for(var s = 0; s <= 4; s++) if(HQV.pk.evoPos(s, d.lines[sp].length) === pos){ st = s; break; } }
  return {name:e.name, id:pid, dex:dex, sprite: e.sprite != null ? e.sprite : null, mega:null, shiny:false, sp:Math.max(0, sp), st:st};
}
function bowlerEl(dex){
  var mon = monOfDex(dex);
  if(mon && HQV.pk && HQV.pk.spriteEl){ var el = HQV.pk.spriteEl(mon, true); el.classList.add("bowl-spr"); return el; }
  var box = api.mk("div", "bowl-spr"); box.setAttribute("aria-hidden", "true");
  if(typeof window.paintCreature === "function"){ try { window.paintCreature(box, {species:0, stage:0, shiny:false, _noFloor:true}, 72, false); return box; } catch(e){} }
  box.textContent = "🎳";
  return box;
}
// Your bowler choices: saved team, everything unlocked, then a few classics.
function rosterDexes(){
  var P = HQV.pk, out = [], seen = {};
  function add(dex){ if(dex > 0 && dex <= 1025 && !seen[dex]){ seen[dex] = 1; var m = monOfDex(dex); if(m) out.push({dex:dex, name:m.name}); } }
  if(P && pkd()){
    ((P.savedTeam && P.savedTeam()) || []).concat((P.unlocked && P.unlocked()) || []).forEach(function(s){
      try { var c = P.cleanSpec(s); if(c) add(P.buildMon(c).dex); } catch(e){}
    });
  }
  [25, 1, 4, 7, 133, 143].forEach(add);
  return out;
}
function myDex(){ var d = bowlSave().dex|0; if(d > 0) return d; var r = rosterDexes(); return r.length ? r[0].dex : DEFAULT_DEX; }

/* ------------------------------ the lane view ------------------------------ */
// Draws one state: the ball (or none), the pins [{x, z, down, off, tilt, dir}], the aim guide.
function LaneView(host){
  this.wrap = api.mk("div", "bowl-stage");
  this.bowlerBox = api.mk("div", "bowl-bowler");
  this.flash = api.mk("div", "bowl-flash"); this.flash.setAttribute("aria-hidden", "true");
  this.wrap.appendChild(this.bowlerBox); this.wrap.appendChild(this.flash);
  host.appendChild(this.wrap);
  this.mode = "";
  this.skin = null; this.bumpers = false; this.dex = 0; this.camZ = -2.6; this.alive = true;
  var self = this;
  if(E.hasWebGL2() && !this.force2d()){
    E.lib().then(function(lib){ if(self.alive) self.build3d(lib.THREE); }, function(){ if(self.alive) self.build2d(); });
  } else this.build2d();
}
LaneView.prototype.force2d = function(){ try { return localStorage.getItem("hq_bowl_2d") === "1"; } catch(e){ return false; } };
LaneView.prototype.setBowler = function(dex){
  if(dex === this.dex) return;
  this.dex = dex; this.bowlerBox.textContent = ""; this.bowlerBox.appendChild(bowlerEl(dex));
};
LaneView.prototype.throwAnim = function(){
  if(api.calm()) return;
  var b = this.bowlerBox; b.classList.remove("throw"); void b.offsetWidth; b.classList.add("throw");
};
LaneView.prototype.showFlash = function(text){
  var f = this.flash; f.textContent = text; f.classList.remove("on");
  if(!text) return;
  void f.offsetWidth; f.classList.add("on");
};
LaneView.prototype.build2d = function(){
  this.mode = "2d";
  var cv = api.mk("canvas", "bowl-canvas bowl-2d"); cv.width = 760; cv.height = 240;
  cv.setAttribute("role", "img"); cv.setAttribute("aria-label", "Top-down bowling lane");
  this.wrap.insertBefore(cv, this.wrap.firstChild); this.cv = cv;
};
LaneView.prototype.build3d = function(THREE){
  var self = this, T = E.tokens(), cv = api.mk("canvas", "bowl-canvas");
  cv.setAttribute("role", "img"); cv.setAttribute("aria-label", "Bowling lane");
  this.wrap.insertBefore(cv, this.wrap.firstChild);
  var r;
  try { r = new THREE.WebGLRenderer({canvas:cv, antialias:true}); } catch(e){ cv.remove(); this.build2d(); return; }
  r.setPixelRatio(Math.min(2, window.devicePixelRatio || 1));
  cv.addEventListener("webglcontextlost", function(ev){ ev.preventDefault(); if(self.three){ self.three = null; cv.remove(); self.build2d(); } });
  var C = function(s){ return new THREE.Color(s); };
  var light = C(T.panel), dark = C(T.ink);
  if(light.getHSL({}).l < dark.getHSL({}).l){ var tmp = light; light = dark; dark = tmp; }
  var scene = new THREE.Scene(); scene.background = C(T.bg2);
  scene.add(new THREE.HemisphereLight(light, C(T.muted), 2.1));
  var sun = new THREE.DirectionalLight(light, 1.4); sun.position.set(1, 6, -2); scene.add(sun);
  var cam = new THREE.PerspectiveCamera(48, 16/9, 0.05, 120);
  var len = (PIT_Z + 6000)/M, w = 2*HALF_W/M;
  function plane(pw, pl, color, x, y, z){
    var g = new THREE.PlaneGeometry(pw, pl); g.rotateX(-Math.PI/2);
    var m = new THREE.Mesh(g, new THREE.MeshLambertMaterial({color:color})); m.position.set(x, y, z); scene.add(m); return m;
  }
  var wood = C(T.gold).lerp(light, 0.55);
  plane(w, len, wood, 0, 0, len/2 - 0.3);
  plane(0.24, len, C(T.line), w/2 + 0.12, -0.04, len/2 - 0.3);
  plane(0.24, len, C(T.line), -w/2 - 0.12, -0.04, len/2 - 0.3);
  plane(w + 0.6, 0.03, C(T.need), 0, 0.002, 0);
  // the target arrows
  for(var k = -3; k <= 3; k++){ var a = plane(0.05, 0.22, C(T.brand), k*0.13, 0.002, 4.6 + Math.abs(k)*0.3); a.rotation.y = 0; }
  plane(w + 0.6, 3, C(T.muted), 0, -0.2, PIT_Z/M + 1.6);
  var bump = new THREE.Group();
  [1, -1].forEach(function(s){ var m = new THREE.Mesh(new THREE.BoxGeometry(0.06, 0.08, len), new THREE.MeshLambertMaterial({color:C(T.brand)})); m.position.set(s*(w/2 + 0.12), 0.04, len/2 - 0.3); bump.add(m); });
  scene.add(bump);
  // pins: a lathe profile (metres) with a stripe at the neck
  var prof = [[0, 0], [0.026, 0], [0.045, 0.04], [0.06, 0.11], [0.052, 0.17], [0.03, 0.24], [0.026, 0.27], [0.033, 0.32], [0.028, 0.365], [0.0, 0.381]];
  var pinGeo = new THREE.LatheGeometry(prof.map(function(p){ return new THREE.Vector2(p[0], p[1]); }), 18);
  var pinMat = new THREE.MeshStandardMaterial({color:light, roughness:0.35}), stripeMat = new THREE.MeshStandardMaterial({color:C(T.need), roughness:0.5});
  var stripeGeo = new THREE.CylinderGeometry(0.0285, 0.0285, 0.018, 18);
  var pins = [];
  for(var i = 0; i < 10; i++){
    var g = new THREE.Group(), body = new THREE.Mesh(pinGeo, pinMat), st = new THREE.Mesh(stripeGeo, stripeMat);
    st.position.y = 0.262; g.add(body); g.add(st);
    var s2 = st.clone(); s2.position.y = 0.29; g.add(s2);
    scene.add(g); pins.push(g);
  }
  var ballGeo = new THREE.SphereGeometry(BR/M, 28, 18), ballMat = new THREE.MeshStandardMaterial({color:C(T.brand), roughness:0.25, metalness:0.1});
  var ball = new THREE.Mesh(ballGeo, ballMat); scene.add(ball);
  var guideGeo = new THREE.BufferGeometry().setFromPoints([new THREE.Vector3(), new THREE.Vector3()]);
  var guide = new THREE.Line(guideGeo, new THREE.LineBasicMaterial({color:C(T.brand)})); scene.add(guide);
  this.three = {THREE:THREE, r:r, scene:scene, cam:cam, pins:pins, ball:ball, ballMat:ballMat, bump:bump, guide:guide, cv:cv, skin:"", light:light, dark:dark};
  this.mode = "3d";
};
// The ball's look: a cosmetic colour, or a Poké Ball for the Poké Ball cosmetic.
LaneView.prototype.ballTexture = function(THREE, skin){
  var T = E.tokens(), cv = document.createElement("canvas"); cv.width = 128; cv.height = 64;
  var g = cv.getContext("2d"), th = this.three;
  void T;
  var lightCss = "#" + th.light.getHexString(), darkCss = "#" + th.dark.getHexString();
  g.fillStyle = skin; g.fillRect(0, 0, 128, 32);
  g.fillStyle = lightCss; g.fillRect(0, 32, 128, 32);
  g.fillStyle = darkCss; g.fillRect(0, 29, 128, 6);
  g.beginPath(); g.arc(32, 32, 9, 0, Math.PI*2); g.fill();
  g.fillStyle = lightCss; g.beginPath(); g.arc(32, 32, 5, 0, Math.PI*2); g.fill();
  var tex = new THREE.CanvasTexture(cv); tex.colorSpace = THREE.SRGBColorSpace; return tex;
};
LaneView.prototype.draw = function(s){
  if(this.mode === "3d" && this.three) this.draw3d(s);
  else if(this.mode === "2d") this.draw2d(s);
};
LaneView.prototype.draw3d = function(s){
  var th = this.three, THREE = th.THREE, w = this.wrap.clientWidth || 640, h = Math.round(w*9/16);
  var size = th.r.getSize(new THREE.Vector2());
  if(size.x !== w || size.y !== h){ th.r.setSize(w, h, false); th.cam.aspect = w/h; th.cam.updateProjectionMatrix(); }
  th.bump.visible = !!s.bumpers;
  var skin = s.skin || "";
  if(skin !== th.skin){
    th.skin = skin;
    if(th.ballMat.map){ th.ballMat.map.dispose(); th.ballMat.map = null; }
    if(skin.toLowerCase() === POKEBALL){ th.ballMat.map = this.ballTexture(THREE, skin); th.ballMat.color.set(0xffffff); }
    else th.ballMat.color.set(skin && /^#[0-9a-f]{6}$/i.test(skin) ? skin : E.tokens().brand);
    th.ballMat.needsUpdate = true;
  }
  var b = s.ball;
  th.ball.visible = !!b;
  if(b){ th.ball.position.set(-b[0]/M, BR/M, b[1]/M); th.ball.rotation.x = b[1]/BR; th.ball.rotation.z = (b[2] || 0)*0.002; }
  s.pins.forEach(function(p, i){
    var g = th.pins[i]; g.visible = !p.off;
    if(p.off) return;
    g.position.set(-p.x/M, 0, p.z/M);
    g.quaternion.identity();
    if(p.tilt > 0){
      var dx = -(p.dir ? p.dir[0] : 0), dz = p.dir ? p.dir[1] : 1, l = Math.sqrt(dx*dx + dz*dz) || 1;
      g.quaternion.setFromAxisAngle(new THREE.Vector3(dz/l, 0, -dx/l), p.tilt*Math.PI/2);
      g.position.y = 0.03*p.tilt;
    }
  });
  var gp = th.guide.geometry.attributes.position;
  th.guide.visible = !!s.guide;
  if(s.guide){
    var gx = s.guide.x, end = gx + Math.round(HEAD_Z*s.guide.aim/AIM_DIV);
    gp.setXYZ(0, -gx/M, 0.01, 0.2); gp.setXYZ(1, -clamp(end, -HALF_W, HALF_W)/M, 0.01, HEAD_Z/M - 1); gp.needsUpdate = true;
  }
  var want = s.cam === "deck" ? HEAD_Z/M - 4.2 : b && s.follow ? clamp(b[1]/M - 3.2, -2.6, HEAD_Z/M - 4.2) : -2.6;
  this.camZ = api.calm() || s.snap ? want : this.camZ + (want - this.camZ)*0.18;
  th.cam.position.set(0, s.cam === "deck" ? 1.0 : 1.2, this.camZ);
  th.cam.lookAt(0, 0.1, this.camZ + 7);
  th.r.render(th.scene, th.cam);
};
LaneView.prototype.draw2d = function(s){
  var cv = this.cv, g = cv.getContext("2d"), T = E.tokens(), W = cv.width, H = cv.height;
  var L = 560, top = 40, half = 70;                      // lane strip: z across, x exaggerated
  function lx(z){ return 20 + z/(PIT_Z + 3000)*L; }
  function ly(x){ return top + half + x/HALF_W*half; }
  g.fillStyle = T.panel2; g.fillRect(0, 0, W, H);
  g.fillStyle = T.line; g.fillRect(20, top - 12, L, 2*half + 24);
  g.fillStyle = T.goldbg || T.panel; g.fillRect(20, top, L, 2*half);
  g.strokeStyle = T.gold; g.lineWidth = 1; g.strokeRect(20, top, L, 2*half);
  g.fillStyle = T.need; g.fillRect(lx(0), top - 12, 2, 2*half + 24);
  if(s.bumpers){ g.fillStyle = T.brand; g.fillRect(20, top - 4, L, 4); g.fillRect(20, top + 2*half, L, 4); }
  if(s.guide){
    g.strokeStyle = T.brand; g.setLineDash([5, 6]); g.beginPath();
    g.moveTo(lx(0), ly(s.guide.x)); g.lineTo(lx(HEAD_Z), ly(clamp(s.guide.x + Math.round(HEAD_Z*s.guide.aim/AIM_DIV), -HALF_W, HALF_W))); g.stroke(); g.setLineDash([]);
  }
  // the deck, near true scale, on the right
  var D = 150, dx0 = W - D - 20, dy0 = (H - D)/2, z0 = HEAD_Z - 2500, span = PIT_Z - z0;
  function px(x){ return dx0 + D/2 + x/(2*HALF_W)*D; }
  function pz(z){ return dy0 + D - (z - z0)/span*D; }
  g.fillStyle = T.goldbg || T.panel; g.fillRect(dx0, dy0, D, D); g.strokeStyle = T.gold; g.strokeRect(dx0, dy0, D, D);
  var light = T.ink;
  s.pins.forEach(function(p){
    if(p.off) return;
    g.fillStyle = p.tilt > 0 ? T.muted : light;
    g.beginPath(); g.arc(lx(p.z), ly(p.x), 3, 0, Math.PI*2); g.fill();
    g.beginPath(); g.arc(px(p.x), pz(p.z), Math.max(3, PR/(2*HALF_W)*D*(p.tilt > 0 ? 1.4 : 1)), 0, Math.PI*2); g.fill();
    if(p.tilt <= 0){ g.strokeStyle = T.need; g.lineWidth = 1.5; g.stroke(); }
  });
  var b = s.ball;
  if(b){
    var skin = s.skin && /^#[0-9a-f]{6}$/i.test(s.skin) ? s.skin : T.brand;
    g.fillStyle = skin; g.beginPath(); g.arc(lx(b[1]), ly(b[0]), 7, 0, Math.PI*2); g.fill();
    if(String(s.skin).toLowerCase() === POKEBALL){ g.fillStyle = T.ink; g.fillRect(lx(b[1]) - 7, ly(b[0]) - 1, 14, 2); }
    if(b[1] > z0){ g.fillStyle = skin; g.beginPath(); g.arc(px(b[0]), pz(b[1]), BR/(2*HALF_W)*D, 0, Math.PI*2); g.fill(); }
  }
  g.fillStyle = T.muted; g.font = "12px " + T.mono; g.fillText("foul line", lx(0) + 4, top - 16); g.fillText("pin deck", dx0, dy0 - 6);
};
LaneView.prototype.destroy = function(){
  this.alive = false;
  if(this.three){ try { this.three.r.dispose(); } catch(e){} this.three = null; }
};

// Pins at rest from a standing mask.
function rackPins(standing){ return PINS.map(function(p, i){ return {x:p[0], z:p[1], down:false, off:!standing[i], tilt:0, dir:null}; }); }

/* ------------------------------ one roll, animated ------------------------------ */
// Replays a roll from its inputs and plays it back in real time (or, under calm, jumps to
// where everything stopped). onDone(res) gets the simulation's result.
function Player(view){ this.view = view; this.frames = null; this.t0 = 0; this.res = null; this.onDone = null; }
Player.prototype.play = function(input, standing, lane, oil, onDone){
  var frames = [], downAt = {};
  var res = simulateRoll({bumpers:lane.bumpers, standing:standing}, input.x, input.aim, input.power, input.spin, oil, function(t, bx, bz, pins){
    if(t % 2) return;
    frames.push({b:[bx, bz, t], p:pins.map(function(p, i){
      if(p.down && downAt[i] == null) downAt[i] = t;
      var o = PINS[i], dx = p.x - o[0], dz = p.z - o[1];
      return {x:p.x, z:p.z, off:p.off, down:p.down, tilt: p.down ? clamp((t - downAt[i])/24, 0.05, 1) : 0, dir: dx || dz ? [dx, dz] : [0, 1]};
    })});
  });
  this.frames = frames; this.res = res; this.onDone = onDone; this.t0 = performance.now();
  if(api.calm() || !frames.length){ this.finish(); }
  return res;
};
Player.prototype.state = function(){
  if(!this.frames) return null;
  var i = Math.floor((performance.now() - this.t0)/1000*TICK/2);
  if(i >= this.frames.length){ this.finish(); return this.last; }
  return this.frames[i];
};
Player.prototype.finish = function(){
  var f = this.frames && this.frames[this.frames.length - 1];
  if(f) this.last = {b:f.b, p:f.p.map(function(p){ return {x:p.x, z:p.z, off:p.off, down:p.down, tilt:p.down ? 1 : 0, dir:p.dir}; })};
  this.frames = null;
  var cb = this.onDone; this.onDone = null;
  if(cb) cb(this.res);
};
Player.prototype.busy = function(){ return !!this.frames; };

/* ------------------------------ the table: controls, sheet, loop ------------------------------ */
// drv: {solo, me(), round(), canRoll(), roll(input), start(frames, bumpers), end(), skip(), concede(), host(), setDex(dex)}
function Table(el, drv){
  var self = this;
  this.drv = drv; this.alive = true; this.held = null; this.heldUntil = 0; this.meter = ""; this.mt0 = 0; this.queue = []; this.idleFns = [];
  this.root = api.mk("div", "bowl-wrap");
  this.top = api.mk("div", "vg-row bowl-top");
  this.status = api.mk("p", "vg-muted bowl-status"); this.status.setAttribute("role", "status"); this.status.setAttribute("aria-live", "polite");
  this.stageBox = api.mk("div", "bowl-stagebox");
  this.ctl = api.mk("div", "bowl-ctl");
  this.sheet = api.mk("div", "bowl-sheet-box");
  this.root.appendChild(this.top); this.root.appendChild(this.status); this.root.appendChild(this.stageBox);
  this.root.appendChild(this.ctl); this.root.appendChild(this.sheet);
  el.appendChild(this.root);
  this.view = new LaneView(this.stageBox);
  this.view.wrap.tabIndex = 0;
  this.view.wrap.setAttribute("aria-label", "Bowling lane. Arrows move and aim; Space or Enter runs the meters and rolls.");
  this.player = new Player(this.view);
  this.input = {x:0, aim:0, power:70, spin:0};
  this.buildControls();
  this.I = HQV.input ? HQV.input.create(this.root, {buttons:{roll:{keys:[], pad:[0]}}}) : null;
  // Space/Enter straight from the key event, so a tap shorter than one frame still counts.
  this.root.addEventListener("keydown", function(e){
    if((e.code !== "Space" && e.code !== "Enter") || e.metaKey || e.ctrlKey || e.altKey) return;
    var tg = (e.target && e.target.tagName || "").toLowerCase();
    if(tg === "input" || tg === "select" || tg === "textarea") return;
    e.preventDefault();
    if(!e.repeat) self.press();
  });
  this.sig = "";
  this.last = 0;
  (function frame(ts){
    if(!self.alive) return;
    self.raf = requestAnimationFrame(frame);
    var dt = self.last ? Math.min(0.1, (ts - self.last)/1000) : 0; self.last = ts;
    self.tick(dt);
  })(0);
  if(HQV.pk && HQV.pk.loadUnlocked) HQV.pk.loadUnlocked().then(function(){ if(self.alive){ self.renderPick(); self.update(true); } }, function(){});
}
Table.prototype.slider = function(key, label, min, max, step){
  var self = this, f = api.mk("label", "bowl-field"), name = api.mk("span", "bowl-flabel", label), out = api.mk("output", "bowl-out");
  var r = api.mk("input"); r.type = "range"; r.min = String(min); r.max = String(max); r.step = String(step); r.value = String(this.input[key]);
  r.addEventListener("input", function(){ self.input[key] = parseInt(r.value, 10) || 0; self.meter = ""; self.showInputs(); });
  f.appendChild(name); f.appendChild(r); f.appendChild(out);
  this.fields[key] = {f:f, r:r, out:out};
  return f;
};
Table.prototype.buildControls = function(){
  var self = this, c = this.ctl; this.fields = {};
  var row = api.mk("div", "bowl-sliders");
  row.appendChild(this.slider("x", "Position", -X_MAX, X_MAX, 50));
  row.appendChild(this.slider("aim", "Aim", -AIM_MAX, AIM_MAX, 1));
  row.appendChild(this.slider("power", "Power", 1, 100, 1));
  row.appendChild(this.slider("spin", "Spin", -SPIN_MAX, SPIN_MAX, 1));
  c.appendChild(row);
  var acts = api.mk("div", "vg-row bowl-acts");
  this.rollBtn = api.btn("Roll 🎳", "primary bowl-roll", function(){ self.meter = ""; self.doRoll(); });
  acts.appendChild(this.rollBtn);
  this.pickBox = api.mk("span", "bowl-pick");
  acts.appendChild(this.pickBox);
  acts.appendChild(api.mk("span", "vg-muted bowl-keys", "←/→ move · ↑/↓ aim · Space: aim → power → spin → roll"));
  c.appendChild(acts);
  this.showInputs(); this.renderPick();
};
Table.prototype.renderPick = function(){
  var self = this, box = this.pickBox; box.textContent = "";
  var list = rosterDexes(); if(!list.length) return;
  var sel = api.mk("select", "vg-select bowl-dex"); sel.setAttribute("aria-label", "Your bowler");
  var cur = myDex();
  list.forEach(function(m){ var o = api.mk("option", null, m.name); o.value = String(m.dex); if(m.dex === cur) o.selected = true; sel.appendChild(o); });
  sel.addEventListener("change", function(){
    var d = parseInt(sel.value, 10) || DEFAULT_DEX, sv = bowlSave(); sv.dex = d; api.persist();
    self.drv.setDex(d); self.update(true);
  });
  box.appendChild(sel);
};
Table.prototype.showInputs = function(){
  var self = this, labels = {x:function(v){ return (v/M).toFixed(2) + " m"; }, aim:String, power:String, spin:function(v){ return v > 0 ? v + " →" : v < 0 ? "← " + (-v) : "0"; }};
  Object.keys(this.fields).forEach(function(k){
    var f = self.fields[k]; if(document.activeElement !== f.r) f.r.value = String(self.input[k]);
    f.out.textContent = labels[k](self.input[k]);
    f.f.classList.toggle("live", self.meter === k);
  });
};
// Space/Enter: aim meter, then power, then spin, then roll. Under calm, roll at once.
Table.prototype.press = function(){
  if(!this.drv.canRoll() || this.player.busy()) return;
  if(api.calm()){ this.doRoll(); return; }
  var next = {"":"aim", aim:"power", power:"spin"}[this.meter];
  if(next){ this.meter = next; this.mt0 = performance.now(); E.say(next === "aim" ? "Aim" : next === "power" ? "Power" : "Spin"); }
  else { this.meter = ""; this.doRoll(); }
  this.showInputs();
};
Table.prototype.doRoll = function(){
  if(!this.drv.canRoll() || this.player.busy()) return;
  var inp = {x:clamp(this.input.x|0, -X_MAX, X_MAX), aim:clamp(this.input.aim|0, -AIM_MAX, AIM_MAX), power:clamp(this.input.power|0, 1, 100), spin:clamp(this.input.spin|0, -SPIN_MAX, SPIN_MAX)};
  this.view.throwAnim();
  this.drv.roll(inp);
};
// A roll to show: animate it, then hold where the pins fell for a moment.
Table.prototype.show = function(ev, lane, oil, after){
  var self = this;
  if(this.player.busy()){ this.queue.push([ev, lane, oil, after]); return; }
  this.view.throwAnim();
  this.player.play(ev.input, ev.standing, lane, oil, function(res){
    self.held = self.player.last; self.heldUntil = performance.now() + (api.calm() ? 1600 : 1300);
    if(after) after(res);
    var nx = self.queue.shift();
    if(nx) self.show(nx[0], nx[1], nx[2], nx[3]);
    else self.idleFns.splice(0).forEach(function(f){ try { f(); } catch(e){} });
  });
};
// Run fn once every queued roll has been shown (the Arena's 'done' comes before the last
// ball has finished rolling on screen).
Table.prototype.whenIdle = function(fn){
  if(this.player.busy() || this.queue.length) this.idleFns.push(fn); else fn();
};
Table.prototype.tick = function(dt){
  var I = this.I, r = this.drv.round();
  if(I){
    I.poll();
    if(I.pressed("roll")) this.press();
    if(!this.meter && this.drv.canRoll()){
      var ax = I.axis("x"), ay = I.axis("y");
      if(ax){ this.input.x = clamp(Math.round(this.input.x + ax*2600*dt), -X_MAX, X_MAX); this.showInputs(); }
      if(ay){ this.aimAcc = (this.aimAcc || 0) - ay*60*dt; var st = this.aimAcc > 0 ? Math.floor(this.aimAcc) : Math.ceil(this.aimAcc); if(st){ this.aimAcc -= st; this.input.aim = clamp(this.input.aim + st, -AIM_MAX, AIM_MAX); this.showInputs(); } }
    }
  }
  if(this.meter && !api.calm()){
    var ph = (performance.now() - this.mt0)/1000, per = {aim:1.8, power:1.3, spin:1.6}[this.meter];
    var u = (ph % per)/per, tri = u < 0.5 ? u*2 : 2 - u*2;
    if(this.meter === "power") this.input.power = 1 + Math.round(tri*99);
    else this.input[this.meter] = Math.round((tri*2 - 1)*100);
    this.showInputs();
  }
  var st = this.player.state(), pins, ball, cam = "start", follow = false;
  if(st){ pins = st.p; ball = st.b; follow = true; }
  else if(this.held && performance.now() < this.heldUntil){ pins = this.held.p; ball = this.held.b; cam = "deck"; }
  else {
    this.held = null;
    pins = rackPins(this.drv.standing());
    ball = this.drv.canRoll() || this.drv.solo ? [this.input.x, 0, 0] : null;
  }
  var lane = this.drv.lane();
  this.view.setBowler(this.drv.bowlerDex());
  this.view.draw({ball:ball, pins:pins, cam:cam, follow:follow, bumpers:lane.bumpers, skin:this.drv.skin(),
    guide: !st && !this.held && this.drv.canRoll() ? {x:this.input.x, aim:this.input.aim} : null});
  this.rollBtn.disabled = !this.drv.canRoll() || this.player.busy();
  void r;
  this.update(false);
};
Table.prototype.update = function(force){
  var r = this.drv.round(), sig = JSON.stringify([r, this.drv.host(), this.drv.canRoll(), this.player.busy()]);
  if(sig === this.sig && !force) return;
  this.sig = sig;
  this.renderTop(r); this.renderSheet(r);
};
Table.prototype.renderTop = function(r){
  var self = this, top = this.top, playing = r && r.phase === "playing"; top.textContent = "";
  if(!playing && this.drv.host()){
    var fs = api.mk("select", "vg-select bowl-frames"); fs.setAttribute("aria-label", "Frames");
    [[10, "10 frames"], [5, "5 frames"]].forEach(function(o){ var e = api.mk("option", null, o[1]); e.value = String(o[0]); fs.appendChild(e); });
    fs.value = String(this.lastFrames || 10);
    fs.addEventListener("change", function(){ self.lastFrames = parseInt(fs.value, 10); });
    var bl = api.mk("label", "bowl-check"), cb = api.mk("input"); cb.type = "checkbox"; cb.checked = !!this.lastBumpers;
    cb.addEventListener("change", function(){ self.lastBumpers = cb.checked; });
    bl.appendChild(cb); bl.appendChild(document.createTextNode(" Bumpers"));
    top.appendChild(fs); top.appendChild(bl);
    top.appendChild(api.btn("Start bowling", "primary bowl-start", function(){ self.drv.start(parseInt(fs.value, 10) || 10, cb.checked); }));
  } else if(!playing) top.appendChild(api.mk("span", "vg-muted", "The host picks frames and bumpers and starts the game."));
  if(playing && this.drv.host()){
    if(!this.drv.solo) top.appendChild(api.btn("Skip turn", "ghost", function(){ self.drv.skip(); }));
    top.appendChild(api.btn("End game", "ghost", function(){ self.drv.end(); }));
  }
  if(playing && !this.drv.solo && this.drv.inGame()) top.appendChild(api.btn("Concede", "ghost", function(){ self.drv.concede(); }));
  var sv = bowlSave(), keys = Object.keys(sv.best);
  if(this.drv.solo && keys.length) top.appendChild(api.mk("span", "vg-muted bowl-best", "Best: " + keys.sort().map(function(k){ return k + " " + sv.best[k]; }).join(" · ")));
  var txt;
  if(playing){
    var t = r.turn, who = t ? this.drv.nameOf(t.user) : "";
    txt = (r.bumpers ? "Bumpers on · " : "") + (t ? "Frame " + (t.frame + 1) + "/" + r.frames + " · ball " + (t.ball + 1) + " · " + (this.drv.isMe(t.user) ? "your turn" : who + " to bowl") : "");
  } else if(r && r.last && r.last.results && r.last.results.length){
    txt = "Last game (" + r.last.key + "): " + r.last.results.map(function(x){ return "#" + x.place + " " + (self.drv.nameOf(x.user && x.user.userId) || "?") + " " + x.score + (x.dnf ? " (dnf)" : ""); }).join(" · ");
  } else txt = "Roll down the lane: set your line, power and spin. A strike shows X, a spare /.";
  this.status.textContent = txt;
};
Table.prototype.renderSheet = function(r){
  var box = this.sheet, self = this; box.textContent = "";
  if(!r || !r.players || !r.players.length) return;
  var n = r.frames || 10, tbl = api.mk("table", "bowl-sheet"), cap = api.mk("caption", "vg-muted", "Score sheet");
  tbl.appendChild(cap);
  var hr = api.mk("tr"); hr.appendChild(api.mk("th", null, "Bowler"));
  for(var i = 0; i < n; i++) hr.appendChild(api.mk("th", null, String(i + 1)));
  hr.appendChild(api.mk("th", null, "Total")); tbl.appendChild(hr);
  r.players.forEach(function(p){
    var tr = api.mk("tr", p.dnf ? "dnf" : ""), sc = scoreCard(p.rolls), uid = p.user && p.user.userId;
    var nm = api.mk("th", null, self.drv.nameOf(uid) + (p.away ? " (away)" : p.dnf ? " (out)" : "")); nm.scope = "row"; tr.appendChild(nm);
    for(var i = 0; i < n; i++){
      var f = p.rolls[i] || [], mk = marks(f, i + 1 === n), td = api.mk("td", "bowl-f" + (r.turn && r.turn.user === uid && r.turn.frame === i ? " cur" : ""));
      var balls = api.mk("div", "bowl-balls");
      var slots = i + 1 === n ? 3 : 2;
      for(var k = 0; k < slots; k++){ var m = mk[k] || ""; balls.appendChild(api.mk("span", "bowl-b" + (m === "X" ? " x" : m === "/" ? " sp" : ""), m)); }
      td.appendChild(balls);
      td.appendChild(api.mk("div", "bowl-cum", sc[i] != null ? String(sc[i]) : ""));
      td.setAttribute("aria-label", "Frame " + (i + 1) + ": " + (mk.filter(Boolean).join(" ") || "not bowled") + (sc[i] != null ? ", " + sc[i] : ""));
      tr.appendChild(td);
    }
    tr.appendChild(api.mk("td", "bowl-tot", String(totalOf(p.rolls))));
    tbl.appendChild(tr);
  });
  box.appendChild(tbl);
};
Table.prototype.announce = function(name, r){
  var w = rollWords(r);
  E.say((name ? name + ": " : "") + w);
  this.view.showFlash(r.strike ? "X" : r.spare ? "/" : r.gutter ? "Gutter" : "");
};
Table.prototype.destroy = function(){
  this.alive = false; cancelAnimationFrame(this.raf);
  if(this.I) this.I.destroy();
  this.view.destroy();
};

/* ------------------------------ solo ------------------------------ */
function soloMount(el){
  var g = null, table = null, paused = false;
  function round(){
    if(!g) return {phase:"idle", players:[], frames:10};
    return {phase:g.phase, frames:g.frames, bumpers:g.bumpers, oil:g.oil, key:keyOf(g.frames, g.bumpers),
      players:[{user:{userId:"me"}, rolls:g.p.rolls, standing:g.p.standing, strikes:g.p.strikes, spares:g.p.spares, away:false, dnf:false}],
      turn: g.phase === "playing" ? {user:"me", frame:g.p.done, ball:g.p.rolls[g.p.done].length} : null, last:g.last || null};
  }
  var drv = {
    solo: true,
    round: round,
    host: function(){ return true; },
    isMe: function(){ return true; },
    inGame: function(){ return true; },
    nameOf: function(){ return "You"; },
    canRoll: function(){ return !paused && !!g && g.phase === "playing"; },
    standing: function(){ return g && g.phase === "playing" ? g.p.standing : fullRack(); },
    lane: function(){ return {bumpers: !!(g && g.bumpers)}; },
    skin: function(){ return (window.HQ_MYCOS && window.HQ_MYCOS.ball) || ""; },
    bowlerDex: function(){ return myDex(); },
    setDex: function(){},
    start: function(frames, bumpers){
      var sv = bowlSave(), r = api.rng("bowl:" + sv.games + ":" + frames + (bumpers ? "b" : ""));
      g = {phase:"playing", frames: frames === 5 ? 5 : 10, bumpers: !!bumpers, oil: Math.floor(r()*OIL.length), p:newCard(frames === 5 ? 5 : 10), last:null};
      table.update(true);
      E.say("New game: " + g.frames + " frames" + (g.bumpers ? " with bumpers" : "") + ". Your roll.");
    },
    end: function(){ if(g){ g.phase = "idle"; table.update(true); } },
    skip: function(){}, concede: function(){},
    roll: function(inp){
      if(!g || g.phase !== "playing") return;
      var standing = g.p.standing.slice(), lane = {bumpers:g.bumpers};
      table.show({input:inp, standing:standing}, lane, g.oil, function(res){
        if(!g) return;
        var out = applyRoll(g.p, g.frames, res);
        table.announce("", out);
        if(out.strike){ note("bowl-strike"); E.sfx && E.sfx("finish"); }
        if(out.complete){ var sc = scoreCard(g.p.rolls)[out.frame]; if(sc != null) E.say("Frame " + (out.frame + 1) + ": " + sc); }
        if(g.p.done >= g.frames){
          var total = totalOf(g.p.rolls), key = keyOf(g.frames, g.bumpers);
          g.phase = "done"; g.last = {key:key, results:[{user:{userId:"me"}, place:1, score:total, strikes:g.p.strikes, spares:g.p.spares, dnf:false}]};
          recordGame(key, total, g.p.strikes);
          E.say("Game over: " + total + (total === g.frames*30 ? ". A perfect game!" : "."));
          api.toast("🎳 " + total + " in " + g.frames + " frames", total >= 200 ? "ach" : undefined);
        }
        table.update(true);
      });
    }
  };
  table = new Table(el, drv);
  table.update(true);
  return {stop:function(){ table.destroy(); }, pause:function(){ paused = true; }, resume:function(){ paused = false; }};
}
var SOLO = null;
HQV.register({id:"bowl", name:"Bowling", icon:"🎳", desc:"Ten pins with your buddy Pokémon",
  mount: function(el){ SOLO = soloMount(el); },
  unmount: function(){ if(SOLO) SOLO.stop(); SOLO = null; },
  pause: function(){ if(SOLO) SOLO.pause(); },
  resume: function(){ if(SOLO) SOLO.resume(); },
  badge: function(){ var b = (api.save && api.save.bowl && api.save.bowl.best) || {}, n = 0; Object.keys(b).forEach(function(k){ n = Math.max(n, b[k]|0); }); return n ? "best " + n : ""; }});

/* ------------------------------ in an Arena room ------------------------------ */
if(!MP) return;
function hostOf(s){ var me = MP.me(), h = false; (s.lobby || []).forEach(function(m){ if(m.userId === me && m.host) h = true; }); return h; }
function playerOf(s, uid){ var r = s.round, out = null; ((r && r.players) || []).forEach(function(p){ if(p.user && p.user.userId === uid) out = p; }); return out; }
MP.handlers.bowl = {
  on: function(m, s){
    if(m.ev === "bowl"){
      // While a roll is still animating, keep the pins it started from on screen.
      s.round = m.round;
      if(m.by) api.toast("🎳 " + MP.nameOf(m.by) + " started Bowling");
    }
    if(m.ev === "char" && s.round){ s.round.chars = s.round.chars || {}; s.round.chars[m.user] = m.c; }
    if(m.ev === "roll"){
      var T = s.table, mine = m.user && m.user.userId === MP.me(), name = MP.nameOf(m.user);
      var lane = {bumpers: !!(s.round && s.round.bumpers)}, oil = s.round ? s.round.oil|0 : 0;
      s.rollingFor = m.user && m.user.userId;
      var done = function(res){
        var same = res.pinsDown.every(function(d, i){ return d === !!m.pins_down[i]; });
        if(!same && T && T.held) T.held = {b:T.held.b, p:rackPins(m.standing.map(function(st, i){ return st && !m.pins_down[i]; }))};
        if(T) T.announce(mine ? "You" : name, {strike:m.strike, spare:m.spare, gutter:m.gutter, fell:m.fell});
        if(mine && m.strike){ note("bowl-strike"); E.sfx && E.sfx("finish"); }
        s.rollingFor = null;
      };
      if(T) T.show(m, lane, oil, done); else done(simulateRoll({bumpers:lane.bumpers, standing:m.standing}, m.input.x, m.input.aim, m.input.power, m.input.spin, oil));
    }
    if(m.ev === "frame" && m.scores){
      var sc = m.scores[m.frame];
      if(sc != null) E.say(MP.nameOf(m.user) + ", frame " + (m.frame + 1) + ": " + sc);
    }
    if(m.ev === "done"){
      if(s.table){ s.table.whenIdle(function(){ MP.handlers.bowl.over(m); }); return; }
      MP.handlers.bowl.over(m);
    }
  },
  over: function(m){
    {
      var me = MP.me(), r0 = null;
      (m.results || []).forEach(function(x){ if(x.user && x.user.userId === me) r0 = x; });
      var win = (m.results || [])[0];
      E.say("Game over. " + (win ? MP.nameOf(win.user) + " wins with " + win.score + "." : ""));
      if(r0 && !r0.dnf){
        recordGame(m.key, r0.score|0, r0.strikes|0);
        api.toast("🎳 " + r0.score + " · place " + r0.place, r0.place === 1 ? "ach" : undefined);
      }
    }
  },
  render: function(ctx, s){ if(s.table) s.table.update(false); }
};
MP.register("bowl", "🎳", "Ten-pin bowling with friends, up to 8", function(ctx){
  var s = MP.st("bowl"), seq = 0;
  function rnd(){ return s.round || {phase:"idle", players:[], frames:10}; }
  function turnUser(){ var r = rnd(); return r.phase === "playing" && r.turn ? r.turn.user : null; }
  var drv = {
    solo: false,
    round: rnd,
    host: function(){ return hostOf(s); },
    isMe: function(uid){ return uid === MP.me(); },
    inGame: function(){ var p = playerOf(s, MP.me()); return !!(p && !p.dnf); },
    nameOf: function(uid){
      var p = playerOf(s, uid); if(p) return MP.nameOf(p.user);
      var l = null; (s.lobby || []).forEach(function(m){ if(m.userId === uid) l = m; });
      return MP.nameOf(l);
    },
    canRoll: function(){ var r = rnd(); return turnUser() === MP.me() && !s.rollingFor && !(r.readyInMs > 0 && s.at && Date.now() - s.at < r.readyInMs); },
    standing: function(){ var u = turnUser(), p = u ? playerOf(s, u) : null; return p ? p.standing : fullRack(); },
    lane: function(){ return {bumpers: !!rnd().bumpers}; },
    skin: function(){ var u = s.rollingFor || turnUser(), p = u ? playerOf(s, u) : null, c = p && p.user && p.user.cos; return (c && c.ball) || (u === MP.me() && window.HQ_MYCOS && window.HQ_MYCOS.ball) || ""; },
    bowlerDex: function(){ var u = s.rollingFor || turnUser() || MP.me(), ch = rnd().chars || {}; return (ch[u]|0) || (u === MP.me() ? myDex() : DEFAULT_DEX); },
    setDex: function(d){ MP.send("bowl", "char", {c:d}); },
    start: function(frames, bumpers){ MP.send("bowl", "start", {frames:frames, bumpers:!!bumpers}); },
    end: function(){ MP.send("bowl", "end"); },
    skip: function(){ MP.send("bowl", "skip"); },
    concede: function(){ MP.send("bowl", "concede"); },
    roll: function(inp){
      var r = rnd(), t = r.turn; if(!t) return;
      seq += 1;
      MP.send("bowl", "roll", {x:inp.x, aim:inp.aim, power:inp.power, spin:inp.spin, seq:seq, frame:t.frame, ball:t.ball});
      s.rollingFor = MP.me();               // until the Arena's roll comes back
      setTimeout(function(){ if(s.rollingFor === MP.me() && !(s.table && s.table.player.busy())) s.rollingFor = null; }, 4000);
    }
  };
  s.table = new Table(ctx.box, drv);
  s.table.update(true);
  ctx.onRejoin = function(){ MP.send("bowl", "view"); };
  MP.send("bowl", "view");
  MP.send("bowl", "char", {c: myDex()});
  ctx.stop = function(){ if(s.table) s.table.destroy(); s.table = null; s.rollingFor = null; };
});
})();
