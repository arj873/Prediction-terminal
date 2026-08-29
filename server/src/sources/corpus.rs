//! Search and ranking over a venue's open-event snapshot.
//!
//! Every venue answers the same three questions — what is open, what matches
//! these words, what is busiest — and none of them offers an endpoint that does
//! it well. Kalshi has no search at all; Polymarket International's ranks by its
//! own relevance and cannot be filtered; Polymarket US's matched "Seeman Jan vs
//! Dufek Jakub Jr" for the query `fed`, and Gemini's answered it with baseball.
//! So each source crawls its catalogue into a [`Corpus`] once per TTL and the
//! ranking lives here, identical for every venue, which is also what makes
//! cross-venue results comparable: the same query scores the same way whoever is
//! listing the market.

use std::cmp::Ordering;
use std::sync::Arc;
use std::time::Instant;

use regex::Regex;
use terminal_core::types::{Market, MoverSort, Venue, VenueEvent};
use terminal_core::util::round_to;

use crate::app::{AppState, CorpusRecord};
use crate::error::{Result, UpstreamError};

/// The shape a search answers in.
///
/// These are wire types: they are what `/api/venue/:venue/search` serialises,
/// and [`terminal_core::types`] is what the client's TypeScript is generated
/// from. Declaring them a second time here is how the two halves of the
/// contract drift apart, so this module re-exports the one definition rather
/// than keeping its own.
pub use terminal_core::types::{EventSearchHit, EventSummary, SearchResponse};

/// How many hits `SRCH` and how many rows `TOP` return when nobody says.
pub const DEFAULT_LIMIT: usize = 25;

/// One venue's open universe, crawled once per TTL.
#[derive(Debug, Clone)]
pub struct Corpus {
    pub venue: Venue,
    pub events: Vec<VenueEvent>,
    pub markets: Vec<Market>,
    /// When the crawl finished. Every age reported to the client is measured
    /// from here, so a panel can say how stale the universe it is showing is.
    pub built_at: Instant,
    /// True when the crawl hit its page cap before the catalogue ran out. The
    /// panel says so rather than presenting a partial universe as the whole one.
    pub truncated: bool,
}

impl Corpus {
    /// A snapshot built now.
    #[must_use]
    pub fn new(
        venue: Venue,
        events: Vec<VenueEvent>,
        markets: Vec<Market>,
        truncated: bool,
    ) -> Self {
        Self {
            venue,
            events,
            markets,
            built_at: Instant::now(),
            truncated,
        }
    }

    /// Age of the snapshot in whole seconds.
    #[must_use]
    pub fn age_seconds(&self) -> f64 {
        round_to(self.built_at.elapsed().as_secs_f64(), 0)
    }
}

/// Note what a catalogue read did, so `/api/health` can say why a search was
/// empty.
///
/// Called by each venue on the one function its own reads funnel through, and
/// on the result rather than inside the cache closure: a cache *hit* never runs
/// the closure, and "the catalogue is loaded" is exactly what a hit means. The
/// age reported is the snapshot's own, so re-recording a hit cannot make a
/// stale catalogue look freshly crawled.
pub fn record(state: &AppState, venue: Venue, result: &Result<Arc<Corpus>>) {
    state.record_corpus(
        venue,
        match result {
            Ok(corpus) => CorpusRecord::Ok {
                events: corpus.events.len() as u32,
                markets: corpus.markets.len() as u32,
                truncated: corpus.truncated,
                built_at: corpus.built_at,
            },
            Err(err) => CorpusRecord::Failed {
                error: err.message.clone(),
                code: err.code.clone(),
                hint: err.hint.clone(),
            },
        },
    );
}

/// Add up a figure across a ladder, keeping "unpublished" distinct from "zero".
///
/// A Polymarket US event has no volume anywhere in its public catalogue. Summing
/// that to `0` would rank it below a genuinely dead Kalshi market and print a
/// confident zero in a column that should read `--`.
pub fn sum_or_null(markets: &[Market], pick: impl Fn(&Market) -> Option<f64>) -> Option<f64> {
    let mut total = 0.0;
    let mut seen = false;
    for market in markets {
        let Some(value) = pick(market) else { continue };
        total += value;
        seen = true;
    }
    seen.then_some(total)
}

/// `\b` immediately before `term`, with the term itself taken literally.
///
/// The pattern is built from whatever the reader typed, so the term is escaped:
/// a title like `Dune(2026)` is searchable by typing `(2026)`, which as a raw
/// pattern is an unterminated group. `(?-u:\b)` keeps JavaScript's ASCII-only
/// reading of a word boundary, so a query scores here exactly what it scored
/// there.
fn word_boundary(term: &str) -> Option<Regex> {
    Regex::new(&format!(r"(?-u:\b){}", regex::escape(term))).ok()
}

/// `\b` on both sides of `term` — the whole word and nothing containing it.
///
/// Distinct from [`word_boundary`], which anchors only the front and is what the
/// scoring bonus uses. A leading `\b` alone is not enough to make a two-letter
/// term safe: `\bus` still matches "used" and "USA".
fn whole_word(term: &str) -> Option<Regex> {
    Regex::new(&format!(r"(?-u:\b){}(?-u:\b)", regex::escape(term))).ok()
}

/// Descending, with an unpublished figure read as zero *for the comparison
/// only* — it is still reported as `None`.
fn by_desc(a: Option<f64>, b: Option<f64>) -> Ordering {
    b.unwrap_or(0.0)
        .partial_cmp(&a.unwrap_or(0.0))
        .unwrap_or(Ordering::Equal)
}

/// How many words a search may name.
///
/// Every term is matched against every event in a ten-thousand-event corpus, so
/// the work is the product of the two and only one of them is bounded. Sixteen
/// is far past any query a person types and far short of one that costs
/// anything; [`refuse_overlong_query`] turns the rest away at the door.
pub const MAX_TERMS: usize = 16;

/// Refuse a query with more words than [`MAX_TERMS`].
///
/// Refused rather than truncated, because silently searching for some of what
/// was asked returns a confident answer to a different question. The caller is
/// told, and can shorten it.
pub fn refuse_overlong_query(query: &str) -> Result<()> {
    let words = query.split_whitespace().count();
    if words > MAX_TERMS {
        return Err(UpstreamError::bad_request(format!(
            "Search takes at most {MAX_TERMS} words; this one had {words}"
        ))
        .with_hint("Narrow the query — the most distinctive few words rank best."));
    }
    Ok(())
}

/// Words too common to carry a query.
///
/// Dropped from what a hit has to contain — `will the fed cut rates` is a
/// question about the Fed, not about "will" — but kept in the phrase the
/// whole-phrase bonus is measured against, so a title wording it exactly still
/// wins. A query that is nothing but these keeps them: searching nothing at all
/// is worse than searching badly.
const STOP_WORDS: &[&str] = &[
    "a", "an", "and", "are", "at", "be", "by", "do", "does", "for", "how", "in", "is", "of", "on",
    "or", "the", "to", "vs", "what", "when", "which", "who", "will",
];

/// A term of at most this many characters must land on a word boundary.
///
/// `us` sits inside "Klaus", "August" and "Arborelius", so as a bare substring
/// it answered `SRCH us election` with the next Pope and the next Secretary
/// General of NATO. Longer terms keep substring matching, which is what lets
/// `fed` find "federal".
const BOUNDARY_ANCHORED_LEN: usize = 2;

/// Points for a term that appears as it was typed.
const TERM_SCORE: u32 = 10;

/// Points for a term that appears only in its other number.
///
/// Below [`TERM_SCORE`] because the stem is a guess: `cuts` should find "rate
/// cut", but `news` also yields `new`, and "New York" is not news.
const VARIANT_SCORE: u32 = 8;

/// Points for a term whose only home is the category bucket.
const CATEGORY_SCORE: u32 = 2;

/// How a term was found: as typed, or only in its other number.
#[derive(Debug, Clone, Copy)]
enum Found {
    Exact,
    Variant,
}

/// What one query word looks for.
///
/// Built once per search rather than once per event: none of it depends on
/// which of the ten thousand events is being scored.
struct Term {
    /// The word as typed first, then its singular or plural. A raw substring
    /// test is asymmetric — `rate` is inside "rates", but `rates` is not inside
    /// "rate" — which is why `SRCH fed rates` found none of the fourteen "Fed
    /// funds rate" events that were open at the time.
    forms: Vec<String>,
    /// `\b`-anchored pattern per form, for the boundary bonus.
    boundaries: Vec<Option<Regex>>,
    /// Whole-word pattern per form, used as the gate for a short term. `Some`
    /// only when [`Term::anchored`], because nothing else reads it.
    wholes: Vec<Option<Regex>>,
    /// True when only a whole-word match counts. See [`BOUNDARY_ANCHORED_LEN`].
    anchored: bool,
}

impl Term {
    fn new(word: &str) -> Self {
        let forms = forms_of(word);
        let boundaries = forms.iter().map(|form| word_boundary(form)).collect();
        let anchored = word.chars().count() <= BOUNDARY_ANCHORED_LEN;
        let wholes = forms
            .iter()
            .map(|form| anchored.then(|| whole_word(form)).flatten())
            .collect();
        Self {
            forms,
            boundaries,
            wholes,
            anchored,
        }
    }

    /// How this term appears in `hay`, if it does at all.
    fn find(&self, hay: &str) -> Option<Found> {
        let mut found = None;
        for (index, form) in self.forms.iter().enumerate() {
            let present = if self.anchored {
                self.wholes[index]
                    .as_ref()
                    .is_some_and(|pattern| pattern.is_match(hay))
            } else {
                hay.contains(form.as_str())
            };
            if !present {
                continue;
            }
            // The word as typed is always `forms[0]`, and nothing beats it.
            if index == 0 {
                return Some(Found::Exact);
            }
            found = Some(Found::Variant);
        }
        found
    }

    /// Does the term start a word anywhere in `hay`?
    fn on_boundary(&self, hay: &str) -> bool {
        self.boundaries
            .iter()
            .flatten()
            .any(|pattern| pattern.is_match(hay))
    }

    /// Does `text` begin with the term, in any of its spellings?
    fn starts(&self, text: &str) -> bool {
        self.forms
            .iter()
            .any(|form| text.starts_with(form.as_str()))
    }
}

/// The word as typed, plus its other number.
///
/// Only the spellings English actually uses: `boxes` yields `box`, but `rates`
/// must not yield `rat`, which is inside "ratio" and "generate".
fn forms_of(word: &str) -> Vec<String> {
    let mut forms = vec![word.to_string()];

    if let Some(stem) = word.strip_suffix("es") {
        if stem.len() >= 2
            && (stem.ends_with(['s', 'x', 'z']) || stem.ends_with("ch") || stem.ends_with("sh"))
        {
            forms.push(stem.to_string());
        }
    }
    // Only when the `-es` rule did not already claim it, so `boxes` gives `box`
    // and not also `boxe`.
    if forms.len() == 1 {
        if let Some(stem) = word.strip_suffix('s') {
            if stem.len() >= 3 && !stem.ends_with('s') {
                forms.push(stem.to_string());
            }
        }
    }
    if !word.ends_with('s') {
        forms.push(format!("{word}s"));
    }

    forms
}

/// One event's standing, before its ladder is worth cloning.
struct Scored {
    event: usize,
    matched: u32,
    score: u32,
    volume24h: Option<f64>,
}

/// Rank open events against a free-text query.
///
/// Each word is looked for in the event's ticker, title, sub-title and its
/// markets' strike labels, in either number. Scoring rewards ticker hits,
/// title-prefix hits and word-boundary hits, and a whole-phrase match outranks
/// scattered terms.
///
/// Two rules keep the answer honest. Events are ordered by *how many* of the
/// query's words they matched before anything else, so a full match can never
/// be pushed down the page by a partial one — but a partial carrying most of
/// the query is still shown, and reports itself as one, because a single stray
/// word should not hide the event the rest of the query describes. And a word
/// found only in the event's category earns almost nothing: the category is a
/// dozen buckets shared across the whole catalogue, so ranking it like a title
/// answered `SRCH election` with the next Pope.
pub fn search_corpus(snapshot: &Corpus, query: &str, limit: usize) -> SearchResponse {
    let lowered = query.to_lowercase();
    // The routes refuse an over-long query before reaching here; the cap is
    // repeated as a backstop so no future caller can reintroduce the cost.
    let words: Vec<&str> = lowered.split_whitespace().take(MAX_TERMS).collect();
    // Every word, stop-words included: a title that says the query verbatim
    // should take the phrase bonus for saying it verbatim.
    let phrase = words.join(" ");

    let carrying: Vec<&str> = words
        .iter()
        .copied()
        .filter(|word| !STOP_WORDS.contains(word))
        .collect();
    // A query that is nothing but stop-words keeps them. `SRCH the` searching
    // for nothing at all would answer with the entire catalogue.
    let terms: Vec<Term> = if carrying.is_empty() {
        &words
    } else {
        &carrying
    }
    .iter()
    .map(|word| Term::new(word))
    .collect();

    // How much of the query a partial has to carry to be worth showing. Zero
    // terms is the empty query, which matches the whole corpus unranked.
    let least = terms.len().div_ceil(2) as u32;

    // Scored first, materialised second. Dropping the all-terms gate means one
    // common word can leave thousands of events standing, and cloning every
    // ladder only to throw all but `limit` of them away was the cost of that.
    let mut scored: Vec<Scored> = Vec::new();

    for (index, event) in snapshot.events.iter().enumerate() {
        let ticker = event.event_ticker.to_lowercase();
        let title = event.title.to_lowercase();
        let category = event.category.to_lowercase();
        let strikes = event
            .markets
            .iter()
            .map(|m| m.yes_sub_title.as_str())
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase();
        // The fields the question is actually asked in. The category is held
        // out of this and scored separately below.
        let content = format!(
            "{ticker} {title} {} {strikes}",
            event.sub_title.to_lowercase()
        );

        let mut score: u32 = 0;
        let mut matched: u32 = 0;

        for term in &terms {
            if let Some(found) = term.find(&content) {
                matched += 1;
                score += match found {
                    Found::Exact => TERM_SCORE,
                    Found::Variant => VARIANT_SCORE,
                };
                if term.find(&ticker).is_some() {
                    score += 12;
                }
                if term.starts(&title) {
                    score += 8;
                }
                if term.on_boundary(&content) {
                    score += 6;
                }
            } else if term.find(&category).is_some() {
                // Still findable — a papal conclave really is filed under
                // `elections` — but never ranked with a title that asks it.
                matched += 1;
                score += CATEGORY_SCORE;
            }
        }

        if matched < least {
            continue;
        }

        if terms.len() > 1 && content.contains(&phrase) {
            score += 25;
        }

        let volume24h = sum_or_null(&event.markets, |m| m.volume24h);
        if volume24h.is_some_and(|v| v > 0.0) {
            score += 5;
        }

        scored.push(Scored {
            event: index,
            matched,
            score,
            volume24h,
        });
    }

    // Completeness first: no partial may outrank a full match, whatever it
    // scored. Then relevance, then liquidity. Stable, so an all-round tie keeps
    // the order the catalogue was crawled in.
    scored.sort_by(|a, b| {
        b.matched
            .cmp(&a.matched)
            .then_with(|| b.score.cmp(&a.score))
            .then_with(|| by_desc(a.volume24h, b.volume24h))
    });
    scored.truncate(limit);

    let hits = scored
        .into_iter()
        .map(|hit| {
            let event = &snapshot.events[hit.event];
            let mut markets = event.markets.clone();
            markets.sort_by(|a, b| by_desc(a.volume24h, b.volume24h));

            EventSearchHit {
                event: EventSummary::from(event),
                markets,
                volume24h: hit.volume24h,
                score: hit.score,
                matched_terms: hit.matched,
                total_terms: terms.len() as u32,
            }
        })
        .collect();

    SearchResponse {
        query: query.to_string(),
        hits,
        scanned: snapshot.events.len() as u32,
        snapshot_age_seconds: snapshot.age_seconds(),
        truncated: snapshot.truncated,
    }
}

/// The figure each board ranks on. A venue that does not publish it reports
/// `None`, which is what keeps it off that board rather than at the bottom.
fn field(sort: MoverSort) -> fn(&Market) -> Option<f64> {
    match sort {
        MoverSort::Volume => |m: &Market| m.volume24h,
        MoverSort::OpenInterest => |m: &Market| m.open_interest,
        MoverSort::Liquidity => |m: &Market| m.liquidity,
        MoverSort::Gainers | MoverSort::Losers => |m: &Market| m.change,
    }
}

/// Leaderboard over a snapshot. Powers the `TOP` command.
///
/// A market whose venue does not publish the sorted figure is left out of that
/// board entirely. Ranking it as a zero would seat every Polymarket US contract
/// at the bottom of `TOP volume` and imply nothing trades there, which is a
/// statement about the venue's API, not its book.
pub fn rank_markets(markets: &[Market], sort: MoverSort, limit: usize) -> Vec<Market> {
    let field = field(sort);
    let movers = matches!(sort, MoverSort::Gainers | MoverSort::Losers);

    let mut eligible: Vec<Market> = markets
        .iter()
        .filter(|m| {
            if field(m).is_none() {
                return false;
            }
            // A mover with no turnover behind it is a stale print, not a move.
            if movers {
                return m.volume24h.unwrap_or(0.0) > 0.0;
            }
            true
        })
        .cloned()
        .collect();

    // Biggest first on every board but `losers`, where the board *is* the
    // bottom of the range and the most negative move leads it.
    eligible.sort_by(|a, b| {
        let ordering = by_desc(field(a), field(b));
        if sort == MoverSort::Losers {
            ordering.reverse()
        } else {
            ordering
        }
    });
    eligible.truncate(limit);
    eligible
}

#[cfg(test)]
mod tests {
    use super::*;

    fn market(ticker: &str) -> Market {
        Market {
            venue: Venue::Kalshi,
            ticker: ticker.to_string(),
            event_ticker: "E".into(),
            series_ticker: "S".into(),
            title: "t".into(),
            yes_sub_title: String::new(),
            no_sub_title: String::new(),
            status: "open".into(),
            market_type: "binary".into(),
            yes_bid: None,
            yes_ask: None,
            no_bid: None,
            no_ask: None,
            mid: None,
            last_price: None,
            previous_price: None,
            change: None,
            volume: Some(0.0),
            volume24h: Some(0.0),
            open_interest: Some(0.0),
            liquidity: Some(0.0),
            open_time: String::new(),
            close_time: String::new(),
            expiration_time: String::new(),
            result: String::new(),
            rules_primary: String::new(),
            category: None,
            strike_type: None,
            floor_strike: None,
            cap_strike: None,
        }
    }

    /// An event with one market, whose strike label is searchable.
    fn event(ticker: &str, title: &str, markets: Vec<Market>) -> VenueEvent {
        VenueEvent {
            venue: Venue::Kalshi,
            event_ticker: ticker.to_string(),
            series_ticker: ticker.split('-').next().unwrap_or(ticker).to_string(),
            title: title.to_string(),
            sub_title: String::new(),
            category: String::new(),
            mutually_exclusive: true,
            markets,
        }
    }

    fn corpus(events: Vec<VenueEvent>) -> Corpus {
        let markets = events.iter().flat_map(|e| e.markets.clone()).collect();
        Corpus::new(Venue::Kalshi, events, markets, false)
    }

    fn scores(response: &SearchResponse) -> Vec<(&str, u32)> {
        response
            .hits
            .iter()
            .map(|h| (h.event.event_ticker.as_str(), h.score))
            .collect()
    }

    /* ------------------------------------------------------------ sumOrNull */

    #[test]
    fn sum_or_null_is_none_when_the_venue_publishes_nothing() {
        let markets = vec![
            Market {
                volume24h: None,
                ..market("A")
            },
            Market {
                volume24h: None,
                ..market("B")
            },
        ];
        assert_eq!(sum_or_null(&markets, |m| m.volume24h), None);
    }

    #[test]
    fn sum_or_null_keeps_a_published_zero() {
        // The distinction the helper exists for: a venue that answers "0" has
        // answered, and must not be filed with the ones that publish nothing.
        let markets = vec![Market {
            volume24h: Some(0.0),
            ..market("A")
        }];
        assert_eq!(sum_or_null(&markets, |m| m.volume24h), Some(0.0));
    }

    #[test]
    fn sum_or_null_adds_only_the_published_legs() {
        let markets = vec![
            Market {
                volume24h: Some(10.0),
                ..market("A")
            },
            Market {
                volume24h: None,
                ..market("B")
            },
            Market {
                volume24h: Some(2.5),
                ..market("C")
            },
        ];
        assert_eq!(sum_or_null(&markets, |m| m.volume24h), Some(12.5));
    }

    #[test]
    fn sum_or_null_of_an_empty_ladder_is_none() {
        assert_eq!(sum_or_null(&[], |m| m.volume24h), None);
    }

    /* --------------------------------------------------------- searchCorpus */

    #[test]
    fn a_full_match_outranks_every_partial_one() {
        let snapshot = corpus(vec![
            event("KXCPI-26", "CPI release", vec![]),
            event("KXFED-26", "Fed decision", vec![]),
        ]);

        let response = search_corpus(&snapshot, "fed decision", DEFAULT_LIMIT);
        assert_eq!(response.hits[0].event.event_ticker, "KXFED-26");
        assert_eq!(response.hits[0].matched_terms, 2);
        assert_eq!(response.hits[0].total_terms, 2);
    }

    #[test]
    fn a_stray_word_does_not_hide_what_the_rest_of_the_query_describes() {
        let snapshot = corpus(vec![
            event("KXFED-26", "Fed decision", vec![]),
            event("KXCPI-26", "CPI release", vec![]),
        ]);

        // One word matching nothing used to cost the whole result. It now costs
        // the event a rank and earns it a label, which is the honest answer:
        // this is the closest thing open, and here is how close.
        let response = search_corpus(&snapshot, "fed nonsense", DEFAULT_LIMIT);
        assert_eq!(scores(&response).len(), 1);
        assert_eq!(response.hits[0].event.event_ticker, "KXFED-26");
        assert_eq!(response.hits[0].matched_terms, 1);
        assert_eq!(response.hits[0].total_terms, 2);
    }

    #[test]
    fn too_little_of_the_query_is_not_a_hit() {
        let snapshot = corpus(vec![event("KXFED-26", "Fed decision", vec![])]);

        // One word in three is not most of a question.
        assert!(
            search_corpus(&snapshot, "fed absolute nonsense", DEFAULT_LIMIT)
                .hits
                .is_empty()
        );
    }

    #[test]
    fn a_plural_finds_the_singular() {
        // `rate` is inside "rates", but `rates` is not inside "rate", so the
        // raw substring test found none of the open "Fed funds rate" events.
        let snapshot = corpus(vec![event(
            "KXFED-26",
            "Fed funds rate at end of 2036",
            vec![],
        )]);

        let response = search_corpus(&snapshot, "fed rates", DEFAULT_LIMIT);
        assert_eq!(response.hits.len(), 1);
        assert_eq!(response.hits[0].matched_terms, 2);
    }

    #[test]
    fn a_stem_scores_below_the_word_as_typed() {
        let folded = event("KXA-26", "Zzz rate zzz", vec![]);
        let typed = event("KXB-26", "Zzz rates zzz", vec![]);
        let snapshot = corpus(vec![folded, typed]);

        // Only one direction needs the fold: "rate" is already inside "rates".
        // The event that spells it the way it was asked scores 10 + 6; the one
        // reached through the fold scores 8 + 6 — found, but not preferred.
        assert_eq!(
            scores(&search_corpus(&snapshot, "rates", 25)),
            [("KXB-26", 16), ("KXA-26", 14)]
        );
    }

    #[test]
    fn a_plural_that_would_stem_to_nonsense_is_left_alone() {
        // `rates` must not also look for `rat`, which is inside "ratio" and
        // "generate"; only the spellings English really takes `-es` in fold.
        assert_eq!(forms_of("rates"), ["rates", "rate"]);
        assert_eq!(forms_of("boxes"), ["boxes", "box"]);
        assert_eq!(forms_of("fed"), ["fed", "feds"]);
        assert_eq!(forms_of("us"), ["us"]);
    }

    #[test]
    fn a_short_term_must_be_a_whole_word() {
        let inside = event("KXA-26", "Klaus wins in August", vec![]);
        let whole = event("KXB-26", "US election", vec![]);
        let snapshot = corpus(vec![inside, whole]);

        // `us` as a bare substring is in "Klaus" and "August", which is how
        // `SRCH us election` answered with the next Pope.
        let response = search_corpus(&snapshot, "us", 25);
        assert_eq!(response.hits.len(), 1);
        assert_eq!(response.hits[0].event.event_ticker, "KXB-26");
    }

    #[test]
    fn grammar_words_do_not_have_to_match() {
        let snapshot = corpus(vec![event(
            "KXFED-26",
            "How many Fed rate cuts in 2026?",
            vec![],
        )]);

        // Every one of these returned nothing: `will` and `the` were required
        // terms, and `cuts` could not reach "cut".
        for query in [
            "will the fed cut rates",
            "what will the fed do",
            "fed rate cuts",
        ] {
            assert_eq!(
                search_corpus(&snapshot, query, 25).hits.len(),
                1,
                "{query} found nothing"
            );
        }
    }

    #[test]
    fn a_query_of_nothing_but_grammar_still_searches_for_it() {
        let snapshot = corpus(vec![
            event("KXA-26", "The fed cut", vec![]),
            event("KXB-26", "Fed cut", vec![]),
        ]);

        // Dropping every term would answer `SRCH the` with the whole catalogue.
        let response = search_corpus(&snapshot, "the", 25);
        assert_eq!(response.hits.len(), 1);
        assert_eq!(response.hits[0].event.event_ticker, "KXA-26");
    }

    #[test]
    fn a_category_hit_ranks_below_a_title_that_asks_the_question() {
        let mut conclave = event("KXPOPE-26", "Who will the next Pope be?", vec![]);
        conclave.category = "Elections".into();
        let ballot = event("KXPRES-28", "Presidential Election Winner 2028", vec![]);
        let snapshot = corpus(vec![conclave, ballot]);

        // Both are findable — a conclave really is an election — but the
        // category is one of a dozen buckets the whole catalogue shares, and
        // scoring it like a title put the Pope above the ballot.
        let response = search_corpus(&snapshot, "election", 25);
        assert_eq!(
            scores(&response),
            [("KXPRES-28", 16), ("KXPOPE-26", CATEGORY_SCORE)]
        );
    }

    #[test]
    fn a_hit_reports_how_much_of_the_query_it_answered() {
        let snapshot = corpus(vec![event("KXFED-26", "Fed decision", vec![])]);

        // Stop-words are in neither count: "1 of 1" is the honest reading of
        // `SRCH will the fed`, because `fed` is the only word that narrowed it.
        let response = search_corpus(&snapshot, "will the fed", 25);
        assert_eq!(response.hits[0].matched_terms, 1);
        assert_eq!(response.hits[0].total_terms, 1);
    }

    #[test]
    fn a_bare_term_scores_ten() {
        // Buried mid-word in the title: no ticker hit, no prefix, no boundary.
        let snapshot = corpus(vec![event("KXABC-26", "Unfeddled outcome", vec![])]);
        assert_eq!(
            scores(&search_corpus(&snapshot, "fed", 25)),
            [("KXABC-26", 10)]
        );
    }

    #[test]
    fn a_ticker_hit_scores_twelve_more() {
        // `kxfeddecision` gives no boundary and the title does not start with
        // the term, so the ticker bonus is the only thing above the base ten.
        let snapshot = corpus(vec![event(
            "KXFEDDECISION-26",
            "Interest rate call",
            vec![],
        )]);
        assert_eq!(
            scores(&search_corpus(&snapshot, "fed", 25)),
            [("KXFEDDECISION-26", 22)]
        );
    }

    #[test]
    fn a_title_prefix_scores_eight_more() {
        let snapshot = corpus(vec![event("KXABC-26", "Fed funds", vec![])]);
        // 10 base + 8 prefix + 6 boundary (" fed funds").
        assert_eq!(
            scores(&search_corpus(&snapshot, "fed", 25)),
            [("KXABC-26", 24)]
        );
    }

    #[test]
    fn a_word_boundary_scores_six_more() {
        let snapshot = corpus(vec![event("KXABC-26", "The fed cut", vec![])]);
        // 10 base + 6 boundary; the title starts with "the", not the term.
        assert_eq!(
            scores(&search_corpus(&snapshot, "fed", 25)),
            [("KXABC-26", 16)]
        );
    }

    #[test]
    fn the_whole_phrase_outranks_scattered_terms() {
        let together = event("KXA-26", "Zzz fed cut zzz", vec![]);
        let apart = event("KXB-26", "Zzz cut zzz and fed zzz", vec![]);
        let snapshot = corpus(vec![apart, together]);

        let response = search_corpus(&snapshot, "fed cut", 25);
        assert_eq!(response.hits[0].event.event_ticker, "KXA-26");
        // Both terms score 10 + 6; only the contiguous one takes the phrase 25.
        assert_eq!(scores(&response), [("KXA-26", 57), ("KXB-26", 32)]);
    }

    #[test]
    fn a_single_term_never_takes_the_phrase_bonus() {
        let snapshot = corpus(vec![event("KXABC-26", "The fed cut", vec![])]);
        assert_eq!(
            scores(&search_corpus(&snapshot, "fed", 25)),
            [("KXABC-26", 16)]
        );
    }

    #[test]
    fn traded_events_score_five_more() {
        let dead = event(
            "KXA-26",
            "The fed cut",
            vec![Market {
                volume24h: Some(0.0),
                ..market("A")
            }],
        );
        let live = event(
            "KXB-26",
            "The fed cut",
            vec![Market {
                volume24h: Some(1.0),
                ..market("B")
            }],
        );
        let unpublished = event(
            "KXC-26",
            "The fed cut",
            vec![Market {
                volume24h: None,
                ..market("C")
            }],
        );
        let snapshot = corpus(vec![dead, live, unpublished]);

        let response = search_corpus(&snapshot, "fed", 25);
        let by_ticker: std::collections::HashMap<_, _> = scores(&response).into_iter().collect();
        assert_eq!(by_ticker["KXB-26"], 21);
        // A published zero and an unpublished figure both fail `> 0`.
        assert_eq!(by_ticker["KXA-26"], 16);
        assert_eq!(by_ticker["KXC-26"], 16);
        assert_eq!(response.hits[0].event.event_ticker, "KXB-26");
    }

    #[test]
    fn a_regex_metacharacter_in_the_query_is_taken_literally() {
        // `(2026)` is an unterminated group as a raw pattern. Escaped, it still
        // earns the boundary bonus after the `e` of `dune`; unescaped, building
        // the pattern fails and the search silently loses the six points — or,
        // in the version of this that throws, the whole search.
        let snapshot = corpus(vec![event("KXRT-DUNE", "Dune(2026) score", vec![])]);
        let response = search_corpus(&snapshot, "(2026)", 25);
        assert_eq!(scores(&response), [("KXRT-DUNE", 16)]);
    }

    #[test]
    fn a_metacharacter_in_a_title_does_not_break_the_scan() {
        let snapshot = corpus(vec![
            event("KXA-26", "Fed cut (September) [rate]", vec![]),
            event("KXB-26", "Fed *", vec![]),
        ]);
        assert_eq!(search_corpus(&snapshot, "fed", 25).hits.len(), 2);
    }

    #[test]
    fn ties_break_on_twenty_four_hour_volume() {
        let quiet = event(
            "KXA-26",
            "The fed cut",
            vec![Market {
                volume24h: Some(5.0),
                ..market("A")
            }],
        );
        let busy = event(
            "KXB-26",
            "The fed cut",
            vec![Market {
                volume24h: Some(500.0),
                ..market("B")
            }],
        );
        let snapshot = corpus(vec![quiet, busy]);

        let response = search_corpus(&snapshot, "fed", 25);
        assert_eq!(response.hits[0].score, response.hits[1].score);
        assert_eq!(response.hits[0].event.event_ticker, "KXB-26");
    }

    #[test]
    fn an_unpublished_volume_does_not_win_a_tie() {
        let unpublished = event(
            "KXA-26",
            "The fed cut",
            vec![Market {
                volume24h: None,
                ..market("A")
            }],
        );
        let zero = event(
            "KXB-26",
            "The fed cut",
            vec![Market {
                volume24h: Some(0.0),
                ..market("B")
            }],
        );
        let snapshot = corpus(vec![unpublished, zero]);

        let response = search_corpus(&snapshot, "fed", 25);
        // Equal for the comparison, so the scan order stands — and the null is
        // still reported as one.
        assert_eq!(response.hits[0].volume24h, None);
        assert_eq!(response.hits[1].volume24h, Some(0.0));
    }

    #[test]
    fn strike_labels_sub_titles_and_categories_are_searched() {
        let mut ladder = event(
            "KXA-26",
            "Nothing here",
            vec![Market {
                yes_sub_title: "82.5° or above".into(),
                ..market("A")
            }],
        );
        ladder.sub_title = "New York".into();
        ladder.category = "Climate".into();
        let snapshot = corpus(vec![ladder]);

        assert_eq!(search_corpus(&snapshot, "82.5", 25).hits.len(), 1);
        assert_eq!(search_corpus(&snapshot, "york", 25).hits.len(), 1);
        assert_eq!(search_corpus(&snapshot, "climate", 25).hits.len(), 1);
    }

    #[test]
    fn the_query_is_case_insensitive() {
        let snapshot = corpus(vec![event("KXFED-26", "Fed Decision", vec![])]);
        assert_eq!(
            scores(&search_corpus(&snapshot, "FED", 25)),
            scores(&search_corpus(&snapshot, "fed", 25))
        );
        assert_eq!(
            search_corpus(&snapshot, "  FeD   deCISION ", 25).hits.len(),
            1
        );
    }

    #[test]
    fn an_empty_query_matches_everything() {
        // No terms means no failed term: the corpus comes back unranked, which
        // is what `SRCH` with no words shows.
        let snapshot = corpus(vec![
            event("KXA-26", "One", vec![]),
            event("KXB-26", "Two", vec![]),
        ]);
        assert_eq!(search_corpus(&snapshot, "", 25).hits.len(), 2);
    }

    #[test]
    fn markets_inside_a_hit_are_most_liquid_first() {
        let snapshot = corpus(vec![event(
            "KXA-26",
            "The fed cut",
            vec![
                Market {
                    volume24h: Some(10.0),
                    ..market("A")
                },
                Market {
                    volume24h: None,
                    ..market("B")
                },
                Market {
                    volume24h: Some(90.0),
                    ..market("C")
                },
            ],
        )]);

        let response = search_corpus(&snapshot, "fed", 25);
        let order: Vec<&str> = response.hits[0]
            .markets
            .iter()
            .map(|m| m.ticker.as_str())
            .collect();
        assert_eq!(order, ["C", "A", "B"]);
        assert_eq!(response.hits[0].volume24h, Some(100.0));
    }

    #[test]
    fn hits_are_capped_at_the_limit_but_scanned_counts_the_corpus() {
        let events: Vec<VenueEvent> = (0..10)
            .map(|i| event(&format!("KXA{i}-26"), "The fed cut", vec![]))
            .collect();
        let snapshot = corpus(events);

        let response = search_corpus(&snapshot, "fed", 3);
        assert_eq!(response.hits.len(), 3);
        assert_eq!(response.scanned, 10);
    }

    #[test]
    fn the_response_reports_the_snapshot_it_searched() {
        let mut snapshot = corpus(vec![event("KXA-26", "The fed cut", vec![])]);
        snapshot.truncated = true;
        snapshot.built_at = Instant::now() - std::time::Duration::from_secs(42);

        let response = search_corpus(&snapshot, "Fed", 25);
        assert_eq!(response.query, "Fed");
        assert!(response.truncated);
        assert_eq!(response.snapshot_age_seconds, 42.0);
    }

    /* ---------------------------------------------------------- rankMarkets */

    #[test]
    fn an_unpublished_figure_is_dropped_a_zero_is_ranked() {
        // The rule the whole board hangs on: `None` means "this venue does not
        // publish it" and must not be sorted as if it were a zero, while a
        // genuine zero is a fact the venue stated and belongs on the board.
        let markets = vec![
            Market {
                volume24h: None,
                ..market("UNPUBLISHED")
            },
            Market {
                volume24h: Some(0.0),
                ..market("ZERO")
            },
            Market {
                volume24h: Some(5.0),
                ..market("BUSY")
            },
        ];

        let ranked = rank_markets(&markets, MoverSort::Volume, 25);
        let tickers: Vec<&str> = ranked.iter().map(|m| m.ticker.as_str()).collect();
        assert_eq!(tickers, ["BUSY", "ZERO"]);
    }

    #[test]
    fn each_board_reads_its_own_field() {
        let markets = vec![
            Market {
                open_interest: None,
                liquidity: Some(1.0),
                ..market("NO_OI")
            },
            Market {
                open_interest: Some(1.0),
                liquidity: None,
                ..market("NO_LIQUIDITY")
            },
        ];

        assert_eq!(
            rank_markets(&markets, MoverSort::OpenInterest, 25)
                .iter()
                .map(|m| m.ticker.as_str())
                .collect::<Vec<_>>(),
            ["NO_LIQUIDITY"]
        );
        assert_eq!(
            rank_markets(&markets, MoverSort::Liquidity, 25)
                .iter()
                .map(|m| m.ticker.as_str())
                .collect::<Vec<_>>(),
            ["NO_OI"]
        );
    }

    #[test]
    fn the_boards_are_ordered_biggest_first() {
        let markets = vec![
            Market {
                volume24h: Some(5.0),
                ..market("SMALL")
            },
            Market {
                volume24h: Some(500.0),
                ..market("BIG")
            },
            Market {
                volume24h: Some(50.0),
                ..market("MID")
            },
        ];

        let tickers: Vec<String> = rank_markets(&markets, MoverSort::Volume, 25)
            .into_iter()
            .map(|m| m.ticker)
            .collect();
        assert_eq!(tickers, ["BIG", "MID", "SMALL"]);
    }

    #[test]
    fn gainers_lead_with_the_biggest_rise_losers_with_the_biggest_fall() {
        let markets = vec![
            Market {
                change: Some(0.02),
                volume24h: Some(10.0),
                ..market("UP_SMALL")
            },
            Market {
                change: Some(0.09),
                volume24h: Some(10.0),
                ..market("UP_BIG")
            },
            Market {
                change: Some(-0.07),
                volume24h: Some(10.0),
                ..market("DOWN_BIG")
            },
        ];

        let gainers: Vec<String> = rank_markets(&markets, MoverSort::Gainers, 25)
            .into_iter()
            .map(|m| m.ticker)
            .collect();
        assert_eq!(gainers, ["UP_BIG", "UP_SMALL", "DOWN_BIG"]);

        let losers: Vec<String> = rank_markets(&markets, MoverSort::Losers, 25)
            .into_iter()
            .map(|m| m.ticker)
            .collect();
        assert_eq!(losers, ["DOWN_BIG", "UP_SMALL", "UP_BIG"]);
    }

    #[test]
    fn a_mover_with_no_turnover_is_not_a_mover() {
        let markets = vec![
            Market {
                change: Some(0.09),
                volume24h: Some(0.0),
                ..market("STALE_PRINT")
            },
            Market {
                change: Some(0.05),
                volume24h: None,
                ..market("NO_VOLUME_PUBLISHED")
            },
            Market {
                change: None,
                volume24h: Some(10.0),
                ..market("NO_CHANGE_PUBLISHED")
            },
            Market {
                change: Some(0.01),
                volume24h: Some(10.0),
                ..market("REAL")
            },
        ];

        for sort in [MoverSort::Gainers, MoverSort::Losers] {
            let tickers: Vec<String> = rank_markets(&markets, sort, 25)
                .into_iter()
                .map(|m| m.ticker)
                .collect();
            assert_eq!(tickers, ["REAL"], "{sort}");
        }

        // The same turnover rule does not apply to the volume board itself.
        assert_eq!(rank_markets(&markets, MoverSort::Volume, 25).len(), 3);
    }

    #[test]
    fn the_board_is_capped_at_the_limit() {
        let markets: Vec<Market> = (0..10)
            .map(|i| Market {
                volume24h: Some(f64::from(i)),
                ..market(&format!("M{i}"))
            })
            .collect();

        let ranked = rank_markets(&markets, MoverSort::Volume, 3);
        assert_eq!(ranked.len(), 3);
        assert_eq!(ranked[0].ticker, "M9");
    }

    #[test]
    fn ranking_an_empty_book_is_empty() {
        assert!(rank_markets(&[], MoverSort::Volume, 25).is_empty());
    }
}
