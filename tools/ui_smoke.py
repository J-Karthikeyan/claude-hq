#!/usr/bin/env python3
"""UI smoke test: start the HQ server and load the page in headless Chrome.

    python3 tools/ui_smoke.py [path/to/claudehq]

Runs claudehq against a throwaway HQ folder (the frontend symlinked in, no state
files) with an empty HOME, so it never reads or writes your real sessions or
settings. Fails when the page or a core API does not answer, when an @include
was left unassembled, or when the page throws a JavaScript error while loading.
Stdlib only; needs Google Chrome (or Chromium) for the browser half.
"""
import json
import os
import shutil
import socket
import subprocess
import sys
import tempfile
import time
import urllib.request

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BIN = sys.argv[1] if len(sys.argv) > 1 else os.path.join(ROOT, "dashboard-rs", "target", "release", "claudehq")
CHROMES = [
    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
    shutil.which("google-chrome") or "", shutil.which("chromium") or "", shutil.which("chromium-browser") or "",
]
# Console output Chrome prints for a page error. Network failures are not page
# bugs here: CI has no Arena and no sprite CDN guarantees.
JS_ERRORS = ("Uncaught", "SyntaxError", "ReferenceError", "TypeError")


def free_port():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def get(url):
    with urllib.request.urlopen(url, timeout=10) as r:
        return r.status, r.read().decode("utf-8", "replace")


def fail(msg, log=""):
    print("FAIL:", msg)
    if log:
        print(log[-4000:])
    sys.exit(1)


def main():
    if not os.access(BIN, os.X_OK):
        fail(f"no claudehq binary at {BIN} (cargo build --release --manifest-path dashboard-rs/Cargo.toml)")
    tmp = tempfile.mkdtemp(prefix="hq-ui-smoke-")
    root, home = os.path.join(tmp, "hq"), os.path.join(tmp, "home")
    os.makedirs(root)
    os.makedirs(os.path.join(home, ".claude", "projects"))
    for name in ("index.html", "ui", "games", "notch"):
        if os.path.exists(os.path.join(ROOT, name)):
            os.symlink(os.path.join(ROOT, name), os.path.join(root, name))
    port = free_port()
    base = f"http://127.0.0.1:{port}"
    log_path = os.path.join(tmp, "server.log")
    env = dict(os.environ, HOME=home, CLAUDEHQ_ROOT=root)
    with open(log_path, "w") as log:
        server = subprocess.Popen([BIN, "--no-open", "--port", str(port), "--root", root],
                                  env=env, stdout=log, stderr=subprocess.STDOUT)
    try:
        for _ in range(60):
            try:
                if get(base + "/")[0] == 200:
                    break
            except OSError:
                pass
            if server.poll() is not None:
                fail("the server exited while starting", open(log_path).read())
            time.sleep(0.5)
        else:
            fail("the server never answered GET /", open(log_path).read())

        status, page = get(base + "/")
        if "@include" in page:
            fail("GET / still contains an @include line: the page was not assembled")
        print(f"ok  GET / -> {status}, {len(page)} bytes, fully assembled")
        for path in ("/api/sessions", "/api/config", "/api/server-stats"):
            status, body = get(base + path)
            json.loads(body)
            print(f"ok  GET {path} -> {status}, JSON")

        chrome = next((c for c in CHROMES if c and os.path.exists(c)), None)
        if not chrome:
            fail("no Chrome/Chromium found for the browser check")
        out = subprocess.run(
            [chrome, "--headless=new", "--disable-gpu", "--no-sandbox", "--no-first-run",
             f"--user-data-dir={os.path.join(tmp, 'chrome')}", "--enable-logging=stderr", "--v=0",
             "--timeout=10000", "--dump-dom", base + "/"],
            capture_output=True, text=True, timeout=90)
        console = [ln for ln in out.stderr.splitlines() if "CONSOLE" in ln]
        errors = [ln for ln in console if any(k in ln for k in JS_ERRORS)]
        if errors:
            fail("the page threw while loading:\n" + "\n".join(errors[:20]))
        if "<body" not in out.stdout or len(out.stdout) < 10000:
            fail("Chrome rendered an empty page", out.stderr)
        print(f"ok  headless Chrome loaded the page: {len(out.stdout)} bytes of DOM, "
              f"{len(console)} console lines, no JavaScript errors")
    finally:
        server.terminate()
        try:
            server.wait(timeout=10)
        except subprocess.TimeoutExpired:
            server.kill()
        shutil.rmtree(tmp, ignore_errors=True)
    print("UI smoke test passed")


if __name__ == "__main__":
    main()
