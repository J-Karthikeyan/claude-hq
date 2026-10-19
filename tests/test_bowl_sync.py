"""Bowling: the browser and the Rust Arena must roll every ball and score every card identically.

games/bowling.js carries the integer physics and the scoring between the BOWL-SIM markers,
mirrored from backend/src/valley/bowling.rs (golf's VS and TICK come from golf.rs). This
checks the constants match and, when node is available, replays every golden roll and
scoring fixture in backend/tests/bowl_golden.json (written by the Rust test
`regenerate_bowl_golden`) through the JS block. Stdlib only."""
import json
import os
import re
import shutil
import subprocess
import unittest

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BOWL_JS = os.path.join(ROOT, "games", "bowling.js")
BOWL_RS = os.path.join(ROOT, "backend", "src", "valley", "bowling.rs")
GOLF_RS = os.path.join(ROOT, "backend", "src", "valley", "golf.rs")
GOLDEN = os.path.join(ROOT, "backend", "tests", "bowl_golden.json")
CONSTS = ("HALF_W", "BR", "PR", "HEAD_Z", "PIN_DX", "ROW_DZ", "PIT_Z", "V_MIN", "V_MAX", "AIM_MAX", "AIM_DIV",
          "SPIN_MAX", "HOOK", "OIL_DIV", "BALL_DRAG", "V_STALL", "BALL_M", "PIN_M", "EN", "ED", "PIN_DRAG",
          "PIN_STOP", "FALL_R", "PIN_TIP", "MAX_TICKS", "SAMPLE")


def read(p):
    with open(p, encoding="utf-8") as f:
        return f.read()


def sim_block():
    m = re.search(r"/\* BOWL-SIM BEGIN \*/(.*?)/\* BOWL-SIM END \*/", read(BOWL_JS), re.S)
    assert m, "BOWL-SIM markers missing"
    return m.group(1)


def js_const(src, name):
    m = re.search(r"\bvar %s = (\[[^\]]*\]|-?\d+);" % name, src)
    assert m, name
    return json.loads(m.group(1))


def rs_const(src, name):
    m = re.search(r"pub const %s: [^=]+= (\[[^\]]*\]|-?\d+);" % name, src)
    assert m, name
    return json.loads(m.group(1))


class BowlSync(unittest.TestCase):
    def test_constants_match(self):
        js, rs, golf = sim_block(), read(BOWL_RS), read(GOLF_RS)
        for name in CONSTS:
            self.assertEqual(js_const(js, name), rs_const(rs, name), name)
        self.assertEqual(js_const(js, "OIL"), rs_const(rs, "OIL"))
        for name in ("VS", "TICK"):
            self.assertEqual(js_const(js, name), rs_const(golf, name), name)

    def test_the_block_stands_alone(self):
        block = sim_block()
        self.assertNotIn("golfSim", block)
        self.assertNotIn("HQV", block)
        self.assertIn("function tdiv", block)
        self.assertIn("function isqrt", block)

    def test_golden_file_is_big_enough(self):
        g = json.loads(read(GOLDEN))
        self.assertGreaterEqual(len(g["rolls"]), 200)
        self.assertGreaterEqual(len(g["cards"]), 5)

    @unittest.skipUnless(shutil.which("node"), "node not installed")
    def test_js_rolls_and_scores_match_the_golden_vectors(self):
        script = sim_block() + r"""
const fs = require("fs");
const g = JSON.parse(fs.readFileSync(process.argv[1], "utf8"));
const bad = [];
for (const v of g.rolls) {
  const standing = []; for (let i = 0; i < 10; i++) standing.push((v.standing & (1 << i)) !== 0);
  const r = simulateRoll({bumpers: v.bumpers, standing: standing}, v.x, v.aim, v.power, v.spin, v.oil);
  let down = 0; r.pinsDown.forEach((d, i) => { if (d) down |= 1 << i; });
  const end = r.path[r.path.length - 1];
  if (down !== v.down || r.gutter !== v.gutter || r.ticks !== v.ticks || end[0] !== v.end[0] || end[1] !== v.end[1]) bad.push(v);
}
const badCards = g.cards.filter(c => JSON.stringify(scoreCard(c.frames)) !== JSON.stringify(c.scores));
console.log(JSON.stringify({n: g.rolls.length, bad: bad.slice(0, 3), nbad: bad.length, badCards: badCards}));
"""
        out = subprocess.run(["node", "-e", script, GOLDEN], capture_output=True, text=True, timeout=120)
        self.assertEqual(out.returncode, 0, out.stderr)
        res = json.loads(out.stdout.strip().splitlines()[-1])
        self.assertGreaterEqual(res["n"], 200)
        self.assertEqual(res["nbad"], 0, res["bad"])
        self.assertEqual(res["badCards"], [])


if __name__ == "__main__":
    unittest.main()
