"""Evolution picks kept by the server (POST /api/evo-choice -> evo-choices.json ->
creature.evo in /api/sessions), so the notch and other browsers see them."""
import http.client
import json
import os
import shutil
import stat
import tempfile
import threading
import unittest
import uuid
from http.server import ThreadingHTTPServer

import dashboard


def sid_for(species, start=0):
    """A session id whose Pokemon is this species (species = sha256(sid) % 48)."""
    i = start
    while True:
        sid = str(uuid.UUID(int=i * 2654435761 % (1 << 128)))
        if dashboard._session_hash(sid) % 48 == species:
            return sid
        i += 1


EEVEE, ZARD, PIKA = sid_for(4), sid_for(1), sid_for(0)   # branch / two Megas / neither


class EvoTables(unittest.TestCase):
    def test_tables_come_from_the_page(self):
        finals, megas = dashboard.evo_tables()
        self.assertEqual(len(finals), 48)
        self.assertEqual(finals[4], {134, 135, 136, 196, 197, 470, 471, 700})   # Eevee's eight
        self.assertEqual(finals[1], {6})                                         # Charizard's line
        self.assertEqual(megas[6], {"charizard-megax", "charizard-megay"})

    def test_clean(self):
        c = dashboard.clean_evo_choice
        self.assertEqual(c(EEVEE, {"branchDex": 197}), {"branchDex": 197})
        self.assertEqual(c(EEVEE, {"branchDex": 25}), {})                 # not one of Eevee's
        self.assertEqual(c(EEVEE, {"branchDex": True}), {})               # a bool is not a dex
        self.assertEqual(c(ZARD, {"megaSlug": "charizard-megay"}), {"megaSlug": "charizard-megay"})
        self.assertEqual(c(ZARD, {"megaSlug": "gengar-mega"}), {})        # not Charizard's
        self.assertEqual(c(ZARD, {"branchDex": 6}), {})                   # no branch to pick
        self.assertEqual(c(PIKA, {"megaSlug": "charizard-megax"}), {})
        self.assertEqual(c("not-a-uuid", {"branchDex": 197}), {})
        self.assertEqual(c(EEVEE, "x"), {})


class EvoStore(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.mkdtemp(prefix="hqevo")
        self._saved = dashboard.EVO_PATH
        dashboard.EVO_PATH = os.path.join(self.dir, "evo-choices.json")

    def tearDown(self):
        dashboard.EVO_PATH = self._saved
        shutil.rmtree(self.dir, ignore_errors=True)

    def test_round_trip_and_clear(self):
        self.assertEqual(dashboard.save_evo_choice(EEVEE, {"branchDex": 197}), {"branchDex": 197})
        self.assertEqual(stat.S_IMODE(os.stat(dashboard.EVO_PATH).st_mode), 0o600)
        self.assertEqual(dashboard.load_evo_choices(), {EEVEE: {"branchDex": 197}})
        self.assertEqual(dashboard.save_evo_choice(EEVEE, {"branchDex": 700}), {"branchDex": 700})
        self.assertEqual(dashboard.save_evo_choice(EEVEE, {"branchDex": None}), {})
        self.assertEqual(dashboard.load_evo_choices(), {})

    def test_a_bad_pick_writes_nothing(self):
        dashboard.save_evo_choice(ZARD, {"megaSlug": "charizard-megax"})
        for sid, patch in ((ZARD, {"megaSlug": "nope"}), (EEVEE, {"branchDex": 1}), ("x", {"branchDex": 197})):
            with self.assertRaises(ValueError):
                dashboard.save_evo_choice(sid, patch)
        self.assertEqual(dashboard.load_evo_choices(), {ZARD: {"megaSlug": "charizard-megax"}})

    def test_a_tampered_file_is_cleaned_on_read(self):
        with open(dashboard.EVO_PATH, "w") as f:
            json.dump({EEVEE: {"branchDex": 25}, ZARD: {"megaSlug": "charizard-megay", "x": 1}, "junk": {}}, f)
        self.assertEqual(dashboard.load_evo_choices(), {ZARD: {"megaSlug": "charizard-megay"}})

    def test_payload_carries_it_only_where_picked(self):
        sessions = [{"sessionId": EEVEE, "creature": {"species": 4}}, {"sessionId": PIKA, "creature": {"species": 0}}]
        dashboard.merge_evo_choices(sessions, {EEVEE: {"branchDex": 197}})
        self.assertEqual(sessions[0]["creature"]["evo"], {"branchDex": 197})
        self.assertNotIn("evo", sessions[1]["creature"])


class EvoEndpoint(unittest.TestCase):
    """The real Handler on an ephemeral 127.0.0.1 port."""

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

    def setUp(self):
        self.dir = tempfile.mkdtemp(prefix="hqevo")
        self._saved = dashboard.EVO_PATH
        dashboard.EVO_PATH = os.path.join(self.dir, "evo-choices.json")

    def tearDown(self):
        dashboard.EVO_PATH = self._saved
        shutil.rmtree(self.dir, ignore_errors=True)

    def post(self, body, token=True, origin=None):
        data = json.dumps(body).encode()
        conn = http.client.HTTPConnection("127.0.0.1", self.port, timeout=10)
        try:
            h = {"Content-Type": "application/json", "Content-Length": str(len(data))}
            if token:
                h["X-HQ-Token"] = dashboard.CSRF_TOKEN
            if origin:
                h["Origin"] = origin
            conn.request("POST", "/api/evo-choice", body=data, headers=h)
            r = conn.getresponse()
            return r.status, json.loads(r.read().decode() or "{}")
        finally:
            conn.close()

    def test_saves_a_valid_pick(self):
        code, resp = self.post({"sessionId": EEVEE, "branchDex": 470})
        self.assertEqual((code, resp), (200, {"sessionId": EEVEE, "evo": {"branchDex": 470}}))
        self.assertEqual(dashboard.load_evo_choices(), {EEVEE: {"branchDex": 470}})

    def test_rejects_bad_ids_and_picks(self):
        for body in ({"sessionId": "../../etc", "branchDex": 470}, {"sessionId": EEVEE, "branchDex": 6},
                     {"sessionId": ZARD, "megaSlug": "<script>"}):
            code, _ = self.post(body)
            self.assertEqual(code, 400, body)
        self.assertFalse(os.path.exists(dashboard.EVO_PATH))

    def test_needs_the_csrf_token_and_a_same_site_origin(self):
        self.assertEqual(self.post({"sessionId": EEVEE, "branchDex": 470}, token=False)[0], 403)
        self.assertEqual(self.post({"sessionId": EEVEE, "branchDex": 470}, origin="https://evil.example")[0], 403)
        self.assertFalse(os.path.exists(dashboard.EVO_PATH))


class PageWiring(unittest.TestCase):
    def test_page_posts_and_seeds(self):
        here = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
        with open(os.path.join(here, "ui", "app", "12-pokedex.js"), encoding="utf-8") as f:
            dex = f.read()
        with open(os.path.join(here, "ui", "app", "09-drawer.js"), encoding="utf-8") as f:
            drawer = f.read()
        self.assertIn("evoChoicePost(sid, patch);", dex)
        self.assertIn('fetch("/api/evo-choice",{method:"POST"', dex)
        self.assertIn("evoSeedFromServer(STATE.sessions);", drawer)
        self.assertIn("/api/evo-choice", dashboard.POST_PATHS)


if __name__ == "__main__":
    unittest.main()
