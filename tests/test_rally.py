"""Valley Rally (games/rally.js + games/rally/): the node physics/stage suite, the house
rules for its scripts, and its registration everywhere the Valley lists a game.
Stdlib only; the node half is skipped without node."""
import json
import os
import re
import shutil
import subprocess
import unittest

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


def read(*p):
    with open(os.path.join(ROOT, *p), encoding="utf-8") as f:
        return f.read()


RALLY_SCRIPTS = [("games", "rally.js"), ("games", "rally", "physics.js"), ("games", "rally", "stage.js"),
                 ("games", "rally", "scene.js"), ("games", "rally", "tools", "tune.js")]


@unittest.skipUnless(shutil.which("node"), "node not installed")
class RallyNode(unittest.TestCase):
    def test_physics_and_stage_suite(self):
        r = subprocess.run(["node", os.path.join(ROOT, "tests", "rally_physics.test.js")],
                           capture_output=True, text=True, cwd=ROOT, timeout=300)
        self.assertEqual(r.returncode, 0, r.stdout[-4000:] + r.stderr[-2000:])
        self.assertRegex(r.stdout, r"all \d+ passed")

    def test_tuning_script_runs(self):
        r = subprocess.run(["node", os.path.join(ROOT, "games", "rally", "tools", "tune.js"), "0", "gravel"],
                           capture_output=True, text=True, cwd=ROOT, timeout=120)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("Moraine T4", r.stdout)

    def test_stage_doc_is_a_valid_map_doc(self):
        script = r"""
const S = require(process.argv[1]);
const st = S.build({seed: "pinewood-run", biome: "forest", night: false, length: 3000});
const d = S.daily(new Date(Date.UTC(2026, 9, 19)));
console.log(JSON.stringify({doc: st.doc, daily: d, again: JSON.stringify(S.build({seed: "pinewood-run", biome: "forest", night: false, length: 3000}).doc) === JSON.stringify(st.doc)}));
"""
        r = subprocess.run(["node", "-e", script, os.path.join(ROOT, "games", "rally", "stage.js")],
                           capture_output=True, text=True, cwd=ROOT, timeout=60)
        self.assertEqual(r.returncode, 0, r.stderr)
        out = json.loads(r.stdout)
        doc = out["doc"]
        self.assertTrue(out["again"])
        self.assertEqual((doc["kind"], doc["v"]), ("rally", 1))
        self.assertEqual(set(doc["data"]), {"seed", "biome", "night", "length", "road", "gates"})
        self.assertEqual(len(doc["data"]["gates"]), 20)
        self.assertLessEqual(len(doc["data"]["road"]), 400)
        self.assertLessEqual(len(json.dumps(doc["data"], separators=(",", ":"))), 12 * 1024)
        for g in doc["data"]["gates"]:
            self.assertEqual(set(g), {"x", "z", "h", "w"})
            self.assertIsInstance(g["h"], int)
            self.assertIsInstance(g["w"], int)
        self.assertEqual(out["daily"]["seed"], "daily-2026-10-19")
        # nothing transcript-shaped can ride along in a stage
        blob = json.dumps(doc)
        for k in ("prompt", "path", "cwd", "folder", "project", "sessionId", "title", "text", "content"):
            self.assertNotIn('"%s"' % k, blob)


class RallyHouseRules(unittest.TestCase):
    def test_no_external_urls_innerhtml_eval_or_math_random(self):
        for parts in RALLY_SCRIPTS:
            src = read(*parts)
            name = "/".join(parts)
            self.assertNotRegex(src, r"https?://", name)
            self.assertFalse("innerHTML" in src, name)
            self.assertFalse("eval(" in src, name)
            self.assertFalse("Math.random" in src, name)     # mulberry32(hashStr(...)) for anything seen twice
            self.assertNotRegex(src, r"^\s*(let|const) ", name)

    def test_original_names_only(self):
        # The cars, sponsors and stages are made up; no real car makers or rally games.
        banned = ("subaru", "impreza", "lancia", "delta integrale", "audi", "quattro", "peugeot", "toyota", "celica",
                  "ford", "escort", "mitsubishi", "lancer", "nissan", "bmw", "colin mcrae", "dirt rally", "wrc",
                  "hardpack", "castrol", "michelin", "pirelli", "red bull", "martini", "rothmans")
        for parts in RALLY_SCRIPTS:
            low = read(*parts).lower()
            for b in banned:
                self.assertIsNone(re.search(r"\b" + re.escape(b) + r"\b", low), "%s mentions %r" % ("/".join(parts), b))

    def test_registered_everywhere_kart_is(self):
        views = read("ui", "app", "10-views.js")
        files = re.findall(r'"([a-z]+)"', re.search(r"VALLEY_FILES = \[([^\]]*)\]", views).group(1))
        self.assertIn("rally", files)
        self.assertLess(files.index("input"), files.index("rally"))
        self.assertLess(files.index("multi"), files.index("rally"))
        self.assertIn('rally:"Valley Rally"', views)
        multi = read("games", "multi.js")
        self.assertIn('rally:"Valley Rally"', multi)
        self.assertRegex(multi, r"var CLIENT_PROTO = \{[^}]*rally:\[1,1\]")
        self.assertIn('["rally",', read("ui", "app", "27-play.js"))
        self.assertIn('["rally","Valley Rally"]', read("ui", "app", "23-progress.js"))
        rs_arena = read("dashboard-rs", "src", "arena.rs")
        self.assertRegex(rs_arena, r'const GAMES: \[&str; \d+\] = \[[^\]]*"rally"')
        self.assertRegex(rs_arena, r'pub const QUICKPLAY_GAMES: \[&str; \d+\] = \[[^\]]*"rally"')
        self.assertRegex(read("dashboard-rs", "src", "ext.rs"), r'const MAP_KINDS: \[&str; \d+\] = \[[^\]]*"rally"')
        self.assertRegex(read("dashboard-rs", "src", "server.rs"), r"rally/\[a-z\]\[a-z0-9-\]\{0,40\}\\\.\(js\|json\|png\)")

    def test_loads_only_its_own_scripts(self):
        src = read("games", "rally.js")
        for name in re.findall(r'\["physics", "stage", "scene"\]', src):
            self.assertTrue(name)
        for n in ("physics", "stage", "scene"):
            self.assertTrue(os.path.isfile(os.path.join(ROOT, "games", "rally", n + ".js")), n)
        self.assertIn('"/games/rally/" + n + ".js"', src)

    def test_hud_styles_use_tokens_and_calm(self):
        css = read("games", "games.css")
        block = css[css.index("/* ---- Valley Rally"):]
        self.assertNotRegex(block, r"#[0-9a-fA-F]{3,6}\b")
        self.assertNotIn("animation:", block.replace("animation:none", ""))


if __name__ == "__main__":
    unittest.main()
