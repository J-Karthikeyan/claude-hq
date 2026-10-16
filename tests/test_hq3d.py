"""HQ 2.0 Wave 0: the 3D HQ (games/hq3d.js) and its glue (ui/app/22-hq.js). The scene
needs WebGL, so this checks what can run headless: how a session maps to a crew
member (state, project, name), and that the page wires the view, the H key, the
header switch and the crew list. The scene module runs in node with stubs."""
import json
import os
import shutil
import subprocess
import sys
import unittest

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, ROOT)
import dashboard  # noqa: E402

SCRIPT = r"""
const fs = require('fs');
const window = {HQV: {}, matchMedia: () => ({matches: false})};
const document = {documentElement: {classList: {contains: () => false}}};
global.performance = {now: () => 0};
for (const f of process.argv.slice(2)) new Function('window', 'document', fs.readFileSync(f, 'utf8'))(window, document);
const H = window.HQV.hq3d, out = {};
out.states = ['working', 'needs', 'idle', 'stale', 'weird', undefined].map(s => H.crewState({status: s}));
out.proj = [H.projectOf({cwd: '/Users/me/code/claude-hq/', folder: '-Users-me-code-claude-hq'}),
            H.projectOf({folder: '-Users-me-fleet'}), H.projectOf({}), H.projectOf({folder: 'plain'})];
out.title = [H.titleOf({title: 'Fix the shinies'}), H.titleOf({title: 'Untitled session', name: '02d87ed7', cwd: '/x/analytics'}),
             H.titleOf({name: 'adapalokesh-15'})];
console.log(JSON.stringify(out));
"""


class HqWiring(unittest.TestCase):
    def setUp(self):
        self.html = dashboard.assemble_index()

    def test_view_tab_switch_and_container(self):
        self.assertIn('data-view="hq"', self.html)
        self.assertIn('id="hqModeBtn"', self.html)
        self.assertIn('id="hqView"', self.html)
        self.assertIn('id="hqCrew"', self.html)
        self.assertIn('if(typeof hqViewChanged==="function") hqViewChanged(v);', self.html)
        self.assertIn('e.key==="h"||e.key==="H"){ e.preventDefault(); hqToggle(); }', self.html)

    def test_page_lands_on_the_classic_dashboard(self):
        # The 3D HQ is never opened on load, whatever was last used: H or the header switch opens it.
        self.assertNotIn('hqModePref()==="3d" && !hqCalm() && hqWebGL()) setView("hq")', self.html)
        self.assertIn('setTimeout(function(){ hqViewChanged(VIEW); }, 0);', self.html)
        self.assertIn('hqModeSave("classic")', self.html)
        self.assertIn('hqModeSave("3d")', self.html)

    def test_scene_files_are_served(self):
        for name in ("engine.js", "hq3d.js", "hqlobby.js", "hqbase.js", "hqcity.js"):
            got = dashboard.game_file(name)
            self.assertIsNotNone(got, name)
            self.assertEqual(dashboard.game_cache_control(name), "no-store")

    def test_nothing_leaves_the_machine(self):
        for name in ("hq3d.js", "hqlobby.js", "hqbase.js", "hqcity.js"):
            with open(os.path.join(ROOT, "games", name), encoding="utf-8") as f:
                src = f.read()
            for bad in ("fetch(", "XMLHttpRequest", "WebSocket", "sendBeacon", "MP.send"):
                self.assertNotIn(bad, src, name + ": " + bad)

    def test_floors_link_up(self):
        # Base front door -> Lobby; Lobby lifts -> Mission Control, front door -> Base; Mission Control lift -> Lobby
        src = {n: open(os.path.join(ROOT, "games", n + ".js"), encoding="utf-8").read() for n in ("hq3d", "hqlobby", "hqbase")}
        self.assertIn('view: "@lobby"', src["hqbase"])
        self.assertIn('view: "@mission"', src["hqlobby"])
        self.assertIn('view: "@base"', src["hqlobby"])
        self.assertIn('view: "@lobby"', src["hq3d"])
        self.assertIn('["hq3d","hqlobby","hqbase","hqcity"]', dashboard.assemble_index())


@unittest.skipUnless(shutil.which("node"), "node not installed")
class HqCrewMapping(unittest.TestCase):
    def test_mapping(self):
        r = subprocess.run(["node", "-", os.path.join(ROOT, "games", "engine.js"), os.path.join(ROOT, "games", "hq3d.js")],
                           input=SCRIPT, capture_output=True, text=True, timeout=30)
        self.assertEqual(r.returncode, 0, r.stderr)
        o = json.loads(r.stdout)
        self.assertEqual(o["states"], ["working", "needs", "idle", "stale", "stale", "stale"])
        self.assertEqual(o["proj"], ["claude-hq", "fleet", "other", "plain"])
        self.assertEqual(o["title"], ["Fix the shinies", "analytics", "adapalokesh-15"])


if __name__ == "__main__":
    unittest.main()


class HqArenaBoundary(unittest.TestCase):
    """HQ 2.1: what an open HQ sends the Arena is allowlisted in arena.py, and the
    crew counts come from this process's own view of your sessions."""

    def setUp(self):
        import arena
        self.arena = arena
        self.sent = []
        self._saved = (arena._request, arena._authed)
        arena._authed = lambda: ("tok", "https://arena.test")
        arena._request = lambda m, url, token=None, body=None: (self.sent.append((m, url, body)) or (200, {}))

    def tearDown(self):
        self.arena._request, self.arena._authed = self._saved

    def test_only_open_look_and_counts_leave(self):
        self.arena.hq_update(open_=True, look={"paint": "#224466", "sign": "Mine", "cwd": "/Users/me", "title": "secret"},
                             crew={"working": 3, "needs": 1, "idle": 99, "sessionId": "x"})
        m, url, body = self.sent[-1]
        self.assertEqual((m, url), ("PUT", "https://arena.test/v1/hq/me"))
        self.assertEqual(body, {"open": True, "look": {"paint": "#224466", "sign": "Mine"},
                                "crew": {"working": 3, "needs": 1, "idle": 64}})

    def test_visit_validates_the_user_id(self):
        self.assertEqual(self.arena.hq_visit("../../v1/admin")[0], 400)
        self.arena.hq_visit("11111111-2222-3333-4444-555555555555")
        self.assertTrue(self.sent[-1][1].endswith("/v1/hq/11111111-2222-3333-4444-555555555555"))

    def test_crew_counts_are_three_numbers(self):
        c = dashboard.hq_crew_counts()
        self.assertEqual(set(c), {"working", "needs", "idle"})
        self.assertTrue(all(isinstance(v, int) for v in c.values()))


class ProgressBoundary(unittest.TestCase):
    def setUp(self):
        import arena
        self.arena = arena
        self.sent = []
        self._saved = (arena._request, arena._authed)
        arena._authed = lambda: ("tok", "https://arena.test")
        arena._request = lambda m, url, token=None, body=None: (self.sent.append((m, url, body)) or (200, {}))

    def tearDown(self):
        self.arena._request, self.arena._authed = self._saved

    def test_read_only_and_validated(self):
        self.arena.progress(); self.arena.leaderboards("kart", "meadow"); self.arena.profile("me")
        self.assertEqual([(m, u.split("arena.test")[1], b) for m, u, b in self.sent],
                         [("GET", "/v1/progress/me", None), ("GET", "/v1/leaderboards/kart?key=meadow", None),
                          ("GET", "/v1/profile/me", None)])
        self.assertEqual(self.arena.leaderboards("chess")[0], 400)
        self.assertEqual(self.arena.profile("../admin")[0], 400)
        self.arena.leaderboards("golf", "../../x")
        self.assertTrue(self.sent[-1][1].endswith("/v1/leaderboards/golf"))

    def test_page_has_card_and_boards(self):
        html = dashboard.assemble_index()
        for needle in ('id="tcardBack"', 'id="gbWrap"', 'id="hqMyCard"', "//@include" not in html and "function tcardOpen"):
            self.assertTrue(needle in html if isinstance(needle, str) else needle)


@unittest.skipUnless(shutil.which("node"), "node not installed")
class PageScriptRunsToTheEnd(unittest.TestCase):
    """The page script is ONE script (ui/app/*): a top-level throw in an early file
    silently drops every later file. Run it under node with a forgiving DOM stub
    and check the last line is reached, both in classic and in 3D-HQ start mode."""

    def run_page(self, mode):
        html = dashboard.assemble_index()
        import re as _re
        scripts = _re.findall(r"<script>\n(.*?)\n</script>", html, _re.S)
        body = scripts[-1] + "\n;globalThis.__END__ = [typeof progLoad, typeof invOpen, typeof hqToggle].join(' ');"
        stub = r"""
const mk = () => new Proxy(function(){}, {get:(t,k)=>{ if(k===Symbol.toPrimitive) return ()=>''; if(k==='classList') return {add(){},remove(){},toggle(){},contains(){return false}};
  if(k==='dataset') return {}; if(k==='value'||k==='textContent'||k==='innerHTML') return ''; if(k==='checked') return false; if(k==='length') return 0;
  if(k==='getContext') return ()=>null; if(k==='then') return undefined; return mk(); }, apply:()=>mk(), construct:()=>mk(), set:()=>true});
const store = {hq_mode: MODE};
globalThis.localStorage = {getItem:k=>store[k]??null, setItem:(k,v)=>{store[k]=String(v)}, removeItem:k=>{delete store[k]}};
globalThis.window = globalThis; globalThis.document = mk(); globalThis.navigator = {userAgent:'node'}; globalThis.location = mk();
globalThis.matchMedia = ()=>({matches:false, addEventListener(){}}); globalThis.fetch = ()=>new Promise(()=>{});
globalThis.requestAnimationFrame = ()=>0; globalThis.setInterval = ()=>0; globalThis.MutationObserver = function(){ return {observe(){}}; };
globalThis.EventSource = undefined; globalThis.WebSocket = function(){}; globalThis.Audio = function(){};
globalThis.getComputedStyle = ()=>mk(); globalThis.addEventListener = ()=>{}; globalThis.performance = {now:()=>0};
globalThis.sessionStorage = globalThis.localStorage; globalThis.crypto = {getRandomValues:a=>a};
const pending = []; globalThis.setTimeout = (f)=>{ pending.push(f); return 0; };
try { (0, eval)(require('fs').readFileSync(0, 'utf8')); } catch(e) { console.log('THROW', e && e.message); }
for (let i = 0; i < 50 && pending.length; i++) { const f = pending.shift(); try { f(); } catch(e) { console.log('TIMER', e && e.message); } }
console.log('END', globalThis.__END__);
""".replace("MODE", json.dumps(mode))
        r = subprocess.run(["node", "-e", stub], input=body, capture_output=True, text=True, timeout=60)
        return r.stdout

    def test_reaches_the_end(self):
        for mode in ("classic", "3d"):
            out = self.run_page(mode)
            self.assertIn("END function function function", out, mode + ": " + out[-600:])
            self.assertNotIn("TIMER", out, mode + ": " + out[-600:])


CITY = r"""
const fs = require('fs');
const window = {HQV: {}};
for (const f of process.argv.slice(1)) new Function('window', fs.readFileSync(f, 'utf8'))(window);
const lay = window.HQV.hqCityLayout, nm = window.HQV.hqCityName;
const list = [{userId: 'c'}, {userId: 'a', isYou: true}, {userId: 'b'}, null, {userId: 7}];
const L1 = lay(list), L2 = lay(list.slice().reverse());
const many = lay(Array.from({length: 60}, (_, i) => ({userId: 'u' + String(i).padStart(2, '0'), isYou: i === 59})));
const blocks = L => [L.plaza].concat(L.lots.map(l => l.block), L.views.map(l => l.block)).map(b => b.x + ',' + b.z);
const lot = L1.lots[0];
const ring = window.HQV.hqBaseViewRing();
let minGap = 1e9;
ring.forEach((a, i) => ring.forEach((b, j) => { if (i < j) minGap = Math.min(minGap, Math.hypot(a.x - b.x, a.z - b.z)); }));
console.log(JSON.stringify({
  order: L1.lots.map(l => l.h.userId), same: JSON.stringify(L1) === JSON.stringify(L2), grid: [L1.cols, L1.rows],
  views: L1.views.map(l => l.v.view), uniqueBlocks: new Set(blocks(L1)).size === blocks(L1).length && new Set(blocks(many)).size === blocks(many).length,
  manyN: many.lots.length, manyYou: many.lots.some(l => l.h.isYou), manyGrid: [many.cols, many.rows],
  nearPlaza: Math.hypot(L1.lots[0].block.x - L1.plaza.x, L1.lots[0].block.z - L1.plaza.z) <= Math.hypot(L1.views[12].block.x - L1.plaza.x, L1.views[12].block.z - L1.plaza.z),
  doorFacesRoad: lot.door.x === lot.x && lot.door.z > lot.z && lot.rot === 0 && L1.views.every(l => l.door.z > l.z),
  ring: ring.length, ringMinGap: Math.round(minGap), ringInside: ring.every(r => Math.hypot(r.x, r.z) + 5 < 46),
  ringClear: ring.every(r => !(r.x > 22 && Math.abs(r.z) < 12) && !(r.x < -21 && Math.abs(r.z) < 14) && !(r.z < -22 && Math.abs(r.x) < 14) && !(Math.abs(r.x) < 5 && r.z > 15)),
  names: [nm({look: {sign: '  Owl Works '}, trainerName: 'Ann'}), nm({displayName: 'Bob', look: {}}), nm({})]
}));
"""


LOT_EXTRAS = r"""
const fs = require('fs');
const window = {HQV: {}};
for (const f of process.argv.slice(1)) new Function('window', fs.readFileSync(f, 'utf8'))(window);
const X = window.HQV.hqLotExtras, blocked = window.HQV.hqLotExtraBlocked, colors = window.HQV.hqLotColors;
// the HQ building's footprint with both wings (games/hqbase.js blockedLocal, campus off)
const bld = (x, z) => (Math.abs(x) < 4.8 && Math.abs(z) < 3.8) || (Math.abs(x + 10.5) < 3.0 && Math.abs(z - 0.5) < 2.8)
  || (Math.abs(x - 10.5) < 3.0 && Math.abs(z - 0.5) < 2.8);
const pts = (cx, cz, hw, hd) => { const o = []; for (let i = 0; i <= 10; i++) for (let j = 0; j <= 10; j++) o.push([cx - hw + 2*hw*i/10, cz - hd + 2*hd*j/10]); return o; };
const G = X.garage, K = X.car, B = X.bike;
const garage = pts(G.x, G.z, G.w/2, G.d/2), car = pts(K.x, K.z, 0.5, 1.05);
const path = pts(0, 7.4, 1.5, 3.4);                       // the door (z 4.8) to the road (z 11)
console.log(JSON.stringify({
  clearOfBuilding: garage.concat(car).every(p => !bld(p[0], p[1])),
  onTheBlock: garage.concat(car).every(p => Math.abs(p[0]) < 17 && p[1] < 11 && p[1] > -15),
  pathFree: path.every(p => !blocked(p[0], p[1])),
  garageSolid: blocked(G.x, G.z) && blocked(K.x, K.z),
  bikeFree: !blocked(B.x, B.z) && !bld(B.x, B.z) && Math.abs(B.x) < 17 && B.z < 11,
  carOnDrive: Math.abs(K.x - G.x) < 0.01 && K.z > G.z + G.d/2,
  cols: colors({paint: '#112233', accent: '#ABCDEF'}), dflt: colors({paint: 'red', accent: '#12'}), none: colors(null)
}));
"""


@unittest.skipUnless(shutil.which("node"), "node not installed")
class ArenaCity(unittest.TestCase):
    """Arena City: every open HQ and every tab's building on a grid of blocks, the same layout for everyone."""

    def test_lot_extras(self):
        """Every HQ gets a garage, its car and its bike, in its own colours, clear of the building and the path."""
        r = subprocess.run(["node", "-e", LOT_EXTRAS, os.path.join(ROOT, "games", "hqbase.js"), os.path.join(ROOT, "games", "hqcity.js")],
                           capture_output=True, text=True, timeout=30)
        self.assertEqual(r.returncode, 0, r.stderr)
        got = json.loads(r.stdout)
        for k in ("clearOfBuilding", "onTheBlock", "pathFree", "garageSolid", "bikeFree", "carOnDrive"):
            self.assertTrue(got[k], k)
        self.assertEqual(got["cols"], {"paint": 0x112233, "accent": 0xABCDEF})
        self.assertEqual(got["dflt"], {"paint": 0x2a3c50, "accent": 0xffb347})   # same fallbacks as the building
        self.assertEqual(got["none"], got["dflt"])
        city = open(os.path.join(ROOT, "games", "hqcity.js"), encoding="utf-8").read()
        self.assertIn('lotExtras(L, g, lotColors(h.look));', city)                # on every HQ lot
        self.assertIn('bikes.push({g: Bm.g,', city)                               # its bike is ridable
        self.assertIn('if(L.h && lotExtraBlocked(lx, lz)) return true;', city)    # you walk round the garage and car

    def test_layout(self):
        r = subprocess.run(["node", "-e", CITY, os.path.join(ROOT, "games", "hqbase.js"), os.path.join(ROOT, "games", "hqcity.js")],
                           capture_output=True, text=True, timeout=30)
        self.assertEqual(r.returncode, 0, r.stderr)
        got = json.loads(r.stdout)
        self.assertEqual(got["order"], ["a", "b", "c"])          # by user id, whoever asks
        self.assertTrue(got["same"])
        self.assertEqual(got["grid"], [5, 4])                     # 3 HQs + 13 tabs + the plaza = 17 blocks
        self.assertEqual(got["views"], ["live", "analytics", "pokedex", "gym", "quests", "village", "arena",
                                        "store", "cali", "valley", "music", "workshop", "compete"])
        self.assertTrue(got["uniqueBlocks"])                      # one building per block
        self.assertTrue(got["nearPlaza"])                         # HQs nearest the plaza, the tabs round them
        self.assertEqual(got["manyN"], 12)                        # capped, and you are always on it
        self.assertTrue(got["manyYou"])
        self.assertEqual(got["manyGrid"], [6, 5])
        self.assertTrue(got["doorFacesRoad"])
        self.assertEqual(got["ring"], 13)                         # every tab has a building round your Base
        self.assertGreaterEqual(got["ringMinGap"], 11)            # wider than a building (9 m), so none overlap
        self.assertTrue(got["ringInside"])
        self.assertTrue(got["ringClear"])                         # off the parking, kart track, annex and front path
        self.assertEqual(got["names"], ["Owl Works", "Bob's HQ", "Someone's HQ"])

    def test_page_wiring(self):
        html = dashboard.assemble_index()
        self.assertIn('return hqCityOn() ? "city" : "base";', html)
        # the street is walkable while private too (Jump to City): presence follows you there when paired
        self.assertIn('if(HQ3D.world==="city" && (hqCityOn() || (window.ARENA && ARENA.paired))) return "hq_city";', html)
        self.assertIn('function hqJumpCity(){', html)
        self.assertIn('label:"Jump to Arena City"', html)
        # riding is sent as a = 3 only to an Arena that lists the "ride" capability
        self.assertIn('if(a===3 && !hqNetCan("ride")) a=2;', html)
        self.assertIn('visit: function(uid){ hqVisit(uid, {from:"city"}); }', html)
        src = open(os.path.join(ROOT, "games", "hq3d.js"), encoding="utf-8").read()
        self.assertIn('d.view.indexOf("visit:") === 0', src)
        self.assertIn('return ride ? 3 : keyMove', src)                 # a rider reports a = 3
        city = open(os.path.join(ROOT, "games", "hqcity.js"), encoding="utf-8").read()
        self.assertIn('bikes = bikePark(P.x + 8.6, P.z);', city)         # the bike park beside the fountain
        self.assertIn('city.views.forEach(viewBuilding);', city)         # every tab has a building on the street
        self.assertIn('viewTitle: function(v){', html)                   # named as the page names its tabs
        self.assertIn('var calm = c.E.calm(), tt = calm ? 0 : t;', city)  # still water in Calm
