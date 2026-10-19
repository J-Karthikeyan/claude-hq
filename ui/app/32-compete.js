/* ================= HQ 2.5: the Compete view and its weekly cups ================= */
// The Compete view (key C) hosts competitive panels from several features, as sub-tabs:
// any file pushes {id, name, icon, order, mount(el), unmount()} onto window.COMPETE_PANELS.
// This file owns the view shell and the Cups panel (order 10): a weekly points race per game on
// a track/level/course the Arena picks, a monthly season podium, and cup trophies on trainer
// cards. Everything is read-only here and on the Arena: cups are scored from the multiplayer
// results the Arena's referees already recorded, so nothing new leaves this machine.

/* ---- styles: tokens only, injected once (keeps ui/hq.css free of per-feature hunks) ---- */
function cpStyle(){
  if(document.getElementById("competeCss")) return;
  var s=document.createElement("style"); s.id="competeCss";
  s.textContent=[
    "#competeView{display:grid;gap:14px}",
    "#competeView.hidden{display:none}",
    ".cp-tabs{display:flex;flex-wrap:wrap;gap:6px}",
    ".cp-tab{height:34px;padding:0 12px;border-radius:999px;border:1px solid var(--line);background:var(--panel);color:var(--muted);font:inherit;font-weight:600;cursor:pointer;display:inline-flex;align-items:center;gap:6px}",
    ".cp-tab[aria-selected=\"true\"]{color:var(--brand);border-color:var(--brand-line);background:var(--brand-soft)}",
    ".cp-tab:focus-visible,.cp-btn:focus-visible,.cp-who:focus-visible{outline:2px solid var(--brand);outline-offset:2px}",
    ".cp-panel{display:grid;gap:14px}",
    ".cp-card{background:var(--panel);border:1px solid var(--line);border-radius:var(--radius);box-shadow:var(--shadow);padding:14px 16px;display:grid;gap:10px;min-width:0}",
    ".cp-head{display:flex;flex-wrap:wrap;align-items:baseline;justify-content:space-between;gap:8px}",
    ".cp-head h3,.cp-head h4{margin:0}",
    ".cp-head h3{font-size:17px}.cp-head h4{font-size:15px}",
    ".cp-sub{color:var(--muted);font-size:12.5px}",
    ".cp-faint{color:var(--faint);font-size:12px}",
    ".cp-grid{display:grid;grid-template-columns:repeat(auto-fill,minmax(300px,1fr));gap:12px}",
    ".cp-cup .cp-key{font-weight:700;color:var(--ink)}",
    ".cp-you{display:flex;gap:8px;align-items:center;flex-wrap:wrap;font-size:12.5px;padding:6px 9px;border-radius:var(--radius-sm);background:var(--brand-soft);border:1px solid var(--brand-line)}",
    ".cp-you.none{background:var(--panel2);border-color:var(--line);color:var(--muted)}",
    ".cp-table{width:100%;border-collapse:collapse;font-size:12.5px}",
    ".cp-table caption{text-align:left;color:var(--faint);font-size:11.5px;padding-bottom:4px}",
    ".cp-table th{text-align:left;color:var(--muted);font-weight:600;padding:4px 6px;border-bottom:1px solid var(--line)}",
    ".cp-table td{padding:5px 6px;border-bottom:1px solid var(--line);font-variant-numeric:tabular-nums}",
    ".cp-table td.n,.cp-table th.n{text-align:right}",
    ".cp-table tr.you td{background:var(--brand-soft)}",
    ".cp-who{all:unset;cursor:pointer;font-weight:600}",
    ".cp-who:hover{text-decoration:underline}",
    ".cp-acts{display:flex;flex-wrap:wrap;gap:6px;align-items:center}",
    ".cp-btn{height:32px;padding:0 12px;border-radius:8px;border:1px solid var(--line);background:var(--panel);color:var(--ink);font:inherit;font-weight:600;cursor:pointer}",
    ".cp-btn.primary{background:var(--brand);border-color:var(--brand);color:var(--on-brand)}",
    ".cp-btn.ghost{border-color:transparent;background:none;color:var(--muted)}",
    ".cp-btn:hover{border-color:var(--brand-line)}",
    ".cp-podium{display:grid;grid-template-columns:repeat(3,minmax(0,1fr));align-items:end;gap:8px;text-align:center}",
    ".cp-step{display:grid;gap:4px;justify-items:center;min-width:0}",
    ".cp-step .cp-av{width:44px;height:44px;border-radius:12px;display:grid;place-items:center;font-weight:700;color:var(--brand);border:2px solid var(--brand-line);background:var(--brand-soft)}",
    ".cp-step .cp-block{width:100%;border-radius:8px 8px 0 0;border:1px solid var(--line);background:var(--panel2);display:grid;place-items:center;font:700 18px var(--mono);color:var(--muted)}",
    ".cp-step.p1 .cp-block{height:74px;background:var(--goldbg);border-color:color-mix(in srgb,var(--gold) 45%,var(--line));color:var(--gold)}",
    ".cp-step.p2 .cp-block{height:54px}",
    ".cp-step.p3 .cp-block{height:40px}",
    ".cp-step .cp-name{font-weight:600;font-size:12.5px;max-width:100%;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}",
    ".cp-empty{color:var(--muted);font-size:12.5px;margin:0}",
    ".cp-last{display:grid;grid-template-columns:repeat(auto-fill,minmax(200px,1fr));gap:8px}",
    ".cp-last>div{background:var(--panel2);border:1px solid var(--line);border-radius:var(--radius-sm);padding:8px 10px;font-size:12.5px;display:grid;gap:3px}",
    ".cp-rules{font-size:12.5px;color:var(--muted)}",
    ".cp-rules summary{cursor:pointer;font-weight:600;color:var(--ink)}",
    ".cp-rules ul{margin:6px 0 0;padding-left:18px;display:grid;gap:3px}",
    ".cp-select{height:32px;border-radius:8px;border:1px solid var(--line);background:var(--panel);color:var(--ink);font:inherit;padding:0 8px}",
    ".cp-trophies{display:flex;flex-wrap:wrap;gap:4px}",
    ".cp-tro{font-size:12px;border:1px solid var(--line);border-radius:999px;padding:2px 9px;background:var(--panel2)}",
    ".cp-tro.p1{border-color:color-mix(in srgb,var(--gold) 45%,var(--line));background:var(--goldbg)}",
    ".cp-tro.season{border-color:var(--brand-line);background:var(--brand-soft)}",
    ".cp-modal-back{position:fixed;inset:0;background:var(--scrim);display:flex;align-items:center;justify-content:center;z-index:80;padding:16px}",
    ".cp-modal{width:min(980px,100%);max-height:90vh;overflow:auto;background:var(--bg);border:1px solid var(--line);border-radius:var(--radius);box-shadow:var(--shadow-lg);padding:14px 16px;display:grid;gap:12px}",
    ".cp-modal-top{display:flex;justify-content:space-between;align-items:center}"
  ].join("\n");
  document.head.appendChild(s);
}

/* ---- small helpers ---- */
var CP_GAMES = {kart:["🏎️","Kart Racing"], rally:["🏁","Valley Rally"], plat:["🏃","Platformer Rush"], golf:["⛳","Mini Golf"],
                fps:["🔫","Blaster Arena"], type:["⌨️","Code Typing Race"], bowl:["🎳","Bowling"]};
function cpGame(g){ return CP_GAMES[g] || ["🏆", String(g||"")]; }
function cpGet(p){
  return fetch(p,{cache:"no-store"}).then(function(r){
    return r.json().then(function(j){ return {ok:r.ok, code:r.status, j:j||{}}; }, function(){ return {ok:false, code:r.status, j:{}}; });
  });
}
function cpText(tag, cls, text){ var e=el(tag, cls); if(text!=null) e.textContent=String(text); return e; }
function cpBtn(label, cls, fn){ var b=cpText("button", "cp-btn"+(cls?" "+cls:""), label); b.type="button"; if(fn) b.addEventListener("click", fn); return b; }
// "2026-10-11 23:59:59.999999" (UTC) -> ms since epoch, or NaN.
function cpUtc(s){ s=String(s||""); return Date.parse(s.slice(0,19).replace(" ","T")+"Z"); }
function cpLeft(endsAt){
  var ms=cpUtc(endsAt)-Date.now(); if(isNaN(ms)) return "";
  if(ms<=0) return "finishing up";
  var m=Math.floor(ms/60000), d=Math.floor(m/1440), h=Math.floor((m%1440)/60);
  return d>0 ? d+"d "+h+"h left" : h>0 ? h+"h "+(m%60)+"m left" : Math.max(1,m)+"m left";
}
function cpPlace(n){ n=n|0; var s=["th","st","nd","rd"], v=n%100; return n+(s[(v-20)%10]||s[v]||s[0]); }
function cpMedal(p){ return p===1 ? "🥇" : p===2 ? "🥈" : p===3 ? "🥉" : ""; }
function cpName(s){ s=s||{}; return s.displayName || s.handle || "Trainer"; }
function cpInitials(s){ return cpName(s).slice(0,2).toUpperCase(); }
function cpSeasonName(season){
  var m=/^(\d{4})-(\d{2})$/.exec(String(season||"")); if(!m) return String(season||"");
  var names=["January","February","March","April","May","June","July","August","September","October","November","December"];
  return (names[(+m[2])-1]||m[2])+" "+m[1];
}
function cpPaired(){ return typeof ARENA!=="undefined" && ARENA && ARENA.paired; }

/* ---- the Compete view: sub-tabs over window.COMPETE_PANELS ---- */
var COMPETE = {tab:null, mounted:null, mountedEl:null, on:false};
function competePanels(){
  var list=(window.COMPETE_PANELS||[]).filter(function(p){ return p && p.id && typeof p.mount==="function"; });
  return list.slice().sort(function(a,b){ return ((a.order|0)-(b.order|0)) || String(a.id).localeCompare(String(b.id)); });
}
function competeSavedTab(){ try { return localStorage.getItem("hq_compete_tab") || ""; } catch(e){ return ""; } }
function competeSaveTab(id){ try { localStorage.setItem("hq_compete_tab", id); } catch(e){} }
function competeUnmount(){
  var m=COMPETE.mounted; COMPETE.mounted=null; COMPETE.mountedEl=null;
  if(m && typeof m.unmount==="function"){ try { m.unmount(); } catch(e){} }
}
function competeEnter(){
  if(typeof COMPETE==="undefined" || !COMPETE) return;   // startup ran setView before this file: the tail below enters
  cpStyle();
  var root=$("competeView"); if(!root) return;
  COMPETE.on=true;
  var panels=competePanels();
  var want=COMPETE.tab || competeSavedTab();
  if(!panels.some(function(p){ return p.id===want; })) want=panels.length ? panels[0].id : null;
  competeShow(want, false);
}
function competeLeave(){
  if(typeof COMPETE==="undefined" || !COMPETE) return;
  COMPETE.on=false;
  competeUnmount();
}
// Render the tab strip and mount one panel. focusTab moves focus to the chosen tab (keyboard use).
function competeShow(id, focusTab){
  var root=$("competeView"); if(!root) return;
  competeUnmount();
  root.textContent="";
  var panels=competePanels();
  if(!panels.length){ root.appendChild(cpText("p","cp-empty","Nothing to compete in yet.")); return; }
  var tabs=el("div","cp-tabs"); tabs.setAttribute("role","tablist"); tabs.setAttribute("aria-label","Compete");
  var body=el("div","cp-panel"); body.id="competePanel"; body.setAttribute("role","tabpanel");
  var btns=[];
  panels.forEach(function(p){
    var b=el("button","cp-tab"); b.type="button"; b.id="cpTab-"+String(p.id).replace(/[^a-z0-9_-]/gi,"");
    b.setAttribute("role","tab");
    var sel=p.id===id;
    b.setAttribute("aria-selected", sel ? "true" : "false");
    b.setAttribute("aria-controls","competePanel");
    b.tabIndex=sel ? 0 : -1;
    if(p.icon){ var ic=cpText("span","",p.icon); ic.setAttribute("aria-hidden","true"); b.appendChild(ic); }
    b.appendChild(cpText("span","",p.name||p.id));
    b.addEventListener("click", function(){ competeSelect(p.id, false); });
    b.addEventListener("keydown", function(e){
      var i=btns.indexOf(b), n=btns.length, j=-1;
      if(e.key==="ArrowRight"||e.key==="ArrowDown") j=(i+1)%n;
      else if(e.key==="ArrowLeft"||e.key==="ArrowUp") j=(i-1+n)%n;
      else if(e.key==="Home") j=0; else if(e.key==="End") j=n-1;
      if(j<0) return;
      e.preventDefault(); competeSelect(panels[j].id, true);
    });
    btns.push(b); tabs.appendChild(b);
    if(sel) body.setAttribute("aria-labelledby", b.id);
  });
  root.appendChild(tabs); root.appendChild(body);
  var cur=null; panels.forEach(function(p){ if(p.id===id) cur=p; });
  if(!cur) return;
  COMPETE.tab=cur.id; COMPETE.mounted=cur; COMPETE.mountedEl=body;
  try { cur.mount(body); } catch(e){ body.textContent=""; body.appendChild(cpText("p","cp-empty","This panel failed to load.")); }
  if(focusTab){ var f=$("cpTab-"+String(cur.id).replace(/[^a-z0-9_-]/gi,"")); if(f) f.focus(); }
}
function competeSelect(id, focusTab){
  if(id===COMPETE.tab && COMPETE.mounted){ if(focusTab){ var f=$("cpTab-"+String(id).replace(/[^a-z0-9_-]/gi,"")); if(f) f.focus(); } return; }
  competeSaveTab(id);
  competeShow(id, focusTab);
}

/* ---- the Cups panel ---- */
var CUPS = {data:null, season:null, seasonSel:null, timer:null, box:null, gen:0, lastYou:{}};
function cupsMount(box){
  cpStyle();
  CUPS.box=box; CUPS.gen++;
  box.textContent="";
  box.appendChild(cpText("p","cp-empty","Loading this week's cups…"));
  cupsLoad();
  if(CUPS.timer) clearInterval(CUPS.timer);
  // The Arena caches a live cup for 30 s: refresh on the same beat while the panel is open.
  CUPS.timer=setInterval(function(){ if(document.visibilityState!=="hidden") cupsLoad(); }, 30000);
}
function cupsUnmount(){
  if(CUPS.timer){ clearInterval(CUPS.timer); CUPS.timer=null; }
  CUPS.box=null; CUPS.gen++;
}
function cupsLoad(){
  var gen=CUPS.gen, box=CUPS.box; if(!box) return;
  var want=CUPS.seasonSel;
  Promise.all([cpGet("/api/arena/cups"), cpGet("/api/arena/cups/season"+(want ? "?season="+encodeURIComponent(want) : ""))]).then(function(rs){
    if(gen!==CUPS.gen || box!==CUPS.box) return;
    var a=rs[0], s=rs[1];
    if(!a.ok){ cupsError(box, a); return; }
    CUPS.data=a.j; CUPS.season=s.ok ? s.j : null;
    cupsRender(box);
  }).catch(function(){ if(gen===CUPS.gen && box===CUPS.box){ box.textContent=""; box.appendChild(cpText("p","cp-empty","The Arena didn't answer.")); } });
}
function cupsError(box, res){
  box.textContent="";
  var msg = res.code===404 ? "This Arena doesn't run weekly cups yet: ask its owner to update it."
          : (!cpPaired() || res.code===400) ? "Pair with an Arena (the Arena view, key 6) to race in the weekly cups."
          : ((res.j && (res.j.error||res.j.detail)) || "The Arena didn't answer.");
  box.appendChild(cpText("p","cp-empty",msg));
}
function cupsRender(box){
  var d=CUPS.data||{}; box.textContent="";
  // Header: the week and how long is left.
  var head=el("div","cp-card");
  var hh=el("div","cp-head");
  hh.appendChild(cpText("h3","","Weekly cups · "+(d.week||"")));
  var left=cpText("span","cp-sub",cpLeft(d.endsAt)); left.setAttribute("aria-live","off"); hh.appendChild(left);
  head.appendChild(hh);
  head.appendChild(cpText("p","cp-sub","Every multiplayer race on a cup's track counts: finish in the top four to score. Ends Sunday 23:59 UTC; the podium gets trophies on their trainer card."));
  head.appendChild(cupsRules());
  box.appendChild(head);

  // This week's cups.
  var grid=el("div","cp-grid");
  (d.cups||[]).forEach(function(c){ grid.appendChild(cupCard(c)); });
  if(!(d.cups||[]).length) grid.appendChild(cpText("p","cp-empty","No cups this week."));
  box.appendChild(grid);

  // Season podium.
  box.appendChild(seasonCard(CUPS.season, d.season));

  // Last week's podiums.
  var lw=(d.lastWeek||[]).filter(function(c){ return (c.standings||[]).length; });
  var last=el("div","cp-card");
  var lh=el("div","cp-head"); lh.appendChild(cpText("h4","","Last week")); last.appendChild(lh);
  if(!lw.length) last.appendChild(cpText("p","cp-empty","No finished cups last week."));
  else {
    var row=el("div","cp-last");
    lw.forEach(function(c){
      var g=cpGame(c.game), x=el("div");
      x.appendChild(cpText("strong","",g[0]+" "+(c.name||g[1])));
      (c.standings||[]).slice(0,3).forEach(function(s){
        var line=el("div");
        line.appendChild(cpText("span","",cpMedal(s.place)+" "));
        line.appendChild(cupWho(s));
        line.appendChild(cpText("span","cp-faint"," · "+(s.points|0)+" pts"));
        x.appendChild(line);
      });
      row.appendChild(x);
    });
    last.appendChild(row);
  }
  box.appendChild(last);
  cupsAnnounce(d);
}
// Tell a screen reader (once per change) when your place in a live cup moves.
function cupsAnnounce(d){
  var msgs=[];
  (d.cups||[]).forEach(function(c){
    var y=c.you, prev=CUPS.lastYou[c.id], now=y ? (y.place|0)+":"+(y.points|0) : "";
    if(prev!=null && now && prev!==now) msgs.push(cpGame(c.game)[1]+" cup: you're "+cpPlace(y.place)+" with "+(y.points|0)+" points");
    CUPS.lastYou[c.id]=now;
  });
  if(msgs.length) announce(msgs.join(". "));
}
function cupsRules(){
  var det=el("details","cp-rules");
  det.appendChild(cpText("summary","","How cups are scored"));
  var ul=el("ul");
  ["1st 10 points, 2nd 7, 3rd 5, 4th 3, any other finish 1, a DNF 0. Only races with two or more players count.",
   "Your best five races count. Racing the same opponents over and over counts at most twice per group, so bring new rivals.",
   "Ties go to more wins, then to whoever reached the total first.",
   "Each month's cup points add up to the season podium: champion, runner-up and third get a season trophy."
  ].forEach(function(t){ ul.appendChild(cpText("li","",t)); });
  det.appendChild(ul);
  return det;
}
function cupWho(s){
  var b=cpText("button","cp-who",cpName(s)+(s.isYou?" (you)":""));
  b.type="button";
  b.setAttribute("aria-label","Trainer card for "+cpName(s));
  b.addEventListener("click", function(){ if(typeof tcardOpen==="function") tcardOpen(s.userId); });
  return b;
}
function cupTable(list, caption, withRaces){
  var tbl=el("table","cp-table");
  tbl.appendChild(cpText("caption","",caption));
  var hr=el("tr");
  [["#",""],["Trainer",""],["Points","n"]].concat(withRaces ? [["Races","n"]] : []).forEach(function(c){
    var th=cpText("th",c[1],c[0]); th.scope="col"; hr.appendChild(th);
  });
  tbl.appendChild(hr);
  list.forEach(function(s){
    var tr=el("tr"); if(s.isYou) tr.className="you";
    tr.appendChild(cpText("td","",(cpMedal(s.place)||"")+(s.place|0)));
    var who=el("td"); who.appendChild(cupWho(s)); tr.appendChild(who);
    tr.appendChild(cpText("td","n",s.points|0));
    if(withRaces) tr.appendChild(cpText("td","n",s.races|0));
    tbl.appendChild(tr);
  });
  return tbl;
}
function cupCard(c){
  var g=cpGame(c.game), card=el("section","cp-card cp-cup");
  card.setAttribute("aria-label", g[1]+" cup");
  var hh=el("div","cp-head");
  hh.appendChild(cpText("h4","",g[0]+" "+g[1]));
  hh.appendChild(cpText("span","cp-faint",(c.entrants|0)+" racing"));
  card.appendChild(hh);
  var key=el("div","cp-sub"); key.appendChild(cpText("span","","This week: "));
  key.appendChild(cpText("span","cp-key",c.keyName||c.key));
  card.appendChild(key);
  var y=c.you;
  var you=el("div","cp-you"+(y ? "" : " none"));
  if(y) you.textContent="You: "+cpPlace(y.place)+" · "+(y.points|0)+" pts from "+(y.races|0)+" race"+((y.races|0)===1?"":"s");
  else you.textContent="You haven't raced this cup yet.";
  card.appendChild(you);
  var st=c.standings||[];
  if(st.length) card.appendChild(cupTable(st, "Top "+st.length+" of "+(c.entrants|0), true));
  else card.appendChild(cpText("p","cp-empty","No one has scored yet: be the first."));
  var acts=el("div","cp-acts");
  acts.appendChild(cpBtn("Race this cup","primary",function(){ cupRace(c); }));
  acts.appendChild(cpBtn("Full table","ghost",function(){ cupFull(c); }));
  card.appendChild(acts);
  return card;
}
// Open the game's multiplayer lobby and say which track/level/course to pick. No preselect hook:
// the host picks it in the game's own menu, and only races on that key score.
function cupRace(c){
  var g=cpGame(c.game), what=(c.keyName||c.key);
  var tip = c.game==="fps" ? "Any Blaster match counts." : c.game==="type" ? "Any typing race counts." : c.game==="bowl" ? "Pick "+what+" (no bumpers)."
          : "Pick "+what+" in the "+g[1]+" menu.";
  toast(g[0]+" "+g[1]+" cup: "+tip+" Race with at least one friend to score.");
  announce(g[1]+" cup. "+tip);
  if(typeof playOpen==="function"){ if(typeof competeOverlayClose==="function") competeOverlayClose(); playOpen(c.game); return; }
  if(typeof setView==="function") setView("valley");
  if(typeof valleyLoad==="function") valleyLoad().then(function(){ if(window.HQV && HQV.api && HQV.api.open) HQV.api.open("mp-"+c.game); });
}
function cupFull(c){
  cpGet("/api/arena/cups/one?id="+encodeURIComponent(c.id)).then(function(res){
    var body=competeOverlay(cpGame(c.game)[1]+" cup · "+(c.week||""));
    if(!body) return;
    if(!res.ok){ body.appendChild(cpText("p","cp-empty",(res.j && (res.j.error||res.j.detail)) || "The Arena didn't answer.")); return; }
    var v=res.j;
    body.appendChild(cpText("p","cp-sub",(v.keyName||v.key)+" · "+(v.status==="final" ? "final" : cpLeft(v.endsAt))));
    if((v.standings||[]).length) body.appendChild(cupTable(v.standings, (v.entrants|0)+" trainers", true));
    else body.appendChild(cpText("p","cp-empty","No one has scored yet."));
    if(v.you && !(v.standings||[]).some(function(s){ return s.isYou; })) body.appendChild(cpText("p","cp-sub","You: "+cpPlace(v.you.place)+" · "+(v.you.points|0)+" pts"));
  });
}

/* ---- the season podium ---- */
function seasonOptions(current){
  var m=/^(\d{4})-(\d{2})$/.exec(String(current||"")); if(!m) return [];
  var y=+m[1], mo=+m[2], out=[];
  for(var i=0;i<12;i++){
    var s=y+"-"+(mo<10?"0":"")+mo;
    if(s<"2026-01") break;
    out.push(s);
    mo--; if(mo<1){ mo=12; y--; }
  }
  return out;
}
function seasonCard(s, current){
  var card=el("section","cp-card"); card.setAttribute("aria-label","Season podium");
  var hh=el("div","cp-head");
  var title=(s && s.season) ? s.season : (CUPS.seasonSel || current);
  hh.appendChild(cpText("h3","","Season · "+cpSeasonName(title)));
  var opts=seasonOptions(current);
  if(opts.length>1){
    var lab=el("label","cp-sub"); lab.appendChild(cpText("span","","Season "));
    var sel=el("select","cp-select");
    opts.forEach(function(o){ var op=cpText("option","",cpSeasonName(o)); op.value=o; if(o===title) op.selected=true; sel.appendChild(op); });
    sel.addEventListener("change", function(){ CUPS.seasonSel = sel.value===current ? null : sel.value; cupsSeasonReload(); });
    lab.appendChild(sel); hh.appendChild(lab);
  }
  card.appendChild(hh);
  if(!s){ card.appendChild(cpText("p","cp-empty","The season table isn't available right now.")); return card; }
  card.appendChild(cpText("p","cp-sub", s.status==="final" ? "Final: "+(s.cups|0)+" cups." : "Live: the sum of every cup ending this month, "+(s.cups|0)+" cups so far."));
  var pod=s.podium||[];
  if(!pod.length) card.appendChild(cpText("p","cp-empty","No points yet this season."));
  else {
    var row=el("div","cp-podium"); row.setAttribute("role","list");
    // Classic podium order: 2nd, 1st, 3rd.
    [pod[1], pod[0], pod[2]].forEach(function(p){
      var step=el("div","cp-step"+(p ? " p"+p.place : "")); step.setAttribute("role","listitem");
      if(p){
        var av=cpText("div","cp-av",cpInitials(p)); av.setAttribute("aria-hidden","true"); step.appendChild(av);
        var nm=el("div","cp-name"); nm.appendChild(cupWho(p)); step.appendChild(nm);
        step.appendChild(cpText("div","cp-faint",(p.points|0)+" pts"));
        var blk=cpText("div","cp-block",cpMedal(p.place)+" "+p.place); blk.setAttribute("aria-label",cpPlace(p.place)+" place"); step.appendChild(blk);
      }
      row.appendChild(step);
    });
    card.appendChild(row);
  }
  var st=s.standings||[];
  if(st.length>3){
    var det=el("details","cp-rules"); det.appendChild(cpText("summary","","Full season table"));
    det.appendChild(cupTable(st, "Season points", false)); card.appendChild(det);
  }
  if(s.you && !st.some(function(x){ return x.isYou; })) card.appendChild(cpText("p","cp-sub","You: "+cpPlace(s.you.place)+" · "+(s.you.points|0)+" pts"));
  return card;
}
function cupsSeasonReload(){
  var gen=CUPS.gen, box=CUPS.box; if(!box) return;
  cpGet("/api/arena/cups/season"+(CUPS.seasonSel ? "?season="+encodeURIComponent(CUPS.seasonSel) : "")).then(function(res){
    if(gen!==CUPS.gen || box!==CUPS.box) return;
    CUPS.season=res.ok ? res.j : null;
    cupsRender(box);
    announce("Season "+cpSeasonName(CUPS.seasonSel || (CUPS.data && CUPS.data.season)));
  });
}

/* ---- a standalone modal (cupsOpen) and the full-table overlay ---- */
var CP_OVERLAY = {back:null, ret:null, onClose:null};
function competeOverlay(title, onClose){
  cpStyle();
  competeOverlayClose();
  var back=el("div","cp-modal-back");
  var box=el("div","cp-modal"); box.setAttribute("role","dialog"); box.setAttribute("aria-modal","true");
  var top=el("div","cp-modal-top"); var h=cpText("b","",title); h.id="cpOverlayTitle"; box.setAttribute("aria-labelledby","cpOverlayTitle");
  var x=cpBtn("✕","ghost",competeOverlayClose); x.setAttribute("aria-label","Close");
  top.appendChild(h); top.appendChild(x); box.appendChild(top);
  var body=el("div","cp-panel"); box.appendChild(body);
  back.appendChild(box);
  back.addEventListener("click", function(e){ if(e.target===back) competeOverlayClose(); });
  back.addEventListener("keydown", function(e){
    if(e.key==="Escape"){ e.preventDefault(); e.stopPropagation(); competeOverlayClose(); return; }
    if(e.key!=="Tab") return;
    var f=box.querySelectorAll("button,select,summary,[tabindex]:not([tabindex='-1'])"); if(!f.length) return;
    var first=f[0], last=f[f.length-1];
    if(e.shiftKey && document.activeElement===first){ e.preventDefault(); last.focus(); }
    else if(!e.shiftKey && document.activeElement===last){ e.preventDefault(); first.focus(); }
  });
  CP_OVERLAY.ret=document.activeElement; CP_OVERLAY.back=back; CP_OVERLAY.onClose=onClose||null;
  document.body.appendChild(back);
  x.focus();
  return body;
}
function competeOverlayClose(){
  var b=CP_OVERLAY.back; if(!b) return;
  CP_OVERLAY.back=null;
  var fn=CP_OVERLAY.onClose; CP_OVERLAY.onClose=null;
  if(fn){ try { fn(); } catch(e){} }
  if(b.parentNode) b.parentNode.removeChild(b);
  var r=CP_OVERLAY.ret; CP_OVERLAY.ret=null;
  if(r && r.focus && document.contains(r)) try { r.focus(); } catch(e){}
}
// The cups as a modal from anywhere (the Arena view, a toast, another feature).
function cupsOpen(){
  var body=competeOverlay("Weekly cups", cupsUnmount);
  if(body) cupsMount(body);
}

/* ---- trophies on the trainer card ---- */
(window.TCARD_EXTRAS=window.TCARD_EXTRAS||[]).push(function(box, prof){
  if(!box || !prof || !prof.userId) return;
  var sec=cpText("div","tcard-sec","Cup trophies"); sec.hidden=true;
  var list=el("div","cp-trophies"); list.hidden=true;
  box.appendChild(sec); box.appendChild(list);
  cpStyle();
  cpGet("/api/arena/cups/trophies?u="+encodeURIComponent(prof.isYou ? "me" : prof.userId)).then(function(res){
    if(!res.ok || !list.isConnected) return;           // an older Arena (404) or the card moved on
    var t=res.j.trophies||[];
    sec.hidden=false; list.hidden=false;
    if(!t.length){ list.appendChild(cpText("span","muted", prof.isYou ? "None yet: finish on a weekly cup podium (key C)." : "None yet.")); return; }
    var sum=[];
    if(res.j.gold) sum.push("🥇×"+res.j.gold); if(res.j.silver) sum.push("🥈×"+res.j.silver); if(res.j.bronze) sum.push("🥉×"+res.j.bronze); if(res.j.seasons) sum.push("👑×"+res.j.seasons);
    if(sum.length) sec.textContent="Cup trophies · "+sum.join(" ");
    t.slice(0,12).forEach(function(x){
      var season=x.kind==="season";
      var c=cpText("span","cp-tro"+(season ? " season" : x.place===1 ? " p1" : ""), (season ? "👑 " : cpMedal(x.place)+" ")+x.label);
      list.appendChild(c);
    });
    if(t.length>12) list.appendChild(cpText("span","muted","+"+(t.length-12)+" more"));
  }).catch(function(){});
});

/* ---- register the panel and un-hide the tab ---- */
(window.COMPETE_PANELS=window.COMPETE_PANELS||[]).push({id:"cups", name:"Cups", icon:"🏆", order:10, mount:cupsMount, unmount:cupsUnmount});
(function(){
  var t=document.querySelector('.viewtab[data-view="compete"]'); if(t) t.hidden=false;
  if(typeof VIEW!=="undefined" && VIEW==="compete") competeEnter();
})();
