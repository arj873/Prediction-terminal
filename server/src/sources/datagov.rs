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

    let mut url = format!(
        "{base}/search?q={}&limit={capped}",
        urlencoding::encode(q)
    );
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
