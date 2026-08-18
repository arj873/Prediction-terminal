//! `/api/venue/{venue}/…` — the same market routes for every broker.
//!
//! One router serves all three, because the normalisers already made their
//! payloads the same shape. The venue segment is validated here so a bad name
//! fails at the door with the list of good ones, rather than as a 404 from
//! whichever upstream happened to be asked.

use std::str::FromStr;

use axum::extract::{Path, RawQuery, State};
use axum::routing::get;
use axum::{Json, Router};
use terminal_core::types::{
    CandlesResponse, CatalogueSnapshot, Market, OrderBook, SearchResponse, SeriesListResponse,
    TopResponse, TradesResponse, VenueEvent,
};
use terminal_core::venue::{normalise_id, venue_ids, MoverSort, Venue};

use crate::app::AppState;
use crate::error::{Result, UpstreamError};
use crate::routes::helpers::{candle_window, now_seconds, QueryParams};
use crate::sources::venues;

/// The paths are absolute because `routes::mod` merges rather than nests, so
/// this list is readable as the venue half of the API map on its own.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/venue/{venue}/markets/{id}", get(market))
        .route("/api/venue/{venue}/markets/{id}/orderbook", get(order_book))
        .route("/api/venue/{venue}/markets/{id}/trades", get(trades))
        .route("/api/venue/{venue}/markets/{id}/candles", get(candles))
        .route("/api/venue/{venue}/events/{id}", get(event))
        .route("/api/venue/{venue}/series", get(series))
        .route("/api/venue/{venue}/search", get(search))
        .route("/api/venue/{venue}/top", get(top))
        .route("/api/venue/{venue}/catalogue", get(catalogue))
}

/// Read the venue segment.
///
/// A name nobody lists is a 400 rather than a 404: the route exists and the
/// argument to it is wrong, and answering "not found" would send the reader
/// looking for a market that was never the problem. The message names the
/// venues, because the only useful next move is to retype one of them.
fn venue_param(raw: &str) -> Result<Venue> {
    Venue::from_str(raw).map_err(|()| {
        let known: Vec<&str> = venue_ids().iter().map(|v| v.as_str()).collect();
        UpstreamError::bad_request(format!("Unknown venue \"{raw}\""))
            .with_hint(format!("Venues are: {}.", known.join(", ")))
    })
}

/// The venue and the identifier, cased the way that venue answers to.
///
/// Kalshi 404s a lower-case ticker and both Polymarkets 404 an upper-case slug,
/// so the fold happens here — once, from the registry — and never at a call
/// site that would have to know which venue it was talking to.
fn reference(venue: &str, id: &str) -> Result<(Venue, String)> {
    let venue = venue_param(venue)?;
    Ok((venue, normalise_id(venue, id)))
}

/// `GET /api/venue/{venue}/markets/{id}`
async fn market(
    State(state): State<AppState>,
    Path((venue, id)): Path<(String, String)>,
) -> Result<Json<Market>> {
    let (venue, id) = reference(&venue, &id)?;
    Ok(Json(venues::get_market(&state, venue, &id).await?))
}

/// `GET /api/venue/{venue}/markets/{id}/orderbook?depth`
async fn order_book(
    State(state): State<AppState>,
    Path((venue, id)): Path<(String, String)>,
    RawQuery(raw): RawQuery,
) -> Result<Json<OrderBook>> {
    let (venue, id) = reference(&venue, &id)?;
    let params = QueryParams::parse(raw.as_deref().unwrap_or_default());
    let depth = params.int_param("depth", 12, 1, 100) as usize;

    Ok(Json(
        venues::get_order_book(&state, venue, &id, depth).await?,
    ))
}

/// `GET /api/venue/{venue}/markets/{id}/trades?limit`
///
/// Polymarket US publishes no public tape, and the dispatcher declines from the
/// registry rather than dialling — the reader gets the note explaining what the
/// venue does publish instead of an empty tape that reads as a dead market.
async fn trades(
    State(state): State<AppState>,
    Path((venue, id)): Path<(String, String)>,
    RawQuery(raw): RawQuery,
) -> Result<Json<TradesResponse>> {
    let (venue, id) = reference(&venue, &id)?;
    let params = QueryParams::parse(raw.as_deref().unwrap_or_default());
    let limit = params.int_param("limit", 50, 1, 1000) as usize;

    Ok(Json(venues::get_trades(&state, venue, &id, limit).await?))
}

/// `GET /api/venue/{venue}/markets/{id}/candles?interval&start&end`
async fn candles(
    State(state): State<AppState>,
    Path((venue, id)): Path<(String, String)>,
    RawQuery(raw): RawQuery,
) -> Result<Json<CandlesResponse>> {
    let (venue, id) = reference(&venue, &id)?;
    let params = QueryParams::parse(raw.as_deref().unwrap_or_default());

    let interval = params.interval()?;
    let (start_ts, end_ts) = candle_window(&params, interval, now_seconds())?;

    Ok(Json(
        venues::get_candles(&state, venue, &id, interval, start_ts, end_ts).await?,
    ))
}

/// `GET /api/venue/{venue}/events/{id}`
async fn event(
    State(state): State<AppState>,
    Path((venue, id)): Path<(String, String)>,
) -> Result<Json<VenueEvent>> {
    let (venue, id) = reference(&venue, &id)?;
    Ok(Json(venues::get_event(&state, venue, &id).await?))
}

/// `GET /api/venue/{venue}/series?category`
async fn series(
    State(state): State<AppState>,
    Path(venue): Path<String>,
    RawQuery(raw): RawQuery,
) -> Result<Json<SeriesListResponse>> {
    let venue = venue_param(&venue)?;
    let params = QueryParams::parse(raw.as_deref().unwrap_or_default());
    let category = params.get("category");

    Ok(Json(SeriesListResponse {
        series: venues::list_series(&state, venue, category).await?,
    }))
}

/// `GET /api/venue/{venue}/search?q&limit`
async fn search(
    State(state): State<AppState>,
    Path(venue): Path<String>,
    RawQuery(raw): RawQuery,
) -> Result<Json<SearchResponse>> {
    let venue = venue_param(&venue)?;
    let params = QueryParams::parse(raw.as_deref().unwrap_or_default());

    let query = params.string("q");
    if query.is_empty() {
        return Err(UpstreamError::bad_request("Search needs at least one word")
            .with_hint("Usage: SRCH <words>, e.g. `SRCH fed rate cut`."));
    }
    let limit = params.int_param("limit", 25, 1, 100) as usize;

    Ok(Json(venues::search(&state, venue, &query, limit).await?))
}

/// `GET /api/venue/{venue}/top?sort&limit`
async fn top(
    State(state): State<AppState>,
    Path(venue): Path<String>,
    RawQuery(raw): RawQuery,
) -> Result<Json<TopResponse>> {
    let venue = venue_param(&venue)?;
    let params = QueryParams::parse(raw.as_deref().unwrap_or_default());

    let raw_sort = params.str_param("sort", "volume").to_lowercase();
    let sort = MoverSort::from_str(&raw_sort).map_err(|()| {
        let known: Vec<&str> = MoverSort::ALL.iter().map(|s| s.as_str()).collect();
        UpstreamError::bad_request(format!("Unknown sort \"{raw_sort}\""))
            .with_hint(format!("Valid sorts: {}.", known.join(", ")))
    })?;
    let limit = params.int_param("limit", 25, 1, 100) as usize;

    Ok(Json(TopResponse {
        venue,
        sort,
        markets: venues::top_markets(&state, venue, sort, limit).await?,
    }))
}

/// `GET /api/venue/{venue}/catalogue` — what the snapshot behind `SRCH` and
/// `TOP` currently holds.
async fn catalogue(
    State(state): State<AppState>,
    Path(venue): Path<String>,
) -> Result<Json<CatalogueSnapshot>> {
    let venue = venue_param(&venue)?;
    let snapshot = venues::corpus_snapshot(&state, venue).await?;

    Ok(Json(CatalogueSnapshot {
        venue,
        events: snapshot.events.len(),
        markets: snapshot.markets.len(),
        truncated: snapshot.truncated,
        age_seconds: snapshot.age_seconds(),
    }))
}

#[cfg(test)]
mod tests {
    //! The venue router against fixture servers for all three brokers.
    //!
    //! Every test drives [`crate::build_router`], so the path table, the
    //! extractors and the error ladder are all exercised — a handler tested
    //! directly would not catch `/search` losing to `/{id}`, or a 501 being
    //! served as a 500.

    use super::*;

    use axum::body::to_bytes;
    use axum::http::{Request, StatusCode};
    use serde_json::{json, Value};
    use tower::ServiceExt;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use crate::config::Config;
    use crate::error::codes;

    /// The three fixture hosts a venue request can reach between them.
    struct Venues {
        kalshi: MockServer,
        gamma: MockServer,
        us: MockServer,
    }

    impl Venues {
        async fn start() -> Self {
            Self {
                kalshi: MockServer::start().await,
                gamma: MockServer::start().await,
                us: MockServer::start().await,
            }
        }

        fn config(&self) -> Config {
            Config {
                kalshi_api_base: self.kalshi.uri(),
                polymarket_gamma_base: self.gamma.uri(),
                polymarket_clob_base: self.gamma.uri(),
                polymarket_data_base: self.gamma.uri(),
                polymarket_us_api_base: self.us.uri(),
                ..Config::default()
            }
        }

        /// Every request the three servers saw, so a test can assert that a
        /// refusal came from the registry rather than from an upstream.
        async fn requests(&self) -> usize {
            let mut total = 0;
            for server in [&self.kalshi, &self.gamma, &self.us] {
                total += server.received_requests().await.map_or(0, |r| r.len());
            }
            total
        }
    }

    async fn mount(server: &MockServer, at: &str, body: Value) {
        Mock::given(method("GET"))
            .and(path(at))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(server)
            .await;
    }

    /// One `GET` through the whole application.
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

    /// A request that needs no upstream at all.
    async fn get_offline(uri: &str) -> (StatusCode, Value) {
        get_json(Config::default(), uri).await
    }

    /* ------------------------------------------------------ the venue segment */

    #[tokio::test]
    async fn an_unknown_venue_is_a_bad_request_that_lists_the_real_ones() {
        // Deliberately not a 404: the path exists, the argument to it does not,
        // and a 404 would send the reader hunting for a market instead.
        let (status, body) = get_offline("/api/venue/nyse/markets/ABC").await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["code"], codes::BAD_REQUEST);
        assert_eq!(body["error"], "Unknown venue \"nyse\"");
        assert_eq!(
            body["hint"],
            "Venues are: kalshi, polymarket, polymarket-us."
        );
    }

    #[tokio::test]
    async fn every_venueless_route_rejects_an_unknown_venue_the_same_way() {
        for uri in [
            "/api/venue/nyse/markets/ABC",
            "/api/venue/nyse/markets/ABC/orderbook",
            "/api/venue/nyse/markets/ABC/trades",
            "/api/venue/nyse/markets/ABC/candles",
            "/api/venue/nyse/events/ABC",
            "/api/venue/nyse/series",
            "/api/venue/nyse/search?q=fed",
            "/api/venue/nyse/top",
            "/api/venue/nyse/catalogue",
        ] {
            let (status, body) = get_offline(uri).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{uri}");
            assert!(
                body["hint"].as_str().unwrap().contains("polymarket-us"),
                "{uri}"
            );
        }
    }

    /* ------------------------------------------------------ case normalisation */

    #[tokio::test]
    async fn folds_an_identifier_to_the_case_its_venue_answers_to() {
        let venues = Venues::start().await;

        // Kalshi shouts. A lower-case ticker typed at the prompt must reach the
        // upper-case path, because Kalshi 404s the other one.
        mount(
            &venues.kalshi,
            "/events/KXFED-26SEP",
            json!({ "event": { "event_ticker": "KXFED-26SEP", "title": "Fed" }, "markets": [] }),
        )
        .await;

        // Both Polymarkets whisper, and 404 an upper-case slug.
        Mock::given(method("GET"))
            .and(path("/events"))
            .and(query_param("slug", "fed-decision"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([{
                "slug": "fed-decision",
                "title": "Fed decision",
                "markets": [],
            }])))
            .mount(&venues.gamma)
            .await;

        mount(
            &venues.us,
            "/v1/events/slug/fed-decision",
            json!({ "event": { "slug": "fed-decision", "title": "Fed decision", "markets": [] } }),
        )
        .await;

        for (uri, expected) in [
            ("/api/venue/kalshi/events/kxfed-26sep", "kalshi"),
            ("/api/venue/polymarket/events/FED-Decision", "polymarket"),
            (
                "/api/venue/polymarket-us/events/FED-DECISION",
                "polymarket-us",
            ),
        ] {
            let (status, body) = get_json(venues.config(), uri).await;
            assert_eq!(status, StatusCode::OK, "{uri}: {body}");
            assert_eq!(body["venue"], expected, "{uri}");
        }
    }

    /* --------------------------------------------------------- capability gaps */

    #[tokio::test]
    async fn polymarket_us_candles_and_trades_answer_501_with_the_capability_note() {
        // Declared, not discovered: the registry already says this venue serves
        // no history and no tape, so the refusal must arrive without the
        // fixture server being dialled at all.
        let venues = Venues::start().await;
        let note = terminal_core::venue::venue_info(Venue::PolymarketUs)
            .capabilities
            .note;
        assert!(!note.is_empty());

        for uri in [
            "/api/venue/polymarket-us/markets/some-slug/candles",
            "/api/venue/polymarket-us/markets/some-slug/trades",
        ] {
            let (status, body) = get_json(venues.config(), uri).await;

            assert_eq!(status, StatusCode::NOT_IMPLEMENTED, "{uri}");
            assert_eq!(body["code"], codes::UNSUPPORTED, "{uri}");
            // The note is the sentence written to be read by a person.
            assert_eq!(body["error"], note, "{uri}");
        }

        assert_eq!(
            venues.requests().await,
            0,
            "a declared gap must not cost an upstream request"
        );
    }

    #[tokio::test]
    async fn a_venue_that_publishes_the_figure_still_serves_candles() {
        let venues = Venues::start().await;
        mount(
            &venues.kalshi,
            "/markets/KXFED-26SEP-T3.75",
            json!({ "market": { "ticker": "KXFED-26SEP-T3.75" } }),
        )
        .await;
        mount(
            &venues.kalshi,
            "/series/KXFED/markets/KXFED-26SEP-T3.75/candlesticks",
            json!({ "candlesticks": [] }),
        )
        .await;

        let (status, body) = get_json(
            venues.config(),
            "/api/venue/kalshi/markets/KXFED-26SEP-T3.75/candles",
        )
        .await;

        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["venue"], "kalshi");
    }

    /* ------------------------------------------------------------ candle window */

    /// The `start_ts`/`end_ts` a candle request reached the upstream with.
    async fn window_asked_for(query: &str) -> (i64, i64) {
        let venues = Venues::start().await;
        mount(
            &venues.kalshi,
            "/markets/KXFED",
            json!({ "market": { "ticker": "KXFED" } }),
        )
        .await;
        mount(
            &venues.kalshi,
            "/series/KXFED/markets/KXFED/candlesticks",
            json!({ "candlesticks": [] }),
        )
        .await;

        let (status, body) = get_json(
            venues.config(),
            &format!("/api/venue/kalshi/markets/KXFED/candles{query}"),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");

        let requests = venues.kalshi.received_requests().await.unwrap();
        let candles = requests
            .iter()
            .find(|r| r.url.path().ends_with("/candlesticks"))
            .expect("the candlestick page was fetched");

        let read = |name: &str| -> i64 {
            candles
                .url
                .query_pairs()
                .find(|(key, _)| key == name)
                .expect(name)
                .1
                .parse()
                .expect(name)
        };
        (read("start_ts"), read("end_ts"))
    }

    #[tokio::test]
    async fn the_default_window_is_scaled_to_the_interval() {
        // A minute chart with a year of default window would ask for half a
        // million bars, so each bucket reaches back its own distance.
        for (query, span) in [
            ("?interval=1", 6 * 3_600),
            ("", 30 * 86_400),
            ("?interval=60", 30 * 86_400),
            ("?interval=1440", 365 * 86_400),
        ] {
            let (start, end) = window_asked_for(query).await;
            assert_eq!(end - start, span, "{query}");
        }
    }

    #[tokio::test]
    async fn clamps_an_end_in_the_future_to_a_day_ahead() {
        // A clock-skewed client asking for next year should get today's bars,
        // not an empty chart.
        let far = now_seconds() + 400 * 86_400;
        let (_, end) = window_asked_for(&format!("?end={far}")).await;
        assert!(end <= now_seconds() + 86_400, "{end} was not clamped");
    }

    #[tokio::test]
    async fn a_window_that_ends_before_it_starts_is_a_bad_request() {
        let venues = Venues::start().await;
        let (status, body) = get_json(
            venues.config(),
            "/api/venue/kalshi/markets/KXFED/candles?start=1700000000&end=1600000000",
        )
        .await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["code"], codes::BAD_REQUEST);
        assert_eq!(venues.requests().await, 0);
    }

    #[tokio::test]
    async fn an_interval_off_the_three_buckets_is_refused_with_the_hint() {
        let (status, body) =
            get_offline("/api/venue/kalshi/markets/KXFED/candles?interval=5").await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["code"], codes::BAD_REQUEST);
        assert_eq!(body["hint"], crate::routes::helpers::INTERVAL_HINT);
    }

    /* ----------------------------------------------------------------- the rest */

    #[tokio::test]
    async fn a_search_with_no_words_says_so_rather_than_searching_for_nothing() {
        for uri in ["/api/venue/kalshi/search", "/api/venue/kalshi/search?q=%20"] {
            let (status, body) = get_offline(uri).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{uri}");
            assert_eq!(body["error"], "Search needs at least one word", "{uri}");
            assert!(body["hint"].as_str().unwrap().contains("SRCH"), "{uri}");
        }
    }

    #[tokio::test]
    async fn an_unknown_top_sort_lists_the_real_ones() {
        let (status, body) = get_offline("/api/venue/kalshi/top?sort=alphabetical").await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"], "Unknown sort \"alphabetical\"");
        assert_eq!(
            body["hint"],
            "Valid sorts: volume, gainers, losers, open_interest, liquidity."
        );
    }

    #[tokio::test]
    async fn a_sort_is_read_case_insensitively_and_defaults_to_volume() {
        let venues = Venues::start().await;
        mount(
            &venues.kalshi,
            "/events",
            json!({ "events": [], "cursor": "" }),
        )
        .await;

        for (uri, expected) in [
            ("/api/venue/kalshi/top", "volume"),
            ("/api/venue/kalshi/top?sort=GAINERS", "gainers"),
            ("/api/venue/kalshi/top?sort=Open_Interest", "open_interest"),
        ] {
            let (status, body) = get_json(venues.config(), uri).await;
            assert_eq!(status, StatusCode::OK, "{uri}: {body}");
            assert_eq!(body["sort"], expected, "{uri}");
            assert_eq!(body["venue"], "kalshi", "{uri}");
        }
    }

    #[tokio::test]
    async fn a_board_the_venue_publishes_nothing_for_is_refused_from_the_registry() {
        // Polymarket's catalogue carries no open interest, so an OI board would
        // rank it last on a figure it never published.
        let venues = Venues::start().await;
        let (status, body) = get_json(
            venues.config(),
            "/api/venue/polymarket/top?sort=open_interest",
        )
        .await;

        assert_eq!(status, StatusCode::NOT_IMPLEMENTED, "{body}");
        assert_eq!(body["code"], codes::UNSUPPORTED);
        assert_eq!(venues.requests().await, 0);
    }

    #[tokio::test]
    async fn series_answers_under_a_series_key() {
        let venues = Venues::start().await;
        mount(
            &venues.kalshi,
            "/series/",
            json!({ "series": [{
                "ticker": "KXFED",
                "title": "Fed decision",
                "category": "Economics",
                "frequency": "monthly",
                "tags": [],
            }] }),
        )
        .await;

        let (status, body) = get_json(venues.config(), "/api/venue/kalshi/series").await;

        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["series"][0]["ticker"], "KXFED");
    }

    #[tokio::test]
    async fn catalogue_reports_the_snapshot_behind_search_and_top() {
        let venues = Venues::start().await;
        mount(
            &venues.kalshi,
            "/events",
            json!({
                "events": [{
                    "event_ticker": "KXFED-26SEP",
                    "series_ticker": "KXFED",
                    "title": "Fed decision",
                    "markets": [{ "ticker": "KXFED-26SEP-T3.75" }],
                }],
                "cursor": "",
            }),
        )
        .await;

        let (status, body) = get_json(venues.config(), "/api/venue/kalshi/catalogue").await;

        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["venue"], "kalshi");
        assert_eq!(body["events"], 1);
        assert_eq!(body["markets"], 1);
        assert_eq!(body["truncated"], false);
        assert!(body["ageSeconds"].as_f64().unwrap() < 5.0);
    }

    #[tokio::test]
    async fn a_repeated_query_key_keeps_its_first_value() {
        // `?depth=1&depth=99` must not silently become the last one.
        let venues = Venues::start().await;
        Mock::given(method("GET"))
            .and(path("/markets/KXFED/orderbook"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "orderbook_fp": {
                    "yes_dollars": [["0.49", "20.00"], ["0.50", "10.00"]],
                    "no_dollars": [["0.48", "30.00"]],
                }
            })))
            .mount(&venues.kalshi)
            .await;

        let (status, body) = get_json(
            venues.config(),
            "/api/venue/kalshi/markets/KXFED/orderbook?depth=1&depth=99",
        )
        .await;

        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["yes"].as_array().unwrap().len(), 1);
    }
}
