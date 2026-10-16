/* Claude HQ: the skatepark in Arena City (the plaza, opposite the bike park).
 *
 * Pure, so tests/test_hq3d.py runs it under node: no THREE, no DOM.
 *   HQV.hqSkatePark  the park's shape in its own frame (centre at 0,0; x across, z along):
 *                    two quarter pipes facing each other, a fun box, a ledge and a rail;
 *                    height(x, z), rails, walkBlocked(x, z) for people on foot.
 *   HQV.hqSkateSim   one skater: newState(), step(state, input, dt) -> events. Rolling on the
 *                    ramps (gravity along the slope), ollies, airs off the quarter pipes,
 *                    spins, kickflips, shove-its, grinds, landing or bailing, and the combo.
 * games/hq3d.js draws what the state says (the board, the rider, the trick) and games/hqcity.js
 * builds the park's meshes from HQV.hqSkatePark, so what you see is what you skate on.
 */
(function(){
"use strict";
var HQV = window.HQV = window.HQV || {};
var PI = Math.PI, G = 9.8;

/* ---------- the park ---------- */
var HW = 5.75, HD = 11;                 // half its width (x) and depth (z), metres
var QP = {r: 3.2, run: 3.0, from: 8};   // quarter pipes: radius, how far they run, where they start (|z|)
var BOX = {hw: 2.2, hd: 2.8, top: 0.55, flat: 1.2};   // fun box: half width, half depth, height, half its flat top
var LEDGE = {x0: -5.2, x1: -3.8, hd: 5, top: 0.45};
var RAIL = {x: 4.0, hd: 5, top: 0.6};
// what you can grind: a line along z at x, top height, from -hd to +hd
var RAILS = [{name: "Rail", x: RAIL.x, top: RAIL.top, hd: RAIL.hd, rail: true},
             {name: "Ledge", x: LEDGE.x1, top: LEDGE.top, hd: LEDGE.hd, rail: false}];

function inPark(x, z){ return Math.abs(x) <= HW && Math.abs(z) <= HD; }
// height of the riding surface at (x, z); 0 off the park
function height(x, z){
  if(!inPark(x, z)) return 0;
  var az = Math.abs(z);
  if(az > QP.from){ var t = Math.min(QP.run, az - QP.from); return QP.r - Math.sqrt(QP.r*QP.r - t*t); }
  if(Math.abs(x) < BOX.hw && az < BOX.hd) return az <= BOX.flat ? BOX.top : BOX.top*(BOX.hd - az)/(BOX.hd - BOX.flat);
  if(x > LEDGE.x0 && x < LEDGE.x1 && az < LEDGE.hd) return LEDGE.top;
  return 0;
}
var QP_TOP = height(0, QP.from + QP.run);   // the lip of a quarter pipe
// slope of the surface along a heading (rise per metre forward)
function slope(x, z, yaw){
  var e = 0.08, sx = Math.sin(yaw)*e, sz = Math.cos(yaw)*e;
  // sample inside the park only: past a quarter pipe's lip there is nothing (height 0), and
  // reading that as a drop would throw you back up the ramp
  var cz = function(v){ return inPark(x, z) ? Math.max(-HD + 0.001, Math.min(HD - 0.001, v)) : v; };
  return (height(x + sx, cz(z + sz)) - height(x - sx, cz(z - sz)))/(2*e);
}
// on a quarter pipe, and how far up it (0 at its foot, run at its lip); sign of z = which end
function onQP(x, z){ var az = Math.abs(z); return inPark(x, z) && az > QP.from ? {t: Math.min(QP.run, az - QP.from), end: z > 0 ? 1 : -1} : null; }
// people on foot walk round the ramps, the box, the ledge and the rail (skaters ride them)
function walkBlocked(x, z){
  if(!inPark(x, z)) return false;
  if(height(x, z) > 0.05) return true;
  return Math.abs(x - RAIL.x) < 0.25 && Math.abs(z) < RAIL.hd + 0.1;
}
HQV.hqSkatePark = {HW: HW, HD: HD, QP: QP, BOX: BOX, LEDGE: LEDGE, RAIL: RAIL, RAILS: RAILS, QP_TOP: QP_TOP,
  inPark: inPark, height: height, slope: slope, onQP: onQP, walkBlocked: walkBlocked};

/* ---------- one skater ---------- */
// Points, and the id other players get for what you are doing (presence `k`).
var TRICKS = {
  ollie: {name: "Ollie", pts: 100}, air: {name: "Air", pts: 250}, bigAir: {name: "Big air", pts: 150},
  kickflip: {name: "Kickflip", pts: 300}, shove: {name: "Shove-it", pts: 250},
  spin180: {name: "180", pts: 200}, spin360: {name: "360", pts: 450}, spin540: {name: "540", pts: 750},
  grind: {name: "Grind", pts: 0}
};
var K = {none: 0, air: 1, flip: 2, shove: 3, spin: 4, grind: 5, bail: 6};   // presence `k`
var PUSH = 5.5, TOP = 9, BRAKE = 8, FRICTION = 0.35, OLLIE_V = 4.6, FLIP_T = 0.45, SHOVE_T = 0.4;
var SPIN_RATE = 7.5, LAND_TOL = 0.75, COMBO_GAP = 2.0, GRIND_PTS = 40, MAX_MULT = 10;

function newState(x, z, yaw){
  return {x: x || 0, z: z || 0, y: 0, vy: 0, yaw: yaw || 0, speed: 0, air: false, vert: false, airT: 0,
    spin: 0, flip: 0, shove: 0, grind: null, grindT: 0, bail: 0, fakie: false,
    combo: {score: 0, mult: 0, names: [], idle: 0}, pending: [], best: 0};
}
// what other players should see you doing (presence k)
function kOf(s){
  if(s.bail > 0) return K.bail;
  if(s.grind) return K.grind;
  if(!s.air) return K.none;
  if(s.flip > 0) return K.flip;
  if(s.shove > 0) return K.shove;
  if(Math.abs(s.spin) > 0.3) return K.spin;
  return K.air;
}
function trick(s, ev, key, extraPts){
  var T = TRICKS[key], pts = T.pts + (extraPts || 0);
  s.combo.score += pts; s.combo.mult = Math.min(MAX_MULT, s.combo.mult + 1); s.combo.names.push(T.name); s.combo.idle = 0;
  ev.push({type: "trick", name: T.name, pts: pts, combo: s.combo.score, mult: s.combo.mult});
}
function bail(s, ev, why){
  if(s.combo.mult) ev.push({type: "bail", why: why, lost: s.combo.score*s.combo.mult});
  else ev.push({type: "bail", why: why, lost: 0});
  s.combo = {score: 0, mult: 0, names: [], idle: 0};
  s.pending = []; s.air = false; s.vert = false; s.grind = null; s.spin = 0; s.flip = 0; s.shove = 0;
  s.vy = 0; s.speed = 0; s.bail = 1.1; s.y = height(s.x, s.z);
}
function bank(s, ev){
  if(!s.combo.mult) return;
  var total = s.combo.score*s.combo.mult;
  ev.push({type: "combo", total: total, names: s.combo.names.slice(), mult: s.combo.mult, best: total > s.best});
  if(total > s.best) s.best = total;
  s.combo = {score: 0, mult: 0, names: [], idle: 0};
}
function nearRail(s){
  for(var i = 0; i < RAILS.length; i++){
    var R = RAILS[i];
    if(Math.abs(s.x - R.x) < 0.42 && Math.abs(s.z) < R.hd && s.y > R.top - 0.3 && s.y < R.top + 0.35) return R;
  }
  return null;
}
// the heading along a rail closest to where you are going
function railYaw(yaw){ return Math.cos(yaw) >= 0 ? 0 : PI; }

// Advance one skater by dt. input: {push, brake, steer (-1..1, +1 = left), ollie, flip, shove}
// (ollie / flip / shove are presses this frame). blocked(x, z): solid walls off the park.
function step(s, input, dt, blocked){
  var ev = [], inp = input || {};
  dt = Math.min(0.1, Math.max(0, dt));
  // small sub-steps: the quarter pipes are steep, and a slow frame must skate like a fast one.
  // A press (ollie / flip / shove) counts once, in the first sub-step.
  var n = Math.max(1, Math.ceil(dt/(1/120))), h = dt/n;
  for(var i = 0; i < n; i++){
    var one = i === 0 ? inp : {push: inp.push, brake: inp.brake, steer: inp.steer};
    if(s.bail > 0){ s.bail -= h; s.speed *= Math.max(0, 1 - 6*h); continue; }
    if(s.grind) stepGrind(s, one, h, ev);
    else if(s.air) stepAir(s, one, h, ev, blocked);
    else stepGround(s, one, h, ev, blocked);
    if(!s.air && !s.grind && s.combo.mult){ s.combo.idle += h; if(s.combo.idle > COMBO_GAP) bank(s, ev); }
  }
  return ev;
}

function move(s, dist, blocked){
  var nx = s.x + Math.sin(s.yaw)*dist, nz = s.z + Math.cos(s.yaw)*dist;
  // off the park, the world's own walls; on it, the park's edges
  var leaving = inPark(s.x, s.z) && !inPark(nx, nz) && (Math.abs(nz) > HD);   // the quarter pipes' backs
  if(leaving || (!inPark(nx, nz) && blocked && blocked(nx, nz)) || (railPost(nx, nz) && s.y < RAIL.top)) return false;
  s.x = nx; s.z = nz; return true;
}
function railPost(x, z){ return Math.abs(x - RAIL.x) < 0.12 && Math.abs(z) <= RAIL.hd; }

function stepGround(s, inp, dt, ev, blocked){
  var fwd = s.speed >= 0 ? 1 : -1;
  if(inp.push) s.speed = s.speed < TOP ? Math.min(TOP, s.speed + PUSH*dt) : s.speed;
  if(inp.brake) s.speed = s.speed > 0 ? Math.max(0, s.speed - BRAKE*dt) : Math.min(0, s.speed + BRAKE*dt);
  var grip = Math.min(1, Math.abs(s.speed)/1.5);
  s.yaw += (inp.steer || 0)*1.9*grip*dt*fwd;
  s.speed += -G*slope(s.x, s.z, s.yaw)*0.85*dt;                 // gravity along the slope
  s.speed -= Math.sign(s.speed)*Math.min(Math.abs(s.speed), FRICTION*dt);
  s.speed = Math.max(-12, Math.min(14, s.speed));
  var q0 = onQP(s.x, s.z), y0 = s.y, dz = Math.cos(s.yaw)*s.speed*dt;
  // up a quarter pipe's lip: launch straight up (you come back down the same ramp)
  if(q0 && dz*q0.end > 0 && Math.abs(s.z + dz) >= QP.from + QP.run - 0.02){
    s.z = q0.end*(QP.from + QP.run - 0.03); s.y = QP_TOP;
    s.air = true; s.vert = true; s.vy = Math.max(1.5, Math.abs(s.speed)*0.95); s.speed = 0; s.airT = 0;
    return;
  }
  if(!move(s, s.speed*dt, blocked)){ s.speed *= -0.3; return; }
  var h = height(s.x, s.z), q = onQP(s.x, s.z);
  if(h - y0 > 0.22 && !q){ s.x -= Math.sin(s.yaw)*s.speed*dt; s.z -= Math.cos(s.yaw)*s.speed*dt; s.speed *= -0.3; return; }   // a kerb: bounce
  if(h < y0 - 0.15){ s.air = true; s.vy = 0; s.airT = 0; s.y = y0; return; }                 // rolled off an edge
  s.y = h;
  if(inp.ollie){ s.air = true; s.vy = OLLIE_V + Math.max(0, slope(s.x, s.z, s.yaw)*s.speed*0.6); s.airT = 0; s.pending.push("ollie"); }
}

function stepAir(s, inp, dt, ev, blocked){
  s.airT += dt;
  if(!s.vert) move(s, s.speed*dt, blocked);
  s.vy -= G*dt; s.y += s.vy*dt;
  s.spin += (inp.steer || 0)*SPIN_RATE*dt;
  if(inp.flip && s.flip === 0 && s.shove === 0){ s.flip = 1e-6; s.pending.push("kickflip"); }
  if(inp.shove && s.shove === 0 && s.flip === 0){ s.shove = 1e-6; s.pending.push("shove"); }
  if(s.flip > 0) s.flip = Math.min(1, s.flip + dt/FLIP_T);
  if(s.shove > 0) s.shove = Math.min(1, s.shove + dt/SHOVE_T);
  if(s.flip >= 1){ s.flip = 0; }
  if(s.shove >= 1){ s.shove = 0; s.yaw += PI; s.speed = -s.speed; }   // the board turned half round under you
  // catching a rail or a ledge on the way down
  var R = s.vy < 0 && !s.vert ? nearRail(s) : null;
  if(R){
    if(s.flip > 0 || s.shove > 0 || Math.abs(spinRem(s.spin)) > LAND_TOL){ bail(s, ev, "Caught the " + R.name.toLowerCase() + " mid-trick"); return; }
    scoreAir(s, ev);
    s.grind = R; s.grindT = 0; s.air = false; s.x = R.x; s.y = R.top; s.vy = 0;
    s.yaw = railYaw(s.speed >= 0 ? s.yaw : s.yaw + PI); s.speed = Math.max(3, Math.abs(s.speed));   // along the rail, the way you were going
    ev.push({type: "grind", name: R.name});
    return;
  }
  var h = height(s.x, s.z);
  if(s.y <= h){ land(s, ev, h); }
}
// how far a spin is from a whole number of half turns
function spinRem(spin){ var k = Math.round(spin/PI); return spin - k*PI; }
function scoreAir(s, ev){
  var halves = Math.abs(Math.round(s.spin/PI));
  s.pending.forEach(function(k){ trick(s, ev, k); });
  if(s.vert) trick(s, ev, "air");
  if(halves === 1) trick(s, ev, "spin180"); else if(halves === 2) trick(s, ev, "spin360"); else if(halves >= 3) trick(s, ev, "spin540");
  if(s.airT > 1.25 && !s.vert) trick(s, ev, "bigAir", Math.round((s.airT - 1.25)*400));   // a flat ollie is ~0.95 s
  if(halves % 2 === 1){ s.yaw += PI; s.speed = -s.speed; s.fakie = !s.fakie; }   // you land facing the other way: same travel
  s.pending = []; s.spin = 0;
}
function land(s, ev, h){
  if(s.flip > 0 || s.shove > 0 || Math.abs(spinRem(s.spin)) > LAND_TOL){ bail(s, ev, s.flip > 0 || s.shove > 0 ? "Landed mid-flip" : "Landed sideways"); return; }
  var vert = s.vert, end = s.z > 0 ? 1 : -1;
  scoreAir(s, ev);
  s.air = false; s.vy = 0; s.y = h;
  if(vert){                                                      // back down the quarter pipe you went up
    s.vert = false; s.yaw = end > 0 ? PI : 0; s.speed = 3;
    s.z = end*(QP.from + QP.run - 0.25); s.y = height(s.x, s.z);
  }
  ev.push({type: "land"});
}
function stepGrind(s, inp, dt, ev){
  var R = s.grind;
  s.grindT += dt;
  s.speed = Math.max(0, s.speed - 0.6*dt);
  s.z += Math.cos(s.yaw)*s.speed*dt; s.x = R.x; s.y = R.top;
  var tick = Math.floor(s.grindT*10) - Math.floor((s.grindT - dt)*10);
  if(tick > 0){ s.combo.score += GRIND_PTS*tick; s.combo.idle = 0; }
  var off = Math.abs(s.z) > R.hd || s.speed < 0.8;
  if(inp.ollie || off){
    if(s.combo.names[s.combo.names.length - 1] !== R.name + " grind"){ s.combo.mult = Math.min(MAX_MULT, s.combo.mult + 1); s.combo.names.push(R.name + " grind"); }
    ev.push({type: "trick", name: R.name + " grind", pts: Math.round(s.grindT*10)*GRIND_PTS, combo: s.combo.score, mult: s.combo.mult});
    s.grind = null; s.air = true; s.airT = 0; s.vy = inp.ollie ? OLLIE_V*0.8 : 0;
    s.x += inp.ollie ? (s.x > 0 ? -0.5 : 0.5) : 0;            // hop off to the open side
    if(inp.ollie) s.pending.push("ollie");
  }
}

HQV.hqSkateSim = {newState: newState, step: step, kOf: kOf, K: K, TRICKS: TRICKS, spinRem: spinRem,
  consts: {TOP: TOP, OLLIE_V: OLLIE_V, LAND_TOL: LAND_TOL, COMBO_GAP: COMBO_GAP}};
})();
