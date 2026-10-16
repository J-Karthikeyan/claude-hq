/* ---- pokedex view ---- */
var POKEDEX=null, pokedexTimer=null, pokedexLoading=false;
function loadPokedex(){
  if(pokedexLoading) return; pokedexLoading=true;
  fetchPokedex()
    .then(function(d){ POKEDEX=d; renderPokedex(d); })
    .catch(function(){ var g=$("dexGrid"); if(g && !POKEDEX){ g.innerHTML='<div class="feed-empty">Pokédex unavailable.</div>'; } $("dexStats").textContent="—"; })
    .then(function(){ pokedexLoading=false; });
}
// --- pokedex client-side filter/sort state (persisted) ---
var DEX_CAUGHT_ONLY = localStorage.getItem("hq_dex_caught")==="1";
var DEX_SORT = (function(){ var s=localStorage.getItem("hq_dex_sort"); return (s==="sessions"||s==="output"||s==="num")?s:"num"; })();
var DEX_TYPE = "All";
// derive a species' display type/sessions/output consistently with the live cards
function dexTypeOf(sp,cr){ return (isPokePack()) ? (POKE_TYPES[pokeIdx(cr)]||"Normal") : (sp.type||cr.type||"Normal"); }
function dexSessionsOf(sp){ return (sp.count!=null?sp.count:(sp.sessions!=null?sp.sessions:0)); }
function dexOutputOf(sp){ return (sp.totalOutput!=null?sp.totalOutput:(sp.output||sp.outputTokens||0)); }
// Persisted per-species high-water stage so the Pokédex never "de-evolves" (backtracks)
// when the current representative session has fewer prompts than one you had before.
var DEX_MAX=(function(){ try{ return JSON.parse(localStorage.getItem("hq_dex_max")||"{}"); }catch(e){ return {}; } })();
function dexHighWater(species, cur){
  var k=String(species), prev=DEX_MAX[k]||0, m=Math.max(prev, cur||0);
  if(m!==prev){ DEX_MAX[k]=m; try{ localStorage.setItem("hq_dex_max", JSON.stringify(DEX_MAX)); }catch(e){} }
  return m;
}
// ---- Per-species FRACTIONAL growth high-water ----------------------------------------
// DEX_MAX persists the integer STAGE across sessions of a species, but not the within-stage
// progress — so a new session of a species you'd already grown reset the progress bar to 0%.
// This persists a continuous growth value (0..4 = stage + within-stage fraction), keyed the
// same way (species % 48). Seeded ONLY from RAW growth, never the floored value, so it can't
// inflate itself. Read by the evolution/level-up progress bar.
var DEX_MAXG=(function(){ try{ return JSON.parse(localStorage.getItem("hq_dex_maxg")||"{}"); }catch(e){ return {}; } })();
function speciesGrowthHW(species){ var v=DEX_MAXG[String(((species|0)%48+48)%48)]; return (typeof v==="number"&&v>0)?Math.min(4,v):0; }
function seedGrowthHW(species, g){ var k=String(((species|0)%48+48)%48); g=Math.max(0,Math.min(4,g||0));
  if(g>(DEX_MAXG[k]||0)){ DEX_MAXG[k]=g; try{ localStorage.setItem("hq_dex_maxg", JSON.stringify(DEX_MAXG)); }catch(e){} return true; } return false; }
// This session's OWN raw growth (ignores every floor), 0..4 = rawStage + within-stage fraction.
function rawGrowthOf(cr){ cr=cr||{}; var raw=Object.assign({},cr,{_noFloor:true});
  var rs=creatureStage(raw), rp=creatureStagePct(cr)/100; return Math.min(4, rs + (rs<4?rp:0)); }
// Effective growth = best of this session's raw growth and everything persisted for the species
// (fractional high-water AND the integer stage floor), so the bar is monotonic and always stays
// consistent with the floored sprite form. Raises the fractional high-water as a side effect.
function effectiveGrowthOf(cr){ var g=rawGrowthOf(cr); seedGrowthHW(pokeIdx(cr), g);
  return Math.min(4, Math.max(g, speciesGrowthHW(pokeIdx(cr)), speciesHighWater(cr))); }
// Persist a per-session evolution/mega choice, then repaint the affected surfaces. Keyed by the
// SAME id used to stamp cr._sid (sessionId||id), or the party card won't repaint on a pick.
function setEvoChoice(sid, patch){
  if(!sid) return;
  EVO_CHOICE[sid]=Object.assign({},EVO_CHOICE[sid],patch);
  try{ localStorage.setItem("hq_evo_choice", JSON.stringify(EVO_CHOICE)); }catch(e){}
  evoChoicePost(sid, patch);                        // so other browsers and the notch see it too
  // A manual re-pick must never fire an evolution splash — drop this session's baseline so
  // checkEvolution re-snapshots next tick instead of seeing a (mega-gaining) form delta.
  if(typeof STAGE_PREV!=="undefined" && STAGE_PREV) delete STAGE_PREV[sid];
  PARTY_SIG=null;                                   // force party re-render (sig now differs)
  if(typeof STATE!=="undefined" && STATE) renderParty(STATE.sessions);
  if(DRAWER_OPEN && DRAWER_SESS){ if(DRAWER_SESS.creature) DRAWER_SESS.creature._sid=sid; renderDrawer(DRAWER_SESS, DRAWER_LAST); }
  if(typeof POKEDEX!=="undefined" && POKEDEX) renderPokedex(POKEDEX);   // dex cards follow the pick too
}
// The server keeps every pick too (POST /api/evo-choice -> evo-choices.json, local only) and
// sends it back as creature.evo, so the notch and other browsers show the same form.
// Fire-and-forget: localStorage stays the instant cache; a session id the server does not
// know (a Pokédex preview key) is simply refused there.
var EVO_UUID=/^[0-9a-fA-F-]{36}$/, EVO_POSTED={};   // sid -> when this browser last picked (ms)
function evoChoicePost(sid, patch){
  if(!EVO_UUID.test(String(sid||"")) || typeof fetch!=="function") return;
  EVO_POSTED[sid]=Date.now();
  var body={sessionId:sid};
  patch=patch||{};
  if("branchDex" in patch) body.branchDex=(patch.branchDex!=null ? (patch.branchDex|0) : null);
  if("megaSlug" in patch) body.megaSlug=(patch.megaSlug!=null ? String(patch.megaSlug) : null);
  try{ fetch("/api/evo-choice",{method:"POST",headers:{"Content-Type":"application/json","X-HQ-Token":CSRF},
    body:JSON.stringify(body)}).catch(function(){}); }catch(e){}
}
// Server wins: a pick that came back in /api/sessions (made in another browser) replaces this
// browser's cached one. Returns true when anything changed (the caller re-renders).
function evoSeedFromServer(sessions){
  var changed=false;
  (sessions||[]).forEach(function(s){
    var sid=s&&(s.sessionId||s.id), ev=s&&s.creature&&s.creature.evo;
    if(!sid) return;
    // a pick only this browser has (made before the server kept them): hand it over, once
    if(!ev && EVO_CHOICE[sid] && !EVO_POSTED[sid]){ evoChoicePost(sid, EVO_CHOICE[sid]); return; }
    if(!ev || typeof ev!=="object") return;
    if(EVO_POSTED[sid] && Date.now()-EVO_POSTED[sid] < 6000) return;   // our own pick is still on its way
    var cur=EVO_CHOICE[sid]||{}, next={};
    if(ev.branchDex!=null) next.branchDex=ev.branchDex|0;
    if(ev.megaSlug!=null) next.megaSlug=String(ev.megaSlug);
    if(cur.branchDex===next.branchDex && cur.megaSlug===next.megaSlug) return;
    EVO_CHOICE[sid]=Object.assign({},cur,next); changed=true;
  });
  if(changed){ try{ localStorage.setItem("hq_evo_choice", JSON.stringify(EVO_CHOICE)); }catch(e){} PARTY_SIG=null; }
  return changed;
}
// --- New Game+ high-water seeding (drives speciesHighWater/creatureStage flooring) ---
// Raise DEX_MAX for a species from a RAW stage value only (never the floored creatureStage
// output — else an inflated value would lock into localStorage). Returns true if it rose.
function seedHW(species, rawStage){
  var k=String(((species|0)%48+48)%48), v=Math.max(0,Math.min(4,rawStage|0));
  if(v>(DEX_MAX[k]||0)){ DEX_MAX[k]=v; try{ localStorage.setItem("hq_dex_max",JSON.stringify(DEX_MAX)); }catch(e){} return true; }
  return false;
}
// Seed from the live sessions' RAW backend stage each render (before renderParty), so a
// concurrent higher session of a species lifts new ones to the same form immediately.
function seedHighWaterFromSessions(sessions){ var ch=false;
  (sessions||[]).forEach(function(s){ var c=s.creature||{}; if(c.stage==null) return;
    if(seedHW(pokeIdx(c), c.stage)) ch=true;
    seedGrowthHW(pokeIdx(c), rawGrowthOf(c)); });   // persist within-stage progress too
  return ch;
}
// Seed once on startup from the backend Pokédex (cross-session maxStage) regardless of
// whether the Pokédex view was ever opened, then repaint the party if the floor rose.
function primeHighWater(){
  fetchPokedex()
   .then(function(d){ if(!d) return; var list=d.species||d.pokedex||d.entries||[]; var ch=false;
     list.forEach(function(sp){ if(sp&&sp.caught&&sp.maxStage!=null){ if(seedHW(sp.species!=null?sp.species:sp.index, sp.maxStage)) ch=true; } });
     if(ch && typeof STATE!=="undefined" && STATE){ PARTY_SIG=null; renderParty(STATE.sessions); } })
   .catch(function(){});
}
// Session whose branch/mega choice a Pok\u00e9dex card should show: a live session of this species
// that has an explicit choice wins, else the entry's example session (hash default).
function dexChoiceSid(sp, cr){
  var k=pokeIdx(cr), hit=null;
  ((STATE&&STATE.sessions)||[]).some(function(s){ var sid=s.sessionId||s.id, c=s.creature;
    if(c && EVO_CHOICE[sid] && pokeIdx(c)===k){ hit=sid; return true; } return false; });
  return hit || sp.exampleSessionId || null;
}
function dexCrOf(sp,i){
  var caught=!!sp.caught, idx=sp.index!=null?sp.index:i;
  var maxStage=sp.maxStage!=null?sp.maxStage:3;
  if(caught) maxStage=dexHighWater(sp.species!=null?sp.species:idx, maxStage);  // monotonic
  var cr={ species:sp.species||sp.name, name:sp.name, hue:sp.hue, index:idx,
    typeHue:(sp.typeHue!=null?sp.typeHue:sp.hue), type:sp.type,
    stage:Math.max(0,Math.min(4,maxStage)), shiny:!!sp.shiny };
  cr._sid=dexChoiceSid(sp, cr);
  return cr;
}
// Clash of Clans has only 32 real troops, but the backend Pokédex has 48 species slots.
// Collapse the 48 into 32 unique troops (merging caught/shiny/sessions/output/max level of
// every species that maps to the same troop) so the dex shows each troop exactly once.
function dedupeTroopDex(list){
  function blank(k){ return {species:k,index:k,id:k+1,name:TROOP_LABELS[k],speciesName:TROOP_LABELS[k],
    type:(TROOP_ROLE[TROOP_ARCH[k%8]]||"Troop"),caught:false,shiny:false,maxStage:0,count:0,totalOutput:0,exampleSessionId:null}; }
  var byK={};
  list.forEach(function(sp,i){
    var k=troopSlot(dexCrOf(sp,i)), b=byK[k]||(byK[k]=blank(k));
    if(sp.caught){ b.caught=true; if(sp.maxStage!=null) b.maxStage=Math.max(b.maxStage, sp.maxStage); }
    if(sp.shiny) b.shiny=true;
    b.count += (sp.count!=null?sp.count:(sp.sessions||0));
    b.totalOutput += (sp.totalOutput!=null?sp.totalOutput:(sp.output||sp.outputTokens||0));
    if(!b.exampleSessionId && sp.exampleSessionId) b.exampleSessionId=sp.exampleSessionId;
  });
  var out=[]; for(var k=0;k<TROOP_N;k++) out.push(byK[k]||blank(k));
  return out;
}
function renderPokedex(d){
  d=d||{};
  var list = d.species||d.pokedex||d.entries||(Array.isArray(d)?d:[]);
  var grid=$("dexGrid"); if(!grid) return;
  var isVil = cfg().creaturePack==="village";
  if(isVil && list.length) list = dedupeTroopDex(list);   // 48 species -> 32 unique troops
  var total   = isVil ? TROOP_N : (d.total!=null ? d.total : (list.length||48));
  var caughtN = isVil ? list.filter(function(s){ return s.caught; }).length : (d.caught!=null ? d.caught : list.filter(function(s){ return s.caught; }).length);
  var shinyN  = isVil ? list.filter(function(s){ return s.shiny; }).length : (d.shinyCount!=null ? d.shinyCount : list.filter(function(s){ return s.shiny; }).length);
  $("dexStats").innerHTML = esc(caughtN)+' / '+esc(total)+(isVil?' recruited':' caught')+
    (shinyN ? ' · <span style="color:var(--gold);font-weight:700">✦ '+esc(shinyN)+' shiny</span>' : '');
  if(!list.length){ grid.innerHTML='<div class="feed-empty">No species yet.</div>'; renderDexCoverage([]); renderDexTypeChips([]); return; }

  // annotate entries with derived type/sessions/output for filter+sort
  var rows=list.map(function(sp,i){ var cr=dexCrOf(sp,i);
    return { sp:sp, i:i, cr:cr, caught:!!sp.caught, type:dexTypeOf(sp,cr),
             sessions:dexSessionsOf(sp), output:dexOutputOf(sp),
             num:(sp.id!=null?sp.id:(i+1)) }; });

  renderDexCoverage(rows);
  renderDexTypeChips(rows);

  // filter
  var view=rows.filter(function(r){
    if(DEX_CAUGHT_ONLY && !r.caught) return false;
    if(DEX_TYPE!=="All" && r.type!==DEX_TYPE) return false;
    return true;
  });
  // sort (keep dex-number order as stable base)
  if(DEX_SORT==="sessions") view.sort(function(a,b){ return (b.sessions-a.sessions)||(a.num-b.num); });
  else if(DEX_SORT==="output") view.sort(function(a,b){ return (b.output-a.output)||(a.num-b.num); });
  else view.sort(function(a,b){ return a.num-b.num; });

  grid.innerHTML="";
  if(!view.length){ grid.innerHTML='<div class="feed-empty">No species match this filter.</div>'; return; }
  view.forEach(function(r){ grid.appendChild(buildDexCard(r.sp,r.i,r.cr)); });
  setupRoving(grid, '.dexcard[role="button"]');
}
// Pok\u00e9dex sprite size: big cards, about six per row on a laptop screen, fewer on narrow ones.
var DEX_SPR = 140;
function buildDexCard(sp,i,cr){
    var caught=!!sp.caught;
    var maxStage = creatureStage(cr);  // high-water (monotonic), matches sprite/name
    var card=el("div","dexcard "+(caught?"caught":"uncaught"));
    var num=el("div","dex-num"); num.textContent="#"+String(sp.id!=null?sp.id:(i+1)).padStart(2,"0"); card.appendChild(num);
    if(caught && sp.shiny){ var sh=el("div","dex-shiny"); sh.textContent="✦"; sh.title="Shiny caught"; card.appendChild(sh); }
    var spr=el("div","dex-sprite");
    // caught pokemon -> real sprite; everything else / uncaught -> deterministic silhouette (greyed via CSS)
    if(caught && isPokePack()){ spr.innerHTML=pokeImgHTML(cr, DEX_SPR); }
    else if(caught && cfg().creaturePack==="aniimo"){ spr.innerHTML=animoImgHTML(cr, DEX_SPR); }
    else if(cfg().creaturePack==="village"){ spr.innerHTML=troopImgHTML(cr, DEX_SPR); }
    else { spr.innerHTML=monsterSVG(creatureSeed(cr), creatureTypeHue(cr), Math.max(0,Math.min(4,maxStage)), caught&&!!sp.shiny, DEX_SPR); }
    card.appendChild(spr);
    var nm=el("div","dex-name");
    nm.textContent = !caught ? "???" : (isPokePack() ? pokeCurrentName(cr) : cfg().creaturePack==="aniimo" ? animoLabel(cr) : cfg().creaturePack==="village" ? troopLabel(cr) : (sp.speciesName||sp.name||"Monster"));
    card.appendChild(nm);
    if(caught){ var tb=el("div"); tb.innerHTML=typeBadgeHTML(cr); if(tb.firstChild) card.appendChild(tb.firstChild); }
    var st=el("div","dex-statrow");
    if(caught){
      var sess=(sp.count!=null?sp.count:(sp.sessions!=null?sp.sessions:0));
      st.innerHTML='<span>'+esc(sess)+' session'+(sess===1?'':'s')+'</span>'+
        '<span>max '+esc(STAGE_NAMES[Math.max(0,Math.min(4,maxStage))]||("stage "+maxStage))+'</span>'+
        '<span>'+esc(fmtTok(sp.totalOutput!=null?sp.totalOutput:(sp.output||sp.outputTokens||0)))+' out</span>';
    } else { st.innerHTML='<span>Not yet caught</span>'; }
    card.appendChild(st);
    if(caught && sp.exampleSessionId){
      card.setAttribute("role","button"); card.tabIndex=0;
      var openIt=function(){ openSession(sp.exampleSessionId,{title:sp.species||sp.name,creature:cr}); };
      card.addEventListener("click",openIt);
      card.addEventListener("keydown",function(e){ if(e.key==="Enter"||e.key===" "){ e.preventDefault(); openIt(); } });
    }
    return card;
}
// distinct types covered line: N (caught) / M (dex)
function renderDexCoverage(rows){
  var box=$("dexCoverage"); if(!box) return;
  if(!rows.length){ box.textContent=""; return; }
  var all={}, caught={};
  rows.forEach(function(r){ if(!r.type) return; all[r.type]=1; if(r.caught) caught[r.type]=1; });
  var allT=Object.keys(all).sort(), N=Object.keys(caught).length, M=allT.length;
  var missing=allT.filter(function(t){ return !caught[t]; });
  box.innerHTML='Types: <b>'+N+'</b>/<b>'+M+'</b> covered'+
    (missing.length ? ' <span class="miss">· missing: '+esc(missing.join(", "))+'</span>' : ' <span class="miss">· all types covered ✓</span>');
}
// one chip per type in the dex (+ "All"), coloured by type hue
function renderDexTypeChips(rows){
  var box=$("dexTypeChips"); if(!box) return;
  var types={}; rows.forEach(function(r){ if(r.type) types[r.type]=1; });
  var list=Object.keys(types).sort();
  if(DEX_TYPE!=="All" && !types[DEX_TYPE]) DEX_TYPE="All";
  box.innerHTML="";
  function mk(label,val,hue){
    var b=el("button","chip"+(val!=="All"?" typechip":"")+(DEX_TYPE===val?" active":""));
    b.type="button"; b.textContent=label; b.setAttribute("data-type",val);
    if(val!=="All" && hue!=null){ b.style.background="hsl("+hue+",60%,45%)"; if(DEX_TYPE===val) b.style.boxShadow="0 0 0 2px var(--brand)"; }
    b.addEventListener("click",function(){ DEX_TYPE=val; if(POKEDEX) renderPokedex(POKEDEX); });
    return b;
  }
  box.appendChild(mk("All","All",null));
  list.forEach(function(t){ box.appendChild(mk(t,t,(POKE_TYPE_HUE[t]!=null?POKE_TYPE_HUE[t]:220))); });
}
// wire caught-only + sort controls (once)
(function(){
  var co=$("dexCaughtOnly"); if(co){ co.checked=DEX_CAUGHT_ONLY;
    co.addEventListener("change",function(){ DEX_CAUGHT_ONLY=co.checked;
      localStorage.setItem("hq_dex_caught",DEX_CAUGHT_ONLY?"1":"0"); if(POKEDEX) renderPokedex(POKEDEX); }); }
  var so=$("dexSort"); if(so){ so.value=DEX_SORT;
    so.addEventListener("change",function(){ DEX_SORT=so.value;
      localStorage.setItem("hq_dex_sort",DEX_SORT); if(POKEDEX) renderPokedex(POKEDEX); }); }
})();
// refresh pokedex while its view is open
pokedexTimer=setInterval(function(){ if(VIEW==="pokedex" && !document.hidden) loadPokedex(); }, 30000);

