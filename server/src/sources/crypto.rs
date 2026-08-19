//! Crypto spot prices from Coinbase Exchange's public market-data API.
//!
//! Chosen over the alternatives for boring reasons that matter here: no key, no
//! account, no geo-fence, documented rate limits, and it answers from datacentre
//! IPs — which the equity feeds mostly do not (see [`super::stocks`]). It is
//! also the venue behind Kalshi's crypto settlement family (CF Benchmarks
//! indices track the same USD spot market), so the true price and the implied
//! price are describing the same thing.
//!
//! The one sharp edge is `/candles`: it caps a response at 300 buckets and
//! returns them *newest first*, so any window worth charting has to be walked
//! backwards in pages and reversed.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::Value;
use terminal_core::types::{
    AssetClass, CandleInterval, SpotCandle, SpotCandlesResponse, SpotQuote,
};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use crate::app::AppState;
use crate::cache::ttl;
use crate::error::{codes, Result, UpstreamError};
use crate::http::{FetchOptions, DESKTOP_UA};

/// Coinbase granularities, in seconds. These are the only accepted values.
fn granularity_of(interval: CandleInterval) -> i64 {
    match interval {
        CandleInterval::OneMinute => 60,
        CandleInterval::OneHour => 3_600,
        CandleInterval::OneDay => 86_400,
    }
}

/// Hard cap Coinbase applies to one `/candles` response.
const MAX_BUCKETS: i64 = 300;

/// Pages per request, so a 1-year daily window cannot fan out unboundedly.
const MAX_PAGES: usize = 12;

/// Coinbase ships every figure as a decimal string, so the fields are read as
/// raw JSON and coerced the way `Number(value)` did.
#[derive(Debug, Default, Clone, Deserialize)]
struct RawTicker {
    #[serde(default)]
    price: Option<Value>,
    #[serde(default)]
    time: Option<String>,
}

#[derive(Debug, Default, Clone, Deserialize)]
struct RawStats {
    #[serde(default)]
    open: Option<Value>,
    #[serde(default)]
    high: Option<Value>,
    #[serde(default)]
    low: Option<Value>,
    #[serde(default)]
    last: Option<Value>,
    #[serde(default)]
    volume: Option<Value>,
}

/// One listed Coinbase product, as the search route consumes it.
#[derive(Debug, Default, Clone, PartialEq, Eq, Deserialize)]
pub struct Product {
    pub id: String,
    #[serde(default)]
    pub base_currency: Option<String>,
    #[serde(default)]
    pub quote_currency: Option<String>,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub trading_disabled: Option<bool>,
}

/// `Number(value)`: a decimal string or a number, and `null`/`""` for absent.
fn num(value: Option<&Value>) -> Option<f64> {
    match value? {
        Value::Number(number) => number.as_f64().filter(|n| n.is_finite()),
        Value::String(text) if text.is_empty() => None,
        Value::String(text) => text.trim().parse::<f64>().ok().filter(|n| n.is_finite()),
        _ => None,
    }
}

/// Symbol → Coinbase product id.
///
/// `BTC` and `BTC-USD` both mean the same thing to a person typing into a
/// terminal, and only one of them means anything to Coinbase.
pub fn product_id(symbol: &str) -> String {
    let upper = symbol.trim().to_uppercase();
    if upper.contains('-') {
        return upper;
    }
    format!("{upper}-USD")
}

/// The bare asset symbol, i.e. the inverse of [`product_id`].
pub fn base_symbol(symbol: &str) -> String {
    let upper = symbol.trim().to_uppercase();
    upper
        .split('-')
        .next()
        .map_or(upper.clone(), str::to_string)
}

async fn get<T>(state: &AppState, path: &str, ttl: Duration) -> Result<Arc<T>>
where
    T: DeserializeOwned + Send + Sync + 'static,
{
    let key = format!("coinbase:{path}");
    state
        .cache()
        .cached(&key, ttl, || async {
            let url = format!("{}{path}", state.config().coinbase_api_base);
            state
                .http()
                .fetch_json::<T>(
                    &url,
                    // Coinbase 403s a request with no User-Agent, so the
                    // browser-shaped header is not optional here even though
                    // this is a JSON API.
                    FetchOptions::new()
                        .timeout(Duration::from_secs(15))
                        .retries(2)
                        .header("User-Agent", DESKTOP_UA),
                )
                .await
        })
        .await
}

/// Tradeable USD-quoted products, for symbol search and validation.
pub async fn list_products(state: &AppState) -> Result<Vec<Product>> {
    let products = get::<Vec<Product>>(state, "/products", ttl::CATALOGUE).await?;
    Ok(products
        .iter()
        .filter(|p| {
            p.status.as_deref() == Some("online")
                && !p.trading_disabled.unwrap_or(false)
                && p.quote_currency.as_deref() == Some("USD")
        })
        .cloned()
        .collect())
}

pub async fn get_quote(state: &AppState, symbol: &str) -> Result<SpotQuote> {
    let id = product_id(symbol);

    // `/ticker` is the live print; `/stats` carries the 24h open/high/low that
    // makes a change figure meaningful. Neither contains the other.
    let (ticker, stats) = futures::future::join(
        get::<RawTicker>(
            state,
            &format!("/products/{}/ticker", urlencoding::encode(&id)),
            ttl::QUOTE,
        ),
        get::<RawStats>(
            state,
            &format!("/products/{}/stats", urlencoding::encode(&id)),
            ttl::QUOTE,
        ),
    )
    .await;

    let ticker = ticker.map_err(|err| not_found(err, &id))?;
    let stats = stats.unwrap_or_default();

    let price = num(ticker.price.as_ref()).or_else(|| num(stats.last.as_ref()));
    // Crypto has no session close, so the 24h open is the honest comparison.
    let previous_close = num(stats.open.as_ref());
    let change = match (price, previous_close) {
        (Some(price), Some(previous_close)) => Some(price - previous_close),
        _ => None,
    };

    Ok(SpotQuote {
        symbol: base_symbol(&id),
        asset_class: AssetClass::Crypto,
        name: pair_name(&id),
        currency: quote_currency(&id),
        price,
        previous_close,
        change,
        change_percent: match (change, previous_close) {
            (Some(change), Some(previous_close)) if previous_close != 0.0 => {
                Some((change / previous_close) * 100.0)
            }
            _ => None,
        },
        day_open: num(stats.open.as_ref()),
        day_high: num(stats.high.as_ref()),
        day_low: num(stats.low.as_ref()),
        volume: num(stats.volume.as_ref()),
        venue: "Coinbase".to_string(),
        time: ticker
            .time
            .as_deref()
            .filter(|time| !time.is_empty())
            .and_then(parse_iso)
            .unwrap_or_else(now_seconds),
        source: "coinbase".to_string(),
    })
}

/// A 404 from `/ticker` means the pair is not listed — say which pair.
fn not_found(err: UpstreamError, id: &str) -> UpstreamError {
    if err.code == codes::NOT_FOUND || err.status == Some(404) {
        return UpstreamError::not_found(format!("Coinbase does not list the pair {id}"))
            .with_hint(format!(
                "Try the base symbol on its own, e.g. `CRY BTC`, or a listed pair such as {}-USDC.",
                id.split('-').next().unwrap_or(id)
            ));
    }
    err
}

/// Coinbase returns candles as positional arrays:
/// `[time, low, high, open, close, volume]`.
///
/// The rows arrive as raw JSON because a malformed one has to be skipped rather
/// than fail the page: a truncated row or a `null` in the array is a reason to
/// drop that bar, not to lose the other 299.
pub fn normalise_candles(raw: &[Value]) -> Vec<SpotCandle> {
    let mut candles: Vec<SpotCandle> = Vec::new();
    for row in raw {
        let Some(row) = row.as_array() else { continue };
        if row.len() < 6 {
            continue;
        }
        let (Some(time), Some(low), Some(high), Some(open), Some(close)) = (
            row[0].as_f64(),
            row[1].as_f64(),
            row[2].as_f64(),
            row[3].as_f64(),
            row[4].as_f64(),
        ) else {
            continue;
        };
        if !time.is_finite() || !close.is_finite() {
            continue;
        }
        candles.push(SpotCandle {
            time: time as i64,
            open,
            high,
            low,
            close,
            volume: row[5].as_f64().filter(|v| v.is_finite()).unwrap_or(0.0),
        });
    }
    candles
}

/// Candles for `[start_ts, end_ts]`, walking backwards in 300-bucket pages.
///
/// Coinbase silently truncates rather than paginating, so asking for a window
/// wider than 300 buckets and trusting the answer would quietly lose the older
/// end of every chart — the part a historical overlay is entirely about.
pub async fn get_candles(
    state: &AppState,
    symbol: &str,
    interval: CandleInterval,
    start_ts: i64,
    end_ts: i64,
) -> Result<SpotCandlesResponse> {
    let id = product_id(symbol);
    let granularity = granularity_of(interval);
    let span = granularity * MAX_BUCKETS;

    // Keyed by time, so a bucket returned by two overlapping pages is stored
    // once; ordered, so the walk's newest-first pages come out ascending.
    let mut collected: BTreeMap<i64, SpotCandle> = BTreeMap::new();
    let mut window_end = end_ts;

    for _page in 0..MAX_PAGES {
        if window_end <= start_ts {
            break;
        }
        let window_start = start_ts.max(window_end - span);
        let path = format!(
            "/products/{}/candles?granularity={granularity}&start={}&end={}",
            urlencoding::encode(&id),
            iso_instant(window_start),
            iso_instant(window_end),
        );

        let raw = get::<Value>(state, &path, ttl::CANDLES)
            .await
            .map_err(|err| not_found(err, &id))?;
        let page = normalise_candles(raw.as_array().map_or(&[][..], Vec::as_slice));
        for candle in &page {
            collected.insert(candle.time, *candle);
        }

        if page.is_empty() {
            break;
        }
        // Step to the bucket before the oldest one returned. Using the response's
        // own oldest timestamp rather than the requested start keeps the walk
        // correct across market gaps.
        let oldest = page
            .iter()
            .map(|candle| candle.time)
            .min()
            .unwrap_or(i64::MAX);
        if oldest <= start_ts {
            break;
        }
        window_end = oldest - granularity;
    }

    Ok(SpotCandlesResponse {
        symbol: base_symbol(&id),
        asset_class: AssetClass::Crypto,
        name: pair_name(&id),
        currency: quote_currency(&id),
        interval,
        candles: collected.into_values().collect(),
        source: "coinbase".to_string(),
    })
}

/// Substring search over listed USD pairs.
pub async fn search(state: &AppState, query: &str, limit: usize) -> Result<Vec<Product>> {
    let needle = query.trim().to_uppercase();
    if needle.is_empty() {
        return Ok(Vec::new());
    }
    let products = list_products(state).await?;

    let mut scored: Vec<(u8, Product)> = products
        .into_iter()
        .map(|product| {
            let base = product
                .base_currency
                .clone()
                .or_else(|| product.id.split('-').next().map(str::to_string))
                .unwrap_or_default();
            let score = if base == needle {
                100
            } else if base.starts_with(&needle) {
                60
            } else if base.contains(&needle) {
                30
            } else if product
                .display_name
                .as_deref()
                .unwrap_or_default()
                .to_uppercase()
                .contains(&needle)
            {
                10
            } else {
                0
            };
            (score, product)
        })
        .filter(|(score, _)| *score > 0)
        .collect();

    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.id.cmp(&b.1.id)));
    Ok(scored
        .into_iter()
        .take(limit)
        .map(|(_, product)| product)
        .collect())
}

/// `BTC-USD` → `BTC / USD`. Only the first hyphen is replaced, as the
/// TypeScript's single-pattern `String.replace` did.
fn pair_name(id: &str) -> String {
    id.replacen('-', " / ", 1)
}

/// The quote leg of a product id, defaulting to USD.
fn quote_currency(id: &str) -> String {
    id.split('-')
        .nth(1)
        .map_or_else(|| "USD".to_string(), str::to_string)
}

fn now_seconds() -> i64 {
    OffsetDateTime::now_utc().unix_timestamp()
}

fn parse_iso(raw: &str) -> Option<i64> {
    OffsetDateTime::parse(raw, &Rfc3339)
        .ok()
        .map(OffsetDateTime::unix_timestamp)
}

/// `new Date(ts * 1000).toISOString()`, which is what `/candles` takes for its
/// `start` and `end`.
fn iso_instant(ts: i64) -> String {
    let moment = OffsetDateTime::from_unix_timestamp(ts).unwrap_or(OffsetDateTime::UNIX_EPOCH);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.000Z",
        moment.year(),
        u8::from(moment.month()),
        moment.day(),
        moment.hour(),
        moment.minute(),
        moment.second(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::http::Http;
    use serde_json::json;
    use wiremock::matchers::{header_exists, method, path as path_matcher, query_param};
    use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

    fn state_for(server: &MockServer) -> AppState {
        AppState::with_http(
            Config {
                coinbase_api_base: server.uri().trim_end_matches('/').to_string(),
                ..Config::default()
            },
            Http::new(),
        )
    }

    /* ----------------------------------------------------------- symbols */

    #[test]
    fn assumes_a_usd_quote_when_only_the_asset_is_given() {
        assert_eq!(product_id("btc"), "BTC-USD");
        assert_eq!(product_id("ETH"), "ETH-USD");
    }

    #[test]
    fn leaves_an_explicit_pair_alone() {
        assert_eq!(product_id("BTC-USDC"), "BTC-USDC");
    }

    #[test]
    fn recovers_the_bare_asset_symbol() {
        assert_eq!(base_symbol("BTC-USD"), "BTC");
        assert_eq!(base_symbol("eth"), "ETH");
    }

    /* ----------------------------------------------------------- candles */

    /// Real Coinbase rows: `[time, low, high, open, close, volume]`, newest first.
    fn raw_rows() -> Vec<Value> {
        vec![
            json!([
                1_786_892_400,
                63_034.7,
                63_050,
                63_037.75,
                63_039.67,
                7.352_487_51
            ]),
            json!([
                1_786_888_800,
                62_987.68,
                63_039,
                62_993.64,
                63_037.75,
                73.748_411_28
            ]),
        ]
    }

    #[test]
    fn maps_the_positional_array_onto_named_fields_in_the_right_order() {
        // The trap: the array is low/high/open/close, not open/high/low/close.
        let candles = normalise_candles(&raw_rows());
        let first = candles[0];
        assert_eq!(first.time, 1_786_892_400);
        assert_eq!(first.open, 63_037.75);
        assert_eq!(first.high, 63_050.0);
        assert_eq!(first.low, 63_034.7);
        assert_eq!(first.close, 63_039.67);
        assert_eq!(first.volume, 7.352_487_51);
    }

    #[test]
    fn skips_malformed_rows_instead_of_emitting_nan_bars() {
        let mut rows = raw_rows();
        rows.push(json!([1_786_885_200, 1, 2]));
        rows.push(Value::Null);
        assert_eq!(normalise_candles(&rows).len(), 2);
    }

    /* ---------------------------------------------------------- the wire */

    #[tokio::test]
    async fn merges_the_live_print_with_the_twenty_four_hour_stats() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/products/BTC-USD/ticker"))
            // Coinbase 403s a request with no User-Agent.
            .and(header_exists("User-Agent"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "trade_id": 812_663_745,
                "price": "63039.67",
                "size": "0.00035",
                "bid": "63039.66",
                "ask": "63039.67",
                "volume": "9182.44",
                "time": "2026-08-18T09:15:30.123456Z"
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path_matcher("/products/BTC-USD/stats"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "open": "62000.00",
                "high": "63500.00",
                "low": "61800.00",
                "last": "63039.67",
                "volume": "9182.44"
            })))
            .mount(&server)
            .await;

        let quote = get_quote(&state_for(&server), "btc").await.unwrap();
        for sent in server.received_requests().await.unwrap() {
            assert_eq!(sent.headers["user-agent"], DESKTOP_UA);
        }
        assert_eq!(quote.symbol, "BTC");
        assert_eq!(quote.name, "BTC / USD");
        assert_eq!(quote.currency, "USD");
        assert_eq!(quote.asset_class, AssetClass::Crypto);
        assert_eq!(quote.price, Some(63_039.67));
        assert_eq!(quote.previous_close, Some(62_000.0));
        assert_eq!(quote.day_high, Some(63_500.0));
        assert_eq!(quote.day_low, Some(61_800.0));
        assert_eq!(quote.volume, Some(9_182.44));
        assert!((quote.change.unwrap() - 1_039.67).abs() < 1e-9);
        assert!((quote.change_percent.unwrap() - 1.676_887).abs() < 1e-4);
        assert_eq!(quote.venue, "Coinbase");
        assert_eq!(quote.source, "coinbase");
        // `2026-08-18T09:15:30Z`, truncated to whole seconds.
        assert_eq!(quote.time, 1_787_044_530);
    }

    #[tokio::test]
    async fn a_dead_stats_call_leaves_the_price_standing() {
        // `/stats` is the nice-to-have half; losing it must not lose the print.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/products/ETH-USD/ticker"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "price": "4210.55" })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path_matcher("/products/ETH-USD/stats"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let quote = get_quote(&state_for(&server), "ETH").await.unwrap();
        assert_eq!(quote.price, Some(4_210.55));
        // Absent, not zero: Coinbase did not publish a 24h open here.
        assert_eq!(quote.previous_close, None);
        assert_eq!(quote.change, None);
        assert_eq!(quote.change_percent, None);
        assert_eq!(quote.day_high, None);
        assert_eq!(quote.volume, None);
    }

    #[tokio::test]
    async fn names_the_pair_coinbase_does_not_list() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/products/DOGE-GBP/ticker"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path_matcher("/products/DOGE-GBP/stats"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let error = get_quote(&state_for(&server), "doge-gbp")
            .await
            .unwrap_err();
        assert_eq!(error.code, codes::NOT_FOUND);
        assert_eq!(error.message, "Coinbase does not list the pair DOGE-GBP");
        assert!(error.hint.unwrap().contains("DOGE-USDC"));
    }

    /// Serves 300-bucket pages of hourly candles ending at the requested `end`,
    /// newest first — the shape and the silent truncation Coinbase applies.
    struct Pager {
        oldest: i64,
    }

    impl Respond for Pager {
        fn respond(&self, request: &Request) -> ResponseTemplate {
            let end = request
                .url
                .query_pairs()
                .find(|(key, _)| key == "end")
                .map(|(_, value)| value.to_string())
                .and_then(|value| {
                    OffsetDateTime::parse(&value, &Rfc3339)
                        .ok()
                        .map(|moment| moment.unix_timestamp())
                })
                .unwrap_or_default();

            let mut rows: Vec<Value> = Vec::new();
            let mut time = end;
            while rows.len() < MAX_BUCKETS as usize && time >= self.oldest {
                rows.push(json!([time, 1.0, 3.0, 2.0, 2.5, 10.0]));
                time -= 3_600;
            }
            ResponseTemplate::new(200).set_body_json(Value::Array(rows))
        }
    }

    #[tokio::test]
    async fn walks_backwards_through_the_three_hundred_bucket_cap() {
        // 500 hourly buckets is more than one response can hold, and Coinbase
        // truncates the *older* end silently rather than paginating.
        let end = 1_786_892_400;
        let start = end - 500 * 3_600;

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/products/BTC-USD/candles"))
            .and(query_param("granularity", "3600"))
            .respond_with(Pager { oldest: start })
            .mount(&server)
            .await;

        let response = get_candles(
            &state_for(&server),
            "BTC",
            CandleInterval::OneHour,
            start,
            end,
        )
        .await
        .unwrap();

        assert_eq!(response.symbol, "BTC");
        assert_eq!(response.name, "BTC / USD");
        assert_eq!(response.interval, CandleInterval::OneHour);
        assert_eq!(response.source, "coinbase");
        // Every bucket in the window, once each, oldest first.
        assert_eq!(response.candles.len(), 501);
        assert_eq!(response.candles[0].time, start);
        assert_eq!(response.candles[500].time, end);
        assert!(response
            .candles
            .windows(2)
            .all(|pair| pair[1].time > pair[0].time));
        // Two requests: the second picks up where the first was truncated.
        assert_eq!(server.received_requests().await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn stops_after_the_page_limit_rather_than_fanning_out() {
        let end = 1_786_892_400;
        // Far more than 12 pages of 300 hourly buckets could cover.
        let start = end - 100_000 * 3_600;

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/products/BTC-USD/candles"))
            .respond_with(Pager { oldest: start })
            .mount(&server)
            .await;

        let response = get_candles(
            &state_for(&server),
            "BTC-USD",
            CandleInterval::OneHour,
            start,
            end,
        )
        .await
        .unwrap();

        assert_eq!(server.received_requests().await.unwrap().len(), MAX_PAGES);
        assert_eq!(response.candles.len(), MAX_PAGES * MAX_BUCKETS as usize);
    }

    #[tokio::test]
    async fn an_empty_page_ends_the_walk() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/products/BTC-USD/candles"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
            .expect(1)
            .mount(&server)
            .await;

        let response = get_candles(
            &state_for(&server),
            "BTC",
            CandleInterval::OneDay,
            1_786_892_400 - 900 * 86_400,
            1_786_892_400,
        )
        .await
        .unwrap();
        assert!(response.candles.is_empty());
    }

    #[tokio::test]
    async fn candles_ask_for_iso_timestamps_and_the_mapped_granularity() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/products/BTC-USD/candles"))
            .and(query_param("granularity", "86400"))
            .and(query_param("start", "2026-08-11T15:00:00.000Z"))
            .and(query_param("end", "2026-08-18T15:00:00.000Z"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
            .expect(1)
            .mount(&server)
            .await;

        get_candles(
            &state_for(&server),
            "BTC",
            CandleInterval::OneDay,
            1_786_460_400,
            1_787_065_200,
        )
        .await
        .unwrap();
    }

    /* ------------------------------------------------------------ search */

    fn catalogue() -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_json(json!([
            { "id": "BTC-USD", "base_currency": "BTC", "quote_currency": "USD", "display_name": "BTC/USD", "status": "online", "trading_disabled": false },
            { "id": "BTC-USDT", "base_currency": "BTC", "quote_currency": "USDT", "display_name": "BTC/USDT", "status": "online" },
            { "id": "ETH-USD", "base_currency": "ETH", "quote_currency": "USD", "display_name": "ETH/USD", "status": "online" },
            { "id": "BTCAUCTION-USD", "base_currency": "BTCAUCTION", "quote_currency": "USD", "display_name": "BTCAUCTION/USD", "status": "online" },
            { "id": "WBTC-USD", "base_currency": "WBTC", "quote_currency": "USD", "display_name": "Wrapped BTC/USD", "status": "online" },
            { "id": "OLD-USD", "base_currency": "OLD", "quote_currency": "USD", "display_name": "BTC legacy", "status": "delisted" },
            { "id": "HALT-USD", "base_currency": "HALT", "quote_currency": "USD", "display_name": "HALT/USD", "status": "online", "trading_disabled": true }
        ]))
    }

    #[tokio::test]
    async fn ranks_an_exact_base_above_a_prefix_above_a_substring() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/products"))
            .respond_with(catalogue())
            .mount(&server)
            .await;

        let results = search(&state_for(&server), "btc", 20).await.unwrap();
        assert_eq!(
            results.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(),
            ["BTC-USD", "BTCAUCTION-USD", "WBTC-USD"]
        );
    }

    #[tokio::test]
    async fn search_leaves_out_what_cannot_be_traded_in_usd() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/products"))
            .respond_with(catalogue())
            .mount(&server)
            .await;

        let state = state_for(&server);
        // Delisted, halted and non-USD pairs never reach the ranking.
        let listed = list_products(&state).await.unwrap();
        assert_eq!(
            listed.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(),
            ["BTC-USD", "ETH-USD", "BTCAUCTION-USD", "WBTC-USD"]
        );
        assert!(search(&state, "halt", 20).await.unwrap().is_empty());
        assert!(search(&state, "  ", 20).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn search_honours_the_limit() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/products"))
            .respond_with(catalogue())
            .mount(&server)
            .await;

        let results = search(&state_for(&server), "btc", 2).await.unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].id, "BTC-USD");
    }
}
