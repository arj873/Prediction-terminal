//! `/api/kalshi/…` — Kalshi's own mount.
//!
//! This predates the other two venues and is kept so that anything written
//! against the original API still works. Every handler calls the same
//! [`crate::sources::kalshi`] functions the venue router reaches through
//! [`crate::sources::venues`], so the two mounts cannot answer differently.
//!
//! Two things live here that the venue-agnostic surface does not carry.
//! Kalshi's catalogue endpoints page with an opaque `cursor`, which neither
//! Polymarket exposes, and its market list takes filters (`event_ticker`,
//! `series_ticker`, `tickers`) that only make sense against its own ticker
//! scheme. Both reach the source directly rather than being flattened into the
//! shared verbs.

use std::str::FromStr;

use axum::extract::{Path, RawQuery, State};
use axum::routing::get;
use axum::{Json, Router};
use terminal_core::types::{
    CandlesResponse, EventsResponse, Market, MarketsResponse, OrderBook, SearchResponse,
    SeriesListResponse, TopResponse, TradesResponse, VenueEvent,
};
use terminal_core::venue::{MoverSort, Venue};

use crate::app::AppState;
use crate::error::{Result, UpstreamError};
use crate::routes::helpers::{candle_window, now_seconds, QueryParams};
use crate::sources::kalshi;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/kalshi/markets", get(markets))
        .route("/api/kalshi/markets/{ticker}", get(market))
        .route("/api/kalshi/markets/{ticker}/orderbook", get(order_book))
        .route("/api/kalshi/markets/{ticker}/trades", get(trades))
        .route("/api/kalshi/markets/{ticker}/candles", get(candles))
        .route("/api/kalshi/events", get(events))
        .route("/api/kalshi/events/{event_ticker}", get(event))
        .route("/api/kalshi/series", get(series))
        .route("/api/kalshi/search", get(search))
        .route("/api/kalshi/top", get(top))
}

/// An optional filter: absent, or present and non-empty.
///
/// A blank `?status=` is a panel that has not chosen yet, not a request for
/// markets whose status is the empty string — passing it through would narrow
/// the upstream query to nothing.
fn optional(params: &QueryParams, name: &str) -> Option<String> {
    let value = params.string(name);
    (!value.is_empty()).then_some(value)
}

/// `GET /api/kalshi/markets?limit&cursor&status&event_ticker&series_ticker&tickers`
async fn markets(
    State(state): State<AppState>,
    RawQuery(raw): RawQuery,
) -> Result<Json<MarketsResponse>> {
    let params = QueryParams::parse(raw.as_deref().unwrap_or_default());

    Ok(Json(
        kalshi::list_markets(
            &state,
            kalshi::ListMarketsParams {
                limit: Some(params.int_param("limit", 100, 1, 1000) as u32),
                cursor: optional(&params, "cursor"),
                status: optional(&params, "status"),
                event_ticker: optional(&params, "event_ticker"),
                series_ticker: optional(&params, "series_ticker"),
                tickers: optional(&params, "tickers"),
            },
        )
        .await?,
    ))
}

/// `GET /api/kalshi/markets/{ticker}`
async fn market(State(state): State<AppState>, Path(ticker): Path<String>) -> Result<Json<Market>> {
    Ok(Json(kalshi::get_market(&state, &ticker).await?))
}

/// `GET /api/kalshi/markets/{ticker}/orderbook?depth`
async fn order_book(
    State(state): State<AppState>,
    Path(ticker): Path<String>,
    RawQuery(raw): RawQuery,
) -> Result<Json<OrderBook>> {
    let params = QueryParams::parse(raw.as_deref().unwrap_or_default());
    let depth = params.int_param("depth", 12, 1, 100) as usize;

    Ok(Json(kalshi::get_order_book(&state, &ticker, depth).await?))
}

/// `GET /api/kalshi/markets/{ticker}/trades?limit&cursor`
///
/// The cursor is why this route exists alongside the shared one: Kalshi's tape
/// pages, and the venue-agnostic `get_trades` takes a limit only.
async fn trades(
    State(state): State<AppState>,
    Path(ticker): Path<String>,
    RawQuery(raw): RawQuery,
) -> Result<Json<TradesResponse>> {
    let params = QueryParams::parse(raw.as_deref().unwrap_or_default());
    let limit = params.int_param("limit", 50, 1, 1000) as usize;
    let cursor = optional(&params, "cursor");

    Ok(Json(
        kalshi::get_trades(&state, &ticker, limit, cursor.as_deref()).await?,
    ))
}

/// `GET /api/kalshi/markets/{ticker}/candles?interval&start&end`
async fn candles(
    State(state): State<AppState>,
    Path(ticker): Path<String>,
    RawQuery(raw): RawQuery,
) -> Result<Json<CandlesResponse>> {
    let params = QueryParams::parse(raw.as_deref().unwrap_or_default());

    let interval = params.interval()?;
    let (start_ts, end_ts) = candle_window(&params, interval, now_seconds())?;

    Ok(Json(
        kalshi::get_candles(&state, &ticker, interval, start_ts, end_ts, None).await?,
    ))
}

/// `GET /api/kalshi/events?limit&cursor&status&series_ticker&nested`
async fn events(
    State(state): State<AppState>,
    RawQuery(raw): RawQuery,
) -> Result<Json<EventsResponse>> {
    let params = QueryParams::parse(raw.as_deref().unwrap_or_default());

    Ok(Json(
        kalshi::list_events(
            &state,
            kalshi::ListEventsParams {
                limit: Some(params.int_param("limit", 100, 1, 200) as u32),
                cursor: optional(&params, "cursor"),
                status: optional(&params, "status"),
                series_ticker: optional(&params, "series_ticker"),
                // Nested markets are the point of the endpoint — the ladder is
                // what makes an event readable — so only an explicit
                // `nested=false` turns them off.
                with_nested_markets: params.get("nested") != Some("false"),
            },
        )
        .await?,
    ))
}

/// `GET /api/kalshi/events/{event_ticker}`
async fn event(
    State(state): State<AppState>,
    Path(event_ticker): Path<String>,
) -> Result<Json<VenueEvent>> {
    Ok(Json(kalshi::get_event(&state, &event_ticker).await?))
}

/// `GET /api/kalshi/series?category`
async fn series(
    State(state): State<AppState>,
    RawQuery(raw): RawQuery,
) -> Result<Json<SeriesListResponse>> {
    let params = QueryParams::parse(raw.as_deref().unwrap_or_default());
    let category = optional(&params, "category");

    Ok(Json(SeriesListResponse {
        series: kalshi::list_series(&state, category.as_deref()).await?,
    }))
}

/// `GET /api/kalshi/search?q&limit`
///
/// Kalshi exposes no public search endpoint, so this ranks against a cached
/// snapshot of open events. See [`crate::sources::kalshi`] for why the snapshot
/// is built from events rather than markets.
async fn search(
    State(state): State<AppState>,
    RawQuery(raw): RawQuery,
) -> Result<Json<SearchResponse>> {
    let params = QueryParams::parse(raw.as_deref().unwrap_or_default());

    let query = params.string("q");
    if query.is_empty() {
        return Err(UpstreamError::bad_request("Search needs at least one word")
            .with_hint("Usage: SRCH <words>, e.g. `SRCH fed rate cut`."));
    }
    let limit = params.int_param("limit", 25, 1, 100) as usize;

    Ok(Json(kalshi::search(&state, &query, limit).await?))
}

/// `GET /api/kalshi/top?sort&limit` — most traded, biggest movers, deepest books.
async fn top(State(state): State<AppState>, RawQuery(raw): RawQuery) -> Result<Json<TopResponse>> {
    let params = QueryParams::parse(raw.as_deref().unwrap_or_default());

    let raw_sort = params.str_param("sort", "volume").to_lowercase();
    let sort = MoverSort::from_str(&raw_sort).map_err(|()| {
        let known: Vec<&str> = MoverSort::ALL.iter().map(|s| s.as_str()).collect();
        UpstreamError::bad_request(format!("Unknown sort \"{raw_sort}\""))
            .with_hint(format!("Valid sorts: {}.", known.join(", ")))
    })?;
    let limit = params.int_param("limit", 25, 1, 100) as usize;

    Ok(Json(TopResponse {
        venue: Venue::Kalshi,
        sort,
        markets: kalshi::top_markets(&state, sort, limit).await?,
    }))
}

#[cfg(test)]
mod tests {
    //! The legacy mount against a Kalshi fixture server.
    //!
    //! Most of these assert the *query* that reached the upstream rather than
    //! the payload that came back: this router's whole job is to turn a query
    //! string into Kalshi's, and the normalisers on the far side are covered by
    //! `sources::kalshi`'s own tests.

    use super::*;

    use axum::body::to_bytes;
    use axum::http::{Request, StatusCode};
    use serde_json::{json, Value};
    use tower::ServiceExt;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use crate::config::Config;
    use crate::error::codes;

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

    fn config(server: &MockServer) -> Config {
        Config {
            kalshi_api_base: server.uri(),
            ..Config::default()
        }
    }

    /// The query string the upstream was asked, as `name=value` pairs.
    async fn upstream_query(server: &MockServer, path_suffix: &str) -> Vec<(String, String)> {
        let requests = server.received_requests().await.unwrap();
        let hit = requests
            .iter()
            .find(|r| r.url.path().ends_with(path_suffix))
            .unwrap_or_else(|| panic!("nothing was fetched ending in {path_suffix}"));

        hit.url
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect()
    }

    fn value_of<'a>(pairs: &'a [(String, String)], name: &str) -> Option<&'a str> {
        pairs
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    /* ------------------------------------------------------------- markets */

    #[tokio::test]
    async fn the_market_list_forwards_every_filter_it_takes() {
        let server = MockServer::start().await;
        mount(&server, "/markets", json!({ "markets": [], "cursor": "" })).await;

        let (status, _) = get_json(
            config(&server),
            "/api/kalshi/markets?limit=7&cursor=abc&status=open\
             &event_ticker=KXFED-26SEP&series_ticker=KXFED&tickers=A,B",
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        let query = upstream_query(&server, "/markets").await;
        assert_eq!(value_of(&query, "limit"), Some("7"));
        assert_eq!(value_of(&query, "cursor"), Some("abc"));
        assert_eq!(value_of(&query, "status"), Some("open"));
        assert_eq!(value_of(&query, "event_ticker"), Some("KXFED-26SEP"));
        assert_eq!(value_of(&query, "series_ticker"), Some("KXFED"));
        assert_eq!(value_of(&query, "tickers"), Some("A,B"));
    }

    #[tokio::test]
    async fn an_absent_filter_is_left_off_the_upstream_query_entirely() {
        // A blank `?status=` is a panel that has not chosen yet; forwarding it
        // would narrow the upstream query to markets with no status at all.
        let server = MockServer::start().await;
        mount(&server, "/markets", json!({ "markets": [], "cursor": "" })).await;

        let (status, _) = get_json(config(&server), "/api/kalshi/markets?status=&cursor=").await;
        assert_eq!(status, StatusCode::OK);

        let query = upstream_query(&server, "/markets").await;
        assert_eq!(value_of(&query, "status"), None);
        assert_eq!(value_of(&query, "cursor"), None);
        // The limit always goes, because it always has a default.
        assert_eq!(value_of(&query, "limit"), Some("100"));
    }

    #[tokio::test]
    async fn the_market_list_limit_is_clamped_to_a_thousand() {
        let server = MockServer::start().await;
        mount(&server, "/markets", json!({ "markets": [], "cursor": "" })).await;

        get_json(config(&server), "/api/kalshi/markets?limit=99999").await;
        let query = upstream_query(&server, "/markets").await;
        assert_eq!(value_of(&query, "limit"), Some("1000"));
    }

    #[tokio::test]
    async fn one_market_is_read_by_ticker() {
        let server = MockServer::start().await;
        mount(
            &server,
            "/markets/KXFED-26SEP-T3.75",
            json!({ "market": { "ticker": "KXFED-26SEP-T3.75", "title": "Fed" } }),
        )
        .await;

        let (status, body) =
            get_json(config(&server), "/api/kalshi/markets/KXFED-26SEP-T3.75").await;

        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["ticker"], "KXFED-26SEP-T3.75");
        assert_eq!(body["venue"], "kalshi");
    }

    #[tokio::test]
    async fn the_book_depth_defaults_to_twelve_and_clamps_to_a_hundred() {
        let server = MockServer::start().await;
        mount(
            &server,
            "/markets/KXFED/orderbook",
            json!({ "orderbook_fp": { "yes_dollars": [], "no_dollars": [] } }),
        )
        .await;

        for (uri, expected) in [
            ("/api/kalshi/markets/KXFED/orderbook", "12"),
            ("/api/kalshi/markets/KXFED/orderbook?depth=3", "3"),
            ("/api/kalshi/markets/KXFED/orderbook?depth=9999", "100"),
            ("/api/kalshi/markets/KXFED/orderbook?depth=0", "1"),
        ] {
            let server = MockServer::start().await;
            mount(
                &server,
                "/markets/KXFED/orderbook",
                json!({ "orderbook_fp": { "yes_dollars": [], "no_dollars": [] } }),
            )
            .await;

            let (status, _) = get_json(config(&server), uri).await;
            assert_eq!(status, StatusCode::OK, "{uri}");
            let query = upstream_query(&server, "/orderbook").await;
            assert_eq!(value_of(&query, "depth"), Some(expected), "{uri}");
        }
    }

    #[tokio::test]
    async fn the_tape_forwards_its_cursor_and_clamps_its_limit() {
        let server = MockServer::start().await;
        mount(&server, "/markets/trades", json!({ "trades": [] })).await;

        let (status, _) = get_json(
            config(&server),
            "/api/kalshi/markets/KXFED/trades?limit=99999&cursor=page2",
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        let query = upstream_query(&server, "/markets/trades").await;
        assert_eq!(value_of(&query, "ticker"), Some("KXFED"));
        assert_eq!(value_of(&query, "limit"), Some("1000"));
        assert_eq!(value_of(&query, "cursor"), Some("page2"));
    }

    /* ------------------------------------------------------------- candles */

    #[tokio::test]
    async fn candles_reach_the_series_scoped_endpoint_with_the_asked_bucket() {
        let server = MockServer::start().await;
        mount(
            &server,
            "/markets/KXFED-26SEP-T3.75",
            json!({ "market": { "ticker": "KXFED-26SEP-T3.75", "series_ticker": "KXFED" } }),
        )
        .await;
        mount(
            &server,
            "/series/KXFED/markets/KXFED-26SEP-T3.75/candlesticks",
            json!({ "candlesticks": [] }),
        )
        .await;

        let (status, body) = get_json(
            config(&server),
            "/api/kalshi/markets/KXFED-26SEP-T3.75/candles?interval=1440",
        )
        .await;

        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["interval"], 1440);

        let query = upstream_query(&server, "/candlesticks").await;
        assert_eq!(value_of(&query, "period_interval"), Some("1440"));

        // A day chart reaches back a year when nobody says otherwise.
        let start: i64 = value_of(&query, "start_ts").unwrap().parse().unwrap();
        let end: i64 = value_of(&query, "end_ts").unwrap().parse().unwrap();
        assert_eq!(end - start, 365 * 86_400);
    }

    #[tokio::test]
    async fn an_interval_off_the_three_buckets_is_refused_with_the_hint() {
        for uri in [
            "/api/kalshi/markets/KXFED/candles?interval=5",
            "/api/kalshi/markets/KXFED/candles?interval=15",
            "/api/kalshi/markets/KXFED/candles?interval=hourly",
        ] {
            let (status, body) = get_offline(uri).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{uri}");
            assert_eq!(body["code"], codes::BAD_REQUEST, "{uri}");
            assert_eq!(body["hint"], crate::routes::helpers::INTERVAL_HINT, "{uri}");
        }
    }

    #[tokio::test]
    async fn a_window_that_ends_before_it_starts_is_a_bad_request() {
        let (status, body) =
            get_offline("/api/kalshi/markets/KXFED/candles?start=1700000000&end=1600000000").await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["code"], codes::BAD_REQUEST);
    }

    /* -------------------------------------------------------------- events */

    #[tokio::test]
    async fn nested_markets_are_on_unless_the_caller_says_false() {
        for (uri, expected) in [
            ("/api/kalshi/events", Some("true")),
            ("/api/kalshi/events?nested=true", Some("true")),
            // Anything that is not the literal `false` keeps them, as the
            // original mount did.
            ("/api/kalshi/events?nested=yes", Some("true")),
            ("/api/kalshi/events?nested=false", None),
        ] {
            let server = MockServer::start().await;
            mount(&server, "/events", json!({ "events": [], "cursor": "" })).await;

            let (status, _) = get_json(config(&server), uri).await;
            assert_eq!(status, StatusCode::OK, "{uri}");

            let query = upstream_query(&server, "/events").await;
            assert_eq!(value_of(&query, "with_nested_markets"), expected, "{uri}");
        }
    }

    #[tokio::test]
    async fn the_event_list_limit_is_clamped_to_two_hundred() {
        let server = MockServer::start().await;
        mount(&server, "/events", json!({ "events": [], "cursor": "" })).await;

        get_json(
            config(&server),
            "/api/kalshi/events?limit=5000&status=open&series_ticker=KXFED&cursor=c1",
        )
        .await;

        let query = upstream_query(&server, "/events").await;
        assert_eq!(value_of(&query, "limit"), Some("200"));
        assert_eq!(value_of(&query, "status"), Some("open"));
        assert_eq!(value_of(&query, "series_ticker"), Some("KXFED"));
        assert_eq!(value_of(&query, "cursor"), Some("c1"));
    }

    #[tokio::test]
    async fn one_event_is_read_by_its_ticker() {
        let server = MockServer::start().await;
        mount(
            &server,
            "/events/KXFED-26SEP",
            json!({ "event": { "event_ticker": "KXFED-26SEP", "title": "Fed" }, "markets": [] }),
        )
        .await;

        let (status, body) = get_json(config(&server), "/api/kalshi/events/KXFED-26SEP").await;

        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["eventTicker"], "KXFED-26SEP");
    }

    /* --------------------------------------------------- series and search */

    #[tokio::test]
    async fn the_series_category_filter_reaches_the_upstream() {
        let server = MockServer::start().await;
        mount(&server, "/series/", json!({ "series": [] })).await;

        get_json(config(&server), "/api/kalshi/series?category=Economics").await;
        let query = upstream_query(&server, "/series/").await;
        assert_eq!(value_of(&query, "category"), Some("Economics"));
    }

    #[tokio::test]
    async fn a_search_with_no_words_says_so_rather_than_searching_for_nothing() {
        for uri in ["/api/kalshi/search", "/api/kalshi/search?q=%20%20"] {
            let (status, body) = get_offline(uri).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{uri}");
            assert_eq!(body["code"], codes::BAD_REQUEST, "{uri}");
            assert_eq!(body["error"], "Search needs at least one word", "{uri}");
            assert!(body["hint"].as_str().unwrap().contains("SRCH"), "{uri}");
        }
    }

    #[tokio::test]
    async fn search_ranks_the_open_event_snapshot() {
        let server = MockServer::start().await;
        mount(
            &server,
            "/events",
            json!({
                "events": [{
                    "event_ticker": "KXFED-26SEP",
                    "series_ticker": "KXFED",
                    "title": "Fed decision in September",
                    "markets": [{ "ticker": "KXFED-26SEP-T3.75" }],
                }],
                "cursor": "",
            }),
        )
        .await;

        let (status, body) = get_json(config(&server), "/api/kalshi/search?q=fed").await;

        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["query"], "fed");
        assert_eq!(body["hits"][0]["event"]["eventTicker"], "KXFED-26SEP");
    }

    /* ----------------------------------------------------------------- top */

    #[tokio::test]
    async fn an_unknown_sort_lists_the_real_ones() {
        let (status, body) = get_offline("/api/kalshi/top?sort=alphabetical").await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"], "Unknown sort \"alphabetical\"");
        assert_eq!(
            body["hint"],
            "Valid sorts: volume, gainers, losers, open_interest, liquidity."
        );
    }

    #[tokio::test]
    async fn the_board_names_kalshi_and_the_sort_it_ranked_on() {
        let server = MockServer::start().await;
        mount(&server, "/events", json!({ "events": [], "cursor": "" })).await;

        let (status, body) = get_json(config(&server), "/api/kalshi/top?sort=LOSERS").await;

        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["venue"], "kalshi");
        assert_eq!(body["sort"], "losers");
    }

    /* -------------------------------------------------- the two mounts agree */

    #[tokio::test]
    async fn the_legacy_mount_and_the_venue_mount_answer_the_same_market() {
        // The whole reason this router still exists: anything written against
        // the original API must keep working, and must not drift from what the
        // venue-agnostic mount says about the same contract.
        let server = MockServer::start().await;
        mount(
            &server,
            "/markets/KXFED-26SEP-T3.75",
            json!({ "market": { "ticker": "KXFED-26SEP-T3.75", "title": "Fed" } }),
        )
        .await;

        let (legacy_status, legacy) =
            get_json(config(&server), "/api/kalshi/markets/KXFED-26SEP-T3.75").await;
        let (venue_status, venue) = get_json(
            config(&server),
            "/api/venue/kalshi/markets/KXFED-26SEP-T3.75",
        )
        .await;

        assert_eq!(legacy_status, StatusCode::OK, "{legacy}");
        assert_eq!(venue_status, StatusCode::OK, "{venue}");
        assert_eq!(legacy, venue);
    }
}
