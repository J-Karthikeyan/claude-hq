"""Map Editor (HQ 2.5): the editor's live checks must agree with the Arena referee.

games/mapedit.js checks a Blaster map as you build it with the shared geometry of
games/fps.js (the FPS-SHARED block) and with limits copied from backend/src/fps.rs
(validate_custom). These tests pin the copied limits to the Rust constants, run the
editor's pure functions under node (when it is installed) against hand-made maps, and
check that the editor's starter maps still equal the fixtures the Rust tests validate
(backend/src/fps/tests.rs, MAPEDIT-STARTERS). Stdlib only."""
import json
import os
import re
import shutil
import subprocess
import unittest

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
MAPEDIT = os.path.join(ROOT, "games", "mapedit.js")
FPS_JS = os.path.join(ROOT, "games", "fps.js")
FPS_RS = os.path.join(ROOT, "backend", "src", "fps.rs")
FPS_TESTS_RS = os.path.join(ROOT, "backend", "src", "fps", "tests.rs")
VIEWS = os.path.join(ROOT, "ui", "app", "10-views.js")
COURTYARD = os.path.join(ROOT, "games", "fps", "map.json")
NODE = shutil.which("node")


def read(p):
    with open(p, encoding="utf-8") as f:
        return f.read()


def num(expr):
    """A plain number or a product like "12 * 1024"."""
    out = 1.0
    for part in expr.split("*"):
        out *= float(part)
    return out


def rs_const(name):
    m = re.search(r"pub const %s: [a-z0-9]+ = ([0-9. *]+);" % name, read(FPS_RS))
    assert m, name
    return num(m.group(1))


def js_const(name):
    m = re.search(r"\b%s = ([0-9.*]+)[,;]" % name, read(MAPEDIT))
    assert m, name
    return num(m.group(1))


# The node side: FS from fps.js, then mapedit.js against a stub HQV; the script reads a
# list of cases from stdin and prints one JSON answer per case.
HARNESS = r"""
var fs = require("fs"), root = %(root)s;
var blk = fs.readFileSync(root + "/games/fps.js", "utf8").match(/\/\* FPS-SHARED BEGIN \*\/([\s\S]*?)\/\* FPS-SHARED END \*\//)[1];
var FS = new Function(blk + "; return FS;")();
var cards = [];
global.window = {HQV: {api: {save: null, toast: function(){}, open: function(){}, persist: function(){}},
                       register: function(g){ cards.push(g); }, fpsShared: FS}};
new Function(fs.readFileSync(root + "/games/mapedit.js", "utf8"))();
var M = window.HQV.mapEdit, input = JSON.parse(fs.readFileSync(0, "utf8")), out = {cards: cards.map(function(c){ return {id: c.id, name: c.name, workshop: !!c.workshop}; }),
    makers: Object.keys(window.HQV.makers.fps || {}).sort(), limits: M.limits};
out.yards = [24, 32, 40, 48].map(function(s){ var c = M.check(M.docOf("Yard", M.openYard(s))); return {errors: c.errors.length, warns: c.warns.length}; });
out.starters = [M.canon(M.openYard(32)), M.canon(M.fromCourtyard(JSON.parse(fs.readFileSync(root + "/games/fps/map.json", "utf8"))))];
out.cases = input.map(function(cs){
  var c = M.check({kind: "fps", v: 1, name: cs.name, data: cs.data});
  return {errors: c.errors.map(function(e){ return e.msg; }), warns: c.warns.map(function(e){ return e.msg; }),
          spawnBad: c.spawnBad, pairs: c.pairs.map(function(p){ return [p.i, p.j, p.warn]; })};
});
out.canon = M.canon({bounds: [-10.004, -1, -10, 10, 12, 10], theme: {sky: "#AABBCC", fog: "#ddeeff", ground: "#112233"},
                     boxes: [[0.123, 0, 0, 1.006, 1, 1, "crate"]], spawns: [[1, 0, 1, -90.4], [2, 0, 2, 725]],
                     pickups: [{id: "x9", kind: "health", at: [0, 0, 0]}, {kind: "bogus", at: [1, 0, 1]}]});
out.names = ["", "  ", "Fine name", "x".repeat(33), "<b>", "see HTTP here", "ok\u0007"].map(M.nameErr);
out.adopt = [M.adopt(null), M.adopt({kind: "kart", data: {}}), M.adopt({kind: "fps", data: {bounds: [1, 2]}}) , !!M.adopt({kind: "fps", name: "Y", data: M.openYard(24)})];
process.stdout.write(JSON.stringify(out));
"""


def yard():
    """A 20 m walled yard: floor, four walls, nothing else, and no spawns yet."""
    return {"bounds": [-10, -1, -10, 10, 12, 10], "theme": {"sky": "#9fd3f0", "fog": "#cfe8f2", "ground": "#6fb35a"},
            "boxes": [[-10, -1, -10, 10, 0, 10, "floor"], [-10, 0, -10, 10, 4, -9.5, "wall"], [-10, 0, 9.5, 10, 4, 10, "wall"],
                      [-10, 0, -9.5, -9.5, 4, 9.5, "wall"], [9.5, 0, -9.5, 10, 4, 9.5, "wall"]],
            "spawns": [], "pickups": []}


def ring(n=8, r=8.0):
    import math
    return [[round(r * math.sin(2 * math.pi * k / n), 2), 0, round(r * math.cos(2 * math.pi * k / n), 2), 0] for k in range(n)]


class MapEditStatic(unittest.TestCase):
    def test_limits_match_the_referee(self):
        for js, rs in (("MAX_BOXES", "MAX_BOXES"), ("MIN_SPAWNS", "MIN_SPAWNS"), ("MAX_SPAWNS", "MAX_SPAWNS"),
                       ("MAX_PICKUPS", "MAX_PICKUPS"), ("COORD_MAX", "COORD_MAX"), ("MAX_DATA", "MAX_DATA_BYTES"),
                       ("NAME_MAX", "NAME_MAX")):
            self.assertEqual(js_const(js), rs_const(rs), js)
        kinds = re.findall(r'\{id: "([a-z]+)", name: "[^"]+", h: [0-9.]+\}', read(MAPEDIT))
        rs_kinds = re.search(r"pub const BOX_KINDS: \[&str; 6\] = \[([^\]]+)\];", read(FPS_RS)).group(1)
        self.assertEqual(sorted(kinds), sorted(re.findall(r'"([a-z]+)"', rs_kinds)))
        self.assertEqual(js_const("LOS_WARN"), 12)
        self.assertEqual(js_const("DRAFTS"), 6)
        self.assertEqual(js_const("DRAFT_MAX"), 8 * 1024)

    def test_house_rules(self):
        src = read(MAPEDIT)
        for bad in ("http://", "https://", "innerHTML", "eval(", "Math.random"):
            self.assertNotIn(bad, src, bad)
        self.assertIn('if(!HQV || !HQV.api) return;', src)
        self.assertIn('id: "make-fps", name: "Map Editor"', src)
        self.assertIn("workshop: true", src)
        self.assertIn('storyNote("make-save")', src)
        self.assertIn("HQV.makers.fps = {", src)
        self.assertIn("e.preventDefault()", src)
        # owned keys never reach the view switcher
        for k in ('"h"', '"j"', '"r"', '"w"', '"c"', '"0"'):
            self.assertIn("k === " + k, src)

    def test_loaded_after_blaster(self):
        files = json.loads(re.search(r"var VALLEY_FILES = (\[[^\]]*\]);", read(VIEWS)).group(1))
        self.assertIn("mapedit", files)
        self.assertLess(files.index("fps"), files.index("mapedit"))

    def test_blaster_takes_custom_maps(self):
        src = read(FPS_JS)
        shared = re.search(r"/\* FPS-SHARED BEGIN \*/(.*?)/\* FPS-SHARED END \*/", src, re.S).group(1)
        self.assertNotIn("CURMAP", shared)  # the mirrored block is untouched
        self.assertIn('msg.map = "custom"', src)
        self.assertIn('storyNote("race-custom")', src)
        self.assertIn("A.arena.maps", src)
        self.assertIn("HQV.fpsPlay = function(doc, room)", src)
        self.assertIsNone(re.search(r"[^_A-Za-z]MAP\.", src), "every arena read goes through CURMAP")


@unittest.skipUnless(NODE, "node is not installed")
class MapEditNode(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cases = []

        def case(name, mutate, nm="Test map"):
            d = yard()
            d["spawns"] = ring()
            mutate(d)
            cases.append((name, {"name": nm, "data": d}))

        case("ok", lambda d: None)
        case("in-wall", lambda d: d["boxes"].append([-1, 0, 7, 1, 3, 9, "wall"]))   # over spawn 1 at (0, 8)
        case("floating", lambda d: d["spawns"].__setitem__(1, [d["spawns"][1][0], 2, d["spawns"][1][2], 0]))
        case("ten", lambda d: d["spawns"].extend([[0, 0, 0, 0], [1, 0, 1, 0]]))
        case("five", lambda d: d["spawns"].__delitem__(slice(5, None)))
        case("outside-box", lambda d: d["boxes"].append([8, 0, 8, 12, 1, 9, "crate"]))
        case("too-many-boxes", lambda d: d["boxes"].extend([[0, 0, 0, 0.5, 0.5, 0.5, "crate"]] * 92))
        case("pickup-in-wall", lambda d: d["pickups"].append({"id": "p1", "kind": "ammo", "at": [-9.8, 0, 0]}))
        case("bad-name", lambda d: None, nm="<oops>")
        # two spawns 6 m apart in the open see each other (a warning, not an error) ...
        case("los-near", lambda d: d["spawns"].__setitem__(slice(0, 2), [[-3, 0, 0, 90], [3, 0, 0, 270]]))
        # ... and a wall between them hides them from each other
        case("los-blocked", lambda d: (d["spawns"].__setitem__(slice(0, 2), [[-3, 0, 0, 90], [3, 0, 0, 270]]),
                                       d["boxes"].append([-0.5, 0, -3, 0.5, 3, 3, "wall"])))
        cls.names = [c[0] for c in cases]
        script = HARNESS % {"root": json.dumps(ROOT)}
        r = subprocess.run([NODE, "-e", script], input=json.dumps([c[1] for c in cases]), capture_output=True, text=True, timeout=60)
        if r.returncode != 0:
            raise AssertionError(r.stderr)
        cls.out = json.loads(r.stdout)
        cls.by = dict(zip(cls.names, cls.out["cases"]))

    def test_the_card_and_the_maker(self):
        self.assertEqual(self.out["cards"], [{"id": "make-fps", "name": "Map Editor", "workshop": True}])
        self.assertEqual(self.out["makers"], ["drafts", "edit", "game", "icon", "name", "play"])

    def test_a_good_map_passes(self):
        self.assertEqual(self.by["ok"]["errors"], [])

    def test_a_spawn_in_a_wall_is_flagged(self):
        c = self.by["in-wall"]
        self.assertEqual(c["spawnBad"], {"0": "is inside a wall"})
        self.assertIn("Spawn 1 is inside a wall.", c["errors"])
        self.assertFalse(any(0 in p[:2] for p in c["pairs"]), "a bad spawn draws no sight lines")

    def test_a_spawn_on_nothing_is_flagged(self):
        self.assertEqual(self.by["floating"]["spawnBad"], {"1": "isn't standing on anything"})

    def test_the_spawn_count_rule(self):
        for name in ("ten", "five"):
            self.assertTrue(any("8-16 spawns, but not 10 or 15" in e for e in self.by[name]["errors"]), name)

    def test_boxes_and_pickups(self):
        self.assertIn("Box 6 (crate) is outside the arena.", self.by["outside-box"]["errors"])
        self.assertTrue(any("Too many boxes: 97 of 96" in e for e in self.by["too-many-boxes"]["errors"]))
        self.assertIn("Ammo pickup 1 is inside a wall.", self.by["pickup-in-wall"]["errors"])
        self.assertTrue(any("< >" in e for e in self.by["bad-name"]["errors"]))

    def test_line_of_sight(self):
        near = self.by["los-near"]
        self.assertEqual(near["errors"], [])
        self.assertIn([0, 1, True], near["pairs"])
        self.assertIn("Spawns 1 and 2 can see each other only 6.0 m apart.", near["warns"])
        blocked = self.by["los-blocked"]
        self.assertNotIn([0, 1, True], blocked["pairs"])
        self.assertEqual(blocked["warns"], [w for w in blocked["warns"] if not w.startswith("Spawns 1 and 2 ")])

    def test_starter_maps_are_clean_and_match_the_rust_fixtures(self):
        for y in self.out["yards"]:
            self.assertEqual(y, {"errors": 0, "warns": 0})
        block = re.search(r"/\* MAPEDIT-STARTERS BEGIN \*/(.*?)/\* MAPEDIT-STARTERS END \*/", read(FPS_TESTS_RS), re.S).group(1)
        fixtures = [json.loads(s) for s in re.findall(r'r##"(.*?)"##', block, re.S)]
        self.assertEqual(fixtures, self.out["starters"], "regenerate MAPEDIT-STARTERS in backend/src/fps/tests.rs")

    def test_canonical_form(self):
        c = self.out["canon"]
        self.assertEqual(list(c), ["bounds", "theme", "boxes", "spawns", "pickups"])
        self.assertEqual(c["bounds"][0], -10)
        self.assertEqual(c["theme"], {"sky": "#aabbcc", "fog": "#ddeeff", "ground": "#112233"})
        self.assertEqual(c["boxes"], [[0.12, 0, 0, 1.01, 1, 1, "crate"]])
        self.assertEqual([s[3] for s in c["spawns"]], [270, 5])
        self.assertEqual([(p["id"], p["kind"]) for p in c["pickups"]], [("p1", "health"), ("p2", "ammo")])

    def test_names_and_adopt(self):
        ok = [n == "" for n in self.out["names"]]
        self.assertEqual(ok, [False, False, True, False, False, False, False])
        self.assertEqual(self.out["adopt"], [None, None, None, True])


if __name__ == "__main__":
    unittest.main()
