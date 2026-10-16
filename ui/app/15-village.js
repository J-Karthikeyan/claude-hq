/* ================= Village (base + army, driven by real stats) ================= */
function troopPower(s){ // original "power" score from real activity
  var tk=s.tokens||{}; var out=tk.output||0; var pr=s.promptCount||0;
  var stg=creatureStage(s.creature||{});
  return Math.round(out/1500 + pr*4 + stg*25 + (s.status==="working"?15:0));
}
var VILLAGE_STATUS={working:["⚔","on the attack"],waiting:["🛡","awaiting orders"],idle:["💤","resting"],stale:["🏚","retired"]};
// ---- Overflow Vial: lift a species one stage from the shared bank ----
// The server only debits its local ledger (vial.json); the stage floor itself is the
// existing New Game+ DEX_MAX high-water, so the effect stays purely local like any other floor.
var VIAL_COSTS={0:30, 1:90, 2:180, 3:400};   // fallback mirror of dashboard.VIAL_STAGE_COST
function vialCostFor(from, vial){
  var costs=(vial && vial.costs) || VIAL_COSTS, c=costs!=null ? costs[from] : null;
  return (typeof c==="number" && c>0) ? (c|0) : (VIAL_COSTS[from]|0);
}
function vialLiftBtn(x, cr, vxp, vial){
  var sid=x.sessionId||x.id||x.exampleSessionId;
  if(!sid || !/^[0-9a-fA-F-]{36}$/.test(String(sid))) return null;
  var from=creatureStage(cr);                 // floored visible stage, 0..4
  if(!(from>=0 && from<4)) return null;       // already Apex: nothing left to lift
  var cost=vialCostFor(from, vial); if(!cost) return null;
  var nextNm=STAGE_NAMES[from+1]||("stage "+(from+1));
  var b=el("button","vil-vial-btn"); b.type="button";
  b.textContent="⚗ "+cost;
  var short=vxp<cost;
  b.disabled=short;
  b.title=short ? ("Overflow Vial: need "+cost+" XP to lift this species to "+nextNm+" (you have "+vxp+")")
                : ("Overflow Vial: spend "+cost+" XP to lift this species to "+nextNm);
  b.setAttribute("aria-label", b.title);
  b.addEventListener("click", function(ev){ ev.stopPropagation();
    // ensure the payload object the floor update writes back to is the session's own card
    vialLift({sessionId:sid, id:sid, creature:cr, sessions:STATE.sessions, promptCount:x.promptCount||0,
              tokens:(x.tokens||{}), status:(x.status||"stale"), title:(x.title||x.alias||creatureSpecies(cr))},
             cr, from, cost); });
  return b;
}
// "30m" / "6h 30m" for a vial drop's fuse (seconds); a dying drop reads "<1m"
function vialTtlFmt(sec){ sec=Math.max(0,Math.floor(sec||0)); if(sec<=0) return "—";
  var h=Math.floor(sec/3600), m=Math.round((sec%3600)/60);
  if(m===60){ h+=1; m=0; } if(h>0) return h+"h"+(m?" "+m+"m":"");
  if(m>0) return m+"m"; return "<1m"; }
// live countdown on the Vial chip — the nearest live deposit's fuse (each drop dies 24 h
// after it landed), refreshed every 30 s; the seconds never enter partySig, so the party
// doesn't restart
var vialCdTicker=null, _lastVialT="";
function vialChipTick(){
  var b=$("vilVial"); if(!b) return;
  var vial=((typeof STATE!=="undefined"&&STATE&&STATE.vial)||{});
  var src=(vial&&vial.src)||[];
  if(!src.length){ b.textContent=fmtTok(vial.xp|0); if(_lastVialT!==""){ _lastVialT=""; b.title="Overflow Vial: empty — it fills as Apex creatures keep working"; } return; }
  var tLev=Math.min.apply(null, src.map(function(r){ return r.ttl||0; }));
  var t=vialTtlFmt(tLev);
  var want=fmtTok(vial.xp|0)+" · "+t+"⌛";
  // renderVillage rewrites the chip on every payload refresh, so don't trust a cached
  // "unchanged" value — rewrite whenever the rendered text isn't the countdown we expect
  if(t===_lastVialT && b.textContent===want) return;
  _lastVialT=t;
  b.textContent=want;
  b.title="Overflow Vial: "+(vial.xp|0)+" XP banked (the soonest-to-die "+(src[0].xp|0)+" XP drop dies in "+t+"). Click for the breakdown.";
}
function vialChipStart(){ if(vialCdTicker) return; vialCdTicker=setInterval(vialChipTick,30000); }
// Overflow Vial: the breakdown — lifetime poured per Apex creature, live drops with their fuse
function renderVialPanel(){
  var body=$("vilVialBody"); if(!body) return;
  var vial=((typeof STATE!=="undefined"&&STATE&&STATE.vial)||{});
  body.innerHTML="";
  var sess=((typeof STATE!=="undefined"&&STATE&&STATE.sessions)||[]);
  var head=el("div","vil-vial-head"), hid=el("b"), hxp=el("span");
  hid.textContent="Live in the vial"; hxp.textContent=fmtTok(vial.xp|0)+" XP";
  head.appendChild(hid); head.appendChild(hxp); body.appendChild(head);
  var sub=el("div","vil-vial-sub");
  sub.textContent="Half the XP an Apex creature keeps earning past 70 prompts, banked for 24 h — spends eat the oldest drop first.";
  body.appendChild(sub);
  var h2=el("div","vil-vial-h"); h2.textContent="Poured by creature (season, ever)"; body.appendChild(h2);
  var byIdx={};
  sess.forEach(function(x){ var p=x.vialPoured|0; if(p>0){ var k=pokeIdx(x.creature||{}); byIdx[k]=(byIdx[k]||0)+p; } });
  var keys=Object.keys(byIdx).sort(function(a,b){ return byIdx[b]-byIdx[a]; });
  var dexSp={};
  if(typeof POKEDEX!=="undefined"&&POKEDEX&&POKEDEX.species) POKEDEX.species.forEach(function(sp){ if(sp) dexSp[pokeIdx(sp)]=sp; });
  var list=el("div","vil-vial-list");
  var pname=function(i){ return (Object.prototype.hasOwnProperty.call(POKE_NAMES,i)?POKE_NAMES[i]:("Species "+i)); };
  if(!keys.length) list.innerHTML='<div class="feed-empty">No Apex creatures have poured yet.</div>';
  keys.slice(0,20).forEach(function(k){
    var row=el("div","vil-vial-row"), nm=el("span","vil-vial-nm"), xp=el("span","vil-vial-xp"), bar=el("span","vil-vial-bar");
    nm.textContent=pname(+k);
    xp.textContent=fmtTok(byIdx[k]|0)+" XP"; bar.style.width=Math.min(100,100*(byIdx[k]/(byIdx[keys[0]]||1)))+"%";
    row.appendChild(bar); row.appendChild(nm); row.appendChild(xp); list.appendChild(row);
  });
  body.appendChild(list);
  var h3=el("div","vil-vial-h"); h3.textContent="Live drops · each dies 24 h after it lands"; body.appendChild(h3);
  var dl=el("div","vil-vial-rows"), src=((vial&&vial.src)||[]);
  if(!src.length) dl.innerHTML='<div class="feed-empty">No live drops — the vial only holds what Apex creatures dripped in the last 24 h.</div>';
  src.forEach(function(r){
    var row=el("div","vil-vial-row2"), rx=el("b"), rt=el("span"), rb=el("span","vil-vial-bar");
    rx.textContent=(r.xp|0)+" XP"; rt.textContent=vialTtlFmt(r.ttl)+" ⌛";
    rb.style.width=Math.max(2,Math.min(100,100*((r.ttl|0)/(vial.expiresIn||86400))))+"%";
    row.appendChild(rb); row.appendChild(rx); row.appendChild(rt); dl.appendChild(row);
  });
  body.appendChild(dl);
  var f=el("div","vil-vial-sub"); f.textContent="A spend takes the soonest-to-die drop first."; body.appendChild(f);
}
function villageVialToggle(){
  var p=$("vilVialPanel"); if(!p) return;
  var on=p.hasAttribute("hidden");
  if(on){ p.removeAttribute("hidden"); p.setAttribute("aria-hidden","false"); renderVialPanel(); }
  else { p.setAttribute("hidden",""); p.setAttribute("aria-hidden","true"); }
}
function vialLift(x, cr, from, cost){
  var sid=x.sessionId||x.id, nm=creatureSpecies(cr);
  var nextNm=STAGE_NAMES[from+1]||("stage "+(from+1));
  if(!confirm("Spend "+cost+" XP from the Overflow Vial to lift "+nm+" to "+nextNm+"?")) return;
  fetch("/api/vial/spend",{method:"POST",headers:{"Content-Type":"application/json","X-HQ-Token":CSRF},
    body:JSON.stringify({sessionId:sid, fromStage:from})})
    .then(function(r){ return r.json().then(function(j){ return {status:r.status, j:(j||{})}; }); })
    .then(function(res){
      var j=res.j;
      if(!j.ok){ toast("⚗ "+(j.error||"the vial refused this"),"ach"); announce("Overflow Vial: "+(j.error||"refused")); return; }
      // the spent species' high-water may be the dex entry, not a live session: refresh BOTH
      // (key on the species slot, same grid the seed/dexCrOf loop reads)
      var k=pokeIdx(cr);
      dexHighWater(k, from+1);       // New Game+ floor, same machinery as any other high-water
      if(typeof POKEDEX!=="undefined" && POKEDEX){
        POKEDEX.species.forEach(function(sp){ if(sp && pokeIdx(sp)===k) sp.maxStage=Math.max(sp.maxStage|0, from+1); });
      }
      if(STATE && STATE.vial) STATE.vial.xp=j.balance|0;
      PARTY_SIG=null;
      if(STATE && STATE.sessions) renderParty(STATE.sessions);
      if(typeof POKEDEX!=="undefined" && POKEDEX) renderPokedex(POKEDEX);
      if(VIEW==="village") renderVillage();
      if(VIEW==="pokedex") renderPokedex(POKEDEX);
      toast("⚗ "+nm+" is now "+nextNm+"!","level");
      logEvent("⚗", nm+" lifted to "+nextNm+" from the Overflow Vial");
      announce(nm+" lifted to "+nextNm);
      confettiBurst();
      try{ playChime(); }catch(e){}
    })
    .catch(function(){ toast("⚗ could not reach the vial","ach"); });
}
function renderVillage(){
  var s=(STATE&&STATE.season)||{}, sess=(STATE&&STATE.sessions)||[];
  var me=(typeof myLevel==="function") ? myLevel() : s;
  var lvl=me.level!=null?me.level:1;
  $("vilTHBadge").textContent=lvl; $("vilTHsub").textContent="Level "+lvl+" · "+(me.rank||"Adventurer");
  // resources from real stats
  var gold=0, mana=0; sess.forEach(function(x){ var tk=x.tokens||{}; gold+=(tk.estCostUSD||0); mana+=(tk.output||0); });
  var ember=(s.totals&&s.totals.prompts)||0;
  $("vilGold").textContent=fmtCost(gold).replace("$",""); $("vilMana").textContent=fmtTok(mana); $("vilEmber").textContent=fmtTok(ember);
  var vial=((STATE&&STATE.vial)||{}), vxp=(vial.xp|0);
  $("vilVial").textContent=fmtTok(vxp);
  $("vilVial").title = vxp>0 ? ("Overflow Vial: "+vxp+" XP banked (each drop lasts 24 h). Click for the breakdown.")
                             : "Overflow Vial: empty — it fills as Apex creatures keep working";
  vialChipTick(); vialChipStart();
  var _vIt=$("vilVialItem");
  if(_vIt && !_vIt.dataset.wired){ _vIt.dataset.wired="1"; _vIt.addEventListener("click",villageVialToggle);
    _vIt.addEventListener("keydown",function(e){ if(e.key==="Enter"||e.key===" "){ e.preventDefault(); villageVialToggle(); } }); }
  var _vX=$("vilVialX");
  if(_vX && !_vX.dataset.wired){ _vX.dataset.wired="1"; _vX.addEventListener("click",function(){ var p=$("vilVialPanel"); if(p){ p.setAttribute("hidden",""); p.setAttribute("aria-hidden","true"); } }); }
  var pct=Math.max(0,Math.min(100,s.pct||0));
  $("vilXP").style.width=pct+"%";
  $("vilXPtext").textContent=(s.xpIntoLevel||0).toLocaleString()+" / "+(s.xpForLevel||0).toLocaleString()+" XP to TH "+(lvl+1);

  // ---- Army Camp ----
  var army=sess.slice().map(function(x){ return {s:x, pw:troopPower(x)}; }).sort(function(a,b){ return b.pw-a.pw; });
  var camp=$("vilArmy"); camp.innerHTML="";
  $("vilArmySub").textContent=army.length+" troop"+(army.length===1?"":"s")+" · total power "+army.reduce(function(a,b){return a+b.pw;},0);
  if(!army.length){ camp.innerHTML='<div class="feed-empty">No troops yet — start a Claude session to recruit one.</div>'; }
  // Show EVERY troop (was top-30-by-power): a 0/1-prompt Egg/Hatchling has near-zero power
  // and fell below the cut, hiding creatures the Overflow Vial needs to be able to lift.
  army.forEach(function(a){
    var x=a.s, cr=x.creature||{}, st=VILLAGE_STATUS[x.status]||VILLAGE_STATUS.idle;
    var card=el("div","vil-troop status-"+(x.status||"idle"));
    var av=el("div","vil-troop-av"); paintCreature(av, cr, 56, true); card.appendChild(av);
    var nm=el("div","vil-troop-nm"); nm.textContent=creatureSpecies(cr); card.appendChild(nm);
    var lv=el("div","vil-troop-lv"); lv.textContent=creatureStageName(cr)+(cr.shiny?" ✦":""); card.appendChild(lv);
    var pw=el("div","vil-troop-pw"); pw.innerHTML='<span class="vil-sword">⚔</span>'+a.pw; card.appendChild(pw);
    var stt=el("div","vil-troop-st"); stt.textContent=st[0]+" "+(x.title||x.alias||"session"); stt.title=st[1]; card.appendChild(stt);
    var vb=vialLiftBtn(x, cr, vxp, vial); if(vb) card.appendChild(vb);
    card.addEventListener("click",function(){ if(typeof openDrawer==="function") openDrawer(x); });
    camp.appendChild(card);
  });

  // ---- Builder Base (projects as buildings) ----
  var byFolder={}; sess.forEach(function(x){ var f=x.folder||"(no project)"; (byFolder[f]=byFolder[f]||[]).push(x); });
  var builds=Object.keys(byFolder).map(function(f){ var arr=byFolder[f];
    var prompts=arr.reduce(function(a,b){return a+(b.promptCount||0);},0);
    return {f:f, n:arr.length, prompts:prompts, blvl:Math.max(1,Math.min(12,1+Math.floor(prompts/25)))}; })
    .sort(function(a,b){ return b.prompts-a.prompts; });
  var bwrap=$("vilBuild"); bwrap.innerHTML="";
  if(!builds.length){ bwrap.innerHTML='<div class="feed-empty">No buildings yet.</div>'; }
  builds.slice(0,12).forEach(function(b){
    var name=b.f.split("/").pop()||b.f;
    var t=el("div","vil-bld");
    t.innerHTML='<div class="vil-bld-roof"></div><div class="vil-bld-body"><span class="vil-bld-lv">Lv '+b.blvl+'</span></div>';
    var lab=el("div","vil-bld-nm"); lab.textContent=name; lab.title=b.f+" · "+b.n+" sessions · "+b.prompts+" prompts"; t.appendChild(lab);
    t.addEventListener("click",function(){ if(typeof openProject==="function") openProject(b.f); });
    bwrap.appendChild(t);
  });

  // ---- War Log (from event log) ----
  var war=$("vilWar"); war.innerHTML="";
  var evs=(typeof EVENTS!=="undefined"&&EVENTS)?EVENTS.slice(0,12):[];
  if(!evs.length){ war.innerHTML='<div class="feed-empty">No battles logged yet.</div>'; }
  evs.forEach(function(e){ var r=el("div","vil-war-row");
    r.innerHTML='<span class="vil-war-ic">'+esc(e.icon||"•")+'</span><span class="vil-war-tx">'+esc(e.text||"")+'</span><span class="vil-war-t">'+esc(relTime(e.t))+'</span>';
    war.appendChild(r); });
}
// ---- Raid: a self-contained deterministic skirmish from your army power ----
function villageRaid(){
  var sess=(STATE&&STATE.sessions)||[];
  var army=sess.map(troopPower).sort(function(a,b){return b-a;});
  var power=army.reduce(function(a,b){return a+b;},0);
  var ov=$("vilRaidOverlay"); if(!ov) return;
  var stage=$("vilRaidStage"); stage.innerHTML="";
  var top=sess.slice().map(function(x){return {s:x,pw:troopPower(x)};}).sort(function(a,b){return b.pw-a.pw;}).slice(0,6);
  top.forEach(function(a,i){ var m=el("div","vil-march"); m.style.animationDelay=(i*0.12)+"s";
    paintCreature(m, a.s.creature||{}, 54, false); stage.appendChild(m); });
  // defenders + result (deterministic, no RNG banned APIs)
  var defense=Math.round(power*0.7 + 40 + (top.length*11));
  var ratio=power/(defense||1);
  var stars = ratio>=1.4?3 : ratio>=1.0?2 : ratio>=0.6?1 : 0;
  var loot = Math.round(power*3.5*(0.5+stars*0.25));
  var res=$("vilRaidResult");
  var starRow=""; for(var i=0;i<3;i++) starRow+='<span class="vil-star '+(i<stars?"on":"")+'">★</span>';
  var verdict = stars>=2?"Victory!" : stars===1?"Partial raid" : "Raid repelled";
  res.innerHTML='<div class="vil-raid-stars">'+starRow+'</div><div class="vil-raid-verdict">'+verdict+'</div>'+
    '<div class="vil-raid-loot">Power '+power+' vs Defense '+defense+' · +'+fmtTok(loot)+' loot</div>';
  ov.classList.add("on"); ov.setAttribute("aria-hidden","false");
  if(!calmMode()) confettiBurst();
  try{ playChime(); }catch(e){}
  if(stars>=1) logEvent("⚔", verdict+" — "+stars+"★ raid ("+fmtTok(loot)+" loot)");
}
function villageRaidClose(){ var ov=$("vilRaidOverlay"); if(ov){ ov.classList.remove("on"); ov.setAttribute("aria-hidden","true"); } if(VIEW==="village") renderVillage(); }
(function(){ var r=$("vilRaid"); if(r) r.addEventListener("click",villageRaid);
  var c=$("vilRaidClose"); if(c) c.addEventListener("click",villageRaidClose);
  var ov=$("vilRaidOverlay"); if(ov) ov.addEventListener("click",function(e){ if(e.target===ov) villageRaidClose(); }); })();

var EVO_TIMER=null, EVO_MORPH_TIMERS=[];
function clearEvoMorph(){ EVO_MORPH_TIMERS.forEach(clearTimeout); EVO_MORPH_TIMERS=[]; var n=$("evoCreature"); if(n) n.classList.remove("morphing"); }
// Sprite HTML at an EXPLICIT stage (bypasses the New Game+ floor via _noFloor), reusing
// the existing sprite helpers — nothing new is drawn.
function spriteHTMLAtStage(cr, stage, px){ var c=Object.assign({},cr,{stage:Math.max(0,Math.min(4,stage|0)),_noFloor:true}); return creatureVisual({creature:c}, px); }
function pokeNameAtStage(cr, stage){ if(!isPokePack()) return creatureSpecies(cr);
  return pokeCurrentName(Object.assign({},cr,{stage:Math.max(0,Math.min(4,stage|0)),_noFloor:true})); }
// Classic evolving animation: stack the pre-form and evolved sprite, cross-toggle them at
// an ACCELERATING pace under a pulsing white-out, then reveal. Returns ms until reveal.
function playEvoMorph(node, cr, fromStage, toStage, px, onReveal){
  node.innerHTML='<div class="evo-morph"><span class="evo-pre">'+spriteHTMLAtStage(cr,fromStage,px)+
    '</span><span class="evo-post" style="opacity:0">'+spriteHTMLAtStage(cr,toStage,px)+'</span></div>';
  var pre=node.querySelector(".evo-pre"), post=node.querySelector(".evo-post");
  if(calmMode()){ if(pre) pre.style.opacity=0; if(post) post.style.opacity=1; if(onReveal) onReveal(); return 0; }
  node.classList.add("morphing");
  var mega=$("evoSplash").classList.contains("mega");
  var sched=mega?[520,470,420,370,320,280,240,205,175,150,130,110,95,82,72,64,58,54,50,48]
                :[600,540,480,420,370,320,280,240,205,175,150,130,112,96,84,74,66,60];
  var acc=0, showPost=false;
  sched.forEach(function(d){ acc+=d; EVO_MORPH_TIMERS.push(setTimeout(function(){ showPost=!showPost;
    if(pre) pre.style.opacity=showPost?0:1; if(post) post.style.opacity=showPost?1:0; }, acc)); });
  EVO_MORPH_TIMERS.push(setTimeout(function(){ node.classList.remove("morphing");
    if(pre) pre.style.opacity=0; if(post){ post.style.opacity=1; post.classList.add("evo-reveal"); }
    if(onReveal) onReveal(); }, acc+140));
  return acc+140;
}
// Announce a notable evolution to everyone on the Arena leaderboard (throttled fan-out over
// the existing nudge mechanism — the server has no broadcast primitive). Fire-and-forget.
// Broadcast a NOTABLE evolution to everyone currently in the Arena lobby as a real "evo" event,
// so their HQ shows the same full-screen evolution splash (not just a nudge). Rides the existing
// "say" relay (server stamps m.from), so no server change is needed. Carries a resolved sprite
// descriptor so peers render the exact evolved/mega form regardless of their own creature pack.
function arenaAnnounceEvo(s, event){
  if(!ARENA.sock || ARENA.sock.readyState!==1) return;   // must be in the lobby to show peers a live splash
  var mega=(event==="mega"), cr=s.creature||{}, name=isPokePack()?pokeCurrentName(cr):creatureSpecies(cr);
  var spr=null;
  if(isPokePack()){
    if(mega){ var mf=pokeMegaForm(cr)||{}; spr={dex:branchFinalDex(cr), slug:mf.slug||"", dir:mf.dir||"xy", shiny:cr.shiny?1:0, mega:1}; }
    else { spr={dex:pokeDexFor(cr), slug:String(name).toLowerCase().replace(/[^a-z0-9]/g,""), dir:"xy", shiny:cr.shiny?1:0, mega:0}; }
  }
  var pre = isPokePack() ? pokeNameAtStage(cr, (typeof STAGE_PREV!=="undefined"&&STAGE_PREV[s.sessionId||s.id]?STAGE_PREV[s.sessionId||s.id].d:3)) : "";
  try{ ARENA.sock.send(JSON.stringify({type:"say", data:{kind:"evo", event:event||"evolve", mega:!!mega, shiny:cr.shiny?1:0, name:name, pre:pre, spr:spr}})); }catch(e){}
  logEvent("📣", "Announced "+(event==="shiny"?("shiny "+name):name+(mega?" (Mega)":""))+" to the Arena lobby");
}
// A specific evolved/mega sprite from a peer's descriptor — pack-independent, with the usual
// onerror fallback chain (X/Y or ORAS gif -> HOME still png -> generated monster).
function peerEvoImg(spr, px){
  var shiny=spr.shiny?1:0, slug=String(spr.slug||"").toLowerCase().replace(/[^a-z0-9-]/g,""), dex=spr.dex|0;
  if(!slug) return '<div style="font-size:'+Math.round(px*0.6)+'px" aria-hidden="true">✨</div>';
  var base = spr.dir==="oras" ? (shiny?PKPARAISO_ORAS_SHINY:PKPARAISO_ORAS) : (shiny?PKPARAISO_XY_SHINY:PKPARAISO_XY);
  var fb=encodeURIComponent(JSON.stringify([hashStr(slug),220,4,shiny,px]));
  return '<img class="pokeimg pokeimg-3d'+(spr.mega?" pokeimg-mega":"")+'" src="'+base+slug+'.gif" width="'+px+'" height="'+px+'"'+
    ' alt="'+esc(slug)+'" data-dex="'+dex+'" data-shiny="'+shiny+'" data-3d="1" data-mega="'+(spr.mega?1:0)+'"'+
    ' data-slug="'+slug+'" data-try="0" data-fb="'+fb+'" onerror="pokeErr(this)">';
}
// A full-screen splash takes over the page, so a member can show one at most every
// PEER_EVO_GAP_MS (a burst of evolutions, or a misbehaving client, can't keep covering the screen).
var PEER_EVO_GAP_MS=60000, PEER_EVO_LAST={};
function peerEvoAllowed(userId, now){
  var k=(typeof userId==="string" && userId) ? userId : "?";
  var last=PEER_EVO_LAST[k];
  if(last!=null && now-last < PEER_EVO_GAP_MS) return false;
  PEER_EVO_LAST[k]=now;
  return true;
}
// A peer's evolution arrived over the lobby -> show them the full-screen splash + a Congratulate button.
function arenaOnEvo(m){
  var d=m.data||{}; if(!d || d.kind!=="evo") return;
  var from=(m.from&&typeof m.from==="object")?m.from:{};
  if(ARENA.you && from.userId && from.userId===ARENA.you.userId) return;   // ignore my own echo
  if(!peerEvoAllowed(from.userId, Date.now())) return;   // one splash per member per minute
  showPeerEvoSplash({
    who:arenaWho(from), handle:(typeof from.handle==="string"?from.handle.slice(0,40):""),
    event:(typeof d.event==="string"?d.event:(d.mega?"mega":"evolve")),
    mega:!!d.mega, shiny:!!d.shiny, name:String(d.name||"A creature").slice(0,40),
    pre:(typeof d.pre==="string"?d.pre.slice(0,40):""),
    spr:(d.spr&&typeof d.spr==="object")?d.spr:null
  });
}
function showPeerEvoSplash(p){
  var el2=$("evoSplash"); if(!el2) return;
  clearEvoMorph();
  var isShiny=(p.event==="shiny");
  el2.classList.toggle("mega", !!p.mega); el2.classList.add("peer"); el2.classList.toggle("shiny", isShiny);
  var size=Math.min(p.mega?360:300, Math.round(window.innerHeight*(p.mega?0.46:0.4)));
  $("evoCreature").innerHTML = p.spr ? peerEvoImg(p.spr, size) : '<div style="font-size:'+Math.round(size*0.6)+'px" aria-hidden="true">✨</div>';
  $("evoKicker").textContent = isShiny ? "✨ SHINY UNLOCKED ✨" : "📣 In the Arena";
  $("evoTitle").textContent  = isShiny ? (p.who+" found a Shiny "+p.name+"!") : (p.who+"'s "+p.name+"!");
  $("evoSub").textContent    = isShiny ? ("A shiny "+p.name+" joined "+p.who+"'s team! ✨")
    : p.mega ? (p.who+"'s creature just MEGA-EVOLVED! ⚡")
    : (p.pre ? (p.who+"'s "+p.pre+" evolved into "+p.name+"! ✨") : (p.who+"'s creature evolved into "+p.name+"! ✨"));
  var cg=$("evoCongrat");
  if(cg){ cg.style.display=""; cg.disabled=false; cg.textContent="🎉 Congratulate";
    cg.onclick=function(){ arenaCongratulate(p); cg.disabled=true; cg.textContent="🎉 Congratulated!"; }; }
  el2.classList.add("on"); el2.setAttribute("aria-hidden","false");
  if(!calmMode()){ el2.classList.remove("flash"); void el2.offsetWidth; el2.classList.add("flash"); confettiBurst(); if(p.mega) setTimeout(confettiBurst,450); }
  try{ playChime(); }catch(e){}
  if(cg) cg.focus(); else $("evoClose").focus();
  clearTimeout(EVO_TIMER); EVO_TIMER=setTimeout(closeEvoSplash, p.mega?9000:8000);
}
// Send the evolving trainer a congrats — a targeted nudge (reaches them even if offline),
// falling back to a lobby chat line if we only have their display name.
function arenaCongratulate(p){
  var note="🎉 Congrats on "+(p.name||"your evolution")+"!";
  if(p.handle){ try{ arenaPost("/api/arena/nudge",{toHandle:p.handle, note:note}); }catch(e){} }
  else if(ARENA.sock && ARENA.sock.readyState===1){ try{ ARENA.sock.send(JSON.stringify({type:"say", data:{kind:"chat", text:note}})); }catch(e){} }
  toast("🎉 Congrats sent to "+(p.who||"them"),"level");
  logEvent("🎉","Congratulated "+(p.who||"a trainer")+" on "+(p.name||"their evolution"));
}
function showEvoSplash(s, fromStage){
  var el2=$("evoSplash"); if(!el2) return;
  var cr=s.creature||{};
  var mega = !!pokeMega(cr);   // reached Apex with an eligible Mega form (3D pack)
  var species=creatureSpecies(cr), stageNm=creatureStageName(cr), nm=isPokePack()?pokeCurrentName(cr):species;
  // the celebration shows the creature at its best: no tired/fainted look here
  var crShow=Object.assign({}, cr); delete crShow.fatigue;
  var size=Math.min(mega?380:320, Math.round(window.innerHeight*(mega?0.5:0.42)));
  el2.classList.toggle("mega", mega);
  el2.classList.remove("peer");
  var _cg=$("evoCongrat"); if(_cg) _cg.style.display="none";   // local evolution: no congrat button
  // Start on the "evolving" beat (pre-form name); reveal the evolved identity at morph end.
  $("evoKicker").textContent = "✨ What?! "+pokeNameAtStage(cr,fromStage)+" is evolving! ✨";
  $("evoTitle").textContent = "";
  $("evoSub").textContent = (s.title||s.name||"Session");
  el2.classList.add("on"); el2.setAttribute("aria-hidden","false");
  try{ playChime(); }catch(e){}
  clearEvoMorph();
  playEvoMorph($("evoCreature"), crShow, fromStage, creatureStage(cr), size, function(){
    $("evoKicker").textContent = mega ? "⚡ MEGA EVOLUTION! ⚡" : "✨ Congratulations! ✨";
    $("evoTitle").textContent = mega ? (nm+"!")
      : (isPokePack() ? (pokeNameAtStage(cr,fromStage)+" evolved into "+nm+"!")
                      : (species+" evolved to "+stageNm+"!"));
    $("evoSub").textContent = mega ? ((s.title||s.name||"Session")+" reached its ultimate form")
                                   : ((s.title||s.name||"Session")+" · now "+stageNm);
    if(!calmMode()){ el2.classList.remove("flash"); void el2.offsetWidth; el2.classList.add("flash"); confettiBurst(); if(mega) setTimeout(confettiBurst,450); }
    try{ if(mega) playChime(); }catch(e){}
  });
  if(mega){  // make the moment reach the user even if HQ isn't the active tab
    try{ if(NOTIF_ON && ("Notification" in window) && Notification.permission==="granted"){
      var n=new Notification("⚡ MEGA EVOLUTION", {body:nm+" — "+(s.title||s.name||"a session")+" reached its ultimate form!", tag:"hq-mega-"+(s.id||s.sessionId||""), silent:false});
      n.onclick=function(){ try{ window.focus(); }catch(e){} this.close(); };
    } }catch(e){}
    try{ window.focus(); }catch(e){}
  }
  $("evoClose").focus();
  // Broadcast only NOTABLE events (final form reached, or mega) to be a good Arena citizen.
  // Announce EVERY evolution to the Arena lobby (this fires only on a real visible-form change).
  try{ arenaAnnounceEvo(s, mega?"mega":"evolve"); }catch(e){}
  clearTimeout(EVO_TIMER); EVO_TIMER=setTimeout(closeEvoSplash, mega?11000:10000);  // ~10s cinematic
}
function closeEvoSplash(){ clearTimeout(EVO_TIMER); clearEvoMorph(); var e=$("evoSplash"); if(e){ e.classList.remove("on","flash","mega","peer"); e.setAttribute("aria-hidden","true"); }
  var cg=$("evoCongrat"); if(cg){ cg.style.display="none"; cg.disabled=false; cg.textContent="🎉 Congratulate"; } }
(function(){ var b=$("evoClose"); if(b) b.addEventListener("click",closeEvoSplash);
  var s=$("evoSplash"); if(s) s.addEventListener("click",function(e){ if(e.target===s) closeEvoSplash(); }); })();
// Raw (un-floored) backend growth stage for a session. Defaults to 0 (not 3) when the
// backend omits stage, so a genuinely-new session is distinguished and the New Game+
// floor is never mistaken for organic growth.
function rawStageOf(s){ var c=(s&&s.creature)||{}; return Math.max(0,Math.min(4, c.stage!=null?c.stage|0:0)); }
// Index of the VISIBLE form for a creature: for pokemon packs the position along the evo
// line (so the 5 internal stages that compress onto 2-3 forms don't count as separate
// forms), plus +1 when a Mega is active so reaching Apex-with-mega still registers as a
// new form. Non-poke packs change appearance every stage, so the stage IS the form.
function formIndexOf(cr){ cr=cr||{};
  if(isPokePack()) return pokeEvoPos(cr, pokeEvoLine(cr)) + (pokeMega(cr)?1:0);
  return creatureStage(cr); }
function checkEvolution(sessions){
  sessions = sessions||[];
  function snap(s){ var c=s.creature||{}, rawC=Object.assign({},c,{_noFloor:true});
    return { r:rawStageOf(s), d:creatureStage(c), rf:formIndexOf(rawC), df:formIndexOf(c) }; }
  if(!STAGE_BASE){
    // Seed baselines silently so a page load doesn't burst-fire every existing evolution/shiny.
    sessions.forEach(function(s){ var id=s.sessionId||s.id; if(id){ STAGE_PREV[id]=snap(s); if(s.creature&&s.creature.shiny) SHINY_SEEN[id]=1; } });
    STAGE_BASE=true; return;
  }
  sessions.forEach(function(s){
    var id=s.sessionId||s.id; if(!id) return;
    var cur=snap(s), prev=STAGE_PREV[id];
    // Celebrate ONLY when the VISIBLE FORM advances because THIS session grew: raw form
    // index up AND displayed form index up. This ignores (a) internal stage-ups that don't
    // change the sprite — e.g. Wartortle occupies two stages, so stage 1→2 is NOT an
    // evolution — and (b) a New Game+ floor rising from a sibling/seed (changes df, not rf).
    if(prev && prev.rf!=null && cur.rf>prev.rf && cur.df>prev.df){
      var mg=!!pokeMega(s.creature);
      var postName = isPokePack()? pokeCurrentName(s.creature) : creatureSpecies(s.creature);
      var preName  = isPokePack()? pokeNameAtStage(s.creature, prev.d) : null;
      if(mg){
        toast("⚡ "+postName+"! Mega Evolution!","level");
        logEvent("⚡", postName+" — Mega Evolution ("+(s.title||s.name||"session")+")");
      } else if(isPokePack() && preName && preName!==postName){
        toast("✨ "+preName+" evolved into "+postName+"!","level");
        logEvent("✨", preName+" evolved into "+postName+" ("+(s.title||s.name||"session")+")");
      } else {
        toast("✨ "+postName+" evolved to "+creatureStageName(s.creature)+"!","level");
        logEvent("✨", postName+" evolved to "+creatureStageName(s.creature)+" ("+(s.title||s.name||"session")+")");
      }
      confettiBurst();
      showEvoSplash(s, prev.d);
    }
    // Shiny unlock: a shiny creature newly appeared in the fleet (deterministic per species, so
    // this fires when a NEW shiny session joins). Celebrate locally + announce to the Arena.
    if(s.creature && s.creature.shiny && !SHINY_SEEN[id]){
      SHINY_SEEN[id]=1;
      var snm=isPokePack()?pokeCurrentName(s.creature):creatureSpecies(s.creature);
      toast("✨ Shiny "+snm+" joined your fleet!","ach");
      logEvent("✨","Shiny "+snm+" ("+(s.title||s.name||"session")+")");
      try{ arenaAnnounceEvo(s, "shiny"); }catch(e){}
    }
    STAGE_PREV[id]=cur;
  });
  var live={}; sessions.forEach(function(s){ var id=s.sessionId||s.id; if(id) live[id]=1; });
  Object.keys(STAGE_PREV).forEach(function(k){ if(!live[k]) delete STAGE_PREV[k]; });
  Object.keys(SHINY_SEEN).forEach(function(k){ if(!live[k]) delete SHINY_SEEN[k]; });
}
function confettiBurst(){
  if(calmMode()) return;
  var cv=$("confetti"); if(!cv) return;
  var ctx=cv.getContext("2d");
  var W=cv.width=window.innerWidth, H=cv.height=window.innerHeight;
  cv.classList.add("on");
  var colors=["#6c5cff","#9b8bff","#3ad07f","#f0c74a","#ff6b6f"];
  var parts=[];
  for(var i=0;i<120;i++){
    parts.push({x:W/2+(Math.random()-0.5)*120, y:H*0.3, vx:(Math.random()-0.5)*10,
      vy:(Math.random()-0.9)*11, g:0.32+Math.random()*0.18, w:5+Math.random()*5, h:7+Math.random()*6,
      rot:Math.random()*6.28, vr:(Math.random()-0.5)*0.4, c:colors[i%colors.length]});
  }
  var start=performance.now(), DUR=1200;
  function frame(now){
    var el2=now-start;
    ctx.clearRect(0,0,W,H);
    parts.forEach(function(p){ p.vy+=p.g; p.x+=p.vx; p.y+=p.vy; p.rot+=p.vr;
      ctx.save(); ctx.translate(p.x,p.y); ctx.rotate(p.rot); ctx.globalAlpha=Math.max(0,1-el2/DUR);
      ctx.fillStyle=p.c; ctx.fillRect(-p.w/2,-p.h/2,p.w,p.h); ctx.restore(); });
    if(el2<DUR){ requestAnimationFrame(frame); }
    else { ctx.clearRect(0,0,W,H); cv.classList.remove("on"); }
  }
  requestAnimationFrame(frame);
}

