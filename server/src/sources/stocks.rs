//! Equity, ETF and cash-index prices.
//!
//! Two providers, tried in order, for the same reason FRED has two:
//!
//! **Yahoo Finance** (`query1.finance.yahoo.com/v8/finance/chart`) is the
//! primary. It is the only free, keyless source here that covers cash indices —
//! `^GSPC`, `^NDX`, `^DJI` — and Kalshi's index ladders settle on the index, not
//! on an ETF tracking it. Quoting SPY against a `KXINX` ladder would be off by a
//! factor of ten, so this matters more than convenience.
//!
//! **Nasdaq** (`api.nasdaq.com`) is the fallback. It answers from datacentre
//! IPs, where Yahoo's API hosts return 429 to a shared address — the same
//! hosted-deployment failure the FRED scrape hits. It covers equities, ETFs and
//! Nasdaq's own indices (`COMP`, `NDX`) but *not* the S&P 500 or the Dow, so it
//! narrows coverage rather than replacing it. The terminal says which provider
//! answered rather than leaving that invisible.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde::Deserialize;
use terminal_core::types::{
    AssetClass, CandleInterval, SpotCandle, SpotCandlesResponse, SpotQuote, SpotSearchResult,
};
use time::{Date, Month, OffsetDateTime};

use crate::app::AppState;
use crate::cache::ttl;
use crate::error::{codes, Result, UpstreamError};
use crate::http::{FetchOptions, DESKTOP_UA};
use crate::providers::{Chain, Provider};
use crate::sources::polygon;

/* --------------------------------------------------------------- intervals */

/// Kalshi's candle periods expressed the way Yahoo spells them.
fn yahoo_interval(interval: CandleInterval) -> &'static str {
    match interval {
        CandleInterval::OneMinute => "1m",
        CandleInterval::OneHour => "1h",
        CandleInterval::OneDay => "1d",
    }
}

/// How far back each interval can actually be requested.
///
/// Yahoo enforces these server-side and answers a too-wide window with an error
/// rather than a truncated series, so the clamp has to happen before the call.
fn max_lookback(interval: CandleInterval) -> i64 {
    match interval {
        CandleInterval::OneMinute => 7 * 86_400,
        CandleInterval::OneHour => 729 * 86_400,
        CandleInterval::OneDay => 40 * 365 * 86_400,
    }
}

/* ------------------------------------------------------------------ yahoo */

/// Yahoo's chart payload.
///
/// Every level is optional and every parallel array is a `Vec<Option<..>>`:
/// Yahoo punches nulls through the series for halted or not-yet-printed
/// buckets, and omits whole branches for an unknown symbol.
#[derive(Debug, Default, Clone, Deserialize)]
pub struct YahooChartResponse {
    #[serde(default)]
    pub chart: Option<YahooChart>,
}

#[derive(Debug, Default, Clone, Deserialize)]
pub struct YahooChart {
    #[serde(default)]
    pub result: Vec<YahooResult>,
    #[serde(default)]
    pub error: Option<YahooError>,
}

#[derive(Debug, Default, Clone, Deserialize)]
pub struct YahooError {
    #[serde(default)]
    pub code: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Default, Clone, Deserialize)]
pub struct YahooResult {
    #[serde(default)]
    pub meta: Option<YahooMeta>,
    #[serde(default)]
    pub timestamp: Vec<Option<i64>>,
    #[serde(default)]
    pub indicators: Option<YahooIndicators>,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct YahooMeta {
    #[serde(default)]
    pub currency: Option<String>,
    #[serde(default)]
    pub symbol: Option<String>,
    #[serde(default)]
    pub exchange_name: Option<String>,
    #[serde(default)]
    pub full_exchange_name: Option<String>,
    #[serde(default)]
    pub instrument_type: Option<String>,
    #[serde(default)]
    pub regular_market_price: Option<f64>,
    #[serde(default)]
    pub previous_close: Option<f64>,
    #[serde(default)]
    pub chart_previous_close: Option<f64>,
    #[serde(default)]
    pub regular_market_day_high: Option<f64>,
    #[serde(default)]
    pub regular_market_day_low: Option<f64>,
    #[serde(default)]
    pub regular_market_volume: Option<f64>,
    #[serde(default)]
    pub regular_market_time: Option<i64>,
    #[serde(default)]
    pub long_name: Option<String>,
    #[serde(default)]
    pub short_name: Option<String>,
}

#[derive(Debug, Default, Clone, Deserialize)]
pub struct YahooIndicators {
    #[serde(default)]
    pub quote: Vec<YahooQuoteRows>,
}

#[derive(Debug, Default, Clone, Deserialize)]
pub struct YahooQuoteRows {
    #[serde(default)]
    pub open: Vec<Option<f64>>,
    #[serde(default)]
    pub high: Vec<Option<f64>>,
    #[serde(default)]
    pub low: Vec<Option<f64>>,
    #[serde(default)]
    pub close: Vec<Option<f64>>,
    #[serde(default)]
    pub volume: Vec<Option<f64>>,
}

/// The two halves of one chart response.
#[derive(Debug, Clone)]
pub struct ChartReading {
    pub quote: SpotQuote,
    pub candles: Vec<SpotCandle>,
}

async fn yahoo<T>(state: &AppState, path: &str, ttl: Duration) -> Result<Arc<T>>
where
    T: DeserializeOwned + Send + Sync + 'static,
{
    let key = format!("yahoo:{path}");
    state
        .cache()
        .cached(&key, ttl, || async {
            let url = format!("{}{path}", state.config().yahoo_api_base);
            state
                .http()
                .fetch_json::<T>(
                    &url,
                    FetchOptions::new()
                        .timeout(Duration::from_secs(15))
                        .retries(1)
                        .header("User-Agent", DESKTOP_UA),
                )
                .await
        })
        .await
}

/// Yahoo's chart payload → candles plus a quote, in one call.
///
/// Public for tests, and because both [`get_quote`] and [`get_candles`] want
/// different halves of the same response — fetching it once and splitting it is
/// cheaper than two endpoints.
pub fn parse_yahoo_chart(
    body: &YahooChartResponse,
    symbol: &str,
    interval: CandleInterval,
) -> Result<ChartReading> {
    let Some(result) = body.chart.as_ref().and_then(|chart| chart.result.first()) else {
        let description = body
            .chart
            .as_ref()
            .and_then(|chart| chart.error.as_ref())
            .and_then(|error| error.description.clone());
        return Err(UpstreamError::not_found(
            description.unwrap_or_else(|| format!("Yahoo Finance returned no data for {symbol}")),
        )
        .with_hint(
            "Check the symbol. Cash indices need their caret form, e.g. `^GSPC` for the S&P 500.",
        ));
    };

    let meta = result.meta.clone().unwrap_or_default();
    let quote_rows = result
        .indicators
        .as_ref()
        .and_then(|indicators| indicators.quote.first())
        .cloned()
        .unwrap_or_default();
    let times = &result.timestamp;

    let mut candles: Vec<SpotCandle> = Vec::new();
    for (i, time) in times.iter().enumerate() {
        // Yahoo pads its arrays with nulls for halted or not-yet-printed buckets.
        // A bar with no close is not a bar.
        let (Some(time), Some(Some(close))) = (time, quote_rows.close.get(i)) else {
            continue;
        };
        if !close.is_finite() {
            continue;
        }
        let close = *close;
        let open = pick(quote_rows.open.get(i), close);
        let high = pick(quote_rows.high.get(i), open.max(close));
        let low = pick(quote_rows.low.get(i), open.min(close));
        candles.push(SpotCandle {
            time: *time,
            open,
            high,
            low,
            close,
            volume: pick(quote_rows.volume.get(i), 0.0),
        });
    }

    let last = candles.last().copied();
    let price = meta.regular_market_price.or(last.map(|bar| bar.close));
    let previous_close = previous_close_of(&meta, &candles, interval);
    let change = match (price, previous_close) {
        (Some(price), Some(previous_close)) => Some(price - previous_close),
        _ => None,
    };

    let quote = SpotQuote {
        symbol: meta.symbol.clone().unwrap_or_else(|| symbol.to_string()),
        asset_class: AssetClass::Stock,
        name: meta
            .long_name
            .clone()
            .or_else(|| meta.short_name.clone())
            .or_else(|| meta.symbol.clone())
            .unwrap_or_else(|| symbol.to_string()),
        currency: meta.currency.clone().unwrap_or_else(|| "USD".to_string()),
        price,
        previous_close,
        change,
        change_percent: match (change, previous_close) {
            (Some(change), Some(previous_close)) if previous_close != 0.0 => {
                Some((change / previous_close) * 100.0)
            }
            _ => None,
        },
        // The final bar is the current session, so its open is today's open. The
        // *first* bar's open is the start of the requested window, which is only
        // the same thing when the window happens to be one day long.
        day_open: last.map(|bar| bar.open),
        day_high: meta.regular_market_day_high,
        day_low: meta.regular_market_day_low,
        volume: meta.regular_market_volume,
        venue: meta
            .full_exchange_name
            .clone()
            .or_else(|| meta.exchange_name.clone())
            .unwrap_or_default(),
        time: meta
            .regular_market_time
            .or(last.map(|bar| bar.time))
            .unwrap_or_else(now_seconds),
        source: "yahoo".to_string(),
    };

    candles.sort_by_key(|a| a.time);
    Ok(ChartReading { quote, candles })
}

fn pick(value: Option<&Option<f64>>, fallback: f64) -> f64 {
    match value {
        Some(Some(value)) if value.is_finite() => *value,
        _ => fallback,
    }
}

/// The prior session's close, for the day-change figure.
///
/// Yahoo's chart meta usually omits `previousClose` entirely and offers
/// `chartPreviousClose`, which is the close before the *requested window* — over
/// a week-long range that is last week's number. Trusting it printed AAPL as
/// -2.4% on a day it closed +0.2%.
///
/// On a daily series the answer is sitting in the data: the bar before the last
/// one. That holds whether the market is open (last bar is today, forming) or
/// shut (last bar is the most recent session), which is exactly the ambiguity a
/// meta field would have to resolve anyway.
fn previous_close_of(
    meta: &YahooMeta,
    candles: &[SpotCandle],
    interval: CandleInterval,
) -> Option<f64> {
    if let Some(previous_close) = meta.previous_close {
        if previous_close.is_finite() {
            return Some(previous_close);
        }
    }
    if interval == CandleInterval::OneDay && candles.len() >= 2 {
        return Some(candles[candles.len() - 2].close);
    }
    meta.chart_previous_close
}

async fn yahoo_chart(
    state: &AppState,
    symbol: &str,
    interval: CandleInterval,
    start_ts: i64,
    end_ts: i64,
) -> Result<ChartReading> {
    let from = start_ts.max(end_ts - max_lookback(interval));
    let path = format!(
        "/v8/finance/chart/{}?period1={from}&period2={end_ts}&interval={}&includePrePost=false&events=div%2Csplit",
        urlencoding::encode(symbol),
        yahoo_interval(interval),
    );

    let body = yahoo::<YahooChartResponse>(state, &path, ttl::CANDLES).await?;
    parse_yahoo_chart(&body, symbol, interval)
}

/* ----------------------------------------------------------------- nasdaq */

#[derive(Debug, Default, Clone, Deserialize)]
pub struct NasdaqEnvelope<T> {
    // Spelled as a function rather than `#[serde(default)]`: the plain form
    // makes serde demand `T: Default` of every payload put through the envelope.
    #[serde(default = "no_data")]
    pub data: Option<T>,
    #[serde(default)]
    pub status: Option<NasdaqStatus>,
}

fn no_data<T>() -> Option<T> {
    None
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NasdaqStatus {
    #[serde(default)]
    pub r_code: Option<i64>,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NasdaqInfo {
    #[serde(default)]
    pub symbol: Option<String>,
    #[serde(default)]
    pub company_name: Option<String>,
    #[serde(default)]
    pub exchange: Option<String>,
    #[serde(default)]
    pub primary_data: Option<NasdaqPrimaryData>,
    #[serde(default)]
    pub key_stats: Option<NasdaqKeyStats>,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NasdaqPrimaryData {
    #[serde(default)]
    pub last_sale_price: Option<String>,
    #[serde(default)]
    pub net_change: Option<String>,
    #[serde(default)]
    pub percentage_change: Option<String>,
    #[serde(default)]
    pub volume: Option<String>,
    #[serde(default)]
    pub last_trade_timestamp: Option<String>,
}

#[derive(Debug, Default, Clone, Deserialize)]
pub struct NasdaqKeyStats {
    #[serde(default, rename = "PreviousClose")]
    pub previous_close: Option<NasdaqStat>,
    #[serde(default, rename = "OpenPrice")]
    pub open_price: Option<NasdaqStat>,
}

#[derive(Debug, Default, Clone, Deserialize)]
pub struct NasdaqStat {
    #[serde(default)]
    pub value: Option<String>,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NasdaqHistorical {
    #[serde(default)]
    pub trades_table: Option<NasdaqTradesTable>,
}

#[derive(Debug, Default, Clone, Deserialize)]
pub struct NasdaqTradesTable {
    #[serde(default)]
    pub rows: Vec<NasdaqRow>,
}

#[derive(Debug, Default, Clone, Deserialize)]
pub struct NasdaqRow {
    #[serde(default)]
    pub date: Option<String>,
    #[serde(default)]
    pub close: Option<String>,
    #[serde(default)]
    pub volume: Option<String>,
    #[serde(default)]
    pub open: Option<String>,
    #[serde(default)]
    pub high: Option<String>,
    #[serde(default)]
    pub low: Option<String>,
}

/// Nasdaq requires the right `assetclass`, and answers 400 for a wrong guess.
const ASSET_CLASSES: [&str; 3] = ["stocks", "etf", "index"];

/// The TTL is part of the key.
///
/// `TtlCache::cached` keys on the string alone, so whoever writes an entry fixes
/// its lifetime for everyone reading it. Two callers ask for the same Nasdaq URL
/// with different freshness needs — [`resolve_asset_class`] at `ttl::META` (60s)
/// and [`get_quote`] at `ttl::QUOTE` (3s) — and because the quote path calls the
/// resolver first, the 60s entry was always written first and the quote then
/// served up to twenty times staler than it asked for.
async fn nasdaq<T>(state: &AppState, path: &str, ttl: Duration) -> Result<Arc<NasdaqEnvelope<T>>>
where
    T: DeserializeOwned + Send + Sync + 'static,
{
    let key = format!("nasdaq:{}:{path}", ttl.as_millis());
    state
        .cache()
        .cached(&key, ttl, || async {
            let url = format!("{}{path}", state.config().nasdaq_api_base);
            state
                .http()
                .fetch_json::<NasdaqEnvelope<T>>(
                    &url,
                    FetchOptions::new()
                        .timeout(Duration::from_secs(15))
                        .retries(1)
                        .header("User-Agent", DESKTOP_UA),
                )
                .await
        })
        .await
}

/// Nasdaq has no concept of a caret-prefixed index symbol.
fn nasdaq_symbol(symbol: &str) -> String {
    symbol.strip_prefix('^').unwrap_or(symbol).to_uppercase()
}

/// Find which `assetclass` Nasdaq files this symbol under.
///
/// There is no lookup endpoint for it, so this probes the three in turn. The
/// answer is cached for the session because a symbol does not change class.
async fn resolve_asset_class(state: &AppState, symbol: &str) -> Result<String> {
    let key = format!("nasdaq:class:{symbol}");
    if let Some(known) = state.cache().get::<String>(&key).await {
        return Ok((*known).clone());
    }

    for asset_class in ASSET_CLASSES {
        let path = format!(
            "/api/quote/{}/info?assetclass={asset_class}",
            urlencoding::encode(symbol)
        );
        let body = nasdaq::<NasdaqInfo>(state, &path, ttl::META).await.ok();
        let answered = body.is_some_and(|body| {
            body.status.as_ref().and_then(|status| status.r_code) == Some(200)
                && body.data.is_some()
        });
        if answered {
            state
                .cache()
                .set(&key, asset_class.to_string(), ttl::CATALOGUE)
                .await;
            return Ok(asset_class.to_string());
        }
    }

    Err(
        UpstreamError::not_found(format!("Nasdaq does not list {symbol}")).with_hint(
            "Nasdaq covers US equities, ETFs and its own indices (COMP, NDX). It has no \
             S&P 500 or Dow index feed — those need Yahoo, which is the primary source.",
        ),
    )
}

/// `"$305.93"` → `305.93`; `"28,229,611"` → `28229611`; `"N/A"` → `None`.
pub fn parse_nasdaq_number(raw: Option<&str>) -> Option<f64> {
    let raw = raw?;
    if raw.is_empty() {
        return None;
    }
    let cleaned: String = raw
        .chars()
        .filter(|c| !matches!(c, '$' | ',' | '%') && !c.is_whitespace())
        .collect();
    if cleaned.is_empty() || cleaned == "N/A" || cleaned == "--" {
        return None;
    }
    cleaned
        .parse::<f64>()
        .ok()
        .filter(|value| value.is_finite())
}

/// Nasdaq dates the rows `MM/DD/YYYY`; charts want unix seconds at UTC midnight.
pub fn parse_nasdaq_date(raw: Option<&str>) -> Option<i64> {
    let raw = raw?.trim();
    let bytes = raw.as_bytes();
    if bytes.len() != 10 || bytes[2] != b'/' || bytes[5] != b'/' {
        return None;
    }
    if !raw
        .char_indices()
        .all(|(i, c)| i == 2 || i == 5 || c.is_ascii_digit())
    {
        return None;
    }
    let month: u8 = raw[0..2].parse().ok()?;
    let day: i64 = raw[3..5].parse().ok()?;
    let year: i32 = raw[6..10].parse().ok()?;

    // `Date.UTC(y, m - 1, d)` takes the day as an *offset* from the first of the
    // month, so an out-of-range day rolls forward rather than failing. Building
    // the first and adding the offset keeps that behaviour.
    let first = Date::from_calendar_date(year, Month::try_from(month).ok()?, 1).ok()?;
    Some(first.midnight().assume_utc().unix_timestamp() + (day - 1) * 86_400)
}

pub fn parse_nasdaq_historical(body: &NasdaqEnvelope<NasdaqHistorical>) -> Vec<SpotCandle> {
    let rows = body
        .data
        .as_ref()
        .and_then(|data| data.trades_table.as_ref())
        .map(|table| table.rows.as_slice())
        .unwrap_or_default();

    let mut candles: Vec<SpotCandle> = Vec::new();
    for row in rows {
        let (Some(time), Some(close)) = (
            parse_nasdaq_date(row.date.as_deref()),
            parse_nasdaq_number(row.close.as_deref()),
        ) else {
            continue;
        };
        let open = parse_nasdaq_number(row.open.as_deref()).unwrap_or(close);
        candles.push(SpotCandle {
            time,
            open,
            high: parse_nasdaq_number(row.high.as_deref()).unwrap_or_else(|| open.max(close)),
            low: parse_nasdaq_number(row.low.as_deref()).unwrap_or_else(|| open.min(close)),
            close,
            volume: parse_nasdaq_number(row.volume.as_deref()).unwrap_or(0.0),
        });
    }

    // Nasdaq returns newest first.
    candles.sort_by_key(|a| a.time);
    candles
}

fn nasdaq_quote(body: &NasdaqEnvelope<NasdaqInfo>, symbol: &str) -> SpotQuote {
    let data = body.data.clone().unwrap_or_default();
    let primary = data.primary_data.clone().unwrap_or_default();
    let price = parse_nasdaq_number(primary.last_sale_price.as_deref());
    let previous_close = parse_nasdaq_number(
        data.key_stats
            .as_ref()
            .and_then(|stats| stats.previous_close.as_ref())
            .and_then(|stat| stat.value.as_deref()),
    );
    let change = parse_nasdaq_number(primary.net_change.as_deref());

    SpotQuote {
        symbol: data.symbol.clone().unwrap_or_else(|| symbol.to_string()),
        asset_class: AssetClass::Stock,
        name: data
            .company_name
            .clone()
            .unwrap_or_else(|| symbol.to_string()),
        currency: "USD".to_string(),
        price,
        previous_close: previous_close.or(match (price, change) {
            (Some(price), Some(change)) => Some(price - change),
            _ => None,
        }),
        change,
        change_percent: parse_nasdaq_number(primary.percentage_change.as_deref()),
        day_open: parse_nasdaq_number(
            data.key_stats
                .as_ref()
                .and_then(|stats| stats.open_price.as_ref())
                .and_then(|stat| stat.value.as_deref()),
        ),
        day_high: None,
        day_low: None,
        volume: parse_nasdaq_number(primary.volume.as_deref()),
        venue: data
            .exchange
            .clone()
            .unwrap_or_else(|| "Nasdaq".to_string()),
        time: now_seconds(),
        source: "nasdaq".to_string(),
    }
}

async fn nasdaq_candles(
    state: &AppState,
    symbol: &str,
    interval: CandleInterval,
    start_ts: i64,
    end_ts: i64,
) -> Result<Vec<SpotCandle>> {
    let plain = nasdaq_symbol(symbol);
    let asset_class = resolve_asset_class(state, &plain).await?;

    // Nasdaq only publishes daily bars historically; its intraday endpoint covers
    // the current session alone. Serving daily bars for an intraday request would
    // silently answer a different question, so this reports the gap instead.
    if interval != CandleInterval::OneDay {
        let period = if interval == CandleInterval::OneMinute {
            "1-minute"
        } else {
            "hourly"
        };
        return Err(UpstreamError::unsupported(format!(
            "Nasdaq has no {period} history for {plain}"
        ))
        .with_hint(
            "The Nasdaq fallback only carries daily bars. Retry with `1d`, or run the \
             terminal from a network Yahoo Finance answers, which has intraday history.",
        ));
    }

    let path = format!(
        "/api/quote/{}/historical?assetclass={asset_class}&fromdate={}&todate={}&limit=9999",
        urlencoding::encode(&plain),
        iso_date(start_ts),
        iso_date(end_ts),
    );
    let body = nasdaq::<NasdaqHistorical>(state, &path, ttl::CANDLES).await?;
    Ok(parse_nasdaq_historical(&body))
}

/* ---------------------------------------------------------------- public */

/// The `{candles, name, currency}` a candles request needs from either provider.
struct CandleSet {
    candles: Vec<SpotCandle>,
    name: String,
    currency: String,
}

/// Yahoo first, Nasdaq behind it.
///
/// A 429 from Yahoo is the signature of a shared datacentre IP rather than a bad
/// symbol, and it is worth naming: it is the difference between "your symbol is
/// wrong" and "this host is blocking your network".
fn price_chain<'a, T>(
    state: &AppState,
    symbol: &'a str,
    via_polygon: impl Future<Output = Result<T>> + Send + 'a,
    via_yahoo: impl Future<Output = Result<T>> + Send + 'a,
    via_nasdaq: impl Future<Output = Result<T>> + Send + 'a,
) -> Chain<'a, T> {
    let keyless = !polygon::has_credentials(state);
    Chain::new(format!("a price for {symbol}"))
        .log_prefix("stocks")
        // Polygon goes first *when a key is set*, because it is the only one of
        // the three that is authenticated rather than IP-reputation based, and
        // the only one that carries the cash indices Kalshi's ladders settle on.
        // Without a key it is skipped rather than failed, so a deployment with
        // no key has exactly the chain it had before.
        .provider(
            Provider::new("polygon", "Polygon.io", via_polygon)
                .available(polygon::has_credentials(state)),
        )
        .provider(Provider::new("yahoo", "Yahoo Finance", via_yahoo))
        .provider(Provider::new("nasdaq", "Nasdaq", via_nasdaq))
        .on_exhausted(move |failures| {
            let status = failures
                .iter()
                .find(|failure| failure.id == "yahoo")
                .and_then(|failure| failure.error.status);
            let mut hint = if status == Some(429) {
                format!(
                    "Yahoo Finance is rate-limiting this IP — it does that to shared \
                     datacentre addresses — and the Nasdaq fallback does not cover {symbol}."
                )
            } else {
                format!(
                    "Yahoo Finance and Nasdaq both refused {symbol}. Check the symbol: \
                     cash indices need a caret, e.g. `^GSPC`."
                )
            };
            // A rate limit is a fact about this address's reputation, and the
            // only remedy the two keyless providers offer — ask from somewhere
            // else — is not one the operator of a hosted deployment can take.
            // Polygon is authenticated rather than IP-judged, which is the
            // whole reason it heads this chain, so when it was the arm we
            // skipped that is the remedy worth naming.
            if status == Some(429) {
                if keyless {
                    hint.push_str(
                        " Set POLYGON_API_KEY (free tier at \
                         https://polygon.io/dashboard/signup) to read the tape directly \
                         instead of depending on this address's reputation.",
                    );
                } else {
                    hint.push_str(
                        " The same request usually succeeds from a residential connection.",
                    );
                }
            }
            UpstreamError::new(
                format!("No price source could quote {symbol}"),
                codes::UPSTREAM_ERROR,
            )
            .with_hint(hint)
        })
}

pub async fn get_quote(state: &AppState, symbol: &str) -> Result<SpotQuote> {
    let upper = symbol.trim().to_uppercase();
    let now = now_seconds();

    let via_yahoo = async {
        Ok(
            yahoo_chart(state, &upper, CandleInterval::OneDay, now - 7 * 86_400, now)
                .await?
                .quote,
        )
    };
    let via_nasdaq = async {
        let plain = nasdaq_symbol(&upper);
        let asset_class = resolve_asset_class(state, &plain).await?;
        let path = format!(
            "/api/quote/{}/info?assetclass={asset_class}",
            urlencoding::encode(&plain)
        );
        let body = nasdaq::<NasdaqInfo>(state, &path, ttl::QUOTE).await?;
        Ok(nasdaq_quote(&body, &plain))
    };

    let via_polygon = polygon::get_quote(state, &upper, AssetClass::Stock);

    Ok(
        price_chain(state, &upper, via_polygon, via_yahoo, via_nasdaq)
            .first_answer()
            .await?
            .value,
    )
}

pub async fn get_candles(
    state: &AppState,
    symbol: &str,
    interval: CandleInterval,
    start_ts: i64,
    end_ts: i64,
) -> Result<SpotCandlesResponse> {
    let upper = symbol.trim().to_uppercase();

    let via_yahoo = async {
        let chart = yahoo_chart(state, &upper, interval, start_ts, end_ts).await?;
        Ok(CandleSet {
            candles: chart.candles,
            name: chart.quote.name,
            currency: chart.quote.currency,
        })
    };
    let via_nasdaq = async {
        Ok(CandleSet {
            candles: nasdaq_candles(state, &upper, interval, start_ts, end_ts).await?,
            name: upper.clone(),
            currency: "USD".to_string(),
        })
    };

    let via_polygon = async {
        let loaded =
            polygon::get_candles(state, &upper, interval, start_ts, end_ts, AssetClass::Stock)
                .await?;
        Ok(CandleSet {
            candles: loaded.candles,
            name: loaded.name,
            currency: loaded.currency,
        })
    };

    // `source` comes from whichever provider answered rather than from a literal
    // written beside each branch, so it cannot disagree with what actually ran.
    let answer = price_chain(state, &upper, via_polygon, via_yahoo, via_nasdaq)
        .first_answer()
        .await?;
    let source = answer.source;
    let CandleSet {
        candles,
        name,
        currency,
    } = answer.value;

    Ok(SpotCandlesResponse {
        symbol: upper.clone(),
        asset_class: AssetClass::Stock,
        name,
        currency,
        interval,
        candles: candles
            .into_iter()
            .filter(|candle| candle.time >= start_ts && candle.time <= end_ts)
            .collect(),
        source: source.to_string(),
    })
}

#[derive(Debug, Default, Clone, Deserialize)]
pub struct YahooSearchResponse {
    #[serde(default)]
    pub quotes: Vec<YahooSearchQuote>,
}

#[derive(Debug, Default, Clone, Deserialize)]
pub struct YahooSearchQuote {
    #[serde(default)]
    pub symbol: Option<String>,
    #[serde(default)]
    pub shortname: Option<String>,
    #[serde(default)]
    pub longname: Option<String>,
    #[serde(default, rename = "exchDisp")]
    pub exch_disp: Option<String>,
    #[serde(default, rename = "quoteType")]
    pub quote_type: Option<String>,
    #[serde(default, rename = "isYahooFinance")]
    pub is_yahoo_finance: Option<bool>,
}

pub async fn search(state: &AppState, query: &str, limit: usize) -> Result<Vec<SpotSearchResult>> {
    let path = format!(
        "/v1/finance/search?q={}&quotesCount={limit}&newsCount=0",
        urlencoding::encode(query)
    );
    let body = yahoo::<YahooSearchResponse>(state, &path, ttl::META).await?;

    Ok(body
        .quotes
        .iter()
        .filter(|quote| quote.symbol.is_some() && quote.is_yahoo_finance != Some(false))
        .filter(|quote| {
            matches!(
                quote.quote_type.as_deref().unwrap_or("EQUITY"),
                "EQUITY" | "ETF" | "INDEX" | "MUTUALFUND"
            )
        })
        .take(limit)
        .map(|quote| {
            let symbol = quote.symbol.clone().unwrap_or_default();
            SpotSearchResult {
                name: quote
                    .longname
                    .clone()
                    .or_else(|| quote.shortname.clone())
                    .unwrap_or_else(|| symbol.clone()),
                symbol,
                asset_class: AssetClass::Stock,
                venue: quote.exch_disp.clone().unwrap_or_default(),
                has_implied: false,
            }
        })
        .collect())
}

fn now_seconds() -> i64 {
    OffsetDateTime::now_utc().unix_timestamp()
}

/// `new Date(ts * 1000).toISOString().slice(0, 10)` — the `YYYY-MM-DD` Nasdaq's
/// historical endpoint takes.
fn iso_date(ts: i64) -> String {
    let date = OffsetDateTime::from_unix_timestamp(ts)
        .unwrap_or(OffsetDateTime::UNIX_EPOCH)
        .date();
    format!(
        "{:04}-{:02}-{:02}",
        date.year(),
        u8::from(date.month()),
        date.day()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::http::Http;
    use serde_json::{json, Value};
    use wiremock::matchers::{header_exists, method, path as path_matcher, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// Real `query1.finance.yahoo.com/v8/finance/chart/AAPL?interval=1d`
    /// response, trimmed. Note: no `previousClose`. Yahoo omits it here and
    /// offers only `chartPreviousClose`, which is the close before the *window*.
    const YAHOO_AAPL: &str = include_str!("fixtures/yahoo_aapl_chart.json");

    fn yahoo_aapl() -> YahooChartResponse {
        serde_json::from_str(YAHOO_AAPL).expect("the AAPL fixture parses")
    }

    fn yahoo_aapl_value() -> Value {
        serde_json::from_str(YAHOO_AAPL).expect("the AAPL fixture parses")
    }

    fn from_value(value: Value) -> YahooChartResponse {
        serde_json::from_value(value).expect("a chart payload")
    }

    fn state_for(server: &MockServer) -> AppState {
        AppState::with_http(
            Config {
                yahoo_api_base: server.uri().trim_end_matches('/').to_string(),
                nasdaq_api_base: server.uri().trim_end_matches('/').to_string(),
                ..Config::default()
            },
            Http::new(),
        )
    }

    /* ------------------------------------------------------------- yahoo */

    #[test]
    fn reads_the_bars_out_of_the_parallel_arrays() {
        let reading = parse_yahoo_chart(&yahoo_aapl(), "AAPL", CandleInterval::OneDay).unwrap();
        assert_eq!(reading.candles.len(), 5);
        let first = reading.candles[0];
        assert_eq!(first.time, 1_786_368_600);
        assert_eq!(first.open, 306.8299865722656);
        assert_eq!(first.high, 308.260009765625);
        assert_eq!(first.low, 304.6099853515625);
        assert_eq!(first.close, 308.260009765625);
        assert_eq!(first.volume, 44_812_500.0);
    }

    #[test]
    fn takes_the_previous_close_from_the_penultimate_daily_bar() {
        // The bug this guards: `chartPreviousClose` (313.33) is the close before
        // the requested week, so trusting it quoted AAPL at -2.4% on a day it
        // closed +0.22%. The bar before the last one is the real prior session.
        let quote = parse_yahoo_chart(&yahoo_aapl(), "AAPL", CandleInterval::OneDay)
            .unwrap()
            .quote;
        assert_eq!(quote.previous_close, Some(305.260009765625));
        assert!((quote.change.unwrap() - 0.67).abs() < 0.01);
        assert!((quote.change_percent.unwrap() - 0.219).abs() < 0.01);
    }

    #[test]
    fn falls_back_to_chart_previous_close_when_the_series_is_not_daily() {
        // On an intraday series the penultimate bar is a minute ago, not a session.
        let quote = parse_yahoo_chart(&yahoo_aapl(), "AAPL", CandleInterval::OneHour)
            .unwrap()
            .quote;
        assert_eq!(quote.previous_close, Some(313.33));
    }

    #[test]
    fn takes_the_day_open_from_the_last_bar_not_the_first() {
        // The first bar opens the *window*; only the last one opens today.
        let quote = parse_yahoo_chart(&yahoo_aapl(), "AAPL", CandleInterval::OneDay)
            .unwrap()
            .quote;
        assert_eq!(quote.day_open, Some(306.0));
    }

    #[test]
    fn carries_the_instrument_metadata_through() {
        let quote = parse_yahoo_chart(&yahoo_aapl(), "AAPL", CandleInterval::OneDay)
            .unwrap()
            .quote;
        assert_eq!(quote.symbol, "AAPL");
        assert_eq!(quote.name, "Apple Inc.");
        assert_eq!(quote.venue, "NasdaqGS");
        assert_eq!(quote.currency, "USD");
        assert_eq!(quote.source, "yahoo");
        assert_eq!(quote.price, Some(305.93));
    }

    #[test]
    fn skips_buckets_yahoo_padded_with_nulls() {
        // Halted or not-yet-printed periods come back as null in every array. A bar
        // with no close is not a bar, and interpolating one would invent a price.
        let mut padded = yahoo_aapl_value();
        padded["chart"]["result"][0]["indicators"]["quote"][0]["close"][2] = Value::Null;
        let reading =
            parse_yahoo_chart(&from_value(padded), "AAPL", CandleInterval::OneDay).unwrap();
        assert_eq!(reading.candles.len(), 4);
        assert!(!reading
            .candles
            .iter()
            .any(|candle| candle.time == 1_786_541_400));
    }

    #[test]
    fn backfills_a_missing_high_or_low_from_the_bar_it_does_have() {
        let mut partial = yahoo_aapl_value();
        partial["chart"]["result"][0]["indicators"]["quote"][0]["high"][0] = Value::Null;
        partial["chart"]["result"][0]["indicators"]["quote"][0]["low"][0] = Value::Null;
        let candles = parse_yahoo_chart(&from_value(partial), "AAPL", CandleInterval::OneDay)
            .unwrap()
            .candles;
        assert_eq!(candles[0].high, candles[0].open.max(candles[0].close));
        assert_eq!(candles[0].low, candles[0].open.min(candles[0].close));
    }

    #[test]
    fn reports_a_diagnosable_error_when_yahoo_has_no_result() {
        let body = from_value(json!({
            "chart": { "result": [], "error": { "description": "No data found" } }
        }));
        let error = parse_yahoo_chart(&body, "NOPE", CandleInterval::OneDay).unwrap_err();
        assert_eq!(error.message, "No data found");
        assert_eq!(error.code, codes::NOT_FOUND);
        assert!(error.hint.unwrap().contains("^GSPC"));
    }

    /* ------------------------------------------------------------ nasdaq */

    #[test]
    fn strips_currency_symbols_and_thousands_separators() {
        assert_eq!(parse_nasdaq_number(Some("$305.93")), Some(305.93));
        assert_eq!(parse_nasdaq_number(Some("28,229,611")), Some(28_229_611.0));
        assert_eq!(parse_nasdaq_number(Some("+0.22%")), Some(0.22));
        assert_eq!(parse_nasdaq_number(Some("776.34")), Some(776.34));
    }

    #[test]
    fn returns_none_for_the_absent_value_markers_rather_than_zero() {
        assert_eq!(parse_nasdaq_number(Some("N/A")), None);
        assert_eq!(parse_nasdaq_number(Some("--")), None);
        assert_eq!(parse_nasdaq_number(Some("")), None);
        assert_eq!(parse_nasdaq_number(None), None);
    }

    #[test]
    fn reads_mm_dd_yyyy_as_utc_midnight() {
        assert_eq!(
            parse_nasdaq_date(Some("08/14/2026")),
            Some(utc_midnight(2026, 8, 14))
        );
    }

    #[test]
    fn returns_none_for_any_other_date_shape() {
        assert_eq!(parse_nasdaq_date(Some("2026-08-14")), None);
        assert_eq!(parse_nasdaq_date(None), None);
    }

    /// `Date.UTC(year, month - 1, day) / 1000`, for the assertions above.
    fn utc_midnight(year: i32, month: u8, day: u8) -> i64 {
        Date::from_calendar_date(year, Month::try_from(month).unwrap(), day)
            .unwrap()
            .midnight()
            .assume_utc()
            .unix_timestamp()
    }

    fn historical(value: Value) -> NasdaqEnvelope<NasdaqHistorical> {
        serde_json::from_value(value).expect("a historical payload")
    }

    #[test]
    fn sorts_oldest_first_and_parses_the_money_strings() {
        // Real `api.nasdaq.com/api/quote/AAPL/historical` rows, newest first.
        let body = historical(json!({
            "data": { "tradesTable": { "rows": [
                { "date": "08/14/2026", "close": "$305.93", "volume": "28,229,380", "open": "$306.00", "high": "$307.49", "low": "$304.30" },
                { "date": "08/13/2026", "close": "$305.26", "volume": "40,349,290", "open": "$304.21", "high": "$306.00", "low": "$302.05" },
                { "date": "08/12/2026", "close": "$302.25", "volume": "41,657,770", "open": "$305.10", "high": "$305.66", "low": "$300.57" }
            ] } }
        }));

        let candles = parse_nasdaq_historical(&body);
        assert_eq!(candles.len(), 3);
        assert_eq!(
            candles.iter().map(|c| c.close).collect::<Vec<_>>(),
            [302.25, 305.26, 305.93]
        );
        let newest = candles[2];
        assert_eq!(newest.time, utc_midnight(2026, 8, 14));
        assert_eq!(newest.open, 306.0);
        assert_eq!(newest.high, 307.49);
        assert_eq!(newest.low, 304.3);
        assert_eq!(newest.close, 305.93);
        assert_eq!(newest.volume, 28_229_380.0);
    }

    #[test]
    fn drops_rows_with_no_usable_date_or_close() {
        let candles = parse_nasdaq_historical(&historical(json!({
            "data": { "tradesTable": { "rows": [
                { "date": "N/A", "close": "$1.00" },
                { "date": "08/14/2026" }
            ] } }
        })));
        assert_eq!(candles.len(), 0);
    }

    #[test]
    fn survives_an_empty_payload() {
        assert!(parse_nasdaq_historical(&historical(json!({}))).is_empty());
        assert!(parse_nasdaq_historical(&historical(json!({ "data": null }))).is_empty());
    }

    /* ---------------------------------------------------------- the wire */

    fn chart_response() -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_raw(YAHOO_AAPL, "application/json")
    }

    #[tokio::test]
    async fn quotes_from_yahoo_and_says_so() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/v8/finance/chart/AAPL"))
            .and(query_param("interval", "1d"))
            .and(header_exists("User-Agent"))
            .respond_with(chart_response())
            .expect(1)
            .mount(&server)
            .await;

        let quote = get_quote(&state_for(&server), " aapl ").await.unwrap();
        assert_eq!(quote.symbol, "AAPL");
        assert_eq!(quote.source, "yahoo");
        assert_eq!(quote.price, Some(305.93));

        let sent = &server.received_requests().await.unwrap()[0];
        assert_eq!(sent.headers["user-agent"], DESKTOP_UA);
    }

    #[tokio::test]
    async fn clamps_a_one_minute_window_to_yahoos_seven_day_limit() {
        // Yahoo answers a too-wide 1m window with an error rather than a shorter
        // series, so the clamp has to happen here.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/v8/finance/chart/AAPL"))
            .and(query_param("interval", "1m"))
            .and(query_param(
                "period1",
                (2_000_000_000i64 - 7 * 86_400).to_string(),
            ))
            .and(query_param("period2", "2000000000"))
            .respond_with(chart_response())
            .expect(1)
            .mount(&server)
            .await;

        get_candles(
            &state_for(&server),
            "AAPL",
            CandleInterval::OneMinute,
            1_000_000_000,
            2_000_000_000,
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn keeps_only_the_bars_inside_the_requested_window() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/v8/finance/chart/AAPL"))
            .respond_with(chart_response())
            .mount(&server)
            .await;

        let response = get_candles(
            &state_for(&server),
            "AAPL",
            CandleInterval::OneDay,
            1_786_455_000,
            1_786_627_800,
        )
        .await
        .unwrap();

        assert_eq!(response.source, "yahoo");
        assert_eq!(response.name, "Apple Inc.");
        assert_eq!(
            response.candles.iter().map(|c| c.time).collect::<Vec<_>>(),
            [1_786_455_000, 1_786_541_400, 1_786_627_800]
        );
    }

    /// Nasdaq's answer for a symbol it does file under `assetclass`.
    fn nasdaq_info() -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_json(json!({
            "data": {
                "symbol": "AAPL",
                "companyName": "Apple Inc. Common Stock",
                "exchange": "NASDAQ-GS",
                "primaryData": {
                    "lastSalePrice": "$305.93",
                    "netChange": "0.67",
                    "percentageChange": "0.22%",
                    "volume": "28,229,611"
                },
                "keyStats": {
                    "PreviousClose": { "value": "$305.26" },
                    "OpenPrice": { "value": "$306.00" }
                }
            },
            "status": { "rCode": 200 }
        }))
    }

    /// What Nasdaq says when the guessed `assetclass` is the wrong one.
    fn nasdaq_wrong_class() -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_json(json!({
            "data": null,
            "status": { "rCode": 400, "bCodeMessage": [{ "errorMessage": "Symbol not found" }] }
        }))
    }

    #[tokio::test]
    async fn falls_back_to_nasdaq_and_probes_for_the_asset_class() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/v8/finance/chart/QQQ"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path_matcher("/api/quote/QQQ/info"))
            .and(query_param("assetclass", "stocks"))
            .respond_with(nasdaq_wrong_class())
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path_matcher("/api/quote/QQQ/info"))
            .and(query_param("assetclass", "etf"))
            .respond_with(nasdaq_info())
            // Twice: once to resolve the class at `ttl::META`, once for the
            // quote itself at `ttl::QUOTE`. See the note on `nasdaq()`.
            .expect(2)
            .mount(&server)
            .await;

        let quote = get_quote(&state_for(&server), "QQQ").await.unwrap();
        assert_eq!(quote.source, "nasdaq");
        assert_eq!(quote.price, Some(305.93));
        assert_eq!(quote.previous_close, Some(305.26));
        assert_eq!(quote.change, Some(0.67));
        assert_eq!(quote.change_percent, Some(0.22));
        assert_eq!(quote.day_open, Some(306.0));
        assert_eq!(quote.volume, Some(28_229_611.0));
        assert_eq!(quote.venue, "NASDAQ-GS");
    }

    #[tokio::test]
    async fn a_quote_read_is_not_served_the_asset_class_probes_stale_entry() {
        // The bug the TTL-in-the-key fixes: the resolver writes the same URL at
        // 60s, and the quote asking for 3s freshness used to be handed that
        // entry — up to twenty times staler than it asked for.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/v8/finance/chart/AAPL"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path_matcher("/api/quote/AAPL/info"))
            .and(query_param("assetclass", "stocks"))
            .respond_with(nasdaq_info())
            .expect(2)
            .mount(&server)
            .await;

        let state = state_for(&server);
        get_quote(&state, "AAPL").await.unwrap();
        assert!(state
            .cache()
            .get::<String>("nasdaq:class:AAPL")
            .await
            .is_some());
        // Both keys exist side by side rather than one shadowing the other.
        let path = "/api/quote/AAPL/info?assetclass=stocks";
        assert!(state
            .cache()
            .get::<NasdaqEnvelope<NasdaqInfo>>(&format!("nasdaq:60000:{path}"))
            .await
            .is_some());
        assert!(state
            .cache()
            .get::<NasdaqEnvelope<NasdaqInfo>>(&format!("nasdaq:3000:{path}"))
            .await
            .is_some());
    }

    #[tokio::test]
    async fn nasdaq_reports_the_intraday_gap_rather_than_serving_daily_bars() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/v8/finance/chart/AAPL"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path_matcher("/api/quote/AAPL/info"))
            .and(query_param("assetclass", "stocks"))
            .respond_with(nasdaq_info())
            .mount(&server)
            .await;

        let error = get_candles(
            &state_for(&server),
            "AAPL",
            CandleInterval::OneHour,
            1_786_368_600,
            1_786_714_200,
        )
        .await
        .unwrap_err();

        // `unsupported` is a pass-through code: the capability gap reaches the
        // caller as itself (501) rather than as a generic chain failure.
        assert_eq!(error.code, codes::UNSUPPORTED);
        assert_eq!(error.message, "Nasdaq has no hourly history for AAPL");
    }

    #[tokio::test]
    async fn nasdaq_serves_daily_bars_oldest_first() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/v8/finance/chart/%5EIXIC"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;
        for wrong in ["stocks", "etf"] {
            Mock::given(method("GET"))
                .and(path_matcher("/api/quote/IXIC/info"))
                .and(query_param("assetclass", wrong))
                .respond_with(nasdaq_wrong_class())
                .mount(&server)
                .await;
        }
        // A caret-prefixed index loses the caret on the way to Nasdaq.
        Mock::given(method("GET"))
            .and(path_matcher("/api/quote/IXIC/info"))
            .and(query_param("assetclass", "index"))
            .respond_with(nasdaq_info())
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path_matcher("/api/quote/IXIC/historical"))
            .and(query_param("assetclass", "index"))
            .and(query_param("fromdate", "2026-08-12"))
            .and(query_param("todate", "2026-08-14"))
            .and(query_param("limit", "9999"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": { "tradesTable": { "rows": [
                    { "date": "08/14/2026", "close": "$305.93", "volume": "28,229,380", "open": "$306.00", "high": "$307.49", "low": "$304.30" },
                    { "date": "08/12/2026", "close": "$302.25", "volume": "41,657,770", "open": "$305.10", "high": "$305.66", "low": "$300.57" }
                ] } },
                "status": { "rCode": 200 }
            })))
            .mount(&server)
            .await;

        let response = get_candles(
            &state_for(&server),
            "^IXIC",
            CandleInterval::OneDay,
            utc_midnight(2026, 8, 12),
            utc_midnight(2026, 8, 14),
        )
        .await
        .unwrap();

        assert_eq!(response.source, "nasdaq");
        assert_eq!(response.symbol, "^IXIC");
        assert_eq!(
            response.candles.iter().map(|c| c.close).collect::<Vec<_>>(),
            [302.25, 305.93]
        );
    }

    #[tokio::test]
    async fn a_symbol_nasdaq_does_not_list_ends_the_chain_with_its_own_error() {
        // `not_found` is definitive: asking a third provider cannot turn a
        // symbol that does not exist into one that does.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/v8/finance/chart/%5EGSPC"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path_matcher("/api/quote/GSPC/info"))
            .respond_with(nasdaq_wrong_class())
            .expect(3)
            .mount(&server)
            .await;

        let error = get_quote(&state_for(&server), "^GSPC").await.unwrap_err();
        assert_eq!(error.code, codes::NOT_FOUND);
        assert_eq!(error.message, "Nasdaq does not list GSPC");
        assert!(error.hint.unwrap().contains("S&P 500 or Dow index feed"));
    }

    /// A state whose asset-class memo is already warm, so the Nasdaq arm goes
    /// straight to the quote call — the only way both arms fail without the
    /// resolver's definitive `not_found` ending the chain first.
    async fn state_with_known_class(server: &MockServer, symbol: &str, class: &str) -> AppState {
        let state = state_for(server);
        state
            .cache()
            .set(
                &format!("nasdaq:class:{symbol}"),
                class.to_string(),
                ttl::CATALOGUE,
            )
            .await;
        state
    }

    #[tokio::test]
    async fn names_a_yahoo_rate_limit_when_both_providers_fail() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/v8/finance/chart/%5EGSPC"))
            .respond_with(ResponseTemplate::new(429))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path_matcher("/api/quote/GSPC/info"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;

        let state = state_with_known_class(&server, "GSPC", "index").await;
        let error = get_quote(&state, "^GSPC").await.unwrap_err();
        assert_eq!(error.code, codes::UPSTREAM_ERROR);
        assert_eq!(error.message, "No price source could quote ^GSPC");
        let hint = error.hint.unwrap();
        assert!(
            hint.starts_with("Yahoo Finance is rate-limiting this IP"),
            "{hint}"
        );
        assert!(hint.contains("does not cover ^GSPC"), "{hint}");
    }

    #[tokio::test]
    async fn a_keyless_deployment_blocked_by_yahoo_is_told_a_polygon_key_is_the_fix() {
        // Yahoo refuses shared datacentre addresses with a 429, which is where
        // a hosted deployment of this terminal runs, and the advice the chain
        // otherwise gives — the same request usually succeeds from a
        // residential connection — is not something its operator can act on.
        // Setting a key is. This is the reason the Polygon arm exists, said to
        // the person who can do something about it.
        //
        // It is also what makes `.available(polygon::has_credentials(state))`
        // load-bearing rather than merely tidy: the arm has to be *skipped* for
        // a skipped-hint to fire at all, so dropping that call turns Polygon
        // into a failed arm and takes this hint away with it.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/v8/finance/chart/%5EGSPC"))
            .respond_with(ResponseTemplate::new(429))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path_matcher("/api/quote/GSPC/info"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;

        let state = state_with_known_class(&server, "GSPC", "index").await;
        let error = get_quote(&state, "^GSPC").await.unwrap_err();
        let hint = error.hint.unwrap();
        assert!(hint.contains("POLYGON_API_KEY"), "{hint}");
        assert!(
            hint.contains("https://polygon.io/dashboard/signup"),
            "{hint}"
        );
        // The chain's own synthesised message survives: the advice is added to
        // the hint rather than replacing what happened with a raw upstream
        // status the reader cannot act on either.
        assert_eq!(error.code, codes::UPSTREAM_ERROR);
        assert_eq!(error.message, "No price source could quote ^GSPC");
        assert!(
            hint.starts_with("Yahoo Finance is rate-limiting this IP"),
            "{hint}"
        );
    }

    #[tokio::test]
    async fn a_deployment_that_already_holds_a_polygon_key_is_not_told_to_get_one() {
        // The remediation only makes sense to somebody who has not set the key.
        // An operator who has, and whose chain is down anyway, would be reading
        // advice they already took.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/v8/finance/chart/%5EGSPC"))
            .respond_with(ResponseTemplate::new(429))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path_matcher("/api/quote/GSPC/info"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;

        let mut state = state_with_known_class(&server, "GSPC", "index").await;
        state = AppState::new(Config {
            polygon_api_key: Some("a-real-key".to_owned()),
            polygon_api_base: server.uri(),
            ..state.config().clone()
        });
        state
            .cache()
            .set("nasdaq:class:GSPC", "index".to_owned(), ttl::CATALOGUE)
            .await;

        let error = get_quote(&state, "^GSPC").await.unwrap_err();
        assert!(
            !error.hint.unwrap_or_default().contains("POLYGON_API_KEY"),
            "a keyed deployment was told to set the key it already set"
        );
    }

    #[tokio::test]
    async fn blames_the_symbol_when_yahoo_did_not_rate_limit() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/v8/finance/chart/WAT"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path_matcher("/api/quote/WAT/info"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;

        let state = state_with_known_class(&server, "WAT", "stocks").await;
        let error = get_quote(&state, "WAT").await.unwrap_err();
        assert!(error.hint.unwrap().contains("cash indices need a caret"));
    }

    #[tokio::test]
    async fn search_keeps_the_instrument_types_the_terminal_can_chart() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/v1/finance/search"))
            .and(query_param("q", "apple"))
            .and(query_param("quotesCount", "3"))
            .and(query_param("newsCount", "0"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "quotes": [
                    { "symbol": "AAPL", "shortname": "Apple Inc.", "longname": "Apple Inc.", "exchDisp": "NASDAQ", "quoteType": "EQUITY", "isYahooFinance": true },
                    { "symbol": "APLE", "shortname": "Apple Hospitality REIT", "exchDisp": "NYSE", "quoteType": "EQUITY" },
                    { "symbol": "AAPL.MX", "shortname": "Apple", "quoteType": "CURRENCY" },
                    { "shortname": "no symbol at all", "quoteType": "EQUITY" },
                    { "symbol": "AAPLX", "shortname": "not a Yahoo instrument", "isYahooFinance": false }
                ]
            })))
            .mount(&server)
            .await;

        let results = search(&state_for(&server), "apple", 3).await.unwrap();
        assert_eq!(
            results
                .iter()
                .map(|r| r.symbol.as_str())
                .collect::<Vec<_>>(),
            ["AAPL", "APLE"]
        );
        assert_eq!(results[1].name, "Apple Hospitality REIT");
        assert_eq!(results[1].venue, "NYSE");
        assert_eq!(results[0].asset_class, AssetClass::Stock);
        assert!(!results[0].has_implied);
    }
}
