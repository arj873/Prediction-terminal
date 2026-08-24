//! Option pricing, implied volatility and the Greeks.
//!
//! The model is Black-76 — European options on a *forward* — rather than the
//! spot-and-a-risk-free-rate form of Black-Scholes, and that choice is the whole
//! design. A terminal has no business asking its user for a risk-free rate and a
//! dividend yield: both are unobservable, both are wrong by the time you have
//! typed them, and a Greek computed from a guessed carry is a guessed Greek.
//!
//! The forward is not a guess. It is quoted, in two different ways:
//!
//!  - **Crypto.** Deribit publishes `underlying_price` per expiry. That *is* the
//!    forward its own marks are struck against.
//!  - **Equities.** Put-call parity says `C - P` is linear in the strike:
//!    `C - P = DF·(F - K)`. Regressing the near-the-money call-put spread
//!    against the strike therefore recovers the discount factor (the slope) and
//!    the forward (the intercept) from quoted prices alone — no rate input, no
//!    dividend calendar. [`fit_forward`] does exactly that.
//!
//! Given `(spot, forward, discount)` the carry is fully determined —
//! `r = -ln(DF)/τ` and `q = r - ln(F/S)/τ` — so the classical Greeks fall out
//! without anything ever having been assumed. When parity cannot be fitted the
//! caller is told so explicitly rather than being handed a number derived from a
//! default rate that looks identical to a real one.
//!
//! Conventions match how a trader reads them, which is also what Deribit
//! publishes, so the two are directly comparable:
//!
//! | Greek | Units |
//! | --- | --- |
//! | delta | per 1 unit of underlying (spot delta, not forward delta) |
//! | gamma | delta per 1 unit of underlying |
//! | vega  | per **1 volatility point** (a move from 40% to 41%) |
//! | theta | per **calendar day** |
//! | rho   | per **1 percentage point** of rate |
//!
//! Nothing here touches the network. Like [`crate::implied`], it is checked
//! against arithmetic and against the venue's own published Greeks.

use crate::types::{OptionGreeks, OptionType};

/// Seconds in a 365-day year. Act/365 is the convention every options desk
/// quotes on.
const YEAR_SECONDS: f64 = 365.0 * 86_400.0;

/// Below this many years to expiry the model degenerates and the Greeks blow up.
const MIN_YEARS: f64 = 1e-8;

/// Volatility search bounds. 500% covers 0DTE crypto; 0.01% is effectively zero.
const MIN_VOL: f64 = 1e-4;
const MAX_VOL: f64 = 5.0;

/* ------------------------------------------------------------ distributions */

/// Standard normal pdf.
#[must_use]
pub fn normal_pdf(x: f64) -> f64 {
    (-0.5 * x * x).exp() / (2.0 * std::f64::consts::PI).sqrt()
}

/// Standard normal cdf, to full double precision (Hart's rational
/// approximation, in West's arrangement).
///
/// The obvious choice — Abramowitz & Stegun 7.1.26 — is accurate to 1.5e-7
/// absolute, which sounds far finer than any tick size and is not. In the wings
/// `N(d1)` and `N(d2)` are themselves of order 1e-8, so an *absolute* error of
/// 1.5e-7 is larger than the quantities being subtracted, and the price of a far
/// out-of-the-money contract becomes mostly approximation error. That is the one
/// place it matters most: the wings are where a volatility smile is read, and a
/// mispriced wing implies a wrong vol there.
///
/// This version is judged on *relative* error instead, for about the same
/// arithmetic: ~1e-16 through the body, degrading to ~1e-8 by N(-12). Measured
/// against high-precision references in the test suite, which pins the bound so
/// the claim cannot rot. For comparison, A&S's 1.5e-7 absolute at N(-8) is a
/// relative error of 2e8 — the answer is not wrong so much as absent.
#[must_use]
pub fn normal_cdf(x: f64) -> f64 {
    let z = x.abs();

    // Beyond 37 standard deviations the tail underflows a double anyway.
    if z > 37.0 {
        return if x > 0.0 { 1.0 } else { 0.0 };
    }

    let e = (-z * z / 2.0).exp();
    let tail = if z < 7.071_067_811_865_475 {
        let mut numerator = 3.526_249_659_989_11e-2 * z + 0.700_383_064_443_688;
        numerator = numerator * z + 6.373_962_203_531_65;
        numerator = numerator * z + 33.912_866_078_383;
        numerator = numerator * z + 112.079_291_497_871;
        numerator = numerator * z + 221.213_596_169_931;
        numerator = numerator * z + 220.206_867_912_376;

        let mut denominator = 8.838_834_764_831_84e-2 * z + 1.755_667_163_182_64;
        denominator = denominator * z + 16.064_177_579_207;
        denominator = denominator * z + 86.780_732_202_946_1;
        denominator = denominator * z + 296.564_248_779_674;
        denominator = denominator * z + 637.333_633_378_831;
        denominator = denominator * z + 793.826_512_519_948;
        denominator = denominator * z + 440.413_735_824_752;

        e * numerator / denominator
    } else {
        // Continued fraction, which is the stable form far out in the tail.
        let mut b = z + 0.65;
        b = z + 4.0 / b;
        b = z + 3.0 / b;
        b = z + 2.0 / b;
        b = z + 1.0 / b;
        e / (b * 2.506_628_274_631)
    };

    if x > 0.0 {
        1.0 - tail
    } else {
        tail
    }
}

/* ------------------------------------------------------------------- inputs */

/// Everything the model needs, all of it observable.
///
/// `spot` is separate from `forward` on purpose: the difference between them is
/// the carry, and the carry is what turns a forward delta into the spot delta a
/// hedger actually trades.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BlackInputs {
    /// Underlying price now.
    pub spot: f64,
    /// Forward price for this expiry.
    pub forward: f64,
    pub strike: f64,
    /// Time to expiry in years, act/365.
    pub years: f64,
    /// Volatility as a decimal — `0.42`, not `42`.
    pub vol: f64,
    /// `e^(-rT)`. `1` means undiscounted.
    pub discount: f64,
    pub option_type: OptionType,
}

struct Terms {
    d1: f64,
    d2: f64,
    sqrt_t: f64,
    /// `DF·F`, which equals `S·e^(-qT)` — the discounted forward.
    df_f: f64,
    /// Continuous rate implied by the discount factor.
    rate: f64,
    /// Continuous carry (dividend/borrow) yield implied by spot vs forward.
    carry: f64,
}

fn terms(inputs: &BlackInputs) -> Option<Terms> {
    let BlackInputs {
        spot,
        forward,
        strike,
        years,
        vol,
        discount,
        ..
    } = *inputs;

    if !forward.is_finite()
        || !strike.is_finite()
        || !spot.is_finite()
        || forward <= 0.0
        || strike <= 0.0
        || spot <= 0.0
        || years <= MIN_YEARS
        || vol <= 0.0
        || discount <= 0.0
    {
        return None;
    }

    let sqrt_t = years.sqrt();
    let d1 = ((forward / strike).ln() + 0.5 * vol * vol * years) / (vol * sqrt_t);
    let rate = -discount.ln() / years;

    Some(Terms {
        d1,
        d2: d1 - vol * sqrt_t,
        sqrt_t,
        df_f: discount * forward,
        rate,
        // F = S·e^((r-q)T)  ⇒  q = r - ln(F/S)/T. Recovered rather than assumed.
        carry: rate - (forward / spot).ln() / years,
    })
}

/* ------------------------------------------------------------------ pricing */

/// Black-76 value, in the underlying's currency.
///
/// Returns the intrinsic value at zero vol or zero time rather than `None`: an
/// expiring option is still worth something, and a chain that blanked its last
/// column on expiry day would be hiding the most-traded contracts on the board.
#[must_use]
pub fn black_price(inputs: &BlackInputs) -> Option<f64> {
    let BlackInputs {
        forward,
        strike,
        years,
        vol,
        discount,
        option_type,
        ..
    } = *inputs;

    if !forward.is_finite() || !strike.is_finite() || forward <= 0.0 || strike <= 0.0 {
        return None;
    }

    if years <= MIN_YEARS || vol <= 0.0 {
        let intrinsic = match option_type {
            OptionType::Call => forward - strike,
            OptionType::Put => strike - forward,
        };
        let df = if discount.is_finite() && discount > 0.0 {
            discount
        } else {
            1.0
        };
        return Some(intrinsic.max(0.0) * df);
    }

    let t = terms(inputs)?;

    Some(match option_type {
        OptionType::Call => discount * (forward * normal_cdf(t.d1) - strike * normal_cdf(t.d2)),
        OptionType::Put => discount * (strike * normal_cdf(-t.d2) - forward * normal_cdf(-t.d1)),
    })
}

/// The full Greek set, in trader units.
///
/// Every one is the classical Black-Scholes partial re-expressed through
/// `(S, F, DF)`; the identity `S·e^(-qT) = DF·F` is what lets the whole set be
/// written without `r` and `q` ever appearing as inputs.
#[must_use]
pub fn black_greeks(inputs: &BlackInputs) -> OptionGreeks {
    let Some(t) = terms(inputs) else {
        return OptionGreeks::default();
    };

    let BlackInputs {
        spot,
        strike,
        years,
        vol,
        discount,
        option_type,
        ..
    } = *inputs;
    let Terms {
        d1,
        d2,
        sqrt_t,
        df_f,
        rate,
        carry,
    } = t;
    let pdf = normal_pdf(d1);

    // Shared across both option types.
    let gamma = df_f * pdf / (spot * spot * vol * sqrt_t);
    let vega = df_f * pdf * sqrt_t;
    let decay = -df_f * pdf * vol / (2.0 * sqrt_t);

    match option_type {
        OptionType::Call => {
            let nd1 = normal_cdf(d1);
            let nd2 = normal_cdf(d2);
            OptionGreeks {
                delta: Some(df_f * nd1 / spot),
                gamma: Some(gamma),
                vega: Some(vega / 100.0),
                theta: Some((decay - rate * strike * discount * nd2 + carry * df_f * nd1) / 365.0),
                rho: Some(strike * years * discount * nd2 / 100.0),
            }
        }
        OptionType::Put => {
            let nmd1 = normal_cdf(-d1);
            let nmd2 = normal_cdf(-d2);
            OptionGreeks {
                delta: Some(-(df_f * nmd1) / spot),
                gamma: Some(gamma),
                vega: Some(vega / 100.0),
                theta: Some(
                    (decay + rate * strike * discount * nmd2 - carry * df_f * nmd1) / 365.0,
                ),
                rho: Some(-(strike * years * discount * nmd2) / 100.0),
            }
        }
    }
}

/* -------------------------------------------------------- implied volatility */

/// Invert [`black_price`] for the volatility.
///
/// Safeguarded Newton: take the Newton step when it stays inside the bracket and
/// bisect when it does not. Pure Newton is the wrong tool alone here because
/// vega collapses to zero in both wings, and a deep out-of-the-money contract
/// quoted at one tick is *exactly* where a chain most wants an answer.
///
/// Returns `None` when no volatility can produce the price — a quote below
/// intrinsic or above the forward is arbitrage or a stale print, not a vol.
/// Reporting it as `0` or as the bracket edge would draw a smile that dives to
/// the floor on the strikes with the widest spreads.
#[must_use]
pub fn implied_vol(price: Option<f64>, inputs: &BlackInputs) -> Option<f64> {
    let price = price?;
    let BlackInputs {
        forward,
        strike,
        years,
        discount,
        option_type,
        ..
    } = *inputs;

    if !price.is_finite() || price <= 0.0 {
        return None;
    }
    if years <= MIN_YEARS || forward <= 0.0 || strike <= 0.0 || discount <= 0.0 {
        return None;
    }

    // No vol can price below intrinsic or above the maximum payoff.
    let intrinsic = match option_type {
        OptionType::Call => forward - strike,
        OptionType::Put => strike - forward,
    }
    .max(0.0)
        * discount;
    let ceiling = discount
        * match option_type {
            OptionType::Call => forward,
            OptionType::Put => strike,
        };
    if price <= intrinsic || price >= ceiling {
        return None;
    }

    let at = |vol: f64| black_price(&BlackInputs { vol, ..*inputs });

    let mut lo = MIN_VOL;
    let mut hi = MAX_VOL;
    let mut vol = 0.5;

    // A price outside the bracket's reach has no solution to find.
    if at(hi).unwrap_or(0.0) < price {
        return None;
    }

    // Convergence is judged *relative* to the price. An absolute tolerance looks
    // reasonable and silently breaks the wings: a far out-of-the-money contract
    // can be worth 1e-39, and every volatility from 1% to 500% then satisfies
    // `|error| < 1e-10` — so the solver stops at whichever one it happened to
    // try and reports a vol that has nothing to do with the quote.
    let tolerance = price * 1e-10;

    for _ in 0..64 {
        let value = at(vol)?;

        let diff = value - price;
        if diff.abs() <= tolerance {
            return Some(vol);
        }

        if diff > 0.0 {
            hi = vol;
        } else {
            lo = vol;
        }

        let vega = d_price_d_vol(&BlackInputs { vol, ..*inputs });
        let step = if vega > 1e-12 {
            vol - diff / vega
        } else {
            f64::NAN
        };

        // Newton when it lands inside the bracket; bisection when it bolts.
        vol = if step.is_finite() && step > lo && step < hi {
            step
        } else {
            0.5 * (lo + hi)
        };

        if hi - lo < 1e-12 {
            break;
        }
    }

    // A solution pinned to a bracket edge is the solver giving up, not an answer.
    if vol > MIN_VOL * 2.0 && vol < MAX_VOL - 1e-6 {
        Some(vol)
    } else {
        None
    }
}

/// Raw `∂V/∂σ` — the un-scaled vega the solver needs, not the per-point one.
fn d_price_d_vol(inputs: &BlackInputs) -> f64 {
    terms(inputs).map_or(0.0, |t| t.df_f * normal_pdf(t.d1) * t.sqrt_t)
}

/* ------------------------------------------------------- forward from parity */

/// A strike where both legs are quoted, so parity has something to say.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ParityPair {
    pub strike: f64,
    pub call_price: f64,
    pub put_price: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ForwardFit {
    pub forward: f64,
    /// `e^(-rT)`.
    pub discount: f64,
    /// Continuous rate implied by the fit.
    pub rate: f64,
    /// Continuous dividend/borrow yield implied by the fit.
    pub carry: f64,
    /// Strikes the regression actually used.
    pub used: usize,
}

/// The band an annualised rate from a parity fit is allowed to fall in.
///
/// Not a view on where rates can go — a filter on where a *regression through
/// option quotes* can be trusted. Everything here is USD-denominated (Deribit
/// settles against USD indices, the equity board is US listed), so a discount
/// factor above 1 is a fitting artefact rather than a market. The small negative
/// allowance absorbs ordinary quote noise without admitting the artefacts.
const MIN_PLAUSIBLE_RATE: f64 = -0.02;
const MAX_PLAUSIBLE_RATE: f64 = 0.25;

/// The near-the-money window, and the rate to fall back on, that
/// [`fit_forward`] uses unless a caller says otherwise.
pub const DEFAULT_PARITY_WINDOW: f64 = 0.1;
pub const DEFAULT_ASSUMED_RATE: f64 = 0.04;

/// Recover the forward and the discount factor from quoted option prices.
///
/// `C - P = DF·(F - K)` is linear in `K` with slope `-DF` and intercept `DF·F`,
/// so an ordinary least-squares line through the call-put spread hands back
/// both. Using several strikes rather than the single at-the-money pair matters:
/// each spread carries two bid/ask spreads of noise, and the regression averages
/// that out instead of inheriting whichever strike happened to be crossed.
///
/// Only near-the-money strikes are eligible, for a reason beyond noise. Listed
/// equity options are **American**, and parity is a European identity — a deep
/// in-the-money put can carry an early-exercise premium that parity reads as a
/// distorted forward. That premium is negligible around the money, which is also
/// where the books are tightest, so the same window fixes both problems.
///
/// Returns `None` when the fit cannot be trusted, rather than a plausible
/// number. The caller is expected to say so out loud.
#[must_use]
pub fn fit_forward(
    pairs: &[ParityPair],
    spot: f64,
    years: f64,
    window: f64,
    assumed_rate: f64,
) -> Option<ForwardFit> {
    if !spot.is_finite() || spot <= 0.0 || years <= MIN_YEARS {
        return None;
    }

    // The window comparison is deliberately tolerant at its boundary. Strikes
    // land on round numbers, so a strike exactly `window` away from spot is
    // common — and `110 / 100 - 1` is `0.10000000000000009` in binary floating
    // point, which a strict `<= 0.1` throws away. Losing a boundary strike can
    // drop a sparse board below the three pairs a regression needs, turning a
    // perfectly fittable forward into an assumed one.
    let limit = window * (1.0 + 1e-9);

    let eligible: Vec<&ParityPair> = pairs
        .iter()
        .filter(|p| {
            p.strike.is_finite()
                && p.call_price.is_finite()
                && p.put_price.is_finite()
                && p.strike > 0.0
                && p.call_price > 0.0
                && p.put_price > 0.0
                && (p.strike / spot - 1.0).abs() <= limit
        })
        .collect();

    if eligible.len() < 3 {
        return None;
    }

    let (mut sum_k, mut sum_y, mut sum_kk, mut sum_ky) = (0.0, 0.0, 0.0, 0.0);
    for p in &eligible {
        let y = p.call_price - p.put_price;
        sum_k += p.strike;
        sum_y += y;
        sum_kk += p.strike * p.strike;
        sum_ky += p.strike * y;
    }

    #[allow(clippy::cast_precision_loss)]
    let n = eligible.len() as f64;
    let denominator = n * sum_kk - sum_k * sum_k;
    // Every eligible strike was the same number — a one-strike ladder in disguise.
    if denominator.abs() < 1e-9 {
        return None;
    }

    let slope = (n * sum_ky - sum_k * sum_y) / denominator;
    let intercept = (sum_y - slope * sum_k) / n;

    let discount = -slope;
    let forward = intercept / discount;
    let rate = -discount.ln() / years;

    // Sanity gates. A regression through crossed or stale quotes can produce an
    // arithmetically fine line that is financially nonsense.
    let usable = discount.is_finite()
        && discount > 0.0
        && forward.is_finite()
        && forward > 0.0
        && (forward / spot - 1.0).abs() <= 0.3
        // The gate that matters most in practice, and the one that is easy to
        // miss. The slope is a discount factor, so over a *one-day* expiry it is
        // ~0.999, and a couple of cents of bid/ask noise on it annualises to a
        // 30%+ rate. A negative fitted rate is the same failure in the other
        // direction: it means the regression put the discount factor above 1,
        // which no USD expiry does. Neither is a small error — the discount
        // factor scales every price on the board, and a 1.6% scaling moves
        // at-the-money implied volatility by about two points.
        //
        // The forward survives both cases: it is the *intercept*, which stays
        // well determined even when the slope is noise.
        && (MIN_PLAUSIBLE_RATE..=MAX_PLAUSIBLE_RATE).contains(&rate);

    if usable {
        return Some(ForwardFit {
            forward,
            discount,
            rate,
            carry: rate - (forward / spot).ln() / years,
            used: eligible.len(),
        });
    }

    // The two-parameter fit failed, but parity still knows where the forward is.
    // Pin the discount factor to the assumed rate and solve for the forward
    // alone: `F = K + (C - P) / DF`, averaged across the eligible strikes. One
    // parameter is far better conditioned than two, and it keeps the dividend
    // and borrow information that a bare `F = S/DF` would throw away.
    let pinned_discount = (-assumed_rate * years).exp();
    let total: f64 = eligible
        .iter()
        .map(|p| p.strike + (p.call_price - p.put_price) / pinned_discount)
        .sum();
    let pinned_forward = total / n;

    if !pinned_forward.is_finite() || pinned_forward <= 0.0 {
        return None;
    }
    if (pinned_forward / spot - 1.0).abs() > 0.3 {
        return None;
    }

    Some(ForwardFit {
        forward: pinned_forward,
        discount: pinned_discount,
        rate: assumed_rate,
        carry: assumed_rate - (pinned_forward / spot).ln() / years,
        used: eligible.len(),
    })
}

/* ---------------------------------------------------------------- moneyness */

/// Years between two unix-second instants, act/365, floored at zero.
#[must_use]
pub fn years_to_expiry(expiry_seconds: f64, now_seconds: f64) -> f64 {
    ((expiry_seconds - now_seconds) / YEAR_SECONDS).max(0.0)
}

/// `K/S`. Above 1 is a high strike, whichever way the contract pays.
///
/// Deliberately not "percent out of the money": that flips sign between calls
/// and puts, and a smile has to plot both on one axis.
#[must_use]
pub fn moneyness(strike: f64, spot: f64) -> Option<f64> {
    if !strike.is_finite() || !spot.is_finite() || spot <= 0.0 {
        return None;
    }
    Some(strike / spot)
}

/// Value at expiry if the underlying never moves from `spot`.
#[must_use]
pub fn intrinsic_value(option_type: OptionType, strike: f64, spot: f64) -> Option<f64> {
    if !strike.is_finite() || !spot.is_finite() {
        return None;
    }
    Some(
        match option_type {
            OptionType::Call => spot - strike,
            OptionType::Put => strike - spot,
        }
        .max(0.0),
    )
}

/// Where the underlying has to be at expiry for this contract to return its
/// premium — the number that decides whether a trade is worth putting on.
#[must_use]
pub fn breakeven(option_type: OptionType, strike: f64, premium: Option<f64>) -> Option<f64> {
    let premium = premium?;
    if !premium.is_finite() || !strike.is_finite() {
        return None;
    }
    Some(match option_type {
        OptionType::Call => strike + premium,
        OptionType::Put => strike - premium,
    })
}

/// One rung of an open-interest ladder, for [`max_pain`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PainRung {
    pub strike: f64,
    pub call_open_interest: f64,
    pub put_open_interest: f64,
}

/// The strike where the most option value expires worthless — "max pain".
///
/// For each candidate settlement price, total what every open contract would pay
/// out; the minimum is the price that costs writers least. It is a positioning
/// read rather than a forecast, and the panel labels it as one, but it is the
/// single most-asked-for number off an open-interest ladder.
#[must_use]
pub fn max_pain(strikes: &[PainRung]) -> Option<(f64, f64)> {
    let rungs: Vec<&PainRung> = strikes
        .iter()
        .filter(|s| s.strike.is_finite() && s.strike > 0.0)
        .collect();
    if rungs.is_empty() {
        return None;
    }

    let mut best: Option<(f64, f64)> = None;

    for candidate in &rungs {
        let mut payout = 0.0;
        for rung in &rungs {
            payout +=
                (candidate.strike - rung.strike).max(0.0) * finite_or_zero(rung.call_open_interest);
            payout +=
                (rung.strike - candidate.strike).max(0.0) * finite_or_zero(rung.put_open_interest);
        }
        if best.is_none_or(|(_, lowest)| payout < lowest) {
            best = Some((candidate.strike, payout));
        }
    }

    best
}

fn finite_or_zero(value: f64) -> f64 {
    if value.is_finite() {
        value
    } else {
        0.0
    }
}

/* --------------------------------------------------------------- expiry time */

/// The instant a US listed option stops trading: 16:00 America/New_York.
///
/// Hard-coding 20:00 UTC would be an hour out for five months of the year, and
/// an hour is not a rounding error on an expiry-day chain — it is a third of the
/// remaining life of a 0DTE contract, and theta and gamma both scale on it.
///
/// The TypeScript this replaces read the offset from the runtime's own timezone
/// database. Rust's standard library has none, and pulling one in for a single
/// rule would be the larger dependency: US daylight time has run from the second
/// Sunday in March to the first Sunday in November since 2007, and every listed
/// expiry is inside that era. The rule is encoded directly and pinned by tests
/// on both sides of both switches, which is what the tz database would have been
/// consulted for.
#[must_use]
pub fn equity_expiry_instant(iso_date: &str) -> Option<i64> {
    let text = iso_date.trim();
    let bytes = text.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return None;
    }
    if !bytes
        .iter()
        .enumerate()
        .all(|(i, b)| i == 4 || i == 7 || b.is_ascii_digit())
    {
        return None;
    }

    let year: i64 = text[0..4].parse().ok()?;
    let month: u32 = text[5..7].parse().ok()?;
    let day: u32 = text[8..10].parse().ok()?;
    if !(1..=12).contains(&month) || day < 1 || day > days_in_month(year, month) {
        return None;
    }

    // 16:00 local, expressed as UTC: 20:00 in winter, 21:00 in summer would be
    // the same clock time with the offset applied — EST is UTC-5, EDT UTC-4.
    let offset_hours = if is_us_daylight_time(year, month, day) {
        4
    } else {
        5
    };
    Some((days_from_civil(year, month, day) * 86_400) + (16 + offset_hours) * 3_600)
}

/// Is this *date* inside US daylight saving time?
///
/// A date rather than an instant, because 16:00 local is far from either
/// transition (both happen at 02:00 local), so the day alone settles it.
fn is_us_daylight_time(year: i64, month: u32, day: u32) -> bool {
    match month {
        ..=2 | 12 => false,
        4..=10 => true,
        3 => day >= nth_weekday_of_month(year, 3, 0, 2),
        // November: daylight time ends on the first Sunday.
        _ => day < nth_weekday_of_month(year, 11, 0, 1),
    }
}

/// The day of the month of the `nth` `weekday` (0 = Sunday) in `month`.
fn nth_weekday_of_month(year: i64, month: u32, weekday: i64, nth: u32) -> u32 {
    // 1970-01-01 was a Thursday, so day 0 has weekday 4.
    let first_weekday = (days_from_civil(year, month, 1) + 4).rem_euclid(7);
    let offset = (weekday - first_weekday).rem_euclid(7);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let first = (offset + 1) as u32;
    first + (nth - 1) * 7
}

fn days_in_month(year: i64, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ => {
            if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 {
                29
            } else {
                28
            }
        }
    }
}

/// Days since the Unix epoch, by Howard Hinnant's `days_from_civil`.
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let m = i64::from(month);
    let d = i64::from(day);
    let y = if m <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}
