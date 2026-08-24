//! FRED (Federal Reserve Economic Data) — scraped from fred.stlouisfed.org.
//!
//! Two surfaces, deliberately different in how much they can be trusted:
//!
//!  - **Observations** come from `/graph/fredgraph.csv?id=<ID>`, the same
//!    endpoint the "Download → CSV" button on every FRED graph page uses. It is
//!    a two-column CSV and has been stable for years. This is the load-bearing
//!    path.
//!  - **Metadata** (units, frequency, seasonal adjustment, notes) is parsed out
//!    of the `/series/<ID>` HTML page, whose markup is not a contract. The
//!    parser therefore tries several strategies and degrades to empty strings
//!    rather than failing — a missing "Units:" line must never cost you the
//!    data itself.
//!
//! If `FRED_API_KEY` is set, the official API is used as a fallback whenever a
//! scrape fails. That matters in practice: fred.stlouisfed.org resets
//! connections from datacentre IP ranges, so a cloud-hosted deployment often
//! cannot scrape it at all even though `api.stlouisfed.org` answers fine.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use fancy_regex::Regex as FancyRegex;
use regex::Regex;
use serde::Deserialize;
use terminal_core::types::{
    DataObservation, DataSearchResponse, DataSearchResult, DataSeries, DataSeriesResponse,
    DataSource,
};

use crate::app::AppState;
use crate::cache::ttl;
use crate::css;
use crate::error::{codes, Result, UpstreamError};
use crate::http::FetchOptions;
use crate::providers::{Chain, Provider};
use crate::scrape;

/// The default `FSRCH` page size, matching the TypeScript's `limit = 25`.
pub const DEFAULT_SEARCH_LIMIT: u32 = 25;

/// Both scraped pages are slow enough that the shared 20s budget is tight, and
/// neither is on a path anyone is waiting on interactively twice.
const SCRAPE_TIMEOUT: Duration = Duration::from_secs(30);

/// FRED series IDs are alphanumerics plus `_` and `.` — reject anything else.
static SERIES_ID: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z0-9_.\-]{1,64}$").expect("SERIES_ID is a valid regex"));

/// Validate and canonicalise a series id.
///
/// Upper-cased because FRED's ids are, and because the cache key and the
/// outbound URL should not depend on how the operator typed it.
pub fn assert_series_id(id: &str) -> Result<String> {
    let trimmed = id.trim().to_uppercase();
    if !SERIES_ID.is_match(&trimmed) {
        return Err(
            UpstreamError::bad_request(format!("\"{id}\" is not a valid FRED series id"))
                .with_hint(
                    "Series ids look like UNRATE, GDPC1 or DGS10. Try `FSRCH <words>` to find one.",
                ),
        );
    }
    Ok(trimmed)
}

/* ------------------------------------------------------------ observations */

/// `YYYY-MM-DD` and nothing else — how a data row is told from a header row.
static ISO_DATE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\d{4}-\d{2}-\d{2}$").expect("ISO_DATE is a valid regex"));

/// A body that opens with a tag is an HTML page, not the CSV we asked for.
static LEADING_TAG: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*<").expect("LEADING_TAG is a valid regex"));

/// Strip one leading and one trailing double quote, as `/^"|"$/g` does.
fn unquote(field: &str) -> &str {
    let field = field.strip_prefix('"').unwrap_or(field);
    field.strip_suffix('"').unwrap_or(field)
}

/// Parse the fredgraph CSV.
///
/// Header is `observation_date,<SERIES_ID>` on current FRED and `DATE,<ID>` on
/// older exports; both are handled by ignoring the header names entirely and
/// taking column 0 as the date and column 1 as the value. A `.` is FRED's
/// missing-value marker and becomes `None` — never `0`, which is a real
/// reading for several series.
#[must_use]
pub fn parse_fred_csv(csv: &str) -> Vec<DataObservation> {
    let mut out = Vec::new();

    for line in csv.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let Some(comma) = line.find(',') else {
            continue;
        };

        let date = unquote(line[..comma].trim());
        let raw_value = unquote(line[comma + 1..].trim());

        // Skip the header row (and any repeat of it) by requiring a real date.
        if !ISO_DATE.is_match(date) {
            continue;
        }

        if raw_value == "." || raw_value.is_empty() {
            out.push(DataObservation {
                date: date.to_owned(),
                value: None,
            });
            continue;
        }

        // `Number()` on a thousands-separated string is `NaN`, so the separators
        // come out first; anything still unreadable is missing rather than zero.
        let value = raw_value
            .replace(',', "")
            .parse::<f64>()
            .ok()
            .filter(|value| value.is_finite());
        out.push(DataObservation {
            date: date.to_owned(),
            value,
        });
    }

    out
}

async fn scrape_observations(
    state: &AppState,
    id: &str,
    start: Option<&str>,
    end: Option<&str>,
) -> Result<Vec<DataObservation>> {
    let base = &state.config().fred_web_base;

    let mut params: Vec<(&str, &str)> = vec![("id", id)];
    if let Some(start) = start.filter(|start| !start.is_empty()) {
        params.push(("cosd", start));
    }
    if let Some(end) = end.filter(|end| !end.is_empty()) {
        params.push(("coed", end));
    }
    let url = with_query(&format!("{base}/graph/fredgraph.csv"), &params)?;

    let csv = state
        .http()
        .fetch_text(
            &url,
            FetchOptions::new()
                .timeout(SCRAPE_TIMEOUT)
                .retries(2)
                .header("Accept", "text/csv,text/plain,*/*")
                .header("Referer", format!("{base}/series/{id}")),
        )
        .await?;

    // A wrong id yields an HTML error page rather than a 404.
    if LEADING_TAG.is_match(&csv) {
        return Err(UpstreamError::not_found(format!(
            "FRED returned a page instead of CSV for {id}"
        ))
        .with_hint(format!(
            "No FRED series called {id}. Try `FSRCH <words>` to search."
        )));
    }

    let observations = parse_fred_csv(&csv);
    if observations.is_empty() {
        return Err(UpstreamError::new(
            format!("FRED returned no observations for {id}"),
            codes::EMPTY_UPSTREAM,
        ));
    }
    Ok(observations)
}

/* --------------------------------------------------------------- metadata */

/// Everything [`DataSeries`] carries except which arm produced it.
///
/// The parser has no idea whether it is the source of record or the fallback,
/// and the provider chain that does know is the one that stamps it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SeriesMeta {
    pub id: String,
    pub title: String,
    pub units: String,
    pub units_short: String,
    pub frequency: String,
    pub seasonal_adjustment: String,
    pub last_updated: String,
    pub observation_start: String,
    pub observation_end: String,
    pub notes: String,
}

impl SeriesMeta {
    /// Attribute this metadata to the arm that produced it.
    ///
    /// `source` is `"scrape"` or `"api"` here, and the publisher's own word for
    /// whichever arm answered at the other eleven — the shared type carries a
    /// string because "which arm" means something different at each of them.
    #[must_use]
    pub fn into_series(self, source: &str) -> DataSeries {
        DataSeries {
            provider: DataSource::Fred,
            id: self.id.clone(),
            title: self.title,
            units: self.units,
            units_short: self.units_short,
            frequency: self.frequency,
            seasonal_adjustment: self.seasonal_adjustment,
            last_updated: self.last_updated,
            observation_start: self.observation_start,
            observation_end: self.observation_end,
            notes: self.notes,
            source: source.to_owned(),
            source_url: format!("https://fred.stlouisfed.org/series/{}", self.id),
        }
    }
}

/// The attribute labels FRED renders beside their values. Lower-cased, because
/// every strategy below matches against them case-insensitively.
const LABELS: [&str; 8] = [
    "units",
    "frequency",
    "seasonal adjustment",
    "source",
    "release",
    "notes",
    "last updated",
    "observation period",
];

/// The document title carries the site suffix; the `<h1>` and the `<title>`
/// both append the id in parentheses. The panel header already shows the id.
static FRED_SUFFIX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\s*\|\s*FRED.*$").expect("FRED_SUFFIX is a valid regex"));
static STL_SUFFIX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\s*\|\s*St\.?\s*Louis\s*Fed.*$").expect("STL_SUFFIX is a valid regex")
});

/// Trailing punctuation on a label: `Units:` and `Units` are one attribute.
static LABEL_PUNCT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[:\s]+$").expect("LABEL_PUNCT is a valid regex"));

/// FRED writes "Percent, Seasonally Adjusted"; the panel wants "Percent", and
/// the adjustment has its own field.
static ADJUSTMENT_SUFFIX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i),\s*(Seasonally Adjusted.*|Not Seasonally Adjusted.*)$")
        .expect("ADJUSTMENT_SUFFIX is a valid regex")
});

/// Rendered as "1948-01-01 to 2026-07-01" near the header, with any of four
/// dashes standing in for "to".
static OBS_RANGE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(\d{4}-\d{2}-\d{2})\s*(?:to|–|-|—)\s*(\d{4}-\d{2}-\d{2})")
        .expect("OBS_RANGE is a valid regex")
});

/// One `Label: value` pattern per label, for the last-ditch strategy.
///
/// The lookahead is what keeps `Units: Percent Frequency: Monthly` — which is
/// what a run of labelled `<p>`s collapses to once the markup is gone — from
/// swallowing the next label into this label's value. `regex` cannot compile a
/// lookahead, so this one pattern uses `fancy-regex`.
static LABEL_PATTERNS: LazyLock<Vec<(&'static str, FancyRegex)>> = LazyLock::new(|| {
    let alternation = LABELS.join("|");
    LABELS
        .iter()
        .map(|label| {
            let pattern =
                format!(r"(?i){label}\s*:\s*([^:]{{1,120}}?)(?=\s+(?:{alternation})\s*:|$)");
            let compiled = FancyRegex::new(&pattern).expect("a label pattern is a valid regex");
            (*label, compiled)
        })
        .collect()
});

/// Note a labelled attribute, first spelling winning.
///
/// Both the key and the value are normalised here rather than at each of the
/// three call sites, which is what lets them disagree about markup and still
/// agree about what "Units" means.
fn record(attributes: &mut HashMap<String, String>, label: &str, value: &str) {
    let key = LABEL_PUNCT.replace(label, "").trim().to_lowercase();
    let value = scrape::collapse_ws(value);
    if key.is_empty() || value.is_empty() {
        return;
    }
    attributes.entry(key).or_insert(value);
}

/// Truncate to `limit` characters.
///
/// The TypeScript's `.slice(0, 4000)` counts UTF-16 units; counting characters
/// is the nearest thing that cannot split a codepoint.
fn truncate_chars(text: String, limit: usize) -> String {
    match text.char_indices().nth(limit) {
        Some((end, _)) => text[..end].to_owned(),
        None => text,
    }
}

/// Pull metadata off the series page.
///
/// FRED renders each attribute as a `<p>` containing a bold/`<span>` label
/// ("Units:") followed by the value. Class names have churned over the years, so
/// the primary strategy is label-text matching over the whole document, with
/// class-based selectors as a backstop.
#[must_use]
pub fn parse_series_page(html: &str, id: &str) -> SeriesMeta {
    let doc = scrape::parse_document(html);

    // ---- title -------------------------------------------------------------
    let mut title = scrape::text_of_first(&doc, css!("#page-title"))
        .or_else(|| {
            scrape::text_of_first(
                &doc,
                css!("h1.series-title, .series-title h1, h1[itemprop=\"name\"]"),
            )
        })
        // `<title>Unemployment Rate (UNRATE) | FRED | St. Louis Fed`
        .or_else(|| {
            scrape::attr_of_first(&doc, css!("meta[property=\"og:title\"]"), "content")
                .map(scrape::collapse_ws)
                .filter(|content| !content.is_empty())
        })
        .or_else(|| scrape::text_of_first(&doc, css!("title")))
        .unwrap_or_default();

    title = FRED_SUFFIX.replace(&title, "").into_owned();
    title = STL_SUFFIX.replace(&title, "").into_owned();
    if let Ok(trailing_id) = Regex::new(&format!(r"(?i)\s*\({}\)\s*$", regex::escape(id))) {
        title = trailing_id.replace(&title, "").into_owned();
    }
    let title = title.trim().to_owned();

    // ---- labelled attributes ----------------------------------------------
    let mut attributes: HashMap<String, String> = HashMap::new();

    // Strategy A: an element whose entire text is a known label, value in the
    // remainder of its parent.
    for line in scrape::select_all(&doc, css!("p, li, div, tr")) {
        // `.children()`, not `.find()`: a `<span>` nested inside the *value*
        // half would slice the label off the wrong end.
        let Some(label_el) = scrape::first_child_matching(line, css!("span, b, strong, th")) else {
            continue;
        };
        let label_text = scrape::text_of(label_el);
        let lowered = label_text.to_lowercase();
        let label = lowered.strip_suffix(':').unwrap_or(&lowered);
        if label.is_empty() || !LABELS.contains(&label) {
            continue;
        }
        let whole = scrape::text_of(line);
        let value = scrape::text_after_label(&whole, &label_text);
        record(&mut attributes, label, value);
    }

    // Strategy B: legacy `.series-meta-label` / `.series-meta-value` pairs.
    for label_el in scrape::select_all(&doc, css!(".series-meta-label, .fg-source-label")) {
        let label = scrape::text_of(label_el);
        let value =
            scrape::next_sibling_matching(label_el, css!(".series-meta-value, .fg-source-value"))
                .map(scrape::text_of)
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| {
                    scrape::parent_element(label_el)
                        .map(|parent| {
                            let whole = scrape::text_of(parent);
                            scrape::text_after_label(&whole, &label).to_owned()
                        })
                        .unwrap_or_default()
                });
        record(&mut attributes, &label, &value);
    }

    // Strategy C: raw-text regex over the page, for markup we did not anticipate.
    if attributes.is_empty() {
        // Deliberately `text_of` rather than `visible_text_of`: the original
        // regexes over `$('body').text()`, which includes script and style.
        let text = scrape::select_one(&doc, css!("body"))
            .map(scrape::text_of)
            .unwrap_or_default();
        for (label, pattern) in LABEL_PATTERNS.iter() {
            if let Ok(Some(captured)) = pattern.captures(&text) {
                if let Some(value) = captured.get(1) {
                    record(&mut attributes, label, value.as_str());
                }
            }
        }
    }

    // ---- units, split into long and short ----------------------------------
    // FRED writes "Percent, Seasonally Adjusted" or "Billions of Dollars".
    let units_raw = attributes.get("units").cloned().unwrap_or_default();
    let units = ADJUSTMENT_SUFFIX.replace(&units_raw, "").trim().to_owned();

    // ---- observation range -------------------------------------------------
    let range_text = scrape::text_of_first(
        &doc,
        css!(".series-obs-range, #series-obs-range, .fg-obs-range"),
    )
    .unwrap_or_else(|| {
        scrape::select_one(&doc, css!("body"))
            .map(scrape::text_of)
            .unwrap_or_default()
    });
    let (observation_start, observation_end) = OBS_RANGE
        .captures(&range_text)
        .map(|range| (range[1].to_owned(), range[2].to_owned()))
        .unwrap_or_default();

    let notes = scrape::text_of_first(
        &doc,
        css!("#notes-container, .series-notes, #series-notes, [itemprop=\"description\"]"),
    )
    .or_else(|| attributes.get("notes").cloned())
    .unwrap_or_default();

    SeriesMeta {
        id: id.to_owned(),
        title: if title.is_empty() {
            id.to_owned()
        } else {
            title
        },
        units_short: units.clone(),
        units,
        frequency: attributes.get("frequency").cloned().unwrap_or_default(),
        seasonal_adjustment: attributes
            .get("seasonal adjustment")
            .cloned()
            .unwrap_or_default(),
        last_updated: attributes.get("last updated").cloned().unwrap_or_default(),
        observation_start,
        observation_end,
        notes: truncate_chars(notes, 4000),
    }
}

async fn scrape_series_meta(state: &AppState, id: &str) -> Result<SeriesMeta> {
    let url = format!(
        "{}/series/{}",
        state.config().fred_web_base,
        urlencoding::encode(id)
    );
    let html = state
        .http()
        .fetch_text(&url, FetchOptions::new().timeout(SCRAPE_TIMEOUT).retries(1))
        .await?;
    Ok(parse_series_page(&html, id))
}

/// The scrape arm: observations are the deliverable, metadata rides alongside.
async fn scrape_series(
    state: &AppState,
    id: &str,
    start: Option<&str>,
    end: Option<&str>,
) -> Result<DataSeriesResponse> {
    let (observations, meta) = futures::join!(
        scrape_observations(state, id, start, end),
        scrape_series_meta(state, id)
    );
    let observations = observations?;

    // Losing the "Units:" line must never cost you the data, so a metadata
    // failure is logged and the series is assembled from the observations.
    let mut series = match meta {
        Ok(meta) => meta.into_series("scrape"),
        Err(err) => {
            tracing::warn!("[fred] metadata scrape failed for {id}: {err}");
            SeriesMeta {
                id: id.to_owned(),
                title: id.to_owned(),
                observation_start: first_date(&observations),
                observation_end: last_date(&observations),
                ..SeriesMeta::default()
            }
            .into_series("scrape")
        }
    };

    if series.observation_start.is_empty() {
        series.observation_start = first_date(&observations);
    }
    if series.observation_end.is_empty() {
        series.observation_end = last_date(&observations);
    }

    Ok(DataSeriesResponse {
        series,
        observations,
    })
}

fn first_date(observations: &[DataObservation]) -> String {
    observations
        .first()
        .map(|observation| observation.date.clone())
        .unwrap_or_default()
}

fn last_date(observations: &[DataObservation]) -> String {
    observations
        .last()
        .map(|observation| observation.date.clone())
        .unwrap_or_default()
}

/* ------------------------------------------------------------ api fallback */

/// The official API's series record.
///
/// Every field is optional here even though the API documents them all: a
/// renamed field should cost the panel one blank line, not a 500.
#[derive(Debug, Default, Deserialize)]
struct ApiSeries {
    #[serde(default)]
    id: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    units: String,
    #[serde(default)]
    units_short: String,
    #[serde(default)]
    frequency: String,
    #[serde(default)]
    seasonal_adjustment: String,
    #[serde(default)]
    last_updated: String,
    #[serde(default)]
    observation_start: String,
    #[serde(default)]
    observation_end: String,
    #[serde(default)]
    notes: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct ApiSeriesEnvelope {
    #[serde(default)]
    seriess: Option<Vec<ApiSeries>>,
}

#[derive(Debug, Default, Deserialize)]
struct ApiObservation {
    #[serde(default)]
    date: String,
    #[serde(default)]
    value: String,
}

#[derive(Debug, Default, Deserialize)]
struct ApiObservationsEnvelope {
    #[serde(default)]
    observations: Option<Vec<ApiObservation>>,
}

fn from_api_series(series: ApiSeries) -> SeriesMeta {
    SeriesMeta {
        id: series.id,
        title: series.title,
        units: series.units,
        units_short: series.units_short,
        frequency: series.frequency,
        seasonal_adjustment: series.seasonal_adjustment,
        last_updated: series.last_updated,
        observation_start: series.observation_start,
        observation_end: series.observation_end,
        notes: truncate_chars(series.notes.unwrap_or_default(), 4000),
    }
}

/// Append `params` to `base` the way `URLSearchParams` does — spaces as `+`,
/// which is what the fixture server's recorded query strings assert.
fn with_query(base: &str, params: &[(&str, &str)]) -> Result<String> {
    let mut url = reqwest::Url::parse(base).map_err(|_| {
        UpstreamError::new(format!("{base} is not a usable URL"), codes::UPSTREAM_ERROR)
    })?;
    {
        let mut query = url.query_pairs_mut();
        for (name, value) in params {
            query.append_pair(name, value);
        }
    }
    Ok(url.into())
}

/// The API takes its credential as a query parameter, not a header.
fn api_url(state: &AppState, path: &str, params: &[(&str, Option<&str>)]) -> Result<String> {
    let key = state.config().fred_api_key.as_deref().unwrap_or_default();
    let mut pairs: Vec<(&str, &str)> = vec![("api_key", key), ("file_type", "json")];
    for (name, value) in params {
        if let Some(value) = value.filter(|value| !value.is_empty()) {
            pairs.push((name, value));
        }
    }
    with_query(&format!("{}{path}", state.config().fred_api_base), &pairs)
}

async fn api_series(
    state: &AppState,
    id: &str,
    start: Option<&str>,
    end: Option<&str>,
) -> Result<DataSeriesResponse> {
    let meta_url = api_url(state, "/series", &[("series_id", Some(id))])?;
    let observations_url = api_url(
        state,
        "/series/observations",
        &[
            ("series_id", Some(id)),
            ("observation_start", start),
            ("observation_end", end),
        ],
    )?;

    let (meta, observations) = futures::try_join!(
        state
            .http()
            .fetch_json::<ApiSeriesEnvelope>(&meta_url, FetchOptions::new()),
        state
            .http()
            .fetch_json::<ApiObservationsEnvelope>(&observations_url, FetchOptions::new()),
    )?;

    let Some(series) = meta.seriess.into_iter().flatten().next() else {
        return Err(UpstreamError::not_found(format!(
            "No FRED series called {id}"
        )));
    };

    Ok(DataSeriesResponse {
        series: from_api_series(series).into_series("api"),
        observations: observations
            .observations
            .unwrap_or_default()
            .into_iter()
            .map(|observation| DataObservation {
                value: if observation.value == "." || observation.value.is_empty() {
                    None
                } else {
                    observation.value.parse::<f64>().ok()
                },
                date: observation.date,
            })
            .collect(),
    })
}

/* ----------------------------------------------------------------- search */

/// `/series/<ID>` and nothing else: a search result page is guaranteed to
/// contain those, and the id is the one thing they carry reliably.
static SERIES_HREF: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:https?://fred\.stlouisfed\.org)?/series/([A-Za-z0-9_.\-]+)/?(?:\?.*)?$")
        .expect("SERIES_HREF is a valid regex")
});

/// Parse FRED search results.
///
/// Rather than depend on result-card classes, this collects every anchor whose
/// href is exactly `/series/<ID>` — the one thing a search result page is
/// guaranteed to contain — and reads the title from the link text.
#[must_use]
pub fn parse_search_page(html: &str) -> Vec<DataSearchResult> {
    let doc = scrape::parse_document(html);
    let mut seen: HashSet<String> = HashSet::new();
    let mut results: Vec<DataSearchResult> = Vec::new();

    for link in scrape::select_all(&doc, css!("a[href*=\"/series/\"]")) {
        let href = scrape::attr(link, "href").unwrap_or_default();
        let Some(captured) = SERIES_HREF.captures(href) else {
            continue;
        };

        let id = captured[1].to_uppercase();
        if seen.contains(&id) {
            continue;
        }

        let title = scrape::text_of(link);
        // Navigation chrome links to /series/ too, but without a descriptive
        // label. The id is only marked seen after that check, so a nav link
        // does not shadow the real result further down the page.
        if title.is_empty() || title.chars().count() < 3 || title.to_uppercase() == id {
            continue;
        }

        seen.insert(id.clone());

        // The metadata line ("Percent, Monthly, Seasonally Adjusted") usually
        // sits in a sibling of the link's container.
        let meta = scrape::closest(link, css!("div, li, tr"))
            .and_then(|card| {
                scrape::select_one(card, css!(".series-meta, .fred-meta, .search-result-meta"))
            })
            .map(scrape::text_of)
            .unwrap_or_default();

        // Named rather than defaulted: `provider` decides which publisher a
        // row is opened against, and a default one would open the wrong series
        // silently. `DataSearchResult` deliberately has no `Default`.
        let mut entry = DataSearchResult {
            provider: DataSource::Fred,
            id,
            title,
            units: None,
            frequency: None,
            seasonal_adjustment: None,
            observation_range: None,
        };
        if !meta.is_empty() {
            let parts: Vec<&str> = meta.split(',').map(str::trim).collect();
            entry.units = part(&parts, 0);
            entry.frequency = part(&parts, 1);
            entry.seasonal_adjustment = part(&parts, 2);
        }
        results.push(entry);
    }

    results
}

/// One comma-separated field of the metadata line; a blank one is absent, as
/// `if (parts[0])` made it.
fn part(parts: &[&str], index: usize) -> Option<String> {
    parts
        .get(index)
        .filter(|value| !value.is_empty())
        .map(|value| (*value).to_owned())
}

async fn scrape_search(state: &AppState, query: &str, limit: u32) -> Result<Vec<DataSearchResult>> {
    let url = with_query(
        &format!("{}/searchresults/", state.config().fred_web_base),
        &[("st", query), ("ob", "sr"), ("od", "desc")], // ob=sr: order by search rank
    )?;
    let html = state
        .http()
        .fetch_text(&url, FetchOptions::new().timeout(SCRAPE_TIMEOUT).retries(1))
        .await?;
    let mut results = parse_search_page(&html);
    results.truncate(limit as usize);
    Ok(results)
}

async fn api_search(state: &AppState, query: &str, limit: u32) -> Result<Vec<DataSearchResult>> {
    let limit = limit.to_string();
    let url = api_url(
        state,
        "/series/search",
        &[
            ("search_text", Some(query)),
            ("limit", Some(&limit)),
            ("order_by", Some("search_rank")),
        ],
    )?;
    let data: ApiSeriesEnvelope = state.http().fetch_json(&url, FetchOptions::new()).await?;

    Ok(data
        .seriess
        .unwrap_or_default()
        .into_iter()
        .map(|series| DataSearchResult {
            provider: DataSource::Fred,
            id: series.id,
            title: series.title,
            units: Some(if series.units_short.is_empty() {
                series.units
            } else {
                series.units_short
            }),
            frequency: Some(series.frequency),
            seasonal_adjustment: Some(series.seasonal_adjustment),
            observation_range: Some(format!(
                "{} to {}",
                series.observation_start, series.observation_end
            )),
        })
        .collect())
}

/* ------------------------------------------------------------ public entry */

/// The scrape is the source of record; the official API stands behind it.
///
/// The API arm only exists when a key is configured, and when it is *not*, a
/// blocked scrape is worth annotating: the operator can fix it, and nothing
/// else in the response would tell them how.
///
/// When both fail the scrape's error is what surfaces — the chain's default —
/// because it carries the `upstream_blocked` hint that says what actually went
/// wrong.
fn fred_chain<'a, T>(
    state: &'a AppState,
    what: String,
    scrape: impl Future<Output = Result<T>> + Send + 'a,
    via_api: impl Future<Output = Result<T>> + Send + 'a,
) -> Chain<'a, T> {
    Chain::new(what)
        .log_prefix("fred")
        .provider(Provider::new("scrape", "fred.stlouisfed.org", scrape))
        .provider(
            Provider::new("api", "the FRED API", via_api)
                .available(state.config().fred_api_key.is_some())
                .skipped_hint(|cause| {
                    (cause.code == codes::UPSTREAM_BLOCKED).then(|| {
                        format!(
                            "{} Set FRED_API_KEY (free, from \
                             https://fred.stlouisfed.org/docs/api/api_key.html) to use the \
                             official API instead.",
                            cause.hint.as_deref().unwrap_or_default()
                        )
                        .trim()
                        .to_owned()
                    })
                }),
        )
}

/// A series and its observations, from whichever arm could answer.
pub async fn get_series(
    state: &AppState,
    raw_id: &str,
    start: Option<&str>,
    end: Option<&str>,
) -> Result<Arc<DataSeriesResponse>> {
    let id = assert_series_id(raw_id)?;
    let key = format!(
        "fred:series:{id}:{}:{}",
        start.unwrap_or_default(),
        end.unwrap_or_default()
    );

    state
        .cache()
        .cached(&key, ttl::FRED, || async {
            fred_chain(
                state,
                format!("series {id}"),
                scrape_series(state, &id, start, end),
                api_series(state, &id, start, end),
            )
            .first_answer()
            .await
            .map(|answer| answer.value)
        })
        .await
}

/// Search the series catalogue.
///
/// Which arm answered is reported by the chain rather than by a variable the
/// fallback reassigned on its way past.
pub async fn search_series(
    state: &AppState,
    query: &str,
    limit: u32,
) -> Result<Arc<DataSearchResponse>> {
    let query = query.trim();
    if query.is_empty() {
        return Err(UpstreamError::bad_request("Search needs at least one word"));
    }
    let capped = limit.clamp(1, 100);
    let key = format!("fred:search:{}:{capped}", query.to_lowercase());

    state
        .cache()
        .cached(&key, ttl::FRED, || async {
            let answer = fred_chain(
                state,
                format!("search {query}"),
                scrape_search(state, query, capped),
                api_search(state, query, capped),
            )
            .first_answer()
            .await?;

            Ok(DataSearchResponse {
                query: query.to_owned(),
                results: answer.value,
                // FRED answers alone, so nothing can be missing or skipped —
                // `ECOS` fills these when it fans the same shape out.
                unavailable: Vec::new(),
                skipped: Vec::new(),
            })
        })
        .await
}

#[cfg(test)]
mod tests {
    //! FRED parser tests.
    //!
    //! These matter more than the usual unit test: fred.stlouisfed.org refuses
    //! connections from datacentre IPs, so CI cannot exercise the live scrape.
    //! The fixtures below reproduce the shapes FRED actually serves — current
    //! and legacy CSV headers, the series page's labelled-attribute markup, and
    //! the search results page — so a regression in the parsing logic is caught
    //! even where the network path is not available.

    use super::*;

    /// `(date, value)` pairs, since the wire type carries no `PartialEq`.
    fn rows(observations: &[DataObservation]) -> Vec<(&str, Option<f64>)> {
        observations
            .iter()
            .map(|observation| (observation.date.as_str(), observation.value))
            .collect()
    }

    /* ------------------------------------------------------ parse_fred_csv */

    #[test]
    fn parses_the_current_observation_date_header() {
        let parsed = parse_fred_csv("observation_date,UNRATE\n1948-01-01,3.4\n1948-02-01,3.8\n");
        assert_eq!(
            rows(&parsed),
            [("1948-01-01", Some(3.4)), ("1948-02-01", Some(3.8))]
        );
    }

    #[test]
    fn parses_the_legacy_date_header_identically() {
        let parsed = parse_fred_csv("DATE,GDP\n1947-01-01,243.164\n");
        assert_eq!(rows(&parsed), [("1947-01-01", Some(243.164))]);
    }

    #[test]
    fn maps_freds_missing_marker_to_null_rather_than_dropping_the_row() {
        let parsed = parse_fred_csv("observation_date,DGS10\n2020-01-01,.\n2020-01-02,1.88\n");
        assert_eq!(
            rows(&parsed),
            [("2020-01-01", None), ("2020-01-02", Some(1.88))]
        );
    }

    #[test]
    fn handles_crlf_line_endings_and_a_trailing_blank_line() {
        let parsed = parse_fred_csv("observation_date,UNRATE\r\n2024-01-01,3.7\r\n\r\n");
        assert_eq!(rows(&parsed), [("2024-01-01", Some(3.7))]);
    }

    #[test]
    fn handles_quoted_fields_and_thousands_separators() {
        let parsed =
            parse_fred_csv("\"observation_date\",\"GDP\"\n\"2024-01-01\",\"28,624.069\"\n");
        assert_eq!(rows(&parsed), [("2024-01-01", Some(28_624.069))]);
    }

    #[test]
    fn ignores_rows_whose_first_column_is_not_a_date() {
        let parsed = parse_fred_csv("observation_date,X\nnot-a-date,1\n2024-01-01,2\n");
        assert_eq!(rows(&parsed), [("2024-01-01", Some(2.0))]);
    }

    #[test]
    fn returns_an_empty_list_for_an_empty_body() {
        assert!(parse_fred_csv("").is_empty());
    }

    #[test]
    fn preserves_negative_and_exponential_values() {
        let parsed = parse_fred_csv("observation_date,X\n2024-01-01,-2.5\n2024-02-01,1.2e3\n");
        assert_eq!(
            rows(&parsed),
            [("2024-01-01", Some(-2.5)), ("2024-02-01", Some(1200.0))]
        );
    }

    /* --------------------------------------------------- parse_series_page */

    const SERIES_PAGE: &str = r#"
<!doctype html><html><head>
<title>Unemployment Rate (UNRATE) | FRED | St. Louis Fed</title>
<meta property="og:title" content="Unemployment Rate (UNRATE) | FRED | St. Louis Fed">
</head><body>
  <h1 id="page-title">Unemployment Rate <span class="series-id">(UNRATE)</span></h1>
  <div class="series-meta">
    <p><span class="series-meta-label">Units:</span> <span class="series-meta-value">Percent, Seasonally Adjusted</span></p>
    <p><span class="series-meta-label">Frequency:</span> <span class="series-meta-value">Monthly</span></p>
    <p><span class="series-meta-label">Seasonal Adjustment:</span> <span class="series-meta-value">Seasonally Adjusted</span></p>
    <p><span class="series-meta-label">Last Updated:</span> <span class="series-meta-value">Aug 1, 2026 7:46 AM CDT</span></p>
  </div>
  <div class="series-obs-range">1948-01-01 to 2026-07-01</div>
  <div id="notes-container">The unemployment rate represents the number of unemployed as a percentage of the labor force.</div>
</body></html>"#;

    #[test]
    fn extracts_the_title_without_the_fred_suffix_or_the_id() {
        let meta = parse_series_page(SERIES_PAGE, "UNRATE");
        assert_eq!(meta.title, "Unemployment Rate");
        assert_eq!(meta.id, "UNRATE");
    }

    #[test]
    fn reads_labelled_attributes_and_strips_the_adjustment_suffix_off_units() {
        let meta = parse_series_page(SERIES_PAGE, "UNRATE");
        assert_eq!(meta.units, "Percent");
        assert_eq!(meta.units_short, "Percent");
        assert_eq!(meta.frequency, "Monthly");
        assert_eq!(meta.seasonal_adjustment, "Seasonally Adjusted");
        assert!(
            meta.last_updated.contains("Aug 1, 2026"),
            "{}",
            meta.last_updated
        );
    }

    #[test]
    fn extracts_the_observation_range() {
        let meta = parse_series_page(SERIES_PAGE, "UNRATE");
        assert_eq!(meta.observation_start, "1948-01-01");
        assert_eq!(meta.observation_end, "2026-07-01");
    }

    #[test]
    fn extracts_notes() {
        let meta = parse_series_page(SERIES_PAGE, "UNRATE");
        assert!(
            meta.notes.contains("percentage of the labor force"),
            "{}",
            meta.notes
        );
    }

    #[test]
    fn falls_back_to_og_title_when_the_page_heading_is_missing() {
        let html = r#"<html><head><meta property="og:title" content="Real Gross Domestic Product (GDPC1) | FRED"></head><body></body></html>"#;
        let meta = parse_series_page(html, "GDPC1");
        assert_eq!(meta.title, "Real Gross Domestic Product");
    }

    #[test]
    fn degrades_to_the_series_id_rather_than_failing_on_unrecognised_markup() {
        // The whole point: losing the "Units:" line must never cost you the data.
        let meta = parse_series_page(
            "<html><body><div>totally different</div></body></html>",
            "DGS10",
        );
        assert_eq!(meta.title, "DGS10");
        assert_eq!(meta.units, "");
        assert_eq!(meta.frequency, "");
    }

    /// The third strategy, which is the only one that needs `fancy-regex`: no
    /// label markup at all, so the labels are found in the body text and the
    /// lookahead is what stops "Units" swallowing "Frequency".
    #[test]
    fn falls_back_to_a_raw_text_regex_when_no_markup_carries_the_labels() {
        let html = "<html><body>Units: Percent, Seasonally Adjusted Frequency: Monthly \
                    Last Updated: Aug 1, 2026</body></html>";
        let meta = parse_series_page(html, "UNRATE");
        assert_eq!(meta.units, "Percent");
        assert_eq!(meta.frequency, "Monthly");
    }

    /* --------------------------------------------------- parse_search_page */

    const SEARCH_PAGE: &str = r#"
<!doctype html><html><body>
  <div class="search-results">
    <div class="series-pager-item">
      <a href="/series/UNRATE">Unemployment Rate</a>
      <div class="series-meta">Percent, Monthly, Seasonally Adjusted</div>
    </div>
    <div class="series-pager-item">
      <a href="https://fred.stlouisfed.org/series/U6RATE">Total Unemployed, Plus All Persons Marginally Attached</a>
      <div class="series-meta">Percent, Monthly, Seasonally Adjusted</div>
    </div>
    <div class="series-pager-item">
      <a href="/series/UNRATE">Unemployment Rate</a>
    </div>
    <a href="/series/">Browse series</a>
    <a href="/categories/32991">A category</a>
  </div>
</body></html>"#;

    #[test]
    fn collects_series_links_and_de_duplicates_repeats() {
        let results = parse_search_page(SEARCH_PAGE);
        assert_eq!(results.len(), 2);
        assert_eq!(
            results
                .iter()
                .map(|result| result.id.as_str())
                .collect::<Vec<_>>(),
            ["UNRATE", "U6RATE"]
        );
    }

    #[test]
    fn handles_both_relative_and_absolute_series_hrefs() {
        let results = parse_search_page(SEARCH_PAGE);
        assert_eq!(results[1].id, "U6RATE");
    }

    #[test]
    fn splits_the_metadata_line_into_units_frequency_and_adjustment() {
        let results = parse_search_page(SEARCH_PAGE);
        let first = &results[0];
        assert_eq!(first.units.as_deref(), Some("Percent"));
        assert_eq!(first.frequency.as_deref(), Some("Monthly"));
        assert_eq!(
            first.seasonal_adjustment.as_deref(),
            Some("Seasonally Adjusted")
        );
    }

    #[test]
    fn ignores_navigation_links_that_are_not_individual_series() {
        let results = parse_search_page(SEARCH_PAGE);
        assert!(!results.iter().any(|result| result.title == "Browse series"));
        assert!(!results.iter().any(|result| result.id.starts_with("32991")));
    }

    #[test]
    fn returns_an_empty_list_when_nothing_matched() {
        assert!(parse_search_page("<html><body>No results.</body></html>").is_empty());
    }

    /* ------------------------------------------------------ assertSeriesId */

    #[test]
    fn upper_cases_and_accepts_valid_ids() {
        assert_eq!(assert_series_id("unrate").unwrap(), "UNRATE");
        assert_eq!(assert_series_id("GDPC1").unwrap(), "GDPC1");
        assert_eq!(assert_series_id("CPIAUCSL").unwrap(), "CPIAUCSL");
    }

    #[test]
    fn rejects_path_traversal_and_query_injection() {
        for bad in [
            "../../etc/passwd",
            "UNRATE&id=X",
            "UN RATE",
            "",
            &"a".repeat(65),
        ] {
            let err = assert_series_id(bad).unwrap_err();
            assert_eq!(err.code, codes::BAD_REQUEST, "{bad}");
            assert!(
                err.message.contains("not a valid FRED series id"),
                "{}",
                err.message
            );
        }
    }
}
