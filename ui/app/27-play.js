/* ================= HQ 2.1: Quick Play, Party Mode, and playing from a friend's HQ ================= */
// Quick Play: pick a game and the Arena matches you into a fresh room with others queued for it
// (your crewmates first). Party Mode: the room plays Kart, Platformer, Blaster and Golf in a row and
// the Arena adds up one score. Only the game name and your place leave this page, as always.
var QP = {state:"idle", game:null, timer:null, room:null};
var PARTY = {view:null};
var PLAY_GAMES = [["kart","🏎️","Kart Racing"],["rally","🏁","Valley Rally"],["plat","🏃","Platformer Rush"],["fps","🔫","Blaster Arena"],["golf","⛳","Mini Golf"],["type","⌨️","Code Typing Race"]];
function playName(g){ for(var i=0;i<PLAY_GAMES.length;i++) if(PLAY_GAMES[i][0]===g) return PLAY_GAMES[i][2]; return g; }
function playPost(p, b){ return fetch(p,{method:"POST",headers:{"Content-Type":"application/json","X-HQ-Token":CSRF},body:JSON.stringify(b||{})})
  .then(function(r){ return r.json().then(function(j){ return {ok:r.ok, code:r.status, j:j}; }, function(){ return {ok:false, code:r.status, j:{}}; }); }); }
function playGet(p){ return fetch(p,{cache:"no-store"}).then(function(r){ return r.json().then(function(j){ return {ok:r.ok, code:r.status, j:j}; }, function(){ return {ok:false, code:r.status, j:{}}; }); }); }

// Open a multiplayer game in the Valley, in whatever Arena room you are in now; then() runs once its
// shell has joined the lobby on the live socket (so an invite or a party move is not dropped).
function playOpen(g, then){
  setView("valley");
  valleyLoad().then(function(){
    if(typeof HQV==="undefined" || !HQV.api) return;
    HQV.api.open("mp-"+g);
    if(!then) return;
    var tries=0, t=setInterval(function(){
      tries++;
      var c=HQV.mp && HQV.mp.ctx && HQV.mp.ctx(g), live=c && ARENA.sock && c.sock===ARENA.sock && ARENA.sock.readyState===1;
      if(live || tries>40){ clearInterval(t); if(live) then(); }
    }, 500);
  });
}

/* ---- Quick Play ---- */
function qpRender(){
  var box=$("qpBody"); if(!box) return; box.textContent="";
  if(QP.state==="waiting"){
    var p=document.createElement("p"); p.setAttribute("aria-live","polite");
    p.textContent="Looking for players for "+playName(QP.game)+"… "+(QP.waiting>1 ? QP.waiting+" in the queue" : "you're first in the queue")+(QP.waited ? " · "+QP.waited+"s" : "");
    var c=document.createElement("button"); c.type="button"; c.className="hbtn ghost"; c.textContent="Cancel";
    c.addEventListener("click", qpLeave);
    box.appendChild(p); box.appendChild(c); return;
  }
  var row=document.createElement("div"); row.className="qp-games";
  PLAY_GAMES.forEach(function(g){
    var b=document.createElement("button"); b.type="button"; b.className="hbtn"; b.textContent=g[1]+" "+g[2];
    b.addEventListener("click", function(){ qpJoin(g[0]); });
    row.appendChild(b);
  });
  box.appendChild(row);
  var n=document.createElement("p"); n.className="muted";
  n.textContent = QP.err ? "⚠ "+QP.err : "You go into a fresh room with whoever else is queued for that game: crewmates first, up to 8. It starts once a second person turns up.";
  box.appendChild(n);
}
function qpJoin(g){
  QP.err=null;
  playPost("/api/arena/quickplay/join",{game:g}).then(function(r){
    if(!r.ok || r.j.state==="error"){ QP.err = r.code===404 ? "This Arena doesn't have Quick Play yet: ask its owner to update it." : (r.j.error||r.j.detail||"The Arena didn't answer."); qpRender(); return; }
    qpApply(r.j);
  });
}
function qpLeave(){ clearTimeout(QP.timer); QP.state="idle"; qpRender(); playPost("/api/arena/quickplay/leave"); }
function qpApply(s){
  clearTimeout(QP.timer);
  QP.state=s.state; QP.game=s.game||QP.game; QP.waiting=s.waiting|0; QP.waited=s.waitedSecs|0;
  if(s.state==="matched") QP.state="idle";
  if(s.state==="matched" && typeof s.room==="string" && arenaIsQp(s.room)){
    QP.room=s.room;
    toast("🎮 Matched! "+s.players+" players for "+playName(s.game), "level");
    if(window.HQV && HQV.engine) HQV.engine.sfx("go");
    arenaGoRoom(s.room, {name:"Quick Play: "+playName(s.game)});
    playOpen(s.game);
  } else if(s.state==="waiting"){
    QP.timer=setTimeout(function(){ playGet("/api/arena/quickplay/status").then(function(r){ if(QP.state==="waiting") qpApply(r.ok ? r.j : {state:"waiting", game:QP.game}); }); }, 2000);
  }
  qpRender();
}

/* ---- Party Mode ---- */
function partySend(op){
  if(!ARENA.sock || ARENA.sock.readyState!==1){ toast("Connect to the Arena first (Arena tab)","ach"); return; }
  try { ARENA.sock.send(JSON.stringify({type:"game", g:"party", op:op})); } catch(e){}
}
function partyOnGame(m){
  if(m.ev==="error"){ toast("⚠ "+String(m.error||"").slice(0,120),"ach"); return; }
  if(m.ev!=="state" || !m.party || typeof m.party!=="object") return;
  var was=PARTY.view; PARTY.view=m.party; partyRender();
  var v=m.party;
  if(m.started && v.by) toast("🎉 "+String(v.by.displayName||v.by.handle||"Someone").slice(0,40)+" started a party: "+v.order.map(playName).join(" → "), "level");
  else if(m.stopped) toast("The party ended early","ach");
  else if(m.scored && v.done){ var w=v.standings[0]; toast("🏆 Party over! "+(w ? String(w.user.displayName||w.user.handle||"?").slice(0,40)+" wins with "+w.points+" points" : ""), "level");
    if(typeof confettiBurst==="function" && !hqCalm()) confettiBurst(); }
  else if(m.scored && v.next) toast("Party: "+playName(m.scored)+" scored. Next up: "+playName(v.next), "level");
  if(v.on && !v.done && (m.started || m.scored) && was!==undefined) partyChip();
}
function partyChip(){
  var v=PARTY.view, old=$("partyChip"); if(old) old.remove();
  if(!v || !v.on || v.done || !v.next) return;
  var c=document.createElement("div"); c.id="partyChip"; c.setAttribute("role","status");
  var t=document.createElement("b"); t.textContent="🎉 Party game "+(v.idx+1)+" of "+v.order.length+": "+playName(v.next); c.appendChild(t);
  var go=document.createElement("button"); go.type="button"; go.className="hbtn primary"; go.textContent="Play";
  go.addEventListener("click", function(){ c.remove(); playOpen(v.next); });
  var x=document.createElement("button"); x.type="button"; x.className="hbtn ghost"; x.textContent="Later"; x.addEventListener("click", function(){ c.remove(); });
  c.appendChild(go); c.appendChild(x); document.body.appendChild(c);
}
function partyRender(){
  var box=$("partyBody"); if(!box) return; box.textContent="";
  var v=PARTY.view;
  if(!ARENA.arena || !ARENA.arena.party){
    var u=document.createElement("p"); u.className="muted";
    u.textContent = ARENA.sock ? "This Arena doesn't run parties yet: ask its owner to update it." : "Connect to the Arena to throw a party with the people in your room.";
    box.appendChild(u); return;
  }
  if(!v || !v.on){
    var s=document.createElement("button"); s.type="button"; s.className="hbtn primary"; s.textContent="🎉 Start a party in "+(ARENA.roomName||"this room");
    s.addEventListener("click", function(){ partySend("start"); });
    var n=document.createElement("p"); n.className="muted"; n.textContent="Kart Racing → Platformer Rush → Blaster Arena → Mini Golf. Places score 10-8-6-5-4-3-2-1; the Arena adds them up.";
    box.appendChild(s); box.appendChild(n); return;
  }
  var steps=document.createElement("ol"); steps.className="party-steps";
  v.order.forEach(function(g, i){ var li=document.createElement("li"); li.className = i<v.idx ? "done" : i===v.idx ? "now" : ""; li.textContent=playName(g); steps.appendChild(li); });
  box.appendChild(steps);
  if(v.standings.length){
    var t=document.createElement("table"); t.className="gb-table"; var hd=document.createElement("tr");
    ["#","Player","Points"].forEach(function(c){ var th=document.createElement("th"); th.textContent=c; hd.appendChild(th); }); t.appendChild(hd);
    v.standings.forEach(function(s, i){ var tr=document.createElement("tr"); if(ARENA.you && s.user.userId===ARENA.you.userId) tr.className="you";
      [i+1, s.user.displayName||s.user.handle||"?", s.points].forEach(function(x){ var td=document.createElement("td"); td.textContent=String(x); tr.appendChild(td); }); t.appendChild(tr); });
    box.appendChild(t);
  }
  var row=document.createElement("div"); row.className="crew-row";
  if(!v.done){ var go=document.createElement("button"); go.type="button"; go.className="hbtn primary"; go.textContent="Play "+playName(v.next); go.addEventListener("click", function(){ playOpen(v.next); }); row.appendChild(go); }
  var mine = ARENA.you && v.by && v.by.userId===ARENA.you.userId;
  if(v.done || mine){ var st=document.createElement("button"); st.type="button"; st.className="hbtn ghost"; st.textContent = v.done ? "New party" : "End party";
    st.addEventListener("click", function(){ partySend(v.done ? "start" : "stop"); }); row.appendChild(st); }
  box.appendChild(row);
}
// a new room (or a reconnect): ask the Arena how the party in it stands
function partySync(){ PARTY.view=null; partyRender(); if(ARENA.arena && ARENA.arena.party) partySend("view"); }

/* ---- from a friend's HQ: their trainer card, and a game with them ---- */
function hqVisitPlay(p, g){
  playOpen(g, function(){ if(HQV.mp && HQV.mp.send) HQV.mp.send(g, "invite", {to:p.userId}); toast("Invited "+String(p.trainerName||p.displayName||p.handle).slice(0,40)+" to "+playName(g), "level"); });
}
(function(){ qpRender(); partyRender(); })();
