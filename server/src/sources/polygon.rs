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
        UpstreamError::not_configured("Polygon.io needs an API key, and none is set").with_hint(
            "Set POLYGON_API_KEY. Keys are free from https://polygon.io/dashboard/api-keys.",
        )
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
                        .header("Authorization", format!("Bearer {key}")),
                )
                .await
                .map_err(annotate)
        })
        .await
}

/// Say which of Polygon's three refusals this is.
///
/// A key Polygon does not recognise answers 401 — verified from this container,
/// which holds no key: `{"status":"ERROR","error":"Unknown API Key"}`. A free
/// key answers 403 for anything intraday or real-time, and 429 above five
/// requests a minute. None of the three is an outage, and all three are
/// indistinguishable from a broken symbol unless named. The 401 in particular
/// falls through the chain to Yahoo, so without this the operator is told about
/// Yahoo's rate limit and never that the key they set was rejected.
fn annotate(error: UpstreamError) -> UpstreamError {
    match error.status {
        Some(401) => UpstreamError::new(
            "Polygon.io rejected this API key",
            crate::error::codes::BAD_CREDENTIALS,
        )
        .with_status(401)
        .with_hint(
            "The key in POLYGON_API_KEY was sent and not accepted. Check it against \
             https://polygon.io/dashboard/api-keys — a revoked or mistyped key answers 401, \
             an out-of-plan request answers 403.",
        ),
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

/// The volume on the last bar this response actually charts.
///
/// Index aggregates carry `o`, `h`, `l`, `c` and `t` and no `v` at all —
/// Polygon publishes no turnover for `I:SPX` or `I:NDX`, which are half the
/// reason this provider exists. [`SpotCandle::volume`] is an `f64` and cannot
/// hold the difference between "nothing traded" and "nobody said", but
/// [`SpotQuote::volume`] is an `Option` and must: a quote that prints `0` for
/// the S&P 500's volume is stating a figure Polygon never published.
///
/// Read off the same bars [`normalise_aggregates`] keeps, so it is the last
/// *charted* bar rather than the last row in the payload.
#[must_use]
pub fn last_volume(raw: &AggregatesResponse) -> Option<f64> {
    raw.results
        .iter()
        .rev()
        .find(|bar| bar.t.is_some() && bar.c.is_some_and(f64::is_finite))
        .and_then(|bar| bar.v)
}

#[derive(Debug)]
pub struct PolygonCandles {
    pub candles: Vec<SpotCandle>,
    pub name: String,
    pub currency: String,
    /// Volume on the final bar, as Polygon stated it — `None` where it stated
    /// none. See [`last_volume`].
    pub last_volume: Option<f64>,
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
        return Err(UpstreamError::new(
            format!("Polygon.io returned no {span} bars for {ticker}"),
            "empty_upstream",
        )
        .with_hint(hint));
    }

    let details = describe(state, &ticker).await.ok();
    Ok(PolygonCandles {
        last_volume: last_volume(&bars),
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
        // Not `last.map(|c| c.volume)`: that turns an index's absent `v` into a
        // published zero. See `last_volume`.
        volume: loaded.last_volume,
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

#[cfg(test)]
mod tests {
    //! Polygon.io parser, wire and chain tests.
    //!
    //! This deployment holds no Polygon key, so the success payloads below are
    //! Polygon's own published response bodies for each endpoint rather than
    //! captures — each fixture says which it is. The one failure payload that
    //! *is* a capture is the 401, which is what api.polygon.io answers this
    //! container today and is the refusal an operator with a mistyped key
    //! actually meets.

    use super::*;
    use serde_json::json;
    use wiremock::matchers::{any, method, path as path_matcher, path_regex};
    use wiremock::{Mock, MockServer, Request, ResponseTemplate};

    use crate::config::Config;
    use crate::error::codes;
    use crate::sources::stocks;

    /// `/v2/aggs/ticker/AAPL/range/1/day/…`, two daily bars, exactly as
    /// Polygon publishes it as the sample response for the stocks custom-bars
    /// endpoint. Documented shape, not a capture — we hold no key.
    ///
    /// Kept for the two things only a real equity bar shows: `t` is the bar
    /// start in *milliseconds*, and `v` is present, which is what makes the
    /// index fixture's missing `v` mean something.
    const AGGS_AAPL: &str = include_str!("fixtures/polygon_aggs_aapl.json");

    /// `/v2/aggs/ticker/I:NDX/range/1/day/…`, two daily bars, verbatim from
    /// Polygon's published sample response for the *indices* custom-bars
    /// endpoint. Documented shape, not a capture.
    ///
    /// Kept because an index bar carries `o`, `h`, `l`, `c` and `t` and **no
    /// `v` at all**. Indices are half the reason this provider exists, and a
    /// quote that fills that absence with `0` publishes a figure Polygon never
    /// stated.
    const AGGS_NDX: &str = include_str!("fixtures/polygon_aggs_ndx.json");

    /// The answer to an aggregates range that contains no bars: a 200, an `OK`
    /// status, counts of zero, and **no `results` key at all**. Constructed to
    /// the documented envelope rather than captured.
    ///
    /// This is what a free key returns for every intraday request and what any
    /// key returns for a misspelt ticker, so it is the shape that decides
    /// whether an empty chart is reported or drawn.
    const NO_BARS: &str = include_str!("fixtures/polygon_no_bars.json");

    /// `/v3/reference/tickers/AAPL`, trimmed to the four fields this module
    /// reads plus the identity around them. Polygon's published sample for the
    /// ticker-overview endpoint; documented shape, not a capture.
    ///
    /// Kept for `"currency_name": "usd"` — lower case on the wire, upper case
    /// in the terminal.
    const TICKER_AAPL: &str = include_str!("fixtures/polygon_ticker_aapl.json");

    /// `/v3/reference/tickers?search=…`. The first row is Polygon's published
    /// sample row verbatim; the other two are the same documented shape for a
    /// crypto pair and for an index. Documented shape, not a capture.
    ///
    /// Kept for the three branches the search parser has: `market: "stocks"`
    /// with an exchange, `market: "crypto"` (which is the only thing that makes
    /// a result crypto), and a row with neither `name` nor `primary_exchange`.
    const TICKERS_SEARCH: &str = include_str!("fixtures/polygon_tickers_search.json");

    /// Captured live from `api.polygon.io` with curl on 2026-08-24, sending
    /// `Authorization: Bearer NOT_A_REAL_KEY`. Byte-for-byte as served, under a
    /// 401.
    ///
    /// This is the refusal a deployment with a mistyped or revoked
    /// `POLYGON_API_KEY` meets, and the only Polygon failure this container can
    /// observe first-hand.
    const UNKNOWN_KEY: &str = include_str!("fixtures/polygon_unknown_key.json");

    /// Polygon's `NOT_AUTHORIZED` body, served under a 403 when a key is valid
    /// but the plan does not cover the request. Constructed to the documented
    /// error shape — a free key is needed to provoke it and we hold none.
    const NOT_AUTHORIZED: &str = include_str!("fixtures/polygon_not_authorized.json");

    /// The Yahoo chart the price chain has always fallen back on, borrowed from
    /// the stocks module so the "nothing moved" tests below compare against the
    /// real other arm rather than a stand-in.
    const YAHOO_AAPL: &str = include_str!("fixtures/yahoo_aapl_chart.json");

    const KEY: &str = "polygon-test-key-9f3a";

    fn fixture(text: &str) -> serde_json::Value {
        serde_json::from_str(text).expect("the fixture parses")
    }

    /// A deployment that has a key, pointed at `server`.
    fn state_for(server: &MockServer) -> AppState {
        AppState::new(Config {
            polygon_api_base: server.uri(),
            polygon_api_key: Some(KEY.to_owned()),
            ..Config::default()
        })
    }

    /// A deployment that has none — this one, and most of them.
    fn state_without_key(server: &MockServer) -> AppState {
        AppState::new(Config {
            polygon_api_base: server.uri(),
            polygon_api_key: None,
            ..Config::default()
        })
    }

    async fn mount_aggs(server: &MockServer, body: &str) {
        Mock::given(method("GET"))
            .and(path_regex(r"^/v2/aggs/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(fixture(body)))
            .mount(server)
            .await;
    }

    async fn mount_details(server: &MockServer, body: serde_json::Value) {
        Mock::given(method("GET"))
            .and(path_regex(r"^/v3/reference/tickers/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(server)
            .await;
    }

    /// Every request the fixture server saw, as `(path, query)`.
    async fn seen(server: &MockServer) -> Vec<(String, String)> {
        server
            .received_requests()
            .await
            .expect("the recorder is on")
            .iter()
            .map(|request: &Request| {
                (
                    request.url.path().to_owned(),
                    request.url.query().unwrap_or_default().to_owned(),
                )
            })
            .collect()
    }

    /* ---------------------------------------------------- the credential gate */

    #[test]
    fn has_credentials_is_false_until_polygon_api_key_is_set() {
        // The single fact `.available(…)` in the stocks price chain reads. If
        // this ever answers true on a bare config, every deployment without a
        // key starts spending a round trip on a 401 before Yahoo is asked.
        assert!(!has_credentials(&AppState::new(Config::default())));
        assert!(has_credentials(&AppState::new(Config {
            polygon_api_key: Some(KEY.to_owned()),
            ..Config::default()
        })));
    }

    #[tokio::test]
    async fn without_a_key_no_request_is_made_and_the_answer_names_the_variable_to_set() {
        let server = MockServer::start().await;
        Mock::given(any())
            .respond_with(ResponseTemplate::new(200).set_body_json(fixture(AGGS_AAPL)))
            .expect(0)
            .mount(&server)
            .await;

        let err = get_candles(
            &state_without_key(&server),
            "AAPL",
            CandleInterval::OneDay,
            1_577_836_800,
            1_579_046_400,
            AssetClass::Stock,
        )
        .await
        .expect_err("no key is no answer");

        assert_eq!(err.code, codes::NOT_CONFIGURED);
        // Shown verbatim in the panel, so it has to carry the fix.
        let hint = err.hint.expect("a hint");
        assert!(hint.contains("POLYGON_API_KEY"), "{hint}");
        assert!(hint.contains("polygon.io/dashboard/api-keys"), "{hint}");
    }

    /* ------------------------------------------------------- the price chain */

    /// The window has to span both fixtures: `stocks::get_candles` drops bars
    /// outside it, and Yahoo's are 2026 while Polygon's published sample is
    /// 2020.
    const SPAN: (i64, i64) = (1_577_000_000, 1_787_000_000);

    async fn mount_yahoo_aapl(server: &MockServer) {
        Mock::given(method("GET"))
            .and(path_matcher("/v8/finance/chart/AAPL"))
            .respond_with(ResponseTemplate::new(200).set_body_json(fixture(YAHOO_AAPL)))
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn without_a_key_polygon_is_skipped_rather_than_asked() {
        // The claim `.available(polygon::has_credentials(state))` makes. The
        // Polygon host here would answer perfectly well and must still never be
        // reached: a skipped provider is not a failed one, so it costs neither
        // a round trip nor a place in the failure list.
        let polygon = MockServer::start().await;
        Mock::given(any())
            .respond_with(ResponseTemplate::new(200).set_body_json(fixture(AGGS_AAPL)))
            .expect(0)
            .mount(&polygon)
            .await;
        let yahoo = MockServer::start().await;
        mount_yahoo_aapl(&yahoo).await;

        let state = AppState::new(Config {
            polygon_api_base: polygon.uri(),
            polygon_api_key: None,
            yahoo_api_base: yahoo.uri(),
            nasdaq_api_base: yahoo.uri(),
            ..Config::default()
        });

        let answer = stocks::get_candles(&state, "AAPL", CandleInterval::OneDay, SPAN.0, SPAN.1)
            .await
            .expect("Yahoo answers, as it did before Polygon existed");

        assert_eq!(answer.source, "yahoo");
        assert_eq!(answer.candles.len(), 5);
        assert_eq!(answer.candles[0].time, 1_786_368_600);
        assert!(seen(&polygon).await.is_empty(), "Polygon was contacted");
    }

    #[tokio::test]
    async fn a_key_puts_polygon_at_the_head_of_the_price_chain() {
        // The other half: with a key it goes *first*, and Yahoo is not asked at
        // all — which is the point, since Yahoo 429s the datacentre addresses
        // this terminal is hosted on.
        let polygon = MockServer::start().await;
        mount_aggs(&polygon, AGGS_AAPL).await;
        mount_details(&polygon, fixture(TICKER_AAPL)).await;
        let yahoo = MockServer::start().await;
        mount_yahoo_aapl(&yahoo).await;

        let state = AppState::new(Config {
            polygon_api_base: polygon.uri(),
            polygon_api_key: Some(KEY.to_owned()),
            yahoo_api_base: yahoo.uri(),
            nasdaq_api_base: yahoo.uri(),
            ..Config::default()
        });

        let answer = stocks::get_candles(&state, "AAPL", CandleInterval::OneDay, SPAN.0, SPAN.1)
            .await
            .expect("Polygon answers");

        assert_eq!(answer.source, "polygon");
        assert_eq!(answer.name, "Apple Inc.");
        assert_eq!(answer.candles.len(), 2);
        assert_eq!(answer.candles[1].close, 74.3575);
        assert!(seen(&yahoo).await.is_empty(), "Yahoo was contacted anyway");
    }

    #[tokio::test]
    async fn without_a_key_the_chain_fails_exactly_as_it_did_before_polygon_existed() {
        // "Skipped, not failed" is only provable at the failure end: when the
        // rest of the chain refuses, the error a keyless deployment gets must
        // not depend on Polygon at all — not on what it would have answered,
        // and not on whether its host is even reachable.
        async fn refused(polygon_base: String) -> UpstreamError {
            let refusing = MockServer::start().await;
            Mock::given(any())
                .respond_with(ResponseTemplate::new(403))
                .mount(&refusing)
                .await;
            let state = AppState::new(Config {
                polygon_api_base: polygon_base,
                polygon_api_key: None,
                yahoo_api_base: refusing.uri(),
                nasdaq_api_base: refusing.uri(),
                ..Config::default()
            });
            stocks::get_candles(&state, "AAPL", CandleInterval::OneDay, SPAN.0, SPAN.1)
                .await
                .expect_err("both remaining providers refused")
        }

        let serving = MockServer::start().await;
        mount_aggs(&serving, AGGS_AAPL).await;
        mount_details(&serving, fixture(TICKER_AAPL)).await;

        // A Polygon host that would answer, and one that refuses the connection
        // outright.
        let with_polygon_up = refused(serving.uri()).await;
        let with_polygon_gone = refused("http://127.0.0.1:1".to_owned()).await;

        assert_eq!(with_polygon_up, with_polygon_gone);
        assert!(seen(&serving).await.is_empty(), "Polygon was contacted");

        // And the reader is never told to go and configure something.
        assert_ne!(with_polygon_up.code, codes::NOT_CONFIGURED);
        let described = format!(
            "{} {}",
            with_polygon_up.message,
            with_polygon_up.hint.clone().unwrap_or_default()
        );
        assert!(!described.contains("Polygon"), "{described}");
        assert!(!described.contains("POLYGON_API_KEY"), "{described}");
    }

    /* ----------------------------------------------------- refusals with a key */

    #[tokio::test]
    async fn a_rejected_key_is_a_credential_problem_with_the_fix_in_it() {
        // What api.polygon.io actually answers a key it does not know — the one
        // Polygon failure this container can reproduce. Left unannotated it is
        // `upstream_status` with no hint, the chain falls through to Yahoo, and
        // the operator is shown Yahoo's rate limit while the real fault is a
        // typo in POLYGON_API_KEY.
        let server = MockServer::start().await;
        Mock::given(any())
            .respond_with(ResponseTemplate::new(401).set_body_json(fixture(UNKNOWN_KEY)))
            .mount(&server)
            .await;

        let err = get_quote(&state_for(&server), "AAPL", AssetClass::Stock)
            .await
            .expect_err("a rejected key is not a quote");

        assert_eq!(err.code, codes::BAD_CREDENTIALS);
        assert_eq!(err.status, Some(401));
        let hint = err.hint.clone().expect("a hint");
        assert!(hint.contains("POLYGON_API_KEY"), "{hint}");
        assert!(hint.contains("polygon.io/dashboard/api-keys"), "{hint}");
        // Still not terminal: Yahoo and Nasdaq are still worth asking.
        assert!(!err.is_terminal());
    }

    #[tokio::test]
    async fn a_request_the_plan_does_not_cover_is_a_plan_limit_not_a_blocked_ip() {
        // The shared fetch hints "this host may be blocking your IP" for a 403,
        // which is right for the scraped feeds and exactly wrong for a keyed
        // API — it sends someone hunting a network fault they do not have.
        let server = MockServer::start().await;
        Mock::given(any())
            .respond_with(ResponseTemplate::new(403).set_body_json(fixture(NOT_AUTHORIZED)))
            .mount(&server)
            .await;

        let err = get_candles(
            &state_for(&server),
            "AAPL",
            CandleInterval::OneMinute,
            1_577_836_800,
            1_579_046_400,
            AssetClass::Stock,
        )
        .await
        .expect_err("out of plan is not a chart");

        assert_eq!(err.code, codes::NOT_CONFIGURED);
        assert_eq!(err.status, Some(403));
        let hint = err.hint.expect("a hint");
        assert!(hint.contains("end-of-day"), "{hint}");
        assert!(hint.contains("`1d`"), "{hint}");
        assert!(!hint.contains("blocking this IP"), "{hint}");
    }

    #[tokio::test]
    async fn rate_limiting_is_reported_as_rate_limiting_and_never_as_no_data() {
        // Five requests a minute is the free plan's whole budget, and a panel
        // that renders "no data" for it sends the reader looking for a wrong
        // symbol instead of waiting sixty seconds.
        let server = MockServer::start().await;
        Mock::given(any())
            .respond_with(ResponseTemplate::new(429))
            // One attempt, one retry: a 429 is transient, so the shared fetch
            // tries again before giving up.
            .expect(2)
            .mount(&server)
            .await;

        let err = search(&state_for(&server), "apple", 10)
            .await
            .expect_err("throttled is not empty");

        assert_eq!(err.code, codes::RATE_LIMITED);
        assert_eq!(err.status, Some(429));
        assert!(err.hint.expect("a hint").contains("five requests a minute"));
    }

    #[tokio::test]
    async fn an_outage_is_left_alone_because_it_is_not_a_plan_limit() {
        // `annotate` claims only Polygon's three refusals. A 500 is Polygon
        // being down, and dressing it as a credential or plan problem would
        // send the operator to the dashboard over an upstream outage.
        let server = MockServer::start().await;
        Mock::given(any())
            .respond_with(ResponseTemplate::new(500))
            .expect(2)
            .mount(&server)
            .await;

        let err = search(&state_for(&server), "apple", 10)
            .await
            .expect_err("a 500 is not a search result");

        assert_eq!(err.code, codes::UPSTREAM_STATUS);
        assert_eq!(err.status, Some(500));
        assert_ne!(err.code, codes::NOT_CONFIGURED);
        assert_ne!(err.code, codes::BAD_CREDENTIALS);
        assert_ne!(err.code, codes::RATE_LIMITED);
    }

    #[tokio::test]
    async fn the_key_travels_as_a_header_and_never_in_the_url_or_the_cache_key() {
        // Polygon also accepts `?apiKey=…`. Sending it that way would put a live
        // credential into the cache key, the tracing spans and any log line that
        // prints a URL.
        let server = MockServer::start().await;
        mount_aggs(&server, AGGS_AAPL).await;
        mount_details(&server, fixture(TICKER_AAPL)).await;
        let state = state_for(&server);

        get_candles(
            &state,
            "AAPL",
            CandleInterval::OneDay,
            1_577_836_800,
            1_579_046_400,
            AssetClass::Stock,
        )
        .await
        .expect("the bars load");

        let sent = server.received_requests().await.expect("requests");
        let aggs = sent
            .iter()
            .find(|request| request.url.path().starts_with("/v2/aggs/"))
            .expect("the aggregates request");
        assert_eq!(
            aggs.headers
                .get("authorization")
                .expect("an Authorization header"),
            &format!("Bearer {KEY}")
        );
        assert!(!aggs.url.as_str().contains(KEY), "{}", aggs.url);
        assert!(!aggs.url.as_str().contains("apiKey"), "{}", aggs.url);

        // The cache key is the path and query verbatim, so it holds no secret.
        assert!(state
            .cache()
            .get::<AggregatesResponse>(
                "polygon:/v2/aggs/ticker/AAPL/range/1/day/2020-01-01/2020-01-15\
                 ?adjusted=true&sort=asc&limit=50000"
            )
            .await
            .is_some());
    }

    /* --------------------------------------------------------------- symbols */

    #[test]
    fn an_index_is_named_by_polygons_root_and_not_by_yahoos_caret() {
        // A wrong guess here quotes a real but unrelated instrument rather than
        // failing, which is the worst way to be wrong. Every pair is stated.
        for (asked, expected) in [
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
        ] {
            assert_eq!(
                polygon_symbol(asked, AssetClass::Stock),
                expected,
                "{asked}"
            );
        }
        // The table is matched on the upper-cased symbol, so the caret form a
        // reader types at the prompt in lower case still lands on it.
        assert_eq!(polygon_symbol(" ^gspc ", AssetClass::Stock), "I:SPX");
        // Nasdaq's composite is `I:COMP`, not `I:IXIC` — the one mapping where
        // dropping the caret would have produced a plausible-looking miss.
        assert_ne!(polygon_symbol("^IXIC", AssetClass::Stock), "I:IXIC");
    }

    #[test]
    fn an_unmapped_caret_symbol_keeps_its_root_behind_an_i_prefix() {
        // A caret is Yahoo's mark for "this is an index", so the fallback is
        // Polygon's spelling of the same thing rather than a bare ticker —
        // `FTSE` on its own is a fund.
        assert_eq!(polygon_symbol("^FTSE", AssetClass::Stock), "I:FTSE");
        assert_eq!(polygon_symbol("^n225", AssetClass::Stock), "I:N225");
    }

    #[test]
    fn a_plain_equity_ticker_is_only_upper_cased() {
        assert_eq!(polygon_symbol("aapl", AssetClass::Stock), "AAPL");
        assert_eq!(polygon_symbol("  brk.b  ", AssetClass::Stock), "BRK.B");
    }

    #[test]
    fn a_crypto_pair_becomes_one_x_prefixed_word() {
        // The terminal writes `BTC-USD`; Polygon writes `X:BTCUSD`. Both
        // separators the rest of the terminal accepts have to survive.
        assert_eq!(polygon_symbol("BTC-USD", AssetClass::Crypto), "X:BTCUSD");
        assert_eq!(polygon_symbol("eth/usd", AssetClass::Crypto), "X:ETHUSD");
        // A bare base is quoted in dollars, which is what every crypto panel
        // here means by a bare base.
        assert_eq!(polygon_symbol("sol", AssetClass::Crypto), "X:SOLUSD");
        // Already in Polygon's own spelling: passed through, not double-prefixed
        // into `X:X:BTCUSD`.
        assert_eq!(polygon_symbol("x:btcusd", AssetClass::Crypto), "X:BTCUSD");
        // The index table is only consulted for equities, so a coin that shares
        // a name with an index is still a coin.
        assert_eq!(polygon_symbol("VIX-USD", AssetClass::Crypto), "X:VIXUSD");
    }

    /* ------------------------------------------------------ aggregate parsing */

    #[test]
    fn a_bar_timestamp_is_divided_by_a_thousand_and_not_offset() {
        // Polygon stamps a bar at its start in milliseconds; a SpotCandle is
        // unix seconds. Carrying the milliseconds through unchanged dates these
        // bars to the year 51,977 and the chart renders empty.
        let candles = normalise_aggregates(&serde_json::from_str(AGGS_AAPL).expect("parses"));
        assert_eq!(candles.len(), 2);
        assert_eq!(candles[0].time, 1_577_941_200);
        assert_eq!(candles[1].time, 1_578_027_600);

        // A bar start that is not a whole second floors rather than rounds up,
        // so a bar never claims to have opened before it did.
        let odd: AggregatesResponse =
            serde_json::from_value(json!({ "results": [{ "t": 1_578_027_600_999u64, "c": 1.0 }] }))
                .expect("parses");
        assert_eq!(normalise_aggregates(&odd)[0].time, 1_578_027_600);
    }

    #[test]
    fn reads_every_figure_of_a_bar_off_the_published_payload() {
        let candles = normalise_aggregates(&serde_json::from_str(AGGS_AAPL).expect("parses"));
        let first = candles[0];
        assert_eq!(first.open, 74.06);
        assert_eq!(first.high, 75.15);
        assert_eq!(first.low, 73.7975);
        assert_eq!(first.close, 75.0875);
        assert_eq!(first.volume, 135_647_456.0);
        assert_eq!(candles[1].close, 74.3575);
        assert_eq!(candles[1].volume, 146_535_512.0);
    }

    #[test]
    fn a_bar_with_no_usable_close_is_not_a_bar() {
        // A halted or not-yet-printed period comes back with `c` null and the
        // rest of the fields still there. Charting it would invent a price, and
        // dating one to the epoch would put a point in 1970.
        let raw: AggregatesResponse = serde_json::from_value(json!({ "results": [
            { "t": 1_578_027_600_000u64, "c": 74.3575 },
            // No close.
            { "t": 1_578_114_000_000u64, "c": null, "o": 74.0, "v": 1.0 },
            // No timestamp.
            { "c": 75.0 },
            // Neither.
            { "v": 0.0 }
        ] }))
        .expect("parses");
        let candles = normalise_aggregates(&raw);
        assert_eq!(candles.len(), 1);
        assert_eq!(candles[0].time, 1_578_027_600);

        // And a close that is not a finite number is not a close either.
        let infinite = AggregatesResponse {
            results: vec![Aggregate {
                t: Some(1_578_027_600_000.0),
                c: Some(f64::INFINITY),
                ..Aggregate::default()
            }],
        };
        assert!(normalise_aggregates(&infinite).is_empty());
    }

    #[test]
    fn a_missing_open_high_or_low_is_backfilled_from_the_bar_it_does_have() {
        // Index bars and thin OTC bars arrive short of fields. The backfill
        // keeps `low <= close <= high` true, which every renderer assumes.
        let raw: AggregatesResponse = serde_json::from_value(json!({ "results": [
            { "t": 1_000_000u64, "c": 10.0 },
            { "t": 2_000_000u64, "c": 10.0, "o": 8.0 },
            { "t": 3_000_000u64, "c": 8.0, "o": 10.0 }
        ] }))
        .expect("parses");
        let candles = normalise_aggregates(&raw);

        assert_eq!(
            (candles[0].open, candles[0].high, candles[0].low),
            (10.0, 10.0, 10.0)
        );
        assert_eq!(
            (candles[1].open, candles[1].high, candles[1].low),
            (8.0, 10.0, 8.0)
        );
        assert_eq!(
            (candles[2].open, candles[2].high, candles[2].low),
            (10.0, 10.0, 8.0)
        );
    }

    #[tokio::test]
    async fn asks_for_ascending_adjusted_bars_because_the_quote_reads_the_last_two() {
        // Nothing re-sorts what comes back, and the quote takes its price from
        // the last candle and its previous close from the one before. A
        // descending answer would print every day-change backwards.
        let server = MockServer::start().await;
        mount_aggs(&server, AGGS_AAPL).await;
        mount_details(&server, fixture(TICKER_AAPL)).await;

        get_candles(
            &state_for(&server),
            "^gspc",
            CandleInterval::OneDay,
            1_577_836_800,
            1_579_046_400,
            AssetClass::Stock,
        )
        .await
        .expect("the bars load");

        let (path, query) = seen(&server)
            .await
            .into_iter()
            .find(|(path, _)| path.starts_with("/v2/aggs/"))
            .expect("the aggregates request");
        // The colon in `I:SPX` is escaped, so it stays one path segment.
        assert_eq!(
            path,
            "/v2/aggs/ticker/I%3ASPX/range/1/day/2020-01-01/2020-01-15"
        );
        assert_eq!(query, "adjusted=true&sort=asc&limit=50000");
    }

    #[tokio::test]
    async fn each_interval_asks_for_its_own_multiplier_and_timespan() {
        // A `1m` request answered with daily bars would silently chart a
        // different question, so the period is part of the path rather than a
        // filter applied afterwards.
        for (interval, expected) in [
            (CandleInterval::OneMinute, "1/minute"),
            (CandleInterval::OneHour, "1/hour"),
            (CandleInterval::OneDay, "1/day"),
        ] {
            let server = MockServer::start().await;
            mount_aggs(&server, AGGS_AAPL).await;
            mount_details(&server, fixture(TICKER_AAPL)).await;

            get_candles(
                &state_for(&server),
                "AAPL",
                interval,
                1_577_836_800,
                1_579_046_400,
                AssetClass::Stock,
            )
            .await
            .expect("the bars load");

            let (path, _) = seen(&server)
                .await
                .into_iter()
                .find(|(path, _)| path.starts_with("/v2/aggs/"))
                .expect("the aggregates request");
            assert_eq!(
                path,
                format!("/v2/aggs/ticker/AAPL/range/{expected}/2020-01-01/2020-01-15")
            );
        }
    }

    /* ----------------------------------------------------------- no bars back */

    #[tokio::test]
    async fn a_range_with_no_bars_is_an_error_and_not_a_chart_with_no_points() {
        // HTTP 200, `"status": "OK"`, and no `results` key at all. Treating that
        // as a series would draw an empty pane over a misspelt ticker and say
        // nothing about it.
        let server = MockServer::start().await;
        mount_aggs(&server, NO_BARS).await;

        let err = get_candles(
            &state_for(&server),
            "AAPLL",
            CandleInterval::OneDay,
            1_577_836_800,
            1_579_046_400,
            AssetClass::Stock,
        )
        .await
        .expect_err("no bars is not a chart");

        assert_eq!(err.code, codes::EMPTY_UPSTREAM);
        assert_eq!(err.message, "Polygon.io returned no day bars for AAPLL");
        assert!(err.hint.expect("a hint").contains("I:SPX"));
    }

    #[tokio::test]
    async fn an_intraday_request_on_a_free_key_says_so_rather_than_drawing_nothing() {
        // The caveat the module header exists to state: a free key serves
        // end-of-day only, so `1m` comes back as a well-formed 200 with nothing
        // in it. Unnamed, that is indistinguishable from a dead symbol.
        let server = MockServer::start().await;
        mount_aggs(&server, NO_BARS).await;
        let state = state_for(&server);

        for (interval, span) in [
            (CandleInterval::OneMinute, "minute"),
            (CandleInterval::OneHour, "hour"),
        ] {
            let err = get_candles(
                &state,
                "AAPL",
                interval,
                1_577_836_800,
                1_579_046_400,
                AssetClass::Stock,
            )
            .await
            .expect_err("no bars is not a chart");

            assert_eq!(
                err.message,
                format!("Polygon.io returned no {span} bars for AAPL")
            );
            let hint = err.hint.expect("a hint");
            assert!(hint.contains("no intraday history"), "{hint}");
            assert!(hint.contains("`1d`"), "{hint}");
        }
    }

    #[tokio::test]
    async fn a_payload_whose_every_bar_is_unusable_counts_as_no_bars() {
        // `results` present and non-empty, and not one row survives parsing.
        // The count that decides is the charted one, not the served one.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_regex(r"^/v2/aggs/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "status": "OK",
                "resultsCount": 2,
                "results": [
                    { "t": 1_578_027_600_000u64, "c": null },
                    { "o": 74.0, "h": 75.0, "l": 73.0, "v": 1.0 }
                ]
            })))
            .mount(&server)
            .await;

        let err = get_candles(
            &state_for(&server),
            "AAPL",
            CandleInterval::OneDay,
            1_577_836_800,
            1_579_046_400,
            AssetClass::Stock,
        )
        .await
        .expect_err("no usable bars is not a chart");
        assert_eq!(err.code, codes::EMPTY_UPSTREAM);
    }

    #[tokio::test]
    async fn a_body_that_is_not_json_is_a_parse_failure_and_not_an_empty_chart() {
        // Polygon's edge serves an HTML maintenance page under a 200. Reading
        // that as "no bars" would blame the symbol.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_regex(r"^/v2/aggs/"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string("<html><body>Service Unavailable</body></html>"),
            )
            .mount(&server)
            .await;

        let err = get_candles(
            &state_for(&server),
            "AAPL",
            CandleInterval::OneDay,
            1_577_836_800,
            1_579_046_400,
            AssetClass::Stock,
        )
        .await
        .expect_err("HTML is not bars");
        assert_eq!(err.code, codes::BAD_UPSTREAM_BODY);
        assert_ne!(err.code, codes::EMPTY_UPSTREAM);
    }

    /* ----------------------------------------------------------------- quotes */

    #[tokio::test]
    async fn a_quote_is_the_last_two_daily_bars_and_the_day_figures_are_the_last_one() {
        let server = MockServer::start().await;
        mount_aggs(&server, AGGS_AAPL).await;
        mount_details(&server, fixture(TICKER_AAPL)).await;

        let quote = get_quote(&state_for(&server), "aapl", AssetClass::Stock)
            .await
            .expect("the quote resolves");

        assert_eq!(quote.symbol, "AAPL");
        assert_eq!(quote.source, "polygon");
        assert_eq!(quote.price, Some(74.3575));
        assert_eq!(quote.previous_close, Some(75.0875));
        assert!((quote.change.expect("a change") + 0.73).abs() < 1e-9);
        assert!((quote.change_percent.expect("a percent") + 0.972_199_1).abs() < 1e-6);
        // The last bar is the current session. The *first* bar opens the
        // requested window, which is only the same thing over a one-day range.
        assert_eq!(quote.day_open, Some(74.2875));
        assert_ne!(quote.day_open, Some(74.06));
        assert_eq!(quote.day_high, Some(75.145));
        assert_eq!(quote.day_low, Some(74.125));
        assert_eq!(quote.volume, Some(146_535_512.0));
        assert_eq!(quote.time, 1_578_027_600);
        // Read off the reference row, and `usd` on the wire is `USD` here.
        assert_eq!(quote.name, "Apple Inc.");
        assert_eq!(quote.currency, "USD");
        assert_eq!(quote.venue, "XNAS");
    }

    #[tokio::test]
    async fn an_index_quote_carries_no_volume_because_polygon_publishes_none() {
        // Index aggregates have no `v` field at all. `SpotCandle::volume` is an
        // f64 and has to floor at zero; the quote's is an Option and must stay
        // empty, or the panel prints "Vol 0" for the S&P 500 — a figure nobody
        // published, next to a price that is real.
        let server = MockServer::start().await;
        mount_aggs(&server, AGGS_NDX).await;
        mount_details(&server, json!({ "status": "OK", "request_id": "0cf72b" })).await;

        let quote = get_quote(&state_for(&server), "^NDX", AssetClass::Stock)
            .await
            .expect("the quote resolves");

        assert_eq!(quote.volume, None);
        assert_ne!(quote.volume, Some(0.0));
        // Everything the index *does* publish is still there.
        assert_eq!(quote.price, Some(11_830.281_788_083_06));
        assert_eq!(quote.previous_close, Some(11_995.882_359_986_66));
        assert_eq!(quote.day_open, Some(12_001.695_525_839_21));
        assert_eq!(quote.day_high, Some(12_069.622_620_335_57));
        assert_eq!(quote.day_low, Some(11_789.859_234_493_93));
        assert_eq!(quote.time, 1_678_428_000);
    }

    #[tokio::test]
    async fn one_bar_is_a_price_with_no_change_rather_than_a_change_of_zero() {
        // A newly listed ticker, or a range one session long. "Unchanged" is a
        // fact about the market; "we have one bar" is not, and the two must not
        // render the same.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_regex(r"^/v2/aggs/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "status": "OK",
                "results": [
                    { "t": 1_578_027_600_000u64, "o": 74.2875, "h": 75.145,
                      "l": 74.125, "c": 74.3575, "v": 146_535_512 }
                ]
            })))
            .mount(&server)
            .await;
        mount_details(&server, fixture(TICKER_AAPL)).await;

        let quote = get_quote(&state_for(&server), "AAPL", AssetClass::Stock)
            .await
            .expect("the quote resolves");

        assert_eq!(quote.price, Some(74.3575));
        assert_eq!(quote.previous_close, None);
        assert_eq!(quote.change, None);
        assert_eq!(quote.change_percent, None);
        assert_ne!(quote.change, Some(0.0));
    }

    #[tokio::test]
    async fn a_previous_close_of_zero_does_not_become_an_infinite_percentage() {
        // Polygon serves zero-priced bars for suspended and delisted OTC lines.
        // Dividing by one prints `inf%`, which serialises to JSON `null` in
        // some encoders and to a broken axis in the rest.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_regex(r"^/v2/aggs/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "status": "OK",
                "results": [
                    { "t": 1_577_941_200_000u64, "c": 0.0, "v": 0 },
                    { "t": 1_578_027_600_000u64, "c": 0.35, "v": 1200 }
                ]
            })))
            .mount(&server)
            .await;
        mount_details(&server, fixture(TICKER_AAPL)).await;

        let quote = get_quote(&state_for(&server), "ZZZZ", AssetClass::Stock)
            .await
            .expect("the quote resolves");

        assert_eq!(quote.previous_close, Some(0.0));
        assert_eq!(quote.change, Some(0.35));
        assert_eq!(quote.change_percent, None);
    }

    #[tokio::test]
    async fn a_quote_still_renders_when_the_reference_lookup_fails() {
        // The bars are the answer; the name and the currency are decoration.
        // Losing the decoration must not lose the price.
        let server = MockServer::start().await;
        mount_aggs(&server, AGGS_AAPL).await;
        Mock::given(method("GET"))
            .and(path_regex(r"^/v3/reference/tickers/"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let quote = get_quote(&state_for(&server), "aapl", AssetClass::Stock)
            .await
            .expect("a quote without a description is still a quote");

        assert_eq!(quote.price, Some(74.3575));
        assert_eq!(quote.name, "AAPL");
        assert_eq!(quote.currency, "USD");
        assert_eq!(quote.venue, "Polygon");
    }

    #[tokio::test]
    async fn a_reference_row_with_no_results_still_describes_something() {
        // `results` is an object, not an array, and Polygon omits it entirely
        // for tickers it has no reference row for. An empty name would render a
        // blank chart title beside a real price.
        let server = MockServer::start().await;
        mount_aggs(&server, AGGS_NDX).await;
        mount_details(&server, json!({ "status": "OK", "request_id": "0cf72b" })).await;

        let loaded = get_candles(
            &state_for(&server),
            "^NDX",
            CandleInterval::OneDay,
            1_678_000_000,
            1_679_000_000,
            AssetClass::Stock,
        )
        .await
        .expect("the bars load");

        assert_eq!(loaded.name, "I:NDX");
        assert_eq!(loaded.currency, "USD");
    }

    #[tokio::test]
    async fn venue_falls_back_from_the_exchange_to_the_market_and_then_to_polygon() {
        // Indices and crypto pairs carry `market` and no `primary_exchange`.
        // "Polygon" is the last resort rather than an empty venue chip.
        for (results, expected) in [
            (json!({ "market": "crypto", "name": "Bitcoin" }), "crypto"),
            (json!({ "name": "Something" }), "Polygon"),
        ] {
            let server = MockServer::start().await;
            mount_aggs(&server, AGGS_AAPL).await;
            mount_details(&server, json!({ "status": "OK", "results": results })).await;

            let quote = get_quote(&state_for(&server), "AAPL", AssetClass::Stock)
                .await
                .expect("the quote resolves");
            assert_eq!(quote.venue, expected);
        }
    }

    /* ----------------------------------------------------------------- search */

    #[tokio::test]
    async fn search_reads_tickers_names_and_venues_off_the_published_payload() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/v3/reference/tickers"))
            .respond_with(ResponseTemplate::new(200).set_body_json(fixture(TICKERS_SEARCH)))
            .mount(&server)
            .await;

        let results = search(&state_for(&server), "a", 10)
            .await
            .expect("Polygon answers");

        assert_eq!(results.len(), 3);
        // Polygon's own order is the relevance order; nothing re-sorts it.
        assert_eq!(results[0].symbol, "A");
        assert_eq!(results[0].name, "Agilent Technologies Inc.");
        assert_eq!(results[0].asset_class, AssetClass::Stock);
        assert_eq!(results[0].venue, "XNYS");
        // `market` is the only thing that makes a result crypto — the `X:`
        // prefix on the ticker is not read.
        assert_eq!(results[1].symbol, "X:BTCUSD");
        assert_eq!(results[1].asset_class, AssetClass::Crypto);
        assert_eq!(results[1].venue, "crypto");
        // No name and no exchange: the ticker stands in for the name, the
        // market for the venue.
        assert_eq!(results[2].symbol, "I:SPX");
        assert_eq!(results[2].name, "I:SPX");
        assert_eq!(results[2].venue, "indices");
        // Nothing here knows about Kalshi ladders.
        assert!(results.iter().all(|result| !result.has_implied));
    }

    #[tokio::test]
    async fn a_search_that_matches_nothing_is_an_empty_list_and_not_a_failure() {
        // Both shapes Polygon uses for "nothing matched": an explicit empty
        // array, and the envelope with no `results` key at all. A search board
        // that errored on either would report an outage over a typo.
        for body in [
            json!({ "status": "OK", "count": 0, "results": [] }),
            json!({ "status": "OK", "count": 0, "request_id": "e70013" }),
        ] {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path_matcher("/v3/reference/tickers"))
                .respond_with(ResponseTemplate::new(200).set_body_json(body))
                .mount(&server)
                .await;

            let results = search(&state_for(&server), "zzzzzzz", 10)
                .await
                .expect("nothing matched is still an answer");
            assert!(results.is_empty());
        }
    }

    #[tokio::test]
    async fn a_row_with_no_ticker_is_dropped_rather_than_given_an_empty_symbol() {
        // A search row without a symbol is a row nothing can be opened from; it
        // would sit in the board as a blank line that does nothing when clicked.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/v3/reference/tickers"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "status": "OK",
                "results": [
                    { "name": "No ticker at all", "market": "stocks" },
                    { "ticker": "AAPL", "name": "Apple Inc.", "market": "stocks",
                      "primary_exchange": "XNAS" }
                ]
            })))
            .mount(&server)
            .await;

        let results = search(&state_for(&server), "apple", 10)
            .await
            .expect("Polygon answers");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].symbol, "AAPL");
    }

    #[tokio::test]
    async fn a_search_limit_is_clamped_to_what_polygon_will_serve() {
        // Polygon refuses a limit above 100 outright rather than truncating, so
        // a board asking for more would get nothing instead of a hundred rows.
        for (asked, sent) in [(0usize, "1"), (5, "5"), (100, "100"), (5_000, "100")] {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path_matcher("/v3/reference/tickers"))
                .respond_with(ResponseTemplate::new(200).set_body_json(fixture(TICKERS_SEARCH)))
                .mount(&server)
                .await;

            search(&state_for(&server), "a", asked)
                .await
                .expect("Polygon answers");

            let (_, query) = seen(&server).await.remove(0);
            assert_eq!(
                query,
                format!("search=a&active=true&limit={sent}"),
                "{asked}"
            );
        }
    }

    #[tokio::test]
    async fn a_query_with_an_ampersand_in_it_stays_one_query() {
        // `S&P 500` unescaped ends the `search` value at the ampersand and adds
        // a parameter called `P 500`. Polygon then searches for `S`, and the
        // board fills with unrelated matches instead of failing.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/v3/reference/tickers"))
            .respond_with(ResponseTemplate::new(200).set_body_json(fixture(TICKERS_SEARCH)))
            .mount(&server)
            .await;

        search(&state_for(&server), "S&P 500", 5)
            .await
            .expect("Polygon answers");

        let sent = server.received_requests().await.expect("one request");
        let pairs: Vec<(String, String)> = sent[0]
            .url
            .query_pairs()
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect();
        assert_eq!(
            pairs,
            [
                ("search".to_owned(), "S&P 500".to_owned()),
                ("active".to_owned(), "true".to_owned()),
                ("limit".to_owned(), "5".to_owned()),
            ]
        );
    }

    /* ------------------------------------------------------------------ cache */

    #[tokio::test]
    async fn the_same_bars_are_not_fetched_twice_inside_the_ttl() {
        // Five requests a minute is the free plan's entire budget, so a panel
        // that re-asks per redraw exhausts it in seconds.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_regex(r"^/v2/aggs/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(fixture(AGGS_AAPL)))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path_regex(r"^/v3/reference/tickers/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(fixture(TICKER_AAPL)))
            .expect(1)
            .mount(&server)
            .await;

        let state = state_for(&server);
        for _ in 0..3 {
            get_candles(
                &state,
                "AAPL",
                CandleInterval::OneDay,
                1_577_836_800,
                1_579_046_400,
                AssetClass::Stock,
            )
            .await
            .expect("the bars load");
        }
    }
}
