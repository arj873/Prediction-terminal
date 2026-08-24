//! Congress.gov — the Library of Congress's legislative API.
//!
//! Kalshi and both Polymarkets list "will bill X pass", "will the government
//! shut down", "who controls the House". Every one of those settles on an action
//! recorded here: a bill's introduction, its committee referral, a floor vote,
//! the President's signature. This is the primary record those markets resolve
//! against.
//!
//! The API needs a key, but degrades rather than failing: `DEMO_KEY` is
//! api.data.gov's shared credential and answers real data at a low per-IP rate —
//! confirmed live from this container, which holds no key of its own. A free key
//! from api.congress.gov raises the limit to 5,000 requests an hour. The
//! terminal says which one it used, because a rate-limit error on a shared key
//! is an operator problem with a five-minute fix, not an outage.

use std::sync::Arc;
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde::Deserialize;
use terminal_core::types::{Bill, BillAction, BillDetail, BillSearchResponse};

use crate::app::AppState;
use crate::cache::ttl;
use crate::error::UpstreamError;
use crate::http::FetchOptions;

type Result<T> = std::result::Result<T, UpstreamError>;

const TIMEOUT: Duration = Duration::from_secs(25);
const RETRIES: u32 = 1;

/// api.data.gov's shared demonstration credential. Rate-limited, but real.
const DEMO_KEY: &str = "DEMO_KEY";

fn api_key(state: &AppState) -> String {
    state
        .config()
        .congress_api_key
        .clone()
        .unwrap_or_else(|| DEMO_KEY.to_owned())
}

#[must_use]
pub fn using_demo_key(state: &AppState) -> bool {
    api_key(state) == DEMO_KEY
}

/// The bill types Congress.gov recognises, lower-case as its URLs want them.
pub const BILL_TYPES: &[&str] = &[
    "hr", "s", "hjres", "sjres", "hconres", "sconres", "hres", "sres",
];

pub fn assert_bill_type(raw: &str) -> Result<&'static str> {
    let normalised: String = raw
        .trim()
        .to_lowercase()
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '.')
        .collect();
    BILL_TYPES
        .iter()
        .find(|t| **t == normalised)
        .copied()
        .ok_or_else(|| {
            UpstreamError::bad_request(format!("\"{raw}\" is not a bill type")).with_hint(format!(
                "Types are {} — e.g. `CONG 119 hr 1`.",
                BILL_TYPES.join(", ")
            ))
        })
}

/// The Congress sitting on a given date.
///
/// The 1st Congress convened in 1789 and each runs two years from an
/// odd-numbered year, so this is arithmetic rather than a table — and it stays
/// right in 2027 without anyone remembering to update it.
#[must_use]
pub fn congress_for(year: i64, month: u32, day: u32) -> i64 {
    // A Congress begins on 3 January of the odd year; before then, the previous
    // one is still sitting.
    let effective = if month == 1 && day < 3 {
        year - 1
    } else {
        year
    };
    (effective - 1789) / 2 + 1
}

#[must_use]
pub fn current_congress() -> i64 {
    let days = crate::routes::helpers::now_seconds().div_euclid(86_400);
    let (year, month, day) = crate::sources::optionboard::civil_from_days_pub(days);
    congress_for(year, month, day)
}

/* ------------------------------------------------------------------ fetch */

async fn get<T>(state: &AppState, path: &str, params: &[(&str, String)]) -> Result<Arc<T>>
where
    T: DeserializeOwned + Send + Sync + 'static,
{
    let base = state.config().congress_api_base.trim_end_matches('/');
    let key = api_key(state);
    let demo = key == DEMO_KEY;

    let mut query: Vec<String> = vec!["format=json".to_owned()];
    for (name, value) in params {
        if !value.is_empty() {
            query.push(format!("{name}={}", encode_value(value)));
        }
    }
    // The key is part of the URL but must not be part of the cache key, or two
    // deployments of the same terminal would never share an entry — and the key
    // would sit in a log line the moment anyone prints the cache.
    let cache_key = format!("congress:{path}?{}", query.join("&"));
    let url = format!(
        "{base}{path}?{}&api_key={}",
        query.join("&"),
        urlencoding::encode(&key)
    );

    state
        .cache()
        .cached(&cache_key, ttl::META, || async {
            state
                .http()
                .fetch_json::<T>(&url, FetchOptions::new().timeout(TIMEOUT).retries(RETRIES))
                .await
                .map_err(|error| annotate(error, demo))
        })
        .await
}

/// Percent-encode one query value, but write a space as `+`.
///
/// Congress.gov's `sort` wants `updateDate+desc`, and a query-string `+` *is* a
/// space — the API echoes the parameter back as `sort=updateDate desc` in its
/// own `pagination.next`. A percent-encoded plus arrives as a literal `+`,
/// which it neither rejects nor honours: it answers in the collection's default
/// order instead, so the "recently updated" window the search claims to read is
/// a different 250 bills from the ones it gets. Measured live against
/// `api.congress.gov/v3/bill/119`.
fn encode_value(value: &str) -> String {
    urlencoding::encode(value).replace("%20", "+")
}

/// Turn the shared-key rate limit into the one sentence that fixes it.
fn annotate(error: UpstreamError, demo: bool) -> UpstreamError {
    if error.status != Some(429) || !demo {
        return error;
    }
    UpstreamError::new(
        "Congress.gov is rate-limiting the shared demonstration key",
        "rate_limited",
    )
    .with_status(429)
    .with_hint(
        "This terminal is using api.data.gov's DEMO_KEY, which is throttled per IP. A free key \
         from https://api.congress.gov/sign-up/ raises the limit to 5,000 requests an hour — set \
         CONGRESS_API_KEY.",
    )
}

/* ----------------------------------------------------------------- shapes */

#[derive(Debug, Default, Clone, Deserialize)]
pub struct ApiAction {
    #[serde(default, rename = "actionDate")]
    pub action_date: Option<String>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub chamber: Option<String>,
}

#[derive(Debug, Default, Clone, Deserialize)]
pub struct ApiSponsor {
    #[serde(default, rename = "fullName")]
    pub full_name: Option<String>,
    #[serde(default)]
    pub party: Option<String>,
    #[serde(default)]
    pub state: Option<String>,
}

#[derive(Debug, Default, Clone, Deserialize)]
pub struct ApiCount {
    #[serde(default)]
    pub count: Option<i64>,
}

#[derive(Debug, Default, Clone, Deserialize)]
pub struct ApiNamed {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub chamber: Option<String>,
}

#[derive(Debug, Default, Clone, Deserialize)]
pub struct ApiBill {
    #[serde(default)]
    pub congress: Option<i64>,
    #[serde(default, rename = "type")]
    pub bill_type: Option<String>,
    #[serde(default)]
    pub number: Option<serde_json::Value>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default, rename = "originChamber")]
    pub origin_chamber: Option<String>,
    #[serde(default, rename = "introducedDate")]
    pub introduced_date: Option<String>,
    #[serde(default, rename = "latestAction")]
    pub latest_action: Option<ApiAction>,
    #[serde(default, rename = "policyArea")]
    pub policy_area: Option<ApiNamed>,
    #[serde(default)]
    pub sponsors: Vec<ApiSponsor>,
    #[serde(default)]
    pub cosponsors: Option<ApiCount>,
    #[serde(default)]
    pub laws: Vec<serde_json::Value>,
}

fn squash(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn action_of(action: Option<&ApiAction>) -> Option<BillAction> {
    let action = action?;
    Some(BillAction {
        date: action
            .action_date
            .clone()
            .unwrap_or_default()
            .chars()
            .take(10)
            .collect(),
        text: squash(action.text.as_deref().unwrap_or("")),
        chamber: action.chamber.clone().unwrap_or_default(),
    })
}

/// `hr` + `1` + `119` → the citation a reader would say out loud.
#[must_use]
pub fn bill_label(bill_type: &str, number: &str, congress: i64) -> String {
    format!(
        "{} {number} ({})",
        bill_type.to_uppercase(),
        ordinal(congress)
    )
}

fn ordinal(n: i64) -> String {
    let rem100 = n % 100;
    if (11..=13).contains(&rem100) {
        return format!("{n}th");
    }
    let suffix = match n % 10 {
        1 => "st",
        2 => "nd",
        3 => "rd",
        _ => "th",
    };
    format!("{n}{suffix}")
}

/// Congress.gov's web URLs spell the type out.
fn chamber_slug(bill_type: &str) -> &'static str {
    match bill_type {
        "s" => "senate-bill",
        "hjres" => "house-joint-resolution",
        "sjres" => "senate-joint-resolution",
        "hconres" => "house-concurrent-resolution",
        "sconres" => "senate-concurrent-resolution",
        "hres" => "house-resolution",
        "sres" => "senate-resolution",
        _ => "house-bill",
    }
}

fn number_of(raw: &Option<serde_json::Value>) -> String {
    match raw {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

#[must_use]
pub fn to_bill(raw: &ApiBill) -> Bill {
    let congress = raw.congress.unwrap_or(0);
    let bill_type = raw.bill_type.clone().unwrap_or_default().to_lowercase();
    let number = number_of(&raw.number);
    let sponsor = raw.sponsors.first();
    let latest = action_of(raw.latest_action.as_ref());

    let became_law = !raw.laws.is_empty()
        || latest.as_ref().is_some_and(|action| {
            let text = action.text.to_lowercase();
            text.contains("became public law") || text.contains("signed by president")
        });

    Bill {
        congress,
        title: squash(raw.title.as_deref().unwrap_or("")),
        label: bill_label(&bill_type, &number, congress),
        origin_chamber: raw.origin_chamber.clone().unwrap_or_default(),
        introduced_date: raw
            .introduced_date
            .clone()
            .unwrap_or_default()
            .chars()
            .take(10)
            .collect(),
        latest_action: latest,
        sponsor: sponsor
            .and_then(|s| s.full_name.clone())
            .unwrap_or_default(),
        sponsor_party: sponsor.and_then(|s| s.party.clone()).unwrap_or_default(),
        sponsor_state: sponsor.and_then(|s| s.state.clone()).unwrap_or_default(),
        cosponsors: raw.cosponsors.as_ref().and_then(|c| c.count),
        policy_area: raw
            .policy_area
            .as_ref()
            .and_then(|p| p.name.clone())
            .unwrap_or_default(),
        // `laws` is present only once a bill is enacted, which is the cleanest
        // signal available; the action text is a fallback for the gap between
        // the signing and the law number being assigned.
        became_law,
        // Congress.gov spells the Congress out with its real ordinal, so a link
        // to the 103rd is `103rd-congress`; `103th-congress` is not a page.
        url: format!(
            "https://www.congress.gov/bill/{}-congress/{}/{number}",
            ordinal(congress),
            chamber_slug(&bill_type)
        ),
        bill_type,
        number,
    }
}

/* ---------------------------------------------------------------- public */

/// Bills per request. The API's ceiling, and the unit `offset` steps by.
const PAGE: usize = 250;

#[derive(Debug, Default, Deserialize)]
struct BillsPayload {
    #[serde(default)]
    bills: Vec<ApiBill>,
}

#[derive(Debug, Default, Deserialize)]
struct BillPayload {
    #[serde(default)]
    bill: Option<ApiBill>,
}

#[derive(Debug, Default, Deserialize)]
struct SummariesPayload {
    #[serde(default)]
    summaries: Vec<SummaryRow>,
}

#[derive(Debug, Default, Clone, Deserialize)]
struct SummaryRow {
    #[serde(default)]
    text: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct ActionsPayload {
    #[serde(default)]
    actions: Vec<ApiAction>,
}

#[derive(Debug, Default, Deserialize)]
struct CommitteesPayload {
    #[serde(default)]
    committees: Vec<ApiNamed>,
}

/// The sentence the search panel prints, because what it did is not what a
/// reader assumes.
const SEARCH_NOTE: &str = "Congress.gov's bill collection has no text parameter — its own search \
                           UI runs on a separate service with no public API — so this reads a \
                           window of the Congress's recently-updated bills and matches titles \
                           here. A miss means \"not among the recently active bills\", not \"no \
                           such bill\".";

/// Search bills by words, most recently acted on first.
///
/// How wide the window is depends on which key is in play, and that is not a
/// detail — it is the difference between the command working and not. Four
/// sequential requests against api.data.gov's shared `DEMO_KEY` exhaust its
/// per-IP throttle immediately: measured from this container, a four-page crawl
/// returns 429 while a one-page crawl returns bills. So a deployment with no key
/// of its own reads one page — the 250 most recently updated bills, which is
/// still the last several weeks of a Congress — and one with a key reads four,
/// for roughly the last few months. The response says which happened rather than
/// quietly returning a shallower answer.
pub async fn search_bills(
    state: &AppState,
    query: &str,
    congress: Option<i64>,
    limit: usize,
) -> Result<BillSearchResponse> {
    let target = congress.unwrap_or_else(current_congress);
    let words: Vec<String> = query
        .trim()
        .to_lowercase()
        .split_whitespace()
        .map(str::to_owned)
        .collect();
    let demo = using_demo_key(state);
    let pages = if words.is_empty() || demo { 1 } else { 4 };

    let mut all: Vec<Bill> = Vec::new();
    for page in 0..pages {
        let payload = get::<BillsPayload>(
            state,
            &format!("/bill/{target}"),
            &[
                ("limit", PAGE.to_string()),
                ("offset", (page * PAGE).to_string()),
                ("sort", "updateDate desc".to_owned()),
            ],
        )
        .await?;
        let batch = &payload.bills;
        all.extend(batch.iter().map(to_bill));
        // A short page is the end of the collection; asking for the next one
        // would spend a request against the rate limit to be told the same.
        if batch.len() < PAGE {
            break;
        }
    }

    let matched: Vec<Bill> = if words.is_empty() {
        all
    } else {
        all.into_iter()
            .filter(|bill| {
                let haystack = format!(
                    "{} {}{} {} {} {}",
                    bill.label,
                    bill.bill_type,
                    bill.number,
                    bill.title,
                    bill.sponsor,
                    bill.policy_area
                )
                .to_lowercase();
                words.iter().all(|word| haystack.contains(word.as_str()))
            })
            .collect()
    };

    // The same bill appears on more than one page when Congress.gov re-sorts
    // between requests, which it does whenever a bill is acted on mid-scan.
    let mut seen = std::collections::HashSet::new();
    let bills: Vec<Bill> = matched
        .into_iter()
        .filter(|bill| {
            seen.insert(format!(
                "{}/{}/{}",
                bill.congress, bill.bill_type, bill.number
            ))
        })
        .take(limit)
        .collect();

    Ok(BillSearchResponse {
        query: query.to_owned(),
        congress: Some(target),
        bills,
        source: if demo {
            "congress.gov (DEMO_KEY)".to_owned()
        } else {
            "congress.gov".to_owned()
        },
        note: if demo {
            format!(
                "{SEARCH_NOTE} On api.data.gov's shared DEMO_KEY this reads one page of {PAGE} \
                 bills rather than four, because four exhausts its per-IP throttle. Set \
                 CONGRESS_API_KEY to widen it."
            )
        } else {
            SEARCH_NOTE.to_owned()
        },
    })
}

/// One bill, with its summary, full action history and committees.
pub async fn get_bill(
    state: &AppState,
    congress: i64,
    raw_type: &str,
    number: &str,
) -> Result<BillDetail> {
    let bill_type = assert_bill_type(raw_type)?;
    let path = format!(
        "/bill/{congress}/{bill_type}/{}",
        urlencoding::encode(number)
    );

    let payload = get::<BillPayload>(state, &path, &[]).await?;
    let Some(raw) = payload.bill.clone() else {
        return Err(UpstreamError::not_found(format!(
            "Congress.gov has no {}",
            bill_label(bill_type, number, congress)
        ))
        .with_hint(format!(
            "Check the Congress number — the current one is {}.",
            current_congress()
        )));
    };

    // Summaries, actions and committees are separate sub-resources. Any of them
    // can be legitimately absent — a bill introduced this morning has no summary
    // — so a failure on one must not cost the reader the bill itself.
    let summaries = get::<SummariesPayload>(state, &format!("{path}/summaries"), &[]).await;
    let actions = get::<ActionsPayload>(
        state,
        &format!("{path}/actions"),
        &[("limit", "250".to_owned())],
    )
    .await;
    let committees = get::<CommitteesPayload>(state, &format!("{path}/committees"), &[]).await;

    let summary = summaries
        .ok()
        .and_then(|s| s.summaries.last().cloned())
        .and_then(|s| s.text)
        .map(|html| strip_html(&html))
        .unwrap_or_default();

    // The citation the reader typed is the one the panel answers under, so the
    // three fields it is built from come from the arguments and not from the
    // payload: Congress.gov states the type as `HR` where every label and URL
    // here want `hr`, and numbers a bill as a JSON string in one collection and
    // a JSON number in another. Overriding all three is what makes a detail
    // response agree with the search row that led to it.
    let bill = to_bill(&ApiBill {
        congress: Some(congress),
        bill_type: Some(bill_type.to_owned()),
        number: Some(serde_json::Value::String(number.to_owned())),
        ..raw
    });

    Ok(BillDetail {
        bill,
        summary,
        actions: actions
            .map(|a| {
                a.actions
                    .iter()
                    .filter_map(|action| action_of(Some(action)))
                    .filter(|action| !action.text.is_empty())
                    .collect()
            })
            .unwrap_or_default(),
        committees: committees
            .map(|c| {
                c.committees
                    .iter()
                    .map(|committee| {
                        [committee.name.as_deref(), committee.chamber.as_deref()]
                            .into_iter()
                            .flatten()
                            .filter(|part| !part.is_empty())
                            .collect::<Vec<_>>()
                            .join(" · ")
                    })
                    .filter(|line| !line.is_empty())
                    .collect()
            })
            .unwrap_or_default(),
    })
}

/// Congress.gov files its summaries as HTML fragments; the terminal shows text.
#[must_use]
pub fn strip_html(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    let mut tag = String::new();

    for ch in html.chars() {
        match ch {
            '<' => {
                in_tag = true;
                tag.clear();
            }
            '>' if in_tag => {
                in_tag = false;
                let name = tag.trim().to_lowercase();
                if name.starts_with("br") {
                    out.push('\n');
                } else if name.starts_with("/p") {
                    out.push_str("\n\n");
                }
            }
            _ if in_tag => tag.push(ch),
            _ => out.push(ch),
        }
    }

    let out = decode_entities(&out);

    // Collapse runs of spaces without touching the paragraph breaks above.
    let mut squeezed = String::with_capacity(out.len());
    let mut last_space = false;
    for ch in out.chars() {
        if ch == ' ' || ch == '\t' {
            if !last_space {
                squeezed.push(' ');
            }
            last_space = true;
        } else {
            last_space = false;
            squeezed.push(ch);
        }
    }

    let mut text = squeezed.trim().to_owned();
    while text.contains("\n\n\n") {
        text = text.replace("\n\n\n", "\n\n");
    }
    // These summaries run to six figures of characters and are full of curly
    // apostrophes — 61 in the enacted H.R. 1 summary, 200 across the five
    // versions of that one bill. `String::truncate` panics unless its index is
    // a character boundary, so back off to the last one.
    let mut cut = text.len().min(8000);
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    text.truncate(cut);
    text
}

fn decode_entities(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('&') {
        out.push_str(&rest[..start]);
        let tail = &rest[start..];
        let Some(end) = tail.find(';').filter(|end| *end <= 10) else {
            out.push('&');
            rest = &tail[1..];
            continue;
        };
        let entity = &tail[1..end];
        let decoded = match entity {
            "nbsp" => Some(' '),
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" | "#39" => Some('\''),
            _ => entity
                .strip_prefix('#')
                .and_then(|digits| digits.parse::<u32>().ok())
                .and_then(char::from_u32),
        };
        match decoded {
            Some(ch) => {
                out.push(ch);
                rest = &tail[end + 1..];
            }
            None => {
                out.push('&');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    //! Congress.gov parser and wire tests.
    //!
    //! Every fixture here was captured live from `api.congress.gov/v3` with
    //! curl and `api_key=DEMO_KEY`, from this container, which holds no key of
    //! its own. Where a payload was too big to assert in full it was trimmed —
    //! by dropping whole rows, by dropping keys this module never reads, or by
    //! taking one contiguous slice of a field. Nothing was added and no value
    //! was altered, which matters most for the figures that are *absent*: a key
    //! the Library of Congress does not send is missing here too, because
    //! writing one in is how a fixture starts proving the opposite of the
    //! module's behaviour. Every figure below is read off the file beside this
    //! module.

    use super::*;
    use serde_json::json;
    use wiremock::matchers::{method, path as path_matcher, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use crate::config::Config;
    use crate::error::codes;

    /// `/bill/119/hr/1`, the One Big Beautiful Bill Act, captured 2026-08-24,
    /// trimmed to the keys this module reads plus `legislationUrl`. The one
    /// enacted bill everybody looks up. Kept because Congress.gov sends no
    /// `cosponsors` key at all for it — which is what "the publisher states no
    /// figure" looks like on the wire, as opposed to a published zero — and
    /// because it publishes its own web link, so the one this module builds can
    /// be checked against the Library of Congress's rather than against itself.
    const BILL_JSON: &str = include_str!("fixtures/congress_bill.json");

    /// What api.data.gov answers over its per-IP throttle, captured 2026-08-24
    /// by asking for a bill list four times in a row. Kept because this body,
    /// not a timeout, is what a keyless deployment actually hits.
    const RATE_LIMITED_JSON: &str = include_str!("fixtures/congress_rate_limited.json");

    /// `/bill/119?limit=5&offset=0&sort=updateDate+desc`, captured 2026-08-24 —
    /// the first five rows of the window the bill search reads. Kept for the
    /// order it arrived in, and because collection rows carry no sponsor, no
    /// policy area and no cosponsor count at all.
    const BILLS_PAGE_JSON: &str = include_str!("fixtures/congress_bills_page.json");

    /// `/law/119?limit=3`, captured 2026-08-24. Same `bills` shape as the
    /// collection, and the only rows that carry a `laws` array — which is what
    /// "became law" is read off. They also carry no `introducedDate`.
    const LAWS_PAGE_JSON: &str = include_str!("fixtures/congress_laws_page.json");

    /// `/bill/119/hr/1/summaries`, captured 2026-08-24: the oldest and the
    /// newest of the five filed versions, each `text` cut to one contiguous
    /// slice of the real HTML. Kept to pin which version is shown, and because
    /// the newest one carries `<ul>`, `<li>` and `&nbsp;` in a real fragment.
    const SUMMARIES_JSON: &str = include_str!("fixtures/congress_summaries.json");

    /// `/bill/119/hr/1/actions?limit=250`, captured 2026-08-24: the six newest
    /// and three oldest of 59. Kept for the order — newest first — and because
    /// no action in the real payload carries a `chamber` at all.
    const ACTIONS_JSON: &str = include_str!("fixtures/congress_actions.json");

    /// `/bill/119/hr/1/committees`, captured 2026-08-24, as served. One
    /// committee, which is what the joined `name · chamber` line is built from.
    const COMMITTEES_JSON: &str = include_str!("fixtures/congress_committees.json");

    /// The Congress every fixture here belongs to.
    const CONGRESS: i64 = 119;

    fn demo_state(server: &MockServer) -> AppState {
        AppState::new(Config {
            congress_api_base: server.uri(),
            ..Config::default()
        })
    }

    fn keyed_state(server: &MockServer) -> AppState {
        AppState::new(Config {
            congress_api_base: server.uri(),
            congress_api_key: Some("a-real-key".to_owned()),
            ..Config::default()
        })
    }

    fn fixture(text: &str) -> serde_json::Value {
        serde_json::from_str(text).expect("the captured fixture parses")
    }

    /// Every request the mock saw, as `path?query`, in order.
    async fn queries(server: &MockServer) -> Vec<String> {
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

    async fn mount_page(server: &MockServer, body: serde_json::Value) {
        Mock::given(method("GET"))
            .and(path_matcher(format!("/bill/{CONGRESS}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(server)
            .await;
    }

    /// `PAGE` rows, built by renumbering the captured page's first bill.
    ///
    /// The crawl asks for a second page only when the first comes back full, so
    /// a five-row fixture can never reach one. Real rows, distinct numbers.
    fn full_page(first: usize) -> serde_json::Value {
        let template = fixture(BILLS_PAGE_JSON)["bills"][0].clone();
        let bills: Vec<serde_json::Value> = (first..first + PAGE)
            .map(|number| {
                let mut bill = template.clone();
                bill["number"] = json!(number.to_string());
                bill
            })
            .collect();
        json!({ "bills": bills })
    }

    /// Four full pages, each with its own numbers, keyed on `offset`.
    async fn mount_four_pages(server: &MockServer) {
        for page in 0..4usize {
            Mock::given(method("GET"))
                .and(path_matcher(format!("/bill/{CONGRESS}")))
                .and(query_param("offset", (page * PAGE).to_string()))
                .respond_with(ResponseTemplate::new(200).set_body_json(full_page(page * PAGE)))
                .mount(server)
                .await;
        }
    }

    /// One bill and its three sub-resources, as `get_bill` asks for them.
    async fn mount_bill(server: &MockServer) {
        for (suffix, body) in [
            ("", BILL_JSON),
            ("/summaries", SUMMARIES_JSON),
            ("/actions", ACTIONS_JSON),
            ("/committees", COMMITTEES_JSON),
        ] {
            Mock::given(method("GET"))
                .and(path_matcher(format!("/bill/{CONGRESS}/hr/1{suffix}")))
                .respond_with(ResponseTemplate::new(200).set_body_json(fixture(body)))
                .mount(server)
                .await;
        }
    }

    fn labels(answer: &BillSearchResponse) -> Vec<&str> {
        answer
            .bills
            .iter()
            .map(|bill| bill.label.as_str())
            .collect()
    }

    /* ------------------------------------------------------------ the key */

    #[test]
    fn a_deployment_with_no_key_of_its_own_uses_the_shared_one_rather_than_refusing() {
        // api.data.gov hands DEMO_KEY out precisely so this works; a terminal
        // that demanded a credential would show an empty panel on a fresh
        // checkout, which is the worse of the two failures.
        let bare = AppState::new(Config::default());
        assert!(using_demo_key(&bare));
        assert_eq!(api_key(&bare), "DEMO_KEY");

        let keyed = AppState::new(Config {
            congress_api_key: Some("abc123".to_owned()),
            ..Config::default()
        });
        assert!(!using_demo_key(&keyed));
        assert_eq!(api_key(&keyed), "abc123");
    }

    #[tokio::test]
    async fn the_key_is_encoded_into_the_url_and_kept_out_of_the_cache_key() {
        let server = MockServer::start().await;
        mount_bill(&server).await;
        let state = AppState::new(Config {
            congress_api_base: server.uri(),
            congress_api_key: Some("k&limit=1".to_owned()),
            ..Config::default()
        });

        get_bill(&state, CONGRESS, "hr", "1")
            .await
            .expect("the bill resolves");

        // An unescaped `&` in a key read from the environment would become a
        // second query parameter and silently override `limit`.
        let sent = queries(&server).await;
        assert!(
            sent.contains(&"/bill/119/hr/1?format=json&api_key=k%26limit%3D1".to_owned()),
            "{sent:?}"
        );
        // And the key is absent from the keyspace, so two deployments share one
        // entry and nobody prints a credential by dumping the cache.
        assert!(state
            .cache()
            .get::<BillPayload>("congress:/bill/119/hr/1?format=json")
            .await
            .is_some());
    }

    /* ------------------------------------------------------- the citation */

    #[test]
    fn a_bill_type_is_read_however_the_reader_punctuated_it() {
        // `H.R.`, `H. R.` and `hr` are one thing to a reader and three strings
        // to Congress.gov's URLs, which accept only the last.
        for raw in ["hr", "HR", " H.R. ", "H. R.", "h.r"] {
            assert_eq!(assert_bill_type(raw).expect("a bill type"), "hr", "{raw:?}");
        }
        assert_eq!(assert_bill_type("HJRes").expect("a bill type"), "hjres");
        assert_eq!(assert_bill_type("S.J.Res.").expect("a bill type"), "sjres");
        assert!(assert_bill_type("").is_err());
        assert!(assert_bill_type("hr1").is_err());
        assert!(assert_bill_type("bill").is_err());
    }

    #[tokio::test]
    async fn a_word_that_is_not_a_bill_type_is_refused_before_a_request_is_spent() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
            .expect(0)
            .mount(&server)
            .await;

        let error = get_bill(&demo_state(&server), CONGRESS, "hres1", "1")
            .await
            .expect_err("not a bill type");

        assert_eq!(error.code, codes::BAD_REQUEST);
        assert_eq!(error.message, "\"hres1\" is not a bill type");
        let hint = error.hint.expect("a hint");
        for kind in BILL_TYPES {
            assert!(hint.contains(kind), "{kind} missing from {hint}");
        }
    }

    #[test]
    fn a_congress_begins_on_the_third_of_january_of_its_odd_year() {
        // The 119th convened on 3 January 2025. A bill filed on the 2nd belongs
        // to the 118th, and filing it under the 119th makes it unfindable.
        assert_eq!(congress_for(2025, 1, 2), 118);
        assert_eq!(congress_for(2025, 1, 3), 119);
        assert_eq!(congress_for(2026, 8, 24), 119);
        assert_eq!(congress_for(2027, 1, 2), 119);
        assert_eq!(congress_for(2027, 1, 3), 120);
        // The 1st convened in 1789, which is the whole basis of the arithmetic.
        assert_eq!(congress_for(1789, 3, 4), 1);
        assert_eq!(congress_for(1994, 6, 1), 103);

        // `current_congress` is that arithmetic wired to the clock, and it is
        // the Congress every keyless search reads and every citation defaults
        // to. `>= 119` passes for any number a broken wiring produces, so check
        // it by inverting: whichever Congress it names must be the one whose
        // two years contain today.
        let days = crate::routes::helpers::now_seconds().div_euclid(86_400);
        let (year, month, day) = crate::sources::optionboard::civil_from_days_pub(days);
        let now = current_congress();
        let convened = 1789 + (now - 1) * 2;
        assert!(
            (convened..=convened + 1).contains(&year)
                || (year == convened + 2 && month == 1 && day < 3),
            "the {} Congress does not sit on {year}-{month:02}-{day:02}",
            ordinal(now)
        );
    }

    #[test]
    fn a_citation_names_the_congress_the_way_a_reader_says_it() {
        assert_eq!(bill_label("hr", "1", 119), "HR 1 (119th)");
        assert_eq!(bill_label("sjres", "12", 121), "SJRES 12 (121st)");
        assert_eq!(bill_label("s", "5", 122), "S 5 (122nd)");
        assert_eq!(bill_label("s", "5", 123), "S 5 (123rd)");
        assert_eq!(bill_label("hr", "1", 101), "HR 1 (101st)");
        // 111, 112 and 113 are "th" however their last digit reads.
        assert_eq!(bill_label("hr", "1", 111), "HR 1 (111th)");
        assert_eq!(bill_label("hr", "1", 112), "HR 1 (112th)");
        assert_eq!(bill_label("hr", "1", 113), "HR 1 (113th)");
    }

    #[test]
    fn a_bill_links_to_the_congress_gov_page_that_exists() {
        let link = |kind: &str, congress: i64| {
            to_bill(&ApiBill {
                congress: Some(congress),
                // Congress.gov states the type upper-case; its URLs want it
                // lower-case, and the two must not be mixed up.
                bill_type: Some(kind.to_uppercase()),
                number: Some(json!("1")),
                ..ApiBill::default()
            })
            .url
        };

        assert_eq!(
            link("hr", 119),
            "https://www.congress.gov/bill/119th-congress/house-bill/1"
        );
        // The API serves back to the 93rd Congress, so `103th-congress` — which
        // is not a page — is reachable from the terminal today.
        assert_eq!(
            link("hr", 103),
            "https://www.congress.gov/bill/103rd-congress/house-bill/1"
        );
        assert_eq!(
            link("hr", 121),
            "https://www.congress.gov/bill/121st-congress/house-bill/1"
        );

        assert_eq!(
            link("s", 119),
            "https://www.congress.gov/bill/119th-congress/senate-bill/1"
        );
        assert_eq!(
            link("hjres", 119),
            "https://www.congress.gov/bill/119th-congress/house-joint-resolution/1"
        );
        assert_eq!(
            link("sjres", 119),
            "https://www.congress.gov/bill/119th-congress/senate-joint-resolution/1"
        );
        assert_eq!(
            link("hconres", 119),
            "https://www.congress.gov/bill/119th-congress/house-concurrent-resolution/1"
        );
        assert_eq!(
            link("sconres", 119),
            "https://www.congress.gov/bill/119th-congress/senate-concurrent-resolution/1"
        );
        assert_eq!(
            link("hres", 119),
            "https://www.congress.gov/bill/119th-congress/house-resolution/1"
        );
        assert_eq!(
            link("sres", 119),
            "https://www.congress.gov/bill/119th-congress/senate-resolution/1"
        );

        // `chamber_slug` falls through to `house-bill`, so a type added to
        // `BILL_TYPES` without a slug would link every reader to the wrong page.
        let slugs: std::collections::HashSet<&str> =
            BILL_TYPES.iter().map(|kind| chamber_slug(kind)).collect();
        assert_eq!(slugs.len(), BILL_TYPES.len());
    }

    /* ----------------------------------------------------------- one bill */

    #[test]
    fn a_figure_congress_gov_does_not_publish_is_absent_rather_than_zero() {
        // Two different silences, and neither of them is a nought. A collection
        // row carries no `cosponsors` key because the collection never carries
        // one; H.R. 1's own record carries none either, because Congress.gov
        // omits the key outright on a bill nobody cosponsored. A panel that drew
        // either as "0" would be stating a figure the Library of Congress has
        // not published — and "0 cosponsors" is a claim a reader would act on.
        let row: ApiBill = serde_json::from_value(fixture(BILLS_PAGE_JSON)["bills"][0].clone())
            .expect("a collection row parses");
        let listed = to_bill(&row);
        assert_eq!(listed.cosponsors, None);
        assert_eq!(listed.sponsor, "");
        assert_eq!(listed.sponsor_party, "");
        assert_eq!(listed.policy_area, "");

        assert!(
            !fixture(BILL_JSON)["bill"]
                .as_object()
                .expect("a bill object")
                .contains_key("cosponsors"),
            "the captured payload must keep its silence, or the assert below \
             proves nothing"
        );
        let detail: BillPayload = serde_json::from_str(BILL_JSON).expect("the fixture parses");
        let bill = to_bill(detail.bill.as_ref().expect("a bill"));
        assert_eq!(bill.cosponsors, None);
        assert_eq!(bill.sponsor, "Rep. Arrington, Jodey C. [R-TX-19]");

        // The other half of the invariant: a count Congress.gov *does* state
        // survives as the number it stated, a real zero included. It sends
        // `{"count": 0, ...}` on a bill whose cosponsors all withdrew, and
        // reading that as "no figure" would hide a fact it did publish. These
        // three rows are built here — the shape is the API's, the payloads are
        // not captured.
        let counted = |count: Option<i64>| {
            to_bill(&ApiBill {
                cosponsors: Some(ApiCount { count }),
                ..ApiBill::default()
            })
            .cosponsors
        };
        assert_eq!(counted(Some(0)), Some(0));
        assert_eq!(counted(Some(2)), Some(2));
        // And a `cosponsors` object with no count inside it is still silence.
        assert_eq!(counted(None), None);

        // A bill with no action recorded has no action, rather than one dated to
        // the empty string.
        assert!(to_bill(&ApiBill::default()).latest_action.is_none());
    }

    #[test]
    fn a_bill_becomes_law_on_the_record_rather_than_on_the_word_president() {
        let page: BillsPayload = serde_json::from_str(LAWS_PAGE_JSON).expect("the fixture parses");
        let bills: Vec<Bill> = page.bills.iter().map(to_bill).collect();

        assert_eq!(bills.len(), 3);
        assert_eq!(bills[0].label, "S 98 (119th)");
        assert_eq!(bills[0].title, "Rural Broadband Protection Act of 2025");
        assert!(bills[0].became_law);
        // `/law` rows state no introduction date, and an absent date is not the
        // first of January.
        assert_eq!(bills[0].introduced_date, "");
        assert_eq!(
            bills[0].url,
            "https://www.congress.gov/bill/119th-congress/senate-bill/98"
        );

        // The `laws` array on its own, under an action that says nothing about
        // a law. This is the branch the test's name is about: without it,
        // "became law" is a reading of prose, and Congress.gov re-words its
        // action lines whenever the last thing to happen to an enacted bill is
        // something other than the enactment.
        let mut recorded = page.bills[0].clone();
        recorded.latest_action = Some(ApiAction {
            action_date: Some("2026-05-11".to_owned()),
            text: Some("Message on Senate action sent to the House.".to_owned()),
            chamber: None,
        });
        assert!(!recorded.laws.is_empty());
        assert!(to_bill(&recorded).became_law);

        // The same bill in the gap between the signing and the law number being
        // assigned: `laws` is empty and only the action text says so.
        let mut pending = page.bills[1].clone();
        pending.laws.clear();
        assert_eq!(
            pending.title.as_deref(),
            Some("21st Century ROAD to Housing Act")
        );
        assert!(to_bill(&pending).became_law);

        let mut signed = pending.clone();
        signed.latest_action = Some(ApiAction {
            action_date: Some("2025-07-04".to_owned()),
            text: Some("Signed by President.".to_owned()),
            chamber: None,
        });
        assert!(to_bill(&signed).became_law);

        // "Presented to President." stands on every enacted bill's record for
        // days before it is law, and a market on "will it be signed" settles on
        // the difference.
        let mut presented = pending.clone();
        presented.latest_action = Some(ApiAction {
            action_date: Some("2025-07-03".to_owned()),
            text: Some("Presented to President.".to_owned()),
            chamber: None,
        });
        assert!(!to_bill(&presented).became_law);

        // As does a bill still sitting in committee.
        let listed: ApiBill = serde_json::from_value(fixture(BILLS_PAGE_JSON)["bills"][2].clone())
            .expect("a collection row parses");
        assert_eq!(
            listed.title.as_deref(),
            Some("Local Health Care Protection Act of 2026")
        );
        assert!(!to_bill(&listed).became_law);
    }

    #[test]
    fn a_title_wrapped_over_lines_is_one_line_and_a_timestamped_date_is_cut_to_its_day() {
        let bill = to_bill(&ApiBill {
            congress: Some(119),
            bill_type: Some("HR".to_owned()),
            // Some collections number a bill as JSON string and some as a JSON
            // number; both are the same bill.
            number: Some(json!(1)),
            title: Some("An act to provide\n  for reconciliation\tpursuant to title II".to_owned()),
            introduced_date: Some("2025-05-20T04:00:00Z".to_owned()),
            latest_action: Some(ApiAction {
                action_date: Some("2025-07-04T00:00:00Z".to_owned()),
                text: Some("  Became Public Law\n No: 119-21.  ".to_owned()),
                chamber: None,
            }),
            ..ApiBill::default()
        });

        assert_eq!(bill.number, "1");
        assert_eq!(
            bill.title,
            "An act to provide for reconciliation pursuant to title II"
        );
        assert_eq!(bill.introduced_date, "2025-05-20");
        let action = bill.latest_action.expect("an action");
        assert_eq!(action.date, "2025-07-04");
        assert_eq!(action.text, "Became Public Law No: 119-21.");
        // Congress.gov states no chamber on any action in the captured payload.
        assert_eq!(action.chamber, "");
    }

    /* ------------------------------------------------------------- search */

    #[tokio::test]
    async fn reads_the_recently_updated_window_and_keeps_congress_gov_s_order() {
        let server = MockServer::start().await;
        mount_page(&server, fixture(BILLS_PAGE_JSON)).await;

        let answer = search_bills(&demo_state(&server), "", Some(CONGRESS), 10)
            .await
            .expect("the collection answers");

        // Congress.gov's own order, which is what "most recently acted on
        // first" means here — the module does not re-sort.
        assert_eq!(
            labels(&answer),
            [
                "SRES 690 (119th)",
                "S 4689 (119th)",
                "HR 10134 (119th)",
                "HR 10118 (119th)",
                "HR 10078 (119th)"
            ]
        );

        // `sort=updateDate+desc`: the `+` is a space on the wire, and the API
        // echoes it back as `sort=updateDate desc` in its own `pagination.next`.
        // Percent-encoded it arrives as a literal plus, which Congress.gov
        // neither rejects nor honours — it answers in the collection's default
        // order, a different 250 bills from the window this claims to read.
        assert_eq!(
            queries(&server).await,
            ["/bill/119?format=json&limit=250&offset=0&sort=updateDate+desc&api_key=DEMO_KEY"]
        );
    }

    #[tokio::test]
    async fn reads_a_bill_row_out_of_the_collection_page() {
        let server = MockServer::start().await;
        mount_page(&server, fixture(BILLS_PAGE_JSON)).await;

        let answer = search_bills(&demo_state(&server), "", Some(CONGRESS), 10)
            .await
            .expect("the collection answers");

        let first = &answer.bills[0];
        assert_eq!(first.congress, 119);
        assert_eq!(first.bill_type, "sres");
        assert_eq!(first.number, "690");
        assert_eq!(first.origin_chamber, "Senate");
        assert_eq!(first.introduced_date, "2026-04-27");
        assert_eq!(
            first.url,
            "https://www.congress.gov/bill/119th-congress/senate-resolution/690"
        );
        assert!(!first.became_law);

        let action = first.latest_action.as_ref().expect("a latest action");
        assert_eq!(action.date, "2026-05-11");
        assert_eq!(
            action.text,
            "Resolution agreed to in Senate without amendment by Yea-Nay Vote. 46 - 45. Record \
             Vote Number: 114. (text: CR 4/27/2026 S2056-2057)"
        );
        assert_eq!(action.chamber, "");
    }

    #[tokio::test]
    async fn every_word_must_match_and_the_latest_action_is_not_searched() {
        let server = MockServer::start().await;
        mount_page(&server, fixture(BILLS_PAGE_JSON)).await;
        let state = demo_state(&server);

        let acts = search_bills(&state, "act", Some(CONGRESS), 10)
            .await
            .expect("bills");
        assert_eq!(
            labels(&acts),
            [
                "S 4689 (119th)",
                "HR 10134 (119th)",
                "HR 10118 (119th)",
                "HR 10078 (119th)"
            ]
        );

        // A second word narrows rather than widens: "no data center" is one
        // bill out of the five.
        let narrowed = search_bills(&state, "no data center", Some(CONGRESS), 10)
            .await
            .expect("bills");
        assert_eq!(labels(&narrowed), ["HR 10118 (119th)"]);

        // "Referred to the House Committee on Energy and Commerce." is H.R.
        // 10134's latest action. The action text is deliberately not in the
        // haystack, so every bill in committee would otherwise match "referred".
        let action_words = search_bills(&state, "referred", Some(CONGRESS), 10)
            .await
            .expect("bills");
        assert!(labels(&action_words).is_empty());
    }

    #[tokio::test]
    async fn a_bill_is_found_by_the_citation_a_reader_types_however_they_cased_it() {
        // The haystack joins the type and number with no space — `hr10118` —
        // which is what makes both "hr 10118" and "hr10118" land on it.
        let server = MockServer::start().await;
        mount_page(&server, fixture(BILLS_PAGE_JSON)).await;
        let state = demo_state(&server);

        for query in ["hr 10118", "HR 10118", "hr10118", "10118"] {
            let answer = search_bills(&state, query, Some(CONGRESS), 10)
                .await
                .expect("bills");
            assert_eq!(labels(&answer), ["HR 10118 (119th)"], "{query:?}");
        }

        // But the free-text search normalises nothing, where `assert_bill_type`
        // strips the dots: `CONG 119 H.R. 10118` opens the bill and searching
        // for the same string finds it only if the reader drops the dots.
        let punctuated = search_bills(&state, "H.R. 10118", Some(CONGRESS), 10)
            .await
            .expect("bills");
        assert!(labels(&punctuated).is_empty());
    }

    #[tokio::test]
    async fn a_query_that_matches_nothing_is_an_empty_list_not_the_window_it_read() {
        let server = MockServer::start().await;
        mount_page(&server, fixture(BILLS_PAGE_JSON)).await;

        let answer = search_bills(&demo_state(&server), "wombat", Some(CONGRESS), 10)
            .await
            .expect("an answer");

        assert!(answer.bills.is_empty());
        assert_eq!(answer.query, "wombat");
        assert_eq!(answer.congress, Some(119));
        // The page was read and every row rejected. Returning the window
        // unfiltered is the failure this guards.
        assert_eq!(queries(&server).await.len(), 1);
    }

    #[tokio::test]
    async fn a_collection_with_nothing_in_it_is_an_empty_list_that_still_says_what_it_did() {
        // HTTP 200 with a body that parses to no bills: a Congress the API has
        // no rows for, and the shape a truncated answer arrives in.
        for body in [json!({ "bills": [] }), json!({})] {
            let server = MockServer::start().await;
            mount_page(&server, body.clone()).await;

            let answer = search_bills(&demo_state(&server), "act", Some(CONGRESS), 10)
                .await
                .expect("an answer");

            assert!(answer.bills.is_empty(), "{body}");
            assert!(answer.note.contains("A miss means"), "{body}");
            assert_eq!(answer.source, "congress.gov (DEMO_KEY)", "{body}");
        }
    }

    #[tokio::test]
    async fn an_html_error_page_at_http_200_is_a_bad_body_rather_than_an_empty_search() {
        // A gateway in front of the API answers 200 with a holding page. Read as
        // "no bills", the panel would say the Congress passed nothing.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200).set_body_string("<html><body>Try again</body></html>"),
            )
            .mount(&server)
            .await;

        let error = search_bills(&demo_state(&server), "act", Some(CONGRESS), 10)
            .await
            .expect_err("not JSON");
        assert_eq!(error.code, codes::BAD_UPSTREAM_BODY);
    }

    #[tokio::test]
    async fn a_search_with_no_congress_named_reads_the_one_sitting_now() {
        let server = MockServer::start().await;
        let now = current_congress();
        Mock::given(method("GET"))
            .and(path_matcher(format!("/bill/{now}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(fixture(BILLS_PAGE_JSON)))
            .mount(&server)
            .await;

        let answer = search_bills(&demo_state(&server), "act", None, 10)
            .await
            .expect("bills");
        assert_eq!(answer.congress, Some(now));
    }

    /* -------------------------------------------------------- the crawl */

    #[tokio::test]
    async fn the_shared_key_reads_one_page_where_a_key_of_its_own_reads_four() {
        // Four sequential requests exhaust api.data.gov's per-IP throttle:
        // measured from this container, a four-page crawl answers 429 while a
        // one-page crawl answers bills. A keyless deployment that crawled four
        // pages would show a rate-limit error instead of a shallower answer.
        let shared = MockServer::start().await;
        mount_four_pages(&shared).await;
        let answer = search_bills(&demo_state(&shared), "executive", Some(CONGRESS), 10)
            .await
            .expect("bills");
        assert_eq!(queries(&shared).await.len(), 1);
        assert_eq!(answer.source, "congress.gov (DEMO_KEY)");

        let keyed = MockServer::start().await;
        mount_four_pages(&keyed).await;
        let answer = search_bills(&keyed_state(&keyed), "executive", Some(CONGRESS), 10)
            .await
            .expect("bills");
        assert_eq!(queries(&keyed).await.len(), 4);
        assert_eq!(answer.source, "congress.gov");
    }

    #[tokio::test]
    async fn an_empty_query_reads_one_page_whatever_key_is_in_play() {
        // Nothing to match on, so the other three pages are 750 rows thrown
        // away and three requests spent to throw them.
        let server = MockServer::start().await;
        mount_four_pages(&server).await;

        let answer = search_bills(&keyed_state(&server), "   ", Some(CONGRESS), 3)
            .await
            .expect("bills");

        assert_eq!(queries(&server).await.len(), 1);
        assert_eq!(answer.bills.len(), 3);
    }

    #[tokio::test]
    async fn a_short_page_ends_the_crawl_rather_than_spending_a_request_to_be_told_so() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher(format!("/bill/{CONGRESS}")))
            .and(query_param("offset", "0"))
            .respond_with(ResponseTemplate::new(200).set_body_json(full_page(0)))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path_matcher(format!("/bill/{CONGRESS}")))
            .and(query_param("offset", "250"))
            .respond_with(ResponseTemplate::new(200).set_body_json(fixture(BILLS_PAGE_JSON)))
            .mount(&server)
            .await;
        // Offsets 500 and 750 are deliberately unmounted: reaching them is both
        // a 404 here and a wasted request against the throttle in production.

        search_bills(&keyed_state(&server), "executive", Some(CONGRESS), 1000)
            .await
            .expect("bills");

        assert_eq!(queries(&server).await.len(), 2);
    }

    #[tokio::test]
    async fn the_same_bill_on_two_pages_is_one_row() {
        // Congress.gov re-sorts between requests whenever a bill is acted on
        // mid-crawl, so page 2 and page 3 overlap. Four identical pages is the
        // extreme of that, and must not read as 1,000 bills.
        let server = MockServer::start().await;
        for page in 0..4usize {
            Mock::given(method("GET"))
                .and(path_matcher(format!("/bill/{CONGRESS}")))
                .and(query_param("offset", (page * PAGE).to_string()))
                .respond_with(ResponseTemplate::new(200).set_body_json(full_page(0)))
                .mount(&server)
                .await;
        }

        let answer = search_bills(&keyed_state(&server), "executive", Some(CONGRESS), 1000)
            .await
            .expect("bills");

        assert_eq!(queries(&server).await.len(), 4);
        assert_eq!(answer.bills.len(), PAGE);
    }

    #[tokio::test]
    async fn the_response_says_which_key_answered_and_how_wide_the_window_was() {
        // A reader who searched for a bill that exists and got nothing needs to
        // know they were shown one page of 250, not the whole Congress.
        let server = MockServer::start().await;
        mount_page(&server, fixture(BILLS_PAGE_JSON)).await;

        let shared = search_bills(&demo_state(&server), "act", Some(CONGRESS), 10)
            .await
            .expect("bills");
        assert_eq!(shared.source, "congress.gov (DEMO_KEY)");
        assert!(
            shared
                .note
                .contains("reads one page of 250 bills rather than four"),
            "{}",
            shared.note
        );
        assert!(
            shared.note.contains("Set CONGRESS_API_KEY to widen it."),
            "{}",
            shared.note
        );
        assert!(
            shared.note.contains(
                "A miss means \"not among the recently active bills\", not \"no such \
                          bill\"."
            ),
            "{}",
            shared.note
        );

        let keyed = search_bills(&keyed_state(&server), "act", Some(CONGRESS), 10)
            .await
            .expect("bills");
        assert_eq!(keyed.source, "congress.gov");
        assert!(!keyed.note.contains("DEMO_KEY"), "{}", keyed.note);
        assert!(keyed.note.contains("A miss means"), "{}", keyed.note);
    }

    #[tokio::test]
    async fn a_window_is_read_once_and_answered_out_of_the_cache_after_that() {
        // Three panels on one search is one request against a key that allows
        // ten an hour.
        let server = MockServer::start().await;
        mount_page(&server, fixture(BILLS_PAGE_JSON)).await;
        let state = demo_state(&server);

        for _ in 0..3 {
            search_bills(&state, "act", Some(CONGRESS), 10)
                .await
                .expect("bills");
        }
        assert_eq!(queries(&server).await.len(), 1);
    }

    /* -------------------------------------------------------- the throttle */

    #[tokio::test]
    async fn the_shared_keys_throttle_is_a_rate_limit_with_the_sign_up_link_not_an_outage() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(429).set_body_json(fixture(RATE_LIMITED_JSON)))
            .mount(&server)
            .await;

        let error = search_bills(&demo_state(&server), "act", Some(CONGRESS), 10)
            .await
            .expect_err("throttled");

        assert_eq!(error.code, codes::RATE_LIMITED);
        assert_eq!(error.status, Some(429));
        assert_eq!(
            error.message,
            "Congress.gov is rate-limiting the shared demonstration key"
        );
        // Served as 429 rather than a 502: an operator reading the board must
        // not go looking for an outage at the Library of Congress.
        assert_eq!(error.http_status().as_u16(), 429);
        let hint = error.hint.expect("a hint");
        assert!(hint.contains("https://api.congress.gov/sign-up/"), "{hint}");
        assert!(hint.contains("CONGRESS_API_KEY"), "{hint}");
        assert!(hint.contains("5,000 requests an hour"), "{hint}");
    }

    #[tokio::test]
    async fn a_deployment_with_its_own_key_is_not_told_to_set_the_key_it_already_set() {
        // The DEMO_KEY remediation is the wrong sentence for an operator who
        // holds a key: theirs is a 5,000-an-hour limit, and setting
        // CONGRESS_API_KEY again fixes nothing.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(429).set_body_json(fixture(RATE_LIMITED_JSON)))
            .mount(&server)
            .await;

        let error = search_bills(&keyed_state(&server), "act", Some(CONGRESS), 10)
            .await
            .expect_err("throttled");

        assert_eq!(error.status, Some(429));
        assert_eq!(error.code, codes::UPSTREAM_STATUS);
        let hint = error.hint.expect("a hint");
        assert!(!hint.contains("CONGRESS_API_KEY"), "{hint}");
        assert!(hint.contains("rate-limiting this IP"), "{hint}");
    }

    #[tokio::test]
    async fn a_throttle_that_clears_on_the_retry_still_answers() {
        // One retry is configured, and on a ten-an-hour shared key that is the
        // difference between a panel that renders and one that does not.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(429).set_body_json(fixture(RATE_LIMITED_JSON)))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        mount_page(&server, fixture(BILLS_PAGE_JSON)).await;

        let answer = search_bills(&demo_state(&server), "", Some(CONGRESS), 10)
            .await
            .expect("the retry answers");

        assert_eq!(answer.bills.len(), 5);
        assert_eq!(queries(&server).await.len(), 2);
    }

    /* --------------------------------------------------------- the detail */

    #[tokio::test]
    async fn reads_a_bill_with_its_summary_actions_and_committees() {
        let server = MockServer::start().await;
        mount_bill(&server).await;

        let detail = get_bill(&demo_state(&server), CONGRESS, "hr", "1")
            .await
            .expect("the bill resolves");

        let bill = &detail.bill;
        assert_eq!(bill.label, "HR 1 (119th)");
        assert_eq!(bill.congress, 119);
        assert_eq!(bill.bill_type, "hr");
        assert_eq!(bill.number, "1");
        assert_eq!(
            bill.title,
            "An act to provide for reconciliation pursuant to title II of H. Con. Res. 14."
        );
        assert_eq!(bill.origin_chamber, "House");
        assert_eq!(bill.introduced_date, "2025-05-20");
        assert_eq!(bill.sponsor, "Rep. Arrington, Jodey C. [R-TX-19]");
        assert_eq!(bill.sponsor_party, "R");
        assert_eq!(bill.sponsor_state, "TX");
        assert_eq!(bill.policy_area, "Economics and Public Finance");
        assert_eq!(bill.cosponsors, None);
        assert!(bill.became_law);
        // Checked against the link Congress.gov publishes for this bill itself,
        // rather than against the format string that built it: `legislationUrl`
        // is in the captured payload and this module never reads it.
        assert_eq!(
            bill.url,
            "https://www.congress.gov/bill/119th-congress/house-bill/1"
        );
        assert_eq!(
            fixture(BILL_JSON)["bill"]["legislationUrl"],
            json!(bill.url)
        );

        // Congress.gov serves actions newest first, which is the order the
        // panel reads top to bottom.
        assert_eq!(detail.actions.len(), 9);
        assert_eq!(detail.actions[0].date, "2025-07-04");
        assert_eq!(detail.actions[0].text, "Became Public Law No: 119-21.");
        assert_eq!(detail.actions[0].chamber, "");
        assert_eq!(detail.actions[4].text, "Presented to President.");
        assert_eq!(detail.actions[8].date, "2025-05-20");
        assert_eq!(
            detail.actions[8].text,
            "The House Committee on the Budget reported an original measure, H. Rept. 119-106, by \
             Mr. Arrington."
        );

        assert_eq!(detail.committees, ["Budget Committee · House"]);

        // The actions collection pages at 20 by default and this bill's history
        // runs to 59. Without the explicit ceiling the panel would show the
        // newest twenty and present them as the whole record.
        let sent = queries(&server).await;
        assert!(
            sent.contains(
                &"/bill/119/hr/1/actions?format=json&limit=250&api_key=DEMO_KEY".to_owned()
            ),
            "{sent:?}"
        );
    }

    #[tokio::test]
    async fn the_newest_summary_is_the_one_shown() {
        // Five versions are filed as a bill moves, oldest first. Showing the
        // introduced version of an enacted law describes a bill that no longer
        // exists.
        let server = MockServer::start().await;
        mount_bill(&server).await;

        let detail = get_bill(&demo_state(&server), CONGRESS, "hr", "1")
            .await
            .expect("the bill resolves");

        assert!(
            detail
                .summary
                .starts_with("A state with a payment error rate that is\n\nat least 6%"),
            "{}",
            detail.summary
        );
        assert!(
            !detail.summary.contains("One Big Beautiful Bill Act"),
            "the introduced version leaked through"
        );
        // `&nbsp;` is a space by the time it reaches the panel, not six
        // characters of markup.
        assert!(
            detail
                .summary
                .contains("fund requirements is the beginning of FY2028"),
            "{}",
            detail.summary
        );
        assert!(detail
            .summary
            .ends_with("if the error rate occurs in FY2026."));
        assert!(!detail.summary.contains('<'), "{}", detail.summary);
        assert!(!detail.summary.contains('&'), "{}", detail.summary);
    }

    #[tokio::test]
    async fn the_detail_answers_under_the_citation_the_reader_typed() {
        let server = MockServer::start().await;
        mount_bill(&server).await;

        let detail = get_bill(&demo_state(&server), CONGRESS, " H.R. ", "1")
            .await
            .expect("the bill resolves");

        // The label is rebuilt from the arguments, which is what makes a detail
        // panel agree with the search row that opened it — Congress.gov states
        // the type as `HR` and the URL wants `hr`.
        assert_eq!(detail.bill.label, "HR 1 (119th)");
        assert_eq!(detail.bill.bill_type, "hr");
        let sent = queries(&server).await;
        assert!(
            sent.contains(&"/bill/119/hr/1?format=json&api_key=DEMO_KEY".to_owned()),
            "{sent:?}"
        );
    }

    #[tokio::test]
    async fn a_bill_congress_gov_does_not_have_is_a_not_found_before_three_more_requests() {
        // A wrong Congress number answers HTTP 200 with no bill in it, which is
        // the whole reason this check exists.
        for body in [json!({ "bill": null }), json!({})] {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .respond_with(ResponseTemplate::new(200).set_body_json(body.clone()))
                .mount(&server)
                .await;

            let error = get_bill(&demo_state(&server), CONGRESS, "hr", "4000")
                .await
                .expect_err("no such bill");

            assert_eq!(error.code, codes::NOT_FOUND, "{body}");
            assert_eq!(
                error.message, "Congress.gov has no HR 4000 (119th)",
                "{body}"
            );
            assert!(
                error
                    .hint
                    .expect("a hint")
                    .contains("Check the Congress number"),
                "{body}"
            );
            // Summaries, actions and committees are three more requests against
            // a ten-an-hour throttle, spent on a bill that is not there.
            assert_eq!(queries(&server).await.len(), 1, "{body}");
        }
    }

    #[tokio::test]
    async fn a_sub_resource_that_fails_costs_its_own_panel_and_not_the_bill() {
        // A bill introduced this morning has no summary filed and no committee
        // referral yet. Losing the bill over that loses the reader the thing
        // they asked for.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/bill/119/hr/1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(fixture(BILL_JSON)))
            .mount(&server)
            .await;
        // Three different ways a sub-resource comes back useless: an outage, a
        // 404, and a body that parses to nothing. All three must cost their own
        // panel and no more.
        Mock::given(method("GET"))
            .and(path_matcher("/bill/119/hr/1/summaries"))
            .respond_with(ResponseTemplate::new(503).set_body_string("unavailable"))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path_matcher("/bill/119/hr/1/actions"))
            .respond_with(ResponseTemplate::new(404).set_body_string("not found"))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path_matcher("/bill/119/hr/1/committees"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
            .mount(&server)
            .await;

        let detail = get_bill(&demo_state(&server), CONGRESS, "hr", "1")
            .await
            .expect("the bill survives");

        assert_eq!(detail.bill.label, "HR 1 (119th)");
        assert_eq!(
            detail.bill.title,
            "An act to provide for reconciliation pursuant to title II of H. Con. Res. 14."
        );
        assert!(detail.bill.became_law);
        assert_eq!(detail.summary, "");
        assert!(detail.actions.is_empty());
        assert!(detail.committees.is_empty());
    }

    #[tokio::test]
    async fn an_action_with_no_text_is_dropped_rather_than_listed_as_a_blank_line() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/bill/119/hr/1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(fixture(BILL_JSON)))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path_matcher("/bill/119/hr/1/actions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "actions": [
                    { "actionDate": "2025-07-04", "text": "Became Public Law No: 119-21." },
                    { "actionDate": "2025-07-03" },
                    { "actionDate": "2025-07-03", "text": "   " },
                    { "text": "Introduced in House" }
                ]
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path_matcher("/bill/119/hr/1/summaries"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path_matcher("/bill/119/hr/1/committees"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
            .mount(&server)
            .await;

        let detail = get_bill(&demo_state(&server), CONGRESS, "hr", "1")
            .await
            .expect("the bill resolves");

        // The dateless row is still an action a reader can read; the two with
        // nothing to say are not.
        assert_eq!(detail.actions.len(), 2);
        assert_eq!(detail.actions[0].text, "Became Public Law No: 119-21.");
        assert_eq!(detail.actions[1].text, "Introduced in House");
        assert_eq!(detail.actions[1].date, "");
    }

    /* ---------------------------------------------------------- summaries */

    #[test]
    fn a_summary_is_shown_as_text_with_its_paragraphs_kept() {
        let payload: SummariesPayload =
            serde_json::from_str(SUMMARIES_JSON).expect("the fixture parses");
        let text = strip_html(
            payload
                .summaries
                .last()
                .expect("a summary")
                .text
                .as_deref()
                .expect("summary text"),
        );

        assert!(
            text.starts_with("A state with a payment error rate that is\n\nat least 6%"),
            "{text}"
        );
        assert!(
            text.ends_with("if the error rate occurs in FY2026."),
            "{text}"
        );
        assert!(!text.contains('<'), "{text}");
        assert!(!text.contains('&'), "{text}");
        // Only `<br>` and `</p>` break a line, so the `<ul>` above runs its
        // items into one another and into the paragraph after it.
        assert!(text.contains("must contribute 5%,at least 8%"), "{text}");
        assert!(text.contains("must contribute 15%.In general,"), "{text}");
    }

    #[test]
    fn a_line_break_is_a_line_break() {
        // Quoted from H.R. 1's enacted summary, which carries four of these.
        assert_eq!(
            strip_html("eligibility.&nbsp;<br/>OPM must develop a process"),
            "eligibility. \nOPM must develop a process"
        );
        assert_eq!(strip_html("<p>one</p><p>two</p>"), "one\n\ntwo");
        // Three paragraph breaks in a row is still one blank line.
        assert_eq!(
            strip_html("<p>one</p><p></p><p></p><p>two</p>"),
            "one\n\ntwo"
        );
    }

    #[test]
    fn an_ampersand_that_is_not_an_entity_is_left_where_it_is() {
        // "(A&amp;O)" is real: administrative and operating costs.
        assert_eq!(strip_html("<p>(A&amp;O) costs</p>"), "(A&O) costs");
        // No `;` within ten characters, so not an entity. Dropping the `&`
        // would silently rewrite the Library of Congress's own text.
        assert_eq!(
            strip_html("R&D spending in FY2028; and"),
            "R&D spending in FY2028; and"
        );
        // An entity the table does not carry is left as it was filed. Deleting
        // the `&` would turn "&sect;1701" into a different citation.
        assert_eq!(strip_html("&sect;1701"), "&sect;1701");
        assert_eq!(strip_html("&#8217;"), "\u{2019}");
        assert_eq!(strip_html("&nbsp;&amp;&lt;&gt;&quot;&#39;"), "&<>\"'");
    }

    #[test]
    fn a_summary_longer_than_the_panel_is_cut_at_a_character_and_not_through_one() {
        // H.R. 1's enacted summary runs to 173,872 characters in 174,002 bytes
        // — 61 of the difference is curly apostrophes. `String::truncate` panics
        // unless its index is a character boundary, and the panel gets nothing
        // when it does.
        let sentence = "the state\u{2019}s SNAP payment error rate. ";
        for pad in 0..sentence.len() {
            // Sweep the 8,000-byte cut across every offset in the sentence, so
            // it lands inside the apostrophe rather than beside it.
            let long = format!("{}{}", "x".repeat(pad), sentence.repeat(400));
            let text = strip_html(&long);
            assert!(text.len() <= 8000, "pad {pad}: {}", text.len());
            assert!(text.len() >= 7998, "pad {pad}: {}", text.len());
            // And it is the *opening* of the summary that survives. A cut that
            // kept the last 8,000 bytes would be the same length and a
            // different bill: Congress.gov leads with what the bill does.
            assert!(long.starts_with(text.as_str()), "pad {pad}");
        }
    }
}
