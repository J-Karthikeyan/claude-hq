//! ISO weeks in UTC, the one copy every weekly feature (cups, featured maps,
//! the world boss) keys off.
//!
//! A week is "YYYY-Www" (ISO 8601: Monday first, week 1 holds the year's first
//! Thursday, so 2026-12-31 can be in 2026-W53 and 2027-01-01 too).

use chrono::{Datelike, NaiveDate, Weekday};

/// The ISO week of a timestamp, "YYYY-Www". Only the date (the first 10
/// characters, "YYYY-MM-DD") is read, so "2026-10-10", "2026-10-10 12:00:00"
/// and "2026-10-10T12:00:00Z" agree. An unreadable date gives "".
pub fn iso_week(ts: &str) -> String {
    let Some(d) = ts.get(..10).and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok()) else {
        return String::new();
    };
    let w = d.iso_week();
    format!("{:04}-W{:02}", w.year(), w.week())
}

/// The UTC span of a week: Monday 00:00:00 to Sunday 23:59:59.999999, in the
/// "YYYY-MM-DD HH:MM:SS" form `game_results.at` uses, so plain string
/// comparison works. A malformed or impossible week gives two empty strings.
pub fn week_bounds(week: &str) -> (String, String) {
    let parsed = (|| {
        let (y, w) = week.split_once("-W")?;
        if y.len() != 4 || w.len() != 2 {
            return None;
        }
        let y: i32 = y.parse().ok()?;
        let w: u32 = w.parse().ok()?;
        let mon = NaiveDate::from_isoywd_opt(y, w, Weekday::Mon)?;
        let sun = NaiveDate::from_isoywd_opt(y, w, Weekday::Sun)?;
        Some((mon, sun))
    })();
    match parsed {
        Some((mon, sun)) => (
            format!("{} 00:00:00", mon.format("%Y-%m-%d")),
            format!("{} 23:59:59.999999", sun.format("%Y-%m-%d")),
        ),
        None => (String::new(), String::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sunday_night_and_monday_morning_are_different_weeks() {
        assert_eq!(iso_week("2026-10-11 23:59:59"), "2026-W41");
        assert_eq!(iso_week("2026-10-12 00:00:00"), "2026-W42");
        assert_eq!(week_bounds("2026-W41"),
                   ("2026-10-05 00:00:00".to_string(), "2026-10-11 23:59:59.999999".to_string()));
    }

    #[test]
    fn year_edges_follow_iso() {
        assert_eq!(iso_week("2026-12-31"), "2026-W53");
        assert_eq!(iso_week("2027-01-03"), "2026-W53");
        assert_eq!(iso_week("2027-01-04"), "2027-W01");
        assert_eq!(week_bounds("2026-W53").0, "2026-12-28 00:00:00");
        assert_eq!(week_bounds("2027-W01").0, "2027-01-04 00:00:00");
    }

    #[test]
    fn junk_is_empty() {
        assert_eq!(iso_week("soon"), "");
        assert_eq!(week_bounds("2027-W53"), (String::new(), String::new()));
        assert_eq!(week_bounds("2026-41"), (String::new(), String::new()));
    }
}
