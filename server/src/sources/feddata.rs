//! Federal Reserve Board — the Data Download Program at federalreserve.gov.
//!
//! This is the Board's own publication, not FRED's mirror of it: H.15 constant
//! maturity yields, the H.4.1 balance sheet, H.6 money stock, G.17 industrial
//! production. Where FRED re-publishes these under its own ids, the DDP is
//! where they appear first and how the Board itself words them.
//!
//! The awkward part is identity. The DDP addresses data by an MD5 hash of a
//! *selection* — `bf17364827e38702b42a58cf8eaa3f78` is "the Treasury constant
//! maturity package of H.15" — and there is no endpoint that maps a series to
//! its package. Asking a reader to type a hash would be absurd, so this module
//! builds the mapping instead: the release's chooser page lists its packages,
//! and one one-observation request per package reveals which series each holds.
//! That index is cached for the session, after which `fed:H15/RIFLGFCY10_N.B`
//! resolves in a single request. An id is therefore `RELEASE/SERIES`, both as
//! the Board writes them.
//!
//! Everything below was checked against `www.federalreserve.gov` from this
//! container while the port was written, because every way this upstream is
//! wrong at you is an HTTP 200.
//!
//! **A zero-byte body is how it says no.** Not a 4xx, not an empty CSV, not a
//! header block with nothing under it — no bytes at all, served as
//! `content-type: text/html` with status 200. Three separate mistakes produce
//! it, and none of them is distinguishable from the others on the wire:
//!
//! ```text
//! $ base=…/Output.aspx?rel=H15&series=bf17364827e38702b42a58cf8eaa3f78&filetype=csv…
//! $ curl -s -o /dev/null -w '%{http_code} %{size_download}\n' "$base&lastobs=&from=08/01/2026&to="
//! 200 0        # an open-ended range: one bound is not enough
//! $ curl -s -o /dev/null -w '%{http_code} %{size_download}\n' "$base…&series=000…000&lastobs=1"
//! 200 0        # a package hash the DDP does not hold
//! $ curl -s -o /dev/null -w '%{http_code} %{size_download}\n' "…rel=G17&series=6752…4175&lastobs=200"
//! 200 0        # a selection too large to build — 150 observations of that
//!              # package is 1.57 MB and answers; 200 of it answers nothing
//! ```
//!
//! Left alone that becomes an empty chart, which reads as "the Board publishes
//! nothing here" rather than as "we asked the wrong question". So an empty body
//! is turned into a described failure by [`read_package`], and the two causes
//! this module can avoid it never commits: [`package_url`] cannot express an
//! open-ended range, filling in the missing bound instead.
//!
//! **`lastobs` is three different parameters.** Absent means the full history;
//! present and *empty* is what makes a `from`/`to` range count; and `lastobs=0`
//! returns the six header rows with no observations beneath them, which parses
//! perfectly into a series with nothing in it. The chooser page's own forms send
//! the empty spelling alongside a range, and so does this module.
//!
//! **A monthly package is dated differently from a daily one.** H.15's daily
//! rows read `2026-08-12` and its monthly averages read `2026-08`, in the same
//! column of the same file format; H.6 weekly rows read `2026-08-11` and G.17's
//! annual package reads `2026`. Matching only the daily form treats every
//! monthly row as a header and returns a package with no observations at all —
//! a well-formed empty answer, and the failure this whole module is shaped
//! around. [`crate::period::period_to_date`] reads all of them, and this module
//! asks it rather than repeating the rule.
//!
//! **Series descriptions contain commas inside quotes.** "Market yield on U.S.
//! Treasury securities at 1-month constant maturity, quoted on investment
//! basis" is one cell containing one comma; splitting the line on commas shifts
//! every later column by one, which does not fail loudly — it pairs a date with
//! the neighbouring series' value and charts it under this one's name. The `csv`
//! crate reads the file, and it also handles the CRLF line endings and the
//! absent trailing newline the DDP actually sends.
//!
//! **A missing reading is `ND` or an empty cell, never zero.** New Year's Day
//! 2026 is published as `2026-01-01,ND,ND,ND,…` — the row exists, the reading
//! does not — and the discontinued IOER and IORR rates are published as empty
//! cells beside a live IORB. Both are `None`. A zero would assert that the
//! Board took the reading and it came out at nought, which on a yield curve is
//! a figure somebody trades off.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use futures::future::join_all;
use terminal_core::dataset::DataSource;
use terminal_core::types::{DataObservation, DataSearchResult, DataSeries, DataSeriesResponse};
use time::OffsetDateTime;

use crate::app::AppState;
use crate::cache::ttl;
use crate::css;
use crate::error::{codes, Result, UpstreamError};
use crate::http::FetchOptions;
use crate::period::period_to_date;
use crate::scrape;
use crate::sources::sdmx::{self, SdmxCatalogueEntry};

/// The chooser page is 80–300 KB of ASP.NET and the probes are 2 KB each; both
/// are quick, and one retry covers a dropped connection without turning a slow
/// index build into a very slow one.
const INDEX_TIMEOUT: Duration = Duration::from_secs(30);

/// A full package history is a megabyte and the Board's own servers take their
/// time building one, so the download gets three times the index's budget.
const SERIES_TIMEOUT: Duration = Duration::from_secs(60);

/// Attempts after the first. A refusal here arrives as an HTTP 200 with no body
/// and is therefore never retried by the transport; a retry only ever costs
/// something on a genuine transport failure.
const RETRIES: u32 = 1;

/// The earliest date the DDP accepts as the open end of a range.
///
/// A `from` earlier than the release's own first observation is fine — H.15
/// answers `from=01/01/1900` with a first row dated 1962-01-02 — which is what
/// makes filling in an unstated bound safe rather than a guess at when the
/// series began.
const EARLIEST: &str = "01/01/1900";

/// The releases the DDP publishes, with the Board's own names.
///
/// Used for the `notes` line on a series and for the hint on an unknown
/// release. It is not a gate: any `rel` the DDP answers a chooser page for
/// works, this is only what an error message can offer a reader who guessed.
const RELEASES: &[(&str, &str)] = &[
    ("H15", "H.15 Selected Interest Rates"),
    ("H41", "H.4.1 Factors Affecting Reserve Balances"),
    ("H6", "H.6 Money Stock Measures"),
    ("H8", "H.8 Assets and Liabilities of Commercial Banks"),
    ("H10", "H.10 Foreign Exchange Rates"),
    ("H3", "H.3 Aggregate Reserves of Depository Institutions"),
    ("G17", "G.17 Industrial Production and Capacity Utilization"),
    ("G19", "G.19 Consumer Credit"),
    ("G20", "G.20 Finance Companies"),
    ("Z1", "Z.1 Financial Accounts of the United States"),
    ("CP", "Commercial Paper"),
    ("PRATES", "Policy Rates"),
    ("SLOOS", "Senior Loan Officer Opinion Survey"),
    (
        "DSR",
        "Household Debt Service and Financial Obligations Ratios",
    ),
    ("CHGDEL", "Charge-Off and Delinquency Rates"),
    ("E2", "E.2 Survey of Terms of Business Lending"),
    ("FOR", "Household Financial Obligations"),
];

/// The Board's own name for a release, where this module knows one.
fn release_name(release: &str) -> Option<&'static str> {
    RELEASES
        .iter()
        .find(|(id, _)| *id == release)
        .map(|(_, name)| *name)
}

/* --------------------------------------------------------------------- id */

/// The two halves of a `RELEASE/SERIES` reference.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FedId {
    /// Upper-cased and undotted: a reader who types `h.15` means `H15`.
    pub release: String,
    /// Left exactly as typed. The Board's series names carry case, dots,
    /// underscores and — on H.10 — a `$`, and none of that is ours to tidy.
    pub series: String,
}

/// Split `H15/RIFLGFCY10_N.B` into its release and its series.
///
/// The release is upper-cased and stripped of the dots the Board's own prose
/// uses (`H.15`, `H.4.1`) because the `rel` parameter takes neither; the series
/// is left alone, since `RXI$US_N.B.EU` is a name and not a path. A reference
/// with no slash names a release and no series, which [`get_series`] answers by
/// listing what that release holds rather than by guessing at one.
#[must_use]
pub fn split_fed_id(id: &str) -> FedId {
    let trimmed = id.trim().trim_matches('/');
    match trimmed.split_once('/') {
        Some((release, series)) => FedId {
            release: release.to_uppercase().replace('.', ""),
            series: series.trim().to_owned(),
        },
        None => FedId {
            release: trimmed.to_uppercase().replace('.', ""),
            series: String::new(),
        },
    }
}

/// `2015-06-30` → `06/30/2015`, the only date form the DDP accepts.
///
/// `None` for anything else, which [`package_url`] turns into the open bound's
/// default rather than into a malformed parameter — the DDP answers a date it
/// cannot read with the same zero bytes it answers everything else with.
#[must_use]
pub fn to_ddp_date(iso: &str) -> Option<String> {
    let trimmed = iso.trim();
    let bytes = trimmed.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return None;
    }
    if !trimmed
        .bytes()
        .enumerate()
        .all(|(at, byte)| matches!(at, 4 | 7) || byte.is_ascii_digit())
    {
        return None;
    }
    Some(format!(
        "{}/{}/{}",
        &trimmed[5..7],
        &trimmed[8..10],
        &trimmed[0..4]
    ))
}

/* -------------------------------------------------------------------- csv */

/// One column of a DDP package: a series and everything the header says about
/// it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FedColumn {
    /// The Board's short name, e.g. `RIFLGFCY10_N.B` — the half a reader types.
    pub name: String,
    pub description: String,
    /// The unit as this module states it; see [`unit_of`] for what is done to
    /// the Board's spelling of it and why.
    pub unit: String,
    /// The scale the figures are published on: 1 for a rate, 1e6 for H.4.1's
    /// millions, 1e9 for H.6's billions. Read but never applied — see
    /// [`unit_of`].
    pub multiplier: f64,
    pub currency: String,
    /// The fully-qualified `H15/H15/RIFLGFCY10_N.B` form, which is the Board's
    /// own key and not something to rebuild from the two halves.
    pub unique_id: String,
}

/// One dated row of a package. `values` aligns with `columns` by index.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FedRow {
    pub date: String,
    pub values: Vec<Option<f64>>,
}

/// A `layout=seriescolumn&label=include` package, read.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FedPackage {
    pub columns: Vec<FedColumn>,
    /// In file order, oldest first, dated `YYYY-MM-DD`.
    pub rows: Vec<FedRow>,
}

/// Normalise a header row's first cell into the key it is looked up by.
///
/// The Board's labels are not stable to the byte. `"Unit:"` carries its colon
/// and `"Time Period"` does not, and — reproducibly, three captures apart —
/// H.4.1 writes `"Unique Identifier:"` for `lastobs=1` and
/// `"Unique Identifier: "` with a trailing space for `lastobs=2`. Since
/// `lastobs=1` is exactly the request [`release_index`] sends, keying on the
/// literal would leave every H.4.1 series unnameable on the one request that
/// names them. Trailing punctuation and space come off, and the rest is
/// lower-cased.
fn header_key(cell: &str) -> String {
    cell.trim_end_matches(|c: char| c == ':' || c.is_whitespace())
        .trim()
        .to_lowercase()
}

/// Collapse runs of whitespace to one space and trim, which the Board's own
/// descriptions need: "at 1-month   constant maturity" arrives with three.
fn collapse(text: &str) -> String {
    scrape::collapse_ws(text)
}

/// Parse a `layout=seriescolumn&label=include` package.
///
/// The file is six labelled header rows — description, unit, multiplier,
/// currency, unique identifier, short name — followed by dated observations.
/// The headers are keyed by their *first cell* rather than by row number,
/// because nothing guarantees the Board emits every one of them for every
/// release, and reading by position would then shift the short names into the
/// unique-identifier slot and leave every series in the package unnameable. A
/// row is an observation when its first cell is a period
/// [`crate::period::period_to_date`] recognises, and a header otherwise; that
/// one test is what lets a monthly package and a daily one share this reader.
///
/// An empty document parses to an empty package rather than to an error. The
/// DDP's zero-byte answer is a failure worth describing, but describing it is
/// [`read_package`]'s job — a parser that threw here could not be used to probe
/// packages speculatively.
#[must_use]
pub fn parse_fed_package(csv: &str) -> FedPackage {
    let mut header: HashMap<String, Vec<String>> = HashMap::new();
    let mut rows: Vec<FedRow> = Vec::new();

    let mut reader = csv::ReaderBuilder::new()
        .has_headers(false)
        .flexible(true)
        .from_reader(csv.as_bytes());

    for record in reader.records() {
        // A row the reader cannot make sense of is dropped rather than failing
        // the file: one malformed line late in a megabyte of history must not
        // cost the reader the other forty years.
        let Ok(record) = record else { continue };
        let Some(first) = record.get(0) else { continue };

        if let Some(date) = period_to_date(first) {
            rows.push(FedRow {
                date,
                values: record
                    .iter()
                    .skip(1)
                    .map(crate::sources::sdmx::number_cell)
                    .collect(),
            });
            continue;
        }

        let key = header_key(first);
        if !key.is_empty() {
            header.insert(
                key,
                record.iter().skip(1).map(str::to_owned).collect::<Vec<_>>(),
            );
        }
    }

    let cell = |label: &str, at: usize| -> String {
        header
            .get(label)
            .and_then(|row| row.get(at))
            .map(|value| value.trim().to_owned())
            .unwrap_or_default()
    };

    // The short names live in the row labelled with the date column's own
    // heading, "Time Period" — the Board puts the series' names where a reader
    // would expect the column titles, which is exactly what they are.
    let names = header.get("time period").cloned().unwrap_or_default();

    let columns: Vec<FedColumn> = names
        .iter()
        .enumerate()
        .map(|(at, name)| {
            let multiplier = sdmx::number_cell(&cell("multiplier", at)).unwrap_or(1.0);
            let currency = cell("currency", at);
            FedColumn {
                name: name.trim().to_owned(),
                description: collapse(&cell("series description", at)),
                unit: unit_of(&cell("unit", at), multiplier, &currency),
                multiplier,
                currency,
                unique_id: cell("unique identifier", at),
            }
        })
        .collect();

    FedPackage { columns, rows }
}

/// The unit a panel should print, from the three columns that state one.
///
/// The Board writes a unit as `Percent:_Per_Year` — underscores for spaces and a
/// colon between the measure and its qualifier. Both are punctuation; the part
/// before the colon is the measure itself, so neither half is dropped. "Per
/// Year" alone does not say what is being measured.
///
/// The awkward case is money. Every currency series in the DDP — H.4.1, H.6,
/// H.8, H.10, G.19 — states its unit as the bare word `Currency` and puts the
/// information a reader needs in the other two columns: `Currency: USD` and
/// `Multiplier: 1e+09`. A panel labelled "Currency" against a figure of
/// `19821.3` tells nobody that M1 is being quoted in billions of dollars, so
/// the three columns are read together into "Billions of USD". This is a
/// deliberate divergence from the TypeScript, which printed the Board's bare
/// word.
///
/// The figures themselves are left exactly as published. Multiplying them out
/// would restate the Board's own numbers, and every mirror a reader might check
/// this against — FRED's H.4.1 series among them — quotes the published scale.
#[must_use]
pub fn unit_of(raw: &str, multiplier: f64, currency: &str) -> String {
    let spelled = collapse(&raw.replace(['_', ':'], " "));

    if !spelled.eq_ignore_ascii_case("currency") {
        return spelled;
    }

    let code = currency.trim();
    // `NA` is the Board's "this is not money", which it writes in the currency
    // column of every rate and index package.
    if code.is_empty() || code.eq_ignore_ascii_case("NA") {
        return spelled;
    }

    match scale_word(multiplier) {
        Some(scale) => format!("{scale} of {code}"),
        None => code.to_owned(),
    }
}

/// The English for a multiplier, or `None` when the figures need no scaling.
///
/// Only the three scales the DDP actually publishes are named. An unrecognised
/// multiplier falls back to naming the currency alone, which is incomplete but
/// true, rather than to a scale word invented from an exponent nobody has seen.
fn scale_word(multiplier: f64) -> Option<&'static str> {
    match multiplier as i64 {
        1_000 => Some("Thousands"),
        1_000_000 => Some("Millions"),
        1_000_000_000 => Some("Billions"),
        _ => None,
    }
}

/* --------------------------------------------------------------- packages */

/// A preformatted package offered on a release's chooser page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageRef {
    /// The MD5 of the selection — the only thing `Output.aspx` answers to.
    pub hash: String,
    /// The Board's wording for it, or the hash when the wording moves.
    pub label: String,
}

/// The packages a release offers.
///
/// Scraped, because the DDP has no machine-readable index of itself. Every
/// package is an element carrying `rel=…&series=<hash>…` in its `value`, so the
/// hash comes from there — it is a URL, and therefore stable — and the label
/// from the markup beside it.
///
/// Two markups, because the DDP is inconsistent about it, and both were seen
/// live: H.15, H.4.1, H.6, H.10 and PRATES render their packages as radio
/// `<input>`s, while G.17 uses a `<select>` whose `<option>`s carry their own
/// label. Matching only the first silently finds nothing for the second — and
/// "no packages" is indistinguishable from "no such release" unless both are
/// handled.
///
/// Where the label comes from is a deliberate divergence from the TypeScript,
/// which read an `<input>`'s label from its parent element's text. On the live
/// page all four of H.15's radios share one `<span id="FreqRequest">` parent,
/// so that reads the whole list for every one of them and — after the `[csv, …]`
/// size annotation is cut off — labels all four packages "Treasury Constant
/// Maturities". The Board wires each input to its own `<label for="…">`, which
/// is where the wording actually is, so that is read first and the parent's text
/// is only the fallback.
#[must_use]
pub fn parse_chooser_page(html: &str) -> Vec<PackageRef> {
    let doc = scrape::parse_document(html);
    let mut seen: Vec<String> = Vec::new();
    let mut packages: Vec<PackageRef> = Vec::new();

    for element in scrape::select_all(&doc, css!("input[value], option[value]")) {
        let value = scrape::attr(element, "value").unwrap_or_default();
        let Some(hash) = package_hash(value) else {
            continue;
        };
        if seen.iter().any(|known| known == hash) {
            continue;
        }
        seen.push(hash.to_owned());

        let text = if element.value().name() == "option" {
            // An `<option>` holds its own label.
            scrape::text_of(element)
        } else {
            label_for(&doc, element)
        };

        // Every label ends with a `[csv, Last 52 Obs, 1.9 KB ]` size annotation
        // that describes the download rather than the data.
        let label = text.split('[').next().unwrap_or_default().trim().to_owned();

        packages.push(PackageRef {
            hash: hash.to_owned(),
            label: if label.is_empty() {
                hash.to_owned()
            } else {
                label
            },
        });
    }

    packages
}

/// The 32 hex digits after `series=` in a chooser control's value, if any.
///
/// The width is checked because the same page carries ASP.NET's `__VIEWSTATE`
/// and `__EVENTVALIDATION` hidden inputs, whose base64 payloads can contain the
/// letters `series=` by coincidence and never contain a package.
fn package_hash(value: &str) -> Option<&str> {
    let after = value.split("series=").nth(1)?;
    let hash = after.get(..32)?;
    hash.bytes()
        .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        .then_some(hash)
}

/// The text of the `<label for="…">` bound to this input, or its parent's text.
fn label_for(doc: &scraper::Html, element: scraper::ElementRef<'_>) -> String {
    let bound = scrape::attr(element, "id")
        .and_then(|id| scrape::selector(&format!(r#"label[for="{id}"]"#)))
        .and_then(|selector| scrape::select_one(doc, &selector))
        .map(scrape::text_of);

    bound
        .or_else(|| scrape::parent_element(element).map(scrape::text_of))
        .unwrap_or_default()
}

/// Which observations of a package to ask for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Window {
    /// The most recent `n` observations. One of them is what an index probe
    /// costs; `0` is not this — it returns the header block alone.
    LastObs(u32),
    /// A closed date range, or the whole history when neither bound is stated.
    /// Deliberately not expressible as a half-open range: see [`package_url`].
    Range {
        from: Option<String>,
        to: Option<String>,
    },
}

/// The URL for one package.
///
/// The DDP is particular in ways that all fail the same silent way — HTTP 200
/// and a zero-byte body:
///
/// * `lastobs` must be present and *empty* for a `from`/`to` range to count,
///   and absent entirely for the full history. `lastobs=0` is neither: it
///   returns the six header rows and no observations, which parses into a
///   perfectly well-formed series containing nothing.
/// * a range needs *both* bounds. An open-ended `to=` returns nothing at all
///   rather than "up to today", so this signature cannot express one — an
///   unstated side is filled in with [`EARLIEST`] or with today.
#[must_use]
pub fn package_url(base: &str, release: &str, hash: &str, window: &Window) -> String {
    let mut url = format!(
        "{}/Output.aspx?rel={}&series={}&filetype=csv&label=include&layout=seriescolumn&type=package",
        base.trim_end_matches('/'),
        urlencoding::encode(release),
        urlencoding::encode(hash),
    );

    match window {
        Window::LastObs(count) => {
            url.push_str(&format!("&lastobs={count}"));
        }
        Window::Range { from, to } => {
            if from.is_none() && to.is_none() {
                return url;
            }
            let from = from.clone().unwrap_or_else(|| EARLIEST.to_owned());
            let to = to.clone().unwrap_or_else(today_ddp);
            url.push_str(&format!(
                "&lastobs=&from={}&to={}",
                urlencoding::encode(&from),
                urlencoding::encode(&to),
            ));
        }
    }

    url
}

/// Today, in the DDP's `MM/DD/YYYY`. A range's upper bound may run past the
/// last observation without harm; only an *absent* one is refused.
fn today_ddp() -> String {
    let today = OffsetDateTime::now_utc().date();
    format!(
        "{:02}/{:02}/{:04}",
        u8::from(today.month()),
        today.day(),
        today.year()
    )
}

/// Fetch and parse one package, turning the zero-byte answer into a failure a
/// reader can act on.
///
/// This is the one place that knows what no bytes means. All three causes look
/// identical on the wire, so the message names the shape of the mistake and the
/// hint names the two remedies that are actually within reach.
async fn read_package(
    state: &AppState,
    url: &str,
    what: &str,
    timeout: Duration,
) -> Result<FedPackage> {
    let body = state
        .http()
        .fetch_text(
            url,
            FetchOptions::new()
                .timeout(timeout)
                .retries(RETRIES)
                .browser_headers(false),
        )
        .await?;

    if body.trim().is_empty() {
        return Err(UpstreamError::new(
            format!(
                "The Federal Reserve returned an empty download for {what} \
                 (HTTP 200, no bytes)"
            ),
            codes::EMPTY_UPSTREAM,
        )
        .with_hint(
            "The Data Download Program answers a request it will not service with no body \
             at all. It does that for a package it does not hold and for a selection too \
             large to build — 150 observations of the G.17 monthly package is 1.6 MB and \
             answers, 200 of it answers nothing. Ask for a narrower date range.",
        ));
    }

    Ok(parse_fed_package(&body))
}

/* ------------------------------------------------------------------ index */

/// Where one series lives, and what the package's header said about it.
#[derive(Debug, Clone)]
pub struct IndexEntry {
    pub hash: String,
    pub column: FedColumn,
}

/// Every series a release publishes, and which package holds it.
///
/// Order is the Board's own — packages in chooser-page order, columns in file
/// order within each — because that is the order the error hints list series
/// in, and "the first few series in H.15" is a more useful prompt than "the
/// first few alphabetically".
#[derive(Debug, Clone, Default)]
pub struct ReleaseIndex {
    entries: Vec<IndexEntry>,
    /// Upper-cased name → position in `entries`.
    by_name: HashMap<String, usize>,
}

impl ReleaseIndex {
    /// Record a column, unless its name is already known.
    ///
    /// First package wins: the Board offers the same series at several
    /// frequencies under distinct names, but it also repeats a name across
    /// overlapping packages — `RESH4E_N.WW` is in four of H.4.1's eight — and
    /// the first package the chooser lists is the primary one.
    fn insert(&mut self, entry: IndexEntry) {
        if entry.column.name.is_empty() {
            return;
        }
        let key = entry.column.name.to_uppercase();
        if self.by_name.contains_key(&key) {
            return;
        }
        self.by_name.insert(key, self.entries.len());
        self.entries.push(entry);
    }

    /// Look a series up, however the reader cased it. The Board's names are
    /// upper-case throughout, so nothing is lost by matching that way and a
    /// reader who typed `riflgfcy10_n.b` gets their chart.
    #[must_use]
    pub fn get(&self, series: &str) -> Option<&IndexEntry> {
        self.by_name
            .get(&series.trim().to_uppercase())
            .and_then(|at| self.entries.get(*at))
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Series names in the Board's own order, for an error a reader can use.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(|entry| entry.column.name.as_str())
    }
}

/// Build — or reuse — the series index for one release.
///
/// Two round trips deep: the chooser page lists the packages, then one
/// one-observation request per package reveals the names inside it. One
/// observation is enough because the names are in the header block, and it
/// keeps the index cheap — H.15's full history is a megabyte, its header is two
/// kilobytes.
///
/// The probes run together rather than in sequence. The DDP throttles nobody
/// and G.17 offers thirteen packages; thirteen sequential 30-second budgets is
/// a minute of somebody waiting at a prompt for an index that is cached for the
/// session afterwards.
///
/// A probe that fails contributes nothing instead of failing the index. A
/// package the DDP has stopped holding answers with no bytes, and losing every
/// series in a release because one of its packages was retired would be a much
/// worse trade than losing the series in that one package.
pub async fn release_index(state: &AppState, release: &str) -> Result<Arc<ReleaseIndex>> {
    let key = format!("fed:index:{release}");

    state
        .cache()
        .cached(&key, ttl::CATALOGUE, || async {
            let base = state.config().fed_ddp_base.trim_end_matches('/').to_owned();
            let html = state
                .http()
                .fetch_text(
                    &format!("{base}/Choose.aspx?rel={}", urlencoding::encode(release)),
                    FetchOptions::new().timeout(INDEX_TIMEOUT).retries(RETRIES),
                )
                .await?;

            let packages = parse_chooser_page(&html);
            if packages.is_empty() {
                let known: Vec<&str> = RELEASES.iter().take(8).map(|(id, _)| *id).collect();
                return Err(UpstreamError::not_found(format!(
                    "The Federal Reserve publishes no release called {release}"
                ))
                .with_hint(format!(
                    "Releases are {} and a few more; an id is the release, a slash, then the \
                     series, e.g. H15/RIFLGFCY10_N.B.",
                    known.join(", ")
                )));
            }

            let probes = packages.iter().map(|package| {
                let url = package_url(&base, release, &package.hash, &Window::LastObs(1));
                async move {
                    read_package(
                        state,
                        &url,
                        &format!("package {}", package.hash),
                        INDEX_TIMEOUT,
                    )
                    .await
                    .ok()
                }
            });

            let mut index = ReleaseIndex::default();
            for (package, parsed) in packages.iter().zip(join_all(probes).await) {
                for column in parsed.into_iter().flat_map(|p| p.columns) {
                    index.insert(IndexEntry {
                        hash: package.hash.clone(),
                        column,
                    });
                }
            }

            if index.is_empty() {
                // The chooser listed packages and not one of them yielded a
                // column. That is the Board having changed the download format
                // out from under this reader, not a reader asking for the wrong
                // release, and it is worth saying so rather than reporting an
                // empty release.
                return Err(UpstreamError::parse_failed(format!(
                    "{release} lists {} package(s) and none of them named a series",
                    packages.len()
                ))
                .with_hint(
                    "The Data Download Program's CSV header block is what names the series \
                     in a package; if it has changed shape, this reader needs updating.",
                ));
            }

            Ok(index)
        })
        .await
}

/* ----------------------------------------------------------------- series */

/// Frequency from the series name's suffix.
///
/// The Board encodes it there — `.B` business daily, `.M` monthly, `.WW`
/// Wednesday-weekly — and publishes it nowhere in the CSV, so the suffix is the
/// only way to label a panel without a second request. `None` where the suffix
/// says something else: G.17's names end `.S` and `.N`, which are seasonal
/// adjustment rather than frequency, and reading those as a frequency would put
/// a confident wrong word in the panel header.
#[must_use]
pub fn frequency_of(name: &str) -> Option<&'static str> {
    let suffix = name.rsplit('.').next().unwrap_or_default().to_uppercase();
    match suffix.as_str() {
        "B" => Some("Business daily"),
        "D" => Some("Daily"),
        "WF" => Some("Weekly (Friday)"),
        "WW" => Some("Weekly (Wednesday)"),
        "W" => Some("Weekly"),
        "M" => Some("Monthly"),
        "Q" => Some("Quarterly"),
        "A" => Some("Annual"),
        _ => None,
    }
}

/// The first few series of a release, for an error message.
fn examples(index: &ReleaseIndex, release: &str, qualified: bool) -> String {
    index
        .names()
        .take(if qualified { 5 } else { 4 })
        .map(|name| {
            if qualified {
                format!("{release}/{name}")
            } else {
                name.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// A series and its observations, from the Board's own download.
pub async fn get_series(
    state: &AppState,
    raw_id: &str,
    start: Option<&str>,
    end: Option<&str>,
) -> Result<DataSeriesResponse> {
    let FedId { release, series } = split_fed_id(raw_id);

    if release.is_empty() {
        return Err(UpstreamError::bad_request(format!(
            "\"{raw_id}\" does not name a Federal Reserve release"
        ))
        .with_hint("Ids look like H15/RIFLGFCY10_N.B — the release, a slash, then the series."));
    }

    let index = release_index(state, &release).await?;

    if series.is_empty() {
        return Err(UpstreamError::bad_request(format!(
            "{release} holds {} series — name one",
            index.len()
        ))
        .with_hint(format!(
            "e.g. {}. `ECOS <words> fed` searches them.",
            examples(&index, &release, true)
        )));
    }

    let Some(entry) = index.get(&series) else {
        return Err(
            UpstreamError::not_found(format!("{release} has no series called {series}")).with_hint(
                format!(
                    "Series in {release} look like {}.",
                    examples(&index, &release, false)
                ),
            ),
        );
    };

    let base = state.config().fed_ddp_base.trim_end_matches('/').to_owned();
    let cache_key = format!(
        "fed:series:{release}/{}:{}:{}",
        entry.column.name,
        start.unwrap_or_default(),
        end.unwrap_or_default()
    );

    let response = state
        .cache()
        .cached(&cache_key, ttl::FRED, || async {
            let window = Window::Range {
                from: start.and_then(to_ddp_date),
                to: end.and_then(to_ddp_date),
            };
            let url = package_url(&base, &release, &entry.hash, &window);
            let package =
                read_package(state, &url, &format!("{release}/{series}"), SERIES_TIMEOUT).await?;

            // The index said which package holds the series; the package says
            // which column. Looking the name up again rather than trusting the
            // index's position is what stops a package that gained a column
            // since the index was built from charting its neighbour's figures
            // under this series' name.
            let Some(at) = package
                .columns
                .iter()
                .position(|column| column.name == entry.column.name)
            else {
                return Err(UpstreamError::new(
                    format!("{release} no longer returns a column for {series}"),
                    codes::EMPTY_UPSTREAM,
                )
                .with_hint(
                    "The package that held it still downloads, so the Board has most likely \
                     renamed or retired the series. Check the release's own chooser page.",
                ));
            };

            let observations: Vec<DataObservation> = package
                .rows
                .iter()
                .map(|row| DataObservation {
                    date: row.date.clone(),
                    // A row shorter than the header — which the DDP has not been
                    // seen to send — reads as no reading, never as zero.
                    value: row.values.get(at).copied().flatten(),
                })
                .collect();

            if observations.is_empty() {
                // The download parsed and held only its header block. That is
                // what a range with no observations in it looks like, and it is
                // also what `lastobs=0` looks like, so the message names the
                // range rather than blaming the series.
                return Err(UpstreamError::new(
                    format!("The Federal Reserve returned no observations for {raw_id}"),
                    codes::EMPTY_UPSTREAM,
                )
                .with_hint(
                    "The package downloaded but held no rows in that window. Widen the date \
                     range, or drop it to ask for the full history.",
                ));
            }

            let column = &package.columns[at];

            Ok(DataSeriesResponse {
                series: DataSeries {
                    provider: DataSource::Fed,
                    // Answered under the Board's spelling of the name, not the
                    // reader's, so a cased-down id and a cased-up one open the
                    // same panel.
                    id: format!("{release}/{}", column.name),
                    title: if column.description.is_empty() {
                        column.name.clone()
                    } else {
                        column.description.clone()
                    },
                    units_short: column.unit.clone(),
                    units: column.unit.clone(),
                    frequency: frequency_of(&column.name)
                        .map(str::to_owned)
                        // The suffix is silent on G.17's names, where it states
                        // seasonal adjustment instead; the curated catalogue
                        // knows those, and a checked entry beats an empty field.
                        .or_else(|| {
                            catalogue_entry(&format!("{release}/{}", column.name))
                                .map(|entry| entry.frequency.clone())
                        })
                        .unwrap_or_default(),
                    // The Board states seasonal adjustment in prose inside the
                    // description ("M1; Not seasonally adjusted") and in no
                    // field of its own. Inferring one from the name's `_N` would
                    // put a claim in the panel that the download never made.
                    seasonal_adjustment: String::new(),
                    // Nor does the download carry a revision timestamp. An
                    // invented one would be read as a release time.
                    last_updated: String::new(),
                    observation_start: observations
                        .first()
                        .map(|o| o.date.clone())
                        .unwrap_or_default(),
                    observation_end: observations
                        .last()
                        .map(|o| o.date.clone())
                        .unwrap_or_default(),
                    notes: release_name(&release)
                        .map(|name| format!("Release: {name}"))
                        .unwrap_or_default(),
                    source: "ddp".to_owned(),
                    source_url: format!("{base}/Choose.aspx?rel={}", urlencoding::encode(&release)),
                },
                observations,
            })
        })
        .await?;

    Ok(Arc::unwrap_or_clone(response))
}

/* -------------------------------------------------------------- catalogue */

/// The Board's headline series, so `ECOS` can answer before you know a name.
///
/// Every id here was resolved against the live DDP while this was written: the
/// release's chooser page was fetched, each of its packages probed, and the name
/// found in the result. The list is what a search can offer, not a limit — any
/// `RELEASE/SERIES` the index resolves still charts.
static CATALOGUE: LazyLock<Vec<SdmxCatalogueEntry>> = LazyLock::new(|| {
    let yield_curve = |id: &str, title: &str, keywords: &str| {
        SdmxCatalogueEntry::new(id, title)
            .with_units("Percent Per Year")
            .with_frequency("Business daily")
            .with_keywords(keywords)
    };

    vec![
        yield_curve(
            "H15/RIFLGFCY10_N.B",
            "10-year Treasury constant maturity yield",
            "treasury yield 10y rates govvie h15",
        ),
        yield_curve(
            "H15/RIFLGFCY02_N.B",
            "2-year Treasury constant maturity yield",
            "treasury yield 2y rates govvie h15",
        ),
        yield_curve(
            "H15/RIFLGFCY30_N.B",
            "30-year Treasury constant maturity yield",
            "treasury yield 30y long bond rates h15",
        ),
        yield_curve(
            "H15/RIFLGFCM03_N.B",
            "3-month Treasury constant maturity yield",
            "treasury bill yield 3m rates h15",
        ),
        SdmxCatalogueEntry::new(
            "H15/RIFSPFF_N.M",
            "Effective federal funds rate, monthly average",
        )
        .with_units("Percent Per Year")
        .with_frequency("Monthly")
        .with_keywords("fed funds effective policy rate eff h15 fomc"),
        SdmxCatalogueEntry::new(
            "H15/RIFSPFF_N.WW",
            "Effective federal funds rate, weekly average",
        )
        .with_units("Percent Per Year")
        .with_frequency("Weekly (Wednesday)")
        .with_keywords("fed funds effective policy rate eff h15 fomc"),
        SdmxCatalogueEntry::new("H15/RIFSPBLP_N.M", "Bank prime loan rate, monthly average")
            .with_units("Percent Per Year")
            .with_frequency("Monthly")
            .with_keywords("prime rate lending h15"),
        SdmxCatalogueEntry::new(
            "PRATES/RESBM_N.D",
            "Interest rate on reserve balances (IORB)",
        )
        .with_units("Percent")
        .with_frequency("Daily")
        .with_keywords("iorb reserves policy rate floor fomc"),
        SdmxCatalogueEntry::new(
            "H41/RESPPALG_N.WW",
            "Assets: securities held outright, Wednesday level",
        )
        .with_units("Millions of USD")
        .with_frequency("Weekly (Wednesday)")
        .with_keywords("balance sheet qt qe soma h41 reserves securities"),
        SdmxCatalogueEntry::new("H6/M1_N.M", "M1 money stock, not seasonally adjusted")
            .with_units("Billions of USD")
            .with_frequency("Monthly")
            .with_keywords("money supply m1 monetary aggregate h6"),
        SdmxCatalogueEntry::new("H6/M2_N.M", "M2 money stock, not seasonally adjusted")
            .with_units("Billions of USD")
            .with_frequency("Monthly")
            .with_keywords("money supply m2 monetary aggregate h6"),
        SdmxCatalogueEntry::new(
            "G17/IP.B50001.S",
            "Industrial production, total index (seasonally adjusted)",
        )
        .with_units("Index 2017 100")
        .with_frequency("Monthly")
        .with_keywords("industrial production output manufacturing g17"),
        SdmxCatalogueEntry::new(
            "G17/CAPUTL.B50001.S",
            "Capacity utilisation, total industry (seasonally adjusted)",
        )
        .with_units("Percentage")
        .with_frequency("Monthly")
        .with_keywords("capacity utilisation utilization slack g17"),
        SdmxCatalogueEntry::new("H10/RXI$US_N.B.EU", "US dollar / euro spot exchange rate")
            .with_units("USD per EUR")
            .with_frequency("Business daily")
            .with_keywords("fx forex eurusd dollar h10"),
        SdmxCatalogueEntry::new("H10/JRXWTFB_N.B", "Nominal broad US dollar index")
            .with_units("Index Jan 2006=100")
            .with_frequency("Business daily")
            .with_keywords("dollar index dxy broad trade weighted h10"),
    ]
});

/// The curated Federal Reserve series, for the search and its tests.
#[must_use]
pub fn catalogue() -> &'static [SdmxCatalogueEntry] {
    &CATALOGUE
}

fn catalogue_entry(id: &str) -> Option<&'static SdmxCatalogueEntry> {
    CATALOGUE.iter().find(|entry| entry.id == id)
}

/// Rank the curated catalogue against a query.
///
/// The DDP publishes no series-search endpoint at all — its own site's search
/// is a form post against a session — so this is a checked list rather than a
/// query, ranked by [`sdmx::search_catalogue`]. Reads no network, so it cannot
/// fail.
pub async fn search(_state: &AppState, query: &str, limit: usize) -> Result<Vec<DataSearchResult>> {
    Ok(sdmx::search_catalogue(
        DataSource::Fed,
        catalogue(),
        query,
        limit,
    ))
}

/// `None` when this deployment can use the publisher; a string is the
/// operator-facing reason it cannot.
///
/// Always `None`: the Data Download Program takes no credential and rate-limits
/// nobody, so there is nothing an operator could have failed to configure.
#[must_use]
pub fn unavailable(_state: &AppState) -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    //! Federal Reserve DDP parser and wire tests.
    //!
    //! Every fixture here is a real download from `www.federalreserve.gov`,
    //! narrowed to a handful of columns and rows and otherwise byte-for-byte:
    //! the Board's quoting, its CRLF line endings and its missing final newline
    //! are all as served, because those are the three things a hand-written
    //! fixture would quietly tidy up.

    use super::*;
    use wiremock::matchers::{method, path as path_matcher, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use crate::config::Config;

    /// H.15's Treasury constant maturity package, `from=12/30/2025&to=01/05/2026`,
    /// narrowed to three of its eleven columns.
    ///
    /// Kept for what each part traps:
    ///
    /// * the descriptions carry a comma *inside* their quotes, and three
    ///   consecutive spaces inside the sentence;
    /// * `"Unique Identifier: "` carries a trailing space that `"Time Period"`
    ///   and `"Unit:"` do not;
    /// * `2026-01-01` is New Year's Day, published as a row of `ND` — the row
    ///   exists and the readings do not.
    const H15_DAILY: &str = include_str!("fixtures/fed_h15_daily.csv");

    /// H.15's monthly averages package, `lastobs=3`, two of thirty columns.
    /// Its rows are dated `2026-05` where the daily package writes `2026-08-12`.
    const H15_MONTHLY: &str = include_str!("fixtures/fed_h15_monthly.csv");

    /// PRATES, `lastobs=1`, all three of its columns.
    ///
    /// IOER and IORR were discontinued in 2021 and IORB was not: the Board
    /// publishes the row as `2026-08-24,,,3.65`. Two blank cells beside a live
    /// figure are the second way this upstream states "no reading".
    const PRATES: &str = include_str!("fixtures/fed_prates.csv");

    /// H.4.1's first package, `lastobs=1`, two of its many columns.
    ///
    /// Two things only this release shows. Its unit column reads the bare word
    /// `Currency`, with the scale in `Multiplier:` and the code in `Currency:`;
    /// and at `lastobs=1` — the exact request the index builder sends — it
    /// spells the label `"Unique Identifier:"` with no trailing space, where
    /// the same package at `lastobs=2` spells it with one. Reproduced three
    /// times against the live host.
    const H41_WEEKLY: &str = include_str!("fixtures/fed_h41_weekly.csv");

    /// `Choose.aspx?rel=H15`, narrowed to its package chooser and one of the
    /// ASP.NET hidden inputs that share the page. Four radios, each wired to its
    /// own `<label for="…">`, and all four inside one `<span>` parent.
    const H15_CHOOSER: &str = include_str!("fixtures/fed_h15_chooser.html");

    /// `Choose.aspx?rel=G17`, narrowed to the first three of its thirteen
    /// `<option>`s — the other markup the DDP renders a chooser in.
    const G17_CHOOSER: &str = include_str!("fixtures/fed_g17_chooser.html");

    const DAILY_HASH: &str = "bf17364827e38702b42a58cf8eaa3f78";
    const WEEKLY_FF_HASH: &str = "8e83f7f17c5cea4d190d85ae6737639f";
    const WEEKLY_HASH: &str = "c3ec77dedd37c9aa112f71c9eba34b50";
    const MONTHLY_HASH: &str = "d7e27b7b09a3a7feae95b9c61781fcd8";

    /// `(date, value)` pairs, since the wire type carries no `PartialEq`.
    fn rows(observations: &[DataObservation]) -> Vec<(&str, Option<f64>)> {
        observations
            .iter()
            .map(|observation| (observation.date.as_str(), observation.value))
            .collect()
    }

    /* ------------------------------------------------------------------- id */

    #[test]
    fn splits_the_release_from_the_series_and_undots_the_boards_own_spelling() {
        assert_eq!(
            split_fed_id("H15/RIFLGFCY10_N.B"),
            FedId {
                release: "H15".to_owned(),
                series: "RIFLGFCY10_N.B".to_owned()
            }
        );
        // The Board writes "H.15" and "H.4.1" in prose; the `rel` parameter
        // takes neither.
        assert_eq!(
            split_fed_id("h.15/RIFSPFF_N.M"),
            FedId {
                release: "H15".to_owned(),
                series: "RIFSPFF_N.M".to_owned()
            }
        );
        assert_eq!(
            split_fed_id(" /H4.1/RESH4A_N.WW/ ").release,
            "H41".to_owned()
        );
    }

    #[test]
    fn leaves_the_series_half_exactly_as_the_board_writes_it() {
        // H.10 puts a `$` in the middle of a name. Upper-casing the release is
        // safe; tidying the series is not.
        assert_eq!(
            split_fed_id("h10/RXI$US_N.B.EU").series,
            "RXI$US_N.B.EU".to_owned()
        );
    }

    #[test]
    fn a_reference_with_no_slash_names_a_release_and_no_series() {
        assert_eq!(
            split_fed_id("H41"),
            FedId {
                release: "H41".to_owned(),
                series: String::new()
            }
        );
    }

    #[test]
    fn writes_dates_the_only_way_the_ddp_accepts_them() {
        assert_eq!(to_ddp_date("2015-06-30").as_deref(), Some("06/30/2015"));
        assert_eq!(to_ddp_date(" 2026-01-05 ").as_deref(), Some("01/05/2026"));
    }

    #[test]
    fn refuses_a_date_it_cannot_read_rather_than_sending_a_malformed_bound() {
        // A bound the DDP cannot parse is answered with the same zero bytes as
        // everything else, so an unreadable date has to become "no bound" and
        // let `package_url` fill it in.
        for bad in ["not-a-date", "2015-6-30", "2015/06/30", "20150630", ""] {
            assert_eq!(to_ddp_date(bad), None, "{bad}");
        }
    }

    /* ------------------------------------------------------------------ csv */

    #[test]
    fn reads_the_labelled_header_block_and_the_observations() {
        let package = parse_fed_package(H15_DAILY);

        assert_eq!(package.columns.len(), 3);
        assert_eq!(package.columns[1].name, "RIFLGFCY10_N.B");
        assert_eq!(package.columns[1].unique_id, "H15/H15/RIFLGFCY10_N.B");
        // The colon separates the measure from its qualifier; both are the unit.
        assert_eq!(package.columns[1].unit, "Percent Per Year");
        assert_eq!(package.columns[1].multiplier, 1.0);
        assert_eq!(package.columns[1].currency, "NA");

        assert_eq!(package.rows.len(), 5);
        assert_eq!(
            package.rows[0],
            FedRow {
                date: "2025-12-30".to_owned(),
                values: vec![Some(3.65), Some(4.14), Some(4.81)],
            }
        );
    }

    #[test]
    fn keeps_the_comma_that_lives_inside_a_quoted_description() {
        // Splitting the line on commas would shift every later column by one,
        // which does not fail loudly: it pairs a date with the wrong series'
        // value. The Board's own three consecutive spaces are collapsed.
        let package = parse_fed_package(H15_DAILY);
        assert_eq!(
            package.columns[1].description,
            "Market yield on U.S. Treasury securities at 10-year constant maturity, \
             quoted on investment basis"
        );
        // If the comma had split the row, the 10-year column would not be at 1.
        assert_eq!(package.columns[2].name, "RIFLGFCY30_N.B");
    }

    #[test]
    fn the_boards_missing_marker_is_not_a_zero() {
        // 2026-01-01 is New Year's Day: the Board publishes the row and no
        // readings in it. A zero would draw the whole curve collapsing to nought
        // for a day, which is a figure somebody trades off.
        let package = parse_fed_package(H15_DAILY);
        assert_eq!(
            package.rows[2],
            FedRow {
                date: "2026-01-01".to_owned(),
                values: vec![None, None, None],
            }
        );
    }

    #[test]
    fn a_blank_cell_beside_a_live_figure_is_not_a_zero_either() {
        // IOER and IORR ended in 2021 and IORB did not; the row is `,,,3.65`.
        let package = parse_fed_package(PRATES);
        assert_eq!(package.columns.len(), 3);
        assert_eq!(
            package.rows[0],
            FedRow {
                date: "2026-08-24".to_owned(),
                values: vec![None, None, Some(3.65)],
            }
        );
    }

    #[test]
    fn reads_a_monthly_package_whose_rows_are_dated_yyyy_mm() {
        // Matching only the daily form treats every monthly row as a header and
        // returns a package with no observations in it at all.
        let package = parse_fed_package(H15_MONTHLY);
        assert_eq!(package.columns[0].name, "RIFSPFF_N.M");
        assert_eq!(
            package
                .rows
                .iter()
                .map(|r| r.date.as_str())
                .collect::<Vec<_>>(),
            ["2026-05-01", "2026-06-01", "2026-07-01"]
        );
        assert_eq!(package.rows[0].values, vec![Some(3.63), Some(6.75)]);
    }

    #[test]
    fn keys_the_header_rows_by_label_so_a_missing_trailing_space_cannot_shift_them() {
        // H.4.1 at `lastobs=1` writes `"Unique Identifier:"` where H.15 writes
        // `"Unique Identifier: "`. Reading the block by position, or by the
        // literal label, would put the short names in the identifier slot and
        // leave every H.4.1 series unnameable — on the exact request the index
        // builder sends.
        let package = parse_fed_package(H41_WEEKLY);
        assert_eq!(package.columns[0].name, "RESH4A_N.WW");
        assert_eq!(package.columns[0].unique_id, "H41/H41/RESH4A_N.WW");
        assert_eq!(package.columns[1].name, "RESH4AO_N.WW");
    }

    #[test]
    fn states_a_currency_package_in_the_scale_the_board_publishes_it_on() {
        // The unit column reads the bare word `Currency`; the scale and the code
        // are in the two columns beside it. "Millions of USD" against 3,866,586
        // is a balance sheet; "Currency" against it is nothing.
        let package = parse_fed_package(H41_WEEKLY);
        assert_eq!(package.columns[0].unit, "Millions of USD");
        assert_eq!(package.columns[0].multiplier, 1_000_000.0);
        // The figures themselves are left exactly as the Board published them.
        assert_eq!(
            package.rows[0].values,
            vec![Some(3_866_586.0), Some(-175_663.0)]
        );
    }

    #[test]
    fn reads_a_header_row_the_release_never_sent_as_absent_rather_than_shifting() {
        // Drop the currency row entirely — reading the block by position would
        // then slide the identifiers into the names' slot.
        let without_currency: String = H41_WEEKLY
            .lines()
            .filter(|line| !line.starts_with("\"Currency:\""))
            .collect::<Vec<_>>()
            .join("\r\n");

        let package = parse_fed_package(&without_currency);
        assert_eq!(package.columns[0].name, "RESH4A_N.WW");
        assert_eq!(package.columns[0].currency, "");
        // With no code to name, the bare word stands rather than being invented.
        assert_eq!(package.columns[0].unit, "Currency");
    }

    #[test]
    fn reads_a_doubled_quote_as_one_literal_quote() {
        let csv = "\"Time Period\",\"A\"\r\n\"Series Description\",\"say \"\"hi\"\"\"\r\n";
        assert_eq!(parse_fed_package(csv).columns[0].description, "say \"hi\"");
    }

    #[test]
    fn reads_the_crlf_endings_and_absent_final_newline_the_ddp_actually_sends() {
        // Every capture from the live host ends mid-line, with no trailing
        // newline; a reader that needed one would drop the newest observation.
        // The fixtures are kept byte-faithful, CRLF included, so a checkout that
        // rewrote them would be caught here rather than in a chart.
        assert!(
            H15_DAILY.contains("\r\n"),
            "the fixture kept its CRLF endings"
        );
        assert!(H15_DAILY.ends_with("2026-01-05,3.71,4.17,4.85"));
        let package = parse_fed_package("\"Time Period\",\"A\"\r\n2026-01-05,1.5");
        assert_eq!(package.rows.len(), 1);
        assert_eq!(package.rows[0].values, vec![Some(1.5)]);
    }

    #[test]
    fn reads_an_empty_document_as_an_empty_package() {
        // The zero-byte body is a failure worth describing, but describing it
        // belongs to the fetch: a parser that threw here could not probe
        // packages speculatively.
        assert_eq!(parse_fed_package(""), FedPackage::default());
    }

    #[test]
    fn a_row_of_nothing_but_a_date_yields_no_readings_rather_than_zeroes() {
        let package = parse_fed_package("\"Time Period\",\"A\",\"B\"\r\n2026-01-05,,\r\n");
        assert_eq!(package.rows[0].values, vec![None, None]);
    }

    /* ----------------------------------------------------------------- unit */

    #[test]
    fn spells_out_the_boards_punctuation_without_dropping_half_the_unit() {
        // "Per Year" alone does not say what is being measured.
        assert_eq!(unit_of("Percent:_Per_Year", 1.0, "NA"), "Percent Per Year");
        assert_eq!(unit_of("Index:_2017_100", 1.0, "NA"), "Index 2017 100");
        assert_eq!(unit_of("Percentage", 1.0, "NA"), "Percentage");
    }

    #[test]
    fn names_each_scale_the_ddp_publishes_money_on() {
        assert_eq!(unit_of("Currency", 1_000_000.0, "USD"), "Millions of USD");
        // H.6 states its multiplier as `1e+09`.
        assert_eq!(unit_of("Currency", 1e9, "USD"), "Billions of USD");
        assert_eq!(unit_of("Currency", 1_000.0, "USD"), "Thousands of USD");
    }

    #[test]
    fn a_scale_nobody_has_seen_names_the_currency_rather_than_inventing_a_word() {
        assert_eq!(unit_of("Currency", 1.0, "USD"), "USD");
        assert_eq!(unit_of("Currency", 42.0, "USD"), "USD");
    }

    #[test]
    fn na_in_the_currency_column_is_the_boards_this_is_not_money() {
        assert_eq!(unit_of("Currency", 1_000_000.0, "NA"), "Currency");
        assert_eq!(unit_of("Currency", 1_000_000.0, ""), "Currency");
    }

    /* -------------------------------------------------------------- chooser */

    #[test]
    fn reads_packages_rendered_as_radio_inputs() {
        let packages = parse_chooser_page(H15_CHOOSER);
        assert_eq!(packages.len(), 4);
        assert_eq!(packages[0].hash, DAILY_HASH);
        assert_eq!(packages[0].label, "Treasury Constant Maturities");
        assert_eq!(packages[3].hash, MONTHLY_HASH);
        assert_eq!(packages[3].label, "Monthly Averages");
    }

    #[test]
    fn reads_each_inputs_own_label_rather_than_the_parent_they_all_share() {
        // All four of H.15's radios sit inside one `<span id="FreqRequest">`, so
        // reading the parent's text labels every package "Treasury Constant
        // Maturities". The Board wires each input to its own `<label for="…">`.
        let labels: Vec<String> = parse_chooser_page(H15_CHOOSER)
            .into_iter()
            .map(|package| package.label)
            .collect();
        assert_eq!(
            labels,
            [
                "Treasury Constant Maturities",
                "Weekly Averages (Fed Funds, Prime and Discount rates)",
                "Weekly Averages",
                "Monthly Averages",
            ]
        );
    }

    #[test]
    fn reads_packages_rendered_as_select_options() {
        // G.17 uses a `<select>` where H.15 uses radios. Matching only one
        // silently found nothing for half the releases.
        let packages = parse_chooser_page(G17_CHOOSER);
        assert_eq!(packages.len(), 3);
        assert_eq!(packages[0].hash, "6752c709e85190bd7d0ad535100a4175");
        assert_eq!(packages[0].label, "All the Latest Monthly Data");
        assert_eq!(packages[2].label, "All the Latest Annual Data");
    }

    #[test]
    fn ignores_the_aspnet_hidden_fields_that_carry_no_package() {
        // The live chooser ships `__VIEWSTATE` and `__EVENTVALIDATION` beside
        // the packages; their base64 is not a selection.
        assert!(H15_CHOOSER.contains("__VIEWSTATE"));
        assert_eq!(parse_chooser_page(H15_CHOOSER).len(), 4);
        assert_eq!(
            parse_chooser_page(
                r#"<input type="hidden" name="__VIEWSTATE" value="NBHoseries=x" />"#
            ),
            Vec::new()
        );
    }

    #[test]
    fn takes_a_package_only_from_a_full_width_lower_case_hash() {
        // A short or upper-cased run after `series=` is not an MD5 the DDP
        // issued, and asking for one costs a request that answers zero bytes.
        for value in [
            "rel=H15&series=deadbeef&type=package",
            "rel=H15&series=BF17364827E38702B42A58CF8EAA3F78&type=package",
            "rel=H15&type=package",
        ] {
            assert_eq!(
                parse_chooser_page(&format!("<option value=\"{value}\">x</option>")),
                Vec::new(),
                "{value}"
            );
        }
    }

    #[test]
    fn keeps_one_entry_for_a_package_the_page_offers_twice() {
        let html = format!(
            "<option value=\"series={DAILY_HASH}\">A</option>\
             <option value=\"series={DAILY_HASH}\">B</option>"
        );
        assert_eq!(parse_chooser_page(&html).len(), 1);
    }

    #[test]
    fn falls_back_to_the_hash_when_the_wording_moves() {
        let html = format!("<option value=\"series={DAILY_HASH}\">[csv, 1 KB]</option>");
        assert_eq!(parse_chooser_page(&html)[0].label, DAILY_HASH);
    }

    /* ------------------------------------------------------------------ url */

    #[test]
    fn asks_for_one_observation_when_all_it_needs_is_the_header_block() {
        let url = package_url("http://ddp", "H15", DAILY_HASH, &Window::LastObs(1));
        assert!(url.starts_with("http://ddp/Output.aspx?rel=H15"), "{url}");
        assert!(url.contains(&format!("series={DAILY_HASH}")), "{url}");
        assert!(url.contains("layout=seriescolumn"), "{url}");
        assert!(url.contains("label=include"), "{url}");
        assert!(url.ends_with("&lastobs=1"), "{url}");
    }

    #[test]
    fn sends_both_bounds_because_an_open_ended_range_returns_nothing() {
        // `to=` empty is answered with HTTP 200 and no bytes, not with "up to
        // today", so an unstated side is filled in rather than left open.
        let url = package_url(
            "http://ddp",
            "H15",
            DAILY_HASH,
            &Window::Range {
                from: Some("08/01/2026".to_owned()),
                to: None,
            },
        );
        assert!(url.contains("&lastobs=&from=08%2F01%2F2026&to="), "{url}");
        assert!(
            regex::Regex::new(r"to=\d{2}%2F\d{2}%2F\d{4}$")
                .unwrap()
                .is_match(&url),
            "{url}"
        );
    }

    #[test]
    fn fills_the_lower_bound_in_from_before_any_release_began() {
        // H.15 answers `from=01/01/1900` with a first row dated 1962-01-02, so
        // an unstated start costs nothing and guesses nothing.
        let url = package_url(
            "http://ddp",
            "H15",
            DAILY_HASH,
            &Window::Range {
                from: None,
                to: Some("08/24/2026".to_owned()),
            },
        );
        assert!(url.contains("from=01%2F01%2F1900"), "{url}");
        assert!(url.ends_with("to=08%2F24%2F2026"), "{url}");
    }

    #[test]
    fn asks_for_the_full_history_by_sending_no_lastobs_at_all() {
        // `lastobs=0` is not this: it returns the six header rows and nothing
        // beneath them, which parses into a well-formed empty series.
        let url = package_url(
            "http://ddp",
            "H15",
            DAILY_HASH,
            &Window::Range {
                from: None,
                to: None,
            },
        );
        assert!(!url.contains("lastobs"), "{url}");
        assert!(!url.contains("from="), "{url}");
    }

    #[test]
    fn trims_a_trailing_slash_off_a_configured_base() {
        let url = package_url("http://ddp/", "H15", DAILY_HASH, &Window::LastObs(1));
        assert!(url.starts_with("http://ddp/Output.aspx"), "{url}");
    }

    /* ------------------------------------------------------------ frequency */

    #[test]
    fn reads_the_frequency_off_the_suffix_the_board_encodes_it_in() {
        assert_eq!(frequency_of("RIFLGFCY10_N.B"), Some("Business daily"));
        assert_eq!(frequency_of("RESBM_N.D"), Some("Daily"));
        assert_eq!(frequency_of("RESPPALG_N.WW"), Some("Weekly (Wednesday)"));
        assert_eq!(frequency_of("RIFSPFF_N.M"), Some("Monthly"));
        assert_eq!(frequency_of("IP.B50001.A"), Some("Annual"));
        assert_eq!(frequency_of("ip.b50001.a"), Some("Annual"));
    }

    #[test]
    fn says_nothing_where_the_suffix_is_not_a_frequency() {
        // G.17 ends its names `.S` and `.N` — seasonally adjusted and not. A
        // confident wrong word in the panel header is worse than a blank one,
        // and the curated catalogue fills these in instead.
        assert_eq!(frequency_of("IP.B50001.S"), None);
        assert_eq!(frequency_of("IP.B50001.N"), None);
        assert_eq!(
            catalogue_entry("G17/IP.B50001.S").map(|entry| entry.frequency.as_str()),
            Some("Monthly")
        );
    }

    /* ---------------------------------------------------------------- index */

    fn entry(hash: &str, name: &str) -> IndexEntry {
        IndexEntry {
            hash: hash.to_owned(),
            column: FedColumn {
                name: name.to_owned(),
                ..FedColumn::default()
            },
        }
    }

    #[test]
    fn the_first_package_listed_wins_a_name_two_of_them_hold() {
        // `RESH4E_N.WW` is in four of H.4.1's eight packages; the chooser lists
        // the primary one first.
        let mut index = ReleaseIndex::default();
        index.insert(entry(DAILY_HASH, "RESH4E_N.WW"));
        index.insert(entry(MONTHLY_HASH, "RESH4E_N.WW"));
        assert_eq!(index.len(), 1);
        assert_eq!(index.get("RESH4E_N.WW").unwrap().hash, DAILY_HASH);
    }

    #[test]
    fn looks_a_series_up_however_the_reader_cased_it() {
        let mut index = ReleaseIndex::default();
        index.insert(entry(DAILY_HASH, "RIFLGFCY10_N.B"));
        assert!(index.get("riflgfcy10_n.b").is_some());
        assert!(index.get("  RIFLGFCY10_N.B  ").is_some());
        assert!(index.get("RIFLGFCY10").is_none());
    }

    #[test]
    fn keeps_the_boards_own_order_so_a_hint_reads_like_the_release() {
        let mut index = ReleaseIndex::default();
        for name in ["RIFLGFCM01_N.B", "RIFLGFCM03_N.B", "RIFLGFCY10_N.B"] {
            index.insert(entry(DAILY_HASH, name));
        }
        assert_eq!(
            index.names().collect::<Vec<_>>(),
            ["RIFLGFCM01_N.B", "RIFLGFCM03_N.B", "RIFLGFCY10_N.B"]
        );
    }

    #[test]
    fn an_unnamed_column_is_not_an_entry() {
        // A package whose header block did not parse contributes columns with
        // empty names; indexing one would make `fed:H15/` resolvable.
        let mut index = ReleaseIndex::default();
        index.insert(entry(DAILY_HASH, ""));
        assert!(index.is_empty());
    }

    /* ------------------------------------------------------------ catalogue */

    #[tokio::test]
    async fn requires_every_term_so_a_second_word_narrows_the_search() {
        let state = AppState::new(Config::default());
        let ids = |results: Vec<DataSearchResult>| {
            results
                .into_iter()
                .map(|result| result.id)
                .collect::<Vec<_>>()
        };

        assert_eq!(
            ids(search(&state, "10-year treasury", 10).await.unwrap()),
            ["H15/RIFLGFCY10_N.B"]
        );
        assert!(ids(search(&state, "treasury", 10).await.unwrap()).len() > 1);
        assert!(search(&state, "treasury eurusd", 10)
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn matches_words_a_reader_would_use_that_are_not_in_the_title() {
        let state = AppState::new(Config::default());
        let results = search(&state, "iorb", 10).await.unwrap();
        assert_eq!(results[0].id, "PRATES/RESBM_N.D");
        assert_eq!(results[0].provider, DataSource::Fed);
        // One surface, so there is no arm to name on the row.
        assert_eq!(results[0].source, None);
        assert_eq!(results[0].frequency.as_deref(), Some("Daily"));
    }

    #[tokio::test]
    async fn honours_the_limit_and_answers_nothing_to_nothing() {
        let state = AppState::new(Config::default());
        assert_eq!(search(&state, "rate", 2).await.unwrap().len(), 2);
        assert!(search(&state, "   ", 10).await.unwrap().is_empty());
    }

    #[test]
    fn every_curated_id_names_a_release_this_module_knows() {
        for entry in catalogue() {
            let id = split_fed_id(&entry.id);
            assert!(!id.series.is_empty(), "{}", entry.id);
            assert!(
                release_name(&id.release).is_some(),
                "{} names an unknown release",
                entry.id
            );
        }
    }

    /* --------------------------------------------------------------- the wire */

    fn state_for(server: &MockServer) -> AppState {
        AppState::new(Config {
            fed_ddp_base: server.uri(),
            ..Config::default()
        })
    }

    async fn mount_chooser(server: &MockServer, body: &str) {
        Mock::given(method("GET"))
            .and(path_matcher("/Choose.aspx"))
            .respond_with(ResponseTemplate::new(200).set_body_string(body))
            .mount(server)
            .await;
    }

    async fn mount_package(server: &MockServer, hash: &str, body: &str) {
        Mock::given(method("GET"))
            .and(path_matcher("/Output.aspx"))
            .and(query_param("series", hash))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/csv")
                    .set_body_string(body),
            )
            .mount(server)
            .await;
    }

    /// H.15 as the live host serves it: four packages, two of which this
    /// deployment has fixtures for and two of which answer the DDP's zero bytes.
    async fn mount_h15(server: &MockServer) {
        mount_chooser(server, H15_CHOOSER).await;
        mount_package(server, DAILY_HASH, H15_DAILY).await;
        mount_package(server, MONTHLY_HASH, H15_MONTHLY).await;
        mount_package(server, WEEKLY_FF_HASH, "").await;
        mount_package(server, WEEKLY_HASH, "").await;
    }

    /// Every request the mock saw, as `path?query`.
    async fn requests(server: &MockServer) -> Vec<String> {
        server
            .received_requests()
            .await
            .expect("the mock records requests")
            .iter()
            .map(|request| {
                format!(
                    "{}?{}",
                    request.url.path(),
                    request.url.query().unwrap_or_default()
                )
            })
            .collect()
    }

    #[tokio::test]
    async fn builds_a_release_index_and_resolves_a_series_through_it() {
        let server = MockServer::start().await;
        mount_h15(&server).await;

        let answer = get_series(
            &state_for(&server),
            "H15/RIFLGFCY10_N.B",
            Some("2025-12-30"),
            Some("2026-01-05"),
        )
        .await
        .expect("the index resolves the series");

        assert_eq!(answer.series.provider, DataSource::Fed);
        assert_eq!(answer.series.id, "H15/RIFLGFCY10_N.B");
        assert_eq!(answer.series.units, "Percent Per Year");
        assert_eq!(answer.series.frequency, "Business daily");
        assert_eq!(answer.series.source, "ddp");
        assert_eq!(answer.series.notes, "Release: H.15 Selected Interest Rates");
        assert_eq!(
            answer.series.source_url,
            format!("{}/Choose.aspx?rel=H15", server.uri())
        );

        // The 10-year column, not the 1-month beside it.
        assert_eq!(
            rows(&answer.observations),
            [
                ("2025-12-30", Some(4.14)),
                ("2025-12-31", Some(4.18)),
                ("2026-01-01", None),
                ("2026-01-02", Some(4.19)),
                ("2026-01-05", Some(4.17)),
            ]
        );
        assert_eq!(answer.series.observation_start, "2025-12-30");
        assert_eq!(answer.series.observation_end, "2026-01-05");

        let seen = requests(&server).await;
        assert!(seen.iter().any(|r| r.starts_with("/Choose.aspx?rel=H15")));
        // One probe per package the chooser listed, then the ranged download.
        assert_eq!(seen.iter().filter(|r| r.ends_with("lastobs=1")).count(), 4);
        assert_eq!(
            seen.iter()
                .filter(|r| r.starts_with("/Output.aspx") && !r.ends_with("lastobs=1"))
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn sends_both_bounds_on_the_wire_because_one_returns_nothing() {
        let server = MockServer::start().await;
        mount_h15(&server).await;

        get_series(
            &state_for(&server),
            "H15/RIFLGFCY10_N.B",
            Some("2026-08-01"),
            None,
        )
        .await
        .expect("an open upper bound is filled in, not sent open");

        let ranged = requests(&server)
            .await
            .into_iter()
            .find(|r| r.starts_with("/Output.aspx") && !r.contains("lastobs=1"))
            .expect("the ranged download was sent");

        assert!(ranged.contains("from=08%2F01%2F2026"), "{ranged}");
        assert!(ranged.contains("lastobs=&"), "{ranged}");
        assert!(
            regex::Regex::new(r"to=\d{2}%2F\d{2}%2F\d{4}")
                .unwrap()
                .is_match(&ranged),
            "{ranged}"
        );
    }

    #[tokio::test]
    async fn finds_a_series_in_a_later_package_and_dates_its_monthly_rows() {
        let server = MockServer::start().await;
        mount_h15(&server).await;

        let answer = get_series(
            &state_for(&server),
            "H15/RIFSPFF_N.M",
            Some("2026-01-01"),
            None,
        )
        .await
        .expect("the fourth package holds the monthly averages");

        assert_eq!(answer.series.frequency, "Monthly");
        assert_eq!(
            rows(&answer.observations),
            [
                ("2026-05-01", Some(3.63)),
                ("2026-06-01", Some(3.63)),
                ("2026-07-01", Some(3.63)),
            ]
        );
    }

    #[tokio::test]
    async fn answers_under_the_boards_spelling_however_the_reader_cased_it() {
        let server = MockServer::start().await;
        mount_h15(&server).await;

        let answer = get_series(&state_for(&server), "h.15/riflgfcy30_n.b", None, None)
            .await
            .expect("case is the reader's, the name is the Board's");
        assert_eq!(answer.series.id, "H15/RIFLGFCY30_N.B");
        assert_eq!(answer.observations.len(), 5);
    }

    #[tokio::test]
    async fn names_the_releases_own_series_when_asked_for_the_release_alone() {
        let server = MockServer::start().await;
        mount_h15(&server).await;

        let error = get_series(&state_for(&server), "H15", None, None)
            .await
            .expect_err("a release is not a series");

        // Three columns from the Treasury package and two from the monthly one;
        // the two packages that answered no bytes contribute nothing.
        assert_eq!(error.message, "H15 holds 5 series — name one");
        assert_eq!(error.code, codes::BAD_REQUEST);
        assert!(
            error
                .hint
                .as_deref()
                .unwrap()
                .contains("H15/RIFLGFCM01_N.B"),
            "{error:?}"
        );
    }

    #[tokio::test]
    async fn reports_an_unknown_series_rather_than_charting_a_neighbouring_column() {
        let server = MockServer::start().await;
        mount_h15(&server).await;

        let error = get_series(&state_for(&server), "H15/NOSUCH", None, None)
            .await
            .expect_err("an unknown name has no column");

        assert_eq!(error.message, "H15 has no series called NOSUCH");
        assert_eq!(error.code, codes::NOT_FOUND);
        assert!(
            error.hint.as_deref().unwrap().contains("RIFLGFCM01_N.B"),
            "{error:?}"
        );
    }

    #[tokio::test]
    async fn one_dead_package_costs_only_its_own_series() {
        // Two of H.15's four packages answer the DDP's zero bytes here. Losing
        // the whole release because one package was retired would be a far worse
        // trade than losing the series inside it.
        let server = MockServer::start().await;
        mount_h15(&server).await;

        let index = release_index(&state_for(&server), "H15")
            .await
            .expect("the surviving packages still index");
        assert_eq!(index.len(), 5);
        assert!(index.get("RIFSPBLP_N.M").is_some());
    }

    #[tokio::test]
    async fn turns_the_zero_byte_download_into_a_described_failure() {
        // The chart would otherwise be empty, which reads as "the Board
        // publishes nothing here" rather than "we asked the wrong question".
        let server = MockServer::start().await;
        mount_chooser(&server, H15_CHOOSER).await;
        mount_package(&server, DAILY_HASH, H15_DAILY).await;
        mount_package(&server, MONTHLY_HASH, H15_MONTHLY).await;
        mount_package(&server, WEEKLY_FF_HASH, "").await;
        mount_package(&server, WEEKLY_HASH, "").await;

        let state = state_for(&server);
        // Warm the index off the good bodies, then take the daily package away.
        release_index(&state, "H15").await.expect("index builds");
        server.reset().await;
        mount_package(&server, DAILY_HASH, "").await;

        let error = get_series(&state, "H15/RIFLGFCY10_N.B", None, None)
            .await
            .expect_err("no bytes is not an empty series");

        assert_eq!(error.code, codes::EMPTY_UPSTREAM);
        assert!(error.message.contains("HTTP 200, no bytes"), "{error:?}");
        assert!(
            error
                .hint
                .as_deref()
                .unwrap()
                .contains("narrower date range"),
            "{error:?}"
        );
    }

    #[tokio::test]
    async fn a_header_block_with_nothing_under_it_names_the_window_not_the_series() {
        // What `lastobs=0` and an empty range both look like: six header rows,
        // no observations, and a perfectly well-formed parse.
        let server = MockServer::start().await;
        mount_chooser(&server, H15_CHOOSER).await;
        mount_package(&server, WEEKLY_FF_HASH, "").await;
        mount_package(&server, WEEKLY_HASH, "").await;
        mount_package(&server, MONTHLY_HASH, H15_MONTHLY).await;

        let headers_only: String = H15_DAILY.lines().take(6).collect::<Vec<_>>().join("\r\n");
        mount_package(&server, DAILY_HASH, &headers_only).await;

        let error = get_series(&state_for(&server), "H15/RIFLGFCY10_N.B", None, None)
            .await
            .expect_err("a series with no readings is not a series");

        assert_eq!(error.code, codes::EMPTY_UPSTREAM);
        assert!(
            error
                .message
                .contains("no observations for H15/RIFLGFCY10_N.B"),
            "{error:?}"
        );
        assert!(
            error
                .hint
                .as_deref()
                .unwrap()
                .contains("Widen the date range"),
            "{error:?}"
        );
    }

    #[tokio::test]
    async fn a_release_with_no_packages_is_no_release() {
        // `Choose.aspx?rel=` and an unknown `rel` answer HTTP 400 live, but a
        // page that parses to no packages is the same answer and must not read
        // as an empty release.
        let server = MockServer::start().await;
        mount_chooser(&server, "<html><body>No such release.</body></html>").await;

        let error = get_series(&state_for(&server), "NOPE/X", None, None)
            .await
            .expect_err("no packages, no release");

        assert_eq!(error.code, codes::NOT_FOUND);
        assert_eq!(
            error.message,
            "The Federal Reserve publishes no release called NOPE"
        );
        assert!(error.hint.as_deref().unwrap().contains("H15"), "{error:?}");
    }

    #[tokio::test]
    async fn a_chooser_that_lists_packages_naming_nothing_is_our_bug_not_the_readers() {
        // Packages exist and no header block parsed: the download format has
        // changed under this reader, which is worth saying rather than reporting
        // an empty release.
        let server = MockServer::start().await;
        mount_chooser(&server, H15_CHOOSER).await;
        Mock::given(method("GET"))
            .and(path_matcher("/Output.aspx"))
            .respond_with(ResponseTemplate::new(200).set_body_string("nothing,useful\r\n"))
            .mount(&server)
            .await;

        let error = release_index(&state_for(&server), "H15")
            .await
            .expect_err("four packages and no series is a parse failure");

        assert_eq!(error.code, codes::PARSE_FAILED);
        assert!(error.message.contains("none of them named a series"));
    }

    #[tokio::test]
    async fn builds_the_index_once_and_charts_off_it_afterwards() {
        // The index is the expensive half — a chooser page and one request per
        // package — and it is what makes the second panel a single request.
        let server = MockServer::start().await;
        mount_h15(&server).await;
        let state = state_for(&server);

        get_series(
            &state,
            "H15/RIFLGFCY10_N.B",
            Some("2025-12-30"),
            Some("2026-01-05"),
        )
        .await
        .expect("first chart");
        let after_first = requests(&server).await.len();

        get_series(
            &state,
            "H15/RIFLGFCY30_N.B",
            Some("2025-12-01"),
            Some("2026-01-05"),
        )
        .await
        .expect("second chart");

        // One more request in total: the second series' own download.
        assert_eq!(requests(&server).await.len(), after_first + 1);
    }

    #[tokio::test]
    async fn refuses_a_reference_that_names_no_release_before_spending_a_request() {
        let server = MockServer::start().await;
        mount_h15(&server).await;

        let error = get_series(&state_for(&server), "  /  ", None, None)
            .await
            .expect_err("nothing to ask the Board about");

        assert_eq!(error.code, codes::BAD_REQUEST);
        assert!(error
            .message
            .contains("does not name a Federal Reserve release"));
        assert!(requests(&server).await.is_empty());
    }

    #[tokio::test]
    async fn a_column_the_package_stopped_returning_is_not_its_neighbour() {
        // The index said which package; the package says which column. Trusting
        // the index's position would chart the neighbouring series' figures
        // under this one's name.
        let server = MockServer::start().await;
        mount_h15(&server).await;
        let state = state_for(&server);
        release_index(&state, "H15").await.expect("index builds");

        // The package still downloads and the column is simply not in it any
        // more, which is what a renamed or retired series looks like.
        let renamed = H15_DAILY.replace("RIFLGFCY10_N.B", "RIFLGFCY07_N.B");

        server.reset().await;
        mount_package(&server, DAILY_HASH, &renamed).await;

        let error = get_series(&state, "H15/RIFLGFCY10_N.B", None, None)
            .await
            .expect_err("the column is gone, not moved");

        assert_eq!(error.code, codes::EMPTY_UPSTREAM);
        assert!(
            error
                .message
                .contains("no longer returns a column for RIFLGFCY10_N.B"),
            "{error:?}"
        );
    }

    #[tokio::test]
    async fn takes_no_credential_so_there_is_nothing_to_be_unconfigured() {
        assert_eq!(unavailable(&AppState::new(Config::default())), None);
    }
}
