/* Valley Rally: stages. Pure code (no DOM, no three.js): node runs it in the tests and the
 * browser loads it as window.HQV.rallyStage. Everything comes from
 * mulberry32(hashStr(seed)) (the same helpers as games/core.js), so every client builds
 * the same stage from {seed, biome, night, length}.
 *
 *   build({seed, biome, night, length}) -> stage   (or build(opts, doc) to follow a MapDoc's road)
 *     stage.doc       the MapDoc the Arena referees: {kind:"rally", v:1, name, data:{seed, biome,
 *                     night, length, road:[[x,z,y]...], gates:[{x,z,h,w}...]}} (rounded: 0.1 m
 *                     coordinates, whole-degree headings, whole-metre widths and length)
 *     stage.env       {height(x,z), normal(x,z,out), surface(x,z), obstacles(x,z,r,cb)} for physics.js
 *     stage.pts       the road centre line every ~2 m: {x, z, y, h (heading rad), s, k (curvature), bank}
 *     stage.gates     the doc's gates; stage.start {x, z, h}; stage.length (finish, m)
 *     stage.scenery   trees, rocks, tufts, fences, chevrons, banners, arches, tower (for scene.js)
 *     stage.locate(x, z, hint) -> {i, s, lat, d}   nearest centre-line point
 *   crossesGate(g, x0, z0, x1, z1) -> false | u (0..1 along the move)   forward crossings only
 *   cleanDoc(doc) -> a valid rally MapDoc (rounded) or null
 *   daily(date) / PRESETS / BIOMES
 *
 * Headings: compass degrees, 0 = north (-z), 90 = east (+x), like the Arena referee.
 */
(function(root){
"use strict";

/* ---------- deterministic randomness: the same as games/core.js ---------- */
function hashStr(s){ var h = 2166136261 >>> 0; s = String(s); for(var i = 0; i < s.length; i++){ h ^= s.charCodeAt(i); h = Math.imul(h, 16777619) >>> 0; } return h >>> 0; }
function mulberry32(seed){ var a = (typeof seed === "number" ? seed : hashStr(seed)) >>> 0; return function(){ a = (a + 0x6D2B79F5) >>> 0; var t = a; t = Math.imul(t ^ (t >>> 15), t | 1); t ^= t + Math.imul(t ^ (t >>> 7), t | 61); return ((t ^ (t >>> 14)) >>> 0)/4294967296; }; }

var PI = Math.PI, TAU = PI*2, DEG = PI/180;
function clamp(v, a, b){ return v < a ? a : v > b ? b : v; }
function lerp(a, b, u){ return a + (b - a)*u; }
function smooth(u){ u = clamp(u, 0, 1); return u*u*(3 - 2*u); }
function r1(v){ return Math.round(v*10)/10; }
function wrapAng(a){ return Math.atan2(Math.sin(a), Math.cos(a)); }

var BIOMES = ["forest", "hills", "snow"];
var BIOME_NAMES = {forest: "Forest", hills: "Dry hills", snow: "Snow"};
var GATES = 20, ROAD_HALF = 3.7, SHOULDER = 1.8, DITCH = 2.0, CELL = 2, RUNOUT = 70;
// surface ids (the order of physics.js SURFACES)
var S_TARMAC = 0, S_GRAVEL = 1, S_DIRT = 2, S_GRASS = 3, S_MUD = 4, S_SNOW = 5, S_LOOSE = 6;

// Named stages (original names), the daily stage, and a three-stage event.
var PRESETS = [
  {id: "pinewood", name: "Pinewood Run",       seed: "pinewood-run",    biome: "forest", night: false, length: 3000},
  {id: "ochre",    name: "Ochre Ridge",        seed: "ochre-ridge",     biome: "hills",  night: false, length: 3400},
  {id: "frost",    name: "Frostbite Pass",     seed: "frostbite-pass",  biome: "snow",   night: false, length: 2900},
  {id: "lantern",  name: "Lantern Woods",      seed: "lantern-woods",   biome: "forest", night: true,  length: 3200},
  {id: "dusk",     name: "Dusk Plateau",       seed: "dusk-plateau",    biome: "hills",  night: true,  length: 3600},
  {id: "aurora",   name: "Aurora Switchbacks", seed: "aurora-switch",   biome: "snow",   night: true,  length: 3100}
];
var WORDS_A = ["Bracken", "Cinder", "Hollow", "Larch", "Mossy", "Quarry", "Rowan", "Saddle", "Thistle", "Willow", "Copper", "Juniper", "Flint", "Heather", "Kestrel", "Marrow"];
var WORDS_B = ["Run", "Climb", "Descent", "Loop", "Pass", "Traverse", "Gully", "Ridge", "Hollow", "Sprint", "Crossing", "Bends"];
function nameFor(seed){
  var r = mulberry32(hashStr("name|" + seed));
  var a = WORDS_A[Math.floor(r()*WORDS_A.length)], b = WORDS_B[Math.floor(r()*WORDS_B.length)];
  return a + " " + b;
}
function utcDay(d){ d = d || new Date(); return d.getUTCFullYear() + "-" + String(d.getUTCMonth() + 1).padStart(2, "0") + "-" + String(d.getUTCDate()).padStart(2, "0"); }
// The daily stage: seed "daily-" + UTC date, the biome rotating day by day.
function daily(d){
  d = d || new Date();
  var day = utcDay(d), n = Math.floor(Date.UTC(d.getUTCFullYear(), d.getUTCMonth(), d.getUTCDate())/86400000);
  var seed = "daily-" + day;
  return {id: "daily", name: "Daily stage " + day, seed: seed, biome: BIOMES[((n % 3) + 3) % 3], night: n % 4 === 3, length: 2700 + (hashStr(seed) % 1000), day: day};
}
// A rally event: three stages from one seed, the biome turning with each.
function event(seed, count){
  count = clamp(count|0 || 3, 1, 5);
  var r = mulberry32(hashStr("event|" + seed)), b0 = Math.floor(r()*3), out = [];
  for(var i = 0; i < count; i++){
    var s = String(seed) + "-" + (i + 1);
    out.push({id: "event-" + i, name: nameFor(s), seed: s, biome: BIOMES[(b0 + i) % 3], night: false, length: 2600 + Math.floor(r()*900)});
  }
  return out;
}

/* ---------- value noise ---------- */
function makeNoise(seed){
  var r = mulberry32(seed), perm = new Uint16Array(512), val = new Float32Array(256), i;
  for(i = 0; i < 256; i++){ perm[i] = i; val[i] = r()*2 - 1; }
  for(i = 255; i > 0; i--){ var j = Math.floor(r()*(i + 1)), t = perm[i]; perm[i] = perm[j]; perm[j] = t; }
  for(i = 0; i < 256; i++) perm[i + 256] = perm[i];
  function v(ix, iz){ return val[perm[(ix & 255) + perm[iz & 255]]]; }
  return function(x, z){
    var ix = Math.floor(x), iz = Math.floor(z), fx = x - ix, fz = z - iz;
    var u = fx*fx*(3 - 2*fx), w = fz*fz*(3 - 2*fz);
    var a = v(ix, iz), b = v(ix + 1, iz), c = v(ix, iz + 1), d = v(ix + 1, iz + 1);
    return a + (b - a)*u + (c - a)*w + (a - b - c + d)*u*w;
  };
}
// biome terrain: [amplitude m, wavelength m] octaves
var TERRAIN = {forest: [[20, 420], [7, 140], [1.4, 34], [0.25, 9]],
               hills:  [[34, 560], [9, 170], [1.8, 44], [0.3, 11]],
               snow:   [[28, 460], [8, 150], [1.2, 36], [0.2, 10]]};
function makeTerrain(seed, biome){
  var n = makeNoise(seed), oct = TERRAIN[biome] || TERRAIN.forest, r = mulberry32(seed ^ 0x5bd1e995), off = [];
  oct.forEach(function(){ off.push([r()*1000, r()*1000]); });
  return function(x, z){
    var h = 0;
    for(var i = 0; i < oct.length; i++) h += oct[i][0]*n(x/oct[i][1] + off[i][0], z/oct[i][1] + off[i][1]);
    return h;
  };
}

/* ---------- the road: a centre line that always makes progress ---------- */
// Heading stays within +-MAXDEV of the stage's main direction, so the road never comes
// back on itself; a coarse grid also keeps separate stretches >= 55 m apart.
var MAXDEV = 78*DEG;
function genPath(rng, target, biome){
  var h0 = rng()*TAU, pts = [{x: 0, z: 0, h: h0}], x = 0, z = 0, h = h0, s = 0, STEP = 2;
  var hash = {}, total = target + RUNOUT + 20;
  function key(px, pz){ return Math.floor(px/50) + "," + Math.floor(pz/50); }
  function near(px, pz, upto){
    var cx = Math.floor(px/50), cz = Math.floor(pz/50);
    for(var a = -1; a <= 1; a++) for(var b = -1; b <= 1; b++){
      var L = hash[(cx + a) + "," + (cz + b)]; if(!L) continue;
      for(var k = 0; k < L.length; k++){ var q = pts[L[k]]; if(L[k] < upto && (q.x - px)*(q.x - px) + (q.z - pz)*(q.z - pz) < 55*55) return true; }
    }
    return false;
  }
  function add(p){ pts.push(p); var k = key(p.x, p.z); (hash[k] = hash[k] || []).push(pts.length - 1); }
  add(pts[0]); pts.pop();
  var tight = biome === "forest" ? 0.22 : biome === "snow" ? 0.26 : 0.18;
  var guard = 0, startRun = 60;
  while(s < total && guard++ < 400){
    // one segment: a straight or a constant-radius corner
    var seg;
    if(s < startRun) seg = {len: startRun, k: 0};
    else if(rng() < 0.32) seg = {len: 30 + rng()*140, k: 0};
    else {
      var u = rng(), R = u < 0.32 ? 90 + rng()*130 : u < 0.32 + (0.68 - tight) ? 42 + rng()*48 : 20 + rng()*22;
      var ang = (20 + rng()*(R < 45 ? 120 : 85))*DEG, dir = rng() < 0.5 ? -1 : 1;
      // keep the heading inside the cone around h0: turn back toward it when near the edge
      var dev = wrapAng(h - h0);
      if(Math.abs(dev + dir*ang) > MAXDEV) dir = -dir;
      if(Math.abs(dev + dir*ang) > MAXDEV) ang = Math.max(15*DEG, MAXDEV - Math.abs(dev));
      seg = {len: R*ang, k: dir/R};
    }
    var n = Math.max(1, Math.round(seg.len/STEP)), save = pts.length, ok = true, sx = x, sz = z, sh = h, ss = s;
    for(var i = 0; i < n; i++){
      h += seg.k*STEP;
      if(Math.abs(wrapAng(h - h0)) > MAXDEV + 2*DEG){ h -= seg.k*STEP; }
      x += Math.sin(h)*STEP; z -= Math.cos(h)*STEP; s += STEP;
      if(near(x, z, pts.length - 40)){ ok = false; break; }
      add({x: x, z: z, h: h});
      if(s >= total) break;
    }
    if(!ok){
      // undo and run straight toward the main direction instead
      while(pts.length > save){ var p = pts.pop(), kk = key(p.x, p.z), L2 = hash[kk]; if(L2) L2.pop(); }
      x = sx; z = sz; h = sh; s = ss;
      var turn = wrapAng(h0 - h), m = Math.max(4, Math.round(Math.abs(turn)/(1/60)/STEP));
      for(var j = 0; j < m + 20 && s < total; j++){
        h += clamp(turn, -1/60*STEP, 1/60*STEP); turn = wrapAng(h0 - h);
        x += Math.sin(h)*STEP; z -= Math.cos(h)*STEP; s += STEP;
        add({x: x, z: z, h: h});
      }
    }
  }
  return pts;
}

// Smooth a closed-form heading series: a moving average of headings (unwrapped) -> new path.
function smoothPath(pts, win){
  var n = pts.length, hs = new Float64Array(n), un = 0, out = [];
  hs[0] = pts[0].h;
  for(var i = 1; i < n; i++){ un += wrapAng(pts[i].h - pts[i-1].h); hs[i] = pts[0].h + un; }
  var x = pts[0].x, z = pts[0].z;
  for(var k = 0; k < n; k++){
    var a = 0, c = 0;
    for(var j = Math.max(0, k - win); j <= Math.min(n - 1, k + win); j++){ a += hs[j]; c++; }
    var h = a/c;
    if(k > 0){ x += Math.sin(h)*2; z -= Math.cos(h)*2; }
    out.push({x: x, z: z, h: h});
  }
  return out;
}

/* ---------- Catmull-Rom resampling of the doc road to a dense centre line ---------- */
function resample(road, step){
  var out = [], n = road.length;
  function P(i){ return road[clamp(i, 0, n - 1)]; }
  for(var i = 0; i < n - 1; i++){
    var p0 = P(i - 1), p1 = P(i), p2 = P(i + 1), p3 = P(i + 2);
    var dx = p2[0] - p1[0], dz = p2[1] - p1[1], len = Math.sqrt(dx*dx + dz*dz), m = Math.max(1, Math.round(len/step));
    for(var k = 0; k < m; k++){
      var t = k/m, t2 = t*t, t3 = t2*t;
      var f = [0, 1, 2].map(function(c){
        return 0.5*((2*p1[c]) + (-p0[c] + p2[c])*t + (2*p0[c] - 5*p1[c] + 4*p2[c] - p3[c])*t2 + (-p0[c] + 3*p1[c] - 3*p2[c] + p3[c])*t3);
      });
      out.push({x: f[0], z: f[1], y: f[2]});
    }
  }
  var L = road[n - 1]; out.push({x: L[0], z: L[1], y: L[2]});
  return out;
}

/* ---------- build ---------- */
function build(opts, doc){
  opts = opts || {};
  var seed = String(opts.seed != null ? opts.seed : "rally").slice(0, 64);
  var biome = BIOMES.indexOf(opts.biome) >= 0 ? opts.biome : "forest";
  var night = !!opts.night, target = clamp(Math.round(+opts.length || 3000), 2500, 4000);
  if(doc && doc.data && typeof doc.data.length === "number") target = clamp(Math.round(doc.data.length), 1000, 6000);
  var master = mulberry32(hashStr(seed));
  var roadSeed = Math.floor(master()*4294967296) >>> 0, terrSeed = Math.floor(master()*4294967296) >>> 0,
      decoSeed = Math.floor(master()*4294967296) >>> 0, surfSeed = Math.floor(master()*4294967296) >>> 0;
  var natural = makeTerrain(terrSeed, biome);
  var road, gates, name = opts.name ? String(opts.name).slice(0, 40) : nameFor(seed);

  if(doc && doc.data && Array.isArray(doc.data.road)){
    road = doc.data.road.map(function(p){ return [+p[0], +p[1], +p[2]]; });
    gates = (doc.data.gates || []).map(function(g){ return {x: +g.x, z: +g.z, h: +g.h, w: +g.w}; });
    name = String(doc.name || name);
  } else {
    /* 1. the centre line */
    var raw = smoothPath(genPath(mulberry32(roadSeed), target, biome), 6);
    var total = target + RUNOUT;
    while(raw.length > 2 && (raw.length - 1)*2 > total + 2) raw.pop();
    /* 2. the elevation profile: the land under it, smoothed, grade-limited, plus jumps */
    var n = raw.length, ys = new Float64Array(n), i, j;
    for(i = 0; i < n; i++) ys[i] = natural(raw[i].x, raw[i].z);
    for(var pass = 0; pass < 2; pass++){
      var tmp = new Float64Array(n), W = 16;
      for(i = 0; i < n; i++){ var a = 0, c = 0; for(j = Math.max(0, i - W); j <= Math.min(n - 1, i + W); j++){ a += ys[j]; c++; } tmp[i] = a/c; }
      ys = tmp;
    }
    var maxG = biome === "hills" ? 0.12 : 0.1;
    for(i = 1; i < n; i++) ys[i] = clamp(ys[i], ys[i-1] - maxG*2, ys[i-1] + maxG*2);
    for(i = n - 2; i >= 0; i--) ys[i] = clamp(ys[i], ys[i+1] - maxG*2, ys[i+1] + maxG*2);
    // crests to jump: on straight-ish stretches, a ramp up then a short drop
    var jr = mulberry32(roadSeed ^ 0x9e3779b9), jumps = 2 + Math.floor(jr()*3), placed = 0, tries = 0;
    while(placed < jumps && tries++ < 60){
      var at = 120 + Math.floor(jr()*(n - 260));
      var straight = true;
      for(j = at - 20; j < at + 26; j++) if(Math.abs(wrapAng(raw[j+1].h - raw[j].h)) > 0.012) straight = false;
      if(!straight) continue;
      // a 36 m ramp to a crest 0.8-1.6 m proud, then a 20 m fall back: a car at full speed flies
      // a few lengths and lands on the downslope, not on its nose
      var A = 0.8 + jr()*0.8;
      for(j = 0; j < 28; j++){ var u = j/18, b = j < 18 ? smooth(u)*A : A*smooth(1 - (j - 18)/10); if(at - 18 + j < n) ys[at - 18 + j] += b; }
      placed++;
    }
    /* 3. the MapDoc road: every ~10 m, rounded */
    var sp = Math.max(5, Math.ceil(((n - 1)*2)/390/2)*2), step = sp/2;
    road = [];
    for(i = 0; i < n; i += step) road.push([r1(raw[i].x), r1(raw[i].z), r1(ys[i])]);
    if((n - 1) % step) road.push([r1(raw[n-1].x), r1(raw[n-1].z), r1(ys[n-1])]);
    gates = null;
  }

  /* 4. the dense centre line every ~2 m from the (rounded) doc road */
  var pts = resample(road, 2), np = pts.length, s = 0, k;
  for(k = 0; k < np; k++){
    if(k > 0){ var ddx = pts[k].x - pts[k-1].x, ddz = pts[k].z - pts[k-1].z; s += Math.sqrt(ddx*ddx + ddz*ddz); }
    pts[k].s = s;
    var A2 = pts[Math.max(0, k - 1)], B2 = pts[Math.min(np - 1, k + 1)];
    pts[k].h = Math.atan2(B2.x - A2.x, -(B2.z - A2.z));
  }
  var curv = new Float64Array(np);
  for(k = 0; k < np; k++){
    var ka = Math.max(0, k - 5), kb = Math.min(np - 1, k + 5), dsk = pts[kb].s - pts[ka].s;
    curv[k] = dsk > 0 ? wrapAng(pts[kb].h - pts[ka].h)/dsk : 0;
  }
  for(k = 0; k < np; k++){
    var sum = 0, cnt = 0;
    for(var q = Math.max(0, k - 6); q <= Math.min(np - 1, k + 6); q++){ sum += curv[q]; cnt++; }
    pts[k].k = sum/cnt;
    // bank toward the inside, up to 12 degrees (a 30 m corner)
    pts[k].bank = clamp(pts[k].k*360, -12, 12)*DEG;
  }
  var roadLen = pts[np - 1].s, finishS = Math.max(200, roadLen - RUNOUT);

  /* 5. gates: 20 along the stage, the last one is the finish */
  if(!gates){
    gates = [];
    for(var g = 0; g < GATES; g++){
      var gs = finishS*(g + 1)/GATES, gp = pointAtS(pts, gs);
      gates.push({x: r1(gp.x), z: r1(gp.z), h: ((Math.round(gp.h/DEG) % 360) + 360) % 360, w: Math.round(ROAD_HALF + SHOULDER + DITCH + 3)});
    }
  }
  var start = {x: pts[0].x, z: pts[0].z, h: (((pts[0].h/DEG) % 360) + 360) % 360};

  /* 6. road sections and their surface */
  var sr = mulberry32(surfSeed), sections = [], s0 = 0;
  var mix = biome === "forest" ? [[S_GRAVEL, 0.62], [S_DIRT, 0.28], [S_TARMAC, 0.10]]
          : biome === "hills" ? [[S_DIRT, 0.5], [S_GRAVEL, 0.4], [S_TARMAC, 0.10]] : [[S_SNOW, 0.85], [S_GRAVEL, 0.15]];
  var prevSurf = -1;
  while(s0 < roadLen){
    var len = 450 + sr()*500, pick = sr(), acc = 0, surf = mix[0][0];
    for(var m2 = 0; m2 < mix.length; m2++){ acc += mix[m2][1]; if(pick <= acc){ surf = mix[m2][0]; break; } }
    if(surf === prevSurf && mix.length > 1) surf = mix[(mix.findIndex(function(e){ return e[0] === surf; }) + 1) % mix.length][0];
    sections.push({s0: s0, s1: Math.min(roadLen, s0 + len), surf: surf}); prevSurf = surf; s0 += len;
  }
  if(sections.length) sections[0].surf = biome === "snow" ? S_SNOW : biome === "hills" ? S_DIRT : S_GRAVEL;
  function surfAtS(sv){ for(var i = 0; i < sections.length; i++) if(sv < sections[i].s1) return sections[i].surf; return sections[sections.length - 1].surf; }
  for(k = 0; k < np; k++) pts[k].surf = surfAtS(pts[k].s);

  /* 7. the heightfield grid: natural land, the road carved in */
  var minX = 1e9, maxX = -1e9, minZ = 1e9, maxZ = -1e9;
  pts.forEach(function(p){ minX = Math.min(minX, p.x); maxX = Math.max(maxX, p.x); minZ = Math.min(minZ, p.z); maxZ = Math.max(maxZ, p.z); });
  var MARGIN = 170;
  var x0 = Math.floor((minX - MARGIN)/CELL)*CELL, z0 = Math.floor((minZ - MARGIN)/CELL)*CELL;
  var nx = Math.ceil((maxX + MARGIN - x0)/CELL) + 1, nz = Math.ceil((maxZ + MARGIN - z0)/CELL) + 1;
  var H = new Float32Array(nx*nz), SURF = new Uint8Array(nx*nz), LAT = new Float32Array(nx*nz), IDX = new Int32Array(nx*nz);
  var offSurf = biome === "snow" ? S_SNOW : S_GRASS;
  var patch = makeNoise(surfSeed ^ 0x1234567);
  var gi2, gx, gz;
  for(gz = 0; gz < nz; gz++) for(gx = 0; gx < nx; gx++){
    gi2 = gz*nx + gx; var wx = x0 + gx*CELL, wz = z0 + gz*CELL;
    H[gi2] = natural(wx, wz); LAT[gi2] = 1e9; IDX[gi2] = -1;
    var pn = patch(wx/23, wz/23);
    SURF[gi2] = biome === "snow" ? (pn > 0.55 ? S_GRAVEL : S_SNOW) : biome === "hills" ? (pn > 0.35 ? S_DIRT : S_GRASS) : (pn > 0.5 ? S_MUD : S_GRASS);
  }
  // nearest centre-line point (and signed lateral offset, + = right of travel) per cell
  // (the first and last points also carve a straight 40 m lead-in and run-off)
  var RC = 34;
  for(k = 0; k < np; k++){
    var P0 = pts[k], tx = Math.sin(P0.h), tz = -Math.cos(P0.h), cx = Math.round((P0.x - x0)/CELL), cz = Math.round((P0.z - z0)/CELL);
    var aMin = k === 0 ? -40 : -1.4, aMax = k === np - 1 ? 40 : 1.4, rc = Math.ceil((k === 0 || k === np - 1 ? RC + 40 : RC)/CELL);
    for(var oz = -rc; oz <= rc; oz++){
      var gzz = cz + oz; if(gzz < 0 || gzz >= nz) continue;
      for(var ox = -rc; ox <= rc; ox++){
        var gxx = cx + ox; if(gxx < 0 || gxx >= nx) continue;
        var ii = gzz*nx + gxx, dx2 = x0 + gxx*CELL - P0.x, dz2 = z0 + gzz*CELL - P0.z;
        var along = dx2*tx + dz2*tz, lat = dx2*(-tz) + dz2*tx;     // right = (-tz, tx)... (cos h, sin h)
        if(along < aMin || along > aMax || Math.abs(lat) > RC) continue;
        if(Math.abs(lat) < Math.abs(LAT[ii])){ LAT[ii] = lat; IDX[ii] = k; }
      }
    }
  }
  var edge = ROAD_HALF + SHOULDER + DITCH, blendOut = 22;
  for(gi2 = 0; gi2 < nx*nz; gi2++){
    var idx = IDX[gi2]; if(idx < 0) continue;
    var Pp = pts[idx], la = LAT[gi2], al = Math.abs(la), sg = la >= 0 ? 1 : -1;
    var tb = Math.tan(Pp.bank);
    // road plane: banked (inside lower); a turn to the right (+k) lowers the right side
    var yRoad = Pp.y - la*tb;
    if(al <= ROAD_HALF){
      // shallow wheel ruts and a slight crown
      var rut = -0.035*Math.exp(-Math.pow((al - 0.78)/0.22, 2)) - 0.012*(al/ROAD_HALF)*(al/ROAD_HALF);
      H[gi2] = yRoad + rut; SURF[gi2] = Pp.surf;
      continue;
    }
    var yEdge = Pp.y - sg*ROAD_HALF*tb;
    var yShould = yEdge - (al - ROAD_HALF)*0.06;
    if(al <= ROAD_HALF + SHOULDER){ H[gi2] = yShould; SURF[gi2] = Pp.surf === S_TARMAC ? S_LOOSE : S_LOOSE; if(biome === "snow") SURF[gi2] = S_SNOW; continue; }
    var u2 = (al - ROAD_HALF - SHOULDER)/DITCH, yDitchBase = yEdge - SHOULDER*0.06;
    var nat = H[gi2];
    if(al <= edge){
      var dip = -0.5*Math.sin(PI*clamp(u2, 0, 1));
      var target2 = lerp(yDitchBase, nat, smooth(u2)*0.25) + dip;
      H[gi2] = target2;
      SURF[gi2] = biome === "snow" ? S_SNOW : biome === "forest" ? (u2 > 0.25 && u2 < 0.75 ? S_MUD : S_GRASS) : S_GRASS;
      continue;
    }
    var bw = smooth((al - edge)/blendOut), yOut = lerp(yDitchBase, nat, 0.25);
    H[gi2] = lerp(yOut, nat, bw);
  }
  function cellH(ix, iz){ ix = clamp(ix, 0, nx - 1); iz = clamp(iz, 0, nz - 1); return H[iz*nx + ix]; }
  function height(x, z){
    var fx = (x - x0)/CELL, fz = (z - z0)/CELL;
    if(fx < 0 || fz < 0 || fx >= nx - 1 || fz >= nz - 1) return natural(x, z);
    var ix = Math.floor(fx), iz = Math.floor(fz), u = fx - ix, w = fz - iz, i0 = iz*nx + ix;
    var a = H[i0], b = H[i0 + 1], c = H[i0 + nx], d = H[i0 + nx + 1];
    return a + (b - a)*u + (c - a)*w + (a - b - c + d)*u*w;
  }
  function normal(x, z, out){
    out = out || [0, 1, 0];
    var fx = (x - x0)/CELL, fz = (z - z0)/CELL, hx, hz;
    if(fx < 0 || fz < 0 || fx >= nx - 1 || fz >= nz - 1){
      var e = 0.5; hx = (natural(x + e, z) - natural(x - e, z))/(2*e); hz = (natural(x, z + e) - natural(x, z - e))/(2*e);
    } else {
      var ix = Math.floor(fx), iz = Math.floor(fz), u = fx - ix, w = fz - iz, i0 = iz*nx + ix;
      var a = H[i0], b = H[i0 + 1], c = H[i0 + nx], d = H[i0 + nx + 1];
      hx = ((b - a) + (a - b - c + d)*w)/CELL; hz = ((c - a) + (a - b - c + d)*u)/CELL;
    }
    var l = Math.sqrt(hx*hx + 1 + hz*hz); out[0] = -hx/l; out[1] = 1/l; out[2] = -hz/l; return out;
  }
  function surface(x, z){
    var ix = Math.round((x - x0)/CELL), iz = Math.round((z - z0)/CELL);
    if(ix < 0 || iz < 0 || ix >= nx || iz >= nz) return offSurf;
    return SURF[iz*nx + ix];
  }

  /* 8. scenery (positions only; scene.js draws them) */
  var dr = mulberry32(decoSeed), sc = {trees: [], rocks: [], tufts: [], fences: [], chevrons: [], banners: [], arches: [], tower: null, posts: [], bales: [], flags: []};
  var obst = {};
  function addObst(x, z, r){ var kk = Math.floor(x/8) + "," + Math.floor(z/8); (obst[kk] = obst[kk] || []).push([x, z, r]); }
  function latAt(x, z){
    var ix = Math.round((x - x0)/CELL), iz = Math.round((z - z0)/CELL);
    if(ix < 0 || iz < 0 || ix >= nx || iz >= nz) return 1e9;
    var v = LAT[iz*nx + ix]; return v > 1e8 ? 1e9 : Math.abs(v);
  }
  var treeCell = biome === "forest" ? 6.5 : biome === "snow" ? 8 : 15;
  var treeDensity = biome === "forest" ? 0.72 : biome === "snow" ? 0.52 : 0.22;
  var clump = makeNoise(decoSeed ^ 0xabcdef);
  var TX0 = x0 + 10, TZ0 = z0 + 10, TX1 = x0 + (nx - 1)*CELL - 10, TZ1 = z0 + (nz - 1)*CELL - 10;
  for(var tzv = TZ0; tzv < TZ1; tzv += treeCell) for(var txv = TX0; txv < TX1; txv += treeCell){
    var px = txv + (dr() - 0.5)*treeCell*0.9, pz = tzv + (dr() - 0.5)*treeCell*0.9, roll = dr(), kind = dr(), scl = 0.75 + dr()*0.65, rot = dr()*TAU;
    var dlat = latAt(px, pz);
    if(dlat < edge + 3.5) continue;
    var cl = clump(px/60, pz/60)*0.5 + 0.5;
    var prob = treeDensity*(0.35 + 0.9*cl)*(dlat > 120 ? 0.85 : 1);
    if(roll > prob) continue;
    var type = biome === "hills" ? (kind < 0.7 ? 1 : 0) : biome === "snow" ? (kind < 0.9 ? 0 : 2) : (kind < 0.62 ? 0 : kind < 0.9 ? 1 : 2);
    var ty = height(px, pz);
    sc.trees.push({x: px, z: pz, y: ty, t: type, s: scl, r: rot, v: dr()});
    if(dlat < edge + 30) addObst(px, pz, 0.45*scl + 0.15);
  }
  // rocks: scattered, plus a few on the outside of corners
  for(var rk = 0; rk < (biome === "hills" ? 520 : 360); rk++){
    var rp = pts[Math.floor(dr()*np)], side = dr() < 0.5 ? -1 : 1, off = edge + 2 + dr()*70;
    var rx = rp.x + Math.cos(rp.h)*off*side, rz = rp.z + Math.sin(rp.h)*off*side;
    if(latAt(rx, rz) < edge + 1.5) continue;
    var rs = 0.4 + Math.pow(dr(), 2)*2.2;
    sc.rocks.push({x: rx, z: rz, y: height(rx, rz), s: rs, r: dr()*TAU, v: dr()});
    if(rs > 0.7 && latAt(rx, rz) < edge + 30) addObst(rx, rz, rs*0.75);
  }
  // grass tufts close to the road (and snow lumps in the snow)
  for(var tf = 0; tf < 5200; tf++){
    var tp = pts[Math.floor(dr()*np)], ts = dr() < 0.5 ? -1 : 1, to = ROAD_HALF + 1.2 + Math.pow(dr(), 1.6)*26;
    var tfx = tp.x + Math.cos(tp.h)*to*ts + (dr() - 0.5)*3, tfz = tp.z + Math.sin(tp.h)*to*ts + (dr() - 0.5)*3;
    if(latAt(tfx, tfz) < ROAD_HALF + 0.8) continue;
    sc.tufts.push({x: tfx, z: tfz, y: height(tfx, tfz), s: 0.6 + dr()*0.8, r: dr()*TAU, v: dr()});
  }
  // chevron boards on the outside of the tighter corners, facing the driver
  var lastChev = -100;
  for(k = 20; k < np - 20; k++){
    var kk2 = pts[k].k;
    if(Math.abs(kk2) < 1/55 || pts[k].s - lastChev < 70) continue;
    // the apex: the local curvature peak
    if(Math.abs(kk2) < Math.abs(pts[k-1].k) || Math.abs(kk2) < Math.abs(pts[k+1].k)) continue;
    lastChev = pts[k].s;
    var outside = kk2 > 0 ? -1 : 1, nb = Math.abs(kk2) > 1/30 ? 4 : 3;
    for(var cb = 0; cb < nb; cb++){
      var cp = pts[clamp(k - 6 + cb*5, 0, np - 1)], co = ROAD_HALF + SHOULDER + DITCH + 1.2;
      var chx = cp.x + Math.cos(cp.h)*co*outside, chz = cp.z + Math.sin(cp.h)*co*outside;
      sc.chevrons.push({x: chx, z: chz, y: height(chx, chz), h: cp.h, dir: kk2 > 0 ? 1 : -1});
      addObst(chx, chz, 0.25);
    }
  }
  // fences along some straights (dry hills), hay bales on the outside of jumps
  if(biome !== "forest"){
    var fs = 200;
    while(fs < roadLen - 200){
      var flen = 60 + dr()*140, fside = dr() < 0.5 ? -1 : 1, foff = edge + 4 + dr()*3;
      for(var fsv = fs; fsv < fs + flen; fsv += 3.2){
        var fp = pointAtS(pts, fsv), fx2 = fp.x + Math.cos(fp.h)*foff*fside, fz2 = fp.z + Math.sin(fp.h)*foff*fside;
        sc.fences.push({x: fx2, z: fz2, y: height(fx2, fz2), h: fp.h});
      }
      fs += flen + 150 + dr()*400;
    }
  }
  // sponsor banners: by the start, the finish and every ~550 m
  var bs = 40, bside = 1;
  while(bs < roadLen - 40){
    var bp = pointAtS(pts, bs), bo = edge + 2.2, bx = bp.x + Math.cos(bp.h)*bo*bside, bz = bp.z + Math.sin(bp.h)*bo*bside;
    sc.banners.push({x: bx, z: bz, y: height(bx, bz), h: bp.h, v: Math.floor(dr()*8), side: bside});
    addObst(bx, bz, 0.2);
    bs += bs < 100 ? 30 : 380 + dr()*340; bside = -bside;
  }
  var fp2 = pointAtS(pts, finishS - 30), fo2 = edge + 2.2;
  sc.banners.push({x: fp2.x - Math.cos(fp2.h)*fo2, z: fp2.z - Math.sin(fp2.h)*fo2, y: height(fp2.x - Math.cos(fp2.h)*fo2, fp2.z - Math.sin(fp2.h)*fo2), h: fp2.h, v: 1, side: -1});
  // start and finish arches; split-gate posts (small flags at every gate)
  var st0 = pointAtS(pts, 6), fin = pointAtS(pts, finishS);
  sc.arches.push({x: st0.x, z: st0.z, y: st0.y, h: st0.h, kind: "start", w: ROAD_HALF + 2.5});
  sc.arches.push({x: fin.x, z: fin.z, y: fin.y, h: fin.h, kind: "finish", w: ROAD_HALF + 2.5});
  [st0, fin].forEach(function(a){ addObst(a.x + Math.cos(a.h)*(ROAD_HALF + 2.5), a.z + Math.sin(a.h)*(ROAD_HALF + 2.5), 0.35); addObst(a.x - Math.cos(a.h)*(ROAD_HALF + 2.5), a.z - Math.sin(a.h)*(ROAD_HALF + 2.5), 0.35); });
  gates.forEach(function(g, gi3){
    if(gi3 === gates.length - 1) return;
    var hr = g.h*DEG, o = ROAD_HALF + SHOULDER + 0.6;
    [-1, 1].forEach(function(sd){ var fx3 = g.x + Math.cos(hr)*o*sd, fz3 = g.z + Math.sin(hr)*o*sd; sc.flags.push({x: fx3, z: fz3, y: height(fx3, fz3), h: hr, i: gi3}); });
  });
  // the water tower (dry hills), set back from the middle of the stage
  if(biome === "hills"){
    var wp = pointAtS(pts, roadLen*0.45), wo = 42, wx2 = wp.x + Math.cos(wp.h)*wo, wz2 = wp.z + Math.sin(wp.h)*wo;
    sc.tower = {x: wx2, z: wz2, y: height(wx2, wz2), h: wp.h};
  }

  function obstacles(x, z, r, cb){
    var c0 = Math.floor((x - r)/8), c1 = Math.floor((x + r)/8), d0 = Math.floor((z - r)/8), d1 = Math.floor((z + r)/8);
    for(var a = c0; a <= c1; a++) for(var b = d0; b <= d1; b++){
      var L = obst[a + "," + b]; if(!L) continue;
      for(var i = 0; i < L.length; i++) cb(L[i][0], L[i][1], L[i][2]);
    }
  }
  // nearest centre-line point: a window around the hint, or the whole line
  function locate(x, z, hint){
    var a = 0, b = np - 1;
    if(hint != null && hint >= 0){ a = Math.max(0, (hint|0) - 60); b = Math.min(np - 1, (hint|0) + 60); }
    var best = 1e18, bi = a;
    for(var i = a; i <= b; i++){ var dx = pts[i].x - x, dz = pts[i].z - z, d = dx*dx + dz*dz; if(d < best){ best = d; bi = i; } }
    var P = pts[bi], tx = Math.sin(P.h), tz = -Math.cos(P.h), dx2 = x - P.x, dz2 = z - P.z;
    var along = dx2*tx + dz2*tz, lat = dx2*(-tz) + dz2*tx;
    return {i: bi, s: clamp(P.s + along, 0, roadLen), lat: lat, d: Math.sqrt(best)};
  }

  var gd = gates.map(function(g){ return {x: g.x, z: g.z, h: g.h, w: g.w}; });
  var gateS = gd.map(function(g){ return locate(g.x, g.z, null).s; });
  var outDoc = {kind: "rally", v: 1, name: name.slice(0, 40), data: {seed: seed, biome: biome, night: night, length: target, road: road, gates: gd}};
  return {
    seed: seed, biome: biome, night: night, target: target, name: outDoc.name, doc: outDoc,
    pts: pts, gates: gd, start: start, length: finishS, roadLen: roadLen, sections: sections,
    grid: {x0: x0, z0: z0, cell: CELL, nx: nx, nz: nz, h: H, surf: SURF},
    natural: natural, scenery: sc, roadHalf: ROAD_HALF, edge: edge,
    env: {height: height, normal: normal, surface: surface, obstacles: obstacles},
    gateS: gateS, locate: locate, pointAtS: function(sv){ return pointAtS(pts, sv); }
  };
}
// point on the dense line at arc length sv (linear between samples)
function pointAtS(pts, sv){
  var n = pts.length, lo = 0, hi = n - 1;
  if(sv <= 0) return pts[0];
  if(sv >= pts[n-1].s) return pts[n-1];
  while(hi - lo > 1){ var mid = (lo + hi) >> 1; if(pts[mid].s <= sv) lo = mid; else hi = mid; }
  var A = pts[lo], B = pts[hi], u = (sv - A.s)/((B.s - A.s) || 1);
  return {x: lerp(A.x, B.x, u), z: lerp(A.z, B.z, u), y: lerp(A.y, B.y, u), h: A.h + wrapAng(B.h - A.h)*u, s: sv, k: lerp(A.k, B.k, u), bank: lerp(A.bank, B.bank, u), i: lo};
}

/* ---------- gates ---------- */
// Did the move (x0,z0) -> (x1,z1) cross gate g forward (from behind its line to on/past
// it) within its half-width? Returns the fraction along the move, or false.
function crossesGate(g, x0, z0, x1, z1){
  var h = (+g.h || 0)*DEG, fx = Math.sin(h), fz = -Math.cos(h);
  var d0 = (x0 - g.x)*fx + (z0 - g.z)*fz, d1 = (x1 - g.x)*fx + (z1 - g.z)*fz;
  if(!(d0 < 0 && d1 >= 0)) return false;
  var u = -d0/(d1 - d0), cx = x0 + (x1 - x0)*u, cz = z0 + (z1 - z0)*u;
  var lat = (cx - g.x)*Math.cos(h) + (cz - g.z)*Math.sin(h);
  return Math.abs(lat) <= (+g.w || 0) ? Math.max(u, 1e-6) : false;
}

/* ---------- MapDoc validation (the Arena referee's limits, backend/src/rally.rs) ---------- */
// seed: 1-64 printable characters; length 1000-6000 m; 50-400 road points [x, z, y] within
// 5000 m, 2-30 m apart; 5-40 gates, half-width 4-20 m. Coordinates round to 0.1 m, headings
// to whole degrees, the length to whole metres, as the referee's canonical form does.
function num(v, lim){ if(typeof v !== "number") return null; return isFinite(v) && Math.abs(v) <= lim ? v : null; }
function cleanDoc(doc){
  if(!doc || typeof doc !== "object" || doc.kind !== "rally" || +doc.v !== 1 || !doc.data || typeof doc.data !== "object") return null;
  var d = doc.data;
  if(typeof d.seed !== "string" || !d.seed.length || d.seed.length > 64 || /[\u0000-\u001f\u007f]/.test(d.seed)) return null;
  if(BIOMES.indexOf(d.biome) < 0 || typeof d.night !== "boolean") return null;
  var len = typeof d.length === "number" ? Math.round(d.length) : NaN; if(!(len >= 1000 && len <= 6000)) return null;
  if(!Array.isArray(d.road) || d.road.length < 50 || d.road.length > 400) return null;
  if(!Array.isArray(d.gates) || d.gates.length < 5 || d.gates.length > 40) return null;
  var road = [];
  for(var i = 0; i < d.road.length; i++){
    var p = d.road[i]; if(!Array.isArray(p) || p.length !== 3) return null;
    var x = num(p[0], 5000), z = num(p[1], 5000), y = num(p[2], 5000);
    if(x === null || z === null || y === null) return null;
    road.push([r1(x), r1(z), r1(y)]);
    if(i > 0){ var gap = Math.sqrt(Math.pow(road[i][0] - road[i-1][0], 2) + Math.pow(road[i][1] - road[i-1][1], 2)); if(gap < 2 || gap > 30) return null; }
  }
  var gates = [];
  for(var j = 0; j < d.gates.length; j++){
    var g = d.gates[j]; if(!g || typeof g !== "object") return null;
    var gx = num(g.x, 5000), gz = num(g.z, 5000), gh = num(g.h, 1e6), gw = num(g.w, 1e6);
    if(gx === null || gz === null || gh === null || gw === null) return null;
    gw = r1(gw); if(gw < 4 || gw > 20) return null;
    gates.push({x: r1(gx), z: r1(gz), h: ((Math.round(gh) % 360) + 360) % 360, w: gw});
  }
  var name = String(doc.name || "").replace(/[<>]/g, "").trim().slice(0, 40) || "Rally stage";
  return {kind: "rally", v: 1, name: name, data: {seed: d.seed, biome: d.biome, night: d.night, length: len, road: road, gates: gates}};
}
// The stage key the Arena uses ("c-" + 12 hex) is the server's; locally a doc gets an
// "x-" id from its content for the solo bests and ghosts.
function localKey(doc){
  var d = doc && doc.data; if(!d) return "x-0";
  return "x-" + (hashStr(d.seed + "|" + d.biome + "|" + (d.night ? 1 : 0) + "|" + d.length) >>> 0).toString(16);
}

var API = {build: build, cleanDoc: cleanDoc, crossesGate: crossesGate, daily: daily, event: event, nameFor: nameFor,
  localKey: localKey, utcDay: utcDay, PRESETS: PRESETS, BIOMES: BIOMES, BIOME_NAMES: BIOME_NAMES, GATES: GATES,
  hashStr: hashStr, mulberry32: mulberry32, pointAtS: pointAtS};
if(typeof module === "object" && module.exports) module.exports = API;
else { root.HQV = root.HQV || {}; root.HQV.rallyStage = API; }
})(typeof window !== "undefined" ? window : this);
