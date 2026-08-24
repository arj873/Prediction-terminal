//! SDMX — the dialect the ECB, the IMF and the OECD all answer in.
//!
//! All three publish through an SDMX 2.1 REST service, all three will hand back
//! CSV, and all three then disagree about everything else: the ECB names the
//! value column `OBS_VALUE` and the title `TITLE`, the OECD repeats every
//! dimension as a code column *and* a label column and calls the title
//! `Measure`, the IMF spells frequency `FREQUENCY` where the other two spell it
//! `FREQ`. What they genuinely share is the two columns that matter —
//! `TIME_PERIOD` and `OBS_VALUE` — and the period vocabulary written into the
//! SDMX standard, which [`crate::period::period_to_date`] already reads.
//!
//! So this module owns the hard parts once: picking the descriptive columns out
//! of a document whose other headers are not knowable in advance, refusing a key
//! that quietly selected more than one series, and the fetch/cache/assemble
//! path. [`ecb`](crate::sources::ecb), [`imf`](crate::sources::imf) and
//! [`oecd`](crate::sources::oecd) are then each an [`SdmxAgency`] — a base URL,
//! a URL grammar and a catalogue — and nothing else. Three copies of this reader
//! is exactly the outcome the split exists to prevent.
//!
//! The failure worth naming up front is the quiet one. A key that
//! under-specifies its dimensions is legal SDMX: it matches every series beneath
//! it and the service answers HTTP 200 with a perfectly well-formed body holding
//! all of them interleaved. Read as one series that draws a line alternating
//! between countries. [`assert_single_series`] refuses it instead.

use std::collections::HashMap;
use std::sync::LazyLock;
use std::time::Duration;

use regex::Regex;
use terminal_core::types::{
    DataObservation, DataSearchResult, DataSeries, DataSeriesResponse, DataSource,
};

use crate::app::AppState;
use crate::cache::ttl;
use crate::error::{codes, Result, UpstreamError};
use crate::http::FetchOptions;
use crate::period::period_to_date;

/// A single attempt's budget. The OECD in particular takes tens of seconds to
/// assemble a long key, and the shared 20s default times out on it while the
/// service is working perfectly well.
const SDMX_TIMEOUT: Duration = Duration::from_secs(40);

/* ------------------------------------------------------------ the columns */

/// Column names each concept goes by, across the three agencies.
///
/// Order is preference, not alternatives-of-equal-worth: `TITLE` is the ECB's
/// human sentence and `Measure` is the OECD's, so a document carrying both
/// should show the fuller one.
const TITLE_COLUMNS: &[&str] = &[
    "TITLE",
    "Series title",
    "TITLE_COMPL",
    "SERIES_NAME",
    "INDICATOR",
    "Measure",
    "MEASURE",
    "STRUCTURE_NAME",
];

const UNIT_COLUMNS: &[&str] = &[
    "UNIT",
    "Unit of measure",
    "UNIT_MEASURE",
    "UNIT_TYPE",
    "Unit multiplier",
];

const FREQ_COLUMNS: &[&str] = &["FREQ", "FREQUENCY", "Frequency of observation", "Frequency"];

const ADJUSTMENT_COLUMNS: &[&str] = &["ADJUSTMENT", "Adjustment", "SEASONAL_ADJUSTMENT", "ADJUST"];

/// The columns that carry a series key, where the document has one at all. Only
/// the ECB does; see [`SdmxSeries::series_count`].
const KEY_COLUMNS: &[&str] = &["KEY", "SERIES_KEY"];

/// An SDMX frequency code, spelled the way FRED spells the same thing.
///
/// A euro-area series and a US one sit in the same panel, and `D` against
/// `Daily` reads as two different fields rather than one. An unknown code is
/// passed through untouched: inventing a word for it would be a guess.
fn frequency_label(code: &str) -> String {
    match code.to_uppercase().as_str() {
        "A" => "Annual",
        // `B` is the OECD's spelling of a half-year, `S` is everyone else's.
        "S" | "B" => "Semiannual",
        "Q" => "Quarterly",
        "M" => "Monthly",
        "W" => "Weekly",
        "D" => "Daily",
        "H" => "Hourly",
        _ => return code.to_owned(),
    }
    .to_owned()
}

/* ------------------------------------------------------------- the reader */

/// A CSV header name, reduced to the form the lookups match on.
///
/// Case-insensitive, with surrounding whitespace and a UTF-8 BOM stripped: more
/// than one of these hosts prefixes a BOM, and the OECD ships header cells with
/// stray spaces around them.
fn normalise_header(name: &str) -> String {
    name.trim_start_matches('\u{feff}').trim().to_lowercase()
}

/// An SDMX CSV document, read once into a header map and its rows.
///
/// The `csv` crate does the reading rather than a hand-rolled split, because
/// every one of these publishers writes free text into a cell: the ECB's
/// `TITLE_COMPL` is `"ECB reference exchange rate, US dollar/Euro, 2.15 pm
/// (C.E.T.)"` and the IMF's `FULL_DESCRIPTION` is a paragraph. Splitting on
/// every comma shifts each later column by one, which does not fail loudly — it
/// silently pairs a date with the wrong series' value.
struct Table {
    /// Normalised header name → column index.
    index: HashMap<String, usize>,
    rows: Vec<csv::StringRecord>,
}

impl Table {
    fn read(text: &str) -> Table {
        let mut reader = csv::ReaderBuilder::new()
            // A row shorter or longer than the header is kept and read by index
            // rather than failing the whole document, which is what reading the
            // rows as raw arrays did.
            .flexible(true)
            .trim(csv::Trim::All)
            .from_reader(text.as_bytes());

        // The *first* occurrence of a name wins. The OECD's labelled CSV emits
        // each dimension twice — a code column then a label column, both
        // normalising to the same name — and the code is the one worth
        // indexing, because `FREQ` of `M` is what [`frequency_label`] reads.
        let mut index: HashMap<String, usize> = HashMap::new();
        if let Ok(header) = reader.headers() {
            for (at, name) in header.iter().enumerate() {
                let key = normalise_header(name);
                if !key.is_empty() {
                    index.entry(key).or_insert(at);
                }
            }
        }

        let rows = reader
            .records()
            .map_while(std::result::Result::ok)
            .filter(|row| row.iter().any(|cell| !cell.is_empty()))
            .collect();

        Table { index, rows }
    }

    fn has(&self, name: &str) -> bool {
        self.index.contains_key(&normalise_header(name))
    }

    /// Read a cell by header name, or `""` when this document has no such
    /// column — which is the normal case, since the three agencies overlap only
    /// partly.
    fn cell<'a>(&self, row: &'a csv::StringRecord, name: &str) -> &'a str {
        self.index
            .get(&normalise_header(name))
            .and_then(|at| row.get(*at))
            .unwrap_or("")
            .trim()
    }

    /// The first of `names` this document actually has a non-empty cell for.
    ///
    /// Three agencies spell the same concept three ways — `TITLE`,
    /// `Series title`, `Measure` — and a caller wants whichever is present
    /// rather than a chain of conditionals at every field.
    fn first<'a>(&self, row: &'a csv::StringRecord, names: &[&str]) -> &'a str {
        names
            .iter()
            .map(|name| self.cell(row, name))
            .find(|value| !value.is_empty())
            .unwrap_or("")
    }
}

/// Read a numeric cell.
///
/// `None` rather than `0.0` for anything unreadable. Every one of these
/// publishers spells "not reported" as an empty cell, and several use `NA`, `ND`
/// or a lone `.`; a zero would assert that the reading was taken and came out at
/// nought, which puts a false point on a chart someone acts on.
#[must_use]
pub fn number_cell(raw: &str) -> Option<f64> {
    let cleaned = raw
        .trim()
        .trim_start_matches('"')
        .trim_end_matches('"')
        .replace(',', "");
    if matches!(cleaned.as_str(), "" | "." | "NA" | "ND" | "NC") {
        return None;
    }
    cleaned
        .parse::<f64>()
        .ok()
        .filter(|value| value.is_finite())
}

/* --------------------------------------------------------------- a series */

/// One SDMX document, read.
#[derive(Debug, Clone, Default)]
pub struct SdmxSeries {
    pub observations: Vec<DataObservation>,
    pub title: String,
    pub units: String,
    pub frequency: String,
    pub seasonal_adjustment: String,
    /// Distinct series keys seen, when the document carries a key column at all.
    pub keys: Vec<String>,
    /// How many series the document holds.
    ///
    /// Counted from repeated time periods rather than from a key column, because
    /// only the ECB publishes one: the OECD spreads the key over a dozen
    /// dimension columns and the IMF's first column is the dataflow, identical
    /// on every row. Two rows for March 2026 is two series, whatever the columns
    /// are called.
    pub series_count: usize,
}

/// Read an SDMX CSV document into one series.
///
/// `agency` is the publisher's name as it should read in an error sentence
/// ("the ECB"), because the reader who sees this typed a key at one publisher
/// and is owed a message about that one rather than about SDMX.
///
/// The distinct keys are counted and carried out rather than collapsed, so that
/// [`assert_single_series`] can say plainly that the key was too loose instead
/// of drawing the interleaving as a line.
pub fn parse_sdmx_csv(csv: &str, agency: &str) -> Result<SdmxSeries> {
    let table = Table::read(csv);

    if table.index.is_empty() || table.rows.is_empty() {
        return Err(UpstreamError::new(
            format!("{agency} returned no observations"),
            codes::EMPTY_UPSTREAM,
        )
        .with_hint("The key is valid but selects nothing. Widen it, or check the period range."));
    }

    if !table.has("TIME_PERIOD") || !table.has("OBS_VALUE") {
        // Naming the columns that *were* there is the difference between "this
        // is broken" and "you asked the structure endpoint, not the data one".
        let mut columns: Vec<(usize, &str)> = table
            .index
            .iter()
            .map(|(name, at)| (*at, name.as_str()))
            .collect();
        columns.sort_unstable();
        let listed: Vec<&str> = columns.into_iter().take(12).map(|(_, name)| name).collect();

        return Err(UpstreamError::new(
            format!("{agency} returned a document without TIME_PERIOD/OBS_VALUE"),
            codes::BAD_UPSTREAM_BODY,
        )
        .with_hint(format!("Columns were: {}", listed.join(", "))));
    }

    let mut observations: Vec<DataObservation> = Vec::with_capacity(table.rows.len());
    let mut keys: Vec<String> = Vec::new();
    let mut per_period: HashMap<String, usize> = HashMap::new();
    let mut title = String::new();
    let mut units = String::new();
    let mut frequency = String::new();
    let mut adjustment = String::new();

    for row in &table.rows {
        // A period this terminal cannot date is dropped rather than guessed at.
        // A header repeated mid-document, or a footer row, lands here too.
        let Some(date) = period_to_date(table.cell(row, "TIME_PERIOD")) else {
            continue;
        };

        *per_period.entry(date.clone()).or_insert(0) += 1;
        observations.push(DataObservation {
            date,
            value: number_cell(table.cell(row, "OBS_VALUE")),
        });

        let key = table.first(row, KEY_COLUMNS);
        if !key.is_empty() && !keys.iter().any(|seen| seen == key) {
            keys.push(key.to_owned());
        }

        // Descriptive columns repeat on every row; the first non-empty one wins
        // so a gap in one row does not blank the panel's header.
        if title.is_empty() {
            title = table.first(row, TITLE_COLUMNS).to_owned();
        }
        if units.is_empty() {
            units = table.first(row, UNIT_COLUMNS).to_owned();
        }
        if frequency.is_empty() {
            frequency = table.first(row, FREQ_COLUMNS).to_owned();
        }
        if adjustment.is_empty() {
            adjustment = table.first(row, ADJUSTMENT_COLUMNS).to_owned();
        }
    }

    if observations.is_empty() {
        // The IMF's answer to a key with the wrong country code lands here: HTTP
        // 200, the right columns, and rows carrying only the dataflow's own
        // description. Reported as an error with a hint rather than as an empty
        // series, because an empty chart looks like "no data yet".
        return Err(UpstreamError::new(
            format!("{agency} returned no readable observations"),
            codes::EMPTY_UPSTREAM,
        )
        .with_hint("Every row had a time period this terminal could not date."));
    }

    observations.sort_by(|a, b| a.date.cmp(&b.date));

    let series_count = keys
        .len()
        .max(per_period.values().copied().max().unwrap_or(0));

    Ok(SdmxSeries {
        observations,
        title,
        units,
        frequency: frequency_label(&frequency),
        seasonal_adjustment: adjustment,
        keys,
        series_count,
    })
}

/// Refuse a key that selected more than one series.
///
/// Called with the id the reader typed, so the message can tell them to narrow
/// *their* key rather than describing SDMX at them.
///
/// This is the failure that most needs catching, because it does not look like
/// one: an under-specified key returns 200 with a perfectly well-formed body,
/// and collapsing it to one point per date would draw a line that alternates
/// between countries. Better to say the key is too loose.
pub fn assert_single_series(series: &SdmxSeries, id: &str, agency: &str) -> Result<()> {
    if series.series_count <= 1 {
        return Ok(());
    }

    let examples = series.keys.iter().take(3).cloned().collect::<Vec<_>>();
    let narrow = if examples.is_empty() {
        ".".to_owned()
    } else {
        format!(" — e.g. {}.", examples.join(", "))
    };

    Err(UpstreamError::bad_request(format!(
        "\"{id}\" matches {} series at {agency}, not one",
        series.series_count
    ))
    .with_hint(format!(
        "A partly-specified key selects every series under it, and they cannot be charted as one \
         line. Fill in the empty dimensions to narrow it{narrow}"
    )))
}

/// Duplicate dates collapse to the last value, which is the revised one.
///
/// A single key can still repeat a period — the ECB restates a day after a
/// correction — and two points on one date would render as a vertical spike.
#[must_use]
pub fn dedupe_observations(observations: Vec<DataObservation>) -> Vec<DataObservation> {
    let mut by_date: HashMap<String, Option<f64>> = HashMap::with_capacity(observations.len());
    for observation in observations {
        by_date.insert(observation.date, observation.value);
    }

    let mut out: Vec<DataObservation> = by_date
        .into_iter()
        .map(|(date, value)| DataObservation { date, value })
        .collect();
    out.sort_by(|a, b| a.date.cmp(&b.date));
    out
}

/* -------------------------------------------------------------- the URLs */

/// A dataflow reference is letters, digits and the punctuation SDMX gives
/// meaning to. Anything else is a typo, not an escape problem.
static FLOW_REFERENCE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[A-Za-z0-9_@.,:+-]{1,120}$").expect("FLOW_REFERENCE is a valid regex")
});

/// Validate a dataflow reference rather than percent-encoding it.
///
/// Whole-string escaping is wrong here: an OECD dataflow is `DSD_KEI@DF_KEI` and
/// an agency-qualified one is `OECD.SDD.STES,DSD_KEI@DF_KEI,4.0`. Escaping the
/// commas breaks the agency form outright, and escaping the `@` has been seen to
/// earn an HTTP 500 from the OECD's service. Both characters are legal in a URL
/// path segment, so the shape is checked and then passed through intact.
pub fn encode_sdmx_flow(flow: &str) -> Result<String> {
    if !FLOW_REFERENCE.is_match(flow) {
        return Err(UpstreamError::bad_request(format!(
            "\"{flow}\" is not a valid SDMX dataflow reference"
        ))
        .with_hint("A dataflow is letters, digits and `_@.,:+-`, e.g. EXR or DSD_KEI@DF_KEI."));
    }
    Ok(flow.to_owned())
}

/// Percent-encode an SDMX key without destroying its grammar.
///
/// A key's dots separate dimensions and its `+` is an "or" between codes within
/// one dimension. Escaping the `+` to `%2B` makes the services read it as a
/// literal plus, which matches nothing at all — and returns 200 with an empty
/// document rather than an error. Encoding each code separately keeps both
/// separators and still escapes anything odd inside a code.
#[must_use]
pub fn encode_sdmx_key(key: &str) -> String {
    key.split('.')
        .map(|dimension| {
            dimension
                .split('+')
                .map(|code| urlencoding::encode(code).into_owned())
                .collect::<Vec<_>>()
                .join("+")
        })
        .collect::<Vec<_>>()
        .join(".")
}

/// `FLOW/KEY`. A flow with no key is legal SDMX and selects the whole flow.
fn split_id(id: &str) -> (String, String) {
    let trimmed = id.trim().trim_matches('/');
    match trimmed.find('/') {
        None => (trimmed.to_owned(), String::new()),
        Some(slash) => (trimmed[..slash].to_owned(), trimmed[slash + 1..].to_owned()),
    }
}

/// `base?name=value&…`, with the values escaped and the path left alone.
///
/// Built by hand rather than through a URL type because the path is the part
/// that must survive untouched — see [`encode_sdmx_flow`] — and a re-parse is
/// one more chance for something to normalise the `@` away.
fn with_query(base: &str, params: &[(&str, &str)]) -> String {
    let query: Vec<String> = params
        .iter()
        .map(|(name, value)| format!("{name}={}", urlencoding::encode(value)))
        .collect();
    format!("{base}?{}", query.join("&"))
}

/* ----------------------------------------------------------- the agencies */

/// One well-known series, so a search can answer before a reader knows SDMX.
///
/// An empty `units`, `frequency` or `keywords` means the entry does not state
/// one, and is left off the search row rather than sent as `""`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SdmxCatalogueEntry {
    /// `FLOW/KEY`, exactly as `ECO` would take it.
    pub id: String,
    pub title: String,
    pub units: String,
    pub frequency: String,
    /// Extra words to match on that are not in the title — `jobless`, `oil`.
    pub keywords: String,
}

impl SdmxCatalogueEntry {
    /// The common case: a fully described entry.
    #[must_use]
    pub fn new(id: impl Into<String>, title: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            ..Self::default()
        }
    }

    #[must_use]
    pub fn with_units(mut self, units: impl Into<String>) -> Self {
        self.units = units.into();
        self
    }

    #[must_use]
    pub fn with_frequency(mut self, frequency: impl Into<String>) -> Self {
        self.frequency = frequency.into();
        self
    }

    #[must_use]
    pub fn with_keywords(mut self, keywords: impl Into<String>) -> Self {
        self.keywords = keywords.into();
        self
    }
}

/// Everything that differs between the ECB, the IMF and the OECD.
///
/// Held as function pointers rather than trait objects so an agency module is a
/// single `const` and the whole configuration is visible on one screen.
pub struct SdmxAgency {
    /// Which publisher, for the wire contract and the cache key.
    pub provider: DataSource,
    /// The publisher's name as it reads mid-sentence — "the ECB", "the OECD".
    pub agency: &'static str,
    /// How this service is asked for CSV. The ECB and the OECD take the query
    /// parameter and ignore the header; the IMF takes the header and ignores the
    /// parameter. Sending both costs nothing and means one code path.
    pub format: &'static str,
    pub accept: &'static str,
    /// Attempts after the first. The OECD answers a burst of requests with HTTP
    /// 500 rather than 429 — the same throttle wearing a different number — and
    /// that reads as an outage unless it is retried through.
    pub retries: u32,
    /// The service root for this deployment, up to but not including `/data`.
    /// Read from [`crate::config::Config`] so nothing here names a host.
    pub base: fn(&AppState) -> String,
    /// The page a reader clicks to check a series against the publisher itself,
    /// given `(service base, flow, key)`.
    pub web_url: fn(&str, &str, &str) -> String,
    /// The curated series this publisher's search offers. See
    /// [`search_catalogue`] for why it is a list rather than a query.
    pub catalogue: fn() -> &'static [SdmxCatalogueEntry],
}

/// Fetch, parse and assemble one series from one agency.
///
/// Everything here is common to all three: splitting `FLOW/KEY`, the period
/// window, the cache key, the too-loose-key check, and filling the gaps in a
/// sparse document from the catalogue entry.
pub async fn series(
    state: &AppState,
    agency: &'static SdmxAgency,
    id: &str,
    start: Option<&str>,
    end: Option<&str>,
) -> Result<DataSeriesResponse> {
    let (flow, key) = split_id(id);
    let catalogue = (agency.catalogue)();

    if flow.is_empty() {
        let example = catalogue
            .first()
            .map_or("FLOW/KEY", |entry| entry.id.as_str());
        return Err(UpstreamError::bad_request(format!(
            "\"{id}\" does not name a {} dataflow",
            agency.agency
        ))
        .with_hint(format!(
            "Ids look like {example} — the dataflow, a slash, then the series key."
        )));
    }

    let cache_key = format!(
        "{}:series:{flow}/{key}:{}:{}",
        agency.provider.as_str(),
        start.unwrap_or_default(),
        end.unwrap_or_default()
    );

    let response = state
        .cache()
        .cached(&cache_key, ttl::FRED, || async {
            fetch_series(state, agency, id, &flow, &key, start, end).await
        })
        .await?;

    Ok((*response).clone())
}

async fn fetch_series(
    state: &AppState,
    agency: &'static SdmxAgency,
    id: &str,
    flow: &str,
    key: &str,
    start: Option<&str>,
    end: Option<&str>,
) -> Result<DataSeriesResponse> {
    let base = (agency.base)(state);
    let path = format!(
        "{}/data/{}/{}",
        base.trim_end_matches('/'),
        encode_sdmx_flow(flow)?,
        if key.is_empty() {
            "all".to_owned()
        } else {
            encode_sdmx_key(key)
        }
    );

    let mut params: Vec<(&str, &str)> = vec![("format", agency.format)];
    // SDMX periods are as coarse as the series is: sending a full date as
    // `startPeriod` against an annual flow is accepted by all three, so the
    // window is passed straight through rather than rounded to the frequency.
    if let Some(start) = start.filter(|start| !start.is_empty()) {
        params.push(("startPeriod", start));
    }
    if let Some(end) = end.filter(|end| !end.is_empty()) {
        params.push(("endPeriod", end));
    }
    let url = with_query(&path, &params);

    let csv = state
        .http()
        .fetch_text(
            &url,
            FetchOptions::new()
                .timeout(SDMX_TIMEOUT)
                .retries(agency.retries)
                // These are statistical APIs, not pages behind bot protection;
                // a browser header set buys nothing and the ECB's service is
                // happier with a plain one.
                .browser_headers(false)
                .header("Accept", agency.accept),
        )
        .await
        .map_err(|err| as_throttle(err, agency.agency))?;

    let parsed = parse_sdmx_csv(&csv, agency.agency)?;
    assert_single_series(&parsed, id, agency.agency)?;

    let full_id = format!("{flow}/{key}");
    let known = (agency.catalogue)()
        .iter()
        .find(|entry| entry.id == full_id);

    let observations = dedupe_observations(parsed.observations);

    // The catalogue fills what the document leaves blank, and never overrides
    // it: the publisher's own statement of a unit outranks our note of it.
    let units = first_non_empty(&[
        &parsed.units,
        known.map_or("", |entry| entry.units.as_str()),
    ]);
    // The last resort, when nobody states a title: the request itself, which at
    // least tells the reader which key drew the line in front of them.
    let described = format!("{} {flow} {key}", agency.agency).trim().to_owned();
    let title = first_non_empty(&[
        // Here the order is the other way round, deliberately. The IMF states no
        // title at all and the OECD states a measure *code*, so a curated
        // sentence is the better header wherever we have one.
        known.map_or("", |entry| entry.title.as_str()),
        &parsed.title,
        &described,
    ]);
    let frequency = first_non_empty(&[
        &parsed.frequency,
        known.map_or("", |entry| entry.frequency.as_str()),
    ]);

    Ok(DataSeriesResponse {
        series: DataSeries {
            provider: agency.provider,
            id: full_id,
            title,
            units: units.clone(),
            // SDMX states one unit string and no abbreviation of it.
            units_short: units,
            frequency,
            seasonal_adjustment: parsed.seasonal_adjustment,
            // None of the three states a revision timestamp per series in the
            // data message, and an invented one would be worse than none.
            last_updated: String::new(),
            observation_start: observations
                .first()
                .map(|o| o.date.clone())
                .unwrap_or_default(),
            observation_end: observations
                .last()
                .map(|o| o.date.clone())
                .unwrap_or_default(),
            notes: String::new(),
            // One surface per agency, so the arm that answered is always this
            // reader.
            source: "sdmx".to_owned(),
            source_url: (agency.web_url)(&base, flow, key),
        },
        observations,
    })
}

fn first_non_empty(candidates: &[&str]) -> String {
    candidates
        .iter()
        .find(|value| !value.is_empty())
        .unwrap_or(&"")
        .to_string()
}

/// Name a throttle as a throttle.
///
/// These services shed load with whatever status is nearest to hand — the OECD
/// alternates 429 and 500 for the same condition — and both read as "the dataset
/// is broken" unless someone says otherwise. It is not: the key is fine, and the
/// answer is cached for half an hour once one request gets through.
///
/// A refusal the HTTP layer has already identified as the origin hanging up on
/// this network is left alone. That one really is not a throttle, and the hint
/// it already carries names the actual remedy.
fn as_throttle(err: UpstreamError, agency: &str) -> UpstreamError {
    let status = err.status.unwrap_or(0);
    if err.code == codes::UPSTREAM_BLOCKED || (status != 429 && status < 500) {
        return err;
    }

    UpstreamError::new(
        format!("{agency} is throttling this IP (HTTP {status})"),
        codes::RATE_LIMITED,
    )
    .with_status(status)
    .with_hint(format!(
        "{agency}'s SDMX service limits requests per address and sheds the excess with 429 or \
         500 — this is not a bad key. Retry in a minute; the answer is then cached for thirty."
    ))
}

/* ----------------------------------------------------------------- search */

/// Rank a curated catalogue against a query.
///
/// These agencies publish no free-text search endpoint — the OECD's data
/// explorer is a JavaScript application over a structure API, and matching a
/// reader's words against 20,000 dataflow names would need the whole structure
/// document on every keystroke. A short, checked list of the series people
/// actually ask for answers the question that matters ("what do I type?") and is
/// honest about being a list rather than a search.
#[must_use]
pub fn search_catalogue(
    provider: DataSource,
    catalogue: &[SdmxCatalogueEntry],
    query: &str,
    limit: usize,
) -> Vec<DataSearchResult> {
    let terms: Vec<String> = query
        .split_whitespace()
        .map(str::to_lowercase)
        .filter(|term| !term.is_empty())
        .collect();

    let mut scored: Vec<(usize, &SdmxCatalogueEntry)> = catalogue
        .iter()
        .filter_map(|entry| {
            let haystack =
                format!("{} {} {}", entry.id, entry.title, entry.keywords).to_lowercase();
            let title = entry.title.to_lowercase();

            // Every term must appear, so `ecb oil` narrows rather than returning
            // every ECB series that mentions the euro.
            if !terms.iter().all(|term| haystack.contains(term.as_str())) {
                return None;
            }

            // A hit in the title counts double: a word in the keywords is a
            // synonym someone wrote down, a word in the title is the thing.
            let score: usize = terms
                .iter()
                .map(|term| usize::from(title.contains(term.as_str())) + 1)
                .sum();
            (score > 0).then_some((score, entry))
        })
        .collect();

    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.title.cmp(&b.1.title)));

    scored
        .into_iter()
        .take(limit)
        .map(|(_, entry)| DataSearchResult {
            // Named rather than defaulted: `provider` decides which publisher a
            // row is opened against, and `DataSearchResult` deliberately has no
            // `Default` for that reason.
            provider,
            // One surface per agency, so there is no arm to name.
            source: None,
            id: entry.id.clone(),
            title: entry.title.clone(),
            units: optional(&entry.units),
            frequency: optional(&entry.frequency),
            seasonal_adjustment: None,
            observation_range: None,
        })
        .collect()
}

/// A catalogue field the entry does not state is absent, not `""`.
fn optional(value: &str) -> Option<String> {
    (!value.is_empty()).then(|| value.to_owned())
}

/// A catalogue search, as the three agency modules expose it.
///
/// An empty query is refused rather than returning the whole catalogue: `ECOS`
/// fans across publishers, and eight full catalogues is not an answer.
pub fn search(
    agency: &'static SdmxAgency,
    query: &str,
    limit: usize,
) -> Result<Vec<DataSearchResult>> {
    let query = query.trim();
    if query.is_empty() {
        return Err(UpstreamError::bad_request("Search needs at least one word"));
    }
    Ok(search_catalogue(
        agency.provider,
        (agency.catalogue)(),
        query,
        limit.clamp(1, 100),
    ))
}

/* ------------------------------------------------------------------ tests */

#[cfg(test)]
mod tests {
    //! SDMX reader tests, over fixtures captured from the three live services.
    //!
    //! Every one of these hosts answers a malformed request with HTTP 200 and a
    //! plausible body, so the fixtures are here to pin the shapes that are wrong
    //! without looking wrong: an ECB key missing a dimension that returns
    //! forty-odd currencies interleaved, and an IMF key with a two-letter
    //! country code that returns the dataflow's own description and no
    //! observations. Neither fails until someone reads a chart drawn from it.

    use super::*;
    use crate::config::Config;
    use crate::sources::{ecb, imf, oecd};
    use wiremock::matchers::{header, method, path as path_matcher, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// `data-api.ecb.europa.eu/service/data/EXR/D.USD.EUR.SP00.A`, the first
    /// four rows of a two-week window. The whole 32-column header is kept
    /// unedited: it is what proves `TITLE` and `UNIT` are found among thirty
    /// other columns, and `TITLE_COMPL` carries the commas inside quotes that a
    /// split-on-comma reader would shift every later column past.
    const ECB_EXR_CSV: &str = include_str!("fixtures/sdmx_ecb_exr.csv");

    /// The same request with the currency dimension left empty —
    /// `EXR/D..EUR.SP00.A`. The live answer holds 29 currencies over three days;
    /// three currencies are kept, which is enough to show three rows landing on
    /// each date. HTTP 200, well-formed, and three interleaved series.
    const ECB_ALL_CURRENCIES_CSV: &str = include_str!("fixtures/sdmx_ecb_all_currencies.csv");

    /// `api.imf.org/.../data/CPI/USA.CPI._T.IX.M`, three months. The 45-column
    /// header is unedited; the free-text columns (`FULL_DESCRIPTION` and the
    /// keyword lists, each a paragraph) are truncated to keep the fixture small,
    /// which leaves the quoting and the column count exactly as served.
    const IMF_CPI_CSV: &str = include_str!("fixtures/sdmx_imf_cpi.csv");

    /// The same flow keyed `US` instead of `USA`. The IMF answers HTTP 200 with
    /// the dataflow's description and no observations at all — the trap that
    /// makes an observation-free document an error rather than an empty series.
    /// Free text truncated as above.
    const IMF_DATAFLOW_ONLY_CSV: &str = include_str!("fixtures/sdmx_imf_dataflow_only.csv");

    /// `sdmx.oecd.org/public/rest/data/DSD_KEI@DF_KEI/USA.M.CP.GR._Z._Z.GY`,
    /// three months. Kept whole because the doubled columns are the point: every
    /// dimension appears as a code column and again as a label column, both
    /// normalising to the same header name.
    const OECD_KEI_CSV: &str = include_str!("fixtures/sdmx_oecd_kei.csv");

    /// A minimal ECB-shaped document, for the cases a captured fixture cannot
    /// show: the ECB publishes no gaps in `EXR`, but a blank `OBS_VALUE` is
    /// ordinary in its monthly flows and must not become a zero.
    const ECB_LITERAL: &str = "KEY,FREQ,CURRENCY,TIME_PERIOD,OBS_VALUE,TITLE,UNIT\n\
        EXR.D.USD.EUR.SP00.A,D,USD,2026-08-12,1.1545,US dollar/Euro,USD\n\
        EXR.D.USD.EUR.SP00.A,D,USD,2026-08-13,1.1602,US dollar/Euro,USD\n\
        EXR.D.USD.EUR.SP00.A,D,USD,2026-08-14,,US dollar/Euro,USD\n";

    /// `(date, value)` pairs, since the wire type carries no `PartialEq`.
    fn rows(observations: &[DataObservation]) -> Vec<(&str, Option<f64>)> {
        observations
            .iter()
            .map(|observation| (observation.date.as_str(), observation.value))
            .collect()
    }

    /* ------------------------------------------------------------ the reader */

    #[test]
    fn reads_observations_and_the_descriptive_columns() {
        let series = parse_sdmx_csv(ECB_LITERAL, "the ECB").expect("the document parses");

        assert_eq!(
            rows(&series.observations),
            [
                ("2026-08-12", Some(1.1545)),
                ("2026-08-13", Some(1.1602)),
                // An empty OBS_VALUE is a gap in the series, not a zero and not
                // a row to drop — dropping it would silently compress the time
                // axis, and a zero would put a false reading on the chart.
                ("2026-08-14", None),
            ]
        );
        assert_eq!(series.title, "US dollar/Euro");
        assert_eq!(series.units, "USD");
        // `D` is spelled the way FRED spells the same thing, so a euro-area
        // series and a US one read alike in the panel's FREQ field.
        assert_eq!(series.frequency, "Daily");
        assert_eq!(series.series_count, 1);
    }

    #[test]
    fn finds_the_two_columns_that_matter_among_thirty_others() {
        let series = parse_sdmx_csv(ECB_EXR_CSV, "the ECB").expect("the fixture parses");

        assert_eq!(
            rows(&series.observations),
            [
                ("2026-08-03", Some(1.1535)),
                ("2026-08-04", Some(1.1515)),
                ("2026-08-05", Some(1.1554)),
                ("2026-08-06", Some(1.1542)),
            ]
        );
        assert_eq!(series.title, "US dollar/Euro ECB reference exchange rate");
        assert_eq!(series.units, "USD");
        assert_eq!(series.frequency, "Daily");
        assert_eq!(series.keys, ["EXR.D.USD.EUR.SP00.A"]);
        assert_eq!(series.series_count, 1);
    }

    #[test]
    fn a_quoted_comma_does_not_shift_the_columns_past_it() {
        // TITLE_COMPL is `"ECB reference exchange rate, US dollar/Euro, 2.15 pm
        // (C.E.T.)"` — two commas inside one field. A split-on-every-comma
        // reader would pair each date with the wrong series' value, silently.
        let series = parse_sdmx_csv(ECB_EXR_CSV, "the ECB").expect("the fixture parses");
        assert_eq!(
            series.observations.first().and_then(|o| o.value),
            Some(1.1535)
        );
        assert_eq!(series.units, "USD");
    }

    #[test]
    fn a_quoted_newline_does_not_end_the_row_it_is_inside() {
        // Not hypothetical: the IMF's `FULL_DESCRIPTION` is a multi-paragraph
        // prose block repeated on every row, so a six-observation CPI response
        // is eighteen *lines* and six *records*. A line-oriented reader sees
        // twelve malformed rows and one plausible series.
        let csv = "TIME_PERIOD,OBS_VALUE,FULL_DESCRIPTION\n\
            2026-01,1.5,\"Consumer prices.\n\nCompiled by the national office.\"\n\
            2026-02,2.5,\"Consumer prices.\n\nCompiled by the national office.\"\n";
        let series = parse_sdmx_csv(csv, "the IMF").expect("the document parses");
        assert_eq!(
            rows(&series.observations),
            [("2026-01-01", Some(1.5)), ("2026-02-01", Some(2.5))]
        );
        // Two records, so two rows landed on two distinct dates — not four rows
        // on two dates, which would read as an under-specified key.
        assert_eq!(series.series_count, 1);
    }

    #[test]
    fn counts_interleaved_series_from_repeated_periods() {
        // The OECD spreads its key over a dozen columns and the IMF's first
        // column is the dataflow, identical on every row — so multiplicity has
        // to be read from the periods rather than from a key column.
        let two = "TIME_PERIOD,OBS_VALUE,REF_AREA\n\
            2026-01,1,USA\n2026-01,2,GBR\n2026-02,3,USA\n2026-02,4,GBR\n";
        let series = parse_sdmx_csv(two, "the OECD").expect("the document parses");

        assert_eq!(series.series_count, 2);
        // No key column at all, so the message cannot offer examples.
        assert!(series.keys.is_empty());

        let refused = assert_single_series(&series, "DF/…", "the OECD")
            .expect_err("two series is not one series");
        assert_eq!(refused.code, codes::BAD_REQUEST);
        assert!(
            refused
                .message
                .contains("matches 2 series at the OECD, not one"),
            "message was {:?}",
            refused.message
        );
    }

    #[test]
    fn an_under_specified_ecb_key_is_refused_rather_than_charted() {
        let series = parse_sdmx_csv(ECB_ALL_CURRENCIES_CSV, "the ECB").expect("the fixture parses");

        // Three currencies over three days: nine rows, three per date.
        assert_eq!(series.observations.len(), 9);
        assert_eq!(series.series_count, 3);
        assert_eq!(
            series.keys,
            [
                "EXR.D.AUD.EUR.SP00.A",
                "EXR.D.BRL.EUR.SP00.A",
                "EXR.D.CAD.EUR.SP00.A"
            ]
        );

        let refused = assert_single_series(&series, "EXR/D..EUR.SP00.A", "the ECB")
            .expect_err("a key missing a dimension is not one series");
        assert!(refused
            .message
            .contains("matches 3 series at the ECB, not one"));
        // The ECB is the only one of the three that publishes a key column, so
        // it is the only one that can name what the loose key caught.
        let hint = refused.hint.unwrap_or_default();
        assert!(hint.contains("EXR.D.AUD.EUR.SP00.A"), "hint was {hint:?}");
    }

    #[test]
    fn accepts_a_single_series_without_complaint() {
        let series = parse_sdmx_csv(ECB_EXR_CSV, "the ECB").expect("the fixture parses");
        assert!(assert_single_series(&series, "EXR/D.USD.EUR.SP00.A", "the ECB").is_ok());
    }

    #[test]
    fn rejects_a_body_with_no_observation_columns() {
        let refused =
            parse_sdmx_csv("A,B\n1,2\n", "the IMF").expect_err("this is not a data message");
        assert_eq!(refused.code, codes::BAD_UPSTREAM_BODY);
        assert!(refused.message.contains("without TIME_PERIOD/OBS_VALUE"));
        // The columns that *were* present are the difference between "broken"
        // and "you asked the structure endpoint".
        assert_eq!(refused.hint.as_deref(), Some("Columns were: a, b"));
    }

    #[test]
    fn rejects_a_body_that_carries_only_a_dataflow_description() {
        // Exactly what the IMF returns for a key using the wrong country code:
        // 200, well-formed, and containing no observations at all.
        let refused = parse_sdmx_csv(IMF_DATAFLOW_ONLY_CSV, "the IMF")
            .expect_err("a description is not a series");
        assert_eq!(refused.code, codes::EMPTY_UPSTREAM);
        assert!(refused.message.contains("no readable observations"));
    }

    #[test]
    fn rejects_an_empty_document() {
        let refused = parse_sdmx_csv("", "the ECB").expect_err("nothing is not a series");
        assert_eq!(refused.code, codes::EMPTY_UPSTREAM);
        assert!(refused.message.contains("no observations"));
    }

    #[test]
    fn rejects_a_header_with_no_rows_under_it() {
        let refused = parse_sdmx_csv("TIME_PERIOD,OBS_VALUE\n", "the ECB")
            .expect_err("a header is not a series");
        assert_eq!(refused.code, codes::EMPTY_UPSTREAM);
        assert_eq!(
            refused.hint.as_deref(),
            Some("The key is valid but selects nothing. Widen it, or check the period range.")
        );
    }

    #[test]
    fn reads_the_imf_monthly_period_vocabulary() {
        // The IMF dates a month `2026-M01`, where the ECB writes `2026-01`.
        let series = parse_sdmx_csv(IMF_CPI_CSV, "the IMF").expect("the fixture parses");
        assert_eq!(
            rows(&series.observations),
            [
                ("2026-01-01", Some(149.160_190_868_838_4)),
                ("2026-02-01", Some(149.863_222_895_088_6)),
                ("2026-03-01", Some(151.435_299_728_738_8)),
            ]
        );
        // `FREQUENCY` rather than `FREQ`, and mapped to the same word.
        assert_eq!(series.frequency, "Monthly");
        // The IMF states no title column at all — every one of them is empty —
        // which is why the catalogue's sentence is what a reader ends up seeing.
        assert!(series.title.is_empty(), "title was {:?}", series.title);
        assert_eq!(series.series_count, 1);
    }

    #[test]
    fn reads_the_oecd_doubled_code_and_label_columns() {
        let series = parse_sdmx_csv(OECD_KEI_CSV, "the OECD").expect("the fixture parses");

        assert_eq!(
            rows(&series.observations),
            [
                ("2026-04-01", Some(3.810_844_932_1)),
                ("2026-05-01", Some(4.248_674_039_164_47)),
                ("2026-06-01", Some(3.531_425_063_786_41)),
            ]
        );
        // `FREQ` and `Frequency of observation` both normalise to a header this
        // reader knows, and the first — the code column — is the one indexed.
        // That is deliberate: `M` is what maps onto a word, `Monthly` is already
        // one and would pass through unrecognised.
        assert_eq!(series.frequency, "Monthly");
        // The same rule applied to `MEASURE`/`Measure` yields the measure *code*
        // rather than its label, which is why the OECD's own titles are carried
        // in the catalogue instead of read off the document.
        assert_eq!(series.title, "CP");
        // `Unit of measure` is checked before `UNIT_MEASURE`, so the unit is the
        // readable one.
        assert_eq!(series.units, "Growth rate");
        // No key column anywhere in an OECD document; multiplicity comes from
        // the periods alone.
        assert!(series.keys.is_empty());
        assert_eq!(series.series_count, 1);
    }

    #[test]
    fn drops_a_period_it_cannot_date_rather_than_guessing() {
        // A footer row, or a repeated header, must not become an observation.
        let csv = "TIME_PERIOD,OBS_VALUE\n2026-Q1,1.5\nlater,9.9\n2026-Q2,2.5\n";
        let series = parse_sdmx_csv(csv, "the OECD").expect("the document parses");
        assert_eq!(
            rows(&series.observations),
            [("2026-01-01", Some(1.5)), ("2026-04-01", Some(2.5))]
        );
    }

    #[test]
    fn sorts_observations_the_publisher_returned_out_of_order() {
        let csv = "TIME_PERIOD,OBS_VALUE\n2026-03,3\n2026-01,1\n2026-02,2\n";
        let series = parse_sdmx_csv(csv, "the IMF").expect("the document parses");
        assert_eq!(
            rows(&series.observations),
            [
                ("2026-01-01", Some(1.0)),
                ("2026-02-01", Some(2.0)),
                ("2026-03-01", Some(3.0))
            ]
        );
    }

    #[test]
    fn keeps_the_first_non_empty_descriptive_cell() {
        // A gap in one row must not blank the panel's header.
        let csv =
            "TIME_PERIOD,OBS_VALUE,TITLE,UNIT\n2026-01,1,,\n2026-02,2,Headline HICP,Percent\n";
        let series = parse_sdmx_csv(csv, "the ECB").expect("the document parses");
        assert_eq!(series.title, "Headline HICP");
        assert_eq!(series.units, "Percent");
    }

    /* ------------------------------------------------------- number_cell */

    #[test]
    fn reads_a_missing_reading_as_null_rather_than_zero() {
        // A zero would assert the reading was taken and came out at nought.
        for empty in ["", ".", "NA", "ND", "NC", "   "] {
            assert_eq!(number_cell(empty), None, "{empty:?} should be null");
        }
    }

    #[test]
    fn strips_thousands_separators_and_surrounding_quotes() {
        assert_eq!(number_cell("\"28,624.069\""), Some(28_624.069));
        assert_eq!(number_cell("-2.5"), Some(-2.5));
        assert_eq!(number_cell("1.2e3"), Some(1200.0));
    }

    #[test]
    fn reads_an_unparseable_cell_as_missing_rather_than_zero() {
        assert_eq!(number_cell("provisional"), None);
        assert_eq!(number_cell("NaN"), None);
        assert_eq!(number_cell("inf"), None);
    }

    /* ------------------------------------------------ dedupe_observations */

    #[test]
    fn a_repeated_date_collapses_to_the_revised_value() {
        let deduped = dedupe_observations(vec![
            DataObservation {
                date: "2026-02-01".to_owned(),
                value: Some(2.0),
            },
            DataObservation {
                date: "2026-01-01".to_owned(),
                value: Some(1.0),
            },
            DataObservation {
                date: "2026-01-01".to_owned(),
                value: Some(1.5),
            },
        ]);
        assert_eq!(
            rows(&deduped),
            [("2026-01-01", Some(1.5)), ("2026-02-01", Some(2.0))]
        );
    }

    #[test]
    fn a_revision_to_a_missing_value_is_kept_as_missing() {
        // The revision wins even when the revision is "we are withdrawing this".
        let deduped = dedupe_observations(vec![
            DataObservation {
                date: "2026-01-01".to_owned(),
                value: Some(1.0),
            },
            DataObservation {
                date: "2026-01-01".to_owned(),
                value: None,
            },
        ]);
        assert_eq!(rows(&deduped), [("2026-01-01", None)]);
    }

    /* --------------------------------------------------------- the URLs */

    #[test]
    fn leaves_the_key_grammar_alone_and_escapes_only_what_is_inside_a_code() {
        assert_eq!(encode_sdmx_key("D.USD.EUR.SP00.A"), "D.USD.EUR.SP00.A");
        // `+` is "or" between codes; escaping it to %2B matches nothing, and
        // matching nothing is answered with 200 and an empty document.
        assert_eq!(encode_sdmx_key("D.USD+GBP.EUR"), "D.USD+GBP.EUR");
        assert_eq!(encode_sdmx_key("A.a b"), "A.a%20b");
        // An empty dimension is "any", and must survive as an empty segment —
        // it is the very thing `assert_single_series` exists to catch later.
        assert_eq!(encode_sdmx_key("D..EUR.SP00.A"), "D..EUR.SP00.A");
    }

    #[test]
    fn passes_a_dataflow_reference_through_intact() {
        assert_eq!(encode_sdmx_flow("EXR").unwrap(), "EXR");
        // Escaping the `@` has been seen to earn an HTTP 500 from the OECD.
        assert_eq!(
            encode_sdmx_flow("DSD_KEI@DF_KEI").unwrap(),
            "DSD_KEI@DF_KEI"
        );
        // The agency-qualified form: escaping the commas breaks it outright.
        assert_eq!(
            encode_sdmx_flow("OECD.SDD.STES,DSD_KEI@DF_KEI,4.0").unwrap(),
            "OECD.SDD.STES,DSD_KEI@DF_KEI,4.0"
        );
    }

    #[test]
    fn refuses_a_dataflow_reference_that_is_not_one() {
        for bad in ["EXR/D", "a b", "EXR?format=csv", ""] {
            let refused = encode_sdmx_flow(bad).expect_err("not a dataflow");
            assert_eq!(refused.code, codes::BAD_REQUEST);
        }
    }

    #[test]
    fn splits_a_flow_from_its_key_and_tolerates_stray_slashes() {
        assert_eq!(
            split_id("EXR/D.USD.EUR.SP00.A"),
            ("EXR".to_owned(), "D.USD.EUR.SP00.A".to_owned())
        );
        assert_eq!(
            split_id("  /EXR/D.USD/  "),
            ("EXR".to_owned(), "D.USD".to_owned())
        );
        // A flow with no key is legal SDMX and selects the whole flow.
        assert_eq!(split_id("EXR"), ("EXR".to_owned(), String::new()));
        assert_eq!(split_id("///"), (String::new(), String::new()));
    }

    /* ------------------------------------------------------- the catalogue */

    fn sample_catalogue() -> Vec<SdmxCatalogueEntry> {
        vec![
            SdmxCatalogueEntry::new("A/1", "Euro area unemployment rate")
                .with_keywords("jobless labour"),
            SdmxCatalogueEntry::new("B/2", "US dollar / euro reference exchange rate")
                .with_keywords("fx forex")
                .with_units("USD per EUR")
                .with_frequency("Daily"),
        ]
    }

    fn ids(results: &[DataSearchResult]) -> Vec<&str> {
        results.iter().map(|row| row.id.as_str()).collect()
    }

    #[test]
    fn requires_every_term_to_match_so_a_second_word_narrows() {
        let catalogue = sample_catalogue();
        assert_eq!(
            ids(&search_catalogue(
                DataSource::Ecb,
                &catalogue,
                "unemployment",
                10
            )),
            ["A/1"]
        );

        let euro_rate = search_catalogue(DataSource::Ecb, &catalogue, "euro rate", 10);
        let mut both = ids(&euro_rate);
        both.sort_unstable();
        assert_eq!(both, ["A/1", "B/2"]);

        assert!(search_catalogue(DataSource::Ecb, &catalogue, "unemployment forex", 10).is_empty());
    }

    #[test]
    fn matches_keywords_that_are_not_in_the_title() {
        let catalogue = sample_catalogue();
        assert_eq!(
            ids(&search_catalogue(
                DataSource::Ecb,
                &catalogue,
                "jobless",
                10
            )),
            ["A/1"]
        );
    }

    #[test]
    fn stamps_every_result_with_its_publisher() {
        let catalogue = sample_catalogue();
        let results = search_catalogue(DataSource::Oecd, &catalogue, "euro", 10);
        assert!(!results.is_empty());
        assert!(results
            .iter()
            .all(|row| row.provider == DataSource::Oecd && row.source.is_none()));
    }

    #[test]
    fn leaves_a_field_the_catalogue_does_not_state_absent() {
        let catalogue = sample_catalogue();
        let results = search_catalogue(DataSource::Ecb, &catalogue, "forex", 10);
        assert_eq!(results[0].units.as_deref(), Some("USD per EUR"));
        assert_eq!(results[0].frequency.as_deref(), Some("Daily"));

        let unemployment = search_catalogue(DataSource::Ecb, &catalogue, "jobless", 10);
        assert_eq!(unemployment[0].units, None);
        assert_eq!(unemployment[0].frequency, None);
    }

    #[test]
    fn honours_the_limit() {
        let catalogue = sample_catalogue();
        assert_eq!(
            search_catalogue(DataSource::Ecb, &catalogue, "euro", 1).len(),
            1
        );
    }

    #[test]
    fn every_catalogued_id_is_a_flow_and_a_key() {
        // A catalogue row a reader clicks has to be an id `ECO` can take, and
        // the shape is checked here rather than discovered as a 404.
        for entry in ecb::catalogue()
            .iter()
            .chain(imf::catalogue())
            .chain(oecd::catalogue())
        {
            let (flow, key) = split_id(&entry.id);
            assert!(
                encode_sdmx_flow(&flow).is_ok(),
                "{} has an unusable dataflow",
                entry.id
            );
            assert!(!key.is_empty(), "{} names no series key", entry.id);
            assert!(!entry.title.is_empty(), "{} has no title", entry.id);
        }
    }

    #[test]
    fn each_catalogue_is_free_of_duplicate_ids() {
        for catalogue in [ecb::catalogue(), imf::catalogue(), oecd::catalogue()] {
            let mut seen: Vec<&str> = catalogue.iter().map(|entry| entry.id.as_str()).collect();
            let before = seen.len();
            seen.sort_unstable();
            seen.dedup();
            assert_eq!(seen.len(), before, "a catalogue repeats an id");
        }
    }

    #[test]
    fn an_empty_query_is_refused_rather_than_returning_the_whole_catalogue() {
        let refused = search(&ecb::ECB, "   ", 25).expect_err("no words is not a search");
        assert_eq!(refused.code, codes::BAD_REQUEST);
    }

    /* ------------------------------------------------------------- the wire */

    /// All three agencies stand behind one mock. Their service roots differ, so
    /// each is pointed at its own prefix and nothing collides.
    fn state_for(server: &MockServer) -> AppState {
        AppState::new(Config {
            ecb_api_base: format!("{}/service", server.uri()),
            imf_api_base: format!("{}/external/sdmx/2.1", server.uri()),
            oecd_api_base: format!("{}/public/rest", server.uri()),
            ..Config::default()
        })
    }

    #[tokio::test]
    async fn assembles_an_ecb_series_from_the_service() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/service/data/EXR/D.USD.EUR.SP00.A"))
            // The ECB reads `?format=` and ignores the Accept header; the header
            // is sent anyway so all three agencies share one code path.
            .and(query_param("format", "csvdata"))
            .and(query_param("startPeriod", "2026-08-01"))
            .and(query_param("endPeriod", "2026-08-14"))
            .respond_with(ResponseTemplate::new(200).set_body_string(ECB_EXR_CSV))
            .expect(1)
            .mount(&server)
            .await;

        let answer = ecb::get_series(
            &state_for(&server),
            "EXR/D.USD.EUR.SP00.A",
            Some("2026-08-01"),
            Some("2026-08-14"),
        )
        .await
        .expect("the series assembles");

        assert_eq!(answer.series.provider, DataSource::Ecb);
        assert_eq!(answer.series.id, "EXR/D.USD.EUR.SP00.A");
        // The catalogue's sentence outranks the document's, because the ECB's
        // own `TITLE` repeats the mechanism rather than naming the series.
        assert_eq!(
            answer.series.title,
            "US dollar / euro reference exchange rate"
        );
        assert_eq!(answer.series.units, "USD");
        assert_eq!(answer.series.units_short, "USD");
        assert_eq!(answer.series.frequency, "Daily");
        assert_eq!(answer.series.source, "sdmx");
        assert_eq!(answer.series.observation_start, "2026-08-03");
        assert_eq!(answer.series.observation_end, "2026-08-06");
        assert_eq!(answer.observations.len(), 4);
        // The link a reader clicks is the publisher's own page, not ours.
        assert!(
            answer.series.source_url.contains("data.ecb.europa.eu"),
            "source_url was {:?}",
            answer.series.source_url
        );
    }

    #[tokio::test]
    async fn omits_the_period_window_the_caller_did_not_give() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/service/data/EXR/D.USD.EUR.SP00.A"))
            .respond_with(ResponseTemplate::new(200).set_body_string(ECB_EXR_CSV))
            .expect(1)
            .mount(&server)
            .await;

        let answer = ecb::get_series(&state_for(&server), "EXR/D.USD.EUR.SP00.A", None, Some(""))
            .await
            .expect("the series assembles");
        assert_eq!(answer.observations.len(), 4);

        // A `startPeriod=` with nothing after it is not the same request as no
        // `startPeriod` at all — an empty bound is a bound, and an SDMX service
        // is entitled to read it as one. Neither an absent nor an empty-string
        // argument may put the parameter on the wire.
        let sent = server.received_requests().await.expect("recording is on");
        let query = sent[0].url.query().unwrap_or_default().to_owned();
        assert_eq!(query, "format=csvdata", "query was {query:?}");
    }

    #[tokio::test]
    async fn puts_an_or_key_on_the_wire_with_its_plus_intact() {
        // `D.USD+GBP.EUR.SP00.A` asks for two currencies. Escaped to `%2B` it
        // asks for one whose code is literally `USD+GBP`, which matches nothing
        // — and matching nothing is answered with 200 and an empty document, not
        // an error, so nobody would find out this way.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string(ECB_ALL_CURRENCIES_CSV))
            .mount(&server)
            .await;

        // Two currencies really is two series, so this refusal is the correct
        // answer; the request that produced it is what is under test.
        let _ = ecb::get_series(&state_for(&server), "EXR/D.USD+GBP.EUR.SP00.A", None, None).await;

        let sent = server.received_requests().await.expect("recording is on");
        assert_eq!(
            sent[0].url.path(),
            "/service/data/EXR/D.USD+GBP.EUR.SP00.A",
            "the key was mangled on its way out"
        );
    }

    #[tokio::test]
    async fn refuses_an_ecb_key_that_selected_every_currency() {
        // The trap: HTTP 200, a well-formed body, and three series in it. Read
        // as one, this draws a line that jumps between currencies.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/service/data/EXR/D..EUR.SP00.A"))
            .respond_with(ResponseTemplate::new(200).set_body_string(ECB_ALL_CURRENCIES_CSV))
            .mount(&server)
            .await;

        let refused = ecb::get_series(&state_for(&server), "EXR/D..EUR.SP00.A", None, None)
            .await
            .expect_err("a key missing a dimension is not one series");

        assert_eq!(refused.code, codes::BAD_REQUEST);
        assert!(refused
            .message
            .contains("matches 3 series at the ECB, not one"));
        assert!(refused
            .hint
            .unwrap_or_default()
            .contains("Fill in the empty dimensions"));
    }

    #[tokio::test]
    async fn assembles_an_imf_series_and_asks_for_csv_by_header() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/external/sdmx/2.1/data/CPI/USA.CPI._T.IX.M"))
            // The IMF reads the Accept header and ignores `?format=`; the ECB
            // and the OECD do the reverse. Both are sent, always.
            .and(header(
                "Accept",
                "application/vnd.sdmx.data+csv;version=2.0.0",
            ))
            .and(query_param("format", "csv"))
            .respond_with(ResponseTemplate::new(200).set_body_string(IMF_CPI_CSV))
            .expect(1)
            .mount(&server)
            .await;

        let answer = imf::get_series(&state_for(&server), "CPI/USA.CPI._T.IX.M", None, None)
            .await
            .expect("the series assembles");

        assert_eq!(answer.series.provider, DataSource::Imf);
        assert_eq!(
            answer.series.title,
            "United States — consumer price index, all items"
        );
        assert_eq!(answer.series.frequency, "Monthly");
        // The document states no unit, so the catalogue's fills the gap.
        assert_eq!(answer.series.units, "Index");
        assert_eq!(
            rows(&answer.observations),
            [
                ("2026-01-01", Some(149.160_190_868_838_4)),
                ("2026-02-01", Some(149.863_222_895_088_6)),
                ("2026-03-01", Some(151.435_299_728_738_8)),
            ]
        );
    }

    #[tokio::test]
    async fn reports_the_imfs_two_hundred_with_no_observations_as_an_error() {
        // `US` is not a code the IMF knows — it wants ISO alpha-3 — and it says
        // so with HTTP 200 and the dataflow's own description.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/external/sdmx/2.1/data/CPI/US.CPI._T.IX.M"))
            .respond_with(ResponseTemplate::new(200).set_body_string(IMF_DATAFLOW_ONLY_CSV))
            .mount(&server)
            .await;

        let refused = imf::get_series(&state_for(&server), "CPI/US.CPI._T.IX.M", None, None)
            .await
            .expect_err("a description is not a series");

        assert_eq!(refused.code, codes::EMPTY_UPSTREAM);
        assert!(refused
            .message
            .contains("the IMF returned no readable observations"));
    }

    #[tokio::test]
    async fn assembles_an_oecd_series_without_escaping_the_dataflows_at_sign() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            // `DSD_KEI@DF_KEI`, unescaped: `%40` here has been seen to earn a
            // 500 from the OECD, and the `@` is legal in a path segment anyway.
            .and(path_matcher(
                "/public/rest/data/DSD_KEI@DF_KEI/USA.M.CP.GR._Z._Z.GY",
            ))
            .and(query_param("format", "csvfilewithlabels"))
            .respond_with(ResponseTemplate::new(200).set_body_string(OECD_KEI_CSV))
            .expect(1)
            .mount(&server)
            .await;

        let answer = oecd::get_series(
            &state_for(&server),
            "DSD_KEI@DF_KEI/USA.M.CP.GR._Z._Z.GY",
            None,
            None,
        )
        .await
        .expect("the series assembles");

        assert_eq!(answer.series.provider, DataSource::Oecd);
        assert_eq!(
            answer.series.title,
            "United States — consumer prices, year on year"
        );
        assert_eq!(answer.series.frequency, "Monthly");
        assert_eq!(
            rows(&answer.observations),
            [
                ("2026-04-01", Some(3.810_844_932_1)),
                ("2026-05-01", Some(4.248_674_039_164_47)),
                ("2026-06-01", Some(3.531_425_063_786_41)),
            ]
        );
        // No stable per-series permalink at the Data Explorer, so the link is
        // the query itself — built from the configured base, because a hardcoded
        // host would send a reader somewhere this deployment never asked.
        assert_eq!(
            answer.series.source_url,
            format!(
                "{}/public/rest/data/DSD_KEI@DF_KEI/USA.M.CP.GR._Z._Z.GY\
                 ?format=csvfilewithlabels",
                server.uri()
            )
        );
    }

    #[tokio::test]
    async fn names_an_oecd_five_hundred_as_the_throttle_it_is() {
        // The OECD sheds a burst of requests with 500 rather than 429. Reported
        // as an outage, that sends a reader looking for a broken dataset.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher(
                "/public/rest/data/DSD_KEI@DF_KEI/USA.M.CP.GR._Z._Z.GY",
            ))
            .respond_with(ResponseTemplate::new(500).set_body_string("Server Error"))
            // Three attempts after the first: the throttle clears in seconds and
            // failing on the first 500 would turn a busy minute into an outage.
            .expect(4)
            .mount(&server)
            .await;

        let refused = oecd::get_series(
            &state_for(&server),
            "DSD_KEI@DF_KEI/USA.M.CP.GR._Z._Z.GY",
            None,
            None,
        )
        .await
        .expect_err("500 is not an answer");

        assert_eq!(refused.code, codes::RATE_LIMITED);
        assert_eq!(refused.status, Some(500));
        assert!(refused.message.contains("the OECD is throttling this IP"));
        assert!(refused
            .hint
            .unwrap_or_default()
            .contains("this is not a bad key"));
    }

    #[tokio::test]
    async fn leaves_a_four_oh_four_alone() {
        // A dataflow that does not exist is a statement about the key, not about
        // the service's load, and must not be dressed up as a throttle.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/service/data/NOPE/D.USD.EUR.SP00.A"))
            .respond_with(ResponseTemplate::new(404).set_body_string("{\"detail\":\"No results\"}"))
            .mount(&server)
            .await;

        let refused = ecb::get_series(&state_for(&server), "NOPE/D.USD.EUR.SP00.A", None, None)
            .await
            .expect_err("404 is not an answer");
        assert_eq!(refused.code, codes::NOT_FOUND);
    }

    #[tokio::test]
    async fn refuses_an_id_with_no_dataflow_before_asking_anyone() {
        let server = MockServer::start().await;
        // Nothing is mounted: reaching the network at all would fail this test.
        let refused = ecb::get_series(&state_for(&server), "   ", None, None)
            .await
            .expect_err("an empty id names nothing");

        assert_eq!(refused.code, codes::BAD_REQUEST);
        assert!(refused.message.contains("does not name a the ECB dataflow"));
        assert!(refused
            .hint
            .unwrap_or_default()
            .contains("the dataflow, a slash, then the series key"));
    }

    #[tokio::test]
    async fn asks_the_service_once_for_a_series_two_panels_want() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/service/data/EXR/D.USD.EUR.SP00.A"))
            .respond_with(ResponseTemplate::new(200).set_body_string(ECB_EXR_CSV))
            // Single-flight and then cached: published data does not move within
            // the half hour, and these services throttle per address.
            .expect(1)
            .mount(&server)
            .await;

        let state = state_for(&server);
        let (first, second) = futures::join!(
            ecb::get_series(&state, "EXR/D.USD.EUR.SP00.A", None, None),
            ecb::get_series(&state, "EXR/D.USD.EUR.SP00.A", None, None),
        );
        assert_eq!(
            first.expect("the first call answers").observations.len(),
            second.expect("the second call answers").observations.len()
        );
    }

    #[tokio::test]
    async fn a_different_period_window_is_a_different_request() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/service/data/EXR/D.USD.EUR.SP00.A"))
            .respond_with(ResponseTemplate::new(200).set_body_string(ECB_EXR_CSV))
            .expect(2)
            .mount(&server)
            .await;

        let state = state_for(&server);
        ecb::get_series(&state, "EXR/D.USD.EUR.SP00.A", Some("2026-01-01"), None)
            .await
            .expect("the first window answers");
        ecb::get_series(&state, "EXR/D.USD.EUR.SP00.A", Some("2026-02-01"), None)
            .await
            .expect("the second window answers");
    }

    #[tokio::test]
    async fn a_flow_with_no_key_asks_for_the_whole_flow() {
        // `all` is SDMX's own word for it, and is what the services expect in
        // the key position rather than an empty path segment.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/service/data/EXR/all"))
            .respond_with(ResponseTemplate::new(200).set_body_string(ECB_EXR_CSV))
            .expect(1)
            .mount(&server)
            .await;

        let answer = ecb::get_series(&state_for(&server), "EXR", None, None)
            .await
            .expect("the whole flow answers");
        assert_eq!(answer.series.id, "EXR/");
    }

    /* -------------------------------------------------- the agency modules */

    #[tokio::test]
    async fn every_agency_answers_a_catalogue_search() {
        let server = MockServer::start().await;
        let state = state_for(&server);

        let euro = ecb::search(&state, "inflation", 5)
            .await
            .expect("ECB search");
        assert!(!euro.is_empty());
        assert!(euro.iter().all(|row| row.provider == DataSource::Ecb));

        let fund = imf::search(&state, "germany cpi", 5)
            .await
            .expect("IMF search");
        assert_eq!(ids(&fund), ["CPI/DEU.CPI._T.IX.M"]);

        let club = oecd::search(&state, "japan unemployment", 5)
            .await
            .expect("OECD search");
        assert_eq!(ids(&club), ["DSD_KEI@DF_KEI/JPN.M.UNEMP.PT_LF._T.Y._Z"]);
    }

    #[tokio::test]
    async fn no_agency_needs_a_credential_this_deployment_might_lack() {
        // All three services answer anonymously, so `SRC` should never show one
        // of them as skipped for want of a key.
        let server = MockServer::start().await;
        let state = state_for(&server);
        assert_eq!(ecb::unavailable(&state), None);
        assert_eq!(imf::unavailable(&state), None);
        assert_eq!(oecd::unavailable(&state), None);
    }
}
