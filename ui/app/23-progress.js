/* ================= HQ 2.1: progression, trainer cards and game leaderboards ================= */
// All three come from the Arena, scored there: your HQ level (session XP + game XP from the
// multiplayer games it refereed), anyone's trainer card, and per-game boards. Read-only here.
var PROG = {data:null, at:0, loading:false};
var PROG_GAMES = [["kart","Kart Racing"],["plat","Platformer Rush"],["golf","Mini Golf"],["fps","Blaster Arena"],["type","Code Typing Race"],["td","Tower Defense"],["bowl","Bowling"],["rally","Valley Rally"]];
function progGet(path){
  return fetch(path,{cache:"no-store"}).then(function(r){ return r.json().then(function(j){ return {ok:r.ok, code:r.status, j:j}; }, function(){ return {ok:false, code:r.status, j:{}}; }); });
}
function progMs(ms){ if(ms==null) return "–"; ms=Math.max(0,ms|0); var m=Math.floor(ms/60000), s=(ms%60000)/1000; return (m?m+":":"")+(m&&s<10?"0":"")+s.toFixed(2); }

// Your HQ level from the Arena (falls back to the season level the page shows when unpaired).
function progLevel(){ return (typeof PROG!=="undefined" && PROG && PROG.data && PROG.data.level) ? PROG.data.level : null; }
// The one level everything calls "your level": the Arena's HQ level (all-time session XP + game XP)
// when paired, else this machine's 30-day season level. {level, rank, pct, xpIntoLevel, xpForLevel, hq}
function myLevel(){
  var d=(typeof PROG!=="undefined" && PROG) ? PROG.data : null;
  if(d && d.level) return {level:d.level, rank:d.rank, xpIntoLevel:d.xpIntoLevel|0, xpForLevel:d.xpForLevel|0,
    pct: d.xpForLevel ? Math.round(1000*(d.xpIntoLevel||0)/d.xpForLevel)/10 : 0, hq:true};
  var s=(typeof STATE!=="undefined" && STATE && STATE.season) || {};
  return {level:s.level!=null ? s.level : null, rank:s.rank, xpIntoLevel:s.xpIntoLevel|0, xpForLevel:s.xpForLevel|0, pct:s.pct||0, hq:false};
}
function progLoad(force){
  if(typeof PROG==="undefined" || !PROG) return;          // called before this file ran (startup)
  if(PROG.loading || (!force && Date.now()-PROG.at < 5*60*1000)) return;
  PROG.loading=true;
  progGet("/api/arena/progress").then(function(res){
    PROG.loading=false; PROG.at=Date.now();
    if(!res.ok || !res.j || !res.j.level) return;
    var prev=PROG.data && PROG.data.level, seen=0;
    try { seen=parseInt(localStorage.getItem("hq_seen_level")||"0",10)||0; } catch(e){}
    PROG.data=res.j;
    // The level-up moment: once per new level, in the HQ (or on next visit to it).
    if(seen && res.j.level>seen) progLevelUp(seen, res.j.level);
    try { localStorage.setItem("hq_seen_level", String(res.j.level)); } catch(e){}
    if(prev!==res.j.level && HQ3D && HQ3D.inst && !HQ3D.visit && HQ3D.inst.lookChanged) HQ3D.inst.lookChanged();
    // everywhere that shows your level now shows this one
    if(typeof STATE!=="undefined" && STATE){ if(typeof renderSeason==="function") renderSeason(STATE.season); if(typeof renderTrainerCard==="function") renderTrainerCard(); }
  }).catch(function(){ PROG.loading=false; });
}
function progLevelUp(from, to){
  // the Base's unlocks (games/hqbase.js has the same list; it may not be loaded yet)
  var unlocks=(window.HQV && HQV.hqUnlocks) || [[5,"Plaza fountain"],[10,"West wing"],[15,"Streak banners"],[20,"Helipad and helicopter"],
    [25,"Sky terrace garden"],[30,"East wing and skybridges"],[35,"Kart track"],[40,"Observatory dome"],[45,"Crew billboard"],[50,"Hall of fame statue"],[55,"Arena annex"]];
  var got=unlocks.filter(function(u){ return u[0]>from && u[0]<=to; }).map(function(u){ return u[1]; });
  toast("⬆ HQ level "+to+"!"+(got.length ? " New at your base: "+got.join(", ")+"." : " Your tower grew."),"level");
  if(typeof confettiBurst==="function" && !hqCalm()) confettiBurst();
  if(window.HQV && HQV.engine) HQV.engine.sfx("levelup");
  announce("HQ level "+to);
}
setInterval(function(){ if(VIEW==="hq" || VIEW==="arena") progLoad(false); }, 30000);

/* ---- the trainer card: one card for anyone, from the Arena ---- */
function tcardOpen(userId){
  var back=$("tcardBack"); if(!back) return;
  var body=$("tcardBody"); body.innerHTML='<p class="muted">Loading…</p>';
  back.classList.add("open"); back.setAttribute("aria-hidden","false");
  var closeBtn=$("tcardClose"); if(closeBtn) closeBtn.focus();
  progGet("/api/arena/profile?u="+encodeURIComponent(userId||"me")).then(function(res){
    if(!res.ok){ body.innerHTML=""; var p=document.createElement("p"); p.className="muted";
      p.textContent = res.code===404 ? "This Arena doesn't have trainer cards yet: ask its owner to update it." : (res.j.error||res.j.detail||"The Arena didn't answer."); body.appendChild(p); return; }
    tcardRender(res.j);
  }).catch(function(){ body.innerHTML='<p class="muted">The Arena didn\'t answer.</p>'; });
}
function tcardClose(){ var back=$("tcardBack"); if(back){ back.classList.remove("open"); back.setAttribute("aria-hidden","true"); } }
function tcardRender(p){
  var body=$("tcardBody"); body.textContent="";
  var pr=p.progress||{}, name=p.trainerName||p.displayName||p.handle;
  var head=document.createElement("div"); head.className="tcard-head";
  var av=document.createElement("div"); av.className="tcard-av"; av.textContent=(name||"?").slice(0,2).toUpperCase();
  var who=document.createElement("div");
  var h=document.createElement("h3"); h.textContent=name+(p.isYou?" (you)":"");
  var sub=document.createElement("div"); sub.className="tcard-sub"; sub.textContent="Lv "+pr.level+" · "+(pr.rank||"")+" · @"+p.handle+(p.crew ? " · ["+p.crew.tag+"] "+p.crew.name : "");
  who.appendChild(h); who.appendChild(sub); head.appendChild(av); head.appendChild(who); body.appendChild(head);
  if(typeof cosFrameApply==="function") cosFrameApply(av, (p.cos||{}).frame);          // the name frame they wear
  var bar=document.createElement("div"); bar.className="tcard-bar"; var fill=document.createElement("i");
  fill.style.width=Math.round(100*(pr.xpIntoLevel||0)/Math.max(1,pr.xpForLevel||1))+"%"; bar.appendChild(fill); body.appendChild(bar);
  var xp=document.createElement("div"); xp.className="tcard-sub"; xp.textContent=(pr.xpIntoLevel|0)+" / "+(pr.xpForLevel|0)+" XP to Lv "+((pr.level|0)+1)+" · "+(pr.sessionXp|0)+" from sessions, "+(pr.gameXp|0)+" from games";
  body.appendChild(xp);
  var stats=document.createElement("div"); stats.className="tcard-stats";
  function stat(label, val){ var d=document.createElement("div"); var s=document.createElement("small"); s.textContent=label; var b=document.createElement("strong"); b.textContent=val; d.appendChild(s); d.appendChild(b); stats.appendChild(d); }
  var t=p.totals||{}, g=p.games||{};
  stat("Streak", (p.streak|0)+" days"); stat("Games", t.played|0); stat("Wins", t.wins|0); stat("Podiums", t.podiums|0);
  if(g.kart && g.kart.bestLap!=null) stat("Best kart lap", progMs(g.kart.bestLap));
  if(g.fps && g.fps.kills!=null) stat("Blaster K/D", g.fps.kd);
  body.appendChild(stats);
  var th=document.createElement("div"); th.className="tcard-sec"; th.textContent="Trophies"; body.appendChild(th);
  var tr=document.createElement("div"); tr.className="chips";
  if(!(p.trophies||[]).length){ var none=document.createElement("span"); none.className="muted"; none.textContent="None yet: finish a multiplayer game in the Valley."; tr.appendChild(none); }
  (p.trophies||[]).forEach(function(x){ var c=document.createElement("span"); c.className="tcard-trophy"; c.textContent="🏆 "+x.name; tr.appendChild(c); });
  body.appendChild(tr);
  (window.TCARD_EXTRAS||[]).forEach(function(f){ try{ f(body, p); }catch(e){} });
}

/* ---- per-game leaderboards (Arena view) ---- */
var GB = {game:"kart", rallyKey:null};
// Valley Rally boards are per stage (key "c-" + 12 hex): the game remembers the stages you
// raced in rooms on this browser (games/rally.js rememberKey), and the board shows one at a time.
function gbRallyKeys(){
  var list=[]; try { list=JSON.parse(localStorage.getItem("hq_rally_keys")||"[]"); } catch(e){ list=[]; }
  return (Array.isArray(list)?list:[]).filter(function(e){ return Array.isArray(e) && /^c-[0-9a-f]{12}$/.test(String(e[0])) && typeof e[1]==="string"; }).slice(0,12);
}
function gbLoad(){
  var box=$("gbBody"); if(!box) return;
  var url="/api/arena/leaderboards?game="+GB.game, rkeys=null;
  if(GB.game==="rally"){
    rkeys=gbRallyKeys();
    if(!rkeys.length){ box.textContent=""; var nk=document.createElement("p"); nk.className="muted"; nk.textContent="Race a Valley Rally stage in an Arena room and its board shows up here."; box.appendChild(nk); return; }
    if(!GB.rallyKey || !rkeys.some(function(k){ return k[0]===GB.rallyKey; })) GB.rallyKey=rkeys[0][0];
    url+="&key="+encodeURIComponent(GB.rallyKey);
  }
  box.innerHTML='<p class="muted">Loading…</p>';
  progGet(url).then(function(res){
    box.textContent="";
    if(rkeys){
      var lab=document.createElement("label"); lab.className="gb-key"; lab.textContent="Stage ";
      var sel=document.createElement("select"); sel.className="gb-stage";
      rkeys.forEach(function(k){ var o=document.createElement("option"); o.value=k[0]; o.textContent=k[1]; if(k[0]===GB.rallyKey) o.selected=true; sel.appendChild(o); });
      sel.addEventListener("change", function(){ GB.rallyKey=sel.value; gbLoad(); });
      lab.appendChild(sel); box.appendChild(lab);
    }
    if(!res.ok){ var e=document.createElement("p"); e.className="muted";
      e.textContent = res.code===404 ? "This Arena doesn't keep game boards yet: ask its owner to update it." : (res.j.error||"Pair with an Arena to see the boards."); box.appendChild(e); return; }
    var boards=res.j.boards||[];
    if(!boards.length){ var n=document.createElement("p"); n.className="muted"; n.textContent="No finished games yet. Race, run, putt or blast with friends in the Valley and the times land here."; box.appendChild(n); return; }
    boards.forEach(function(bd){
      var rk = rkeys ? rkeys.filter(function(k){ return k[0]===bd.key; })[0] : null;
      var h=document.createElement("h4"); h.className="gb-key"; h.textContent=bd.key==="match" || bd.key==="all" ? "All races" : GB.game==="td" ? String(bd.key).replace("-", " · ") : rk ? rk[1] : bd.key; box.appendChild(h);
      var tbl=document.createElement("table"); tbl.className="gb-table";
      var cols = GB.game==="td" ? ["#","Trainer","Waves","Played"] : GB.game==="bowl" ? ["#","Trainer","Best","Wins","Played"]
               : GB.game==="type" ? ["#","Trainer","Best WPM","Accuracy","Wins"] : GB.game==="fps" ? ["#","Trainer","Kills","K/D","Played"] : GB.game==="golf" ? ["#","Trainer","Best (strokes)","Wins","Played"]
               : GB.game==="kart" ? ["#","Trainer","Best race","Best lap","Wins"] : ["#","Trainer","Best time","Wins","Played"];
      var thead=document.createElement("tr"); cols.forEach(function(c){ var th=document.createElement("th"); th.textContent=c; thead.appendChild(th); }); tbl.appendChild(thead);
      bd.entries.forEach(function(e){
        var tr=document.createElement("tr"); if(e.isYou) tr.className="you";
        var vals = GB.game==="td" ? [e.rank, null, e.best, e.played] : GB.game==="bowl" ? [e.rank, null, e.best, e.wins, e.played]
                 : GB.game==="type" ? [e.rank, null, e.wpm, e.acc+"%", e.wins] : GB.game==="fps" ? [e.rank, null, e.kills, e.kd, e.played] : GB.game==="golf" ? [e.rank, null, e.best, e.wins, e.played]
                 : GB.game==="kart" ? [e.rank, null, progMs(e.best), progMs(e.bestLap), e.wins] : [e.rank, null, progMs(e.best), e.wins, e.played];
        vals.forEach(function(v, i){
          var td=document.createElement("td");
          if(i===1){ var b=document.createElement("button"); b.type="button"; b.className="gb-who"; b.textContent=e.user.displayName||e.user.handle; b.addEventListener("click", function(){ tcardOpen(e.user.userId); }); td.appendChild(b); }
          else td.textContent=String(v);
          tr.appendChild(td);
        });
        tbl.appendChild(tr);
      });
      var wrap=document.createElement("div"); wrap.className="gb-scroll"; wrap.appendChild(tbl); box.appendChild(wrap);
    });
  }).catch(function(){ box.innerHTML='<p class="muted">The Arena didn\'t answer.</p>'; });
}
function gbTabs(){
  var t=$("gbTabs"); if(!t) return; t.textContent="";
  PROG_GAMES.forEach(function(g){
    var b=document.createElement("button"); b.type="button"; b.className="hbtn ghost"; b.textContent=g[1];
    b.setAttribute("aria-pressed", GB.game===g[0] ? "true" : "false");
    b.addEventListener("click", function(){ GB.game=g[0]; gbTabs(); gbLoad(); });
    t.appendChild(b);
  });
}
(function(){
  var c=$("tcardClose"); if(c) c.addEventListener("click", tcardClose);
  var back=$("tcardBack"); if(back) back.addEventListener("click", function(e){ if(e.target===back) tcardClose(); });
  document.addEventListener("keydown", function(e){ if(e.key==="Escape" && back && back.classList.contains("open")){ tcardClose(); } });
  var mine=$("hqMyCard"); if(mine) mine.addEventListener("click", function(){ tcardOpen("me"); });
  var gbr=$("gbRefresh"); if(gbr) gbr.addEventListener("click", gbLoad);
  gbTabs();
})();
