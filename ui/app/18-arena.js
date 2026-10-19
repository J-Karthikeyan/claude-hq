/* ---- Arena (multiplayer leaderboard) ---------------------------------- */

function arenaPost(path, body){
  return fetch(path,{
    method:"POST",
    headers:{"Content-Type":"application/json","X-HQ-Token":CSRF},
    body:JSON.stringify(body||{})
  }).then(function(r){
    return r.json().then(function(j){return {ok:r.ok,status:r.status,j:j};},function(){return {ok:r.ok,status:r.status,j:{}};});
  });
}

function arenaSay(text, kind){
  var m=$("arenaMsg"); if(!m) return;
  m.textContent = text||"";
  m.className = "arena-msg" + (text?"":" hidden") + (kind?(" "+kind):"");
}

function arenaNum(n){
  if(n>=1e9) return (n/1e9).toFixed(1)+"B";
  if(n>=1e6) return (n/1e6).toFixed(1)+"M";
  if(n>=1e3) return (n/1e3).toFixed(1)+"k";
  return String(n||0);
}

// Header Arena dot: green publishing, red failed, amber connected-but-stale.
function renderArenaStat(st){
  var b=$("arenaStat"); if(!b) return;
  var show = !!(st && st.paired && st.enabled);
  b.classList.toggle("hidden", !show);
  if(!show) return;
  var lp = st.lastPublish||{}, cls="wait", tip="Arena: connected";
  if(lp.ok===false){ cls="err"; tip="Arena: last publish failed"+(lp.error?" \u2014 "+lp.error:""); }
  else if(!lp.at){ cls="wait"; tip="Arena: connected, not published yet"; }
  else {
    var age = Date.now() - new Date(lp.at).getTime();
    cls = age > 20*60*1000 ? "wait" : "ok";
    tip = "Arena: "+(cls==="ok"?"published ":"last published ")+relTime(lp.at);
  }
  $("arenaStatDot").className = "arena-stat-dot "+cls;
  b.title = tip; b.setAttribute("aria-label", tip);
}
function pollArenaStat(){
  fetch("/api/arena/status",{cache:"no-store"}).then(function(r){return r.json();})
    .then(function(st){ renderArenaStat(st); arenaStayOnline(st); pantryTick(); }).catch(function(){});
}
(function(){ var b=$("arenaStat"); if(b) b.addEventListener("click",function(){ setView("arena"); }); })();
pollArenaStat(); setInterval(function(){ if(!document.hidden) pollArenaStat(); }, 60000);

function loadArena(){
  fetch("/api/arena/status",{cache:"no-store"}).then(function(r){return r.json();})
    .then(function(st){
      ARENA.status = st;
      renderArenaStat(st);
      var paired = !!st.paired;
      ARENA.paired = paired;
      $("arenaSetup").classList.toggle("hidden", paired);
      $("arenaBoardWrap").classList.toggle("hidden", !paired);
            $("arenaLobbyWrap").classList.toggle("hidden", !paired);
      $("arenaChatWrap").classList.toggle("hidden", !paired);
      $("arenaVoiceWrap").classList.toggle("hidden", !paired);
      $("arenaFoot").classList.toggle("hidden", !paired);
      if(!paired){
        $("arenaSub").textContent = "Not connected";
        var u=$("arenaUrl"); if(u && !u.value) u.value = st.url||"";
        pantryLoad(true);   // -> "unpaired": snack buttons hide, the drawer explains how to get coins
        return;
      }
      $("arenaSub").textContent = "Signed in as " + (st.handle||"?");
      var lp = st.lastPublish||{};
      $("arenaLast").textContent = lp.at
        ? (lp.ok ? (" · last published " + new Date(lp.at).toLocaleTimeString())
                 : (" · publish failed: " + (lp.error||"")))
        : " · not published yet";
      arenaLoadBoard();
      arenaRoomsLoad(); arenaRenderRoomBar();
      pantryLoad(true);
      arenaRenderChat();
      arenaOpenSocket();
    }).catch(function(){ $("arenaSub").textContent = "Status unavailable"; });
}

// Nudge someone from the leaderboard (by handle). Server persists it, so it
// reaches them even with their Arena tab closed.
//
// An Arena server older than offline nudges has no /v1/nudge, and FastAPI
// answers that with a bare 404 "Not Found" (a missing *person* is "no such
// person"). Then someone who is in the lobby right now is nudged over the lobby
// socket instead -- exactly what the lobby's own button does -- and anyone else
// gets a message saying why, rather than a silent "nudge failed".
function arenaNudgeHandle(handle, btn){
  if(btn) btn.disabled = true;
  arenaPost("/api/arena/nudge", {toHandle: handle}).then(function(res){
    if(btn) btn.disabled = false;
    if(res.ok){
      toast(res.j.deliveredLive ? "\uD83D\uDC4B Nudge delivered" : "\uD83D\uDC4B Nudge queued \u2014 they\u2019ll get it when Claude HQ is running", "level");
      return;
    }
    var reason = String((res.j && (res.j.error || res.j.detail)) || "nudge failed");
    if(res.status===404 && /^not found$/i.test(reason)){
      var peer = (ARENA.lobby||[]).filter(function(m){ return m.handle===handle; })[0];
      if(peer){ arenaSendNudge(peer); return; }
      toast("\u26A0 This Arena server can\u2019t queue nudges yet \u2014 ask whoever hosts it to update it. People in this room can still be nudged.", "ach");
      return;
    }
    toast("\u26A0 "+reason, "ach");
  }).catch(function(){ if(btn) btn.disabled=false; });
}
function arenaLoadBoard(){
  if(ARENA.mode==="accounts"){ arenaLoadAccounts(); return; }
  arenaModeNote("");
  var wh=$("arenaWhoH"); if(wh) wh.textContent="Trainer";
  fetch("/api/arena/board?window="+encodeURIComponent(ARENA.window),{cache:"no-store"})
    .then(function(r){return r.json();})
    .then(function(b){
      var body=$("arenaRows"); if(!body) return;
      if(b.error){
        body.innerHTML = '<tr><td colspan="10" class="muted"></td></tr>';
        body.querySelector("td").textContent = b.error;
        return;
      }
      $("arenaSeason").textContent = (b.seasonName||"") + " · " + b.startsOn + " → " + b.endsOn;
      var rows = b.entries||[];
      if(!rows.length){
        body.innerHTML = '<tr><td colspan="10" class="muted">No one has published stats yet.</td></tr>';
        return;
      }
      body.innerHTML = "";
      rows.forEach(function(e){
        var tr = document.createElement("tr");
        if(e.isYou) tr.className = "you";

        var rk = document.createElement("td");
        rk.className = "arena-rank" + (e.rank<=3 ? (" r"+e.rank) : "");
        rk.textContent = e.rank<=3 ? ["🥇","🥈","🥉"][e.rank-1] : String(e.rank);
        tr.appendChild(rk);

        var who = document.createElement("td");
        var wrap = document.createElement("div"); wrap.className = "arena-who";
        if(e.avatarUrl){
          var img = document.createElement("img");
          img.className="arena-av"; img.src=e.avatarUrl; img.alt=""; img.loading="lazy";
          wrap.appendChild(img);
        }
        var nm = document.createElement("div"); nm.className="arena-name";
        nm.textContent = e.trainerName || e.displayName || e.handle;
        var sub = document.createElement("small");
        sub.textContent = "@" + e.handle + " · " + e.rankTitle;
        nm.appendChild(sub); wrap.appendChild(nm);
        if(!e.isYou){
          var nb=document.createElement("button"); nb.className="arena-nudge"; nb.type="button";
          nb.textContent="\uD83D\uDC4B"; nb.title="Nudge "+(e.trainerName||e.displayName||e.handle)+" to come to the Arena";
          nb.addEventListener("click", function(){ arenaNudgeHandle(e.handle, nb); });
          wrap.appendChild(nb);
          wrap.appendChild(arenaGiftButton(e.handle, e.trainerName || e.displayName || e.handle, "\uD83C\uDF81"));
        }
        who.appendChild(wrap); tr.appendChild(who);

        [arenaNum(e.xp), e.level, arenaNum(e.prompts), arenaNum(e.tools),
         e.artifacts, e.activeDays, (e.streak ? e.streak+"🔥" : "0"),
         arenaNum(e.tokensTotal)].forEach(function(v){
          var td = document.createElement("td"); td.textContent = v; tr.appendChild(td);
        });
        body.appendChild(tr);
      });
      arenaGiftButtonsSync();
    }).catch(function(){});
}

/* ---- Standings by account ----
   People signed into the same Claude account (opt-in: Settings > Account board) are one row:
   their combined numbers for the window, expandable to each member's share. The Arena sends only
   public handles and counts; the account key itself never reaches this page. */
var ACCT_OPEN = {};   // account row key (its members' handles) -> expanded
function arenaModeNote(t){ var n=$("arenaModeNote"); if(!n) return; n.textContent=t||""; n.classList.toggle("hidden", !t); }
function arenaSetMode(m){
  ARENA.mode = m==="accounts" ? "accounts" : "players";
  try{ localStorage.setItem("hq_arena_mode", ARENA.mode); }catch(e){}
  Array.prototype.forEach.call(document.querySelectorAll("#arenaMode .arena-win"), function(o){
    var on = o.getAttribute("data-mode")===ARENA.mode;
    o.classList.toggle("on", on); o.setAttribute("aria-pressed", on ? "true" : "false");
  });
}
function acctName(a){
  var ms=(a && a.members) || [];
  var names=ms.slice(0,3).map(function(m){ return m.trainerName || m.displayName || m.handle; });
  var more=Math.max(0, (a.memberCount|0) - names.length);
  return names.join(", ") + (more ? " +"+more : "");
}
function acctWho(img, title, sub){
  var wrap=document.createElement("div"); wrap.className="arena-who";
  if(img) wrap.appendChild(img);
  var nm=document.createElement("div"); nm.className="arena-name"; nm.textContent=title;
  var sm=document.createElement("small"); sm.textContent=sub; nm.appendChild(sm); wrap.appendChild(nm);
  return wrap;
}
function acctAvatar(url){
  if(!url) return null;
  var i=document.createElement("img"); i.className="arena-av"; i.src=url; i.alt=""; i.loading="lazy";
  i.addEventListener("error", function(){ i.remove(); });
  return i;
}
// Plain numbers, not |0: token totals run into the billions, past int32.
function acctN(v){ v=Number(v); return isFinite(v) && v>0 ? Math.floor(v) : 0; }
function acctNums(tr, e){
  [arenaNum(acctN(e.xp)), acctN(e.level), arenaNum(acctN(e.prompts)), arenaNum(acctN(e.tools)), acctN(e.artifacts), acctN(e.activeDays),
   (acctN(e.streak) ? acctN(e.streak)+"🔥" : "0"), arenaNum(acctN(e.tokensTotal))].forEach(function(v){
    var td=document.createElement("td"); td.textContent=String(v); tr.appendChild(td);
  });
}
function arenaLoadAccounts(){
  var wh=$("arenaWhoH"); if(wh) wh.textContent="Account";
  var win=ARENA.window;
  fetch("/api/arena/accounts/standings?window="+encodeURIComponent(win),{cache:"no-store"})
    .then(function(r){ return r.json().then(function(j){ return {ok:r.ok, status:r.status, j:j}; }, function(){ return {ok:false, status:r.status, j:{}}; }); })
    .then(function(res){
      if(ARENA.mode!=="accounts" || ARENA.window!==win) return;   // the user switched while this was in flight
      var body=$("arenaRows"); if(!body) return;
      body.textContent="";
      function emptyRow(t){ var tr=document.createElement("tr"), td=document.createElement("td"); td.colSpan=10; td.className="muted"; td.textContent=t; tr.appendChild(td); body.appendChild(tr); }
      if(!res.ok){
        arenaModeNote("");
        emptyRow(res.status===404 ? "This Arena doesn't have account standings yet: ask its owner to update it." : String(res.j.error || res.j.detail || "Couldn't load account standings."));
        return;
      }
      var b=res.j;
      $("arenaSeason").textContent = (b.seasonName||"") + " · " + b.startsOn + " → " + b.endsOn;
      var on=(typeof CONFIG!=="undefined" && CONFIG && CONFIG.arenaAccountBoard===true);
      var list=b.accounts||[];
      var mine=list.some(function(a){ return a.isMine; });
      arenaModeNote(!on ? "You're not in account standings: turn on Settings › Account board, then Publish now."
        : (!mine ? "Account board is on. You'll appear once someone else signed into your Claude account turns it on and publishes." : ""));
      if(!list.length){ emptyRow("No accounts yet: an account shows once two people on it have turned on Account board and published."); return; }
      list.forEach(function(a){
        var key=(a.members||[]).map(function(m){ return m.handle; }).sort().join(",");
        var open=!!ACCT_OPEN[key];
        var tr=document.createElement("tr"); if(a.isMine) tr.className="you";
        var rk=document.createElement("td"); rk.className="arena-rank"+(a.rank<=3 ? (" r"+a.rank) : "");
        rk.textContent = a.rank<=3 ? ["🥇","🥈","🥉"][a.rank-1] : String(a.rank); tr.appendChild(rk);
        var who=document.createElement("td");
        var avs=document.createElement("div"); avs.className="acct-avs"; avs.setAttribute("aria-hidden","true");
        (a.members||[]).slice(0,3).forEach(function(m){ var i=acctAvatar(m.avatarUrl); if(i) avs.appendChild(i); });
        var w=acctWho(avs, acctName(a), (a.memberCount|0)+" people"+(a.isMine ? " · your account" : "")
          +(a.partial ? " · totals cover the "+((a.members||[]).length)+" listed" : ""));
        var tg=document.createElement("button"); tg.type="button"; tg.className="acct-toggle";
        tg.setAttribute("aria-expanded", open ? "true" : "false");
        tg.setAttribute("aria-label", (open ? "Hide" : "Show")+" each member's share for "+acctName(a));
        tg.textContent = open ? "▾" : "▸";
        w.insertBefore(tg, w.firstChild);
        who.appendChild(w); tr.appendChild(who);
        acctNums(tr, a);
        body.appendChild(tr);
        var rows=[];
        (a.members||[]).forEach(function(m){
          var mr=document.createElement("tr"); mr.className="acct-member"+(m.isYou ? " you" : ""); mr.hidden=!open;
          var c0=document.createElement("td"); c0.textContent=""; mr.appendChild(c0);
          var c1=document.createElement("td");
          var mw=acctWho(acctAvatar(m.avatarUrl), m.trainerName || m.displayName || m.handle,
                         "@"+m.handle+" · "+(m.share|0)+"% of this account's XP");
          var bar=document.createElement("span"); bar.className="acct-share"; bar.setAttribute("aria-hidden","true");
          var fill=document.createElement("span"); fill.style.width=Math.max(0, Math.min(100, m.share|0))+"%"; bar.appendChild(fill);
          mw.querySelector(".arena-name").appendChild(bar);
          c1.appendChild(mw); mr.appendChild(c1);
          acctNums(mr, m);
          body.appendChild(mr); rows.push(mr);
        });
        tg.addEventListener("click", function(){
          var now=!ACCT_OPEN[key]; ACCT_OPEN[key]=now;
          rows.forEach(function(r){ r.hidden=!now; });
          tg.setAttribute("aria-expanded", now ? "true" : "false");
          tg.setAttribute("aria-label", (now ? "Hide" : "Show")+" each member's share for "+acctName(a));
          tg.textContent = now ? "▾" : "▸";
          if(typeof announce==="function") announce(now ? "Showing "+(a.memberCount|0)+" members" : "Members hidden");
        });
      });
    }).catch(function(){});
}

/* ---- Arena pantry: Poke Coins, snacks and gifts ----
   All of it lives on the Arena server under your GitHub account (every paired device shares one
   wallet); the page reaches it only through dashboard.py's /api/arena/pantry proxy, which forwards
   an allowlist of fields (kind, qty, coins, recipient, note, requestId) and never a session.
   A server without these routes answers a bare 404: then it's "not supported yet", re-checked
   every 10 minutes, and rest alone still restores every creature. */
var PANTRY_STALE_MS=60000, PANTRY_REFRESH_MS=300000, PANTRY_REPROBE_MS=600000;
function pantryValid(j){ return !!(j && typeof j==="object" && typeof j.coins==="number" && j.items && typeof j.items==="object" && j.claim && typeof j.claim==="object"); }
function lsGet(k){ try{ return localStorage.getItem(k); }catch(e){ return null; } }
function lsSet(k,v){ try{ localStorage.setItem(k,v); }catch(e){} }
// Everything that shows coins or snacks, redrawn together.
function pantryChanged(){
  renderStore(); refreshDrawerCare(); renderTrainerCard(); arenaGiftButtonsSync();
  if(GIFT.handle) giftFill(false);
  if(STATE) renderParty(STATE.sessions);   // the card Snack buttons (partySig-guarded, so usually a no-op)
}
// Why an open gift dialog had to close: the store is gone, not just unreachable for a moment.
// ("reload" keeps it open: the dialog's own Send turns into "Reload page", see giftUpdate.)
function pantryGoneMsg(st){
  return st==="unsupported" ? "this Arena server doesn\u2019t have the store yet"
       : st==="unpaired" ? "the Arena isn\u2019t connected"
       : st==="restart" ? "restart Claude HQ, then reload this page, to finish updating it" : "";
}
function pantrySetStore(st){
  if(PANTRY.stale && (st==="ok" || st==="error")) st="reload";   // nothing works here until a reload
  if(st==="unsupported") PANTRY.offAt=Date.now();
  if(st==="unpaired"){ PANTRY.j=null; PANTRY.at=0; }
  if(PANTRY.store===st) return;
  PANTRY.store=st; PANTRY.rev++;
  // A transient "error" (a background reload that failed) leaves an open gift alone: its own Send
  // says what happened, and closing it would drop the form of a gift that may still be unconfirmed.
  if(pantryGoneMsg(st) && GIFT.handle){ giftClose(); announce("Gift closed: "+pantryGoneMsg(st)); }
  pantryChanged();
}
// dashboard.py refused this page's token (pantryLocalReject): every POST from here will be refused
// the same way, so the store says "reload" until one happens. GETs still work, but loading and
// retrying stop: nothing could be done with them.
function pantryStale(){ PANTRY.stale=true; pantrySetStore("reload"); }
var PANTRY_RELOAD_MSG="Claude HQ restarted since this page loaded. Reload the page to use the store.";
// Balance-bearing responses (the GET and every successful claim/buy/eat/give) land here.
function pantryApply(j){
  if(!pantryValid(j)) return;
  if(PANTRY.j && (j.coins|0)>(PANTRY.j.coins|0)) PANTRY.bump=true;
  PANTRY.j=j; PANTRY.at=Date.now(); PANTRY.rev++; PANTRY.store=PANTRY.stale ? "reload" : "ok";
  pantryChanged();
}
function pantryLoad(force){
  if(!ARENA.paired){ PANTRY.retrying=false; pantrySetStore("unpaired"); return; }
  var now=Date.now();
  if(PANTRY.loading) return;
  if(!force && PANTRY.store==="unsupported" && now-PANTRY.offAt<PANTRY_REPROBE_MS) return;
  if(!force && PANTRY.store==="ok" && now-PANTRY.at<PANTRY_STALE_MS) return;
  if(!force && PANTRY.store==="reload") return;
  PANTRY.loading=true;
  if(!PANTRY.j) renderStore();
  fetch("/api/arena/pantry",{cache:"no-store"}).then(function(r){
    return r.json().then(function(j){ return {ok:r.ok,status:r.status,j:j}; }, function(){ return {ok:r.ok,status:r.status,j:{}}; });
  }).then(function(res){
    PANTRY.loading=false;
    if(res.status===200 && pantryValid(res.j)){ PANTRY.retrying=false; pantryApply(res.j); pantryAutoClaim(); pantryRetryPending(); return; }
    var miss=pantryMissState(res);
    pantryLoadFailed(miss || (res.status===400 ? "unpaired" : "error"));
  }, function(){ PANTRY.loading=false; pantryLoadFailed("error"); });
}
// A load that came back empty-handed. After a Retry click it's said out loud even when nothing
// changed (still "error": pantrySetStore redraws and announces nothing then), and the store's
// "Retrying..." goes back to "Retry".
function pantryLoadFailed(st){
  var asked=PANTRY.retrying; PANTRY.retrying=false;
  pantrySetStore(st);
  if(asked){ renderStore(); if(PANTRY.store==="error") announce("Store still unavailable: can\u2019t reach the Arena server."); }
  refreshDrawerCare();
}
// The Retry buttons (store panel, drawer): a forced load that reports its outcome. A click while a
// load is already out waits for that one.
function pantryRetry(){ if(PANTRY.retrying) return; PANTRY.retrying=true; renderStore(); pantryLoad(true); }
// The minutely status poll: refresh when there's nothing yet, it's 5 min old, or a new day's
// coins are due. Otherwise just keep the store's countdown fresh (that line only: rebuilding the
// shelf would move focus off a Buy button and make a screen reader read it again).
function pantryTick(){
  if(!ARENA.paired){ pantrySetStore("unpaired"); return; }
  var j=PANTRY.j, now=Date.now(), next=(j && j.claim) ? Date.parse(j.claim.nextClaimAt) : NaN;
  if(!j || PANTRY.store!=="ok" || now-PANTRY.at>PANTRY_REFRESH_MS || (!isNaN(next) && now>=next)) pantryLoad();
  else if(VIEW==="store") svTick();
}
// "For coming online": claim today's coins once, automatically. A 200 or 4xx settles today for
// this browser; a network failure leaves it for the next load. Only the tab whose request
// granted the coins says so (the server answers the others claimed:false).
function pantryAutoClaim(){
  var c=PANTRY.j && PANTRY.j.claim;
  if(!c || !c.claimable || PANTRY.claiming || PANTRY.stale || lsGet("hq_coin_claim_try")===c.today) return;
  pantryClaim(false);
}
function pantryClaim(manual){
  if(PANTRY.claiming || !PANTRY.j) return;
  var today=PANTRY.j.claim && PANTRY.j.claim.today;
  PANTRY.claiming=true; renderStore();
  arenaPost("/api/arena/pantry/claim", {}).then(function(res){
    PANTRY.claiming=false;
    // Turned away by dashboard.py itself: no claim was made, so today stays open for the next try.
    if(pantryLocalReject(res)){
      pantryStale();
      if(manual) toast("\u26a0 Claude HQ restarted since this page loaded. Reload the page to collect today\u2019s coins.", "ach");
      return;
    }
    var miss=pantryMissState(res);   // never reached a claim handler: nothing settled for today
    if(res.status>=200 && res.status<500 && !miss && typeof today==="string") lsSet("hq_coin_claim_try", today);
    if(res.ok && pantryValid(res.j)){
      var got=res.j, n=got.granted|0;
      pantryApply(got);
      if(got.claimed && n>0){
        var msg="\uD83E\uDE99 +"+n+" Poke Coin"+(n===1?"":"s")+" for showing up today"+(got.starter?" \u00B7 and a free \uD83C\uDF59 Rice Ball to get you started":"");
        PANTRY.bump=true; renderTrainerCard();
        toast(msg,"level"); announce(msg);
        logEvent("\uD83E\uDE99", "Collected "+coinsN(n)+(got.starter?" and a free Rice Ball":""));
      }
      return;
    }
    if(miss){ pantrySetStore(miss); return; }
    if(manual) toast("\u26A0 "+pantryErr(res, "Couldn\u2019t collect today\u2019s coins"), "ach");
    renderStore();
  }, function(){ PANTRY.claiming=false; if(manual) toast("\u26A0 Couldn\u2019t reach the Arena", "ach"); renderStore(); });
}
/* Requests whose answer was lost (no reply, or a 5xx: the Arena may have applied them or not).
   Each is kept in a localStorage map {key: {rid, t, ...}} until a definitive answer, so the retry
   replays the SAME requestId (even after a reload, or after closing and reopening a dialog) and
   the server can't apply it twice. Entries are dropped after a day. mem: this tab's copy, used
   when localStorage is unavailable. */
var RETRY_TTL=86400000;
function retryMapLoad(name, mem, keyOk){
  var src=mem||{}, out={}, now=Date.now();
  try{ var raw=JSON.parse(localStorage.getItem(name)||"{}"); if(raw && typeof raw==="object" && !Array.isArray(raw)) src=raw; }catch(e){}
  Object.keys(src).forEach(function(k){
    var p=src[k];
    if(keyOk(k, p) && p && typeof p.rid==="string" && /^[A-Za-z0-9_-]{16,64}$/.test(p.rid) &&
       typeof p.t==="number" && now-p.t>=0 && now-p.t<=RETRY_TTL) out[k]=p;
  });
  return out;
}
function retryMapSave(name, map){ try{ localStorage.setItem(name, JSON.stringify(map)); }catch(e){} }
function pantryUnsure(res){ return res.status===0 || res.status>=500; }
// A lost buy, per food kind (localStorage "hq_buy_retry"): its requestId and quantity, so a retry
// replays exactly the same purchase.
var STORE_RETRY={};
function storeRetryAll(){ return retryMapLoad("hq_buy_retry", STORE_RETRY, function(k){ return isFood(k); }); }
function storeRetryGet(kind){ var p=storeRetryAll()[kind]; return p ? {rid:p.rid, qty:Math.max(1, Math.min(5, (p.qty|0)||1))} : null; }
function storeRetryRid(kind){ var p=storeRetryGet(kind); return p ? p.rid : null; }
function storeRetrySet(kind, rid, qty){
  var m=storeRetryAll();
  if(rid) m[kind]={rid:rid, qty:Math.max(1, Math.min(5, (qty|0)||1)), t:(m[kind] && m[kind].rid===rid) ? m[kind].t : Date.now()};
  else delete m[kind];
  STORE_RETRY=m; retryMapSave("hq_buy_retry", m);
}
// Buy qty (1-5) of one snack. {ok, replayed, qty} | {ok:false, lost:true, rid} (couldn't confirm:
// retry with the SAME rid, which replays the saved quantity) | {ok:false, stale:true} (refused by
// dashboard.py itself: nothing sent) | {ok:false, msg}. replayed: that rid had already bought it
// earlier (a lost buy that had landed), so nothing was bought now; the balances in the answer are
// today's. quiet: the caller reports errors itself (no toast).
function pantryBuy(kind, rid, qty, quiet){
  var saved=storeRetryGet(kind);
  if(rid && saved && saved.rid===rid) qty=saved.qty;
  qty=Math.max(1, Math.min(5, (qty|0)||1));
  rid = rid || newRequestId();
  return arenaPost("/api/arena/pantry/buy", {requestId:rid, kind:kind, qty:qty}).then(function(res){
    if(res.ok && pantryValid(res.j)){ storeRetrySet(kind, null); pantryApply(res.j); return {ok:true, replayed:res.j.replayed===true, qty:(res.j.qty|0)||qty}; }
    if(pantryUnsure(res)){ storeRetrySet(kind, rid, qty); renderStore(); return {ok:false, lost:true, rid:rid}; }
    if(pantryLocalReject(res)){ pantryStale(); return {ok:false, stale:true}; }   // a lost buy stays saved
    storeRetrySet(kind, null);
    var miss=pantryMissState(res);
    if(miss){ pantrySetStore(miss); return {ok:false}; }
    var err=pantryErr(res, "Couldn’t buy that");
    if(res.status===400 && /not paired/i.test(err)){ pantrySetStore("unpaired"); return {ok:false}; }
    if(!quiet) toast("⚠ "+err, "ach");
    return {ok:false, msg:err};
  }, function(){ storeRetrySet(kind, rid, qty); renderStore(); return {ok:false, lost:true, rid:rid}; });
}
function focusQuiet(n){ if(!n) return; try{ n.focus({preventScroll:true}); }catch(e){ n.focus(); } }

/* ---- gifts: coins and/or one snack kind, to anyone on the board or in the lobby ----
   GIFT.body: the exact request of a gift that has no answer yet (in flight, or lost). While the
   dialog shows one (unsure), the form is locked and "Send again" replays that body as is, whatever
   the pantry shows now. GIFT.zone: where the dialog was opened ("arenaLobby" / "arenaRows"). */
var GIFT={handle:null, name:"", rid:null, busy:false, unsure:false, body:null, zone:null};
// label: "\uD83C\uDF81" (standings rows: icon-only, so aria-label names it) or "\uD83C\uDF81 gift" (lobby chips).
function arenaGiftButton(handle, name, label){
  var b=document.createElement("button"); b.className="arena-nudge arena-gift"; b.type="button";
  b.textContent=label; b.setAttribute("data-who", String(name||handle||"").slice(0,40));
  b.setAttribute("data-handle", String(handle||""));   // so focus can find this person's button again
  if(/[A-Za-z]/.test(label)) b.setAttribute("data-worded","1");
  b.addEventListener("click", function(){ openGift(handle, name, b); });
  arenaGiftButtonSync(b);
  return b;
}
// saved: giftUnsureAll(), when syncing many. An unconfirmed gift to this person keeps the button
// usable even with nothing left to give (that gift may be what emptied the pantry).
function arenaGiftButtonSync(b, saved){
  var j=PANTRY.j, ok=PANTRY.store==="ok" && !!j;
  var has=ok && ((j.coins|0)>0 || FOOD_ORDER.some(function(k){ return j.items && (j.items[k]|0)>0; }) ||
                 !!(saved || giftUnsureAll())[b.getAttribute("data-handle")||""]);
  var who=b.getAttribute("data-who")||"", none=ok && !has;
  b.classList.toggle("hidden", !ok);
  b.disabled = none;
  b.title = none ? "Nothing to give yet \u00B7 you get 5 Poke Coins a day" : "Give Poke Coins or a snack to "+who;
  // A worded button's spoken name starts with the word it shows ("click gift" in voice control).
  b.setAttribute("aria-label", b.getAttribute("data-worded")!=="1" ? b.title
    : (none ? "Gift: nothing to give yet \u00B7 you get 5 Poke Coins a day" : "Gift "+who+" Poke Coins or a snack"));
}
// Gift buttons show only when the store works; with nothing to give they explain why.
function arenaGiftButtonsSync(){
  var saved=giftUnsureAll();
  Array.prototype.forEach.call(document.querySelectorAll(".arena-gift"), function(b){ arenaGiftButtonSync(b, saved); });
}
/* A gift's requestId outlives the dialog. The gift is saved per recipient (localStorage
   "hq_gift_unsure") BEFORE its request goes out and cleared only by a definitive answer (a 200 or
   a 4xx). Reopening the dialog for that person while it's saved brings the same gift back, locked,
   with the same id: closing the dialog, a reload, or a pantry reload that fails meanwhile can't
   turn "Send again" into a second gift. */
function giftBodyOk(b, handle, rid){
  return !!(b && typeof b==="object" && b.requestId===rid && b.toHandle===handle &&
    typeof b.coins==="number" && b.coins%1===0 && b.coins>=0 && b.coins<=5 &&
    typeof b.qty==="number" && b.qty%1===0 && b.qty>=0 && b.qty<=3 &&
    (b.kind==null ? b.qty===0 : (isFood(b.kind) && b.qty>0)) && b.coins+b.qty>0 &&
    typeof b.note==="string" && b.note.length<=80);
}
var GIFT_UNSURE={};
function giftUnsureAll(){
  return retryMapLoad("hq_gift_unsure", GIFT_UNSURE, function(k, p){
    return /^[A-Za-z0-9_-]{1,64}$/.test(k) && !!p && typeof p==="object" && giftBodyOk(p.body, k, p.rid); });
}
function giftUnsureFor(handle){ return giftUnsureAll()[handle] || null; }
// body null: settled, forget it.
function giftUnsureSet(handle, body){
  var m=giftUnsureAll();
  if(body){ var p=m[handle]; m[handle]={rid:body.requestId, t:(p && p.rid===body.requestId) ? p.t : Date.now(), body:body}; }
  else delete m[handle];
  GIFT_UNSURE=m; retryMapSave("hq_gift_unsure", m);
}
function openGift(handle, name, btn){
  if(PANTRY.store!=="ok" || !PANTRY.j || typeof handle!=="string" || !/^[A-Za-z0-9_-]{1,64}$/.test(handle)) return;
  rememberOpener();
  var zone=(btn && btn.closest) ? btn.closest("#arenaLobby,#arenaRows") : null, u=giftUnsureFor(handle), b=u ? u.body : null;
  GIFT.handle=handle; GIFT.name=String(name||handle).slice(0,40); GIFT.zone=zone ? zone.id : null; GIFT.busy=false;
  GIFT.unsure=!!b; GIFT.body=b; GIFT.rid=b ? u.rid : newRequestId();
  $("giftTitle").textContent="\uD83C\uDF81 Give to "+GIFT.name;
  $("giftCoins").value=String(b ? b.coins : 0); $("giftQty").value=String(b ? b.qty : 0); $("giftNote").value=b ? b.note : "";
  $("giftErr").textContent = b ? "Your last gift to "+GIFT.name+" wasn\u2019t confirmed. Send again to finish it (safe, won\u2019t send twice)" : "";
  giftFill(true);
  $("giftBack").classList.add("open");
  (b ? $("giftSend") : $("giftCoins")).focus();
}
function giftClose(){
  var h=GIFT.handle, zone=GIFT.zone, wasOpen=$("giftBack").classList.contains("open");
  $("giftBack").classList.remove("open");
  GIFT.handle=null; GIFT.busy=false; GIFT.unsure=false; GIFT.body=null; GIFT.zone=null;
  if(wasOpen) giftReturnFocus(h, zone);
}
// Focus goes back to the button that opened the dialog. A lobby join/leave or status update and a
// standings reload rebuild those buttons, and giving away your last coins disables them, so
// failing that: this person's gift button as it is now, else their nudge button, else the section.
function giftFocusable(n){ return !!(n && n.focus && document.contains(n) && !n.disabled && !(n.closest && n.closest(".hidden"))); }
function giftReturnFocus(handle, zone){
  if(!handle || giftFocusable(MODAL_RETURN)){ restoreOpener(); return; }
  MODAL_RETURN=null;
  var pick=null;
  [zone, "arenaLobby", "arenaRows"].some(function(z){
    var root=z ? $(z) : null; if(!root) return false;
    var bs=root.querySelectorAll(".arena-gift");
    for(var i=0;i<bs.length && !pick;i++){
      if(bs[i].getAttribute("data-handle")!==handle) continue;
      if(giftFocusable(bs[i])) pick=bs[i];
      else { var nb=bs[i].parentNode ? bs[i].parentNode.querySelector(".arena-nudge:not(.arena-gift)") : null; if(giftFocusable(nb)) pick=nb; }
    }
    return !!pick;
  });
  if(!pick){ var sec=$(zone==="arenaRows" ? "arenaBoardWrap" : "arenaLobbyWrap"); if(giftFocusable(sec)) pick=sec; }
  if(pick){ try{ pick.focus(); }catch(e){} }
}
// (Re)build the snack list from what you own, keeping the current pick when you still have it. A
// locked (unconfirmed) gift keeps its own kind listed even at 0 owned: that gift may have taken it.
function giftFill(reset){
  var j=PANTRY.j||{}, items=j.items||{}, sel=$("giftKind"), lock=GIFT.unsure && GIFT.body;
  var cur=lock ? (GIFT.body.kind||"") : (reset ? "" : sel.value);
  sel.innerHTML="";
  var o0=el("option"); o0.value=""; o0.textContent="No snack"; sel.appendChild(o0);
  pantryCatalog().forEach(function(it){
    var own=items[it.kind]|0; if(own<=0 && !(lock && it.kind===cur)) return;
    var o=el("option"); o.value=it.kind; o.textContent=it.emoji+" "+it.name+(own>0 ? " \u00D7"+own : ""); sel.appendChild(o);
  });
  sel.value=cur; if(sel.value!==cur) sel.value="";
  giftUpdate();
}
function giftVals(){
  var j=PANTRY.j||{}, items=j.items||{}, lim=j.limits||{};
  var c=parseInt($("giftCoins").value,10), q=parseInt($("giftQty").value,10), k=$("giftKind").value;
  if(!isFood(k)) k="";
  var capC=(lim.giftMaxCoins|0)||5, capQ=(lim.giftMaxQty|0)||3;
  return {coins:(c>=0?c:0), kind:k, qty:(k && q>=0 ? q : 0), note:$("giftNote").value.replace(/\s+/g," ").trim().slice(0,80),
          have:j.coins|0, capC:capC, maxC:Math.min(capC, j.coins|0), maxQ:k ? Math.min(capQ, items[k]|0) : 0,
          left:(typeof lim.giftsLeftToday==="number") ? lim.giftsLeftToday : null, bad:!(c>=0) || !!(k && !(q>=0))};
}
// "2 Poke Coins + 1 Berry" (sep " + ") or "2 Poke Coins and a Berry" (sep " and ", article).
function giftWhat(coins, kind, qty, sep, article){
  var parts=[], it=foodInfo(kind);
  if(coins>0) parts.push(coinsN(coins));
  if(it && qty>0) parts.push(article && qty===1 ? foodA(it) : foodN(it, qty));
  return parts.join(sep);
}
function giftUpdate(){
  var v=giftVals(), err="";
  $("giftCoins").max=String(v.maxC); $("giftQty").max=String(v.maxQ);
  $("giftQty").disabled=!v.kind || GIFT.unsure;
  $("giftCoins").disabled=$("giftKind").disabled=$("giftNote").disabled=GIFT.unsure;
  $("giftCoinsHint").textContent="You have "+coinsN(v.have)+" \u00B7 up to "+v.capC+" per gift";
  var limits = v.left==null ? "" : (v.left>0 ? v.left+" gift"+(v.left===1?"":"s")+" left today" : "No gifts left today: try again tomorrow");
  if(GIFT.unsure && GIFT.body){
    // Replaying a gift that may already have landed: the server judges it, not today's balances
    // or gift count (the first try may be what spent them).
    var b=GIFT.body;
    $("giftSummary").textContent=GIFT.name+" gets "+giftWhat(b.coins, b.kind, b.qty, " + ", false);
    $("giftLimits").textContent="";
    giftSendButton("Send again", false);
  } else {
    if(v.bad) err="Use whole numbers";
    else if(v.coins>v.maxC) err=(v.have<v.capC) ? "You have "+coinsN(v.have) : "At most "+coinsN(v.capC)+" per gift";
    else if(v.kind && v.qty>v.maxQ) err="You can give up to "+v.maxQ+" of those";
    else if(v.kind && v.qty===0) err="Pick an amount for that snack";
    var what=giftWhat(v.coins, v.kind, v.qty, " + ", false);
    $("giftSummary").textContent = err ? err : (what ? GIFT.name+" gets "+what : "Pick Poke Coins, a snack, or both");
    $("giftLimits").textContent = limits;
    giftSendButton("Send", !!err || !what || v.left===0);
  }
  // Nothing can be sent from this page until it's reloaded (pantryStale): Send becomes that.
  if(PANTRY.store==="reload" && !GIFT.busy){
    $("giftLimits").textContent="Claude HQ restarted since this page loaded: "+
      (GIFT.unsure ? "reload the page, then Send again (it won\u2019t be sent twice)" : "reload the page to send gifts");
    giftSendButton("Reload page", false);
  }
}
// Send in flight stays focusable (aria-disabled, and giftSend ignores it while busy): a disabled
// button can't hold focus, so a keyboard user would drop out of the dialog onto the page.
function giftSendButton(label, off){
  var send=$("giftSend");
  send.textContent = GIFT.busy ? "Sending\u2026" : label;
  send.disabled = !GIFT.busy && off;
  if(GIFT.busy) send.setAttribute("aria-disabled","true"); else send.removeAttribute("aria-disabled");
}
// After an answer that leaves the dialog open: if focus left it (Send turned disabled under it),
// put it back on the first control that can take it.
function giftKeepFocus(){
  var box=$("giftBack"), a=document.activeElement;
  if(!box.classList.contains("open") || (a && a!==box && box.contains(a) && !a.disabled)) return;
  var t=[$("giftSend"), $("giftCoins"), $("giftCancel")].filter(function(n){ return !n.disabled; })[0];
  if(t) t.focus();
}
function giftSend(){
  if(GIFT.busy || !GIFT.handle) return;
  if(PANTRY.store==="reload"){ pageReload(); return; }   // a lost gift stays saved for after it
  var h=GIFT.handle, name=GIFT.name, rid=GIFT.rid, body=(GIFT.unsure && GIFT.body) ? GIFT.body : null, replay=!!body;
  if(!body){
    var v=giftVals(); if(!giftWhat(v.coins, v.kind, v.qty, "+", false)) return;
    body={requestId:rid, toHandle:h, coins:v.coins, qty:v.kind?v.qty:0, note:v.note};
    if(v.kind && v.qty>0) body.kind=v.kind;
  }
  // The dialog may be closed (or reopened for someone else) by the time the answer comes back.
  function mine(){ return GIFT.handle===h && GIFT.rid===rid; }
  giftUnsureSet(h, body);   // saved until a definitive answer (see giftUnsureAll)
  GIFT.busy=true; $("giftErr").textContent=""; giftUpdate();
  arenaPost("/api/arena/pantry/give", body).then(function(res){
    if(mine()) GIFT.busy=false;
    if(res.ok && pantryValid(res.j)){
      giftUnsureSet(h, null);
      var s=(res.j.sent && typeof res.j.sent==="object") ? res.j.sent : {coins:body.coins, kind:body.kind||null, qty:body.qty};
      var what=giftWhat(s.coins|0, s.kind, s.qty|0, " and ", true) || giftWhat(body.coins, body.kind, body.qty, " and ", true);
      // A replay means an earlier try had landed; its answer (delivered live or not) was lost, and
      // the replay's deliveredLive is always 0, so say neither.
      var msg = res.j.replayed ? "\uD83C\uDF81 Sent "+name+" "+what
              : res.j.deliveredLive ? "\uD83C\uDF81 "+name+" got "+what
              : "\uD83C\uDF81 Sent: "+name+" gets it next time Claude HQ is running";
      pantryApply(res.j);   // first, so the gift buttons focus returns to are current
      if(mine()) giftClose();
      toast(msg,"level"); announce(msg); logEvent("\uD83C\uDF81", "Gave "+name+" "+what);
      return;
    }
    if(pantryUnsure(res)){ giftUnsure(h, name, body); return; }
    if(pantryLocalReject(res)){
      // Turned away by dashboard.py itself (this page is older than its last start): no answer
      // from the Arena. A replay stays saved, since its first try may have landed; a new gift
      // simply didn't go out.
      if(!replay) giftUnsureSet(h, null);
      pantryStale();
      var how = replay ? "Reload the page, then Send again: it won\u2019t be sent twice." : "Nothing was sent: reload the page to send it.";
      if(!mine()){ toast("\u26A0 Gift to "+name+": Claude HQ restarted since this page loaded. "+how, "ach"); return; }
      $("giftErr").textContent="Claude HQ restarted since this page loaded. "+how;
      giftFill(false); giftKeepFocus();
      return;
    }
    // A definitive no: the gift didn't happen, so there's nothing left to confirm.
    giftUnsureSet(h, null);
    var err=pantryErr(res, "That gift didn\u2019t go through"), miss=pantryMissState(res);
    if(miss || (res.status===400 && /not paired/i.test(err))){
      if(mine()) giftClose();
      pantrySetStore(miss || "unpaired");
      if(miss==="unsupported") toast("\u26A0 This Arena server doesn\u2019t have the store yet","ach");
      else if(miss==="restart") toast("\u26A0 Restart Claude HQ, then reload this page: gifts need the new version","ach");
      return;
    }
    if(!mine()){ toast("\u26A0 "+name+": "+err, "ach"); return; }
    GIFT.unsure=false; GIFT.body=null; GIFT.rid=newRequestId();   // editing it makes a new gift
    $("giftErr").textContent=err;
    giftFill(false); giftKeepFocus();
  }, function(){ if(mine()) GIFT.busy=false; giftUnsure(h, name, body); });
}
// No answer, or a 5xx: the gift may or may not have landed. It stays saved with its requestId, and
// the form locks, so "Send again" (now, or after reopening the dialog for this person) can only
// replay this exact gift, which the server never applies twice.
function giftUnsure(h, name, body){
  giftUnsureSet(h, body);
  if(GIFT.handle!==h || GIFT.rid!==body.requestId){
    toast("\u26A0 Couldn\u2019t confirm the gift to "+name+". Open it again to finish it: it won\u2019t be sent twice.", "ach");
    return;
  }
  GIFT.unsure=true; GIFT.body=body;
  $("giftCoins").value=String(body.coins); $("giftQty").value=String(body.qty); $("giftNote").value=body.note;
  $("giftErr").textContent="Couldn\u2019t confirm. Send again (safe, won\u2019t send twice)";
  giftFill(false);
  $("giftSend").focus();
}
(function(){
  var gb=$("giftBack"); if(!gb) return;
  gb.addEventListener("click",function(e){ if(e.target===gb) giftClose(); });
  $("giftX").addEventListener("click",giftClose);
  $("giftCancel").addEventListener("click",giftClose);
  $("giftSend").addEventListener("click",giftSend);
  ["giftCoins","giftQty","giftNote"].forEach(function(id){
    $(id).addEventListener("input",giftUpdate);
    $(id).addEventListener("keydown",function(e){ if(e.key==="Enter" && !$("giftSend").disabled){ e.preventDefault(); giftSend(); } });
  });
  $("giftKind").addEventListener("change",function(){
    var q=$("giftQty");
    if($("giftKind").value && !(parseInt(q.value,10)>0)) q.value="1";
    if(!$("giftKind").value) q.value="0";
    giftUpdate();
  });
})();
// A gift arrived on the lobby socket. Untrusted: shape-checked, shown as text, no sound.
var GIFT_SEEN={}, GIFT_RELOAD_T=null;
function arenaOnGift(m){
  if(!m.from || typeof m.from!=="object") return;
  var k=(m.kind==null) ? null : m.kind, c=m.coins, q=m.qty;
  if(k!==null && !isFood(k)) return;
  if(!(typeof c==="number" && c%1===0 && c>=0 && c<=5)) return;
  if(!(typeof q==="number" && q%1===0 && q>=0 && q<=3)) return;
  if(c+q<=0 || (q>0 && !k)) return;
  if(typeof m.id==="string"){ if(GIFT_SEEN[m.id]) return; GIFT_SEEN[m.id]=1; }
  var note=(typeof m.note==="string") ? m.note.replace(/\s+/g," ").trim().slice(0,80) : "";
  var who=arenaWho(m.from), it=foodInfo(k), bits=[];
  if(c>0) bits.push(coinsN(c));
  if(it && q>0) bits.push(it.emoji+" "+it.name+" \u00D7"+q);
  var msg="\uD83C\uDF81 "+who+" sent you "+bits.join(" + ")+(note ? " \u2014 \u201C"+note+"\u201D" : "");
  toast(msg,"level"); announce(msg);
  logEvent("\uD83C\uDF81", who+" sent you "+giftWhat(c, k, q, " and ", false)+(note ? ": "+note : ""));
  if(NOTIF_ON && ("Notification" in window) && Notification.permission==="granted"){
    try{ new Notification("\uD83C\uDF81 "+who+" sent you a gift", {body:giftWhat(c, k, q, " and ", false)+(note ? ": "+note : ""), tag:"hq-gift", silent:true}); }catch(e){}
  }
  clearTimeout(GIFT_RELOAD_T); GIFT_RELOAD_T=setTimeout(function(){ pantryLoad(true); }, 1000);
}


