#!/usr/bin/env node
/* Valley Rally: handling metrics per car and surface, for tuning games/rally/physics.js.
 *   node games/rally/tools/tune.js            all cars, all surfaces
 *   node games/rally/tools/tune.js 0 gravel   one car, one surface
 * Prints 0-100 km/h, 100-0 braking distance, top speed, a skidpad (steady-state
 * lateral g), the handbrake-turn yaw and how long a power drift holds.
 * Uses the same scenarios as tests/rally_physics.test.js (exported from there).
 */
"use strict";
var path = require("path");
var P = require(path.join(__dirname, "..", "physics.js"));
var S = require(path.join(__dirname, "..", "..", "..", "tests", "rally_physics.test.js")).scenarios;

var carArg = process.argv[2], surfArg = process.argv[3];
var cars = carArg != null && carArg !== "all" ? [P.CARS[+carArg]] : P.CARS;
var surfs = surfArg ? [surfArg] : ["tarmac", "gravel", "dirt", "snow", "grass", "mud"];
function f(v, d){ return v == null ? "  –  " : (typeof v === "number" ? v.toFixed(d == null ? 2 : d) : String(v)); }
cars.forEach(function(spec){
  console.log("\n" + spec.name + " · " + spec.kind + " · " + spec.mass + " kg");
  console.log("surface   0-100 s  100-0 m   top km/h  skidpad g  hb yaw deg  drift s  launch slip");
  surfs.forEach(function(sf){
    var a = S.accel(spec, sf), b = S.brake(spec, sf), top = S.topSpeed(spec, sf), sk = S.skidpad(spec, sf),
        hb = S.handbrake(spec, sf, true), dr = S.drift(spec, sf, {assist: true});
    console.log((sf + "        ").slice(0, 9) + " " + f(a.t0100) + "    " + f(b.dist, 1) + "    " + f(top, 0) + "      " + f(sk, 2) +
      "       " + f(hb.yaw, 0) + "        " + f(dr.held, 2) + "    " + f(a.maxSlip, 2));
  });
});
