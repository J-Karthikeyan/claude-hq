/* Valley Rally: the three.js scene. A classic script (window.HQV.rallyScene) that is handed
 * the vendored three.js module by games/rally.js, so it never imports anything itself.
 *
 *   var R = HQV.rallyScene.create(THREE, canvas, {quality: "low"|"medium"|"high", calm});
 *   R.setStage(stage)              terrain chunks, road ribbon, scenery, sky, lights, fog
 *   var h = R.addCar(spec, {ghost, name, number, color})   a procedural car
 *   R.poseCar(h, pose, car)        place it (pose from physics.pose(), car = physics car or a stand-in)
 *   R.fx(car, dt)                  dust, gravel, spray, tyre tracks and mud under a car
 *   R.render(dt, cam)              cam: {mode, car, pose, calm}
 *   R.setQuality(q); R.size(w, h); R.dispose()
 *
 * The look is a painted, cel-shaded one: toon materials with a few soft colour bands, dark
 * ink outlines (inverted hulls, a constant width on screen), crisp long shadows from a low
 * sun, a deep blue sky with painted clouds. Everything is generated here: geometry from
 * primitives, textures drawn on canvases (seeded with mulberry32, so a stage always looks
 * the same), no image files.
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

/* Quality levels: render scale, shadow map, how far the ink outlines reach, grass, effects. */
var QUALITY = {
  low:    {pr: 1,    shadow: 1024, shR: 34, ink: 0, grass: 45, parts: 700,  tracks: 1500, far: 0.62, near: 110},
  medium: {pr: 1.25, shadow: 2048, shR: 44, ink: 1, grass: 75, parts: 1400, tracks: 4000, far: 0.85, near: 160},
  high:   {pr: 1.75, shadow: 4096, shR: 56, ink: 2, grass: 105, parts: 1800, tracks: 6000, far: 1,    near: 190}
};

/* Biome palettes (scene colours, not UI: the 3D world has its own look in every theme).
 * Muted, natural, painterly: sage and olive greens, a warm tan road, a deep blue sky. */
var BIOME = {
  forest: {sky: [0x2458c4, 0x5a93dc, 0xc4dcef], fog: 0xbcd3e6, hemiSky: 0xbcd4f2, hemiGround: 0x5d6b47, sun: 0xfff0d8, sunI: 2.8, hemiI: 1.4,
           grass: [0x6b8a4a, 0x7a9852, 0x5e7c43], verge: 0x8aa35e, mud: 0x6f5a40, dirt: 0xb19a7b, loose: 0xc6b293, snow: 0xf2f6fa,
           conifer: [0x4d6e58, 0x557760, 0x46654f], leaf: [0x7e8c50, 0x8b9858, 0x6d7e47], trunk: 0x4b3a31, rock: 0x8e8b83, tuft: [0x6f8f45, 0xb9c27a]},
  hills:  {sky: [0x2d63c2, 0x6aa0dc, 0xe6dcc3], fog: 0xe0d6bf, hemiSky: 0xd6e2f2, hemiGround: 0x8e7a52, sun: 0xffe6bf, sunI: 2.9, hemiI: 1.35,
           grass: [0xb0a160, 0xbfae6b, 0x9f9356], verge: 0xc8b878, mud: 0x7a5f40, dirt: 0xc49c6c, loose: 0xd2b88f, snow: 0xf2f6fa,
           conifer: [0x58704a, 0x617a50, 0x506845], leaf: [0x8c974e, 0x98a257, 0x7d8a46], trunk: 0x5a4434, rock: 0xa59885, tuft: [0xa99d5c, 0xe0d39a]},
  snow:   {sky: [0x3a6cc4, 0x7aa4dc, 0xe2ebf4], fog: 0xdbe5f0, hemiSky: 0xd8e6fa, hemiGround: 0xaab8c8, sun: 0xfff4e6, sunI: 2.5, hemiI: 1.5,
           grass: [0xf3f6fa, 0xe8eef5, 0xdde6f0], verge: 0xeef3f8, mud: 0xb4bfca, dirt: 0xb9a991, loose: 0xc9ced4, snow: 0xf3f6fa,
           conifer: [0x3b5e4c, 0x426852, 0x355544], leaf: [0x56795a, 0x5f8462, 0x4d6e51], trunk: 0x463830, rock: 0x8c949b, tuft: [0xc9d2c4, 0xeef2f4]}
};
// surface ids (physics.js SURFACES): tarmac, gravel, dirt, grass, mud, snow, loose
var ROAD_COL = [0x5d6064, 0xb5a085, 0xae9373, 0x7f9a52, 0x6f5a40, 0xdfe6ee, 0xc3ad8c];
var INK = 0x1a1814;

function create(THREE, canvas, opts){
  opts = opts || {};
  var qName = QUALITY[opts.quality] ? opts.quality : opts.low ? "low" : "high", Qd = QUALITY[qName];
  var low = qName === "low";
  var renderer = new THREE.WebGLRenderer({canvas: canvas, antialias: qName !== "low", powerPreference: "high-performance"});
  renderer.setPixelRatio(Math.min(root.devicePixelRatio || 1, Qd.pr));
  renderer.outputColorSpace = THREE.SRGBColorSpace;
  renderer.toneMapping = THREE.NoToneMapping;           // flat painted colours, not filmic
  renderer.shadowMap.enabled = true; renderer.shadowMap.type = THREE.PCFShadowMap;
  var scene = new THREE.Scene(), camera = new THREE.PerspectiveCamera(58, 1.6, 0.1, 1500);
  var hemi = new THREE.HemisphereLight(0xffffff, 0x556644, 1.1); scene.add(hemi);
  var sun = new THREE.DirectionalLight(0xffffff, 3); scene.add(sun); scene.add(sun.target);
  sun.castShadow = true; sun.shadow.bias = -0.0004; sun.shadow.normalBias = 0.035; sun.shadow.radius = 1.5;
  function shadowSetup(){
    sun.shadow.mapSize.set(Qd.shadow, Qd.shadow);
    if(sun.shadow.map){ sun.shadow.map.dispose(); sun.shadow.map = null; }
    var shc = sun.shadow.camera; shc.left = -Qd.shR; shc.right = Qd.shR; shc.top = Qd.shR; shc.bottom = -Qd.shR; shc.near = 1; shc.far = 400; shc.updateProjectionMatrix();
  }
  shadowSetup();
  var sunDir = new THREE.Vector3(0.6, 0.45, 0.4).normalize();
  var world = new THREE.Group(); scene.add(world);
  var R = {renderer: renderer, scene: scene, camera: camera, stage: null, cars: [], chunks: [], deco: [], low: low, quality: qName, alive: true};
  var M = {};                                   // shared materials
  var TMP = new THREE.Vector3(), TMP2 = new THREE.Vector3(), Q = new THREE.Quaternion(), MAT4 = new THREE.Matrix4(), EUL = new THREE.Euler(), COL = new THREE.Color();
  var RES = {value: new THREE.Vector2(1280, 720)};          // drawing-buffer size, for the outline width in pixels

  /* ---------- toon shading and ink outlines ---------- */
  function ramp(vals){
    var d = new Uint8Array(vals.length*4);
    vals.forEach(function(v, i){ d[i*4] = d[i*4 + 1] = d[i*4 + 2] = Math.round(v*255); d[i*4 + 3] = 255; });
    var t = new THREE.DataTexture(d, vals.length, 1, THREE.RGBAFormat);
    t.minFilter = t.magFilter = THREE.NearestFilter; t.generateMipmaps = false; t.needsUpdate = true;
    return t;
  }
  // three soft bands for objects; the land is kept nearly flat, like a painted backdrop
  var RAMP_OBJ = ramp([0.30, 0.52, 0.80, 1.0]), RAMP_LAND = ramp([0.62, 0.74, 0.92, 1.0]);
  function toon(p, land){ p = p || {}; p.gradientMap = land ? RAMP_LAND : RAMP_OBJ; return new THREE.MeshToonMaterial(p); }
  // An ink outline: the mesh's back faces, pushed out along smooth normals by a fixed number
  // of pixels (thinner far away), drawn in ink. Works for plain and instanced meshes.
  function inkMat(px, farD){
    var m = new THREE.MeshBasicMaterial({color: INK, side: THREE.BackSide});
    var U = {uPx: {value: px}, uFarD: {value: farD || 30}};
    m.onBeforeCompile = function(sh){
      sh.uniforms.uPx = U.uPx; sh.uniforms.uFarD = U.uFarD; sh.uniforms.uRes = RES;
      sh.vertexShader = "uniform float uPx; uniform float uFarD; uniform vec2 uRes;\n" + sh.vertexShader.replace("#include <project_vertex>",
        "vec3 inkN = normal;\n#ifdef USE_INSTANCING\n inkN = mat3(instanceMatrix) * inkN;\n#endif\n" +
        "vec4 mvPosition = vec4(transformed, 1.0);\n#ifdef USE_INSTANCING\n mvPosition = instanceMatrix * mvPosition;\n#endif\n" +
        "mvPosition = modelViewMatrix * mvPosition;\n" +
        "vec3 inkV = normalize(mat3(modelViewMatrix) * inkN);\n" +
        "vec4 inkC = projectionMatrix * mvPosition;\n" +
        "vec2 inkS = (projectionMatrix * vec4(inkV, 0.0)).xy; float inkL = length(inkS); inkS = inkL > 1e-5 ? inkS/inkL : vec2(0.0);\n" +
        "float inkK = uPx * clamp(uFarD/max(1.0, -mvPosition.z), 0.35, 1.0);\n" +
        "inkC.xy += inkS * inkK * 2.0/uRes * inkC.w;\n" +
        "gl_Position = inkC;");
    };
    m.customProgramCacheKey = function(){ return "rally-ink"; };
    m.userData.U = U;
    return m;
  }
  // smooth normals for a hull: every corner gets the area-weighted mean of the faces at its position
  function hullGeo(g){
    var src = g.index ? g.toNonIndexed() : g, p = src.attributes.position.array, n = p.length/3, acc = {}, keys = new Array(n);
    var ax, ay, az, bx, by, bz, cx, cy, cz;
    for(var i = 0; i < n; i++) keys[i] = Math.round(p[i*3]*500) + "," + Math.round(p[i*3 + 1]*500) + "," + Math.round(p[i*3 + 2]*500);
    for(var t = 0; t + 2 < n; t += 3){
      ax = p[t*3]; ay = p[t*3 + 1]; az = p[t*3 + 2]; bx = p[t*3 + 3] - ax; by = p[t*3 + 4] - ay; bz = p[t*3 + 5] - az; cx = p[t*3 + 6] - ax; cy = p[t*3 + 7] - ay; cz = p[t*3 + 8] - az;
      var nx = by*cz - bz*cy, ny = bz*cx - bx*cz, nz = bx*cy - by*cx;
      for(var k = 0; k < 3; k++){ var a = acc[keys[t + k]] || (acc[keys[t + k]] = [0, 0, 0]); a[0] += nx; a[1] += ny; a[2] += nz; }
    }
    var nor = new Float32Array(n*3);
    for(i = 0; i < n; i++){ var v = acc[keys[i]] || [0, 1, 0], l = Math.sqrt(v[0]*v[0] + v[1]*v[1] + v[2]*v[2]) || 1; nor[i*3] = v[0]/l; nor[i*3 + 1] = v[1]/l; nor[i*3 + 2] = v[2]/l; }
    var out = new THREE.BufferGeometry();
    out.setAttribute("position", new THREE.BufferAttribute(new Float32Array(p), 3)); out.setAttribute("normal", new THREE.BufferAttribute(nor, 3));
    out.computeBoundingSphere();
    if(src !== g) src.dispose();
    return out;
  }

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
  // a primitive, made non-indexed, transformed, coloured (with an optional per-vertex tweak)
  function prim(g, color, tx, tweak){
    g = g.index ? g.toNonIndexed() : g;
    if(tx) g.applyMatrix4(tx);
    g.computeVertexNormals();
    var c = new THREE.Color(color), n = g.attributes.position.count, arr = new Float32Array(n*3), p = g.attributes.position.array, nm = g.attributes.normal.array;
    for(var i = 0; i < n; i++){
      var cc = c;
      if(tweak){ cc = tweak(p[i*3], p[i*3+1], p[i*3+2], nm[i*3+1], c, i) || c; }
      arr[i*3] = cc.r; arr[i*3+1] = cc.g; arr[i*3+2] = cc.b;
    }
    g.setAttribute("color", new THREE.BufferAttribute(arr, 3));
    if(g.attributes.uv) g.deleteAttribute("uv");
    return g;
  }
  // move every distinct vertex position by a seeded amount (keeps shared corners together)
  function jitter(g, amt, seed, sy){
    var p = g.attributes.position, r = mulberry32(seed), cache = {};
    for(var i = 0; i < p.count; i++){
      var key = Math.round(p.getX(i)*1000) + "," + Math.round(p.getY(i)*1000) + "," + Math.round(p.getZ(i)*1000);
      var d = cache[key] || (cache[key] = [(r() - 0.5)*amt, (r() - 0.5)*amt*(sy == null ? 1 : sy), (r() - 0.5)*amt]);
      p.setXYZ(i, p.getX(i) + d[0], p.getY(i) + d[1], p.getZ(i) + d[2]);
    }
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
    var t = new THREE.CanvasTexture(c); t.colorSpace = THREE.SRGBColorSpace; t.anisotropy = low ? 1 : 8;
    if(repeat){ t.wrapS = t.wrapT = THREE.RepeatWrapping; }
    return t;
  }
  function hexCss(c){ return "#" + ("000000" + (c >>> 0).toString(16)).slice(-6); }
  function shade(c, k){ var C = new THREE.Color(c); C.multiplyScalar(k); return "#" + C.getHexString(); }

  /* ---------- sky, painted clouds, stars ---------- */
  var sky = null, clouds = [], stars = null, moon = null, cloudTex = [], bands = [];
  function buildSky(st, pal){
    if(sky){ scene.remove(sky); sky.geometry.dispose(); sky.material.dispose(); }
    clouds.forEach(function(c){ scene.remove(c); c.material.dispose(); }); clouds = [];
    cloudTex.forEach(function(t){ t.dispose(); }); cloudTex = [];
    if(stars){ scene.remove(stars); stars.geometry.dispose(); stars.material.dispose(); stars = null; }
    if(moon){ scene.remove(moon); moon.material.map.dispose(); moon.material.dispose(); moon = null; }
    bands.forEach(function(b){ scene.remove(b); b.geometry.dispose(); b.material.map.dispose(); b.material.dispose(); }); bands = [];
    var top = new THREE.Color(st.night ? 0x040818 : pal.sky[0]), mid = new THREE.Color(st.night ? 0x0b1430 : pal.sky[1]), hor = new THREE.Color(st.night ? 0x1a2648 : pal.sky[2]);
    var g = new THREE.SphereGeometry(1300, 32, 20), p = g.attributes.position, col = new Float32Array(p.count*3);
    for(var i = 0; i < p.count; i++){
      var y = p.getY(i)/1300, u = clamp(y, 0, 1);
      if(u < 0.18) COL.copy(hor).lerp(mid, Math.pow(u/0.18, 0.8)); else COL.copy(mid).lerp(top, Math.pow((u - 0.18)/0.82, 0.7));
      if(y < 0) COL.copy(hor);
      col[i*3] = COL.r; col[i*3+1] = COL.g; col[i*3+2] = COL.b;
    }
    g.setAttribute("color", new THREE.BufferAttribute(col, 3));
    sky = new THREE.Mesh(g, new THREE.MeshBasicMaterial({vertexColors: true, side: THREE.BackSide, fog: false, depthWrite: false}));
    sky.renderOrder = -10; scene.add(sky);
    var rng = mulberry32(hashStr("sky|" + st.seed));
    // two painted treelines far off, the further one paler: depth in layers of haze
    var fogHex = st.night ? 0x0b1226 : pal.fog, treeHex = st.biome === "hills" ? pal.leaf[2] : pal.conifer[0];
    [[1000, 0.72, 70, 0.55], [880, 0.5, 60, 0.75]].forEach(function(L, li){
      var col = "#" + new THREE.Color(treeHex).lerp(new THREE.Color(fogHex), L[1]).getHexString();
      var tex = canvasTex(2048, 128, function(c2, w, h){
        c2.fillStyle = col; c2.beginPath(); c2.moveTo(0, h);
        var x = 0;
        while(x <= w){
          var tw = 6 + rng()*12, th = h*(0.35 + rng()*0.5)*(0.6 + 0.4*Math.sin(x/w*Math.PI*6 + li*2));
          if(st.biome === "hills"){ c2.lineTo(x, h - th*0.6); c2.quadraticCurveTo(x + tw, h - th*0.75, x + tw*2, h - th*0.6); x += tw*2; }
          else { c2.lineTo(x, h - th*0.45); c2.lineTo(x + tw*0.5, h - th); c2.lineTo(x + tw, h - th*0.45); x += tw*0.8; }
        }
        c2.lineTo(w, h); c2.closePath(); c2.fill();
      });
      tex.wrapS = THREE.RepeatWrapping; tex.repeat.set(3, 1);
      var bg = new THREE.CylinderGeometry(L[0], L[0], L[2], 64, 1, true);
      var bm = new THREE.Mesh(bg, new THREE.MeshBasicMaterial({map: tex, transparent: true, side: THREE.BackSide, fog: false, depthWrite: false}));
      bm.userData.dy = L[2]/2 - 18 - li*6; bm.renderOrder = -9 + li; scene.add(bm); bands.push(bm);
    });
    if(!st.night){
      // painted cumulus: soft white puffs with a flat, bluish-grey underside
      for(var v = 0; v < 4; v++){
        cloudTex.push(canvasTex(512, 256, function(c2, w, h){
          var puffs = [], n = 7 + Math.floor(rng()*5);
          for(var k = 0; k < n; k++){ var u2 = k/(n - 1); puffs.push([w*(0.14 + u2*0.72) + (rng() - 0.5)*30, h*0.62 - Math.sin(u2*Math.PI)*h*(0.12 + rng()*0.16), 34 + Math.sin(u2*Math.PI)*46 + rng()*22]); }
          // underside shade first, then the lit tops, then a soft clip of the flat base
          puffs.forEach(function(q){ var gr = c2.createRadialGradient(q[0], q[1] + q[2]*0.25, 2, q[0], q[1] + q[2]*0.25, q[2]*1.05); gr.addColorStop(0, "rgba(196,210,232,0.95)"); gr.addColorStop(0.75, "rgba(196,210,232,0.75)"); gr.addColorStop(1, "rgba(196,210,232,0)"); c2.fillStyle = gr; c2.beginPath(); c2.arc(q[0], q[1] + q[2]*0.25, q[2]*1.05, 0, Math.PI*2); c2.fill(); });
          puffs.forEach(function(q){ var gr = c2.createRadialGradient(q[0] - q[2]*0.15, q[1] - q[2]*0.2, 2, q[0], q[1], q[2]); gr.addColorStop(0, "rgba(255,255,255,1)"); gr.addColorStop(0.7, "rgba(250,252,255,0.92)"); gr.addColorStop(1, "rgba(250,252,255,0)"); c2.fillStyle = gr; c2.beginPath(); c2.arc(q[0], q[1], q[2], 0, Math.PI*2); c2.fill(); });
          c2.globalCompositeOperation = "destination-out";
          var fade = c2.createLinearGradient(0, h*0.7, 0, h*0.84); fade.addColorStop(0, "rgba(0,0,0,0)"); fade.addColorStop(1, "rgba(0,0,0,1)");
          c2.fillStyle = fade; c2.fillRect(0, h*0.7, w, h*0.3);
          c2.globalCompositeOperation = "source-over";
        }));
      }
      for(var c = 0; c < 22; c++){
        var a = rng()*Math.PI*2, d = 760 + rng()*360, sm = new THREE.SpriteMaterial({map: cloudTex[c % cloudTex.length], transparent: true, depthWrite: false, fog: false, opacity: 0.88 + rng()*0.12});
        var sp = new THREE.Sprite(sm); sp.position.set(Math.cos(a)*d, 120 + rng()*230, Math.sin(a)*d); var sc = 300 + rng()*320; sp.scale.set(sc, sc*0.5, 1);
        sp.renderOrder = -9; scene.add(sp); clouds.push(sp);
      }
    } else {
      var sg = new THREE.BufferGeometry(), sp2 = new Float32Array(1100*3);
      for(var s = 0; s < 1100; s++){
        var th = rng()*Math.PI*2, ph = Math.acos(0.05 + rng()*0.95);
        sp2[s*3] = Math.cos(th)*Math.sin(ph)*1200; sp2[s*3+1] = Math.cos(ph)*1200; sp2[s*3+2] = Math.sin(th)*Math.sin(ph)*1200;
      }
      sg.setAttribute("position", new THREE.BufferAttribute(sp2, 3));
      stars = new THREE.Points(sg, new THREE.PointsMaterial({color: 0xdfe8ff, size: 2, sizeAttenuation: false, fog: false, depthWrite: false}));
      stars.renderOrder = -9; scene.add(stars);
      var mtex = canvasTex(128, 128, function(g2, w){ var gr = g2.createRadialGradient(64, 64, 10, 64, 64, 62); gr.addColorStop(0, "rgba(255,250,235,1)"); gr.addColorStop(0.42, "rgba(240,240,255,0.95)"); gr.addColorStop(0.5, "rgba(200,210,255,0.25)"); gr.addColorStop(1, "rgba(200,210,255,0)"); g2.fillStyle = gr; g2.fillRect(0, 0, w, w); });
      moon = new THREE.Sprite(new THREE.SpriteMaterial({map: mtex, transparent: true, depthWrite: false, fog: false}));
      moon.position.set(sunDir.x*1000, 260, sunDir.z*1000); moon.scale.set(110, 110, 1); moon.renderOrder = -8; scene.add(moon);
    }
  }

  /* ---------- terrain: smooth, painterly, more detail near the road ---------- */
  var CH = 64;                                     // cells per chunk side
  function makeNoise(seed){
    var r = mulberry32(seed), val = new Float32Array(256), perm = new Uint16Array(512), i;
    for(i = 0; i < 256; i++){ perm[i] = i; val[i] = r(); }
    for(i = 255; i > 0; i--){ var j = Math.floor(r()*(i + 1)), t = perm[i]; perm[i] = perm[j]; perm[j] = t; }
    for(i = 0; i < 256; i++) perm[i + 256] = perm[i];
    function v(ix, iz){ return val[perm[(ix & 255) + perm[iz & 255]]]; }
    return function(x, z){
      var ix = Math.floor(x), iz = Math.floor(z), fx = x - ix, fz = z - iz, u = fx*fx*(3 - 2*fx), w = fz*fz*(3 - 2*fz);
      var A = v(ix, iz), B = v(ix + 1, iz), C2 = v(ix, iz + 1), D = v(ix + 1, iz + 1);
      return A + (B - A)*u + (C2 - A)*w + (A - B - C2 + D)*u*w;
    };
  }
  var GC = [new THREE.Color(), new THREE.Color(), new THREE.Color()], TC = new THREE.Color();
  // The land's colour at a grid point: big seeded patches of the three greens, a faint
  // finer mottle, a little darker in dips (from the heightfield itself).
  function landColor(st, pal, k, gi, gj, out){
    var G = st.grid, nx = G.nx, nz = G.nz, x = G.x0 + gi*G.cell, z = G.z0 + gj*G.cell, sid = G.surf[k];
    var n1 = R.noiseA(x/55, z/55), n2 = R.noiseB(x/9, z/9);
    if(sid === 3){
      GC[0].setHex(pal.grass[0]); GC[1].setHex(pal.grass[1]); GC[2].setHex(pal.grass[2]);
      var t3 = clamp((n1 - 0.2)*2.4, 0, 2);
      if(t3 < 1) out.copy(GC[2]).lerp(GC[0], t3); else out.copy(GC[0]).lerp(GC[1], t3 - 1);
    } else if(sid === 4) out.setHex(pal.mud); else if(sid === 2) out.setHex(pal.dirt); else if(sid === 5) out.setHex(pal.snow);
    else if(sid === 6 || sid === 1) out.setHex(pal.loose); else out.setHex(0x6a6d70);
    var d = 2, i0 = Math.max(0, gi - d), i1 = Math.min(nx - 1, gi + d), j0 = Math.max(0, gj - d), j1 = Math.min(nz - 1, gj + d);
    var avg = (G.h[gj*nx + i0] + G.h[gj*nx + i1] + G.h[j0*nx + gi] + G.h[j1*nx + gi])/4, cv = avg - G.h[k];
    out.multiplyScalar((1 - clamp(cv*0.25, 0, 0.14))*(0.97 + n2*0.06));
    return out;
  }
  function gridNormal(G, gi, gj, out){
    var nx = G.nx, nz = G.nz, a = Math.max(0, gi - 1), b = Math.min(nx - 1, gi + 1), c = Math.max(0, gj - 1), d = Math.min(nz - 1, gj + 1);
    var dx = (G.h[gj*nx + b] - G.h[gj*nx + a])/((b - a)*G.cell || 1), dz = (G.h[d*nx + gi] - G.h[c*nx + gi])/((d - c)*G.cell || 1);
    var l = Math.sqrt(dx*dx + 1 + dz*dz); out[0] = -dx/l; out[1] = 1/l; out[2] = -dz/l;
  }
  var NRM = [0, 1, 0];
  function buildChunk(st, pal, cx, cz, step){
    var G = st.grid, x0 = G.x0, z0 = G.z0, cell = G.cell, nx = G.nx, nz = G.nz;
    var i0 = cx*CH, j0 = cz*CH, i1 = Math.min(nx - 1, i0 + CH), j1 = Math.min(nz - 1, j0 + CH);
    var cols = Math.floor((i1 - i0)/step) + 1, rows = Math.floor((j1 - j0)/step) + 1;
    if(cols < 2 || rows < 2) return null;
    // the grid, plus a skirt hanging 3 m under the four edges (hides cracks next to other detail levels)
    var nv = cols*rows + 2*(cols + rows), pos = new Float32Array(nv*3), nor = new Float32Array(nv*3), col = new Float32Array(nv*3), idx = [];
    function put(v, gi, gj, drop){
      var k = gj*nx + gi;
      pos[v*3] = x0 + gi*cell; pos[v*3 + 1] = G.h[k] - (drop || 0); pos[v*3 + 2] = z0 + gj*cell;
      gridNormal(G, gi, gj, NRM); nor[v*3] = NRM[0]; nor[v*3 + 1] = NRM[1]; nor[v*3 + 2] = NRM[2];
      landColor(st, pal, k, gi, gj, TC); col[v*3] = TC.r; col[v*3 + 1] = TC.g; col[v*3 + 2] = TC.b;
    }
    function GI(c){ return Math.min(i1, i0 + c*step); } function GJ(r){ return Math.min(j1, j0 + r*step); }
    for(var r = 0; r < rows; r++) for(var c = 0; c < cols; c++) put(r*cols + c, GI(c), GJ(r));
    for(var rr = 0; rr < rows - 1; rr++) for(var cc = 0; cc < cols - 1; cc++){
      var a = rr*cols + cc, b = a + 1, d = a + cols, e = d + 1;
      if((cc + rr) % 2 === 0) idx.push(a, d, b, b, d, e); else idx.push(a, d, e, a, e, b);
    }
    var sv = cols*rows;
    function skirt(list){
      var start = sv;
      list.forEach(function(q){ put(sv++, q[1], q[2], 3); });
      for(var i = 0; i < list.length - 1; i++){ var t0 = list[i][0], t1 = list[i + 1][0], s0 = start + i, s1 = start + i + 1; idx.push(t0, s0, t1, t1, s0, s1, t0, t1, s0, t1, s1, s0); }
    }
    var e1 = [], e2 = [], e3 = [], e4 = [];
    for(c = 0; c < cols; c++){ e1.push([c, GI(c), GJ(0)]); e2.push([(rows - 1)*cols + c, GI(c), GJ(rows - 1)]); }
    for(r = 0; r < rows; r++){ e3.push([r*cols, GI(0), GJ(r)]); e4.push([r*cols + cols - 1, GI(cols - 1), GJ(r)]); }
    skirt(e1); skirt(e2); skirt(e3); skirt(e4);
    var g = new THREE.BufferGeometry();
    g.setAttribute("position", new THREE.BufferAttribute(pos.subarray(0, sv*3), 3)); g.setAttribute("normal", new THREE.BufferAttribute(nor.subarray(0, sv*3), 3));
    g.setAttribute("color", new THREE.BufferAttribute(col.subarray(0, sv*3), 3)); g.setIndex(idx);
    g.computeBoundingSphere();
    var m = new THREE.Mesh(g, M.terrain); m.receiveShadow = true; m.matrixAutoUpdate = false;
    return m;
  }
  function buildTerrain(st, pal){
    var G = st.grid, ncx = Math.ceil((G.nx - 1)/CH), ncz = Math.ceil((G.nz - 1)/CH);
    for(var cz = 0; cz < ncz; cz++) for(var cx = 0; cx < ncx; cx++){
      var midx = G.x0 + (cx + 0.5)*CH*G.cell, midz = G.z0 + (cz + 0.5)*CH*G.cell, L = st.locate(midx, midz);
      var coarse = buildChunk(st, pal, cx, cz, 4);
      if(!coarse) continue;
      var ch = {x: midx, z: midz, coarse: coarse, fine: null, cx: cx, cz: cz, near: L.d < 170};
      world.add(coarse);
      R.chunks.push(ch);
    }
    // the land past the grid: one coarse sheet from the noise, lowered under the grid
    var ext = 900, step = 24, x0 = G.x0 - ext, z0 = G.z0 - ext, x1 = G.x0 + (G.nx - 1)*G.cell + ext, z1 = G.z0 + (G.nz - 1)*G.cell + ext;
    var cols = Math.ceil((x1 - x0)/step) + 1, rows = Math.ceil((z1 - z0)/step) + 1;
    var pos = new Float32Array(cols*rows*3), col = new Float32Array(cols*rows*3), idx = [];
    var gx1 = G.x0 + (G.nx - 1)*G.cell, gz1 = G.z0 + (G.nz - 1)*G.cell;
    for(var r = 0; r < rows; r++) for(var c = 0; c < cols; c++){
      var x = x0 + c*step, z = z0 + r*step, inside = x > G.x0 + 8 && x < gx1 - 8 && z > G.z0 + 8 && z < gz1 - 8;
      var y = st.natural(x, z) - (inside ? 8 : 0.4), k = (r*cols + c)*3;
      pos[k] = x; pos[k+1] = y; pos[k+2] = z;
      COL.setHex(pal.grass[0]).lerp(TC.setHex(pal.grass[2]), R.noiseA(x/90, z/90)); COL.multiplyScalar(0.96);
      col[k] = COL.r; col[k+1] = COL.g; col[k+2] = COL.b;
      if(r < rows - 1 && c < cols - 1){ var a = r*cols + c; idx.push(a, a + cols, a + 1, a + 1, a + cols, a + cols + 1); }
    }
    var sg = new THREE.BufferGeometry(); sg.setAttribute("position", new THREE.BufferAttribute(pos, 3)); sg.setAttribute("color", new THREE.BufferAttribute(col, 3)); sg.setIndex(idx);
    sg.computeVertexNormals();
    var skirt = new THREE.Mesh(sg, M.terrain); skirt.matrixAutoUpdate = false; skirt.receiveShadow = false; world.add(skirt);
    R.deco.push(skirt);
  }
  // swap chunks between coarse and fine around the camera (fine built lazily, a few per frame)
  var built = 0;
  function updateChunks(cx, cz){
    var budget = 2;
    for(var i = 0; i < R.chunks.length; i++){
      var ch = R.chunks[i], dx = ch.x - cx, dz = ch.z - cz, d = Math.sqrt(dx*dx + dz*dz);
      var wantFine = ch.near && d < (low ? 170 : 270);
      if(wantFine && !ch.fine && budget > 0){ ch.fine = buildChunk(R.stage, R.pal, ch.cx, ch.cz, 1); if(ch.fine) world.add(ch.fine); budget--; built++; }
      if(ch.fine){ ch.fine.visible = wantFine; ch.coarse.visible = !wantFine; }
      else ch.coarse.visible = true;
      if(d > (low ? 500 : 680)){ ch.coarse.visible = false; if(ch.fine) ch.fine.visible = false; }
    }
  }

  /* ---------- the road ribbon ---------- */
  var ROAD_COLS = [-1.75, -1.3, -1, -0.62, -0.22, 0, 0.22, 0.62, 1, 1.3, 1.75];   // across, in road half-widths (the outer pairs are the shoulder)
  var ROAD_REPEAT = 28;                                                            // metres of road per texture tile
  // The road, painted (u: 0 = left shoulder edge, 1 = right; v along the road, tiling):
  // packed tan dirt, lighter loose patches along the edges and the crown, two pairs of long
  // dark wheel ruts that wander a little, and a ragged grass edge over the shoulders.
  function roadTexture(sid, seed, edgeHex, roadHalf){
    var rng = mulberry32(hashStr("road|" + sid + "|" + seed));
    return canvasTex(256, 1024, function(g, w, h){
      var base = ROAD_COL[sid], span = roadHalf*3.5, tar = sid === 0, snow = sid === 5;
      function U(lat){ return (0.5 + lat/span)*w; }
      var edgeL = U(-roadHalf), edgeR = U(roadHalf);
      // periodic wobble along the tile (integer frequencies, so it tiles)
      var ph = [rng()*6.28, rng()*6.28, rng()*6.28, rng()*6.28];
      function wob(y, k, a){ var t = y/h*Math.PI*2; return Math.sin(t*k + ph[k % 4])*a + Math.sin(t*(k*2 + 1) + ph[(k + 1) % 4])*a*0.45; }
      g.fillStyle = hexCss(base); g.fillRect(0, 0, w, h);
      // big soft tone variation
      for(var i = 0; i < 26; i++){
        var x = rng()*w, y = rng()*h, rx = 20 + rng()*60, ry = 60 + rng()*160, lt = rng() < 0.5;
        g.fillStyle = shade(base, lt ? 1.05 : 0.95); g.globalAlpha = 0.5;
        [-h, 0, h].forEach(function(o){ g.beginPath(); g.ellipse(x, y + o, rx, ry, 0, 0, Math.PI*2); g.fill(); });
      }
      g.globalAlpha = 1;
      if(!tar){
        // lighter loose dirt: patches hugging the edges and running down the crown
        var light = snow ? "#ffffff" : shade(base, 1.17);
        for(var k = 0; k < 70; k++){
          var where = rng(), px = where < 0.35 ? edgeL + 6 + rng()*30 : where < 0.7 ? edgeR - 6 - rng()*30 : w/2 + (rng() - 0.5)*24;
          var py = rng()*h, prx = 6 + rng()*16, pry = 30 + rng()*120;
          g.fillStyle = light; g.globalAlpha = 0.35 + rng()*0.35;
          [-h, 0, h].forEach(function(o){ g.beginPath(); g.ellipse(px, py + o, prx, pry, 0, 0, Math.PI*2); g.fill(); });
        }
        g.globalAlpha = 1;
        // the ruts: where the wheels run, packed darker, long and slightly wandering
        var dark = snow ? "#a9b6c6" : shade(base, 0.6), darker = snow ? "#97a6b8" : shade(base, 0.5);
        [[-0.8, 15, 0.62], [0.8, 15, 0.62], [-1.5, 9, 0.28], [1.5, 9, 0.28], [-0.05, 7, 0.16]].forEach(function(rt, ri){
          var cx = U(rt[0]);
          g.lineCap = "round"; g.strokeStyle = dark; g.globalAlpha = rt[2]; g.lineWidth = rt[1];
          g.beginPath(); for(var y2 = 0; y2 <= h; y2 += 8){ var xx = cx + wob(y2, 1 + ri % 3, 4); if(y2) g.lineTo(xx, y2); else g.moveTo(xx, y2); } g.stroke();
          if(ri < 2){
            g.strokeStyle = darker; g.globalAlpha = 0.45; g.lineWidth = 5;
            [-4, 4].forEach(function(o2){ g.beginPath(); for(var y3 = 0; y3 <= h; y3 += 8){ var x3 = cx + o2 + wob(y3, 1 + ri % 3, 4) + wob(y3, 3, 1.5); if(y3) g.lineTo(x3, y3); else g.moveTo(x3, y3); } g.stroke(); });
          }
        });
        g.globalAlpha = 1;
        // a light scatter of small stones
        for(var s = 0; s < 900; s++){
          var sx = rng()*w, sy = rng()*h, sr = 1 + rng()*1.6;
          g.fillStyle = shade(base, rng() < 0.5 ? 0.72 : 1.25); g.globalAlpha = 0.5; g.fillRect(sx, sy, sr, sr);
        }
        g.globalAlpha = 1;
        // the grass edge over the shoulder: ragged, with a soft inner line
        g.fillStyle = hexCss(edgeHex);
        [[0, edgeL, 1], [w, edgeR, -1]].forEach(function(e){
          g.beginPath(); g.moveTo(e[0], 0);
          for(var y4 = 0; y4 <= h; y4 += 6){ var xx2 = e[1] - e[2]*(10 - wob(y4, 2, 7) - wob(y4, 5, 3)); g.lineTo(xx2, y4); }
          g.lineTo(e[0], h); g.closePath(); g.fill();
        });
      } else {
        g.fillStyle = "rgba(240,240,232,0.92)"; g.fillRect(edgeL + 4, 0, 4, h); g.fillRect(edgeR - 8, 0, 4, h);
        g.fillStyle = "rgba(245,220,120,0.9)"; for(var y5 = 0; y5 < h; y5 += 128) g.fillRect(w/2 - 2, y5, 4, 64);
        [U(-0.8), U(0.8)].forEach(function(cx2){ g.fillStyle = "rgba(20,20,22,0.18)"; g.fillRect(cx2 - 7, 0, 14, h); });
        g.fillStyle = hexCss(edgeHex); g.fillRect(0, 0, edgeL - 10, h); g.fillRect(edgeR + 10, 0, w - edgeR - 10, h);
      }
    }, true);
  }
  function buildRoad(st, pal){
    var pts = st.pts, n = pts.length, W = st.roadHalf, groups = {};
    var edgeHex = st.biome === "snow" ? pal.snow : pal.verge;
    for(var i = 0; i < n - 1; i++){
      var sid = pts[i].surf, G = groups[sid] || (groups[sid] = {pos: [], uv: [], col: [], idx: [], rows: 0, last: -2});
      if(G.last !== i) addRow(G, pts[i], i);
      addRow(G, pts[i + 1], i + 1);
      var a = (G.rows - 2)*ROAD_COLS.length, b = (G.rows - 1)*ROAD_COLS.length;
      // (wound so the faces point up: forward x right is down)
      for(var c = 0; c < ROAD_COLS.length - 1; c++) G.idx.push(a + c, a + c + 1, b + c, a + c + 1, b + c + 1, b + c);
      G.last = i + 1;
    }
    function addRow(G, P, pi){
      var nx = Math.cos(P.h), nz = Math.sin(P.h);
      for(var c = 0; c < ROAD_COLS.length; c++){
        var lat = ROAD_COLS[c]*W, x = P.x + nx*lat, z = P.z + nz*lat;
        var y = st.env.height(x, z) + (Math.abs(ROAD_COLS[c]) > 1.2 ? 0.02 : 0.03);
        G.pos.push(x, y, z);
        G.uv.push((ROAD_COLS[c] + 1.75)/3.5, P.s/ROAD_REPEAT);
        var nv = (hashStr(pi + "|" + c + "|" + st.seed) % 1000)/1000, k = 0.96 + nv*0.06;
        G.col.push(k, k, k);
      }
      G.rows++;
    }
    Object.keys(groups).forEach(function(k){
      var G = groups[k], g = new THREE.BufferGeometry();
      g.setAttribute("position", new THREE.Float32BufferAttribute(G.pos, 3)); g.setAttribute("uv", new THREE.Float32BufferAttribute(G.uv, 2));
      g.setAttribute("color", new THREE.Float32BufferAttribute(G.col, 3)); g.setIndex(G.idx);
      g.computeVertexNormals(); g.computeBoundingSphere();
      var mat = toon({map: roadTexture(+k, st.seed, edgeHex, W), vertexColors: true, polygonOffset: true, polygonOffsetFactor: -2, polygonOffsetUnits: -2}, true);
      var m = new THREE.Mesh(g, mat); m.receiveShadow = true; m.matrixAutoUpdate = false; world.add(m); R.deco.push(m);
    });
  }

  /* ---------- scenery models (merged primitives with vertex colours) ---------- */
  // A tall stylised conifer: a dark trunk, then five stacked, angular, slightly skewed tiers.
  function coniferGeo(pal, snowy, v){
    var r = mulberry32(hashStr("conifer|" + v)), parts = [];
    parts.push(prim(jitter(new THREE.CylinderGeometry(0.16, 0.32, 4.6, 6), 0.05, 7 + v, 0), pal.trunk, tr(0, 2.3, 0)));
    var tiers = [[2.7, 3.2, 2.4], [2.25, 3.0, 4.2], [1.8, 2.8, 5.9], [1.35, 2.6, 7.5], [0.85, 2.4, 9.0]];
    tiers.forEach(function(t, i){
      var c = new THREE.Color(pal.conifer[(i + v) % 3]).multiplyScalar(0.94 + i*0.03);
      var g = jitter(new THREE.ConeGeometry(t[0], t[1], 7, 1), 0.35, hashStr("tier|" + v + "|" + i), 0.6);
      parts.push(prim(g, c, tr((r() - 0.5)*0.25, t[2] + t[1]/2, (r() - 0.5)*0.25, (r() - 0.5)*0.12, r()*6.28, (r() - 0.5)*0.12),
        function(x, y, z, ny){ return snowy && ny > 0.35 ? COL.set(0xf2f6fa) : ny < -0.5 ? COL.copy(c).multiplyScalar(0.7) : null; }));
    });
    return merge(parts);
  }
  // A round-canopy broadleaf: a tall trunk with two limbs, a crown of faceted clumps.
  function roundGeo(pal, v, birch){
    var r = mulberry32(hashStr("round|" + v + "|" + birch)), parts = [];
    var th = birch ? 5.2 : 4.4, trunkC = birch ? 0xe6e1d5 : pal.trunk;
    parts.push(prim(jitter(new THREE.CylinderGeometry(birch ? 0.1 : 0.2, birch ? 0.16 : 0.32, th, 6), 0.04, 31 + v, 0), trunkC, tr(0, th/2, 0, 0, 0, (r() - 0.5)*0.08)));
    [-1, 1].forEach(function(sd){ parts.push(prim(new THREE.CylinderGeometry(0.06, 0.12, 2.0, 5), trunkC, tr(sd*0.55, th - 0.6, 0.1*sd, 0, 0, -sd*0.75))); });
    var blobs = birch ? [[0, th + 1.0, 0, 1.25], [0.7, th + 0.2, 0.3, 0.95], [-0.6, th + 0.4, -0.3, 1.0], [0.1, th + 1.9, 0.1, 0.9]]
                      : [[0, th + 1.2, 0, 2.0], [1.3, th + 0.5, 0.4, 1.45], [-1.2, th + 0.6, -0.5, 1.5], [0.3, th + 0.4, -1.2, 1.35], [-0.4, th + 0.7, 1.2, 1.4], [0.2, th + 2.4, -0.1, 1.35]];
    blobs.forEach(function(b, i){
      var c = new THREE.Color(pal.leaf[(i + v) % 3]);
      var g = jitter(new THREE.IcosahedronGeometry(b[3], 1), b[3]*0.26, hashStr("blob|" + v + "|" + i + "|" + birch));
      parts.push(prim(g, c, tr(b[0], b[1], b[2], r()*3, r()*3, 0, 1, 0.82, 1),
        function(x, y, z, ny){ return ny > 0.55 ? COL.copy(c).multiplyScalar(1.1) : ny < -0.4 ? COL.copy(c).multiplyScalar(0.78) : null; }));
    });
    return merge(parts);
  }
  function rockGeo(pal, seed){
    var g = new THREE.DodecahedronGeometry(1, 0), p = g.attributes.position, rng = mulberry32(seed), cache = {};
    for(var i = 0; i < p.count; i++){
      var key = Math.round(p.getX(i)*100) + "," + Math.round(p.getY(i)*100) + "," + Math.round(p.getZ(i)*100);
      var k = cache[key] || (cache[key] = 0.75 + rng()*0.5);
      p.setXYZ(i, p.getX(i)*k*1.2, p.getY(i)*k*0.75, p.getZ(i)*k);
    }
    return prim(g, pal.rock, tr(0, 0.25, 0), function(x, y, z, ny){ return COL.setHex(pal.rock).multiplyScalar(0.86 + ny*0.16); });
  }
  // A clump of thin grass blades: dark at the root, lighter (and a touch yellow) at the tips.
  function tuftGeo(pal, v){
    var r = mulberry32(hashStr("tuft|" + v)), n = 13, pos = [], col = [], nor = [];
    var c0 = new THREE.Color(pal.tuft[0]).multiplyScalar(0.72), c1 = new THREE.Color(pal.tuft[1]);
    for(var i = 0; i < n; i++){
      var a = r()*Math.PI*2, d = Math.sqrt(r())*0.35, x = Math.cos(a)*d, z = Math.sin(a)*d, hgt = 0.45 + r()*0.65, wd = 0.022 + r()*0.02;
      var lean = (r() - 0.5)*0.5, la = r()*Math.PI*2, tx = x + Math.cos(la)*lean*hgt, tz = z + Math.sin(la)*lean*hgt, bo = r()*Math.PI;
      var bx = Math.cos(bo)*wd, bz = Math.sin(bo)*wd;
      pos.push(x - bx, 0, z - bz, x + bx, 0, z + bz, tx, hgt, tz);
      var tip = COL.copy(c1).multiplyScalar(0.9 + r()*0.2);
      col.push(c0.r, c0.g, c0.b, c0.r, c0.g, c0.b, tip.r, tip.g, tip.b);
      nor.push(0, 1, 0, 0, 1, 0, 0, 1, 0);
    }
    var g = new THREE.BufferGeometry();
    g.setAttribute("position", new THREE.Float32BufferAttribute(pos, 3)); g.setAttribute("normal", new THREE.Float32BufferAttribute(nor, 3)); g.setAttribute("color", new THREE.Float32BufferAttribute(col, 3));
    g.computeBoundingSphere();
    return g;
  }
  function instanced(geo, mat, list, place, cast){
    var im = new THREE.InstancedMesh(geo, mat, list.length);
    for(var i = 0; i < list.length; i++){ place(list[i], MAT4, i); im.setMatrixAt(i, MAT4); }
    im.instanceMatrix.needsUpdate = true; im.castShadow = !!cast; im.receiveShadow = false;
    im.computeBoundingSphere(); im.frustumCulled = true;
    return im;
  }
  // put instances into spatial buckets so whole buckets are culled by distance and frustum
  function bucketed(list, size){
    var B = {};
    list.forEach(function(o){ var k = Math.floor(o.x/size) + "," + Math.floor(o.z/size); (B[k] = B[k] || []).push(o); });
    return Object.keys(B).map(function(k){ return B[k]; });
  }
  var decoBuckets = [];               // {mesh, x, z, far, near, ink (0 none, 1 near ink, 2 far ink), grass}
  function addBucket(mesh, list, far, near, extra){
    var x = 0, z = 0; list.forEach(function(o){ x += o.x; z += o.z; });
    var b = {mesh: mesh, x: x/list.length, z: z/list.length, far: far, near: near || 0, ink: 0, grass: false};
    if(extra) for(var k in extra) b[k] = extra[k];
    world.add(mesh); decoBuckets.push(b);
    return b;
  }
  // an instanced mesh plus its ink outline (sharing the instance matrices)
  function addInked(geo, hull, mat, ink, list, place, cast, far, near, level){
    var im = instanced(geo, mat, list, place, cast);
    addBucket(im, list, far, near);
    if(hull){
      var ih = new THREE.InstancedMesh(hull, ink, list.length); ih.instanceMatrix = im.instanceMatrix; ih.count = list.length;
      ih.computeBoundingSphere(); ih.castShadow = false;
      addBucket(ih, list, far, near, {ink: level});
    }
  }
  function buildScenery(st, pal){
    var sc = st.scenery, snowy = st.biome === "snow";
    var treeGeos = [[coniferGeo(pal, snowy, 0), coniferGeo(pal, snowy, 1), coniferGeo(pal, snowy, 2)], [roundGeo(pal, 0, false), roundGeo(pal, 1, false)], [roundGeo(pal, 2, true)]];
    var treeHulls = treeGeos.map(function(L){ return L.map(hullGeo); });
    // far: one simple shape per tree (with its own outline on High)
    var farGeos = [
      merge([prim(new THREE.CylinderGeometry(0.2, 0.3, 4.6, 5), pal.trunk, tr(0, 2.3, 0)), prim(new THREE.ConeGeometry(2.5, 8.2, 6), pal.conifer[1], tr(0, 7.4, 0), snowy ? function(x, y, z, ny){ return ny > 0.35 && y > 8.5 ? COL.set(0xf2f6fa) : null; } : null)]),
      merge([prim(new THREE.CylinderGeometry(0.2, 0.3, 4.4, 5), pal.trunk, tr(0, 2.2, 0)), prim(new THREE.IcosahedronGeometry(2.6, 0), pal.leaf[1], tr(0, 5.6, 0, 0, 0, 0, 1, 0.8, 1))]),
      merge([prim(new THREE.CylinderGeometry(0.1, 0.15, 5.2, 4), 0xe6e1d5, tr(0, 2.6, 0)), prim(new THREE.IcosahedronGeometry(1.6, 0), pal.leaf[0], tr(0, 6.3, 0))])];
    var farHulls = farGeos.map(hullGeo);
    var NEAR = Qd.near, FAR = 470;
    function placeTree(o, m){ EUL.set(0, o.r, 0); Q.setFromEuler(EUL); m.compose(TMP.set(o.x, o.y - 0.15, o.z), Q, TMP2.set(o.s, o.s*(0.9 + o.v*0.3), o.s)); }
    bucketed(sc.trees, 160).forEach(function(list){
      [0, 1, 2].forEach(function(t){
        var all = list.filter(function(o){ return o.t === t; }); if(!all.length) return;
        var variants = treeGeos[t];
        variants.forEach(function(geo, vi){
          var L = all.filter(function(o){ return Math.floor(o.v*variants.length) === vi; });
          if(L.length) addInked(geo, treeHulls[t][vi], M.flat, M.inkTree, L, placeTree, true, NEAR, 0, 1);
        });
        addInked(farGeos[t], farHulls[t], M.flat, M.inkFar, all, placeTree, false, FAR, NEAR, 2);
      });
    });
    var rockGeos = [rockGeo(pal, 11), rockGeo(pal, 23), rockGeo(pal, 37)], rockHulls = rockGeos.map(hullGeo);
    bucketed(sc.rocks, 240).forEach(function(list){
      rockGeos.forEach(function(geo, vi){
        var L = list.filter(function(o){ return Math.floor(o.v*3) === vi; }); if(!L.length) return;
        addInked(geo, rockHulls[vi], M.flat, M.inkTree, L, function(o, m){ EUL.set(o.v*0.4, o.r, 0); Q.setFromEuler(EUL); m.compose(TMP.set(o.x, o.y - o.s*0.2, o.z), Q, TMP2.set(o.s, o.s, o.s)); }, true, 420, 0, 1);
      });
    });
    // grass: the stage's tufts, each a clump of blades, plus two more clumps nearby (seeded)
    var tufts = [], gr = mulberry32(hashStr("blades|" + st.seed));
    sc.tufts.forEach(function(o){
      tufts.push(o);
      for(var k = 0; k < (snowy ? 0 : 2); k++){
        var x = o.x + (gr() - 0.5)*2.4, z = o.z + (gr() - 0.5)*2.4;
        var G = st.grid, gi = Math.round((x - G.x0)/G.cell), gj = Math.round((z - G.z0)/G.cell);
        if(gi < 0 || gj < 0 || gi >= G.nx || gj >= G.nz || G.surf[gj*G.nx + gi] !== 3) continue;   // grass only, never on the road
        tufts.push({x: x, z: z, y: st.env.height(x, z), s: 0.6 + gr()*0.7, r: gr()*6.28, v: gr()});
      }
    });
    var tgs = [tuftGeo(pal, 0), tuftGeo(pal, 1), tuftGeo(pal, 2)];
    bucketed(tufts, 60).forEach(function(list){
      tgs.forEach(function(tg, vi){
        var L = list.filter(function(o){ return Math.floor(o.v*3) === vi; }); if(!L.length) return;
        addBucket(instanced(tg, M.grass, L, function(o, m){ EUL.set(0, o.r, 0); Q.setFromEuler(EUL); m.compose(TMP.set(o.x, o.y - 0.04, o.z), Q, TMP2.set(o.s, o.s*(0.8 + o.v*0.5), o.s)); }, false), L, 120, 0, {grass: true});
      });
    });
    // fences: posts and two rails between neighbours
    if(sc.fences.length){
      var post = prim(new THREE.BoxGeometry(0.14, 1.2, 0.14), 0x7a5c40, tr(0, 0.6, 0)), rail = prim(new THREE.BoxGeometry(1, 0.09, 0.05), 0x8d6d4c, null);
      var rails = [];
      for(var i = 0; i < sc.fences.length - 1; i++){
        var a = sc.fences[i], b = sc.fences[i + 1], dx = b.x - a.x, dz = b.z - a.z, d = Math.sqrt(dx*dx + dz*dz);
        if(d > 4.5) continue;
        [0.55, 0.95].forEach(function(hh){ rails.push({x: (a.x + b.x)/2, z: (a.z + b.z)/2, y: (a.y + b.y)/2 + hh, len: d, h: Math.atan2(dx, dz), tilt: Math.atan2(b.y - a.y, d)}); });
      }
      var postH = hullGeo(post);
      bucketed(sc.fences, 240).forEach(function(list){ addInked(post, postH, M.flat, M.inkTree, list, function(o, m){ m.compose(TMP.set(o.x, o.y - 0.1, o.z), Q.identity(), TMP2.set(1, 1, 1)); }, true, 380, 0, 1); });
      bucketed(rails, 240).forEach(function(list){ addBucket(instanced(rail, M.flat, list, function(o, m){ EUL.set(0, o.h + Math.PI/2, o.tilt, "YXZ"); Q.setFromEuler(EUL); m.compose(TMP.set(o.x, o.y - 0.1, o.z), Q, TMP2.set(o.len, 1, 1)); }, true), list, 380); });
    }
    // chevron boards: yellow with a black arrow, on a short post
    var chevTex = [1, -1].map(function(dir){
      return canvasTex(128, 128, function(g, w, h){
        g.fillStyle = "#16181c"; g.fillRect(0, 0, w, h); g.fillStyle = "#f4c21a"; g.fillRect(8, 8, w - 16, h - 16); g.fillStyle = "#16181c";
        g.beginPath();
        if(dir > 0){ g.moveTo(38, 22); g.lineTo(82, 64); g.lineTo(38, 106); g.lineTo(56, 106); g.lineTo(100, 64); g.lineTo(56, 22); }
        else { g.moveTo(90, 22); g.lineTo(46, 64); g.lineTo(90, 106); g.lineTo(72, 106); g.lineTo(28, 64); g.lineTo(72, 22); }
        g.closePath(); g.fill();
      });
    });
    var board = new THREE.BoxGeometry(0.8, 0.8, 0.05), postG = prim(new THREE.BoxGeometry(0.08, 1.2, 0.08), 0x3b3e44, tr(0, 0.6, 0)), postGH = hullGeo(postG), boardH = hullGeo(board);
    [1, -1].forEach(function(dir, di){
      var L = sc.chevrons.filter(function(c){ return c.dir === dir; }); if(!L.length) return;
      var mat = toon({map: chevTex[di]}); M.extra.push(mat);
      // face the oncoming driver: the board's normal points back down the road
      var place = function(o, m){ EUL.set(0, -o.h, 0); Q.setFromEuler(EUL); m.compose(TMP.set(o.x, o.y + 1.45, o.z), Q, TMP2.set(1, 1, 1)); };
      addInked(board, boardH, mat, M.inkTree, L, place, true, 480, 0, 1);
      addInked(postG, postGH, M.flat, M.inkTree, L, function(o, m){ m.compose(TMP.set(o.x, o.y + 0.1, o.z), Q.identity(), TMP2.set(1, 1, 1)); }, true, 480, 0, 1);
    });
    // gate flags (small, in the stage colours; the finish has its arch)
    if(sc.flags.length){
      var flagG = merge([prim(new THREE.CylinderGeometry(0.035, 0.035, 2.2, 5), 0xe8e8e8, tr(0, 1.1, 0)),
                         prim(new THREE.BoxGeometry(0.02, 0.42, 0.62), 0xe0533d, tr(0, 1.95, 0.31))]);
      addInked(flagG, hullGeo(flagG), M.flat, M.inkTree, sc.flags, function(o, m){ EUL.set(0, -o.h, 0); Q.setFromEuler(EUL); m.compose(TMP.set(o.x, o.y, o.z), Q, TMP2.set(1, 1, 1)); }, true, 480, 0, 1);
    }
    // sponsor banners (original names) on two posts
    var banTex = SPONSORS.map(function(sp, i){
      var cols = [["#1d3b6e", "#ffd34d"], ["#c4372b", "#ffffff"], ["#1f6b45", "#f4efe3"], ["#f0b23a", "#1f2a44"], ["#3b2d5e", "#ff9f6b"], ["#e8e4da", "#2a5d8f"], ["#111418", "#7fe0a0"], ["#7a2e2e", "#ffe6a8"]][i % 8];
      return canvasTex(512, 96, function(g, w, h){
        g.fillStyle = cols[0]; g.fillRect(0, 0, w, h); g.fillStyle = cols[1];
        g.font = "italic 900 52px sans-serif"; g.textAlign = "center"; g.textBaseline = "middle";
        g.fillText(sp[0], w/2 - 40, h/2 + 2);
        g.font = "700 26px sans-serif"; g.fillText(sp[1], w - 70, h/2 + 3);
        g.fillRect(0, 0, w, 6); g.fillRect(0, h - 6, w, 6);
      });
    });
    var banG = new THREE.BoxGeometry(6, 1.1, 0.04), banH = hullGeo(banG), banPost = new THREE.CylinderGeometry(0.06, 0.06, 2.0, 5); banPost.translate(0, 1, 0);
    sc.banners.forEach(function(b){
      var mat = toon({map: banTex[b.v % banTex.length]}); M.extra.push(mat);
      var m = new THREE.Mesh(banG, mat);
      // along the road, its face turned toward the road
      m.position.set(b.x, b.y + 1.4, b.z); m.rotation.y = -b.h - b.side*Math.PI/2; m.castShadow = true; world.add(m); R.deco.push(m);
      var hm = new THREE.Mesh(banH, M.inkTree); hm.position.copy(m.position); hm.rotation.copy(m.rotation); world.add(hm); R.deco.push(hm); hm.userData.ink = 1;
      [-2.8, 2.8].forEach(function(o){
        var p = new THREE.Mesh(banPost, M.metal), dx = Math.sin(b.h)*o, dz = -Math.cos(b.h)*o;
        p.position.set(b.x + dx, b.y + 0.3, b.z + dz); world.add(p); R.deco.push(p);
      });
    });
    // round hay bales at the banners' ends and spectators standing well back behind them (seeded, visual only)
    var pr = mulberry32(hashStr("props|" + st.seed)), bales = [], people = [];
    sc.banners.forEach(function(b){
      var ox = Math.cos(b.h)*b.side, oz = Math.sin(b.h)*b.side, tx = Math.sin(b.h), tz = -Math.cos(b.h);
      [-1, 1].forEach(function(k){ var x = b.x + tx*k*3.9 + ox*0.3, z = b.z + tz*k*3.9 + oz*0.3; bales.push({x: x, z: z, y: st.env.height(x, z), h: b.h + (pr() - 0.5)*0.4, v: pr()}); });
      if(st.night) return;
      var n = 3 + Math.floor(pr()*5);
      for(var k = 0; k < n; k++){
        var al = (pr() - 0.5)*8, out = 3.2 + pr()*2.8, x = b.x + tx*al + ox*out, z = b.z + tz*al + oz*out;
        people.push({x: x, z: z, y: st.env.height(x, z), r: Math.atan2(-ox, -oz) + (pr() - 0.5)*0.8, v: pr(), s: 0.92 + pr()*0.14});
      }
    });
    if(bales.length){
      var baleG = prim(new THREE.CylinderGeometry(0.62, 0.62, 1.2, 14), 0xd8b45c, tr(0, 0.62, 0, 0, 0, Math.PI/2), function(x, y, z, ny, c){ return Math.abs(x) > 0.55 ? COL.set(0xe6c97a) : null; });
      addInked(baleG, hullGeo(baleG), M.flat, M.inkTree, bales, function(o, m){ EUL.set(0, -o.h, 0); Q.setFromEuler(EUL); m.compose(TMP.set(o.x, o.y - 0.05, o.z), Q, TMP2.set(1, 1, 1)); }, true, 360, 0, 1);
    }
    if(people.length){
      var shirts = [0xc4372b, 0x2a5d8f, 0xf0b23a, 0x2f6a4f];
      shirts.forEach(function(sh, si){
        var L = people.filter(function(o){ return Math.floor(o.v*4) === si; }); if(!L.length) return;
        var pg = merge([prim(new THREE.CylinderGeometry(0.1, 0.12, 0.85, 6), 0x2c3140, tr(-0.11, 0.42, 0)), prim(new THREE.CylinderGeometry(0.1, 0.12, 0.85, 6), 0x2c3140, tr(0.11, 0.42, 0)),
                        prim(new THREE.CylinderGeometry(0.22, 0.25, 0.62, 7), sh, tr(0, 1.15, 0)), prim(new THREE.SphereGeometry(0.13, 8, 6), 0xd9a882, tr(0, 1.6, 0)),
                        prim(new THREE.CylinderGeometry(0.06, 0.06, 0.55, 5), sh, tr(-0.29, 1.12, 0, 0, 0, 0.18)), prim(new THREE.CylinderGeometry(0.06, 0.06, 0.55, 5), sh, tr(0.29, 1.12, 0, 0, 0, -0.18)),
                        prim(new THREE.CylinderGeometry(0.15, 0.15, 0.08, 8), si % 2 ? 0xf4f2ea : 0x1d2733, tr(0, 1.72, 0.02))]);
        addInked(pg, hullGeo(pg), M.flat, M.inkTree, L, function(o, m){ EUL.set(0, o.r, 0); Q.setFromEuler(EUL); m.compose(TMP.set(o.x, o.y - 0.05, o.z), Q, TMP2.set(o.s, o.s, o.s)); }, true, 260, 0, 1);
      });
    }
    // start / finish arches
    sc.arches.forEach(function(a){
      var g = new THREE.Group(), w = a.w;
      var tex = canvasTex(512, 96, function(c, W, H){
        for(var k = 0; k < W; k += 24){ c.fillStyle = (k/24) % 2 ? "#111" : "#f4f4f0"; c.fillRect(k, 0, 24, 14); c.fillStyle = (k/24) % 2 ? "#f4f4f0" : "#111"; c.fillRect(k, H - 14, 24, 14); }
        c.fillStyle = a.kind === "finish" ? "#c4372b" : "#1d3b6e"; c.fillRect(0, 14, W, H - 28);
        c.fillStyle = "#ffffff"; c.font = "italic 900 50px sans-serif"; c.textAlign = "center"; c.textBaseline = "middle"; c.fillText(a.kind === "finish" ? "FINISH" : "START", W/2, H/2 + 2);
      });
      var tm = toon({map: tex}); M.extra.push(tm);
      var beamG = new THREE.BoxGeometry(w*2 + 0.6, 1.2, 0.35);
      var beam = new THREE.Mesh(beamG, [M.dark, M.dark, M.dark, M.dark, tm, tm]);
      beam.position.y = 5.0; beam.castShadow = true; g.add(beam);
      var bh = new THREE.Mesh(hullGeo(beamG), M.inkTree); bh.position.y = 5.0; g.add(bh);
      [-1, 1].forEach(function(sd){ var tg = new THREE.BoxGeometry(0.5, 5.6, 0.5), t = new THREE.Mesh(tg, M.dark); t.position.set(sd*w, 2.8, 0); t.castShadow = true; g.add(t); var th2 = new THREE.Mesh(hullGeo(tg), M.inkTree); th2.position.copy(t.position); g.add(th2); });
      g.position.set(a.x, a.y - 0.2, a.z); g.rotation.y = -a.h; world.add(g); R.deco.push(g);
    });
    // the water tower
    if(sc.tower){
      var t = sc.tower, tg2 = new THREE.Group();
      [[-1, -1], [1, -1], [-1, 1], [1, 1]].forEach(function(l){ var leg = new THREE.Mesh(new THREE.CylinderGeometry(0.18, 0.24, 9, 6), M.metal); leg.position.set(l[0]*1.7, 4.5, l[1]*1.7); leg.rotation.set(l[1]*0.06, 0, -l[0]*0.06); leg.castShadow = true; tg2.add(leg); });
      var tankG = new THREE.CylinderGeometry(3.0, 3.0, 4.2, 12), tank = new THREE.Mesh(tankG, toon({color: 0xc9c2b4})); tank.position.y = 11; tank.castShadow = true; tg2.add(tank); M.extra.push(tank.material);
      var roofG = new THREE.ConeGeometry(3.3, 1.8, 12), roof = new THREE.Mesh(roofG, toon({color: 0x8e3b2e})); roof.position.y = 14; roof.castShadow = true; tg2.add(roof); M.extra.push(roof.material);
      [[tankG, 11], [roofG, 14]].forEach(function(q){ var hm = new THREE.Mesh(hullGeo(q[0]), M.inkTree); hm.position.y = q[1]; tg2.add(hm); });
      tg2.position.set(t.x, t.y - 0.3, t.z); world.add(tg2); R.deco.push(tg2);
    }
  }
  function updateDeco(cx, cz){
    var farK = Qd.far, grassD = Qd.grass;
    for(var i = 0; i < decoBuckets.length; i++){
      var b = decoBuckets[i], dx = b.x - cx, dz = b.z - cz, d2 = dx*dx + dz*dz, far = b.grass ? grassD : b.far*(b.near ? farK : 1);
      var on = d2 < far*far && d2 >= b.near*b.near;
      if(b.ink && b.ink > Qd.ink) on = false;
      b.mesh.visible = on;
    }
  }

  /* ---------- setStage ---------- */
  R.setStage = function(st){
    R.clearStage();
    R.stage = st;
    var pal = R.pal = BIOME[st.biome] || BIOME.forest;
    M.terrain = toon({vertexColors: true}, true);
    M.flat = toon({vertexColors: true});            // (the scenery geometry is unindexed: one normal per face, so it reads faceted)
    M.grass = toon({vertexColors: true, side: THREE.DoubleSide});
    M.dark = toon({color: 0x2b2f36});
    M.metal = toon({color: 0x8d969c});
    M.inkTree = inkMat(1.6, 26); M.inkFar = inkMat(1.2, 26);
    M.extra = [];
    var fogC = st.night ? 0x0b1226 : pal.fog;
    scene.fog = new THREE.Fog(fogC, st.night ? 25 : 150, st.night ? 260 : (low ? 430 : 620));
    scene.background = new THREE.Color(fogC);
    hemi.color.setHex(st.night ? 0x31467a : pal.hemiSky); hemi.groundColor.setHex(st.night ? 0x10161f : pal.hemiGround);
    hemi.intensity = st.night ? 0.42 : pal.hemiI;
    sun.color.setHex(st.night ? 0x9fb4ff : pal.sun); sun.intensity = st.night ? 0.45 : pal.sunI;
    // a low sun for long crisp shadows: off to one side of the opening straight and a little
    // ahead, so the trees' shadows fall across the road (the side is seeded)
    var r = mulberry32(hashStr("sun|" + st.seed)), H = (st.start.h || 0)*Math.PI/180, side = r() < 0.5 ? -1 : 1, ahead = 0.25 + r()*0.35, el = st.night ? 0.6 : 0.5 + r()*0.12;
    var sx = side*Math.cos(H)*Math.cos(ahead) + Math.sin(H)*Math.sin(ahead), sz = side*Math.sin(H)*Math.cos(ahead) - Math.cos(H)*Math.sin(ahead);
    sunDir.set(sx*Math.cos(el), Math.sin(el), sz*Math.cos(el)).normalize();
    R.noiseA = makeNoise(hashStr("landA|" + st.seed)); R.noiseB = makeNoise(hashStr("landB|" + st.seed));
    buildSky(st, pal);
    buildTerrain(st, pal);
    buildRoad(st, pal);
    buildScenery(st, pal);
    fxInit();
  };
  R.clearStage = function(){
    world.children.slice().forEach(function(o){ world.remove(o); disposeTree(o); });
    R.chunks = []; R.deco = []; decoBuckets = []; R.stage = null;
    ["terrain", "flat", "grass", "dark", "metal", "inkTree", "inkFar"].forEach(function(k){ if(M[k]){ M[k].dispose(); M[k] = null; } });
    (M.extra || []).forEach(function(m){ if(m.map) m.map.dispose(); m.dispose(); }); M.extra = [];
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
      g.fillStyle = "#16181c"; g.fillRect(0, 0, w, h); g.fillStyle = "#f4f2ea"; g.fillRect(4, 4, w - 8, h - 8);
      g.fillStyle = "#16181c"; g.font = "800 40px sans-serif"; g.textAlign = "center"; g.textBaseline = "middle"; g.fillText(String(text).slice(0, 9), w/2, h/2 + 2);
    });
  }
  // the black panel between the tail lamps, with the car's (made-up) name and its drive
  function badgeTex(spec){
    return canvasTex(256, 64, function(g, w, h){
      g.fillStyle = "#121315"; g.fillRect(0, 0, w, h);
      g.fillStyle = "#e8e6e0"; g.font = "600 22px serif"; g.textAlign = "center"; g.textBaseline = "middle";
      g.fillText(String(spec.name || "").toUpperCase().slice(0, 16), w/2, h/2 - 6);
      g.font = "700 11px sans-serif"; g.fillText((spec.drive || "").toUpperCase(), w/2, h/2 + 16);
      g.fillStyle = "#f2b33d"; g.fillRect(16, h/2 - 2, 6, 6);
    });
  }
  // a tail lamp: red with an amber band and a white reverse lamp, in a dark frame
  function tailTex(){
    return canvasTex(128, 48, function(g, w, h){
      g.fillStyle = "#1a1a1c"; g.fillRect(0, 0, w, h);
      g.fillStyle = "#e8452c"; g.fillRect(3, 3, w - 6, h*0.56);
      g.fillStyle = "#ff8a3a"; g.fillRect(3, 3 + h*0.56, w - 6, h*0.2);
      g.fillStyle = "#f2f0e8"; g.fillRect(3, h*0.8, w*0.42, h*0.16);
      g.fillStyle = "rgba(255,255,255,0.35)"; g.fillRect(6, 6, w - 12, 3);
    });
  }
  function doorTex(num, spec){
    return canvasTex(128, 128, function(g, w){
      g.fillStyle = "#ffffff"; g.beginPath(); g.arc(64, 64, 58, 0, Math.PI*2); g.fill();
      g.strokeStyle = spec.colors.stripe; g.lineWidth = 8; g.stroke();
      g.fillStyle = "#16181c"; g.font = "italic 900 64px sans-serif"; g.textAlign = "center"; g.textBaseline = "middle"; g.fillText(String(num), 64, 68);
    });
  }
  function roofTex(num){
    return canvasTex(128, 64, function(g, w, h){
      g.fillStyle = "#f4f2ea"; g.fillRect(0, 0, w, h); g.fillStyle = "#16181c"; g.font = "italic 900 46px sans-serif"; g.textAlign = "center"; g.textBaseline = "middle"; g.fillText(String(num), w/2, h/2 + 3);
    });
  }
  function glassTex(){
    return canvasTex(64, 64, function(g, w, h){
      var gr = g.createLinearGradient(0, 0, w, h); gr.addColorStop(0, "#2c3d5c"); gr.addColorStop(1, "#16203a"); g.fillStyle = gr; g.fillRect(0, 0, w, h);
      g.fillStyle = "rgba(190,210,240,0.22)"; g.beginPath(); g.moveTo(w*0.15, 0); g.lineTo(w*0.42, 0); g.lineTo(w*0.12, h); g.lineTo(0, h); g.lineTo(0, h*0.55); g.closePath(); g.fill();
    });
  }
  // knobbly gravel tread: staggered blocks with dark grooves (u runs round the tyre, v across it)
  function treadTex(){
    return canvasTex(256, 64, function(g, w, h){
      g.fillStyle = "#17181b"; g.fillRect(0, 0, w, h);
      for(var x = 0; x < w; x += 16){
        g.fillStyle = "#3d3f45"; g.fillRect(x + 1, 3, 11, h*0.28); g.fillRect(x + 7, h*0.36, 11, h*0.28); g.fillRect(x + 1, h*0.69, 11, h*0.28);
        g.fillStyle = "#4b4d54"; g.fillRect(x + 1, 3, 11, 3); g.fillRect(x + 7, h*0.36, 11, 3); g.fillRect(x + 1, h*0.69, 11, 3);
      }
    }, true);
  }
  // a multi-spoke rim: lip, eight spokes and a hub; the gaps are transparent (the brake shows through)
  function rimTex(color, blur){
    return canvasTex(256, 256, function(g, w){
      var c = 128;
      if(blur){
        // spun: the spokes smear into a disc of the rim colour over the dark gaps
        var gr = g.createRadialGradient(c, c, 10, c, c, 124); gr.addColorStop(0, color); gr.addColorStop(0.25, color); gr.addColorStop(0.32, "#55575c"); gr.addColorStop(0.85, "#6a6c70"); gr.addColorStop(0.9, color); gr.addColorStop(1, color);
        g.fillStyle = gr; g.beginPath(); g.arc(c, c, 124, 0, Math.PI*2); g.fill();
        g.globalAlpha = 0.55; g.fillStyle = color; g.beginPath(); g.arc(c, c, 108, 0, Math.PI*2); g.fill(); g.globalAlpha = 1;
      } else {
        g.lineWidth = 16; g.strokeStyle = color; g.beginPath(); g.arc(c, c, 116, 0, Math.PI*2); g.stroke();
        g.lineWidth = 3; g.strokeStyle = "#1a1b1e"; g.beginPath(); g.arc(c, c, 107, 0, Math.PI*2); g.stroke();
        g.fillStyle = color;
        for(var k = 0; k < 8; k++){
          g.save(); g.translate(c, c); g.rotate(k*Math.PI/4);
          g.beginPath(); g.moveTo(-9, 20); g.lineTo(9, 20); g.lineTo(6, 110); g.lineTo(-6, 110); g.closePath(); g.fill();
          g.restore();
        }
        g.beginPath(); g.arc(c, c, 34, 0, Math.PI*2); g.fill();
        g.strokeStyle = "#1a1b1e"; g.lineWidth = 3; g.stroke();
        g.fillStyle = "#3a3b40"; for(var n = 0; n < 5; n++){ var a = n*Math.PI*2/5; g.beginPath(); g.arc(c + Math.cos(a)*18, c + Math.sin(a)*18, 4, 0, Math.PI*2); g.fill(); }
        g.fillStyle = "#1a1b1e"; g.beginPath(); g.arc(c, c, 8, 0, Math.PI*2); g.fill();
      }
    });
  }
  // the tyre's sidewall: rubber with a fine raised ring and an original maker's name around it
  function wallTex(){
    return canvasTex(256, 256, function(g, w){
      var c = 128;
      g.fillStyle = "#26272b"; g.fillRect(0, 0, w, w);
      g.strokeStyle = "#34363b"; g.lineWidth = 3; g.beginPath(); g.arc(c, c, 92, 0, Math.PI*2); g.stroke();
      g.fillStyle = "#8d8f94"; g.font = "800 17px sans-serif"; g.textAlign = "center"; g.textBaseline = "middle";
      ["GRITWING", "GRAVEL 2"].forEach(function(word, wi){
        var a0 = wi ? Math.PI*0.25 : -Math.PI*0.75;
        for(var i = 0; i < word.length; i++){
          var a = a0 + (i - word.length/2)*0.13; g.save(); g.translate(c + Math.cos(a)*108, c + Math.sin(a)*108); g.rotate(a + Math.PI/2); g.fillText(word.charAt(i), 0, 0); g.restore();
        }
      });
    });
  }
  // side profiles: [along from the nose (m), height (m)] for the lower body and the glasshouse
  var SHAPES = {
    moraine:   {len: 3.95, w: 1.74, body: [[0, 0.24], [0, 0.55], [0.1, 0.66], [0.95, 0.75], [3.68, 0.80], [3.95, 0.74], [3.95, 0.24]],
                glass: [[1.0, 0.75], [1.62, 1.22], [3.2, 1.24], [3.72, 0.82]], wing: [3.3, 1.27, 0.3], arch: 0.06},
    saltcedar: {len: 4.25, w: 1.70, body: [[0, 0.24], [0, 0.52], [0.1, 0.62], [1.35, 0.71], [3.05, 0.76], [3.25, 0.80], [4.2, 0.80], [4.25, 0.72], [4.25, 0.24]],
                glass: [[1.4, 0.71], [2.0, 1.12], [2.95, 1.13], [3.4, 0.80]], wing: [3.95, 0.98, 0.32], arch: 0.04},
    pipit:     {len: 3.70, w: 1.66, body: [[0, 0.24], [0, 0.58], [0.1, 0.68], [0.75, 0.76], [3.5, 0.80], [3.70, 0.75], [3.70, 0.24]],
                glass: [[0.8, 0.76], [1.42, 1.25], [3.22, 1.26], [3.58, 0.84]], wing: [3.30, 1.30, 0.28], arch: 0.05}
  };
  function extrudeGeo(pts2, width, bevel, segs){
    var sh = pts2;
    if(!(pts2 instanceof THREE.Shape)){ sh = new THREE.Shape(); sh.moveTo(pts2[0][0], pts2[0][1]); for(var i = 1; i < pts2.length; i++) sh.lineTo(pts2[i][0], pts2[i][1]); sh.closePath(); }
    var g = new THREE.ExtrudeGeometry(sh, {depth: width, bevelEnabled: !!bevel, bevelThickness: bevel || 0, bevelSize: bevel || 0, bevelSegments: segs || 2, curveSegments: 10});
    // shape x (along from the nose) -> z (backward), shape y -> y, depth -> x (across)
    g.translate(0, 0, -width/2); g.rotateY(-Math.PI/2);
    if(g.attributes.uv) g.deleteAttribute("uv");
    return g;
  }
  // narrow a glasshouse toward its roof (tumblehome): x scaled by height
  function taper(g, y0, y1, k){
    var p = g.attributes.position;
    for(var i = 0; i < p.count; i++){ var u = clamp((p.getY(i) - y0)/(y1 - y0), 0, 1); p.setX(i, p.getX(i)*(1 - k*u)); }
    g.computeVertexNormals();
    return g;
  }
  // One draw call per material: merge a group's untextured meshes that share a material
  function bake(group){
    group.updateMatrixWorld(true);
    var inv = new THREE.Matrix4().copy(group.matrixWorld).invert(), by = {}, order = [];
    group.traverse(function(n){
      if(!n.isMesh || n.material.map || Array.isArray(n.material) || n.userData.keep) return;
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
    group.children.slice().forEach(function(c){ if(!c.isMesh && !c.isLight && c.children.length === 0 && !c.isSprite) group.remove(c); });
  }
  // mud that builds up on the lower body over a stage (a per-car uniform, 0..1)
  function muddy(m, U){
    m.onBeforeCompile = function(sh){
      sh.uniforms.uMud = U.mud; sh.uniforms.uMudCol = U.col;
      sh.vertexShader = "varying vec3 vMudP;\n" + sh.vertexShader.replace("#include <begin_vertex>", "#include <begin_vertex>\nvMudP = position;");
      sh.fragmentShader = "uniform float uMud; uniform vec3 uMudCol; varying vec3 vMudP;\n" + sh.fragmentShader.replace("#include <color_fragment>",
        "#include <color_fragment>\n" +
        "float mudN = sin(vMudP.z*9.0 + vMudP.x*3.0)*0.5 + sin(vMudP.z*23.0 - vMudP.x*7.0)*0.25;\n" +
        "float mudH = -0.3 + uMud*0.55 + mudN*0.06;\n" +
        "float mudK = uMud*(1.0 - smoothstep(mudH - 0.08, mudH + 0.04, vMudP.y));\n" +
        "diffuseColor.rgb = mix(diffuseColor.rgb, uMudCol, clamp(mudK, 0.0, 0.9));");
    };
    m.customProgramCacheKey = function(){ return "rally-mud"; };
    return m;
  }
  R.addCar = function(spec, o){
    o = o || {};
    var S = SHAPES[spec.id] || SHAPES.moraine, cs = spec.colors || {};
    var ghost = !!o.ghost, inked = !ghost;
    var MU = {mud: {value: 0}, col: {value: new THREE.Color(0x6e5a44)}};
    var mats = [];
    function mat(c, extra, mud){
      var p = {color: c}; for(var k in (extra || {})) p[k] = extra[k];
      if(ghost){ p.transparent = true; p.opacity = 0.38; p.depthWrite = false; }
      var m = toon(p); if(mud && !ghost) muddy(m, MU); mats.push(m); return m;
    }
    var paint = mat(o.color || cs.body, null, true), stripe = mat(cs.stripe, null, true), stripe2 = mat(cs.stripe2, null, true);
    var black = mat(0x1a1b1e, null, true), rubber = mat(0x1d1e21), trim = mat(0xdcdad3, null, true), chrome = mat(0xb9bdc2);
    var glass = mat(0xffffff, {map: glassTex()}), lampMat = mat(0xfff6d8, {emissive: 0x000000});
    var tailT = tailTex(), tailMat = mat(0xffffff, {map: tailT, emissive: 0xffffff, emissiveMap: tailT, emissiveIntensity: 0.12});
    var podMat = mat(0xfff2c0, {emissive: 0x000000});
    var car = new THREE.Group(), body = new THREE.Group(); car.add(body);
    // body frame: physics hardpoints sit at y = 0 (the CG); the ground is ~0.52 m below at rest
    var base = -0.52, nose = -(spec.a + (S.len - spec.a - spec.b)/2), BW = S.w - 0.2, BV = 0.06;
    // the side profile with the two wheel arches cut out of its bottom edge
    var ARCH = spec.radius + 0.1, alF = -spec.a - nose, alR = spec.b - nose, B0 = 0.32;
    var prof = S.body.map(function(q){ return [q[0], Math.max(B0, q[1])]; });
    var bsh = new THREE.Shape(); bsh.moveTo(prof[0][0], prof[0][1]);
    for(var bi = 1; bi < prof.length; bi++) bsh.lineTo(prof[bi][0], prof[bi][1]);
    [alR, alF].forEach(function(al){ bsh.lineTo(al + ARCH, B0); bsh.absarc(al, B0, ARCH, 0, Math.PI, false); });
    bsh.lineTo(prof[0][0], prof[0][1]);
    var bodyG = extrudeGeo(bsh, BW, BV, 2);
    var bodyMesh = new THREE.Mesh(bodyG, paint); bodyMesh.position.set(0, base, nose); body.add(bodyMesh);
    // the glasshouse, in paint (the windows are panels laid over it, leaving the frames)
    var g0 = S.glass, belt = g0[0][1], roofY = Math.max(g0[1][1], g0[2][1]), GW = S.w*0.84, GV = 0.035, TK = 0.16;
    var ghG = taper(extrudeGeo(g0, GW, GV, 2), belt, roofY + GV, TK);
    var gh = new THREE.Mesh(ghG, paint); gh.position.set(0, base, nose); body.add(gh);
    function halfAt(h){ return (GW/2 + GV)*(1 - TK*clamp((h - belt)/(roofY + GV - belt), 0, 1)); }
    function P3(al, h, x){ return [x, base + h, nose + al]; }
    function quad(A, B, C, D, m, keep){
      // A B (top edge) / D C (bottom edge), uv 0..1
      var g = new THREE.BufferGeometry(), p = [A, B, C, A, C, D], uv = [0, 1, 1, 1, 1, 0, 0, 1, 1, 0, 0, 0], pos = [];
      p.forEach(function(v){ pos.push(v[0], v[1], v[2]); });
      g.setAttribute("position", new THREE.Float32BufferAttribute(pos, 3)); g.setAttribute("uv", new THREE.Float32BufferAttribute(uv, 2)); g.computeVertexNormals();
      var me = new THREE.Mesh(g, m); me.material.side = THREE.DoubleSide; if(keep) me.userData.keep = true; body.add(me); return me;
    }
    // windscreen and rear window: panels on the sloped faces, inset from the edges
    [[0, 1, 0.1, 0.9], [2, 3, 0.1, 0.88]].forEach(function(e){
      var a = g0[e[0]], b = g0[e[1]], da = b[0] - a[0], dh = b[1] - a[1], L = Math.sqrt(da*da + dh*dh), na = -dh/L, nh = da/L, off = GV + 0.012;
      var lo = [a[0] + da*e[2] + na*off, a[1] + dh*e[2] + nh*off], hi = [a[0] + da*e[3] + na*off, a[1] + dh*e[3] + nh*off];
      if(lo[1] > hi[1]){ var t = lo; lo = hi; hi = t; }
      var wl = halfAt(lo[1]) - 0.1, wh = halfAt(hi[1]) - 0.1;
      quad(P3(hi[0], hi[1], -wh), P3(hi[0], hi[1], wh), P3(lo[0], lo[1], wl), P3(lo[0], lo[1], -wl), glass);
    });
    // side windows: the glass profile shrunk toward its middle, on each tapered side; a B-pillar splits them
    [-1, 1].forEach(function(sd){
      var cxp = 0, chp = 0; g0.forEach(function(q){ cxp += q[0]/4; chp += q[1]/4; });
      var ins = g0.map(function(q, i){ var k = i < 2 ? 0.8 : 0.84; return [cxp + (q[0] - cxp)*k, chp + (q[1] - chp)*(i === 0 || i === 3 ? 0.66 : 0.8) - (i === 0 || i === 3 ? 0.0 : 0.01)]; });
      var mid = (ins[1][0] + ins[2][0])/2, gap = 0.06;
      function sideQuad(a, b, c, d){
        function V(q){ return P3(q[0], q[1], sd*(halfAt(q[1]) + 0.008)); }
        var m = quad(V(a), V(b), V(c), V(d), glass); return m;
      }
      var botMid = [mid, lerp(ins[0][1], ins[3][1], (mid - ins[0][0])/(ins[3][0] - ins[0][0]))];
      var topF = [mid - gap, lerp(ins[1][1], ins[2][1], (mid - gap - ins[1][0])/(ins[2][0] - ins[1][0]))], topB = [mid + gap, topF[1]];
      sideQuad(ins[1], topF, [mid - gap, botMid[1]], ins[0]);
      sideQuad(topB, ins[2], ins[3], [mid + gap, botMid[1]]);
    });
    var roofLen = g0[2][0] - g0[1][0], roofMid = (g0[1][0] + g0[2][0])/2;
    [-1, 1].forEach(function(sd){
      // livery: a stripe down each side and a door number roundel
      var st1 = new THREE.Mesh(new THREE.BoxGeometry(0.02, 0.1, S.len*0.8), stripe); st1.position.set(sd*(BW/2 + BV + 0.008), base + 0.52, nose + S.len*0.5); body.add(st1);
      var st2 = new THREE.Mesh(new THREE.BoxGeometry(0.02, 0.045, S.len*0.8), stripe2); st2.position.set(sd*(BW/2 + BV + 0.009), base + 0.44, nose + S.len*0.5); body.add(st2);
      if(!ghost){
        var dm = new THREE.Mesh(new THREE.CircleGeometry(0.2, 20), mat(0xffffff, {map: doorTex(o.number || 7, spec)}));
        dm.position.set(sd*(BW/2 + BV + 0.02), base + 0.66, nose + S.len*0.46); dm.rotation.y = sd*Math.PI/2; body.add(dm);
      }
      // sills, wheel-arch flares, mud flaps, mirrors
      var sill = new THREE.Mesh(new THREE.BoxGeometry(0.07, 0.1, alR - alF - ARCH*2 - 0.1), black); sill.position.set(sd*(BW/2 + BV + 0.01), base + B0 + 0.04, nose + (alF + alR)/2); body.add(sill);
      // box flares over the arches, wide enough to cover the tyres that stand out past the body
      [spec.a, -spec.b].forEach(function(az){
        var fl = new THREE.Mesh(new THREE.CylinderGeometry(ARCH + 0.04, ARCH + 0.04, 0.22, 14, 1, true, 0, Math.PI), paint);
        fl.rotation.z = Math.PI/2; fl.position.set(sd*(BW/2 + BV + 0.07), base + B0, -az); fl.scale.set(1, 0.82, 1); body.add(fl);
        [-1, 1].forEach(function(e){ var lip = new THREE.Mesh(new THREE.BoxGeometry(0.18, 0.06, 0.1), paint); lip.position.set(sd*(BW/2 + BV + 0.07), base + B0 + 0.02, -az + e*(ARCH + 0.06)); body.add(lip); });
      });
            // short mud flaps, inboard of the tyres' outer shoulders so the tyres still show from behind
      [spec.a, -spec.b].forEach(function(az){ var mf = new THREE.Mesh(new THREE.BoxGeometry(0.2, 0.17, 0.02), black); mf.position.set(sd*(spec.track/2 + 0.08), base + 0.15, -az + ARCH + 0.04); body.add(mf); });
      // door shut lines (ink) and a side skirt
      [g0[0][0] + 0.04, (g0[1][0] + g0[2][0])/2 + 0.02].forEach(function(al){ var dl = new THREE.Mesh(new THREE.BoxGeometry(0.012, 0.42, 0.012), black); dl.position.set(sd*(BW/2 + BV + 0.004), base + 0.5, nose + al); body.add(dl); });
      var mir = new THREE.Mesh(new THREE.BoxGeometry(0.16, 0.1, 0.08), paint); mir.position.set(sd*(halfAt(g0[0][1] + 0.08) + 0.08), base + g0[0][1] + 0.08, nose + g0[0][0] + 0.12); body.add(mir);
      // head lamps
      var hl = new THREE.Mesh(new THREE.BoxGeometry(0.36, 0.14, 0.04), lampMat); hl.position.set(sd*0.56, base + 0.6, nose - BV - 0.01); body.add(hl);
    });
    // the tail: two wide lamps, a dark badge panel between them, a plate, the bumper, exhaust
    var rearZ = nose + S.len + BV + 0.012, tailY = base + Math.min(0.66, S.body[S.body.length - 2][1] - 0.12);
    [-1, 1].forEach(function(sd){
      var tl = new THREE.Mesh(new THREE.PlaneGeometry(0.42, 0.16), tailMat); tl.position.set(sd*(BW/2 - 0.2), tailY, rearZ); tl.userData.keep = true; body.add(tl);
    });
    if(!ghost){
      var bp = new THREE.Mesh(new THREE.PlaneGeometry(BW - 0.86, 0.15), mat(0xffffff, {map: badgeTex(spec)})); bp.position.set(0, tailY, rearZ); body.add(bp);
      var pmat = mat(0xffffff, {map: plateTex(spec.plate || "RALLY")});
      var pf = new THREE.Mesh(new THREE.PlaneGeometry(0.5, 0.12), pmat); pf.position.set(0, base + B0 + 0.05, nose - BV - 0.115); pf.rotation.y = Math.PI; body.add(pf);
      var pr = new THREE.Mesh(new THREE.PlaneGeometry(0.52, 0.13), pmat); pr.position.set(0, base + B0 + 0.17, rearZ + 0.002); body.add(pr);
      var rn = new THREE.Mesh(new THREE.PlaneGeometry(0.5, 0.25), mat(0xffffff, {map: roofTex(o.number || 7)})); rn.rotation.x = -Math.PI/2; rn.position.set(0, base + roofY + GV + 0.004, nose + roofMid + 0.15); body.add(rn);
    }
    var hb = new THREE.Mesh(new THREE.BoxGeometry(0.34, 0.035, 0.03), tailMat); hb.position.set(0, base + roofY - 0.02, nose + g0[2][0] + 0.03); hb.userData.keep = true; body.add(hb);
    var grille = new THREE.Mesh(new THREE.BoxGeometry(0.62, 0.14, 0.03), black); grille.position.set(0, base + 0.58, nose - BV - 0.01); body.add(grille);
    [nose - BV - 0.04, nose + S.len + BV + 0.04].forEach(function(z){ var bb = new THREE.Mesh(new THREE.BoxGeometry(BW - 0.1, 0.15, 0.14), black); bb.position.set(0, base + B0 + 0.05, z); body.add(bb); });
    var ex = new THREE.Mesh(new THREE.CylinderGeometry(0.05, 0.05, 0.22, 10), chrome); ex.rotation.x = Math.PI/2; ex.position.set(-0.42, base + B0 - 0.04, rearZ + 0.08); body.add(ex);
    var exIn = new THREE.Mesh(new THREE.CircleGeometry(0.036, 10), black); exIn.position.set(-0.42, base + B0 - 0.04, rearZ + 0.192); body.add(exIn);
    // a tow hook under the bumper
    var hook = new THREE.Mesh(new THREE.TorusGeometry(0.05, 0.016, 6, 12), mat(0xe2b23c)); hook.position.set(0.28, base + B0 - 0.05, rearZ + 0.12); hook.rotation.y = Math.PI/2; body.add(hook);
    // the rally light pod
    var pod = new THREE.Group();
    var bar = new THREE.Mesh(new THREE.BoxGeometry(1.1, 0.05, 0.05), black); pod.add(bar);
    [-0.42, -0.14, 0.14, 0.42].forEach(function(x){
      var cup = new THREE.Mesh(new THREE.CylinderGeometry(0.11, 0.11, 0.09, 12), black); cup.rotation.x = Math.PI/2; cup.position.set(x, 0, -0.03); pod.add(cup);
      var lens = new THREE.Mesh(new THREE.CircleGeometry(0.09, 12), podMat); lens.position.set(x, 0, -0.08); lens.rotation.y = Math.PI; pod.add(lens);
    });
    pod.position.set(0, base + 0.5, nose - 0.16); body.add(pod);
    // the wing: a blade on two stands, with end plates
    var wing = S.wing, wz = nose + wing[0] + wing[2]/2, ww = S.w*0.86;
    var blade = new THREE.Mesh(new THREE.BoxGeometry(ww, 0.035, wing[2]), trim); blade.position.set(0, base + wing[1], wz); blade.rotation.x = -0.08; body.add(blade);
    [-1, 1].forEach(function(sd){
      var ep = new THREE.Mesh(new THREE.BoxGeometry(0.03, 0.15, wing[2] + 0.08), trim); ep.position.set(sd*(ww/2 + 0.015), base + wing[1] + 0.02, wz); body.add(ep);
      var sp = new THREE.Mesh(new THREE.BoxGeometry(0.04, 0.16, 0.12), black); sp.position.set(sd*0.5, base + wing[1] - 0.08, wz); body.add(sp);
    });
    // roof vent / scoop
    var vent = new THREE.Mesh(new THREE.BoxGeometry(0.32, 0.07, 0.32), black); vent.position.set(0, base + roofY + GV + 0.03, nose + g0[1][0] + 0.32); body.add(vent);
    bake(body);
    body.traverse(function(n){ if(n.isMesh){ n.castShadow = !ghost; n.receiveShadow = !ghost; } });
    // ink outlines around the merged body parts
    var inkC = inked ? inkMat(2.4, 9) : null, hulls = [];
    if(inked){
      body.children.slice().forEach(function(n){
        if(!n.isMesh || n.material.map || n.material === glass) return;
        var hm = new THREE.Mesh(hullGeo(n.geometry), inkC); hm.position.copy(n.position); hm.quaternion.copy(n.quaternion); hm.scale.copy(n.scale); body.add(hm); hulls.push(hm);
      });
    }
    // wheels: suspension (y) > steer (y rot) > spin (x rot). Chunky tyres standing out past the
    // body under the flares: a treaded band, lettered sidewalls, a recessed multi-spoke rim (a
    // spun-blur version swaps in at speed) with the brake disc and a bright caliper behind it.
    var wheels = [], tread = treadTex(); tread.repeat.set(4, 1);
    var RD = spec.radius, TW = 0.29, RIN = RD*0.66;
    var tyreMat = mat(0xffffff, {map: tread, emissive: 0x18191c}), wallMat = mat(0xffffff, {map: wallTex(), emissive: 0x141518}), dishMat = mat(0x2c2d31, {side: THREE.DoubleSide});
    var rimSharp = rimTex(cs.rim || "#d0d0d0", false), rimBlur = rimTex(cs.rim || "#d0d0d0", true);
    var rimMat = mat(0xffffff, {map: rimSharp, alphaTest: 0.5, side: THREE.DoubleSide}), discMat = mat(0x8e9298), calMat = mat(ghost ? 0xd23a2a : (spec.id === "saltcedar" ? 0xe2b23c : 0xd23a2a));
    var tyreG = new THREE.CylinderGeometry(RD, RD, TW, 28, 1, true); tyreG.rotateZ(Math.PI/2);
    var tyreH = inked ? hullGeo(new THREE.CylinderGeometry(RD, RD, TW, 28, 1).rotateZ(Math.PI/2)) : null;
    var wallG = new THREE.RingGeometry(RIN, RD, 28, 1), rimG = new THREE.CircleGeometry(RIN*0.98, 28), discG = new THREE.CircleGeometry(RD*0.5, 20);
    var dishG = new THREE.CylinderGeometry(RIN, RIN, 0.07, 20, 1, true); dishG.rotateZ(Math.PI/2);
    var calG = new THREE.BoxGeometry(0.05, 0.14, 0.09);
    [[-1, 1], [1, 1], [-1, -1], [1, -1]].forEach(function(w){
      var sus = new THREE.Group(), steer = new THREE.Group(), spin = new THREE.Group(), o2 = w[0];
      sus.position.set(o2*(spec.track/2 + 0.2), 0, w[1] > 0 ? -spec.a : spec.b);
      var tyre = new THREE.Mesh(tyreG, tyreMat); tyre.castShadow = !ghost; spin.add(tyre);
      [1, -1].forEach(function(face){
        var wl = new THREE.Mesh(wallG, wallMat); wl.position.x = o2*face*TW/2; wl.rotation.y = o2*face*Math.PI/2; spin.add(wl);
      });
      var dish = new THREE.Mesh(dishG, dishMat); dish.position.x = o2*(TW/2 - 0.035); spin.add(dish);
      var rim = new THREE.Mesh(rimG, rimMat); rim.position.x = o2*(TW/2 - 0.045); rim.rotation.y = o2*Math.PI/2; spin.add(rim);
      var disc = new THREE.Mesh(discG, discMat); disc.position.x = o2*(TW/2 - 0.1); disc.rotation.y = o2*Math.PI/2; spin.add(disc);
      var cal = new THREE.Mesh(calG, calMat); cal.position.set(o2*(TW/2 - 0.085), RD*0.3, RD*0.26); cal.rotation.x = -0.7; steer.add(cal);
      if(tyreH){ var th = new THREE.Mesh(tyreH, inkC); spin.add(th); }
      steer.add(spin); sus.add(steer); car.add(sus);
      wheels.push({sus: sus, steer: steer, spin: spin});
    });
    var h = {g: car, body: body, wheels: wheels, spec: spec, ghost: ghost, lamps: [lampMat, podMat], tail: tailMat, lights: null, tag: null, mats: mats, mud: MU.mud, ink: inkC, hulls: hulls,
             rim: {mat: rimMat, sharp: rimSharp, blur: rimBlur, on: false}};
    if(inkC) mats.push(inkC);
    if(o.name){
      var tc = document.createElement("canvas"), tg = tc.getContext("2d"); tc.width = 256; tc.height = 64;
      tg.font = "700 30px sans-serif"; tg.textAlign = "center"; tg.textBaseline = "middle";
      var tw = Math.min(250, tg.measureText(o.name).width + 28);
      tg.fillStyle = "rgba(20,24,32,0.72)"; tg.fillRect(128 - tw/2, 8, tw, 48); tg.fillStyle = "#ffffff"; tg.fillText(String(o.name).slice(0, 18), 128, 33);
      var tt = new THREE.CanvasTexture(tc); tt.colorSpace = THREE.SRGBColorSpace;
      var spt = new THREE.Sprite(new THREE.SpriteMaterial({map: tt, depthTest: false, transparent: true})); spt.scale.set(2.6, 0.65, 1); spt.position.set(0, 1.6, 0); spt.renderOrder = 10;
      car.add(spt); h.tag = spt; mats.push(spt.material);
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
      var coneG = new THREE.ConeGeometry(2.6, 14, 16, 1, true); coneG.translate(0, -7, 0); coneG.rotateX(Math.PI/2);   // apex at the lamp, opening forward (-z)
      var coneM = new THREE.MeshBasicMaterial({color: 0xfff3d6, transparent: true, opacity: 0.055, depthWrite: false, side: THREE.DoubleSide, blending: THREE.AdditiveBlending});
      [-0.55, 0.55].forEach(function(x){ var c = new THREE.Mesh(coneG, coneM); c.position.set(x, -0.05, -2.0); c.rotation.x = -0.05; h.body.add(c); L.push(c); });
      var tailL = new THREE.PointLight(0xff3020, 0.35, 4, 2); tailL.position.set(0, -0.2, 2.4); h.body.add(tailL); L.push(tailL);
      h.lights = L;
    }
    if(h.lights) h.lights.forEach(function(l){ l.visible = !!on; });
    h.lit = !!on;
    h.lamps.forEach(function(m){ m.emissive.setHex(on ? 0xfff2c8 : 0x000000); m.emissiveIntensity = on ? 1.2 : 0; });
  };
  R.removeCar = function(h){
    if(!h) return; scene.remove(h.g);
    h.g.traverse(function(n){ if(n.geometry) n.geometry.dispose(); });
    h.mats.forEach(function(m){ if(m.map) m.map.dispose(); m.dispose(); });
    if(h.rim){ h.rim.sharp.dispose(); h.rim.blur.dispose(); }
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
    // the rims blur once they spin fast (from the physics wheel speeds, or the stand-in's travel)
    var om = 0;
    if(car && car.wheels) for(var k = 0; k < 4; k++) om = Math.max(om, Math.abs(car.wheels[k].omega || 0));
    if(car && car.speedHint != null) om = Math.max(om, car.speedHint/spec.radius);
    var blurOn = om > 38;
    if(h.rim && blurOn !== h.rim.on){ h.rim.on = blurOn; h.rim.mat.map = blurOn ? h.rim.blur : h.rim.sharp; h.rim.mat.needsUpdate = true; }
    var brk = car && car.tel ? car.tel.brake > 0.1 : false;
    h.tail.emissiveIntensity = brk ? 1.25 : (h.lit ? 0.55 : 0.12);
  };
  R.setMud = function(h, v){ if(h && h.mud) h.mud.value = clamp(v, 0, 1); };

  /* ---------- particles: dust, gravel, spray (one pooled Points with a small shader) ---------- */
  var FX = null, PMAX = 1800;
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
      uniforms: {uLight: {value: R.stage && R.stage.night ? 0.32 : 1}, uScale: {value: 400}, uFog: {value: new THREE.Vector3(fc.r, fc.g, fc.b)}, uFogNear: {value: scene.fog ? scene.fog.near : 100}, uFogFar: {value: scene.fog ? scene.fog.far : 500}},
      vertexShader: "attribute float aSize; attribute float aAlpha; attribute vec3 aColor; varying float vA; varying vec3 vC; varying float vD; uniform float uScale;" +
        "void main(){ vec4 mv = modelViewMatrix*vec4(position,1.0); vD = -mv.z; gl_PointSize = aSize*uScale/max(0.5,-mv.z); vA = aAlpha*clamp((-mv.z - 0.6)/2.5, 0.0, 1.0); vC = aColor; gl_Position = projectionMatrix*mv; }",
      // a soft round puff, a little lighter on top (lit from above); linear colours, converted for the screen
      fragmentShader: "varying float vA; varying vec3 vC; varying float vD; uniform vec3 uFog; uniform float uFogNear; uniform float uFogFar; uniform float uLight;" +
        "void main(){ vec2 d = gl_PointCoord - 0.5; float r = dot(d,d)*4.0; if(r > 1.0) discard; float a = vA*pow(1.0 - r, 1.6);" +
        "vec3 c = vC*uLight*(1.04 - d.y*0.22);" +
        "float f = clamp((vD - uFogNear)/(uFogFar - uFogNear), 0.0, 1.0); gl_FragColor = vec4(mix(c, uFog, f), a*(1.0 - f*0.6));" +
        "\n#include <colorspace_fragment>\n}",
      transparent: true, depthWrite: false
    });
    var pts = new THREE.Points(g, mat); pts.frustumCulled = false; pts.renderOrder = 5; scene.add(pts);
    FX = {pts: pts, g: g, n: 0, list: [], next: 0, tr: null};
    for(var i = 0; i < PMAX; i++) FX.list.push({life: 0});
    var TM = QUALITY.high.tracks, tg = new THREE.BufferGeometry();
    var tpos = new Float32Array(TM*6*3), tcol = new Float32Array(TM*6*4);
    tg.setAttribute("position", new THREE.BufferAttribute(tpos, 3).setUsage(THREE.DynamicDrawUsage));
    tg.setAttribute("color", new THREE.BufferAttribute(tcol, 4).setUsage(THREE.DynamicDrawUsage));
    tg.boundingSphere = new THREE.Sphere(new THREE.Vector3(), 1e6);
    var tm = new THREE.MeshBasicMaterial({vertexColors: true, transparent: true, depthWrite: false, polygonOffset: true, polygonOffsetFactor: -4, polygonOffsetUnits: -4});
    var tmesh = new THREE.Mesh(tg, tm); tmesh.frustumCulled = false; tmesh.renderOrder = 2; scene.add(tmesh);
    FX.tr = {mesh: tmesh, g: tg, max: Qd.tracks, at: 0, last: {}, dirty: false, count: 0};
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
    var p = FX.list[FX.next]; FX.next = (FX.next + 1) % Qd.parts;
    p.x = x; p.y = y; p.z = z; p.vx = vx; p.vy = vy; p.vz = vz; p.s = size; p.gr = grow; p.life = life; p.max = life; p.c = color; p.a = alpha; p.grav = grav;
  }
  // light, surface-tinted dust (tan on gravel and dirt, white on snow, brown on mud, pale smoke on tarmac)
  var DUST = {1: 0xe2d2b2, 2: 0xd8bf9a, 3: 0xb9ab8c, 4: 0x9a7d5c, 5: 0xf6f9fd, 6: 0xe6d6b6, 0: 0xdcdcdc};
  var CHIP = {1: 0xb39a76, 2: 0xa88a64, 3: 0x8c7a56, 4: 0x6e5236, 5: 0xffffff, 6: 0xb8a07c, 0: 0x999999};
  var MUD_RATE = {1: 0.006, 2: 0.008, 3: 0.004, 4: 0.03, 5: 0.002, 6: 0.006, 0: -0.004};
  R.fx = function(car, dt, calmOn, h){
    if(!FX || !car || !car.wheels) return;
    var M2 = car.M, fwdx = -M2[2], fwdz = -M2[8], i, mudGain = 0;
    for(i = 0; i < 4; i++){
      var w = car.wheels[i];
      if(!w.contact) { FX.tr.last[i] = null; continue; }
      var sid = w.surf, loose = sid !== 0;
      var slip = Math.min(3, w.slip || 0), wheelSpeed = Math.abs(w.omega*car.spec.radius), ground = Math.abs(w.vx) + Math.abs(w.vy);
      mudGain += (MUD_RATE[sid] || 0)*Math.min(1, ground/15)*(1 + slip*0.5);
      var amount = (loose ? 0.3*Math.min(1, ground/16) : 0) + Math.max(0, slip - 1)*0.45 + (loose ? Math.min(1.5, Math.abs(wheelSpeed - Math.abs(w.vx))*0.06) : 0);
      if(sid === 0 && slip < 1.3) amount = 0;
      if(sid === 3) amount *= 0.35;                 // grass throws clods, not clouds
      if(calmOn) amount *= 0.4;
      var n2 = amount*dt*(low ? 30 : 80);
      while(n2 > 0){
        if(n2 < 1 && fxr() > n2) break;
        n2 -= 1;
        var big = sid === 5 ? 1.3 : sid === 4 ? 0.8 : 1;
        // a billow: thrown back and up off the wheel, slowing quickly, growing as it drifts
        emit(w.cp[0] + (fxr() - 0.5)*0.5, w.cp[1] + 0.2, w.cp[2] + (fxr() - 0.5)*0.5,
             // (carried along a little with the car, so a slide's cloud hangs beside it)
             -fwdx*(1.5 + fxr()*2.5) + (fxr() - 0.5)*2 + car.v[0]*0.4, 0.5 + fxr()*1.2, -fwdz*(1.5 + fxr()*2.5) + (fxr() - 0.5)*2 + car.v[2]*0.4,
             (1.0 + fxr()*0.8)*big, 2.6 + fxr()*1.8, 1.8 + fxr()*1.6, DUST[sid] || 0xd0c4b0, sid === 0 ? 0.3 : 0.5, -0.3);
        // a few small, light chips (gravel, mud, snow) off a spinning or sliding wheel
        if(loose && sid !== 3 && (slip > 1.2 || wheelSpeed > Math.abs(w.vx) + 3) && fxr() < 0.15){
          emit(w.cp[0], w.cp[1] + 0.1, w.cp[2], -fwdx*(3 + fxr()*4) + (fxr() - 0.5)*2, 1.0 + fxr()*2, -fwdz*(3 + fxr()*4) + (fxr() - 0.5)*2,
               0.04 + fxr()*0.03, 0, 0.25 + fxr()*0.15, CHIP[sid] || 0xa08c70, 0.85, 9.8);
        }
      }
      // tyre tracks: a quad every ~0.3 m of travel
      var T = FX.tr, L = T.last[i], mark = loose ? 0.34 + Math.min(0.42, slip*0.2) : Math.max(0, slip - 1.1)*0.5;
      if(sid === 5) mark = 0.28 + Math.min(0.4, slip*0.15);
      var px = w.cp[0], pz = w.cp[2], py = w.cp[1] + 0.05;
      if(L){
        var dx = px - L.x, dz = pz - L.z, d = Math.sqrt(dx*dx + dz*dz);
        if(d > 0.3){
          if(mark > 0.03 && d < 3) addTrack(L, {x: px, y: py, z: pz}, mark, sid);
          T.last[i] = {x: px, y: py, z: pz};
        }
      } else T.last[i] = {x: px, y: py, z: pz};
    }
    if(h && h.mud) h.mud.value = clamp(h.mud.value + mudGain*dt*0.25, 0, 1);
  };
  function addTrack(A, B, a, sid){
    var T = FX.tr, dx = B.x - A.x, dz = B.z - A.z, d = Math.sqrt(dx*dx + dz*dz) || 1, nx = -dz/d*0.14, nz = dx/d*0.14;
    var pos = T.g.attributes.position.array, col = T.g.attributes.color.array, o = T.at*18, oc = T.at*24;
    var v = [[A.x - nx, A.y, A.z - nz], [B.x - nx, B.y, B.z - nz], [A.x + nx, A.y, A.z + nz], [A.x + nx, A.y, A.z + nz], [B.x - nx, B.y, B.z - nz], [B.x + nx, B.y, B.z + nz]];
    var c = sid === 5 ? [0.5, 0.56, 0.66] : sid === 0 ? [0.03, 0.03, 0.035] : sid === 4 ? [0.12, 0.08, 0.05] : [0.16, 0.12, 0.085];
    for(var k = 0; k < 6; k++){ pos[o + k*3] = v[k][0]; pos[o + k*3 + 1] = v[k][1]; pos[o + k*3 + 2] = v[k][2]; col[oc + k*4] = c[0]; col[oc + k*4 + 1] = c[1]; col[oc + k*4 + 2] = c[2]; col[oc + k*4 + 3] = Math.min(0.72, a); }
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
      pos[n*3] = p.x; pos[n*3+1] = p.y; pos[n*3+2] = p.z; size[n] = p.s; alpha[n] = p.grav > 1 ? p.a*Math.min(1, u*3) : p.a*Math.min(1, (1 - u)*7)*Math.min(1, u*u*(3 - 2*u)*1.6);
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
  var LX = new THREE.Vector3(), LY = new THREE.Vector3(), UP = new THREE.Vector3(0, 1, 0), SC = new THREE.Vector3();
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
      // chase: low and close behind; only its heading and height are smoothed (faster the faster
      // you go), so the car stays the same size on screen. It swings toward the direction of
      // travel in a slide (look-ahead), and mode 3 orbits after the finish.
      var fx = F.x, fz = F.z, fl = Math.sqrt(fx*fx + fz*fz) || 1; fx /= fl; fz /= fl;
      var vl = vel.length(), vx = vl > 3 ? vel.x/vl : fx, vz = vl > 3 ? vel.z/vl : fz;
      var want = Math.atan2(lerp(fx, vx, 0.35), -lerp(fz, vz, 0.35));
      if(c.mode === 3) want = Math.atan2(fx, -fz) + 0.55 + (c.calm ? 0 : 0.55*Math.sin(cam.t*0.35));
      if(!cam.init || c.calm){ cam.yaw = want; cam.h = P.p[1]; cam.init = true; }
      else {
        var dy = Math.atan2(Math.sin(want - cam.yaw), Math.cos(want - cam.yaw));
        cam.yaw += dy*Math.min(1, dt*(c.mode === 3 ? 6 : 2.6 + 4*Math.min(1, spd/30)));
        cam.h += (P.p[1] - cam.h)*Math.min(1, dt*6);
      }
      var dist = c.mode === 3 ? 7.5 : 4.9 + Math.min(0.9, spd*0.022), height = c.mode === 3 ? 2.6 : 1.28;
      var cx2 = Math.sin(cam.yaw), cz2 = -Math.cos(cam.yaw);
      TMP.set(P.p[0] - cx2*dist, cam.h + height, P.p[2] - cz2*dist);
      if(R.stage){
        // never inside a tree: walk the camera in toward the car while a trunk (and its crown) is in the way
        var px0 = P.p[0], pz0 = P.p[2];
        for(var tries = 0; tries < 6; tries++){
          var blocked = false, cxx = TMP.x, czz = TMP.z;
          R.stage.env.obstacles(cxx, czz, 2.2, function(ox, oz, orr){ var ddx = cxx - ox, ddz = czz - oz; if(ddx*ddx + ddz*ddz < (orr + 1.9)*(orr + 1.9)) blocked = true; });
          if(!blocked) break;
          TMP.x = px0 + (TMP.x - px0)*0.75; TMP.z = pz0 + (TMP.z - pz0)*0.75; TMP.y += 0.35;
        }
        var gh = R.stage.env.height(TMP.x, TMP.z) + 0.8; if(TMP.y < gh) TMP.y = gh;
      }
      cam.p.copy(TMP);
      var ahead = c.mode === 3 ? 0 : 6 + Math.min(10, spd*0.3);
      cam.look.set(P.p[0] + vx*ahead, P.p[1] + (c.mode === 3 ? 0.4 : 0.62), P.p[2] + vz*ahead);
      camera.position.copy(cam.p);
    } else {
      // bumper (1) or hood (2): fixed to the body
      var fa = car && car.spec ? car.spec.a : 1.1;
      var off2 = c.mode === 1 ? [0, -0.2, fa + 0.95] : [0, 0.5, fa*0.8];
      camera.position.set(P.p[0], P.p[1], P.p[2]).addScaledVector(RT, off2[0]).addScaledVector(U, off2[1]).addScaledVector(F, off2[2]);
      cam.look.copy(camera.position).addScaledVector(F, 20).addScaledVector(U, c.mode === 1 ? 0.5 : -0.1);
    }
    if(shakeAmt > 0){
      var t = cam.t*31;
      camera.position.x += Math.sin(t*1.7)*shakeAmt; camera.position.y += Math.sin(t*2.3 + 1)*shakeAmt; camera.position.z += Math.cos(t*1.9)*shakeAmt;
    }
    // (screenshots) a turntable view of the car: {yaw from behind, dist, height}
    if(R.debugView){ var dv = R.debugView, hd = Math.atan2(F.x, -F.z) + dv.yaw; camera.position.set(P.p[0] - Math.sin(hd)*dv.dist, P.p[1] + dv.height, P.p[2] + Math.cos(hd)*dv.dist); cam.look.set(P.p[0], P.p[1] - 0.1, P.p[2]); }
    camera.up.set(0, 1, 0);
    if(c.mode === 1 || c.mode === 2){ camera.up.copy(U); }
    camera.lookAt(cam.look);
    var fov = c.calm ? 58 : 56 + Math.min(10, spd*0.2);
    if(c.mode === 1) fov += 6;
    if(Math.abs(camera.fov - fov) > 0.05){ camera.fov += (fov - camera.fov)*Math.min(1, dt*3); camera.updateProjectionMatrix(); }
    // the sun's shadow box: centred a little ahead of the car, snapped to whole shadow texels (no shimmer)
    SC.set(P.p[0] + F.x*Qd.shR*0.45, P.p[1], P.p[2] + F.z*Qd.shR*0.45);
    LX.crossVectors(UP, sunDir).normalize(); LY.crossVectors(sunDir, LX);
    var ts = 2*Qd.shR/Qd.shadow, la = Math.round(SC.dot(LX)/ts)*ts, lb = Math.round(SC.dot(LY)/ts)*ts, lc = SC.dot(sunDir);
    SC.copy(LX).multiplyScalar(la).addScaledVector(LY, lb).addScaledVector(sunDir, lc);
    sun.target.position.copy(SC); sun.position.copy(SC).addScaledVector(sunDir, 180);
    if(sky) sky.position.copy(camera.position);
    for(var bi = 0; bi < bands.length; bi++) bands[bi].position.set(camera.position.x, camera.position.y*0.35 + bands[bi].userData.dy, camera.position.z);
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
  function syncRes(){ renderer.getDrawingBufferSize(RES.value); }
  R.size = function(w, h){ renderer.setSize(w, h, false); camera.aspect = w/h; camera.updateProjectionMatrix(); syncRes(); };
  R.setPixelRatio = function(r){ renderer.setPixelRatio(r); syncRes(); };
  // switch quality live (render scale, shadow map, outlines, grass, effects); antialiasing is fixed at creation
  R.setQuality = function(q){
    if(!QUALITY[q] || q === qName) return;
    qName = q; Qd = QUALITY[q]; R.quality = q; low = q === "low"; R.low = low;
    renderer.setPixelRatio(Math.min(root.devicePixelRatio || 1, Qd.pr)); syncRes();
    shadowSetup();
    if(FX){ FX.tr.max = Math.min(QUALITY.high.tracks, Qd.tracks); FX.tr.at = FX.tr.at % FX.tr.max; FX.tr.count = Math.min(FX.tr.count, FX.tr.max); FX.tr.dirty = true; }
  };
  R.info = function(){
    var vis = {chunks: 0, fine: 0, deco: 0, inst: 0, ink: 0, other: 0};
    R.chunks.forEach(function(c){ if(c.coarse.visible) vis.chunks++; if(c.fine && c.fine.visible) vis.fine++; });
    decoBuckets.forEach(function(b){ if(b.mesh.visible){ vis.inst++; if(b.ink) vis.ink++; } });
    var meshes = 0; scene.traverseVisible(function(n){ if(n.isMesh || n.isPoints || n.isSprite) meshes++; });
    vis.meshes = meshes;
    return {calls: renderer.info.render.calls, tris: renderer.info.render.triangles, chunks: built, vis: vis, quality: qName};
  };
  R.dispose = function(){
    R.alive = false;
    R.cars.slice().forEach(R.removeCar);
    R.clearStage();
    if(sky){ scene.remove(sky); sky.geometry.dispose(); sky.material.dispose(); }
    cloudTex.forEach(function(t){ t.dispose(); });
    bands.forEach(function(b){ scene.remove(b); b.geometry.dispose(); b.material.map.dispose(); b.material.dispose(); }); bands = [];
    RAMP_OBJ.dispose(); RAMP_LAND.dispose();
    renderer.dispose();
    try { var ext = renderer.getContext().getExtension("WEBGL_lose_context"); if(ext) ext.loseContext(); } catch(e){}
  };
  return R;
}

HQV.rallyScene = {create: create, SPONSORS: SPONSORS, QUALITY: QUALITY};
})(typeof window !== "undefined" ? window : this);
