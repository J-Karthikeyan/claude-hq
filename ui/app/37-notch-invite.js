/* ---------- "New: HQ Notch" -- shown once when you next open HQ on a Mac without it ----------
   HQ Notch (notch/) puts your party in the Mac's notch, with approvals from there. This asks
   once, the next time HQ opens on a Mac where it is not installed: Install now (ext_notch.py runs
   notch/tools/install.py), Copy the command, Not now (again in 3 days), or Don't show again.
   It never opens over another dialog, and nothing here leaves the machine. Settings > HQ Notch
   and the command palette reopen it any time (openNotchInvite), installed or not. */
var NOTCH_INVITE_KEY = "hq_notch_invite";          // "never" | "installed" | "later:<ms>"
var NOTCH_INVITE_LATER = 3*24*3600*1000;
var NOTCH_INVITE = {timer: 0, poll: 0, opener: null, manual: false};
function notchInviteSeen(){
  var v = ""; try { v = localStorage.getItem(NOTCH_INVITE_KEY) || ""; } catch(e){}
  if(v === "never" || v === "installed") return true;
  var m = /^later:(\d+)$/.exec(v); return !!(m && Date.now() - (+m[1]) < NOTCH_INVITE_LATER);
}
function notchInviteRemember(v){ try { localStorage.setItem(NOTCH_INVITE_KEY, v); } catch(e){} }
// {stale: true} when this server has no notch routes yet: a Python HQ that was not restarted
// after a pull, or a Rust HQ (dashboard-rs) built before it learned them (it needs one rebuild).
function notchStatus(){
  return fetch("/api/notch/status").then(function(r){
    if(r.status === 404) return {stale: true};
    return r.ok ? r.json() : null;
  }).catch(function(){ return null; });
}
function notchServerIsRust(){
  return fetch("/api/server-stats").then(function(r){ return r.ok ? r.json() : null; })
    .then(function(j){ return !!(j && j.impl === "rust"); }).catch(function(){ return false; });
}
var NOTCH_RESTART = "launchctl kickstart -k gui/$(id -u)/com.claudehq.dashboard";
var NOTCH_REBUILD = "cargo build --release --manifest-path dashboard-rs/Cargo.toml && " + NOTCH_RESTART;
function notchOpenStale(manual){
  notchServerIsRust().then(function(rust){ notchInviteOpen({stale: true, rust: rust}, manual); });
}

// Look once, a few seconds after the page settles; wait while another dialog is open.
function notchInviteCheck(tries){
  tries = tries|0;
  if(notchInviteSeen() || tries > 20) return;
  if(typeof currentModal === "function" && currentModal()){ NOTCH_INVITE.timer = setTimeout(function(){ notchInviteCheck(tries + 1); }, 15000); return; }
  notchStatus().then(function(s){
    if(s && s.stale){ notchOpenStale(false); return; }
    if(!s || !s.mac || !s.available) return;
    if(s.installed){ notchInviteRemember("installed"); return; }
    notchInviteOpen(s);
  });
}

function notchInviteModal(){
  var back = $("notchInviteBack"); if(back) return back;
  back = document.createElement("div"); back.className = "tcard-back"; back.id = "notchInviteBack"; back.setAttribute("aria-hidden", "true");
  var m = document.createElement("div"); m.className = "tcard-modal ni-modal"; m.setAttribute("role", "dialog"); m.setAttribute("aria-modal", "true"); m.setAttribute("aria-labelledby", "notchInviteTitle");
  var top = document.createElement("div"); top.className = "tcard-top";
  var t = document.createElement("b"); t.id = "notchInviteTitle"; t.textContent = "New: HQ Notch";
  var x = document.createElement("button"); x.type = "button"; x.className = "hbtn icon ghost"; x.id = "notchInviteClose"; x.setAttribute("aria-label", "Not now"); x.textContent = "✕";
  top.appendChild(t); top.appendChild(x); m.appendChild(top);
  var body = document.createElement("div"); body.id = "notchInviteBody"; body.className = "ni-body"; m.appendChild(body);
  back.appendChild(m); document.body.appendChild(back);
  x.addEventListener("click", function(){ notchInviteClose("later"); });
  back.addEventListener("click", function(e){ if(e.target === back) notchInviteClose("later"); });
  back.addEventListener("keydown", function(e){ if(e.key === "Escape"){ e.preventDefault(); e.stopPropagation(); notchInviteClose("later"); } });
  return back;
}
function niEl(tag, cls, text){ var n = document.createElement(tag); if(cls) n.className = cls; if(text != null) n.textContent = text; return n; }

function notchInviteRender(s, phase, msg){
  var b = $("notchInviteBody"); if(!b) return;
  b.textContent = "";
  b.appendChild(niEl("p", "ni-lead", "Your Claude HQ party, living in the Mac's notch."));
  var ul = niEl("ul", "ni-list");
  ["Every live session as its HQ Pokémon: working, needs you, done",
   "Allow, Always or Deny permission prompts, answer Claude's questions and approve plans, right from the notch",
   "A live ticker of each session's tools and edits; click a card to jump to its terminal",
   "Shares the notch with Sapphire, NotchNook and other notch apps; phone alerts if you want them"
  ].forEach(function(line){ ul.appendChild(niEl("li", null, line)); });
  b.appendChild(ul);
  b.appendChild(niEl("p", "ni-note muted", "Installing builds the app (about a minute), puts it in ~/Applications, starts it at login, and adds HQ Notch's hooks to ~/.claude/settings.json (backed up first; only its own entries)."));
  var row = niEl("div", "ni-actions");
  if(phase === "stale"){
    var rust = !!(s && s.rust), cmd = rust ? NOTCH_REBUILD : NOTCH_RESTART;
    b.appendChild(niEl("p", "ni-error", rust
      ? "Your HQ runs on Rust (claudehq) and was built before it learned about HQ Notch. Rebuild it once; after that, HQ's Update rebuilds it for you."
      : "This HQ server started before HQ Notch was added. Restart it (or use Update) and this dialog will offer the install."));
    b.appendChild(niEl("p", "ni-note muted", "Run this in Terminal, in your Claude HQ folder:"));
    b.appendChild(niEl("code", "ni-cmd", cmd));
    var cpy = niEl("button", "hbtn on", "Copy command"); cpy.type = "button"; cpy.id = "notchInviteGo";
    cpy.addEventListener("click", function(){
      try { navigator.clipboard.writeText(cmd).then(function(){ if(typeof toast === "function") toast("Copied: run it in your Claude HQ folder"); }); } catch(e){}
    });
    var lat = niEl("button", "hbtn ghost", "Not now"); lat.type = "button"; lat.addEventListener("click", function(){ notchInviteClose("later"); });
    [cpy, lat].forEach(function(n){ row.appendChild(n); });
  } else if(phase === "installed"){
    row.appendChild(niEl("span", "ni-status ok", "HQ Notch is installed on this Mac" + (s && s.running ? " and running." : ".")));
    var re = niEl("button", "hbtn ghost", "Reinstall"); re.type = "button"; re.disabled = !(s && s.tools);
    re.addEventListener("click", notchInviteInstall); row.appendChild(re);
    var cl = niEl("button", "hbtn", "Close"); cl.type = "button"; cl.id = "notchInviteGo"; cl.addEventListener("click", function(){ notchInviteClose("installed"); }); row.appendChild(cl);
  } else if(phase === "running"){
    row.appendChild(niEl("span", "ni-status", "Installing… this takes about a minute."));
  } else if(phase === "done"){
    row.appendChild(niEl("span", "ni-status ok", "Installed. Look at your notch: new Claude tabs show up there."));
    var ok = niEl("button", "hbtn", "Done"); ok.type = "button"; ok.addEventListener("click", function(){ notchInviteClose("installed"); }); row.appendChild(ok);
  } else {
    if(msg) b.appendChild(niEl("p", "ni-error", msg));
    var go = niEl("button", "hbtn on", "Install now"); go.type = "button"; go.id = "notchInviteGo";
    go.disabled = !(s && s.tools);
    if(!(s && s.tools)) b.appendChild(niEl("p", "ni-note muted", "Needs the Xcode Command Line Tools first: run xcode-select --install in Terminal."));
    go.addEventListener("click", notchInviteInstall);
    var cp = niEl("button", "hbtn ghost", "Copy command"); cp.type = "button";
    cp.addEventListener("click", function(){
      var cmd = "python3 notch/tools/install.py";
      try { navigator.clipboard.writeText(cmd).then(function(){ if(typeof toast === "function") toast("Copied: run it in your Claude HQ folder"); }); } catch(e){}
    });
    var later = niEl("button", "hbtn ghost", "Not now"); later.type = "button"; later.addEventListener("click", function(){ notchInviteClose("later"); });
    var never = niEl("button", "hbtn ghost ni-never", "Don't show again"); never.type = "button"; never.addEventListener("click", function(){ notchInviteClose("never"); });
    [go, cp, later, never].forEach(function(n){ row.appendChild(n); });
  }
  b.appendChild(row);
}

function notchInviteOpen(s, manual){
  var back = notchInviteModal();
  NOTCH_INVITE.opener = document.activeElement; NOTCH_INVITE.manual = !!manual;
  back.classList.add("open"); back.setAttribute("aria-hidden", "false");
  notchInviteRender(s, s && s.stale ? "stale" : s && s.installed ? "installed" : "ask");
  var go = $("notchInviteGo"); (go && !go.disabled ? go : $("notchInviteClose")).focus();
  if(typeof announce === "function") announce("New: HQ Notch. Your Claude HQ party in the Mac's notch.");
}
function notchInviteClose(why){
  var back = $("notchInviteBack"); if(!back) return;
  if(why === "never" || why === "installed") notchInviteRemember(why);
  else if(!NOTCH_INVITE.manual) notchInviteRemember("later:" + Date.now());   // reopened by hand: closing keeps the old choice
  NOTCH_INVITE.manual = false;
  back.classList.remove("open"); back.setAttribute("aria-hidden", "true");
  if(NOTCH_INVITE.poll){ clearInterval(NOTCH_INVITE.poll); NOTCH_INVITE.poll = 0; }
  var o = NOTCH_INVITE.opener; NOTCH_INVITE.opener = null;
  if(o && o.focus && o.isConnected){ try { o.focus(); } catch(e){} }
}
function notchInviteInstall(){
  notchInviteRender(null, "running");
  fetch("/api/notch/install", {method: "POST", headers: {"Content-Type": "application/json", "X-HQ-Token": CSRF}, body: "{}"})
    .then(function(r){ return r.json().then(function(j){ return {ok: r.ok || r.status === 409, j: j}; }); })
    .then(function(res){
      if(!res.ok){ notchStatus().then(function(s){ notchInviteRender(s, "ask", (res.j && res.j.error) || "The install could not start."); }); return; }
      NOTCH_INVITE.poll = setInterval(function(){
        notchStatus().then(function(s){
          var j = s && s.job; if(!j || j.state === "running") return;
          clearInterval(NOTCH_INVITE.poll); NOTCH_INVITE.poll = 0;
          if(j.state === "done"){ notchInviteRender(s, "done"); notchInviteRemember("installed"); if(typeof announce === "function") announce("HQ Notch is installed."); }
          else notchInviteRender(s, "ask", "The install failed: " + String(j.log || "").split("\n").filter(Boolean).slice(-2).join(" "));
        });
      }, 2000);
    })
    .catch(function(){ notchStatus().then(function(s){ notchInviteRender(s, "ask", "HQ did not answer."); }); });
}
// Reopen by hand (Settings > HQ Notch, command palette), whatever was chosen before.
function openNotchInvite(){
  notchStatus().then(function(s){
    var say = function(m){ if(typeof toast === "function") toast(m); };
    if(s && s.stale){ notchOpenStale(true); return; }
    if(!s){ say("HQ did not answer."); return; }
    if(!s.mac){ say("HQ Notch is for Macs only."); return; }
    if(!s.available){ say("This HQ has no notch/ folder: update HQ first."); return; }
    notchInviteOpen(s, true);
  });
}
NOTCH_INVITE.timer = setTimeout(function(){ notchInviteCheck(0); }, 4000);
