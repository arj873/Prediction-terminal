//! Lining one broker's series up against the others'.
//!
//! The terminal quotes six exchanges that list many of the same questions and
//! agree on no identifier for any of them. This module builds, per venue, an
//! index of open series, then pairs those indexes up — by a curated table where
//! one exists and by [`score_series`] everywhere else — so `Fed decision in
//! Oct 2026?` can be read at every price at once.
//!
//! Two rules keep it honest:
//!
//!   A curated link is checked, not asserted. Every entry names live series
//!   identifiers, and a leg that is not in today's catalogue is dropped rather
//!   than shown — a table that goes stale degrades to a text match instead of
//!   quoting a market that no longer exists.
//!
//!   Every match carries its confidence and its reason. `linked` means the pair
//!   is stated in the table; anything else is the matcher's reading of two
//!   titles, labelled as such, with the terms it matched on.

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

use futures::future::join_all;
use time::format_description::well_known::Rfc3339;
use time::format_description::BorrowedFormatItem;
use time::{Date, OffsetDateTime};

use terminal_core::matching::{
    could_reach_floor, pair_labels, prepare, score_event, score_series_prepared, tokenise,
    MatchScore, Prepared, SeriesDescriptor, MATCH_FLOOR,
};
use terminal_core::types::{
    CompareEventLeg, CompareLeg, CompareResponse, CompareRow, CompareUnmatched, LinkedSeries,
    LinkedSeriesResponse, Market, MatchConfidence, SeriesLeg, Venue, VenueEvent, VenueUnavailable,
};
use terminal_core::util::{round4, round_to};
use terminal_core::venue::{venue_ids, venue_info};

use crate::app::AppState;
use crate::cache::ttl;
use crate::error::{Result, UpstreamError};
use crate::sources::corpus::{sum_or_null, Corpus};
use crate::sources::{forecastex, gemini, kalshi, polymarket, polymarketus, predictfun};

/// Where the whole board lives for a catalogue TTL.
const INDEX_KEY: &str = "xvenue:indexes";

/* ------------------------------------------------------------------ index */

/// An insertion-ordered set of tokens, which is what a JavaScript `Set` is.
///
/// The order is load-bearing here for the same reason it is in the matcher: it
/// decides the order candidates are gathered in, and a hash set would hand a
/// different order — and so a different winner among equal scores — on each run.
#[derive(Debug, Default, Clone)]
struct TokenSet {
    order: Vec<String>,
    index: HashSet<String>,
}

impl TokenSet {
    fn insert(&mut self, token: String) {
        if self.index.insert(token.clone()) {
            self.order.push(token);
        }
    }

    fn iter(&self) -> std::slice::Iter<'_, String> {
        self.order.iter()
    }
}

impl FromIterator<String> for TokenSet {
    fn from_iter<I: IntoIterator<Item = String>>(tokens: I) -> Self {
        let mut set = TokenSet::default();
        for token in tokens {
            set.insert(token);
        }
        set
    }
}

/// One venue's listing of one series, ready to be matched.
struct IndexedSeries {
    venue: Venue,
    series_ticker: String,
    title: String,
    /// The series' open events, busiest first.
    events: Vec<VenueEvent>,
    markets: u32,
    volume24h: Option<f64>,
    close_time: String,
    sample_event: String,
    /// Every token the title and its context — the exemplar's sub-title and
    /// category — yield, for the blocking index.
    tokens: TokenSet,
    /// This series' half of a comparison, derived once.
    ///
    /// The matcher's per-pair cost is almost entirely one-sided work — content
    /// tokens, the numbers in the title, the flattened identifier — and an
    /// anchor is offered to hundreds of candidates. Deriving it here, once per
    /// series per catalogue refresh, is the difference between a board that
    /// answers and one that does not.
    prepared: Prepared,
}

/// One venue's series, with the two lookups the pairing pass needs.
struct VenueIndex {
    venue: Venue,
    series: Vec<IndexedSeries>,
    /// Lower-cased series identifier → position in `series`.
    by_ticker: HashMap<String, usize>,
    /// Token → series carrying it, for blocking.
    by_token: HashMap<String, Vec<usize>>,
    /// Carried across from the snapshot exactly as the TypeScript carried them.
    /// Nothing below reads either yet; dropping them would quietly lose the fact
    /// that a venue's catalogue was crawled partially.
    #[allow(dead_code)]
    built_at: Instant,
    #[allow(dead_code)]
    truncated: bool,
}

/// Where one series sits: which venue index, and which of its series.
///
/// The TypeScript keyed its sets and maps on the `IndexedSeries` object itself.
/// A position is the Rust reading of that identity, and it is also what keeps
/// the leftovers reportable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct SeriesRef {
    venue: usize,
    series: usize,
}

/// A token in more than this share of a venue's series tells us nothing and
/// would drag every candidate into every comparison. `sports` is one of these.
///
/// The ceiling it implies never falls below four, or a venue listing a handful
/// of series would block every token it has and become uncomparable.
const COMMON_TOKEN_SHARE: f64 = 0.05;

fn build_index(venue: Venue, events: &[VenueEvent], truncated: bool) -> VenueIndex {
    let mut order: Vec<String> = Vec::new();
    let mut grouped: HashMap<String, Vec<&VenueEvent>> = HashMap::new();
    for event in events {
        let key = if event.series_ticker.is_empty() {
            &event.event_ticker
        } else {
            &event.series_ticker
        };
        if key.is_empty() {
            continue;
        }
        match grouped.get_mut(key) {
            Some(bucket) => bucket.push(event),
            None => {
                order.push(key.clone());
                grouped.insert(key.clone(), vec![event]);
            }
        }
    }

    let mut series: Vec<IndexedSeries> = Vec::new();
    for series_ticker in order {
        let group = &grouped[&series_ticker];

        // The busiest event stands for the series: it is the one with a live
        // book, and its title is the one worth matching on.
        let mut sorted: Vec<&VenueEvent> = group.clone();
        sorted.sort_by(|a, b| {
            by_desc(
                sum_or_null(&a.markets, |m| m.volume24h).unwrap_or(0.0),
                sum_or_null(&b.markets, |m| m.volume24h).unwrap_or(0.0),
            )
        });
        let lead = sorted[0];

        let markets: u32 = group
            .iter()
            .map(|e| u32::try_from(e.markets.len()).unwrap_or(u32::MAX))
            .sum();

        // `sumOrNull` over the group's ladders flattened: a venue that publishes
        // no volume anywhere stays `None` rather than summing to a confident 0.
        let mut volume = 0.0;
        let mut published = false;
        for event in group {
            if let Some(figure) = sum_or_null(&event.markets, |m| m.volume24h) {
                volume += figure;
                published = true;
            }
        }

        let mut closes: Vec<&str> = group
            .iter()
            .flat_map(|e| e.markets.iter())
            .map(|m| m.close_time.as_str())
            .filter(|close| !close.is_empty())
            .collect();
        closes.sort_unstable();

        let context = format!("{} {}", lead.sub_title, lead.category);
        let tokens: TokenSet = tokenise(&format!("{} {context}", lead.title))
            .into_iter()
            .collect();
        let prepared =
            prepare(&SeriesDescriptor::new(&series_ticker, &lead.title).with_context(&context));

        series.push(IndexedSeries {
            venue,
            series_ticker: series_ticker.clone(),
            title: lead.title.clone(),
            events: sorted.into_iter().cloned().collect(),
            markets,
            volume24h: published.then_some(volume),
            close_time: closes.first().map(|s| (*s).to_string()).unwrap_or_default(),
            sample_event: lead.event_ticker.clone(),
            tokens,
            prepared,
        });
    }

    let by_ticker: HashMap<String, usize> = series
        .iter()
        .enumerate()
        .map(|(i, s)| (s.series_ticker.to_lowercase(), i))
        .collect();

    let mut counts: HashMap<&str, usize> = HashMap::new();
    for s in &series {
        for token in s.tokens.iter() {
            *counts.entry(token.as_str()).or_insert(0) += 1;
        }
    }

    let ceiling = ((series.len() as f64 * COMMON_TOKEN_SHARE).floor() as usize).max(4);
    let mut by_token: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, s) in series.iter().enumerate() {
        for token in s.tokens.iter() {
            if counts.get(token.as_str()).copied().unwrap_or(0) > ceiling {
                continue;
            }
            by_token.entry(token.clone()).or_default().push(i);
        }
    }

    VenueIndex {
        venue,
        series,
        by_ticker,
        by_token,
        built_at: Instant::now(),
        truncated,
    }
}

/// Every venue's index, with the ones that failed named rather than hidden.
struct Indexes {
    ok: Vec<VenueIndex>,
    unavailable: Vec<VenueUnavailable>,
    built_at: Instant,
    /// The finished board: every series more than one broker lists, sorted, and
    /// not yet filtered by anyone's query.
    ///
    /// It lives here rather than being computed per request because it is a
    /// function of these indexes and nothing else — the query and the row limit
    /// only ever narrow it. Pairing thousands of series against thousands more
    /// is not work to repeat for a panel that polls, and doing it here means it
    /// happens once per catalogue refresh, inside the same single-flight, and
    /// can never be out of step with the index it was derived from.
    links: Vec<LinkedSeries>,
    /// How many series each venue offered, for the panel's footer.
    scanned: BTreeMap<String, u32>,
}

/// One venue's cached catalogue snapshot.
///
/// Dispatched against the venue modules directly: the board reads exactly these
/// snapshots and nothing else about a venue is needed here.
async fn corpus_of(state: &AppState, venue: Venue) -> Result<Arc<Corpus>> {
    match venue {
        Venue::Kalshi => kalshi::corpus_snapshot(state).await,
        Venue::Polymarket => polymarket::corpus_snapshot(state).await,
        Venue::PolymarketUs => polymarketus::corpus_snapshot(state).await,
        Venue::Gemini => gemini::corpus_snapshot(state).await,
        Venue::PredictFun => predictfun::corpus_snapshot(state).await,
        Venue::ForecastEx => forecastex::corpus_snapshot(state).await,
    }
}

/// One event, quoted live rather than read out of the snapshot.
async fn event_of(state: &AppState, venue: Venue, event_ticker: &str) -> Result<VenueEvent> {
    match venue {
        Venue::Kalshi => kalshi::get_event(state, event_ticker).await,
        Venue::Polymarket => polymarket::get_event(state, event_ticker).await,
        Venue::PolymarketUs => polymarketus::get_event(state, event_ticker).await,
        Venue::Gemini => gemini::get_event(state, event_ticker).await,
        Venue::PredictFun => predictfun::get_event(state, event_ticker).await,
        Venue::ForecastEx => forecastex::get_event(state, event_ticker).await,
    }
}

/// Sort the settled crawls into the board and the apologies.
///
/// A dead venue is named, never allowed to fail the whole board: a view
/// covering four of six brokers has to say which two are missing, or "no match"
/// reads as "no such market".
fn collect_indexes(venues: &[Venue], settled: Vec<Result<VenueIndex>>) -> Indexes {
    let mut ok: Vec<VenueIndex> = Vec::new();
    let mut unavailable: Vec<VenueUnavailable> = Vec::new();

    for (venue, result) in venues.iter().zip(settled) {
        match result {
            Ok(index) => ok.push(index),
            Err(err) => unavailable.push(VenueUnavailable {
                venue: *venue,
                error: err.message,
            }),
        }
    }

    Indexes {
        ok,
        unavailable,
        built_at: Instant::now(),
        // Filled by `link_all`, which is CPU-bound and belongs off the reactor.
        links: Vec::new(),
        scanned: BTreeMap::new(),
    }
}

async fn build_indexes(state: &AppState) -> Indexes {
    let venues = venue_ids();
    let settled = join_all(venues.iter().map(|venue| async move {
        let snapshot = corpus_of(state, *venue).await?;
        Ok(build_index(*venue, &snapshot.events, snapshot.truncated))
    }))
    .await;

    collect_indexes(&venues, settled)
}

/// Crawl every catalogue and pair them up, ready to be served.
///
/// The pairing runs on a blocking thread. It is the one piece of arithmetic in
/// this server large enough to matter — thousands of series against thousands
/// more — and it contains no `await`, so a request that started it could not be
/// cancelled and a runtime worker running it could not be yielded. Left on the
/// reactor it would not merely be slow: with four workers and a panel that
/// polls, requests would pile up faster than they retired until nothing on the
/// server answered at all, including the health check. Here it can take as long
/// as it takes without holding anything else up.
async fn build_board(state: &AppState) -> Result<Indexes> {
    let built = build_indexes(state).await;

    // A join failure means the pairing panicked, which is a bug. Refusing is the
    // only honest answer and — because the cache stores results and not
    // rejections — it is also the only one that does not pin an empty board in
    // front of every reader for a fifteen-minute catalogue TTL.
    tokio::task::spawn_blocking(move || paired(built))
        .await
        .map_err(|err| {
            tracing::error!(error = %err, "[xvenue] pairing pass failed");
            UpstreamError::new(
                "The cross-venue board could not be built.",
                crate::error::codes::INTERNAL,
            )
        })
}

/// Run the pairing pass over a set of indexes and hand back the finished board.
fn paired(built: Indexes) -> Indexes {
    let (links, scanned) = link_all(&built);
    Indexes {
        links,
        scanned,
        ..built
    }
}

async fn indexes(state: &AppState) -> Result<Arc<Indexes>> {
    state
        .cache()
        .cached(INDEX_KEY, ttl::CATALOGUE, || build_board(state))
        .await
}

/* --------------------------------------------------------- curated links */

/// Series the terminal states are the same question, rather than inferring it.
///
/// Each entry was read off the live catalogues, and each is re-checked against
/// them on every request — a leg whose identifier no longer exists is dropped, so
/// this table can go stale without ever producing a wrong quote. That is what
/// makes it safe to name a leg at a venue whose book rotates: ForecastEx retires
/// a product without warning, and a retired `FFDEC` simply stops appearing.
///
/// It exists for the cases the matcher cannot reach on wording alone: nothing in
/// "Fed decision in Oct 2026?" and Polymarket's series slug `fomc` shares a
/// single token, and a temperature series named `KXHIGHNY` against
/// `weather-daily-high-nyc` needs to know that NY and NYC are one city before
/// either title helps. Everything the matcher *can* reach is left to it, so this
/// stays short enough to keep true.
struct CuratedLink {
    key: &'static str,
    title: &'static str,
    legs: &'static [(Venue, &'static str)],
}

impl CuratedLink {
    fn leg(&self, venue: Venue) -> Option<&'static str> {
        self.legs
            .iter()
            .find(|(id, _)| *id == venue)
            .map(|(_, ticker)| *ticker)
    }
}

const CURATED_LINKS: &[CuratedLink] = &[
    CuratedLink {
        key: "fed-decision",
        title: "Federal Reserve rate decision",
        legs: &[
            (Venue::Kalshi, "KXFEDDECISION"),
            (Venue::Polymarket, "fomc"),
            (Venue::PolymarketUs, "usfed-fomc"),
            (Venue::Gemini, "FED"),
            (Venue::PredictFun, "fed-decision-in"),
            (Venue::ForecastEx, "FFDEC"),
        ],
    },
    CuratedLink {
        key: "fed-rate-range",
        title: "Fed funds target range",
        legs: &[
            (Venue::Kalshi, "KXFED"),
            (Venue::Polymarket, "fed-interest-rates"),
            (Venue::ForecastEx, "FF"),
        ],
    },
    CuratedLink {
        key: "cpi-yoy",
        title: "US CPI, year over year",
        legs: &[
            (Venue::Kalshi, "KXCPIYOY"),
            (Venue::PolymarketUs, "cpi"),
            (Venue::ForecastEx, "CPIY"),
        ],
    },
    CuratedLink {
        key: "us-midterms-house",
        title: "US House control after the midterms",
        legs: &[
            (Venue::Kalshi, "KXHOUSE"),
            (Venue::PolymarketUs, "usho-midterms"),
            (Venue::Gemini, "CTRLUSHOU"),
            (Venue::ForecastEx, "HORC"),
        ],
    },
    CuratedLink {
        key: "us-midterms-senate",
        title: "US Senate control after the midterms",
        legs: &[
            (Venue::Kalshi, "KXSENATE"),
            (Venue::PolymarketUs, "usse-midterms"),
            (Venue::Gemini, "CTRLUSSEN"),
            (Venue::PredictFun, "which-party-will-win-the-senate-in"),
            (Venue::ForecastEx, "SENM"),
        ],
    },
    CuratedLink {
        key: "nyc-high-temperature",
        title: "Daily high temperature, New York City",
        legs: &[
            (Venue::Kalshi, "KXHIGHNY"),
            (Venue::PolymarketUs, "weather-daily-high-nyc"),
            // Gemini files every city under one `WXHIGH` product and ForecastEx
            // names the reporting station rather than the city — `UHLGA` is
            // LaGuardia — so neither identifier says "New York" anywhere. The
            // matcher cannot reach either from the wording.
            (Venue::Gemini, "WXHIGH-NYC"),
            (Venue::ForecastEx, "UHLGA"),
        ],
    },
    CuratedLink {
        key: "chicago-high-temperature",
        title: "Daily high temperature, Chicago",
        legs: &[
            (Venue::Kalshi, "KXHIGHCHI"),
            (Venue::PolymarketUs, "weather-daily-high-chicago"),
            (Venue::Gemini, "WXHIGH-CHI"),
            // Midway, for the same reason `UHLGA` stands in for New York.
            (Venue::ForecastEx, "UHMDW"),
        ],
    },
    CuratedLink {
        key: "best-picture",
        title: "Academy Award for Best Picture",
        legs: &[
            (Venue::Kalshi, "KXOSCARPIC"),
            (Venue::PolymarketUs, "oscars-2026"),
        ],
    },
    CuratedLink {
        key: "world-series",
        title: "World Series champion",
        legs: &[
            (Venue::Kalshi, "KXWORLDSERIES"),
            (Venue::PolymarketUs, "mlb-2026"),
        ],
    },
    CuratedLink {
        key: "super-bowl",
        title: "Super Bowl champion",
        legs: &[
            (Venue::Kalshi, "KXPROFOOTBALLCHAMP"),
            (Venue::PolymarketUs, "nfl-2026"),
            // predict.fun cannot say "Super Bowl" — the phrase is trademarked
            // and the exchange writes around it, listing the same field as "NFL
            // Champion 2027" — so no wording the matcher could reach connects
            // this listing to the other two.
            (Venue::PredictFun, "big-game-champion"),
        ],
    },
];

/* ---------------------------------------------------------------- linking */

fn leg_of(series: &IndexedSeries) -> SeriesLeg {
    SeriesLeg {
        venue: series.venue,
        series_ticker: series.series_ticker.clone(),
        title: series.title.clone(),
        events: u32::try_from(series.events.len()).unwrap_or(u32::MAX),
        markets: series.markets,
        volume24h: series.volume24h,
        close_time: series.close_time.clone(),
        sample_event: series.sample_event.clone(),
    }
}

/// Descending, and never `Ordering::Less` on a NaN that cannot arrive here.
fn by_desc(a: f64, b: f64) -> Ordering {
    b.partial_cmp(&a).unwrap_or(Ordering::Equal)
}

/// Busiest leg first, with venues that publish no volume ordered after.
fn by_activity(a: &SeriesLeg, b: &SeriesLeg) -> Ordering {
    by_desc(a.volume24h.unwrap_or(-1.0), b.volume24h.unwrap_or(-1.0))
        .then_with(|| b.markets.cmp(&a.markets))
}

/// Every candidate pairing for `anchor` at `other`, above the floor.
///
/// Candidates come from the token index rather than the whole catalogue — with
/// thousands of series per venue, scoring every pair would mean millions of
/// comparisons for one board. A series sharing no distinguishing token with the
/// anchor cannot clear the floor anyway.
fn candidates_for(anchor: &IndexedSeries, other: &VenueIndex) -> Vec<(usize, MatchScore)> {
    let mut seen: HashSet<usize> = HashSet::new();
    let mut candidates: Vec<usize> = Vec::new();
    for token in anchor.tokens.iter() {
        for candidate in other.by_token.get(token).into_iter().flatten() {
            if seen.insert(*candidate) {
                candidates.push(*candidate);
            }
        }
    }

    // Sharing one token is what the blocking index asks for, and it is a low
    // bar: a catalogue of thousands offers hundreds of candidates that share an
    // incidental word and nothing else. Turning those away on a token count,
    // before any phrase folding or number arithmetic, is what keeps the board
    // answering — and it turns exactly the same pairs away that the floor test
    // below would have, two orders of magnitude later.
    let mut scored: Vec<(usize, MatchScore)> = Vec::new();
    for candidate in candidates {
        let other_series = &other.series[candidate];
        if !could_reach_floor(&anchor.prepared, &other_series.prepared, MATCH_FLOOR) {
            continue;
        }
        let matched = score_series_prepared(&anchor.prepared, &other_series.prepared);
        if matched.score >= MATCH_FLOOR {
            scored.push((candidate, matched));
        }
    }
    scored
}

/// The single best counterpart for one series — used when comparing one event,
/// where there is no field of other anchors to weigh it against.
fn best_match(anchor: &IndexedSeries, other: &VenueIndex) -> Option<(usize, MatchScore)> {
    let mut best: Option<(usize, MatchScore)> = None;
    for candidate in candidates_for(anchor, other) {
        if best
            .as_ref()
            .is_none_or(|(_, current)| candidate.1.score > current.score)
        {
            best = Some(candidate);
        }
    }
    best
}

/// Assign one venue's series to another's, globally rather than one at a time.
///
/// Taking each anchor's own favourite in turn is not good enough where a family
/// of sibling series all resemble each other. The Emmys list twenty categories
/// that differ by three words — comedy or drama, lead or supporting, actor or
/// actress — and every one of them scores respectably against every other. Asked
/// in isolation, "Outstanding Lead Actor in a Comedy Series" will happily take
/// whichever sibling happens to score highest, leaving the sibling that was
/// actually its twin to be claimed by someone else.
///
/// So every candidate pairing is scored first, then assigned best-first with
/// both sides struck off as they are taken — the discipline [`pair_labels`]
/// already applies to the rungs of one ladder, applied here to the catalogue.
fn assign_pairs(
    left: &VenueIndex,
    anchors: &[usize],
    right: &VenueIndex,
    right_venue: usize,
    taken: &HashSet<SeriesRef>,
) -> Vec<(usize, usize, MatchScore)> {
    let mut all: Vec<(usize, usize, MatchScore)> = Vec::new();

    for anchor in anchors {
        for (candidate, matched) in candidates_for(&left.series[*anchor], right) {
            if taken.contains(&SeriesRef {
                venue: right_venue,
                series: candidate,
            }) {
                continue;
            }
            all.push((*anchor, candidate, matched));
        }
    }

    all.sort_by(|x, y| by_desc(x.2.score, y.2.score));

    let mut assigned: Vec<(usize, usize, MatchScore)> = Vec::new();
    let mut used_anchors: HashSet<usize> = HashSet::new();
    let mut used_candidates: HashSet<usize> = HashSet::new();

    for pair in all {
        if used_anchors.contains(&pair.0) || used_candidates.contains(&pair.1) {
            continue;
        }
        used_anchors.insert(pair.0);
        used_candidates.insert(pair.1);
        assigned.push(pair);
    }

    assigned
}

/// The weakest confidence among a set of matches — what the group is worth.
fn weakest(confidences: impl IntoIterator<Item = MatchConfidence>) -> MatchConfidence {
    confidences
        .into_iter()
        .fold(MatchConfidence::Linked, |worst, c| worst.max(c))
}

fn matches_query(series: &LinkedSeries, terms: &[String]) -> bool {
    if terms.is_empty() {
        return true;
    }
    let mut haystack = format!("{} {}", series.key, series.title);
    for leg in &series.legs {
        haystack.push(' ');
        haystack.push_str(&leg.series_ticker);
        haystack.push(' ');
        haystack.push_str(&leg.title);
    }
    let haystack = haystack.to_lowercase();
    terms.iter().all(|term| haystack.contains(term.as_str()))
}

/// A `Map` keyed by a group's first member, iterated in insertion order.
///
/// Reproducing the JavaScript `Map` here is not pedantry: a group is re-keyed by
/// delete-then-set as it grows, which moves it to the end of the iteration, and
/// the final sort is stable — so this order decides which of two equally scored
/// boards prints first.
#[derive(Default)]
struct GroupKeys {
    order: Vec<SeriesRef>,
    keyed: HashMap<SeriesRef, usize>,
}

impl GroupKeys {
    fn delete(&mut self, key: SeriesRef) {
        if self.keyed.remove(&key).is_some() {
            self.order.retain(|k| *k != key);
        }
    }

    fn set(&mut self, key: SeriesRef, group: usize) {
        if self.keyed.insert(key, group).is_none() {
            self.order.push(key);
        }
    }
}

/// One group of series the terminal believes are one question.
struct Group {
    members: Vec<SeriesRef>,
    evidence: Vec<MatchScore>,
}

/// Series that more than one broker lists.
///
/// The pairing itself happens with the index, in [`link_all`]; what is left for
/// a request is to narrow the finished board to what was asked for. So `?q=fed`
/// costs a scan of a few hundred rows rather than a re-run of the whole pass.
pub async fn linked_series(
    state: &AppState,
    query: &str,
    limit: usize,
) -> Result<LinkedSeriesResponse> {
    let built = indexes(state).await?;
    Ok(link_series(&built, query, limit))
}

fn link_series(built: &Indexes, query: &str, limit: usize) -> LinkedSeriesResponse {
    let lowered = query.to_lowercase();
    let terms: Vec<String> = lowered.split_whitespace().map(str::to_string).collect();

    // Filtering a sorted list keeps it sorted, so the row order a reader sees is
    // the board's order whether or not they typed a query.
    let series: Vec<LinkedSeries> = built
        .links
        .iter()
        .filter(|s| matches_query(s, &terms))
        .take(limit)
        .cloned()
        .collect();

    LinkedSeriesResponse {
        query: query.to_string(),
        series,
        scanned: built.scanned.clone(),
        unavailable: built.unavailable.clone(),
        // Read at answer time, not at build time: a panel needs to know how old
        // the universe it is looking at is, and that grows between requests.
        snapshot_age_seconds: round_to(built.built_at.elapsed().as_secs_f64(), 0),
    }
}

/// Pair every venue's series against every other's.
///
/// This is the board itself, and it depends on nothing but the indexes: no
/// query, no row limit, no clock. That is what lets it be built once with the
/// index and read by every request until the catalogue is crawled again.
///
/// Curated links are resolved first and claim their legs; everything left is
/// matched on wording, anchored at the venue with the most series so the pass
/// covers the widest catalogue.
fn link_all(built: &Indexes) -> (Vec<LinkedSeries>, BTreeMap<String, u32>) {
    let mut scanned: BTreeMap<String, u32> = BTreeMap::new();
    for index in &built.ok {
        scanned.insert(
            index.venue.as_str().to_string(),
            u32::try_from(index.series.len()).unwrap_or(u32::MAX),
        );
    }

    let mut claimed: HashSet<SeriesRef> = HashSet::new();
    let mut found: Vec<LinkedSeries> = Vec::new();

    // ---- curated -----------------------------------------------------------
    for link in CURATED_LINKS {
        let mut legs: Vec<SeriesLeg> = Vec::new();
        let mut refs: Vec<SeriesRef> = Vec::new();
        for (v, index) in built.ok.iter().enumerate() {
            let Some(wanted) = link.leg(index.venue) else {
                continue;
            };
            let Some(&s) = index.by_ticker.get(&wanted.to_lowercase()) else {
                continue;
            };
            legs.push(leg_of(&index.series[s]));
            refs.push(SeriesRef {
                venue: v,
                series: s,
            });
        }
        if legs.len() < 2 {
            // The link is dropped, so it claims nothing. Claiming a leg here
            // would take a live series out of the text-matching pass on behalf
            // of a pairing that no longer exists — a broker rotates a ticker,
            // the curated entry stops resolving, and the two sides that *are*
            // listed become unpairable rather than merely unlabelled. A stale
            // table entry should cost a stated link, not a found one.
            continue;
        }

        // Only a surviving link speaks for its legs, so the matcher cannot
        // pair one of them a second way.
        claimed.extend(refs);

        legs.sort_by(by_activity);
        found.push(LinkedSeries {
            key: link.key.to_string(),
            title: link.title.to_string(),
            legs,
            confidence: MatchConfidence::Linked,
            reason: "curated link, checked against both catalogues".to_string(),
            score: 1.0,
        });
    }

    // ---- matched -----------------------------------------------------------
    // Every venue against every other, not each against one anchor. Anchoring on
    // the largest catalogue made any question the anchor does not list invisible:
    // Polymarket International and Polymarket US both run all 435 House districts
    // under word-for-word identical titles, and the 46 of them Kalshi does not
    // list were unreachable — not mis-scored, never compared.
    let mut links: Vec<(SeriesRef, SeriesRef, MatchScore)> = Vec::new();

    for i in 0..built.ok.len() {
        for j in (i + 1)..built.ok.len() {
            let left = &built.ok[i];
            let right = &built.ok[j];
            let free: Vec<usize> = (0..left.series.len())
                .filter(|s| {
                    !claimed.contains(&SeriesRef {
                        venue: i,
                        series: *s,
                    })
                })
                .collect();

            for (anchor, candidate, matched) in assign_pairs(left, &free, right, j, &claimed) {
                links.push((
                    SeriesRef {
                        venue: i,
                        series: anchor,
                    },
                    SeriesRef {
                        venue: j,
                        series: candidate,
                    },
                    matched,
                ));
            }
        }
    }

    // Strongest links first, so a doubtful pairing can never pre-empt a confident
    // one when the two would land in the same group.
    links.sort_by(|x, y| by_desc(x.2.score, y.2.score));

    let mut all_groups: Vec<Group> = Vec::new();
    let mut groups = GroupKeys::default();
    let mut group_of: HashMap<SeriesRef, usize> = HashMap::new();

    for (a, b, matched) in links {
        let left = group_of.get(&a).copied();
        let right = group_of.get(&b).copied();
        if left.is_some() && left == right {
            continue;
        }

        let left_members = left.map_or_else(|| vec![a], |g| all_groups[g].members.clone());
        let right_members = right.map_or_else(|| vec![b], |g| all_groups[g].members.clone());
        let mut merged = left_members.clone();
        merged.extend_from_slice(&right_members);

        // One series per venue per group. Two Kalshi series can both link to the
        // same question at different venues without being the same question, and
        // merging them would put two prices in one column.
        let venues: HashSet<Venue> = merged.iter().map(|m| built.ok[m.venue].venue).collect();
        if venues.len() != merged.len() {
            continue;
        }

        let mut evidence: Vec<MatchScore> = Vec::new();
        if let Some(g) = left {
            evidence.extend(all_groups[g].evidence.iter().cloned());
        }
        if let Some(g) = right {
            evidence.extend(all_groups[g].evidence.iter().cloned());
        }
        evidence.push(matched);

        let group = all_groups.len();
        let key = merged[0];
        all_groups.push(Group {
            members: merged.clone(),
            evidence,
        });

        for member in &merged {
            group_of.insert(*member, group);
        }
        if left.is_some() {
            groups.delete(left_members[0]);
        }
        if right.is_some() {
            groups.delete(right_members[0]);
        }
        groups.set(key, group);
    }

    for key in &groups.order {
        let group = &all_groups[groups.keyed[key]];
        if group.members.len() < 2 {
            continue;
        }

        // A group is worth what its weakest pairing is worth, not its best: two
        // solid legs and a doubtful third is a doubtful board.
        let score = group
            .evidence
            .iter()
            .fold(1.0_f64, |worst, m| worst.min(m.score));

        // The busiest member names the group, since its title is the one with a
        // live book behind it.
        let mut members: Vec<&IndexedSeries> = group
            .members
            .iter()
            .map(|m| &built.ok[m.venue].series[m.series])
            .collect();
        let lead = {
            let mut by_volume = members.clone();
            by_volume
                .sort_by(|a, b| by_desc(a.volume24h.unwrap_or(-1.0), b.volume24h.unwrap_or(-1.0)));
            by_volume[0]
        };

        let mut legs: Vec<SeriesLeg> = members.drain(..).map(leg_of).collect();
        legs.sort_by(by_activity);

        found.push(LinkedSeries {
            key: lead.series_ticker.to_lowercase(),
            title: lead.title.clone(),
            legs,
            confidence: weakest(group.evidence.iter().map(|m| m.confidence)),
            reason: group
                .evidence
                .iter()
                .map(|m| m.reason.as_str())
                .collect::<Vec<_>>()
                .join(" · "),
            score,
        });
    }

    found.sort_by(|a, b| {
        b.legs
            .len()
            .cmp(&a.legs.len())
            .then_with(|| by_desc(a.score, b.score))
            .then_with(|| {
                by_desc(
                    a.legs.first().and_then(|l| l.volume24h).unwrap_or(-1.0),
                    b.legs.first().and_then(|l| l.volume24h).unwrap_or(-1.0),
                )
            })
    });

    (found, scanned)
}

/* --------------------------------------------------------------- compare */

/// Where the anchor event was found.
struct Anchor {
    venue: usize,
    series: usize,
    event: usize,
}

/// Locate an event in the indexes, wherever it is listed.
fn find_event(built: &Indexes, event_ticker: &str, venue: Option<Venue>) -> Result<Anchor> {
    let wanted = event_ticker.to_lowercase();

    for (v, index) in built.ok.iter().enumerate() {
        if venue.is_some_and(|only| index.venue != only) {
            continue;
        }
        for (s, series) in index.series.iter().enumerate() {
            if let Some(e) = series
                .events
                .iter()
                .position(|event| event.event_ticker.to_lowercase() == wanted)
            {
                return Ok(Anchor {
                    venue: v,
                    series: s,
                    event: e,
                });
            }
        }
    }

    Err(UpstreamError::not_found(format!(
        "No open event {event_ticker} in any venue's catalogue"
    ))
    .with_hint("Cross-venue comparison works on open events. Use `XV <words>` to find one."))
}

const ISO_DATE: &[BorrowedFormatItem<'_>] =
    time::macros::format_description!("[year]-[month]-[day]");

/// `Date.parse`, in milliseconds, for the timestamps the venues publish.
///
/// A bare date is accepted as UTC midnight because JavaScript's `Date.parse`
/// accepts it, and Polymarket's `endDate` is not consistently a full timestamp.
fn parse_ms(text: &str) -> Option<f64> {
    if text.is_empty() {
        return None;
    }
    if let Ok(moment) = OffsetDateTime::parse(text, &Rfc3339) {
        return Some(moment.unix_timestamp_nanos() as f64 / 1e6);
    }
    Date::parse(text, ISO_DATE)
        .ok()
        .map(|date| date.midnight().assume_utc().unix_timestamp_nanos() as f64 / 1e6)
}

/// The counterpart event at another venue.
///
/// Two steps, because a series match and an expiry match are different
/// questions: find the series first (wording, dates ignored), then the event
/// within it whose numbers and close time line up (dates decisive). Skipping the
/// first step would let `Fed decision in Oct 2026?` match an unrelated series
/// that happens to mention October.
fn counterpart(
    anchor_series: &IndexedSeries,
    anchor_event: &VenueEvent,
    other: &VenueIndex,
) -> Option<(VenueEvent, MatchScore)> {
    let curated = CURATED_LINKS.iter().find(|l| {
        l.leg(anchor_series.venue).is_some_and(|ticker| {
            ticker.to_lowercase() == anchor_series.series_ticker.to_lowercase()
        })
    });
    let curated_ticker = curated.and_then(|l| l.leg(other.venue));

    // A curated ticker that is not in today's catalogue ends the search rather
    // than falling back: the table said which series this is, and it is gone.
    let series = match curated_ticker {
        Some(ticker) => other.by_ticker.get(&ticker.to_lowercase()).copied()?,
        None => best_match(anchor_series, other)?.0,
    };
    let series = &other.series[series];

    let anchor_close = anchor_event
        .markets
        .first()
        .and_then(|m| parse_ms(&m.close_time));

    let mut best: Option<(VenueEvent, MatchScore)> = None;
    for event in &series.events {
        let matched = score_event(
            &SeriesDescriptor::new(&anchor_event.event_ticker, &anchor_event.title),
            &SeriesDescriptor::new(&event.event_ticker, &event.title),
        );

        // Titles alone rarely separate one expiry from the next — Polymarket US
        // words October's and December's Fed events identically — so closeness of
        // the close time breaks the tie.
        let close = event.markets.first().and_then(|m| parse_ms(&m.close_time));
        let days = match (anchor_close, close) {
            (Some(a), Some(b)) => Some((a - b).abs() / 86_400_000.0),
            _ => None,
        };
        let proximity = days.map_or(0.0, |d| (1.0 - d / 30.0).max(0.0));

        let combined = matched.score * 0.7 + proximity * 0.3;
        if combined < MATCH_FLOOR {
            continue;
        }
        if best
            .as_ref()
            .is_none_or(|(_, current)| combined > current.score)
        {
            let reason = match days {
                Some(d) if d < 1.0 => format!("{}; closes the same day", matched.reason),
                Some(d) => format!(
                    "{}; closes {}d apart",
                    matched.reason,
                    round_to(d, 0) as i64
                ),
                None => matched.reason.clone(),
            };
            best = Some((
                event.clone(),
                MatchScore {
                    // The confidence stays the *text* reading's. Proximity broke
                    // a tie between expiries; it is not evidence about whether
                    // the two brokers are asking the same question.
                    confidence: matched.confidence,
                    shared: matched.shared.clone(),
                    score: combined,
                    reason,
                },
            ));
        }
    }

    best
}

fn compare_leg(market: &Market, event_ticker: &str) -> CompareLeg {
    CompareLeg {
        venue: market.venue,
        ticker: market.ticker.clone(),
        event_ticker: event_ticker.to_string(),
        yes_bid: market.yes_bid,
        yes_ask: market.yes_ask,
        mid: market.mid,
        volume24h: market.volume24h,
    }
}

/// A contract's rung, as its venue words it.
fn label_of(market: &Market) -> String {
    if market.yes_sub_title.is_empty() {
        market.title.clone()
    } else {
        market.yes_sub_title.clone()
    }
}

/// The gap between brokers on one outcome.
///
/// `divergence` is what the two books think, spread apart. `edge` is what could
/// actually be traded: the cheapest ask anywhere against the richest bid
/// anywhere, positive only when one venue's offer is below another's bid. They
/// differ by the width of the spreads, which is precisely the part that is not
/// profit.
fn summarise(legs: &[CompareLeg]) -> (Option<f64>, Option<f64>, Vec<Venue>) {
    let mids: Vec<f64> = legs.iter().filter_map(|l| l.mid).collect();
    let divergence = if mids.len() >= 2 {
        let max = mids.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        let min = mids.iter().copied().fold(f64::INFINITY, f64::min);
        Some(round4(max - min))
    } else {
        None
    };

    let mut edge: Option<f64> = None;
    let mut edge_venues: Vec<Venue> = Vec::new();

    for buy in legs {
        for sell in legs {
            if buy.venue == sell.venue {
                continue;
            }
            let (Some(ask), Some(bid)) = (buy.yes_ask, sell.yes_bid) else {
                continue;
            };
            let gap = round4(bid - ask);
            if edge.is_none_or(|current| gap > current) {
                edge = Some(gap);
                edge_venues = vec![buy.venue, sell.venue];
            }
        }
    }

    (divergence, edge, edge_venues)
}

/// One question, quoted at every broker that lists it.
///
/// Quotes are re-fetched live rather than read from the snapshot: the catalogue
/// is up to fifteen minutes old, which is fine for finding a market and useless
/// for pricing one.
pub async fn compare(
    state: &AppState,
    event_ticker: &str,
    venue: Option<Venue>,
) -> Result<CompareResponse> {
    let built = indexes(state).await?;
    let at = find_event(&built, event_ticker, venue)?;

    let anchor_index = &built.ok[at.venue];
    let anchor_series = &anchor_index.series[at.series];
    let anchor_stale = &anchor_series.events[at.event];

    let mut partners: Vec<(Venue, VenueEvent, MatchScore)> = Vec::new();
    for other in &built.ok {
        if other.venue == anchor_index.venue {
            continue;
        }
        if let Some((event, matched)) = counterpart(anchor_series, anchor_stale, other) {
            partners.push((other.venue, event, matched));
        }
    }

    // Live quotes for the anchor and every partner. One failing venue must not
    // take the whole comparison down — it is dropped and the rest still print.
    let mut calls = vec![event_of(
        state,
        anchor_index.venue,
        &anchor_stale.event_ticker,
    )];
    for (partner_venue, event, _) in &partners {
        calls.push(event_of(state, *partner_venue, &event.event_ticker));
    }
    let live = join_all(calls).await;

    let mut live = live.into_iter();
    let anchor = live
        .next()
        .and_then(std::result::Result::ok)
        .unwrap_or_else(|| anchor_stale.clone());

    let mut events: Vec<CompareEventLeg> = vec![CompareEventLeg {
        venue: anchor.venue,
        event_ticker: anchor.event_ticker.clone(),
        title: anchor.title.clone(),
        close_time: anchor
            .markets
            .first()
            .map(|m| m.close_time.clone())
            .unwrap_or_default(),
        confidence: MatchConfidence::Linked,
        score: 1.0,
        reason: "anchor".to_string(),
    }];

    let mut ladders: Vec<VenueEvent> = Vec::new();

    for ((_, stale, matched), result) in partners.into_iter().zip(live) {
        let event = result.unwrap_or(stale);
        events.push(CompareEventLeg {
            venue: event.venue,
            event_ticker: event.event_ticker.clone(),
            title: event.title.clone(),
            close_time: event
                .markets
                .first()
                .map(|m| m.close_time.clone())
                .unwrap_or_default(),
            confidence: matched.confidence,
            score: round_to(matched.score, 2),
            reason: matched.reason,
        });
        ladders.push(event);
    }

    // ---- rows --------------------------------------------------------------
    // The anchor's ladder sets the rows; each partner's contracts are paired onto
    // it, and whatever is left over is reported rather than dropped.
    let mut rows: Vec<CompareRow> = anchor
        .markets
        .iter()
        .map(|market| CompareRow {
            label: label_of(market),
            legs: vec![compare_leg(market, &anchor.event_ticker)],
            divergence: None,
            edge: None,
            edge_venues: Vec::new(),
        })
        .collect();

    let mut unmatched: Vec<CompareUnmatched> = Vec::new();

    for ladder in &ladders {
        let theirs: Vec<String> = ladder.markets.iter().map(label_of).collect();
        let ours: Vec<String> = rows.iter().map(|r| r.label.clone()).collect();
        let paired = pair_labels(&ours, &theirs);

        let used: HashSet<usize> = paired.iter().map(|p| p.right).collect();
        for pair in &paired {
            rows[pair.left].legs.push(compare_leg(
                &ladder.markets[pair.right],
                &ladder.event_ticker,
            ));
        }
        for (i, market) in ladder.markets.iter().enumerate() {
            if !used.contains(&i) {
                unmatched.push(CompareUnmatched {
                    venue: ladder.venue,
                    label: theirs[i].clone(),
                    ticker: market.ticker.clone(),
                });
            }
        }
    }

    for row in &mut rows {
        let (divergence, edge, edge_venues) = summarise(&row.legs);
        row.divergence = divergence;
        row.edge = edge;
        row.edge_venues = edge_venues;
    }

    Ok(CompareResponse {
        title: anchor.title.clone(),
        events,
        // A row only one venue quotes is not a comparison; it stays out of the
        // table and its counterpart-less state is already visible in `events`.
        rows: rows.into_iter().filter(|r| r.legs.len() > 1).collect(),
        unmatched,
    })
}

/// Build the cross-venue board: crawl every catalogue, then pair them up.
///
/// Fired once at startup and never awaited by a request; a failure is logged and
/// dropped, because everything here is cached and the first `XV` simply pays for
/// the crawl instead. Since the pairing now lives with the index it is derived
/// from, this warms the board a request actually reads and not merely the raw
/// material for it.
pub async fn warm_indexes(state: &AppState) {
    match indexes(state).await {
        Ok(built) => {
            let shape = built
                .ok
                .iter()
                .map(|i| format!("{} {}", venue_info(i.venue).code, i.series.len()))
                .collect::<Vec<_>>()
                .join(", ");
            tracing::info!(shape, linked = built.links.len(), "[xvenue] board warm");
            for miss in &built.unavailable {
                tracing::warn!(
                    venue = miss.venue.as_str(),
                    error = %miss.error,
                    "[xvenue] venue unavailable"
                );
            }
        }
        Err(err) => tracing::warn!(error = %err, "[xvenue] index warm failed"),
    }
}

/* ------------------------------------------------------------------ tests */

/// Cross-venue board tests.
///
/// The corpora are built by hand rather than crawled: everything up to the live
/// re-quote is pure, and the cases that matter — a blocking token, a curated leg
/// that no longer exists, two sibling series competing for one counterpart — are
/// only reachable with a catalogue shaped exactly so.
#[cfg(test)]
mod tests {
    use super::*;

    use serde_json::json;
    use terminal_core::matching::{confidence_of, score_series};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use crate::config::Config;

    /* ---------------------------------------------------------- fixtures */

    fn market(venue: Venue, ticker: &str, label: &str, close: &str) -> Market {
        Market {
            venue,
            ticker: ticker.to_string(),
            event_ticker: String::new(),
            series_ticker: String::new(),
            title: label.to_string(),
            yes_sub_title: label.to_string(),
            no_sub_title: String::new(),
            status: "active".to_string(),
            market_type: "binary".to_string(),
            yes_bid: None,
            yes_ask: None,
            no_bid: None,
            no_ask: None,
            mid: None,
            last_price: None,
            previous_price: None,
            change: None,
            volume: None,
            volume24h: None,
            open_interest: None,
            liquidity: None,
            open_time: String::new(),
            close_time: close.to_string(),
            expiration_time: close.to_string(),
            result: String::new(),
            rules_primary: String::new(),
            category: None,
            strike_type: None,
            floor_strike: None,
            cap_strike: None,
        }
    }

    /// A contract with a two-sided book and a volume figure.
    fn quoted(venue: Venue, ticker: &str, label: &str, bid: f64, ask: f64, volume: f64) -> Market {
        Market {
            yes_bid: Some(bid),
            yes_ask: Some(ask),
            mid: Some(round4((bid + ask) / 2.0)),
            volume24h: Some(volume),
            ..market(venue, ticker, label, "2026-10-28T21:00:00Z")
        }
    }

    fn event(
        venue: Venue,
        event_ticker: &str,
        series_ticker: &str,
        title: &str,
        markets: Vec<Market>,
    ) -> VenueEvent {
        VenueEvent {
            venue,
            event_ticker: event_ticker.to_string(),
            series_ticker: series_ticker.to_string(),
            title: title.to_string(),
            sub_title: String::new(),
            category: String::new(),
            mutually_exclusive: false,
            markets,
        }
    }

    /// An event with one nameless contract, for a catalogue whose shape is all
    /// that is under test.
    fn bare(venue: Venue, series_ticker: &str, title: &str) -> VenueEvent {
        event(
            venue,
            &format!("{series_ticker}-26OCT"),
            series_ticker,
            title,
            vec![market(venue, &format!("{series_ticker}-A"), title, "")],
        )
    }

    /// A board built the way the server builds one, minus the crawl.
    ///
    /// The pairing pass is run here rather than left to the caller so that every
    /// test below reads `built.links` exactly as a request does, and a change to
    /// where that work happens cannot quietly stop being tested.
    fn indexes_of(venues: Vec<(Venue, Vec<VenueEvent>)>) -> Indexes {
        paired(collect_indexes(
            &venues.iter().map(|(venue, _)| *venue).collect::<Vec<_>>(),
            venues
                .into_iter()
                .map(|(venue, events)| Ok(build_index(venue, &events, false)))
                .collect(),
        ))
    }

    fn keys(response: &LinkedSeriesResponse) -> Vec<&str> {
        response.series.iter().map(|s| s.key.as_str()).collect()
    }

    /* ------------------------------------------------------------- index */

    #[test]
    fn the_index_takes_the_busiest_event_as_the_series_exemplar() {
        let quiet = event(
            Venue::Kalshi,
            "KXFEDDECISION-27JAN",
            "KXFEDDECISION",
            "Fed decision in Jan 2027?",
            vec![quoted(Venue::Kalshi, "A", "Cut 25bps", 0.1, 0.2, 5.0)],
        );
        let busy = event(
            Venue::Kalshi,
            "KXFEDDECISION-26OCT",
            "KXFEDDECISION",
            "Fed decision in Oct 2026?",
            vec![quoted(Venue::Kalshi, "B", "Cut 25bps", 0.1, 0.2, 900.0)],
        );

        let index = build_index(Venue::Kalshi, &[quiet, busy], false);
        assert_eq!(index.series.len(), 1);
        assert_eq!(index.series[0].title, "Fed decision in Oct 2026?");
        assert_eq!(index.series[0].sample_event, "KXFEDDECISION-26OCT");
        // Both events stay on the series; only the order changed.
        assert_eq!(index.series[0].events.len(), 2);
        assert_eq!(index.series[0].markets, 2);
        assert_eq!(index.series[0].volume24h, Some(905.0));
    }

    #[test]
    fn the_index_keeps_an_unpublished_volume_distinct_from_zero() {
        let event = event(
            Venue::PolymarketUs,
            "usfed-fomc-2026-10-28",
            "usfed-fomc",
            "Fed Decision in October",
            vec![market(Venue::PolymarketUs, "a", "No change", "")],
        );
        let index = build_index(Venue::PolymarketUs, &[event], false);
        assert_eq!(index.series[0].volume24h, None);
    }

    #[test]
    fn the_index_reports_the_soonest_close_across_the_series() {
        let later = event(
            Venue::Kalshi,
            "KXFEDDECISION-27JAN",
            "KXFEDDECISION",
            "Fed decision in Jan 2027?",
            vec![market(
                Venue::Kalshi,
                "A",
                "Cut 25bps",
                "2027-01-27T21:00:00Z",
            )],
        );
        let sooner = event(
            Venue::Kalshi,
            "KXFEDDECISION-26OCT",
            "KXFEDDECISION",
            "Fed decision in Oct 2026?",
            vec![market(
                Venue::Kalshi,
                "B",
                "Cut 25bps",
                "2026-10-28T21:00:00Z",
            )],
        );
        let index = build_index(Venue::Kalshi, &[later, sooner], false);
        assert_eq!(index.series[0].close_time, "2026-10-28T21:00:00Z");
    }

    #[test]
    fn an_event_with_no_series_ticker_is_indexed_under_its_own() {
        let orphan = event(
            Venue::Polymarket,
            "some-one-off-question",
            "",
            "Some one-off question",
            vec![market(Venue::Polymarket, "a", "Yes", "")],
        );
        let index = build_index(Venue::Polymarket, &[orphan], false);
        assert_eq!(index.series[0].series_ticker, "some-one-off-question");
    }

    #[test]
    fn the_blocking_index_drops_a_token_carried_by_more_than_a_twentieth_of_a_venue() {
        // Ceiling is floor(100 * 0.05) = 5, and a token is dropped when it is
        // carried by *more* than that.
        let mut events: Vec<VenueEvent> = Vec::new();
        for i in 0..6 {
            events.push(bare(
                Venue::Kalshi,
                &format!("KXBASKET{i}"),
                &format!("Basketball fixture {i}"),
            ));
        }
        for i in 6..11 {
            events.push(bare(
                Venue::Kalshi,
                &format!("KXCURL{i}"),
                &format!("Curling fixture {i}"),
            ));
        }
        for i in 11..100 {
            events.push(bare(
                Venue::Kalshi,
                &format!("KXFILL{i}"),
                &format!("Filler fixture {i}"),
            ));
        }

        let index = build_index(Venue::Kalshi, &events, false);
        assert_eq!(index.series.len(), 100);

        // Six is over the ceiling; five is exactly at it and survives.
        assert!(
            !index.by_token.contains_key("basketball"),
            "a token in 6 of 100 series should be dropped"
        );
        assert_eq!(index.by_token.get("curling").map(Vec::len), Some(5));
        // And the token every single series carries is gone entirely.
        assert!(!index.by_token.contains_key("fixture"));
    }

    #[test]
    fn the_blocking_ceiling_never_falls_below_four() {
        // floor(10 * 0.05) is 0, which would drop every token in a small
        // catalogue and leave the venue uncomparable.
        let mut events: Vec<VenueEvent> = Vec::new();
        for i in 0..4 {
            events.push(bare(
                Venue::Kalshi,
                &format!("KXCURL{i}"),
                &format!("Curling fixture {i}"),
            ));
        }
        for i in 4..10 {
            events.push(bare(
                Venue::Kalshi,
                &format!("KXBASKET{i}"),
                &format!("Basketball fixture {i}"),
            ));
        }

        let index = build_index(Venue::Kalshi, &events, false);
        assert_eq!(index.by_token.get("curling").map(Vec::len), Some(4));
        assert!(!index.by_token.contains_key("basketball"));
    }

    #[test]
    fn a_venue_whose_corpus_failed_is_named_rather_than_hidden() {
        let built = collect_indexes(
            &[Venue::Kalshi, Venue::Polymarket, Venue::PolymarketUs],
            vec![
                Ok(build_index(
                    Venue::Kalshi,
                    &[bare(Venue::Kalshi, "KXNOBEL", "Nobel Peace Prize winner")],
                    false,
                )),
                Err(UpstreamError::timeout("Polymarket timed out after 20s")),
                Ok(build_index(
                    Venue::PolymarketUs,
                    &[bare(
                        Venue::PolymarketUs,
                        "nobel-peace",
                        "Nobel Peace Prize Winner",
                    )],
                    false,
                )),
            ],
        );

        assert_eq!(built.ok.len(), 2);
        assert_eq!(built.unavailable.len(), 1);
        assert_eq!(built.unavailable[0].venue, Venue::Polymarket);
        assert_eq!(built.unavailable[0].error, "Polymarket timed out after 20s");

        // …and the rest of the board still prints.
        let response = link_series(&paired(built), "", 40);
        assert_eq!(response.unavailable.len(), 1);
        assert_eq!(response.series.len(), 1);
        assert_eq!(response.series[0].legs.len(), 2);
        assert_eq!(response.scanned.get("kalshi"), Some(&1));
        assert_eq!(response.scanned.get("polymarket"), None);
    }

    /* ----------------------------------------------------- curated links */

    #[test]
    fn a_curated_link_is_stated_when_both_legs_are_live() {
        let built = indexes_of(vec![
            (
                Venue::Kalshi,
                vec![bare(
                    Venue::Kalshi,
                    "KXFEDDECISION",
                    "Fed decision in Oct 2026?",
                )],
            ),
            (
                Venue::PolymarketUs,
                vec![bare(
                    Venue::PolymarketUs,
                    "usfed-fomc",
                    "Fed Decision in October",
                )],
            ),
        ]);

        let response = link_series(&built, "", 40);
        assert_eq!(keys(&response), vec!["fed-decision"]);
        assert_eq!(response.series[0].confidence, MatchConfidence::Linked);
        assert_eq!(response.series[0].score, 1.0);
        assert_eq!(
            response.series[0].reason,
            "curated link, checked against both catalogues"
        );
    }

    #[test]
    fn a_curated_leg_absent_from_the_live_catalogue_drops_the_link() {
        // Kalshi still lists its side; Polymarket US has retired `usfed-fomc`,
        // and the table is not allowed to quote it anyway.
        let built = indexes_of(vec![
            (
                Venue::Kalshi,
                vec![bare(
                    Venue::Kalshi,
                    "KXFEDDECISION",
                    "Fed decision in Oct 2026?",
                )],
            ),
            (
                Venue::PolymarketUs,
                vec![bare(Venue::PolymarketUs, "nfl-2027", "NFL Champion 2027")],
            ),
        ]);

        let response = link_series(&built, "", 40);
        assert!(
            !keys(&response).contains(&"fed-decision"),
            "a link with one live leg is not a link"
        );
    }

    #[test]
    fn a_dropped_curated_link_leaves_its_surviving_leg_free_to_be_matched() {
        // The table names `fomc` at Polymarket, which is not listed today — but
        // `fed-decision` is, and it is plainly the same question as Kalshi's
        // side. A stale table entry must cost a *stated* link, not a found one:
        // claiming the surviving leg on behalf of a pairing that no longer
        // exists took a live series out of the text-matching pass entirely, so a
        // rotated ticker at one broker made the other broker's listing
        // unpairable rather than merely unlabelled.
        let built = indexes_of(vec![
            (
                Venue::Kalshi,
                vec![bare(
                    Venue::Kalshi,
                    "KXFEDDECISION",
                    "Fed decision in Oct 2026?",
                )],
            ),
            (
                Venue::Polymarket,
                vec![bare(
                    Venue::Polymarket,
                    "fed-decision",
                    "Fed Decision in October?",
                )],
            ),
        ]);

        let response = link_series(&built, "", 40);
        let group = response.series.first().unwrap_or_else(|| {
            panic!("the two live Fed series should pair: {:?}", keys(&response))
        });
        assert_eq!(group.legs.len(), 2);
        // Found by text, so it is labelled as the reading it is — never as the
        // curated link, which is the one band that is stated rather than
        // inferred.
        assert_ne!(group.confidence, MatchConfidence::Linked);
    }

    /* --------------------------------------------------------- assigning */

    /// One Emmy category listed twice at each venue — the award itself and the
    /// ceremony-night book — which is the family the global pass exists for:
    /// every one of the four pairings scores respectably, and *both* anchors
    /// read best against the same counterpart.
    fn sibling_indexes() -> (VenueIndex, VenueIndex) {
        let left = build_index(
            Venue::Kalshi,
            &[
                bare(
                    Venue::Kalshi,
                    "KXEMMYLEADACTORCOMEDY",
                    "Emmy Winner: Outstanding Lead Actor in a Comedy Series",
                ),
                bare(
                    Venue::Kalshi,
                    "KXEMMYLEADACTORCOMEDYNIGHT",
                    "Emmy Winner: Outstanding Lead Actor in a Comedy Series, night one",
                ),
            ],
            false,
        );
        let right = build_index(
            Venue::Polymarket,
            &[
                bare(
                    Venue::Polymarket,
                    "emmy-lead-actor-comedy",
                    "Emmy: Outstanding Lead Actor in a Comedy Series",
                ),
                bare(
                    Venue::Polymarket,
                    "emmy-lead-actor-comedy-ceremony",
                    "Emmy: Outstanding Lead Actor in a Comedy Series ceremony night",
                ),
            ],
            false,
        );
        (left, right)
    }

    #[test]
    fn assignment_is_global_best_first_not_one_anchor_at_a_time() {
        let (left, right) = sibling_indexes();

        // Asked in isolation, both anchors name the same counterpart …
        let award_alone = best_match(&left.series[0], &right).expect("a match");
        let night_alone = best_match(&left.series[1], &right).expect("a match");
        assert_eq!(award_alone.0, night_alone.0, "the shape under test");
        assert!(award_alone.1.score > night_alone.1.score);

        // … so per-anchor greedy would double-book it and leave one sibling
        // unpaired. Global best-first hands it to the anchor that scored higher
        // and gives the other its own second choice.
        let assigned = assign_pairs(&left, &[0, 1], &right, 1, &HashSet::new());
        assert_eq!(assigned.len(), 2);

        let paired: HashMap<usize, usize> = assigned.iter().map(|(a, b, _)| (*a, *b)).collect();
        assert_eq!(paired.len(), 2, "no counterpart is claimed twice");
        assert_ne!(paired[&0], paired[&1]);
        assert_eq!(
            right.series[paired[&0]].series_ticker,
            "emmy-lead-actor-comedy"
        );
        assert_eq!(
            right.series[paired[&1]].series_ticker,
            "emmy-lead-actor-comedy-ceremony"
        );

        // Best-first, so the strongest pairing is also the one reported first.
        assert!(assigned[0].2.score > assigned[1].2.score);
    }

    #[test]
    fn an_already_claimed_counterpart_is_not_offered_to_the_matcher() {
        let (left, right) = sibling_indexes();
        let taken: HashSet<SeriesRef> = [SeriesRef {
            venue: 1,
            series: 0,
        }]
        .into_iter()
        .collect();

        let assigned = assign_pairs(&left, &[0, 1], &right, 1, &taken);
        assert!(assigned.iter().all(|(_, candidate, _)| *candidate != 0));
    }

    /* ---------------------------------------------------------- grouping */

    #[test]
    fn a_group_holds_at_most_one_series_per_venue() {
        // Two Kalshi series both read as the same question as one Polymarket
        // series. Merging them would put two Kalshi prices in one column.
        let built = indexes_of(vec![
            (
                Venue::Kalshi,
                vec![
                    bare(Venue::Kalshi, "KXNOBELPEACE", "Nobel Peace Prize winner"),
                    bare(Venue::Kalshi, "KXNOBELPEACEALT", "Nobel Peace Prize winner"),
                ],
            ),
            (
                Venue::Polymarket,
                vec![bare(
                    Venue::Polymarket,
                    "nobel-peace-prize",
                    "Nobel Peace Prize Winner",
                )],
            ),
        ]);

        let response = link_series(&built, "", 40);
        for series in &response.series {
            let venues: HashSet<Venue> = series.legs.iter().map(|l| l.venue).collect();
            assert_eq!(
                venues.len(),
                series.legs.len(),
                "one leg per venue: {:?}",
                series.legs
            );
        }
    }

    #[test]
    fn a_group_is_worth_what_its_weakest_pairing_is_worth() {
        let built = indexes_of(vec![
            (
                Venue::Kalshi,
                vec![bare(
                    Venue::Kalshi,
                    "KXNOBELPEACE",
                    "2026 Nobel Peace Prize winner",
                )],
            ),
            (
                Venue::Polymarket,
                vec![bare(
                    Venue::Polymarket,
                    "nobel-peace-prize-winner-2026",
                    "Nobel Peace Prize Winner 2026",
                )],
            ),
            (
                Venue::PolymarketUs,
                vec![bare(
                    Venue::PolymarketUs,
                    "nobel-peace",
                    "Nobel Peace Prize",
                )],
            ),
        ]);

        let response = link_series(&built, "", 40);
        let board = response
            .series
            .iter()
            .find(|s| s.legs.len() == 3)
            .expect("a three-venue board");

        // Two links build a board of three: the strongest pair, then whichever
        // link drags the third member in. The board is worth the weaker of them.
        let kalshi = SeriesDescriptor::new("KXNOBELPEACE", "2026 Nobel Peace Prize winner");
        let poly = SeriesDescriptor::new(
            "nobel-peace-prize-winner-2026",
            "Nobel Peace Prize Winner 2026",
        );
        let us = SeriesDescriptor::new("nobel-peace", "Nobel Peace Prize");
        let mut pairwise = [
            score_series(&kalshi, &poly).score,
            score_series(&kalshi, &us).score,
            score_series(&poly, &us).score,
        ];
        pairwise.sort_by(|a, b| by_desc(*a, *b));

        assert_eq!(board.score, pairwise[1]);
        assert!(
            board.score < pairwise[0],
            "the board took its best pairing's score rather than its worst"
        );
        assert_eq!(board.confidence, confidence_of(pairwise[1]));
        assert!(board.reason.contains('·'), "{}", board.reason);
    }

    #[test]
    fn weakest_reads_linked_as_the_best_band_and_weak_as_the_worst() {
        assert_eq!(weakest([]), MatchConfidence::Linked);
        assert_eq!(
            weakest([MatchConfidence::Strong, MatchConfidence::Linked]),
            MatchConfidence::Strong
        );
        assert_eq!(
            weakest([
                MatchConfidence::Strong,
                MatchConfidence::Weak,
                MatchConfidence::Likely,
            ]),
            MatchConfidence::Weak
        );
    }

    #[test]
    fn the_busiest_leg_names_the_board_and_leads_it() {
        let kalshi = event(
            Venue::Kalshi,
            "KXNOBELPEACE-26",
            "KXNOBELPEACE",
            "Nobel Peace Prize winner",
            vec![quoted(Venue::Kalshi, "a", "Someone", 0.1, 0.2, 12.0)],
        );
        let poly = event(
            Venue::Polymarket,
            "nobel-peace-prize-2026",
            "nobel-peace-prize",
            "Nobel Peace Prize Winner",
            vec![quoted(Venue::Polymarket, "b", "Someone", 0.1, 0.2, 4_000.0)],
        );

        let built = indexes_of(vec![
            (Venue::Kalshi, vec![kalshi]),
            (Venue::Polymarket, vec![poly]),
        ]);
        let response = link_series(&built, "", 40);
        assert_eq!(response.series.len(), 1);
        assert_eq!(response.series[0].key, "nobel-peace-prize");
        assert_eq!(response.series[0].legs[0].venue, Venue::Polymarket);
        assert_eq!(response.series[0].legs[0].volume24h, Some(4_000.0));
    }

    #[test]
    fn the_query_narrows_the_board_on_key_title_and_leg() {
        let built = indexes_of(vec![
            (
                Venue::Kalshi,
                vec![
                    bare(Venue::Kalshi, "KXFEDDECISION", "Fed decision in Oct 2026?"),
                    bare(Venue::Kalshi, "KXNOBELPEACE", "Nobel Peace Prize winner"),
                ],
            ),
            (
                Venue::PolymarketUs,
                vec![
                    bare(Venue::PolymarketUs, "usfed-fomc", "Fed Decision in October"),
                    bare(
                        Venue::PolymarketUs,
                        "nobel-peace",
                        "Nobel Peace Prize Winner",
                    ),
                ],
            ),
        ]);

        assert_eq!(link_series(&built, "", 40).series.len(), 2);
        assert_eq!(keys(&link_series(&built, "fed", 40)), vec!["fed-decision"]);
        assert_eq!(link_series(&built, "nobel", 40).series.len(), 1);
        assert!(link_series(&built, "fed nobel", 40).series.is_empty());
        // The limit is applied after the sort, not before it.
        assert_eq!(link_series(&built, "", 1).series.len(), 1);
    }

    /* ----------------------------------------------------------- compare */

    #[test]
    fn divergence_is_the_widest_mid_against_the_narrowest() {
        let legs = vec![
            compare_leg(&quoted(Venue::Kalshi, "a", "Yes", 0.70, 0.73, 1.0), "e1"),
            compare_leg(
                &quoted(Venue::Polymarket, "b", "Yes", 0.71, 0.72, 1.0),
                "e2",
            ),
            compare_leg(
                &quoted(Venue::PolymarketUs, "c", "Yes", 0.60, 0.62, 1.0),
                "e3",
            ),
        ];

        let (divergence, _, _) = summarise(&legs);
        // Mids are 0.715, 0.715 and 0.61.
        assert_eq!(divergence, Some(0.105));
    }

    #[test]
    fn divergence_needs_two_quotes_to_mean_anything() {
        let legs = vec![compare_leg(
            &quoted(Venue::Kalshi, "a", "Yes", 0.70, 0.73, 1.0),
            "e1",
        )];
        assert_eq!(summarise(&legs).0, None);
    }

    #[test]
    fn edge_is_the_cheapest_ask_anywhere_against_the_richest_bid() {
        // Kalshi offers at 0.73; Polymarket US bids 0.80. Buying the Kalshi ask
        // and selling the Polymarket US bid is worth 7 cents.
        let legs = vec![
            compare_leg(&quoted(Venue::Kalshi, "a", "Yes", 0.70, 0.73, 1.0), "e1"),
            compare_leg(
                &quoted(Venue::PolymarketUs, "c", "Yes", 0.80, 0.82, 1.0),
                "e3",
            ),
        ];

        let (_, edge, venues) = summarise(&legs);
        assert_eq!(edge, Some(0.07));
        // Cheap side first: buy at Kalshi, sell at Polymarket US.
        assert_eq!(venues, vec![Venue::Kalshi, Venue::PolymarketUs]);
    }

    #[test]
    fn edge_is_negative_when_the_books_are_not_crossed() {
        let legs = vec![
            compare_leg(&quoted(Venue::Kalshi, "a", "Yes", 0.70, 0.73, 1.0), "e1"),
            compare_leg(
                &quoted(Venue::Polymarket, "b", "Yes", 0.68, 0.72, 1.0),
                "e2",
            ),
        ];

        let (_, edge, venues) = summarise(&legs);
        // Buying Kalshi at 0.73 to sell Polymarket at 0.68 loses 5 cents; the
        // other way round loses 2, and the widest gap is the one reported.
        assert_eq!(edge, Some(-0.02));
        assert_eq!(venues, vec![Venue::Polymarket, Venue::Kalshi]);
    }

    #[test]
    fn edge_is_unstated_when_one_side_of_a_book_is_empty() {
        let mut hollow = quoted(Venue::Polymarket, "b", "Yes", 0.71, 0.72, 1.0);
        hollow.yes_bid = None;
        hollow.yes_ask = None;

        let legs = vec![
            compare_leg(&quoted(Venue::Kalshi, "a", "Yes", 0.70, 0.73, 1.0), "e1"),
            compare_leg(&hollow, "e2"),
        ];
        assert_eq!(summarise(&legs).1, None);
    }

    #[test]
    fn a_close_time_within_a_day_reads_as_the_same_day() {
        let anchor = event(
            Venue::Kalshi,
            "KXFEDDECISION-26OCT",
            "KXFEDDECISION",
            "Fed decision in Oct 2026?",
            vec![market(
                Venue::Kalshi,
                "a",
                "No change",
                "2026-10-28T21:00:00Z",
            )],
        );
        let series = &build_index(Venue::Kalshi, std::slice::from_ref(&anchor), false).series[0];

        let other = build_index(
            Venue::PolymarketUs,
            &[
                event(
                    Venue::PolymarketUs,
                    "usfed-fomc-2026-10-28",
                    "usfed-fomc",
                    "Fed Decision in October",
                    vec![market(
                        Venue::PolymarketUs,
                        "b",
                        "No change",
                        "2026-10-28T22:00:00Z",
                    )],
                ),
                event(
                    Venue::PolymarketUs,
                    "usfed-fomc-2026-12-16",
                    "usfed-fomc",
                    "Fed Decision in December",
                    vec![market(
                        Venue::PolymarketUs,
                        "c",
                        "No change",
                        "2026-12-16T22:00:00Z",
                    )],
                ),
            ],
            false,
        );

        let (event, matched) = counterpart(series, &anchor, &other).expect("a counterpart");
        assert_eq!(event.event_ticker, "usfed-fomc-2026-10-28");
        assert!(
            matched.reason.contains("closes the same day"),
            "{}",
            matched.reason
        );
    }

    #[test]
    fn a_curated_ticker_that_is_gone_ends_the_search_rather_than_guessing() {
        let anchor = bare(Venue::Kalshi, "KXFEDDECISION", "Fed decision in Oct 2026?");
        let series = &build_index(Venue::Kalshi, std::slice::from_ref(&anchor), false).series[0];

        // The catalogue carries a series the matcher would happily settle for,
        // but the table already said this is `usfed-fomc`, and that is gone.
        let other = build_index(
            Venue::PolymarketUs,
            &[bare(
                Venue::PolymarketUs,
                "fed-decision",
                "Fed Decision in October",
            )],
            false,
        );

        assert!(counterpart(series, &anchor, &other).is_none());
    }

    /* ------------------------------------------------------- the wire */

    /// A state whose Kalshi and Polymarket US bases both point at the fixture
    /// server; their paths do not collide.
    fn state_for(server: &MockServer) -> AppState {
        AppState::new(Config {
            kalshi_api_base: server.uri(),
            polymarket_us_api_base: server.uri(),
            ..Config::default()
        })
    }

    /// The Fed ladder as each venue words it, with the anchor listing one rung
    /// the partner does not and the partner listing two the anchor does not.
    fn fed_indexes() -> Indexes {
        let kalshi = event(
            Venue::Kalshi,
            "KXFEDDECISION-26OCT",
            "KXFEDDECISION",
            "Fed decision in Oct 2026?",
            vec![
                quoted(Venue::Kalshi, "KXFED-C25", "Cut 25bps", 0.04, 0.05, 500.0),
                quoted(
                    Venue::Kalshi,
                    "KXFED-HOLD",
                    "Fed maintains rate",
                    0.70,
                    0.73,
                    900.0,
                ),
                quoted(Venue::Kalshi, "KXFED-H25", "Hike 25bps", 0.23, 0.24, 100.0),
            ],
        );
        let us = event(
            Venue::PolymarketUs,
            "usfed-fomc-2026-10-28",
            "usfed-fomc",
            "Fed Decision in October",
            vec![
                quoted(
                    Venue::PolymarketUs,
                    "25-bps-decrease",
                    "25 bps decrease",
                    0.07,
                    0.08,
                    1.0,
                ),
                quoted(
                    Venue::PolymarketUs,
                    "no-change",
                    "No change",
                    0.70,
                    0.71,
                    1.0,
                ),
                quoted(
                    Venue::PolymarketUs,
                    "50-bps-decrease",
                    "50+ bps decrease",
                    0.01,
                    0.02,
                    1.0,
                ),
            ],
        );

        indexes_of(vec![
            (Venue::Kalshi, vec![kalshi]),
            (Venue::PolymarketUs, vec![us]),
        ])
    }

    async fn seed(state: &AppState, built: Indexes) {
        state.cache().set(INDEX_KEY, built, ttl::CATALOGUE).await;
    }

    fn kalshi_event_json() -> serde_json::Value {
        json!({
            "event": {
                "event_ticker": "KXFEDDECISION-26OCT",
                "series_ticker": "KXFEDDECISION",
                "title": "Fed decision in Oct 2026?",
                "markets": [
                    {
                        "ticker": "KXFED-C25",
                        "event_ticker": "KXFEDDECISION-26OCT",
                        "yes_sub_title": "Cut 25bps",
                        "yes_bid_dollars": "0.0600",
                        "yes_ask_dollars": "0.0700",
                        "close_time": "2026-10-28T21:00:00Z"
                    },
                    {
                        "ticker": "KXFED-HOLD",
                        "event_ticker": "KXFEDDECISION-26OCT",
                        "yes_sub_title": "Fed maintains rate",
                        "yes_bid_dollars": "0.7400",
                        "yes_ask_dollars": "0.7600",
                        "close_time": "2026-10-28T21:00:00Z"
                    },
                    {
                        "ticker": "KXFED-H25",
                        "event_ticker": "KXFEDDECISION-26OCT",
                        "yes_sub_title": "Hike 25bps",
                        "yes_bid_dollars": "0.2300",
                        "yes_ask_dollars": "0.2400",
                        "close_time": "2026-10-28T21:00:00Z"
                    }
                ]
            }
        })
    }

    fn us_event_json() -> serde_json::Value {
        json!({
            "event": {
                "ticker": "usfed-fomc-2026-10-28",
                "slug": "usfed-fomc-2026-10-28",
                "seriesSlug": "usfed-fomc",
                "title": "Fed Decision in October",
                "markets": [
                    {
                        "slug": "25-bps-decrease",
                        "title": "25 bps decrease",
                        "bestBidQuote": "0.0900",
                        "bestAskQuote": "0.1000",
                        "endDate": "2026-10-28T21:00:00Z"
                    },
                    {
                        "slug": "no-change",
                        "title": "No change",
                        "bestBidQuote": "0.8000",
                        "bestAskQuote": "0.8100",
                        "endDate": "2026-10-28T21:00:00Z"
                    },
                    {
                        "slug": "50-bps-decrease",
                        "title": "50+ bps decrease",
                        "bestBidQuote": "0.0100",
                        "bestAskQuote": "0.0200",
                        "endDate": "2026-10-28T21:00:00Z"
                    }
                ]
            }
        })
    }

    #[tokio::test]
    async fn compare_requotes_both_venues_live() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/events/KXFEDDECISION-26OCT"))
            .respond_with(ResponseTemplate::new(200).set_body_json(kalshi_event_json()))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/events/slug/usfed-fomc-2026-10-28"))
            .respond_with(ResponseTemplate::new(200).set_body_json(us_event_json()))
            .mount(&server)
            .await;

        let state = state_for(&server);
        seed(&state, fed_indexes()).await;

        let response = compare(&state, "KXFEDDECISION-26OCT", None)
            .await
            .expect("a comparison");

        assert_eq!(response.title, "Fed decision in Oct 2026?");
        assert_eq!(response.events.len(), 2);
        assert_eq!(response.events[0].confidence, MatchConfidence::Linked);
        assert_eq!(response.events[0].reason, "anchor");
        assert_eq!(response.events[1].venue, Venue::PolymarketUs);
        assert_eq!(response.events[1].event_ticker, "usfed-fomc-2026-10-28");

        // The snapshot's Kalshi hold quote was 0.70/0.73; the live one is
        // 0.74/0.76, and the row prints the live one.
        let hold = response
            .rows
            .iter()
            .find(|r| r.label == "Fed maintains rate")
            .expect("the hold rung");
        assert_eq!(hold.legs.len(), 2);
        assert_eq!(hold.legs[0].yes_bid, Some(0.74));
        assert_eq!(hold.legs[1].venue, Venue::PolymarketUs);
        assert_eq!(hold.legs[1].yes_bid, Some(0.80));
        // Mids of 0.75 and 0.805.
        assert_eq!(hold.divergence, Some(0.055));
        // Buy the Kalshi ask at 0.76, sell the Polymarket US bid at 0.80.
        assert_eq!(hold.edge, Some(0.04));
        assert_eq!(hold.edge_venues, vec![Venue::Kalshi, Venue::PolymarketUs]);
    }

    #[tokio::test]
    async fn compare_prints_the_rest_when_one_venue_fails() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/events/KXFEDDECISION-26OCT"))
            .respond_with(ResponseTemplate::new(200).set_body_json(kalshi_event_json()))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/events/slug/usfed-fomc-2026-10-28"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let state = state_for(&server);
        seed(&state, fed_indexes()).await;

        let response = compare(&state, "KXFEDDECISION-26OCT", None)
            .await
            .expect("a comparison");

        // The partner leg is still there, quoted from the snapshot.
        assert_eq!(response.events.len(), 2);
        let hold = response
            .rows
            .iter()
            .find(|r| r.label == "Fed maintains rate")
            .expect("the hold rung");
        assert_eq!(hold.legs.len(), 2);
        // Live Kalshi, stale Polymarket US.
        assert_eq!(hold.legs[0].yes_bid, Some(0.74));
        assert_eq!(hold.legs[1].yes_bid, Some(0.70));
    }

    #[tokio::test]
    async fn compare_reports_every_rung_one_venue_lists_and_the_other_does_not() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/events/KXFEDDECISION-26OCT"))
            .respond_with(ResponseTemplate::new(200).set_body_json(kalshi_event_json()))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/events/slug/usfed-fomc-2026-10-28"))
            .respond_with(ResponseTemplate::new(200).set_body_json(us_event_json()))
            .mount(&server)
            .await;

        let state = state_for(&server);
        seed(&state, fed_indexes()).await;

        let response = compare(&state, "KXFEDDECISION-26OCT", None)
            .await
            .expect("a comparison");

        // Polymarket US lists a 50+ bps rung Kalshi does not; it is named rather
        // than dropped.
        let labels: Vec<&str> = response
            .unmatched
            .iter()
            .map(|u| u.label.as_str())
            .collect();
        assert!(labels.contains(&"50+ bps decrease"), "{labels:?}");
        assert!(response
            .unmatched
            .iter()
            .all(|u| u.venue == Venue::PolymarketUs));

        // Kalshi's hike rung has no counterpart, so it is not a comparison and
        // stays out of the table.
        assert!(response.rows.iter().all(|r| r.legs.len() > 1));
        assert!(!response.rows.iter().any(|r| r.label == "Hike 25bps"));
    }

    #[tokio::test]
    async fn compare_says_so_when_the_event_is_in_nobody_s_catalogue() {
        let server = MockServer::start().await;
        let state = state_for(&server);
        seed(&state, fed_indexes()).await;

        let err = compare(&state, "KXNOSUCHTHING-26OCT", None)
            .await
            .expect_err("a not-found");
        assert_eq!(err.code, crate::error::codes::NOT_FOUND);
        assert!(err.message.contains("KXNOSUCHTHING-26OCT"));
        assert!(err.hint.is_some());
    }

    #[tokio::test]
    async fn compare_can_be_pinned_to_one_venue() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/events/slug/usfed-fomc-2026-10-28"))
            .respond_with(ResponseTemplate::new(200).set_body_json(us_event_json()))
            .mount(&server)
            .await;

        let state = state_for(&server);
        seed(&state, fed_indexes()).await;

        // The Kalshi event exists, but the caller asked Polymarket US only.
        assert!(
            compare(&state, "KXFEDDECISION-26OCT", Some(Venue::PolymarketUs))
                .await
                .is_err()
        );

        let response = compare(&state, "usfed-fomc-2026-10-28", Some(Venue::PolymarketUs))
            .await
            .expect("a comparison");
        assert_eq!(response.events[0].venue, Venue::PolymarketUs);
    }

    #[tokio::test]
    async fn linked_series_reads_the_cached_index() {
        let server = MockServer::start().await;
        let state = state_for(&server);
        seed(&state, fed_indexes()).await;

        let response = linked_series(&state, "fed", 40).await.expect("a board");
        assert_eq!(response.query, "fed");
        assert_eq!(keys(&response), vec!["fed-decision"]);
        assert_eq!(response.scanned.get("kalshi"), Some(&1));
        assert!(response.unavailable.is_empty());
        assert!(response.snapshot_age_seconds < 5.0);
    }

    #[tokio::test]
    async fn warming_never_propagates_a_dead_venue() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        // Every base points at a server that answers nothing usable; the warm
        // has to come back quietly all the same.
        warm_indexes(&AppState::new(Config {
            kalshi_api_base: server.uri(),
            polymarket_gamma_base: server.uri(),
            polymarket_us_api_base: server.uri(),
            ..Config::default()
        }))
        .await;
    }
}
