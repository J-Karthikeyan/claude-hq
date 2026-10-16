#!/usr/bin/env python3
"""Build and install HQ Notch (macOS), or remove it. Stdlib only.

    python3 notch/tools/install.py               build, install app + hooks + login agent, launch
    python3 notch/tools/install.py --no-login    ... without starting at login
    python3 notch/tools/install.py --uninstall   remove app, hooks, login agent, caches
    python3 notch/tools/install.py --status      show what is installed

What it touches (and nothing else):
  ~/Applications/HQ Notch.app
  ~/Library/Application Support/HQNotch/        hook relay + socket
  ~/Library/LaunchAgents/com.claudehq.notch.plist
  ~/Library/Caches/HQNotch/                     sprite cache (uninstall only)
  ~/.claude/settings.json                       hook entries, backed up first; only entries
                                                whose command points at our relay are ever
                                                added or removed
"""
import json
import os
import plistlib
import shutil
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
NOTCH = os.path.dirname(HERE)
HOME = os.path.expanduser("~")
APP = os.path.join(HOME, "Applications", "HQ Notch.app")
SUPPORT = os.path.join(HOME, "Library", "Application Support", "HQNotch")
HOOK_DST = os.path.join(SUPPORT, "hq_notch_hook.py")
AGENT_LABEL = "com.claudehq.notch"
AGENT = os.path.join(HOME, "Library", "LaunchAgents", AGENT_LABEL + ".plist")
CACHE = os.path.join(HOME, "Library", "Caches", "HQNotch")
SETTINGS = os.path.join(HOME, ".claude", "settings.json")
BUNDLE_ID = "com.claudehq.notch"
VERSION = "0.1.0"

# Claude Code events the notch listens to. PermissionRequest blocks (the relay waits up
# to 115 s for the notch), so it gets Claude Code's longer hook timeout.
EVENTS = ["SessionStart", "SessionEnd", "UserPromptSubmit", "PreToolUse", "PostToolUse",
          "PostToolUseFailure", "PermissionRequest", "Notification", "Stop", "StopFailure",
          "SubagentStart", "SubagentStop"]
TIMEOUTS = {"PermissionRequest": 120}
MARKER = "HQNotch/hq_notch_hook.py"


def hook_command():
    return '/usr/bin/python3 "%s"' % HOOK_DST


def is_ours(h):
    return isinstance(h, dict) and MARKER in str(h.get("command", ""))


def merge_hooks(settings, install):
    """Return settings with our hook entries added (install) or removed. Pure (tested)."""
    s = json.loads(json.dumps(settings or {}))
    hooks = s.get("hooks") if isinstance(s.get("hooks"), dict) else {}
    for ev in list(hooks):
        groups = []
        for g in hooks[ev] if isinstance(hooks[ev], list) else []:
            if not isinstance(g, dict):
                groups.append(g)
                continue
            hs = [h for h in g.get("hooks", []) if not is_ours(h)]
            if hs:
                g = dict(g, hooks=hs)
                groups.append(g)
        if groups:
            hooks[ev] = groups
        else:
            del hooks[ev]
    if install:
        for ev in EVENTS:
            entry = {"type": "command", "command": hook_command(), "timeout": TIMEOUTS.get(ev, 10)}
            hooks.setdefault(ev, []).append({"matcher": "", "hooks": [entry]} if ev in (
                "PreToolUse", "PostToolUse", "PostToolUseFailure", "PermissionRequest") else {"hooks": [entry]})
    if hooks:
        s["hooks"] = hooks
    else:
        s.pop("hooks", None)
    return s


def write_settings(install):
    try:
        with open(SETTINGS, encoding="utf-8") as f:
            current = json.load(f)
    except FileNotFoundError:
        current = {}
    new = merge_hooks(current, install)
    if new == current:
        return False
    if os.path.exists(SETTINGS):
        backup = "%s.bak-hqnotch-%s" % (SETTINGS, time.strftime("%Y%m%d%H%M%S"))
        shutil.copy2(SETTINGS, backup)
        print("  backed up settings.json ->", backup)
    os.makedirs(os.path.dirname(SETTINGS), exist_ok=True)
    tmp = SETTINGS + ".hqnotch.tmp"
    with open(tmp, "w", encoding="utf-8") as f:
        json.dump(new, f, indent=2)
        f.write("\n")
    os.replace(tmp, SETTINGS)
    return True


def build():
    print("building (swift build -c release)…")
    subprocess.run(["swift", "build", "-c", "release", "--package-path", NOTCH], check=True)
    out = subprocess.run(["swift", "build", "-c", "release", "--package-path", NOTCH, "--show-bin-path"],
                         check=True, capture_output=True, text=True).stdout.strip()
    return os.path.join(out, "HQNotch")


def bundle(binary):
    macos = os.path.join(APP, "Contents", "MacOS")
    if os.path.exists(APP):
        shutil.rmtree(APP)
    os.makedirs(macos)
    shutil.copy2(binary, os.path.join(macos, "HQNotch"))
    info = {
        "CFBundleName": "HQ Notch", "CFBundleDisplayName": "HQ Notch", "CFBundleIdentifier": BUNDLE_ID,
        "CFBundleExecutable": "HQNotch", "CFBundlePackageType": "APPL",
        "CFBundleShortVersionString": VERSION, "CFBundleVersion": VERSION,
        "LSMinimumSystemVersion": "14.0", "LSUIElement": True, "NSHighResolutionCapable": True,
        "NSAppleEventsUsageDescription":
            "HQ Notch focuses the Terminal or iTerm2 tab a Claude session runs in when you click its card.",
    }
    with open(os.path.join(APP, "Contents", "Info.plist"), "wb") as f:
        plistlib.dump(info, f)
    # Ad-hoc signature: fine on this Mac; sharing the .app needs a Developer ID.
    subprocess.run(["codesign", "--force", "--sign", "-", APP], check=True, capture_output=True)


def install_hook():
    os.makedirs(SUPPORT, exist_ok=True)
    os.chmod(SUPPORT, 0o700)
    shutil.copy2(os.path.join(NOTCH, "hook", "hq_notch_hook.py"), HOOK_DST)
    os.chmod(HOOK_DST, 0o755)


def agent_plist():
    return {
        "Label": AGENT_LABEL,
        "ProgramArguments": [os.path.join(APP, "Contents", "MacOS", "HQNotch")],
        "RunAtLoad": True,
        "KeepAlive": {"SuccessfulExit": False},   # restart after a crash, not after Quit
        "ProcessType": "Interactive",
    }


def launchctl(*args):
    return subprocess.run(["launchctl", *args], capture_output=True, text=True)


def stop_app():
    launchctl("bootout", "gui/%d/%s" % (os.getuid(), AGENT_LABEL))
    subprocess.run(["pkill", "-x", "HQNotch"], capture_output=True)


def start(login):
    stop_app()
    if login:
        os.makedirs(os.path.dirname(AGENT), exist_ok=True)
        with open(AGENT, "wb") as f:
            plistlib.dump(agent_plist(), f)
        r = launchctl("bootstrap", "gui/%d" % os.getuid(), AGENT)
        if r.returncode != 0:
            print("  launchctl bootstrap failed (%s); opening the app directly" % r.stderr.strip())
            subprocess.run(["open", APP])
    else:
        if os.path.exists(AGENT):
            os.remove(AGENT)
        subprocess.run(["open", APP])


def do_install(login):
    if sys.platform != "darwin":
        raise SystemExit("HQ Notch is macOS-only.")
    binary = build()
    print("installing", APP)
    bundle(binary)
    install_hook()
    if write_settings(True):
        print("  added HQ Notch hooks to", SETTINGS)
    start(login)
    print("done. HQ Notch is running%s. New Claude Code sessions report to it right away;"
          % (" and starts at login" if login else ""))
    print("sessions already open pick up the hooks when they next reload settings.")


def do_uninstall():
    stop_app()
    if os.path.exists(AGENT):
        os.remove(AGENT)
    if write_settings(False):
        print("  removed HQ Notch hooks from", SETTINGS)
    for p in (APP, SUPPORT, CACHE):
        if os.path.exists(p):
            shutil.rmtree(p)
            print("  removed", p)
    subprocess.run(["defaults", "delete", BUNDLE_ID], capture_output=True)
    subprocess.run(["defaults", "delete", "HQNotch"], capture_output=True)
    print("HQ Notch uninstalled.")


def do_status():
    try:
        with open(SETTINGS, encoding="utf-8") as f:
            s = json.load(f)
    except (OSError, ValueError):
        s = {}
    ours = sorted(ev for ev, gs in (s.get("hooks") or {}).items()
                  if any(is_ours(h) for g in gs if isinstance(g, dict) for h in g.get("hooks", [])))
    running = subprocess.run(["pgrep", "-x", "HQNotch"], capture_output=True).returncode == 0
    print("app:      ", "installed" if os.path.exists(APP) else "missing", "| running" if running else "| not running")
    print("hook:     ", "installed" if os.path.exists(HOOK_DST) else "missing")
    print("login:    ", "yes" if os.path.exists(AGENT) else "no")
    print("hooks in settings.json: %d/%d %s" % (len(ours), len(EVENTS), ",".join(ours)))


def main(argv):
    if "--uninstall" in argv:
        do_uninstall()
    elif "--status" in argv:
        do_status()
    else:
        do_install(login="--no-login" not in argv)


if __name__ == "__main__":
    main(sys.argv[1:])
