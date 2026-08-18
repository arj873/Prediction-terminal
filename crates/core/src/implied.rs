//! Turning a Kalshi strike ladder into an implied price for the underlying.
//!
//! A binary contract quoted at `p` is a market forecast of `P(settlement lands
//! in this contract's region) = p`. A *ladder* of them over the same underlying
//! and expiry therefore quotes a whole probability distribution, and the number
//! traders want out of it — "where does the market think BTC closes?" — is a
//! location statistic of that distribution.
//!
//! The pipeline is four steps, in order:
//!
//!   1. legs → survival knots  `S(k) = P(price > k)`, from any mix of
//!      above/below/range contracts.
//!   2. monotone fit           a survival function cannot rise with the strike;
//!      quotes sometimes say it does.
//!   3. locate                 the interpolated 50% crossing (median), or the
//!      bucket-weighted mean.
//!   4. report the tail        mass outside the quoted strikes, so a caller can
//!      tell a confident number from an extrapolated one.
//!
//! Nothing here touches the network or the DOM: it is the one part of this
//! feature that can be checked against arithmetic rather than a live market.

use std::cmp::Ordering;

use crate::types::{ImpliedMethod, StrikeType};

/// One contract's contribution: the region it pays out on, and its price.
///
/// `lo`/`hi` are `None` for an unbounded side, so `{lo: Some(63000), hi: None}`
/// is "$63,000 or above" and `{lo: None, hi: Some(52750)}` is "$52,750 or
/// below".
#[derive(Debug, Clone, PartialEq)]
pub struct ImpliedLeg {
    pub lo: Option<f64>,
    pub hi: Option<f64>,
    /// Probability, 0..1.
    pub p: f64,
    /// Present for diagnostics; the maths only reads `lo`/`hi`.
    pub ticker: Option<String>,
}

/// A point on the estimated survival function.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SurvivalKnot {
    pub strike: f64,
    /// `P(price > strike)`, 0..1, non-increasing in `strike` after the fit.
    pub survival: f64,
}

/// Why [`ImpliedResult::value`] is `None`, for the panel to show instead of an
/// empty chart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImpliedReason {
    NoStrikes,
    NoCrossing,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ImpliedResult {
    /// The implied price, or `None` when the ladder cannot support one.
    pub value: Option<f64>,
    pub method: ImpliedMethod,
    pub knots: Vec<SurvivalKnot>,
    /// Probability mass below the lowest strike plus mass above the highest.
    pub tail_mass: f64,
    pub reason: Option<ImpliedReason>,
}

/* --------------------------------------------------------------- quoting */

/// The market's probability for one contract, given its book.
///
/// A two-sided book gives the mid. A one-sided book is the interesting case:
/// an offer at 1¢ with no bid does *not* mean 0.5¢ — it means somebody will
/// sell at 1¢ and nobody will buy at any price, so the true value lies in
/// `[0, ask]`, and its midpoint is the honest reading. Treating a lone ask as a
/// mid is what makes deep out-of-the-money strikes contribute phantom
/// probability, which on an 80-strike ladder adds up to a badly skewed
/// distribution.
///
/// Last price is the final fallback: it is a real trade, but a stale one.
pub fn quote_probability(bid: Option<f64>, ask: Option<f64>, last: Option<f64>) -> Option<f64> {
    match (valid(bid), valid(ask)) {
        (Some(b), Some(a)) => Some(clamp01((b + a) / 2.0)),
        (_, Some(a)) => Some(clamp01(a / 2.0)),
        (Some(b), _) => Some(clamp01((b + 1.0) / 2.0)),
        (None, None) => valid(last).map(clamp01),
    }
}

/// Kalshi reports an empty book side as `0`, which is "absent", not "worthless".
fn valid(value: Option<f64>) -> Option<f64> {
    value.filter(|v| v.is_finite() && *v > 0.0 && *v <= 1.0)
}

/// `Math.min(1, Math.max(0, value))`, NaN and all.
///
/// `f64::clamp` is the faithful spelling, not `max(0.0).min(1.0)`: Rust's `max`
/// and `min` discard a NaN operand and would quietly turn a NaN survival into
/// `0`, where JavaScript — and `clamp` — propagate it.
fn clamp01(value: f64) -> f64 {
    value.clamp(0.0, 1.0)
}

/// Map a Kalshi strike type and its bounds onto a leg's payout region.
///
/// A `None` strike type covers both the plain yes/no market with no numeric
/// strike and Kalshi's `structured`/`custom` strikes, which are rules rather
/// than price levels and have no place on a survival curve.
pub fn leg_from_strike(
    strike_type: Option<StrikeType>,
    floor_strike: Option<f64>,
    cap_strike: Option<f64>,
    p: f64,
    ticker: Option<&str>,
) -> Option<ImpliedLeg> {
    let lo = floor_strike.filter(|v| v.is_finite());
    let hi = cap_strike.filter(|v| v.is_finite());
    let ticker = ticker.filter(|t| !t.is_empty()).map(str::to_owned);

    match strike_type? {
        // The half-cent offset between `>` and `>=` is far below the tick size
        // of any ladder Kalshi lists; both are the same knot.
        StrikeType::Greater | StrikeType::GreaterOrEqual => Some(ImpliedLeg {
            lo: Some(lo?),
            hi: None,
            p,
            ticker,
        }),
        StrikeType::Less | StrikeType::LessOrEqual => Some(ImpliedLeg {
            lo: None,
            hi: Some(hi?),
            p,
            ticker,
        }),
        StrikeType::Between => Some(ImpliedLeg {
            lo: Some(lo?),
            hi: Some(hi?),
            p,
            ticker,
        }),
    }
}

/* -------------------------------------------------------- survival curve */

/// Legs → survival knots.
///
/// Two ladder shapes reach here and they need opposite treatment:
///
/// **Nested** (`BTC $63,000 or above`, `$63,250 or above`, …). Each contract
/// already *is* `P(price > k)`, so its price is the knot. Probabilities overlap
/// by construction and must not be summed.
///
/// **Disjoint** (`BTC $62,750 to $62,999`, `$63,000 to $63,249`, …). These tile
/// the line, so they are a probability mass function and the survival at each
/// bucket floor is the sum of every bucket at or above it.
///
/// A disjoint ladder is also *normalised* to sum to 1. Bid/ask spreads make the
/// raw sum drift above 1 — on a 300-bucket ETH ladder the drift moved the
/// implied price by more than $200, which is the difference between a useful
/// overlay and a misleading one. Normalising is not cosmetic: mutual exclusivity
/// is a fact about the event, so the constraint is real information.
pub fn survival_knots(legs: &[ImpliedLeg]) -> Vec<SurvivalKnot> {
    let usable: Vec<&ImpliedLeg> = legs
        .iter()
        .filter(|l| l.p.is_finite() && (l.lo.is_some() || l.hi.is_some()))
        .collect();
    if usable.is_empty() {
        return Vec::new();
    }

    /*
     * What tells the two shapes apart is a *bounded* leg, not the absence of a
     * `hi`. A leg with both bounds is a bucket and belongs to a mass function; a
     * leg with one bound is a cumulative claim and already states a survival.
     *
     * Testing `all(|l| l.hi.is_none())` instead meant a single "or below" rung
     * — which is one-sided, and every bit as cumulative as its "or above"
     * siblings — dragged an otherwise nested ladder down the disjoint path,
     * where overlapping cumulative prices were summed and normalised as if they
     * were disjoint masses. It moved a BTC ladder's median by about 2%,
     * silently, and `select_strikes` can drop or restore that one rung between
     * polls, so a single overlay could flip interpretation mid-chart.
     */
    let bucketed = usable.iter().any(|l| l.lo.is_some() && l.hi.is_some());

    if !bucketed {
        let knots: Vec<SurvivalKnot> = usable
            .iter()
            .filter_map(|l| match (l.lo, l.hi) {
                // "k or above" quotes P(price > k) directly.
                (Some(lo), _) => Some(SurvivalKnot {
                    strike: lo,
                    survival: clamp01(l.p),
                }),
                // "k or below" quotes the complement of the same thing.
                (None, Some(hi)) => Some(SurvivalKnot {
                    strike: hi,
                    survival: clamp01(1.0 - l.p),
                }),
                (None, None) => None,
            })
            .collect();
        return collapse_duplicates(knots);
    }

    let total: f64 = usable.iter().map(|l| l.p).sum();
    let scale = if total > 0.0 { 1.0 / total } else { 0.0 };

    // Walk the buckets from the top down, accumulating mass. Each bucket's floor
    // is a strike whose survival is everything stacked above it.
    let mut descending: Vec<(f64, Option<f64>, f64)> = usable
        .iter()
        .filter_map(|l| Some((l.lo?, l.hi, l.p)))
        .collect();
    descending.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(Ordering::Equal));

    let mut knots: Vec<SurvivalKnot> = Vec::with_capacity(descending.len() + 1);
    let mut running = 0.0;
    for &(lo, _, p) in &descending {
        running += p * scale;
        knots.push(SurvivalKnot {
            strike: lo,
            survival: clamp01(running),
        });
    }

    /*
     * Close the top of a bounded ladder.
     *
     * Emitting a knot only at each bucket's floor throws away the cap of the
     * highest bucket, so `(120, 130]` left the curve ending at 120 with the
     * bucket's own mass still sitting on it. `tail_mass` then read that mass as
     * unbracketed and reported 30% on a ladder that brackets everything — and
     * `tail_mass` is the number the panel offers a reader for judging how much
     * of the estimate is assumption. Nothing lies above a bounded cap, so
     * survival there is zero, and saying that costs one knot.
     */
    if let Some(cap) = descending.first().and_then(|&(_, hi, _)| hi) {
        knots.push(SurvivalKnot {
            strike: cap,
            survival: 0.0,
        });
    }

    // The `less` leg contributes only mass below the lowest floor, which the
    // accumulation above already accounts for; it needs no knot of its own.
    knots.reverse();
    collapse_duplicates(knots)
}

/// Average duplicate strikes and sort ascending, so knots are a clean function.
fn collapse_duplicates(knots: Vec<SurvivalKnot>) -> Vec<SurvivalKnot> {
    let mut grouped: Vec<(f64, f64, usize)> = Vec::with_capacity(knots.len());
    for knot in knots {
        match grouped
            .iter_mut()
            .find(|(strike, _, _)| *strike == knot.strike)
        {
            Some(entry) => {
                entry.1 += knot.survival;
                entry.2 += 1;
            }
            None => grouped.push((knot.strike, knot.survival, 1)),
        }
    }

    let mut out: Vec<SurvivalKnot> = grouped
        .into_iter()
        .map(|(strike, sum, n)| SurvivalKnot {
            strike,
            survival: sum / n as f64,
        })
        .collect();
    out.sort_by(|a, b| a.strike.partial_cmp(&b.strike).unwrap_or(Ordering::Equal));
    out
}

/// Force the survival curve to be non-increasing.
///
/// `P(price > k)` cannot grow as `k` grows, but quoted ladders violate it
/// constantly — adjacent strikes are quoted by different participants at
/// different moments, and a 1¢ tick is wider than the true gap between
/// neighbouring strikes. Left uncorrected, a violation puts a spurious 50%
/// crossing in the middle of the ladder and the implied line jumps.
///
/// This is pool-adjacent-violators, which returns the closest non-increasing
/// curve in the least-squares sense — it corrects the inconsistency without
/// choosing a direction to be wrong in, which a simpler running-minimum or
/// running-maximum pass would.
pub fn enforce_monotone(knots: &[SurvivalKnot]) -> Vec<SurvivalKnot> {
    if knots.len() < 2 {
        return knots.to_vec();
    }

    let mut values: Vec<f64> = Vec::with_capacity(knots.len());
    let mut weights: Vec<usize> = Vec::with_capacity(knots.len());

    for knot in knots {
        values.push(knot.survival);
        weights.push(1);
        // Merge backwards while the previous block sits *below* this one.
        while values.len() > 1 && values[values.len() - 2] < values[values.len() - 1] {
            let n = values.len();
            let (v2, w2) = (values[n - 1], weights[n - 1]);
            let (v1, w1) = (values[n - 2], weights[n - 2]);
            values.truncate(n - 2);
            weights.truncate(n - 2);
            values.push((v1 * w1 as f64 + v2 * w2 as f64) / (w1 + w2) as f64);
            weights.push(w1 + w2);
        }
    }

    let mut out: Vec<SurvivalKnot> = Vec::with_capacity(knots.len());
    let mut index = 0;
    for (block, &weight) in weights.iter().enumerate() {
        let value = clamp01(values[block]);
        for _ in 0..weight {
            if let Some(knot) = knots.get(index) {
                out.push(SurvivalKnot {
                    strike: knot.strike,
                    survival: value,
                });
            }
            index += 1;
        }
    }
    out
}

/* ------------------------------------------------------------- estimators */

/// The strike where the market puts even odds of finishing above or below.
///
/// This is the default because it needs nothing from outside the quoted ladder:
/// the crossing is bracketed by two real strikes with real quotes. Its cost is
/// that a market whose whole ladder sits on one side of 50% has no crossing at
/// all, and this returns `None` rather than inventing one.
pub fn implied_median(knots: &[SurvivalKnot]) -> Option<f64> {
    for pair in knots.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        if a.survival >= 0.5 && b.survival <= 0.5 {
            if a.survival == b.survival {
                return Some(a.strike);
            }
            let t = (a.survival - 0.5) / (a.survival - b.survival);
            return Some(a.strike + t * (b.strike - a.strike));
        }
    }
    None
}

/// Probability-weighted mean of the distribution.
///
/// Between consecutive strikes the mass `S(k_i) - S(k_i+1)` is placed at the
/// midpoint. The tails are the honest weakness: mass below the lowest strike and
/// above the highest has no bracket, so each is placed half an average strike
/// step beyond the edge. That guess is why [`ImpliedResult::tail_mass`] is
/// reported — with 1% in the tails the mean is solid, with 30% it is mostly the
/// assumption talking.
pub fn implied_mean(knots: &[SurvivalKnot]) -> Option<f64> {
    if knots.len() < 2 {
        return None;
    }

    let first = knots.first()?;
    let last = knots.last()?;
    let step = (last.strike - first.strike) / (knots.len() - 1) as f64;

    let mut total = 0.0;
    for pair in knots.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        total += (a.survival - b.survival) * ((a.strike + b.strike) / 2.0);
    }

    total += (1.0 - first.survival) * (first.strike - step / 2.0);
    total += last.survival * (last.strike + step / 2.0);
    Some(total)
}

/// Mass the quoted strikes do not bracket: below the floor plus above the cap.
pub fn tail_mass(knots: &[SurvivalKnot]) -> f64 {
    match (knots.first(), knots.last()) {
        (Some(first), Some(last)) => {
            let below = 1.0 - first.survival;
            let above = last.survival;
            clamp01(below + above)
        }
        _ => 1.0,
    }
}

/// Run the whole pipeline: legs in, one implied price out.
///
/// Returns the intermediate knots too, because a panel that can show *why* a
/// number moved is worth far more than one that only shows the number.
pub fn implied_price(legs: &[ImpliedLeg], method: ImpliedMethod) -> ImpliedResult {
    let knots = enforce_monotone(&survival_knots(legs));

    if knots.len() < 2 {
        return ImpliedResult {
            value: None,
            method,
            tail_mass: tail_mass(&knots),
            knots,
            reason: Some(ImpliedReason::NoStrikes),
        };
    }

    let value = match method {
        ImpliedMethod::Mean => implied_mean(&knots),
        ImpliedMethod::Median => implied_median(&knots),
    };
    ImpliedResult {
        value,
        method,
        tail_mass: tail_mass(&knots),
        knots,
        // A median with no 50% crossing means the ladder does not straddle the
        // market — the strikes are all above, or all below, where price is
        // trading.
        reason: value.is_none().then_some(ImpliedReason::NoCrossing),
    }
}

/// Implied-price maths tests.
///
/// This is the one part of the feature that can be checked against arithmetic
/// rather than a live market, so it is checked hard. The ladders below are
/// shaped like the real ones — Kalshi's `KXBTCD` (an "or above" ladder) and
/// `KXBTC` (a mutually-exclusive range ladder) over the same underlying and the
/// same expiry — because the two need opposite handling and getting that wrong
/// is silent: both shapes still produce a plausible-looking number.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::{round4, round_to};

    /// A leg with no ticker, which is all the maths ever reads.
    fn leg(lo: Option<f64>, hi: Option<f64>, p: f64) -> ImpliedLeg {
        ImpliedLeg {
            lo,
            hi,
            p,
            ticker: None,
        }
    }

    fn knot(strike: f64, survival: f64) -> SurvivalKnot {
        SurvivalKnot { strike, survival }
    }

    /// `P(price > k)` at each strike, to 4dp.
    fn survivals(knots: &[SurvivalKnot]) -> Vec<(f64, f64)> {
        knots
            .iter()
            .map(|k| (k.strike, round4(k.survival)))
            .collect()
    }

    mod quote_probability {
        use super::*;

        #[test]
        fn takes_the_mid_of_a_two_sided_book() {
            assert_eq!(
                quote_probability(Some(0.58), Some(0.6), Some(0.59)),
                Some(0.59)
            );
        }

        #[test]
        fn halves_a_lone_ask_because_the_true_value_lies_in_zero_to_ask() {
            // Somebody will sell at 1c and nobody will buy at any price. Reading
            // that as a 1c mid is what makes 80 dead wings add up to real
            // probability mass.
            assert_eq!(quote_probability(None, Some(0.01), None), Some(0.005));
        }

        #[test]
        fn splits_the_difference_between_a_lone_bid_and_certainty() {
            assert_eq!(quote_probability(Some(0.99), None, None), Some(0.995));
        }

        #[test]
        fn falls_back_to_the_last_trade_when_the_book_is_empty() {
            assert_eq!(quote_probability(None, None, Some(0.42)), Some(0.42));
        }

        #[test]
        fn treats_a_zero_quote_as_absent_not_as_worthless() {
            // Kalshi reports an empty book side as "0.0000".
            assert_eq!(quote_probability(Some(0.0), Some(0.0), Some(0.0)), None);
            assert_eq!(
                quote_probability(Some(0.0), Some(0.0), Some(0.3)),
                Some(0.3)
            );
        }

        #[test]
        fn rejects_out_of_range_prices() {
            assert_eq!(quote_probability(None, None, Some(1.4)), None);
            assert_eq!(quote_probability(None, None, Some(-0.2)), None);
        }
    }

    mod leg_from_strike {
        use super::*;

        #[test]
        fn maps_an_or_above_contract_to_a_lower_bounded_region() {
            assert_eq!(
                leg_from_strike(Some(StrikeType::Greater), Some(63000.0), None, 0.5, None),
                Some(leg(Some(63000.0), None, 0.5))
            );
            assert_eq!(
                leg_from_strike(
                    Some(StrikeType::GreaterOrEqual),
                    Some(63000.0),
                    None,
                    0.5,
                    None
                ),
                Some(leg(Some(63000.0), None, 0.5))
            );
        }

        #[test]
        fn maps_an_or_below_contract_to_an_upper_bounded_region() {
            assert_eq!(
                leg_from_strike(Some(StrikeType::Less), None, Some(52750.0), 0.01, None),
                Some(leg(None, Some(52750.0), 0.01))
            );
        }

        #[test]
        fn maps_a_range_contract_to_a_bounded_bucket() {
            assert_eq!(
                leg_from_strike(
                    Some(StrikeType::Between),
                    Some(62750.0),
                    Some(62999.99),
                    0.3,
                    None
                ),
                Some(leg(Some(62750.0), Some(62999.99), 0.3))
            );
        }

        #[test]
        fn rejects_strikes_that_are_not_numeric_price_levels() {
            // Kalshi's `structured` and `custom` strikes are rules, not numbers,
            // so they never parse into a `StrikeType` and arrive here as `None`
            // — the same door the plain yes/no market comes through.
            assert_eq!(leg_from_strike(None, None, None, 0.5, None), None);
            assert_eq!(
                leg_from_strike(Some(StrikeType::Greater), None, None, 0.5, None),
                None
            );
        }
    }

    mod survival_knots {
        use super::*;

        #[test]
        fn reads_a_nested_or_above_ladder_as_the_survival_function_directly() {
            // Each contract already *is* P(price > k). Summing them would be wrong.
            let legs = [
                leg(Some(100.0), None, 0.9),
                leg(Some(110.0), None, 0.5),
                leg(Some(120.0), None, 0.1),
            ];
            assert_eq!(
                survivals(&survival_knots(&legs)),
                vec![(100.0, 0.9), (110.0, 0.5), (120.0, 0.1)]
            );
        }

        #[test]
        fn accumulates_a_disjoint_range_ladder_from_the_top_down() {
            let legs = [
                leg(Some(100.0), Some(110.0), 0.2),
                leg(Some(110.0), Some(120.0), 0.5),
                leg(Some(120.0), Some(130.0), 0.3),
            ];
            // P(>120) = 0.3, P(>110) = 0.8, P(>100) = 1.0, and nothing above the cap.
            assert_eq!(
                survivals(&survival_knots(&legs)),
                vec![(100.0, 1.0), (110.0, 0.8), (120.0, 0.3), (130.0, 0.0)]
            );
        }

        #[test]
        fn normalises_a_disjoint_ladder_whose_quotes_sum_past_one() {
            // Bid/ask spreads inflate the raw sum. On a real 300-bucket ETH
            // ladder the un-normalised curve moved the implied price by more
            // than $200.
            let legs = [
                leg(Some(100.0), Some(110.0), 0.4),
                leg(Some(110.0), Some(120.0), 1.0),
                leg(Some(120.0), Some(130.0), 0.6),
            ];
            assert_eq!(
                survivals(&survival_knots(&legs)),
                vec![(100.0, 1.0), (110.0, 0.8), (120.0, 0.3), (130.0, 0.0)]
            );
        }

        #[test]
        fn closes_a_bounded_ladder_at_its_cap_so_no_bracketed_mass_reads_as_tail() {
            // The top bucket's ceiling used to be dropped, leaving that bucket's
            // own mass sitting on the last knot — which tail_mass then reported
            // as unbracketed. tail_mass is what the panel offers a reader for
            // judging how much of the estimate is assumption, so overstating it
            // is not cosmetic.
            let bounded = [
                leg(Some(100.0), Some(110.0), 0.2),
                leg(Some(110.0), Some(120.0), 0.5),
                leg(Some(120.0), Some(130.0), 0.3),
            ];
            assert_eq!(implied_price(&bounded, ImpliedMethod::Mean).tail_mass, 0.0);

            // An open-ended top genuinely has mass nobody brackets, and still says so.
            let open_topped = [
                leg(Some(100.0), Some(110.0), 0.2),
                leg(Some(110.0), Some(120.0), 0.5),
                leg(Some(120.0), None, 0.3),
            ];
            assert!(implied_price(&open_topped, ImpliedMethod::Mean).tail_mass > 0.0);
        }

        #[test]
        fn reads_a_one_sided_or_below_rung_as_cumulative_not_as_a_bucket() {
            // Every leg here is one-sided, so the whole ladder is cumulative.
            // Testing for the absence of a `hi` instead of the presence of a
            // *bounded* leg let this single "or below" rung drag the other three
            // down the disjoint path, where overlapping cumulative prices were
            // summed and normalised as though they were disjoint masses —
            // moving the median about 2%, silently.
            let nested = [
                leg(Some(100.0), None, 0.9),
                leg(Some(110.0), None, 0.5),
                leg(Some(120.0), None, 0.1),
            ];
            let with_floor_rung = [
                leg(None, Some(100.0), 0.1),
                leg(Some(100.0), None, 0.9),
                leg(Some(110.0), None, 0.5),
                leg(Some(120.0), None, 0.1),
            ];

            assert_eq!(
                implied_price(&nested, ImpliedMethod::Median).value,
                Some(110.0)
            );
            assert_eq!(
                implied_price(&with_floor_rung, ImpliedMethod::Median).value,
                Some(110.0)
            );
        }

        #[test]
        fn still_reads_a_genuine_bucket_ladder_with_cap_and_floor_rungs_as_a_mass_function() {
            // A bounded ladder wearing "or below" and "or above" rungs at its
            // ends is a real PMF and must keep taking the disjoint path.
            let legs = [
                leg(None, Some(100.0), 0.1),
                leg(Some(100.0), Some(110.0), 0.2),
                leg(Some(110.0), Some(120.0), 0.5),
                leg(Some(120.0), None, 0.2),
            ];
            assert_eq!(
                survivals(&survival_knots(&legs)),
                vec![(100.0, 0.9), (110.0, 0.7), (120.0, 0.2)]
            );
        }

        #[test]
        fn averages_duplicate_strikes_rather_than_double_counting_them() {
            let legs = [leg(Some(100.0), None, 0.8), leg(Some(100.0), None, 0.6)];
            assert_eq!(survivals(&survival_knots(&legs)), vec![(100.0, 0.7)]);
        }

        #[test]
        fn returns_nothing_when_no_leg_carries_a_usable_bound() {
            assert_eq!(survival_knots(&[]), vec![]);
            assert_eq!(survival_knots(&[leg(None, None, 0.5)]), vec![]);
        }
    }

    mod enforce_monotone {
        use super::*;

        #[test]
        fn leaves_an_already_non_increasing_curve_alone() {
            let knots = [knot(1.0, 0.9), knot(2.0, 0.5), knot(3.0, 0.2)];
            assert_eq!(enforce_monotone(&knots), knots.to_vec());
        }

        #[test]
        fn pools_adjacent_violators_into_their_weighted_mean() {
            // 0.4 then 0.6 is impossible: P(>2) cannot exceed P(>1). Both become
            // 0.5, which is the closest non-increasing curve rather than a
            // directional guess.
            let fixed = enforce_monotone(&[
                knot(1.0, 0.9),
                knot(2.0, 0.4),
                knot(3.0, 0.6),
                knot(4.0, 0.1),
            ]);
            assert_eq!(
                survivals(&fixed),
                vec![(1.0, 0.9), (2.0, 0.5), (3.0, 0.5), (4.0, 0.1)]
            );
        }

        #[test]
        fn pools_a_run_of_violators_across_more_than_two_points() {
            let fixed = enforce_monotone(&[knot(1.0, 0.2), knot(2.0, 0.5), knot(3.0, 0.8)]);
            // The whole run is inconsistent, so it collapses to one level: their mean.
            assert_eq!(survivals(&fixed), vec![(1.0, 0.5), (2.0, 0.5), (3.0, 0.5)]);
        }

        #[test]
        fn clamps_into_zero_one() {
            let fixed = enforce_monotone(&[knot(1.0, 1.4), knot(2.0, -0.3)]);
            assert_eq!(survivals(&fixed), vec![(1.0, 1.0), (2.0, 0.0)]);
        }
    }

    mod implied_median {
        use super::*;

        #[test]
        fn interpolates_the_fifty_percent_crossing_between_bracketing_strikes() {
            // Survival falls 0.8 -> 0.2 across [100, 110]; half of that drop is at 105.
            let value = implied_median(&[knot(100.0, 0.8), knot(110.0, 0.2)]);
            assert_eq!(value, Some(105.0));
        }

        #[test]
        fn returns_the_strike_itself_when_it_sits_exactly_at_fifty_percent() {
            let value = implied_median(&[knot(100.0, 0.9), knot(110.0, 0.5), knot(120.0, 0.1)]);
            assert_eq!(value, Some(110.0));
        }

        #[test]
        fn returns_null_when_the_ladder_never_crosses_fifty_percent() {
            // Every strike is above the money: the median is outside the quoted
            // range and inventing one would be worse than saying so.
            assert_eq!(implied_median(&[knot(100.0, 0.3), knot(110.0, 0.1)]), None);
        }
    }

    mod implied_mean {
        use super::*;

        #[test]
        fn weights_each_bucket_by_its_mass_and_places_it_at_the_midpoint() {
            // Buckets: (100,110] 0.2 @105, (110,120] 0.5 @115, plus the two tails.
            // Tails: 0.2 below 100 placed at 95, 0.1 above 120 placed at 125.
            // 0.2*95 + 0.2*105 + 0.5*115 + 0.1*125 = 19 + 21 + 57.5 + 12.5 = 110
            let value = implied_mean(&[knot(100.0, 0.8), knot(110.0, 0.6), knot(120.0, 0.1)]);
            assert_eq!(value, Some(110.0));
        }

        #[test]
        fn needs_at_least_two_knots_to_have_a_bucket_at_all() {
            assert_eq!(implied_mean(&[knot(100.0, 0.5)]), None);
        }
    }

    mod tail_mass {
        use super::*;

        #[test]
        fn sums_the_mass_below_the_lowest_and_above_the_highest_strike() {
            let mass = tail_mass(&[knot(100.0, 0.97), knot(120.0, 0.02)]);
            assert_eq!(round_to(mass, 3), 0.05);
        }

        #[test]
        fn is_total_when_there_are_no_strikes_to_bracket_anything() {
            assert_eq!(tail_mass(&[]), 1.0);
        }
    }

    mod implied_price {
        use super::*;

        #[test]
        fn prices_a_nested_or_above_ladder() {
            let legs = [
                leg(Some(62500.0), None, 0.98),
                leg(Some(63000.0), None, 0.6),
                leg(Some(63500.0), None, 0.15),
                leg(Some(64000.0), None, 0.02),
            ];
            let result = implied_price(&legs, ImpliedMethod::Median);
            // Crossing lies between 63000 (0.6) and 63500 (0.15): 63000 + 500*(0.1/0.45)
            let value = result
                .value
                .expect("a ladder straddling 50% has a crossing");
            assert_eq!(value.round(), 63111.0);
            assert_eq!(result.method, ImpliedMethod::Median);
        }

        #[test]
        fn prices_a_disjoint_range_ladder_to_the_same_place_as_the_nested_one() {
            // Same distribution, expressed as buckets instead of as an "or above"
            // ladder. The two shapes must not disagree.
            let nested = implied_price(
                &[
                    leg(Some(100.0), None, 0.9),
                    leg(Some(110.0), None, 0.6),
                    leg(Some(120.0), None, 0.2),
                ],
                ImpliedMethod::Median,
            );
            let buckets = implied_price(
                &[
                    leg(Some(100.0), Some(110.0), 0.3),
                    leg(Some(110.0), Some(120.0), 0.4),
                    leg(Some(120.0), Some(130.0), 0.2),
                ],
                ImpliedMethod::Median,
            );
            // Bucket masses normalise to 1/3, 4/9, 2/9 → survival 1.0, 0.667, 0.222.
            let nested = nested.value.expect("the nested ladder crosses 50%");
            let buckets = buckets.value.expect("the bucket ladder crosses 50%");
            assert_eq!(nested.round(), 113.0);
            assert_eq!(buckets.round(), 114.0);
        }

        #[test]
        fn recovers_a_sane_median_from_a_ladder_full_of_dead_one_sided_wings() {
            // The failure this guards: 40 wings quoted "no bid, 1c ask" around a
            // live centre. Read as mids they contribute 0.2 of phantom mass and
            // drag the crossing; halved and normalised, the centre holds.
            let wing = quote_probability(None, Some(0.01), None).expect("a lone ask still quotes");
            let mut legs: Vec<ImpliedLeg> = Vec::new();
            for i in 0..10 {
                let strike = 900.0 + 10.0 * f64::from(i);
                legs.push(leg(Some(strike), Some(strike + 10.0), wing));
            }
            legs.push(leg(Some(1000.0), Some(1010.0), 0.5));
            legs.push(leg(Some(1010.0), Some(1020.0), 0.45));
            for i in 0..10 {
                let strike = 1020.0 + 10.0 * f64::from(i);
                legs.push(leg(Some(strike), Some(strike + 10.0), wing));
            }

            let result = implied_price(&legs, ImpliedMethod::Median);
            let value = result.value.expect("the live buckets straddle 50%");
            assert!(
                value > 1000.0 && value < 1020.0,
                "expected the crossing inside the live buckets, got {value}"
            );
        }

        #[test]
        fn reports_why_it_could_not_produce_a_number() {
            assert_eq!(
                implied_price(&[], ImpliedMethod::Median).reason,
                Some(ImpliedReason::NoStrikes)
            );
            assert_eq!(
                implied_price(
                    &[leg(Some(100.0), None, 0.3), leg(Some(110.0), None, 0.1)],
                    ImpliedMethod::Median,
                )
                .reason,
                Some(ImpliedReason::NoCrossing)
            );
        }

        #[test]
        fn never_lets_a_crossing_land_outside_the_quoted_strikes() {
            let result = implied_price(
                &[
                    leg(Some(100.0), None, 0.9),
                    leg(Some(110.0), None, 0.55),
                    leg(Some(120.0), None, 0.45),
                    leg(Some(130.0), None, 0.1),
                ],
                ImpliedMethod::Median,
            );
            let value = result.value.expect("the ladder straddles 50%");
            assert!((100.0..=130.0).contains(&value));
        }
    }
}
