//! Crypto options from Deribit's public API.
//!
//! There was no real choice to make here, which is worth stating plainly rather
//! than dressing up as a survey: Deribit is where crypto options trade. It has
//! consistently carried the large majority of global open interest in BTC and
//! ETH options, and its expiries are the ones every desk quotes off. A crypto
//! options panel sourced from anywhere else would be showing a shadow of the
//! real board.
//!
//! It also happens to fit this codebase's constraints exactly — the same ones
//! that picked Coinbase for spot in [`crate::sources::crypto`]: no key, no
//! account, no geo-fence, documented rate limits, and it answers from datacentre
//! IPs, which the equity feeds mostly do not.
//!
//! Two product families, and the difference leaks into the arithmetic:
//!
//!  - **Inverse** (`BTC-25DEC26-104000-C`) — coin-margined, and *quoted in the
//!    coin*. A mark of `0.623` means 0.623 BTC, so every price is multiplied by
//!    the index to reach the USD this terminal displays everywhere else.
//!  - **Linear** (`SOL_USDC-17AUG26-66-C`) — USDC-margined and already quoted in
//!    dollars per unit of underlying.
//!
//! The whole board arrives in two requests per currency — `get_instruments` for
//! the contract definitions and `get_book_summary_by_currency` for every quote —
//! rather than one request per contract, which for BTC alone would be 998.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde::Deserialize;
use terminal_core::types::{AssetClass, CandleInterval, OptionType, SpotCandle};

use crate::app::AppState;
use crate::cache::ttl;
use crate::error::UpstreamError;
use crate::http::FetchOptions;
use crate::sources::optionboard::{BoardQuote, OptionBoard};

type Result<T> = std::result::Result<T, UpstreamError>;

const TIMEOUT: Duration = Duration::from_secs(20);
const RETRIES: u32 = 1;

/// One underlying Deribit lists options on.
struct Underlying {
    symbol: &'static str,
    /// The currency bucket the board lives under — `BTC` for inverse,
    /// `USDC` for linear.
    currency: &'static str,
    linear: bool,
    name: &'static str,
}

/// The underlyings Deribit lists options on, and how to reach each one.
///
/// BTC and ETH are taken from the inverse book rather than their USDC-linear
/// twins: both exist, and the coin-margined board is the deeper and older one by
/// a wide margin, so it is the price discovery venue. Everything else is only
/// listed as USDC-linear.
const UNDERLYINGS: &[Underlying] = &[
    Underlying {
        symbol: "BTC",
        currency: "BTC",
        linear: false,
        name: "Bitcoin",
    },
    Underlying {
        symbol: "ETH",
        currency: "ETH",
        linear: false,
        name: "Ether",
    },
    Underlying {
        symbol: "SOL",
        currency: "USDC",
        linear: true,
        name: "Solana",
    },
    Underlying {
        symbol: "XRP",
        currency: "USDC",
        linear: true,
        name: "XRP",
    },
    Underlying {
        symbol: "AVAX",
        currency: "USDC",
        linear: true,
        name: "Avalanche",
    },
    Underlying {
        symbol: "HYPE",
        currency: "USDC",
        linear: true,
        name: "Hyperliquid",
    },
    Underlying {
        symbol: "TRX",
        currency: "USDC",
        linear: true,
        name: "TRON",
    },
];

/// Common spellings that mean one of the above.
const ALIASES: &[(&str, &str)] = &[
    ("XBT", "BTC"),
    ("BITCOIN", "BTC"),
    ("ETHEREUM", "ETH"),
    ("ETHER", "ETH"),
    ("SOLANA", "SOL"),
    ("RIPPLE", "XRP"),
    ("AVALANCHE", "AVAX"),
    ("HYPERLIQUID", "HYPE"),
    ("TRON", "TRX"),
];

fn underlying(symbol: &str) -> Option<&'static Underlying> {
    UNDERLYINGS.iter().find(|u| u.symbol == symbol)
}

/// The symbols this venue can be asked about, for the picker.
#[must_use]
pub fn underlying_symbols() -> Vec<(&'static str, &'static str)> {
    UNDERLYINGS.iter().map(|u| (u.symbol, u.name)).collect()
}

/// `BTC-USD`, `bitcoin`, `btc` → `BTC`. `None` when Deribit lists no board.
#[must_use]
pub fn resolve_symbol(symbol: &str) -> Option<&'static str> {
    let upper = symbol.trim().to_uppercase();
    let base = upper
        .split(['-', '_', '/'])
        .next()
        .unwrap_or(upper.as_str());
    let candidate = ALIASES
        .iter()
        .find(|(alias, _)| *alias == base)
        .map_or(base, |(_, canonical)| *canonical);
    underlying(candidate).map(|u| u.symbol)
}

#[must_use]
pub fn supports(symbol: &str) -> bool {
    resolve_symbol(symbol).is_some()
}

/* ------------------------------------------------------------------- wire */

#[derive(Debug, Deserialize)]
struct RawEnvelope<T> {
    result: Option<T>,
    error: Option<RawError>,
}

#[derive(Debug, Deserialize)]
struct RawError {
    message: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawInstrument {
    pub instrument_name: String,
    #[serde(default)]
    pub base_currency: Option<String>,
    #[serde(default)]
    pub price_index: Option<String>,
    #[serde(default)]
    pub option_type: Option<String>,
    #[serde(default)]
    pub strike: Option<f64>,
    #[serde(default)]
    pub expiration_timestamp: Option<f64>,
    #[serde(default)]
    pub contract_size: Option<f64>,
    #[serde(default)]
    pub is_active: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawSummary {
    pub instrument_name: String,
    #[serde(default)]
    pub bid_price: Option<f64>,
    #[serde(default)]
    pub ask_price: Option<f64>,
    #[serde(default)]
    pub mark_price: Option<f64>,
    #[serde(default)]
    pub last: Option<f64>,
    #[serde(default)]
    pub price_change: Option<f64>,
    #[serde(default)]
    pub volume: Option<f64>,
    #[serde(default)]
    pub open_interest: Option<f64>,
    #[serde(default)]
    pub mark_iv: Option<f64>,
    #[serde(default)]
    pub underlying_price: Option<f64>,
}

#[derive(Debug, Default, Deserialize)]
pub struct RawChart {
    #[serde(default)]
    pub ticks: Vec<f64>,
    #[serde(default)]
    pub open: Vec<f64>,
    #[serde(default)]
    pub high: Vec<f64>,
    #[serde(default)]
    pub low: Vec<f64>,
    #[serde(default)]
    pub close: Vec<f64>,
    #[serde(default)]
    pub volume: Vec<f64>,
}

#[derive(Debug, Deserialize)]
struct RawIndex {
    #[serde(default)]
    index_price: Option<f64>,
}

async fn get<T>(state: &AppState, path: &str, ttl: Duration) -> Result<Arc<T>>
where
    T: DeserializeOwned + Send + Sync + 'static,
{
    let url = format!(
        "{}{path}",
        state.config().deribit_api_base.trim_end_matches('/')
    );
    let key = format!("deribit:{path}");

    state
        .cache()
        .cached(&key, ttl, || async {
            let body = state
                .http()
                .fetch_json::<RawEnvelope<T>>(
                    &url,
                    FetchOptions::new().timeout(TIMEOUT).retries(RETRIES),
                )
                .await?;

            if let Some(error) = body.error {
                return Err(UpstreamError::new(
                    format!(
                        "Deribit rejected the request: {}",
                        error.message.as_deref().unwrap_or("unknown")
                    ),
                    "upstream_error",
                )
                .with_hint("Deribit answered with a JSON-RPC error rather than data."));
            }

            body.result.ok_or_else(|| {
                UpstreamError::new("Deribit returned no result", "bad_upstream_body")
            })
        })
        .await
}

fn num(value: Option<f64>) -> Option<f64> {
    value.filter(|v| v.is_finite())
}

/// Deribit reports an empty book side as `null`, and a dead one as `0`.
fn quote_price(value: Option<f64>) -> Option<f64> {
    num(value).filter(|v| *v > 0.0)
}

/* ------------------------------------------------------------------ board */

/// One Deribit instrument name, split into its parts.
#[derive(Debug, Clone, PartialEq)]
pub struct Instrument {
    pub base: String,
    pub currency: String,
    pub expiry_label: String,
    pub strike: f64,
    pub option_type: OptionType,
}

/// Split a Deribit instrument name into its parts.
///
/// Both families are handled by one rule because both put the expiry, the strike
/// and the leg in the same three trailing segments; only the head differs
/// (`BTC` versus `SOL_USDC`).
#[must_use]
pub fn parse_instrument(name: &str) -> Option<Instrument> {
    let upper = name.trim().to_uppercase();
    let parts: Vec<&str> = upper.split('-').collect();
    if parts.len() != 4 {
        return None;
    }
    let (head, expiry_label, strike, leg) = (parts[0], parts[1], parts[2], parts[3]);

    let (base, quote) = head.split_once('_').unwrap_or((head, head));

    let strike_value: f64 = strike.parse().ok()?;
    if !strike_value.is_finite() || strike_value <= 0.0 {
        return None;
    }
    let option_type = match leg {
        "C" => OptionType::Call,
        "P" => OptionType::Put,
        _ => return None,
    };

    Some(Instrument {
        base: base.to_owned(),
        currency: quote.to_owned(),
        expiry_label: expiry_label.to_owned(),
        strike: strike_value,
        option_type,
    })
}

/// Every live contract on one underlying, normalised to USD per unit.
///
/// Exported separately from the fetch so the normalisation can be tested against
/// a captured payload without a network — the conversion from an inverse mark in
/// BTC to a dollar price is exactly the kind of arithmetic that looks right and
/// is off by a factor of the index.
#[must_use]
pub fn build_board(
    symbol: &str,
    instruments: &[RawInstrument],
    summaries: &[RawSummary],
    index: Option<f64>,
) -> OptionBoard {
    let info = underlying(symbol).expect("build_board is called with a resolved symbol");
    let by_summary: HashMap<&str, &RawSummary> = summaries
        .iter()
        .map(|s| (s.instrument_name.as_str(), s))
        .collect();

    let live: Vec<&RawInstrument> = instruments
        .iter()
        .filter(|instrument| {
            instrument.is_active != Some(false)
                && instrument
                    .base_currency
                    .as_deref()
                    .is_some_and(|c| c.eq_ignore_ascii_case(symbol))
                && instrument.option_type.is_some()
                && instrument.strike.is_some_and(f64::is_finite)
                && instrument.expiration_timestamp.is_some_and(f64::is_finite)
        })
        .collect();

    // Inverse marks are quoted in the coin; multiply through by the index once,
    // here, so nothing downstream has to remember which family it is holding.
    let scale = if info.linear { Some(1.0) } else { index };

    let mut quotes: Vec<BoardQuote> = Vec::new();

    for instrument in &live {
        let Some(summary) = by_summary.get(instrument.instrument_name.as_str()) else {
            continue;
        };
        let Some(scale) = scale else { continue };

        let option_type = if instrument.option_type.as_deref() == Some("put") {
            OptionType::Put
        } else {
            OptionType::Call
        };
        let mark_iv = num(summary.mark_iv);
        let venue_forward = num(summary.underlying_price);

        #[allow(clippy::cast_possible_truncation)]
        let expiry = (instrument.expiration_timestamp.unwrap_or(0.0) / 1000.0).floor() as i64;

        quotes.push(BoardQuote {
            contract: instrument.instrument_name.clone(),
            option_type,
            strike: instrument.strike.unwrap_or(0.0),
            expiry,
            bid: scaled(quote_price(summary.bid_price), scale),
            ask: scaled(quote_price(summary.ask_price), scale),
            last: scaled(quote_price(summary.last), scale),
            mark: scaled(quote_price(summary.mark_price), scale),
            // `price_change` is a percentage move, not a price move.
            change: num(summary.price_change),
            volume: num(summary.volume),
            open_interest: num(summary.open_interest),
            // Deribit quotes volatility in percentage points.
            venue_iv: mark_iv.filter(|v| *v > 0.0).map(|v| v / 100.0),
            // `underlying_price` is the synthetic future for this expiry — the
            // forward Deribit's own marks are struck against.
            venue_forward,
            venue_discount: discount_from(venue_forward, index),
        });
    }

    let contract_size = live
        .iter()
        .find_map(|i| i.contract_size.filter(|s| s.is_finite()))
        .unwrap_or(1.0);

    let note = if info.linear {
        String::new()
    } else {
        format!(
            "{symbol} options are coin-margined and quoted in {symbol} on Deribit; \
             prices here are converted to USD at the index ({}).",
            index.map_or_else(|| "unavailable".to_owned(), |v| format!("{v:.2}"))
        )
    };

    OptionBoard {
        symbol: symbol.to_owned(),
        name: info.name.to_owned(),
        asset_class: AssetClass::Crypto,
        currency: "USD".to_owned(),
        venue: "Deribit".to_owned(),
        source: "deribit".to_owned(),
        contract_size,
        spot: index,
        quotes,
        note,
    }
}

fn scaled(value: Option<f64>, scale: f64) -> Option<f64> {
    value.map(|v| v * scale)
}

/// The discount factor for an expiry: `spot / forward`.
///
/// Deribit does publish an `interest_rate` per instrument, and it reads `0.0` —
/// while its own forward for a ten-month expiry sits 3.9% above the index, which
/// is a 4.5% annualised rate. Trusting the field over the forward mispriced a
/// long-dated contract by that same 3.9% and left every delta on the board
/// disagreeing with Deribit's own.
///
/// The forward is the market's statement of the carry, so it is what gets used.
/// A crypto option has no dividend, so all of that carry is interest and
/// `DF = S/F` follows exactly — which is also what makes the spot delta computed
/// here reproduce Deribit's published delta to five decimal places.
fn discount_from(forward: Option<f64>, spot: Option<f64>) -> Option<f64> {
    let (forward, spot) = (forward?, spot?);
    if forward <= 0.0 || spot <= 0.0 {
        return None;
    }
    let discount = spot / forward;
    // A forward more than 50% from spot is a bad tick, not a basis.
    (discount > 0.5 && discount < 1.5).then_some(discount)
}

pub async fn get_board(state: &AppState, symbol: &str) -> Result<OptionBoard> {
    let Some(resolved) = resolve_symbol(symbol) else {
        let listed = UNDERLYINGS
            .iter()
            .map(|u| u.symbol)
            .collect::<Vec<_>>()
            .join(", ");
        return Err(UpstreamError::not_found(format!(
            "Deribit lists no options on {}",
            symbol.trim().to_uppercase()
        ))
        .with_hint(format!("Crypto option boards exist for {listed}.")));
    };

    let info = underlying(resolved).expect("resolved");
    let currency = urlencoding::encode(info.currency).into_owned();
    let instruments_path =
        format!("/public/get_instruments?currency={currency}&kind=option&expired=false");
    let summaries_path =
        format!("/public/get_book_summary_by_currency?currency={currency}&kind=option");

    let (instrument_list, summary_list) = futures::try_join!(
        get::<Vec<RawInstrument>>(state, &instruments_path, ttl::CATALOGUE),
        get::<Vec<RawSummary>>(state, &summaries_path, ttl::QUOTE)
    )?;

    // Read the index name off the instrument rather than assembling it: Deribit
    // spells them `btc_usd` for inverse and `sol_usdc` for linear, and guessing
    // that mapping is a needless way to be wrong.
    let index_name = instrument_list
        .iter()
        .find(|i| {
            i.base_currency
                .as_deref()
                .is_some_and(|c| c.eq_ignore_ascii_case(resolved))
                && i.price_index.is_some()
        })
        .and_then(|i| i.price_index.clone())
        .unwrap_or_else(|| {
            format!(
                "{}_{}",
                resolved.to_lowercase(),
                if info.linear { "usdc" } else { "usd" }
            )
        });

    let index = get::<RawIndex>(
        state,
        &format!(
            "/public/get_index_price?index_name={}",
            urlencoding::encode(&index_name)
        ),
        ttl::QUOTE,
    )
    .await
    .ok()
    .and_then(|raw| num(raw.index_price));

    let board = build_board(resolved, &instrument_list, &summary_list, index);

    if board.quotes.is_empty() {
        return Err(UpstreamError::not_found(format!(
            "Deribit returned no live {resolved} option contracts"
        ))
        .with_hint("The board may be between listings. Try again shortly."));
    }

    Ok(board)
}

/* ---------------------------------------------------------------- history */

/// Deribit's chart resolutions, keyed by this terminal's candle intervals.
fn resolution(interval: CandleInterval) -> &'static str {
    match interval {
        CandleInterval::OneMinute => "1",
        CandleInterval::OneHour => "60",
        CandleInterval::OneDay => "1D",
    }
}

#[must_use]
pub fn normalise_chart(raw: &RawChart) -> Vec<SpotCandle> {
    let mut candles: Vec<SpotCandle> = Vec::new();

    for (i, tick) in raw.ticks.iter().enumerate() {
        let Some(close) = raw.close.get(i).copied().filter(|c| c.is_finite()) else {
            continue;
        };
        let open = raw.open.get(i).copied().unwrap_or(close);
        #[allow(clippy::cast_possible_truncation)]
        let time = (tick / 1000.0).floor() as i64;
        candles.push(SpotCandle {
            time,
            open,
            high: raw.high.get(i).copied().unwrap_or_else(|| open.max(close)),
            low: raw.low.get(i).copied().unwrap_or_else(|| open.min(close)),
            close,
            volume: raw.volume.get(i).copied().unwrap_or(0.0),
        });
    }

    candles.sort_by_key(|c| c.time);
    candles
}

/// Price history for one contract.
///
/// Inverse contracts print in the coin, so the series is converted bar by bar
/// against the perpetual's own history rather than against today's index — using
/// the current index for a month-old bar would bake this week's spot move into
/// an option price that never traded there, which is precisely the artefact that
/// makes a converted chart worse than an unconverted one. The perpetual tracks
/// the index within a few basis points, which is invisible at chart scale.
pub async fn get_history(
    state: &AppState,
    contract: &str,
    interval: CandleInterval,
    start_ts: i64,
    end_ts: i64,
) -> Result<(Vec<SpotCandle>, String)> {
    let Some(parsed) = parse_instrument(contract) else {
        return Err(UpstreamError::bad_request(format!(
            "\"{contract}\" is not a Deribit instrument name"
        ))
        .with_hint("Deribit contracts look like `BTC-25DEC26-104000-C`."));
    };

    let chart_path = |instrument: &str| {
        format!(
            "/public/get_tradingview_chart_data?instrument_name={}&start_timestamp={}&end_timestamp={}&resolution={}",
            urlencoding::encode(instrument),
            start_ts * 1000,
            end_ts * 1000,
            resolution(interval)
        )
    };

    let candles = get::<RawChart>(state, &chart_path(contract), ttl::CANDLES)
        .await
        .ok()
        .map(|raw| normalise_chart(&raw))
        .unwrap_or_default();

    if candles.is_empty() {
        return Ok((Vec::new(), String::new()));
    }

    // Linear contracts are already in dollars.
    if parsed.currency != parsed.base {
        return Ok((candles, String::new()));
    }

    let perpetual = get::<RawChart>(
        state,
        &chart_path(&format!("{}-PERPETUAL", parsed.base)),
        ttl::CANDLES,
    )
    .await
    .ok()
    .map(|raw| normalise_chart(&raw))
    .unwrap_or_default();

    if perpetual.is_empty() {
        return Ok((
            candles,
            format!(
                "Quoted in {} — Deribit's underlying history was unavailable to convert it to USD.",
                parsed.base
            ),
        ));
    }

    let by_time: HashMap<i64, &SpotCandle> = perpetual.iter().map(|c| (c.time, c)).collect();
    let converted: Vec<SpotCandle> = candles
        .iter()
        .filter_map(|candle| {
            let reference = by_time.get(&candle.time)?;
            Some(SpotCandle {
                time: candle.time,
                open: candle.open * reference.open,
                high: candle.high * reference.high,
                low: candle.low * reference.low,
                close: candle.close * reference.close,
                volume: candle.volume,
            })
        })
        .collect();

    Ok((
        converted,
        format!(
            "Converted from {} to USD bar-by-bar against {}-PERPETUAL.",
            parsed.base, parsed.base
        ),
    ))
}

/// Which underlying a contract name belongs to, for routing a contract lookup.
#[must_use]
pub fn symbol_of_contract(contract: &str) -> Option<&'static str> {
    let parsed = parse_instrument(contract)?;
    underlying(&parsed.base).map(|u| u.symbol)
}

#[cfg(test)]
mod tests {
    //! Deribit wire and normalisation tests.
    //!
    //! Both fixtures were captured live from `www.deribit.com/api/v2` and
    //! trimmed to two expiries — the front one and the longest-dated, which is
    //! where the forward basis is largest and therefore where every conversion
    //! error shows up. The fields kept are exactly the fields this module reads,
    //! so the file doubles as a statement of what is depended on.

    use super::*;
    use wiremock::matchers::{method, path as path_matcher, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use crate::config::Config;

    const INSTRUMENTS: &str = include_str!("fixtures/deribit_instruments.json");
    const SUMMARY: &str = include_str!("fixtures/deribit_summary.json");
    const INDEX: &str = include_str!("fixtures/deribit_index.json");

    fn state_for(server: &MockServer) -> AppState {
        AppState::new(Config {
            deribit_api_base: server.uri(),
            ..Config::default()
        })
    }

    fn fixture(text: &str) -> serde_json::Value {
        serde_json::from_str(text).expect("the captured fixture parses")
    }

    fn raw_instruments() -> Vec<RawInstrument> {
        serde_json::from_value(fixture(INSTRUMENTS)["result"].clone()).expect("instruments parse")
    }

    fn raw_summaries() -> Vec<RawSummary> {
        serde_json::from_value(fixture(SUMMARY)["result"].clone()).expect("summaries parse")
    }

    fn index_price() -> f64 {
        fixture(INDEX)["result"]["index_price"]
            .as_f64()
            .expect("the index fixture carries a price")
    }

    async fn mounted() -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/public/get_instruments"))
            .and(query_param("currency", "BTC"))
            .respond_with(ResponseTemplate::new(200).set_body_json(fixture(INSTRUMENTS)))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path_matcher("/public/get_book_summary_by_currency"))
            .and(query_param("currency", "BTC"))
            .respond_with(ResponseTemplate::new(200).set_body_json(fixture(SUMMARY)))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path_matcher("/public/get_index_price"))
            .respond_with(ResponseTemplate::new(200).set_body_json(fixture(INDEX)))
            .mount(&server)
            .await;
        server
    }

    /* ------------------------------------------------------- instrument names */

    #[test]
    fn reads_both_product_families_from_one_rule() {
        let inverse = parse_instrument("BTC-25DEC26-104000-C").expect("inverse parses");
        assert_eq!(inverse.base, "BTC");
        assert_eq!(inverse.currency, "BTC");
        assert_eq!(inverse.strike, 104_000.0);
        assert_eq!(inverse.option_type, OptionType::Call);

        let linear = parse_instrument("SOL_USDC-17AUG26-66-P").expect("linear parses");
        assert_eq!(linear.base, "SOL");
        // The quote currency is what separates the families.
        assert_eq!(linear.currency, "USDC");
        assert_eq!(linear.option_type, OptionType::Put);
    }

    #[test]
    fn refuses_anything_that_is_not_an_instrument_name() {
        for bad in [
            "BTC-25DEC26-104000",
            "BTC-25DEC26-104000-X",
            "BTC-25DEC26-0-C",
            "BTC-PERPETUAL",
            "",
        ] {
            assert!(parse_instrument(bad).is_none(), "accepted {bad:?}");
        }
    }

    #[test]
    fn resolves_the_spellings_a_reader_might_type() {
        for spelling in ["btc", "BTC-USD", "bitcoin", "XBT", "BTC_USDC"] {
            assert_eq!(resolve_symbol(spelling), Some("BTC"), "{spelling}");
        }
        assert_eq!(resolve_symbol("solana"), Some("SOL"));
        assert_eq!(resolve_symbol("DOGE"), None);
    }

    #[test]
    fn routes_a_contract_back_to_its_underlying() {
        assert_eq!(symbol_of_contract("BTC-25DEC26-104000-C"), Some("BTC"));
        assert_eq!(symbol_of_contract("SOL_USDC-17AUG26-66-C"), Some("SOL"));
        assert_eq!(symbol_of_contract("AAPL260918C00300000"), None);
    }

    /* ------------------------------------------------------------- conversion */

    #[test]
    fn converts_an_inverse_mark_from_the_coin_to_dollars() {
        let instruments = raw_instruments();
        let summaries = raw_summaries();
        let index = index_price();
        let board = build_board("BTC", &instruments, &summaries, Some(index));

        let quote = board
            .quotes
            .iter()
            .find(|q| q.mark.is_some())
            .expect("some contract has a mark");
        let raw = summaries
            .iter()
            .find(|s| s.instrument_name == quote.contract)
            .expect("the summary is the one it came from");

        // The whole conversion: a mark of 0.0123 BTC on a $77,000 index is $947,
        // not $0.0123. Getting this backwards is off by a factor of the index.
        let expected = raw.mark_price.unwrap() * index;
        assert!(
            (quote.mark.unwrap() - expected).abs() < 1e-6,
            "{} marked {:?}, expected {expected}",
            quote.contract,
            quote.mark
        );
        assert!(quote.mark.unwrap() > 1.0, "a dollar price, not a coin one");
    }

    #[test]
    fn takes_the_discount_factor_from_the_basis_and_not_from_the_interest_rate_field() {
        // The defect this whole module is arranged around. Deribit publishes
        // `interest_rate: 0.0` on every instrument in this fixture while its own
        // long-dated forward sits nearly 4% above the index.
        let board = build_board(
            "BTC",
            &raw_instruments(),
            &raw_summaries(),
            Some(index_price()),
        );

        let long_dated = board
            .quotes
            .iter()
            .max_by_key(|q| q.expiry)
            .expect("the board has quotes");
        let forward = long_dated.venue_forward.expect("a venue forward");
        let discount = long_dated.venue_discount.expect("a venue discount");

        assert!(
            forward > index_price() * 1.02,
            "the long-dated forward should carry a real basis, was {forward}"
        );
        assert!(
            discount < 0.99,
            "a real basis means a discount factor under one, was {discount}"
        );
        // `DF = S/F` exactly — which is what makes our delta match Deribit's.
        assert!((discount - index_price() / forward).abs() < 1e-12);
    }

    #[test]
    fn reads_a_venue_volatility_out_of_percentage_points() {
        let board = build_board(
            "BTC",
            &raw_instruments(),
            &raw_summaries(),
            Some(index_price()),
        );
        let quote = board
            .quotes
            .iter()
            .find(|q| q.venue_iv.is_some())
            .expect("the fixture carries mark_iv");
        let iv = quote.venue_iv.unwrap();
        // Decimal, not percent: a 45% vol is 0.45. Off by 100 and every Greek is
        // nonsense in a way that still renders.
        assert!(
            (0.01..=5.0).contains(&iv),
            "{} implied {iv}, which is not a decimal volatility",
            quote.contract
        );
    }

    #[test]
    fn refuses_a_basis_that_is_a_bad_tick_rather_than_a_carry() {
        assert!(discount_from(Some(100.0), Some(96.0)).is_some());
        // A forward at half or double spot is not a carry any market quotes.
        assert_eq!(discount_from(Some(300.0), Some(100.0)), None);
        assert_eq!(discount_from(Some(50.0), Some(100.0)), None);
        assert_eq!(discount_from(None, Some(100.0)), None);
        assert_eq!(discount_from(Some(100.0), None), None);
    }

    #[test]
    fn keeps_an_untraded_strike_rather_than_dropping_it() {
        let board = build_board(
            "BTC",
            &raw_instruments(),
            &raw_summaries(),
            Some(index_price()),
        );
        // An open strike with no book is a real feature of a board; a chain with
        // holes punched in it reads as missing data.
        assert!(
            board.quotes.iter().any(|q| q.bid.is_none()),
            "the fixture should contain a one-sided or empty book"
        );
        assert_eq!(board.quotes.len(), raw_summaries().len());
    }

    #[test]
    fn says_out_loud_that_an_inverse_board_was_converted() {
        let board = build_board(
            "BTC",
            &raw_instruments(),
            &raw_summaries(),
            Some(index_price()),
        );
        assert!(board.note.contains("coin-margined"), "note: {}", board.note);
        assert!(board.note.contains("converted to USD"));
    }

    #[test]
    fn cannot_price_an_inverse_board_with_no_index_so_it_quotes_none() {
        // Without an index there is no conversion, and a coin-denominated price
        // shown as dollars would be wrong by five orders of magnitude.
        let board = build_board("BTC", &raw_instruments(), &raw_summaries(), None);
        assert!(board.quotes.is_empty());
        assert_eq!(board.spot, None);
    }

    /* ------------------------------------------------------------------ wire */

    #[tokio::test]
    async fn builds_a_board_from_two_requests_rather_than_one_per_contract() {
        let server = mounted().await;
        let state = state_for(&server);

        let board = get_board(&state, "btc").await.expect("the board loads");
        assert_eq!(board.symbol, "BTC");
        assert_eq!(board.venue, "Deribit");
        assert_eq!(board.contract_size, 1.0);
        assert!(board.quotes.len() > 100, "{} quotes", board.quotes.len());

        // Two expiries in the fixture, and both survive the round trip.
        let expiries: std::collections::HashSet<i64> =
            board.quotes.iter().map(|q| q.expiry).collect();
        assert_eq!(expiries.len(), 2);
    }

    #[tokio::test]
    async fn refuses_an_underlying_deribit_does_not_list() {
        let server = mounted().await;
        let state = state_for(&server);

        let error = get_board(&state, "DOGE")
            .await
            .expect_err("DOGE is not listed");
        assert_eq!(error.code, "not_found");
        // The refusal names what does exist, so the next command is obvious.
        assert!(error.hint.unwrap_or_default().contains("BTC"));
    }

    #[tokio::test]
    async fn reports_a_json_rpc_error_rather_than_reading_it_as_an_empty_board() {
        // Deribit answers a bad request with HTTP 200 and an `error` object.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "jsonrpc": "2.0",
                "error": { "code": 10009, "message": "not_enough_funds" }
            })))
            .mount(&server)
            .await;

        let error = get_board(&state_for(&server), "BTC")
            .await
            .expect_err("a 200 with an error body is still a failure");
        assert_eq!(error.code, "upstream_error");
        assert!(error.message.contains("not_enough_funds"));
    }

    /* --------------------------------------------------------------- history */

    #[test]
    fn reverses_nothing_and_drops_a_bar_with_no_close() {
        let raw = RawChart {
            ticks: vec![3_000_000.0, 1_000_000.0, 2_000_000.0],
            open: vec![1.0, 2.0, 3.0],
            high: vec![1.5, 2.5, 3.5],
            low: vec![0.5, 1.5, 2.5],
            close: vec![1.2, f64::NAN, 3.2],
            volume: vec![10.0, 20.0, 30.0],
        };
        let candles = normalise_chart(&raw);
        // The NaN close is dropped, and what remains is in time order.
        assert_eq!(candles.len(), 2);
        assert_eq!(candles[0].time, 2_000);
        assert_eq!(candles[1].time, 3_000);
    }
}
