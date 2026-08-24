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
    let effective = if month == 1 && day < 3 { year - 1 } else { year };
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
            query.push(format!("{name}={}", urlencoding::encode(value)));
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
    format!("{} {number} ({})", bill_type.to_uppercase(), ordinal(congress))
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
        url: format!(
            "https://www.congress.gov/bill/{congress}th-congress/{}/{number}",
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
                ("sort", "updateDate+desc".to_owned()),
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
        .filter(|bill| seen.insert(format!("{}/{}/{}", bill.congress, bill.bill_type, bill.number)))
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
    let path = format!("/bill/{congress}/{bill_type}/{}", urlencoding::encode(number));

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

    let mut bill = to_bill(&ApiBill {
        congress: Some(congress),
        bill_type: Some(bill_type.to_owned()),
        number: Some(serde_json::Value::String(number.to_owned())),
        ..raw
    });
    // `to_bill` rebuilds the label from the arguments, which is what makes a
    // detail response agree with the search row that led to it.
    bill.label = bill_label(bill_type, number, congress);

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
    text.truncate(8000);
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
