//! Protocol version + capabilities per game, mirroring `PROTOCOL` in
//! backend/app/valley.py. The room welcome lists the games this Arena runs (only
//! the real-time ones here) so a client can tell "this Arena is too old for me"
//! from "I am too old for this Arena" before it joins; every game message also
//! carries `pv`. Keep in step with the Python table and games/multi.js.

use serde_json::{json, Value};

/// (game, version, capabilities) for every game this server referees, in
/// `valley.GAMES` order -- the order reaches the wire, because `arena_info`
/// builds its map by walking this and `serde_json` preserves insertion order.
///
/// Python gives every game `{"v": 1, "caps": []}` and overrides only kart
/// (valley.py:66-67), so a new game joins this table with v1 and no caps.
pub const GAMES: &[(&str, i64, &[&str])] = &[
    ("pond", 1, &[]),
    ("race", 1, &[]),
    ("duel", 1, &[]),
    ("mines", 1, &[]),
    ("farm", 1, &[]),
    ("golf", 1, &[]),
    ("kart", 2, &["scale", "tracks"]), // v2: geometry scale + server track list
    ("plat", 1, &[]),
    ("fps", 1, &[]),
    ("hq", 1, &["ride", "look", "skate"]),  // "ride": a = 3 (on a bike); "look": the 3D character (`c`); "skate": a = 4 + its trick `k` and height `y`
    ("type", 1, &[]),
    ("td", 1, &[]),
    ("bowl", 1, &[]),
];

/// The protocol version of game `g` (1 for a game this server does not know).
pub fn version(g: &str) -> i64 {
    GAMES.iter().find(|(n, _, _)| *n == g).map(|(_, v, _)| *v).unwrap_or(1)
}

/// Does this Arena referee game `g`? The one place anything asks, so a game
/// landing in [`GAMES`] opens every surface gated on it at once.
pub fn runs(g: &str) -> bool {
    GAMES.iter().any(|(n, _, _)| *n == g)
}

/// What this Arena runs, for the room welcome.
///
/// The `party` key is load-bearing, not decoration: the page gates its whole
/// Party Mode panel on it, and without it says "This Arena doesn't run parties
/// yet" however well party.rs works.
pub fn arena_info() -> Value {
    let mut games = serde_json::Map::new();
    for (g, v, caps) in GAMES {
        games.insert((*g).to_string(), json!({"v": v, "caps": caps}));
    }
    json!({"impl": "rs", "games": games,
           "party": {"v": 1, "order": crate::valley::party::ORDER},
           // Now Playing (/v1/music/now) and listen-along rooms ({"type": "music"}).
           "music": {"v": 1}, "maps": {"v": 1}})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_matches_python() {
        let info = arena_info();
        assert_eq!(info["impl"], "rs");
        assert_eq!(info["maps"], json!({"v": 1}));
        assert_eq!(info["games"]["kart"], json!({"v": 2, "caps": ["scale", "tracks"]}));
        assert_eq!(info["games"]["plat"], json!({"v": 1, "caps": []}));
        // HQ presence carries a = 3 (riding a bike in Arena City) and a = 4 (skating) only where these are listed.
        assert_eq!(info["games"]["hq"], json!({"v": 1, "caps": ["ride", "look", "skate"]}));
        assert_eq!(version("fps"), 1);
        assert_eq!(version("pond"), 1);
        // An unknown game still answers 1, as Python's PROTOCOL.get(g, {}) does.
        assert_eq!(version("nosuchgame"), 1);
    }

    #[test]
    fn the_welcome_advertises_every_game_in_pythons_order() {
        // valley.py:59. The order reaches the wire, so it is asserted on the
        // serialised form -- serde_json::Map's PartialEq ignores key order.
        let info = arena_info();
        let keys: Vec<&String> = info["games"].as_object().unwrap().keys().collect();
        assert_eq!(keys, ["pond", "race", "duel", "mines", "farm", "golf", "kart",
                          "plat", "fps", "hq", "type", "td", "bowl"]
                       .iter().map(|s| s.to_string()).collect::<Vec<_>>()
                       .iter().collect::<Vec<_>>());
        // Every game this Arena advertises, it referees.
        for (g, _, _) in GAMES {
            assert!(runs(g), "{g} is advertised but runs() denies it");
        }
    }

    #[test]
    fn the_welcome_carries_the_party_key_the_page_gates_on() {
        // valley.py:72. Without this the page hides Party Mode outright.
        let info = arena_info();
        assert_eq!(info["party"], json!({"v": 1, "order": ["kart", "plat", "fps", "golf"]}));
        assert_eq!(info["maps"], json!({"v": 1}));
    }
}
