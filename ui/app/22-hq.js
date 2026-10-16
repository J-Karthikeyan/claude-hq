/* ================= HQ 2.0: the 3D HQ view, and the switch to and from the classic dashboard ================= */
// The scene itself lives in games/hq3d.js (loaded with games/engine.js the first time HQ opens, so the
// page stays light for anyone who never opens it). This file feeds it the live payload, lists the crew
// beside it (also the keyboard and screen-reader way in), and remembers which mode you use:
// localStorage hq_mode = "3d" | "classic". With Calm mode or reduced motion, the classic dashboard
// is where HQ opens unless you switch.
var HQ3D = {inst:null, load:null, filter:null, lastClassic:"live", failed:false};
function hqModePref(){ try { return localStorage.getItem("hq_mode"); } catch(e){ return null; } }
function hqModeSave(m){ try { localStorage.setItem("hq_mode", m); } catch(e){} }
function hqCalm(){ return document.documentElement.classList.contains("hq-calm") ||
  !!(window.matchMedia && window.matchMedia("(prefers-reduced-motion: reduce)").matches); }
function hqWebGL(){ try { return !!document.createElement("canvas").getContext("webgl2"); } catch(e){ return false; } }

function hqLoadScripts(){
  if(HQ3D.load) return HQ3D.load;
  // The engine (shared with the Valley's 3D games), the HQ host + Mission Control, then the
  // other floors of the building: the Lobby and the Base outside.
  var files = ((window.HQV && window.HQV.engine) ? [] : ["engine"]).concat((window.HQV && window.HQV.avatar3d) ? [] : ["avatar3d"], ["hq3d","hqlobby","hqbase","hqskate","hqcity"]);
  HQ3D.load = files.reduce(function(p, name){
    return p.then(function(){ return new Promise(function(res, rej){
      var sc=document.createElement("script"); sc.src="/games/"+name+".js"; sc.async=false;
      sc.onload=res; sc.onerror=function(){ rej(new Error(name+".js failed to load")); };
      document.head.appendChild(sc);
    }); });
  }, Promise.resolve()).catch(function(e){ HQ3D.load=null; throw e; });
  return HQ3D.load;
}
function hqApi(){
  return {
    name: ((typeof cfg==="function" && cfg().trainerName) || "You"),
    // your 3D character (ui/app/30-character.js), or null for the hashed look
    character: function(){ return typeof charSpec==="function" ? charSpec() : null; },
    openSession: hqOpen,
    go: function(v){ hqModeSave("classic"); setView(v); },
    // a tab's [name, subtitle], for its building in the city and round your Base
    viewTitle: function(v){ return (typeof VIEW_TITLES!=="undefined" && VIEW_TITLES[v]) || null; },
    onFilter: function(p){ HQ3D.filter = p || null; hqRenderCrew(); },
    // your HQ level: the Arena's (sessions + games) when paired, else the season level the page shows
    level: function(){ var a=(typeof progLevel==="function") && progLevel(); if(a) return a; var s=(STATE && STATE.season) || {}; return Math.max(1, (s.level|0) || 1); },
    look: function(){ return Object.assign({}, hqLook(), {decor: (window.HQ_MYCOS || {}).decor,
                                                          crew: (typeof crewBanner==="function") ? crewBanner() : null}); },
    startWorld: (function(){ try { var w=localStorage.getItem("hq_world"); return w==="base"||w==="lobby"||w==="mission" ? w : "base"; } catch(e){ return "base"; } })(),
    // HQ 2.1 Arena City: outside is the street of open HQs unless you are private (HQ closed)
    outside: function(){ return hqCityOn() ? "city" : "base"; },
    city: function(){ return HQ_CITY.list; },
    cityReturn: function(){ return HQ_CITY.back; },
    visit: function(uid){ hqVisit(uid, {from:"city"}); },
    frame: function(){ return (window.HQ_MYCOS || {}).frame; },
    // HQ 2.5 prestige stars: extra floors on YOUR building only (visits pass no prestige)
    prestige: function(){ return typeof prestigeStars==="function" ? prestigeStars() : 0; },
    onWorld: function(name){ HQ3D.world = name; hqRenderWhere(); },
    announce: function(t){ if(typeof announce==="function") announce(t); },
    // a skate combo banked in the city park (games/hq3d.js): maybe a new best for the week board
    skateCombo: function(c){ hqSkateCombo(c); }
  };
}
// How your building looks (HQ customisation; saved on the Arena in a later step): paint, accent, sign.
function hqLook(){ try { var j=JSON.parse(localStorage.getItem("hq_look")||"{}"); return (j && typeof j==="object") ? j : {}; } catch(e){ return {}; } }
var HQ_WHERE = [["base","Base"],["lobby","Lobby"],["mission","Mission Control"]];
// Jump to City: straight onto Arena City's street from anywhere (opens the 3D HQ first if needed).
// You can walk the street while your HQ is private too; your building only stands on it when it is open.
function hqJumpCity(){
  if(HQ3D.visit){ toast("Go back home first, then jump to the city","level"); return; }
  if(!(window.ARENA && ARENA.paired)){ toast("Pair with the Arena (Arena tab) to visit Arena City","ach"); return; }
  var go=function(){ if(HQ3D.inst && HQ3D.inst.goWorld){ HQ3D.inst.goWorld("city"); hqCityLoad(true); } };
  if(VIEW!=="hq"){ hqModeSave("3d"); setView("hq"); }
  if(HQ3D.inst) go(); else hqLoadScripts().then(function(){ setTimeout(go, 300); }).catch(function(){});
}
function hqRenderWhere(){
  hqSkateShow();
  var box=$("hqWhere"); if(!box) return;
  box.textContent="";
  var city=!HQ3D.visit && hqCityOn();
  if(!HQ3D.visit && HQ3D.world!=="city"){
    var j=document.createElement("button"); j.type="button"; j.className="hbtn hq3d-jump"; j.textContent="\uD83C\uDFD9 Jump to City";
    j.title="Go straight to Arena City: the street of open HQs, the fountain and the bike park";
    j.addEventListener("click", hqJumpCity); box.appendChild(j);
  }
  HQ_WHERE.forEach(function(w){
    var b=document.createElement("button"); b.type="button"; b.className="hbtn ghost"; b.textContent = w[0]==="base" && city ? "City" : w[1];
    var here=HQ3D.world||"", on = here===w[0] || (w[0]==="base" && here==="city"); b.setAttribute("aria-pressed", on ? "true" : "false");
    b.addEventListener("click", function(){ if(HQ3D.inst && HQ3D.inst.goWorld) HQ3D.inst.goWorld(w[0]); });
    box.appendChild(b);
  });
  if(HQ3D.visit) return;
  // City / Private: your building on the Arena's street (open to visitors), or just your own HQ
  var t=document.createElement("button"); t.type="button"; t.className="hbtn hq3d-city"+(city?" on":"");
  t.textContent = city ? "🏙 City · go private" : "🔒 Private · join the city";
  t.title = city ? "Close your HQ to visitors and see only your own building" : "Open your HQ to visitors and put your building on the Arena's street";
  t.setAttribute("aria-pressed", city ? "true" : "false");
  t.addEventListener("click", function(){ hqCitySet(!city); });
  box.appendChild(t);
}
/* ---- HQ 2.1: Arena City. Everyone who opened their HQ has a building on one round street. ---- */
// Being in the city is being open to visitors: going private closes your HQ again.
var HQ_CITY = {list:[], sig:"", at:0, back:null, busy:false};
function hqCityOn(){ return typeof HQ_REMOTE!=="undefined" && !!(HQ_REMOTE && HQ_REMOTE.open && HQ_REMOTE.me); }
function hqCitySet(on){
  if(HQ_CITY.busy) return; HQ_CITY.busy=true;
  hqArena("POST","/api/arena/hq/me",{open:on, look:hqLook(), crew:true}).then(function(res){
    HQ_CITY.busy=false;
    if(!res.ok){ toast("⚠ "+hqArenaWhy(res),"ach"); return; }
    HQ_REMOTE.open=!!res.j.open; HQ_REMOTE.me=res.j.userId||HQ_REMOTE.me;
    var o=$("hqOpen"); if(o) o.checked=HQ_REMOTE.open;
    toast(HQ_REMOTE.open ? "🏙 Your HQ is on the Arena City street, open to visitors" : "🔒 Private: your HQ is closed to visitors","level");
    hqCityLoad(true);
    var w=HQ3D.world; if(HQ3D.inst && (w==="base" || w==="city")) HQ3D.inst.goWorld("base");   // "base" = outside: the city or your Base
    hqRenderWhere();
  }).catch(function(){ HQ_CITY.busy=false; toast("⚠ The Arena didn't answer","ach"); });
}
// Outside follows City / Private: if you are outside, step onto the right one.
function hqCityOutside(){
  hqCityLoad(true); hqRenderWhere();
  // standing outside your own Base while your HQ is on the street: step onto the street (the city
  // itself you may walk while private, so being there is never undone here)
  var w=HQ3D.world; if(HQ3D.inst && !HQ3D.visit && w==="base" && hqCityOn()) HQ3D.inst.goWorld("base");
}
function hqCityLoad(force){
  if((!hqCityOn() && HQ3D.world!=="city") || HQ3D.visit) return;
  if(!force && Date.now()-HQ_CITY.at < 60000) return;
  HQ_CITY.at=Date.now();
  hqArena("GET","/api/arena/hq/open").then(function(res){
    if(!res.ok) return;
    var list=(res.j.hqs||[]).slice(0,100), sig=JSON.stringify(list);
    if(sig===HQ_CITY.sig) return;
    HQ_CITY.sig=sig; HQ_CITY.list=list;
    if(HQ3D.inst && !HQ3D.visit && HQ3D.inst.cityChanged) HQ3D.inst.cityChanged();
  }).catch(function(){});
}
setInterval(function(){ if(VIEW==="hq" && !document.hidden) hqCityLoad(false); }, 5000);
// A crew member's card: the same session drawer the classic views open.
function hqOpen(id){
  var s=((STATE && STATE.sessions) || []).filter(function(x){ return x.sessionId===id; })[0];
  if(s && typeof openDrawer==="function") openDrawer(s);
  else if(typeof openSession==="function") openSession(id);
}
function hqEnter(){
  var stage=$("hqStage"); if(!stage) return;
  if(!hqWebGL()){
    HQ3D.failed=true; stage.textContent="";
    var p=document.createElement("p"); p.className="hq3d-msg"; p.textContent="This browser can't draw the 3D HQ (no WebGL 2). The crew list on the right and the classic dashboard work as usual.";
    var b=document.createElement("button"); b.className="hbtn"; b.type="button"; b.textContent="Classic dashboard"; b.addEventListener("click", hqToggle);
    stage.appendChild(p); stage.appendChild(b); hqRenderCrew(); return;
  }
  hqLoadScripts().then(function(){
    if(VIEW!=="hq") return;
    if(!HQ3D.inst){ HQ3D.inst = window.HQV.hq3d.mount(stage, hqApi()); }
    if(!HQ3D.visit) HQ3D.inst.update((STATE && STATE.sessions) || []);
    HQ3D.inst.setFilter(HQ3D.filter);
    HQ3D.inst.resume();
  }).catch(function(e){ stage.textContent="The 3D HQ couldn't load: "+e.message; });
  hqRenderCrew(); hqRenderWhere(); hqSync();
}
function hqLeave(){ if(HQ3D.inst) HQ3D.inst.pause(); }
function hqViewChanged(v){
  if(!HQ3D) return;     // the first setView() runs before this file has set up (it calls this again at the end)
  if(v!=="hq") HQ3D.lastClassic = v;
  var b=$("hqModeBtn"), l=$("hqModeLbl");
  if(b){ b.setAttribute("aria-pressed", v==="hq" ? "true" : "false"); }
  if(l) l.textContent = v==="hq" ? "Classic" : "3D HQ";
  if(v==="hq"){ hqEnter(); if(typeof progLoad==="function") progLoad(false); } else hqLeave();
  if(v==="arena" && typeof gbLoad==="function"){ gbLoad(); progLoad(false); if(typeof crewLoad==="function") crewLoad(); }
}
// One key (H) and the header switch flip between the 3D HQ and the classic view you came from.
function hqToggle(){
  if(VIEW==="hq"){ hqModeSave("classic"); setView(HQ3D.lastClassic && HQ3D.lastClassic!=="hq" ? HQ3D.lastClassic : "live"); }
  else { hqModeSave("3d"); setView("hq"); }
}
function hqOnState(d){
  if(!HQ3D || VIEW!=="hq") return;
  if(HQ3D.inst && !HQ3D.visit) HQ3D.inst.update((d && d.sessions) || []);
  hqRenderCrew();
}
var HQ_STATE_TXT = {working:"Working", needs:"Needs you", idle:"Idle", stale:"Away"};
function hqCrewState(s){ var st=s&&s.status; return st==="working"||st==="needs"||st==="idle" ? st : "stale"; }
function hqProject(s){ return window.HQV && HQV.hq3d ? HQV.hq3d.projectOf(s) : String((s && (s.cwd||"").split("/").pop()) || "other").slice(0,40); }
// The crew beside the scene: everyone live, needs-you first; the same click opens their card.
function hqRenderCrew(){
  var ul=$("hqCrew"); if(!ul) return;
  if(HQ3D.visit){
    var c=HQ3D.visit.crew||{}; ul.textContent=""; $("hqCrewCount").textContent=""; $("hqFilter").hidden=true;
    [["needs","Need them",c.needs],["working","Working",c.working],["idle","Idle",c.idle]].forEach(function(r){
      var li=document.createElement("li"), d=document.createElement("div"); d.className="hq3d-mate"; d.dataset.state=r[0];
      var dot=document.createElement("span"); dot.className="hq3d-dot"; var b=document.createElement("b"); b.textContent=(r[2]|0)+" "+r[1].toLowerCase();
      var sm=document.createElement("small"); sm.textContent="Their sessions stay private: counts only."; d.appendChild(dot); d.appendChild(b); d.appendChild(sm); li.appendChild(d); ul.appendChild(li);
    });
    return;
  }
  var order={needs:0, working:1, idle:2, stale:3};
  var all=((STATE && STATE.sessions) || []).filter(function(s){ return hqCrewState(s)!=="stale"; });
  var list=all.filter(function(s){ return !HQ3D.filter || hqProject(s)===HQ3D.filter; })
    .sort(function(a,b){ return (order[hqCrewState(a)]-order[hqCrewState(b)]) || ((a.ageSecs||0)-(b.ageSecs||0)); });
  $("hqCrewCount").textContent = all.length ? String(all.length)+" live" : "";
  var fb=$("hqFilter");
  if(HQ3D.filter){
    fb.hidden=false; fb.textContent="";
    var t=document.createElement("span"); t.textContent="Showing "+HQ3D.filter; fb.appendChild(t);
    var x=document.createElement("button"); x.type="button"; x.className="hbtn ghost"; x.textContent="Show everyone";
    x.addEventListener("click", function(){ HQ3D.filter=null; if(HQ3D.inst) HQ3D.inst.setFilter(null); hqRenderCrew(); });
    fb.appendChild(x);
  } else fb.hidden=true;
  ul.textContent="";
  if(!list.length){ var li=document.createElement("li"); li.className="muted"; li.textContent="No live sessions right now: the desks are empty."; ul.appendChild(li); return; }
  list.forEach(function(s){
    var st=hqCrewState(s), li=document.createElement("li"), b=document.createElement("button");
    b.type="button"; b.className="hq3d-mate"; b.dataset.state=st;
    var dot=document.createElement("span"); dot.className="hq3d-dot"; dot.setAttribute("aria-hidden","true");
    var name=document.createElement("b"); name.textContent=(window.HQV && HQV.hq3d) ? HQV.hq3d.titleOf(s) : (s.title||s.name||"Session");
    var meta=document.createElement("small"); meta.textContent=HQ_STATE_TXT[st]+" · "+hqProject(s);
    b.appendChild(dot); b.appendChild(name); b.appendChild(meta);
    b.addEventListener("click", function(){ hqOpen(s.sessionId); });
    li.appendChild(b); ul.appendChild(li);
  });
}
(function(){
  var b=$("hqModeBtn"); if(b) b.addEventListener("click", hqToggle);
  // HQ always opens on the classic dashboard (Live unless you were on another tab); the 3D HQ is one
  // H key or the header switch away. After every file of the page script has run (later files add to HQ).
  setTimeout(function(){ hqViewChanged(VIEW); }, 0);
})();

/* ---- HQ 2.1: customise your building; visit friends' HQs through the Arena ---- */
// The look is kept here (localStorage hq_look) and, when paired, on the Arena so visitors see it.
// Visiting mounts a second copy of the scene fed by THEIR level, look and crew counts.
// Paint: the default navy and graphite, then colours bold enough to tell HQs apart across the street.
var HQ_PAINTS = ["#2a3c50","#2c2c34","#b5473a","#d07a2c","#c9a227","#3f8f5a","#2f8fa3","#3d6fd1","#7a52c7","#c2507f","#d9d2c3","#6b4a32"];
var HQ_ACCENTS = ["#ffb347","#5fd3e6","#6fd38a","#ff6b5b","#9b8cf0","#f4f4f4"];
var HQ_REMOTE = {open:false, synced:false, beat:0};
function hqSaveLook(l){ try { localStorage.setItem("hq_look", JSON.stringify(l)); } catch(e){} }
function hqSwatches(id, list, key){
  var box=$(id); if(!box) return; box.textContent="";
  var cur=hqLook()[key];
  list.forEach(function(c){
    var b=document.createElement("button"); b.type="button"; b.className="hq3d-sw"; b.style.background=c;
    b.setAttribute("aria-label", key+" "+c); b.setAttribute("aria-pressed", cur===c ? "true" : "false");
    b.addEventListener("click", function(){ var l=hqLook(); l[key]=c; hqSaveLook(l); hqSwatches(id, list, key); hqPreview(); });
    box.appendChild(b);
  });
}
function hqPreview(){ if(HQ3D.inst && !HQ3D.visit && HQ3D.inst.lookChanged) HQ3D.inst.lookChanged(); }
function hqRenderBuild(){
  hqSwatches("hqPaint", HQ_PAINTS, "paint"); hqSwatches("hqAccent", HQ_ACCENTS, "accent");
  var s=$("hqSign"); if(s && document.activeElement!==s) s.value=hqLook().sign||"";
  var o=$("hqOpen"); if(o) o.checked=!!HQ_REMOTE.open;
}
function hqArena(method, path, body){
  var opt={cache:"no-store"};
  if(method==="POST"){ opt.method="POST"; opt.headers={"Content-Type":"application/json","X-HQ-Token":CSRF}; opt.body=JSON.stringify(body||{}); }
  return fetch(path, opt).then(function(r){ return r.json().then(function(j){ return {ok:r.ok, code:r.status, j:j}; }, function(){ return {ok:false, code:r.status, j:{}}; }); });
}
function hqArenaWhy(res){
  if(res.code===400 && /not paired/.test((res.j && res.j.error)||"")) return "Pair with an Arena (Arena tab) to save online and visit friends.";
  if(res.code===404) return "This Arena doesn't have HQ visits yet: ask its owner to update it.";
  return (res.j && (res.j.error || res.j.detail)) || "The Arena didn't answer.";
}
// Pull the saved look and openness once; the Arena copy wins over this browser's.
function hqSync(){
  if(typeof HQ_REMOTE==="undefined" || !HQ_REMOTE || HQ_REMOTE.synced) return;   // (boot runs before this file's state is set)
  HQ_REMOTE.synced=true;
  hqArena("GET","/api/arena/hq/me").then(function(res){
    if(!res.ok) return;
    HQ_REMOTE.open=!!res.j.open; HQ_REMOTE.me=res.j.userId||null;
    var l=res.j.look||{}, mine=hqLook();
    ["paint","accent","sign"].forEach(function(k){ if(l[k]) mine[k]=l[k]; });
    hqSaveLook(mine); hqRenderBuild(); hqPreview();
    hqCityOutside();
  }).catch(function(){});
}
function hqSave(){
  var l=hqLook(), s=$("hqSign"), note=$("hqSaveNote");
  l.sign = (s && s.value.trim()) || ""; if(!l.sign) delete l.sign;
  hqSaveLook(l); hqPreview();
  var open=!!($("hqOpen") && $("hqOpen").checked);
  note.textContent="Saving…";
  hqArena("POST","/api/arena/hq/me",{open:open, look:l, crew:true}).then(function(res){
    if(res.ok){ HQ_REMOTE.open=!!res.j.open; HQ_REMOTE.me=res.j.userId||HQ_REMOTE.me; note.textContent = HQ_REMOTE.open ? "Saved. Your HQ is open to visitors and stands on the Arena City street." : "Saved. Your HQ is closed to visitors (private)."; hqCityOutside(); }
    else note.textContent="Saved here. "+hqArenaWhy(res);
  }).catch(function(){ note.textContent="Saved here. The Arena didn't answer."; });
}
// While your HQ is open and you are in it, refresh the crew counts visitors see (every 2 minutes).
function hqHeartbeat(){
  if(!HQ_REMOTE.open || VIEW!=="hq" || document.hidden) return;
  var now=Date.now(); if(now - HQ_REMOTE.beat < 120000) return;
  HQ_REMOTE.beat=now; hqArena("POST","/api/arena/hq/me",{crew:true}).catch(function(){});
}
setInterval(hqHeartbeat, 15000);
function hqLoadOpen(){
  var ul=$("hqOpenList"); if(!ul) return;
  ul.innerHTML='<li class="muted">Looking…</li>';
  hqArena("GET","/api/arena/hq/open").then(function(res){
    ul.textContent="";
    if(!res.ok){ var e=document.createElement("li"); e.className="muted"; e.textContent=hqArenaWhy(res); ul.appendChild(e); return; }
    var list=(res.j.hqs||[]).filter(function(h){ return !h.isYou; });
    if(!list.length){ var n=document.createElement("li"); n.className="muted"; n.textContent="No one has opened their HQ yet. Open yours above and tell a friend."; ul.appendChild(n); return; }
    list.forEach(function(h){
      var li=document.createElement("li"), b=document.createElement("button"); b.type="button"; b.className="hq3d-mate";
      var nm=document.createElement("b"); nm.textContent=(h.trainerName||h.displayName||h.handle)+"'s HQ";
      var c=h.crew||{}, meta=document.createElement("small");
      meta.textContent="Lv "+h.level+" · "+(c.working|0)+" working · "+(c.needs|0)+" need them · "+(c.idle|0)+" idle";
      var dot=document.createElement("span"); dot.className="hq3d-dot"; dot.setAttribute("aria-hidden","true");
      b.appendChild(dot); b.appendChild(nm); b.appendChild(meta);
      b.addEventListener("click", function(){ hqVisit(h.userId); });
      li.appendChild(b); ul.appendChild(li);
    });
  }).catch(function(){ ul.innerHTML='<li class="muted">The Arena didn\'t answer.</li>'; });
}
// Their crew as stand-ins: states only, no names or projects (that is all the Arena has).
function hqVisitCrew(c){
  var out=[], n=0;
  [["needs",c.needs],["working",c.working],["idle",c.idle]].forEach(function(p){
    for(var i=0;i<Math.min(16,p[1]|0);i++){ out.push({sessionId:"visit-"+(n++), status:p[0], title:"Crew member", cwd:"", folder:"crew", spark:[], creature:{typeHue:(n*47)%360}}); }
  });
  return out.slice(0,16);
}
function hqMount(api){
  if(HQ3D.inst){ HQ3D.inst.destroy(); HQ3D.inst=null; }
  var stage=$("hqStage"); stage.textContent="";
  HQ3D.inst=window.HQV.hq3d.mount(stage, api);
}
function hqVisit(userId, opts){
  var fromCity = !!(opts && opts.from==="city");
  hqArena("GET","/api/arena/hq/visit?u="+encodeURIComponent(userId)).then(function(res){
    if(!res.ok){ toast("⚠ "+hqArenaWhy(res),"ach"); return; }
    var p=res.j, who=p.trainerName||p.displayName||p.handle;
    HQ3D.visit=p;
    hqLoadScripts().then(function(){
      var base=hqApi();
      if(fromCity) HQ_CITY.back=userId;          // home again: back out on the street at their door
      hqMount({
        name: base.name, remember:false, startWorld: fromCity ? "lobby" : "base", frame: base.frame,
        level: function(){ return Math.max(1, p.level|0); },
        look: function(){ return Object.assign({}, p.look||{}, {decor: (p.cos||{}).decor}); },
        openSession: function(){ toast("That's "+who+"'s crew: their sessions stay private.","level"); },
        go: function(){ toast("That room is in "+who+"'s HQ. Go home first.","level"); },
        onFilter: function(){}, onWorld: base.onWorld
      });
      HQ3D.inst.update(hqVisitCrew(p.crew||{})); HQ3D.inst.resume();
      hqRenderVisiting(); hqRenderCrew();
      announce("Visiting "+who+"'s HQ");
    });
  }).catch(function(){ toast("⚠ The Arena didn't answer","ach"); });
}
function hqGoHome(){
  if(!HQ3D.visit) return;
  HQ3D.visit=null;
  hqMount(Object.assign(hqApi(), {startWorld:"base"}));
  setTimeout(function(){ HQ_CITY.back=null; }, 4000);
  HQ3D.inst.update((STATE && STATE.sessions) || []); HQ3D.inst.resume();
  hqRenderVisiting(); hqRenderCrew();
}
function hqRenderVisiting(){
  var v=$("hqVisiting"); if(!v) return;
  var p=HQ3D.visit; v.hidden=!p; v.textContent="";
  if(!p) return;
  var t=document.createElement("span"); t.textContent="Visiting "+(p.trainerName||p.displayName||p.handle)+"'s HQ · Lv "+p.level;
  var b=document.createElement("button"); b.type="button"; b.className="hbtn"; b.textContent="Back home"; b.addEventListener("click", hqGoHome);
  // their trophies, and a game with them from here (opens it in the Valley and invites them)
  var tc=document.createElement("button"); tc.type="button"; tc.className="hbtn ghost"; tc.textContent="🏆 Trainer card";
  tc.addEventListener("click", function(){ if(typeof tcardOpen==="function") tcardOpen(p.userId); });
  var sel=document.createElement("select"); sel.className="hbtn ghost"; sel.setAttribute("aria-label","Play a game with them");
  var o0=document.createElement("option"); o0.value=""; o0.textContent="🎮 Play with them…"; sel.appendChild(o0);
  (typeof PLAY_GAMES!=="undefined" ? PLAY_GAMES : []).forEach(function(g){ var o=document.createElement("option"); o.value=g[0]; o.textContent=g[1]+" "+g[2]; sel.appendChild(o); });
  sel.addEventListener("change", function(){ var g=sel.value; sel.value=""; if(g && typeof hqVisitPlay==="function") hqVisitPlay(p, g); });
  v.appendChild(t); v.appendChild(tc); v.appendChild(sel); v.appendChild(b);
}
(function(){
  var sv=$("hqSave"); if(sv) sv.addEventListener("click", hqSave);
  var si=$("hqSign"); if(si) si.addEventListener("input", function(){ var l=hqLook(); l.sign=si.value.trim(); if(!l.sign) delete l.sign; hqSaveLook(l); hqPreview(); });
  var vs=$("hqVisitSec"); if(vs) vs.addEventListener("toggle", function(){ if(vs.open) hqLoadOpen(); });
  var bs=$("hqBuildSec"); if(bs) bs.addEventListener("toggle", function(){ if(bs.open){ hqRenderBuild(); hqSync(); } });
})();

/* ---- HQ 2.1: live together. Everyone in the same HQ sees everyone else walk around. ---- */
// One extra Arena socket, to the room of the HQ you are in: "hq_<owner id>" (yours while it is
// open to visitors, or the one you are visiting). It carries only where you stand: the floor,
// x/z, facing and whether you walk. The server sends everyone's spot back 8 times a second.
var HQNET = {ws:null, room:null, gen:0, sentAt:0, last:"", peers:[], timer:null};
function hqNetWant(){
  if(VIEW!=="hq" || !HQ3D.inst || document.hidden) return null;
  if(HQ3D.visit) return "hq_"+HQ3D.visit.userId;
  if(HQ3D.world==="city" && (hqCityOn() || (window.ARENA && ARENA.paired))) return "hq_city";   // everyone on the street sees everyone
  if(HQ_REMOTE.open && HQ_REMOTE.me) return "hq_"+HQ_REMOTE.me;
  return null;
}
function hqNetClose(){
  HQNET.gen++; HQNET.room=null; HQNET.peers=[];
  if(HQNET.ws && !HQNET.shared){ try { HQNET.ws.close(); } catch(e){} }
  HQNET.ws=null; HQNET.shared=false;
  if(HQ3D.inst && HQ3D.inst.setPeers) HQ3D.inst.setPeers([]);
  hqRenderHere();
}
// The everyone-sees-everyone positions, from either socket.
function hqNetOnGame(m){
  if(!m || m.g!=="hq" || m.ev!=="snap" || !Array.isArray(m.ps)) return;
  if(HQNET.shared && ARENA.roomId!==HQNET.room) return;
  HQNET.peers=m.ps.filter(function(p){ return p && p.u!==HQNET.me; });
  if(HQ3D.inst && HQ3D.inst.setPeers) HQ3D.inst.setPeers(HQNET.peers);
  hqRenderHere();
}
function hqNetSync(){
  var want=hqNetWant();
  // When the Arena socket is already in this place (talk follows you: ui/app/28-talk.js), presence rides
  // it: one socket, so you are one person in the room and chat, voice and positions share it.
  var A=window.ARENA||{}, shared = !!(want && A.roomId===want && A.sock && A.sock.readyState===1 && A.you);
  if(shared){
    if(HQNET.ws===A.sock) return;
    hqNetClose();
    HQNET.room=want; HQNET.ws=A.sock; HQNET.shared=true; HQNET.me=A.you.userId;
    try { A.sock.send(JSON.stringify({type:"game", g:"hq", op:"join"})); } catch(e){}
    HQNET.lookSent=null;
    return;
  }
  if(HQNET.shared){ hqNetClose(); }
  if(want===HQNET.room) return;
  hqNetClose();
  if(!want) return;
  HQNET.room=want; var gen=HQNET.gen;
  arenaPost("/api/arena/ticket").then(function(res){
    if(gen!==HQNET.gen || !res.ok || !res.j || !res.j.wsUrl) { if(gen===HQNET.gen) HQNET.room=null; return; }
    var ws;
    try { ws=new WebSocket(res.j.wsUrl+"/v1/rooms/"+encodeURIComponent(want)+"/ws?ticket="+encodeURIComponent(res.j.ticket)); }
    catch(e){ HQNET.room=null; return; }
    HQNET.ws=ws;
    ws.onmessage=function(ev){
      if(gen!==HQNET.gen) return;
      var m; try { m=JSON.parse(ev.data); } catch(e){ return; }
      if(!m || typeof m!=="object") return;
      if(m.type==="welcome"){ HQNET.me=(m.you&&m.you.userId)||null; HQNET.arena=(m.arena && typeof m.arena==="object") ? m.arena : null; ws.send(JSON.stringify({type:"game", g:"hq", op:"join"})); HQNET.lookSent=null; return; }
      if(m.type!=="game" || m.g!=="hq") return;
      hqNetOnGame(m);
    };
    ws.onclose=function(){ if(gen===HQNET.gen){ HQNET.ws=null; HQNET.room=null; HQNET.peers=[]; if(HQ3D.inst && HQ3D.inst.setPeers) HQ3D.inst.setPeers([]); hqRenderHere(); } };
  }).catch(function(){ if(gen===HQNET.gen) HQNET.room=null; });
}
// Does the Arena the presence goes to know this hq capability (e.g. "ride": a = 3 for riding a bike)?
function hqNetCan(cap){
  var info = HQNET.shared ? (window.ARENA && ARENA.arena) : HQNET.arena;
  var hq = info && info.games && info.games.hq;
  return !!(hq && Array.isArray(hq.caps) && hq.caps.indexOf(cap)>=0);
}
// Send where you are 8 times a second while you move (once every 2 s when you stand still).
function hqNetSend(){
  var ws=HQNET.ws; if(!ws || ws.readyState!==1 || !HQ3D.inst || !HQ3D.inst.where) return;
  var w=HQ3D.inst.where(); if(!w || !w.world) return;
  // Your character, once per join and whenever it changes, to an Arena that shows it ("look").
  var spec = typeof charSpec==="function" ? charSpec() : null, sg = spec ? spec.join(".") : null;
  if(sg && HQNET.lookSent!==sg && hqNetCan("look")){
    try { ws.send(JSON.stringify({type:"game", g:"hq", op:"look", c:spec})); HQNET.lookSent=sg; } catch(e){}
  }
  var a=HQ3D.inst.moving ? HQ3D.inst.moving() : 0;
  if(a===3 && !hqNetCan("ride")) a=2;   // an Arena without bikes drops a 3 (and you would freeze): send "running"
  if(a===4 && !hqNetCan("skate")) a=2;  // nor does one without skateboards know a 4
  // a car (5) or a motorbike (6) needs "drive"; without it, show a bike where bikes are known
  if((a===5 || a===6) && !hqNetCan("drive")) a = hqNetCan("ride") ? 3 : 2;
  var msg={type:"game", g:"hq", op:"pos", w:w.world, x:Math.round(w.x*100), z:Math.round(w.z*100),
           r:Math.round(((w.yaw*180/Math.PI)%360+360)%360), a:a};
  // on a board: the trick you are doing (k) and how high you are (y, decimetres), so others see it
  var sk = a===4 && HQ3D.inst.skate ? HQ3D.inst.skate() : null;
  if(sk){ msg.k=sk.k|0; msg.y=sk.y|0; }
  var key=[msg.w,msg.x,msg.z,msg.r,msg.a,msg.k,msg.y].join(","), now=Date.now();
  if(key===HQNET.last && now-HQNET.sentAt<2000) return;
  HQNET.last=key; HQNET.sentAt=now;
  try { ws.send(JSON.stringify(msg)); } catch(e){}
}
setInterval(function(){ if(VIEW==="hq") hqSync(); hqNetSync(); hqNetSend(); }, 125);
// "Here now": who else is in this HQ, and on which floor.
var HQ_FLOOR_NAME = {base:"outside", city:"in Arena City", lobby:"in the Lobby", mission:"in Mission Control"};
// (riding a bike shows as "on a bike" beside the floor, a skateboard as "skating", and so on)
var HQ_MOVING_NAME = {3:" · on a bike", 4:" · skating", 5:" · driving", 6:" · on a motorbike"};
function hqRenderHere(){
  var box=$("hqHere"); if(!box) return;
  var ps=HQNET.peers||[];
  box.hidden=!ps.length; box.textContent="";
  if(!ps.length) return;
  var h=document.createElement("b"); h.textContent="Here now"; box.appendChild(h);
  ps.slice(0,12).forEach(function(p){
    var d=document.createElement("button"); d.type="button"; d.className="hq3d-person"; d.textContent=(p.n||"Visitor")+" · "+(HQ_FLOOR_NAME[p.w]||"")+(HQ_MOVING_NAME[p.a]||"");
    d.title="Open their trainer card"; d.addEventListener("click", function(){ if(typeof tcardOpen==="function") tcardOpen(p.u); });
    if(typeof cosFrameApply==="function") cosFrameApply(d, p.f);
    box.appendChild(d);
  });
}

/* ---- Arena City skate board: each player's best combo this UTC week (backend-rs/src/skate.rs) ----
   Only a combo's total and its trick count leave this machine (ext_skate.py checks both), and
   only when paired, only for a new best this week. The board is read through the local proxy. */
var HQ_SKATE = {board:null, at:0, loading:false, post:null, timer:0, sentAt:0, err:null};
function hqSkatePaired(){ return !!(window.ARENA && ARENA.paired); }
function hqSkateShow(){
  var sec=$("hqSkateSec"); if(!sec) return;
  var on = HQ3D.world==="city" && hqSkatePaired();
  sec.hidden=!on;
  if(on && !HQ_SKATE.loading && Date.now()-HQ_SKATE.at > 60000) hqSkateLoad();
}
function hqSkateLoad(){
  HQ_SKATE.loading=true;
  fetch("/api/arena/skate",{cache:"no-store"}).then(function(r){ return r.json().then(function(j){ return {ok:r.ok, code:r.status, j:j||{}}; }); })
    .then(function(res){
      if(res.ok && res.j && Array.isArray(res.j.top)){ HQ_SKATE.board=res.j; HQ_SKATE.err=null; }
      else HQ_SKATE.err = res.code===404 ? "This Arena has no skate board yet." : (res.j.error||"The Arena didn't answer.");
    }).catch(function(){ HQ_SKATE.err="The Arena didn't answer."; })
    .then(function(){ HQ_SKATE.loading=false; HQ_SKATE.at=Date.now(); hqSkateRender(); });
}
function hqSkateRender(){
  var ol=$("hqSkateBoard"), me=$("hqSkateMe"); if(!ol || !me) return;
  ol.textContent="";
  var b=HQ_SKATE.board;
  if(!b){ me.textContent=HQ_SKATE.err||"Loading…"; return; }
  (b.top||[]).slice(0,10).forEach(function(e){
    var li=document.createElement("li"), n=document.createElement("span"), v=document.createElement("b");
    n.textContent=e.displayName||e.handle||"Someone"; v.textContent=String(e.best|0);
    li.title=(e.tricks|0)+" tricks"; li.appendChild(n); li.appendChild(v); ol.appendChild(li);
  });
  if(!(b.top||[]).length){ var li=document.createElement("li"); li.className="muted"; li.textContent="No combos yet this week. Be the first."; ol.appendChild(li); }
  me.textContent = b.me ? "You: #"+(b.me.rank|0)+" · best "+(b.me.best|0) : "Land a combo in the skatepark to get on the board.";
}
// Called when a combo banks. Only a new best for this week goes out, at most once every 4 s
// (the Arena refuses faster ones); the latest best waits for the next slot.
function hqSkateCombo(c){
  if(!c || !hqSkatePaired()) return;
  var total=Math.round(+c.total||0), tricks=Math.max(1, Math.min(10, c.tricks|0));
  if(total<1 || total>200000) return;
  var mine=HQ_SKATE.board && HQ_SKATE.board.me ? (HQ_SKATE.board.me.best|0) : 0;
  if(total<=mine || (HQ_SKATE.post && total<=HQ_SKATE.post.total)) return;
  HQ_SKATE.post={total:total, tricks:tricks};
  if(HQ_SKATE.timer) return;
  var wait=Math.max(0, 4000-(Date.now()-HQ_SKATE.sentAt));
  HQ_SKATE.timer=setTimeout(hqSkateSend, wait);
}
function hqSkateSend(){
  HQ_SKATE.timer=0;
  var p=HQ_SKATE.post; if(!p) return;
  HQ_SKATE.post=null; HQ_SKATE.sentAt=Date.now();
  fetch("/api/arena/skate/combo",{method:"POST", headers:{"Content-Type":"application/json","X-HQ-Token":CSRF}, body:JSON.stringify(p)})
    .then(function(r){ return r.json().then(function(j){ return {ok:r.ok, j:j||{}}; }); })
    .then(function(res){
      if(res.ok && typeof res.j.best==="number"){
        if(!HQ_SKATE.board) HQ_SKATE.board={week:res.j.week, top:[], me:null};
        HQ_SKATE.board.me={best:res.j.best, rank:res.j.rank};
        if(typeof announce==="function") announce("Skate board: #"+(res.j.rank|0)+" this week with "+(res.j.best|0)+".");
        HQ_SKATE.at=0; hqSkateShow();
      }
    }).catch(function(){})
    .then(function(){ if(HQ_SKATE.post && !HQ_SKATE.timer) HQ_SKATE.timer=setTimeout(hqSkateSend, 4000); });
}
