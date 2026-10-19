"""Mini Golf: the browser and the Arena server must roll every shot identically.

games/golf/courses.json is mirrored byte-for-byte at backend/data/golf_courses.json (the
Arena embeds it at build time); the JS simulation in games/golf.js (GOLF-SIM block), when
node is available, reproduces every
golden vector exactly. Stdlib only."""
import json
import os
import re
import shutil
import subprocess
import sys
import unittest

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
GOLF_JS = os.path.join(ROOT, "games", "golf.js")
GOLDEN = os.path.join(ROOT, "backend", "tests", "golf_golden.json")
CONSTS = ("R", "CUP", "VS", "TICK", "VMIN", "VMAX", "DRAG_NUM", "DRAG_DEN", "ROLL", "STOP", "REST_NUM",
          "REST_DEN", "CAPTURE", "SUBSTEP", "MAX_TICKS", "MAX_STROKES", "OOB_PENALTY", "AIM_MAX", "SAND_DRAG",
          "SAND_ROLL", "ICE_DRAG", "ICE_ROLL", "BUMP_NUM", "BUMP_DEN", "CLOCK", "BLADE_GAP", "BLADE_HIT", "Z_SAND",
          "Z_ICE", "Z_WATER")


def read(p):
    with open(p, encoding="utf-8") as f:
        return f.read()


def sim_block():
    m = re.search(r"/\* GOLF-SIM BEGIN \*/(.*?)/\* GOLF-SIM END \*/", read(GOLF_JS), re.S)
    assert m, "GOLF-SIM markers missing"
    return m.group(1)


def const_value(src, name):
    m = re.search(r"\b%s\s*=\s*([0-9]+(?:\s*\*\s*[A-Z_]+|\s*\*\s*[0-9]+)?)" % name, src)
    assert m, name
    expr = m.group(1)
    if "*" in expr:
        a, b = [x.strip() for x in expr.split("*")]
        b = int(b) if b.isdigit() else const_value(src, b)
        return int(a) * b
    return int(expr)


class GolfSync(unittest.TestCase):
    def test_course_file_is_mirrored_for_the_server(self):
        with open(os.path.join(ROOT, "games", "golf", "courses.json"), "rb") as a, \
                open(os.path.join(ROOT, "backend", "data", "golf_courses.json"), "rb") as b:
            self.assertEqual(a.read(), b.read(), "run python3 tools/golf_walls.py")

    def test_course_walls_match_the_models(self):
        out = subprocess.run([sys.executable, os.path.join(ROOT, "tools", "golf_walls.py"), "--check"],
                             capture_output=True, text=True, timeout=120)
        self.assertEqual(out.returncode, 0, out.stdout + out.stderr)

    def test_courses_reference_known_pieces(self):
        data = json.loads(read(os.path.join(ROOT, "games", "golf", "courses.json")))
        self.assertGreaterEqual(len(data["courses"]), 3)
        for c in data["courses"]:
            self.assertGreaterEqual(len(c["holes"]), 2, c["id"])
            for h in c["holes"]:
                for t in h["tiles"]:
                    self.assertIn(t[0], data["pieces"])
                    self.assertTrue(os.path.isfile(os.path.join(ROOT, "games", "golf", t[0] + ".glb")), t[0])

    @unittest.skipUnless(shutil.which("node"), "node not installed")
    def test_js_simulation_matches_golden_vectors(self):
        script = sim_block() + r"""
const fs = require("fs");
const data = JSON.parse(fs.readFileSync(process.argv[1], "utf8"));
const vec = JSON.parse(fs.readFileSync(process.argv[2], "utf8"));
const cache = {}; const bad = [];
for (const v of vec) {
  const c = data.courses.find(x => x.id === v.course), key = v.course + "/" + v.hole;
  const h = cache[key] || (cache[key] = GS.compileHole(c.holes[v.hole], data.pieces));
  const r = GS.simulate(h, v.from[0], v.from[1], v.ax, v.az, v.power, true, v.clk);
  if (r.end[0] !== v.end[0] || r.end[1] !== v.end[1] || r.holed !== v.holed || r.oob !== v.oob || r.water !== v.water
      || r.ticks !== v.ticks || r.path.length < 1) bad.push(v);
}
console.log(JSON.stringify({n: vec.length, bad: bad.slice(0, 3), nbad: bad.length}));
"""
        out = subprocess.run(["node", "-e", script, os.path.join(ROOT, "games", "golf", "courses.json"), GOLDEN],
                             capture_output=True, text=True, timeout=120)
        self.assertEqual(out.returncode, 0, out.stderr)
        res = json.loads(out.stdout.strip().splitlines()[-1])
        self.assertGreaterEqual(res["n"], 150)
        self.assertEqual(res["nbad"], 0, res["bad"])


if __name__ == "__main__":
    unittest.main()
