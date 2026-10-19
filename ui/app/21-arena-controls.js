/* ---- Cali: California Burrito taco Tuesdays (the diner lives in the "cali" view) ----
   Three different things go wrong here and only one of them is the host's fault, so say which.
   The pantry's helpers tell the shapes apart: dashboard.py's own refusals carry "error" and no
   "detail" (pantryLocalReject), a stale local proxy 404s with {"error":"not found"} and an Arena
   without the routes 404s with a bare "Not Found" (pantryMissState). */
var CALI_RELOAD_MSG = "Claude HQ restarted since this page loaded. Reload the page to log the dinner — the plates stay as they are.";
var CALI_RESTART_MSG = "Restart Claude HQ, then reload this page, to finish updating it: the copy that’s running is older than this page and has no taco board yet.";
var CALI_UNSUPPORTED_MSG = "This Arena server doesn’t have the taco board yet — ask whoever hosts it to update it.";
// Pooled buy-1-get-1: every second taco at the table is free, whatever it is. Mirrors
// tacos.paid_tacos; it only powers the running receipt, the server owns the real number.
function caliPaid(total){ return Math.floor((total+1)/2); }

/* ---- Arena controls ---- */
(function(){ var m="players"; try{ m=localStorage.getItem("hq_arena_mode")||"players"; }catch(e){} arenaSetMode(m); })();
Array.prototype.forEach.call(document.querySelectorAll("#arenaMode .arena-win"), function(b){
  b.addEventListener("click", function(){ arenaSetMode(b.getAttribute("data-mode")); arenaLoadBoard(); });
});
Array.prototype.forEach.call(document.querySelectorAll("#arenaWins .arena-win"), function(b){
  b.addEventListener("click", function(){
    ARENA.window = b.getAttribute("data-window");
    Array.prototype.forEach.call(document.querySelectorAll("#arenaWins .arena-win"), function(o){
      o.classList.toggle("on", o===b);
    });
    arenaLoadBoard();
  });
});

(function(){
  var signin=$("arenaSignin"), pair=$("arenaPair"), pub=$("arenaPublish"), un=$("arenaUnpair");
  var prevBtn=$("arenaPreviewBtn"), prev=$("arenaPreview");
  if(prevBtn) prevBtn.addEventListener("click", function(){
    if(prev && !prev.classList.contains("hidden")){ prev.classList.add("hidden"); return; }
    prevBtn.disabled = true;
    fetch("/api/arena/preview",{cache:"no-store"}).then(function(r){return r.json();})
      .then(function(j){
        var days=(j.days||[]).length, note="// "+days+" day"+(days===1?"":"s")+" \u2014 this is the entire payload; nothing else leaves your machine\n";
        if(prev){ prev.textContent = note + JSON.stringify(j, null, 2); prev.classList.remove("hidden"); }
      }).catch(function(){ if(prev){ prev.textContent="// preview unavailable"; prev.classList.remove("hidden"); } })
      .then(function(){ prevBtn.disabled=false; });
  });

  if(signin) signin.addEventListener("click", function(){
    var url = ($("arenaUrl").value||"").trim().replace(/\/+$/,"");
    if(!/^https?:\/\//.test(url)){ arenaSay("Enter the Arena server URL first.","err"); return; }
    postConfig({arenaUrl:url}).then(function(){
      window.open(url + "/v1/auth/github/start", "_blank", "noopener");
      arenaSay("Sign in, then paste the code below.","ok");
    });
  });

  if(pair) pair.addEventListener("click", function(){
    var url = ($("arenaUrl").value||"").trim().replace(/\/+$/,"");
    var code = ($("arenaCode").value||"").trim().toUpperCase();
    if(!code){ arenaSay("Paste the pairing code.","err"); return; }
    arenaSay("Connecting…");
    postConfig({arenaUrl:url, arenaEnabled:true}).then(function(){
      return arenaPost("/api/arena/pair", {code:code, label:"claude-hq"});
    }).then(function(res){
      if(!res.ok){ arenaSay(res.j.error || "Pairing failed.","err"); return; }
      arenaSay("Connected. Publishing…","ok");
      return arenaPost("/api/arena/publish").then(function(){ loadArena(); });
    });
  });

  if(pub) pub.addEventListener("click", function(){
    pub.disabled = true;
    arenaPost("/api/arena/publish").then(function(res){
      pub.disabled = false;
      if(res.ok) toast("Published " + (res.j.accepted||0) + " days");
      loadArena();
    });
  });

  if(un) un.addEventListener("click", function(){
    if(!confirm("Disconnect this machine from the Arena? Your stats stay on the server.")) return;
    voiceLeave();
    ARENA.paired = false;
    arenaCloseSocket();
    ARENA.roomId="lobby"; ARENA.roomName="Lobby"; ARENA.roomRole=null; ARENA.rooms=null; ARENA.roomsOk=null; ARENA.roomConfirmed={}; ARENA.chatBy={};
    arenaRoomRemember("lobby"); arenaRoomPanelsClose(); arenaRenderRoomBar();
    arenaPost("/api/arena/unpair").then(function(){
      postConfig({arenaEnabled:false}).then(function(){ loadArena(); });
    });
  });
})();

if("serviceWorker" in navigator){ window.addEventListener("load",function(){
  navigator.serviceWorker.register("/sw.js").catch(function(){});
}); }
