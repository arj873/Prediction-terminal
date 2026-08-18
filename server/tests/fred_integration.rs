//! End-to-end exercise of the FRED scrape path against a local fixture server.
//!
//! The live host (fred.stlouisfed.org) resets connections from datacentre IPs,
//! so the real network path cannot be exercised in CI. Pointing `fred_web_base`
//! at a fixture server covers everything except the socket itself: URL
//! construction, the CSV/metadata fan-out, the "HTML instead of CSV" not-found
//! case, and the assembly of the response the client consumes.
//!
//! It also covers the thing the scrape alone cannot: the two-arm provider
//! chain. The API arm exists only where a key is configured, and when it is not
//! configured a blocked scrape is annotated with the way out — which is the
//! failure a hosted deployment actually hits.

use terminal_core::types::FredSource;
use terminal_server::codes;
use terminal_server::config::Config;
use terminal_server::sources::fred::{get_series, search_series, DEFAULT_SEARCH_LIMIT};
use terminal_server::AppState;
use wiremock::matchers::{method, path, path_regex, query_param};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const CSV: &str = "observation_date,UNRATE
2024-01-01,3.7
2024-02-01,3.9
2024-03-01,.
2024-04-01,3.9
";

const SERIES_HTML: &str = r#"<!doctype html><html><head>
<title>Unemployment Rate (UNRATE) | FRED | St. Louis Fed</title></head><body>
<h1 id="page-title">Unemployment Rate <span>(UNRATE)</span></h1>
<p><span class="series-meta-label">Units:</span> <span class="series-meta-value">Percent, Seasonally Adjusted</span></p>
<p><span class="series-meta-label">Frequency:</span> <span class="series-meta-value">Monthly</span></p>
<div class="series-obs-range">1948-01-01 to 2024-04-01</div>
<div id="notes-container">Fixture notes.</div>
</body></html>"#;

const SEARCH_HTML: &str = r#"<html><body>
<div><a href="/series/UNRATE">Unemployment Rate</a><div class="series-meta">Percent, Monthly, Seasonally Adjusted</div></div>
<div><a href="/series/U6RATE">Broader unemployment</a></div>
</body></html>"#;

const NOT_FOUND_HTML: &str = r#"<!doctype html><html><body><h1>Page not found</h1></body></html>"#;

/// What an egress proxy says when the origin hung up before answering. This is
/// the shape of the failure that makes the API arm worth having at all.
const CONNECTION_RESET: &str =
    "upstream connect error or disconnect/reset before headers. reset reason: remote reset";

const API_SERIES: &str = r#"{"seriess":[{"id":"UNRATE","title":"Unemployment Rate",
"units":"Percent","units_short":"%","frequency":"Monthly",
"seasonal_adjustment":"Seasonally Adjusted","last_updated":"2026-08-01 07:46:02-05",
"observation_start":"1948-01-01","observation_end":"2026-07-01","notes":"From the API."}]}"#;

const API_OBSERVATIONS: &str = r#"{"observations":[
{"date":"2024-01-01","value":"3.7"},
{"date":"2024-02-01","value":"."}]}"#;

const API_SEARCH: &str = r#"{"seriess":[{"id":"UNRATE","title":"Unemployment Rate",
"units":"Percent","units_short":"%","frequency":"Monthly",
"seasonal_adjustment":"Seasonally Adjusted","last_updated":"2026-08-01 07:46:02-05",
"observation_start":"1948-01-01","observation_end":"2026-07-01"}]}"#;

/// The fixture host: the CSV endpoint, the series page and the search page,
/// with a bad id answered the way FRED answers one — an HTML page, not a 404.
async fn fixture_host() -> MockServer {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/graph/fredgraph.csv"))
        .and(query_param("id", "NOSUCH"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(NOT_FOUND_HTML, "text/html"))
        .with_priority(1)
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/graph/fredgraph.csv"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(CSV, "text/csv"))
        .with_priority(2)
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path_regex(r"^/series/.+"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(SERIES_HTML, "text/html"))
        .with_priority(2)
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/searchresults/"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(SEARCH_HTML, "text/html"))
        .with_priority(2)
        .mount(&server)
        .await;

    server
}

/// A host that hangs up on everything, the way fred.stlouisfed.org treats a
/// datacentre IP.
async fn blocked_host() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(503).set_body_string(CONNECTION_RESET))
        .mount(&server)
        .await;
    server
}

/// api.stlouisfed.org, which answers the same deployments the web host refuses.
async fn api_host() -> MockServer {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/series"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(API_SERIES, "application/json"))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/series/observations"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(API_OBSERVATIONS, "application/json"))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/series/search"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(API_SEARCH, "application/json"))
        .mount(&server)
        .await;

    server
}

fn state_for(server: &MockServer) -> AppState {
    AppState::new(Config {
        fred_web_base: server.uri(),
        ..Config::default()
    })
}

async fn requests(server: &MockServer) -> Vec<Request> {
    server.received_requests().await.unwrap_or_default()
}

/// `{path}{?query}`, the shape the TypeScript fixture server recorded.
fn line_of(request: &Request) -> String {
    match request.url.query() {
        Some(query) => format!("{}?{query}", request.url.path()),
        None => request.url.path().to_owned(),
    }
}

async fn lines(server: &MockServer) -> Vec<String> {
    requests(server).await.iter().map(line_of).collect()
}

fn param(request: &Request, name: &str) -> Option<String> {
    request
        .url
        .query_pairs()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.into_owned())
}

/* ----------------------------------------- getSeries against a fixture host */

#[tokio::test]
async fn returns_observations_with_the_missing_marker_preserved_as_null() {
    let server = fixture_host().await;
    let state = state_for(&server);

    let result = get_series(&state, "UNRATE", None, None)
        .await
        .expect("the fixture host answers");

    assert_eq!(
        result
            .observations
            .iter()
            .map(|o| (o.date.as_str(), o.value))
            .collect::<Vec<_>>(),
        [
            ("2024-01-01", Some(3.7)),
            ("2024-02-01", Some(3.9)),
            // The `.` marker is missing data, not a reading of zero.
            ("2024-03-01", None),
            ("2024-04-01", Some(3.9)),
        ]
    );
}

#[tokio::test]
async fn merges_metadata_scraped_from_the_series_page() {
    let server = fixture_host().await;
    let state = state_for(&server);

    let series = &get_series(&state, "UNRATE", None, None)
        .await
        .expect("the fixture host answers")
        .series;

    assert_eq!(series.id, "UNRATE");
    assert_eq!(series.title, "Unemployment Rate");
    assert_eq!(series.units, "Percent");
    assert_eq!(series.frequency, "Monthly");
    assert_eq!(series.source, FredSource::Scrape);
    assert!(series.notes.contains("Fixture notes"), "{}", series.notes);
}

#[tokio::test]
async fn requests_the_csv_endpoint_with_the_id_and_date_window() {
    let server = fixture_host().await;
    let state = state_for(&server);

    get_series(&state, "GDPC1", Some("2020-01-01"), Some("2021-01-01"))
        .await
        .expect("the fixture host answers");

    let sent = requests(&server).await;
    let csv_request = sent
        .iter()
        .find(|request| request.url.path() == "/graph/fredgraph.csv")
        .expect("expected a fredgraph.csv request");

    assert_eq!(param(csv_request, "id").as_deref(), Some("GDPC1"));
    assert_eq!(param(csv_request, "cosd").as_deref(), Some("2020-01-01"));
    assert_eq!(param(csv_request, "coed").as_deref(), Some("2021-01-01"));
}

#[tokio::test]
async fn omits_the_date_window_when_no_window_was_asked_for() {
    let server = fixture_host().await;
    let state = state_for(&server);

    get_series(&state, "UNRATE", None, None)
        .await
        .expect("the fixture host answers");

    let sent = requests(&server).await;
    let csv_request = sent
        .iter()
        .find(|request| request.url.path() == "/graph/fredgraph.csv")
        .expect("expected a fredgraph.csv request");

    assert_eq!(param(csv_request, "cosd"), None);
    assert_eq!(param(csv_request, "coed"), None);
}

#[tokio::test]
async fn fetches_the_csv_and_the_series_page_together() {
    let server = fixture_host().await;
    let state = state_for(&server);

    get_series(&state, "UNRATE", None, None)
        .await
        .expect("the fixture host answers");

    let sent = lines(&server).await;
    assert!(
        sent.iter()
            .any(|line| line.starts_with("/graph/fredgraph.csv")),
        "{sent:?}"
    );
    assert!(sent.iter().any(|line| line == "/series/UNRATE"), "{sent:?}");

    // The CSV request carries the headers the download button sends, including
    // the Referer that names the page it came from.
    let raw = requests(&server).await;
    let csv_request = raw
        .iter()
        .find(|request| request.url.path() == "/graph/fredgraph.csv")
        .expect("expected a fredgraph.csv request");
    assert_eq!(
        csv_request
            .headers
            .get("accept")
            .and_then(|value| value.to_str().ok()),
        Some("text/csv,text/plain,*/*")
    );
    assert_eq!(
        csv_request
            .headers
            .get("referer")
            .and_then(|value| value.to_str().ok()),
        Some(format!("{}/series/UNRATE", server.uri()).as_str())
    );
}

#[tokio::test]
async fn upper_cases_the_series_id_before_hitting_the_network() {
    let server = fixture_host().await;
    let state = state_for(&server);

    get_series(&state, "cpiaucsl", None, None)
        .await
        .expect("the fixture host answers");

    let sent = lines(&server).await;
    assert!(
        sent.iter().any(|line| line.contains("id=CPIAUCSL")),
        "{sent:?}"
    );
}

#[tokio::test]
async fn reports_not_found_when_fred_answers_with_a_page_instead_of_csv() {
    let server = fixture_host().await;
    let state = state_for(&server);

    let err = get_series(&state, "NOSUCH", None, None)
        .await
        .expect_err("an HTML page where CSV was promised");

    assert_eq!(err.code, codes::NOT_FOUND);
    assert!(
        err.hint
            .as_deref()
            .is_some_and(|hint| hint.contains("FSRCH")),
        "{:?}",
        err.hint
    );
}

#[tokio::test]
async fn rejects_a_malformed_series_id_without_making_a_request() {
    let server = fixture_host().await;
    let state = state_for(&server);

    let err = get_series(&state, "../etc/passwd", None, None)
        .await
        .expect_err("a path traversal is not a series id");

    assert_eq!(err.code, codes::BAD_REQUEST);
    assert!(
        err.message.contains("not a valid FRED series id"),
        "{}",
        err.message
    );
    assert!(requests(&server).await.is_empty());
}

#[tokio::test]
async fn caches_so_a_repeated_request_does_not_re_fetch() {
    let server = fixture_host().await;
    let state = state_for(&server);

    get_series(&state, "DGS10", None, None)
        .await
        .expect("the fixture host answers");
    let after_first = requests(&server).await.len();

    get_series(&state, "DGS10", None, None)
        .await
        .expect("the cached answer");

    assert_eq!(requests(&server).await.len(), after_first);
}

#[tokio::test]
async fn still_returns_observations_when_the_metadata_page_fails() {
    // Losing the "Units:" line must never cost you the data.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/graph/fredgraph.csv"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(CSV, "text/csv"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path_regex(r"^/series/.+"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;
    let state = state_for(&server);

    let result = get_series(&state, "UNRATE", None, None)
        .await
        .expect("the observations still arrive");

    assert_eq!(result.observations.len(), 4);
    assert_eq!(result.series.title, "UNRATE");
    assert_eq!(result.series.units, "");
    // Falling back to the observations themselves for the range.
    assert_eq!(result.series.observation_start, "2024-01-01");
    assert_eq!(result.series.observation_end, "2024-04-01");
    assert_eq!(result.series.source, FredSource::Scrape);
}

/* -------------------------------------------------------- the provider chain */

#[tokio::test]
async fn falls_back_to_the_official_api_when_the_scrape_is_blocked() {
    let web = blocked_host().await;
    let api = api_host().await;
    let state = AppState::new(Config {
        fred_web_base: web.uri(),
        fred_api_base: api.uri(),
        fred_api_key: Some("testkey".to_owned()),
        ..Config::default()
    });

    let result = get_series(&state, "UNRATE", None, None)
        .await
        .expect("the API arm answers what the scrape could not");

    assert_eq!(result.series.source, FredSource::Api);
    assert_eq!(result.series.units_short, "%");
    assert_eq!(result.series.notes, "From the API.");
    assert_eq!(
        result
            .observations
            .iter()
            .map(|o| (o.date.as_str(), o.value))
            .collect::<Vec<_>>(),
        [("2024-01-01", Some(3.7)), ("2024-02-01", None)]
    );

    // The credential rides in the query string, which is how this API takes it.
    let sent = requests(&api).await;
    let series_request = sent
        .iter()
        .find(|request| request.url.path() == "/series")
        .expect("expected a /series request");
    assert_eq!(param(series_request, "api_key").as_deref(), Some("testkey"));
    assert_eq!(param(series_request, "file_type").as_deref(), Some("json"));
    assert_eq!(
        param(series_request, "series_id").as_deref(),
        Some("UNRATE")
    );
}

#[tokio::test]
async fn passes_the_date_window_to_the_api_arm_under_its_own_parameter_names() {
    let web = blocked_host().await;
    let api = api_host().await;
    let state = AppState::new(Config {
        fred_web_base: web.uri(),
        fred_api_base: api.uri(),
        fred_api_key: Some("testkey".to_owned()),
        ..Config::default()
    });

    get_series(&state, "UNRATE", Some("2020-01-01"), Some("2021-01-01"))
        .await
        .expect("the API arm answers");

    let sent = requests(&api).await;
    let observations = sent
        .iter()
        .find(|request| request.url.path() == "/series/observations")
        .expect("expected a /series/observations request");

    assert_eq!(
        param(observations, "observation_start").as_deref(),
        Some("2020-01-01")
    );
    assert_eq!(
        param(observations, "observation_end").as_deref(),
        Some("2021-01-01")
    );
}

#[tokio::test]
async fn annotates_a_blocked_scrape_with_the_key_that_would_have_covered_it() {
    // No key, so the API arm is not available at all. The operator can fix
    // that, and nothing else in the response would tell them how.
    let web = blocked_host().await;
    let state = state_for(&web);

    let err = get_series(&state, "UNRATE", None, None)
        .await
        .expect_err("a host that hangs up");

    assert_eq!(err.code, codes::UPSTREAM_BLOCKED);
    let hint = err.hint.expect("the skipped arm contributes a hint");
    assert!(hint.contains("Set FRED_API_KEY"), "{hint}");
    assert!(
        hint.contains("https://fred.stlouisfed.org/docs/api/api_key.html"),
        "{hint}"
    );
    // The scrape's own explanation is kept in front of it.
    assert!(hint.contains("bot protection"), "{hint}");
    assert_eq!(hint.trim(), hint, "the hint is trimmed");
}

#[tokio::test]
async fn a_missing_series_ends_the_chain_without_asking_the_api() {
    // A `not_found` is a definitive answer: no series is missing at the scrape
    // and present at the API.
    let web = fixture_host().await;
    let api = api_host().await;
    let state = AppState::new(Config {
        fred_web_base: web.uri(),
        fred_api_base: api.uri(),
        fred_api_key: Some("testkey".to_owned()),
        ..Config::default()
    });

    let err = get_series(&state, "NOSUCH", None, None)
        .await
        .expect_err("no such series");

    assert_eq!(err.code, codes::NOT_FOUND);
    assert!(
        requests(&api).await.is_empty(),
        "the chain kept going past a definitive answer"
    );
}

/* -------------------------------------- searchSeries against a fixture host */

#[tokio::test]
async fn parses_results_out_of_the_search_page() {
    let server = fixture_host().await;
    let state = state_for(&server);

    let result = search_series(&state, "unemployment", DEFAULT_SEARCH_LIMIT)
        .await
        .expect("the fixture host answers");

    assert_eq!(result.source, FredSource::Scrape);
    assert_eq!(
        result
            .results
            .iter()
            .map(|entry| entry.id.as_str())
            .collect::<Vec<_>>(),
        ["UNRATE", "U6RATE"]
    );
    assert_eq!(result.results[0].frequency.as_deref(), Some("Monthly"));
    // The second card has no metadata line, and gains no fields from thin air.
    assert_eq!(result.results[1].frequency, None);
}

#[tokio::test]
async fn sends_the_query_as_the_st_parameter() {
    let server = fixture_host().await;
    let state = state_for(&server);

    search_series(&state, "real gdp", DEFAULT_SEARCH_LIMIT)
        .await
        .expect("the fixture host answers");

    let sent = lines(&server).await;
    let search = sent
        .iter()
        .find(|line| line.starts_with("/searchresults/"))
        .expect("expected a search request");

    // `URLSearchParams` spells a space `+`, and so does this.
    assert!(search.contains("st=real+gdp"), "{search}");
    assert!(search.contains("ob=sr"), "{search}");
    assert!(search.contains("od=desc"), "{search}");
}

#[tokio::test]
async fn caps_the_result_list_at_the_requested_limit() {
    let server = fixture_host().await;
    let state = state_for(&server);

    let result = search_series(&state, "unemployment", 1)
        .await
        .expect("the fixture host answers");

    assert_eq!(result.results.len(), 1);
    assert_eq!(result.results[0].id, "UNRATE");
}

#[tokio::test]
async fn rejects_an_empty_query() {
    let server = fixture_host().await;
    let state = state_for(&server);

    let err = search_series(&state, "   ", DEFAULT_SEARCH_LIMIT)
        .await
        .expect_err("a search needs something to search for");

    assert_eq!(err.code, codes::BAD_REQUEST);
    assert!(err.message.contains("at least one word"), "{}", err.message);
    assert!(requests(&server).await.is_empty());
}

#[tokio::test]
async fn searches_through_the_api_arm_and_reports_it_as_the_source() {
    let web = blocked_host().await;
    let api = api_host().await;
    let state = AppState::new(Config {
        fred_web_base: web.uri(),
        fred_api_base: api.uri(),
        fred_api_key: Some("testkey".to_owned()),
        ..Config::default()
    });

    let result = search_series(&state, "unemployment", 10)
        .await
        .expect("the API arm answers what the scrape could not");

    // Which arm answered is reported by the chain, not by a variable the
    // fallback reassigned on its way past.
    assert_eq!(result.source, FredSource::Api);
    assert_eq!(result.results.len(), 1);
    assert_eq!(result.results[0].id, "UNRATE");
    // `units_short` wins over `units` on this surface.
    assert_eq!(result.results[0].units.as_deref(), Some("%"));
    assert_eq!(
        result.results[0].observation_range.as_deref(),
        Some("1948-01-01 to 2026-07-01")
    );

    let sent = requests(&api).await;
    let search = sent
        .iter()
        .find(|request| request.url.path() == "/series/search")
        .expect("expected a /series/search request");
    assert_eq!(
        param(search, "search_text").as_deref(),
        Some("unemployment")
    );
    assert_eq!(param(search, "limit").as_deref(), Some("10"));
    assert_eq!(param(search, "order_by").as_deref(), Some("search_rank"));
}
