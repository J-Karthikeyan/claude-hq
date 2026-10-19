"""HQ Notch from HQ (ext_notch.py + ui/app/37-notch-invite.js): status, the one-click install
(CSRF-protected, macOS only, never twice at once), and the page's invite wiring. The real
installer is never run: INSTALLER is pointed at a stand-in script."""
import http.client
import json
import os
import shutil
import sys
import tempfile
import threading
import time
import unittest
from http.server import ThreadingHTTPServer
from unittest import mock

import dashboard
import ext_notch

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


class Status(unittest.TestCase):
    def test_shape(self):
        code, s = ext_notch.status(None)
        self.assertEqual(code, 200)
        self.assertEqual(sorted(s), ["available", "installed", "job", "mac", "running", "tools"])
        self.assertTrue(s["available"])                          # notch/tools/install.py ships with HQ
        self.assertIn(s["job"]["state"], ("idle", "running", "done", "failed"))

    def test_not_a_mac(self):
        with mock.patch.object(sys, "platform", "linux"):
            self.assertEqual(ext_notch.install({})[0], 400)
            self.assertFalse(ext_notch.status(None)[1]["installed"])

    def test_needs_the_command_line_tools(self):
        with mock.patch.object(sys, "platform", "darwin"), mock.patch.object(shutil, "which", return_value=None):
            code, resp = ext_notch.install({})
        self.assertEqual(code, 400)
        self.assertIn("xcode-select --install", resp["error"])


class Install(unittest.TestCase):
    """The install runs the installer once, in the background, and reports how it went."""

    def setUp(self):
        self.dir = tempfile.mkdtemp(prefix="hqni")
        self.fake = os.path.join(self.dir, "install.py")
        self._inst = ext_notch.INSTALLER
        ext_notch.INSTALLER = self.fake
        ext_notch._job.update(state="idle", started=None, rc=None, log="")

    def tearDown(self):
        ext_notch.INSTALLER = self._inst
        ext_notch._job.update(state="idle", started=None, rc=None, log="")
        shutil.rmtree(self.dir, ignore_errors=True)

    def write(self, body):
        with open(self.fake, "w") as f:
            f.write(body)

    def wait(self):
        for _ in range(100):
            if ext_notch._job["state"] != "running":
                return ext_notch._job_view()
            time.sleep(0.05)
        self.fail("installer never finished")

    def run_it(self):
        with mock.patch.object(sys, "platform", "darwin"), mock.patch.object(shutil, "which", return_value="/usr/bin/swift"):
            return ext_notch.install({})

    def test_success(self):
        self.write("print('done. HQ Notch is running')\n")
        self.assertEqual(self.run_it(), (202, {"started": True}))
        j = self.wait()
        self.assertEqual((j["state"], j["rc"]), ("done", 0))
        self.assertIn("HQ Notch is running", j["log"])

    def test_failure_is_reported(self):
        self.write("import sys; print('swift build failed'); sys.exit(3)\n")
        self.run_it()
        j = self.wait()
        self.assertEqual((j["state"], j["rc"]), ("failed", 3))

    def test_never_twice_at_once(self):
        self.write("import time; time.sleep(1)\n")
        self.assertEqual(self.run_it()[0], 202)
        self.assertEqual(self.run_it()[0], 409)
        self.wait()

    def test_log_is_capped(self):
        self.write("print('x' * 50000)\n")
        self.run_it()
        self.assertLessEqual(len(self.wait()["log"]), ext_notch.LOG_MAX)


class Endpoint(unittest.TestCase):
    """The real Handler: the install is a protected POST."""

    @classmethod
    def setUpClass(cls):
        cls.server = ThreadingHTTPServer(("127.0.0.1", 0), dashboard.Handler)
        cls.port = cls.server.server_address[1]
        cls.thread = threading.Thread(target=cls.server.serve_forever, daemon=True)
        cls.thread.start()

    @classmethod
    def tearDownClass(cls):
        cls.server.shutdown()
        cls.server.server_close()
        cls.thread.join(5)

    def req(self, method, path, token=True):
        conn = http.client.HTTPConnection("127.0.0.1", self.port, timeout=10)
        try:
            h = {"Content-Type": "application/json", "Content-Length": "2"}
            if token:
                h["X-HQ-Token"] = dashboard.CSRF_TOKEN
            conn.request(method, path, body=b"{}" if method == "POST" else None, headers=h)
            r = conn.getresponse()
            return r.status, json.loads(r.read().decode() or "{}")
        finally:
            conn.close()

    def test_status_is_served(self):
        code, s = self.req("GET", "/api/notch/status")
        self.assertEqual(code, 200)
        self.assertIn("installed", s)

    def test_install_needs_the_token(self):
        with mock.patch.object(ext_notch, "install", return_value=(202, {"started": True})) as inst:
            code, _ = self.req("POST", "/api/notch/install", token=False)
            self.assertEqual(code, 403)
            inst.assert_not_called()
            code, resp = self.req("POST", "/api/notch/install")
            self.assertEqual((code, resp), (202, {"started": True}))
            inst.assert_called_once()


class Page(unittest.TestCase):
    def test_wiring(self):
        html = dashboard.assemble_index()
        self.assertIn("function notchInviteCheck(", html)
        with open(os.path.join(ROOT, "ui", "app", "37-notch-invite.js"), encoding="utf-8") as f:
            js = f.read()
        self.assertIn('if(!s || !s.mac || !s.available) return;', js)            # only on a Mac, only with the notch shipped
        self.assertIn('if(s.installed){ notchInviteRemember("installed"); return; }', js)
        self.assertIn('if(typeof currentModal === "function" && currentModal())', js)   # never over another dialog
        self.assertIn('"X-HQ-Token": CSRF', js)
        self.assertNotIn("innerHTML", js)
        self.assertIn("function openNotchInvite(", js)                           # reopen by hand any time
        self.assertIn('id="setNotchOpen"', html)                                 # ...from Settings
        self.assertIn('label:"HQ Notch"', html)                                   # ...and the command palette
        self.assertNotIn("function el(", js)                                    # 00-core.js owns el()
        with open(os.path.join(ROOT, "ui", "app", "09-drawer.js"), encoding="utf-8") as f:
            drawer = f.read()
        self.assertIn('$("notchInviteBack").classList.contains("open")', drawer)  # focus stays in it


if __name__ == "__main__":
    unittest.main()
