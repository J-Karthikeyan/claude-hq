"""Strikezone: the browser and the Arena server must agree on the dock and the rules.

games/strike/map.json is mirrored byte-for-byte at backend/app/strike_map.json (the server
deploys backend/ only), and the shared geometry in games/strike.js (STRIKE-SHARED block) must
give the same answers as backend/app/strike.py: the browser predicts its own movement with
the same collision the server checks, and draws its shots with the same rays and spread
the server judges them with. The server module is stdlib-only, so it is loaded straight
from its file. Stdlib only; the JS half runs under node when it is installed (the script
goes over stdin: one argv string is capped at 128 KB on Linux)."""
import importlib.util
import json
import math
import os
import re
import shutil
import subprocess
import unittest

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
STRIKE_JS = os.path.join(ROOT, "games", "strike.js")
MAP = os.path.join(ROOT, "games", "strike", "map.json")
SERVER_MAP = os.path.join(ROOT, "backend", "app", "strike_map.json")
NODE = shutil.which("node")


def load_server():
    spec = importlib.util.spec_from_file_location("strike_server", os.path.join(ROOT, "backend", "app", "strike.py"))
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def read(p):
    with open(p, encoding="utf-8") as f:
        return f.read()


def block():
    return re.search(r"/\* STRIKE-SHARED BEGIN \*/(.*?)/\* STRIKE-SHARED END \*/", read(STRIKE_JS), re.S).group(1)


class StrikeSync(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.strike = load_server()

    def test_map_file_is_mirrored(self):
        with open(MAP, "rb") as a, open(SERVER_MAP, "rb") as b:
            self.assertEqual(a.read(), b.read(), "copy games/strike/map.json to backend/app/strike_map.json")

    def test_totem_clock_drops_and_the_mole_needs_a_bot(self):
        f = self.strike
        s = f.Strike()
        self.assertIsNone(s.start({"u0": {"id": "u0"}, "u1": {"id": "u1"}}, 5, 30, 0.0, 2))
        t = f.COUNTDOWN
        s.tick(t, True)
        self.assertEqual(s.phase, "round")
        totems = 0
        t += f.TOTEM_FIRST + 0.05
        for ev, _ in s.tick(t, True):
            if ev == "totem":
                totems += 1
        self.assertEqual(totems, 1, "exactly one totem lands TOTEM_FIRST into the round")
        n_map = len(s.m["pickups"])
        self.assertEqual(len(s.items), n_map + f.DROP_SLOTS + f.PACK_SLOTS,
                         "the drop rings exist past the map's packs, all match long")
        self.assertIn(s.kinds[n_map], f.TOTEM_KINDS, "the kind is one of the five")
        self.assertIsNone(s.items[n_map], "the fresh totem slot starts live")

    def test_drops_repeat_from_recycled_slots(self):
        # totems and loose packs are two independent clocks; each drop replaces the OLDEST
        # slot of its ring (taken or not), so drops keep coming and the map never fills up
        f = self.strike
        s = f.Strike()
        self.assertIsNone(s.start({"u0": {"id": "u0"}, "u1": {"id": "u1"}}, 5, 30, 0.0, 2))
        t = f.COUNTDOWN
        s.tick(t, True)
        n_map = len(s.m["pickups"])
        totem_slots = set()
        pack_slots = set()
        # run 4 totem gaps + 5 pack gaps, ticking at HZ like the real loop: 4 totems over
        # 3 slots, 5 packs over 2 slots
        end = t + f.TOTEM_FIRST + f.TOTEM_GAP * 4 + f.PACK_GAP * 5 + 0.5
        while t < end:
            t += 1.0 / f.HZ
            for ev, d in s.tick(t, True):
                if ev == "totem":
                    totem_slots.add(d["i"])
                elif ev == "pack":
                    pack_slots.add(d["i"])
        self.assertEqual(len(s.items), n_map + f.DROP_SLOTS + f.PACK_SLOTS,
                         "the slot list never grows past the rings")
        self.assertTrue(all(n_map <= i < n_map + f.DROP_SLOTS for i in totem_slots),
                        "totems land only in the totem ring")
        self.assertTrue(all(n_map + f.DROP_SLOTS <= i < n_map + f.DROP_SLOTS + f.PACK_SLOTS for i in pack_slots),
                        "loose packs land only in the pack ring")
        self.assertEqual(len(totem_slots | pack_slots), f.DROP_SLOTS + f.PACK_SLOTS,
                         "every ring slot is used")

    def test_a_loose_pack_refills_the_taker(self):
        # a loose health/ammo drop behaves like the map's own packs when walked over
        f = self.strike
        s = f.Strike()
        self.assertIsNone(s.start({"u0": {"id": "u0"}, "u1": {"id": "u1"}}, 5, 30, 0.0, 2))
        t = f.COUNTDOWN
        s.tick(t, True)
        t += f.PACK_FIRST + 0.05
        evs = s.tick(t, True)
        pev = [d for ev, d in evs if ev == "pack"]
        self.assertEqual(len(pev), 1, "the first loose pack lands PACK_FIRST into the round")
        n_map = len(s.m["pickups"])
        self.assertIn(pev[0]["kind"], ("health", "ammo"))
        self.assertTrue(n_map + f.DROP_SLOTS <= pev[0]["i"] < n_map + f.DROP_SLOTS + f.PACK_SLOTS)
        # stand on it, hurt first so a health pack has room to heal
        p = s.players["u0"]
        p.update(x=pev[0]["x"] / 100.0, y=0.0, z=pev[0]["z"] / 100.0, r=0, pt=0, protect=0.0,
                 spawn_t=t - 5.0, q=0, q0=0, t0=t, qmax=0)
        p["hist"].clear()
        p["hp"] = 40
        s.pos("u0", {"x": pev[0]["x"], "y": 0, "z": pev[0]["z"], "r": 0, "p": 0, "q": 0, "e": p["life"]},
              t + 0.1)
        if pev[0]["kind"] == "health":
            self.assertEqual(p["hp"], 90, "the loose health pack healed its taker")

    def test_charmed_kills_pay_the_charmer(self):
        f = self.strike
        s = f.Strike()
        self.assertIsNone(s.start({"u0": {"id": "u0"}, "u1": {"id": "u1"}}, 5, 30, 0.0, 2))
        t = f.COUNTDOWN
        s.tick(t, True)
        s.players["u0"]["charm_until"] = t + f.CHARM
        before = list(s.scores)
        s._kill("u0", "u1", 0, False, t)
        self.assertEqual(s.scores[1], before[1] + 1, "the mole's kill pays the charmer's (cyan) team")
        self.assertEqual(s.scores[0], before[0], "amber does not keep the point its charmed shooter scored")

    def test_a_frozen_bot_holds_still(self):
        f = self.strike
        s = f.Strike()
        self.assertIsNone(s.start({"u0": {"id": "u0"}, "u1": {"id": "u1"}}, 5, 30, 0.0, 2))
        t = f.COUNTDOWN
        s.tick(t, True)
        bot = next(u for u, p in s.players.items() if p["bot"])
        x, z = s.players[bot]["x"], s.players[bot]["z"]
        s.players[bot]["freeze_until"] = t + f.FREEZE
        for _ in range(50):
            t += 1.0 / f.HZ
            s.tick(t, True)
        self.assertEqual((s.players[bot]["x"], s.players[bot]["z"]), (x, z),
                         "the paralysed bot does not move while frozen")

    def test_the_shield_totem_blocks_damage_but_not_their_own_shots(self):
        f = self.strike
        s = f.Strike()
        self.assertIsNone(s.start({"u0": {"id": "u0"}, "u1": {"id": "u1"}}, 5, 30, 0.0, 2))
        t = f.COUNTDOWN
        s.tick(t, True)
        # u0 and u1 face each other 6 m apart; u1 takes the shield totem
        for uid, x, z, r in (("u0", -6.0, -9.0, 18000), ("u1", -6.0, -3.0, 0)):
            p = s.players[uid]
            p.update(x=x, y=0.0, z=z, r=r, pt=0, protect=0.0, spawn_t=t - 5.0,
                     q=0, q0=0, t0=t, qmax=0)
            p["hist"].clear()
        s.players["u1"]["shield_totem_until"] = t + f.SHIELD_TOTEM
        hp0, hp0u0 = s.players["u1"]["hp"], s.players["u0"]["hp"]
        o = [s.players["u0"]["x"], s.players["u0"]["y"] + f.EYE, s.players["u0"]["z"]]
        # u0 fires down the lane at u1: the bubble eats every point of it
        s.fire("u0", {"w": 0, "ox": round(o[0] * 100), "oy": round(o[1] * 100), "oz": round(o[2] * 100),
                      "r": 18000, "p": 0, "q": 10, "ip": 0, "life": s.players["u0"]["life"]}, t + 0.1)
        self.assertEqual(s.players["u1"]["hp"], hp0, "nothing hurts the shield totem's taker")
        # but the shielded player shoots back at full damage
        o2 = [s.players["u1"]["x"], s.players["u1"]["y"] + f.EYE, s.players["u1"]["z"]]
        s.fire("u1", {"w": 0, "ox": round(o2[0] * 100), "oy": round(o2[1] * 100), "oz": round(o2[2] * 100),
                      "r": 0, "p": 0, "q": 20, "ip": 0, "life": s.players["u1"]["life"]}, t + 0.2)
        self.assertLess(s.players["u0"]["hp"], hp0u0, "the shielded player still damages others")
        # and the window lapses: shots land again once the bubble pops
        s.players["u1"]["shield_totem_until"] = t
        s.fire("u0", {"w": 0, "ox": round(o[0] * 100), "oy": round(o[1] * 100), "oz": round(o[2] * 100),
                      "r": 18000, "p": 0, "q": 30, "ip": 0, "life": s.players["u0"]["life"]}, t + 0.3)
        self.assertLess(s.players["u1"]["hp"], hp0, "the bubble pops when the window ends")

    def test_the_movement_totems_open_windows_and_relax_the_referee(self):
        # wings/leap/speed are the taker's own buffs: the take sets the window, and while
        # it runs the referee relaxes exactly what the browser's physics will send
        f = self.strike
        s = f.Strike()
        self.assertIsNone(s.start({"u0": {"id": "u0"}, "u1": {"id": "u1"}}, 5, 30, 0.0, 2))
        t = f.COUNTDOWN
        s.tick(t, True)
        p = s.players["u0"]
        p.update(x=0.0, y=0.0, z=-8.0, r=0, pt=0, protect=0.0, spawn_t=t - 5.0,
                 q=0, q0=0, t0=t, qmax=0)
        p["hist"].clear()
        p["air_at"] = t
        # the take itself: each window opens on the server clock. wings is special: the
        # cruise boundary is the shown WINGS seconds, but the judged window runs past it —
        # rise + cruise + the automatic glide down is one judged flight (the client flies
        # all three without input), so the referee must not "teleport" the landing.
        for kind, attr, dur in (("leap", "leap_until", f.LEAP), ("speed", "speed_until", f.SPEED)):
            self.assertTrue(s._take_totem("u0", p, kind, t + 1.0), kind + " applies to its taker")
            self.assertGreater(p[attr], t + 1.0, kind + " opens a window")
            self.assertLessEqual(p[attr], t + 1.0 + dur, kind + " for exactly its duration")
            p[attr] = 0.0
        self.assertTrue(s._take_totem("u0", p, "wings", t + 1.0), "wings applies to its taker")
        self.assertEqual(p["wings_fly"], t + 1.0 + f.WINGS, "cruise = the shown window")
        self.assertEqual(p["wings_until"], t + 1.0 + f.WINGS + f.WINGS_LAND, "judged = cruise + the glide down")
        p["wings_until"] = p["wings_fly"] = 0.0
        # a legal walk-speed move as the baseline
        ok, fix = s.pos("u0", {"x": 0, "y": 0, "z": -800, "r": 0, "p": 0, "q": 30, "e": p["life"]}, t + 0.3)
        self.assertTrue(ok and fix is None)
        # speed: 6.5 m in 0.3 s is past even the sprint cap — refused bare, allowed dashing
        ok, _ = s.pos("u0", {"x": 650, "y": 0, "z": -800, "r": 0, "p": 0, "q": 60, "e": p["life"]}, t + 0.6)
        self.assertFalse(ok, "a dash-speed burst without the speed totem is refused")
        p["x"], p["z"] = 0.0, -8.0
        p["speed_until"] = t + 10.0
        ok, fix = s.pos("u0", {"x": 650, "y": 0, "z": -800, "r": 0, "p": 0, "q": 90, "e": p["life"]}, t + 0.9)
        self.assertTrue(ok and fix is None, "the speed totem's window buys the dash cap")
        # leap: 4.5 m up in 0.3 s is past the plain gate — refused bare, allowed leaping
        p.update(x=0.0, y=0.0, z=-8.0)
        p["hist"].clear()
        p["leap_until"] = t + 10.0
        ok, fix = s.pos("u0", {"x": 0, "y": 450, "z": -800, "r": 0, "p": 0, "q": 120, "e": p["life"]}, t + 1.2)
        self.assertTrue(ok and fix is None, "the leap totem's window buys the tall jump")
        p["leap_until"] = 0.0
        p.update(x=0.0, y=0.0, z=-8.0)
        p["hist"].clear()
        ok, _ = s.pos("u0", {"x": 0, "y": 450, "z": -800, "r": 0, "p": 0, "q": 150, "e": p["life"]}, t + 1.5)
        self.assertFalse(ok, "a plain life cannot rise like a leaper")
        # wings: a hover past MAX_AIR (2 s) is refused bare, allowed while winged
        p.update(x=0.0, y=4.0, z=-8.0)
        p["hist"].clear()
        p["air_at"] = t + 2.0
        p["wings_until"] = 0.0
        ok, _ = s.pos("u0", {"x": 0, "y": 400, "z": -800, "r": 0, "p": 0, "q": 450, "e": p["life"]}, t + 4.5)
        self.assertFalse(ok, "a hover past MAX_AIR is refused without the wings totem")
        p["wings_until"] = t + 30.0
        ok, fix = s.pos("u0", {"x": 0, "y": 400, "z": -800, "r": 0, "p": 0, "q": 460, "e": p["life"]}, t + 4.6)
        self.assertTrue(ok and fix is None, "the wings totem parks the air clock")
        # the descent past the cruise boundary is still judged (the glide-down after the
        # shown window): a mid-air hover at cruise + 3 s of descent time is legal while
        # wings_until still runs, refused the moment it lapses
        p.update(x=0.0, y=7.0, z=-8.0)
        p["hist"].clear()
        p["air_at"] = t + 11.0   # airborne since just after cruise began
        s2 = f.Strike()
        s2.start({"u0": {"id": "u0"}, "u1": {"id": "u1"}}, 5, 30, 0.0, 2)
        s2.tick(t, True)
        p2 = s2.players["u0"]
        p2.update(x=0.0, y=9.0, z=-8.0, r=0, pt=0, protect=0.0, spawn_t=t - 5.0,
                  q=0, q0=0, t0=t, qmax=0)
        p2["hist"].clear()
        p2["air_at"] = t + 11.0
        s2._take_totem("u0", p2, "wings", t + 1.0)   # judged until t+16, cruise until t+11
        # gliding down at cruise + 3 s (14 s of air): legal while wings_until runs
        ok, fix = s2.pos("u0", {"x": 0, "y": 800, "z": -800, "r": 0, "p": 0, "q": 1410, "e": p2["life"]}, t + 14.1)
        self.assertTrue(ok and fix is None, "the automatic glide down is still wings flight")
        # once the judged window closes, the same mid-air stay is refused
        ok, _ = s2.pos("u0", {"x": 0, "y": 700, "z": -800, "r": 0, "p": 0, "q": 1620, "e": p2["life"]}, t + 16.2)
        self.assertFalse(ok, "past wings + WINGS_LAND there is no more flight")
        # and a respawn starts with every movement window closed
        p["wings_until"] = p["wings_fly"] = p["leap_until"] = p["speed_until"] = t + 30.0
        s._place(p, 0, t + 5.0)
        self.assertEqual((p["wings_until"], p["wings_fly"], p["leap_until"], p["speed_until"]), (0.0, 0.0, 0.0, 0.0))

    def test_the_teleport_totem_plants_the_taker_on_open_ground(self):
        # the teleport is instant: no window, the referee itself moves the body so the
        # next pos frame starts from the destination (no "teleport" correction), and the
        # pick event carries the destination so the taker's client can snap
        f = self.strike
        s = f.Strike()
        self.assertIsNone(s.start({"u0": {"id": "u0"}, "u1": {"id": "u1"}}, 5, 30, 0.0, 2))
        t = f.COUNTDOWN
        s.tick(t, True)
        p = s.players["u0"]
        x0, z0 = p["x"], p["z"]
        self.assertTrue(s._take_totem("u0", p, "teleport", t + 1.0))
        self.assertTrue((p["x"], p["z"]) != (x0, z0), "the taker moved somewhere else")
        self.assertEqual(p["y"], 0.0, "always on the ground, never in the air")
        self.assertGreaterEqual(p["x"], -17.5, "inside the drop scatter's x range")
        self.assertLessEqual(p["x"], 17.5)
        self.assertGreaterEqual(p["z"], -10.5, "inside the drop scatter's z range")
        self.assertLessEqual(p["z"], 10.5)
        self.assertLess(f.overlaps(s.m, p["x"], 0.1, p["z"], 0.3), 0, "never inside a box")
        self.assertEqual(p["air_at"], t + 1.0, "the air clock restarts at the destination")
        self.assertEqual(p["hist"][-1], (t + 1.0, p["x"], 0.0, p["z"]), "history points at the destination")
        # two hops in a row land somewhere fresh (the seed advances with the take counter)
        px, pz = p["x"], p["z"]
        self.assertTrue(s._take_totem("u0", p, "teleport", t + 2.0))
        self.assertTrue((p["x"], p["z"]) != (px, pz), "the second hop lands somewhere fresh")
        # and a walk-speed pos frame from the destination is accepted (no correction)
        ok, fix = s.pos("u0", {"x": round(p["x"] * 100), "y": 0, "z": round(p["z"] * 100),
                               "r": 0, "p": 0, "q": 100, "e": p["life"]}, t + 2.1)
        self.assertTrue(ok and fix is None, "the next frame starts from the destination: {fix}".format(fix=fix))

    def test_the_host_picks_one_bot_skill_for_both_sides(self):
        # MP lobbies fill short sides with bots on BOTH teams, so one skill level applies
        # to the whole match — never a sharper filler's side against a duller's
        f = self.strike
        s = f.Strike()
        self.assertIsNone(s.start({"u0": {"id": "u0"}, "u1": {"id": "u1"}}, 5, 30, 0.0, 3, "hard"))
        self.assertEqual(s.bot_diff, "hard")
        self.assertEqual((s.bot_aim, s.bot_react, s.bot_turn, s.bot_rate),
                         (f.BOT_DIFFS["hard"]["aim"], f.BOT_DIFFS["hard"]["react"],
                          f.BOT_DIFFS["hard"]["turn"], f.BOT_DIFFS["hard"]["rate"]))
        # an unknown id — including a crafted wire one — quietly falls back to normal
        s2 = f.Strike()
        s2.start({"u0": {"id": "u0"}, "u1": {"id": "u1"}}, 5, 30, 0.0, 3, "nightmare")
        self.assertEqual(s2.bot_diff, "normal")
        self.assertEqual(s2.bot_aim, 1.0)
        # and a match started without a diff is normal (the host didn't pick one)
        s3 = f.Strike()
        s3.start({"u0": {"id": "u0"}, "u1": {"id": "u1"}}, 5, 30, 0.0, 3)
        self.assertEqual(s3.bot_diff, "normal")

    def test_constants_match(self):
        src = block()
        for name in ("R", "H", "EYE", "STEP_H", "RUN", "ACCEL", "AIR_ACCEL", "GRAVITY", "JUMP_V", "FALL_MAX",
                     "BODY_R", "BODY_H", "HEAD_R", "HEAD_TOP"):
            m = re.search(r"\b%s\s*=\s*([0-9.]+)" % name, src)
            self.assertIsNotNone(m, name)
            self.assertEqual(float(m.group(1)), float(getattr(self.strike, name)), name)
        js = read(STRIKE_JS)
        self.assertLessEqual(float(re.search(r"\bRUN\s*=\s*([0-9.]+)", src).group(1)), self.strike.MAX_SPEED)
        self.assertLessEqual(float(re.search(r"\bINTERP\s*=\s*([0-9.]+)", js).group(1)) +
                             float(re.search(r"\bJIT_MAX\s*=\s*([0-9.]+)", js).group(1)), self.strike.IP_CAP)
        self.assertEqual(float(re.search(r"\bSWITCH\s*=\s*([0-9.]+)", js).group(1)), self.strike.SWITCH)
        self.assertEqual(float(re.search(r"\bRESPAWN\s*=\s*([0-9.]+)", js).group(1)), self.strike.RESPAWN)
        # the host's choices (Teams / First to) must be exactly what the server accepts
        self.assertEqual(json.loads(re.search(r"var MINUTES = (\[[^\]]*\]), KILLS = (\[[^\]]*\]), TEAM_SIZES = (\[[^\]]*\]);", js).group(1)),
                         list(self.strike.MINUTES))
        self.assertEqual(json.loads(re.search(r"var MINUTES = (\[[^\]]*\]), KILLS = (\[[^\]]*\]), TEAM_SIZES = (\[[^\]]*\]);", js).group(2)),
                         list(self.strike.KILLS))
        self.assertEqual(json.loads(re.search(r"var MINUTES = (\[[^\]]*\]), KILLS = (\[[^\]]*\]), TEAM_SIZES = (\[[^\]]*\]);", js).group(3)),
                         list(self.strike.TEAM_SIZES))

    def test_weapons_match(self):
        # FS.WEAPONS under node is the honest parse (regex over JS object literals is not)
        self.skipTest("covered by test_shared_functions_match_under_node (FS.WEAPONS)")

    @unittest.skipUnless(NODE, "node not installed")
    def test_shared_functions_match_under_node(self):
        f = self.strike
        m = f.MAP
        data = json.loads(read(MAP))
        # rays: from a grid of eyes in many directions, against the map and a player
        rays = []
        for i in range(160):
            o = (-20 + (i * 7.3) % 40, 0.3 + (i * 1.7) % 4, -14 + (i * 3.1) % 28)
            yaw, pitch = (i * 37.7) % 360 - 180, (i * 11.3) % 120 - 60
            rays.append([o, yaw, pitch, ((i * 5.1) % 30 - 15, (i % 3) * 0.7, (i * 2.3) % 30 - 15)])
        # movement: a few runners steering through the dock, jumping now and then
        runs = []
        for k in range(6):
            sx, sy, sz, _ = m["spawns"][k]
            steps = []
            for j in range(900):
                a = (k * 1.3 + j * 0.011) % (2 * math.pi)
                steps.append([math.sin(a), -math.cos(a), j % 97 == 0])
            runs.append([[sx, sy, sz], steps])
        script = block() + """
var input = %s, m = FS.compileMap(input.data), out = {rays: [], runs: [], spread: [], mb: [], over: [], weapons: []};
FS.WEAPONS.forEach(function(w){ out.weapons.push(w); });
input.rays.forEach(function(r){
  var d = FS.dirOf(r[1], r[2]);
  out.rays.push([d, FS.rayMap(m, r[0], d, 80), FS.rayPlayer(r[0], d, r[3][0], r[3][1], r[3][2], 80)]);
});
input.runs.forEach(function(rn){
  var s = {x: rn[0][0], y: rn[0][1], z: rn[0][2], vx: 0, vy: 0, vz: 0, g: true}, trace = [];
  rn[1].forEach(function(st, j){ FS.move(m, s, st[0], st[1], st[2], 1/120); if(j %% 30 === 0) trace.push([s.x, s.y, s.z, s.g]); });
  out.runs.push(trace);
});
for(var n = 0; n < 200; n++){ out.spread.push([FS.spreadOf(n, 0), FS.spreadOf(n, 1)]); out.mb.push(FS.mb32(n * 7919)); }
for(var x = -21; x <= 21; x += 1.5) for(var z = -15; z <= 15; z += 1.5) out.over.push(FS.overlaps(m, x, 0.2, z, 0.05));
console.log(JSON.stringify(out));
""" % json.dumps({"data": data, "rays": rays, "runs": runs})
        res = subprocess.run([NODE, "-"], input=script, capture_output=True, text=True, timeout=120)
        self.assertEqual(res.returncode, 0, res.stderr)
        got = json.loads(res.stdout)
        for i, w in enumerate(got["weapons"]):
            self.assertEqual(w["id"], self.strike.WEAPONS[i]["id"])
            self.assertEqual(w["name"], self.strike.WEAPONS[i]["name"])
            for k in ("interval", "damage", "head", "mag", "reserve", "reload", "spread", "range", "pack"):
                self.assertEqual(float(w[k]), float(self.strike.WEAPONS[i][k]), (w["id"], k))
        for (o, yaw, pitch, tp), (d, tm, tpl) in zip(rays, got["rays"]):
            want = f.dir_of(yaw, pitch)
            for a, b in zip(d, want):
                self.assertAlmostEqual(a, b, places=9)
            self.assertAlmostEqual(tm, f.ray_map(m, o, want, 80), places=6)
            wp = f.ray_player(o, want, tp[0], tp[1], tp[2], 80)
            if wp is None:
                self.assertIsNone(tpl)
            else:
                self.assertAlmostEqual(tpl[0], wp[0], places=6)
                self.assertEqual(tpl[1], wp[1])
        hits = sum(1 for _, _, tpl in got["rays"] if tpl is not None)
        self.assertGreater(len(rays), hits)
        for (start, steps), trace in zip(runs, got["runs"]):
            s = {"x": start[0], "y": start[1], "z": start[2], "vx": 0.0, "vy": 0.0, "vz": 0.0, "g": True}
            want = []
            for j, st in enumerate(steps):
                f.move(m, s, st[0], st[1], st[2], 1 / 120)
                if j % 30 == 0:
                    want.append([s["x"], s["y"], s["z"], s["g"]])
            for a, b in zip(trace, want):
                for u, v in zip(a[:3], b[:3]):
                    self.assertAlmostEqual(u, v, places=6)
                self.assertEqual(a[3], b[3])
        for n, (a, b) in enumerate(got["spread"]):
            self.assertEqual(tuple(a), f.spread_of(n, 0))
            self.assertEqual(tuple(b), f.spread_of(n, 1))
            self.assertEqual(got["mb"][n], f.mb32(n * 7919))
        k = 0
        x = -21.0
        while x <= 21:
            z = -15.0
            while z <= 15:
                self.assertEqual(got["over"][k], f.overlaps(m, x, 0.2, z, 0.05), (x, z))
                k += 1
                z += 1.5
            x += 1.5


if __name__ == "__main__":
    unittest.main()
