//! US Energy Information Administration — api.eia.gov v2.
//!
//! The settlement feed for energy markets: the weekly retail gasoline price a
//! "gas above $X" contract resolves on, Henry Hub, WTI and Brent spot, crude
//! inventories, electricity generation. Kalshi lists several of these directly,
//! so reading the publisher rather than a mirror is the difference between
//! settling on time and settling on somebody else's refresh schedule.
//!
//! **There is no anonymous tier, and that is a gate rather than a degraded
//! mode.** Every route answers 403 to a request with no key. Verified live from
//! this container:
//!
//! ```text
//! $ curl -s -w '%{http_code}\n' https://api.eia.gov/v2/seriesid/PET.RWTC.D
//! {"error":{"code":"API_KEY_MISSING",
//!           "message":"No api_key was supplied.  Please register for one at
//!                      https://www.eia.gov/opendata/register.php"}}
//! 403
//! ```
//!
//! So [`unavailable`] reports the publisher as unusable when `EIA_API_KEY` is
//! unset, and `ECOS` skips it with that sentence instead of fanning a request
//! out to a host that can only ever say no. Asking anyway would spend a request
//! and a reader's patience to learn something knowable before the socket opens.
//! `fixtures/eia_api_key_missing.json` is that captured refusal.
//!
//! Two id forms, because the v2 API has two front doors and they suit different
//! readers:
//!
//! ```text
//! PET.RWTC.D                    a v1-style series id, resolved through the
//!                               /seriesid compatibility route. This is what
//!                               published documentation and most of the
//!                               internet quotes, so it is the form that works
//!                               when a reader pastes something in.
//! petroleum/pri/spt/data?facets[series][]=RWTC&frequency=daily
//!                               the native route form, for everything the
//!                               compatibility layer does not cover. The
//!                               reader's own query survives intact and only
//!                               the parameters they left unset are filled in.
//! ```
//!
//! Three ways this API is wrong at you without ever failing, and what each
//! costs:
//!
//! **A page is a truncation, and ascending order truncates the wrong end.** The
//! v2 API caps one request at 5,000 rows and states the full count in
//! `response.total`. WTI spot has traded daily since 1986 — over ten thousand
//! readings — so a full-history request is *always* short, and the answer is
//! well-formed either way. Asking for it ascending, as the TypeScript this
//! replaces did, keeps the first 5,000 rows in that ordering: 1986 to 2005, and
//! a chart that stops twenty years ago. This module asks descending and sorts
//! locally, so what a truncation loses is the oldest history rather than every
//! price since the reader was born, and [`truncation_note`] puts the loss on
//! the series itself rather than leaving it to be inferred from where the line
//! stops.
//!
//! **The value column is not always called `value`.** A route with one figure
//! per row names it `value`; `electricity/retail-sales` carries `price`,
//! `revenue`, `sales` and `customers`, and a caller asks for one of them with
//! their own `data[0]=price`. Reading `row.value` regardless — again, what the
//! TypeScript did — yields a row set that parses perfectly into a chart of
//! nothing but nulls. [`data_column`] reads back whichever column was asked for.
//!
//! **A figure the EIA does not state is not zero.** A null `value`, an empty
//! string and a non-numeric cell are all [`None`]. A zero in a gasoline price
//! series is a reading somebody acts on, and nobody published it.
//!
//! Periods here are the widest of any publisher in the terminal — hourly
//! electricity demand through to annual consumption — and every one of them is
//! read by [`crate::period::period_to_date`], which dates `2026-08-12T14` to
//! its day. Twenty-four hourly readings therefore collapse onto one date, and
//! [`crate::sources::sdmx::dedupe_observations`] keeps the last of them rather
//! than drawing a vertical smear where a day should be.

use std::sync::{Arc, LazyLock};
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Map, Value};
use terminal_core::dataset::DataSource;
use terminal_core::types::{DataObservation, DataSearchResult, DataSeries, DataSeriesResponse};

use crate::app::AppState;
use crate::cache::ttl;
use crate::error::{codes, Result, UpstreamError};
use crate::http::FetchOptions;
use crate::period::period_to_date;
use crate::sources::sdmx::{dedupe_observations, search_catalogue, SdmxCatalogueEntry};

/// A single attempt's budget. A long daily history is a five-thousand-row JSON
/// document and the API takes its time assembling one.
const TIMEOUT: Duration = Duration::from_secs(40);

/// One retry. A 403 is definitive and is not retried by the transport anyway;
/// this only ever costs something on a genuine transport failure.
const RETRIES: u32 = 1;

/// Rows per request. The API's own ceiling — asking for more is not an error,
/// it is silently served 5,000.
const PAGE: usize = 5_000;

/// The EIA's browser, which is where a reader checks a series by hand. There is
/// no per-series permalink to give them.
const SOURCE_URL: &str = "https://www.eia.gov/opendata/browser/";

/// Where a key comes from, quoted wherever this module has to refuse.
const REGISTER_URL: &str = "https://www.eia.gov/opendata/register.php";

/* ------------------------------------------------------------ credentials */

/// The configured key, treated as absent when it is blank.
fn api_key(state: &AppState) -> Option<&str> {
    state
        .config()
        .eia_api_key
        .as_deref()
        .map(str::trim)
        .filter(|key| !key.is_empty())
}

/// `None` when this deployment can use the publisher; a string is the
/// operator-facing reason it cannot.
///
/// Unlike every other publisher here, the absence of a key is total: there is
/// no anonymous tier to fall back to and no reduced answer to serve, so this is
/// a hard gate and the sentence it returns is the whole of what an operator
/// needs to fix it.
pub fn unavailable(state: &AppState) -> Option<String> {
    if api_key(state).is_some() {
        return None;
    }
    Some(format!(
        "The EIA publishes nothing anonymously — every route answers HTTP 403 without a key. \
         Register free at {REGISTER_URL} and set EIA_API_KEY."
    ))
}

/// The key, or the error a reader sees in the panel that asked for a chart.
fn require_key(state: &AppState) -> Result<&str> {
    api_key(state).ok_or_else(|| {
        UpstreamError::not_configured("The EIA needs an API key, and none is set").with_hint(
            format!(
                "The EIA publishes nothing anonymously. Register free at {REGISTER_URL} and set \
                 EIA_API_KEY."
            ),
        )
    })
}

/* -------------------------------------------------------------- the query */

/// The query half of an id, read and rebuilt rather than pasted through.
///
/// A reader writes `?facets[series][]=RWTC` with the brackets literal, and the
/// wire needs them percent-encoded; a reader who has already encoded them must
/// not have their `%5B` encoded a second time into `%255B`. So each pair is
/// decoded on the way in and encoded on the way out, which is what
/// `URLSearchParams` did for the TypeScript this replaces.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct Query(Vec<(String, String)>);

impl Query {
    /// Read `a=1&b=2`, tolerating an empty string, a stray `&` and a key with
    /// no `=`.
    fn parse(raw: &str) -> Self {
        let pairs = raw
            .split('&')
            .filter(|pair| !pair.is_empty())
            .map(|pair| match pair.split_once('=') {
                Some((key, value)) => (decode(key), decode(value)),
                None => (decode(pair), String::new()),
            })
            .collect();
        Self(pairs)
    }

    fn has(&self, key: &str) -> bool {
        self.0.iter().any(|(name, _)| name == key)
    }

    fn append(&mut self, key: &str, value: impl Into<String>) {
        self.0.push((key.to_owned(), value.into()));
    }

    /// Replace every existing value for `key` with one. `URLSearchParams.set`,
    /// and the reason a reader's own `start=` cannot outrank the range the
    /// panel asked for.
    fn set(&mut self, key: &str, value: impl Into<String>) {
        self.0.retain(|(name, _)| name != key);
        self.0.push((key.to_owned(), value.into()));
    }

    fn encode(&self) -> String {
        self.0
            .iter()
            .map(|(key, value)| {
                format!(
                    "{}={}",
                    urlencoding::encode(key),
                    urlencoding::encode(value)
                )
            })
            .collect::<Vec<_>>()
            .join("&")
    }
}

/// Percent-decode, falling back to the raw text where the escape is malformed.
///
/// A broken escape is the reader's typo and belongs in the request they can see
/// the answer to, not in an error from a decoder they never invoked.
fn decode(raw: &str) -> String {
    urlencoding::decode(raw)
        .map(std::borrow::Cow::into_owned)
        .unwrap_or_else(|_| raw.to_owned())
}

/// Split `path?query` into its two halves, tolerating a missing query.
fn split_route(id: &str) -> (&str, &str) {
    match id.split_once('?') {
        Some((path, query)) => (path, query),
        None => (id, ""),
    }
}

/// Which row key carries the figure.
///
/// Whatever the caller asked for in `data[0]`, because the API answers with the
/// column they named and nothing else. `value` is the default and the right
/// answer for every route with one figure per row.
fn data_column(query: &Query) -> String {
    query
        .0
        .iter()
        .find(|(key, value)| key.starts_with("data[") && !value.is_empty())
        .map(|(_, value)| value.clone())
        .unwrap_or_else(|| "value".to_owned())
}

/// Whether an id names a native v2 route rather than a v1-style series id.
///
/// Decided on the path alone. The TypeScript this replaces tested the whole id,
/// so `PET.RWTC.D?length=100` — a v1 id carrying a parameter, which is a
/// perfectly reasonable thing to type — was read as a series id and
/// percent-encoded whole, sending `/seriesid/PET.RWTC.D%3Flength%3D100`.
fn is_native(path: &str) -> bool {
    path.contains('/')
}

/// The request URL for `id`, with the reader's own parameters preserved.
///
/// Only the parameters the caller left unset are filled in, so a native route
/// id can specify its own facets, frequency, ordering and page size and get
/// exactly those.
fn build_url(
    base: &str,
    key: &str,
    raw_id: &str,
    start: Option<&str>,
    end: Option<&str>,
) -> String {
    let id = raw_id.trim().trim_start_matches('/');
    let (path, raw_query) = split_route(id);
    let mut query = Query::parse(raw_query);

    let url = if is_native(path) {
        // `petroleum/pri/spt` and `petroleum/pri/spt/data` are the same route
        // written two ways; both take the one `/data/` suffix, not two.
        let route = path.trim_end_matches('/');
        let route = route.strip_suffix("/data").unwrap_or(route);
        format!("{}/{route}/data/", base.trim_end_matches('/'))
    } else {
        format!(
            "{}/seriesid/{}",
            base.trim_end_matches('/'),
            urlencoding::encode(path)
        )
    };

    query.set("api_key", key);
    if !query.0.iter().any(|(name, _)| name.starts_with("data[")) {
        query.append("data[0]", "value");
    }
    if let Some(start) = start {
        query.set("start", start);
    }
    if let Some(end) = end {
        query.set("end", end);
    }
    // Descending, so the 5,000-row ceiling costs the oldest history rather than
    // everything published since. The observations are sorted into chart order
    // here afterwards, so the reader never sees this ordering.
    if !query.has("sort[0][column]") {
        query.append("sort[0][column]", "period");
        query.append("sort[0][direction]", "desc");
    }
    if !query.has("length") {
        query.set("length", PAGE.to_string());
    }

    format!("{url}?{}", query.encode())
}

/* ------------------------------------------------------------ raw upstream */

/// A figure, which arrives as a JSON number on some routes and a decimal string
/// on others.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum RawEiaNumber {
    Number(f64),
    Text(String),
}

impl RawEiaNumber {
    /// The figure, or `None` where there is not one.
    ///
    /// An empty string, a marker like `NA` and anything else unparseable are
    /// all absent readings. JavaScript's `Number("")` is `0`, which is how the
    /// TypeScript this replaces would have charted a blank cell as a price of
    /// nought; refusing it here is deliberate.
    fn value(&self) -> Option<f64> {
        match self {
            Self::Number(value) => value.is_finite().then_some(*value),
            Self::Text(text) => text
                .trim()
                .parse::<f64>()
                .ok()
                .filter(|value| value.is_finite()),
        }
    }
}

/// One reading, plus whatever else the route carries.
///
/// The extra columns are kept rather than dropped because the figure is not
/// always in `value` — see [`data_column`] — and because a route names its
/// per-column units `<column>-units` when it answers more than one.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct RawEiaRow {
    period: Option<String>,
    value: Option<RawEiaNumber>,
    units: Option<String>,
    /// The compatibility route spells it with a hyphen and some native routes
    /// spell it camelCase. Both are read; neither is guaranteed.
    #[serde(rename = "series-description")]
    series_description: Option<String>,
    #[serde(rename = "seriesDescription")]
    series_description_camel: Option<String>,
    #[serde(flatten)]
    other: Map<String, Value>,
}

impl RawEiaRow {
    /// The figure in `column`, wherever the route put it.
    fn figure(&self, column: &str) -> Option<f64> {
        if column == "value" {
            return self.value.as_ref().and_then(RawEiaNumber::value);
        }
        match self.other.get(column)? {
            Value::Number(number) => number.as_f64().filter(|value| value.is_finite()),
            Value::String(text) => text
                .trim()
                .parse::<f64>()
                .ok()
                .filter(|value| value.is_finite()),
            _ => None,
        }
    }

    /// The units this row states, under either of the two names a route uses.
    fn units_for(&self, column: &str) -> Option<String> {
        if let Some(units) = self.units.as_deref().filter(|units| !units.is_empty()) {
            return Some(units.to_owned());
        }
        self.other
            .get(&format!("{column}-units"))
            .and_then(Value::as_str)
            .filter(|units| !units.is_empty())
            .map(str::to_owned)
    }

    fn description(&self) -> Option<&str> {
        self.series_description
            .as_deref()
            .or(self.series_description_camel.as_deref())
            .filter(|text| !text.is_empty())
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct RawEiaBody {
    /// How many rows match, whether or not this page holds them. The one field
    /// that makes a truncation visible.
    total: Option<RawEiaNumber>,
    frequency: Option<String>,
    description: Option<String>,
    data: Option<Vec<RawEiaRow>>,
}

/// What the API says when it is refusing rather than answering.
///
/// An object on every refusal this container could provoke — `{"error":
/// {"code":"API_KEY_MISSING","message":"…"}}` — where the TypeScript this
/// replaces declared it a bare string and would have printed `[object Object]`
/// into the hint a reader is meant to act on. Both shapes are read, because
/// older documentation shows the flat form.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum RawEiaError {
    Message(String),
    Detail {
        code: Option<String>,
        message: Option<String>,
    },
}

impl RawEiaError {
    fn text(&self) -> String {
        match self {
            Self::Message(message) => message.trim().to_owned(),
            Self::Detail { code, message } => match (code.as_deref(), message.as_deref()) {
                (Some(code), Some(message)) => format!("{}: {}", code.trim(), message.trim()),
                (None, Some(message)) => message.trim().to_owned(),
                (Some(code), None) => code.trim().to_owned(),
                (None, None) => String::new(),
            },
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct RawEiaResponse {
    response: Option<RawEiaBody>,
    error: Option<RawEiaError>,
}

/* ---------------------------------------------------------------- series */

/// What to say on a series the API had to cut short, or `""` when it did not.
///
/// A page is not an error and must not be reported as one — five thousand
/// readings is a chart — but a reader looking at a line that begins in 2005 is
/// owed the reason it does. Stated in words on the series rather than inferred
/// from where the line starts.
fn truncation_note(total: Option<f64>, held: usize) -> String {
    match total {
        Some(total) if total > held as f64 => format!(
            "The EIA states {total:.0} observations for this series and answers at most {PAGE} \
             per request; this is the most recent {held}. Narrow the range with a start date to \
             see earlier history."
        ),
        _ => String::new(),
    }
}

/// A series and its observations.
pub async fn get_series(
    state: &AppState,
    raw_id: &str,
    start: Option<&str>,
    end: Option<&str>,
) -> Result<DataSeriesResponse> {
    let id = raw_id.trim().to_owned();
    if id.is_empty() {
        return Err(
            UpstreamError::bad_request("Missing EIA series id").with_hint(
                "Ids look like PET.RWTC.D (WTI spot), NG.RNGWHHD.D (Henry Hub), or a native route \
             such as petroleum/pri/spt/data?facets[series][]=RWTC.",
            ),
        );
    }

    // Before the cache and before the socket: with no key this request has one
    // possible outcome, and spending a round trip to reach it helps nobody.
    let key = require_key(state)?.to_owned();

    let cache_key = format!(
        "eia:series:{id}:{}:{}",
        start.unwrap_or_default(),
        end.unwrap_or_default()
    );

    let response = state
        .cache()
        .cached(&cache_key, ttl::FRED, || async {
            let url = build_url(&state.config().eia_api_base, &key, &id, start, end);
            let payload: RawEiaResponse = state
                .http()
                .fetch_json(&url, FetchOptions::new().timeout(TIMEOUT).retries(RETRIES))
                .await
                .map_err(|err| explain(err, &id))?;

            let body = payload.response.unwrap_or_default();
            let rows = body.data.unwrap_or_default();
            if rows.is_empty() {
                // A route that exists and a facet that matches nothing are the
                // same answer on the wire: HTTP 200, a well-formed envelope and
                // an empty `data` array. So the message has to leave both open.
                let hint = payload
                    .error
                    .as_ref()
                    .map(RawEiaError::text)
                    .filter(|text| !text.is_empty())
                    .unwrap_or_else(|| {
                        "Either the id is wrong or its facets match nothing. `ECOS <words> eia` \
                         lists the series this terminal knows."
                            .to_owned()
                    });
                return Err(UpstreamError::not_found(format!(
                    "The EIA returned no observations for {id}"
                ))
                .with_hint(hint));
            }

            let (_, raw_query) = split_route(id.trim_start_matches('/'));
            let column = data_column(&Query::parse(raw_query));

            // Sorted on the publisher's own period rather than on the date it
            // resolves to, so that twenty-four hourly readings collapsing onto
            // one day keep the last hour of it rather than an arbitrary one.
            let mut dated: Vec<(&str, DataObservation)> = Vec::with_capacity(rows.len());
            for row in &rows {
                let period = row.period.as_deref().unwrap_or_default();
                let Some(date) = period_to_date(period) else {
                    continue;
                };
                dated.push((
                    period,
                    DataObservation {
                        date,
                        value: row.figure(&column),
                    },
                ));
            }
            dated.sort_by(|a, b| a.0.cmp(b.0));

            let observations =
                dedupe_observations(dated.into_iter().map(|(_, point)| point).collect());

            let known = catalogue_entry(&id);
            let first = &rows[0];
            let units = first
                .units_for(&column)
                .or_else(|| known.map(|entry| entry.units.clone()))
                .unwrap_or_default();
            let title = first
                .description()
                .map(str::to_owned)
                .or_else(|| body.description.filter(|text| !text.is_empty()))
                .or_else(|| known.map(|entry| entry.title.clone()))
                .unwrap_or_else(|| id.clone());

            Ok(DataSeriesResponse {
                series: DataSeries {
                    provider: DataSource::Eia,
                    id: id.clone(),
                    title,
                    units_short: units.clone(),
                    units,
                    frequency: body
                        .frequency
                        .filter(|text| !text.is_empty())
                        .or_else(|| known.map(|entry| entry.frequency.clone()))
                        .unwrap_or_default(),
                    // The EIA states no seasonal adjustment and no revision
                    // timestamp on these routes. An invented one would be read
                    // as a release time.
                    seasonal_adjustment: String::new(),
                    last_updated: String::new(),
                    observation_start: observations
                        .first()
                        .map(|point| point.date.clone())
                        .unwrap_or_default(),
                    observation_end: observations
                        .last()
                        .map(|point| point.date.clone())
                        .unwrap_or_default(),
                    notes: truncation_note(
                        body.total.as_ref().and_then(RawEiaNumber::value),
                        observations.len(),
                    ),
                    // Which front door answered. A reader weighs a figure from
                    // the compatibility layer differently from one addressed at
                    // the route that owns it.
                    source: if is_native(split_route(&id).0) {
                        "v2".to_owned()
                    } else {
                        "seriesid".to_owned()
                    },
                    source_url: SOURCE_URL.to_owned(),
                },
                observations,
            })
        })
        .await?;

    Ok(Arc::unwrap_or_clone(response))
}

/// Name a rejected key as a rejected key.
///
/// The transport reports a 403 as `upstream_status`, which reads as "the EIA is
/// having a bad day". It is not: the one thing that answers 403 here is a key
/// the EIA will not accept, and the remedy is a new one rather than a retry.
fn explain(err: UpstreamError, id: &str) -> UpstreamError {
    if err.status != Some(403) {
        return err;
    }
    UpstreamError::new(
        format!("The EIA rejected this deployment's API key (asking for {id})"),
        codes::BAD_CREDENTIALS,
    )
    .with_status(403)
    .with_hint(format!(
        "EIA_API_KEY is set but the EIA will not accept it. Keys are free and issued instantly at \
         {REGISTER_URL}."
    ))
}

/* -------------------------------------------------------------- catalogue */

/// The energy prints markets settle on. Any other id still charts; this is only
/// what `ECOS` can offer before a reader knows one.
///
/// Held as [`SdmxCatalogueEntry`] — the shape the SDMX agencies use — because
/// the ranking rule is the same rule, and a second copy of it would be a second
/// place for `eia oil` to start behaving differently from `ecb oil`. The type
/// is named for where it was first needed, not for a wire format the EIA
/// speaks.
static CATALOGUE: LazyLock<Vec<SdmxCatalogueEntry>> = LazyLock::new(|| {
    vec![
        SdmxCatalogueEntry::new("PET.RWTC.D", "Cushing, OK WTI spot price FOB")
            .with_units("Dollars per barrel")
            .with_frequency("Daily")
            .with_keywords("crude oil wti spot petroleum"),
        SdmxCatalogueEntry::new("PET.RBRTE.D", "Europe Brent spot price FOB")
            .with_units("Dollars per barrel")
            .with_frequency("Daily")
            .with_keywords("crude oil brent spot petroleum"),
        SdmxCatalogueEntry::new(
            "PET.EMM_EPMR_PTE_NUS_DPG.W",
            "US regular all-formulations retail gasoline price",
        )
        .with_units("Dollars per gallon")
        .with_frequency("Weekly")
        .with_keywords("gasoline petrol pump price retail gas"),
        SdmxCatalogueEntry::new(
            "PET.EMD_EPD2D_PTE_NUS_DPG.W",
            "US No. 2 diesel retail price",
        )
        .with_units("Dollars per gallon")
        .with_frequency("Weekly")
        .with_keywords("diesel fuel retail price"),
        SdmxCatalogueEntry::new(
            "PET.WCESTUS1.W",
            "US ending stocks of crude oil excluding SPR",
        )
        .with_units("Thousand barrels")
        .with_frequency("Weekly")
        .with_keywords("crude inventories stocks eia build draw"),
        SdmxCatalogueEntry::new(
            "PET.WCSSTUS1.W",
            "US ending stocks of crude oil in the Strategic Petroleum Reserve",
        )
        .with_units("Thousand barrels")
        .with_frequency("Weekly")
        .with_keywords("spr strategic petroleum reserve stocks"),
        SdmxCatalogueEntry::new("NG.RNGWHHD.D", "Henry Hub natural gas spot price")
            .with_units("Dollars per million Btu")
            .with_frequency("Daily")
            .with_keywords("natural gas henry hub spot nat gas"),
        SdmxCatalogueEntry::new(
            "NG.NW2_EPG0_SWO_R48_BCF.W",
            "US lower 48 working natural gas in underground storage",
        )
        .with_units("Billion cubic feet")
        .with_frequency("Weekly")
        .with_keywords("natural gas storage inventories injection withdrawal"),
        SdmxCatalogueEntry::new(
            "ELEC.GEN.ALL-US-99.M",
            "US net electricity generation, all sectors, all fuels",
        )
        .with_units("Thousand megawatthours")
        .with_frequency("Monthly")
        .with_keywords("electricity generation power grid"),
        SdmxCatalogueEntry::new("TOTAL.TETCBUS.M", "US total primary energy consumption")
            .with_units("Trillion Btu")
            .with_frequency("Monthly")
            .with_keywords("energy consumption total primary"),
    ]
});

fn catalogue_entry(id: &str) -> Option<&'static SdmxCatalogueEntry> {
    CATALOGUE.iter().find(|entry| entry.id == id)
}

/// Search the curated catalogue. Reads no network, so it cannot fail.
///
/// Deliberately not gated on the key. The list is public knowledge and costs
/// nothing to answer; a deployment with no key never reaches here anyway,
/// because [`unavailable`] takes the publisher out of the fan-out before the
/// search starts.
pub async fn search(_state: &AppState, query: &str, limit: usize) -> Result<Vec<DataSearchResult>> {
    Ok(search_catalogue(DataSource::Eia, &CATALOGUE, query, limit))
}

#[cfg(test)]
mod tests {
    //! EIA parser and wire tests.
    //!
    //! No test here has ever spoken to `api.eia.gov`, and none could: the
    //! container this was ported in holds no key and the API has no anonymous
    //! tier, so the only live evidence available is the refusal in
    //! `fixtures/eia_api_key_missing.json` — which is captured, verbatim, and
    //! is what the gate in [`unavailable`] exists for. Everything else is built
    //! to the documented shape of the v2 API and to the payload the TypeScript
    //! this replaces was written against, and the URL half is asserted in full,
    //! because with no live path the request we send is the part most worth
    //! pinning down.

    use super::*;
    use serde_json::json;
    use wiremock::matchers::{method, path as path_matcher, query_param};
    use wiremock::{Mock, MockServer, Request, ResponseTemplate};

    use crate::config::Config;

    /// The live 403, captured with curl and unedited. See the module docs.
    const KEY_MISSING_JSON: &str = include_str!("fixtures/eia_api_key_missing.json");

    const KEY: &str = "fixture-key";

    fn keyed_state_for(server: &MockServer) -> AppState {
        AppState::new(Config {
            eia_api_base: server.uri(),
            eia_api_key: Some(KEY.to_owned()),
            ..Config::default()
        })
    }

    fn keyless_state_for(server: &MockServer) -> AppState {
        AppState::new(Config {
            eia_api_base: server.uri(),
            eia_api_key: None,
            ..Config::default()
        })
    }

    /// `(date, value)` pairs, since the wire type carries no `PartialEq`.
    fn rows(observations: &[DataObservation]) -> Vec<(&str, Option<f64>)> {
        observations
            .iter()
            .map(|observation| (observation.date.as_str(), observation.value))
            .collect()
    }

    /// A two-row weekly gasoline answer in the shape the compatibility route
    /// sends: the figure as a JSON number, the units and the description
    /// repeated on every row.
    fn gasoline_body() -> serde_json::Value {
        json!({
            "response": {
                "total": "2",
                "dateFormat": "YYYY-MM-DD",
                "frequency": "weekly",
                "data": [
                    {
                        "period": "2026-08-10",
                        "value": 3.11,
                        "units": "$/GAL",
                        "series-description": "US regular gasoline"
                    },
                    {
                        "period": "2026-08-03",
                        "value": 3.09,
                        "units": "$/GAL",
                        "series-description": "US regular gasoline"
                    }
                ]
            }
        })
    }

    fn url_of(request: &Request) -> String {
        format!(
            "{}?{}",
            request.url.path(),
            request.url.query().unwrap_or_default()
        )
    }

    /* --------------------------------------------------------------- the key */

    #[test]
    fn a_deployment_with_no_key_declares_itself_unusable_rather_than_degraded() {
        // There is no anonymous tier to fall back to, so this is the whole
        // difference between `ECOS` naming a publisher it skipped and a reader
        // watching a panel fail with someone else's HTTP status.
        let reason = unavailable(&AppState::new(Config::default())).expect("no key is a gate");
        assert!(reason.contains("EIA_API_KEY"));
        assert!(reason.contains(REGISTER_URL));
        assert!(reason.contains("403"));
    }

    #[test]
    fn a_blank_key_is_no_key() {
        // An env var set to the empty string is how a deployment usually ends
        // up here, and it must not read as configured.
        for blank in ["", "   "] {
            assert!(unavailable(&AppState::new(Config {
                eia_api_key: Some(blank.to_owned()),
                ..Config::default()
            }))
            .is_some());
        }
    }

    #[test]
    fn a_key_makes_the_publisher_available() {
        assert_eq!(
            unavailable(&AppState::new(Config {
                eia_api_key: Some(KEY.to_owned()),
                ..Config::default()
            })),
            None
        );
    }

    #[test]
    fn the_captured_refusal_carries_an_object_where_the_typescript_expected_a_string() {
        // `{"error":{"code":…,"message":…}}`, live from api.eia.gov. Read as a
        // bare string it renders as `[object Object]` in the one sentence a
        // reader is meant to act on, which is why the error field is untagged.
        let payload: RawEiaResponse =
            serde_json::from_str(KEY_MISSING_JSON).expect("the captured refusal parses");
        let text = payload.error.expect("a refusal states its error").text();
        assert!(text.starts_with("API_KEY_MISSING: "));
        assert!(text.contains(REGISTER_URL));
    }

    #[test]
    fn the_flat_error_form_is_read_too() {
        let payload: RawEiaResponse =
            serde_json::from_str(r#"{"error":"invalid series id"}"#).expect("the flat form parses");
        assert_eq!(payload.error.expect("an error").text(), "invalid series id");
    }

    /* --------------------------------------------------------------- the URL */

    #[test]
    fn a_v1_style_id_goes_through_the_compatibility_route() {
        let url = build_url("https://eia.test/v2", KEY, "PET.RWTC.D", None, None);
        assert!(
            url.starts_with("https://eia.test/v2/seriesid/PET.RWTC.D?"),
            "{url}"
        );
        assert!(url.contains("api_key=fixture-key"));
        assert!(url.contains("data%5B0%5D=value"));
        assert!(url.contains("length=5000"));
    }

    #[test]
    fn a_route_path_goes_to_the_native_form_and_gains_one_data_suffix() {
        let native = build_url("https://eia.test/v2", KEY, "petroleum/pri/spt", None, None);
        assert!(native.starts_with("https://eia.test/v2/petroleum/pri/spt/data/?"));

        // An id that already ends in `/data` is the same route written out, and
        // must not become `/data/data/`.
        for written_out in ["petroleum/pri/spt/data", "petroleum/pri/spt/data/"] {
            let url = build_url("https://eia.test/v2", KEY, written_out, None, None);
            assert!(
                url.starts_with("https://eia.test/v2/petroleum/pri/spt/data/?"),
                "{url}"
            );
        }
    }

    #[test]
    fn a_leading_slash_does_not_double_the_one_in_the_base() {
        let url = build_url(
            "https://eia.test/v2/",
            KEY,
            "/petroleum/pri/spt",
            None,
            None,
        );
        assert!(
            url.starts_with("https://eia.test/v2/petroleum/pri/spt/data/?"),
            "{url}"
        );
    }

    #[test]
    fn asks_descending_so_the_five_thousand_row_ceiling_costs_the_oldest_history() {
        // WTI has traded daily since 1986. Ascending — which is what the
        // TypeScript sent — the page holds 1986 to 2005 and the chart stops
        // twenty years ago, with nothing on the wire to say so.
        let url = build_url("https://eia.test/v2", KEY, "PET.RWTC.D", None, None);
        assert!(url.contains("sort%5B0%5D%5Bcolumn%5D=period"), "{url}");
        assert!(url.contains("sort%5B0%5D%5Bdirection%5D=desc"), "{url}");
    }

    #[test]
    fn a_readers_own_facets_survive_and_are_encoded_for_the_wire() {
        let url = build_url(
            "https://eia.test/v2",
            KEY,
            "petroleum/pri/spt/data?facets[series][]=RWTC&frequency=daily",
            None,
            None,
        );
        assert!(url.contains("facets%5Bseries%5D%5B%5D=RWTC"), "{url}");
        assert!(url.contains("frequency=daily"), "{url}");
    }

    #[test]
    fn a_facet_the_reader_already_encoded_is_not_encoded_twice() {
        // `%5B` becoming `%255B` is a facet the EIA does not have, answered
        // with an empty `data` array and an HTTP 200.
        let url = build_url(
            "https://eia.test/v2",
            KEY,
            "petroleum/pri/spt/data?facets%5Bseries%5D%5B%5D=RWTC",
            None,
            None,
        );
        assert!(url.contains("facets%5Bseries%5D%5B%5D=RWTC"), "{url}");
        assert!(!url.contains("%255B"), "{url}");
    }

    #[test]
    fn the_readers_own_ordering_length_and_column_all_win() {
        let url = build_url(
            "https://eia.test/v2",
            KEY,
            "electricity/retail-sales/data?data[0]=price&sort[0][column]=period\
             &sort[0][direction]=asc&length=10",
            None,
            None,
        );
        assert!(url.contains("data%5B0%5D=price"), "{url}");
        assert!(!url.contains("data%5B0%5D=value"), "{url}");
        assert!(url.contains("sort%5B0%5D%5Bdirection%5D=asc"), "{url}");
        assert!(!url.contains("direction%5D=desc"), "{url}");
        assert!(url.contains("length=10"), "{url}");
        assert!(!url.contains("length=5000"), "{url}");
    }

    #[test]
    fn the_panels_range_outranks_one_written_into_the_id() {
        // `set`, not `append`: two `start` parameters would leave the EIA to
        // pick one, and the panel's range is the one the reader just asked for.
        let url = build_url(
            "https://eia.test/v2",
            KEY,
            "petroleum/pri/spt/data?start=1990-01-01",
            Some("2020-01-01"),
            Some("2020-12-31"),
        );
        assert!(url.contains("start=2020-01-01"), "{url}");
        assert!(!url.contains("1990-01-01"), "{url}");
        assert!(url.contains("end=2020-12-31"), "{url}");
    }

    #[test]
    fn a_v1_id_carrying_a_parameter_still_reaches_the_compatibility_route() {
        // The TypeScript decided nativeness on the whole id, so this one had
        // its `?` percent-encoded into the path and asked for a series called
        // `PET.RWTC.D%3Flength%3D100`.
        let url = build_url(
            "https://eia.test/v2",
            KEY,
            "PET.RWTC.D?length=100",
            None,
            None,
        );
        assert!(
            url.starts_with("https://eia.test/v2/seriesid/PET.RWTC.D?"),
            "{url}"
        );
        assert!(url.contains("length=100"), "{url}");
    }

    #[test]
    fn the_query_reader_survives_the_shapes_a_reader_can_type() {
        assert_eq!(Query::parse(""), Query::default());
        assert_eq!(Query::parse("&&"), Query::default());
        // A key with no `=` is a key with an empty value, as URLSearchParams
        // reads it — not a pair to be dropped.
        assert_eq!(
            Query::parse("frequency"),
            Query(vec![("frequency".to_owned(), String::new())])
        );
        // A malformed escape is the reader's typo and is passed through, so
        // they see the EIA's answer to what they actually asked.
        assert_eq!(
            Query::parse("a=%zz"),
            Query(vec![("a".to_owned(), "%zz".to_owned())])
        );
        // A value containing `=` keeps it; only the first splits the pair.
        assert_eq!(
            Query::parse("a=b=c"),
            Query(vec![("a".to_owned(), "b=c".to_owned())])
        );
    }

    #[test]
    fn the_figure_column_is_whichever_one_was_asked_for() {
        assert_eq!(data_column(&Query::parse("")), "value");
        assert_eq!(data_column(&Query::parse("data[0]=price")), "price");
        assert_eq!(data_column(&Query::parse("data[]=revenue")), "revenue");
        // An empty `data[0]` states nothing, so the default stands rather than
        // the module reading a column called "".
        assert_eq!(data_column(&Query::parse("data[0]=")), "value");
    }

    /* ------------------------------------------------------------- the wire */

    #[tokio::test]
    async fn assembles_a_series_from_the_compatibility_route() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/seriesid/PET.EMM_EPMR_PTE_NUS_DPG.W"))
            .respond_with(ResponseTemplate::new(200).set_body_json(gasoline_body()))
            .mount(&server)
            .await;

        let answer = get_series(
            &keyed_state_for(&server),
            "PET.EMM_EPMR_PTE_NUS_DPG.W",
            None,
            None,
        )
        .await
        .expect("the series resolves");

        // The rows arrive newest first, because that is what we asked for.
        // A chart wants the opposite.
        assert_eq!(
            rows(&answer.observations),
            [("2026-08-03", Some(3.09)), ("2026-08-10", Some(3.11))]
        );
        assert_eq!(answer.series.provider, DataSource::Eia);
        assert_eq!(answer.series.id, "PET.EMM_EPMR_PTE_NUS_DPG.W");
        assert_eq!(answer.series.title, "US regular gasoline");
        assert_eq!(answer.series.units, "$/GAL");
        assert_eq!(answer.series.units_short, "$/GAL");
        assert_eq!(answer.series.frequency, "weekly");
        assert_eq!(answer.series.source, "seriesid");
        assert_eq!(answer.series.source_url, SOURCE_URL);
        assert_eq!(answer.series.observation_start, "2026-08-03");
        assert_eq!(answer.series.observation_end, "2026-08-10");
        // Two of two: nothing was cut short, so nothing is said about it.
        assert_eq!(answer.series.notes, "");
    }

    #[tokio::test]
    async fn a_native_route_says_which_front_door_answered() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/petroleum/pri/spt/data/"))
            .and(query_param("api_key", KEY))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "response": {
                    "frequency": "daily",
                    "description": "Cushing, OK WTI Spot Price FOB",
                    "data": [{ "period": "2026-08-12", "value": "63.42", "units": "$/BBL" }]
                }
            })))
            .mount(&server)
            .await;

        let answer = get_series(
            &keyed_state_for(&server),
            "petroleum/pri/spt/data?facets[series][]=RWTC&frequency=daily",
            None,
            None,
        )
        .await
        .expect("the series resolves");

        assert_eq!(answer.series.source, "v2");
        // A figure sent as a decimal string is the same figure.
        assert_eq!(rows(&answer.observations), [("2026-08-12", Some(63.42))]);
        // No row states a description, so the envelope's does.
        assert_eq!(answer.series.title, "Cushing, OK WTI Spot Price FOB");
    }

    #[tokio::test]
    async fn a_figure_the_eia_does_not_state_is_not_a_zero() {
        // Every one of these is a reading nobody published. Charted as 0.0 they
        // are a gasoline price of nothing, on a line somebody trades off.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/seriesid/PET.RWTC.D"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "response": { "data": [
                    { "period": "2026-08-10", "value": 63.4, "units": "$/BBL" },
                    { "period": "2026-08-11", "value": null, "units": "$/BBL" },
                    { "period": "2026-08-12", "value": "", "units": "$/BBL" },
                    { "period": "2026-08-13", "value": "NA", "units": "$/BBL" },
                    { "period": "2026-08-14", "units": "$/BBL" }
                ]}
            })))
            .mount(&server)
            .await;

        let answer = get_series(&keyed_state_for(&server), "PET.RWTC.D", None, None)
            .await
            .expect("the series resolves");

        assert_eq!(
            rows(&answer.observations),
            [
                ("2026-08-10", Some(63.4)),
                ("2026-08-11", None),
                ("2026-08-12", None),
                ("2026-08-13", None),
                ("2026-08-14", None),
            ]
        );
    }

    #[tokio::test]
    async fn reads_the_column_the_caller_asked_for_rather_than_one_called_value() {
        // `electricity/retail-sales` has four figures per row and none of them
        // is `value`. Reading `value` regardless parses cleanly into a chart of
        // nulls, which is what the TypeScript this replaces would have drawn.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/electricity/retail-sales/data/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "response": { "data": [
                    {
                        "period": "2026-05",
                        "price": "16.42",
                        "price-units": "cents per kilowatt-hour",
                        "revenue": "18043.1"
                    }
                ]}
            })))
            .mount(&server)
            .await;

        let answer = get_series(
            &keyed_state_for(&server),
            "electricity/retail-sales/data?data[0]=price&facets[sectorid][]=RES",
            None,
            None,
        )
        .await
        .expect("the series resolves");

        assert_eq!(rows(&answer.observations), [("2026-05-01", Some(16.42))]);
        // And the per-column units, which is where a multi-column route puts
        // them.
        assert_eq!(answer.series.units, "cents per kilowatt-hour");
    }

    #[tokio::test]
    async fn an_hourly_feed_becomes_a_daily_chart_keeping_the_last_reading_of_each_day() {
        // The EIA's grid feeds are hourly and `period_to_date` dates each
        // reading to its day. Twenty-four points on one x value is a vertical
        // smear, so the day keeps its last hour.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/electricity/rto/region-data/data/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "response": { "frequency": "hourly", "data": [
                    { "period": "2026-08-12T23", "value": 41_200, "units": "megawatthours" },
                    { "period": "2026-08-12T22", "value": 43_100, "units": "megawatthours" },
                    { "period": "2026-08-11T22", "value": 39_950, "units": "megawatthours" }
                ]}
            })))
            .mount(&server)
            .await;

        let answer = get_series(
            &keyed_state_for(&server),
            "electricity/rto/region-data",
            None,
            None,
        )
        .await
        .expect("the series resolves");

        assert_eq!(
            rows(&answer.observations),
            [
                ("2026-08-11", Some(39_950.0)),
                ("2026-08-12", Some(41_200.0))
            ]
        );
    }

    #[tokio::test]
    async fn says_on_the_series_when_the_api_could_only_send_a_page_of_it() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/seriesid/PET.RWTC.D"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "response": {
                    "total": 10_215,
                    "frequency": "daily",
                    "data": [{ "period": "2026-08-12", "value": 63.42, "units": "$/BBL" }]
                }
            })))
            .mount(&server)
            .await;

        let answer = get_series(&keyed_state_for(&server), "PET.RWTC.D", None, None)
            .await
            .expect("a page is a chart, not a failure");

        assert_eq!(answer.observations.len(), 1);
        assert!(answer.series.notes.contains("10215 observations"));
        assert!(answer.series.notes.contains("most recent 1"));
    }

    #[test]
    fn a_page_that_holds_everything_says_nothing() {
        assert_eq!(truncation_note(Some(4.0), 4), "");
        assert_eq!(truncation_note(None, 4), "");
        // And a `total` the EIA states as a string is the same number.
        assert!(!truncation_note(Some(10_215.0), 5_000).is_empty());
    }

    #[tokio::test]
    async fn an_id_that_matches_nothing_is_not_found_rather_than_an_empty_chart() {
        // HTTP 200 with a well-formed envelope and no rows is what a wrong
        // facet, a wrong route and a series that does not exist all look like.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/seriesid/PET.NOSUCH.D"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "response": { "total": 0, "data": [] }
            })))
            .mount(&server)
            .await;

        let err = get_series(&keyed_state_for(&server), "PET.NOSUCH.D", None, None)
            .await
            .expect_err("no rows is not a series");

        assert_eq!(err.code, codes::NOT_FOUND);
        assert!(err.message.contains("PET.NOSUCH.D"));
        assert!(err.hint.expect("a hint").contains("ECOS"));
    }

    #[tokio::test]
    async fn an_empty_answer_that_explains_itself_says_the_eias_own_words() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/petroleum/nosuch/data/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "response": { "data": [] },
                "error": { "code": "ROUTE_NOT_FOUND", "message": "invalid route" }
            })))
            .mount(&server)
            .await;

        let err = get_series(&keyed_state_for(&server), "petroleum/nosuch", None, None)
            .await
            .expect_err("no rows is not a series");

        assert_eq!(err.code, codes::NOT_FOUND);
        assert_eq!(
            err.hint.expect("the EIA said why"),
            "ROUTE_NOT_FOUND: invalid route"
        );
    }

    #[tokio::test]
    async fn a_row_the_terminal_cannot_date_is_dropped_rather_than_guessed_at() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/seriesid/PET.RWTC.D"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "response": { "data": [
                    { "period": "2026-08-12", "value": 63.4 },
                    { "period": "not a period", "value": 99.9 },
                    { "value": 99.9 }
                ]}
            })))
            .mount(&server)
            .await;

        let answer = get_series(&keyed_state_for(&server), "PET.RWTC.D", None, None)
            .await
            .expect("the readable row still charts");
        assert_eq!(rows(&answer.observations), [("2026-08-12", Some(63.4))]);
    }

    #[tokio::test]
    async fn an_uncurated_id_charts_under_the_publishers_own_words() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/seriesid/PET.WGTSTUS1.W"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "response": { "data": [{ "period": "2026-08-07", "value": 219_400 }]}
            })))
            .mount(&server)
            .await;

        let answer = get_series(&keyed_state_for(&server), "PET.WGTSTUS1.W", None, None)
            .await
            .expect("an uncurated series still charts");

        // Nothing states a title or units, so the panel heads itself with the
        // id rather than with an invented sentence.
        assert_eq!(answer.series.title, "PET.WGTSTUS1.W");
        assert_eq!(answer.series.units, "");
        assert_eq!(answer.series.frequency, "");
    }

    #[tokio::test]
    async fn a_curated_id_falls_back_to_the_curated_words() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/seriesid/NG.RNGWHHD.D"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "response": { "data": [{ "period": "2026-08-12", "value": 3.05 }]}
            })))
            .mount(&server)
            .await;

        let answer = get_series(&keyed_state_for(&server), "NG.RNGWHHD.D", None, None)
            .await
            .expect("the series resolves");

        assert_eq!(answer.series.title, "Henry Hub natural gas spot price");
        assert_eq!(answer.series.units, "Dollars per million Btu");
        assert_eq!(answer.series.frequency, "Daily");
    }

    #[tokio::test]
    async fn sends_the_key_the_range_and_the_ordering_the_module_decided_on() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/seriesid/PET.RWTC.D"))
            .and(query_param("api_key", KEY))
            .and(query_param("start", "2020-01-01"))
            .and(query_param("end", "2020-12-31"))
            .and(query_param("sort[0][direction]", "desc"))
            .and(query_param("length", "5000"))
            .respond_with(ResponseTemplate::new(200).set_body_json(gasoline_body()))
            .expect(1)
            .mount(&server)
            .await;

        get_series(
            &keyed_state_for(&server),
            "PET.RWTC.D",
            Some("2020-01-01"),
            Some("2020-12-31"),
        )
        .await
        .expect("the series resolves");

        let sent = server.received_requests().await.expect("one request");
        assert_eq!(sent.len(), 1);
        assert!(url_of(&sent[0]).contains("data%5B0%5D=value"));
    }

    #[tokio::test]
    async fn without_a_key_nothing_is_sent_at_all() {
        // The one possible answer is a 403, and it is knowable before the
        // socket opens.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(gasoline_body()))
            .expect(0)
            .mount(&server)
            .await;

        let err = get_series(&keyless_state_for(&server), "PET.RWTC.D", None, None)
            .await
            .expect_err("no key is no chart");

        assert_eq!(err.code, codes::NOT_CONFIGURED);
        assert!(err.hint.expect("a hint").contains("EIA_API_KEY"));
    }

    #[tokio::test]
    async fn an_empty_id_is_refused_before_a_key_is_even_looked_for() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(gasoline_body()))
            .expect(0)
            .mount(&server)
            .await;

        let err = get_series(&keyed_state_for(&server), "   ", None, None)
            .await
            .expect_err("nothing is not an id");
        assert_eq!(err.code, codes::BAD_REQUEST);
        assert!(err.hint.expect("a hint").contains("PET.RWTC.D"));
    }

    #[tokio::test]
    async fn a_rejected_key_is_named_as_one_rather_than_as_a_bad_day_at_the_eia() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/seriesid/PET.RWTC.D"))
            .respond_with(
                ResponseTemplate::new(403)
                    .set_body_raw(KEY_MISSING_JSON, "application/json")
                    .append_header("content-type", "application/json"),
            )
            .mount(&server)
            .await;

        let err = get_series(&keyed_state_for(&server), "PET.RWTC.D", None, None)
            .await
            .expect_err("a 403 is not a series");

        assert_eq!(err.code, codes::BAD_CREDENTIALS);
        assert_eq!(err.status, Some(403));
        assert!(err.message.contains("PET.RWTC.D"));
        assert!(err.hint.expect("a hint").contains(REGISTER_URL));
    }

    #[tokio::test]
    async fn the_assembled_series_is_cached_rather_than_re_asked() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/seriesid/PET.RWTC.D"))
            .respond_with(ResponseTemplate::new(200).set_body_json(gasoline_body()))
            .expect(1)
            .mount(&server)
            .await;

        let state = keyed_state_for(&server);
        let first = get_series(&state, "PET.RWTC.D", None, None)
            .await
            .expect("the series resolves");
        let second = get_series(&state, "PET.RWTC.D", None, None)
            .await
            .expect("the series resolves");
        assert_eq!(first.observations.len(), second.observations.len());
    }

    #[tokio::test]
    async fn a_failure_is_not_cached_as_though_it_were_an_answer() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/seriesid/PET.RWTC.D"))
            // Both attempts: a 500 is retryable, so one mock answer would let
            // the retry fall through to the healthy one below and never
            // produce the failure this test is about.
            .respond_with(ResponseTemplate::new(500))
            .up_to_n_times(2)
            .expect(2)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path_matcher("/seriesid/PET.RWTC.D"))
            .respond_with(ResponseTemplate::new(200).set_body_json(gasoline_body()))
            .mount(&server)
            .await;

        let state = keyed_state_for(&server);
        get_series(&state, "PET.RWTC.D", None, None)
            .await
            .expect_err("the EIA was down");
        let answer = get_series(&state, "PET.RWTC.D", None, None)
            .await
            .expect("the second ask gets through");
        assert_eq!(answer.observations.len(), 2);
    }

    #[tokio::test]
    async fn two_ranges_of_one_series_are_two_cache_entries() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/seriesid/PET.RWTC.D"))
            .respond_with(ResponseTemplate::new(200).set_body_json(gasoline_body()))
            .expect(2)
            .mount(&server)
            .await;

        let state = keyed_state_for(&server);
        get_series(&state, "PET.RWTC.D", Some("2020-01-01"), None)
            .await
            .expect("the series resolves");
        get_series(&state, "PET.RWTC.D", Some("2021-01-01"), None)
            .await
            .expect("the series resolves");
    }

    /* ---------------------------------------------------------------- search */

    #[tokio::test]
    async fn search_finds_the_print_a_market_settles_on() {
        let state = AppState::new(Config::default());
        let results = search(&state, "gasoline", 10)
            .await
            .expect("the catalogue answers");

        assert_eq!(results[0].id, "PET.EMM_EPMR_PTE_NUS_DPG.W");
        assert_eq!(results[0].provider, DataSource::Eia);
        assert_eq!(results[0].units.as_deref(), Some("Dollars per gallon"));
        assert_eq!(results[0].frequency.as_deref(), Some("Weekly"));
        // Nothing was asked of the EIA, so no arm of it answered.
        assert_eq!(results[0].source, None);
    }

    #[tokio::test]
    async fn search_matches_a_keyword_that_is_in_no_title() {
        // Nothing in the catalogue is titled "pump", and the price a reader
        // means when they say it is the weekly retail gasoline print.
        let state = AppState::new(Config::default());
        let results = search(&state, "pump", 10)
            .await
            .expect("the catalogue answers");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "PET.EMM_EPMR_PTE_NUS_DPG.W");
    }

    #[tokio::test]
    async fn search_ranks_a_title_match_above_a_keyword_match() {
        // Both stocks series carry `spr` — one in its keywords and one in the
        // title "excluding SPR" — so the one that names it wins the top row.
        let state = AppState::new(Config::default());
        let results = search(&state, "spr", 10)
            .await
            .expect("the catalogue answers");
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].id, "PET.WCESTUS1.W");
    }

    #[tokio::test]
    async fn search_requires_every_term_and_honours_the_limit() {
        let state = AppState::new(Config::default());
        let both = search(&state, "natural gas", 10)
            .await
            .expect("the catalogue answers");
        assert!(both.len() >= 2);
        assert!(both.iter().all(|row| row.id.starts_with("NG.")));

        assert_eq!(
            search(&state, "natural gas", 1)
                .await
                .expect("the catalogue answers")
                .len(),
            1
        );
        // `eia gold` is not every EIA series.
        assert!(search(&state, "crude gold", 10)
            .await
            .expect("the catalogue answers")
            .is_empty());
    }

    #[tokio::test]
    async fn search_for_nothing_matches_nothing() {
        let state = AppState::new(Config::default());
        for query in ["", "   ", "zzzz"] {
            assert!(
                search(&state, query, 10)
                    .await
                    .expect("the catalogue answers")
                    .is_empty(),
                "{query:?} matched something"
            );
        }
    }

    #[test]
    fn every_curated_id_is_one_this_module_would_route() {
        // A catalogue row a reader cannot then chart would be worse than no
        // row. Each of these is a v1-style id, so each takes the compatibility
        // route rather than being read as a route path.
        for entry in CATALOGUE.iter() {
            assert!(
                !is_native(split_route(&entry.id).0),
                "{} would be read as a route path",
                entry.id
            );
            assert!(catalogue_entry(&entry.id).is_some());
        }
    }
}
