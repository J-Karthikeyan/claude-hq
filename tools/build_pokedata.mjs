// Dev-only: regenerates the vendored Pokemon battle data. NOT part of the runtime.
//
//   node tools/build_pokedata.mjs            (Node 22.6+ / 26: imports .ts natively)
//   node tools/build_pokedata.mjs <dir>      (use already-downloaded pokedex/moves/learnsets/typechart .ts)
//
// Downloads Pokemon Showdown's data files at a pinned commit (MIT, see
// LICENSES/pokemon-showdown-MIT.txt) into a temp dir, keeps only the species reachable
// through index.html's POKE_EVO / POKE_BRANCH / MEGA_FORMS, picks a canonical 4-move
// level-up set per evolution stage, and writes two byte-identical payloads:
//   games/pokedata.js               window.HQV_POKEDATA = {...}   (served by dashboard.py)
//   backend/data/pokemon.json       the same JSON                 (the Arena duel referee)
// tests/test_pokedata.py checks the two match and that every referenced id exists.
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import {fileURLToPath, pathToFileURL} from "node:url";

const SHA = "14546894d86f9589ac11130c510bbe73b6968665";
const RAW = "https://raw.githubusercontent.com/smogon/pokemon-showdown/" + SHA + "/";
const ROOT = path.dirname(path.dirname(fileURLToPath(import.meta.url)));

let src = process.argv[2];
if(!src){
  src = fs.mkdtempSync(path.join(os.tmpdir(), "pokedata-"));
  for(const f of ["pokedex", "moves", "learnsets", "typechart"]){
    const r = await fetch(RAW + "data/" + f + ".ts");
    if(!r.ok) throw new Error(f + ".ts: HTTP " + r.status);
    fs.writeFileSync(path.join(src, f + ".ts"), await r.text());
  }
}
const imp = f => import(pathToFileURL(path.join(path.resolve(src), f + ".ts")).href);
const {Pokedex} = await imp("pokedex"), {Moves} = await imp("moves");
const {Learnsets} = await imp("learnsets"), {TypeChart} = await imp("typechart");

// ---- what the app can show: parsed from the page's script (ui/app/*.js) so the two never drift ----
const appDir = path.join(ROOT, "ui", "app");
const html = fs.readdirSync(appDir).filter(f => f.endsWith(".js")).sort()
  .map(f => fs.readFileSync(path.join(appDir, f), "utf8")).join("\n");
const POKE_EVO = JSON.parse(/var POKE_EVO=(\[\[[\d,\[\]]*\]\]);/.exec(html)[1]);
const branchSrc = /var POKE_BRANCH=\{([\s\S]*?)\n\};/.exec(html)[1];
const BRANCHES = {};
for(const m of branchSrc.matchAll(/(\d+):\[([^\n]*?)\]\s*,?\s*(?:\/\/[^\n]*)?\n?/g))
  BRANCHES[m[1]] = [...m[2].matchAll(/dex:(\d+)/g)].map(x => +x[1]);
const megaSrc = /var MEGA_FORMS=\{([\s\S]*?)\n\};/.exec(html)[1];
const MEGA_FOR = {}, MEGA_NAME = {};
for(const m of megaSrc.matchAll(/(\d+):\[([^\]]*)\]/g))
  for(const f of m[2].matchAll(/name:"([^"]+)",slug:"([^"]+)"/g)){
    (MEGA_FOR[m[1]] = MEGA_FOR[m[1]] || []).push(f[2]); MEGA_NAME[f[2]] = f[1];
  }
// PokeAPI sprite ids of the Mega forms (other/showdown/<id>.gif on the existing sprite CDN).
const MEGA_ID = {"charizard-megax":10034,"charizard-megay":10035,"blastoise-mega":10036,"venusaur-mega":10033,"gengar-mega":10038,"lucario-mega":10059,"gardevoir-mega":10051,"gallade-mega":10068,"gyarados-mega":10041,"garchomp-mega":10058,"tyranitar-mega":10049,"scizor-mega":10046,"aggron-mega":10053,"alakazam-mega":10037,"aerodactyl-mega":10042,"blaziken-mega":10050,"pidgeot-mega":10073,"slowbro-mega":10071,"sceptile-mega":10065,"swampert-mega":10064,"salamence-mega":10089,"metagross-mega":10076,"mewtwo-megax":10043,"mewtwo-megay":10044,"sharpedo-mega":10070,"kangaskhan-mega":10039,"steelix-mega":10072,"absol-mega":10057,"beedrill-mega":10090,"altaria-mega":10067,"abomasnow-mega":10060,"heracross-mega":10047,"camerupt-mega":10087,"manectric-mega":10055,"ampharos-mega":10045,"houndoom-mega":10048};

const toId = s => s.toLowerCase().replace(/[^a-z0-9]/g, "");
const byNum = {};
for(const [id, s] of Object.entries(Pokedex)) if(!s.forme && !byNum[s.num]) byNum[s.num] = id;
const dexes = new Set(POKE_EVO.flat());
Object.values(BRANCHES).flat().forEach(d => dexes.add(d));
const megaSlugs = Object.values(MEGA_FOR).flat();

// ---- only moves whose whole effect the battle engines implement ----
const BAD = ["ohko","multihit","selfdestruct","selfSwitch","forceSwitch","volatileStatus","sideCondition","weather","terrain",
  "pseudoWeather","slotCondition","stallingMove","breaksProtect","isZ","isMax","ignoreImmunity","overrideOffensiveStat",
  "overrideDefensiveStat","overrideOffensivePokemon","sleepUsable","struggleRecoil","selfBoost","willCrit","thawsTarget",
  "hasCrashDamage","mindBlownRecoil","condition","isFutureMove","callsMove","tracksTarget","smartTarget","multiaccuracy",
  "ignoreAbility","ignoreDefensive","ignoreEvasion","onDamagePriority","nonGhostTarget","self"];
const STATUSES = ["par", "brn", "psn", "tox", "slp"];
function simple(m){
  if(!m || m.isNonstandard) return false;
  for(const [k, v] of Object.entries(m)){ if(typeof v === "function" || BAD.includes(k)) return false; }
  if(m.damage !== undefined && m.damage !== "level") return false;
  if(m.flags && (m.flags.charge || m.flags.recharge || m.flags.futuremove)) return false;
  if(["all","allySide","foeSide","ally","adjacentAlly","adjacentAllyOrSelf"].includes(m.target)) return false;
  if(m.secondaries) return false;
  if(m.secondary){
    const s = m.secondary;
    if(!Object.keys(s).every(k => ["chance","status","boosts","volatileStatus","self"].includes(k))) return false;
    if(s.status && !STATUSES.includes(s.status)) return false;
    if(s.volatileStatus && s.volatileStatus !== "flinch") return false;
    if(s.self && !(Object.keys(s.self).length === 1 && s.self.boosts)) return false;
  }
  if(m.category === "Status"){
    const has = ["boosts","status","heal"].filter(k => m[k]);
    if(has.length !== 1) return false;
    if(m.status && !STATUSES.includes(m.status)) return false;
  } else if(m.boosts || m.status || m.heal) return false;
  return true;
}
const LV = [8, 18, 30, 42, 55];     // battle level for creature evolution stage 0..4
function lvlMoves(id){
  const L = Learnsets[id].learnset; let best = 0;
  for(const srcs of Object.values(L)) for(const s of srcs){ const mm = /^(\d)L(\d+)$/.exec(s); if(mm) best = Math.max(best, +mm[1]); }
  const out = [];
  for(const [mv, srcs] of Object.entries(L)) for(const s of srcs){ const mm = /^(\d)L(\d+)$/.exec(s); if(mm && +mm[1] === best) out.push([mv, +mm[2]]); }
  return {gen: best, moves: out};
}
function pick(id, level){
  const sp = Pokedex[id]; let {gen, moves} = lvlMoves(id);
  // Canonical: an evolved Pokemon keeps the moves it learned as its pre-evolutions.
  for(let p = sp.prevo; p; p = Pokedex[toId(p)].prevo){ const pid = toId(p); if(Learnsets[pid] && Learnsets[pid].learnset) moves = moves.concat(lvlMoves(pid).moves); }
  const uniq = [...new Set(moves.filter(([mv, l]) => l <= level && simple(Moves[mv])).map(([mv]) => mv))];
  const score = mv => { const m = Moves[mv]; if(m.category === "Status") return 0; if(m.damage === "level") return 60 * m.accuracy / 100;
    const acc = m.accuracy === true ? 100 : m.accuracy; return m.basePower * (sp.types.includes(m.type) ? 1.5 : 1) * acc / 100; };
  const dmg = uniq.filter(mv => Moves[mv].category !== "Status").sort((a, b) => score(b) - score(a) || a.localeCompare(b));
  const st = uniq.filter(mv => Moves[mv].category === "Status").sort((a, b) => Moves[b].num - Moves[a].num);
  const set = [], stab = dmg.filter(mv => sp.types.includes(Moves[mv].type));
  if(stab[0]) set.push(stab[0]);                                                   // best STAB
  const cov = dmg.find(mv => !set.includes(mv) && !sp.types.includes(Moves[mv].type)); if(cov) set.push(cov);   // coverage
  for(const mv of stab) if(set.length < 3 && !set.includes(mv) && Moves[mv].type !== Moves[set[0]].type) set.push(mv);
  if(st.length) set.push(st[0]);                                                   // newest status move
  for(const mv of dmg) if(set.length < 4 && !set.includes(mv)) set.push(mv);
  for(const mv of st) if(set.length < 4 && !set.includes(mv)) set.push(mv);
  if(!set.some(mv => Moves[mv].category !== "Status")){
    // No damaging level-up move yet: the best <=60bp damaging move from any non-event source.
    const L = Learnsets[id].learnset, pref = mv => sp.types.includes(Moves[mv].type) ? 2 : Moves[mv].type === "Normal" ? 1 : 0;
    const any = Object.entries(L).filter(([mv, s]) => s.some(x => !/S/.test(x)) && simple(Moves[mv]) && Moves[mv].category !== "Status" && Moves[mv].basePower <= 60)
      .map(([mv]) => mv).sort((a, b) => pref(b) - pref(a) || score(b) - score(a) || a.localeCompare(b));
    set.unshift(any[0] || "tackle");
  }
  return set.slice(0, 4);
}

const P = {}, used = new Set();
function add(id, extra){
  const sp = Pokedex[id]; if(!sp) throw new Error("missing species " + id);
  const lid = Learnsets[id] && Learnsets[id].learnset ? id : toId(sp.baseSpecies);
  const e = {num: sp.num, name: sp.name, types: sp.types, bs: sp.baseStats, moves: {}};
  Object.assign(e, extra || {});
  for(const lv of LV){ e.moves[lv] = pick(lid, lv); e.moves[lv].forEach(m => used.add(m)); }
  P[id] = e;
}
const byDex = {}, megas = {};
for(const d of [...dexes].sort((a, b) => a - b)){ const id = byNum[d]; if(!id) throw new Error("no species for dex " + d); add(id); byDex[d] = id; }
for(const slug of megaSlugs){
  const id = slug.replace("-", ""); if(!MEGA_ID[slug]) throw new Error("no sprite id for " + slug);
  add(id, {name: MEGA_NAME[slug], forme: Pokedex[id].forme, sprite: MEGA_ID[slug]}); megas[slug] = id;
}
const M = {};
for(const mv of [...used].sort()){
  const m = Moves[mv];
  const o = {name: m.name, type: m.type, cat: m.category.toLowerCase(), bp: m.basePower, acc: m.accuracy === true ? 0 : m.accuracy, pp: m.pp, pri: m.priority};
  if(m.drain) o.drain = m.drain; if(m.recoil) o.recoil = m.recoil; if(m.heal) o.heal = m.heal; if(m.critRatio > 1) o.crit = m.critRatio;
  if(m.status) o.status = m.status; if(m.damage === "level") o.fixed = "level"; if(m.boosts) o.boosts = m.boosts; if(m.target === "self") o.self = 1;
  if(m.secondary) o.sec = m.secondary;
  M[mv] = o;
}
M.struggle = {name: "Struggle", type: "???", cat: "physical", bp: 50, acc: 0, pp: 0, pri: 0, struggle: 1};
// Attacker -> {defender: multiplier}, 1x omitted (Showdown's damageTaken: 1 = 2x, 2 = 0.5x, 3 = 0x).
const chart = {}, cap = t => t[0].toUpperCase() + t.slice(1);
for(const [def, d] of Object.entries(TypeChart)){
  if(def === "stellar") continue;
  for(const [atk, v] of Object.entries(d.damageTaken)){
    if(!/^[A-Z]/.test(atk) || atk === "Stellar" || !v) continue;
    (chart[atk] = chart[atk] || {})[cap(def)] = [1, 2, 0.5, 0][v];
  }
}
const sortKeys = o => Object.fromEntries(Object.keys(o).sort().map(k => [k, o[k]]));
for(const k of Object.keys(chart)) chart[k] = sortKeys(chart[k]);

const out = {
  source: "smogon/pokemon-showdown@" + SHA.slice(0, 8) + " (MIT)",
  license: "MIT (c) 2011-2026 Guangcong Luo and other contributors",
  levels: LV, lines: POKE_EVO, branches: BRANCHES, megaFor: MEGA_FOR,
  byDex, megas, chart: sortKeys(chart), pokemon: P, moves: M
};
const json = JSON.stringify(out);
const LICENSE = fs.readFileSync(path.join(ROOT, "LICENSES", "pokemon-showdown-MIT.txt"), "utf8")
  .replace(/https?:\/\//g, "").trim().split("\n").map(l => (" * " + l).trimEnd()).join("\n");
const js = "/* Pokemon battle data, generated by tools/build_pokedata.mjs - do not edit by hand.\n" +
  " * Species, base stats, types, level-up moves and the type chart come from Pokemon Showdown\n" +
  " * (smogon/pokemon-showdown@" + SHA + "), used under the MIT License:\n *\n" + LICENSE + "\n */\n" +
  "window.HQV_POKEDATA = " + json + ";\n";
fs.writeFileSync(path.join(ROOT, "games", "pokedata.js"), js);
fs.writeFileSync(path.join(ROOT, "backend", "data", "pokemon.json"), json + "\n");
console.log("species", Object.keys(P).length, "moves", Object.keys(M).length, "bytes", json.length);
for(const [id, lv] of [["pikachu", 30], ["charizard", 55], ["gyarados", 42], ["gengar", 55], ["alakazam", 55], ["kakuna", 18], ["magikarp", 55]])
  console.log(id + "@" + lv, P[id].moves[lv].map(m => M[m].name).join(", "));
