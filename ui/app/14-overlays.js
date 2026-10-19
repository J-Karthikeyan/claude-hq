/* ---- "welcome back" recap: diff current state vs the last snapshot ---- */
var RECAP_PREV=null, RECAP_DONE=false, RECAP_AWAY_MS=15*60*1000;
try{ RECAP_PREV=JSON.parse(localStorage.getItem("hq_snapshot")||"null"); }catch(e){ RECAP_PREV=null; }
function snapshotNow(){
  var s=(STATE&&STATE.season)||{}, stages={};
  ((STATE&&STATE.sessions)||[]).forEach(function(x){ if(x.sessionId) stages[x.sessionId]=creatureStage(x.creature||{}); });
  var me=(typeof myLevel==="function") ? myLevel() : s;
  return { ts:Date.now(), level:me.level!=null?me.level:0, hq:!!me.hq,
           caught:(TRAINER_DEX&&TRAINER_DEX.caughtCount)||0, stages:stages };
}
function saveSnapshot(){ try{ localStorage.setItem("hq_snapshot", JSON.stringify(snapshotNow())); }catch(e){} }
function dismissRecap(){ $("recapBanner").classList.add("hidden"); saveSnapshot(); }
function maybeRecap(){
  if(RECAP_DONE || !STATE || !TRAINER_DEX) return;   // wait until both are loaded
  RECAP_DONE=true;
  if(!RECAP_PREV || !RECAP_PREV.ts || (Date.now()-RECAP_PREV.ts) < RECAP_AWAY_MS){ saveSnapshot(); return; }
  var cur=snapshotNow(), bits=[];
  var lv=(!!RECAP_PREV.hq===!!cur.hq) ? cur.level-(RECAP_PREV.level||0) : 0;      // never compare a season level with an HQ level if(lv>0) bits.push("leveled up to Lv."+cur.level);
  var caught=cur.caught-(RECAP_PREV.caught||0); if(caught>0) bits.push("caught "+caught+" new species");
  var evolved=0, fresh=0, ps=RECAP_PREV.stages||{};
  Object.keys(cur.stages).forEach(function(sid){
    if(!(sid in ps)) fresh++; else if(cur.stages[sid]>ps[sid]) evolved++;
  });
  if(evolved>0) bits.push(evolved+" session"+(evolved===1?"":"s")+" evolved");
  if(fresh>0) bits.push(fresh+" new session"+(fresh===1?"":"s"));
  var needs=((STATE&&STATE.sessions)||[]).filter(function(x){return x.status==="needs";}).length;
  if(needs>0) bits.push(needs+" tab"+(needs===1?"":"s")+" now need"+(needs===1?"s":"")+" you");
  if(!bits.length){ saveSnapshot(); return; }
  var awayMin=Math.round((Date.now()-RECAP_PREV.ts)/60000);
  var away = awayMin>=1440 ? Math.round(awayMin/1440)+"d" : awayMin>=60 ? Math.round(awayMin/60)+"h" : awayMin+"m";
  $("recapTitle").textContent = "Welcome back — while you were away ("+away+"):";
  $("recapSub").textContent = bits.join(" · ");
  $("recapBanner").classList.remove("hidden");
  saveSnapshot();
}
$("recapX").addEventListener("click",dismissRecap);
// keep the snapshot fresh while active so a quick refresh doesn't trigger a recap
setInterval(saveSnapshot, 60000);
window.addEventListener("beforeunload", saveSnapshot);
document.addEventListener("visibilitychange", function(){ if(document.hidden) saveSnapshot(); });

/* ---- command palette ---- */
var CMDK_OPEN=false, CMDK_SEL=0, CMDK_ITEMS=[];
function buildCmdkItems(q){
  q=(q||"").toLowerCase();
  var items=[];
  // views
  items.push({group:"Views", ic:"🎮", label:"Live view", run:function(){ setView("live"); }});
  items.push({group:"Views", ic:"📈", label:"Analytics view", run:function(){ setView("analytics"); }});
  items.push({group:"Views", ic:"📕", label:"Pokédex view", run:function(){ setView("pokedex"); }});
  items.push({group:"Actions", ic:"📋", label:"Event log", run:function(){ openLog(); }});
  items.push({group:"Actions", ic:"⌨️", label:"Keyboard shortcuts & help", run:function(){ openHelp(); }});
  items.push({group:"Views", ic:"⚔️", label:"Gym / Team analysis", run:function(){ setView("gym"); }});
  items.push({group:"Views", ic:"\uD83C\uDFEA", label:"Store", run:function(){ setView("store"); }});
  items.push({group:"Views", ic:"\uD83C\uDF2E", label:"Cali Tuesdays", run:function(){ setView("cali"); }});
  items.push({group:"Views", ic:"📺", label:"War Room", run:function(){ openWarroom(); }});
  if(typeof hqJumpCity==="function") items.push({group:"Views", ic:"\uD83C\uDFD9", label:"Jump to Arena City", sub:"the 3D street, the fountain and the bike park", run:function(){ hqJumpCity(); }});
  // actions
  items.push({group:"Actions", ic:"⚙️", label:"Open Settings", run:function(){ openSettings(); }});
  if(typeof openNotchInvite==="function") items.push({group:"Actions", ic:"\uD83D\uDCBB", label:"HQ Notch", sub:"install or check the notch app", run:function(){ openNotchInvite(); }});
  items.push({group:"Actions", ic:"⬇️", label:"Export CSV", run:function(){ downloadExport("csv"); }});
  items.push({group:"Actions", ic:"⬇️", label:"Export JSON", run:function(){ downloadExport("json"); }});
  items.push({group:"Actions", ic:"🔔", label:"Toggle notifications", run:function(){ $("bellBtn").click(); }});
  items.push({group:"Actions", ic:"🗣️", label:"Toggle voice alerts", run:function(){ toggleVoice(); }});
  items.push({group:"Actions", ic:"📝", label:"Daily Digest", run:function(){ openDigest(); }});
  items.push({group:"Actions", ic:"🔄", label:"Force refresh", run:function(){ forceRefresh(); }});
  // live sessions
  (STATE&&STATE.sessions||[]).forEach(function(s){
    var cr=s.creature||{};
    items.push({group:"Sessions", ic:creatureEmoji(cr), cr:cr, label:s.title||s.name||s.id,
      sub:(s.status||""), run:function(){ openSession(s.sessionId||s.id,{title:s.title,folder:s.folder,status:s.status,creature:s.creature}); }});
    if(s.status==="working"){
      items.push({group:"Focus", ic:"⤢", label:"Focus: "+(s.title||s.id), sub:"now playing",
        run:function(){ openFocus(s.sessionId); }});
    }
    items.push({group:"Resume", ic:"▶", label:"Resume: "+(s.title||s.id), sub:s.folder||"",
      run:function(){ sessResume(s.sessionId); }});
    items.push({group:"Transcript", ic:"📜", label:"Read transcript: "+(s.title||s.id), sub:s.folder||"",
      run:function(){ openTranscript(s.sessionId||s.id, s.title); }});
    items.push({group:"Pin", ic:s.pinned?"📌":"📍", label:(s.pinned?"Unpin: ":"Pin: ")+(s.title||s.id),
      run:function(){ togglePin(s); }});
  });
  if(q){ items=items.filter(function(it){ return (it.label+" "+(it.sub||"")+" "+it.group).toLowerCase().indexOf(q)>=0; }); }
  return items;
}
function renderCmdk(){
  var q=$("cmdkInput").value.trim();
  CMDK_ITEMS=buildCmdkItems(q);
  if(CMDK_SEL>=CMDK_ITEMS.length) CMDK_SEL=Math.max(0,CMDK_ITEMS.length-1);
  var list=$("cmdkList"); list.innerHTML="";
  if(!CMDK_ITEMS.length){ list.innerHTML='<div class="cmdk-empty">No matches.</div>'; return; }
  var lastGroup=null;
  CMDK_ITEMS.forEach(function(it,i){
    if(it.group!==lastGroup){ var g=el("div","cmdk-group"); g.textContent=it.group; list.appendChild(g); lastGroup=it.group; }
    var row=el("div","cmdk-item"+(i===CMDK_SEL?" sel":""));
    var ic=el("div","ci-ic");
    if(it.cr && cfg().creaturePack==="monsters"){ ic.innerHTML=monsterSVG(creatureSeed(it.cr),creatureTypeHue(it.cr),creatureStage(it.cr),!!it.cr.shiny,18); }
    else if(it.cr && cfg().creaturePack==="village"){ ic.innerHTML=troopVisual(it.cr,18); }
    else if(it.cr && isPokePack()){ ic.innerHTML=pokeImgHTML(it.cr,18); }
    else if(it.cr && cfg().creaturePack==="aniimo"){ ic.innerHTML=animoImgHTML(it.cr,18); }
    else { ic.textContent=it.ic; }
    row.appendChild(ic);
    var t=el("div","ci-t"); t.textContent=it.label; row.appendChild(t);
    if(it.sub){ var sb=el("div","ci-sub"); sb.textContent=it.sub; row.appendChild(sb); }
    row.addEventListener("click",function(){ runCmdkItem(i); });
    row.addEventListener("mousemove",function(){ if(CMDK_SEL!==i){ CMDK_SEL=i; markCmdkSel(); } });
    list.appendChild(row);
  });
}
function markCmdkSel(){
  var rows=$("cmdkList").querySelectorAll(".cmdk-item");
  Array.prototype.forEach.call(rows,function(r,i){ r.classList.toggle("sel", i===CMDK_SEL); });
  var sel=rows[CMDK_SEL]; if(sel&&sel.scrollIntoView) sel.scrollIntoView({block:"nearest"});
}
function runCmdkItem(i){
  var it=CMDK_ITEMS[i]; if(!it) return;
  closeCmdk();
  try{ it.run(); }catch(e){}
}
function cmdkKeydown(e){
  if(e.key==="ArrowDown"){ e.preventDefault(); CMDK_SEL=Math.min(CMDK_ITEMS.length-1,CMDK_SEL+1); markCmdkSel(); }
  else if(e.key==="ArrowUp"){ e.preventDefault(); CMDK_SEL=Math.max(0,CMDK_SEL-1); markCmdkSel(); }
  else if(e.key==="Enter"){ e.preventDefault(); runCmdkItem(CMDK_SEL); }
}
function openCmdk(){
  rememberOpener();
  $("cmdk").classList.add("open"); CMDK_OPEN=true; CMDK_SEL=0;
  $("cmdkInput").value=""; renderCmdk(); $("cmdkInput").focus();
}
function closeCmdk(){ $("cmdk").classList.remove("open"); CMDK_OPEN=false; restoreOpener(); }
function toggleCmdk(){ CMDK_OPEN?closeCmdk():openCmdk(); }
$("cmdkInput").addEventListener("input",function(){ CMDK_SEL=0; renderCmdk(); });
$("cmdk").addEventListener("click",function(e){ if(e.target===this) closeCmdk(); });
$("cmdkBtn").addEventListener("click",openCmdk);

/* ---- header menu + exports ---- */
function closeMenu(){ $("menuPop").classList.remove("open"); $("menuBtn").setAttribute("aria-expanded","false"); }
function toggleMenu(){
  var open=$("menuPop").classList.toggle("open");
  $("menuBtn").setAttribute("aria-expanded", open?"true":"false");
}
$("menuBtn").addEventListener("click",function(e){ e.stopPropagation(); toggleMenu(); });
document.addEventListener("click",function(e){
  if($("menuPop").classList.contains("open") && !e.target.closest(".hmenu")) closeMenu();
});
function downloadExport(kind){
  var url = kind==="csv" ? "/api/export.csv" : "/api/export.json";
  var a=el("a"); a.href=url; a.download = kind==="csv" ? "claude-hq-export.csv" : "claude-hq-export.json";
  document.body.appendChild(a); a.click(); a.remove();
}
$("expCsv").addEventListener("click",function(){ downloadExport("csv"); closeMenu(); });
$("expJson").addEventListener("click",function(){ downloadExport("json"); closeMenu(); });
$("menuRefresh").addEventListener("click",function(){ forceRefresh(); closeMenu(); });
$("menuNotif").addEventListener("click",function(){ $("bellBtn").click(); closeMenu(); });
$("menuDigest").addEventListener("click",function(){ openDigest(); closeMenu(); });
$("menuVoice").addEventListener("click",function(){ toggleVoice(); closeMenu(); });
function forceRefresh(){ load(); pulse(); if(VIEW==="analytics") loadHistory(); }

/* ---- evolution-line preview modal ----
   Reuses the existing sprite helper (pokeImgFor with an explicit dex) for every node,
   so nothing new is drawn: base->final along POKE_EVO, plus the Mega form when eligible.
   Non-apex nodes are rendered from a stage-clamped clone so the 3D pack's mega-override
   (creatureStage>=4) doesn't hijack mid-line sprites. */
// Build the evolution-line markup for a creature (reused inline in the drawer and by the
// preview modal). Each node reuses the existing sprite helper with an explicit dex + a
// stage-clamped, _noFloor clone so the 3D mega-override never hijacks a mid-line sprite.
function evoLineHTML(cr, px){
  cr=cr||{}; px=px||72;
  if(!isPokePack()) return '';
  var line=pokeEvoLine(cr), pos=pokeEvoPos(cr,line), megaActive=!!pokeMega(cr),
      canMega=(cfg().creaturePack==="pokemon3d" && pokeCanMega(cr)),
      lastI=line.length-1, chosenFinal=branchFinalDex(cr);
  var html='<div class="evoline">';
  line.forEach(function(dex,i){
    var showDex=(i===lastI)?chosenFinal:dex;                 // final node reflects the chosen branch
    var clone=Object.assign({},cr,{stage:3,_noFloor:true});
    var nm=DEX_NAMES[showDex]||POKE_REAL[pokeIdx(cr)]||"???";
    if(i>0) html+='<span class="evoarrow" aria-hidden="true">→</span>';
    html+='<div class="evonode'+((!megaActive && i===pos)?" current":"")+'">'+
      '<div class="evo-spr">'+pokeImgFor(showDex,clone,px,nm)+'</div>'+
      '<div class="evo-nm">'+esc(nm)+'</div><div class="evo-dx">#'+String(showDex).padStart(3,"0")+'</div></div>';
  });
  if(canMega){
    var mf=pokeMegaForm(Object.assign({},cr,{stage:4,_noFloor:true})),
        mclone=Object.assign({},cr,{stage:4,_noFloor:true}),
        mnm=(mf&&mf.name)||("Mega "+(DEX_NAMES[chosenFinal]||POKE_REAL[pokeIdx(cr)]||""));
    html+='<span class="evoarrow" aria-hidden="true">⚡</span>'+
      '<div class="evonode mega'+(megaActive?" current":"")+'">'+
      '<div class="evo-spr">'+pokeImgFor(chosenFinal,mclone,px,mnm)+'</div>'+
      '<div class="evo-nm">'+esc(mnm)+'</div><div class="evo-dx">'+(megaActive?"active":"at Apex")+'</div></div>';
  }
  html+='</div>';
  var note = megaActive ? "Mega-evolved — its ultimate form."
    : (canMega ? "Reaches its Mega form at Apex."
    : (line.length>1 ? "Grows along this line as the session earns prompts." : "This species has a single form."));
  return html+'<div class="evoprev-note">'+esc(note)+'</div>';
}
function openEvoPreview(cr){
  cr=cr||{}; var body=$("evoPrevBody"); if(!body) return;
  body.innerHTML = isPokePack() ? evoLineHTML(cr,72)
    : '<div class="feed-empty">Evolution preview is available in the Pokémon packs.</div>';
  rememberOpener(); $("evoPreviewBack").classList.add("open"); var c=$("evoPrevClose"); if(c) c.focus();
}
function closeEvoPreview(){ $("evoPreviewBack").classList.remove("open"); restoreOpener(); }
(function(){ var b=$("evoPrevClose"); if(b) b.addEventListener("click",closeEvoPreview);
  var s=$("evoPreviewBack"); if(s) s.addEventListener("click",function(e){ if(e.target===s) closeEvoPreview(); }); })();

/* ---- Evolution / Mega PATH PICKER (choose branch + X/Y mega) ---- */
// Build an <img> for a SPECIFIC mega form (used for the X/Y toggle tiles), honoring its source dir.
function megaFormImg(f, cr, px){
  var shiny=cr.shiny?1:0, seed=creatureSeed(cr), hue=creatureTypeHue(cr), finalDex=branchFinalDex(cr);
  var base = f.dir==="oras" ? (shiny?PKPARAISO_ORAS_SHINY:PKPARAISO_ORAS) : (shiny?PKPARAISO_XY_SHINY:PKPARAISO_XY);
  var fb=encodeURIComponent(JSON.stringify([seed,hue,4,shiny,px]));
  return '<img class="pokeimg pokeimg-3d pokeimg-mega" src="'+base+f.slug+'.gif" width="'+px+'" height="'+px+'"'+
    ' alt="'+esc(f.name)+'" data-dex="'+finalDex+'" data-shiny="'+shiny+'" data-3d="1" data-mega="1"'+
    ' data-slug="'+f.slug+'" data-try="0" data-fb="'+fb+'" onerror="pokeErr(this)" loading="lazy">';
}
function renderEvoPicker(cr){
  var body=$("evoPickBody"); if(!body) return;
  if(!isPokePack()){ body.innerHTML='<div class="feed-empty">Path choices are available in the Pokémon packs.</div>'; return; }
  var sid=cr&&cr._sid;
  var opts=POKE_BRANCH[pokeIdx(cr)];
  var chosenFinal=branchFinalDex(cr);
  var megaForms=MEGA_FORMS[chosenFinal]||[];
  var activeMega=pokeMegaForm(Object.assign({},cr,{stage:4,_noFloor:true}));
  var h='<div class="evopick-prev">'+evoLineHTML(cr,60)+'</div>';
  if(opts){
    h+='<div class="evopick-h">Evolution path</div>';
    h+='<div class="evopick-grid" role="radiogroup" aria-label="Evolution path">';
    opts.forEach(function(o){
      var clone=Object.assign({},cr,{stage:3,_noFloor:true});
      var sel=(o.dex===chosenFinal);
      h+='<button type="button" class="evopick-opt'+(sel?" sel":"")+'" role="radio" aria-checked="'+(sel?"true":"false")+'"'+
         ' data-branch="'+o.dex+'"><div class="evo-spr">'+pokeImgFor(o.dex,clone,64,o.name)+'</div>'+
         '<div class="evo-nm">'+esc(o.name)+'</div><div class="evo-dx">#'+String(o.dex).padStart(3,"0")+'</div></button>';
    });
    h+='</div>';
  }
  if(megaForms.length>1){
    h+='<div class="evopick-h">Mega form <span class="muted">(at Apex)</span></div>';
    h+='<div class="evopick-grid mega" role="radiogroup" aria-label="Mega form">';
    megaForms.forEach(function(f){
      var sel=activeMega&&activeMega.slug===f.slug;
      h+='<button type="button" class="evopick-opt'+(sel?" sel":"")+'" role="radio" aria-checked="'+(sel?"true":"false")+'"'+
         ' data-mega="'+f.slug+'"><div class="evo-spr">'+megaFormImg(f,cr,64)+'</div>'+
         '<div class="evo-nm">'+esc(f.name)+'</div></button>';
    });
    h+='</div>';
  }
  if(!opts && megaForms.length<=1) h+='<div class="feed-empty">This species has a single evolution path.</div>';
  body.innerHTML=h;
  // wire selections
  Array.prototype.forEach.call(body.querySelectorAll("[data-branch]"),function(b){
    b.addEventListener("click",function(){ setEvoChoice(sid,{branchDex:+b.getAttribute("data-branch")}); renderEvoPicker(cr); }); });
  Array.prototype.forEach.call(body.querySelectorAll("[data-mega]"),function(b){
    b.addEventListener("click",function(){ setEvoChoice(sid,{megaSlug:b.getAttribute("data-mega")}); renderEvoPicker(cr); }); });
  Array.prototype.forEach.call(body.querySelectorAll(".evopick-grid"),function(g){ if(typeof setupRoving==="function") setupRoving(g,'[role="radio"]'); });
  var first=body.querySelector(".evopick-opt.sel")||body.querySelector(".evopick-opt"); if(first) first.focus();
}
var EVO_PICK_CR=null;
function openEvoPicker(cr){ cr=cr||{}; if(!cr._sid){ toast("Open this from a session card","level"); return; }
  EVO_PICK_CR=cr; rememberOpener(); renderEvoPicker(cr);
  $("evoPickBack").classList.add("open"); }
function closeEvoPicker(){ $("evoPickBack").classList.remove("open"); EVO_PICK_CR=null; restoreOpener(); }
(function(){ var b=$("evoPickClose"); if(b) b.addEventListener("click",closeEvoPicker);
  var s=$("evoPickBack"); if(s) s.addEventListener("click",function(e){ if(e.target===s) closeEvoPicker(); }); })();

/* ---- digest + focus wiring ---- */
$("menuVoice").innerHTML = ico("volume")+"Voice alerts: "+(VOICE?"on":"off");
$("menuVoice").setAttribute("aria-pressed", VOICE?"true":"false");
$("digestClose").addEventListener("click",closeDigest);
$("digestDate").addEventListener("change",loadDigest);
$("digestModeDay").addEventListener("click",function(){ setDigestMode("day"); });
$("digestModeWeek").addEventListener("click",function(){ setDigestMode("week"); });
refreshDigestModeUI();
$("digestDownload").addEventListener("click",function(){
  var date=$("digestDate").value||todayLocalISO();
  var days=digestDays();
  var a=el("a"); a.href="/api/digest?download=1&days="+days+"&date="+encodeURIComponent(date);
  a.download="claude-hq-digest-"+date+(days>1?("-"+days+"d"):"")+".md"; document.body.appendChild(a); a.click(); a.remove();
});
$("digestBack").addEventListener("click",function(e){ if(e.target===$("digestBack")) closeDigest(); });

/* ---- "how it works" info modal ---- */
function openInfo(){ rememberOpener(); $("infoBack").classList.add("open"); $("infoClose").focus(); }
function closeInfo(){ $("infoBack").classList.remove("open"); restoreOpener(); }
$("dexInfoBtn").addEventListener("click",openInfo);
$("infoClose").addEventListener("click",closeInfo);
$("infoBack").addEventListener("click",function(e){ if(e.target===$("infoBack")) closeInfo(); });
function openHelp(){ rememberOpener(); $("helpBack").classList.add("open"); $("helpClose").focus(); }
function closeHelp(){ $("helpBack").classList.remove("open"); restoreOpener(); }
$("logBtn").addEventListener("click",openLog);
$("logClose").addEventListener("click",closeLog);
$("logClear").addEventListener("click",clearLog);
$("logBack").addEventListener("click",function(e){ if(e.target===$("logBack")) closeLog(); });
$("helpBtn").addEventListener("click",openHelp);
$("helpClose").addEventListener("click",closeHelp);
$("helpBack").addEventListener("click",function(e){ if(e.target===$("helpBack")) closeHelp(); });
$("focusClose").addEventListener("click",closeFocus);

/* ---- error visibility ----
   Uncaught errors and rejected promises normally vanish silently (and the app has many
   deliberate empty catches for optional/localStorage paths). Always log to the console;
   when debug mode is on (localStorage hq_debug="1"), also surface a toast so failures are
   visible while developing. Toggle from the console: localStorage.hq_debug="1". */
function hqDebug(){ try{ return localStorage.getItem("hq_debug")==="1"; }catch(e){ return false; } }
function hqErr(e, ctx){
  try{ console.error("[HQ]"+(ctx?" "+ctx:""), e); }catch(_){}
  if(hqDebug() && typeof toast==="function"){
    try{ toast("⚠ "+(ctx?ctx+": ":"")+((e&&e.message)||e||"error"), "ach"); }catch(_){}
  }
}
window.addEventListener("error", function(ev){ hqErr(ev.error||ev.message, "uncaught"); });
window.addEventListener("unhandledrejection", function(ev){ hqErr(ev.reason, "promise"); });

/* ---- celebrations ---- */
var CELEB_READY=false, PREV_LEVEL=null, PREV_ACH={};
function toast(msg, cls){
  var w=$("toastWrap");
  var t=el("div","toast"+(cls?(" "+cls):""));
  t.textContent=msg;
  w.appendChild(t);
  setTimeout(function(){ t.style.transition="opacity .4s"; t.style.opacity="0"; setTimeout(function(){ if(t.parentNode) t.remove(); },420); }, 4200);
}
function reducedMotion(){ return window.matchMedia && window.matchMedia("(prefers-reduced-motion: reduce)").matches; }

/* ---- event log: a persisted history of notable moments ---- */
var EVENTS=(function(){ try{ return JSON.parse(localStorage.getItem("hq_events")||"[]"); }catch(e){ return []; } })();
function logEvent(icon, text){
  EVENTS.unshift({t:Date.now(), icon:icon||"•", text:text||""});
  if(EVENTS.length>150) EVENTS.length=150;
  try{ localStorage.setItem("hq_events", JSON.stringify(EVENTS)); }catch(e){}
  var b=$("logBtn"); if(b) b.classList.add("has-new");
  if($("logBack") && $("logBack").classList.contains("open")) renderLog();
}
function openLog(){ rememberOpener(); $("logBack").classList.add("open"); var b=$("logBtn"); if(b) b.classList.remove("has-new"); renderLog(); $("logClose").focus(); }
function closeLog(){ $("logBack").classList.remove("open"); restoreOpener(); }
function clearLog(){ EVENTS=[]; try{ localStorage.setItem("hq_events","[]"); }catch(e){} renderLog(); }
function renderLog(){
  var box=$("logList"); if(!box) return;
  if(!EVENTS.length){ box.innerHTML='<div class="insight-empty">No events yet — evolutions, level-ups, and needs-you moments will show up here.</div>'; return; }
  box.innerHTML="";
  EVENTS.forEach(function(e){
    var row=el("div","logrow");
    row.innerHTML='<span class="logrow-ic">'+esc(e.icon)+'</span><span class="logrow-tx">'+esc(e.text)+'</span><span class="logrow-t">'+esc(relTime(new Date(e.t).toISOString()))+'</span>';
    box.appendChild(row);
  });
}
function checkCelebration(season){
  if(!season) return;
  var lvl=season.level;
  var unlocked={}; (season.achievements||[]).forEach(function(a){ if(a.unlocked) unlocked[a.id]=a.name; });
  if(!CELEB_READY){ PREV_LEVEL=lvl; PREV_ACH=unlocked; CELEB_READY=true; return; }
  // with the Arena, level-ups are the HQ level's (23-progress.js celebrates those); the 30-day season only counts unpaired
  if(typeof progLevel==="function" && progLevel()) lvl=null;
  if(lvl!=null && PREV_LEVEL!=null && lvl>PREV_LEVEL){ toast("🎉 Level "+lvl+"!","level"); confettiBurst(); logEvent("🎉","Reached Level "+lvl); }
  Object.keys(unlocked).forEach(function(id){
    if(!PREV_ACH[id]){ toast("🏅 Achievement: "+unlocked[id],"ach"); confettiBurst(); logEvent("🏅","Achievement unlocked: "+unlocked[id]); }
  });
  PREV_LEVEL=lvl; PREV_ACH=unlocked;
}
// Celebrate when a session's creature stage INCREASES between updates.
// Baseline on first load so we never celebrate the initial snapshot.
var STAGE_PREV={}, STAGE_BASE=false, SHINY_SEEN={};
