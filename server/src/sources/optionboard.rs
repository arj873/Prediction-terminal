//! The option board, and everything derived from it.
//!
//! Two venues answer very different questions with very different payloads —
//! Deribit publishes a mark volatility and its own per-expiry forward, Nasdaq
//! publishes four prices and nothing else — so each source normalises into one
//! [`OptionBoard`] and every view is derived from that. The chain, the
//! volatility surface and the open-interest ladder are then the *same* numbers
//! seen three ways, rather than three pipelines that can disagree with each
//! other about what the delta of a contract is.
//!
//! The derivation is deliberately layered, because each step can fail
//! independently and a caller needs to know which one did:
//!
//!  1. **Forward.** The venue's, else fitted from put-call parity, else assumed
//!     from a configured rate. Recorded as `forward_source` either way.
//!  2. **Volatility.** The venue's mark IV, else solved from the book mid.
//!  3. **Greeks.** Only once 1 and 2 both produced a number. A contract with no
//!     volatility gets `None` Greeks, never zeroes.

use std::collections::HashMap;

use terminal_core::greeks::{
    black_greeks, black_price, breakeven, fit_forward, implied_vol, intrinsic_value, max_pain,
    moneyness, years_to_expiry, BlackInputs, PainRung, ParityPair, DEFAULT_PARITY_WINDOW,
};
use terminal_core::types::{
    AssetClass, ForwardSource, IvSource, OptionChain, OptionContract, OptionExpiry, OptionGreeks,
    OptionPositioning, OptionSmilePoint, OptionStrikeStat, OptionSurface, OptionTermPoint,
    OptionType,
};

use crate::error::UpstreamError;

type Result<T> = std::result::Result<T, UpstreamError>;

/// One live contract, normalised: prices per unit of underlying, in `currency`.
#[derive(Debug, Clone)]
pub struct BoardQuote {
    pub contract: String,
    pub option_type: OptionType,
    pub strike: f64,
    /// Expiry instant, unix seconds.
    pub expiry: i64,
    pub bid: Option<f64>,
    pub ask: Option<f64>,
    pub last: Option<f64>,
    /// The venue's own mark, where it publishes one.
    pub mark: Option<f64>,
    pub change: Option<f64>,
    pub volume: Option<f64>,
    pub open_interest: Option<f64>,
    /// The venue's own implied volatility, decimal. `None` when it publishes
    /// none.
    pub venue_iv: Option<f64>,
    /// The venue's own forward for this expiry. `None` when it publishes none.
    pub venue_forward: Option<f64>,
    /// The venue's own discount factor for this expiry. `None` when it
    /// publishes none.
    pub venue_discount: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct OptionBoard {
    pub symbol: String,
    pub name: String,
    pub asset_class: AssetClass,
    pub currency: String,
    pub venue: String,
    pub source: String,
    /// Underlying units per contract — 100 for US listed equity options, 1 on
    /// Deribit.
    pub contract_size: f64,
    pub spot: Option<f64>,
    pub quotes: Vec<BoardQuote>,
    pub note: String,
}

/* --------------------------------------------------------------- expiries */

/// Board → the expiry strip, soonest first.
#[must_use]
pub fn expiries_of(board: &OptionBoard, now: f64) -> Vec<OptionExpiry> {
    let mut order: Vec<i64> = Vec::new();
    let mut buckets: HashMap<i64, (usize, f64, f64)> = HashMap::new();

    for quote in &board.quotes {
        let bucket = buckets.entry(quote.expiry).or_insert_with(|| {
            order.push(quote.expiry);
            (0, 0.0, 0.0)
        });
        bucket.0 += 1;
        bucket.1 += quote.open_interest.unwrap_or(0.0);
        bucket.2 += quote.volume.unwrap_or(0.0);
    }

    let mut expiries: Vec<OptionExpiry> = order
        .into_iter()
        .map(|expiry| {
            let (contracts, open_interest, volume) = buckets[&expiry];
            #[allow(clippy::cast_precision_loss)]
            let seconds = expiry as f64;
            OptionExpiry {
                expiry,
                date: iso_date_of(expiry),
                years_to_expiry: years_to_expiry(seconds, now),
                // Round rather than floor: an expiry 23h50m out is "tomorrow",
                // not "today".
                days_to_expiry: ((seconds - now) / 86_400.0).round(),
                contracts,
                open_interest,
                volume,
            }
        })
        .collect();

    expiries.sort_by_key(|e| e.expiry);
    expiries
}

/// `YYYY-MM-DD` for a unix instant, UTC.
#[must_use]
pub fn iso_date_of(seconds: i64) -> String {
    let days = seconds.div_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02}")
}

/// Howard Hinnant's `civil_from_days`, the inverse of the one in
/// [`terminal_core::greeks`].
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    (if m <= 2 { y + 1 } else { y }, m as u32, d as u32)
}

/// Pick the expiry a request means.
///
/// An empty `wanted` means "the front month", which is the first expiry that has
/// not already passed — not simply the first on the board, because a chain
/// fetched at 16:05 on expiry day still lists the contracts that stopped trading
/// five minutes ago, and defaulting to them would show a board of frozen quotes.
pub fn resolve_expiry(expiries: &[OptionExpiry], wanted: &str, now: f64) -> Result<OptionExpiry> {
    if expiries.is_empty() {
        return Err(
            UpstreamError::not_found("No option expiries are listed for this underlying")
                .with_hint("The venue lists no live option board for this symbol."),
        );
    }

    #[allow(clippy::cast_precision_loss)]
    let live: Vec<&OptionExpiry> = expiries.iter().filter(|e| e.expiry as f64 > now).collect();

    let needle = wanted.trim().to_lowercase();
    if needle.is_empty() {
        return Ok(live
            .first()
            .map_or_else(|| expiries[expiries.len() - 1].clone(), |e| (*e).clone()));
    }

    // An exact date, or a unix instant.
    if let Some(exact) = expiries
        .iter()
        .find(|e| e.date == needle || e.expiry.to_string() == needle)
    {
        return Ok(exact.clone());
    }

    let pool: Vec<&OptionExpiry> = if live.is_empty() {
        expiries.iter().collect()
    } else {
        live
    };

    // `30d` / `3m` — the listed expiry closest to that horizon, which is how a
    // board is actually navigated when the exact Friday is not memorised.
    if let Some(days) = horizon_days(&needle) {
        let best = pool
            .iter()
            .min_by(|a, b| {
                (a.days_to_expiry - days)
                    .abs()
                    .total_cmp(&(b.days_to_expiry - days).abs())
            })
            .expect("the pool is non-empty");
        return Ok((*best).clone());
    }

    // A bare index into the strip: `1` is the front expiry.
    if needle.len() <= 2 && needle.chars().all(|c| c.is_ascii_digit()) {
        if let Ok(index) = needle.parse::<usize>() {
            if index >= 1 && index <= pool.len() {
                return Ok(pool[index - 1].clone());
            }
        }
    }

    let listed = expiries
        .iter()
        .take(12)
        .map(|e| e.date.clone())
        .collect::<Vec<_>>()
        .join(", ");
    let ellipsis = if expiries.len() > 12 { ", …" } else { "" };
    Err(
        UpstreamError::not_found(format!("No listed expiry matches \"{wanted}\"")).with_hint(
            format!(
                "Listed expiries: {listed}{ellipsis}. A horizon like `30d` picks the nearest one."
            ),
        ),
    )
}

/// `30d`, `3m`, `2w`, `1y` → a number of days.
fn horizon_days(needle: &str) -> Option<f64> {
    let (digits, unit) = needle.split_at(needle.find(|c: char| !c.is_ascii_digit())?);
    let unit = unit.trim();
    if digits.is_empty() || unit.len() != 1 {
        return None;
    }
    let count: f64 = digits.parse().ok()?;
    let per = match unit {
        "d" => 1.0,
        "w" => 7.0,
        "m" => 30.0,
        "y" => 365.0,
        _ => return None,
    };
    Some(count * per)
}

/* ---------------------------------------------------------------- forwards */

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ExpiryCarry {
    pub forward: Option<f64>,
    pub discount: Option<f64>,
    pub rate: Option<f64>,
    pub carry: Option<f64>,
    pub source: ForwardSource,
}

/// The forward and discount factor for one expiry.
///
/// Order matters and is the point of the function: a venue that publishes its
/// own forward is authoritative — its marks are struck against that number, so
/// using anything else would produce Greeks inconsistent with the prices next to
/// them. Only when the venue is silent does parity get a turn, and only when
/// parity fails is anything assumed.
#[must_use]
pub fn carry_for(
    quotes: &[BoardQuote],
    spot: Option<f64>,
    years: f64,
    assumed_rate: f64,
) -> ExpiryCarry {
    let none = ExpiryCarry {
        forward: None,
        discount: None,
        rate: None,
        carry: None,
        source: ForwardSource::Assumed,
    };
    let Some(spot) = spot.filter(|s| s.is_finite() && *s > 0.0) else {
        return none;
    };

    // 1. The venue's own forward.
    if let Some(venue_forward) = quotes
        .iter()
        .find_map(|q| q.venue_forward.filter(|f| f.is_finite() && *f > 0.0))
    {
        let discount = quotes
            .iter()
            .find_map(|q| q.venue_discount.filter(|d| *d > 0.0))
            .unwrap_or(1.0);
        let rate = if years > 0.0 {
            -discount.ln() / years
        } else {
            0.0
        };
        return ExpiryCarry {
            forward: Some(venue_forward),
            discount: Some(discount),
            rate: Some(rate),
            carry: Some(if years > 0.0 {
                rate - (venue_forward / spot).ln() / years
            } else {
                0.0
            }),
            source: ForwardSource::Venue,
        };
    }

    // 2. Put-call parity, from the quoted book.
    let mut order: Vec<u64> = Vec::new();
    let mut legs: HashMap<u64, (Option<f64>, Option<f64>)> = HashMap::new();
    for quote in quotes {
        let Some(mid) = mid_of(quote).filter(|m| *m > 0.0) else {
            continue;
        };
        let key = quote.strike.to_bits();
        let entry = legs.entry(key).or_insert_with(|| {
            order.push(key);
            (None, None)
        });
        match quote.option_type {
            OptionType::Call => entry.0 = Some(mid),
            OptionType::Put => entry.1 = Some(mid),
        }
    }

    let pairs: Vec<ParityPair> = order
        .into_iter()
        .filter_map(|key| match legs[&key] {
            (Some(call_price), Some(put_price)) => Some(ParityPair {
                strike: f64::from_bits(key),
                call_price,
                put_price,
            }),
            _ => None,
        })
        .collect();

    if let Some(fitted) = fit_forward(&pairs, spot, years, DEFAULT_PARITY_WINDOW, assumed_rate) {
        return ExpiryCarry {
            forward: Some(fitted.forward),
            discount: Some(fitted.discount),
            rate: Some(fitted.rate),
            carry: Some(fitted.carry),
            source: ForwardSource::Parity,
        };
    }

    // 3. Nothing in the market would say. Assume, and admit it.
    let discount = (-assumed_rate * years).exp();
    ExpiryCarry {
        forward: Some(spot / discount),
        discount: Some(discount),
        rate: Some(assumed_rate),
        carry: Some(0.0),
        source: ForwardSource::Assumed,
    }
}

/// Book mid, falling back to the venue's mark and then to the last print.
///
/// A one-sided book is *not* halved the way a Kalshi binary is: an equity option
/// bid at 4.20 with no offer is worth about 4.20, whereas a binary bid at 1¢
/// with no offer is worth somewhere in [0, 1¢]. The bounded payoff is what makes
/// the binary case different, and options do not have one.
#[must_use]
pub fn mid_of(quote: &BoardQuote) -> Option<f64> {
    let bid = quote.bid.filter(|b| b.is_finite() && *b > 0.0);
    let ask = quote.ask.filter(|a| a.is_finite() && *a > 0.0);

    if let (Some(bid), Some(ask)) = (bid, ask) {
        return Some((bid + ask) / 2.0);
    }
    if let Some(mark) = quote.mark.filter(|m| m.is_finite() && *m > 0.0) {
        return Some(mark);
    }
    bid.or(ask)
        .or_else(|| quote.last.filter(|l| l.is_finite() && *l > 0.0))
}

/* ------------------------------------------------------------- enrichment */

/// A board quote plus everything the model can say about it.
///
/// Volatility comes from the venue when it publishes one, because that is the
/// number its own marks and its own risk system use, and solving a slightly
/// different one from a stale mid would put two disagreeing vols on one screen.
#[must_use]
pub fn enrich(
    quote: &BoardQuote,
    spot: Option<f64>,
    carry: &ExpiryCarry,
    years: f64,
) -> OptionContract {
    let mid = mid_of(quote);
    let effective_spot = spot.or(carry.forward);

    let intrinsic =
        effective_spot.and_then(|s| intrinsic_value(quote.option_type, quote.strike, s));

    let mut contract = OptionContract {
        contract: quote.contract.clone(),
        option_type: quote.option_type,
        strike: quote.strike,
        expiry: quote.expiry,
        bid: quote.bid,
        ask: quote.ask,
        mid,
        last: quote.last,
        change: quote.change,
        mark: quote.mark,
        volume: quote.volume,
        open_interest: quote.open_interest,
        iv: None,
        iv_source: None,
        greeks: OptionGreeks::default(),
        theo: None,
        intrinsic,
        extrinsic: None,
        breakeven: breakeven(quote.option_type, quote.strike, mid),
        in_the_money: effective_spot.is_some_and(|s| match quote.option_type {
            OptionType::Call => s > quote.strike,
            OptionType::Put => s < quote.strike,
        }),
    };

    if let (Some(intrinsic), Some(mid)) = (intrinsic, mid) {
        // Can go slightly negative on a crossed or stale quote; that is real
        // information about the book, so it is not clamped away.
        contract.extrinsic = Some(mid - intrinsic);
    }

    let (Some(forward), Some(discount), Some(spot)) =
        (carry.forward, carry.discount, effective_spot)
    else {
        return contract;
    };

    let pricing = BlackInputs {
        spot,
        forward,
        strike: quote.strike,
        years,
        vol: 0.0,
        discount,
        option_type: quote.option_type,
    };

    let venue_iv = quote.venue_iv.filter(|v| v.is_finite() && *v > 0.0);
    let Some(iv) = venue_iv.or_else(|| implied_vol(mid, &pricing)) else {
        return contract;
    };

    contract.iv = Some(iv);
    contract.iv_source = Some(if venue_iv.is_some() {
        IvSource::Venue
    } else {
        IvSource::Solved
    });
    let priced = BlackInputs { vol: iv, ..pricing };
    contract.greeks = black_greeks(&priced);
    contract.theo = black_price(&priced);
    contract
}

/* -------------------------------------------------------------------- chain */

#[must_use]
pub fn chain_for(
    board: &OptionBoard,
    expiry: &OptionExpiry,
    expiries: &[OptionExpiry],
    assumed_rate: f64,
) -> OptionChain {
    let quotes: Vec<BoardQuote> = board
        .quotes
        .iter()
        .filter(|q| q.expiry == expiry.expiry)
        .cloned()
        .collect();
    let carry = carry_for(&quotes, board.spot, expiry.years_to_expiry, assumed_rate);

    let contracts: Vec<OptionContract> = quotes
        .iter()
        .map(|q| enrich(q, board.spot, &carry, expiry.years_to_expiry))
        .collect();

    let mut calls: Vec<OptionContract> = contracts
        .iter()
        .filter(|c| c.option_type == OptionType::Call)
        .cloned()
        .collect();
    let mut puts: Vec<OptionContract> = contracts
        .iter()
        .filter(|c| c.option_type == OptionType::Put)
        .cloned()
        .collect();
    calls.sort_by(|a, b| a.strike.total_cmp(&b.strike));
    puts.sort_by(|a, b| a.strike.total_cmp(&b.strike));

    OptionChain {
        symbol: board.symbol.clone(),
        name: board.name.clone(),
        asset_class: board.asset_class,
        currency: board.currency.clone(),
        spot: board.spot,
        forward: carry.forward,
        forward_source: carry.source,
        discount_factor: carry.discount,
        rate: carry.rate,
        carry: carry.carry,
        expiry: expiry.clone(),
        expiries: expiries.to_vec(),
        calls,
        puts,
        atm_iv: atm_vol(&contracts, carry.forward),
        contract_size: board.contract_size,
        venue: board.venue.clone(),
        source: board.source.clone(),
        note: board.note.clone(),
    }
}

/// At-the-money volatility: the smile interpolated at the forward.
///
/// Reading the vol of the single nearest strike would make the number jump
/// whenever spot crossed a rung, which on a $5-strike board is several times a
/// session. Interpolating between the two strikes that bracket the forward moves
/// continuously, which is what makes a term structure readable.
#[must_use]
pub fn atm_vol(contracts: &[OptionContract], forward: Option<f64>) -> Option<f64> {
    let forward = forward.filter(|f| f.is_finite())?;

    let rungs: Vec<Rung> = smile_rungs(contracts, Some(forward))
        .into_iter()
        .filter(|r| r.iv.is_some())
        .collect();
    if rungs.is_empty() {
        return None;
    }
    if rungs.len() == 1 {
        return rungs[0].iv;
    }

    let above = rungs.iter().find(|r| r.strike >= forward);
    let below = rungs.iter().rev().find(|r| r.strike <= forward);

    match (above, below) {
        (Some(above), Some(below)) => {
            if (above.strike - below.strike).abs() < f64::EPSILON {
                return above.iv;
            }
            let weight = (forward - below.strike) / (above.strike - below.strike);
            Some(below.iv? + weight * (above.iv? - below.iv?))
        }
        (Some(one), None) | (None, Some(one)) => one.iv,
        (None, None) => None,
    }
}

#[derive(Debug, Clone, Copy)]
struct Rung {
    strike: f64,
    call_iv: Option<f64>,
    put_iv: Option<f64>,
    iv: Option<f64>,
    volume: f64,
    open_interest: f64,
}

/// Collapse both legs at each strike into one volatility.
///
/// The out-of-the-money leg wins. In theory the two are equal — same strike,
/// same expiry, one distribution — and in practice the in-the-money leg is where
/// the spread is widest, the volume is thinnest and, on American equity options,
/// where the early-exercise premium sits. Averaging them drags the smile toward
/// whichever side is stale.
fn smile_rungs(contracts: &[OptionContract], forward: Option<f64>) -> Vec<Rung> {
    let mut order: Vec<u64> = Vec::new();
    let mut by_strike: HashMap<u64, Rung> = HashMap::new();

    for contract in contracts {
        let key = contract.strike.to_bits();
        let rung = by_strike.entry(key).or_insert_with(|| {
            order.push(key);
            Rung {
                strike: contract.strike,
                call_iv: None,
                put_iv: None,
                iv: None,
                volume: 0.0,
                open_interest: 0.0,
            }
        });
        match contract.option_type {
            OptionType::Call => rung.call_iv = contract.iv,
            OptionType::Put => rung.put_iv = contract.iv,
        }
        rung.volume += contract.volume.unwrap_or(0.0);
        rung.open_interest += contract.open_interest.unwrap_or(0.0);
    }

    let mut rungs: Vec<Rung> = order.into_iter().map(|key| by_strike[&key]).collect();
    for rung in &mut rungs {
        let out_of_the_money = forward.and_then(|forward| {
            if rung.strike >= forward {
                rung.call_iv
            } else {
                rung.put_iv
            }
        });
        rung.iv = out_of_the_money.or(rung.call_iv).or(rung.put_iv);
    }

    rungs.sort_by(|a, b| a.strike.total_cmp(&b.strike));
    rungs
}

/* ------------------------------------------------------------------ surface */

#[must_use]
pub fn surface_for(
    board: &OptionBoard,
    expiry: &OptionExpiry,
    expiries: &[OptionExpiry],
    assumed_rate: f64,
) -> OptionSurface {
    let quotes: Vec<BoardQuote> = board
        .quotes
        .iter()
        .filter(|q| q.expiry == expiry.expiry)
        .cloned()
        .collect();
    let carry = carry_for(&quotes, board.spot, expiry.years_to_expiry, assumed_rate);
    let contracts: Vec<OptionContract> = quotes
        .iter()
        .map(|q| enrich(q, board.spot, &carry, expiry.years_to_expiry))
        .collect();

    let smile: Vec<OptionSmilePoint> = smile_rungs(&contracts, carry.forward)
        .into_iter()
        .map(|rung| OptionSmilePoint {
            strike: rung.strike,
            moneyness: board.spot.and_then(|spot| moneyness(rung.strike, spot)),
            call_iv: rung.call_iv,
            put_iv: rung.put_iv,
            iv: rung.iv,
            volume: rung.volume,
            open_interest: rung.open_interest,
        })
        .collect();

    // The term structure is the same at-the-money calculation at every expiry,
    // so one board answers "is the front rich against the back?" without any
    // extra upstream traffic.
    let term: Vec<OptionTermPoint> = expiries
        .iter()
        .map(|candidate| {
            let slice: Vec<BoardQuote> = board
                .quotes
                .iter()
                .filter(|q| q.expiry == candidate.expiry)
                .cloned()
                .collect();
            let slice_carry =
                carry_for(&slice, board.spot, candidate.years_to_expiry, assumed_rate);
            let enriched: Vec<OptionContract> = slice
                .iter()
                .map(|q| enrich(q, board.spot, &slice_carry, candidate.years_to_expiry))
                .collect();
            OptionTermPoint {
                expiry: candidate.expiry,
                date: candidate.date.clone(),
                days_to_expiry: candidate.days_to_expiry,
                atm_iv: atm_vol(&enriched, slice_carry.forward),
                forward: slice_carry.forward,
                open_interest: candidate.open_interest,
                volume: candidate.volume,
            }
        })
        .collect();

    OptionSurface {
        symbol: board.symbol.clone(),
        name: board.name.clone(),
        asset_class: board.asset_class,
        spot: board.spot,
        expiry: expiry.clone(),
        forward: carry.forward,
        atm_iv: atm_vol(&contracts, carry.forward),
        smile,
        term,
        skew: risk_reversal(&contracts, 0.25),
        venue: board.venue.clone(),
        source: board.source.clone(),
        note: board.note.clone(),
    }
}

/// 25-delta risk reversal: the put wing's volatility minus the call wing's.
///
/// Positive means downside is bid — the shape of an equity index board almost
/// always, and the shape of a crypto board only when the market is frightened.
/// Quoted on delta rather than on strike so it is comparable across expiries and
/// across underlyings of wildly different price.
#[must_use]
pub fn risk_reversal(contracts: &[OptionContract], target: f64) -> Option<f64> {
    let nearest = |option_type: OptionType, wanted: f64| -> Option<f64> {
        let mut best: Option<(f64, f64)> = None;
        for contract in contracts {
            if contract.option_type != option_type {
                continue;
            }
            let (Some(iv), Some(delta)) = (contract.iv, contract.greeks.delta) else {
                continue;
            };
            let distance = (delta - wanted).abs();
            if best.is_none_or(|(closest, _)| distance < closest) {
                best = Some((distance, iv));
            }
        }
        // A board whose closest contract to 25-delta is at 60-delta has no wing
        // to speak of, and quoting one would be inventing a number.
        best.filter(|(distance, _)| *distance < 0.12)
            .map(|(_, iv)| iv)
    };

    Some(nearest(OptionType::Put, -target)? - nearest(OptionType::Call, target)?)
}

/* -------------------------------------------------------------- positioning */

#[must_use]
pub fn positioning_for(board: &OptionBoard, expiry: &OptionExpiry) -> OptionPositioning {
    let mut order: Vec<u64> = Vec::new();
    let mut by_strike: HashMap<u64, OptionStrikeStat> = HashMap::new();

    for quote in board.quotes.iter().filter(|q| q.expiry == expiry.expiry) {
        let key = quote.strike.to_bits();
        let stat = by_strike.entry(key).or_insert_with(|| {
            order.push(key);
            OptionStrikeStat {
                strike: quote.strike,
                call_open_interest: 0.0,
                put_open_interest: 0.0,
                call_volume: 0.0,
                put_volume: 0.0,
                pain_payout: None,
            }
        });
        match quote.option_type {
            OptionType::Call => {
                stat.call_open_interest += quote.open_interest.unwrap_or(0.0);
                stat.call_volume += quote.volume.unwrap_or(0.0);
            }
            OptionType::Put => {
                stat.put_open_interest += quote.open_interest.unwrap_or(0.0);
                stat.put_volume += quote.volume.unwrap_or(0.0);
            }
        }
    }

    let mut strikes: Vec<OptionStrikeStat> = order.into_iter().map(|key| by_strike[&key]).collect();
    strikes.sort_by(|a, b| a.strike.total_cmp(&b.strike));

    // The pain curve, not just its minimum: the shape is what shows whether the
    // low is a sharp pin or a flat basin the underlying could sit anywhere in.
    let ladder = strikes.clone();
    for stat in &mut strikes {
        let mut payout = 0.0;
        for other in &ladder {
            payout += (stat.strike - other.strike).max(0.0) * other.call_open_interest;
            payout += (other.strike - stat.strike).max(0.0) * other.put_open_interest;
        }
        stat.pain_payout = Some(payout * board.contract_size);
    }

    let total_call_open_interest = strikes.iter().map(|s| s.call_open_interest).sum::<f64>();
    let total_put_open_interest = strikes.iter().map(|s| s.put_open_interest).sum::<f64>();
    let total_call_volume = strikes.iter().map(|s| s.call_volume).sum::<f64>();
    let total_put_volume = strikes.iter().map(|s| s.put_volume).sum::<f64>();

    let rungs: Vec<PainRung> = strikes
        .iter()
        .map(|s| PainRung {
            strike: s.strike,
            call_open_interest: s.call_open_interest,
            put_open_interest: s.put_open_interest,
        })
        .collect();

    OptionPositioning {
        symbol: board.symbol.clone(),
        name: board.name.clone(),
        asset_class: board.asset_class,
        spot: board.spot,
        expiry: expiry.clone(),
        strikes,
        max_pain: max_pain(&rungs).map(|(strike, _)| strike),
        total_call_open_interest,
        total_put_open_interest,
        total_call_volume,
        total_put_volume,
        put_call_open_interest: (total_call_open_interest > 0.0)
            .then(|| total_put_open_interest / total_call_open_interest),
        put_call_volume: (total_call_volume > 0.0).then(|| total_put_volume / total_call_volume),
        venue: board.venue.clone(),
        source: board.source.clone(),
        note: board.note.clone(),
    }
}

#[cfg(test)]
mod tests {
    //! Derivation tests.
    //!
    //! These use a synthetic board rather than a captured one on purpose: what
    //! is being checked is the *layering* — that the forward comes from the
    //! venue before parity and from parity before an assumption, that a contract
    //! with no volatility gets no Greeks rather than zero ones, and that three
    //! views over one board cannot disagree. A real payload would test the
    //! parser instead, and both source modules already do that.

    use super::*;
    use terminal_core::greeks::black_price;

    const RATE: f64 = 0.04;
    /// A day the tests can treat as "now": 2026-08-24T00:00:00Z.
    const NOW: f64 = 1_787_529_600.0;
    const IN_30_DAYS: i64 = 1_787_529_600 + 30 * 86_400;
    const IN_60_DAYS: i64 = 1_787_529_600 + 60 * 86_400;

    fn quote(strike: f64, option_type: OptionType, expiry: i64) -> BoardQuote {
        BoardQuote {
            contract: format!(
                "TEST{expiry}{}{strike}",
                if option_type == OptionType::Call {
                    "C"
                } else {
                    "P"
                }
            ),
            option_type,
            strike,
            expiry,
            bid: None,
            ask: None,
            last: None,
            mark: None,
            change: None,
            volume: Some(10.0),
            open_interest: Some(100.0),
            venue_iv: None,
            venue_forward: None,
            venue_discount: None,
        }
    }

    /// A board priced off a known forward, so every derived number has a right
    /// answer that is not the implementation's own output.
    fn board_priced_at(spot: f64, forward: f64, vol: f64, expiry: i64, years: f64) -> OptionBoard {
        let discount = (-RATE * years).exp();
        let mut quotes = Vec::new();
        for step in -5..=5 {
            let strike = spot + f64::from(step) * 5.0;
            for option_type in [OptionType::Call, OptionType::Put] {
                let price = black_price(&terminal_core::greeks::BlackInputs {
                    spot,
                    forward,
                    strike,
                    years,
                    vol,
                    discount,
                    option_type,
                })
                .expect("the synthetic board prices");
                let mut q = quote(strike, option_type, expiry);
                q.bid = Some(price * 0.99);
                q.ask = Some(price * 1.01);
                quotes.push(q);
            }
        }
        OptionBoard {
            symbol: "TEST".to_owned(),
            name: "Test underlying".to_owned(),
            asset_class: AssetClass::Stock,
            currency: "USD".to_owned(),
            venue: "TEST".to_owned(),
            source: "test".to_owned(),
            contract_size: 100.0,
            spot: Some(spot),
            quotes,
            note: String::new(),
        }
    }

    /* ---------------------------------------------------------------- carry */

    #[test]
    fn prefers_the_venues_own_forward_over_anything_it_could_fit() {
        // A venue's marks are struck against its own forward, so using a fitted
        // one would produce Greeks inconsistent with the prices beside them.
        let mut board = board_priced_at(100.0, 101.0, 0.3, IN_30_DAYS, 30.0 / 365.0);
        for quote in &mut board.quotes {
            quote.venue_forward = Some(123.0);
            quote.venue_discount = Some(0.98);
        }

        let carry = carry_for(&board.quotes, Some(100.0), 30.0 / 365.0, RATE);
        assert_eq!(carry.source, ForwardSource::Venue);
        assert_eq!(carry.forward, Some(123.0));
        assert_eq!(carry.discount, Some(0.98));
    }

    #[test]
    fn fits_the_forward_from_parity_when_the_venue_states_none() {
        let years = 30.0 / 365.0;
        let board = board_priced_at(100.0, 101.0, 0.3, IN_30_DAYS, years);
        let carry = carry_for(&board.quotes, Some(100.0), years, RATE);

        assert_eq!(carry.source, ForwardSource::Parity);
        // The board was priced off a forward of 101, and parity recovers it from
        // the quotes alone — no rate input, no dividend calendar.
        let forward = carry.forward.expect("a fitted forward");
        assert!((forward - 101.0).abs() < 0.05, "fitted {forward}");
    }

    #[test]
    fn admits_an_assumption_rather_than_passing_one_off_as_a_quote() {
        // One strike is not a regression, so there is nothing to fit.
        let board = OptionBoard {
            quotes: vec![quote(100.0, OptionType::Call, IN_30_DAYS)],
            ..board_priced_at(100.0, 101.0, 0.3, IN_30_DAYS, 30.0 / 365.0)
        };
        let carry = carry_for(&board.quotes, Some(100.0), 30.0 / 365.0, RATE);
        assert_eq!(carry.source, ForwardSource::Assumed);
        assert_eq!(carry.rate, Some(RATE));
    }

    #[test]
    fn has_no_carry_at_all_without_a_spot() {
        let board = board_priced_at(100.0, 101.0, 0.3, IN_30_DAYS, 30.0 / 365.0);
        let carry = carry_for(&board.quotes, None, 30.0 / 365.0, RATE);
        assert_eq!(carry.forward, None);
        assert_eq!(carry.discount, None);
        assert_eq!(carry.source, ForwardSource::Assumed);
    }

    /* ------------------------------------------------------------------ mid */

    #[test]
    fn does_not_halve_a_one_sided_option_book_the_way_a_binary_is_halved() {
        // An equity option bid at 4.20 with no offer is worth about 4.20. A
        // binary bid at 1¢ with no offer is worth somewhere in [0, 1¢] — the
        // bounded payoff is what makes that case different, and options have no
        // such bound.
        let mut q = quote(100.0, OptionType::Call, IN_30_DAYS);
        q.bid = Some(4.20);
        assert_eq!(mid_of(&q), Some(4.20));

        q.ask = Some(4.60);
        assert_eq!(mid_of(&q), Some(4.40));
    }

    #[test]
    fn falls_back_through_mark_and_then_last() {
        let mut q = quote(100.0, OptionType::Call, IN_30_DAYS);
        assert_eq!(mid_of(&q), None, "an empty book has no mid");

        q.last = Some(3.0);
        assert_eq!(mid_of(&q), Some(3.0));

        q.mark = Some(3.5);
        assert_eq!(mid_of(&q), Some(3.5), "a venue mark beats a stale print");
    }

    /* -------------------------------------------------------------- enrich */

    #[test]
    fn gives_a_contract_with_no_volatility_no_greeks_rather_than_zero_ones() {
        let years = 30.0 / 365.0;
        let carry = ExpiryCarry {
            forward: Some(101.0),
            discount: Some(0.99),
            rate: Some(RATE),
            carry: Some(0.0),
            source: ForwardSource::Parity,
        };
        // No book at all, so no volatility can be solved.
        let contract = enrich(
            &quote(100.0, OptionType::Call, IN_30_DAYS),
            Some(100.0),
            &carry,
            years,
        );

        assert_eq!(contract.iv, None);
        assert_eq!(contract.iv_source, None);
        assert_eq!(contract.greeks, OptionGreeks::default());
        assert_eq!(contract.theo, None);
        // Everything that does not need a volatility is still answered.
        assert_eq!(contract.intrinsic, Some(0.0));
        assert!(!contract.in_the_money);
    }

    #[test]
    fn prefers_the_venues_volatility_and_says_which_it_used() {
        let years = 30.0 / 365.0;
        let carry = ExpiryCarry {
            forward: Some(101.0),
            discount: Some(0.99),
            rate: Some(RATE),
            carry: Some(0.0),
            source: ForwardSource::Venue,
        };

        let mut q = quote(100.0, OptionType::Call, IN_30_DAYS);
        q.bid = Some(3.0);
        q.ask = Some(3.2);

        let solved = enrich(&q, Some(100.0), &carry, years);
        assert_eq!(solved.iv_source, Some(IvSource::Solved));

        q.venue_iv = Some(0.42);
        let stated = enrich(&q, Some(100.0), &carry, years);
        assert_eq!(stated.iv_source, Some(IvSource::Venue));
        assert_eq!(stated.iv, Some(0.42));
        // Two vols on one screen is the thing this avoids: the theo is priced at
        // the vol reported, not at a different one.
        assert!(stated.theo.is_some());
    }

    #[test]
    fn keeps_a_negative_extrinsic_because_it_is_real_information_about_the_book() {
        let years = 30.0 / 365.0;
        let carry = ExpiryCarry {
            forward: Some(101.0),
            discount: Some(0.99),
            rate: Some(RATE),
            carry: Some(0.0),
            source: ForwardSource::Parity,
        };
        // A deep in-the-money call quoted below intrinsic — a crossed or stale
        // book, which a reader wants to see rather than have clamped away.
        let mut q = quote(50.0, OptionType::Call, IN_30_DAYS);
        q.bid = Some(40.0);
        q.ask = Some(40.2);

        let contract = enrich(&q, Some(100.0), &carry, years);
        assert_eq!(contract.intrinsic, Some(50.0));
        assert!(
            contract.extrinsic.expect("an extrinsic") < 0.0,
            "extrinsic was {:?}",
            contract.extrinsic
        );
    }

    /* -------------------------------------------------------------- expiry */

    #[test]
    fn defaults_to_the_first_expiry_that_has_not_already_passed() {
        // A chain fetched at 16:05 on expiry day still lists the contracts that
        // stopped trading five minutes ago; defaulting to them would show a
        // board of frozen quotes.
        let expired = OptionExpiry {
            expiry: 1_787_529_600 - 86_400,
            date: "2026-08-23".to_owned(),
            years_to_expiry: 0.0,
            days_to_expiry: -1.0,
            contracts: 4,
            open_interest: 0.0,
            volume: 0.0,
        };
        let live = OptionExpiry {
            expiry: IN_30_DAYS,
            date: "2026-09-23".to_owned(),
            years_to_expiry: 30.0 / 365.0,
            days_to_expiry: 30.0,
            contracts: 4,
            open_interest: 0.0,
            volume: 0.0,
        };

        let chosen = resolve_expiry(&[expired.clone(), live.clone()], "", NOW).unwrap();
        assert_eq!(chosen.date, live.date);

        // Named explicitly, an expired one is still reachable.
        let named = resolve_expiry(&[expired.clone(), live], "2026-08-23", NOW).unwrap();
        assert_eq!(named.date, expired.date);
    }

    #[test]
    fn picks_the_listed_expiry_nearest_a_horizon() {
        let strip = vec![
            OptionExpiry {
                expiry: IN_30_DAYS,
                date: "2026-09-23".to_owned(),
                years_to_expiry: 30.0 / 365.0,
                days_to_expiry: 30.0,
                contracts: 4,
                open_interest: 0.0,
                volume: 0.0,
            },
            OptionExpiry {
                expiry: IN_60_DAYS,
                date: "2026-10-23".to_owned(),
                years_to_expiry: 60.0 / 365.0,
                days_to_expiry: 60.0,
                contracts: 4,
                open_interest: 0.0,
                volume: 0.0,
            },
        ];

        assert_eq!(
            resolve_expiry(&strip, "30d", NOW).unwrap().days_to_expiry,
            30.0
        );
        assert_eq!(
            resolve_expiry(&strip, "2m", NOW).unwrap().days_to_expiry,
            60.0
        );
        assert_eq!(
            resolve_expiry(&strip, "1w", NOW).unwrap().days_to_expiry,
            30.0
        );
        // A bare index walks the strip.
        assert_eq!(
            resolve_expiry(&strip, "2", NOW).unwrap().days_to_expiry,
            60.0
        );
    }

    #[test]
    fn names_the_listed_expiries_when_it_cannot_match_one() {
        let strip = vec![OptionExpiry {
            expiry: IN_30_DAYS,
            date: "2026-09-23".to_owned(),
            years_to_expiry: 30.0 / 365.0,
            days_to_expiry: 30.0,
            contracts: 4,
            open_interest: 0.0,
            volume: 0.0,
        }];
        let error = resolve_expiry(&strip, "next friday", NOW).expect_err("no such expiry");
        assert_eq!(error.code, "not_found");
        assert!(error.hint.unwrap_or_default().contains("2026-09-23"));

        let empty = resolve_expiry(&[], "", NOW).expect_err("an empty board has no expiry");
        assert_eq!(empty.code, "not_found");
    }

    /* --------------------------------------------------------- three views */

    #[test]
    fn three_views_over_one_board_report_the_same_numbers() {
        let years = 30.0 / 365.0;
        let board = board_priced_at(100.0, 101.0, 0.3, IN_30_DAYS, years);
        let expiries = expiries_of(&board, NOW);
        let expiry = &expiries[0];

        let chain = chain_for(&board, expiry, &expiries, RATE);
        let surface = surface_for(&board, expiry, &expiries, RATE);
        let positioning = positioning_for(&board, expiry);

        // Same forward, same at-the-money vol — the whole reason all three are
        // derived from one normalised board.
        assert_eq!(chain.forward, surface.forward);
        assert_eq!(chain.atm_iv, surface.atm_iv);
        assert_eq!(chain.spot, positioning.spot);

        // The open interest a reader totals in `OI` is the open interest the
        // chain is showing.
        let from_chain: f64 = chain
            .calls
            .iter()
            .filter_map(|c| c.open_interest)
            .sum::<f64>();
        assert!((from_chain - positioning.total_call_open_interest).abs() < 1e-9);
    }

    #[test]
    fn reads_the_at_the_money_vol_by_interpolating_rather_than_snapping_to_a_rung() {
        // Snapping would make the number jump every time spot crossed a strike,
        // which on a $5-strike board is several times a session.
        let years = 30.0 / 365.0;
        let board = board_priced_at(100.0, 102.5, 0.3, IN_30_DAYS, years);
        let expiries = expiries_of(&board, NOW);
        let chain = chain_for(&board, &expiries[0], &expiries, RATE);

        let atm = chain.atm_iv.expect("an at-the-money vol");
        // The board was priced at a flat 30% vol, and the forward sits exactly
        // between two strikes, so the interpolation has to land on it.
        assert!((atm - 0.3).abs() < 1e-3, "atm vol was {atm}");
    }

    #[test]
    fn takes_each_smile_rung_from_its_out_of_the_money_leg() {
        let years = 30.0 / 365.0;
        let mut board = board_priced_at(100.0, 101.0, 0.3, IN_30_DAYS, years);
        // Make the in-the-money leg of a low strike stale and cheap, which is
        // where the widest spread and the early-exercise premium live.
        for q in &mut board.quotes {
            if q.strike < 90.0 && q.option_type == OptionType::Call {
                q.bid = Some(0.01);
                q.ask = Some(0.02);
            }
        }

        let expiries = expiries_of(&board, NOW);
        let surface = surface_for(&board, &expiries[0], &expiries, RATE);
        let low = surface
            .smile
            .iter()
            .find(|r| (r.strike - 75.0).abs() < 1e-9)
            .expect("the 75 rung exists");

        // The rung reads its put — the out-of-the-money leg — not the wrecked
        // call, so the smile is not dragged toward whichever side is stale.
        assert_eq!(low.iv, low.put_iv);
        assert_ne!(low.iv, low.call_iv);
    }

    #[test]
    fn quotes_no_risk_reversal_when_the_board_has_no_wing_to_read_it_from() {
        let years = 30.0 / 365.0;
        // Three strikes clustered at the money: nothing near 25-delta.
        let mut board = board_priced_at(100.0, 101.0, 0.3, IN_30_DAYS, years);
        board.quotes.retain(|q| (q.strike - 100.0).abs() < 1e-9);

        let expiries = expiries_of(&board, NOW);
        let surface = surface_for(&board, &expiries[0], &expiries, RATE);
        assert_eq!(
            surface.skew, None,
            "a board with no wing has no risk reversal to quote"
        );
    }

    #[test]
    fn scales_the_pain_curve_by_the_contract_size() {
        let years = 30.0 / 365.0;
        let board = board_priced_at(100.0, 101.0, 0.3, IN_30_DAYS, years);
        let expiries = expiries_of(&board, NOW);
        let positioning = positioning_for(&board, &expiries[0]);

        assert!(positioning.max_pain.is_some());
        // 100 shares to a contract: a payout quoted per share would be a
        // hundredth of the money actually at stake.
        let curve: Vec<f64> = positioning
            .strikes
            .iter()
            .filter_map(|s| s.pain_payout)
            .collect();
        assert_eq!(curve.len(), positioning.strikes.len());
        assert!(curve.iter().any(|p| *p > 0.0));

        // The minimum of the published curve is the strike reported as max pain.
        let lowest = positioning
            .strikes
            .iter()
            .min_by(|a, b| a.pain_payout.unwrap().total_cmp(&b.pain_payout.unwrap()))
            .unwrap();
        assert_eq!(positioning.max_pain, Some(lowest.strike));
    }

    #[test]
    fn rounds_days_to_expiry_rather_than_flooring_it() {
        // An expiry 23h50m out is "tomorrow", not "today".
        let almost_a_day = OptionBoard {
            quotes: vec![quote(
                100.0,
                OptionType::Call,
                1_787_529_600 + 23 * 3_600 + 50 * 60,
            )],
            ..board_priced_at(100.0, 101.0, 0.3, IN_30_DAYS, 30.0 / 365.0)
        };
        let expiries = expiries_of(&almost_a_day, NOW);
        assert_eq!(expiries[0].days_to_expiry, 1.0);
    }
}
