/* ---------- detail drawer ---------- */
var DRAWER_OPEN = false, LAST_FOCUS = null, DRAWER_SID = null, DRAWER_SESS = null,
    DRAWER_TAIL = null, DRAWER_TL_LEN = 0, DRAWER_LAST = null;
// Drawer energy + snacks: the last detail payload, what #dwCare / the avatar last showed,
// and whether to move focus onto the best snack once the pantry arrives.
var DRAWER_DETAIL = null, DRAWER_CARE_SIG = "", DRAWER_AV_SIG = "", DRAWER_CARE_FOCUS = false, CARE_FOCUS_KEEP = null;
// CARE_BUSY {sid,kind}: a snack or buy in flight from the drawer. CARE_NOTE {sid,text,until}: an inline outcome.
var CARE_BUSY = null, CARE_NOTE = null, EAT_INFLIGHT = {};
// Build the timeline rows into a container (reused by the live tail).
function fillTimeline(wrap, tl){
  wrap.innerHTML="";
  (tl||[]).slice(-60).forEach(function(ev){
    var kind = (ev.kind||ev.type||"").toLowerCase();
    var cls = kind==="you"||kind==="user" ? "you" : (kind==="tool"||kind==="tool_use" ? "tool" : "claude");
    var row = el("div","trow "+cls);
    var tk2 = el("div","tk");
    var who = (typeof assistantLabel==="function") ? assistantLabel(DRAWER_DETAIL||DRAWER_SESS) : "Claude";
    tk2.textContent = cls==="you"?"You":(cls==="tool"?(ev.tool||ev.name||"tool"):who);
    var tx = el("div","tx"); tx.textContent = ev.text||ev.detail||ev.summary||"";
    row.appendChild(tk2); row.appendChild(tx); wrap.appendChild(row);
  });
  DRAWER_TL_LEN = (tl||[]).length;
}
// While a WORKING session's drawer is open, poll its detail so the timeline,
// the in-flight tool line, and tokens update live (console-style).
function startDrawerTail(sess){
  stopDrawerTail();
  if((sess.status||"")!=="working") return;
  DRAWER_TAIL = setInterval(function(){
    if(!DRAWER_OPEN || !DRAWER_SID){ stopDrawerTail(); return; }
    fetch('/api/session/'+encodeURIComponent(DRAWER_SID),{cache:"no-store"})
      .then(function(r){ return r.ok?r.json():null; })
      .then(function(d){
        if(!d || !DRAWER_OPEN) return;
        var wrap = $("dwTimeline"); var tl = d.timeline||[];
        if(wrap && tl.length!==DRAWER_TL_LEN){
          var atBottom = wrap.scrollTop+wrap.clientHeight >= wrap.scrollHeight-8;
          fillTimeline(wrap, tl);
          if(atBottom) wrap.scrollTop = wrap.scrollHeight;   // follow new activity
        }
        // refresh token grid + status subline if present
        var tk=d.tokens||{};
        var out=$("dwTokOut"); if(out) out.textContent=fmtTok(tk.output||0);
        var cost=$("dwTokCost"); if(cost) cost.textContent=fmtCost(tk.estCostUSD||0);
        if(d.status && d.status!=="working"){   // it stopped working — settle the UI
          var dot=$("dwLiveDot"); if(dot){ dot.classList.add("done"); dot.title="Idle"; }
          stopDrawerTail();
        }
      }).catch(function(){});
  }, 3000);
}
function stopDrawerTail(){ if(DRAWER_TAIL){ clearInterval(DRAWER_TAIL); DRAWER_TAIL=null; } }
// opts.care: open on the creature's energy + snacks (the card's Snack / Revive, Next up's Care).
function openDrawer(sess, opts){
  var dw = $("drawer"), bd = $("backdrop");
  LAST_FOCUS = document.activeElement;
  DRAWER_SID = sess.sessionId || sess.id; DRAWER_SESS = sess;
  DRAWER_DETAIL = null; DRAWER_CARE_SIG = ""; DRAWER_CARE_FOCUS = !!(opts && opts.care); CARE_NOTE = null;
  var cr = sess.creature||{};
  paintCreature($("dwAv"), cr, 76); DRAWER_AV_SIG = drawerAvSig(cr);
  $("dwTitle").textContent = sess.title || "Untitled session";
  $("dwSub").textContent = (sess.status||"")+" · "+(sess.folder||"~");
  $("dwBody").innerHTML = '<div class="dw-loading">'+LOADING_HTML+'</div>';
  bd.classList.add("open"); dw.classList.add("open"); dw.setAttribute("aria-hidden","false");
  DRAWER_OPEN = true; document.body.style.overflow="hidden";
  $("dwClose").focus();
  if(ARENA.paired && Date.now()-PANTRY.at > 60000) pantryLoad();
  fetch('/api/session/'+encodeURIComponent(DRAWER_SID),{cache:"no-store"})
    .then(function(r){ if(!r.ok) throw new Error(r.status); return r.json(); })
    .then(function(d){ renderDrawer(sess, d); startDrawerTail(sess); if(DRAWER_CARE_FOCUS) drawerFocusCare(true); })
    .catch(function(){ $("dwBody").innerHTML='<div class="dw-loading">Could not load session detail.</div>'; });
}
function closeDrawer(){
  if(!DRAWER_OPEN) return;
  var sid=DRAWER_SID;
  stopDrawerTail(); DRAWER_SID=null; DRAWER_SESS=null; DRAWER_DETAIL=null; DRAWER_CARE_FOCUS=false; CARE_NOTE=null;
  $("drawer").classList.remove("open"); $("backdrop").classList.remove("open");
  $("drawer").setAttribute("aria-hidden","true");
  DRAWER_OPEN=false; document.body.style.overflow="";
  drawerReturnFocus(sid);
}
// Back to whatever opened the drawer. That can be gone by now: a snack changes the creature's
// state, which rebuilds the party, card and all. Then it's the same session's card: its button of
// the same kind (data-act, e.g. the Snack one) when the new card still has it, else the card.
function drawerReturnFocus(sid){
  var t=LAST_FOCUS; LAST_FOCUS=null;
  if(t && t.focus && document.contains(t)){ t.focus(); return; }
  var cards=document.querySelectorAll("#party .card"), card=null;
  for(var i=0;i<cards.length;i++){ if(sid && cards[i].getAttribute("data-sid")===sid){ card=cards[i]; break; } }
  if(!card) return;
  var act=(t && t.getAttribute) ? t.getAttribute("data-act") : null, b=null;
  if(act){ var bs=card.querySelectorAll(".actbtn"); for(var k=0;k<bs.length;k++){ if(bs[k].getAttribute("data-act")===act && !bs[k].disabled){ b=bs[k]; break; } } }
  (b||card).focus();
}
function dwSec(title){
  var s = el("div","dw-sec");
  var h = el("h5"); h.textContent=title; s.appendChild(h);
  return s;
}
function renderDrawer(sess, d){
  d = d||{};
  DRAWER_LAST = d;                       // remember detail so setEvoChoice can re-render
  if(sess && sess.creature) sess.creature._sid = sess.sessionId || sess.id;  // stamp for branch/mega choice
  var body = $("dwBody"); body.innerHTML="";
  var tk = d.tokens||sess.tokens||{};

  // status line refresh
  $("dwSub").textContent = assistantLabel(d.source?d:sess)+" · "+((d.status||sess.status||"")+"")+(tk.model?" · "+tk.model:"");

  // creature / evolution
  var crD = d.creature || sess.creature || {};
  crD._sid = sess.sessionId || sess.id;   // stamp so branch/mega choice resolves in the drawer preview
  var secCr = dwSec("Creature"); secCr.id = "dwCrSec";
  var crBox = el("div","evowrap"); crBox.style.marginTop="0";
  crBox.innerHTML =
    '<div class="crline"><span class="species">'+esc(creatureSpecies(crD))+'</span>'+typeBadgeHTML(crD)+
    '<span class="stagepill">'+esc(creatureStageName(crD))+(crD.shiny?' <span class="shinystar" title="Shiny">✦</span>':'')+'</span></div>'+
    evobarHTML(crD);
  if(isPokePack()){ var evln=el("div","dw-evoline"); evln.innerHTML=evoLineHTML(crD, 54); crBox.appendChild(evln);
    if(pokeHasChoice(crD)){ var pb=el("button","hbtn small dw-choose"); pb.type="button";
      pb.innerHTML=ico("book")+"Choose path";
      pb.addEventListener("click",function(){ crD._sid=sess.sessionId||sess.id; openEvoPicker(crD); });
      crBox.appendChild(pb); } }
  secCr.appendChild(crBox);
  // energy + snacks (omitted when the creature has no fatigue, e.g. turned off in Settings)
  DRAWER_DETAIL = d;
  var care = renderCare(drawerCareSess(), drawerCareCreature());
  if(care) secCr.appendChild(care);
  DRAWER_CARE_SIG = careSig();
  body.appendChild(secCr);
  // keep the drawer avatar in sync with the freshest creature data
  paintCreature($("dwAv"), crD, 76); DRAWER_AV_SIG = drawerAvSig(crD);
  // Overflow Vial: a fully evolved (Apex) creature keeps earning past 70 prompts, and the
  // vial banks half of it — show what this creature has poured into the vial over its life
  var vp=(sess && (sess.vialPoured|0))||0;
  if(vp>0){ var vb=el("div","dw-vial"); vb.setAttribute("role","note");
    vb.textContent="⚗ "+fmtTok(vp)+" XP poured into the Overflow Vial";
    vb.title="Apex surplus: this session kept earning past 70 prompts, and the vial banked half of every drip.";
    secCr.appendChild(vb); }

  // tokens + cost
  var secT = dwSec("Usage");
  var grid = el("div","dw-tokgrid");
  [["list-price est.",fmtCost(tk.estCostUSD),"dwTokCost"],["generated",fmtTok(tk.output),"dwTokOut"],["processed",fmtTok(tk.total),null]].forEach(function(p){
    var c = el("div","dw-tok"); var n=el("div","n"); if(p[2]) n.id=p[2]; n.textContent=p[1]; var l=el("div","l"); l.textContent=p[0];
    c.appendChild(n); c.appendChild(l); grid.appendChild(c);
  });
  secT.appendChild(grid);
  body.appendChild(secT);

  // history / timing
  if(d.firstActivity){
    var secH=dwSec("History");
    var hg=el("div","dw-tokgrid");
    [["started",relTime(d.firstActivity)+" ago"],
     ["active days",String(d.activeDays||0)],
     ["spans",(d.spanDays||0)+"d"]].forEach(function(p){
      var c=el("div","dw-tok");var n=el("div","n");n.textContent=p[1];
      var l=el("div","l");l.textContent=p[0];c.appendChild(n);c.appendChild(l);hg.appendChild(c);
    });
    secH.appendChild(hg); body.appendChild(secH);
  }

  // resume command
  // action buttons (resume / reveal / focus / close)
  if(d.sessionId){
    var secA = dwSec("Actions");
    var arow = el("div","cardactions");
    var live = (STATE&&STATE.sessions||[]).filter(function(x){return x.sessionId===d.sessionId;})[0];
    function dwAct(label,cls,fn){ var b=el("button","actbtn"+(cls?" "+cls:"")); b.type="button"; setActLabel(b,label);
      b.addEventListener("click",fn); arow.appendChild(b); }
    dwAct("▶ Resume",null,function(){ sessResume(d.sessionId); });
    dwAct("📂 Reveal",null,function(){ sessReveal(d.sessionId); });
    dwAct("📜 Read full transcript",null,function(){ openTranscript(d.sessionId, sess.title||d.title); });
    var projSlug = looksLikeSlug(d.folder) ? d.folder : (looksLikeSlug(sess.folder) ? sess.folder : "");
    if(projSlug){ dwAct("📊 Project deep-dive",null,function(){ closeDrawer(); openProject(projSlug); }); }
    dwAct("✏️ Rename",null,function(){
      var cur=(live&&live.alias)||"";
      var v=window.prompt("Rename this session in Claude HQ\n(leave blank to reset to the auto title):", cur);
      if(v===null) return;
      postMeta({sessionId:d.sessionId, name:v.trim()}).then(function(r){
        if(r.ok){ toast(v.trim()?("✏️ Renamed to "+v.trim()):"↩ Reset to auto title","level");
                  applyMetaLocal(d.sessionId,{}); forceRefresh(); closeDrawer(); }
        else toast("⚠ "+((r.j&&r.j.error)||"rename failed"),"ach");
      }).catch(function(){ toast("⚠ rename failed","ach"); });
    });
    if(live && live.status==="working"){ dwAct("⤢ Focus",null,function(){ closeDrawer(); openFocus(d.sessionId); }); }
    if(live && live.kind==="interactive" && live.pid){ dwAct("✕ Close","danger",function(){ sessClose(live); }); }
    secA.appendChild(arow); body.appendChild(secA);

    // tags + note editor (saved via /api/meta)
    var secM = dwSec("Tags & note");
    var curTags = (live && live.tags) || d.tags || sess.tags || [];
    var curNote = (live && live.note!=null) ? live.note : (d.note!=null ? d.note : (sess.note||""));
    var ti = el("input"); ti.type="text"; ti.className="dw-tagin";
    ti.placeholder="tags, comma separated"; ti.value=(curTags||[]).join(", ");
    ti.setAttribute("aria-label","Tags");
    var na = el("textarea"); na.className="dw-note"; na.placeholder="Private note…"; na.value=curNote||"";
    na.setAttribute("aria-label","Note");
    var save = el("button","hbtn on"); save.type="button"; save.textContent="Save tags & note";
    save.addEventListener("click",function(){
      var newTags = ti.value.split(",").map(function(x){return x.trim();}).filter(Boolean);
      postMeta({sessionId:d.sessionId, tags:newTags, note:na.value}).then(function(r){
        if(r.ok){ applyMetaLocal(d.sessionId,{tags:newTags,note:na.value}); toast("✓ Saved tags & note","level"); if(STATE) renderParty(STATE.sessions); }
        else toast("⚠ "+(r.j.error||"save failed"),"ach");
      }).catch(function(){ toast("⚠ save failed","ach"); });
    });
    secM.appendChild(ti); secM.appendChild(na); secM.appendChild(save);
    body.appendChild(secM);
  }

  if(d.resumeCmd){
    var secR = dwSec("Resume");
    var rr = el("div","dw-resume");
    var code = el("code"); code.textContent = d.resumeCmd;
    var btn = el("button","hbtn"); btn.type="button"; btn.textContent="Copy";
    btn.addEventListener("click",function(){
      var done=function(){ btn.textContent="Copied ✓"; setTimeout(function(){btn.textContent="Copy";},1400); };
      if(navigator.clipboard&&navigator.clipboard.writeText){ navigator.clipboard.writeText(d.resumeCmd).then(done,done); }
      else { try{ var ta=el("textarea"); ta.value=d.resumeCmd; document.body.appendChild(ta); ta.select(); document.execCommand("copy"); ta.remove(); done(); }catch(e){} }
    });
    rr.appendChild(code); rr.appendChild(btn); secR.appendChild(rr);
    body.appendChild(secR);
  }

  // sparkHourly
  var sh = d.sparkHourly||[];
  if(sh.length){
    var secS = dwSec("24h activity");
    var mx = Math.max.apply(null, sh.concat([1]));
    var pk=-1,pkv=-1; sh.forEach(function(v,i){ if((v||0)>pkv){pkv=v||0;pk=i;} });
    var sw = el("div","dw-spark"); sw.setAttribute("aria-hidden","true");
    sh.forEach(function(v,i){
      var b=el("i"); if(i===pk&&pkv>0)b.className="peak";
      b.style.height=Math.max(1,Math.round((v||0)/mx*44))+"px";
      b.title=String(i).padStart(2,"0")+":00 — "+(v||0); sw.appendChild(b);
    });
    secS.appendChild(sw); body.appendChild(secS);
  }

  // timeline (live-tails while a working session's drawer is open)
  var tl = d.timeline||[];
  if(tl.length || (d.status||sess.status)==="working"){
    var secL = dwSec("Activity");
    if((d.status||sess.status)==="working"){
      var lb = el("span","orb sm"); lb.dataset.orb="working"; lb.id="dwLiveDot"; lb.title="Live — thinking";
      lb.setAttribute("role","img"); lb.setAttribute("aria-label","live"); secL.querySelector("h5").appendChild(lb);
    }
    var wrap = el("div","tline"); wrap.id="dwTimeline";
    fillTimeline(wrap, tl);
    secL.appendChild(wrap); body.appendChild(secL);
  }

  // files
  var files = d.files||[];
  if(files.length){
    var secF = dwSec("Files touched ("+files.length+")");
    files.slice(0,40).forEach(function(f){
      var row = el("div","filerow");
      var act = fileAction(f.action);
      var fp = el("div","fp"); fp.textContent = f.path||f.file||""; fp.title = f.path||f.file||"";
      var bd2 = el("div","fbadge "+act); bd2.textContent = act;
      row.appendChild(bd2); row.appendChild(fp);
      if(f.count!=null){ var fc=el("div","fc"); fc.textContent="×"+f.count; row.appendChild(fc); }
      secF.appendChild(row);
    });
    body.appendChild(secF);
  }

  // links
  var links = d.links||sess.links||[];
  if(links.length){
    var secLk = dwSec("Links");
    var pills = el("div","pills");
    links.forEach(function(l){
      var a=el("a","pill "+(l.type==="pr"?"pr":"artifact"));
      a.href=l.url||"#"; a.target="_blank"; a.rel="noopener";
      a.textContent=(l.type==="pr"?"🔗 ":"✨ ")+(l.label||l.type||"link");
      pills.appendChild(a);
    });
    secLk.appendChild(pills); body.appendChild(secLk);
  }

  // full last reply
  var full = d.lastReplyFull || sess.lastReply;
  if(full){
    var secR2 = dwSec("Last reply");
    var pre = el("div","dw-full"); pre.textContent = full;
    secR2.appendChild(pre); body.appendChild(secR2);
  }
}

/* ---------- drawer: creature energy + snacks ----------
   #dwCare lives in the drawer's Creature section. It is rebuilt only when what it shows
   changes (careSig), so keyboard focus survives the ~1.5 s SSE ticks. Which session ate is
   sent to the LOCAL dashboard only; the Arena just sees "one Rice Ball eaten". */
function sessBySid(sid){ var ss=(STATE&&STATE.sessions)||[]; for(var i=0;i<ss.length;i++){ if(ss[i].sessionId===sid) return ss[i]; } return null; }
function drawerCareSess(){ return sessBySid(DRAWER_SID) || DRAWER_SESS; }
function drawerCareCreature(){
  var live=sessBySid(DRAWER_SID);
  return (live && live.creature) || (DRAWER_DETAIL && DRAWER_DETAIL.creature) || (DRAWER_SESS && DRAWER_SESS.creature) || null;
}
// What the drawer avatar shows: repaint only when this changes (a repaint restarts an animated sprite).
function drawerAvSig(cr){ cr=cr||{}; var f=fzOf(cr);
  return [cfg().creaturePack, cr.species, cr.stage, cr.shiny?1:0, cr.name, cr.index, cr.hue, f?f.state:""].join("|"); }
function pantryErr(res, fallback){ var e=res && res.j && (res.j.error || res.j.detail); return (typeof e==="string" && e) ? e.slice(0,200) : fallback; }
function careSig(){
  var f=fzOf(drawerCareCreature()); if(!f) return "none|"+PANTRY.store;
  var p=pendingEatFor(DRAWER_SID), lm=f.lastMeal;
  var note=(CARE_NOTE && CARE_NOTE.sid===DRAWER_SID && Date.now()<CARE_NOTE.until) ? CARE_NOTE.text : "";
  return [f.state, fzPct(f), f.loadMins, f.mayFaint?1:0, f.phase, f.restMins, f.restInMins, f.streakMins,
          (lm && lm.at) ? lm.at+relTime(lm.at) : "", p ? p.kind+(EAT_INFLIGHT[p.rid]?"*":"") : "",
          PANTRY.rev, PANTRY.store, PANTRY.loading?1:0, CARE_BUSY?CARE_BUSY.kind:"", note, cfg().creaturePack].join("|");
}
// Best fit: a revive when fainted (one you own, else the cheapest you can buy); else the smallest
// snack that gets it back under "tired", else the biggest. Snacks you own or can buy today win.
function careBestKind(f, cat, items, coins){
  function canBuy(it){ return it.inStock!==false && coins>=it.price; }
  if(f.state==="unconscious"){
    var rv=cat.filter(function(it){ return it.revives; });
    var mine=rv.filter(function(it){ return (items[it.kind]|0)>0; });
    var pool=(mine.length ? mine : rv.filter(canBuy)).slice().sort(function(a,b){ return a.price-b.price; });
    return pool.length ? pool[0].kind : (rv.length ? rv[0].kind : null);
  }
  var food=cat.filter(function(it){ return !it.revives; });
  var usable=food.filter(function(it){ return (items[it.kind]|0)>0 || canBuy(it); });
  var pool2=usable.length ? usable : food, load=fzMins(f.loadMins);
  var fits=pool2.filter(function(it){ return load-it.restoreMins < FZ_TIRED_MINS; }).sort(function(a,b){ return (a.restoreMins-b.restoreMins) || (a.price-b.price); });
  if(fits.length) return fits[0].kind;
  pool2=pool2.slice().sort(function(a,b){ return (b.restoreMins-a.restoreMins) || (a.price-b.price); });
  return pool2.length ? pool2[0].kind : null;
}
function careMeals(sid, f, cr, pending){
  var box=el("div","dw-meals"); box.id="dwMeals"; box.setAttribute("role","group"); box.setAttribute("aria-label","Snacks");
  var st=PANTRY.store, j=PANTRY.j;
  function say(t){ var s=el("span"); s.textContent=t; box.appendChild(s); }
  function act(label, key, fn){ var b=el("button","hbtn"); b.type="button"; b.textContent=label; b.setAttribute("data-act",key); b.addEventListener("click",fn); box.appendChild(b); return b; }
  if(f.state==="rested"){ say("Full of energy: no snack needed."); return box; }
  if(st==="unpaired"){
    say("Rest restores energy. Connect the Arena to get 5 Poke Coins a day for snacks.");
    act("Open Arena","arena",function(){ openArenaSection("arenaSetup", !ARENA.paired); });
    return box;
  }
  if(st==="unsupported"){ say("Rest restores energy. This Arena server doesn\u2019t have the store yet."); return box; }
  if(st==="restart"){ say("Rest restores energy. Restart Claude HQ, then reload this page: snacks need the new version."); return box; }
  if(st==="reload"){ say("Rest restores energy. Claude HQ restarted since this page loaded: reload the page for snacks."); act("Reload page","reload-page",pageReload); return box; }
  if(st==="error" && !j){ say("Pantry unreachable right now. Rest still works."); act("Retry","reload",pantryRetry); return box; }
  if(!j){ box.insertAdjacentHTML("beforeend", orb("xs","searching")); say("Checking your pantry\u2026"); return box; }
  var offline = st!=="ok";
  if(offline) box.classList.add("offline");
  var cat=pantryCatalog(), items=j.items||{}, coins=j.coins|0, ko=f.state==="unconscious";
  var load=fzMins(f.loadMins), now=fzPct(f), nm=creatureSpecies(cr);
  var best=careBestKind(f, cat, items, coins);
  // What you own, plus the best fit when you own none of it: the whole shelf is in the Store tab.
  var show=cat.filter(function(it){ return (items[it.kind]|0)>0 || it.kind===best; });
  if(!show.length) say("No snacks in your bag that help right now. The Store has plenty.");
  show.forEach(function(it){
    var own=Math.max(0, items[it.kind]|0), fits = ko ? it.revives : !it.revives;
    var after = it.revives ? Math.min(load, fzMins(it.wakeToMins)) : Math.max(0, load-fzMins(it.restoreMins));
    var to = Math.max(0, Math.min(100, Math.round(100*(1-after/FZ_SCALE_MINS))));
    var buy = fits && own===0 && coins>=it.price && it.inStock!==false;
    var b=el("button","meal-btn"+(buy?" buy":"")); b.type="button"; b.setAttribute("data-kind", it.kind);
    if(CARE_BUSY && CARE_BUSY.sid===sid && CARE_BUSY.kind===it.kind) b.insertAdjacentHTML("beforeend", orb("xs"));
    var em=el("span"); em.setAttribute("aria-hidden","true"); em.textContent=it.emoji;
    var label;
    if(buy){
      b.appendChild(document.createTextNode("Buy ")); b.appendChild(em);
      var pr=el("span","mb-p"); pr.textContent=" \u00B7 \uD83E\uDE99"+it.price; b.appendChild(pr);
      label="Buy "+foodA(it)+" for "+coinsN(it.price);
      b.addEventListener("click",function(){ careBuy(sid, it.kind); });
    } else {
      b.appendChild(em);
      var nmS=el("span"); nmS.textContent=it.name; b.appendChild(nmS);
      var n=el("span","mb-n"); n.textContent="\u00D7"+own; b.appendChild(n);
      if(fits){ var pv=el("span","mb-p"); pv.textContent=(it.revives?"wakes \u2192 ":"\u2192 ")+to+"%"; b.appendChild(pv); }
      if(!fits){
        b.disabled=true; b.title = it.revives ? "For fainted creatures" : "It\u2019s fainted: only a revive item works";
        label=it.name+": "+(it.revives ? "for fainted creatures" : "it\u2019s fainted, only a revive item works");
      } else if(own>0){
        label="Give "+nm+" "+foodA(it)+": "+(it.revives?"wakes it, ":"")+"energy "+now+"% to "+to+"%, "+own+" left";
        b.addEventListener("click",function(){ pantryEat(sid, it.kind, b); });
      } else {
        var need=it.price-coins;
        b.disabled=true; b.title="Need "+need+" more Poke Coin"+(need===1?"":"s");
        label=it.name+": need "+need+" more Poke Coin"+(need===1?"":"s");
      }
    }
    if(it.kind===best && !b.disabled){
      b.classList.add("best");
      var sr=el("span","sr-only"); sr.textContent=" (best fit)"; b.appendChild(sr);
      label+=" (best fit)";
    }
    if(CARE_BUSY || pending || offline) b.disabled=true;
    b.setAttribute("aria-label", label+(offline?" (offline)":""));
    box.appendChild(b);
  });
  if(offline){ say("(offline) Pantry unreachable right now. Rest still works."); act("Retry","reload",pantryRetry); }
  return box;
}
function renderCare(sess, cr){
  var f=fzOf(cr); if(!f) return null;
  var sid=(sess && sess.sessionId) || DRAWER_SID, ko=f.state==="unconscious", rm=fzMins(f.restMins);
  var box=el("div","dw-care"); box.id="dwCare"; box.tabIndex=-1;
  box.insertAdjacentHTML("beforeend", energyMeterHTML(f, "Energy "+fzPct(f)+"% \u00B7 "+fzLabel(f.state)));
  var line;
  if(f.phase==="active"){
    line = f.state==="rested" ? "Working \u00B7 full of energy"
         : "Working"+(fzMins(f.streakMins)>=60 ? " for "+fmtMins(f.streakMins) : "")+" \u00B7 a ~"+rm+"m break would "+(ko?"wake it":"restore it");
  } else if(f.phase==="pause"){
    line = "Catching its breath \u00B7 resting starts in "+fzMins(f.restInMins)+"m";
  } else {
    line = ko ? "Resting \u00B7 wakes in ~"+rm+"m" : (f.state==="rested" ? "Rested" : "Resting \u00B7 rested in ~"+rm+"m");
  }
  var pl=el("p","care-line"); pl.textContent=line; box.appendChild(pl);
  var lm=f.lastMeal, li=(lm && typeof lm==="object") ? foodInfo(lm.kind) : null;
  if(li){
    var ago=relTime(lm.at), lp=el("p","care-line");
    lp.textContent="Last snack: "+li.emoji+" "+li.name+(ago==="\u2014" ? "" : " \u00B7 "+(ago==="just now" ? ago : ago+" ago"));
    box.appendChild(lp);
  }
  var p=pendingEatFor(sid);
  if(p){
    var pi=foodInfo(p.kind), pr=el("div","care-pending"), pt=el("span");
    if(PANTRY.store==="reload"){
      // A retry from this page would be turned away too; the snacks row below has the Reload.
      pt.textContent=pi.emoji+" "+pi.name+" on its way \u00B7 reload the page to finish it (it won\u2019t be eaten twice)"; pr.appendChild(pt);
    } else {
      pt.textContent=pi.emoji+" "+pi.name+" on its way \u00B7 waiting for the Arena\u2026"; pr.appendChild(pt);
      var rb=el("button","hbtn"); rb.type="button"; rb.textContent="Retry"; rb.setAttribute("data-act","retry");
      rb.setAttribute("aria-label","Retry the "+pi.name+" (safe: it won\u2019t be eaten twice)");
      rb.disabled=!!EAT_INFLIGHT[p.rid];
      rb.addEventListener("click",function(){ pantryEat(p.sid, p.kind, rb, p.rid, true); });
      pr.appendChild(rb);
    }
    box.appendChild(pr);
  }
  if(CARE_NOTE && CARE_NOTE.sid===sid && Date.now()<CARE_NOTE.until){
    var nt=el("p","care-note"); nt.textContent=CARE_NOTE.text; box.appendChild(nt);   // announced by careNote()
  }
  box.appendChild(careMeals(sid, f, cr, !!p));
  if(PANTRY.store==="ok" && PANTRY.j){
    var sl=el("button","care-link"); sl.type="button"; sl.setAttribute("data-act","store");
    sl.textContent="\uD83E\uDE99 "+(PANTRY.j.coins|0)+" \u00B7 Store \u2192";
    sl.setAttribute("aria-label", "You have "+coinsN(PANTRY.j.coins|0)+". Open the store");
    sl.addEventListener("click", openStore);
    box.appendChild(sl);
  }
  return box;
}
// Rebuild #dwCare only when what it shows changed; put focus back on the same button.
function refreshDrawerCare(force){
  if(!DRAWER_OPEN || !DRAWER_SID) return;
  var cr=drawerCareCreature();
  if(cr){ var as=drawerAvSig(cr); if(as!==DRAWER_AV_SIG){ DRAWER_AV_SIG=as; paintCreature($("dwAv"), cr, 76); } }
  var sec=$("dwCrSec"); if(!sec) return;          // the detail hasn't rendered yet
  var sig=careSig(); if(!force && sig===DRAWER_CARE_SIG) return;
  DRAWER_CARE_SIG=sig;
  var old=$("dwCare"), a=document.activeElement, had=false, keep=null;
  if(old && a && old.contains(a)){ had=true; keep=a.getAttribute("data-kind")||a.getAttribute("data-act")||CARE_FOCUS_KEEP; }
  var fresh=renderCare(drawerCareSess(), cr);
  if(old && fresh) old.parentNode.replaceChild(fresh, old);
  else if(old) old.parentNode.removeChild(old);
  else if(fresh) sec.appendChild(fresh);
  if(!fresh) return;
  if(had){
    var t = keep ? (fresh.querySelector('[data-kind="'+keep+'"]:not([disabled])') || fresh.querySelector('[data-act="'+keep+'"]:not([disabled])')) : null;
    CARE_FOCUS_KEEP = t ? null : keep;       // a busy (disabled) button: hold focus on the section until it's back
    try{ (t||fresh).focus({preventScroll:true}); }catch(e){ (t||fresh).focus(); }
  } else if(DRAWER_CARE_FOCUS) drawerFocusCare(false);
}
// Opened with {care:true}: bring the section into view and focus the best-fit snack. If the
// pantry is still loading, try again when it lands (unless focus has moved on meanwhile).
function drawerFocusCare(scroll){
  var c=$("dwCare"); if(!c){ DRAWER_CARE_FOCUS=false; return; }
  if(scroll){ try{ c.scrollIntoView({behavior:calmMode()?"auto":"smooth", block:"nearest"}); }catch(e){} }
  var a=document.activeElement;
  if(a && a!==document.body && a!==$("dwClose") && !c.contains(a)){ DRAWER_CARE_FOCUS=false; return; }
  var b=c.querySelector(".meal-btn.best:not([disabled])") || c.querySelector(".meal-btn:not([disabled])");
  if(!b && !PANTRY.j && (PANTRY.store==="unknown" || PANTRY.loading)) return;
  DRAWER_CARE_FOCUS=false;
  b = b || c.querySelector("button:not([disabled])") || c;
  try{ b.focus({preventScroll:true}); }catch(e){ b.focus(); }
}
// An inline outcome under the snacks. #dwCare is rebuilt around it, so it's no live region of its
// own: it's announced here, unless a toast (the polite #toastWrap) already says the same thing.
function careNote(sid, text, toasted){
  CARE_NOTE={sid:sid, text:text, until:Date.now()+15000}; refreshDrawerCare(true);
  if(!toasted) announce(text);
}
// From the drawer to a section of the Arena view. Whatever opened the drawer sits in a view that
// is about to hide, so focus goes to the section itself (tabindex=-1), shown right away when
// `show` (loadArena's status check settles the rest).
function openArenaSection(id, show){
  closeDrawer(); setView("arena");
  var w=$(id); if(!w) return;
  if(show) w.classList.remove("hidden");
  focusQuiet(w);
  try{ w.scrollIntoView({behavior:calmMode()?"auto":"smooth", block:"start"}); }catch(e){}
}
function openStore(){ closeDrawer(); setView("store"); focusQuiet(document.querySelector('#svList .sv-row[tabindex="0"]') || $("svDialog")); }
// The drawer's Buy (shown only when you own none of it). A lost buy of the same kind is replayed
// first, with its own requestId, so it can't be bought twice. If that replay says the earlier buy
// had landed, nothing was bought now: then you already have one (done), or you had eaten it and
// this click is for a new one, which gets a new requestId.
function careBuy(sid, kind){
  if(CARE_BUSY) return;
  CARE_BUSY={sid:sid, kind:kind}; refreshDrawerCare(true);
  var it=foodInfo(kind);
  function done(r){
    CARE_BUSY=null;
    if(r.ok) announce("Bought "+foodA(it)+". "+coinsN(PANTRY.j ? PANTRY.j.coins|0 : 0)+" left.");
    else if(r.lost) careNote(sid, "Couldn\u2019t confirm the "+it.name+". Retry it from the store: it won\u2019t be bought twice.");
    else if(r.stale) careNote(sid, PANTRY_RELOAD_MSG);
    else if(r.msg) careNote(sid, r.msg, true);   // pantryBuy toasted it
    refreshDrawerCare(true);
  }
  var lost=storeRetryRid(kind);
  pantryBuy(kind, lost).then(function(r){
    if(!(lost && r.ok && r.replayed)) return done(r);
    var own=(PANTRY.j && PANTRY.j.items) ? PANTRY.j.items[kind]|0 : 0;
    if(own>0){
      CARE_BUSY=null;
      careNote(sid, "Your earlier "+it.name+" purchase had gone through: you have "+own+". Nothing more was bought.");
      return;
    }
    return pantryBuy(kind, null).then(done);
  });
}

/* Pending snacks: written BEFORE the eat request so a reload (or a lost response) resumes it with
   the same requestId. The retry is safe: the server replays the id, and the meal is recorded at
   the server's original time. localStorage "hq_pending_eats", at most 5, dropped after 15 min. */
var PENDING_EAT_TTL = 900000;
function pendingEatsLoad(){
  var a=[]; try{ a=JSON.parse(localStorage.getItem("hq_pending_eats")||"[]"); }catch(e){ a=[]; }
  if(!Array.isArray(a)) return [];
  return a.filter(function(p){ return p && typeof p.rid==="string" && /^[A-Za-z0-9_-]{16,64}$/.test(p.rid) &&
    typeof p.sid==="string" && p.sid && isFood(p.kind) && typeof p.t==="number"; });
}
function pendingEatsSave(a){ try{ localStorage.setItem("hq_pending_eats", JSON.stringify(a.slice(-5))); }catch(e){} }
function pendingEatAdd(p){ var a=pendingEatsLoad(); if(a.some(function(x){ return x.rid===p.rid; })) return; a.push(p); pendingEatsSave(a); }
function pendingEatDrop(rid){ var a=pendingEatsLoad(), b=a.filter(function(x){ return x.rid!==rid; }); if(b.length!==a.length) pendingEatsSave(b); }
function pendingEatFor(sid){
  var now=Date.now(); return pendingEatsLoad().filter(function(p){ return p.sid===sid && now-p.t<=PENDING_EAT_TTL; })[0] || null;
}
// After each good pantry load: retry what's still pending, drop what's too old to trust.
function pantryRetryPending(){
  var now=Date.now(), a=pendingEatsLoad(), keep=[];
  a.forEach(function(p){
    if(now-p.t>PENDING_EAT_TTL){
      var s=sessBySid(p.sid);
      logEvent("\u26A0", "Couldn\u2019t confirm a snack for "+(s ? creatureSpecies(s.creature) : "a creature"));
    } else keep.push(p);
  });
  if(keep.length!==a.length) pendingEatsSave(keep);
  if(PANTRY.stale) return;   // they'd be turned away: they wait for the page to be reloaded
  keep.forEach(function(p){ if(!EAT_INFLIGHT[p.rid]) pantryEat(p.sid, p.kind, null, p.rid, true, true); });
}
// dashboard.py turning a RETRY down before asking the Arena: energy turned off in Settings, or
// no transcript with that id any more. The first try may already have been eaten on the Arena,
// so it stays pending: the next load retries it, and it's dropped after 15 min like any other.
function eatLocalRefusal(res){
  var j=res.j||{};
  return (res.status===409 && j.code==="fatigue_off") || (res.status===404 && j.error==="unknown session");
}
// The creature a snack is for: the payload's, else (a session outside the payload, opened in the
// drawer) the one the drawer shows. null when neither is at hand.
function eatCreature(sid){
  var live=sessBySid(sid); if(live && live.creature) return live.creature;
  return (DRAWER_OPEN && sid===DRAWER_SID) ? drawerCareCreature() : null;
}
// Give a creature a snack. rid is reused for every retry of the same snack; retry:true skips
// the local "is it hungry?" gate (the first try already passed it). quiet: a background retry.
function pantryEat(sid, kind, btn, rid, isRetry, quiet){
  if(!sid || !isFood(kind)) return;
  rid = rid || newRequestId();
  if(EAT_INFLIGHT[rid] || (!quiet && CARE_BUSY)) return;
  var cr0=eatCreature(sid), f0=fzOf(cr0);
  pendingEatAdd({rid:rid, sid:sid, kind:kind, t:Date.now()});
  EAT_INFLIGHT[rid]=1;
  if(!quiet) CARE_BUSY={sid:sid, kind:kind};
  refreshDrawerCare(true);
  arenaPost("/api/arena/pantry/eat", {sessionId:sid, kind:kind, requestId:rid, retry:!!isRetry}).then(function(res){
    delete EAT_INFLIGHT[rid]; if(!quiet) CARE_BUSY=null;
    if(res.status===200){ pendingEatDrop(rid); mealsTodayBump(rid); eatDone(sid, kind, res.j||{}, f0, cr0); }
    else if(res.status===202){
      // still pending: one automatic retry, then the Retry button, the next pantry load or a reload
      if(!quiet) careNote(sid, pantryErr(res, "The Arena didn\u2019t answer. Your snack is safe: Claude HQ will retry it."));
      if(!isRetry) setTimeout(function(){ pantryEat(sid, kind, null, rid, true, true); }, 2000);
    }
    else if(pantryLocalReject(res)){
      // Turned away by dashboard.py itself (this page is older than its last start): a retry stays
      // pending for after the reload (its first try may have landed); a new snack just didn't go out.
      if(!isRetry) pendingEatDrop(rid);
      pantryStale();
      if(!quiet) careNote(sid, "Claude HQ restarted since this page loaded. "+
        (isRetry ? "Reload the page to finish the snack: it won\u2019t be eaten twice." : "Nothing was eaten: reload the page to give it the snack."));
    }
    else if(isRetry && eatLocalRefusal(res)){
      if(!quiet) careNote(sid, "Couldn\u2019t finish the snack yet ("+pantryErr(res, "refused")+"). It stays saved: Claude HQ will retry it.");
    }
    else if(pantryMissState(res)){ pendingEatDrop(rid); pantrySetStore(pantryMissState(res)); }
    else if(res.status===400 && /not paired/i.test(pantryErr(res,""))){ pendingEatDrop(rid); pantrySetStore("unpaired"); }
    else if(res.status>=400 && res.status<500){
      pendingEatDrop(rid);
      var err=pantryErr(res, "That snack didn\u2019t go through");
      careNote(sid, err, true); toast("\u26A0 "+err, "ach");
    }
    refreshDrawerCare(true);
  }, function(){
    delete EAT_INFLIGHT[rid]; if(!quiet) CARE_BUSY=null;
    if(!quiet) careNote(sid, "Couldn\u2019t reach Claude HQ. Your snack is saved: retry when it\u2019s back.");
    refreshDrawerCare(true);
  });
}
// cr0: the creature as it was when the snack went out (eatCreature), in case the drawer that showed
// a session outside the payload has closed since.
function eatDone(sid, kind, j, f0, cr0){
  if(j.items) pantryApply(j);
  var info=foodInfo(kind), live=sessBySid(sid), f1=j.fatigue, cr=eatCreature(sid) || cr0 || null;
  // The server's fresh energy goes where it's shown: the payload's creature, or the drawer's (a
  // session outside the payload, which no refresh brings in).
  if(cr && f1 && typeof f1==="object" && FZ_STATES.indexOf(f1.state)>=0) cr.fatigue=f1;
  var nm=cr ? creatureSpecies(cr) : "Your creature", f=fzOf(cr);
  refreshDrawerCare(true);
  if(STATE) renderParty(STATE.sessions);
  var gain=(f0 && f) ? Math.max(0, fzMins(f0.loadMins)-fzMins(f.loadMins)) : 0;
  if(gain>0 && DRAWER_OPEN && DRAWER_SID===sid) eatFx(gain);
  var a=f0 ? fzPct(f0) : null, b=f ? fzPct(f) : null, moved=(a!=null && b!=null);
  var msg = info.revives ? "\u2728 "+nm+" woke up!"+(moved ? " Energy "+a+"% \u2192 "+b+"%" : "")
                         : info.emoji+" "+nm+" ate "+foodA(info)+(moved ? " \u00B7 energy "+a+"% \u2192 "+b+"%" : "");
  toast(msg, "level"); announce(msg);
  var title=(live && live.title) || (sid===DRAWER_SID && DRAWER_SESS && DRAWER_SESS.title) || "session";
  logEvent(info.emoji, "Gave "+nm+" "+foodA(info)+" ("+title+")");
  forceRefresh();
}
// The drawer avatar's "nom" + a floating "+45m". Calm mode: just the text, still, for 3 s.
function eatFx(gainMins){
  var av=$("dwAv"); if(!av) return;
  var g=el("span","en-gain"); g.setAttribute("aria-hidden","true"); g.textContent="+"+fmtMins(gainMins);
  if(calmMode()){ av.appendChild(g); setTimeout(function(){ if(g.parentNode) g.parentNode.removeChild(g); }, 3000); return; }
  av.classList.remove("nom"); void av.offsetWidth; av.classList.add("nom");
  g.classList.add("fly"); av.appendChild(g);
  setTimeout(function(){ av.classList.remove("nom"); if(g.parentNode) g.parentNode.removeChild(g); }, 950);
}

/* ---------- energy transitions: quiet by design ----------
   Toasts only for entering Fatigued and fainting, at most one per (session, state) every
   45 min, and creatures that tire in the same update share ONE toast (a multi-agent run
   can tire several at once). Tired and "may faint" never toast; waking up and resting are
   log-only. */
var FZ_PREV={}, FZ_BASE=false, FZ_TOAST_AT={}, FZ_TOAST_GAP=2700000;
function fzThrottled(key){ var now=Date.now(); if(FZ_TOAST_AT[key] && now-FZ_TOAST_AT[key]<FZ_TOAST_GAP) return true; FZ_TOAST_AT[key]=now; return false; }
// "Voltkit", "Voltkit and Pupitar", "Voltkit, Pupitar and 2 more"
function fzNames(list){
  var n=list.map(function(x){ return x.nm; });
  if(n.length<=2) return n.join(" and ");
  return n[0]+", "+n[1]+" and "+(n.length-2)+" more";
}
function checkFatigue(sessions){
  sessions=sessions||[];
  if(!FZ_BASE){
    sessions.forEach(function(s){ var id=s.sessionId||s.id, f=fzOf(s.creature); if(id && f && s.kind!=="archived") FZ_PREV[id]=f.state; });
    FZ_BASE=true; return;
  }
  var live={}, fainted=[], tired=[];
  sessions.forEach(function(s){
    if(s.kind==="archived") return;
    var id=s.sessionId||s.id; if(!id) return;
    var f=fzOf(s.creature); if(!f){ delete FZ_PREV[id]; return; }
    live[id]=1;
    var prev=FZ_PREV[id], cur=f.state;
    FZ_PREV[id]=cur;
    if(prev==null || prev===cur) return;
    var nm=creatureSpecies(s.creature), where=" ("+(s.title||s.name||"session")+")";
    if(cur==="unconscious"){
      if(fzThrottled(id+"|unconscious")) return;
      fainted.push({id:id, nm:nm, f:f}); logEvent("\uD83D\uDCAB", nm+" fainted"+where);
    } else if(prev==="unconscious"){
      if(!fzThrottled(id+"|woke")) logEvent("\u2728", nm+" woke up"+where);
    } else if(cur==="fatigued"){
      if(fzThrottled(id+"|fatigued")) return;
      tired.push({id:id, nm:nm, f:f}); logEvent("\uD83D\uDCA6", nm+" is fatigued"+where);
    } else if(prev==="fatigued" && cur==="rested"){
      if(!fzThrottled(id+"|rested")) logEvent("\uD83C\uDF3F", nm+" is rested again"+where);
    }
  });
  Object.keys(FZ_PREV).forEach(function(k){ if(!live[k]) delete FZ_PREV[k]; });
  var store=PANTRY.store==="ok", msg;
  if(fainted.length){
    var one=fainted.length===1, rm=fzMins(fainted[0].f.restMins);
    msg="\uD83D\uDCAB "+fzNames(fainted)+(one ? " fainted! It wakes after a ~"+rm+"m break" : " fainted! They wake after a break")+
        (store ? (one ? ", or with a Revive Tonic" : " or a Revive Tonic") : "");
    toast(msg,"ach"); announce(msg);
    if(NOTIF_ON && document.hidden && ("Notification" in window) && Notification.permission==="granted"){
      try{ new Notification("\uD83D\uDCAB "+fzNames(fainted)+" fainted", {body:one ? "It wakes after a ~"+rm+"m break" : "They wake after a break",
                                                                        silent:true, tag:"hq-faint-"+fainted[0].id}); }catch(e){}
    }
  }
  if(tired.length){
    var f0=tired[0].f;
    msg = tired.length===1
      ? "\uD83D\uDCA6 "+tired[0].nm+" is fatigued after "+fmtMins(Math.max(fzMins(f0.streakMins), fzMins(f0.loadMins)))+" of work: a short break"+(store?" or a snack":"")+" will perk it up"
      : "\uD83D\uDCA6 "+fzNames(tired)+" are fatigued: a short break"+(store?" or a snack":"")+" will perk them up";
    toast(msg,"level"); announce(msg);
  }
}

// Set when an update arrives while the tab is hidden: nothing is painted then, so the page
// renders once when it is shown again (see the visibilitychange handler).
var RENDER_DIRTY=false;
function render(){
  if(!STATE) return;
  syncConfig(STATE.config);
  STATE_AT = Date.now();
  // Stamp each live creature with its sessionId so branch/mega choice lookups (evoChoice,
  // branchFinalDex, pokeMegaForm) resolve. Object.assign clones carry _sid forward.
  (STATE.sessions||[]).forEach(function(s){ if(s.creature) s.creature._sid = s.sessionId || s.id; });
  // Picks made in another browser (or before this one) come back from the server: they win.
  if(typeof evoSeedFromServer==="function") evoSeedFromServer(STATE.sessions);
  // The Valley pauses its game when a tab starts needing you, even while hidden.
  if(typeof HQV!=="undefined" && HQV.onState){ try{ HQV.onState(STATE); }catch(e){} }
  if(typeof valleyPillSync==="function") valleyPillSync();
  if(document.hidden){
    // Hidden tab: skip all painting, but keep what reaches you outside the page current:
    // needs-you / faint notifications, the tab title, and the Arena status you share.
    RENDER_DIRTY=true;
    checkTransitions(STATE.sessions);
    checkFatigue(STATE.sessions);
    updateDocTitle(STATE.sessions);
    arenaStatusTick();
    return;
  }
  RENDER_DIRTY=false;
  seedHighWaterFromSessions(STATE.sessions);   // lift New Game+ floor before painting cards
  renderSeason(STATE.season);
  renderInsights(STATE.season);
  renderTagFilter(STATE.sessions);
  renderFolderFilter(STATE.sessions);
  renderParty(STATE.sessions);
  renderFeed(STATE.feed);
  renderAlertBanner(STATE.sessions);
  renderHealth(STATE.health);
  renderTrainerCard();
  maybeRecap();
  renderGymCard();
  renderNextUp();
  if(VIEW==="gym") renderGym();
  if(VIEW==="village") renderVillage();
  checkTransitions(STATE.sessions);
  checkCelebration(STATE.season);
  checkEvolution(STATE.sessions);
  checkFatigue(STATE.sessions);
  refreshDrawerCare();
  updateDocTitle(STATE.sessions);
  arenaStatusTick();
  if(FOCUS_ID) renderFocus();
  if(WARROOM_ON) renderWarroom();
  if(CMDK_OPEN) renderCmdk();
  $("updated").textContent = "updated "+relTime(STATE.updated);
  if(STATE.version){ var vl=$("hqVersionLine"); if(vl) vl.textContent="Claude HQ v"+STATE.version+" · github.com/RoString36222/claude-hq"; }
}

function updateDocTitle(sessions){
  var need = (sessions||[]).filter(function(s){ return s.status==="needs"; }).length;
  var work = (sessions||[]).filter(function(s){ return s.status==="working"; }).length;
  var n = need+work;
  document.title = (n>0? "("+n+") ":"")+"Claude HQ";
}

function pulse(){
  var p = $("pulse"); p.classList.remove("go"); void p.offsetWidth; p.classList.add("go");
}

function load(){
  fetch('/api/sessions',{cache:"no-store"})
    .then(function(r){ if(!r.ok) throw new Error(r.status); return r.json(); })
    .then(function(d){ applyPayload(d); })
    .catch(function(e){
      $("updated").textContent = "offline — retrying…";
    });
}

$("search").addEventListener("input",function(e){
  QUERY = e.target.value.trim();
  if(STATE) renderParty(STATE.sessions);
});
$("search").addEventListener("keydown",function(e){
  if(e.key==="Enter" && $("searchScope").value==="all"){
    e.preventDefault();
    runSearch(e.target.value.trim());
  }
});
$("searchScope").addEventListener("change",function(e){
  var all = e.target.value==="all";
  $("search").placeholder = all ? "Search all history…  (press Enter)" : "Filter cards…";
});

// sort dropdown
$("sortSel").value = SORT;
$("sortSel").addEventListener("change",function(e){
  SORT = e.target.value; localStorage.setItem("hq_sort",SORT);
  if(STATE) renderParty(STATE.sessions);
});

// filter chips
$("filterChips").addEventListener("click",function(e){
  var btn = e.target.closest(".chip"); if(!btn) return;
  FILTER = btn.getAttribute("data-filter");
  Array.prototype.forEach.call($("filterChips").children,function(c){
    c.classList.toggle("active", c===btn);
  });
  if(STATE) renderParty(STATE.sessions);
});

// alert banner interactions
$("alertBanner").addEventListener("click",function(e){
  if(e.target.id==="abX") return;
  // scroll to the needs group
  var needsHead = document.querySelector(".ghead .dot.needs");
  if(needsHead){ needsHead.closest(".grp").scrollIntoView({behavior:"smooth",block:"start"}); }
  else { $("party").scrollIntoView({behavior:"smooth",block:"start"}); }
});
$("alertBanner").addEventListener("keydown",function(e){
  if(e.key==="Enter"||e.key===" "){ e.preventDefault(); this.click(); }
});
$("abX").addEventListener("click",function(e){
  e.stopPropagation();
  ALERT_DISMISSED = $("alertBanner")._sig || null;
  $("alertBanner").classList.add("hidden");
});

// bell toggle: cycles notif+chime. If notif not granted, request; else toggle chime.
$("bellBtn").addEventListener("click",function(){
  var granted = ("Notification" in window) && Notification.permission==="granted";
  if(("Notification" in window) && Notification.permission==="default"){
    Notification.requestPermission().then(function(p){
      NOTIF_ON = (p==="granted"); localStorage.setItem("hq_notif", NOTIF_ON?"1":"0");
      CHIME_ON = true; localStorage.setItem("hq_chime","1");
      updateBellUI(); playChime();
    });
    return;
  }
  // toggle: on -> mute chime -> off entirely -> back on
  if((NOTIF_ON&&granted) && CHIME_ON){ CHIME_ON=false; }
  else if((NOTIF_ON&&granted) && !CHIME_ON){ NOTIF_ON=false; }
  else { NOTIF_ON = granted; CHIME_ON=true; if(!granted) NOTIF_ON=false; }
  localStorage.setItem("hq_notif", NOTIF_ON?"1":"0");
  localStorage.setItem("hq_chime", CHIME_ON?"1":"0");
  updateBellUI();
  if(CHIME_ON) playChime();
});
updateBellUI();

// Soundboard: clips are served by the Arena host (or a local ./sounds dir in
// dev) and listed at /api/arena/sounds -- nothing is baked into the page, so
// dropping a new file on the server makes it appear here. Playback rides the
// shared WebAudio context; decoded buffers are cached after the first fetch.
var SOUNDS = { list:null, buffers:{}, loaded:false };
function soundPopOpen(){ return $("soundPop").classList.contains("open"); }
function closeSound(){ $("soundPop").classList.remove("open"); $("soundBtn").setAttribute("aria-expanded","false"); }
function toggleSound(){
  var open = $("soundPop").classList.toggle("open");
  $("soundBtn").setAttribute("aria-expanded", open?"true":"false");
  if(open) loadSounds();
}
function loadSounds(force){
  if(SOUNDS.loaded && !force) return;
  SOUNDS.loaded = true;
  fetch("/api/arena/sounds",{cache:"no-store"})
    .then(function(r){ return r.ok ? r.json() : {sounds:[]}; })
    .then(function(j){ SOUNDS.list = (j && j.sounds) || []; renderSounds(); })
    .catch(function(){ SOUNDS.list = []; SOUNDS.loaded = false; renderSounds(); });
}
function renderSounds(){
  var grid = $("soundGrid"); grid.textContent = "";
  var list = SOUNDS.list || [];
  if(!list.length){
    var e = el("div","sound-empty");
    e.textContent = "No sounds yet \u2014 add one below.";
    grid.appendChild(e); return;
  }
  list.forEach(function(s){
    var b = el("button","hbtn sound-tile"); b.type = "button";
    b.setAttribute("role","menuitem"); b.dataset.file = s.file;
    b.innerHTML = '<svg class="i" aria-hidden="true"><use href="#i-play"/></svg>';
    var label = el("span"); label.textContent = s.name || s.file; b.appendChild(label);
    b.addEventListener("click", function(){ playSound(s.file, b); });
    grid.appendChild(b);
  });
}
function playSound(file, btn){
  if(!audioCtx){ var AC = window.AudioContext||window.webkitAudioContext; if(!AC) return; audioCtx = new AC(); }
  if(audioCtx.state === "suspended") audioCtx.resume();
  var play = function(buf){
    try{
      var src = audioCtx.createBufferSource(); src.buffer = buf;
      src.connect(audioCtx.destination); src.start(0);
      if(btn){ btn.classList.add("playing");
        src.onended = function(){ btn.classList.remove("playing"); };
        setTimeout(function(){ btn.classList.remove("playing"); }, (buf.duration*1000)+200);
      }
    }catch(e){}
  };
  if(SOUNDS.buffers[file]){ play(SOUNDS.buffers[file]); return; }
  fetch("/api/arena/sounds/"+encodeURIComponent(file),{cache:"no-store"})
    .then(function(r){ if(!r.ok) throw 0; return r.arrayBuffer(); })
    .then(function(ab){ return audioCtx.decodeAudioData(ab); })
    .then(function(buf){ SOUNDS.buffers[file] = buf; play(buf); })
    .catch(function(){ toast("\u26A0 Couldn\u2019t play that sound", "ach"); });
}
var SOUND_EXTS = ["ogg","mp3","wav","m4a","webm"];
function uploadSounds(files){
  var queue = Array.prototype.slice.call(files || []);
  var foot = $("soundFoot");
  var okCount = 0;
  (function next(){
    if(!queue.length){
      if(foot) foot.classList.remove("busy");
      if(okCount) loadSounds(true);           // refresh the grid with new clips
      return;
    }
    var f = queue.shift();
    var ext = (f.name.split(".").pop() || "").toLowerCase();
    if(SOUND_EXTS.indexOf(ext) < 0){ toast("“"+f.name+"” isn’t a supported audio file","ach"); return next(); }
    if(f.size > 5*1024*1024){ toast("“"+f.name+"” is larger than 5 MB","ach"); return next(); }
    if(foot) foot.classList.add("busy");
    var reader = new FileReader();
    reader.onload = function(){
      var b64 = String(reader.result || "").split(",")[1] || "";
      arenaPost("/api/arena/sounds", {name:f.name, data:b64}).then(function(res){
        if(res.ok){ okCount++; toast("🔊 Added “"+(f.name.replace(/\.[^.]+$/,""))+"”","level"); }
        else{ toast((res.j && res.j.error) || "Upload failed","ach"); }
        next();
      });
    };
    reader.onerror = function(){ toast("Couldn’t read “"+f.name+"”","ach"); next(); };
    reader.readAsDataURL(f);
  })();
}
function setupSoundUpload(){
  var btn = $("soundAddBtn"), input = $("soundFile"), pop = $("soundPop");
  if(!btn || !input || !pop || btn._wired) return;
  btn._wired = true;
  btn.addEventListener("click", function(e){ e.stopPropagation(); input.click(); });
  input.addEventListener("change", function(){ if(input.files && input.files.length) uploadSounds(input.files); input.value = ""; });
  var dragDepth = 0;
  pop.addEventListener("dragenter", function(e){ e.preventDefault(); e.stopPropagation(); dragDepth++; pop.classList.add("dragging"); });
  pop.addEventListener("dragover", function(e){ e.preventDefault(); e.stopPropagation(); if(e.dataTransfer) e.dataTransfer.dropEffect = "copy"; });
  pop.addEventListener("dragleave", function(e){ e.preventDefault(); e.stopPropagation(); if(--dragDepth <= 0){ dragDepth = 0; pop.classList.remove("dragging"); } });
  pop.addEventListener("drop", function(e){
    e.preventDefault(); e.stopPropagation(); dragDepth = 0; pop.classList.remove("dragging");
    if(e.dataTransfer && e.dataTransfer.files && e.dataTransfer.files.length) uploadSounds(e.dataTransfer.files);
  });
}
setupSoundUpload();
$("soundBtn").addEventListener("click",function(e){ e.stopPropagation(); toggleSound(); });
document.addEventListener("click",function(e){
  if(soundPopOpen() && !e.target.closest(".sound-wrap")) closeSound();
});

// drawer close
$("dwClose").addEventListener("click",closeDrawer);
$("backdrop").addEventListener("click",closeDrawer);

// keyboard shortcuts
document.addEventListener("keydown",function(e){
  // Cmd/Ctrl-K opens the command palette from anywhere
  if((e.metaKey||e.ctrlKey) && (e.key==="k"||e.key==="K")){ e.preventDefault(); toggleCmdk(); return; }
  if(e.key==="Escape"){
    if($("evoSplash").classList.contains("on")){ closeEvoSplash(); return; }
    if($("evoPickBack").classList.contains("open")){ closeEvoPicker(); return; }
    if($("evoPreviewBack").classList.contains("open")){ closeEvoPreview(); return; }
    if($("giftBack").classList.contains("open")){ giftClose(); return; }
    if($("warroom").classList.contains("on")){ closeWarroom(); return; }
    if($("transcriptBack").classList.contains("open")){ closeTranscript(); return; }
    if($("projectBack").classList.contains("open")){ closeProject(); return; }
    if($("settingsBack").classList.contains("open")){ closeSettings(); return; }
    if($("focus").classList.contains("on")){ closeFocus(); return; }
    if($("digestBack").classList.contains("open")){ closeDigest(); return; }
    if($("infoBack").classList.contains("open")){ closeInfo(); return; }
    if($("helpBack").classList.contains("open")){ closeHelp(); return; }
    if($("logBack").classList.contains("open")){ closeLog(); return; }
    if(CMDK_OPEN){ closeCmdk(); return; }
    if(SEARCHOV_OPEN){ closeSearchOv(); return; }
    if(DRAWER_OPEN){ closeDrawer(); return; }
    if($("menuPop").classList.contains("open")){ closeMenu(); return; }
  }
  if(CMDK_OPEN){ cmdkKeydown(e); return; }
  var tag = (e.target.tagName||"").toLowerCase();
  var typing = tag==="input"||tag==="textarea"||tag==="select"||e.target.isContentEditable;
  if(typing) return;
  // A game that already handled this key (golf power 1-9/0, its R/? keys, battle moves 1-4)
  // keeps it: don't also switch views or refresh.
  if(e.defaultPrevented) return;
  // Single-key shortcuts only: leave Cmd/Ctrl/Alt combos (Cmd-R, Cmd-1..9, ...) to the browser.
  if(e.metaKey||e.ctrlKey||e.altKey) return;
  if(e.key==="/"){ e.preventDefault(); $("search").focus(); }
  else if(e.key==="r"||e.key==="R"){ e.preventDefault(); forceRefresh(); }
  else if(e.key==="?"){ e.preventDefault(); openHelp(); }
  else if(e.key>="1"&&e.key<="9"){ e.preventDefault(); setView(["live","analytics","pokedex","gym","quests","arena","village","store","cali"][+e.key-1]); }
  else if(e.key==="0"){ e.preventDefault(); setView("valley"); }
  else if(e.key==="h"||e.key==="H"){ e.preventDefault(); hqToggle(); }
  else if(e.key==="j"||e.key==="J"){ e.preventDefault(); setView("music"); }
  else if(e.key==="w"||e.key==="W"){ if(!$("workshopView")) return; e.preventDefault(); setView("workshop"); }
  else if(e.key==="c"||e.key==="C"){ e.preventDefault(); setView("compete"); }
  // In a voice call, from any view: M mutes, V switches the camera, S shares your screen. Holding a key down
  // doesn't repeat it, and outside a call these keys do nothing.
  else if(VCHAN.on && !e.repeat && (e.key==="m"||e.key==="M")){ e.preventDefault(); voiceToggleMute(); }
  else if(VCHAN.on && !e.repeat && (e.key==="v"||e.key==="V")){ e.preventDefault(); voiceToggleCam(); }
  else if(VCHAN.on && !e.repeat && (e.key==="s"||e.key==="S")){ e.preventDefault(); voiceToggleScreen(); }
});

/* ---------- focus management: trap Tab inside open modals/drawer + return focus ---------- */
var MODAL_RETURN=null;
function rememberOpener(){ if(!MODAL_RETURN) MODAL_RETURN=document.activeElement; }
function restoreOpener(){ var t=MODAL_RETURN; MODAL_RETURN=null;
  if(t && t.focus && document.contains(t)){ try{ t.focus(); }catch(e){} } }
// The currently-topmost open overlay, or null. Order = visual stacking priority.
function currentModal(){
  if($("warroom") && $("warroom").classList.contains("on")) return $("warroom");
  if($("evoPickBack") && $("evoPickBack").classList.contains("open")) return $("evoPickBack");
  if($("evoPreviewBack") && $("evoPreviewBack").classList.contains("open")) return $("evoPreviewBack");
  if(typeof CMDK_OPEN!=="undefined" && CMDK_OPEN) return $("cmdk");
  if($("transcriptBack") && $("transcriptBack").classList.contains("open")) return $("transcriptBack");
  if($("projectBack") && $("projectBack").classList.contains("open")) return $("projectBack");
  if($("giftBack") && $("giftBack").classList.contains("open")) return $("giftBack");
  if($("settingsBack") && $("settingsBack").classList.contains("open")) return $("settingsBack");
  if($("infoBack") && $("infoBack").classList.contains("open")) return $("infoBack");
  if($("digestBack") && $("digestBack").classList.contains("open")) return $("digestBack");
  if($("focus") && $("focus").classList.contains("on")) return $("focus");
  if(typeof SEARCHOV_OPEN!=="undefined" && SEARCHOV_OPEN) return $("searchOv");
  if(typeof DRAWER_OPEN!=="undefined" && DRAWER_OPEN) return $("drawer");
  return null;
}
function focusablesIn(root){
  var sel='a[href],button:not([disabled]),input:not([disabled]),select:not([disabled]),'+
          'textarea:not([disabled]),[tabindex]:not([tabindex="-1"])';
  return Array.prototype.slice.call(root.querySelectorAll(sel)).filter(function(node){
    return node.offsetWidth>0 || node.offsetHeight>0 || node===document.activeElement;
  });
}
// Capture-phase Tab trap: keeps keyboard focus within the active overlay.
document.addEventListener("keydown",function(e){
  if(e.key!=="Tab") return;
  var m=currentModal(); if(!m) return;
  var f=focusablesIn(m); if(!f.length){ e.preventDefault(); m.focus&&m.focus(); return; }
  var first=f[0], last=f[f.length-1], a=document.activeElement;
  if(f.indexOf(a)<0){ e.preventDefault(); (e.shiftKey?last:first).focus(); return; }
  if(e.shiftKey && a===first){ e.preventDefault(); last.focus(); }
  else if(!e.shiftKey && a===last){ e.preventDefault(); first.focus(); }
},true);

// keep relative times fresh
setInterval(function(){ if(STATE){ $("updated").textContent="updated "+relTime(STATE.updated); } },15000);

