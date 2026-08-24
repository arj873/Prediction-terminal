//! SEC EDGAR — filings and the XBRL facts inside them.
//!
//! Two surfaces, and they answer different questions:
//!
//!  - **Filings** (`/submissions/CIK….json`) is the wire. An 8-K appears here
//!    within seconds of acceptance, which is often before the press release, and
//!    is what a market on a merger, a bankruptcy or an earnings date settles on.
//!  - **Company concept** (`/api/xbrl/companyconcept/…`) is the history. Every
//!    value a company has ever reported for one tag — `Revenues`,
//!    `EarningsPerShareDiluted` — with the filing that reported it.
//!    Restatements appear as additional values for the same period rather than
//!    as edits, so "what did they say, and when did they change it" is
//!    answerable.
//!
//! EDGAR's fair-access policy asks every caller to identify itself. Measured
//! from this container rather than taken on trust: a request with **no**
//! User-Agent is answered `403`, and so is one sending curl's own default,
//! because the string names a tool rather than a caller. The two-token
//! `Name contact@domain` form the policy asks for is answered `200`, and so is
//! an ordinary browser string, and so is a bare `x` — what EDGAR refuses is a
//! header it recognises as an undeclared tool, not a short one. The branch this
//! was ported from claimed a browser-shaped User-Agent is refused; that does not
//! reproduce, and the note is corrected rather than carried across. The default
//! names this project, and an operator running it publicly should set
//! `SEC_USER_AGENT` to their own contact, which is what the policy actually
//! asks for.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde::Deserialize;
use terminal_core::types::{
    SecCompany, SecConceptResponse, SecFact, SecFiling, SecFilingsResponse, SecSearchResult,
};

use crate::app::AppState;
use crate::cache::ttl;
use crate::error::UpstreamError;
use crate::http::FetchOptions;

type Result<T> = std::result::Result<T, UpstreamError>;

const TIMEOUT: Duration = Duration::from_secs(30);
const RETRIES: u32 = 1;

fn user_agent(state: &AppState) -> String {
    let configured = state.config().sec_user_agent.trim();
    if configured.is_empty() {
        "prediction-terminal contact@example.com".to_owned()
    } else {
        configured.to_owned()
    }
}

/// EDGAR keys everything on a zero-padded ten-digit CIK.
#[must_use]
pub fn pad_cik(raw: &str) -> String {
    let digits: String = raw.chars().filter(char::is_ascii_digit).collect();
    format!("{digits:0>10}")
}

async fn fetch<T>(state: &AppState, url: String, key: String, ttl: Duration) -> Result<Arc<T>>
where
    T: DeserializeOwned + Send + Sync + 'static,
{
    let agent = user_agent(state);
    state
        .cache()
        .cached(&key, ttl, || async {
            state
                .http()
                .fetch_json::<T>(
                    &url,
                    FetchOptions::new()
                        .timeout(TIMEOUT)
                        .retries(RETRIES)
                        .header("User-Agent", &agent),
                )
                .await
        })
        .await
}

/* -------------------------------------------------------------- ticker map */

#[derive(Debug, Clone, Deserialize)]
struct TickerRow {
    #[serde(default)]
    cik_str: Option<i64>,
    #[serde(default)]
    ticker: Option<String>,
    #[serde(default)]
    title: Option<String>,
}

#[derive(Debug, Clone)]
struct Filer {
    cik: String,
    name: String,
    ticker: String,
}

/// The ticker → CIK map, fetched once per session.
///
/// It is the only way to go from a symbol to a CIK — EDGAR has no lookup
/// endpoint — and at roughly 10,000 rows it is small enough to hold. Keyed by
/// ticker *and* by CIK so `SEC AAPL` and `SEC 320193` both resolve, and the
/// insertion order is kept so a name search walks the file's own ordering
/// rather than a hash order that would differ run to run.
async fn ticker_map(state: &AppState) -> Result<Arc<(Vec<Filer>, HashMap<String, usize>)>> {
    let url = format!(
        "{}/files/company_tickers.json",
        state.config().sec_www_base.trim_end_matches('/')
    );
    let agent = user_agent(state);

    state
        .cache()
        .cached("sec:tickers", ttl::CATALOGUE, || async {
            let raw = state
                .http()
                .fetch_json::<HashMap<String, TickerRow>>(
                    &url,
                    FetchOptions::new()
                        .timeout(Duration::from_secs(40))
                        .retries(RETRIES)
                        .header("User-Agent", &agent),
                )
                .await?;

            // The file is a JSON object keyed by a stringified index; sorting by
            // that index restores the ordering EDGAR publishes, which is by
            // descending size and is what makes the first match the primary one.
            let mut rows: Vec<(usize, TickerRow)> = raw
                .into_iter()
                .filter_map(|(key, row)| Some((key.parse::<usize>().ok()?, row)))
                .collect();
            rows.sort_by_key(|(index, _)| *index);

            let mut filers: Vec<Filer> = Vec::with_capacity(rows.len());
            let mut index: HashMap<String, usize> = HashMap::with_capacity(rows.len() * 2);

            for (_, row) in rows {
                let (Some(ticker), Some(cik)) = (row.ticker, row.cik_str) else {
                    continue;
                };
                let filer = Filer {
                    cik: pad_cik(&cik.to_string()),
                    name: row.title.unwrap_or_else(|| ticker.clone()),
                    ticker,
                };
                let position = filers.len();
                // A company with several share classes appears more than once;
                // the first listing is its primary one, so it wins.
                index.entry(filer.ticker.clone()).or_insert(position);
                index.entry(filer.cik.clone()).or_insert(position);
                filers.push(filer);
            }

            Ok((filers, index))
        })
        .await
}

/// Resolve a ticker, a CIK or a company name to a CIK.
pub async fn resolve_cik(state: &AppState, query: &str) -> Result<String> {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return Err(UpstreamError::bad_request("Missing company"));
    }

    let map = ticker_map(state).await?;
    let (filers, index) = map.as_ref();

    if let Some(position) = index.get(&trimmed.to_uppercase()) {
        return Ok(filers[*position].cik.clone());
    }

    // A bare number is a CIK whether or not it is in the ticker file — plenty of
    // filers (funds, individuals filing Form 4) have a CIK and no ticker at all.
    if !trimmed.is_empty() && trimmed.len() <= 10 && trimmed.chars().all(|c| c.is_ascii_digit()) {
        return Ok(pad_cik(trimmed));
    }

    let words: Vec<String> = trimmed
        .to_lowercase()
        .split_whitespace()
        .map(str::to_owned)
        .collect();
    for filer in filers {
        let name = filer.name.to_lowercase();
        if words.iter().all(|word| name.contains(word.as_str())) {
            return Ok(filer.cik.clone());
        }
    }

    Err(
        UpstreamError::not_found(format!("EDGAR has no filer matching \"{query}\""))
            .with_hint("Try a ticker (AAPL), a CIK (320193), or a distinctive word from the name."),
    )
}

/// Companies whose ticker or name matches, for the search panel.
pub async fn search(state: &AppState, query: &str, limit: usize) -> Result<Vec<SecSearchResult>> {
    let words: Vec<String> = query
        .trim()
        .to_lowercase()
        .split_whitespace()
        .map(str::to_owned)
        .collect();
    if words.is_empty() {
        return Ok(Vec::new());
    }

    let map = ticker_map(state).await?;
    let (filers, _) = map.as_ref();

    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    let mut results = Vec::new();

    for filer in filers {
        // Checked before the push rather than after it, as in `zip_filings`: a
        // panel with no room left wants no rows, and testing afterwards hands
        // back the first match however small the limit.
        if results.len() >= limit {
            break;
        }
        if seen.contains(filer.cik.as_str()) {
            continue;
        }
        let haystack = format!("{} {}", filer.ticker, filer.name).to_lowercase();
        if !words.iter().all(|word| haystack.contains(word.as_str())) {
            continue;
        }
        seen.insert(filer.cik.as_str());
        results.push(SecSearchResult {
            cik: filer.cik.clone(),
            ticker: filer.ticker.clone(),
            name: filer.name.clone(),
        });
    }

    Ok(results)
}

/* -------------------------------------------------------------- filings */

#[derive(Debug, Default, Clone, Deserialize)]
pub struct SubmissionsFilings {
    #[serde(default, rename = "accessionNumber")]
    pub accession_number: Vec<String>,
    #[serde(default, rename = "filingDate")]
    pub filing_date: Vec<String>,
    #[serde(default, rename = "reportDate")]
    pub report_date: Vec<String>,
    #[serde(default)]
    pub form: Vec<String>,
    #[serde(default)]
    pub items: Vec<String>,
    /// Optional per element, not merely optional as a whole: EDGAR ships `null`
    /// inside these parallel arrays — `isXBRLNumeric` in this very object is
    /// 979 nulls in 1,001 — and a bare `Vec<f64>` would fail the *document* on
    /// one of them, costing a reader every filing to say nothing about one.
    #[serde(default)]
    pub size: Vec<Option<f64>>,
    #[serde(default, rename = "primaryDocument")]
    pub primary_document: Vec<String>,
    #[serde(default, rename = "primaryDocDescription")]
    pub primary_doc_description: Vec<String>,
}

#[derive(Debug, Default, Clone, Deserialize)]
pub struct Submissions {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub tickers: Vec<String>,
    #[serde(default)]
    pub exchanges: Vec<String>,
    #[serde(default)]
    pub sic: Option<String>,
    #[serde(default, rename = "sicDescription")]
    pub sic_description: Option<String>,
    #[serde(default, rename = "fiscalYearEnd")]
    pub fiscal_year_end: Option<String>,
    #[serde(default)]
    pub filings: Option<SubmissionsRecent>,
}

#[derive(Debug, Default, Clone, Deserialize)]
pub struct SubmissionsRecent {
    #[serde(default)]
    pub recent: Option<SubmissionsFilings>,
}

/// EDGAR ships `filings.recent` as parallel arrays — one per field, aligned by
/// index — rather than as a list of objects.
///
/// Zipping them here means nothing downstream has to know that, and an array
/// that is short (which happens when a field does not apply to every form)
/// yields an empty string rather than shifting every subsequent filing's data by
/// one. That off-by-one is the whole reason this is a named function with a
/// test: it would misattribute a form type to the wrong filing, silently.
#[must_use]
pub fn zip_filings(
    filings: &SubmissionsFilings,
    cik: &str,
    www_base: &str,
    limit: usize,
    form: Option<&str>,
) -> Vec<SecFiling> {
    let wanted = form
        .map(|f| f.trim().to_uppercase())
        .filter(|f| !f.is_empty());
    let bare_cik = cik.trim_start_matches('0');
    let mut out = Vec::new();

    let at = |list: &[String], i: usize| list.get(i).cloned().unwrap_or_default();

    for (i, accession) in filings.accession_number.iter().enumerate() {
        if out.len() >= limit {
            break;
        }
        let form_type = at(&filings.form, i);
        if let Some(wanted) = &wanted {
            if !form_type.eq_ignore_ascii_case(wanted) {
                continue;
            }
        }

        let bare = accession.replace('-', "");
        let document = at(&filings.primary_document, i);
        let url = if document.is_empty() {
            format!("{www_base}/Archives/edgar/data/{bare_cik}/{bare}/")
        } else {
            format!("{www_base}/Archives/edgar/data/{bare_cik}/{bare}/{document}")
        };

        out.push(SecFiling {
            accession: accession.clone(),
            form: form_type,
            filed: at(&filings.filing_date, i),
            report_date: at(&filings.report_date, i),
            items: at(&filings.items, i),
            primary_document: document,
            description: at(&filings.primary_doc_description, i),
            size: filings.size.get(i).copied().flatten(),
            url,
        });
    }

    out
}

fn company_of(raw: &Submissions, cik: &str, www_base: &str) -> SecCompany {
    SecCompany {
        cik: cik.to_owned(),
        name: raw.name.clone().unwrap_or_else(|| cik.to_owned()),
        tickers: raw.tickers.clone(),
        exchanges: raw.exchanges.clone(),
        sic: raw.sic.clone().unwrap_or_default(),
        sic_description: raw.sic_description.clone().unwrap_or_default(),
        fiscal_year_end: raw.fiscal_year_end.clone().unwrap_or_default(),
        url: format!(
            "{www_base}/cgi-bin/browse-edgar?action=getcompany&CIK={cik}&type=&dateb=&owner=include&count=40"
        ),
    }
}

async fn submissions(state: &AppState, cik: &str) -> Result<Arc<Submissions>> {
    let url = format!(
        "{}/submissions/CIK{cik}.json",
        state.config().sec_data_base.trim_end_matches('/')
    );
    fetch::<Submissions>(state, url, format!("sec:submissions:{cik}"), ttl::META).await
}

pub async fn get_filings(
    state: &AppState,
    query: &str,
    limit: usize,
    form: Option<&str>,
) -> Result<SecFilingsResponse> {
    let cik = resolve_cik(state, query).await?;
    let raw = submissions(state, &cik).await?;
    let www_base = state.config().sec_www_base.trim_end_matches('/').to_owned();

    let recent = raw
        .filings
        .as_ref()
        .and_then(|f| f.recent.clone())
        .unwrap_or_default();

    let filings = zip_filings(&recent, &cik, &www_base, limit.clamp(1, 250), form);

    let mut forms: Vec<String> = recent.form.clone();
    forms.sort();
    forms.dedup();

    if filings.is_empty() {
        if let Some(form) = form.filter(|f| !f.trim().is_empty()) {
            let present = forms
                .iter()
                .take(10)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ");
            return Err(UpstreamError::not_found(format!(
                "{} has filed no recent {form}",
                raw.name.clone().unwrap_or_else(|| cik.clone())
            ))
            .with_hint(format!(
                "EDGAR's recent-filings document covers the last ~1,000 filings. Forms present \
                 are {present}."
            )));
        }
    }

    Ok(SecFilingsResponse {
        company: company_of(&raw, &cik, &www_base),
        filings,
        forms,
    })
}

/* ------------------------------------------------------------ xbrl facts */

#[derive(Debug, Default, Clone, Deserialize)]
struct ConceptUnit {
    #[serde(default)]
    start: Option<String>,
    #[serde(default)]
    end: Option<String>,
    #[serde(default)]
    val: Option<f64>,
    #[serde(default)]
    accn: Option<String>,
    #[serde(default)]
    fy: Option<i64>,
    #[serde(default)]
    fp: Option<String>,
    #[serde(default)]
    form: Option<String>,
    #[serde(default)]
    filed: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct ConceptResponse {
    #[serde(default)]
    taxonomy: Option<String>,
    #[serde(default)]
    tag: Option<String>,
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    units: HashMap<String, Vec<ConceptUnit>>,
}

/// XBRL tags are CamelCase identifiers; anything else is a typo, not a tag.
fn is_tag(tag: &str) -> bool {
    let bytes = tag.as_bytes();
    (2..=121).contains(&bytes.len())
        && bytes[0].is_ascii_alphabetic()
        && bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || *b == b'_' || *b == b'-')
}

/// Every value a company has reported for one XBRL tag.
///
/// The unit is chosen rather than assumed: a concept can be reported in `USD`,
/// `shares` and `USD/shares` at once, and picking the first key in the object
/// would depend on JSON ordering — which is a hash order here, so it would
/// differ between runs. The richest series wins, which is what someone asking
/// for `Revenues` means.
pub async fn get_concept(
    state: &AppState,
    query: &str,
    raw_tag: &str,
    taxonomy: &str,
) -> Result<SecConceptResponse> {
    let tag = raw_tag.trim();
    if !is_tag(tag) {
        return Err(
            UpstreamError::bad_request(format!("\"{raw_tag}\" is not an XBRL tag")).with_hint(
                "Tags are CamelCase, e.g. Revenues, NetIncomeLoss, Assets, \
                 EarningsPerShareDiluted.",
            ),
        );
    }

    let cik = resolve_cik(state, query).await?;
    let url = format!(
        "{}/api/xbrl/companyconcept/CIK{cik}/{}/{}.json",
        state.config().sec_data_base.trim_end_matches('/'),
        urlencoding::encode(taxonomy),
        urlencoding::encode(tag)
    );

    let raw = fetch::<ConceptResponse>(
        state,
        url,
        format!("sec:concept:{cik}:{taxonomy}:{tag}"),
        ttl::FRED,
    )
    .await
    .map_err(|error| {
        if error.code == "not_found" {
            UpstreamError::not_found(format!("This filer has never reported {taxonomy}:{tag}"))
                .with_hint(
                    "Not every issuer tags every concept. Common ones: Revenues, \
                     RevenueFromContractWithCustomerExcludingAssessedTax, NetIncomeLoss, Assets, \
                     Liabilities, StockholdersEquity, EarningsPerShareDiluted.",
                )
        } else {
            error
        }
    })?;

    let company = submissions(state, &cik)
        .await
        .map(|s| (*s).clone())
        .unwrap_or_default();
    let www_base = state.config().sec_www_base.trim_end_matches('/').to_owned();

    // The richest series wins. Ties break on the unit name so the choice is
    // deterministic rather than dependent on hash order.
    let mut units: Vec<(&String, &Vec<ConceptUnit>)> = raw.units.iter().collect();
    units.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then_with(|| a.0.cmp(b.0)));
    let (unit_name, values) = units
        .first()
        .map_or((String::new(), Vec::new()), |(name, list)| {
            ((*name).clone(), (*list).clone())
        });

    let mut facts: Vec<SecFact> = values
        .into_iter()
        .filter_map(|f| {
            let value = f.val.filter(|v| v.is_finite())?;
            let end = f.end.filter(|e| !e.is_empty())?;
            Some(SecFact {
                end,
                start: f.start.unwrap_or_default(),
                value,
                fiscal_year: f.fy,
                fiscal_period: f.fp.unwrap_or_default(),
                form: f.form.unwrap_or_default(),
                filed: f.filed.unwrap_or_default(),
                accession: f.accn.unwrap_or_default(),
                unit: unit_name.clone(),
            })
        })
        .collect();
    facts.sort_by(|a, b| a.end.cmp(&b.end).then_with(|| a.filed.cmp(&b.filed)));

    let mut description = raw.description.clone().unwrap_or_default();
    description.truncate(2000);

    Ok(SecConceptResponse {
        company: company_of(&company, &cik, &www_base),
        taxonomy: raw.taxonomy.clone().unwrap_or_else(|| taxonomy.to_owned()),
        tag: raw.tag.clone().unwrap_or_else(|| tag.to_owned()),
        label: raw.label.clone().unwrap_or_else(|| tag.to_owned()),
        description,
        unit: unit_name,
        facts,
    })
}
#[cfg(test)]
mod tests {
    //! EDGAR parser and wire tests.
    //!
    //! Every fixture here was captured live from `data.sec.gov` and
    //! `www.sec.gov` with curl on 2026-08-24 and trimmed to what a test can
    //! assert in full — a projection of a real answer, in EDGAR's own field
    //! order, never a hand-built payload. The three cases the wire types admit
    //! but the captures do not contain (a ticker row missing a field, a fact
    //! with no value, two units of equal length) are built inline with `json!`
    //! and say so.
    //!
    //! The fair-access policy was measured from this container rather than
    //! taken on trust: no `User-Agent` is answered `403` with the page in
    //! `sec_undeclared_tool.html`, so is curl's own default, and a header that
    //! names a caller rather than a tool — `x` included — is answered `200`.

    use super::*;
    use serde_json::json;
    use wiremock::matchers::{header, method, path as path_matcher};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use crate::config::Config;
    use crate::error::codes;

    /// Apple's submissions document, trimmed to twelve filings — with the
    /// `items` array deliberately left at four, because that mismatch is the
    /// whole reason the zip is a named function rather than an inline loop.
    const SUBMISSIONS: &str = include_str!("fixtures/sec_submissions.json");

    /// Thirteen consecutive filings out of the same document, captured
    /// 2026-08-24 as the window Apple filed between 2018-04-17 and 2018-05-25,
    /// every parallel array cut at the same two indices.
    ///
    /// Kept for what that fortnight happens to contain: an 8-K citing items
    /// `2.02,9.01` (the quarterly results a market settles on), a second 8-K
    /// citing `8.01`, and the `8-K/A` that amends it — so a form filter that
    /// swept amendments in with their originals would be visible. It also keeps
    /// `filings.files`, EDGAR's pointer at the 1,240 older filings this module
    /// does not read, and `isXBRLNumeric`, an array of nulls in a field nothing
    /// here declares.
    const SUBMISSIONS_8K: &str = include_str!("fixtures/sec_submissions_8k.json");

    /// `company_tickers.json` as `www.sec.gov` serves it, cut from 10,403 rows
    /// to eleven with the keys and values verbatim.
    ///
    /// Kept for three shapes the real file has and a toy one would not:
    /// Alphabet's four share classes under one CIK, Berkshire's two under keys
    /// `9` and `7469` — so reading those keys as text rather than as numbers
    /// would make BRK-A the primary listing — and the three separate companies
    /// whose names begin "Apple".
    const TICKERS: &str = include_str!("fixtures/sec_tickers.json");

    /// Apple's `us-gaap:Assets`, the first eight facts EDGAR sends.
    ///
    /// Kept because it is a restatement in the wild: FY2008 total assets were
    /// filed at $39.572bn and restated to $36.171bn by the 10-K/A of
    /// 2010-01-25, so the same period carries two different figures from four
    /// different filings. Every fact is an instant and none of them has a
    /// `start` at all.
    const ASSETS: &str = include_str!("fixtures/sec_concept_assets.json");

    /// Apple's `us-gaap:EarningsPerShareDiluted`, seven consecutive facts.
    ///
    /// Kept for the 7-for-1 split of 2014, which restated FY2012 diluted EPS
    /// from $44.15 to $6.31 without anything having changed about the year; for
    /// the 8-K of 2015-01-28, which EDGAR files under `"fy": null, "fp": null`;
    /// and because the annual figure and the quarter that ends on the same day
    /// are interleaved here in an order that is not the order a reader wants.
    const EPS: &str = include_str!("fixtures/sec_concept_eps.json");

    /// Toyota's `us-gaap:Revenues`, five JPY facts and three USD ones.
    ///
    /// Kept because it is a concept reported in two currencies at once, which
    /// is the case the unit choice exists for: ¥18.95tn and $203.7bn describe
    /// the same year and belong on different axes.
    const TOYOTA: &str = include_str!("fixtures/sec_concept_revenues_toyota.json");

    /// AstraZeneca's `ifrs-full:Revenue`, four facts.
    ///
    /// Kept because data.sec.gov ships `label` and `description` as `null` for
    /// the IFRS taxonomy — every foreign private issuer's concepts arrive with
    /// no heading of their own.
    const IFRS: &str = include_str!("fixtures/sec_concept_ifrs_revenue.json");

    /// The page `data.sec.gov` serves with a `403` to a request that sends no
    /// `User-Agent`, captured 2026-08-24 with the `<style>` block and the
    /// "More Information" footer dropped. What is kept is the part that names
    /// the cause and the remedy — undeclared traffic, and a user agent carrying
    /// company-specific information. The ten-requests-a-second guidance was in
    /// the dropped footer; the rate limit is what the `429` case covers.
    const UNDECLARED_TOOL: &str = include_str!("fixtures/sec_undeclared_tool.html");

    /// EDGAR's 404 body, captured from a company-concept URL for a tag the
    /// filer has never reported. The API is S3 underneath, so a missing
    /// document is XML — from a `.json` URL — and nothing downstream may depend
    /// on a 404 body parsing as JSON.
    const NO_SUCH_KEY: &str = include_str!("fixtures/sec_no_such_key.xml");

    /// What `Config::default()` sends. Not a credential: nothing issues it and
    /// nothing checks it.
    const DEFAULT_AGENT: &str = crate::config::defaults::SEC_USER_AGENT;

    fn submissions_fixture() -> Submissions {
        serde_json::from_str(SUBMISSIONS).expect("the captured fixture parses")
    }

    fn recent() -> SubmissionsFilings {
        submissions_fixture()
            .filings
            .and_then(|f| f.recent)
            .expect("the fixture carries recent filings")
    }

    fn recent_8k() -> SubmissionsFilings {
        serde_json::from_str::<Submissions>(SUBMISSIONS_8K)
            .expect("the captured fixture parses")
            .filings
            .and_then(|f| f.recent)
            .expect("the fixture carries recent filings")
    }

    fn state_for(server: &MockServer) -> AppState {
        AppState::new(Config {
            sec_data_base: server.uri(),
            sec_www_base: server.uri(),
            ..Config::default()
        })
    }

    fn state_with_agent(server: &MockServer, agent: &str) -> AppState {
        AppState::new(Config {
            sec_data_base: server.uri(),
            sec_www_base: server.uri(),
            sec_user_agent: agent.to_owned(),
            ..Config::default()
        })
    }

    /// The ticker file, which every lookup goes through.
    async fn mount_tickers(server: &MockServer) {
        Mock::given(method("GET"))
            .and(path_matcher("/files/company_tickers.json"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(TICKERS, "application/json"))
            .mount(server)
            .await;
    }

    async fn mount_json(server: &MockServer, path: &str, body: &'static str) {
        Mock::given(method("GET"))
            .and(path_matcher(path))
            .respond_with(ResponseTemplate::new(200).set_body_raw(body, "application/json"))
            .mount(server)
            .await;
    }

    /// The `User-Agent` every request carried, in the order they were sent.
    async fn agents_of(server: &MockServer) -> Vec<String> {
        server
            .received_requests()
            .await
            .expect("the mock server records requests")
            .iter()
            .map(|request| {
                request
                    .headers
                    .get("user-agent")
                    .and_then(|value| value.to_str().ok())
                    .unwrap_or_default()
                    .to_owned()
            })
            .collect()
    }

    /* ------------------------------------------------------------------ CIK */

    #[test]
    fn pads_a_cik_to_the_ten_digits_edgar_keys_on() {
        assert_eq!(pad_cik("320193"), "0000320193");
        assert_eq!(pad_cik("0000320193"), "0000320193");
        // A reader may paste the form EDGAR prints in its own URLs.
        assert_eq!(pad_cik("CIK0000320193"), "0000320193");
        assert_eq!(pad_cik("1,318,605"), "0001318605");
    }

    #[test]
    fn a_number_too_long_to_be_a_cik_is_left_alone_rather_than_cut_to_ten_digits() {
        // Truncating would address a *different* filer and return its filings
        // under the name the reader typed. Overlong is EDGAR's 404 to give.
        assert_eq!(pad_cik("12345678901"), "12345678901");
    }

    /* --------------------------------------------------------- the ticker file */

    #[tokio::test]
    async fn the_ticker_file_is_read_in_its_own_numeric_order_not_as_text() {
        // The file is a JSON object keyed by stringified index, ordered by
        // size, and serde reads it into a hash map — so the order has to be
        // rebuilt from the keys. Berkshire's B shares are row 9 and its A
        // shares row 7469; sorted as text, `7469` comes first and the terminal
        // offers a reader the $700,000 share class as the primary listing.
        let server = MockServer::start().await;
        mount_tickers(&server).await;

        let results = search(&state_for(&server), "berkshire", 5)
            .await
            .expect("the ticker file answers");

        assert_eq!(results.len(), 1, "one company, not one row per class");
        assert_eq!(results[0].ticker, "BRK-B");
        assert_eq!(results[0].cik, "0001067983");
        assert_eq!(results[0].name, "BERKSHIRE HATHAWAY INC");
    }

    #[tokio::test]
    async fn a_company_with_four_share_classes_is_one_row_out_and_four_ways_in() {
        // Alphabet files once and trades four ways. A search that listed the
        // same filer four times would push everything else off the panel, and a
        // reader who types GOOG rather than GOOGL still means Alphabet.
        let server = MockServer::start().await;
        mount_tickers(&server).await;
        let state = state_for(&server);

        let results = search(&state, "alphabet", 10)
            .await
            .expect("the ticker file answers");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].ticker, "GOOGL");

        for symbol in ["GOOGL", "goog", "GOOGM", "googn"] {
            assert_eq!(
                resolve_cik(&state, symbol).await.expect("a filer"),
                "0001652044",
                "{symbol}"
            );
        }
        // And by the CIK itself, padded or not, since EDGAR prints both.
        assert_eq!(resolve_cik(&state, "1652044").await.unwrap(), "0001652044");
        assert_eq!(
            resolve_cik(&state, "0001652044").await.unwrap(),
            "0001652044"
        );
    }

    #[tokio::test]
    async fn a_name_search_returns_every_filer_carrying_all_the_words_in_edgars_order() {
        // Three separate companies are called Apple something. The one a reader
        // means is almost always the largest, which is the one EDGAR lists
        // first, so file order is the ranking and must survive the hash map.
        let server = MockServer::start().await;
        mount_tickers(&server).await;
        let state = state_for(&server);

        let results = search(&state, "apple", 10)
            .await
            .expect("the ticker file answers");
        assert_eq!(
            results
                .iter()
                .map(|r| r.ticker.as_str())
                .collect::<Vec<_>>(),
            ["AAPL", "APLE", "AAPI"]
        );
        assert_eq!(results[0].name, "Apple Inc.");
        assert_eq!(results[2].name, "Apple iSports Group, Inc.");

        // A second word narrows it to the one filer whose name carries both.
        let narrowed = search(&state, "apple hospitality", 10)
            .await
            .expect("the ticker file answers");
        assert_eq!(narrowed.len(), 1);
        assert_eq!(narrowed[0].cik, "0001418121");

        // The limit cuts the list rather than the match.
        assert_eq!(search(&state, "apple", 2).await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn a_reader_who_types_a_symbol_is_searching_on_it_and_not_only_on_the_name() {
        // The search box takes whatever is typed and a symbol is the commonest
        // thing in it. `BRK-B` appears in no company name at all, so a search
        // that read names alone would answer nothing to the most specific query
        // a reader can make — and `AAPI` would match Apple Inc. rather than the
        // filer that trades under it.
        let server = MockServer::start().await;
        mount_tickers(&server).await;
        let state = state_for(&server);

        let berkshire = search(&state, "BRK-B", 10)
            .await
            .expect("the ticker file answers");
        assert_eq!(berkshire.len(), 1);
        assert_eq!(berkshire[0].name, "BERKSHIRE HATHAWAY INC");
        assert_eq!(berkshire[0].cik, "0001067983");

        let isports = search(&state, "aapi", 10)
            .await
            .expect("the ticker file answers");
        assert_eq!(isports.len(), 1);
        assert_eq!(isports[0].name, "Apple iSports Group, Inc.");
    }

    #[tokio::test]
    async fn resolving_a_name_takes_the_first_filer_that_carries_the_words_not_the_last() {
        let server = MockServer::start().await;
        mount_tickers(&server).await;

        // "Apple" alone is Apple Inc., not the REIT further down the file.
        assert_eq!(
            resolve_cik(&state_for(&server), "apple").await.unwrap(),
            "0000320193"
        );
    }

    #[tokio::test]
    async fn a_search_that_matches_nothing_is_an_empty_list_not_a_failure() {
        // A search panel showing nothing is a true answer. The same query as a
        // *resolution* is a failure, because a filings panel needs one filer
        // and cannot make one up.
        let server = MockServer::start().await;
        mount_tickers(&server).await;
        let state = state_for(&server);

        let results = search(&state, "hooli", 10)
            .await
            .expect("a miss is not a failure");
        assert!(results.is_empty());

        let error = resolve_cik(&state, "hooli")
            .await
            .expect_err("no such filer");
        assert_eq!(error.code, codes::NOT_FOUND);
    }

    #[tokio::test]
    async fn a_search_with_no_words_in_it_asks_edgar_nothing() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(TICKERS, "application/json"))
            .expect(0)
            .mount(&server)
            .await;
        let state = state_for(&server);

        assert!(search(&state, "", 10).await.expect("no query").is_empty());
        assert!(search(&state, "   ", 10)
            .await
            .expect("no query")
            .is_empty());
    }

    #[tokio::test]
    async fn a_search_with_no_room_in_it_returns_nothing_rather_than_one_result_anyway() {
        // `limit` is the panel's, and a board with no rows left wants no rows.
        let server = MockServer::start().await;
        mount_tickers(&server).await;

        assert!(search(&state_for(&server), "apple", 0)
            .await
            .expect("no room")
            .is_empty());
    }

    #[tokio::test]
    async fn a_ticker_row_missing_its_symbol_or_its_cik_is_skipped_rather_than_half_built() {
        // Built inline: the captured file has no such row, and the wire type
        // admits one. A half-built filer would carry an empty ticker, which is
        // a substring of every query, so it would match everything.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/files/company_tickers.json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "0": { "cik_str": 320193, "ticker": "AAPL", "title": "Apple Inc." },
                "1": { "cik_str": 1000045, "title": "NICHOLAS FINANCIAL INC" },
                "2": { "ticker": "ZZZZ", "title": "NO CIK AT ALL" }
            })))
            .mount(&server)
            .await;
        let state = state_for(&server);

        assert!(search(&state, "nicholas", 10).await.unwrap().is_empty());
        assert!(search(&state, "zzzz", 10).await.unwrap().is_empty());
        assert_eq!(resolve_cik(&state, "AAPL").await.unwrap(), "0000320193");
    }

    #[tokio::test]
    async fn the_ticker_file_is_fetched_once_and_shared_by_every_later_lookup() {
        // It is 800 KB and the only route from a symbol to a CIK; fetching it
        // per panel would cost more than everything else the terminal does.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/files/company_tickers.json"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(TICKERS, "application/json"))
            .expect(1)
            .mount(&server)
            .await;
        let state = state_for(&server);

        for query in ["AAPL", "tesla", "0000320193"] {
            resolve_cik(&state, query).await.expect("a filer");
        }
    }

    #[tokio::test]
    async fn an_empty_company_is_refused_before_the_ticker_file_is_touched() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(TICKERS, "application/json"))
            .expect(0)
            .mount(&server)
            .await;
        let state = state_for(&server);

        for query in ["", "   "] {
            let error = resolve_cik(&state, query).await.expect_err("no company");
            assert_eq!(error.code, codes::BAD_REQUEST);
            assert_eq!(error.message, "Missing company");
        }
    }

    #[tokio::test]
    async fn refuses_a_filer_it_cannot_place_rather_than_guessing_one() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/files/company_tickers.json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "0": { "cik_str": 320193, "ticker": "AAPL", "title": "Apple Inc." }
            })))
            .mount(&server)
            .await;

        let error = resolve_cik(&state_for(&server), "not a real company")
            .await
            .expect_err("no such filer");
        assert_eq!(error.code, "not_found");
        assert!(error.hint.unwrap_or_default().contains("ticker"));
    }

    #[tokio::test]
    async fn resolves_a_bare_number_as_a_cik_even_when_it_has_no_ticker() {
        // Plenty of filers — funds, individuals filing Form 4 — have a CIK and
        // no ticker at all, so the ticker file cannot be the only route in.
        let server = MockServer::start().await;
        mount_tickers(&server).await;

        let state = state_for(&server);
        assert_eq!(resolve_cik(&state, "1067983").await.unwrap(), "0001067983");
        assert_eq!(resolve_cik(&state, "AAPL").await.unwrap(), "0000320193");
        // A name works too, on the words the filer's own title carries.
        assert_eq!(resolve_cik(&state, "apple").await.unwrap(), "0000320193");
    }

    /* ---------------------------------------------------------- fair access */

    #[tokio::test]
    async fn every_request_declares_itself_because_edgar_answers_403_to_one_that_does_not() {
        // Measured live: no `User-Agent` is refused, any non-empty one is
        // served. Both hosts enforce it, so the ticker file on www.sec.gov and
        // the submissions document on data.sec.gov each have to carry it — and
        // this mock only answers a request that does.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/files/company_tickers.json"))
            .and(header("User-Agent", DEFAULT_AGENT))
            .respond_with(ResponseTemplate::new(200).set_body_raw(TICKERS, "application/json"))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path_matcher("/submissions/CIK0000320193.json"))
            .and(header("User-Agent", DEFAULT_AGENT))
            .respond_with(ResponseTemplate::new(200).set_body_raw(SUBMISSIONS, "application/json"))
            .mount(&server)
            .await;
        // Anything undeclared gets what EDGAR gives it.
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(403).set_body_raw(UNDECLARED_TOOL, "text/html"))
            .mount(&server)
            .await;

        let response = get_filings(&state_for(&server), "AAPL", 5, None)
            .await
            .expect("declared traffic is served");
        assert_eq!(response.company.name, "Apple Inc.");

        let agents = agents_of(&server).await;
        assert_eq!(agents.len(), 2, "the ticker file and the submissions");
        assert!(
            agents.iter().all(|agent| agent == DEFAULT_AGENT),
            "{agents:?}"
        );
    }

    #[tokio::test]
    async fn an_operators_own_contact_reaches_the_wire_trimmed_of_what_an_env_file_adds() {
        // What the policy actually asks for is a contact address, and an
        // operator sets one. Read from a file or a `.env` it commonly arrives
        // with a trailing newline — which cannot go in a header at all, and
        // would fail the request as a malformed header rather than as anything
        // a reader could act on.
        let server = MockServer::start().await;
        mount_tickers(&server).await;
        mount_json(&server, "/submissions/CIK0000320193.json", SUBMISSIONS).await;

        let state = state_with_agent(&server, "  Acme Research desk@acme.test\n");
        get_filings(&state, "AAPL", 1, None)
            .await
            .expect("the filings load");

        let agents = agents_of(&server).await;
        assert!(
            agents.iter().all(|a| a == "Acme Research desk@acme.test"),
            "{agents:?}"
        );
    }

    #[tokio::test]
    async fn a_blank_contact_falls_back_to_this_projects_own_rather_than_sending_none() {
        // `SEC_USER_AGENT=` in a deployment's environment is an empty string,
        // not an absent setting. Sending it verbatim is a 403 on every panel.
        let server = MockServer::start().await;
        mount_tickers(&server).await;

        let state = state_with_agent(&server, "   ");
        resolve_cik(&state, "AAPL").await.expect("a filer");

        assert_eq!(
            agents_of(&server).await,
            ["prediction-terminal contact@example.com"]
        );
    }

    #[tokio::test]
    async fn edgars_refusal_of_undeclared_traffic_is_reported_as_a_refusal_not_as_no_filings() {
        // The captured page is what a reader would be shown nothing about if
        // this became an empty filings list: it names the cause — traffic that
        // does not declare itself — and the remedy, which is a header naming
        // the caller. Neither survives an empty list.
        let server = MockServer::start().await;
        mount_tickers(&server).await;
        Mock::given(method("GET"))
            .and(path_matcher("/submissions/CIK0000320193.json"))
            .respond_with(ResponseTemplate::new(403).set_body_raw(UNDECLARED_TOOL, "text/html"))
            // A 403 is a decision, not a wobble: asking again gets the same
            // answer more slowly.
            .expect(1)
            .mount(&server)
            .await;

        let error = get_filings(&state_for(&server), "AAPL", 5, None)
            .await
            .expect_err("a refusal is not an empty list");

        assert_eq!(error.code, codes::UPSTREAM_STATUS);
        assert_eq!(error.status, Some(403));
        assert!(error.hint.expect("a hint").contains("403"));
    }

    #[tokio::test]
    async fn a_throttled_request_is_retried_once_and_then_reported_as_rate_limiting() {
        // EDGAR limits every caller to ten requests a second. Reported as an
        // outage it reads as "the SEC is down"; reported as an empty list it
        // reads as "this filer has filed nothing". It is neither, and the hint
        // has to say to wait.
        let server = MockServer::start().await;
        mount_tickers(&server).await;
        Mock::given(method("GET"))
            .and(path_matcher("/submissions/CIK0000320193.json"))
            .respond_with(ResponseTemplate::new(429).set_body_raw(UNDECLARED_TOOL, "text/html"))
            // Retryable, and `RETRIES` is 1: one attempt and one retry.
            .expect(2)
            .mount(&server)
            .await;

        let error = get_filings(&state_for(&server), "AAPL", 5, None)
            .await
            .expect_err("a throttle is not an empty list");

        assert_eq!(error.status, Some(429));
        let hint = error.hint.expect("a hint");
        assert!(hint.contains("rate-limiting"), "{hint}");
        assert!(hint.contains("retry"), "{hint}");
    }

    /* -------------------------------------------------------------- the zip */

    #[test]
    fn a_short_array_yields_an_empty_field_rather_than_shifting_every_later_filing() {
        // This is the defect the function exists to prevent. `items` applies
        // only to 8-Ks, so EDGAR sends a shorter array — and zipping by index
        // without allowing for that would attribute filing 5's form to filing 4
        // and be wrong about every one after it, silently.
        let filings = recent();
        assert!(
            filings.items.len() < filings.accession_number.len(),
            "the fixture must keep the short-array case"
        );

        let zipped = zip_filings(&filings, "0000320193", "https://www.sec.gov", 100, None);
        assert_eq!(zipped.len(), filings.accession_number.len());

        for (i, filing) in zipped.iter().enumerate() {
            // Every filing keeps its *own* form and date, not its neighbour's.
            assert_eq!(filing.accession, filings.accession_number[i]);
            assert_eq!(filing.form, filings.form[i]);
            assert_eq!(filing.filed, filings.filing_date[i]);
        }
        // And the ones past the short array simply have no items.
        assert!(zipped[filings.items.len()].items.is_empty());

        // That last assertion cannot on its own tell "past the end" from "the
        // value that is there", because every one of this document's four
        // captured `items` is an empty string. The 8-K window carries real ones,
        // so cutting it short is what proves the difference: filing 8 keeps its
        // own items and filing 10 gets nothing rather than filing 8's.
        let mut short = recent_8k();
        assert_eq!(short.items[8], "8.01,9.01");
        assert_eq!(short.items[10], "2.02,9.01");
        short.items.truncate(9);

        let zipped = zip_filings(&short, "0000320193", "https://www.sec.gov", 50, None);
        assert_eq!(zipped[8].items, "8.01,9.01");
        assert_eq!(zipped[10].items, "");

        // And on a field where every captured row differs, so that neither the
        // neighbour's value nor the first one can pass for absent.
        let mut short = recent_8k();
        short.primary_doc_description.truncate(6);
        let zipped = zip_filings(&short, "0000320193", "https://www.sec.gov", 50, None);
        assert_eq!(zipped[5].description, "FORM 4");
        assert_eq!(zipped[6].description, "");
    }

    #[test]
    fn filters_to_one_form_without_disturbing_the_alignment() {
        let filings = recent();
        let eight_ks = zip_filings(
            &filings,
            "0000320193",
            "https://www.sec.gov",
            100,
            Some("8-K"),
        );
        assert!(!eight_ks.is_empty(), "the fixture holds an 8-K");
        assert!(eight_ks.iter().all(|f| f.form == "8-K"));

        // The filter is case-insensitive, because nobody types `8-K` twice the
        // same way.
        let lower = zip_filings(
            &filings,
            "0000320193",
            "https://www.sec.gov",
            100,
            Some("8-k"),
        );
        assert_eq!(lower.len(), eight_ks.len());
    }

    #[test]
    fn builds_an_archive_url_with_the_unpadded_cik_and_the_bare_accession() {
        // EDGAR's archive paths use the CIK *without* its leading zeros and the
        // accession *without* its dashes. Either one wrong is a 404.
        let filings = recent();
        let zipped = zip_filings(&filings, "0000320193", "https://www.sec.gov", 1, None);
        let filing = &zipped[0];

        assert!(
            filing.url.contains("/data/320193/"),
            "url was {}",
            filing.url
        );
        assert!(!filing.url.contains("0000320193"), "url was {}", filing.url);
        assert!(
            filing.url.contains(&filing.accession.replace('-', "")),
            "url was {}",
            filing.url
        );
    }

    #[test]
    fn honours_the_limit() {
        let filings = recent();
        assert_eq!(
            zip_filings(&filings, "0000320193", "https://www.sec.gov", 3, None).len(),
            3
        );
    }

    #[test]
    fn a_filing_with_no_primary_document_links_to_its_folder_rather_than_to_a_dead_file() {
        // A handful of older filings carry no primary document, and appending
        // an empty name produces a URL with a trailing slash that 404s. The
        // folder itself is a real page listing everything in the filing.
        let mut filings = recent();
        filings.primary_document[0] = String::new();

        let zipped = zip_filings(&filings, "0000320193", "https://www.sec.gov", 1, None);
        assert_eq!(
            zipped[0].url,
            "https://www.sec.gov/Archives/edgar/data/320193/000114036126033928/"
        );
    }

    #[test]
    fn a_filing_edgar_states_no_size_for_is_none_rather_than_a_zero_byte_document() {
        // Nothing here may invent a figure the publisher did not state: a
        // filing of zero bytes is a broken document, which is a different
        // claim from "EDGAR did not say".
        let mut filings = recent();
        filings.size.truncate(3);

        let zipped = zip_filings(&filings, "0000320193", "https://www.sec.gov", 5, None);
        assert_eq!(zipped[2].size, Some(4966.0));
        assert_eq!(zipped[3].size, None);
    }

    #[test]
    fn a_null_in_the_size_array_costs_that_one_figure_and_not_the_whole_document() {
        // The null element is written here rather than captured — no filer's
        // `size` currently carries one — but `isXBRLNumeric` in the very same
        // object is 979 nulls in 1,001, so this is a shape EDGAR uses in these
        // parallel arrays and not an invented one. Read into a `Vec<f64>` it
        // would fail the whole submissions document, and a reader asking for
        // Apple's filings would be told the answer was unreadable because one
        // filing had no byte count.
        let filings: SubmissionsFilings = serde_json::from_value(json!({
            "accessionNumber": ["0000320193-18-000067", "0000320193-18-000064"],
            "form": ["8-K", "4"],
            "size": [401_862, null]
        }))
        .expect("a null element is a missing figure, not a broken document");

        let zipped = zip_filings(&filings, "0000320193", "https://www.sec.gov", 5, None);
        assert_eq!(zipped[0].size, Some(401_862.0));
        assert_eq!(zipped[1].size, None);
        assert_eq!(zipped[1].form, "4");
    }

    #[test]
    fn an_eight_k_filter_does_not_sweep_in_the_amendment_that_corrects_it() {
        // An 8-K and its 8-K/A are different disclosures filed on different
        // days, and the amendment is often the news. Folding the two together
        // would date a correction to the original's report date.
        let filings = recent_8k();

        let eight_ks = zip_filings(
            &filings,
            "0000320193",
            "https://www.sec.gov",
            50,
            Some("8-K"),
        );
        assert_eq!(eight_ks.len(), 2);
        assert_eq!(eight_ks[0].accession, "0001193125-18-154515");
        assert_eq!(eight_ks[1].accession, "0000320193-18-000067");

        let amendments = zip_filings(
            &filings,
            "0000320193",
            "https://www.sec.gov",
            50,
            Some("8-K/A"),
        );
        assert_eq!(amendments.len(), 1);
        assert_eq!(amendments[0].accession, "0001193125-18-154948");
        assert_eq!(amendments[0].description, "AMENDMENT NO. 1 TO FORM 8-K");
        // The amendment reports on the same day the original did.
        assert_eq!(amendments[0].report_date, "2018-04-30");
        assert_eq!(amendments[0].filed, "2018-05-08");
    }

    #[test]
    fn an_eight_k_carries_the_items_it_cites_because_the_item_is_what_names_the_event() {
        // Item 2.02 is results of operations — the earnings release a market
        // settles on — and 5.02 is a director or officer leaving. Losing the
        // items makes every 8-K look alike.
        let filings = recent_8k();
        let zipped = zip_filings(&filings, "0000320193", "https://www.sec.gov", 50, None);

        let earnings = &zipped[10];
        assert_eq!(earnings.accession, "0000320193-18-000067");
        assert_eq!(earnings.items, "2.02,9.01");
        assert_eq!(earnings.filed, "2018-05-01");
        assert_eq!(earnings.size, Some(401_862.0));
        assert_eq!(
            earnings.url,
            "https://www.sec.gov/Archives/edgar/data/320193/000032019318000067/\
             a8-kq220183312018.htm"
        );
        assert_eq!(zipped[8].items, "8.01,9.01");
        // A Form 4 cites no items at all, which is an empty string of its own
        // rather than the neighbouring 8-K's.
        assert_eq!(zipped[11].form, "4");
        assert_eq!(zipped[11].items, "");
    }

    /* ------------------------------------------------------------ filings */

    #[tokio::test]
    async fn reads_a_filers_recent_filings_and_its_identity() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/files/company_tickers.json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "0": { "cik_str": 320193, "ticker": "AAPL", "title": "Apple Inc." },
                "1": { "cik_str": 1318605, "ticker": "TSLA", "title": "Tesla, Inc." }
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path_matcher("/submissions/CIK0000320193.json"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(SUBMISSIONS, "application/json"))
            .mount(&server)
            .await;

        let state = state_for(&server);
        let response = get_filings(&state, "aapl", 5, None)
            .await
            .expect("the filings load");

        assert_eq!(response.company.cik, "0000320193");
        assert_eq!(response.company.name, "Apple Inc.");
        assert_eq!(response.filings.len(), 5);
        assert!(!response.forms.is_empty());
        // The form list is deduplicated and sorted, so a panel can offer it as a
        // filter without doing that work again.
        let mut sorted = response.forms.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted, response.forms);
    }

    #[tokio::test]
    async fn reads_the_filers_own_description_of_itself_off_the_same_document() {
        // The SIC code and its wording are the SEC's classification of the
        // filer, and the panel heads the filings with them rather than with the
        // reader's query.
        let server = MockServer::start().await;
        mount_tickers(&server).await;
        mount_json(&server, "/submissions/CIK0000320193.json", SUBMISSIONS_8K).await;

        let response = get_filings(&state_for(&server), "AAPL", 40, None)
            .await
            .expect("the filings load");

        assert_eq!(response.company.sic, "3571");
        assert_eq!(response.company.sic_description, "Electronic Computers");
        assert_eq!(response.company.fiscal_year_end, "0926");
        assert_eq!(response.company.tickers, ["AAPL"]);
        assert_eq!(response.company.exchanges, ["Nasdaq"]);
        // Newest first, as EDGAR orders them: a filings panel is a wire.
        assert_eq!(response.filings.len(), 13);
        assert_eq!(response.filings[0].filed, "2018-05-25");
        assert_eq!(response.filings[12].filed, "2018-04-17");
        assert_eq!(response.forms, ["10-Q", "4", "8-K", "8-K/A"]);
    }

    #[tokio::test]
    async fn a_form_this_filer_has_not_filed_recently_is_a_not_found_that_names_what_it_has() {
        // Answering an empty list would read as "Apple has never filed a
        // 13F-HR", which is a claim about the company. The truth is a claim
        // about the document: it holds the last thousand filings and none of
        // them is one.
        let server = MockServer::start().await;
        mount_tickers(&server).await;
        mount_json(&server, "/submissions/CIK0000320193.json", SUBMISSIONS_8K).await;

        let error = get_filings(&state_for(&server), "AAPL", 40, Some("13F-HR"))
            .await
            .expect_err("no such form here");

        assert_eq!(error.code, codes::NOT_FOUND);
        assert_eq!(error.message, "Apple Inc. has filed no recent 13F-HR");
        let hint = error.hint.expect("a hint");
        assert!(hint.contains("~1,000 filings"), "{hint}");
        assert!(hint.contains("10-Q, 4, 8-K, 8-K/A"), "{hint}");
    }

    #[tokio::test]
    async fn the_form_filter_does_not_shrink_the_list_of_forms_the_panel_offers() {
        // The filter list has to describe the document, not the current
        // selection, or picking 8-K once would leave a panel with no way back.
        let server = MockServer::start().await;
        mount_tickers(&server).await;
        mount_json(&server, "/submissions/CIK0000320193.json", SUBMISSIONS_8K).await;

        let response = get_filings(&state_for(&server), "AAPL", 40, Some("8-K"))
            .await
            .expect("the filings load");

        assert_eq!(response.filings.len(), 2);
        assert_eq!(response.forms, ["10-Q", "4", "8-K", "8-K/A"]);
    }

    #[tokio::test]
    async fn the_older_filings_edgar_keeps_in_a_second_document_are_not_quietly_folded_in() {
        // `filings.files` points at 1,240 more filings from 1994 to 2015 in a
        // file this module does not fetch. Absent and said so is a workable
        // answer; absent and unmentioned is a reader concluding Apple filed
        // nothing before 2018.
        let raw: serde_json::Value =
            serde_json::from_str(SUBMISSIONS_8K).expect("the fixture parses");
        assert_eq!(raw["filings"]["files"][0]["filingCount"], 1240);
        assert_eq!(raw["filings"]["files"][0]["filingTo"], "2015-06-02");

        let server = MockServer::start().await;
        mount_tickers(&server).await;
        mount_json(&server, "/submissions/CIK0000320193.json", SUBMISSIONS_8K).await;

        let response = get_filings(&state_for(&server), "AAPL", 250, None)
            .await
            .expect("the filings load");

        // Only what `recent` carries, and nothing invented from the pointer.
        assert_eq!(response.filings.len(), 13);
        assert!(response
            .filings
            .iter()
            .all(|filing| filing.filed.as_str() >= "2018-04-17"));
        // One document was read, so one request was made.
        assert_eq!(
            server
                .received_requests()
                .await
                .expect("requests are recorded")
                .len(),
            2
        );
    }

    #[tokio::test]
    async fn a_cik_edgar_never_assigned_is_a_not_found_rather_than_a_filer_with_nothing_on_file() {
        // EDGAR's 404 body is S3's XML, from a `.json` URL. The status decides
        // before the body is read, so the reader is told the filer does not
        // exist rather than that the answer would not parse.
        let server = MockServer::start().await;
        mount_tickers(&server).await;
        Mock::given(method("GET"))
            .and(path_matcher("/submissions/CIK0009999999.json"))
            .respond_with(ResponseTemplate::new(404).set_body_raw(NO_SUCH_KEY, "application/xml"))
            .mount(&server)
            .await;

        let error = get_filings(&state_for(&server), "9999999", 5, None)
            .await
            .expect_err("no such filer");

        assert_eq!(error.code, codes::NOT_FOUND);
        assert_eq!(error.status, Some(404));
    }

    #[tokio::test]
    async fn a_submissions_document_with_no_filings_block_is_an_empty_list_not_a_panic() {
        // A filer that has registered and filed nothing yet, and any future
        // document that stops carrying `filings`, both arrive as a plausible
        // 200. Every field this module reads is optional for exactly this.
        let server = MockServer::start().await;
        mount_tickers(&server).await;
        Mock::given(method("GET"))
            .and(path_matcher("/submissions/CIK0009999999.json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "cik": "0009999999",
                "name": "A FILER WITH NOTHING ON FILE"
            })))
            .mount(&server)
            .await;

        let response = get_filings(&state_for(&server), "9999999", 40, None)
            .await
            .expect("an empty document still describes a filer");

        assert!(response.filings.is_empty());
        assert!(response.forms.is_empty());
        assert_eq!(response.company.name, "A FILER WITH NOTHING ON FILE");
        // Absent is empty, not a made-up code.
        assert_eq!(response.company.sic, "");
        assert_eq!(response.company.fiscal_year_end, "");
        assert!(response.company.tickers.is_empty());
    }

    #[tokio::test]
    async fn an_error_page_served_as_a_two_hundred_is_a_bad_body_not_a_filer_with_no_filings() {
        // Akamai sits in front of both hosts and can answer a JSON URL with
        // HTML. Reported as an empty document it becomes a claim about Apple.
        let server = MockServer::start().await;
        mount_tickers(&server).await;
        Mock::given(method("GET"))
            .and(path_matcher("/submissions/CIK0000320193.json"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(UNDECLARED_TOOL, "text/html"))
            .mount(&server)
            .await;

        let error = get_filings(&state_for(&server), "AAPL", 5, None)
            .await
            .expect_err("HTML is not a submissions document");

        assert_eq!(error.code, codes::BAD_UPSTREAM_BODY);
    }

    #[tokio::test]
    async fn the_archive_links_point_at_the_www_host_and_the_lookups_at_the_data_host() {
        // EDGAR is two hosts: `data.sec.gov` answers the APIs and serves no
        // documents, `www.sec.gov` serves the documents and the ticker file.
        // Building an archive URL on the data host 404s every link in the
        // panel.
        let data = MockServer::start().await;
        let www = MockServer::start().await;
        mount_tickers(&www).await;
        mount_json(&data, "/submissions/CIK0000320193.json", SUBMISSIONS_8K).await;

        let state = AppState::new(Config {
            sec_data_base: data.uri(),
            sec_www_base: www.uri(),
            ..Config::default()
        });
        let response = get_filings(&state, "AAPL", 5, None)
            .await
            .expect("the filings load");

        assert_eq!(
            response.company.url,
            format!(
                "{}/cgi-bin/browse-edgar?action=getcompany&CIK=0000320193&type=&dateb=&\
                 owner=include&count=40",
                www.uri()
            )
        );
        assert!(
            response.filings[0]
                .url
                .starts_with(&format!("{}/Archives/", www.uri())),
            "{}",
            response.filings[0].url
        );
        assert_eq!(
            data.received_requests().await.expect("recorded").len(),
            1,
            "the data host answers the submissions and nothing else"
        );
    }

    #[tokio::test]
    async fn a_second_panel_on_the_same_filer_costs_no_second_request() {
        // Filings, a form filter and a concept panel all want the same
        // document, and EDGAR's ten-a-second limit is shared by every panel.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/files/company_tickers.json"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(TICKERS, "application/json"))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path_matcher("/submissions/CIK0000320193.json"))
            .respond_with(
                ResponseTemplate::new(200).set_body_raw(SUBMISSIONS_8K, "application/json"),
            )
            .expect(1)
            .mount(&server)
            .await;

        let state = state_for(&server);
        get_filings(&state, "AAPL", 5, None).await.expect("filings");
        get_filings(&state, "aapl", 40, Some("8-K"))
            .await
            .expect("filings");
        get_filings(&state, "0000320193", 1, None)
            .await
            .expect("filings");
    }

    #[tokio::test]
    async fn two_filers_do_not_share_one_cached_submissions_document() {
        // The other half of the cache key. Sharing one entry between filers is
        // not a slow panel but a wrong one: the second company a reader opens
        // would be headed by the first one's name and listed with its filings,
        // and nothing on the screen would say so. Tesla's document is built
        // inline because only the identity is under test here.
        let server = MockServer::start().await;
        mount_tickers(&server).await;
        mount_json(&server, "/submissions/CIK0000320193.json", SUBMISSIONS).await;
        Mock::given(method("GET"))
            .and(path_matcher("/submissions/CIK0001318605.json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "cik": "0001318605",
                "name": "Tesla, Inc.",
                "tickers": ["TSLA"],
                "sic": "3711",
                "sicDescription": "Motor Vehicles & Passenger Car Bodies"
            })))
            .mount(&server)
            .await;
        let state = state_for(&server);

        let apple = get_filings(&state, "AAPL", 5, None).await.expect("filings");
        let tesla = get_filings(&state, "TSLA", 5, None).await.expect("filings");

        assert_eq!(apple.company.name, "Apple Inc.");
        assert_eq!(apple.company.cik, "0000320193");
        assert_eq!(apple.filings.len(), 5);
        assert_eq!(tesla.company.name, "Tesla, Inc.");
        assert_eq!(tesla.company.cik, "0001318605");
        assert_eq!(tesla.company.sic, "3711");
        assert!(tesla.filings.is_empty());
    }

    /* --------------------------------------------------------- xbrl concepts */

    #[tokio::test]
    async fn refuses_something_that_is_not_an_xbrl_tag_before_spending_a_request() {
        let server = MockServer::start().await;
        let state = state_for(&server);
        for bad in ["", "9Revenues", "Revenues; DROP", "a b"] {
            let error = get_concept(&state, "AAPL", bad, "us-gaap")
                .await
                .expect_err("not a tag");
            assert_eq!(error.code, "bad_request", "accepted {bad:?}");
        }
    }

    #[test]
    fn reads_a_camel_case_tag_and_refuses_everything_that_would_leave_the_path() {
        // The tag becomes a path segment. A dot or a slash in it addresses a
        // different document, so the check is the only thing between a typo and
        // an arbitrary fetch.
        assert!(is_tag("Revenues"));
        assert!(is_tag("EarningsPerShareDiluted"));
        assert!(is_tag("AccountsPayableCurrent"));
        assert!(!is_tag("../../submissions/CIK0000320193"));
        assert!(!is_tag("Revenues.json"));
        // One character is a typo, not a tag; 121 is the taxonomy's own bound.
        assert!(!is_tag("R"));
        assert!(is_tag(&"A".repeat(121)));
        assert!(!is_tag(&"A".repeat(122)));
    }

    async fn mount_concept(server: &MockServer, path: &str, body: &'static str) {
        mount_tickers(server).await;
        mount_json(server, "/submissions/CIK0000320193.json", SUBMISSIONS).await;
        mount_json(server, path, body).await;
    }

    #[tokio::test]
    async fn the_same_period_reported_twice_stays_two_facts_because_the_change_is_the_point() {
        // Apple's FY2008 balance sheet was filed at $39.572bn of total assets
        // and restated to $36.171bn by the 10-K/A of 2010-01-25. Collapsing the
        // period to one number answers "what are the assets" and destroys "what
        // did they say, and when did they change it" — which is the only reason
        // this surface exists.
        let server = MockServer::start().await;
        mount_concept(
            &server,
            "/api/xbrl/companyconcept/CIK0000320193/us-gaap/Assets.json",
            ASSETS,
        )
        .await;

        let answer = get_concept(&state_for(&server), "AAPL", "Assets", "us-gaap")
            .await
            .expect("the concept loads");

        assert_eq!(answer.facts.len(), 8);
        let fy2008: Vec<&SecFact> = answer
            .facts
            .iter()
            .filter(|fact| fact.end == "2008-09-27")
            .collect();
        assert_eq!(fy2008.len(), 4, "four filings reported that year end");
        assert_eq!(
            fy2008.iter().map(|f| f.value).collect::<Vec<_>>(),
            [
                39_572_000_000.0,
                39_572_000_000.0,
                36_171_000_000.0,
                36_171_000_000.0
            ]
        );

        // Each value names the filing that carried it, so the correction is
        // dated rather than merely present.
        assert_eq!(fy2008[1].form, "10-K");
        assert_eq!(fy2008[1].filed, "2009-10-27");
        assert_eq!(fy2008[2].form, "10-K/A");
        assert_eq!(fy2008[2].filed, "2010-01-25");
        assert_eq!(fy2008[2].accession, "0001193125-10-012091");
        assert_eq!(fy2008[2].fiscal_year, Some(2009));
        assert_eq!(fy2008[2].fiscal_period, "FY");

        assert_eq!(answer.unit, "USD");
        assert_eq!(answer.tag, "Assets");
        assert_eq!(answer.label, "Assets");
        assert!(answer
            .description
            .starts_with("Sum of the carrying amounts"));
        // The company block comes from the submissions document beside it.
        assert_eq!(answer.company.name, "Apple Inc.");
    }

    #[tokio::test]
    async fn an_instant_fact_has_no_start_and_is_still_a_fact() {
        // Every balance-sheet concept is an instant, and EDGAR sends no `start`
        // key at all for one. Requiring a period start would drop all 146 of
        // Apple's `Assets` facts and leave a panel that looks like a filer who
        // never reported its assets.
        let server = MockServer::start().await;
        mount_concept(
            &server,
            "/api/xbrl/companyconcept/CIK0000320193/us-gaap/Assets.json",
            ASSETS,
        )
        .await;

        let answer = get_concept(&state_for(&server), "AAPL", "Assets", "us-gaap")
            .await
            .expect("the concept loads");

        assert!(answer.facts.iter().all(|fact| fact.start.is_empty()));
        assert_eq!(answer.facts[4].end, "2009-06-27");
        assert_eq!(answer.facts[4].value, 48_140_000_000.0);
    }

    #[tokio::test]
    async fn two_facts_filed_on_one_day_keep_the_order_edgar_sent_them_in() {
        // 2009-09-26 was restated twice on 2010-01-25 — by a 10-Q and by a
        // 10-K/A. The sort has nothing left to order them by, so it must leave
        // them alone; an unstable one would swap them between runs and make the
        // panel's own history look unstable.
        let server = MockServer::start().await;
        mount_concept(
            &server,
            "/api/xbrl/companyconcept/CIK0000320193/us-gaap/Assets.json",
            ASSETS,
        )
        .await;

        let answer = get_concept(&state_for(&server), "AAPL", "Assets", "us-gaap")
            .await
            .expect("the concept loads");

        assert_eq!(answer.facts[6].filed, "2010-01-25");
        assert_eq!(answer.facts[6].form, "10-Q");
        assert_eq!(answer.facts[6].accession, "0001193125-10-012085");
        assert_eq!(answer.facts[7].filed, "2010-01-25");
        assert_eq!(answer.facts[7].form, "10-K/A");
        assert_eq!(answer.facts[7].accession, "0001193125-10-012091");
    }

    #[tokio::test]
    async fn facts_are_ordered_by_period_then_by_filing_whatever_order_edgar_sends() {
        // EDGAR groups this concept by period *start*, so the wire order runs
        // the five annual figures and then the two quarterly ones. A reader
        // wants the period and then the date the figure was stated.
        let wire: Vec<f64> = serde_json::from_str::<serde_json::Value>(EPS)
            .expect("the fixture parses")["units"]["USD/shares"]
            .as_array()
            .expect("an array of facts")
            .iter()
            .map(|fact| fact["val"].as_f64().expect("a value"))
            .collect();
        assert_eq!(wire, [44.15, 44.15, 44.15, 6.31, 6.31, 8.67, 8.67]);

        let server = MockServer::start().await;
        mount_concept(
            &server,
            "/api/xbrl/companyconcept/CIK0000320193/us-gaap/EarningsPerShareDiluted.json",
            EPS,
        )
        .await;

        let answer = get_concept(
            &state_for(&server),
            "AAPL",
            "EarningsPerShareDiluted",
            "us-gaap",
        )
        .await
        .expect("the concept loads");

        assert_eq!(
            answer.facts.iter().map(|f| f.value).collect::<Vec<_>>(),
            [44.15, 8.67, 44.15, 8.67, 44.15, 6.31, 6.31]
        );
        assert_eq!(
            answer
                .facts
                .iter()
                .map(|f| f.filed.as_str())
                .collect::<Vec<_>>(),
            [
                "2012-10-31",
                "2012-10-31",
                "2013-04-24",
                "2013-04-24",
                "2013-10-30",
                "2014-10-27",
                "2015-01-28"
            ]
        );
    }

    #[tokio::test]
    async fn the_year_and_the_quarter_that_ends_with_it_are_told_apart_by_their_start() {
        // Apple's FY2012 and its fourth quarter both end 2012-09-29: $44.15 is
        // the year, $8.67 the quarter. They are two facts about one date and
        // the period start is the only thing separating them — a chart keyed on
        // the end alone would draw the quarter as a collapse in earnings.
        let server = MockServer::start().await;
        mount_concept(
            &server,
            "/api/xbrl/companyconcept/CIK0000320193/us-gaap/EarningsPerShareDiluted.json",
            EPS,
        )
        .await;

        let answer = get_concept(
            &state_for(&server),
            "AAPL",
            "EarningsPerShareDiluted",
            "us-gaap",
        )
        .await
        .expect("the concept loads");

        assert!(answer.facts.iter().all(|fact| fact.end == "2012-09-29"));
        assert_eq!(answer.facts[0].start, "2011-09-25");
        assert_eq!(answer.facts[0].value, 44.15);
        assert_eq!(answer.facts[1].start, "2012-07-01");
        assert_eq!(answer.facts[1].value, 8.67);
    }

    #[tokio::test]
    async fn a_split_restatement_keeps_both_figures_because_neither_is_wrong() {
        // The 7-for-1 split of 2014 restated FY2012 diluted EPS from $44.15 to
        // $6.31. Both are what Apple reported, and a market on an earnings
        // print that read only the latest would compare a split-adjusted figure
        // with an unadjusted expectation.
        let server = MockServer::start().await;
        mount_concept(
            &server,
            "/api/xbrl/companyconcept/CIK0000320193/us-gaap/EarningsPerShareDiluted.json",
            EPS,
        )
        .await;

        let answer = get_concept(
            &state_for(&server),
            "AAPL",
            "EarningsPerShareDiluted",
            "us-gaap",
        )
        .await
        .expect("the concept loads");

        let annual: Vec<&SecFact> = answer
            .facts
            .iter()
            .filter(|fact| fact.start == "2011-09-25")
            .collect();
        assert_eq!(annual.len(), 5);
        assert_eq!(annual[0].value, 44.15);
        assert_eq!(annual[0].filed, "2012-10-31");
        assert_eq!(annual[4].value, 6.31);
        assert_eq!(annual[4].filed, "2015-01-28");
        assert_eq!(answer.unit, "USD/shares");
    }

    #[tokio::test]
    async fn a_fiscal_year_edgar_leaves_null_stays_null_rather_than_becoming_year_zero() {
        // The 8-K of 2015-01-28 restated four years of EPS for the split, and
        // EDGAR files it under no fiscal year and no fiscal period at all —
        // `"fy": null, "fp": null` in the captured payload. A zero would date
        // it to year nought and a made-up quarter would file it under one.
        let server = MockServer::start().await;
        mount_concept(
            &server,
            "/api/xbrl/companyconcept/CIK0000320193/us-gaap/EarningsPerShareDiluted.json",
            EPS,
        )
        .await;

        let answer = get_concept(
            &state_for(&server),
            "AAPL",
            "EarningsPerShareDiluted",
            "us-gaap",
        )
        .await
        .expect("the concept loads");

        let last = answer.facts.last().expect("a fact");
        assert_eq!(last.filed, "2015-01-28");
        assert_eq!(last.form, "8-K");
        assert_eq!(last.fiscal_year, None);
        assert_eq!(last.fiscal_period, "");
        // The ones that do carry a fiscal year keep it.
        assert_eq!(answer.facts[0].fiscal_year, Some(2012));
        assert_eq!(answer.facts[0].fiscal_period, "FY");
    }

    #[tokio::test]
    async fn a_concept_reported_in_two_currencies_charts_one_of_them_rather_than_both() {
        // Toyota reports `Revenues` in yen and in dollars in the same document.
        // Merging the units puts ¥18.95tn and $203.7bn on one axis and makes
        // the yen series look like a step change; picking whichever key the
        // hash map yields first makes the panel change currency between runs.
        let server = MockServer::start().await;
        mount_tickers(&server).await;
        Mock::given(method("GET"))
            .and(path_matcher("/submissions/CIK0001094517.json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "cik": "0001094517", "name": "TOYOTA MOTOR CORP/"
            })))
            .mount(&server)
            .await;
        mount_json(
            &server,
            "/api/xbrl/companyconcept/CIK0001094517/us-gaap/Revenues.json",
            TOYOTA,
        )
        .await;

        let answer = get_concept(&state_for(&server), "1094517", "Revenues", "us-gaap")
            .await
            .expect("the concept loads");

        assert_eq!(answer.unit, "JPY", "the richer series wins");
        assert_eq!(answer.facts.len(), 5);
        assert!(answer.facts.iter().all(|fact| fact.unit == "JPY"));
        assert_eq!(answer.facts[0].value, 18_950_973_000_000.0);
        assert_eq!(answer.facts[4].value, 18_993_688_000_000.0);
        assert!(
            !answer
                .facts
                .iter()
                .any(|fact| fact.value == 203_687_000_000.0),
            "the dollar figures belong to the other unit"
        );
        // The same year is filed with a period start a day apart across
        // filings; both are kept, each dated by the filing that carried it.
        assert_eq!(answer.facts[0].start, "2009-04-01");
        assert_eq!(answer.facts[0].filed, "2010-07-22");
        assert_eq!(answer.facts[1].start, "2009-03-31");
        assert_eq!(answer.facts[1].filed, "2011-07-20");
    }

    #[tokio::test]
    async fn a_tie_between_units_is_broken_by_name_rather_than_by_hash_order() {
        // Built inline: the captured payloads have no tie. `units` is a JSON
        // object read into a hash map, whose iteration order differs between
        // runs, so without the tie-break a reader would see Canadian dollars
        // one day and US dollars the next with nothing having changed.
        for _ in 0..10 {
            let fresh = MockServer::start().await;
            mount_tickers(&fresh).await;
            mount_json(&fresh, "/submissions/CIK0000320193.json", SUBMISSIONS).await;
            Mock::given(method("GET"))
                .and(path_matcher(
                    "/api/xbrl/companyconcept/CIK0000320193/us-gaap/Revenues.json",
                ))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "taxonomy": "us-gaap",
                    "tag": "Revenues",
                    "units": {
                        "USD": [{ "start": "2024-01-01", "end": "2024-12-31", "val": 1.0 }],
                        "CAD": [{ "start": "2024-01-01", "end": "2024-12-31", "val": 2.0 }]
                    }
                })))
                .mount(&fresh)
                .await;

            let answer = get_concept(&state_for(&fresh), "AAPL", "Revenues", "us-gaap")
                .await
                .expect("the concept loads");
            assert_eq!(answer.unit, "CAD");
            assert_eq!(answer.facts[0].value, 2.0);
        }
    }

    #[tokio::test]
    async fn a_fact_with_no_value_or_no_period_end_is_dropped_rather_than_charted_as_zero() {
        // Built inline from the shapes the wire type admits. A diluted EPS of
        // zero is a company that broke even, which is a thing a reader would
        // act on and is not what a missing value means; a fact with no period
        // end cannot be placed on an axis at all.
        let server = MockServer::start().await;
        mount_tickers(&server).await;
        mount_json(&server, "/submissions/CIK0000320193.json", SUBMISSIONS).await;
        Mock::given(method("GET"))
            .and(path_matcher(
                "/api/xbrl/companyconcept/CIK0000320193/us-gaap/Revenues.json",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "taxonomy": "us-gaap",
                "tag": "Revenues",
                "units": { "USD": [
                    { "start": "2024-01-01", "end": "2024-12-31", "val": 391_035_000_000_i64,
                      "form": "10-K", "filed": "2024-11-01" },
                    { "start": "2023-01-01", "end": "2023-12-31", "val": null },
                    { "start": "2022-01-01", "end": "", "val": 394_328_000_000_i64 },
                    { "start": "2021-01-01", "val": 365_817_000_000_i64 }
                ] }
            })))
            .mount(&server)
            .await;

        let answer = get_concept(&state_for(&server), "AAPL", "Revenues", "us-gaap")
            .await
            .expect("the readable facts still chart");

        assert_eq!(answer.facts.len(), 1);
        assert_eq!(answer.facts[0].value, 391_035_000_000.0);
        assert_eq!(answer.facts[0].end, "2024-12-31");
    }

    #[tokio::test]
    async fn a_concept_with_no_units_at_all_is_an_empty_answer_rather_than_a_zero() {
        // Built inline. An empty answer has to stay empty: one fact of zero
        // would be a reported figure, and there is none.
        let server = MockServer::start().await;
        mount_tickers(&server).await;
        mount_json(&server, "/submissions/CIK0000320193.json", SUBMISSIONS).await;
        Mock::given(method("GET"))
            .and(path_matcher(
                "/api/xbrl/companyconcept/CIK0000320193/us-gaap/Revenues.json",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "taxonomy": "us-gaap", "tag": "Revenues", "label": "Revenues", "units": {}
            })))
            .mount(&server)
            .await;

        let answer = get_concept(&state_for(&server), "AAPL", "Revenues", "us-gaap")
            .await
            .expect("an empty concept is still an answer");

        assert!(answer.facts.is_empty());
        assert_eq!(answer.unit, "");
        assert_eq!(answer.tag, "Revenues");
    }

    #[tokio::test]
    async fn a_tag_this_filer_has_never_reported_is_named_as_that_not_as_an_unreadable_body() {
        // EDGAR answers an unreported tag with a 404 whose body is S3's XML.
        // Passed through as "the body is not JSON" it reads as our bug; the
        // truth is that not every issuer tags every concept, and the hint has
        // to name ones that exist.
        let server = MockServer::start().await;
        mount_tickers(&server).await;
        mount_json(&server, "/submissions/CIK0000320193.json", SUBMISSIONS).await;
        Mock::given(method("GET"))
            .and(path_matcher(
                "/api/xbrl/companyconcept/CIK0000320193/us-gaap/PremiumsEarnedNet.json",
            ))
            .respond_with(ResponseTemplate::new(404).set_body_raw(NO_SUCH_KEY, "application/xml"))
            .mount(&server)
            .await;

        let error = get_concept(&state_for(&server), "AAPL", "PremiumsEarnedNet", "us-gaap")
            .await
            .expect_err("never reported");

        assert_eq!(error.code, codes::NOT_FOUND);
        assert_eq!(
            error.message,
            "This filer has never reported us-gaap:PremiumsEarnedNet"
        );
        assert!(error.hint.expect("a hint").contains("NetIncomeLoss"));
    }

    #[tokio::test]
    async fn an_issuer_whose_taxonomy_carries_no_label_is_headed_by_its_tag() {
        // data.sec.gov ships `label` and `description` as null for every
        // `ifrs-full` concept, so a foreign private issuer's panel would be
        // headed by nothing. The taxonomy is a path segment and its hyphen must
        // reach EDGAR unescaped.
        let server = MockServer::start().await;
        mount_tickers(&server).await;
        Mock::given(method("GET"))
            .and(path_matcher("/submissions/CIK0000901832.json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "cik": "0000901832", "name": "ASTRAZENECA PLC"
            })))
            .mount(&server)
            .await;
        mount_json(
            &server,
            "/api/xbrl/companyconcept/CIK0000901832/ifrs-full/Revenue.json",
            IFRS,
        )
        .await;

        let answer = get_concept(&state_for(&server), "901832", "Revenue", "ifrs-full")
            .await
            .expect("the concept loads");

        assert_eq!(answer.label, "Revenue");
        assert_eq!(answer.tag, "Revenue");
        assert_eq!(answer.taxonomy, "ifrs-full");
        assert_eq!(answer.description, "");
        assert_eq!(answer.unit, "USD");
        assert_eq!(answer.facts.len(), 4);
        // The same year in two 20-Fs, unchanged and reported twice.
        assert_eq!(answer.facts[1].value, 23_002_000_000.0);
        assert_eq!(answer.facts[1].filed, "2018-03-06");
        assert_eq!(answer.facts[2].value, 23_002_000_000.0);
        assert_eq!(answer.facts[2].filed, "2019-03-05");

        let paths: Vec<String> = server
            .received_requests()
            .await
            .expect("recorded")
            .iter()
            .map(|request| request.url.path().to_owned())
            .collect();
        assert!(
            paths.contains(
                &"/api/xbrl/companyconcept/CIK0000901832/ifrs-full/Revenue.json".to_owned()
            ),
            "{paths:?}"
        );
    }

    #[tokio::test]
    async fn two_tags_do_not_share_one_cached_answer() {
        // The cache key carries the tag. Without it the second panel a reader
        // opens shows the first one's numbers under the second one's heading.
        let server = MockServer::start().await;
        mount_tickers(&server).await;
        mount_json(&server, "/submissions/CIK0000320193.json", SUBMISSIONS).await;
        mount_json(
            &server,
            "/api/xbrl/companyconcept/CIK0000320193/us-gaap/Assets.json",
            ASSETS,
        )
        .await;
        mount_json(
            &server,
            "/api/xbrl/companyconcept/CIK0000320193/us-gaap/EarningsPerShareDiluted.json",
            EPS,
        )
        .await;
        let state = state_for(&server);

        let assets = get_concept(&state, "AAPL", "Assets", "us-gaap")
            .await
            .expect("the concept loads");
        let eps = get_concept(&state, "AAPL", "EarningsPerShareDiluted", "us-gaap")
            .await
            .expect("the concept loads");

        assert_eq!(assets.tag, "Assets");
        assert_eq!(assets.unit, "USD");
        assert_eq!(assets.facts.len(), 8);
        assert_eq!(eps.tag, "EarningsPerShareDiluted");
        assert_eq!(eps.unit, "USD/shares");
        assert_eq!(eps.facts.len(), 7);
    }

    #[tokio::test]
    async fn a_concept_still_answers_when_the_filers_identity_cannot_be_read() {
        // The facts are the answer and the company block is the heading. A
        // failure on the submissions document must not take the numbers down
        // with it — a chart with a bare CIK over it is worth more than an
        // error.
        let server = MockServer::start().await;
        mount_tickers(&server).await;
        Mock::given(method("GET"))
            .and(path_matcher("/submissions/CIK0000320193.json"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;
        mount_json(
            &server,
            "/api/xbrl/companyconcept/CIK0000320193/us-gaap/Assets.json",
            ASSETS,
        )
        .await;

        let answer = get_concept(&state_for(&server), "AAPL", "Assets", "us-gaap")
            .await
            .expect("the facts still load");

        assert_eq!(answer.facts.len(), 8);
        assert_eq!(answer.company.cik, "0000320193");
        assert_eq!(answer.company.name, "0000320193");
        assert!(answer.company.tickers.is_empty());
    }
}
