"""The Overflow Vial ledger (vial.json): Apex surplus drips in, a spend consumes
oldest-first, and every deposit dies 24 h after it lands.

Stdlib only. Nothing here touches the network or the real dashboard directory.
"""
import glob
import json
import os
import stat
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

import dashboard  # noqa: E402

SID = "11111111-1111-4111-8111-111111111111"
NOW = 1790000000.0


class VialLedgerTests(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self._path = dashboard.VIAL_PATH
        dashboard.VIAL_PATH = os.path.join(self._tmp.name, "vial.json")

    def tearDown(self):
        dashboard.VIAL_PATH = self._path
        self._tmp.cleanup()

    def _raw(self):
        with open(dashboard.VIAL_PATH, encoding="utf-8") as f:
            return json.load(f)

    def test_missing_file_is_an_empty_vial(self):
        self.assertEqual(dashboard.vial_tick(NOW), 0)
        st = dashboard._vial_read(NOW)
        self.assertEqual(st, {"deposits": [], "spent": [], "seenPrompts": 0})

    def test_grant_deposits_and_balance_is_the_live_sum(self):
        dashboard.vial_grant(50, at=NOW - 10)
        dashboard.vial_grant(30, at=NOW - 5)
        self.assertEqual(dashboard.vial_tick(NOW), 80)

    def test_a_deposit_dies_exactly_one_window_after_it_lands(self):
        dashboard.vial_grant(100, at=NOW - dashboard.VIAL_WINDOW + 1)
        self.assertEqual(dashboard.vial_tick(NOW), 100)
        self.assertEqual(dashboard.vial_tick(NOW + 1), 0)   # aged out on the next tick

    def test_spend_consumes_oldest_first_and_leaves_the_split(self):
        dashboard.vial_grant(50, at=NOW - 20)   # oldest: dies first
        dashboard.vial_grant(50, at=NOW - 5)
        r = dashboard.vial_spend(SID, 1, at=NOW)   # stage 1 -> 2 costs 90
        self.assertTrue(r["ok"])
        self.assertEqual(r["cost"], 90)
        self.assertEqual(r["balance"], 10)
        st = dashboard._vial_read(NOW)
        self.assertEqual(st["deposits"], [{"at": NOW - 5, "xp": 10}])
        self.assertEqual(st["spent"][0]["sid"], SID)
        self.assertEqual(st["spent"][0]["stage"], 2)

    def test_an_expired_deposit_never_drags_a_spend_onto_fresh_xp(self):
        # The old deposit is the ONLY one a spend of 30 can take from; it then ages out.
        # If the spend still "bound" to it, the next spend would eat the fresh 100.
        dashboard.vial_grant(40, at=NOW - dashboard.VIAL_WINDOW + 2)  # still live now
        dashboard.vial_grant(100, at=NOW - 1)
        r = dashboard.vial_spend(SID, 0, at=NOW)   # Egg -> Hatchling is 30
        self.assertTrue(r["ok"])
        self.assertEqual(r["balance"], 110)         # 40-30 + 100
        later = NOW + 3                            # the 10-at leftover of the old deposit dies
        self.assertEqual(dashboard.vial_tick(later), 100)

    def test_spend_refused_when_the_vial_is_short(self):
        dashboard.vial_grant(20, at=NOW)
        r = dashboard.vial_spend(SID, 0, at=NOW)   # needs 30
        self.assertFalse(r["ok"])
        self.assertEqual(r["error"], "the vial is short")
        self.assertEqual(r["cost"], 30)
        self.assertEqual(r["balance"], 20)
        self.assertEqual(dashboard._vial_read(NOW)["spent"], [])

    def test_apex_and_bad_ids_are_refused(self):
        dashboard.vial_grant(5000, at=NOW)
        self.assertFalse(dashboard.vial_spend(SID, 4, at=NOW)["ok"])
        self.assertFalse(dashboard.vial_spend("../etc/passwd", 1, at=NOW)["ok"])
        self.assertFalse(dashboard.vial_spend(SID, True, at=NOW)["ok"])
        self.assertEqual(dashboard.vial_tick(NOW), 5000)

    def test_ledger_ceiling_trims_the_oldest_deposits(self):
        dashboard.vial_grant(dashboard.VIAL_MAX - 10, at=NOW - 50)   # 9990
        dashboard.vial_grant(20, at=NOW)                             # +20 = 10010
        self.assertEqual(dashboard.vial_tick(NOW), dashboard.VIAL_MAX)
        st = dashboard._vial_read(NOW)
        self.assertEqual(sum(d["xp"] for d in st["deposits"]), dashboard.VIAL_MAX)
        self.assertEqual(st["deposits"][0]["at"], NOW - 50)
        # the oldest is trimmed by exactly the overshoot (10), not dropped whole
        self.assertEqual(st["deposits"][0]["xp"], dashboard.VIAL_MAX - 20)
        self.assertEqual(st["deposits"][1], {"at": NOW, "xp": 20})

    def test_file_is_valid_private_json_with_no_temp_left(self):
        dashboard.vial_grant(10, at=NOW)
        data = self._raw()
        self.assertEqual(data["version"], 1)
        self.assertEqual(data["deposits"], [{"at": NOW, "xp": 10}])
        self.assertEqual(data["seenPrompts"], 0)
        self.assertEqual(stat.S_IMODE(os.stat(dashboard.VIAL_PATH).st_mode), 0o600)
        self.assertEqual(glob.glob(os.path.join(self._tmp.name, ".vial-*.tmp")), [])

    def test_corrupt_file_reads_empty_and_bad_entries_are_dropped(self):
        with open(dashboard.VIAL_PATH, "w") as f:
            f.write("{not json")
        self.assertEqual(dashboard.vial_tick(NOW), 0)
        with open(dashboard.VIAL_PATH, "w", encoding="utf-8") as f:
            f.write(json.dumps({"version": 1, "deposits": [
                {"at": NOW, "xp": 10},
                {"at": NOW, "xp": -5},
                {"at": NOW, "xp": float("inf")},
                {"at": NOW - dashboard.VIAL_WINDOW - 1, "xp": 10},
                "not a dict",
            ], "spent": [
                {"at": NOW, "xp": 3, "sid": SID, "stage": 1},
                {"at": True, "xp": 3},
            ], "seenPrompts": -4}))
        st = dashboard._vial_read(NOW)
        self.assertEqual(st["deposits"], [{"at": NOW, "xp": 10.0}])
        self.assertEqual(len(st["spent"]), 1)
        self.assertEqual(st["seenPrompts"], 0)


class SurplusDripTests(unittest.TestCase):
    """vial_drip() turns prompts past the Apex threshold into half-XP deposits,
    exactly once (the seenPrompts watermark is the idempotence)."""

    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self._path = dashboard.VIAL_PATH
        dashboard.VIAL_PATH = os.path.join(self._tmp.name, "vial.json")

    def tearDown(self):
        dashboard.VIAL_PATH = self._path
        self._tmp.cleanup()

    def test_surplus_drips_in_once(self):
        # 5 prompts past Apex -> (5*10)//2 = 25 XP
        self.assertEqual(dashboard.vial_drip(5, at=NOW), 25)
        self.assertEqual(dashboard.vial_tick(NOW + 1), 25)
        self.assertEqual(dashboard.vial_drip(5, at=NOW + 2), 0)  # no double-drip
        self.assertEqual(dashboard.vial_tick(NOW + 3), 25)

    def test_only_the_fresh_surplus_lands(self):
        self.assertEqual(dashboard.vial_drip(5, at=NOW), 25)
        self.assertEqual(dashboard.vial_drip(15, at=NOW + 1), 50)  # +10 prompts -> +50 XP
        self.assertEqual(dashboard.vial_tick(NOW + 2), 75)

    def test_no_surplus_no_deposit(self):
        self.assertEqual(dashboard.vial_drip(0, at=NOW), 0)
        self.assertEqual(dashboard.vial_drip(-3, at=NOW), 0)
        self.assertEqual(dashboard.vial_tick(NOW + 1), 0)
        self.assertEqual(dashboard._vial_read(NOW)["seenPrompts"], 0)

    def test_watermark_survives_the_round_trip(self):
        dashboard.vial_drip(5, at=NOW)
        st = dashboard._vial_read(NOW + 1)
        self.assertEqual(st["seenPrompts"], 5)
        self.assertEqual(st["deposits"], [{"at": NOW, "xp": 25}])


class SpendRouteTests(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self._path = dashboard.VIAL_PATH
        dashboard.VIAL_PATH = os.path.join(self._tmp.name, "vial.json")

    def tearDown(self):
        dashboard.VIAL_PATH = self._path
        self._tmp.cleanup()

    def test_spend_is_a_registered_post_path(self):
        self.assertIn("/api/vial/spend", dashboard.POST_PATHS)

    def test_spend_debits_the_ledger_and_invalidates_the_payload(self):
        dashboard.vial_grant(200, at=NOW)
        dashboard._invalidate_payload_memo()
        gen0 = dashboard._payload_memo["gen"]
        r = dashboard.vial_spend(SID, 0, at=NOW)
        self.assertTrue(r["ok"])
        self.assertEqual(r["cost"], 30)
        self.assertEqual(r["balance"], 170)
        self.assertGreater(dashboard._payload_memo["gen"], gen0)

    def test_stage_costs_are_the_half_efficiency_tax(self):
        self.assertEqual(dashboard.VIAL_STAGE_COST, {0: 30, 1: 90, 2: 180, 3: 400})


class VialSourcesTests(unittest.TestCase):
    """The village breakdown panel reads vial.src: the live drops and the time until each
    dies. A spend that PARTIALLY consumes a drop leaves the remainder at the original
    timestamp, so its ttl is still the death of the XP sitting in it."""

    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self._path = dashboard.VIAL_PATH
        dashboard.VIAL_PATH = os.path.join(self._tmp.name, "vial.json")

    def tearDown(self):
        dashboard.VIAL_PATH = self._path
        self._tmp.cleanup()

    def test_sources_list_the_live_drops_last_expiry_first(self):
        # Note: vial_grant/_vial_read(at) only keeps deposits with at <= at+60, so a grant
        # with a timestamp EARLIER than a prior one rewrites history out of existence —
        # grant in chronological order (as the real build_payload drip always does).
        dashboard.vial_grant(50, at=NOW - 82800)          # dies in 1 h
        dashboard.vial_grant(400, at=NOW - 7200)          # dies in 22 h
        dashboard.vial_grant(200, at=NOW - 3600)          # dies in 23 h
        dashboard.vial_drip(6, at=NOW)                    # fresh, dies in 24 h
        st = dashboard._vial_read(NOW)
        src = dashboard.vial_sources(st, NOW)
        self.assertEqual([r["xp"] for r in src], [30, 200, 400, 50])  # fresh first
        self.assertAlmostEqual(src[0]["ttl"], 86400.0, places=2)
        self.assertAlmostEqual(src[1]["ttl"], 82800.0, places=2)
        self.assertAlmostEqual(src[2]["ttl"], 79200.0, places=2)
        self.assertAlmostEqual(src[3]["ttl"], 3600.0, places=2)
        # the sum of the sources IS the balance
        self.assertEqual(sum(r["xp"] for r in src), 680)

    def test_a_partial_spend_keeps_the_die_time_of_the_leftover(self):
        dashboard.vial_grant(100, at=NOW - 3600)
        dashboard.vial_spend(SID, 0, at=NOW)              # consumes 30 of the drop
        st = dashboard._vial_read(NOW)
        src = dashboard.vial_sources(st, NOW)
        self.assertEqual(len(src), 1)
        self.assertEqual(src[0]["xp"], 70)
        self.assertAlmostEqual(src[0]["ttl"], 82800.0, places=2)


if __name__ == "__main__":
    unittest.main()
