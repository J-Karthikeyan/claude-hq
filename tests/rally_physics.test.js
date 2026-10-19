#!/usr/bin/env node
/* Valley Rally physics tests: node tests/rally_physics.test.js (no dependencies).
 * Also exports the scenarios for games/rally/tools/tune.js. tests/test_rally.py runs
 * this file under python's unittest when node is installed. */
"use strict";
var path = require("path");
var P = require(path.join(__dirname, "..", "games", "rally", "physics.js"));
var StageMod = null;
function Stage_(){ return StageMod || (StageMod = require(path.join(__dirname, "..", "games", "rally", "stage.js"))); }

var DT = P.DT;
function surfIndex(name){ return P.SURF_INDEX[name] != null ? P.SURF_INDEX[name] : 0; }
function flat(surf){ var si = surfIndex(surf); return {height: function(){ return 0; }, surface: function(){ return si; }}; }
// a plane rising toward -z (north) at `deg` degrees
function slope(deg, surf){
  var k = Math.tan(deg*Math.PI/180), si = surfIndex(surf || "gravel");
  var n = [0, 1/Math.sqrt(1 + k*k), -k/Math.sqrt(1 + k*k)];
  return {height: function(x, z){ return -z*k; }, normal: function(x, z, o){ o[0] = n[0]; o[1] = n[1]; o[2] = n[2]; return o; },
          surface: function(){ return si; }};
}
function hills(){
  var si = surfIndex("gravel");
  return {height: function(x, z){ return 3*Math.sin(x*0.02)*Math.cos(z*0.017) + 0.6*Math.sin(x*0.11 + z*0.07) + 0.15*Math.sin(x*0.9)*Math.sin(z*0.8); },
          surface: function(x, z){ var u = Math.sin(x*0.013) + Math.cos(z*0.011); return u > 1.2 ? surfIndex("mud") : u > 0.6 ? surfIndex("grass") : u < -1.3 ? surfIndex("tarmac") : si; }};
}
function mulberry32(a){ return function(){ a = (a + 0x6D2B79F5) >>> 0; var t = a; t = Math.imul(t ^ (t >>> 15), t | 1); t ^= t + Math.imul(t ^ (t >>> 7), t | 61); return ((t ^ (t >>> 14)) >>> 0)/4294967296; }; }
function yawOf(c){ return P.headingOf(c.M); }
function angDiff(a, b){ return Math.atan2(Math.sin(a - b), Math.cos(a - b)); }
function finite(c){ return c.p.concat(c.v, c.q, c.w).every(function(x){ return isFinite(x); }) && isFinite(c.rpm); }

// drive straight at a target speed (simple throttle/brake controller)
function cruise(c, env, target, secs, steer){
  for(var t = 0; t < secs; t += DT){
    var e = target - c.tel.speed;
    P.step(c, {throttle: Math.max(0, Math.min(1, e*0.6 + 0.25)), brake: e < -2 ? 0.3 : 0, steer: steer || 0}, env, DT);
  }
}

var scenarios = {
  accel: function(spec, surf){
    var env = flat(surf), c = P.place(P.create(spec), 0, 0, 0, env), t = 0, maxSlip = 0;
    while(t < 25){
      P.step(c, {throttle: 1}, env, DT); t += DT;
      maxSlip = Math.max(maxSlip, c.tel.slip);
      if(c.tel.speed >= 100/3.6) return {t0100: t, maxSlip: maxSlip, car: c};
    }
    return {t0100: null, maxSlip: maxSlip, car: c};
  },
  brake: function(spec, surf){
    var env = flat(surf), c = P.place(P.create(spec), 0, 0, 0, env);
    while(c.tel.speed < 100/3.6 + 0.3 && c.t < 40) P.step(c, {throttle: 1}, env, DT);
    var z0 = c.p[2], x0 = c.p[0], t = 0;
    while(c.tel.speed > 0.3 && t < 20){ P.step(c, {brake: 1}, env, DT); t += DT; }
    return {dist: Math.sqrt((c.p[2] - z0)*(c.p[2] - z0) + (c.p[0] - x0)*(c.p[0] - x0)), t: t};
  },
  topSpeed: function(spec, surf){
    var env = flat(surf), c = P.place(P.create(spec), 0, 0, 0, env), top = 0;
    for(var t = 0; t < 70; t += DT){ P.step(c, {throttle: 1}, env, DT); top = Math.max(top, c.tel.kmh); }
    return top;
  },
  // steady-state lateral acceleration: hold ~14 m/s and wind on steering
  skidpad: function(spec, surf){
    var env = flat(surf), c = P.place(P.create(spec, {assist: false}), 0, 0, 0, env), best = 0;
    cruise(c, env, 14, 6);
    var pv = c.v.slice(), lat = 0;
    for(var t = 0; t < 8; t += DT){
      var st = Math.min(1, t/6), e = 14 - c.tel.speed;
      P.step(c, {throttle: Math.max(0, Math.min(1, e*0.8 + 0.3)), steer: st}, env, DT);
      // lateral acceleration from the change of velocity, across the car (smoothed over ~0.25 s)
      var ax = (c.v[0] - pv[0])/DT, az = (c.v[2] - pv[2])/DT; pv = [c.v[0], c.v[1], c.v[2]];
      var latNow = Math.abs(ax*c.M[0] + az*c.M[6])/P.G; lat += (latNow - lat)*0.035;
      if(t > 1.5 && Math.abs(c.tel.beta) < 0.3) best = Math.max(best, lat);
    }
    return best;
  },
  // at ~17 m/s: a little steer, then the handbrake for 0.7 s; yaw change after 1.6 s
  handbrake: function(spec, surf, pull){
    var env = flat(surf), c = P.place(P.create(spec, {assist: false}), 0, 0, 0, env);
    cruise(c, env, 17, 7);
    var y0 = yawOf(c), turned = 0, prev = y0;
    for(var t = 0; t < 1.6; t += DT){
      P.step(c, {throttle: 0, steer: 0.35, handbrake: pull && t < 0.7 ? 1 : 0}, env, DT);
      var y = yawOf(c); turned += angDiff(y, prev); prev = y;
    }
    return {yaw: Math.abs(turned)*180/Math.PI, speed: c.tel.speed};
  },
  // Scandinavian-flick / handbrake entry, then hold a right-hand power drift with
  // throttle and countersteer. Returns the longest spell of a held drift (seconds).
  drift: function(spec, surf, opt){
    opt = opt || {};
    var env = flat(surf), c = P.place(P.create(spec, {assist: opt.assist !== false}), 0, 0, 0, env);
    cruise(c, env, 18, 7);
    var t, held = 0, run = 0, target = 0.42, maxB = 0;
    // flick: a dab left, then hard right with the handbrake
    for(t = 0; t < 0.35; t += DT) P.step(c, {throttle: 0.3, steer: -0.6}, env, DT);
    for(t = 0; t < 0.4; t += DT) P.step(c, {throttle: 0.2, steer: 1, handbrake: t < 0.25 ? 1 : 0}, env, DT);
    var log = [], bPrev = -c.tel.beta, I = 0;
    for(t = 0; t < 6; t += DT){
      // a driver holding the angle: steering and throttle on the drift angle and how fast it changes
      var b = -c.tel.beta;                              // right-hand drift: beta < 0
      var err = target - b, db = (b - bPrev)/DT; bPrev = b;
      var steer, thr;
      I = Math.max(-0.3, Math.min(0.3, I + 0.8*err*DT));
      if(c.assist) steer = Math.max(-1, Math.min(1, 0.45 + 2.2*err - 0.35*db));
      else {
        // a driver's own countersteer: aim the front wheels along the front axle's travel,
        // plus a little slip into the turn that grows when the angle is too small
        var M = c.M, a = spec.a, w = c.w, v = c.v;
        var fx = -M[2], fy = -M[5], fz = -M[8], rx = M[0], ry = M[3], rz = M[6];
        var vx = v[0] + (w[1]*fz - w[2]*fy)*a, vy = v[1] + (w[2]*fx - w[0]*fz)*a, vz = v[2] + (w[0]*fy - w[1]*fx)*a;
        var thetaF = Math.atan2(vx*rx + vy*ry + vz*rz, Math.max(1, vx*fx + vy*fy + vz*fz));
        // and against the rotation itself: a yaw rate above a steady circle's (v/25 m) means less aim
        var rOver = Math.abs(c.w[1]) - c.tel.speed/25;
        var aim = Math.max(-0.2, Math.min(0.2, 0.06 + 0.7*err - 0.12*db - 0.25*rOver));
        steer = Math.max(-1, Math.min(1, (thetaF + aim)/(c.lockNow || 0.42)));
      }
      thr = Math.max(0.15, Math.min(1, 0.7 + 2.0*err - 0.3*db + I - (c.assist ? 0 : 0.6*Math.max(0, Math.abs(c.w[1]) - c.tel.speed/25))));
      P.step(c, {throttle: thr, steer: steer, analog: !c.assist}, env, DT);
      maxB = Math.max(maxB, Math.abs(c.tel.beta));
      var ok = Math.abs(c.tel.beta) > 0.2 && Math.abs(c.tel.beta) < 1.2 && c.tel.speed > 6;
      run = ok ? run + DT : 0; held = Math.max(held, run);
      if(opt.log && (Math.round(t/DT) % 12) === 0) log.push([t.toFixed(2), (c.tel.beta*57.3).toFixed(1), c.tel.speed.toFixed(1), steer.toFixed(2), thr.toFixed(2), c.tel.gear, "k", c.wheels.map(function(w){ return w.kappa.toFixed(2); }).join(","), "tA", c.wheels.map(function(w){ return (Math.atan(w.tanA)*57.3).toFixed(0); }).join(","), "Fy", c.wheels.map(function(w){ return w.Fy|0; }).join(","), "Fz", c.wheels.map(function(w){ return w.Fz|0; }).join(","), "d", (c.delta*57.3).toFixed(0), "r", c.w[1].toFixed(2), "vxy", c.wheels.map(function(w){ return w.vx.toFixed(1)+"/"+w.vy.toFixed(1); }).join(",")].join(" "));
    }
    return {held: held, maxBeta: maxB*180/Math.PI, speed: c.tel.speed, log: log};
  }
};

/* ---------------- the tests ---------------- */
var results = [], failed = 0;
function test(name, fn){
  var t0 = Date.now();
  try { fn(); results.push("ok    " + name + " (" + (Date.now() - t0) + " ms)"); }
  catch(e){ failed++; results.push("FAIL  " + name + ": " + (e && e.message || e)); }
}
function ok(cond, msg){ if(!cond) throw new Error(msg || "assertion failed"); }

function run(){
  test("determinism: the same inputs give the same state", function(){
    function go(){
      var env = hills(), c = P.place(P.create(0), 5, 5, 0.4, env), r = mulberry32(42);
      var ctl = {throttle: 0, brake: 0, steer: 0, handbrake: 0};
      for(var i = 0; i < 6000; i++){
        if(i % 30 === 0){ ctl = {throttle: r(), brake: r() < 0.2 ? r() : 0, steer: r()*2 - 1, handbrake: r() < 0.1 ? 1 : 0}; }
        P.step(c, ctl, env, DT);
      }
      return P.snapshot(c);
    }
    var a = go(), b = go();
    ok(a === b, "two runs differ");
  });

  P.CARS.forEach(function(spec, ci){
    test("0-100 km/h on tarmac in a plausible band: " + spec.name, function(){
      var a = scenarios.accel(spec, "tarmac");
      ok(a.t0100 != null, "never reached 100 km/h");
      ok(a.t0100 > 3.2 && a.t0100 < 9.5, "0-100 took " + a.t0100.toFixed(2) + " s");
    });
    test("0-100 km/h is slower on gravel than tarmac: " + spec.name, function(){
      var t = scenarios.accel(spec, "tarmac").t0100, g = scenarios.accel(spec, "gravel").t0100;
      ok(g != null && g > t + 0.2, "tarmac " + t + " gravel " + g);
      ok(g < 14, "gravel 0-100 took " + g);
    });
    test("100-0 km/h braking distance in a plausible band: " + spec.name, function(){
      var b = scenarios.brake(spec, "tarmac");
      ok(b.dist > 30 && b.dist < 62, "tarmac stop in " + b.dist.toFixed(1) + " m");
      var g = scenarios.brake(spec, "gravel");
      ok(g.dist > b.dist, "gravel stop " + g.dist.toFixed(1) + " m not longer than tarmac " + b.dist.toFixed(1));
    });
    test("handbrake induces yaw: " + spec.name, function(){
      var without = scenarios.handbrake(spec, "gravel", false), with_ = scenarios.handbrake(spec, "gravel", true);
      ok(with_.yaw > without.yaw + 20, "yaw with " + with_.yaw.toFixed(1) + " without " + without.yaw.toFixed(1));
    });
  });

  test("a steady gravel drift holds with countersteer for >= 2 s (assist on: AWD and RWD)", function(){
    [P.CARS[0], P.CARS[1]].forEach(function(spec){
      var d = scenarios.drift(spec, "gravel", {assist: true});
      ok(d.held >= 2, spec.name + ": held " + d.held.toFixed(2) + " s (max beta " + d.maxBeta.toFixed(0) + ")");
    });
  });
  test("a gravel drift holds with the driver's own countersteer (no assist: RWD)", function(){
    [P.CARS[1]].forEach(function(spec){
      var d = scenarios.drift(spec, "gravel", {assist: false});
      ok(d.held >= 2, spec.name + ": held " + d.held.toFixed(2) + " s (max beta " + d.maxBeta.toFixed(0) + ")");
    });
  });

  test("no NaN or explosion over 10 minutes of random inputs on hilly terrain", function(){
    P.CARS.forEach(function(spec, ci){
      var env = hills(), c = P.place(P.create(spec), 0, 0, 0, env), r = mulberry32(1000 + ci);
      var ctl = {}, maxV = 0;
      for(var i = 0; i < 600*120; i++){
        if(i % 45 === 0) ctl = {throttle: r() < 0.7 ? r() : 0, brake: r() < 0.25 ? r() : 0, steer: r()*2 - 1, handbrake: r() < 0.08 ? 1 : 0, analog: r() < 0.5};
        P.step(c, ctl, env, DT);
        if(!finite(c)) throw new Error(spec.name + ": non-finite state at step " + i);
        maxV = Math.max(maxV, c.tel.speed);
        if(c.upsideT > 3) P.place(c, c.p[0], c.p[2], P.headingOf(c.M), env);
        // keep it on the test patch
        if(Math.abs(c.p[0]) > 2000 || Math.abs(c.p[2]) > 2000) P.place(c, 0, 0, 0, env);
      }
      ok(maxV < 80, spec.name + ": reached " + maxV.toFixed(1) + " m/s");
      ok(Math.abs(c.p[1] - env.height(c.p[0], c.p[2])) < 6, spec.name + ": ended far from the ground");
    });
  });

  test("a car at rest stays at rest on a 10 degree gravel slope (and on tarmac)", function(){
    ["gravel", "tarmac"].forEach(function(sf){
      [0, Math.PI/2, Math.PI].forEach(function(h){
        var env = slope(10, sf), c = P.place(P.create(0), 0, 0, h, env), x0 = c.p[0], z0 = c.p[2];
        for(var i = 0; i < 120*10; i++) P.step(c, {}, env, DT);
        var d = Math.sqrt((c.p[0] - x0)*(c.p[0] - x0) + (c.p[2] - z0)*(c.p[2] - z0));
        ok(d < 0.25, sf + " heading " + h.toFixed(2) + ": crept " + d.toFixed(3) + " m");
        ok(c.tel.speed < 0.05, sf + ": still moving at " + c.tel.speed.toFixed(3));
      });
    });
  });

  test("airtime: over a sharp crest at speed the wheels leave the ground, then land", function(){
    var si = surfIndex("gravel");
    var env = {height: function(x, z){ var u = -z - 120; return u < 0 ? 0 : u < 12 ? u*0.18 : Math.max(0, 2.16 - (u - 12)*0.35); }, surface: function(){ return si; }};
    var c = P.place(P.create(0), 0, 0, 0, env), air = 0, landed = false;
    for(var t = 0; t < 12; t += DT){
      P.step(c, {throttle: 1}, env, DT);
      if(c.tel.air) air += DT; else if(air > 0.2) landed = true;
    }
    ok(air > 0.2, "no airtime (" + air.toFixed(2) + " s)");
    ok(landed && c.M[4] > 0.8, "did not land upright");
  });

  test("the automatic gearbox shifts up through the gears and shows the gear", function(){
    var env = flat("tarmac"), c = P.place(P.create(0), 0, 0, 0, env), seen = {};
    for(var t = 0; t < 30; t += DT){ P.step(c, {throttle: 1}, env, DT); seen[c.gear] = 1; }
    ok(seen[1] && seen[2] && seen[3] && seen[4] && seen[5], "gears seen: " + Object.keys(seen).join(","));
    ok(c.rpm < c.spec.engine.limiter + 400, "rpm " + c.rpm);
  });

  test("reverse: holding brake at a stop engages reverse and backs up", function(){
    var env = flat("gravel"), c = P.place(P.create(1), 0, 0, 0, env);
    for(var t = 0; t < 3; t += DT) P.step(c, {brake: 1}, env, DT);
    ok(c.gear === -1, "gear " + c.gear);
    ok(c.tel.vFwd < -1, "not reversing: " + c.tel.vFwd);
  });

  test("stage: the same seed builds the same stage, and its MapDoc is valid", function(){
    var a = Stage_().build({seed: "test-1", biome: "forest", night: false, length: 3000});
    var b = Stage_().build({seed: "test-1", biome: "forest", night: false, length: 3000});
    ok(JSON.stringify(a.doc) === JSON.stringify(b.doc), "docs differ");
    ok(a.doc.data.gates.length === 20, "gates " + a.doc.data.gates.length);
    ok(a.doc.data.road.length <= 400 && a.doc.data.road.length > 100, "road points " + a.doc.data.road.length);
    ok(a.length > 2400 && a.length < 4200, "length " + a.length);
    var cl = Stage_().cleanDoc(JSON.parse(JSON.stringify(a.doc)));
    ok(cl && JSON.stringify(cl) === JSON.stringify(a.doc), "cleanDoc changed a valid doc");
    ok(!Stage_().cleanDoc({kind: "kart"}), "cleanDoc accepted a kart doc");
  });

  test("stage: a car can sit on the carved road at the start and every gate", function(){
    ["forest", "hills", "snow"].forEach(function(bi){
      var st = Stage_().build({seed: "gates-" + bi, biome: bi, night: false, length: 3200});
      var env = st.env;
      [st.start].concat(st.gates).forEach(function(g, gi){
        var c = P.place(P.create(0), g.x, g.z, g.h*Math.PI/180, env);
        for(var i = 0; i < 240; i++) P.step(c, {}, env, DT);
        ok(c.M[4] > 0.9 && c.tel.speed < 0.2, bi + " gate " + gi + ": not resting upright (up " + c.M[4].toFixed(2) + ", v " + c.tel.speed.toFixed(2) + ")");
      });
    });
  });

  test("stage: gate crossing detection follows the gate line within its half-width", function(){
    var g = {x: 10, z: 0, h: 90, w: 8};                 // heading east: the gate line runs north-south
    ok(Stage_().crossesGate(g, 5, 2, 15, 1), "missed a straight crossing");
    ok(!Stage_().crossesGate(g, 5, 12, 15, 12), "counted a crossing outside the half-width");
    ok(!Stage_().crossesGate(g, 15, 1, 5, 2), "counted a backwards crossing");
    ok(!Stage_().crossesGate(g, 2, 0, 8, 0), "counted a move that stops short");
  });

  test("stage: the MapDoc fits the Arena's limits (rounding, spacing, size, gate 0 ahead of the grid)", function(){
    ["forest", "hills", "snow"].forEach(function(bi){
      [2500, 4000].forEach(function(len){
        var st = Stage_().build({seed: "limits-" + bi + len, biome: bi, night: bi === "snow", length: len}), d = st.doc.data;
        ok(JSON.stringify(d).length <= 12*1024, bi + " " + len + ": data is " + JSON.stringify(d).length + " bytes");
        ok(Number.isInteger(d.length), "length not an integer");
        for(var i = 1; i < d.road.length; i++){
          var dx = d.road[i][0] - d.road[i-1][0], dz = d.road[i][1] - d.road[i-1][1], sp = Math.sqrt(dx*dx + dz*dz);
          ok(sp >= 2 && sp <= 30, bi + ": road spacing " + sp.toFixed(2) + " at " + i);
          d.road[i].forEach(function(v){ ok(Math.round(v*10) === v*10, "unrounded " + v); });
        }
        d.gates.forEach(function(g){ ok(Number.isInteger(g.h) && Number.isInteger(g.w) && Math.round(g.x*10) === g.x*10, "gate not rounded"); });
        var g0 = d.gates[0], dr = Math.sqrt(Math.pow(g0.x - d.road[0][0], 2) + Math.pow(g0.z - d.road[0][1], 2));
        ok(dr > 40, "gate 0 only " + dr.toFixed(1) + " m from the grid");
        // a client building from the doc gets the same gates and a matching centre line
        var re = Stage_().build({seed: d.seed, biome: d.biome, night: d.night, length: d.length}, st.doc);
        ok(JSON.stringify(re.doc.data) === JSON.stringify(d), "rebuilding from the doc changed it");
      });
    });
  });

  test("stage: a simple driver follows the road and crosses every gate in order (each biome)", function(){
    ["forest", "hills", "snow"].forEach(function(bi){
      var st = Stage_().build({seed: "drive-" + bi, biome: bi, night: false, length: 2600}), env = st.env;
      var c = P.place(P.create(0), st.start.x, st.start.z, st.start.h*Math.PI/180, env), next = 0, hint = 0, t = 0;
      var px = c.p[0], pz = c.p[2];
      while(next < st.gates.length && t < 400){
        var L = st.locate(c.p[0], c.p[2], hint); hint = L.i;
        // pure pursuit on a point ahead; speed from the curvature ahead
        var look = Math.max(8, c.tel.speed*0.9), A = st.pointAtS(L.s + look);
        var kmax = 0; for(var q = 0; q < 60; q += 6) kmax = Math.max(kmax, Math.abs(st.pointAtS(L.s + q).k));
        var vT = Math.min(24, Math.sqrt(0.45*9.81*0.7/Math.max(kmax, 1e-3)));
        var fx = -c.M[2], fz = -c.M[8], dx = A.x - c.p[0], dz = A.z - c.p[2];
        var ang = Math.atan2(fx*dz - fz*dx, fx*dx + fz*dz);
        var e = vT - c.tel.speed;
        P.step(c, {throttle: Math.max(0, Math.min(1, 0.3 + e*0.5)), brake: e < -2 ? Math.min(1, -e*0.15) : 0, steer: Math.max(-1, Math.min(1, ang*2.2)), analog: true}, env, DT);
        t += DT;
        if(Stage_().crossesGate(st.gates[next], px, pz, c.p[0], c.p[2])) next++;
        px = c.p[0]; pz = c.p[2];
        if(c.upsideT > 3) throw new Error(bi + ": rolled over at s=" + L.s.toFixed(0));
      }
      ok(next === st.gates.length, bi + ": crossed " + next + " of " + st.gates.length + " gates in " + t.toFixed(0) + " s");
    });
  });

  results.forEach(function(r){ console.log(r); });
  console.log(failed ? failed + " failed" : "all " + results.length + " passed");
  if(failed) process.exitCode = 1;
}

module.exports = {scenarios: scenarios, flat: flat, slope: slope, hills: hills};
if(require.main === module) run();
