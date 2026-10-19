/* ================= HQ 2.5: the skill tree ================= */
// Perks and titles earned from HOW you work. This machine classifies your own transcripts
// (tool use and file kinds, worksignals.py) into seven per-day counts; only those seven
// integers per UTC day reach the Arena, and only after you say yes (config workSignals).
// The Arena turns them into tiers, perks and titles. Other trainers see tiers and the title
// you wear, never counts. Panels: the Compete view (order 30), skillsOpen() (a modal), and
// a strip on every trainer card (TCARD_EXTRAS).
var SKILLS = {data:null, at:0, local:null, busy:false};
// Mirrors backend/src/skills.rs CATS / PERKS / TIERS (tests/test_skills_proxy.py keeps them in step).
var SKILL_TIERS = [10,50,150,400,1000];
var SKILL_CATS = [
  {id:"tests",    name:"Testing",     icon:"🧪", how:"running test suites and editing tests",
   perks:["Smoke Tester","Assertive","Test Pilot","Coverage Hound","Green Machine"]},
  {id:"refactor", name:"Refactoring", icon:"🧩", how:"multi-part edits that reshape code",
   perks:["Tidy Up","Renamer","Untangler","Pattern Weaver","Architect"]},
  {id:"docs",     name:"Docs",        icon:"📜", how:"writing Markdown, text and docs/ files",
   perks:["Note Taker","Readme Writer","Scribe","Chronicler","Loremaster"]},
  {id:"review",   name:"Review",      icon:"🔍", how:"reading diffs, logs and pull requests",
   perks:["Skimmer","Nitpicker","Second Pair of Eyes","Sharp Eye","Gatekeeper"]},
  {id:"debug",    name:"Debugging",   icon:"🐞", how:"retrying after errors and chasing tracebacks",
   perks:["Bug Spotter","Stack Reader","Bug Hunter","Root Causer","Exterminator"]},
  {id:"explore",  name:"Exploring",   icon:"🧭", how:"searching and reading code",
   perks:["Wanderer","Pathfinder","Scout","Surveyor","Cartographer"]},
  {id:"build",    name:"Building",    icon:"🔨", how:"writing and editing code",
   perks:["Tinkerer","Builder","Maker","Engineer","Forgemaster"]}
];
var SKILL_TITLE_TIERS = [3,5];

function skillCat(id){ for(var i=0;i<SKILL_CATS.length;i++){ if(SKILL_CATS[i].id===id) return SKILL_CATS[i]; } return null; }
// "tests-3" -> "Test Pilot" (null for anything that is not a title id).
function skillTitleName(id){
  if(typeof id!=="string") return null;
  var m=/^([a-z]+)-([35])$/.exec(id); if(!m) return null;
  var c=skillCat(m[1]); return c ? c.perks[(m[2]|0)-1] : null;
}
function skillGet(path){
  return fetch(path,{cache:"no-store"}).then(function(r){
    return r.json().then(function(j){ return {ok:r.ok, code:r.status, j:j||{}}; }, function(){ return {ok:false, code:r.status, j:{}}; });
  });
}
function skillPost(path, body){
  return fetch(path,{method:"POST", headers:{"Content-Type":"application/json","X-HQ-Token":CSRF}, body:JSON.stringify(body||{})})
    .then(function(r){ return r.json().then(function(j){ return {ok:r.ok, code:r.status, j:j||{}}; }, function(){ return {ok:r.ok, code:r.status, j:{}}; }); });
}
function skillsLoad(force){
  if(!force && SKILLS.data && Date.now()-SKILLS.at < 60000) return Promise.resolve(SKILLS.data);
  var a=skillGet("/api/arena/skills?u=me").then(function(res){
    if(res.ok && res.j && res.j.cats){ SKILLS.data=res.j; SKILLS.at=Date.now(); SKILLS.err=null; }
    else { SKILLS.err = res.code===404 ? "old" : (res.j.error||res.j.detail||"The Arena didn't answer."); }
  }).catch(function(){ SKILLS.err="The Arena didn't answer."; });
  var b=skillGet("/api/skills/local").then(function(res){ if(res.ok) SKILLS.local=res.j; }).catch(function(){});
  return Promise.all([a,b]).then(function(){ return SKILLS.data; });
}

/* ---- the tree ---- */
function skillPips(tier, label){
  var p=document.createElement("span"); p.className="sk-pips"; p.setAttribute("role","img");
  p.setAttribute("aria-label", (label ? label+": " : "")+"tier "+(tier|0)+" of 5");
  for(var i=1;i<=5;i++){ var d=document.createElement("i"); if(i<=(tier|0)) d.className="on"; d.setAttribute("aria-hidden","true"); p.appendChild(d); }
  return p;
}
function skillsRender(box){
  if(!box) return;
  box.textContent="";
  var d=SKILLS.data, loc=SKILLS.local||{};
  var intro=document.createElement("p"); intro.className="sk-intro muted";
  intro.textContent="Earned from how you work: this Mac sorts your sessions into seven kinds of work and the Arena turns the daily counts into tiers, perks and titles.";
  box.appendChild(intro);
  if(!d){
    var e=document.createElement("p"); e.className="muted";
    e.textContent = SKILLS.err==="old" ? "This Arena doesn't have the skill tree yet: ask its owner to update it."
      : (typeof ARENA!=="undefined" && ARENA && ARENA.paired===false) ? "Pair with an Arena (Arena tab) to grow your skill tree."
      : (SKILLS.err || "Loading…");
    box.appendChild(e);
    box.appendChild(skillsSharing(loc));
    return;
  }
  // The title you wear.
  var tr=document.createElement("div"); tr.className="sk-titlerow";
  var lbl=document.createElement("label"); lbl.className="sk-titlelbl"; lbl.textContent="Title on your trainer card";
  var sel=document.createElement("select"); sel.className="sk-titlesel"; lbl.htmlFor=sel.id="skTitleSel";
  var none=document.createElement("option"); none.value=""; none.textContent="No title"; sel.appendChild(none);
  (d.titles||[]).forEach(function(t){ var n=skillTitleName(t); if(!n) return; var o=document.createElement("option"); o.value=t; o.textContent=n; sel.appendChild(o); });
  sel.value = d.title && skillTitleName(d.title) ? d.title : "";
  sel.disabled = !(d.titles||[]).length;
  var hint=document.createElement("span"); hint.className="muted sk-small";
  hint.textContent = (d.titles||[]).length ? "Titles unlock at tier 3 and tier 5 of each skill." : "Reach tier 3 in any skill to unlock your first title.";
  sel.addEventListener("change", function(){ skillsWear(sel.value || null, sel); });
  tr.appendChild(lbl); tr.appendChild(sel); tr.appendChild(hint); box.appendChild(tr);
  // One branch per category.
  var grid=document.createElement("div"); grid.className="sk-grid"; grid.setAttribute("role","list");
  var today=(loc.days && typeof loc.days==="object") ? loc.days : {};
  SKILL_CATS.forEach(function(c){
    var v=(d.cats||{})[c.id]||{}, pts=Math.max(0,v.points|0), tier=Math.max(0,Math.min(5,v.tier|0));
    var card=document.createElement("div"); card.className="sk-cat"+(tier>=5?" max":""); card.setAttribute("role","listitem");
    var head=document.createElement("div"); head.className="sk-head";
    var ic=document.createElement("span"); ic.className="sk-ic"; ic.setAttribute("aria-hidden","true"); ic.textContent=c.icon;
    var nm=document.createElement("b"); nm.textContent=c.name;
    head.appendChild(ic); head.appendChild(nm); head.appendChild(skillPips(tier, c.name)); card.appendChild(head);
    var bar=document.createElement("div"); bar.className="sk-bar";
    var lo=tier>0 ? SKILL_TIERS[tier-1] : 0, hi=v.next!=null ? v.next : null;
    var pct = hi ? Math.round(100*(pts-lo)/Math.max(1,hi-lo)) : 100;
    bar.setAttribute("role","progressbar"); bar.setAttribute("aria-label", c.name+" points");
    bar.setAttribute("aria-valuemin","0"); bar.setAttribute("aria-valuemax","100"); bar.setAttribute("aria-valuenow", String(Math.max(0,Math.min(100,pct))));
    var fill=document.createElement("i"); fill.style.width=Math.max(0,Math.min(100,pct))+"%"; bar.appendChild(fill); card.appendChild(bar);
    var sub=document.createElement("div"); sub.className="sk-small muted";
    var wk=0; for(var day in today){ if(today[day] && typeof today[day][c.id]==="number") wk+=today[day][c.id]|0; }
    sub.textContent = pts+" pts"+(hi ? " · "+(hi-pts)+" to tier "+(tier+1) : " · maxed")+" · "+wk+" this week on this Mac";
    card.appendChild(sub);
    var how=document.createElement("div"); how.className="sk-small muted"; how.textContent="From "+c.how+"."; card.appendChild(how);
    var perks=document.createElement("ol"); perks.className="sk-perks"; perks.setAttribute("aria-label", c.name+" perks");
    c.perks.forEach(function(pn, i){
      var t=i+1, li=document.createElement("li");
      var got=t<=tier, isTitle=SKILL_TITLE_TIERS.indexOf(t)>=0;
      li.className="sk-perk"+(got?" got":"")+(isTitle?" title":"");
      li.textContent=pn;
      var tag=document.createElement("small"); tag.textContent = (isTitle ? "title · " : "")+SKILL_TIERS[i]+" pts";
      li.appendChild(tag);
      li.setAttribute("aria-label", pn+", tier "+t+(isTitle?", a title":"")+(got?", unlocked":", locked at "+SKILL_TIERS[i]+" points"));
      perks.appendChild(li);
    });
    card.appendChild(perks);
    grid.appendChild(card);
  });
  box.appendChild(grid);
  box.appendChild(skillsSharing(loc));
}
// What leaves this machine, and the switch.
function skillsSharing(loc){
  var w=document.createElement("div"); w.className="sk-share";
  var on=!!(loc && loc.enabled);
  var p=document.createElement("p"); p.className="sk-small";
  var lr=loc && loc.lastReport;
  p.textContent = on
    ? "Sharing is on: every 15 minutes this Mac sends seven numbers per day (one count per skill) for the last week."+(lr ? " Last sent "+(typeof relTime==="function" ? relTime(lr.at) : lr.at)+(lr.ok ? "." : " (the Arena refused it).") : " Nothing sent yet.")
    : "Sharing is off: your tree only grows while this Mac may send its seven daily counts. No commands, files, paths, projects or text, ever.";
  w.appendChild(p);
  var b=document.createElement("button"); b.type="button"; b.className="hbtn ghost";
  b.textContent = on ? "Stop sharing work signals" : "Share work signals";
  b.addEventListener("click", function(){
    b.disabled=true;
    skillsSetSharing(!on).then(function(){ return skillsLoad(true); }).then(function(){ skillsRerender(); });
  });
  w.appendChild(b);
  return w;
}
function skillsSetSharing(on){
  var body={workSignals:!!on}; if(!on) body.workSignalsPRs=false;
  var post=(typeof postConfig==="function") ? postConfig(body) : skillPost("/api/config", body);
  return post.then(function(r){
    if(r && r.ok){ if(typeof CONFIG!=="undefined" && CONFIG){ CONFIG.workSignals=!!on; if(!on) CONFIG.workSignalsPRs=false; }
      var s=$("setWorkSignals"); if(s) s.checked=!!on;
      announce(on ? "Work signals on" : "Work signals off"); }
    return r;
  }).catch(function(){});
}
function skillsWear(id, sel){
  if(sel) sel.disabled=true;
  return skillPost("/api/arena/skills/title", {title:id}).then(function(res){
    if(sel) sel.disabled=false;
    if(!res.ok){ toast("⚠ "+(res.j.error||res.j.detail||"Couldn't change your title"),"ach"); if(sel && SKILLS.data) sel.value=SKILLS.data.title||""; return; }
    if(SKILLS.data) SKILLS.data.title=res.j.title!==undefined ? res.j.title : id;
    var n=skillTitleName(id);
    toast(n ? "🎖 You now wear “"+n+"”" : "Title removed","level");
    announce(n ? "Title set to "+n : "Title removed");
  }).catch(function(){ if(sel) sel.disabled=false; });
}
var SKILLS_MOUNTS=[];
function skillsRerender(){ SKILLS_MOUNTS.forEach(function(el){ if(el && el.isConnected) skillsRender(el); }); }
function skillsMount(el){
  if(!el) return;
  if(SKILLS_MOUNTS.indexOf(el)<0) SKILLS_MOUNTS.push(el);
  if(SKILLS.data) skillsRender(el); else { el.textContent=""; var p=document.createElement("p"); p.className="muted"; p.textContent="Loading…"; el.appendChild(p); }
  skillsLoad(false).then(function(){ skillsRender(el); });
}
function skillsUnmount(el){ SKILLS_MOUNTS=SKILLS_MOUNTS.filter(function(x){ return x!==el && x && x.isConnected; }); }

/* ---- the modal ---- */
var SKILLS_OPENER=null;
function skillsModal(){
  var back=$("skillsBack"); if(back) return back;
  back=document.createElement("div"); back.className="tcard-back"; back.id="skillsBack"; back.setAttribute("aria-hidden","true");
  var m=document.createElement("div"); m.className="tcard-modal sk-modal"; m.setAttribute("role","dialog"); m.setAttribute("aria-modal","true"); m.setAttribute("aria-labelledby","skillsTitle");
  var top=document.createElement("div"); top.className="tcard-top";
  var t=document.createElement("b"); t.id="skillsTitle"; t.textContent="Skill tree";
  var x=document.createElement("button"); x.type="button"; x.className="hbtn icon ghost"; x.id="skillsClose"; x.setAttribute("aria-label","Close"); x.textContent="✕";
  top.appendChild(t); top.appendChild(x); m.appendChild(top);
  var body=document.createElement("div"); body.id="skillsBody"; m.appendChild(body);
  back.appendChild(m); document.body.appendChild(back);
  x.addEventListener("click", skillsClose);
  back.addEventListener("click", function(e){ if(e.target===back) skillsClose(); });
  back.addEventListener("keydown", function(e){ if(e.key==="Escape"){ e.preventDefault(); e.stopPropagation(); skillsClose(); } });
  return back;
}
function skillsOpen(){
  var back=skillsModal();
  SKILLS_OPENER=document.activeElement;
  back.classList.add("open"); back.setAttribute("aria-hidden","false");
  skillsMount($("skillsBody"));
  var x=$("skillsClose"); if(x) x.focus();
}
function skillsClose(){
  var back=$("skillsBack"); if(!back) return;
  back.classList.remove("open"); back.setAttribute("aria-hidden","true");
  skillsUnmount($("skillsBody"));
  if(SKILLS_OPENER && SKILLS_OPENER.focus && SKILLS_OPENER.isConnected){ try{ SKILLS_OPENER.focus(); }catch(e){} }
  SKILLS_OPENER=null;
}
window.skillsOpen=skillsOpen;

/* ---- Compete view panel (order 30) ---- */
(window.COMPETE_PANELS=window.COMPETE_PANELS||[]).push({id:"skills", name:"Skills", icon:"🌳", order:30,
  mount:function(el){ this._el=el; skillsMount(el); },
  unmount:function(){ skillsUnmount(this._el); this._el=null; }});

/* ---- trainer card strip: title + tier pips (anyone's; public view only for others) ---- */
(window.TCARD_EXTRAS=window.TCARD_EXTRAS||[]).push(function(box, prof){
  if(!box || !prof) return;
  var uid = prof.isYou ? "me" : prof.userId;
  if(!uid || (uid!=="me" && !/^[0-9a-f-]{36}$/i.test(uid))) return;
  var sec=document.createElement("div"); sec.className="sk-tcard"; box.appendChild(sec);
  skillGet("/api/arena/skills?u="+encodeURIComponent(uid)).then(function(res){
    if(!res.ok || !res.j || !res.j.cats) { if(sec.parentNode) sec.parentNode.removeChild(sec); return; }
    var h=document.createElement("div"); h.className="tcard-sec"; h.textContent="Skills"; sec.appendChild(h);
    var tn=skillTitleName(res.j.title);
    if(tn){ var t=document.createElement("span"); t.className="tcard-trophy sk-titlechip"; t.textContent="🎖 "+tn; sec.appendChild(t); }
    var row=document.createElement("div"); row.className="sk-tcrow";
    SKILL_CATS.forEach(function(c){
      var tier=((res.j.cats[c.id]||{}).tier)|0;
      var cell=document.createElement("span"); cell.className="sk-tccell"; cell.title=c.name+": tier "+tier;
      var ic=document.createElement("span"); ic.setAttribute("aria-hidden","true"); ic.textContent=c.icon;
      cell.appendChild(ic); cell.appendChild(skillPips(tier, c.name)); row.appendChild(cell);
    });
    sec.appendChild(row);
    if(prof.isYou){
      var b=document.createElement("button"); b.type="button"; b.className="hbtn ghost sk-open"; b.textContent="Open skill tree";
      b.addEventListener("click", function(){ if(typeof tcardClose==="function") tcardClose(); skillsOpen(); });
      sec.appendChild(b);
    }
  }).catch(function(){ if(sec.parentNode) sec.parentNode.removeChild(sec); });
});

/* ---- the one-time consent prompt (first paired load) ---- */
function skillsAsked(){ try { return !!localStorage.getItem("hq_ws_asked"); } catch(e){ return true; } }
function skillsMarkAsked(v){ try { localStorage.setItem("hq_ws_asked", v||"1"); } catch(e){} }
function skillsConsent(){
  if($("wsConsentBack")) return;
  var back=document.createElement("div"); back.className="tcard-back open"; back.id="wsConsentBack"; back.setAttribute("aria-hidden","false");
  var m=document.createElement("div"); m.className="tcard-modal sk-consent"; m.setAttribute("role","dialog"); m.setAttribute("aria-modal","true");
  m.setAttribute("aria-labelledby","wsConsentTitle"); m.setAttribute("aria-describedby","wsConsentText");
  var t=document.createElement("b"); t.id="wsConsentTitle"; t.textContent="🌳 Grow a skill tree from how you work?"; m.appendChild(t);
  var p=document.createElement("div"); p.id="wsConsentText"; p.className="sk-small";
  var lines=[
    "This Mac can sort your Claude sessions into seven kinds of work: testing, refactoring, docs, review, debugging, exploring and building.",
    "If you say yes, it sends the Arena only seven whole numbers per day (one count per kind) for the last week, every 15 minutes. Those earn perks and titles, and later loot chests.",
    "Never sent: tool names, commands, file names, paths, projects, repos or any text. Friends see your tiers and the title you pick, never the counts.",
    "You can switch it off any time in Settings."
  ];
  lines.forEach(function(s){ var q=document.createElement("p"); q.textContent=s; p.appendChild(q); });
  m.appendChild(p);
  var row=document.createElement("div"); row.className="sk-consent-btns";
  var no=document.createElement("button"); no.type="button"; no.className="hbtn ghost"; no.textContent="Not now";
  var yes=document.createElement("button"); yes.type="button"; yes.className="hbtn"; yes.textContent="Enable";
  row.appendChild(no); row.appendChild(yes); m.appendChild(row);
  back.appendChild(m); document.body.appendChild(back);
  var opener=document.activeElement;
  function close(){ if(back.parentNode) back.parentNode.removeChild(back); if(opener && opener.focus && opener.isConnected){ try{ opener.focus(); }catch(e){} } }
  no.addEventListener("click", function(){ skillsMarkAsked("no"); close(); announce("Work signals stay off"); });
  yes.addEventListener("click", function(){
    yes.disabled=no.disabled=true;
    skillsSetSharing(true).then(function(r){
      if(r && r.ok){ skillsMarkAsked("yes"); close(); toast("🌳 Work signals on: your skill tree grows as you work","level"); }
      else { yes.disabled=no.disabled=false; toast("⚠ Couldn't save that setting","ach"); }
    });
  });
  back.addEventListener("keydown", function(e){
    if(e.key==="Escape"){ e.preventDefault(); e.stopPropagation(); skillsMarkAsked("no"); close(); }
    else if(e.key==="Tab"){ var f=[no,yes]; var i=f.indexOf(document.activeElement); e.preventDefault(); f[(i+(e.shiftKey?f.length-1:1))%f.length].focus(); }
  });
  yes.focus();
}
// Ask once, the first time this HQ is seen paired (and only when sharing is still off).
(function(){
  var tries=0;
  function check(){
    if(skillsAsked() || tries++ > 40) return;
    fetch("/api/arena/status",{cache:"no-store"}).then(function(r){ return r.json(); }).then(function(st){
      if(!st || !st.paired){ setTimeout(check, 30000); return; }
      return fetch("/api/config",{cache:"no-store"}).then(function(r){ return r.json(); }).then(function(c){
        if(c && c.workSignals===true){ skillsMarkAsked("yes"); return; }
        if(!skillsAsked()) skillsConsent();
      });
    }).catch(function(){ setTimeout(check, 30000); });
  }
  setTimeout(check, 4000);
})();
