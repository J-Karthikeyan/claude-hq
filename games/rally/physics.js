/* Valley Rally: car physics. Pure code: no DOM, no three.js, runs the same in node
 * (tests/rally_physics.test.js, games/rally/tools/tune.js) and in the browser
 * (window.HQV.rallyPhysics). Same house rules as the rest of games/: no build step,
 * `var` style, nothing random (the same inputs always give the same car).
 *
 * The model, in the order one fixed 120 Hz step runs it:
 *   1. A 3D rigid body (position, velocity, orientation quaternion, angular velocity
 *      in world space, a diagonal inertia tensor in body space).
 *   2. Four raycast wheels against the terrain heightfield (env.height/normal/surface):
 *      spring + damper (separate bump and rebound), travel limits with progressive
 *      bump stops, anti-roll bars; the tyre's normal load is the suspension force, so
 *      weight moves with braking, throttle and cornering, and a wheel that leaves the
 *      ground over a crest carries nothing until it lands.
 *   3. The drivetrain and tyres in SUB substeps (480 Hz): engine torque curve (turbo
 *      boost lag on the turbo car), idle, rev limiter, engine braking, an automatic
 *      clutch for launches, a 6-speed automatic with shift points and a torque cut,
 *      AWD centre split with a limited-slip coupling, LSDs per axle, brakes with a
 *      bias, a handbrake that locks the rears (and opens the centre on AWD).
 *      Each wheel's spin is integrated implicitly against a linearised tyre force, so
 *      a light wheel on a stiff tyre never oscillates.
 *   4. Tyres: a simplified Pacejka curve in normalised combined slip (friction
 *      ellipse), load sensitivity, slip ratio and slip angle with relaxation lengths
 *      (smooth at rest, holds on a slope), a low-speed damper, rolling resistance and
 *      loose-surface ploughing per surface.
 *   5. Aero drag + a little downforce, hull contact (roof, bumpers, sills) for crashes
 *      and rollovers, round obstacles (trees, rocks).
 *
 * Frames: body x = right, y = up, z = back (forward is -z, like a three.js object).
 * Heading (compass, radians or degrees) is atan2(forward.x, -forward.z): 0 = -z (north),
 * 90 deg = +x (east), the same convention as games/kart.js and the Arena referees.
 */
(function(root){
"use strict";

var G = 9.81, DT = 1/120, SUB = 4, AIR = 1.2, ASSIST_GAIN = 1.0;
var PI = Math.PI, TAU = PI*2;

function clamp(v, a, b){ return v < a ? a : v > b ? b : v; }
function sgn(v){ return v > 0 ? 1 : v < 0 ? -1 : 0; }
function lerp(a, b, u){ return a + (b - a)*u; }

/* ---------- surfaces ----------
 * mu: peak grip; kx/ky: slip ratio and tan(slip angle) at the peak; C: Pacejka shape
 * (how fast the force builds); slide: force kept far past the peak (loose surfaces keep
 * almost all of it, which is what makes a gravel drift hold); roll: rolling resistance;
 * ls: load sensitivity (grip per newton falls as the load rises: weight transfer costs
 * grip, more so on a loose surface); plough: drag per newton of load from pushing loose
 * material; rumble: for audio/camera. */
var SURFACES = [
  {id: "tarmac", name: "Tarmac", mu: 1.08, kx: 0.10, ky: 0.13, C: 1.55, slide: 0.74, ls: 0.10, roll: 0.013, plough: 0.0,   rumble: 0.04},
  {id: "gravel", name: "Gravel", mu: 0.80, kx: 0.34, ky: 0.22, C: 1.30, slide: 0.92, ls: 0.17, roll: 0.026, plough: 0.024, rumble: 0.45},
  {id: "dirt",   name: "Dirt",   mu: 0.76, kx: 0.30, ky: 0.20, C: 1.35, slide: 0.90, ls: 0.15, roll: 0.022, plough: 0.018, rumble: 0.32},
  {id: "grass",  name: "Grass",  mu: 0.56, kx: 0.30, ky: 0.24, C: 1.30, slide: 0.86, ls: 0.15, roll: 0.045, plough: 0.035, rumble: 0.55},
  {id: "mud",    name: "Mud",    mu: 0.42, kx: 0.40, ky: 0.28, C: 1.20, slide: 0.94, ls: 0.18, roll: 0.090, plough: 0.090, rumble: 0.60},
  {id: "snow",   name: "Snow",   mu: 0.48, kx: 0.32, ky: 0.25, C: 1.25, slide: 0.95, ls: 0.16, roll: 0.035, plough: 0.040, rumble: 0.22},
  {id: "loose",  name: "Loose gravel", mu: 0.66, kx: 0.36, ky: 0.25, C: 1.28, slide: 0.93, ls: 0.18, roll: 0.035, plough: 0.045, rumble: 0.70}
];
var SURF_INDEX = {};
SURFACES.forEach(function(s, i){ SURF_INDEX[s.id] = i; });

/* ---------- cars (original designs) ---------- */
// curve: [rpm, Nm] full-throttle torque. gears: forward ratios; final: final drive.
// split: share of torque to the front axle (AWD). diff locks: preload Nm + ramp x input.
var CARS = [
  {id: "moraine", name: "Moraine T4", kind: "AWD turbo hatch", drive: "awd", eng: "turbo4", plate: "MORAINE",
   mass: 1250, inertia: [1750, 1980, 560], a: 1.14, b: 1.36, track: 1.52, hpY: 0.0, lfree: 0.33, travel: 0.25, radius: 0.315, wheelI: 1.15,
   spring: [31000, 28000], bump: [2300, 2100], rebound: [3600, 3300], arb: [9000, 7500],
   engine: {idle: 900, redline: 7200, limiter: 7500, launch: 3800, inertia: 0.20, turbo: 0.35, brake: 55,
            curve: [[800, 150], [2000, 245], [3000, 355], [4000, 400], [5000, 398], [6000, 368], [7000, 320], [7600, 280]]},
   gears: [3.10, 2.12, 1.60, 1.28, 1.05, 0.88], reverse: 3.2, final: 4.85, eff: 0.88, shiftUp: 6900, shiftTime: 0.14,
   split: 0.36, centre: [70, 0.12], frontDiff: [25, 0.08], rearDiff: [140, 0.45],
   brake: 3900, bias: 0.78, hand: 2600, cda: 0.80, cla: 0.30, lock: 0.66, rcLift: 0.30, kyF: 1.0, kyR: 0.8,
   colors: {body: "#e9eef2", stripe: "#d94b3d", stripe2: "#2f5fb3", rim: "#d8d2c4", glass: "#1d2733"}},
  {id: "saltcedar", name: "Saltcedar 240R", kind: "RWD coupe", drive: "rwd", eng: "six", plate: "SALTCDR",
   mass: 1180, inertia: [1650, 1900, 520], a: 1.22, b: 1.33, track: 1.50, hpY: 0.0, lfree: 0.33, travel: 0.24, radius: 0.315, wheelI: 1.1,
   spring: [30000, 26000], bump: [2200, 2000], rebound: [3400, 3100], arb: [11000, 6500],
   engine: {idle: 850, redline: 7800, limiter: 8100, launch: 3400, inertia: 0.18, turbo: 0, brake: 50,
            curve: [[800, 125], [2000, 170], [3000, 200], [4000, 218], [5000, 232], [6000, 236], [7000, 224], [8200, 196]]},
   gears: [3.30, 2.25, 1.68, 1.32, 1.08, 0.90], reverse: 3.4, final: 4.3, eff: 0.90, shiftUp: 7550, shiftTime: 0.15,
   split: 0, centre: [0, 0], frontDiff: [0, 0], rearDiff: [100, 0.32],
   brake: 3800, bias: 0.76, hand: 2100, cda: 0.74, cla: 0.22, lock: 0.68, rcLift: 0.30, kyF: 1.0, kyR: 0.78,
   colors: {body: "#2e6a4f", stripe: "#f2c641", stripe2: "#f4efe3", rim: "#c9ccd1", glass: "#1a2129"}},
  {id: "pipit", name: "Pipit Sprint 16", kind: "FWD hot hatch", drive: "fwd", eng: "four", plate: "PIPIT 16",
   mass: 1060, inertia: [1450, 1600, 470], a: 0.98, b: 1.42, track: 1.46, hpY: 0.0, lfree: 0.32, travel: 0.23, radius: 0.305, wheelI: 1.0,
   spring: [30000, 22000], bump: [2100, 1700], rebound: [3300, 2800], arb: [7000, 9500],
   engine: {idle: 900, redline: 7600, limiter: 7900, launch: 3600, inertia: 0.15, turbo: 0, brake: 45,
            curve: [[800, 120], [2000, 175], [3000, 205], [4000, 222], [5000, 232], [6000, 228], [7000, 210], [8000, 185]]},
   gears: [3.25, 2.20, 1.66, 1.32, 1.10, 0.92], reverse: 3.3, final: 4.70, eff: 0.91, shiftUp: 7350, shiftTime: 0.12,
   split: 1, centre: [0, 0], frontDiff: [90, 0.30], rearDiff: [0, 0],
   brake: 3500, bias: 0.80, hand: 2400, cda: 0.72, cla: 0.18, lock: 0.66, rcLift: 0.28, kyF: 1.0, kyR: 0.85,
   colors: {body: "#f0b23a", stripe: "#1f2a44", stripe2: "#e7e2d6", rim: "#2a2f38", glass: "#1b232c"}}
];

/* ---------- small vector + quaternion helpers (arrays, no allocation in hot paths) ---------- */
function qNorm(q){ var l = Math.sqrt(q[0]*q[0] + q[1]*q[1] + q[2]*q[2] + q[3]*q[3]) || 1; q[0] /= l; q[1] /= l; q[2] /= l; q[3] /= l; }
// row-major 3x3: world = M * body; columns are the body axes in world space
function qToM(q, m){
  var x = q[0], y = q[1], z = q[2], w = q[3];
  var xx = x*x, yy = y*y, zz = z*z, xy = x*y, xz = x*z, yz = y*z, wx = w*x, wy = w*y, wz = w*z;
  m[0] = 1 - 2*(yy + zz); m[1] = 2*(xy - wz);     m[2] = 2*(xz + wy);
  m[3] = 2*(xy + wz);     m[4] = 1 - 2*(xx + zz); m[5] = 2*(yz - wx);
  m[6] = 2*(xz - wy);     m[7] = 2*(yz + wx);     m[8] = 1 - 2*(xx + yy);
}
function qYaw(h){ var a = -h/2; return [0, Math.sin(a), 0, Math.cos(a)]; }   // heading h (rad) -> quaternion
// heading of a car (radians, 0..2pi) from its matrix
function headingOf(m){ var fx = -m[2], fz = -m[8]; var h = Math.atan2(fx, -fz); return h < 0 ? h + TAU : h; }

/* ---------- the tyre: simplified Pacejka in normalised combined slip ---------- */
// rho = 1 at the peak; below it sin(C atan(B rho)) with B so the peak lands at rho = 1;
// past it the force falls to `slide` of the peak with an exponential tail.
function tyreShape(rho, s){
  if(rho <= 1){ var B = Math.tan(PI/(2*s.C)); return Math.sin(s.C*Math.atan(B*rho)); }
  return 1 - (1 - s.slide)*(1 - Math.exp(-(rho - 1)*1.6));
}
// shape(rho)/rho, safe at 0 (the slope there is C*B)
function tyreShapeOver(rho, s){
  if(rho < 1e-6) return s.C*Math.tan(PI/(2*s.C));
  return tyreShape(rho, s)/rho;
}

/* ---------- creation ---------- */
function create(spec, opts){
  spec = typeof spec === "number" ? CARS[clamp(spec|0, 0, CARS.length - 1)] : (spec || CARS[0]);
  opts = opts || {};
  var c = {spec: spec, p: [0, 0, 0], v: [0, 0, 0], q: [0, 0, 0, 1], w: [0, 0, 0], M: [1, 0, 0, 0, 1, 0, 0, 0, 1],
    wheels: [], gear: 1, pend: 0, shiftT: 0, since: 1, rpm: spec.engine.idle, boost: 0, locked: false, limiter: false,
    revT: 0, steerIn: 0, delta: 0, t: 0, upsideT: 0, airT: 0, landing: 0, acc: 0, assist: opts.assist !== false,
    prevP: [0, 0, 0], prevQ: [0, 0, 0, 1], hit: 0, hitT: 0,
    tel: {speed: 0, kmh: 0, rpm: 0, gear: 1, beta: 0, drift: false, slip: 0, rumble: 0, surface: 0, air: false, throttle: 0, brake: 0, steer: 0, hand: false}};
  var L = [[-1, 1], [1, 1], [-1, -1], [1, -1]];          // FL, FR, RL, RR (x sign, front?)
  for(var i = 0; i < 4; i++){
    var front = L[i][1] > 0;
    var share = 0;
    if(spec.drive === "awd") share = (front ? spec.split : 1 - spec.split)/2;
    else if(spec.drive === "rwd") share = front ? 0 : 0.5;
    else share = front ? 0.5 : 0;
    c.wheels.push({i: i, front: front, left: L[i][0] < 0, hp: [L[i][0]*spec.track/2, spec.hpY, front ? -spec.a : spec.b],
      share: share, len: spec.lfree, comp: 0, compPrev: 0, contact: false, n: [0, 1, 0], cp: [0, 0, 0], surf: 0,
      Fz: 0, omega: 0, ang: 0, kappa: 0, tanA: 0, Fx: 0, Fy: 0, vx: 0, vy: 0, slip: 0, steer: 0, drive: 0, brakeT: 0, locked: false});
  }
  return c;
}

/* ---------- placement and resets ---------- */
// Put the car on the ground at (x, z) facing heading h (radians), at rest on its springs.
function place(c, x, z, h, env, yOff){
  var s = c.spec;
  c.q = qYaw(h || 0); qToM(c.q, c.M);
  var y0 = env && env.height ? env.height(x, z) : 0;
  var sag = (s.mass*G*0.25)/((s.spring[0] + s.spring[1])/2);
  c.p = [x, y0 + s.radius + s.lfree - sag - s.hpY + (yOff || 0), z];
  c.v = [0, 0, 0]; c.w = [0, 0, 0];
  c.prevP = c.p.slice(); c.prevQ = c.q.slice();
  c.gear = 1; c.pend = 0; c.shiftT = 0; c.since = 1; c.rpm = s.engine.idle; c.boost = 0; c.revT = 0;
  c.steerIn = 0; c.delta = 0; c.upsideT = 0; c.airT = 0; c.landing = 0; c.acc = 0; c.hit = 0;
  c.wheels.forEach(function(w){ w.omega = 0; w.kappa = 0; w.tanA = 0; w.comp = 0; w.compPrev = 0; w.len = s.lfree; w.Fx = 0; w.Fy = 0; w.Fz = 0; });
  // settle on the springs for a moment so the first frame is calm
  if(env && env.height){
    var ctl = {throttle: 0, brake: 0, steer: 0, handbrake: 0, hold: true};
    for(var k = 0; k < 36; k++) step(c, ctl, env, DT);
    c.v = [0, 0, 0]; c.w = [0, 0, 0];
    c.wheels.forEach(function(w){ w.omega = 0; w.kappa = 0; w.tanA = 0; });
    c.prevP = c.p.slice(); c.prevQ = c.q.slice();
  }
  c.t = 0;
  return c;
}

/* ---------- engine ---------- */
function curveAt(e, rpm){
  var cv = e.curve, n = cv.length;
  if(rpm <= cv[0][0]) return cv[0][1]*Math.max(0, rpm/cv[0][0]);
  for(var i = 1; i < n; i++){
    if(rpm <= cv[i][0]){ var u = (rpm - cv[i-1][0])/(cv[i][0] - cv[i-1][0]); return lerp(cv[i-1][1], cv[i][1], u); }
  }
  return cv[n-1][1];
}
// Net crank torque at rpm and throttle (0..1): drive minus pumping/friction losses.
function engineTorque(c, rpm, thr){
  var e = c.spec.engine;
  if(c.limiter) thr = 0;
  var full = curveAt(e, rpm);
  if(e.turbo) full *= 0.62 + 0.38*c.boost;
  var loss = e.brake*(0.25 + 0.75*clamp(rpm/e.redline, 0, 1.2));
  return thr*full - (1 - thr)*loss;
}
function gearRatio(c, g){
  var s = c.spec; g = g == null ? c.gear : g;
  if(g < 0) return -s.reverse*s.final;
  if(g === 0) return 0;
  return s.gears[clamp(g, 1, s.gears.length) - 1]*s.final;
}
function rpmFromOmega(om, ratio){ return om*ratio*60/TAU; }

/* ---------- terrain ray ---------- */
var NTMP = [0, 1, 0];
function terrainNormal(env, x, z, out){
  if(env.normal){ env.normal(x, z, out); return out; }
  var e = 0.4, hx = env.height(x + e, z) - env.height(x - e, z), hz = env.height(x, z + e) - env.height(x, z - e);
  var nx = -hx/(2*e), nz = -hz/(2*e), l = Math.sqrt(nx*nx + 1 + nz*nz);
  out[0] = nx/l; out[1] = 1/l; out[2] = nz/l; return out;
}
// distance along direction d from P to the heightfield, or -1 past maxT. Newton on
// f(t) = P.y + d.y t - H(P.xz + d.xz t), seeded with the vertical guess.
function rayTerrain(env, P, d, maxT, nOut){
  var t = d[1] < -0.2 ? (P[1] - env.height(P[0], P[2]))/(-d[1]) : 0;
  if(t < -1) t = -1;
  for(var k = 0; k < 4; k++){
    var x = P[0] + d[0]*t, z = P[2] + d[2]*t;
    var f = P[1] + d[1]*t - env.height(x, z);
    terrainNormal(env, x, z, nOut);
    var gx = -nOut[0]/nOut[1], gz = -nOut[2]/nOut[1];
    var fp = d[1] - (gx*d[0] + gz*d[2]);
    if(Math.abs(fp) < 0.05) fp = fp < 0 ? -0.05 : 0.05;
    var dt = f/fp; t -= dt;
    if(Math.abs(dt) < 1e-4) break;
  }
  if(!(t === t)) return -1;
  return t > maxT ? -1 : t;
}

/* ---------- one fixed step ---------- */
// ctl: {throttle 0..1, brake 0..1, steer -1..1, handbrake 0..1, analog: bool (gamepad),
//       hold: bool (parked: brakes on, no drive)}
var TMP = {F: [0, 0, 0], T: [0, 0, 0]};
function step(c, ctl, env, dt){
  dt = dt || DT;
  var s = c.spec, M = c.M, p = c.p, v = c.v, w = c.w, e = s.engine;
  ctl = ctl || {};
  var thrIn = clamp(+ctl.throttle || 0, 0, 1), brkIn = clamp(+ctl.brake || 0, 0, 1), hand = clamp(+ctl.handbrake || 0, 0, 1);
  var steerIn = clamp(+ctl.steer || 0, -1, 1), hold = !!ctl.hold;
  c.sinceHB = hand > 0.3 ? 0 : (c.sinceHB == null ? 9 : c.sinceHB + dt);
  qToM(c.q, M);
  c.t += dt; c.since += dt;
  var right = [M[0], M[3], M[6]], up = [M[1], M[4], M[7]], back = [M[2], M[5], M[8]];
  var vFwd = -(v[0]*back[0] + v[1]*back[1] + v[2]*back[2]), vRight = v[0]*right[0] + v[1]*right[1] + v[2]*right[2];
  var speed = Math.sqrt(v[0]*v[0] + v[1]*v[1] + v[2]*v[2]);

  /* --- gearbox: reverse selection, then automatic shifts --- */
  if(c.gear > 0){
    if(Math.abs(vFwd) < 1.0 && brkIn > 0.5 && thrIn < 0.1 && !hold){ c.revT += dt; if(c.revT > 0.3){ c.gear = -1; c.pend = 0; c.shiftT = 0; c.revT = 0; } }
    else c.revT = 0;
  } else if(c.gear < 0){
    if(vFwd > -1.0 && thrIn > 0.5 && brkIn < 0.1){ c.revT += dt; if(c.revT > 0.15){ c.gear = 1; c.revT = 0; } }
    else c.revT = 0;
  }
  var thr = c.gear < 0 ? brkIn : thrIn, brk = c.gear < 0 ? thrIn : brkIn;
  if(hold){ thr = 0; brk = 1; }
  // the slide angle and how fast it is changing (for the assist)
  var beta = speed > 1.5 ? Math.atan2(vRight, Math.max(0.5, Math.abs(vFwd))) : 0;
  var dBeta = c.betaPrev != null ? (beta - c.betaPrev)/dt : 0;
  c.betaPrev = beta;
  // Stability assist, engine side (an ESC-style intervention, only in a slide that is growing,
  // never plain traction control): less throttle as the slide angle and its growth rise, and
  // less again while the driven wheels spin up in that slide.
  if(c.assist && c.gear > 0 && speed > 4){
    var grow = dBeta*beta > 0, ab = Math.abs(beta);
    if(grow && ab > 0.22) thr *= clamp(1 - (ab - 0.22)/0.4 - Math.min(0.4, Math.abs(dBeta)*0.25), 0.15, 1);
    if(ab > 0.15){
      var spin = 0; c.wheels.forEach(function(wh){ if(wh.share > 0) spin = Math.max(spin, wh.kappa); });
      if(spin > 0.45) thr *= clamp(1 - (spin - 0.45)*1.2, 0.3, 1);
    }
  }
  // the driven wheels' speed, as the gearbox sees it
  var drivenOm = 0;
  c.wheels.forEach(function(wh){ drivenOm += wh.omega*wh.share; });
  if(c.shiftT > 0){
    c.shiftT -= dt;
    if(c.shiftT <= 0){ c.gear = c.pend; c.shiftT = 0; c.since = 0; }
  } else if(c.gear > 0){
    // Shift on the slower of wheel speed and road speed: wheelspin alone never upshifts
    // (it bounces off the limiter instead, like a sequential box held in gear).
    var gr = gearRatio(c), wr = rpmFromOmega(drivenOm, gr), road = rpmFromOmega(Math.max(0, vFwd)/s.radius, gr);
    var nG = s.gears.length, rpmUp = Math.min(wr, road*1.12);
    // bouncing off the limiter on wheelspin for a while: short-shift, as a driver would
    c.limT = wr > s.shiftUp && thr > 0.5 ? (c.limT || 0) + dt : 0;
    var nextRoad = c.gear < nG ? rpmFromOmega(Math.max(0, vFwd)/s.radius, gearRatio(c, c.gear + 1)) : 0;
    if(c.limT > 0.35 && nextRoad > 2600) rpmUp = s.shiftUp + 1;
    if(c.gear < nG && rpmUp > s.shiftUp && c.since > 0.3 && thr > 0.05 && c.airT < 0.3){
      c.pend = c.gear + 1; c.shiftT = s.shiftTime; c.upAt = c.t;
    } else if(c.gear > 1 && c.since > (thr > 0.6 ? 0.9 : 0.4) && (c.t - (c.upAt || -9) > 2 || brk > 0.2 || thr < 0.3)){
      var gl = gearRatio(c, c.gear - 1), lower = rpmFromOmega(Math.max(0, vFwd)/s.radius, gl);
      var down = brk > 0.2 ? 0.6*e.redline : thr > 0.6 ? 0.52*e.redline : 0.34*e.redline;
      down = Math.min(down, 0.86*s.shiftUp*gr/gl);
      var rpmDown = Math.max(wr*0.9, road);
      if(rpmDown < down && lower < s.shiftUp - 500){ c.pend = c.gear - 1; c.shiftT = s.shiftTime*0.7; }
    }
  }

  /* --- steering: speed-sensitive lock and rate, keyboard smoothing, assist --- */
  // A stick keeps most of the lock at speed (you need it to catch a slide); a key, which
  // is all or nothing, gets less the faster you go (the assist adds the countersteer).
  // At speed the front tyres reach their grip at only ~5 degrees, so a key (all or nothing) gets
  // about that much plus a margin; a stick keeps more lock (20 degrees and up) to catch slides.
  var lock = ctl.analog ? Math.max(0.34, s.lock/(1 + speed/30)) : Math.max(0.08, s.lock/(1 + Math.max(0, speed - 3)/5));
  c.lockNow = lock;
  if(ctl.analog){
    c.steerIn += (steerIn - c.steerIn)*Math.min(1, dt*18);
  } else {
    var rate = 5.2/(1 + speed/9), back2 = Math.abs(steerIn) < Math.abs(c.steerIn) || steerIn*c.steerIn < 0 ? 1.7 : 1;
    var d = steerIn - c.steerIn, mx = rate*back2*dt;
    c.steerIn += clamp(d, -mx, mx);
  }
  var assist = 0, on = 0;
  if(c.assist && vFwd > 3 && c.gear > 0 && c.airT === 0){   // (not in the air: there is nothing to catch)
    // Once the car is sliding (past ~8 degrees, fully by ~20), point the front wheels along the
    // front axle's own travel: no front slip, so the fronts stop adding rotation to the slide.
    var fade = clamp((vFwd - 3)/6, 0, 1);
    on = clamp((Math.abs(beta) - 0.14)/0.2, 0, 1);
    // a handbrake turn is on purpose: the assist lets the car rotate, then catches it
    on *= clamp(((c.sinceHB || 9) - 0.15)/0.6, 0, 1);
    var wUp = w[0]*up[0] + w[1]*up[1] + w[2]*up[2], betaF = Math.atan2(vRight - wUp*s.a, Math.max(0.5, vFwd));
    assist = clamp(betaF*on*fade*ASSIST_GAIN, -s.lock, s.lock);
  }
  // In a slide the assist steers relative to the travel; your keys then ask for that much more
  // angle into the slide (up to ~20 degrees), which is how a held drift is steered.
  var assistOn = typeof on === "number" ? on : 0;
  // ...and with the handbrake up (and just after) a key gets real lock, for a handbrake turn.
  var kbLock = lerp(lock, Math.max(lock, 0.35), assistOn);
  if(!ctl.analog && c.sinceHB < 0.6) kbLock = Math.max(kbLock, lerp(0.45, lock, c.sinceHB/0.6));
  var want = c.steerIn*(ctl.analog ? lock : kbLock);
  // The assist never doubles your own countersteer: steering the same way it does, you get the
  // larger of the two; steering into the slide, yours is added (so you can still hold an angle).
  c.delta = clamp(want*assist > 0 ? (Math.abs(want) > Math.abs(assist) ? want : assist) : want + assist, -s.lock, s.lock);

  /* --- engine speed, clutch and the torque the gearbox puts out --- */
  var ratio = gearRatio(c);
  if(e.turbo){ var bt = thr*clamp((c.rpm - 1800)/2200, 0, 1); c.boost += (bt - c.boost)*(1 - Math.exp(-dt/e.turbo)); }
  if(c.rpm > e.limiter) c.limiter = true; else if(c.rpm < e.limiter - 250) c.limiter = false;

  /* --- wheels: suspension rays, loads, contact frames --- */
  var Ftot = [0, -s.mass*G, 0], Ttot = [0, 0, 0];
  var down = [-up[0], -up[1], -up[2]], maxT = s.lfree + s.radius;
  var grounded = 0, rumble = 0, surfSum = [0, 0, 0, 0, 0, 0, 0];
  var i, wh, k;
  var P = [0, 0, 0], r = [0, 0, 0];
  for(i = 0; i < 4; i++){
    wh = c.wheels[i];
    var hp = wh.hp;
    r[0] = M[0]*hp[0] + M[1]*hp[1] + M[2]*hp[2]; r[1] = M[3]*hp[0] + M[4]*hp[1] + M[5]*hp[2]; r[2] = M[6]*hp[0] + M[7]*hp[1] + M[8]*hp[2];
    P[0] = p[0] + r[0]; P[1] = p[1] + r[1]; P[2] = p[2] + r[2];
    var t = env ? rayTerrain(env, P, down, maxT, wh.n) : -1;
    wh.compPrev = wh.comp;
    if(t < 0 && t !== -1) t = 0;
    if(t === -1 || up[1] < 0.05){
      wh.contact = false; wh.len = Math.min(s.lfree, wh.len + 1.5*dt); wh.comp = 0; wh.Fz = 0; continue;
    }
    wh.contact = true; grounded++;
    wh.len = Math.max(0, t - s.radius);
    wh.cp[0] = P[0] + down[0]*t; wh.cp[1] = P[1] + down[1]*t; wh.cp[2] = P[2] + down[2]*t;
    wh.comp = s.lfree - wh.len;
    wh.surf = env.surface ? clamp(env.surface(wh.cp[0], wh.cp[2])|0, 0, SURFACES.length - 1) : 0;
  }
  // anti-roll bars: per axle, on the compression difference
  var arbF = 0, arbR = 0;
  if(c.wheels[0].contact && c.wheels[1].contact) arbF = s.arb[0]*(c.wheels[0].comp - c.wheels[1].comp);
  if(c.wheels[2].contact && c.wheels[3].contact) arbR = s.arb[1]*(c.wheels[2].comp - c.wheels[3].comp);
  for(i = 0; i < 4; i++){
    wh = c.wheels[i];
    if(!wh.contact) continue;
    var ax = wh.front ? 0 : 1;
    var cv = (wh.comp - wh.compPrev)/dt;
    var fs = s.spring[ax]*wh.comp + (cv > 0 ? s.bump[ax] : s.rebound[ax])*cv;
    fs += (wh.left ? 1 : -1)*(wh.front ? arbF : arbR);
    var minLen = s.lfree - s.travel;
    if(wh.len < minLen){ var o = minLen - wh.len; fs += 90000*o + 1.2e6*o*o + (cv > 0 ? 6000*cv : 0); }
    wh.Fz = Math.max(0, Math.min(fs, 60000));
    // landing: remember the biggest hit for the camera / audio
    if(cv > 2.5) c.landing = Math.max(c.landing, Math.min(1, (cv - 2.5)/6));
  }

  /* --- per wheel contact kinematics --- */
  var steerS = Math.sin(c.delta), steerC = Math.cos(c.delta);
  for(i = 0; i < 4; i++){
    wh = c.wheels[i];
    wh.steer = wh.front ? c.delta : 0;
    if(!wh.contact){ wh.vx = 0; wh.vy = 0; continue; }
    var n = wh.n;
    // wheel forward in body: (sin d, 0, -cos d)
    var fb0 = wh.front ? steerS : 0, fb2 = wh.front ? -steerC : -1;
    var fx = M[0]*fb0 + M[2]*fb2, fy = M[3]*fb0 + M[5]*fb2, fz = M[6]*fb0 + M[8]*fb2;
    var dn = fx*n[0] + fy*n[1] + fz*n[2]; fx -= n[0]*dn; fy -= n[1]*dn; fz -= n[2]*dn;
    var fl = Math.sqrt(fx*fx + fy*fy + fz*fz) || 1; fx /= fl; fy /= fl; fz /= fl;
    // lateral (right) = f x n
    var lx = fy*n[2] - fz*n[1], ly = fz*n[0] - fx*n[2], lz = fx*n[1] - fy*n[0];
    wh.fw = wh.fw || [0, 0, 0]; wh.lat = wh.lat || [0, 0, 0];
    wh.fw[0] = fx; wh.fw[1] = fy; wh.fw[2] = fz; wh.lat[0] = lx; wh.lat[1] = ly; wh.lat[2] = lz;
    var rx = wh.cp[0] - p[0], ry = wh.cp[1] - p[1], rz = wh.cp[2] - p[2];
    var vcx = v[0] + w[1]*rz - w[2]*ry, vcy = v[1] + w[2]*rx - w[0]*rz, vcz = v[2] + w[0]*ry - w[1]*rx;
    wh.vx = vcx*fx + vcy*fy + vcz*fz; wh.vy = vcx*lx + vcy*ly + vcz*lz;
  }

  /* --- drivetrain + tyres, SUB substeps --- */
  var h = dt/SUB;
  var brakeTot = s.brake*brk;
  for(i = 0; i < 4; i++){ c.wheels[i].Fx = 0; c.wheels[i].Fy = 0; c.wheels[i].slip = 0; }
  // hill hold: a stopped car with no throttle keeps its brakes on until you drive off
  c.parked = !hold && thr < 0.05 && (c.parked || Math.abs(vFwd) < 0.3);
  var openClutch = c.shiftT > 0 || c.gear === 0 || (hand > 0.3 && s.drive === "rwd");
  for(var sub = 0; sub < SUB; sub++){
    // engine and clutch
    drivenOm = 0;
    for(i = 0; i < 4; i++) drivenOm += c.wheels[i].omega*c.wheels[i].share;
    var wheelRpm = rpmFromOmega(drivenOm, ratio);
    var Tout = 0, refl = 0;
    if(openClutch){
      var target = e.idle + thr*(e.limiter - e.idle)*0.85;
      if(c.shiftT > 0 && c.pend > 0) target = Math.max(e.idle, rpmFromOmega(drivenOm, gearRatio(c, c.pend)));
      c.rpm += (target - c.rpm)*Math.min(1, h*(c.shiftT > 0 ? 25 : 6));
      c.locked = false;
    } else {
      var lockRpm = thr > 0.05 ? e.idle + thr*(e.launch - e.idle) : e.idle*1.15;
      if(wheelRpm >= lockRpm - 40 && wheelRpm > e.idle*0.95){
        c.rpm = wheelRpm; c.locked = true;
        Tout = engineTorque(c, c.rpm, thr)*ratio*s.eff;
        refl = e.inertia*ratio*ratio;
      } else {
        c.locked = false;
        var tgt = Math.max(e.idle, lockRpm);
        c.rpm += (tgt - c.rpm)*Math.min(1, h*7);
        if(thr > 0.02){
          // the clutch slips at launch: it passes the engine's torque at its own rpm
          var engage = clamp((c.rpm - e.idle)/(e.launch - e.idle + 1), 0.15, 1);
          Tout = Math.max(0, engineTorque(c, c.rpm, thr))*engage*ratio*s.eff;
        }
      }
    }
    if(c.rpm < e.idle*0.6) c.rpm = e.idle*0.6;
    // centre and axle differentials
    var Tf = 0, Tr = 0, wf = 0, wr2 = 0;
    if(s.drive === "awd"){
      if(hand > 0.3){ Tf = Tout; Tr = 0; }
      else {
        Tf = Tout*s.split; Tr = Tout*(1 - s.split);
        var omF = (c.wheels[0].omega + c.wheels[1].omega)/2, omR = (c.wheels[2].omega + c.wheels[3].omega)/2;
        var capC = s.centre[0] + s.centre[1]*Math.abs(Tout), xc = capC*Math.tanh((omF - omR)/2.5);
        Tf -= xc; Tr += xc;
      }
    } else if(s.drive === "rwd") Tr = Tout; else Tf = Tout;
    var Tw = [0, 0, 0, 0];
    function axle(T, A, B, dl){
      var ta = T/2, tb = T/2, cap = dl[0] + dl[1]*Math.abs(T), x = cap*Math.tanh((c.wheels[A].omega - c.wheels[B].omega)/3);
      Tw[A] = ta - x; Tw[B] = tb + x;
    }
    axle(Tf, 0, 1, s.frontDiff); axle(Tr, 2, 3, s.rearDiff);
    // each wheel: implicit spin update against the linearised tyre force
    for(i = 0; i < 4; i++){
      wh = c.wheels[i];
      var I = s.wheelI + (c.locked ? refl*wh.share : 0);
      var R = s.radius;
      // brakes (+ hill hold, + handbrake on the rears)
      var Tb = brakeTot*(wh.front ? s.bias : 1 - s.bias)/2;
      if(!wh.front && hand > 0) Tb += s.hand*hand;
      if(c.parked) Tb = Math.max(Tb, 1100);
      wh.brakeT = Tb;
      if(!wh.contact){
        // spinning in the air: drive and brakes only
        var om = wh.omega + h*Tw[i]/I;
        var bl = (Tb + 2)*h/I;
        om = Math.abs(om) <= bl ? 0 : om - sgn(om)*bl;
        wh.omega = om; wh.kappa *= 0.9; wh.tanA *= 0.9; continue;
      }
      var S = SURFACES[wh.surf];
      var Fz = wh.Fz, vx = wh.vx, vy = wh.vy;
      var mu = S.mu*clamp(1 - S.ls*(Fz/3600 - 1), 0.62, 1.15);
      var Dx = mu*Fz, Dy = mu*Fz*0.97;
      // the rear tyres reach their grip at a smaller slip angle than the fronts (stiffer in
      // cornering): a car with a stable, slightly understeering balance that you rotate on purpose
      var kyA = S.ky*(wh.front ? s.kyF : s.kyR);
      var a = Math.abs(vx);
      // relaxation lengths, integrated exactly over the substep
      var sx = 0.16, sy = 0.38;
      // sigma dk/dt = slip velocity - |vx| k: k(t+h) = k e^(-|vx|h/sigma) + (slip velocity/sigma) ph
      var ex = Math.exp(-a*h/sx), phx = a > 1e-4 ? (1 - ex)*sx/a : h;
      var ey = Math.exp(-a*h/sy), phy = a > 1e-4 ? (1 - ey)*sy/a : h;
      var tanA = clamp(wh.tanA*ey + vy*phy/sy, -6, 6);
      // the slip ratio for a given wheel speed om: k0 + k1*om
      var k0 = wh.kappa*ex - vx*phx/sx, k1 = R*phx/sx;
      function fxAt(om){
        var kap = clamp(k0 + k1*om, -6, 6);
        var rx = kap/S.kx, ry = tanA/kyA, rho = Math.sqrt(rx*rx + ry*ry);
        return Dx*tyreShapeOver(rho, S)*rx;
      }
      var om0 = wh.omega;
      var F0 = fxAt(om0), eps = 0.05, J = (fxAt(om0 + eps) - F0)/eps;
      if(!(J > 0)) J = 0;
      var Troll = S.roll*Fz*R*clamp(a/0.5, 0, 1);
      var om1 = om0 + h*(Tw[i] - R*F0)/(I + h*R*J);
      // brakes and rolling resistance can stop a wheel but never spin it backwards
      var fr = (Tb + Troll)*h/(I + h*R*J);
      wh.locked = false;
      if(Math.abs(om1) <= fr){ om1 = 0; wh.locked = Tb > 50; } else om1 -= sgn(om1)*fr;
      var kap = clamp(k0 + k1*om1, -6, 6);
      wh.omega = om1; wh.kappa = kap; wh.tanA = tanA;
      var rx2 = kap/S.kx, ry2 = tanA/kyA, rho2 = Math.sqrt(rx2*rx2 + ry2*ry2), so = tyreShapeOver(rho2, S);
      var Fx = Dx*so*rx2, Fy = -Dy*so*ry2;
      // low-speed damper: the tyre's carcass at a crawl (fades out by 3 m/s)
      // (by the contact patch's whole speed: a car sliding sideways is not crawling)
      var vtot = Math.sqrt(vx*vx + vy*vy), lowk = clamp(1 - vtot/2.5, 0, 1);
      if(lowk > 0){
        Fy -= clamp(lowk*Fz*0.9*vy, -Dy, Dy);
        if(wh.locked || Math.abs(om1) < 0.01) Fx -= clamp(lowk*Fz*0.9*vx, -Dx, Dx);
      }
      // ploughing loose material
      if(S.plough > 0){
        var vt = Math.sqrt(vx*vx + vy*vy);
        if(vt > 0.05){ var pf = S.plough*Fz*clamp(vt/8, 0, 1.6); Fx -= pf*vx/vt; Fy -= pf*vy/vt; }
      }
      wh.Fx += Fx/SUB; wh.Fy += Fy/SUB;
      wh.slip = Math.max(wh.slip, rho2);
    }
  }
  if(c.rpm > e.limiter + 300) c.rpm = e.limiter + 300;

  /* --- forces on the body --- */
  for(i = 0; i < 4; i++){
    wh = c.wheels[i];
    if(!wh.contact) continue;
    var Sf = SURFACES[wh.surf];
    rumble += Sf.rumble*clamp(Math.abs(wh.vx)/12, 0, 1); surfSum[wh.surf] += 1;
    // suspension along the body's up axis, at the hardpoint
    var hp2 = wh.hp;
    r[0] = M[0]*hp2[0] + M[1]*hp2[1] + M[2]*hp2[2]; r[1] = M[3]*hp2[0] + M[4]*hp2[1] + M[5]*hp2[2]; r[2] = M[6]*hp2[0] + M[7]*hp2[1] + M[8]*hp2[2];
    addForce(Ftot, Ttot, r, up[0]*wh.Fz, up[1]*wh.Fz, up[2]*wh.Fz);
    // tyre forces in the contact plane: longitudinal at the contact patch (full pitch
    // transfer under brakes and throttle), lateral a little above it (the roll centre)
    r[0] = wh.cp[0] - p[0]; r[1] = wh.cp[1] - p[1]; r[2] = wh.cp[2] - p[2];
    addForce(Ftot, Ttot, r, wh.fw[0]*wh.Fx, wh.fw[1]*wh.Fx, wh.fw[2]*wh.Fx);
    var lift = s.rcLift;
    r[0] += up[0]*lift; r[1] += up[1]*lift; r[2] += up[2]*lift;
    addForce(Ftot, Ttot, r, wh.lat[0]*wh.Fy, wh.lat[1]*wh.Fy, wh.lat[2]*wh.Fy);
  }
  // aero: drag on the whole body, downforce along -up at the axles
  if(speed > 0.1){
    var dq = 0.5*AIR*s.cda*speed;
    Ftot[0] -= dq*v[0]; Ftot[1] -= dq*v[1]; Ftot[2] -= dq*v[2];
    var dfv = 0.5*AIR*s.cla*vFwd*vFwd;
    Ftot[0] -= up[0]*dfv; Ftot[1] -= up[1]*dfv; Ftot[2] -= up[2]*dfv;
  }
  // hull contact: bumpers, sills, roof against the ground
  hull(c, env, Ftot, Ttot, right, up, back);
  if(env && env.obstacles) obstacles(c, env, Ftot, Ttot, right, back);
  // Stability assist (keyboard default): past ~30 degrees of slide, resist the slide
  // *growing* (a held drift, whose angle is steady, is left alone).
  if(c.assist && vFwd > 4 && hand < 0.3 && dBeta*beta > 0 && Math.abs(beta) > 0.35){
    var yk = s.inertia[1]*clamp((Math.abs(beta) - 0.35)/0.35, 0, 1)*Math.min(Math.abs(dBeta), 3)*2.2;
    var ty = -sgn(beta)*yk;
    Ttot[0] += up[0]*ty; Ttot[1] += up[1]*ty; Ttot[2] += up[2]*ty;
  }

  /* --- integrate (semi-implicit Euler) --- */
  var im = 1/s.mass;
  v[0] += Ftot[0]*im*dt; v[1] += Ftot[1]*im*dt; v[2] += Ftot[2]*im*dt;
  var sp2 = Math.sqrt(v[0]*v[0] + v[1]*v[1] + v[2]*v[2]);
  if(sp2 > 95){ v[0] *= 95/sp2; v[1] *= 95/sp2; v[2] *= 95/sp2; }
  p[0] += v[0]*dt; p[1] += v[1]*dt; p[2] += v[2]*dt;
  // angular: in body space with the gyroscopic term
  var tb0 = M[0]*Ttot[0] + M[3]*Ttot[1] + M[6]*Ttot[2], tb1 = M[1]*Ttot[0] + M[4]*Ttot[1] + M[7]*Ttot[2], tb2 = M[2]*Ttot[0] + M[5]*Ttot[1] + M[8]*Ttot[2];
  var wb0 = M[0]*w[0] + M[3]*w[1] + M[6]*w[2], wb1 = M[1]*w[0] + M[4]*w[1] + M[7]*w[2], wb2 = M[2]*w[0] + M[5]*w[1] + M[8]*w[2];
  var I0 = s.inertia[0], I1 = s.inertia[1], I2 = s.inertia[2];
  var L0 = I0*wb0, L1 = I1*wb1, L2 = I2*wb2;
  var gy0 = wb1*L2 - wb2*L1, gy1 = wb2*L0 - wb0*L2, gy2 = wb0*L1 - wb1*L0;
  wb0 += (tb0 - gy0)/I0*dt; wb1 += (tb1 - gy1)/I1*dt; wb2 += (tb2 - gy2)/I2*dt;
  w[0] = M[0]*wb0 + M[1]*wb1 + M[2]*wb2; w[1] = M[3]*wb0 + M[4]*wb1 + M[5]*wb2; w[2] = M[6]*wb0 + M[7]*wb1 + M[8]*wb2;
  var wl = Math.sqrt(w[0]*w[0] + w[1]*w[1] + w[2]*w[2]);
  if(wl > 25){ w[0] *= 25/wl; w[1] *= 25/wl; w[2] *= 25/wl; }
  var q = c.q, hx = 0.5*dt;
  var qx = q[0], qy = q[1], qz = q[2], qw = q[3];
  q[0] += hx*( w[0]*qw + w[1]*qz - w[2]*qy);
  q[1] += hx*(-w[0]*qz + w[1]*qw + w[2]*qx);
  q[2] += hx*( w[0]*qy - w[1]*qx + w[2]*qw);
  q[3] += hx*(-w[0]*qx - w[1]*qy - w[2]*qz);
  qNorm(q);
  qToM(q, M);
  // spin angles for the visuals
  for(i = 0; i < 4; i++){ wh = c.wheels[i]; wh.ang = (wh.ang + wh.omega*dt) % TAU; }

  /* --- bookkeeping and telemetry --- */
  c.airT = grounded === 0 ? c.airT + dt : 0;
  c.upsideT = M[4] < 0.15 ? c.upsideT + dt : 0;
  c.landing *= Math.exp(-dt*4);
  c.hitT = Math.max(0, c.hitT - dt);
  var T = c.tel, best = 0, bi = 0;
  for(k = 0; k < surfSum.length; k++) if(surfSum[k] > best){ best = surfSum[k]; bi = k; }
  var vF2 = -(v[0]*M[2] + v[1]*M[5] + v[2]*M[8]), vR2 = v[0]*M[0] + v[1]*M[3] + v[2]*M[6];
  T.speed = Math.sqrt(v[0]*v[0] + v[1]*v[1] + v[2]*v[2]); T.kmh = T.speed*3.6; T.vFwd = vF2;
  T.rpm = c.rpm; T.gear = c.shiftT > 0 ? c.gear : c.gear; T.shifting = c.shiftT > 0;
  T.beta = T.speed > 2 ? Math.atan2(vR2, Math.abs(vF2)) : 0;
  T.drift = T.speed > 8 && Math.abs(T.beta) > 0.24 && grounded >= 2;
  var sl = 0; for(i = 0; i < 4; i++) sl = Math.max(sl, c.wheels[i].slip);
  T.slip = sl; T.rumble = grounded ? rumble/grounded : 0; if(grounded) T.surface = bi; T.air = grounded === 0;
  T.throttle = thr; T.brake = brk; T.steer = c.steerIn; T.hand = hand > 0.3; T.boost = c.boost; T.grounded = grounded;
  T.heading = headingOf(M);
}

function addForce(F, T, r, fx, fy, fz){
  F[0] += fx; F[1] += fy; F[2] += fz;
  T[0] += r[1]*fz - r[2]*fy; T[1] += r[2]*fx - r[0]*fz; T[2] += r[0]*fy - r[1]*fx;
}

// The body's shell: 8 box corners + roof centre + nose/tail mids, penalty contacts.
var HULL = [[-0.84, -0.22, -2.02], [0.84, -0.22, -2.02], [-0.84, -0.22, 2.0], [0.84, -0.22, 2.0],
            [-0.80, 0.78, -1.0], [0.80, 0.78, -1.0], [-0.80, 0.78, 1.2], [0.80, 0.78, 1.2],
            [0, 0.84, 0.1], [0, -0.05, -2.12], [0, -0.05, 2.1], [-0.9, 0.2, 0], [0.9, 0.2, 0]];
var HN = [0, 1, 0];
function hull(c, env, F, T, right, up, back){
  if(!env || !env.height) return;
  var M = c.M, p = c.p, v = c.v, w = c.w;
  for(var i = 0; i < HULL.length; i++){
    var b = HULL[i];
    var rx = M[0]*b[0] + M[1]*b[1] + M[2]*b[2], ry = M[3]*b[0] + M[4]*b[1] + M[5]*b[2], rz = M[6]*b[0] + M[7]*b[1] + M[8]*b[2];
    var x = p[0] + rx, y = p[1] + ry, z = p[2] + rz;
    var gh = env.height(x, z), pen = gh - y;
    if(pen <= 0) continue;
    terrainNormal(env, x, z, HN);
    pen *= HN[1];
    var vx = v[0] + w[1]*rz - w[2]*ry, vy = v[1] + w[2]*rx - w[0]*rz, vz = v[2] + w[0]*ry - w[1]*rx;
    var vn = vx*HN[0] + vy*HN[1] + vz*HN[2];
    // a stiff, damped contact, capped per point: a bank at speed stops the car, never launches it
    var N = Math.min(70000, 140000*Math.min(pen, 0.4) - 9000*Math.max(vn, -6));
    if(N <= 0) continue;
    if(vn < -3) { c.hit = Math.max(c.hit, Math.min(1, -vn/10)); c.hitT = 0.3; }
    var tx = vx - HN[0]*vn, ty = vy - HN[1]*vn, tz = vz - HN[2]*vn, tl = Math.sqrt(tx*tx + ty*ty + tz*tz);
    var fr = tl > 1e-3 ? 0.55*N*Math.min(1, tl/0.6)/tl : 0;
    addForce(F, T, [rx, ry, rz], HN[0]*N - tx*fr, HN[1]*N - ty*fr, HN[2]*N - tz*fr);
  }
}
// Round obstacles (tree trunks, rocks): env.obstacles(x, z, radius, cb(ox, oz, or))
var OB_PTS = [[-0.86, -2.0], [0.86, -2.0], [-0.86, 2.0], [0.86, 2.0], [0, -2.1], [0, 2.1], [-0.9, 0], [0.9, 0]];
function obstacles(c, env, F, T){
  var M = c.M, p = c.p, v = c.v, w = c.w;
  env.obstacles(p[0], p[2], 3.0, function(ox, oz, orr){
    for(var i = 0; i < OB_PTS.length; i++){
      var b = OB_PTS[i];
      var rx = M[0]*b[0] + M[2]*b[1], ry = M[3]*b[0] + M[5]*b[1], rz = M[6]*b[0] + M[8]*b[1];
      var dx = p[0] + rx - ox, dz = p[2] + rz - oz, d = Math.sqrt(dx*dx + dz*dz);
      if(d >= orr || d < 1e-4) continue;
      var nx = dx/d, nz = dz/d, pen = orr - d;
      var vx = v[0] + w[1]*rz - w[2]*ry, vz = v[2] + w[0]*ry - w[1]*rx;
      var vn = vx*nx + vz*nz;
      var N = Math.min(110000, 160000*Math.min(pen, 0.5) - 12000*Math.max(Math.min(vn, 0), -8));
      if(N <= 0) continue;
      if(vn < -3){ c.hit = Math.max(c.hit, Math.min(1, -vn/12)); c.hitT = 0.3; }
      var tx = vx - nx*vn, tz = vz - nz*vn, tl = Math.sqrt(tx*tx + tz*tz), fr = tl > 1e-3 ? 0.4*N*Math.min(1, tl/0.5)/tl : 0;
      addForce(F, T, [rx, ry, rz], nx*N - tx*fr, 0, nz*N - tz*fr);
    }
  });
}

/* ---------- fixed-step driver with render interpolation ---------- */
// Runs as many 120 Hz steps as frameDt covers; c.alpha is how far render time sits
// between the last two steps (use pose(c) for an interpolated position/quaternion).
function advance(c, ctl, env, frameDt, maxSteps){
  c.acc += Math.min(frameDt, 0.25);
  var n = 0, cap = maxSteps || 12;
  while(c.acc >= DT && n < cap){
    c.prevP[0] = c.p[0]; c.prevP[1] = c.p[1]; c.prevP[2] = c.p[2];
    c.prevQ[0] = c.q[0]; c.prevQ[1] = c.q[1]; c.prevQ[2] = c.q[2]; c.prevQ[3] = c.q[3];
    step(c, ctl, env, DT); c.acc -= DT; n++;
  }
  if(n >= cap) c.acc = 0;
  c.alpha = clamp(c.acc/DT, 0, 1);
  return n;
}
// interpolated pose: {p:[x,y,z], q:[x,y,z,w]}
function pose(c, out){
  out = out || {p: [0, 0, 0], q: [0, 0, 0, 1]};
  var a = c.alpha == null ? 1 : c.alpha, A = c.prevQ, B = c.q;
  for(var i = 0; i < 3; i++) out.p[i] = lerp(c.prevP[i], c.p[i], a);
  var d = A[0]*B[0] + A[1]*B[1] + A[2]*B[2] + A[3]*B[3], sg = d < 0 ? -1 : 1;
  for(var j = 0; j < 4; j++) out.q[j] = lerp(A[j], B[j]*sg, a);
  qNorm(out.q);
  return out;
}

// A snapshot of the full state (for tests and determinism checks).
function snapshot(c){
  return JSON.stringify([c.p, c.v, c.q, c.w, c.gear, c.rpm, c.wheels.map(function(w){ return [w.omega, w.kappa, w.tanA, w.comp]; })]);
}

var API = {G: G, DT: DT, SUB: SUB, SURFACES: SURFACES, SURF_INDEX: SURF_INDEX, CARS: CARS,
  create: create, place: place, step: step, advance: advance, pose: pose, snapshot: snapshot,
  headingOf: headingOf, qYaw: qYaw, qToM: qToM, curveAt: curveAt, gearRatio: gearRatio, tyreShape: tyreShape,
  rayTerrain: rayTerrain, clamp: clamp};
if(typeof module === "object" && module.exports) module.exports = API;
else { root.HQV = root.HQV || {}; root.HQV.rallyPhysics = API; }
})(typeof window !== "undefined" ? window : this);
