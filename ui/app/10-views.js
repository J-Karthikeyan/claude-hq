/* ================= v3 additions ================= */

/* ---- view switcher ---- */
// Arena state must exist before setView(VIEW) runs at startup (it calls arenaCloseSocket).
var ARENA_ROOM_RE = /^r_[A-Za-z0-9_-]{22}$/;
var ARENA = {window:"season", status:null, sock:null, retry:null, you:null, chat:[], chatSeen:{},
             paired:false, opening:false, gen:0, fails:0, peerStatus:{}, unread:0, unreadTagged:false,
             nudgeCount:{},
             roomId:(function(){try{var r=localStorage.getItem("hq_arena_room");return(r&&/^r_[A-Za-z0-9_-]{22}$/.test(r))?r:"lobby";}catch(e){return"lobby";}})(),
             roomName:"Lobby",roomRole:null,rooms:null,lobbyOnline:null,limits:null,
             roomsOk:null,roomsLoading:null,roomConfirmed:{},chatBy:{},roomClose:0,
             roomAnnounce:false,roomsTimer:null,roomJoinOpen:null};
// Lobby options (see "Arena lobby: presence and live status"): staying in the lobby is on unless turned off,
// sharing your live status is off unless turned on.
var ARENA_STAY = (function(){ try { return localStorage.getItem("hq_arena_stay") !== "0"; } catch(e){ return true; } })();
var ARENA_SHARE_STATUS = (function(){ try { return localStorage.getItem("hq_arena_status") === "1"; } catch(e){ return false; } })();
// Voice state, also needed by the first setView() (which keeps the lobby socket open while in voice).
var VCHAN = {on:false, peer:null, stream:null, muted:false, cam:false, screen:false, vidStream:null, vidTile:null, peers:{}, roomId:null, roster:{}, ctx:null, me:null, meter:null};
var VIEW = (function(){ var v=localStorage.getItem("hq_view"); return (v==="analytics"||v==="pokedex"||v==="gym"||v==="quests"||v==="arena"||v==="store"||v==="cali"||v==="valley"||v==="workshop"||v==="compete")?v:"live"; })();
var VIEW_TITLES = {
  live:["Live","Every Claude session, right now"], analytics:["Analytics","Usage, cost and rhythm over time"],
  pokedex:["Pokédex","Your creature collection"], gym:["Gym","Active sessions scored as a team"],
  quests:["Quests","Daily, weekly and collection goals"], arena:["Arena","Friends, standings and voice"],
  village:["Village","Your base & army, built from your Claude activity"],
  store:["Store","Snacks for your creatures, paid in Poke Coins"],
  cali:["Cali Tuesdays","Taco Tuesday at California Burrito: seat friends, plate their food, check out"],
  valley:["Valley","Minigames to play while your tabs work"],
  hq:["HQ","Mission Control: your sessions as crew at their desks"],
  music:["Music","What everyone's playing, and rooms to listen together"],
  workshop:["Workshop","Make tracks, levels and maps; share them and race what others made"],
  compete:["Compete","Weekly cups, the season podium and more"]
};
// The Clash of Clans pack renames the "Pokédex" collection to "Troops" (nav tab + title).
function syncPackLabels(){
  var vil = (typeof cfg==="function") && cfg().creaturePack==="village";
  var label = vil ? "Troops" : "Pokédex";
  VIEW_TITLES.pokedex = [label, vil ? "Your Clash of Clans troop collection" : "Your creature collection"];
  var tl=document.querySelector('.viewtab[data-view="pokedex"] .vt-l'); if(tl) tl.textContent=label;
  if(typeof VIEW!=="undefined" && VIEW==="pokedex" && $("viewTitle")){
    $("viewTitle").textContent=VIEW_TITLES.pokedex[0]; $("viewSub").textContent=VIEW_TITLES.pokedex[1];
  }
}
function setView(v){
  VIEW = v; localStorage.setItem("hq_view", v);
  if(typeof voiceDockRender === "function") voiceDockRender();
  syncPackLabels();
  var vt = VIEW_TITLES[v]||VIEW_TITLES.live;
  if($("viewTitle")){ $("viewTitle").textContent = vt[0]; $("viewSub").textContent = vt[1]; }
  $("liveView").classList.toggle("hidden", v!=="live");
  $("analyticsView").classList.toggle("hidden", v!=="analytics");
  var pv=$("pokedexView"); if(pv) pv.classList.toggle("hidden", v!=="pokedex");
  var gv=$("gymView"); if(gv) gv.classList.toggle("hidden", v!=="gym");
  var qv=$("questsView"); if(qv) qv.classList.toggle("hidden", v!=="quests");
  var vv=$("villageView"); if(vv) vv.classList.toggle("hidden", v!=="village");
  var av=$("arenaView"); if(av) av.classList.toggle("hidden", v!=="arena");
  var stv=$("storeView"); if(stv) stv.classList.toggle("hidden", v!=="store");
  var cdv=$("caliView"); if(cdv) cdv.classList.toggle("hidden", v!=="cali");
  var vlv=$("valleyView"); if(vlv) vlv.classList.toggle("hidden", v!=="valley");
  var hqv=$("hqView"); if(hqv) hqv.classList.toggle("hidden", v!=="hq");
  var muv=$("musicView"); if(muv) muv.classList.toggle("hidden", v!=="music");
  var wsv=$("workshopView"); if(wsv) wsv.classList.toggle("hidden", v!=="workshop");
  var cpv=$("competeView"); if(cpv) cpv.classList.toggle("hidden", v!=="compete");
  Array.prototype.forEach.call(document.querySelectorAll(".viewtab"),function(t){
    var on = t.getAttribute("data-view")===v;
    t.classList.toggle("active", on); t.setAttribute("aria-selected", on?"true":"false");
    if(on) t.setAttribute("aria-current","page"); else t.removeAttribute("aria-current");
  });
  if(v==="analytics"){ loadHistory(); loadInsights(); }
  if(v==="pokedex") loadPokedex();
  if(v==="gym") renderGym();
  if(v==="quests") loadQuests();
  if(v==="village") renderVillage();
  if(v==="store") svEnter(); else svLeave();
  if(v==="cali") cdEnter(); else cdLeave();
  if(v==="valley") valleyEnter(); else valleyLeave();
  if(v==="music") musicEnter(); else musicLeave();
  if(v==="workshop"){ if(typeof workshopEnter==="function") workshopEnter(); } else if(typeof workshopLeave==="function") workshopLeave();
  if(v==="compete"){ if(typeof competeEnter==="function") competeEnter(); } else if(typeof competeLeave==="function") competeLeave();
  if(typeof hqViewChanged==="function") hqViewChanged(v);
  valleyPillSync();
  // The lobby socket stays open on other views while you stay in the lobby, or while you're in voice (it carries it).
  if(v==="arena"){ arenaUnreadClear(); ARENA.nudgeCount={}; loadArena(); } else { arenaRoomFormsClear(); if(!VCHAN.on && !ARENA_STAY && !muWantsSocket()) arenaCloseSocket(); }
}
Array.prototype.forEach.call(document.querySelectorAll(".viewtab"),function(t){
  t.addEventListener("click",function(){ setView(t.getAttribute("data-view")); });
});

/* ---- Valley minigames: loaded from /games/ on first use (keeps this file from growing) ---- */
var VALLEY_FILES = ["core","engine","fishart","fishing","garden","bundles","mines","pokedata","pokebattle","battle","puzzle","town","festival","arcade","multi","golf","input","kart","platformer","fps","strike","typerace","trackedit","leveledit","mapedit","td","bowling","story"];
var VALLEY_LOAD = null;
function valleyLoad(){
  if(VALLEY_LOAD) return VALLEY_LOAD;
  if(!document.getElementById("valleyCss")){
    var l=document.createElement("link"); l.id="valleyCss"; l.rel="stylesheet"; l.href="/games/games.css"; document.head.appendChild(l);
  }
  // Scripts run in order: core first, then each game registers with it.
  VALLEY_LOAD = VALLEY_FILES.reduce(function(p, name){
    return p.then(function(){ return new Promise(function(res, rej){
      var sc=document.createElement("script"); sc.src="/games/"+name+".js"; sc.async=false;
      sc.onload=res; sc.onerror=function(){ rej(new Error(name+".js failed to load")); };
      document.head.appendChild(sc);
    }); });
  }, Promise.resolve()).catch(function(e){ VALLEY_LOAD=null; throw e; });
  return VALLEY_LOAD;
}
function valleyEnter(){
  var root=$("valleyView"); if(!root) return;
  if(!window.HQV || !window.HQV.enter) root.textContent="Loading the Valley…";
  valleyLoad().then(function(){ if(VIEW==="valley" && window.HQV) window.HQV.enter(root); })
    .catch(function(e){ root.textContent="The Valley couldn’t load: "+e.message; });
}
function valleyLeave(){ if(window.HQV && window.HQV.leave) window.HQV.leave(); }
// Waiting Room: while a tab works and you're on another view, offer a game.
function valleyPillSync(){
  var working=((STATE&&STATE.sessions)||[]).filter(function(s){ return s.status==="working"; }).length;
  var show = working>0 && VIEW!=="valley" && VIEW!=="store" && !cfgValleyOff();
  var p=$("valleyPill");
  if(!show){ if(p) p.remove(); return; }
  if(!p){
    p=document.createElement("button"); p.id="valleyPill"; p.type="button";
    p.addEventListener("click",function(){ setView("valley"); });
    document.body.appendChild(p);
    if(!document.getElementById("valleyCss")){ var l=document.createElement("link"); l.id="valleyCss"; l.rel="stylesheet"; l.href="/games/games.css"; document.head.appendChild(l); }
  }
  var txt="🎣 Play while you wait · "+working+" working";
  if(p.textContent!==txt) p.textContent=txt;
}
// Valley multiplayer traffic from the Arena room. Join notices and invites work even
// before the Valley view has ever been opened; everything else goes to games/multi.js.
var VALLEY_GAME_NAMES = {pond:"Fishing Pond", race:"Puzzle Race", duel:"Creature Duel", mines:"Co-op Mines", farm:"Shared Farm", golf:"Mini Golf", kart:"Kart Racing", plat:"Platformer Rush", fps:"Blaster Arena", strike:"Strikezone", type:"Code Typing Race", td:"Tower Defense", bowl:"Bowling"};
function valleyOnGame(m){
  if(m && m.g==="party"){ if(typeof partyOnGame==="function") partyOnGame(m); return; }
  if(m && m.g==="hq"){ if(typeof hqNetOnGame==="function") hqNetOnGame(m); return; }      // HQ presence on the shared socket
  if(!m || typeof m.g!=="string" || !VALLEY_GAME_NAMES[m.g]) return;
  var you = ARENA.you && ARENA.you.userId;
  if(m.ev==="invite"){ valleyInvite(m); return; }
  if(m.ev==="lobby" && m.joined && m.joined.userId!==you){
    var who = String(m.joined.displayName||m.joined.handle||"Someone").slice(0,40);
    toast("🎮 "+who+" joined "+VALLEY_GAME_NAMES[m.g], "level");
  }
  if(typeof HQV!=="undefined" && HQV.onGame){ try{ HQV.onGame(m); }catch(e){} }
}
function valleyInvite(m){
  var from = (m.from && typeof m.from==="object") ? m.from : {};
  var who = String(from.displayName||from.handle||"A friend").slice(0,40), game = VALLEY_GAME_NAMES[m.g];
  var room = (typeof m.room==="string" && (m.room==="lobby" || ARENA_ROOM_RE.test(m.room))) ? m.room : null;
  toast("🎮 "+who+" invited you to "+game, "level");
  try{ if(typeof Notification!=="undefined" && Notification.permission==="granted" && document.hidden) new Notification("Claude HQ", {body: who+" invited you to "+game}); }catch(e){}
  var old=$("valleyInvite"); if(old) old.remove();
  var card=document.createElement("div"); card.id="valleyInvite"; card.setAttribute("role","dialog"); card.setAttribute("aria-label","Game invite");
  var t=document.createElement("b"); t.textContent=who+" invited you to "+game; card.appendChild(t);
  var row=document.createElement("div"); row.className="vg-row";
  var join=document.createElement("button"); join.type="button"; join.className="hbtn primary"; join.textContent="Join";
  join.addEventListener("click",function(){
    card.remove();
    if(room && room!==ARENA.roomId) arenaGoRoom(room);
    setView("valley");
    valleyLoad().then(function(){ if(typeof HQV!=="undefined" && HQV.api) HQV.api.open("mp-"+m.g); });
  });
  var later=document.createElement("button"); later.type="button"; later.className="hbtn ghost"; later.textContent="Later";
  later.addEventListener("click",function(){ card.remove(); });
  row.appendChild(join); row.appendChild(later); card.appendChild(row);
  document.body.appendChild(card);
  if(!document.getElementById("valleyCss")){ var l=document.createElement("link"); l.id="valleyCss"; l.rel="stylesheet"; l.href="/games/games.css"; document.head.appendChild(l); }
  join.focus();
  setTimeout(function(){ if(card.parentNode) card.remove(); }, 60000);
}
function cfgValleyOff(){ try{ return localStorage.getItem("hq_valley_pill")==="0"; }catch(e){ return false; } }
setView(VIEW);
primeHighWater();   // seed New Game+ evolution floor from the backend Pokédex on startup

/* ---- live streaming (SSE) with poll fallback ---- */
var es=null, pollTimer=null, sseRetry=null;
function setLive(on){
  var e=$("liveState"); if(!e) return;
  e.classList.toggle("on", on);
  var lt=e.querySelector(".lt"); if(lt) lt.textContent = on?"live":"polling";
}
// Every open tab reloads once the server restarts (after an Update, a launchd restart or a
// crash): the new process has a new CSRF token and may serve new code, so an old tab would
// otherwise keep failing its POSTs. The boot id changes on every start.
var SERVER_BOOT=null;
function checkBoot(d){
  if(!d || !d.boot) return;
  if(SERVER_BOOT && d.boot!==SERVER_BOOT){
    var ae=document.activeElement, typing=ae && (ae.tagName==="TEXTAREA" || (ae.tagName==="INPUT" && ae.type!=="checkbox" && ae.type!=="range"));
    if(typing){ toast("⬇ Claude HQ restarted. Reloading when you finish typing…","level"); ae.addEventListener("blur",function(){ location.reload(); },{once:true}); }
    else location.reload();
    SERVER_BOOT=d.boot; return;
  }
  SERVER_BOOT=d.boot;
}
function applyPayload(d){ checkBoot(d); STATE=d; render(); pulse(); if(typeof hqOnState==="function") hqOnState(d); if(typeof focusOnState==="function") focusOnState(d); }
function startPoll(){ if(pollTimer) return; load(); pollTimer=setInterval(load, CONFIG.refreshMs||5000); }
function stopPoll(){ if(pollTimer){ clearInterval(pollTimer); pollTimer=null; } }
// re-arm the poll loop with the current cadence (after a settings change)
function restartPoll(){ if(pollTimer){ clearInterval(pollTimer); pollTimer=null; startPoll(); } }
function scheduleSseRetry(){
  if(sseRetry) return;
  sseRetry=setTimeout(function(){ sseRetry=null; startStream(); }, 15000);
}
function startStream(){
  if(!window.EventSource){ startPoll(); return; }
  try{ es=new EventSource('/api/stream'); }
  catch(err){ startPoll(); return; }
  es.onopen=function(){ setLive(true); stopPoll(); };
  es.onmessage=function(ev){
    try{ var d=JSON.parse(ev.data); if(d && d.sessions){ applyPayload(d); setLive(true); stopPoll(); } }catch(err){}
  };
  es.onerror=function(){
    setLive(false);
    try{ if(es) es.close(); }catch(err){}
    es=null;
    startPoll();          // fall back to 5s polling
    scheduleSseRetry();   // periodically retry SSE
  };
}
// Battery/CPU: when the tab is hidden, stop the fetch loops entirely; on return, resume and
// refresh immediately so the view is never stale. (The orb canvas already pauses on hidden.)
document.addEventListener("visibilitychange", function(){
  if(document.hidden){ stopPoll(); return; }
  try{
    if(RENDER_DIRTY && STATE) render();             // paint what arrived while hidden, once
    if(es){ load(); } else { startPoll(); }          // fleet refresh
    if(VIEW==="analytics"){ loadHistory(); loadInsights(); }
    else if(VIEW==="pokedex"){ loadPokedex(); }
    else if(VIEW==="quests"){ loadQuests(); }
    else if(VIEW==="arena" && typeof arenaRoomsLoad==="function"){ arenaRoomsLoad(); }
  }catch(e){ hqErr(e,"visibility-resume"); }
});

/* ---- fleet feed ---- */
function feedActor(f){
  var who = (f.actor||f.who||f.kind||"").toLowerCase();
  if(who==="you"||who==="user") return {label:"You", cls:"you"};
  if(who==="tool"||who==="tool_use") return {label:(f.tool||f.name||"tool"), cls:"tool"};
  return {label: (f.source==="cursor"?"Cursor":"Claude"), cls:"claude"};
}
// Per-section render signatures: render() runs on every SSE tick, so each section below rebuilds
// its DOM only when its own inputs changed (a rebuild restarts animated sprites and costs layout).
// paintEnvSig covers what every painted sprite depends on besides the creature itself.
function paintEnvSig(){
  var dark=!!(window.matchMedia && window.matchMedia("(prefers-color-scheme: dark)").matches);
  return [cfg().creaturePack, document.documentElement.getAttribute("data-theme")||"", dark?1:0].join(",");
}
// What a painted creature looks like: species, (floored) stage, shiny, evolution/mega choice, energy.
function creatureSigKey(cr){
  var c=cr||{}, ch=EVO_CHOICE[c._sid]||{}, f=fzOf(c);
  return [c.species, creatureStage(c), c.shiny?1:0, ch.branchDex||"", ch.megaSlug||"", f?f.state:"", c.emoji||""].join(",");
}
var FEED_SIG=null;
function feedSig(feed, byId){
  return paintEnvSig()+"||"+(feed||[]).length+"||"+JSON.stringify((feed||[]).slice(0,25).map(function(f){
    var live=byId[f.sessionId||f.id];
    return [f.sessionId||f.id||"", (live&&live.title)||f.title||f.session||"", feedActor(f).label, f.text||f.detail||f.summary||"",
            creatureSigKey((live&&live.creature)||f.creature||{emoji:f.emoji}), f.t||f.ts||f.time||f.timestamp||""];
  }));
}
function renderFeed(feed){
  var wrap=$("feedList"); if(!wrap) return;
  feed = feed||[];
  var byId={}; ((STATE&&STATE.sessions)||[]).forEach(function(s){ byId[s.sessionId]=s; });
  var sig=feedSig(feed, byId);
  if(sig===FEED_SIG && wrap.firstChild){
    // Same events: only the relative times move on.
    Array.prototype.forEach.call(wrap.querySelectorAll(".feed-time"),function(tm){ setText(tm, relTime(tm._t)); });
    return;
  }
  FEED_SIG=sig;
  if(!feed.length){ wrap.innerHTML='<div class="feed-empty">No recent fleet activity.</div>'; $("feedSub").textContent=""; return; }
  wrap.innerHTML="";
  feed.slice(0,25).forEach(function(f){
    var row=el("div","feed-row");
    var ic=el("div","feed-ic");
    // use the live session's REAL creature (species/stage) so the feed shows the
    // correct evolved form — the backend feed events don't carry the creature.
    var live=byId[f.sessionId||f.id];
    var cr=(live&&live.creature) || f.creature || {emoji:f.emoji};
    paintCreature(ic, cr, 30);
    row.appendChild(ic);
    var body=el("div","feed-body");
    var tt=el("div","feed-title"); tt.textContent = (live&&live.title) || f.title || f.session || "Session"; body.appendChild(tt);
    var act=feedActor(f);
    var what=el("div","feed-what");
    what.innerHTML = '<b>'+esc(act.label)+'</b> '+esc(f.text||f.detail||f.summary||"");
    body.appendChild(what);
    row.appendChild(body);
    var tm=el("div","feed-time");
    tm._t = f.t||f.ts||f.time||f.timestamp;
    tm.textContent = relTime(tm._t);
    row.appendChild(tm);
    var sid=f.sessionId||f.id;
    row.addEventListener("click",function(){ if(sid) openSession(sid,{title:f.title,creature:f.creature}); });
    wrap.appendChild(row);
  });
  $("feedSub").textContent = feed.length>25 ? "latest 25" : (feed.length+" event"+(feed.length===1?"":"s"));
}

/* ---- open a session drawer by id (feed / search / hall of fame) ---- */
function openSession(id, meta){
  meta = meta||{};
  openDrawer({
    sessionId:id, id:id,
    title: meta.title||"Session",
    folder: meta.folder||"",
    status: meta.status||"",
    creature: meta.creature||null,
    links:[], tokens:null
  });
}

/* ---- all-history search overlay ---- */
var SEARCHOV_OPEN=false;
function highlight(text, q){
  var safe = esc(text||"");
  if(!q) return safe;
  try{
    var re = new RegExp("("+q.replace(/[.*+?^${}()|[\]\\]/g,"\\$&")+")","ig");
    return safe.replace(re,"<mark>$1</mark>");
  }catch(e){ return safe; }
}
function openSearchOv(){ rememberOpener(); $("searchOv").classList.add("open"); SEARCHOV_OPEN=true; $("searchOvX").focus(); }
function closeSearchOv(){ $("searchOv").classList.remove("open"); SEARCHOV_OPEN=false; restoreOpener(); }
function runSearch(q){
  if(!q){ return; }
  openSearchOv();
  $("searchOvTitle").textContent = 'Searching “'+q+'”…';
  $("searchOvResults").innerHTML = '<div class="searchov-empty">Searching…</div>';
  fetch('/api/search?q='+encodeURIComponent(q),{cache:"no-store"})
    .then(function(r){ if(!r.ok) throw new Error(r.status); return r.json(); })
    .then(function(d){ renderSearchResults(q, d); })
    .catch(function(){ $("searchOvResults").innerHTML='<div class="searchov-empty">Search failed. Try again.</div>'; });
}
function renderSearchResults(q, d){
  var results = (d && (d.results||d.hits||d.sessions)) || (Array.isArray(d)?d:[]);
  var box=$("searchOvResults");
  $("searchOvTitle").textContent = results.length+" result"+(results.length===1?"":"s")+' for “'+q+'”';
  if(!results.length){ box.innerHTML='<div class="searchov-empty">No matches in your history.</div>'; return; }
  box.innerHTML="";
  results.forEach(function(h){
    var hit=el("div","hit");
    var top=el("div","hit-top");
    var t=el("div","hit-title"); t.textContent = h.title||"Untitled session"; top.appendChild(t);
    var isLive = (h.live===true) || (h.tag==="live") || (h.status && h.status!=="archived" && h.live!==false && h.archived===false);
    var tag=el("span","hit-tag "+(isLive?"live":"archived")); tag.textContent = isLive?"live":"archived"; top.appendChild(tag);
    hit.appendChild(top);
    var meta=el("div","hit-meta");
    var when = h.date||h.ts||h.time||h.lastActivity;
    meta.textContent = (h.source==="cursor"?"Cursor":"Claude") + " · " + prettyFolder(h.folder||h.slug||"") + " · " + relTime(when);
    hit.appendChild(meta);
    var snip = h.snippet||h.snip||h.text||"";
    if(snip){ var s=el("div","hit-snip"); s.innerHTML = highlight(snip, q); hit.appendChild(s); }
    var sid=h.sessionId||h.id;
    hit.addEventListener("click",function(){ if(sid){ closeSearchOv(); openSession(sid,{title:h.title,folder:h.folder}); } });
    box.appendChild(hit);
  });
}
$("searchOvX").addEventListener("click",closeSearchOv);
$("searchOv").addEventListener("click",function(e){ if(e.target===this) closeSearchOv(); });

