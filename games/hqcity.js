/* Claude HQ 2.1: Arena City, the street of open HQs.
 *
 * Everyone who opened their HQ to visitors has a building in a city of square blocks:
 * a grid of roads, the plaza on the middle block, each HQ on a block of its own facing
 * the road, built from that HQ's level, paint, accent, sign (its name, on the banners)
 * and decor. Your own building is one of them. Walk up to someone's front door to go
 * into their HQ as a visitor (their Lobby); your own door takes you into your Lobby.
 * Every tab of HQ has a building too (games/hqbase.js, HQV.hqViews): its door opens it.
 *
 * A floor of the 3D HQ: registered as HQV.hqWorlds.city and built by games/hq3d.js.
 * The street comes from ctx.api.city() (the Arena's list of open HQs: counts and
 * cosmetics only) and is rebuilt when that list changes (w.onCity). Everyone gets
 * the same layout from the same list (sorted by user id; the view buildings are a fixed
 * list), so the people walking the street (live presence, room "hq_city") stand in the
 * same places for everyone.
 */
(function(){
"use strict";
var HQV = window.HQV = window.HQV || {};
HQV.hqWorlds = HQV.hqWorlds || {};

var MAX_LOTS = 12;          // an Arena has a handful of members (5 today); room to spare
var BLOCK_W = 36, BLOCK_D = 26;   // a block: room for a whole campus with both wings (m)
var ROAD = 8;               // the road round every block (m)
var HQ_Z = 2, VIEW_Z = 6;   // how far forward of the block's middle each kind of building stands
var VIEW_DEPTH = 7;         // a view building's depth (games/hqbase.js VB_D)

// The city's layout from the list: who stands on which block. Pure, so it is easy to test.
// The plaza takes the middle block; HQs the blocks nearest it (by user id), then the view
// buildings. Every building faces +z, onto the road along the front of its block.
function layout(list){
  var hs = (list || []).filter(function(h){ return h && typeof h.userId === "string"; })
    .sort(function(a, b){ return a.userId < b.userId ? -1 : a.userId > b.userId ? 1 : 0; });
  if(hs.length > MAX_LOTS){
    var you = hs.filter(function(h){ return h.isYou; });
    hs = hs.filter(function(h){ return !h.isYou; }).slice(0, MAX_LOTS - you.length).concat(you)
      .sort(function(a, b){ return a.userId < b.userId ? -1 : 1; });
  }
  var views = HQV.hqViews || [];
  var n = hs.length + views.length + 1, cols = Math.ceil(Math.sqrt(n)), rows = Math.ceil(n/cols);
  var PX = BLOCK_W + ROAD, PZ = BLOCK_D + ROAD;
  function cx(i){ return (i - (cols - 1)/2)*PX; }
  function cz(j){ return (j - (rows - 1)/2)*PZ; }
  var pi = Math.floor((cols - 1)/2), pj = Math.floor((rows - 1)/2), cells = [];
  for(var j = 0; j < rows; j++) for(var i = 0; i < cols; i++) if(i !== pi || j !== pj) cells.push({i: i, j: j, d: (i - pi)*(i - pi) + (j - pj)*(j - pj)});
  cells.sort(function(a, b){ return a.d - b.d || a.j - b.j || a.i - b.i; });
  function lot(cell, dz, front){
    var x = cx(cell.i), z = cz(cell.j) + dz;
    return {x: x, z: z, rot: 0, dx: 0, dz: 1, block: {x: cx(cell.i), z: cz(cell.j)}, door: {x: x, z: z + front}};
  }
  return {cols: cols, rows: rows, W: cols*PX, D: rows*PZ, plaza: {x: cx(pi), z: cz(pj)},
    lots: hs.map(function(h, k){ return Object.assign(lot(cells[k], HQ_Z, 4.8), {h: h}); }),          // under the canopy
    views: views.map(function(v, k){ return Object.assign(lot(cells[hs.length + k], VIEW_Z, VIEW_DEPTH/2 + 1.2), {v: v}); })};
}
HQV.hqCityLayout = layout;

// What stands beside every HQ, in its lot's own frame (the door faces +z, the road is at z 11):
// a garage in front of the right wing with its door to the road, and inside it, side by side and
// all facing out, the HQ's car, its motorbike and its bicycle. Drive or ride any of them (E).
// Clear of the tower, both wings and the path to the door; see tests/test_hq3d.py.
var LOT_EXTRAS = {
  garage: {x: 13.0, z: 6.7, w: 7.0, d: 6.2, h: 3.0},
  car: {x: 14.5, z: 7.9, yaw: 0, len: 3.6, wid: 1.7},   // forward is +z: nose at the open door
  moto: {x: 12.2, z: 8.2, yaw: 0},
  bike: {x: 10.8, z: 8.3, yaw: 0}
};
HQV.hqLotExtras = LOT_EXTRAS;
// what you cannot walk (or drive) through, in the lot's frame: the garage's three walls, and
// the car while it is parked (carParked; not while you are driving it). The bike is not.
function lotExtraBlocked(x, z, carParked){
  var G = LOT_EXTRAS.garage, K = LOT_EXTRAS.car, T = 0.2 + 0.3;   // wall + a body's width
  var inX = Math.abs(x - G.x) < G.w/2 + 0.3, inZ = z > G.z - G.d/2 - 0.3 && z < G.z + G.d/2;
  if(inX && Math.abs(z - (G.z - G.d/2)) < T) return true;                          // back wall
  if(inZ && (Math.abs(x - (G.x - G.w/2)) < T || Math.abs(x - (G.x + G.w/2)) < T)) return true;   // side walls
  return carParked !== false && Math.abs(x - K.x) < K.wid/2 + 0.2 && Math.abs(z - K.z) < K.len/2 + 0.2;
}
HQV.hqLotExtraBlocked = lotExtraBlocked;
// an HQ's own colours, read the way its building reads them (games/hqbase.js): paint and accent
function lotColors(look){
  look = look || {};
  var hex = function(v, d){ return /^#[0-9a-f]{6}$/i.test(v || "") ? parseInt(v.slice(1), 16) : d; };
  return {paint: hex(look.paint, 0x2a3c50), accent: hex(look.accent, 0xffb347)};
}
HQV.hqLotColors = lotColors;
function nameOf(h){
  var s = h.look && typeof h.look.sign === "string" && h.look.sign.trim();
  return s || ((h.trainerName || h.displayName || h.handle || "Someone") + "'s HQ");
}
HQV.hqCityName = nameOf;

HQV.hqWorlds.city = function(c, w){
  var THREE = c.THREE, PI = c.PI, S = w.scene, COL = c.COL, HEX = c.HEX;
  var mat = c.mat, emis = c.emis, box = c.box, cyl = c.cyl, sph = c.sph;
  var hits = [], doors = [], night = true;
  var api = c.api || {};

  var hemi = new THREE.HemisphereLight(0xbcd7ff, 0x223322, 2.2); S.add(hemi);
  var sun = new THREE.DirectionalLight(0xffffff, 2.2); sun.position.set(40, 70, 30); sun.castShadow = true;
  sun.shadow.mapSize.set(2048, 2048); S.add(sun); S.add(sun.target);
  var skyBg = {
    night: c.canvasTex(512, 512, function(g){ var gr = g.createLinearGradient(0, 0, 0, 512); gr.addColorStop(0, "#040a12"); gr.addColorStop(0.6, "#0b1826"); gr.addColorStop(1, "#16304a"); g.fillStyle = gr; g.fillRect(0, 0, 512, 512); var R = c.rng(9); for(var i = 0; i < 220; i++){ g.fillStyle = "rgba(255,255,255," + (0.25 + R()*0.7) + ")"; g.fillRect(R()*512, R()*330, 1, 1); } }).t,
    day: c.canvasTex(512, 512, function(g){ var gr = g.createLinearGradient(0, 0, 0, 512); gr.addColorStop(0, "#5ea6dc"); gr.addColorStop(1, "#cfe8f7"); g.fillStyle = gr; g.fillRect(0, 0, 512, 512); }).t
  };
  var grassT = c.canvasTex(256, 256, function(g){ var R = c.rng(4); g.fillStyle = "#1f3d2c"; g.fillRect(0, 0, 256, 256);
    for(var i = 0; i < 900; i++){ g.fillStyle = R() < 0.5 ? "rgba(46,92,58,.5)" : "rgba(22,48,33,.5)"; g.fillRect(R()*256, R()*256, 2 + R()*5, 2 + R()*5); } });
  grassT.t.wrapS = grassT.t.wrapT = THREE.RepeatWrapping;
  var paveT = c.canvasTex(256, 256, function(g){ g.fillStyle = "#3b4c5c"; g.fillRect(0, 0, 256, 256); g.strokeStyle = "rgba(255,255,255,.08)"; g.lineWidth = 2;
    for(var i = 0; i <= 256; i += 32){ g.beginPath(); g.moveTo(i, 0); g.lineTo(i, 256); g.stroke(); g.beginPath(); g.moveTo(0, i); g.lineTo(256, i); g.stroke(); } });
  paveT.t.wrapS = paveT.t.wrapT = THREE.RepeatWrapping;

  var streetG = new THREE.Group(); S.add(streetG);
  var city = {W: 80, D: 60, plaza: {x: 0, z: 0}, lots: [], views: []}, sig = null, lampGlows = [], cars = [], dyn = {facMats: [], glows: [], beacons: [], flags: [], leds: []};
  var fount = null, bikes = [];
  var floor = null;
  function flat(geo, m, y){ var o = new THREE.Mesh(geo, m); o.rotation.x = -PI/2; o.position.y = y; o.receiveShadow = true; streetG.add(o); return o; }

  // one HQ on its lot: the same building as that HQ's own Base (games/hqbase.js), front facing the middle
  var lotsB = [], viewsB = [];
  function building(L){
    var h = L.h, cos = h.cos || {}, crew = h.crew || {}, name = nameOf(h);
    var g = new THREE.Group(); g.position.set(L.x, 0, L.z); g.rotation.y = L.rot; streetG.add(g);
    var pad = new THREE.Mesh(new THREE.PlaneGeometry(30, 15), new THREE.MeshStandardMaterial({map: paveT.t, roughness: 0.9}));
    pad.rotation.x = -PI/2; pad.position.set(0, 0.02, 1.5); pad.receiveShadow = true; g.add(pad);
    if(h.isYou){ var ring = new THREE.Mesh(new THREE.RingGeometry(14.6, 15.1, 64), new THREE.MeshBasicMaterial({color: COL.amber, transparent: true, opacity: 0.5, toneMapped: false}));
      ring.rotation.x = -PI/2; ring.position.set(0, 0.04, 0.5); ring.scale.y = 0.45; g.add(ring); }
    var B = HQV.hqBuilding(c, {
      level: function(){ return Math.max(1, h.level | 0); },
      look: function(){ return Object.assign({}, h.look || {}, {sign: name, decor: cos.decor}); },
      live: function(){ return (crew.working | 0) + (crew.needs | 0); },
      campus: false,
      door: {view: h.isYou ? "@lobby" : "visit:" + h.userId, label: name, userId: h.userId,
             tip: h.isYou ? "Your HQ: go in" : name + ": walk in as a visitor"},
      doorFront: function(){ return new THREE.Vector3(L.door.x, 0, L.door.z); }
    });
    g.add(B.g); B.setLive((crew.working | 0) + (crew.needs | 0)); B.setNight(night);
    var sub = c.label((h.isYou ? "You · " : "") + (crew.working | 0) + " working · " + (crew.needs | 0) + " need them", HEX.green, 0.7);
    sub.position.set(0, B.top() + 5.6, 0); g.add(sub);
    B.hits.forEach(function(o){ hits.push(o); }); B.doors.forEach(function(d){ doors.push(d); });
    L.B = B; L.H = B.top() + 2; L.fade = {g: B.g, on: false}; lotsB.push(B);
    lotExtras(L, g, lotColors(h.look));
  }
  // the garage beside an HQ and its car, motorbike and bicycle, in its colours (LOT_EXTRAS); all
  // three join w.bikes, so hq3d.js lets you drive and ride them
  function lotExtras(L, g, col){
    var X = LOT_EXTRAS, G = X.garage, K = X.car, Bk = X.bike;
    var wallM = mat(col.paint, {roughness: 0.8}), trimM = emis(col.accent, col.accent, 0.35);
    var gg = new THREE.Group(); gg.position.set(G.x, 0, G.z); g.add(gg);
    // the garage: a floor, three walls, a flat roof, the roller door rolled up under the lintel
    box(G.w, 0.06, G.d, emis(0x4a545e, 0xfff1d6, 0.05, {roughness: 0.9}), 0, 0.03, 0, gg);   // the floor, lit by its lamp
    box(G.w, G.h, 0.2, wallM, 0, G.h/2, -G.d/2 + 0.1, gg);
    box(0.2, G.h, G.d, wallM, -G.w/2 + 0.1, G.h/2, 0, gg);
    box(0.2, G.h, G.d, wallM, G.w/2 - 0.1, G.h/2, 0, gg);
    box(G.w + 0.3, 0.2, G.d + 0.3, mat(0x3a4552, {roughness: 0.7}), 0, G.h + 0.1, 0, gg);
    box(G.w, 0.45, 0.2, wallM, 0, G.h - 0.22, G.d/2 - 0.1, gg);                        // the lintel over the opening
    box(G.w + 0.3, 0.12, 0.06, trimM, 0, G.h - 0.06, G.d/2 + 0.02, gg);                // accent strip
    var slats = c.canvasTex(64, 32, function(gx){ gx.fillStyle = "#c9d1d9"; gx.fillRect(0, 0, 64, 32); gx.fillStyle = "rgba(0,0,0,.3)";
      for(var y = 3; y < 32; y += 6) gx.fillRect(0, y, 64, 2); });
    var roll = new THREE.Mesh(new THREE.BoxGeometry(G.w - 0.45, 0.38, 0.4), new THREE.MeshStandardMaterial({map: slats.t, roughness: 0.6, metalness: 0.3}));
    roll.position.set(0, G.h - 0.62, G.d/2 - 0.35); roll.castShadow = true; gg.add(roll);              // the door, rolled up
    sph(0.1, emis(0xfff1d6, 0xffd9a0, 2), 0, G.h - 0.12, G.d/2 + 0.2, gg);
    var gl = c.glow(0xffd9a0, 2.2, 0.7); gl.position.set(0, G.h - 0.2, G.d/2 + 0.45); gg.add(gl); lampGlows.push(gl);
    var inside = c.glow(0xfff1d6, 3.0, 0.35); inside.position.set(0, G.h - 0.4, 0); gg.add(inside); lampGlows.push(inside);
    // the apron from the door to the road
    var apron = new THREE.Mesh(new THREE.PlaneGeometry(G.w - 0.4, 11 - (G.z + G.d/2)), mat(0x323a43, {roughness: 0.9}));
    apron.rotation.x = -PI/2; apron.position.set(G.x, 0.03, (G.z + G.d/2 + 11)/2); apron.receiveShadow = true; g.add(apron);
    var cr = Math.cos(L.rot), sr = Math.sin(L.rot);                               // the lot's frame -> the street's
    function street(p){ return {x: L.x + p.x*cr + p.z*sr, z: L.z - p.x*sr + p.z*cr}; }
    // the car, parked inside, nose out: body in the HQ's accent, roof in its paint
    var Cm = c.car(col.accent, col.paint); Cm.g.position.set(K.x, 0, K.z); Cm.g.rotation.y = K.yaw; g.add(Cm.g);
    // under the roof the sun never reaches it: the garage lamp lights its paint (no extra light, so a
    // city of garages costs nothing more to draw)
    function lamplit(gr){ gr.traverse(function(o){ var m = o.material; if(m && m.color && m.emissive && !(m.emissiveIntensity > 0.5)){
      m.emissive.copy(m.color); m.emissiveIntensity = 0.28; } }); }
    lamplit(Cm.g);
    var kp = street(K); L.car = {g: Cm.g};
    bikes.push({g: Cm.g, x: kp.x, z: kp.z, yaw: K.yaw + L.rot, color: col.accent, roof: col.paint, kind: "car"});
    // its motorbike and its bicycle beside the car, in the HQ's accent
    var Mo = X.moto, Mm = c.moto(col.accent); Mm.g.position.set(Mo.x, 0, Mo.z); Mm.g.rotation.y = Mo.yaw; g.add(Mm.g);
    var mp = street(Mo);
    lamplit(Mm.g); bikes.push({g: Mm.g, x: mp.x, z: mp.z, yaw: Mo.yaw + L.rot, color: col.accent, kind: "moto"});
    var Bm = c.bike(col.accent); Bm.g.position.set(Bk.x, 0, Bk.z); Bm.g.rotation.y = Bk.yaw; g.add(Bm.g);
    var bp = street(Bk); lamplit(Bm.g);
    bikes.push({g: Bm.g, x: bp.x, z: bp.z, yaw: Bk.yaw + L.rot, color: col.accent});
  }
  // one tab's building (games/hqbase.js): its door opens that tab
  function viewBuilding(L){
    var V = HQV.hqViewBuilding(c, L.v, {doorFront: function(){ return new THREE.Vector3(L.door.x, 0, L.door.z); }});
    V.g.position.set(L.x, 0, L.z); V.g.rotation.y = L.rot; streetG.add(V.g); V.setNight(night);
    V.hits.forEach(function(o){ hits.push(o); }); V.doors.forEach(function(d){ doors.push(d); });
    L.B = V; L.H = V.top() + 2; L.fade = {g: V.g, on: false}; viewsB.push(V);
  }

  function build(){
    var list = (api.city && api.city()) || [];
    var key = JSON.stringify(list.map(function(h){ return [h.userId, h.level, h.look, h.cos, h.crew, !!h.isYou]; }));
    if(key === sig) return; sig = key;
    while(streetG.children.length) streetG.remove(streetG.children[0]);
    hits.length = 0; doors.length = 0; lampGlows.length = 0; cars.length = 0; lotsB.length = 0; viewsB.length = 0;
    dyn = {facMats: [], glows: [], beacons: [], flags: [], leds: []};
    city = layout(list);
    var W = city.W, D = city.D, PX = BLOCK_W + ROAD, PZ = BLOCK_D + ROAD, P = city.plaza;
    var OX = W/2 + 22, OZ = D/2 + 22;                                   // the edge of the world, past the trees
    var gm = new THREE.MeshStandardMaterial({map: grassT.t, roughness: 0.95}); grassT.t.repeat.set(OX/6, OZ/6);
    floor = flat(new THREE.PlaneGeometry(2*OX, 2*OZ), gm, 0);
    // the roads: one along every row and column edge of the blocks, kerbs and a dashed middle line
    var roadM = mat(0x2a2f36, {roughness: 0.9}), kerbM = mat(0x8e9aa6), dashes = [];
    var xs = [], zs = [];
    for(var ri = 0; ri <= city.cols; ri++) xs.push(-W/2 + ri*PX);
    for(var rj = 0; rj <= city.rows; rj++) zs.push(-D/2 + rj*PZ);
    zs.forEach(function(z){ flat(new THREE.PlaneGeometry(W + ROAD, ROAD), roadM, 0.03).position.z = z;
      for(var x = -W/2; x < W/2; x += 2.6) if(!xs.some(function(x2){ return Math.abs(x - x2) < ROAD/2 + 0.6; })) dashes.push([x, z, 0]); });
    xs.forEach(function(x){ flat(new THREE.PlaneGeometry(ROAD, D + ROAD), roadM, 0.031).position.x = x;
      for(var z = -D/2; z < D/2; z += 2.6) if(!zs.some(function(z2){ return Math.abs(z - z2) < ROAD/2 + 0.6; })) dashes.push([x, z, PI/2]); });
    var dash = new THREE.InstancedMesh(new THREE.BoxGeometry(1.2, 0.02, 0.14), new THREE.MeshBasicMaterial({color: 0xffc86e, toneMapped: false}), Math.max(1, dashes.length));
    var m4 = new THREE.Matrix4(), q = new THREE.Quaternion(), up = new THREE.Vector3(0, 1, 0);
    dashes.forEach(function(d, n){ q.setFromAxisAngle(up, d[2]); m4.compose(new THREE.Vector3(d[0], 0.05, d[1]), q, new THREE.Vector3(1, 1, 1)); dash.setMatrixAt(n, m4); });
    dash.count = dashes.length; streetG.add(dash);
    // the blocks: a paved pavement round a lawn; the plaza block is paved all over
    paveT.t.repeat.set(BLOCK_W/5, BLOCK_D/5);
    var paveM = new THREE.MeshStandardMaterial({map: paveT.t, roughness: 0.9});
    for(var bi = 0; bi < city.cols; bi++) for(var bj = 0; bj < city.rows; bj++){
      var bx = -W/2 + (bi + 0.5)*PX, bz = -D/2 + (bj + 0.5)*PZ;
      var pv = flat(new THREE.PlaneGeometry(BLOCK_W, BLOCK_D), paveM, 0.02); pv.position.x = bx; pv.position.z = bz;
      var kb = flat(new THREE.PlaneGeometry(BLOCK_W + 0.3, BLOCK_D + 0.3), kerbM, 0.018); kb.position.x = bx; kb.position.z = bz;
      if(bx === P.x && bz === P.z) continue;
      var lawn = flat(new THREE.PlaneGeometry(BLOCK_W - 4, BLOCK_D - 4), gm, 0.022); lawn.position.x = bx; lawn.position.z = bz;
    }
    // the plaza on the middle block: a fountain, the bike park beside it, and the city's name
    fount = fountain(); fount.g.position.set(P.x, 0, P.z); streetG.add(fount.g);
    bikes = bikePark(P.x + 8.6, P.z);
    var nb = city.lots.length;
    var cityL = c.label("Arena City · " + nb + " HQ" + (nb === 1 ? "" : "s"), HEX.amber, 1.3); cityL.position.set(P.x, 6.4, P.z); streetG.add(cityL);
    function tree(x, z, sc){ var tg = new THREE.Group(); tg.position.set(x, 0, z); tg.scale.setScalar(sc); streetG.add(tg);
      cyl(0.2, 0.28, 1.2, mat(0x5a3d2a), 0, 0.6, 0, tg, 8);
      for(var cn = 0; cn < 3; cn++) c.add(new THREE.Mesh(new THREE.ConeGeometry(1.3 - cn*0.3, 1.6, 8), mat([0x2f6b45, 0x2a5f3d, 0x357a4f][cn])), 0, 1.6 + cn*0.9, 0, tg); }
    [[-1, -1], [1, -1], [-1, 1], [1, 1]].forEach(function(k){ var pt = new THREE.Group(); pt.position.set(P.x + k[0]*(BLOCK_W/2 - 3), 0, P.z + k[1]*(BLOCK_D/2 - 3)); streetG.add(pt);
      cyl(0.2, 0.28, 1.2, mat(0x5a3d2a), 0, 0.6, 0, pt, 8); sph(1.2, mat(0x3c8a4f, {roughness: 0.9}), 0, 2.1, 0, pt); });
    // street lamps on the corner of every crossing
    function lamp(x, z){ cyl(0.07, 0.1, 3.4, mat(0x2b3a48, {metalness: 0.6}), x, 1.7, z, streetG, 8); sph(0.16, emis(0xfff1d6, 0xffd9a0, 2), x, 3.3, z, streetG);
      var gl = c.glow(0xffd9a0, 2.2, 0.8); gl.position.set(x, 3.25, z); streetG.add(gl); lampGlows.push(gl); }
    xs.forEach(function(x){ zs.forEach(function(z){ lamp(x + ROAD/2 + 0.8, z + ROAD/2 + 0.8); }); });
    // trees round the edge of the city
    var tr = c.rng(77), nt = Math.min(160, Math.round((W + D)/3));
    for(var t = 0; t < nt; t++){
      var side = tr()*4 | 0, u = tr()*2 - 1, out = ROAD/2 + 4 + tr()*12;
      var tx = side < 2 ? u*(W/2 + 10) : (side === 2 ? -1 : 1)*(W/2 + out), tz = side < 2 ? (side === 0 ? -1 : 1)*(D/2 + out) : u*(D/2 + 10);
      tree(tx, tz, 0.8 + tr()*0.6);
    }
    // cars round the road on the edge of the city: [colour, speed (m/s; the sign is the way round)]
    [[0xd14a5c, 6], [0x4a7bd1, -5], [0xe0e4e8, 5.5], [0xffb347, -4.5], [0x3fa66a, 5], [0x8a4ad1, -6]].forEach(function(cd, n){
      var k = new THREE.Group(), bodyM = mat(cd[0], {metalness: 0.6, roughness: 0.3});
      box(1.9, 0.45, 0.95, bodyM, 0, 0.42, 0, k); box(1.0, 0.42, 0.85, mat(0x0b1622, {metalness: 0.6, roughness: 0.15}), -0.1, 0.82, 0, k);
      var lane = cd[1] > 0 ? -1.4 : 1.4, ax = W/2 + lane, az = D/2 + lane;
      streetG.add(k); cars.push({g: k, s: n/6*2*(2*ax + 2*az), v: cd[1], ax: ax, az: az});
    });
    city.lots.forEach(building);
    city.views.forEach(viewBuilding);
    w.floor = floor; w.bounds = [-OX + 2, OX - 2, -OZ + 2, OZ - 2];
    // arriving: at your own door, or the door of the HQ you just visited
    var mine = city.lots.filter(function(L){ return L.h.isYou; })[0];
    var back = api.cityReturn && api.cityReturn(), ret = city.lots.filter(function(L){ return L.h.userId === back; })[0];
    // the camera stands on the street side, looking at the building's front (a little off-axis)
    function at(L){ return L ? {x: L.door.x + L.dx*2.6, z: L.door.z + L.dz*2.6, yaw: Math.atan2(L.dx, L.dz), camYaw: L.rot + 0.55} : {x: P.x, z: P.z + 7, yaw: 0}; }
    w.spawn = at(ret || mine); w.spawnFrom = {lobby: at(mine)};
    applyTime();
  }

  /* ---------- the fountain: a stone basin, two bowls, falling water, a jet on top ---------- */
  var FOUNT_R = 4.4;
  function lathe(pts, m, y, p){
    var o = new THREE.Mesh(new THREE.LatheGeometry(pts.map(function(q){ return new THREE.Vector2(q[0], q[1]); }), 48), m);
    o.position.y = y || 0; o.castShadow = true; o.receiveShadow = true; p.add(o); return o;
  }
  function fountain(){
    var g = new THREE.Group(), stone = mat(0xb9c2cb, {roughness: 0.7}), stone2 = mat(0x8f9aa6, {roughness: 0.75});
    // the basin: a low stone wall with a rim to sit on, and a water surface inside
    lathe([[3.9, 0], [4.4, 0], [4.45, 0.62], [4.3, 0.74], [4.0, 0.74], [3.92, 0.62], [3.9, 0.12], [0, 0.12]], stone, 0, g);
    var rip = c.canvasTex(256, 256, function(gx){
      gx.fillStyle = "#1d6f93"; gx.fillRect(0, 0, 256, 256);
      var R = c.rng(31); for(var i = 0; i < 70; i++){ gx.strokeStyle = "rgba(170,230,255," + (0.08 + R()*0.2) + ")"; gx.lineWidth = 1 + R()*2;
        gx.beginPath(); gx.ellipse(R()*256, R()*256, 8 + R()*30, 3 + R()*8, 0, 0, 7); gx.stroke(); }
    });
    rip.t.wrapS = rip.t.wrapT = THREE.RepeatWrapping; rip.t.repeat.set(3, 3);
    var waterM = new THREE.MeshStandardMaterial({map: rip.t, color: 0x7fd0f0, emissive: new THREE.Color(COL.cyan), emissiveIntensity: 0.28,
      transparent: true, opacity: 0.88, roughness: 0.15, metalness: 0.1});
    var pool = new THREE.Mesh(new THREE.CircleGeometry(3.9, 48), waterM); pool.rotation.x = -PI/2; pool.position.y = 0.52; g.add(pool);
    // the column and two bowls, each spilling a sheet of water into the one below
    lathe([[0.85, 0], [0.7, 0.15], [0.5, 0.3], [0.45, 1.2], [0.6, 1.45], [0, 1.45]], stone2, 0.1, g);
    lathe([[0.35, 0], [1.9, 0.2], [2.05, 0.32], [1.95, 0.38], [1.75, 0.3], [0.3, 0.18]], stone, 1.48, g);
    var bowl1 = new THREE.Mesh(new THREE.CircleGeometry(1.78, 40), waterM); bowl1.rotation.x = -PI/2; bowl1.position.y = 1.83; g.add(bowl1);
    lathe([[0.42, 0], [0.3, 0.2], [0.24, 0.85], [0.36, 1.0], [0, 1.0]], stone2, 1.75, g);
    lathe([[0.2, 0], [1.0, 0.12], [1.1, 0.24], [1.02, 0.29], [0.9, 0.22], [0.18, 0.12]], stone, 2.72, g);
    var bowl2 = new THREE.Mesh(new THREE.CircleGeometry(0.93, 32), waterM); bowl2.rotation.x = -PI/2; bowl2.position.y = 2.98; g.add(bowl2);
    lathe([[0.16, 0], [0.12, 0.25], [0.2, 0.4], [0.07, 0.62], [0, 0.66]], stone2, 2.95, g);
    var sheetT = c.canvasTex(64, 256, function(gx){
      gx.clearRect(0, 0, 64, 256); var R = c.rng(57);
      for(var i = 0; i < 26; i++){ var x = R()*64, h = 30 + R()*120, y = R()*256; gx.fillStyle = "rgba(200,240,255," + (0.25 + R()*0.45) + ")"; gx.fillRect(x, y, 1 + R()*2.5, h); gx.fillRect(x, y - 256, 1 + R()*2.5, h); }
    });
    sheetT.t.wrapS = sheetT.t.wrapT = THREE.RepeatWrapping; sheetT.t.repeat.set(14, 1);
    var sheetM = new THREE.MeshBasicMaterial({map: sheetT.t, transparent: true, opacity: 0.75, depthWrite: false, side: THREE.DoubleSide, toneMapped: false});
    var sheet1 = new THREE.Mesh(new THREE.CylinderGeometry(2.04, 2.22, 1.32, 48, 1, true), sheetM); sheet1.position.y = 1.18; g.add(sheet1);
    var sheet2 = new THREE.Mesh(new THREE.CylinderGeometry(1.1, 1.24, 0.96, 40, 1, true), sheetM); sheet2.position.y = 2.53; g.add(sheet2);
    // the jet: drops thrown up from the top that arc down into the top bowl
    var NJ = 160, jp = new Float32Array(NJ*3), jr = c.rng(71), jets = [];
    for(var i = 0; i < NJ; i++) jets.push({a: jr()*PI*2, v: 0.55 + jr()*0.45, o: jr()});
    var jg = new THREE.BufferGeometry(); jg.setAttribute("position", new THREE.BufferAttribute(jp, 3));
    var jet = new THREE.Points(jg, new THREE.PointsMaterial({color: 0xcff2ff, size: 0.09, transparent: true, opacity: 0.9, depthWrite: false, toneMapped: false}));
    g.add(jet);
    var glow = c.glow(COL.cyan, 7, 0.25); glow.position.y = 1.2; g.add(glow);
    return {g: g, rip: rip.t, sheet: sheetT.t, jet: jet, jp: jp, jets: jets, glow: glow, waterM: waterM};
  }
  function fountainStep(t){
    var F = fount; if(!F) return;
    var calm = c.E.calm(), tt = calm ? 0 : t;        // Calm / reduced motion: still water
    F.rip.offset.set(tt*0.02, tt*0.035);
    F.sheet.offset.y = tt*0.9;
    F.jet.visible = !calm;
    if(calm) return;
    var p = F.jp;
    for(var i = 0; i < F.jets.length; i++){
      var j = F.jets[i], u = (t*0.8 + j.o) % 1, r = u*0.82*j.v, y = 3.6 + u*1.5*j.v - u*u*(1.5*j.v + 0.55);
      p[i*3] = Math.cos(j.a)*r; p[i*3 + 1] = y; p[i*3 + 2] = Math.sin(j.a)*r;
    }
    F.jet.geometry.attributes.position.needsUpdate = true;
  }

  /* ---------- the bike park: a rack of bikes beside the fountain ---------- */
  // Walk up to one and press E (hq3d.js does the riding). w.bikes lists them: {g, x, z, yaw, color}.
  function bikePark(cx, cz){
    var list = [], g = new THREE.Group(); g.position.set(cx, 0, cz); streetG.add(g);
    var padT = c.canvasTex(128, 256, function(gx){ gx.fillStyle = "#4a5866"; gx.fillRect(0, 0, 128, 256); gx.strokeStyle = "rgba(255,255,255,.35)"; gx.lineWidth = 3;
      for(var y = 20; y < 256; y += 39){ gx.beginPath(); gx.moveTo(14, y); gx.lineTo(114, y); gx.stroke(); } });
    var pad = new THREE.Mesh(new THREE.PlaneGeometry(3.6, 9.6), new THREE.MeshStandardMaterial({map: padT.t, roughness: 0.85}));
    pad.rotation.x = -PI/2; pad.position.y = 0.035; pad.receiveShadow = true; g.add(pad);
    var steel = mat(0x9aa7b5, {metalness: 0.8, roughness: 0.3});
    // the rack: a rail with a hoop per slot, front wheels go in on the fountain side
    box(0.06, 0.06, 9, steel, -0.7, 0.42, 0, g);
    var cols = [0xff6b5b, 0x5fd3e6, 0xffb347, 0x6fd38a, 0x9b8cf0, 0xe0559b];
    for(var i = 0; i < 6; i++){
      var z = -3.75 + i*1.5;
      var hoop = new THREE.Mesh(new THREE.TorusGeometry(0.34, 0.03, 6, 20, PI), steel); hoop.position.set(-0.7, 0.05, z); hoop.rotation.y = PI/2; hoop.castShadow = true; g.add(hoop);
      var B = c.bike(cols[i]); B.g.position.set(0, 0, z); B.g.rotation.y = -PI/2; g.add(B.g);   // facing the fountain
      list.push({g: B.g, x: cx, z: cz + z, yaw: -PI/2, color: cols[i]});
    }
    var sign = c.label("\uD83D\uDEB2 Bike park · press E", HEX.green, 0.7); sign.position.set(0.9, 2.6, 0); g.add(sign);
    return list;
  }

  function applyTime(){
    lotsB.concat(viewsB).forEach(function(B){ B.setNight(night); });
    lampGlows.forEach(function(g){ g.material.opacity = night ? 0.85 : 0.08; });
  }
  function blocked(x, z){
    if(Math.abs(x) > city.W/2 + 18 || Math.abs(z) > city.D/2 + 18) return true;
    if(Math.hypot(x - city.plaza.x, z - city.plaza.z) < FOUNT_R + 0.35) return true;   // the fountain
    var all = city.lots.concat(city.views);
    for(var i = 0; i < all.length; i++){
      var L = all[i], rx = x - L.x, rz = z - L.z, c0 = Math.cos(-L.rot), s0 = Math.sin(-L.rot);
      var lx = rx*c0 + rz*s0, lz = -rx*s0 + rz*c0;              // into the lot's own frame
      if(L.B && L.B.blockedLocal(lx, lz)) return true;
      if(L.h && lotExtraBlocked(lx, lz, !L.car || L.car.g.visible)) return true;   // its garage, and its car unless you drive it
    }
    return false;
  }

  build();
  w.hits = hits; w.doors = doors; w.span = 18; w.zoom = 0.8; w.camY = 2.5; w.follow = 1; w.camPitch = 0.72;
  w.blocked = blocked;
  Object.defineProperty(w, "bikes", {get: function(){ return bikes; }, configurable: true});
  w.onCity = function(){ build(); };
  w.setTime = function(n){ night = n; S.background = n ? skyBg.night : skyBg.day; S.fog = new THREE.Fog(n ? 0x0f1f30 : 0xb8dcf2, 90, 220);
    hemi.intensity = n ? 1.3 : 2.4; sun.intensity = n ? 0.7 : 2.4; sun.color.set(n ? 0xa9b8ff : 0xfff2dc);
    var sc = sun.shadow.camera, R = Math.max(city.W, city.D)/2 + 30; sc.left = -R; sc.right = R; sc.top = R; sc.bottom = -R; sc.far = 260; sc.updateProjectionMatrix(); applyTime(); };
  w.onLook = function(){ sig = null; build(); };
  // A building between you and the camera fades (games/hqbase.js), so you are never lost behind a tower.
  var fadeAcc = 0;
  function cutaway(){ HQV.hqCutaway(c, city.lots.concat(city.views)); }
  // a point s metres round the rectangle with half-sides ax, az (clockwise seen from above), and its heading
  function rectAt(s, ax, az){
    var per = 4*(ax + az); s = ((s % per) + per) % per;
    if(s < 2*ax) return {x: -ax + s, z: -az, dx: 1, dz: 0}; s -= 2*ax;
    if(s < 2*az) return {x: ax, z: -az + s, dx: 0, dz: 1}; s -= 2*az;
    if(s < 2*ax) return {x: ax - s, z: az, dx: -1, dz: 0}; s -= 2*ax;
    return {x: -ax, z: az - s, dx: 0, dz: -1};
  }
  w.update = function(t, dt, k){
    fadeAcc += dt; if(fadeAcc > 0.15){ fadeAcc = 0; cutaway(); }
    lotsB.concat(viewsB).forEach(function(B){ B.animate(t, dt, k); });
    fountainStep(t);
    cars.forEach(function(cr){ cr.s += cr.v*dt*k; var p = rectAt(cr.s, cr.ax, cr.az), sg = cr.v < 0 ? -1 : 1;
      cr.g.position.set(p.x, 0, p.z); cr.g.rotation.y = Math.atan2(-p.dz*sg, p.dx*sg); });
  };
  return w;
};
})();
