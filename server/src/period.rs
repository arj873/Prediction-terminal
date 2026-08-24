//! Reading a statistical period as a calendar date.
//!
//! A chart's x-axis is a date, but almost nothing here publishes one. The ECB
//! dates a quarter `2026-Q3`, the BLS dates a month `M07`, the Federal Reserve
//! dates a monthly average `2026-08` in the same column where its daily release
//! writes `2026-08-12`, and the EIA dates an hourly reading `2026-08-12T14`.
//! Five publishers, one question, and getting it wrong is not a visible failure:
//! a period the parser does not recognise looks like a header row and silently
//! disappears from the series.
//!
//! Two rules hold everywhere:
//!
//! * A period becomes the day it **begins**. FRED already does this — Q1 2026
//!   is `2026-01-01` there — and mixing conventions would put an OECD quarterly
//!   line three months away from the FRED series it is read against.
//! * An unrecognised period returns `None` and drops one observation, rather
//!   than being guessed at and mis-dating the whole series.

use std::sync::LazyLock;

use regex::Regex;

fn compile(pattern: &str) -> Regex {
    Regex::new(pattern).expect("a period pattern compiles")
}

static ISO_DATE: LazyLock<Regex> = LazyLock::new(|| compile(r"^\d{4}-\d{2}-\d{2}$"));
static HOURLY: LazyLock<Regex> = LazyLock::new(|| compile(r"^(\d{4}-\d{2}-\d{2})[T ]\d{2}"));
static QUARTER: LazyLock<Regex> = LazyLock::new(|| compile(r"^(\d{4})-?[Qq]([1-4])$"));
static SEMESTER: LazyLock<Regex> = LazyLock::new(|| compile(r"^(\d{4})-?[SsBb]([1-2])$"));
static TRIMESTER: LazyLock<Regex> = LazyLock::new(|| compile(r"^(\d{4})-?[Tt]([1-3])$"));
static MONTH: LazyLock<Regex> = LazyLock::new(|| compile(r"^(\d{4})-?[Mm]?(\d{2})$"));
static WEEK: LazyLock<Regex> = LazyLock::new(|| compile(r"^(\d{4})-?[Ww](\d{1,2})$"));
static YEAR: LazyLock<Regex> = LazyLock::new(|| compile(r"^\d{4}$"));

/// `YYYY-MM-DD` for the first day of the period `raw` names, or `None`.
pub fn period_to_date(raw: &str) -> Option<String> {
    let period = raw.trim();
    if period.is_empty() {
        return None;
    }

    // 2026-08-12 — already a date.
    if ISO_DATE.is_match(period) {
        return Some(period.to_string());
    }

    // 2026-08-12T14 — an hourly reading, dated to its day.
    if let Some(caps) = HOURLY.captures(period) {
        return Some(caps[1].to_string());
    }

    // 2026-Q3 / 2026Q3 / 2026-q3
    if let Some(caps) = QUARTER.captures(period) {
        let q: u32 = caps[2].parse().ok()?;
        return Some(format!("{}-{:02}-01", &caps[1], 1 + (q - 1) * 3));
    }

    // 2026-S1 — half-year. `B` is the same thing in a few OECD flows.
    if let Some(caps) = SEMESTER.captures(period) {
        let h: u32 = caps[2].parse().ok()?;
        return Some(format!("{}-{:02}-01", &caps[1], 1 + (h - 1) * 6));
    }

    // 2026-T2 — four-monthly (SDMX "trimester"), rare but present at the OECD.
    if let Some(caps) = TRIMESTER.captures(period) {
        let t: u32 = caps[2].parse().ok()?;
        return Some(format!("{}-{:02}-01", &caps[1], 1 + (t - 1) * 4));
    }

    // 2026-M08 and 2026-08 — monthly.
    if let Some(caps) = MONTH.captures(period) {
        let m: u32 = caps[2].parse().ok()?;
        if (1..=12).contains(&m) {
            return Some(format!("{}-{m:02}-01", &caps[1]));
        }
        // `M13` is the BLS's annual average, and `M00` is nothing at all.
        // Neither is a month, and dating either to one would interleave a
        // twelve-month average with the twelve months it averages.
        return None;
    }

    // 2026-W35 — the Monday of that ISO week.
    if let Some(caps) = WEEK.captures(period) {
        let year: i32 = caps[1].parse().ok()?;
        let week: u32 = caps[2].parse().ok()?;
        return iso_week_start(year, week);
    }

    // 2026 — annual.
    if YEAR.is_match(period) {
        return Some(format!("{period}-01-01"));
    }

    None
}

/// The Monday of ISO week `week` in `year`, as `YYYY-MM-DD`.
///
/// Done with day counting rather than with a calendar type because the rule is
/// a definition, not a conversion: ISO-8601 says week 1 is the week containing
/// 4 January, so the Monday of week 1 is 4 January minus its own weekday
/// offset, and every later week is seven days on from that.
pub fn iso_week_start(year: i32, week: u32) -> Option<String> {
    if !(1..=53).contains(&week) {
        return None;
    }
    let jan4 = days_from_civil(year, 1, 4);
    // `days_from_civil` counts from 1970-01-01, a Thursday, so shifting by 3
    // puts Monday at 0 and makes the remainder the ISO weekday minus one.
    let offset = (jan4 + 3).rem_euclid(7);
    let monday = jan4 - offset + (i64::from(week) - 1) * 7;
    let (y, m, d) = civil_from_days(monday);
    Some(format!("{y:04}-{m:02}-{d:02}"))
}

/// Days since 1970-01-01, by Howard Hinnant's civil-calendar algorithm.
fn days_from_civil(y: i32, m: u32, d: u32) -> i64 {
    let y = i64::from(if m <= 2 { y - 1 } else { y });
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let m = i64::from(m);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The inverse of [`days_from_civil`].
fn civil_from_days(days: i64) -> (i32, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y as i32, m as u32, d as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_date_is_already_a_date() {
        assert_eq!(period_to_date("2026-08-12").as_deref(), Some("2026-08-12"));
        assert_eq!(
            period_to_date(" 2026-08-12 ").as_deref(),
            Some("2026-08-12")
        );
    }

    #[test]
    fn an_hourly_reading_is_dated_to_its_day() {
        // The EIA's electricity feeds are hourly; a chart on this grid is daily.
        assert_eq!(
            period_to_date("2026-08-12T14").as_deref(),
            Some("2026-08-12")
        );
        assert_eq!(
            period_to_date("2026-08-12 14").as_deref(),
            Some("2026-08-12")
        );
    }

    #[test]
    fn a_period_becomes_the_day_it_begins() {
        // FRED's own convention. Mixing it would put an OECD quarterly line
        // three months from the FRED series it is read against.
        assert_eq!(period_to_date("2026-Q1").as_deref(), Some("2026-01-01"));
        assert_eq!(period_to_date("2026-Q3").as_deref(), Some("2026-07-01"));
        assert_eq!(period_to_date("2026Q4").as_deref(), Some("2026-10-01"));
        assert_eq!(period_to_date("2026-q2").as_deref(), Some("2026-04-01"));
        assert_eq!(period_to_date("2026-S2").as_deref(), Some("2026-07-01"));
        assert_eq!(period_to_date("2026-B1").as_deref(), Some("2026-01-01"));
        assert_eq!(period_to_date("2026-T2").as_deref(), Some("2026-05-01"));
        assert_eq!(period_to_date("2026-M08").as_deref(), Some("2026-08-01"));
        assert_eq!(period_to_date("2026-08").as_deref(), Some("2026-08-01"));
        assert_eq!(period_to_date("2026").as_deref(), Some("2026-01-01"));
    }

    #[test]
    fn the_blss_thirteenth_month_is_not_a_month() {
        // `M13` is the annual average, interleaved with the twelve months it
        // averages. Dating it to a month would draw the average as a
        // thirteenth reading in the same year.
        assert_eq!(period_to_date("2026-M13"), None);
        assert_eq!(period_to_date("2026-M00"), None);
        assert_eq!(period_to_date("2026-13"), None);
    }

    #[test]
    fn a_week_becomes_its_monday() {
        // 2026-01-01 is a Thursday, so ISO week 1 begins Monday 2025-12-29.
        assert_eq!(period_to_date("2026-W01").as_deref(), Some("2025-12-29"));
        assert_eq!(period_to_date("2026-W35").as_deref(), Some("2026-08-24"));
        assert_eq!(period_to_date("2026W35").as_deref(), Some("2026-08-24"));
        // 2027-01-01 is a Friday, so week 1 begins on the 4th.
        assert_eq!(period_to_date("2027-W01").as_deref(), Some("2027-01-04"));
        // 2024-01-01 was itself a Monday.
        assert_eq!(period_to_date("2024-W01").as_deref(), Some("2024-01-01"));
    }

    #[test]
    fn a_week_outside_the_year_is_no_week() {
        assert_eq!(period_to_date("2026-W00"), None);
        assert_eq!(period_to_date("2026-W54"), None);
    }

    #[test]
    fn an_unrecognised_period_drops_one_observation_rather_than_mis_dating_the_series() {
        // A header row reaching the parser must not become a data point, and a
        // period nobody has seen must not be guessed at.
        assert_eq!(period_to_date(""), None);
        assert_eq!(period_to_date("   "), None);
        assert_eq!(period_to_date("Period"), None);
        assert_eq!(period_to_date("2026-XX"), None);
        assert_eq!(period_to_date("not a period"), None);
    }

    #[test]
    fn the_civil_calendar_round_trips_across_a_leap_boundary() {
        for (y, m, d) in [
            (1970, 1, 1),
            (2000, 2, 29),
            (2024, 2, 29),
            (2026, 8, 24),
            (2100, 3, 1),
        ] {
            assert_eq!(civil_from_days(days_from_civil(y, m, d)), (y, m, d));
        }
    }
}
