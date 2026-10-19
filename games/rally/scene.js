/* Valley Rally: the three.js scene. A classic script (window.HQV.rallyScene) that is handed
 * the vendored three.js module by games/rally.js, so it never imports anything itself.
 *
 *   var R = HQV.rallyScene.create(THREE, canvas, {low, calm});
 *   R.setStage(stage)              terrain chunks, road ribbon, scenery, sky, lights, fog
 *   var h = R.addCar(spec, {ghost, name, number, color})   a procedural car
 *   R.poseCar(h, pose, car)        place it (pose from physics.pose(), car = physics car or a stand-in)
 *   R.fx(car, dt)                  dust, gravel, spray and tyre tracks under a car
 *   R.render(dt, cam)              cam: {mode, car, pose, calm}
 *   R.size(w, h); R.dispose()
 *
 * Everything is generated here: geometry from primitives, textures drawn on canvases
 * (seeded with mulberry32, so a stage always looks the same), no files. Low-poly toon
 * look: flat-shaded vertex colours, one sun with soft shadows, fog, a gradient sky.
 */
(function(root){
"use strict";
var HQV = root.HQV = root.HQV || {};
function clamp(v, a, b){ return v < a ? a : v > b ? b : v; }
function lerp(a, b, u){ return a + (b - a)*u; }
function hashStr(s){ var h = 2166136261 >>> 0; s = String(s); for(var i = 0; i < s.length; i++){ h ^= s.charCodeAt(i); h = Math.imul(h, 16777619) >>> 0; } return h >>> 0; }
function mulberry32(seed){ var a = (typeof seed === "number" ? seed : hashStr(seed)) >>> 0; return function(){ a = (a + 0x6D2B79F5) >>> 0; var t = a; t = Math.imul(t ^ (t >>> 15), t | 1); t ^= t + Math.imul(t ^ (t >>> 7), t | 61); return ((t ^ (t >>> 14)) >>> 0)/4294967296; }; }

/* Original sponsor names for banners and liveries (made up for this game). */
var SPONSORS = [["BRAMBLEFOX", "OILS"], ["GRITWING", "TYRES"], ["MOSSBEAK", "COFFEE"], ["QUARRYLARK", "RADIO"],
                ["DUSTMOTH", "SPARES"], ["RIVETPINE", "TOOLS"], ["THISTLEBOLT", "ENERGY"], ["LARCHLIGHT", "LAMPS"]];

/* Biome palettes (scene colours, not UI: the 3D world has its own look in every theme). */
var BIOME = {
  forest: {sky: [0x5fa8e6, 0xbfe3f5], fog: 0xcfe6ee, hemiSky: 0xdff1ff, hemiGround: 0x4a6b33, sun: 0xfff1d6, sunI: 2.6, hemiI: 1.25,
           grass: [0x5e9a3c, 0x78b24a, 0x4c8a35], mud: 0x6a5236, dirt: 0x9b7a52, loose: 0xa89270, snow: 0xf2f6fa,
           conifer: [0x2f6b3a, 0x3d7d42, 0x285f35], leaf: [0x5aa046, 0x6db34f, 0x4f9440], trunk: 0x6b4a32, rock: 0x8d8f8c, tuft: 0x6fae45},
  hills:  {sky: [0x6aaee0, 0xe9dcc0], fog: 0xe7d9bd, hemiSky: 0xfff1d8, hemiGround: 0x9a7d4e, sun: 0xffe2b0, sunI: 2.9, hemiI: 1.2,
           grass: [0xb7a75a, 0xc9b56a, 0xa39550], mud: 0x7a5c3a, dirt: 0xc08f5a, loose: 0xc8ad84, snow: 0xf2f6fa,
           conifer: [0x4f6b3a, 0x5d7740, 0x46603a], leaf: [0x8a9a48, 0x9aa850, 0x7c8c40], trunk: 0x76563a, rock: 0xa89a86, tuft: 0xbfae62},
  snow:   {sky: [0x7fa9d6, 0xe6eef6], fog: 0xe4ecf3, hemiSky: 0xf0f6ff, hemiGround: 0xb4c2cf, sun: 0xfff6ea, sunI: 2.2, hemiI: 1.45,
           grass: [0xf4f7fb, 0xe9eef5, 0xdfe7f0], mud: 0xb9c3cc, dirt: 0xb7a58e, loose: 0xc9ccd0, snow: 0xf4f7fb,
           conifer: [0x2d5a40, 0x356748, 0x284f3a], leaf: [0x4f7d55, 0x5a8a5f, 0x47714c], trunk: 0x5e4a3a, rock: 0x8f969c, tuft: 0xdfe6ee}
};
// surface ids (physics.js SURFACES): tarmac, gravel, dirt, grass, mud, snow, loose
var ROAD_COL = [0x55585c, 0xa58f6c, 0x9b7652, 0x6f9a48, 0x6a5236, 0xcfd8e2, 0xb09a78];

function create(THREE, canvas, opts){
  opts = opts || {};
  var low = !!opts.low;
  var renderer = new THREE.WebGLRenderer({canvas: canvas, antialias: !low, powerPreference: "high-performance"});
  renderer.setPixelRatio(low ? 1 : Math.min(root.devicePixelRatio || 1, 1.5));
  renderer.outputColorSpace = THREE.SRGBColorSpace;
  renderer.shadowMap.enabled = !low; renderer.shadowMap.type = THREE.PCFSoftShadowMap;
  var scene = new THREE.Scene(), camera = new THREE.PerspectiveCamera(62, 1.6, 0.1, 1400);
  var hemi = new THREE.HemisphereLight(0xffffff, 0x556644, 1.3); scene.add(hemi);
  var sun = new THREE.DirectionalLight(0xffffff, 2.5); scene.add(sun); scene.add(sun.target);
  sun.castShadow = !low; sun.shadow.mapSize.set(2048, 2048); sun.shadow.bias = -0.0004; sun.shadow.normalBias = 0.04; sun.shadow.radius = 3;
  var shc = sun.shadow.camera; shc.left = -70; shc.right = 70; shc.top = 70; shc.bottom = -70; shc.near = 1; shc.far = 320;
  var sunDir = new THREE.Vector3(0.6, 0.45, 0.4).normalize();
  var world = new THREE.Group(); scene.add(world);
  var R = {renderer: renderer, scene: scene, camera: camera, stage: null, cars: [], chunks: [], deco: [], low: low, alive: true};
  var M = {};                                   // shared materials
  var TMP = new THREE.Vector3(), TMP2 = new THREE.Vector3(), Q = new THREE.Quaternion(), MAT4 = new THREE.Matrix4(), EUL = new THREE.Euler(), COL = new THREE.Color();

  /* ---------- small geometry helpers ---------- */
  // merge non-indexed geometries that carry position + normal + color
  function merge(list){
    var n = 0; list.forEach(function(g){ n += g.attributes.position.count; });
    var pos = new Float32Array(n*3), nor = new Float32Array(n*3), col = new Float32Array(n*3), o = 0;
    list.forEach(function(g){
      pos.set(g.attributes.position.array, o*3); nor.set(g.attributes.normal.array, o*3); col.set(g.attributes.color.array, o*3);
      o += g.attributes.position.count; g.dispose();
    });
    var out = new THREE.BufferGeometry();
    out.setAttribute("position", new THREE.BufferAttribute(pos, 3)); out.setAttribute("normal", new THREE.BufferAttribute(nor, 3)); out.setAttribute("color", new THREE.BufferAttribute(col, 3));
    out.computeBoundingSphere(); out.computeBoundingBox();
    return out;
  }
  // a primitive, made non-indexed, transformed, flat-coloured (with an optional per-vertex tweak)
  function prim(g, color, tx, tweak){
    g = g.index ? g.toNonIndexed() : g;
    if(tx) g.applyMatrix4(tx);
    g.computeVertexNormals();
    var c = new THREE.Color(color), n = g.attributes.position.count, arr = new Float32Array(n*3), p = g.attributes.position.array, nm = g.attributes.normal.array;
    for(var i = 0; i < n; i++){
      var cc = c;
      if(tweak){ cc = tweak(p[i*3], p[i*3+1], p[i*3+2], nm[i*3+1], c) || c; }
      arr[i*3] = cc.r; arr[i*3+1] = cc.g; arr[i*3+2] = cc.b;
    }
    g.setAttribute("color", new THREE.BufferAttribute(arr, 3));
    if(g.attributes.uv) g.deleteAttribute("uv");
    return g;
  }
  function tr(x, y, z, rx, ry, rz, sx, sy, sz){
    var m = new THREE.Matrix4();
    EUL.set(rx || 0, ry || 0, rz || 0, "YXZ"); Q.setFromEuler(EUL);
    m.compose(TMP.set(x || 0, y || 0, z || 0), Q, TMP2.set(sx == null ? 1 : sx, sy == null ? (sx == null ? 1 : sx) : sy, sz == null ? (sx == null ? 1 : sx) : sz));
    return m;
  }
  function canvasTex(w, h, draw, repeat){
    var c = document.createElement("canvas"); c.width = w; c.height = h;
    draw(c.getContext("2d"), w, h);
    var t = new THREE.CanvasTexture(c); t.colorSpace = THREE.SRGBColorSpace; t.anisotropy = low ? 1 : 4;
    if(repeat){ t.wrapS = t.wrapT = THREE.RepeatWrapping; }
    return t;
  }
  function hexCss(c){ return "#" + ("000000" + (c >>> 0).toString(16)).slice(-6); }
  function shade(c, k){ var C = new THREE.Color(c); C.multiplyScalar(k); return "#" + C.getHexString(); }

  /* ---------- sky, clouds, stars ---------- */
  var sky = null, clouds = [], stars = null, moon = null;
  function buildSky(st, pal){
    if(sky){ scene.remove(sky); sky.geometry.dispose(); sky.material.dispose(); }
    clouds.forEach(function(c){ scene.remove(c); c.material.dispose(); }); clouds = [];
    if(stars){ scene.remove(stars); stars.geometry.dispose(); stars.material.dispose(); stars = null; }
    if(moon){ scene.remove(moon); moon.material.dispose(); moon = null; }
    var top = new THREE.Color(st.night ? 0x050a1c : pal.sky[0]), hor = new THREE.Color(st.night ? 0x1b2848 : pal.sky[1]);
    var g = new THREE.SphereGeometry(1200, 24, 16), p = g.attributes.position, col = new Float32Array(p.count*3);
    for(var i = 0; i < p.count; i++){
      var y = p.getY(i)/1200, u = clamp(Math.pow(Math.max(0, y), 0.55), 0, 1);
      COL.copy(hor).lerp(top, u); if(y < 0) COL.copy(hor).multiplyScalar(st.night ? 0.8 : 0.96);
      col[i*3] = COL.r; col[i*3+1] = COL.g; col[i*3+2] = COL.b;
    }
    g.setAttribute("color", new THREE.BufferAttribute(col, 3));
    sky = new THREE.Mesh(g, new THREE.MeshBasicMaterial({vertexColors: true, side: THREE.BackSide, fog: false, depthWrite: false}));
    sky.renderOrder = -10; scene.add(sky);
    var rng = mulberry32(hashStr("sky|" + st.seed));
    if(!st.night){
      var ctex = canvasTex(256, 128, function(g2, w, h){
        for(var k = 0; k < 14; k++){
          var x = 40 + rng()*(w - 80), yy = 50 + rng()*40, r = 18 + rng()*30;
          var gr = g2.createRadialGradient(x, yy, 1, x, yy, r);
          gr.addColorStop(0, "rgba(255,255,255,0.95)"); gr.addColorStop(0.6, "rgba(255,255,255,0.55)"); gr.addColorStop(1, "rgba(255,255,255,0)");
          g2.fillStyle = gr; g2.beginPath(); g2.arc(x, yy, r, 0, Math.PI*2); g2.fill();
        }
      });
      for(var c = 0; c < 16; c++){
        var a = rng()*Math.PI*2, d = 700 + rng()*300, sm = new THREE.SpriteMaterial({map: ctex, transparent: true, depthWrite: false, fog: false, opacity: 0.75 + rng()*0.25});
        var sp = new THREE.Sprite(sm); sp.position.set(Math.cos(a)*d, 150 + rng()*170, Math.sin(a)*d); var sc = 260 + rng()*260; sp.scale.set(sc, sc*0.5, 1);
        sp.renderOrder = -9; scene.add(sp); clouds.push(sp);
      }
    } else {
      var sg = new THREE.BufferGeometry(), sp2 = new Float32Array(900*3);
      for(var s = 0; s < 900; s++){
        var th = rng()*Math.PI*2, ph = Math.acos(0.05 + rng()*0.95);
        sp2[s*3] = Math.cos(th)*Math.sin(ph)*1100; sp2[s*3+1] = Math.cos(ph)*1100; sp2[s*3+2] = Math.sin(th)*Math.sin(ph)*1100;
      }
      sg.setAttribute("position", new THREE.BufferAttribute(sp2, 3));
      stars = new THREE.Points(sg, new THREE.PointsMaterial({color: 0xdfe8ff, size: 2, sizeAttenuation: false, fog: false, depthWrite: false}));
      stars.renderOrder = -9; scene.add(stars);
      var mtex = canvasTex(128, 128, function(g2, w){ var gr = g2.createRadialGradient(64, 64, 10, 64, 64, 62); gr.addColorStop(0, "rgba(255,250,235,1)"); gr.addColorStop(0.42, "rgba(240,240,255,0.95)"); gr.addColorStop(0.5, "rgba(200,210,255,0.25)"); gr.addColorStop(1, "rgba(200,210,255,0)"); g2.fillStyle = gr; g2.fillRect(0, 0, w, w); });
      moon = new THREE.Sprite(new THREE.SpriteMaterial({map: mtex, transparent: true, depthWrite: false, fog: false}));
      moon.position.set(sunDir.x*1000, 260, sunDir.z*1000); moon.scale.set(110, 110, 1); moon.renderOrder = -8; scene.add(moon);
    }
  }

  /* ---------- terrain ---------- */
  var CH = 64;                                     // cells per chunk side
  function surfColor(pal, sid, x, z, noise){
    var c;
    if(sid === 3) c = pal.grass[noise < 0.33 ? 0 : noise < 0.66 ? 1 : 2];
    else if(sid === 4) c = pal.mud; else if(sid === 2) c = pal.dirt; else if(sid === 5) c = pal.snow;
    else if(sid === 6) c = pal.loose; else if(sid === 1) c = pal.loose; else c = 0x6a6d70;
    return c;
  }
  function buildChunk(st, pal, cx, cz, step){
    var G = st.grid, x0 = G.x0, z0 = G.z0, cell = G.cell, nx = G.nx, nz = G.nz;
    var i0 = cx*CH, j0 = cz*CH, i1 = Math.min(nx - 1, i0 + CH), j1 = Math.min(nz - 1, j0 + CH);
    var cols = Math.floor((i1 - i0)/step) + 1, rows = Math.floor((j1 - j0)/step) + 1;
    if(cols < 2 || rows < 2) return null;
    var tris = (cols - 1)*(rows - 1)*2, pos = new Float32Array(tris*9), col = new Float32Array(tris*9), o = 0;
    var vx = [], vy = [], vz = [], vc = [];
    var rng = mulberry32(hashStr(st.seed + "|t|" + cx + "," + cz));
    for(var r = 0; r < rows; r++) for(var c = 0; c < cols; c++){
      var gi = Math.min(i1, i0 + c*step), gj = Math.min(j1, j0 + r*step), k = gj*nx + gi;
      var x = x0 + gi*cell, z = z0 + gj*cell;
      vx.push(x); vy.push(G.h[k]); vz.push(z);
      var sid = G.surf[k], nz2 = ((hashStr(gi + ":" + gj) % 1000)/1000);
      COL.setHex(surfColor(pal, sid, x, z, nz2));
      var j2 = 0.94 + nz2*0.1; COL.multiplyScalar(j2);
      vc.push(COL.r, COL.g, COL.b);
    }
    function V(c, r){ return r*cols + c; }
    for(var rr = 0; rr < rows - 1; rr++) for(var cc = 0; cc < cols - 1; cc++){
      var a = V(cc, rr), b = V(cc + 1, rr), d = V(cc, rr + 1), e = V(cc + 1, rr + 1);
      var quad = (cc + rr) % 2 === 0 ? [a, d, b, b, d, e] : [a, d, e, a, e, b];
      for(var t = 0; t < 6; t++){
        var vi = quad[t];
        pos[o] = vx[vi]; pos[o+1] = vy[vi]; pos[o+2] = vz[vi];
        // flat shading per triangle: colour of the triangle's first vertex keeps facets crisp
        var fv = quad[t - (t % 3)];
        col[o] = vc[fv*3]; col[o+1] = vc[fv*3+1]; col[o+2] = vc[fv*3+2];
        o += 3;
      }
    }
    var g = new THREE.BufferGeometry();
    g.setAttribute("position", new THREE.BufferAttribute(pos, 3)); g.setAttribute("color", new THREE.BufferAttribute(col, 3));
    g.computeVertexNormals(); g.computeBoundingSphere();
    var m = new THREE.Mesh(g, M.terrain); m.receiveShadow = !low; m.matrixAutoUpdate = false;
    return m;
  }
  function buildTerrain(st, pal){
    var G = st.grid, ncx = Math.ceil((G.nx - 1)/CH), ncz = Math.ceil((G.nz - 1)/CH);
    for(var cz = 0; cz < ncz; cz++) for(var cx = 0; cx < ncx; cx++){
      // only chunks near the road get full detail; all get a coarse version
      var midx = G.x0 + (cx + 0.5)*CH*G.cell, midz = G.z0 + (cz + 0.5)*CH*G.cell, L = st.locate(midx, midz);
      var near = L.d < 170;
      var coarse = buildChunk(st, pal, cx, cz, 4);
      if(!coarse) continue;
      var ch = {x: midx, z: midz, coarse: coarse, fine: null, cx: cx, cz: cz, near: near};
      world.add(coarse);
      R.chunks.push(ch);
    }
    // the land past the grid: one coarse skirt from the noise, lowered under the grid
    var ext = 900, step = 24, x0 = G.x0 - ext, z0 = G.z0 - ext, x1 = G.x0 + (G.nx - 1)*G.cell + ext, z1 = G.z0 + (G.nz - 1)*G.cell + ext;
    var cols = Math.ceil((x1 - x0)/step) + 1, rows = Math.ceil((z1 - z0)/step) + 1;
    var pos = new Float32Array(cols*rows*3), col = new Float32Array(cols*rows*3), idx = [];
    var gx1 = G.x0 + (G.nx - 1)*G.cell, gz1 = G.z0 + (G.nz - 1)*G.cell;
    for(var r = 0; r < rows; r++) for(var c = 0; c < cols; c++){
      var x = x0 + c*step, z = z0 + r*step, inside = x > G.x0 + 8 && x < gx1 - 8 && z > G.z0 + 8 && z < gz1 - 8;
      var y = st.natural(x, z) - (inside ? 8 : 0.4), k = (r*cols + c)*3;
      pos[k] = x; pos[k+1] = y; pos[k+2] = z;
      COL.setHex(pal.grass[(r + c) % 3]); COL.multiplyScalar(0.92);
      col[k] = COL.r; col[k+1] = COL.g; col[k+2] = COL.b;
      if(r < rows - 1 && c < cols - 1){ var a = r*cols + c; idx.push(a, a + cols, a + 1, a + 1, a + cols, a + cols + 1); }
    }
    var sg = new THREE.BufferGeometry(); sg.setAttribute("position", new THREE.BufferAttribute(pos, 3)); sg.setAttribute("color", new THREE.BufferAttribute(col, 3)); sg.setIndex(idx);
    sg = sg.toNonIndexed(); sg.computeVertexNormals();
    var skirt = new THREE.Mesh(sg, M.terrain); skirt.matrixAutoUpdate = false; skirt.receiveShadow = false; world.add(skirt);
    R.deco.push(skirt);
  }
  // swap chunks between coarse and fine around the camera (fine built lazily, a few per frame)
  var built = 0;
  function updateChunks(cx, cz){
    var budget = 2;
    for(var i = 0; i < R.chunks.length; i++){
      var ch = R.chunks[i], dx = ch.x - cx, dz = ch.z - cz, d = Math.sqrt(dx*dx + dz*dz);
      var wantFine = ch.near && d < (low ? 160 : 260);
      if(wantFine && !ch.fine && budget > 0){ ch.fine = buildChunk(R.stage, R.pal, ch.cx, ch.cz, 1); if(ch.fine) world.add(ch.fine); budget--; built++; }
      if(ch.fine){ ch.fine.visible = wantFine; ch.coarse.visible = !wantFine; }
      else ch.coarse.visible = true;
      if(d > (low ? 480 : 640)){ ch.coarse.visible = false; if(ch.fine) ch.fine.visible = false; }
    }
  }

  /* ---------- the road ribbon ---------- */
  var ROAD_COLS = [-1.75, -1, -0.62, -0.22, 0, 0.22, 0.62, 1, 1.75];   // across, in road half-widths (the outer pair is the shoulder)
  function roadTexture(sid, seed){
    var rng = mulberry32(hashStr("road|" + sid + "|" + seed));
    return canvasTex(256, 512, function(g, w, h){
      var base = ROAD_COL[sid];
      g.fillStyle = hexCss(base); g.fillRect(0, 0, w, h);
      var spk = sid === 0 ? 2600 : 4200;
      for(var i = 0; i < spk; i++){
        var x = rng()*w, y = rng()*h, r = sid === 0 ? 1 : 0.8 + rng()*1.6;
        g.fillStyle = shade(base, sid === 5 ? 0.9 + rng()*0.12 : 0.72 + rng()*0.5); g.globalAlpha = 0.55 + rng()*0.4;
        g.fillRect(x, y, r, r);
      }
      g.globalAlpha = 1;
      // shoulders: the outer 0.75/3.5 of each side is loose material
      var sh = Math.round(w*(0.75/3.5)*0.5);
      if(sid !== 0 && sid !== 5){
        g.fillStyle = "rgba(255,240,215,0.14)"; g.fillRect(0, 0, sh, h); g.fillRect(w - sh, 0, sh, h);
      }
      // tyre ruts: darker, smoother bands where the wheels run (lat +-0.78 m)
      [0.39, 0.61].forEach(function(u){
        var cx = u*w, gr = g.createLinearGradient(cx - 18, 0, cx + 18, 0);
        var dark = sid === 5 ? "rgba(110,130,155,0.6)" : sid === 0 ? "rgba(0,0,0,0.18)" : "rgba(60,40,25,0.32)";
        gr.addColorStop(0, "rgba(0,0,0,0)"); gr.addColorStop(0.5, dark); gr.addColorStop(1, "rgba(0,0,0,0)");
        g.fillStyle = gr; g.fillRect(cx - 18, 0, 36, h);
      });
      if(sid === 0){
        g.fillStyle = "rgba(240,240,232,0.92)"; g.fillRect(sh + 4, 0, 4, h); g.fillRect(w - sh - 8, 0, 4, h);
        g.fillStyle = "rgba(245,220,120,0.9)"; for(var y2 = 0; y2 < h; y2 += 128) g.fillRect(w/2 - 2, y2, 4, 64);
      } else {
        // loose stones along the crown and the edges
        for(var k = 0; k < 900; k++){
          var ux = rng() < 0.5 ? (0.5 + (rng() - 0.5)*0.08) : (rng() < 0.5 ? rng()*0.24 : 1 - rng()*0.24);
          g.fillStyle = shade(base, 0.6 + rng()*0.7); g.fillRect(ux*w, rng()*h, 1.5 + rng()*2, 1.5 + rng()*2);
        }
      }
    }, true);
  }
  function buildRoad(st){
    var pts = st.pts, n = pts.length, W = st.roadHalf, groups = {};
    var shoulderLat = W*1.75;
    for(var i = 0; i < n - 1; i++){
      var sid = pts[i].surf, G = groups[sid] || (groups[sid] = {pos: [], uv: [], idx: [], rows: 0, last: -2});
      if(G.last !== i){
        // start a new strip row (first row, or after another surface's stretch)
        addRow(G, pts[i]); G.rowsStart = G.rows - 1;
      }
      addRow(G, pts[i + 1]);
      var a = (G.rows - 2)*ROAD_COLS.length, b = (G.rows - 1)*ROAD_COLS.length;
      for(var c = 0; c < ROAD_COLS.length - 1; c++) G.idx.push(a + c, b + c, a + c + 1, a + c + 1, b + c, b + c + 1);
      G.last = i + 1;
    }
    function addRow(G, P){
      var nx = Math.cos(P.h), nz = Math.sin(P.h);
      for(var c = 0; c < ROAD_COLS.length; c++){
        var lat = ROAD_COLS[c]*W, x = P.x + nx*lat, z = P.z + nz*lat;
        var y = st.env.height(x, z) + (Math.abs(ROAD_COLS[c]) > 1.2 ? 0.015 : 0.03);
        G.pos.push(x, y, z);
        G.uv.push((ROAD_COLS[c] + 1.75)/3.5, P.s/9);
      }
      G.rows++;
    }
    Object.keys(groups).forEach(function(k){
      var G = groups[k], g = new THREE.BufferGeometry();
      g.setAttribute("position", new THREE.Float32BufferAttribute(G.pos, 3)); g.setAttribute("uv", new THREE.Float32BufferAttribute(G.uv, 2)); g.setIndex(G.idx);
      g.computeVertexNormals(); g.computeBoundingSphere();
      var mat = new THREE.MeshLambertMaterial({map: roadTexture(+k, st.seed), polygonOffset: true, polygonOffsetFactor: -2, polygonOffsetUnits: -2});
      var m = new THREE.Mesh(g, mat); m.receiveShadow = !low; m.matrixAutoUpdate = false; world.add(m); R.deco.push(m);
    });
  }

  /* ---------- scenery models (merged primitives with vertex colours) ---------- */
  function coniferGeo(pal, snowy, v){
    var parts = [prim(new THREE.CylinderGeometry(0.14, 0.2, 1.4, 5), pal.trunk, tr(0, 0.7, 0))];
    var tiers = [[1.9, 2.6, 1.6], [1.5, 2.2, 2.7], [1.05, 1.9, 3.7], [0.6, 1.4, 4.6]];
    tiers.forEach(function(t, i){
      var c = pal.conifer[(i + v) % 3];
      parts.push(prim(new THREE.ConeGeometry(t[0], t[1], 7), c, tr(0, t[2], 0, 0, i*0.4), snowy ? function(x, y, z, ny){ return ny > 0.3 && y > t[2] + t[1]*0.05 ? COL.set(0xf4f7fb) : null; } : null));
    });
    return merge(parts);
  }
  function roundGeo(pal, v, birch){
    var parts = [prim(new THREE.CylinderGeometry(birch ? 0.09 : 0.16, birch ? 0.12 : 0.24, birch ? 3.2 : 2.0, 5), birch ? 0xe9e6dc : pal.trunk, tr(0, birch ? 1.6 : 1.0, 0))];
    var blobs = birch ? [[0, 3.6, 0, 1.0], [0.4, 3.0, 0.2, 0.7]] : [[0, 2.9, 0, 1.55], [0.75, 2.4, 0.3, 1.05], [-0.6, 2.5, -0.35, 1.1], [0.1, 3.6, -0.1, 1.0]];
    blobs.forEach(function(b, i){ parts.push(prim(new THREE.IcosahedronGeometry(b[3], 0), pal.leaf[(i + v) % 3], tr(b[0], b[1], b[2], i*0.7, i*1.3, 0))); });
    return merge(parts);
  }
  function rockGeo(pal, seed){
    var g = new THREE.DodecahedronGeometry(1, 0).toNonIndexed(), p = g.attributes.position, rng = mulberry32(seed), cache = {};
    for(var i = 0; i < p.count; i++){
      var key = Math.round(p.getX(i)*100) + "," + Math.round(p.getY(i)*100) + "," + Math.round(p.getZ(i)*100);
      var k = cache[key] || (cache[key] = 0.75 + rng()*0.5);
      p.setXYZ(i, p.getX(i)*k*1.2, p.getY(i)*k*0.75, p.getZ(i)*k);
    }
    return prim(g, pal.rock, tr(0, 0.25, 0), function(x, y, z, ny){ return COL.setHex(pal.rock).multiplyScalar(0.82 + ny*0.22); });
  }
  function tuftGeo(pal){
    var parts = [];
    for(var i = 0; i < 5; i++){ var a = i*1.26; parts.push(prim(new THREE.ConeGeometry(0.07, 0.55 + (i % 3)*0.15, 3), pal.tuft, tr(Math.cos(a)*0.12, 0.25, Math.sin(a)*0.12, Math.cos(a)*0.35, 0, Math.sin(a)*0.35))); }
    return merge(parts);
  }
  function instanced(geo, mat, list, place, cast){
    var im = new THREE.InstancedMesh(geo, mat, list.length);
    for(var i = 0; i < list.length; i++){ place(list[i], MAT4, i); im.setMatrixAt(i, MAT4); }
    im.instanceMatrix.needsUpdate = true; im.castShadow = !!cast && !low; im.receiveShadow = false;
    im.computeBoundingSphere(); im.frustumCulled = true;
    return im;
  }
  // put instances into spatial buckets so whole buckets are culled by distance and frustum
  function bucketed(list, size){
    var B = {};
    list.forEach(function(o){ var k = Math.floor(o.x/size) + "," + Math.floor(o.z/size); (B[k] = B[k] || []).push(o); });
    return Object.keys(B).map(function(k){ return B[k]; });
  }
  var decoBuckets = [];               // {mesh, x, z, far}
  function addBucket(mesh, list, far, near){
    var x = 0, z = 0; list.forEach(function(o){ x += o.x; z += o.z; });
    world.add(mesh); decoBuckets.push({mesh: mesh, x: x/list.length, z: z/list.length, far: far, near: near || 0});
  }
  function buildScenery(st, pal){
    var sc = st.scenery, snowy = st.biome === "snow";
    var treeGeos = [[coniferGeo(pal, snowy, 0), coniferGeo(pal, snowy, 1)], [roundGeo(pal, 0, false), roundGeo(pal, 1, false)], [roundGeo(pal, 2, true)]];
    // near: the detailed trees (and their shadows); far: one simple shape per tree
    var farGeos = [
      merge([prim(new THREE.ConeGeometry(2.0, 4.6, 6), pal.conifer[1], tr(0, 2.6, 0), snowy ? function(x, y, z, ny){ return y > 3.4 ? COL.set(0xf4f7fb) : null; } : null)]),
      merge([prim(new THREE.CylinderGeometry(0.2, 0.25, 2.0, 4), pal.trunk, tr(0, 1.0, 0)), prim(new THREE.IcosahedronGeometry(1.85, 0), pal.leaf[1], tr(0, 2.95, 0))]),
      merge([prim(new THREE.CylinderGeometry(0.1, 0.12, 3.2, 4), 0xe9e6dc, tr(0, 1.6, 0)), prim(new THREE.IcosahedronGeometry(1.1, 0), pal.leaf[0], tr(0, 3.5, 0))])];
    var NEAR = low ? 120 : 175, FAR = low ? 300 : 430;
    function placeTree(o, m){ EUL.set(0, o.r, 0); Q.setFromEuler(EUL); m.compose(TMP.set(o.x, o.y - 0.15, o.z), Q, TMP2.set(o.s, o.s*(0.9 + o.v*0.3), o.s)); }
    bucketed(sc.trees, 180).forEach(function(list){
      [0, 1, 2].forEach(function(t){
        var all = list.filter(function(o){ return o.t === t; }); if(!all.length) return;
        var variants = treeGeos[t];
        variants.forEach(function(geo, vi){
          var L = all.filter(function(o){ return Math.floor(o.v*variants.length) === vi; });
          if(L.length) addBucket(instanced(geo, M.flat, L, placeTree, true), L, NEAR);
        });
        addBucket(instanced(farGeos[t], M.flat, all, placeTree, false), all, FAR, NEAR);
      });
    });
    var rockGeos = [rockGeo(pal, 11), rockGeo(pal, 23), rockGeo(pal, 37)];
    bucketed(sc.rocks, 260).forEach(function(list){
      rockGeos.forEach(function(geo, vi){
        var L = list.filter(function(o){ return Math.floor(o.v*3) === vi; }); if(!L.length) return;
        addBucket(instanced(geo, M.flat, L, function(o, m){ EUL.set(o.v*0.4, o.r, 0); Q.setFromEuler(EUL); m.compose(TMP.set(o.x, o.y - o.s*0.2, o.z), Q, TMP2.set(o.s, o.s, o.s)); }, true), L, 520);
      });
    });
    var tg = tuftGeo(pal);
    bucketed(sc.tufts, 200).forEach(function(list){
      addBucket(instanced(tg, M.flat, list, function(o, m){ EUL.set(0, o.r, 0); Q.setFromEuler(EUL); m.compose(TMP.set(o.x, o.y - 0.05, o.z), Q, TMP2.set(o.s, o.s, o.s)); }, false), list, 150);
    });
    // fences: posts and two rails between neighbours
    if(sc.fences.length){
      var post = prim(new THREE.BoxGeometry(0.14, 1.2, 0.14), 0x8a6a48, tr(0, 0.6, 0)), rail = prim(new THREE.BoxGeometry(1, 0.09, 0.05), 0x9d7b55, null);
      var rails = [];
      for(var i = 0; i < sc.fences.length - 1; i++){
        var a = sc.fences[i], b = sc.fences[i + 1], dx = b.x - a.x, dz = b.z - a.z, d = Math.sqrt(dx*dx + dz*dz);
        if(d > 4.5) continue;
        [0.55, 0.95].forEach(function(hh){ rails.push({x: (a.x + b.x)/2, z: (a.z + b.z)/2, y: (a.y + b.y)/2 + hh, len: d, h: Math.atan2(dx, dz), tilt: Math.atan2(b.y - a.y, d)}); });
      }
      bucketed(sc.fences, 260).forEach(function(list){ addBucket(instanced(post, M.flat, list, function(o, m){ m.compose(TMP.set(o.x, o.y - 0.1, o.z), Q.identity(), TMP2.set(1, 1, 1)); }, true), list, 400); });
      bucketed(rails, 260).forEach(function(list){ addBucket(instanced(rail, M.flat, list, function(o, m){ EUL.set(0, o.h + Math.PI/2, o.tilt, "YXZ"); Q.setFromEuler(EUL); m.compose(TMP.set(o.x, o.y - 0.1, o.z), Q, TMP2.set(o.len, 1, 1)); }, true), list, 400); });
    }
    // chevron boards
    var chevTex = [1, -1].map(function(dir){
      return canvasTex(128, 64, function(g, w, h){
        g.fillStyle = "#f4c21a"; g.fillRect(0, 0, w, h); g.fillStyle = "#16181c";
        for(var k = 0; k < 3; k++){
          var x = 18 + k*36; g.beginPath();
          if(dir > 0){ g.moveTo(x, 8); g.lineTo(x + 20, 32); g.lineTo(x, 56); g.lineTo(x + 12, 56); g.lineTo(x + 32, 32); g.lineTo(x + 12, 8); }
          else { g.moveTo(x + 32, 8); g.lineTo(x + 12, 32); g.lineTo(x + 32, 56); g.lineTo(x + 20, 56); g.lineTo(x, 32); g.lineTo(x + 20, 8); }
          g.closePath(); g.fill();
        }
        g.strokeStyle = "#16181c"; g.lineWidth = 4; g.strokeRect(2, 2, w - 4, h - 4);
      });
    });
    var board = new THREE.PlaneGeometry(1.3, 0.65), postG = prim(new THREE.CylinderGeometry(0.05, 0.05, 1.1, 5), 0x5b5f66, tr(0, 0.55, 0));
    [1, -1].forEach(function(dir, di){
      var L = sc.chevrons.filter(function(c){ return c.dir === dir; }); if(!L.length) return;
      var mat = new THREE.MeshLambertMaterial({map: chevTex[di], side: THREE.DoubleSide});
      // face the oncoming driver: the board's normal points back down the road
      addBucket(instanced(board, mat, L, function(o, m){ EUL.set(0, -o.h, 0); Q.setFromEuler(EUL); m.compose(TMP.set(o.x, o.y + 1.35, o.z), Q, TMP2.set(1, 1, 1)); }, true), L, 500);
      addBucket(instanced(postG, M.flat, L, function(o, m){ m.compose(TMP.set(o.x, o.y + 0.25, o.z), Q.identity(), TMP2.set(1, 1, 1)); }, true), L, 500);
    });
    // gate flags (small, in the stage colours; the finish has its arch)
    if(sc.flags.length){
      var flagG = merge([prim(new THREE.CylinderGeometry(0.035, 0.035, 2.2, 5), 0xe8e8e8, tr(0, 1.1, 0)),
                         prim(new THREE.BoxGeometry(0.02, 0.42, 0.62), 0xe0533d, tr(0, 1.95, 0.31))]);
      addBucket(instanced(flagG, M.flat, sc.flags, function(o, m){ EUL.set(0, -o.h, 0); Q.setFromEuler(EUL); m.compose(TMP.set(o.x, o.y, o.z), Q, TMP2.set(1, 1, 1)); }, true), sc.flags, 500);
    }
    // sponsor banners (original names) on two posts
    var banTex = SPONSORS.map(function(sp, i){
      var cols = [["#1d3b6e", "#ffd34d"], ["#c4372b", "#ffffff"], ["#1f6b45", "#f4efe3"], ["#f0b23a", "#1f2a44"], ["#3b2d5e", "#ff9f6b"], ["#e8e4da", "#2a5d8f"], ["#111418", "#7fe0a0"], ["#7a2e2e", "#ffe6a8"]][i % 8];
      return canvasTex(512, 96, function(g, w, h){
        g.fillStyle = cols[0]; g.fillRect(0, 0, w, h); g.fillStyle = cols[1];
        g.font = "900 52px sans-serif"; g.textAlign = "center"; g.textBaseline = "middle";
        g.fillText(sp[0], w/2 - 40, h/2 + 2);
        g.font = "700 26px sans-serif"; g.fillText(sp[1], w - 70, h/2 + 3);
        g.fillRect(0, 0, w, 6); g.fillRect(0, h - 6, w, 6);
      });
    });
    var banG = new THREE.PlaneGeometry(6, 1.1), banPost = prim(new THREE.CylinderGeometry(0.06, 0.06, 2.0, 5), 0x6b6f75, tr(0, 1.0, 0));
    sc.banners.forEach(function(b){
      var m = new THREE.Mesh(banG, new THREE.MeshLambertMaterial({map: banTex[b.v % banTex.length], side: THREE.DoubleSide}));
      // along the road, its face turned toward the road
      m.position.set(b.x, b.y + 1.4, b.z); m.rotation.y = -b.h - b.side*Math.PI/2; m.castShadow = !low; world.add(m); R.deco.push(m);
      [-2.8, 2.8].forEach(function(o){
        var p = new THREE.Mesh(banPost, M.flat), dx = Math.sin(b.h)*o, dz = -Math.cos(b.h)*o;
        p.position.set(b.x + dx, b.y + 0.3, b.z + dz); world.add(p); R.deco.push(p);
      });
    });
    // start / finish arches
    sc.arches.forEach(function(a){
      var g = new THREE.Group(), w = a.w;
      var tex = canvasTex(512, 96, function(c, W, H){
        for(var k = 0; k < W; k += 24){ c.fillStyle = (k/24) % 2 ? "#111" : "#f4f4f0"; c.fillRect(k, 0, 24, 14); c.fillStyle = (k/24) % 2 ? "#f4f4f0" : "#111"; c.fillRect(k, H - 14, 24, 14); }
        c.fillStyle = a.kind === "finish" ? "#c4372b" : "#1d3b6e"; c.fillRect(0, 14, W, H - 28);
        c.fillStyle = "#ffffff"; c.font = "900 50px sans-serif"; c.textAlign = "center"; c.textBaseline = "middle"; c.fillText(a.kind === "finish" ? "FINISH" : "START", W/2, H/2 + 2);
      });
      var beam = new THREE.Mesh(new THREE.BoxGeometry(w*2 + 0.6, 1.2, 0.35), [M.dark, M.dark, M.dark, M.dark, new THREE.MeshLambertMaterial({map: tex}), new THREE.MeshLambertMaterial({map: tex})]);
      beam.position.y = 5.0; beam.castShadow = !low; g.add(beam);
      [-1, 1].forEach(function(sd){ var t = new THREE.Mesh(new THREE.BoxGeometry(0.5, 5.6, 0.5), M.dark); t.position.set(sd*w, 2.8, 0); t.castShadow = !low; g.add(t); });
      g.position.set(a.x, a.y - 0.2, a.z); g.rotation.y = -a.h; world.add(g); R.deco.push(g);
    });
    // the water tower
    if(sc.tower){
      var t = sc.tower, tg2 = new THREE.Group();
      [[-1, -1], [1, -1], [-1, 1], [1, 1]].forEach(function(l){ var leg = new THREE.Mesh(new THREE.CylinderGeometry(0.18, 0.24, 9, 6), M.metal); leg.position.set(l[0]*1.7, 4.5, l[1]*1.7); leg.rotation.set(l[1]*0.06, 0, -l[0]*0.06); leg.castShadow = !low; tg2.add(leg); });
      var tank = new THREE.Mesh(new THREE.CylinderGeometry(3.0, 3.0, 4.2, 12), new THREE.MeshLambertMaterial({color: 0xb9c2c7, flatShading: true})); tank.position.y = 11; tank.castShadow = !low; tg2.add(tank);
      var roof = new THREE.Mesh(new THREE.ConeGeometry(3.3, 1.8, 12), new THREE.MeshLambertMaterial({color: 0x8e3b2e, flatShading: true})); roof.position.y = 14; roof.castShadow = !low; tg2.add(roof);
      tg2.position.set(t.x, t.y - 0.3, t.z); world.add(tg2); R.deco.push(tg2);
    }
  }
  function updateDeco(cx, cz){
    for(var i = 0; i < decoBuckets.length; i++){
      var b = decoBuckets[i], dx = b.x - cx, dz = b.z - cz, d2 = dx*dx + dz*dz;
      b.mesh.visible = d2 < b.far*b.far*(low ? 0.6 : 1) && d2 >= b.near*b.near;
    }
  }

  /* ---------- setStage ---------- */
  R.setStage = function(st){
    R.clearStage();
    R.stage = st;
    var pal = R.pal = BIOME[st.biome] || BIOME.forest;
    M.terrain = new THREE.MeshLambertMaterial({vertexColors: true, flatShading: true});
    M.flat = new THREE.MeshLambertMaterial({vertexColors: true, flatShading: true});
    M.dark = new THREE.MeshLambertMaterial({color: 0x2b2f36});
    M.metal = new THREE.MeshLambertMaterial({color: 0x8d969c, flatShading: true});
    var fogC = st.night ? 0x0d1428 : pal.fog;
    scene.fog = new THREE.Fog(fogC, st.night ? 25 : 140, st.night ? 260 : (low ? 420 : 560));
    scene.background = new THREE.Color(fogC);
    hemi.color.setHex(st.night ? 0x3a4d7a : pal.hemiSky); hemi.groundColor.setHex(st.night ? 0x121820 : pal.hemiGround);
    hemi.intensity = st.night ? 0.32 : pal.hemiI;
    sun.color.setHex(st.night ? 0x9fb4ff : pal.sun); sun.intensity = st.night ? 0.32 : pal.sunI;
    // a low sun for long soft shadows, from a seeded side of the stage
    var r = mulberry32(hashStr("sun|" + st.seed)), az = r()*Math.PI*2, el = st.night ? 0.6 : 0.42 + r()*0.18;
    sunDir.set(Math.cos(az)*Math.cos(el), Math.sin(el), Math.sin(az)*Math.cos(el));
    buildSky(st, pal);
    buildTerrain(st, pal);
    buildRoad(st);
    buildScenery(st, pal);
    fxInit();
  };
  R.clearStage = function(){
    world.children.slice().forEach(function(o){ world.remove(o); disposeTree(o); });
    R.chunks = []; R.deco = []; decoBuckets = []; R.stage = null;
    fxClear();
  };
  function disposeTree(o){
    o.traverse(function(n){
      if(n.geometry) n.geometry.dispose();
      if(n.material){ (Array.isArray(n.material) ? n.material : [n.material]).forEach(function(m){ if(m.map) m.map.dispose(); m.dispose(); }); }
    });
  }

  /* ---------- cars ---------- */
  function plateTex(text){
    return canvasTex(256, 64, function(g, w, h){
      g.fillStyle = "#f2f0e6"; g.fillRect(0, 0, w, h); g.strokeStyle = "#1c1f24"; g.lineWidth = 6; g.strokeRect(3, 3, w - 6, h - 6);
      g.fillStyle = "#1c1f24"; g.font = "800 40px monospace"; g.textAlign = "center"; g.textBaseline = "middle"; g.fillText(String(text).slice(0, 9), w/2, h/2 + 2);
    });
  }
  function doorTex(num, spec){
    return canvasTex(128, 128, function(g, w){
      g.fillStyle = "#ffffff"; g.beginPath(); g.arc(64, 64, 58, 0, Math.PI*2); g.fill();
      g.strokeStyle = spec.colors.stripe; g.lineWidth = 8; g.stroke();
      g.fillStyle = "#16181c"; g.font = "900 64px sans-serif"; g.textAlign = "center"; g.textBaseline = "middle"; g.fillText(String(num), 64, 68);
    });
  }
  // side profiles: [along from the nose (m), height (m)] for the lower body and the glasshouse
  var SHAPES = {
    moraine:   {len: 3.95, w: 1.74, body: [[0, 0.22], [0, 0.55], [0.12, 0.66], [0.95, 0.74], [3.65, 0.80], [3.95, 0.72], [3.95, 0.22]],
                glass: [[1.0, 0.74], [1.62, 1.22], [3.2, 1.24], [3.72, 0.82]], wing: [3.15, 1.30, 0.42], arch: 0.06},
    saltcedar: {len: 4.25, w: 1.70, body: [[0, 0.22], [0, 0.52], [0.10, 0.62], [1.35, 0.70], [3.05, 0.76], [3.25, 0.80], [4.2, 0.80], [4.25, 0.70], [4.25, 0.22]],
                glass: [[1.4, 0.70], [2.0, 1.12], [2.95, 1.13], [3.4, 0.80]], wing: [4.05, 0.98, 0.32], arch: 0.04},
    pipit:     {len: 3.70, w: 1.66, body: [[0, 0.22], [0, 0.58], [0.10, 0.68], [0.75, 0.76], [3.5, 0.80], [3.70, 0.74], [3.70, 0.22]],
                glass: [[0.8, 0.76], [1.42, 1.25], [3.22, 1.26], [3.58, 0.84]], wing: [3.30, 1.32, 0.28], arch: 0.05}
  };
  function extrudeProfile(pts2, width, mat, bevel){
    var sh = new THREE.Shape();
    sh.moveTo(pts2[0][0], pts2[0][1]); for(var i = 1; i < pts2.length; i++) sh.lineTo(pts2[i][0], pts2[i][1]); sh.closePath();
    var g = new THREE.ExtrudeGeometry(sh, {depth: width, bevelEnabled: !!bevel, bevelThickness: bevel || 0, bevelSize: bevel || 0, bevelSegments: 1, curveSegments: 1});
    // shape x (along from nose) -> -z ... : nose at z = -len/2
    g.translate(0, 0, -width/2); g.rotateY(-Math.PI/2);
    return new THREE.Mesh(g, mat);
  }
  // One draw call per material: merge a group's untextured meshes that share a material
  // (a car is ~60 boxes; drawn one by one, twice with shadows, that is a lot of calls).
  function bake(group){
    group.updateMatrixWorld(true);
    var inv = new THREE.Matrix4().copy(group.matrixWorld).invert(), by = {}, order = [];
    group.traverse(function(n){
      if(!n.isMesh || n.material.map || Array.isArray(n.material)) return;
      var k = n.material.uuid; if(!by[k]){ by[k] = {mat: n.material, list: []}; order.push(k); }
      by[k].list.push(n);
    });
    order.forEach(function(k){
      var e = by[k]; if(e.list.length < 2) return;
      var geos = e.list.map(function(n){
        var g = n.geometry.index ? n.geometry.toNonIndexed() : n.geometry.clone();
        Object.keys(g.attributes).forEach(function(a){ if(a !== "position" && a !== "normal") g.deleteAttribute(a); });
        g.applyMatrix4(new THREE.Matrix4().multiplyMatrices(inv, n.matrixWorld));
        return g;
      });
      var n = 0; geos.forEach(function(g){ n += g.attributes.position.count; });
      var pos = new Float32Array(n*3), nor = new Float32Array(n*3), o = 0;
      geos.forEach(function(g){ pos.set(g.attributes.position.array, o*3); nor.set(g.attributes.normal.array, o*3); o += g.attributes.position.count; g.dispose(); });
      var mg = new THREE.BufferGeometry(); mg.setAttribute("position", new THREE.BufferAttribute(pos, 3)); mg.setAttribute("normal", new THREE.BufferAttribute(nor, 3));
      mg.computeBoundingSphere();
      e.list.forEach(function(m){ if(m.parent) m.parent.remove(m); });
      group.add(new THREE.Mesh(mg, e.mat));
    });
    // drop the now-empty helper groups
    group.children.slice().forEach(function(c){ if(!c.isMesh && !c.isLight && c.children.length === 0 && !c.isSprite) group.remove(c); });
  }
  R.addCar = function(spec, o){
    o = o || {};
    var S = SHAPES[spec.id] || SHAPES.moraine, cs = spec.colors || {};
    var ghost = !!o.ghost;
    function mat(c, extra){
      var p = {color: c, flatShading: true}; for(var k in (extra || {})) p[k] = extra[k];
      if(ghost){ p.transparent = true; p.opacity = 0.38; p.depthWrite = false; }
      return new THREE.MeshLambertMaterial(p);
    }
    var paint = mat(o.color || cs.body), stripe = mat(cs.stripe), stripe2 = mat(cs.stripe2), glass = mat(cs.glass || 0x1d2733), black = mat(0x16181c), rubber = mat(0x1b1d20), rim = mat(cs.rim || 0xd0d0d0);
    var lampMat = mat(0xfff6d8, {emissive: 0x000000}), tailMat = mat(0x8c1a12, {emissive: 0x000000}), podMat = mat(0xfff2c0, {emissive: 0x000000});
    var car = new THREE.Group(), body = new THREE.Group(); car.add(body);
    // body frame: physics hardpoints sit at y = 0 (the CG); the ground is ~0.52 m below at rest
    var base = -0.52, half = S.len/2, nose = -(spec.a + (S.len - spec.a - spec.b)/2);
    var bodyMesh = extrudeProfile(S.body, S.w - 0.08, paint, 0.05); bodyMesh.position.set(0, base, nose); body.add(bodyMesh);
    var gl = extrudeProfile(S.glass, S.w*0.84, glass, 0.02); gl.position.set(0, base, nose); body.add(gl);
    // roof panel and pillars in paint over the glass
    var g0 = S.glass, roofLen = g0[2][0] - g0[1][0];
    var roof = new THREE.Mesh(new THREE.BoxGeometry(S.w*0.86, 0.07, roofLen + 0.08), paint); roof.position.set(0, base + g0[1][1] + 0.02, nose + (g0[1][0] + g0[2][0])/2); body.add(roof);
    [-1, 1].forEach(function(sd){
      var bp = new THREE.Mesh(new THREE.BoxGeometry(0.05, (g0[1][1] - g0[0][1]) + 0.02, 0.14), paint); bp.position.set(sd*S.w*0.425, base + (g0[0][1] + g0[1][1])/2, nose + (g0[1][0] + g0[2][0])/2); body.add(bp);
      // livery: a stripe down each side and a door number roundel
      var st1 = new THREE.Mesh(new THREE.BoxGeometry(0.02, 0.1, S.len*0.82), stripe); st1.position.set(sd*(S.w/2 + 0.045), base + 0.55, nose + S.len*0.5); body.add(st1);
      var st2 = new THREE.Mesh(new THREE.BoxGeometry(0.02, 0.05, S.len*0.82), stripe2); st2.position.set(sd*(S.w/2 + 0.046), base + 0.47, nose + S.len*0.5); body.add(st2);
      if(!ghost){
        var dm = new THREE.Mesh(new THREE.CircleGeometry(0.22, 18), new THREE.MeshLambertMaterial({map: doorTex(o.number || 7, spec)}));
        dm.position.set(sd*(S.w/2 + 0.05), base + 0.72, nose + S.len*0.46); dm.rotation.y = sd*Math.PI/2; body.add(dm);
      }
      // a dark sill along the bottom of each side
      var sill = new THREE.Mesh(new THREE.BoxGeometry(0.03, 0.12, S.len*0.7), black); sill.position.set(sd*(S.w/2 + 0.04), base + 0.3, nose + S.len*0.5); body.add(sill);
      // wheel-arch flares
      [spec.a, -spec.b].forEach(function(az){ var fl = new THREE.Mesh(new THREE.BoxGeometry(0.16, 0.14, 0.92), paint); fl.position.set(sd*(S.w/2 + 0.04), base + 0.66, -az); body.add(fl); });
      // mud flaps behind each wheel
      [spec.a, -spec.b].forEach(function(az){ var mf = new THREE.Mesh(new THREE.BoxGeometry(0.3, 0.3, 0.02), black); mf.position.set(sd*(spec.track/2 - 0.02), base + 0.25, -az + 0.48); body.add(mf); });
      // head and tail lamps
      var hl = new THREE.Mesh(new THREE.BoxGeometry(0.34, 0.13, 0.04), lampMat); hl.position.set(sd*0.55, base + 0.6, nose - 0.05); body.add(hl);
      var tl = new THREE.Mesh(new THREE.BoxGeometry(0.3, 0.12, 0.04), tailMat); tl.position.set(sd*0.6, base + 0.64, nose + S.len + 0.05); body.add(tl);
    });
    // grille, bumpers, the rally light pod
    var grille = new THREE.Mesh(new THREE.BoxGeometry(0.62, 0.14, 0.03), black); grille.position.set(0, base + 0.58, nose - 0.05); body.add(grille);
    [nose - 0.05, nose + S.len + 0.05].forEach(function(z){ var bb = new THREE.Mesh(new THREE.BoxGeometry(S.w + 0.06, 0.15, 0.14), black); bb.position.set(0, base + 0.29, z); body.add(bb); });
    var pod = new THREE.Group();
    var bar = new THREE.Mesh(new THREE.BoxGeometry(1.1, 0.05, 0.05), black); pod.add(bar);
    [-0.42, -0.14, 0.14, 0.42].forEach(function(x){
      var cup = new THREE.Mesh(new THREE.CylinderGeometry(0.11, 0.11, 0.09, 12), black); cup.rotation.x = Math.PI/2; cup.position.set(x, 0, -0.03); pod.add(cup);
      var lens = new THREE.Mesh(new THREE.CircleGeometry(0.09, 12), podMat); lens.position.set(x, 0, -0.08); lens.rotation.y = Math.PI; pod.add(lens);
    });
    pod.position.set(0, base + 0.5, nose - 0.12); body.add(pod);
    // number plates (an original name)
    if(!ghost){
      var pm = new THREE.MeshLambertMaterial({map: plateTex(spec.plate || "RALLY")});
      var pf = new THREE.Mesh(new THREE.PlaneGeometry(0.5, 0.12), pm); pf.position.set(0, base + 0.3, nose - 0.085); pf.rotation.y = Math.PI; body.add(pf);
      var pr = new THREE.Mesh(new THREE.PlaneGeometry(0.5, 0.12), pm); pr.position.set(0, base + 0.34, nose + S.len + 0.085); body.add(pr);
    }
    // the wing
    var wing = S.wing, wg = new THREE.Mesh(new THREE.BoxGeometry(S.w*0.92, 0.04, wing[2]), stripe); wg.position.set(0, base + wing[1], nose + wing[0] + wing[2]/2); body.add(wg);
    [-0.6, 0.6].forEach(function(x){ var sp = new THREE.Mesh(new THREE.BoxGeometry(0.04, 0.12, 0.12), black); sp.position.set(x, base + wing[1] - 0.06, nose + wing[0] + wing[2]/2); body.add(sp); });
    // roof vent / scoop
    var vent = new THREE.Mesh(new THREE.BoxGeometry(0.3, 0.06, 0.3), black); vent.position.set(0, base + g0[1][1] + 0.07, nose + g0[1][0] + 0.45); body.add(vent);
    bake(body);
    body.traverse(function(n){ if(n.isMesh){ n.castShadow = !ghost && !low; n.receiveShadow = false; } });
    // wheels: suspension (y) > steer (y rot) > spin (x rot)
    var wheels = [];
    var tyreG = new THREE.CylinderGeometry(spec.radius, spec.radius, 0.26, 16); tyreG.rotateZ(Math.PI/2);
    var rimG = new THREE.CylinderGeometry(spec.radius*0.62, spec.radius*0.62, 0.25, 10); rimG.rotateZ(Math.PI/2);
    var spokeG = new THREE.BoxGeometry(0.26, spec.radius*1.15, 0.05);
    [[-1, 1], [1, 1], [-1, -1], [1, -1]].forEach(function(w){
      var sus = new THREE.Group(), steer = new THREE.Group(), spin = new THREE.Group();
      // drawn a touch outboard of the physics track so the tyres fill the arches
      sus.position.set(w[0]*(spec.track/2 + 0.09), 0, w[1] > 0 ? -spec.a : spec.b);
      var tyre = new THREE.Mesh(tyreG, rubber), rm = new THREE.Mesh(rimG, rim);
      spin.add(tyre); spin.add(rm);
      for(var k = 0; k < 3; k++){ var sp2 = new THREE.Mesh(spokeG, black); sp2.rotation.x = k*Math.PI/3; spin.add(sp2); }
      spin.traverse(function(n){ if(n.isMesh) n.castShadow = !ghost && !low; });
      bake(spin);
      steer.add(spin); sus.add(steer); car.add(sus);
      wheels.push({sus: sus, steer: steer, spin: spin});
    });
    var h = {g: car, body: body, wheels: wheels, spec: spec, ghost: ghost, lamps: [lampMat, podMat], tail: tailMat, lights: null, tag: null, mats: [paint, stripe, stripe2, glass, black, rubber, rim, lampMat, tailMat, podMat]};
    if(o.name){
      var tc = document.createElement("canvas"), tg = tc.getContext("2d"); tc.width = 256; tc.height = 64;
      tg.font = "700 30px sans-serif"; tg.textAlign = "center"; tg.textBaseline = "middle";
      var tw = Math.min(250, tg.measureText(o.name).width + 28);
      tg.fillStyle = "rgba(20,24,32,0.72)"; tg.fillRect(128 - tw/2, 8, tw, 48); tg.fillStyle = "#ffffff"; tg.fillText(String(o.name).slice(0, 18), 128, 33);
      var tt = new THREE.CanvasTexture(tc); tt.colorSpace = THREE.SRGBColorSpace;
      var spt = new THREE.Sprite(new THREE.SpriteMaterial({map: tt, depthTest: false, transparent: true})); spt.scale.set(2.6, 0.65, 1); spt.position.set(0, 1.6, 0); spt.renderOrder = 10;
      car.add(spt); h.tag = spt;
    }
    scene.add(car); R.cars.push(h);
    return h;
  };
  // headlights with real cones (for the player's car at night)
  R.carLights = function(h, on){
    if(on && !h.lights){
      var L = [];
      [-0.55, 0.55].forEach(function(x){
        var s = new THREE.SpotLight(0xfff1d0, 60, 120, 0.42, 0.55, 1.3); s.position.set(x, 0.1, -2.0);
        var t = new THREE.Object3D(); t.position.set(x*0.6, -0.9, -24); h.body.add(t); s.target = t; h.body.add(s); L.push(s);
      });
      var pod = new THREE.SpotLight(0xfff6e0, 45, 90, 0.75, 0.7, 1.4); pod.position.set(0, 0.0, -2.2);
      var pt = new THREE.Object3D(); pt.position.set(0, -1.2, -16); h.body.add(pt); pod.target = pt; h.body.add(pod); L.push(pod);
      // a faint visible cone in the air
      var coneG = new THREE.ConeGeometry(2.6, 14, 16, 1, true); coneG.translate(0, -7, 0); coneG.rotateX(Math.PI/2);   // apex at the lamp, opening forward (-z)
      var coneM = new THREE.MeshBasicMaterial({color: 0xfff3d6, transparent: true, opacity: 0.055, depthWrite: false, side: THREE.DoubleSide, blending: THREE.AdditiveBlending});
      [-0.55, 0.55].forEach(function(x){ var c = new THREE.Mesh(coneG, coneM); c.position.set(x, -0.05, -2.0); c.rotation.x = -0.05; h.body.add(c); L.push(c); });
      var tailL = new THREE.PointLight(0xff3020, 1.2, 6, 2); tailL.position.set(0, -0.2, 2.4); h.body.add(tailL); L.push(tailL);
      h.lights = L;
    }
    if(h.lights) h.lights.forEach(function(l){ l.visible = !!on; });
    h.lamps.forEach(function(m){ m.emissive.setHex(on ? 0xfff2c8 : 0x000000); m.emissiveIntensity = on ? 1.2 : 0; });
  };
  R.removeCar = function(h){
    if(!h) return; scene.remove(h.g);
    h.g.traverse(function(n){ if(n.geometry) n.geometry.dispose(); });
    h.mats.forEach(function(m){ m.dispose(); });
    R.cars = R.cars.filter(function(c){ return c !== h; });
  };
  // pose: {p, q} (physics body frame); car: {wheels: [{len, steer, ang, contact}], brake, spec}
  R.poseCar = function(h, pose, car){
    h.g.position.set(pose.p[0], pose.p[1], pose.p[2]);
    h.g.quaternion.set(pose.q[0], pose.q[1], pose.q[2], pose.q[3]);
    var spec = h.spec;
    for(var i = 0; i < 4; i++){
      var w = h.wheels[i], ws = car && car.wheels ? car.wheels[i] : null;
      var len = ws ? ws.len : spec.lfree - 0.1;
      w.sus.position.y = spec.hpY - len;
      w.steer.rotation.y = ws ? -ws.steer : 0;
      w.spin.rotation.x = ws ? -ws.ang : 0;
    }
    var brk = car && car.tel ? car.tel.brake > 0.1 : false;
    h.tail.emissive.setHex(brk ? 0xff2a12 : (h.lit ? 0x5a0c06 : 0x000000)); h.tail.emissiveIntensity = brk ? 1.4 : 1;
  };

  /* ---------- particles: dust, gravel, spray (one pooled Points with a small shader) ---------- */
  var FX = null, PMAX = low ? 700 : 1800;
  function fxInit(){
    fxClear();
    var g = new THREE.BufferGeometry();
    var pos = new Float32Array(PMAX*3), size = new Float32Array(PMAX), alpha = new Float32Array(PMAX), col = new Float32Array(PMAX*3);
    g.setAttribute("position", new THREE.BufferAttribute(pos, 3).setUsage(THREE.DynamicDrawUsage));
    g.setAttribute("aSize", new THREE.BufferAttribute(size, 1).setUsage(THREE.DynamicDrawUsage));
    g.setAttribute("aAlpha", new THREE.BufferAttribute(alpha, 1).setUsage(THREE.DynamicDrawUsage));
    g.setAttribute("aColor", new THREE.BufferAttribute(col, 3).setUsage(THREE.DynamicDrawUsage));
    g.boundingSphere = new THREE.Sphere(new THREE.Vector3(), 1e6);
    var fc = scene.fog ? scene.fog.color : new THREE.Color(0xffffff);
    var mat = new THREE.ShaderMaterial({
      uniforms: {uScale: {value: 400}, uFog: {value: new THREE.Vector3(fc.r, fc.g, fc.b)}, uFogNear: {value: scene.fog ? scene.fog.near : 100}, uFogFar: {value: scene.fog ? scene.fog.far : 500}},
      vertexShader: "attribute float aSize; attribute float aAlpha; attribute vec3 aColor; varying float vA; varying vec3 vC; varying float vD; uniform float uScale;" +
        "void main(){ vec4 mv = modelViewMatrix*vec4(position,1.0); vD = -mv.z; gl_PointSize = aSize*uScale/max(0.5,-mv.z); vA = aAlpha; vC = aColor; gl_Position = projectionMatrix*mv; }",
      fragmentShader: "varying float vA; varying vec3 vC; varying float vD; uniform vec3 uFog; uniform float uFogNear; uniform float uFogFar;" +
        "void main(){ vec2 d = gl_PointCoord - 0.5; float r = dot(d,d)*4.0; if(r > 1.0) discard; float a = vA*(1.0 - r)*(1.0 - r);" +
        "float f = clamp((vD - uFogNear)/(uFogFar - uFogNear), 0.0, 1.0); gl_FragColor = vec4(mix(vC, uFog, f), a*(1.0 - f*0.6)); }",
      transparent: true, depthWrite: false
    });
    var pts = new THREE.Points(g, mat); pts.frustumCulled = false; pts.renderOrder = 5; scene.add(pts);
    FX = {pts: pts, g: g, n: 0, list: [], next: 0,
          // tyre tracks: a ring of quads
          tr: null};
    for(var i = 0; i < PMAX; i++) FX.list.push({life: 0});
    var TM = low ? 1500 : 4000, tg = new THREE.BufferGeometry();
    var tpos = new Float32Array(TM*6*3), tcol = new Float32Array(TM*6*4);
    tg.setAttribute("position", new THREE.BufferAttribute(tpos, 3).setUsage(THREE.DynamicDrawUsage));
    tg.setAttribute("color", new THREE.BufferAttribute(tcol, 4).setUsage(THREE.DynamicDrawUsage));
    tg.boundingSphere = new THREE.Sphere(new THREE.Vector3(), 1e6);
    var tm = new THREE.MeshBasicMaterial({vertexColors: true, transparent: true, depthWrite: false, polygonOffset: true, polygonOffsetFactor: -4, polygonOffsetUnits: -4});
    var tmesh = new THREE.Mesh(tg, tm); tmesh.frustumCulled = false; tmesh.renderOrder = 2; scene.add(tmesh);
    FX.tr = {mesh: tmesh, g: tg, max: TM, at: 0, last: {}, dirty: false, count: 0};
  }
  function fxClear(){
    if(!FX) return;
    scene.remove(FX.pts); FX.g.dispose(); FX.pts.material.dispose();
    scene.remove(FX.tr.mesh); FX.tr.g.dispose(); FX.tr.mesh.material.dispose();
    FX = null;
  }
  // seeded jitter for the effects (mulberry32 like everything else here)
  var fxr = mulberry32(12345);
  function emit(x, y, z, vx, vy, vz, size, grow, life, color, alpha, grav){
    if(!FX) return;
    var p = FX.list[FX.next]; FX.next = (FX.next + 1) % PMAX;
    p.x = x; p.y = y; p.z = z; p.vx = vx; p.vy = vy; p.vz = vz; p.s = size; p.gr = grow; p.life = life; p.max = life; p.c = color; p.a = alpha; p.grav = grav;
  }
  var DUST = {1: 0xc9b38e, 2: 0xb28d64, 3: 0x8d9a5a, 4: 0x6b5236, 5: 0xf4f8fc, 6: 0xcdb894, 0: 0xb0b0b0};
  R.fx = function(car, dt, calmOn){
    if(!FX || !car || !car.wheels) return;
    var tel = car.tel, spd = tel.speed, i;
    var M2 = car.M, fwdx = -M2[2], fwdz = -M2[8];
    for(i = 0; i < 4; i++){
      var w = car.wheels[i];
      if(!w.contact) { FX.tr.last[i] = null; continue; }
      var sid = w.surf, loose = sid !== 0;
      var slip = Math.min(3, w.slip || 0), wheelSpeed = Math.abs(w.omega*car.spec.radius), ground = Math.abs(w.vx) + Math.abs(w.vy);
      // dust: loose surfaces always throw some; slip throws a lot
      var amount = (loose ? 0.35*Math.min(1, ground/16) : 0) + Math.max(0, slip - 1)*0.5 + (loose ? Math.abs(wheelSpeed - Math.abs(w.vx))*0.06 : 0);
      if(sid === 0 && slip < 1.3) amount = 0;
      if(calmOn) amount *= 0.4;
      var n2 = amount*dt*(low ? 30 : 70);
      while(n2 > 0){
        if(n2 < 1 && fxr() > n2) break;
        n2 -= 1;
        var c = DUST[sid] || 0xbbbbbb, big = sid === 5 ? 1.3 : 1;
        emit(w.cp[0] + (fxr() - 0.5)*0.4, w.cp[1] + 0.15, w.cp[2] + (fxr() - 0.5)*0.4,
             -fwdx*(1 + fxr()*2) + (fxr() - 0.5)*1.5, 0.6 + fxr()*1.4, -fwdz*(1 + fxr()*2) + (fxr() - 0.5)*1.5,
             (0.8 + fxr()*0.8)*big, 2.0 + fxr()*1.4, 1.6 + fxr()*1.8, c, sid === 0 ? 0.35 : 0.46, -0.25);
        // gravel chips / mud / snow lumps thrown back off a spinning or sliding wheel
        if(loose && (slip > 1 || wheelSpeed > Math.abs(w.vx) + 2) && fxr() < 0.6){
          var cc = sid === 4 ? 0x4e3a26 : sid === 5 ? 0xffffff : 0x6b5a45;
          emit(w.cp[0], w.cp[1] + 0.1, w.cp[2], -fwdx*(3 + fxr()*5) + (fxr() - 0.5)*2, 1.2 + fxr()*2.5, -fwdz*(3 + fxr()*5) + (fxr() - 0.5)*2,
               0.08 + fxr()*0.06, 0, 0.6 + fxr()*0.4, cc, 0.95, 9.8);
        }
      }
      // tyre tracks: a quad every ~0.3 m of travel
      var T = FX.tr, L = T.last[i], mark = loose ? 0.16 + Math.min(0.5, slip*0.18) : Math.max(0, slip - 1.1)*0.5;
      if(sid === 5) mark = 0.25 + Math.min(0.4, slip*0.15);
      var px = w.cp[0], pz = w.cp[2], py = w.cp[1] + 0.035;
      if(L){
        var dx = px - L.x, dz = pz - L.z, d = Math.sqrt(dx*dx + dz*dz);
        if(d > 0.3){
          if(mark > 0.03 && d < 3) addTrack(L, {x: px, y: py, z: pz}, mark, sid);
          T.last[i] = {x: px, y: py, z: pz};
        }
      } else T.last[i] = {x: px, y: py, z: pz};
    }
  };
  function addTrack(A, B, a, sid){
    var T = FX.tr, dx = B.x - A.x, dz = B.z - A.z, d = Math.sqrt(dx*dx + dz*dz) || 1, nx = -dz/d*0.11, nz = dx/d*0.11;
    var pos = T.g.attributes.position.array, col = T.g.attributes.color.array, o = T.at*18, oc = T.at*24;
    var v = [[A.x - nx, A.y, A.z - nz], [B.x - nx, B.y, B.z - nz], [A.x + nx, A.y, A.z + nz], [A.x + nx, A.y, A.z + nz], [B.x - nx, B.y, B.z - nz], [B.x + nx, B.y, B.z + nz]];
    var c = sid === 5 ? [0.62, 0.68, 0.76] : sid === 0 ? [0.08, 0.08, 0.09] : sid === 4 ? [0.24, 0.17, 0.1] : [0.36, 0.27, 0.18];
    for(var k = 0; k < 6; k++){ pos[o + k*3] = v[k][0]; pos[o + k*3 + 1] = v[k][1]; pos[o + k*3 + 2] = v[k][2]; col[oc + k*4] = c[0]; col[oc + k*4 + 1] = c[1]; col[oc + k*4 + 2] = c[2]; col[oc + k*4 + 3] = Math.min(0.7, a); }
    T.at = (T.at + 1) % T.max; T.count = Math.min(T.max, T.count + 1); T.dirty = true;
  }
  function fxUpdate(dt){
    if(!FX) return;
    var pos = FX.g.attributes.position.array, size = FX.g.attributes.aSize.array, alpha = FX.g.attributes.aAlpha.array, col = FX.g.attributes.aColor.array, n = 0;
    for(var i = 0; i < PMAX; i++){
      var p = FX.list[i];
      if(p.life <= 0) continue;
      p.life -= dt;
      if(p.life <= 0) continue;
      p.vy -= p.grav*dt; var drag = Math.exp(-dt*(p.grav > 1 ? 0.4 : 1.6)); p.vx *= drag; p.vz *= drag; if(p.grav <= 0) p.vy *= drag;
      p.x += p.vx*dt; p.y += p.vy*dt; p.z += p.vz*dt; p.s += p.gr*dt;
      var u = p.life/p.max;
      pos[n*3] = p.x; pos[n*3+1] = p.y; pos[n*3+2] = p.z; size[n] = p.s; alpha[n] = p.a*Math.min(1, u*2.2)*(p.grav > 1 ? 1 : Math.min(1, (1 - u)*6 + 0.2));
      COL.setHex(p.c); col[n*3] = COL.r; col[n*3+1] = COL.g; col[n*3+2] = COL.b;
      n++;
    }
    FX.g.setDrawRange(0, n);
    FX.g.attributes.position.needsUpdate = true; FX.g.attributes.aSize.needsUpdate = true; FX.g.attributes.aAlpha.needsUpdate = true; FX.g.attributes.aColor.needsUpdate = true;
    if(FX.tr.dirty){ FX.tr.g.attributes.position.needsUpdate = true; FX.tr.g.attributes.color.needsUpdate = true; FX.tr.g.setDrawRange(0, FX.tr.count*6); FX.tr.dirty = false; }
  }

  /* ---------- camera ---------- */
  var cam = {p: new THREE.Vector3(), v: new THREE.Vector3(), look: new THREE.Vector3(), init: false, shake: 0, t: 0, mode: -1, yaw: 0, h: 0};
  var F = new THREE.Vector3(), U = new THREE.Vector3(), RT = new THREE.Vector3(), V3 = new THREE.Vector3();
  // c: {mode 0 chase / 1 bumper / 2 hood / 3 orbit (after the finish), pose, car (physics), calm, dt}
  function updateCamera(c, dt){
    if(!c || !c.pose) return;
    var P = c.pose, q = Q.set(P.q[0], P.q[1], P.q[2], P.q[3]);
    F.set(0, 0, -1).applyQuaternion(q); U.set(0, 1, 0).applyQuaternion(q); RT.set(1, 0, 0).applyQuaternion(q);
    var car = c.car, spd = car && car.tel ? car.tel.speed : 0, vel = car ? V3.set(car.v[0], car.v[1], car.v[2]) : V3.set(0, 0, 0);
    cam.t += dt;
    if(c.mode !== cam.mode){ cam.init = false; cam.mode = c.mode; }
    var shakeAmt = 0;
    if(!c.calm && car && car.tel){ shakeAmt = Math.min(1, car.tel.rumble*Math.min(1, spd/25))*0.05 + (car.landing || 0)*0.12 + (car.hitT > 0 ? car.hit*0.15 : 0); }
    if(c.mode === 0 || c.mode === 3){
      // chase: a fixed distance behind the car; only its heading and height are smoothed
      // (faster the faster you go), so the car stays the same size on screen at any speed.
      // It swings toward the direction of travel in a slide (look-ahead), and mode 3 orbits.
      var fx = F.x, fz = F.z, fl = Math.sqrt(fx*fx + fz*fz) || 1; fx /= fl; fz /= fl;
      var vl = vel.length(), vx = vl > 3 ? vel.x/vl : fx, vz = vl > 3 ? vel.z/vl : fz;
      var want = Math.atan2(lerp(fx, vx, 0.35), -lerp(fz, vz, 0.35));
      // the finish camera swings slowly behind and beside the car (the road is clear of trees there)
      if(c.mode === 3) want = Math.atan2(fx, -fz) + 0.55 + (c.calm ? 0 : 0.55*Math.sin(cam.t*0.35));
      if(!cam.init || c.calm){ cam.yaw = want; cam.h = P.p[1]; cam.init = true; }
      else {
        var dy = Math.atan2(Math.sin(want - cam.yaw), Math.cos(want - cam.yaw));
        cam.yaw += dy*Math.min(1, dt*(c.mode === 3 ? 6 : 2.6 + 4*Math.min(1, spd/30)));
        cam.h += (P.p[1] - cam.h)*Math.min(1, dt*6);
      }
      var dist = c.mode === 3 ? 7.5 : 6.0 + Math.min(1.2, spd*0.03), height = c.mode === 3 ? 2.6 : 2.05;
      var cx2 = Math.sin(cam.yaw), cz2 = -Math.cos(cam.yaw);
      TMP.set(P.p[0] - cx2*dist, cam.h + height, P.p[2] - cz2*dist);
      if(R.stage){ var gh = R.stage.env.height(TMP.x, TMP.z) + 0.9; if(TMP.y < gh) TMP.y = gh; }
      cam.p.copy(TMP);
      var ahead = c.mode === 3 ? 0 : Math.min(8, spd*0.28);
      cam.look.set(P.p[0] + vx*ahead, P.p[1] + (c.mode === 3 ? 0.4 : 0.85), P.p[2] + vz*ahead);
      camera.position.copy(cam.p);
    } else {
      // bumper (1) or hood (2): fixed to the body
      // (offsets along the car's forward axis: the bumper sits past the nose, the hood over the front axle)
      var fa = car && car.spec ? car.spec.a : 1.1;
      var off2 = c.mode === 1 ? [0, -0.2, fa + 0.95] : [0, 0.5, fa*0.8];
      camera.position.set(P.p[0], P.p[1], P.p[2]).addScaledVector(RT, off2[0]).addScaledVector(U, off2[1]).addScaledVector(F, off2[2]);
      cam.look.copy(camera.position).addScaledVector(F, 20).addScaledVector(U, c.mode === 1 ? 0.5 : -0.1);
    }
    if(shakeAmt > 0){
      var t = cam.t*31;
      camera.position.x += Math.sin(t*1.7)*shakeAmt; camera.position.y += Math.sin(t*2.3 + 1)*shakeAmt; camera.position.z += Math.cos(t*1.9)*shakeAmt;
    }
    camera.up.set(0, 1, 0);
    if(c.mode === 1 || c.mode === 2){ camera.up.copy(U); }
    camera.lookAt(cam.look);
    var fov = c.calm ? 62 : 60 + Math.min(10, spd*0.2);
    if(c.mode === 1) fov += 6;
    if(Math.abs(camera.fov - fov) > 0.05){ camera.fov += (fov - camera.fov)*Math.min(1, dt*3); camera.updateProjectionMatrix(); }
    // the sun's shadow box follows the car
    sun.position.set(P.p[0] + sunDir.x*160, P.p[1] + sunDir.y*160, P.p[2] + sunDir.z*160); sun.target.position.set(P.p[0] + F.x*20, P.p[1], P.p[2] + F.z*20);
    if(sky) sky.position.copy(camera.position);
    if(stars) stars.position.copy(camera.position);
    if(moon) moon.position.set(camera.position.x + sunDir.x*1000, camera.position.y + 300, camera.position.z + sunDir.z*1000);
    updateChunks(P.p[0], P.p[2]);
    updateDeco(P.p[0], P.p[2]);
  }
  R.resetCamera = function(){ cam.init = false; };

  R.render = function(dt, c){
    updateCamera(c, dt);
    fxUpdate(dt);
    if(clouds.length && !(c && c.calm)) for(var i = 0; i < clouds.length; i++){ var cl = clouds[i]; cl.position.x += dt*1.6; if(cl.position.x > 1100) cl.position.x -= 2000; }
    renderer.render(scene, camera);
  };
  R.size = function(w, h){ renderer.setSize(w, h, false); camera.aspect = w/h; camera.updateProjectionMatrix(); };
  R.setPixelRatio = function(r){ renderer.setPixelRatio(r); };
  R.info = function(){
    var vis = {chunks: 0, fine: 0, deco: 0, inst: 0, other: 0};
    R.chunks.forEach(function(c){ if(c.coarse.visible) vis.chunks++; if(c.fine && c.fine.visible) vis.fine++; });
    decoBuckets.forEach(function(b){ if(b.mesh.visible) vis.inst++; });
    var meshes = 0; scene.traverseVisible(function(n){ if(n.isMesh || n.isPoints || n.isSprite) meshes++; });
    vis.meshes = meshes;
    return {calls: renderer.info.render.calls, tris: renderer.info.render.triangles, chunks: built, vis: vis};
  };
  R.dispose = function(){
    R.alive = false;
    R.cars.slice().forEach(R.removeCar);
    R.clearStage();
    if(sky){ scene.remove(sky); sky.geometry.dispose(); sky.material.dispose(); }
    renderer.dispose();
    try { var ext = renderer.getContext().getExtension("WEBGL_lose_context"); if(ext) ext.loseContext(); } catch(e){}
  };
  return R;
}

HQV.rallyScene = {create: create, SPONSORS: SPONSORS};
})(typeof window !== "undefined" ? window : this);
