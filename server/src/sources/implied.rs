//! Kalshi strike ladders → an implied price for the underlying, live and
//! historical.
//!
//! The maths lives in [`terminal_core::implied`]; this module is the plumbing
//! around it — which ladders price a given symbol, which of their strikes are
//! worth reading, and how to turn N per-strike candle series into one price
//! series.
//!
//! Two facts about Kalshi shape everything here:
//!
//! **The price ladders are not in the search corpus.** [`super::kalshi`] builds
//! its snapshot from the first 4,000 open events, and the crypto and index
//! ladders fall outside that window. So candidates are resolved by querying
//! `/events?series_ticker=` for a known set of series rather than by searching.
//!
//! **A ladder is the unit, not a contract.** One binary contract quotes one
//! probability; it takes a ladder of them to locate a price. That is why the
//! picker offers events and not markets.

use std::cmp::Ordering;
use std::collections::{BTreeSet, HashMap};
use std::sync::LazyLock;

use futures::StreamExt;
use terminal_core::implied::{implied_price, leg_from_strike, quote_probability, ImpliedLeg};
use terminal_core::types::{
    AssetClass, Candle, CandleInterval, ImpliedCandidate, ImpliedCandidatesResponse, ImpliedMethod,
    ImpliedPoint, ImpliedSeriesResponse, Market, VenueEvent,
};

use crate::app::AppState;
use crate::cache::ttl;
use crate::error::{Result, UpstreamError};
use crate::sources::corpus::sum_or_null;
use crate::sources::kalshi::{get_candles, get_event, list_events, ListEventsParams};

/* ---------------------------------------------------------------- registry */

/// One underlying the implied overlay can price.
#[derive(Debug)]
pub struct Underlying {
    /// Canonical symbol, as typed into the terminal.
    pub symbol: &'static str,
    pub name: &'static str,
    pub asset_class: AssetClass,
    /// What the spot source is asked for — a Yahoo symbol or Coinbase product.
    pub spot_symbol: &'static str,
    /// Kalshi series that quote this underlying's *settlement level*.
    ///
    /// Deliberately not "every series mentioning the asset". A running-maximum
    /// product (`KXWTIMAX`, "how high will oil get this year") is a ladder over
    /// the path's high, not over where it settles — collapsing one into an
    /// implied price would produce a confidently wrong number. Only
    /// terminal-value ladders belong here.
    ///
    /// Series that currently list no open events are harmless: they resolve to
    /// nothing and are skipped. Keeping them listed means the terminal keeps
    /// working as Kalshi rotates products in and out.
    pub series: &'static [&'static str],
    pub aliases: &'static [&'static str],
}

pub static UNDERLYINGS: &[Underlying] = &[
    // --- crypto -------------------------------------------------------------
    Underlying {
        symbol: "BTC",
        name: "Bitcoin",
        asset_class: AssetClass::Crypto,
        spot_symbol: "BTC-USD",
        aliases: &["BITCOIN", "XBT"],
        series: &["KXBTCD", "KXBTC", "KXBTCY", "KXBTCW", "KXBTCQ"],
    },
    Underlying {
        symbol: "ETH",
        name: "Ethereum",
        asset_class: AssetClass::Crypto,
        spot_symbol: "ETH-USD",
        aliases: &["ETHEREUM"],
        series: &["KXETHD", "KXETH", "KXETHY", "KXETHW"],
    },
    Underlying {
        symbol: "SOL",
        name: "Solana",
        asset_class: AssetClass::Crypto,
        spot_symbol: "SOL-USD",
        aliases: &["SOLANA"],
        series: &["KXSOLD", "KXSOLE", "KXSOL"],
    },
    Underlying {
        symbol: "XRP",
        name: "XRP",
        asset_class: AssetClass::Crypto,
        spot_symbol: "XRP-USD",
        aliases: &["RIPPLE"],
        series: &["KXXRPD", "KXXRP"],
    },
    Underlying {
        symbol: "BNB",
        name: "BNB",
        asset_class: AssetClass::Crypto,
        spot_symbol: "BNB-USD",
        aliases: &[],
        series: &["KXBNBD", "KXBNB", "KXBNBY"],
    },
    Underlying {
        symbol: "HYPE",
        name: "Hyperliquid",
        asset_class: AssetClass::Crypto,
        spot_symbol: "HYPE-USD",
        aliases: &[],
        series: &["KXHYPED", "KXHYPE"],
    },
    Underlying {
        symbol: "DOGE",
        name: "Dogecoin",
        asset_class: AssetClass::Crypto,
        spot_symbol: "DOGE-USD",
        aliases: &[],
        series: &["KXDOGED", "KXDOGE"],
    },
    Underlying {
        symbol: "ADA",
        name: "Cardano",
        asset_class: AssetClass::Crypto,
        spot_symbol: "ADA-USD",
        aliases: &[],
        series: &["KXADAD", "KXADA"],
    },
    Underlying {
        symbol: "AVAX",
        name: "Avalanche",
        asset_class: AssetClass::Crypto,
        spot_symbol: "AVAX-USD",
        aliases: &[],
        series: &["KXAVAXD", "KXAVAX"],
    },
    Underlying {
        symbol: "LINK",
        name: "Chainlink",
        asset_class: AssetClass::Crypto,
        spot_symbol: "LINK-USD",
        aliases: &[],
        series: &["KXLINKD", "KXLINK"],
    },
    Underlying {
        symbol: "LTC",
        name: "Litecoin",
        asset_class: AssetClass::Crypto,
        spot_symbol: "LTC-USD",
        aliases: &[],
        series: &["KXLTCD", "KXLTC"],
    },
    Underlying {
        symbol: "BCH",
        name: "Bitcoin Cash",
        asset_class: AssetClass::Crypto,
        spot_symbol: "BCH-USD",
        aliases: &[],
        series: &["KXBCHD", "KXBCH"],
    },
    Underlying {
        symbol: "DOT",
        name: "Polkadot",
        asset_class: AssetClass::Crypto,
        spot_symbol: "DOT-USD",
        aliases: &[],
        series: &["KXDOTD", "KXDOT"],
    },
    Underlying {
        symbol: "XLM",
        name: "Stellar",
        asset_class: AssetClass::Crypto,
        spot_symbol: "XLM-USD",
        aliases: &[],
        series: &["KXXLMD", "KXXLM"],
    },
    Underlying {
        symbol: "ZEC",
        name: "Zcash",
        asset_class: AssetClass::Crypto,
        spot_symbol: "ZEC-USD",
        aliases: &[],
        series: &["KXZECD", "KXZEC"],
    },
    // --- indices, commodities and FX ---------------------------------------
    Underlying {
        symbol: "^GSPC",
        name: "S&P 500",
        asset_class: AssetClass::Stock,
        spot_symbol: "^GSPC",
        aliases: &["SPX", "SP500", "INX", "GSPC"],
        series: &["KXINXU", "KXINX", "KXINXY"],
    },
    Underlying {
        symbol: "^NDX",
        name: "Nasdaq-100",
        asset_class: AssetClass::Stock,
        spot_symbol: "^NDX",
        aliases: &["NDX", "NASDAQ100", "NASDAQ"],
        series: &["KXNASDAQ100U", "KXNASDAQ100", "KXNASDAQ100Y"],
    },
    Underlying {
        symbol: "^DJI",
        name: "Dow Jones Industrial Average",
        asset_class: AssetClass::Stock,
        spot_symbol: "^DJI",
        aliases: &["DJI", "DJIA", "DOW"],
        series: &["KXDJI"],
    },
    Underlying {
        symbol: "GC=F",
        name: "Gold",
        asset_class: AssetClass::Stock,
        spot_symbol: "GC=F",
        aliases: &["GOLD", "XAU"],
        series: &["KXGOLDW", "KXGOLD", "KXGOLDY"],
    },
    Underlying {
        symbol: "CL=F",
        name: "WTI Crude Oil",
        asset_class: AssetClass::Stock,
        spot_symbol: "CL=F",
        aliases: &["WTI", "OIL", "CRUDE"],
        series: &["KXWTI", "KXWTIW"],
    },
    Underlying {
        symbol: "EURUSD=X",
        name: "EUR / USD",
        asset_class: AssetClass::Stock,
        spot_symbol: "EURUSD=X",
        aliases: &["EURUSD", "EUR"],
        series: &["KXEURUSD"],
    },
    Underlying {
        symbol: "USDJPY=X",
        name: "USD / JPY",
        asset_class: AssetClass::Stock,
        spot_symbol: "USDJPY=X",
        aliases: &["USDJPY", "JPY"],
        series: &["KXUSDJPY"],
    },
];

/// Canonical symbols and aliases, upper-cased, to their registry entry. Built
/// once: the table is static, and every `IMP` and every `STK` search row asks
/// it a question.
static BY_KEY: LazyLock<HashMap<String, &'static Underlying>> = LazyLock::new(|| {
    let mut by_key: HashMap<String, &'static Underlying> = HashMap::new();
    for underlying in UNDERLYINGS {
        by_key.insert(underlying.symbol.to_uppercase(), underlying);
        for alias in underlying.aliases {
            by_key.insert(alias.to_uppercase(), underlying);
        }
    }
    by_key
});

/// Resolve a typed symbol to a registry entry, or `None` if unknown.
pub fn find_underlying(symbol: &str) -> Option<&'static Underlying> {
    BY_KEY.get(&symbol.trim().to_uppercase()).copied()
}

/// Every symbol the implied overlay can price, for `HELP` and the picker.
pub fn list_underlyings() -> &'static [Underlying] {
    UNDERLYINGS
}

/* ------------------------------------------------------------- ladder legs */

/// Contracts with a numeric strike. Everything else cannot locate a price.
fn ladder_markets(event: &VenueEvent) -> Vec<Market> {
    event
        .markets
        .iter()
        .filter(|m| m.strike_type.is_some() && (m.floor_strike.is_some() || m.cap_strike.is_some()))
        .cloned()
        .collect()
}

/// A ladder needs enough rungs to bracket a crossing. Three is the floor.
const MIN_STRIKES: usize = 3;

/// Build legs from the live book.
fn live_legs(markets: &[Market]) -> Vec<ImpliedLeg> {
    let mut legs: Vec<ImpliedLeg> = Vec::new();
    for market in markets {
        let Some(p) = quote_probability(market.yes_bid, market.yes_ask, market.last_price) else {
            continue;
        };
        if let Some(leg) = leg_from_strike(
            market.strike_type,
            market.floor_strike,
            market.cap_strike,
            p,
            Some(&market.ticker),
        ) {
            legs.push(leg);
        }
    }
    legs
}

/* --------------------------------------------------------------- discovery */

/// Ladders that price `symbol`, newest expiry first, each with its live implied
/// price already computed.
///
/// Computing the implied price here rather than on selection is the point: the
/// picker can show what each ladder currently implies, so choosing between four
/// expiries is an informed choice rather than a guess.
pub async fn get_candidates(state: &AppState, symbol: &str) -> Result<ImpliedCandidatesResponse> {
    // An unmapped symbol is an empty result, not an error: the endpoint exists,
    // the question is well-formed, and the answer is "none". Answering 404 here
    // made every plain stock chart log a failed request while rendering fine.
    let Some(underlying) = find_underlying(symbol) else {
        let typed = symbol.trim().to_uppercase();
        let known: Vec<&str> = UNDERLYINGS.iter().map(|u| u.symbol).collect();
        return Ok(ImpliedCandidatesResponse {
            symbol: typed.clone(),
            asset_class: AssetClass::Stock,
            name: typed,
            candidates: Vec::new(),
            note: Some(format!(
                "Kalshi lists price ladders for major crypto, the index complex, gold, oil \
                 and two FX pairs — not for individual equities. Known symbols: {}.",
                known.join(", ")
            )),
        });
    };

    let candidates = state
        .cache()
        .cached(
            &format!("implied:candidates:{}", underlying.symbol),
            ttl::QUOTE,
            || async {
                // One request per series, run together. A series with no open
                // events resolves to nothing, which is the normal state for the
                // rotated-out ones.
                let per_series: Vec<Vec<VenueEvent>> =
                    futures::future::join_all(underlying.series.iter().map(|series| async move {
                        list_events(
                            state,
                            ListEventsParams {
                                series_ticker: Some((*series).to_string()),
                                status: Some("open".to_string()),
                                with_nested_markets: true,
                                limit: Some(200),
                                cursor: None,
                            },
                        )
                        .await
                        .map(|response| response.events)
                        .unwrap_or_default()
                    }))
                    .await;

                let mut found: Vec<ImpliedCandidate> = Vec::new();
                for event in per_series.into_iter().flatten() {
                    let markets = ladder_markets(&event);
                    if markets.len() < MIN_STRIKES {
                        continue;
                    }
                    found.push(describe_candidate(&event, &markets));
                }

                // Soonest expiry first: that is the ladder most tightly anchored
                // to spot, and the one a reader almost always wants as their
                // first overlay.
                found.sort_by(|a, b| {
                    expiry_key(&a.strike_date)
                        .cmp(expiry_key(&b.strike_date))
                        .then_with(|| {
                            b.volume24h
                                .partial_cmp(&a.volume24h)
                                .unwrap_or(Ordering::Equal)
                        })
                });
                Ok(found)
            },
        )
        .await?;

    Ok(ImpliedCandidatesResponse {
        symbol: underlying.symbol.to_string(),
        asset_class: underlying.asset_class,
        name: underlying.name.to_string(),
        candidates: (*candidates).clone(),
        note: candidates.is_empty().then(|| {
            format!(
                "Kalshi maps {} ladder series to {}, but none has an open event right now.",
                underlying.series.len(),
                underlying.name
            )
        }),
    })
}

/// A ladder with no stated expiry sorts last, as `strikeDate || '9999'` did.
///
/// Compared byte-wise where the TypeScript used `localeCompare`: every value
/// here is either an ISO-8601 stamp or the sentinel, and the two orderings agree
/// on those.
fn expiry_key(strike_date: &str) -> &str {
    if strike_date.is_empty() {
        "9999"
    } else {
        strike_date
    }
}

fn describe_candidate(event: &VenueEvent, markets: &[Market]) -> ImpliedCandidate {
    let legs = live_legs(markets);
    let result = implied_price(&legs, ImpliedMethod::Median);

    let strikes: Vec<f64> = markets
        .iter()
        .flat_map(|m| [m.floor_strike, m.cap_strike])
        .flatten()
        .collect();

    ImpliedCandidate {
        event_ticker: event.event_ticker.clone(),
        series_ticker: event.series_ticker.clone(),
        title: event.title.clone(),
        sub_title: event.sub_title.clone(),
        strike_date: first_close_time(markets),
        strikes: markets.len() as u32,
        quoted: legs.len() as u32,
        volume24h: sum_or_null(markets, |m| m.volume24h).unwrap_or(0.0),
        strike_low: strikes.iter().copied().reduce(f64::min),
        strike_high: strikes.iter().copied().reduce(f64::max),
        implied: result.value,
        tail_mass: (!result.knots.is_empty()).then_some(result.tail_mass),
    }
}

/// Every rung of a ladder closes together, so the first close time is the expiry.
fn first_close_time(markets: &[Market]) -> String {
    for market in markets {
        if !market.close_time.is_empty() {
            return market.close_time.clone();
        }
    }
    String::new()
}

/* --------------------------------------------------------------- historical */

/// Cap on strikes read per ladder.
///
/// Some ladders run to 400 rungs. Reading all of them would mean 400 upstream
/// candle requests for one overlay line, and the far wings contribute almost
/// nothing — a strike quoted at 99¢ or 1¢ moves the 50% crossing not at all.
/// The strikes that matter are the ones near the money, which is what
/// [`select_strikes`] keeps.
const MAX_STRIKES: usize = 48;

/// Parallel candle requests in flight. Enough to be quick, not enough to flood.
const FAN_OUT: usize = 6;

/// Choose which rungs to read.
///
/// Preference order is: has ever been quoted (open interest or volume), then
/// proximity to the live implied price. Ordering by proximity rather than by
/// volume matters — volume clusters at round numbers, but the 50% crossing is
/// located by the strikes bracketing it, whichever they are.
pub fn select_strikes(markets: &[Market], centre: Option<f64>) -> Vec<&Market> {
    let active: Vec<&Market> = markets
        .iter()
        .filter(|m| m.open_interest.unwrap_or(0.0) > 0.0 || m.volume.unwrap_or(0.0) > 0.0)
        .collect();

    let mut pool = if active.len() >= MIN_STRIKES {
        active
    } else {
        markets.iter().collect()
    };
    if pool.len() <= MAX_STRIKES {
        return pool;
    }

    let anchor = centre.unwrap_or_else(|| {
        let midpoints: Vec<f64> = pool.iter().map(|m| midpoint(m)).collect();
        median(&midpoints)
    });

    pool.sort_by(|a, b| {
        (midpoint(a) - anchor)
            .abs()
            .partial_cmp(&(midpoint(b) - anchor).abs())
            .unwrap_or(Ordering::Equal)
    });
    pool.truncate(MAX_STRIKES);
    pool
}

fn midpoint(market: &Market) -> f64 {
    let lo = market.floor_strike;
    let hi = market.cap_strike;
    match (lo, hi) {
        (Some(lo), Some(hi)) => (lo + hi) / 2.0,
        _ => lo.or(hi).unwrap_or(0.0),
    }
}

fn median(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal));
    let mid = sorted.len() / 2;
    if sorted.len() % 2 == 0 {
        (sorted[mid - 1] + sorted[mid]) / 2.0
    } else {
        sorted[mid]
    }
}

/// One rung of a ladder with the candles read for it.
#[derive(Debug, Clone)]
pub struct RungSeries {
    pub market: Market,
    pub candles: Vec<Candle>,
}

/// The implied price of the underlying through time, from one ladder.
///
/// Every rung is read as its own candle series, then the ladder is re-priced at
/// each bucket on the union of their timestamps. Within a bucket a rung that has
/// no candle yet contributes nothing, and one that has stopped printing carries
/// its last quote forward — which is what a resting book actually does, and the
/// alternative (dropping it) would silently shrink the ladder and drag the
/// crossing around.
pub async fn get_implied_series(
    state: &AppState,
    event_ticker: &str,
    interval: CandleInterval,
    start_ts: i64,
    end_ts: i64,
    method: ImpliedMethod,
) -> Result<ImpliedSeriesResponse> {
    // One overlay costs up to MAX_STRIKES candle requests, and panels poll. The
    // per-market candle cache alone would still re-run the whole fan-out on every
    // tick, so the assembled series is cached as a unit. The route quantises
    // `end`, which is what makes this key stable between two ticks a second apart.
    let series = state
        .cache()
        .cached(
            &format!(
                "implied:series:{event_ticker}:{}:{start_ts}:{end_ts}:{}",
                interval.minutes(),
                method.as_str()
            ),
            ttl::CANDLES,
            || compute_implied_series(state, event_ticker, interval, start_ts, end_ts, method),
        )
        .await?;

    Ok((*series).clone())
}

async fn compute_implied_series(
    state: &AppState,
    event_ticker: &str,
    interval: CandleInterval,
    start_ts: i64,
    end_ts: i64,
    method: ImpliedMethod,
) -> Result<ImpliedSeriesResponse> {
    let event = get_event(state, event_ticker).await?;
    let markets = ladder_markets(&event);

    if markets.len() < MIN_STRIKES {
        return Err(
            UpstreamError::bad_request(format!("{event_ticker} is not a price ladder")).with_hint(
                format!(
                    "An implied price needs at least {MIN_STRIKES} contracts with numeric strikes \
                     over one underlying. This event has {}.",
                    markets.len()
                ),
            ),
        );
    }

    // Anchor the strike selection on where the ladder currently prices the
    // underlying, so the rungs read are the ones bracketing the crossing.
    let live = implied_price(&live_legs(&markets), method);
    // Owned rather than borrowed: a closure taking `&Market` and returning an
    // async block needs a higher-ranked bound that the block cannot satisfy, so
    // the whole future stops being nameable by an axum handler. One clone per
    // rung buys a signature that composes.
    let selected: Vec<Market> = select_strikes(&markets, live.value)
        .into_iter()
        .cloned()
        .collect();
    let requested = selected.len();
    let series_ticker = event.series_ticker.clone();

    // `buffered`, not `buffer_unordered`: `build_points` walks the rungs in the
    // order they were selected, and so does `contributors`.
    let series: Vec<RungSeries> = futures::stream::iter(selected.into_iter().map(|market| {
        let series_ticker = series_ticker.clone();
        async move {
            let candles = get_candles(
                state,
                &market.ticker,
                interval,
                start_ts,
                end_ts,
                Some(&series_ticker),
            )
            .await
            .map(|response| response.candles)
            .unwrap_or_default();
            RungSeries { market, candles }
        }
    }))
    .buffered(FAN_OUT)
    .collect()
    .await;

    let contributing: Vec<RungSeries> = series
        .into_iter()
        .filter(|s| !s.candles.is_empty())
        .collect();
    let points = build_points(&contributing, method);

    Ok(ImpliedSeriesResponse {
        event_ticker: event.event_ticker.clone(),
        title: event.title.clone(),
        strike_date: first_close_time(&markets),
        method,
        interval,
        contributors: contributing
            .iter()
            .map(|s| s.market.ticker.clone())
            .collect(),
        skipped: (requested - contributing.len()) as u32,
        points,
    })
}

/// Re-price the ladder at every bucket on the union timeline.
///
/// A cursor per rung walks its candles forward as the timeline advances, so
/// this stays linear in total candles rather than quadratic — a 48-strike ladder
/// over a year of hourly bars is otherwise a few hundred million comparisons.
/// That cursor is the reason this is written as an index walk rather than a
/// search per bucket; it is ported as written.
pub fn build_points(series: &[RungSeries], method: ImpliedMethod) -> Vec<ImpliedPoint> {
    let timeline: BTreeSet<i64> = series
        .iter()
        .flat_map(|s| s.candles.iter().map(|c| c.time))
        .collect();

    let mut cursors: Vec<Option<usize>> = vec![None; series.len()];
    let mut points: Vec<ImpliedPoint> = Vec::with_capacity(timeline.len());

    for time in timeline {
        let mut legs: Vec<ImpliedLeg> = Vec::new();

        for (entry, cursor) in series.iter().zip(cursors.iter_mut()) {
            // Advance to the newest candle at or before `time`; never past it, so
            // a rung cannot be priced with information it did not yet have.
            let mut next = cursor.map_or(0, |index| index + 1);
            while next < entry.candles.len() && entry.candles[next].time <= time {
                *cursor = Some(next);
                next += 1;
            }
            let Some(index) = *cursor else { continue };

            let candle = &entry.candles[index];
            let Some(p) = quote_probability(candle.bid, candle.ask, Some(candle.close)) else {
                continue;
            };

            if let Some(leg) = leg_from_strike(
                entry.market.strike_type,
                entry.market.floor_strike,
                entry.market.cap_strike,
                p,
                Some(&entry.market.ticker),
            ) {
                legs.push(leg);
            }
        }

        if legs.len() < MIN_STRIKES {
            points.push(ImpliedPoint {
                time,
                value: None,
                strikes: legs.len() as u32,
                tail_mass: 1.0,
            });
            continue;
        }

        let result = implied_price(&legs, method);
        points.push(ImpliedPoint {
            time,
            value: result.value,
            strikes: legs.len() as u32,
            tail_mass: result.tail_mass,
        });
    }

    points
}

/// Historical implied-series assembly tests.
///
/// [`build_points`] is where N per-strike candle series become one price series,
/// and its two rules are easy to get wrong and invisible when you do: a rung
/// that has stopped printing must carry its last quote forward, and a rung must
/// never be priced with a candle from the future. Both are tested here against
/// ladders with deliberately ragged coverage.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use terminal_core::types::{StrikeType, Venue};
    use terminal_core::util::round4;
    use wiremock::matchers::{method as method_matcher, path as path_matcher, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// A ladder rung. Only the fields the maths reads are filled in.
    fn rung(ticker: &str, floor_strike: f64) -> Market {
        Market {
            venue: Venue::Kalshi,
            ticker: ticker.to_string(),
            event_ticker: "EVT".to_string(),
            series_ticker: "SER".to_string(),
            title: ticker.to_string(),
            yes_sub_title: String::new(),
            no_sub_title: String::new(),
            status: "open".to_string(),
            market_type: "binary".to_string(),
            yes_bid: None,
            yes_ask: None,
            no_bid: None,
            no_ask: None,
            mid: None,
            last_price: None,
            previous_price: None,
            change: None,
            volume: Some(0.0),
            volume24h: Some(0.0),
            open_interest: Some(0.0),
            liquidity: Some(0.0),
            open_time: String::new(),
            close_time: String::new(),
            expiration_time: String::new(),
            result: String::new(),
            rules_primary: String::new(),
            category: None,
            strike_type: Some(StrikeType::Greater),
            floor_strike: Some(floor_strike),
            cap_strike: None,
        }
    }

    /// A candle carrying a two-sided book, which is what the maths prefers.
    fn candle(time: i64, bid: f64, ask: f64) -> Candle {
        let mid = (bid + ask) / 2.0;
        Candle {
            time,
            open: mid,
            high: mid,
            low: mid,
            close: mid,
            volume: Some(0.0),
            open_interest: Some(0.0),
            traded: true,
            bid: Some(bid),
            ask: Some(ask),
        }
    }

    fn rung_series(ticker: &str, floor_strike: f64, candles: Vec<Candle>) -> RungSeries {
        RungSeries {
            market: rung(ticker, floor_strike),
            candles,
        }
    }

    /* ---------------------------------------------------------- buildPoints */

    #[test]
    fn prices_the_ladder_at_every_bucket_on_the_union_timeline() {
        let series = vec![
            rung_series(
                "A",
                100.0,
                vec![candle(10, 0.89, 0.91), candle(20, 0.79, 0.81)],
            ),
            rung_series(
                "B",
                110.0,
                vec![candle(10, 0.49, 0.51), candle(20, 0.39, 0.41)],
            ),
            rung_series(
                "C",
                120.0,
                vec![candle(10, 0.09, 0.11), candle(20, 0.04, 0.06)],
            ),
        ];

        let points = build_points(&series, ImpliedMethod::Median);
        assert_eq!(
            points.iter().map(|p| p.time).collect::<Vec<i64>>(),
            [10, 20]
        );
        // At t=10 the 50% crossing sits on strike 110 exactly.
        assert_eq!(points[0].value, Some(110.0));
        assert_eq!(points[0].strikes, 3);
        // At t=20 survival falls 0.8 -> 0.4 across [100, 110]: crossing at 107.5.
        assert_eq!(points[1].value, Some(107.5));
    }

    #[test]
    fn carries_a_rung_that_stopped_printing_forward_rather_than_dropping_it() {
        // A resting book does not vanish because no candle closed. Dropping the
        // rung would silently shrink the ladder and move the crossing.
        let series = vec![
            rung_series(
                "A",
                100.0,
                vec![candle(10, 0.89, 0.91), candle(20, 0.89, 0.91)],
            ),
            rung_series("B", 110.0, vec![candle(10, 0.49, 0.51)]),
            rung_series(
                "C",
                120.0,
                vec![candle(10, 0.09, 0.11), candle(20, 0.09, 0.11)],
            ),
        ];

        let points = build_points(&series, ImpliedMethod::Median);
        assert_eq!(points.len(), 2);
        assert_eq!(
            points[1].strikes, 3,
            "the stale rung should still contribute"
        );
        assert_eq!(points[1].value, Some(110.0));
    }

    #[test]
    fn never_prices_a_rung_with_a_candle_it_did_not_yet_have() {
        // Rung C only starts at t=20. At t=10 the ladder is two rungs, not three,
        // and must not borrow C's later quote.
        let series = vec![
            rung_series(
                "A",
                100.0,
                vec![candle(10, 0.89, 0.91), candle(20, 0.89, 0.91)],
            ),
            rung_series(
                "B",
                110.0,
                vec![candle(10, 0.49, 0.51), candle(20, 0.49, 0.51)],
            ),
            rung_series("C", 120.0, vec![candle(20, 0.09, 0.11)]),
        ];

        let points = build_points(&series, ImpliedMethod::Median);
        assert_eq!(points[0].strikes, 2);
        assert_eq!(points[1].strikes, 3);
    }

    #[test]
    fn emits_a_null_point_rather_than_a_guess_when_too_few_rungs_are_quoted() {
        let series = vec![
            rung_series("A", 100.0, vec![candle(10, 0.89, 0.91)]),
            rung_series("B", 110.0, vec![candle(20, 0.49, 0.51)]),
        ];

        let points = build_points(&series, ImpliedMethod::Median);
        assert_eq!(points[0].value, None);
        assert_eq!(points[0].strikes, 1);
    }

    #[test]
    fn returns_nothing_for_an_empty_ladder() {
        assert!(build_points(&[], ImpliedMethod::Median).is_empty());
    }

    /* -------------------------------------------------------- selectStrikes */

    /// 120 rungs, every one of them holding open interest.
    fn long_ladder() -> Vec<Market> {
        (0..120)
            .map(|i| Market {
                open_interest: Some(5.0),
                ..rung(&format!("K{i}"), 1000.0 + f64::from(i) * 10.0)
            })
            .collect()
    }

    #[test]
    fn keeps_the_whole_ladder_when_it_is_small_enough_to_read_entirely() {
        let ladder = long_ladder();
        let small = &ladder[0..12];
        assert_eq!(select_strikes(small, Some(1050.0)).len(), 12);
    }

    #[test]
    fn keeps_the_rungs_nearest_the_money_when_the_ladder_is_too_long() {
        // Ordering by proximity rather than by volume matters: volume clusters at
        // round numbers, but the crossing is located by its bracketing strikes.
        let ladder = long_ladder();
        let selected = select_strikes(&ladder, Some(1600.0));
        assert!(selected.len() < ladder.len());

        let strikes: Vec<f64> = selected
            .iter()
            .map(|m| m.floor_strike.expect("every rung has a floor"))
            .collect();
        let min = strikes.iter().copied().fold(f64::INFINITY, f64::min);
        let max = strikes.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        assert!(min < 1600.0 && max > 1600.0);

        let furthest_selected = strikes
            .iter()
            .map(|s| (s - 1600.0).abs())
            .fold(f64::NEG_INFINITY, f64::max);
        let furthest_available = ladder
            .iter()
            .map(|m| (m.floor_strike.expect("every rung has a floor") - 1600.0).abs())
            .fold(f64::NEG_INFINITY, f64::max);
        assert!(furthest_selected < furthest_available);
    }

    #[test]
    fn prefers_rungs_that_have_traded_or_hold_open_interest() {
        let mut mixed: Vec<Market> = (0..5)
            .map(|i| rung(&format!("DEAD{i}"), 100.0 + f64::from(i)))
            .collect();
        mixed.extend((0..5).map(|i| Market {
            volume: Some(10.0),
            ..rung(&format!("LIVE{i}"), 200.0 + f64::from(i))
        }));

        let selected = select_strikes(&mixed, Some(202.0));
        assert_eq!(selected.len(), 5);
        assert!(selected.iter().all(|m| m.ticker.starts_with("LIVE")));
    }

    #[test]
    fn falls_back_to_the_whole_ladder_when_nothing_has_traded_yet() {
        let cold: Vec<Market> = (0..6)
            .map(|i| rung(&format!("C{i}"), 100.0 + f64::from(i)))
            .collect();
        assert_eq!(select_strikes(&cold, None).len(), 6);
    }

    /* ------------------------------------------------------- findUnderlying */

    #[test]
    fn resolves_a_canonical_symbol() {
        assert_eq!(find_underlying("BTC").map(|u| u.name), Some("Bitcoin"));
        assert_eq!(
            find_underlying("btc").map(|u| u.asset_class),
            Some(AssetClass::Crypto)
        );
    }

    #[test]
    fn resolves_the_aliases_people_actually_type() {
        assert_eq!(find_underlying("SPX").map(|u| u.symbol), Some("^GSPC"));
        assert_eq!(find_underlying("NASDAQ").map(|u| u.symbol), Some("^NDX"));
        assert_eq!(find_underlying("GOLD").map(|u| u.symbol), Some("GC=F"));
        assert_eq!(find_underlying("WTI").map(|u| u.symbol), Some("CL=F"));
    }

    #[test]
    fn does_not_claim_a_symbol_it_has_no_ladder_for() {
        assert!(find_underlying("AAPL").is_none());
    }

    #[test]
    fn maps_only_terminal_value_ladders_never_running_maximum_products() {
        // `KXWTIMAX` asks how *high* oil gets, not where it settles. Collapsing it
        // into an implied price would produce a confidently wrong number.
        for underlying in [find_underlying("WTI"), find_underlying("BTC")] {
            let underlying = underlying.expect("both symbols are mapped");
            assert!(
                !underlying
                    .series
                    .iter()
                    .any(|s| s.contains("MAX") || s.contains("MIN")),
                "{}",
                underlying.series.join(",")
            );
        }
    }

    #[test]
    fn every_alias_resolves_to_its_own_entry() {
        for underlying in list_underlyings() {
            for alias in underlying.aliases {
                assert_eq!(
                    find_underlying(alias).map(|u| u.symbol),
                    Some(underlying.symbol),
                    "alias {alias} is claimed by another entry"
                );
            }
        }
    }

    /* -------------------------------------------------------------- upstream */

    /// A state whose Kalshi base points at the fixture server.
    fn state_for(server: &MockServer) -> AppState {
        AppState::new(Config {
            kalshi_api_base: server.uri(),
            ..Config::default()
        })
    }

    /// One "or above" rung, as `/events?with_nested_markets=true` sends it.
    fn raw_rung(event: &str, close: &str, strike: f64, bid: &str, ask: &str, vol: &str) -> String {
        format!(
            r#"{{
              "ticker": "{event}-T{strike:.2}",
              "event_ticker": "{event}",
              "close_time": "{close}",
              "strike_type": "greater",
              "floor_strike": {strike:.2},
              "yes_bid_dollars": "{bid}",
              "yes_ask_dollars": "{ask}",
              "volume_24h_fp": "{vol}",
              "open_interest_fp": "10.00"
            }}"#
        )
    }

    /// A three-rung XRP ladder: survival 0.9 / 0.5 / 0.1 across 2.5, 2.6, 2.7.
    fn xrp_rungs(event: &str, close: &str) -> String {
        format!(
            "{}, {}, {}",
            raw_rung(event, close, 2.50, "0.8900", "0.9100", "100.00"),
            raw_rung(event, close, 2.60, "0.4900", "0.5100", "150.00"),
            raw_rung(event, close, 2.70, "0.0900", "0.1100", "50.00"),
        )
    }

    /// Three open events on one series: a later expiry listed first, the nearest
    /// expiry second, and a two-rung ladder that is not a ladder at all.
    fn xrp_ladder() -> String {
        format!(
            r#"{{"events": [
              {{
                "event_ticker": "KXXRPD-26AUG20",
                "series_ticker": "KXXRPD",
                "title": "XRP price at 5pm",
                "sub_title": "Aug 20",
                "markets": [{}]
              }},
              {{
                "event_ticker": "KXXRPD-26AUG18",
                "series_ticker": "KXXRPD",
                "title": "XRP price at 5pm",
                "sub_title": "Aug 18",
                "markets": [{}]
              }},
              {{
                "event_ticker": "KXXRPD-26AUG19",
                "series_ticker": "KXXRPD",
                "title": "XRP price at 5pm",
                "sub_title": "Aug 19",
                "markets": [{}, {}]
              }}
            ]}}"#,
            xrp_rungs("KXXRPD-26AUG20", "2026-08-20T20:00:00Z"),
            xrp_rungs("KXXRPD-26AUG18", "2026-08-18T20:00:00Z"),
            raw_rung(
                "KXXRPD-26AUG19",
                "2026-08-19T20:00:00Z",
                2.50,
                "0.8900",
                "0.9100",
                "10.00"
            ),
            raw_rung(
                "KXXRPD-26AUG19",
                "2026-08-19T20:00:00Z",
                2.60,
                "0.4900",
                "0.5100",
                "10.00"
            ),
        )
    }

    #[tokio::test]
    async fn an_unknown_symbol_is_an_empty_list_with_a_note_not_a_404() {
        let server = MockServer::start().await;
        let response = get_candidates(&state_for(&server), " aapl ").await.unwrap();

        assert_eq!(response.symbol, "AAPL");
        assert_eq!(response.name, "AAPL");
        assert_eq!(response.asset_class, AssetClass::Stock);
        assert!(response.candidates.is_empty());
        let note = response.note.expect("an unmapped symbol carries a note");
        assert!(note.contains("not for individual equities"));
        // The note lists what *is* mapped, so the reader can retype.
        assert!(note.contains("BTC, ETH"));
        assert!(note.ends_with("USDJPY=X."));
    }

    #[tokio::test]
    async fn a_dead_series_is_skipped_rather_than_failing_the_picker() {
        let server = MockServer::start().await;
        // XRP maps two series. One answers, the other 404s.
        Mock::given(method_matcher("GET"))
            .and(path_matcher("/events"))
            .and(query_param("series_ticker", "KXXRPD"))
            .and(query_param("status", "open"))
            .and(query_param("with_nested_markets", "true"))
            .and(query_param("limit", "200"))
            .respond_with(ResponseTemplate::new(200).set_body_string(xrp_ladder()))
            .mount(&server)
            .await;
        Mock::given(method_matcher("GET"))
            .and(path_matcher("/events"))
            .and(query_param("series_ticker", "KXXRP"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let state = state_for(&server);
        let response = get_candidates(&state, "ripple").await.unwrap();

        // The alias resolves to the canonical entry, and the note is absent.
        assert_eq!(response.symbol, "XRP");
        assert_eq!(response.name, "XRP");
        assert_eq!(response.asset_class, AssetClass::Crypto);
        assert_eq!(response.note, None);

        // The two-rung ladder is dropped: three strikes are the floor. What is
        // left sorts soonest expiry first, whatever order Kalshi listed it in.
        assert_eq!(
            response
                .candidates
                .iter()
                .map(|c| c.event_ticker.as_str())
                .collect::<Vec<&str>>(),
            ["KXXRPD-26AUG18", "KXXRPD-26AUG20"]
        );

        let candidate = &response.candidates[0];
        assert_eq!(candidate.event_ticker, "KXXRPD-26AUG18");
        assert_eq!(candidate.series_ticker, "KXXRPD");
        assert_eq!(candidate.sub_title, "Aug 18");
        assert_eq!(candidate.strikes, 3);
        assert_eq!(candidate.quoted, 3);
        assert_eq!(candidate.volume24h, 300.0);
        assert_eq!(candidate.strike_low, Some(2.5));
        assert_eq!(candidate.strike_high, Some(2.7));
        assert_eq!(candidate.strike_date, "2026-08-18T20:00:00Z");
        // Survival hits 0.5 on the 2.60 rung exactly.
        assert_eq!(candidate.implied.map(round4), Some(2.6));
        // 10% below the lowest strike plus 10% above the highest.
        assert_eq!(candidate.tail_mass.map(|m| (m * 100.0).round()), Some(20.0));

        // The assembled list is cached under the canonical symbol.
        assert!(state
            .cache()
            .get::<Vec<ImpliedCandidate>>("implied:candidates:XRP")
            .await
            .is_some());
    }

    #[tokio::test]
    async fn a_symbol_whose_series_are_all_dark_says_so_in_a_note() {
        let server = MockServer::start().await;
        Mock::given(method_matcher("GET"))
            .and(path_matcher("/events"))
            .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"events": []}"#))
            .mount(&server)
            .await;

        let response = get_candidates(&state_for(&server), "DOGE").await.unwrap();
        assert!(response.candidates.is_empty());
        assert_eq!(
            response.note.as_deref(),
            Some("Kalshi maps 2 ladder series to Dogecoin, but none has an open event right now.")
        );
    }

    /// The same three-rung ladder, as `/events/{ticker}` sends it.
    fn xrp_event() -> String {
        format!(
            r#"{{"event": {{
              "event_ticker": "KXXRPD-26AUG18",
              "series_ticker": "KXXRPD",
              "title": "XRP price at 5pm",
              "markets": [{}]
            }}}}"#,
            xrp_rungs("KXXRPD-26AUG18", "2026-08-18T20:00:00Z"),
        )
    }

    /// Two hourly buckets of a two-sided book.
    fn raw_candles(bid_a: &str, ask_a: &str, bid_b: &str, ask_b: &str) -> String {
        format!(
            r#"{{"candlesticks": [
              {{
                "end_period_ts": 10,
                "price": {{"open_dollars": "{bid_a}", "close_dollars": "{ask_a}"}},
                "yes_bid": {{"close_dollars": "{bid_a}"}},
                "yes_ask": {{"close_dollars": "{ask_a}"}}
              }},
              {{
                "end_period_ts": 20,
                "price": {{"open_dollars": "{bid_b}", "close_dollars": "{ask_b}"}},
                "yes_bid": {{"close_dollars": "{bid_b}"}},
                "yes_ask": {{"close_dollars": "{ask_b}"}}
              }}
            ]}}"#
        )
    }

    fn candles_mock(ticker: &str, body: String) -> Mock {
        Mock::given(method_matcher("GET"))
            .and(path_matcher(format!(
                "/series/KXXRPD/markets/{ticker}/candlesticks"
            )))
            .respond_with(ResponseTemplate::new(200).set_body_string(body))
    }

    #[tokio::test]
    async fn a_ladder_becomes_one_price_series_cached_as_a_unit() {
        let server = MockServer::start().await;
        Mock::given(method_matcher("GET"))
            .and(path_matcher("/events/KXXRPD-26AUG18"))
            .respond_with(ResponseTemplate::new(200).set_body_string(xrp_event()))
            .mount(&server)
            .await;
        candles_mock(
            "KXXRPD-26AUG18-T2.50",
            raw_candles("0.8900", "0.9100", "0.7900", "0.8100"),
        )
        .mount(&server)
        .await;
        candles_mock(
            "KXXRPD-26AUG18-T2.60",
            raw_candles("0.4900", "0.5100", "0.3900", "0.4100"),
        )
        .mount(&server)
        .await;
        candles_mock(
            "KXXRPD-26AUG18-T2.70",
            raw_candles("0.0900", "0.1100", "0.0400", "0.0600"),
        )
        .mount(&server)
        .await;

        let state = state_for(&server);
        let series = get_implied_series(
            &state,
            "KXXRPD-26AUG18",
            CandleInterval::OneHour,
            0,
            3600,
            ImpliedMethod::Median,
        )
        .await
        .unwrap();

        assert_eq!(series.event_ticker, "KXXRPD-26AUG18");
        assert_eq!(series.title, "XRP price at 5pm");
        assert_eq!(series.strike_date, "2026-08-18T20:00:00Z");
        assert_eq!(series.method, ImpliedMethod::Median);
        assert_eq!(series.interval, CandleInterval::OneHour);
        assert_eq!(series.skipped, 0);
        // `buffered` keeps the rungs in selection order.
        assert_eq!(
            series.contributors,
            [
                "KXXRPD-26AUG18-T2.50",
                "KXXRPD-26AUG18-T2.60",
                "KXXRPD-26AUG18-T2.70"
            ]
        );

        assert_eq!(
            series.points.iter().map(|p| p.time).collect::<Vec<i64>>(),
            [10, 20]
        );
        assert_eq!(series.points[0].value.map(round4), Some(2.6));
        assert_eq!(series.points[0].strikes, 3);
        // Survival falls 0.8 -> 0.4 across [2.50, 2.60]: crossing at 2.575.
        assert_eq!(series.points[1].value.map(round4), Some(2.575));

        assert!(state
            .cache()
            .get::<ImpliedSeriesResponse>("implied:series:KXXRPD-26AUG18:60:0:3600:median")
            .await
            .is_some());
    }

    #[tokio::test]
    async fn a_rung_whose_candles_fail_is_counted_as_skipped_not_fatal() {
        let server = MockServer::start().await;
        Mock::given(method_matcher("GET"))
            .and(path_matcher("/events/KXXRPD-26AUG18"))
            .respond_with(ResponseTemplate::new(200).set_body_string(xrp_event()))
            .mount(&server)
            .await;
        candles_mock(
            "KXXRPD-26AUG18-T2.50",
            raw_candles("0.8900", "0.9100", "0.7900", "0.8100"),
        )
        .mount(&server)
        .await;
        candles_mock(
            "KXXRPD-26AUG18-T2.60",
            raw_candles("0.4900", "0.5100", "0.3900", "0.4100"),
        )
        .mount(&server)
        .await;
        Mock::given(method_matcher("GET"))
            .and(path_matcher(
                "/series/KXXRPD/markets/KXXRPD-26AUG18-T2.70/candlesticks",
            ))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let series = get_implied_series(
            &state_for(&server),
            "KXXRPD-26AUG18",
            CandleInterval::OneHour,
            0,
            3600,
            ImpliedMethod::Median,
        )
        .await
        .unwrap();

        assert_eq!(series.skipped, 1);
        assert_eq!(series.contributors.len(), 2);
        // Two rungs cannot make a ladder, so every bucket is an honest null.
        assert!(series.points.iter().all(|p| p.value.is_none()));
        assert!(series.points.iter().all(|p| p.strikes == 2));
    }

    #[tokio::test]
    async fn an_event_that_is_not_a_price_ladder_is_a_bad_request() {
        let server = MockServer::start().await;
        Mock::given(method_matcher("GET"))
            .and(path_matcher("/events/KXFEDDECISION-27JAN"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"{"event": {
                  "event_ticker": "KXFEDDECISION-27JAN",
                  "series_ticker": "KXFEDDECISION",
                  "title": "Fed decision in January",
                  "markets": [{"ticker": "KXFEDDECISION-27JAN-H26", "strike_type": "greater", "floor_strike": 3.75}]
                }}"#,
            ))
            .mount(&server)
            .await;

        let err = get_implied_series(
            &state_for(&server),
            "KXFEDDECISION-27JAN",
            CandleInterval::OneHour,
            0,
            3600,
            ImpliedMethod::Median,
        )
        .await
        .unwrap_err();

        assert_eq!(err.code, crate::error::codes::BAD_REQUEST);
        assert!(err.message.contains("is not a price ladder"));
        assert!(err
            .hint
            .expect("the hint counts the strikes")
            .contains("This event has 1."));
    }
}
