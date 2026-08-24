//! Bureau of Labor Statistics — the US labour statistics of record.
//!
//! CPI, the unemployment rate, nonfarm payrolls and average hourly earnings are
//! published here first; FRED mirrors them hours later. For a market that
//! settles on a print, reading the publisher rather than the mirror is the
//! difference between settling on time and settling on someone else's schedule.
//!
//! Four things shape this module, and each of them is a way the API can be
//! wrong at you without ever failing.
//!
//! **A refusal is an HTTP 200.** `api.bls.gov` answers a request it will not
//! service with status 200, `content-type: application/json`, and a body whose
//! `status` field reads `REQUEST_NOT_PROCESSED`. Verified live from this
//! container, whose address had already spent the anonymous daily allowance:
//!
//! ```text
//! $ curl -s -o /dev/null -w '%{http_code}\n' \
//!     'https://api.bls.gov/publicAPI/v1/timeseries/data/LNS14000000?startyear=2024&endyear=2024'
//! 200
//! {"status":"REQUEST_NOT_PROCESSED","responseTime":0,
//!  "message":["Request could not be serviced, as the daily threshold for total number of
//!              requests allocated to the user with registration key  has been reached."],
//!  "Results":{}}
//! ```
//!
//! So the envelope is read before the data is, and nothing here treats a
//! successful transport as a successful request. [`fixtures/bls_throttled.json`]
//! is that exact body, kept so the test asserts what the bureau really sends.
//!
//! **The year window.** The API takes a start and an end *year* and caps the
//! span — ten years unregistered, twenty with a free key. Ask for more and you
//! are not told: the answer is the most recent decade, well-formed, plausible
//! and silently short. A chart of "unemployment since 1948" is therefore eight
//! requests rather than one, so [`windows`] splits the range and the pieces are
//! merged back. The point is not that the module knows which way the API fails
//! — truncating or refusing — but that it never has to find out, because it
//! never sends a span wider than the arm it is talking to accepts.
//!
//! **Periods are not dates.** The bureau dates a monthly reading `M07`, a
//! quarter `Q02`, a half-year `S01`, and an annual *average* `M13` — a summary
//! row sitting in the same array as the twelve monthlies it averages. Charting
//! `M13` alongside `M01`–`M12` would draw thirteen points a year, one of them
//! the mean of the other twelve. [`crate::period::period_to_date`] already
//! refuses `M13`, and this module leans on that refusal rather than repeating
//! the rule: [`period_date`] rewrites the bureau's zero-padded codes into the
//! one period vocabulary the terminal reads, and every judgement about what is
//! and is not a month is made there.
//!
//! **A missing figure is `-`.** The bureau states an unpublished reading as a
//! bare hyphen. It is `None`, never `0.0`: a zero in an unemployment series is
//! a reading someone will act on, and nobody published it.
//!
//! One deliberate difference from the TypeScript this replaces. That module
//! POSTed a JSON body listing `seriesid`, `startyear` and `endyear`; the shared
//! [`crate::http::Http`] client here is GET-only, so this module uses the
//! bureau's single-series GET form,
//! `/{version}/timeseries/data/{seriesId}?startyear=&endyear=[&registrationkey=]`.
//! That route is real: `v1` and `v2` answer it with the JSON envelope above,
//! while `/v3/timeseries/data/...` and `/v1/nonsense/...` answer a Tomcat 404
//! page and the POST route `/v1/timeseries/data/` answers a GET with 415 — so
//! the envelope we get back is this route replying, not a catch-all. What the
//! GET form does not carry is `catalog=true`, which the bureau documents for
//! the POST form only; the catalogue block is still read where a payload has
//! one, and where it does not the curated titles below stand in.

use std::collections::BTreeMap;
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use regex::Regex;
use serde::Deserialize;
use terminal_core::dataset::DataSource;
use terminal_core::types::{DataObservation, DataSearchResult, DataSeries, DataSeriesResponse};
use time::OffsetDateTime;

use crate::app::AppState;
use crate::cache::ttl;
use crate::error::{codes, Result, UpstreamError};
use crate::http::FetchOptions;
use crate::period::period_to_date;

/// Years of history one unregistered request buys.
const SPAN_UNREGISTERED: i32 = 10;

/// And with a free registration key. Both are the bureau's numbers, not ours;
/// exceeding either is the failure this module exists to make impossible.
const SPAN_REGISTERED: i32 = 20;

/// Requests an unregistered address gets per day. Quoted in the hint because
/// "REQUEST_NOT_PROCESSED" tells an operator nothing they can act on.
const DAILY_QUOTA_UNREGISTERED: u32 = 25;

/// The bureau's own timeout budget is generous and its answers are large; a
/// windowed range pays this per window, which is why the windows are sequential
/// rather than eight of them at once.
const TIMEOUT: Duration = Duration::from_secs(30);

/// One retry, not the usual two. A refusal arrives as an HTTP 200 and is
/// therefore never retried by the transport, so retries only ever cost
/// something on a genuine transport failure — and each attempt that does reach
/// the bureau is one of twenty-five for the day.
const RETRIES: u32 = 1;

/// BLS series ids are upper-case alphanumerics and nothing else.
///
/// The width covers what the bureau actually issues: `WPUFD4` at six,
/// `LNS14000000` at eleven, `CIU1010000000000A` at seventeen and
/// `JTS000000000000000JOL` at twenty-one.
static SERIES_ID: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Z0-9]{6,32}$").expect("SERIES_ID is a valid regex"));

/// Validate and canonicalise a series id.
///
/// Upper-cased because the bureau's ids are, and because neither the cache key
/// nor the outbound URL should depend on how the reader typed it. Checked
/// before anything is sent: a malformed id spends one of the day's requests to
/// be told it is malformed, and the id grammar is knowable here.
pub fn assert_bls_series_id(id: &str) -> Result<String> {
    let trimmed = id.trim().to_uppercase();
    if !SERIES_ID.is_match(&trimmed) {
        return Err(
            UpstreamError::bad_request(format!("\"{id}\" is not a valid BLS series id")).with_hint(
                "BLS ids are letters and digits with no punctuation, e.g. LNS14000000 \
                 (unemployment rate) or CUUR0000SA0 (CPI-U). Try `ECOS <words> bls`.",
            ),
        );
    }
    Ok(trimmed)
}

/* ---------------------------------------------------------------- periods */

/// A BLS `(year, period)` pair as the day the period begins, or `None` to drop
/// the row.
///
/// The bureau writes its period codes zero-padded — `M07`, `Q02`, `S01`, `A01`
/// — and [`crate::period::period_to_date`] reads the terminal's one period
/// vocabulary, where a quarter is `2026-Q3` and a month is `2026-M08`. So this
/// rewrites rather than re-decides: the digits are read, the code is spelled
/// the way the shared reader spells it, and every question about whether the
/// result is a real period is answered there. That matters most for the
/// aggregates the API interleaves with real readings — `M13` is the annual
/// average, `Q05` and `S03` are the same summary for quarterly and semiannual
/// series — which the shared reader already refuses, and which must therefore
/// never reach a chart through here either.
///
/// `A01` is an annual series' single reading and is dated to 1 January, which
/// is what the shared reader makes of a bare year.
pub fn period_date(year: &str, period: &str) -> Option<String> {
    let year = year.trim();
    let code = period.trim().to_uppercase();
    let mut chars = code.chars();
    let letter = chars.next()?;
    let digits = chars.as_str();

    // An empty tail is not a code, and a non-numeric one is not either.
    if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let n: u32 = digits.parse().ok()?;

    match letter {
        // `M13` reaches the shared reader as `2026-M13` and is refused there.
        'M' => period_to_date(&format!("{year}-M{n:02}")),
        // `Q05` becomes `2026-Q5`, which the shared reader's `[1-4]` refuses.
        'Q' => period_to_date(&format!("{year}-Q{n}")),
        // `S03` becomes `2026-S3`, refused the same way.
        'S' => period_to_date(&format!("{year}-S{n}")),
        // Annual: the reading covers the year, and is dated to its first day.
        'A' => period_to_date(year),
        _ => None,
    }
}

/* ----------------------------------------------------------------- values */

/// A BLS `value` field as a number, or `None` where the bureau states none.
///
/// Three spellings have to land on `None` rather than on `0.0`: the bureau's
/// missing-value marker `-`, an empty field, and anything else that is not a
/// number. The TypeScript this replaces read the field with JavaScript's
/// `Number()`, which answers `0` for an empty string and for a string of
/// spaces — a figure nobody published, drawn as a reading of zero on a chart
/// somebody trades off. Refusing all three here is a deliberate difference and
/// not an oversight.
///
/// Thousands separators are stripped because the bureau groups the large
/// series — nonfarm payrolls arrive as `"158,043"` — and `158.0` for 158
/// million jobs is worse than no reading at all.
pub fn observation_value(raw: &str) -> Option<f64> {
    let ungrouped: String = raw.chars().filter(|c| *c != ',').collect();
    let trimmed = ungrouped.trim();
    if trimmed.is_empty() || trimmed == "-" {
        return None;
    }
    trimmed
        .parse::<f64>()
        .ok()
        .filter(|value| value.is_finite())
}

/* ---------------------------------------------------------------- windows */

/// One request's worth of years.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct YearWindow {
    pub start: i32,
    pub end: i32,
}

/// The year windows covering `[from, to]`, each within the API's span cap.
///
/// Without this, "unemployment since 1948" is one request that comes back with
/// the last ten years in it and no indication that the other sixty-eight were
/// dropped — which reads as data rather than as a truncation, and is the single
/// most damaging thing this upstream does.
///
/// A `span` of zero would loop forever, so it is floored at one year; that is a
/// guard against a future edit rather than a case the constants can reach.
pub fn windows(from: i32, to: i32, span: i32) -> Vec<YearWindow> {
    let span = span.max(1);
    let mut out = Vec::new();
    let mut start = from;
    while start <= to {
        out.push(YearWindow {
            start,
            end: (start + span - 1).min(to),
        });
        start += span;
    }
    out
}

/* ------------------------------------------------------------ raw upstream */

/// The catalogue block, which only a keyed v2 request is documented to return.
///
/// Its keys are snake_case where the rest of the payload is camelCase, which is
/// the bureau's inconsistency and not a transcription error here.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct RawBlsCatalog {
    pub series_title: Option<String>,
    /// The units, in the bureau's words — "Percent", "Number in thousands".
    pub measure_data_type: Option<String>,
    /// "Seasonally Adjusted" or "Not Seasonally Adjusted".
    pub seasonality: Option<String>,
    /// The programme that publishes the series, e.g. "Current Population
    /// Survey".
    pub survey_name: Option<String>,
}

/// One reading. Everything is a string, including the figure.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RawBlsDatum {
    pub year: Option<String>,
    /// `M01`–`M13`, `Q01`–`Q05`, `S01`–`S03`, `A01`.
    pub period: Option<String>,
    /// "January", "Annual", "1st Quarter" — a label, never parsed.
    pub period_name: Option<String>,
    /// A decimal string, or `-` where the bureau publishes nothing.
    pub value: Option<String>,
    /// Present, as the string `"true"`, on the most recent reading only.
    pub latest: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RawBlsSeries {
    /// Capitalised `ID`, which no rename convention produces.
    #[serde(rename = "seriesID")]
    pub series_id: Option<String>,
    pub catalog: Option<RawBlsCatalog>,
    pub data: Option<Vec<RawBlsDatum>>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct RawBlsResults {
    pub series: Option<Vec<RawBlsSeries>>,
}

/// The envelope every answer arrives in, refusals included.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RawBlsResponse {
    /// `REQUEST_SUCCEEDED` or `REQUEST_NOT_PROCESSED`. The HTTP status is 200
    /// either way, so this field is the actual result of the request.
    pub status: Option<String>,
    /// Sentences, not codes. Non-empty on a success too — a request whose range
    /// runs past the latest print carries "No Data Available for Series … Year:
    /// 2026" while still succeeding, which is why the message is read for
    /// wording and the `status` for the verdict.
    pub message: Option<Vec<String>>,
    #[serde(rename = "Results")]
    pub results: Option<RawBlsResults>,
}

/// Wording the bureau uses when it is shedding a request rather than failing.
///
/// The live body says "the daily threshold … has been reached"; the registered
/// tier and the per-query series cap are worded with "limit" and "exceeded".
/// All three are the same thing to a reader — come back later, or register —
/// and none of them is a broken series id.
static THROTTLED: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)threshold|limit|exceed").expect("THROTTLED is a valid regex")
});

/// The configured key, treated as absent when it is blank.
fn api_key(state: &AppState) -> Option<&str> {
    state
        .config()
        .bls_api_key
        .as_deref()
        .map(str::trim)
        .filter(|key| !key.is_empty())
}

/// Which arm of the API a deployment is talking to, and what that arm allows.
///
/// Reported on the series as `source`, because how much history a chart can
/// hold and how many charts a session can draw both depend on it, and neither
/// is visible in the numbers themselves.
fn arm(state: &AppState) -> (&'static str, i32) {
    match api_key(state) {
        Some(_) => ("v2", SPAN_REGISTERED),
        None => ("v1", SPAN_UNREGISTERED),
    }
}

/// Turn a refused envelope into the error a reader can act on.
fn refusal(id: &str, payload: &RawBlsResponse, has_key: bool) -> UpstreamError {
    let message = payload
        .message
        .as_deref()
        .unwrap_or_default()
        .join(" ")
        .trim()
        .to_owned();

    let throttled = THROTTLED.is_match(&message);
    let text = if message.is_empty() {
        format!("The BLS refused the request for {id}")
    } else {
        message
    };

    let error = UpstreamError::new(
        text,
        if throttled {
            codes::RATE_LIMITED
        } else {
            codes::UPSTREAM_ERROR
        },
    );

    // The daily quota is the failure an operator will actually hit, and the
    // remedy — a free key — is not guessable from "REQUEST_NOT_PROCESSED".
    if throttled && !has_key {
        return error.with_hint(format!(
            "The unregistered BLS API allows {DAILY_QUOTA_UNREGISTERED} queries per IP per day. \
             A free key from https://data.bls.gov/registrationEngine/ raises that to 500 and \
             doubles the history per call — set BLS_API_KEY."
        ));
    }
    error
}

/// One window of one series, or `None` when the bureau knows the id and has
/// nothing for those years.
async fn fetch_window(
    state: &AppState,
    id: &str,
    window: YearWindow,
) -> Result<Option<RawBlsSeries>> {
    let key = api_key(state);
    let (version, _) = arm(state);

    let mut url = format!(
        "{}/{version}/timeseries/data/{id}?startyear={}&endyear={}",
        state.config().bls_api_base,
        window.start,
        window.end
    );
    if let Some(key) = key {
        url.push_str("&registrationkey=");
        url.push_str(&urlencoding::encode(key));
    }

    let payload: RawBlsResponse = state
        .http()
        .fetch_json(&url, FetchOptions::new().timeout(TIMEOUT).retries(RETRIES))
        .await?;

    // A status that is present and is not success is a refusal, whatever the
    // transport said. An absent status is tolerated: it is not how the bureau
    // answers, and refusing a body that carries readable data over a missing
    // envelope field would lose a chart to a field nobody reads.
    if let Some(status) = payload.status.as_deref() {
        if status != "REQUEST_SUCCEEDED" {
            return Err(refusal(id, &payload, key.is_some()));
        }
    }

    Ok(payload
        .results
        .and_then(|results| results.series)
        .and_then(|series| series.into_iter().next()))
}

/* ---------------------------------------------------------------- series */

fn this_year() -> i32 {
    OffsetDateTime::now_utc().year()
}

/// `2015-01-01` → 2015. A bare year is accepted too, and anything else falls
/// back rather than failing: a range this module cannot read is a range the
/// reader gets defaults for, not an error where a chart was asked for.
fn year_of(date: Option<&str>, fallback: i32) -> i32 {
    let head: String = date.unwrap_or_default().trim().chars().take(4).collect();
    match head.parse::<i32>() {
        Ok(year) if (1900..=2100).contains(&year) => year,
        _ => fallback,
    }
}

/// A series and its observations, windowed so nothing is silently truncated.
pub async fn get_series(
    state: &AppState,
    raw_id: &str,
    start: Option<&str>,
    end: Option<&str>,
) -> Result<DataSeriesResponse> {
    let id = assert_bls_series_id(raw_id)?;
    let (version, span) = arm(state);

    let end_year = year_of(end, this_year());
    // With no start, show one full window — the longest history one request
    // buys, rather than a decade of a keyed deployment's two.
    let start_year = year_of(start, end_year - span + 1).min(end_year);

    let cache_key = format!("bls:series:{id}:{start_year}:{end_year}:{version}");

    let response = state
        .cache()
        .cached(&cache_key, ttl::FRED, || async {
            let mut parts: Vec<RawBlsSeries> = Vec::new();
            // Sequential rather than parallel: the bureau counts requests
            // against a daily quota per IP, and a burst of eight is the fastest
            // way to spend it.
            for window in windows(start_year, end_year, span) {
                if let Some(part) = fetch_window(state, &id, window).await? {
                    parts.push(part);
                }
            }

            if parts
                .iter()
                .all(|part| part.data.as_deref().unwrap_or_default().is_empty())
            {
                // A well-formed id the bureau does not publish comes back as a
                // success with an empty `data` array, so "no such series" and
                // "no readings in these years" are the same answer on the wire
                // and have to be reported as the one question they leave open.
                return Err(UpstreamError::not_found(format!(
                    "The BLS has no data for {id} in {start_year}–{end_year}"
                ))
                .with_hint(
                    "Either the series id is wrong or it has no observations in that window. \
                     Try `ECOS <words> bls` to find an id.",
                ));
            }

            // Keyed by date so the windows merge in date order whatever order
            // they arrived in, and so a date restated by a later window keeps
            // the later reading — which is the revised one.
            let mut by_date: BTreeMap<String, Option<f64>> = BTreeMap::new();
            for part in &parts {
                for datum in part.data.as_deref().unwrap_or_default() {
                    let (Some(year), Some(period)) = (&datum.year, &datum.period) else {
                        continue;
                    };
                    let Some(date) = period_date(year, period) else {
                        continue;
                    };
                    by_date.insert(
                        date,
                        observation_value(datum.value.as_deref().unwrap_or_default()),
                    );
                }
            }

            let observations: Vec<DataObservation> = by_date
                .into_iter()
                .map(|(date, value)| DataObservation { date, value })
                .collect();

            let catalog = parts.iter().find_map(|part| part.catalog.as_ref());
            let known = catalogue_entry(&id);
            let units = catalog
                .and_then(|c| c.measure_data_type.clone())
                .or_else(|| known.map(|entry| entry.units.to_owned()))
                .unwrap_or_default();

            Ok(DataSeriesResponse {
                series: DataSeries {
                    provider: DataSource::Bls,
                    id: id.clone(),
                    title: catalog
                        .and_then(|c| c.series_title.clone())
                        .or_else(|| known.map(|entry| entry.title.to_owned()))
                        .unwrap_or_else(|| id.clone()),
                    units_short: units.clone(),
                    units,
                    frequency: known
                        .map(|entry| entry.frequency.to_owned())
                        .unwrap_or_default(),
                    seasonal_adjustment: catalog
                        .and_then(|c| c.seasonality.clone())
                        .unwrap_or_default(),
                    // The bureau publishes no revision timestamp on this route;
                    // an invented one would be read as a release time.
                    last_updated: String::new(),
                    observation_start: observations
                        .first()
                        .map(|o| o.date.clone())
                        .unwrap_or_default(),
                    observation_end: observations
                        .last()
                        .map(|o| o.date.clone())
                        .unwrap_or_default(),
                    notes: catalog
                        .and_then(|c| c.survey_name.as_deref())
                        .map(|survey| format!("Survey: {survey}"))
                        .unwrap_or_default(),
                    source: version.to_owned(),
                    source_url: format!("https://data.bls.gov/timeseries/{id}"),
                },
                observations,
            })
        })
        .await?;

    Ok(Arc::unwrap_or_clone(response))
}

/* -------------------------------------------------------------- catalogue */

/// One well-known series, so `ECOS` can answer before a reader knows an id.
#[derive(Debug, Clone, Copy)]
pub struct BlsCatalogueEntry {
    pub id: &'static str,
    pub title: &'static str,
    pub units: &'static str,
    pub frequency: &'static str,
    /// Words a reader would search for that are not in the title — `jobless`,
    /// `nfp`, `payrolls`.
    pub keywords: &'static str,
}

/// The headline BLS series, by id.
///
/// The bureau publishes no series-search API — its own site searches a database
/// with no public endpoint — so this is a curated list rather than a query. It
/// covers the prints that move markets; any other id still charts, this is only
/// what `ECOS` can offer before you know one.
pub const CATALOGUE: &[BlsCatalogueEntry] = &[
    BlsCatalogueEntry {
        id: "LNS14000000",
        title: "Unemployment rate, seasonally adjusted",
        units: "Percent",
        frequency: "Monthly",
        keywords: "jobless unemployment labour labor household survey",
    },
    BlsCatalogueEntry {
        id: "LNS11300000",
        title: "Labor force participation rate, seasonally adjusted",
        units: "Percent",
        frequency: "Monthly",
        keywords: "participation labour labor supply",
    },
    BlsCatalogueEntry {
        id: "LNS12300000",
        title: "Employment-population ratio, seasonally adjusted",
        units: "Percent",
        frequency: "Monthly",
        keywords: "employment population ratio labour labor",
    },
    BlsCatalogueEntry {
        id: "LNS13327709",
        title: "U-6 total unemployed plus marginally attached and part-time for economic reasons",
        units: "Percent",
        frequency: "Monthly",
        keywords: "u6 underemployment jobless broad unemployment",
    },
    BlsCatalogueEntry {
        id: "CES0000000001",
        title: "Total nonfarm employment, seasonally adjusted",
        units: "Thousands of persons",
        frequency: "Monthly",
        keywords: "payrolls nfp jobs establishment survey employment",
    },
    BlsCatalogueEntry {
        id: "CES0500000003",
        title: "Average hourly earnings, total private, seasonally adjusted",
        units: "Dollars per hour",
        frequency: "Monthly",
        keywords: "wages earnings pay ahe payrolls",
    },
    BlsCatalogueEntry {
        id: "CES0500000002",
        title: "Average weekly hours, total private, seasonally adjusted",
        units: "Hours",
        frequency: "Monthly",
        keywords: "hours worked payrolls",
    },
    BlsCatalogueEntry {
        id: "CUUR0000SA0",
        title: "CPI-U, all items, US city average, not seasonally adjusted",
        units: "Index 1982-84=100",
        frequency: "Monthly",
        keywords: "cpi inflation prices consumer headline",
    },
    BlsCatalogueEntry {
        id: "CUSR0000SA0",
        title: "CPI-U, all items, US city average, seasonally adjusted",
        units: "Index 1982-84=100",
        frequency: "Monthly",
        keywords: "cpi inflation prices consumer headline",
    },
    BlsCatalogueEntry {
        id: "CUUR0000SA0L1E",
        title: "CPI-U, all items less food and energy, not seasonally adjusted",
        units: "Index 1982-84=100",
        frequency: "Monthly",
        keywords: "core cpi inflation prices consumer",
    },
    BlsCatalogueEntry {
        id: "CUSR0000SA0L1E",
        title: "CPI-U, all items less food and energy, seasonally adjusted",
        units: "Index 1982-84=100",
        frequency: "Monthly",
        keywords: "core cpi inflation prices consumer",
    },
    BlsCatalogueEntry {
        id: "CUUR0000SETB01",
        title: "CPI-U, gasoline (all types), US city average",
        units: "Index 1982-84=100",
        frequency: "Monthly",
        keywords: "gasoline petrol fuel energy prices cpi",
    },
    BlsCatalogueEntry {
        id: "CUUR0000SAF1",
        title: "CPI-U, food, US city average",
        units: "Index 1982-84=100",
        frequency: "Monthly",
        keywords: "food groceries prices cpi",
    },
    BlsCatalogueEntry {
        id: "CUUR0000SAH1",
        title: "CPI-U, shelter, US city average",
        units: "Index 1982-84=100",
        frequency: "Monthly",
        keywords: "shelter rent housing prices cpi",
    },
    BlsCatalogueEntry {
        id: "WPUFD4",
        title: "PPI final demand",
        units: "Index Nov 2009=100",
        frequency: "Monthly",
        keywords: "ppi producer prices wholesale inflation",
    },
    BlsCatalogueEntry {
        id: "WPUFD49104",
        title: "PPI final demand less foods, energy and trade services",
        units: "Index",
        frequency: "Monthly",
        keywords: "core ppi producer prices wholesale inflation",
    },
    BlsCatalogueEntry {
        id: "PRS85006092",
        title: "Nonfarm business sector labour productivity, percent change from previous quarter",
        units: "Percent",
        frequency: "Quarterly",
        keywords: "productivity output per hour",
    },
    BlsCatalogueEntry {
        id: "CIU1010000000000A",
        title: "Employment Cost Index, total compensation, all civilian workers",
        units: "Percent change, 12-month",
        frequency: "Quarterly",
        keywords: "eci wages compensation labour costs",
    },
    BlsCatalogueEntry {
        id: "JTS000000000000000JOL",
        title: "Job openings, total nonfarm, seasonally adjusted (JOLTS)",
        units: "Thousands",
        frequency: "Monthly",
        keywords: "jolts vacancies job openings labour demand",
    },
    BlsCatalogueEntry {
        id: "JTS000000000000000QUR",
        title: "Quits rate, total nonfarm, seasonally adjusted (JOLTS)",
        units: "Percent",
        frequency: "Monthly",
        keywords: "jolts quits turnover labour",
    },
];

fn catalogue_entry(id: &str) -> Option<&'static BlsCatalogueEntry> {
    CATALOGUE.iter().find(|entry| entry.id == id)
}

/// Rank the curated catalogue against a query.
///
/// Every term has to appear somewhere, so `bls oil` does not return the whole
/// list; a term in the title counts double, so the series a reader is naming
/// outranks one that merely lists the word as a keyword. A query with no terms
/// scores nothing and matches nothing, rather than returning the catalogue to
/// somebody who asked for nothing.
pub fn search_catalogue(query: &str, limit: usize) -> Vec<DataSearchResult> {
    let terms: Vec<String> = query
        .to_lowercase()
        .split_whitespace()
        .map(str::to_owned)
        .collect();

    let mut scored: Vec<(u32, &'static BlsCatalogueEntry)> = CATALOGUE
        .iter()
        .filter_map(|entry| {
            let haystack =
                format!("{} {} {}", entry.id, entry.title, entry.keywords).to_lowercase();
            let title = entry.title.to_lowercase();
            if !terms.iter().all(|term| haystack.contains(term)) {
                return None;
            }
            let score: u32 = terms
                .iter()
                .map(|term| if title.contains(term) { 2 } else { 1 })
                .sum();
            (score > 0).then_some((score, entry))
        })
        .collect();

    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.title.cmp(b.1.title)));

    scored
        .into_iter()
        .take(limit)
        .map(|(_, entry)| DataSearchResult {
            provider: DataSource::Bls,
            // The curated list is not an arm of the publisher — nothing was
            // asked of the bureau — so there is no arm to name.
            source: None,
            id: entry.id.to_owned(),
            title: entry.title.to_owned(),
            units: Some(entry.units.to_owned()),
            frequency: Some(entry.frequency.to_owned()),
            seasonal_adjustment: None,
            observation_range: None,
        })
        .collect()
}

/// Search the curated catalogue. Reads no network, so it cannot fail.
pub async fn search(_state: &AppState, query: &str, limit: usize) -> Result<Vec<DataSearchResult>> {
    Ok(search_catalogue(query, limit))
}

/// `None` when this deployment can use the publisher; a string is the
/// operator-facing reason it cannot.
///
/// Always `None`: the v1 arm is anonymous, so a deployment with no key is a
/// slower deployment rather than an unavailable one. What the absence costs —
/// twenty-five queries a day and ten years of history per call — is stated on
/// the registry row, which is where `SRC` reads it from.
pub fn unavailable(_state: &AppState) -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    //! BLS parser and wire tests.
    //!
    //! The container these were written in had already spent its address's
    //! twenty-five anonymous queries for the day, so the *successful* body here
    //! is hand-built to the bureau's documented shape rather than captured. The
    //! refusal is not: `fixtures/bls_throttled.json` is exactly what
    //! `api.bls.gov` returned, HTTP 200 and all, and the test that reads it is
    //! the one that matters most — every other failure mode of this upstream
    //! also arrives as a 200.

    use super::*;
    use serde_json::json;
    use wiremock::matchers::{method, path as path_matcher, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use crate::config::Config;

    /// A v1 answer for `LNS14000000`, 2024, in the documented shape.
    ///
    /// Five rows, kept for what each one traps:
    ///
    /// * `M13` — the annual average, first in the array as the bureau sends it,
    ///   and never a thirteenth month.
    /// * `M04` — the value edited to the bureau's `-` marker, so the missing
    ///   reading is asserted to be `None` and not `0.0`.
    /// * `M03`, `M02`, `M01` — real readings, newest first, which is the order
    ///   the API answers in and the opposite of the order a chart wants.
    ///
    /// The figures are the published 2024 seasonally-adjusted rates apart from
    /// April; `latest` and `footnotes` ride along because the payload carries
    /// them and nothing here may choke on a field it does not read.
    const SERIES_JSON: &str = include_str!("fixtures/bls_series.json");

    /// The live refusal, captured with curl. See the module docs.
    const THROTTLED_JSON: &str = include_str!("fixtures/bls_throttled.json");

    fn series_fixture() -> serde_json::Value {
        serde_json::from_str(SERIES_JSON).expect("the series fixture parses")
    }

    fn throttled_fixture() -> serde_json::Value {
        serde_json::from_str(THROTTLED_JSON).expect("the captured refusal parses")
    }

    /// `(date, value)` pairs, since the wire type carries no `PartialEq`.
    fn rows(observations: &[DataObservation]) -> Vec<(&str, Option<f64>)> {
        observations
            .iter()
            .map(|observation| (observation.date.as_str(), observation.value))
            .collect()
    }

    fn state_for(server: &MockServer) -> AppState {
        AppState::new(Config {
            bls_api_base: server.uri(),
            ..Config::default()
        })
    }

    fn keyed_state_for(server: &MockServer) -> AppState {
        AppState::new(Config {
            bls_api_base: server.uri(),
            bls_api_key: Some("0123456789abcdef0123456789abcdef".to_owned()),
            ..Config::default()
        })
    }

    /* --------------------------------------------------------------- periods */

    #[test]
    fn reads_monthly_quarterly_semiannual_and_annual_codes() {
        assert_eq!(period_date("2026", "M07").as_deref(), Some("2026-07-01"));
        assert_eq!(period_date("2026", "Q02").as_deref(), Some("2026-04-01"));
        assert_eq!(period_date("2026", "S02").as_deref(), Some("2026-07-01"));
        assert_eq!(period_date("2026", "A01").as_deref(), Some("2026-01-01"));
    }

    #[test]
    fn drops_the_aggregate_codes_the_api_interleaves_with_real_readings() {
        // `M13` is the annual *average*, sitting in the same array as M01–M12.
        // Charting it would draw thirteen points a year, one of them the mean
        // of the other twelve. `Q05` and `S03` are the same summary for the
        // quarterly and semiannual series.
        assert_eq!(period_date("2026", "M13"), None);
        assert_eq!(period_date("2026", "Q05"), None);
        assert_eq!(period_date("2026", "S03"), None);
    }

    #[test]
    fn the_refusal_relied_on_is_the_shared_periods_readers_own() {
        // Stated as a test so that a future edit to `crate::period` that let
        // `M13` through would fail here rather than quietly put an annual
        // average on a monthly chart.
        assert_eq!(period_to_date("2026-M13"), None);
        assert_eq!(period_to_date("2026-M07").as_deref(), Some("2026-07-01"));
    }

    #[test]
    fn rejects_a_malformed_year() {
        assert_eq!(period_date("20xx", "M01"), None);
        assert_eq!(period_date("", "M01"), None);
        assert_eq!(period_date("20xx", "A01"), None);
    }

    #[test]
    fn rejects_a_period_that_is_not_a_period() {
        assert_eq!(period_date("2026", ""), None);
        assert_eq!(period_date("2026", "M"), None);
        assert_eq!(period_date("2026", "MXX"), None);
        assert_eq!(period_date("2026", "X01"), None);
        assert_eq!(period_date("2026", "Annual"), None);
    }

    #[test]
    fn reads_a_code_however_it_is_cased_or_spaced() {
        assert_eq!(
            period_date(" 2026 ", " m07 ").as_deref(),
            Some("2026-07-01")
        );
        assert_eq!(period_date("2026", "q3").as_deref(), Some("2026-07-01"));
    }

    /* ---------------------------------------------------------------- values */

    #[test]
    fn reads_a_plain_and_a_grouped_figure() {
        assert_eq!(observation_value("4.1"), Some(4.1));
        assert_eq!(observation_value("-0.3"), Some(-0.3));
        // Nonfarm payrolls arrive grouped; 158.0 for 158 million jobs would be
        // worse than no reading at all.
        assert_eq!(observation_value("158,043"), Some(158_043.0));
    }

    #[test]
    fn the_bureaus_missing_marker_is_not_a_zero() {
        // A zero in an unemployment series is a reading somebody acts on, and
        // nobody published it.
        assert_eq!(observation_value("-"), None);
        assert_eq!(observation_value(" - "), None);
    }

    #[test]
    fn an_empty_field_is_not_a_zero_either() {
        // JavaScript's `Number("")` is 0 and `Number("   ")` is 0, which the
        // TypeScript this replaces would have charted. Rust refuses both, and
        // that difference is the point.
        assert_eq!(observation_value(""), None);
        assert_eq!(observation_value("   "), None);
    }

    #[test]
    fn refuses_a_value_that_is_not_a_number() {
        assert_eq!(observation_value("n/a"), None);
        assert_eq!(observation_value("4.1%"), None);
        assert_eq!(observation_value("NaN"), None);
        assert_eq!(observation_value("inf"), None);
    }

    /* -------------------------------------------------------------- id shape */

    #[test]
    fn accepts_and_upper_cases_a_real_id() {
        assert_eq!(
            assert_bls_series_id(" lns14000000 ").expect("a real id"),
            "LNS14000000"
        );
        assert_eq!(
            assert_bls_series_id("JTS000000000000000JOL").expect("a real id"),
            "JTS000000000000000JOL"
        );
        assert_eq!(assert_bls_series_id("WPUFD4").expect("a real id"), "WPUFD4");
    }

    #[test]
    fn refuses_an_id_with_punctuation_and_says_what_one_looks_like() {
        let err = assert_bls_series_id("not a series").expect_err("not an id");
        assert_eq!(err.code, codes::BAD_REQUEST);
        assert!(err.message.contains("not a valid BLS series id"));
        assert!(err.hint.expect("a hint").contains("LNS14000000"));

        assert!(assert_bls_series_id("LNS-14000000").is_err());
        assert!(assert_bls_series_id("UNRATE").is_err(), "too short");
        assert!(assert_bls_series_id("").is_err());
    }

    /* --------------------------------------------------------------- windows */

    #[test]
    fn splits_a_range_into_spans_the_api_will_actually_accept() {
        assert_eq!(
            windows(2000, 2029, 10),
            [
                YearWindow {
                    start: 2000,
                    end: 2009
                },
                YearWindow {
                    start: 2010,
                    end: 2019
                },
                YearWindow {
                    start: 2020,
                    end: 2029
                },
            ]
        );
    }

    #[test]
    fn does_not_extend_the_last_window_past_the_requested_end() {
        assert_eq!(
            windows(2020, 2026, 10),
            [YearWindow {
                start: 2020,
                end: 2026
            }]
        );
    }

    #[test]
    fn returns_one_window_for_a_single_year() {
        assert_eq!(
            windows(2026, 2026, 20),
            [YearWindow {
                start: 2026,
                end: 2026
            }]
        );
    }

    #[test]
    fn a_key_halves_the_number_of_requests_history_costs() {
        // 1948 to 2027 is eight requests unregistered and four with a key, and
        // the whole reason the registered span is worth reporting as `source`.
        assert_eq!(windows(1948, 2027, SPAN_UNREGISTERED).len(), 8);
        assert_eq!(windows(1948, 2027, SPAN_REGISTERED).len(), 4);
    }

    #[test]
    fn an_end_before_the_start_asks_for_nothing() {
        assert!(windows(2026, 2020, 10).is_empty());
        // And a span of zero terminates rather than looping forever.
        assert_eq!(windows(2020, 2021, 0).len(), 2);
    }

    #[test]
    fn reads_a_year_out_of_a_date_a_bare_year_or_neither() {
        assert_eq!(year_of(Some("2015-01-01"), 2026), 2015);
        assert_eq!(year_of(Some("2015"), 2026), 2015);
        assert_eq!(year_of(Some("not a date"), 2026), 2026);
        assert_eq!(year_of(Some("1776-07-04"), 2026), 2026, "before the range");
        assert_eq!(year_of(None, 2026), 2026);
    }

    /* ------------------------------------------------------------- the wire */

    async fn mount_series(server: &MockServer) {
        Mock::given(method("GET"))
            .and(path_matcher("/v1/timeseries/data/LNS14000000"))
            .respond_with(ResponseTemplate::new(200).set_body_json(series_fixture()))
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn assembles_a_series_and_drops_the_annual_average() {
        let server = MockServer::start().await;
        mount_series(&server).await;

        let answer = get_series(
            &state_for(&server),
            "LNS14000000",
            Some("2024"),
            Some("2024"),
        )
        .await
        .expect("the series resolves");

        // Four monthlies from five rows: `M13` is the average of the year, not
        // a thirteenth month of it. And April is a gap, not a zero.
        assert_eq!(
            rows(&answer.observations),
            [
                ("2024-01-01", Some(3.7)),
                ("2024-02-01", Some(3.9)),
                ("2024-03-01", Some(3.8)),
                ("2024-04-01", None),
            ]
        );
        assert_eq!(answer.series.observation_start, "2024-01-01");
        assert_eq!(answer.series.observation_end, "2024-04-01");
    }

    #[tokio::test]
    async fn names_the_publisher_the_arm_and_the_page_a_reader_can_check() {
        let server = MockServer::start().await;
        mount_series(&server).await;

        let answer = get_series(
            &state_for(&server),
            "lns14000000",
            Some("2024"),
            Some("2024"),
        )
        .await
        .expect("the series resolves");

        assert_eq!(answer.series.provider, DataSource::Bls);
        assert_eq!(answer.series.id, "LNS14000000");
        // No key, so the anonymous arm answered and the panel says so.
        assert_eq!(answer.series.source, "v1");
        assert_eq!(
            answer.series.source_url,
            "https://data.bls.gov/timeseries/LNS14000000"
        );
        // The payload carries no catalogue, so the curated row supplies the
        // words rather than the panel heading itself with an id.
        assert_eq!(
            answer.series.title,
            "Unemployment rate, seasonally adjusted"
        );
        assert_eq!(answer.series.units, "Percent");
        assert_eq!(answer.series.units_short, "Percent");
        assert_eq!(answer.series.frequency, "Monthly");
        assert_eq!(answer.series.seasonal_adjustment, "");
        assert_eq!(answer.series.notes, "");
    }

    #[tokio::test]
    async fn an_id_nobody_curated_still_charts_under_its_own_name() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/v1/timeseries/data/SMU19000000000000001"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "status": "REQUEST_SUCCEEDED",
                "responseTime": 91,
                "message": [],
                "Results": { "series": [{
                    "seriesID": "SMU19000000000000001",
                    "data": [{ "year": "2024", "period": "M01", "periodName": "January", "value": "1,584.3" }]
                }]}
            })))
            .mount(&server)
            .await;

        let answer = get_series(
            &state_for(&server),
            "SMU19000000000000001",
            Some("2024"),
            Some("2024"),
        )
        .await
        .expect("an uncurated series still charts");

        assert_eq!(answer.series.title, "SMU19000000000000001");
        assert_eq!(answer.series.units, "");
        assert_eq!(rows(&answer.observations), [("2024-01-01", Some(1584.3))]);
    }

    #[tokio::test]
    async fn reads_the_catalogue_block_where_a_payload_carries_one() {
        // The GET form does not ask for it, so this is what a keyed payload
        // that volunteers one is made of rather than what we request.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/v2/timeseries/data/LNS14000000"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "status": "REQUEST_SUCCEEDED",
                "responseTime": 118,
                "message": [],
                "Results": { "series": [{
                    "seriesID": "LNS14000000",
                    "catalog": {
                        "series_title": "(Seas) Unemployment Rate",
                        "measure_data_type": "Percent or rate",
                        "seasonality": "Seasonally Adjusted",
                        "survey_name": "Labor Force Statistics from the Current Population Survey"
                    },
                    "data": [{ "year": "2024", "period": "M01", "periodName": "January", "value": "3.7" }]
                }]}
            })))
            .mount(&server)
            .await;

        let answer = get_series(
            &keyed_state_for(&server),
            "LNS14000000",
            Some("2024"),
            Some("2024"),
        )
        .await
        .expect("the series resolves");

        // The publisher's own words outrank the curated ones where it states
        // them, because the curated list is a stand-in and this is the source.
        assert_eq!(answer.series.title, "(Seas) Unemployment Rate");
        assert_eq!(answer.series.units, "Percent or rate");
        assert_eq!(answer.series.seasonal_adjustment, "Seasonally Adjusted");
        assert_eq!(
            answer.series.notes,
            "Survey: Labor Force Statistics from the Current Population Survey"
        );
        assert_eq!(answer.series.source, "v2");
    }

    #[tokio::test]
    async fn splits_a_long_range_into_windows_the_api_would_accept() {
        // 35 years unregistered is four ten-year requests. Sent as one, the
        // bureau would answer with the last decade and say nothing about the
        // other twenty-five years.
        let server = MockServer::start().await;
        for (start, end) in [
            ("1990", "1999"),
            ("2000", "2009"),
            ("2010", "2019"),
            ("2020", "2024"),
        ] {
            Mock::given(method("GET"))
                .and(path_matcher("/v1/timeseries/data/CUUR0000SA0"))
                .and(query_param("startyear", start))
                .and(query_param("endyear", end))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "status": "REQUEST_SUCCEEDED",
                    "message": [],
                    "Results": { "series": [{
                        "seriesID": "CUUR0000SA0",
                        "data": [{ "year": start, "period": "M01", "value": "100.0" }]
                    }]}
                })))
                .expect(1)
                .mount(&server)
                .await;
        }

        let answer = get_series(
            &state_for(&server),
            "CUUR0000SA0",
            Some("1990-01-01"),
            Some("2024-12-31"),
        )
        .await
        .expect("the windows merge into one series");

        // One reading per window, in date order however the windows arrived.
        assert_eq!(
            rows(&answer.observations),
            [
                ("1990-01-01", Some(100.0)),
                ("2000-01-01", Some(100.0)),
                ("2010-01-01", Some(100.0)),
                ("2020-01-01", Some(100.0)),
            ]
        );
    }

    #[tokio::test]
    async fn a_key_widens_the_window_to_twenty_years_and_asks_the_v2_arm() {
        let server = MockServer::start().await;
        for (start, end) in [("1990", "2009"), ("2010", "2024")] {
            Mock::given(method("GET"))
                .and(path_matcher("/v2/timeseries/data/CUUR0000SA0"))
                .and(query_param("startyear", start))
                .and(query_param("endyear", end))
                .and(query_param(
                    "registrationkey",
                    "0123456789abcdef0123456789abcdef",
                ))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "status": "REQUEST_SUCCEEDED",
                    "message": [],
                    "Results": { "series": [{
                        "seriesID": "CUUR0000SA0",
                        "data": [{ "year": start, "period": "M01", "value": "100.0" }]
                    }]}
                })))
                .expect(1)
                .mount(&server)
                .await;
        }

        let answer = get_series(
            &keyed_state_for(&server),
            "CUUR0000SA0",
            Some("1990"),
            Some("2024"),
        )
        .await
        .expect("the windows merge into one series");

        assert_eq!(answer.series.source, "v2");
        assert_eq!(answer.observations.len(), 2);
    }

    #[tokio::test]
    async fn with_no_start_it_asks_for_one_full_window_of_history() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/v1/timeseries/data/LNS14000000"))
            .and(query_param("startyear", "2015"))
            .and(query_param("endyear", "2024"))
            .respond_with(ResponseTemplate::new(200).set_body_json(series_fixture()))
            .expect(1)
            .mount(&server)
            .await;

        get_series(&state_for(&server), "LNS14000000", None, Some("2024"))
            .await
            .expect("the series resolves");
    }

    #[tokio::test]
    async fn a_start_after_the_end_still_asks_for_one_year() {
        // Rather than looping to nothing and reporting an empty series, which
        // would read as "the bureau has no data" for a reader's typo.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/v1/timeseries/data/LNS14000000"))
            .and(query_param("startyear", "2024"))
            .and(query_param("endyear", "2024"))
            .respond_with(ResponseTemplate::new(200).set_body_json(series_fixture()))
            .expect(1)
            .mount(&server)
            .await;

        get_series(
            &state_for(&server),
            "LNS14000000",
            Some("2030"),
            Some("2024"),
        )
        .await
        .expect("the series resolves");
    }

    #[tokio::test]
    async fn refuses_a_malformed_id_before_spending_a_request_on_it() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(series_fixture()))
            .expect(0)
            .mount(&server)
            .await;

        let err = get_series(&state_for(&server), "not a series", None, None)
            .await
            .expect_err("a malformed id is refused here");
        assert_eq!(err.code, codes::BAD_REQUEST);
    }

    #[tokio::test]
    async fn reads_the_two_hundred_that_says_the_daily_quota_is_spent() {
        // The captured body. HTTP 200, `application/json`, and not an answer.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/v1/timeseries/data/LNS14000000"))
            .respond_with(ResponseTemplate::new(200).set_body_json(throttled_fixture()))
            .mount(&server)
            .await;

        let err = get_series(
            &state_for(&server),
            "LNS14000000",
            Some("2024"),
            Some("2024"),
        )
        .await
        .expect_err("a refusal is not a series");

        assert_eq!(err.code, codes::RATE_LIMITED);
        assert!(err.message.contains("daily threshold"));
        let hint = err.hint.expect("the remedy is not guessable from the body");
        assert!(hint.contains("25 queries per IP per day"));
        assert!(hint.contains("BLS_API_KEY"));
    }

    #[tokio::test]
    async fn a_keyed_deployment_is_not_told_to_go_and_get_a_key() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/v2/timeseries/data/LNS14000000"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "status": "REQUEST_NOT_PROCESSED",
                "responseTime": 0,
                "message": ["Request could not be serviced, as the daily threshold for total \
                             number of requests allocated to the user with registration key \
                             0123456789abcdef0123456789abcdef has been reached."],
                "Results": {}
            })))
            .mount(&server)
            .await;

        let err = get_series(
            &keyed_state_for(&server),
            "LNS14000000",
            Some("2024"),
            Some("2024"),
        )
        .await
        .expect_err("a refusal is not a series");

        assert_eq!(err.code, codes::RATE_LIMITED);
        assert_eq!(err.hint, None);
    }

    #[tokio::test]
    async fn a_refusal_that_is_not_a_throttle_is_an_upstream_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/v1/timeseries/data/LNS14000000"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "status": "REQUEST_NOT_PROCESSED",
                "responseTime": 12,
                "message": ["Sorry, the service is currently unavailable."],
                "Results": {}
            })))
            .mount(&server)
            .await;

        let err = get_series(
            &state_for(&server),
            "LNS14000000",
            Some("2024"),
            Some("2024"),
        )
        .await
        .expect_err("a refusal is not a series");

        assert_eq!(err.code, codes::UPSTREAM_ERROR);
        assert_eq!(err.message, "Sorry, the service is currently unavailable.");
        assert_eq!(err.hint, None);
    }

    #[tokio::test]
    async fn a_refusal_with_nothing_to_say_still_names_the_series() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/v1/timeseries/data/LNS14000000"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "status": "REQUEST_NOT_PROCESSED",
                "Results": {}
            })))
            .mount(&server)
            .await;

        let err = get_series(
            &state_for(&server),
            "LNS14000000",
            Some("2024"),
            Some("2024"),
        )
        .await
        .expect_err("a refusal is not a series");
        assert_eq!(err.message, "The BLS refused the request for LNS14000000");
    }

    #[tokio::test]
    async fn an_id_the_bureau_does_not_publish_is_not_found_rather_than_empty() {
        // A well-formed id nobody publishes succeeds, with an empty `data`
        // array — indistinguishable on the wire from a real series with no
        // readings in the window, so the message has to leave both open.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/v1/timeseries/data/ZZZ99999999"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "status": "REQUEST_SUCCEEDED",
                "responseTime": 33,
                "message": ["No Data Available for Series ZZZ99999999 Year: 2024"],
                "Results": { "series": [{ "seriesID": "ZZZ99999999", "data": [] }] }
            })))
            .mount(&server)
            .await;

        let err = get_series(
            &state_for(&server),
            "ZZZ99999999",
            Some("2024"),
            Some("2024"),
        )
        .await
        .expect_err("an empty series is not a series");

        assert_eq!(err.code, codes::NOT_FOUND);
        assert!(err.message.contains("2024–2024"));
        assert!(err.hint.expect("a hint").contains("ECOS"));
    }

    #[tokio::test]
    async fn a_body_with_no_series_at_all_is_not_found_too() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/v1/timeseries/data/LNS14000000"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "status": "REQUEST_SUCCEEDED",
                "message": [],
                "Results": {}
            })))
            .mount(&server)
            .await;

        let err = get_series(
            &state_for(&server),
            "LNS14000000",
            Some("2024"),
            Some("2024"),
        )
        .await
        .expect_err("nothing is not a series");
        assert_eq!(err.code, codes::NOT_FOUND);
    }

    #[tokio::test]
    async fn a_row_the_terminal_cannot_date_is_dropped_rather_than_guessed_at() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/v1/timeseries/data/LNS14000000"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "status": "REQUEST_SUCCEEDED",
                "message": [],
                "Results": { "series": [{
                    "seriesID": "LNS14000000",
                    "data": [
                        { "year": "2024", "period": "M01", "value": "3.7" },
                        { "year": "2024", "period": "X99", "value": "9.9" },
                        { "year": "twenty", "period": "M02", "value": "9.9" },
                        { "period": "M03", "value": "9.9" }
                    ]
                }]}
            })))
            .mount(&server)
            .await;

        let answer = get_series(
            &state_for(&server),
            "LNS14000000",
            Some("2024"),
            Some("2024"),
        )
        .await
        .expect("the readable row still charts");
        assert_eq!(rows(&answer.observations), [("2024-01-01", Some(3.7))]);
    }

    #[tokio::test]
    async fn the_assembled_series_is_cached_rather_than_re_asked() {
        // Each request is one of twenty-five for the day, and four panels on
        // one series must not be four of them.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/v1/timeseries/data/LNS14000000"))
            .respond_with(ResponseTemplate::new(200).set_body_json(series_fixture()))
            .expect(1)
            .mount(&server)
            .await;

        let state = state_for(&server);
        let first = get_series(&state, "LNS14000000", Some("2024"), Some("2024"))
            .await
            .expect("the series resolves");
        let second = get_series(&state, "LNS14000000", Some("2024"), Some("2024"))
            .await
            .expect("the series resolves");

        assert_eq!(first.observations.len(), second.observations.len());
    }

    #[tokio::test]
    async fn a_refusal_is_not_cached_as_though_it_were_an_answer() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/v1/timeseries/data/LNS14000000"))
            .respond_with(ResponseTemplate::new(200).set_body_json(throttled_fixture()))
            .up_to_n_times(1)
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path_matcher("/v1/timeseries/data/LNS14000000"))
            .respond_with(ResponseTemplate::new(200).set_body_json(series_fixture()))
            .expect(1)
            .mount(&server)
            .await;

        let state = state_for(&server);
        get_series(&state, "LNS14000000", Some("2024"), Some("2024"))
            .await
            .expect_err("the quota was spent");
        // Tomorrow's first request must not be served yesterday's refusal.
        let answer = get_series(&state, "LNS14000000", Some("2024"), Some("2024"))
            .await
            .expect("the second ask gets through");
        assert_eq!(answer.observations.len(), 4);
    }

    /* ---------------------------------------------------------------- search */

    #[tokio::test]
    async fn search_ranks_a_title_match_above_a_keyword_match() {
        let state = AppState::new(Config::default());
        let results = search(&state, "unemployment", 10)
            .await
            .expect("the catalogue answers");

        assert_eq!(results[0].id, "LNS14000000");
        assert_eq!(results[0].provider, DataSource::Bls);
        assert_eq!(results[0].units.as_deref(), Some("Percent"));
        assert_eq!(results[0].frequency.as_deref(), Some("Monthly"));
        // Nothing was asked of the bureau, so no arm of it answered.
        assert_eq!(results[0].source, None);
        // U-6 names unemployment in its title too, and the payrolls series does
        // not name it at all.
        assert!(results.iter().any(|row| row.id == "LNS13327709"));
        assert!(!results.iter().any(|row| row.id == "CES0500000002"));
    }

    #[tokio::test]
    async fn search_requires_every_term_to_match() {
        let state = AppState::new(Config::default());
        let results = search(&state, "core cpi", 10)
            .await
            .expect("the catalogue answers");

        assert!(results
            .iter()
            .all(|row| row.title.to_lowercase().contains("less food and energy")));
        assert_eq!(results.len(), 2);
    }

    #[tokio::test]
    async fn search_matches_a_keyword_that_is_in_no_title() {
        let state = AppState::new(Config::default());
        let results = search(&state, "nfp", 10)
            .await
            .expect("the catalogue answers");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "CES0000000001");
    }

    #[tokio::test]
    async fn search_finds_a_series_by_its_id() {
        let state = AppState::new(Config::default());
        let results = search(&state, "cuur0000sa0l1e", 10)
            .await
            .expect("the catalogue answers");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "CUUR0000SA0L1E");
    }

    #[tokio::test]
    async fn search_honours_the_limit_it_is_given() {
        let state = AppState::new(Config::default());
        let results = search(&state, "cpi", 2)
            .await
            .expect("the catalogue answers");
        assert_eq!(results.len(), 2);
    }

    #[tokio::test]
    async fn search_for_nothing_matches_nothing() {
        // A blank query must not hand back the whole curated list as though it
        // were a result set.
        let state = AppState::new(Config::default());
        assert!(search(&state, "", 10)
            .await
            .expect("the catalogue answers")
            .is_empty());
        assert!(search(&state, "   ", 10)
            .await
            .expect("the catalogue answers")
            .is_empty());
        assert!(search(&state, "zzzz", 10)
            .await
            .expect("the catalogue answers")
            .is_empty());
    }

    #[test]
    fn every_curated_id_is_one_this_module_would_accept() {
        // A catalogue row a reader cannot then chart would be worse than no row.
        for entry in CATALOGUE {
            assert_eq!(
                assert_bls_series_id(entry.id).as_deref(),
                Ok(entry.id),
                "{} is not a valid BLS id",
                entry.id
            );
        }
    }

    #[test]
    fn the_publisher_is_available_without_a_key() {
        // The v1 arm is anonymous, so an absent key is a slower deployment
        // rather than an unavailable publisher.
        assert_eq!(unavailable(&AppState::new(Config::default())), None);
        assert_eq!(
            unavailable(&AppState::new(Config {
                bls_api_key: Some("0123456789abcdef0123456789abcdef".to_owned()),
                ..Config::default()
            })),
            None
        );
    }
}
