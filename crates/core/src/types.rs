//! Wire contract shared by the server routes and the browser client.
//!
//! Everything the server hands back is already normalised: prices are plain
//! numbers in dollars (0..1 for a binary contract), sizes and volumes are plain
//! numbers, and timestamps are unix seconds (UTC). Each upstream has its own
//! dialect — Kalshi speaks fixed-point decimal *strings* (`"0.6900"`,
//! `"12645.98"`), Polymarket wraps prices in `{value, currency}` objects and
//! ships JSON arrays as JSON *strings* — and none of that leaks past the server
//! boundary. A normalised [`Market`] from Kalshi and one from either Polymarket
//! are the same shape, and carry a [`Venue`] saying which.
//!
//! These structs are the single source of truth for the TypeScript the client
//! compiles against: `ts-rs` derives the `.ts` declarations from them, so the
//! two cannot drift.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

pub use crate::venue::{MoverSort, Venue};

/* ------------------------------------------------------------------ errors */

/// The body of every failed API response.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../client/src/lib/api/gen/")]
pub struct ApiError {
    pub error: String,
    /// Machine-readable reason, e.g. `upstream_blocked`, `not_found`.
    pub code: String,
    /// Operator-facing remediation hint, shown verbatim in the terminal.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub hint: Option<String>,
    /// Upstream HTTP status, when the failure came from a remote host.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub status: Option<u16>,
}

/* ----------------------------------------------------------------- markets */

/// A contract's lifecycle state.
///
/// Left as free text rather than an enum: each venue has its own vocabulary
/// (`finalized`, `determined`, `MARKET_STATUS_OPEN`) and the terminal displays
/// whatever it is told rather than dropping a state it has not seen before.
pub type MarketStatus = String;

/// How a contract's strike relates to the settlement value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../../../client/src/lib/api/gen/")]
pub enum StrikeType {
    /// Settles YES above [`Market::floor_strike`].
    Greater,
    GreaterOrEqual,
    /// Settles YES at or below [`Market::cap_strike`].
    Less,
    LessOrEqual,
    /// Settles YES inside `[floor_strike, cap_strike]`.
    Between,
}

/// One tradeable contract, normalised across venues.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../client/src/lib/api/gen/")]
pub struct Market {
    pub venue: Venue,
    /// The contract's identifier at its venue: a Kalshi ticker
    /// (`KXFEDDECISION-26SEP-T3.75`) or a Polymarket market slug. Upper-case at
    /// Kalshi, lower-case at both Polymarkets — the venue decides, not the
    /// caller.
    pub ticker: String,
    pub event_ticker: String,
    pub series_ticker: String,
    pub title: String,
    /// Short label for the YES leg, e.g. `"82.5° or above"`.
    pub yes_sub_title: String,
    pub no_sub_title: String,
    pub status: MarketStatus,
    pub market_type: String,
    /// Dollars, 0..1. `None` when the book side is empty.
    pub yes_bid: Option<f64>,
    pub yes_ask: Option<f64>,
    pub no_bid: Option<f64>,
    pub no_ask: Option<f64>,
    /// Midpoint of the YES book, or last price when the book is one-sided.
    pub mid: Option<f64>,
    pub last_price: Option<f64>,
    pub previous_price: Option<f64>,
    /// `last_price - previous_price`, in dollars.
    pub change: Option<f64>,
    /// Contracts traded, ever.
    ///
    /// `None` means *this venue does not publish the figure*, which is not the
    /// same as zero and must not render as one: Polymarket US's public
    /// catalogue carries no volume at all, and a `0` in that column would read
    /// as a dead market rather than an unanswered question. The same rule
    /// governs [`Market::open_interest`] and [`Market::liquidity`].
    pub volume: Option<f64>,
    pub volume24h: Option<f64>,
    pub open_interest: Option<f64>,
    /// Resting depth, in dollars.
    pub liquidity: Option<f64>,
    pub open_time: String,
    pub close_time: String,
    pub expiration_time: String,
    /// Settlement result once determined: `"yes"`, `"no"`, or `""`.
    pub result: String,
    pub rules_primary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub category: Option<String>,
    /// How this contract's strike relates to the settlement value. `None` on a
    /// plain yes/no market that has no numeric strike at all.
    pub strike_type: Option<StrikeType>,
    /// Lower bound of the YES region, in the underlying's units.
    pub floor_strike: Option<f64>,
    /// Upper bound of the YES region, in the underlying's units.
    pub cap_strike: Option<f64>,
}

/// One question, with every contract that resolves it.
///
/// All three venues group markets this way — Kalshi calls it an event, both
/// Polymarkets call it an event too — so it is the unit the terminal compares
/// across brokers.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../client/src/lib/api/gen/")]
pub struct VenueEvent {
    pub venue: Venue,
    pub event_ticker: String,
    pub series_ticker: String,
    pub title: String,
    pub sub_title: String,
    pub category: String,
    pub mutually_exclusive: bool,
    pub markets: Vec<Market>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../client/src/lib/api/gen/")]
pub struct MarketsResponse {
    pub markets: Vec<Market>,
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../client/src/lib/api/gen/")]
pub struct EventsResponse {
    pub events: Vec<VenueEvent>,
    pub cursor: Option<String>,
}

/// One row of the resting order book, price ascending.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../client/src/lib/api/gen/")]
pub struct BookLevel {
    pub price: f64,
    pub size: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../client/src/lib/api/gen/")]
pub struct OrderBook {
    pub venue: Venue,
    pub ticker: String,
    /// Resting YES bids, best (highest) first.
    pub yes: Vec<BookLevel>,
    /// Resting NO bids, best (highest) first.
    pub no: Vec<BookLevel>,
    /// The NO book expressed in YES terms (`1 - noPrice`), best (lowest) first —
    /// i.e. the YES ask ladder. Derived server-side so the client never has to
    /// remember the inversion.
    pub yes_asks: Vec<BookLevel>,
    pub best_yes_bid: Option<f64>,
    pub best_yes_ask: Option<f64>,
    pub spread: Option<f64>,
    pub mid: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../client/src/lib/api/gen/")]
pub struct Trade {
    pub venue: Venue,
    pub trade_id: String,
    pub ticker: String,
    pub ts: i64,
    pub count: f64,
    pub yes_price: f64,
    pub no_price: f64,
    /// Which side the aggressor lifted.
    pub taker_side: String,
    pub is_block_trade: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../client/src/lib/api/gen/")]
pub struct TradesResponse {
    pub trades: Vec<Trade>,
    pub cursor: Option<String>,
}

/// Candlestick period, in minutes.
///
/// The only values Kalshi accepts, and therefore the grid the whole terminal
/// uses: an implied-price line, a spot chart and a Polymarket price history are
/// only readable against each other if they sit on the same buckets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(into = "u16", try_from = "u16")]
pub enum CandleInterval {
    OneMinute,
    /// The default: an hour of bars reads well over the window most panels open
    /// with, and it is the interval Kalshi's own charts start on.
    #[default]
    OneHour,
    OneDay,
}

impl CandleInterval {
    /// The period in minutes — the number the wire and Kalshi both use.
    pub fn minutes(self) -> u16 {
        match self {
            CandleInterval::OneMinute => 1,
            CandleInterval::OneHour => 60,
            CandleInterval::OneDay => 1440,
        }
    }

    /// The period in seconds.
    pub fn seconds(self) -> i64 {
        i64::from(self.minutes()) * 60
    }

    pub const ALL: &'static [CandleInterval] = &[
        CandleInterval::OneMinute,
        CandleInterval::OneHour,
        CandleInterval::OneDay,
    ];
}

impl From<CandleInterval> for u16 {
    fn from(value: CandleInterval) -> Self {
        value.minutes()
    }
}

impl TryFrom<u16> for CandleInterval {
    type Error = String;

    fn try_from(value: u16) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(CandleInterval::OneMinute),
            60 => Ok(CandleInterval::OneHour),
            1440 => Ok(CandleInterval::OneDay),
            other => Err(format!(
                "interval must be 1, 60 or 1440 minutes, got {other}"
            )),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../client/src/lib/api/gen/")]
pub struct Candle {
    /// Period *end*, unix seconds.
    pub time: i64,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    /// Contracts traded in the period. `None` where the venue's history carries
    /// prices only — Polymarket International publishes a price series with no
    /// size attached, and drawing that as a zero volume bar would assert
    /// something the upstream never said.
    pub volume: Option<f64>,
    /// Open interest at the period's end, under the same rule as `volume`.
    pub open_interest: Option<f64>,
    /// False when nothing printed in the period and the candle was carried
    /// forward from the previous close (Kalshi returns only `previous_dollars`
    /// there; Polymarket simply has no sample in the bucket).
    pub traded: bool,
    pub bid: Option<f64>,
    pub ask: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../client/src/lib/api/gen/")]
pub struct CandlesResponse {
    pub venue: Venue,
    pub ticker: String,
    pub series_ticker: String,
    #[ts(type = "1 | 60 | 1440")]
    pub interval: CandleInterval,
    pub candles: Vec<Candle>,
    /// How the bars were obtained, when it is not the venue's own candle feed.
    /// Polymarket International publishes a price *sample* series rather than
    /// OHLC, so its bars are aggregated here and say so instead of implying a
    /// traded high and low that the upstream never stated.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../client/src/lib/api/gen/")]
pub struct SeriesInfo {
    pub venue: Venue,
    pub ticker: String,
    pub title: String,
    pub category: String,
    pub frequency: String,
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../client/src/lib/api/gen/")]
pub struct SeriesListResponse {
    pub series: Vec<SeriesInfo>,
}

/// What a venue's catalogue snapshot currently holds.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../client/src/lib/api/gen/")]
pub struct CatalogueSnapshot {
    pub venue: Venue,
    pub events: usize,
    pub markets: usize,
    /// True when the crawl stopped before the catalogue ran out.
    pub truncated: bool,
    pub age_seconds: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../client/src/lib/api/gen/")]
pub struct SearchResponse {
    pub query: String,
    pub markets: Vec<Market>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../client/src/lib/api/gen/")]
pub struct TopResponse {
    pub venue: Venue,
    pub sort: MoverSort,
    pub markets: Vec<Market>,
}

/* ------------------------------------------------------------- cross-venue */

/// How sure the terminal is that two venues are listing the same question.
///
/// `Linked` is the only band that is *stated* rather than inferred: it comes
/// from the curated table of series identifiers, which is checked against live
/// catalogues. Everything below it is a text match, and is labelled as one, so
/// a trader never mistakes a guess for a fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export, export_to = "../../../client/src/lib/api/gen/")]
pub enum MatchConfidence {
    Linked,
    Strong,
    Likely,
    Weak,
}

impl MatchConfidence {
    pub const ALL: &'static [MatchConfidence] = &[
        MatchConfidence::Linked,
        MatchConfidence::Strong,
        MatchConfidence::Likely,
        MatchConfidence::Weak,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            MatchConfidence::Linked => "linked",
            MatchConfidence::Strong => "strong",
            MatchConfidence::Likely => "likely",
            MatchConfidence::Weak => "weak",
        }
    }
}

/// One venue's listing of a series that at least one other venue also lists.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../client/src/lib/api/gen/")]
pub struct SeriesLeg {
    pub venue: Venue,
    /// Series identifier at that venue: `KXFEDDECISION`, `fomc`, `fed-decision`.
    pub series_ticker: String,
    pub title: String,
    /// Open events in the series.
    pub events: u32,
    /// Open contracts across those events.
    pub markets: u32,
    pub volume24h: Option<f64>,
    /// Soonest close across the series' open events, ISO.
    pub close_time: String,
    /// The event that stands for the series — what a click opens.
    pub sample_event: String,
}

/// A recurring question the terminal believes more than one broker lists.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../client/src/lib/api/gen/")]
pub struct LinkedSeries {
    /// Stable canonical key, e.g. `fed-decision`.
    pub key: String,
    pub title: String,
    /// The venues carrying it, busiest first. Always two or more.
    pub legs: Vec<SeriesLeg>,
    pub confidence: MatchConfidence,
    /// Why these were paired — a curated link, or the terms that matched.
    pub reason: String,
    pub score: f64,
}

/// A venue that did not answer, and why.
///
/// A board covering two of three brokers has to say which one is missing, or a
/// "no match" reads as "no such market".
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../client/src/lib/api/gen/")]
pub struct VenueUnavailable {
    pub venue: Venue,
    pub error: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../client/src/lib/api/gen/")]
pub struct LinkedSeriesResponse {
    pub query: String,
    pub series: Vec<LinkedSeries>,
    /// Series scanned, per venue.
    pub scanned: BTreeMap<String, u32>,
    pub unavailable: Vec<VenueUnavailable>,
    pub snapshot_age_seconds: f64,
}

/// One venue's quote for a contract in a side-by-side comparison.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../client/src/lib/api/gen/")]
pub struct CompareLeg {
    pub venue: Venue,
    pub ticker: String,
    pub event_ticker: String,
    pub yes_bid: Option<f64>,
    pub yes_ask: Option<f64>,
    pub mid: Option<f64>,
    pub volume24h: Option<f64>,
}

/// One outcome, quoted at every venue that lists it.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../client/src/lib/api/gen/")]
pub struct CompareRow {
    /// The outcome, as the richest venue words it.
    pub label: String,
    pub legs: Vec<CompareLeg>,
    /// Richest mid minus cheapest mid, in dollars. `None` under two quotes.
    pub divergence: Option<f64>,
    /// Cheapest ask anywhere minus the richest bid anywhere. Positive means the
    /// books are crossed between brokers — buy the ask at one, sell the bid at
    /// the other — before fees, latency and the fact that both legs must fill.
    pub edge: Option<f64>,
    /// The two venues that edge is between, cheap side first.
    pub edge_venues: Vec<Venue>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../client/src/lib/api/gen/")]
pub struct CompareEventLeg {
    pub venue: Venue,
    pub event_ticker: String,
    pub title: String,
    pub close_time: String,
    /// Confidence that this event is the same question as the first leg's.
    pub confidence: MatchConfidence,
    pub score: f64,
    pub reason: String,
}

/// A contract only one venue lists.
///
/// Named rather than dropped — a ladder that is finer at one broker is
/// information, not noise.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../client/src/lib/api/gen/")]
pub struct CompareUnmatched {
    pub venue: Venue,
    pub label: String,
    pub ticker: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../client/src/lib/api/gen/")]
pub struct CompareResponse {
    pub title: String,
    /// The events being compared, the anchor first.
    pub events: Vec<CompareEventLeg>,
    pub rows: Vec<CompareRow>,
    pub unmatched: Vec<CompareUnmatched>,
}

/* -------------------------------------------------------------------- spot */

/// Which upstream family a symbol is priced from.
///
/// This is a routing decision, not a taxonomy: `Stock` covers equities, ETFs and
/// cash indices, because they all come from the same equity feed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export, export_to = "../../../client/src/lib/api/gen/")]
pub enum AssetClass {
    Stock,
    Crypto,
}

impl AssetClass {
    pub fn as_str(self) -> &'static str {
        match self {
            AssetClass::Stock => "stock",
            AssetClass::Crypto => "crypto",
        }
    }
}

/// Live quote for an underlying. Prices are in the instrument's own currency.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../client/src/lib/api/gen/")]
pub struct SpotQuote {
    pub symbol: String,
    pub asset_class: AssetClass,
    pub name: String,
    pub currency: String,
    pub price: Option<f64>,
    /// Previous session's close for equities, price 24h ago for crypto.
    pub previous_close: Option<f64>,
    pub change: Option<f64>,
    pub change_percent: Option<f64>,
    pub day_open: Option<f64>,
    pub day_high: Option<f64>,
    pub day_low: Option<f64>,
    pub volume: Option<f64>,
    /// Exchange or venue the print came from, e.g. `NASDAQ`, `Coinbase`.
    pub venue: String,
    /// When the price was observed, unix seconds.
    pub time: i64,
    /// Which provider answered — surfaced in the panel header.
    pub source: String,
}

/// One bar of an underlying's price history.
///
/// Deliberately shares [`CandleInterval`] with Kalshi: an implied-price line is
/// only readable against the true price if both sit on the same buckets.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../client/src/lib/api/gen/")]
pub struct SpotCandle {
    /// Period *start*, unix seconds — the convention both upstreams use.
    pub time: i64,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../client/src/lib/api/gen/")]
pub struct SpotCandlesResponse {
    pub symbol: String,
    pub asset_class: AssetClass,
    pub name: String,
    pub currency: String,
    #[ts(type = "1 | 60 | 1440")]
    pub interval: CandleInterval,
    pub candles: Vec<SpotCandle>,
    pub source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../client/src/lib/api/gen/")]
pub struct SpotSearchResult {
    pub symbol: String,
    pub name: String,
    pub asset_class: AssetClass,
    pub venue: String,
    /// True when the terminal knows a Kalshi ladder that prices this symbol.
    pub has_implied: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../client/src/lib/api/gen/")]
pub struct SpotSearchResponse {
    pub query: String,
    pub results: Vec<SpotSearchResult>,
}

/* ----------------------------------------------------------------- implied */

/// How a strike ladder is collapsed into one number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export, export_to = "../../../client/src/lib/api/gen/")]
pub enum ImpliedMethod {
    #[default]
    Median,
    Mean,
}

impl ImpliedMethod {
    pub fn as_str(self) -> &'static str {
        match self {
            ImpliedMethod::Median => "median",
            ImpliedMethod::Mean => "mean",
        }
    }
}

/// An underlying the terminal knows how to price from a Kalshi ladder.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../client/src/lib/api/gen/")]
pub struct ImpliedUnderlying {
    pub symbol: String,
    pub name: String,
    pub asset_class: AssetClass,
    pub aliases: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../client/src/lib/api/gen/")]
pub struct ImpliedUnderlyingsResponse {
    pub underlyings: Vec<ImpliedUnderlying>,
}

/// A Kalshi ladder that prices an underlying, as offered in the picker.
///
/// One candidate is one *event* — a single expiry with its full strike ladder.
/// A single strike cannot imply a price; the ladder is the unit of choice.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../client/src/lib/api/gen/")]
pub struct ImpliedCandidate {
    pub event_ticker: String,
    pub series_ticker: String,
    pub title: String,
    pub sub_title: String,
    /// When the ladder settles, ISO 8601. Empty when Kalshi does not state one.
    pub strike_date: String,
    /// Contracts in the ladder with a usable numeric strike.
    pub strikes: u32,
    /// How many of those are quoted (a two-sided book or a last print).
    pub quoted: u32,
    /// Summed 24h volume across the ladder, in contracts.
    pub volume24h: f64,
    /// Lowest and highest strike, so the picker can show the covered range.
    pub strike_low: Option<f64>,
    pub strike_high: Option<f64>,
    /// Implied price from the live book right now, or `None` if underivable.
    pub implied: Option<f64>,
    /// Probability mass sitting outside the quoted strike range.
    pub tail_mass: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../client/src/lib/api/gen/")]
pub struct ImpliedCandidatesResponse {
    pub symbol: String,
    pub asset_class: AssetClass,
    pub name: String,
    pub candidates: Vec<ImpliedCandidate>,
    /// Why the list is empty, when it is. "No Kalshi ladder prices this symbol"
    /// is an answer, not a failure — the panel shows this instead of an error.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../client/src/lib/api/gen/")]
pub struct ImpliedPoint {
    /// Bucket end, unix seconds — aligned to the Kalshi candle grid.
    pub time: i64,
    /// Implied price of the underlying, or `None` when the ladder was unusable.
    pub value: Option<f64>,
    /// Strikes that were quoted in this bucket.
    pub strikes: u32,
    /// Probability mass outside the quoted strike range in this bucket.
    pub tail_mass: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../client/src/lib/api/gen/")]
pub struct ImpliedSeriesResponse {
    pub event_ticker: String,
    pub title: String,
    pub strike_date: String,
    pub method: ImpliedMethod,
    #[ts(type = "1 | 60 | 1440")]
    pub interval: CandleInterval,
    /// Tickers of the contracts that fed the calculation.
    pub contributors: Vec<String>,
    /// Ladder contracts that were skipped for having no quotes at all.
    pub skipped: u32,
    pub points: Vec<ImpliedPoint>,
}

/* -------------------------------------------------------------------- fred */

/// Which arm of the FRED provider chain answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export, export_to = "../../../client/src/lib/api/gen/")]
pub enum FredSource {
    Scrape,
    Api,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../client/src/lib/api/gen/")]
pub struct FredObservation {
    /// `YYYY-MM-DD`.
    pub date: String,
    /// `None` for FRED's `.` missing-value marker.
    pub value: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../client/src/lib/api/gen/")]
pub struct FredSeries {
    pub id: String,
    pub title: String,
    pub units: String,
    pub units_short: String,
    pub frequency: String,
    pub seasonal_adjustment: String,
    pub last_updated: String,
    pub observation_start: String,
    pub observation_end: String,
    pub notes: String,
    /// Where the payload came from — the terminal shows this in the panel header.
    pub source: FredSource,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../client/src/lib/api/gen/")]
pub struct FredSeriesResponse {
    pub series: FredSeries,
    pub observations: Vec<FredObservation>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../client/src/lib/api/gen/")]
pub struct FredSearchResult {
    pub id: String,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub units: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub frequency: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub seasonal_adjustment: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub observation_range: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../client/src/lib/api/gen/")]
pub struct FredSearchResponse {
    pub query: String,
    pub results: Vec<FredSearchResult>,
    pub source: FredSource,
}

/* --------------------------------------------------------------- billboard */

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../client/src/lib/api/gen/")]
pub struct BillboardEntry {
    pub rank: u32,
    pub title: String,
    pub artist: String,
    /// Last week's rank; `None` for a new entry.
    pub last_week: Option<u32>,
    pub peak: Option<u32>,
    pub weeks_on_chart: Option<u32>,
    pub image_url: Option<String>,
    /// `rank` improvement vs `last_week`; positive means the entry moved up.
    #[serde(rename = "move")]
    #[ts(rename = "move")]
    pub movement: Option<i32>,
    pub is_new: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../client/src/lib/api/gen/")]
pub struct BillboardChart {
    /// Slug, e.g. `hot-100`.
    pub chart: String,
    pub title: String,
    /// Chart week, `YYYY-MM-DD`.
    pub date: String,
    pub entries: Vec<BillboardEntry>,
    pub source_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../client/src/lib/api/gen/")]
pub struct BillboardChartListItem {
    pub slug: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../client/src/lib/api/gen/")]
pub struct BillboardChartsResponse {
    pub charts: Vec<BillboardChartListItem>,
}

/* -------------------------------------------------------------------- news */

/// One headline from the news wire. All text is plain — no markup, no escapes.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../client/src/lib/api/gen/")]
pub struct NewsArticle {
    /// Upstream article id, kept as text: it is an identity, never an amount.
    pub id: String,
    pub headline: String,
    /// One-paragraph precis. Empty for a headline-only item, which is normal.
    pub summary: String,
    pub author: String,
    /// Who filed it, e.g. `benzinga`.
    pub publisher: String,
    /// Article link. Empty when the item has none, or its scheme was not http(s).
    pub url: String,
    /// Publication time, unix seconds (UTC).
    pub time: i64,
    /// Last edit, unix seconds. Equal to [`NewsArticle::time`] for an unrevised
    /// item.
    pub updated: i64,
    /// Tickers the publisher tagged, e.g. `["NVDA", "AMD"]`.
    pub symbols: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../client/src/lib/api/gen/")]
pub struct NewsFeed {
    /// Symbols the wire was filtered to; empty means everything.
    pub symbols: Vec<String>,
    /// How many days back the window reaches — the panel states it.
    pub days: u32,
    /// Newest first, by publication time.
    pub articles: Vec<NewsArticle>,
    /// Provider, shown in the panel so the wire is never passed off as ours.
    pub source: String,
    pub source_url: String,
}

/* ---------------------------------------------------- entertainment: kalshi */

/// Genre tags for a Kalshi entertainment event.
///
/// Tags, not a single category, because the clusters genuinely overlap: an Oscar
/// market is both `Film` and `Awards`, and someone typing `ENT film` expects to
/// see it. An event carries every tag that applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export, export_to = "../../../client/src/lib/api/gen/")]
pub enum EntGenre {
    Music,
    Film,
    Tv,
    Games,
    Awards,
    Celeb,
}

impl EntGenre {
    pub const ALL: &'static [EntGenre] = &[
        EntGenre::Music,
        EntGenre::Film,
        EntGenre::Tv,
        EntGenre::Games,
        EntGenre::Awards,
        EntGenre::Celeb,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            EntGenre::Music => "music",
            EntGenre::Film => "film",
            EntGenre::Tv => "tv",
            EntGenre::Games => "games",
            EntGenre::Awards => "awards",
            EntGenre::Celeb => "celeb",
        }
    }
}

impl std::str::FromStr for EntGenre {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        EntGenre::ALL
            .iter()
            .copied()
            .find(|g| g.as_str() == s)
            .ok_or(())
    }
}

/// What `ENT` was asked for: one genre, or the whole book.
///
/// A separate enum from [`EntGenre`] rather than `Option<EntGenre>` because
/// "all" is a value the panel echoes back and renders as a chip, not the absence
/// of a filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export, export_to = "../../../client/src/lib/api/gen/")]
pub enum EntGenreFilter {
    Music,
    Film,
    Tv,
    Games,
    Awards,
    Celeb,
    #[default]
    All,
}

impl EntGenreFilter {
    /// The genre being filtered to, or `None` for the whole book.
    pub fn genre(self) -> Option<EntGenre> {
        match self {
            EntGenreFilter::Music => Some(EntGenre::Music),
            EntGenreFilter::Film => Some(EntGenre::Film),
            EntGenreFilter::Tv => Some(EntGenre::Tv),
            EntGenreFilter::Games => Some(EntGenre::Games),
            EntGenreFilter::Awards => Some(EntGenre::Awards),
            EntGenreFilter::Celeb => Some(EntGenre::Celeb),
            EntGenreFilter::All => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self.genre() {
            Some(genre) => genre.as_str(),
            None => "all",
        }
    }
}

impl From<EntGenre> for EntGenreFilter {
    fn from(genre: EntGenre) -> Self {
        match genre {
            EntGenre::Music => EntGenreFilter::Music,
            EntGenre::Film => EntGenreFilter::Film,
            EntGenre::Tv => EntGenreFilter::Tv,
            EntGenre::Games => EntGenreFilter::Games,
            EntGenre::Awards => EntGenreFilter::Awards,
            EntGenre::Celeb => EntGenreFilter::Celeb,
        }
    }
}

impl std::str::FromStr for EntGenreFilter {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s == "all" {
            return Ok(EntGenreFilter::All);
        }
        s.parse::<EntGenre>().map(EntGenreFilter::from)
    }
}

/// A terminal command that shows the data a market settles against.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../client/src/lib/api/gen/")]
pub struct EntFeed {
    /// Command to run, e.g. `RT dune part three`.
    pub command: String,
    /// Human label for the upstream, e.g. `Rotten Tomatoes`.
    pub source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../client/src/lib/api/gen/")]
pub struct EntEvent {
    pub event_ticker: String,
    pub series_ticker: String,
    pub title: String,
    pub sub_title: String,
    pub genres: Vec<EntGenre>,
    /// Markets in the event, most liquid first.
    pub markets: Vec<Market>,
    pub volume24h: f64,
    pub open_interest: f64,
    /// Soonest close across the event's markets, ISO — the clock that matters.
    pub close_time: String,
    /// The data feed Kalshi settles this series against, when we know of one.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub feed: Option<EntFeed>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../client/src/lib/api/gen/")]
pub struct EntResponse {
    pub genre: EntGenreFilter,
    pub events: Vec<EntEvent>,
    /// Events scanned in the snapshot.
    pub scanned: u32,
    pub snapshot_age_seconds: f64,
    /// Per-genre counts, so the panel can show what else is available.
    pub counts: BTreeMap<String, u32>,
}

/* ------------------------------------------ entertainment: rotten tomatoes */

#[derive(Debug, Clone, Default, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../client/src/lib/api/gen/")]
pub struct RtScore {
    /// 0..100, or `None` when the score has not been issued yet.
    ///
    /// "No score yet" is never `0` — the distinction is the whole market.
    pub score: Option<u32>,
    /// Average critic/user rating on the source's own scale, e.g. `8.40`.
    pub average_rating: String,
    pub review_count: Option<u32>,
    /// `certified fresh` / `fresh` / `rotten`, as RT states it.
    pub state: String,
    pub certified: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../client/src/lib/api/gen/")]
pub struct RtTitle {
    /// RT url slug, e.g. `dune_part_two`.
    pub slug: String,
    pub title: String,
    pub year: String,
    pub media_type: String,
    /// The Tomatometer — critics.
    pub critics: RtScore,
    /// The Popcornmeter — verified audience.
    pub audience: RtScore,
    pub synopsis: String,
    pub source_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../client/src/lib/api/gen/")]
pub struct RtSearchResult {
    pub slug: String,
    pub title: String,
    pub year: String,
    pub media_type: String,
    pub critics_score: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../client/src/lib/api/gen/")]
pub struct RtSearchResponse {
    pub query: String,
    pub results: Vec<RtSearchResult>,
}

/* --------------------------------------------------- entertainment: netflix */

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../client/src/lib/api/gen/")]
pub struct NetflixEntry {
    pub rank: u32,
    pub title: String,
    /// Season label for TV rows; `""` when Netflix reports `N/A`.
    pub season: String,
    /// Views for the week (global feed only; country feeds are rank-only).
    pub views: Option<f64>,
    pub hours_viewed: Option<f64>,
    /// Runtime in hours, as Netflix publishes it.
    pub runtime: Option<f64>,
    pub weeks_in_top10: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../client/src/lib/api/gen/")]
pub struct NetflixTop10 {
    /// `global`, or an ISO-3166 alpha-2 country code.
    pub scope: String,
    pub scope_label: String,
    /// `tv` or `films`.
    pub category: String,
    pub category_label: String,
    /// Week ending, `YYYY-MM-DD`.
    pub week: String,
    pub entries: Vec<NetflixEntry>,
    pub source_url: String,
}

/* ------------------------------------------- entertainment: spotify/youtube */

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../client/src/lib/api/gen/")]
pub struct StreamEntry {
    pub rank: u32,
    /// Previous position; `None` for a debut.
    pub last_rank: Option<u32>,
    pub title: String,
    pub artist: String,
    /// Streams or views for the period.
    pub streams: Option<f64>,
    /// Change vs the previous period.
    pub streams_change: Option<f64>,
    /// Cumulative total since entering the chart.
    pub total: Option<f64>,
    pub peak: Option<u32>,
    pub days: Option<u32>,
    #[serde(rename = "move")]
    #[ts(rename = "move")]
    pub movement: Option<i32>,
    pub is_new: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../client/src/lib/api/gen/")]
pub struct StreamChart {
    /// `spotify` or `youtube`.
    pub source: String,
    /// Chart identifier, e.g. `us-daily`.
    pub chart: String,
    pub title: String,
    /// Date the chart covers, when the page states one.
    pub date: String,
    pub entries: Vec<StreamEntry>,
    pub source_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../client/src/lib/api/gen/")]
pub struct StreamChartListItem {
    pub slug: String,
    pub name: String,
    pub source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../client/src/lib/api/gen/")]
pub struct StreamChartsResponse {
    pub charts: Vec<StreamChartListItem>,
}

/* ------------------------------------------------ entertainment: box office */

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../client/src/lib/api/gen/")]
pub struct BoxOfficeEntry {
    pub rank: u32,
    /// Yesterday's rank; `None` when absent.
    pub last_rank: Option<u32>,
    pub title: String,
    /// Gross for the day, in whole dollars.
    pub gross: Option<f64>,
    /// Percent change vs the previous day.
    pub change_day: Option<f64>,
    /// Percent change vs the same day last week.
    pub change_week: Option<f64>,
    pub theaters: Option<u32>,
    /// Per-theatre average, in dollars.
    pub average: Option<f64>,
    pub total_gross: Option<f64>,
    pub days_in_release: Option<u32>,
    pub distributor: String,
    #[serde(rename = "move")]
    #[ts(rename = "move")]
    pub movement: Option<i32>,
    pub is_new: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../client/src/lib/api/gen/")]
pub struct BoxOfficeDay {
    /// `YYYY-MM-DD`.
    pub date: String,
    pub title: String,
    pub entries: Vec<BoxOfficeEntry>,
    /// Summed gross across the chart, in dollars.
    pub total_gross: f64,
    pub source_url: String,
}

/* ----------------------------------------------------- entertainment: steam */

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../client/src/lib/api/gen/")]
pub struct SteamGame {
    pub app_id: u32,
    pub name: String,
    pub rank: Option<u32>,
    /// Players in game right now.
    pub current_players: Option<u64>,
    /// Highest concurrent players in the last 24h.
    pub peak_players: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../client/src/lib/api/gen/")]
pub struct SteamChart {
    /// `top` for the most-played leaderboard, or `game` for a single title.
    pub view: String,
    pub games: Vec<SteamGame>,
    pub source_url: String,
}

/* -------------------------------------------------- entertainment: tv guide */

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../client/src/lib/api/gen/")]
pub struct TvEpisode {
    /// `HH:MM`, network local time.
    pub airtime: String,
    pub show: String,
    pub network: String,
    pub season: Option<u32>,
    pub episode: Option<u32>,
    pub name: String,
    /// Minutes, when the schedule states one.
    pub runtime: Option<u32>,
    #[serde(rename = "type")]
    #[ts(rename = "type")]
    pub kind: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../client/src/lib/api/gen/")]
pub struct TvSchedule {
    /// `YYYY-MM-DD`.
    pub date: String,
    pub country: String,
    pub episodes: Vec<TvEpisode>,
    pub source_url: String,
}

/* ------------------------------------------------------------------ health */

#[derive(Debug, Clone, Copy, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../client/src/lib/api/gen/")]
pub struct CacheStats {
    pub hits: u64,
    pub misses: u64,
    pub entries: u64,
    pub evictions: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../client/src/lib/api/gen/")]
pub struct HealthResponse {
    pub ok: bool,
    pub uptime_seconds: f64,
    pub cache: CacheStats,
    pub fred_api_key: bool,
    pub alpaca_keys: bool,
    pub time: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candle_interval_is_only_the_three_kalshi_periods() {
        assert_eq!(
            CandleInterval::try_from(1u16),
            Ok(CandleInterval::OneMinute)
        );
        assert_eq!(CandleInterval::try_from(60u16), Ok(CandleInterval::OneHour));
        assert_eq!(
            CandleInterval::try_from(1440u16),
            Ok(CandleInterval::OneDay)
        );
        assert!(CandleInterval::try_from(5u16).is_err());
        assert!(CandleInterval::try_from(0u16).is_err());
    }

    #[test]
    fn candle_interval_serialises_as_its_minute_count() {
        let json = serde_json::to_string(&CandleInterval::OneDay).unwrap();
        assert_eq!(json, "1440");
        let back: CandleInterval = serde_json::from_str("60").unwrap();
        assert_eq!(back, CandleInterval::OneHour);
    }

    #[test]
    fn candle_interval_seconds_match_its_minutes() {
        assert_eq!(CandleInterval::OneMinute.seconds(), 60);
        assert_eq!(CandleInterval::OneHour.seconds(), 3_600);
        assert_eq!(CandleInterval::OneDay.seconds(), 86_400);
    }

    #[test]
    fn a_null_figure_stays_null_across_the_wire() {
        // Polymarket US publishes no volume; `null` must not become `0`.
        let json = r#"{"volume":null,"volume24h":null,"openInterest":null}"#;
        #[derive(Deserialize, Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Probe {
            volume: Option<f64>,
            volume24h: Option<f64>,
            open_interest: Option<f64>,
        }
        let probe: Probe = serde_json::from_str(json).unwrap();
        assert!(probe.volume.is_none());
        assert!(probe.volume24h.is_none());
        assert!(probe.open_interest.is_none());
        assert_eq!(serde_json::to_string(&probe).unwrap(), json);
    }

    #[test]
    fn venue_serialises_as_its_wire_identifier() {
        assert_eq!(serde_json::to_string(&Venue::Kalshi).unwrap(), "\"kalshi\"");
        assert_eq!(
            serde_json::to_string(&Venue::PolymarketUs).unwrap(),
            "\"polymarket-us\""
        );
    }

    #[test]
    fn strike_type_uses_the_snake_case_names_the_client_expects() {
        assert_eq!(
            serde_json::to_string(&StrikeType::GreaterOrEqual).unwrap(),
            "\"greater_or_equal\""
        );
        assert_eq!(
            serde_json::to_string(&StrikeType::Between).unwrap(),
            "\"between\""
        );
    }

    #[test]
    fn confidence_bands_order_from_stated_to_guessed() {
        assert!(MatchConfidence::Linked < MatchConfidence::Strong);
        assert!(MatchConfidence::Strong < MatchConfidence::Likely);
        assert!(MatchConfidence::Likely < MatchConfidence::Weak);
        assert_eq!(MatchConfidence::ALL.len(), 4);
    }

    #[test]
    fn ent_genres_round_trip_through_their_wire_names() {
        for genre in EntGenre::ALL {
            let text = genre.as_str();
            assert_eq!(text.parse::<EntGenre>(), Ok(*genre));
            assert_eq!(serde_json::to_string(genre).unwrap(), format!("\"{text}\""));
        }
    }

    #[test]
    fn an_optional_hint_is_omitted_rather_than_sent_as_null() {
        let err = ApiError {
            error: "No such market".into(),
            code: "not_found".into(),
            hint: None,
            status: None,
        };
        assert_eq!(
            serde_json::to_string(&err).unwrap(),
            r#"{"error":"No such market","code":"not_found"}"#
        );
    }
}
