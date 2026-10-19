"""HQ 2.5 Track Editor (games/trackedit.js) and custom tracks in Kart Racing.

Static checks always run: the editor follows the games/ rules, its limits match the
Arena's (backend-rs/src/kart.rs), it registers the Workshop maker and stays inside its
own save key, and games/kart.js only offers custom races on an Arena that says "maps".
With node installed, the editor's TRACKEDIT-WALK block runs on fixtures: every built-in
track round-trips grid -> tiles, broken loops get a plain-words reason, and the starting
grid check agrees with kart.rs (a left turn into the start line doesn't fit). Stdlib only."""
import json
import os
import re
import shutil
import subprocess
import unittest

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
EDITOR = os.path.join(ROOT, "games", "trackedit.js")
KART_JS = os.path.join(ROOT, "games", "kart.js")
KART_RS = os.path.join(ROOT, "backend-rs", "src", "kart.rs")
TRACKS = os.path.join(ROOT, "games", "kart", "tracks.json")
NODE = shutil.which("node")


def read(p):
    with open(p, encoding="utf-8") as f:
        return f.read()


def block(src, name):
    return re.search(r"/\* %s BEGIN \*/(.*?)/\* %s END \*/" % (name, name), src, re.S).group(1)


class TrackEditorStatic(unittest.TestCase):
    def setUp(self):
        self.src = read(EDITOR)

    def test_games_rules(self):
        self.assertNotRegex(self.src, r"https?://")
        self.assertNotIn("innerHTML", self.src)
        self.assertNotIn("eval(", self.src)
        self.assertNotIn("Math.random", self.src)
        self.assertIn("if(!HQV || !HQV.api) return;", self.src)

    def test_limits_match_the_arena(self):
        rs = read(KART_RS)
        walk = block(self.src, "TRACKEDIT-WALK")
        for js, rust in (("MIN_TILES", "CUSTOM_MIN_TILES"), ("MAX_TILES", "CUSTOM_MAX_TILES")):
            a = re.search(r"\b%s = (\d+)" % js, walk).group(1)
            b = re.search(r"pub const %s: usize = (\d+);" % rust, rs).group(1)
            self.assertEqual(a, b, js)
        self.assertIn('["forest", "tents", "empty"]', rs)
        for s in ("forest", "tents", "empty"):
            self.assertIn('["%s",' % s, self.src)

    def test_registers_the_card_and_the_maker(self):
        self.assertRegex(self.src, r'HQV\.register\(\{id: "make-kart", name: "Track Editor"[^}]*workshop: true')
        self.assertIn("HQV.makers.kart = {", self.src)
        for k in ("game:", "edit:", "play:", "drafts:"):
            self.assertIn(k, self.src[self.src.index("HQV.makers.kart = {"):])
        self.assertIn('note("make-save")', self.src)

    def test_drafts_live_in_workshop_kart_only(self):
        self.assertIn("MAX_DRAFTS = 24", self.src)
        self.assertIn("DRAFT_MAX_BYTES = 8192", self.src)
        self.assertNotRegex(self.src, r"s\.workshop\.(plat|fps)")
        self.assertIn("api.persist()", self.src)

    def test_owned_keys_prevent_default(self):
        handler = self.src[self.src.index('canvas.addEventListener("keydown"'):]
        handler = handler[:handler.index("canvas.addEventListener(\"focus\"")]
        for key in ('"ArrowUp"', '" "', '"Delete"', '"Enter"'):
            self.assertIn(key, handler)
        self.assertGreaterEqual(handler.count("ev.preventDefault()"), 4)

    def test_mapdoc_has_no_forbidden_keys(self):
        doc = self.src[self.src.index("function docOf(res){"):]
        doc = doc[:doc.index("\n}")]
        keys = set(re.findall(r"\b([a-zA-Z]+):", doc))
        self.assertEqual(keys, {"kind", "v", "name", "data", "tiles", "scenery", "theme", "sky", "fog", "ground"})
        # Local-only fields that must never reach the Arena (was test_arena.FORBIDDEN_KEYS).
        forbidden = {"prompt", "prompts_text", "reply", "text", "content", "path", "paths",
                     "cwd", "folder", "project", "projectName", "sessionId", "sessionTitle",
                     "title", "file", "files"}
        self.assertEqual(keys & forbidden, set())

    def test_publish_only_when_the_workshop_is_here(self):
        self.assertIn('typeof window.workshopPublish === "function"', self.src)

    def test_loaded_after_kart(self):
        views = read(os.path.join(ROOT, "ui", "app", "10-views.js"))
        files = re.findall(r'"([a-z]+)"', re.search(r"VALLEY_FILES = \[([^\]]*)\]", views).group(1))
        self.assertIn("trackedit", files)
        self.assertLess(files.index("kart"), files.index("trackedit"))


class KartCustomStatic(unittest.TestCase):
    def setUp(self):
        self.src = read(KART_JS)

    def test_custom_lives_outside_the_mirrored_block(self):
        kt = block(self.src, "KART-TRACK")
        self.assertNotIn("custom", kt.lower())

    def test_custom_races_need_maps_and_a_new_arena(self):
        self.assertIn("A.arena && A.arena.maps", self.src)
        self.assertIn("if(mp && host && LEGACY === false && mapsOk())", self.src)
        self.assertIn('MP.send("kart", "start", {track: "custom"', self.src)
        self.assertIn('HQV.story.note("race-custom")', self.src)

    def test_view_custom_is_compiled_before_the_lookup(self):
        av = self.src[self.src.index("function applyView(view, force){"):]
        self.assertLess(av.index("addCustom(view.track"), av.index("!TRACKS[view.track]){ if(V.phase"))
        self.assertIn("CKEY_RE = /^c-[0-9a-f]{12}$/", self.src)


@unittest.skipUnless(NODE, "node not installed")
class TrackEditorUnderNode(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        walk = block(read(EDITOR), "TRACKEDIT-WALK")
        kt = block(read(KART_JS), "KART-TRACK")
        tracks = json.loads(read(TRACKS))["tracks"]
        script = kt + walk + r"""
function fits(tiles){
  var tr = KT.compileTrack(tiles);
  for(var k = 0; k < 8; k++){ var g = KT.gridSlot(k), l = KT.locate(tr, g[0], g[1]); if(!l || Math.abs(l[2]) > KT.ROAD_HALF) return false; }
  return true;
}
function grid(rows){ var a = []; rows.forEach(function(r){ for(var i = 0; i < r.length; i++) a.push(r.charAt(i) === "#" ? 1 : 0); }); return a; }
var out = {builtins: {}, cases: {}, names: {}};
TRACKS.forEach(function(t){
  var p = TW.place(t.path, 16), w = p && TW.walk(p.cells, 16, p.start, fits);
  out.builtins[t.id] = w && w.ok ? w.tiles : (w ? w.msg : "nofit");
});
var pad = "................";
function fill(rows){ var r = rows.map(function(x){ return (x + pad).slice(0, 16); }); while(r.length < 16) r.push(pad); return grid(r); }
var C = out.cases;
C.empty = TW.walk(fill([]), 16, null, fits);
C.deadend = TW.walk(fill(["####", "#..#", "#..#", "####", "#"]), 16, null, fits);
C.short = TW.walk(fill(["##", "##"]), 16, null, fits);
C.eight = TW.walk(fill(["###", "#.#", "###"]), 16, null, fits);
C.ring = TW.walk(fill(["#####", "#...#", "#...#", "#####"]), 16, null, fits);
C.ringStart = TW.walk(fill(["#####", "#...#", "#...#", "#####"]), 16, {c: 0, r: 1, d: 2}, fits);
C.notStraight = TW.walk(fill(["#####", "#...#", "#...#", "#####"]), 16, {c: 0, r: 0, d: 0}, fits);
C.twoLoops = TW.walk(fill(["####.####", "#..#.#..#", "####.####"]), 16, null, fits);
C.fork = TW.walk(fill(["#####", "#.#.#", "#####"]), 16, null, fits);
var rings = []; for(var y = 0; y < 16; y++){ var row = ""; for(var x = 0; x < 16; x++){
  var outer = x === 0 || y === 0 || x === 15 || y === 15, inner = x >= 2 && x <= 13 && y >= 2 && y <= 13 && (x === 2 || y === 2 || x === 13 || y === 13);
  row += outer || inner ? "#" : "."; } rings.push(row); }
C.long = TW.walk(fill(rings), 16, null, fits);
C.leftIn = fits("FSLSSLSSLSSL"); C.rightIn = fits("FSRSSRSSRSSR");
C.place = TW.place("FSSRSSRSSSSRSSRS", 16); C.noClose = TW.place("FSSS", 16);
["Back Lot", "  ", "a".repeat(33), "<b>", "see HTTP here", "tab\there", "Ünïcode ✓"].forEach(function(n){ out.names[n] = TW.nameError(n); });
console.log(JSON.stringify(out));
""".replace("TRACKS", json.dumps(tracks))
        res = subprocess.run([NODE, "-"], input=script, capture_output=True, text=True, timeout=60)
        if res.returncode != 0:
            raise AssertionError(res.stderr)
        cls.out = json.loads(res.stdout)
        cls.tracks = tracks

    def test_every_built_in_track_round_trips(self):
        for t in self.tracks:
            self.assertEqual(self.out["builtins"][t["id"]], t["path"], t["id"])

    def test_reasons_in_plain_words(self):
        c = self.out["cases"]
        self.assertIn("Paint a loop", c["empty"]["msg"])
        self.assertIn("Dead end at column 1, row 5", c["deadend"]["msg"])
        self.assertIn("at least 8", c["short"]["msg"])
        self.assertTrue(c["eight"]["ok"])   # the smallest track: a 3 x 3 ring
        self.assertEqual(len(c["eight"]["tiles"]), 8)
        self.assertIn("aren't on the loop", c["twoLoops"]["msg"])
        self.assertFalse(c["fork"]["ok"])
        self.assertIn("at most 80", c["long"]["msg"])

    def test_a_ring_is_a_track_and_the_start_sets_the_direction(self):
        c = self.out["cases"]
        self.assertTrue(c["ring"]["ok"])
        self.assertEqual(len(c["ring"]["tiles"]), 14)
        self.assertEqual(c["ring"]["tiles"][0], "F")
        self.assertEqual(c["ring"]["tiles"].count("R"), 4)   # auto start: clockwise
        self.assertTrue(c["ringStart"]["ok"])
        self.assertEqual(c["ringStart"]["tiles"].count("L"), 4)   # down the left side: the other way round
        self.assertEqual(c["ringStart"]["start"], {"c": 0, "r": 1, "d": 2})
        # a corner can't be the start line: the editor picks a straight itself
        self.assertTrue(c["notStraight"]["ok"])
        self.assertNotEqual(c["notStraight"]["start"], {"c": 0, "r": 0, "d": 0})

    def test_the_grid_check_matches_the_arena(self):
        c = self.out["cases"]
        self.assertFalse(c["leftIn"])   # kart.rs: a_loop_may_end_on_a_corner_and_the_grid_check_still_bites
        self.assertTrue(c["rightIn"])
        self.assertIsNone(c["noClose"])
        self.assertEqual(sum(c["place"]["cells"]), 16)

    def test_names_follow_the_arena_rules(self):
        n = self.out["names"]
        self.assertEqual(n["Back Lot"], "")
        self.assertEqual(n["Ünïcode ✓"], "")
        for bad in ("  ", "a" * 33, "<b>", "see HTTP here", "tab\there"):
            self.assertNotEqual(n[bad], "", bad)


if __name__ == "__main__":
    unittest.main()
