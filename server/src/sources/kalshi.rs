//! Kalshi public trade-api v2 client.
//!
//! The upstream speaks fixed-point decimal strings — `"0.6900"` for prices
//! (suffix `_dollars`) and `"12645.98"` for sizes and volumes (suffix `_fp`).
//! Everything here converts to plain numbers so the rest of the codebase never
//! has to think about it again.

use std::sync::Arc;
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::Value;
use terminal_core::types::{
    BookLevel, Candle, CandleInterval, CandlesResponse, EventsResponse, Market, MarketsResponse,
    OrderBook, SeriesInfo, StrikeType, Trade, TradesResponse, Venue, VenueEvent,
};
use terminal_core::util::round4;
use terminal_core::venue::MoverSort;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use crate::app::AppState;
use crate::cache::ttl;
use crate::error::{Result, UpstreamError};
use crate::http::FetchOptions;
use crate::sources::corpus::{
    rank_markets, refuse_overlong_query, search_corpus, Corpus, SearchResponse,
};

/* --------------------------------------------------------------- coercion */

/// Parse a fixed-point decimal string. Returns `None` for absent/garbage.
fn num(value: &Value) -> Option<f64> {
    match value {
        Value::Number(n) => n.as_f64().filter(|n| n.is_finite()),
        Value::String(s) if !s.is_empty() => s.trim().parse::<f64>().ok().filter(|n| n.is_finite()),
        _ => None,
    }
}

/// One figure as Kalshi sends it: a fixed-point decimal string (`"0.6900"`,
/// `"12645.98"`), a bare JSON number, or nothing at all.
///
/// Parsing happens once, on the way in, but the *reading* is deliberately left
/// to the call site — [`Fixed::num`], [`Fixed::num0`] and [`Fixed::price`]
/// disagree about what a missing figure and a zero mean, and all three
/// disagreements matter.
#[derive(Debug, Clone, Copy, Default, PartialEq, Deserialize)]
#[serde(from = "serde_json::Value")]
pub struct Fixed(Option<f64>);

impl From<Value> for Fixed {
    fn from(value: Value) -> Self {
        Fixed(num(&value))
    }
}

impl Fixed {
    /// The figure, or `None` when the upstream did not send one.
    pub fn num(self) -> Option<f64> {
        self.0
    }

    /// Same as [`Fixed::num`] but collapses missing values to 0.
    ///
    /// Only for figures Kalshi always sends and where an absent field genuinely
    /// means none — book sizes and candle volumes. A market's headline volume
    /// and open interest use [`Fixed::num`], because `null` there means "this
    /// venue does not publish it", a distinction the other two venues make real.
    pub fn num0(self) -> f64 {
        self.0.unwrap_or(0.0)
    }

    /// A price of exactly 0 on a Kalshi book means "no resting order", not
    /// "this contract is worthless" — an empty side reports `"0.0000"`. Treat
    /// it as absent so the terminal renders `--` rather than a fake 0¢ quote.
    pub fn price(self) -> Option<f64> {
        self.0.filter(|n| *n != 0.0)
    }
}

/* ------------------------------------------------------------ raw upstream */

#[derive(Debug, Clone, Default, Deserialize)]
struct RawMarket {
    #[serde(default)]
    ticker: String,
    #[serde(default)]
    event_ticker: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    yes_sub_title: Option<String>,
    #[serde(default)]
    no_sub_title: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    market_type: Option<String>,
    #[serde(default)]
    yes_bid_dollars: Fixed,
    #[serde(default)]
    yes_ask_dollars: Fixed,
    #[serde(default)]
    no_bid_dollars: Fixed,
    #[serde(default)]
    no_ask_dollars: Fixed,
    #[serde(default)]
    last_price_dollars: Fixed,
    #[serde(default)]
    previous_price_dollars: Fixed,
    #[serde(default)]
    volume_fp: Fixed,
    #[serde(default)]
    volume_24h_fp: Fixed,
    #[serde(default)]
    open_interest_fp: Fixed,
    #[serde(default)]
    liquidity_dollars: Fixed,
    #[serde(default)]
    open_time: Option<String>,
    #[serde(default)]
    close_time: Option<String>,
    #[serde(default)]
    expiration_time: Option<String>,
    #[serde(default)]
    result: Option<String>,
    #[serde(default)]
    rules_primary: Option<String>,
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    strike_type: Option<String>,
    #[serde(default)]
    floor_strike: Fixed,
    #[serde(default)]
    cap_strike: Fixed,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct RawEvent {
    #[serde(default)]
    event_ticker: String,
    #[serde(default)]
    series_ticker: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    sub_title: Option<String>,
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    mutually_exclusive: Option<bool>,
    #[serde(default)]
    markets: Option<Vec<RawMarket>>,
}

/// One candlestick bucket, as `/candlesticks` sends it.
///
/// A bucket in which nothing printed carries only `price.previous_dollars`;
/// [`normalise_candles`] is where that becomes a flat bar rather than a gap.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct RawCandle {
    #[serde(default)]
    pub end_period_ts: i64,
    #[serde(default)]
    pub open_interest_fp: Fixed,
    #[serde(default)]
    pub volume_fp: Fixed,
    #[serde(default)]
    pub price: Option<RawCandlePrice>,
    #[serde(default)]
    pub yes_bid: Option<RawCandleBook>,
    #[serde(default)]
    pub yes_ask: Option<RawCandleBook>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct RawCandlePrice {
    #[serde(default)]
    pub open_dollars: Fixed,
    #[serde(default)]
    pub high_dollars: Fixed,
    #[serde(default)]
    pub low_dollars: Fixed,
    #[serde(default)]
    pub close_dollars: Fixed,
    #[serde(default)]
    pub mean_dollars: Fixed,
    #[serde(default)]
    pub previous_dollars: Fixed,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct RawCandleBook {
    #[serde(default)]
    pub open_dollars: Fixed,
    #[serde(default)]
    pub close_dollars: Fixed,
    #[serde(default)]
    pub high_dollars: Fixed,
    #[serde(default)]
    pub low_dollars: Fixed,
}

/* --------------------------------------------------------- ticker helpers */

/// Derive the series ticker from a market or event ticker.
///
/// Kalshi tickers are `SERIES-EVENTSUFFIX[-STRIKE]`, and the series segment is
/// the part before the first hyphen — except for the handful of series whose own
/// ticker contains a hyphen (`KXMVECROSSCATEGORY-SHARD1`). The candlesticks
/// endpoint is the only caller that needs this, and it 404s on a wrong guess, so
/// [`get_candles`] verifies against the market record rather than trusting this.
pub fn series_from_ticker(ticker: &str) -> &str {
    match ticker.find('-') {
        Some(first) => &ticker[..first],
        None => ticker,
    }
}

/* ------------------------------------------------------------ normalisers */

fn normalise_market(raw: &RawMarket, series_ticker: Option<&str>) -> Market {
    let yes_bid = raw.yes_bid_dollars.price();
    let yes_ask = raw.yes_ask_dollars.price();
    let last = raw.last_price_dollars.price();
    let previous = raw.previous_price_dollars.price();
    let event_ticker = raw.event_ticker.clone().unwrap_or_default();

    let mid = match (yes_bid, yes_ask) {
        (Some(bid), Some(ask)) => Some((bid + ask) / 2.0),
        _ => last.or(yes_bid).or(yes_ask),
    };

    Market {
        venue: Venue::Kalshi,
        ticker: raw.ticker.clone(),
        series_ticker: series_ticker.map_or_else(
            || {
                series_from_ticker(if event_ticker.is_empty() {
                    &raw.ticker
                } else {
                    &event_ticker
                })
                .to_string()
            },
            str::to_string,
        ),
        event_ticker,
        title: raw.title.clone().unwrap_or_else(|| raw.ticker.clone()),
        yes_sub_title: raw.yes_sub_title.clone().unwrap_or_default(),
        no_sub_title: raw.no_sub_title.clone().unwrap_or_default(),
        status: raw.status.clone().unwrap_or_else(|| "unknown".to_string()),
        market_type: raw
            .market_type
            .clone()
            .unwrap_or_else(|| "binary".to_string()),
        yes_bid,
        yes_ask,
        no_bid: raw.no_bid_dollars.price(),
        no_ask: raw.no_ask_dollars.price(),
        mid: mid.map(round4),
        last_price: last,
        previous_price: previous,
        change: match (last, previous) {
            (Some(last), Some(previous)) => Some(round4(last - previous)),
            _ => None,
        },
        volume: raw.volume_fp.num(),
        volume24h: raw.volume_24h_fp.num(),
        open_interest: raw.open_interest_fp.num(),
        liquidity: raw.liquidity_dollars.num(),
        open_time: raw.open_time.clone().unwrap_or_default(),
        close_time: raw.close_time.clone().unwrap_or_default(),
        expiration_time: raw.expiration_time.clone().unwrap_or_default(),
        result: raw.result.clone().unwrap_or_default(),
        rules_primary: raw.rules_primary.clone().unwrap_or_default(),
        category: raw.category.clone().filter(|c| !c.is_empty()),
        strike_type: strike_type(raw.strike_type.as_deref()),
        // Unlike prices, strikes arrive as JSON numbers already. They are also
        // the one field where 0 is a legitimate value (a rate or a spread can
        // settle at zero), so `num` is right here and `price` would not be.
        floor_strike: raw.floor_strike.num(),
        cap_strike: raw.cap_strike.num(),
    }
}

/// Strike types that name a numeric price level. Kalshi also uses `structured`
/// and `custom` for contracts whose "strike" is a rule rather than a number;
/// those carry no bound the implied-price maths can use.
fn strike_type(raw: Option<&str>) -> Option<StrikeType> {
    match raw.unwrap_or_default() {
        "greater" => Some(StrikeType::Greater),
        "greater_or_equal" => Some(StrikeType::GreaterOrEqual),
        "less" => Some(StrikeType::Less),
        "less_or_equal" => Some(StrikeType::LessOrEqual),
        "between" => Some(StrikeType::Between),
        _ => None,
    }
}

fn normalise_event(raw: &RawEvent) -> VenueEvent {
    let series_ticker = raw
        .series_ticker
        .clone()
        .unwrap_or_else(|| series_from_ticker(&raw.event_ticker).to_string());

    VenueEvent {
        venue: Venue::Kalshi,
        event_ticker: raw.event_ticker.clone(),
        title: raw
            .title
            .clone()
            .unwrap_or_else(|| raw.event_ticker.clone()),
        sub_title: raw.sub_title.clone().unwrap_or_default(),
        category: raw.category.clone().unwrap_or_default(),
        mutually_exclusive: raw.mutually_exclusive.unwrap_or(false),
        markets: raw
            .markets
            .as_deref()
            .unwrap_or_default()
            .iter()
            .map(|m| normalise_market(m, Some(&series_ticker)))
            .collect(),
        series_ticker,
    }
}

/* ---------------------------------------------------------------- queries */

/// Render a query string, dropping absent and empty values.
///
/// The order is the order given, because it is also the order of the cache key.
fn qs(params: &[(&str, Option<String>)]) -> String {
    let mut parts: Vec<String> = Vec::new();
    for (key, value) in params {
        let Some(value) = value else { continue };
        if value.is_empty() {
            continue;
        }
        parts.push(format!("{key}={}", urlencoding::encode(value)));
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!("?{}", parts.join("&"))
    }
}

async fn get<T>(state: &AppState, path: &str, ttl: Duration) -> Result<Arc<T>>
where
    T: DeserializeOwned + Send + Sync + 'static,
{
    let url = format!("{}{path}", state.config().kalshi_api_base);
    state
        .cache()
        .cached(&format!("kalshi:{path}"), ttl, || async {
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

#[derive(Debug, Clone, Default)]
pub struct ListMarketsParams {
    pub limit: Option<u32>,
    pub cursor: Option<String>,
    pub status: Option<String>,
    pub event_ticker: Option<String>,
    pub series_ticker: Option<String>,
    pub tickers: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct MarketsPayload {
    #[serde(default)]
    markets: Option<Vec<RawMarket>>,
    #[serde(default)]
    cursor: Option<String>,
}

pub async fn list_markets(state: &AppState, params: ListMarketsParams) -> Result<MarketsResponse> {
    let path = format!(
        "/markets{}",
        qs(&[
            (
                "limit",
                Some(params.limit.unwrap_or(100).clamp(1, 1000).to_string())
            ),
            ("cursor", params.cursor),
            ("status", params.status),
            ("event_ticker", params.event_ticker),
            ("series_ticker", params.series_ticker),
            ("tickers", params.tickers),
        ])
    );
    let raw = get::<MarketsPayload>(state, &path, ttl::QUOTE).await?;
    Ok(MarketsResponse {
        markets: raw
            .markets
            .as_deref()
            .unwrap_or_default()
            .iter()
            .map(|m| normalise_market(m, None))
            .collect(),
        cursor: raw.cursor.clone().filter(|c| !c.is_empty()),
    })
}

#[derive(Debug, Default, Deserialize)]
struct MarketPayload {
    #[serde(default)]
    market: Option<RawMarket>,
}

pub async fn get_market(state: &AppState, ticker: &str) -> Result<Market> {
    let raw = get::<MarketPayload>(
        state,
        &format!("/markets/{}", urlencoding::encode(ticker)),
        ttl::QUOTE,
    )
    .await?;

    match raw.market.as_ref() {
        Some(market) => Ok(normalise_market(market, None)),
        None => Err(UpstreamError::not_found(format!(
            "No Kalshi market with ticker {ticker}"
        ))),
    }
}

#[derive(Debug, Clone, Default)]
pub struct ListEventsParams {
    pub limit: Option<u32>,
    pub cursor: Option<String>,
    pub status: Option<String>,
    pub series_ticker: Option<String>,
    pub with_nested_markets: bool,
}

#[derive(Debug, Default, Deserialize)]
struct EventsPayload {
    #[serde(default)]
    events: Option<Vec<RawEvent>>,
    #[serde(default)]
    cursor: Option<String>,
}

pub async fn list_events(state: &AppState, params: ListEventsParams) -> Result<EventsResponse> {
    let path = format!(
        "/events{}",
        qs(&[
            (
                "limit",
                Some(params.limit.unwrap_or(100).clamp(1, 200).to_string())
            ),
            ("cursor", params.cursor),
            ("status", params.status),
            ("series_ticker", params.series_ticker),
            (
                "with_nested_markets",
                params.with_nested_markets.then(|| "true".to_string())
            ),
        ])
    );
    let raw = get::<EventsPayload>(state, &path, ttl::META).await?;
    Ok(EventsResponse {
        events: raw
            .events
            .as_deref()
            .unwrap_or_default()
            .iter()
            .map(normalise_event)
            .collect(),
        cursor: raw.cursor.clone().filter(|c| !c.is_empty()),
    })
}

#[derive(Debug, Default, Deserialize)]
struct EventPayload {
    #[serde(default)]
    event: Option<RawEvent>,
    #[serde(default)]
    markets: Option<Vec<RawMarket>>,
}

pub async fn get_event(state: &AppState, event_ticker: &str) -> Result<VenueEvent> {
    let raw = get::<EventPayload>(
        state,
        &format!(
            "/events/{}{}",
            urlencoding::encode(event_ticker),
            qs(&[("with_nested_markets", Some("true".to_string()))])
        ),
        ttl::QUOTE,
    )
    .await?;

    let Some(raw_event) = raw.event.as_ref() else {
        return Err(UpstreamError::not_found(format!(
            "No Kalshi event with ticker {event_ticker}"
        )));
    };

    // Some responses nest markets under the event, others return them alongside.
    let mut event = normalise_event(raw_event);
    if event.markets.is_empty() {
        if let Some(markets) = raw.markets.as_deref().filter(|m| !m.is_empty()) {
            event.markets = markets
                .iter()
                .map(|m| normalise_market(m, Some(&event.series_ticker)))
                .collect();
        }
    }
    Ok(event)
}

/// The two bid ladders, as `/orderbook` sends them.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct RawOrderBook {
    #[serde(default)]
    pub orderbook_fp: Option<RawBook>,
}

/// Rows are `[price, size]` pairs of fixed-point strings. Read positionally and
/// permissively: a row that is short or malformed reads as a zero and is
/// dropped by the filter below, rather than failing the whole book.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct RawBook {
    #[serde(default)]
    pub yes_dollars: Option<Vec<Vec<Fixed>>>,
    #[serde(default)]
    pub no_dollars: Option<Vec<Vec<Fixed>>>,
}

/// Turn Kalshi's two bid ladders into a conventional book.
///
/// Kalshi quotes both sides as *bids*: a YES ladder and a NO ladder. There is no
/// ask ladder, because an offer to sell YES at `p` is identical to a bid to buy
/// NO at `1 - p`. Traders expect a bid/ask, so the NO side is inverted into YES
/// ask terms here — once, on the server — rather than in every panel that
/// renders a book.
pub fn normalise_order_book(raw: &RawOrderBook, ticker: &str, depth: usize) -> OrderBook {
    let book = raw.orderbook_fp.clone().unwrap_or_default();

    let to_levels = |rows: Option<&Vec<Vec<Fixed>>>| -> Vec<BookLevel> {
        rows.map(Vec::as_slice)
            .unwrap_or_default()
            .iter()
            .map(|row| BookLevel {
                price: row.first().copied().unwrap_or_default().num0(),
                size: row.get(1).copied().unwrap_or_default().num0(),
            })
            .filter(|l| l.price > 0.0 && l.size > 0.0)
            .collect()
    };

    // Kalshi returns both sides ascending by price. Best bid is the highest.
    let mut yes = to_levels(book.yes_dollars.as_ref());
    yes.sort_by(|a, b| b.price.total_cmp(&a.price));
    yes.truncate(depth);

    let mut no = to_levels(book.no_dollars.as_ref());
    no.sort_by(|a, b| b.price.total_cmp(&a.price));
    no.truncate(depth);

    // A resting NO bid at q is an offer to sell YES at 1 - q.
    let mut yes_asks: Vec<BookLevel> = no
        .iter()
        .map(|l| BookLevel {
            price: round4(1.0 - l.price),
            size: l.size,
        })
        .collect();
    yes_asks.sort_by(|a, b| a.price.total_cmp(&b.price));

    let best_yes_bid = yes.first().map(|l| l.price);
    let best_yes_ask = yes_asks.first().map(|l| l.price);

    OrderBook {
        venue: Venue::Kalshi,
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

pub async fn get_order_book(state: &AppState, ticker: &str, depth: usize) -> Result<OrderBook> {
    let raw = get::<RawOrderBook>(
        state,
        &format!(
            "/markets/{}/orderbook{}",
            urlencoding::encode(ticker),
            qs(&[("depth", Some(depth.to_string()))])
        ),
        ttl::QUOTE,
    )
    .await?;
    Ok(normalise_order_book(&raw, ticker, depth))
}

#[derive(Debug, Clone, Default, Deserialize)]
struct RawTrade {
    #[serde(default)]
    trade_id: String,
    #[serde(default)]
    ticker: String,
    #[serde(default)]
    created_time: String,
    #[serde(default)]
    count_fp: Fixed,
    #[serde(default)]
    yes_price_dollars: Fixed,
    #[serde(default)]
    no_price_dollars: Fixed,
    #[serde(default)]
    taker_side: Option<String>,
    #[serde(default)]
    is_block_trade: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
struct TradesPayload {
    #[serde(default)]
    trades: Option<Vec<RawTrade>>,
    #[serde(default)]
    cursor: Option<String>,
}

/// Unix seconds for an ISO timestamp, or 0 when it is unreadable — the tape is
/// worth showing even if one print's clock is malformed.
fn unix_seconds(created_time: &str) -> i64 {
    OffsetDateTime::parse(created_time, &Rfc3339).map_or(0, OffsetDateTime::unix_timestamp)
}

pub async fn get_trades(
    state: &AppState,
    ticker: &str,
    limit: usize,
    cursor: Option<&str>,
) -> Result<TradesResponse> {
    let raw = get::<TradesPayload>(
        state,
        &format!(
            "/markets/trades{}",
            qs(&[
                ("ticker", Some(ticker.to_string())),
                ("limit", Some(limit.clamp(1, 1000).to_string())),
                ("cursor", cursor.map(str::to_string)),
            ])
        ),
        ttl::QUOTE,
    )
    .await?;

    Ok(TradesResponse {
        trades: raw
            .trades
            .as_deref()
            .unwrap_or_default()
            .iter()
            .map(|t| {
                let yes_price = t.yes_price_dollars.num();
                let no_price = t.no_price_dollars.num();
                // Only one side is always present; the pair sums to 1.
                let yes = yes_price.unwrap_or_else(|| no_price.map_or(0.0, |no| round4(1.0 - no)));
                let no = no_price.unwrap_or_else(|| round4(1.0 - yes));
                Trade {
                    venue: Venue::Kalshi,
                    trade_id: t.trade_id.clone(),
                    ticker: t.ticker.clone(),
                    ts: unix_seconds(&t.created_time),
                    count: t.count_fp.num0(),
                    yes_price: yes,
                    no_price: no,
                    taker_side: t.taker_side.clone().unwrap_or_else(|| "yes".to_string()),
                    is_block_trade: t.is_block_trade.unwrap_or(false),
                }
            })
            .collect(),
        cursor: raw.cursor.clone().filter(|c| !c.is_empty()),
    })
}

#[derive(Debug, Clone, Default, Deserialize)]
struct RawSeries {
    #[serde(default)]
    ticker: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    frequency: Option<String>,
    #[serde(default)]
    tags: Option<Vec<String>>,
}

#[derive(Debug, Default, Deserialize)]
struct SeriesPayload {
    #[serde(default)]
    series: Option<Vec<RawSeries>>,
}

pub async fn list_series(state: &AppState, category: Option<&str>) -> Result<Vec<SeriesInfo>> {
    let raw = get::<SeriesPayload>(
        state,
        &format!(
            "/series/{}",
            qs(&[("category", category.map(str::to_string))])
        ),
        ttl::CATALOGUE,
    )
    .await?;

    Ok(raw
        .series
        .as_deref()
        .unwrap_or_default()
        .iter()
        .map(|s| SeriesInfo {
            venue: Venue::Kalshi,
            ticker: s.ticker.clone(),
            title: s.title.clone().unwrap_or_else(|| s.ticker.clone()),
            category: s.category.clone().unwrap_or_default(),
            frequency: s.frequency.clone().unwrap_or_default(),
            tags: s.tags.clone().unwrap_or_default(),
        })
        .collect())
}

/* ----------------------------------------------------------------- search */

/// The searchable universe, built from *events* rather than markets.
///
/// This is not a micro-optimisation — it is the only workable route. Kalshi's
/// open-market list is ~99.98% auto-generated multivariate parlay legs
/// (`KXMVE…`, titles like `"yes Boston,yes Chicago WS,yes Miami,…"`): paging
/// `/markets` returned 11,998 of them in the first 12,000 rows, two of which
/// were real. `/events?with_nested_markets=true` excludes them entirely and
/// yields ~2,000 markets per page of real, human-authored contracts.
///
/// Held for [`ttl::CATALOGUE`] and warmed at boot, so the first search a user
/// runs does not pay for the crawl. Prices inside the snapshot go stale, so
/// anything that renders a live quote re-fetches the individual market.
///
/// Searching and ranking it is [`search_corpus`]/[`rank_markets`]' job, so all
/// three venues score a query the same way.
const CORPUS_KEY: &str = "kalshi:corpus";

/// Pages of 200 to crawl.
///
/// Sized to cover the whole open universe rather than to bound the crawl: at 20
/// pages the snapshot stopped at 4,000 of ~9,800 open events, and because the
/// upstream does not order by category the truncation fell unevenly — it hid
/// 265 of the 545 open Entertainment events, so half of them were unreachable
/// from `SRCH` and `ENT`. 60 pages leaves headroom above the current universe;
/// the loop still stops as soon as the cursor runs out.
const CORPUS_MAX_PAGES: usize = 60;

/// Crawl the open-event catalogue. The uncached form of [`corpus_snapshot`].
pub async fn build_corpus(state: &AppState) -> Result<Corpus> {
    let mut events: Vec<VenueEvent> = Vec::new();
    let mut markets: Vec<Market> = Vec::new();
    let mut cursor: Option<String> = None;
    let mut truncated = false;

    for page in 0..CORPUS_MAX_PAGES {
        let path = format!(
            "/events{}",
            qs(&[
                ("limit", Some("200".to_string())),
                ("status", Some("open".to_string())),
                ("with_nested_markets", Some("true".to_string())),
                ("cursor", cursor.clone()),
            ])
        );

        let raw: EventsPayload = state
            .http()
            .fetch_json(
                &format!("{}{path}", state.config().kalshi_api_base),
                FetchOptions::new()
                    .timeout(Duration::from_secs(30))
                    .retries(2),
            )
            .await?;

        let page_events = raw.events.as_deref().unwrap_or_default();
        for raw_event in page_events {
            let event = normalise_event(raw_event);
            markets.extend(event.markets.iter().cloned());
            events.push(event);
        }

        cursor = raw.cursor.clone().filter(|c| !c.is_empty());
        if cursor.is_none() || page_events.is_empty() {
            break;
        }
        truncated = page == CORPUS_MAX_PAGES - 1;
    }

    Ok(Corpus::new(Venue::Kalshi, events, markets, truncated))
}

async fn corpus(state: &AppState) -> Result<Arc<Corpus>> {
    state
        .cache()
        .cached(CORPUS_KEY, ttl::CATALOGUE, || build_corpus(state))
        .await
}

/// The open-event snapshot, for callers that slice it differently to [`search`].
///
/// Exported so the entertainment browser can filter by category without paying
/// for its own crawl — it reads the same cached snapshot `SRCH` and `TOP` use.
pub async fn corpus_snapshot(state: &AppState) -> Result<Arc<Corpus>> {
    corpus(state).await
}

/// Kick off the crawl without blocking startup.
///
/// A cold corpus takes ~15s to build. Doing it at boot means the operator waits,
/// not the first user to type `SRCH`.
pub fn warm_corpus(state: &AppState) {
    let state = state.clone();
    tokio::spawn(async move {
        match corpus(&state).await {
            Ok(snapshot) => tracing::info!(
                events = snapshot.events.len(),
                markets = snapshot.markets.len(),
                "[kalshi] corpus warm"
            ),
            Err(err) => tracing::warn!(error = %err, "[kalshi] corpus warm failed"),
        }
    });
}

/// Rank open Kalshi events against a free-text query.
pub async fn search(state: &AppState, query: &str, limit: usize) -> Result<SearchResponse> {
    let snapshot = corpus(state).await?;
    refuse_overlong_query(query)?;
    Ok(search_corpus(&snapshot, query, limit))
}

/// Leaderboard over the snapshot. Powers the `TOP` command.
pub async fn top_markets(state: &AppState, sort: MoverSort, limit: usize) -> Result<Vec<Market>> {
    let snapshot = corpus(state).await?;
    Ok(rank_markets(&snapshot.markets, sort, limit))
}

/* ---------------------------------------------------------------- candles */

/// Normalise a candlestick page.
///
/// Periods with no trades carry only `previous_dollars`. Those are emitted as
/// flat candles at the previous close with `traded: false`, so a chart stays
/// continuous instead of gapping, and the client can style them differently.
pub fn normalise_candles(raw_candles: &[RawCandle]) -> Vec<Candle> {
    let mut candles: Vec<Candle> = Vec::new();
    let mut previous_close: Option<f64> = None;

    for c in raw_candles {
        let p = c.price.clone().unwrap_or_default();
        let bid = c
            .yes_bid
            .as_ref()
            .map_or(Fixed::default(), |b| b.close_dollars)
            .price();
        let ask = c
            .yes_ask
            .as_ref()
            .map_or(Fixed::default(), |a| a.close_dollars)
            .price();

        let open = p.open_dollars.num();
        let close = p.close_dollars.num();
        let traded = open.is_some() && close.is_some();

        let (o, h, l, cl);
        if let (Some(open), Some(close)) = (open, close) {
            o = open;
            cl = close;
            h = p.high_dollars.num().unwrap_or_else(|| o.max(cl));
            l = p.low_dollars.num().unwrap_or_else(|| o.min(cl));
        } else {
            // No prints this period. Hold the last close; failing that, use the
            // book mid so the series still starts somewhere sensible.
            let carry = p
                .previous_dollars
                .num()
                .or(previous_close)
                .or_else(|| match (bid, ask) {
                    (Some(bid), Some(ask)) => Some(round4((bid + ask) / 2.0)),
                    _ => bid.or(ask),
                });
            let Some(carry) = carry else { continue };
            o = carry;
            h = carry;
            l = carry;
            cl = carry;
        }

        previous_close = Some(cl);
        candles.push(Candle {
            time: c.end_period_ts,
            open: o,
            high: h,
            low: l,
            close: cl,
            volume: Some(c.volume_fp.num0()),
            open_interest: Some(c.open_interest_fp.num0()),
            traded,
            bid,
            ask,
        });
    }

    candles.sort_by_key(|c| c.time);
    candles
}

#[derive(Debug, Default, Deserialize)]
struct CandlesPayload {
    #[serde(default)]
    candlesticks: Option<Vec<RawCandle>>,
}

/// Candlesticks for a market.
///
/// The upstream path embeds the *series* ticker, which a market ticker only
/// sometimes yields by prefix. We look the market up first (cached, and normally
/// already warm because the chart panel just quoted it) and use its recorded
/// series, falling back to the prefix guess if the lookup fails.
///
/// `known_series_ticker` skips the market lookup when the series is already
/// known. The implied-price fan-out reads one ladder's worth of candles at a
/// time and knows the series from the event, so passing it halves that route's
/// upstream request count.
pub async fn get_candles(
    state: &AppState,
    ticker: &str,
    interval: CandleInterval,
    start_ts: i64,
    end_ts: i64,
    known_series_ticker: Option<&str>,
) -> Result<CandlesResponse> {
    let mut series_ticker = known_series_ticker
        .unwrap_or_else(|| series_from_ticker(ticker))
        .to_string();

    if known_series_ticker.is_none() {
        // Market lookup is an optimisation; the prefix guess is right most of
        // the time.
        if let Ok(market) = get_market(state, ticker).await {
            if !market.series_ticker.is_empty() {
                series_ticker = market.series_ticker;
            }
        }
    }

    let path = format!(
        "/series/{}/markets/{}/candlesticks{}",
        urlencoding::encode(&series_ticker),
        urlencoding::encode(ticker),
        qs(&[
            ("start_ts", Some(start_ts.to_string())),
            ("end_ts", Some(end_ts.to_string())),
            ("period_interval", Some(interval.minutes().to_string())),
        ])
    );

    let raw = get::<CandlesPayload>(state, &path, ttl::CANDLES).await?;

    Ok(CandlesResponse {
        venue: Venue::Kalshi,
        ticker: ticker.to_string(),
        series_ticker,
        interval,
        candles: normalise_candles(raw.candlesticks.as_deref().unwrap_or_default()),
        note: None,
    })
}

#[cfg(test)]
mod tests {
    //! Kalshi normalisation tests.
    //!
    //! The fixtures are trimmed copies of real `api.elections.kalshi.com`
    //! responses, so the fixed-point string formats (`"0.0900"`, `"39942.93"`)
    //! and the shape of a no-trade candlestick are exactly what the upstream
    //! sends.

    use super::*;
    use crate::config::Config;
    use wiremock::matchers::{method, path as path_matcher, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn book_of(json: &str) -> RawOrderBook {
        serde_json::from_str(json).expect("fixture parses")
    }

    fn candles_of(json: &str) -> Vec<RawCandle> {
        serde_json::from_str(json).expect("fixture parses")
    }

    /// Real shape: two *bid* ladders, ascending by price, as decimal strings.
    const RAW_BOOK: &str = r#"{
      "orderbook_fp": {
        "yes_dollars": [
          ["0.0600", "2392.98"],
          ["0.0700", "4195.84"],
          ["0.0900", "925.60"]
        ],
        "no_dollars": [
          ["0.7800", "264.32"],
          ["0.8800", "338.81"],
          ["0.9000", "10.00"]
        ]
      }
    }"#;

    const TRADED_CANDLE: &str = r#"{
      "end_period_ts": 1786852800,
      "open_interest_fp": "39997.12",
      "volume_fp": "90.41",
      "price": {
        "open_dollars": "0.1000",
        "high_dollars": "0.1100",
        "low_dollars": "0.1000",
        "close_dollars": "0.1000",
        "mean_dollars": "0.1050",
        "previous_dollars": "0.1100"
      },
      "yes_bid": { "close_dollars": "0.1000" },
      "yes_ask": { "close_dollars": "0.1100" }
    }"#;

    /// What Kalshi returns for a period in which nothing printed: `price`
    /// carries only `previous_dollars`, with no OHLC at all.
    const QUIET_CANDLE: &str = r#"{
      "end_period_ts": 1786866000,
      "open_interest_fp": "39997.12",
      "volume_fp": "0.00",
      "price": { "previous_dollars": "0.1000" },
      "yes_bid": { "close_dollars": "0.0900" },
      "yes_ask": { "close_dollars": "0.1100" }
    }"#;

    /* ------------------------------------------------- series_from_ticker */

    #[test]
    fn takes_the_segment_before_the_first_hyphen() {
        assert_eq!(
            series_from_ticker("KXFEDDECISION-27JAN-H26"),
            "KXFEDDECISION"
        );
        assert_eq!(series_from_ticker("KXHIGHNY-26AUG16-B82.5"), "KXHIGHNY");
    }

    #[test]
    fn returns_the_whole_ticker_when_there_is_no_hyphen() {
        assert_eq!(series_from_ticker("KXELONMARS"), "KXELONMARS");
    }

    /* ------------------------------------------------ normalise_order_book */

    #[test]
    fn sorts_each_bid_ladder_best_first() {
        let book = normalise_order_book(&book_of(RAW_BOOK), "X", 12);
        assert_eq!(
            book.yes.iter().map(|l| l.price).collect::<Vec<_>>(),
            [0.09, 0.07, 0.06]
        );
        assert_eq!(
            book.no.iter().map(|l| l.price).collect::<Vec<_>>(),
            [0.9, 0.88, 0.78]
        );
    }

    #[test]
    fn inverts_the_no_ladder_into_yes_asks_cheapest_first() {
        let book = normalise_order_book(&book_of(RAW_BOOK), "X", 12);
        // A NO bid at 0.90 is an offer to sell YES at 0.10.
        assert_eq!(
            book.yes_asks.iter().map(|l| l.price).collect::<Vec<_>>(),
            [0.1, 0.12, 0.22]
        );
        assert_eq!(book.yes_asks[0].size, 10.0);
    }

    #[test]
    fn produces_no_floating_point_dust_from_the_one_minus_q_inversion() {
        let book = normalise_order_book(
            &book_of(r#"{ "orderbook_fp": { "no_dollars": [["0.7000", "5"]] } }"#),
            "X",
            12,
        );
        // 1 - 0.7 is 0.30000000000000004 in binary floating point.
        assert_eq!(book.yes_asks[0].price, 0.3);
    }

    #[test]
    fn derives_best_bid_best_ask_spread_and_mid() {
        let book = normalise_order_book(&book_of(RAW_BOOK), "X", 12);
        assert_eq!(book.best_yes_bid, Some(0.09));
        assert_eq!(book.best_yes_ask, Some(0.1));
        assert_eq!(book.spread, Some(0.01));
        assert_eq!(book.mid, Some(0.095));
    }

    #[test]
    fn reports_nulls_rather_than_a_fake_quote_for_an_empty_book() {
        let book = normalise_order_book(&book_of("{}"), "X", 12);
        assert!(book.yes.is_empty());
        assert_eq!(book.best_yes_bid, None);
        assert_eq!(book.best_yes_ask, None);
        assert_eq!(book.spread, None);
        assert_eq!(book.mid, None);
    }

    #[test]
    fn leaves_spread_null_when_only_one_side_rests() {
        let book = normalise_order_book(
            &book_of(r#"{ "orderbook_fp": { "yes_dollars": [["0.4000", "10"]] } }"#),
            "X",
            12,
        );
        assert_eq!(book.best_yes_bid, Some(0.4));
        assert_eq!(book.best_yes_ask, None);
        assert_eq!(book.spread, None);
    }

    #[test]
    fn drops_zero_price_and_zero_size_rows() {
        let book = normalise_order_book(
            &book_of(
                r#"{
                  "orderbook_fp": {
                    "yes_dollars": [
                      ["0.0000", "100"],
                      ["0.5000", "0.00"],
                      ["0.4000", "10"]
                    ]
                  }
                }"#,
            ),
            "X",
            12,
        );
        assert_eq!(book.yes.len(), 1);
        assert_eq!(book.yes[0].price, 0.4);
    }

    #[test]
    fn honours_the_depth_cap() {
        let rows: Vec<String> = (0..40)
            .map(|i| format!(r#"["0.{}00", "5"]"#, i + 10))
            .collect();
        let book = normalise_order_book(
            &book_of(&format!(
                r#"{{ "orderbook_fp": {{ "yes_dollars": [{}] }} }}"#,
                rows.join(",")
            )),
            "X",
            5,
        );
        assert_eq!(book.yes.len(), 5);
    }

    /* -------------------------------------------------- normalise_candles */

    #[test]
    fn maps_a_traded_period_to_real_ohlc() {
        let candles = normalise_candles(&candles_of(&format!("[{TRADED_CANDLE}]")));
        let c = &candles[0];
        assert_eq!(c.open, 0.1);
        assert_eq!(c.high, 0.11);
        assert_eq!(c.low, 0.1);
        assert_eq!(c.close, 0.1);
        assert_eq!(c.volume, Some(90.41));
        assert_eq!(c.open_interest, Some(39997.12));
        assert!(c.traded);
    }

    #[test]
    fn synthesises_a_flat_candle_for_a_period_with_no_prints() {
        let candles = normalise_candles(&candles_of(&format!("[{QUIET_CANDLE}]")));
        let c = &candles[0];
        // Charts must stay continuous — a quiet hour is a flat bar, not a gap.
        assert_eq!(c.open, 0.1);
        assert_eq!(c.high, 0.1);
        assert_eq!(c.low, 0.1);
        assert_eq!(c.close, 0.1);
        assert!(!c.traded);
        assert_eq!(c.volume, Some(0.0));
    }

    #[test]
    fn carries_the_previous_close_forward_when_previous_dollars_is_absent() {
        let orphan = r#"{ "end_period_ts": 1786870000, "price": {}, "volume_fp": "0" }"#;
        let candles = normalise_candles(&candles_of(&format!("[{TRADED_CANDLE},{orphan}]")));
        assert_eq!(candles.len(), 2);
        assert_eq!(candles[1].close, candles[0].close);
        assert!(!candles[1].traded);
    }

    #[test]
    fn falls_back_to_the_book_mid_when_there_is_no_price_history_at_all() {
        let first = r#"[{
          "end_period_ts": 1786870000,
          "price": {},
          "yes_bid": { "close_dollars": "0.2000" },
          "yes_ask": { "close_dollars": "0.3000" }
        }]"#;
        let candles = normalise_candles(&candles_of(first));
        assert_eq!(candles[0].close, 0.25);
    }

    #[test]
    fn drops_a_leading_candle_with_no_price_information_of_any_kind() {
        let candles = normalise_candles(&candles_of(r#"[{ "end_period_ts": 1, "price": {} }]"#));
        assert!(candles.is_empty());
    }

    #[test]
    fn sorts_output_ascending_by_time_as_lightweight_charts_requires() {
        let candles = normalise_candles(&candles_of(&format!("[{QUIET_CANDLE},{TRADED_CANDLE}]")));
        assert!(candles[0].time < candles[1].time);
    }

    #[test]
    fn exposes_the_closing_bid_and_ask_with_an_empty_side_as_null() {
        let raw = r#"[{
          "end_period_ts": 1786852800,
          "open_interest_fp": "39997.12",
          "volume_fp": "90.41",
          "price": {
            "open_dollars": "0.1000",
            "high_dollars": "0.1100",
            "low_dollars": "0.1000",
            "close_dollars": "0.1000",
            "previous_dollars": "0.1100"
          },
          "yes_bid": { "close_dollars": "0.0000" },
          "yes_ask": { "close_dollars": "0.1100" }
        }]"#;
        let candles = normalise_candles(&candles_of(raw));
        assert_eq!(candles[0].bid, None);
        assert_eq!(candles[0].ask, Some(0.11));
    }

    #[test]
    fn returns_an_empty_array_for_an_empty_response() {
        assert!(normalise_candles(&[]).is_empty());
    }

    /* ------------------------------------------------------------ upstream */

    /// A state whose Kalshi base points at the fixture server.
    fn state_for(server: &MockServer) -> AppState {
        AppState::new(Config {
            kalshi_api_base: server.uri(),
            ..Config::default()
        })
    }

    const MARKET_JSON: &str = r#"{
      "ticker": "KXFEDDECISION-27JAN-H26",
      "event_ticker": "KXFEDDECISION-27JAN",
      "title": "Fed decision in January",
      "yes_sub_title": "3.75% or above",
      "status": "active",
      "market_type": "binary",
      "yes_bid_dollars": "0.6900",
      "yes_ask_dollars": "0.7100",
      "no_bid_dollars": "0.2900",
      "no_ask_dollars": "0.0000",
      "last_price_dollars": "0.7000",
      "previous_price_dollars": "0.6600",
      "volume_fp": "12645.98",
      "volume_24h_fp": "820.00",
      "open_interest_fp": "39942.93",
      "liquidity_dollars": "1200.50",
      "open_time": "2026-01-02T15:00:00Z",
      "close_time": "2027-01-27T20:00:00Z",
      "strike_type": "greater_or_equal",
      "floor_strike": 3.75
    }"#;

    #[tokio::test]
    async fn a_market_reads_every_fixed_point_field_as_a_number() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/markets/KXFEDDECISION-27JAN-H26"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(format!(r#"{{"market": {MARKET_JSON} }}"#)),
            )
            .mount(&server)
            .await;

        let state = state_for(&server);
        let market = get_market(&state, "KXFEDDECISION-27JAN-H26").await.unwrap();

        assert_eq!(market.venue, Venue::Kalshi);
        assert_eq!(market.series_ticker, "KXFEDDECISION");
        assert_eq!(market.yes_bid, Some(0.69));
        assert_eq!(market.yes_ask, Some(0.71));
        assert_eq!(market.mid, Some(0.7));
        assert_eq!(market.change, Some(0.04));
        assert_eq!(market.volume, Some(12645.98));
        assert_eq!(market.open_interest, Some(39942.93));
        assert_eq!(market.strike_type, Some(StrikeType::GreaterOrEqual));
        assert_eq!(market.floor_strike, Some(3.75));
        // An empty book side reports "0.0000"; it must not read as a 0¢ quote.
        assert_eq!(market.no_ask, None);
        // Kalshi publishes no cap here, and `null` must not become 0.
        assert_eq!(market.cap_strike, None);
    }

    #[tokio::test]
    async fn a_response_with_no_market_is_a_not_found() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/markets/NOPE"))
            .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
            .mount(&server)
            .await;

        let err = get_market(&state_for(&server), "NOPE").await.unwrap_err();
        assert_eq!(err.code, crate::error::codes::NOT_FOUND);
        assert!(err.message.contains("NOPE"));
    }

    #[tokio::test]
    async fn listing_markets_clamps_the_limit_and_caches_under_its_path() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/markets"))
            .and(query_param("limit", "1000"))
            .and(query_param("status", "open"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(format!(r#"{{"markets": [{MARKET_JSON}], "cursor": ""}}"#)),
            )
            .mount(&server)
            .await;

        let state = state_for(&server);
        let markets = list_markets(
            &state,
            ListMarketsParams {
                limit: Some(5000),
                status: Some("open".to_string()),
                ..ListMarketsParams::default()
            },
        )
        .await
        .unwrap();

        assert_eq!(markets.markets.len(), 1);
        // An empty cursor means "no more pages", not a cursor of "".
        assert_eq!(markets.cursor, None);
        assert!(state
            .cache()
            .get::<MarketsPayload>("kalshi:/markets?limit=1000&status=open")
            .await
            .is_some());
    }

    #[tokio::test]
    async fn the_book_is_asked_for_at_the_requested_depth() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/markets/X/orderbook"))
            .and(query_param("depth", "3"))
            .respond_with(ResponseTemplate::new(200).set_body_string(RAW_BOOK))
            .mount(&server)
            .await;

        let book = get_order_book(&state_for(&server), "X", 3).await.unwrap();
        assert_eq!(book.ticker, "X");
        assert_eq!(book.best_yes_bid, Some(0.09));
        assert_eq!(book.best_yes_ask, Some(0.1));
    }

    #[tokio::test]
    async fn trades_come_from_the_collection_path_not_the_market_one() {
        let server = MockServer::start().await;
        // Kalshi serves prints from `/markets/trades?ticker=…`, not from
        // `/markets/{ticker}/trades`.
        Mock::given(method("GET"))
            .and(path_matcher("/markets/trades"))
            .and(query_param("ticker", "KXHIGHNY-26AUG16-B82.5"))
            .and(query_param("limit", "2"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"{
                  "trades": [
                    {
                      "trade_id": "a1",
                      "ticker": "KXHIGHNY-26AUG16-B82.5",
                      "created_time": "2026-08-16T14:22:31.123456Z",
                      "count_fp": "25.00",
                      "yes_price_dollars": "0.6900",
                      "taker_side": "no",
                      "is_block_trade": true
                    },
                    {
                      "trade_id": "a2",
                      "ticker": "KXHIGHNY-26AUG16-B82.5",
                      "created_time": "2026-08-16T14:20:00Z",
                      "count_fp": "3",
                      "no_price_dollars": "0.3100"
                    }
                  ],
                  "cursor": "next"
                }"#,
            ))
            .mount(&server)
            .await;

        let trades = get_trades(&state_for(&server), "KXHIGHNY-26AUG16-B82.5", 2, None)
            .await
            .unwrap();

        assert_eq!(trades.cursor.as_deref(), Some("next"));
        // 2026-08-16T14:22:31.123456Z, floored to the second.
        assert_eq!(trades.trades[0].ts, 1_786_890_151);
        assert_eq!(trades.trades[0].count, 25.0);
        assert_eq!(trades.trades[0].taker_side, "no");
        assert!(trades.trades[0].is_block_trade);
        // Only one side is sent; the pair sums to 1, without float dust.
        assert_eq!(trades.trades[0].no_price, 0.31);
        assert_eq!(trades.trades[1].yes_price, 0.69);
        assert_eq!(trades.trades[1].taker_side, "yes");
        assert!(!trades.trades[1].is_block_trade);
    }

    #[tokio::test]
    async fn an_event_takes_its_markets_from_alongside_when_they_are_not_nested() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/events/KXFEDDECISION-27JAN"))
            .and(query_param("with_nested_markets", "true"))
            .respond_with(ResponseTemplate::new(200).set_body_string(format!(
                r#"{{
                  "event": {{
                    "event_ticker": "KXFEDDECISION-27JAN",
                    "series_ticker": "KXFEDDECISION",
                    "title": "Fed decision in January",
                    "mutually_exclusive": true
                  }},
                  "markets": [{MARKET_JSON}]
                }}"#
            )))
            .mount(&server)
            .await;

        let event = get_event(&state_for(&server), "KXFEDDECISION-27JAN")
            .await
            .unwrap();
        assert!(event.mutually_exclusive);
        assert_eq!(event.markets.len(), 1);
        // The event's series is stamped onto markets that arrived without one.
        assert_eq!(event.markets[0].series_ticker, "KXFEDDECISION");
    }

    #[tokio::test]
    async fn a_response_with_no_event_is_a_not_found() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/events/NOPE"))
            .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
            .mount(&server)
            .await;

        let err = get_event(&state_for(&server), "NOPE").await.unwrap_err();
        assert_eq!(err.code, crate::error::codes::NOT_FOUND);
    }

    #[tokio::test]
    async fn nested_markets_are_only_requested_when_asked_for() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/events"))
            .and(query_param("limit", "200"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"{"events": [{"event_ticker": "KXHIGHNY-26AUG16", "title": "NY high"}]}"#,
            ))
            .mount(&server)
            .await;

        let state = state_for(&server);
        let events = list_events(
            &state,
            ListEventsParams {
                // Clamped to 200, the upstream's own ceiling.
                limit: Some(900),
                ..ListEventsParams::default()
            },
        )
        .await
        .unwrap();

        assert_eq!(events.events.len(), 1);
        assert_eq!(events.events[0].series_ticker, "KXHIGHNY");
        assert!(state
            .cache()
            .get::<EventsPayload>("kalshi:/events?limit=200")
            .await
            .is_some());
    }

    #[tokio::test]
    async fn the_series_list_keeps_its_trailing_slash() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/series/"))
            .and(query_param("category", "Economics"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"{"series": [{"ticker": "KXFEDDECISION", "category": "Economics", "tags": ["fed"]}]}"#,
            ))
            .mount(&server)
            .await;

        let series = list_series(&state_for(&server), Some("Economics"))
            .await
            .unwrap();
        assert_eq!(series.len(), 1);
        // A series with no title falls back to its ticker.
        assert_eq!(series[0].title, "KXFEDDECISION");
        assert_eq!(series[0].tags, ["fed"]);
        assert_eq!(series[0].frequency, "");
    }

    #[tokio::test]
    async fn candles_verify_the_series_against_the_market_record() {
        let server = MockServer::start().await;
        // The prefix guess for this ticker is `KXMVECROSSCATEGORY`, which 404s;
        // the market record names the real series.
        Mock::given(method("GET"))
            .and(path_matcher("/markets/KXMVECROSSCATEGORY-SHARD1-26AUG"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"{"market": {
                  "ticker": "KXMVECROSSCATEGORY-SHARD1-26AUG",
                  "event_ticker": "KXMVECROSSCATEGORY-SHARD1-26AUG"
                }}"#,
            ))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path_matcher(
                "/series/KXMVECROSSCATEGORY/markets/KXMVECROSSCATEGORY-SHARD1-26AUG/candlesticks",
            ))
            .and(query_param("period_interval", "60"))
            .and(query_param("start_ts", "100"))
            .and(query_param("end_ts", "200"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(format!(r#"{{"candlesticks": [{TRADED_CANDLE}]}}"#)),
            )
            .mount(&server)
            .await;

        let response = get_candles(
            &state_for(&server),
            "KXMVECROSSCATEGORY-SHARD1-26AUG",
            CandleInterval::OneHour,
            100,
            200,
            None,
        )
        .await
        .unwrap();

        assert_eq!(response.series_ticker, "KXMVECROSSCATEGORY");
        assert_eq!(response.candles.len(), 1);
        assert_eq!(response.interval, CandleInterval::OneHour);
        assert!(response.note.is_none());
    }

    #[tokio::test]
    async fn a_known_series_skips_the_market_lookup_entirely() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher(
                "/series/KXMVECROSSCATEGORY-SHARD1/markets/KXMVECROSSCATEGORY-SHARD1-26AUG/candlesticks",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"candlesticks": []}"#))
            .mount(&server)
            .await;

        let response = get_candles(
            &state_for(&server),
            "KXMVECROSSCATEGORY-SHARD1-26AUG",
            CandleInterval::OneMinute,
            0,
            60,
            Some("KXMVECROSSCATEGORY-SHARD1"),
        )
        .await
        .unwrap();

        // No `/markets/{ticker}` request was mounted, so the lookup was skipped.
        assert_eq!(response.series_ticker, "KXMVECROSSCATEGORY-SHARD1");
        assert!(response.candles.is_empty());
    }

    #[tokio::test]
    async fn a_failed_market_lookup_falls_back_to_the_prefix_guess() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/markets/KXHIGHNY-26AUG16-B82.5"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path_matcher(
                "/series/KXHIGHNY/markets/KXHIGHNY-26AUG16-B82.5/candlesticks",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"candlesticks": []}"#))
            .mount(&server)
            .await;

        let response = get_candles(
            &state_for(&server),
            "KXHIGHNY-26AUG16-B82.5",
            CandleInterval::OneDay,
            0,
            86_400,
            None,
        )
        .await
        .unwrap();
        assert_eq!(response.series_ticker, "KXHIGHNY");
    }

    #[tokio::test]
    async fn the_corpus_pages_until_the_cursor_runs_out() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/events"))
            .and(query_param("cursor", "page2"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"{"events": [{"event_ticker": "KXHIGHNY-26AUG17", "markets": [{"ticker": "KXHIGHNY-26AUG17-B80"}]}]}"#,
            ))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path_matcher("/events"))
            .and(query_param("status", "open"))
            .and(query_param("with_nested_markets", "true"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"{"events": [{"event_ticker": "KXHIGHNY-26AUG16", "markets": [{"ticker": "KXHIGHNY-26AUG16-B82.5"}]}], "cursor": "page2"}"#,
            ))
            .mount(&server)
            .await;

        let snapshot = corpus_snapshot(&state_for(&server)).await.unwrap();
        assert_eq!(snapshot.venue, Venue::Kalshi);
        assert_eq!(snapshot.events.len(), 2);
        assert_eq!(snapshot.markets.len(), 2);
        // The last page carried no cursor, so nothing was left behind.
        assert!(!snapshot.truncated);
        // Markets inherit the event's series, not their own prefix guess.
        assert_eq!(snapshot.markets[0].series_ticker, "KXHIGHNY");
    }

    #[tokio::test]
    async fn search_and_top_read_the_same_snapshot() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/events"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"{"events": [
                  {
                    "event_ticker": "KXFEDDECISION-27JAN",
                    "title": "Fed decision in January",
                    "markets": [{"ticker": "KXFEDDECISION-27JAN-H26", "volume_24h_fp": "10.00"}]
                  },
                  {
                    "event_ticker": "KXHIGHNY-26AUG16",
                    "title": "Highest temperature in NYC",
                    "markets": [{"ticker": "KXHIGHNY-26AUG16-B82.5", "volume_24h_fp": "900.00"}]
                  }
                ]}"#,
            ))
            // One crawl serves both commands.
            .expect(1)
            .mount(&server)
            .await;

        let state = state_for(&server);
        let found = search(&state, "fed", 25).await.unwrap();
        assert_eq!(found.query, "fed");
        assert_eq!(found.hits.len(), 1);
        assert_eq!(found.hits[0].event.event_ticker, "KXFEDDECISION-27JAN");
        assert_eq!(found.scanned, 2);

        let top = top_markets(&state, MoverSort::Volume, 25).await.unwrap();
        assert_eq!(top.len(), 2);
        assert_eq!(top[0].ticker, "KXHIGHNY-26AUG16-B82.5");
    }

    #[tokio::test]
    async fn the_snapshot_is_built_once_and_shared() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/events"))
            .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"events": []}"#))
            .expect(1)
            .mount(&server)
            .await;

        let state = state_for(&server);
        let first = corpus_snapshot(&state).await.unwrap();
        let second = corpus_snapshot(&state).await.unwrap();
        assert!(Arc::ptr_eq(&first, &second));
    }
}
