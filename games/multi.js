/* Valley: multiplayer games, refereed by the Arena server (backend/app/valley.py).
 *
 * Every game here has a lobby (who's in, who hosts) with invites; the server decides
 * bites, words, damage, loot and the shared farm, and this file only draws what it says.
 * Messages: {type:"game", g, op, ...} out, {type:"game", g, ev, ...} in (via HQV.onGame).
 * Only game moves travel; nothing transcript-derived is ever sent.
 */
(function(){
"use strict";
var HQV = window.HQV; if(!HQV) return;
var api = HQV.api;
var NAMES = {pond:"Fishing Pond", race:"Puzzle Race", duel:"Creature Duel", mines:"Co-op Mines", farm:"Shared Farm", golf:"Mini Golf", kart:"Kart Racing", plat:"Platformer Rush", fps:"Blaster Arena", strike:"Strikezone", type:"Code Typing Race", td:"Tower Defense", bowl:"Bowling"};
var LIVE = {};          // g -> {lobby:[...], ...game state from the server}
// Protocol versions this client speaks, per game: [oldest, newest]. Must cover the
// server's PROTOCOL table (backend/app/valley.py, backend/src/protocol.rs). A server
// below the range is an Arena to update; above it, Claude HQ is the one to update.
var CLIENT_PROTO = {pond:[1,1], race:[1,1], duel:[1,1], mines:[1,1], farm:[1,1], golf:[1,1], kart:[1,2], plat:[1,1], fps:[1,1], strike:[1,4], hq:[1,1], type:[1,1], td:[1,1], bowl:[1,1]};
// null = fine to play; else {who:"arena"|"hq", text} explaining why not.
function protoCheck(g){
  var info = A().arena;
  if(!info) return null;                         // an Arena from before 2.0: each game copes on its own
  var gi = info.games[g], want = CLIENT_PROTO[g] || [1,1], name = NAMES[g] || g;
  if(!gi) return {who:"arena", text:"This Arena doesn't run " + name + " yet. Ask its owner to update it" + (info.impl === "rs" ? " (or use the Python Arena, which runs every game)." : ".")};
  var v = gi.v|0;
  if(v < want[0]) return {who:"arena", text:"This Arena runs an older " + name + " than Claude HQ speaks. Ask its owner to update the Arena."};
  if(v > want[1]) return {who:"hq", text:"This Arena runs a newer " + name + ". Update Claude HQ to play it."};
  return null;
}
HQV.protoCheck = protoCheck;

function A(){ return window.ARENA || {}; }
function me(){ var y = A().you; return y && y.userId; }
function sockOpen(){ var s = A().sock; return !!(s && s.readyState === 1); }
function send(g, op, data){
  if(!sockOpen()) return false;
  // A game's moves wait until its shell has (re-)joined the lobby on this very socket:
  // after a quick reconnect the server would only answer "join the lobby first".
  var c = CTX[g];
  if(c && op !== "join" && op !== "leave" && c.sock !== A().sock) return false;
  var msg = {type:"game", g:g, op:op}; for(var k in (data||{})) msg[k] = data[k];
  try { A().sock.send(JSON.stringify(msg)); return true; } catch(e){ return false; }
}
function nameOf(p){ return (p && (p.displayName || p.handle)) || "someone"; }
function st(g){ return LIVE[g] = LIVE[g] || {lobby:[]}; }

/* ---------- shared shell: connect, lobby panel, invites ---------- */
function shell(g, el, body){
  var root = api.mk("div","vg-mp"), lobbyBox = api.mk("div","vg-lobby"), gameBox = api.mk("div","vg-mp-game");
  var badge = api.mk("span","vg-reconnect","Reconnecting…"); badge.hidden = true; badge.setAttribute("role","status");
  root.appendChild(badge); root.appendChild(lobbyBox); root.appendChild(gameBox); el.appendChild(root);
  var tries = 0, joined = false, ctx = {g:g, lobbyBox:lobbyBox, box:gameBox, alive:true, sock:null, offline:false, watch:0};
  (function connect(){
    if(!ctx.alive) return;
    if(!sockOpen()){
      lobbyBox.textContent = "";
      lobbyBox.appendChild(api.mk("p","vg-muted", tries ? "Connecting to the Arena…" : "Play with friends in your current Arena room."));
      if(!A().sock && tries > 4){
        lobbyBox.textContent = "";
        lobbyBox.appendChild(api.mk("p",null,"Multiplayer games run in an Arena room. Pair with an Arena server and open the Arena once to connect."));
        lobbyBox.appendChild(api.btn("Open the Arena","primary",function(){ if(window.setView) window.setView("arena"); }));
        ctx.watch = setInterval(watch, 400);    // join as soon as the Arena connects
        return;
      }
      tries++; setTimeout(connect, 600); return;
    }
    var bad = protoCheck(g);
    if(bad){
      lobbyBox.textContent = "";
      lobbyBox.appendChild(api.mk("p", null, bad.text));
      if(bad.who === "hq" && typeof window.updateRun === "function") lobbyBox.appendChild(api.btn("Update Claude HQ", "primary", function(){ window.updateRun(); }));
      return;
    }
    watch();
    ctx.watch = setInterval(watch, 250);
  })();
  // The room socket can drop (wifi, laptop sleep, server restart): show a small badge (or let
  // a game with its own badge, ctx.onConn, show it), keep the game running, and when
  // index.html has a new socket open, rejoin the lobby on it; the server answers with a full
  // snapshot, so the game resyncs from that. send() holds a game's moves until that rejoin.
  function hook(name, arg){ if(ctx[name]){ try { ctx[name](arg); } catch(e){} } }
  function watch(){
    if(!ctx.alive){ clearInterval(ctx.watch); return; }
    var sk = A().sock, open = sockOpen();
    if(open && sk !== ctx.sock){
      var again = joined; joined = true; ctx.sock = sk; send(g, "join");
      if(ctx.offline){ ctx.offline = false; badge.hidden = true; }
      if(again){ hook("onRejoin"); hook("onConn", true); }
    } else if(!open && joined && !ctx.offline){
      ctx.offline = true; badge.hidden = !!ctx.onConn;
      hook("onOffline"); hook("onConn", false);
    }
  }
  ctx.renderLobby = function(){ renderLobby(ctx); };
  ctx.cleanup = function(){
    ctx.alive = false; clearInterval(ctx.watch);
    if(joined && ctx.beforeLeave){ try { ctx.beforeLeave(); } catch(e){} }
    if(joined) send(g, "leave");
  };
  body(ctx);
  return ctx;
}
function renderLobby(ctx){
  // The roster part is redrawn on every lobby event; the invite picker below it is one
  // persistent node (redrawing it would blur the box and drop what you're typing).
  var s = st(ctx.g), pick = invitePicker(ctx);
  if(!ctx.lobbyInfo) ctx.lobbyInfo = api.mk("div","vg-lobby-info");
  if(ctx.lobbyInfo.parentNode !== ctx.lobbyBox || pick.parentNode !== ctx.lobbyBox){
    ctx.lobbyBox.textContent = ""; ctx.lobbyBox.appendChild(ctx.lobbyInfo); ctx.lobbyBox.appendChild(pick);
  }
  var box = ctx.lobbyInfo; box.textContent = "";
  var head = api.mk("div","vg-row");
  head.appendChild(api.mk("b",null,"Lobby · "+s.lobby.length+" player"+(s.lobby.length===1?"":"s")+" · room: "+(A().roomName || A().roomId || "?")));
  box.appendChild(head);
  var list = api.mk("div","vg-players");
  s.lobby.forEach(function(p){
    var chip = api.mk("span","vg-player"+(p.userId===me()?" me":""), nameOf(p)+(p.host?" ★":""));
    chip.title = p.host ? "Host" : "";
    list.appendChild(chip);
  });
  box.appendChild(list);
}

/* ---------- invite / nudge picker ----------
 * One search box with suggestions (avatar, name, @handle): people in this Arena room who
 * aren't in the game get a game invite, anyone else on the Arena board gets a nudge; an exact
 * @handle nobody matches can still be nudged. Arrows / Enter / Esc, or click. */
var ROSTER = {list: [], at: 0, loading: false, ok: false};    // everyone on the Arena board
function rosterLoad(then){
  if(ROSTER.loading || (ROSTER.ok && Date.now() - ROSTER.at < 600000)){ if(then) then(); return; }
  ROSTER.loading = true;
  fetch("/api/arena/board?window=all", {cache: "no-store"}).then(function(r){ return r.ok ? r.json() : null; }).then(function(b){
    var seen = {};
    ROSTER.list = ((b && b.entries) || []).filter(function(e){
      if(!e || typeof e.handle !== "string" || !/^[A-Za-z0-9_-]{1,39}$/.test(e.handle) || seen[e.handle.toLowerCase()]) return false;
      return (seen[e.handle.toLowerCase()] = true);
    }).map(function(e){
      return {handle: e.handle, name: String(e.displayName || e.trainerName || e.handle).slice(0, 40),
              av: typeof e.avatarUrl === "string" && /^https:\/\//.test(e.avatarUrl) ? e.avatarUrl : ""};
    });
    ROSTER.ok = !!b; ROSTER.at = Date.now();
  }, function(){}).then(function(){ ROSTER.loading = false; if(then) then(); });
}
function invitePicker(ctx){
  if(ctx.picker){ ctx.picker.refresh(); return ctx.picker.el; }
  var wrap = api.mk("div","vg-invite"), inp = api.mk("input","vg-input vg-invite-in"), list = api.mk("div","vg-sugg hidden");
  var lid = "vgSugg-"+ctx.g;
  list.id = lid; list.setAttribute("role","listbox"); list.setAttribute("aria-label","People to invite");
  inp.type = "text"; inp.maxLength = 41; inp.autocomplete = "off"; inp.spellcheck = false;
  inp.placeholder = "Invite or nudge: type a name or @handle";
  inp.setAttribute("role","combobox"); inp.setAttribute("aria-autocomplete","list"); inp.setAttribute("aria-expanded","false"); inp.setAttribute("aria-controls", lid);
  var go = api.btn("Invite","", function(){ pick(P.active >= 0 ? P.items[P.active] : null, true); });
  var row = api.mk("div","vg-row vg-invite-row"); row.appendChild(inp); row.appendChild(go);
  wrap.appendChild(row); wrap.appendChild(list);
  var P = {el: wrap, items: [], active: -1, open: false};
  // Candidates: people online in this room and not in the game (invite), then the Arena (nudge).
  function candidates(){
    var s = st(ctx.g), inLobby = {}, q = inp.value.trim().replace(/^@/, "").toLowerCase(), out = [], have = {};
    s.lobby.forEach(function(p){ inLobby[p.userId] = 1; });
    (A().lobby || []).forEach(function(p){
      if(!p || !p.userId || inLobby[p.userId] || p.userId === me()) return;
      var h = String(p.handle || ""); have[h.toLowerCase()] = 1;
      out.push({kind: "invite", userId: p.userId, handle: h, name: nameOf(p), av: typeof p.avatarUrl === "string" && /^https:\/\//.test(p.avatarUrl) ? p.avatarUrl : ""});
    });
    var mine = String((A().you && A().you.handle) || "").toLowerCase();
    ROSTER.list.forEach(function(r){
      var k = r.handle.toLowerCase(); if(have[k] || k === mine) return;
      out.push({kind: "nudge", handle: r.handle, name: r.name, av: r.av});
    });
    if(!q) return out.filter(function(c){ return c.kind === "invite"; }).concat(out.filter(function(c){ return c.kind === "nudge"; })).slice(0, 8);
    var scored = [];
    out.forEach(function(c){
      var h = c.handle.toLowerCase(), n = c.name.toLowerCase();
      var sc = (h.indexOf(q) === 0 || n.indexOf(q) === 0) ? 0 : n.split(" ").some(function(w){ return w.indexOf(q) === 0; }) ? 1 : (h.indexOf(q) >= 0 || n.indexOf(q) >= 0) ? 2 : -1;
      if(sc >= 0) scored.push({c: c, s: sc + (c.kind === "invite" ? 0 : 0.5)});
    });
    scored.sort(function(a, b){ return (a.s - b.s) || a.c.name.localeCompare(b.c.name); });
    return scored.slice(0, 8).map(function(x){ return x.c; });
  }
  function paint(){
    Array.prototype.forEach.call(list.children, function(o, i){ var on = i === P.active; o.classList.toggle("on", on); o.setAttribute("aria-selected", on ? "true" : "false"); });
    if(P.active >= 0 && P.items[P.active]){ inp.setAttribute("aria-activedescendant", lid+"-"+P.active); go.textContent = P.items[P.active].kind === "invite" ? "Invite" : "Nudge"; }
    else { inp.removeAttribute("aria-activedescendant"); go.textContent = /^@?[A-Za-z0-9_-]{1,39}$/.test(inp.value.trim()) ? "Nudge" : "Invite"; }
  }
  function show(){
    P.items = candidates(); P.active = P.items.length && inp.value.trim() ? 0 : -1;
    list.textContent = "";
    P.items.forEach(function(c, i){
      var o = api.mk("div","vg-sugg-o"); o.id = lid+"-"+i; o.setAttribute("role","option");
      if(c.av){ var im = document.createElement("img"); im.alt = ""; im.loading = "lazy"; im.referrerPolicy = "no-referrer"; im.width = 22; im.height = 22; im.src = c.av; o.appendChild(im); }
      else o.appendChild(api.mk("span","vg-sugg-dot", (c.name || "?").charAt(0).toUpperCase()));
      o.appendChild(api.mk("span","vg-sugg-n", c.name));
      o.appendChild(api.mk("span","vg-sugg-h", c.kind === "invite" ? "in this room · invite" : "@"+c.handle+" · nudge"));
      o.addEventListener("mousedown", function(e){ e.preventDefault(); pick(c, false); });
      list.appendChild(o);
    });
    if(!P.items.length && inp.value.trim()){
      list.appendChild(api.mk("p","vg-sugg-note", ROSTER.ok ? "Nobody on the Arena matches. Enter nudges that exact @handle." : "Loading the Arena…"));
    }
    P.open = !!(P.items.length || inp.value.trim());
    list.classList.toggle("hidden", !P.open);
    inp.setAttribute("aria-expanded", P.items.length ? "true" : "false");
    paint();
  }
  function close(){ P.open = false; list.classList.add("hidden"); inp.setAttribute("aria-expanded","false"); P.active = -1; paint(); }
  function pick(c, fromButton){
    if(!c){
      var raw = inp.value.trim().replace(/^@/, "");
      if(!/^[A-Za-z0-9_-]{1,39}$/.test(raw)){ if(fromButton) api.toast("Type a name or @handle"); inp.focus(); return; }
      c = {kind: "nudge", handle: raw, name: raw};
    }
    if(c.kind === "invite"){ ctx.pendingInvite = {handle: c.handle, name: c.name}; send(ctx.g, "invite", {to: c.userId}); }
    else nudge(c.handle, ctx.g);
    inp.value = ""; close(); inp.focus();
  }
  inp.addEventListener("focus", function(){ rosterLoad(function(){ if(document.activeElement === inp) show(); }); show(); });
  inp.addEventListener("input", show);
  inp.addEventListener("blur", function(){ setTimeout(close, 120); });
  inp.addEventListener("keydown", function(e){
    var n = P.items.length;
    if(e.key === "ArrowDown" && n){ e.preventDefault(); if(!P.open) show(); P.active = (P.active + 1) % n; paint(); }
    else if(e.key === "ArrowUp" && n){ e.preventDefault(); P.active = P.active <= 0 ? n - 1 : P.active - 1; paint(); }
    else if(e.key === "Enter"){ e.preventDefault(); pick(P.active >= 0 ? P.items[P.active] : null, true); }
    else if(e.key === "Escape" && P.open){ e.preventDefault(); e.stopPropagation(); close(); }
  });
  P.refresh = function(){ if(P.open && document.activeElement === inp) show(); };
  ctx.picker = P;
  return wrap;
}
function nudge(handle, g){
  if(typeof window.arenaPost !== "function") return;
  window.arenaPost("/api/arena/nudge", {toHandle:handle, note:"Come play "+NAMES[g]+" in the Valley"}).then(function(r){
    api.toast(r && r.ok ? "👋 Nudged @"+handle : "Couldn’t nudge @"+handle);
  });
}

/* ---------- incoming: route by game ---------- */
var CTX = {};   // g -> mounted ctx
HQV.onGame = function(m){
  var g = m.g, s = st(g);
  if(m.ev === "lobby"){ s.lobby = Array.isArray(m.members) ? m.members : []; }
  if(m.ev === "error"){
    var eh = HANDLERS[g];
    var handled = false;
    if(eh && eh.onError){ try { handled = eh.onError(m, s); } catch(e){} }
    if(handled) return;     // the game handled it (e.g. pond land retry)
    if(CTX[g]) api.toast("⚠ "+String(m.error||"error").slice(0,120));
    return;
  }
  if(m.ev === "invited"){
    var c = CTX[g];
    if(m.delivered) api.toast("Invite sent");
    else if(c && c.pendingInvite && c.pendingInvite.handle) nudge(c.pendingInvite.handle, g);   // not online: fall back to a nudge
    return;
  }
  var h = HANDLERS[g]; if(h && h.on) h.on(m, s);
  var ctx = CTX[g]; if(ctx && ctx.alive){ if(m.ev === "lobby") ctx.renderLobby(); if(h && h.render) h.render(ctx, s); }
};

function register(g, icon, desc, mount){
  HQV.register({id:"mp-"+g, mp:true, name:NAMES[g], icon:icon, desc:desc,
    mount:function(el){ CTX[g] = shell(g, el, function(ctx){ mount(ctx); }); },
    unmount:function(){ var c = CTX[g]; if(c){ c.cleanup(); if(c.stop) c.stop(); } delete CTX[g]; },
    pause:function(){ var c = CTX[g]; if(c) c.paused = true; },
    resume:function(){ var c = CTX[g]; if(c) c.paused = false; },
    badge:function(){ var s = LIVE[g]; return s && s.lobby.length ? s.lobby.length+" playing" : ""; }});
}
var HANDLERS = {};
// Games in their own files (golf.js) reuse the lobby, invite and connect shell through this.
HQV.mp = {register:register, send:send, handlers:HANDLERS, st:st, me:me, nameOf:nameOf, sockOpen:sockOpen,
  ctx:function(g){ return CTX[g]; }};

/* =============================== POND =============================== */
// A shared dock drawn with games/fishart.js (same scene, rig and reel as the solo pond).
// The server referees every cast (valley.Pond): it picks the fish, the bite time and the
// treasure, and it refuses a land sooner than bite + MIN_REEL_MS[rarity]. That table is
// mirrored here so a fast, clean reel waits a moment instead of being refused.
//
// Smoothness: everything you do renders at once (charge, cast arc, hook, reel, the fish
// leaping to you on land) and the server's answer only fills in the score; other anglers
// animate from their discrete events (cast arc, waiting bob, hooked thrash, catch arc),
// dock slots glide when people join or leave, the boss HP bar is predicted from your own
// pulls and eased toward the server's value, and the side panel patches nodes in place.
var MIN_REEL = {1:1500, 2:2200, 3:3000, 4:4000};   // keep in sync with backend/app/valley.py MIN_REEL_MS
var POND = null;                                    // the mounted dock view, if any
function pondFeed(s, text){
  s.feed = s.feed || []; s.feedSeq = (s.feedSeq|0) + 1;
  s.feed.unshift({id:s.feedSeq, text:text}); if(s.feed.length > 6) s.feed.length = 6;
}
HANDLERS.pond = {
  on: function(m, s){
    s.lines = s.lines || {}; s.last = s.last || {};
    var V = POND, now = performance.now(), uid = m.user && m.user.userId, mine = !!uid && uid === me();
    if(m.ev === "pond"){
      var p = m.pond||{}, lines = {};
      s.scores = p.scores||{}; s.goal = p.goal|0; s.goalTarget = p.goalTarget||20; s.boss = p.boss || null;
      // Newer servers send every line with its aim and state; older ones only who is casting.
      if(Array.isArray(p.lines)) p.lines.forEach(function(l){ if(l && l.userId) lines[l.userId] = {id:l.id, aim: typeof l.aim === "number" ? l.aim : null, hooked: !!l.hooked}; });
      else (p.casting||[]).forEach(function(u){ lines[u] = {aim:null, hooked:false}; });
      s.lines = lines;
      if(V) V.resync(now);
    }
    if(m.ev === "lobby" && m.left && m.left.userId && m.left.userId !== me()){
      // Someone left (or their last socket dropped): the server reels their line in with a
      // 'lost' first; this also clears a line from a server too old to do that.
      var gone = m.left.userId, rp = V && V.remote[gone];
      delete s.lines[gone];
      if(rp && rp.phase !== "idle"){ rp.phase = "idle"; if(rp.bob) rp.reelIn = {x:rp.bob.x, y:rp.bob.y, at:now}; rp.bob = null; }
    }
    if(m.ev === "cast" && V) V.onCast(m, now);
    if(m.ev === "loot" && api.items[m.item]){ s.loot = m.item; api.inv.add(m.item, 1); if(V) V.onLoot(m.item); }
    if(m.ev === "casting" && uid){ s.lines[uid] = {id:m.id, aim: typeof m.aim === "number" ? m.aim : null, hooked:false}; if(V && !mine) V.remoteCast(uid, now, false); }
    if(m.ev === "hooked" && uid && s.lines[uid] && (m.id == null || s.lines[uid].id == null || s.lines[uid].id === m.id)){ s.lines[uid].hooked = true; if(V && !mine) V.remoteHooked(uid, now); }
    if(m.ev === "caught" || m.ev === "lost"){
      var it = api.items[m.fish], line = uid && s.lines[uid];
      // An event about an older line (one we let go of on unmount or a reconnect) must not
      // touch the line that is out now: cast ids tell them apart.
      var stale = m.id != null && (mine ? !!(s.mine && (s.mine.id != null ? s.mine.id !== m.id : s.mine.pending)) : !!(line && line.id != null && line.id !== m.id));
      if(uid && !stale) delete s.lines[uid];
      pondFeed(s, m.ev === "caught" ? (mine ? "You" : nameOf(m.user))+" caught "+(it?it.name:m.fish)+" (+"+m.points+(m.perfect ? ", perfect" : "")+")" : (mine ? "You" : nameOf(m.user))+" lost "+(it?it.name:"a fish"));
      if(m.ev === "caught"){ s.scores = m.scores||s.scores; s.goal = m.goal|0; s.goalTarget = m.goalTarget||s.goalTarget; if(uid && it) s.last[uid] = m.fish;
        if(mine) api.inv.add(m.fish, 1); }
      if(V && !stale) V.onResult(m, mine, now);
      if(mine && !stale) s.mine = null;
    }
    if(m.ev === "goal"){ pondFeed(s, "🎉 Room goal reached! Everyone gets a Gold Ore"); if(CTX.pond){ api.inv.add("gold", 1); api.toast("🎉 Room goal reached! +1 Gold Ore", "ach"); } if(V) V.fx.confetti(); }
    if(m.ev === "boss" || m.ev === "bosshp"){
      var prev = s.boss; s.boss = m.boss; s.pulling = m.pulling|0;
      if(m.ev === "boss" && V) V.bossGoneLocal = false;
      if(m.ev === "boss"){ pondFeed(s, "🐉 A Merge Leviathan surfaced! Everyone hold to reel!"); api.toast("🐉 Boss fish! Everyone hold to reel it in"); if(V) V.bossUp(now); }
      else if(V && prev && m.boss && m.boss.hp < prev.hp) V.bossHit(now);
      if(V) V.bossSync(now);
    }
    if(m.ev === "bossdown"){
      var helped = (m.helpers||[]).indexOf(me()) >= 0;
      s.boss = null; s.pulling = 0; s.scores = m.scores||s.scores; pondFeed(s, "🐉 The room landed the Merge Leviathan!");
      if(helped) api.inv.add("leviathan", 1);
      if(V) V.bossDown(now, helped);
    }
    if(m.ev === "bossgone"){
      s.boss = null; s.pulling = 0;
      if(V && V.bossGoneLocal) V.bossGoneLocal = false;        // our countdown already let it go
      else { pondFeed(s, "The Leviathan slipped away…"); if(V) V.bossGone(now); }
    }
  },
  onError: function(m){ return POND ? POND.onError(String(m.error||""), performance.now()) : false; },
  render: function(ctx, s){ if(ctx.side) ctx.side.update(s); }
};

// The side panel: built once, then only text, widths and order change (no rebuild per
// message, so nothing flickers and the aria-live feed announces only new lines).
function pondSide(el){
  var goal = api.mk("div","vg-goal"), goalT = api.mk("span"), gm = api.mk("div","vg-meter"), gf = api.mk("i");
  gm.appendChild(gf); goal.appendChild(goalT); goal.appendChild(gm); el.appendChild(goal);
  var boss = api.mk("div","vg-boss"), bossT = api.mk("b"), bm = api.mk("div","vg-meter low"), bf = api.mk("i"), bossN = api.mk("span","vg-muted");
  bm.appendChild(bf); boss.appendChild(bossT); boss.appendChild(bm); boss.appendChild(bossN); boss.hidden = true; el.appendChild(boss);
  el.appendChild(api.mk("b", null, "Scores"));
  var empty = api.mk("span","vg-muted","No catches yet."), ol = api.mk("ol","vg-score");
  el.appendChild(empty); el.appendChild(ol);
  var feed = api.mk("div","vg-feed"); feed.setAttribute("aria-live","polite"); feed.setAttribute("aria-relevant","additions");
  var feedEmpty = api.mk("div","vg-muted","Catches, misses and boss fights show up here."); feed.appendChild(feedEmpty);
  el.appendChild(feed);
  var rows = {}, feedNodes = [];     // uid -> {li, ic, rar, nm, pts, fish}; feed nodes newest first
  function set(n, t){ t = String(t); if(n.textContent !== t) n.textContent = t; }
  function width(n, pct){ var w = Math.max(0, Math.min(100, Math.round(pct)))+"%"; if(n.style.width !== w) n.style.width = w; }
  var side = {el:el, mut:0};
  side.update = function(s){
    var tgt = s.goalTarget||20;
    set(goalT, "Room goal: "+(s.goal|0)+"/"+tgt+" fish"); width(gf, (s.goal|0)/tgt*100);
    boss.hidden = !s.boss;
    var names = {}; (s.lobby||[]).forEach(function(p){ names[p.userId] = nameOf(p); });
    var sc = s.scores||{}, ids = Object.keys(sc).sort(function(a,b){ return sc[b]-sc[a] || (a < b ? -1 : 1); });
    empty.hidden = ids.length > 0;
    ids.forEach(function(u, i){
      var r = rows[u];
      if(!r){ r = rows[u] = {li:api.mk("li", u === me() ? "me" : ""), ic:null, rar:api.mk("span"), nm:api.mk("span"), pts:api.mk("b"), fish:null};
        r.li.appendChild(r.rar); r.li.appendChild(r.nm); r.li.appendChild(r.pts); }
      var last = (s.last||{})[u];
      if(last !== r.fish && api.items[last]){
        r.fish = last; var ic = api.iconEl(last, 2); ic.title = api.items[last].name;
        if(r.ic) r.li.replaceChild(ic, r.ic); else r.li.insertBefore(ic, r.rar);
        r.ic = ic; r.rar.className = "vg-rar vg-rar-"+api.items[last].rarity; set(r.rar, api.items[last].name);
      }
      set(r.nm, u === me() ? "You" : (names[u]||"?")); set(r.pts, sc[u]);
      if(ol.children[i] !== r.li) ol.insertBefore(r.li, ol.children[i] || null);   // moves, never rebuilds
    });
    Object.keys(rows).forEach(function(u){ if(!(u in sc)){ rows[u].li.remove(); delete rows[u]; } });
    // feed: prepend only entries we haven't shown, drop the ones that fell off the end
    var list = s.feed||[], have = feedNodes.length ? feedNodes[0].id : 0, add = list.filter(function(f){ return f.id > have; });
    for(var k=add.length-1;k>=0;k--){ var n = api.mk("div", null, add[k].text); n.dataset.id = add[k].id; feed.insertBefore(n, feed.firstChild); feedNodes.unshift({id:add[k].id, n:n}); side.mut++; }
    while(feedNodes.length > 6){ feedNodes.pop().n.remove(); }
    feedEmpty.hidden = feedNodes.length > 0;
  };
  // Called from the animation loop: boss line at most once a second (only when it changed).
  side.boss = function(s, left, hpShown){
    if(!s.boss) return;
    set(bossT, "BOSS: Merge Leviathan · "+left+"s");
    width(bf, hpShown/(s.boss.max||1)*100);
    set(bossN, "HP "+Math.max(0, Math.round(hpShown))+"/"+s.boss.max+" · hold Space / mouse to reel together · "+(s.pulling||0)+" pulling now");
  };
  return side;
}

register("pond", "🎣", "Shared dock, room goal and a boss fish", function(ctx){
  var FA = HQV.fishArt, s = st("pond");
  s.feed = s.feed || []; s.lines = s.lines || {}; s.last = s.last || {};
  var DECK_TOP = 123, dbg = false; try { dbg = !!localStorage.getItem("hq-debug"); } catch(e){}
  var layout = api.mk("div","vg-dock"), left = api.mk("div","vg-body"), row = api.mk("div","vg-row");
  var castBtn = api.btn("Cast","primary",function(){ quickCast(); cv.focus(); });
  var snd = api.btn("", "ghost", function(){ FA.sfx.set(!FA.sfx.on()); syncSound(); if(FA.sfx.on()) FA.sfx.unlock(); });
  function syncSound(){ var on = FA.sfx.on(); snd.textContent = on ? "🔊 Sound" : "🔇 Sound"; snd.setAttribute("aria-pressed", on ? "true" : "false"); }
  syncSound();
  row.appendChild(castBtn); row.appendChild(snd);
  var wrap = api.mk("div","vg-fish-wrap"), cv = api.canvas(FA.W, FA.H); wrap.appendChild(cv);
  var msg = api.mk("p","vg-msg"); msg.setAttribute("aria-live","polite");
  left.appendChild(row); left.appendChild(wrap); left.appendChild(msg);
  var sideEl = api.mk("div","vg-mp-side");
  layout.appendChild(left); layout.appendChild(sideEl); ctx.box.appendChild(layout);
  ctx.side = pondSide(sideEl);
  var view = FA.view(cv), scene = new FA.Scene("dock:"+(A().roomId||"room"), "dock"), fx = new FA.Fx(), shadows = new FA.Shadows(scene, 5);
  var raf = 0, last = 0, pullHold = false, lastPull = 0, shakeUntil = 0, say = "", lastDraw = 0, sideBossAt = 0;
  var V = POND = {fx:fx, remote:{}, card:null, boss:null, errors:[], pulls:[], rtt:150, hpShown:null, bossEnds:0, slotX:{}, frames:[], lastToken:null};
  function clamp(v, a, b){ return v < a ? a : v > b ? b : v; }

  /* ---- who stands where: stable order by user id, up to 8; slots glide, never jump ---- */
  function players(){
    var l = (s.lobby||[]).slice().sort(function(a,b){ return String(a.userId).localeCompare(String(b.userId)); }).slice(0,8);
    if(me() && !l.some(function(p){ return p.userId === me(); })) l.push({userId:me(), displayName:"You"});
    return l;
  }
  function slots(){
    var l = players(), n = l.length, out = {};
    l.forEach(function(p, i){ out[p.userId] = {tx: n > 1 ? Math.round(24 + i*(272/(n-1))) : 160, p:p}; });
    Object.keys(out).forEach(function(u){ var x = V.slotX[u]; out[u].x = Math.round(x == null ? out[u].tx : x); });
    return out;
  }
  function glideSlots(dt){
    var sl = slots(), k = 1 - Math.exp(-dt/140);
    Object.keys(sl).forEach(function(u){
      var x = V.slotX[u], tx = sl[u].tx;
      if(x == null || api.calm() || Math.abs(tx - x) > 200) V.slotX[u] = tx;   // first sight / calm: place it
      else V.slotX[u] = Math.abs(tx - x) < 0.3 ? tx : x + (tx - x)*k;
    });
    Object.keys(V.slotX).forEach(function(u){ if(!sl[u]) delete V.slotX[u]; });
  }
  // Where a cast with this power lands: the same rule for me and for everyone watching me.
  function castTarget(uid, x0, pw){
    var r = api.rng(uid+":cast:"+pw);
    return {x: clamp(Math.round(x0 + 8 + (r()*2-1)*16), 12, 308), y: Math.round(128 - pw*38)};
  }

  /* ---- my line ---- */
  var rig = new FA.Rig({scene:scene, fx:fx, shadows:shadows, look:FA.looks(me()||"me"), me:true, dir:1, x:154, y:DECK_TOP, zone:36, treasure:false, serverLands:true,
    target: function(pw){ var sl = slots()[me()]; return castTarget(me()||"me", sl ? sl.tx : rig.o.x + 6, Math.round(pw*1000)/1000); },
    cast: function(pw, now){
      if(!send("pond","cast",{aim:Math.round(pw*1000)/1000})){ say = ctx.offline ? "Reconnecting to the Arena…" : "Not connected to the Arena yet."; rig.end("reeled", now); return; }
      s.mine = {pending:true, sentAt:now};
      say = "Casting…";
    },
    canLand: function(now){ var m = s.mine; return !!(m && m.token) && now - m.recvAt >= m.biteIn + MIN_REEL[m.rarity||4] + 250; },
    onResult: onRig});
  function onRig(kind, info){
    var now = performance.now(), m = s.mine;
    if(kind === "bite"){ say = "Bite! Press Space now."; if(!api.calm()) shakeUntil = now + 150; return; }
    if(kind === "hooked"){ if(m && m.token) send("pond","hook",{token:m.token}); say = s.boss ? "Hooked! The boss comes first: hold to pull together." : "Hooked! Hold to keep the fish in the green bar."; return; }
    if(kind === "land"){
      if(m && m.token){ send("pond","land",{token:m.token, perfect:!!(info.result && info.result.perfect), chest:!!(info.result && info.result.chest)}); m.tries = 1; m.landAt = now; }
      say = "Landing…"; return;
    }
    if(kind === "won") return;
    // spooked / missed / reeled / lost: tell the server, unless it told us
    if(!(info && info.server)){
      if(m && m.token) send("pond","lose",{token:m.token});
      else if(m && m.pending) m.cancelled = true;
    }
    if(!(m && m.pending && !m.token)) s.mine = null;
    say = kind === "spooked" ? "Too early, it got spooked." : kind === "reeled" ? (/^(Not connected|Reconnecting)/.test(say) ? say : "Reeled in.") : "It got away…";
  }
  V.onCast = function(m, now){
    var prev = s.mine;
    if(prev && prev.sentAt) V.rtt = V.rtt*0.7 + (now - prev.sentAt)*0.3;
    if(prev && prev.cancelled || (rig.phase !== "flying" && rig.phase !== "waiting")){ send("pond","lose",{token:m.token}); s.mine = null; return; }
    s.mine = {token:m.token, id:m.id, fish:m.fish, rarity:m.rarity|0, biteIn:m.biteIn|0, recvAt:now};
    V.lastToken = m.token;
    rig.arm({fish:m.fish, rarity:m.rarity|0, biteAt:now + (m.biteIn|0), token:m.token, chest:!!m.chest}, now);
    say = "Waiting for a bite… ignore the nibbles.";
  };
  V.onLoot = function(item){ V.loot = item; if(V.card && V.card.setLoot) V.card.setLoot(item); };
  V.onError = function(err, now){
    V.errors.push(err.slice(0,80));
    var m = s.mine;
    if(/^too fast/.test(err) && m && m.token && rig.phase === "sent"){
      if((m.tries|0) < 3){ setTimeout(function(){ if(s.mine === m && rig.phase === "sent"){ m.tries++; send("pond","land",{token:m.token, perfect:!!(rig.result && rig.result.perfect), chest:!!(rig.result && rig.result.chest)}); } }, 400); }
      return true;
    }
    if(err === "not yet") return true;     // a hook a hair early: cosmetic, the reel goes on
    if(/already have a line/.test(err)){
      // Only an older Arena says this (newer ones replace the old line and broadcast it as
      // lost). Let go of our last known line quietly, no lobby churn, and ask for a recast.
      if(m && m.pending){ s.mine = null; rig.end("reeled", now); }
      if(V.lastToken) send("pond","lose",{token:V.lastToken});
      say = "Your old line was still out. Reeled it in; cast again.";
      return true;
    }
    return false;
  };
  V.onResult = function(m, mine, now){
    var sl = slots(), uid = m.user && m.user.userId, slot = sl[uid];
    var it = api.items[m.fish], col = FA.RARITY_COL[(it||{}).rarity||1];
    if(mine){
      if(m.ev === "caught"){
        rig.finish(true, now);
        var tok = (s.mine && s.mine.token) || V.lastToken, u = api.rng(String(tok||m.fish+now))();
        var perfect = !!(rig.result && rig.result.perfect), size = FA.sizeFor(m.fish, u, perfect);
        say = "You caught "+(it?it.name:m.fish)+"! "+size.len+" cm, "+FA.fmtKg(size.kg)+" (+"+m.points+")";
        if(slot) fx.popup(slot.x, DECK_TOP - 10, m.fish, "+"+m.points, col);
        var loot = V.loot; V.loot = null;
        setTimeout(function(){ if(POND === V){ closeCard(); V.card = FA.card(wrap, {id:m.fish, size:size, stars:(perfect?1:0)+(size.big?1:0)+(rig.perfectCast?1:0), perfect:perfect, points:m.points, loot:loot || V.loot}, {onClose:function(){ V.card = null; cv.focus(); }}); V.loot = null; } }, 300);
      } else { rig.finish(false, now); }
      return;
    }
    var p = V.remote[uid]; if(!p) p = V.remote[uid] = {castN:0, phase:"idle"};
    if(m.ev === "caught"){
      p.cheerUntil = now + 900; p.arc = {fish:m.fish, x0:p.bob ? p.bob.x : (slot ? slot.x : 160), y0:p.bob ? p.bob.y : 100, at:now};
      if(slot) fx.popup(slot.x, DECK_TOP - 10, m.fish, "+"+m.points, col);
      if(p.bob) fx.ripple(p.bob.x, p.bob.y);
      if(it && it.rarity >= 3 && p.bob) fx.sparkle(p.bob.x, p.bob.y - 6);
    } else { p.slumpUntil = now + 800; if(p.bob) p.reelIn = {x:p.bob.x, y:p.bob.y, at:now}; }
    p.phase = "idle"; p.bob = null;
  };
  // Another angler cast (or, instant=true, was already out when we joined / reconnected).
  V.remoteCast = function(uid, now, instant, hooked){
    var sl = slots()[uid], p = V.remote[uid] = V.remote[uid] || {castN:0, phase:"idle"}, line = s.lines[uid] || {};
    p.castN++; p.id = line.id;
    var x0 = sl ? sl.tx : 160, r = api.rng(uid+":r:"+p.castN);
    var aim = typeof line.aim === "number" ? line.aim : 0.3 + r()*0.6;
    p.target = castTarget(uid, x0, aim);
    p.nib = r()*5000; p.at = now; p.phase = instant ? (hooked ? "hooked" : "wait") : "fly"; p.bob = instant ? p.target : null;
    if(hooked) p.hookAt = now - 1000;
  };
  V.remoteHooked = function(uid, now){
    var p = V.remote[uid]; if(!p || p.phase === "idle") { V.remoteCast(uid, now, true, true); p = V.remote[uid]; }
    if(p.phase === "fly"){ p.bob = p.target; }
    p.phase = "hooked"; p.hookAt = now;
    if(p.bob){ fx.splash(p.bob.x, p.bob.y, 6); fx.ripple(p.bob.x, p.bob.y, 120); }
  };
  // A full snapshot arrived (join, rejoin after a reconnect): rebuild every remote line.
  V.resync = function(now){
    var keep = {};
    Object.keys(s.lines).forEach(function(uid){
      if(uid === me()) return;
      keep[uid] = 1;
      var p = V.remote[uid], l = s.lines[uid];
      if(!p || p.phase === "idle") V.remoteCast(uid, now, true, l.hooked);
      else if(l.hooked && p.phase !== "hooked") V.remoteHooked(uid, now);
    });
    Object.keys(V.remote).forEach(function(uid){ var p = V.remote[uid]; if(!keep[uid] && p.phase !== "idle"){ p.phase = "idle"; p.bob = null; } });
    if(s.boss){ if(!V.boss) V.bossUp(now, true); V.bossSync(now); } else if(V.boss) V.boss = null;
  };

  /* ---- the boss: predicted from my pulls, eased toward the server ---- */
  V.bossUp = function(now, quiet){
    V.boss = {at:now, seed:Date.now(), hit:0}; V.hpShown = s.boss ? s.boss.hp : null; V.pulls = [];
    if(quiet) return;
    fx.splash(160, 108, 14); for(var i=0;i<4;i++) fx.ripple(160, 110, i*160, true); FA.sfx.play("boss");
  };
  V.bossSync = function(now){ if(s.boss){ V.bossEnds = now + (s.boss.left|0)*1000; V.hpAt = now; V.pulls = V.pulls.filter(function(t){ return t > now - V.rtt; }); } };
  V.bossHit = function(now){ if(V.boss) V.boss.hit = now; };
  V.bossDown = function(now, helped){
    var b = V.boss || {seed:Date.now()};
    V.boss = null; V.hpShown = null; V.bossArc = {at:now, x0:V.bossX||160, y0:V.bossY||106};
    FA.sfx.play("catch"); fx.confetti();
    if(helped) setTimeout(function(){ if(POND === V){ closeCard(); var size = FA.sizeFor("leviathan", api.rng("boss:"+b.seed)(), false);
      V.card = FA.card(wrap, {id:"leviathan", size:size, stars:2, title:"Group catch!", points:10}, {onClose:function(){ V.card = null; cv.focus(); }}); } }, 900);
  };
  V.bossGone = function(now){ V.boss = null; V.hpShown = null; V.bossDive = {at:now, x:V.bossX||160, y:V.bossY||106}; fx.ripple(V.bossX||160, (V.bossY||106)+6, 0, true); fx.ripple(V.bossX||160, (V.bossY||106)+6, 200, true); };
  function bossHpTarget(now){
    // Server HP, minus my pulls it can't have counted yet: a pull sent at t reaches the server
    // at about t + rtt/2, and that HP left the server about rtt/2 before we got it (at hpAt).
    var b = s.boss; if(!b) return 0;
    var dmg = 1 + 0.5*Math.max(0, (s.pulling|0) - 1), since = (V.hpAt||0) - V.rtt, pending = V.pulls.filter(function(t){ return t > since; }).length;
    return Math.max(0, b.hp - pending*dmg);
  }
  function pull(now){
    if(!send("pond","pull")) return;
    lastPull = now; V.pulls.push(now); if(V.pulls.length > 32) V.pulls.shift();
    if(V.boss) V.boss.jolt = now;
    if(!api.calm() && V.bossX != null) fx.splash(V.bossX + (Math.random()*30 - 15), (V.bossY||106) + 4, 2);
  }
  function closeCard(){ if(V.card){ V.card.close(true); V.card = null; } }

  /* ---- connection: drop / rejoin without freezing ---- */
  ctx.onOffline = function(){
    var now = performance.now();
    if(rig.phase !== "idle") rig.end("reeled", now, {server:true});
    s.mine = null; pullHold = false;
    Object.keys(V.remote).forEach(function(u){ V.remote[u].phase = "idle"; V.remote[u].bob = null; });
    say = "Connection lost. Reconnecting…";
  };
  ctx.onRejoin = function(){
    // The old socket may still hold our line on the server for a moment: let it go.
    if(V.lastToken) send("pond","lose",{token:V.lastToken});
    say = "Back on the dock.";
  };
  ctx.beforeLeave = function(){ var m = s.mine; if(m && m.token) send("pond","lose",{token:m.token}); };

  /* ---- input: Pointer Events (mouse, pen, touch) + Space ---- */
  function press(){
    if(ctx.paused || V.card) return;
    FA.sfx.unlock();
    var now = performance.now();
    pullHold = true;                             // held through a boss spawn = pulling
    if(s.boss){
      if(now - lastPull >= 160) pull(now);
      if(rig.phase === "bite") rig.down(now);   // still strike your own bite
      return;
    }
    rig.down(now);
  }
  function release(){ pullHold = false; if(!ctx.paused) rig.up(performance.now()); else rig.hold = false; }
  function quickCast(){ if(ctx.paused || V.card || s.boss || rig.phase !== "idle") return; FA.sfx.unlock(); rig.release(performance.now(), 0.7); }
  function kd(e){ if(e.key !== " " && e.key !== "Spacebar") return; e.preventDefault(); if(!e.repeat) press(); }
  function ku(e){ if(e.key !== " " && e.key !== "Spacebar") return; e.preventDefault(); release(); }
  function pd(e){ if(e.button != null && e.button !== 0 && e.pointerType === "mouse") return; e.preventDefault(); cv.focus(); try { cv.setPointerCapture(e.pointerId); } catch(x){} press(); }
  function pu(){ release(); }
  cv.addEventListener("keydown", kd); cv.addEventListener("keyup", ku); cv.addEventListener("pointerdown", pd);
  cv.addEventListener("pointerup", pu); cv.addEventListener("pointercancel", pu); cv.addEventListener("lostpointercapture", pu);
  window.addEventListener("pointerup", pu);

  /* ---- loop ---- */
  function step(now, dt){
    var m = s.mine;
    if(m && m.pending && !m.token && now - m.sentAt > 4000){ s.mine = null; if(rig.phase === "flying" || rig.phase === "waiting") rig.end("reeled", now); say = "No answer from the Arena. Cast again."; }
    if(rig.phase === "sent" && m && m.landAt && now - m.landAt > 5000){ rig.end("lost", now, {fish:rig.fish}); }
    if(s.boss && pullHold && now - lastPull >= 160) pull(now);
    // The server only notices a boss ran out when someone acts; past our own countdown we let
    // it go here so nobody is stuck waiting (the next cast makes the server say "bossgone").
    if(s.boss && V.bossEnds && now > V.bossEnds + 1500){
      s.boss = null; s.pulling = 0; V.bossGoneLocal = true; pondFeed(s, "The Leviathan slipped away…"); V.bossGone(now); ctx.side.update(s);
    }
    rig.frozen = !!s.boss;
    rig.update(now, dt);
    glideSlots(dt);
    var sl = slots();
    Object.keys(V.remote).forEach(function(uid){
      var p = V.remote[uid];
      if(p.phase === "fly" && now - p.at >= 450){ p.phase = "wait"; p.bob = p.target; fx.splash(p.bob.x, p.bob.y, 5); }
      if(!sl[uid] && p.phase === "idle" && !p.arc) delete V.remote[uid];
    });
    if(s.boss){
      var tgt = bossHpTarget(now);
      if(V.hpShown == null || api.calm()) V.hpShown = tgt;
      else V.hpShown += (tgt - V.hpShown)*(1 - Math.exp(-dt/90));
    }
  }
  var BUBBLE_T = 700;
  function drawRemote(g, uid, sx, p, now, night, bossMouth){
    var calm = api.calm(), hooked = p.phase === "hooked";
    var frame = now < (p.cheerUntil||0) ? "cheer" : now < (p.slumpUntil||0) ? "slump" : ((hooked || (bossMouth && (s.pulling|0) > (pullHold ? 1 : 0))) && Math.floor(now/120)%2) ? "reel" : "idle";
    var hand = FA.drawAngler(g, sx - 6, DECK_TOP, {frame:frame, look:FA.looks(uid)});
    var tip = FA.drawRod(g, hand, bossMouth ? 15 : p.phase === "idle" ? 25 : hooked ? 20 + (calm ? 0 : Math.sin(now/160)*6) : 35, 1);
    if(bossMouth) FA.drawLine(g, tip.x, tip.y, bossMouth.x, bossMouth.y, 2, night);
    else if(p.phase === "fly"){
      var u = clamp((now - p.at)/450, 0, 1), x = tip.x + (p.target.x - tip.x)*u, y = tip.y + (p.target.y - tip.y)*u - 40*Math.sin(Math.PI*u);
      FA.drawLine(g, tip.x, tip.y, x, y, 4, night); FA.drawBobber(g, x, y, 0, true);
    } else if((p.phase === "wait" || hooked) && p.bob){
      var jx = hooked && !calm ? Math.round(Math.sin(now/90 + p.nib)*2) : 0;
      var nib = hooked ? 3 : (!calm && Math.sin((now + p.nib)/650) > 0.93 ? 2 : (!calm && Math.sin(now/500 + p.nib) > 0 ? 1 : 0));
      FA.drawLine(g, tip.x, tip.y, p.bob.x + jx, p.bob.y - 3, hooked ? 2 : 18, night); FA.drawBobber(g, p.bob.x + jx, p.bob.y, nib);
      if(hooked && !calm && Math.floor(now/260)%3 === 0) fx.ripple(p.bob.x, p.bob.y);
    }
    if(hooked && now - (p.hookAt||0) < BUBBLE_T) FA.label(g, "!", sx, DECK_TOP - 12, "#ffd75a");
    if(p.reelIn){ var v = (now - p.reelIn.at)/300; if(v >= 1) p.reelIn = null; else FA.drawBobber(g, p.reelIn.x + (tip.x - p.reelIn.x)*v, p.reelIn.y + (tip.y - p.reelIn.y)*v, 0, true); }
    if(p.arc){ var w = (now - p.arc.at)/400; if(w >= 1){ fx.sparkle(hand.x, hand.y - 4); p.arc = null; } else FA.drawFish(g, p.arc.fish, p.arc.x0 + (hand.x - p.arc.x0)*w, p.arc.y0 + (hand.y - 6 - p.arc.y0)*w - 30*Math.sin(Math.PI*w), 1, false, now); }
  }
  function drawGoal(g){
    var goal = s.goal|0, tgt = s.goalTarget||20, txt = "ROOM GOAL "+goal+"/"+tgt, tw = FA.textWidth(txt), cells = Math.min(20, tgt), cw = 6;
    var x0 = Math.round(160 - (tw + 6 + cells*(cw+1))/2);
    g.fillStyle = "rgba(10,14,24,0.72)"; g.fillRect(x0-3, 1, tw + 6 + cells*(cw+1) + 5, 9);
    FA.text(g, txt, x0, 3, "#ffffff");
    for(var i=0;i<cells;i++){
      var cx = x0 + tw + 6 + i*(cw+1), lit = i < Math.round(goal*cells/tgt);
      g.fillStyle = lit ? "#6fd36a" : "#2a3a48"; g.fillRect(cx, 3, cw, 5);
      if(lit){ g.fillStyle = "#2f6f3a"; g.fillRect(cx+1, 5, 3, 1); g.fillRect(cx+4, 4, 1, 3); }
    }
  }
  function bossLeft(now){ return Math.max(0, Math.ceil((V.bossEnds - now)/1000)); }
  function drawBoss(g, now, calm){
    var b = s.boss, jolt = V.boss && (now - V.boss.hit < 90 || now - (V.boss.jolt||0) < 70) ? 1 : 0;
    var bx = Math.round(160 + (calm ? 0 : Math.sin(now/900)*50)), by = Math.round(106 + (calm ? 0 : Math.sin(now/400)*2)) + jolt;
    V.bossX = bx; V.bossY = by;
    var facing = calm ? true : Math.cos(now/900) > 0;   // moving right -> faces right
    g.fillStyle = "rgba(0,0,0,0.28)";
    for(var y=0;y<14;y++){ var f = 1 - Math.pow((y-6.5)/7, 2), w = Math.round(48*Math.sqrt(Math.max(0, f))); g.fillRect(bx - Math.round(w/2), by + 8 + y - 7, w, 1); }
    FA.drawFish(g, "leviathan", bx, by, 4, !facing, now);
    var mouth = {x: bx + (facing ? 30 : -30), y: by + 2};
    // HP bar (eased) + local countdown + together badge
    var hx = 100, hy = 14, hp = V.hpShown != null ? V.hpShown : b.hp, frac = clamp(hp/(b.max||1), 0, 1);
    g.fillStyle = "rgba(10,14,24,0.72)"; g.fillRect(hx-2, hy-2, 124 + 18, 10);
    g.fillStyle = "#2a1838"; g.fillRect(hx, hy, 120, 6);
    g.fillStyle = "#7a3fb0"; g.fillRect(hx, hy, Math.round(120*frac), 6);
    g.fillStyle = "rgba(255,255,255,0.7)"; g.fillRect(hx, hy, Math.round(120*frac), 1);
    for(var k=1;k<8;k++){ g.fillStyle = "rgba(10,14,24,0.5)"; g.fillRect(hx + k*15, hy, 1, 6); }
    FA.text(g, bossLeft(now)+"S", hx + 123, hy, "#ffffff");
    var mult = 1 + 0.5*Math.max(0, (s.pulling|0) - 1);
    if((s.pulling|0) >= 2) FA.label(g, "X"+mult+" TOGETHER", 160, 26, "#f2d14b");
    return mouth;
  }
  function draw(now){
    var g = view.g, calm = api.calm(), night;
    scene.boss = s.boss ? 1 : 0;
    scene.drawBack(g, now); night = scene.night();
    if(!s.boss) shadows.draw(g, now);
    var mouth = s.boss ? drawBoss(g, now, calm) : null;
    if(V.bossDive){ var d = (now - V.bossDive.at)/900; if(d >= 1) V.bossDive = null; else { g.globalAlpha = 1 - d; FA.drawFish(g, "leviathan", V.bossDive.x, V.bossDive.y + d*30, 4, false, now); g.globalAlpha = 1; } }
    var sl = slots(), myId = me();
    Object.keys(sl).forEach(function(uid){
      var x = sl[uid].x;
      if(uid === myId){
        rig.o.x = x - 6; rig.o.y = DECK_TOP;
        if(mouth && (rig.phase === "idle" || rig.phase === "reeling")){
          var fr = pullHold && Math.floor(now/120)%2 ? "reel" : "idle";
          var hand = FA.drawAngler(g, x - 6, DECK_TOP, {frame:fr, look:rig.o.look, me:true});
          var tip = FA.drawRod(g, hand, pullHold ? 10 : 18, 1);
          FA.drawLine(g, tip.x, tip.y, mouth.x, mouth.y, pullHold ? 1 : 3, night);
        } else rig.draw(g, now, scene);
      } else drawRemote(g, uid, x, V.remote[uid] || {phase:"idle"}, now, night, mouth);
    });
    if(V.bossArc){ var u = (now - V.bossArc.at)/900; if(u >= 1) V.bossArc = null; else if(calm){ g.globalAlpha = 1 - u; FA.drawFish(g, "leviathan", V.bossArc.x0, V.bossArc.y0, 4, false, now); g.globalAlpha = 1; } else FA.drawFish(g, "leviathan", V.bossArc.x0 + (160 - V.bossArc.x0)*u, V.bossArc.y0 + (140 - V.bossArc.y0)*u - 70*Math.sin(Math.PI*u), 4, false, now); }
    fx.draw(g, now);
    scene.drawFront(g, now);
    Object.keys(sl).forEach(function(uid){
      var nm = uid === myId ? "YOU" : nameOf(sl[uid].p).replace(/[^A-Za-z0-9 !+\-\/:.?']/g, "").slice(0, 8) || "?";
      FA.label(g, nm, sl[uid].x, DECK_TOP + 21, uid === myId ? "#ffd75a" : "#ffffff");
    });
    drawGoal(g);
    var mySlot = sl[myId] ? sl[myId].x : 160;
    rig.drawUI(g, now, mySlot > 160 ? 8 : FA.W - 52);
    if(ctx.paused) FA.label(g, "PAUSED", 160, 70, "#f2d14b", 2);
    else if(ctx.offline) FA.label(g, "RECONNECTING...", 160, 70, "#f2d14b");
    view.blit(now < shakeUntil ? {x: Math.floor(now/30)%2 ? 2 : -2, y:0} : null);
    if(V.card && !calm) V.card.draw(now);
  }
  function loop(t){
    if(!ctx.alive) return;
    var now = performance.now(), dt = last ? Math.min(50, now - last) : 16;
    // Paused (a tab needs you): keep a still frame, redrawn at most 4 times a second.
    if(ctx.paused && now - lastDraw < 250){ raf = requestAnimationFrame(loop); return; }
    var t0 = performance.now();
    last = now;
    if(!ctx.paused) step(now, dt);
    shadows.update(dt); fx.update(dt);
    draw(now);
    lastDraw = now;
    var m = s.mine, txt = ctx.paused ? (m ? "Paused: your line is still in the water." : "Paused") : ctx.offline ? "Connection lost. Reconnecting…" : s.boss ? (rig.phase === "idle" || rig.phase === "reeling" ? "Boss! Hold Space or the mouse to reel it in together." : say) : say || (rig.phase === "idle" ? "Hold Space or the mouse to charge a cast, release to cast." : "");
    if(msg.textContent !== txt) msg.textContent = txt;
    var np = players().length, lab = "Shared pond dock with "+np+" angler"+(np === 1 ? "" : "s")+". "+(s.boss ? "Boss fish: hold Space to pull." : rig.phase === "reeling" ? "Reeling: hold Space to raise the green bar." : rig.phase === "bite" ? "Bite! Press Space." : "Hold Space to cast.");
    if(cv.getAttribute("aria-label") !== lab) cv.setAttribute("aria-label", lab);
    var dis = !!s.boss || rig.phase !== "idle" || !!ctx.offline; if(castBtn.disabled !== dis) castBtn.disabled = dis;
    if(s.boss && now - sideBossAt >= 1000){ sideBossAt = now; ctx.side.boss(s, bossLeft(now), V.hpShown != null ? V.hpShown : s.boss.hp); }
    if(dbg){
      V.frames.push(performance.now() - t0); if(V.frames.length > 600) V.frames.shift();
      var r = rig.reel; window.__hqDock = {phase:rig.phase, progress:r ? r.progress : 0, fishY:r ? r.fishY : 0, barY:r ? r.barY : 0, barV:r ? r.barV : 0, zone:r ? r.zone : 0, inside:r ? r.inside : false,
      fish:rig.fish, mine:s.mine ? {token:!!s.mine.token, pending:!!s.mine.pending, tries:s.mine.tries|0} : null, nibbling:rig.phase === "waiting" && rig.nibbling(now),
      remote:Object.keys(V.remote).map(function(u){ return V.remote[u].phase; }), players:players().length, goal:s.goal|0, boss:s.boss ? s.boss.hp : null, hpShown:V.hpShown, pulling:s.pulling|0,
      pullHold:pullHold, frozen:!!rig.frozen, card:!!V.card, errors:V.errors.slice(-5), renders:scene.renders, offline:!!ctx.offline, rtt:Math.round(V.rtt),
      slotX:JSON.parse(JSON.stringify(V.slotX)), feedMut:ctx.side.mut, drawMs:V.frames.slice(-120)}; }
    raf = requestAnimationFrame(loop);
  }
  raf = requestAnimationFrame(loop);
  ctx.stop = function(){
    cancelAnimationFrame(raf); closeCard();
    window.removeEventListener("pointerup", pu);
    if(POND === V) POND = null;
    s.mine = null; try { delete window.__hqDock; } catch(e){}
  };
  ctx.side.update(s);
});

/* =============================== RACE =============================== */
HANDLERS.race = {
  on: function(m, s){
    if(m.ev === "start"){ s.round = m.round; s.rows = []; s.cur = ""; s.others = {}; s.winner = null; s.word = null; s.until = Date.now() + (m.secs|0)*1000; if(m.by) api.toast("🧩 "+nameOf(m.by)+" started a puzzle race!"); }
    if(m.ev === "mark" && m.round === s.round){ s.rows.push({word:m.word, marks:m.marks}); }
    if(m.ev === "progress" && m.user && m.user.userId !== me()){ s.others[nameOf(m.user)] = {n:m.n, hits:m.hits}; }
    if(m.ev === "win"){ s.winner = m.user; s.word = m.word; s.round = null; if(m.user && m.user.userId === me()) api.inv.add("quartz", 1); }
    if(m.ev === "timeout"){ s.word = m.word; s.round = null; }
  },
  render: function(ctx, s){ renderRace(ctx, s); }
};
function renderRace(ctx, s){
  var box = ctx.box; box.textContent = "";
  if(!s.round){
    if(s.winner) box.appendChild(api.mk("p",null,"🏆 "+(s.winner.userId===me()?"You":nameOf(s.winner))+" won — the word was "+String(s.word||"").toUpperCase()));
    else if(s.word) box.appendChild(api.mk("p",null,"Time's up — the word was "+String(s.word).toUpperCase()));
    box.appendChild(api.btn("Start a race","primary",function(){ send("race","start"); }));
    box.appendChild(api.mk("p","vg-muted","Everyone in the lobby races for the same word. The server marks guesses; others only see how many letters you've got right."));
    return;
  }
  var left = Math.max(0, Math.round((s.until - Date.now())/1000));
  box.appendChild(api.mk("b",null,"Round "+s.round+" · "+left+"s left"));
  var board = api.mk("div","vg-wordle"); board.tabIndex = 0; board.setAttribute("aria-label","Race board. Type and press Enter.");
  for(var r=0;r<6;r++){
    var row = api.mk("div","vg-wrow"), g = s.rows[r], live = !g && r === s.rows.length;
    for(var c=0;c<5;c++){ var ch = g ? g.word[c] : live ? (s.cur[c]||"") : ""; row.appendChild(api.mk("span","vg-tile"+(g?" "+g.marks[c]:""), ch.toUpperCase())); }
    board.appendChild(row);
  }
  board.addEventListener("keydown", function(e){
    if(e.metaKey||e.ctrlKey||e.altKey) return;
    if(e.key==="Enter"){ e.preventDefault(); if(s.cur.length===5){ send("race","guess",{round:s.round, word:s.cur}); s.cur=""; } }
    else if(e.key==="Backspace"){ e.preventDefault(); s.cur = s.cur.slice(0,-1); renderRace(ctx, s); }
    else if(/^[a-zA-Z]$/.test(e.key) && s.cur.length<5){ e.preventDefault(); s.cur += e.key.toLowerCase(); renderRace(ctx, s); }
  });
  box.appendChild(board);
  var others = Object.keys(s.others||{});
  box.appendChild(api.mk("p","vg-muted", others.length ? others.map(function(n){ return n+": "+s.others[n].n+" guesses, "+s.others[n].hits+"/5 right"; }).join(" · ") : "Waiting for others' guesses…"));
  setTimeout(function(){ if(ctx.alive && document.activeElement && document.activeElement.tagName!=="SELECT" && document.activeElement.tagName!=="INPUT") board.focus(); }, 0);
}
register("race", "🏁", "Live puzzle race against the room", function(ctx){
  var s = st("race"); renderRace(ctx, s);
  var t = setInterval(function(){ if(s.round) renderRace(ctx, s); }, 1000); ctx.stop = function(){ clearInterval(t); };
});

/* =============================== DUEL =============================== */
// Real-Pokemon duel (games/pokebattle.js draws it). We send only {species, stage, branch,
// mega, shiny, name} per creature and a move/switch choice per turn; the server derives
// every stat and move, resolves both choices at once and sends back the ordered events
// plus an authoritative snapshot.
// Smoothness rules: one battle scene per match that is never rebuilt (only updated);
// your pick shows instantly (local echo) and can be changed until the foe is in; turns
// queue and play in order, and a backgrounded tab (or a backlog > 2 turns) snaps straight
// to the latest snapshot; stale/duplicate turns (by seq) are dropped; countdowns run on
// the local clock from the server's remaining-ms and the waiting side 'poke's the server
// when one runs out; a dropped socket rejoins and resyncs from a full snapshot.
function myDuelTeam(){
  // the team saved in the team builder (unlocked Pokemon), else the live session creatures
  var saved = HQV.pk && HQV.pk.savedTeam ? HQV.pk.savedTeam() : null;
  if(saved) return saved.map(function(m){ return {sp: m.sp, st: m.st, br: m.br, mg: m.mg, sh: m.sh, name: ""}; });
  var team = typeof window.gymTeam === "function" ? window.gymTeam() : [];
  return team.slice(0,6).map(function(m){
    var cr = m.cr || {}, br = null, mg = null;
    try { br = window.branchFinalDex(cr); } catch(e){}
    try { mg = window.pokeMega(cr); } catch(e){}
    return {sp: typeof window.pokeIdx === "function" ? window.pokeIdx(cr) : 0,
            st: typeof window.creatureStage === "function" ? Math.max(0, Math.min(4, window.creatureStage(cr)|0)) : 2,
            br: br, mg: mg, sh: !!cr.shiny, name: String(m.name||"").slice(0,24)};
  });
}
function duelClock(s, d){
  // Deadlines in local time, from the server's "ms left" at the moment the message arrived.
  var t = performance.now();
  s.deadlineAt = t + (d.deadline_ms != null ? d.deadline_ms : (d.deadline_in|0)*1000);
  s.awayAt = {}; var aw = d.away || {}; for(var u in aw) s.awayAt[u] = t + (aw[u]|0);
}
function duelRecord(m){
  var d = m.duel; if(!d || (d.ids||[]).indexOf(me()) < 0 || !m.winner) return;
  var b = api.save.battle;
  if(m.winner.userId === me()) b.wins = (b.wins|0)+1; else b.losses = (b.losses|0)+1;
  api.persist();
}
HANDLERS.duel = {
  on: function(m, s){
    s.queue = s.queue || [];
    if(m.ev === "challenge"){ s.incoming = m.from; api.toast("⚔️ "+nameOf(m.from)+" challenged you to a duel!"); }
    else if(m.ev === "challenged"){ s.sent = m.to; }
    else if(m.ev === "declined"){ s.sent = null; api.toast(nameOf(m.by)+" declined your duel"); }
    else if(m.ev === "duel"){
      var d = m.duel;
      var c = CTX.duel, live = c && c.scene && !c.scene.dead ? c : null;
      if(!d){                                                    // no match (any more)
        if(!s.duel || s.queue.some(function(q){ return q.end; })) return;
        // It ended while our socket was down and we never heard the ending: close the battle definitively.
        if(live) s.queue.push({end:{duel:null, winner:undefined, missed:true}});
        else { s.duel = null; s.queue = []; s.live = null; }
        return;
      }
      if(s.duel && s.duel.mid === d.mid){ s.queue.push({snap:d}); }      // a resync (rejoin/sync): apply after queued turns
      else {
        // A different match: any scene still showing an older one goes; renderDuel mounts the new one.
        if(live && live.mid !== d.mid){ clearInterval(live.duelTimer); live.scene.destroy(); live.scene = null; live.lobbySig = null; live.playing = false; }
        s.queue = []; s.ended = null; s.seq = 0; s.live = null; s.fresh = !!(s.sent || s.incoming);   // intro only for a match we just started
      }
      s.incoming = null; s.sent = null; s.duel = d; duelClock(s, d);
      if(HQV.pk && HQV.pk.preload){ var ids = d.ids||[]; ids.forEach(function(u){ var sd = d.sides[u]; if(sd) HQV.pk.preload(sd.team); }); }
    }
    else if(m.ev === "waiting"){ if(s.duel && m.mid === s.duel.mid && m.turn === s.duel.turn) s.duel.waiting = m.waiting || []; }
    else if(m.ev === "turn"){
      if(!s.duel || m.mid !== s.duel.mid || !m.duel || (m.seq|0) <= (s.seq|0)) return;        // stale or duplicate
      s.seq = m.seq|0; s.queue.push({events: Array.isArray(m.events) ? m.events : [], duel: m.duel});
      s.duel = m.duel; duelClock(s, m.duel);
    }
    else if(m.ev === "away" && s.duel && m.mid === s.duel.mid){ s.awayAt = s.awayAt || {}; s.awayAt[m.user] = performance.now() + (m.ms|0); }
    else if(m.ev === "back" && s.duel && m.mid === s.duel.mid){ if(s.awayAt) delete s.awayAt[m.user]; }
    else if(m.ev === "late"){ s.late = m.turn; send("duel","sync"); }       // our pick missed the turn: resync
    else if(m.ev === "duelend"){
      if(s.duel && m.duel && m.duel.mid !== s.duel.mid) return;
      if(s.queue.some(function(q){ return q.end; })) return;            // already ending (e.g. a replay after a missed one)
      duelRecord(m);
      var cl = CTX.duel;
      if(s.duel && cl && cl.scene && !cl.scene.dead) s.queue.push({end:m});
      else if(s.duel && cl && cl.alive && !cl.scene) s.queue.push({end:m});     // the scene is still loading
      else { s.ended = m; s.queue = []; s.duel = null; }                                // no battle on screen: just show the result
    }
  },
  render: function(ctx, s){ renderDuel(ctx, s); }
};
function copyDuel(d){ return JSON.parse(JSON.stringify(d)); }
// The server's {sides:{uid:...}} -> the scene's {me, foe}, from my seat (spectators sit with a).
function duelSeat(d){ var i = (d.ids||[]).indexOf(me()); return i < 0 ? 0 : i; }
function duelView(d){
  var seat = duelSeat(d), ids = d.ids || [];
  return {me: copyDuel(d.sides[ids[seat]]), foe: copyDuel(d.sides[ids[1-seat]])};
}
function duelName(d, uid){ return uid === d.a.userId ? nameOf(d.a) : nameOf(d.b); }
function duelWho(d){
  var seat = duelSeat(d), foe = seat === 0 ? d.b : d.a;
  var playing = (d.ids||[]).indexOf(me()) >= 0;
  return function(side, name, sendOut){
    if(side === seat) return sendOut ? (playing ? "Go! "+name : nameOf(seat === 0 ? d.a : d.b)+" sent out "+name) : name;
    return sendOut ? nameOf(foe)+" sent out "+name : "The foe's "+name;
  };
}
function duelLobbySig(s){
  return JSON.stringify([s.incoming && s.incoming.userId, s.sent && s.sent.userId, (s.lobby||[]).map(function(p){ return p.userId; }),
    s.ended ? [s.ended.winner && s.ended.winner.userId, !!s.ended.forfeit] : 0, !!(HQV.pk && HQV.pk.data())]);
}
function renderDuel(ctx, s){
  var box = ctx.box, d = s.duel;
  // A battle in progress (or still playing its last turn) owns the box and is only updated.
  if(ctx.scene && !ctx.scene.dead){ pumpDuel(ctx, s); return; }
  if(d && !s.ended){ ctx.building = false; startDuelScene(ctx, s, d); return; }
  if(ctx.building) return;                  // the team builder owns the box until Save/Cancel
  var sig = duelLobbySig(s);
  if(ctx.lobbySig === sig) return;          // nothing on this screen changed: keep the DOM
  ctx.lobbySig = sig; box.textContent = "";
  if(s.incoming){
    var inc = api.mk("div","vg-row"); inc.appendChild(api.mk("b",null,nameOf(s.incoming)+" challenged you!"));
    inc.appendChild(api.btn("Accept","primary",function(){ var t = myDuelTeam(); if(!t.length){ api.toast("Pick a team, or have a working or idle session creature"); return; } send("duel","accept",{team:t}); }));
    inc.appendChild(api.btn("Decline","ghost",function(){ send("duel","decline"); s.incoming = null; renderDuel(ctx, s); }));
    box.appendChild(inc);
  }
  if(s.ended && s.ended.winner !== undefined) box.appendChild(api.mk("p",null, s.ended.winner ? "🏆 "+(s.ended.winner.userId===me()?"You won":nameOf(s.ended.winner)+" won")+(s.ended.forfeit?" (forfeit)":"")+"." : "It's a draw."));
  if(s.sent) box.appendChild(api.mk("p","vg-muted","Challenge sent to "+nameOf(s.sent)+". Waiting for them to accept…"));
  var others = (s.lobby||[]).filter(function(p){ return p.userId !== me(); });
  if(!others.length) box.appendChild(api.mk("p","vg-muted","Invite someone into this lobby to duel."));
  others.forEach(function(p){ var r = api.mk("div","vg-row"); r.appendChild(api.mk("span",null,nameOf(p)));
    r.appendChild(api.btn("Challenge","",function(){ var t = myDuelTeam(); if(!t.length){ api.toast("Pick a team, or have a working or idle session creature"); return; }
      if(send("duel","challenge",{to:p.userId, team:t})){ s.sent = p; renderDuel(ctx, s); } }));
    box.appendChild(r); });
  var saved = HQV.pk && HQV.pk.savedTeam ? HQV.pk.savedTeam() : null;
  var tr = api.mk("div","vg-row");
  tr.appendChild(api.mk("span","vg-muted", saved ? "Your team: "+saved.map(function(m){ return HQV.pk.buildMon(m).name; }).join(", ")+"."
    : "Your team is your working and idle session creatures, battling as the Pokémon they are now."));
  if(HQV.pk && HQV.pk.teamBuilder) tr.appendChild(api.btn(saved ? "Edit team" : "Pick a team","",function(){
    ctx.building = true; box.textContent = "";
    HQV.pk.teamBuilder(box, {onDone: function(){ ctx.building = false; ctx.lobbySig = null; renderDuel(ctx, s); }});
  }));
  box.appendChild(tr);
  box.appendChild(api.mk("p","vg-muted","Real moves, stats and types. Both of you choose each turn; the Arena server resolves it."));
  if(HQV.pk && HQV.pk.loadUnlocked && !HQV.pk.unlocked() && HQV.pk.hasSavedTeam()) HQV.pk.loadUnlocked().then(function(){ if(ctx.alive && !ctx.scene && !ctx.building){ ctx.lobbySig = null; renderDuel(ctx, s); } });
}
function startDuelScene(ctx, s, d){
  var P = HQV.pk, box = ctx.box;
  if(!P || !P.data()){ if(ctx.lobbySig !== "loading"){ ctx.lobbySig = "loading"; box.textContent = ""; box.appendChild(api.mk("p","vg-muted","Loading battle data…")); } return; }
  box.textContent = ""; ctx.lobbySig = null;
  var playing = (d.ids||[]).indexOf(me()) >= 0;
  var head = api.mk("div","vg-row"); head.appendChild(api.mk("b",null, nameOf(d.a)+" vs "+nameOf(d.b)+(playing ? "" : " · watching")));
  box.appendChild(head);
  ctx.mid = d.mid; ctx.playing = false; ctx.acted = null; ctx.poked = 0;
  s.live = copyDuel(d);
  var sc = ctx.scene = new P.Scene(box, {
    runLabel: playing ? "FORFEIT" : "LEAVE",
    onMove: function(i){ duelAct(ctx, s, {k:"move", i:i}); },
    onSwitch: function(i){ duelAct(ctx, s, {k:"switch", to:i}); },
    onRun: function(){
      if(!playing) return;
      sc.setMode("over", [{label:"Yes, forfeit", fn:function(){ send("duel","forfeit"); sc.setMode("wait", "Forfeiting…"); }},
                          {label:"Keep battling", primary:true, fn:function(){ sc.setMode("main"); }}]);
      sc.text.textContent = "Forfeit this duel?";
    }
  });
  if(ctx.paused) sc.pause();
  sc.setView(duelView(d));
  // Drop queued turns the snapshot already contains (a mid-match join or refresh).
  s.queue = (s.queue||[]).filter(function(q){ return q.end || (q.duel && q.duel.turn > d.turn); });
  clearInterval(ctx.duelTimer);
  ctx.duelTimer = setInterval(function(){ duelPrompt(ctx, s); }, 250);
  var oldStop = ctx.stop;
  ctx.stop = function(){ clearInterval(ctx.duelTimer); if(ctx.scene) ctx.scene.destroy(); ctx.scene = null; if(oldStop) oldStop(); };
  if(s.fresh && !document.hidden){
    s.fresh = false;
    var w = duelWho(d), seat = duelSeat(d), ids = d.ids;
    var foeMon = d.sides[ids[1-seat]].team[d.sides[ids[1-seat]].active], myMon = d.sides[ids[seat]].team[d.sides[ids[seat]].active];
    ctx.playing = true;
    sc.intro([w(1-seat, foeMon.name, true)+"!", w(seat, myMon.name, true)+"!"]).then(function(){ ctx.playing = false; pumpDuel(ctx, s); });
  } else { s.fresh = false; sc.log("Rejoined the duel on turn "+d.turn+"."); pumpDuel(ctx, s); }
}
function duelAct(ctx, s, a){
  var d = s.live, sc = ctx.scene; if(!d || ctx.playing || !sc || sc.busy) return;
  if(!send("duel","act",{turn:d.turn, a:a})){ api.toast("Reconnecting… try again in a moment"); return; }
  // Local echo: show the pick right away; the server's answer (a turn) comes later.
  var me0 = d.sides[me()], mon = me0 && me0.team[me0.active], M = HQV.pk.data().moves, label;
  if(a.k === "move" && mon && mon.moves[a.i]) label = (M[mon.moves[a.i].id]||{}).name || "a move";
  else if(a.k === "switch" && me0 && me0.team[a.to]) label = "to switch to "+me0.team[a.to].name;
  ctx.acted = {key: d.turn+":"+d.phase, label: label || "your move", sock: A().sock}; ctx.changing = null;
  duelPrompt(ctx, s);
}
// One queued item at a time, in order: animated turns, resync snapshots, the ending.
function pumpDuel(ctx, s){
  var sc = ctx.scene;
  if(ctx.playing || !sc || sc.dead) return;
  var q = s.queue || [];
  if(q.length && (document.hidden || q.filter(function(x){ return x.events; }).length > 2)){
    // Backgrounded or behind: no animations, log the lines and jump to the newest snapshot.
    var last = null;
    while(q.length && !q[0].end){
      var it = q.shift(), d0 = it.duel || it.snap;
      if(it.events){ var who0 = duelWho(d0); it.events.forEach(function(ev){ var l = HQV.pk.text(ev, who0); if(l) sc.log(l); }); }
      last = d0;
    }
    if(last){ s.live = copyDuel(last); ctx.acted = null; sc.setView(duelView(last)); }
  }
  var item = q.shift();
  if(!item){ duelPrompt(ctx, s); return; }
  if(item.snap){
    // A resync snapshot can overtake nothing on the same socket: if my pick for this very turn
    // went out on it after the join, the snapshot predates it, so the pick still stands. A pick
    // sent on an older socket may be lost, so then the menu comes back.
    var ak = item.snap.turn+":"+item.snap.phase;
    if(!(ctx.acted && ctx.acted.key === ak && ctx.acted.sock === A().sock)) ctx.acted = null;
    s.live = copyDuel(item.snap); sc.setView(duelView(item.snap)); return pumpDuel(ctx, s);
  }
  if(item.end){
    var m = item.end; s.ended = m; s.duel = null; ctx.playing = true; s.queue = [];
    sc.cmd.removeAttribute("data-left");
    var line = m.missed ? "The duel ended while you were offline." : !m.winner ? "It's a draw!" : m.winner.userId === me() ? (m.left ? "Your opponent didn't come back. You win!" : m.timeout ? "Your opponent stopped choosing. You win!" : m.forfeit ? "Your opponent forfeited. You win!" : "You won the duel!") :
      (m.duel && (m.duel.ids||[]).indexOf(me()) >= 0 ? (m.timeout ? "You ran out of time twice, so you forfeited." : m.left ? "You were offline too long, so you forfeited." : m.forfeit ? "You forfeited." : "You lost the duel.") : nameOf(m.winner)+" won the duel!");
    sc.setMode("busy");
    sc.say(line, 600).then(function(){
      ctx.playing = false;
      if(sc.dead) return;
      sc.setMode("over", [{label:"Back to the lobby", primary:true, fn:function(){ clearInterval(ctx.duelTimer); sc.destroy(); ctx.scene = null; ctx.lobbySig = null; renderDuel(ctx, s); }}]);
    });
    return;
  }
  ctx.playing = true;
  var d = item.duel, seat = duelSeat(d);
  sc.play(item.events, seat, duelWho(d)).then(function(){
    ctx.playing = false;
    s.live = copyDuel(d); ctx.acted = null;
    if(!sc.dead){ sc.setView(duelView(d)); pumpDuel(ctx, s); }     // authoritative snap after the animation
  });
}
function duelPrompt(ctx, s){
  var sc = ctx.scene, d = s.live;
  if(!sc || sc.dead || ctx.playing || !d || (s.queue && s.queue.length) || sc.mode === "over" || sc.busy) return;
  var cur = s.duel && s.duel.mid === d.mid && s.duel.turn === d.turn ? s.duel : d;
  var playing = (d.ids||[]).indexOf(me()) >= 0, waiting = cur.waiting || [], now = performance.now();
  // The visible timer runs out a second before the server's deadline (which also allows slack_ms),
  // so a pick made at "1s" always lands in time.
  var slack = cur.slack_ms != null ? cur.slack_ms|0 : 2000;
  var key = d.turn+":"+d.phase, left = Math.max(0, Math.ceil(((s.deadlineAt||now) - 1000 - now)/1000));
  // Someone dropped: their grace countdown replaces the turn timer.
  var away = Object.keys(s.awayAt||{}).filter(function(u){ return u !== me(); }), awayLeft = 0;
  away.forEach(function(u){ awayLeft = Math.max(awayLeft, Math.ceil((s.awayAt[u]-now)/1000)); });
  // Countdown over (turn timer or someone's grace): ask the server to enforce it, every 3 s until it does.
  var due = (s.deadlineAt && now >= s.deadlineAt + slack + 250) || (away.length && awayLeft <= 0);
  if(due && sockOpen() && now - (ctx.poked||0) > 3000){ ctx.poked = now; send("duel","poke"); }
  var mine = playing && (waiting.indexOf(me()) >= 0 || ctx.changing === key) && !(ctx.acted && ctx.acted.key === key);
  var foeReady = playing && waiting.indexOf(d.ids[1-duelSeat(d)]) < 0 && d.phase === "choose";
  if(mine){
    var want = d.phase === "replace" ? "replace" : (sc.mode === "fight" || sc.mode === "team" ? sc.mode : "main");
    if(sc.mode !== want) sc.setMode(want);
    var tag = left+"s"+(away.length ? " · "+duelName(d, away[0])+" reconnecting… "+Math.max(0, awayLeft)+"s" : foeReady ? " · "+duelName(d, d.ids[1-duelSeat(d)])+" is ready" : "");
    if(sc.cmd.getAttribute("data-left") !== tag) sc.cmd.setAttribute("data-left", tag);
    return;
  }
  sc.cmd.removeAttribute("data-left");
  var others = waiting.filter(function(u){ return u !== me(); }).map(function(u){ return duelName(d, u); });
  var txt;
  if(away.length) txt = away.map(function(u){ return duelName(d, u); }).join(" and ")+" lost connection. Waiting "+Math.max(0, awayLeft)+"s for them to come back…";
  else if(playing){
    txt = (ctx.acted && ctx.acted.key === key ? "You chose "+ctx.acted.label+". " : "")+
      (others.length ? "Waiting for "+others.join(" and ")+"… "+left+"s" : "Both ready. Resolving the turn…");
  } else txt = "Watching"+(others.length ? " · "+others.join(" and ")+" choosing… "+left+"s" : "");
  var change = playing && ctx.acted && ctx.acted.key === key && others.length && !away.length ?
    {label:"Change", fn:function(){ ctx.acted = null; ctx.changing = key; duelPrompt(ctx, s); }} : null;
  sc.setMode("wait", txt, change);
}
register("duel", "⚔️", "Live Pokémon battles with friends", function(ctx){ renderDuel(ctx, st("duel")); });

/* =============================== MINES =============================== */
HANDLERS.mines = {
  on: function(m, s){
    if(m.ev === "mines"){ s.run = m.run; if(m.note) s.note = m.note; if(m.by && m.run) api.toast("⛏️ "+nameOf(m.by)+" started a co-op mine run"); }
    if(m.ev === "loot"){ var got = []; Object.keys(m.items||{}).forEach(function(id){ var n = m.items[id]|0; if(n>0 && api.items[id]){ api.inv.add(id, n); got.push(n+" "+api.items[id].name); } });
      api.toast(m.fainted ? "You fainted and kept half: "+(got.join(", ")||"nothing") : "Climbed out with "+(got.join(", ")||"nothing")); }
    if(m.ev === "minesend"){ s.run = null; s.note = "Run over — deepest floor "+m.depth+"."; }
  },
  render: function(ctx, s){ renderMines(ctx, s); }
};
var MCOL = ["#3a6fd8","#d8433a","#3aa86a","#c9a14a","#8a3fd8","#d83aa0","#3ac9c9","#e07b25"];
function renderMines(ctx, s){
  var box = ctx.box; box.textContent = "";
  var r = s.run;
  if(!r){
    if(s.note) box.appendChild(api.mk("p",null,s.note));
    box.appendChild(api.btn("Start a co-op run","primary",function(){ send("mines","start"); }));
    box.appendChild(api.mk("p","vg-muted","Everyone in the lobby goes down together. Loot belongs to whoever breaks the rock; slimes chase the nearest miner."));
    return;
  }
  var T = 16, cv = api.canvas(12*T, 8*T), g = cv.getContext("2d"); g.imageSmoothingEnabled = false;
  for(var y=0;y<r.grid.length;y++) for(var x=0;x<r.grid[y].length;x++){
    var c = r.grid[y][x]; g.fillStyle = "#6e5a48"; g.fillRect(x*T,y*T,T,T);
    if(c==="rock"){ g.fillStyle = "#8a8a96"; g.fillRect(x*T+2,y*T+2,T-4,T-4); g.fillStyle="#4a4a52"; g.fillRect(x*T+4,y*T+5,3,3); }
    if(c==="ladder"){ g.fillStyle = "#c9a14a"; g.fillRect(x*T+3,y*T,2,T); g.fillRect(x*T+11,y*T,2,T); for(var k=2;k<T;k+=5) g.fillRect(x*T+3,y*T+k,10,2); }
  }
  (r.slimes||[]).forEach(function(sl){ g.fillStyle = "#5ac85a"; g.fillRect(sl[0]*T+3, sl[1]*T+6, 10, 8); });
  var keys = Object.keys(r.players||{});
  keys.forEach(function(uid, i){ var p = r.players[uid]; if(p.out) return; g.fillStyle = MCOL[i%MCOL.length]; g.fillRect(p.x*T+4, p.y*T+3, 8, 11); if(uid===me()){ g.strokeStyle="#ffffff"; g.strokeRect(p.x*T+3.5, p.y*T+2.5, 9, 12); } });
  var info = api.mk("div","vg-row"); info.appendChild(api.mk("b",null,"Floor "+r.depth));
  keys.forEach(function(uid, i){ var p = r.players[uid], loot = (r.loot||{})[uid]||{}, n = Object.keys(loot).reduce(function(a,k){ return a+loot[k]; },0);
    var chip = api.mk("span","vg-player", (uid===me()?"You":p.name)+" "+"♥".repeat(Math.max(0,p.hp))+(p.out?" (out)":"")+" · "+n+" loot"); chip.style.borderColor = MCOL[i%MCOL.length]; info.appendChild(chip); });
  box.appendChild(info);
  cv.setAttribute("aria-label","Co-op mine floor "+r.depth+". Arrow keys or WASD move.");
  cv.addEventListener("keydown", function(e){
    var d = {ArrowUp:[0,-1],ArrowDown:[0,1],ArrowLeft:[-1,0],ArrowRight:[1,0],w:[0,-1],s:[0,1],a:[-1,0],d:[1,0]}[e.key];
    if(d && !ctx.paused){ e.preventDefault(); send("mines","move",{dx:d[0], dy:d[1]}); }
  });
  box.appendChild(cv);
  if(s.note) box.appendChild(api.mk("p","vg-msg",s.note));
  var mine = r.players[me()];
  if(mine && !mine.out) box.appendChild(api.btn("Climb out with your loot","",function(){ send("mines","exit"); }));
  setTimeout(function(){ if(ctx.alive && (!document.activeElement || document.activeElement===document.body || document.activeElement.tagName==="CANVAS")) cv.focus(); }, 0);
}
register("mines", "⛏️", "Dig the same floor together", function(ctx){ renderMines(ctx, st("mines")); });

/* =============================== FARM =============================== */
HANDLERS.farm = {
  on: function(m, s){
    if(m.ev === "farm"){ s.farm = m.farm; }
    if(m.ev === "note"){ s.notes = [String(m.text||"").slice(0,80)].concat(s.notes||[]).slice(0,5); }
    if(m.ev === "harvest" && api.items[m.crop]){ api.inv.add(m.crop, m.n|0); api.toast("🧺 You harvested "+api.items[m.crop].name+" ×"+(m.n|0)); }
  },
  render: function(ctx, s){ renderFarm(ctx, s); }
};
function renderFarm(ctx, s){
  var box = ctx.box; box.textContent = "";
  var f = s.farm; if(!f){ box.appendChild(api.mk("p","vg-muted","Loading the room's farm…")); return; }
  var head = api.mk("div","vg-row");
  head.appendChild(api.mk("span","vg-muted", f.gardeners+" gardener"+(f.gardeners===1?"":"s")+" · "+f.season+" · Water is the gardeners' published prompts since planting."));
  var sb = api.btn("Open the room's seed box","",function(){ send("farm","seeds"); }); sb.disabled = !f.seedBoxOpen; head.appendChild(sb);
  box.appendChild(head);
  var grid = api.mk("div","vg-garden");
  (f.plots||[]).forEach(function(p, i){
    var cell = api.mk("div","vg-plot"+(p && p.ripe ? " ripe" : ""));
    if(!p){
      var seeds = Object.keys(f.seeds||{}).filter(function(k){ return f.seeds[k]>0 && api.items[k]; });
      cell.appendChild(api.mk("span","vg-soil-empty","Empty"));
      if(seeds.length){ var sel = api.mk("select","vg-select"); sel.setAttribute("aria-label","Plant in plot "+(i+1)); sel.appendChild(api.mk("option",null,"Plant…"));
        seeds.forEach(function(k){ var o = api.mk("option",null,api.items[k].name+" ("+f.seeds[k]+")"); o.value = k; sel.appendChild(o); });
        sel.addEventListener("change", function(){ if(sel.value) send("farm","plant",{plot:i, crop:sel.value}); }); cell.appendChild(sel); }
    } else {
      var it = api.items[p.crop]; cell.appendChild(api.iconEl(p.crop, 4));
      cell.appendChild(api.mk("b",null,(it?it.name:p.crop)));
      cell.appendChild(api.mk("span","vg-muted","by "+p.by));
      if(p.ripe) cell.appendChild(api.btn("Harvest","primary",function(){ send("farm","harvest",{plot:i}); }));
      else { cell.appendChild(api.mk("span","vg-muted","Water "+p.water+"/"+p.need+(p.hoursLeft>0?" · "+p.hoursLeft+"h":"")));
        var w = api.mk("div","vg-meter"), fi = api.mk("i"); fi.style.width = Math.round(Math.min(p.water/p.need, 1)*100)+"%"; w.appendChild(fi); cell.appendChild(w); }
    }
    grid.appendChild(cell);
  });
  box.appendChild(grid);
  (s.notes||[]).forEach(function(n){ box.appendChild(api.mk("div","vg-muted",n)); });
}
register("farm", "🌾", "One garden the whole room grows", function(ctx){ renderFarm(ctx, st("farm")); });
})();
