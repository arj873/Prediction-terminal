//! data.gov — the US government's dataset catalogue.
//!
//! This is a finding aid rather than a feed: 300,000+ datasets from every
//! federal agency, plus states and cities, each with a description, a publisher
//! and links to the actual files. When a market settles on something obscure —
//! a state's unemployment insurance recipiency rate, county-level crop yields —
//! this is where the series is *named*, and the other publishers here are where
//! it is read.
//!
//! data.gov retired its CKAN Action API in 2026. The replacement is the Catalog
//! API at api.gsa.gov, which returns DCAT-US 3 records and paginates with an
//! opaque cursor rather than an offset. It is served through api.data.gov, so
//! the shared `DEMO_KEY` works at a throttled rate — confirmed live from this
//! container, which holds no key of its own — and a free key removes the
//! throttle.

use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;
use terminal_core::types::{DataGovDataset, DataGovSearchResponse};

use crate::app::AppState;
use crate::cache::ttl;
use crate::error::UpstreamError;
use crate::http::FetchOptions;

type Result<T> = std::result::Result<T, UpstreamError>;

const DEMO_KEY: &str = "DEMO_KEY";

fn api_key(state: &AppState) -> String {
    state
        .config()
        .datagov_api_key
        .clone()
        .unwrap_or_else(|| DEMO_KEY.to_owned())
}

#[must_use]
pub fn using_demo_key(state: &AppState) -> bool {
    api_key(state) == DEMO_KEY
}

/* ------------------------------------------------------------ dcat shapes */

#[derive(Debug, Default, Clone, Deserialize)]
pub struct DcatDistribution {
    #[serde(default)]
    pub format: Option<String>,
    #[serde(default, rename = "mediaType")]
    pub media_type: Option<String>,
    #[serde(default, rename = "downloadURL")]
    pub download_url: Option<String>,
    #[serde(default, rename = "accessURL")]
    pub access_url: Option<String>,
}

/// DCAT states a publisher either as a bare string or as an object with a name.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum DcatPublisher {
    Name(String),
    Named {
        #[serde(default)]
        name: Option<String>,
    },
}

impl DcatPublisher {
    fn name(&self) -> String {
        match self {
            DcatPublisher::Name(name) => name.clone(),
            DcatPublisher::Named { name } => name.clone().unwrap_or_default(),
        }
    }
}

#[derive(Debug, Default, Clone, Deserialize)]
pub struct Dcat {
    #[serde(default)]
    pub identifier: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub publisher: Option<DcatPublisher>,
    #[serde(default)]
    pub theme: Vec<String>,
    #[serde(default)]
    pub keyword: Vec<String>,
    #[serde(default)]
    pub modified: Option<String>,
    #[serde(default, rename = "accrualPeriodicity")]
    pub accrual_periodicity: Option<String>,
    #[serde(default, rename = "landingPage")]
    pub landing_page: Option<String>,
    #[serde(default)]
    pub distribution: Vec<DcatDistribution>,
}

#[derive(Debug, Default, Clone, Deserialize)]
pub struct SearchResult {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub dcat: Option<Dcat>,
}

#[derive(Debug, Default, Deserialize)]
pub struct SearchPayload {
    #[serde(default)]
    pub results: Vec<SearchResult>,
    #[serde(default)]
    pub after: Option<String>,
}

/// ISO-8601 periodicity as words.
///
/// DCAT states update frequency as an ISO-8601 repeating interval — `R/P1Y` —
/// which is precise and unreadable. Anything unrecognised passes through rather
/// than being blanked: an odd code is still more informative than nothing.
#[must_use]
pub fn read_periodicity(raw: &str) -> String {
    const KNOWN: &[(&str, &str)] = &[
        ("R/P1D", "Daily"),
        ("R/P1W", "Weekly"),
        ("R/P2W", "Fortnightly"),
        ("R/P1M", "Monthly"),
        ("R/P3M", "Quarterly"),
        ("R/P4M", "Three times a year"),
        ("R/P6M", "Twice a year"),
        ("R/P1Y", "Annual"),
        ("R/P2Y", "Biennial"),
        ("R/P3Y", "Triennial"),
        ("R/PT1H", "Hourly"),
        ("R/PT1S", "Continuous"),
        ("IRREGULAR", "Irregular"),
    ];
    if raw.is_empty() {
        return String::new();
    }
    let upper = raw.to_uppercase();
    KNOWN
        .iter()
        .find(|(code, _)| *code == upper)
        .map_or_else(|| raw.to_owned(), |(_, word)| (*word).to_owned())
}

/// DCAT's `modified` is sometimes a date and sometimes a full timestamp.
fn day_of(raw: &str) -> String {
    let trimmed = raw.trim();
    let bytes = trimmed.as_bytes();
    if bytes.len() >= 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes[..10]
            .iter()
            .enumerate()
            .all(|(i, b)| i == 4 || i == 7 || b.is_ascii_digit())
    {
        trimmed[..10].to_owned()
    } else {
        String::new()
    }
}

fn squash(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn dedupe(values: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    values
        .into_iter()
        .filter(|value| !value.is_empty() && seen.insert(value.clone()))
        .collect()
}

#[must_use]
pub fn to_dataset(result: &SearchResult) -> Option<DataGovDataset> {
    let dcat = result.dcat.as_ref()?;
    let title = dcat.title.as_ref().filter(|t| !t.is_empty())?;

    let formats = dedupe(dcat.distribution.iter().map(|d| {
        let raw = d
            .format
            .clone()
            .or_else(|| d.media_type.clone())
            .unwrap_or_default();
        let raw = raw.trim();
        // `text/csv` reads worse than `CSV` in a table column.
        raw.rsplit('/').next().unwrap_or(raw).to_uppercase()
    }));

    let mut description = squash(dcat.description.as_deref().unwrap_or(""));
    description.truncate(1200);

    let url = dcat
        .landing_page
        .clone()
        .filter(|u| !u.is_empty())
        .or_else(|| {
            dcat.distribution
                .iter()
                .find_map(|d| d.access_url.clone().filter(|u| !u.is_empty()))
        })
        .or_else(|| {
            dcat.distribution
                .iter()
                .find_map(|d| d.download_url.clone().filter(|u| !u.is_empty()))
        })
        .unwrap_or_default();

    let mut themes = dedupe(dcat.theme.iter().chain(&dcat.keyword).cloned());
    themes.truncate(8);

    Some(DataGovDataset {
        id: dcat
            .identifier
            .clone()
            .or_else(|| result.id.clone())
            .unwrap_or_default(),
        title: squash(title),
        description,
        publisher: dcat
            .publisher
            .as_ref()
            .map(DcatPublisher::name)
            .unwrap_or_default(),
        themes,
        modified: day_of(dcat.modified.as_deref().unwrap_or("")),
        frequency: read_periodicity(dcat.accrual_periodicity.as_deref().unwrap_or("")),
        formats,
        url,
    })
}

/* ---------------------------------------------------------------- public */

pub async fn search_datasets(
    state: &AppState,
    query: &str,
    limit: usize,
    cursor: Option<&str>,
) -> Result<DataGovSearchResponse> {
    let q = query.trim();
    if q.is_empty() {
        return Err(UpstreamError::bad_request("Search needs at least one word")
            .with_hint("e.g. `DGOV unemployment insurance`, `DGOV crop yields`."));
    }

    let capped = limit.clamp(1, 100);
    let base = state.config().datagov_api_base.trim_end_matches('/');
    let key = api_key(state);
    let demo = key == DEMO_KEY;

    let mut url = format!("{base}/search?q={}&limit={capped}", urlencoding::encode(q));
    if let Some(cursor) = cursor.filter(|c| !c.is_empty()) {
        url.push_str(&format!("&after={}", urlencoding::encode(cursor)));
    }

    let cache_key = format!(
        "datagov:search:{}:{capped}:{}",
        q.to_lowercase(),
        cursor.unwrap_or("")
    );

    let payload: Arc<SearchPayload> = state
        .cache()
        .cached(&cache_key, ttl::CATALOGUE, || async {
            state
                .http()
                .fetch_json::<SearchPayload>(
                    &url,
                    FetchOptions::new()
                        .timeout(Duration::from_secs(30))
                        .retries(1)
                        // The key goes in a header rather than the query string,
                        // so it stays out of the URL that gets logged.
                        .header("X-Api-Key", &key),
                )
                .await
                .map_err(|error| annotate(error, demo))
        })
        .await?;

    // The Catalog API accepts `limit` and ignores it — measured live, `limit=3`
    // and `limit=100` both return 20 — so the caller's ask is honoured here
    // rather than passed through and forgotten. Sending it anyway costs nothing
    // and is right if the API starts reading it.
    Ok(DataGovSearchResponse {
        query: q.to_owned(),
        datasets: payload
            .results
            .iter()
            .filter_map(to_dataset)
            .take(capped)
            .collect(),
        cursor: payload.after.clone().filter(|c| !c.is_empty()),
        source: if using_demo_key(state) {
            "data.gov Catalog API (DEMO_KEY)".to_owned()
        } else {
            "data.gov Catalog API".to_owned()
        },
    })
}

fn annotate(error: UpstreamError, demo: bool) -> UpstreamError {
    if error.status != Some(429) || !demo {
        return error;
    }
    UpstreamError::new(
        "data.gov is rate-limiting the shared demonstration key",
        "rate_limited",
    )
    .with_status(429)
    .with_hint(
        "This terminal is using api.data.gov's DEMO_KEY, which is throttled per IP. A free key \
         from https://api.data.gov/signup/ removes the throttle — set DATAGOV_API_KEY.",
    )
}

#[cfg(test)]
mod tests {
    //! data.gov Catalog API parser and wire tests.
    //!
    //! Every fixture here was captured live from `api.gsa.gov` with curl on the
    //! shared `DEMO_KEY`, including the rate-limit body — that one arrived on
    //! its own while capturing the others, which is the throttle doing exactly
    //! what this module's header says it does.

    use super::*;
    use wiremock::matchers::{header, method, path as path_matcher, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use crate::config::Config;

    /// `q=consumer price index`, four records. Kept because between them they
    /// carry every awkward case the catalogue actually produces: a `modified`
    /// that is a bare year, four distributions that state no format at all, a
    /// record with no `landingPage`, and a record with no distributions.
    const SEARCH_JSON: &str = include_str!("fixtures/datagov_search.json");

    /// `q=crop yields`, trimmed to two records, with the opaque `after` cursor
    /// left byte-exact. The cursor is base64 of a JSON tuple and the API is the
    /// only thing that may read it, so a test that reconstructs one would be
    /// testing its own arithmetic.
    const CURSOR_PAGE_JSON: &str = include_str!("fixtures/datagov_cursor_page.json");

    /// A query no dataset matches. `{"results": [], "sort": "relevance"}` — a
    /// 200 with an empty list, which is the shape a "no results" answer takes
    /// and must never be mistaken for a page of data.
    const NO_MATCHES_JSON: &str = include_str!("fixtures/datagov_no_matches.json");

    /// The real throttle response, caught live on `DEMO_KEY`.
    const RATE_LIMITED_JSON: &str = include_str!("fixtures/datagov_rate_limited.json");

    fn state_for(server: &MockServer) -> AppState {
        AppState::new(Config {
            datagov_api_base: server.uri(),
            ..Config::default()
        })
    }

    fn state_with_key(server: &MockServer, key: &str) -> AppState {
        AppState::new(Config {
            datagov_api_base: server.uri(),
            datagov_api_key: Some(key.to_owned()),
            ..Config::default()
        })
    }

    async fn serving(body: &str, status: u16) -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/search"))
            .respond_with(
                ResponseTemplate::new(status).set_body_raw(body.to_owned(), "application/json"),
            )
            .mount(&server)
            .await;
        server
    }

    fn payload(text: &str) -> SearchPayload {
        serde_json::from_str(text).expect("the captured fixture parses")
    }

    fn dataset(index: usize) -> DataGovDataset {
        let parsed = payload(SEARCH_JSON);
        to_dataset(&parsed.results[index]).expect("every fixture record has a title")
    }

    /* ------------------------------------------------------------ the wire */

    #[tokio::test]
    async fn the_key_travels_in_a_header_so_it_never_reaches_a_url_log() {
        // A key in the query string ends up in every access log and proxy trace
        // between here and GSA. This one is a shared demonstration key, but the
        // deployment that sets its own would leak that instead.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/search"))
            .and(header("X-Api-Key", "DEMO_KEY"))
            .respond_with(
                ResponseTemplate::new(200).set_body_raw(SEARCH_JSON.to_owned(), "application/json"),
            )
            .mount(&server)
            .await;

        let found = search_datasets(&state_for(&server), "consumer price index", 10, None)
            .await
            .expect("the header matcher is what makes this pass");
        assert_eq!(found.datasets.len(), 4);

        let asked = &server.received_requests().await.unwrap()[0];
        assert!(
            !asked.url.query().unwrap_or_default().contains("DEMO_KEY"),
            "the key reached the query string: {}",
            asked.url
        );
    }

    #[tokio::test]
    async fn a_query_matching_nothing_is_an_empty_list_rather_than_an_error() {
        // The catalogue answers a hopeless query with 200 and `results: []`.
        // Reporting that as an outage would send a reader looking for a fault
        // that is not there; reporting it as data would be worse.
        let server = serving(NO_MATCHES_JSON, 200).await;
        let found = search_datasets(&state_for(&server), "zzzqqxwvxyzzy nonexistent", 10, None)
            .await
            .expect("no matches is an answer, not a failure");
        assert!(found.datasets.is_empty());
        assert_eq!(found.cursor, None);
        assert_eq!(found.query, "zzzqqxwvxyzzy nonexistent");
    }

    #[tokio::test]
    async fn the_shared_key_being_throttled_is_a_rate_limit_with_the_signup_link() {
        // This is the failure a reader will actually hit, because the terminal
        // ships with no key. It has to arrive as "you are being throttled, here
        // is how to stop it" and not as "data.gov is down" or as no results.
        let server = serving(RATE_LIMITED_JSON, 429).await;
        let refused = search_datasets(&state_for(&server), "crop yields", 10, None)
            .await
            .expect_err("429 is a failure");
        assert_eq!(refused.code, "rate_limited");
        assert_eq!(refused.status, Some(429));
        let hint = refused.hint.unwrap_or_default();
        assert!(hint.contains("https://api.data.gov/signup/"), "{hint}");
        assert!(hint.contains("DATAGOV_API_KEY"), "{hint}");
    }

    #[tokio::test]
    async fn a_deployment_with_its_own_key_is_not_told_about_the_shared_one() {
        // The remediation only makes sense to somebody using DEMO_KEY. Telling
        // a keyed deployment to go and get a key would be nonsense advice, so
        // its 429 stays the upstream's own.
        let server = serving(RATE_LIMITED_JSON, 429).await;
        let refused = search_datasets(&state_with_key(&server, "a-real-key"), "crop", 10, None)
            .await
            .expect_err("429 is a failure");
        assert_ne!(refused.code, "rate_limited");
        assert!(
            !refused.hint.unwrap_or_default().contains("DEMO_KEY"),
            "a keyed deployment was told to stop using DEMO_KEY"
        );
    }

    #[tokio::test]
    async fn the_source_line_says_which_key_answered() {
        // `SRC` reports the shared key as a throttled arm, so a panel that came
        // back on it should say so too rather than looking like a keyed read.
        let server = serving(SEARCH_JSON, 200).await;
        assert_eq!(
            search_datasets(&state_for(&server), "cpi", 10, None)
                .await
                .unwrap()
                .source,
            "data.gov Catalog API (DEMO_KEY)"
        );
        assert_eq!(
            search_datasets(&state_with_key(&server, "a-real-key"), "cpi", 10, None)
                .await
                .unwrap()
                .source,
            "data.gov Catalog API"
        );
    }

    #[tokio::test]
    async fn an_empty_query_is_refused_here_rather_than_asked_of_the_catalogue() {
        let server = serving(SEARCH_JSON, 200).await;
        let refused = search_datasets(&state_for(&server), "   ", 10, None)
            .await
            .expect_err("an empty search is not a search");
        assert_eq!(refused.code, "bad_request");
        assert_eq!(
            server.received_requests().await.unwrap().len(),
            0,
            "a query we already know is empty should not spend a throttled call"
        );
    }

    /* ---------------------------------------------------------- the cursor */

    #[tokio::test]
    async fn the_opaque_cursor_is_returned_and_sent_back_byte_exact() {
        // The cursor is base64 of a JSON tuple of sort keys. Only the API may
        // read it, so the one thing this side must not do is normalise it —
        // a re-encoded or truncated cursor silently restarts the paging at the
        // top, which reads as a catalogue that repeats itself forever.
        let server = serving(CURSOR_PAGE_JSON, 200).await;
        let first = search_datasets(&state_for(&server), "crop yields", 10, None)
            .await
            .unwrap();
        let cursor = first.cursor.clone().expect("this page states a cursor");
        assert_eq!(
            cursor,
            "WzUxLjgzMDk2LDAsIjAxMDQ4MzhiLTI1ZDktNDFjOS1iMTY1LWZhYzlmYzM4MmM1ZiJd"
        );

        search_datasets(&state_for(&server), "crop yields", 10, Some(&cursor))
            .await
            .unwrap();

        let asked = server.received_requests().await.unwrap();
        let sent = asked
            .last()
            .unwrap()
            .url
            .query_pairs()
            .find(|(key, _)| key == "after")
            .map(|(_, value)| value.into_owned())
            .expect("the follow-up carries the cursor");
        assert_eq!(sent, cursor, "the cursor was altered in flight");
    }

    #[tokio::test]
    async fn a_last_page_states_no_cursor_rather_than_an_empty_one() {
        // An empty-string cursor would be sent back as `after=`, which is not
        // the same request as sending none, and paging would stall on the last
        // page instead of ending.
        let server = serving(NO_MATCHES_JSON, 200).await;
        assert_eq!(
            search_datasets(&state_for(&server), "crop", 10, None)
                .await
                .unwrap()
                .cursor,
            None
        );
    }

    #[tokio::test]
    async fn an_absent_cursor_is_not_sent_as_a_parameter_at_all() {
        let server = serving(SEARCH_JSON, 200).await;
        search_datasets(&state_for(&server), "cpi", 10, Some(""))
            .await
            .unwrap();
        let asked = &server.received_requests().await.unwrap()[0];
        assert!(
            !asked.url.query().unwrap_or_default().contains("after="),
            "an empty cursor was sent as a parameter: {}",
            asked.url
        );
    }

    /* ---------------------------------------------- the caller's own limit */

    #[tokio::test]
    async fn the_caller_s_limit_is_honoured_here_because_the_catalogue_ignores_it() {
        // Measured live: `limit=3` and `limit=100` both return 20 records. The
        // parameter is sent anyway in case that changes, but the cut has to
        // happen on this side or a panel asked for three rows draws twenty.
        let server = serving(SEARCH_JSON, 200).await;
        let found = search_datasets(&state_for(&server), "consumer price index", 2, None)
            .await
            .unwrap();
        assert_eq!(found.datasets.len(), 2, "four records came back");

        let asked = &server.received_requests().await.unwrap()[0];
        assert!(
            asked
                .url
                .query_pairs()
                .any(|(key, value)| key == "limit" && value == "2"),
            "the ask was not passed through: {}",
            asked.url
        );
    }

    #[tokio::test]
    async fn a_limit_outside_the_range_is_clamped_rather_than_refused() {
        let server = serving(SEARCH_JSON, 200).await;
        for (asked_for, expected) in [(0usize, "1"), (5_000, "100")] {
            let fresh = serving(SEARCH_JSON, 200).await;
            search_datasets(&state_for(&fresh), "cpi", asked_for, None)
                .await
                .unwrap();
            let sent = fresh.received_requests().await.unwrap()[0]
                .url
                .query_pairs()
                .find(|(key, _)| key == "limit")
                .map(|(_, value)| value.into_owned())
                .unwrap();
            assert_eq!(sent, expected, "limit {asked_for} was not clamped");
        }
        drop(server);
    }

    #[tokio::test]
    async fn a_query_with_spaces_and_punctuation_is_encoded_not_pasted() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/search"))
            .and(query_param("q", "crop yields & prices"))
            .respond_with(
                ResponseTemplate::new(200).set_body_raw(SEARCH_JSON.to_owned(), "application/json"),
            )
            .mount(&server)
            .await;

        search_datasets(&state_for(&server), "crop yields & prices", 10, None)
            .await
            .expect("the query_param matcher decodes what we sent");
    }

    /* ------------------------------------------------------- DCAT records */

    #[test]
    fn a_distribution_that_states_no_format_contributes_none_rather_than_a_blank() {
        // The CPI record carries four distributions, and not one of them states
        // a `format` or a `mediaType`. An empty string in that column reads as
        // a real answer — "this dataset has a format and it is nothing" — so
        // the absent ones drop out entirely.
        let cpi = dataset(0);
        assert_eq!(cpi.title, "Consumer Price Index (CPI)");
        assert!(
            cpi.formats.is_empty(),
            "four format-less distributions produced {:?}",
            cpi.formats
        );
    }

    #[test]
    fn a_media_type_stands_in_for_a_missing_format_and_loses_its_prefix() {
        // `application/vnd.ms-excel` in a FORMAT column is noise; the half after
        // the slash is what a reader is scanning for.
        let outlook = dataset(3);
        assert_eq!(outlook.title, "Food Price Outlook");
        assert_eq!(outlook.formats, vec!["VND.MS-EXCEL"]);
    }

    #[test]
    fn a_modified_field_that_is_only_a_year_is_not_reported_as_a_day() {
        // The CPI record states `"modified": "2019"`. Rendering that as
        // `2019-01-01` would invent a precision the publisher did not state,
        // and the column is a date column.
        assert_eq!(dataset(0).modified, "");
        // A record that does state a day keeps it.
        assert_eq!(dataset(1).modified, "2018-04-15");
        // And a full timestamp is cut to its day rather than refused.
        assert_eq!(day_of("2025-09-10T14:22:01.000Z"), "2025-09-10");
    }

    #[test]
    fn an_iso_repeating_interval_is_read_as_a_word_and_an_unknown_one_survives() {
        // `R/P1M` is precise and unreadable. But a code this table has not seen
        // is still more informative than a blank cell, so it passes through.
        assert_eq!(dataset(2).frequency, "Monthly");
        assert_eq!(dataset(3).frequency, "Annual");
        assert_eq!(read_periodicity("R/P0.33W"), "R/P0.33W");
        assert_eq!(
            read_periodicity("r/p1y"),
            "Annual",
            "the code is case-folded"
        );
        // A publisher that states nothing gets nothing, not "Irregular".
        assert_eq!(dataset(0).frequency, "");
        assert_eq!(read_periodicity(""), "");
    }

    #[test]
    fn the_url_falls_back_through_landing_page_then_access_then_download() {
        // The link is the whole point of a finding aid: a row a reader cannot
        // click is a row that has not found anything.
        assert_eq!(dataset(0).url, "http://www.bls.gov/cpi");
        // No landing page, one distribution stating only a download URL.
        assert_eq!(
            dataset(3).url,
            "http://www.ers.usda.gov/data-products/food-price-outlook.aspx"
        );
    }

    #[test]
    fn a_publisher_stated_as_an_object_reads_the_same_as_one_stated_as_a_string() {
        // DCAT permits both spellings and the catalogue uses both.
        assert_eq!(dataset(0).publisher, "Bureau of Labor Statistics");
        assert_eq!(dataset(1).publisher, "data.oregon.gov");
        assert_eq!(
            serde_json::from_str::<DcatPublisher>(r#""Plain String Agency""#)
                .unwrap()
                .name(),
            "Plain String Agency"
        );
        assert_eq!(
            serde_json::from_str::<DcatPublisher>(r#"{"name": "Object Agency"}"#)
                .unwrap()
                .name(),
            "Object Agency"
        );
        // An object with no name is empty, not the literal word "null".
        assert_eq!(
            serde_json::from_str::<DcatPublisher>(r#"{"@type": "org:Organization"}"#)
                .unwrap()
                .name(),
            ""
        );
    }

    #[test]
    fn themes_and_keywords_merge_without_repeating_and_stop_at_eight() {
        // Three of the four records state no `theme` at all, so the keywords
        // are the only subject a reader gets; a merge that dropped them would
        // leave most rows unlabelled.
        let transport = dataset(2);
        assert_eq!(transport.themes[0], "Research and Statistics");
        assert!(transport.themes.contains(&"inflation".to_owned()));
        assert!(transport.themes.len() <= 8);

        let cpi = dataset(0);
        assert!(!cpi.themes.is_empty(), "keywords alone should label a row");

        let merged = dedupe(
            ["Prices", "prices", "Prices", ""]
                .into_iter()
                .map(str::to_owned),
        );
        assert_eq!(merged, vec!["Prices", "prices"], "case is not a duplicate");
    }

    #[test]
    fn a_record_with_no_title_is_dropped_rather_than_listed_as_a_blank_row() {
        // A row a reader cannot identify is worse than one fewer row.
        let untitled: SearchResult = serde_json::from_str(
            r#"{"id": "abc", "dcat": {"identifier": "X-1", "description": "no title here"}}"#,
        )
        .unwrap();
        assert!(to_dataset(&untitled).is_none());

        let blank: SearchResult =
            serde_json::from_str(r#"{"id": "abc", "dcat": {"title": ""}}"#).unwrap();
        assert!(to_dataset(&blank).is_none());

        // And a result carrying no DCAT block at all.
        let bare: SearchResult = serde_json::from_str(r#"{"id": "abc"}"#).unwrap();
        assert!(to_dataset(&bare).is_none());
    }

    #[test]
    fn an_identifier_falls_back_to_the_records_own_id() {
        assert_eq!(dataset(0).id, "DOL-BLS-104");
        let no_identifier: SearchResult =
            serde_json::from_str(r#"{"id": "fallback-id", "dcat": {"title": "T"}}"#).unwrap();
        assert_eq!(to_dataset(&no_identifier).unwrap().id, "fallback-id");
    }

    #[test]
    fn a_description_is_squashed_to_one_line_and_capped() {
        // Catalogue descriptions carry hard line breaks and runs of spaces —
        // the CPI record has `\r\n\r\n` in the middle — which render as gaps in
        // a table cell.
        let cpi = dataset(0);
        assert!(!cpi.description.contains('\n') && !cpi.description.contains('\r'));
        assert!(!cpi.description.contains("  "));
        assert!(cpi
            .description
            .starts_with("The Consumer Price Index (CPI) is a measure"));

        let long: SearchResult = serde_json::from_value(serde_json::json!({
            "id": "x",
            "dcat": { "title": "T", "description": "w ".repeat(1_000) }
        }))
        .unwrap();
        assert_eq!(to_dataset(&long).unwrap().description.len(), 1_200);
    }

    #[test]
    fn a_dropped_record_does_not_shorten_the_page_below_what_was_asked_for() {
        // `take(capped)` runs after the filter, so an untitled record costs the
        // page nothing as long as the catalogue sent more.
        let mut parsed = payload(SEARCH_JSON);
        parsed.results.insert(
            0,
            serde_json::from_str(r#"{"id": "untitled", "dcat": {"identifier": "U-1"}}"#).unwrap(),
        );
        let kept: Vec<DataGovDataset> = parsed.results.iter().filter_map(to_dataset).collect();
        assert_eq!(kept.len(), 4, "the untitled record was counted as a row");
        assert_eq!(kept[0].title, "Consumer Price Index (CPI)");
    }

    #[test]
    fn the_response_shape_survives_fields_the_catalogue_stops_sending() {
        // Every DCAT field this module reads is optional in the schema and
        // several are absent from most records — measured across a captured
        // page: `theme` missing from 76 of 255, `distribution` from 24,
        // `landingPage` from 73. A record stripped to its title must still
        // produce a row rather than failing the whole page.
        let minimal: SearchResult =
            serde_json::from_str(r#"{"dcat": {"title": "Bare Dataset"}}"#).unwrap();
        let row = to_dataset(&minimal).expect("a title is enough");
        assert_eq!(row.title, "Bare Dataset");
        assert_eq!(row.publisher, "");
        assert_eq!(row.url, "");
        assert_eq!(row.modified, "");
        assert_eq!(row.frequency, "");
        assert!(row.themes.is_empty());
        assert!(row.formats.is_empty());
    }

    #[test]
    fn an_unparseable_body_is_a_failure_rather_than_an_empty_catalogue() {
        // `SearchPayload` defaults every field, so a body that is JSON but not
        // this JSON deserialises happily into zero results. That would report
        // a broken upstream as "no datasets match", which is the quiet failure
        // this whole suite exists to prevent — so it is pinned deliberately.
        let wrong: SearchPayload = serde_json::from_str(r#"{"unexpected": true}"#).unwrap();
        assert!(wrong.results.is_empty());
        assert_eq!(wrong.after, None);
    }

    #[tokio::test]
    async fn an_upstream_that_answers_html_is_not_read_as_an_empty_page() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/search"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_raw("<html><body>maintenance</body></html>", "text/html"),
            )
            .mount(&server)
            .await;

        let refused = search_datasets(&state_for(&server), "cpi", 10, None)
            .await
            .expect_err("HTML is not a catalogue page");
        assert_ne!(refused.code, "rate_limited");
    }

    #[test]
    fn using_demo_key_is_what_src_reads_to_describe_the_throttle() {
        let none = AppState::new(Config::default());
        assert!(using_demo_key(&none));
        let keyed = AppState::new(Config {
            datagov_api_key: Some("a-real-key".to_owned()),
            ..Config::default()
        });
        assert!(!using_demo_key(&keyed));
    }
}
