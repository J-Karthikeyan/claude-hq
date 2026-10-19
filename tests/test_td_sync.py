"""Tower Defense: the rules block in games/td.js mirrors backend/src/valley/td.rs.

Both files carry a `TD-RULES BEGIN ... END` block of one-line constants. Every
constant (numbers, the MULT4 table, the type and kind names, and each map's path
and tower slots) must hold the same values in the same order on both sides, or
solo play and the Arena referee would disagree."""
import os
import re
import unittest

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


def block(*parts):
    with open(os.path.join(ROOT, *parts), encoding="utf-8") as f:
        src = f.read()
    m = re.search(r"/\* TD-RULES BEGIN \*/(.*?)/\* TD-RULES END \*/", src, re.S)
    assert m, "no TD-RULES block in " + "/".join(parts)
    out = {}
    for line in m.group(1).splitlines():
        c = re.match(r"\s*(?:pub const|var)\s+([A-Z][A-Z0-9_]*)\b[^=]*=\s*(.*?);\s*$", line)
        if not c:
            continue
        name, val = c.group(1), c.group(2)
        if '"' in val:
            out[name] = re.findall(r'"([^"]*)"', val)
        else:
            out[name] = [int(x) for x in re.findall(r"-?\d+", val)]
    return out


class TdRulesSync(unittest.TestCase):
    def setUp(self):
        self.rs = block("backend", "src", "valley", "td.rs")
        self.js = block("games", "td.js")

    def test_every_constant_matches(self):
        self.assertEqual(sorted(self.rs), sorted(self.js))
        for k in self.rs:
            self.assertEqual(self.rs[k], self.js[k], k)

    def test_mult4_and_the_maps(self):
        self.assertEqual(self.rs["MULT4"], [0, 1, 2, 4, 8, 16])
        self.assertEqual(self.rs["MAP_IDS"], ["garden", "circuit", "datacenter"])
        for m in self.rs["MAP_IDS"]:
            path, slots = self.rs[m.upper() + "_PATH"], self.rs[m.upper() + "_SLOTS"]
            self.assertEqual(path, self.js[m.upper() + "_PATH"], m)
            self.assertEqual(slots, self.js[m.upper() + "_SLOTS"], m)
            self.assertTrue(10 <= len(slots) // 2 <= 14, m)
            xs, ys = path[0::2] + slots[0::2], path[1::2] + slots[1::2]
            self.assertTrue(all(0 <= x < 16 for x in xs) and all(0 <= y < 10 for y in ys), m)

    def test_the_kinds_carry_their_types(self):
        self.assertEqual(self.rs["KINDS"], ["grub", "stinger", "beetle", "moth", "glitch", "boss"])
        self.assertEqual(self.rs["KIND_T2"], ["", "Poison", "Steel", "Flying", "Electric", "Rock"])
        self.assertEqual(len(self.rs["KIND_HP"]), 6)


if __name__ == "__main__":
    unittest.main()
