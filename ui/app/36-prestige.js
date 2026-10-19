/* ================= HQ 2.5: prestige ================= */
// At shown level 50 you can reset the level you SHOW for a prestige star. Only the
// shown level resets: your XP, level unlocks, boards and HQ floors all stay (the Arena
// stores an offset, backend/src/prestige.rs). Star 1 grants the Prestige star frame,
// star 3 the Rooftop crown (both grant-only, soulbound cosmetics). Your own 3D HQ grows a
// prestige floor per star (up to 5) and a beacon (games/hqbase.js, opts.prestige).
// Panels: the Compete view (order 40), prestigeOpen() (a modal), and a ★ strip on every
// trainer card (TCARD_EXTRAS). The only thing a claim sends is a random requestId.
var PRESTIGE = {data:null, at:0, busy:false, loading:null, err:null};
var PRESTIGE_AT = 50;

function prestigeGet(path){
  return fetch(path,{cache:"no-store"}).then(function(r){
    return r.json().then(function(j){ return {ok:r.ok, code:r.status, j:j||{}}; }, function(){ return {ok:false, code:r.status, j:{}}; });
  });
}
function prestigePaired(){ return !!(window.ARENA && ARENA.paired); }
// Fetch /me (cached for a minute). A change in stars rebuilds your 3D HQ.
function prestigeLoad(force){
  if(PRESTIGE.loading) return PRESTIGE.loading;
  if(!force && PRESTIGE.data && Date.now()-PRESTIGE.at < 60000) return Promise.resolve(PRESTIGE.data);
  var before=prestigeStars();
  PRESTIGE.loading=prestigeGet("/api/arena/prestige?u=me").then(function(res){
    if(res.ok && res.j && typeof res.j.stars==="number"){ PRESTIGE.data=res.j; PRESTIGE.err=null; }
    else PRESTIGE.err = res.code===404 ? "old" : (res.j.error||res.j.detail||"The Arena didn't answer.");
  }).catch(function(){ PRESTIGE.err="The Arena didn't answer."; }).then(function(){
    PRESTIGE.at=Date.now(); PRESTIGE.loading=null;
    if(prestigeStars()!==before) prestigeLookChanged();
    return PRESTIGE.data;
  });
  return PRESTIGE.loading;
}
// Your stars (0 when unpaired, unknown or on an Arena without prestige). Never blocks:
// a stale cache kicks off a background refresh.
function prestigeStars(){
  var d=PRESTIGE.data;
  if(prestigePaired() && !PRESTIGE.loading && Date.now()-PRESTIGE.at > 60000) setTimeout(function(){ prestigeLoad(); }, 0);
  return (d && typeof d.stars==="number" && d.stars>0) ? Math.min(99, d.stars|0) : 0;
}
window.prestigeStars=prestigeStars;
function prestigeLookChanged(){ try { if(window.HQ3D && HQ3D.inst && HQ3D.inst.lookChanged) HQ3D.inst.lookChanged(); } catch(e){} }
function prestigeStarText(n){ n=n|0; return n>0 ? "★"+n : ""; }

/* ---- the panel (shared by the modal and the Compete view) ---- */
function prestigeMount(el){
  if(!el) return;
  el.textContent="";
  var box=document.createElement("div"); box.className="pr-panel"; el.appendChild(box);
  if(!prestigePaired()){ var p0=document.createElement("p"); p0.className="muted"; p0.textContent="Pair with the Arena (Arena tab) to prestige."; box.appendChild(p0); return; }
  var wait=document.createElement("p"); wait.className="muted"; wait.textContent="Loading…"; box.appendChild(wait);
  prestigeLoad(true).then(function(){ if(box.isConnected) prestigeRender(box); });
}
function prestigeUnmount(el){ if(el) el.textContent=""; }
function prestigeRender(box){
  box.textContent="";
  var d=PRESTIGE.data;
  if(!d){
    var e=document.createElement("p"); e.className="muted";
    e.textContent = PRESTIGE.err==="old" ? "This Arena doesn't have prestige yet." : (PRESTIGE.err||"The Arena didn't answer.");
    box.appendChild(e); return;
  }
  var head=document.createElement("div"); head.className="pr-head";
  var star=document.createElement("span"); star.className="pr-stars"; star.setAttribute("role","img");
  star.setAttribute("aria-label", (d.stars|0)+" prestige star"+((d.stars|0)===1?"":"s"));
  star.textContent = (d.stars|0) ? "★ "+(d.stars|0) : "☆ 0";
  var lv=document.createElement("div"); lv.className="pr-lv";
  var big=document.createElement("b"); big.textContent="Level "+(d.shownLevel|0);
  var sm=document.createElement("small"); sm.className="muted"; sm.textContent="True level "+(d.trueLevel|0)+" · "+(d.xp|0).toLocaleString()+" XP";
  lv.appendChild(big); lv.appendChild(sm); head.appendChild(star); head.appendChild(lv); box.appendChild(head);
  var bar=document.createElement("div"); bar.className="pr-bar"; bar.setAttribute("role","progressbar");
  var pct=Math.max(0, Math.min(100, Math.round(((d.shownLevel|0) - 1 + ((d.into|0)/Math.max(1, d.need|0))) / ((d.at||PRESTIGE_AT) - 1) * 100)));
  bar.setAttribute("aria-valuemin","0"); bar.setAttribute("aria-valuemax","100"); bar.setAttribute("aria-valuenow", String(pct));
  bar.setAttribute("aria-label","Progress to level "+(d.at||PRESTIGE_AT));
  var fill=document.createElement("i"); fill.style.width=pct+"%"; bar.appendChild(fill); box.appendChild(bar);
  var how=document.createElement("p"); how.className="pr-how";
  how.textContent = d.canPrestige
    ? "You've reached level "+(d.at||PRESTIGE_AT)+". Prestige to earn a star: your shown level starts again at 1, everything you've earned stays."
    : "Reach level "+(d.at||PRESTIGE_AT)+" to prestige ("+Math.max(0,(d.at||PRESTIGE_AT)-(d.shownLevel|0))+" to go). Each star adds a floor to your HQ.";
  box.appendChild(how);
  var rw=document.createElement("ul"); rw.className="pr-rewards";
  [[1,"Prestige star frame"],[3,"Rooftop crown for your HQ"],[5,"A five-floor prestige skyline"]].forEach(function(r){
    var li=document.createElement("li"); var got=(d.stars|0)>=r[0]; li.className=got?"got":"";
    li.textContent=(got?"✓ ":"")+"★"+r[0]+": "+r[1]; rw.appendChild(li);
  });
  box.appendChild(rw);
  var b=document.createElement("button"); b.type="button"; b.className="hbtn pr-claim"; b.textContent="Prestige…";
  b.disabled=!d.canPrestige || PRESTIGE.busy;
  b.addEventListener("click", function(){ prestigeConfirm(box); });
  box.appendChild(b);
}
// The confirm step lives inline, so focus stays in the same dialog or panel.
function prestigeConfirm(box){
  var old=box.querySelector(".pr-confirm"); if(old){ old.querySelector("button").focus(); return; }
  var c=document.createElement("div"); c.className="pr-confirm"; c.setAttribute("role","alertdialog"); c.setAttribute("aria-label","Confirm prestige");
  var p=document.createElement("p"); p.textContent="Prestige now? Your shown level resets to 1; unlocks, boards and XP stay.";
  var row=document.createElement("div"); row.className="pr-btns";
  var yes=document.createElement("button"); yes.type="button"; yes.className="hbtn"; yes.textContent="Prestige";
  var no=document.createElement("button"); no.type="button"; no.className="hbtn ghost"; no.textContent="Not now";
  row.appendChild(no); row.appendChild(yes); c.appendChild(p); c.appendChild(row); box.appendChild(c);
  no.addEventListener("click", function(){ c.parentNode && c.parentNode.removeChild(c); var cl=box.querySelector(".pr-claim"); if(cl) cl.focus(); });
  yes.addEventListener("click", function(){ prestigeClaim(box); });
  yes.focus();
}
function prestigeClaim(box){
  if(PRESTIGE.busy) return;
  PRESTIGE.busy=true;
  var rid="prestige-"+(typeof newRequestId==="function" ? String(newRequestId()).replace(/[^A-Za-z0-9_-]/g,"").slice(0,40) : String(Date.now()));
  if(rid.length<16) rid=(rid+"-0000000000000000").slice(0,24);
  fetch("/api/arena/prestige/claim",{method:"POST", headers:{"Content-Type":"application/json","X-HQ-Token":CSRF}, body:JSON.stringify({requestId:rid})})
    .then(function(r){ return r.json().then(function(j){ return {ok:r.ok, j:j||{}}; }, function(){ return {ok:r.ok, j:{}}; }); })
    .then(function(res){
      PRESTIGE.busy=false;
      if(res.ok && typeof res.j.stars==="number"){
        PRESTIGE.data=res.j; PRESTIGE.at=Date.now();
        var n=res.j.stars|0, msg="Prestige ★"+n+"! Your level starts again at 1.";
        if(n===1) msg+=" Prestige star frame unlocked."; if(n===3) msg+=" Rooftop crown unlocked.";
        if(typeof toast==="function") toast("★ "+msg,"level");
        if(typeof announce==="function") announce(msg);
        prestigeLookChanged();
        if(typeof invLoad==="function"){ try{ invLoad(); }catch(e){} }
      } else {
        if(typeof toast==="function") toast("⚠ "+(res.j.error||res.j.detail||"Prestige failed"),"ach");
        if(typeof announce==="function") announce("Prestige failed");
      }
      if(box && box.isConnected){ prestigeRender(box);
        // the confirm step is gone: keep focus inside the dialog or panel
        var x=$("prestigeClose"), cl=box.querySelector(".pr-claim");
        if(x && box.closest && box.closest("#prestigeBack")) x.focus(); else if(cl && !cl.disabled) cl.focus(); }
    }).catch(function(){ PRESTIGE.busy=false; if(typeof toast==="function") toast("⚠ The Arena didn't answer","ach"); if(box && box.isConnected) prestigeRender(box); });
}

/* ---- the modal ---- */
var PRESTIGE_OPENER=null;
function prestigeModal(){
  var back=$("prestigeBack"); if(back) return back;
  back=document.createElement("div"); back.className="tcard-back"; back.id="prestigeBack"; back.setAttribute("aria-hidden","true");
  var m=document.createElement("div"); m.className="tcard-modal pr-modal"; m.setAttribute("role","dialog"); m.setAttribute("aria-modal","true"); m.setAttribute("aria-labelledby","prestigeTitle");
  var top=document.createElement("div"); top.className="tcard-top";
  var t=document.createElement("b"); t.id="prestigeTitle"; t.textContent="Prestige";
  var x=document.createElement("button"); x.type="button"; x.className="hbtn icon ghost"; x.id="prestigeClose"; x.setAttribute("aria-label","Close"); x.textContent="✕";
  top.appendChild(t); top.appendChild(x); m.appendChild(top);
  var body=document.createElement("div"); body.id="prestigeBody"; m.appendChild(body);
  back.appendChild(m); document.body.appendChild(back);
  x.addEventListener("click", prestigeClose);
  back.addEventListener("click", function(e){ if(e.target===back) prestigeClose(); });
  back.addEventListener("keydown", function(e){ if(e.key==="Escape"){ e.preventDefault(); e.stopPropagation(); prestigeClose(); } });
  return back;
}
function prestigeOpen(){
  var back=prestigeModal();
  PRESTIGE_OPENER=document.activeElement;
  back.classList.add("open"); back.setAttribute("aria-hidden","false");
  prestigeMount($("prestigeBody"));
  var x=$("prestigeClose"); if(x) x.focus();
}
function prestigeClose(){
  var back=$("prestigeBack"); if(!back) return;
  back.classList.remove("open"); back.setAttribute("aria-hidden","true");
  prestigeUnmount($("prestigeBody"));
  if(PRESTIGE_OPENER && PRESTIGE_OPENER.focus && PRESTIGE_OPENER.isConnected){ try{ PRESTIGE_OPENER.focus(); }catch(e){} }
  PRESTIGE_OPENER=null;
}
window.prestigeOpen=prestigeOpen;

/* ---- Compete view panel (order 40) ---- */
(window.COMPETE_PANELS=window.COMPETE_PANELS||[]).push({id:"prestige", name:"Prestige", icon:"★", order:40,
  mount:function(el){ this._el=el; prestigeMount(el); },
  unmount:function(){ prestigeUnmount(this._el); this._el=null; }});

/* ---- trainer card strip: ★N and the shown level (anyone's) ---- */
(window.TCARD_EXTRAS=window.TCARD_EXTRAS||[]).push(function(box, prof){
  if(!box || !prof) return;
  var uid = prof.isYou ? "me" : prof.userId;
  if(!uid || (uid!=="me" && !/^[0-9a-f-]{36}$/i.test(uid))) return;
  var sec=document.createElement("div"); sec.className="pr-tcard"; box.appendChild(sec);
  prestigeGet("/api/arena/prestige?u="+encodeURIComponent(uid)).then(function(res){
    if(!res.ok || !res.j || typeof res.j.stars!=="number"){ if(sec.parentNode) sec.parentNode.removeChild(sec); return; }
    var n=res.j.stars|0;
    if(!n && !prof.isYou){ if(sec.parentNode) sec.parentNode.removeChild(sec); return; }
    var h=document.createElement("div"); h.className="tcard-sec"; h.textContent="Prestige"; sec.appendChild(h);
    var chip=document.createElement("span"); chip.className="tcard-trophy pr-chip";
    chip.textContent=(n ? "★"+n : "☆ No stars yet")+" · Level "+(res.j.shownLevel|0);
    sec.appendChild(chip);
    if(prof.isYou){
      if(typeof res.j.canPrestige==="boolean"){ PRESTIGE.data=res.j; PRESTIGE.at=Date.now(); }
      var b=document.createElement("button"); b.type="button"; b.className="hbtn ghost pr-open"; b.textContent=res.j.canPrestige ? "Prestige now…" : "Prestige";
      b.addEventListener("click", function(){ if(typeof tcardClose==="function") tcardClose(); prestigeOpen(); });
      sec.appendChild(b);
    }
  }).catch(function(){ if(sec.parentNode) sec.parentNode.removeChild(sec); });
});
