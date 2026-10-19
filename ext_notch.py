"""HQ Notch, from HQ: is it on this Mac, and install it with one click (stdlib only).

GET  /api/notch/status   -> {mac, installed, running, tools, available, job}
POST /api/notch/install  -> runs notch/tools/install.py in the background (macOS only)

The install is the same as running `python3 notch/tools/install.py` yourself: it builds the
app with the Xcode Command Line Tools, puts it in ~/Applications, and adds HQ Notch's hooks to
~/.claude/settings.json (backing the file up first, and touching only its own entries). It
only ever runs because you clicked Install in HQ (a POST with HQ's CSRF token). Nothing here
talks to the network.
"""
import os
import shutil
import subprocess
import sys
import threading
import time

HERE = os.path.dirname(os.path.abspath(__file__))
INSTALLER = os.path.join(HERE, "notch", "tools", "install.py")
APP = os.path.join(os.path.expanduser("~"), "Applications", "HQ Notch.app")
LOG_MAX = 2000                      # how much of the installer's output the page may show
TIMEOUT = 900                       # a first build takes a minute or two; never hang forever

_lock = threading.Lock()
_job = {"state": "idle", "started": None, "rc": None, "log": ""}


def _mac():
    return sys.platform == "darwin"


def _running():
    if not _mac():
        return False
    try:
        return subprocess.run(["pgrep", "-x", "HQNotch"], capture_output=True, timeout=3).returncode == 0
    except Exception:
        return False


def _job_view():
    with _lock:
        j = dict(_job)
    j["log"] = j["log"][-LOG_MAX:]
    return j


def status(arg=None):
    return 200, {
        "mac": _mac(),
        "installed": _mac() and os.path.isdir(APP),
        "running": _running(),
        "tools": shutil.which("swift") is not None,
        "available": os.path.isfile(INSTALLER),
        "job": _job_view(),
    }


def _run():
    try:
        p = subprocess.run([sys.executable, INSTALLER], cwd=HERE, capture_output=True, text=True, timeout=TIMEOUT)
        out, rc = (p.stdout or "") + (p.stderr or ""), p.returncode
    except subprocess.TimeoutExpired:
        out, rc = "the install took longer than %d s and was stopped" % TIMEOUT, -1
    except Exception as e:                          # pragma: no cover - defensive
        out, rc = "could not run the installer: %s" % e, -1
    with _lock:
        _job.update(state="done" if rc == 0 else "failed", rc=rc, log=out[-LOG_MAX:])


def install(body=None):
    """Start the installer once. 202 started, 409 already running, 400 when it cannot run here."""
    if not _mac():
        return 400, {"error": "HQ Notch is for macOS"}
    if not os.path.isfile(INSTALLER):
        return 400, {"error": "notch/tools/install.py is missing from this HQ"}
    if shutil.which("swift") is None:
        return 400, {"error": "needs the Xcode Command Line Tools: run xcode-select --install, then try again"}
    with _lock:
        if _job["state"] == "running":
            return 409, {"error": "already installing"}
        _job.update(state="running", started=time.time(), rc=None, log="")
    threading.Thread(target=_run, name="hq-notch-install", daemon=True).start()
    return 202, {"started": True}


GET = {"/api/notch/status": status}
