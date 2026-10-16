#!/usr/bin/env python3
"""Parity check: the Python dashboard.py vs the Rust claudehq, side by side.

Both servers run against ONE temp copy of the Claude HQ folder (code symlinked,
state files copied, NO arena-link.json, Arena URL blanked, work signals off), so
neither can pair with or post to an Arena, and every mutation lands in the temp
dir. Both read the same ~/.claude (and ~/.cursor) transcripts.

Then it GETs every route on both, diffs status, key headers and normalized JSON,
replays a set of safe POSTs on each (resetting the temp state in between, and
comparing the state files they leave), and prints a report. Stdlib only.

    python3 dashboard-rs/tools/parity.py [--rust-bin PATH] [--py-port 18765] [--rs-port 18766]

Never uses port 8765 (the live dashboard). Never opens a browser (--no-open).
Skips /api/music/now (AppleScript to music apps), /api/music/search with a real
query and /api/music/oembed with a real id (YouTube), and POST /api/update
(git fetch + restart): those would leave the machine or touch other apps.
"""
import argparse
import base64
import http.client
import json
import os
import re
import shutil
import signal
import subprocess
import sys
import tempfile
import threading
import time
import urllib.parse

HERE = os.path.dirname(os.path.abspath(__file__))
CRATE = os.path.dirname(HERE)
REPO = os.path.dirname(CRATE)

CODE_FILES = ("dashboard.py", "arena.py", "music.py", "worksignals.py", "ext_boss.py", "ext_cups.py",
              "ext_loot.py", "ext_maps.py", "ext_prestige.py", "ext_skills.py", "index.html")
CODE_DIRS = ("ui", "games")
STATE_FILES = ("config.json", "sessions-meta.json", "meals.json", "games-save.json", ".dex-seed")

# Values that legitimately differ between two processes or two instants.
VOLATILE_KEYS = {"updated", "generated", "boot", "pid", "uptimeSecs", "cpuPct", "rssMb", "threads",
                 "load", "python", "impl", "ageSecs"}
# Time-driven values (fatigue is recomputed against the wall clock on every build).
CLOCK_KEYS = {"energy", "loadMins", "restInMins", "restMins", "streakMins", "phase", "state", "spark",
              "sparkHourly", "stuck", "stuckReason", "mayFaint"}


def log(*a):
    print(*a, flush=True)


# --------------------------------------------------------------------------- #
# Setup
# --------------------------------------------------------------------------- #

def make_root(src_root):
    tmp = tempfile.mkdtemp(prefix="hq-parity-")
    for f in CODE_FILES:
        shutil.copy2(os.path.join(REPO, f), os.path.join(tmp, f))
    for d in CODE_DIRS:
        os.symlink(os.path.join(REPO, d), os.path.join(tmp, d))
    for f in STATE_FILES:
        p = os.path.join(src_root, f)
        if os.path.exists(p):
            shutil.copy2(p, os.path.join(tmp, f))
    cfg_path = os.path.join(tmp, "config.json")
    try:
        with open(cfg_path, encoding="utf-8") as f:
            cfg = json.load(f)
    except Exception:
        cfg = {}
    # Nothing may leave the machine: no Arena URL, sharing off, work signals off.
    cfg.update(arenaUrl="", arenaEnabled=False, workSignals=False, workSignalsPRs=False, musicShare=False)
    with open(cfg_path, "w", encoding="utf-8") as f:
        json.dump(cfg, f, indent=2)
    assert not os.path.exists(os.path.join(tmp, "arena-link.json"))
    return tmp


def snapshot(root):
    out = {}
    for name in sorted(os.listdir(root)):
        p = os.path.join(root, name)
        if os.path.islink(p) or name in CODE_FILES or name.startswith("."):
            continue
        if os.path.isfile(p) and name.endswith(".json"):
            with open(p, "rb") as f:
                out[name] = f.read()
    sd = os.path.join(root, "sounds")
    if os.path.isdir(sd):
        for n in sorted(os.listdir(sd)):
            with open(os.path.join(sd, n), "rb") as f:
                out["sounds/" + n] = f.read()
    return out


def restore(root, snap):
    for name in list(os.listdir(root)):
        p = os.path.join(root, name)
        if name.endswith(".json") and os.path.isfile(p) and not os.path.islink(p) and name not in snap:
            os.remove(p)
    shutil.rmtree(os.path.join(root, "sounds"), ignore_errors=True)
    for name, data in snap.items():
        p = os.path.join(root, name)
        os.makedirs(os.path.dirname(p), exist_ok=True)
        with open(p, "wb") as f:
            f.write(data)


def make_home(root):
    """A frozen HOME: copies of the transcript folders (live sessions keep
    appending to the real ones) and a stub `claude` that lists a fixed set of
    live agents, so both servers build from identical, still data."""
    home = os.path.join(root, "home")
    real = os.path.expanduser("~")
    for rel in (".claude/projects", ".cursor/projects"):
        src = os.path.join(real, rel)
        if os.path.isdir(src):
            shutil.copytree(src, os.path.join(home, rel), symlinks=True)
    db = "Library/Application Support/Cursor/User/globalStorage/state.vscdb"
    if os.path.isfile(os.path.join(real, db)) and os.path.getsize(os.path.join(real, db)) < 512 * 1024 * 1024:
        os.makedirs(os.path.dirname(os.path.join(home, db)), exist_ok=True)
        shutil.copy2(os.path.join(real, db), os.path.join(home, db))
    paths = []
    for d in os.listdir(os.path.join(home, ".claude/projects")) if os.path.isdir(os.path.join(home, ".claude/projects")) else []:
        dp = os.path.join(home, ".claude/projects", d)
        if os.path.isdir(dp):
            paths += [os.path.join(dp, f) for f in os.listdir(dp) if f.endswith(".jsonl")]
    paths.sort(key=os.path.getmtime, reverse=True)
    sids = [os.path.basename(p)[:-6] for p in paths[:3]]
    agents = []
    for i, sid in enumerate(sids):
        a = {"sessionId": sid, "cwd": "/tmp/parity-%d" % i, "name": "agent-%d" % i, "pid": 900000 + i,
             "startedAt": 1790000000000}
        if i == 0:
            a.update(kind="interactive", status="busy")
        elif i == 1:
            a.update(kind="interactive", status="idle")
        else:
            a.update(kind="background", state="blocked")
        agents.append(a)
    stub = os.path.join(home, ".claude/local/claude")
    os.makedirs(os.path.dirname(stub), exist_ok=True)
    with open(stub, "w") as f:
        f.write("#!/bin/sh\ncat <<'EOF'\n%s\nEOF\n" % json.dumps(agents))
    os.chmod(stub, 0o755)
    return home


ENV = dict(os.environ)


def start(cmd, cwd, port, logf):
    proc = subprocess.Popen(cmd, cwd=cwd, stdout=logf, stderr=subprocess.STDOUT, start_new_session=True, env=ENV)
    for _ in range(300):
        try:
            c = http.client.HTTPConnection("127.0.0.1", port, timeout=60)
            c.request("GET", "/api/config")
            c.getresponse().read()
            c.close()
            return proc
        except OSError:
            if proc.poll() is not None:
                raise SystemExit("server exited: %s" % " ".join(cmd))
            time.sleep(0.2)
    raise SystemExit("server did not come up: %s" % " ".join(cmd))


def stop(proc):
    if proc and proc.poll() is None:
        os.killpg(proc.pid, signal.SIGTERM)
        try:
            proc.wait(5)
        except subprocess.TimeoutExpired:
            os.killpg(proc.pid, signal.SIGKILL)


# --------------------------------------------------------------------------- #
# HTTP
# --------------------------------------------------------------------------- #

def request(port, method, path, body=None, headers=None, timeout=120):
    c = http.client.HTTPConnection("127.0.0.1", port, timeout=timeout)
    try:
        c.putrequest(method, path, skip_host=True, skip_accept_encoding=True)
        hdrs = {"Host": "127.0.0.1:%d" % port}
        hdrs.update(headers or {})
        for k, v in hdrs.items():
            if v is not None:
                c.putheader(k, v)
        if body is not None and "Content-Length" not in hdrs:
            c.putheader("Content-Length", str(len(body)))
        c.endheaders()
        if body:
            c.send(body)
        r = c.getresponse()
        data = r.read() if method != "HEAD" else b""
        return r.status, {k.lower(): v for k, v in r.getheaders()}, data
    finally:
        c.close()


def first_sse_frame(port):
    c = http.client.HTTPConnection("127.0.0.1", port, timeout=120)
    c.request("GET", "/api/stream")
    r = c.getresponse()
    buf = b""
    while b"\n\n" not in buf:
        chunk = r.read1(65536) if hasattr(r, "read1") else r.read(1)
        if not chunk:
            break
        buf += chunk
    c.close()
    return r.status, r.getheader("Content-Type"), buf.split(b"\n\n", 1)[0]


def both(fn, *args):
    """Run fn(py_port, ...) and fn(rs_port, ...) at the same time."""
    out = [None, None]

    def run(i, port):
        try:
            out[i] = fn(port, *args)
        except Exception as e:  # noqa: BLE001
            out[i] = ("EXC", repr(e))
    ts = [threading.Thread(target=run, args=(0, PY)), threading.Thread(target=run, args=(1, RS))]
    for t in ts:
        t.start()
    for t in ts:
        t.join()
    return out


# --------------------------------------------------------------------------- #
# Diff
# --------------------------------------------------------------------------- #

def normalize(v, drop=VOLATILE_KEYS):
    if isinstance(v, dict):
        return {k: normalize(x, drop) for k, x in v.items() if k not in drop}
    if isinstance(v, list):
        return [normalize(x, drop) for x in v]
    if isinstance(v, float) and v == int(v) and abs(v) < 2 ** 53:
        return float(v)
    return v


def diff(a, b, path="", out=None, ignore=frozenset()):
    out = [] if out is None else out
    if isinstance(a, dict) and isinstance(b, dict):
        for k in list(a) + [k for k in b if k not in a]:
            if k in ignore:
                continue
            if k not in a:
                out.append((path + "/" + k, "<missing>", b[k]))
            elif k not in b:
                out.append((path + "/" + k, a[k], "<missing>"))
            else:
                diff(a[k], b[k], path + "/" + k, out, ignore)
        if list(a) != list(b) and set(a) == set(b):
            out.append((path + " (key order)", list(a), list(b)))
    elif isinstance(a, list) and isinstance(b, list):
        if len(a) != len(b):
            out.append((path + " (len)", len(a), len(b)))
        for i, (x, y) in enumerate(zip(a, b)):
            diff(x, y, "%s[%d]" % (path, i), out, ignore)
    elif isinstance(a, (int, float)) and isinstance(b, (int, float)) and not isinstance(a, bool) and not isinstance(b, bool):
        if a != b:
            out.append((path, a, b))
    elif a != b or type(a) is not type(b):
        out.append((path, a, b))
    return out


def short(v, n=140):
    s = json.dumps(v, ensure_ascii=False) if not isinstance(v, str) else repr(v)
    return s if len(s) <= n else s[: n - 1] + "…"


RESULTS = []   # (name, ok, notes)


def record(name, ok, notes=()):
    RESULTS.append((name, ok, list(notes)))
    mark = "OK  " if ok else "DIFF"
    log("%s %s" % (mark, name))
    for n in list(notes)[:25]:
        log("       " + n)
    if len(notes) > 25:
        log("       … %d more" % (len(notes) - 25))


BYTES = {"same": 0, "json": 0}


def compare_json(name, rp, rr, ignore=frozenset(), drop_clock=False):
    (s1, h1, b1), (s2, h2, b2) = rp, rr
    notes = []
    BYTES["json"] += 1
    BYTES["same"] += b1 == b2
    if s1 != s2:
        notes.append("status %s vs %s" % (s1, s2))
    ct1, ct2 = h1.get("content-type"), h2.get("content-type")
    if ct1 != ct2:
        notes.append("content-type %r vs %r" % (ct1, ct2))
    for h in ("cache-control", "content-disposition", "etag"):
        if h1.get(h) != h2.get(h):
            notes.append("%s %r vs %r" % (h, h1.get(h), h2.get(h)))
    drop = VOLATILE_KEYS | (CLOCK_KEYS if drop_clock else set())
    try:
        j1, j2 = json.loads(b1), json.loads(b2)
    except ValueError:
        if b1 != b2:
            notes.append("body differs (%d vs %d bytes)" % (len(b1), len(b2)))
        record(name, not notes, notes)
        return
    for p, a, b in diff(normalize(j1, drop), normalize(j2, drop), ignore=ignore):
        notes.append("%s: py=%s rs=%s" % (p, short(a), short(b)))
    record(name, not notes, notes)


def compare_raw(name, rp, rr, headers=("content-type", "cache-control", "etag", "content-disposition")):
    (s1, h1, b1), (s2, h2, b2) = rp, rr
    notes = []
    if s1 != s2:
        notes.append("status %s vs %s" % (s1, s2))
    for h in headers:
        if h1.get(h) != h2.get(h):
            notes.append("%s %r vs %r" % (h, h1.get(h), h2.get(h)))
    if b1 != b2:
        i = next((k for k, (x, y) in enumerate(zip(b1, b2)) if x != y), min(len(b1), len(b2)))
        notes.append("body differs (%d vs %d bytes) at byte %d: py=%r rs=%r"
                     % (len(b1), len(b2), i, b1[max(0, i - 40):i + 40], b2[max(0, i - 40):i + 40]))
    record(name, not notes, notes)


# --------------------------------------------------------------------------- #
# The run
# --------------------------------------------------------------------------- #

def get(port, path, headers=None):
    return request(port, "GET", path, headers=headers)


def main():
    global PY, RS
    ap = argparse.ArgumentParser()
    ap.add_argument("--rust-bin", default=os.path.join(CRATE, "target", "release", "claudehq"))
    ap.add_argument("--py-port", type=int, default=18765)
    ap.add_argument("--rs-port", type=int, default=18766)
    ap.add_argument("--state-from", default=REPO, help="folder whose state files are copied")
    ap.add_argument("--keep", action="store_true", help="keep the temp folder")
    ap.add_argument("--frozen-home", action="store_true",
                    help="run both servers on a copied ~/.claude + ~/.cursor and a stub `claude agents`")
    args = ap.parse_args()
    PY, RS = args.py_port, args.rs_port
    assert 8765 not in (PY, RS), "8765 is the live dashboard"
    if not os.path.exists(args.rust_bin):
        raise SystemExit("build it first: ~/.cargo/bin/cargo build --release --manifest-path %s/Cargo.toml" % CRATE)

    root = make_root(args.state_from)
    log("temp root: %s" % root)
    if args.frozen_home:
        ENV["HOME"] = make_home(root)
        log("frozen HOME: %s" % ENV["HOME"])
    pylog = open(os.path.join(root, "py.log"), "w")
    rslog = open(os.path.join(root, "rs.log"), "w")
    py = rs = None
    try:
        py = start([sys.executable, os.path.join(root, "dashboard.py"), "--port", str(PY), "--no-open"], root, PY, pylog)
        rs = start([args.rust_bin, "--root", root, "--port", str(RS), "--no-open"], root, RS, rslog)
        run_gets(root)
        run_posts(root)
    finally:
        stop(py)
        stop(rs)
        pylog.close()
        rslog.close()
        if not args.keep:
            shutil.rmtree(root, ignore_errors=True)
    bad = [r for r in RESULTS if not r[1]]
    log("\n%d checks, %d identical, %d with differences" % (len(RESULTS), len(RESULTS) - len(bad), len(bad)))
    log("%d of %d JSON bodies byte-identical (the rest differ only in volatile keys: clock, pid, boot id)"
        % (BYTES["same"], BYTES["json"]))
    for name, _, notes in bad:
        log("  - %s (%d)" % (name, len(notes)))


def run_gets(root):
    log("\n== GET ==")
    # Page shell and static files.
    rp, rr = both(get, "/")
    csrf_re = re.compile(rb'<meta name="hq-csrf" content="([0-9a-f]{32})">')
    tok_py, tok_rs = csrf_re.search(rp[2]), csrf_re.search(rr[2])
    body_p = rp[2].replace(tok_py.group(1), b"TOKEN") if tok_py else rp[2]
    body_r = rr[2].replace(tok_rs.group(1), b"TOKEN") if tok_rs else rr[2]
    compare_raw("GET / (CSRF token masked)", (rp[0], rp[1], body_p), (rr[0], rr[1], body_r))
    global TOKENS
    TOKENS = (tok_py.group(1).decode() if tok_py else "", tok_rs.group(1).decode() if tok_rs else "")
    for p in ("/manifest.webmanifest", "/icon.svg", "/sw.js", "/games/core.js", "/games/games.css",
              "/games/golf/courses.json", "/games/vendor/three-core.js", "/games/../dashboard.py",
              "/games/..%2fdashboard.py", "/games/Core.js", "/games/golf/LICENSE-kenney.txt"):
        compare_raw("GET " + p, *both(get, p))
    rp, rr = both(get, "/games/golf/courses.json")
    tag = rp[1].get("etag")
    if tag:
        compare_raw("GET /games/golf/courses.json If-None-Match", *both(get, "/games/golf/courses.json", {"If-None-Match": tag}))
    compare_raw("GET / with a foreign Host", *both(lambda port: request(port, "GET", "/api/config", headers={"Host": "evil.example"})))
    compare_raw("HEAD /api/export.csv", *both(lambda port: request(port, "HEAD", "/api/export.csv")),
                headers=("content-type", "cache-control", "content-disposition"))
    compare_raw("HEAD /", *both(lambda port: request(port, "HEAD", "/")), headers=("content-type", "cache-control"))
    compare_raw("HEAD /anything", *both(lambda port: request(port, "HEAD", "/anything")), headers=("content-type",))

    # The live payload. Build both at the same moment (each runs `claude agents`).
    rp, rr = both(get, "/api/sessions")
    compare_json("GET /api/sessions", rp, rr, drop_clock=True)
    j1, j2 = json.loads(rp[2]), json.loads(rr[2])
    strict = diff(normalize(j1), normalize(j2))
    log("     (info) /api/sessions with clock-driven keys kept: %d differing values%s"
        % (len(strict), "".join("\n       %s: py=%s rs=%s" % (p, short(a), short(b)) for p, a, b in strict[:8])))
    sessions = json.loads(rp[2]).get("sessions", [])
    log("     (info) %d sessions, %d feed events, season xp %s"
        % (len(sessions), len(j1.get("feed", [])), j1.get("season", {}).get("xp")))
    sids = [s["sessionId"] for s in sessions[:4]]
    cursor = [s["sessionId"] for s in sessions if s.get("source") == "cursor"][:1]
    folder = sessions[0]["folder"] if sessions else ""

    s1 = both(first_sse_frame)
    notes = []
    for i, (st, ct, frame) in enumerate(s1):
        if st != 200 or not ct.startswith("text/event-stream") or not frame.startswith(b"data: "):
            notes.append("%s: %r %r %r" % (("py", "rs")[i], st, ct, frame[:80]))
    if not notes:
        a, b = (json.loads(f[2][6:]) for f in s1)
        if set(a) != set(b):
            notes.append("frame keys %s vs %s" % (sorted(a), sorted(b)))
    record("GET /api/stream (first frame)", not notes, notes)

    simple = ["/api/games/state", "/api/arena/status", "/api/arena/sounds", "/api/arena/sounds/nope.mp3",
              "/api/arena/sounds/..%2fx.mp3", "/api/arena/preview", "/api/arena/rooms",
              "/api/arena/rooms/members?roomId=bad", "/api/arena/rooms/members?roomId=r_ABCDEFGHIJKLMNOPQRSTUv",
              "/api/arena/board", "/api/arena/board?window=7d", "/api/arena/pantry", "/api/arena/quickplay/status",
              "/api/arena/crews", "/api/arena/crews/mine", "/api/arena/cosmetics", "/api/arena/server-stats",
              "/api/arena/progress", "/api/arena/leaderboards?game=kart", "/api/arena/profile?u=bad",
              "/api/music/search?q=", "/api/music/oembed?v=bad", "/api/music/audio-state?v=../x",
              "/api/arena/music/now", "/api/arena/hq/me", "/api/arena/hq/open", "/api/arena/hq/visit?u=x",
              "/api/arena/cali/board?window=forever", "/api/arena/cali/orders", "/api/update", "/api/config",
              "/api/meta", "/api/arena/skills", "/api/skills/local", "/api/arena/loot", "/api/loot/status",
              "/api/arena/prestige", "/api/arena/maps?kind=golf", "/api/arena/maps/one?id=nope",
              "/api/arena/maps/featured", "/api/arena/cups", "/api/arena/cups/one?id=cup-2026-W41-td",
              "/api/arena/cups/season?season=2026-13", "/api/arena/cups/trophies", "/api/arena/boss",
              "/api/arena/boss/badges?u=../x", "/api/nope", "/api/session/abc", "/api/session/%2A",
              "/api/transcript/abc", "/api/session/abc/export.md",
              "/api/session/00000000-0000-4000-8000-000000000000", "/api/project?folder=../etc",
              "/api/search?q=", "/api/search?q=%20%20", "/api/digest?date=2026-09-01",
              "/api/digest?date=2026-10-10&days=7", "/api/digest?date=bad&days=x", "/api/history",
              "/api/pokedex", "/api/insights"]
    for p in simple:
        compare_json("GET " + p, *both(get, p))
    compare_json("GET /api/server-stats (shape)", *both(get, "/api/server-stats"),
                 ignore=frozenset({"machineRamMb", "cpus", "platform", "version"}))

    for sid in sids + cursor:
        compare_json("GET /api/session/" + sid[:8], *both(get, "/api/session/" + sid), drop_clock=True)
        compare_json("GET /api/transcript/" + sid[:8], *both(get, "/api/transcript/%s?offset=0&limit=200" % sid))
        compare_json("GET /api/transcript/%s?q=the" % sid[:8], *both(get, "/api/transcript/%s?q=the&offset=2&limit=5" % sid))
        compare_raw("GET /api/session/%s/export.md" % sid[:8], *both(get, "/api/session/%s/export.md" % sid))
    if folder:
        compare_json("GET /api/project?folder=" + folder, *both(get, "/api/project?folder=" + urllib.parse.quote(folder)))
    for q in ("the", "rust port", "dashboard", "zzzzqqq", "<>"):
        rp, rr = both(get, "/api/search?q=" + urllib.parse.quote(q))
        compare_json("GET /api/search?q=%s (%s results)" % (q, json.loads(rp[2]).get("count")), rp, rr)
    compare_json("GET /api/digest", *both(get, "/api/digest"))
    compare_raw("GET /api/digest?download=1", *both(get, "/api/digest?download=1&date=2026-10-10&days=3"))
    rp, rr = both(get, "/api/export.json")
    compare_json("GET /api/export.json", rp, rr, drop_clock=True)
    rp, rr = both(get, "/api/export.csv")
    strip_age = lambda b: b"\r\n".join(re.sub(rb"^([^,]*,(?:\"[^\"]*\"|[^,]*),[^,]*,[^,]*,[^,]*,)[^,]*", rb"\1AGE", ln)
                                       for ln in b.split(b"\r\n"))
    compare_raw("GET /api/export.csv (ages masked)", (rp[0], rp[1], strip_age(rp[2])), (rr[0], rr[1], strip_age(rr[2])))


def post(port, path, body, token=True, headers=None):
    data = body if isinstance(body, bytes) else json.dumps(body).encode()
    h = {"Content-Type": "application/json"}
    if token:
        h["X-HQ-Token"] = TOKENS[0] if port == PY else TOKENS[1]
    h.update(headers or {})
    return request(port, "POST", path, body=data, headers=h)


def run_posts(root):
    log("\n== POST (state reset between servers) ==")
    base = snapshot(root)
    sid = "11111111-1111-4111-8111-111111111111"
    rid = "a1b2c3d4-e5f6-4a7b-8c9d-0e1f2a3b4c5d"
    wav = base64.b64encode(b"RIFF....WAVEfmt ").decode()
    cases = [
        ("/api/config", {"theme": "forest", "dailyBudgetUSD": 12.5, "trainerName": "  Ash \u0007  K ", "refreshMs": "3000",
                         "character": [6, -1, "x"], "trainerAvatar": [99], "musicCookies": "safari", "bogus": 1}),
        ("/api/config", {"stuckMinutes": 999, "dailyBudgetUSD": -1}),
        ("/api/meta", {"sessionId": sid, "pinned": 1, "tags": [" a ", 5, "b" * 30], "note": "n", "name": "  Alias  "}),
        ("/api/meta", {"sessionId": "nope"}),
        ("/api/games/state", {"state": {"v": 1, "inv": {"koi": 2}, "é": "ü"}}),
        ("/api/games/state", {"state": [1]}),
        ("/api/action", {"action": "nope"}),
        ("/api/action", {"action": "resume", "sessionId": "bad"}),
        ("/api/action", {"action": "reveal", "sessionId": "00000000-0000-4000-8000-000000000000"}),
        ("/api/action", {"action": "close", "pid": "x"}),
        ("/api/action", {"action": "close", "pid": 1}),
        ("/api/arena/pair", {"code": "abcd"}),
        ("/api/arena/pair", {}),
        ("/api/arena/unpair", {}),
        ("/api/arena/publish", {}),
        ("/api/arena/ticket", {}),
        ("/api/arena/nudge", {"toHandle": "gary"}),
        ("/api/arena/nudge", {}),
        ("/api/arena/pantry/claim", {}),
        ("/api/arena/pantry/buy", {"requestId": rid, "kind": "berry", "qty": 9}),
        ("/api/arena/pantry/buy", {"requestId": rid, "kind": "berry"}),
        ("/api/arena/pantry/give", {"requestId": rid, "toHandle": "ga ry", "coins": 1}),
        ("/api/arena/pantry/eat", {"sessionId": "bad", "kind": "berry", "requestId": rid}),
        ("/api/arena/pantry/eat", {"sessionId": sid, "kind": "pizza", "requestId": rid}),
        ("/api/arena/pantry/eat", {"sessionId": sid, "kind": "berry", "requestId": rid}),
        ("/api/arena/pantry/reward", {"requestId": "hack:x:y", "kind": "quest"}),
        ("/api/arena/pantry/reward", {"requestId": "quest:d_x:2026-10-01", "kind": "quest", "questId": "d_x", "coins": 2}),
        ("/api/arena/pantry/steal", {}),
        ("/api/arena/hq/me", {"open": True}),
        ("/api/arena/portrait", {"png": "data:image/jpeg;base64,AAAA"}),
        ("/api/arena/portrait", {"remove": True}),
        ("/api/arena/cosmetics/buy", {"item": "k-red"}),
        ("/api/arena/market/sell", {"cat": "fish", "qty": 2}),
        ("/api/arena/crews/create", {"name": "x"}),
        ("/api/arena/quickplay/join", {"game": "kart"}),
        ("/api/arena/cali/order", {"requestId": rid, "diners": []}),
        ("/api/arena/cali/order", {"requestId": rid, "diners": [{"name": "Ana", "tacos": {"mildHard": 3}}]}),
        ("/api/arena/rooms/create", {}),
        ("/api/arena/rooms/join", {"roomId": "r_ABCDEFGHIJKLMNOPQRSTUv", "password": "pw"}),
        ("/api/arena/rooms/kick", {"roomId": "r_ABCDEFGHIJKLMNOPQRSTUv", "userId": "bad"}),
        ("/api/arena/skills/title", {"title": "tests-3"}),
        ("/api/arena/skills/title", {"title": "magic-9"}),
        ("/api/arena/loot/open", {"chestId": "bad"}),
        ("/api/arena/prestige/claim", {}),
        ("/api/arena/maps/save", {"kind": "golf"}),
        ("/api/arena/maps/like", {"id": "m-0123456789ab", "on": True}),
        ("/api/arena/boss/fight", {"team": []}),
        ("/api/arena/sounds", {"name": "ding.wav", "data": wav}),
        ("/api/arena/sounds", {"name": "x.exe", "data": wav}),
        ("/api/nope", {}),
    ]
    for path, body in cases:
        restore(root, base)
        rp = post(PY, path, body)
        snap_py = snapshot(root)
        restore(root, base)
        rr = post(RS, path, body)
        snap_rs = snapshot(root)
        name = "POST %s %s" % (path, short(body, 70))
        compare_json(name, rp, rr)
        notes = []
        for k in sorted(set(snap_py) | set(snap_rs)):
            a, b = snap_py.get(k), snap_rs.get(k)
            if a == b:
                continue
            try:
                ja, jb = json.loads(a), json.loads(b)
                d = diff(normalize(ja), normalize(jb))
                if d:
                    notes += ["%s%s: py=%s rs=%s" % (k, p, short(x), short(y)) for p, x, y in d]
                else:
                    notes.append("%s: same JSON, different bytes" % k)
            except (TypeError, ValueError):
                notes.append("%s: %s vs %s" % (k, "absent" if a is None else "%d bytes" % len(a),
                                               "absent" if b is None else "%d bytes" % len(b)))
        if notes:
            record("  state after " + name, False, notes)
    restore(root, base)
    # The gates.
    compare_json("POST without the CSRF token", *both(lambda p: post(p, "/api/config", {}, token=False)))
    compare_json("POST from a foreign Origin", *both(lambda p: post(p, "/api/config", {}, headers={"Origin": "https://evil.example"})))
    compare_json("POST with Sec-Fetch-Site: cross-site", *both(lambda p: post(p, "/api/config", {}, headers={"Sec-Fetch-Site": "cross-site"})))
    compare_json("POST with a foreign Host", *both(lambda p: post(p, "/api/config", {}, headers={"Host": "evil.example"})))
    compare_json("POST over the size cap (413)", *both(lambda p: post(p, "/api/config", b"", headers={"Content-Length": str(1024 * 1024 + 1)})))
    compare_json("POST with a bad Content-Length (400)", *both(lambda p: post(p, "/api/config", b"", headers={"Content-Length": "abc"})))
    compare_json("POST with an empty body", *both(lambda p: post(p, "/api/config", b"", headers={"Content-Length": "0"})))
    restore(root, base)
    compare_json("POST a JSON array body", *both(lambda p: post(p, "/api/config", b"[1]")))
    compare_json("POST invalid JSON", *both(lambda p: post(p, "/api/config", b"{nope")), ignore=frozenset({"error"}))
    restore(root, base)


PY = RS = 0
TOKENS = ("", "")

if __name__ == "__main__":
    main()
