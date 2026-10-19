/* Tower Defense (HQ 2.5): your Pokémon hold the line against twenty waves of bugs.
 *
 * Solo in the Valley (card "td") runs the rules right here; co-op in an Arena room
 * (card "mp-td", up to 4) is refereed by the Rust Arena (backend/src/valley/td.rs),
 * which sends a full view ('td'), a 5 Hz snapshot ('tsnap'), 'wave' and 'done'.
 *
 * The rules are integer-only and live in the TD-RULES block below, mirrored constant
 * for constant from td.rs (tests/test_td_sync.py compares the two). Types matter: a
 * tower hits with its first type, and the multiplier is the real type chart, per bug
 * type, on the x4 integer scale MULT4. Fire burns (and hits armour x1.5), Water
 * knocks back every 4th hit, Electric chains, Ice slows, Grass roots, Rock/Ground
 * splash (Ground cannot hit flyers), Flying/Psychic/Ghost pierce, Poison stacks.
 *
 * Towers are DOM overlays built with HQV.pk.spriteEl (its onerror chain ends at
 * window.paintCreature), so offline or with a non-Pokémon pack they still show.
 * Particles and hit lines are skipped under calm; every wave, leak and win is also
 * announced. Keyboard: Tab through slots, Enter places the picked Pokémon, 1-3 set a
 * tower's target mode, S sells.
 */
(function(){
"use strict";
var HQV = window.HQV; if(!HQV || !HQV.api || !HQV.engine) return;
var api = HQV.api, E = HQV.engine, MP = HQV.mp || null;

/* TD-RULES BEGIN */
var WAVES = 20;
var LIVES = 20;
var MAX_PLAYERS = 4;
var MAX_TOWERS = 6;
var START_BERRIES = 150;
var INCOME = [20, 3];
var SELL_PCT = 70;
var READY_SECS = 20;
var SNAP_EVERY = 2;
var COST = [50, 70, 90, 120, 150];
var BP = [40, 50, 60, 70, 80];
var MULT4 = [0, 1, 2, 4, 8, 16];
var SPE_BANDS = [60, 90, 120, 150];
var CD = [12, 10, 8, 7, 6];
var RANGE = [300, 250, 200];
var DIFF_HP4 = [3, 4, 6];
var KIND_HP = [60, 50, 110, 45, 70, 1200];
var KIND_SPD = [8, 11, 5, 10, 13, 4];
var KIND_BOUNTY = [3, 4, 6, 4, 5, 40];
var KIND_LEAK = [1, 1, 2, 1, 1, 5];
var KIND_ARMOUR = [0, 0, 1, 0, 0, 1];
var UNLOCK = [1, 3, 5, 7, 9];
var BOSS_WAVES = [10, 20];
var FX = [30, 5, 8, 80, 4, 2, 150, 20, 60, 10, 80, 2, 40, 5, 12];
var KINDS = ["grub", "stinger", "beetle", "moth", "glitch", "boss"];
var KIND_T1 = ["Bug", "Bug", "Bug", "Bug", "Bug", "Bug"];
var KIND_T2 = ["", "Poison", "Steel", "Flying", "Electric", "Rock"];
var DIFFS = ["easy", "normal", "hard"];
var MODES = ["first", "strong", "close"];
var MAP_IDS = ["garden", "circuit", "datacenter"];
var GARDEN_PATH = [[0, 2], [4, 2], [4, 7], [10, 7], [10, 2], [15, 2]];
var GARDEN_SLOTS = [[2, 1], [2, 3], [5, 4], [3, 5], [6, 6], [7, 8], [9, 5], [11, 4], [8, 3], [12, 1], [13, 3], [6, 8]];
var CIRCUIT_PATH = [[0, 8], [3, 8], [3, 1], [8, 1], [8, 8], [12, 8], [12, 4], [15, 4]];
var CIRCUIT_SLOTS = [[1, 7], [2, 4], [4, 3], [4, 6], [6, 2], [5, 5], [7, 4], [9, 3], [9, 6], [10, 7], [11, 5], [13, 5], [14, 3]];
var DATACENTER_PATH = [[0, 1], [13, 1], [13, 4], [2, 4], [2, 8], [15, 8]];
var DATACENTER_SLOTS = [[3, 2], [6, 2], [9, 2], [12, 2], [14, 3], [4, 5], [7, 5], [10, 5], [1, 6], [3, 7], [6, 7], [9, 7], [12, 7], [14, 6]];
var LONG_TYPES = ["Flying", "Psychic", "Electric", "Ice", "Water", "Dragon", "Fairy", "Ghost"];
var MID_TYPES = ["Fire", "Grass", "Poison", "Dark", "Normal"];
/* TD-RULES END */

// Names for the FX table, in its order (td.rs uses the same indexes).
var BURN_TICKS = 0, DOT_EVERY = 1, BURN_DIV = 2, KNOCK = 3, KNOCK_EVERY = 4, CHAIN = 5, CHAIN_R = 6,
    SLOW_TICKS = 7, SLOW_PCT = 8, ROOT_TICKS = 9, SPLASH_R = 10, PIERCE = 11, PSN_TICKS = 12, PSN_MAX = 13, PSN_DIV = 14;
var BOSS = 5, HZ = 10, COLS = 16, ROWS = 10, TILE = 40;
var MAPS = [{id:"garden", name:"Garden", path:GARDEN_PATH, slots:GARDEN_SLOTS},
            {id:"circuit", name:"Circuit", path:CIRCUIT_PATH, slots:CIRCUIT_SLOTS},
            {id:"datacenter", name:"Datacenter", path:DATACENTER_PATH, slots:DATACENTER_SLOTS}];
var KIND_NAME = ["Grub", "Stinger", "Beetle", "Moth", "Glitch", "Bug King"];
var MODE_NAME = ["First", "Strongest", "Closest"];

function pk(){ return HQV.pk && HQV.pk.data && HQV.pk.data() ? HQV.pk : null; }
function idiv(a, b){ return Math.floor(a / b); }

/* ------------------------------ geometry ------------------------------ */
function centre(c){ return [c[0]*100 + 50, c[1]*100 + 50]; }
function pathLen(path){ var n = 0; for(var i = 1; i < path.length; i++) n += (Math.abs(path[i][0]-path[i-1][0]) + Math.abs(path[i][1]-path[i-1][1]))*100; return n; }
function sgn(x){ return x > 0 ? 1 : x < 0 ? -1 : 0; }
function posAt(path, d){
  var left = Math.max(0, d);
  for(var i = 1; i < path.length; i++){
    var a = centre(path[i-1]), b = centre(path[i]), len = Math.abs(b[0]-a[0]) + Math.abs(b[1]-a[1]);
    if(left <= len) return [a[0] + sgn(b[0]-a[0])*left, a[1] + sgn(b[1]-a[1])*left];
    left -= len;
  }
  return centre(path[path.length-1]);
}

/* ------------------------------- waves -------------------------------- */
// r() is a [0,1) generator (api.rng); pick(n) = floor(r() * n), as td.rs's Mulberry::pick.
function genWaves(r){
  var out = [];
  for(var w = 1; w <= WAVES; w++){
    var open = []; for(var k = 0; k < UNLOCK.length; k++) if(UNLOCK[k] <= w) open.push(k);
    var gap = Math.max(4, 12 - idiv(w, 3)), n = 6 + 2*w, v = [];
    for(var i = 0; i < n; i++) v.push([i*gap, open[Math.floor(r()*open.length)]]);
    if(BOSS_WAVES.indexOf(w) >= 0) v.push([n*gap + 10, BOSS]);
    out.push(v);
  }
  return out;
}

/* ------------------------------- types -------------------------------- */
function m4(atk, dfn){
  if(!dfn) return MULT4[3];
  var P = pk(), e = P ? P.typeMult(atk, [dfn]) : 1, q = Math.floor(e*4);
  return MULT4[q === 0 ? 0 : q === 1 ? 1 : q === 2 ? 2 : q === 4 ? 3 : q === 8 ? 4 : 5];
}
function mult16(atk, kind){ return m4(atk, KIND_T1[kind]) * m4(atk, KIND_T2[kind]); }
function hitDmg(base, t, kind){
  var m = mult16(t, kind); if(!m) return 0;
  var d = idiv(base*m, 16);
  if(t === "Fire" && KIND_ARMOUR[kind] === 1) d = idiv(d*3, 2);
  return Math.max(1, d);
}
function cooldown(spe){ var band = 0; SPE_BANDS.forEach(function(b){ if(spe >= b) band++; }); return CD[band]; }
function rangeOf(t){ return LONG_TYPES.indexOf(t) >= 0 ? RANGE[0] : MID_TYPES.indexOf(t) >= 0 ? RANGE[1] : RANGE[2]; }
function multText(x){ return "×" + String(x); }
// "×4 (Bug ×2 · Steel ×2)" for a tower type against a bug kind.
function matchup(t, kind){
  var a = m4(t, KIND_T1[kind]), b = m4(t, KIND_T2[kind]), s = multText(a*b/16) + " (" + KIND_T1[kind] + " " + multText(a/4);
  if(KIND_T2[kind]) s += " · " + KIND_T2[kind] + " " + multText(b/4);
  s += ")";
  if(t === "Fire" && KIND_ARMOUR[kind]) s += ", burns armour ×1.5";
  if(a*b === 0) s += ": cannot hit it";
  return s;
}
var EFFECT = {Fire:"burns; ×1.5 vs armour", Water:"knocks back every 4th hit", Electric:"chains to 2 more", Ice:"slows 40%",
  Grass:"roots for 1 s", Rock:"splash damage", Ground:"splash; cannot hit flyers", Flying:"pierces", Psychic:"pierces",
  Ghost:"pierces", Poison:"stacking poison"};

// One tower, from a creature spec. Same numbers as td.rs's Tower::new.
function makeTower(slot, owner, raw){
  var P = pk(); if(!P) return null;
  var spec = P.cleanSpec(raw); if(!spec) return null;
  var mon = P.buildMon(spec), t = mon.types[0] || "Normal", atk = Math.max(mon.stats.atk, mon.stats.spa);
  return {slot:slot, owner:owner, mode:MODES[0], cost:COST[spec.st], base: idiv(atk*BP[spec.st], 50) + 2,
    cd:cooldown(mon.stats.spe), range:rangeOf(t), ttype:t, cdLeft:0, hits:0,
    spec:{sp:spec.sp, st:spec.st, br: spec.br === mon.dex ? spec.br : null, mg:mon.mega, sh:spec.sh},
    mon:{name:mon.name, id:mon.id, dex:mon.dex, sprite:mon.sprite, mega:mon.mega, shiny:mon.shiny, sp:mon.sp, st:mon.st, types:mon.types}};
}

/* ------------------------- the solo simulation ------------------------- */
// A line-for-line port of td.rs's Td for one local player ("me").
function Sim(map, diff, r){
  this.phase = "build"; this.map = map; this.diff = diff; this.wave = 0; this.cleared = 0; this.lives = LIVES;
  this.players = [{user:{userId:"me"}, berries:START_BERRIES, ready:false, away:false}];
  this.towers = []; this.bugs = []; this.waves = genWaves(r); this.spawnI = 0; this.tick = 0; this.nextId = 0; this.hits = [];
}
Sim.prototype.def = function(){ return MAPS[this.map]; };
Sim.prototype.cap = function(){ return Math.min(MAX_TOWERS, idiv(this.def().slots.length, Math.max(1, this.players.length))); };
Sim.prototype.view = function(){
  var self = this;
  return {map:MAP_IDS[this.map], diff:DIFFS[this.diff], wave:this.wave, cleared:this.cleared, waves:WAVES, lives:this.lives,
    players:this.players.map(function(p){ return {user:p.user, berries:p.berries, ready:p.ready, away:p.away,
      towers:self.towers.filter(function(t){ return t.owner === p.user.userId; }).length}; }),
    towers:this.towers, phase:this.phase, cap:this.cap(), readyInMs:null};
};
Sim.prototype.place = function(uid, slot, raw){
  if(this.phase !== "build" && this.phase !== "wave") return "no game is running";
  var p = this.players[0];
  if(typeof slot !== "number" || slot < 0 || slot >= this.def().slots.length || Math.floor(slot) !== slot) return "no such slot";
  if(this.towers.some(function(t){ return t.slot === slot; })) return "that slot is taken";
  var tw = makeTower(slot, uid, raw); if(!tw) return "that creature is not valid";
  if(this.towers.filter(function(t){ return t.owner === uid; }).length >= this.cap()) return "you have placed all your towers";
  if(p.berries < tw.cost) return "not enough berries";
  p.berries -= tw.cost; this.towers.push(tw); this.towers.sort(function(a, b){ return a.slot - b.slot; });
  return null;
};
Sim.prototype.sell = function(uid, slot){
  var i = -1; this.towers.forEach(function(t, j){ if(t.slot === slot) i = j; });
  if(i < 0) return "no such slot";
  this.players[0].berries += idiv(this.towers[i].cost*SELL_PCT, 100); this.towers.splice(i, 1); return null;
};
Sim.prototype.target = function(uid, slot, mode){
  var m = MODES.indexOf(mode); if(m < 0) return "pick first, strong or close";
  var t = this.towers.filter(function(x){ return x.slot === slot; })[0]; if(!t) return "no such slot";
  t.mode = MODES[m]; return null;
};
Sim.prototype.beginWave = function(){
  this.wave = this.cleared + 1; this.phase = "wave"; this.spawnI = 0; this.tick = 0;
  this.players.forEach(function(p){ p.ready = false; });
};
Sim.prototype.bugHp = function(kind){
  var w = this.wave - 1, np = Math.max(1, this.players.length);
  return Math.max(1, idiv(KIND_HP[kind]*(100 + 20*w + 2*w*w)*DIFF_HP4[this.diff]*(3 + np), 1600));
};
Sim.prototype.finish = function(win){
  this.phase = "done"; this.bugs = [];
  return {key:MAP_IDS[this.map]+"-"+DIFFS[this.diff], map:MAP_IDS[this.map], diff:DIFFS[this.diff], mode:"coop", waves:this.cleared, win:win};
};
Sim.prototype.pickTarget = function(tw, path){
  var tp = centre(this.def().slots[tw.slot]), best = -1, bs = 0, mi = MODES.indexOf(tw.mode);
  for(var i = 0; i < this.bugs.length; i++){
    var b = this.bugs[i]; if(b.hp <= 0 || mult16(tw.ttype, b.kind) === 0) continue;
    var p = posAt(path, b.d), d2 = (p[0]-tp[0])*(p[0]-tp[0]) + (p[1]-tp[1])*(p[1]-tp[1]);
    if(d2 > tw.range*tw.range) continue;
    var score = mi === 0 ? b.d : mi === 1 ? b.hp : -d2;
    if(best < 0 || score > bs){ best = i; bs = score; }
  }
  return best;
};
Sim.prototype.strike = function(slot, i, base, t, f0, f1){
  var b = this.bugs[i], d = idiv(hitDmg(base, t, b.kind)*f0, f1);
  if(d <= 0) return;
  b.hp -= Math.max(1, d);
  if(t === "Fire"){ b.burn = FX[BURN_TICKS]; b.burnDmg = Math.max(b.burnDmg, Math.max(1, idiv(d, FX[BURN_DIV]))); }
  else if(t === "Ice") b.slow = FX[SLOW_TICKS];
  else if(t === "Grass"){ if(b.kind !== BOSS) b.root = FX[ROOT_TICKS]; }
  else if(t === "Poison"){ b.psn = Math.min(FX[PSN_MAX], b.psn + 1); b.psnT = FX[PSN_TICKS]; b.psnDmg = Math.max(b.psnDmg, Math.max(1, idiv(d, FX[PSN_DIV]))); }
  if(this.hits.length < 60) this.hits.push([slot, b.id, mult16(t, b.kind)]);
};
Sim.prototype.fire = function(ti, path){
  var tw = this.towers[ti], i = this.pickTarget(tw, path); if(i < 0) return;
  tw.cdLeft = tw.cd; tw.hits += 1;
  var t = tw.ttype, self = this;
  this.strike(tw.slot, i, tw.base, t, 1, 1);
  var at = posAt(path, this.bugs[i].d);
  function near(r){
    var v = [];
    self.bugs.forEach(function(b, j){
      if(j === i || b.hp <= 0) return;
      var p = posAt(path, b.d), d2 = (p[0]-at[0])*(p[0]-at[0]) + (p[1]-at[1])*(p[1]-at[1]);
      if(d2 <= r*r) v.push([d2*1000 + Math.min(b.id, 999), j]);
    });
    v.sort(function(a, b){ return a[0] - b[0] || a[1] - b[1]; });
    return v.map(function(x){ return x[1]; });
  }
  if(t === "Water"){ if(tw.hits % FX[KNOCK_EVERY] === 0 && this.bugs[i].kind !== BOSS) this.bugs[i].d = Math.max(0, this.bugs[i].d - FX[KNOCK]); }
  else if(t === "Electric") near(FX[CHAIN_R]).slice(0, FX[CHAIN]).forEach(function(j){ self.strike(tw.slot, j, tw.base, t, 1, 2); });
  else if(t === "Rock" || t === "Ground") near(FX[SPLASH_R]).forEach(function(j){ self.strike(tw.slot, j, tw.base, t, 1, 2); });
  else if(t === "Flying" || t === "Psychic" || t === "Ghost"){
    var tp = centre(this.def().slots[tw.slot]), more = [];
    this.bugs.forEach(function(b, j){
      if(j === i || b.hp <= 0 || mult16(t, b.kind) === 0) return;
      var p = posAt(path, b.d);
      if((p[0]-tp[0])*(p[0]-tp[0]) + (p[1]-tp[1])*(p[1]-tp[1]) <= tw.range*tw.range) more.push([-b.d, j]);
    });
    more.sort(function(a, b){ return a[0] - b[0] || a[1] - b[1]; });
    more.slice(0, FX[PIERCE]).forEach(function(x){ self.strike(tw.slot, x[1], tw.base, t, 1, 1); });
  }
};
// One 10 Hz step. Returns events: {wave:n} | {view:1} | {done:payload}.
Sim.prototype.step = function(){
  var evs = []; if(this.phase !== "wave") return evs;
  var path = this.def().path, end = pathLen(path), wi = this.wave - 1, W = this.waves[wi], self = this;
  while(this.spawnI < W.length && W[this.spawnI][0] <= this.tick){
    var kind = W[this.spawnI][1], hp = this.bugHp(kind);
    this.bugs.push({id:++this.nextId, kind:kind, d:0, hp:hp, max:hp, slow:0, root:0, burn:0, burnDmg:0, psn:0, psnT:0, psnDmg:0});
    this.spawnI++;
  }
  var dot = this.tick % FX[DOT_EVERY] === 0;
  this.bugs.forEach(function(b){
    if(b.root > 0) b.root--; else b.d += idiv(KIND_SPD[b.kind]*(b.slow > 0 ? FX[SLOW_PCT] : 100), 100);
    b.slow = Math.max(0, b.slow - 1);
    if(b.burn > 0){ b.burn--; if(dot) b.hp -= b.burnDmg; }
    if(b.psnT > 0){ b.psnT--; if(dot) b.hp -= b.psnDmg*b.psn; if(b.psnT === 0) b.psn = 0; }
  });
  var leaked = 0;
  this.bugs = this.bugs.filter(function(b){ if(b.d >= end && b.hp > 0){ leaked += KIND_LEAK[b.kind]; return false; } return true; });
  this.lives = Math.max(0, this.lives - leaked);
  for(var ti = 0; ti < this.towers.length; ti++){
    if(this.towers[ti].cdLeft > 0) this.towers[ti].cdLeft--; else this.fire(ti, path);
  }
  var bounty = 0;
  this.bugs = this.bugs.filter(function(b){ if(b.hp <= 0){ bounty += KIND_BOUNTY[b.kind]; return false; } return true; });
  this.players.forEach(function(p){ if(!p.away) p.berries += bounty; });
  this.tick++;
  if(this.lives <= 0) evs.push({done:this.finish(false)});
  else if(this.spawnI >= W.length && !this.bugs.length){
    this.cleared = this.wave;
    if(this.cleared >= WAVES) evs.push({done:this.finish(true)});
    else { this.phase = "build"; var inc = INCOME[0] + INCOME[1]*this.wave; self.players.forEach(function(p){ if(!p.away) p.berries += inc; }); evs.push({view:1}); }
  }
  return evs;
};
Sim.prototype.snapBugs = function(){
  var path = this.def().path;
  return this.bugs.map(function(b){ var p = posAt(path, b.d);
    return [b.id, b.kind, p[0], p[1], idiv(Math.max(0, b.hp)*100, Math.max(1, b.max)), (b.slow > 0 ? 1 : 0) | (b.root > 0 ? 2 : 0) | (b.burn > 0 ? 4 : 0) | (b.psn > 0 ? 8 : 0)]; });
};

/* ------------------------------ the roster ----------------------------- */
// Your Pokémon to build with: the saved battle team first, then everything unlocked, then
// loot cards from the Arena (card:p<dex3> / card:h<dex3>, holo = shiny). Offline or before
// the Pokédex answers, three starters so the game is always playable.
var LOOT = null;
function lineOfDex(dex){ var P = pk(); if(!P) return -1; var L = P.data().lines; for(var i = 0; i < L.length; i++) if(L[i][0] === dex) return i; return -1; }
function loadLoot(){
  if(LOOT) return Promise.resolve(LOOT);
  return fetch("/api/arena/loot", {cache:"no-store"}).then(function(r){ return r.ok ? r.json() : {}; }).then(function(j){
    var out = [], cards = (j && typeof j.cards === "object" && j.cards) || {};
    Object.keys(cards).sort().forEach(function(k){
      var m = /^([ph])(\d{3})$/.exec(k); if(!m || !(cards[k] > 0)) return;
      var sp = lineOfDex(parseInt(m[2], 10)); if(sp < 0) return;
      out.push({sp:sp, st:0, br:0, mg:0, sh: m[1] === "h" ? 1 : 0, _card:true});
    });
    LOOT = out; return out;
  }, function(){ LOOT = []; return []; });
}
function roster(){
  var P = pk(), out = [], seen = {};
  function add(s, tag){
    if(!s || !P) return; var c = P.cleanSpec(s); if(!c) return;
    var k = c.sp+":"+c.st+":"+(c.sh ? 1 : 0); if(seen[k]) return; seen[k] = 1;
    out.push({spec:{sp:c.sp, st:c.st, br:c.br, mg:c.mg, sh:c.sh}, tag:tag});
  }
  if(!P) return out;
  (P.savedTeam && P.savedTeam() || []).forEach(function(s){ add(s, "team"); });
  (P.unlocked && P.unlocked() || []).forEach(function(s){ add(s, ""); });
  (LOOT || []).forEach(function(s){ add(s, "card"); });
  if(!out.length) [1, 4, 7].forEach(function(dex){ var sp = lineOfDex(dex); if(sp >= 0) add({sp:sp, st:0}, "starter"); });
  return out;
}

/* ------------------------------- the save ------------------------------ */
function tdSave(){
  var s = api.save; if(!s) return {best:{}, runs:0};
  if(!s.td || typeof s.td !== "object") s.td = {best:{}};
  if(!s.td.best || typeof s.td.best !== "object") s.td.best = {};
  return s.td;
}
function note(ev){ try { if(HQV.story && HQV.story.note) HQV.story.note(ev); } catch(e){} }

/* ------------------------------- the view ------------------------------ */
// One board UI for both drivers. drv: {solo, me(), view(), bugs(), host(), place, sell,
// target, ready, start(map, diff), end()}.
function Board(el, drv){
  var self = this; this.drv = drv; this.el = el; this.pick = null; this.sel = null; this.hover = null;
  this.fx = []; this.prev = {}; this.cur = {}; this.curAt = 0; this.sig = "";
  el.textContent = "";
  var wrap = api.mk("div", "td-wrap");
  this.top = api.mk("div", "vg-row td-top");
  this.status = api.mk("p", "vg-msg td-status"); this.status.setAttribute("role", "status");
  var main = api.mk("div", "td-main"), boardBox = api.mk("div", "td-board");
  this.cv = api.canvas(COLS*TILE, ROWS*TILE); this.cv.classList.add("td-canvas"); this.cv.tabIndex = -1;
  this.cv.setAttribute("aria-label", "Tower Defense board"); this.cv.setAttribute("role", "img");
  this.ov = api.mk("div", "td-ov");
  this.tip = api.mk("div", "td-tip"); this.tip.hidden = true; this.tip.setAttribute("role", "tooltip");
  boardBox.appendChild(this.cv); boardBox.appendChild(this.ov); boardBox.appendChild(this.tip);
  this.side = api.mk("div", "td-side");
  this.actions = api.mk("div", "vg-row td-actions");
  this.towerBox = api.mk("div", "td-panel td-towerbox");
  this.rosterBox = api.mk("div", "td-panel");
  this.guide = api.mk("div", "td-panel td-guide");
  this.playersBox = api.mk("div", "td-panel");
  this.side.appendChild(this.actions); this.side.appendChild(this.towerBox); this.side.appendChild(this.rosterBox);
  this.side.appendChild(this.guide); this.side.appendChild(this.playersBox);
  main.appendChild(boardBox); main.appendChild(this.side);
  wrap.appendChild(this.top); wrap.appendChild(this.status); wrap.appendChild(main);
  el.appendChild(wrap);
  this.cv.addEventListener("mousemove", function(e){ self.onMove(e); });
  this.cv.addEventListener("mouseleave", function(){ self.setHover(null); });
  this.buildGuide();
  var P = pk();
  if(HQV.pk && HQV.pk.loadUnlocked) HQV.pk.loadUnlocked().then(function(){ self.sig = ""; self.update(true); });
  loadLoot().then(function(){ self.sig = ""; self.update(true); });
  if(!P) this.status.textContent = "Loading the type chart…";
}
Board.prototype.map = function(){ var v = this.drv.view(); return MAPS[Math.max(0, MAP_IDS.indexOf(v && v.map))]; };
Board.prototype.myTowers = function(){ var me = this.drv.me(), v = this.drv.view(); return ((v && v.towers) || []).filter(function(t){ return t.owner === me; }); };
Board.prototype.myBerries = function(){ var me = this.drv.me(), v = this.drv.view(), b = 0; ((v && v.players) || []).forEach(function(p){ if(p.user && p.user.userId === me) b = p.berries; }); return b; };
Board.prototype.running = function(){ var v = this.drv.view(); return !!v && (v.phase === "build" || v.phase === "wave"); };
Board.prototype.towerAt = function(slot){ var v = this.drv.view(); return ((v && v.towers) || []).filter(function(t){ return t.slot === slot; })[0] || null; };

Board.prototype.buildGuide = function(){
  var self = this, g = this.guide; g.textContent = "";
  g.appendChild(api.mk("h4", "td-h", "Bugs (hover for your matchups)"));
  var list = api.mk("div", "td-guide-list");
  KINDS.forEach(function(k, i){
    var b = api.btn(KIND_NAME[i] + " · " + KIND_T1[i] + (KIND_T2[i] ? "/" + KIND_T2[i] : "") + (KIND_ARMOUR[i] ? " · armour" : "") + (i === 3 ? " · flies" : ""), "ghost td-kind", function(){ self.setHover(self.hover === i ? null : i); });
    b.setAttribute("data-kind", k);
    b.addEventListener("mouseenter", function(){ self.setHover(i); });
    b.addEventListener("mouseleave", function(){ self.setHover(null); });
    b.addEventListener("focus", function(){ self.setHover(i); });
    b.addEventListener("blur", function(){ self.setHover(null); });
    list.appendChild(b);
  });
  g.appendChild(list);
};
// Hovering a bug (on the board or in the guide) shows every tower's multiplier on it.
Board.prototype.setHover = function(kind){
  this.hover = kind;
  var tip = this.tip, v = this.drv.view(), self = this;
  Array.prototype.forEach.call(this.ov.querySelectorAll(".td-mult"), function(n){ n.remove(); });
  if(kind == null || !v){ tip.hidden = true; return; }
  tip.textContent = "";
  tip.appendChild(api.mk("b", null, KIND_NAME[kind] + " (" + KIND_T1[kind] + (KIND_T2[kind] ? "/" + KIND_T2[kind] : "") + ")"));
  var towers = v.towers || [];
  if(!towers.length) tip.appendChild(api.mk("div", null, "Place a Pokémon to see its multiplier."));
  towers.forEach(function(t){
    var line = api.mk("div", "td-tip-line", t.mon.name + " (" + t.mon.types[0] + "): " + matchup(t.mon.types[0], kind));
    line.setAttribute("data-slot", String(t.slot)); tip.appendChild(line);
    var cell = self.ov.querySelector('[data-slot="' + t.slot + '"]');
    if(cell){ var m = mult16(t.mon.types[0], kind), badge = api.mk("span", "td-mult" + (m > 16 ? " good" : m < 16 ? " bad" : ""), multText(m/16)); cell.appendChild(badge); }
  });
  if(this.pick){
    var P = pk(), mon = P ? P.buildMon(P.cleanSpec(this.pick)) : null;
    if(mon) tip.appendChild(api.mk("div", "td-tip-line", "Picked " + mon.name + ": " + matchup(mon.types[0], kind)));
  }
  tip.hidden = false;
};
Board.prototype.onMove = function(e){
  var r = this.cv.getBoundingClientRect(), x = (e.clientX - r.left)/r.width*COLS*100, y = (e.clientY - r.top)/r.height*ROWS*100;
  var best = null, bd = 60*60;
  (this.drv.bugs() || []).forEach(function(b){ var d2 = (b[2]-x)*(b[2]-x) + (b[3]-y)*(b[3]-y); if(d2 < bd){ bd = d2; best = b; } });
  if(best){ if(this.hover !== best[1]) this.setHover(best[1]); }
  else if(this.hover != null && !(document.activeElement && document.activeElement.classList.contains("td-kind"))) this.setHover(null);
};

// The panels redraw only when what they show changes (tsnap arrives 5 times a second).
Board.prototype.update = function(force){
  var v = this.drv.view(), P = pk();
  var sig = JSON.stringify([v && v.phase, v && v.wave, v && v.map, v && v.diff, v && v.lives, v && v.towers && v.towers.map(function(t){ return [t.slot, t.owner, t.mode, t.spec.sp, t.spec.st]; }),
    v && v.players && v.players.map(function(p){ return [p.user.userId, p.berries, p.ready, p.away]; }), this.pick, this.sel, !!P, this.drv.host(), v && v.readyInMs != null]);
  if(sig === this.sig && !force) return;
  this.sig = sig;
  this.renderTop(v); this.renderSlots(v); this.renderActions(v); this.renderTower(v); this.renderRoster(v); this.renderPlayers(v);
  if(this.hover != null) this.setHover(this.hover);
};
Board.prototype.renderTop = function(v){
  var self = this, top = this.top; top.textContent = "";
  if(!this.running() && this.drv.host()){
    var ms = api.mk("select", "vg-select td-mapsel"); ms.setAttribute("aria-label", "Map");
    MAPS.forEach(function(m){ var o = api.mk("option", null, m.name); o.value = m.id; ms.appendChild(o); });
    var ds = api.mk("select", "vg-select td-diffsel"); ds.setAttribute("aria-label", "Difficulty");
    DIFFS.forEach(function(d){ var o = api.mk("option", null, d[0].toUpperCase() + d.slice(1)); o.value = d; ds.appendChild(o); });
    ms.value = this.lastMap || "garden"; ds.value = this.lastDiff || "normal";
    ms.addEventListener("change", function(){ self.lastMap = ms.value; });
    ds.addEventListener("change", function(){ self.lastDiff = ds.value; });
    top.appendChild(ms); top.appendChild(ds);
    top.appendChild(api.btn("Start defending", "primary td-start", function(){ self.lastMap = ms.value; self.lastDiff = ds.value; self.drv.start(ms.value, ds.value); }));
  } else if(!this.running()){
    top.appendChild(api.mk("span", "vg-muted", "The host picks a map and starts the game."));
  }
  if(this.running() && this.drv.host()) top.appendChild(api.btn("End game", "ghost", function(){ self.drv.end(); }));
  var best = tdSave().best, keys = Object.keys(best);
  if(this.drv.solo && keys.length) top.appendChild(api.mk("span", "vg-muted td-best", "Best: " + keys.sort().map(function(k){ return k.replace("-", " ") + " " + best[k]; }).join(" · ")));
  var st = "";
  if(v && (this.running() || v.phase === "done")){
    st = MAPS[Math.max(0, MAP_IDS.indexOf(v.map))].name + " · " + v.diff + " · Wave " + v.wave + "/" + WAVES + " · ❤ " + v.lives + " · 🫐 " + this.myBerries();
    if(v.phase === "build") st += " · building";
  } else if(v && v.last) st = "Last game: " + v.last.key.replace("-", " ") + ", " + v.last.waves + " waves" + (v.last.win ? " (cleared!)" : "");
  else st = "Place Pokémon on the stones beside the path. Types matter: hover a bug to see each tower's multiplier.";
  if(pk()) this.status.textContent = st;
};
Board.prototype.renderSlots = function(v){
  var self = this, ov = this.ov, m = this.map(); ov.textContent = "";
  if(!v || !(this.running() || v.phase === "done")) return;
  var me = this.drv.me();
  m.slots.forEach(function(c, i){
    var t = self.towerAt(i), b = api.mk("button", "td-slot" + (t ? " has" : "") + (t && t.owner === me ? " mine" : "") + (self.sel === i ? " sel" : ""));
    b.type = "button"; b.setAttribute("data-slot", String(i));
    b.style.left = (c[0]/COLS*100) + "%"; b.style.top = (c[1]/ROWS*100) + "%";
    b.style.width = (100/COLS) + "%"; b.style.height = (100/ROWS) + "%";
    var label = "Slot " + (i+1);
    if(t){
      label += ": " + t.mon.name + " (" + t.mon.types.join("/") + "), targets " + MODE_NAME[MODES.indexOf(t.mode)] + (t.owner === me ? ". 1-3 target mode, S sells" : "");
      if(HQV.pk && HQV.pk.spriteEl){ var sp = HQV.pk.spriteEl(t.mon, false); sp.classList.add("td-spr"); b.appendChild(sp); }
      b.title = t.mon.name + " · " + t.mon.types.join("/") + " · " + (EFFECT[t.mon.types[0]] || "steady hits");
    } else label += self.pick ? ": empty. Enter places your pick" : ": empty";
    b.setAttribute("aria-label", label);
    b.addEventListener("click", function(){ self.slotAct(i); });
    b.addEventListener("keydown", function(e){
      if(e.key === "Enter"){ e.preventDefault(); self.slotAct(i); }
      else if(e.key === "1" || e.key === "2" || e.key === "3"){ e.preventDefault(); var tw = self.towerAt(i); if(tw && tw.owner === me) self.drv.target(i, MODES[parseInt(e.key, 10) - 1]); }
      else if(e.key === "s" || e.key === "S"){ e.preventDefault(); var tw2 = self.towerAt(i); if(tw2 && tw2.owner === me) self.drv.sell(i); }
    });
    ov.appendChild(b);
  });
  if(this.refocus != null){ var f = ov.querySelector('[data-slot="' + this.refocus + '"]'); if(f) f.focus(); this.refocus = null; }
};
Board.prototype.slotAct = function(i){
  var t = this.towerAt(i);
  this.refocus = i;
  if(t){ this.sel = this.sel === i ? null : i; this.update(true); return; }
  if(!this.pick){ api.toast("Pick a Pokémon from your roster first"); return; }
  this.drv.place(i, this.pick);
};
Board.prototype.renderActions = function(v){
  var self = this, a = this.actions; a.textContent = "";
  if(!v || v.phase !== "build") return;
  var me = this.drv.me(), mine = (v.players || []).filter(function(p){ return p.user && p.user.userId === me; })[0];
  if(!mine) return;
  var label = this.drv.solo ? "Start wave " + (v.wave + 1) : mine.ready ? "Ready ✓ (waiting)" : "Ready for wave " + (v.wave + 1);
  var b = api.btn(label, "primary td-ready", function(){ self.drv.ready(); });
  if(mine.ready) b.disabled = true;
  a.appendChild(b);
  if(v.readyInMs != null) a.appendChild(api.mk("span", "vg-muted", "Starts in " + Math.ceil(v.readyInMs/1000) + " s"));
};
Board.prototype.renderTower = function(v){
  var self = this, box = this.towerBox; box.textContent = "";
  var t = this.sel != null ? this.towerAt(this.sel) : null;
  if(!t){ box.hidden = true; return; }
  box.hidden = false;
  var me = this.drv.me();
  box.appendChild(api.mk("h4", "td-h", t.mon.name + " · " + t.mon.types.join("/")));
  box.appendChild(api.mk("p", "vg-muted", (EFFECT[t.mon.types[0]] || "steady hits") + " · power " + t.base + " · every " + (t.cd/HZ) + " s · range " + (t.range/100) + " tiles"));
  if(t.owner !== me){ box.appendChild(api.mk("p", "vg-muted", "A teammate's tower.")); return; }
  var row = api.mk("div", "vg-row");
  MODES.forEach(function(m, k){
    var b = api.btn((k+1) + " " + MODE_NAME[k], t.mode === m ? "primary" : "ghost", function(){ self.drv.target(t.slot, m); });
    b.setAttribute("aria-pressed", t.mode === m ? "true" : "false"); row.appendChild(b);
  });
  row.appendChild(api.btn("Sell (+" + idiv(t.cost*SELL_PCT, 100) + ")", "ghost", function(){ self.sel = null; self.drv.sell(t.slot); }));
  box.appendChild(row);
};
Board.prototype.renderRoster = function(v){
  var self = this, box = this.rosterBox, P = pk(); box.textContent = "";
  if(!v || !this.running()) { box.hidden = true; return; }
  box.hidden = false;
  var mineCount = this.myTowers().length, cap = v.cap | 0, berries = this.myBerries();
  box.appendChild(api.mk("h4", "td-h", "Your Pokémon · towers " + mineCount + "/" + cap));
  if(!P){ box.appendChild(api.mk("p", "vg-muted", "Loading…")); return; }
  var list = api.mk("div", "td-roster");
  roster().forEach(function(r){
    var mon = P.buildMon(P.cleanSpec(r.spec)), cost = COST[r.spec.st], picked = self.pick && JSON.stringify(self.pick) === JSON.stringify(r.spec);
    var b = api.mk("button", "td-mon" + (picked ? " picked" : "")); b.type = "button";
    b.setAttribute("aria-pressed", picked ? "true" : "false");
    if(HQV.pk && HQV.pk.spriteEl){ var sp = HQV.pk.spriteEl(mon, false); sp.classList.add("td-spr"); b.appendChild(sp); }
    var txt = api.mk("span", "td-mon-t");
    txt.appendChild(api.mk("b", null, mon.name + (r.tag ? " · " + r.tag : "")));
    var types = api.mk("span", "pkb-types");
    mon.types.forEach(function(t){ var c = api.mk("span", "pkb-type", t); c.style.background = "hsl(" + ((window.POKE_TYPE_HUE || {})[t] || 220) + ",60%,42%)"; types.appendChild(c); });
    txt.appendChild(types);
    txt.appendChild(api.mk("span", "vg-muted", "🫐 " + cost + " · " + (EFFECT[mon.types[0]] || "steady hits")));
    b.appendChild(txt);
    if(berries < cost) b.classList.add("poor");
    b.title = KINDS.map(function(k, i){ return KIND_NAME[i] + " " + multText(mult16(mon.types[0], i)/16); }).join(" · ");
    b.addEventListener("click", function(){ self.pick = picked ? null : r.spec; self.update(true);
      if(self.pick) E.say("Picked " + mon.name + ". Choose a slot on the board."); });
    list.appendChild(b);
  });
  box.appendChild(list);
};
Board.prototype.renderPlayers = function(v){
  var box = this.playersBox; box.textContent = "";
  if(this.drv.solo || !v || !(v.players || []).length){ box.hidden = true; return; }
  box.hidden = false;
  box.appendChild(api.mk("h4", "td-h", "Team"));
  (v.players || []).forEach(function(p){
    box.appendChild(api.mk("div", "td-player", (MP ? MP.nameOf(p.user) : "?") + " · 🫐 " + p.berries + " · " + p.towers + " towers" + (p.away ? " · away" : p.ready ? " · ready" : "")));
  });
};
Board.prototype.snap = function(bugs, hits, now){
  var self = this, prev = {};
  Object.keys(this.cur).forEach(function(k){ prev[k] = self.cur[k]; });
  this.prev = prev; this.cur = {}; this.curAt = now;
  (bugs || []).forEach(function(b){ self.cur[b[0]] = b; });
  if(!api.calm()){
    var m = this.map();
    (hits || []).forEach(function(h){
      var c = m.slots[h[0]], b = self.cur[h[1]] || self.prev[h[1]]; if(!c || !b) return;
      self.fx.push({x0:c[0]*TILE + TILE/2, y0:c[1]*TILE + TILE/2, x1:b[2]*TILE/100, y1:b[3]*TILE/100, at:now, m:h[2]});
    });
    if(this.fx.length > 80) this.fx.splice(0, this.fx.length - 80);
  } else this.fx = [];
};
Board.prototype.draw = function(now, period){
  var cv = this.cv, g = cv.getContext("2d"), T = E.tokens(), m = this.map(), v = this.drv.view();
  g.fillStyle = T.panel2; g.fillRect(0, 0, cv.width, cv.height);
  g.strokeStyle = T.line; g.lineWidth = 1;
  for(var x = 0; x <= COLS; x++){ g.beginPath(); g.moveTo(x*TILE + 0.5, 0); g.lineTo(x*TILE + 0.5, cv.height); g.stroke(); }
  for(var y = 0; y <= ROWS; y++){ g.beginPath(); g.moveTo(0, y*TILE + 0.5); g.lineTo(cv.width, y*TILE + 0.5); g.stroke(); }
  // the path
  g.strokeStyle = T.bg2; g.lineWidth = TILE*0.8; g.lineCap = "square"; g.lineJoin = "miter";
  g.beginPath(); m.path.forEach(function(c, i){ var px = c[0]*TILE + TILE/2, py = c[1]*TILE + TILE/2; if(i) g.lineTo(px, py); else g.moveTo(px, py); }); g.stroke();
  g.strokeStyle = T.muted; g.lineWidth = 2; g.setLineDash([6, 8]);
  g.beginPath(); m.path.forEach(function(c, i){ var px = c[0]*TILE + TILE/2, py = c[1]*TILE + TILE/2; if(i) g.lineTo(px, py); else g.moveTo(px, py); }); g.stroke();
  g.setLineDash([]);
  var end = m.path[m.path.length-1];
  g.fillStyle = T.need; g.fillRect(end[0]*TILE + TILE - 6, end[1]*TILE + 4, 6, TILE - 8);
  // stones for towers
  m.slots.forEach(function(c){ g.fillStyle = T.panel; g.strokeStyle = T.line; g.lineWidth = 2; g.fillRect(c[0]*TILE + 4, c[1]*TILE + 4, TILE - 8, TILE - 8); g.strokeRect(c[0]*TILE + 4, c[1]*TILE + 4, TILE - 8, TILE - 8); });
  // the selected tower's range
  var sel = this.sel != null ? this.towerAt(this.sel) : null;
  if(sel){ var sc = m.slots[sel.slot]; g.strokeStyle = T.brand; g.lineWidth = 2; g.beginPath(); g.arc(sc[0]*TILE + TILE/2, sc[1]*TILE + TILE/2, sel.range*TILE/100, 0, Math.PI*2); g.stroke(); }
  // bugs, eased between snapshots
  var a = Math.max(0, Math.min(1, (now - this.curAt)/period)), self = this;
  var col = [T.good, T.need, T.ink, T.gold, T.brand, T.need];
  Object.keys(this.cur).forEach(function(k){
    var b = self.cur[k], p = self.prev[k], bx = b[2], by = b[3];
    if(p && !api.calm()){ bx = p[2] + (b[2]-p[2])*a; by = p[3] + (b[3]-p[3])*a; }
    var px = bx*TILE/100, py = by*TILE/100, r = b[1] === BOSS ? 14 : b[1] === 2 ? 10 : 8;
    g.fillStyle = col[b[1]] || T.ink; g.beginPath(); g.arc(px, py, r, 0, Math.PI*2); g.fill();
    g.strokeStyle = self.hover === b[1] ? T.brand : T.panel; g.lineWidth = self.hover === b[1] ? 3 : 2; g.stroke();
    if(b[1] === 3){ g.fillStyle = T.panel; g.fillRect(px - r - 4, py - 2, 4, 4); g.fillRect(px + r, py - 2, 4, 4); }
    if(b[5] & 1){ g.strokeStyle = T.brand; g.lineWidth = 1; g.beginPath(); g.arc(px, py, r + 4, 0, Math.PI*2); g.stroke(); }
    if(b[5] & 2){ g.fillStyle = T.good; g.fillRect(px - 2, py + r, 4, 5); }
    if(b[5] & 4){ g.fillStyle = T.need; g.fillRect(px + r - 2, py - r - 2, 4, 4); }
    if(b[5] & 8){ g.fillStyle = T.gold; g.fillRect(px - r - 2, py - r - 2, 4, 4); }
    g.fillStyle = T.line; g.fillRect(px - 12, py - r - 8, 24, 3);
    g.fillStyle = b[4] > 50 ? T.good : b[4] > 20 ? T.gold : T.need; g.fillRect(px - 12, py - r - 8, 24*b[4]/100, 3);
  });
  // hit lines and sparks (none under calm)
  if(!api.calm()){
    this.fx = this.fx.filter(function(f){ return now - f.at < 180; });
    this.fx.forEach(function(f){
      var k = 1 - (now - f.at)/180;
      g.globalAlpha = Math.max(0, k); g.strokeStyle = f.m > 16 ? T.gold : f.m < 16 ? T.muted : T.ink; g.lineWidth = f.m > 16 ? 3 : 2;
      g.beginPath(); g.moveTo(f.x0, f.y0); g.lineTo(f.x1, f.y1); g.stroke();
      g.fillStyle = f.m > 16 ? T.gold : T.ink;
      for(var s = 0; s < 3; s++){ var an = (f.at % 360 + s*120)*Math.PI/180, rr = 4 + 10*(1-k); g.fillRect(f.x1 + Math.cos(an)*rr - 1, f.y1 + Math.sin(an)*rr - 1, 3, 3); }
      g.globalAlpha = 1;
    });
  }
  if(v && v.phase === "done"){
    g.fillStyle = T.panel; g.globalAlpha = 0.85; g.fillRect(cv.width/2 - 170, cv.height/2 - 30, 340, 60); g.globalAlpha = 1;
    g.fillStyle = T.ink; g.font = "bold 20px " + T.mono; g.textAlign = "center";
    g.fillText(v.lives > 0 && v.cleared >= WAVES ? "All 20 waves cleared!" : "The bugs got through · " + v.cleared + " waves", cv.width/2, cv.height/2 + 7);
    g.textAlign = "left";
  }
};

/* ------------------------------ solo mode ------------------------------ */
function soloMount(el){
  var sim = null, raf = 0, acc = 0, last = 0, paused = false, alive = true, board = null;
  var drv = {
    solo: true,
    me: function(){ return "me"; },
    view: function(){ return sim ? sim.view() : {phase:"idle", towers:[], players:[]}; },
    bugs: function(){ return sim ? sim.snapBugs() : []; },
    host: function(){ return true; },
    start: function(map, diff){
      var sv = tdSave(); sv.runs = (sv.runs|0) + 1; api.persist();
      sim = new Sim(Math.max(0, MAP_IDS.indexOf(map)), Math.max(0, DIFFS.indexOf(diff)), api.rng("td:" + map + ":" + diff + ":" + sv.runs));
      board.sel = null; board.cur = {}; board.prev = {}; board.update(true);
      E.say("Defend the " + map + " on " + diff + ". Place Pokémon, then start wave 1.");
    },
    end: function(){
      if(!sim || (sim.phase !== "build" && sim.phase !== "wave")) return;
      if(sim.cleared) record(sim.finish(false)); else sim = null;
      board.cur = {}; board.update(true);
    },
    place: function(slot, spec){ if(!sim) return; var e = sim.place("me", slot, spec); if(e) api.toast(e); else E.sfx("ui"); board.update(true); },
    sell: function(slot){ if(!sim) return; var e = sim.sell("me", slot); if(e) api.toast(e); board.sel = null; board.update(true); },
    target: function(slot, mode){ if(!sim) return; var e = sim.target("me", slot, mode); if(e) api.toast(e); board.update(true); },
    ready: function(){ if(!sim || sim.phase !== "build") return; sim.beginWave(); E.say("Wave " + sim.wave + " of " + WAVES); E.sfx("go"); board.update(true); }
  };
  // The best per "<map>-<diff>" moves up as each wave is cleared, not only at the end.
  function bestNote(v){
    var sv = tdSave(), k = v.map + "-" + v.diff;
    if(v.cleared > 0 && !(sv.best[k] >= v.cleared)){ sv.best[k] = v.cleared; api.persist(); }
  }
  function record(d){
    var sv = tdSave(), k = d.key;
    if(!(sv.best[k] >= d.waves)) sv.best[k] = d.waves;
    api.persist();
    if(d.win){ note("td-clear"); E.say("All 20 waves cleared! " + k.replace("-", " ") + " is safe."); E.sfx("finish"); api.toast("🛡️ All 20 waves cleared!", "ach"); }
    else E.say("The bugs broke through after " + d.waves + " waves.");
  }
  board = new Board(el, drv);
  board.update(true);
  function frame(ts){
    if(!alive) return;
    raf = requestAnimationFrame(frame);
    if(!last) last = ts;
    var dt = Math.min(500, ts - last); last = ts;
    if(sim && sim.phase === "wave" && !paused){
      acc += dt;
      while(acc >= 1000/HZ && sim.phase === "wave"){
        acc -= 1000/HZ;
        var lives = sim.lives, evs = sim.step();
        if(sim.lives < lives) E.say("A bug got through: " + sim.lives + " lives left");
        if(sim.tick % SNAP_EVERY === 0 || evs.length) board.snap(sim.snapBugs(), sim.hits.splice(0), performance.now());
        evs.forEach(function(e){
          if(e.view){ note("td-wave"); bestNote(sim.view()); E.say("Wave " + sim.cleared + " cleared. Build, then start wave " + (sim.cleared + 1) + "."); board.cur = {}; board.prev = {}; }
          if(e.done){ if(e.done.win || sim.cleared) note("td-wave"); record(e.done); board.cur = {}; }
        });
        board.update(false);
      }
    } else acc = 0;
    board.draw(performance.now(), 1000/HZ*SNAP_EVERY);
  }
  raf = requestAnimationFrame(frame);
  return {stop: function(){ alive = false; cancelAnimationFrame(raf); }, pause: function(){ paused = true; }, resume: function(){ paused = false; last = 0; }};
}
var SOLO = null;
HQV.register({id:"td", name:"Tower Defense", icon:"🛡️", desc:"Your Pokémon vs bug waves",
  mount: function(el){ SOLO = soloMount(el); },
  unmount: function(){ if(SOLO) SOLO.stop(); SOLO = null; },
  pause: function(){ if(SOLO) SOLO.pause(); },
  resume: function(){ if(SOLO) SOLO.resume(); },
  badge: function(){ var b = (api.save && api.save.td && api.save.td.best) || {}, n = 0; Object.keys(b).forEach(function(k){ n = Math.max(n, b[k]|0); }); return n ? "best " + n + " waves" : ""; }});

/* ------------------------------ co-op mode ----------------------------- */
if(!MP) return;
function hostOf(s){ var me = MP.me(), h = false; (s.lobby || []).forEach(function(m){ if(m.userId === me && m.host) h = true; }); return h; }
MP.handlers.td = {
  on: function(m, s){
    var now = performance.now(), B = s.board;
    if(m.ev === "td"){
      var prev = s.td; s.td = m.td;
      if(m.by) api.toast("🛡️ " + MP.nameOf(m.by) + " started Tower Defense");
      if(m.td.phase !== "wave"){ s.bugs = []; if(B){ B.cur = {}; B.prev = {}; } }
      if(prev && prev.phase === "wave" && m.td.phase === "build"){ note("td-wave"); E.say("Wave " + m.td.cleared + " cleared. Ready up for wave " + (m.td.cleared + 1) + "."); }
      if(m.td.readyInMs != null) s.readyAt = Date.now() + m.td.readyInMs; else s.readyAt = null;
    }
    if(m.ev === "wave"){ E.say("Wave " + m.n + " of " + WAVES); E.sfx("go"); if(s.td){ s.td.phase = "wave"; s.td.wave = m.n; s.td.readyInMs = null; } s.readyAt = null; }
    if(m.ev === "tsnap"){
      if(s.td){
        if(m.lives < s.td.lives) E.say("A bug got through: " + m.lives + " lives left");
        s.td.lives = m.lives;
        (m.berries || []).forEach(function(b){ (s.td.players || []).forEach(function(p){ if(p.user.userId === b[0]) p.berries = b[1]; }); });
      }
      s.bugs = m.bugs || [];
      if(B) B.snap(s.bugs, m.hits, now);
    }
    if(m.ev === "done"){
      s.bugs = []; if(B){ B.cur = {}; }
      if(s.td){ s.td.phase = "done"; s.td.cleared = m.waves; s.td.last = {key:m.key, waves:m.waves, win:m.win}; }
      if(m.win){ note("td-clear"); E.say("All 20 waves cleared together!"); E.sfx("finish"); api.toast("🛡️ All 20 waves cleared!", "ach"); }
      else E.say("The bugs broke through after " + m.waves + " waves.");
      if(m.waves) note("td-wave");
    }
  },
  render: function(ctx, s){ if(s.board) s.board.update(false); }
};
MP.register("td", "🛡️", "Co-op tower defense with your Pokémon, up to 4", function(ctx){
  var s = MP.st("td"), alive = true, raf = 0;
  var drv = {
    solo: false,
    me: function(){ return MP.me(); },
    view: function(){
      var v = s.td || {phase:"idle", towers:[], players:[]};
      if(s.readyAt) v.readyInMs = Math.max(0, s.readyAt - Date.now());
      return v;
    },
    bugs: function(){ return s.bugs || []; },
    host: function(){ return hostOf(s); },
    start: function(map, diff){ MP.send("td", "start", {map:map, diff:diff}); },
    end: function(){ MP.send("td", "end"); },
    place: function(slot, spec){ MP.send("td", "place", {slot:slot, mon:{sp:spec.sp, st:spec.st, br:spec.br, mg:spec.mg, sh:spec.sh}}); },
    sell: function(slot){ MP.send("td", "sell", {slot:slot}); },
    target: function(slot, mode){ MP.send("td", "target", {slot:slot, mode:mode}); },
    ready: function(){ MP.send("td", "ready"); }
  };
  s.board = new Board(ctx.box, drv);
  s.board.update(true);
  ctx.onRejoin = function(){ MP.send("td", "view"); };
  MP.send("td", "view");
  var tick = setInterval(function(){ if(s.readyAt && s.board) s.board.update(true); }, 1000);
  (function frame(){ if(!alive) return; raf = requestAnimationFrame(frame); s.board.draw(performance.now(), 1000/HZ*SNAP_EVERY); })();
  ctx.stop = function(){ alive = false; cancelAnimationFrame(raf); clearInterval(tick); s.board = null; };
});
})();
