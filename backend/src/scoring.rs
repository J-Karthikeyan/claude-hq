//! Scoring rules, ported from `backend/app/scoring.py`.
//!
//! These are pure functions with no I/O, which makes them the easiest place to
//! start reading Rust: no async, no borrowing puzzles, just types.

use chrono::NaiveDate;
use std::collections::HashSet;

pub const XP_PER_PROMPT: i64 = 10;
pub const XP_PER_TOOL: i64 = 3;
pub const XP_PER_ARTIFACT: i64 = 40;

/// `&[(i64, &str)]` is a borrowed slice of tuples -- no allocation, and the
/// whole table lives in the binary rather than being built at startup.
const RANKS: &[(i64, &str)] = &[
    (1, "Prompt Apprentice"),
    (3, "Prompt Adept"),
    (5, "Prompt Conjurer"),
    (8, "Prompt Sorcerer"),
    (12, "Prompt Archmage"),
    (18, "Prompt Ascendant"),
    (999, "Prompt Deity"),
];

pub fn xp_from_counts(prompts: i64, tools: i64, artifacts: i64) -> i64 {
    prompts * XP_PER_PROMPT + tools * XP_PER_TOOL + artifacts * XP_PER_ARTIFACT
}

/// XP needed to advance FROM the given level to the next.
pub fn xp_for_level(level: i64) -> i64 {
    400 + 120 * level
}

/// Returns `(level, xp_into_level, xp_for_level)`.
///
/// Rust has no tuple-unpacking-with-names, so callers destructure:
///     let (level, into, need) = derive_level(xp);
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

/// `&'static str` because every rank name is baked into the binary -- there is
/// nothing to allocate or free, and the reference is valid for the program's life.
pub fn rank_for_level(level: i64) -> &'static str {
    for (threshold, title) in RANKS {
        if level <= *threshold {
            return title;
        }
    }
    RANKS[RANKS.len() - 1].1
}

/// Consecutive active days ending today or yesterday.
pub fn streak_from_dates(active: &HashSet<NaiveDate>, today: NaiveDate) -> i64 {
    let yesterday = today.pred_opt().expect("date underflow");

    // `Option` instead of Python's `None` sentinel: the compiler forces the
    // "no streak" case to be handled rather than letting it slip through.
    let mut cur = if active.contains(&today) {
        Some(today)
    } else if active.contains(&yesterday) {
        Some(yesterday)
    } else {
        None
    };

    let mut n = 0;
    while let Some(d) = cur {
        if !active.contains(&d) {
            break;
        }
        n += 1;
        cur = d.pred_opt();
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xp_matches_the_python_formula() {
        assert_eq!(xp_from_counts(812, 3100, 12), 17_900);
        assert_eq!(xp_from_counts(0, 0, 0), 0);
    }

    #[test]
    fn levels_match_the_python_thresholds() {
        assert_eq!(derive_level(0), (1, 0, 520));
        assert_eq!(derive_level(520), (2, 0, 640));
        assert_eq!(derive_level(1000), (2, 480, 640));
        assert_eq!(derive_level(12_400).0, 12);
    }

    #[test]
    fn ranks_cap_at_deity() {
        assert_eq!(rank_for_level(1), "Prompt Apprentice");
        assert_eq!(rank_for_level(14), "Prompt Ascendant");
        assert_eq!(rank_for_level(19), "Prompt Deity");
        assert_eq!(rank_for_level(999), "Prompt Deity");
    }

    #[test]
    fn streak_counts_back_from_today() {
        let today = NaiveDate::from_ymd_opt(2026, 9, 26).unwrap();
        let active: HashSet<_> = (0..4).map(|i| today - chrono::Duration::days(i)).collect();
        assert_eq!(streak_from_dates(&active, today), 4);
    }

    #[test]
    fn streak_tolerates_a_gap_of_one_day() {
        // Active yesterday but not today still counts -- you have not lost it yet.
        let today = NaiveDate::from_ymd_opt(2026, 9, 26).unwrap();
        let active: HashSet<_> = (1..4).map(|i| today - chrono::Duration::days(i)).collect();
        assert_eq!(streak_from_dates(&active, today), 3);
    }

    #[test]
    fn streak_is_zero_when_cold() {
        let today = NaiveDate::from_ymd_opt(2026, 9, 26).unwrap();
        let active: HashSet<_> = [today - chrono::Duration::days(5)].into_iter().collect();
        assert_eq!(streak_from_dates(&active, today), 0);
    }
}
