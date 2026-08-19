//! End-to-end exercise of the news fetch path against a local fixture server.
//!
//! The live host needs a credential nobody has in CI, so the real network path
//! cannot be exercised there. Pointing `alpaca_data_base` at a fixture server
//! covers everything except the socket: that the key pair is sent as headers
//! rather than in the query string, that the request overrides the two upstream
//! defaults that would otherwise cost us headlines, and that the two failures an
//! operator will actually hit — no key, and a wrong key — arrive as distinct,
//! actionable errors rather than as a generic bad gateway.

use std::sync::Mutex;

use terminal_core::types::NewsFeed;
use terminal_server::codes;
use terminal_server::config::{AlpacaKeys, Config};
use terminal_server::sources::alpaca::{self, get_news, MAX_LIMIT};
use terminal_server::AppState;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const PAYLOAD: &str = include_str!("../src/sources/fixtures/alpaca_news_one.json");

const GOOD_KEY: &str = "PKTESTKEYID";
const GOOD_SECRET: &str = "testsecret";

fn keys(key_id: &str, secret_key: &str) -> Option<AlpacaKeys> {
    Some(AlpacaKeys {
        key_id: key_id.to_string(),
        secret_key: secret_key.to_string(),
    })
}

fn syms(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| (*s).to_string()).collect()
}

/// A fixture host that answers `/news`, and answers a wrong key with 401 and a
/// JSON message rather than a page — which is what Alpaca does.
async fn fixture_host() -> MockServer {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/news"))
        .and(header("APCA-API-KEY-ID", GOOD_KEY))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(PAYLOAD.as_bytes(), "application/json"),
        )
        .with_priority(1)
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/news"))
        .respond_with(ResponseTemplate::new(401).set_body_raw(
            r#"{"message":"access key verification failed"}"#,
            "application/json",
        ))
        .mount(&server)
        .await;

    server
}

fn state_for(server: &MockServer, alpaca: Option<AlpacaKeys>) -> AppState {
    AppState::new(Config {
        alpaca_data_base: server.uri(),
        alpaca,
        ..Config::default()
    })
}

async fn requests(server: &MockServer) -> Vec<Request> {
    server.received_requests().await.unwrap_or_default()
}

/// The query of the request the fixture host saw, as pairs.
fn query_of(request: &Request) -> Vec<(String, String)> {
    request
        .url
        .query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect()
}

fn param(request: &Request, name: &str) -> Option<String> {
    query_of(request)
        .into_iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value)
}

/* ------------------------------------------- getNews against a fixture host */

#[tokio::test]
async fn sends_the_key_pair_as_headers_never_in_the_query_string() {
    let server = fixture_host().await;
    let state = state_for(&server, keys(GOOD_KEY, GOOD_SECRET));

    get_news(&state, &syms(&["NVDA"]), None, None)
        .await
        .expect("the fixture host answers");

    let sent = requests(&server).await;
    let sent = sent.first().expect("expected a request");

    assert_eq!(
        sent.headers
            .get("apca-api-key-id")
            .and_then(|v| v.to_str().ok()),
        Some(GOOD_KEY)
    );
    assert_eq!(
        sent.headers
            .get("apca-api-secret-key")
            .and_then(|v| v.to_str().ok()),
        Some(GOOD_SECRET)
    );

    // A secret in a query string ends up in every access log between here and
    // the origin.
    assert_eq!(param(sent, "key"), None);
    let query = sent.url.query().unwrap_or_default();
    assert!(!query.contains(GOOD_SECRET), "{query}");
    assert!(!query.contains(GOOD_KEY), "{query}");
}

#[tokio::test]
async fn overrides_the_two_upstream_defaults_that_would_cost_headlines() {
    let server = fixture_host().await;
    let state = state_for(&server, keys(GOOD_KEY, GOOD_SECRET));

    get_news(&state, &syms(&["AMD"]), Some(40), Some(14))
        .await
        .expect("the fixture host answers");

    let sent = requests(&server).await;
    let sent = sent.first().expect("expected a request");

    assert_eq!(sent.url.path(), "/news");
    assert_eq!(param(sent, "symbols").as_deref(), Some("AMD"));
    assert_eq!(param(sent, "limit").as_deref(), Some("40"));
    assert_eq!(param(sent, "sort").as_deref(), Some("desc"));
    // Headline-only items are the fastest-moving ones on the wire.
    assert_eq!(param(sent, "exclude_contentless").as_deref(), Some("false"));
    // The body is never rendered, so it is never requested.
    assert_eq!(param(sent, "include_content").as_deref(), Some("false"));

    // `start` defaults to *today* upstream, which reads as "no news exists"
    // for any thinly-covered ticker before lunchtime.
    let start = param(sent, "start").expect("start");
    let parsed = OffsetDateTime::parse(&start, &Rfc3339).expect("start is RFC-3339");
    let expected = OffsetDateTime::now_utc().unix_timestamp() - 14 * 86_400;
    assert!(
        (parsed.unix_timestamp() - expected).abs() < 60,
        "start was {start}"
    );
}

#[tokio::test]
async fn asks_for_the_whole_wire_when_no_symbol_is_given() {
    let server = fixture_host().await;
    let state = state_for(&server, keys(GOOD_KEY, GOOD_SECRET));

    get_news(&state, &[], None, None)
        .await
        .expect("the fixture host answers");

    let sent = requests(&server).await;
    assert_eq!(param(sent.first().expect("a request"), "symbols"), None);
}

#[tokio::test]
async fn returns_the_normalised_feed_the_panel_renders() {
    let server = fixture_host().await;
    let state = state_for(&server, keys(GOOD_KEY, GOOD_SECRET));

    let feed = get_news(&state, &syms(&["MSFT"]), None, None)
        .await
        .expect("the fixture host answers");

    assert_eq!(feed.articles.len(), 1);
    assert_eq!(feed.symbols, syms(&["MSFT"]));
    assert_eq!(feed.days, 7);
    assert_eq!(feed.source, "Alpaca / Benzinga");

    let article = &feed.articles[0];
    assert_eq!(
        article.headline,
        "Nvidia Q3 Beat: Data Center Revenue Tops Street's View"
    );
    assert_eq!(article.summary, "Shares rose in after-hours trade.");
    assert_eq!(article.time, 1_786_739_467);
}

#[tokio::test]
async fn clamps_a_limit_the_upstream_would_reject_outright() {
    let server = fixture_host().await;
    let state = state_for(&server, keys(GOOD_KEY, GOOD_SECRET));

    get_news(&state, &syms(&["INTC"]), Some(5000), None)
        .await
        .expect("the fixture host answers");

    let sent = requests(&server).await;
    assert_eq!(
        param(sent.first().expect("a request"), "limit"),
        Some(MAX_LIMIT.to_string())
    );
}

#[tokio::test]
async fn caches_so_n_polling_panels_make_one_upstream_call() {
    let server = fixture_host().await;
    let state = state_for(&server, keys(GOOD_KEY, GOOD_SECRET));

    get_news(&state, &syms(&["COIN"]), None, None)
        .await
        .unwrap();
    get_news(&state, &syms(&["COIN"]), None, None)
        .await
        .unwrap();

    assert_eq!(requests(&server).await.len(), 1);

    // Filed under the query, not under the window's start instant — which moves
    // every call, and would mean nothing ever hit.
    assert!(state
        .cache()
        .get::<NewsFeed>("alpaca:news:COIN:30:7")
        .await
        .is_some());
}

#[tokio::test]
async fn refuses_a_malformed_symbol_without_making_a_request() {
    let server = fixture_host().await;

    let err = alpaca::assert_symbols("NVDA&limit=9999").unwrap_err();
    assert!(err.message.contains("not a symbol"), "{}", err.message);
    assert!(requests(&server).await.is_empty());
}

/* ------------------------------------------------- getNews credential failures */

#[tokio::test]
async fn says_the_feed_is_unconfigured_and_does_not_call_out() {
    let server = fixture_host().await;
    let state = state_for(&server, None);

    let err = get_news(&state, &syms(&["GME"]), None, None)
        .await
        .expect_err("no key pair is set");

    assert_eq!(err.code, codes::NOT_CONFIGURED);
    assert_eq!(err.http_status(), 503);
    // The hint is shown verbatim in the panel, so it has to name the fix.
    assert!(
        err.hint
            .as_deref()
            .is_some_and(|h| h.contains("ALPACA_API_KEY_ID")),
        "{:?}",
        err.hint
    );
    assert!(requests(&server).await.is_empty());
}

#[tokio::test]
async fn treats_half_a_key_pair_as_no_key_pair() {
    // `Config` resolves a half-set pair to `None` — one key without the other
    // fails at the wire as a confusing 401, and the useful answer is the setup
    // hint. This is the shape the feed sees when only the key id is exported.
    let server = fixture_host().await;
    let state = state_for(&server, None);

    let err = get_news(&state, &syms(&["RIVN"]), None, None)
        .await
        .expect_err("half a pair is no pair");

    assert_eq!(err.code, codes::NOT_CONFIGURED);
    assert!(requests(&server).await.is_empty());
}

/// Serialises the one test that has to mutate the process environment.
static ENV: Mutex<()> = Mutex::new(());

#[tokio::test]
async fn reads_alpacas_sdk_variable_names_when_the_prefixed_ones_are_unset() {
    let server = fixture_host().await;

    let config = {
        let _guard = ENV
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        std::env::remove_var("ALPACA_API_KEY_ID");
        std::env::remove_var("ALPACA_API_SECRET_KEY");
        std::env::set_var("APCA_API_KEY_ID", GOOD_KEY);
        std::env::set_var("APCA_API_SECRET_KEY", GOOD_SECRET);

        let config = Config {
            alpaca_data_base: server.uri(),
            ..Config::from_env()
        };

        std::env::remove_var("APCA_API_KEY_ID");
        std::env::remove_var("APCA_API_SECRET_KEY");
        config
    };

    assert!(alpaca::has_credentials(&config));

    let feed = get_news(&AppState::new(config), &syms(&["PLTR"]), None, None)
        .await
        .expect("the SDK's own variable names carry the pair");
    assert_eq!(feed.articles.len(), 1);
}

#[tokio::test]
async fn calls_a_rejected_key_a_rejected_key_not_a_blocked_ip() {
    let server = fixture_host().await;
    let state = state_for(&server, keys("WRONGKEY", GOOD_SECRET));

    let err = get_news(&state, &syms(&["TSLA"]), None, None)
        .await
        .expect_err("the fixture host rejects the key");

    // The generic 4xx hint sends someone hunting a network problem they do not
    // have; this one points at the key pair.
    assert_eq!(err.code, codes::BAD_CREDENTIALS);
    assert_eq!(err.status, Some(401));
    assert_eq!(err.http_status(), 502);
    assert!(
        err.hint
            .as_deref()
            .is_some_and(|h| h.contains("ALPACA_API_SECRET_KEY")),
        "{:?}",
        err.hint
    );

    // The key pair reached the wire — this is a rejection, not a missing key.
    assert_eq!(requests(&server).await.len(), 1);
}
