//! Reading the recurring question out of a dated slug.
//!
//! Four venues name a contract with a slug that carries its occasion:
//! `usfed-fomc-2026-10-28`, `fed-decision-in-september-762`,
//! `mlb-oak-hou-2026-08-23`, `bitcoin-up-or-down-on-august-18-2026`. A series
//! here is the *question* — "what will the Fed do at this meeting" — so the
//! occasion has to come off before two instances of it can be recognised as one
//! series, and before either can be paired with the same question at another
//! broker.
//!
//! Getting this wrong is expensive in both directions. Strip too little and the
//! September and October FOMC books become two one-event series, neither of
//! which lines up against Kalshi's `KXFEDDECISION`. Strip too much and every
//! congressional district collapses into one.
//!
//! This lives in the pure crate rather than beside any one venue because three
//! of the four read it and the fourth would have copied it. It is the rule, not
//! a venue's dialect.

/// A slug segment naming a month.
fn is_month_segment(part: &str) -> bool {
    matches!(
        part,
        "jan"
            | "january"
            | "feb"
            | "february"
            | "mar"
            | "march"
            | "apr"
            | "april"
            | "may"
            | "jun"
            | "june"
            | "jul"
            | "july"
            | "aug"
            | "august"
            | "sep"
            | "sept"
            | "september"
            | "oct"
            | "october"
            | "nov"
            | "november"
            | "dec"
            | "december"
    )
}

fn is_year(part: &str) -> bool {
    part.len() == 4
        && (part.starts_with("19") || part.starts_with("20"))
        && part.bytes().all(|b| b.is_ascii_digit())
}

fn is_day_or_month(part: &str) -> bool {
    (1..=2).contains(&part.len()) && part.bytes().all(|b| b.is_ascii_digit())
}

/// Drop the segments of a slug that name an occasion rather than a question.
///
/// Numeric dates are removed as whole groups, never as loose numbers, because a
/// slug's other numbers carry meaning: `ushr-tx-15-2026-11-03` is the Texas 15th
/// district on 3 November 2026, and stripping every short number would file all
/// 38 Texas districts under one series. A year anchors the group — the two
/// segments after it if they are a month and day (`2026-11-03`), otherwise the
/// two before it (`03-14-2027`), otherwise nothing.
///
/// A month spelled in words anchors its own, smaller group: the one adjacent
/// segment that is a day number. `bitcoin-up-or-down-on-august-18-2026` is a
/// question asked again tomorrow, and leaving the `18` on gives it a series of
/// its own every day. The day is only taken next to a named month, so the
/// district case above is untouched — it has no month name to anchor on.
pub fn strip_dates(slug: &str) -> String {
    let parts: Vec<&str> = slug.split('-').collect();
    let mut drop = vec![false; parts.len()];

    let at = |index: isize| -> &str {
        if index < 0 {
            return "";
        }
        parts.get(index as usize).copied().unwrap_or("")
    };

    for i in 0..parts.len() {
        let signed = i as isize;

        if is_month_segment(parts[i]) {
            drop[i] = true;
            if is_day_or_month(at(signed + 1)) {
                drop[i + 1] = true;
            } else if is_day_or_month(at(signed - 1)) {
                drop[(signed - 1) as usize] = true;
            }
        }

        if !is_year(parts[i]) {
            continue;
        }

        drop[i] = true;
        if is_day_or_month(at(signed + 1)) && is_day_or_month(at(signed + 2)) {
            drop[i + 1] = true;
            drop[i + 2] = true;
        } else if is_day_or_month(at(signed - 1)) && is_day_or_month(at(signed - 2)) {
            drop[(signed - 1) as usize] = true;
            drop[(signed - 2) as usize] = true;
        }
    }

    parts
        .iter()
        .enumerate()
        .filter(|(i, part)| !part.is_empty() && !drop[*i])
        .map(|(_, part)| *part)
        .collect::<Vec<_>>()
        .join("-")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removes_a_trailing_iso_date() {
        assert_eq!(strip_dates("usse-nc-2026-11-03"), "usse-nc");
        assert_eq!(strip_dates("mlb-nlchamp-2026-09-27"), "mlb-nlchamp");
    }

    #[test]
    fn removes_a_date_that_is_not_at_the_end() {
        assert_eq!(strip_dates("oscars-03-14-2027-bestpic"), "oscars-bestpic");
        assert_eq!(
            strip_dates("oscars-nom-2027-01-31-bestpic"),
            "oscars-nom-bestpic"
        );
        assert_eq!(strip_dates("usgubp-ok-2026-06-16-rep"), "usgubp-ok-rep");
    }

    #[test]
    fn removes_a_month_named_in_words_wherever_it_sits() {
        // Without this each month of CPI is its own one-event series, and none
        // of them can pair with the monthly CPI market at either other broker.
        assert_eq!(strip_dates("uscpi-august-yoy"), "uscpi-yoy");
        assert_eq!(strip_dates("uscpi-september-yoy"), "uscpi-yoy");
    }

    #[test]
    fn removes_the_day_beside_a_month_named_in_words() {
        // predict.fun words its daily books this way, and without the day the
        // 18th and the 19th are two one-event series that pair with nothing.
        assert_eq!(
            strip_dates("bitcoin-up-or-down-on-august-18-2026"),
            "bitcoin-up-or-down-on"
        );
        assert_eq!(
            strip_dates("bitcoin-up-or-down-on-august-19-2026"),
            "bitcoin-up-or-down-on"
        );
        // Either side of the month name; `18-august` reads the same way.
        assert_eq!(strip_dates("nba-finals-18-june-game"), "nba-finals-game");
        // And Gemini's FOMC wording, where the day has no year after it.
        assert_eq!(strip_dates("fed-decision-september-17"), "fed-decision");
    }

    #[test]
    fn keeps_a_number_that_is_not_part_of_a_date() {
        // The district is the question. Stripping loose numbers would file all
        // 38 Texas House races under one series.
        assert_eq!(strip_dates("ushr-tx-15-2026-11-03"), "ushr-tx-15");
        assert_eq!(strip_dates("ushr-tx-28-2026-11-03"), "ushr-tx-28");
        assert_eq!(strip_dates("bbus-s28-winner"), "bbus-s28-winner");
        // No month name anywhere, so the day rule cannot reach the district.
        assert_eq!(strip_dates("ushr-tx-15"), "ushr-tx-15");
    }

    #[test]
    fn removes_a_bare_trailing_season_year() {
        assert_eq!(strip_dates("nfl-2026"), "nfl");
    }

    #[test]
    fn leaves_a_slug_with_no_occasion_in_it_alone() {
        assert_eq!(strip_dates("jerpowgov"), "jerpowgov");
        assert_eq!(strip_dates("big-game-champion"), "big-game-champion");
    }

    #[test]
    fn a_slug_that_is_only_a_date_strips_to_nothing() {
        // Callers fall back to the raw slug rather than key a series on "".
        assert_eq!(strip_dates("2026-11-03"), "");
    }
}
