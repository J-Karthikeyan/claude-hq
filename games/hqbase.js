/* Claude HQ 2.0: the Base, your HQ from outside.
 *
 * A campus that grows with your HQ level: the tower gains floors, and wings, a
 * fountain, streak banners, a helipad, a sky terrace, skybridges, a kart track, an
 * observatory dome, a crew billboard, a hall-of-fame statue and an Arena annex unlock
 * as you level. Lit windows follow how many sessions are live. Cars circle the ring
 * road, people stroll the plaza, day and night follow your clock. Walk to the front
 * door to go into the Lobby.
 *
 * A floor of the 3D HQ: registered as HQV.hqWorlds.base and built by games/hq3d.js
 * with its ctx helpers. The building's paint and sign come from ctx.look() (HQ
 * customisation); the level from ctx.level() (the season level the page shows).
 */
(function(){
"use strict";
var HQV = window.HQV = window.HQV || {};
HQV.hqWorlds = HQV.hqWorlds || {};

var UNLOCKS = [[1, "Main tower and lobby"], [5, "Plaza fountain"], [10, "West wing"], [15, "Streak banners"], [20, "Helipad and helicopter"],
  [25, "Sky terrace garden"], [30, "East wing and skybridges"], [35, "Kart track"], [40, "Observatory dome"], [45, "Crew billboard"],
  [50, "Hall of fame statue"], [55, "Arena annex"]];
HQV.hqUnlocks = UNLOCKS;

/* ---------- the building ----------
 * The tower, lobby, wings, helipad, sky terrace, skybridges, dome, banners (the HQ's name),
 * billboard and decor: shared by your Base and every lot in Arena City (games/hqcity.js), so
 * an HQ looks the same wherever you see it. opts: level(), look(), live() (crew at work, for
 * the billboard), campus (the plaza extras only the Base has room for: fountain, statue, kart
 * track, Arena annex), door (where the front door goes) and doorFront() (its point, in the
 * world). It fills its own hits/doors; animate() and applyTime() keep it alive. */
function makeBuilding(c, opts){
  var THREE = c.THREE, PI = c.PI, COL = c.COL, HEX = c.HEX;
  var mat = c.mat, emis = c.emis, box = c.box, cyl = c.cyl, sph = c.sph;
  var hits = [], doors = [], night = true, twinkle = 0, liveSeen = -1;
  var B = {hits: hits, doors: doors, wings: 0, level: 1};
  /* ---------- the building, rebuilt when your level or look changes ---------- */
  var baseG = new THREE.Group();
  var dyn = null, built = {level: null, look: ""}, litShare = 0.5;
  // a dim copy of the paint, so the building keeps its colour at night (the emissive map's background)
  function dimPaint(hex, k){ var n = parseInt(hex.slice(1), 16); return "rgb(" + [n >> 16 & 255, n >> 8 & 255, n & 255].map(function(v){ return Math.round(v*k); }).join(",") + ")"; }
  function paintWin(e, wn){
    e.fillStyle = "#000"; e.fillRect(wn.x, wn.y, wn.w, wn.h);
    if(!wn.lit) return;
    e.fillStyle = wn.col; e.fillRect(wn.x, wn.y, wn.w, wn.h);
    e.fillStyle = "rgba(0,0,0,.55)"; e.fillRect(wn.x + 2, wn.y + wn.h - 7, wn.w - 4, 3);
    if(wn.who){ var px = wn.x + wn.who; e.beginPath(); e.arc(px + 3, wn.y + wn.h - 15, 3, 0, 7); e.fill(); e.fillRect(px, wn.y + wn.h - 12, 6, 6); }
    e.fillStyle = "rgba(0,0,0,.6)"; e.fillRect(wn.x + wn.w/2 - 1, wn.y, 2, wn.h);
  }
  function facade(cols, floors, seed, paint){
    var cw = 40, fh = 48, Wd = cols*cw, H = floors*fh, R = c.rng(seed);
    var map = c.canvasTex(Wd, H), emi = c.canvasTex(Wd, H), g = map.g, wins = [];
    g.fillStyle = paint; g.fillRect(0, 0, Wd, H);
    emi.g.fillStyle = dimPaint(paint, 0.7); emi.g.fillRect(0, 0, Wd, H);
    for(var f = 0; f < floors; f++){
      var y0 = f*fh; g.fillStyle = "rgba(255,255,255,.12)"; g.fillRect(0, y0, Wd, 9);
      for(var k = 0; k < cols; k++){
        var x = k*cw + 5, y = y0 + 13, ww = cw - 10, hh = fh - 18;
        var gr = g.createLinearGradient(x, y, x + ww, y + hh); gr.addColorStop(0, "#24486a"); gr.addColorStop(1, "#0e1c2a"); g.fillStyle = gr; g.fillRect(x, y, ww, hh);
        g.fillStyle = "rgba(255,255,255,.09)"; g.fillRect(x, y, ww*0.35, hh);
        var wn = {x: x, y: y, w: ww, h: hh, col: R() < 0.82 ? "#ffcf7a" : "#9fe3f0", r: R(), who: R() < 0.5 ? 4 + R()*(ww - 12) : 0};
        wn.lit = wn.r < litShare; wins.push(wn); paintWin(emi.g, wn);
      }
    }
    map.t.needsUpdate = emi.t.needsUpdate = true;
    return {map: map, emi: emi, wins: wins};
  }
  function tower(x, y0, z, wd, d, floors, seed, paint){
    var cols = Math.max(2, Math.round(wd/1.15)), colsD = Math.max(2, Math.round(d/1.15));
    var Ff = facade(cols, floors, seed, paint), Fs = facade(colsD, floors, seed + 7, paint), Fb = facade(cols, floors, seed + 13, paint);
    dyn.facades.push(Ff, Fs, Fb);
    function fm(F){ var m = new THREE.MeshStandardMaterial({map: F.map.t, emissiveMap: F.emi.t, emissive: new THREE.Color(0xffffff), emissiveIntensity: 1, roughness: 0.35, metalness: 0.35}); dyn.facMats.push(m); return m; }
    var mF = fm(Ff), mS = fm(Fs), mB = fm(Fb), roof = mat(0x2b3a48, {roughness: 0.8});
    var H = floors*2.6, body = new THREE.Mesh(new THREE.BoxGeometry(wd, H, d), [mS, mS, roof, roof, mF, mB]); c.add(body, x, y0 + H/2, z, baseG);
    // the floor lines carry the building's light colour (HQ customisation), so each HQ reads from across the street
    var ledge = emis(0x2b3a48, dyn.accent, 0.5, {metalness: 0.4, roughness: 0.5}); dyn.ledges.push(ledge);
    for(var f = 1; f <= floors; f++) box(wd + 0.24, 0.12, d + 0.24, ledge, x, y0 + f*2.6 - 0.06, z, baseG, true);
    [[-1, -1], [1, -1], [-1, 1], [1, 1]].forEach(function(q){ box(0.24, H, 0.24, mat(0x5a7590, {metalness: 0.5}), x + q[0]*wd/2, y0 + H/2, z + q[1]*d/2, baseG); });
    var top = y0 + H, led = emis(0x0d1a24, dyn.accent, 2);
    box(wd + 0.3, 0.5, 0.14, roof, x, top + 0.25, z + d/2, baseG); box(wd + 0.3, 0.5, 0.14, roof, x, top + 0.25, z - d/2, baseG);
    box(0.14, 0.5, d + 0.3, roof, x - wd/2, top + 0.25, z, baseG); box(0.14, 0.5, d + 0.3, roof, x + wd/2, top + 0.25, z, baseG);
    box(wd + 0.34, 0.07, 0.07, led, x, top + 0.52, z + d/2 + 0.02, baseG, true); box(0.07, 0.07, d + 0.34, led, x + wd/2 + 0.02, top + 0.52, z, baseG, true);
    dyn.leds.push(led);
    return top;
  }
  function lobbyBlock(x, z, wd, d){
    var gl = new THREE.MeshStandardMaterial({color: 0x0b1622, emissive: new THREE.Color(dyn.accent).lerp(new THREE.Color(0xffb866), 0.35), emissiveIntensity: 0.3, transparent: true, opacity: 0.9, roughness: 0.1, metalness: 0.3});
    dyn.lobbyMats.push(gl);
    box(wd - 0.4, 3.2, d - 0.4, gl, x, 1.6, z, baseG);
    for(var m = -wd/2 + 0.2; m <= wd/2 - 0.2 + 0.01; m += 1.2) box(0.1, 3.2, 0.1, mat(0x5a7590, {metalness: 0.6}), x + m, 1.6, z + d/2 - 0.18, baseG);
    box(wd + 0.4, 0.3, d + 0.4, mat(0x4d6680), x, 3.35, z, baseG);
    return 3.5;
  }
  function beacon(x, y, z){ var b = sph(0.22, emis(COL.coral, COL.coral, 3), x, y, z, baseG); dyn.beacons.push(b); var g = c.glow(COL.coral, 1.4, 0.9); g.position.copy(b.position); baseG.add(g); dyn.beaconGlows.push(g); }
  function build(){
    var level = Math.max(1, opts.level() | 0), look = opts.look() || {};
    var paint = /^#[0-9a-f]{6}$/i.test(look.paint || "") ? look.paint : "#2a3c50";
    var accent = /^#[0-9a-f]{6}$/i.test(look.accent || "") ? parseInt(look.accent.slice(1), 16) : COL.amber;
    while(baseG.children.length) baseG.remove(baseG.children[0]);
    hits.length = 0; doors.length = 0;
    dyn = {facades: [], facMats: [], leds: [], lobbyMats: [], beacons: [], beaconGlows: [], banners: [], heli: null, jets: [], karts: [], glows: [], accent: accent,
           flags: [], fireworks: [], ledges: [], prestige: null};
    var floorsLow = Math.min(8, 1 + Math.floor(level/5)), floorsHigh = level >= 25 ? Math.min(8, 1 + Math.floor((level - 25)/4)) : 0;
    var y = lobbyBlock(0, 0, 9, 7);
    var topLow = tower(0, y, 0, 9, 7, floorsLow, 100 + level % 7, paint), top = topLow;
    if(floorsHigh){ top = tower(0, topLow + 0.5, -0.5, 6, 4.6, floorsHigh, 200 + level % 5, paint); box(9.2, 0.5, 7.2, mat(0x2b3a48), 0, topLow + 0.25, 0, baseG); }
    // HQ 2.5 prestige: one gold-glass floor per star (up to 5) on the roof, and a beacon.
    // Only your OWN building passes opts.prestige; city lots and visits never do.
    var stars = opts.prestige ? Math.max(0, Math.min(5, opts.prestige() | 0)) : 0;
    if(stars){
      var pw = floorsHigh ? 5.4 : 7.8, pd = floorsHigh ? 4.0 : 6.0, pz = floorsHigh ? -0.5 : 0;
      var pm = mat(0xf5d76e, {metalness: 0.6, roughness: 0.25, emissive: new THREE.Color(0xf5d76e), emissiveIntensity: 0.25});
      var pg = new THREE.MeshStandardMaterial({color: 0xbfe3f5, transparent: true, opacity: 0.55, roughness: 0.05, metalness: 0.3, emissive: new THREE.Color(0xffd36e), emissiveIntensity: 0.35});
      dyn.facMats.push(pg);
      for(var ps = 0; ps < stars; ps++){
        var py = top + 0.3 + ps*2.4;
        var pf = box(pw, 2.0, pd, pg, 0, py + 1.0, pz, baseG); pf.name = "prestige-floor";
        box(pw + 0.3, 0.3, pd + 0.3, pm, 0, py + 2.15, pz, baseG);
      }
      top += 0.3 + stars*2.4 + 0.3;
      var bm = new THREE.MeshBasicMaterial({color: 0xffe08a, transparent: true, opacity: 0.35, toneMapped: false, depthWrite: false});
      var beam = new THREE.Mesh(new THREE.CylinderGeometry(0.25, 0.9, 16, 16, 1, true), bm); beam.name = "prestige-beacon";
      beam.position.set(0, top + 9, pz); baseG.add(beam);
      var bglow = c.glow(0xffd36e, 5, 0.6); bglow.position.set(0, top + 1, pz); baseG.add(bglow);
      dyn.prestige = {beam: beam, mat: bm, glow: bglow, stars: stars};
    }
    built.stars = stars;
    // the front door: canopy, sign, revolving door; walk up to it to go in
    box(5.6, 0.28, 2.6, mat(0x2b3a48, {metalness: 0.5}), 0, 3.15, 4.6, baseG); [-2.5, 2.5].forEach(function(x){ box(0.2, 3.1, 0.2, mat(0x9aa7b5, {metalness: 0.7}), x, 1.55, 5.7, baseG); });
    var cs = c.canvasTex(512, 64, function(g){ g.fillStyle = "#0b1622"; g.fillRect(0, 0, 512, 64); g.fillStyle = "#" + accent.toString(16).padStart(6, "0"); g.font = "700 44px system-ui, sans-serif"; g.textAlign = "center"; g.fillText(c.short((look.sign || "CLAUDE HQ").toUpperCase(), 16), 256, 48); });
    var sgn = c.screen(5.4, 0.5, cs.t); sgn.position.set(0, 3.15, 5.92); baseG.add(sgn);
    var cg = c.glow(accent, 4, 0.35); cg.position.set(0, 3.1, 6.2); baseG.add(cg); dyn.glows.push(cg);
    var revolve = cyl(0.9, 0.9, 2.6, new THREE.MeshStandardMaterial({color: 0xbfe3f5, transparent: true, opacity: 0.3, roughness: 0.05, emissive: new THREE.Color(0xffb866), emissiveIntensity: 0.2}), 0, 1.3, 3.5, baseG, 20);
    var door = Object.assign({view: "@lobby", label: "Front door", tip: "Front door: into the Lobby"}, opts.door || {},
                             {front: opts.doorFront ? opts.doorFront() : new THREE.Vector3(0, 0, 4.8)});   // under the canopy
    [revolve, sgn].forEach(function(o){ o.userData.door = door; hits.push(o); }); doors.push(door);
    var lt = c.label("Front door", HEX.green, 0.5); lt.position.set(0, 4.3, 6.1); baseG.add(lt);
    // the roof
    if(level >= 40){
      var dome = new THREE.Mesh(new THREE.SphereGeometry(2.1, 28, 14, 0, PI*2, 0, PI/2), new THREE.MeshStandardMaterial({color: 0xbfe3f5, transparent: true, opacity: 0.4, roughness: 0.05, metalness: 0.3, emissive: new THREE.Color(COL.cyan), emissiveIntensity: 0.35}));
      c.add(dome, 0, top + 0.1, -0.5, baseG); cyl(2.2, 2.2, 0.3, mat(0x4d6680), 0, top + 0.15, -0.5, baseG, 28);
      box(0.12, 5, 0.12, mat(0x9aa7b5), 2.2, top + 2.5, -2, baseG); beacon(2.2, top + 5.1, -2);
    } else {
      var rx = 0, rz = floorsHigh ? -0.5 : 0, rw = floorsHigh ? 6 : 9, rd = floorsHigh ? 4.6 : 7;
      [[-rw*0.25, -rd*0.2], [rw*0.05, -rd*0.25], [rw*0.28, rd*0.15]].forEach(function(q){ box(1.1, 0.7, 0.9, mat(0x8e9aa6, {metalness: 0.5}), rx + q[0], top + 0.35, rz + q[1], baseG); });
      cyl(0.7, 0.7, 1.2, mat(0x8a5a35, {roughness: 0.8}), rx - rw*0.28, top + 1.6, rz + rd*0.22, baseG, 16);
      box(0.14, 5, 0.14, mat(0x9aa7b5, {metalness: 0.6}), rx + rw*0.32, top + 2.5, rz - rd*0.3, baseG); beacon(rx + rw*0.32, top + 5.1, rz - rd*0.3);
    }
    if(level >= 25){
      var ty = topLow + 0.5;
      [[-3.8, 0, 1.2, 6.8], [3.8, 0, 1.2, 6.8], [0, 2.8, 6.2, 1.2]].forEach(function(p){ box(p[2], 0.25, p[3], mat(0x2f6b45, {roughness: 0.9}), p[0], ty + 0.12, p[1], baseG); });
      [[-3.8, -2.2], [-3.8, 1.8], [3.8, -2.2], [3.8, 1.8]].forEach(function(p){ sph(0.5, mat(0x3c8a4f, {roughness: 0.9}), p[0], ty + 1.0, p[1], baseG); });
      for(var sl = 0; sl < 10; sl++){ var gw = c.glow(0xffd9a0, 0.35, 0.9); gw.position.set(-4.2 + sl*0.93, ty + 1.6, 3.4); baseG.add(gw); dyn.glows.push(gw); }
    }
    // wings, helipad, skybridges
    var floorsW = Math.max(1, floorsLow - 2);
    [[-10.5, 10], [10.5, 30]].forEach(function(wd, n){
      if(level < wd[1]) return;
      var yy = lobbyBlock(wd[0], 0.5, 5.5, 5), wt = tower(wd[0], yy, 0.5, 5.5, 5, floorsW, 300 + n*11 + level % 3, paint);
      if(n === 0 && level >= 20){
        cyl(2.3, 2.3, 0.16, mat(0x2b3a48), wd[0], wt + 0.1, 0.5, baseG, 32);
        var hT = c.canvasTex(256, 256, function(g){ g.strokeStyle = HEX.amber; g.lineWidth = 10; g.beginPath(); g.arc(128, 128, 110, 0, 7); g.stroke(); g.fillStyle = "#e9eff3"; g.font = "700 150px system-ui, sans-serif"; g.textAlign = "center"; g.fillText("H", 128, 182); });
        var hp = new THREE.Mesh(new THREE.CircleGeometry(2.1, 32), new THREE.MeshBasicMaterial({map: hT.t, transparent: true, toneMapped: false})); hp.rotation.x = -PI/2; hp.position.set(wd[0], wt + 0.19, 0.5); baseG.add(hp);
        var heli = new THREE.Group(), red = mat(0xd14a5c, {metalness: 0.5, roughness: 0.35});
        sph(0.8, red, 0, 0.95, 0, heli).scale.set(1.5, 0.9, 0.9); box(0.12, 0.12, 2.4, red, 0, 1.1, -1.9, heli);
        var rotor = new THREE.Group(); rotor.position.y = 1.92; heli.add(rotor); box(4.2, 0.03, 0.18, mat(0x1d2128), 0, 0, 0, rotor); box(0.18, 0.03, 4.2, mat(0x1d2128), 0, 0, 0, rotor);
        heli.position.set(wd[0], wt + 0.2, 0.5); heli.rotation.y = PI/2; baseG.add(heli); dyn.heli = {g: heli, rotor: rotor, y: wt + 0.2, x: wd[0]};
      }
      if(level >= 30){
        var by = Math.max(4.8, wt - 1.3), bx = (4.5 + Math.abs(wd[0]) - 2.75)/2*Math.sign(wd[0]), bl = Math.abs(wd[0]) - 2.75 - 4.5;
        box(bl, 1.6, 2, new THREE.MeshStandardMaterial({color: 0x0b1622, emissive: new THREE.Color(COL.cyan), emissiveIntensity: 0.45, transparent: true, opacity: 0.8, roughness: 0.1}), bx, by, 0.5, baseG);
      }
    });
    if(level >= 15){
      // the HQ's name (the sign you set when you open it to visitors) on both banners, the level under it
      var accHex = "#" + accent.toString(16).padStart(6, "0");
      var bt = c.canvasTex(128, 512, function(g){ g.fillStyle = "#1a1408"; g.fillRect(0, 0, 128, 512); g.fillStyle = accHex; g.fillRect(0, 0, 128, 14); g.fillRect(0, 498, 128, 14); g.save(); g.translate(64, 256); g.rotate(-PI/2); g.textAlign = "center"; g.fillStyle = accHex;
        var nm = c.short((look.sign || "CLAUDE HQ").toUpperCase(), 16); g.font = "700 " + (nm.length > 11 ? 40 : 54) + "px system-ui, sans-serif"; g.fillText(nm, 0, 6); g.fillStyle = HEX.ink; g.font = "600 28px ui-monospace, monospace"; g.fillText("LEVEL " + level, 0, 46); g.restore(); });
      [-2.9, 2.9].forEach(function(x){ var bn = c.screen(1.3, 5.2, bt.t); bn.material.side = THREE.DoubleSide; var piv = new THREE.Group(); piv.position.set(x, topLow - 0.3, 3.75); baseG.add(piv); bn.position.y = -2.6; piv.add(bn); dyn.banners.push(piv); });
    }
    if(level >= 45 && floorsHigh){
      var live = opts.live ? opts.live() : 0;
      var bbT = c.canvasTex(512, 300, function(g){ g.fillStyle = "#06111a"; g.fillRect(0, 0, 512, 300); g.strokeStyle = HEX.cyan; g.lineWidth = 6; g.strokeRect(6, 6, 500, 288); g.fillStyle = HEX.amber; g.font = "700 64px system-ui, sans-serif"; g.fillText(live + " CREW LIVE", 30, 110); g.fillStyle = HEX.cyan; g.font = "600 36px ui-monospace, monospace"; g.fillText("HQ level " + level, 30, 200); });
      var bb = c.screen(4.2, 2.5, bbT.t); bb.position.set(3.16, top - 3.5, -0.5); bb.rotation.y = PI/2; baseG.add(bb);
    }
    if(opts.campus && level >= 5){
      var fg = new THREE.Group(); fg.position.set(0, 0, 10.4); baseG.add(fg);
      cyl(2.5, 2.6, 0.55, mat(0x9aa7b5, {roughness: 0.6}), 0, 0.27, 0, fg, 32);
      cyl(2.3, 2.3, 0.05, new THREE.MeshStandardMaterial({color: 0x2a7fa8, emissive: new THREE.Color(COL.cyan), emissiveIntensity: 0.35, transparent: true, opacity: 0.85, roughness: 0.05}), 0, 0.5, 0, fg, 32);
      cyl(0.9, 1.0, 0.5, mat(0x9aa7b5), 0, 0.75, 0, fg, 20); cyl(0.25, 0.3, 1.0, mat(0x9aa7b5), 0, 1.4, 0, fg, 12);
      for(var j = 0; j < 10; j++){ var jg = c.glow(0x9fe3f0, 0.35, 0.9); fg.add(jg); dyn.jets.push({g: jg, a: j/10*PI*2, o: (j % 5)/5}); }
    }
    if(opts.campus && level >= 50){
      var st = new THREE.Group(); st.position.set(7.4, 0, 9.6); baseG.add(st);
      box(1.8, 1.0, 1.8, mat(0x4d5866, {roughness: 0.6}), 0, 0.5, 0, st);
      var gold = mat(0xd8b34a, {metalness: 0.9, roughness: 0.25});
      sph(0.9, gold, 0, 1.85, 0, st).scale.y = 0.9; [-0.4, 0.4].forEach(function(x){ c.add(new THREE.Mesh(new THREE.ConeGeometry(0.25, 0.7, 6), gold), x, 2.75, 0, st).rotation.z = -x; });
    }
    if(opts.campus && level >= 35){
      var ktT = c.canvasTex(512, 768, function(g){ g.fillStyle = "#1f3d2c"; g.fillRect(0, 0, 512, 768); g.lineWidth = 92; g.strokeStyle = "#2a2f36"; g.beginPath(); g.ellipse(256, 384, 176, 300, 0, 0, 7); g.stroke(); g.lineWidth = 10; g.setLineDash([22, 22]); g.strokeStyle = "#d14a5c"; g.beginPath(); g.ellipse(256, 384, 222, 346, 0, 0, 7); g.stroke(); g.setLineDash([]); });
      var kt = new THREE.Mesh(new THREE.PlaneGeometry(14, 21), new THREE.MeshStandardMaterial({map: ktT.t, roughness: 0.9})); kt.rotation.x = -PI/2; kt.position.set(-31, 0.03, 0); kt.receiveShadow = true; baseG.add(kt);
      [0xd14a5c, 0x4a7bd1, 0x6fd38a].forEach(function(col, n){ var kg = new THREE.Group(); box(1.1, 0.3, 0.65, mat(col, {metalness: 0.5}), 0, 0.3, 0, kg); baseG.add(kg); dyn.karts.push({g: kg, o: n*0.33}); });
      var kl = c.label("Kart Racing", HEX.coral, 0.9); kl.position.set(-31, 4.5, 0); baseG.add(kl);
    }
    if(opts.campus && level >= 55){
      var ar = new THREE.Group(); ar.position.set(0, 0, -34); baseG.add(ar);
      c.add(new THREE.Mesh(new THREE.CylinderGeometry(9.5, 8, 4, 40, 1, true), mat(0x3a5068, {side: THREE.DoubleSide, roughness: 0.7})), 0, 2, 0, ar);
      var field = new THREE.Mesh(new THREE.CircleGeometry(7.6, 40), new THREE.MeshStandardMaterial({color: 0x1d4a3a, emissive: new THREE.Color(COL.cyan), emissiveIntensity: 0.15})); field.rotation.x = -PI/2; field.position.y = 0.05; ar.add(field);
      var al = c.label("Arena", HEX.cyan, 1.1); al.position.set(0, 7, 0); ar.add(al);
    }
    // HQ 2.1 decor (a cosmetic the owner wears): flags, gnomes, fireworks or a neon outline
    if(look.decor === "flags"){
      for(var fi = 0; fi < 8; fi++){ var fa = fi/8*PI*2 + 0.2, fx = Math.cos(fa)*13.6, fz = Math.sin(fa)*13.6;
        cyl(0.05, 0.05, 4.2, mat(0xb8c4cf, {metalness: 0.6}), fx, 2.1, fz, baseG, 6);
        var fl = box(1.1, 0.7, 0.03, mat([0xff6b5b, 0x5fd3e6, 0xffb347, 0x6fd38a][fi % 4]), fx + 0.55, 3.75, fz, baseG); fl.userData.wave = fi; dyn.flags.push(fl); }
    } else if(look.decor === "gnomes"){
      for(var gi = 0; gi < 10; gi++){ var ga = gi/10*PI*2 + 0.3, gx = Math.cos(ga)*14.8, gz = Math.sin(ga)*14.8, gn = new THREE.Group(); gn.position.set(gx, 0, gz); baseG.add(gn);
        cyl(0.22, 0.28, 0.45, mat(0x3a5ea8), 0, 0.22, 0, gn, 10); sph(0.18, mat(0xf1c9a5), 0, 0.58, 0, gn); sph(0.16, mat(0xf4f4f4), 0, 0.48, 0.1, gn);
        c.add(new THREE.Mesh(new THREE.ConeGeometry(0.2, 0.45, 10), mat(0xd14a5c)), 0, 0.88, 0, gn); }
    } else if(look.decor === "fireworks"){
      for(var fw = 0; fw < 3; fw++){ var g = new THREE.Group(); baseG.add(g); var parts = [];
        for(var q = 0; q < 18; q++){ var sp = c.glow([COL.amber, COL.cyan, COL.coral][fw], 0.6, 1); g.add(sp); parts.push({s: sp, a: q/18*PI*2, b: (q % 3 - 1)*0.6}); }
        dyn.fireworks.push({g: g, parts: parts, o: fw/3, x: (fw - 1)*6, y: top + 8 + fw*2}); }
    } else if(look.decor === "crown"){
      // HQ 2.5 prestige reward (star 3): a gold crown on the roof, seen by everyone (cos decor)
      var crown = new THREE.Group(); crown.name = "decor-crown"; crown.position.set(0, top, floorsHigh ? -0.5 : 0); baseG.add(crown);
      var gm = mat(0xf5d76e, {metalness: 0.85, roughness: 0.2, emissive: new THREE.Color(0xd8a43a), emissiveIntensity: 0.25});
      var band = new THREE.Mesh(new THREE.CylinderGeometry(2.6, 2.4, 1.0, 32, 1, true), Object.assign(gm.clone(), {side: THREE.DoubleSide})); band.position.y = 0.5; crown.add(band);
      for(var ci = 0; ci < 8; ci++){ var ca = ci/8*PI*2, pt = new THREE.Mesh(new THREE.ConeGeometry(0.45, 1.4, 4), gm); pt.position.set(Math.cos(ca)*2.5, 1.7, Math.sin(ca)*2.5); crown.add(pt);
        var gem = new THREE.Mesh(new THREE.OctahedronGeometry(0.22), mat([0xd14a5c, 0x4a7bd1, 0x3fa66a][ci % 3], {metalness: 0.2, roughness: 0.1})); gem.position.set(Math.cos(ca)*2.62, 0.55, Math.sin(ca)*2.62); crown.add(gem); }
    } else if(look.decor === "neon"){
      // a bright outline on every corner and floor line of the tower, lit by day too
      var nm = new THREE.MeshBasicMaterial({color: accent, toneMapped: false});
      [[-1, -1], [1, -1], [-1, 1], [1, 1]].forEach(function(q){ box(0.2, top, 0.2, nm, q[0]*4.66, top/2, q[1]*3.66, baseG, true); });
      for(var nf = 3.5; nf < topLow; nf += 5.2){ box(9.5, 0.1, 0.1, nm, 0, nf, 3.68, baseG, true); box(9.5, 0.1, 0.1, nm, 0, nf, -3.68, baseG, true); box(0.1, 0.1, 7.5, nm, 4.68, nf, 0, baseG, true); box(0.1, 0.1, 7.5, nm, -4.68, nf, 0, baseG, true); }
      box(9.6, 0.16, 0.16, nm, 0, top + 0.6, 3.7, baseG, true); box(9.6, 0.16, 0.16, nm, 0, top + 0.6, -3.7, baseG, true);
      var ng = c.glow(accent, 9, 0.35); ng.position.set(0, top + 1, 0); baseG.add(ng);
    }
    // your crew's banner on the roof (HQ 2.1 crews)
    if(look.crew && /^#[0-9a-f]{6}$/i.test(look.crew.color || "")){
      var crewCol = parseInt(look.crew.color.slice(1), 16);
      cyl(0.08, 0.08, 4, mat(0xb8c4cf, {metalness: 0.6}), -2.6, top + 2, 1.6, baseG, 8);
      var ct = c.canvasTex(256, 128, function(g){ g.fillStyle = look.crew.color; g.fillRect(0, 0, 256, 128); g.fillStyle = "#0e1923"; g.font = "700 64px system-ui, sans-serif"; g.textAlign = "center"; g.fillText(String(look.crew.tag || "").slice(0, 4), 128, 86); });
      var cf = c.screen(1.8, 0.9, ct.t); cf.material.side = THREE.DoubleSide; cf.position.set(-1.7, top + 3.5, 1.6); baseG.add(cf); cf.userData.wave = 9; dyn.flags.push(cf);
    }
    var sb = c.label(c.short((look.sign || "Claude HQ"), 16) + " · Lv " + level, HEX.amber, 1.1); sb.position.set(0, top + 7, 0); baseG.add(sb);
    built.level = level; built.look = JSON.stringify(look); built.top = top;
    B.wings = level >= 30 ? 2 : level >= 10 ? 1 : 0; B.level = level;
    applyTime();
  }
  function applyTime(){
    if(!dyn) return;
    dyn.facMats.forEach(function(m){ m.emissiveIntensity = night ? 1 : 0.06; });
    dyn.lobbyMats.forEach(function(m){ m.emissiveIntensity = night ? 0.45 : 0.12; });
    dyn.ledges.forEach(function(m){ m.emissiveIntensity = night ? 1.1 : 0.45; });
    dyn.glows.forEach(function(g){ g.visible = night; });
  }
  B.g = baseG;
  B.top = function(){ return built.top || 10; };
  B.build = build;
  B.stale = function(){ return Math.max(1, opts.level() | 0) !== built.level || JSON.stringify(opts.look() || {}) !== built.look ||
    (opts.prestige ? Math.max(0, Math.min(5, opts.prestige() | 0)) : 0) !== (built.stars | 0); };
  B.setNight = function(n){ night = n; applyTime(); };
  // more crew at work, more windows lit
  B.setLive = function(live){
    litShare = Math.min(0.9, 0.3 + live*0.08);
    if(live !== liveSeen && dyn){ liveSeen = live; dyn.facades.forEach(function(F){ F.wins.forEach(function(wn){ wn.lit = wn.r < litShare; paintWin(F.emi.g, wn); }); F.emi.t.needsUpdate = true; }); }
  };
  // what you cannot walk through, in the building's own frame
  B.blockedLocal = function(x, z){
    if(Math.abs(x) < 4.8 && Math.abs(z) < 3.8) return true;                         // the tower
    if(B.wings >= 1 && Math.abs(x + 10.5) < 3.0 && Math.abs(z - 0.5) < 2.8) return true;
    if(B.wings >= 2 && Math.abs(x - 10.5) < 3.0 && Math.abs(z - 0.5) < 2.8) return true;
    if(!opts.campus) return false;
    if(B.level >= 5 && Math.hypot(x, z - 10.4) < 2.8) return true;                  // the fountain
    if(B.level >= 50 && Math.abs(x - 7.4) < 1.1 && Math.abs(z - 9.6) < 1.1) return true;
    if(B.level >= 55 && Math.hypot(x, z + 34) < 9.8) return true;
    return false;
  };
  B.animate = function(t, dt, k){
    if(dyn.heli){ dyn.heli.rotor.rotation.y = t*14*k; var hv = (t % 24)/24, lift = hv < 0.4 ? 0 : hv < 0.5 ? (hv - 0.4)*10 : hv < 0.9 ? 1 : (1 - hv)*10;
      dyn.heli.g.position.y = dyn.heli.y + lift*6*k; dyn.heli.g.position.x = dyn.heli.x + lift*Math.sin(t*0.4)*3*k; }
    dyn.beacons.forEach(function(b, n){ var on = ((t + n*0.3) % 1.4) < 0.25; b.material.emissiveIntensity = on ? 3 : 0.4; dyn.beaconGlows[n].material.opacity = on ? 1 : 0.1; });
    // the prestige beacon turns and pulses; under calm (k < 1) it holds still
    if(dyn.prestige){ var pb = dyn.prestige; if(k >= 1){ pb.beam.rotation.y = t*0.8; pb.mat.opacity = 0.25 + 0.15*Math.sin(t*2.2); } else { pb.beam.rotation.y = 0; pb.mat.opacity = 0.35; } }
    dyn.banners.forEach(function(b, n){ b.rotation.x = Math.sin(t*1.6 + n)*0.06*k; });
    dyn.flags.forEach(function(f){ f.rotation.y = Math.sin(t*2.4 + f.userData.wave)*0.35*k; });
    dyn.fireworks.forEach(function(fw){ var u = (t*0.35*k + fw.o) % 1; fw.g.visible = night && u > 0.15;
      fw.parts.forEach(function(p){ var r = (u - 0.15)*9; p.s.position.set(fw.x + Math.cos(p.a)*r, fw.y + Math.sin(p.a)*r*0.8 - u*u*4 + p.b, Math.sin(p.a)*r*0.4); p.s.material.opacity = Math.max(0, 1 - u); }); });
    dyn.jets.forEach(function(j){ var u = (t*0.9*k + j.o) % 1, r = 0.6 + u*1.4; j.g.position.set(Math.cos(j.a)*r, 2.0 + Math.sin(u*PI)*1.3 - u*0.9, Math.sin(j.a)*r); j.g.material.opacity = 0.9*(1 - u*0.6); });
    dyn.karts.forEach(function(kk){ var a = t*0.7*k + kk.o*PI*2; kk.g.position.set(-31 + Math.cos(a)*4.4, 0.05, Math.sin(a)*8); kk.g.rotation.y = Math.atan2(-Math.cos(a)*8, -Math.sin(a)*4.4); });
    twinkle += dt;
    if(twinkle > 0.45 && night && dyn.facades.length){ twinkle = 0; var F = dyn.facades[Math.floor(Math.random()*dyn.facades.length)], wn = F.wins[Math.floor(Math.random()*F.wins.length)]; wn.lit = !wn.lit; paintWin(F.emi.g, wn); F.emi.t.needsUpdate = true; }
  };
  build();
  return B;
}
HQV.hqBuilding = makeBuilding;

/* ---------- the view buildings: every tab of HQ has a building of its own ----------
 * They stand in Arena City (games/hqcity.js) and round your Base; walk in through the door
 * to open that tab. The list is fixed (not whichever tabs this page shows) so the city's grid
 * comes out the same for everyone on the street. Labels follow the page (api.viewTitle), so
 * a pack that renames a tab renames its building too. */
var VIEWS = [
  {view: "live", label: "Live", icon: "📡", col: 0x5fd3e6, h: 11},
  {view: "analytics", label: "Analytics", icon: "📊", col: 0x5fd3e6, h: 9},
  {view: "pokedex", label: "Pokédex", icon: "📖", col: 0x9b8cf0, h: 8},
  {view: "gym", label: "Gym", icon: "🥊", col: 0xff6b5b, h: 7},
  {view: "quests", label: "Quests", icon: "🎯", col: 0xffb347, h: 8},
  {view: "village", label: "Village", icon: "🏰", col: 0x6fd38a, h: 7},
  {view: "arena", label: "Arena", icon: "🏆", col: 0x5fd3e6, h: 12},
  {view: "store", label: "Store", icon: "🛒", col: 0x6fd38a, h: 6},
  {view: "cali", label: "Cali Tuesdays", icon: "🌮", col: 0xffb347, h: 6},
  {view: "valley", label: "Valley", icon: "✨", col: 0x6fd38a, h: 7},
  {view: "music", label: "Music", icon: "🎵", col: 0x9b8cf0, h: 8},
  {view: "workshop", label: "Workshop", icon: "🛠", col: 0xffb347, h: 7},
  {view: "compete", label: "Compete", icon: "🥇", col: 0xff6b5b, h: 10}
];
HQV.hqViews = VIEWS;
var VB_W = 9, VB_D = 7;     // a view building's footprint (m); its front (and door) faces +z

// One view building. opts.doorFront(): its door's point in the world (where you walk to go in).
function makeViewBuilding(c, v, opts){
  var THREE = c.THREE, mat = c.mat, emis = c.emis, box = c.box;
  var api = c.api || {}, t = (api.viewTitle && api.viewTitle(v.view)) || [], title = t[0] || v.label;
  var hex = "#" + v.col.toString(16).padStart(6, "0"), h = v.h, hits = [], doors = [];
  var g = new THREE.Group();
  box(VB_W + 1, 0.3, VB_D + 1, mat(0x3b4c5c, {roughness: 0.9}), 0, 0.15, 0, g);
  var body = box(VB_W, h, VB_D, mat(0x22313f, {roughness: 0.6, metalness: 0.2}), 0, 0.3 + h/2, 0, g);
  // lit bands round the walls, and a cap in the building's colour
  var bandM = emis(0x0d1a24, v.col, 1.2), capM = emis(0x16222e, v.col, 0.6), bands = [];
  for(var y = 2.6; y < h - 0.6; y += 2.2) bands.push(box(VB_W + 0.06, 0.18, VB_D + 0.06, bandM, 0, 0.3 + y, 0, g, true));
  box(VB_W + 0.4, 0.4, VB_D + 0.4, capM, 0, 0.5 + h, 0, g);
  // the sign over the door: the tab's name
  var st = c.canvasTex(512, 96, function(gx){ gx.fillStyle = "#0b1622"; gx.fillRect(0, 0, 512, 96); gx.fillStyle = hex; gx.font = "700 54px system-ui, sans-serif";
    gx.textAlign = "center"; gx.textBaseline = "middle"; gx.fillText(c.short(title.toUpperCase(), 16), 256, 50); });
  var sign = new THREE.Mesh(new THREE.PlaneGeometry(7, 1.3), new THREE.MeshBasicMaterial({map: st.t, toneMapped: false}));
  sign.position.set(0, 4.4, VB_D/2 + 0.05); g.add(sign);
  // the door: glass panels with a lit frame, and a mat in front
  var doorM = emis(0x0b1622, v.col, 0.45);
  var dl = box(1.15, 3, 0.12, doorM, -0.6, 1.8, VB_D/2 + 0.08, g), dr = box(1.15, 3, 0.12, doorM, 0.6, 1.8, VB_D/2 + 0.08, g);
  box(2.7, 0.12, 0.16, emis(0x0d1a24, v.col, 2), 0, 3.36, VB_D/2 + 0.1, g, true);
  box(2.6, 0.02, 1.4, emis(0x0d1a24, v.col, 0.5), 0, 0.31, VB_D/2 + 0.9, g, true);
  var icon = c.glyph(v.icon, hex); icon.scale.set(2.4, 2.4, 1); icon.position.set(0, h + 2.4, 0); g.add(icon);
  var lb = c.label(title, hex, 0.9); lb.position.set(0, h + 4.4, 0); g.add(lb);
  var door = {view: v.view, label: title, tip: title + (t[1] ? ": " + t[1] : ": go in"),
              front: opts.doorFront ? opts.doorFront() : new THREE.Vector3(0, 0, VB_D/2 + 1.2)};
  [body, sign, dl, dr].forEach(function(o){ o.userData.door = door; hits.push(o); }); doors.push(door);
  var night = true, ph = c.hashStr(v.view) % 7;
  return {g: g, hits: hits, doors: doors, view: v.view, top: function(){ return h + 0.7; },
    blockedLocal: function(x, z){ return Math.abs(x) < VB_W/2 + 0.3 && Math.abs(z) < VB_D/2 + 0.3; },
    setNight: function(n){ night = n; bandM.emissiveIntensity = n ? 1.2 : 0.35; capM.emissiveIntensity = n ? 0.6 : 0.15; doorM.emissiveIntensity = n ? 0.45 : 0.2; },
    animate: function(tt, dt, k){ icon.position.y = h + 2.4 + Math.sin(tt*1.4 + ph)*0.25*k; }};
}
HQV.hqViewBuilding = makeViewBuilding;

// A building between you and the camera fades, so you are never lost behind a tower (Arena City
// and the view buildings round your Base). items: [{x, z, H (its height), side (half its width;
// default a campus), fade: {g, on}}].
function fade(L, on){
  if(L.fade.on === on) return; L.fade.on = on;
  L.fade.g.traverse(function(o){ if(!o.isMesh) return; (Array.isArray(o.material) ? o.material : [o.material]).forEach(function(m){
    if(m.userData.op == null){ m.userData.op = m.opacity; m.userData.tr = m.transparent; }
    m.transparent = on || m.userData.tr; m.opacity = on ? 0.16 : m.userData.op; m.depthWrite = !on; m.needsUpdate = true; }); o.castShadow = !on; });
}
function cutaway(c, items){
  var me = c.avatar(), v = c.view && c.view(); if(!me || !v) return;
  var ux = Math.sin(v.yaw), uz = Math.cos(v.yaw), tp = Math.max(0.2, Math.tan(v.pitch));
  items.forEach(function(L){
    if(!L.fade) return;
    var vx = L.x - me.x, vz = L.z - me.z, along = vx*ux + vz*uz, side = Math.abs(vx*uz - vz*ux);
    fade(L, along > 2 && side < (L.side || 14) && along < L.H/tp + 8);
  });
}
HQV.hqCutaway = cutaway;

// Where the view buildings stand round your Base: a ring outside the ring road, facing the middle,
// clear of the parking lot (east), the kart track (west), the Arena annex (north) and the front
// path (south). Pure, so it is easy to test: [{v, a, x, z, rot}].
var BASE_RING = 37.5, PI_ = Math.PI;
function baseViewRing(){
  var free = [], step = PI_/22;
  function clear(a){
    var d = function(b){ return Math.abs(((a - b) % (2*PI_) + 3*PI_) % (2*PI_) - PI_); };   // angle between a and b
    return d(0) > 0.34 && d(PI_) > 0.38 && d(-PI_/2) > 0.42 && d(PI_/2) > 0.2;
  }
  for(var i = 0; i < 44; i++){ var a = i*step + step/2; if(clear(a)) free.push(a); }
  // spread the buildings evenly over the free angles
  return VIEWS.map(function(v, n){
    var a = free[Math.floor(n*free.length/VIEWS.length)], dx = -Math.cos(a), dz = -Math.sin(a);
    return {v: v, a: a, x: Math.cos(a)*BASE_RING, z: Math.sin(a)*BASE_RING, rot: Math.atan2(dx, dz), dx: dx, dz: dz};
  });
}
HQV.hqBaseViewRing = baseViewRing;

HQV.hqWorlds.base = function(c, w){
  var THREE = c.THREE, PI = c.PI, S = w.scene, COL = c.COL, HEX = c.HEX;
  var mat = c.mat, emis = c.emis, box = c.box, cyl = c.cyl, sph = c.sph;

  // light + sky
  var hemi = new THREE.HemisphereLight(0xbcd7ff, 0x223322, 2.2); S.add(hemi);
  var sun = new THREE.DirectionalLight(0xffffff, 2.2); sun.position.set(34, 60, 26); sun.castShadow = true;
  sun.shadow.mapSize.set(2048, 2048); var sc = sun.shadow.camera; sc.left = -45; sc.right = 45; sc.top = 45; sc.bottom = -45; sc.far = 160;
  S.add(sun); S.add(sun.target);
  var skyBg = {
    night: c.canvasTex(512, 512, function(g){ var gr = g.createLinearGradient(0, 0, 0, 512); gr.addColorStop(0, "#040a12"); gr.addColorStop(0.6, "#0b1826"); gr.addColorStop(1, "#16304a"); g.fillStyle = gr; g.fillRect(0, 0, 512, 512); var R = c.rng(8); for(var i = 0; i < 220; i++){ g.fillStyle = "rgba(255,255,255," + (0.25 + R()*0.7) + ")"; var s = R() < 0.1 ? 2 : 1; g.fillRect(R()*512, R()*330, s, s); } g.fillStyle = "rgba(255,244,214,.95)"; g.beginPath(); g.arc(410, 90, 20, 0, 7); g.fill(); }).t,
    day: c.canvasTex(512, 512, function(g){ var gr = g.createLinearGradient(0, 0, 0, 512); gr.addColorStop(0, "#5ea6dc"); gr.addColorStop(1, "#cfe8f7"); g.fillStyle = gr; g.fillRect(0, 0, 512, 512); }).t
  };

  // ground: grass, the ring road, plaza paving, paths and a parking lot (one texture)
  function W2C(x, z, u){ return [(x + 50)*u, (z + 50)*u]; }
  var gt = c.canvasTex(2048, 2048, function(g, wd){
    var u = wd/100, R = c.rng(41), m = wd/2, p;
    g.fillStyle = "#1f3d2c"; g.fillRect(0, 0, wd, wd);
    for(var i = 0; i < 14000; i++){ g.fillStyle = R() < 0.5 ? "rgba(46,92,58,.45)" : "rgba(22,48,33,.5)"; g.fillRect(R()*wd, R()*wd, 3 + R()*7, 3 + R()*7); }
    g.fillStyle = "#3b4c5c"; p = W2C(-1.8, 15, u); g.fillRect(p[0], p[1], 3.6*u, 36*u);
    g.fillStyle = "#2a2f36"; p = W2C(20, -1.6, u); g.fillRect(p[0], p[1], 6*u, 3.2*u); p = W2C(25, -8, u); g.fillRect(p[0], p[1], 11*u, 16*u);
    g.strokeStyle = "rgba(233,239,243,.75)"; g.lineWidth = 0.12*u;
    for(var s = 0; s < 6; s++){ var q = W2C(25.5 + s*1.8, -7.5, u); g.beginPath(); g.moveTo(q[0], q[1]); g.lineTo(q[0], q[1] + 4.5*u); g.stroke(); q = W2C(25.5 + s*1.8, 3, u); g.beginPath(); g.moveTo(q[0], q[1]); g.lineTo(q[0], q[1] + 4.5*u); g.stroke(); }
    g.lineWidth = 4*u; g.strokeStyle = "#2a2f36"; g.beginPath(); g.arc(m, m, 19*u, 0, 7); g.stroke();
    g.lineWidth = 0.22*u; g.strokeStyle = "#8e9aa6"; [16.9, 21.1].forEach(function(r){ g.beginPath(); g.arc(m, m, r*u, 0, 7); g.stroke(); });
    g.setLineDash([1.4*u, 1.2*u]); g.lineWidth = 0.14*u; g.strokeStyle = "rgba(255,200,110,.8)"; g.beginPath(); g.arc(m, m, 19*u, 0, 7); g.stroke(); g.setLineDash([]);
    g.fillStyle = "rgba(233,239,243,.8)"; for(var z = 0; z < 6; z++){ p = W2C(-1.6 + z*0.6, 17, u); g.fillRect(p[0], p[1], 0.35*u, 4*u); }
    g.fillStyle = "#3b4c5c"; g.beginPath(); g.arc(m, m, 15.6*u, 0, 7); g.fill();
    g.strokeStyle = "rgba(255,255,255,.07)"; g.lineWidth = 0.08*u;
    for(var r = 2; r < 15.6; r += 1.3){ g.beginPath(); g.arc(m, m, r*u, 0, 7); g.stroke(); }
    for(var a = 0; a < 48; a++){ var an = a/48*PI*2; g.beginPath(); g.moveTo(m + Math.cos(an)*2*u, m + Math.sin(an)*2*u); g.lineTo(m + Math.cos(an)*15.6*u, m + Math.sin(an)*15.6*u); g.stroke(); }
    g.strokeStyle = "rgba(255,179,71,.55)"; g.lineWidth = 0.18*u; g.beginPath(); g.arc(m, m, 15.4*u, 0, 7); g.stroke();
  });
  var ground = new THREE.Mesh(new THREE.CircleGeometry(50, 96), new THREE.MeshStandardMaterial({map: gt.t, roughness: 0.95}));
  ground.rotation.x = -PI/2; ground.receiveShadow = true; S.add(ground);

  var ring = baseViewRing();
  // trees, bushes, benches, street lamps
  var tr = c.rng(57);
  for(var t = 0; t < 90; t++){
    var a2 = tr()*PI*2, r2 = 23 + tr()*24, tx = Math.cos(a2)*r2, tz = Math.sin(a2)*r2;
    if((tx > 22 && Math.abs(tz) < 10) || (tx < -21 && Math.abs(tz) < 13) || (tz < -22 && Math.abs(tx) < 13) || (Math.abs(tx) < 3.5 && tz > 15)) continue;
    if(ring.some(function(r){ return Math.hypot(tx - r.x, tz - r.z) < 7.5; })) continue;   // the view buildings
    var tg = new THREE.Group(); tg.position.set(tx, 0, tz); tg.scale.setScalar(0.8 + tr()*0.6); S.add(tg);
    cyl(0.2, 0.28, 1.2, mat(0x5a3d2a), 0, 0.6, 0, tg, 8);
    if(tr() < 0.55){ for(var cn = 0; cn < 3; cn++){ c.add(new THREE.Mesh(new THREE.ConeGeometry(1.3 - cn*0.3, 1.6, 8), mat([0x2f6b45, 0x2a5f3d, 0x357a4f][cn])), 0, 1.6 + cn*0.9, 0, tg); } }
    else { [[0, 2.1, 0, 1.2], [0.6, 2.6, 0.2, 0.8], [-0.5, 2.5, -0.3, 0.85]].forEach(function(b){ sph(b[3], mat([0x3c8a4f, 0x4a9a58, 0x2f7a45][Math.floor(tr()*3)], {roughness: 0.9}), b[0], b[1], b[2], tg); }); }
  }
  for(var bu = 0; bu < 30; bu++){ var ba = bu/30*PI*2 + 0.05, bx = Math.cos(ba)*16.2, bz = Math.sin(ba)*16.2; if(Math.abs(bx) < 2.4 && bz > 0) continue; sph(0.55, mat(0x2f7a45, {roughness: 0.9}), bx, 0.35, bz).scale.y = 0.7; }
  for(var bn = 0; bn < 8; bn++){ var bna = bn/8*PI*2 + PI/8, bg = new THREE.Group(); bg.position.set(Math.cos(bna)*14.4, 0, Math.sin(bna)*14.4); bg.rotation.y = -bna + PI/2; S.add(bg); box(1.6, 0.1, 0.5, mat(0x8a5a35), 0, 0.45, 0, bg); box(1.6, 0.45, 0.08, mat(0x8a5a35), 0, 0.75, -0.22, bg); }
  var lampGlows = [];
  function streetLamp(x, z){ cyl(0.07, 0.1, 3.4, mat(0x2b3a48, {metalness: 0.6}), x, 1.7, z, null, 8); var bulb = sph(0.16, emis(0xfff1d6, 0xffd9a0, 2), x, 3.3, z); var gl = c.glow(0xffd9a0, 2.2, 0.8); gl.position.set(x, 3.25, z); S.add(gl); lampGlows.push(gl); }
  for(var lp = 0; lp < 14; lp++){ var la = lp/14*PI*2 + 0.22; streetLamp(Math.cos(la)*15.9, Math.sin(la)*15.9); }
  for(var lr = 0; lr < 16; lr++){ var lra = lr/16*PI*2 + 0.1; streetLamp(Math.cos(lra)*21.9, Math.sin(lra)*21.9); }

  // cars on the ring road, parked cars, chargers
  function car(col){
    var g = new THREE.Group(), body = mat(col, {metalness: 0.6, roughness: 0.3});
    box(1.9, 0.45, 0.95, body, 0, 0.42, 0, g); box(1.0, 0.42, 0.85, mat(0x0b1622, {metalness: 0.6, roughness: 0.15}), -0.1, 0.82, 0, g);
    [[0.6, 0.45], [-0.6, 0.45], [0.6, -0.45], [-0.6, -0.45]].forEach(function(wl){ cyl(0.2, 0.2, 0.14, mat(0x111111), wl[0], 0.2, wl[1], g, 10).rotation.x = PI/2; });
    var lights = [];
    [-0.3, 0.3].forEach(function(z){ var h = c.glow(0xfff1d6, 0.7, 0.9); h.position.set(0.98, 0.45, z); g.add(h); lights.push(h); var tl = c.glow(0xff4040, 0.45, 0.9); tl.position.set(-0.98, 0.45, z); g.add(tl); lights.push(tl); });
    return {g: g, lights: lights};
  }
  var cars = [];
  [[0xd14a5c, 18, 0.0, 0.22], [0x4a7bd1, 18, 2.4, 0.22], [0xe0e4e8, 20, 1.1, -0.18], [0xffb347, 20, 4.0, -0.18], [0x3fa66a, 18, 4.6, 0.22]].forEach(function(cd){ var k = car(cd[0]); S.add(k.g); cars.push({c: k, r: cd[1], a: cd[2], v: cd[3]}); });
  [[27, -5.3, 0x8a4ad1], [28.8, -5.3, 0xe0e4e8], [32.4, -5.3, 0x2aa7a1], [27, 5.3, 0xd14a5c], [30.6, 5.3, 0x4a7bd1], [34.2, 5.3, 0xc9a14a]].forEach(function(pc){ var k = car(pc[2]); k.g.position.set(pc[0], 0, pc[1]); k.g.rotation.y = PI/2; S.add(k.g); k.lights.forEach(function(l){ l.visible = false; }); });

  // people strolling round the plaza (the Kenney characters the HQ uses)
  var strollers = [];
  for(var sw = 0; sw < 5; sw++){
    (function(n){
      var holder = new THREE.Group(); S.add(holder);
      var st = {g: holder, ch: null, ph: n*1.25, dir: n % 2 ? 1 : -1, rx: 9 + (n % 3), rz: 3.2 + (n % 2)*0.8};
      c.character("stroller:" + n, function(ch){ st.ch = ch; holder.add(ch.o); c.anim(ch, "walk"); });
      strollers.push(st);
    })(sw);
  }
  var clouds = new THREE.Group(); S.add(clouds);
  for(var cl = 0; cl < 6; cl++){ var cg = new THREE.Group(); cg.position.set(-40 + cl*16, 34 + (cl % 3)*4, -20 + (cl % 2)*30); clouds.add(cg); [[0, 0, 0, 2.6], [2.4, -0.4, 0.4, 2], [-2.3, -0.5, -0.2, 1.9], [1, 0.9, -0.5, 1.7]].forEach(function(b){ sph(b[3], mat(0xffffff, {roughness: 1}), b[0], b[1], b[2], cg).castShadow = false; }); }

  var Bld = makeBuilding(c, {level: function(){ return c.level(); }, look: function(){ return c.look(); }, campus: true,
    prestige: function(){ return (c.api && typeof c.api.prestige === "function") ? (c.api.prestige() | 0) : 0; },
    live: function(){ return (c.sessions() || []).filter(function(s){ return c.crewState(s) !== "stale"; }).length; }});
  S.add(Bld.g);
  // a building for every tab, round the outside of the ring road
  var views = ring.map(function(r){
    var V = makeViewBuilding(c, r.v, {doorFront: function(){ return new THREE.Vector3(r.x + r.dx*(VB_D/2 + 1.2), 0, r.z + r.dz*(VB_D/2 + 1.2)); }});
    V.g.position.set(r.x, 0, r.z); V.g.rotation.y = r.rot; S.add(V.g);
    V.at = r; V.x = r.x; V.z = r.z; V.H = V.top() + 2; V.side = 7; V.fade = {g: V.g, on: false}; return V;
  });
  var night = true;
  function applyTime(){
    Bld.setNight(night);
    views.forEach(function(V){ V.setNight(night); });
    lampGlows.forEach(function(g){ g.material.opacity = night ? 0.85 : 0.08; });
    cars.forEach(function(k){ k.c.lights.forEach(function(l){ l.material.opacity = night ? 0.95 : 0.25; }); });
    clouds.visible = !night;
  }

  // what you cannot walk through
  function blocked(x, z){
    if(Math.hypot(x, z) > 46 || Bld.blockedLocal(x, z)) return true;
    return views.some(function(V){ var r = V.at, rx = x - r.x, rz = z - r.z, c0 = Math.cos(-r.rot), s0 = Math.sin(-r.rot);
      return V.blockedLocal(rx*c0 + rz*s0, -rx*s0 + rz*c0); });          // into the building's own frame
  }

  applyTime();
  var acc = 0, fadeAcc = 0;
  // Bld rebuilds its own hits/doors on a level change, so these are read fresh each time
  Object.defineProperty(w, "hits", {get: function(){ return views.reduce(function(a, V){ return a.concat(V.hits); }, Bld.hits.slice()); }, configurable: true});
  Object.defineProperty(w, "doors", {get: function(){ return views.reduce(function(a, V){ return a.concat(V.doors); }, Bld.doors.slice()); }, configurable: true});
  w.floor = ground; w.span = 17; w.zoom = 0.85; w.camY = 2.5; w.follow = 1; w.camPitch = 0.5;
  w.bounds = [-46, 46, -46, 46];
  w.spawn = {x: 3, z: 14, yaw: PI};
  w.spawnFrom = {lobby: {x: 0, z: 7.4, yaw: 0}};
  w.blocked = blocked;
  w.setTime = function(n){ night = n; S.background = n ? skyBg.night : skyBg.day; S.fog = new THREE.Fog(n ? 0x0f1f30 : 0xb8dcf2, 70, 160);
    hemi.intensity = n ? 1.3 : 2.4; sun.intensity = n ? 0.7 : 2.4; sun.color.set(n ? 0xa9b8ff : 0xfff2dc); applyTime(); };
  w.onData = function(list){ Bld.setLive((list || []).filter(function(s){ return c.crewState(s) !== "stale"; }).length); };
  w.onLook = function(){ Bld.build(); };
  w.update = function(t, dt, k){
    acc += dt;
    if(acc > 2){ acc = 0; if(Bld.stale()) Bld.build(); }
    Bld.animate(t, dt, k);
    views.forEach(function(V){ V.animate(t, dt, k); });
    fadeAcc += dt; if(fadeAcc > 0.15){ fadeAcc = 0; cutaway(c, views); }
    cars.forEach(function(k2){ k2.a += k2.v*dt*k; var x = Math.cos(k2.a)*k2.r, z = Math.sin(k2.a)*k2.r, dx = -Math.sin(k2.a)*k2.v, dz = Math.cos(k2.a)*k2.v; k2.c.g.position.set(x, 0, z); k2.c.g.rotation.y = Math.atan2(-dz, dx); });
    strollers.forEach(function(s){ var a = t*0.12*s.dir*k + s.ph, x = Math.cos(a)*s.rx, z = 9.6 + Math.sin(a)*s.rz; s.g.position.set(x, 0, z);
      var dx = -Math.sin(a)*s.rx*s.dir, dz = Math.cos(a)*s.rz*s.dir; s.g.rotation.y = Math.atan2(dx, dz); if(s.ch) s.ch.mixer.update(dt*k); });
    clouds.position.x = ((t*0.6*k) % 40) - 20;
  };
  return w;
};
})();
