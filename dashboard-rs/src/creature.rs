//! Creatures: the original 48 species, stable per-session picks, evolution
//! stages, the per-install shiny roll, ranks and levels.

use crate::py;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

pub const CREATURES: [(&str, &str); 8] = [
    ("⚡🐭", "Pikachu"),
    ("🔥🦎", "Charmander"),
    ("💧🐢", "Squirtle"),
    ("🌱🐸", "Bulbasaur"),
    ("💨🐉", "Dragonite"),
    ("💦🦆", "Psyduck"),
    ("🌙🦊", "Umbreon"),
    ("🪨🐛", "Geodude"),
];

pub const SPECIES_NAMES: [&str; 48] = [
    "Emberpup", "Aquafin", "Sprigling", "Voltkit", "Mystifox", "Boulderbug",
    "Frostnib", "Umbracat", "Pebblemol", "Cindermouse", "Tidewhorl", "Mossling",
    "Sparkfly", "Dreamowl", "Craghorn", "Glacimini", "Nocturnip", "Duskmoth",
    "Lumazee", "Brambeak", "Coralux", "Zaptadpole", "Gloamkit", "Terrapawn",
    "Flarelynx", "Marisprite", "Thornvale", "Ionbuzz", "Chronowisp", "Dracowyrm",
    "Rimepix", "Faewhisk",
    "Cindertail", "Brinewisp", "Fernbud", "Voltling", "Psybloom", "Stonecrag",
    "Frostmane", "Shadowpip", "Wispkit", "Drakelet", "Pixiewing", "Magmaturtle",
    "Rippletusk", "Vinecoil", "Sparkmoth", "Glimmerfawn",
];

pub const SPECIES_TYPES: [&str; 48] = [
    "fire", "water", "grass", "electric", "psychic", "rock",
    "ice", "shadow", "rock", "fire", "water", "grass",
    "electric", "psychic", "rock", "ice", "shadow", "shadow",
    "psychic", "grass", "water", "electric", "shadow", "normal",
    "fire", "water", "grass", "electric", "psychic", "dragon",
    "ice", "fairy",
    "fire", "water", "grass", "electric", "psychic", "rock",
    "ice", "shadow", "normal", "dragon", "fairy", "fire",
    "water", "grass", "electric", "fairy",
];

pub fn type_hue(t: &str) -> i64 {
    match t {
        "fire" => 10,
        "water" => 205,
        "grass" => 120,
        "electric" => 52,
        "psychic" => 285,
        "rock" => 30,
        "ice" => 190,
        "shadow" => 265,
        "normal" => 45,
        "dragon" => 250,
        "fairy" => 320,
        _ => 45,
    }
}

pub const STAGE_THRESHOLDS: [i64; 5] = [0, 3, 12, 30, 70];
pub const STAGE_NAMES: [&str; 5] = ["Egg", "Hatchling", "Juvenile", "Adult", "Apex"];

pub const RANKS: [(i64, &str); 7] = [
    (1, "Prompt Apprentice"),
    (3, "Prompt Adept"),
    (5, "Prompt Conjurer"),
    (8, "Prompt Sorcerer"),
    (12, "Prompt Archmage"),
    (18, "Prompt Ascendant"),
    (999, "Prompt Deity"),
];

fn sha256_hex(s: &str) -> String {
    hex::encode(Sha256::digest(s.as_bytes()))
}

/// int(hexdigest, 16) % m, computed over the digest bytes.
fn digest_mod(digest: &[u8], m: u64) -> u64 {
    let mut r: u128 = 0;
    for b in digest {
        r = (r * 256 + *b as u128) % m as u128;
    }
    r as u64
}

/// Stable 24-bit sprite seed for a species index.
pub fn species_seed(species: i64) -> i64 {
    let d = Sha256::digest(format!("nymonster:species:{}", species).as_bytes());
    ((d[29] as i64) << 16) | ((d[30] as i64) << 8) | d[31] as i64
}

/// Shiny is a stable per-species property (1 in 4 species) salted per install.
pub fn shiny_for_species(species: i64, salt: &str) -> bool {
    let sp = species.rem_euclid(48);
    let h = Sha256::digest(format!("nymonster:shiny:{}:{}", salt, sp).as_bytes());
    if digest_mod(&h, 6) == 0 {
        return true;
    }
    let h2 = Sha256::digest(format!("nymonster:shiny2:{}:{}", salt, sp).as_bytes());
    digest_mod(&h2, 10) == 0
}

/// int(sha256(sessionId).hexdigest(), 16) reduced mod m.
pub fn session_hash_mod(session_id: &str, m: u64) -> u64 {
    digest_mod(&Sha256::digest(session_id.as_bytes()), m)
}

/// (stage, stageName, stagePct) from a prompt count.
pub fn stage_for(prompt_count: i64) -> (usize, &'static str, f64) {
    let pc = prompt_count;
    let mut stage = 0;
    for (i, th) in STAGE_THRESHOLDS.iter().enumerate() {
        if pc >= *th {
            stage = i;
        }
    }
    let pct = if stage >= STAGE_THRESHOLDS.len() - 1 {
        1.0
    } else {
        let lo = STAGE_THRESHOLDS[stage];
        let hi = STAGE_THRESHOLDS[stage + 1];
        let p = if hi > lo { (pc - lo) as f64 / (hi - lo) as f64 } else { 1.0 };
        p.clamp(0.0, 1.0)
    };
    (stage, STAGE_NAMES[stage], py::round(pct, 4))
}

/// The creature object for a session (legacy emoji fields + the monster fields).
pub fn creature_for(session_id: &str, prompt_count: i64, salt: &str) -> serde_json::Map<String, Value> {
    let (emoji, name) = CREATURES[session_hash_mod(session_id, 8) as usize];
    let hue = session_hash_mod(session_id, 360) as i64;
    let species = session_hash_mod(session_id, 48) as i64;
    let shiny = shiny_for_species(species, salt);
    let stype = SPECIES_TYPES[species as usize];
    let (stage, stage_name, stage_pct) = stage_for(prompt_count);
    match json!({
        "emoji": emoji, "name": name, "hue": hue,
        "species": species,
        "speciesName": SPECIES_NAMES[species as usize],
        "type": stype,
        "seed": species_seed(species),
        "typeHue": type_hue(stype),
        "stage": stage as i64,
        "stageName": stage_name,
        "stagePct": stage_pct,
        "shiny": shiny,
    }) {
        Value::Object(m) => m,
        _ => unreachable!(),
    }
}

pub fn rank_for_level(level: i64) -> &'static str {
    for (th, title) in RANKS {
        if level <= th {
            return title;
        }
    }
    RANKS[RANKS.len() - 1].1
}

pub fn xp_for_level(level: i64) -> i64 {
    400 + 120 * level
}

/// (level, xp_into, xp_for)
pub fn derive_level(total_xp: i64) -> (i64, i64, i64) {
    let mut level = 1;
    let mut remaining = total_xp;
    loop {
        let need = xp_for_level(level);
        if remaining < need {
            return (level, remaining, need);
        }
        remaining -= need;
        level += 1;
        if level > 999 {
            return (level, remaining, xp_for_level(level));
        }
    }
}

pub fn sha256_hex_of(s: &str) -> String {
    sha256_hex(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stages() {
        assert_eq!(stage_for(0), (0, "Egg", 0.0));
        assert_eq!(stage_for(3), (1, "Hatchling", 0.0));
        assert_eq!(stage_for(7), (1, "Hatchling", 0.4444));
        assert_eq!(stage_for(70), (4, "Apex", 1.0));
    }

    #[test]
    fn levels_and_ranks() {
        assert_eq!(derive_level(0), (1, 0, 520));
        assert_eq!(derive_level(520), (2, 0, 640));
        assert_eq!(rank_for_level(1), "Prompt Apprentice");
        assert_eq!(rank_for_level(13), "Prompt Ascendant");
        assert_eq!(rank_for_level(2000), "Prompt Deity");
    }

    #[test]
    fn hashes_match_python() {
        // python3 -c "import hashlib;print(int(hashlib.sha256(b'abc').hexdigest(),16)%48)"
        assert_eq!(session_hash_mod("abc", 48), 13);
        // species_seed(0): int(sha256(b'nymonster:species:0').hexdigest(),16) & 0xFFFFFF
        let d = Sha256::digest(b"nymonster:species:0");
        let want = u32::from_be_bytes([0, d[29], d[30], d[31]]) as i64;
        assert_eq!(species_seed(0), want);
    }
}
