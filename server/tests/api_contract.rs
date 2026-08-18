//! The contract the client is written against.
//!
//! Not a test of any one feed — those live beside their sources. This covers the
//! things every panel depends on and no single route owns: what a failure looks
//! like on the wire, which HTTP status each failure code becomes, that a typo'd
//! `/api` path reads as a missing route rather than as the SPA, what the rate
//! limiter says when it refuses, and that the artwork proxy cannot be talked
//! into fetching something it should not.
//!
//! Everything runs against [`terminal_server::build_router`] with a [`Config`]
//! pointing at a fixture server, so no test here touches the network.

use std::path::PathBuf;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use axum::routing::get;
use axum::{Json, Router};
use serde_json::Value;
use tower::ServiceExt;
use wiremock::matchers::{method, path, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

use terminal_server::config::Config;
use terminal_server::error::{codes, UpstreamError};
use terminal_server::routes::billboard::{art_http, fetch_art};
use terminal_server::{build_router, AppState};

/* ------------------------------------------------------------------ harness */

/// A config that reaches nothing. Every base still points at its real host, so
/// a test that accidentally makes a request will hang rather than pass quietly —
/// the tests below either fail before any fetch, or override the one base they
/// exercise.
fn config() -> Config {
    Config {
        // The news feed's credentials are the one thing several cases care
        // about, and "unset" is the interesting state.
        alpaca: None,
        fred_api_key: None,
        ..Config::default()
    }
}

fn app(config: Config) -> Router {
    build_router(AppState::new(config))
}

async fn get_status_and_body(app: &Router, uri: &str) -> (StatusCode, Vec<u8>) {
    let response = app
        .clone()
        .oneshot(
            Request::get(uri)
                .body(Body::empty())
                .expect("a valid request"),
        )
        .await
        .expect("the router is infallible");

    let status = response.status();
    let bytes = to_bytes(response.into_body(), 8 * 1024 * 1024)
        .await
        .expect("a readable body");
    (status, bytes.to_vec())
}

/// Request `uri` and read the JSON body.
async fn get_json(app: &Router, uri: &str) -> (StatusCode, Value) {
    let (status, bytes) = get_status_and_body(app, uri).await;
    let body = serde_json::from_slice(&bytes).unwrap_or_else(|err| {
        panic!(
            "{uri} answered with something that is not JSON ({err}): {}",
            String::from_utf8_lossy(&bytes)
        )
    });
    (status, body)
}

/// A scratch directory that cleans itself up, standing in for a built client.
struct TempDir(PathBuf);

impl TempDir {
    fn with_an_index_html(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!("terminal-contract-{name}"));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("a scratch directory");
        std::fs::write(
            path.join("index.html"),
            "<!doctype html><title>terminal</title>",
        )
        .expect("an index.html");
        TempDir(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/* ------------------------------------------------------------- error ladder */

/// A router that fails on demand, one rung per code.
///
/// The ladder has to be exercised whole, and two of its rungs have no route that
/// can reach them from a fixture server: `unsupported` belongs to venues whose
/// routes ask a live venue, and `upstream_timeout` needs an upstream that
/// accepts a connection and then says nothing for tens of seconds. Driving a
/// handler with the real signature — `terminal_server::Result<Json<_>>`, the
/// same conversion axum performs for every route in the server — covers those
/// two without a test that sleeps or a route that lies. The rungs that *are*
/// reachable are then asserted again through the real API below.
fn ladder() -> Router {
    async fn fail(
        axum::extract::Path(code): axum::extract::Path<String>,
    ) -> terminal_server::Result<Json<Value>> {
        Err(UpstreamError::new("deliberate failure", code))
    }

    Router::new().route("/fail/{code}", get(fail))
}

#[tokio::test]
async fn every_error_code_becomes_its_own_http_status() {
    let app = ladder();

    for (code, expected) in [
        (codes::BAD_REQUEST, StatusCode::BAD_REQUEST),
        (codes::NOT_FOUND, StatusCode::NOT_FOUND),
        (codes::UNSUPPORTED, StatusCode::NOT_IMPLEMENTED),
        (codes::NOT_CONFIGURED, StatusCode::SERVICE_UNAVAILABLE),
        (codes::UPSTREAM_TIMEOUT, StatusCode::GATEWAY_TIMEOUT),
        (codes::RATE_LIMITED, StatusCode::TOO_MANY_REQUESTS),
        (codes::INTERNAL, StatusCode::INTERNAL_SERVER_ERROR),
    ] {
        let (status, body) = get_json(&app, &format!("/fail/{code}")).await;
        assert_eq!(status, expected, "{code}");
        assert_eq!(body["code"], code);
    }
}

#[tokio::test]
async fn anything_else_is_a_bad_gateway() {
    // The terminal asked an upstream a question and did not get a usable
    // answer, which is a bad *gateway* rather than a broken server.
    let app = ladder();

    for code in [
        codes::UPSTREAM_STATUS,
        codes::UPSTREAM_BLOCKED,
        codes::UPSTREAM_ERROR,
        codes::BAD_UPSTREAM_BODY,
        codes::PARSE_FAILED,
        codes::BAD_CREDENTIALS,
        codes::EMPTY_UPSTREAM,
        codes::RESPONSE_TOO_LARGE,
        "something_nobody_has_seen_yet",
    ] {
        let (status, body) = get_json(&app, &format!("/fail/{code}")).await;
        assert_eq!(status, StatusCode::BAD_GATEWAY, "{code}");
        assert_eq!(body["code"], code);
    }
}

/* ------------------------------------------------ the ladder, through routes */

#[tokio::test]
async fn a_malformed_query_is_a_bad_request_with_a_hint_the_client_shows_verbatim() {
    let app = app(config());

    let (status, body) = get_json(&app, "/api/fred/series/UNRATE?start=yesterday").await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "\"yesterday\" is not a valid start date");
    assert_eq!(body["code"], "bad_request");
    // Shown to the reader exactly as written, so it is asserted exactly.
    assert_eq!(
        body["hint"],
        "Dates are YYYY-MM-DD, e.g. `FRED UNRATE 2015-01-01`."
    );
}

#[tokio::test]
async fn an_error_body_carries_only_the_fields_it_has() {
    // `hint` and `status` are optional and omitted rather than sent as null: the
    // client tests for their presence.
    let app = app(config());

    let (status, body) = get_json(&app, "/api/fred/search?q=").await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        body,
        serde_json::json!({
            "error": "Search needs at least one word",
            "code": "bad_request",
        })
    );
}

#[tokio::test]
async fn an_upstream_404_reaches_the_client_as_a_404() {
    let upstream = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path_regex(r"^/charts/hot-100/?$"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&upstream)
        .await;

    let app = app(Config {
        billboard_base: upstream.uri(),
        ..config()
    });

    let (status, body) = get_json(&app, "/api/billboard/chart/hot-100").await;

    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["code"], "not_found");
}

#[tokio::test]
async fn an_unhelpful_upstream_status_is_a_bad_gateway_that_reports_it() {
    let upstream = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path_regex(r"^/charts/hot-100/?$"))
        .respond_with(ResponseTemplate::new(403))
        .mount(&upstream)
        .await;

    let app = app(Config {
        billboard_base: upstream.uri(),
        ..config()
    });

    let (status, body) = get_json(&app, "/api/billboard/chart/hot-100").await;

    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(body["code"], "upstream_status");
    // The upstream's status is reported, not adopted.
    assert_eq!(body["status"], 403);
}

#[tokio::test]
async fn the_news_feed_without_credentials_is_not_configured() {
    let app = app(config());

    let (status, body) = get_json(&app, "/api/news?symbols=NVDA").await;

    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["code"], "not_configured");
    assert!(
        body["hint"]
            .as_str()
            .is_some_and(|hint| hint.contains("ALPACA_API_KEY_ID")),
        "the hint should name the variables to set: {}",
        body["hint"]
    );
}

#[tokio::test]
async fn a_symbol_the_feed_cannot_filter_by_is_refused_before_the_credentials_are_read() {
    // Order matters: a mistyped symbol is the caller's bug whether or not the
    // server has keys, and "not configured" would send them looking in the
    // wrong place.
    let app = app(config());

    let (status, body) = get_json(&app, "/api/news?symbols=nvda!").await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "bad_request");
}

/* ------------------------------------------------------------------- health */

#[tokio::test]
async fn health_reports_liveness_credentials_and_the_cache() {
    let app = app(config());

    let (status, body) = get_json(&app, "/api/health").await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["ok"], true);
    assert!(body["uptimeSeconds"].is_f64() || body["uptimeSeconds"].is_i64());
    assert_eq!(body["fredApiKey"], false);
    assert_eq!(body["alpacaKeys"], false);
    // An RFC 3339 instant, so an operator can tell a stale tab from a live one.
    assert!(
        body["time"].as_str().is_some_and(|time| time.contains('T')),
        "{}",
        body["time"]
    );

    let cache = &body["cache"];
    for field in ["hits", "misses", "entries", "evictions"] {
        assert!(cache[field].is_u64(), "cache.{field}: {cache}");
    }
}

#[tokio::test]
async fn health_sees_the_credentials_when_they_are_set() {
    let app = app(Config {
        fred_api_key: Some("key".into()),
        alpaca: Some(terminal_server::config::AlpacaKeys {
            key_id: "id".into(),
            secret_key: "secret".into(),
        }),
        ..config()
    });

    let (_, body) = get_json(&app, "/api/health").await;
    assert_eq!(body["fredApiKey"], true);
    assert_eq!(body["alpacaKeys"], true);
}

/* ------------------------------------------------------------- unknown route */

/// A path under `/api` that names no route is a missing route, not a page.
///
/// Asserted with a client build present, because that is the arrangement where
/// it can break: the SPA fallback catches everything the API did not claim, and
/// a mistyped fetch answered with `index.html` fails to parse somewhere else
/// entirely, three layers from the typo.
#[tokio::test]
async fn an_unknown_api_path_is_json_and_not_the_client() {
    let client = TempDir::with_an_index_html("unknown-api-path");
    let app = app(Config {
        client_dir: Some(client.0.clone()),
        ..config()
    });

    let (status, body) = get_json(&app, "/api/fred/nope").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["code"], "not_found");
    assert_eq!(body["error"], "No such API route");

    // A path with no prefix in common with any route, for the same answer.
    let (status, body) = get_json(&app, "/api/definitely-not-a-route/at/all").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["code"], "not_found");

    // And the SPA fallback really is mounted — otherwise the assertions above
    // would be passing for the wrong reason.
    let (status, bytes) = get_status_and_body(&app, "/some/deep/panel/route").await;
    assert_eq!(status, StatusCode::OK);
    assert!(String::from_utf8_lossy(&bytes).contains("<!doctype html>"));
}

/* --------------------------------------------------------------- rate limit */

#[tokio::test]
async fn the_rate_limiter_refuses_with_a_body_the_client_can_read() {
    let app = app(Config {
        rate_limit: 3,
        ..config()
    });

    for attempt in 1..=3 {
        let (status, _) = get_json(&app, "/api/health").await;
        assert_eq!(status, StatusCode::OK, "call {attempt} is inside the cap");
    }

    let (status, body) = get_json(&app, "/api/health").await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(body["code"], "rate_limited");
    assert_eq!(body["error"], "Too many requests");
    assert_eq!(body["hint"], "This client exceeded 3 API calls per minute.");
}

/* ------------------------------------------------------------ artwork proxy */

/// Every refusal the allowlist makes, through the real route.
///
/// This is the only endpoint that fetches a URL the caller chose, so the
/// interesting assertion is that none of these reach the network at all.
#[tokio::test]
async fn the_artwork_proxy_refuses_anything_off_the_allowlist() {
    let app = app(config());

    for (url, why) in [
        // Not https: the allowlist is about who answers, and plain HTTP means
        // anyone on the path can.
        (
            "http://charts-static.billboard.com/art.jpg",
            "plain http is not allowlisted",
        ),
        // A look-alike registered by someone else.
        ("https://evil-billboard.com/art.jpg", "a look-alike host"),
        // The suffix attack the exact-match check exists to stop.
        (
            "https://charts-static.billboard.com.evil.test/art.jpg",
            "a suffix of an allowlisted host is not that host",
        ),
        // A host-less scheme, which reads as no host at all.
        ("file:///etc/passwd", "a host-less scheme"),
        // Link-local metadata, the reason any of this matters.
        ("https://169.254.169.254/latest/meta-data/", "link-local"),
    ] {
        let (status, body) = get_json(&app, &format!("/api/billboard/art?u={url}")).await;

        assert_eq!(status, StatusCode::BAD_REQUEST, "{why}: {url}");
        assert_eq!(body["code"], "bad_request", "{why}: {url}");
    }

    // No `u` at all is the same refusal, not a panic.
    let (status, body) = get_json(&app, "/api/billboard/art").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "Artwork URL is not a valid URL");
}

/// The fetch half of the proxy, exercised against a fixture.
///
/// It cannot be reached through the route: the allowlist admits only `https://`
/// billboard.com hosts, so no fixture server can stand in for one — which is
/// exactly the property the test above asserts. [`fetch_art`] is therefore
/// called directly, with the same client the route uses.
#[tokio::test]
async fn the_artwork_proxy_will_not_follow_a_redirect() {
    // A followed redirect would be resolved *after* the allowlist check, so an
    // allowlisted host could bounce the request anywhere it liked and the
    // allowlist would have decided nothing.
    let upstream = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/art.jpg"))
        .respond_with(
            ResponseTemplate::new(302)
                .insert_header("location", "http://169.254.169.254/latest/meta-data/"),
        )
        .expect(1)
        .mount(&upstream)
        .await;

    let err = fetch_art(art_http(), &format!("{}/art.jpg", upstream.uri()))
        .await
        .expect_err("a redirect is not artwork");

    assert_eq!(err.code, codes::UPSTREAM_STATUS);
    assert_eq!(err.status, Some(302));
}

#[tokio::test]
async fn the_artwork_proxy_refuses_a_body_that_is_not_an_image() {
    let upstream = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_raw("<!doctype html>", "text/html"))
        .mount(&upstream)
        .await;

    let err = fetch_art(art_http(), &format!("{}/art.jpg", upstream.uri()))
        .await
        .expect_err("HTML is not artwork");

    assert_eq!(err.code, codes::BAD_UPSTREAM_BODY);
    assert_eq!(err.message, "Artwork URL did not return an image");
}

#[tokio::test]
async fn the_artwork_proxy_refuses_an_oversized_body() {
    // Chart thumbnails are tens of kilobytes; three megabytes is already far
    // more room than any of them need.
    let upstream = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(vec![b'x'; 3 * 1024 * 1024 + 1], "image/jpeg"),
        )
        .mount(&upstream)
        .await;

    let err = fetch_art(art_http(), &format!("{}/fat.jpg", upstream.uri()))
        .await
        .expect_err("over the 3 MiB cap");

    assert_eq!(err.code, codes::RESPONSE_TOO_LARGE);
}

#[tokio::test]
async fn the_artwork_proxy_passes_an_image_through() {
    let upstream = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(vec![0xffu8, 0xd8, 0xff], "image/jpeg"),
        )
        .mount(&upstream)
        .await;

    let (bytes, content_type) = fetch_art(art_http(), &format!("{}/art.jpg", upstream.uri()))
        .await
        .expect("a JPEG is artwork");

    assert_eq!(bytes, vec![0xffu8, 0xd8, 0xff]);
    assert_eq!(content_type, "image/jpeg");
}

/* ------------------------------------------------------- the constant lists */

#[tokio::test]
async fn the_static_chart_lists_need_no_upstream() {
    // Both exist so the client can offer completions without a round trip, so
    // both must answer with every base pointing at hosts this test cannot reach.
    let app = app(config());

    let (status, body) = get_json(&app, "/api/billboard/charts").await;
    assert_eq!(status, StatusCode::OK);
    let charts = body["charts"].as_array().expect("an array of charts");
    assert!(charts.iter().any(|chart| chart["slug"] == "hot-100"));
    assert!(charts.iter().all(|chart| chart["name"].is_string()));

    let (status, body) = get_json(&app, "/api/ent/charts?source=spotify").await;
    assert_eq!(status, StatusCode::OK);
    let charts = body["charts"].as_array().expect("an array of charts");
    assert!(!charts.is_empty());
    assert!(charts.iter().all(|chart| chart["source"] == "spotify"));

    // An unrecognised source lists everything: this is the endpoint a client
    // calls to find out what exists.
    let (_, body) = get_json(&app, "/api/ent/charts?source=napster").await;
    let all = body["charts"].as_array().expect("an array of charts");
    assert!(all.len() > charts.len());
}
