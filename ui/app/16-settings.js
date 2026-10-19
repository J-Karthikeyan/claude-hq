/* ================= v5 wiring ================= */

/* ---- health strip + budget ---- */
function gotoStuck(){
  if(VIEW!=="live") setView("live");
  var first = document.querySelector(".card.stuckcard");
  if(first){ first.scrollIntoView({behavior:"smooth",block:"center"}); }
  else { $("party").scrollIntoView({behavior:"smooth",block:"start"}); }
}
function renderHealth(h){
  var strip=$("healthStrip"); if(!strip) return;
  h = h||{};
  var stuck = h.stuck||[];
  var main=$("healthMain");
  if(stuck.length){
    main.innerHTML="";
    var b=el("div","health-stuck"); b.setAttribute("role","button"); b.tabIndex=0;
    b.textContent = "🩺 "+stuck.length+" tab"+(stuck.length===1?"":"s")+" may be stuck";
    b.addEventListener("click",gotoStuck);
    b.addEventListener("keydown",function(e){ if(e.key==="Enter"||e.key===" "){ e.preventDefault(); gotoStuck(); } });
    main.appendChild(b);
  } else {
    main.innerHTML = '<div class="health-ok">🩺 All tabs healthy</div>';
  }
  var budget = h.dailyBudgetUSD!=null ? h.dailyBudgetUSD : (CONFIG.dailyBudgetUSD||0);
  var cost = h.dailyCostUSD!=null ? h.dailyCostUSD : 0;
  var bw=$("budgetWrap");
  if(!budget || budget<=0){ bw.classList.add("hidden"); }
  else {
    bw.classList.remove("hidden");
    var pct = Math.max(0, Math.min(100, cost/budget*100));
    $("budgetRing").style.setProperty("--bpct", pct);
    var over = h.overBudget!=null ? h.overBudget : (cost>budget);
    bw.classList.toggle("over", !!over);
    bw.classList.toggle("warn", !over && pct>=80);
    $("budgetText").innerHTML = "Today <b>"+esc(fmtCost(cost))+"</b> / "+esc(fmtCost(budget));
    checkBudgetAlert(cost, budget, !!over);
  }
  strip.classList.toggle("hidden", !(stuck.length>0 || (budget && budget>0)));
}
function renderTagFilter(sessions){
  var sel=$("tagFilter"); if(!sel) return;
  var set={}; (sessions||[]).forEach(function(s){ (s.tags||[]).forEach(function(t){ if(t) set[t]=1; }); });
  var tags=Object.keys(set).sort();
  if(FILTERTAG && tags.indexOf(FILTERTAG)<0) FILTERTAG="";
  // Rebuild only when the options changed, and never under an open/focused picker (that would
  // close it or move the highlighted option); it catches up on the next tick after blur.
  var sig=JSON.stringify([tags, FILTERTAG]);
  if(sig===sel._sig || document.activeElement===sel) return;
  sel._sig=sig;
  sel.innerHTML="";
  var o0=el("option"); o0.value=""; o0.textContent="All tags"; sel.appendChild(o0);
  tags.forEach(function(t){ var o=el("option"); o.value=t; o.textContent="#"+t; sel.appendChild(o); });
  sel.value=FILTERTAG;
  sel.classList.toggle("hidden", tags.length===0);
}
$("tagFilter").addEventListener("change",function(e){ setTagFilter(e.target.value); });

function prettyFolderShort(slug){ var p=prettyFolder(slug); return p.length>26?("…"+p.slice(-25)):p; }
function renderFolderFilter(sessions){
  var sel=$("folderFilter"); if(!sel) return;
  var set={}; (sessions||[]).forEach(function(s){ if(s.folder) set[s.folder]=(set[s.folder]||0)+1; });
  var folders=Object.keys(set).sort();
  if(FILTERFOLDER && folders.indexOf(FILTERFOLDER)<0) FILTERFOLDER="";
  var sig=JSON.stringify([folders.map(function(f){ return [f, set[f]]; }), FILTERFOLDER]);   // see renderTagFilter
  if(sig===sel._sig || document.activeElement===sel) return;
  sel._sig=sig;
  sel.innerHTML="";
  var o0=el("option"); o0.value=""; o0.textContent="All projects"; sel.appendChild(o0);
  folders.forEach(function(f){ var o=el("option"); o.value=f; o.textContent=prettyFolderShort(f)+" ("+set[f]+")"; sel.appendChild(o); });
  sel.value=FILTERFOLDER;
  sel.classList.toggle("hidden", folders.length<2);
}
$("folderFilter").addEventListener("change",function(e){ FILTERFOLDER=e.target.value; if(STATE) renderParty(STATE.sessions); });

/* ---- settings modal ---- */
var SERVER_THEMES=["aurora","midnight","forest","mono"];
/* ---- Trainer builder (Settings) ---- */
var TB_SPEC=null;
var TB_IDX={skin:TR.SKIN,hair:TR.HAIR,hairColor:TR.HAIRC,outfit:TR.OUTFIT,outfitColor:TR.OUTC,hat:TR.HAT,accessory:TR.ACC,bg:TR.BG,face:TR.FACE};
var TB_TITLE={skin:"Skin",hair:"Hair",hairColor:"Hair color",outfit:"Outfit",outfitColor:"Outfit color",hat:"Headwear",accessory:"Accessory",bg:"Background",face:"Expression"};
var TB_COLOR={skin:TR_SKIN,hairColor:TR_HAIRC,outfitColor:TR_OUTC};
var TB_ORDER=["skin","face","hair","hairColor","outfit","outfitColor","hat","accessory","bg"];
function tbPreview(){ var p=$("tbPreview"); if(p&&typeof trainerSVG==="function") p.innerHTML=trainerSVG(TB_SPEC,112,{animate:!calmMode()}); }
function buildTrainerBuilder(spec){
  var host=$("tbControls"); if(!host||typeof trainerSVG!=="function") return;
  TB_SPEC=(_validTrainerSpec(spec)?spec:resolveTrainerSpec()).slice(0,9);
  host.innerHTML="";
  TB_ORDER.forEach(function(key){
    var idx=TB_IDX[key], max=TR_MAX[idx], labels=TR_LABELS[key]||[];
    var f=el("div","tb-field"), lb=el("label"); lb.textContent=TB_TITLE[key]; f.appendChild(lb);
    if(TB_COLOR[key]){
      var pal=TB_COLOR[key], sw=el("div","tb-sw"); sw.setAttribute("role","group"); sw.setAttribute("aria-label",TB_TITLE[key]);
      for(var v=0;v<=max;v++){(function(v){
        var btn=el("button"); btn.type="button"; btn.style.background=pal[v];
        btn.title=labels[v]||("Option "+(v+1)); btn.setAttribute("aria-label",btn.title);
        btn.setAttribute("aria-pressed", TB_SPEC[idx]===v?"true":"false");
        btn.addEventListener("click",function(){ TB_SPEC[idx]=v; Array.prototype.forEach.call(sw.children,function(c,ci){c.setAttribute("aria-pressed",ci===v?"true":"false");}); tbPreview(); });
        sw.appendChild(btn);
      })(v);}
      f.appendChild(sw);
    } else {
      var sel=el("select");
      for(var v2=0;v2<=max;v2++){ var o=el("option"); o.value=String(v2); o.textContent=labels[v2]||("Option "+(v2+1)); if(TB_SPEC[idx]===v2)o.selected=true; sel.appendChild(o); }
      sel.addEventListener("change",function(){ TB_SPEC[idx]=parseInt(sel.value,10)||0; tbPreview(); });
      f.appendChild(sel);
    }
    host.appendChild(f);
  });
  tbPreview();
}
function readTrainerBuilder(){ return _validTrainerSpec(TB_SPEC)?TB_SPEC.slice(0,9):resolveTrainerSpec(); }
(function(){
  var r=$("tbRandom"); if(r) r.addEventListener("click",function(){ var s=[]; for(var i=0;i<9;i++) s.push(Math.floor(Math.random()*(TR_MAX[i]+1))); buildTrainerBuilder(s); });
  var rs=$("tbReset"); if(rs) rs.addEventListener("click",function(){ buildTrainerBuilder(defaultTrainerSpec(stableTrainerKey())); });
})();

function openSettings(){
  rememberOpener();
  var c=CONFIG;
  buildTrainerBuilder();
  if(typeof buildCharBuilder==="function") buildCharBuilder();
  $("setTheme").value = effectiveTheme();
  $("setPack").value = c.creaturePack||"pokemon";
  $("setRefresh").value = Math.round((c.refreshMs||5000)/1000);
  $("setStuck").value = c.stuckMinutes!=null ? c.stuckMinutes : 15;
  $("setBudget").value = c.dailyBudgetUSD!=null ? c.dailyBudgetUSD : 0;
  var lt=$("setLargeText"); if(lt) lt.checked=largePref();
  var cm=$("setCalm"); if(cm) cm.checked=calmPref();
  var fz=$("setFatigue"); if(fz) fz.checked = c.creatureFatigue!==false;
  var ms=$("setMusicShare"); if(ms) ms.checked = c.musicShare!==false;
  var mc=$("setMusicCookies"); if(mc) mc.value = c.musicCookies || "";
  var ws=$("setWorkSignals"), wp=$("setWorkSignalsPRs");
  if(ws) ws.checked = c.workSignals===true;
  if(wp){ wp.checked = c.workSignalsPRs===true; wp.disabled = !(ws && ws.checked); }
  var ab=$("setAccountBoard"); if(ab) ab.checked = c.arenaAccountBoard===true;
  var tn=$("setTrainer"); if(tn) tn.value=trainerPref();
  if(typeof syncAccentControls==="function") syncAccentControls();
  $("settingsBack").classList.add("open");
  $("setTheme").focus();
}
// revert any live preview back to the persisted display prefs, then close.
if($("setNotchOpen")) $("setNotchOpen").addEventListener("click",function(){ if(typeof openNotchInvite==="function") openNotchInvite(); });
function closeSettings(){ if(typeof charClosePreview==="function") charClosePreview(); applyDisplayPrefs(); $("settingsBack").classList.remove("open"); restoreOpener(); }
// change theme -> persist immediately (localStorage is authoritative) so it survives
// both the every-tick syncConfig re-apply AND a page reload; sync to server if known.
$("setTheme").addEventListener("change",function(){
  var t=$("setTheme").value;
  try{ localStorage.setItem("hq_theme",t); }catch(e){}
  applyDisplayPrefs();
  if(SERVER_THEMES.indexOf(t)>=0){ postConfig({theme:t}).catch(function(){}); }
});
function saveSettings(){
  var theme=$("setTheme").value;
  var largeText=$("setLargeText")?$("setLargeText").checked:false;
  var calm=$("setCalm")?$("setCalm").checked:false;
  // Display prefs are LOCAL (server does not persist contrast/large/calm reliably).
  try{ localStorage.setItem("hq_theme",theme);
       localStorage.setItem("hq_large",largeText?"1":"0");
       localStorage.setItem("hq_calm",calm?"1":"0"); }catch(e){}
  var body = {
    creaturePack: $("setPack").value,
    refreshMs: Math.max(1000, (parseInt($("setRefresh").value,10)||5)*1000),
    stuckMinutes: Math.max(1, parseInt($("setStuck").value,10)||15),
    dailyBudgetUSD: Math.max(0, parseFloat($("setBudget").value)||0),
    trainerName: $("setTrainer") ? $("setTrainer").value.trim().slice(0,32) : "",
    trainerAvatar: readTrainerBuilder(),
    character: (typeof readCharBuilder==="function" && typeof char3dOk==="function" && char3dOk()) ? readCharBuilder() : undefined,
    creatureFatigue: $("setFatigue") ? $("setFatigue").checked : true,
    musicShare: $("setMusicShare") ? $("setMusicShare").checked : true,
    musicCookies: $("setMusicCookies") ? $("setMusicCookies").value : ""
  };
  if($("setWorkSignals")){ body.workSignals=!!$("setWorkSignals").checked; body.workSignalsPRs=body.workSignals && !!($("setWorkSignalsPRs") && $("setWorkSignalsPRs").checked); }
  if($("setAccountBoard")) body.arenaAccountBoard=!!$("setAccountBoard").checked;
  var acctWas = (typeof CONFIG!=="undefined" && CONFIG && CONFIG.arenaAccountBoard===true);
  if(body.character===undefined) delete body.character;   // no WebGL2: the 3D character is left as it was
  // localStorage mirror so the Trainer Card paints instantly on next boot,
  // before the /api/config fetch resolves (resolveTrainerSpec validates it).
  try{ localStorage.setItem("hq_trainer", JSON.stringify(body.trainerAvatar)); }catch(e){}
  // Only send the theme to the backend when it is one the backend understands,
  // so an unknown value (e.g. "contrast") can never wipe a valid stored theme.
  if(SERVER_THEMES.indexOf(theme)>=0) body.theme=theme;
  applyConfig(body);      // apply immediately (theme swap + creature repaint)
  applyDisplayPrefs();    // ensure contrast/large/calm applied even if not in body
  if(body.character && typeof charSaved==="function") charSaved(body.character);
  renderTrainerCard();
  restartPoll();          // new cadence for the poll fallback
  if(STATE) renderHealth(STATE.health);
  postConfig(body).then(function(r){
    toast(r.ok?"⚙️ Settings saved":"⚠ "+(r.j.error||"save failed"), r.ok?"level":"ach");
    // Account board changed: publish now, so joining (or leaving) takes effect at once
    // instead of on the next 5-minute background publish.
    // Unpaired, the local route answers "not paired" and nothing leaves the machine.
    if(r.ok && body.arenaAccountBoard!==undefined && body.arenaAccountBoard!==acctWas && typeof arenaPost==="function"){
      arenaPost("/api/arena/publish").then(function(){ if(typeof arenaLoadBoard==="function") arenaLoadBoard(); });
    }
  }).catch(function(){ toast("⚠ save failed","ach"); });
  $("settingsBack").classList.remove("open"); restoreOpener();
}
$("settingsBtn").addEventListener("click",openSettings);
$("settingsClose").addEventListener("click",closeSettings);
$("settingsCancel").addEventListener("click",closeSettings);
$("settingsSave").addEventListener("click",saveSettings);
if($("chSave")) $("chSave").addEventListener("click",saveSettings);   // the same Save, right under the character
$("settingsBack").addEventListener("click",function(e){ if(e.target===this){ closeSettings(); } });
$("menuSettings").addEventListener("click",function(){ openSettings(); closeMenu(); });

/* ---- transcript reader ---- */
var TR_SID=null, TR_TITLE="", TR_OFFSET=0, TR_LIMIT=50, TR_TOTAL=0, TR_Q="";
function openTranscript(sid, title){
  if(!sid) return;
  rememberOpener();
  TR_SID=sid; TR_TITLE=title||"Transcript"; TR_OFFSET=0; TR_Q="";
  if($("transcriptSearch")) $("transcriptSearch").value="";
  $("transcriptTitle").textContent = "📜 "+(TR_TITLE||"Transcript");
  $("transcriptBack").classList.add("open");
  loadTranscript();
  $("transcriptClose").focus();
}
function closeTranscript(){ $("transcriptBack").classList.remove("open"); TR_SID=null; restoreOpener(); }
function loadTranscript(){
  if(!TR_SID) return;
  $("transcriptBody").innerHTML='<div class="dw-loading">'+LOADING_HTML+'</div>';
  $("transcriptPrev").disabled=true; $("transcriptNext").disabled=true;
  var url='/api/transcript/'+encodeURIComponent(TR_SID)+'?offset='+TR_OFFSET+'&limit='+TR_LIMIT+
    (TR_Q?('&q='+encodeURIComponent(TR_Q)):'');
  fetch(url,{cache:"no-store"})
    .then(function(r){ if(!r.ok) throw new Error(r.status); return r.json(); })
    .then(renderTranscript)
    .catch(function(){ $("transcriptBody").innerHTML='<div class="dw-loading">Could not load transcript.</div>'; $("transcriptPos").textContent="—"; });
}
function renderTranscript(d){
  d = d||{};
  var evs = d.events||d.messages||d.items||[];
  TR_TOTAL = d.total!=null ? d.total : (TR_OFFSET+evs.length);
  if(d.offset!=null) TR_OFFSET=d.offset;
  if(d.limit!=null) TR_LIMIT=d.limit;
  var box=$("transcriptBody"); box.innerHTML="";
  if(!evs.length){ box.innerHTML='<div class="searchov-empty">'+(TR_Q?'No matches for "'+esc(TR_Q)+'".':'No events on this page.')+'</div>'; }
  else {
    var wrap=el("div","tr-list");
    evs.forEach(function(ev){
      var role=(ev.role||ev.kind||ev.type||"").toLowerCase();
      var cls = (role==="you"||role==="user") ? "you"
              : (role==="tool"||role==="tool_use") ? "tool"
              : (role==="system") ? "system" : "claude";
      var row=el("div","tr-ev "+cls);
      var k=el("div","tr-k");
      var who = (d.source==="cursor") ? "Cursor" : "Claude";
      k.textContent = cls==="you"?"You":cls==="tool"?(ev.tool||ev.name||"tool"):cls==="system"?"System":who;
      var x=el("div","tr-x");
      var txt = ev.text||ev.detail||ev.content||ev.summary||"";
      if(TR_Q) x.innerHTML = highlight(txt, TR_Q);   // highlight() escapes then wraps matches
      else x.textContent = txt;                       // ESCAPED via textContent
      row.appendChild(k); row.appendChild(x); wrap.appendChild(row);
    });
    box.appendChild(wrap);
  }
  var end = Math.min(TR_TOTAL, TR_OFFSET+evs.length);
  if(TR_Q){
    var mt = d.matched!=null?d.matched:TR_TOTAL;
    $("transcriptPos").textContent = mt ? ("match "+(evs.length?TR_OFFSET+1:TR_OFFSET)+"–"+end+" of "+mt) : '0 matches';
  } else {
    $("transcriptPos").textContent = TR_TOTAL ? ((evs.length?TR_OFFSET+1:TR_OFFSET)+"–"+end+" of "+TR_TOTAL) : "0";
  }
  $("transcriptPrev").disabled = TR_OFFSET<=0;
  $("transcriptNext").disabled = end>=TR_TOTAL;
}
$("transcriptPrev").addEventListener("click",function(){ TR_OFFSET=Math.max(0,TR_OFFSET-TR_LIMIT); loadTranscript(); });
$("transcriptNext").addEventListener("click",function(){ TR_OFFSET=TR_OFFSET+TR_LIMIT; loadTranscript(); });
var TR_SEARCH_T=null;
$("transcriptSearch").addEventListener("input",function(e){
  var v=e.target.value.trim();
  clearTimeout(TR_SEARCH_T);
  TR_SEARCH_T=setTimeout(function(){ if(v===TR_Q) return; TR_Q=v; TR_OFFSET=0; loadTranscript(); }, 300);
});
$("transcriptClose").addEventListener("click",closeTranscript);
$("transcriptExport").addEventListener("click",function(){
  if(!TR_SID) return;
  var a=el("a"); a.href='/api/session/'+encodeURIComponent(TR_SID)+'/export.md';
  a.download="claude-session-"+TR_SID+".md";
  document.body.appendChild(a); a.click(); a.remove();
});
$("transcriptBack").addEventListener("click",function(e){ if(e.target===this) closeTranscript(); });

/* ---- war room ---- */
var WARROOM_ON=false, WR_IDX=0, WR_TIMER=null, WR_PAUSED=false;
function warroomSessions(){ return (STATE&&STATE.sessions||[]).filter(function(s){ return s.status==="working"; }); }
function openWarroom(){ rememberOpener(); WARROOM_ON=true; WR_IDX=0; $("warroom").classList.add("on"); renderWarroom(); startWarroomTimer(); var mw=$("menuWarRoom"); if(mw) mw.setAttribute("aria-pressed","true"); $("warroomClose").focus(); }
function closeWarroom(){ WARROOM_ON=false; $("warroom").classList.remove("on"); stopWarroomTimer(); var mw=$("menuWarRoom"); if(mw) mw.setAttribute("aria-pressed","false"); restoreOpener(); }
function startWarroomTimer(){ stopWarroomTimer(); if(calmMode()) return; WR_TIMER=setInterval(function(){ if(!WR_PAUSED){ WR_IDX++; renderWarroom(); } },6000); }
function stopWarroomTimer(){ if(WR_TIMER){ clearInterval(WR_TIMER); WR_TIMER=null; } }
function renderWarroom(){
  if(!WARROOM_ON) return;
  var wr=$("warroom");
  var list=warroomSessions();
  if(!list.length){
    wr.classList.remove("working");
    $("wrCreature").classList.remove("cre-svg","shiny","fz-tired","fz-fatigued","fz-ko");
    $("wrCreature").textContent="😴";
    $("wrTitle").textContent="No sessions working right now";
    $("wrNow").style.display="none"; $("wrTimer").textContent=""; $("wrMeta").textContent="";
    $("wrDots").innerHTML=""; return;
  }
  if(WR_IDX>=list.length) WR_IDX=WR_IDX%list.length;
  var s=list[WR_IDX];
  wr.classList.add("working");
  paintCreature($("wrCreature"), s.creature, 140, false);
  $("wrTitle").textContent=s.title||"Untitled session";
  var now=$("wrNow");
  now.style.display=""; now.innerHTML=orb("sm",orbStateFor(s.now))+esc(s.now||"Thinking…");
  $("wrTimer").textContent=fmtAge(warAge(s));
  var tk=s.tokens||{};
  var wf=fzOf(s.creature);
  $("wrMeta").textContent=creatureSpecies(s.creature)+" · "+creatureStageName(s.creature)+" · "+(s.folder||"~")+" · "+fmtTok(tk.output||0)+" out · ≈"+fmtCost(tk.estCostUSD||0)+
    (wf && wf.state==="unconscious" ? " \u00B7 \uD83D\uDCAB fainted" : (wf && wf.state==="fatigued" ? " \u00B7 \uD83D\uDCA6 fatigued" : ""));
  var dots=$("wrDots"); dots.innerHTML="";
  list.forEach(function(_,i){ var dd=el("i"); if(i===WR_IDX) dd.className="on"; dots.appendChild(dd); });
}
function warAge(s){ return (s.ageSecs||0)+Math.max(0, Math.round((Date.now()-STATE_AT)/1000)); }
// live-ticking elapsed timer (1s) independent of the 6s rotation
setInterval(function(){
  if(!WARROOM_ON) return;
  var list=warroomSessions(); if(!list.length) return;
  var s=list[WR_IDX%list.length]; if(s) $("wrTimer").textContent=fmtAge(warAge(s));
},1000);
$("warroom").addEventListener("mouseenter",function(){ WR_PAUSED=true; });
$("warroom").addEventListener("mouseleave",function(){ WR_PAUSED=false; });
$("warroomClose").addEventListener("click",closeWarroom);
$("menuWarRoom").addEventListener("click",function(){ openWarroom(); closeMenu(); });


/* ---- server stats in Settings (HQ 2.1): this HQ server and the Arena ---- */
var SRV = {timer:null};
function srvFmtUp(s){ s=s|0; var d=Math.floor(s/86400), h=Math.floor(s%86400/3600), m=Math.floor(s%3600/60);
  return d ? d+"d "+h+"h" : h ? h+"h "+m+"m" : m+"m "+(s%60)+"s"; }
function srvCard(title, rows, note){
  var c=document.createElement("div"); c.className="srv-card";
  var h=document.createElement("b"); h.textContent=title; c.appendChild(h);
  rows.forEach(function(r){ if(r[1]==null || r[1]==="") return;
    var d=document.createElement("div"); d.className="srv-kv";
    var k=document.createElement("span"); k.textContent=r[0]; var v=document.createElement("span"); v.textContent=String(r[1]);
    d.appendChild(k); d.appendChild(v); c.appendChild(d); });
  if(note){ var n=document.createElement("small"); n.className="hint"; n.textContent=note; c.appendChild(n); }
  return c;
}
function srvLoad(){
  var box=$("srvStats"); if(!box) return;
  var get=function(p){ return fetch(p,{cache:"no-store"}).then(function(r){ return r.json().then(function(j){ return {ok:r.ok, code:r.status, j:j}; }); }).catch(function(){ return {ok:false, code:0, j:{}}; }); };
  Promise.all([get("/api/server-stats"), get("/api/arena/server-stats")]).then(function(res){
    var me=res[0].j||{}, ar=res[1];
    box.textContent="";
    box.appendChild(srvCard("This HQ (your machine)", [
      ["Memory", me.rssMb!=null ? me.rssMb+" MB"+(me.machineRamMb ? " of "+Math.round(me.machineRamMb/1024)+" GB" : "") : null],
      ["CPU", me.cpuPct!=null ? me.cpuPct+"% of a core" : null],
      ["Uptime", me.uptimeSecs!=null ? srvFmtUp(me.uptimeSecs) : null],
      ["Load", me.load ? me.load.join(" · ")+" ("+me.cpus+" cores)" : null],
      ["Threads", me.threads], ["Version", me.version ? me.version+" · Python "+me.python : null]]));
    if(ar.ok){ var a=ar.j;
      box.appendChild(srvCard("Arena ("+(a.impl==="py" ? "Python" : a.impl)+")", [
        ["Memory", a.rssMb!=null ? a.rssMb+" MB" : null], ["CPU", a.cpuPct!=null ? a.cpuPct+"% of a core" : null],
        ["Uptime", srvFmtUp(a.uptimeSecs)], ["Online", a.online+" in "+a.rooms+" room"+(a.rooms===1?"":"s")],
        ["Game loops", a.gameLoops+" of "+a.gameLoopsMax+(a.overruns ? " · "+a.overruns+" late ticks" : "")],
        ["Database", a.db+(a.dbMb!=null ? " · "+a.dbMb+" MB" : "")], ["Version", a.version]]));
    } else {
      box.appendChild(srvCard("Arena", [], ar.code===404 ? "This Arena doesn't report stats yet: update it with ops/release.sh."
        : ar.code===400 ? "Not paired: connect in the Arena tab." : "The Arena didn't answer."));
    }
  });
}
(function(){
  var back=$("settingsBack"); if(!back) return;
  new MutationObserver(function(){
    var open=back.classList.contains("open");
    if(open && !SRV.timer){ srvLoad(); SRV.timer=setInterval(srvLoad, 5000); }
    else if(!open && SRV.timer){ clearInterval(SRV.timer); SRV.timer=null; }
  }).observe(back, {attributes:true, attributeFilter:["class"]});
})();

/* ---- one volume for every game sound (HQ 2.1 sound pack) ---- */
function sfxVolumeNow(){
  if(window.HQV && HQV.engine) return HQV.engine.sfxVolume();
  var v=null; try { v=localStorage.getItem("hq_sfx"); } catch(e){}
  if(v===null || v==="") return (document.documentElement.classList.contains("hq-calm") || (window.matchMedia && matchMedia("(prefers-reduced-motion: reduce)").matches)) ? 0 : 0.5;
  return Math.max(0, Math.min(1, parseFloat(v) || 0));
}
(function(){
  var r=$("setSfx"), lbl=$("setSfxVal"); if(!r) return;
  function show(){ var v=Math.round(sfxVolumeNow()*100); r.value=v; lbl.textContent = v ? v+"%" : "off"; }
  var back=$("settingsBack"); if(back) new MutationObserver(function(){ if(back.classList.contains("open")) show(); }).observe(back, {attributes:true, attributeFilter:["class"]});
  r.addEventListener("input", function(){
    var v=(+r.value||0)/100;
    if(window.HQV && HQV.engine) HQV.engine.sfxSetVolume(v); else { try { localStorage.setItem("hq_sfx", String(v)); } catch(e){} }
    lbl.textContent = v ? Math.round(v*100)+"%" : "off";
  });
  r.addEventListener("change", function(){ if(window.HQV && HQV.engine) HQV.engine.sfx("coin"); });
})();
// Work signals: the PR switch only means something while the main one is on.
(function(){ var ws=$("setWorkSignals"), wp=$("setWorkSignalsPRs"); if(ws && wp) ws.addEventListener("change", function(){ wp.disabled=!ws.checked; if(!ws.checked) wp.checked=false; }); })();
