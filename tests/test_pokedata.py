"""Vendored Pokemon battle data (games/pokedata.js + backend/data/pokemon.json):
the two copies match, every creature the app can show has an entry, every referenced
move exists and only uses effects the engines implement, and the license ships with it."""
import json
import os
import re
import sys
import unittest

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
JS = os.path.join(ROOT, "games", "pokedata.js")
PY = os.path.join(ROOT, "backend", "data", "pokemon.json")
PREFIX = "window.HQV_POKEDATA = "


def load_js():
    with open(JS, encoding="utf-8") as f:
        src = f.read()
    line = next(ln for ln in src.splitlines() if ln.startswith(PREFIX))
    return json.loads(line[len(PREFIX):].rstrip().rstrip(";")), src


def index_html():
    """index.html with its /*@include x*/ and //@include x lines filled in, as the HQ serves it."""
    with open(os.path.join(ROOT, "index.html"), encoding="utf-8") as f:
        page = f.read()

    def inc(m):
        with open(os.path.join(ROOT, m.group(1) or m.group(2)), encoding="utf-8") as g:
            return g.read()
    return re.sub(r"^[ \t]*(?:/\*@include ([^*\s]+)\*/|//@include (\S+))[ \t]*$", inc, page, flags=re.M)


class PokedataTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.data, cls.src = load_js()
        with open(PY, encoding="utf-8") as f:
            cls.server = json.load(f)

    def test_client_and_server_copies_match(self):
        self.assertEqual(self.data, self.server)

    def test_license_is_vendored(self):
        self.assertIn("MIT License", self.src)
        self.assertIn("Permission is hereby granted", self.src)
        self.assertTrue(os.path.isfile(os.path.join(ROOT, "LICENSES", "pokemon-showdown-MIT.txt")))
        self.assertIsNone(re.search(r"https?://", self.src))     # games/*.js rule: no URLs

    def test_every_reachable_pokemon_has_an_entry(self):
        html = index_html()
        evo = json.loads(re.search(r"var POKE_EVO=(\[\[[\d,\[\]]*\]\]);", html).group(1))
        self.assertEqual(evo, self.data["lines"])
        dexes = {d for line in evo for d in line}
        branch = re.search(r"var POKE_BRANCH=\{([\s\S]*?)\n\};", html).group(1)
        dexes |= {int(d) for d in re.findall(r"dex:(\d+)", branch)}
        for d in dexes:
            self.assertIn(str(d), self.data["byDex"], d)
            self.assertIn(self.data["byDex"][str(d)], self.data["pokemon"])
        megas = re.findall(r'slug:"([a-z-]+)"', re.search(r"var MEGA_FORMS=\{([\s\S]*?)\n\};", html).group(1))
        self.assertEqual(len(megas), 36)
        for slug in megas:
            pid = self.data["megas"][slug]
            self.assertIn(pid, self.data["pokemon"])
            self.assertIsInstance(self.data["pokemon"][pid]["sprite"], int)

    def test_movesets_are_real_and_implementable(self):
        moves, levels = self.data["moves"], self.data["levels"]
        allowed = {"name", "type", "cat", "bp", "acc", "pp", "pri", "drain", "recoil", "heal", "crit",
                   "status", "boosts", "self", "fixed", "sec", "struggle"}
        for mid, mv in moves.items():
            self.assertLessEqual(set(mv), allowed, mid)
            self.assertIn(mv["cat"], ("physical", "special", "status"), mid)
        for pid, e in self.data["pokemon"].items():
            self.assertEqual(sorted(e["bs"]), ["atk", "def", "hp", "spa", "spd", "spe"])
            for lv in levels:
                ms = e["moves"][str(lv)]
                self.assertTrue(1 <= len(ms) <= 4, pid)
                for m in ms:
                    self.assertIn(m, moves, (pid, m))
                self.assertTrue(any(moves[m]["cat"] != "status" for m in ms), (pid, lv))

    def test_canonical_samples(self):
        p, m = self.data["pokemon"], self.data["moves"]
        names = lambda pid, lv: [m[x]["name"] for x in p[pid]["moves"][str(lv)]]   # noqa: E731
        self.assertEqual(names("pikachu", 30), ["Spark", "Iron Tail", "Play Nice", "Thunder Shock"])
        self.assertEqual(names("charizard", 55), ["Flare Blitz", "Dragon Claw", "Air Slash", "Scary Face"])
        self.assertEqual(names("magikarp", 55), ["Tackle"])
        self.assertEqual(p["charizard"]["types"], ["Fire", "Flying"])
        self.assertEqual(p["charizard"]["bs"]["spa"], 109)
        self.assertEqual(p["charizardmegax"]["types"], ["Fire", "Dragon"])

    def test_type_chart_matches_the_gym(self):
        html = index_html()
        body = re.search(r"var TYPE_CHART=\{([\s\S]*?)\n\};", html).group(1)
        gym = {}
        for atk, row in re.findall(r"(\w+):\{([^}]*)\}", body):
            gym[atk] = {d: float(v) for d, v in re.findall(r"(\w+):([\d.]+)", row)}
        self.assertEqual(gym, self.data["chart"])


if __name__ == "__main__":
    unittest.main()
