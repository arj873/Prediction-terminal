//! Polygon.io — a licensed consolidated tape, when a deployment has a key.
//!
//! This is not a twelfth thing to browse; it is a better answer to a question
//! the terminal already asks. `STK`, `CRY` and the true-price half of `IMP` go
//! through a provider chain, and Polygon slots in *ahead* of Yahoo and Nasdaq
//! whenever `POLYGON_API_KEY` is set, because it fixes the two failures those
//! two actually have:
//!
//!  - Yahoo returns 429 to shared datacentre addresses, which is exactly where a
//!    hosted deployment of this terminal runs. Re-confirmed from this container:
//!    its `v7` option host refuses outright, and its chart host is erratic.
//!  - Nasdaq answers from those addresses but has no S&P 500 or Dow, and
//!    Kalshi's index ladders settle on the index rather than on an ETF tracking
//!    it.
//!
//! Polygon has neither problem: it is authenticated rather than IP-reputation
//! based, and `I:SPX` is the index itself. Without a key the chain is exactly
//! what it was, so this module changes nothing for a deployment that has none —
//! which includes this one, so the live path here is exercised only against
//! `wiremock`. The shape below is Polygon's documented one.
//!
//! A caveat the terminal states rather than hides: Polygon's free tier serves
//! end-of-day data with a 15-minute delay and no intraday aggregates. A `1m`
//! chart on a free key therefore returns nothing, and that reads as an empty
//! chart unless it is named — so it is.

use std::sync::Arc;
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde::Deserialize;
use terminal_core::types::{AssetClass, CandleInterval, SpotCandle, SpotQuote, SpotSearchResult};

use crate::app::AppState;
use crate::cache::ttl;
use crate::error::UpstreamError;
use crate::http::FetchOptions;
use crate::routes::helpers::now_seconds;

type Result<T> = std::result::Result<T, UpstreamError>;

const TIMEOUT: Duration = Duration::from_secs(25);
const RETRIES: u32 = 1;

/// Whether this deployment can use Polygon at all.
#[must_use]
pub fn has_credentials(state: &AppState) -> bool {
    state.config().polygon_api_key.is_some()
}

/* --------------------------------------------------------------- symbols */

/// Cash indices, as this terminal names them and as Polygon does.
///
/// The rest of the terminal speaks Yahoo's caret convention because Yahoo is the
/// primary equity provider. Polygon uses an `I:` prefix and its own root, so the
/// handful that differ are stated here rather than guessed — a wrong guess would
/// quote an unrelated instrument rather than fail.
const INDEX_SYMBOLS: &[(&str, &str)] = &[
    ("^GSPC", "I:SPX"),
    ("SPX", "I:SPX"),
    ("^NDX", "I:NDX"),
    ("NDX", "I:NDX"),
    ("^IXIC", "I:COMP"),
    ("COMP", "I:COMP"),
    ("^DJI", "I:DJI"),
    ("DJI", "I:DJI"),
    ("^RUT", "I:RUT"),
    ("RUT", "I:RUT"),
    ("^VIX", "I:VIX"),
    ("VIX", "I:VIX"),
];

/// Crypto pairs are `X:BTCUSD`; the terminal writes them `BTC-USD`.
#[must_use]
pub fn polygon_symbol(symbol: &str, asset_class: AssetClass) -> String {
    let upper = symbol.trim().to_uppercase();

    if asset_class == AssetClass::Crypto {
        if upper.starts_with("X:") {
            return upper;
        }
        let mut parts = upper.split(['-', '/']);
        let base = parts.next().unwrap_or(&upper);
        let quote = parts.next().unwrap_or("USD");
        return format!("X:{base}{quote}");
    }

    if let Some((_, mapped)) = INDEX_SYMBOLS.iter().find(|(name, _)| *name == upper) {
        return (*mapped).to_owned();
    }
    // An unmapped caret symbol is still an index; Polygon spells it without one.
    upper
        .strip_prefix('^')
        .map_or(upper.clone(), |rest| format!("I:{rest}"))
}

/// Kalshi's candle periods as Polygon's multiplier/timespan pair.
fn timespan(interval: CandleInterval) -> (u32, &'static str) {
    match interval {
        CandleInterval::OneMinute => (1, "minute"),
        CandleInterval::OneHour => (1, "hour"),
        CandleInterval::OneDay => (1, "day"),
    }
}

/* ----------------------------------------------------------------- fetch */

#[derive(Debug, Default, Deserialize)]
pub struct AggregatesResponse {
    #[serde(default)]
    pub results: Vec<Aggregate>,
}

#[derive(Debug, Default, Clone, Copy, Deserialize)]
pub struct Aggregate {
    /// Bar start, unix milliseconds.
    #[serde(default)]
    pub t: Option<f64>,
    #[serde(default)]
    pub o: Option<f64>,
    #[serde(default)]
    pub h: Option<f64>,
    #[serde(default)]
    pub l: Option<f64>,
    #[serde(default)]
    pub c: Option<f64>,
    #[serde(default)]
    pub v: Option<f64>,
}

#[derive(Debug, Default, Deserialize)]
struct TickerDetails {
    #[serde(default)]
    results: Option<TickerResult>,
}

#[derive(Debug, Default, Clone, Deserialize)]
struct TickerResult {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    market: Option<String>,
    #[serde(default)]
    primary_exchange: Option<String>,
    #[serde(default)]
    currency_name: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct TickerSearchResponse {
    #[serde(default)]
    results: Vec<TickerResult2>,
}

#[derive(Debug, Default, Clone, Deserialize)]
struct TickerResult2 {
    #[serde(default)]
    ticker: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    market: Option<String>,
    #[serde(default)]
    primary_exchange: Option<String>,
}

fn require_key(state: &AppState) -> Result<&str> {
    state.config().polygon_api_key.as_deref().ok_or_else(|| {
        UpstreamError::not_configured("Polygon.io needs an API key, and none is set")
            .with_hint("Set POLYGON_API_KEY. Keys are free from https://polygon.io/dashboard/api-keys.")
    })
}

async fn get<T>(
    state: &AppState,
    path: &str,
    params: &[(&str, String)],
    ttl: Duration,
) -> Result<Arc<T>>
where
    T: DeserializeOwned + Send + Sync + 'static,
{
    let key = require_key(state)?.to_owned();
    let base = state.config().polygon_api_base.trim_end_matches('/');

    let query = params
        .iter()
        .map(|(k, v)| format!("{k}={}", urlencoding::encode(v)))
        .collect::<Vec<_>>()
        .join("&");
    let url = if query.is_empty() {
        format!("{base}{path}")
    } else {
        format!("{base}{path}?{query}")
    };

    // The key travels as a header, so it never reaches the cache key or a log.
    let cache_key = format!("polygon:{path}?{query}");

    state
        .cache()
        .cached(&cache_key, ttl, || async {
            state
                .http()
                .fetch_json::<T>(
                    &url,
                    FetchOptions::new()
                        .timeout(TIMEOUT)
                        .retries(RETRIES)
                        .header("Authorization", &format!("Bearer {key}")),
                )
                .await
                .map_err(annotate)
        })
        .await
}

/// Say which of Polygon's two refusals this is.
///
/// A free key answers 403 for anything intraday or real-time and 429 above five
/// requests a minute. Both are plan limits rather than outages, and both are
/// indistinguishable from a broken symbol unless named.
fn annotate(error: UpstreamError) -> UpstreamError {
    match error.status {
        Some(403) => UpstreamError::not_configured(
            "Polygon.io declined this request on the current plan",
        )
        .with_status(403)
        .with_hint(
            "A free Polygon key covers end-of-day aggregates only. Intraday bars (`1m`, `1h`) \
             and real-time quotes need a paid plan; daily bars (`1d`) work on every plan.",
        ),
        Some(429) => UpstreamError::new("Polygon.io is rate-limiting this key", "rate_limited")
            .with_status(429)
            .with_hint(
                "The free plan allows five requests a minute. Wait a moment, or upgrade the key.",
            ),
        _ => error,
    }
}

/* --------------------------------------------------------------- candles */

fn iso(seconds: i64) -> String {
    crate::sources::optionboard::iso_date_of(seconds)
}

/// Polygon's aggregate bars → this terminal's spot candles.
///
/// Exported for tests: Polygon timestamps a bar at its *start*, in
/// milliseconds, and this terminal's spot candles are unix seconds at the bar
/// start — so this is a divide and not an offset, which is exactly the kind of
/// thing that looks right and is a thousand times wrong.
#[must_use]
pub fn normalise_aggregates(raw: &AggregatesResponse) -> Vec<SpotCandle> {
    let mut candles = Vec::with_capacity(raw.results.len());
    for bar in &raw.results {
        let (Some(start), Some(close)) = (bar.t, bar.c.filter(|c| c.is_finite())) else {
            continue;
        };
        let open = bar.o.unwrap_or(close);
        #[allow(clippy::cast_possible_truncation)]
        let time = (start / 1000.0).floor() as i64;
        candles.push(SpotCandle {
            time,
            open,
            high: bar.h.unwrap_or_else(|| open.max(close)),
            low: bar.l.unwrap_or_else(|| open.min(close)),
            close,
            volume: bar.v.unwrap_or(0.0),
        });
    }
    candles
}

pub struct PolygonCandles {
    pub candles: Vec<SpotCandle>,
    pub name: String,
    pub currency: String,
}

pub async fn get_candles(
    state: &AppState,
    symbol: &str,
    interval: CandleInterval,
    start_ts: i64,
    end_ts: i64,
    asset_class: AssetClass,
) -> Result<PolygonCandles> {
    let ticker = polygon_symbol(symbol, asset_class);
    let (multiplier, span) = timespan(interval);

    let path = format!(
        "/v2/aggs/ticker/{}/range/{multiplier}/{span}/{}/{}",
        urlencoding::encode(&ticker),
        iso(start_ts),
        iso(end_ts)
    );
    let params = [
        ("adjusted", "true".to_owned()),
        ("sort", "asc".to_owned()),
        ("limit", "50000".to_owned()),
    ];
    let bars = get::<AggregatesResponse>(
        state,
        &path,
        &params,
        if interval == CandleInterval::OneDay {
            ttl::CANDLES
        } else {
            ttl::QUOTE
        },
    )
    .await?;

    let candles = normalise_aggregates(&bars);

    if candles.is_empty() {
        let hint = if interval == CandleInterval::OneDay {
            "Check the symbol — indices need their Polygon form, e.g. I:SPX."
        } else {
            "A free Polygon key serves no intraday history. Retry with `1d`."
        };
        return Err(
            UpstreamError::new(
                format!("Polygon.io returned no {span} bars for {ticker}"),
                "empty_upstream",
            )
            .with_hint(hint),
        );
    }

    let details = describe(state, &ticker).await.ok();
    Ok(PolygonCandles {
        candles,
        name: details
            .as_ref()
            .map_or_else(|| symbol.to_uppercase(), |d| d.name.clone()),
        currency: details.map_or_else(|| "USD".to_owned(), |d| d.currency),
    })
}

struct Described {
    name: String,
    currency: String,
    venue: String,
}

async fn describe(state: &AppState, ticker: &str) -> Result<Described> {
    let payload = get::<TickerDetails>(
        state,
        &format!("/v3/reference/tickers/{}", urlencoding::encode(ticker)),
        &[],
        ttl::CATALOGUE,
    )
    .await?;
    let result = payload.results.clone().unwrap_or_default();
    Ok(Described {
        name: result.name.unwrap_or_else(|| ticker.to_owned()),
        currency: result
            .currency_name
            .unwrap_or_else(|| "usd".to_owned())
            .to_uppercase(),
        venue: result
            .primary_exchange
            .or(result.market)
            .unwrap_or_else(|| "Polygon".to_owned()),
    })
}

/* ----------------------------------------------------------------- quote */

/// A quote, built from the last two daily bars.
///
/// Polygon's snapshot endpoint is the natural source and is not on the free
/// plan, so this derives the same figures from aggregates, which every plan
/// serves. The previous close is the bar before the last one — the same rule the
/// Yahoo provider settled on, and for the same reason: it is right whether or
/// not the session is still open.
pub async fn get_quote(
    state: &AppState,
    symbol: &str,
    asset_class: AssetClass,
) -> Result<SpotQuote> {
    let now = now_seconds();
    let loaded = get_candles(
        state,
        symbol,
        CandleInterval::OneDay,
        now - 14 * 86_400,
        now,
        asset_class,
    )
    .await?;
    let details = describe(state, &polygon_symbol(symbol, asset_class))
        .await
        .ok();

    let last = loaded.candles.last();
    let previous = loaded.candles.iter().rev().nth(1);
    let price = last.map(|c| c.close);
    let previous_close = previous.map(|c| c.close);
    let change = match (price, previous_close) {
        (Some(price), Some(previous)) => Some(price - previous),
        _ => None,
    };

    Ok(SpotQuote {
        symbol: symbol.to_uppercase(),
        asset_class,
        name: details
            .as_ref()
            .map_or_else(|| symbol.to_uppercase(), |d| d.name.clone()),
        currency: details
            .as_ref()
            .map_or_else(|| "USD".to_owned(), |d| d.currency.clone()),
        price,
        previous_close,
        change,
        change_percent: match (change, previous_close) {
            (Some(change), Some(previous)) if previous != 0.0 => Some(change / previous * 100.0),
            _ => None,
        },
        day_open: last.map(|c| c.open),
        day_high: last.map(|c| c.high),
        day_low: last.map(|c| c.low),
        volume: last.map(|c| c.volume),
        venue: details.map_or_else(|| "Polygon".to_owned(), |d| d.venue),
        time: last.map_or(now, |c| c.time),
        source: "polygon".to_owned(),
    })
}

/* ---------------------------------------------------------------- search */

pub async fn search(state: &AppState, query: &str, limit: usize) -> Result<Vec<SpotSearchResult>> {
    let payload = get::<TickerSearchResponse>(
        state,
        "/v3/reference/tickers",
        &[
            ("search", query.to_owned()),
            ("active", "true".to_owned()),
            ("limit", limit.clamp(1, 100).to_string()),
        ],
        ttl::META,
    )
    .await?;

    Ok(payload
        .results
        .iter()
        .filter_map(|r| {
            let ticker = r.ticker.clone()?;
            Some(SpotSearchResult {
                name: r.name.clone().unwrap_or_else(|| ticker.clone()),
                asset_class: if r.market.as_deref() == Some("crypto") {
                    AssetClass::Crypto
                } else {
                    AssetClass::Stock
                },
                venue: r
                    .primary_exchange
                    .clone()
                    .or_else(|| r.market.clone())
                    .unwrap_or_else(|| "Polygon".to_owned()),
                symbol: ticker,
                has_implied: false,
            })
        })
        .collect())
}
