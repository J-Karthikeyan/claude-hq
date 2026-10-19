//! The story campaign's tracks and levels (games/story.js, between the STORY-MAPS markers)
//! must be playable on this Arena: every MapDoc goes through the referees' own validators.
//! Test-only; the story itself never talks to the server.

use serde_json::Value;

/// Read at test time rather than include_str!: the release image never needs games/, and an
/// embedded path would have to be COPYed into it (tests/test_rust_build_context.py).
fn story_js() -> String {
    std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../games/story.js")).expect("games/story.js")
}

/// The JSON array between `/* STORY-MAPS BEGIN */` and `/* STORY-MAPS END */`.
fn story_maps() -> Vec<Value> {
    let (begin, end) = ("/* STORY-MAPS BEGIN */", "/* STORY-MAPS END */");
    let src = story_js();
    let a = src.find(begin).expect("STORY-MAPS BEGIN marker") + begin.len();
    let b = src[a..].find(end).expect("STORY-MAPS END marker") + a;
    let v: Value = serde_json::from_str(&src[a..b]).expect("the STORY-MAPS block is plain JSON");
    v.as_array().expect("the STORY-MAPS block is an array").clone()
}

#[test]
fn the_story_has_three_kart_tracks_and_two_plat_levels() {
    let maps = story_maps();
    let count = |k: &str| maps.iter().filter(|d| d["kind"] == k).count();
    assert_eq!((count("kart"), count("plat"), maps.len()), (3, 2, 5));
    for d in &maps {
        let keys: Vec<&String> = d.as_object().expect("a MapDoc is an object").keys().collect();
        assert_eq!(keys, ["kind", "v", "name", "data"], "{d}");
        assert_eq!(d["v"], 1);
        let name = d["name"].as_str().expect("a name");
        assert!((1..=32).contains(&name.chars().count()) && name.trim() == name, "{name}");
        assert!(!name.contains(['<', '>']) && !name.to_lowercase().contains("http"), "{name}");
        assert!(serde_json::to_string(&d["data"]).unwrap().len() <= 12 * 1024, "{name} is too big");
    }
}

#[test]
fn every_story_map_passes_the_arena_validators() {
    for d in story_maps() {
        let name = d["name"].as_str().unwrap_or("?").to_string();
        let canon = match d["kind"].as_str() {
            Some("kart") => crate::kart::validate_custom(&d["data"]),
            Some("plat") => crate::platformer::validate_custom(&d["data"]),
            k => panic!("{name}: unexpected kind {k:?}"),
        }
        .unwrap_or_else(|e| panic!("{name}: {e}"));
        // stored already canonical, so a gallery round-trip keeps the same board key
        assert_eq!(canon, d["data"], "{name} is not stored in canonical form");
    }
}

#[test]
fn story_maps_have_distinct_content() {
    let maps = story_maps();
    let mut seen: Vec<String> = maps.iter().map(|d| format!("{}:{}", d["kind"], d["data"])).collect();
    seen.sort();
    seen.dedup();
    assert_eq!(seen.len(), maps.len());
}
