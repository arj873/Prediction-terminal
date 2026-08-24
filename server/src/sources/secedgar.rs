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
//! User-Agent is answered `403`, and a request with any non-empty one — the
//! two-token `Name contact@domain` form the policy asks for, and an ordinary
//! browser string alike — is answered `200`. The branch this was ported from
//! claimed a browser-shaped User-Agent is refused; that does not reproduce, and
//! the note is corrected rather than carried across. The default names this
//! project, and an operator running it publicly should set `SEC_USER_AGENT` to
//! their own contact, which is what the policy actually asks for.

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
    if !trimmed.is_empty()
        && trimmed.len() <= 10
        && trimmed.chars().all(|c| c.is_ascii_digit())
    {
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
        UpstreamError::not_found(format!("EDGAR has no filer matching \"{query}\"")).with_hint(
            "Try a ticker (AAPL), a CIK (320193), or a distinctive word from the name.",
        ),
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
        if results.len() >= limit {
            break;
        }
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
    #[serde(default)]
    pub size: Vec<f64>,
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
    let wanted = form.map(|f| f.trim().to_uppercase()).filter(|f| !f.is_empty());
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
            size: filings.size.get(i).copied(),
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
    //! The submissions fixture was captured live from `data.sec.gov` for Apple
    //! and trimmed to twelve filings — with the `items` array deliberately left
    //! at four, because that mismatch is the whole reason the zip is a named
    //! function rather than an inline loop.

    use super::*;
    use wiremock::matchers::{method, path as path_matcher};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use crate::config::Config;

    const SUBMISSIONS: &str = include_str!("fixtures/sec_submissions.json");

    fn submissions_fixture() -> Submissions {
        serde_json::from_str(SUBMISSIONS).expect("the captured fixture parses")
    }

    fn recent() -> SubmissionsFilings {
        submissions_fixture()
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

    /* ------------------------------------------------------------------ CIK */

    #[test]
    fn pads_a_cik_to_the_ten_digits_edgar_keys_on() {
        assert_eq!(pad_cik("320193"), "0000320193");
        assert_eq!(pad_cik("0000320193"), "0000320193");
        // A reader may paste the form EDGAR prints in its own URLs.
        assert_eq!(pad_cik("CIK0000320193"), "0000320193");
        assert_eq!(pad_cik("1,318,605"), "0001318605");
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
    }

    #[test]
    fn filters_to_one_form_without_disturbing_the_alignment() {
        let filings = recent();
        let eight_ks = zip_filings(&filings, "0000320193", "https://www.sec.gov", 100, Some("8-K"));
        assert!(!eight_ks.is_empty(), "the fixture holds an 8-K");
        assert!(eight_ks.iter().all(|f| f.form == "8-K"));

        // The filter is case-insensitive, because nobody types `8-K` twice the
        // same way.
        let lower = zip_filings(&filings, "0000320193", "https://www.sec.gov", 100, Some("8-k"));
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

    /* ------------------------------------------------------------------ wire */

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
    async fn resolves_a_bare_number_as_a_cik_even_when_it_has_no_ticker() {
        // Plenty of filers — funds, individuals filing Form 4 — have a CIK and
        // no ticker at all, so the ticker file cannot be the only route in.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/files/company_tickers.json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "0": { "cik_str": 320193, "ticker": "AAPL", "title": "Apple Inc." }
            })))
            .mount(&server)
            .await;

        let state = state_for(&server);
        assert_eq!(resolve_cik(&state, "1067983").await.unwrap(), "0001067983");
        assert_eq!(resolve_cik(&state, "AAPL").await.unwrap(), "0000320193");
        // A name works too, on the words the filer's own title carries.
        assert_eq!(resolve_cik(&state, "apple").await.unwrap(), "0000320193");
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
}
