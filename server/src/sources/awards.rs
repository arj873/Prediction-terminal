//! Awards — nominees and winners, from Wikidata.
//!
//! This is the single largest hole in the entertainment book. Kalshi's award
//! markets are ~61% of the entertainment volume that has no companion feed, and
//! `KXOSCARPIC` alone carries more open interest than anything the terminal
//! already covers. Their settlement sources name oscars.org, emmys.com,
//! grammy.com and thegameawards.com — none of which answer a datacentre IP:
//! oscars.org and its awards database both return 403 from a container, the way
//! fred.stlouisfed.org resets one.
//!
//! Wikidata does answer, and it holds the same facts as structured statements:
//!
//! ```text
//!   P166   award received      → the winner
//!   P1411  nominated for       → the nominee
//!   P585   point in time       → which ceremony, as a qualifier on the statement
//!   P1686  for work            → what a person was nominated *for*
//! ```
//!
//! Two things about this source change how the panel should be read.
//!
//! **Winners are reliable; nominee lists lag.** Editors record a winner within
//! minutes and fill the losing slate in over days or weeks. So the panel reports
//! the nominee count it actually got rather than implying a complete slate — a
//! partial ballot presented as whole is exactly the kind of wrong number that
//! still looks right.
//!
//! **A ceremony that has not happened is empty, and that is the answer.** An
//! open market settling at a future ceremony returns no rows, which is a note,
//! not a `not_found` — the same rule as a film with no Tomatometer yet.
//!
//! Queries go to the WDQS SPARQL endpoint rather than `wikidata.org/w/api.php`,
//! which rate-limits shared egress hard enough to be unusable. Entity search
//! still happens against the MediaWiki API — but *server-side*, through WDQS's
//! `wikibase:mwapi` service, so the request Wikimedia throttles comes from WDQS
//! and not from here.

use std::collections::HashMap;

use serde::Deserialize;
use terminal_core::types::{AwardEntry, AwardResult};

use crate::app::AppState;
use crate::cache::ttl;
use crate::error::{codes, Result, UpstreamError};
use crate::http::FetchOptions;

/// Wikimedia asks automated clients to identify themselves, and throttles the
/// ones that do not. This is the contact string, not camouflage — unlike the
/// scraped sources, WDQS is a public API being used exactly as intended.
const UA: &str = "prediction-terminal/1.0 (https://github.com/arj873/Prediction-terminal)";

/// Short names for the awards Kalshi actually lists, expanded to the label
/// Wikidata files them under.
///
/// Not a QID table on purpose. QIDs are opaque, need verifying one at a time,
/// and go stale silently when an item is merged; a label search resolves the
/// same thing and keeps working for the award Kalshi lists next month. The
/// aliases exist because "best picture" has to find the Academy Award and not
/// the dozen other bodies that hand out a prize by that name.
const ALIASES: &[(&str, &str)] = &[
    // ---- Academy Awards ---------------------------------------------------
    ("best picture", "Academy Award for Best Picture"),
    ("picture", "Academy Award for Best Picture"),
    ("oscar", "Academy Award for Best Picture"),
    ("best director", "Academy Award for Best Director"),
    ("director", "Academy Award for Best Director"),
    ("best actor", "Academy Award for Best Actor"),
    ("actor", "Academy Award for Best Actor"),
    ("best actress", "Academy Award for Best Actress"),
    ("actress", "Academy Award for Best Actress"),
    (
        "supporting actor",
        "Academy Award for Best Supporting Actor",
    ),
    (
        "supporting actress",
        "Academy Award for Best Supporting Actress",
    ),
    ("animated", "Academy Award for Best Animated Feature"),
    (
        "documentary",
        "Academy Award for Best Documentary Feature Film",
    ),
    (
        "international",
        "Academy Award for Best International Feature Film",
    ),
    ("cinematography", "Academy Award for Best Cinematography"),
    ("score", "Academy Award for Best Original Score"),
    ("song", "Academy Award for Best Original Song"),
    (
        "adapted screenplay",
        "Academy Award for Best Adapted Screenplay",
    ),
    (
        "original screenplay",
        "Academy Award for Best Original Screenplay",
    ),
    ("screenplay", "Academy Award for Best Original Screenplay"),
    ("visual effects", "Academy Award for Best Visual Effects"),
    ("makeup", "Academy Award for Best Makeup and Hairstyling"),
    // ---- Emmys ------------------------------------------------------------
    (
        "comedy series",
        "Primetime Emmy Award for Outstanding Comedy Series",
    ),
    (
        "drama series",
        "Primetime Emmy Award for Outstanding Drama Series",
    ),
    (
        "limited series",
        "Primetime Emmy Award for Outstanding Limited or Anthology Series",
    ),
    ("emmy", "Primetime Emmy Award for Outstanding Drama Series"),
    // ---- Grammys ----------------------------------------------------------
    ("album of the year", "Grammy Award for Album of the Year"),
    ("aoty", "Grammy Award for Album of the Year"),
    ("record of the year", "Grammy Award for Record of the Year"),
    ("roty", "Grammy Award for Record of the Year"),
    ("song of the year", "Grammy Award for Song of the Year"),
    ("soty", "Grammy Award for Song of the Year"),
    ("new artist", "Grammy Award for Best New Artist"),
    ("grammy", "Grammy Award for Album of the Year"),
    // ---- Games ------------------------------------------------------------
    // Wikidata's label is "The Game Awards − Game of the Year", with a U+2212
    // minus. Searching the label verbatim finds nothing; dropping the leading
    // article and the separator finds it every time.
    ("game of the year", "Game Awards Game of the Year"),
    ("goty", "Game Awards Game of the Year"),
    ("game awards", "Game Awards Game of the Year"),
    // ---- Other bodies -----------------------------------------------------
    (
        "golden globe",
        "Golden Globe Award for Best Motion Picture Drama",
    ),
    ("globe", "Golden Globe Award for Best Motion Picture Drama"),
    ("bafta", "BAFTA Award for Best Film"),
];

/// Awards the terminal advertises, for `AWRD` with no argument.
pub const AWARD_MENU: &[(&str, &str)] = &[
    ("best picture", "Oscar — Best Picture"),
    ("best director", "Oscar — Best Director"),
    ("best actor", "Oscar — Best Actor"),
    ("best actress", "Oscar — Best Actress"),
    ("supporting actor", "Oscar — Best Supporting Actor"),
    ("supporting actress", "Oscar — Best Supporting Actress"),
    ("animated", "Oscar — Best Animated Feature"),
    ("international", "Oscar — Best International Feature"),
    ("comedy series", "Emmy — Outstanding Comedy Series"),
    ("drama series", "Emmy — Outstanding Drama Series"),
    ("limited series", "Emmy — Outstanding Limited Series"),
    ("album of the year", "Grammy — Album of the Year"),
    ("record of the year", "Grammy — Record of the Year"),
    ("song of the year", "Grammy — Song of the Year"),
    ("new artist", "Grammy — Best New Artist"),
    ("game of the year", "The Game Awards — Game of the Year"),
    ("golden globe", "Golden Globe — Best Motion Picture, Drama"),
];

/* ------------------------------------------------------- sparql plumbing */

#[derive(Debug, Deserialize)]
struct SparqlResponse {
    results: Option<SparqlResults>,
}

#[derive(Debug, Deserialize)]
struct SparqlResults {
    bindings: Option<Vec<HashMap<String, SparqlValue>>>,
}

/// One cell of a SPARQL result row.
///
/// Public because `Binding` — the row type the parse helpers hand back — is
/// spelled in terms of it, and a public function cannot return a type its
/// callers cannot name.
#[derive(Debug, Deserialize)]
pub struct SparqlValue {
    value: Option<String>,
}

pub type Binding = HashMap<String, SparqlValue>;

fn bound(row: &Binding, key: &str) -> String {
    row.get(key)
        .and_then(|v| v.value.clone())
        .unwrap_or_default()
}

/// Strip the entity prefix off a Wikidata URI: `…/entity/Q42` → `Q42`.
fn qid(uri: &str) -> String {
    uri.rsplit("/entity/")
        .next()
        .unwrap_or(uri)
        .rsplit('/')
        .next()
        .unwrap_or(uri)
        .to_string()
}

fn looks_like_qid(text: &str) -> bool {
    text.len() > 1 && text.starts_with('Q') && text[1..].bytes().all(|b| b.is_ascii_digit())
}

/// A SPARQL string literal.
///
/// The award name reaches here from the command line and is interpolated into a
/// query rather than bound as a parameter — WDQS takes the query as one blob
/// over GET, so there is nowhere to bind. Backslashes and quotes therefore have
/// to be neutralised and control characters dropped, or a title with an
/// apostrophe becomes a syntax error at best.
fn literal(value: &str) -> String {
    let escaped: String = value
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    format!("\"{escaped}\"")
}

/// Parse a SPARQL results body into its rows.
///
/// Split out so every query below is testable against a captured response
/// without a network, which is what the rest of `sources/` does too.
pub fn parse_bindings(body: &str) -> Result<Vec<Binding>> {
    let parsed: SparqlResponse = serde_json::from_str(body).map_err(|err| {
        UpstreamError::new(
            format!("Wikidata's response did not parse: {err}"),
            codes::BAD_UPSTREAM_BODY,
        )
    })?;
    Ok(parsed.results.and_then(|r| r.bindings).unwrap_or_default())
}

async fn ask(state: &AppState, query: &str) -> Result<Vec<Binding>> {
    let url = format!(
        "{}?format=json&query={}",
        state.config().wikidata_sparql_base,
        urlencoding::encode(query)
    );
    let body = state
        .http()
        .fetch_text(
            &url,
            FetchOptions::new()
                .browser_headers(false)
                .header("User-Agent", UA)
                .header("Accept", "application/sparql-results+json")
                // WDQS answers a split query in well under a second when it is
                // healthy, and 502s or stalls when it is not. One retry, not
                // two: a panel makes *two* of these calls in sequence — resolve
                // the award, then read its record — so every attempt here is
                // paid twice over, and three attempts at thirty seconds is a
                // panel that sits blank for two minutes before admitting the
                // upstream is down.
                .timeout(std::time::Duration::from_secs(15))
                .retries(1),
        )
        .await?;
    parse_bindings(&body)
}

/* ---------------------------------------------------------- resolution */

pub fn expand_alias(raw: &str) -> String {
    let key: String = raw
        .trim()
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    ALIASES
        .iter()
        .find(|(alias, _)| *alias == key)
        .map(|(_, label)| (*label).to_string())
        .unwrap_or_else(|| raw.trim().to_string())
}

fn search_query(search: &str) -> String {
    // The `EXISTS` guard is what makes this a search for an *award* rather than
    // for a page about one: it keeps only items something has actually been
    // nominated for or has won, which no article, list or category satisfies.
    format!(
        r#"
      SELECT ?item ?itemLabel WHERE {{
        SERVICE wikibase:mwapi {{
          bd:serviceParam wikibase:api "EntitySearch" ;
                          wikibase:endpoint "www.wikidata.org" ;
                          mwapi:search {} ;
                          mwapi:language "en" .
          ?item wikibase:apiOutputItem mwapi:item .
        }}
        FILTER(EXISTS {{ [] ps:P166 ?item }} || EXISTS {{ [] ps:P1411 ?item }})
        SERVICE wikibase:label {{ bd:serviceParam wikibase:language "en" }}
      }} LIMIT 1"#,
        literal(search)
    )
}

/// Award name → Wikidata item.
///
/// Cached for a catalogue TTL: an award's identity does not change, and this is
/// the round trip that would otherwise double every panel load.
pub async fn resolve_award(state: &AppState, name: &str) -> Result<(String, String)> {
    let search = expand_alias(name);
    if search.is_empty() {
        return Err(UpstreamError::bad_request("Missing award name")
            .with_hint("Usage: `AWRD <award> [year]`, e.g. `AWRD best picture 2026`."));
    }

    let key = format!("awards:id:{}", search.to_lowercase());
    let found = state
        .cache()
        .cached(&key, ttl::CATALOGUE, || async {
            let rows = ask(state, &search_query(&search)).await?;
            let first = rows.first();
            let id = first
                .map(|row| qid(&bound(row, "item")))
                .unwrap_or_default();

            if !looks_like_qid(&id) {
                return Err(UpstreamError::not_found(format!(
                    "Wikidata has no award matching \"{name}\""
                ))
                .with_hint(
                    "Try the full name — `AWRD \"Academy Award for Best Picture\"` — or one \
                     of the short forms: best picture, drama series, album of the year, \
                     game of the year.",
                ));
            }

            let label = first.map(|row| bound(row, "itemLabel")).unwrap_or_default();
            Ok((
                id,
                if label.is_empty() {
                    search.clone()
                } else {
                    label
                },
            ))
        })
        .await?;

    Ok((*found).clone())
}

/* --------------------------------------------------------- the records */

/// Rows to pull from each side before merging.
///
/// Fixed, and deliberately not the caller's `limit`. The two sides are merged,
/// deduped and re-sorted afterwards, so truncating inside the query truncates
/// the wrong thing: a small limit returns the most recent nominees and the most
/// recent winners, which overlap almost entirely and leave a single row. It also
/// keeps `limit` out of the cache key, so one fetch serves any row count.
///
/// 600 comfortably covers the ~100 years of a long-running Academy Award
/// category.
const FETCH_CAP: usize = 600;

pub fn assert_year(raw: &str) -> Result<u32> {
    let year: u32 = raw.trim().parse().map_err(|_| {
        UpstreamError::bad_request(format!("\"{raw}\" is not a ceremony year"))
            .with_hint("Pass a four-digit year, e.g. `AWRD best picture 2026`.")
    })?;

    // Wide enough for the first Academy Awards (1929) and any ceremony a market
    // could plausibly reference, narrow enough to reject a stray ticker.
    if !(1900..=2100).contains(&year) {
        return Err(
            UpstreamError::bad_request(format!("\"{raw}\" is not a ceremony year"))
                .with_hint("Pass a four-digit year, e.g. `AWRD best picture 2026`."),
        );
    }
    Ok(year)
}

fn side_query(award_id: &str, property: &str, year: Option<u32>) -> String {
    // A year filter needs a bound date, so it also selects for dated
    // statements. Without one the date stays OPTIONAL: an undated statement is
    // still a real record, and dropping it silently would understate the field.
    let when = match year {
        None => "OPTIONAL { ?st pq:P585 ?when }".to_string(),
        Some(y) => format!(
            r#"?st pq:P585 ?when .
       FILTER(?when >= "{y}-01-01"^^xsd:dateTime && ?when < "{}-01-01"^^xsd:dateTime)"#,
            y + 1
        ),
    };

    format!(
        r#"
    SELECT ?name ?nameLabel ?workLabel ?when WHERE {{
      ?name p:{property} ?st .
      ?st ps:{property} wd:{award_id} .
      {when}
      OPTIONAL {{ ?st pq:P1686 ?work }}
      SERVICE wikibase:label {{ bd:serviceParam wikibase:language "en" }}
    }}
    ORDER BY DESC(?when)
    LIMIT {FETCH_CAP}"#
    )
}

/// Turn one side's rows into entries.
pub fn parse_side(rows: &[Binding], won: bool) -> Vec<AwardEntry> {
    rows.iter()
        .map(|row| {
            let label = bound(row, "nameLabel");
            let work = bound(row, "workLabel");
            let iso = bound(row, "when");
            let year: Option<u32> = iso.get(..4).and_then(|y| y.parse().ok());

            AwardEntry {
                id: qid(&bound(row, "name")),
                // The label service echoes the QID back when an item has no
                // English label. That is not a name, and showing it as one is
                // worse than nothing.
                name: if looks_like_qid(&label) {
                    String::new()
                } else {
                    label
                },
                work: if looks_like_qid(&work) {
                    String::new()
                } else {
                    work
                },
                won,
                year,
            }
        })
        .collect()
}

/// Dedupe key: one person can be nominated twice in a year, for different works.
fn key_of(entry: &AwardEntry) -> String {
    format!(
        "{}|{}|{}",
        entry.id,
        entry.year.map(|y| y.to_string()).unwrap_or_default(),
        entry.work
    )
}

/// Merge the two sides on the recipient.
///
/// A winner is almost always recorded as a nominee too, and listing them twice
/// would double the apparent field.
pub fn merge_sides(nominees: Vec<AwardEntry>, winners: Vec<AwardEntry>) -> Vec<AwardEntry> {
    let mut order: Vec<String> = Vec::new();
    let mut merged: HashMap<String, AwardEntry> = HashMap::new();

    for entry in nominees.into_iter().chain(winners) {
        let key = key_of(&entry);
        match merged.get_mut(&key) {
            // Winners land second, so `won` only ever gets promoted, never cleared.
            Some(existing) => existing.won |= entry.won,
            None => {
                order.push(key.clone());
                merged.insert(key, entry);
            }
        }
    }

    let mut entries: Vec<AwardEntry> = order
        .into_iter()
        .filter_map(|key| merged.remove(&key))
        .filter(|entry| !entry.name.is_empty())
        .collect();

    entries.sort_by(|a, b| {
        b.year
            .cmp(&a.year)
            .then_with(|| b.won.cmp(&a.won))
            .then_with(|| a.name.cmp(&b.name))
    });
    entries
}

/// What the panel should say about what it is showing.
///
/// An empty ceremony is the interesting case: for an award Kalshi is trading, no
/// rows almost always means the nominations have not been announced, which is
/// the state the market exists to price — not a failure to find anything.
pub fn note(entries: &[AwardEntry], year: Option<u32>) -> String {
    if entries.is_empty() {
        return match year {
            None => "Wikidata holds no recipients for this award.".to_string(),
            Some(y) => format!(
                "No {y} ceremony recorded yet — nominations are announced weeks before \
                 the ceremony."
            ),
        };
    }

    let winners = entries.iter().filter(|e| e.won).count();
    if year.is_some() && winners == 0 {
        return format!("{} nominees recorded, no winner yet.", entries.len());
    }
    String::new()
}

/// Nominees and winners for an award, optionally for one ceremony.
pub async fn get_award(
    state: &AppState,
    name: &str,
    year: Option<u32>,
    limit: usize,
) -> Result<AwardResult> {
    let (award_id, award) = resolve_award(state, name).await?;
    let key = format!(
        "awards:{award_id}:{}",
        year.map(|y| y.to_string()).unwrap_or_else(|| "all".into())
    );

    let full = state
        .cache()
        .cached(&key, ttl::AWARDS, || async {
            // Two queries rather than one UNION. The combined form joins two
            // unbound statement patterns and then runs the label service over
            // the product of both, which WDQS answers with a 502 or a timeout
            // for anything as heavily awarded as Best Picture. Split, each half
            // returns in well under a second — and they run concurrently.
            let (winners, nominees) = futures::future::join(
                ask(state, &side_query(&award_id, "P166", year)),
                ask(state, &side_query(&award_id, "P1411", year)),
            )
            .await;

            let entries = merge_sides(parse_side(&nominees?, false), parse_side(&winners?, true));

            let mut years: Vec<u32> = entries.iter().filter_map(|e| e.year).collect();
            years.sort_unstable_by(|a, b| b.cmp(a));
            years.dedup();

            Ok(AwardResult {
                award_id: award_id.clone(),
                award: award.clone(),
                year,
                note: note(&entries, year),
                entries,
                years,
                source_url: format!("https://www.wikidata.org/wiki/{award_id}"),
            })
        })
        .await?;

    let mut result = (*full).clone();
    // `years` stays whole — it is the picker of what else is on record, and
    // trimming it to the visible rows would hide the ceremonies you can ask for.
    result.entries.truncate(limit);
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(pairs: &[(&str, &str)]) -> Binding {
        pairs
            .iter()
            .map(|(k, v)| {
                (
                    (*k).to_string(),
                    SparqlValue {
                        value: Some((*v).to_string()),
                    },
                )
            })
            .collect()
    }

    #[test]
    fn expands_the_short_names_kalshi_lists_markets_under() {
        assert_eq!(
            expand_alias("best picture"),
            "Academy Award for Best Picture"
        );
        assert_eq!(
            expand_alias("  BEST   PICTURE "),
            "Academy Award for Best Picture"
        );
        assert_eq!(expand_alias("goty"), "Game Awards Game of the Year");
        // Anything unrecognised is passed through as a search term.
        assert_eq!(expand_alias("Hugo Award"), "Hugo Award");
    }

    #[test]
    fn every_advertised_award_resolves_to_an_alias() {
        for (key, label) in AWARD_MENU {
            assert_ne!(
                expand_alias(key),
                *key,
                "{label} is advertised but its key {key} expands to nothing"
            );
        }
    }

    #[test]
    fn reads_a_qid_out_of_an_entity_uri() {
        assert_eq!(qid("http://www.wikidata.org/entity/Q102427"), "Q102427");
        assert_eq!(qid("Q42"), "Q42");
        assert!(looks_like_qid("Q102427"));
        assert!(!looks_like_qid("Academy Award"));
        assert!(!looks_like_qid("Q"));
    }

    #[test]
    fn neutralises_a_quote_in_an_award_name() {
        // The name reaches the query as a blob over GET, so there is nowhere to
        // bind it. A title with an apostrophe must not become a syntax error.
        assert_eq!(literal(r#"Say "hi""#), r#""Say \"hi\"""#);
        assert_eq!(literal(r"back\slash"), r#""back\\slash""#);
        assert_eq!(literal("new\nline"), r#""new line""#);
    }

    #[test]
    fn shows_no_name_rather_than_the_qid_the_label_service_echoes() {
        // An item with no English label comes back as its own QID. Printing
        // that in a name column is worse than printing nothing.
        let rows = vec![row(&[
            ("name", "http://www.wikidata.org/entity/Q999"),
            ("nameLabel", "Q999"),
            ("when", "2026-03-15T00:00:00Z"),
        ])];
        let entries = parse_side(&rows, true);
        assert_eq!(entries[0].name, "");
        assert_eq!(entries[0].id, "Q999");
        assert_eq!(entries[0].year, Some(2026));
    }

    #[test]
    fn keeps_an_undated_statement_because_it_is_still_a_record() {
        let rows = vec![row(&[
            ("name", "http://www.wikidata.org/entity/Q1"),
            ("nameLabel", "Someone"),
        ])];
        let entries = parse_side(&rows, false);
        assert_eq!(entries[0].year, None);
    }

    #[test]
    fn promotes_a_nominee_to_a_winner_rather_than_listing_them_twice() {
        let nominees = parse_side(
            &[
                row(&[
                    ("name", ".../entity/Q1"),
                    ("nameLabel", "Oppenheimer"),
                    ("when", "2024-03-10T00:00:00Z"),
                ]),
                row(&[
                    ("name", ".../entity/Q2"),
                    ("nameLabel", "Barbie"),
                    ("when", "2024-03-10T00:00:00Z"),
                ]),
            ],
            false,
        );
        let winners = parse_side(
            &[row(&[
                ("name", ".../entity/Q1"),
                ("nameLabel", "Oppenheimer"),
                ("when", "2024-03-10T00:00:00Z"),
            ])],
            true,
        );

        let merged = merge_sides(nominees, winners);
        assert_eq!(merged.len(), 2, "the winner was listed twice");
        assert!(merged[0].won, "the winner should sort first within a year");
        assert_eq!(merged[0].name, "Oppenheimer");
        assert!(!merged[1].won);
    }

    #[test]
    fn counts_two_nominations_in_one_year_for_different_works_separately() {
        let nominees = parse_side(
            &[
                row(&[
                    ("name", ".../entity/Q1"),
                    ("nameLabel", "A Director"),
                    ("workLabel", "First Film"),
                    ("when", "2024-03-10T00:00:00Z"),
                ]),
                row(&[
                    ("name", ".../entity/Q1"),
                    ("nameLabel", "A Director"),
                    ("workLabel", "Second Film"),
                    ("when", "2024-03-10T00:00:00Z"),
                ]),
            ],
            false,
        );
        assert_eq!(merge_sides(nominees, Vec::new()).len(), 2);
    }

    #[test]
    fn sorts_newest_ceremony_first_and_the_winner_above_the_slate() {
        let entries = merge_sides(
            parse_side(
                &[
                    row(&[
                        (("name"), ".../entity/Q1"),
                        ("nameLabel", "Older"),
                        ("when", "2020-01-01T00:00:00Z"),
                    ]),
                    row(&[
                        (("name"), ".../entity/Q2"),
                        ("nameLabel", "Zed"),
                        ("when", "2024-01-01T00:00:00Z"),
                    ]),
                    row(&[
                        (("name"), ("/entity/Q3")),
                        ("nameLabel", "Alice"),
                        ("when", "2024-01-01T00:00:00Z"),
                    ]),
                ],
                false,
            ),
            Vec::new(),
        );
        assert_eq!(
            entries.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(),
            ["Alice", "Zed", "Older"],
            "within a year, ties break on name"
        );
    }

    #[test]
    fn says_a_ceremony_has_not_happened_rather_than_finding_nothing() {
        // The state an open market exists to price.
        let empty: Vec<AwardEntry> = Vec::new();
        assert!(note(&empty, Some(2027)).contains("No 2027 ceremony recorded yet"));
        assert!(note(&empty, None).contains("no recipients"));
    }

    #[test]
    fn reports_the_slate_it_actually_got() {
        // Editors record a winner within minutes and fill the losing slate in
        // over days. A partial ballot presented as whole looks right and is not.
        let nominees = parse_side(
            &[
                row(&[
                    ("name", ".../entity/Q1"),
                    ("nameLabel", "One"),
                    ("when", "2026-01-01T00:00:00Z"),
                ]),
                row(&[
                    ("name", ".../entity/Q2"),
                    ("nameLabel", "Two"),
                    ("when", "2026-01-01T00:00:00Z"),
                ]),
            ],
            false,
        );
        let entries = merge_sides(nominees, Vec::new());
        assert_eq!(
            note(&entries, Some(2026)),
            "2 nominees recorded, no winner yet."
        );
    }

    #[test]
    fn says_nothing_once_a_winner_is_on_record() {
        let entries = merge_sides(
            Vec::new(),
            parse_side(
                &[row(&[
                    ("name", ".../entity/Q1"),
                    ("nameLabel", "Winner"),
                    ("when", "2024-01-01T00:00:00Z"),
                ])],
                true,
            ),
        );
        assert_eq!(note(&entries, Some(2024)), "");
    }

    #[test]
    fn holds_the_year_to_a_plausible_ceremony() {
        assert_eq!(assert_year("2026").unwrap(), 2026);
        assert_eq!(assert_year(" 1929 ").unwrap(), 1929);
        assert!(assert_year("KXOSCARPIC").is_err());
        assert!(assert_year("1800").is_err());
        assert!(assert_year("3000").is_err());
    }

    #[test]
    fn asks_for_a_dated_statement_only_when_a_year_was_named() {
        let all = side_query("Q102427", "P166", None);
        assert!(all.contains("OPTIONAL { ?st pq:P585 ?when }"));

        let one = side_query("Q102427", "P166", Some(2026));
        assert!(one.contains(r#""2026-01-01"^^xsd:dateTime"#));
        assert!(one.contains(r#""2027-01-01"^^xsd:dateTime"#));
        assert!(!one.contains("OPTIONAL { ?st pq:P585 ?when }"));
    }

    #[test]
    fn reads_the_rows_out_of_a_sparql_results_body() {
        let body = r#"{"head":{"vars":["item"]},"results":{"bindings":[
          {"item":{"type":"uri","value":"http://www.wikidata.org/entity/Q102427"},
           "itemLabel":{"type":"literal","value":"Academy Award for Best Picture"}}
        ]}}"#;
        let rows = parse_bindings(body).expect("bindings");
        assert_eq!(rows.len(), 1);
        assert_eq!(qid(&bound(&rows[0], "item")), "Q102427");
        assert_eq!(
            bound(&rows[0], "itemLabel"),
            "Academy Award for Best Picture"
        );
    }

    #[test]
    fn reads_an_empty_result_set_as_no_rows_not_as_a_failure() {
        let body = r#"{"head":{"vars":["item"]},"results":{"bindings":[]}}"#;
        assert!(parse_bindings(body).expect("bindings").is_empty());
    }
}
