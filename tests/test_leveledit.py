"""The Level Editor (games/leveledit.js) and the Arena must agree on what a custom
Platformer Rush level is.

The editor's live check (the LEVEL-RULES block) mirrors validate_custom in
backend/src/platformer.rs: the same caps and route-envelope constants (compared here by
regex), the same refusals for the same broken levels, and the same canonical form, pinned by
content key against the keys backend/src/platformer/tests.rs pins from the Rust side.
The route envelope is the browser character's own jump (games/platformer.js). Stdlib only;
the JS half runs under node when it is installed."""
import hashlib
import json
import os
import re
import shutil
import subprocess
import unittest

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
EDITOR = os.path.join(ROOT, "games", "leveledit.js")
PLAT_JS = os.path.join(ROOT, "games", "platformer.js")
PLAT_RS = os.path.join(ROOT, "backend", "src", "platformer.rs")
LEVELS = os.path.join(ROOT, "games", "platformer", "levels.json")
NODE = shutil.which("node")

FORBIDDEN_KEYS = {"prompt", "prompts_text", "reply", "text", "content", "path", "paths", "cwd", "folder", "project",
                  "projectName", "sessionId", "sessionTitle", "title", "file", "files"}


def read(p):
    with open(p, encoding="utf-8") as f:
        return f.read()


def rules_block():
    return re.search(r"/\* LEVEL-RULES BEGIN \*/(.*?)/\* LEVEL-RULES END \*/", read(EDITOR), re.S).group(1)


def plat_block():
    return re.search(r"/\* PLAT-LEVEL BEGIN \*/(.*?)/\* PLAT-LEVEL END \*/", read(PLAT_JS), re.S).group(1)


def js_constants():
    body = re.search(r"var K = \{(.*?)\};", rules_block(), re.S).group(1)
    return {k: float(v) for k, v in re.findall(r"([A-Z_]+):\s*([-0-9.]+)", body)}


def rust_constants():
    out = {}
    for name, expr in re.findall(r"^pub const ([A-Z_0-9]+): (?:f64|usize) = ([^;]+);", read(PLAT_RS), re.M):
        expr = expr.strip()
        if re.fullmatch(r"[0-9.]+(\s*\*\s*[0-9.]+)*", expr):
            v = 1.0
            for part in expr.split("*"):
                v *= float(part)
            out[name] = v
        else:
            out[name] = expr
    return out


def rust_list(name):
    m = re.search(r"pub const %s: \[&str; \d+\] = \[(.*?)\];" % name, read(PLAT_RS), re.S)
    return re.findall(r'"([^"]+)"', m.group(1))


def js_list(name):
    m = re.search(r"var %s = \[(.*?)\];" % name, rules_block(), re.S)
    return re.findall(r'"([^"]+)"', m.group(1))


def custom_data():
    """The same level as custom_data() in backend/src/platformer/tests.rs."""
    return {
        "kill": -6, "coopGoal": 2, "coopSecs": 90,
        "theme": {"sky": "#8FD3FF", "fog": "#cdeeff", "sea": "#5aa9e6", "light": "#fff6e0"},
        "spawns": [[-1, 0, 1], [1, 0, 1]],
        "cps": [[0, 0.5, -6]],
        "flag": [0, 1, -12],
        "coins": [[0, 1.6, -3.5], [0, 2.0, -8.5]],
        "solids": [{"m": "platform-large", "x": 0, "y": -0.5, "z": 0},
                   {"m": "platform-medium", "x": 0, "y": 0, "z": -6},
                   {"m": "platform-large", "x": 0, "y": 0.5, "z": -12}],
        "route": [[0, 0, -2, "w"], [0, 0.5, -5, "j"], [0, 0.5, -7, "w"], [0, 1, -10, "j"], [0, 1, -12, "w"]],
        "deco": [{"m": "cloud", "x": 8, "y": 6, "z": -6, "r": 0, "s": 3.2}],
        "junk": "dropped",
    }


def with_(data, fn):
    d = json.loads(json.dumps(data))
    fn(d)
    return d


def set_(path, value):
    def fn(d):
        cur = d
        for k in path[:-1]:
            cur = cur[k]
        cur[path[-1]] = value
    return fn


def bad_cases():
    """(what, data, the reason it must give): the Rust test's table, refusal for refusal."""
    gap = {"route": [[0, 0, -2, "w"], [0, 0.5, -5, "j"], [0, 0.5, -7, "w"], [0, 1, -14, "j"], [0, 1, -16, "w"]],
           "flag": [0, 1, -16]}

    def gap_fn(d):
        d["solids"][2]["z"] = -16
        d.update(json.loads(json.dumps(gap)))

    def flat_gap(d):
        d["solids"][1]["y"] = -0.5
        d["cps"][0] = [0, 0, -6]
        d["route"][1] = [0, 0, -5, "w"]
        d["route"][2] = [0, 0, -7, "w"]

    def too_high(d):
        d["solids"][1]["y"] = 3.5
        d["cps"][0] = [0, 4, -6]
        d["route"][1] = [0, 4, -5, "d"]
        d["route"][2] = [0, 4, -7, "w"]

    c = custom_data()
    return [
        ("unknown model", with_(c, set_(["solids", 1, "m"], "castle")), "unknown model 'castle'"),
        ("no spawns", with_(c, set_(["spawns"], [])), "spawns: 1 to 8"),
        ("9 spawns", with_(c, set_(["spawns"], [[0, 0, 1]] * 9)), "spawns: 1 to 8"),
        ("spawn in the air", with_(c, set_(["spawns", 1], [1, 3, 1])), "spawn 2 isn't standing"),
        ("null coordinate", with_(c, set_(["solids", 0, "x"], None)), "not a number"),
        ("1e9 coordinate", with_(c, set_(["coins", 0, 2], 1e9)), "out of range"),
        ("65 solids", with_(c, set_(["solids"], [{"m": "brick", "x": 0, "y": -9, "z": 0}] * 65)), "solids: 1 to 64"),
        ("unreachable gap", with_(c, gap_fn), "route point 4 is too far to jump: try a double jump"),
        ("w point floating 3 m up", with_(c, set_(["route", 2], [0, 3.5, -7, "w"])), "walks onto nothing"),
        ("j landing in the air", with_(c, set_(["route", 1], [5, 0.5, -5, "j"])), "route point 2 lands in the air"),
        ("coopGoal > coins", with_(c, set_(["coopGoal"], 3)), "co-op goal must be"),
        ("no flag", with_(c, lambda d: d.pop("flag")), "flag is missing"),
        ("route misses the checkpoint", with_(c, set_(["cps", 0], [2.5, 0, 2])), "misses checkpoint 1"),
        ("route not at the flag", with_(c, set_(["flag"], [2, 1, -10.5])), "doesn't end at the flag"),
        ("bad route kind", with_(c, set_(["route", 0, 3], "x")), "kind must be"),
        ("one route point", with_(c, set_(["route"], [[0, 1, -12, "w"]])), "route: 2 to 200"),
        ("walk across a gap", with_(c, set_(["route", 1, 3], "w")), "more than 0.3 m"),
        ("co-op secs", with_(c, set_(["coopSecs"], 10)), "co-op time"),
        ("theme", with_(c, set_(["theme", "sea"], "blue")), "theme sea"),
        ("rotation", with_(c, set_(["solids", 0, "r"], 45)), "turn must be"),
        ("scale", with_(c, set_(["solids", 0, "s"], 9)), "scale must be"),
        ("deco model", with_(c, set_(["deco", 0, "m"], "platform")), "decoration 1"),
        ("bool number", with_(c, set_(["kill"], True)), "kill height is not a number"),
        ("kill above the floor", with_(c, set_(["kill"], 0)), "kill height must be"),
        ("coin in a block", with_(c, set_(["coins", 0], [0, -0.3, 0])), "coin 1 is inside"),
        ("walk over a flat gap", with_(c, flat_gap), "walks over a gap"),
        ("walk into a block", with_(c, lambda d: d["solids"].append({"m": "brick", "x": 0, "y": -0.3, "z": -1})), "walks into a block"),
        ("jump through a block", with_(c, lambda d: d["solids"].append({"m": "brick", "x": 0, "y": 0.6, "z": -3.5})), "jumps through a platform"),
        ("too high even for a double jump", with_(c, too_high), "too high even for a double jump"),
        ("not an object", [1, 2, 3], "not an object"),
        ("null", None, "not an object"),
    ]


# The keys custom_keys_are_pinned_for_the_editor pins in backend/src/platformer/tests.rs:
# custom_data(), then meadow, sky and fortress.
PINNED = ["c-ca296b942375", "c-afd79d84fac1", "c-80356f2bd64f", "c-b8253b24bae3"]


def key_of(canon):
    s = json.dumps(canon, separators=(",", ":"), ensure_ascii=False)
    return "c-" + hashlib.sha256(("plat:" + s).encode("utf-8")).hexdigest()[:12]


class Static(unittest.TestCase):
    def test_constants_match_the_arena(self):
        js, rs = js_constants(), rust_constants()
        self.assertGreater(len(js), 25)
        for name, v in js.items():
            self.assertIn(name, rs, name)
            self.assertEqual(v, rs[name], name)

    def test_model_lists_match_the_arena(self):
        self.assertEqual(js_list("MODEL_NAMES"), rust_list("MODEL_NAMES"))
        self.assertEqual(js_list("DECO_MODELS"), rust_list("DECO_MODELS"))
        plat = plat_block()
        models = re.findall(r'"([a-z-]+)": \["(?:box|round)"', plat)
        self.assertEqual(sorted(models), sorted(js_list("MODEL_NAMES")))

    def test_the_route_envelope_is_the_browser_characters_jump(self):
        js, src = js_constants(), read(PLAT_JS)
        def char(name):
            return float(re.search(r"\b%s = ([0-9.]+)" % name, src).group(1))
        self.assertEqual(js["ROUTE_JUMP"], char("JUMP"))
        self.assertEqual(js["ROUTE_DJUMP"], char("DJUMP"))
        self.assertEqual(js["ROUTE_GRAV"], char("GRAV"))
        self.assertLess(js["ROUTE_SPEED"], char("RUN"))
        self.assertEqual(js["CP_R"], char("CP_R"))

    def test_no_forbidden_strings(self):
        src = read(EDITOR)
        for bad in ("http://", "https://", "innerHTML", "eval(", "Math.random", "localStorage"):
            self.assertNotIn(bad, src, bad)

    def test_a_level_doc_carries_no_forbidden_key(self):
        def keys(o):
            if isinstance(o, dict):
                out = set(o)
                for v in o.values():
                    out |= keys(v)
                return out
            if isinstance(o, list):
                out = set()
                for v in o:
                    out |= keys(v)
                return out
            return set()
        doc = {"kind": "plat", "v": 1, "name": "Three Hops", "data": custom_data()}
        self.assertEqual(keys(doc) & FORBIDDEN_KEYS, set())
        # what the start op adds around it
        self.assertEqual({"type", "g", "op", "level", "mode", "custom"} & FORBIDDEN_KEYS, set())

    def test_the_editor_registers_its_card_and_maker(self):
        src = read(EDITOR)
        self.assertIn('id: "make-plat"', src)
        self.assertIn("workshop: true", src)
        self.assertIn("HQV.makers.plat", src)
        self.assertIn('storyNote("make-save")', src)
        self.assertIn('if(!HQV || !HQV.api) return;', src)
        # drafts live in the Valley save under workshop.plat, at most 6 of them, under 8 KiB each
        self.assertIn("s.workshop.plat", src)
        self.assertIn("MAX_DRAFTS = 6", src)
        self.assertIn("DRAFT_MAX = 8192", src)

    def test_the_platformer_takes_custom_levels(self):
        src = read(PLAT_JS)
        self.assertIn('storyNote("race-custom")', src)
        self.assertIn("A.arena.maps", src)
        self.assertIn('level: "custom"', src)
        self.assertIn("view.custom", src)
        # the mirrored block itself is untouched by custom levels
        self.assertNotIn("custom", plat_block())

    def test_the_canonical_key_rule(self):
        self.assertEqual(len(PINNED), 4)
        self.assertTrue(all(re.fullmatch(r"c-[0-9a-f]{12}", k) for k in PINNED))


@unittest.skipUnless(NODE, "node is not installed")
class UnderNode(unittest.TestCase):
    """Runs the editor's LEVEL-RULES block on top of the platformer's PLAT-LEVEL block."""

    @classmethod
    def setUpClass(cls):
        cases = [[w, d, why] for (w, d, why) in bad_cases()]
        levels = json.loads(read(LEVELS))["levels"]
        fix_a = with_(custom_data(), set_(["coins", 0, 0], 1.004))
        fix_b = with_(custom_data(), set_(["coins", 0, 0], 1.0))
        script = plat_block() + "\n" + rules_block() + r"""
var RU = levelRules(PL), IN = JSON.parse(require("fs").readFileSync(0, "utf8")), out = {};
var good = RU.check(IN.good);
out.good = {ok: good.ok, errors: good.errors, canon: good.canon, keys: Object.keys(good.canon || {})};
out.levels = IN.levels.map(function(l){ var c = RU.check(l); return {id: l.id, ok: c.ok, errors: c.errors, canon: c.canon,
  again: c.canon ? JSON.stringify(RU.check(c.canon).canon) === JSON.stringify(c.canon) : false}; });
out.bad = IN.bad.map(function(b){ var c = RU.check(b[1]); return [b[0], c.ok, c.errors[0] || ""]; });
var a = RU.check(IN.a).canon, b = RU.check(IN.b).canon;
out.same = JSON.stringify(a) === JSON.stringify(b);
out.r2 = [RU.r2(-0.125), RU.r2(0.125), RU.r2(-0.001), RU.r2(1.004)];
out.names = ["  Three Hops  ", "", "a".repeat(33), "<b>", "see HTTPS", "tab\there", "Ünïcode ok ✓"].map(RU.cleanName);
out.bound = [RU.liftBound(1.3)];
process.stdout.write(JSON.stringify(out));
"""
        payload = json.dumps({"good": custom_data(), "levels": levels, "bad": cases, "a": fix_a, "b": fix_b})
        r = subprocess.run([NODE, "-e", script], input=payload, capture_output=True, text=True, timeout=60)
        if r.returncode:
            raise AssertionError(r.stderr)
        cls.out = json.loads(r.stdout)

    def test_a_good_level_passes_with_canonical_keys(self):
        g = self.out["good"]
        self.assertTrue(g["ok"], g["errors"])
        self.assertEqual(g["keys"], ["kill", "coopGoal", "coopSecs", "theme", "spawns", "cps", "flag", "coins", "solids", "route", "deco"])
        self.assertEqual(g["canon"]["theme"]["sky"], "#8fd3ff")
        self.assertNotIn("junk", g["canon"])

    def test_the_built_in_levels_pass(self):
        for l in self.out["levels"]:
            self.assertTrue(l["ok"], (l["id"], l["errors"]))
            self.assertTrue(l["again"], l["id"])

    def test_canonical_form_matches_the_arena_byte_for_byte(self):
        keys = [key_of(self.out["good"]["canon"])] + [key_of(l["canon"]) for l in self.out["levels"]]
        self.assertEqual(keys, PINNED)

    def test_broken_levels_are_refused_for_the_same_reason(self):
        for what, ok, first in self.out["bad"]:
            why = [c[2] for c in bad_cases() if c[0] == what][0]
            self.assertFalse(ok, what)
            self.assertIn(why, first, what)

    def test_rounding_and_names(self):
        self.assertTrue(self.out["same"])
        self.assertEqual(self.out["r2"], [-0.13, 0.13, 0, 1.0])
        names = self.out["names"]
        self.assertEqual(names[0], {"name": "Three Hops"})
        for n in names[1:6]:
            self.assertIn("error", n)
        self.assertEqual(names[6], {"name": "Ünïcode ok ✓"})
        self.assertAlmostEqual(self.out["bound"][0], 3.106666666666666, places=9)


if __name__ == "__main__":
    unittest.main()
