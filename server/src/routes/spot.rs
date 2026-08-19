//! Spot price routes — the true price half of the implied-vs-actual chart.
//!
//! `{class}` picks the provider family rather than describing the instrument, so
//! the same two routes serve equities, ETFs, cash indices and crypto pairs.

use axum::extract::{Path, RawQuery, State};
use axum::routing::get;
use axum::{Json, Router};
use terminal_core::types::{
    AssetClass, CandleInterval, SpotCandlesResponse, SpotQuote, SpotSearchResponse,
    SpotSearchResult,
};

use crate::app::AppState;
use crate::error::{Result, UpstreamError};
use crate::routes::helpers::{candle_window, now_seconds, snap_to_grid, QueryParams};
use crate::sources::{crypto, implied, stocks};

/// `/search` is declared before `/{class}/{symbol}`.
///
/// `axum` prefers a static segment over a capture whatever the order, so this
/// is belt-and-braces — but the failure it guards against is silent (`search`
/// resolving as an asset class) rather than loud, so the order is stated here
/// and asserted in the tests below.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/spot/search", get(search))
        .route("/api/spot/{class}/{symbol}", get(quote))
        .route("/api/spot/{class}/{symbol}/candles", get(candles))
}

/// Read the `{class}` segment.
///
/// The synonyms are deliberately generous: the cost of accepting `stocks` for
/// `stock` is nil, and the cost of rejecting it is a command retyped.
fn asset_class(raw: &str) -> Result<AssetClass> {
    match raw.to_lowercase().as_str() {
        "stock" | "stocks" | "equity" => Ok(AssetClass::Stock),
        "crypto" | "coin" => Ok(AssetClass::Crypto),
        _ => Err(
            UpstreamError::bad_request(format!("Unknown asset class \"{raw}\""))
                .with_hint("Use `stock` (equities, ETFs, indices) or `crypto`."),
        ),
    }
}

/// Expand a registry alias before hitting an upstream.
///
/// Somebody types `SPX`; Yahoo knows it as `^GSPC`. Resolving here means the
/// alias table is stated once, next to the Kalshi ladders it also names.
fn resolve_symbol(symbol: &str, class: AssetClass) -> String {
    match implied::find_underlying(symbol) {
        Some(underlying) if underlying.asset_class == class => underlying.spot_symbol.to_string(),
        _ => symbol.to_string(),
    }
}

/// The candle window, floored onto a 15-second grid.
///
/// A panel polling every 15s sends a fresh `end` each time, so an unsnapped
/// window differs only in its last second — a new cache key on every poll, and
/// `ttl::CANDLES` never fires. Nothing is lost: no bucket is shorter than a
/// minute.
///
/// The emptiness check runs *after* the snap, because a sub-grid window that
/// was legal before it survives as `start == end` afterwards.
fn snapped_window(params: &QueryParams, interval: CandleInterval, now: i64) -> Result<(i64, i64)> {
    let (start_ts, end_ts) = candle_window(params, interval, snap_to_grid(now))?;
    let (start_ts, end_ts) = (snap_to_grid(start_ts), snap_to_grid(end_ts));

    if start_ts >= end_ts {
        return Err(UpstreamError::bad_request("`start` must be before `end`"));
    }
    Ok((start_ts, end_ts))
}

/// `GET /api/spot/search?q&limit&class`
///
/// Both providers are asked at once and either is allowed to fail: a Coinbase
/// outage should cost the coin rows, not the whole search. `Promise.allSettled`
/// in prose — an empty list from the failed side, not a failed request.
async fn search(
    State(state): State<AppState>,
    RawQuery(raw): RawQuery,
) -> Result<Json<SpotSearchResponse>> {
    let params = QueryParams::parse(raw.as_deref().unwrap_or_default());

    let query = params.string("q");
    if query.is_empty() {
        return Err(
            UpstreamError::bad_request("Search needs a symbol or a name")
                .with_hint("Usage: `SSRCH <words>`, e.g. `SSRCH apple`."),
        );
    }
    let limit = params.int_param("limit", 20, 1, 50) as usize;
    let wanted = match params.get("class") {
        Some(raw) => Some(asset_class(raw)?),
        None => None,
    };

    let equities = async {
        if wanted == Some(AssetClass::Crypto) {
            Vec::new()
        } else {
            stocks::search(&state, &query, limit)
                .await
                .unwrap_or_default()
        }
    };
    let coins = async {
        if wanted == Some(AssetClass::Stock) {
            Vec::new()
        } else {
            crypto::search(&state, &query, limit)
                .await
                .unwrap_or_default()
        }
    };
    let (equities, coins) = futures::future::join(equities, coins).await;

    let mut results: Vec<SpotSearchResult> = coins
        .iter()
        .map(|product| SpotSearchResult {
            symbol: product
                .base_currency
                .clone()
                .or_else(|| product.id.split('-').next().map(str::to_string))
                .unwrap_or_else(|| product.id.clone()),
            name: product
                .display_name
                .clone()
                .unwrap_or_else(|| product.id.clone()),
            asset_class: AssetClass::Crypto,
            venue: "Coinbase".to_string(),
            has_implied: false,
        })
        .collect();
    results.extend(equities);

    // Flag the rows that can carry an implied overlay, so the search result
    // answers "and can I chart the market's forecast of it?" in one pass.
    for result in &mut results {
        result.has_implied = implied::find_underlying(&result.symbol).is_some();
    }
    results.truncate(limit);

    Ok(Json(SpotSearchResponse { query, results }))
}

/// `GET /api/spot/{class}/{symbol}`
async fn quote(
    State(state): State<AppState>,
    Path((class, symbol)): Path<(String, String)>,
) -> Result<Json<SpotQuote>> {
    let class = asset_class(&class)?;
    let symbol = resolve_symbol(&symbol, class);

    Ok(Json(match class {
        AssetClass::Crypto => crypto::get_quote(&state, &symbol).await?,
        AssetClass::Stock => stocks::get_quote(&state, &symbol).await?,
    }))
}

/// `GET /api/spot/{class}/{symbol}/candles?interval&start&end`
async fn candles(
    State(state): State<AppState>,
    Path((class, symbol)): Path<(String, String)>,
    RawQuery(raw): RawQuery,
) -> Result<Json<SpotCandlesResponse>> {
    let class = asset_class(&class)?;
    let symbol = resolve_symbol(&symbol, class);
    let params = QueryParams::parse(raw.as_deref().unwrap_or_default());

    let interval = params.interval()?;
    let (start_ts, end_ts) = snapped_window(&params, interval, now_seconds())?;

    Ok(Json(match class {
        AssetClass::Crypto => {
            crypto::get_candles(&state, &symbol, interval, start_ts, end_ts).await?
        }
        AssetClass::Stock => {
            stocks::get_candles(&state, &symbol, interval, start_ts, end_ts).await?
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    use axum::body::to_bytes;
    use axum::http::{Request, StatusCode};
    use serde_json::{json, Value};
    use tower::ServiceExt;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use crate::config::Config;
    use crate::error::codes;

    /// Everything a spot request can reach: Yahoo and Nasdaq for equities,
    /// Coinbase for coins.
    struct Spot {
        yahoo: MockServer,
        nasdaq: MockServer,
        coinbase: MockServer,
    }

    impl Spot {
        async fn start() -> Self {
            Self {
                yahoo: MockServer::start().await,
                nasdaq: MockServer::start().await,
                coinbase: MockServer::start().await,
            }
        }

        fn config(&self) -> Config {
            Config {
                yahoo_api_base: self.yahoo.uri(),
                nasdaq_api_base: self.nasdaq.uri(),
                coinbase_api_base: self.coinbase.uri(),
                ..Config::default()
            }
        }
    }

    /// The smallest Yahoo chart payload that parses into one bar and a quote.
    fn yahoo_chart(symbol: &str) -> Value {
        json!({ "chart": { "result": [{
            "meta": {
                "symbol": symbol,
                "currency": "USD",
                "longName": "Fixture Inc.",
                "regularMarketPrice": 305.93,
                "previousClose": 305.26,
            },
            "timestamp": [1_786_455_000],
            "indicators": { "quote": [{
                "open": [306.0], "high": [307.5], "low": [304.3],
                "close": [305.93], "volume": [28_229_611.0],
            }] },
        }] } })
    }

    async fn mount(server: &MockServer, at: &str, body: Value) {
        Mock::given(method("GET"))
            .and(path(at))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(server)
            .await;
    }

    async fn get_json(config: Config, uri: &str) -> (StatusCode, Value) {
        let app = crate::build_router(AppState::new(config));
        let response = app
            .oneshot(Request::get(uri).body(axum::body::Body::empty()).unwrap())
            .await
            .unwrap();

        let status = response.status();
        let bytes = to_bytes(response.into_body(), 4 * 1024 * 1024)
            .await
            .unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    async fn get_offline(uri: &str) -> (StatusCode, Value) {
        get_json(Config::default(), uri).await
    }

    /* ------------------------------------------------------- the class segment */

    #[test]
    fn reads_every_synonym_for_the_two_provider_families() {
        for raw in ["stock", "stocks", "equity", "STOCK", "Equity"] {
            assert_eq!(asset_class(raw).unwrap(), AssetClass::Stock, "{raw}");
        }
        for raw in ["crypto", "coin", "CRYPTO", "Coin"] {
            assert_eq!(asset_class(raw).unwrap(), AssetClass::Crypto, "{raw}");
        }
    }

    #[tokio::test]
    async fn an_unknown_asset_class_is_a_bad_request_naming_the_two() {
        for uri in [
            "/api/spot/bond/TLT",
            "/api/spot/bond/TLT/candles",
            "/api/spot/search?q=apple&class=bond",
        ] {
            let (status, body) = get_offline(uri).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{uri}");
            assert_eq!(body["code"], codes::BAD_REQUEST, "{uri}");
            assert!(
                body["error"].as_str().unwrap().contains("bond"),
                "{}",
                body["error"]
            );
            assert!(body["hint"].as_str().unwrap().contains("crypto"), "{uri}");
        }
    }

    /* --------------------------------------------------------- route ordering */

    #[tokio::test]
    async fn search_resolves_as_search_and_never_as_a_symbol() {
        // A regression in the route table would send `/api/spot/search` into
        // `/{class}/{symbol}` — where `search` is not an asset class — and the
        // panel would report a bad class for a command that has none.
        let spot = Spot::start().await;
        mount(&spot.coinbase, "/products", json!([])).await;
        Mock::given(method("GET"))
            .and(path("/v1/finance/search"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "quotes": [] })))
            .mount(&spot.yahoo)
            .await;

        let (status, body) = get_json(spot.config(), "/api/spot/search?q=apple").await;

        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["query"], "apple");
        assert!(body["results"].is_array());
        // Not the asset-class refusal the misrouted form would produce.
        assert!(body.get("error").is_none(), "{body}");
    }

    #[test]
    fn the_static_search_route_is_declared_before_the_capture() {
        // Stated in the router and asserted here, because `axum` resolving it
        // correctly by preference does not stop a later edit from relying on
        // that and a third route from breaking the assumption.
        let source = include_str!("spot.rs");
        let search_at = source.find("\"/api/spot/search\"").expect("search route");
        let capture_at = source
            .find("\"/api/spot/{class}/{symbol}\"")
            .expect("symbol route");
        assert!(search_at < capture_at);
    }

    /* ---------------------------------------------------------------- search */

    #[tokio::test]
    async fn a_search_with_no_words_says_so() {
        for uri in ["/api/spot/search", "/api/spot/search?q=%20"] {
            let (status, body) = get_offline(uri).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{uri}");
            assert_eq!(body["error"], "Search needs a symbol or a name", "{uri}");
            assert!(body["hint"].as_str().unwrap().contains("SSRCH"), "{uri}");
        }
    }

    #[tokio::test]
    async fn search_asks_both_families_and_lists_the_coins_first() {
        let spot = Spot::start().await;
        mount(
            &spot.coinbase,
            "/products",
            json!([{
                "id": "BTC-USD",
                "base_currency": "BTC",
                "quote_currency": "USD",
                "display_name": "BTC/USD",
                "status": "online",
                "trading_disabled": false,
            }]),
        )
        .await;
        Mock::given(method("GET"))
            .and(path("/v1/finance/search"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "quotes": [
                { "symbol": "BTCM", "shortname": "Bitcoin miner", "quoteType": "EQUITY",
                  "exchDisp": "NASDAQ", "isYahooFinance": true }
            ] })))
            .mount(&spot.yahoo)
            .await;

        let (status, body) = get_json(spot.config(), "/api/spot/search?q=btc").await;

        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["results"][0]["symbol"], "BTC");
        assert_eq!(body["results"][0]["assetClass"], "crypto");
        assert_eq!(body["results"][0]["venue"], "Coinbase");
        assert_eq!(body["results"][1]["symbol"], "BTCM");
        assert_eq!(body["results"][1]["assetClass"], "stock");
    }

    #[tokio::test]
    async fn a_row_with_a_mapped_ladder_is_flagged_as_chartable() {
        // The point of the flag: the search result answers "and can I chart the
        // market's forecast of it?" without a second round trip.
        let spot = Spot::start().await;
        mount(
            &spot.coinbase,
            "/products",
            json!([{
                "id": "BTC-USD", "base_currency": "BTC", "quote_currency": "USD",
                "display_name": "BTC/USD", "status": "online", "trading_disabled": false,
            }]),
        )
        .await;
        Mock::given(method("GET"))
            .and(path("/v1/finance/search"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "quotes": [
                { "symbol": "ZZZZ", "shortname": "Nothing prices this", "quoteType": "EQUITY",
                  "isYahooFinance": true }
            ] })))
            .mount(&spot.yahoo)
            .await;

        let (_, body) = get_json(spot.config(), "/api/spot/search?q=btc").await;

        assert_eq!(body["results"][0]["symbol"], "BTC");
        assert_eq!(body["results"][0]["hasImplied"], true);
        assert_eq!(body["results"][1]["hasImplied"], false);
    }

    #[tokio::test]
    async fn one_dead_provider_costs_its_own_rows_and_nothing_else() {
        let spot = Spot::start().await;
        mount(
            &spot.coinbase,
            "/products",
            json!([{
                "id": "BTC-USD", "base_currency": "BTC", "quote_currency": "USD",
                "display_name": "BTC/USD", "status": "online", "trading_disabled": false,
            }]),
        )
        .await;
        // Yahoo down, and Nasdaq is not a search provider at all.
        Mock::given(method("GET"))
            .and(path("/v1/finance/search"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&spot.yahoo)
            .await;

        let (status, body) = get_json(spot.config(), "/api/spot/search?q=btc").await;

        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["results"].as_array().unwrap().len(), 1);
        assert_eq!(body["results"][0]["symbol"], "BTC");
    }

    #[tokio::test]
    async fn both_providers_failing_is_an_empty_list_rather_than_a_failed_request() {
        let spot = Spot::start().await;
        Mock::given(method("GET"))
            .and(path("/products"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&spot.coinbase)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/finance/search"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&spot.yahoo)
            .await;

        let (status, body) = get_json(spot.config(), "/api/spot/search?q=btc").await;

        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["results"].as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn a_class_filter_skips_the_other_family_entirely() {
        let spot = Spot::start().await;
        mount(
            &spot.coinbase,
            "/products",
            json!([{
                "id": "BTC-USD", "base_currency": "BTC", "quote_currency": "USD",
                "display_name": "BTC/USD", "status": "online", "trading_disabled": false,
            }]),
        )
        .await;
        Mock::given(method("GET"))
            .and(path("/v1/finance/search"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "quotes": [
                { "symbol": "BTCM", "shortname": "Bitcoin miner", "quoteType": "EQUITY",
                  "isYahooFinance": true }
            ] })))
            .mount(&spot.yahoo)
            .await;

        let (_, coins_only) = get_json(spot.config(), "/api/spot/search?q=btc&class=crypto").await;
        assert_eq!(coins_only["results"].as_array().unwrap().len(), 1);
        assert_eq!(coins_only["results"][0]["assetClass"], "crypto");

        let (_, stocks_only) = get_json(spot.config(), "/api/spot/search?q=btc&class=stock").await;
        assert_eq!(stocks_only["results"].as_array().unwrap().len(), 1);
        assert_eq!(stocks_only["results"][0]["assetClass"], "stock");
    }

    #[tokio::test]
    async fn the_result_list_is_cut_to_the_limit() {
        let spot = Spot::start().await;
        mount(
            &spot.coinbase,
            "/products",
            json!([
                { "id": "BTC-USD", "base_currency": "BTC", "quote_currency": "USD",
                  "display_name": "BTC/USD", "status": "online", "trading_disabled": false },
                { "id": "BTT-USD", "base_currency": "BTT", "quote_currency": "USD",
                  "display_name": "BTT/USD", "status": "online", "trading_disabled": false },
            ]),
        )
        .await;
        Mock::given(method("GET"))
            .and(path("/v1/finance/search"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "quotes": [] })))
            .mount(&spot.yahoo)
            .await;

        let (_, body) = get_json(spot.config(), "/api/spot/search?q=bt&limit=1").await;
        assert_eq!(body["results"].as_array().unwrap().len(), 1);
    }

    /* ----------------------------------------------------------- alias expansion */

    #[test]
    fn a_registry_alias_expands_to_the_symbol_the_provider_knows() {
        // Somebody types `SPX`; Yahoo knows it as `^GSPC`.
        assert_eq!(resolve_symbol("SPX", AssetClass::Stock), "^GSPC");
        assert_eq!(resolve_symbol("spx", AssetClass::Stock), "^GSPC");
        assert_eq!(resolve_symbol("BTC", AssetClass::Crypto), "BTC-USD");
        // A symbol nothing maps passes straight through.
        assert_eq!(resolve_symbol("AAPL", AssetClass::Stock), "AAPL");
        // And a mapping for the *other* family is not applied: `crypto/SPX`
        // must not quietly become a Yahoo index request.
        assert_eq!(resolve_symbol("SPX", AssetClass::Crypto), "SPX");
        assert_eq!(resolve_symbol("BTC", AssetClass::Stock), "BTC");
    }

    #[tokio::test]
    async fn a_quote_reaches_the_upstream_under_the_expanded_symbol() {
        let spot = Spot::start().await;
        // Percent-encoded, because a cash index carries a caret.
        mount(
            &spot.yahoo,
            "/v8/finance/chart/%5EGSPC",
            yahoo_chart("^GSPC"),
        )
        .await;

        let (status, body) = get_json(spot.config(), "/api/spot/stock/SPX").await;

        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["symbol"], "^GSPC");
        assert_eq!(body["assetClass"], "stock");
    }

    #[tokio::test]
    async fn a_crypto_quote_goes_to_coinbase() {
        let spot = Spot::start().await;
        mount(
            &spot.coinbase,
            "/products/BTC-USD/ticker",
            json!({ "price": "60000.00", "volume": "1234.5", "time": "2026-08-18T00:00:00Z" }),
        )
        .await;
        mount(
            &spot.coinbase,
            "/products/BTC-USD/stats",
            json!({ "open": "59000.00", "high": "61000.00", "low": "58500.00" }),
        )
        .await;

        let (status, body) = get_json(spot.config(), "/api/spot/crypto/BTC").await;

        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["assetClass"], "crypto");
        assert_eq!(body["price"], 60000.0);
    }

    /* -------------------------------------------------------------- the window */

    #[test]
    fn the_default_window_is_scaled_to_the_interval() {
        let now = 1_700_000_000;
        for (interval, span) in [
            (CandleInterval::OneMinute, 6 * 3_600),
            (CandleInterval::OneHour, 30 * 86_400),
            (CandleInterval::OneDay, 365 * 86_400),
        ] {
            let (start, end) = snapped_window(&QueryParams::parse(""), interval, now).unwrap();
            assert_eq!(end - start, span, "{interval:?}");
        }
    }

    #[test]
    fn clamps_an_end_in_the_future_to_a_day_ahead() {
        let now = 1_700_000_000;
        let far = now + 400 * 86_400;
        let (_, end) = snapped_window(
            &QueryParams::parse(&format!("end={far}")),
            CandleInterval::OneHour,
            now,
        )
        .unwrap();

        assert_eq!(end, snap_to_grid(snap_to_grid(now) + 86_400));
    }

    #[test]
    fn a_window_that_ends_before_it_starts_is_the_callers_mistake() {
        let err = snapped_window(
            &QueryParams::parse("start=1700000000&end=1600000000"),
            CandleInterval::OneHour,
            1_700_000_000,
        )
        .unwrap_err();
        assert_eq!(err.code, codes::BAD_REQUEST);
    }

    #[test]
    fn a_window_narrower_than_the_grid_collapses_and_is_refused() {
        // Legal before the snap, empty after it. Serving it would ask the
        // upstream for a window it can only answer with nothing.
        let err = snapped_window(
            &QueryParams::parse("start=1700000001&end=1700000004"),
            CandleInterval::OneHour,
            1_700_000_100,
        )
        .unwrap_err();
        assert_eq!(err.code, codes::BAD_REQUEST);
    }

    #[test]
    fn every_instant_in_one_grid_cell_produces_the_same_window() {
        // This is what makes `ttl::CANDLES` fire at all: a panel polling on its
        // own timer must produce one cache key, not a fresh one per tick.
        let cell = snap_to_grid(1_700_000_000);
        let first = snapped_window(&QueryParams::parse(""), CandleInterval::OneHour, cell).unwrap();

        for offset in 0..15 {
            let window = snapped_window(
                &QueryParams::parse(""),
                CandleInterval::OneHour,
                cell + offset,
            )
            .unwrap();
            assert_eq!(window, first, "offset {offset}");
        }
        // The next cell is a new key, which is the whole point of a 15s TTL.
        let next =
            snapped_window(&QueryParams::parse(""), CandleInterval::OneHour, cell + 15).unwrap();
        assert_ne!(next, first);
    }

    #[test]
    fn both_bounds_always_land_on_the_grid() {
        for raw in ["", "end=1700000007", "start=1699000007&end=1700000007"] {
            let (start, end) = snapped_window(
                &QueryParams::parse(raw),
                CandleInterval::OneHour,
                1_700_000_123,
            )
            .unwrap();
            assert_eq!(start % 15, 0, "{raw}");
            assert_eq!(end % 15, 0, "{raw}");
        }
    }

    #[tokio::test]
    async fn three_polls_inside_one_cell_ask_for_the_same_window() {
        // The mock matches those two bounds and nothing else, so a poll that
        // snapped differently would 404 rather than quietly widening the chart.
        let spot = Spot::start().await;
        Mock::given(method("GET"))
            .and(path("/v8/finance/chart/AAPL"))
            .and(query_param("period1", "1697407995"))
            .and(query_param("period2", "1699999995"))
            .respond_with(ResponseTemplate::new(200).set_body_json(yahoo_chart("AAPL")))
            .mount(&spot.yahoo)
            .await;

        let config = spot.config();
        for end in [1_700_000_000, 1_700_000_004, 1_700_000_009] {
            let (status, body) = get_json(
                config.clone(),
                &format!("/api/spot/stock/AAPL/candles?end={end}"),
            )
            .await;
            assert_eq!(status, StatusCode::OK, "end={end}: {body}");
        }

        // Each `get_json` builds its own router, so each poll has a cold cache
        // and every one of them had to match the same two bounds.
        assert_eq!(spot.yahoo.received_requests().await.unwrap().len(), 3);
    }

    #[tokio::test]
    async fn a_polling_panel_shares_one_cache_key_across_ticks() {
        let spot = Spot::start().await;
        Mock::given(method("GET"))
            .and(path("/v8/finance/chart/AAPL"))
            .respond_with(ResponseTemplate::new(200).set_body_json(yahoo_chart("AAPL")))
            .mount(&spot.yahoo)
            .await;

        // One router, one cache — as a running server has.
        let app = crate::build_router(AppState::new(spot.config()));
        for end in [1_700_000_000, 1_700_000_004, 1_700_000_009] {
            let response = app
                .clone()
                .oneshot(
                    Request::get(format!("/api/spot/stock/AAPL/candles?end={end}"))
                        .body(axum::body::Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK, "end={end}");
        }

        assert_eq!(
            spot.yahoo.received_requests().await.unwrap().len(),
            1,
            "three polls inside one 15s cell must share one cache key"
        );
    }

    #[tokio::test]
    async fn an_interval_off_the_three_buckets_is_refused_with_the_hint() {
        for uri in [
            "/api/spot/stock/AAPL/candles?interval=5",
            "/api/spot/crypto/BTC/candles?interval=900",
        ] {
            let (status, body) = get_offline(uri).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{uri}");
            assert_eq!(body["code"], codes::BAD_REQUEST, "{uri}");
            assert_eq!(body["hint"], crate::routes::helpers::INTERVAL_HINT, "{uri}");
        }
    }

    #[tokio::test]
    async fn candles_reach_the_upstream_on_the_grid() {
        let spot = Spot::start().await;
        Mock::given(method("GET"))
            .and(path("/v8/finance/chart/AAPL"))
            .respond_with(ResponseTemplate::new(200).set_body_json(yahoo_chart("AAPL")))
            .mount(&spot.yahoo)
            .await;

        let (status, body) = get_json(
            spot.config(),
            "/api/spot/stock/AAPL/candles?start=1699000007&end=1700000007",
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");

        let requests = spot.yahoo.received_requests().await.unwrap();
        let asked = &requests[0].url;
        let read = |name: &str| -> i64 {
            asked
                .query_pairs()
                .find(|(key, _)| key == name)
                .expect(name)
                .1
                .parse()
                .expect(name)
        };
        assert_eq!(read("period1"), 1_699_000_005);
        assert_eq!(read("period2"), 1_699_999_995);
    }
}
