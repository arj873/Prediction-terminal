//! Polymarket International (polymarket.com) client.
//!
//! Three public hosts, because one does not carry everything:
//!
//!   gamma-api    catalogue — events, markets, series, prices, volume
//!   clob         the live book, and the price history behind every chart
//!   data-api     the public print tape
//!
//! The dialect differs from Kalshi's in three ways worth knowing before reading
//! the normalisers. Prices arrive as JSON *numbers* but the arrays beside them
//! (`outcomes`, `outcomePrices`, `clobTokenIds`) arrive as JSON *strings* that
//! have to be parsed a second time. There is no NO book: a Polymarket market is
//! a pair of ERC-1155 tokens and the terminal quotes the first one, deriving the
//! NO side from it. And the CLOB identifies a market by a 77-digit token id,
//! never by the slug a human types — so anything touching the book or the chart
//! resolves the slug through the catalogue first.

use std::collections::BTreeMap;
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use futures::StreamExt;
use regex::Regex;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer};
use serde_json::Value;
use terminal_core::types::{
    BookLevel, Candle, CandleInterval, CandlesResponse, Market, MarketStatus, OrderBook,
    SeriesInfo, Trade, TradesResponse, Venue, VenueEvent,
};
use terminal_core::util::round4;
use terminal_core::venue::MoverSort;

use crate::app::AppState;
use crate::cache::ttl;
use crate::error::{Result, UpstreamError};
use crate::http::FetchOptions;
use crate::sources::corpus::{
    rank_markets, refuse_overlong_query, search_corpus, Corpus, SearchResponse,
};

const VENUE: Venue = Venue::Polymarket;

/// Per-attempt budget for a catalogue, book or history call.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
/// The corpus crawl asks for 100 events at a time and gamma takes its time.
const CORPUS_TIMEOUT: Duration = Duration::from_secs(30);
const RETRIES: u32 = 2;

/// Levels of each ladder returned by [`get_order_book`] unless asked otherwise.
pub const DEFAULT_BOOK_DEPTH: usize = 12;
/// Prints returned by [`get_trades`] unless asked otherwise.
pub const DEFAULT_TRADE_LIMIT: usize = 50;
/// Rows returned by [`search`] and [`top_markets`] unless asked otherwise.
pub const DEFAULT_RESULT_LIMIT: usize = 25;

/* --------------------------------------------------------------- coercion */

/// The `Number(value)` the TypeScript leans on, with its `null`/`''` guards.
///
/// A value that is absent, empty or not a finite number is *unstated*, never
/// zero — several of these figures legitimately read `0`.
fn num(value: &Value) -> Option<f64> {
    match value {
        Value::Null => None,
        Value::String(text) => num_text(text),
        Value::Number(number) => number.as_f64().filter(|n| n.is_finite()),
        Value::Bool(flag) => Some(if *flag { 1.0 } else { 0.0 }),
        _ => None,
    }
}

/// [`num`] for a figure that arrived as text.
fn num_text(text: &str) -> Option<f64> {
    if text.is_empty() {
        return None;
    }
    let trimmed = text.trim();
    if trimmed.is_empty() {
        // `Number('  ')` is 0, and only the exactly-empty string is guarded
        // above.
        return Some(0.0);
    }
    trimmed.parse::<f64>().ok().filter(|n| n.is_finite())
}

/// [`num`] for a figure that is already a number.
fn num_of(value: f64) -> Option<f64> {
    value.is_finite().then_some(value)
}

/// A quote of exactly 0 means "nothing rests on this side".
///
/// Polymarket's minimum tick is 0.001, so a real resting order can never be at
/// zero, and gamma reports an empty side as `0`. A quote of exactly 1 is *not*
/// given the same treatment — it is a legitimate price for a contract the market
/// has already made up its mind about.
fn price(value: &Value) -> Option<f64> {
    num(value).filter(|n| *n != 0.0)
}

/// [`price`] for a figure that is already a number.
fn price_of(value: f64) -> Option<f64> {
    num_of(value).filter(|n| *n != 0.0)
}

/// The `String(value)` that `parseStringArray` maps over an array.
fn js_string(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => "null".to_owned(),
        Value::Bool(flag) => flag.to_string(),
        Value::Number(number) => match number.as_f64() {
            // JavaScript prints a whole float without its `.0`.
            Some(n) if n.fract() == 0.0 && n.abs() < 1e15 => format!("{}", n as i64),
            _ => number.to_string(),
        },
        other => other.to_string(),
    }
}

/// Parse one of gamma's stringified JSON arrays.
///
/// `outcomes`, `outcomePrices` and `clobTokenIds` all arrive as strings holding
/// JSON — `"[\"Yes\", \"No\"]"` — rather than as arrays. Occasionally one is
/// already an array; both shapes are accepted so a fixed upstream would not
/// break this.
pub fn parse_string_array(value: &Value) -> Vec<String> {
    if let Value::Array(items) = value {
        return items.iter().map(js_string).collect();
    }
    let Value::String(text) = value else {
        return Vec::new();
    };
    if text.is_empty() {
        return Vec::new();
    }
    match serde_json::from_str::<Value>(text) {
        Ok(Value::Array(items)) => items.iter().map(js_string).collect(),
        _ => Vec::new(),
    }
}

/* ------------------------------------------------------------ raw upstream */

/// Gamma's catalogue record for one contract.
///
/// The money and array fields are held as [`Value`] rather than as typed fields
/// because gamma genuinely ships more than one shape for each of them — a price
/// as a number or a decimal string, an outcome list as an array or as a string
/// holding an array — and a strict deserialiser would turn that into a 502.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RawGammaMarket {
    pub id: Option<String>,
    pub slug: Option<String>,
    pub question: Option<String>,
    pub condition_id: Option<String>,
    pub group_item_title: Option<String>,
    pub description: Option<String>,
    pub outcomes: Value,
    pub outcome_prices: Value,
    pub clob_token_ids: Value,
    pub best_bid: Value,
    pub best_ask: Value,
    pub last_trade_price: Value,
    pub one_day_price_change: Value,
    pub volume_num: Value,
    pub volume: Value,
    pub volume24hr: Value,
    pub liquidity_num: Value,
    pub liquidity: Value,
    pub start_date: Option<String>,
    pub end_date: Option<String>,
    pub end_date_iso: Option<String>,
    pub closed_time: Option<String>,
    pub active: Option<bool>,
    pub closed: Option<bool>,
    pub archived: Option<bool>,
    pub accepting_orders: Option<bool>,
    pub events: Option<Vec<RawGammaEvent>>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RawGammaEvent {
    pub id: Option<String>,
    pub ticker: Option<String>,
    pub slug: Option<String>,
    pub title: Option<String>,
    pub description: Option<String>,
    pub series_slug: Option<String>,
    pub series: Option<Vec<RawGammaSeries>>,
    pub neg_risk: Option<bool>,
    pub closed: Option<bool>,
    pub active: Option<bool>,
    pub volume24hr: Value,
    pub tags: Option<Vec<RawGammaTag>>,
    pub markets: Option<Vec<RawGammaMarket>>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RawGammaSeries {
    pub slug: Option<String>,
    pub title: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RawGammaTag {
    /// A decimal string in every payload seen, but read through [`num`] because
    /// the ordering below is the only thing that depends on it.
    pub id: Value,
    pub slug: Option<String>,
    pub label: Option<String>,
}

/* ------------------------------------------------------------ normalisers */

/// The event a market belongs to, as the normalisers name it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Parent {
    pub event_ticker: String,
    pub series_ticker: String,
}

/// The series a Polymarket event belongs to.
///
/// Gamma states one on most recurring events (`fomc`, `nfl`, `bitcoin-price`),
/// which is the identifier that lines up with a Kalshi series ticker. Where it
/// does not, the event slug's stem is the best available stand-in: Polymarket
/// suffixes repeat listings with a date or a random tail
/// (`fed-decision-in-january-20260729233815502`), and stripping that leaves a
/// label that at least groups the same question together.
pub fn series_from_event(raw: &RawGammaEvent) -> String {
    let stated = raw.series_slug.as_deref().or_else(|| {
        raw.series
            .as_deref()
            .and_then(|rows| rows.first())
            .and_then(|first| first.slug.as_deref())
    });
    if let Some(stated) = stated {
        return stated.to_owned();
    }
    series_from_slug(raw.ticker.as_deref().or(raw.slug.as_deref()).unwrap_or(""))
}

/// A long digit run is a creation timestamp, not part of the name.
static TIMESTAMP_TAIL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"-\d{6,}$").expect("TIMESTAMP_TAIL is a valid regex"));
/// …and a short one is the "-762" style collision suffix.
static COLLISION_TAIL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"-\d{1,5}$").expect("COLLISION_TAIL is a valid regex"));
static TRAILING_DASHES: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"-+$").expect("TRAILING_DASHES is a valid regex"));

/// Strip the disambiguating tail Polymarket appends to a repeated listing.
pub fn series_from_slug(slug: &str) -> String {
    let stripped = TIMESTAMP_TAIL.replace(slug, "");
    let stripped = COLLISION_TAIL.replace(&stripped, "").into_owned();
    let stripped = TRAILING_DASHES.replace(&stripped, "").into_owned();
    if stripped.is_empty() {
        slug.to_owned()
    } else {
        stripped
    }
}

fn status_of(raw: &RawGammaMarket) -> MarketStatus {
    if raw.closed == Some(true) {
        return "settled".to_owned();
    }
    if raw.archived == Some(true) {
        return "closed".to_owned();
    }
    if raw.active == Some(false) {
        return "unopened".to_owned();
    }
    if raw.accepting_orders == Some(false) {
        return "closed".to_owned();
    }
    "open".to_owned()
}

/// Which way a settled market resolved.
///
/// Gamma states no winner field; a resolved market simply prints its outcome
/// prices as `["0", "1"]`. Read only when the market is closed, so a contract
/// trading at 99.5¢ is never reported as already settled.
fn result_of(raw: &RawGammaMarket, prices: &[f64]) -> String {
    if raw.closed != Some(true) {
        return String::new();
    }
    if prices.first() == Some(&1.0) {
        return "yes".to_owned();
    }
    if prices.get(1) == Some(&1.0) {
        return "no".to_owned();
    }
    String::new()
}

pub fn normalise_market(raw: &RawGammaMarket, parent: Option<&Parent>) -> Market {
    let owned_parent;
    let event = match parent {
        Some(parent) => parent,
        None => {
            owned_parent = parent_of(raw);
            &owned_parent
        }
    };

    let outcomes = parse_string_array(&raw.outcomes);
    let prices: Vec<f64> = parse_string_array(&raw.outcome_prices)
        .iter()
        .map(|p| num_text(p).unwrap_or(0.0))
        .collect();

    let yes_bid = price(&raw.best_bid);
    let yes_ask = price(&raw.best_ask);
    // `outcomePrices[0]` is gamma's own mark for the YES token — the last print
    // when there is one, the midpoint when there is not.
    let last = price(&raw.last_trade_price).or_else(|| prices.first().copied().and_then(price_of));
    let change = num(&raw.one_day_price_change);

    let mid = match (yes_bid, yes_ask) {
        (Some(bid), Some(ask)) => Some((bid + ask) / 2.0),
        _ => last.or(yes_bid).or(yes_ask),
    };
    let close_time = raw
        .end_date
        .clone()
        .or_else(|| raw.end_date_iso.clone())
        .unwrap_or_default();

    Market {
        venue: VENUE,
        ticker: raw.slug.clone().unwrap_or_default(),
        event_ticker: event.event_ticker.clone(),
        series_ticker: event.series_ticker.clone(),
        title: raw
            .question
            .clone()
            .or_else(|| raw.slug.clone())
            .unwrap_or_default(),
        // Within a grouped event the outcome's own label is the strike; a
        // standalone binary market only has "Yes".
        yes_sub_title: first_non_empty(&[
            raw.group_item_title.as_deref().unwrap_or(""),
            outcomes.first().map(String::as_str).unwrap_or(""),
            "Yes",
        ]),
        no_sub_title: outcomes.get(1).cloned().unwrap_or_else(|| "No".to_owned()),
        status: status_of(raw),
        market_type: "binary".to_owned(),
        yes_bid,
        yes_ask,
        // Polymarket runs one book per token pair and quotes only the first token.
        // An offer to sell YES at `p` is a bid to buy NO at `1 - p`, so the NO side
        // is derived here rather than left blank.
        no_bid: yes_ask.map(|ask| round4(1.0 - ask)),
        no_ask: yes_bid.map(|bid| round4(1.0 - bid)),
        mid: mid.map(round4),
        last_price: last,
        previous_price: match (last, change) {
            (Some(last), Some(change)) => Some(round4(last - change)),
            _ => None,
        },
        change,
        volume: num(&raw.volume_num).or_else(|| num(&raw.volume)),
        volume24h: num(&raw.volume24hr),
        // Gamma reports open interest per *event*, never per market, and the
        // event's figure is not this contract's. Left unstated rather than guessed.
        open_interest: None,
        liquidity: num(&raw.liquidity_num).or_else(|| num(&raw.liquidity)),
        open_time: raw.start_date.clone().unwrap_or_default(),
        close_time: close_time.clone(),
        expiration_time: raw.closed_time.clone().unwrap_or(close_time),
        result: result_of(raw, &prices),
        rules_primary: raw.description.clone().unwrap_or_default(),
        category: None,
        // Polymarket words its strikes into the question ("Will BTC be above
        // $120,000…"), and never states them as numbers. Parsing one out of the
        // prose would feed the implied-price maths a guess, so nothing is claimed.
        strike_type: None,
        floor_strike: None,
        cap_strike: None,
    }
}

/// The `a || b || c` the YES label is chosen with: the first *non-empty* one.
fn first_non_empty(candidates: &[&str]) -> String {
    candidates
        .iter()
        .copied()
        .find(|candidate| !candidate.is_empty())
        .unwrap_or("")
        .to_owned()
}

/// A market fetched on its own still carries its parent event inline.
fn parent_of(raw: &RawGammaMarket) -> Parent {
    let Some(event) = raw.events.as_deref().and_then(|events| events.first()) else {
        return Parent {
            event_ticker: String::new(),
            series_ticker: series_from_slug(raw.slug.as_deref().unwrap_or("")),
        };
    };
    Parent {
        event_ticker: event
            .ticker
            .clone()
            .or_else(|| event.slug.clone())
            .unwrap_or_default(),
        series_ticker: series_from_event(event),
    }
}

/// The closest thing gamma has to Kalshi's category.
///
/// There is no category field — only a flat bag of tags, in no useful order:
/// the FOMC event's first tag is `fomc` and its fifth is `Politics`. But tag ids
/// are issued in sequence, and the top-level ones were created first, so the
/// lowest id in the bag is the broadest label. `Sports` is 1, `Politics` is 2;
/// `fomc` is 100478.
fn category_of(raw: &RawGammaEvent) -> String {
    let mut best: Option<(f64, String)> = None;
    for tag in raw.tags.as_deref().unwrap_or(&[]) {
        let label = tag
            .label
            .clone()
            .or_else(|| tag.slug.clone())
            .unwrap_or_default();
        let Some(id) = num(&tag.id) else { continue };
        if label.is_empty() {
            continue;
        }
        if best.as_ref().is_none_or(|(best_id, _)| id < *best_id) {
            best = Some((id, label));
        }
    }
    best.map(|(_, label)| label).unwrap_or_default()
}

pub fn normalise_event(raw: &RawGammaEvent) -> VenueEvent {
    let event_ticker = raw
        .ticker
        .clone()
        .or_else(|| raw.slug.clone())
        .unwrap_or_default();
    let series_ticker = series_from_event(raw);
    let parent = Parent {
        event_ticker: event_ticker.clone(),
        series_ticker: series_ticker.clone(),
    };

    VenueEvent {
        venue: VENUE,
        event_ticker: event_ticker.clone(),
        series_ticker,
        title: raw.title.clone().unwrap_or(event_ticker),
        sub_title: raw
            .series
            .as_deref()
            .and_then(|rows| rows.first())
            .and_then(|first| first.title.clone())
            .unwrap_or_default(),
        category: category_of(raw),
        // `negRisk` is Polymarket's name for a ladder whose legs cannot both win —
        // exactly Kalshi's mutually-exclusive flag.
        mutually_exclusive: raw.neg_risk.unwrap_or(false),
        markets: raw
            .markets
            .as_deref()
            .unwrap_or(&[])
            .iter()
            .map(|market| normalise_market(market, Some(&parent)))
            .collect(),
    }
}

/* ---------------------------------------------------------------- queries */

/// `URLSearchParams`: alphanumerics and `*-._` survive, a space becomes `+`,
/// everything else is percent-encoded.
fn form_encode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for byte in input.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'*' | b'-' | b'.' | b'_' => {
                out.push(byte as char);
            }
            b' ' => out.push('+'),
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// A query string in the order the parameters are written, skipping the unset.
fn qs(params: &[(&str, String)]) -> String {
    let mut out = String::new();
    for (key, value) in params {
        if value.is_empty() {
            continue;
        }
        out.push(if out.is_empty() { '?' } else { '&' });
        out.push_str(&form_encode(key));
        out.push('=');
        out.push_str(&form_encode(value));
    }
    out
}

async fn get<T>(state: &AppState, base: &str, path: &str, ttl: Duration) -> Result<Arc<T>>
where
    T: DeserializeOwned + Send + Sync + 'static,
{
    let url = format!("{base}{path}");
    state
        .cache()
        .cached(&format!("polymarket:{base}{path}"), ttl, || async {
            state
                .http()
                .fetch_json::<T>(
                    &url,
                    FetchOptions::new()
                        .timeout(REQUEST_TIMEOUT)
                        .retries(RETRIES),
                )
                .await
        })
        .await
}

/* ------------------------------------------------------------------ lookups */

/// The raw catalogue record for a market slug.
async fn raw_market(state: &AppState, slug: &str) -> Result<RawGammaMarket> {
    let path = format!("/markets{}", qs(&[("slug", slug.to_owned())]));
    let rows = get::<Vec<RawGammaMarket>>(
        state,
        &state.config().polymarket_gamma_base,
        &path,
        ttl::QUOTE,
    )
    .await?;

    rows.first().cloned().ok_or_else(|| {
        UpstreamError::not_found(format!("No Polymarket market with slug {slug}")).with_hint(
            "Polymarket identifies markets by slug, as in the tail of its URL. Try `SRCH` to find one.",
        )
    })
}

pub async fn get_market(state: &AppState, slug: &str) -> Result<Market> {
    Ok(normalise_market(&raw_market(state, slug).await?, None))
}

pub async fn get_event(state: &AppState, slug: &str) -> Result<VenueEvent> {
    let path = format!("/events{}", qs(&[("slug", slug.to_owned())]));
    let rows = get::<Vec<RawGammaEvent>>(
        state,
        &state.config().polymarket_gamma_base,
        &path,
        ttl::QUOTE,
    )
    .await?;

    let row = rows
        .first()
        .ok_or_else(|| UpstreamError::not_found(format!("No Polymarket event with slug {slug}")))?;
    Ok(normalise_event(row))
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct RawGammaSeriesRow {
    slug: Option<String>,
    ticker: Option<String>,
    title: Option<String>,
    recurrence: Option<String>,
    series_type: Option<String>,
}

pub async fn list_series(state: &AppState) -> Result<Vec<SeriesInfo>> {
    let path = format!(
        "/series{}",
        qs(&[("limit", "100".to_owned()), ("closed", "false".to_owned()),])
    );
    let rows = get::<Vec<RawGammaSeriesRow>>(
        state,
        &state.config().polymarket_gamma_base,
        &path,
        ttl::CATALOGUE,
    )
    .await?;

    Ok(rows
        .iter()
        .map(|s| SeriesInfo {
            venue: VENUE,
            ticker: s
                .slug
                .clone()
                .or_else(|| s.ticker.clone())
                .unwrap_or_default(),
            title: s
                .title
                .clone()
                .or_else(|| s.slug.clone())
                .unwrap_or_default(),
            category: s.series_type.clone().unwrap_or_default(),
            frequency: s.recurrence.clone().unwrap_or_default(),
            tags: Vec::new(),
        })
        .collect())
}

/// The YES token id for a market slug.
///
/// Everything on the CLOB — the book, the price history — is keyed by token id,
/// and nothing in the terminal's command surface carries one. Held for the
/// metadata TTL because the mapping is immutable once a market is deployed.
async fn yes_token_id(state: &AppState, slug: &str) -> Result<Arc<String>> {
    state
        .cache()
        .cached(&format!("polymarket:token:{slug}"), ttl::META, || async {
            let raw = raw_market(state, slug).await?;
            parse_string_array(&raw.clob_token_ids)
                .into_iter()
                .next()
                .filter(|token| !token.is_empty())
                .ok_or_else(|| {
                    UpstreamError::not_found(format!(
                        "Polymarket market {slug} has no order book"
                    ))
                    .with_hint(
                        "This market was never deployed to the CLOB, so it has no book or price history.",
                    )
                })
        })
        .await
}

/* -------------------------------------------------------------------- book */

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct RawClobBook {
    pub bids: Option<Vec<RawClobLevel>>,
    pub asks: Option<Vec<RawClobLevel>>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct RawClobLevel {
    pub price: Value,
    pub size: Value,
}

/// Turn the CLOB's two ladders into the terminal's book.
///
/// Unlike Kalshi, Polymarket quotes a genuine bid and ask on one token, so no
/// inversion is needed to build the ask side. The NO ladder is the one that has
/// to be derived: a resting offer to sell YES at `p` is a bid to buy NO at
/// `1 - p`, and traders comparing this book against a Kalshi one expect to see it.
pub fn normalise_order_book(raw: &RawClobBook, ticker: &str, depth: usize) -> OrderBook {
    let rows = |side: Option<&Vec<RawClobLevel>>| -> Vec<BookLevel> {
        side.map(Vec::as_slice)
            .unwrap_or(&[])
            .iter()
            .map(|level| BookLevel {
                price: num(&level.price).unwrap_or(0.0),
                size: num(&level.size).unwrap_or(0.0),
            })
            .filter(|level| level.price > 0.0 && level.size > 0.0)
            .collect()
    };

    let mut yes = rows(raw.bids.as_ref());
    yes.sort_by(|a, b| b.price.total_cmp(&a.price));
    yes.truncate(depth);

    let mut yes_asks = rows(raw.asks.as_ref());
    yes_asks.sort_by(|a, b| a.price.total_cmp(&b.price));
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
        ticker: ticker.to_owned(),
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

pub async fn get_order_book(state: &AppState, slug: &str, depth: usize) -> Result<OrderBook> {
    let token = yes_token_id(state, slug).await?;
    let path = format!("/book{}", qs(&[("token_id", token.to_string())]));
    let raw = get::<RawClobBook>(
        state,
        &state.config().polymarket_clob_base,
        &path,
        ttl::QUOTE,
    )
    .await?;
    Ok(normalise_order_book(&raw, slug, depth))
}

/* ------------------------------------------------------------------ trades */

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RawDataTrade {
    pub transaction_hash: Option<String>,
    pub timestamp: Value,
    pub price: Value,
    pub size: Value,
    pub side: Option<String>,
    pub outcome_index: Value,
    pub outcome: Option<String>,
}

/// Normalise the public print tape.
///
/// Every trade is reported from the point of view of the outcome token that
/// changed hands, so a `BUY` of the NO token at 26¢ and a `SELL` of the YES
/// token at 74¢ are the same print seen from two sides. Both are restated in
/// YES terms — price and aggressor alike — so the tape reads the same way as
/// Kalshi's beside it.
pub fn normalise_trades(raw: &[RawDataTrade], ticker: &str) -> TradesResponse {
    let trades = raw
        .iter()
        .enumerate()
        .map(|(index, t)| {
            let is_yes_token = num(&t.outcome_index).unwrap_or(0.0) == 0.0;
            let traded = num(&t.price).unwrap_or(0.0);
            let yes_price = if is_yes_token {
                traded
            } else {
                round4(1.0 - traded)
            };
            let bought_yes = if t.side.as_deref().unwrap_or("BUY").to_uppercase() == "BUY" {
                is_yes_token
            } else {
                !is_yes_token
            };

            Trade {
                venue: VENUE,
                // The hash identifies the transaction, not the fill: one transaction can
                // sweep several levels, so the index keeps sibling prints distinct.
                trade_id: format!(
                    "{}-{index}",
                    t.transaction_hash.as_deref().unwrap_or("trade")
                ),
                ticker: ticker.to_owned(),
                ts: num(&t.timestamp).unwrap_or(0.0).floor() as i64,
                count: num(&t.size).unwrap_or(0.0),
                yes_price,
                no_price: round4(1.0 - yes_price),
                taker_side: if bought_yes { "yes" } else { "no" }.to_owned(),
                is_block_trade: false,
            }
        })
        .collect();

    TradesResponse {
        trades,
        cursor: None,
    }
}

pub async fn get_trades(state: &AppState, slug: &str, limit: usize) -> Result<TradesResponse> {
    let raw = raw_market(state, slug).await?;
    let Some(condition_id) = raw.condition_id.as_deref().filter(|id| !id.is_empty()) else {
        return Err(UpstreamError::not_found(format!(
            "Polymarket market {slug} has no on-chain condition"
        )));
    };

    let path = format!(
        "/trades{}",
        qs(&[
            ("market", condition_id.to_owned()),
            ("limit", limit.clamp(1, 500).to_string()),
        ])
    );
    let rows = get::<Vec<RawDataTrade>>(
        state,
        &state.config().polymarket_data_base,
        &path,
        ttl::QUOTE,
    )
    .await?;
    Ok(normalise_trades(&rows, slug))
}

/* ----------------------------------------------------------------- candles */

/// One price sample from the CLOB's history feed.
#[derive(Debug, Clone, Copy, Deserialize)]
pub struct RawHistoryPoint {
    /// Unix seconds.
    #[serde(default = "not_a_number", deserialize_with = "lenient_number")]
    pub t: f64,
    /// Price of the YES token at that moment, 0..1.
    #[serde(default = "not_a_number", deserialize_with = "lenient_number")]
    pub p: f64,
}

fn not_a_number() -> f64 {
    f64::NAN
}

/// Read a figure the way [`num`] does, leaving anything unusable as `NaN` so the
/// bucketer drops the point rather than emitting a bar built from it.
fn lenient_number<'de, D: Deserializer<'de>>(de: D) -> std::result::Result<f64, D::Error> {
    let value = Value::deserialize(de)?;
    Ok(num(&value).unwrap_or(f64::NAN))
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct RawHistoryResponse {
    history: Option<Vec<RawHistoryPoint>>,
}

/// The widest window the CLOB will serve against explicit timestamps.
///
/// Measured, not documented: `startTs`/`endTs` spans of exactly 1,296,000s
/// return data at every fidelity and one second more returns an empty history
/// with a 200. Silently empty is the worst possible failure mode for a chart, so
/// requests are shaped to stay inside it.
const HISTORY_MAX_SPAN: i64 = 15 * 86400;

/// Chunks of [`HISTORY_MAX_SPAN`] to stitch before falling back.
const HISTORY_MAX_CHUNKS: i64 = 8;

/// Concurrent history requests. Enough to be quick, not enough to flood.
const HISTORY_FAN_OUT: usize = 4;

/// Lookback windows the CLOB accepts in place of timestamps, longest last.
///
/// These are ranges ending *now*, not bucket sizes — `fidelity` still decides
/// the bucket. They are the only way to reach past the timestamp ceiling.
const HISTORY_INTERVALS: &[(&str, f64)] = &[
    ("1d", 86_400.0),
    ("1w", 7.0 * 86_400.0),
    ("1m", 31.0 * 86_400.0),
    ("max", f64::INFINITY),
];

async fn history_chunk(
    state: &AppState,
    token: &str,
    fidelity: u16,
    start_ts: i64,
    end_ts: i64,
) -> Result<Vec<RawHistoryPoint>> {
    let path = format!(
        "/prices-history{}",
        qs(&[
            ("market", token.to_owned()),
            ("startTs", start_ts.to_string()),
            ("endTs", end_ts.to_string()),
            ("fidelity", fidelity.to_string()),
        ])
    );
    let raw = get::<RawHistoryResponse>(
        state,
        &state.config().polymarket_clob_base,
        &path,
        ttl::CANDLES,
    )
    .await?;
    Ok(raw.history.clone().unwrap_or_default())
}

/// The points covering `[start_ts, end_ts]`, and the caveat when they are not
/// the points that were asked for.
struct HistoryWindow {
    points: Vec<RawHistoryPoint>,
    note: Option<String>,
}

async fn history_window(
    state: &AppState,
    token: &str,
    fidelity: u16,
    start_ts: i64,
    end_ts: i64,
) -> Result<HistoryWindow> {
    let span = end_ts - start_ts;
    let chunks = (span as f64 / HISTORY_MAX_SPAN as f64).ceil();

    if chunks <= HISTORY_MAX_CHUNKS as f64 {
        let mut windows: Vec<(i64, i64)> = Vec::new();
        let mut from = start_ts;
        while from < end_ts {
            windows.push((from, (from + HISTORY_MAX_SPAN).min(end_ts)));
            from += HISTORY_MAX_SPAN;
        }

        let fetched: Vec<Result<Vec<RawHistoryPoint>>> = futures::stream::iter(
            windows
                .into_iter()
                .map(|(from, to)| history_chunk(state, token, fidelity, from, to)),
        )
        .buffered(HISTORY_FAN_OUT)
        .collect()
        .await;

        let mut points = Vec::new();
        for chunk in fetched {
            points.extend(chunk?);
        }
        return Ok(HistoryWindow { points, note: None });
    }

    // Too wide to request by timestamp. Ask for the nearest lookback window that
    // covers it and clip — and say so, because the upstream also caps how many
    // points a lookback returns, so a long hourly chart really can come back short.
    let lookback = HISTORY_INTERVALS
        .iter()
        .find(|entry| entry.1 >= span as f64)
        .unwrap_or_else(|| {
            HISTORY_INTERVALS
                .last()
                .expect("HISTORY_INTERVALS is non-empty")
        });

    let path = format!(
        "/prices-history{}",
        qs(&[
            ("market", token.to_owned()),
            ("interval", lookback.0.to_owned()),
            ("fidelity", fidelity.to_string()),
        ])
    );
    let raw = get::<RawHistoryResponse>(
        state,
        &state.config().polymarket_clob_base,
        &path,
        ttl::CANDLES,
    )
    .await?;

    let points = raw
        .history
        .clone()
        .unwrap_or_default()
        .into_iter()
        .filter(|point| point.t >= start_ts as f64 && point.t <= end_ts as f64)
        .collect();

    Ok(HistoryWindow {
        points,
        note: Some(format!(
            "Polymarket serves windows wider than 15 days only as a \"{}\" lookback \
             from now, and caps how much it returns — this chart may start later than requested.",
            lookback.0
        )),
    })
}

/// Bucket a price sample series into candles on the terminal's grid.
///
/// Polymarket publishes prices, not OHLC: each point is the YES token's price at
/// a moment. Open, high, low and close are therefore the first, highest, lowest
/// and last *sample* in the bucket, which is what a candle means when the
/// underlying is a quote series — and volume stays `None`, because no size is
/// attached to any of it.
///
/// Buckets are stamped with their period *end*, matching Kalshi, so both venues'
/// candles for the same question land on the same x-axis.
pub fn bucket_history(points: &[RawHistoryPoint], interval: CandleInterval) -> Vec<Candle> {
    let seconds = interval.seconds();
    let seconds_f = seconds as f64;

    struct Bucket {
        open: f64,
        high: f64,
        low: f64,
        close: f64,
    }
    // Ordered, so the times below come out ascending without a second sort.
    let mut buckets: BTreeMap<i64, Bucket> = BTreeMap::new();

    for point in points {
        let (Some(p), Some(t)) = (num_of(point.p), num_of(point.t)) else {
            continue;
        };

        // A sample at exactly a boundary closes the period it ends, not the next.
        let ceiled = (t / seconds_f).ceil() * seconds_f;
        let end = if ceiled == 0.0 { t } else { ceiled } as i64;

        buckets
            .entry(end)
            .and_modify(|bucket| {
                bucket.high = bucket.high.max(p);
                bucket.low = bucket.low.min(p);
                bucket.close = p;
            })
            .or_insert(Bucket {
                open: p,
                high: p,
                low: p,
                close: p,
            });
    }

    let mut candles: Vec<Candle> = Vec::new();
    let mut previous: Option<(i64, f64)> = None;

    for (time, bucket) in &buckets {
        // Fill the gap to the next observed bucket with flat carried-forward bars,
        // so a quiet market draws a continuous line rather than a jump. Bounded so a
        // market that stopped printing months ago cannot generate a million bars.
        if let Some((previous_time, previous_close)) = previous {
            let missing = ((time - previous_time) / seconds - 1).min(MAX_CARRY_FORWARD);
            for step in 1..=missing {
                candles.push(flat_candle(previous_time + step * seconds, previous_close));
            }
        }

        candles.push(Candle {
            time: *time,
            open: bucket.open,
            high: bucket.high,
            low: bucket.low,
            close: bucket.close,
            volume: None,
            open_interest: None,
            traded: true,
            bid: None,
            ask: None,
        });
        previous = Some((*time, bucket.close));
    }

    candles
}

/// Flat bars to bridge one gap. Beyond this the gap is left as a gap.
const MAX_CARRY_FORWARD: i64 = 512;

fn flat_candle(time: i64, close: f64) -> Candle {
    Candle {
        time,
        open: close,
        high: close,
        low: close,
        close,
        volume: None,
        open_interest: None,
        traded: false,
        bid: None,
        ask: None,
    }
}

pub async fn get_candles(
    state: &AppState,
    slug: &str,
    interval: CandleInterval,
    start_ts: i64,
    end_ts: i64,
) -> Result<CandlesResponse> {
    let (token, market) =
        futures::future::join(yes_token_id(state, slug), raw_market(state, slug)).await;
    let (token, market) = (token?, market?);
    let window = history_window(state, &token, interval.minutes(), start_ts, end_ts).await?;

    Ok(CandlesResponse {
        venue: VENUE,
        ticker: slug.to_owned(),
        series_ticker: parent_of(&market).series_ticker,
        interval,
        candles: bucket_history(&window.points, interval),
        note: Some(window.note.unwrap_or_else(|| {
            "Polymarket publishes a price series rather than OHLC: these bars are its samples \
             bucketed, and carry no volume."
                .to_owned()
        })),
    })
}

/* ----------------------------------------------------------------- corpus */

const CORPUS_KEY: &str = "polymarket:corpus";

/// Pages of 100, ordered by 24h volume.
///
/// Gamma refuses an offset past 2,500 ("use /events/keyset for deeper
/// pagination"), so the whole open universe is not reachable by offset at all
/// and the crawl has to be bounded. Ordering by volume first makes the
/// truncation principled rather than arbitrary — it drops the dead tail, not a
/// random slice — which is the failure the Kalshi crawl had to be fixed for.
/// The snapshot still reports [`Corpus::truncated`] so a panel can say so.
const CORPUS_PAGE: usize = 100;
const CORPUS_MAX_PAGES: usize = 24;

pub async fn build_corpus(state: &AppState) -> Result<Corpus> {
    let gamma = &state.config().polymarket_gamma_base;
    let mut events: Vec<VenueEvent> = Vec::new();
    let mut markets: Vec<Market> = Vec::new();
    let mut truncated = false;

    for page in 0..CORPUS_MAX_PAGES {
        let path = format!(
            "/events{}",
            qs(&[
                ("limit", CORPUS_PAGE.to_string()),
                ("offset", (page * CORPUS_PAGE).to_string()),
                ("closed", "false".to_owned()),
                ("active", "true".to_owned()),
                ("order", "volume24hr".to_owned()),
                ("ascending", "false".to_owned()),
            ])
        );

        let fetched = state
            .http()
            .fetch_json::<Value>(
                &format!("{gamma}{path}"),
                FetchOptions::new().timeout(CORPUS_TIMEOUT).retries(RETRIES),
            )
            .await;

        let items: Vec<Value> = match fetched {
            Ok(Value::Array(items)) => items,
            Ok(_) => Vec::new(),
            // Gamma answers 422 once the offset passes its ceiling, which it words as
            // "use /events/keyset for deeper pagination". That is the catalogue
            // ending as far as offsets can reach, not a failure — the pages already
            // collected are the whole liquid universe. Only a first-page failure is
            // a real one.
            Err(err) => {
                if page > 0 && err.status == Some(422) {
                    truncated = true;
                    break;
                }
                return Err(err);
            }
        };

        // A row this deserialiser cannot read is dropped, but it still counts
        // towards the page: the page *length* is what says whether the
        // catalogue ran out, and shrinking it would end the crawl early.
        let count = items.len();
        for item in items {
            let Ok(raw_event) = serde_json::from_value::<RawGammaEvent>(item) else {
                continue;
            };
            let event = normalise_event(&raw_event);
            markets.extend(event.markets.iter().cloned());
            events.push(event);
        }

        if count < CORPUS_PAGE {
            break;
        }
        truncated = page == CORPUS_MAX_PAGES - 1;
    }

    Ok(Corpus::new(VENUE, events, markets, truncated))
}

pub async fn corpus_snapshot(state: &AppState) -> Result<Arc<Corpus>> {
    let result = state
        .cache()
        .cached(CORPUS_KEY, ttl::CATALOGUE, || build_corpus(state))
        .await;
    crate::sources::corpus::record(state, Venue::Polymarket, &result);
    result
}

pub fn warm_corpus(state: &AppState) {
    let state = state.clone();
    tokio::spawn(async move {
        match corpus_snapshot(&state).await {
            Ok(corpus) => tracing::info!(
                events = corpus.events.len(),
                markets = corpus.markets.len(),
                "[polymarket] corpus warm"
            ),
            Err(err) => tracing::warn!(error = %err, "[polymarket] corpus warm failed"),
        }
    });
}

pub async fn search(state: &AppState, query: &str, limit: usize) -> Result<SearchResponse> {
    refuse_overlong_query(query)?;
    let snapshot = corpus_snapshot(state).await?;
    Ok(search_corpus(&snapshot, query, limit))
}

pub async fn top_markets(state: &AppState, sort: MoverSort, limit: usize) -> Result<Vec<Market>> {
    Ok(rank_markets(
        &corpus_snapshot(state).await?.markets,
        sort,
        limit,
    ))
}

#[cfg(test)]
mod tests {
    //! Polymarket International normalisation tests.
    //!
    //! The fixtures are trimmed copies of real `gamma-api.polymarket.com` and
    //! `clob.polymarket.com` responses, so the stringified JSON arrays
    //! (`"[\"Yes\", \"No\"]"`), the decimal-string book levels and the `{t, p}`
    //! price series are exactly what the upstreams send.

    use super::*;
    use crate::config::Config;
    use serde_json::json;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const HOUR: i64 = 3600;

    fn from_json<T: DeserializeOwned>(value: Value) -> T {
        serde_json::from_value(value).expect("the fixture deserialises")
    }

    /// The fixture with some of its fields replaced — the `{ ...raw, x }` the
    /// TypeScript tests spread.
    fn patched(base: Value, patch: Value) -> Value {
        let mut base = base;
        let (Some(base_map), Value::Object(patch_map)) = (base.as_object_mut(), patch) else {
            panic!("both the fixture and the patch must be objects");
        };
        for (key, value) in patch_map {
            base_map.insert(key, value);
        }
        base
    }

    /* ------------------------------------------------------------- arrays */

    #[test]
    fn parses_gammas_stringified_json_arrays() {
        assert_eq!(
            parse_string_array(&json!("[\"Yes\", \"No\"]")),
            vec!["Yes", "No"]
        );
        assert_eq!(
            parse_string_array(&json!("[\"0.735\", \"0.265\"]")),
            vec!["0.735", "0.265"]
        );
    }

    #[test]
    fn accepts_a_real_array_in_case_the_upstream_is_ever_fixed() {
        assert_eq!(parse_string_array(&json!(["Yes", "No"])), vec!["Yes", "No"]);
    }

    #[test]
    fn returns_nothing_for_absent_or_unparseable_values() {
        assert!(parse_string_array(&Value::Null).is_empty());
        assert!(parse_string_array(&json!("")).is_empty());
        assert!(parse_string_array(&json!("not json")).is_empty());
        assert!(parse_string_array(&json!("{\"a\":1}")).is_empty());
    }

    /* ------------------------------------------------------------- series */

    #[test]
    fn prefers_the_series_gamma_states() {
        let stated: RawGammaEvent = from_json(json!({
            "slug": "fed-decision-in-september-762",
            "seriesSlug": "fomc",
        }));
        assert_eq!(series_from_event(&stated), "fomc");

        let nested: RawGammaEvent = from_json(json!({
            "slug": "x",
            "series": [{ "slug": "nfl", "title": "NFL" }],
        }));
        assert_eq!(series_from_event(&nested), "nfl");
    }

    #[test]
    fn falls_back_to_the_slug_stem_when_it_states_none() {
        // Polymarket suffixes a repeat listing with a creation timestamp or a
        // collision counter; neither is part of the series' name.
        let timestamped: RawGammaEvent = from_json(json!({
            "slug": "fed-decision-in-january-20260729233815502",
        }));
        assert_eq!(series_from_event(&timestamped), "fed-decision-in-january");
        assert_eq!(
            series_from_slug("fed-decision-in-september-762"),
            "fed-decision-in-september"
        );
        assert_eq!(
            series_from_slug("nobel-peace-prize-winner"),
            "nobel-peace-prize-winner"
        );
    }

    /* ------------------------------------------------------------ markets */

    /// Real shape, trimmed: one rung of the September FOMC ladder.
    fn fed_rung() -> Value {
        json!({
            "slug": "will-there-be-no-change-in-fed-interest-rates-after-the-september-2026-meeting-615",
            "question": "Will there be no change in Fed interest rates after the September 2026 meeting?",
            "conditionId": "0xabc",
            "groupItemTitle": "No change",
            "outcomes": "[\"Yes\", \"No\"]",
            "outcomePrices": "[\"0.735\", \"0.265\"]",
            "clobTokenIds": "[\"561528276\", \"902713122\"]",
            "bestBid": 0.73,
            "bestAsk": 0.74,
            "lastTradePrice": 0.74,
            "oneDayPriceChange": 0.02,
            "volumeNum": 1_234_567.5,
            "volume24hr": 77_074.52,
            "liquidityNum": 641_943.36,
            "startDate": "2026-05-13T21:23:13.517067Z",
            "endDate": "2026-09-16T00:00:00Z",
            "active": true,
            "closed": false,
            "acceptingOrders": true,
            "events": [{ "ticker": "fed-decision-in-september-762", "slug": "x", "seriesSlug": "fomc" }]
        })
    }

    fn fed_market() -> RawGammaMarket {
        from_json(fed_rung())
    }

    fn fed_market_with(patch: Value) -> RawGammaMarket {
        from_json(patched(fed_rung(), patch))
    }

    #[test]
    fn reads_the_quote_and_labels_the_venue() {
        let market = normalise_market(&fed_market(), None);
        assert_eq!(market.venue, Venue::Polymarket);
        assert_eq!(market.yes_bid, Some(0.73));
        assert_eq!(market.yes_ask, Some(0.74));
        assert_eq!(market.mid, Some(0.735));
        assert_eq!(market.last_price, Some(0.74));
    }

    #[test]
    fn derives_the_no_side_from_the_yes_book() {
        let market = normalise_market(&fed_market(), None);
        // An offer to sell YES at 0.74 is a bid to buy NO at 0.26 — and the
        // subtraction must not leave 0.26000000000000006 behind.
        assert_eq!(market.no_bid, Some(0.26));
        assert_eq!(market.no_ask, Some(0.27));
    }

    #[test]
    fn recovers_the_previous_price_from_the_24h_change() {
        let market = normalise_market(&fed_market(), None);
        assert_eq!(market.change, Some(0.02));
        assert_eq!(market.previous_price, Some(0.72));
    }

    #[test]
    fn takes_its_event_and_series_from_the_inlined_parent() {
        let market = normalise_market(&fed_market(), None);
        assert_eq!(market.event_ticker, "fed-decision-in-september-762");
        assert_eq!(market.series_ticker, "fomc");
    }

    #[test]
    fn labels_the_yes_leg_with_its_rung_not_the_whole_question() {
        assert_eq!(
            normalise_market(&fed_market(), None).yes_sub_title,
            "No change"
        );
        // A standalone binary market has no rung, so the outcome name stands in.
        let standalone = fed_market_with(json!({
            "groupItemTitle": "",
            "outcomes": "[\"Yes\", \"No\"]",
        }));
        assert_eq!(normalise_market(&standalone, None).yes_sub_title, "Yes");
    }

    #[test]
    fn reports_an_empty_book_side_as_absent_not_as_a_zero_quote() {
        let raw = fed_market_with(json!({ "bestBid": 0, "bestAsk": 0, "lastTradePrice": 0 }));
        let market = normalise_market(&raw, None);
        assert_eq!(market.yes_bid, None);
        assert_eq!(market.yes_ask, None);
        assert_eq!(market.no_bid, None);
        // …but the mark price gamma always publishes still stands in for last.
        assert_eq!(market.last_price, Some(0.735));
    }

    #[test]
    fn keeps_a_price_of_1_a_decided_market_is_not_an_empty_one() {
        let raw = fed_market_with(json!({ "bestAsk": 1 }));
        assert_eq!(normalise_market(&raw, None).yes_ask, Some(1.0));
    }

    #[test]
    fn leaves_open_interest_unstated_rather_than_zero() {
        // Gamma publishes open interest per event only, and an event's figure is
        // not this contract's.
        assert_eq!(normalise_market(&fed_market(), None).open_interest, None);
    }

    #[test]
    fn states_no_strike_because_polymarket_words_them_into_the_question() {
        let market = normalise_market(&fed_market(), None);
        assert!(market.strike_type.is_none());
        assert_eq!(market.floor_strike, None);
        assert_eq!(market.cap_strike, None);
    }

    #[test]
    fn reads_the_settled_side_off_the_resolved_prices_and_only_when_closed() {
        let open = fed_market_with(json!({ "outcomePrices": "[\"1\", \"0\"]" }));
        assert_eq!(normalise_market(&open, None).result, "");

        let settled_yes =
            fed_market_with(json!({ "closed": true, "outcomePrices": "[\"1\", \"0\"]" }));
        assert_eq!(normalise_market(&settled_yes, None).result, "yes");

        let settled_no =
            fed_market_with(json!({ "closed": true, "outcomePrices": "[\"0\", \"1\"]" }));
        assert_eq!(normalise_market(&settled_no, None).result, "no");
    }

    #[test]
    fn maps_the_activity_flags_onto_a_status() {
        assert_eq!(normalise_market(&fed_market(), None).status, "open");
        assert_eq!(
            normalise_market(&fed_market_with(json!({ "acceptingOrders": false })), None).status,
            "closed"
        );
        assert_eq!(
            normalise_market(&fed_market_with(json!({ "active": false })), None).status,
            "unopened"
        );
        assert_eq!(
            normalise_market(&fed_market_with(json!({ "closed": true })), None).status,
            "settled"
        );
    }

    /* ------------------------------------------------------------- events */

    fn fed_event() -> Value {
        json!({
            "ticker": "fed-decision-in-september-762",
            "title": "Fed Decision in September?",
            "seriesSlug": "fomc",
            "series": [{ "slug": "fomc", "title": "FOMC" }],
            "negRisk": true,
            "tags": [
                { "id": "100478", "label": "fomc" },
                { "id": "2", "label": "Politics" },
                { "id": "100328", "label": "Economy" }
            ],
            "markets": [
                { "slug": "a", "question": "A?", "groupItemTitle": "No change", "bestBid": 0.7, "bestAsk": 0.71 }
            ]
        })
    }

    #[test]
    fn takes_the_broadest_tag_as_the_category_not_the_first_one() {
        // Tag ids are issued in sequence, so the lowest is the top-level label:
        // `Politics` is 2 and `fomc` is 100478, and gamma lists them in neither order.
        let event: RawGammaEvent = from_json(fed_event());
        assert_eq!(normalise_event(&event).category, "Politics");
    }

    #[test]
    fn reads_negrisk_as_mutual_exclusivity() {
        let event: RawGammaEvent = from_json(fed_event());
        assert!(normalise_event(&event).mutually_exclusive);

        let flat: RawGammaEvent = from_json(patched(fed_event(), json!({ "negRisk": false })));
        assert!(!normalise_event(&flat).mutually_exclusive);
    }

    #[test]
    fn stamps_its_own_event_and_series_onto_every_nested_market() {
        let raw: RawGammaEvent = from_json(fed_event());
        let event = normalise_event(&raw);
        assert_eq!(
            event.markets[0].event_ticker,
            "fed-decision-in-september-762"
        );
        assert_eq!(event.markets[0].series_ticker, "fomc");
        assert_eq!(event.markets[0].venue, Venue::Polymarket);
    }

    /* --------------------------------------------------------------- book */

    /// The CLOB returns both sides ascending, as decimal strings.
    fn clob_book() -> Value {
        json!({
            "bids": [
                { "price": "0.700", "size": "300" },
                { "price": "0.720", "size": "150" },
                { "price": "0.730", "size": "7966.95" }
            ],
            "asks": [
                { "price": "0.790", "size": "40" },
                { "price": "0.760", "size": "900" },
                { "price": "0.740", "size": "5000" }
            ]
        })
    }

    fn prices(levels: &[BookLevel]) -> Vec<f64> {
        levels.iter().map(|level| level.price).collect()
    }

    #[test]
    fn sorts_bids_best_first_and_asks_cheapest_first() {
        let raw: RawClobBook = from_json(clob_book());
        let book = normalise_order_book(&raw, "slug", DEFAULT_BOOK_DEPTH);
        assert_eq!(prices(&book.yes), vec![0.73, 0.72, 0.7]);
        assert_eq!(prices(&book.yes_asks), vec![0.74, 0.76, 0.79]);
    }

    #[test]
    fn derives_the_no_ladder_from_the_yes_offers() {
        let raw: RawClobBook = from_json(clob_book());
        let book = normalise_order_book(&raw, "slug", DEFAULT_BOOK_DEPTH);
        // Selling YES at 0.74 is buying NO at 0.26; best NO bid is the highest.
        assert_eq!(prices(&book.no), vec![0.26, 0.24, 0.21]);
        assert_eq!(book.no[0].size, 5000.0);
    }

    #[test]
    fn derives_best_bid_best_ask_spread_and_mid() {
        let raw: RawClobBook = from_json(clob_book());
        let book = normalise_order_book(&raw, "slug", DEFAULT_BOOK_DEPTH);
        assert_eq!(book.best_yes_bid, Some(0.73));
        assert_eq!(book.best_yes_ask, Some(0.74));
        assert_eq!(book.spread, Some(0.01));
        assert_eq!(book.mid, Some(0.735));
        assert_eq!(book.venue, Venue::Polymarket);
    }

    #[test]
    fn reports_nulls_rather_than_a_fake_quote_for_an_empty_book() {
        let book = normalise_order_book(&RawClobBook::default(), "slug", DEFAULT_BOOK_DEPTH);
        assert!(book.yes.is_empty());
        assert_eq!(book.best_yes_bid, None);
        assert_eq!(book.spread, None);
    }

    #[test]
    fn honours_the_depth_cap() {
        let rows: Vec<Value> = (0..40)
            .map(|i| json!({ "price": format!("0.{}", i + 10), "size": "5" }))
            .collect();
        let raw: RawClobBook = from_json(json!({ "bids": rows }));
        assert_eq!(normalise_order_book(&raw, "slug", 5).yes.len(), 5);
    }

    /* ------------------------------------------------------------- trades */

    #[test]
    fn restates_a_no_token_print_in_yes_terms() {
        let raw: Vec<RawDataTrade> = from_json(json!([{
            "transactionHash": "0x1", "timestamp": 1_786_900_389, "price": 0.26,
            "size": 5, "side": "BUY", "outcomeIndex": 1
        }]));
        let response = normalise_trades(&raw, "slug");
        // Buying NO at 26¢ is the same print as selling YES at 74¢.
        assert_eq!(response.trades[0].yes_price, 0.74);
        assert_eq!(response.trades[0].no_price, 0.26);
        assert_eq!(response.trades[0].taker_side, "no");
    }

    #[test]
    fn reads_a_yes_token_buy_as_a_yes_taker() {
        let raw: Vec<RawDataTrade> = from_json(json!([{
            "transactionHash": "0x2", "timestamp": 1, "price": 0.74,
            "size": 5, "side": "BUY", "outcomeIndex": 0
        }]));
        let response = normalise_trades(&raw, "slug");
        assert_eq!(response.trades[0].yes_price, 0.74);
        assert_eq!(response.trades[0].taker_side, "yes");
    }

    #[test]
    fn reads_a_yes_token_sell_as_a_no_taker() {
        let raw: Vec<RawDataTrade> = from_json(json!([{
            "transactionHash": "0x3", "timestamp": 1, "price": 0.73,
            "size": 9.99, "side": "SELL", "outcomeIndex": 0
        }]));
        let response = normalise_trades(&raw, "slug");
        assert_eq!(response.trades[0].taker_side, "no");
        assert_eq!(response.trades[0].count, 9.99);
    }

    #[test]
    fn keeps_sibling_fills_of_one_transaction_distinct() {
        let raw: Vec<RawDataTrade> = from_json(json!([
            { "transactionHash": "0x9", "timestamp": 1, "price": 0.7, "size": 1, "side": "BUY", "outcomeIndex": 0 },
            { "transactionHash": "0x9", "timestamp": 1, "price": 0.71, "size": 2, "side": "BUY", "outcomeIndex": 0 }
        ]));
        let response = normalise_trades(&raw, "slug");
        assert_ne!(response.trades[0].trade_id, response.trades[1].trade_id);
    }

    /* ------------------------------------------------------------ candles */

    fn point(t: i64, p: f64) -> RawHistoryPoint {
        RawHistoryPoint { t: t as f64, p }
    }

    #[test]
    fn buckets_samples_into_ohlc_on_the_period_end() {
        let candles = bucket_history(
            &[
                point(HOUR * 10 + 60, 0.5),
                point(HOUR * 10 + 600, 0.6),
                point(HOUR * 10 + 1200, 0.4),
                point(HOUR * 10 + 3000, 0.55),
            ],
            CandleInterval::OneHour,
        );
        let candle = &candles[0];
        assert_eq!(candle.time, HOUR * 11);
        assert_eq!(candle.open, 0.5);
        assert_eq!(candle.high, 0.6);
        assert_eq!(candle.low, 0.4);
        assert_eq!(candle.close, 0.55);
        assert!(candle.traded);
    }

    #[test]
    fn leaves_volume_and_open_interest_unstated() {
        // The upstream is a price series with no size attached to any point;
        // drawing a zero-volume bar would assert something it never said.
        let candles = bucket_history(&[point(HOUR, 0.5)], CandleInterval::OneHour);
        assert_eq!(candles[0].volume, None);
        assert_eq!(candles[0].open_interest, None);
    }

    #[test]
    fn carries_the_last_close_across_a_quiet_stretch() {
        let candles = bucket_history(
            &[point(HOUR + 10, 0.5), point(HOUR * 4 + 10, 0.8)],
            CandleInterval::OneHour,
        );
        // Two observed buckets, two hours of hold in between — a continuous line,
        // not a jump.
        assert_eq!(candles.len(), 4);
        assert_eq!(candles[1].close, 0.5);
        assert!(!candles[1].traded);
        assert!(!candles[2].traded);
        assert_eq!(candles[3].close, 0.8);
        assert!(candles[3].traded);
    }

    #[test]
    fn sorts_ascending_as_lightweight_charts_requires() {
        let candles = bucket_history(
            &[point(HOUR * 5, 0.2), point(HOUR * 2, 0.1)],
            CandleInterval::OneHour,
        );
        assert!(candles[0].time < candles.last().expect("candles").time);
    }

    #[test]
    fn returns_nothing_for_an_empty_history() {
        assert!(bucket_history(&[], CandleInterval::OneHour).is_empty());
    }

    #[test]
    fn skips_malformed_points_instead_of_emitting_nan_bars() {
        let candles = bucket_history(
            &[point(HOUR, f64::NAN), point(HOUR, 0.3)],
            CandleInterval::OneHour,
        );
        assert_eq!(candles.len(), 1);
        assert_eq!(candles[0].close, 0.3);
    }

    #[test]
    fn a_history_point_that_is_null_or_text_is_read_the_way_the_coercion_does() {
        let points: Vec<RawHistoryPoint> =
            from_json(json!([{ "t": "3600", "p": "0.5" }, { "t": 3600, "p": null }]));
        assert_eq!(points[0].t, 3600.0);
        assert_eq!(points[0].p, 0.5);
        assert!(points[1].p.is_nan());
        assert_eq!(bucket_history(&points, CandleInterval::OneHour).len(), 1);
    }

    /* ---------------------------------------------------------- upstreams */

    fn state_for(gamma: &str, clob: &str, data: &str) -> AppState {
        AppState::new(Config {
            polymarket_gamma_base: gamma.to_owned(),
            polymarket_clob_base: clob.to_owned(),
            polymarket_data_base: data.to_owned(),
            ..Config::default()
        })
    }

    const SLUG: &str =
        "will-there-be-no-change-in-fed-interest-rates-after-the-september-2026-meeting-615";

    /// Gamma answering `/markets?slug=…` with the FOMC rung, once.
    async fn gamma_with_the_rung(server: &MockServer, row: Value) {
        Mock::given(method("GET"))
            .and(path("/markets"))
            .and(query_param("slug", SLUG))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([row])))
            .expect(1)
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn get_market_reads_the_row_gamma_returns_for_the_slug() {
        let gamma = MockServer::start().await;
        gamma_with_the_rung(&gamma, fed_rung()).await;
        let state = state_for(&gamma.uri(), "http://unused", "http://unused");

        let market = get_market(&state, SLUG).await.expect("the row normalises");
        assert_eq!(market.ticker, SLUG);
        assert_eq!(market.yes_bid, Some(0.73));
        assert_eq!(market.series_ticker, "fomc");
    }

    #[tokio::test]
    async fn an_unknown_slug_is_a_not_found_with_a_hint() {
        let gamma = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/markets"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
            .mount(&gamma)
            .await;
        let state = state_for(&gamma.uri(), "http://unused", "http://unused");

        let err = get_market(&state, "nope")
            .await
            .expect_err("no such market");
        assert_eq!(err.code, crate::error::codes::NOT_FOUND);
        assert!(err.message.contains("nope"));
        assert!(err.hint.expect("a hint").contains("SRCH"));
    }

    #[tokio::test]
    async fn the_book_is_keyed_by_the_token_id_the_catalogue_states() {
        let gamma = MockServer::start().await;
        let clob = MockServer::start().await;
        // One gamma call serves both the token lookup and the market row.
        gamma_with_the_rung(&gamma, fed_rung()).await;
        Mock::given(method("GET"))
            .and(path("/book"))
            .and(query_param("token_id", "561528276"))
            .respond_with(ResponseTemplate::new(200).set_body_json(clob_book()))
            .expect(1)
            .mount(&clob)
            .await;

        let state = state_for(&gamma.uri(), &clob.uri(), "http://unused");
        let book = get_order_book(&state, SLUG, DEFAULT_BOOK_DEPTH)
            .await
            .expect("the book normalises");

        assert_eq!(book.ticker, SLUG);
        assert_eq!(book.best_yes_bid, Some(0.73));
        assert_eq!(book.best_yes_ask, Some(0.74));
    }

    #[tokio::test]
    async fn a_market_never_deployed_to_the_clob_has_no_book() {
        let gamma = MockServer::start().await;
        gamma_with_the_rung(&gamma, patched(fed_rung(), json!({ "clobTokenIds": "" }))).await;
        let state = state_for(&gamma.uri(), "http://unused", "http://unused");

        let err = get_order_book(&state, SLUG, DEFAULT_BOOK_DEPTH)
            .await
            .expect_err("no token id, no book");
        assert_eq!(err.code, crate::error::codes::NOT_FOUND);
        assert!(err.hint.expect("a hint").contains("never deployed"));
    }

    #[tokio::test]
    async fn the_tape_is_asked_for_by_on_chain_condition() {
        let gamma = MockServer::start().await;
        let data = MockServer::start().await;
        gamma_with_the_rung(&gamma, fed_rung()).await;
        Mock::given(method("GET"))
            .and(path("/trades"))
            .and(query_param("market", "0xabc"))
            .and(query_param("limit", "50"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([{
                "transactionHash": "0x1", "timestamp": 1_786_900_389, "price": 0.26,
                "size": 5, "side": "BUY", "outcomeIndex": 1
            }])))
            .expect(1)
            .mount(&data)
            .await;

        let state = state_for(&gamma.uri(), "http://unused", &data.uri());
        let tape = get_trades(&state, SLUG, DEFAULT_TRADE_LIMIT)
            .await
            .expect("the tape normalises");

        assert_eq!(tape.trades.len(), 1);
        assert_eq!(tape.trades[0].yes_price, 0.74);
        assert_eq!(tape.trades[0].ticker, SLUG);
        assert!(tape.cursor.is_none());
    }

    #[tokio::test]
    async fn a_market_with_no_on_chain_condition_has_no_tape() {
        let gamma = MockServer::start().await;
        gamma_with_the_rung(&gamma, patched(fed_rung(), json!({ "conditionId": null }))).await;
        let state = state_for(&gamma.uri(), "http://unused", "http://unused");

        let err = get_trades(&state, SLUG, DEFAULT_TRADE_LIMIT)
            .await
            .expect_err("no condition, no tape");
        assert_eq!(err.code, crate::error::codes::NOT_FOUND);
        assert!(err.message.contains("on-chain condition"));
    }

    #[tokio::test]
    async fn the_trade_limit_is_clamped_into_the_range_the_data_api_serves() {
        let gamma = MockServer::start().await;
        let data = MockServer::start().await;
        gamma_with_the_rung(&gamma, fed_rung()).await;
        Mock::given(method("GET"))
            .and(path("/trades"))
            .and(query_param("limit", "500"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
            .expect(1)
            .mount(&data)
            .await;

        let state = state_for(&gamma.uri(), "http://unused", &data.uri());
        let tape = get_trades(&state, SLUG, 10_000)
            .await
            .expect("an over-large limit is clamped, not refused");
        assert!(tape.trades.is_empty());
    }

    /// A history request answering with one sample per window.
    async fn history_chunk_mock(clob: &MockServer, start_ts: &str, sample: i64, price: f64) {
        Mock::given(method("GET"))
            .and(path("/prices-history"))
            .and(query_param("startTs", start_ts))
            .and(query_param("fidelity", "1440"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({ "history": [{ "t": sample, "p": price }] })),
            )
            .expect(1)
            .mount(clob)
            .await;
    }

    #[tokio::test]
    async fn a_window_wider_than_fifteen_days_is_stitched_from_chunks() {
        let gamma = MockServer::start().await;
        let clob = MockServer::start().await;
        gamma_with_the_rung(&gamma, fed_rung()).await;

        // 45 days is three chunks of the 15-day ceiling, requested by timestamp.
        history_chunk_mock(&clob, "0", 3_600, 0.4).await;
        history_chunk_mock(&clob, "1296000", 1_299_600, 0.5).await;
        history_chunk_mock(&clob, "2592000", 2_595_600, 0.6).await;

        let state = state_for(&gamma.uri(), &clob.uri(), "http://unused");
        let response = get_candles(&state, SLUG, CandleInterval::OneDay, 0, 45 * 86_400)
            .await
            .expect("the chunks stitch");

        assert_eq!(response.ticker, SLUG);
        assert_eq!(response.series_ticker, "fomc");
        assert_eq!(response.interval, CandleInterval::OneDay);
        let traded: Vec<f64> = response
            .candles
            .iter()
            .filter(|candle| candle.traded)
            .map(|candle| candle.close)
            .collect();
        assert_eq!(traded, vec![0.4, 0.5, 0.6]);
        assert!(response
            .note
            .expect("a note")
            .contains("price series rather than OHLC"));
    }

    #[tokio::test]
    async fn a_window_past_eight_chunks_falls_back_to_a_lookback_and_says_so() {
        let gamma = MockServer::start().await;
        let clob = MockServer::start().await;
        gamma_with_the_rung(&gamma, fed_rung()).await;

        // 121 days needs nine chunks, which is past the stitch limit.
        Mock::given(method("GET"))
            .and(path("/prices-history"))
            .and(query_param("interval", "max"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({ "history": [
                { "t": 86_400, "p": 0.3 },
                { "t": 200 * 86_400, "p": 0.9 }
            ] })),
            )
            .expect(1)
            .mount(&clob)
            .await;

        let state = state_for(&gamma.uri(), &clob.uri(), "http://unused");
        let response = get_candles(&state, SLUG, CandleInterval::OneDay, 0, 121 * 86_400)
            .await
            .expect("the lookback answers");

        // The point past the requested window is clipped client-side.
        let traded: Vec<f64> = response
            .candles
            .iter()
            .filter(|candle| candle.traded)
            .map(|candle| candle.close)
            .collect();
        assert_eq!(traded, vec![0.3]);

        let note = response.note.expect("a note");
        assert!(note.contains("\"max\" lookback"), "{note}");
        assert!(note.contains("may start later than requested"), "{note}");
    }

    #[tokio::test]
    async fn list_series_maps_gammas_series_rows() {
        let gamma = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/series"))
            .and(query_param("limit", "100"))
            .and(query_param("closed", "false"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                { "slug": "fomc", "title": "FOMC", "recurrence": "monthly", "seriesType": "recurring" },
                { "ticker": "nfl" }
            ])))
            .expect(1)
            .mount(&gamma)
            .await;

        let state = state_for(&gamma.uri(), "http://unused", "http://unused");
        let series = list_series(&state).await.expect("the rows normalise");

        assert_eq!(series.len(), 2);
        assert_eq!(series[0].venue, Venue::Polymarket);
        assert_eq!(series[0].ticker, "fomc");
        assert_eq!(series[0].title, "FOMC");
        assert_eq!(series[0].category, "recurring");
        assert_eq!(series[0].frequency, "monthly");
        // A row with only a ticker falls back to it, and states no title at all.
        assert_eq!(series[1].ticker, "nfl");
        assert_eq!(series[1].title, "");
    }

    #[tokio::test]
    async fn get_event_normalises_the_event_and_its_ladder() {
        let gamma = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/events"))
            .and(query_param("slug", "fed-decision-in-september-762"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([fed_event()])))
            .expect(1)
            .mount(&gamma)
            .await;

        let state = state_for(&gamma.uri(), "http://unused", "http://unused");
        let event = get_event(&state, "fed-decision-in-september-762")
            .await
            .expect("the event normalises");

        assert_eq!(event.event_ticker, "fed-decision-in-september-762");
        assert_eq!(event.sub_title, "FOMC");
        assert_eq!(event.category, "Politics");
        assert_eq!(event.markets.len(), 1);
    }

    #[tokio::test]
    async fn an_unknown_event_slug_is_a_not_found() {
        let gamma = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/events"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
            .mount(&gamma)
            .await;

        let state = state_for(&gamma.uri(), "http://unused", "http://unused");
        let err = get_event(&state, "nope").await.expect_err("no such event");
        assert_eq!(err.code, crate::error::codes::NOT_FOUND);
        assert!(err.hint.is_none());
    }

    /* ------------------------------------------------------------- corpus */

    /// A page of `count` events, each with one market.
    fn corpus_page(offset: usize, count: usize) -> Value {
        Value::Array(
            (0..count)
                .map(|i| {
                    let n = offset + i;
                    json!({
                        "ticker": format!("event-{n}"),
                        "title": format!("Event {n}"),
                        "seriesSlug": "series",
                        "markets": [{
                            "slug": format!("market-{n}"),
                            "question": format!("Question {n}?"),
                            "bestBid": 0.5,
                            "bestAsk": 0.6,
                            "volume24hr": 10
                        }]
                    })
                })
                .collect(),
        )
    }

    #[tokio::test]
    async fn the_crawl_stops_on_the_first_short_page() {
        let gamma = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/events"))
            .and(query_param("offset", "0"))
            .and(query_param("order", "volume24hr"))
            .and(query_param("ascending", "false"))
            .respond_with(ResponseTemplate::new(200).set_body_json(corpus_page(0, 3)))
            .expect(1)
            .mount(&gamma)
            .await;

        let state = state_for(&gamma.uri(), "http://unused", "http://unused");
        let corpus = build_corpus(&state).await.expect("the crawl completes");

        assert_eq!(corpus.venue, Venue::Polymarket);
        assert_eq!(corpus.events.len(), 3);
        assert_eq!(corpus.markets.len(), 3);
        assert!(!corpus.truncated);
    }

    #[tokio::test]
    async fn a_422_past_the_first_page_is_the_catalogue_ending() {
        let gamma = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/events"))
            .and(query_param("offset", "0"))
            .respond_with(ResponseTemplate::new(200).set_body_json(corpus_page(0, CORPUS_PAGE)))
            .expect(1)
            .mount(&gamma)
            .await;
        // Gamma words its offset ceiling as a 422 telling you to use keyset
        // pagination. That is the end of what offsets reach, not a failure.
        Mock::given(method("GET"))
            .and(path("/events"))
            .and(query_param("offset", "100"))
            .respond_with(
                ResponseTemplate::new(422)
                    .set_body_json(json!({ "error": "use /events/keyset for deeper pagination" })),
            )
            .expect(1)
            .mount(&gamma)
            .await;

        let state = state_for(&gamma.uri(), "http://unused", "http://unused");
        let corpus = build_corpus(&state).await.expect("the 422 ends the crawl");

        assert_eq!(corpus.events.len(), CORPUS_PAGE);
        assert!(corpus.truncated);
    }

    #[tokio::test]
    async fn a_422_on_the_first_page_is_a_real_failure() {
        let gamma = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/events"))
            .respond_with(ResponseTemplate::new(422))
            .mount(&gamma)
            .await;

        let state = state_for(&gamma.uri(), "http://unused", "http://unused");
        let err = build_corpus(&state)
            .await
            .expect_err("nothing was collected, so this is an outage");
        assert_eq!(err.status, Some(422));
    }

    #[tokio::test]
    async fn a_body_that_is_not_a_list_ends_the_crawl_rather_than_failing_it() {
        let gamma = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/events"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "events": [] })))
            .expect(1)
            .mount(&gamma)
            .await;

        let state = state_for(&gamma.uri(), "http://unused", "http://unused");
        let corpus = build_corpus(&state)
            .await
            .expect("an odd body is not a failure");
        assert!(corpus.events.is_empty());
        assert!(!corpus.truncated);
    }

    #[tokio::test]
    async fn the_snapshot_is_built_once_and_then_searched_and_ranked() {
        let gamma = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/events"))
            .respond_with(ResponseTemplate::new(200).set_body_json(corpus_page(0, 3)))
            // Both the search and the leaderboard read one cached crawl.
            .expect(1)
            .mount(&gamma)
            .await;

        let state = state_for(&gamma.uri(), "http://unused", "http://unused");

        let found = search(&state, "event 1", DEFAULT_RESULT_LIMIT)
            .await
            .expect("the corpus is searchable");
        assert_eq!(found.query, "event 1");
        assert_eq!(found.scanned, 3);
        // "Event 0" and "Event 2" carry half the query, so they are listed
        // under the one event that carries all of it rather than dropped.
        assert_eq!(found.hits.len(), 3);
        assert_eq!(found.hits[0].event.event_ticker, "event-1");
        assert_eq!(found.hits[0].matched_terms, 2);
        assert!(found.hits[1..].iter().all(|hit| hit.matched_terms == 1));

        let top = top_markets(&state, MoverSort::Volume, DEFAULT_RESULT_LIMIT)
            .await
            .expect("the corpus is rankable");
        assert_eq!(top.len(), 3);
    }

    #[tokio::test]
    async fn holds_the_catalogue_under_the_keys_the_typescript_used() {
        let gamma = MockServer::start().await;
        gamma_with_the_rung(&gamma, fed_rung()).await;
        let state = state_for(&gamma.uri(), "http://unused", "http://unused");

        // The token mapping is immutable once a market is deployed, so it is
        // held under its own key for the metadata TTL rather than re-derived.
        let token = yes_token_id(&state, SLUG).await.expect("a token id");
        assert_eq!(*token, "561528276");

        let base = gamma.uri();
        assert!(state
            .cache()
            .get::<Vec<RawGammaMarket>>(&format!("polymarket:{base}/markets?slug={SLUG}"))
            .await
            .is_some());
        assert!(state
            .cache()
            .get::<String>(&format!("polymarket:token:{SLUG}"))
            .await
            .is_some());
    }
}
