"""
Arena deploy panel — a small control surface for a self-hosted instance.

Deliberate choices:

  * Stdlib only. This runs on the HOST, not in a container, because it drives
    git and docker compose. Adding a venv and dependencies to a box whose whole
    job is running containers is the wrong trade, and a smaller dependency
    surface on the thing that can execute deploys is worth the extra code.

  * Binds the loopback or the Docker bridge gateway only -- never a public
    interface. Caddy terminates TLS and proxies to it.

  * There is no free-form command input. Actions are a fixed set, so a bug in
    request parsing cannot become arbitrary execution.

  * Access is a hardcoded allowlist of GitHub logins checked on EVERY request,
    not just at login -- removing someone from the list logs them out at once
    rather than whenever their cookie happens to expire.
"""
import hashlib
import hmac
import html
import http.cookies
import json
import os
import re
import secrets
import subprocess
import time
import urllib.parse
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

PORT = int(os.environ.get("PANEL_PORT", "8090"))
# Caddy runs in a container, so 127.0.0.1 on the host is unreachable to it.
# The installer sets this to the Docker bridge gateway (e.g. 172.17.0.1),
# which containers and the host can reach but nothing outside can.
BIND = os.environ.get("PANEL_BIND", "127.0.0.1")
ARENA_DIR = os.environ.get("ARENA_DIR", "/repo")
ARENA_DOMAIN = os.environ.get("ARENA_DOMAIN", "")
PANEL_BASE_URL = os.environ.get("PANEL_BASE_URL", "")
DEPLOY_LOG = os.environ.get("ARENA_DEPLOY_LOG", "/repo/.arena-deploy.log")
BRANCH = os.environ.get("ARENA_BRANCH", "main")
HEALTH_URL = os.environ.get("ARENA_HEALTH_URL", "http://app:8080/health")

CLIENT_ID = os.environ.get("PANEL_GITHUB_CLIENT_ID", "")
CLIENT_SECRET = os.environ.get("PANEL_GITHUB_CLIENT_SECRET", "")
SECRET_KEY = os.environ.get("PANEL_SECRET_KEY", "")
ALLOWED = {u.strip().lower() for u in os.environ.get("PANEL_ALLOWED_USERS", "").split(",") if u.strip()}

SESSION_TTL = 12 * 3600
COOKIE = "arena_panel"


def _sign(payload: str) -> str:
    mac = hmac.new(SECRET_KEY.encode(), payload.encode(), hashlib.sha256).hexdigest()
    return f"{payload}.{mac}"


def _verify(token: str, purpose: str = "") -> str | None:
    """Check a signed token, optionally requiring a purpose prefix.

    Purpose-bound tokens carry "<purpose>:" *inside* the signed payload, so a
    session cookie cannot be replayed as a CSRF token or the reverse. The
    prefix therefore has to stay in the string the MAC is verified against --
    stripping it before calling here is what made every CSRF check fail.
    """
    try:
        payload, mac = token.rsplit(".", 1)
        login, exp = payload.split("|", 1)
    except ValueError:
        return None
    if not hmac.compare_digest(mac, hmac.new(SECRET_KEY.encode(), payload.encode(), hashlib.sha256).hexdigest()):
        return None
    if float(exp) < time.time():
        return None
    if purpose:
        want = purpose + ":"
        if not login.startswith(want):
            return None
        login = login[len(want):]
    elif ":" in login:
        # An unprefixed call must not accept a purpose-bound token.
        return None
    # Re-checked on every request: removing a login revokes access immediately.
    return login if login.lower() in ALLOWED else None


def run(args, cwd=None, timeout=600):
    p = subprocess.run(args, cwd=cwd, capture_output=True, text=True, timeout=timeout)
    return p.returncode, (p.stdout + p.stderr).strip()


def git(*args):
    return run(["git", *args], cwd=ARENA_DIR)[1]


# The stages ops/release.sh announces, in the order it announces them. The bar
# counts these rather than running a timer, so it reports where the deploy
# actually IS -- a progress bar that guesses is worse than none on a console
# someone watches during an incident.
PHASES = (
    ("FETCH", "Fetching"),
    ("CI GATE", "Checking GitHub CI"),
    ("BUILD", "Building"),
    ("MIGRATE", "Migrating the database"),
    ("START", "Starting"),
    ("LIVE", "Released"),
)
def released_sha(state_text):
    """The commit the running release was built from: the -<sha7> end of
    ARENA_VERSION in backend/.release.env (written by ops/release.sh), or ""."""
    ver = ""
    for line in (state_text or "").splitlines():
        if line.startswith("ARENA_VERSION="):
            ver = line.split("=", 1)[1].strip()
    sha = ver.rsplit("-", 1)[-1] if "-" in ver else ""
    return sha if len(sha) >= 7 and all(c in "0123456789abcdef" for c in sha) else ""


_ANSI = re.compile(r"\x1b\[[0-9;]*m")
# BuildKit's own progress: "#13 [app build 8/8] RUN ..." names the step and how
# many there are, and "#13  127.8   Compiling foo v1.2" counts seconds into it.
_STEP = re.compile(r"^#(\d+) \[[^\]]*?(\d+)/(\d+)\]")
_TICK = re.compile(r"^#(\d+) +(\d+\.\d+) ")
_CRATE = re.compile(r"\bCompiling [A-Za-z0-9_.+-]+ v")
# Enough of the tail to hold a whole build. A release compiles ~400 crates and
# BuildKit prints a line each, so the 160 lines this used to read were gone
# seconds into the build -- taking the stage marker with them, which made the
# bar fall back to indeterminate exactly when it was needed most.
LOG_TAIL_BYTES = 600_000


def read_log_tail(path, limit=LOG_TAIL_BYTES):
    """The last `limit` bytes of the log, starting at a line boundary."""
    try:
        with open(path, "rb") as f:
            f.seek(0, 2)
            size = f.tell()
            f.seek(max(0, size - limit))
            raw = f.read()
    except OSError:
        return ""
    text = raw.decode("utf-8", "replace")
    return text.split("\n", 1)[1] if size > limit and "\n" in text else text


def _build_sub(lines, prev_build):
    """Where the BUILD stage itself has got to.

    Two real signals, no guessing: BuildKit numbers its steps, and within the
    long one (cargo) it prints a line per crate. The crate total is taken from
    the PREVIOUS build in this same log -- the only honest denominator
    available, and labelled as an estimate because a dependency change moves
    it.
    """
    step = total = None
    secs = None
    cur_id = None
    crates = 0
    for ln in lines:
        m = _STEP.match(ln)
        if m:
            cur_id, step, total = m.group(1), int(m.group(2)), int(m.group(3))
            crates = 0            # a new step restarts the crate count
            continue
        t = _TICK.match(ln)
        if t:
            if cur_id is not None and t.group(1) != cur_id:
                continue
            secs = float(t.group(2))
        if _CRATE.search(ln):
            crates += 1

    if step is None:
        return None
    pct = round(100 * (step - 1) / total) if total else None
    note = "step %d of %d" % (step, total)
    # Inside the step that is actually slow, the crate count is the finer
    # signal, so it refines the fraction rather than replacing it.
    if crates:
        if prev_build:
            frac = min(1.0, crates / prev_build)
            pct = round(100 * ((step - 1) + frac) / total)
            note = "step %d of %d \u00b7 %d of ~%d crates" % (step, total, crates, prev_build)
        else:
            note = "step %d of %d \u00b7 %d crates compiled" % (step, total, crates)
    return {"label": "build", "pct": pct, "note": note, "secs": secs}


def _prev_build_crates(all_lines, run_start):
    """How many crates the last COMPLETED build compiled, as a denominator."""
    best, count, inside = 0, 0, False
    for ln in all_lines[:run_start]:
        body = ln.split("==> ", 1)[1] if "==> " in ln else ""
        if body.startswith("Building"):
            inside, count = True, 0
        elif inside and (body.startswith("Starting") or body.startswith("Released")):
            best, inside = max(best, count), False
        elif inside and _CRATE.search(ln):
            count += 1
    return best or None


# The CI gate's own lines (ops/release.sh). While GitHub is still running the
# checks, release.sh holds the release and re-checks every 20s rather than
# failing it, and says so: "holding for CI, up to 300s" once, then one
# "waiting on: a, b (passed: c)" per check. A finished gate prints one
# "<name> <conclusion>" line per check, and its die() names why it stopped.
_CI_SHA = re.compile(r"==> Checking GitHub CI for (\w+)")
_CI_HOLD = re.compile(r"holding for CI, up to (\d+)s")
_CI_WAITING = re.compile(r"waiting on: (.+?)(?: \(passed: (.+)\))?$")
_CI_RESULT = re.compile(r"^ {4}(\S+) +([a-z_]+)$")
_CI_STOPS = (("CI failed for this commit", "failed"), ("CI has not passed", "failed"),
             ("CI still running after", "timeout"), ("could not reach GitHub", "unreachable"))
CI_POLL_SECS = 20


def _ci(run):
    """Where the CI gate has got to, or None if this run has not reached it.

    state is waiting, passed, failed, timeout or unreachable; checks are in the
    order GitHub listed them, each pending, or its conclusion once completed.
    """
    at = next((i for i, ln in enumerate(run) if _CI_SHA.search(ln)), None)
    if at is None:
        return None
    end = next((i for i in range(at + 1, len(run)) if "==> " in run[i]), len(run))
    gate = run[at + 1:end]
    checks, budget, polls = {}, None, 0
    for ln in gate:
        m = _CI_HOLD.search(ln)
        if m:
            budget = int(m[1])
            continue
        m = _CI_WAITING.search(ln)
        if m:
            polls += 1
            for name in (m[2] or "").split(", "):
                if name:
                    checks[name] = "success"
            for name in m[1].split(", "):
                checks[name] = "pending"
            continue
        if "no CI runs for this commit yet" in ln:
            polls += 1
            continue
        m = _CI_RESULT.match(ln)
        if m:
            checks[m[1]] = "pending" if m[2] in ("in_progress", "queued") else m[2]
    state = next((st for text, st in _CI_STOPS if any(text in ln for ln in gate)), None)
    if state is None:
        state = "passed" if any("CI passed" in ln for ln in gate) else "waiting"
    return {
        "sha": _CI_SHA.search(run[at])[1], "state": state, "budget": budget,
        # A lower bound: the newest "waiting" line was printed this long into the hold.
        "waited": max(0, polls - 1) * CI_POLL_SECS,
        "checks": [{"name": n, "status": st} for n, st in checks.items()],
    }


def parse_progress(text):
    """How far the newest deploy has got, read out of the deploy log.

    `say()` in release.sh colours its output, so the escapes come off first.
    The log is appended to forever, so only the newest run counts -- it starts
    at autodeploy's "new commits" line or at release.sh's own first stage.

    A stage that is skipped simply never appears (a Python release runs no
    migration), so the bar steps over it instead of stalling on it.
    """
    lines = [_ANSI.sub("", ln).rstrip() for ln in text.splitlines()]
    start = 0
    for i, ln in enumerate(lines):
        if ln.lstrip().startswith("==> Fetching") or " new commits " in ln:
            start = i
    run = lines[start:]

    idx, phase = 0, None
    for ln in run:
        body = ln.split("==> ", 1)[1] if "==> " in ln else ""
        for n, (label, marker) in enumerate(PHASES, start=1):
            if body.startswith(marker) and n > idx:
                idx, phase = n, label

    state = "running"
    if any("ERROR" in ln or "\u2717" in ln or "release failed" in ln
           or "Rolling back" in ln for ln in run):
        state = "fail"
    elif phase == "LIVE":
        state, idx = "ok", len(PHASES)

    sub = None
    if phase == "BUILD" and state == "running":
        at = max((i for i, ln in enumerate(run)
                  if "==> " in ln and ln.split("==> ", 1)[1].startswith("Building")),
                 default=0)
        sub = _build_sub(run[at:], _prev_build_crates(lines, start))

    ci = _ci(run)
    reason = None
    if state == "fail":
        # Why it stopped decides what the page says. A release held at the CI
        # gate never touched the running Arena, so it must not read as a
        # rollback: "aborted" there sent people looking for damage that was
        # never done.
        if ci and ci["state"] in ("failed", "timeout", "unreachable"):
            reason = "ci_" + ci["state"]
        elif any("Rolling back" in ln for ln in run):
            reason = "rolled_back"
        else:
            reason = "failed"

    return {
        "phase": phase, "idx": idx, "total": len(PHASES),
        "ci": ci, "reason": reason,
        # Zero until a stage is seen: an unknown deploy reads as indeterminate,
        # never as "0% and climbing".
        "pct": round(100 * idx / len(PHASES)) if idx else 0,
        "state": state,
        "sub": sub,
        "line": next((ln for ln in reversed(run) if ln.strip()), ""),
    }


def status():
    run(["git", "fetch", "origin", BRANCH, "--quiet"], cwd=ARENA_DIR, timeout=60)
    # "Running" is what was RELEASED, not where the checkout is: release.sh moves the
    # checkout before its CI gate, so a release that stopped there left HEAD on the
    # new commit while the old image kept serving -- and this page said "deployed".
    try:
        with open(os.path.join(ARENA_DIR, "backend", ".release.env")) as f:
            base = released_sha(f.read())
    except OSError:
        base = ""
    if not base or not git("rev-parse", "--verify", "--quiet", base + "^{commit}"):
        base = "HEAD"
    local = git("rev-parse", base)[:7]
    remote = git("rev-parse", f"origin/{BRANCH}")[:7]
    behind = git("rev-list", "--count", f"{base}..origin/{BRANCH}") or "0"
    pending = git("log", "--oneline", f"{base}..origin/{BRANCH}") if behind != "0" else ""
    # Reached over the compose network by service name, not 127.0.0.1: the
    # panel runs in its own container, so loopback is the panel itself (:8090)
    # and never the app. The app is `expose`d, not published, so it has no
    # host port either -- a loopback probe cannot succeed in either layout.
    code, _ = run(["curl", "-fsS", "--max-time", "5", HEALTH_URL], timeout=15)
    try:
        with open(DEPLOY_LOG) as f:
            log = "".join(f.readlines()[-40:])
    except OSError:
        log = "(no deploy log yet)"
    return {
        "local": local, "remote": remote, "behind": int(behind or 0),
        "pending": pending, "healthy": code == 0,
        "subject": git("log", "-1", "--pretty=%s"),
        "when": git("log", "-1", "--pretty=%cr"),
        "log": log,
    }


PAGE = """<!doctype html><meta charset="utf-8"><title>ARENA // DEPLOY CONTROL</title>
<meta name="viewport" content="width=device-width,initial-scale=1,viewport-fit=cover">
<style>
 /* Tactical telemetry: a deploy console, not a product page. Dark substrate,
    monospace telemetry, one hazard red, 90-degree corners everywhere. System
    font stacks only -- this box has no network and no build step, so a webfont
    would simply fail to arrive. */
 :root{color-scheme:dark;
   --bg:#0a0a0a;--panel:#121212;--line:#2a2a2a;--ink:#eaeaea;--dim:#8a8a8a;
   --red:#e61919;--green:#4af626;
   --mono:ui-monospace,SFMono-Regular,"SF Mono",Menlo,Consolas,"Liberation Mono",monospace;
   --sans:system-ui,-apple-system,"Segoe UI",Roboto,Helvetica,Arial,sans-serif}
 *{box-sizing:border-box;border-radius:0}
 html{background:var(--bg)}
 body{margin:0;background:var(--bg);color:var(--ink);padding:0;
   font:13px/1.35 var(--mono);letter-spacing:.06em;text-transform:uppercase}
 /* Mechanical grain and a CRT sweep. Both inert overlays: pointer-events none,
    aria-hidden, and no layout cost. The noise is an inline data URI, not a
    fetch. */
 .grain,.scan{position:fixed;inset:0;pointer-events:none;z-index:9}
 .grain{opacity:.035;background-image:url("data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' width='120' height='120'%3E%3Cfilter id='n'%3E%3CfeTurbulence type='fractalNoise' baseFrequency='0.82' numOctaves='3' stitchTiles='stitch'/%3E%3C/filter%3E%3Crect width='120' height='120' filter='url(%23n)'/%3E%3C/svg%3E")}
 /* A sweep you notice only on a flat field. At .22 it greyed the telemetry
    it lies over, which is the one thing on the page that must stay legible. */
 .scan{opacity:.5;background:repeating-linear-gradient(0deg,transparent,transparent 3px,rgba(0,0,0,.12) 3px,rgba(0,0,0,.12) 4px)}
 @media (prefers-reduced-motion:reduce){.scan{display:none}}
 .wrap{max-width:60rem;margin:0 auto;padding:clamp(14px,3vw,34px);position:relative;z-index:1}

 /* Masthead: macro type against dense metadata. */
 .masthead{display:grid;grid-template-columns:1fr auto;gap:18px;align-items:end;
   border-bottom:2px solid var(--ink);padding-bottom:10px}
 h1{font:900 clamp(2.9rem,13vw,8rem)/.84 var(--sans);letter-spacing:-.045em;
   margin:0;text-transform:uppercase}
 /* Inherits 900 otherwise, which fills the glyph's counter in and reads as a
    missing-character box rather than a mark. */
 h1 .reg{font-weight:400;font-size:.15em;vertical-align:super;letter-spacing:0;
   margin-left:.12em;color:var(--dim)}
 .sub{margin:6px 0 0;color:var(--dim);font-size:10.5px;letter-spacing:.14em}
 .ident{margin:0;display:grid;grid-template-columns:auto auto;gap:2px 10px;
   font-size:10.5px;letter-spacing:.1em;text-align:right}
 .ident dt{color:var(--dim)}
 .ident dd{margin:0}

 /* Grid determinism: a 1px gap over a line-coloured parent draws the hairlines,
    so no child needs a border of its own. */
 /* Three explicit columns, not auto-fit: auto-fit sizes tracks to the
    container and would open a fourth, empty one at wide viewports -- a dead
    lit panel in a console that is meant to read as fully populated. */
 .bay{margin:22px 0 0;display:grid;grid-template-columns:repeat(3,minmax(0,1fr));
   gap:1px;background:var(--line);border:1px solid var(--line)}
 @media (max-width:46rem){.bay{grid-template-columns:1fr}}
 .cell{background:var(--panel);padding:13px 14px;min-height:4.6rem;
   display:flex;flex-direction:column;justify-content:space-between;gap:8px}
 .cell.wide{grid-column:1/-1}
 .cell dt{color:var(--dim);font-size:10px;letter-spacing:.16em}
 .cell dd{margin:0;font-size:15px;letter-spacing:.02em}
 .cell samp{font:inherit;font-size:15px}
 .subj{font-size:12px;letter-spacing:.04em;text-transform:none;line-height:1.4}
 .age{color:var(--dim);font-size:10.5px;text-transform:uppercase;letter-spacing:.12em}
 .dot{display:inline-block;width:7px;height:7px;margin-right:9px;vertical-align:.08em}
 /* Terminal green is spent here and nowhere else: one indicator, the one fact
    an operator opens this page to read. */
 .dot.up{background:var(--green)}
 .dot.down{background:var(--red)}
 .health.up{color:var(--green)}
 .health.down{color:var(--red)}
 .alert dd{color:var(--red)}
 .pending{margin:8px 0 0;max-height:7rem}

 /* Controls: ASCII framing from CSS so the brackets stay decorative and out of
    the accessible name. */
 .actions{display:flex;flex-wrap:wrap;gap:1px;background:var(--line);
   border:1px solid var(--line);border-top:0}
 .act{flex:1 1 11rem;font:inherit;font-size:12px;letter-spacing:.14em;
   padding:15px 12px;border:0;background:var(--panel);color:var(--ink);
   cursor:pointer;text-transform:uppercase}
 .act::before{content:"[ ";color:var(--dim)}
 .act::after{content:" ]";color:var(--dim)}
 .act:hover:not(:disabled){background:var(--ink);color:var(--bg)}
 .act:hover:not(:disabled)::before,.act:hover:not(:disabled)::after{color:var(--bg)}
 .act:focus-visible{outline:2px solid var(--red);outline-offset:-4px}
 .act.primary{color:var(--red)}
 .act.primary:hover:not(:disabled){background:var(--red);color:var(--bg)}
 .act:disabled{color:#555;cursor:not-allowed}
 .act:disabled::before,.act:disabled::after{color:#3a3a3a}

 /* Section rules that span the container, per the blueprint grid. */
 h2{display:flex;align-items:center;gap:12px;font-size:10px;letter-spacing:.2em;
   color:var(--dim);margin:26px 0 0;font-weight:400}
 h2 .rule{flex:1;height:1px;background:var(--line)}
 pre{background:var(--panel);border:1px solid var(--line);margin:8px 0 0;
   padding:13px;overflow:auto;max-height:22rem;font:11.5px/1.5 var(--mono);
   color:#b9c0c6;letter-spacing:.02em;text-transform:none;white-space:pre-wrap}
 .msg{display:none;margin-top:18px;padding:12px 14px;font-size:11.5px;
   letter-spacing:.12em;border-left:3px solid var(--line);background:var(--panel)}
 .msg.show{display:block}
 .msg.ok{border-left-color:var(--green);color:var(--green)}
 .msg.err{border-left-color:var(--red);color:var(--red)}
 .foot{display:flex;justify-content:space-between;gap:12px;flex-wrap:wrap;
   margin-top:26px;padding-top:10px;border-top:1px solid var(--line);
   color:var(--dim);font-size:10px;letter-spacing:.18em}

 /* ------------------------------------------------- motion -------------- */
 /* Mechanical, not organic: steps() everywhere a consumer UI would ease, so
    movement reads as a relay closing rather than a spring settling. Every
    animation here is decoration over a state that is ALSO carried by text, so
    reduced-motion can switch the lot off without losing information. */
 @keyframes march{to{background-position:32px 0}}
 @keyframes blink{50%{opacity:.25}}
 @keyframes headpulse{50%{box-shadow:0 0 18px 4px rgba(255,255,255,.95);width:3px}}
 @keyframes shake{25%{transform:translateX(-3px)}75%{transform:translateX(3px)}}
 @keyframes powerup{from{transform:scaleY(0);opacity:0}to{transform:scaleY(1);opacity:1}}
 @keyframes glitch{
   0%{text-shadow:none;transform:none}
   20%{text-shadow:-3px 0 var(--red),3px 0 #2ff3ff;transform:translateX(2px)}
   40%{text-shadow:3px 0 var(--red),-3px 0 #2ff3ff;transform:translateX(-2px)}
   60%{text-shadow:-2px 0 var(--red),2px 0 #2ff3ff;transform:translateX(1px)}
   100%{text-shadow:none;transform:none}}

 /* Buttons: a stepped wipe fills from the left, so the hover state lands in
    five visible frames instead of sliding. */
 .act{position:relative;overflow:hidden;isolation:isolate}
 .act>span{position:relative;z-index:1}
 .act::after{content:"";position:absolute;inset:0;z-index:0;background:var(--ink);
   transform:scaleX(0);transform-origin:left;transition:transform .14s steps(5)}
 .act:hover:not(:disabled)::after{transform:scaleX(1)}
 .act.primary::after{background:var(--red)}
 .act:hover:not(:disabled){background:var(--panel);color:var(--bg)}
 .act.primary:hover:not(:disabled){color:var(--bg)}
 .act:active:not(:disabled){transform:translateY(1px)}
 .act.primary:hover:not(:disabled)>span{animation:glitch .3s steps(3) 1}
 /* In flight: hazard stripes march across the face and a block caret blinks. */
 .act.armed{color:var(--red)}
 .act.armed::after{transform:scaleX(1);background:
   repeating-linear-gradient(135deg,rgba(230,25,25,.30) 0 8px,transparent 8px 16px);
   animation:march .5s linear infinite}
 .act.armed>span::after{content:" \u258c";animation:blink .6s steps(2) infinite}

 /* ------------------------------------------------- progress ------------- */
 /* Hidden until a run starts, and driven by ops/release.sh's own stages -- an
    unknown deploy shows stripes and no number rather than a plausible lie. */
 .rig{display:none;margin-top:1px;border:1px solid var(--line);background:var(--panel);
   padding:14px;transform-origin:top;animation:powerup .22s steps(4) 1}
 .rig.on{display:block}
 .chips{display:grid;grid-template-columns:repeat(6,1fr);gap:1px;background:var(--line);
   border:1px solid var(--line)}
 .chip{background:#111;padding:7px 3px;text-align:center;font-size:8.5px;
   letter-spacing:.1em;color:#4c4c4c;white-space:nowrap;overflow:hidden}
 .chip.on{color:var(--ink);background:#1c1c1c}
 .chip.cur{color:var(--red);animation:blink .8s steps(2) infinite}
 .track{position:relative;height:26px;margin-top:10px;background:#070707;
   border:1px solid var(--line);overflow:hidden}
 /* Three unmistakable states, because red has to keep meaning ALARM. A
    running deploy is machine-white with hazard stripes; only a failure is red,
    and only success is green. Running and failing looked near-identical when
    both were red, which is the wrong thing to be subtle about at 3am. */
 .fill{position:absolute;top:0;bottom:0;left:0;width:0;background:#cfcfcf;
   transition:width .4s cubic-bezier(.2,.9,.2,1)}
 .fill::after{content:"";position:absolute;inset:0;
   background:repeating-linear-gradient(135deg,rgba(0,0,0,.4) 0 8px,transparent 8px 16px);
   animation:march .5s linear infinite}
 /* The LED segmentation is an overlay, so the fill underneath stays one box. */
 .cells{position:absolute;inset:0;pointer-events:none;
   background:repeating-linear-gradient(90deg,transparent 0 calc(2.5% - 2px),
     var(--bg) calc(2.5% - 2px) 2.5%)}
 .head{position:absolute;top:0;bottom:0;left:0;width:2px;background:#fff;
   box-shadow:0 0 10px 2px rgba(255,255,255,.75);
   transition:left .4s cubic-bezier(.2,.9,.2,1);animation:headpulse .7s steps(2) infinite}
 /* Indeterminate: the whole track crawls and the readout says so. */
 .rig.wait .fill{width:100%;background:transparent}
 .rig.wait .head{display:none}
 .rig.ok .fill{background:var(--green)}
 .rig.ok .head{box-shadow:0 0 14px 3px var(--green);animation:none}
 .rig.fail .fill{background:var(--red)}
 .rig.ok .fill::after,.rig.fail .fill::after{animation:none}
 .rig.fail{animation:shake .18s steps(2) 2}
 .rig.fail .head{animation:none}
 /* The stage bar. Total progress is always up; this appears only once a stage
    has been running long enough that "BUILD, 50%" stops being an answer. */
 .sub{display:none;margin-top:9px;border-top:1px solid var(--line);padding-top:9px}
 .sub.on{display:block}
 .subhead{display:flex;justify-content:space-between;gap:10px;font-size:9.5px;
   letter-spacing:.16em;color:var(--dim)}
 .subsecs{font-variant-numeric:tabular-nums}
 .subtrack{position:relative;height:8px;margin-top:5px;background:#070707;
   border:1px solid var(--line);overflow:hidden}
 .subfill{position:absolute;top:0;bottom:0;left:0;width:0;background:#8a8a8a;
   transition:width .4s cubic-bezier(.2,.9,.2,1)}
 /* No fraction to show yet: crawl, rather than sit at zero looking stuck. */
 .sub.indet .subfill{width:35%;background:
   repeating-linear-gradient(135deg,#6a6a6a 0 6px,transparent 6px 12px);
   animation:crawl 1.1s linear infinite}
 @keyframes crawl{from{transform:translateX(-120%)}to{transform:translateX(330%)}}
 .subnote{margin-top:5px;font-size:10px;letter-spacing:.08em;color:var(--dim);
   text-transform:none}
 .rig.ok .sub,.rig.fail .sub{display:none}

 .read{display:flex;justify-content:space-between;align-items:baseline;gap:12px;margin-top:9px}
 .pct{font:900 clamp(1.6rem,6vw,2.8rem)/1 var(--sans);letter-spacing:-.04em;
   font-variant-numeric:tabular-nums}
 .rig.ok .pct{color:var(--green)}
 .rig.fail .pct{color:var(--red)}
 .nowline{color:var(--dim);font-size:10px;letter-spacing:.1em;text-align:right;
   overflow:hidden;text-overflow:ellipsis;white-space:nowrap;text-transform:none}

 /* ------------------------------------------- the ending ----------------- */
 /* A deploy finishing is the one moment this page exists for, so it gets a
    sequence rather than a colour change. Two of them, deliberately nothing
    alike: you must be able to tell them apart from across a room, with the
    screen in peripheral vision, before reading a word. */

 /* SUCCESS -- a CRT flyback sweep, then the stamp lands. */
 @keyframes sweep{from{transform:translateX(-100%)}to{transform:translateX(100%)}}
 @keyframes chipflash{0%,100%{background:#1c1c1c}40%{background:var(--green);color:#071a04}}
 @keyframes stampin{
   0%{transform:scale(1.6);opacity:0;letter-spacing:.5em}
   60%{transform:scale(1);opacity:1;letter-spacing:.02em}
   70%{transform:scale(1.04)}
   100%{transform:scale(1);opacity:1;letter-spacing:.02em}}
 @keyframes okglow{0%,100%{text-shadow:none}50%{text-shadow:0 0 22px var(--green)}}

 /* FAILURE -- a vignette alarm, not a full-screen strobe. Two pulses just
    over two per second: bright enough to pull your eye from the next desk,
    and deliberately short of the three-per-second flash threshold, which a
    full-bleed red strobe would have sailed past. */
 @keyframes alarmv{0%,100%{box-shadow:inset 0 0 0 0 rgba(230,25,25,0)}
   50%{box-shadow:inset 0 0 120px 24px rgba(230,25,25,.55)}}
 @keyframes stampbad{
   0%{transform:scale(1.5) skewX(-14deg);opacity:0}
   45%{transform:scale(1) skewX(0);opacity:1}
   55%{text-shadow:-4px 0 #2ff3ff,4px 0 var(--red);transform:translateX(3px)}
   70%{text-shadow:4px 0 #2ff3ff,-4px 0 var(--red);transform:translateX(-3px)}
   100%{text-shadow:none;transform:none;opacity:1}}
 @keyframes hardshake{
   10%{transform:translateX(-7px)}30%{transform:translateX(7px)}
   50%{transform:translateX(-5px)}70%{transform:translateX(4px)}
   90%{transform:translateX(-2px)}100%{transform:none}}

 /* The sweep rides over the whole console, once, and is gone. */
 .flash{position:fixed;inset:0;pointer-events:none;z-index:12;display:none;overflow:hidden}
 .flash.go{display:block}
 .flash.go::before{content:"";position:absolute;top:0;bottom:0;width:45%;
   background:linear-gradient(90deg,transparent,rgba(74,246,38,.30),rgba(255,255,255,.75),
     rgba(74,246,38,.30),transparent);
   animation:sweep .55s cubic-bezier(.4,0,.2,1) 1 both}
 .flash.bad{animation:alarmv .45s steps(2) 2}
 .flash.bad::before{display:none}

 .verdict{display:none;margin-top:10px;padding:10px 12px;text-align:center;
   font:900 clamp(1.3rem,5vw,2.2rem)/1 var(--sans);letter-spacing:.02em;
   border:2px solid currentColor}
 .rig.done-ok .verdict,.rig.done-fail .verdict{display:block}
 .rig.done-ok .verdict{color:var(--green);animation:stampin .5s steps(6) 1 both,
   okglow 1.1s ease-in-out 2}
 .rig.done-fail .verdict{color:var(--red);animation:stampbad .6s steps(7) 1 both}
 .rig.done-ok .chip{animation:chipflash .45s steps(3) 1 both;
   animation-delay:calc(var(--i,0) * 70ms)}
 .rig.done-fail{animation:hardshake .4s steps(5) 2}
 /* The failed run keeps its hazard stripes, stopped: a frozen warning, not a
    tidy end state. */
 .rig.done-fail .fill::after{animation:none;opacity:.85}


 /* ------------------------------------------- the CI hold --------------- */
 /* release.sh holds the release at the CI gate until GitHub finishes, rather
    than failing it because CI was a minute behind the merge. Holding is
    neither running nor failing, so it gets the one colour nothing else on the
    page uses: amber, for "waiting on someone else". It is the longest quiet
    stretch of a deploy, so it is also the one built to be watched. */
 :root{--amber:#ffb000}
 @keyframes spin{to{transform:rotate(360deg)}}
 @keyframes ping{0%{transform:scale(.7);opacity:.9}100%{transform:scale(3.4);opacity:0}}
 @keyframes comet{from{left:-35%}to{left:100%}}
 @keyframes ticker{to{transform:translateX(-50%)}}
 @keyframes rgbsplit{0%,84%,100%{text-shadow:none;transform:none}
   86%{text-shadow:-4px 0 var(--red),4px 0 #2ff3ff;transform:translateX(3px) skewX(-8deg)}
   90%{text-shadow:3px 0 var(--red),-3px 0 #2ff3ff;transform:translateX(-2px)}
   94%{text-shadow:-1px 0 var(--red),1px 0 #2ff3ff;transform:none}}
 @keyframes lockon{0%{transform:scale(2.6);opacity:0}60%{transform:scale(.85);opacity:1}100%{transform:scale(1);opacity:1}}
 @keyframes passwipe{from{transform:scaleX(0)}to{transform:scaleX(1)}}
 @keyframes breathe{50%{box-shadow:inset 0 0 0 1px rgba(255,176,0,.9),0 0 34px -6px rgba(255,176,0,.6)}}
 @keyframes clearedin{0%{transform:scale(2);opacity:0;letter-spacing:.4em}
   55%{transform:scale(.97);opacity:1;letter-spacing:-.02em}100%{transform:scale(1);opacity:1}}
 @keyframes clearedout{to{transform:scaleY(0);opacity:0}}
 @keyframes dots{0%{content:""}25%{content:"."}50%{content:".."}75%{content:"..."}}

 .hold{display:none;position:relative;margin-top:10px;padding:16px;gap:18px;
   grid-template-columns:auto minmax(0,1fr);align-items:center;overflow:hidden;
   border:1px solid rgba(255,176,0,.5);transform-origin:top;
   background:radial-gradient(110% 150% at 0 50%,rgba(255,176,0,.09),transparent 62%),#0d0b06;
   animation:powerup .22s steps(4) 1,breathe 2.4s ease-in-out infinite}
 .hold.on{display:grid}
 /* The radar: range rings and a crosshair drawn by gradients, a beam that
    never stops, one blip per check. The ring around it is the time budget. */
 .radar{position:relative;width:138px;height:138px;margin:4px;border-radius:50%;
   background:
    radial-gradient(circle,transparent 0 32%,rgba(255,176,0,.25) 32% calc(32% + 1px),
      transparent calc(32% + 1px) 64%,rgba(255,176,0,.25) 64% calc(64% + 1px),transparent calc(64% + 1px)),
    linear-gradient(transparent calc(50% - .5px),rgba(255,176,0,.2) 0 calc(50% + .5px),transparent 0),
    linear-gradient(90deg,transparent calc(50% - .5px),rgba(255,176,0,.2) 0 calc(50% + .5px),transparent 0),
    #070604}
 .beam{position:absolute;inset:0;border-radius:50%;
   background:conic-gradient(from 0deg,transparent 0 280deg,rgba(255,176,0,.08) 300deg,rgba(255,176,0,.65) 360deg);
   animation:spin 2.2s linear infinite}
 .ring{position:absolute;inset:-7px;width:calc(100% + 14px);height:calc(100% + 14px);
   transform:rotate(-90deg);overflow:visible}
 .ring circle{fill:none;stroke-width:3}
 .ring .rtrack{stroke:rgba(255,176,0,.14);stroke-dasharray:1 2}
 .ring .rfill{stroke:var(--amber);stroke-dasharray:100;stroke-dashoffset:100;
   filter:drop-shadow(0 0 4px rgba(255,176,0,.8));transition:stroke-dashoffset .25s linear}
 .clock{position:absolute;inset:0;display:grid;place-content:center;text-align:center;z-index:1}
 .clock b{font:900 25px/1 var(--sans);letter-spacing:-.02em;color:#fff;
   font-variant-numeric:tabular-nums;text-shadow:0 0 12px rgba(255,176,0,.7)}
 .clock small{margin-top:3px;font-size:8.5px;letter-spacing:.18em;color:rgba(255,176,0,.75)}
 .blip{position:absolute;width:8px;height:8px;margin:-4px 0 0 -4px;background:var(--amber);
   box-shadow:0 0 8px var(--amber);z-index:2;transition:left .5s,top .5s}
 .blip::after{content:"";position:absolute;inset:0;border:1px solid var(--amber);
   animation:ping 1.6s ease-out infinite;animation-delay:inherit}
 .blip.pass{background:var(--green);box-shadow:0 0 10px var(--green);animation:lockon .45s steps(5) 1 both}
 .blip.pass::after{inset:-5px;border-color:var(--green);animation:none}
 .blip.bad{background:var(--red);box-shadow:0 0 12px var(--red)}
 .blip.bad::after{border-color:var(--red)}

 .holdtitle{font:900 clamp(1.5rem,5.4vw,2.6rem)/.95 var(--sans);letter-spacing:-.03em;
   color:var(--amber);animation:rgbsplit 3.1s steps(1) infinite}
 .holdtitle::after{content:"";display:inline-block;width:1.2em;text-align:left;
   animation:dots 1.4s steps(1) infinite}
 .holdsub{margin-top:6px;font-size:10.5px;letter-spacing:.1em;color:#c9b27a;text-transform:none}
 .holdsub b{color:#fff;font-weight:400;letter-spacing:.06em}
 .lanes{list-style:none;margin:12px 0 0;padding:0;display:grid;gap:6px}
 .lane{display:grid;grid-template-columns:minmax(4.5rem,8rem) minmax(0,1fr) 4.8rem;gap:10px;
   align-items:center;font-size:10.5px;letter-spacing:.14em}
 .lane .ln{overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
 .lane .lt{position:relative;height:11px;background:#070707;border:1px solid #2b2618;overflow:hidden}
 .lane .lt::before{content:"";position:absolute;top:0;bottom:0;width:35%;
   background:linear-gradient(90deg,transparent,rgba(255,176,0,.6) 70%,#fff);
   box-shadow:0 0 14px var(--amber);animation:comet 1.25s cubic-bezier(.55,0,.45,1) infinite;
   animation-delay:var(--d,0s)}
 .lane .lt::after{content:"";position:absolute;inset:0;pointer-events:none;
   background:repeating-linear-gradient(90deg,transparent 0 6px,#070707 6px 8px)}
 .lane .ls{text-align:right;color:var(--amber);animation:blink .8s steps(2) infinite}
 .lane.pass .lt::before{left:0;width:100%;background:var(--green);box-shadow:0 0 12px var(--green);
   transform-origin:left;animation:passwipe .35s steps(6) 1 both}
 .lane.pass .ls{color:var(--green);animation:lockon .4s steps(5) 1 both}
 .lane.bad .lt::before{left:0;width:100%;background:var(--red);box-shadow:0 0 12px var(--red);animation:none}
 .lane.bad .ls{color:var(--red);animation:none}
 .noruns{font-size:10.5px;letter-spacing:.12em;color:#c9b27a}
 .ticker{margin-top:12px;padding-top:8px;border-top:1px dashed rgba(255,176,0,.3);
   overflow:hidden;white-space:nowrap;font-size:9.5px;letter-spacing:.16em;color:rgba(255,176,0,.6)}
 .ticker span{display:inline-block;padding-right:0;animation:ticker 22s linear infinite}

 /* While the release holds, the main track is a holding pattern, not progress. */
 .rig.holding .fill{background:#2a2108}
 .rig.holding .fill::after{background:repeating-linear-gradient(135deg,rgba(255,176,0,.55) 0 8px,transparent 8px 16px)}
 .rig.holding .head{background:var(--amber);box-shadow:0 0 12px 3px var(--amber)}
 .rig.holding .chip.cur{color:var(--amber)}
 .rig.holding .pct{color:var(--amber)}

 /* CI went green: the radar turns, the lanes are already full, the stamp
    lands, and the whole module folds away so the build can have the stage. */
 .hold.cleared{border-color:var(--green);animation:none;
   box-shadow:inset 0 0 0 1px var(--green),0 0 40px -6px rgba(74,246,38,.6)}
 .hold.cleared .beam{background:conic-gradient(from 0deg,transparent 0 260deg,rgba(74,246,38,.7) 360deg);
   animation-duration:.5s}
 .hold.cleared .ring .rfill{stroke:var(--green);stroke-dashoffset:0!important}
 .hold.cleared .holdtitle{color:var(--green);animation:clearedin .5s steps(6) 1 both}
 .hold.cleared .holdtitle::after,.hold.red .holdtitle::after{content:"";animation:none}
 .hold.cleared::after{content:"";position:absolute;top:0;bottom:0;width:35%;
   background:linear-gradient(90deg,transparent,rgba(74,246,38,.35),rgba(255,255,255,.5),transparent);
   animation:comet .6s cubic-bezier(.4,0,.2,1) 1 both}
 .hold.gone{animation:clearedout .3s steps(4) 1 both}
 /* CI went red: the hold stays up, frozen, with the failing lane showing. */
 .hold.red{border-color:var(--red);animation:none;box-shadow:inset 0 0 0 1px var(--red)}
 .hold.red .beam,.hold.red .ring .rfill{animation:none;opacity:.25}
 .hold.red .holdtitle{color:var(--red);animation:stampbad .6s steps(7) 1 both}
 .hold.red .ticker span{animation:none}
 /* Held, not failed: the wait ran out or GitHub did not answer. Nothing was
    touched, so no alarm -- an amber stamp, and the bar stops where it was. */
 .rig.done-held{animation:none}
 .rig.done-held .fill{background:var(--amber)}
 .rig.done-held .fill::after{animation:none;opacity:.6}
 .rig.done-held .pct{color:var(--amber)}
 .rig.done-held .verdict{display:block;color:var(--amber);animation:stampin .5s steps(6) 1 both}
 /* The ticker is one long nowrap line: without min-width:0 its column grows
    to fit it and drags the whole console past a phone's edge. */
 .hold>div{min-width:0}
 @media (max-width:34rem){
   .hold{grid-template-columns:minmax(0,1fr);justify-items:center}
   .hold>div:last-child{width:100%}
   .lane{grid-template-columns:minmax(4rem,6rem) minmax(0,1fr) 4.2rem;gap:8px}
 }

 /* Everything above is ornament on a state the text already gives, so this
    turns all of it off and keeps the bar's width, which is the information. */
 @media (prefers-reduced-motion:reduce){
   .act::after,.act.armed::after,.act.primary:hover>span,.rig,.fill::after,
   .head,.chip.cur,.flash,.flash.go::before,.rig.done-ok .verdict,
   .rig.done-fail .verdict,.rig.done-ok .chip,.rig.done-fail,
   .sub.indet .subfill,.hold,.beam,.blip,.blip::after,.lane .lt::before,.lane .ls,
   .holdtitle,.holdtitle::after,.ticker span,.hold.cleared::after,
   .rig.done-held .verdict{animation:none!important}
   /* A comet that cannot move would sit off the end of its lane. */
   .lane:not(.pass):not(.bad) .lt::before{left:0;width:100%;opacity:.35}
   /* Still has to read as "no number yet" rather than as 35% done. */
   .sub.indet .subfill{width:100%;opacity:.35}
   /* The verdict still SHOWS -- it is the result, not the celebration. */
   .flash.go,.flash.bad{display:none}
   .act::after{transition:none}
   .fill,.head{transition:none}
 }

 /* A grid track's default min-width is auto, so a long commit subject or a
    wide log line pushes the whole console past the viewport instead of
    wrapping. This page gets opened on a phone mid-incident, and a horizontal
    scrollbar over telemetry is the worst time to meet one. */
 .cell,.masthead>*{min-width:0}
 .subj,samp,pre{overflow-wrap:anywhere}
 @media (max-width:34rem){
   .masthead{grid-template-columns:1fr;align-items:start;gap:10px}
   .ident{text-align:left;grid-template-columns:auto 1fr;gap:2px 14px}
   .act{flex:1 1 100%}
 }
</style>
<div class="flash" id="flash" aria-hidden="true"></div>
<div class="grain" aria-hidden="true"></div>
<div class="scan" aria-hidden="true"></div>
<main class="wrap">
 <header class="masthead">
  <div>
   <h1>Deploy<span class="reg">&reg;</span></h1>
   <p class="sub">/// arena control surface &middot; unit __DOMAIN__</p>
  </div>
  <dl class="ident">
   <dt>operator</dt><dd>__WHO__</dd>
   <dt>branch</dt><dd>__BRANCH__</dd>
  </dl>
 </header>

 <output class="msg" id="msg"></output>

 <dl class="bay">
  <div class="cell">
   <dt>service</dt>
   <dd><i class="dot __DOTCLS__"></i><span class="health __DOTCLS__">__HEALTH__</span></dd>
  </div>
  <div class="cell"><dt>running</dt><dd><samp>__LOCAL__</samp></dd></div>
  <div class="cell"><dt>head &middot; __BRANCH__</dt><dd><samp>__REMOTE__</samp></dd></div>
  <div class="cell wide"><dt>last commit</dt>
   <dd class="subj">__SUBJECT__ <span class="age">__WHEN__</span></dd></div>
  __BEHIND__
 </dl>

 <nav class="actions">
  <button class="act primary" id="deploy" __DISABLED__><span>deploy latest</span></button>
  <button class="act" id="rollback"><span>roll back one</span></button>
  <button class="act" id="refresh"><span>refresh</span></button>
 </nav>

 <section class="rig" id="rig" aria-live="polite">
  <div class="chips" id="chips"></div>
  <div class="track"><div class="fill" id="fill"></div><div class="cells"></div>
   <div class="head" id="head"></div></div>
  <div class="sub" id="sub">
   <div class="subhead"><span id="subname">stage</span>
    <span class="subsecs" id="subsecs"></span></div>
   <div class="subtrack"><div class="subfill" id="subfill"></div></div>
   <div class="subnote" id="subnote"></div>
  </div>
  <div class="hold" id="hold">
   <div class="radar" aria-hidden="true"><i class="beam"></i>
    <svg class="ring" viewBox="0 0 100 100"><circle class="rtrack" cx="50" cy="50" r="48"/>
     <circle class="rfill" id="rfill" cx="50" cy="50" r="48" pathLength="100"/></svg>
    <div id="blips"></div>
    <div class="clock"><b id="clock">0:00</b><small id="clockof">of 5:00</small></div>
   </div>
   <div>
    <div class="holdtitle" id="holdtitle">holding for ci</div>
    <div class="holdsub" id="holdsub"></div>
    <ul class="lanes" id="lanes"></ul>
    <div class="ticker" aria-hidden="true"><span id="ticker"></span></div>
   </div>
  </div>
  <div class="read"><output class="pct" id="pct">--</output>
   <span class="nowline" id="nowline">standing by</span></div>
  <div class="verdict" id="verdict"></div>
 </section>

 <h2>deploy log <span class="rule"></span> tail 40</h2>
 <pre id="log">__LOG__</pre>

 <footer class="foot"><span>+ arena deploy</span><span>rev 2.1</span><span>__DOMAIN__</span></footer>
</main>
<script>
var CSRF="__CSRF__";
var PHASES=["fetch","ci gate","build","migrate","start","live"];
var POLL=null;
function $id(i){return document.getElementById(i);}
function msg(t,cls){var m=$id("msg");m.textContent=t;m.className="msg show "+cls;}

(function(){
  var c=$id("chips");
  for(var i=0;i<PHASES.length;i++){
    var d=document.createElement("div");d.className="chip";d.textContent=PHASES[i];
    d.style.setProperty("--i",i);   // staggers the success cascade
    c.appendChild(d);
  }
})();

// The ending fires ONCE. paint() runs on every poll and again on the final
// settle, so without this the sweep would restart and the stamp would re-land
// each time -- which reads as a second deploy starting.
var ENDED=false;
// Only a rollback is "aborted": a release held at the CI gate never touched
// the running Arena, and saying otherwise sends people looking for damage.
var VERDICT={ci_failed:"ci red \u00b7 nothing shipped",
  ci_timeout:"ci still running \u00b7 nothing shipped",
  ci_unreachable:"github unreachable \u00b7 nothing shipped",
  rolled_back:"aborted \u00b7 rolled back", failed:"failed"};
function finish(ok,reason){
  if(ENDED) return; ENDED=true;
  var rig=$id("rig"), fl=$id("flash");
  // Held, not failed: nothing went wrong on this box, so no alarm.
  var held=reason==="ci_timeout"||reason==="ci_unreachable";
  rig.classList.add(ok?"done-ok":held?"done-held":"done-fail");
  $id("verdict").textContent = ok ? "live" : (VERDICT[reason]||VERDICT.failed);
  fl.className = "flash " + (ok?"go":held?"":"bad");
  // Leave the overlay up only as long as its animation: a fixed, full-bleed
  // element outlives its purpose fast.
  setTimeout(function(){fl.className="flash";}, ok?700:1000);
}

// Paint whatever the server last reported. Never invents a number: with no
// stage seen yet the rig goes indeterminate and the readout says WAIT, which
// is the honest state while release.sh is still starting up.
// When the current stage started, so a slow one can say how long it has been.
// The log gives real seconds for the build (BuildKit prints them); everything
// else is timed here, which is why a reload mid-deploy restarts the clock.
var STAGE_AT=Date.now(), STAGE_IDX=-1;
var SLOW_AFTER=10;   // seconds before the stage bar is worth the space

function paintSub(j){
  var box=$id("sub");
  if(j.idx!==STAGE_IDX){ STAGE_IDX=j.idx; STAGE_AT=Date.now(); }
  var secs=(j.sub&&j.sub.secs!=null)?j.sub.secs:(Date.now()-STAGE_AT)/1000;
  if(j.state!=="running"||!j.idx||secs<SLOW_AFTER||holding(j)){ box.className="sub"; return; }
  var pct=(j.sub&&j.sub.pct!=null)?j.sub.pct:null;
  box.className="sub on"+(pct==null?" indet":"");
  $id("subname").textContent=(j.phase||"stage")+" \u00b7 in progress";
  $id("subsecs").textContent=Math.round(secs)+"s";
  $id("subfill").style.width=(pct==null?35:pct)+"%";
  $id("subnote").textContent=(j.sub&&j.sub.note)?j.sub.note:"no step detail from this stage";
}

// ---- the CI hold ---------------------------------------------------------
// The hold's clock runs here, between polls, so it ticks every second rather
// than every poll. It starts from the server's lower bound, so a reload mid-
// hold picks up roughly where it was instead of back at zero.
var HOLD={on:false,at:0,budget:300,timer:null}, LANES={};
function holding(j){ return j.state==="running"&&j.phase==="CI GATE"&&j.ci&&j.ci.state==="waiting"; }
function mmss(s){ s=Math.max(0,Math.floor(s)); return Math.floor(s/60)+":"+("0"+s%60).slice(-2); }
function tickHold(){
  var el=(Date.now()-HOLD.at)/1000, b=HOLD.budget;
  $id("clock").textContent=mmss(el);
  $id("clockof").textContent=el<b?"of "+mmss(b):"overtime";
  $id("rfill").style.strokeDashoffset=Math.max(0,100-100*el/b);
}
function lanes(checks){
  var ul=$id("lanes"), radar=$id("blips");
  checks.forEach(function(c,i){
    var L=LANES[c.name];
    if(!L){
      var li=document.createElement("li");
      li.innerHTML='<span class="ln"></span><span class="lt"></span><span class="ls"></span>';
      li.querySelector(".ln").textContent=c.name;
      li.style.setProperty("--d",(i*0.31)+"s");
      var b=document.createElement("i"); b.style.animationDelay=(i*0.4)+"s";
      ul.appendChild(li); radar.appendChild(b);
      L=LANES[c.name]={li:li,blip:b,st:null};
    }
    var st=c.status==="pending"?"run":/^(success|skipped|neutral)$/.test(c.status)?"pass":"bad";
    if(L.st===st) return;   // re-setting the class would replay the lock-on
    L.st=st;
    L.li.className="lane "+st; L.blip.className="blip "+st;
    L.li.querySelector(".ls").textContent=st==="run"?"running":st==="pass"?"pass":c.status.replace("_"," ");
  });
  // Spread the blips round the scope, alternating range rings.
  var names=Object.keys(LANES);
  names.forEach(function(n,i){
    var a=i/names.length*2*Math.PI-Math.PI/2+0.7, r=i%2?24:38;
    LANES[n].blip.style.left=(50+r*Math.cos(a))+"%";
    LANES[n].blip.style.top=(50+r*Math.sin(a))+"%";
  });
}
function paintHold(j){
  var h=$id("hold"), ci=j.ci;
  if(holding(j)){
    if(!HOLD.on){
      HOLD.on=true; HOLD.at=Date.now()-(ci.waited||0)*1000;
      clearInterval(HOLD.timer); HOLD.timer=setInterval(tickHold,250);
      var t="polling github check-runs for "+ci.sha+" every 20s · the release goes the moment every check is green · a red check stops it here, before anything is touched · nothing ships until ci says so · ";
      $id("ticker").textContent=t+t;
    }
    if(ci.budget) HOLD.budget=ci.budget;
    h.className="hold on";
    $id("holdtitle").textContent="holding for ci";
    var waiting=ci.checks.filter(function(c){return c.status==="pending";}).length;
    $id("holdsub").innerHTML="";
    $id("holdsub").append(ci.checks.length
      ? (waiting+" of "+ci.checks.length+" checks still running on ")
      : "github hasn’t queued the checks for ", (function(){var b=document.createElement("b");b.textContent=ci.sha;return b;})(),
      ci.checks.length?" · it ships the moment they’re green":" yet · standing by");
    lanes(ci.checks); tickHold();
    return;
  }
  if(!HOLD.on) return;
  HOLD.on=false; clearInterval(HOLD.timer);
  if(ci&&ci.state==="passed"){
    // Cleared: every lane locks green, the stamp lands, then it folds away.
    lanes(ci.checks);
    $id("holdtitle").textContent="ci green · launching";
    $id("holdsub").textContent="every check passed · handing over to the build";
    h.className="hold on cleared";
    setTimeout(function(){h.className="hold on cleared gone";},1700);
    setTimeout(function(){h.className="hold";},2000);
  } else if(ci&&ci.state==="failed"){
    lanes(ci.checks);
    $id("holdtitle").textContent="ci red";
    $id("holdsub").textContent="a check failed on github · held at the gate, nothing was touched";
    h.className="hold on red";
  } else {
    h.className="hold";
  }
}

function paint(j){
  var rig=$id("rig");
  // Keep whichever ending has already landed: className is rebuilt here.
  var ended=(rig.className.match(/done-\\w+/)||[""])[0];
  rig.className="rig on"+(j.state==="ok"?" ok":j.state==="fail"?" fail":"")
    +(j.idx?"":" wait")+(holding(j)?" holding":"")+(ended?" "+ended:"");
  for(var i=0;i<PHASES.length;i++){
    var ch=rig.querySelectorAll(".chip")[i];
    ch.className="chip"+(i<j.idx?" on":"")+(i===j.idx-1&&j.state==="running"?" cur":"");
  }
  $id("fill").style.width=(j.idx?j.pct:100)+"%";
  $id("head").style.left=j.pct+"%";
  $id("pct").textContent=j.idx?j.pct+"%":"--";
  $id("nowline").textContent=j.line||"waiting for the release to start";
  if(holding(j)) $id("pct").textContent="hold";
  paintSub(j);
  paintHold(j);
}
function poll(){
  fetch("/api/progress",{headers:{"X-Panel-Token":CSRF}})
   .then(function(r){return r.json();}).then(paint).catch(function(){});
}
function act(path){
  var bs=document.querySelectorAll("button");
  bs.forEach(function(b){b.disabled=true;b.classList.add("armed");});
  $id("rig").className="rig on wait";
  $id("pct").textContent="--";
  $id("nowline").textContent="opening the release";
  msg("Working\u2026","ok");
  STAGE_IDX=-1; STAGE_AT=Date.now();
  poll(); POLL=setInterval(poll,900);
  fetch(path,{method:"POST",headers:{"X-Panel-Token":CSRF}})
   .then(function(r){return r.json();})
   .then(function(j){
     clearInterval(POLL);
     // One last read, so the bar settles on the real final stage rather than
     // wherever the poll happened to leave it.
     fetch("/api/progress",{headers:{"X-Panel-Token":CSRF}})
      .then(function(r){return r.json();})
      .then(function(pr){ pr.state=j.ok?"ok":"fail"; if(j.ok){pr.idx=pr.total;pr.pct=100;} paint(pr); })
      .catch(function(){})
      // The POST's own answer decides, not the poll: a last poll can still read
      // "running" off a log the release has not finished flushing.
      .then(function(){ finish(j.ok, j.reason); });
     msg(j.ok?(j.message||"Done"):(j.error||"Failed"), j.ok?"ok":"err");
     bs.forEach(function(b){b.classList.remove("armed");});
     // Long enough for the ending to play out. A failure holds longer: there
     // is something to read, and the reload wipes it.
     setTimeout(function(){location.reload();}, j.ok?3400:6000);
   })
   .catch(function(){
     clearInterval(POLL);
     msg("Request failed","err");
     $id("rig").className="rig on fail";
     finish(false,"request failed");
     bs.forEach(function(b){b.disabled=false;b.classList.remove("armed");});
   });
}
$id("deploy").onclick=function(){act("/api/deploy");};
$id("rollback").onclick=function(){
  if(confirm("Roll back to the previous release?"))act("/api/rollback");};
$id("refresh").onclick=function(){location.reload();};
</script>
"""


# What the deploy button reports when a release did not ship, by parse_progress's
# reason. Only one of them touched the running Arena.
FAILURES = {
    "ci_failed": "CI failed on GitHub. Nothing was deployed.",
    "ci_timeout": "CI was still running when the wait ran out. Nothing was deployed: deploy again once it is green.",
    "ci_unreachable": "Could not reach GitHub to check CI. Nothing was deployed.",
    "rolled_back": "Deploy failed and was rolled back to the previous release.",
    "failed": "Deploy failed.",
}


def render(login, csrf):
    s = status()
    behind = ""
    if s["behind"]:
        # Inside the <dl>, so it has to be a dt/dd group: a bare <pre> there
        # would not be valid markup.
        behind = ('<div class="cell wide alert"><dt>pending</dt>'
                  f'<dd>&uarr; {s["behind"]} commit(s) not deployed'
                  f'<pre class="pending">{html.escape(s["pending"])}</pre></dd></div>')
    return (PAGE
            .replace("__WHO__", html.escape(login))
            .replace("__DOMAIN__", html.escape(ARENA_DOMAIN or "arena"))
            .replace("__BRANCH__", html.escape(BRANCH))
            .replace("__HEALTH__", "healthy" if s["healthy"] else "not responding")
            .replace("__DOTCLS__", "up" if s["healthy"] else "down")
            .replace("__LOCAL__", html.escape(s["local"]))
            .replace("__REMOTE__", html.escape(s["remote"]))
            .replace("__SUBJECT__", html.escape(s["subject"][:70]))
            .replace("__WHEN__", html.escape(s["when"]))
            .replace("__BEHIND__", behind)
            .replace("__DISABLED__", "" if s["behind"] else "disabled")
            .replace("__LOG__", html.escape(s["log"]))
            .replace("__CSRF__", csrf))


class Handler(BaseHTTPRequestHandler):
    def _send(self, code, body, ctype="text/html; charset=utf-8", cookie=None):
        data = body.encode() if isinstance(body, str) else body
        self.send_response(code)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(data)))
        self.send_header("X-Frame-Options", "DENY")
        self.send_header("Referrer-Policy", "no-referrer")
        if cookie:
            self.send_header("Set-Cookie", cookie)
        self.end_headers()
        self.wfile.write(data)

    def _json(self, code, obj):
        self._send(code, json.dumps(obj), "application/json")

    def _login(self):
        raw = self.headers.get("Cookie", "")
        if not raw:
            return None
        jar = http.cookies.SimpleCookie()
        jar.load(raw)
        c = jar.get(COOKIE)
        return _verify(c.value) if c else None

    def _csrf(self, login):
        return _sign(f"csrf:{login}|{time.time() + SESSION_TTL}")

    # --- GET ---------------------------------------------------------------
    def do_GET(self):
        path = self.path.split("?", 1)[0]

        if path == "/auth/login":
            if not CLIENT_ID:
                return self._send(503, "<p>PANEL_GITHUB_CLIENT_ID is not set on the server.</p>")
            state = _sign(f"state:{secrets.token_hex(8)}|{time.time() + 600}")
            q = urllib.parse.urlencode({
                "client_id": CLIENT_ID,
                "redirect_uri": f"{PANEL_BASE_URL}/auth/callback",
                "scope": "read:user",
                "state": state,
            })
            self.send_response(302)
            self.send_header("Location", "https://github.com/login/oauth/authorize?" + q)
            self.end_headers()
            return

        if path == "/auth/callback":
            qs = urllib.parse.parse_qs(self.path.split("?", 1)[1] if "?" in self.path else "")
            code = (qs.get("code") or [""])[0]
            state = (qs.get("state") or [""])[0]
            if not code or not state or not state.startswith("state:"):
                return self._send(400, "<p>Bad callback.</p>")
            try:
                payload, mac = state.rsplit(".", 1)
                if not hmac.compare_digest(
                    mac, hmac.new(SECRET_KEY.encode(), payload.encode(), hashlib.sha256).hexdigest()
                ) or float(payload.split("|", 1)[1]) < time.time():
                    return self._send(400, "<p>Expired or invalid state.</p>")
            except Exception:
                return self._send(400, "<p>Invalid state.</p>")

            login = self._exchange(code)
            if login is None:
                return self._send(502, "<p>GitHub sign-in failed.</p>")
            if login.lower() not in ALLOWED:
                return self._send(403, f"<p><b>{html.escape(login)}</b> is not on the allowlist.</p>")

            token = _sign(f"{login}|{time.time() + SESSION_TTL}")
            c = f"{COOKIE}={token}; HttpOnly; Secure; SameSite=Lax; Path=/; Max-Age={SESSION_TTL}"
            self.send_response(302)
            self.send_header("Location", "/")
            self.send_header("Set-Cookie", c)
            self.end_headers()
            return

        if path == "/auth/logout":
            self.send_response(302)
            self.send_header("Location", "/")
            self.send_header("Set-Cookie", f"{COOKIE}=; Path=/; Max-Age=0")
            self.end_headers()
            return

        login = self._login()
        if not login:
            return self._send(200,
                '<!doctype html><meta charset="utf-8"><title>Arena Deploy</title>'
                '<body style="background:#0e1117;color:#e6edf3;font-family:system-ui;'
                'display:grid;place-items:center;height:100vh;margin:0">'
                '<div style="text-align:center"><h1>Arena Deploy</h1>'
                '<p style="color:#8b949e">Restricted to the maintainers.</p>'
                '<a href="/auth/login" style="display:inline-block;margin-top:10px;padding:10px 18px;'
                'background:#58a6ff;color:#0b1117;border-radius:7px;text-decoration:none;'
                'font-weight:600">Sign in with GitHub</a></div>')

        if path == "/":
            return self._send(200, render(login, self._csrf(login)))
        if path == "/api/progress":
            # Polled while a deploy runs. Safe to read on a GET: it reports the
            # same log the page already shows, and changes nothing. The server
            # is a ThreadingHTTPServer, so this answers while /api/deploy is
            # still blocking in another thread.
            return self._json(200, parse_progress(read_log_tail(DEPLOY_LOG)))
        self._send(404, "<p>Not found.</p>")

    def _exchange(self, code):
        try:
            data = urllib.parse.urlencode({
                "client_id": CLIENT_ID, "client_secret": CLIENT_SECRET,
                "code": code, "redirect_uri": f"{PANEL_BASE_URL}/auth/callback",
            }).encode()
            req = urllib.request.Request("https://github.com/login/oauth/access_token", data=data)
            req.add_header("Accept", "application/json")
            with urllib.request.urlopen(req, timeout=20) as r:
                tok = json.loads(r.read().decode()).get("access_token")
            if not tok:
                return None
            req = urllib.request.Request("https://api.github.com/user")
            req.add_header("Authorization", f"Bearer {tok}")
            req.add_header("Accept", "application/vnd.github+json")
            with urllib.request.urlopen(req, timeout=20) as r:
                return json.loads(r.read().decode()).get("login")
        except Exception:
            return None

    # --- POST --------------------------------------------------------------
    def do_POST(self):
        login = self._login()
        if not login:
            return self._json(401, {"ok": False, "error": "not signed in"})
        token = self.headers.get("X-Panel-Token", "")
        if not token or _verify(token, "csrf") != login:
            return self._json(403, {"ok": False, "error": "bad CSRF token"})

        path = self.path.split("?", 1)[0]
        if path == "/api/deploy":
            rc, out = run(["bash", f"{ARENA_DIR}/ops/autodeploy.sh"], timeout=900)
            if rc == 0:
                return self._json(200, {"ok": True, "message": "Deployed", "error": None, "reason": None})
            reason = parse_progress(read_log_tail(DEPLOY_LOG))["reason"] or "failed"
            why = FAILURES.get(reason, FAILURES["failed"])
            return self._json(200, {"ok": False, "reason": reason, "message": why,
                                    "error": why + (" " + out[-400:] if out else "")})
        if path == "/api/rollback":
            # Delegated, like /api/deploy. This used to `git reset --hard HEAD~1`
            # and rebuild with a bare `docker compose`, which had two problems:
            # compose reads only backend/.env, so it rebuilt as the PYTHON Arena
            # whatever was actually released, and a rebuild of the previous
            # commit is not the previous release -- ops/release.sh restores an
            # earlier stamped IMAGE without rebuilding, which is both faster and
            # the only version that is known to have been healthy.
            rc, out = run(["bash", f"{ARENA_DIR}/ops/release.sh", "rollback"], timeout=900)
            return self._json(200, {"ok": rc == 0,
                                    "message": "Rolled back" if rc == 0 else "Rollback failed",
                                    "error": None if rc == 0 else out[-400:]})
        self._json(404, {"ok": False, "error": "unknown action"})

    def log_message(self, *a):
        return


def main():
    missing = [n for n, v in [("PANEL_SECRET_KEY", SECRET_KEY),
                              ("PANEL_GITHUB_CLIENT_ID", CLIENT_ID),
                              ("PANEL_GITHUB_CLIENT_SECRET", CLIENT_SECRET),
                              ("PANEL_BASE_URL", PANEL_BASE_URL)] if not v]
    if missing:
        raise SystemExit("missing required env: " + ", ".join(missing))
    if not ALLOWED:
        raise SystemExit("PANEL_ALLOWED_USERS is empty — refusing to start with no allowlist")
    # In a container with no published port, Docker provides the isolation and
    # binding all interfaces is correct. On a host it is not, so the refusal
    # stands unless the container image explicitly says otherwise.
    in_container = os.environ.get("PANEL_IN_CONTAINER") == "1"
    if not in_container and not (
        BIND.startswith("127.") or BIND.startswith("172.")
        or BIND.startswith("10.") or BIND.startswith("192.168.")
    ):
        raise SystemExit(f"refusing to bind {BIND}: panel must not listen on a public interface")
    print(f"deploy panel on {BIND}:{PORT}; allowed: {', '.join(sorted(ALLOWED))}", flush=True)
    ThreadingHTTPServer((BIND, PORT), Handler).serve_forever()


if __name__ == "__main__":
    main()
