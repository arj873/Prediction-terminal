//! Implied-price routes.
//!
//! Three endpoints, matching the questions the panel asks: *which symbols can
//! be priced at all* (the registry), *which ladders price this one* (the
//! picker), and *what did one of them imply over time* (the overlay line).

use axum::extract::{RawQuery, State};
use axum::routing::get;
use axum::{Json, Router};
use terminal_core::types::{
    CandleInterval, ImpliedCandidatesResponse, ImpliedMethod, ImpliedSeriesResponse,
    ImpliedUnderlying, ImpliedUnderlyingsResponse,
};

use crate::app::AppState;
use crate::error::{Result, UpstreamError};
use crate::routes::helpers::{candle_window, now_seconds, snap_to_grid, QueryParams};
use crate::sources::implied;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/implied/underlyings", get(underlyings))
        .route("/api/implied/candidates", get(candidates))
        .route("/api/implied/series", get(series))
}

/// Why those two ways of collapsing a ladder, and what each costs.
const METHOD_HINT: &str =
    "`median` reads the 50% crossing and needs no assumption beyond the quoted \
     strikes. `mean` weights the whole distribution but has to guess at the tails.";

/// Read the `method` parameter.
fn method_param(params: &QueryParams) -> Result<ImpliedMethod> {
    match params.str_param("method", "median").as_str() {
        "median" => Ok(ImpliedMethod::Median),
        "mean" => Ok(ImpliedMethod::Mean),
        other => Err(
            UpstreamError::bad_request(format!("Unknown method \"{other}\""))
                .with_hint(METHOD_HINT),
        ),
    }
}

/// The overlay window, floored onto a 15-second grid.
///
/// Two panels polling the same overlay a second apart would otherwise ask for
/// two windows differing only in their last second, miss the cache, and each
/// re-run the whole strike fan-out — which is up to 48 candle requests. Nothing
/// is lost: no candle bucket is shorter than a minute.
///
/// The emptiness check runs *after* the snap, because a sub-grid window that
/// was legal before it survives as `start == end` afterwards. The same reader
/// lives in [`crate::routes::spot`], for the same reason.
fn snapped_window(params: &QueryParams, interval: CandleInterval, now: i64) -> Result<(i64, i64)> {
    let (start_ts, end_ts) = candle_window(params, interval, snap_to_grid(now))?;
    let (start_ts, end_ts) = (snap_to_grid(start_ts), snap_to_grid(end_ts));

    if start_ts >= end_ts {
        return Err(UpstreamError::bad_request("`start` must be before `end`"));
    }
    Ok((start_ts, end_ts))
}

/// `GET /api/implied/underlyings`
///
/// Every symbol with a mapped ladder — powers `HELP IMP` and symbol validation.
/// Static: the registry is compiled in, so this reads no upstream.
async fn underlyings() -> Json<ImpliedUnderlyingsResponse> {
    Json(ImpliedUnderlyingsResponse {
        underlyings: implied::list_underlyings()
            .iter()
            .map(|u| ImpliedUnderlying {
                symbol: u.symbol.to_string(),
                name: u.name.to_string(),
                asset_class: u.asset_class,
                aliases: u.aliases.iter().map(|a| (*a).to_string()).collect(),
            })
            .collect(),
    })
}

/// `GET /api/implied/candidates?symbol`
async fn candidates(
    State(state): State<AppState>,
    RawQuery(raw): RawQuery,
) -> Result<Json<ImpliedCandidatesResponse>> {
    let params = QueryParams::parse(raw.as_deref().unwrap_or_default());

    let symbol = params.string("symbol");
    if symbol.is_empty() {
        return Err(UpstreamError::bad_request("Which underlying?")
            .with_hint("Usage: `IMP <symbol>`, e.g. `IMP BTC`."));
    }

    Ok(Json(implied::get_candidates(&state, &symbol).await?))
}

/// `GET /api/implied/series?event&interval&method&start&end`
async fn series(
    State(state): State<AppState>,
    RawQuery(raw): RawQuery,
) -> Result<Json<ImpliedSeriesResponse>> {
    let params = QueryParams::parse(raw.as_deref().unwrap_or_default());

    // Kalshi shouts its tickers, and 404s one that does not.
    let event_ticker = params.string("event").to_uppercase();
    if event_ticker.is_empty() {
        return Err(UpstreamError::bad_request("Which Kalshi event?")
            .with_hint("Pass `event=<event-ticker>`, e.g. `event=KXBTCD-26AUG1617`."));
    }

    let interval = params.interval()?;
    let method = method_param(&params)?;
    let (start_ts, end_ts) = snapped_window(&params, interval, now_seconds())?;

    Ok(Json(
        implied::get_implied_series(&state, &event_ticker, interval, start_ts, end_ts, method)
            .await?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    use axum::body::to_bytes;
    use axum::http::{Request, StatusCode};
    use serde_json::{json, Value};
    use tower::ServiceExt;
    use wiremock::matchers::{method as http_method, path, path_regex};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use crate::config::Config;
    use crate::error::codes;

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

    /// One three-rung BTC ladder, quoted, with an empty candle page per rung.
    ///
    /// Three is the floor: an implied price needs enough strikes to bracket the
    /// crossing.
    async fn ladder(server: &MockServer) {
        Mock::given(http_method("GET"))
            .and(path("/events/KXBTCD-26AUG1617"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "event": {
                    "event_ticker": "KXBTCD-26AUG1617",
                    "series_ticker": "KXBTCD",
                    "title": "Bitcoin price on Aug 16",
                },
                "markets": [
                    { "ticker": "KXBTCD-26AUG1617-T60000", "strike_type": "greater_or_equal",
                      "floor_strike": 60000, "yes_bid_dollars": "0.80", "yes_ask_dollars": "0.82",
                      "volume_fp": "100.00" },
                    { "ticker": "KXBTCD-26AUG1617-T61000", "strike_type": "greater_or_equal",
                      "floor_strike": 61000, "yes_bid_dollars": "0.50", "yes_ask_dollars": "0.52",
                      "volume_fp": "100.00" },
                    { "ticker": "KXBTCD-26AUG1617-T62000", "strike_type": "greater_or_equal",
                      "floor_strike": 62000, "yes_bid_dollars": "0.20", "yes_ask_dollars": "0.22",
                      "volume_fp": "100.00" },
                ],
            })))
            .mount(server)
            .await;

        Mock::given(http_method("GET"))
            .and(path_regex(r".*/candlesticks$"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "candlesticks": [] })))
            .mount(server)
            .await;
    }

    /* ----------------------------------------------------------- underlyings */

    #[tokio::test]
    async fn the_underlying_list_is_the_static_registry_and_reads_no_upstream() {
        let server = MockServer::start().await;
        let (status, body) = get_json(config(&server), "/api/implied/underlyings").await;

        assert_eq!(status, StatusCode::OK, "{body}");
        let rows = body["underlyings"].as_array().unwrap();
        assert_eq!(rows.len(), implied::list_underlyings().len());

        let btc = rows
            .iter()
            .find(|row| row["symbol"] == "BTC")
            .expect("BTC is in the registry");
        assert_eq!(btc["name"], "Bitcoin");
        assert_eq!(btc["assetClass"], "crypto");
        assert!(btc["aliases"]
            .as_array()
            .unwrap()
            .contains(&json!("BITCOIN")));

        assert_eq!(server.received_requests().await.unwrap().len(), 0);
    }

    /* ------------------------------------------------------------ candidates */

    #[tokio::test]
    async fn candidates_without_a_symbol_asks_which_underlying() {
        for uri in ["/api/implied/candidates", "/api/implied/candidates?symbol="] {
            let (status, body) = get_offline(uri).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{uri}");
            assert_eq!(body["code"], codes::BAD_REQUEST, "{uri}");
            assert_eq!(body["error"], "Which underlying?", "{uri}");
            assert!(body["hint"].as_str().unwrap().contains("IMP BTC"), "{uri}");
        }
    }

    #[tokio::test]
    async fn an_unmapped_symbol_is_an_empty_answer_rather_than_a_failure() {
        // The endpoint exists, the question is well-formed, and the answer is
        // "none" — a 404 here made every plain stock chart log a failed request
        // while rendering fine.
        let server = MockServer::start().await;
        let (status, body) = get_json(config(&server), "/api/implied/candidates?symbol=AAPL").await;

        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["symbol"], "AAPL");
        assert_eq!(body["candidates"].as_array().unwrap().len(), 0);
        assert!(body["note"].as_str().unwrap().contains("BTC"));
    }

    /* ---------------------------------------------------------------- series */

    #[tokio::test]
    async fn series_without_an_event_asks_which_one() {
        for uri in ["/api/implied/series", "/api/implied/series?event=%20"] {
            let (status, body) = get_offline(uri).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{uri}");
            assert_eq!(body["error"], "Which Kalshi event?", "{uri}");
            assert!(body["hint"].as_str().unwrap().contains("event="), "{uri}");
        }
    }

    #[tokio::test]
    async fn the_event_ticker_is_upper_cased_before_it_reaches_kalshi() {
        // Kalshi 404s a lower-case ticker, so a prompt that shouts nothing must
        // still reach the ladder.
        let server = MockServer::start().await;
        ladder(&server).await;

        let (status, body) = get_json(
            config(&server),
            "/api/implied/series?event=kxbtcd-26aug1617",
        )
        .await;

        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["eventTicker"], "KXBTCD-26AUG1617");
    }

    #[tokio::test]
    async fn the_method_defaults_to_median_and_reads_the_two_it_knows() {
        let server = MockServer::start().await;
        ladder(&server).await;

        for (uri, expected) in [
            ("/api/implied/series?event=KXBTCD-26AUG1617", "median"),
            (
                "/api/implied/series?event=KXBTCD-26AUG1617&method=median",
                "median",
            ),
            (
                "/api/implied/series?event=KXBTCD-26AUG1617&method=mean",
                "mean",
            ),
        ] {
            let (status, body) = get_json(config(&server), uri).await;
            assert_eq!(status, StatusCode::OK, "{uri}: {body}");
            assert_eq!(body["method"], expected, "{uri}");
        }
    }

    #[tokio::test]
    async fn an_unknown_method_explains_what_the_two_cost() {
        let (status, body) =
            get_offline("/api/implied/series?event=KXBTCD-26AUG1617&method=mode").await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"], "Unknown method \"mode\"");
        assert_eq!(body["hint"], METHOD_HINT);
    }

    #[tokio::test]
    async fn an_interval_off_the_three_buckets_is_refused_with_the_hint() {
        for uri in [
            "/api/implied/series?event=KXBTCD-26AUG1617&interval=5",
            "/api/implied/series?event=KXBTCD-26AUG1617&interval=0",
        ] {
            let (status, body) = get_offline(uri).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{uri}");
            assert_eq!(body["hint"], crate::routes::helpers::INTERVAL_HINT, "{uri}");
        }
    }

    #[tokio::test]
    async fn the_interval_reaches_the_upstream_candle_request() {
        let server = MockServer::start().await;
        ladder(&server).await;

        let (status, body) = get_json(
            config(&server),
            "/api/implied/series?event=KXBTCD-26AUG1617&interval=1",
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["interval"], 1);

        let requests = server.received_requests().await.unwrap();
        let candles = requests
            .iter()
            .find(|r| r.url.path().ends_with("/candlesticks"))
            .expect("the fan-out read a rung");
        let period = candles
            .url
            .query_pairs()
            .find(|(key, _)| key == "period_interval")
            .unwrap()
            .1
            .into_owned();
        assert_eq!(period, "1");
    }

    /* ---------------------------------------------------------- the window */

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
        for raw in [
            "start=1700000000&end=1600000000",
            // Equal bounds are empty, and equally a mistake.
            "start=1700000000&end=1700000000",
            // Legal before the snap, empty after it.
            "start=1700000001&end=1700000004",
        ] {
            let err = snapped_window(
                &QueryParams::parse(raw),
                CandleInterval::OneHour,
                1_700_000_100,
            )
            .expect_err(raw);
            assert_eq!(err.code, codes::BAD_REQUEST, "{raw}");
        }
    }

    #[test]
    fn every_instant_in_one_grid_cell_produces_the_same_window() {
        // The cache key for an overlay is built from these two numbers. If they
        // move with the clock, the TTL exists but never fires and each poll
        // re-runs the whole strike fan-out.
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
        assert_ne!(
            snapped_window(&QueryParams::parse(""), CandleInterval::OneHour, cell + 15).unwrap(),
            first
        );
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
    async fn three_polls_inside_one_cell_share_one_cache_key() {
        let server = MockServer::start().await;
        ladder(&server).await;

        // One router, one cache — as a running server has.
        let app = crate::build_router(AppState::new(config(&server)));
        for end in [1_700_000_000, 1_700_000_004, 1_700_000_009] {
            let response = app
                .clone()
                .oneshot(
                    Request::get(format!(
                        "/api/implied/series?event=KXBTCD-26AUG1617&end={end}"
                    ))
                    .body(axum::body::Body::empty())
                    .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK, "end={end}");
        }

        // One event lookup plus one candle page per rung. A second poll that
        // missed the cache would re-run the whole fan-out.
        assert_eq!(server.received_requests().await.unwrap().len(), 4);
    }
}
