"""Arena protocol versions (Wave 0): the client's CLIENT_PROTO in games/multi.js must
cover both servers' tables, and protoCheck must tell "update the Arena" from
"update Claude HQ". The JS runs in node (piped on stdin); skipped without node."""
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


# Games only the Rust Arena referees (HQ 2.5). The Python Arena is frozen, so
# these are filtered out of the Rust and client lists before comparing.
RUST_ONLY = ("td", "bowl")
# The one game where the two referees run different protocol versions: the Rust
# Arena is ahead of the frozen Python one — v2 flies the launcher's rockets,
# v3 also allows the sprint flag on pos; Python judges hitscan-only v1. The
# tables must agree everywhere else.
VERSION_DRIFT = {"strike": (1, 4)}  # v2 rockets, v3 sprint flag, v4 parachute drops (Python judges hitscan-only v1)


def client_proto():
    m = re.search(r"var CLIENT_PROTO = (\{[^}]*\});", read("games", "multi.js"))
    return json.loads(re.sub(r"(\w+):", r'"\1":', m.group(1)))


class ProtocolTables(unittest.TestCase):
    def test_client_covers_rust_versions(self):
        rs = read("backend", "src", "protocol.rs")
        cp = client_proto()
        rows = re.findall(r'\("(\w+)", (\d+), &\[', rs)
        self.assertTrue(rows, "no protocol rows parsed out of protocol.rs")
        for g, v in rows:
            self.assertTrue(cp[g][0] <= int(v) <= cp[g][1], g)


@unittest.skipUnless(shutil.which("node"), "node not installed")
class ProtoCheck(unittest.TestCase):
    SCRIPT = r"""
const fs = require('fs');
function stub(){ return new Proxy(function(){}, {get:(t,k)=>k===Symbol.toPrimitive?()=>'':stub(), apply:()=>stub(), set:()=>true}); }
const window = {HQV:{api:stub(), games:{}, register:()=>{}, onGame:null}};
window.ARENA = {arena:{impl:"rs", games:{kart:{v:2,caps:[]}, plat:{v:1,caps:[]}, fps:{v:3,caps:[]}}}};
new Function('window','document', fs.readFileSync(process.argv[2],'utf8'))(window, stub());
const pc = window.HQV.protoCheck, out = {};
out.kart = pc("kart"); out.golf = pc("golf"); out.fps = pc("fps");
window.ARENA.arena.games.plat.v = 0; out.plat = pc("plat");
window.ARENA.arena = null; out.legacy = pc("golf");
console.log(JSON.stringify(out));
"""

    def test_verdicts(self):
        r = subprocess.run(["node", "-", os.path.join(ROOT, "games", "multi.js")], input=self.SCRIPT,
                           capture_output=True, text=True, cwd=ROOT, timeout=30)
        self.assertEqual(r.returncode, 0, r.stderr)
        out = json.loads(r.stdout)
        self.assertIsNone(out["kart"])                       # in range
        # An Arena whose table omits golf -- stubbed above, not the real Rust
        # one, which runs it now. What is under test is the client's verdict.
        self.assertEqual(out["golf"]["who"], "arena")
        self.assertIn("Python Arena", out["golf"]["text"])
        self.assertEqual(out["fps"]["who"], "hq")            # server newer than client
        self.assertEqual(out["plat"]["who"], "arena")        # server older than client
        self.assertIsNone(out["legacy"])                     # pre-2.0 Arena: games cope alone


if __name__ == "__main__":
    unittest.main()
