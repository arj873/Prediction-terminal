//! Polymarket US (polymarket.us) client — the CFTC-regulated book.
//!
//! A different exchange from Polymarket International, not a regional skin of
//! it: different host, different schema, different markets. It splits its API in
//! two, and only one half is usable here:
//!
//! ```text
//! gateway.polymarket.us   public. events, markets, books, series, search.
//! api.polymarket.us       API key required. orders, positions, and the only
//!                         historical trade data the exchange publishes.
//! ```
//!
//! The terminal holds no credentials by design, so it reads the gateway and
//! nothing else. That is a real limit rather than an oversight, and the two
//! places it bites — no chart, no tape — say so in as many words instead of
//! failing obscurely or drawing an empty panel.
//!
//! Prices arrive as `{value, currency}` objects and quantities as decimal
//! strings; both are flattened to plain numbers here.

use std::sync::Arc;
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::Value;
use terminal_core::slug::strip_dates;
use terminal_core::types::{
    BookLevel, CandleInterval, CandlesResponse, Market, MarketStatus, OrderBook, SeriesInfo,
    TradesResponse, Venue, VenueEvent,
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

const VENUE: Venue = Venue::PolymarketUs;

/* --------------------------------------------------------------- coercion */

/// Unwrap a `{value, currency}` money object, or a bare decimal string.
pub fn money(value: &Value) -> Option<f64> {
    match value {
        Value::Null => None,
        Value::Object(map) => map.get("value").and_then(money),
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

/// `money` over a field that may be absent altogether.
fn money_of(value: Option<&Value>) -> Option<f64> {
    value.and_then(money)
}

/// JavaScript's `Number(string)`: surrounding whitespace is not a value, and
/// a string of nothing but whitespace is zero.
fn js_number(text: &str) -> Option<f64> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Some(0.0);
    }
    trimmed.parse::<f64>().ok().filter(|n| n.is_finite())
}

/// As [`money`], but an empty book side reports `0` and means "nothing".
fn quote(value: Option<&Value>) -> Option<f64> {
    money_of(value).filter(|n| *n != 0.0)
}

/* ------------------------------------------------------------ raw upstream */

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RawUsMarket {
    pub id: Option<String>,
    pub slug: Option<String>,
    pub question: Option<String>,
    pub title: Option<String>,
    pub title_short: Option<String>,
    pub subtitle: Option<String>,
    pub description: Option<String>,
    pub category: Option<String>,
    pub market_type: Option<String>,
    pub status: Option<String>,
    pub active: Option<bool>,
    pub closed: Option<bool>,
    pub archived: Option<bool>,
    pub hidden: Option<bool>,
    pub start_date: Option<String>,
    pub end_date: Option<String>,
    pub game_start_time: Option<String>,
    pub best_bid_quote: Option<Value>,
    pub best_ask_quote: Option<Value>,
    /// Present, and deliberately unread. See [`normalise_market`] — the pair is
    /// not self-consistent across endpoints, and its shape is not worth
    /// committing to for a field nothing reads.
    pub outcomes: Option<Value>,
    pub outcome_prices: Option<Value>,
    pub market_sides: Option<Vec<RawUsMarketSide>>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RawUsMarketSide {
    pub description: Option<String>,
    pub long: Option<bool>,
    pub price: Option<Value>,
    pub quote: Option<Value>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RawUsEvent {
    pub id: Option<String>,
    pub ticker: Option<String>,
    pub slug: Option<String>,
    pub title: Option<String>,
    pub description: Option<String>,
    pub category: Option<String>,
    pub series_slug: Option<String>,
    pub start_date: Option<String>,
    pub end_date: Option<String>,
    pub active: Option<bool>,
    pub closed: Option<bool>,
    pub archived: Option<bool>,
    pub hidden: Option<bool>,
    pub markets: Option<Vec<RawUsMarket>>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RawUsBook {
    pub market_data: Option<RawUsBookData>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RawUsBookData {
    pub market_slug: Option<String>,
    pub bids: Option<Vec<RawUsBookLevel>>,
    pub offers: Option<Vec<RawUsBookLevel>>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RawUsBookLevel {
    pub px: Option<Value>,
    pub qty: Option<Value>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RawUsBbo {
    pub market_data: Option<RawUsBboData>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RawUsBboData {
    pub market_slug: Option<String>,
    pub current_px: Option<Value>,
    pub last_trade_px: Option<Value>,
    pub settlement_px: Option<Value>,
    pub shares_traded: Option<Value>,
    pub open_interest: Option<Value>,
    pub best_bid: Option<Value>,
    pub best_ask: Option<Value>,
}

/// The event and series a nested market inherits from its parent listing.
#[derive(Debug, Clone, Default)]
pub struct MarketParent {
    pub event_ticker: String,
    pub series_ticker: String,
}

/* ------------------------------------------------------------ normalisers */

/// `MARKET_STATUS_OPEN` → `open`.
fn status_of(raw: &RawUsMarket) -> MarketStatus {
    let stated = raw
        .status
        .as_deref()
        .map(|s| s.strip_prefix("MARKET_STATUS_").unwrap_or(s).to_lowercase());
    if let Some(stated) = stated.filter(|s| !s.is_empty()) {
        return stated;
    }
    if raw.closed == Some(true) {
        return "closed".into();
    }
    if raw.active == Some(false) {
        return "unopened".into();
    }
    "open".into()
}

/// One contract, quoted from its best bid and offer.
///
/// **`outcome_prices` is not read, on purpose.** The field exists on every
/// market and means different things on different endpoints: in a nested event
/// listing it holds `[bestBid, bestAsk]`, and the same market fetched by slug
/// holds `[askToBuyYes, askToBuyNo]` — for one market, `["0.4600","0.4780"]` in
/// one place and `["0.4780","0.54"]` in the other. Nor does the `outcomes`
/// array order it: the same payload labels one market `["Yes","No"]` and its
/// neighbour `["No","Yes"]` with both price arrays in bid/ask order. Reading
/// either would silently mislabel a bid as an ask on half the book. The
/// `bestBidQuote`/`bestAskQuote` pair is unambiguous everywhere, so it is the
/// only price source used.
pub fn normalise_market(raw: &RawUsMarket, parent: Option<&MarketParent>) -> Market {
    let yes_bid = quote(raw.best_bid_quote.as_ref());
    let yes_ask = quote(raw.best_ask_quote.as_ref());
    let mid = match (yes_bid, yes_ask) {
        (Some(bid), Some(ask)) => Some(round4((bid + ask) / 2.0)),
        _ => yes_bid.or(yes_ask),
    };

    let label = first_non_empty([
        raw.title.as_deref(),
        raw.title_short.as_deref(),
        raw.question.as_deref(),
        raw.slug.as_deref(),
    ]);
    let short = raw
        .market_sides
        .iter()
        .flatten()
        .find(|side| side.long == Some(false))
        .and_then(|side| side.description.clone());

    Market {
        venue: VENUE,
        ticker: raw.slug.clone().unwrap_or_default(),
        event_ticker: parent.map(|p| p.event_ticker.clone()).unwrap_or_default(),
        series_ticker: parent.map(|p| p.series_ticker.clone()).unwrap_or_default(),
        title: first_non_empty([raw.question.as_deref(), Some(&label)]),
        yes_sub_title: match raw.subtitle.as_deref().filter(|s| !s.is_empty()) {
            Some(subtitle) => format!("{label} · {subtitle}"),
            None => label.clone(),
        },
        no_sub_title: short.unwrap_or_else(|| "No".into()),
        status: status_of(raw),
        market_type: raw.market_type.clone().unwrap_or_else(|| "binary".into()),
        yes_bid,
        yes_ask,
        // One book per contract, quoted in YES terms; the NO side is its mirror.
        no_bid: yes_ask.map(|ask| round4(1.0 - ask)),
        no_ask: yes_bid.map(|bid| round4(1.0 - bid)),
        mid,
        // The catalogue carries no prints. `bbo` does, and [`get_market`] fills
        // these in from it; a market read out of the corpus leaves them
        // unstated.
        last_price: None,
        previous_price: None,
        change: None,
        volume: None,
        volume24h: None,
        open_interest: None,
        liquidity: None,
        open_time: raw.start_date.clone().unwrap_or_default(),
        close_time: raw.end_date.clone().unwrap_or_default(),
        expiration_time: raw.end_date.clone().unwrap_or_default(),
        result: String::new(),
        rules_primary: raw.description.clone().unwrap_or_default(),
        category: raw.category.clone().filter(|c| !c.is_empty()),
        strike_type: None,
        floor_strike: None,
        cap_strike: None,
    }
}

/// JavaScript's `a || b || c` over strings: the first one that is not empty.
fn first_non_empty<'a>(candidates: impl IntoIterator<Item = Option<&'a str>>) -> String {
    candidates
        .into_iter()
        .flatten()
        .find(|value| !value.is_empty())
        .unwrap_or_default()
        .to_string()
}

/// The series an event belongs to.
///
/// The exchange states one on its sports and weather books (`mlb-2026`,
/// `weather-daily-high-nyc`) but leaves it empty on much of the rest, including
/// the FOMC, CPI and election books that matter most for comparing brokers.
/// Slugs are disciplined enough to fall back on: strip the date and what is left
/// names the recurring question, so `usfed-fomc-2026-10-28` and
/// `usfed-fomc-2026-12-09` are one series without the exchange saying so.
///
/// The date is not always at the end, and is not always numeric. `uscpi-august-
/// yoy` puts the month in the middle, and stripping only a trailing date leaves
/// every month its own series — twelve one-event series where there should be
/// one twelve-event series, none of which can pair with the monthly CPI market
/// at either other broker. So dates are removed wherever they appear.
pub fn series_from_event(raw: &RawUsEvent) -> String {
    if let Some(series) = raw.series_slug.as_deref().filter(|s| !s.is_empty()) {
        return series.to_string();
    }
    let slug = first_non_empty([raw.ticker.as_deref(), raw.slug.as_deref()]);
    let stripped = strip_dates(&slug);
    if stripped.is_empty() {
        slug
    } else {
        stripped
    }
}

pub fn normalise_event(raw: &RawUsEvent) -> VenueEvent {
    let event_ticker = first_non_empty([raw.ticker.as_deref(), raw.slug.as_deref()]);
    let parent = MarketParent {
        event_ticker: event_ticker.clone(),
        series_ticker: series_from_event(raw),
    };

    VenueEvent {
        venue: VENUE,
        event_ticker: event_ticker.clone(),
        series_ticker: parent.series_ticker.clone(),
        title: raw.title.clone().unwrap_or(event_ticker),
        sub_title: String::new(),
        category: raw.category.clone().unwrap_or_default(),
        // The exchange states no exclusivity flag. A multi-leg event here is
        // normally a winner-take-all field, but "normally" is not a claim worth
        // making: `EVT` only draws the Σmid check when it is told the legs
        // exclude each other, and a wrong flag would put a false arbitrage on
        // screen.
        mutually_exclusive: false,
        markets: raw
            .markets
            .iter()
            .flatten()
            .map(|market| normalise_market(market, Some(&parent)))
            .collect(),
    }
}

/// Merge a live BBO into a catalogue market.
///
/// The catalogue carries a bid and an ask; `bbo` adds the last print, the open
/// interest and the shares traded, which is the difference between a row in a
/// list and a quote worth acting on.
pub fn apply_bbo(market: Market, raw: &RawUsBbo) -> Market {
    let Some(data) = raw.market_data.as_ref() else {
        return market;
    };

    let yes_bid = quote(data.best_bid.as_ref()).or(market.yes_bid);
    let yes_ask = quote(data.best_ask.as_ref()).or(market.yes_ask);
    let last = quote(data.last_trade_px.as_ref()).or_else(|| quote(data.current_px.as_ref()));

    Market {
        yes_bid,
        yes_ask,
        no_bid: yes_ask.map(|ask| round4(1.0 - ask)),
        no_ask: yes_bid.map(|bid| round4(1.0 - bid)),
        mid: match (yes_bid, yes_ask) {
            (Some(bid), Some(ask)) => Some(round4((bid + ask) / 2.0)),
            _ => last.or(yes_bid).or(yes_ask),
        },
        last_price: last,
        // `sharesTraded` is a lifetime figure, so it belongs in `volume`.
        // Nothing in the public API breaks it down by day, so `volume24h` stays
        // unstated.
        volume: money_of(data.shares_traded.as_ref()),
        open_interest: money_of(data.open_interest.as_ref()),
        ..market
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

async fn get<T>(state: &AppState, path: &str, ttl: Duration) -> Result<Arc<T>>
where
    T: DeserializeOwned + Send + Sync + 'static,
{
    let url = format!("{}{path}", state.config().polymarket_us_api_base);
    state
        .cache()
        .cached(&format!("polymarket-us:{path}"), ttl, || async {
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

#[derive(Debug, Clone, Default, Deserialize)]
struct MarketEnvelope {
    #[serde(default)]
    market: Option<RawUsMarket>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct EventEnvelope {
    #[serde(default)]
    event: Option<RawUsEvent>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct EventsEnvelope {
    #[serde(default)]
    events: Option<Vec<RawUsEvent>>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct SeriesEnvelope {
    #[serde(default)]
    series: Option<Vec<RawUsSeries>>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct RawUsSeries {
    slug: Option<String>,
    title: Option<String>,
}

/* ----------------------------------------------------------------- lookups */

pub async fn get_market(state: &AppState, slug: &str) -> Result<Market> {
    let encoded = urlencoding::encode(slug).into_owned();
    let (catalogue, bbo) = futures::future::join(
        get::<MarketEnvelope>(state, &format!("/v1/market/slug/{encoded}"), ttl::META),
        get::<RawUsBbo>(state, &format!("/v1/markets/{encoded}/bbo"), ttl::QUOTE),
    )
    .await;

    let catalogue = catalogue?;
    let bbo = bbo.map(|raw| (*raw).clone()).unwrap_or_default();

    let Some(raw) = catalogue.market.as_ref() else {
        return Err(
            UpstreamError::not_found(format!("No Polymarket US market with slug {slug}"))
                .with_hint(
                "Polymarket US identifies markets by slug, as in `tec-mlb-champ-2026-09-27-lad`.",
            ),
        );
    };

    // The by-slug record has no parent, so recover the event from the corpus,
    // which is already warm. A miss only costs the two ticker fields.
    let snapshot = corpus_snapshot(state).await?;
    let known = snapshot.markets.iter().find(|m| m.ticker == slug);
    let market = normalise_market(
        raw,
        Some(&MarketParent {
            event_ticker: known.map(|m| m.event_ticker.clone()).unwrap_or_default(),
            series_ticker: known.map(|m| m.series_ticker.clone()).unwrap_or_default(),
        }),
    );

    Ok(apply_bbo(market, &bbo))
}

pub async fn get_event(state: &AppState, slug: &str) -> Result<VenueEvent> {
    let encoded = urlencoding::encode(slug);
    let raw =
        get::<EventEnvelope>(state, &format!("/v1/events/slug/{encoded}"), ttl::QUOTE).await?;

    let Some(event) = raw.event.as_ref() else {
        return Err(UpstreamError::not_found(format!(
            "No Polymarket US event with slug {slug}"
        )));
    };
    Ok(normalise_event(event))
}

pub async fn list_series(state: &AppState) -> Result<Vec<SeriesInfo>> {
    let path = format!("/v1/series{}", qs(&[("limit", "200".into())]));
    let raw = get::<SeriesEnvelope>(state, &path, ttl::CATALOGUE).await?;

    Ok(raw
        .series
        .iter()
        .flatten()
        .map(|series| SeriesInfo {
            venue: VENUE,
            ticker: series.slug.clone().unwrap_or_default(),
            title: first_non_empty([series.title.as_deref(), series.slug.as_deref()]),
            category: String::new(),
            frequency: String::new(),
            tags: Vec::new(),
        })
        .collect())
}

/* -------------------------------------------------------------------- book */

pub fn normalise_order_book(raw: &RawUsBook, ticker: &str, depth: usize) -> OrderBook {
    let rows = |side: Option<&Vec<RawUsBookLevel>>| -> Vec<BookLevel> {
        side.into_iter()
            .flatten()
            .map(|level| BookLevel {
                price: money_of(level.px.as_ref()).unwrap_or(0.0),
                size: money_of(level.qty.as_ref()).unwrap_or(0.0),
            })
            .filter(|level| level.price > 0.0 && level.size > 0.0)
            .collect()
    };

    let data = raw.market_data.as_ref();

    let mut yes = rows(data.and_then(|d| d.bids.as_ref()));
    yes.sort_by(|a, b| b.price.total_cmp(&a.price));
    yes.truncate(depth);

    let mut yes_asks = rows(data.and_then(|d| d.offers.as_ref()));
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

pub async fn get_order_book(state: &AppState, slug: &str, depth: usize) -> Result<OrderBook> {
    let encoded = urlencoding::encode(slug);
    let raw = get::<RawUsBook>(state, &format!("/v1/markets/{encoded}/book"), ttl::QUOTE).await?;
    Ok(normalise_order_book(&raw, slug, depth))
}

/* ------------------------------------------------- what the gateway lacks */

/// Polymarket US publishes no public trade history and no public candles.
///
/// Both live behind `api.polymarket.us`, which requires an account, identity
/// verification and an API key — `POST /v1beta1/report/trades/search` for prints
/// and `POST /v1beta1/report/trades/stats` for OHLC. The terminal holds no
/// credentials, so it says which endpoint exists and what it costs, rather than
/// returning an empty tape that reads as a market nobody trades.
fn unavailable(what: &str, endpoint: &str) -> UpstreamError {
    let mut capitalised = what.chars();
    let capitalised = match capitalised.next() {
        Some(first) => first.to_uppercase().collect::<String>() + capitalised.as_str(),
        None => String::new(),
    };

    UpstreamError::unsupported(format!("Polymarket US publishes no public {what}")).with_hint(
        format!(
            "{capitalised} for this venue is served by {endpoint} on api.polymarket.us, \
             which requires a Polymarket US account and an API key. The terminal reads only \
             public endpoints. Quote and book (DES, OB) work."
        ),
    )
}

pub async fn get_trades(_state: &AppState, _slug: &str, _limit: usize) -> Result<TradesResponse> {
    Err(unavailable(
        "trade tape",
        "POST /v1beta1/report/trades/search",
    ))
}

pub async fn get_candles(
    _state: &AppState,
    _slug: &str,
    _interval: CandleInterval,
    _start_ts: i64,
    _end_ts: i64,
) -> Result<CandlesResponse> {
    Err(unavailable(
        "price history",
        "POST /v1beta1/report/trades/stats",
    ))
}

/* ----------------------------------------------------------------- corpus */

const CORPUS_KEY: &str = "polymarket-us:corpus";

/// The crawl is partitioned by category, and the reason is bandwidth.
///
/// Polymarket US embeds a full team record — logos, colours, three providers'
/// ids — on both sides of every sports market, so 500 sports events weigh 25 MB
/// against 3 MB for 500 political ones. The open universe is ~2,300 events and
/// ~90% of them are sports, most of them table-tennis and ITF fixtures with one
/// market each.
///
/// A flat page cap would therefore be paid almost entirely in table tennis, and
/// because the gateway returns categories interleaved rather than grouped, the
/// truncation would fall across every category at once — which is exactly how
/// the Kalshi crawl once hid half the entertainment book. Asking per category
/// instead means the small ones always complete, and only the whale is capped.
const CORPUS_PAGE: usize = 100;

/// Pages per category, and the tighter cap sports is held to.
const CORPUS_MAX_PAGES: usize = 8;
const CORPUS_MAX_SPORTS_PAGES: usize = 4;

/// A page of sports events runs to ~5 MB, well past the scraper default.
const CORPUS_MAX_BYTES: usize = 48 * 1024 * 1024;

/// Categories known to exist, seeded so a quiet one is never missed.
///
/// The live set is discovered too — a category invented next week is crawled
/// without a code change — but discovery reads one page, and a category with no
/// event on that page would otherwise go unasked-for entirely.
const SEED_CATEGORIES: &[&str] = &[
    "politics",
    "macro",
    "crypto",
    "finance",
    "technology",
    "science",
    "geopolitics",
    "culture",
    "sports",
];

async fn event_page(state: &AppState, category: &str, offset: usize) -> Result<Vec<RawUsEvent>> {
    let path = format!(
        "/v1/events{}",
        qs(&[
            ("limit", CORPUS_PAGE.to_string()),
            ("offset", offset.to_string()),
            ("active", "true".into()),
            ("closed", "false".into()),
            ("categories", category.to_string()),
        ])
    );

    let url = format!("{}{path}", state.config().polymarket_us_api_base);
    let raw: EventsEnvelope = state
        .http()
        .fetch_json(
            &url,
            FetchOptions::new()
                .timeout(Duration::from_secs(30))
                .retries(2)
                .max_bytes(CORPUS_MAX_BYTES),
        )
        .await?;

    Ok(raw.events.unwrap_or_default())
}

pub async fn build_corpus(state: &AppState) -> Result<Corpus> {
    let mut events: Vec<VenueEvent> = Vec::new();
    let mut markets: Vec<Market> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut truncated = false;

    let mut take =
        |rows: Vec<RawUsEvent>, events: &mut Vec<VenueEvent>, markets: &mut Vec<Market>| {
            for raw in rows {
                let key = first_non_empty([raw.slug.as_deref(), raw.ticker.as_deref()]);
                if key.is_empty() || raw.hidden == Some(true) || !seen.insert(key) {
                    continue;
                }
                let event = normalise_event(&raw);
                markets.extend(event.markets.iter().cloned());
                events.push(event);
            }
        };

    let probe = event_page(state, "", 0).await?;

    let mut categories: Vec<String> = SEED_CATEGORIES.iter().map(|c| (*c).to_string()).collect();
    for raw in &probe {
        if let Some(category) = raw.category.as_deref().filter(|c| !c.is_empty()) {
            if !categories.iter().any(|known| known == category) {
                categories.push(category.to_string());
            }
        }
    }

    take(probe, &mut events, &mut markets);

    for category in categories {
        let cap = if category == "sports" {
            CORPUS_MAX_SPORTS_PAGES
        } else {
            CORPUS_MAX_PAGES
        };
        for page in 0..cap {
            let rows = event_page(state, &category, page * CORPUS_PAGE).await?;
            let count = rows.len();
            take(rows, &mut events, &mut markets);
            if count < CORPUS_PAGE {
                break;
            }
            if page == cap - 1 {
                truncated = true;
            }
        }
    }

    Ok(Corpus::new(VENUE, events, markets, truncated))
}

pub async fn corpus_snapshot(state: &AppState) -> Result<Arc<Corpus>> {
    state
        .cache()
        .cached(CORPUS_KEY, ttl::CATALOGUE, || async {
            build_corpus(state).await
        })
        .await
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
                "[polymarket-us] corpus warm"
            ),
            Err(err) => tracing::warn!(error = %err, "[polymarket-us] corpus warm failed"),
        }
    });
}

/// Search the local snapshot rather than the gateway's own `/v1/search`.
///
/// That endpoint exists, and it answers `fed` with "Seeman Jan vs Dufek Jakub
/// Jr" — a tennis fixture — while its own `fed-decision` series goes
/// unmentioned. Ranking the snapshot the same way as the other two venues gives
/// results that are both usable and comparable across brokers.
pub async fn search(state: &AppState, query: &str, limit: usize) -> Result<SearchResponse> {
    let snapshot = corpus_snapshot(state).await?;
    refuse_overlong_query(query)?;
    Ok(search_corpus(&snapshot, query, limit))
}

/// Leaderboards over the snapshot.
///
/// Every sort but the movers comes back empty here: the public catalogue states
/// no volume, open interest or resting depth, and [`rank_markets`] drops a
/// market rather than ranking an unpublished figure as zero.
pub async fn top_markets(state: &AppState, sort: MoverSort, limit: usize) -> Result<Vec<Market>> {
    let snapshot = corpus_snapshot(state).await?;
    Ok(rank_markets(&snapshot.markets, sort, limit))
}

/* ------------------------------------------------------------------ tests */

/// Polymarket US normalisation tests.
///
/// The fixtures are trimmed copies of real `gateway.polymarket.us` responses,
/// so the `{value, currency}` money objects and the decimal-string quantities
/// are exactly what the upstream sends — including the inconsistent
/// `outcomes`/`outcomePrices` pair that the normaliser deliberately ignores.
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{method, path, query_param, query_param_is_missing};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use crate::config::Config;

    fn market_of(value: Value) -> RawUsMarket {
        serde_json::from_value(value).expect("the fixture parses as a market")
    }

    fn event_of(value: Value) -> RawUsEvent {
        serde_json::from_value(value).expect("the fixture parses as an event")
    }

    fn bbo_of(value: Value) -> RawUsBbo {
        serde_json::from_value(value).expect("the fixture parses as a bbo")
    }

    fn book_of(value: Value) -> RawUsBook {
        serde_json::from_value(value).expect("the fixture parses as a book")
    }

    /* -------------------------------------------------------------- coercion */

    #[test]
    fn money_unwraps_a_value_currency_object() {
        assert_eq!(
            money(&json!({ "value": "0.4600", "currency": "USD" })),
            Some(0.46)
        );
    }

    #[test]
    fn money_reads_a_bare_decimal_string_as_quantities_arrive() {
        assert_eq!(money(&json!("64154.0000")), Some(64154.0));
    }

    #[test]
    fn money_returns_null_for_absent_or_unparseable_values() {
        assert_eq!(money_of(None), None);
        assert_eq!(money(&Value::Null), None);
        assert_eq!(money(&json!("")), None);
        assert_eq!(money(&json!({ "value": "" })), None);
        assert_eq!(money(&json!("n/a")), None);
    }

    #[test]
    fn money_keeps_a_real_zero_a_settled_contract_can_be_worth_nothing() {
        assert_eq!(money(&json!("0")), Some(0.0));
    }

    /* --------------------------------------------------------------- markets */

    /// Real shape, trimmed. Note `outcomePrices`: on this endpoint it holds
    /// `[askToBuyYes, askToBuyNo]`, and in a nested event listing the *same
    /// market* holds `[bestBid, bestAsk]`. Neither is read.
    fn raw() -> Value {
        json!({
            "slug": "tec-mlb-nlchamp-2026-09-27-lad",
            "question": "National League Champion",
            "title": "Los Angeles Dodgers",
            "subtitle": "",
            "description": "Will Los Angeles Dodgers win the 2026 National League pennant…",
            "category": "sports",
            "marketType": "futures",
            "status": "MARKET_STATUS_OPEN",
            "active": true,
            "closed": false,
            "startDate": "2026-03-19T14:32:23Z",
            "endDate": "2026-11-06T16:20:09Z",
            "bestBidQuote": { "value": "0.4600" },
            "bestAskQuote": { "value": "0.4780" },
            "outcomes": "[\"Yes\",\"No\"]",
            "outcomePrices": "[\"0.4780\",\"0.54\"]",
            "marketSides": [
                { "description": "Yes", "long": true, "price": "0.4780", "quote": { "value": "0.4780" } },
                { "description": "No", "long": false, "price": "0.54", "quote": { "value": "0.54" } }
            ]
        })
    }

    fn raw_with(key: &str, value: Value) -> Value {
        let mut raw = raw();
        raw[key] = value;
        raw
    }

    #[test]
    fn quotes_from_best_bid_ask_quote_and_ignores_outcome_prices() {
        let market = normalise_market(&market_of(raw()), None);
        assert_eq!(market.venue, Venue::PolymarketUs);
        assert_eq!(market.yes_bid, Some(0.46));
        assert_eq!(market.yes_ask, Some(0.478));
        assert_eq!(market.mid, Some(0.469));
    }

    #[test]
    fn is_unmoved_by_outcome_prices_meaning_something_else() {
        // Same market, the event-listing spelling of the same fields. The quote
        // must not change with the endpoint it was read from.
        let listing = normalise_market(
            &market_of(raw_with("outcomePrices", json!("[\"0.4600\",\"0.4780\"]"))),
            None,
        );
        assert_eq!(listing.yes_bid, Some(0.46));
        assert_eq!(listing.yes_ask, Some(0.478));
    }

    #[test]
    fn is_unmoved_by_the_outcomes_array_being_ordered_either_way() {
        // The gateway labels one market ["Yes","No"] and its neighbour
        // ["No","Yes"] with both price arrays still in the same order.
        let flipped = normalise_market(
            &market_of(raw_with("outcomes", json!("[\"No\",\"Yes\"]"))),
            None,
        );
        assert_eq!(flipped.yes_bid, Some(0.46));
        assert_eq!(flipped.yes_ask, Some(0.478));
    }

    #[test]
    fn derives_the_no_side_from_the_yes_book() {
        let market = normalise_market(&market_of(raw()), None);
        assert_eq!(market.no_bid, Some(0.522));
        assert_eq!(market.no_ask, Some(0.54));
    }

    #[test]
    fn leaves_the_figures_the_public_catalogue_does_not_publish_unstated() {
        let market = normalise_market(&market_of(raw()), None);
        // Not zero: the exchange publishes none of these without an API key,
        // and a zero in a volume column reads as a dead market.
        assert_eq!(market.volume, None);
        assert_eq!(market.volume24h, None);
        assert_eq!(market.open_interest, None);
        assert_eq!(market.liquidity, None);
        assert_eq!(market.last_price, None);
        assert_eq!(market.change, None);
    }

    #[test]
    fn unwraps_the_status_enum() {
        assert_eq!(normalise_market(&market_of(raw()), None).status, "open");
        assert_eq!(
            normalise_market(
                &market_of(raw_with("status", json!("MARKET_STATUS_CLOSED"))),
                None
            )
            .status,
            "closed"
        );

        let mut unstated = raw_with("closed", json!(true));
        unstated
            .as_object_mut()
            .expect("the fixture is an object")
            .remove("status");
        assert_eq!(
            normalise_market(&market_of(unstated), None).status,
            "closed"
        );
    }

    #[test]
    fn labels_the_legs_from_the_market_title_and_its_short_side() {
        let market = normalise_market(&market_of(raw()), None);
        assert_eq!(market.yes_sub_title, "Los Angeles Dodgers");
        assert_eq!(market.no_sub_title, "No");
        assert_eq!(
            normalise_market(&market_of(raw_with("subtitle", json!("to win"))), None).yes_sub_title,
            "Los Angeles Dodgers · to win"
        );
    }

    #[test]
    fn reports_an_empty_book_side_as_absent_not_as_a_zero_quote() {
        let mut empty = raw_with("bestBidQuote", json!({ "value": "0" }));
        empty["bestAskQuote"] = json!({ "value": "" });

        let market = normalise_market(&market_of(empty), None);
        assert_eq!(market.yes_bid, None);
        assert_eq!(market.yes_ask, None);
        assert_eq!(market.mid, None);
    }

    /* ------------------------------------------------------------- apply_bbo */

    fn bbo() -> RawUsBbo {
        bbo_of(json!({
            "marketData": {
                "marketSlug": "tec-mlb-nlchamp-2026-09-27-lad",
                "currentPx": { "value": "0.4690" },
                "lastTradePx": { "value": "0.4670" },
                "sharesTraded": "342.0000",
                "openInterest": "64154.0000",
                "bestBid": { "value": "0.4600" },
                "bestAsk": { "value": "0.4780" }
            }
        }))
    }

    fn base() -> Market {
        normalise_market(
            &market_of(json!({
                "slug": "tec-mlb-nlchamp-2026-09-27-lad",
                "bestBidQuote": { "value": "0.4000" },
                "bestAskQuote": { "value": "0.5000" }
            })),
            None,
        )
    }

    #[test]
    fn prefers_the_live_book_over_the_catalogue_snapshot() {
        let market = apply_bbo(base(), &bbo());
        assert_eq!(market.yes_bid, Some(0.46));
        assert_eq!(market.yes_ask, Some(0.478));
        assert_eq!(market.no_bid, Some(0.522));
    }

    #[test]
    fn fills_in_the_last_print_open_interest_and_lifetime_volume() {
        let market = apply_bbo(base(), &bbo());
        assert_eq!(market.last_price, Some(0.467));
        assert_eq!(market.open_interest, Some(64154.0));
        assert_eq!(market.volume, Some(342.0));
        // Nothing public breaks volume down by day, so this stays unstated.
        assert_eq!(market.volume24h, None);
    }

    #[test]
    fn leaves_the_market_untouched_when_the_bbo_is_empty() {
        let untouched = apply_bbo(base(), &RawUsBbo::default());
        assert_eq!(
            serde_json::to_value(&untouched).unwrap(),
            serde_json::to_value(base()).unwrap()
        );
    }

    /* ---------------------------------------------------------------- events */

    #[test]
    fn prefers_the_series_the_exchange_states() {
        assert_eq!(
            series_from_event(&event_of(json!({
                "slug": "mlb-champ-2026-09-27",
                "seriesSlug": "mlb-2026"
            }))),
            "mlb-2026"
        );
    }

    #[test]
    fn falls_back_to_the_slug_stem_which_is_where_the_fomc_book_lives() {
        // The exchange states no series on its rate or election books, and the
        // two October and December FOMC events have to group together
        // regardless.
        assert_eq!(
            series_from_event(&event_of(json!({ "slug": "usfed-fomc-2026-10-28" }))),
            "usfed-fomc"
        );
        assert_eq!(
            series_from_event(&event_of(json!({ "slug": "usfed-fomc-2026-12-09" }))),
            "usfed-fomc"
        );
        assert_eq!(
            series_from_event(&event_of(json!({ "slug": "jerpowgov" }))),
            "jerpowgov"
        );
    }

    #[test]
    fn stamps_its_event_and_series_onto_every_nested_market() {
        let event = normalise_event(&event_of(json!({
            "slug": "usfed-fomc-2026-10-28",
            "title": "Fed Decision in October",
            "category": "macro",
            "markets": [
                { "slug": "a", "title": "No Change", "bestBidQuote": { "value": "0.70" } }
            ]
        })));

        assert_eq!(event.venue, Venue::PolymarketUs);
        assert_eq!(event.series_ticker, "usfed-fomc");
        assert_eq!(event.markets[0].event_ticker, "usfed-fomc-2026-10-28");
        assert_eq!(event.markets[0].series_ticker, "usfed-fomc");
    }

    #[test]
    fn claims_no_exclusivity_because_the_exchange_states_none() {
        // `EVT` only draws its Σmid arbitrage check when told the legs exclude
        // each other; a guessed flag would put a false arbitrage on screen.
        let event = normalise_event(&event_of(json!({
            "slug": "x",
            "markets": [{ "slug": "a" }, { "slug": "b" }]
        })));
        assert!(!event.mutually_exclusive);
    }

    /* ------------------------------------------------------------------ book */

    fn raw_book() -> RawUsBook {
        book_of(json!({
            "marketData": {
                "marketSlug": "tec-mlb-nlchamp-2026-09-27-lad",
                "bids": [
                    { "px": { "value": "0.4400" }, "qty": "505.0000" },
                    { "px": { "value": "0.4600" }, "qty": "150.0000" },
                    { "px": { "value": "0.4360" }, "qty": "3458.0000" }
                ],
                "offers": [
                    { "px": { "value": "0.4800" }, "qty": "998.0000" },
                    { "px": { "value": "0.4780" }, "qty": "11000.0000" },
                    { "px": { "value": "0.4790" }, "qty": "60.0000" }
                ]
            }
        }))
    }

    #[test]
    fn sorts_bids_best_first_and_offers_cheapest_first() {
        let book = normalise_order_book(&raw_book(), "slug", 12);
        let bids: Vec<f64> = book.yes.iter().map(|l| l.price).collect();
        let asks: Vec<f64> = book.yes_asks.iter().map(|l| l.price).collect();
        assert_eq!(bids, vec![0.46, 0.44, 0.436]);
        assert_eq!(asks, vec![0.478, 0.479, 0.48]);
    }

    #[test]
    fn derives_the_no_ladder_and_the_top_of_book() {
        let book = normalise_order_book(&raw_book(), "slug", 12);
        assert_eq!(book.no[0].price, 0.522);
        assert_eq!(book.best_yes_bid, Some(0.46));
        assert_eq!(book.best_yes_ask, Some(0.478));
        assert_eq!(book.spread, Some(0.018));
        assert_eq!(book.mid, Some(0.469));
        assert_eq!(book.venue, Venue::PolymarketUs);
    }

    #[test]
    fn reports_nulls_rather_than_a_fake_quote_for_an_empty_book() {
        let book = normalise_order_book(&RawUsBook::default(), "slug", 12);
        assert!(book.yes.is_empty());
        assert_eq!(book.best_yes_ask, None);
        assert_eq!(book.mid, None);
    }

    #[test]
    fn drops_zero_price_and_zero_size_rows() {
        let book = normalise_order_book(
            &book_of(json!({
                "marketData": {
                    "bids": [
                        { "px": { "value": "0" }, "qty": "100" },
                        { "px": { "value": "0.5" }, "qty": "0" },
                        { "px": { "value": "0.4" }, "qty": "10" }
                    ]
                }
            })),
            "slug",
            12,
        );
        assert_eq!(book.yes.len(), 1);
        assert_eq!(book.yes[0].price, 0.4);
    }

    /* ------------------------------------------------------------- the wire */

    fn state_for(server: &MockServer) -> AppState {
        AppState::new(Config {
            polymarket_us_api_base: server.uri(),
            ..Config::default()
        })
    }

    /// One event carrying the market `get_market` asks for, so the corpus can
    /// hand back its event and series tickers.
    fn corpus_page() -> Value {
        json!({
            "events": [{
                "slug": "tec-mlb-nlchamp-2026-09-27",
                "ticker": "tec-mlb-nlchamp-2026-09-27",
                "title": "National League Champion",
                "category": "sports",
                "markets": [{
                    "slug": "tec-mlb-nlchamp-2026-09-27-lad",
                    "title": "Los Angeles Dodgers",
                    "bestBidQuote": { "value": "0.4000" },
                    "bestAskQuote": { "value": "0.5000" }
                }]
            }]
        })
    }

    async fn mount_corpus(server: &MockServer) {
        Mock::given(method("GET"))
            .and(path("/v1/events"))
            .respond_with(ResponseTemplate::new(200).set_body_json(corpus_page()))
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn get_market_merges_the_catalogue_the_live_bbo_and_the_corpus() {
        let server = MockServer::start().await;
        mount_corpus(&server).await;

        Mock::given(method("GET"))
            .and(path("/v1/market/slug/tec-mlb-nlchamp-2026-09-27-lad"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "market": raw() })))
            .expect(1)
            .mount(&server)
            .await;

        Mock::given(method("GET"))
            .and(path("/v1/markets/tec-mlb-nlchamp-2026-09-27-lad/bbo"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "marketData": {
                    "lastTradePx": { "value": "0.4670" },
                    "sharesTraded": "342.0000",
                    "openInterest": "64154.0000",
                    "bestBid": { "value": "0.4600" },
                    "bestAsk": { "value": "0.4780" }
                }
            })))
            .expect(1)
            .mount(&server)
            .await;

        let market = get_market(&state_for(&server), "tec-mlb-nlchamp-2026-09-27-lad")
            .await
            .expect("the market resolves");

        assert_eq!(market.ticker, "tec-mlb-nlchamp-2026-09-27-lad");
        // Recovered from the corpus, which is the only place the parent is
        // stated.
        assert_eq!(market.event_ticker, "tec-mlb-nlchamp-2026-09-27");
        assert_eq!(market.series_ticker, "tec-mlb-nlchamp");
        assert_eq!(market.last_price, Some(0.467));
        assert_eq!(market.volume, Some(342.0));
        assert_eq!(market.open_interest, Some(64154.0));
        assert_eq!(market.volume24h, None);
    }

    #[tokio::test]
    async fn get_market_survives_a_bbo_that_will_not_answer() {
        let server = MockServer::start().await;
        mount_corpus(&server).await;

        Mock::given(method("GET"))
            .and(path("/v1/market/slug/tec-mlb-nlchamp-2026-09-27-lad"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "market": raw() })))
            .mount(&server)
            .await;

        // No `bbo` mock: the quote endpoint 404s and the catalogue quote stands.
        let market = get_market(&state_for(&server), "tec-mlb-nlchamp-2026-09-27-lad")
            .await
            .expect("the market resolves without a bbo");

        assert_eq!(market.yes_bid, Some(0.46));
        assert_eq!(market.last_price, None);
    }

    #[tokio::test]
    async fn get_market_reports_an_unknown_slug_as_not_found() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/market/slug/nope"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
            .mount(&server)
            .await;

        let err = get_market(&state_for(&server), "nope")
            .await
            .expect_err("an unlisted slug is not found");
        assert_eq!(err.code, crate::error::codes::NOT_FOUND);
        assert!(err.message.contains("nope"));
        assert!(err.hint.unwrap().contains("slug"));
    }

    #[tokio::test]
    async fn get_event_normalises_every_nested_market() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/events/slug/usfed-fomc-2026-10-28"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "event": {
                    "slug": "usfed-fomc-2026-10-28",
                    "title": "Fed Decision in October",
                    "category": "macro",
                    "markets": [
                        { "slug": "a", "title": "No Change", "bestBidQuote": { "value": "0.70" } }
                    ]
                }
            })))
            .mount(&server)
            .await;

        let event = get_event(&state_for(&server), "usfed-fomc-2026-10-28")
            .await
            .expect("the event resolves");
        assert_eq!(event.series_ticker, "usfed-fomc");
        assert_eq!(event.markets.len(), 1);
        assert_eq!(event.markets[0].yes_bid, Some(0.7));
    }

    #[tokio::test]
    async fn get_event_reports_an_unknown_slug_as_not_found() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/events/slug/nope"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
            .mount(&server)
            .await;

        let err = get_event(&state_for(&server), "nope")
            .await
            .expect_err("an unlisted slug is not found");
        assert_eq!(err.code, crate::error::codes::NOT_FOUND);
    }

    #[tokio::test]
    async fn list_series_asks_for_two_hundred_and_titles_them() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/series"))
            .and(query_param("limit", "200"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "series": [
                    { "slug": "mlb-2026", "title": "MLB 2026", "active": true },
                    { "slug": "usfed-fomc" }
                ]
            })))
            .expect(1)
            .mount(&server)
            .await;

        let series = list_series(&state_for(&server)).await.expect("series list");
        assert_eq!(series.len(), 2);
        assert_eq!(series[0].ticker, "mlb-2026");
        assert_eq!(series[0].title, "MLB 2026");
        // No title stated: the slug is the only name there is.
        assert_eq!(series[1].title, "usfed-fomc");
        assert_eq!(series[1].venue, Venue::PolymarketUs);
    }

    #[tokio::test]
    async fn get_order_book_reads_the_book_endpoint_and_is_cached_by_path() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/markets/slug/book"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "marketData": {
                    "bids": [{ "px": { "value": "0.4600" }, "qty": "150.0000" }],
                    "offers": [{ "px": { "value": "0.4780" }, "qty": "11000.0000" }]
                }
            })))
            // Two reads, one request: the second is served from
            // `polymarket-us:/v1/markets/slug/book`.
            .expect(1)
            .mount(&server)
            .await;

        let state = state_for(&server);
        let book = get_order_book(&state, "slug", 12).await.expect("a book");
        assert_eq!(book.best_yes_bid, Some(0.46));
        assert_eq!(book.best_yes_ask, Some(0.478));
        assert_eq!(book.no[0].price, 0.522);

        get_order_book(&state, "slug", 12).await.expect("a book");
    }

    #[tokio::test]
    async fn the_book_is_read_from_the_cache_key_the_typescript_used() {
        // No mock at all: if the key were spelled differently this would go to
        // the network and 404.
        let server = MockServer::start().await;
        let state = state_for(&server);
        state
            .cache()
            .cached(
                "polymarket-us:/v1/markets/slug/book",
                ttl::QUOTE,
                || async {
                    Ok(book_of(json!({
                        "marketData": {
                            "bids": [{ "px": { "value": "0.3000" }, "qty": "5" }]
                        }
                    })))
                },
            )
            .await
            .expect("the entry is admitted");

        let book = get_order_book(&state, "slug", 12)
            .await
            .expect("the cached book");
        assert_eq!(book.best_yes_bid, Some(0.3));
    }

    #[tokio::test]
    async fn get_order_book_honours_the_depth_it_is_given() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/markets/deep/book"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "marketData": {
                    "bids": [
                        { "px": { "value": "0.10" }, "qty": "1" },
                        { "px": { "value": "0.20" }, "qty": "1" },
                        { "px": { "value": "0.30" }, "qty": "1" }
                    ]
                }
            })))
            .mount(&server)
            .await;

        let book = get_order_book(&state_for(&server), "deep", 2)
            .await
            .expect("a book");
        assert_eq!(book.yes.len(), 2);
        assert_eq!(book.yes[0].price, 0.3);
    }

    /* ------------------------------------------- what the gateway lacks */

    #[tokio::test]
    async fn the_tape_says_which_authenticated_endpoint_serves_it() {
        let server = MockServer::start().await;
        let err = get_trades(&state_for(&server), "slug", 50)
            .await
            .expect_err("there is no public tape");

        assert_eq!(err.code, crate::error::codes::UNSUPPORTED);
        // 501, not an empty tape that reads as a market nobody trades.
        assert_eq!(err.http_status(), axum::http::StatusCode::NOT_IMPLEMENTED);
        assert_eq!(err.message, "Polymarket US publishes no public trade tape");

        let hint = err.hint.expect("the gap is explained");
        assert!(hint.starts_with("Trade tape for this venue is served by "));
        assert!(hint.contains("POST /v1beta1/report/trades/search"));
        assert!(hint.contains("api.polymarket.us"));
        assert!(hint.contains("Quote and book (DES, OB) work."));
    }

    #[tokio::test]
    async fn the_chart_says_which_authenticated_endpoint_serves_it() {
        let server = MockServer::start().await;
        let err = get_candles(
            &state_for(&server),
            "slug",
            CandleInterval::OneHour,
            0,
            1_000,
        )
        .await
        .expect_err("there is no public price history");

        assert_eq!(err.code, crate::error::codes::UNSUPPORTED);
        assert_eq!(
            err.message,
            "Polymarket US publishes no public price history"
        );

        let hint = err.hint.expect("the gap is explained");
        assert!(hint.starts_with("Price history for this venue is served by "));
        assert!(hint.contains("POST /v1beta1/report/trades/stats"));
    }

    /* ---------------------------------------------------------------- corpus */

    #[tokio::test]
    async fn the_crawl_asks_each_category_for_its_own_pages() {
        let server = MockServer::start().await;

        // The probe: one page, no category filter. It names a category the
        // seed list does not, which must then be crawled in its own right.
        Mock::given(method("GET"))
            .and(path("/v1/events"))
            .and(query_param_is_missing("categories"))
            .and(query_param("limit", "100"))
            .and(query_param("offset", "0"))
            .and(query_param("active", "true"))
            .and(query_param("closed", "false"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "events": [
                    { "slug": "usfed-fomc-2026-10-28", "category": "macro", "markets": [{ "slug": "a" }] },
                    { "slug": "wx-nyc-high-2026-10-28", "category": "weather", "markets": [{ "slug": "b" }] },
                    { "slug": "hidden-one", "category": "macro", "hidden": true, "markets": [{ "slug": "c" }] }
                ]
            })))
            .expect(1)
            .mount(&server)
            .await;

        // Discovered from the probe, and asked for even though it is not seeded.
        Mock::given(method("GET"))
            .and(path("/v1/events"))
            .and(query_param("categories", "weather"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "events": [
                    { "slug": "wx-chi-high-2026-10-28", "category": "weather", "markets": [{ "slug": "d" }] }
                ]
            })))
            .expect(1)
            .mount(&server)
            .await;

        // A seeded category that returns a row already seen in the probe.
        Mock::given(method("GET"))
            .and(path("/v1/events"))
            .and(query_param("categories", "macro"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "events": [
                    { "slug": "usfed-fomc-2026-10-28", "category": "macro", "markets": [{ "slug": "a" }] }
                ]
            })))
            .expect(1)
            .mount(&server)
            .await;

        Mock::given(method("GET"))
            .and(path("/v1/events"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "events": [] })))
            .mount(&server)
            .await;

        let corpus = build_corpus(&state_for(&server))
            .await
            .expect("the crawl completes");

        // Two from the probe, one from the weather page; the hidden row is
        // skipped and the repeated macro row is deduped by slug.
        assert_eq!(corpus.events.len(), 3);
        assert_eq!(corpus.markets.len(), 3);
        assert!(!corpus.truncated);
        assert_eq!(corpus.venue, Venue::PolymarketUs);
    }

    #[tokio::test]
    async fn sports_is_held_to_the_tighter_page_cap() {
        let server = MockServer::start().await;

        let full_page = |prefix: &str| -> Value {
            let events: Vec<Value> = (0..CORPUS_PAGE)
                .map(|i| json!({ "slug": format!("{prefix}-{i}"), "markets": [] }))
                .collect();
            json!({ "events": events })
        };

        Mock::given(method("GET"))
            .and(path("/v1/events"))
            .and(query_param("categories", "sports"))
            .respond_with(ResponseTemplate::new(200).set_body_json(full_page("sport")))
            .expect(u64::try_from(CORPUS_MAX_SPORTS_PAGES).unwrap())
            .mount(&server)
            .await;

        Mock::given(method("GET"))
            .and(path("/v1/events"))
            .and(query_param("categories", "politics"))
            .respond_with(ResponseTemplate::new(200).set_body_json(full_page("pol")))
            .expect(u64::try_from(CORPUS_MAX_PAGES).unwrap())
            .mount(&server)
            .await;

        Mock::given(method("GET"))
            .and(path("/v1/events"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "events": [] })))
            .mount(&server)
            .await;

        let corpus = build_corpus(&state_for(&server))
            .await
            .expect("the crawl completes");

        // Every page repeats the same slugs, so dedupe leaves one page of each.
        assert_eq!(corpus.events.len(), CORPUS_PAGE * 2);
        // Both categories were still answering when their cap ran out.
        assert!(corpus.truncated);
    }

    #[tokio::test]
    async fn the_snapshot_is_built_once_and_shared() {
        let server = MockServer::start().await;
        mount_corpus(&server).await;

        let state = state_for(&server);
        let first = corpus_snapshot(&state).await.expect("a snapshot");
        let second = corpus_snapshot(&state).await.expect("a snapshot");
        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(first.events.len(), 1);
    }

    #[tokio::test]
    async fn top_markets_ranks_nothing_because_the_catalogue_states_nothing() {
        let server = MockServer::start().await;
        mount_corpus(&server).await;

        // The catalogue publishes no volume, change, open interest or depth, so
        // every board is empty rather than ordered on an unpublished zero.
        for sort in MoverSort::ALL {
            let markets = top_markets(&state_for(&server), *sort, 25)
                .await
                .expect("a board");
            assert!(markets.is_empty(), "{sort}");
        }
    }

    #[tokio::test]
    async fn search_reads_the_snapshot_rather_than_the_gateway() {
        let server = MockServer::start().await;
        mount_corpus(&server).await;

        let response = search(&state_for(&server), "national league", 25)
            .await
            .expect("a search");
        assert_eq!(response.query, "national league");
    }
}
