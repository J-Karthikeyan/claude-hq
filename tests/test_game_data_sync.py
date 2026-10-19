"""Each 3D game's data file exists twice: the browser loads games/<game>/*.json, and the
Arena embeds backend/data/*.json at build time (include_str!). Both must be byte-identical,
or the browser predicts a different arena than the server referees.

(Golf's pair is checked in test_golf_sync, Pokemon's in test_pokedata.) Stdlib only."""
import os
import unittest

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

PAIRS = [
    ("games/fps/map.json", "backend/data/fps_map.json"),
    ("games/kart/tracks.json", "backend/data/kart_tracks.json"),
    ("games/platformer/levels.json", "backend/data/platformer_levels.json"),
    ("games/strike/map.json", "backend/data/strike_map.json"),
]


class GameDataSync(unittest.TestCase):
    def test_browser_and_arena_copies_match(self):
        for browser, arena in PAIRS:
            with self.subTest(browser=browser):
                with open(os.path.join(ROOT, browser), "rb") as a, open(os.path.join(ROOT, arena), "rb") as b:
                    self.assertEqual(a.read(), b.read(), f"copy {browser} to {arena}")


if __name__ == "__main__":
    unittest.main()
