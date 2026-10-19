/* Claude HQ: the shared 3D game engine (HQ 2.0, Wave 0).
 *
 * Kart Racing, Platformer Rush and Blaster Arena each carried their own copy of
 * these parts; the 3D HQ uses them too. Loaded once, after core.js and before
 * the games (VALLEY_FILES in ui/app/10-views.js), as HQV.engine.
 *
 *   util    now, clamp, hex, angLerp, calm, say, tokens, myClock, fmtTime, dist3
 *   three   lib()           three.js + GLTFLoader + SkeletonUtils, loaded once
 *           loadGlb(lib, kit, name)   a Kenney model from /games/<kit>/, cached
 *           hasWebGL2()
 *   net     senderTime(P, q, t, jitMax)   a remote frame's time on our clock
 *           jitter(P, late)               the running jitter estimate
 *           pushSnap(sn, s, cap)          append to a snapshot buffer, bounded
 *           bracket(sn, rt)               the two snapshots around render time
 *
 * Same house rules as core.js: no build step, nothing leaves the machine.
 */
(function(){
"use strict";
var HQV = window.HQV = window.HQV || {};

/* ---------- util ---------- */
function now(){ return performance.now()/1000; }
function clamp(v, a, b){ return v < a ? a : v > b ? b : v; }
function hex(s, fb){ return typeof s === "string" && /^#[0-9a-f]{6}$/i.test(s) ? parseInt(s.slice(1), 16) : fb; }
function angLerp(a, b, u){ return a + Math.atan2(Math.sin(b - a), Math.cos(b - a))*u; }
function calm(){ return document.documentElement.classList.contains("hq-calm") ||
  (window.matchMedia && window.matchMedia("(prefers-reduced-motion: reduce)").matches); }
function say(t){ if(typeof window.announce === "function"){ try { window.announce(t); } catch(e){} } }
// Theme colours for 2D canvases (gauges, minimaps, map views) from the page's tokens.
function tokens(){
  var cs = getComputedStyle(document.documentElement);
  function g(n, fb){ var v = (cs.getPropertyValue(n) || "").trim(); return v || fb; }
  return {ink: g("--ink", "currentColor"), muted: g("--muted", "gray"), line: g("--line", "gray"), panel: g("--panel", "canvas"),
          panel2: g("--panel2", "canvas"), brand: g("--brand", "royalblue"), need: g("--need", "crimson"), good: g("--good", "seagreen"),
          gold: g("--gold", "goldenrod"), bg2: g("--bg2", "canvas"), mono: g("--mono", "monospace")};
}
// My clock for the server, centiseconds (never goes backwards; a reload starts over).
function myClock(){ return Math.floor(performance.now()/10) % 1073741824; }
// m:ss.cc for race and run times.
function fmtTime(ms){
  if(ms == null || !isFinite(ms)) return "–";
  ms = Math.max(0, Math.round(ms)); var m = Math.floor(ms/60000), s = (ms % 60000)/1000;
  return m+":"+(s < 10 ? "0" : "")+s.toFixed(2);
}
function dist3(a, b){ var dx = a[0] - b[0], dy = a[1] - b[1], dz = a[2] - b[2]; return Math.sqrt(dx*dx + dy*dy + dz*dz); }

/* ---------- three.js and models ---------- */
var KITS = ["golf", "kart", "platformer", "fps", "hq"];
var LIB = null, GLB = {};
// three.js, its GLTF loader and SkeletonUtils, imported once for every game.
// Each Kenney kit's GLBs point at their own "Textures/colormap.png"; one copy sits
// next to them in /games/<kit>/.
function lib(){
  return LIB || (LIB = Promise.all([import("/games/vendor/three-module.js"), import("/games/vendor/three-gltf-loader.js"),
                                    import("/games/vendor/three-skeleton-utils.js")])
    .then(function(m){
      var THREE = m[0], manager = new THREE.LoadingManager();
      manager.setURLModifier(function(u){
        var k = /\/games\/([a-z]+)\/Textures\/colormap\.png$/.exec(u);
        return k && KITS.indexOf(k[1]) >= 0 ? "/games/"+k[1]+"/colormap.png" : u;
      });
      return {THREE: THREE, loader: new m[1].GLTFLoader(manager), clone: m[2].clone, tex: new THREE.TextureLoader()};
    }, function(e){ LIB = null; throw e; }));
}
// A model from /games/<kit>/<name>.glb, loaded once and shared (clone it with lib.clone).
function loadGlb(L, kit, name){
  if(!/^[a-z][a-z0-9-]{0,40}$/.test(name) || KITS.indexOf(kit) < 0) return Promise.reject(new Error("bad model name"));
  var key = kit+"/"+name;
  return GLB[key] || (GLB[key] = new Promise(function(res, rej){
    L.loader.load("/games/"+key+".glb", res, null, function(e){ delete GLB[key]; rej(e); });
  }));
}
function hasWebGL2(){ try { return !!document.createElement("canvas").getContext("webgl2"); } catch(e){ return false; } }

/* ---------- snapshot interpolation ---------- */
// A remote player's frame carries its sender's clock q (centiseconds). Map it onto
// our clock: keep the smallest offset seen (the least-delayed frame), drift slowly
// toward newer ones, and track how late frames run (jitter) so the render delay can
// cover it. Returns the frame's time on our clock, or null for a stale frame.
// P holds lastQ (-1 at first), off (null at first) and jit (0 at first).
function senderTime(P, q, t, jitMax){
  q = q|0;
  if(P.lastQ >= 0 && q <= P.lastQ && P.lastQ - q < 6000) return null;   // older than what we have
  if(P.lastQ >= 0 && q <= P.lastQ) P.off = null;                          // the sender reloaded: start over
  P.lastQ = q;
  var off = t - q/100;
  if(P.off === null || off < P.off) P.off = off; else P.off += (off - P.off)*0.01;
  var st = q/100 + P.off;
  jitter(P, clamp(t - st, 0, jitMax));
  return st;
}
// Rise fast, fall slowly: one late frame widens the buffer, a calm spell narrows it.
function jitter(P, late){ P.jit += (late - P.jit)*(late > P.jit ? 0.3 : 0.02); }
// Append s (with s.t) keeping times strictly increasing and at most cap entries.
function pushSnap(sn, s, cap){
  var last = sn[sn.length - 1];
  if(last && s.t <= last.t) s.t = last.t + 0.001;
  sn.push(s);
  if(sn.length > cap) sn.shift();
  return s;
}
// The snapshots around render time rt: {A, B, u} between two, {A: first} before
// the buffer, {A: last, after: rt - last.t} past it, or null when it is empty.
function bracket(sn, rt){
  var n = sn.length; if(!n) return null;
  if(rt <= sn[0].t) return {A: sn[0], B: null, u: 0};
  if(rt >= sn[n-1].t) return {A: sn[n-1], B: null, u: 0, after: rt - sn[n-1].t};
  for(var i = n - 1; i > 0 && sn[i-1].t > rt; i--){}
  var A = sn[i-1], B = sn[i];
  return {A: A, B: B, u: (rt - A.t)/((B.t - A.t) || 1)};
}

/* ---------- sound (HQ 2.1 sound pack): every sound is synthesised, no files ---------- */
// One volume for every game and the HQ: localStorage hq_sfx (0..1). Off by default under Calm
// mode or reduced motion (until you set a volume yourself). Audio starts after your first click.
var SND = {ctx: null, master: null};
function sfxVolume(){
  var v = null; try { v = localStorage.getItem("hq_sfx"); } catch(e){}
  if(v === null || v === "") return calm() ? 0 : 0.5;
  v = parseFloat(v); return isFinite(v) ? clamp(v, 0, 1) : 0.5;
}
function sfxSetVolume(v){ try { localStorage.setItem("hq_sfx", String(clamp(+v || 0, 0, 1))); } catch(e){} if(SND.master) SND.master.gain.value = sfxVolume(); }
function audio(){
  if(SND.ctx) { if(SND.ctx.state === "suspended") SND.ctx.resume().catch(function(){}); return SND.ctx; }
  var C = window.AudioContext || window.webkitAudioContext; if(!C) return null;
  try { SND.ctx = new C(); SND.master = SND.ctx.createGain(); SND.master.gain.value = sfxVolume(); SND.master.connect(SND.ctx.destination); } catch(e){ return null; }
  return SND.ctx;
}
function tone(ac, type, f0, f1, dur, vol, at){
  var t = ac.currentTime + (at || 0), o = ac.createOscillator(), g = ac.createGain();
  o.type = type; o.frequency.setValueAtTime(f0, t); if(f1) o.frequency.exponentialRampToValueAtTime(Math.max(20, f1), t + dur);
  g.gain.setValueAtTime(0.0001, t); g.gain.exponentialRampToValueAtTime(vol, t + 0.008); g.gain.exponentialRampToValueAtTime(0.0001, t + dur);
  o.connect(g); g.connect(SND.master); o.start(t); o.stop(t + dur + 0.02);
}
function noise(ac, dur, vol, freq, at){
  var t = ac.currentTime + (at || 0), n = Math.floor(ac.sampleRate*dur), b = ac.createBuffer(1, n, ac.sampleRate), d = b.getChannelData(0);
  for(var i = 0; i < n; i++) d[i] = (Math.random()*2 - 1)*(1 - i/n);
  var src = ac.createBufferSource(), f = ac.createBiquadFilter(), g = ac.createGain();
  src.buffer = b; f.type = "bandpass"; f.frequency.value = freq || 1200; g.gain.value = vol;
  src.connect(f); f.connect(g); g.connect(SND.master); src.start(t);
}
var SFX = {
  ui: function(a){ tone(a, "sine", 660, 0, 0.06, 0.08); },
  count: function(a){ tone(a, "square", 440, 0, 0.16, 0.12); },
  go: function(a){ tone(a, "square", 880, 0, 0.35, 0.14); },
  jump: function(a){ tone(a, "sine", 320, 760, 0.14, 0.16); },
  djump: function(a){ tone(a, "sine", 520, 1100, 0.14, 0.14); },
  coin: function(a){ tone(a, "square", 988, 0, 0.07, 0.08); tone(a, "square", 1319, 0, 0.12, 0.08, 0.07); },
  shot: function(a, o){ noise(a, 0.09, 0.35, (o && o.w) ? 1800 : 900); tone(a, "sawtooth", (o && o.w) ? 240 : 170, 60, 0.1, 0.1); },
  hit: function(a){ tone(a, "triangle", 1400, 900, 0.06, 0.1); },
  putt: function(a, o){ var p = clamp((o && o.power) || 40, 5, 100)/100; noise(a, 0.05, 0.25 + p*0.3, 2400); tone(a, "triangle", 900, 300, 0.06, 0.1); },
  sink: function(a){ tone(a, "sine", 784, 0, 0.18, 0.12); tone(a, "sine", 1175, 0, 0.3, 0.12, 0.12); },
  finish: function(a){ [523, 659, 784, 1047].forEach(function(f, i){ tone(a, "square", f, 0, 0.16, 0.1, i*0.1); }); },
  levelup: function(a){ [392, 523, 659, 784, 1047].forEach(function(f, i){ tone(a, "triangle", f, 0, 0.22, 0.12, i*0.08); }); },
  door: function(a){ noise(a, 0.35, 0.18, 600); },
  bell: function(a){ tone(a, "sine", 2093, 2050, 0.35, 0.1); tone(a, "sine", 2637, 2600, 0.3, 0.06); tone(a, "sine", 2093, 2050, 0.35, 0.1, 0.14); }   // a bike bell
};
function sfx(name, opt){
  if(sfxVolume() <= 0 || !SFX[name]) return;
  var a = audio(); if(!a || a.state !== "running") return;
  try { SFX[name](a, opt); } catch(e){}
}
// A continuous engine note: engineSound(kind) -> {set(rpm 900..7600), stop()}. Each vehicle
// has its own voice: oscillator shapes, pitch range, filter, and a lope, rattle or turbo whistle.
var ENGINES = {
  v8:     {lo: 26, hi: 120, a: "sawtooth", b: "square",   ratio: 0.5, cut: 700,  q: 4, gain: 0.06,  lope: [7, 0.35]},
  diesel: {lo: 22, hi: 90,  a: "square",   b: "sawtooth", ratio: 0.5, cut: 520,  q: 2, gain: 0.055, rattle: 0.035},
  turbo:  {lo: 30, hi: 140, a: "sawtooth", b: "triangle", ratio: 2.0, cut: 1100, q: 6, gain: 0.05,  whistle: 1800},
  hotrod: {lo: 24, hi: 130, a: "sawtooth", b: "sawtooth", ratio: 1.01, cut: 850, q: 8, gain: 0.06,  lope: [4.5, 0.5]},
  bike:   {lo: 55, hi: 260, a: "sawtooth", b: "square",   ratio: 2.0, cut: 2400, q: 3, gain: 0.04,  lope: [22, 0.15]},
  // Valley Rally: a turbo four with its whistle, a straight six, a high-revving four
  turbo4: {lo: 34, hi: 158, a: "sawtooth", b: "triangle", ratio: 2.0, cut: 1250, q: 5, gain: 0.05,  whistle: 2300, lope: [11, 0.08]},
  six:    {lo: 30, hi: 140, a: "sawtooth", b: "sawtooth", ratio: 1.5, cut: 1050, q: 6, gain: 0.055},
  four:   {lo: 38, hi: 176, a: "square",   b: "sawtooth", ratio: 2.0, cut: 1600, q: 4, gain: 0.045, lope: [16, 0.1]}
};
function engineSound(kind){
  var P = ENGINES[kind] || ENGINES.v8;
  var a = sfxVolume() > 0 ? audio() : null; if(!a) return {set: function(){}, stop: function(){}};
  var o = a.createOscillator(), o2 = a.createOscillator(), f = a.createBiquadFilter(), g = a.createGain(), amp = a.createGain();
  o.type = P.a; o2.type = P.b; f.type = "lowpass"; f.frequency.value = P.cut; f.Q.value = P.q; g.gain.value = 0; amp.gain.value = 1;
  o.connect(f); o2.connect(f); f.connect(amp); amp.connect(g); g.connect(SND.master); o.start(); o2.start();
  var nodes = [o, o2];
  if(P.lope){                       // an uneven idle: the gain wobbles (a cam's lope, a bike's buzz)
    var l = a.createOscillator(), ld = a.createGain(); l.frequency.value = P.lope[0]; ld.gain.value = P.lope[1];
    l.connect(ld); ld.connect(amp.gain); l.start(); nodes.push(l);
  }
  var wh = null, whg = null;
  if(P.whistle){                    // a turbo spooling up with the revs
    wh = a.createOscillator(); whg = a.createGain(); wh.type = "sine"; whg.gain.value = 0;
    wh.connect(whg); whg.connect(SND.master); wh.start(); nodes.push(wh);
  }
  var rt = null;
  if(P.rattle){                     // diesel clatter: filtered noise riding on the note
    var n = Math.floor(a.sampleRate*0.5), b = a.createBuffer(1, n, a.sampleRate), d = b.getChannelData(0);
    for(var i = 0; i < n; i++) d[i] = Math.random()*2 - 1;
    var src = a.createBufferSource(), nf = a.createBiquadFilter(); rt = a.createGain();
    src.buffer = b; src.loop = true; nf.type = "bandpass"; nf.frequency.value = 900; rt.gain.value = 0;
    src.connect(nf); nf.connect(rt); rt.connect(SND.master); src.start(); nodes.push(src);
  }
  return {
    set: function(rpm){
      var u = clamp((rpm - 900)/6700, 0, 1), hz = P.lo + (P.hi - P.lo)*u, t = a.currentTime, on = sfxVolume() > 0;
      o.frequency.setTargetAtTime(hz, t, 0.05); o2.frequency.setTargetAtTime(hz*P.ratio, t, 0.05);
      f.frequency.setTargetAtTime(P.cut*(0.7 + u*0.9), t, 0.08);
      g.gain.setTargetAtTime(on ? P.gain*(0.7 + u*0.5) : 0, t, 0.1);
      if(wh){ wh.frequency.setTargetAtTime(P.whistle*(0.4 + u), t, 0.15); whg.gain.setTargetAtTime(on ? 0.012*u*u : 0, t, 0.15); }
      if(rt) rt.gain.setTargetAtTime(on ? P.rattle*(1 - u*0.5) : 0, t, 0.1);
    },
    stop: function(){
      try { g.gain.setTargetAtTime(0, a.currentTime, 0.05); if(whg) whg.gain.setTargetAtTime(0, a.currentTime, 0.05); if(rt) rt.gain.setTargetAtTime(0, a.currentTime, 0.05);
            nodes.forEach(function(nd){ nd.stop(a.currentTime + 0.3); }); } catch(e){}
    }
  };
}
// A looping band of noise (tyre scrub, gravel under the car): noiseLoop(freq, q) ->
// {set(gain, freq), stop()}. Silent until set() is called with a gain.
function noiseLoop(freq, q){
  var a = sfxVolume() > 0 ? audio() : null; if(!a) return {set: function(){}, stop: function(){}};
  var n = Math.floor(a.sampleRate*0.6), b = a.createBuffer(1, n, a.sampleRate), d = b.getChannelData(0);
  for(var i = 0; i < n; i++) d[i] = Math.random()*2 - 1;
  var src = a.createBufferSource(), f = a.createBiquadFilter(), g = a.createGain();
  src.buffer = b; src.loop = true; f.type = "bandpass"; f.frequency.value = freq || 1000; f.Q.value = q || 1; g.gain.value = 0;
  src.connect(f); f.connect(g); g.connect(SND.master); src.start();
  return {
    set: function(gain, fq){
      var t = a.currentTime, on = sfxVolume() > 0;
      g.gain.setTargetAtTime(on ? clamp(+gain || 0, 0, 0.3) : 0, t, 0.06);
      if(fq) f.frequency.setTargetAtTime(clamp(fq, 60, 8000), t, 0.1);
    },
    stop: function(){ try { g.gain.setTargetAtTime(0, a.currentTime, 0.05); src.stop(a.currentTime + 0.3); } catch(e){} }
  };
}
// Browsers allow audio only after a user gesture: wake it on the first one.
if(typeof window.addEventListener === "function"){
  window.addEventListener("pointerdown", function(){ if(sfxVolume() > 0) audio(); }, {once: true, capture: true});
  window.addEventListener("keydown", function(){ if(sfxVolume() > 0) audio(); }, {once: true, capture: true});
}

/* ---------- spectating (HQ 2.1): "Watching X  ◀ ▶" with [ and ] ---------- */
// watchBar(stage, opts) -> {update(), destroy()}; opts.list() -> [{id, name}], opts.get() -> id,
// opts.set(id), opts.stop() (optional: a "Stop watching" button). Shows only while opts.get() is set.
function watchBar(stage, opts){
  var bar = document.createElement("div"); bar.className = "vg-watch"; bar.setAttribute("role", "group"); bar.setAttribute("aria-label", "Spectating");
  var prev = document.createElement("button"), label = document.createElement("span"), next = document.createElement("button");
  prev.type = next.type = "button"; prev.className = next.className = "hbtn ghost"; prev.textContent = "◀"; next.textContent = "▶";
  prev.setAttribute("aria-label", "Watch the previous player"); next.setAttribute("aria-label", "Watch the next player");
  label.setAttribute("aria-live", "polite");
  bar.appendChild(prev); bar.appendChild(label); bar.appendChild(next);
  var stop = null;
  if(opts.stop){ stop = document.createElement("button"); stop.type = "button"; stop.className = "hbtn ghost"; stop.textContent = "Stop watching"; stop.addEventListener("click", opts.stop); bar.appendChild(stop); }
  stage.appendChild(bar);
  function step(d){ var L = opts.list(); if(!L.length) return; var i = 0; L.forEach(function(p, k){ if(p.id === opts.get()) i = k; });
    var n = L[(i + d + L.length) % L.length]; opts.set(n.id); upd(); say("Watching " + n.name); }
  prev.addEventListener("click", function(){ step(-1); }); next.addEventListener("click", function(){ step(1); });
  function key(e){ if(!opts.get() || bar.hidden) return; if(e.key === "[" ){ e.preventDefault(); step(-1); } else if(e.key === "]"){ e.preventDefault(); step(1); } }
  window.addEventListener("keydown", key);
  function upd(){ var id = opts.get(); bar.hidden = !id; if(!id) return;
    var p = opts.list().filter(function(x){ return x.id === id; })[0]; label.textContent = "Watching " + (p ? p.name : "…") + "  ·  [ ] to switch"; }
  return {update: upd, destroy: function(){ window.removeEventListener("keydown", key); if(bar.parentNode) bar.parentNode.removeChild(bar); }};
}

HQV.engine = {now: now, clamp: clamp, hex: hex, angLerp: angLerp, calm: calm, say: say, tokens: tokens, myClock: myClock,
              fmtTime: fmtTime, dist3: dist3, lib: lib, loadGlb: loadGlb, hasWebGL2: hasWebGL2,
              senderTime: senderTime, jitter: jitter, pushSnap: pushSnap, bracket: bracket,
              sfx: sfx, sfxVolume: sfxVolume, sfxSetVolume: sfxSetVolume, engineSound: engineSound, engines: Object.keys(ENGINES), watchBar: watchBar,
              noiseLoop: noiseLoop};
})();
