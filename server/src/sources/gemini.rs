//! Gemini prediction markets (gemini.com/prediction-markets) client.
//!
//! The exchange runs its prediction book on the same matching engine as its
//! crypto pairs, and it shows in the API: everything the terminal needs is
//! public, unauthenticated and split across two hosts that speak different
//! dialects.
//!
//! ```text
//! www.gemini.com/prediction-markets   catalogue and single event. Names,
//!                                     rules, strikes, settlement, turnover.
//!                                     Every figure is a decimal *string*.
//! api.gemini.com                      book, tape and OHLC bars, keyed on the
//!                                     instrument symbol. Strings on the v1
//!                                     paths, plain numbers on the v2 ones.
//! ```
//!
//! Two identifiers, both upper-case and both case-insensitive upstream. An
//! event is its ticker (`DEMNOM2028`, `NFL-2608212330-CAR-JAX-M`); a contract is
//! its `instrumentSymbol` (`GEMI-DEMNOM2028-DEMNOM28AOC`), which is the only
//! thing api.gemini.com answers to and the only field that names a leg
//! uniquely.
//!
//! What the crawl costs is one request: `?limit=500&status=active` returns the
//! whole open universe — 480 events and 3,481 contracts, 12 MB decoded but a
//! fraction of that compressed — so the corpus is a single call rather than the
//! paginated per-category walk the other brokers need.
//!
//! What the exchange never states is turnover per contract, open interest
//! anywhere, and any resting-depth figure outside the book itself. Those stay
//! `None`, and the three boards that would have ranked on them are empty by
//! declaration in the venue registry rather than by accident here.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::Value;
use terminal_core::types::{
    BookLevel, Candle, CandleInterval, CandlesResponse, Market, MarketStatus, OrderBook,
    SeriesInfo, StrikeType, Trade, TradesResponse, Venue, VenueEvent,
};
use terminal_core::util::round4;
use terminal_core::venue::MoverSort;

use crate::app::AppState;
use crate::cache::ttl;
use crate::error::{codes, Result, UpstreamError};
use crate::http::FetchOptions;
use crate::sources::corpus::{refuse_overlong_query, search_corpus, Corpus, SearchResponse};

const VENUE: Venue = Venue::Gemini;

/* --------------------------------------------------------------- coercion */

/// Read a decimal string, or a number that already is one.
///
/// The catalogue and the v1 paths state every figure as a string and the v2
/// paths state the same figures as numbers, so both spellings have to land on
/// the same `f64` — otherwise a bar's close and a book's price would compare
/// unequal for no reason a reader could see.
pub fn num(value: &Value) -> Option<f64> {
    match value {
        Value::Null => None,
        Value::String(text) => {
            if text.is_empty() {
                return None;
            }
            js_number(text)
        }
        Value::Number(number) => number.as_f64().filter(|n| n.is_finite()),
        _ => None,
    }
}

/// [`num`] over a field the upstream may omit altogether.
fn num_of(value: Option<&Value>) -> Option<f64> {
    value.and_then(num)
}

/// JavaScript's `Number(string)`: surrounding whitespace is not a value, and a
/// string of nothing but whitespace is zero. Label debris like `"CKHEE."` is
/// `NaN` there and `None` here.
fn js_number(text: &str) -> Option<f64> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Some(0.0);
    }
    trimmed.parse::<f64>().ok().filter(|n| n.is_finite())
}

/// As [`num`], but a price of zero is the *absence* of one.
///
/// `priceMinimum` is $0.01 on every contract and the tick is a cent, so nothing
/// can rest at zero. A zero in a price field is an empty side wearing a
/// number's clothes, and it has to reach the panel as `--` rather than as a
/// contract someone will sell you for nothing.
fn quote(value: Option<&Value>) -> Option<f64> {
    num_of(value).filter(|n| *n != 0.0)
}

/* ------------------------------------------------------------ raw upstream */

/// How every contract states its rules: a Contentful rich-text document.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RawGeminiRichText {
    pub content: Option<Vec<RawGeminiRichTextNode>>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RawGeminiRichTextNode {
    pub value: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RawGeminiSide {
    pub yes: Option<Value>,
    pub no: Option<Value>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RawGeminiPrices {
    /// Present on every contract, and deliberately unread — see
    /// [`normalise_market`]. Where a book exists these restate it; where none
    /// does they hold an indicative mark instead.
    pub buy: Option<RawGeminiSide>,
    pub sell: Option<RawGeminiSide>,
    /// Absent, key and all, on roughly half the open contracts.
    pub best_bid: Option<Value>,
    pub best_ask: Option<Value>,
    /// Absent on the legs that have never traded, and on every settled one.
    pub last_trade_price: Option<Value>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RawGeminiStrike {
    /// `above` | `under_or_equal` | `below` | `reference`.
    pub r#type: Option<String>,
    /// A decimal string on 601 of 629 strikes, and label debris on the other 28.
    pub value: Option<Value>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RawGeminiContract {
    pub id: Option<String>,
    pub label: Option<String>,
    pub abbreviated_name: Option<String>,
    pub ticker: Option<String>,
    /// The authoritative identifier. Never rebuild it from the two tickers.
    pub instrument_symbol: Option<String>,
    pub description: Option<RawGeminiRichText>,
    pub prices: Option<RawGeminiPrices>,
    pub status: Option<String>,
    pub market_state: Option<String>,
    pub effective_date: Option<String>,
    pub expiry_date: Option<String>,
    /// A percentage of the older price, not a dollar delta.
    pub price_delta24h_pct: Option<Value>,
    pub price_delta1h_pct: Option<Value>,
    /// `yes` or `no`, on exactly the settled contracts.
    pub resolution_side: Option<String>,
    pub strike: Option<RawGeminiStrike>,
    /// The venue's own display order for the leg — see [`normalise_event`].
    pub sort_order: Option<i64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RawGeminiSubcategory {
    pub slug: Option<String>,
    pub name: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RawGeminiEvent {
    pub id: Option<String>,
    pub ticker: Option<String>,
    pub slug: Option<String>,
    pub title: Option<String>,
    /// `categorical` or `binary`. Not an exclusivity flag — see
    /// [`normalise_event`].
    pub r#type: Option<String>,
    pub category: Option<String>,
    pub subcategory: Option<RawGeminiSubcategory>,
    pub status: Option<String>,
    /// Stated on 83 of 480 open events, and authoritative where it is.
    pub series: Option<String>,
    pub tags: Option<Vec<String>>,
    /// Contracts traded, ever and in the last 24h. Stated per event only.
    pub volume: Option<Value>,
    pub volume24h: Option<Value>,
    pub volume_delta24h_pct: Option<Value>,
    pub effective_date: Option<String>,
    pub expiry_date: Option<String>,
    /// Start of the observed period, not of trading — see [`normalise_market`].
    pub start_time: Option<String>,
    pub resolved_at: Option<String>,
    pub contracts: Option<Vec<RawGeminiContract>>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RawGeminiCatalogue {
    pub data: Option<Vec<RawGeminiEvent>>,
    pub pagination: Option<RawGeminiPagination>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RawGeminiPagination {
    pub limit: Option<usize>,
    pub offset: Option<usize>,
    pub total: Option<usize>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RawGeminiLevel {
    pub price: Option<Value>,
    /// Contracts, and fractional on the instruments that trade in hundredths.
    pub amount: Option<Value>,
    /// The snapshot time, repeated on every level rather than a per-level age.
    pub timestamp: Option<Value>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RawGeminiBook {
    pub bids: Option<Vec<RawGeminiLevel>>,
    pub asks: Option<Vec<RawGeminiLevel>>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RawGeminiTrade {
    /// Unix seconds. `timestampms` is the same instant in milliseconds.
    pub timestamp: Option<Value>,
    pub timestampms: Option<Value>,
    pub tid: Option<Value>,
    pub price: Option<Value>,
    pub amount: Option<Value>,
    /// `buy` or `sell`. The payload says nothing further about what it means.
    pub r#type: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RawGeminiTicker {
    pub symbol: Option<String>,
    /// The price 24h ago, and the current one.
    pub open: Option<Value>,
    pub close: Option<Value>,
    pub high: Option<Value>,
    pub low: Option<Value>,
    /// 24 hourly closes of disputed order — see [`apply_ticker`].
    pub changes: Option<Vec<Value>>,
    pub bid: Option<Value>,
    pub ask: Option<Value>,
}

/// `[periodStartMs, open, high, low, close, volume]`, all plain numbers.
///
/// Kept as a `Value` row rather than a fixed tuple so a short or malformed row
/// is dropped by [`normalise_candles`] instead of failing the whole response —
/// one bad bar must not cost the reader the chart.
pub type RawGeminiKline = Value;

/* ------------------------------------------------------------ normalisers */

/// `active`/`open` → open, `settled`/`closed` → settled.
///
/// A contract's `status` and `marketState` agreed on all 3,481 open rows, so
/// either would serve. Anything the exchange starts saying that is neither — its
/// input validator also names `approved` and `under_review`, and neither has
/// appeared — passes through in the venue's own word rather than being folded
/// into whichever of ours looks closest, because a status nobody has seen is
/// worth reading literally.
pub fn status_of(raw: &RawGeminiContract) -> MarketStatus {
    let stated = raw
        .status
        .as_deref()
        .or(raw.market_state.as_deref())
        .unwrap_or_default()
        .to_lowercase();

    match stated.as_str() {
        "active" | "open" => "open".into(),
        "settled" | "closed" => "settled".into(),
        "" => "open".into(),
        _ => stated,
    }
}

/// The rules text, which arrives as a document rather than a string.
///
/// Every contract states its terms as a Contentful rich-text tree and not one
/// states them as prose, so there is no string spelling to prefer: without
/// flattening, every rules pane in the terminal reads `[object Object]`.
pub fn flatten_rules(raw: Option<&RawGeminiRichText>) -> String {
    raw.and_then(|doc| doc.content.as_ref())
        .into_iter()
        .flatten()
        .filter_map(|node| node.value.as_deref())
        .collect()
}

/// A contract's numeric bound, as the terminal states one.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Strike {
    pub strike_type: Option<StrikeType>,
    pub floor_strike: Option<f64>,
    pub cap_strike: Option<f64>,
}

/// The structured strike, when the number in it is a number.
///
/// 629 open contracts carry a `{type, value}` strike and 28 of them carry
/// debris from whatever generated the label instead of a figure — "Lockheed
/// Martin" arrives as `{type:"below", value:"CKHEE."}` and "Hike 25bps" as
/// `{type:"above", value:"KE25"}`. So the value is parsed before the type is
/// believed, and a strike that fails to parse leaves the contract with none: a
/// `NaN` bound would sort to one end of every ladder it appeared in and price a
/// spread against nothing.
///
/// `above` and `below` are the exchange's shorthand for bounds that include
/// their own edge, which the labels and the rules text agree on: every numeric
/// `above` strike is a "$61,000 or above" rung whose terms read "if the price of
/// Bitcoin is $61,000 or above … this market will resolve to Yes", and every
/// numeric `below` one is a "76°F or below" weather bucket worded the same way.
/// Both are therefore the inclusive bound, the same one `under_or_equal` states
/// for a golf top-10 finish. Reading them as strict would put the settlement
/// value itself on the losing side of a rung that pays on it, and the two halves
/// of a ladder would then leave a gap at every tick. `reference` is the level a
/// five-minute up/down book is quoted against rather than a bound anything
/// settles over, so it is no strike at all.
pub fn strike_of(raw: Option<&RawGeminiStrike>) -> Strike {
    let Some(value) = num_of(raw.and_then(|s| s.value.as_ref())) else {
        return Strike::default();
    };

    match raw.and_then(|s| s.r#type.as_deref()) {
        Some("above") => Strike {
            strike_type: Some(StrikeType::GreaterOrEqual),
            floor_strike: Some(value),
            cap_strike: None,
        },
        Some("under_or_equal" | "below") => Strike {
            strike_type: Some(StrikeType::LessOrEqual),
            floor_strike: None,
            cap_strike: Some(value),
        },
        _ => Strike::default(),
    }
}

/// The price 24h ago, recovered from the percentage the venue states instead.
///
/// `priceDelta24hPct` is a percentage *of the older price*, so assigning it to
/// `change` would print a ten-dollar move on a 22¢ contract that ticked up two
/// cents. Inverting it gives the older price, and the subtraction gives the move
/// in dollars. The settled contracts state no percentage at all; they get no
/// previous price rather than a flat one, so `change` reads `--` instead of
/// `0.00`.
pub fn previous_from(last_price: Option<f64>, delta_pct: Option<&Value>) -> Option<f64> {
    let last = last_price?;
    let pct = num_of(delta_pct)?;

    let factor = 1.0 + pct / 100.0;
    // A contract that lost all of its value states no older price this can
    // reach; the division would chart as a spike to infinity.
    if factor == 0.0 {
        return None;
    }
    Some(round4(last / factor))
}

/// What a leg inherits from the event that owns it.
#[derive(Debug, Clone, Default)]
pub struct GeminiParent {
    pub event_ticker: String,
    pub series_ticker: String,
    pub title: String,
    pub category: String,
    pub market_type: String,
    pub open_time: String,
    pub close_time: String,
    /// The event's turnover, and only when the event *is* a single contract.
    /// [`normalise_event`] decides; the reasoning is there.
    pub volume: Option<f64>,
    pub volume24h: Option<f64>,
}

/// One leg of an event, quoted from `bestBid` and `bestAsk`.
///
/// **`prices.buy` and `prices.sell` are not read, on purpose.** Where a book
/// exists they restate it exactly — `buy.yes` equalled `bestAsk` on all 1,785
/// open contracts carrying both, with no exceptions, and `sell.no` equalled
/// `1 - bestAsk` to the cent. But on the 501 contracts with nothing resting they
/// hold an indicative mark instead, with `buy.yes == sell.yes ==
/// lastTradePrice`. Reading them would put a two-sided quote at a zero spread on
/// a contract nobody is quoting, which is the one thing a book panel must never
/// do. `bestBid`/`bestAsk` are simply absent on those rows, and absent is the
/// truth.
///
/// The NO side is derived rather than read for the same reason: there is one
/// instrument per leg, quoted in YES terms, and the venue's own `buy.no` and
/// `sell.no` are that mirror already.
pub fn normalise_market(raw: &RawGeminiContract, parent: &GeminiParent) -> Market {
    let prices = raw.prices.clone().unwrap_or_default();
    let yes_bid = quote(prices.best_bid.as_ref());
    let yes_ask = quote(prices.best_ask.as_ref());
    let last_price = quote(prices.last_trade_price.as_ref());
    let previous_price = previous_from(last_price, raw.price_delta24h_pct.as_ref());
    let strike = strike_of(raw.strike.as_ref());

    Market {
        venue: VENUE,
        ticker: raw.instrument_symbol.clone().unwrap_or_default(),
        event_ticker: parent.event_ticker.clone(),
        series_ticker: parent.series_ticker.clone(),
        title: parent.title.clone(),
        yes_sub_title: raw
            .label
            .clone()
            .or_else(|| raw.abbreviated_name.clone())
            .or_else(|| raw.ticker.clone())
            .unwrap_or_default(),
        // The exchange words one side of a leg and never the other, even on the
        // one-contract events where the label is literally "Yes".
        no_sub_title: "No".into(),
        status: status_of(raw),
        market_type: parent.market_type.clone(),
        yes_bid,
        yes_ask,
        no_bid: yes_ask.map(|ask| round4(1.0 - ask)),
        no_ask: yes_bid.map(|bid| round4(1.0 - bid)),
        mid: match (yes_bid, yes_ask) {
            (Some(bid), Some(ask)) => Some(round4((bid + ask) / 2.0)),
            _ => last_price.or(yes_bid).or(yes_ask),
        },
        last_price,
        previous_price,
        change: match (last_price, previous_price) {
            (Some(last), Some(previous)) => Some(round4(last - previous)),
            _ => None,
        },
        volume: parent.volume,
        volume24h: parent.volume24h,
        // No endpoint on either host carries open interest — not the event, the
        // contract, the book, the ticker, the tape or a candle row.
        open_interest: None,
        // Not published either, but the book states it for anyone who asks for
        // the book: [`get_market`] adds it up there rather than across 3,481
        // crawls.
        liquidity: None,
        // `effectiveDate` is when this leg opened. The event's `startTime` names
        // the start of the period being *observed* — kickoff, or the first tick
        // of a price window — which on a Sunday NFL game is days after the book
        // opened, so reading it here would say the contract had not started
        // trading for most of the time it traded.
        open_time: raw
            .effective_date
            .clone()
            .unwrap_or_else(|| parent.open_time.clone()),
        close_time: parent.close_time.clone(),
        expiration_time: raw
            .expiry_date
            .clone()
            .unwrap_or_else(|| parent.close_time.clone()),
        // Stated per leg rather than one winner per event, and it has to stay
        // that way: a cumulative ladder settles several legs YES, and a
        // head-to-head in the snapshot resolved YES on both.
        result: match raw.resolution_side.as_deref() {
            Some(side @ ("yes" | "no")) => side.to_string(),
            _ => String::new(),
        },
        rules_primary: flatten_rules(raw.description.as_ref()),
        category: Some(parent.category.clone()).filter(|c| !c.is_empty()),
        strike_type: strike.strike_type,
        floor_strike: strike.floor_strike,
        cap_strike: strike.cap_strike,
    }
}

/// Strip the settlement date out of an event ticker.
///
/// Six digits is the load-bearing part of the rule. Tickers embed their date as
/// `YYMMDD` or `YYMMDDHHMM`, so six or more digits in a row is a date and
/// anything shorter is part of the question: a two-or-more threshold would take
/// the year off `SEN26MI` and file all 36 Senate races as one race.
///
/// [`terminal_core::slug::strip_dates`] is the shared rule and is deliberately
/// not used here. It reads a *slug*, whose date occupies whole hyphen-separated
/// segments (`usfed-fomc-2026-10-28`); a Gemini ticker glues its date onto the
/// question with no separator at all (`BTC2608212100`, `WXHIGH-CHI-2608210359`),
/// so there is no segment to drop and the shared rule would return the ticker
/// unchanged.
pub fn strip_event_date(ticker: &str) -> String {
    let mut kept = String::with_capacity(ticker.len());
    let mut digits = String::new();

    for ch in ticker.chars() {
        if ch.is_ascii_digit() {
            digits.push(ch);
            continue;
        }
        if digits.len() < 6 {
            kept.push_str(&digits);
        }
        digits.clear();
        kept.push(ch);
    }
    if digits.len() < 6 {
        kept.push_str(&digits);
    }

    // Removing a date from the middle leaves `--`, and from an end leaves a
    // trailing hyphen; neither belongs in a series name.
    let mut collapsed = String::with_capacity(kept.len());
    let mut previous_dash = false;
    for ch in kept.chars() {
        if ch == '-' && previous_dash {
            continue;
        }
        previous_dash = ch == '-';
        collapsed.push(ch);
    }

    collapsed.trim_matches('-').to_string()
}

/// The series a recurring question belongs to.
///
/// Gemini states one on 83 of 480 open events — its crypto, metals, energy and
/// weather ladders — and those are exactly the events where guessing goes wrong,
/// so a stated series is preferred. `BTC2608202100` says `BTC1H` while
/// `BTC2608312100` says `BTC`: an hourly bitcoin ladder and a monthly one are
/// different products, and stripping the date from both fuses them into one
/// series that would then be matched against a single market at the next broker.
///
/// Everywhere else the ticker carries its own date and removing it leaves the
/// question: `FED260917` and `FED260729` are both `FED`, one series with two
/// open expiries.
///
/// The two disagree in one direction where the stated name loses. Gemini files
/// all seventeen cities' daily highs under the single product `WXHIGH`, but a
/// series here is a recurring *question*, and "the high in Chicago tomorrow" is
/// not the same question as "the high in Miami tomorrow" — grouping them makes
/// sixteen of the seventeen unreachable from `XV`, since one event has to stand
/// for the family. So where the stripped ticker *extends* the stated name, the
/// longer one wins: `WXHIGH-CHI` over `WXHIGH`. Where the stated name extends
/// the stripped one it still wins, which is what keeps `BTC1H` apart from `BTC`.
/// Anything else is a name the ticker does not resemble, and the venue's own is
/// the safer of the two.
pub fn series_from_event(raw: &RawGeminiEvent) -> String {
    let ticker = raw.ticker.clone().unwrap_or_default();
    let stripped = {
        let stripped = strip_event_date(&ticker);
        if stripped.is_empty() {
            ticker.clone()
        } else {
            stripped
        }
    };

    let Some(stated) = raw.series.as_deref().filter(|s| !s.is_empty()) else {
        return stripped;
    };

    if stripped.starts_with(&format!("{stated}-")) {
        stripped
    } else {
        stated.to_string()
    }
}

/// One event with its legs.
///
/// The single-event endpoint and a catalogue row carry the same event shape —
/// the single one adds a `featuredImageUrl` the catalogue omits and nothing else
/// that is read here — so one normaliser serves both paths.
pub fn normalise_event(raw: &RawGeminiEvent) -> VenueEvent {
    let contracts = raw.contracts.clone().unwrap_or_default();
    let event_ticker = raw.ticker.clone().unwrap_or_default();

    // Turnover is stated once per event, in contracts, and never per leg.
    // Copying the event's figure onto each of DEMNOM2028's 12 legs would have
    // `sum_or_null` report 12× the true turnover in the search panel and `TOP`
    // show twelve identical rows, so a leg claims it only when it *is* the
    // event — the 15 one-contract books, where the two figures are one figure.
    let sole = contracts.len() == 1;

    // The array order is not the exchange's order. More than half the events
    // that number every leg disagree with their own numbering, and the
    // disagreement shows: `WXHIGH-CHI` arrives as 79-80°F, 81-82°F, 77-78°F,
    // 76°F or below, 83-84°F, 85°F or above, where its `sortOrder` reads the
    // ladder from cold to hot. A strike ladder out of sequence is unreadable, so
    // an event that numbers all of its legs is read in that order — and one that
    // numbers only some is left alone rather than half-sorted around the gaps.
    let mut ordered = contracts;
    if !ordered.is_empty() && ordered.iter().all(|c| c.sort_order.is_some()) {
        ordered.sort_by_key(|c| c.sort_order.unwrap_or(0));
    }

    let parent = GeminiParent {
        event_ticker: event_ticker.clone(),
        series_ticker: series_from_event(raw),
        title: raw.title.clone().unwrap_or_else(|| event_ticker.clone()),
        category: raw.category.clone().unwrap_or_default(),
        market_type: raw.r#type.clone().unwrap_or_else(|| "binary".into()),
        open_time: raw.effective_date.clone().unwrap_or_default(),
        close_time: raw.expiry_date.clone().unwrap_or_default(),
        volume: sole.then(|| num_of(raw.volume.as_ref())).flatten(),
        volume24h: sole.then(|| num_of(raw.volume24h.as_ref())).flatten(),
    };

    VenueEvent {
        venue: VENUE,
        event_ticker,
        series_ticker: parent.series_ticker.clone(),
        title: parent.title.clone(),
        // The event carries a `shortSummary`, and it is written commentary that
        // is regenerated through the day — a subtitle that changes under the
        // reader.
        sub_title: String::new(),
        category: parent.category.clone(),
        // Never true, and `type` is the trap that makes it look derivable.
        // `categorical` covers both DEMNOM2028, whose 12 legs are a genuine
        // winner-take-all field summing to about a dollar, and BTC2608212100,
        // whose legs are "$61,000 or above", "$61,500 or above", "$62,000 or
        // above" — a nested ladder summing to $6.80, where every leg below the
        // settlement price pays. The prose marker is no better: a third of the
        // events say "This event is mutually exclusive" in their rules and the
        // two-leg Senate control book and every NFL head-to-head are not among
        // them. `EVT` only draws its Σmid arbitrage check when told the legs
        // exclude each other, so a guessed flag puts a false arbitrage on
        // screen.
        mutually_exclusive: false,
        markets: ordered
            .iter()
            .map(|contract| normalise_market(contract, &parent))
            .collect(),
    }
}

/* ---------------------------------------------------------------- queries */

/// A query string, skipping the parameters that were never set.
fn qs(params: &[(&str, String)]) -> String {
    let pairs: Vec<String> = params
        .iter()
        .filter(|(_, value)| !value.is_empty())
        .map(|(key, value)| {
            format!(
                "{}={}",
                urlencoding::encode(key),
                urlencoding::encode(value)
            )
        })
        .collect();

    if pairs.is_empty() {
        String::new()
    } else {
        format!("?{}", pairs.join("&"))
    }
}

/// The catalogue host: names, rules and prices, all as strings.
async fn catalogue<T>(state: &AppState, path: &str, cache_ttl: Duration) -> Result<Arc<T>>
where
    T: DeserializeOwned + Send + Sync + 'static,
{
    let url = format!(
        "{}/prediction-markets{path}",
        state.config().gemini_catalogue_base
    );
    state
        .cache()
        .cached(&format!("gemini:{path}"), cache_ttl, || async {
            state
                .http()
                .fetch_json::<T>(
                    &url,
                    FetchOptions::new()
                        .timeout(Duration::from_secs(30))
                        .retries(2),
                )
                .await
        })
        .await
}

/// The trading host: book, tape and bars, keyed on the instrument symbol.
async fn api<T>(state: &AppState, path: &str, cache_ttl: Duration) -> Result<Arc<T>>
where
    T: DeserializeOwned + Send + Sync + 'static,
{
    let url = format!("{}{path}", state.config().gemini_api_base);
    state
        .cache()
        .cached(&format!("gemini:api:{path}"), cache_ttl, || async {
            state
                .http()
                .fetch_json::<T>(
                    &url,
                    FetchOptions::new()
                        .timeout(Duration::from_secs(20))
                        .retries(2),
                )
                .await
        })
        .await
}

/// What kind of identifier the reader got wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Event,
    Contract,
}

/// The not-found both lookups answer with, worded so the reader can retype it.
///
/// api.gemini.com says no to an unknown symbol with a 400 rather than a 404
/// ("is not a valid symbol"), which would otherwise reach the panel as a
/// transport failure instead of a typo.
fn missing(id: &str, what: Kind) -> UpstreamError {
    let (noun, hint) = match what {
        Kind::Event => (
            "event",
            "Gemini identifies an event by its ticker, as in `gem:DEMNOM2028` or \
             `gem:NFL-2608212330-CAR-JAX-M`. The slug in the web address is not a lookup key, \
             and is not unique — three different events share `btc-price-today-at-2am-edt`.",
        ),
        Kind::Contract => (
            "contract",
            "Gemini identifies a contract by its instrument symbol, as in \
             `gem:GEMI-DEMNOM2028-DEMNOM28AOC`. `EVT` on the parent event lists the symbols \
             of its legs.",
        ),
    };

    UpstreamError::not_found(format!("No Gemini {noun} {id}")).with_hint(hint)
}

/// The trading host said no to a symbol, and the catalogue says which no it was.
///
/// api.gemini.com answers `is not a valid symbol` both for a mistyped symbol and
/// for a contract it has simply never opened an instrument for — every settled
/// leg of `HORMUZNORMAL` among them, all of which the catalogue lists and `EVT`
/// prints. Both arrive as a 400, so the snapshot is consulted before the reader
/// is told to check the spelling of a symbol the terminal handed them a moment
/// ago.
async fn rejected(state: &AppState, symbol: &str) -> UpstreamError {
    let listed = corpus_snapshot(state)
        .await
        .map(|corpus| corpus.markets.iter().any(|m| m.ticker == symbol))
        .unwrap_or(false);

    if !listed {
        return missing(symbol, Kind::Contract);
    }

    UpstreamError::not_found(format!("Gemini has opened no instrument for {symbol}")).with_hint(
        format!(
            "{symbol} is listed in the Gemini catalogue, but api.gemini.com answers \
             `is not a valid symbol` for it, which is what it says about a contract that has \
             never been quoted and never traded. `DES` shows what the catalogue states; the \
             book, tape and bars have nothing to read until the leg opens."
        ),
    )
}

/// Ask api.gemini.com about one instrument, reading its rejection as a typo.
async fn instrument<T>(
    state: &AppState,
    symbol: &str,
    path: &str,
    cache_ttl: Duration,
) -> Result<Arc<T>>
where
    T: DeserializeOwned + Send + Sync + 'static,
{
    match api::<T>(state, path, cache_ttl).await {
        Ok(value) => Ok(value),
        Err(err) if matches!(err.status, Some(400 | 404)) => Err(rejected(state, symbol).await),
        Err(err) => Err(err),
    }
}

/* ----------------------------------------------------------------- lookups */

pub async fn get_event(state: &AppState, id: &str) -> Result<VenueEvent> {
    let path = format!("/{}", urlencoding::encode(id));
    let raw = match catalogue::<RawGeminiEvent>(state, &path, ttl::QUOTE).await {
        Ok(raw) => raw,
        Err(err) if err.code == codes::NOT_FOUND => return Err(missing(id, Kind::Event)),
        Err(err) => return Err(err),
    };

    // A 200 with no ticker in it is the catalogue answering about nothing; the
    // reader mistyped, and that is a not-found rather than a parse failure.
    if raw.ticker.as_deref().unwrap_or_default().is_empty() {
        return Err(missing(id, Kind::Event));
    }
    Ok(normalise_event(&raw))
}

/// Which event owns an instrument symbol.
///
/// The open snapshot answers instantly, and answers nearly every lookup. It
/// cannot answer for a leg of a settled event — the crawl reads `status=active`
/// — and `EVT` on a settled event hands the reader exactly those symbols, so
/// refusing them would make the terminal reject identifiers it had just printed.
/// The symbol's own prefixes are tried against the single-event endpoint
/// instead, longest first: `GEMI-BTC2608171800-HI63700` asks for
/// `BTC2608171800-HI63700`, is told there is no such event, then asks for
/// `BTC2608171800` and is given it. Only the event half of the symbol is ever
/// used, which is why the handful of legs whose symbol does not contain their
/// own contract ticker are found as readily as the rest, and why a hyphenated
/// event ticker costs one wasted request rather than a wrong answer.
///
/// The snapshot is an accelerator, never a gate: a crawl that failed leaves the
/// prefix walk to answer on its own rather than taking the lookup down with it.
async fn event_ticker_for(state: &AppState, symbol: &str) -> Result<String> {
    if let Ok(corpus) = corpus_snapshot(state).await {
        if let Some(known) = corpus.markets.iter().find(|m| m.ticker == symbol) {
            return Ok(known.event_ticker.clone());
        }
    }

    let trimmed = symbol
        .strip_prefix("GEMI-")
        .or_else(|| symbol.strip_prefix("gemi-"))
        .unwrap_or(symbol);
    let parts: Vec<&str> = trimmed.split('-').collect();

    for take in (1..parts.len()).rev() {
        let candidate = parts[..take].join("-");
        let path = format!("/{}", urlencoding::encode(&candidate));
        // Naming no event is the expected answer for every candidate but one.
        if let Ok(raw) = catalogue::<RawGeminiEvent>(state, &path, ttl::QUOTE).await {
            if raw
                .contracts
                .iter()
                .flatten()
                .any(|c| c.instrument_symbol.as_deref() == Some(symbol))
            {
                return Ok(candidate);
            }
        }
    }

    Err(missing(symbol, Kind::Contract))
}

/// One contract, recovered through the event that owns it.
///
/// An instrument symbol is not fetchable on its own: api.gemini.com quotes it
/// but publishes no name, no rules and no parent. So [`event_ticker_for`] finds
/// the owning event, the event is read for live prices, and the leg is picked
/// out of it — one step longer than the other venues' recovery because there is
/// no by-symbol endpoint to start from.
///
/// The two api.gemini.com sidecars are best-effort. The book adds the resting
/// depth nothing in the catalogue states; `/v2/ticker` adds the only 24h open
/// published per contract, and 404s on an instrument that has never traded —
/// a fact about the contract, not a failure. Neither is allowed to take the
/// quote down with it.
pub async fn get_market(state: &AppState, id: &str) -> Result<Market> {
    let event_ticker = event_ticker_for(state, id).await?;
    let event = get_event(state, &event_ticker).await?;
    let Some(leg) = event.markets.iter().find(|m| m.ticker == id) else {
        return Err(missing(id, Kind::Contract));
    };

    let encoded = urlencoding::encode(id);
    let (book, ticker) = futures::future::join(
        api::<RawGeminiBook>(state, &format!("/v1/book/{encoded}"), ttl::QUOTE),
        api::<RawGeminiTicker>(state, &format!("/v2/ticker/{encoded}"), ttl::QUOTE),
    )
    .await;

    let merged = apply_book(leg.clone(), book.ok().as_deref());
    Ok(apply_ticker(merged, ticker.ok().as_deref()))
}

/// Add the resting depth the exchange never states.
///
/// No endpoint publishes a liquidity figure, but the book is public and weighs a
/// few hundred bytes, so the dollars committed to it are added up here: a bid is
/// worth price × size, and an offer is backed by (1 - price) × size, because
/// whoever offers YES at 20¢ has 80¢ of collateral behind every contract. Both
/// halves count, so the number means "dollars resting on this leg" rather than
/// "dollars on one side of it".
///
/// An empty book gives zero, and that is a genuine zero rather than a silence:
/// the exchange was asked and answered that nothing rests. Turnover is the
/// opposite case — never asked, never answered — so it stays `None`.
pub fn apply_book(market: Market, raw: Option<&RawGeminiBook>) -> Market {
    let Some(raw) = raw else {
        return market;
    };

    let (bids, asks) = ladders(raw);
    let depth: f64 = bids
        .iter()
        .map(|level| level.price * level.size)
        .sum::<f64>()
        + asks
            .iter()
            .map(|level| (1.0 - level.price) * level.size)
            .sum::<f64>();

    let yes_bid = bids.first().map(|level| level.price).or(market.yes_bid);
    let yes_ask = asks.first().map(|level| level.price).or(market.yes_ask);

    Market {
        yes_bid,
        yes_ask,
        no_bid: yes_ask.map(|ask| round4(1.0 - ask)),
        no_ask: yes_bid.map(|bid| round4(1.0 - bid)),
        mid: match (yes_bid, yes_ask) {
            (Some(bid), Some(ask)) => Some(round4((bid + ask) / 2.0)),
            _ => market.last_price.or(yes_bid).or(yes_ask),
        },
        liquidity: Some(round4(depth)),
        ..market
    }
}

/// Merge the instrument's own 24h summary.
///
/// `/v2/ticker` restates the top of book and adds an `open` — the price 24h ago
/// — which is the only one the exchange states per contract. It is what gives a
/// move to the legs whose catalogue row carries a 24h percentage but no last
/// print for it to apply to: a percentage of a price nobody states is not a
/// figure, and those legs would otherwise show `--` in the change column while
/// the exchange's own page shows a number.
///
/// Its `changes` array of 24 hourly closes is **not** read. The documented order
/// is newest first and the payload's is not: on every instrument sampled,
/// `changes[0]` equalled `open` and `changes[23]` equalled `close`. A series read
/// the wrong way round draws the day's move backwards, and `open` and `close`
/// state the same two prices without the ambiguity.
pub fn apply_ticker(market: Market, raw: Option<&RawGeminiTicker>) -> Market {
    let Some(raw) = raw else {
        return market;
    };

    let yes_bid = quote(raw.bid.as_ref()).or(market.yes_bid);
    let yes_ask = quote(raw.ask.as_ref()).or(market.yes_ask);
    let last_price = quote(raw.close.as_ref()).or(market.last_price);
    // The catalogue's own percentage is the exchange's arithmetic, so it wins;
    // this only fills the gap where there was none.
    let previous_price = market.previous_price.or_else(|| quote(raw.open.as_ref()));

    Market {
        yes_bid,
        yes_ask,
        no_bid: yes_ask.map(|ask| round4(1.0 - ask)),
        no_ask: yes_bid.map(|bid| round4(1.0 - bid)),
        mid: match (yes_bid, yes_ask) {
            (Some(bid), Some(ask)) => Some(round4((bid + ask) / 2.0)),
            _ => last_price.or(yes_bid).or(yes_ask),
        },
        last_price,
        previous_price,
        change: match (last_price, previous_price) {
            (Some(last), Some(previous)) => Some(round4(last - previous)),
            _ => None,
        },
        ..market
    }
}

/* -------------------------------------------------------------------- book */

/// Both ladders, parsed and sorted: bids best (highest) first, offers cheapest
/// first.
///
/// The exchange already returns them that way; sorting anyway costs nothing on a
/// book this size and means a change of habit upstream cannot quietly invert a
/// ladder that the whole panel reads top-down.
fn ladders(raw: &RawGeminiBook) -> (Vec<BookLevel>, Vec<BookLevel>) {
    let rows = |side: Option<&Vec<RawGeminiLevel>>| -> Vec<BookLevel> {
        side.into_iter()
            .flatten()
            .map(|level| BookLevel {
                price: num_of(level.price.as_ref()).unwrap_or(0.0),
                size: num_of(level.amount.as_ref()).unwrap_or(0.0),
            })
            .filter(|level| level.price > 0.0 && level.size > 0.0)
            .collect()
    };

    let mut bids = rows(raw.bids.as_ref());
    bids.sort_by(|a, b| b.price.total_cmp(&a.price));

    let mut asks = rows(raw.asks.as_ref());
    asks.sort_by(|a, b| a.price.total_cmp(&b.price));

    (bids, asks)
}

/// One book per leg, quoted in YES terms.
///
/// There is no separate NO instrument to fetch, so the NO ladder is the ask side
/// inverted — a resting offer of YES at 20¢ is a bid for NO at 80¢, the same
/// order seen from the other end. A settled instrument answers with both sides
/// empty, which reaches the panel as a book with no top rather than as an error.
pub fn normalise_order_book(raw: &RawGeminiBook, ticker: &str, depth: usize) -> OrderBook {
    let (mut yes, mut yes_asks) = ladders(raw);
    yes.truncate(depth);
    yes_asks.truncate(depth);

    let mut no: Vec<BookLevel> = yes_asks
        .iter()
        .map(|level| BookLevel {
            price: round4(1.0 - level.price),
            size: level.size,
        })
        .collect();
    no.sort_by(|a, b| b.price.total_cmp(&a.price));

    let best_yes_bid = yes.first().map(|level| level.price);
    let best_yes_ask = yes_asks.first().map(|level| level.price);

    OrderBook {
        venue: VENUE,
        ticker: ticker.to_string(),
        yes,
        no,
        yes_asks,
        best_yes_bid,
        best_yes_ask,
        spread: match (best_yes_bid, best_yes_ask) {
            (Some(bid), Some(ask)) => Some(round4(ask - bid)),
            _ => None,
        },
        mid: match (best_yes_bid, best_yes_ask) {
            (Some(bid), Some(ask)) => Some(round4((bid + ask) / 2.0)),
            _ => None,
        },
    }
}

/// The whole ladder, capped here rather than upstream.
///
/// `/v1/book` takes `limit_bids` and `limit_asks`, and they are deliberately not
/// sent: one cached snapshot then serves a quote panel asking for 5 levels and a
/// ladder asking for 50 without a second request, and [`get_market`] needs every
/// level to add the resting depth up.
pub async fn get_order_book(state: &AppState, id: &str, depth: usize) -> Result<OrderBook> {
    let path = format!("/v1/book/{}", urlencoding::encode(id));
    let raw = instrument::<RawGeminiBook>(state, id, &path, ttl::QUOTE).await?;
    Ok(normalise_order_book(&raw, id, depth))
}

/* -------------------------------------------------------------------- tape */

/// The exchange's own ceiling. `limit_trades=1000` is a 400, not a truncation.
const MAX_TRADES: usize = 500;

/// The public print tape, newest first.
///
/// Two things this tape is not, both of which matter more than what it is.
///
/// It is **not a count of anything**. Repeated identical requests for one
/// instrument returned 130, 142 and 164 rows within a minute of each other, and
/// the variants are not prefixes of one another — each replica held prints the
/// others lacked. So nothing here derives a volume or a trade count, and nothing
/// paginates on `tid`: a walk backwards through the ids would skip prints
/// depending on which replica answered.
///
/// It is **not the instrument's lifetime**. It reaches back about three months;
/// the daily bars from `/v2/klines` on the same instrument start earlier and
/// carry contracts that never appear on the tape at all. A backfill has to come
/// from the bars.
///
/// `type` is `buy` or `sell` and the payload states nothing further, so it maps
/// to the aggressor's side as a reading of the field name rather than as a fact
/// the exchange asserted.
pub fn normalise_trades(rows: &[RawGeminiTrade], ticker: &str, limit: usize) -> TradesResponse {
    let mut trades: Vec<Trade> = Vec::new();

    for row in rows {
        if trades.len() >= limit {
            break;
        }

        let yes_price = num_of(row.price.as_ref());
        let count = num_of(row.amount.as_ref());
        let ms = num_of(row.timestampms.as_ref());
        let seconds = num_of(row.timestamp.as_ref()).or(ms.map(|ms| ms / 1000.0));

        // A print with no price, no size or no clock is not a print the tape can
        // show. Defaulting any of the three would put a free trade, an empty one
        // or one stamped 1970 in a column the reader scans for outliers.
        let (Some(yes_price), Some(count), Some(seconds)) = (yes_price, count, seconds) else {
            continue;
        };

        trades.push(Trade {
            venue: VENUE,
            // A 16-digit integer: an identity, never an amount, so it travels as
            // text and never through an f64 that cannot hold it exactly.
            trade_id: match row.tid.as_ref() {
                Some(Value::Number(n)) => n.to_string(),
                Some(Value::String(s)) => s.clone(),
                _ => String::new(),
            },
            ticker: ticker.to_string(),
            ts: seconds.floor() as i64,
            count,
            yes_price,
            no_price: round4(1.0 - yes_price),
            taker_side: if row.r#type.as_deref() == Some("sell") {
                "no".into()
            } else {
                "yes".into()
            },
            // No block or cross flag exists on this venue.
            is_block_trade: false,
        });
    }

    // No cursor: see above — a `since_tid` walk over an inconsistent tape can
    // silently drop prints, so the panel gets one honest page instead.
    TradesResponse {
        trades,
        cursor: None,
    }
}

pub async fn get_trades(state: &AppState, id: &str, limit: usize) -> Result<TradesResponse> {
    let want = limit.clamp(1, MAX_TRADES);
    let path = format!(
        "/v1/trades/{}{}",
        urlencoding::encode(id),
        qs(&[("limit_trades", want.to_string())])
    );
    let rows = instrument::<Vec<RawGeminiTrade>>(state, id, &path, ttl::QUOTE).await?;
    Ok(normalise_trades(&rows, id, want))
}

/* ----------------------------------------------------------------- candles */

/// The exchange's interval name for one of the terminal's three periods.
///
/// It serves `1m, 5m, 15m, 30m, 1hr, 6hr, 1day` and rejects everything else with
/// a 400, so the terminal's grid maps onto it exactly and nothing has to be
/// aggregated. `1h` and `1min` are both rejections, not synonyms.
fn interval_name(interval: CandleInterval) -> &'static str {
    match interval {
        CandleInterval::OneMinute => "1m",
        CandleInterval::OneHour => "1hr",
        CandleInterval::OneDay => "1day",
    }
}

/// Bars from the matching engine, oldest first.
///
/// Two conversions, both of which are silent bugs if skipped. The feed returns
/// newest first, and a chart drawn in that order runs backwards. And its
/// timestamp is the period *start* in milliseconds, where a [`Candle`] is
/// stamped with its period *end* in seconds — half a day's error on a daily bar.
///
/// A zero-volume bar is the exchange stating that nothing printed in the period
/// and carrying the previous close forward, so it is marked untraded and keeps
/// its zero: unlike turnover, this is a figure the venue does answer.
pub fn normalise_candles(rows: &[RawGeminiKline], interval: CandleInterval) -> Vec<Candle> {
    let period = interval.seconds();
    let mut candles: Vec<Candle> = Vec::new();

    for row in rows {
        let Some(row) = row.as_array().filter(|row| row.len() >= 6) else {
            continue;
        };

        let start_ms = num(&row[0]);
        let open = num(&row[1]);
        let high = num(&row[2]);
        let low = num(&row[3]);
        let close = num(&row[4]);
        let (Some(start_ms), Some(open), Some(high), Some(low), Some(close)) =
            (start_ms, open, high, low, close)
        else {
            continue;
        };

        let volume = num(&row[5]);
        candles.push(Candle {
            time: (start_ms / 1000.0).floor() as i64 + period,
            open,
            high,
            low,
            close,
            volume,
            open_interest: None,
            traded: volume.unwrap_or(0.0) > 0.0,
            bid: None,
            ask: None,
        });
    }

    candles.sort_by_key(|candle| candle.time);
    candles
}

/// Price history over a window.
///
/// `/v2/klines` rather than `/v2/candles`, which serves the same rows in the
/// same shape but only ever the maximum window — the range is the whole point of
/// this call. No `note` on the response: these are true OHLC bars off the
/// matching engine, not a price sample series bucketed into bars, so there is
/// nothing to disclaim.
///
/// The series label is read out of the snapshot when there is one, and left
/// empty when there is not: the bars are the answer, and a cold or failed crawl
/// must not cost the reader the chart over a caption.
pub async fn get_candles(
    state: &AppState,
    id: &str,
    interval: CandleInterval,
    start_ts: i64,
    end_ts: i64,
) -> Result<CandlesResponse> {
    let path = format!(
        "/v2/klines{}",
        qs(&[
            ("symbol", id.to_string()),
            ("interval", interval_name(interval).to_string()),
            ("startTime", (start_ts * 1000).to_string()),
            ("endTime", (end_ts * 1000).to_string()),
        ])
    );
    let rows = instrument::<Vec<RawGeminiKline>>(state, id, &path, ttl::CANDLES).await?;

    let series_ticker = corpus_snapshot(state)
        .await
        .ok()
        .and_then(|corpus| {
            corpus
                .markets
                .iter()
                .find(|m| m.ticker == id)
                .map(|m| m.series_ticker.clone())
        })
        .unwrap_or_default();

    Ok(CandlesResponse {
        venue: VENUE,
        ticker: id.to_string(),
        series_ticker,
        interval,
        candles: normalise_candles(&rows, interval),
        note: None,
    })
}

/* ----------------------------------------------------------------- corpus */

const CORPUS_KEY: &str = "gemini:corpus";

/// The whole open universe, in one request where one is enough.
///
/// `limit` is honoured at any size — 500, 1,000 and 2,000 each echoed the limit
/// back — so there is nothing to paginate and no truncation to apportion between
/// categories, which is the problem every other venue's crawl spends its
/// complexity on.
///
/// It is a real ceiling on the answer, though, and the universe outgrows a fixed
/// one. This module first shipped asking for 500 against a catalogue of 480; an
/// hour later the same catalogue held 539, because a day's weather books open in
/// a batch. The crawl was still *correct* — it set `truncated` — but a board
/// quietly missing 39 events is not what `truncated` is for. So the pagination
/// footer is read, and a crawl that came back short asks once more for exactly
/// the number the venue says exists. Growth costs one extra request on the
/// refresh that notices it, and nothing afterwards.
///
/// Two corners of the catalogue are deliberately outside it. `status=settled` is
/// hard-capped at 1,000 rows however it is paged, and `status=under_review`
/// holds a handful of events that no other status returns. Neither is open
/// interest to a trader reading a live board, and both would cost a second
/// double-digit-megabyte request.
const CORPUS_LIMIT: usize = 1_000;

/// The decoded body is ~12 MB today, past the fetch default. The ceiling is set
/// well above that so the open universe has room to grow without the crawl
/// starting to fail on a Friday afternoon.
const CORPUS_MAX_BYTES: usize = 32 * 1024 * 1024;

/// What the event states that a [`Market`] has nowhere to carry.
#[derive(Debug, Clone, Default)]
struct EventFacts {
    volume24h: Option<f64>,
    tags: Vec<String>,
}

/// The snapshot plus the per-event figures the wire type has no field for.
///
/// The corpus is held behind its own `Arc` so [`corpus_snapshot`] hands out a
/// pointer rather than copying several thousand markets on every search.
struct GeminiSnapshot {
    corpus: Arc<Corpus>,
    /// Keyed by event ticker.
    facts: HashMap<String, EventFacts>,
}

/// One catalogue request, at the limit asked for.
async fn fetch_catalogue(state: &AppState, limit: usize) -> Result<RawGeminiCatalogue> {
    let url = format!(
        "{}/prediction-markets{}",
        state.config().gemini_catalogue_base,
        qs(&[("limit", limit.to_string()), ("status", "active".into())])
    );

    state
        .http()
        .fetch_json(
            &url,
            FetchOptions::new()
                .timeout(Duration::from_secs(45))
                .retries(2)
                .max_bytes(CORPUS_MAX_BYTES),
        )
        .await
}

/// How many rows the venue says exist, however many it sent.
fn stated_total(raw: &RawGeminiCatalogue, sent: usize) -> usize {
    raw.pagination
        .as_ref()
        .and_then(|page| page.total)
        .unwrap_or(sent)
}

async fn build_snapshot(state: &AppState) -> Result<GeminiSnapshot> {
    let mut raw = fetch_catalogue(state, CORPUS_LIMIT).await?;
    let mut rows = raw.data.take().unwrap_or_default();
    let mut total = stated_total(&raw, rows.len());

    if rows.len() < total {
        // The universe outgrew the standing limit. Ask again for exactly what
        // the venue says it holds, and keep the wider answer only if it really
        // is wider — a replica that lags and answers with fewer rows must not
        // shrink a board that was already complete.
        let mut wider = fetch_catalogue(state, total).await?;
        let widened = wider.data.take().unwrap_or_default();
        if widened.len() > rows.len() {
            total = stated_total(&wider, widened.len());
            rows = widened;
        }
    }

    let mut events: Vec<VenueEvent> = Vec::with_capacity(rows.len());
    let mut markets: Vec<Market> = Vec::new();
    let mut facts: HashMap<String, EventFacts> = HashMap::with_capacity(rows.len());

    for row in &rows {
        if row.ticker.as_deref().unwrap_or_default().is_empty() {
            continue;
        }
        let event = normalise_event(row);
        markets.extend(event.markets.iter().cloned());

        let mut tags = row.tags.clone().unwrap_or_default();
        if let Some(slug) = row
            .subcategory
            .as_ref()
            .and_then(|s| s.slug.as_deref())
            .filter(|slug| !slug.is_empty())
        {
            tags.push(slug.to_string());
        }
        facts.insert(
            event.event_ticker.clone(),
            EventFacts {
                volume24h: num_of(row.volume24h.as_ref()),
                tags,
            },
        );

        events.push(event);
    }

    Ok(GeminiSnapshot {
        corpus: Arc::new(Corpus::new(VENUE, events, markets, rows.len() < total)),
        facts,
    })
}

async fn snapshot(state: &AppState) -> Result<Arc<GeminiSnapshot>> {
    state
        .cache()
        .cached(CORPUS_KEY, ttl::CATALOGUE, || async {
            build_snapshot(state).await
        })
        .await
}

pub async fn corpus_snapshot(state: &AppState) -> Result<Arc<Corpus>> {
    Ok(Arc::clone(&snapshot(state).await?.corpus))
}

/// Build the snapshot ahead of the first reader, so a search pays for none of
/// it. A failure is logged and dropped: the next reader retries.
pub fn warm_corpus(state: &AppState) {
    let state = state.clone();
    tokio::spawn(async move {
        match corpus_snapshot(&state).await {
            Ok(corpus) => tracing::info!(
                events = corpus.events.len(),
                markets = corpus.markets.len(),
                "[gemini] corpus warm"
            ),
            Err(err) => tracing::warn!(error = %err, "[gemini] corpus warm failed"),
        }
    });
}

/// The series in the open snapshot, derived rather than listed.
///
/// There is no series endpoint and no series list to read, so the families come
/// from the events themselves — [`series_from_event`] on each, grouped. The
/// category filter runs over the snapshot rather than over the catalogue's own
/// repeatable `?category=` param, so what this lists and what `SRCH` and `TOP`
/// see are the same universe read at the same moment; a second request would
/// make them disagree for no gain.
///
/// `frequency` stays empty. The exchange states none, and a series named `BTC1H`
/// is not a licence to write "hourly" into a field the reader will take as the
/// venue's own word.
pub async fn list_series(state: &AppState, category: Option<&str>) -> Result<Vec<SeriesInfo>> {
    let snapshot = snapshot(state).await?;
    let want = category.map(|c| c.trim().to_lowercase());

    struct Family {
        title: String,
        category: String,
        tags: Vec<String>,
    }

    // Ordered by ticker as it is built: the list is a catalogue, and a reader
    // scanning `XV` for a family wants it where the alphabet says it is.
    let mut families: std::collections::BTreeMap<String, Family> =
        std::collections::BTreeMap::new();

    for event in &snapshot.corpus.events {
        if let Some(want) = want.as_deref() {
            if event.category.to_lowercase() != want {
                continue;
            }
        }
        if event.series_ticker.is_empty() {
            continue;
        }

        let tags = snapshot
            .facts
            .get(&event.event_ticker)
            .map(|facts| facts.tags.clone())
            .unwrap_or_default();

        let family = families
            .entry(event.series_ticker.clone())
            .or_insert_with(|| Family {
                // The first event's title stands for the family: they are the
                // same question at different expiries, so any of them names it.
                title: event.title.clone(),
                category: event.category.clone(),
                tags: Vec::new(),
            });
        for tag in tags {
            if !family.tags.contains(&tag) {
                family.tags.push(tag);
            }
        }
    }

    Ok(families
        .into_iter()
        .map(|(ticker, family)| SeriesInfo {
            venue: VENUE,
            ticker,
            title: family.title,
            category: family.category,
            frequency: String::new(),
            tags: family.tags,
        })
        .collect())
}

/// Search the local snapshot rather than the catalogue's own `?search=`.
///
/// That parameter is honoured and semantic, which is worse than absent: it
/// answers `fed` with the September FOMC market and then five baseball games,
/// and answers `zzzznotathing` with two events rather than none. Ranking the
/// snapshot the way every other venue is ranked gives results a trader can
/// compare across brokers.
pub async fn search(state: &AppState, query: &str, limit: usize) -> Result<SearchResponse> {
    refuse_overlong_query(query)?;
    let snapshot = corpus_snapshot(state).await?;
    Ok(search_corpus(&snapshot, query, limit))
}

/// Movers, ranked here rather than by [`rank_markets`](crate::sources::corpus::rank_markets).
///
/// The shared ranker drops a market whose sorted figure is `None` and gates the
/// movers on `volume24h > 0`, which between them would exclude every Gemini
/// contract but the 15 that are the whole of their event — turnover is stated
/// per event, so a leg never carries one. Ranking the same board against the
/// *event's* turnover keeps what that gate is for, that a move with nothing
/// traded behind it is a stale print rather than a move, without claiming each
/// of a 12-leg field's legs did the field's volume.
///
/// The three boards the venue registry does not declare return empty rather than
/// ranking on a substitute: there is no open interest anywhere in this API, no
/// resting-depth figure outside the per-contract book, and no per-contract
/// turnover at all. `TOP` reads that declaration and does not offer them.
pub async fn top_markets(state: &AppState, sort: MoverSort, limit: usize) -> Result<Vec<Market>> {
    if !matches!(sort, MoverSort::Gainers | MoverSort::Losers) {
        return Ok(Vec::new());
    }

    let snapshot = snapshot(state).await?;
    let mut movers: Vec<Market> = snapshot
        .corpus
        .markets
        .iter()
        .filter(|m| {
            m.change.is_some()
                && snapshot
                    .facts
                    .get(&m.event_ticker)
                    .and_then(|facts| facts.volume24h)
                    .unwrap_or(0.0)
                    > 0.0
        })
        .cloned()
        .collect();

    // Biggest rise first on `gainers`, biggest fall first on `losers`. `TOP`
    // puts several venues on one board and orders the merge itself, so what
    // matters here is that the `limit` rows handed over are the venue's real
    // movers and not the quiet end of its book.
    movers.sort_by(|a, b| {
        let ordering = a.change.unwrap_or(0.0).total_cmp(&b.change.unwrap_or(0.0));
        if sort == MoverSort::Losers {
            ordering
        } else {
            ordering.reverse()
        }
    });
    movers.truncate(limit);
    Ok(movers)
}

/* ------------------------------------------------------------------ tests */

#[cfg(test)]
mod tests {
    //! Gemini normalisation tests.
    //!
    //! The fixtures are trimmed copies of real responses from both hosts, so the
    //! decimal-string prices, the Contentful rules documents and the strike
    //! objects with label debris in them are exactly what the exchange sends —
    //! including the `prices.buy`/`prices.sell` pair that collapses to an
    //! indicative mark when nothing rests, which the normaliser deliberately
    //! ignores.

    use super::*;
    use serde_json::json;
    use wiremock::matchers::{method, path as path_matcher, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use crate::config::Config;

    /// `www.gemini.com/prediction-markets?limit=500&status=active`, trimmed by
    /// hand to seven events and sixteen legs. Each row is here for a trap:
    ///
    /// * `DEMNOM2028` — a 12-leg field (three kept) whose turnover is the
    ///   event's and must not reach a leg, with a real `priceDelta24hPct`.
    /// * `BTC2608212100` — a cumulative `categorical` ladder that sums well past
    ///   a dollar, stated series `BTC1H`, `above` strikes, `sortOrder` on every
    ///   leg, and one leg with no book at all.
    /// * `WXHIGH-CHI-2608210359` — stated series `WXHIGH` that the stripped
    ///   ticker extends, `sortOrder` out of array order, `below` strikes.
    /// * `FED260917` — the `{type:"above", value:"KE25"}` label debris.
    /// * `SEN26MI` — a two-digit year that must survive the date strip, plus a
    ///   settled leg whose prices collapse to `{"buy":{},"sell":{}}`.
    /// * `RECESSION26` — a one-leg event, where the turnover *is* the leg's.
    /// * `HORMUZNORMAL` — a settled leg the catalogue lists and api.gemini.com
    ///   answers `is not a valid symbol` for.
    const CATALOGUE_JSON: &str = include_str!("fixtures/gemini_catalogue.json");

    fn contract_of(value: Value) -> RawGeminiContract {
        serde_json::from_value(value).expect("the fixture parses as a contract")
    }

    fn event_of(value: Value) -> RawGeminiEvent {
        serde_json::from_value(value).expect("the fixture parses as an event")
    }

    fn book_of(value: Value) -> RawGeminiBook {
        serde_json::from_value(value).expect("the fixture parses as a book")
    }

    fn ticker_of(value: Value) -> RawGeminiTicker {
        serde_json::from_value(value).expect("the fixture parses as a ticker")
    }

    fn trades_of(value: Value) -> Vec<RawGeminiTrade> {
        serde_json::from_value(value).expect("the fixture parses as a tape")
    }

    fn catalogue_fixture() -> Value {
        serde_json::from_str(CATALOGUE_JSON).expect("the captured catalogue parses")
    }

    /* -------------------------------------------------------------- coercion */

    #[test]
    fn num_reads_the_decimal_strings_every_figure_arrives_as() {
        assert_eq!(num(&json!("0.19")), Some(0.19));
        assert_eq!(num(&json!("7804")), Some(7804.0));
        assert_eq!(num(&json!("3801.44")), Some(3801.44));
    }

    #[test]
    fn num_reads_a_plain_number_as_the_v2_candle_rows_send() {
        assert_eq!(num(&json!(5467)), Some(5467.0));
        assert_eq!(num(&json!(0.22)), Some(0.22));
    }

    #[test]
    fn num_states_nothing_for_absent_or_unparseable_values() {
        assert_eq!(num_of(None), None);
        assert_eq!(num(&Value::Null), None);
        assert_eq!(num(&json!("")), None);
        // Label debris where a strike should be.
        assert_eq!(num(&json!("CKHEE.")), None);
        assert_eq!(num(&json!("KE25")), None);
    }

    #[test]
    fn num_keeps_a_real_zero_a_bar_with_nothing_printed_in_it_says_so() {
        assert_eq!(num(&json!(0)), Some(0.0));
        assert_eq!(num(&json!("0")), Some(0.0));
    }

    #[test]
    fn a_zero_price_is_an_absent_quote_not_a_free_contract() {
        // The tick is a cent and `priceMinimum` is $0.01, so nothing can rest at
        // zero: a zero in a price field is an empty side wearing a number.
        assert_eq!(quote(Some(&json!("0"))), None);
        assert_eq!(quote(Some(&json!(0.0))), None);
        assert_eq!(quote(Some(&json!("0.01"))), Some(0.01));
    }

    /* --------------------------------------------------------------- markets */

    fn parent() -> GeminiParent {
        GeminiParent {
            event_ticker: "DEMNOM2028".into(),
            series_ticker: "DEMNOM".into(),
            title: "2028 Democratic Nominee for President?".into(),
            category: "Politics".into(),
            market_type: "categorical".into(),
            open_time: "2026-02-19T22:02:11.412Z".into(),
            close_time: "2028-11-10T00:00:00.000Z".into(),
            volume: None,
            volume24h: None,
        }
    }

    /// The AOC leg of DEMNOM2028, verbatim but for a shortened rules document.
    fn aoc() -> Value {
        json!({
            "id": "1889-10934",
            "label": "Alexandria Ocasio-Cortez",
            "abbreviatedName": "AOC",
            "ticker": "DEMNOM28AOC",
            "instrumentSymbol": "GEMI-DEMNOM2028-DEMNOM28AOC",
            "description": { "content": [
                { "value": "This market will resolve to \u{201c}Yes\u{201d} if Alexandria Ocasio-Cortez wins " },
                { "value": "and formally accepts the 2028 Democratic Party nomination." }
            ]},
            "prices": {
                "buy": { "yes": "0.2", "no": "0.81" },
                "sell": { "yes": "0.19", "no": "0.8" },
                "bestBid": "0.19",
                "bestAsk": "0.2",
                "lastTradePrice": "0.22"
            },
            "status": "active",
            "marketState": "open",
            "effectiveDate": "2026-02-20T14:00:00.000Z",
            "expiryDate": "2028-11-10T00:00:00.000Z",
            "priceDelta24hPct": "10",
            "priceDelta1hPct": "0"
        })
    }

    fn aoc_with(key: &str, value: Value) -> RawGeminiContract {
        let mut raw = aoc();
        raw[key] = value;
        contract_of(raw)
    }

    #[test]
    fn quotes_from_best_bid_and_best_ask_and_coerces_the_dollar_strings() {
        let market = normalise_market(&contract_of(aoc()), &parent());
        assert_eq!(market.venue, Venue::Gemini);
        assert_eq!(market.yes_bid, Some(0.19));
        assert_eq!(market.yes_ask, Some(0.2));
        assert_eq!(market.mid, Some(0.195));
        assert_eq!(market.last_price, Some(0.22));
    }

    #[test]
    fn carries_the_instrument_symbol_as_the_ticker_so_get_market_can_round_trip_it() {
        let market = normalise_market(&contract_of(aoc()), &parent());
        assert_eq!(market.ticker, "GEMI-DEMNOM2028-DEMNOM28AOC");
        assert_eq!(market.event_ticker, "DEMNOM2028");
        assert_eq!(market.series_ticker, "DEMNOM");
    }

    #[test]
    fn reads_the_symbol_the_exchange_states_rather_than_rebuilding_it_from_the_tickers() {
        // A handful of symbols do not contain their own contract ticker, so a
        // rebuilt one would name a leg that does not exist.
        let mut raw = aoc();
        raw["ticker"] = json!("TUCKERFAVREAU");
        raw["instrumentSymbol"] = json!("GEMI-SEN26ME-TROYJACKSON");
        let market = normalise_market(&contract_of(raw), &parent());
        assert_eq!(market.ticker, "GEMI-SEN26ME-TROYJACKSON");
    }

    #[test]
    fn derives_the_no_side_from_the_yes_book_exactly_as_the_venue_states_it() {
        // The exchange publishes `buy.no` and `sell.no` too, and they agreed to
        // the cent on every contract that carries the pair — which is why the
        // mirror is safe to derive rather than read.
        let market = normalise_market(&contract_of(aoc()), &parent());
        assert_eq!(market.no_bid, Some(0.8));
        assert_eq!(market.no_ask, Some(0.81));
    }

    #[test]
    fn ignores_prices_buy_and_sell_which_hold_a_mark_when_nothing_rests() {
        // A real shape: no bestBid/bestAsk at all, and buy.yes == sell.yes ==
        // lastTradePrice. Reading them would print a two-sided quote at a zero
        // spread on a contract nobody is quoting.
        let mut raw = aoc();
        raw["instrumentSymbol"] = json!("GEMI-NOBELPEACE26-AMODEI");
        raw["prices"] = json!({
            "buy": { "yes": "0.95", "no": "0.05" },
            "sell": { "yes": "0.95", "no": "0.05" },
            "lastTradePrice": "0.95"
        });
        raw.as_object_mut().unwrap().remove("priceDelta24hPct");

        let market = normalise_market(&contract_of(raw), &parent());
        assert_eq!(market.yes_bid, None);
        assert_eq!(market.yes_ask, None);
        assert_eq!(market.no_bid, None);
        assert_eq!(market.no_ask, None);
        // The print is still a fact, and the mid falls back to it.
        assert_eq!(market.last_price, Some(0.95));
        assert_eq!(market.mid, Some(0.95));
    }

    #[test]
    fn reports_a_settled_contracts_emptied_prices_as_absent_not_as_zero_quotes() {
        // A settled leg's prices collapse to {"buy":{},"sell":{}} — the bestBid,
        // bestAsk and lastTradePrice keys disappear entirely.
        let market = normalise_market(
            &contract_of(json!({
                "label": "Before August 01, 2026",
                "instrumentSymbol": "GEMI-HORMUZNORMAL-HORMUZAUGUST1",
                "prices": { "buy": {}, "sell": {} },
                "status": "settled",
                "marketState": "closed",
                "resolutionSide": "no"
            })),
            &parent(),
        );
        assert_eq!(market.yes_bid, None);
        assert_eq!(market.yes_ask, None);
        assert_eq!(market.last_price, None);
        assert_eq!(market.mid, None);
        assert_eq!(market.change, None);
        assert_eq!(market.status, "settled");
    }

    #[test]
    fn states_only_the_side_that_exists_when_the_book_is_one_sided() {
        // The mirror is per side: an offer nobody has made cannot become a NO
        // bid.
        let market = normalise_market(
            &aoc_with(
                "prices",
                json!({ "buy": {}, "sell": {}, "bestBid": "0.19", "lastTradePrice": "0.22" }),
            ),
            &parent(),
        );
        assert_eq!(market.yes_bid, Some(0.19));
        assert_eq!(market.yes_ask, None);
        assert_eq!(market.no_bid, None);
        assert_eq!(market.no_ask, Some(0.81));
        // No two-sided quote to take a midpoint of, so the last print stands in.
        assert_eq!(market.mid, Some(0.22));
    }

    #[test]
    fn leaves_turnover_open_interest_and_depth_unstated_on_a_leg_of_a_multi_leg_event() {
        // Gemini states turnover per event only, and never publishes open
        // interest or a liquidity figure — a 0 here would read as a dead market.
        let market = normalise_market(&contract_of(aoc()), &parent());
        assert_eq!(market.volume, None);
        assert_eq!(market.volume24h, None);
        assert_eq!(market.open_interest, None);
        assert_eq!(market.liquidity, None);
    }

    #[test]
    fn derives_the_move_in_dollars_from_the_percentage_the_venue_states() {
        // last 0.22 at +10% over 24h → 0.20 a day ago, a two-cent move.
        // Assigning the percentage to `change` would print a ten-dollar one.
        let market = normalise_market(&contract_of(aoc()), &parent());
        assert_eq!(market.previous_price, Some(0.2));
        assert_eq!(market.change, Some(0.02));
    }

    #[test]
    fn states_no_move_on_the_contracts_that_carry_no_percentage() {
        let mut raw = aoc();
        raw.as_object_mut().unwrap().remove("priceDelta24hPct");
        let market = normalise_market(&contract_of(raw), &parent());
        assert_eq!(market.previous_price, None);
        assert_eq!(market.change, None);
    }

    #[test]
    fn maps_the_settlement_side_onto_the_result_per_leg() {
        // Per leg, not one winner per event: a cumulative ladder settles several
        // legs YES.
        assert_eq!(
            normalise_market(&aoc_with("resolutionSide", json!("yes")), &parent()).result,
            "yes"
        );
        assert_eq!(
            normalise_market(&aoc_with("resolutionSide", json!("no")), &parent()).result,
            "no"
        );
        assert_eq!(normalise_market(&contract_of(aoc()), &parent()).result, "");
    }

    #[test]
    fn flattens_the_rich_text_rules_document() {
        // Without this every rules pane in the terminal reads `[object Object]`.
        let market = normalise_market(&contract_of(aoc()), &parent());
        assert!(market
            .rules_primary
            .starts_with("This market will resolve to \u{201c}Yes\u{201d}"));
        assert!(market
            .rules_primary
            .ends_with("formally accepts the 2028 Democratic Party nomination."));
        assert_eq!(flatten_rules(None), "");
    }

    #[test]
    fn labels_the_yes_leg_from_the_contract_and_the_no_leg_from_nothing() {
        let market = normalise_market(&contract_of(aoc()), &parent());
        assert_eq!(market.yes_sub_title, "Alexandria Ocasio-Cortez");
        assert_eq!(market.no_sub_title, "No");
        assert_eq!(market.title, "2028 Democratic Nominee for President?");
        assert_eq!(market.category.as_deref(), Some("Politics"));
    }

    #[test]
    fn opens_at_the_legs_own_effective_date_not_the_events_start_time() {
        // An event's startTime is the kickoff — when trading stops, not when it
        // began.
        let market = normalise_market(&contract_of(aoc()), &parent());
        assert_eq!(market.open_time, "2026-02-20T14:00:00.000Z");
        assert_eq!(market.close_time, "2028-11-10T00:00:00.000Z");
        assert_eq!(market.expiration_time, "2028-11-10T00:00:00.000Z");
    }

    /* ---------------------------------------------------------------- status */

    #[test]
    fn status_maps_the_venues_two_live_states_onto_the_terminals() {
        assert_eq!(
            status_of(&contract_of(
                json!({ "status": "active", "marketState": "open" })
            )),
            "open"
        );
        assert_eq!(
            status_of(&contract_of(
                json!({ "status": "settled", "marketState": "closed" })
            )),
            "settled"
        );
    }

    #[test]
    fn status_falls_through_to_market_state_when_the_contract_states_none() {
        assert_eq!(
            status_of(&contract_of(json!({ "marketState": "closed" }))),
            "settled"
        );
        assert_eq!(status_of(&RawGeminiContract::default()), "open");
    }

    #[test]
    fn status_folds_the_case_the_exchange_happens_to_send() {
        assert_eq!(
            status_of(&contract_of(json!({ "status": "Active" }))),
            "open"
        );
        assert_eq!(
            status_of(&contract_of(json!({ "marketState": "CLOSED" }))),
            "settled"
        );
    }

    #[test]
    fn status_passes_an_unseen_state_through_in_the_venues_own_word() {
        // The exchange's validator names `under_review`; nothing in the snapshot
        // used it, and guessing which of ours it resembles would be worse.
        assert_eq!(
            status_of(&contract_of(json!({ "status": "under_review" }))),
            "under_review"
        );
    }

    /* ------------------------------------------------------------- previous */

    #[test]
    fn previous_inverts_the_percentage_into_the_older_price() {
        assert_eq!(previous_from(Some(0.22), Some(&json!("10"))), Some(0.2));
        assert_eq!(previous_from(Some(0.71), Some(&json!("-4.05"))), Some(0.74));
    }

    #[test]
    fn previous_states_nothing_when_either_half_is_missing() {
        assert_eq!(previous_from(None, Some(&json!("10"))), None);
        assert_eq!(previous_from(Some(0.22), None), None);
    }

    #[test]
    fn previous_keeps_a_flat_market_flat_rather_than_dropping_it() {
        assert_eq!(previous_from(Some(0.98), Some(&json!("0"))), Some(0.98));
    }

    #[test]
    fn previous_reads_a_percentage_the_exchange_sends_as_a_string_sign_and_all() {
        assert_eq!(previous_from(Some(0.98), Some(&json!("38.03"))), Some(0.71));
        assert_eq!(previous_from(Some(0.2), Some(&json!("-80"))), Some(1.0));
    }

    #[test]
    fn previous_states_no_older_price_for_a_contract_that_lost_all_of_it() {
        // -100% inverts to a division by zero; an infinity would chart as a
        // spike.
        assert_eq!(previous_from(Some(0.01), Some(&json!("-100"))), None);
    }

    /* ---------------------------------------------------------------- strikes */

    #[test]
    fn strike_reads_above_as_inclusive_because_the_rung_pays_at_its_own_strike() {
        // Every numeric `above` strike is labelled "$61,000 or above", and its
        // terms read "if the price of Bitcoin is $61,000 or above … resolve to
        // Yes".
        let strike = strike_of(Some(&strike_json(
            json!({ "type": "above", "value": "61000" }),
        )));
        assert_eq!(strike.strike_type, Some(StrikeType::GreaterOrEqual));
        assert_eq!(strike.floor_strike, Some(61000.0));
        assert_eq!(strike.cap_strike, None);
    }

    #[test]
    fn strike_reads_below_and_under_or_equal_as_the_one_inclusive_upper_bound() {
        // The venue spells the same relation two ways: every numeric `below` is
        // a "76°F or below" weather bucket, and `under_or_equal` is a top-10
        // finish "including ties". Strict bounds would leave a gap at every tick.
        for raw in [
            json!({ "type": "below", "value": "76" }),
            json!({ "type": "under_or_equal", "value": "76" }),
        ] {
            let strike = strike_of(Some(&strike_json(raw)));
            assert_eq!(strike.strike_type, Some(StrikeType::LessOrEqual));
            assert_eq!(strike.floor_strike, None);
            assert_eq!(strike.cap_strike, Some(76.0));
        }
    }

    #[test]
    fn strike_reads_reference_as_no_strike_it_is_a_level_quoted_against() {
        let strike = strike_of(Some(&strike_json(
            json!({ "type": "reference", "value": "64182.97" }),
        )));
        assert_eq!(strike, Strike::default());
    }

    #[test]
    fn strike_rejects_the_rows_carrying_label_debris_instead_of_a_figure() {
        // "Lockheed Martin" and "Hike 25bps" arrive as strikes. A NaN bound
        // would sort to one end of every ladder it appeared in.
        assert_eq!(
            strike_of(Some(&strike_json(
                json!({ "type": "below", "value": "CKHEE." })
            ))),
            Strike::default()
        );
        assert_eq!(
            strike_of(Some(&strike_json(
                json!({ "type": "above", "value": "KE25" })
            ))),
            Strike::default()
        );
        assert_eq!(strike_of(None), Strike::default());
    }

    #[test]
    fn strike_reaches_the_market_it_belongs_to() {
        let market = normalise_market(
            &contract_of(json!({
                "label": "$61,000 or above",
                "instrumentSymbol": "GEMI-BTC2608212100-HI61000",
                "prices": { "bestBid": "0.97", "bestAsk": "0.98", "lastTradePrice": "0.83" },
                "strike": { "type": "above", "value": "61000" }
            })),
            &parent(),
        );
        assert_eq!(market.strike_type, Some(StrikeType::GreaterOrEqual));
        assert_eq!(market.floor_strike, Some(61000.0));
        assert_eq!(market.cap_strike, None);
    }

    fn strike_json(value: Value) -> RawGeminiStrike {
        serde_json::from_value(value).expect("the fixture parses as a strike")
    }

    /* ------------------------------------------------------------------ series */

    fn series_of(ticker: &str, stated: Option<&str>) -> String {
        let mut raw = json!({ "ticker": ticker });
        if let Some(stated) = stated {
            raw["series"] = json!(stated);
        }
        series_from_event(&event_of(raw))
    }

    #[test]
    fn series_prefers_the_one_the_exchange_states_because_the_ticker_fuses_products() {
        // The hourly bitcoin ladder and the monthly one strip to the same key.
        assert_eq!(series_of("BTC2608202100", Some("BTC1H")), "BTC1H");
        assert_eq!(series_of("BTC2608312100", Some("BTC")), "BTC");
        assert_eq!(series_of("BTC15M2608200200", Some("BTC15M")), "BTC15M");
    }

    #[test]
    fn series_keeps_the_city_when_the_stated_series_drops_it() {
        // The venue files all seventeen daily-high weather books under one
        // WXHIGH product. A series here is a recurring question, and the high in
        // Miami is not the high in Chicago — grouped, sixteen of the seventeen
        // become unreachable from `XV`, since one event stands for the family.
        assert_eq!(
            series_of("WXHIGH-CHI-2608210359", Some("WXHIGH")),
            "WXHIGH-CHI"
        );
        assert_eq!(
            series_of("WXHIGH-MIA-2608210359", Some("WXHIGH")),
            "WXHIGH-MIA"
        );
    }

    #[test]
    fn series_strips_the_settlement_date_where_no_series_is_stated() {
        assert_eq!(series_of("FED260917", None), "FED");
        assert_eq!(series_of("FED260729", None), "FED");
        assert_eq!(series_of("NFL-2608212330-CAR-JAX-M", None), "NFL-CAR-JAX-M");
        assert_eq!(
            series_of("TENNIS-ATPCIN-20260818-FAR-WAL", None),
            "TENNIS-ATPCIN-FAR-WAL"
        );
        assert_eq!(series_of("GOLF-BMW-WIN-20260823", None), "GOLF-BMW-WIN");
    }

    #[test]
    fn series_keeps_a_two_digit_year_so_the_senate_races_stay_36_series() {
        // A two-or-more-digit threshold would strip the year and leave every
        // race as `SEN`.
        assert_eq!(series_of("SEN26MI", None), "SEN26MI");
        assert_eq!(series_of("SEN26AK", None), "SEN26AK");
        assert_eq!(series_of("HOUSE26AZ-06", None), "HOUSE26AZ-06");
    }

    #[test]
    fn series_ignores_an_empty_series_field_rather_than_filing_under_nothing() {
        assert_eq!(series_of("FED260917", Some("")), "FED");
    }

    #[test]
    fn series_falls_back_to_the_ticker_when_stripping_would_leave_nothing() {
        assert_eq!(series_of("2608212100", None), "2608212100");
        assert_eq!(series_from_event(&RawGeminiEvent::default()), "");
    }

    /* ------------------------------------------------------------------ events */

    fn btc() -> Value {
        json!({
            "ticker": "BTC2608212100",
            "title": "BTC price on August 21",
            "type": "categorical",
            "series": "BTC1H",
            "category": "Crypto",
            "status": "active",
            "volume": "3801.44",
            "volume24h": "522.93",
            "effectiveDate": "2026-08-14T09:27:07.707Z",
            "expiryDate": "2026-08-21T21:00:00.000Z",
            "startTime": "2026-08-14T09:30:00.000Z",
            "contracts": [
                {
                    "label": "$61,000 or above",
                    "instrumentSymbol": "GEMI-BTC2608212100-HI61000",
                    "prices": { "bestBid": "0.97", "bestAsk": "0.98", "lastTradePrice": "0.83" },
                    "strike": { "type": "above", "value": "61000" }
                },
                {
                    "label": "$61,500 or above",
                    "instrumentSymbol": "GEMI-BTC2608212100-HI61500",
                    "prices": { "bestBid": "0.94", "bestAsk": "0.96", "lastTradePrice": "0.74" },
                    "strike": { "type": "above", "value": "61500" }
                }
            ]
        })
    }

    #[test]
    fn stamps_its_event_and_series_onto_every_leg() {
        let event = normalise_event(&event_of(btc()));
        assert_eq!(event.venue, Venue::Gemini);
        assert_eq!(event.event_ticker, "BTC2608212100");
        assert_eq!(event.series_ticker, "BTC1H");
        assert_eq!(event.markets.len(), 2);
        assert_eq!(event.markets[0].event_ticker, "BTC2608212100");
        assert_eq!(event.markets[1].series_ticker, "BTC1H");
        assert_eq!(event.markets[0].title, "BTC price on August 21");
        assert_eq!(event.markets[0].market_type, "categorical");
        assert_eq!(event.markets[0].category.as_deref(), Some("Crypto"));
    }

    #[test]
    fn claims_no_exclusivity_because_categorical_includes_cumulative_ladders() {
        // These legs are "$61,000 or above" and "$61,500 or above": the full
        // ladder's asks sum to $6.80, and every leg below the settlement pays.
        assert!(!normalise_event(&event_of(btc())).mutually_exclusive);
    }

    #[test]
    fn claims_no_exclusivity_on_a_two_leg_head_to_head_either() {
        // Obviously exclusive, and the exchange still says nothing that says so.
        let game = normalise_event(&event_of(json!({
            "ticker": "NFL-2608212330-CAR-JAX-M",
            "type": "categorical",
            "contracts": [{ "instrumentSymbol": "A" }, { "instrumentSymbol": "B" }]
        })));
        assert!(!game.mutually_exclusive);
    }

    #[test]
    fn withholds_the_events_turnover_from_the_legs_of_a_multi_leg_event() {
        // Copying it onto both legs would have `sum_or_null` report double the
        // true turnover in the search panel.
        let event = normalise_event(&event_of(btc()));
        assert_eq!(event.markets[0].volume, None);
        assert_eq!(event.markets[0].volume24h, None);
        assert_eq!(event.markets[1].volume24h, None);
    }

    #[test]
    fn gives_a_one_leg_events_turnover_to_its_only_leg_where_the_two_are_one_figure() {
        let event = normalise_event(&event_of(json!({
            "ticker": "RECESSION26",
            "title": "Recession this year?",
            "type": "binary",
            "volume": "47069",
            "volume24h": "126",
            "contracts": [{
                "label": "Yes",
                "instrumentSymbol": "GEMI-RECESSION26-2026",
                "prices": { "bestBid": "0.04", "bestAsk": "0.05", "lastTradePrice": "0.04" }
            }]
        })));
        assert_eq!(event.markets[0].volume, Some(47069.0));
        assert_eq!(event.markets[0].volume24h, Some(126.0));
        // Still nothing anywhere for these two.
        assert_eq!(event.markets[0].open_interest, None);
        assert_eq!(event.markets[0].liquidity, None);
    }

    #[test]
    fn keeps_a_one_leg_events_stated_zero_turnover_as_a_zero() {
        // The exchange answered "nothing traded". That is not the same silence
        // as the multi-leg legs above, and must not become one.
        let event = normalise_event(&event_of(json!({
            "ticker": "ZECEXPLOIT26",
            "type": "binary",
            "volume": "0",
            "volume24h": "0",
            "contracts": [{ "label": "Proof published", "instrumentSymbol": "Z" }]
        })));
        assert_eq!(event.markets[0].volume, Some(0.0));
        assert_eq!(event.markets[0].volume24h, Some(0.0));
    }

    #[test]
    fn reads_the_legs_in_the_order_the_venue_numbers_them_not_the_arrays() {
        // WXHIGH-CHI arrives with its temperature buckets shuffled; sortOrder
        // reads the ladder cold to hot, and a strike ladder out of sequence is
        // unreadable.
        let event = normalise_event(&event_of(json!({
            "ticker": "WXHIGH-CHI-2608210359",
            "series": "WXHIGH",
            "contracts": [
                { "label": "79\u{b0}F to 80\u{b0}F", "instrumentSymbol": "A", "sortOrder": 3 },
                { "label": "76\u{b0}F or below", "instrumentSymbol": "B", "sortOrder": 1 },
                { "label": "85\u{b0}F or above", "instrumentSymbol": "C", "sortOrder": 6 }
            ]
        })));
        let labels: Vec<&str> = event
            .markets
            .iter()
            .map(|m| m.yes_sub_title.as_str())
            .collect();
        assert_eq!(
            labels,
            vec![
                "76\u{b0}F or below",
                "79\u{b0}F to 80\u{b0}F",
                "85\u{b0}F or above"
            ]
        );
    }

    #[test]
    fn leaves_a_partly_numbered_event_in_the_order_it_arrived() {
        // Sorting around the gaps would move the numbered legs past the rest for
        // no stated reason; the payload order is at least the exchange's own.
        let event = normalise_event(&event_of(json!({
            "ticker": "FED260917",
            "contracts": [
                { "label": "Fed maintains rate", "instrumentSymbol": "A" },
                { "label": "Hike 25bps", "instrumentSymbol": "B", "sortOrder": 0 }
            ]
        })));
        let labels: Vec<&str> = event
            .markets
            .iter()
            .map(|m| m.yes_sub_title.as_str())
            .collect();
        assert_eq!(labels, vec!["Fed maintains rate", "Hike 25bps"]);
    }

    #[test]
    fn leaves_the_subtitle_empty_rather_than_filling_it_with_hourly_commentary() {
        // `shortSummary` is regenerated through the day: a subtitle that changes
        // under the reader.
        assert_eq!(normalise_event(&event_of(btc())).sub_title, "");
    }

    /* -------------------------------------------------------------------- book */

    /// `/v1/book/GEMI-DEMNOM2028-DEMNOM28AOC`, verbatim.
    fn raw_book() -> RawGeminiBook {
        book_of(json!({
            "bids": [
                { "price": "0.19", "amount": "1530.0", "timestamp": "1787032777" },
                { "price": "0.18", "amount": "25.0", "timestamp": "1787032777" },
                { "price": "0.17", "amount": "788.0", "timestamp": "1787032777" },
                { "price": "0.16", "amount": "100.0", "timestamp": "1787032777" },
                { "price": "0.01", "amount": "250.0", "timestamp": "1787032777" }
            ],
            "asks": [
                { "price": "0.2", "amount": "1250.0", "timestamp": "1787032777" },
                { "price": "0.21", "amount": "110.0", "timestamp": "1787032777" }
            ]
        }))
    }

    #[test]
    fn coerces_the_string_ladders_and_orders_them_best_first() {
        let book = normalise_order_book(&raw_book(), "GEMI-DEMNOM2028-DEMNOM28AOC", 12);
        assert_eq!(book.venue, Venue::Gemini);
        let bids: Vec<f64> = book.yes.iter().map(|l| l.price).collect();
        let asks: Vec<f64> = book.yes_asks.iter().map(|l| l.price).collect();
        assert_eq!(bids, vec![0.19, 0.18, 0.17, 0.16, 0.01]);
        assert_eq!(asks, vec![0.2, 0.21]);
        assert_eq!(book.yes[0].size, 1530.0);
    }

    #[test]
    fn derives_the_no_ladder_by_inverting_the_offers() {
        // A resting offer of YES at 20¢ is a bid for NO at 80¢ — the same order
        // seen from the other end, since there is no separate NO instrument.
        let book = normalise_order_book(&raw_book(), "x", 12);
        let no: Vec<f64> = book.no.iter().map(|l| l.price).collect();
        assert_eq!(no, vec![0.8, 0.79]);
        assert_eq!(book.no[0].size, 1250.0);
    }

    #[test]
    fn states_the_top_of_book_and_the_spread() {
        let book = normalise_order_book(&raw_book(), "x", 12);
        assert_eq!(book.best_yes_bid, Some(0.19));
        assert_eq!(book.best_yes_ask, Some(0.2));
        assert_eq!(book.spread, Some(0.01));
        assert_eq!(book.mid, Some(0.195));
    }

    #[test]
    fn reports_a_settled_instruments_empty_book_as_absent_not_as_a_zero_quote() {
        // A settled instrument answers {"bids":[],"asks":[]}.
        let book = normalise_order_book(&book_of(json!({ "bids": [], "asks": [] })), "x", 12);
        assert!(book.yes.is_empty());
        assert!(book.no.is_empty());
        assert_eq!(book.best_yes_bid, None);
        assert_eq!(book.best_yes_ask, None);
        assert_eq!(book.spread, None);
        assert_eq!(book.mid, None);
    }

    #[test]
    fn caps_each_side_of_the_book_at_the_requested_depth() {
        let book = normalise_order_book(&raw_book(), "x", 2);
        assert_eq!(book.yes.len(), 2);
        assert_eq!(book.yes_asks.len(), 2);
        assert_eq!(book.best_yes_bid, Some(0.19));
    }

    #[test]
    fn drops_book_rows_with_no_price_or_no_size() {
        let book = normalise_order_book(
            &book_of(json!({ "bids": [
                { "price": "0", "amount": "100" },
                { "price": "0.4", "amount": "0" },
                { "price": "0.3", "amount": "5" }
            ]})),
            "x",
            12,
        );
        assert_eq!(book.yes.len(), 1);
        assert_eq!(book.yes[0].price, 0.3);
    }

    /* --------------------------------------------------------------- apply_book */

    fn base() -> Market {
        normalise_market(&contract_of(aoc()), &parent())
    }

    fn small_book() -> RawGeminiBook {
        book_of(json!({
            "bids": [
                { "price": "0.19", "amount": "1530.0" },
                { "price": "0.18", "amount": "25.0" }
            ],
            "asks": [{ "price": "0.2", "amount": "1250.0" }]
        }))
    }

    #[test]
    fn adds_up_the_resting_depth_the_exchange_never_states() {
        // 0.19×1530 + 0.18×25 on the bid, and 0.80×1250 behind the offer:
        // whoever offers YES at 20¢ has 80¢ of collateral behind every contract.
        let market = apply_book(base(), Some(&small_book()));
        assert_eq!(market.liquidity, Some(1295.2));
    }

    #[test]
    fn refreshes_the_top_of_book_from_the_live_ladder() {
        let stale = normalise_market(
            &aoc_with(
                "prices",
                json!({ "bestBid": "0.10", "bestAsk": "0.30", "lastTradePrice": "0.22" }),
            ),
            &parent(),
        );
        let market = apply_book(stale, Some(&small_book()));
        assert_eq!(market.yes_bid, Some(0.19));
        assert_eq!(market.yes_ask, Some(0.2));
        assert_eq!(market.no_bid, Some(0.8));
        assert_eq!(market.mid, Some(0.195));
    }

    #[test]
    fn calls_an_empty_book_zero_depth_the_exchange_was_asked_and_answered() {
        let market = apply_book(base(), Some(&book_of(json!({ "bids": [], "asks": [] }))));
        assert_eq!(market.liquidity, Some(0.0));
        // Turnover is the other case: never asked, never answered.
        assert_eq!(market.volume24h, None);
    }

    #[test]
    fn leaves_the_market_untouched_when_the_book_could_not_be_read() {
        let untouched = apply_book(base(), None);
        assert_eq!(
            serde_json::to_value(&untouched).unwrap(),
            serde_json::to_value(base()).unwrap()
        );
    }

    /* ------------------------------------------------------------- apply_ticker */

    /// `/v2/ticker/GEMI-DEMNOM2028-DEMNOM28AOC`, verbatim but for a trimmed
    /// `changes` array.
    fn raw_ticker() -> RawGeminiTicker {
        ticker_of(json!({
            "symbol": "GEMI-DEMNOM2028-DEMNOM28AOC",
            "open": "0.2",
            "high": "0.22",
            "low": "0.2",
            "close": "0.22",
            "changes": ["0.2", "0.21", "0.22"],
            "bid": "0.1900",
            "ask": "0.2000"
        }))
    }

    #[test]
    fn coerces_the_four_decimal_quote_strings_the_ticker_sends() {
        let market = apply_ticker(base(), Some(&raw_ticker()));
        assert_eq!(market.yes_bid, Some(0.19));
        assert_eq!(market.yes_ask, Some(0.2));
        assert_eq!(market.last_price, Some(0.22));
    }

    #[test]
    fn gives_a_move_to_a_contract_whose_catalogue_row_carries_no_percentage() {
        // The 24h open is the only other place a previous price exists, and a
        // percentage of a price nobody states is not a figure.
        let mut raw = aoc();
        raw.as_object_mut().unwrap().remove("priceDelta24hPct");
        let bare = normalise_market(&contract_of(raw), &parent());
        assert_eq!(bare.change, None);

        let market = apply_ticker(bare, Some(&raw_ticker()));
        assert_eq!(market.previous_price, Some(0.2));
        assert_eq!(market.change, Some(0.02));
    }

    #[test]
    fn keeps_the_exchanges_own_percentage_where_it_stated_one() {
        let mut raw = raw_ticker();
        raw.open = Some(json!("0.05"));
        let market = apply_ticker(base(), Some(&raw));
        assert_eq!(market.previous_price, Some(0.2));
    }

    #[test]
    fn does_not_read_the_changes_array_whose_order_is_disputed() {
        // The documented order is newest first and the payload's is not:
        // `changes[0]` equalled `open` and the last equalled `close` on every
        // instrument sampled. `open` and `close` say the same two prices
        // unambiguously, so reversing the array cannot change the answer.
        let mut reversed = raw_ticker();
        reversed.changes = Some(vec![json!("0.22"), json!("0.21"), json!("0.2")]);
        assert_eq!(
            serde_json::to_value(apply_ticker(base(), Some(&reversed))).unwrap(),
            serde_json::to_value(apply_ticker(base(), Some(&raw_ticker()))).unwrap()
        );
    }

    #[test]
    fn leaves_the_market_untouched_when_the_instrument_has_never_traded() {
        // `/v2/ticker` 404s on those, which is a fact about the contract.
        let untouched = apply_ticker(base(), None);
        assert_eq!(
            serde_json::to_value(&untouched).unwrap(),
            serde_json::to_value(base()).unwrap()
        );
    }

    /* -------------------------------------------------------------------- tape */

    /// `/v1/trades/GEMI-DEMNOM2028-DEMNOM28AOC`, verbatim.
    fn raw_trades() -> Vec<RawGeminiTrade> {
        trades_of(json!([
            {
                "timestamp": 1787183859,
                "timestampms": 1787183859270i64,
                "tid": 1893456011072804i64,
                "price": "0.18",
                "amount": "13",
                "exchange": "gemini",
                "type": "buy"
            },
            {
                "timestamp": 1787114019,
                "timestampms": 1787114019968i64,
                "tid": 1893456011046845i64,
                "price": "0.19",
                "amount": "234",
                "exchange": "gemini",
                "type": "sell"
            }
        ]))
    }

    #[test]
    fn coerces_the_print_price_and_size_and_restates_the_no_price() {
        let response = normalise_trades(&raw_trades(), "GEMI-DEMNOM2028-DEMNOM28AOC", 50);
        assert_eq!(response.trades[0].yes_price, 0.18);
        assert_eq!(response.trades[0].no_price, 0.82);
        assert_eq!(response.trades[0].count, 13.0);
        assert_eq!(response.trades[0].venue, Venue::Gemini);
        assert_eq!(response.trades[0].ticker, "GEMI-DEMNOM2028-DEMNOM28AOC");
    }

    #[test]
    fn carries_the_sixteen_digit_trade_id_as_text() {
        // An identity, never an amount: an f64 cannot hold it exactly.
        let response = normalise_trades(&raw_trades(), "x", 50);
        assert_eq!(response.trades[0].trade_id, "1893456011072804");
    }

    #[test]
    fn takes_the_seconds_timestamp_as_it_stands_without_touching_the_ms_twin() {
        let response = normalise_trades(&raw_trades(), "x", 50);
        assert_eq!(response.trades[0].ts, 1_787_183_859);
    }

    #[test]
    fn falls_back_to_the_millisecond_stamp_when_only_that_is_present() {
        let rows = trades_of(json!([
            { "timestampms": 1787003922607i64, "price": "0.2", "amount": "5" }
        ]));
        let response = normalise_trades(&rows, "x", 50);
        assert_eq!(response.trades[0].ts, 1_787_003_922);
    }

    #[test]
    fn drops_a_print_it_cannot_price_size_or_place_rather_than_inventing_one() {
        // A defaulted price is a free trade, a defaulted size an empty one and a
        // defaulted clock a print stamped 1970 — all three read as outliers.
        let mut rows = trades_of(json!([
            { "timestamp": 1787003922, "amount": "100", "type": "buy" },
            { "timestamp": 1787003922, "price": "0.2", "type": "buy" },
            { "price": "0.2", "amount": "100", "type": "buy" }
        ]));
        rows.extend(raw_trades());

        let response = normalise_trades(&rows, "x", 50);
        assert_eq!(response.trades.len(), 2);
        assert_eq!(response.trades[0].yes_price, 0.18);
    }

    #[test]
    fn reads_buy_and_sell_as_the_aggressors_side_and_flags_no_blocks() {
        // The payload states nothing beyond the field name, so this is a reading
        // of it rather than a fact the exchange asserted.
        let response = normalise_trades(&raw_trades(), "x", 50);
        assert_eq!(response.trades[0].taker_side, "yes");
        assert_eq!(response.trades[1].taker_side, "no");
        assert!(!response.trades[0].is_block_trade);
    }

    #[test]
    fn offers_no_cursor_because_a_tid_walk_over_an_inconsistent_tape_skips_prints() {
        // Identical requests returned 130, 142 and 164 rows within a minute, and
        // none is a prefix of another.
        assert!(normalise_trades(&raw_trades(), "x", 50).cursor.is_none());
    }

    #[test]
    fn honours_the_requested_tape_page_size() {
        assert_eq!(normalise_trades(&raw_trades(), "x", 1).trades.len(), 1);
        assert_eq!(normalise_trades(&[], "x", 50).trades.len(), 0);
    }

    /* ----------------------------------------------------------------- candles */

    /// `/v2/klines?symbol=GEMI-DEMNOM2028-DEMNOM28AOC&interval=1day`, verbatim,
    /// newest first.
    fn daily() -> Vec<Value> {
        vec![
            json!([1786924800000i64, 0.2, 0.22, 0.2, 0.22, 5467]),
            json!([1786838400000i64, 0.21, 0.21, 0.19, 0.2, 975]),
            json!([1786752000000i64, 0.18, 0.21, 0.18, 0.21, 1819]),
        ]
    }

    #[test]
    fn turns_the_period_start_in_milliseconds_into_a_period_end_in_seconds() {
        // 1786924800000 is 2026-08-17T00:00Z; the daily bar it opens closes a
        // day later. Stamping the start would be half a day out on every bar.
        let candles = normalise_candles(&daily(), CandleInterval::OneDay);
        assert_eq!(candles[2].time, 1_786_924_800 + 86_400);
    }

    #[test]
    fn reverses_the_feed_which_arrives_newest_first() {
        // A chart drawn in the order the exchange sends runs backwards.
        let candles = normalise_candles(&daily(), CandleInterval::OneDay);
        let closes: Vec<f64> = candles.iter().map(|c| c.close).collect();
        assert_eq!(closes, vec![0.21, 0.2, 0.22]);
    }

    #[test]
    fn carries_the_ohlc_through_as_dollars_and_the_volume_as_contracts() {
        let candles = normalise_candles(&daily(), CandleInterval::OneDay);
        let last = &candles[2];
        assert_eq!(last.open, 0.2);
        assert_eq!(last.high, 0.22);
        assert_eq!(last.low, 0.2);
        assert_eq!(last.close, 0.22);
        assert_eq!(last.volume, Some(5467.0));
        assert!(last.traded);
    }

    #[test]
    fn marks_a_bar_with_nothing_printed_untraded_and_keeps_its_stated_zero() {
        // A real 1hr row: the close is carried forward, and the exchange does
        // state the zero — unlike turnover, which it never states at all.
        let candles = normalise_candles(
            &[json!([1787029200000i64, 0.22, 0.22, 0.22, 0.22, 0])],
            CandleInterval::OneHour,
        );
        assert_eq!(candles[0].volume, Some(0.0));
        assert!(!candles[0].traded);
    }

    #[test]
    fn states_no_open_interest_or_quote_on_a_bar_because_the_row_carries_none() {
        let candles = normalise_candles(&daily(), CandleInterval::OneDay);
        assert_eq!(candles[0].open_interest, None);
        assert_eq!(candles[0].bid, None);
        assert_eq!(candles[0].ask, None);
    }

    #[test]
    fn closes_a_one_minute_bar_a_minute_after_it_opened() {
        let candles = normalise_candles(
            &[json!([1787003880000i64, 0.7, 0.71, 0.7, 0.71, 30])],
            CandleInterval::OneMinute,
        );
        assert_eq!(candles[0].time, 1_787_003_880 + 60);
    }

    #[test]
    fn drops_a_malformed_bar_rather_than_charting_a_nan() {
        let mut rows = daily();
        rows.push(json!([1, 2]));
        rows.push(json!("not a row"));
        assert_eq!(
            normalise_candles(&rows, CandleInterval::OneDay).len(),
            daily().len()
        );
    }

    #[test]
    fn names_each_interval_the_way_the_exchange_spells_it() {
        // `1h` and `1min` are 400s, not synonyms.
        assert_eq!(interval_name(CandleInterval::OneMinute), "1m");
        assert_eq!(interval_name(CandleInterval::OneHour), "1hr");
        assert_eq!(interval_name(CandleInterval::OneDay), "1day");
    }

    /* --------------------------------------------------------------- the wire */

    /// Both hosts stand behind one mock: the catalogue paths all begin
    /// `/prediction-markets` and the trading ones `/v1` or `/v2`, so nothing
    /// collides.
    fn state_for(server: &MockServer) -> AppState {
        AppState::new(Config {
            gemini_catalogue_base: server.uri(),
            gemini_api_base: server.uri(),
            ..Config::default()
        })
    }

    async fn mount_catalogue(server: &MockServer) {
        Mock::given(method("GET"))
            .and(path_matcher("/prediction-markets"))
            .respond_with(ResponseTemplate::new(200).set_body_json(catalogue_fixture()))
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn the_crawl_asks_once_for_the_whole_open_universe() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/prediction-markets"))
            .and(query_param("limit", "1000"))
            .and(query_param("status", "active"))
            .respond_with(ResponseTemplate::new(200).set_body_json(catalogue_fixture()))
            // One request, not a paginated walk: `limit` has no cap upstream.
            .expect(1)
            .mount(&server)
            .await;

        let corpus = corpus_snapshot(&state_for(&server))
            .await
            .expect("the crawl completes");

        assert_eq!(corpus.venue, Venue::Gemini);
        assert_eq!(corpus.events.len(), 7);
        assert_eq!(corpus.markets.len(), 16);
        // `pagination.total` matched the row count, so nothing was left behind.
        assert!(!corpus.truncated);
    }

    #[tokio::test]
    async fn the_crawl_asks_again_when_the_universe_outgrew_its_limit() {
        // The live catalogue held 480 events one afternoon and 539 an hour
        // later, because a day's weather books open in a batch. A standing limit
        // is therefore a ceiling that the universe walks through, and a board
        // silently missing 39 events is worse than a second request.
        let server = MockServer::start().await;
        let mut short = catalogue_fixture();
        let all = short["data"].as_array().expect("the fixture is an array").clone();
        short["data"] = json!(all[..4]);
        short["pagination"] = json!({ "limit": 1000, "offset": 0, "total": all.len() });

        Mock::given(method("GET"))
            .and(path_matcher("/prediction-markets"))
            .and(query_param("limit", "1000"))
            .respond_with(ResponseTemplate::new(200).set_body_json(short))
            .expect(1)
            .mount(&server)
            .await;

        let mut whole = catalogue_fixture();
        whole["pagination"] = json!({ "limit": all.len(), "offset": 0, "total": all.len() });
        Mock::given(method("GET"))
            .and(path_matcher("/prediction-markets"))
            .and(query_param("limit", all.len().to_string()))
            .respond_with(ResponseTemplate::new(200).set_body_json(whole))
            .expect(1)
            .mount(&server)
            .await;

        let corpus = corpus_snapshot(&state_for(&server))
            .await
            .expect("the crawl completes");

        assert_eq!(corpus.events.len(), 7);
        assert!(!corpus.truncated);
    }

    #[tokio::test]
    async fn a_second_crawl_that_answers_with_fewer_rows_does_not_shrink_the_board() {
        // The re-ask goes to whichever replica answers, and one that lags would
        // otherwise replace a complete board with a shorter one.
        let server = MockServer::start().await;
        let all = catalogue_fixture()["data"]
            .as_array()
            .expect("the fixture is an array")
            .clone();

        let mut most = catalogue_fixture();
        most["data"] = json!(all[..6]);
        most["pagination"] = json!({ "limit": 1000, "offset": 0, "total": all.len() });
        Mock::given(method("GET"))
            .and(path_matcher("/prediction-markets"))
            .and(query_param("limit", "1000"))
            .respond_with(ResponseTemplate::new(200).set_body_json(most))
            .mount(&server)
            .await;

        let mut lagging = catalogue_fixture();
        lagging["data"] = json!(all[..2]);
        lagging["pagination"] = json!({ "limit": all.len(), "offset": 0, "total": all.len() });
        Mock::given(method("GET"))
            .and(path_matcher("/prediction-markets"))
            .and(query_param("limit", all.len().to_string()))
            .respond_with(ResponseTemplate::new(200).set_body_json(lagging))
            .mount(&server)
            .await;

        let corpus = corpus_snapshot(&state_for(&server))
            .await
            .expect("the crawl completes");

        assert_eq!(corpus.events.len(), 6);
        // Still short of what the venue says it holds, and the board says so.
        assert!(corpus.truncated);
    }

    #[tokio::test]
    async fn the_snapshot_is_built_once_and_shared() {
        let server = MockServer::start().await;
        mount_catalogue(&server).await;

        let state = state_for(&server);
        let first = corpus_snapshot(&state).await.expect("a snapshot");
        let second = corpus_snapshot(&state).await.expect("a snapshot");
        assert!(Arc::ptr_eq(&first, &second));
    }

    #[tokio::test]
    async fn the_crawl_reports_a_catalogue_it_could_not_finish() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/prediction-markets"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": [{ "ticker": "FED260917", "contracts": [{ "instrumentSymbol": "A" }] }],
                "pagination": { "limit": 500, "offset": 0, "total": 900 }
            })))
            .mount(&server)
            .await;

        let corpus = corpus_snapshot(&state_for(&server))
            .await
            .expect("the crawl completes");
        assert!(corpus.truncated);
    }

    #[tokio::test]
    async fn get_event_normalises_every_leg_of_the_event_it_is_given() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/prediction-markets/BTC2608212100"))
            .respond_with(ResponseTemplate::new(200).set_body_json(btc()))
            .expect(1)
            .mount(&server)
            .await;

        let event = get_event(&state_for(&server), "BTC2608212100")
            .await
            .expect("the event resolves");
        assert_eq!(event.series_ticker, "BTC1H");
        assert_eq!(event.markets.len(), 2);
        assert_eq!(event.markets[0].yes_bid, Some(0.97));
        assert!(!event.mutually_exclusive);
    }

    #[tokio::test]
    async fn get_event_reports_an_unknown_ticker_as_not_found_with_the_shape_to_retype() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/prediction-markets/NOPENOPE"))
            .respond_with(ResponseTemplate::new(404).set_body_json(
                json!({ "error": "NOT_FOUND", "message": "Prediction market not found" }),
            ))
            .mount(&server)
            .await;

        let err = get_event(&state_for(&server), "NOPENOPE")
            .await
            .expect_err("an unlisted ticker is not found");
        assert_eq!(err.code, codes::NOT_FOUND);
        assert_eq!(err.message, "No Gemini event NOPENOPE");
        // The slug in the web address is not a lookup key and is not unique.
        assert!(err.hint.unwrap().contains("DEMNOM2028"));
    }

    #[tokio::test]
    async fn get_event_reads_a_two_hundred_with_no_ticker_in_it_as_not_found() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/prediction-markets/EMPTY"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
            .mount(&server)
            .await;

        let err = get_event(&state_for(&server), "EMPTY")
            .await
            .expect_err("an empty event is not an event");
        assert_eq!(err.code, codes::NOT_FOUND);
    }

    #[tokio::test]
    async fn get_market_recovers_the_leg_through_its_parent_and_merges_both_sidecars() {
        let server = MockServer::start().await;
        mount_catalogue(&server).await;

        Mock::given(method("GET"))
            .and(path_matcher("/prediction-markets/DEMNOM2028"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(catalogue_fixture()["data"][0].clone()),
            )
            .mount(&server)
            .await;

        Mock::given(method("GET"))
            .and(path_matcher("/v1/book/GEMI-DEMNOM2028-DEMNOM28AOC"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "bids": [{ "price": "0.19", "amount": "1530.0" }, { "price": "0.18", "amount": "25.0" }],
                "asks": [{ "price": "0.2", "amount": "1250.0" }]
            })))
            .mount(&server)
            .await;

        Mock::given(method("GET"))
            .and(path_matcher("/v2/ticker/GEMI-DEMNOM2028-DEMNOM28AOC"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "symbol": "GEMI-DEMNOM2028-DEMNOM28AOC",
                "open": "0.2", "high": "0.22", "low": "0.18", "close": "0.18",
                "bid": "0.1900", "ask": "0.2000"
            })))
            .mount(&server)
            .await;

        let market = get_market(&state_for(&server), "GEMI-DEMNOM2028-DEMNOM28AOC")
            .await
            .expect("the contract resolves");

        assert_eq!(market.ticker, "GEMI-DEMNOM2028-DEMNOM28AOC");
        assert_eq!(market.event_ticker, "DEMNOM2028");
        // The book: quote and the resting depth nothing in the catalogue states.
        assert_eq!(market.yes_bid, Some(0.19));
        assert_eq!(market.yes_ask, Some(0.2));
        assert_eq!(market.liquidity, Some(1295.2));
        // Turnover is the event's and stays off a leg of a 3-leg event.
        assert_eq!(market.volume24h, None);
        assert_eq!(market.open_interest, None);
    }

    #[tokio::test]
    async fn get_market_survives_sidecars_that_will_not_answer() {
        let server = MockServer::start().await;
        mount_catalogue(&server).await;

        Mock::given(method("GET"))
            .and(path_matcher("/prediction-markets/DEMNOM2028"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(catalogue_fixture()["data"][0].clone()),
            )
            .mount(&server)
            .await;

        // No book and no ticker mounted: `/v2/ticker` 404s on an instrument that
        // has never traded, and neither may take the quote down with it.
        let market = get_market(&state_for(&server), "GEMI-DEMNOM2028-DEMNOM28AOC")
            .await
            .expect("the catalogue quote stands on its own");

        assert_eq!(market.yes_bid, Some(0.17));
        assert_eq!(market.yes_ask, Some(0.18));
        assert_eq!(market.liquidity, None);
    }

    #[tokio::test]
    async fn get_market_walks_the_symbol_prefixes_when_the_snapshot_cannot_answer() {
        // A leg of a settled event is not in a `status=active` crawl, and `EVT`
        // on that event hands the reader exactly those symbols, so refusing them
        // would make the terminal reject identifiers it had just printed.
        //
        // The walk starts one segment short of the whole symbol and never asks
        // for the whole of it. An `instrumentSymbol` is the event ticker plus a
        // contract ticker, so the full string cannot name an event — asking for
        // it would be a request guaranteed to 404. Where the *contract* half is
        // itself hyphenated the walk pays one wasted request, which is the case
        // this covers: `FED260917-HIKE` names no event, `FED260917` does.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/prediction-markets"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "data": [] })))
            .mount(&server)
            .await;

        Mock::given(method("GET"))
            .and(path_matcher("/prediction-markets/FED260917-HIKE-25"))
            .respond_with(ResponseTemplate::new(404).set_body_json(json!({ "error": "NOT_FOUND" })))
            .expect(0)
            .mount(&server)
            .await;

        Mock::given(method("GET"))
            .and(path_matcher("/prediction-markets/FED260917-HIKE"))
            .respond_with(ResponseTemplate::new(404).set_body_json(json!({ "error": "NOT_FOUND" })))
            .expect(1)
            .mount(&server)
            .await;

        Mock::given(method("GET"))
            .and(path_matcher("/prediction-markets/FED260917"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "ticker": "FED260917",
                "title": "Fed decision in September?",
                "type": "categorical",
                "contracts": [{
                    "label": "Hike 25bps",
                    "instrumentSymbol": "GEMI-FED260917-HIKE-25",
                    "prices": { "bestBid": "0.4", "bestAsk": "0.42" }
                }]
            })))
            .expect(1)
            .mount(&server)
            .await;

        let market = get_market(&state_for(&server), "GEMI-FED260917-HIKE-25")
            .await
            .expect("the prefix walk finds the owning event");
        assert_eq!(market.event_ticker, "FED260917");
        assert_eq!(market.yes_bid, Some(0.4));
    }
    #[tokio::test]
    async fn get_order_book_reads_the_whole_ladder_and_caps_it_here() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/v1/book/GEMI-DEMNOM2028-DEMNOM28AOC"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "bids": [
                    { "price": "0.19", "amount": "1530.0" },
                    { "price": "0.18", "amount": "25.0" },
                    { "price": "0.17", "amount": "788.0" }
                ],
                "asks": [{ "price": "0.2", "amount": "1250.0" }]
            })))
            // `limit_bids`/`limit_asks` are deliberately not sent, so one cached
            // snapshot serves a 5-level quote panel and a 50-level ladder alike.
            .expect(1)
            .mount(&server)
            .await;

        let state = state_for(&server);
        let book = get_order_book(&state, "GEMI-DEMNOM2028-DEMNOM28AOC", 2)
            .await
            .expect("a book");
        assert_eq!(book.yes.len(), 2);
        assert_eq!(book.best_yes_bid, Some(0.19));

        let deeper = get_order_book(&state, "GEMI-DEMNOM2028-DEMNOM28AOC", 50)
            .await
            .expect("the same snapshot, deeper");
        assert_eq!(deeper.yes.len(), 3);
    }

    #[tokio::test]
    async fn a_four_hundred_on_a_listed_leg_says_the_instrument_was_never_opened() {
        // api.gemini.com answers `is not a valid symbol` both for a typo and for
        // a leg it has never opened an instrument for, so the snapshot decides
        // which sentence the reader gets.
        let server = MockServer::start().await;
        mount_catalogue(&server).await;

        Mock::given(method("GET"))
            .and(path_matcher("/v1/book/GEMI-HORMUZNORMAL-HORMUZAUGUST1"))
            .respond_with(ResponseTemplate::new(400).set_body_json(json!({
                "result": "error",
                "reason": "Bad Request",
                "message": "Supplied value 'GEMI-HORMUZNORMAL-HORMUZAUGUST1' is not a valid symbol"
            })))
            .mount(&server)
            .await;

        let err = get_order_book(&state_for(&server), "GEMI-HORMUZNORMAL-HORMUZAUGUST1", 12)
            .await
            .expect_err("the trading host has no such instrument");

        assert_eq!(err.code, codes::NOT_FOUND);
        assert_eq!(
            err.message,
            "Gemini has opened no instrument for GEMI-HORMUZNORMAL-HORMUZAUGUST1"
        );
        let hint = err.hint.expect("the gap is explained");
        assert!(hint.contains("listed in the Gemini catalogue"));
        assert!(hint.contains("never been quoted and never traded"));
    }

    #[tokio::test]
    async fn a_four_hundred_on_an_unlisted_symbol_is_reported_as_a_typo() {
        let server = MockServer::start().await;
        mount_catalogue(&server).await;

        Mock::given(method("GET"))
            .and(path_matcher("/v1/book/GEMI-NOPE-NOPE"))
            .respond_with(ResponseTemplate::new(400).set_body_json(json!({
                "result": "error",
                "message": "Supplied value 'GEMI-NOPE-NOPE' is not a valid symbol"
            })))
            .mount(&server)
            .await;

        let err = get_order_book(&state_for(&server), "GEMI-NOPE-NOPE", 12)
            .await
            .expect_err("an unlisted symbol is a typo");

        assert_eq!(err.code, codes::NOT_FOUND);
        assert_eq!(err.message, "No Gemini contract GEMI-NOPE-NOPE");
        assert!(err.hint.unwrap().contains("instrument symbol"));
    }

    #[tokio::test]
    async fn get_trades_asks_for_the_page_it_was_given_and_never_past_the_venues_ceiling() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/v1/trades/GEMI-DEMNOM2028-DEMNOM28AOC"))
            // 1,000 is a 400 upstream, not a truncation, so the ask is clamped.
            .and(query_param("limit_trades", "500"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                {
                    "timestamp": 1787183859,
                    "timestampms": 1787183859270i64,
                    "tid": 1893456011072804i64,
                    "price": "0.18",
                    "amount": "13",
                    "type": "buy"
                }
            ])))
            .expect(1)
            .mount(&server)
            .await;

        let response = get_trades(&state_for(&server), "GEMI-DEMNOM2028-DEMNOM28AOC", 5_000)
            .await
            .expect("a tape");
        assert_eq!(response.trades.len(), 1);
        assert_eq!(response.trades[0].trade_id, "1893456011072804");
        // No cursor: the tape is replica-inconsistent and a tid walk skips
        // prints.
        assert!(response.cursor.is_none());
    }

    #[tokio::test]
    async fn get_candles_sends_the_venues_own_interval_name_and_millisecond_bounds() {
        let server = MockServer::start().await;
        mount_catalogue(&server).await;

        Mock::given(method("GET"))
            .and(path_matcher("/v2/klines"))
            .and(query_param("symbol", "GEMI-DEMNOM2028-DEMNOM28AOC"))
            // `1h` and `1min` are 400s upstream, not synonyms.
            .and(query_param("interval", "1hr"))
            .and(query_param("startTime", "1786752000000"))
            .and(query_param("endTime", "1787011200000"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                [1787004000000i64, 0.2, 0.22, 0.2, 0.22, 5467],
                [1787000400000i64, 0.21, 0.21, 0.19, 0.2, 0]
            ])))
            .expect(1)
            .mount(&server)
            .await;

        let response = get_candles(
            &state_for(&server),
            "GEMI-DEMNOM2028-DEMNOM28AOC",
            CandleInterval::OneHour,
            1_786_752_000,
            1_787_011_200,
        )
        .await
        .expect("bars");

        assert_eq!(response.venue, Venue::Gemini);
        assert_eq!(response.interval, CandleInterval::OneHour);
        // Reversed to oldest-first, and stamped at the period end.
        assert_eq!(response.candles[0].time, 1_787_000_400 + 3_600);
        assert!(!response.candles[0].traded);
        assert!(response.candles[1].traded);
        // Read out of the snapshot: DEMNOM2028 states no series, so its ticker
        // stripped of a date is the family name.
        assert_eq!(response.series_ticker, "DEMNOM2028");
        // True OHLC off the matching engine, so there is nothing to disclaim.
        assert!(response.note.is_none());
    }

    #[tokio::test]
    async fn list_series_derives_the_families_from_the_events_themselves() {
        let server = MockServer::start().await;
        mount_catalogue(&server).await;

        let series = list_series(&state_for(&server), None)
            .await
            .expect("a series list");

        let tickers: Vec<&str> = series.iter().map(|s| s.ticker.as_str()).collect();
        assert_eq!(
            tickers,
            vec![
                "BTC1H",
                "DEMNOM2028",
                "FED",
                "HORMUZNORMAL",
                "RECESSION26",
                "SEN26MI",
                "WXHIGH-CHI",
            ]
        );
        // The exchange states no cadence, and `BTC1H` is not a licence to write
        // "hourly" into a field the reader takes as the venue's own word.
        assert!(series.iter().all(|s| s.frequency.is_empty()));
        // The event's tags and its subcategory slug both reach the family, which
        // are two different spellings of the same thing upstream — the tag is
        // written for a reader and the slug for a machine.
        let weather = series.iter().find(|s| s.ticker == "WXHIGH-CHI").unwrap();
        assert_eq!(weather.category, "Weather");
        assert!(weather.tags.iter().any(|tag| tag == "Daily Temperature"));
        assert!(weather
            .tags
            .iter()
            .any(|tag| tag == "weather_daily-temperature"));
        assert_eq!(weather.venue, Venue::Gemini);
    }

    #[tokio::test]
    async fn list_series_narrows_to_a_category_over_the_same_snapshot_search_reads() {
        let server = MockServer::start().await;
        mount_catalogue(&server).await;

        let series = list_series(&state_for(&server), Some("  Crypto "))
            .await
            .expect("a series list");
        let tickers: Vec<&str> = series.iter().map(|s| s.ticker.as_str()).collect();
        assert_eq!(tickers, vec!["BTC1H"]);
    }

    #[tokio::test]
    async fn search_ranks_the_snapshot_rather_than_the_catalogues_own_parameter() {
        // The upstream `?search=` is semantic: it answers `zzzznotathing` with
        // two events rather than none.
        let server = MockServer::start().await;
        mount_catalogue(&server).await;

        let response = search(&state_for(&server), "democratic nominee", 25)
            .await
            .expect("a search");
        assert_eq!(response.query, "democratic nominee");
        assert!(response
            .hits
            .iter()
            .any(|hit| hit.event.event_ticker == "DEMNOM2028"));
    }

    #[tokio::test]
    async fn search_refuses_a_query_with_more_words_than_it_will_match() {
        let server = MockServer::start().await;
        // No mock at all: the refusal happens before the crawl is asked for.
        let err = search(&state_for(&server), &"fed ".repeat(40), 25)
            .await
            .expect_err("an overlong query is refused");
        assert_eq!(err.code, codes::BAD_REQUEST);
    }

    #[tokio::test]
    async fn top_markets_ranks_the_movers_on_each_legs_own_change() {
        let server = MockServer::start().await;
        mount_catalogue(&server).await;
        let state = state_for(&server);

        let gainers = top_markets(&state, MoverSort::Gainers, 25)
            .await
            .expect("a board");
        let losers = top_markets(&state, MoverSort::Losers, 25)
            .await
            .expect("a board");

        assert!(!gainers.is_empty());
        // Biggest rise first, biggest fall first.
        for pair in gainers.windows(2) {
            assert!(pair[0].change.unwrap() >= pair[1].change.unwrap());
        }
        for pair in losers.windows(2) {
            assert!(pair[0].change.unwrap() <= pair[1].change.unwrap());
        }
        assert_eq!(
            gainers.first().map(|m| m.ticker.as_str()),
            losers.last().map(|m| m.ticker.as_str())
        );
    }

    #[tokio::test]
    async fn top_markets_gates_the_movers_on_the_parent_events_turnover() {
        // Turnover is stated per event, so a leg never carries one: gating on the
        // leg would empty the board, and gating on nothing would rank a stale
        // print as a move. WXHIGH-CHI traded nothing in 24h and is left out.
        let server = MockServer::start().await;
        mount_catalogue(&server).await;

        let gainers = top_markets(&state_for(&server), MoverSort::Gainers, 25)
            .await
            .expect("a board");
        assert!(gainers
            .iter()
            .all(|m| m.event_ticker != "WXHIGH-CHI-2608210359"));
        assert!(gainers.iter().any(|m| m.event_ticker == "DEMNOM2028"));
    }

    #[tokio::test]
    async fn top_markets_leaves_the_three_boards_the_registry_does_not_declare_empty() {
        // There is no open interest anywhere in this API, no resting-depth
        // figure outside the per-contract book, and no per-contract turnover.
        let server = MockServer::start().await;
        mount_catalogue(&server).await;
        let state = state_for(&server);

        for sort in [
            MoverSort::Volume,
            MoverSort::OpenInterest,
            MoverSort::Liquidity,
        ] {
            let markets = top_markets(&state, sort, 25).await.expect("a board");
            assert!(markets.is_empty(), "{sort}");
        }
    }
}
