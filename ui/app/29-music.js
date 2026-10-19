/* ================= Music: Now Playing, listen-along rooms, DJ visualiser ================= */
// Three parts, one view:
//   - Now Playing: dashboard.py reads what this Mac plays (Spotify, Apple Music, a YouTube Music tab)
//     and, while paired and "Share what I play" is on, puts it on the Arena. Everyone listening right
//     now is listed here and shows on their lobby chip. Links open the same song in your own app.
//   - Listen room: your Arena room (the Lobby, or the private room you're in) plays one shared queue
//     of YouTube videos. The Arena keeps the queue and a clock; every page plays the video itself in
//     an embedded YouTube player and keeps it within a second of the room. No audio passes through
//     the Arena or this machine's server.
//   - Go live: the DJ shares a tab's or the system's sound and it streams to everyone who presses
//     Listen, browser to browser over WebRTC (like a call's screen share, but music-grade audio). The
//     Arena relays only the connection setup, as it does for voice. Any app works: Spotify, Apple Music...
//   - Visualiser: drawn from real audio (yours when live, the stream when listening live, or the DJ's
//     16-number spectrum relayed ~10 times a second); otherwise a gentle beat seeded per song.
// Room messages ({type:"music"}) are documented in backend/src/music.rs.
var MU = {
  st:null, offset:0, now:null, people:[], peopleAt:0, tuned:false, frame:null, frameV:null, frameReady:false,
  yt:{t:0, state:-1, dur:0, at:0}, durSent:{}, endSent:null, errV:null, volume:80, video:true,
  mode:"bars", raf:0, lastDraw:0, levels:null, remote:null, dj:null, djSendAt:0, results:[], searching:false,
  sig:"", shareNoted:false
};
try { MU.volume = Math.max(0, Math.min(100, parseInt(localStorage.getItem("hq_mu_vol"), 10))); if(isNaN(MU.volume)) MU.volume = 80; } catch(e){}
try { MU.mode = ["bars","ring","wave"].indexOf(localStorage.getItem("hq_mu_mode")) >= 0 ? localStorage.getItem("hq_mu_mode") : "bars"; } catch(e){}
try { MU.video = localStorage.getItem("hq_mu_video") !== "0"; } catch(e){}
var MU_SRC = {spotify:"Spotify", apple:"Apple Music", ytmusic:"YouTube Music"};
var MU_BANDS = 16;

function muOn(){ return typeof VIEW !== "undefined" && VIEW === "music"; }
function muCalm(){ return typeof hqCalm === "function" ? hqCalm() : document.documentElement.classList.contains("hq-calm"); }
function muArenaHas(){ return !!(ARENA.arena && ARENA.arena.music); }
function muSockOk(){ return !!(ARENA.sock && ARENA.sock.readyState === 1); }
// The page keeps the room socket while you're on this view or tuned in, even with "stay in the lobby" off.
function muWantsSocket(){ return !!(typeof MU !== "undefined" && MU && ARENA.paired && (muOn() || MU.tuned)); }
function muSend(op, extra){
  if(!muSockOk() || !muArenaHas()) return false;
  var m = {type:"music", op:op}; if(extra) for(var k in extra) m[k] = extra[k];
  try { ARENA.sock.send(JSON.stringify(m)); return true; } catch(e){ return false; }
}
function muFmt(ms){
  ms = Math.max(0, ms|0); var s = Math.floor(ms/1000), m = Math.floor(s/60); s = s % 60;
  if(m >= 60) return Math.floor(m/60) + ":" + String(m%60).padStart(2,"0") + ":" + String(s).padStart(2,"0");
  return m + ":" + String(s).padStart(2,"0");
}
function muPlace(){ return ARENA.roomId === "lobby" || !ARENA.roomId ? "the Lobby" : (ARENA.roomName || "your room"); }
// Where the room's current item is right now, on the Arena's clock.
function muPos(){
  var s = MU.st; if(!s || !s.cur) return 0;
  var p = s.playing ? (Date.now() + MU.offset - s.startAt) : s.pos;
  p = Math.max(0, p);
  return s.cur.durMs ? Math.min(p, s.cur.durMs) : p;
}
// An original cover tile for a song: two hues and a stripe angle from its title.
function muArt(seed, size){
  var r = mulberry32(hashStr("mu:" + (seed || "")));
  var h1 = Math.floor(r()*360), h2 = (h1 + 40 + Math.floor(r()*120)) % 360, a = Math.floor(r()*180);
  var d = document.createElement("span"); d.className = "mu-art"; d.setAttribute("aria-hidden", "true");
  d.style.background = "linear-gradient(" + a + "deg, hsl(" + h1 + " 70% 55%), hsl(" + h2 + " 70% 40%))";
  if(size) { d.style.width = d.style.height = size + "px"; }
  var n = document.createElement("span"); n.className = "mu-art-note"; n.textContent = "♪"; d.appendChild(n);
  return d;
}
// "Open in" links for a track: straight to the song where we know its id, otherwise a search.
function muLinks(t){
  var q = encodeURIComponent(((t.title || "") + " " + (t.artist || "")).trim());
  var box = el("span", "mu-links");
  function a(label, href, title){
    var x = document.createElement("a"); x.className = "mu-link"; x.href = href; x.target = "_blank"; x.rel = "noopener noreferrer";
    x.textContent = label; x.title = title; box.appendChild(x);
  }
  a("Spotify", t.spotifyId ? "https://open.spotify.com/track/" + encodeURIComponent(t.spotifyId) : "https://open.spotify.com/search/" + q, "Open in Spotify");
  a("Apple Music", "https://music.apple.com/search?term=" + q, "Find in Apple Music");
  a("YT Music", t.youtubeId ? "https://music.youtube.com/watch?v=" + encodeURIComponent(t.youtubeId) : "https://music.youtube.com/search?q=" + q, "Open in YouTube Music");
  return box;
}

/* ---------- Now Playing: mine and everyone's ---------- */
function muLoadMine(){
  return fetch("/api/music/now", {cache:"no-store"}).then(function(r){ return r.json(); }).then(function(j){
    MU.now = j && typeof j === "object" ? j : null;
    if(MU.now && MU.now.track) MU.now.track._at = Date.now() - Math.max(0, Math.min(5000, MU.now.ageMs|0));
    if(MU.now && MU.now.share && MU.now.paired && MU.now.shared && !MU.shareNoted) muShareNotice();
    muRenderMine();
  }).catch(function(){});
}
function muLoadPeople(){
  if(!ARENA.paired) { MU.people = []; return Promise.resolve(); }
  return fetch("/api/arena/music/now", {cache:"no-store"}).then(function(r){ return r.ok ? r.json() : null; }).then(function(j){
    MU.people = (j && Array.isArray(j.listening)) ? j.listening.filter(function(x){ return x && x.user && x.track; }) : [];
    var at = Date.now(); MU.people.forEach(function(x){ x.track._at = at; });
    MU.peopleAt = Date.now();
    muRenderPeople();
    if(typeof arenaRerenderLobby === "function") arenaRerenderLobby();
  }).catch(function(){});
}
// Once per install: say that sharing is on, and where to turn it off.
function muShareNotice(){
  MU.shareNoted = true;
  var seen = false; try { seen = localStorage.getItem("hq_mu_noticed") === "1"; } catch(e){}
  if(seen) return;
  try { localStorage.setItem("hq_mu_noticed", "1"); } catch(e){}
  toast("🎵 Sharing what you play with the Arena. Turn it off in Music or Settings.", "level");
}
function muSetShare(on){
  CONFIG.musicShare = !!on;
  postConfig({musicShare: !!on}).then(function(r){
    toast(r.ok ? (on ? "🎵 Sharing what you play" : "Music sharing off") : "⚠ " + (r.j.error || "save failed"), r.ok ? "level" : "ach");
    setTimeout(muLoadMine, 400);
  }).catch(function(){ toast("⚠ save failed", "ach"); });
}
function muTrackRow(t, opts){
  opts = opts || {};
  var row = el("div", "mu-track");
  row.appendChild(muArt(t.title + "|" + t.artist, opts.big ? 72 : 44));
  var body = el("div", "mu-track-body");
  var ti = el("div", "mu-title"); ti.textContent = t.title || "Unknown"; body.appendChild(ti);
  var ar = el("div", "mu-artist"); ar.textContent = [t.artist, t.album].filter(Boolean).join(" · ") || " "; body.appendChild(ar);
  var meta = el("div", "mu-meta");
  var src = el("span", "mu-src mu-src-" + (MU_SRC[t.source] ? t.source : "other")); src.textContent = MU_SRC[t.source] || "Music"; meta.appendChild(src);
  if(t.playing === false){ var pz = el("span", "mu-paused"); pz.textContent = "paused"; meta.appendChild(pz); }
  if(t.durationMs){
    var pr = el("span", "mu-bar mu-tick"); pr.setAttribute("role", "progressbar"); pr.setAttribute("aria-valuemin", "0");
    pr.setAttribute("aria-valuemax", String(Math.round(t.durationMs/1000)));
    pr.setAttribute("aria-label", "Position");
    pr.appendChild(el("i"));
    meta.appendChild(pr);
    meta.appendChild(el("span", "mu-time"));
    muAnchor(meta, t);
  }
  body.appendChild(meta);
  row.appendChild(body);
  return row;
}
// A track's position, ticking on from when we last heard it (the player's own clock, not our poll's).
function muTrackPos(t){
  var p = t.positionMs || 0;
  if(t.playing !== false && t._at) p += Date.now() - t._at;
  return t.durationMs ? Math.max(0, Math.min(p, t.durationMs)) : Math.max(0, p);
}
// Point a row's bar + time at a (fresh) track, without rebuilding it.
function muAnchor(meta, t){
  if(!meta) return;
  var old = meta._track;
  // The same song still where we expected (within a second): keep our clock, so it never twitches.
  if(old && muSongKey(old) === muSongKey(t) && Math.abs(muTrackPos(old) - muTrackPos(t)) < 1000) return;
  meta._track = t;
  muTickOne(meta);
}
function muTickOne(meta){
  var t = meta && meta._track; if(!t || !t.durationMs) return;
  var p = muTrackPos(t), bar = meta.querySelector(".mu-bar"), tm = meta.querySelector(".mu-time");
  if(bar){ bar.firstChild.style.width = (100 * p / t.durationMs).toFixed(2) + "%"; bar.setAttribute("aria-valuenow", String(Math.round(p/1000))); }
  if(tm) tm.textContent = muFmt(p) + " / " + muFmt(t.durationMs);
}
function muTickAll(){
  Array.prototype.forEach.call(document.querySelectorAll("#musicView .mu-meta"), muTickOne);
}
function muSongKey(t){ return t ? [t.title, t.artist, t.album, t.source, t.playing !== false, t.durationMs].join("|") : ""; }
function muRenderMine(){
  var box = $("muMine"); if(!box) return;
  var n = MU.now;
  // Same song, same state: just move its clock (rebuilding every poll made the card flicker and jump).
  var key = n ? [n.platform, muSongKey(n.track), muArenaHas(), muPlace()].join("#") : "";
  if(key && key === box.getAttribute("data-key")){
    var meta = box.querySelector(".mu-meta"); if(meta && n.track) muAnchor(meta, n.track);
    muRenderShareState(n);
    return;
  }
  box.setAttribute("data-key", key);
  box.textContent = "";
  if(!n){ var p = el("p", "muted"); p.textContent = "Looking for music…"; box.appendChild(p); return; }
  if(n.platform && n.platform !== "darwin"){
    var q = el("p", "muted"); q.textContent = "Now Playing reads Spotify, Apple Music and YouTube Music on macOS. You can still listen along in rooms below."; box.appendChild(q);
  } else if(!n.track){
    var e = el("p", "muted mu-empty"); e.textContent = "Nothing playing. Start a song in Spotify, Apple Music or a YouTube Music tab."; box.appendChild(e);
  } else {
    box.appendChild(muTrackRow(n.track, {big:true}));
    var acts = el("div", "mu-acts");
    acts.appendChild(muLinks(n.track));
    if(muArenaHas()){
      var add = el("button", "mu-btn"); add.type = "button"; add.textContent = "➕ Add to room";
      add.title = "Find this song on YouTube and queue it in " + muPlace();
      add.addEventListener("click", function(){ muFindFor(n.track); });
      acts.appendChild(add);
    }
    box.appendChild(acts);
  }
  muRenderShareState(n);
}
function muRenderShareState(n){
  var share = $("muShare"); if(share) share.checked = !!n.share;
  var st = $("muShareState");
  if(st){
    var txt;
    if(!n.share) txt = "Not sharing. Only you see this.";
    else if(!n.paired) txt = "Pair with the Arena to share it.";
    else if(n.shareError === 404) txt = "Your Arena doesn’t support music yet. It needs an update; until then only you see this.";
    else if(n.shareError) txt = "Couldn’t reach the Arena (" + n.shareError + "). Retrying.";
    else if(n.shared) txt = "Shared: friends on the Arena see this.";
    else txt = n.track && n.track.playing ? "Sharing in a moment…" : "Shared while something plays.";
    st.textContent = txt;
  }
}
function muRenderPeople(){
  var box = $("muPeople"); if(!box) return;
  var key = [ARENA.paired, muArenaHas()].concat(MU.people.map(function(p){ return p.user.userId + ":" + muSongKey(p.track); })).join("#");
  if(key === box.getAttribute("data-key")){
    // Same people, same songs: re-anchor their clocks in place.
    var metas = box.querySelectorAll(".mu-meta"), others = MU.people.filter(function(p){ return !p.user.isYou; });
    Array.prototype.forEach.call(metas, function(m, i){ if(others[i]) muAnchor(m, others[i].track); });
    return;
  }
  box.setAttribute("data-key", key);
  box.textContent = "";
  var cnt = $("muPeopleCount");
  var others = MU.people.filter(function(p){ return !p.user.isYou; });
  if(cnt) cnt.textContent = others.length ? String(others.length) : "";
  if(!ARENA.paired){ var p0 = el("p", "muted"); p0.textContent = "Pair with the Arena to see what friends are playing."; box.appendChild(p0); return; }
  if(!others.length){ var p1 = el("p", "muted mu-empty"); p1.textContent = "No one else is sharing music right now."; box.appendChild(p1); return; }
  others.forEach(function(p){
    var li = el("div", "mu-person");
    var who = el("div", "mu-who");
    if(p.user.avatarUrl){ var img = document.createElement("img"); img.src = p.user.avatarUrl; img.alt = ""; img.loading = "lazy"; who.appendChild(img); }
    var nm = el("b"); nm.textContent = p.user.displayName || p.user.handle || "Someone"; who.appendChild(nm);
    if(p.user.handle){ var h = el("small", "muted"); h.textContent = "@" + p.user.handle; who.appendChild(h); }
    li.appendChild(who);
    li.appendChild(muTrackRow(p.track));
    var acts = el("div", "mu-acts");
    acts.appendChild(muLinks(p.track));
    if(muArenaHas()){
      var b = el("button", "mu-btn"); b.type = "button"; b.textContent = "🎧 Play in room";
      b.title = "Queue " + (p.track.title || "this song") + " in " + muPlace() + " so everyone hears it together";
      b.addEventListener("click", function(){ muFindFor(p.track); });
      acts.appendChild(b);
    }
    li.appendChild(acts);
    box.appendChild(li);
  });
}
// A lobby chip's "🎵 song" for someone sharing (19-arena-rooms.js calls this).
function muChipFor(userId){
  if(!userId) return null;
  var p = null;
  for(var i = 0; i < MU.people.length; i++){ if(MU.people[i].user && MU.people[i].user.userId === userId){ p = MU.people[i]; break; } }
  if(!p || !p.track || !p.track.playing) return null;
  var s = el("span", "mu-chip");
  var words = p.track.title + (p.track.artist ? " — " + p.track.artist : "");
  s.textContent = "🎵 " + p.track.title;
  s.title = "Listening to " + words + " on " + (MU_SRC[p.track.source] || "music");
  s.setAttribute("aria-label", s.title);
  return s;
}

/* ---------- Listen room: state from the Arena ---------- */
function musicOnWelcome(){
  MU.st = null; MU.endSent = null;
  muLiveOnLobby();
  if(muArenaHas()) muSend("sync");
  muRenderRoom();
}
function musicOnLost(){ muLiveOnLost(); muRenderRoom(); }
function musicOnMsg(m){
  if(!m || typeof m !== "object") return;
  if(m.op === "state" && m.state && typeof m.state === "object"){
    if(typeof m.now === "number") MU.offset = m.now - Date.now();
    var prev = MU.st;
    MU.st = m.state;
    var by = m.by && typeof m.by === "object" ? m.by : null;
    var mine = !!(by && ARENA.you && by.userId === ARENA.you.userId);
    if(by && !mine) muActivity(by, m.what, m.state);
    if(!prev || !prev.cur || !m.state.cur || prev.cur.id !== m.state.cur.id){ MU.endSent = null; MU.errV = null; }
    muRenderRoom();
    muSyncPlayer(true);
  } else if(m.op === "viz" && Array.isArray(m.b) && m.b.length === MU_BANDS){
    MU.remote = {b: m.b.map(function(x){ return Math.max(0, Math.min(255, x|0)); }), at: Date.now(), from: m.from};
  } else if(m.op === "error" && typeof m.error === "string"){
    if(m.error.indexOf("unknown op swap") >= 0 && MU.st && MU.st.cur){ muSend("next", {id:MU.st.cur.id}); return; }
    muNote(m.error.slice(0, 140));
  }
}
function muActivity(by, what, st){
  var who = by.displayName || by.handle || "Someone", t = st && st.cur ? st.cur.title : "";
  var words = {add:"queued a song", play:"pressed play", pause:"paused", seek:"jumped ahead", next:"skipped", remove:"removed a song", clear:"cleared the queue"}[what];
  if(!words) return;
  if(what === "add" && st && st.queue && st.queue.length) t = st.queue[st.queue.length-1].title;
  muNote(who + " " + words + (t && (what === "add" || what === "next") ? ": " + t : ""));
}
function muNote(text){
  var n = $("muNote"); if(!n) return;
  n.textContent = text; n.classList.remove("hidden");
  clearTimeout(MU.noteT); MU.noteT = setTimeout(function(){ n.classList.add("hidden"); }, 5000);
  announce(text);
}

/* ---------- the embedded player ---------- */
// YouTube's embed, driven with its postMessage protocol (no YouTube script runs in this page).
function muPost(func, args){
  if(!MU.frame || !MU.frame.contentWindow) return;
  try { MU.frame.contentWindow.postMessage(JSON.stringify({event:"command", func:func, args:args || [], id:"hqmusic", channel:"widget"}), "*"); } catch(e){}
}
function muMakeFrame(v, startS, host2){
  var host = $("muPlayer"); if(!host) return;
  host.textContent = "";
  var f = document.createElement("iframe");
  f.className = "mu-frame";
  f.title = "Room player";
  f.allow = "autoplay; encrypted-media; picture-in-picture";
  f.referrerPolicy = "strict-origin-when-cross-origin";
  f.src = "https://" + (host2 ? "www.youtube.com" : "www.youtube-nocookie.com") + "/embed/" + encodeURIComponent(v) + "?enablejsapi=1&autoplay=1&playsinline=1&rel=0&modestbranding=1&start=" + Math.max(0, startS|0) + "&origin=" + encodeURIComponent(location.origin);
  f.addEventListener("load", function(){
    // Ask the player to report its time and state to us, again until it answers (it boots after "load").
    var tries = 0;
    clearInterval(MU.hello);
    MU.hello = setInterval(function(){
      if(MU.frame !== f || MU.yt.at || ++tries > 40){ clearInterval(MU.hello); return; }
      try { f.contentWindow.postMessage(JSON.stringify({event:"listening", id:"hqmusic", channel:"widget"}), "*"); } catch(e){}
    }, 250);
    MU.frameReady = true;
    muPost("addEventListener", ["onStateChange"]);
    muPost("addEventListener", ["onError"]);
    muPost("setVolume", [MU.volume]);
    setTimeout(function(){ muSyncPlayer(true); }, 600);
  });
  host.appendChild(f);
  MU.frame = f; MU.frameV = v; MU.frameReady = false; MU.yt = {t:0, state:-1, dur:0, at:0};
  host.classList.toggle("novideo", !MU.video);
}
function muDropFrame(){
  var host = $("muPlayer"); if(host) host.textContent = "";
  MU.frame = null; MU.frameV = null; MU.frameReady = false;
}
window.addEventListener("message", function(e){
  if(!MU.frame || e.source !== MU.frame.contentWindow) return;
  if(!/^https:\/\/(www\.)?youtube(-nocookie)?\.com$/.test(e.origin)) return;
  var d; try { d = typeof e.data === "string" ? JSON.parse(e.data) : e.data; } catch(err){ return; }
  if(!d || typeof d !== "object") return;
  if(d.event === "infoDelivery" && d.info && typeof d.info === "object"){
    var i = d.info;
    if(typeof i.currentTime === "number") { MU.yt.t = i.currentTime; MU.yt.at = Date.now(); }
    if(typeof i.duration === "number" && i.duration > 0) MU.yt.dur = i.duration;
    if(typeof i.playerState === "number") MU.yt.state = i.playerState;
    muOnPlayer();
  } else if(d.event === "onStateChange" && typeof d.info === "number"){
    MU.yt.state = d.info; muOnPlayer();
  } else if(d.event === "onError"){
    muOnPlayerError(d.info);
  }
});
function muOnPlayer(){
  var s = MU.st; if(!s || !s.cur || MU.frameV !== s.cur.v) return;
  // The first page to learn how long a video is tells the room (progress bars, end of queue).
  if(!s.cur.durMs && MU.yt.dur > 0 && !MU.durSent[s.cur.id]){ MU.durSent[s.cur.id] = 1; muSend("dur", {id:s.cur.id, ms:Math.round(MU.yt.dur*1000)}); }
  // Ended: ask for the next one (the Arena advances once, however many pages ask).
  if(MU.yt.state === 0 && MU.endSent !== s.cur.id){ MU.endSent = s.cur.id; muSend("next", {id:s.cur.id}); }
}
// A video that won't play here: try YouTube's main player once, then swap in another upload of the
// same song (the person who queued it does the looking, so the room gets one swap, not one per page).
function muOnPlayerError(code){
  var s = MU.st; if(!s || !s.cur) return;
  var cur = s.cur, key = cur.id + ":" + cur.v;
  if(MU.errV === key) return;
  MU.errV = key;
  if(!MU.altTried) MU.altTried = {};
  if(!MU.altTried[key]){
    MU.altTried[key] = 1; MU.errV = null;
    muMakeFrame(cur.v, muPos() / 1000, true);
    return;
  }
  muMarkBad(cur.v);
  muNote("That upload can\u2019t play outside YouTube. Finding another\u2026");
  var mine = !!(cur.by && ARENA.you && cur.by.userId === ARENA.you.userId);
  setTimeout(function(){ muReplace(cur); }, mine ? 0 : 7000);   // a backstop if whoever queued it isn't listening
}
function muReplace(cur){
  var s = MU.st; if(!s || !s.cur || s.cur.id !== cur.id || s.cur.v !== cur.v) return;   // already handled
  // Three uploads of one song have failed: it isn't going to play here, move on.
  if(!MU.swaps) MU.swaps = {};
  if((MU.swaps[cur.id] = (MU.swaps[cur.id] || 0) + 1) > 3){ muNote("No playable upload found. Skipping\u2026"); muSend("next", {id:cur.id}); return; }
  var q = cur.title.replace(/\((official|music|video|audio|lyrics?|visuali[sz]er|hd|4k|remaster(ed)?)[^)]*\)/ig, "").replace(/\s+/g, " ").trim();
  fetch("/api/music/search?q=" + encodeURIComponent((q + " lyrics").slice(0, 200)), {cache:"no-store"})
    .then(function(r){ return r.json(); })
    .then(function(j){
      var s2 = MU.st; if(!s2 || !s2.cur || s2.cur.id !== cur.id || s2.cur.v !== cur.v) return;
      var alt = ((j && j.results) || []).filter(function(r){ return r && r.v !== cur.v && !muIsBad(r.v); })[0];
      if(alt){ muSend("swap", {id:cur.id, v:alt.v, title:cur.title}); muNote("Playing another upload of " + cur.title); }
      else { muNote("No playable upload found. Skipping\u2026"); muSend("next", {id:cur.id}); }
    })
    .catch(function(){ muSend("next", {id:cur.id}); });
}
/* ---------- the room song from this machine (yt-dlp) ----------
   When this HQ has yt-dlp, the room's current song is fetched here and played from 127.0.0.1: no
   YouTube player, so nothing to be blocked, and the visualiser hears the real audio. */
MU.ytdlp = null;          // null: not asked yet; true / false
MU.localFail = {};        // video id -> 1: couldn't be fetched here, use YouTube's player for it
function muLocalOn(v){ return MU.ytdlp === true && !MU.localFail[v]; }
function muAudioState(v){
  return fetch("/api/music/audio-state?v=" + encodeURIComponent(v), {cache:"no-store"}).then(function(r){ return r.json(); });
}
function muAudioEl(){
  if(MU.au) return MU.au;
  var a = MU.au = document.createElement("audio");
  a.preload = "auto";
  a.addEventListener("loadedmetadata", function(){
    var s = MU.st; if(!s || !s.cur || MU.auV !== s.cur.v) return;
    MU.auReady = true;
    if(!s.cur.durMs && a.duration > 0 && isFinite(a.duration) && !MU.durSent[s.cur.id]){ MU.durSent[s.cur.id] = 1; muSend("dur", {id:s.cur.id, ms:Math.round(a.duration*1000)}); }
    muSyncLocal(true); muRenderPlayerLocal();
  });
  a.addEventListener("ended", function(){
    var s = MU.st; if(s && s.cur && MU.auV === s.cur.v && MU.endSent !== s.cur.id){ MU.endSent = s.cur.id; muSend("next", {id:s.cur.id}); }
  });
  a.addEventListener("error", function(){
    var s = MU.st; if(!s || !s.cur || MU.auV !== s.cur.v) return;
    MU.localFail[s.cur.v] = 1; MU.auV = null; muSyncPlayer(true);
  });
  var host = $("muLiveAudio"); if(host) host.appendChild(a);
  return a;
}
// Our analyser on the room song (made on the Tune in click, so the AudioContext is allowed to start).
function muAudioGraph(){
  if(MU.auAn || !MU.au) return;
  try {
    var Ctx = window.AudioContext || window.webkitAudioContext;
    MU.auCtx = new Ctx();
    var src = MU.auCtx.createMediaElementSource(MU.au), an = MU.auCtx.createAnalyser();
    an.fftSize = 256; an.smoothingTimeConstant = 0.6;
    src.connect(an); an.connect(MU.auCtx.destination);
    MU.auAn = an; MU.auBuf = new Uint8Array(an.frequencyBinCount);
  } catch(e){}
}
function muLoadLocal(cur){
  var a = muAudioEl(), v = cur.v, seq = MU.auSeq = (MU.auSeq || 0) + 1, t0 = Date.now();
  MU.auV = v; MU.auReady = false;
  try { a.pause(); a.removeAttribute("src"); a.load(); } catch(e){}
  muRenderPlayerLocal("Fetching \u201c" + cur.title + "\u201d\u2026");
  (function poll(){
    if(seq !== MU.auSeq || !MU.tuned) return;
    muAudioState(v).then(function(st){
      if(seq !== MU.auSeq) return;
      if(st.state === "ready"){ a.src = "/api/music/audio/" + encodeURIComponent(v); a.volume = MU.volume / 100; try { a.load(); } catch(e){} return; }
      if(st.state === "loading" && Date.now() - t0 < 120000){ setTimeout(poll, 700); return; }
      MU.localFail[v] = 1; MU.auV = null;
      muNote("Couldn\u2019t fetch that song here" + (st.error ? " (" + String(st.error).slice(0, 80) + ")" : "") + ". Trying YouTube\u2019s player\u2026");
      muSyncPlayer(true);
    }).catch(function(){ if(seq === MU.auSeq) setTimeout(poll, 1500); });
  })();
}
function muDropLocal(){
  MU.auSeq = (MU.auSeq || 0) + 1; MU.auV = null; MU.auReady = false;
  if(MU.au){ try { MU.au.pause(); MU.au.removeAttribute("src"); MU.au.load(); } catch(e){} }
}
function muSyncLocal(hard){
  var s = MU.st, a = MU.au; if(!s || !s.cur || !a || !MU.auReady || MU.auV !== s.cur.v) return;
  var want = muPos() / 1000;
  if(s.playing){
    if(hard || Math.abs(a.currentTime - want) > 0.6) a.currentTime = want;
    if(a.paused){ var p = a.play(); if(p && p.catch) p.catch(function(){ muNote("Press Tune in again to start the sound"); }); }
  } else {
    if(!a.paused) a.pause();
    if(hard) a.currentTime = want;
  }
}
function muRenderPlayerLocal(msg){
  var host = $("muPlayer"); if(!host) return;
  host.classList.remove("novideo");
  host.textContent = "";
  var p = el("p", "mu-player-empty");
  p.textContent = msg || "\uD83C\uDFA7 Playing from your HQ (fetched with yt-dlp), in sync with the room.";
  host.appendChild(p);
}
// Bring the local player in line with the room: right video, play/pause, within ~1.5 s.
function muSyncPlayer(hard){
  var s = MU.st;
  if(!MU.tuned){ if(MU.frame) muDropFrame(); muDropLocal(); return; }
  if(!s || !s.cur){ if(MU.frame) muDropFrame(); muDropLocal(); muRenderPlayerEmpty(); return; }
  if(muLocalOn(s.cur.v)){
    if(MU.frame) muDropFrame();
    if(MU.auV !== s.cur.v) muLoadLocal(s.cur); else muSyncLocal(hard);
    // Fetch the next song ahead, so it starts on time.
    if(s.queue && s.queue[0] && !MU.localFail[s.queue[0].v] && MU.preV !== s.queue[0].v){ MU.preV = s.queue[0].v; muAudioState(MU.preV).catch(function(){}); }
    return;
  }
  muDropLocal();
  var want = muPos() / 1000;
  if(MU.frameV !== s.cur.v){ muMakeFrame(s.cur.v, want); if(!s.playing) setTimeout(function(){ muPost("pauseVideo"); }, 900); return; }
  if(!MU.frameReady) return;
  if(s.playing){
    if(MU.yt.state !== 1 && MU.yt.state !== 3) muPost("playVideo");
    var have = MU.yt.t + (MU.yt.state === 1 && MU.yt.at ? (Date.now() - MU.yt.at)/1000 : 0);
    if(hard || Math.abs(have - want) > 1.5) muPost("seekTo", [want, true]);
  } else {
    if(MU.yt.state === 1 || MU.yt.state === 3) muPost("pauseVideo");
    if(hard) muPost("seekTo", [want, true]);
  }
}
function muRenderPlayerEmpty(){
  var host = $("muPlayer"); if(!host || host.querySelector(".mu-frame") || (MU.auV && MU.st && MU.st.cur)) return;
  host.textContent = "";
  var p = el("p", "mu-player-empty"); p.textContent = "The queue is empty. Add a song below and everyone here hears it together."; host.appendChild(p);
}
function muTune(on){
  MU.tuned = !!on;
  if(MU.tuned){
    if(MU.ytdlp === true){ muAudioEl(); muAudioGraph(); if(MU.auCtx && MU.auCtx.state === "suspended") MU.auCtx.resume().catch(function(){}); }
    if(ARENA.paired) arenaOpenSocket(); muSyncPlayer(true); announce("Tuned in to " + muPlace());
  }
  else { muDropFrame(); muDropLocal(); announce("Stopped listening"); }
  muRenderRoom();
}

/* ---------- room UI ---------- */
function muRenderRoom(){
  var box = $("muRoom"); if(!box) return;
  var head = $("muRoomHead"), gate = $("muRoomGate"), main = $("muRoomMain");
  var ok = ARENA.paired && muSockOk() && muArenaHas();
  if(head) head.textContent = "🎧 Listening room · " + muPlace().replace(/^the /, "");
  if(gate){
    var why = !ARENA.paired ? "Pair with the Arena (Arena tab) to listen together."
      : !muSockOk() ? "Connecting to " + muPlace() + "…"
      : !muArenaHas() ? "This Arena doesn’t run listening rooms yet. It needs an update."
      : "";
    gate.textContent = why; gate.classList.toggle("hidden", !why);
  }
  if(main) main.classList.toggle("hidden", !ok);
  var tb = $("muTune");
  if(tb){ tb.textContent = MU.tuned ? "Stop listening" : "🎧 Tune in"; tb.classList.toggle("on", MU.tuned); tb.setAttribute("aria-pressed", MU.tuned ? "true" : "false"); tb.disabled = !ok; }
  var s = MU.st || {};
  var cur = $("muCur");
  if(cur){
    cur.textContent = "";
    if(s.cur){
      cur.appendChild(muArt(s.cur.v, 40));
      var b = el("div", "mu-track-body");
      var t = el("div", "mu-title"); t.textContent = s.cur.title; b.appendChild(t);
      var by = el("div", "mu-artist"); by.textContent = "added by " + ((s.cur.by && s.cur.by.displayName) || "someone"); b.appendChild(by);
      cur.appendChild(b);
    } else {
      var e = el("p", "muted"); e.textContent = "Nothing playing in " + muPlace() + "."; cur.appendChild(e);
    }
  }
  var pp = $("muPlayPause");
  if(pp){ pp.disabled = !s.cur; pp.textContent = s.playing ? "⏸" : "▶"; pp.setAttribute("aria-label", s.playing ? "Pause for everyone" : "Play for everyone"); pp.title = pp.getAttribute("aria-label"); }
  var sk = $("muSkip"); if(sk) sk.disabled = !s.cur;
  var dj = $("muDjLabel");
  if(dj){
    var who = MU.dj ? "" : (s.dj ? (s.dj.displayName || "Someone") + " is the DJ" : "");
    dj.textContent = who; dj.classList.toggle("hidden", !who);
  }
  muRenderQueue();
  muRenderProgress();
  muRenderLive();
  if(MU.tuned && !s.cur) muRenderPlayerEmpty();
}
function muRenderProgress(){
  var s = MU.st || {}, bar = $("muProg"), fill = $("muProgFill"), tm = $("muProgTime");
  if(!bar) return;
  var d = s.cur && s.cur.durMs, p = muPos();
  if(fill) fill.style.width = d ? Math.min(100, 100*p/d).toFixed(2) + "%" : "0%";
  if(tm) tm.textContent = s.cur ? muFmt(p) + (d ? " / " + muFmt(d) : "") : "";
  bar.setAttribute("aria-valuemax", String(d ? Math.round(d/1000) : 0));
  bar.setAttribute("aria-valuenow", String(Math.round(p/1000)));
  bar.setAttribute("aria-valuetext", s.cur ? muFmt(p) + (d ? " of " + muFmt(d) : "") : "nothing playing");
}
function muRenderQueue(){
  var q = $("muQueue"); if(!q) return;
  var s = MU.st || {}, items = s.queue || [];
  var sig = items.map(function(i){ return i.id; }).join(",");
  if(sig === q.getAttribute("data-sig") && q.children.length) return;
  q.setAttribute("data-sig", sig);
  q.textContent = "";
  var qc = $("muQueueCount"); if(qc) qc.textContent = items.length ? String(items.length) : "";
  if(!items.length){ var e = el("li", "muted mu-empty"); e.textContent = "Up next is empty."; q.appendChild(e); return; }
  items.forEach(function(it, i){
    var li = el("li", "mu-q");
    var n = el("span", "mu-q-n"); n.textContent = String(i+1); li.appendChild(n);
    var b = el("div", "mu-track-body");
    var t = el("div", "mu-title"); t.textContent = it.title; b.appendChild(t);
    var by = el("div", "mu-artist"); by.textContent = (it.by && it.by.displayName ? it.by.displayName : "someone") + (it.durMs ? " · " + muFmt(it.durMs) : ""); b.appendChild(by);
    li.appendChild(b);
    var x = el("button", "mu-x"); x.type = "button"; x.textContent = "✕"; x.setAttribute("aria-label", "Remove " + it.title + " from the queue");
    x.addEventListener("click", function(){ muSend("remove", {id:it.id}); });
    li.appendChild(x);
    q.appendChild(li);
  });
}

/* ---------- adding songs ---------- */
// Videos that wouldn't play here (an owner blocked embedding): remembered so they never come back.
MU.bad = (function(){ try { var a = JSON.parse(localStorage.getItem("hq_mu_bad") || "[]"); return Array.isArray(a) ? a.filter(function(v){ return /^[A-Za-z0-9_-]{11}$/.test(v); }).slice(-300) : []; } catch(e){ return []; } })();
function muIsBad(v){ return MU.bad.indexOf(v) >= 0; }
function muMarkBad(v){
  if(!v || muIsBad(v)) return;
  MU.bad.push(v); if(MU.bad.length > 300) MU.bad.shift();
  try { localStorage.setItem("hq_mu_bad", JSON.stringify(MU.bad)); } catch(e){}
}
// quick: the as-you-type search (no per-result check, results show as they come, focus stays in the box).
function muSearch(q, quick){
  q = (q || "").trim(); if(!q) return;
  var seq = MU.searchSeq = (MU.searchSeq || 0) + 1;
  if(!quick){ MU.searching = true; muRenderResults(); }
  fetch("/api/music/search?q=" + encodeURIComponent(q.slice(0, 200)) + (quick ? "&quick=1" : ""), {cache:"no-store"})
    .then(function(r){ return r.json(); })
    .then(function(j){
      if(seq !== MU.searchSeq) return;   // a newer search is on its way
      MU.results = (j && Array.isArray(j.results)) ? j.results.filter(function(r){ return r && !muIsBad(r.v); }).slice(0, 6) : [];
      MU.searchErr = j && typeof j.error === "string" ? j.error.slice(0, 160) : "";
      MU.searching = false; muRenderResults(!quick, quick);
    })
    .catch(function(){ if(seq === MU.searchSeq){ MU.results = []; MU.searchErr = ""; MU.searching = false; muRenderResults(!quick, quick); } });
}
function muRenderResults(done, quiet){
  var box = $("muResults"); if(!box) return;
  box.textContent = "";
  if(quiet && !MU.results.length) return;
  if(MU.searching){ var p = el("li", "muted"); p.textContent = "Searching YouTube…"; box.appendChild(p); return; }
  if(done && !MU.results.length){ var e = el("li", "muted"); e.textContent = MU.searchErr || "No videos found. Try other words, or paste a YouTube link."; box.appendChild(e); return; }
  MU.results.forEach(function(r){
    var li = el("li", "mu-res");
    var b = el("button", "mu-res-btn"); b.type = "button";
    var t = el("span", "mu-title"); t.textContent = r.title || "YouTube video"; b.appendChild(t);
    var a = el("span", "mu-artist"); a.textContent = [r.author, r.length].filter(Boolean).join(" · "); b.appendChild(a);
    b.setAttribute("aria-label", "Queue " + (r.title || "this video"));
    b.addEventListener("click", function(){ muAdd(r); });
    li.appendChild(b); box.appendChild(li);
  });
  if(done && !quiet && MU.results.length){ var f = box.querySelector("button"); if(f) f.focus(); }
}
function muAdd(r){
  if(!r || !/^[A-Za-z0-9_-]{11}$/.test(r.v || "")) return;
  if(!muSend("add", {v:r.v, title:(r.title || "").slice(0, 150)})){ toast("Not connected to the room", "ach"); return; }
  MU.results = []; muRenderResults();
  var i = $("muAddInput"); if(i) i.value = "";
  if(!MU.tuned) muTune(true);
  toast("🎵 Queued in " + muPlace(), "level");
}
// "Play in room" for any track (yours or a friend's): straight in if we know its YouTube id, else search.
function muFindFor(t){
  if(!t) return;
  if(t.youtubeId){ muAdd({v:t.youtubeId, title:t.title + (t.artist ? " — " + t.artist : "")}); return; }
  var q = (t.title + " " + (t.artist || "")).trim();
  var i = $("muAddInput"); if(i){ i.value = q; i.focus(); }
  muSearch(q);
  var r = $("muRoom"); if(r && r.scrollIntoView) r.scrollIntoView({block:"nearest", behavior: muCalm() ? "auto" : "smooth"});
}

/* ---------- visualiser ---------- */
// Levels 0..1 per band, from (in order) your own captured audio, the room DJ's frames, or a beat
// seeded by the current song. Never Math.random: the same song always dances the same way.
function muLevels(now){
  var out = new Array(MU_BANDS), i;
  if(MU.dj && MU.dj.an){
    var buf = MU.dj.buf; MU.dj.an.getByteFrequencyData(buf);
    var bands = muBandsFrom(buf);
    for(i = 0; i < MU_BANDS; i++) out[i] = bands[i] / 255;
    if(now - MU.djSendAt > 100){ MU.djSendAt = now; muSend("viz", {b:bands}); }
    return out;
  }
  if(MU.auAn && MU.tuned && MU.auV && MU.au && !MU.au.paused){
    MU.auAn.getByteFrequencyData(MU.auBuf);
    var rb = muBandsFrom(MU.auBuf);
    for(i = 0; i < MU_BANDS; i++) out[i] = rb[i] / 255;
    return out;
  }
  if(MU.listen && MU.listen.an){
    var lb = MU.listen.buf; MU.listen.an.getByteFrequencyData(lb);
    var lbands = muBandsFrom(lb);
    for(i = 0; i < MU_BANDS; i++) out[i] = lbands[i] / 255;
    return out;
  }
  if(MU.remote && now - MU.remote.at < 1200){
    for(i = 0; i < MU_BANDS; i++) out[i] = MU.remote.b[i] / 255;
    return out;
  }
  var s = MU.st, playing = !!(s && s.cur && s.playing && MU.tuned);
  if(!playing){ for(i = 0; i < MU_BANDS; i++) out[i] = 0.04; return out; }
  var r = mulberry32(hashStr("viz:" + s.cur.v)), bpm = 92 + Math.floor(r()*48), t = muPos() / 1000;
  var beat = (t * bpm / 60) % 1, kick = Math.exp(-beat * 5);
  for(i = 0; i < MU_BANDS; i++){
    var ph = r() * 6.283, sp = 0.7 + r() * 1.8, low = 1 - i / MU_BANDS;
    var v = 0.28 + 0.22 * Math.sin(t * sp + ph) + 0.5 * kick * low * (0.6 + 0.4 * Math.sin(t * 0.5 + ph));
    out[i] = Math.max(0.03, Math.min(1, v));
  }
  return out;
}
// 128 FFT bins -> 16 bands, spaced roughly logarithmically so the bass gets its share.
function muBandsFrom(buf){
  var n = buf.length, out = [], lo = 1;
  for(var i = 0; i < MU_BANDS; i++){
    var hi = Math.max(lo + 1, Math.round(Math.pow(n, (i + 1) / MU_BANDS)));
    var sum = 0, c = 0;
    for(var k = lo; k < Math.min(hi, n); k++){ sum += buf[k]; c++; }
    out.push(c ? Math.min(255, Math.round(sum / c)) : 0);
    lo = hi;
  }
  return out;
}
function muColors(){
  var cs = getComputedStyle(document.documentElement);
  function v(n, f){ var x = (cs.getPropertyValue(n) || "").trim(); return x || f; }
  return {a:v("--brand", "#7c6cf0"), b:v("--brand2", "#5fd3e6"), c:v("--good", "#3fbf7f"), bg:v("--panel2", "#111"), line:v("--line", "#333")};
}
function muDraw(now){
  var cv = $("muViz"); if(!cv) return;
  var w = cv.clientWidth, h = cv.clientHeight; if(!w || !h) return;
  var dpr = Math.min(2, window.devicePixelRatio || 1);
  if(cv.width !== Math.round(w*dpr) || cv.height !== Math.round(h*dpr)){ cv.width = Math.round(w*dpr); cv.height = Math.round(h*dpr); }
  var g = cv.getContext("2d"); g.setTransform(dpr, 0, 0, dpr, 0, 0);
  var lv = muLevels(now), i;
  // Ease toward the new levels so frames don't flicker.
  if(!MU.levels) MU.levels = lv.slice();
  for(i = 0; i < MU_BANDS; i++) MU.levels[i] += (lv[i] - MU.levels[i]) * (muCalm() ? 1 : 0.35);
  var L = MU.levels, col = muColors();
  g.clearRect(0, 0, w, h);
  var grad = g.createLinearGradient(0, h, 0, 0); grad.addColorStop(0, col.a); grad.addColorStop(0.6, col.b); grad.addColorStop(1, col.c);
  g.fillStyle = grad; g.strokeStyle = grad;
  if(MU.mode === "ring"){
    var cx = w/2, cy = h/2, r0 = Math.min(w, h) * 0.2, rr = Math.min(w, h) * 0.28, n = MU_BANDS * 4;
    g.lineWidth = Math.max(2, Math.min(w, h) / 90); g.lineCap = "round";
    for(i = 0; i < n; i++){
      var lvv = L[(i < n/2 ? i : n - 1 - i) % MU_BANDS], a = i / n * Math.PI * 2 - Math.PI/2;
      var r1 = r0 + 4, r2 = r0 + 4 + lvv * rr;
      g.beginPath(); g.moveTo(cx + Math.cos(a)*r1, cy + Math.sin(a)*r1); g.lineTo(cx + Math.cos(a)*r2, cy + Math.sin(a)*r2); g.stroke();
    }
    g.globalAlpha = 0.25 + 0.5 * L[0]; g.beginPath(); g.arc(cx, cy, r0 * (0.8 + 0.25 * L[0]), 0, Math.PI*2); g.fill(); g.globalAlpha = 1;
  } else if(MU.mode === "wave"){
    g.lineWidth = Math.max(2, h / 60); g.lineJoin = "round";
    for(var side = -1; side <= 1; side += 2){
      g.beginPath();
      for(i = 0; i <= MU_BANDS * 2; i++){
        var x = i / (MU_BANDS * 2) * w, li = L[Math.min(MU_BANDS-1, i < MU_BANDS ? i : MU_BANDS*2 - i)];
        var y = h/2 + side * li * h * 0.42 * Math.sin(i * 0.9 + now / 500 * (muCalm() ? 0 : 1) + 1.2);
        if(i === 0) g.moveTo(x, y); else g.lineTo(x, y);
      }
      g.stroke();
    }
  } else {
    var gap = Math.max(2, w / 160), bw = (w - gap * (MU_BANDS*2 + 1)) / (MU_BANDS*2);
    for(i = 0; i < MU_BANDS*2; i++){
      var li2 = L[i < MU_BANDS ? MU_BANDS - 1 - i : i - MU_BANDS], bh = Math.max(2, li2 * (h - 8));
      var bx = gap + i * (bw + gap);
      g.fillRect(bx, h - bh, bw, bh);
    }
  }
}
function muLoop(now){
  MU.raf = 0;
  var full = !!(document.fullscreenElement && document.fullscreenElement.id === "muVizWrap");
  if((!muOn() && !full) || document.hidden){ return; }
  // Calm / reduced motion: a still picture refreshed twice a second instead of 60 fps motion.
  if(!muCalm() || now - MU.lastDraw > 500){ MU.lastDraw = now; muDraw(now); }
  else if(MU.dj) muLevels(now);   // keep sending the DJ's frames even when not drawing
  if(Math.floor(now / 250) !== Math.floor((MU.progAt || 0) / 250)){ MU.progAt = now; muRenderProgress(); }
  MU.raf = requestAnimationFrame(muLoop);
}
function muStartLoop(){ if(!MU.raf) MU.raf = requestAnimationFrame(muLoop); }
function muSetMode(m){
  MU.mode = m; try { localStorage.setItem("hq_mu_mode", m); } catch(e){}
  Array.prototype.forEach.call(document.querySelectorAll(".mu-mode"), function(b){ var on = b.getAttribute("data-mode") === m; b.classList.toggle("on", on); b.setAttribute("aria-pressed", on ? "true" : "false"); });
  if(muCalm()) muDraw(performance.now());
}
// Be the DJ: share a tab's (or the screen's) audio; we analyse it here and send only 16 levels.
function muDjStart(){
  if(MU.dj) return muDjStop();
  if(!navigator.mediaDevices || !navigator.mediaDevices.getDisplayMedia){ toast("This browser can’t share audio", "ach"); return; }
  navigator.mediaDevices.getDisplayMedia({video:true, audio:{echoCancellation:false, noiseSuppression:false, autoGainControl:false, channelCount:2},
                                          systemAudio:"include", selfBrowserSurface:"exclude", surfaceSwitching:"include"})
    .then(function(stream){
      var at = stream.getAudioTracks();
      stream.getVideoTracks().forEach(function(t){ t.stop(); });   // only the sound is used
      if(!at.length){ toast("No audio shared. Pick the tab playing music (e.g. open.spotify.com) and tick “Share tab audio”.", "ach"); return; }
      try { at[0].contentHint = "music"; } catch(e){}
      var Ctx = window.AudioContext || window.webkitAudioContext;
      var ctx = new Ctx(), src = ctx.createMediaStreamSource(new MediaStream(at)), an = ctx.createAnalyser();
      an.fftSize = 256; an.smoothingTimeConstant = 0.6; src.connect(an);   // not to the speakers: no echo
      MU.dj = {ctx:ctx, an:an, buf:new Uint8Array(an.frequencyBinCount), tracks:at, stream:new MediaStream(at)};
      at[0].addEventListener("ended", muDjStop);
      if(MU.listen) muListenStop();
      muLiveSay("live");
      muDjRender(); muRenderRoom(); muStartLoop();
      announce("You’re live. Everyone in " + muPlace() + " can listen.");
    })
    .catch(function(e){ if(e && e.name !== "NotAllowedError") toast("Couldn’t capture audio", "ach"); });
}
function muDjStop(){
  if(!MU.dj) return;
  try { MU.dj.tracks.forEach(function(t){ t.stop(); }); } catch(e){}
  try { MU.dj.ctx.close(); } catch(e){}
  MU.dj = null;
  Object.keys(MU.out).forEach(muOutClose);
  muLiveSay("off");
  muDjRender(); muRenderRoom();
}
function muDjRender(){
  var b = $("muDj"); if(!b) return;
  b.textContent = MU.dj ? "⏹ End live" : "🔴 Go live";
  b.classList.toggle("on", !!MU.dj); b.setAttribute("aria-pressed", MU.dj ? "true" : "false");
}
function muFullscreen(){
  var w = $("muVizWrap"); if(!w) return;
  if(document.fullscreenElement) { document.exitFullscreen().catch(function(){}); return; }
  if(w.requestFullscreen) w.requestFullscreen().then(muStartLoop).catch(function(){});
}

/* ---------- Go live: the DJ's audio, browser to browser ----------
   say    {kind:"mudj", op:"live"|"off"|"ask", peer}                 who is live, to the room
   signal {to:userId, data:{kind:"mudj", op:"join"|"offer"|"answer"|"ice"|"bye", fromPeer, toPeer, sdp, candidate}}
   A listener asks to join; the DJ makes one send-only connection per listener (up to MU_LIVE_MAX) carrying
   just the shared audio. The Arena relays the setup to that one person, never the audio. */
var MU_LIVE_MAX = 12;
var MU_PEER_RE = /^[a-f0-9]{16}$/;
MU.peer = (typeof voiceNewPeerId === "function") ? voiceNewPeerId() : "0000000000000000";
MU.out = {};      // as the DJ: listener peer id -> {pc, userId, ice}
MU.lives = {};    // DJs live in this room: peer id -> {userId, who, at}
MU.listen = null; // as a listener: {peer, userId, who, pc, audio, an, buf, ice, ctx}
function muIce(){ return typeof VCHAN_ICE !== "undefined" ? VCHAN_ICE : []; }
function muLiveSay(op){
  if(!muSockOk()) return;
  try { ARENA.sock.send(JSON.stringify({type:"say", data:{kind:"mudj", op:op, peer:MU.peer}})); } catch(e){}
}
function muLiveSignal(userId, data){
  if(!muSockOk()) return;
  data.kind = "mudj"; data.fromPeer = MU.peer;
  try { ARENA.sock.send(JSON.stringify({type:"signal", to:userId, data:data})); } catch(e){}
}
function muLiveOnLobby(){ MU.lives = {}; muLiveSay("ask"); if(MU.dj) muLiveSay("live"); }
function muLiveOnLost(){
  MU.lives = {};
  Object.keys(MU.out).forEach(muOutClose);
  if(MU.listen) muListenStop(true);
}
// Opus set up for music on this connection: stereo, up to 256 kbps, no silence suppression.
function muHiFi(sdp){
  try {
    var m = /a=rtpmap:(\d+) opus\/48000\/2/i.exec(sdp); if(!m) return sdp;
    var pt = m[1], re = new RegExp("a=fmtp:" + pt + " ([^\\r\\n]*)");
    var keys = /^(stereo|sprop-stereo|maxaveragebitrate|usedtx|useinbandfec)=/;
    var want = "stereo=1;sprop-stereo=1;maxaveragebitrate=256000;usedtx=0;useinbandfec=1";
    if(re.test(sdp)) return sdp.replace(re, function(_, p){ return "a=fmtp:" + pt + " " + p.split(";").filter(function(x){ return x && !keys.test(x); }).concat([want]).join(";"); });
    return sdp.replace(m[0], m[0] + "\r\na=fmtp:" + pt + " " + want);
  } catch(e){ return sdp; }
}
function musicOnSay(m){
  var d = m.data, from = m.from;
  if(!d || !from || typeof from.userId !== "string" || typeof d.peer !== "string" || !MU_PEER_RE.test(d.peer) || d.peer === MU.peer) return;
  if(d.op === "ask"){ if(MU.dj) muLiveSay("live"); return; }
  if(d.op === "live"){
    var fresh = !MU.lives[d.peer];
    MU.lives[d.peer] = {userId:from.userId, who:arenaWho(from), at:Date.now()};
    if(fresh) muNote(arenaWho(from) + " went live 🔴");
  } else if(d.op === "off"){
    delete MU.lives[d.peer];
    if(MU.listen && MU.listen.peer === d.peer){ muListenStop(true); muNote(arenaWho(from) + " ended their live set"); }
  } else return;
  muRenderRoom();
}
function musicOnSignal(m){
  var d = m.data, from = m.from;
  if(!d || !from || typeof from.userId !== "string" || d.toPeer !== MU.peer || typeof d.fromPeer !== "string" || !MU_PEER_RE.test(d.fromPeer)) return;
  var peer = d.fromPeer;
  // ---- as the DJ ----
  if(d.op === "join" && MU.dj){
    if(!MU.out[peer] && Object.keys(MU.out).length >= MU_LIVE_MAX){ muLiveSignal(from.userId, {op:"bye", toPeer:peer, why:"full"}); return; }
    muOutClose(peer);
    var pc = new RTCPeerConnection({iceServers:muIce()});
    var o = MU.out[peer] = {pc:pc, userId:from.userId, ice:[]};
    MU.dj.tracks.forEach(function(t){
      var snd = pc.addTrack(t, MU.dj.stream);
      try {
        var prm = snd.getParameters();
        if(!prm.encodings || !prm.encodings.length) prm.encodings = [{}];
        prm.encodings[0].maxBitrate = 256000;
        snd.setParameters(prm).catch(function(){});
      } catch(e){}
    });
    pc.onicecandidate = function(e){ if(e.candidate) muLiveSignal(o.userId, {op:"ice", toPeer:peer, candidate:e.candidate.toJSON()}); };
    pc.onconnectionstatechange = function(){ if(pc.connectionState === "failed" || pc.connectionState === "closed") muOutClose(peer); muRenderRoom(); };
    pc.createOffer()
      .then(function(of){ return pc.setLocalDescription({type:"offer", sdp:muHiFi(of.sdp)}).catch(function(){ return pc.setLocalDescription(of); }); })
      .then(function(){ muLiveSignal(o.userId, {op:"offer", toPeer:peer, sdp:pc.localDescription.sdp}); })
      .catch(function(){ muOutClose(peer); });
    muRenderRoom();
    return;
  }
  if(d.op === "answer" && MU.out[peer] && typeof d.sdp === "string" && d.sdp.length < 15000){
    var q = MU.out[peer];
    q.pc.setRemoteDescription({type:"answer", sdp:d.sdp})
      .then(function(){ q.ice.splice(0).forEach(function(c){ q.pc.addIceCandidate(c).catch(function(){}); }); })
      .catch(function(){ muOutClose(peer); });
    return;
  }
  if(d.op === "bye"){
    if(MU.out[peer]){ muOutClose(peer); muRenderRoom(); }
    if(MU.listen && MU.listen.peer === peer){ muListenStop(true); toast(d.why === "full" ? "That live set is full" : "The live set ended", "ach"); }
    return;
  }
  // ---- as a listener ----
  if(d.op === "offer" && MU.listen && MU.listen.peer === peer && typeof d.sdp === "string" && d.sdp.length < 15000){
    var L = MU.listen;
    L.pc.setRemoteDescription({type:"offer", sdp:d.sdp})
      .then(function(){ L.ice.splice(0).forEach(function(c){ L.pc.addIceCandidate(c).catch(function(){}); }); return L.pc.createAnswer(); })
      .then(function(a){ return L.pc.setLocalDescription({type:"answer", sdp:muHiFi(a.sdp)}).catch(function(){ return L.pc.setLocalDescription(a); }); })
      .then(function(){ muLiveSignal(L.userId, {op:"answer", toPeer:peer, sdp:L.pc.localDescription.sdp}); })
      .catch(function(){ muListenStop(); toast("Couldn’t connect to the live set", "ach"); });
    return;
  }
  if(d.op === "ice" && d.candidate && typeof d.candidate === "object"){
    var c = MU.out[peer] || (MU.listen && MU.listen.peer === peer ? MU.listen : null);
    if(c){ if(c.pc.remoteDescription) c.pc.addIceCandidate(d.candidate).catch(function(){}); else c.ice.push(d.candidate); }
  }
}
// Someone who left the room without saying "off" (closed the lid): drop their live set and our sends to them.
function muLivePrune(){
  if(!muSockOk()) return;
  var here = {}; (ARENA.lobby || []).forEach(function(m){ if(m && m.userId) here[m.userId] = 1; });
  var changed = false;
  Object.keys(MU.lives).forEach(function(p){ if(!here[MU.lives[p].userId]){ delete MU.lives[p]; changed = true; } });
  Object.keys(MU.out).forEach(function(p){ if(!here[MU.out[p].userId]){ muOutClose(p); changed = true; } });
  if(MU.listen && !here[MU.listen.userId]){ muListenStop(true); muNote("The live set ended"); changed = true; }
  if(changed) muRenderRoom();
}
function muOutClose(peer){
  var o = MU.out[peer]; if(!o) return;
  delete MU.out[peer];
  try { o.pc.close(); } catch(e){}
}
function muListen(peer){
  var dj = MU.lives[peer]; if(!dj) return;
  if(MU.listen) muListenStop();
  if(MU.tuned) muTune(false);   // one thing at a time: the room's YouTube player stops while you listen live
  var pc = new RTCPeerConnection({iceServers:muIce()});
  var L = MU.listen = {peer:peer, userId:dj.userId, who:dj.who, pc:pc, audio:null, an:null, buf:null, ice:[], ctx:null};
  pc.onicecandidate = function(e){ if(e.candidate) muLiveSignal(L.userId, {op:"ice", toPeer:peer, candidate:e.candidate.toJSON()}); };
  pc.ontrack = function(e){
    // Play it as soon as it arrives: the default buffer is sized for calls and adds most of the delay.
    try { if(e.receiver){ if("jitterBufferTarget" in e.receiver) e.receiver.jitterBufferTarget = 0; e.receiver.playoutDelayHint = 0; } } catch(err){}
    var stream = (e.streams && e.streams[0]) || new MediaStream([e.track]);
    if(!L.audio){
      L.audio = document.createElement("audio"); L.audio.autoplay = true; L.audio.setAttribute("playsinline", "");
      var host = $("muLiveAudio"); if(host) host.appendChild(L.audio);
    }
    L.audio.srcObject = stream; L.audio.volume = MU.volume / 100;
    var pl = L.audio.play(); if(pl && pl.catch) pl.catch(function(){ toast("Press Listen live again to start the sound", "ach"); });
    // Our own analyser on what we hear: the visualiser follows the real music.
    try {
      var Ctx = window.AudioContext || window.webkitAudioContext;
      L.ctx = new Ctx(); var an = L.ctx.createAnalyser(); an.fftSize = 256; an.smoothingTimeConstant = 0.6;
      L.ctx.createMediaStreamSource(stream).connect(an); L.an = an; L.buf = new Uint8Array(an.frequencyBinCount);
    } catch(err){}
    muStartLoop();
  };
  pc.onconnectionstatechange = function(){
    if(MU.listen !== L) return;
    if(pc.connectionState === "failed"){ muListenStop(); toast("Lost the live set", "ach"); return; }
    muRenderRoom();
  };
  muLiveSignal(L.userId, {op:"join", toPeer:peer});
  announce("Listening live to " + dj.who);
  muRenderRoom();
}
function muListenStop(quiet){
  var L = MU.listen; if(!L) return;
  MU.listen = null;
  if(!quiet) muLiveSignal(L.userId, {op:"bye", toPeer:L.peer});
  try { L.pc.close(); } catch(e){}
  try { if(L.audio){ L.audio.srcObject = null; L.audio.remove(); } } catch(e){}
  try { if(L.ctx) L.ctx.close(); } catch(e){}
  muRenderRoom();
}
function muRenderLive(){
  var box = $("muLive"); if(!box) return;
  var peers = Object.keys(MU.lives);
  var sig = peers.join(",") + "|" + (MU.listen ? MU.listen.peer + ":" + MU.listen.pc.connectionState : "") + "|" + (MU.dj ? Object.keys(MU.out).length : "-");
  if(sig === box.getAttribute("data-sig")) return;
  box.setAttribute("data-sig", sig);
  box.textContent = "";
  box.classList.toggle("hidden", !peers.length && !MU.dj);
  if(MU.dj){
    var me = el("div", "mu-live-row mine");
    var t = el("span", "mu-live-dot"); t.setAttribute("aria-hidden", "true"); me.appendChild(t);
    var n = Object.keys(MU.out).length, w = el("span");
    w.textContent = "You’re live in " + muPlace() + " · " + n + " listening"; me.appendChild(w);
    box.appendChild(me);
  }
  peers.forEach(function(p){
    var L = MU.lives[p], on = !!(MU.listen && MU.listen.peer === p);
    var row = el("div", "mu-live-row");
    var dot = el("span", "mu-live-dot"); dot.setAttribute("aria-hidden", "true"); row.appendChild(dot);
    var who = el("span"); who.textContent = L.who + " is live";
    if(on) who.textContent += MU.listen.pc.connectionState === "connected" ? " · you’re listening" : " · connecting…";
    row.appendChild(who);
    var b = el("button", "mu-btn" + (on ? " on" : "")); b.type = "button";
    b.textContent = on ? "Stop listening" : "🎧 Listen live";
    b.setAttribute("aria-pressed", on ? "true" : "false");
    b.addEventListener("click", function(){ if(on) muListenStop(); else muListen(p); });
    row.appendChild(b);
    box.appendChild(row);
  });
}

/* ---------- view lifecycle + wiring ---------- */
function musicEnter(){
  if(ARENA.paired) arenaOpenSocket();
  muLoadMine(); muLoadPeople();
  muRenderRoom(); muDjRender(); muSetMode(MU.mode);
  var vol = $("muVol"); if(vol) vol.value = MU.volume;
  var vb = $("muVideo"); if(vb){ vb.checked = MU.video; }
  muStartLoop();
}
function musicLeave(){ /* the player keeps playing while you're tuned in; the loop stops itself */ }
(function(){
  var sh = $("muShare"); if(sh) sh.addEventListener("change", function(){ muSetShare(sh.checked); });
  var tn = $("muTune"); if(tn) tn.addEventListener("click", function(){ muTune(!MU.tuned); });
  var pp = $("muPlayPause"); if(pp) pp.addEventListener("click", function(){ var s = MU.st; if(!s || !s.cur) return; if(!MU.tuned) muTune(true); muSend(s.playing ? "pause" : "play"); });
  var sk = $("muSkip"); if(sk) sk.addEventListener("click", function(){ var s = MU.st; if(s && s.cur) muSend("next", {id:s.cur.id}); });
  var cl = $("muClear"); if(cl) cl.addEventListener("click", function(){ muSend("clear"); });
  var pr = $("muProg");
  if(pr){
    function seekTo(frac){ var s = MU.st; if(!s || !s.cur || !s.cur.durMs) return; muSend("seek", {ms:Math.round(Math.max(0, Math.min(1, frac)) * s.cur.durMs)}); }
    pr.addEventListener("click", function(e){ var r = pr.getBoundingClientRect(); seekTo((e.clientX - r.left) / r.width); });
    pr.addEventListener("keydown", function(e){
      var s = MU.st; if(!s || !s.cur || !s.cur.durMs) return;
      var step = e.key === "ArrowRight" ? 10000 : e.key === "ArrowLeft" ? -10000 : 0; if(!step) return;
      e.preventDefault(); seekTo((muPos() + step) / s.cur.durMs);
    });
  }
  var f = $("muAddForm");
  if(f) f.addEventListener("submit", function(e){ e.preventDefault(); clearTimeout(MU.typeT); var i = $("muAddInput"); muSearch(i ? i.value : ""); });
  var ai = $("muAddInput");
  if(ai){
    // Songs pop up as you type (a link waits for Enter / Search).
    ai.addEventListener("input", function(){
      clearTimeout(MU.typeT);
      var q = ai.value.trim();
      if(q.length < 2){ MU.searchSeq = (MU.searchSeq || 0) + 1; MU.results = []; muRenderResults(); return; }
      if(/^(https?:\/\/|www\.|youtu)/i.test(q)) return;
      MU.typeT = setTimeout(function(){ muSearch(q, true); }, 300);
    });
    ai.addEventListener("keydown", function(e){
      if(e.key === "ArrowDown"){ var b = document.querySelector("#muResults .mu-res-btn"); if(b){ e.preventDefault(); b.focus(); } }
      else if(e.key === "Escape" && MU.results.length){ e.preventDefault(); MU.results = []; muRenderResults(); }
    });
  }
  var rl = $("muResults");
  if(rl) rl.addEventListener("keydown", function(e){
    if(e.key !== "ArrowDown" && e.key !== "ArrowUp") return;
    var bs = Array.prototype.slice.call(rl.querySelectorAll(".mu-res-btn")), i = bs.indexOf(document.activeElement);
    if(i < 0) return;
    e.preventDefault();
    if(e.key === "ArrowUp" && i === 0){ var inp = $("muAddInput"); if(inp) inp.focus(); return; }
    var n = bs[Math.max(0, Math.min(bs.length - 1, i + (e.key === "ArrowDown" ? 1 : -1)))]; if(n) n.focus();
  });
  var vol = $("muVol");
  if(vol) vol.addEventListener("input", function(){ MU.volume = Math.max(0, Math.min(100, parseInt(vol.value, 10) || 0)); try { localStorage.setItem("hq_mu_vol", String(MU.volume)); } catch(e){} muPost("setVolume", [MU.volume]); if(MU.listen && MU.listen.audio) MU.listen.audio.volume = MU.volume / 100; if(MU.au) MU.au.volume = MU.volume / 100; });
  var vb = $("muVideo");
  if(vb) vb.addEventListener("change", function(){ MU.video = vb.checked; try { localStorage.setItem("hq_mu_video", MU.video ? "1" : "0"); } catch(e){} var h = $("muPlayer"); if(h) h.classList.toggle("novideo", !MU.video); });
  var dj = $("muDj"); if(dj) dj.addEventListener("click", muDjStart);
  var fs = $("muFull"); if(fs) fs.addEventListener("click", muFullscreen);
  Array.prototype.forEach.call(document.querySelectorAll(".mu-mode"), function(b){ b.addEventListener("click", function(){ muSetMode(b.getAttribute("data-mode")); }); });
  document.addEventListener("visibilitychange", function(){ if(!document.hidden && muOn()) muStartLoop(); });
  document.addEventListener("fullscreenchange", muStartLoop);
  // Drift check while tuned in, and the polls: yours every 5 s on this view, everyone's every 20 s while paired.
  setInterval(function(){ if(MU.tuned) muSyncPlayer(false); }, 2000);
  // Does this HQ have yt-dlp? (asked once; a check with a harmless id that is never fetched)
  fetch("/api/music/audio-state?v=-", {cache:"no-store"}).then(function(r){ return r.json(); })
    .then(function(j){ MU.ytdlp = !!(j && j.state !== "unavailable"); }).catch(function(){ MU.ytdlp = false; });
  setInterval(function(){ if(muOn()) muLoadMine(); }, 2000);
  setInterval(function(){ if(muOn() && !document.hidden) muTickAll(); }, 250);
  setInterval(function(){ if(ARENA.paired && (muOn() || muSockOk())) muLoadPeople(); }, 20000);
  setInterval(function(){
    muLivePrune();
    var sig = [ARENA.paired, muSockOk(), muArenaHas(), ARENA.roomId, MU.tuned].join("|");
    if(sig !== MU.sig){ MU.sig = sig; if(muOn()) muRenderRoom(); }
  }, 1000);
})();
