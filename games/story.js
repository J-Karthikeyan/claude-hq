/* Story campaign: a light storyline through HQ and the Valley.
   Six chapters, each with a few goals tied to real activity (your level, your Claude tabs,
   your Pokédex, other Valley games and the 2.5 makers). Finishing a chapter unlocks area
   pages with deep links and a few story tracks/levels you play through the editors.
   Local only: nothing leaves the machine unless you race a story track in a room.
   Nothing else in HQ is ever gated on the story, and the story grants no coins. */
(function(){
"use strict";
var HQV = window.HQV; if(!HQV || !HQV.api) return;
var api = HQV.api, mk = api.mk, btn = api.btn;

// The note events other games send through HQV.story.note(ev, n).
var NOTES = ["td-wave", "td-clear", "bowl-strike", "bowl-game", "make-save", "make-publish", "race-custom"];

// Story tracks and levels (MapDocs). A pure JSON array: backend/src/story_check.rs parses this
// block and runs every doc through the Arena's own validators, so keep it plain JSON.
var STORY_MAPS = /* STORY-MAPS BEGIN */[
 {"kind":"kart","v":1,"name":"Rookie Ring","data":{"tiles":"FSSSRSSRSSSSSSRSSRSS","scenery":"forest","theme":{"sky":"#a8daf2","fog":"#d6edf6","ground":"#7cbf5e"}}},
 {"kind":"kart","v":1,"name":"Orchard Bend","data":{"tiles":"FSSRRLSSRSSSLRSSRSSRSLSRSS","scenery":"tents","theme":{"sky":"#f5d9a0","fog":"#f8e8c8","ground":"#b9b55a"}}},
 {"kind":"kart","v":1,"name":"Summit Switchbacks","data":{"tiles":"FSSSRSSSSRSSLRSSSSSSRSSSRSSLSRSS","scenery":"empty","theme":{"sky":"#c4d2ec","fog":"#e6ecf6","ground":"#9db07a"}}},
 {"kind":"plat","v":1,"name":"Lantern Steps","data":{"kill":-6,"coopGoal":3,"coopSecs":120,"theme":{"sky":"#9fd6ff","fog":"#d2efff","sea":"#5aa9e6","light":"#fff6e0"},"spawns":[[-1,0,1],[1,0,1]],"cps":[[0,0.5,-6],[2,1.5,-18]],"flag":[2,2,-24],"coins":[[0,1.6,-3.5],[0,2,-8.5],[1,2.5,-14.5],[2,3,-20.5]],"solids":[{"m":"platform-large","x":0,"y":-0.5,"z":0},{"m":"platform-medium","x":0,"y":0,"z":-6},{"m":"platform-large","x":0,"y":0.5,"z":-12},{"m":"platform-medium","x":2,"y":1,"z":-18},{"m":"platform-large","x":2,"y":1.5,"z":-24}],"route":[[0,0,-2,"w"],[0,0.5,-5,"j"],[0,0.5,-7,"w"],[0,1,-10,"j"],[0,1,-12,"w"],[1,1,-14,"w"],[2,1.5,-17,"j"],[2,1.5,-19,"w"],[2,2,-22,"j"],[2,2,-24,"w"]],"deco":[{"m":"cloud","x":8,"y":6,"z":-10,"r":0,"s":3}]}},
 {"kind":"plat","v":1,"name":"Garden Hop","data":{"kill":-6,"coopGoal":2,"coopSecs":90,"theme":{"sky":"#b8e6c8","fog":"#e0f5e8","sea":"#62b3c9","light":"#fff8e8"},"spawns":[[-1,0,0],[0,0,1]],"cps":[[6,0.5,0],[12,1,-5]],"flag":[12,1.5,-11],"coins":[[3.5,1.4,0],[8.5,1.8,0],[12,2.1,-7.5]],"solids":[{"m":"platform-grass-large-round","x":0,"y":-0.5,"z":0},{"m":"platform-medium","x":6,"y":0,"z":0},{"m":"platform-large","x":12,"y":0.5,"z":0},{"m":"platform-medium","x":12,"y":0.5,"z":-5},{"m":"platform-grass-large-round","x":12,"y":1,"z":-11}],"route":[[2,0,0,"w"],[5,0.5,0,"j"],[7,0.5,0,"w"],[10,1,0,"j"],[12,1,-2,"w"],[12,1,-4.5,"j"],[12,1,-6,"w"],[12,1.5,-9,"j"],[12,1.5,-11,"w"]],"deco":[]}}
]/* STORY-MAPS END */;

/* ---------- chapters ----------
   goal kinds: level (window.myLevel), act (api.activity), catch (fetchPokedex),
   valley (other games' saves), note (HQV.story.note counters).
   unlocks: {area} pages with a deep link, {map: index into STORY_MAPS}. */
var PROF = "Professor Pine", CLAUDE = "Claude";
var CHAPTERS = [
 {id:"c1", name:"A Desk in the Valley", icon:"🌱",
  intro:[[PROF, "Welcome! Every prompt you send ripples out into this Valley. Let's see what grows."],
         [CLAUDE, "I'll keep working in your tabs. You keep an eye on what we make together."]],
  outro:[PROF, "A fine start. The road out of the Valley is open now."],
  goals:[{k:"level", n:2, label:"Reach level 2"},
         {k:"act", m:"prompts30", n:10, label:"Send 10 prompts this season"},
         {k:"catch", m:"caught", n:1, label:"Catch your first creature"},
         {k:"valley", m:"bag", n:3, label:"Carry 3 Valley finds in your bag"}],
  unlocks:[{area:"valley", name:"The Valley", icon:"🌾", text:"Fish, farm and mine while your tabs work. Finds fill your bag.", go:{view:"valley"}},
           {area:"pokedex", name:"Pokédex", icon:"📕", text:"Every session hatches a creature. Catch them all here.", go:{view:"pokedex"}}]},
 {id:"c2", name:"Bug Season", icon:"🐛",
  intro:[[PROF, "Bugs! A whole swarm, marching on the garden path."],
         [CLAUDE, "Bugs are my specialty. Line up your Pokémon as towers and hold the line."]],
  outro:[CLAUDE, "Swarm cleared. I've drawn you a track to celebrate."],
  goals:[{k:"level", n:5, label:"Reach level 5"},
         {k:"note", ev:"td-wave", n:5, label:"Hold back 5 bug waves in Tower Defense"},
         {k:"catch", m:"caught", n:3, label:"Catch 3 different creatures"},
         {k:"act", m:"weekActiveDays", n:2, label:"Work with Claude on 2 days this week"}],
  unlocks:[{area:"td", name:"Tower Defense", icon:"🛡️", text:"Your Pokémon against bug waves, solo or co-op.", go:{game:"td"}},
           {map:0}]},
 {id:"c3", name:"The Workshop", icon:"🛠️",
  intro:[[PROF, "Racers keep asking for new roads. Why not build them yourself?"],
         [CLAUDE, "Paint a loop in the Track Editor, or race the one I drew for you."]],
  outro:[PROF, "The Workshop is yours. Two more of Claude's roads are on the board."],
  goals:[{k:"note", ev:"make-save", n:1, label:"Save something you made in an editor"},
         {k:"note", ev:"race-custom", n:1, label:"Race a custom track or level"},
         {k:"level", n:8, label:"Reach level 8"},
         {k:"valley", m:"bag", n:10, label:"Carry 10 Valley finds"}],
  unlocks:[{area:"workshop", name:"Workshop", icon:"🧰", text:"Make tracks, levels and maps, then share them in the gallery.", go:{view:"workshop", game:"make-kart"}},
           {map:1}, {map:3}]},
 {id:"c4", name:"Strike Night", icon:"🎳",
  intro:[[PROF, "The lanes are lit tonight. Your buddy wants to bowl."],
         [CLAUDE, "Aim, power, a little spin. I'll keep score."]],
  outro:[CLAUDE, "Strikes on the sheet! The Compete hall has opened its doors."],
  goals:[{k:"note", ev:"bowl-game", n:1, label:"Finish a game of Bowling"},
         {k:"note", ev:"bowl-strike", n:3, label:"Roll 3 strikes"},
         {k:"level", n:12, label:"Reach level 12"},
         {k:"act", m:"streak", n:3, label:"Keep a 3-day streak"}],
  unlocks:[{area:"bowl", name:"Bowling", icon:"🎳", text:"Ten pins, solo or with friends.", go:{game:"bowl"}},
           {area:"compete", name:"Compete", icon:"🏆", text:"Weekly cups, the world boss, skills and trades.", go:{view:"compete"}}]},
 {id:"c5", name:"Builders' Guild", icon:"🏗️",
  intro:[[PROF, "The Guild only lets in makers whose work others can play."],
         [CLAUDE, "Publish one map and clear a full bug campaign. I'll vouch for you."]],
  outro:[PROF, "Welcome to the Guild. Claude left you the hardest road it knows."],
  goals:[{k:"note", ev:"make-publish", n:1, label:"Publish a map to the gallery"},
         {k:"note", ev:"make-save", n:3, label:"Save 3 things in the editors"},
         {k:"note", ev:"td-clear", n:1, label:"Clear all waves of a Tower Defense map"},
         {k:"catch", m:"caught", n:10, label:"Catch 10 different creatures"},
         {k:"level", n:16, label:"Reach level 16"}],
  unlocks:[{area:"gallery", name:"Map gallery", icon:"🖼️", text:"Play, like and remix what other trainers built.", go:{view:"workshop"}},
           {map:2}, {map:4}]},
 {id:"c6", name:"Champion of the Valley", icon:"👑",
  intro:[[PROF, "One last trial. The whole Valley is watching."],
         [CLAUDE, "Every road, every lane, every wave. We've done harder things in a single tab."]],
  outro:[PROF, "Champion! The story is told, but the Valley keeps growing with every prompt."],
  goals:[{k:"level", n:20, label:"Reach level 20"},
         {k:"note", ev:"td-clear", n:3, label:"Clear 3 Tower Defense maps"},
         {k:"note", ev:"bowl-game", n:5, label:"Finish 5 games of Bowling"},
         {k:"note", ev:"race-custom", n:5, label:"Race 5 custom tracks or levels"},
         {k:"catch", m:"shiny", n:1, label:"Catch a shiny"}],
  unlocks:[{area:"arena", name:"Arena", icon:"🏟️", text:"Boards, trophies and your trainer card.", go:{view:"arena"}}]}
];

/* ---------- save: api.save.story {ch, done:{id:day}, notes:{ev:n}} (self-healing) ---------- */
function plainObj(o){ return o && typeof o === "object" && !Array.isArray(o); }
function ssave(){
  var s = api.save;
  if(!plainObj(s)) return {ch:0, done:{}, notes:{}};
  if(!plainObj(s.story)) s.story = {};
  var st = s.story;
  if(typeof st.ch !== "number" || !isFinite(st.ch) || st.ch < 0) st.ch = 0;
  st.ch = Math.min(CHAPTERS.length, Math.floor(st.ch));
  if(!plainObj(st.done)) st.done = {};
  if(!plainObj(st.notes)) st.notes = {};
  return st;
}
function doneCount(){ var st = ssave(), n = 0; while(n < CHAPTERS.length && st.done[CHAPTERS[n].id]) n++; return n; }

/* ---------- metrics ---------- */
var DEX = null, DEX_AT = 0, MAPS_PUB = 0;
function level(){
  var v = null;
  try { v = typeof window.myLevel === "function" ? window.myLevel() : null; } catch(e){ v = null; }
  if(typeof v === "number") return v|0;
  if(v && typeof v === "object" && v.level != null) return v.level|0;
  return (api.activity().level|0);
}
function loadDex(force){
  if(typeof window.fetchPokedex !== "function") return Promise.resolve(null);
  if(!force && DEX && Date.now() - DEX_AT < 30000) return Promise.resolve(DEX);
  return window.fetchPokedex().then(function(d){ DEX = plainObj(d) ? d : null; DEX_AT = Date.now(); return DEX; }, function(){ return DEX; });
}
// Your published maps (map-gallery) also count for "publish", when that Arena route answers.
var MAPS_AT = 0;
function loadMaps(){
  if(typeof fetch !== "function" || Date.now() - MAPS_AT < 60000) return Promise.resolve();
  MAPS_AT = Date.now();
  return fetch("/api/arena/maps?mine=1", {cache:"no-store"}).then(function(r){ return r.ok ? r.json() : null; }).then(function(d){
    var ms = d && Array.isArray(d.maps) ? d.maps : [];
    MAPS_PUB = ms.filter(function(m){ return m && (m.scope === "public" || m.scope === "room"); }).length;
  }, function(){});
}
function valleyMetric(m){
  var s = plainObj(api.save) ? api.save : {};
  if(m === "bag"){ var t = 0, inv = plainObj(s.inv) ? s.inv : {}; for(var k in inv) t += Math.max(0, inv[k]|0); return t; }
  if(m === "golf"){ return plainObj(s.golf) && plainObj(s.golf.best) ? Object.keys(s.golf.best).length : 0; }
  if(m === "battle"){ return plainObj(s.battle) ? (s.battle.wins|0) : 0; }
  return 0;
}
function catchMetric(m){
  if(!DEX) return 0;
  if(m === "shiny") return DEX.shinyCount|0;
  return DEX.caughtCount|0;
}
function noteCount(ev){
  var n = ssave().notes[ev]|0;
  if(ev === "make-publish") n = Math.max(n, MAPS_PUB);
  return n;
}
function goalValue(g){
  if(g.k === "level") return level();
  if(g.k === "act"){ var a = api.activity(); return a[g.m]|0; }
  if(g.k === "catch") return catchMetric(g.m);
  if(g.k === "valley") return valleyMetric(g.m);
  if(g.k === "note") return noteCount(g.ev);
  return 0;
}
function goalState(g){ var v = goalValue(g); return {v:Math.min(v, g.n), met:v >= g.n}; }
function chapterMet(c){ return c.goals.every(function(g){ return goalState(g).met; }); }

/* ---------- progression: completes the current chapter when every goal is met ---------- */
var VIEW = null;   // the mounted story page, if any
function advance(){
  var st = ssave(), i = doneCount(), changed = false;
  while(i < CHAPTERS.length && chapterMet(CHAPTERS[i])){
    st.done[CHAPTERS[i].id] = api.day(); changed = true;
    announce("Chapter "+(i+1)+" complete: "+CHAPTERS[i].name+". "+unlockText(CHAPTERS[i]));
    api.toast(CHAPTERS[i].icon+" Chapter "+(i+1)+" complete: "+CHAPTERS[i].name, "good");
    i++;
  }
  if(changed){ st.ch = Math.min(i, CHAPTERS.length - 1); api.persist(); }
  return changed;
}
function unlockText(c){
  var names = c.unlocks.map(function(u){ return u.map != null ? (STORY_MAPS[u.map] || {}).name : u.name; }).filter(Boolean);
  return names.length ? "Unlocked: "+names.join(", ")+"." : "";
}
function unlockedList(){
  var out = [], n = doneCount();
  for(var i = 0; i < n; i++) CHAPTERS[i].unlocks.forEach(function(u){ out.push(u.map != null ? "map:"+u.map : u.area); });
  return out;
}
function announce(text){
  if(VIEW && VIEW.live) VIEW.live.textContent = text;
  else if(HQV.engine && typeof HQV.engine.say === "function"){ try { HQV.engine.say(text); } catch(e){} }
}

/* ---------- the public hook ---------- */
HQV.story = {
  note: function(ev, n){
    if(NOTES.indexOf(ev) < 0) return;
    n = n == null ? 1 : Math.floor(Number(n));
    if(!isFinite(n) || n < 1) return;
    var st = ssave();
    st.notes[ev] = Math.min(1e6, (st.notes[ev]|0) + Math.min(n, 1000));
    api.persist();
    advance();
    if(VIEW) VIEW.render();
  },
  unlocked: unlockedList,
  chapter: function(){ var i = Math.min(doneCount(), CHAPTERS.length - 1), c = CHAPTERS[i];
    return {index:i, id:c.id, name:c.name, done:doneCount() >= CHAPTERS.length}; }
};
HQV.storyMaps = function(){ return JSON.parse(JSON.stringify(STORY_MAPS)); };

/* ---------- deep links ---------- */
function goArea(go){
  if(!go) return;
  if(go.game){
    if(go.view === "workshop" && HQV.makers && HQV.makers.kart && typeof window.setView === "function" && hasTab("workshop")){ window.setView("workshop"); return; }
    api.open(go.game); return;
  }
  if(go.view && typeof window.setView === "function") window.setView(go.view);
}
function hasTab(v){ var b = document.querySelector('[data-view="'+v+'"]'); return !!(b && !b.hidden); }

/* ---------- the buddy: your first caught creature, painted the HQ way ---------- */
function buddyEl(who){
  var box = mk("div", "vg-face"); box.setAttribute("aria-hidden", "true");
  if(who === CLAUDE && DEX && Array.isArray(DEX.species)){
    var sp = null; DEX.species.forEach(function(s){ if(!sp && s && s.caught) sp = s; });
    if(sp && typeof window.paintCreature === "function"){
      try { window.paintCreature(box, {species:sp.species|0, stage:Math.max(0, Math.min(4, sp.maxStage|0)), shiny:!!sp.shiny, _noFloor:true}, 56, false); return box; } catch(e){}
    }
  }
  box.textContent = who === PROF ? "🧑‍🏫" : "✨";
  box.style.fontSize = "40px"; box.style.textAlign = "center"; box.style.lineHeight = "56px";
  return box;
}

/* ---------- the page ---------- */
function mount(el){
  var root = mk("div", "vg-goal"); root.style.maxWidth = "640px";
  var live = mk("p", "vg-muted"); live.setAttribute("role", "status"); live.setAttribute("aria-live", "polite");
  live.style.position = "absolute"; live.style.left = "-9999px";
  el.appendChild(root); el.appendChild(live);
  var sel = Math.min(doneCount(), CHAPTERS.length - 1), line = 0, timer = null;
  VIEW = {live:live, render:render};

  function stopType(){ if(timer){ clearInterval(timer); timer = null; } }
  function typeInto(node, text){
    stopType();
    if(api.calm()){ node.textContent = text; return; }
    var i = 0; node.textContent = "";
    timer = setInterval(function(){
      i += 2; node.textContent = text.slice(0, i);
      if(i >= text.length) stopType();
    }, 28);
  }
  function dialogue(c, open){
    var lines = c.intro.slice(); if(ssave().done[c.id]) lines.push(c.outro);
    if(!open) lines = [[PROF, "This chapter opens once the one before it is complete."]];
    if(line >= lines.length) line = 0;
    var who = lines[line][0], text = lines[line][1];
    var box = mk("div", "vg-npc"); box.appendChild(buddyEl(who));
    var info = mk("div", "vg-npc-info"); info.appendChild(mk("b", null, who));
    var say = mk("p", "vg-say"); say.setAttribute("aria-label", who+": "+text); info.appendChild(say);
    var row = mk("div", "vg-row");
    if(lines.length > 1){
      row.appendChild(btn(line + 1 < lines.length ? "Next ▸" : "From the top ↺", "ghost", function(){
        if(timer){ stopType(); say.textContent = text; return; }   // first press finishes the line
        line = (line + 1) % lines.length; render();
      }));
      row.appendChild(mk("span", "vg-muted", (line + 1)+" / "+lines.length));
    }
    info.appendChild(row); box.appendChild(info);
    typeInto(say, text);
    return box;
  }
  function goalRow(g, open){
    var s = goalState(g), wrap = mk("li", "vg-need"+(s.met && open ? " met" : ""));
    wrap.style.display = "flex"; wrap.style.flexDirection = "column"; wrap.style.alignItems = "stretch"; wrap.style.gap = "4px";
    var top = mk("div", "vg-row");
    top.appendChild(mk("span", null, s.met && open ? "✅" : "⬜"));
    top.appendChild(mk("span", null, g.label));
    top.appendChild(mk("span", "vg-muted", s.v+" / "+g.n));
    wrap.appendChild(top);
    var meter = mk("div", "vg-meter"), fill = mk("i");
    fill.style.width = Math.round(100 * s.v / g.n)+"%"; meter.appendChild(fill);
    meter.setAttribute("role", "progressbar"); meter.setAttribute("aria-label", g.label);
    meter.setAttribute("aria-valuemin", "0"); meter.setAttribute("aria-valuemax", String(g.n)); meter.setAttribute("aria-valuenow", String(s.v));
    wrap.appendChild(meter);
    if(g.k === "catch" && !DEX) wrap.appendChild(mk("span", "vg-muted", "Reading your Pokédex…"));
    return wrap;
  }
  function mapCard(i, got){
    var doc = STORY_MAPS[i], kind = doc.kind, maker = HQV.makers && HQV.makers[kind];
    var card = mk("div", "vg-npc"); card.style.flexDirection = "column";
    card.appendChild(mk("b", null, (kind === "kart" ? "🏎️ " : "🏝️ ")+doc.name));
    var row = mk("div", "vg-row");
    if(!got){ row.appendChild(mk("span", "vg-muted", "Finish this chapter to unlock it.")); }
    else if(!maker){ row.appendChild(mk("span", "vg-muted", "needs the "+(kind === "kart" ? "Track" : "Level")+" editor")); card.setAttribute("aria-disabled", "true"); card.style.opacity = ".6"; }
    else {
      var copy = function(){ return JSON.parse(JSON.stringify(doc)); };
      row.appendChild(btn(kind === "kart" ? "Test-drive" : "Test-run", "primary", function(){ try { maker.play(copy(), {}); } catch(e){} }));
      if(api.inArenaRoom()) row.appendChild(btn("Race in room", null, function(){ try { maker.play(copy(), {room:true}); } catch(e){} }));
      row.appendChild(btn("Open in editor", "ghost", function(){ try { maker.edit(copy()); } catch(e){} }));
    }
    card.appendChild(row);
    return card;
  }
  function unlockRow(u, got){
    if(u.map != null) return mapCard(u.map, got);
    var card = mk("div", "vg-npc"); card.style.alignItems = "center";
    card.appendChild(mk("span", "vg-card-ic", u.icon));
    var t = mk("div", "vg-npc-info"); t.appendChild(mk("b", null, u.name));
    t.appendChild(mk("span", "vg-muted", got ? u.text : "Finish this chapter to open this page."));
    card.appendChild(t);
    if(got) card.appendChild(btn("Go", "primary", function(){ goArea(u.go); }));
    return card;
  }
  function render(){
    stopType();
    sig = signature();
    root.textContent = "";
    var n = doneCount();
    var tabs = mk("div", "vg-row vg-tabs"); tabs.setAttribute("aria-label", "Chapters");
    CHAPTERS.forEach(function(c, i){
      var open = i <= n, b = btn((ssave().done[c.id] ? "✓ " : open ? c.icon+" " : "🔒 ")+(i + 1), i === sel ? "primary" : "", function(){ sel = i; line = 0; render(); });
      b.setAttribute("aria-pressed", i === sel ? "true" : "false");
      b.setAttribute("aria-label", "Chapter "+(i + 1)+": "+c.name+(ssave().done[c.id] ? ", complete" : open ? "" : ", locked"));
      tabs.appendChild(b);
    });
    root.appendChild(tabs);
    var c = CHAPTERS[sel], open = sel <= n, done = !!ssave().done[c.id];
    root.appendChild(mk("h3", null, "Chapter "+(sel + 1)+" · "+c.name+(done ? " ✓" : "")));
    root.appendChild(dialogue(c, open));
    root.appendChild(mk("h4", "vg-section", "Goals"));
    var ul = mk("ul", "vg-needs"); ul.style.listStyle = "none"; ul.style.padding = "0"; ul.style.flexDirection = "column";
    c.goals.forEach(function(g){ ul.appendChild(goalRow(g, open)); });
    root.appendChild(ul);
    root.appendChild(mk("h4", "vg-section", done ? "Unlocked" : "Unlocks"));
    c.unlocks.forEach(function(u){ root.appendChild(unlockRow(u, done)); });
    if(n >= CHAPTERS.length) root.appendChild(mk("p", "vg-msg", "👑 Every chapter told. Thanks for playing the story."));
    root.appendChild(mk("p", "vg-muted", "The story only reads your progress. Nothing else in HQ waits on it, and it never sends anything."));
  }
  // Re-read the inputs; repaint only when something you can see changed (keeps the typewriter still).
  var sig = "";
  function signature(){ return doneCount()+"|"+!!DEX+"|"+CHAPTERS[sel].goals.map(goalValue).join(","); }
  function refresh(){
    Promise.all([loadDex(), loadMaps()]).then(function(){
      if(!VIEW || VIEW.render !== render) return;
      if(advance()) sel = Math.min(doneCount(), CHAPTERS.length - 1);
      var now = signature(); if(now !== sig) render();
    });
  }
  render();
  refresh();
  VIEW.refresh = refresh;
  VIEW.stop = stopType;
}

HQV.register({
  id:"story", name:"Story", icon:"📖", desc:"Chapters through HQ and the Valley, unlocked as you level",
  mount:function(el){ mount(el); },
  unmount:function(){ if(VIEW && VIEW.stop) VIEW.stop(); VIEW = null; },
  pause:function(){}, resume:function(){ if(VIEW && VIEW.refresh) VIEW.refresh(); },
  badge:function(){ var n = doneCount(); return n >= CHAPTERS.length ? "👑" : "Ch "+(n + 1)+"/"+CHAPTERS.length; },
  onTick:function(){ if(VIEW && VIEW.refresh) VIEW.refresh(); }
});
HQV.storyDebug = {chapters:CHAPTERS, notes:NOTES, advance:advance};
})();
