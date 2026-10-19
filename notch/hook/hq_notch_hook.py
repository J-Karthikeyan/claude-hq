#!/usr/bin/env python3
"""HQ Notch hook relay: Claude Code hook JSON on stdin -> HQ Notch app over a Unix socket.

Stdlib only. It never blocks Claude Code and never fails it:
  * every event except PermissionRequest is fire-and-forget (0.3 s connect/send budget);
  * PermissionRequest waits for the notch's decision (allow / always / deny / answer)
    for at most WAIT_SECS, then prints NOTHING, so Claude Code asks in the terminal
    exactly as it would without the notch;
  * `--ask` (installed as a PreToolUse hook matching AskUserQuestion) waits the same way
    for answers to Claude's questions, so they can be answered before any permission step;
  * any error (app not running, bad JSON, socket gone) prints nothing and exits 0.

Privacy: the socket is a local file in ~/Library/Application Support/HQNotch (mode 0600
by the app). Nothing here touches the network.
"""
import json
import os
import socket
import sys

SOCKET_PATH = os.path.expanduser("~/Library/Application Support/HQNotch/hq.sock")
PROTOCOL = 1
WAIT_SECS = 115          # Claude Code gives the hook 120 s (see the installer); stay under it
FIRE_SECS = 0.3
STR_MAX = 2000           # cap each string sent for a permission card
PLAN_MAX = 12000         # ...except a plan waiting for approval (ExitPlanMode), which is read in full
DIFF_MAX = 4000          # each side of an edit sent for the notch's diff view
DIFF_EDITS = 8           # at most this many edits of a MultiEdit
EDIT_TOOLS = ("Edit", "Write", "MultiEdit", "NotebookEdit")
# Environment the app uses to find (and focus) the session's terminal window.
ENV_KEYS = ("TERM_PROGRAM", "KITTY_WINDOW_ID", "KITTY_LISTEN_ON", "ITERM_SESSION_ID",
            "TERM_SESSION_ID", "TMUX", "TMUX_PANE", "__CFBundleIdentifier")


def _clip(v, limit):
    return v if len(v) <= limit else v[: limit - 1] + "…"


def _cap(v, depth=0):
    """Shrink a tool_input for display: long strings trimmed, deep structures cut."""
    if isinstance(v, str):
        return _clip(v, STR_MAX)
    if depth > 4:
        return None
    if isinstance(v, list):
        return [_cap(x, depth + 1) for x in v[:50]]
    if isinstance(v, dict):
        return {str(k): _cap(x, depth + 1) for k, x in list(v.items())[:50]}
    return v


def _lines(s):
    if not isinstance(s, str) or not s:
        return 0
    return s.count("\n") + (0 if s.endswith("\n") else 1)


def edit_counts(tool, ti):
    """(+added, -removed) line counts for an edit tool's input, or None."""
    ti = ti if isinstance(ti, dict) else {}
    if tool == "Edit":
        return _lines(ti.get("new_string")), _lines(ti.get("old_string"))
    if tool == "Write":
        return _lines(ti.get("content")), 0
    if tool == "MultiEdit":
        plus = minus = 0
        for e in ti.get("edits") or []:
            if isinstance(e, dict):
                plus += _lines(e.get("new_string"))
                minus += _lines(e.get("old_string"))
        return plus, minus
    if tool == "NotebookEdit":
        return _lines(ti.get("new_source")), 0
    return None


def edit_diff(tool, ti):
    """The change an edit tool made, for the notch's diff view: {"edits": [{"old", "new"}],
    "truncated"}. Each side capped at DIFF_MAX, at most DIFF_EDITS edits. None for other tools.
    Like everything here it goes only to the local socket."""
    ti = ti if isinstance(ti, dict) else {}

    def text(v):
        return v if isinstance(v, str) else ""
    if tool == "Edit":
        pairs = [(text(ti.get("old_string")), text(ti.get("new_string")))]
    elif tool == "MultiEdit":
        pairs = [(text(e.get("old_string")), text(e.get("new_string")))
                 for e in (ti.get("edits") or []) if isinstance(e, dict)]
    elif tool == "Write":
        pairs = [("", text(ti.get("content")))]
    elif tool == "NotebookEdit":
        pairs = [("", text(ti.get("new_source")))]
    else:
        return None
    cut = len(pairs) > DIFF_EDITS or any(len(o) > DIFF_MAX or len(n) > DIFF_MAX for o, n in pairs)
    return {"edits": [{"old": _clip(o, DIFF_MAX), "new": _clip(n, DIFF_MAX)} for o, n in pairs[:DIFF_EDITS]],
            "truncated": cut}


def tool_summary(tool, ti):
    """A short one-line label for a tool call: what the notch ticker shows."""
    ti = ti if isinstance(ti, dict) else {}
    target = ""
    for key in ("file_path", "notebook_path", "path", "command", "pattern", "url", "query",
                "description", "skill", "prompt"):
        val = ti.get(key)
        if isinstance(val, str) and val.strip():
            target = val.strip()
            break
    if key in ("file_path", "notebook_path", "path") and target:
        target = os.path.basename(target.rstrip("/")) or target
    target = " ".join(target.split())
    if len(target) > 120:
        target = target[:119] + "…"
    return {"tool": str(tool or ""), "target": target}


def build_message(payload, argv):
    """The compact message the app receives. Pure (testable)."""
    event = str(payload.get("hook_event_name") or "")
    tool = payload.get("tool_name") or ""
    ti = payload.get("tool_input")
    msg = {
        "v": PROTOCOL,
        "event": event,
        "sessionId": str(payload.get("session_id") or ""),
        "cwd": str(payload.get("cwd") or ""),
        "transcriptPath": str(payload.get("transcript_path") or ""),
        "ppid": os.getppid(),
        "env": {k: os.environ[k] for k in ENV_KEYS if os.environ.get(k)},
    }
    if tool:
        msg["tool"] = tool_summary(tool, ti)
        msg["toolUseId"] = str(payload.get("tool_use_id") or "")
    if event in ("PostToolUse",) and tool in EDIT_TOOLS:
        counts = edit_counts(tool, ti)
        if counts:
            msg["plus"], msg["minus"] = counts
        diff = edit_diff(tool, ti)
        if diff:
            msg["diff"] = diff
    if event == "PostToolUseFailure":
        msg["error"] = _cap(str(payload.get("error") or ""))[:300]
    if event == "UserPromptSubmit":
        msg["prompt"] = _cap(str(payload.get("prompt") or ""))[:300]
    if event == "Notification":
        msg["message"] = str(payload.get("message") or "")[:300]
        msg["notificationType"] = str(payload.get("notification_type") or "")
    if event in ("Stop", "StopFailure", "SubagentStop"):
        msg["error"] = str(payload.get("error") or "")[:300]
    if event == "SessionStart":
        msg["source"] = str(payload.get("source") or "")
    if event == "PermissionRequest" or is_ask(payload, argv):
        msg["toolInput"] = _cap(ti if isinstance(ti, (dict, list)) else {})
        if tool == "ExitPlanMode" and isinstance(ti, dict) and isinstance(ti.get("plan"), str):
            msg["toolInput"]["plan"] = _clip(ti["plan"], PLAN_MAX)   # the plan card shows all of it
        sugg = payload.get("permission_suggestions")
        msg["canAlways"] = bool(event == "PermissionRequest" and isinstance(sugg, list) and sugg)
        msg["question"] = tool == "AskUserQuestion"
    return msg


def is_ask(payload, argv):
    """The `--ask` hook: PreToolUse for AskUserQuestion, answered from the notch."""
    return ("--ask" in argv and payload.get("hook_event_name") == "PreToolUse"
            and payload.get("tool_name") == "AskUserQuestion")


def answers_fit(tool_input, answers):
    """True when `answers` answers exactly the questions asked: one entry per question
    text; single-select = one of its option labels, multi-select = a non-empty list of
    distinct labels. Anything else is rejected (the terminal then asks)."""
    if not isinstance(tool_input, dict) or not isinstance(answers, dict):
        return False
    items = tool_input.get("questions")
    if not isinstance(items, list) or not items or len(items) != len(answers):
        return False
    for item in items:
        if not isinstance(item, dict):
            return False
        text = item.get("question")
        labels = [o.get("label") for o in (item.get("options") or []) if isinstance(o, dict)]
        pick = answers.get(text) if isinstance(text, str) else None
        if item.get("multiSelect"):
            if (not isinstance(pick, list) or not pick or len(set(map(str, pick))) != len(pick)
                    or not all(isinstance(p, str) and p in labels for p in pick)):
                return False
        elif not (isinstance(pick, str) and pick in labels):
            return False
    return True


def decision_output(reply, payload):
    """Translate the app's reply into Claude Code's documented PermissionRequest output.
    Returns a dict to print, or None to print nothing (= ask in the terminal)."""
    if not isinstance(reply, dict):
        return None
    decision = reply.get("decision")
    if payload.get("hook_event_name") == "PreToolUse":
        # --ask: only an answer is meaningful here. Anything else prints nothing, and the
        # question goes on to Claude Code's normal flow (terminal, or PermissionRequest).
        ti = payload.get("tool_input")
        answers = reply.get("answers")
        if decision != "answer" or payload.get("tool_name") != "AskUserQuestion" or not answers_fit(ti, answers):
            return None
        updated = dict(ti)
        updated["answers"] = answers
        return {"hookSpecificOutput": {"hookEventName": "PreToolUse", "permissionDecision": "allow",
                                       "updatedInput": updated}}
    if decision == "answer":
        ti = payload.get("tool_input")
        answers = reply.get("answers")
        if payload.get("tool_name") != "AskUserQuestion" or not answers_fit(ti, answers):
            return None
        updated = dict(ti)
        updated["answers"] = answers
        return {"hookSpecificOutput": {"hookEventName": "PermissionRequest",
                                       "decision": {"behavior": "allow", "updatedInput": updated}}}
    if decision == "allow":
        return {"hookSpecificOutput": {"hookEventName": "PermissionRequest",
                                       "decision": {"behavior": "allow"}}}
    if decision == "always":
        d = {"behavior": "allow"}
        sugg = payload.get("permission_suggestions")
        if isinstance(sugg, list) and sugg:
            d["updatedPermissions"] = sugg
        return {"hookSpecificOutput": {"hookEventName": "PermissionRequest", "decision": d}}
    if decision == "deny":
        message = reply.get("message")
        message = message if isinstance(message, str) and message.strip() else "Denied from HQ Notch"
        return {"hookSpecificOutput": {"hookEventName": "PermissionRequest",
                                       "decision": {"behavior": "deny", "message": message[:500]}}}
    return None   # "pass", unknown, or missing: let the terminal ask


def _connect(timeout):
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.settimeout(timeout)
    s.connect(SOCKET_PATH)
    return s


def _read_line(s):
    buf = b""
    while b"\n" not in buf:
        chunk = s.recv(65536)
        if not chunk:
            break
        buf += chunk
        if len(buf) > 1 << 20:
            return None
    line = buf.split(b"\n", 1)[0]
    return json.loads(line.decode("utf-8")) if line else None


def main(argv):
    try:
        payload = json.loads(sys.stdin.read() or "{}")
    except Exception:
        return 0
    if not isinstance(payload, dict) or not os.path.exists(SOCKET_PATH):
        return 0
    msg = build_message(payload, argv)
    blocking = msg["event"] == "PermissionRequest" or is_ask(payload, argv)
    msg["wantsReply"] = blocking
    data = (json.dumps(msg, separators=(",", ":")) + "\n").encode("utf-8")
    try:
        s = _connect(FIRE_SECS)
        try:
            s.sendall(data)
            if not blocking:
                return 0
            s.settimeout(WAIT_SECS)
            reply = _read_line(s)
        finally:
            s.close()
    except Exception:
        return 0
    out = decision_output(reply, payload)
    if out is not None:
        sys.stdout.write(json.dumps(out) + "\n")
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main(sys.argv[1:]))
    except Exception:
        sys.exit(0)
