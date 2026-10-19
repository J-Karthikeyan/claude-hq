#!/usr/bin/env node
/* Valley Rally: drive every preset stage with every car, the way a player would.
 *   node games/rally/tools/drive.js           keys: digital steering refreshed each step, ramped
 *                                             throttle and brake, assist + traction control on
 *   node games/rally/tools/drive.js analog    a stick: proportional steering, assist on, no TC
 * A careful driver: corner speeds from the grip of the surface under the car, back to the last
 * gate (R) when stuck for 3 s or upside down. Prints the finish time (or the gate reached) and
 * how many resets it took, per stage and car.
 */
"use strict";
var path = require("path");
var P = require(path.join(__dirname, "..", "physics.js")), S = require(path.join(__dirname, "..", "stage.js"));
var kb = process.argv[2] !== "analog", vcap = 24, gl = 0.45;
S.PRESETS.forEach(function(p){
  var line = [];
  P.CARS.forEach(function(spec, ci){
    var st = S.build(p), env = st.env, DT = P.DT;
    var c = P.place(P.create(ci, {assist: true, tc: kb}), st.start.x, st.start.z, st.start.h*Math.PI/180, env);
    var next = 0, hint = 0, t = 0, px = c.p[0], pz = c.p[2], resets = 0, stuck = 0, thr = 0, brk = 0;
    while(next < st.gates.length && t < 500){
      var L = st.locate(c.p[0], c.p[2], hint); hint = L.i;
      var A = st.pointAtS(L.s + Math.max(8, c.tel.speed*0.9)), kmax = 0;
      for(var q = 0; q < Math.max(60, c.tel.speed*2.5); q += 6) kmax = Math.max(kmax, Math.abs(st.pointAtS(L.s + q).k));
      var mu = (P.SURFACES[c.tel.surface] || {mu: 0.8}).mu;
      var vT = Math.min(vcap, Math.sqrt(gl*9.81*mu/Math.max(kmax, 1e-3)));
      var fx = -c.M[2], fz = -c.M[8], dx = A.x - c.p[0], dz = A.z - c.p[2], ang = Math.atan2(fx*dz - fz*dx, fx*dx + fz*dz), e = vT - c.tel.speed;
      var ctl;
      if(kb){
        var w = e > 0 ? 1 : 0, b = e < -2 ? 1 : 0, sx = ang*2.5;
        thr += (w - thr)*Math.min(1, DT*(w > thr ? 9 : 14)); brk += (b - brk)*Math.min(1, DT*(b > brk ? 5 : 14));
        ctl = {throttle: thr, brake: brk, steer: sx > 0.15 ? 1 : sx < -0.15 ? -1 : 0};
      } else ctl = {throttle: Math.max(0, Math.min(1, 0.3 + e*0.5)), brake: e < -2 ? Math.min(1, -e*0.15) : 0, steer: Math.max(-1, Math.min(1, ang*2.2)), analog: true};
      P.step(c, ctl, env, DT); t += DT;
      if(S.crossesGate(st.gates[next], px, pz, c.p[0], c.p[2])) next++;
      px = c.p[0]; pz = c.p[2];
      stuck = c.tel.speed < 2 ? stuck + DT : 0;
      if(stuck > 3 || c.upsideT > 3){
        stuck = 0; resets++;
        var g = next ? st.gates[next - 1] : {x: st.start.x, z: st.start.z, h: st.start.h};
        P.place(c, g.x, g.z, g.h*Math.PI/180, env);
      }
    }
    line.push(spec.name.split(" ")[0] + " " + (next === st.gates.length ? t.toFixed(0) + "s" : "gate " + next) + (resets ? " (" + resets + " resets)" : ""));
  });
  console.log((p.name + "                    ").slice(0, 20), line.join("  |  "));
});
