/* ================= HQ 2.1: crews ================= */
// Make a crew (name, 2-4 letter tag, banner colour) or join one with its private invite code. The
// crew climbs the crew board on its members' combined XP, and its banner flies on your HQ's roof.
var CREW = {mine:null, loaded:false};
var CREW_COLORS = ["#ffb347","#5fd3e6","#6fd38a","#ff6b5b","#9b8cf0","#f4f4f4"];
function crewGet(p){ return fetch(p,{cache:"no-store"}).then(function(r){ return r.json().then(function(j){ return {ok:r.ok, code:r.status, j:j}; }, function(){ return {ok:false, code:r.status, j:{}}; }); }); }
function crewPost(p, b){ return fetch(p,{method:"POST",headers:{"Content-Type":"application/json","X-HQ-Token":CSRF},body:JSON.stringify(b||{})})
  .then(function(r){ return r.json().then(function(j){ return {ok:r.ok, code:r.status, j:j}; }, function(){ return {ok:false, code:r.status, j:{}}; }); }); }
function crewSetMine(c){
  CREW.mine=c||null; CREW.loaded=true;
  try { localStorage.setItem("hq_crew", JSON.stringify(c ? {tag:c.tag, color:c.color, name:c.name} : null)); } catch(e){}
  if(typeof HQ3D!=="undefined" && HQ3D && HQ3D.inst && !HQ3D.visit && HQ3D.inst.lookChanged) HQ3D.inst.lookChanged();
}
function crewBanner(){ try { return JSON.parse(localStorage.getItem("hq_crew")||"null"); } catch(e){ return null; } }
function crewLoad(){
  var box=$("crewBody"); if(!box) return;
  box.innerHTML='<p class="muted">Loading…</p>';
  Promise.all([crewGet("/api/arena/crews/mine"), crewGet("/api/arena/crews")]).then(function(res){
    box.textContent="";
    if(!res[0].ok){ var e=document.createElement("p"); e.className="muted";
      e.textContent = res[0].code===404 ? "This Arena doesn't have crews yet: ask its owner to update it." : "Pair with an Arena to make or join a crew."; box.appendChild(e); return; }
    crewSetMine(res[0].j.crew);
    if(CREW.mine) crewRenderMine(box, CREW.mine); else crewRenderJoin(box);
    crewRenderBoard(box, (res[1].ok && res[1].j.crews) || []);
  });
}
function crewMsg(box, t){ var m=box.querySelector(".crew-msg"); if(!m){ m=document.createElement("p"); m.className="crew-msg muted"; m.setAttribute("aria-live","polite"); box.insertBefore(m, box.firstChild); } m.textContent=t; }
function crewRenderMine(box, c){
  var card=document.createElement("div"); card.className="crew-card"; card.style.borderLeftColor=c.color;
  var h=document.createElement("b"); h.textContent="["+c.tag+"] "+c.name; card.appendChild(h);
  var sub=document.createElement("div"); sub.className="muted"; sub.textContent="Crew level "+c.level+" · "+c.members.length+" member"+(c.members.length===1?"":"s")+" · "+c.xp.toLocaleString()+" XP"; card.appendChild(sub);
  var mem=document.createElement("div"); mem.className="chips";
  c.members.forEach(function(m){ var b=document.createElement("button"); b.type="button"; b.className="tcard-trophy"; b.textContent=(m.owner?"★ ":"")+(m.displayName||m.handle)+" · Lv "+m.level;
    b.addEventListener("click", function(){ if(typeof tcardOpen==="function") tcardOpen(m.userId); }); mem.appendChild(b); });
  card.appendChild(mem);
  var row=document.createElement("div"); row.className="crew-row";
  var code=document.createElement("code"); code.textContent=c.code; code.title="Invite code";
  var cp=document.createElement("button"); cp.type="button"; cp.className="hbtn ghost"; cp.textContent="Copy invite code";
  cp.addEventListener("click", function(){ (navigator.clipboard ? navigator.clipboard.writeText(c.code) : Promise.reject()).then(function(){ crewMsg(box, "Copied. Send it to a friend: they join from this panel."); }, function(){ crewMsg(box, "Invite code: "+c.code); }); });
  var lv=document.createElement("button"); lv.type="button"; lv.className="hbtn ghost"; lv.textContent="Leave crew";
  lv.addEventListener("click", function(){ if(lv.dataset.sure!=="1"){ lv.dataset.sure="1"; lv.textContent="Click again to leave"; return; }
    crewPost("/api/arena/crews/leave").then(function(){ crewSetMine(null); crewLoad(); }); });
  row.appendChild(document.createTextNode("Invite code ")); row.appendChild(code); row.appendChild(cp); row.appendChild(lv); card.appendChild(row);
  box.appendChild(card);
}
function crewRenderJoin(box){
  var wrap=document.createElement("div"); wrap.className="crew-forms";
  var j=document.createElement("form"); j.className="crew-form";
  j.innerHTML='<b>Join a crew</b><label for="crewCode" class="muted">Invite code</label><input id="crewCode" maxlength="8" autocomplete="off" placeholder="8 letters"><button class="hbtn" type="submit">Join</button>';
  j.addEventListener("submit", function(e){ e.preventDefault(); crewPost("/api/arena/crews/join",{code:$("crewCode").value}).then(function(r){
    if(r.ok){ crewSetMine(r.j.crew); crewLoad(); } else crewMsg(box, "⚠ "+(r.j.detail||r.j.error||"That didn't work.")); }); });
  var c=document.createElement("form"); c.className="crew-form";
  c.innerHTML='<b>Start a crew</b><label for="crewName" class="muted">Name</label><input id="crewName" maxlength="32" autocomplete="off" placeholder="Night Owls">'+
    '<label for="crewTag" class="muted">Tag (2-4)</label><input id="crewTag" maxlength="4" autocomplete="off" placeholder="OWL"><div class="hq3d-swatches" id="crewColors" role="group" aria-label="Banner colour"></div><button class="hbtn" type="submit">Create</button>';
  var pick=CREW_COLORS[0];
  c.addEventListener("submit", function(e){ e.preventDefault(); crewPost("/api/arena/crews/create",{name:$("crewName").value, tag:$("crewTag").value, color:pick}).then(function(r){
    if(r.ok){ crewSetMine(r.j.crew); crewLoad(); } else crewMsg(box, "⚠ "+(r.j.detail||r.j.error||"That didn't work.")); }); });
  wrap.appendChild(j); wrap.appendChild(c); box.appendChild(wrap);
  var sw=c.querySelector("#crewColors");
  function paint(){ sw.textContent=""; CREW_COLORS.forEach(function(col){ var b=document.createElement("button"); b.type="button"; b.className="hq3d-sw"; b.style.background=col;
    b.setAttribute("aria-label","Banner "+col); b.setAttribute("aria-pressed", col===pick ? "true" : "false"); b.addEventListener("click", function(){ pick=col; paint(); }); sw.appendChild(b); }); }
  paint();
}
function crewRenderBoard(box, list){
  var h=document.createElement("h4"); h.className="gb-key"; h.textContent="Crew board"; box.appendChild(h);
  if(!list.length){ var p=document.createElement("p"); p.className="muted"; p.textContent="No crews yet: start the first one."; box.appendChild(p); return; }
  var t=document.createElement("table"); t.className="gb-table";
  var head=document.createElement("tr"); ["#","Crew","Level","Members","XP"].forEach(function(c){ var th=document.createElement("th"); th.textContent=c; head.appendChild(th); }); t.appendChild(head);
  list.forEach(function(c){ var tr=document.createElement("tr"); if(c.isMine) tr.className="you";
    [c.rank, "["+c.tag+"] "+c.name, c.level, c.members.length, c.xp.toLocaleString()].forEach(function(v, i){ var td=document.createElement("td"); td.textContent=String(v); if(i===1) td.style.borderLeft="3px solid "+c.color; tr.appendChild(td); });
    t.appendChild(tr); });
  var w=document.createElement("div"); w.className="gb-scroll"; w.appendChild(t); box.appendChild(w);
}
