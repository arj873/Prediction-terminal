//! `/api/xv/…` — the cross-venue views.
//!
//! `series` answers "who else lists this?"; `compare` answers "at what price?".

use std::str::FromStr;

use axum::extract::{RawQuery, State};
use axum::routing::get;
use axum::{Json, Router};
use terminal_core::types::{CompareResponse, LinkedSeriesResponse};
use terminal_core::venue::{venue_ids, Venue};

use crate::app::AppState;
use crate::error::{Result, UpstreamError};
use crate::routes::helpers::QueryParams;
use crate::sources::crossvenue;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/xv/series", get(series))
        .route("/api/xv/compare", get(compare))
}

/// The venues, named the way a caller has to spell them.
fn venue_hint() -> String {
    let known: Vec<&str> = venue_ids().iter().map(|v| v.as_str()).collect();
    format!("Venues are: {}.", known.join(", "))
}

/// `GET /api/xv/series?q&limit`
///
/// An empty `q` is not an error here, unlike a venue search: with no words the
/// board is the whole set of linked series, which is the view `XV` opens on.
async fn series(
    State(state): State<AppState>,
    RawQuery(raw): RawQuery,
) -> Result<Json<LinkedSeriesResponse>> {
    let params = QueryParams::parse(raw.as_deref().unwrap_or_default());

    let query = params.string("q");
    let limit = params.int_param("limit", 40, 1, 200) as usize;

    Ok(Json(
        crossvenue::linked_series(&state, &query, limit).await?,
    ))
}

/// `GET /api/xv/compare?event&venue`
///
/// `venue` disambiguates which broker the event ticker belongs to. It is
/// optional: without it the ticker is looked for across every index, which is
/// what a reader typing one ticker means.
async fn compare(
    State(state): State<AppState>,
    RawQuery(raw): RawQuery,
) -> Result<Json<CompareResponse>> {
    let params = QueryParams::parse(raw.as_deref().unwrap_or_default());

    let event = params.string("event");
    if event.is_empty() {
        return Err(UpstreamError::bad_request("Comparison needs an event")
            .with_hint("Usage: `XV <event-ticker>`, e.g. `XV KXFEDDECISION-26OCT`."));
    }

    let raw_venue = params.string("venue");
    let venue = if raw_venue.is_empty() {
        None
    } else {
        Some(Venue::from_str(&raw_venue).map_err(|()| {
            UpstreamError::bad_request(format!("Unknown venue \"{raw_venue}\""))
                .with_hint(venue_hint())
        })?)
    };

    Ok(Json(crossvenue::compare(&state, &event, venue).await?))
}

#[cfg(test)]
mod tests {
    use super::*;

    use axum::body::to_bytes;
    use axum::http::{Request, StatusCode};
    use serde_json::{json, Value};
    use tower::ServiceExt;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use crate::config::Config;
    use crate::error::codes;

    /// The catalogues the cross-venue index is built from.
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

    /// The same question, listed at Kalshi and at Polymarket International.
    async fn catalogues(venues: &Venues) {
        mount(
            &venues.kalshi,
            "/events",
            json!({
                "events": [{
                    "event_ticker": "KXFEDDECISION-26OCT",
                    "series_ticker": "KXFEDDECISION",
                    "title": "Fed decision in October",
                    "markets": [{
                        "ticker": "KXFEDDECISION-26OCT-T3.75",
                        "yes_bid_dollars": "0.40",
                        "yes_ask_dollars": "0.42",
                    }],
                }],
                "cursor": "",
            }),
        )
        .await;

        mount(
            &venues.gamma,
            "/events",
            json!([{
                "slug": "fed-decision-in-october",
                "title": "Fed decision in October",
                "seriesSlug": "fomc",
                "markets": [{
                    "slug": "fed-decision-in-october",
                    "question": "Fed decision in October",
                    "outcomePrices": "[\"0.41\", \"0.59\"]",
                }],
            }]),
        )
        .await;

        mount(&venues.us, "/v1/events", json!({ "events": [] })).await;
    }

    async fn get_json(config: Config, uri: &str) -> (StatusCode, Value) {
        let app = crate::build_router(AppState::new(config));
        let response = app
            .oneshot(Request::get(uri).body(axum::body::Body::empty()).unwrap())
            .await
            .unwrap();

        let status = response.status();
        let bytes = to_bytes(response.into_body(), 8 * 1024 * 1024)
            .await
            .unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    async fn get_offline(uri: &str) -> (StatusCode, Value) {
        get_json(Config::default(), uri).await
    }

    /* ---------------------------------------------------------------- series */

    #[tokio::test]
    async fn the_board_opens_with_no_query_at_all() {
        // Unlike a venue search, no words is the default view rather than a
        // mistake: `XV` on its own lists everything that is linked.
        let venues = Venues::start().await;
        catalogues(&venues).await;

        let (status, body) = get_json(venues.config(), "/api/xv/series").await;

        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body["series"].is_array(), "{body}");
    }

    #[tokio::test]
    async fn a_query_narrows_the_board() {
        let venues = Venues::start().await;
        catalogues(&venues).await;

        let (status, body) = get_json(venues.config(), "/api/xv/series?q=fed").await;

        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(!body["series"].as_array().unwrap().is_empty(), "{body}");
    }

    #[tokio::test]
    async fn the_row_limit_defaults_to_forty_and_clamps_to_two_hundred() {
        // Read through the handler's own reader, because the clamp is the only
        // thing this route does to the number before passing it on.
        let read = |raw: &str| QueryParams::parse(raw).int_param("limit", 40, 1, 200);

        assert_eq!(read(""), 40);
        assert_eq!(read("limit="), 40);
        assert_eq!(read("limit=abc"), 40);
        assert_eq!(read("limit=5"), 5);
        assert_eq!(read("limit=0"), 1);
        assert_eq!(read("limit=-3"), 1);
        assert_eq!(read("limit=99999"), 200);
    }

    #[tokio::test]
    async fn the_limit_actually_cuts_the_board() {
        let venues = Venues::start().await;
        catalogues(&venues).await;

        let (status, body) = get_json(venues.config(), "/api/xv/series?limit=1").await;

        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body["series"].as_array().unwrap().len() <= 1);
    }

    /* --------------------------------------------------------------- compare */

    #[tokio::test]
    async fn a_comparison_with_no_event_says_what_to_type() {
        for uri in ["/api/xv/compare", "/api/xv/compare?event=%20"] {
            let (status, body) = get_offline(uri).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{uri}");
            assert_eq!(body["code"], codes::BAD_REQUEST, "{uri}");
            assert_eq!(body["error"], "Comparison needs an event", "{uri}");
            assert!(body["hint"].as_str().unwrap().contains("XV "), "{uri}");
        }
    }

    #[tokio::test]
    async fn an_unknown_venue_filter_is_a_bad_request_listing_the_real_ones() {
        let venues = Venues::start().await;
        let (status, body) = get_json(
            venues.config(),
            "/api/xv/compare?event=KXFEDDECISION-26OCT&venue=nyse",
        )
        .await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"], "Unknown venue \"nyse\"");
        assert_eq!(
            body["hint"],
            "Venues are: kalshi, polymarket, polymarket-us, gemini, predictfun, forecastex."
        );
        // Refused before any catalogue was crawled.
        assert_eq!(venues.requests().await, 0);
    }

    #[tokio::test]
    async fn an_absent_venue_filter_searches_every_index() {
        let venues = Venues::start().await;
        catalogues(&venues).await;

        let (status, body) =
            get_json(venues.config(), "/api/xv/compare?event=KXFEDDECISION-26OCT").await;

        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["events"][0]["eventTicker"], "KXFEDDECISION-26OCT");
    }

    #[tokio::test]
    async fn a_venue_filter_names_which_broker_the_ticker_belongs_to() {
        let venues = Venues::start().await;
        catalogues(&venues).await;

        let (status, body) = get_json(
            venues.config(),
            "/api/xv/compare?event=KXFEDDECISION-26OCT&venue=kalshi",
        )
        .await;

        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["events"][0]["venue"], "kalshi");
    }

    #[tokio::test]
    async fn a_blank_venue_filter_reads_as_no_filter() {
        // A panel that has not chosen a venue sends `venue=`; that must mean
        // "look everywhere", not "look at the venue named empty string".
        let venues = Venues::start().await;
        catalogues(&venues).await;

        let (status, body) = get_json(
            venues.config(),
            "/api/xv/compare?event=KXFEDDECISION-26OCT&venue=",
        )
        .await;

        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["events"][0]["eventTicker"], "KXFEDDECISION-26OCT");
    }

    #[tokio::test]
    async fn an_event_no_index_lists_is_not_found() {
        let venues = Venues::start().await;
        catalogues(&venues).await;

        let (status, body) =
            get_json(venues.config(), "/api/xv/compare?event=KXNOTHING-99DEC").await;

        assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
        assert_eq!(body["code"], codes::NOT_FOUND);
    }
}
