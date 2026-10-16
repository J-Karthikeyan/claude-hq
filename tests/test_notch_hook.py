"""HQ Notch hook relay (notch/hook/hq_notch_hook.py): message shaping, decision
translation, and the never-block / fail-open guarantees, over a real Unix socket."""
import importlib.util
import json
import os
import socket
import subprocess
import sys
import tempfile
import threading
import time
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
HOOK = os.path.join(os.path.dirname(HERE), "notch", "hook", "hq_notch_hook.py")

spec = importlib.util.spec_from_file_location("hq_notch_hook", HOOK)
hook = importlib.util.module_from_spec(spec)
spec.loader.exec_module(hook)

QUESTION = {"questions": [
    {"question": "Which db?", "header": "DB", "multiSelect": False,
     "options": [{"label": "Postgres"}, {"label": "SQLite"}]},
    {"question": "Extras?", "multiSelect": True, "options": [{"label": "Auth"}, {"label": "Cache"}]},
]}


class MessageTests(unittest.TestCase):
    def test_edit_counts(self):
        self.assertEqual(hook.edit_counts("Edit", {"old_string": "a\nb", "new_string": "a\nb\nc\n"}), (3, 2))
        self.assertEqual(hook.edit_counts("Write", {"content": "x\ny"}), (2, 0))
        self.assertEqual(hook.edit_counts("MultiEdit", {"edits": [{"old_string": "a", "new_string": "b\nc"},
                                                                   "junk"]}), (2, 1))
        self.assertIsNone(hook.edit_counts("Bash", {"command": "ls"}))
        self.assertEqual(hook.edit_counts("Edit", None), (0, 0))

    def test_summary_uses_basename_and_trims(self):
        s = hook.tool_summary("Read", {"file_path": "/Users/me/secret/project/app.py"})
        self.assertEqual(s, {"tool": "Read", "target": "app.py"})
        long = hook.tool_summary("Bash", {"command": "echo " + "x" * 400})
        self.assertLessEqual(len(long["target"]), 120)
        self.assertEqual(hook.tool_summary("Task", {})["target"], "")

    def test_permission_message_carries_input_and_always(self):
        m = hook.build_message({"hook_event_name": "PermissionRequest", "session_id": "s", "cwd": "/p",
                                "tool_name": "Bash", "tool_input": {"command": "x" * 5000},
                                "permission_suggestions": [{"type": "addRules"}]}, [])
        self.assertEqual(m["event"], "PermissionRequest")
        self.assertTrue(m["canAlways"])
        self.assertFalse(m["question"])
        self.assertLessEqual(len(m["toolInput"]["command"]), hook.STR_MAX)

    def test_post_tool_use_counts_only_for_edits(self):
        m = hook.build_message({"hook_event_name": "PostToolUse", "session_id": "s", "tool_name": "Edit",
                                "tool_input": {"old_string": "a", "new_string": "b\nc"}}, [])
        self.assertEqual((m["plus"], m["minus"]), (2, 1))
        m = hook.build_message({"hook_event_name": "PostToolUse", "session_id": "s", "tool_name": "Bash",
                                "tool_input": {"command": "ls"}}, [])
        self.assertNotIn("plus", m)
        self.assertNotIn("toolInput", m)   # only permission cards carry the full input


class PlanAndDiffTests(unittest.TestCase):
    def test_plan_is_sent_nearly_whole(self):
        plan = "# Plan\n" + "step\n" * 3000                  # ~15 000 chars
        m = hook.build_message({"hook_event_name": "PermissionRequest", "session_id": "s", "tool_name": "ExitPlanMode",
                                "tool_input": {"plan": plan}}, [])
        self.assertEqual(len(m["toolInput"]["plan"]), hook.PLAN_MAX)
        self.assertTrue(m["toolInput"]["plan"].startswith("# Plan\nstep"))
        self.assertTrue(m["toolInput"]["plan"].endswith("…"))
        # other tools keep the normal cap
        m = hook.build_message({"hook_event_name": "PermissionRequest", "session_id": "s", "tool_name": "Bash",
                                "tool_input": {"command": "x" * 5000, "plan": "y" * 5000}}, [])
        self.assertEqual(len(m["toolInput"]["plan"]), hook.STR_MAX)

    def test_edit_diff_payload(self):
        m = hook.build_message({"hook_event_name": "PostToolUse", "session_id": "s", "tool_name": "Edit",
                                "tool_input": {"file_path": "/a/b.py", "old_string": "a\nb", "new_string": "a\nc"}}, [])
        self.assertEqual(m["diff"], {"edits": [{"old": "a\nb", "new": "a\nc"}], "truncated": False})
        m = hook.build_message({"hook_event_name": "PostToolUse", "session_id": "s", "tool_name": "Write",
                                "tool_input": {"file_path": "/a/c.md", "content": "hello"}}, [])
        self.assertEqual(m["diff"]["edits"], [{"old": "", "new": "hello"}])
        m = hook.build_message({"hook_event_name": "PostToolUse", "session_id": "s", "tool_name": "Read",
                                "tool_input": {"file_path": "/a/b.py"}}, [])
        self.assertNotIn("diff", m)
        # only after the tool ran: a PreToolUse carries no diff
        m = hook.build_message({"hook_event_name": "PreToolUse", "session_id": "s", "tool_name": "Edit",
                                "tool_input": {"old_string": "a", "new_string": "b"}}, [])
        self.assertNotIn("diff", m)

    def test_edit_diff_is_capped(self):
        d = hook.edit_diff("Edit", {"old_string": "o" * 9000, "new_string": "n"})
        self.assertEqual(len(d["edits"][0]["old"]), hook.DIFF_MAX)
        self.assertTrue(d["truncated"])
        edits = [{"old_string": str(i), "new_string": str(i + 1)} for i in range(20)] + ["junk"]
        d = hook.edit_diff("MultiEdit", {"edits": edits})
        self.assertEqual(len(d["edits"]), hook.DIFF_EDITS)
        self.assertEqual(d["edits"][0], {"old": "0", "new": "1"})
        self.assertTrue(d["truncated"])
        self.assertFalse(hook.edit_diff("MultiEdit", {"edits": edits[:3]})["truncated"])
        self.assertEqual(hook.edit_diff("Edit", None), {"edits": [{"old": "", "new": ""}], "truncated": False})
        self.assertIsNone(hook.edit_diff("Bash", {"command": "ls"}))


class DecisionTests(unittest.TestCase):
    def out(self, reply, payload=None):
        return hook.decision_output(reply, payload or {"tool_name": "Bash"})

    def test_allow_deny(self):
        self.assertEqual(self.out({"decision": "allow"})["hookSpecificOutput"]["decision"], {"behavior": "allow"})
        d = self.out({"decision": "deny", "message": "no"})["hookSpecificOutput"]["decision"]
        self.assertEqual(d, {"behavior": "deny", "message": "no"})
        self.assertEqual(self.out({"decision": "deny"})["hookSpecificOutput"]["decision"]["message"],
                         "Denied from HQ Notch")

    def test_always_forwards_suggestions(self):
        sugg = [{"type": "addRules", "rules": [{"toolName": "Bash"}], "behavior": "allow", "destination": "localSettings"}]
        d = self.out({"decision": "always"}, {"tool_name": "Bash", "permission_suggestions": sugg})
        self.assertEqual(d["hookSpecificOutput"]["decision"], {"behavior": "allow", "updatedPermissions": sugg})
        d = self.out({"decision": "always"})
        self.assertEqual(d["hookSpecificOutput"]["decision"], {"behavior": "allow"})

    def test_pass_and_garbage_print_nothing(self):
        for reply in ({"decision": "pass"}, {"decision": "maybe"}, {}, None, "allow", []):
            self.assertIsNone(self.out(reply), reply)

    def test_answers(self):
        payload = {"tool_name": "AskUserQuestion", "tool_input": QUESTION}
        good = {"Which db?": "SQLite", "Extras?": ["Auth", "Cache"]}
        d = hook.decision_output({"decision": "answer", "answers": good}, payload)
        upd = d["hookSpecificOutput"]["decision"]["updatedInput"]
        self.assertEqual(upd["answers"], good)
        self.assertEqual(upd["questions"], QUESTION["questions"])
        self.assertNotIn("answers", QUESTION)   # input not mutated

    def test_bad_answers_rejected(self):
        payload = {"tool_name": "AskUserQuestion", "tool_input": QUESTION}
        for bad in ({"Which db?": "Mongo", "Extras?": ["Auth"]},     # unknown label
                    {"Which db?": "SQLite"},                          # missing question
                    {"Which db?": ["SQLite"], "Extras?": ["Auth"]},   # list for single
                    {"Which db?": "SQLite", "Extras?": "Auth"},       # string for multi
                    {"Which db?": "SQLite", "Extras?": []},           # empty multi
                    {"Which db?": "SQLite", "Extras?": ["Auth", "Auth"]}):
            self.assertIsNone(hook.decision_output({"decision": "answer", "answers": bad}, payload), bad)
        # answers are only accepted for AskUserQuestion
        self.assertIsNone(hook.decision_output({"decision": "answer", "answers": {}}, {"tool_name": "Bash"}))


class AskHookTests(unittest.TestCase):
    """--ask: the PreToolUse hook for AskUserQuestion."""
    PAYLOAD = {"hook_event_name": "PreToolUse", "session_id": "s", "tool_name": "AskUserQuestion",
               "tool_input": QUESTION}

    def test_only_blocks_for_ask_flag_on_ask_tool(self):
        self.assertTrue(hook.is_ask(self.PAYLOAD, ["--ask"]))
        self.assertFalse(hook.is_ask(self.PAYLOAD, []))                       # generic PreToolUse hook
        self.assertFalse(hook.is_ask(dict(self.PAYLOAD, tool_name="Bash"), ["--ask"]))
        self.assertFalse(hook.is_ask(dict(self.PAYLOAD, hook_event_name="PostToolUse"), ["--ask"]))

    def test_message_carries_questions(self):
        m = hook.build_message(self.PAYLOAD, ["--ask"])
        self.assertTrue(m["question"])
        self.assertFalse(m["canAlways"])
        self.assertEqual(m["toolInput"]["questions"][0]["question"], "Which db?")
        self.assertNotIn("toolInput", hook.build_message(self.PAYLOAD, []))  # fire-and-forget copy

    def test_answer_is_pretooluse_allow_with_updated_input(self):
        good = {"Which db?": "Postgres", "Extras?": ["Cache"]}
        out = hook.decision_output({"decision": "answer", "answers": good}, self.PAYLOAD)
        h = out["hookSpecificOutput"]
        self.assertEqual((h["hookEventName"], h["permissionDecision"]), ("PreToolUse", "allow"))
        self.assertEqual(h["updatedInput"]["answers"], good)
        self.assertEqual(h["updatedInput"]["questions"], QUESTION["questions"])

    def test_anything_but_a_valid_answer_prints_nothing(self):
        for reply in ({"decision": "pass"}, {"decision": "allow"}, {"decision": "deny"},
                      {"decision": "answer", "answers": {"Which db?": "Mongo", "Extras?": ["Cache"]}}, None):
            self.assertIsNone(hook.decision_output(reply, self.PAYLOAD), reply)


class SocketTests(unittest.TestCase):
    """Run the hook as Claude Code does (subprocess, JSON on stdin) against a fake app."""

    def setUp(self):
        self.tmp = tempfile.mkdtemp(prefix="hqn")
        self.sock_path = os.path.join(self.tmp, "hq.sock")
        self.received = []
        self.reply = None
        self.delay = 0

    def tearDown(self):
        try:
            os.unlink(self.sock_path)
        except OSError:
            pass
        os.rmdir(self.tmp)

    def serve(self, count=1):
        srv = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        srv.bind(self.sock_path)
        srv.listen(4)

        def loop():
            for _ in range(count):
                c, _ = srv.accept()
                buf = b""
                while b"\n" not in buf:
                    chunk = c.recv(65536)
                    if not chunk:
                        break
                    buf += chunk
                self.received.append(json.loads(buf.split(b"\n")[0]))
                if self.received[-1].get("wantsReply"):
                    time.sleep(self.delay)
                    if self.reply is not None:
                        c.sendall((json.dumps(self.reply) + "\n").encode())
                c.close()
            srv.close()
        t = threading.Thread(target=loop, daemon=True)
        t.start()
        return t

    def run_hook(self, payload, timeout=10, argv=()):
        # Point the module at the temp socket by patching before main runs.
        script = ("import importlib.util,sys\n"
                  "s=importlib.util.spec_from_file_location('h',%r);h=importlib.util.module_from_spec(s);"
                  "s.loader.exec_module(h);h.SOCKET_PATH=%r;h.WAIT_SECS=3\n"
                  "sys.exit(h.main(%r))\n" % (HOOK, self.sock_path, list(argv)))
        start = time.time()
        p = subprocess.run([sys.executable, "-c", script], input=json.dumps(payload), capture_output=True,
                           text=True, timeout=timeout)
        return p, time.time() - start

    def test_no_app_is_silent_and_fast(self):
        p, secs = self.run_hook({"hook_event_name": "PermissionRequest", "session_id": "s", "tool_name": "Bash"})
        self.assertEqual((p.returncode, p.stdout), (0, ""))
        self.assertLess(secs, 2)

    def test_bad_stdin_is_silent(self):
        p = subprocess.run([sys.executable, HOOK], input="not json", capture_output=True, text=True, timeout=10)
        self.assertEqual((p.returncode, p.stdout), (0, ""))

    def test_fire_and_forget(self):
        t = self.serve()
        p, _ = self.run_hook({"hook_event_name": "PostToolUse", "session_id": "s1", "tool_name": "Edit",
                              "tool_input": {"file_path": "/a/b.py", "old_string": "x", "new_string": "y"}})
        t.join(3)
        self.assertEqual((p.returncode, p.stdout), (0, ""))
        m = self.received[0]
        self.assertEqual((m["event"], m["sessionId"], m["tool"]["target"], m["wantsReply"]),
                         ("PostToolUse", "s1", "b.py", False))

    def test_permission_round_trip(self):
        self.reply = {"decision": "allow"}
        t = self.serve()
        p, _ = self.run_hook({"hook_event_name": "PermissionRequest", "session_id": "s1", "tool_name": "Bash",
                              "tool_input": {"command": "ls"}})
        t.join(3)
        self.assertTrue(self.received[0]["wantsReply"])
        out = json.loads(p.stdout)
        self.assertEqual(out["hookSpecificOutput"]["decision"], {"behavior": "allow"})

    def test_ask_round_trip(self):
        self.reply = {"decision": "answer", "answers": {"Which db?": "SQLite", "Extras?": ["Auth"]}}
        t = self.serve()
        p, _ = self.run_hook(dict(AskHookTests.PAYLOAD), argv=["--ask"])
        t.join(3)
        self.assertTrue(self.received[0]["wantsReply"])
        self.assertTrue(self.received[0]["question"])
        h = json.loads(p.stdout)["hookSpecificOutput"]
        self.assertEqual(h["permissionDecision"], "allow")
        self.assertEqual(h["updatedInput"]["answers"]["Which db?"], "SQLite")

    def test_generic_pretooluse_never_waits(self):
        self.reply = {"decision": "answer", "answers": {}}
        self.delay = 4
        t = self.serve()
        p, secs = self.run_hook(dict(AskHookTests.PAYLOAD))      # no --ask
        t.join(3)
        self.assertEqual((p.returncode, p.stdout), (0, ""))
        self.assertFalse(self.received[0]["wantsReply"])
        self.assertLess(secs, 2)

    def test_app_never_answers_falls_back_to_terminal(self):
        self.reply = None
        self.delay = 4          # longer than the patched WAIT_SECS=3
        self.serve()
        p, secs = self.run_hook({"hook_event_name": "PermissionRequest", "session_id": "s1", "tool_name": "Bash"})
        self.assertEqual((p.returncode, p.stdout), (0, ""))
        self.assertLess(secs, 4.5)


if __name__ == "__main__":
    unittest.main()
