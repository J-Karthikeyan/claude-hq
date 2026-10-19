//! Prints scoring output as JSON so it can be diffed against the Python.
use std::io::Write;
#[path = "../scoring.rs"]
#[allow(dead_code)] // the server uses parts of scoring this checker does not
mod scoring;

fn main() {
    let mut out = std::io::stdout().lock();
    for xp in [0i64, 1, 519, 520, 1000, 12_400, 27_720, 45_214, 999_999] {
        let (lvl, into, need) = scoring::derive_level(xp);
        writeln!(out, r#"{{"xp":{},"level":{},"into":{},"need":{},"rank":"{}"}}"#,
                 xp, lvl, into, need, scoring::rank_for_level(lvl)).unwrap();
    }
    for (p, t, a) in [(0i64,0i64,0i64), (10,30,1), (812,3100,12), (1656,7916,130)] {
        writeln!(out, r#"{{"p":{},"t":{},"a":{},"xp":{}}}"#,
                 p, t, a, scoring::xp_from_counts(p, t, a)).unwrap();
    }
}
