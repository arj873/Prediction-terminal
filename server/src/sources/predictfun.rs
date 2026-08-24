//! predict.fun client — the BNB-chain order-book exchange.
//!
//! The venue documents a REST API at `api.predict.fun` and gates every path of
//! it, read-only ones included, behind an account-issued `x-api-key`. Its own
//! website never calls it: the browser bundle talks to a single public GraphQL
//! endpoint instead, which answers the whole market-data surface — catalogue,
//! contract, book, tape, price history, taxonomy — with no credential at all.
//! That is the endpoint read here, so nothing this module needs is paywalled.
//!
//! Three things about that endpoint shape the code below, and all three were
//! re-confirmed against the live host while this port was written.
//!
//!   * It answers a failed query with **HTTP 200** and a top-level `errors`
//!     array, and a missing entity with `data.category == null` and no error at
//!     all. Status alone therefore tells you nothing; [`gql`] reads the body.
//!   * A wrong `Origin` is a hard 403 (`request-origin not allowed`) while no
//!     `Origin` is a plain 200, so nothing here ever sets one. This repo's
//!     browser-shaped header set does not include `Origin`, which is why the
//!     ordinary [`FetchOptions`] defaults are safe to use.
//!   * `pagination.first` is clamped in silence, so a crawl that asks for more
//!     just gets the ceiling and a wrong idea of how far it got. The ceiling is
//!     per connection rather than global: `categories` stops at 100 and
//!     `matchEventLog` at 150.
//!
//! The venue's unit of grouping is a *category* (32 NFL teams under
//! `big-game-champion-2027`) and its unit of trading is a *market*, one leg of
//! that category with a numbered outcome pair — index 1 is always the YES side,
//! whatever it is called. Categories are named by a slug a trader would type;
//! legs are named by a database integer nobody would. See [`make_ticker`].
//!
//! Money arrives in three dialects and none of them leaves this file: book and
//! quote prices are plain dollars, the tape is 1e18-scaled decimal strings, and
//! turnover is US *dollars* rather than contracts — which the venue's registry
//! note says out loud, because a dollar volume silently compared against
//! Kalshi's contract volume is a wrong number rather than a missing one.

use std::sync::{Arc, LazyLock};
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::{json, Value};
use terminal_core::slug::strip_dates;
use terminal_core::types::{
    BookLevel, Candle, CandleInterval, CandlesResponse, Market, MarketStatus, OrderBook,
    SeriesInfo, Trade, TradesResponse, Venue, VenueEvent,
};
use terminal_core::util::{round4, round_to};
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

/// Every row this module produces is stamped with it, so a merged board can
/// say which broker a price came from.
const VENUE: Venue = Venue::PredictFun;

/// What joins a category slug to a leg id in a contract ticker.
///
/// A hyphen would be unreadable in both directions: the venue's own slugs end in
/// hyphenated numbers (`btc-updown-15m-1787032800`, `big-game-champion-2027`),
/// so `slug-26952` cannot be split back into its parts without guessing which
/// trailing number is the leg. `~` appears in no slug, so the split is exact.
const TICKER_SEPARATOR: char = '~';

/* --------------------------------------------------------------- transport */

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
struct GraphQlBody<T> {
    data: Option<T>,
    errors: Option<Vec<GraphQlError>>,
}

/// Hand-written rather than derived: the derive would demand `T: Default` of
/// every envelope, and an envelope's payload is only ever absent or parsed.
impl<T> Default for GraphQlBody<T> {
    fn default() -> Self {
        Self {
            data: None,
            errors: None,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct GraphQlError {
    message: Option<String>,
    extensions: Option<GraphQlErrorExtensions>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct GraphQlErrorExtensions {
    code: Option<String>,
}

/// Run one query and hand back its `data`, or fail.
///
/// Sent as GET with the query in the URL. The endpoint serves both verbs for
/// queries, and GET is the one that goes through this repo's [`crate::http`] —
/// which brings the timeout, the bounded retries, the response ceiling and the
/// transport-error vocabulary that every other venue module already relies on.
/// A hand-rolled POST would have to restate all four, in a file that is supposed
/// to be about predict.fun.
///
/// The shared fetch also covers the nginx failure mode for free: when the
/// endpoint answers an overweight query with an HTML error page, the parse fails
/// and the caller gets an [`UpstreamError`] rather than a deserialisation panic
/// deep inside a normaliser. What is left for this helper is the
/// 200-with-`errors` case, which no HTTP-level check would ever notice.
///
/// `predictfun_graphql_base` is the whole endpoint URL rather than a prefix, so
/// the query string is appended to it directly and a wiremock server can stand
/// in for the host without the module knowing.
async fn gql<T>(
    state: &AppState,
    key: &str,
    cache_ttl: Duration,
    query: &str,
    variables: Value,
    timeout: Duration,
) -> Result<Arc<T>>
where
    T: DeserializeOwned + Send + Sync + 'static,
{
    let mut url = format!(
        "{}?query={}",
        state.config().predictfun_graphql_base,
        urlencoding::encode(query)
    );
    if variables.as_object().is_some_and(|map| !map.is_empty()) {
        url.push_str("&variables=");
        url.push_str(&urlencoding::encode(&variables.to_string()));
    }

    state
        .cache()
        .cached(&format!("predictfun:{key}"), cache_ttl, || async {
            let body: GraphQlBody<T> = state
                .http()
                .fetch_json(
                    &url,
                    FetchOptions::new()
                        .timeout(timeout)
                        .retries(2)
                        // A crawl page of 100 categories with every leg runs to
                        // ~780 KB and the 30-leg timeseries to ~160 KB, so the
                        // ceiling is raised only against a catalogue that grows
                        // an order of magnitude, not against today's.
                        .max_bytes(32 * 1024 * 1024),
                )
                .await?;

            if let Some(failure) = body.errors.as_ref().and_then(|list| list.first()) {
                let reported = failure
                    .extensions
                    .as_ref()
                    .and_then(|ext| ext.code.as_deref())
                    .unwrap_or("none");
                return Err(UpstreamError::new(
                    format!(
                        "predict.fun rejected the query: {}",
                        failure.message.as_deref().unwrap_or("unknown error")
                    ),
                    crate::error::codes::UPSTREAM_ERROR,
                )
                .with_hint(format!(
                    "The GraphQL endpoint answers a rejected query with HTTP 200 and an error \
                     body. Reported code: {reported}."
                )));
            }

            body.data.ok_or_else(|| {
                UpstreamError::new(
                    "predict.fun returned no data for the query",
                    crate::error::codes::UPSTREAM_ERROR,
                )
            })
        })
        .await
}

/* --------------------------------------------------------------- coercion */

/// A published number, or `None` where the venue published nothing.
///
/// Unlike the Polymarkets, this venue says `null` when it means "no resting
/// order" rather than `0`, so there is no zero to undo here — and a `0` that
/// does arrive is the venue's own statement and survives as one. Nulls are not
/// an edge case: 266 of the 1,926 outcomes on the busiest catalogue page carry
/// no bid, so every quote read goes through here.
pub fn figure(value: Option<f64>) -> Option<f64> {
    value.filter(|n| n.is_finite())
}

/// A price the venue sent as text, as the last-print field does (`"0.14"`).
///
/// Both spellings are accepted because the field is documented as neither: it
/// arrives as a string on every leg sampled, and a venue that starts sending the
/// same figure as a number should not blank the last-price column to announce it.
pub fn decimal(value: Option<&Value>) -> Option<f64> {
    match value {
        Some(Value::Number(number)) => figure(number.as_f64()),
        Some(Value::String(text)) => {
            let trimmed = text.trim();
            // JavaScript's `Number("")` is 0, so an empty or blank string has to
            // be turned away here rather than parsed into a free contract.
            if trimmed.is_empty() {
                return None;
            }
            figure(trimmed.parse::<f64>().ok())
        }
        _ => None,
    }
}

/// Read one of the tape's 1e18-scaled integer strings.
///
/// Both the filled size and the executed price arrive this way
/// (`"207230000000000000000"` is 207.23 shares, `"70000000000000000"` is 7¢).
/// Anything that is not a run of digits is a shape this module does not
/// understand, and reporting it as `0` would put a free trade on the tape.
pub fn from_wei(value: Option<&Value>) -> Option<f64> {
    let owned;
    let text = match value {
        Some(Value::String(text)) => text.trim(),
        Some(Value::Number(number)) => {
            owned = number.to_string();
            owned.as_str()
        }
        _ => return None,
    };

    let digits = text.strip_prefix('-').unwrap_or(text);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    figure(text.parse::<f64>().ok().map(|n| n / 1e18))
}

/// The venue's own YES↔NO price mirror.
///
/// Taken verbatim from predict.fun's order-book note, rounding included: the
/// book holds YES prices only, and the NO ladder is `1 - price` computed at the
/// *market's* `decimalPrecision`. That precision is per leg and genuinely
/// differs from its category's — 326 of the 963 legs on a live catalogue page
/// disagree with the category that owns them, `big-game-champion-2027` among
/// them, where legs quoted to three places sit in a category quoted to two — so
/// using the category's would put a NO ladder a tenth of a cent away from the
/// one the exchange itself shows.
///
/// The half-up rounding is JavaScript's `Math.round` rather than Rust's
/// `f64::round`, for the reason [`round4`] spells out: the two disagree on a
/// negative half, and a ladder that rounds differently from the exchange's own
/// arithmetic is off by a tick at every rung.
pub fn complement(price: f64, decimal_precision: Option<i64>) -> f64 {
    let places = decimal_precision
        .filter(|p| (1..=6).contains(p))
        .unwrap_or(2);
    let factor = 10f64.powi(places as i32);
    (factor - (price * factor + 0.5).floor()) / factor
}

/* ------------------------------------------------------------ raw upstream */

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RawPfPageInfo {
    pub has_next_page: Option<bool>,
    pub end_cursor: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RawPfEdge<T> {
    pub cursor: Option<String>,
    pub node: Option<T>,
}

impl<T> Default for RawPfEdge<T> {
    fn default() -> Self {
        Self {
            cursor: None,
            node: None,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RawPfConnection<T> {
    pub total_count: Option<i64>,
    pub page_info: Option<RawPfPageInfo>,
    pub edges: Option<Vec<Option<RawPfEdge<T>>>>,
}

/// Also hand-written, for the reason [`GraphQlBody`]'s is: a connection of a
/// type that has no `Default` is still an empty connection.
impl<T> Default for RawPfConnection<T> {
    fn default() -> Self {
        Self {
            total_count: None,
            page_info: None,
            edges: None,
        }
    }
}

/// Flatten a Relay connection, which is how every list on this API arrives.
pub fn nodes<T>(connection: Option<&RawPfConnection<T>>) -> Vec<&T> {
    connection
        .and_then(|c| c.edges.as_ref())
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|edge| edge.node.as_ref())
        .collect()
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RawPfOutcomeStatistics {
    /// Shares outstanding. See [`normalise_market`] for why this is the open
    /// interest figure rather than a restated volume.
    pub shares_count: Option<f64>,
    pub positions_value_usd: Option<f64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RawPfOutcome {
    pub id: Option<String>,
    /// 1 is the YES side on every market, whatever the venue named it. Confirmed
    /// across 541 live legs: every one carries exactly indices 1 and 2, and the
    /// names run `Yes`/`No` on most, `Up`/`Down`, `T1`/`DNS` or two team codes
    /// on the rest — in that order.
    pub index: Option<i64>,
    pub name: Option<String>,
    pub status: Option<String>,
    pub chance_percentage: Option<f64>,
    pub bid_price_in_currency: Option<f64>,
    pub ask_price_in_currency: Option<f64>,
    pub statistics: Option<RawPfOutcomeStatistics>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RawPfMarketStatistics {
    pub volume_total_usd: Option<f64>,
    pub volume24h_usd: Option<f64>,
    /// Turnover against the previous day, not a price move. Unread.
    pub volume24h_change_usd: Option<f64>,
    /// Published, unsigned, and deliberately unread: it is the 24h *range*.
    ///
    /// It is named like the daily move and behaves like a spread. It is never
    /// negative — 0 negative values against 293 positive ones across the 963
    /// legs of a live catalogue page, re-measured for this port — and where it
    /// is non-zero it tracks max − min of the venue's own chance series rather
    /// than the distance between its ends: leg 932250 reported 15.5 over a day
    /// that finished 0.4 lower, and leg 935920 reported 29.0 over a 29-point
    /// *fall*. Passing it through as [`Market::change`] would paint every crash
    /// on this venue green.
    pub percentage_chance_change24h: Option<f64>,
    pub liquidity3_c_ask_usd: Option<f64>,
    /// Present, unscaled and unread — see [`normalise_market`].
    pub total_liquidity_usd: Option<f64>,
}

/// One resting rung, `[price, size]`, price in dollars and size in shares.
///
/// Kept as a `Value` row rather than a fixed pair so a short or malformed level
/// is dropped by [`normalise_order_book`] instead of failing the whole market:
/// one unreadable rung must not cost the reader the book, the quote and the
/// leg's name along with it.
pub type RawPfLevel = Value;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RawPfLastOrder {
    /// Whole cents, in YES terms whichever outcome is named — see
    /// [`last_print_of`].
    pub price: Option<Value>,
    pub side: Option<String>,
    /// The side the resting order sat on, not the space its price is quoted in.
    pub outcome: Option<String>,
    pub kind: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RawPfOrderbook {
    pub market_id: Option<i64>,
    /// Cheapest first.
    pub asks: Option<Vec<RawPfLevel>>,
    /// Best (highest) first.
    pub bids: Option<Vec<RawPfLevel>>,
    pub last_order_settled: Option<RawPfLastOrder>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RawPfResolution {
    pub index: Option<i64>,
    pub name: Option<String>,
    pub status: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RawPfMarket {
    pub id: Option<String>,
    /// The short leg label: `"Los Angeles Rams"`, `">¥42B"`, `"↑ 5.5%"`.
    pub title: Option<String>,
    /// The full question sentence.
    pub question: Option<String>,
    pub description: Option<String>,
    pub status: Option<String>,
    pub market_type: Option<String>,
    pub is_trading_enabled: Option<bool>,
    pub decimal_precision: Option<i64>,
    /// Published, and deliberately unread. See [`normalise_market`].
    pub chance_percentage: Option<f64>,
    pub near_midpoint_liquidity_usd: Option<f64>,
    pub resolution: Option<RawPfResolution>,
    pub statistics: Option<RawPfMarketStatistics>,
    pub orderbook: Option<RawPfOrderbook>,
    pub outcomes: Option<RawPfConnection<RawPfOutcome>>,
    pub category: Option<RawPfCategory>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RawPfCategoryStatistics {
    pub volume_total_usd: Option<f64>,
    pub volume24h_usd: Option<f64>,
    pub liquidity3_c_ask_usd: Option<f64>,
    pub liquidity_value_usd: Option<f64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RawPfTagRef {
    pub id: Option<String>,
    pub name: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RawPfCategory {
    pub id: Option<String>,
    pub slug: Option<String>,
    pub title: Option<String>,
    pub description: Option<String>,
    pub status: Option<String>,
    pub market_variant: Option<String>,
    /// The venue's own statement that the legs exclude each other.
    pub is_neg_risk: Option<bool>,
    pub decimal_precision: Option<i64>,
    pub starts_at: Option<String>,
    pub ends_at: Option<String>,
    pub statistics: Option<RawPfCategoryStatistics>,
    pub tags: Option<RawPfConnection<RawPfTagRef>>,
    pub markets: Option<RawPfConnection<RawPfMarket>>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RawPfTradeCategory {
    pub slug: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RawPfTradeMarket {
    pub id: Option<String>,
    pub title: Option<String>,
    pub category: Option<RawPfTradeCategory>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RawPfTradeOutcome {
    pub index: Option<i64>,
    pub name: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RawPfAccount {
    pub address: Option<String>,
    pub name: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RawPfTrade {
    pub transaction_hash: Option<String>,
    /// 1e18-scaled shares.
    pub amount_filled: Option<Value>,
    /// 1e18-scaled dollars, in the named outcome's own price space.
    pub price_executed: Option<Value>,
    pub quote_type: Option<String>,
    /// ISO-8601 UTC, one-second granularity.
    pub timestamp: Option<String>,
    pub market: Option<RawPfTradeMarket>,
    pub outcome: Option<RawPfTradeOutcome>,
    pub account: Option<RawPfAccount>,
}

/// One probability sample: `x` unix seconds, `y` a percentage 0..100.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RawPfPoint {
    pub x: Option<f64>,
    pub y: Option<f64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RawPfSeriesMarket {
    pub id: Option<String>,
    pub title: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RawPfSeries {
    /// Asked for, and not read: it under-reports. The `_1H` window says `_1m`
    /// over samples 120 seconds apart and the `_1D` window says `_5m` over
    /// samples 600 seconds apart, both measured live. The spacing that matters
    /// is the one in the `x` values, and [`bucket_samples`] reads those.
    pub data_granularity: Option<String>,
    pub market: Option<RawPfSeriesMarket>,
    pub data: Option<RawPfConnection<RawPfPoint>>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RawPfTag {
    pub id: Option<String>,
    pub name: Option<String>,
    /// Open categories carrying the tag — the count asked for with a filter.
    pub open: Option<i64>,
    pub parent: Option<RawPfTagRef>,
    pub children: Option<Vec<Option<RawPfTagRef>>>,
}

/* ------------------------------------------------------------ identifiers */

/// The terminal's name for one leg: `<category slug>~<leg id>`.
///
/// Neither half stands alone. The slug is what a trader types and what the
/// website URL uses, but most of the venue's open categories hold more than one
/// leg — 963 legs across the 100 busiest categories — so a slug alone does not
/// name a contract. The leg id is exact but is a bare database integer: `26952`
/// says nothing about what it is, and the venue publishes no per-leg page to
/// look it up on. Carrying both makes the ticker readable and reversible, and
/// [`get_market`] accepts either half on its own for the cases where that is
/// unambiguous.
pub fn make_ticker(category_slug: &str, market_id: &str) -> String {
    if category_slug.is_empty() {
        return market_id.to_string();
    }
    format!("{category_slug}{TICKER_SEPARATOR}{market_id}")
}

/// Split a ticker back up, on the *last* separator. `None` when there is none.
pub fn parse_ticker(ticker: &str) -> Option<(&str, &str)> {
    ticker.rsplit_once(TICKER_SEPARATOR)
}

/// A slug segment that is a machine-minted stamp rather than part of a question.
fn is_stamp(part: &str) -> bool {
    part.len() >= 6 && part.bytes().all(|b| b.is_ascii_digit())
}

/// Drop a short trailing digit run, which is where this venue disambiguates.
fn without_trailing_run(slug: &str) -> &str {
    match slug.rsplit_once('-') {
        Some((head, last))
            if (3..=5).contains(&last.len()) && last.bytes().all(|b| b.is_ascii_digit()) =>
        {
            head
        }
        _ => slug,
    }
}

/// The recurring question behind a slug.
///
/// Dates come off through the shared [`strip_dates`], which is the same rule
/// Polymarket US's slugs need and for the same reason: `fed-decision-in-september`
/// and `fed-decision-in-october` are one question asked twice, and leaving the
/// month on makes them two one-event series, neither of which then lines up
/// against `KXFEDDECISION` at Kalshi — which is the pairing this terminal exists
/// to draw.
///
/// Two suffixes are this venue's own, and are taken off **first**, because
/// [`strip_dates`] reads whole segments rather than digit runs and would leave
/// both in place. A machine-minted stamp — ten digits of unix seconds on the
/// crypto books (`btc-updown-15m-1787032800`, a new slug every fifteen minutes)
/// or seventeen of creation time on the season books
/// (`laliga-2027-champion-20260701200737375`) — is dropped wherever it appears.
/// A short trailing run is dropped only at the end (`fed-decision-in-september-762`),
/// because that position is where this venue puts a disambiguator and nowhere
/// else: a number in the middle of a slug is part of the question, and
/// `nasdaq-100` should not become `nasdaq`.
///
/// The order is load-bearing and was checked against the live catalogue while
/// porting: today's two open Fed books are `fed-decision-in-september-762` and
/// `fed-decision-in-october-20260617190323537`, and only stripping stamp, then
/// short run, then dates folds both onto `fed-decision-in` — which is exactly
/// the series the curated cross-venue link in
/// [`crate::sources::crossvenue`] names.
pub fn series_from_slug(slug: &str) -> String {
    let without_stamps = slug
        .split('-')
        .filter(|part| !is_stamp(part))
        .collect::<Vec<_>>()
        .join("-");
    let trimmed = without_trailing_run(&without_stamps);

    let stripped = strip_dates(trimmed);
    if stripped.is_empty() {
        trimmed.to_string()
    } else {
        stripped
    }
}

/* ------------------------------------------------------------ normalisers */

/// Leg status, read off the leg rather than its category.
///
/// A settled leg lives happily inside an open category — 37 resolved and 14
/// price-proposed legs among the 963 on a live catalogue page of *open*
/// categories — so taking the category's word would show a decided market as
/// tradable. `REGISTERED` is the ordinary live state here, not a pre-open one,
/// and `isTradingEnabled` is the venue's halt switch on top of it.
pub fn status_of(raw: &RawPfMarket) -> MarketStatus {
    let stated = raw.status.as_deref().unwrap_or_default().to_uppercase();
    match stated.as_str() {
        "RESOLVED" => "settled".into(),
        "PRICE_PROPOSED" | "PRICE_DISPUTED" => "determined".into(),
        "PAUSED" => "closed".into(),
        "INITIALIZING" | "INITIALIZED" | "CREATING" => "unopened".into(),
        "CREATED" | "REGISTERED" | "UNPAUSED" => {
            if raw.is_trading_enabled == Some(false) {
                "closed".into()
            } else {
                "open".into()
            }
        }
        "" => "open".into(),
        _ => stated.to_lowercase(),
    }
}

/// Which side won, by outcome index rather than by name.
///
/// The names are whatever the question needed — `DNS`, `Down`, a team — so only
/// the index carries the yes/no meaning the terminal settles on.
fn result_of(raw: &RawPfMarket) -> String {
    match raw.resolution.as_ref().and_then(|r| r.index) {
        Some(1) => "yes".into(),
        Some(2) => "no".into(),
        _ => String::new(),
    }
}

/// The label that identifies a leg on screen and in the search index.
///
/// The venue splits the label across two fields and which one matters flips by
/// market. On the NFL book the outcomes are the generic `Yes`/`No` pair and the
/// leg title is the team; on the esports book the leg title is `Match Winner`
/// for every leg and the outcomes are `T1`/`DNS`. Preferring a named outcome and
/// falling back to the title gets both right, where either field alone reduces
/// one whole family of markets to identical rows.
fn leg_label(title: &str, outcome_name: Option<&str>, yes_side: bool) -> String {
    let name = outcome_name.unwrap_or_default().trim();
    let generic = name.eq_ignore_ascii_case("yes") || name.eq_ignore_ascii_case("no");
    if !name.is_empty() && !generic {
        return name.to_string();
    }
    if yes_side {
        if title.is_empty() {
            return name.to_string();
        }
        return title.to_string();
    }
    if name.is_empty() {
        "No".to_string()
    } else {
        name.to_string()
    }
}

/// The last print, which is already quoted in YES terms.
///
/// `lastOrderSettled` names an outcome beside the price, which reads as an
/// invitation to flip a NO print into YES terms — the tape really does work that
/// way, so the two look alike and behave differently. Of 137 live legs whose
/// last settled order names the NO outcome, 110 sit nearer the YES mid as quoted
/// than inverted, and the ones that decide it are unambiguous: leg 1457355 is
/// quoted 0.01 on YES and reports `{price: "0.01", outcome: "No"}`, which
/// inverted would have printed 99¢ on a penny market. `outcome` names the side
/// the resting order was on, not the space its price is in.
///
/// The price is also the venue's display figure rather than a tick-exact fill:
/// 340 of 340 sampled prints carry exactly two decimals, including on legs
/// quoted to three, so a sub-cent market can report a last of `0.00`. The exact
/// fills are on the tape — see [`normalise_trades`].
pub fn last_print_of(raw: &RawPfMarket) -> Option<f64> {
    decimal(
        raw.orderbook
            .as_ref()
            .and_then(|book| book.last_order_settled.as_ref())
            .and_then(|last| last.price.as_ref()),
    )
}

/// One leg, quoted from its own outcome pair.
///
/// **`chancePercentage` is not read, on purpose.** It looks like a midpoint and
/// is not one: across 751 live legs with a two-sided book it agreed with the
/// book's mid on 5.7% of them, and it disagrees loudly rather than subtly — leg
/// 1459048 quotes 0.10/0.85 and reports a chance of 70. It is also not rounded
/// to anything, returning raw float artifacts like 18.999999999999993. Whatever
/// it is (a smoothed or last-trade figure the venue never documents), printing
/// it in a mid column would put a price on screen that no side of the book
/// supports, so the mid is computed here from the YES outcome's own bid and ask
/// and the last print comes from the book's last settled order.
///
/// Open interest is the one figure the venue publishes under an unrecognisable
/// name: `sharesCount` on each outcome is shares outstanding, and the YES and NO
/// sides agree to five figures on every leg sampled — the signature of minted
/// pairs, not of a volume restatement.
pub fn normalise_market(raw: &RawPfMarket, category: Option<&RawPfCategory>) -> Market {
    let parent = category.or(raw.category.as_ref());
    let slug = parent
        .and_then(|c| c.slug.as_deref().or(c.id.as_deref()))
        .unwrap_or_default();
    let id = raw.id.as_deref().unwrap_or_default();

    let outcomes = nodes(raw.outcomes.as_ref());
    let yes = outcomes.iter().find(|o| o.index == Some(1));
    let no = outcomes.iter().find(|o| o.index == Some(2));

    let yes_bid = figure(yes.and_then(|o| o.bid_price_in_currency));
    let yes_ask = figure(yes.and_then(|o| o.ask_price_in_currency));
    let last_price = last_print_of(raw);

    // The venue states the NO side itself, and it agrees with the mirror of the
    // YES side to the last decimal — exactly, on all 490 live legs quoting both.
    // The mirror is the fallback for the leg that is quoted on one side only, so
    // a half-published book still ladders.
    let no_bid = figure(no.and_then(|o| o.bid_price_in_currency))
        .or_else(|| yes_ask.map(|ask| complement(ask, raw.decimal_precision)));
    let no_ask = figure(no.and_then(|o| o.ask_price_in_currency))
        .or_else(|| yes_bid.map(|bid| complement(bid, raw.decimal_precision)));

    let mid = match (yes_bid, yes_ask) {
        (Some(bid), Some(ask)) => Some(round4((bid + ask) / 2.0)),
        _ => last_price.or(yes_bid).or(yes_ask),
    };

    let tag = nodes(parent.and_then(|c| c.tags.as_ref()))
        .first()
        .and_then(|t| t.name.clone())
        .unwrap_or_default();

    let title = raw.title.as_deref().unwrap_or_default();

    Market {
        venue: VENUE,
        ticker: make_ticker(slug, id),
        event_ticker: slug.to_string(),
        series_ticker: series_from_slug(slug),
        title: raw
            .question
            .clone()
            .or_else(|| raw.title.clone())
            .unwrap_or_default(),
        yes_sub_title: leg_label(title, yes.and_then(|o| o.name.as_deref()), true),
        no_sub_title: leg_label(title, no.and_then(|o| o.name.as_deref()), false),
        status: status_of(raw),
        market_type: raw
            .market_type
            .clone()
            .or_else(|| parent.and_then(|c| c.market_variant.clone()))
            .unwrap_or_default(),
        yes_bid,
        yes_ask,
        no_bid,
        no_ask,
        mid,
        last_price,
        // The venue states a 24h *range* and no earlier price at all — see
        // `percentage_chance_change24h` on [`RawPfMarketStatistics`]. A range has
        // no direction, and the terminal colours this column by its sign, so both
        // fields stay unstated rather than painting every fall as a rise.
        previous_price: None,
        change: None,
        // Dollars, not contracts — the venue meters turnover in collateral and
        // the registry note says so, because the honest number in the wrong unit
        // is more dangerous silently than the missing one is loudly.
        volume: figure(raw.statistics.as_ref().and_then(|s| s.volume_total_usd)),
        volume24h: figure(raw.statistics.as_ref().and_then(|s| s.volume24h_usd)),
        open_interest: figure(
            yes.and_then(|o| o.statistics.as_ref())
                .and_then(|s| s.shares_count),
        ),
        // Dollars resting within 3¢ of the ask. `totalLiquidityUsd` sits next to
        // it and is unscaled to the point of being meaningless — 599,342,012 on
        // a leg whose whole book is about $15.7k — so it is never read.
        liquidity: figure(raw.statistics.as_ref().and_then(|s| s.liquidity3_c_ask_usd)),
        open_time: parent.and_then(|c| c.starts_at.clone()).unwrap_or_default(),
        // The venue draws no line between the last trade and settlement; one
        // timestamp closes the category and every leg under it.
        close_time: parent.and_then(|c| c.ends_at.clone()).unwrap_or_default(),
        expiration_time: parent.and_then(|c| c.ends_at.clone()).unwrap_or_default(),
        result: result_of(raw),
        // The leg's own terms when the leg was read on its own, the category's
        // when it came out of the catalogue — see [`MARKET_FIELDS`].
        rules_primary: raw
            .description
            .clone()
            .or_else(|| parent.and_then(|c| c.description.clone()))
            .unwrap_or_default(),
        category: Some(tag).filter(|t| !t.is_empty()),
        // Strikes exist here only as prose inside the leg title (`">¥42B"`,
        // `"↑ 5.5%"`). Nothing in the payload asserts a bound, and a regex over
        // a title would be a guess printed in a column the terminal treats as
        // fact.
        strike_type: None,
        floor_strike: None,
        cap_strike: None,
    }
}

/// One category, with every leg under it.
///
/// `isNegRisk` is the venue's own statement that the legs exclude each other —
/// it is what makes the site offer NO-to-opposing-YES conversion on the
/// categories that carry it — so it maps straight through. The rest get `false`,
/// which is also what an absent flag gets: `EVT` draws its Σmid arbitrage line
/// only on a stated exclusivity, and a guessed one puts a trade on screen that
/// does not exist.
pub fn normalise_event(raw: &RawPfCategory) -> VenueEvent {
    let slug = raw
        .slug
        .as_deref()
        .or(raw.id.as_deref())
        .unwrap_or_default();

    VenueEvent {
        venue: VENUE,
        event_ticker: slug.to_string(),
        series_ticker: series_from_slug(slug),
        title: raw.title.clone().unwrap_or_else(|| slug.to_string()),
        sub_title: String::new(),
        category: nodes(raw.tags.as_ref())
            .first()
            .and_then(|t| t.name.clone())
            .unwrap_or_default(),
        mutually_exclusive: raw.is_neg_risk == Some(true),
        markets: nodes(raw.markets.as_ref())
            .into_iter()
            .map(|market| normalise_market(market, Some(raw)))
            .collect(),
    }
}

/* -------------------------------------------------------------------- book */

/// One rung, or nothing where the row is not two numbers.
fn level(row: &RawPfLevel) -> Option<BookLevel> {
    let pair = row.as_array().filter(|row| row.len() >= 2)?;
    Some(BookLevel {
        price: figure(pair[0].as_f64()).unwrap_or(0.0),
        size: figure(pair[1].as_f64()).unwrap_or(0.0),
    })
}

/// Every readable rung of one side, with the rows that price or size nothing
/// dropped: a level resting zero size at zero dollars is padding, and drawing
/// it would put a free contract at the top of a ladder.
fn levels(rows: Option<&Vec<RawPfLevel>>) -> Vec<BookLevel> {
    rows.into_iter()
        .flatten()
        .filter_map(level)
        .filter(|rung| rung.price > 0.0 && rung.size > 0.0)
        .collect()
}

/// The ladder, with the NO side mirrored from it.
///
/// The exchange keeps one book per leg and stores it in YES prices only, so the
/// NO bids a trader can hit are the YES asks read backwards. Depth is applied
/// here because the venue takes no depth argument at all — it returns the whole
/// book, dozens of levels on a busy leg — and the sort is re-applied before
/// slicing so the top of the book cannot be cut off by an upstream that
/// reorders.
pub fn normalise_order_book(raw: &RawPfMarket, ticker: &str, depth: usize) -> OrderBook {
    let book = raw.orderbook.as_ref();

    let mut yes = levels(book.and_then(|b| b.bids.as_ref()));
    yes.sort_by(|a, b| b.price.total_cmp(&a.price));
    yes.truncate(depth);

    let mut yes_asks = levels(book.and_then(|b| b.asks.as_ref()));
    yes_asks.sort_by(|a, b| a.price.total_cmp(&b.price));
    yes_asks.truncate(depth);

    let mut no: Vec<BookLevel> = yes_asks
        .iter()
        .map(|rung| BookLevel {
            price: complement(rung.price, raw.decimal_precision),
            size: rung.size,
        })
        .collect();
    no.sort_by(|a, b| b.price.total_cmp(&a.price));

    let best_yes_bid = yes.first().map(|rung| rung.price);
    let best_yes_ask = yes_asks.first().map(|rung| rung.price);

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

/* -------------------------------------------------------------------- tape */

/// Unix seconds from one of the tape's ISO-8601 stamps.
///
/// The venue writes them to the millisecond and always at `.000`, but the
/// rounding is done on the exact nanoseconds rather than by truncation, so a
/// venue that starts stamping mid-second does not silently move every print back
/// to the previous one.
fn seconds_of(timestamp: Option<&str>) -> Option<i64> {
    let moment = OffsetDateTime::parse(timestamp?, &Rfc3339).ok()?;
    let rounded = (moment.unix_timestamp_nanos() + 500_000_000) / 1_000_000_000;
    i64::try_from(rounded).ok()
}

/// The public tape.
///
/// Two upstream habits are corrected here. Sizes and prices are 1e18-scaled
/// strings, and a price is quoted in whichever outcome the fill names — a NO
/// fill at 0.951 is the same trade as a YES fill at 0.049 — so every row is
/// re-based to YES before it reaches a chart or a VWAP.
///
/// The identity is the transaction hash, not the edge cursor. The cursor
/// base64-decodes to `{orderId, createdAt}` and therefore repeats across several
/// fills of one resting order: 50 rows of one leg carried 44 distinct cursors
/// and 50 distinct hashes, so keying on the cursor silently drops an eighth of
/// the tape as duplicates.
///
/// `taker_side` is left empty because the venue documents no aggressor side and
/// the data does not supply one. Its `quoteType` was read as the taker's
/// direction until the same leg was seen printing `Yes/ASK` and `Yes/BID` at one
/// identical price, which that reading cannot produce; the distribution fits the
/// resting side better than the aggressing one. An unknown side prints as
/// unknown rather than colouring half the tape wrong.
pub fn normalise_trades(
    raw: Option<&RawPfConnection<RawPfTrade>>,
    fallback_ticker: &str,
) -> Vec<Trade> {
    let mut trades: Vec<Trade> = Vec::new();

    for row in nodes(raw) {
        let size = from_wei(row.amount_filled.as_ref());
        let executed = from_wei(row.price_executed.as_ref());
        let ts = seconds_of(row.timestamp.as_deref());
        // A fill with no readable size, price or clock is not a fill the tape
        // can show. Defaulting any of the three would put a free trade, an empty
        // one, or one stamped 1970 in a column the reader scans for outliers.
        let (Some(size), Some(executed), Some(ts)) = (size, executed, ts) else {
            continue;
        };

        let yes_price = if row.outcome.as_ref().and_then(|o| o.index) == Some(2) {
            round4(1.0 - executed)
        } else {
            round4(executed)
        };

        let slug = row
            .market
            .as_ref()
            .and_then(|m| m.category.as_ref())
            .and_then(|c| c.slug.as_deref())
            .unwrap_or_default();
        let market_id = row
            .market
            .as_ref()
            .and_then(|m| m.id.as_deref())
            .unwrap_or_default();
        let ticker = if !market_id.is_empty() && !slug.is_empty() {
            make_ticker(slug, market_id)
        } else {
            fallback_ticker.to_string()
        };

        trades.push(Trade {
            venue: VENUE,
            trade_id: row
                .transaction_hash
                .clone()
                .unwrap_or_else(|| format!("{ticker}:{ts}:{size}")),
            ticker,
            ts,
            count: round_to(size, 6),
            yes_price,
            no_price: round4(1.0 - yes_price),
            taker_side: String::new(),
            is_block_trade: false,
        });
    }

    trades
}

/* ----------------------------------------------------------------- history */

/// Flat bars bridge one gap. Beyond this the gap is left as a gap.
const MAX_CARRY_FORWARD: i64 = 512;

/// A bar that carries the previous close forward, marked untraded so the chart
/// can draw the line without claiming anything printed in the period.
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

/// Bucket a probability sample series onto the terminal's candle grid.
///
/// predict.fun publishes no OHLC for its own markets — the history is a series
/// of `{x: unix seconds, y: chance in percent}` samples of the YES leg — so
/// open, high, low and close are the first, highest, lowest and last *sample* in
/// each period, and volume and open interest stay `None` because no size is
/// attached to any of it. Buckets are stamped with their period end, matching
/// Kalshi, so the same question drawn from two venues lands on one x-axis.
///
/// This is the same shape of arithmetic Polymarket International needs, kept
/// separate rather than shared: that module's samples are `{t, p}` in dollars
/// and belong to another venue's file, and reaching across venue modules would
/// make a change to one venue's history format a change to another's chart.
pub fn bucket_samples(points: &[&RawPfPoint], interval: CandleInterval) -> Vec<Candle> {
    let seconds = interval.seconds();
    let seconds_f = seconds as f64;

    struct Bucket {
        open: f64,
        high: f64,
        low: f64,
        close: f64,
    }

    // Ordered by period end as it is built, which is the order a chart draws in.
    let mut buckets: std::collections::BTreeMap<i64, Bucket> = std::collections::BTreeMap::new();

    for point in points {
        let (Some(t), Some(percent)) = (figure(point.x), figure(point.y)) else {
            continue;
        };
        let p = round4(percent / 100.0);

        // A sample on a boundary closes the period it ends, not the one it opens.
        let ceiling = (t / seconds_f).ceil() * seconds_f;
        let end = if ceiling == 0.0 {
            t as i64
        } else {
            ceiling as i64
        };

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

    let mut candles: Vec<Candle> = Vec::with_capacity(buckets.len());
    let mut previous: Option<(i64, f64)> = None;

    for (time, bucket) in &buckets {
        // The sample spacing is coarser than the finest bucket the terminal
        // draws (two minutes at the shortest lookback), so quiet periods are
        // bridged with flat carried-forward bars rather than left as holes in
        // the line. Bounded, because a series that stops mid-window must not
        // generate a million bars.
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

/* ---------------------------------------------------------------- taxonomy */

/// One tag of the venue's taxonomy, as a series row.
///
/// The ticker is the numeric tag id and not its name, because the id is the only
/// spelling the venue's own filter accepts — `categories(filter:{tag:"6"})` is
/// the query behind a category-filtered board, and a name would have to be
/// mapped back to it on every call.
///
/// `category` is the tag's immediate parent, or the tag itself when it is
/// top-level, so filtering on `Sports` returns Sports and the fourteen sub-tags
/// hanging directly off it. The tree is deeper than two levels in places — `UCL`
/// hangs off `Soccer`, which hangs off `Sports` — and a grandchild files under
/// its own parent rather than under the root, which is the grouping a reader
/// scanning for football tags actually wants.
pub fn normalise_series(raw: &RawPfTag) -> SeriesInfo {
    let children = raw
        .children
        .as_ref()
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|child| child.name.clone())
        .collect();

    SeriesInfo {
        venue: VENUE,
        ticker: raw.id.clone().unwrap_or_default(),
        title: raw
            .name
            .clone()
            .or_else(|| raw.id.clone())
            .unwrap_or_default(),
        category: raw
            .parent
            .as_ref()
            .and_then(|p| p.name.clone())
            .or_else(|| raw.name.clone())
            .unwrap_or_default(),
        // The venue states none, and a tag named `NBA` is not a licence to write
        // "seasonal" into a field the reader takes as the venue's own word.
        frequency: String::new(),
        tags: children,
    }
}

/* ---------------------------------------------------------------- queries */

/// The leg fields every view needs.
///
/// `description` is not among them, and that is the difference between a crawl
/// that weighs 780 KB a page and one that weighs well past a megabyte: the
/// settlement rules run to two kilobytes a leg and account for half of every
/// payload they appear in. Only `DES` renders them, and `DES` reads one leg
/// through [`MARKET_QUERY`], which asks for them there. A leg read out of the
/// catalogue falls back to its category's rules, which are the same terms
/// written once for the group.
///
/// The fee, on-chain and reward fields are left unasked because nothing in the
/// terminal reads them.
pub const MARKET_FIELDS: &str = "
  id title question status marketType isTradingEnabled decimalPrecision
  resolution { index name status }
  statistics { volumeTotalUsd volume24hUsd liquidity3CAskUsd }
  outcomes { edges { node { id index name status bidPriceInCurrency askPriceInCurrency statistics { sharesCount } } } }
";

/// The category fields a leg inherits: its dates, its exclusivity flag, its
/// bucket and the terms it states once for the whole group.
pub const CATEGORY_FIELDS: &str = "
  id slug title description status marketVariant isNegRisk decimalPrecision startsAt endsAt
  statistics { volumeTotalUsd volume24hUsd liquidity3CAskUsd }
  tags(pagination: { first: 5 }) { edges { node { id name } } }
";

/// The queries below are the wire contract, carried across from the TypeScript
/// verbatim and re-run against the live schema while porting: `Query` still
/// exposes `category`, `categories`, `timeseries`, `market`, `markets`,
/// `matchEventLog`, `categoryTags` and `search`.
///
/// The three that splice a field set in are built once behind a [`LazyLock`]
/// rather than formatted per request: a query text that is rebuilt on every
/// lookup is a query text that can differ between two lookups, and these are the
/// contract rather than a parameter.
static MARKET_QUERY: LazyLock<String> = LazyLock::new(|| {
    format!(
        "query Market($id: ID!) {{
  market(id: $id) {{
    {MARKET_FIELDS}
    description
    orderbook {{ asks bids lastOrderSettled {{ price side outcome kind }} }}
    category {{ {CATEGORY_FIELDS} }}
  }}
}}"
    )
});

static CATEGORY_QUERY: LazyLock<String> = LazyLock::new(|| {
    format!(
        "query Category($id: ID!) {{
  category(id: $id) {{
    {CATEGORY_FIELDS}
    markets(pagination: {{ first: 100 }}) {{ edges {{ node {{ {MARKET_FIELDS} }} }} }}
  }}
}}"
    )
});

pub const TRADES_QUERY: &str = "query Trades($marketId: ID!, $first: Int!) {
  market(id: $marketId) { id }
  matchEventLog(filter: { marketId: $marketId }, pagination: { first: $first }) {
    pageInfo { hasNextPage endCursor }
    edges { node {
      transactionHash amountFilled priceExecuted quoteType timestamp
      market { id title category { slug } }
      outcome { index name }
      account { address name }
    } }
  }
}";

pub const TIMESERIES_QUERY: &str = "query Timeseries($id: ID!, $interval: TimeseriesInterval!) {
  timeseries(categoryId: $id, filter: { interval: $interval }, pagination: { first: 100 }) {
    edges { node {
      dataGranularity
      market { id title }
      data(pagination: { first: 1000 }) { edges { node { x y } } }
    } }
  }
}";

pub const TAGS_QUERY: &str = "query Tags {
  categoryTags(pagination: { first: 100 }) {
    edges { node {
      id name
      open: totalCount(filter: { status: OPEN })
      parent { id name }
      children { id name }
    } }
  }
}";

static CRAWL_QUERY: LazyLock<String> = LazyLock::new(|| {
    format!(
        "query Crawl($after: String) {{
  categories(filter: {{ status: OPEN }}, sort: VOLUME_24H_DESC, pagination: {{ first: 100, after: $after }}) {{
    totalCount
    pageInfo {{ hasNextPage endCursor }}
    edges {{ node {{
      {CATEGORY_FIELDS}
      markets(pagination: {{ first: 100 }}) {{ edges {{ node {{ {MARKET_FIELDS} }} }} }}
    }} }}
  }}
}}"
    )
});

/* ----------------------------------------------------------------- lookups */

/// What to type instead, on every not-found this module raises. Written once
/// because a reader who mistyped one of the three spellings needs the same
/// sentence whichever command they were in.
const IDENTIFIER_HINT: &str = "predict.fun names a contract `<category slug>~<leg id>`, as in \
     `pf:big-game-champion-2027~26952`. The category slug alone works when the category has a \
     single leg, and the bare leg id (`pf:26952`) always works.";

/// `data.market`, which is `null` rather than an error when the id is unknown.
#[derive(Debug, Clone, Default, Deserialize)]
struct MarketEnvelope {
    market: Option<RawPfMarket>,
}

/// `data.category`, which is `null` rather than an error when the slug is wrong.
#[derive(Debug, Clone, Default, Deserialize)]
struct CategoryEnvelope {
    category: Option<RawPfCategory>,
}

/// The tape and, beside it, the leg itself — asked for in the same round trip so
/// an empty page can be told apart from a mistyped id. See [`get_trades`].
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TradesEnvelope {
    market: Option<RawPfTradeMarket>,
    match_event_log: Option<RawPfConnection<RawPfTrade>>,
}

/// Every leg's history for one category, because there is no way to ask for one.
#[derive(Debug, Clone, Default, Deserialize)]
struct TimeseriesEnvelope {
    timeseries: Option<RawPfConnection<RawPfSeries>>,
}

/// The tag tree, which is what this venue has instead of a series list.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TagsEnvelope {
    category_tags: Option<RawPfConnection<RawPfTag>>,
}

/// One page of the open catalogue, cursor and all.
#[derive(Debug, Clone, Default, Deserialize)]
struct CrawlEnvelope {
    categories: Option<RawPfConnection<RawPfCategory>>,
}

/// One leg with its book, its terms and its category, or a not-found.
///
/// `market(id:)` answers an unknown *integer* with `data.market == null` and no
/// error, so the miss is read off the payload rather than off the status.
async fn raw_market(state: &AppState, market_id: &str) -> Result<Arc<RawPfMarket>> {
    let data: Arc<MarketEnvelope> = gql(
        state,
        &format!("market:{market_id}"),
        ttl::QUOTE,
        &MARKET_QUERY,
        json!({ "id": market_id }),
        Duration::from_secs(25),
    )
    .await?;

    // The envelope is behind an `Arc` the cache owns, so the leg is cloned out
    // of it rather than borrowed: the alternative is handing every caller a
    // borrow of a cache entry that may be evicted under them.
    match data.market.clone() {
        Some(market) => Ok(Arc::new(market)),
        None => Err(
            UpstreamError::not_found(format!("No predict.fun market with id {market_id}"))
                .with_hint(IDENTIFIER_HINT),
        ),
    }
}

/// One category with every leg under it, or a not-found — read the same way,
/// off a `data.category` that is `null` rather than off an error.
async fn raw_category(state: &AppState, slug: &str) -> Result<Arc<RawPfCategory>> {
    let data: Arc<CategoryEnvelope> = gql(
        state,
        &format!("category:{slug}"),
        ttl::QUOTE,
        &CATEGORY_QUERY,
        json!({ "id": slug }),
        Duration::from_secs(25),
    )
    .await?;

    match data.category.clone() {
        Some(category) => Ok(Arc::new(category)),
        None => Err(
            UpstreamError::not_found(format!("No predict.fun category with slug {slug}"))
                .with_hint(IDENTIFIER_HINT),
        ),
    }
}

/// Turn whatever the trader typed into the leg id the API answers to.
///
/// Three spellings reach here and all three are worth accepting. The full ticker
/// is the canonical one. A bare leg id is what the venue itself prints in its
/// book and websocket topics. A bare category slug is what the website URL
/// shows, and for a single-leg category it names exactly one contract — for the
/// rest it names an event, which is a different command, so it is refused with
/// the legs listed rather than resolved to an arbitrary one.
///
/// The slug is never passed to `market(id:)`: that argument takes integers only
/// and answers anything else with `InvalidInputError. invalid id` — an error,
/// not a miss, and therefore a 200 with an `errors` array that would surface as
/// an upstream failure rather than as a typo.
async fn resolve_leg(state: &AppState, reference: &str) -> Result<String> {
    let parsed = parse_ticker(reference);
    let candidate = parsed.map_or(reference, |(_, id)| id);
    if !candidate.is_empty() && candidate.bytes().all(|b| b.is_ascii_digit()) {
        return Ok(candidate.to_string());
    }

    let slug = parsed.map_or(reference, |(slug, _)| slug);
    let category = raw_category(state, slug).await?;
    let legs = nodes(category.markets.as_ref());
    if let [only] = legs.as_slice() {
        if let Some(id) = only.id.as_deref().filter(|id| !id.is_empty()) {
            return Ok(id.to_string());
        }
    }

    let sample = legs
        .iter()
        .take(3)
        .map(|leg| {
            format!(
                "{} ({})",
                make_ticker(slug, leg.id.as_deref().unwrap_or_default()),
                leg.title.as_deref().unwrap_or("unnamed")
            )
        })
        .collect::<Vec<_>>()
        .join(", ");

    Err(UpstreamError::not_found(format!(
        "\"{slug}\" is a predict.fun category with {} legs, not a single contract",
        legs.len()
    ))
    .with_hint(format!(
        "Use `EVT pf:{slug}` for the whole event, or name a leg: {sample}\u{2026}"
    )))
}

/// One contract, from a ticker, a bare leg id or a single-leg category slug.
pub async fn get_market(state: &AppState, id: &str) -> Result<Market> {
    let leg = resolve_leg(state, id).await?;
    let raw = raw_market(state, &leg).await?;
    Ok(normalise_market(&raw, None))
}

/// One category with its legs, from a slug or from any leg's ticker.
pub async fn get_event(state: &AppState, id: &str) -> Result<VenueEvent> {
    // A trader who has a contract ticker in hand and wants its event should not
    // have to trim it by hand; the slug is the part before the separator.
    let slug = parse_ticker(id).map_or(id, |(slug, _)| slug);
    let raw = raw_category(state, slug).await?;
    Ok(normalise_event(&raw))
}

/// The ladder for one leg.
///
/// The book arrives inside the leg rather than on a path of its own, so this is
/// the same read [`get_market`] makes and the cached response serves both.
pub async fn get_order_book(state: &AppState, id: &str, depth: usize) -> Result<OrderBook> {
    let leg = resolve_leg(state, id).await?;
    let raw = raw_market(state, &leg).await?;
    let slug = raw
        .category
        .as_ref()
        .and_then(|c| c.slug.as_deref())
        .unwrap_or_default();
    Ok(normalise_order_book(&raw, &make_ticker(slug, &leg), depth))
}

/// What `matchEventLog` will actually return, whatever is asked for.
///
/// The clamp is silent and it is *not* the 100 that `categories` enforces: asked
/// for 200, 500 and 1,000 the tape returned 150 rows every time with
/// `hasNextPage` still true, on two different legs. The TypeScript this module
/// was ported from clamped to 100 for both connections, which under-served the
/// tape by a third; the ceiling is read per connection here because that is what
/// the live endpoint does.
const MAX_TRADES: usize = 150;

/// The public tape for one leg, newest first, with the venue's own cursor when
/// there is more behind it.
pub async fn get_trades(state: &AppState, id: &str, limit: usize) -> Result<TradesResponse> {
    let market_id = resolve_leg(state, id).await?;
    let first = limit.clamp(1, MAX_TRADES);

    let data: Arc<TradesEnvelope> = gql(
        state,
        &format!("trades:{market_id}:{first}"),
        ttl::QUOTE,
        TRADES_QUERY,
        json!({ "marketId": market_id, "first": first }),
        Duration::from_secs(25),
    )
    .await?;

    // The tape connection answers an unknown leg with an empty page rather than
    // an error — confirmed live, `market: null` beside `edges: []` — so the leg
    // is confirmed in the same round trip: a quiet market and a mistyped id must
    // not both read as a market nobody trades.
    if data.market.is_none() {
        return Err(
            UpstreamError::not_found(format!("No predict.fun market with id {market_id}"))
                .with_hint(IDENTIFIER_HINT),
        );
    }

    let log = data.match_event_log.as_ref();
    let page = log.and_then(|l| l.page_info.as_ref());
    Ok(TradesResponse {
        trades: normalise_trades(log, &market_id),
        cursor: page
            .filter(|info| info.has_next_page == Some(true))
            .and_then(|info| info.end_cursor.clone()),
    })
}

/// The lookback windows the history query accepts, shortest first.
///
/// There is no `from`/`to` on this API: the only knob is one of six fixed
/// windows ending at now, each with its own sample spacing — two minutes on the
/// hour windows, ten on the day, an hour on the week, a day beyond that, all
/// re-measured live for this port. So a requested range is served by the
/// shortest window that reaches back far enough and then clipped; asking for the
/// widest every time would trade the whole chart's resolution for a range the
/// caller did not want.
const LOOKBACKS: &[(&str, i64)] = &[
    ("_1H", 3_600),
    ("_3H", 10_800),
    ("_1D", 86_400),
    ("_7D", 604_800),
    ("_30D", 2_592_000),
    ("MAX", i64::MAX),
];

/// Price history for one leg, bucketed onto the terminal's candle grid.
///
/// The response carries a `note` because these are not bars off a matching
/// engine: they are the venue's probability samples bucketed, from whichever
/// fixed lookback window reached back far enough, clipped to what was asked for.
pub async fn get_candles(
    state: &AppState,
    id: &str,
    interval: CandleInterval,
    start_ts: i64,
    end_ts: i64,
) -> Result<CandlesResponse> {
    let market_id = resolve_leg(state, id).await?;
    let raw = raw_market(state, &market_id).await?;
    let slug = raw
        .category
        .as_ref()
        .and_then(|c| c.slug.as_deref().or(c.id.as_deref()))
        .unwrap_or_default()
        .to_string();
    let ticker = make_ticker(&slug, &market_id);

    let span = (OffsetDateTime::now_utc().unix_timestamp() - start_ts).max(1);
    let (token, _) = LOOKBACKS
        .iter()
        .find(|(_, seconds)| *seconds >= span)
        .copied()
        .unwrap_or(("MAX", i64::MAX));

    // History is published per category, one series per leg with no way to ask
    // for one — a 30-leg category is 160 KB to chart a single team — so the
    // whole set is cached under the category and every leg's chart shares one
    // read.
    let data: Arc<TimeseriesEnvelope> = gql(
        state,
        &format!("timeseries:{slug}:{token}"),
        ttl::CANDLES,
        TIMESERIES_QUERY,
        json!({ "id": slug, "interval": token }),
        Duration::from_secs(30),
    )
    .await?;

    let all = nodes(data.timeseries.as_ref());
    let Some(series) = all
        .iter()
        .find(|s| s.market.as_ref().and_then(|m| m.id.as_deref()) == Some(market_id.as_str()))
    else {
        return Err(UpstreamError::not_found(format!(
            "predict.fun publishes no price history for {ticker}"
        ))
        .with_hint(format!(
            "timeseries(categoryId: \"{slug}\") returned no series for leg {market_id}. History \
             appears once a leg has traded."
        )));
    };

    let points: Vec<&RawPfPoint> = nodes(series.data.as_ref())
        .into_iter()
        .filter(|p| {
            figure(p.x)
                .map(|t| t >= start_ts as f64 && t <= end_ts as f64)
                .unwrap_or(false)
        })
        .collect();

    Ok(CandlesResponse {
        venue: VENUE,
        ticker,
        series_ticker: series_from_slug(&slug),
        interval,
        candles: bucket_samples(&points, interval),
        // The disclosure is the response's own, in the same register Polymarket
        // International's carries: these are not OHLC bars off a matching engine
        // and the reader is entitled to know it before reading a high off one.
        note: Some(format!(
            "predict.fun publishes a probability sample series rather than OHLC: these bars are \
             its samples bucketed, and carry no volume or open interest. History is served only \
             as fixed lookback windows ending at now — this chart is its \"{token}\" window \
             clipped to the range requested, so it may start later, or be sampled more coarsely, \
             than asked for."
        )),
    })
}

/// The venue's tag taxonomy, which is what it has instead of series.
///
/// There is no series object here — recurring questions are related only by the
/// shape of their slugs, which [`series_from_slug`] already reads. What the
/// venue does publish is a tag tree with a live count per tag, and that is the
/// thing a category-filtered board is actually asking for.
pub async fn list_series(state: &AppState, category: Option<&str>) -> Result<Vec<SeriesInfo>> {
    let data: Arc<TagsEnvelope> = gql(
        state,
        "tags",
        ttl::CATALOGUE,
        TAGS_QUERY,
        json!({}),
        Duration::from_secs(25),
    )
    .await?;

    // Only tags with something open behind them: the tree keeps 99 tags and 39
    // of them have no live category, and offering those as filters means
    // offering a filter that returns nothing.
    let series: Vec<SeriesInfo> = nodes(data.category_tags.as_ref())
        .into_iter()
        .filter(|tag| tag.open.unwrap_or(0) > 0)
        .map(normalise_series)
        .collect();

    // A bare `?category=` reaches here as `Some("")`, and treating that as a
    // filter answers with nothing at all rather than with the whole tag tree.
    let Some(wanted) = category
        .map(|c| c.trim().to_lowercase())
        .filter(|want| !want.is_empty())
    else {
        return Ok(series);
    };
    Ok(series
        .into_iter()
        .filter(|s| s.category.to_lowercase() == wanted || s.title.to_lowercase() == wanted)
        .collect())
}

/* ----------------------------------------------------------------- corpus */

/// One key for the whole snapshot, so every panel that searches or ranks shares
/// a single crawl rather than each paying for its own.
const CORPUS_KEY: &str = "predictfun:corpus";

/// Pages of open categories, ordered by 24h turnover.
///
/// The page size is fixed at the 100 the API silently enforces — it clamps
/// anything larger without saying so, verified live at 101, 150 and 250 — and a
/// crawl that asked for 500 would quietly walk a fifth of the catalogue while
/// believing it had walked it all.
///
/// The open universe is about 1,139 categories and 9,000-odd legs, and each page
/// of 100 with every leg attached is ~780 KB and two to three seconds — so a
/// full crawl is twelve pages and about half a minute, once per catalogue TTL.
/// The cap sits just past the whole universe rather than under it, so the
/// ordinary case completes and a venue that doubles overnight truncates instead
/// of running for minutes.
///
/// Volume ordering is what makes the truncation principled: what a cap drops is
/// the untraded tail, not an arbitrary slice of the middle.
const CORPUS_MAX_PAGES: usize = 14;

/// One page of open categories, from the cursor the last page ended on.
async fn crawl_page(state: &AppState, after: Option<&str>) -> Result<Arc<CrawlEnvelope>> {
    gql(
        state,
        &format!("crawl:{}", after.unwrap_or("first")),
        ttl::CATALOGUE,
        &CRAWL_QUERY,
        json!({ "after": after }),
        Duration::from_secs(40),
    )
    .await
}

/// Walk the catalogue into one snapshot.
///
/// Categories are de-duplicated by slug as they arrive: the cursor walk is the
/// venue's, and a venue that repeats a row across two pages must not double the
/// universe every panel then searches.
async fn build_corpus(state: &AppState) -> Result<Corpus> {
    let mut events: Vec<VenueEvent> = Vec::new();
    let mut markets: Vec<Market> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut cursor: Option<String> = None;
    let mut truncated = false;

    for page in 0..CORPUS_MAX_PAGES {
        let envelope = crawl_page(state, cursor.as_deref()).await?;
        let connection = envelope.categories.as_ref();

        for raw in nodes(connection) {
            let key = raw
                .slug
                .as_deref()
                .or(raw.id.as_deref())
                .unwrap_or_default();
            if key.is_empty() || !seen.insert(key.to_string()) {
                continue;
            }
            let event = normalise_event(raw);
            markets.extend(event.markets.iter().cloned());
            events.push(event);
        }

        let info = connection.and_then(|c| c.page_info.as_ref());
        let next = info
            .filter(|info| info.has_next_page == Some(true))
            .and_then(|info| info.end_cursor.clone());
        let Some(next) = next else { break };
        cursor = Some(next);

        // `totalCount` is real on this connection but null on nearly every
        // other, so the flag is driven by where the walk stopped rather than by
        // counts.
        if page == CORPUS_MAX_PAGES - 1 {
            truncated = true;
        }
    }

    Ok(Corpus::new(VENUE, events, markets, truncated))
}

/// The open universe, crawled once per catalogue TTL and shared by pointer.
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
                "[predictfun] corpus warm"
            ),
            Err(err) => tracing::warn!(error = %err, "[predictfun] corpus warm failed"),
        }
    });
}

/// Search the snapshot rather than the venue's own `search` query.
///
/// That query exists and is respectable — it answers `fed` with the Fed rate
/// ladder — but it scores by its own rules and cannot be made to score by anyone
/// else's. Ranking the local snapshot the way every other venue is ranked is
/// what makes a cross-venue result list comparable instead of six lists stapled
/// together.
pub async fn search(state: &AppState, query: &str, limit: usize) -> Result<SearchResponse> {
    refuse_overlong_query(query)?;
    let snapshot = corpus_snapshot(state).await?;
    Ok(search_corpus(&snapshot, query, limit))
}

/// Leaderboards over the snapshot.
///
/// Turnover, resting depth and — under the name `sharesCount` — open interest
/// are all stated per leg in the catalogue, so those three boards rank on the
/// venue's own numbers. The movers do not: the only 24h figure here is a
/// magnitude with no direction (see `percentage_chance_change24h` on
/// [`RawPfMarketStatistics`]), [`Market::change`] is therefore unstated, and
/// [`rank_markets`] drops a market rather than ranking an unpublished figure as
/// zero. `TOP gainers` and `TOP losers` come back empty on this venue — which is
/// what the venue registry already declares — until the venue publishes a signed
/// move or the crawl can afford the per-category history that would reconstruct
/// one.
pub async fn top_markets(state: &AppState, sort: MoverSort, limit: usize) -> Result<Vec<Market>> {
    let snapshot = corpus_snapshot(state).await?;
    Ok(rank_markets(&snapshot.markets, sort, limit))
}

/* ------------------------------------------------------------------ tests */

#[cfg(test)]
mod tests {
    //! predict.fun normalisation tests.
    //!
    //! The fixtures are trimmed copies of real `graphql.predict.fun` responses,
    //! so the things that make this venue awkward are all present as they
    //! arrive: the outcome pair that is only sometimes called Yes/No, the 1e18
    //! scaled strings on the tape, the last print that names an outcome but is
    //! quoted in YES either way, the leg quoted to three places inside a
    //! category quoted to two, and the null bid and ask that a live market with
    //! real money behind it can carry.

    use super::*;
    use wiremock::matchers::{method, path as path_matcher, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use crate::config::Config;
    use crate::error::codes;

    /// `market(id: "927684")` under [`MARKET_QUERY`], captured whole. Leg 927684
    /// is the Spurs leg of `nba-2027-champion`: a live two-sided book quoted to
    /// three places, a last print that names the NO outcome, `sharesCount` on
    /// both sides, and the category inline with its three tags.
    const MARKET_JSON: &str = include_str!("fixtures/predictfun_market.json");

    /// `matchEventLog` for the same leg under [`TRADES_QUERY`], trimmed to three
    /// rows: two YES fills and one NO fill, which is the row that has to be
    /// re-based before it can be charted. `hasNextPage` is true, so the cursor
    /// path is exercised too.
    const TRADES_JSON: &str = include_str!("fixtures/predictfun_trades.json");

    /// One page of [`CRAWL_QUERY`], trimmed by hand to three categories and two
    /// legs each. Each category is here for a reason:
    ///
    /// * `nba-2027-champion` — the category the market fixture belongs to, whose
    ///   slug carries a season year in the middle rather than at the end.
    /// * `big-game-champion-2027` — the curated Super Bowl cross-venue link, and
    ///   the live example of legs quoted to three places inside a category
    ///   quoted to two.
    /// * `fed-decision-in-september-762` — the curated Fed link, and the slug
    ///   whose trailing disambiguator has to come off before the month does.
    ///
    /// `hasNextPage` is false, so the crawl completes in one page.
    const CRAWL_JSON: &str = include_str!("fixtures/predictfun_crawl.json");

    /// Unwrap a captured response the way [`gql`] does: these are whole
    /// GraphQL bodies as the endpoint sent them, `data` envelope and all.
    fn payload<T: DeserializeOwned>(captured: &str) -> T {
        let body: GraphQlBody<T> =
            serde_json::from_str(captured).expect("the captured response parses");
        assert!(body.errors.is_none(), "the capture is a successful answer");
        body.data.expect("the capture carries a data payload")
    }

    /// The YES outcome's `sharesCount`, read straight out of the captured file.
    fn captured_shares_count() -> Option<f64> {
        let raw: Value = serde_json::from_str(MARKET_JSON).expect("the capture parses");
        raw["data"]["market"]["outcomes"]["edges"][0]["node"]["statistics"]["sharesCount"].as_f64()
    }

    fn market_of(value: Value) -> RawPfMarket {
        serde_json::from_value(value).expect("the fixture parses as a market")
    }

    fn category_of(value: Value) -> RawPfCategory {
        serde_json::from_value(value).expect("the fixture parses as a category")
    }

    fn trades_of(value: Value) -> RawPfConnection<RawPfTrade> {
        serde_json::from_value(value).expect("the fixture parses as a tape")
    }

    /// Wrap rows the way every list on this API arrives.
    fn connection(nodes: Vec<Value>) -> Value {
        json!({ "edges": nodes.into_iter().map(|node| json!({ "node": node })).collect::<Vec<_>>() })
    }

    fn points(pairs: &[(i64, Option<f64>)]) -> Vec<RawPfPoint> {
        pairs
            .iter()
            .map(|(x, y)| RawPfPoint {
                x: Some(*x as f64),
                y: *y,
            })
            .collect()
    }

    fn bucket(pairs: &[(i64, Option<f64>)], interval: CandleInterval) -> Vec<Candle> {
        let owned = points(pairs);
        bucket_samples(&owned.iter().collect::<Vec<_>>(), interval)
    }

    /* -------------------------------------------------------------- coercion */

    #[test]
    fn figure_keeps_a_stated_zero_which_is_a_price_the_venue_quotes() {
        assert_eq!(figure(Some(0.0)), Some(0.0));
    }

    #[test]
    fn figure_reports_an_unpublished_figure_as_absent_rather_than_as_zero() {
        assert_eq!(figure(None), None);
        assert_eq!(figure(Some(f64::NAN)), None);
        assert_eq!(figure(Some(f64::INFINITY)), None);
    }

    #[test]
    fn decimal_reads_the_last_print_which_the_venue_sends_as_text() {
        assert_eq!(decimal(Some(&json!("0.14"))), Some(0.14));
        assert_eq!(decimal(Some(&json!(0.14))), Some(0.14));
    }

    #[test]
    fn decimal_returns_nothing_for_an_absent_or_unreadable_price() {
        assert_eq!(decimal(None), None);
        assert_eq!(decimal(Some(&Value::Null)), None);
        assert_eq!(decimal(Some(&json!(""))), None);
        assert_eq!(decimal(Some(&json!("   "))), None);
        assert_eq!(decimal(Some(&json!("n/a"))), None);
    }

    #[test]
    fn decimal_keeps_a_zero_written_as_text_which_the_venue_does_send() {
        // JavaScript's `Number("")` is 0, so the empty cases above are the ones
        // that must not reach the parse. A written "0.00" must.
        assert_eq!(decimal(Some(&json!("0.00"))), Some(0.0));
        assert_eq!(decimal(Some(&json!("0"))), Some(0.0));
    }

    #[test]
    fn from_wei_unscales_the_tape_where_size_and_price_are_1e18_strings() {
        assert_eq!(
            from_wei(Some(&json!("207230000000000000000"))),
            Some(207.23)
        );
        assert_eq!(from_wei(Some(&json!("70000000000000000"))), Some(0.07));
        assert_eq!(from_wei(Some(&json!("951000000000000000"))), Some(0.951));
        // 0.01 shares — the smallest fill seen on the live tape.
        assert_eq!(from_wei(Some(&json!("10000000000000000"))), Some(0.01));
    }

    #[test]
    fn from_wei_refuses_a_shape_it_does_not_understand_rather_than_taping_a_free_trade() {
        assert_eq!(from_wei(Some(&json!("0.07"))), None);
        assert_eq!(from_wei(None), None);
        assert_eq!(from_wei(Some(&Value::Null)), None);
        assert_eq!(from_wei(Some(&json!(""))), None);
        assert_eq!(from_wei(Some(&json!("n/a"))), None);
    }

    #[test]
    fn from_wei_keeps_a_scaled_zero_apart_from_an_absent_one() {
        assert_eq!(from_wei(Some(&json!("0"))), Some(0.0));
    }

    #[test]
    fn complement_mirrors_yes_to_no_at_the_legs_own_precision() {
        // Leg 26952 is quoted to three places: ask 0.14 → NO bid 0.86.
        assert_eq!(complement(0.14, Some(3)), 0.86);
        assert_eq!(complement(0.139, Some(3)), 0.861);
    }

    #[test]
    fn complement_rounds_at_the_stated_precision_rather_than_at_the_categorys() {
        // Leg 26952's category `big-game-champion-2027` is quoted to two places
        // and the leg to three — live today — so using the category's would move
        // the NO ladder a tenth of a cent away from the exchange's own.
        assert_eq!(complement(0.139, Some(2)), 0.86);
        assert_eq!(complement(0.139, Some(3)), 0.861);
    }

    #[test]
    fn complement_falls_back_to_two_places_when_the_venue_states_no_precision() {
        assert_eq!(complement(0.25, None), 0.75);
        // Out of range is not a precision either; 0 places would round a book to
        // whole dollars.
        assert_eq!(complement(0.25, Some(0)), 0.75);
        assert_eq!(complement(0.25, Some(9)), 0.75);
    }

    /* ------------------------------------------------------------ identifiers */

    #[test]
    fn make_ticker_round_trips_a_slug_that_itself_ends_in_a_hyphenated_number() {
        let ticker = make_ticker("btc-updown-15m-1787032800", "1436748");
        assert_eq!(ticker, "btc-updown-15m-1787032800~1436748");
        assert_eq!(
            parse_ticker(&ticker),
            Some(("btc-updown-15m-1787032800", "1436748"))
        );
    }

    #[test]
    fn make_ticker_names_a_leg_by_its_id_alone_when_no_category_is_known() {
        assert_eq!(make_ticker("", "26952"), "26952");
    }

    #[test]
    fn parse_ticker_splits_on_the_last_separator_so_nothing_can_shadow_it() {
        assert_eq!(
            parse_ticker("big-game-champion-2027~26952"),
            Some(("big-game-champion-2027", "26952"))
        );
    }

    #[test]
    fn parse_ticker_reports_a_bare_identifier_as_unsplittable_rather_than_guessing() {
        assert_eq!(parse_ticker("26952"), None);
        assert_eq!(parse_ticker("big-game-champion-2027"), None);
    }

    #[test]
    fn series_strips_the_unix_stamp_the_machine_made_crypto_series_regenerates() {
        assert_eq!(
            series_from_slug("btc-updown-15m-1787032800"),
            "btc-updown-15m"
        );
        assert_eq!(
            series_from_slug("btc-updown-15m-1787033700"),
            "btc-updown-15m"
        );
        assert_eq!(
            series_from_slug("eth-updown-5m-1787033400"),
            "eth-updown-5m"
        );
    }

    #[test]
    fn series_strips_the_fixture_date_so_a_leagues_games_group_together() {
        assert_eq!(series_from_slug("mlb-oak-hou-2026-08-23"), "mlb-oak-hou");
        assert_eq!(series_from_slug("lol-t1-dnf-2026-08-17"), "lol-t1-dnf");
    }

    #[test]
    fn series_strips_a_season_year_so_consecutive_seasons_are_one_question() {
        assert_eq!(
            series_from_slug("big-game-champion-2027"),
            "big-game-champion"
        );
        assert_eq!(series_from_slug("nba-2027-champion"), "nba-champion");
        assert_eq!(
            series_from_slug("what-will-fed-rate-hit-before-2027"),
            "what-will-fed-rate-hit-before"
        );
    }

    #[test]
    fn series_strips_a_date_spelled_in_words_which_is_where_the_fed_books_live() {
        // The whole reason this is not left alone, and both slugs are live today:
        // September's and October's FOMC books are one question asked twice, and
        // keeping the month makes them two one-event series that pair with
        // nothing at Kalshi. `fed-decision-in` is the ticker the curated
        // cross-venue link names.
        assert_eq!(
            series_from_slug("fed-decision-in-september-762"),
            "fed-decision-in"
        );
        assert_eq!(
            series_from_slug("fed-decision-in-october-20260617190323537"),
            "fed-decision-in"
        );
        assert_eq!(
            series_from_slug("bitcoin-up-or-down-on-august-18-2026"),
            "bitcoin-up-or-down-on"
        );
    }

    #[test]
    fn series_takes_the_day_off_a_named_month_but_leaves_a_district_number_alone() {
        // The day is only claimed next to a month name. Nothing anchors the `15`
        // in a district slug, so all 38 Texas districts stay 38 series.
        assert_eq!(
            series_from_slug("ethereum-up-or-down-august-18-2026-2am-et"),
            "ethereum-up-or-down-2am-et"
        );
        assert_eq!(series_from_slug("ushr-tx-15"), "ushr-tx-15");
    }

    #[test]
    fn series_strips_the_creation_stamp_the_season_books_carry_which_is_not_a_year() {
        // Some categories are minted with a seventeen-digit creation time. Left
        // on, every league's title race is its own one-event series and pairs
        // with nothing at another broker.
        assert_eq!(
            series_from_slug("laliga-2027-champion-20260701200737375"),
            "laliga-champion"
        );
    }

    #[test]
    fn series_leaves_the_second_half_of_a_split_season_on_and_that_is_the_known_limit() {
        // `2026-27` is a season, not a date, and the shared stripper takes
        // numbers only in whole date groups — the rule that keeps all 38 Texas
        // districts apart. So the `27` survives and the book re-keys at each
        // season rollover. Within a season it is stable, which is what `XV`
        // needs; loosening the rule to catch it would cost far more than it
        // saves.
        assert_eq!(
            series_from_slug("nhl-2026-27-calder-trophy-20260625172243021"),
            "nhl-27-calder-trophy"
        );
    }

    #[test]
    fn series_keeps_a_number_in_the_middle_of_a_slug_which_is_part_of_the_question() {
        // The short-run rule fires only at the end, where this venue puts its
        // disambiguator. Anywhere else the digits are the market.
        assert_eq!(series_from_slug("nasdaq-100-above"), "nasdaq-100-above");
        assert_eq!(
            series_from_slug("btc-updown-15m-1787032800"),
            "btc-updown-15m"
        );
    }

    #[test]
    fn series_keeps_a_slug_that_is_nothing_but_an_occasion() {
        // Stripping everything would name every such category the same series,
        // which is worse than naming it after its own slug.
        assert_eq!(series_from_slug("2026-11-03"), "2026-11-03");
    }

    #[test]
    fn series_produces_exactly_the_tickers_the_curated_cross_venue_links_name() {
        // `crossvenue` hard-codes these three as this venue's leg of the Fed,
        // Senate and Super Bowl links, because no wording connects the listings:
        // predict.fun cannot say "Super Bowl" (the phrase is trademarked, so it
        // lists the field as "NFL Champion 2027"), and the Fed books are two
        // slugs for one question. All three slugs are live today, and if this
        // rule stops producing these three strings the links go dark silently.
        assert_eq!(
            series_from_slug("fed-decision-in-september-762"),
            "fed-decision-in"
        );
        assert_eq!(
            series_from_slug("fed-decision-in-october-20260617190323537"),
            "fed-decision-in"
        );
        assert_eq!(
            series_from_slug("which-party-will-win-the-senate-in-2026"),
            "which-party-will-win-the-senate-in"
        );
        assert_eq!(
            series_from_slug("big-game-champion-2027"),
            "big-game-champion"
        );
    }

    /* ---------------------------------------------------------------- status */

    #[test]
    fn status_reads_registered_as_live_because_that_is_the_ordinary_trading_state() {
        assert_eq!(
            status_of(&market_of(
                json!({ "status": "REGISTERED", "isTradingEnabled": true })
            )),
            "open"
        );
        assert_eq!(
            status_of(&market_of(json!({ "status": "UNPAUSED" }))),
            "open"
        );
    }

    #[test]
    fn status_honours_the_venues_halt_switch_over_the_registration_state() {
        assert_eq!(
            status_of(&market_of(
                json!({ "status": "REGISTERED", "isTradingEnabled": false })
            )),
            "closed"
        );
        assert_eq!(
            status_of(&market_of(json!({ "status": "PAUSED" }))),
            "closed"
        );
    }

    #[test]
    fn status_separates_a_decided_market_from_a_settled_one() {
        assert_eq!(
            status_of(&market_of(json!({ "status": "PRICE_PROPOSED" }))),
            "determined"
        );
        assert_eq!(
            status_of(&market_of(json!({ "status": "PRICE_DISPUTED" }))),
            "determined"
        );
        assert_eq!(
            status_of(&market_of(json!({ "status": "RESOLVED" }))),
            "settled"
        );
    }

    #[test]
    fn status_reads_the_pre_open_states_as_unopened() {
        assert_eq!(
            status_of(&market_of(json!({ "status": "INITIALIZING" }))),
            "unopened"
        );
        assert_eq!(
            status_of(&market_of(json!({ "status": "CREATING" }))),
            "unopened"
        );
    }

    #[test]
    fn status_passes_a_word_nobody_has_seen_through_in_the_venues_own_spelling() {
        // A state that is neither ours nor one of the venue's known ones is worth
        // reading literally rather than folded into whichever of ours looks
        // closest.
        assert_eq!(
            status_of(&market_of(json!({ "status": "MOTHBALLED" }))),
            "mothballed"
        );
        assert_eq!(status_of(&market_of(json!({}))), "open");
    }

    /* --------------------------------------------------------------- markets */

    /// Leg 26952 of the NFL champion category, verbatim but trimmed. Note
    /// `chancePercentage: 14.0` against a book of 0.139/0.14 — a mid of 0.1395.
    fn rams() -> Value {
        json!({
            "id": "26952",
            "title": "Los Angeles Rams",
            "question": "Will the Los Angeles Rams win the 2027 NFL league championship?",
            "description": "This market will resolve according to the team that wins the 2027 NFL league championship.",
            "status": "REGISTERED",
            "marketType": null,
            "isTradingEnabled": true,
            "decimalPrecision": 3,
            "chancePercentage": 14.0,
            "resolution": null,
            "statistics": {
                "totalLiquidityUsd": 107890435.51,
                "liquidity3CAskUsd": 5574.42,
                "volumeTotalUsd": 5896.0,
                "volume24hUsd": 67.38,
                "volume24hChangeUsd": 213.38,
                "percentageChanceChange24h": 1.0
            },
            "orderbook": {
                "marketId": 26952,
                "asks": [[0.14, 1838.8899999999999], [0.141, 6131.34], [0.142, 2439.4965]],
                "bids": [[0.139, 6302.299999999999], [0.138, 1782.75], [0.137, 7412.0]],
                "lastOrderSettled": { "price": "0.14", "kind": "LIMIT", "side": "Ask", "outcome": "Yes" }
            },
            "outcomes": connection(vec![
                json!({
                    "id": "51626", "index": 1, "name": "Yes", "status": null,
                    "chancePercentage": 13.9,
                    "bidPriceInCurrency": 0.139, "askPriceInCurrency": 0.14,
                    "statistics": { "sharesCount": 5425.598222287107, "positionsValueUsd": 754.1581528979079 }
                }),
                json!({
                    "id": "51627", "index": 2, "name": "No", "status": null,
                    "chancePercentage": 86.0,
                    "bidPriceInCurrency": 0.86, "askPriceInCurrency": 0.861,
                    "statistics": { "sharesCount": 5425.611265690695, "positionsValueUsd": 4666.025688493998 }
                }),
            ])
        })
    }

    fn nfl_champion() -> Value {
        json!({
            "id": "big-game-champion-2027",
            "slug": "big-game-champion-2027",
            "title": "NFL Champion 2027",
            "status": "OPEN",
            "marketVariant": "DEFAULT",
            "isNegRisk": true,
            "decimalPrecision": 2,
            "startsAt": "2026-06-01T22:30:00.000Z",
            "endsAt": "2027-02-14T23:55:00.000Z",
            "statistics": {
                "liquidityValueUsd": 1460060205.98,
                "liquidity3CAskUsd": 67651.43,
                "volumeTotalUsd": 1455756.12,
                "volume24hUsd": 157362.69
            },
            "tags": connection(vec![
                json!({ "id": "4", "name": "Sports" }),
                json!({ "id": "45", "name": "NFL" }),
            ])
        })
    }

    /// The Rams leg with one field replaced, which is how the traps are posed.
    fn rams_with(edits: &[(&str, Value)]) -> RawPfMarket {
        let mut raw = rams();
        for (key, value) in edits {
            raw[*key] = value.clone();
        }
        market_of(raw)
    }

    fn rams_market() -> Market {
        normalise_market(&market_of(rams()), Some(&category_of(nfl_champion())))
    }

    #[test]
    fn market_names_the_leg_by_its_category_and_id_both_of_which_round_trip() {
        let market = rams_market();
        assert_eq!(market.venue, Venue::PredictFun);
        assert_eq!(market.ticker, "big-game-champion-2027~26952");
        assert_eq!(market.event_ticker, "big-game-champion-2027");
        assert_eq!(market.series_ticker, "big-game-champion");
    }

    #[test]
    fn market_quotes_both_sides_from_the_outcome_pair() {
        let market = rams_market();
        assert_eq!(market.yes_bid, Some(0.139));
        assert_eq!(market.yes_ask, Some(0.14));
        assert_eq!(market.no_bid, Some(0.86));
        assert_eq!(market.no_ask, Some(0.861));
    }

    #[test]
    fn market_computes_the_mid_from_the_book_instead_of_trusting_chance_percentage() {
        // The venue reports a chance of 14.0 on a 0.139/0.14 book. It agreed with
        // the mid on 5.7% of live legs and is not a midpoint at all.
        assert_eq!(rams_market().mid, Some(0.1395));
    }

    #[test]
    fn market_does_not_take_chance_percentage_even_when_it_disagrees_wildly() {
        // Leg 1459048, live: bid 0.10, ask 0.85, chancePercentage 70.
        let wide = normalise_market(
            &rams_with(&[
                ("chancePercentage", json!(70.0)),
                (
                    "outcomes",
                    connection(vec![
                        json!({ "index": 1, "name": "Yes", "bidPriceInCurrency": 0.1, "askPriceInCurrency": 0.85 }),
                        json!({ "index": 2, "name": "No", "bidPriceInCurrency": 0.15, "askPriceInCurrency": 0.9 }),
                    ]),
                ),
            ]),
            None,
        );
        assert_eq!(wide.mid, Some(0.475));
    }

    #[test]
    fn market_derives_the_no_side_from_the_yes_book_when_only_one_side_is_stated() {
        let one_sided = normalise_market(
            &rams_with(&[(
                "outcomes",
                connection(vec![json!({
                    "index": 1, "name": "Yes",
                    "bidPriceInCurrency": 0.139, "askPriceInCurrency": 0.14
                })]),
            )]),
            None,
        );
        assert_eq!(one_sided.no_bid, Some(0.86));
        assert_eq!(one_sided.no_ask, Some(0.861));
    }

    #[test]
    fn market_reports_an_unquoted_leg_as_absent_not_as_a_zero_bid() {
        // A leg with six figures of lifetime turnover, REGISTERED inside an OPEN
        // category, can carry no bid or ask at all — 266 of 1,926 live outcomes
        // do.
        let unquoted = normalise_market(
            &rams_with(&[
                ("orderbook", Value::Null),
                (
                    "outcomes",
                    connection(vec![
                        json!({ "index": 1, "name": "Yes", "bidPriceInCurrency": null, "askPriceInCurrency": null }),
                        json!({ "index": 2, "name": "No", "bidPriceInCurrency": null, "askPriceInCurrency": null }),
                    ]),
                ),
            ]),
            Some(&category_of(nfl_champion())),
        );
        assert_eq!(unquoted.yes_bid, None);
        assert_eq!(unquoted.yes_ask, None);
        assert_eq!(unquoted.no_bid, None);
        assert_eq!(unquoted.no_ask, None);
        assert_eq!(unquoted.mid, None);
    }

    #[test]
    fn market_reads_open_interest_off_shares_count_which_is_what_the_venue_calls_it() {
        // The YES and NO counts agree to five figures — minted pairs, not
        // turnover.
        assert_eq!(rams_market().open_interest, Some(5425.598222287107));
    }

    #[test]
    fn market_carries_turnover_as_the_dollars_the_venue_meters_and_the_depth_it_trusts() {
        let market = rams_market();
        assert_eq!(market.volume, Some(5896.0));
        assert_eq!(market.volume24h, Some(67.38));
        // liquidity3CAskUsd, not totalLiquidityUsd — the latter reads
        // 107,890,435 on this leg, whose whole book is about $5.5k.
        assert_eq!(market.liquidity, Some(5574.42));
    }

    #[test]
    fn market_states_no_24h_move_because_the_venue_publishes_a_magnitude() {
        // `percentageChanceChange24h` reads like the daily move and is the daily
        // range: 0 negative values against 293 positive ones across 963 live
        // legs. Signing it would paint a crash green, and nothing else in the
        // schema states an earlier price.
        let market = rams_market();
        assert_eq!(market.change, None);
        assert_eq!(market.previous_price, None);

        let wild = normalise_market(
            &rams_with(&[(
                "statistics",
                json!({ "volumeTotalUsd": 5896.0, "percentageChanceChange24h": 77.0 }),
            )]),
            None,
        );
        assert_eq!(wild.change, None);

        // A zero range is a real statement about the day, but it is still a range
        // and not a move, so it is not reported as a flat one either.
        let flat = normalise_market(
            &rams_with(&[(
                "statistics",
                json!({ "volumeTotalUsd": 5896.0, "percentageChanceChange24h": 0.0 }),
            )]),
            None,
        );
        assert_eq!(flat.change, None);
        assert_eq!(flat.previous_price, None);
    }

    #[test]
    fn market_leaves_the_figures_it_is_not_given_unstated_rather_than_zero() {
        let quiet = normalise_market(
            &rams_with(&[("statistics", json!({ "volumeTotalUsd": 5896.0 }))]),
            None,
        );
        assert_eq!(quiet.volume, Some(5896.0));
        assert_eq!(quiet.volume24h, None);
        assert_eq!(quiet.liquidity, None);

        // A stated zero is the venue's own answer and survives as one: a leg
        // really can rest $0 within three cents of its ask.
        let dead = normalise_market(
            &rams_with(&[(
                "statistics",
                json!({ "volumeTotalUsd": 119464.85, "volume24hUsd": 0.0, "liquidity3CAskUsd": 0.0 }),
            )]),
            None,
        );
        assert_eq!(dead.volume24h, Some(0.0));
        assert_eq!(dead.liquidity, Some(0.0));
    }

    #[test]
    fn market_takes_the_last_print_as_quoted_because_it_is_already_in_yes_terms() {
        assert_eq!(rams_market().last_price, Some(0.14));

        // Leg 1457355 is quoted 0.01 on YES and reports its last settled order as
        // {price: "0.01", outcome: "No"}. `outcome` names the side the resting
        // order sat on, not the space the price is in — inverting it would print
        // 99¢ on a penny market. 110 of 137 live NO-named prints agree.
        let no_side = normalise_market(
            &rams_with(&[
                ("decimalPrecision", json!(2)),
                (
                    "orderbook",
                    json!({
                        "asks": [[0.01, 5000]],
                        "bids": [],
                        "lastOrderSettled": { "price": "0.01", "side": "Bid", "outcome": "No" }
                    }),
                ),
                (
                    "outcomes",
                    connection(vec![
                        json!({ "index": 1, "name": "Up", "bidPriceInCurrency": null, "askPriceInCurrency": 0.01 }),
                        json!({ "index": 2, "name": "Down", "bidPriceInCurrency": 0.99, "askPriceInCurrency": null }),
                    ]),
                ),
            ]),
            None,
        );
        assert_eq!(no_side.last_price, Some(0.01));
        // One-sided book, so the mid falls back to the print rather than
        // inventing a midpoint out of the single quote that exists.
        assert_eq!(no_side.mid, Some(0.01));
    }

    #[test]
    fn market_keeps_a_last_print_of_zero_which_is_the_venue_rounding_to_the_cent() {
        // Every print comes back at two decimals — 340 of 340 sampled — including
        // on legs quoted to three, so a leg that last traded at 0.003 reports
        // 0.00. That is a stated price, not a missing one, and the exact fill is
        // on the tape.
        let sub_cent = normalise_market(
            &rams_with(&[
                (
                    "orderbook",
                    json!({
                        "asks": [[0.003, 447.14]],
                        "bids": [],
                        "lastOrderSettled": { "price": "0.00", "outcome": "Yes" }
                    }),
                ),
                (
                    "outcomes",
                    connection(vec![
                        json!({ "index": 1, "name": "Yes", "bidPriceInCurrency": null, "askPriceInCurrency": 0.003 }),
                        json!({ "index": 2, "name": "No", "bidPriceInCurrency": 0.997, "askPriceInCurrency": null }),
                    ]),
                ),
            ]),
            None,
        );
        assert_eq!(sub_cent.last_price, Some(0.0));
        assert_eq!(sub_cent.yes_ask, Some(0.003));
    }

    #[test]
    fn market_leaves_the_last_print_unstated_on_a_leg_that_has_never_traded() {
        let never = normalise_market(
            &rams_with(&[(
                "orderbook",
                json!({ "asks": [], "bids": [], "lastOrderSettled": null }),
            )]),
            None,
        );
        assert_eq!(never.last_price, None);
    }

    #[test]
    fn market_labels_the_legs_from_the_leg_title_when_the_outcomes_are_a_plain_pair() {
        let market = rams_market();
        assert_eq!(
            market.title,
            "Will the Los Angeles Rams win the 2027 NFL league championship?"
        );
        assert_eq!(market.yes_sub_title, "Los Angeles Rams");
        assert_eq!(market.no_sub_title, "No");
    }

    #[test]
    fn market_labels_them_from_the_outcomes_when_the_outcomes_are_the_question() {
        // Every leg of an esports match is titled "Match Winner"; only the
        // outcome names say which team a row is about.
        let esports = normalise_market(
            &rams_with(&[
                ("title", json!("Match Winner")),
                (
                    "outcomes",
                    connection(vec![
                        json!({ "index": 1, "name": "T1", "bidPriceInCurrency": 0.6, "askPriceInCurrency": 0.62 }),
                        json!({ "index": 2, "name": "DNS", "bidPriceInCurrency": 0.38, "askPriceInCurrency": 0.4 }),
                    ]),
                ),
            ]),
            None,
        );
        assert_eq!(esports.yes_sub_title, "T1");
        assert_eq!(esports.no_sub_title, "DNS");

        let crypto = normalise_market(
            &rams_with(&[
                ("title", json!("Bitcoin Up or Down on August 18?")),
                (
                    "outcomes",
                    connection(vec![
                        json!({ "index": 1, "name": "Up" }),
                        json!({ "index": 2, "name": "Down" }),
                    ]),
                ),
            ]),
            None,
        );
        assert_eq!(crypto.yes_sub_title, "Up");
        assert_eq!(crypto.no_sub_title, "Down");
    }

    #[test]
    fn market_takes_the_legs_own_status_since_a_settled_leg_can_sit_in_an_open_category() {
        let market = rams_market();
        assert_eq!(market.status, "open");
        assert_eq!(market.result, "");

        let resolved = normalise_market(
            &rams_with(&[
                ("status", json!("RESOLVED")),
                (
                    "resolution",
                    json!({ "index": 2, "name": "DNS", "status": "WON" }),
                ),
            ]),
            Some(&category_of(nfl_champion())),
        );
        assert_eq!(resolved.status, "settled");
        assert_eq!(resolved.result, "no");

        let won = normalise_market(
            &rams_with(&[(
                "resolution",
                json!({ "index": 1, "name": "T1", "status": "WON" }),
            )]),
            None,
        );
        assert_eq!(won.result, "yes");
    }

    #[test]
    fn market_takes_its_dates_its_type_and_its_bucket_from_the_category() {
        let market = rams_market();
        assert_eq!(market.open_time, "2026-06-01T22:30:00.000Z");
        assert_eq!(market.close_time, "2027-02-14T23:55:00.000Z");
        assert_eq!(market.expiration_time, "2027-02-14T23:55:00.000Z");
        // The leg states no type of its own; the category's variant is the
        // fallback.
        assert_eq!(market.market_type, "DEFAULT");
        assert_eq!(market.category.as_deref(), Some("Sports"));
    }

    #[test]
    fn market_states_no_strike_because_the_venue_only_writes_one_into_a_title() {
        let ladder = normalise_market(&rams_with(&[("title", json!(">\u{a5}42B"))]), None);
        assert_eq!(ladder.strike_type, None);
        assert_eq!(ladder.floor_strike, None);
        assert_eq!(ladder.cap_strike, None);
    }

    #[test]
    fn market_falls_back_to_the_categorys_terms_for_a_leg_read_out_of_the_catalogue() {
        // The crawl does not ask for the leg's own rules — they are two kilobytes
        // apiece and only `DES` shows them — so a catalogue leg carries the terms
        // its category states for the group.
        assert!(rams_market()
            .rules_primary
            .starts_with("This market will resolve"));

        let mut leg = rams();
        leg["description"] = Value::Null;
        let mut parent = nfl_champion();
        parent["description"] =
            json!("Resolves to the team that wins the 2027 NFL league championship.");

        let catalogued = normalise_market(&market_of(leg), Some(&category_of(parent)));
        assert_eq!(
            catalogued.rules_primary,
            "Resolves to the team that wins the 2027 NFL league championship."
        );
    }

    #[test]
    fn market_reads_the_category_off_the_leg_when_it_is_not_passed_one() {
        let market = normalise_market(&rams_with(&[("category", nfl_champion())]), None);
        assert_eq!(market.event_ticker, "big-game-champion-2027");
        assert_eq!(market.series_ticker, "big-game-champion");
    }

    /* ----------------------------------------------------------------- events */

    fn nfl_event() -> VenueEvent {
        let mut category = nfl_champion();
        category["markets"] = connection(vec![rams()]);
        normalise_event(&category_of(category))
    }

    #[test]
    fn event_groups_the_legs_under_the_category_and_stamps_its_tickers_on_them() {
        let event = nfl_event();
        assert_eq!(event.venue, Venue::PredictFun);
        assert_eq!(event.event_ticker, "big-game-champion-2027");
        assert_eq!(event.series_ticker, "big-game-champion");
        assert_eq!(event.title, "NFL Champion 2027");
        assert_eq!(event.category, "Sports");
        assert_eq!(event.markets.len(), 1);
        assert_eq!(event.markets[0].event_ticker, "big-game-champion-2027");
        assert_eq!(event.markets[0].ticker, "big-game-champion-2027~26952");
    }

    #[test]
    fn event_claims_exclusivity_only_where_the_venue_states_it() {
        // The site offers NO-to-opposing-YES conversion on this one and says so.
        assert!(nfl_event().mutually_exclusive);

        let mut stated_false = nfl_champion();
        stated_false["isNegRisk"] = json!(false);
        assert!(!normalise_event(&category_of(stated_false)).mutually_exclusive);

        // An unstated flag is not a quiet yes: a wrong one puts a false arbitrage
        // on screen when `EVT` sums the legs.
        let mut unstated = nfl_champion();
        unstated["isNegRisk"] = Value::Null;
        assert!(!normalise_event(&category_of(unstated)).mutually_exclusive);
    }

    #[test]
    fn event_groups_a_fixture_under_the_series_its_slug_names() {
        let fixture = normalise_event(&category_of(json!({
            "id": "lol-t1-dnf-2026-08-17",
            "slug": "lol-t1-dnf-2026-08-17",
            "title": "LoL: T1 vs DN SOOPers (BO5) - KeSPA Cup Playoffs",
            "status": "RESOLVED",
            "isNegRisk": false,
            "markets": connection(vec![json!({
                "id": "1370660",
                "title": "Match Winner",
                "status": "RESOLVED",
                "chancePercentage": 1.0,
                "resolution": { "index": 2, "name": "DNS", "status": "WON" },
                "outcomes": connection(vec![
                    json!({ "index": 1, "name": "T1", "status": "LOST", "bidPriceInCurrency": null, "askPriceInCurrency": null }),
                    json!({ "index": 2, "name": "DNS", "status": "WON", "bidPriceInCurrency": null, "askPriceInCurrency": null }),
                ])
            })])
        })));

        assert_eq!(fixture.series_ticker, "lol-t1-dnf");
        assert_eq!(fixture.markets[0].status, "settled");
        assert_eq!(fixture.markets[0].result, "no");
        assert_eq!(fixture.markets[0].yes_sub_title, "T1");
        // A settled leg publishes no prices at all, and none are invented for it.
        assert_eq!(fixture.markets[0].mid, None);
        assert_eq!(fixture.markets[0].open_interest, None);
    }

    /* ------------------------------------------------------------------- book */

    #[test]
    fn book_ladders_the_yes_side_and_mirrors_the_no_side_at_the_legs_precision() {
        let book = normalise_order_book(&market_of(rams()), "big-game-champion-2027~26952", 12);
        assert_eq!(book.venue, Venue::PredictFun);
        assert_eq!(
            book.yes.iter().map(|l| l.price).collect::<Vec<_>>(),
            vec![0.139, 0.138, 0.137]
        );
        assert_eq!(
            book.yes_asks.iter().map(|l| l.price).collect::<Vec<_>>(),
            vec![0.14, 0.141, 0.142]
        );
        // 1 - 0.14 at three places, the exchange's own complement.
        assert_eq!(
            book.no.iter().map(|l| l.price).collect::<Vec<_>>(),
            vec![0.86, 0.859, 0.858]
        );
        assert_eq!(book.no[0].size, 1838.8899999999999);
    }

    #[test]
    fn book_reports_the_top_of_book_and_the_spread_it_implies() {
        let book = normalise_order_book(&market_of(rams()), "x", 12);
        assert_eq!(book.best_yes_bid, Some(0.139));
        assert_eq!(book.best_yes_ask, Some(0.14));
        assert_eq!(book.spread, Some(0.001));
        assert_eq!(book.mid, Some(0.1395));
    }

    #[test]
    fn book_applies_depth_itself_because_the_venue_returns_the_whole_book() {
        let book = normalise_order_book(&market_of(rams()), "x", 2);
        assert_eq!(book.yes.len(), 2);
        assert_eq!(book.yes_asks.len(), 2);
        assert_eq!(book.no.len(), 2);
    }

    #[test]
    fn book_sorts_before_slicing_so_depth_can_never_cut_off_the_best_price() {
        let scrambled = normalise_order_book(
            &rams_with(&[(
                "orderbook",
                json!({ "bids": [[0.12, 100], [0.139, 50]], "asks": [[0.2, 100], [0.14, 50]] }),
            )]),
            "x",
            1,
        );
        assert_eq!(scrambled.best_yes_bid, Some(0.139));
        assert_eq!(scrambled.best_yes_ask, Some(0.14));
    }

    #[test]
    fn book_reports_an_empty_side_as_absent_not_as_a_zero_quote() {
        let book = normalise_order_book(
            &rams_with(&[("orderbook", json!({ "asks": [], "bids": [] }))]),
            "x",
            12,
        );
        assert!(book.yes.is_empty());
        assert!(book.no.is_empty());
        assert_eq!(book.best_yes_bid, None);
        assert_eq!(book.best_yes_ask, None);
        assert_eq!(book.spread, None);
        assert_eq!(book.mid, None);
    }

    #[test]
    fn book_drops_rows_that_price_or_size_nothing() {
        let book = normalise_order_book(
            &rams_with(&[(
                "orderbook",
                json!({ "bids": [[0, 100], [0.5, 0], [0.4, 10], [0.3], "nonsense"] }),
            )]),
            "x",
            12,
        );
        assert_eq!(book.yes.len(), 1);
        assert_eq!(book.yes[0].price, 0.4);
    }

    /* ------------------------------------------------------------------- tape */

    /// Two real rows: a YES fill on leg 26936 and a NO fill on leg 26939.
    fn tape() -> Value {
        json!({ "edges": [
            {
                "cursor": "eyJvcmRlcklkIjoyMjgxNjM3NzA2LCJjcmVhdGVkQXQiOiIyMDI2LTA4LTE4VDA1OjM0OjQ5LjAwMFoifQ==",
                "node": {
                    "transactionHash": "0x4d20a10735cc659e19c7121ef9a4fa550192c5792bb187b78b01908f2a66844d",
                    "amountFilled": "207230000000000000000",
                    "priceExecuted": "70000000000000000",
                    "quoteType": "ASK",
                    "timestamp": "2026-08-18T05:34:49.000Z",
                    "market": { "id": "26936", "title": "Buffalo Bills", "category": { "slug": "big-game-champion-2027" } },
                    "outcome": { "index": 1, "name": "Yes" },
                    "account": { "address": "0xB9b438572B3707a54A3EBCa1132482B0B4780408", "name": "laozhang" }
                }
            },
            {
                "cursor": "eyJvcmRlcklkIjoyMjgzMzE3OTg5fQ==",
                "node": {
                    "transactionHash": "0x5a2e0af0bed02c6a71cc9ce179065cdda9680249ab3e6562a6deb8d42071eb4b",
                    "amountFilled": "10000000000000000",
                    "priceExecuted": "951000000000000000",
                    "quoteType": "ASK",
                    "timestamp": "2026-08-18T06:51:18.000Z",
                    "market": { "id": "26939", "title": "Dallas Cowboys", "category": { "slug": "big-game-champion-2027" } },
                    "outcome": { "index": 2, "name": "No" },
                    "account": { "address": "0xD542F3b61f2dB5ee5bEF170EAbB39c6C6c8C1163", "name": "ioup" }
                }
            }
        ]})
    }

    fn taped() -> Vec<Trade> {
        normalise_trades(Some(&trades_of(tape())), "")
    }

    #[test]
    fn trades_unscale_size_and_price_out_of_their_1e18_strings() {
        let trades = taped();
        assert_eq!(trades[0].count, 207.23);
        assert_eq!(trades[0].yes_price, 0.07);
        assert_eq!(trades[0].no_price, 0.93);
    }

    #[test]
    fn trades_rebase_a_fill_named_on_the_no_outcome_into_yes_terms() {
        // 0.951 of NO is the same trade as 0.049 of YES; charting it as 0.951
        // would put a 90-cent error on a 5-cent market.
        let trades = taped();
        assert_eq!(trades[1].yes_price, 0.049);
        assert_eq!(trades[1].no_price, 0.951);
        assert_eq!(trades[1].count, 0.01);
    }

    #[test]
    fn trades_read_the_iso_timestamp_as_unix_seconds() {
        assert_eq!(taped()[0].ts, 1_787_031_289_i64);
    }

    #[test]
    fn trades_name_each_fill_by_its_transaction_hash_and_its_leg() {
        let trades = taped();
        assert_eq!(
            trades[0].trade_id,
            "0x4d20a10735cc659e19c7121ef9a4fa550192c5792bb187b78b01908f2a66844d"
        );
        assert_eq!(trades[0].ticker, "big-game-champion-2027~26936");
        assert_eq!(trades[1].ticker, "big-game-champion-2027~26939");
    }

    #[test]
    fn trades_keep_two_fills_of_one_resting_order_apart_which_the_cursor_would_not() {
        // The cursor decodes to {orderId, createdAt} and repeats across fills: 50
        // live rows carried 44 distinct cursors and 50 distinct hashes.
        let shared =
            "eyJvcmRlcklkIjoyMjIxNDgzNjUwLCJjcmVhdGVkQXQiOiIyMDI2LTA4LTE2VDE1OjMzOjExLjAwMFoifQ==";
        let both = normalise_trades(
            Some(&trades_of(json!({ "edges": [
                { "cursor": shared, "node": {
                    "transactionHash": "0xaaa", "amountFilled": "10000000000000000",
                    "priceExecuted": "70000000000000000", "timestamp": "2026-08-16T15:33:11.000Z",
                    "outcome": { "index": 1, "name": "Yes" } } },
                { "cursor": shared, "node": {
                    "transactionHash": "0xbbb", "amountFilled": "107240000000000000000",
                    "priceExecuted": "70000000000000000", "timestamp": "2026-08-16T15:33:11.000Z",
                    "outcome": { "index": 1, "name": "Yes" } } }
            ]}))),
            "",
        );
        assert_eq!(both.len(), 2);
        assert_ne!(both[0].trade_id, both[1].trade_id);
    }

    #[test]
    fn trades_state_no_aggressor_because_the_venue_documents_none() {
        // `quoteType` was read as the taker's direction until one leg was seen
        // printing Yes/ASK and Yes/BID at an identical price, which that reading
        // cannot produce. An unknown side prints as unknown.
        let trades = taped();
        assert_eq!(trades[0].taker_side, "");
        assert_eq!(trades[1].taker_side, "");
        assert!(!trades[0].is_block_trade);
    }

    #[test]
    fn trades_drop_a_row_they_cannot_read_rather_than_tape_a_zero_priced_fill() {
        let broken = normalise_trades(
            Some(&trades_of(json!({ "edges": [
                { "node": { "transactionHash": "0x1", "amountFilled": "1.5",
                            "priceExecuted": "70000000000000000", "timestamp": "2026-08-18T05:34:49.000Z" } },
                { "node": { "transactionHash": "0x2", "amountFilled": "10000000000000000",
                            "priceExecuted": null, "timestamp": "2026-08-18T05:34:49.000Z" } },
                { "node": { "transactionHash": "0x3", "amountFilled": "10000000000000000",
                            "priceExecuted": "70000000000000000", "timestamp": "never" } }
            ]}))),
            "",
        );
        assert!(broken.is_empty());
    }

    #[test]
    fn trades_fall_back_to_the_leg_they_were_asked_about_when_a_row_omits_its_own() {
        let bare = normalise_trades(
            Some(&trades_of(json!({ "edges": [{ "node": {
                "transactionHash": "0x9", "amountFilled": "10000000000000000",
                "priceExecuted": "70000000000000000", "timestamp": "2026-08-18T05:34:49.000Z",
                "outcome": { "index": 1, "name": "Yes" }
            }}]}))),
            "big-game-champion-2027~26936",
        );
        assert_eq!(bare[0].ticker, "big-game-champion-2027~26936");
    }

    /* ---------------------------------------------------------------- history */

    /// Real 10-minute samples from a category's `_1D` series, in percent.
    const SAMPLES: &[(i64, Option<f64>)] = &[
        (1_786_947_000, Some(2.0)),
        (1_786_947_600, Some(2.0)),
        (1_786_948_200, Some(3.0)),
        (1_786_948_800, Some(1.5)),
    ];

    #[test]
    fn candles_read_the_samples_as_percentages_and_report_dollars() {
        let candles = bucket(&[(1_786_947_000, Some(2.0))], CandleInterval::OneHour);
        assert_eq!(candles[0].close, 0.02);
    }

    #[test]
    fn candles_take_open_high_low_and_close_from_the_samples_in_the_period() {
        let candles = bucket(SAMPLES, CandleInterval::OneHour);
        assert_eq!(candles.len(), 1);
        assert_eq!(candles[0].open, 0.02);
        assert_eq!(candles[0].high, 0.03);
        assert_eq!(candles[0].low, 0.015);
        assert_eq!(candles[0].close, 0.015);
        assert!(candles[0].traded);
    }

    #[test]
    fn candles_stamp_a_bar_with_the_period_it_ends_so_venues_share_an_x_axis() {
        let candles = bucket(SAMPLES, CandleInterval::OneHour);
        assert_eq!(candles[0].time, 1_786_950_000_i64);
        assert_eq!(candles[0].time % 3600, 0);
    }

    #[test]
    fn candles_state_no_size_because_a_probability_sample_carries_none() {
        let candles = bucket(SAMPLES, CandleInterval::OneHour);
        assert_eq!(candles[0].volume, None);
        assert_eq!(candles[0].open_interest, None);
        assert_eq!(candles[0].bid, None);
        assert_eq!(candles[0].ask, None);
    }

    #[test]
    fn candles_bridge_a_quiet_stretch_with_bars_marked_as_untraded() {
        let gapped = bucket(
            &[(1_786_947_000, Some(2.0)), (1_786_958_000, Some(4.0))],
            CandleInterval::OneHour,
        );
        assert_eq!(gapped.len(), 4);
        assert_eq!(
            gapped.iter().map(|c| c.traded).collect::<Vec<_>>(),
            vec![true, false, false, true]
        );
        // A carried-forward bar holds the last close rather than inventing a
        // move.
        assert_eq!(gapped[1].open, 0.02);
        assert_eq!(gapped[1].close, 0.02);
    }

    #[test]
    fn candles_stop_bridging_once_the_gap_is_longer_than_the_chart_is_worth() {
        // A series that stops mid-window must not generate a million flat bars,
        // so past the cap the gap is left as a gap.
        let far = 1_786_947_000_i64 + 3600 * 900;
        let gapped = bucket(
            &[(1_786_947_000, Some(2.0)), (far, Some(4.0))],
            CandleInterval::OneHour,
        );
        assert_eq!(gapped.len(), 2 + MAX_CARRY_FORWARD as usize);
    }

    #[test]
    fn candles_ignore_a_sample_the_series_left_blank() {
        let blank = bucket(&[(1_786_947_000, None)], CandleInterval::OneHour);
        assert!(blank.is_empty());

        let no_clock = bucket_samples(
            &[&RawPfPoint {
                x: None,
                y: Some(2.0),
            }],
            CandleInterval::OneHour,
        );
        assert!(no_clock.is_empty());
    }

    #[test]
    fn candles_bucket_the_real_ten_minute_samples_the_venue_publishes() {
        // Four consecutive live samples of leg 1054371, which fell from 29 to 27
        // and recovered inside one hour. The sample on the hour boundary closes
        // the period it ends rather than opening the next one.
        let candles = bucket(
            &[
                (1_787_488_200, Some(29.0)),
                (1_787_488_800, Some(27.0)),
                (1_787_489_400, Some(28.0)),
                (1_787_490_000, Some(29.0)),
            ],
            CandleInterval::OneHour,
        );
        assert_eq!(candles.len(), 1);
        assert_eq!(candles[0].time, 1_787_490_000_i64);
        assert_eq!(candles[0].open, 0.29);
        assert_eq!(candles[0].high, 0.29);
        assert_eq!(candles[0].low, 0.27);
        assert_eq!(candles[0].close, 0.29);
    }

    /* --------------------------------------------------------------- taxonomy */

    #[test]
    fn series_names_a_tag_by_the_id_the_venues_own_filter_accepts() {
        let series: RawPfTag = serde_json::from_value(json!({
            "id": "4", "name": "Sports", "open": 817, "parent": null,
            "children": [{ "id": "113", "name": "World Cup" }, { "id": "14", "name": "Soccer" }]
        }))
        .expect("the fixture parses as a tag");
        let info = normalise_series(&series);

        assert_eq!(info.venue, Venue::PredictFun);
        assert_eq!(info.ticker, "4");
        assert_eq!(info.title, "Sports");
        // A top-level tag files under itself, so filtering on it keeps its
        // children.
        assert_eq!(info.category, "Sports");
        assert_eq!(info.tags, vec!["World Cup", "Soccer"]);
        assert_eq!(info.frequency, "");
    }

    #[test]
    fn series_files_a_sub_tag_under_its_immediate_parent() {
        let weather: RawPfTag = serde_json::from_value(json!({
            "id": "410", "name": "Weather", "open": 31,
            "parent": { "id": "13", "name": "Culture" }, "children": []
        }))
        .expect("the fixture parses as a tag");
        let info = normalise_series(&weather);
        assert_eq!(info.category, "Culture");
        assert!(info.tags.is_empty());
    }

    /* --------------------------------------------------------- the fixtures */

    #[test]
    fn the_captured_market_answer_normalises_into_the_leg_it_describes() {
        let envelope: MarketEnvelope = payload(MARKET_JSON);
        let raw = envelope.market.expect("the fixture names a market");
        let market = normalise_market(&raw, None);

        assert_eq!(market.ticker, "nba-2027-champion~927684");
        assert_eq!(market.series_ticker, "nba-champion");
        assert_eq!(
            market.title,
            "Will San Antonio Spurs win the 2027 NBA Finals?"
        );
        assert_eq!(market.yes_sub_title, "San Antonio Spurs");
        assert_eq!(market.status, "open");
        assert_eq!(market.yes_bid, Some(0.203));
        assert_eq!(market.yes_ask, Some(0.207));
        assert_eq!(market.no_bid, Some(0.793));
        assert_eq!(market.mid, Some(0.205));
        // `{price: "0.20", outcome: "No"}` — quoted in YES terms all the same,
        // and 0.20 sits on the book where 0.80 would not.
        assert_eq!(market.last_price, Some(0.2));
        assert_eq!(market.volume, Some(134996.89));
        assert_eq!(market.volume24h, Some(570.68));
        assert_eq!(market.liquidity, Some(11767.57));
        // Read back off the fixture rather than transcribed, so the assertion
        // cannot drift from the file: `sharesCount` is passed through untouched,
        // neither rounded nor rescaled, and a 17-digit decimal is exactly the
        // kind of literal a hand-copied expectation gets wrong by one ulp.
        assert_eq!(
            market.open_interest,
            captured_shares_count(),
            "open interest is the YES outcome's own sharesCount"
        );
        assert_eq!(market.change, None);
        assert_eq!(market.category.as_deref(), Some("Sports"));
        assert_eq!(market.close_time, "2027-07-01T00:00:00.000Z");
    }

    #[test]
    fn the_captured_book_ladders_at_the_legs_three_place_precision() {
        let envelope: MarketEnvelope = payload(MARKET_JSON);
        let raw = envelope.market.expect("the fixture names a market");
        let book = normalise_order_book(&raw, "nba-2027-champion~927684", 12);

        assert_eq!(book.best_yes_bid, Some(0.203));
        assert_eq!(book.best_yes_ask, Some(0.207));
        assert_eq!(book.spread, Some(0.004));
        // 1 - 0.207 at three places.
        assert_eq!(book.no[0].price, 0.793);
    }

    #[test]
    fn the_captured_tape_unscales_and_rebases_the_rows_it_carries() {
        let envelope: TradesEnvelope = payload(TRADES_JSON);
        let trades = normalise_trades(envelope.match_event_log.as_ref(), "");

        assert_eq!(trades.len(), 3);
        assert_eq!(trades[0].count, 35.92);
        assert_eq!(trades[0].yes_price, 0.203);
        assert_eq!(trades[0].ticker, "nba-2027-champion~927684");
        // The size is a repeating decimal in wei; six places is where the tape
        // is read, and a plain truncation would report 72.463768.
        assert_eq!(trades[1].count, 72.463768);
        assert_eq!(trades[1].yes_price, 0.207);
        // The third row names the NO outcome at 0.793, which is a YES fill at
        // 0.207 — the same price the two rows above it printed.
        assert_eq!(trades[2].yes_price, 0.207);
        assert_eq!(trades[2].no_price, 0.793);
        assert_eq!(trades[2].count, 70.49);
    }

    #[test]
    fn the_captured_catalogue_mirrors_a_leg_at_its_own_precision_not_its_categorys() {
        // `big-game-champion-2027` is quoted to two places and every leg under it
        // to three — live today, and the reason `complement` reads the leg. With
        // the venue's own NO side removed the mirror has to fill it in, and at
        // three places `1 - 0.138` is 0.862 where the category's two would give
        // 0.86 — a tenth of a cent away from the ladder the exchange shows.
        let envelope: CrawlEnvelope = payload(CRAWL_JSON);
        let categories = envelope.categories.expect("the crawl carries categories");
        let nfl = nodes(Some(&categories))
            .into_iter()
            .find(|c| c.slug.as_deref() == Some("big-game-champion-2027"))
            .expect("the NFL book is in the capture")
            .clone();

        assert_eq!(nfl.decimal_precision, Some(2));
        let mut leg = nodes(nfl.markets.as_ref())
            .first()
            .copied()
            .expect("the capture carries legs")
            .clone();
        assert_eq!(leg.decimal_precision, Some(3));

        // Drop the stated NO outcome so the mirror is the only thing left.
        let yes_only = nodes(leg.outcomes.as_ref())
            .into_iter()
            .find(|o| o.index == Some(1))
            .expect("the leg has a YES outcome")
            .clone();
        leg.outcomes = Some(RawPfConnection {
            total_count: None,
            page_info: None,
            edges: Some(vec![Some(RawPfEdge {
                cursor: None,
                node: Some(yes_only),
            })]),
        });

        let market = normalise_market(&leg, Some(&nfl));
        assert_eq!(market.yes_ask, Some(0.138));
        assert_eq!(market.no_bid, Some(0.862));
    }

    /* --------------------------------------------------------------- the wire */

    fn state_for(server: &MockServer) -> AppState {
        AppState::new(Config {
            // The config field is the whole endpoint URL rather than a prefix,
            // which is what lets a mock stand in for the host.
            predictfun_graphql_base: format!("{}/graphql", server.uri()),
            ..Config::default()
        })
    }

    fn body(value: Value) -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_json(value)
    }

    fn market_fixture() -> Value {
        serde_json::from_str(MARKET_JSON).expect("the captured market parses")
    }

    fn trades_fixture() -> Value {
        serde_json::from_str(TRADES_JSON).expect("the captured tape parses")
    }

    fn crawl_fixture() -> Value {
        serde_json::from_str(CRAWL_JSON).expect("the captured crawl parses")
    }

    /// A category answer built from the crawl fixture's own rows, which is the
    /// same shape `category(id:)` returns.
    fn category_answer(slug: &str) -> Value {
        let crawl = crawl_fixture();
        let node = crawl["data"]["categories"]["edges"]
            .as_array()
            .expect("the crawl fixture carries edges")
            .iter()
            .find(|edge| edge["node"]["slug"] == slug)
            .map(|edge| edge["node"].clone())
            .expect("the crawl fixture carries the category asked for");
        json!({ "data": { "category": node } })
    }

    async fn mount_all(server: &MockServer, answer: Value) {
        Mock::given(method("GET"))
            .and(path_matcher("/graphql"))
            .respond_with(body(answer))
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn a_two_hundred_carrying_an_errors_array_is_a_failure_and_not_an_empty_answer() {
        // The endpoint rejects a query with HTTP 200 and an `errors` body, so a
        // status check alone would hand the panel a market made of nothing.
        let server = MockServer::start().await;
        mount_all(
            &server,
            json!({
                "data": null,
                "errors": [{
                    "message": "[08ccdb] InvalidInputError. invalid id",
                    "extensions": { "cause": "invalid id", "code": "InvalidInputError" }
                }]
            }),
        )
        .await;

        let err = get_market(&state_for(&server), "26952")
            .await
            .expect_err("a 200 with errors is not a success");
        assert!(err.message.contains("invalid id"));
        // The reported code goes in the hint, because it is the only place the
        // venue says what kind of no this was.
        assert!(err.hint.unwrap().contains("InvalidInputError"));
    }

    #[tokio::test]
    async fn a_null_entity_with_no_error_at_all_is_read_as_a_miss() {
        // The other half of the same trap: a category that does not exist comes
        // back as `data.category == null` with no `errors` key anywhere.
        let server = MockServer::start().await;
        mount_all(&server, json!({ "data": { "category": null } })).await;

        let err = get_event(&state_for(&server), "no-such-category")
            .await
            .expect_err("a null category is not a category");
        assert_eq!(err.code, codes::NOT_FOUND);
        assert_eq!(
            err.message,
            "No predict.fun category with slug no-such-category"
        );
        assert!(err.hint.unwrap().contains('~'));
    }

    #[tokio::test]
    async fn the_request_never_carries_an_origin_header() {
        // A wrong `Origin` is a hard 403 (`request-origin not allowed`) and no
        // `Origin` is a plain 200, so the one header this venue cares about is
        // the one that must not be sent.
        let server = MockServer::start().await;
        mount_all(&server, market_fixture()).await;

        let _ = get_market(&state_for(&server), "927684").await;

        let requests = server
            .received_requests()
            .await
            .expect("the mock recorded the request");
        assert_eq!(requests.len(), 1);
        assert!(requests[0].headers.get("origin").is_none());
        // The query really did travel in the URL, which is why GET works at all.
        assert!(requests[0].url.query().unwrap().contains("query=query"));
    }

    #[tokio::test]
    async fn get_market_reads_a_bare_leg_id_without_a_round_trip_through_the_category() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/graphql"))
            .and(query_param("variables", r#"{"id":"927684"}"#))
            .respond_with(body(market_fixture()))
            // One request: a numeric reference is already the leg id.
            .expect(1)
            .mount(&server)
            .await;

        let market = get_market(&state_for(&server), "927684")
            .await
            .expect("the leg resolves");
        assert_eq!(market.ticker, "nba-2027-champion~927684");
    }

    #[tokio::test]
    async fn get_market_takes_the_leg_id_from_the_last_separator_of_a_full_ticker() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/graphql"))
            .and(query_param("variables", r#"{"id":"927684"}"#))
            .respond_with(body(market_fixture()))
            .expect(1)
            .mount(&server)
            .await;

        let market = get_market(&state_for(&server), "nba-2027-champion~927684")
            .await
            .expect("the ticker resolves");
        assert_eq!(market.event_ticker, "nba-2027-champion");
    }

    #[tokio::test]
    async fn get_market_resolves_a_slug_that_names_exactly_one_leg() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/graphql"))
            .and(query_param("variables", r#"{"id":"solo-question"}"#))
            .respond_with(body(json!({ "data": { "category": {
                "id": "solo-question", "slug": "solo-question", "title": "A single-leg book",
                "markets": connection(vec![json!({ "id": "555001", "title": "Yes" })])
            }}})))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path_matcher("/graphql"))
            .and(query_param("variables", r#"{"id":"555001"}"#))
            .respond_with(body(json!({ "data": { "market": {
                "id": "555001", "question": "Will it?", "status": "REGISTERED",
                "category": { "id": "solo-question", "slug": "solo-question" }
            }}})))
            .mount(&server)
            .await;

        let market = get_market(&state_for(&server), "solo-question")
            .await
            .expect("a single-leg category names one contract");
        assert_eq!(market.ticker, "solo-question~555001");
    }

    #[tokio::test]
    async fn get_market_refuses_a_multi_leg_slug_and_names_the_legs_to_pick_from() {
        // A slug is never passed to `market(id:)`: that argument takes integers
        // only and answers anything else with an error rather than a miss.
        let server = MockServer::start().await;
        mount_all(&server, category_answer("nba-2027-champion")).await;

        let err = get_market(&state_for(&server), "nba-2027-champion")
            .await
            .expect_err("a category is not a contract");
        assert_eq!(err.code, codes::NOT_FOUND);
        assert!(err.message.contains("with 2 legs"));
        let hint = err.hint.expect("the reader is told what to type instead");
        assert!(hint.contains("EVT pf:nba-2027-champion"));
        assert!(hint.contains("nba-2027-champion~927684 (San Antonio Spurs)"));
    }

    #[tokio::test]
    async fn get_event_trims_a_contract_ticker_down_to_the_category_it_names() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/graphql"))
            .and(query_param(
                "variables",
                r#"{"id":"big-game-champion-2027"}"#,
            ))
            .respond_with(body(category_answer("big-game-champion-2027")))
            .expect(1)
            .mount(&server)
            .await;

        let event = get_event(&state_for(&server), "big-game-champion-2027~26952")
            .await
            .expect("the event resolves");
        assert_eq!(event.event_ticker, "big-game-champion-2027");
        assert_eq!(event.series_ticker, "big-game-champion");
        assert_eq!(event.markets.len(), 2);
        assert!(event.mutually_exclusive);
        // The venue states the NO side itself and this leg carries one, so the
        // mirror is not reached for; the stated figure is what is reported.
        assert_eq!(event.markets[0].no_bid, Some(0.862));
    }

    #[tokio::test]
    async fn get_order_book_names_the_ladder_by_the_ticker_the_venue_implies() {
        let server = MockServer::start().await;
        mount_all(&server, market_fixture()).await;

        let book = get_order_book(&state_for(&server), "927684", 2)
            .await
            .expect("a book");
        assert_eq!(book.ticker, "nba-2027-champion~927684");
        assert_eq!(book.yes.len(), 2);
        assert_eq!(book.best_yes_bid, Some(0.203));
    }

    #[tokio::test]
    async fn get_trades_never_asks_for_more_than_the_connection_will_return() {
        // Asked for 200, 500 or 1,000 the tape returns 150 rows with
        // `hasNextPage` still true, so asking for more would report a shorter
        // tape than the caller believes it received.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/graphql"))
            .and(query_param(
                "variables",
                r#"{"first":150,"marketId":"927684"}"#,
            ))
            .respond_with(body(trades_fixture()))
            .expect(1)
            .mount(&server)
            .await;

        let tape = get_trades(&state_for(&server), "927684", 5_000)
            .await
            .expect("a tape");
        assert_eq!(tape.trades.len(), 3);
        // `hasNextPage` is true in the fixture, so the cursor is handed on.
        assert!(tape.cursor.is_some());
    }

    #[tokio::test]
    async fn get_trades_reads_an_empty_page_on_an_unknown_leg_as_a_miss() {
        // The tape connection answers an unknown leg with an empty page rather
        // than an error, so the leg is confirmed in the same round trip: a quiet
        // market and a mistyped id must not both read as a market nobody trades.
        let server = MockServer::start().await;
        mount_all(
            &server,
            json!({ "data": {
                "market": null,
                "matchEventLog": { "pageInfo": { "hasNextPage": false, "endCursor": null }, "edges": [] }
            }}),
        )
        .await;

        let err = get_trades(&state_for(&server), "99999999", 50)
            .await
            .expect_err("an empty page on an unknown leg is a miss");
        assert_eq!(err.code, codes::NOT_FOUND);
        assert_eq!(err.message, "No predict.fun market with id 99999999");
    }

    #[tokio::test]
    async fn get_trades_reports_a_quiet_but_real_leg_as_an_empty_tape() {
        let server = MockServer::start().await;
        mount_all(
            &server,
            json!({ "data": {
                "market": { "id": "927684" },
                "matchEventLog": { "pageInfo": { "hasNextPage": false, "endCursor": null }, "edges": [] }
            }}),
        )
        .await;

        let tape = get_trades(&state_for(&server), "927684", 50)
            .await
            .expect("a leg that exists and has not traded is not an error");
        assert!(tape.trades.is_empty());
        assert_eq!(tape.cursor, None);
    }

    #[tokio::test]
    async fn get_candles_asks_for_the_shortest_window_that_reaches_back_far_enough() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/graphql"))
            .and(query_param("variables", r#"{"id":"927684"}"#))
            .respond_with(body(market_fixture()))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path_matcher("/graphql"))
            // The history has no from/to: a half-hour lookback is served by the
            // `_1H` window and clipped here.
            .and(query_param(
                "variables",
                r#"{"id":"nba-2027-champion","interval":"_1H"}"#,
            ))
            .respond_with(body(
                json!({ "data": { "timeseries": connection(vec![json!({
                    "dataGranularity": "_1m",
                    "market": { "id": "927684", "title": "San Antonio Spurs" },
                    "data": connection(vec![
                        json!({ "x": 1_787_488_200_i64, "y": 21.0 }),
                        json!({ "x": 1_787_490_000_i64, "y": 22.0 }),
                    ])
                })])}}),
            ))
            .expect(1)
            .mount(&server)
            .await;

        let now = OffsetDateTime::now_utc().unix_timestamp();
        let candles = get_candles(
            &state_for(&server),
            "927684",
            CandleInterval::OneHour,
            now - 1_800,
            now + 86_400,
        )
        .await
        .expect("a chart");

        assert_eq!(candles.ticker, "nba-2027-champion~927684");
        assert_eq!(candles.series_ticker, "nba-champion");
        // The note is the disclosure: these are bucketed probability samples and
        // the window is the venue's, not the caller's.
        let note = candles.note.expect("a sample series discloses itself");
        assert!(note.contains("probability sample series"));
        assert!(note.contains("\"_1H\""));
    }

    #[tokio::test]
    async fn get_candles_clips_the_venues_window_to_the_range_that_was_asked_for() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/graphql"))
            .and(query_param("variables", r#"{"id":"927684"}"#))
            .respond_with(body(market_fixture()))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path_matcher("/graphql"))
            .and(query_param(
                "variables",
                r#"{"id":"nba-2027-champion","interval":"MAX"}"#,
            ))
            .respond_with(body(json!({ "data": { "timeseries": connection(vec![
                // A second leg's series shares the payload; only the leg asked
                // about is charted.
                json!({ "market": { "id": "927680" }, "data": connection(vec![json!({ "x": 1_787_490_000_i64, "y": 99.0 })]) }),
                json!({ "market": { "id": "927684" }, "data": connection(vec![
                    json!({ "x": 1_000_000_000_i64, "y": 5.0 }),
                    json!({ "x": 1_787_488_200_i64, "y": 21.0 }),
                    json!({ "x": 1_787_490_000_i64, "y": 22.0 }),
                    json!({ "x": 2_000_000_000_i64, "y": 90.0 }),
                ])}),
            ])}})))
            .mount(&server)
            .await;

        let candles = get_candles(
            &state_for(&server),
            "927684",
            CandleInterval::OneHour,
            // Far enough back that the widest window is always the one chosen,
            // whatever day the test runs on, and late enough that the oldest
            // sample in the payload is outside the range and must be clipped.
            1_500_000_000,
            1_787_490_000,
        )
        .await
        .expect("a chart");

        // The two samples outside the range are dropped, and the two inside land
        // in the one hour bucket they end.
        assert_eq!(candles.candles.len(), 1);
        assert_eq!(candles.candles[0].time, 1_787_490_000_i64);
        assert_eq!(candles.candles[0].open, 0.21);
        assert_eq!(candles.candles[0].close, 0.22);
        assert_eq!(candles.candles[0].volume, None);
    }

    #[tokio::test]
    async fn get_candles_says_so_when_the_venue_publishes_no_history_for_the_leg() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/graphql"))
            .and(query_param("variables", r#"{"id":"927684"}"#))
            .respond_with(body(market_fixture()))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path_matcher("/graphql"))
            .and(query_param(
                "variables",
                r#"{"id":"nba-2027-champion","interval":"MAX"}"#,
            ))
            .respond_with(body(json!({ "data": { "timeseries": connection(vec![
                json!({ "market": { "id": "927680" }, "data": connection(vec![]) })
            ])}})))
            .mount(&server)
            .await;

        let err = get_candles(
            &state_for(&server),
            "927684",
            CandleInterval::OneHour,
            1_000_000_000,
            2_000_000_000,
        )
        .await
        .expect_err("a leg with no series is not a chart");
        assert_eq!(err.code, codes::NOT_FOUND);
        assert!(err.hint.unwrap().contains("once a leg has traded"));
    }

    #[tokio::test]
    async fn list_series_offers_only_the_tags_with_something_open_behind_them() {
        // A third of the tree has no live category, and offering those as filters
        // means offering a filter that returns nothing.
        let server = MockServer::start().await;
        mount_all(
            &server,
            json!({ "data": { "categoryTags": connection(vec![
                json!({ "id": "4", "name": "Sports", "open": 817, "parent": null,
                        "children": [{ "id": "14", "name": "Soccer" }] }),
                json!({ "id": "243", "name": "Int'l Friendlies", "open": 0,
                        "parent": { "id": "4", "name": "Sports" }, "children": [] }),
                json!({ "id": "410", "name": "Weather", "open": 31,
                        "parent": { "id": "13", "name": "Culture" }, "children": [] }),
            ])}}),
        )
        .await;

        let series = list_series(&state_for(&server), None)
            .await
            .expect("a taxonomy");
        assert_eq!(series.len(), 2);
        assert_eq!(series[0].ticker, "4");
        assert_eq!(series[1].ticker, "410");
    }

    #[tokio::test]
    async fn an_empty_category_is_no_filter_rather_than_one_nothing_matches() {
        // `GET /api/venue/predictfun/series?category=` reaches here as
        // `Some("")`. Read as a filter it answers with an empty tag tree, which
        // is a statement about the venue rather than about the query.
        let server = MockServer::start().await;
        mount_all(
            &server,
            json!({ "data": { "categoryTags": connection(vec![
                json!({ "id": "4", "name": "Sports", "open": 817, "parent": null, "children": [] }),
                json!({ "id": "410", "name": "Weather", "open": 31,
                        "parent": { "id": "13", "name": "Culture" }, "children": [] }),
            ])}}),
        )
        .await;
        let state = state_for(&server);

        let all = list_series(&state, None).await.expect("a taxonomy");
        for blank in ["", "   "] {
            let asked = list_series(&state, Some(blank)).await.expect("a taxonomy");
            assert_eq!(asked.len(), all.len(), "{blank:?}");
        }
    }

    #[tokio::test]
    async fn list_series_narrows_on_a_tags_own_name_or_its_parents() {
        let server = MockServer::start().await;
        mount_all(
            &server,
            json!({ "data": { "categoryTags": connection(vec![
                json!({ "id": "4", "name": "Sports", "open": 817, "parent": null, "children": [] }),
                json!({ "id": "14", "name": "Soccer", "open": 631,
                        "parent": { "id": "4", "name": "Sports" }, "children": [] }),
                json!({ "id": "410", "name": "Weather", "open": 31,
                        "parent": { "id": "13", "name": "Culture" }, "children": [] }),
            ])}}),
        )
        .await;

        let state = state_for(&server);
        // A top-level tag files under itself, so asking for Sports keeps both it
        // and the sub-tags hanging off it.
        let sports = list_series(&state, Some("  Sports "))
            .await
            .expect("a taxonomy");
        assert_eq!(
            sports.iter().map(|s| s.ticker.as_str()).collect::<Vec<_>>(),
            vec!["4", "14"]
        );

        let culture = list_series(&state, Some("culture"))
            .await
            .expect("a taxonomy");
        assert_eq!(culture.len(), 1);
        assert_eq!(culture[0].title, "Weather");
    }

    #[tokio::test]
    async fn the_crawl_walks_the_catalogue_and_stops_where_the_venue_says_to() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/graphql"))
            .and(query_param("variables", r#"{"after":null}"#))
            .respond_with(body(crawl_fixture()))
            // One page: the fixture says `hasNextPage: false`.
            .expect(1)
            .mount(&server)
            .await;

        let corpus = corpus_snapshot(&state_for(&server))
            .await
            .expect("the crawl completes");
        assert_eq!(corpus.venue, Venue::PredictFun);
        assert_eq!(corpus.events.len(), 3);
        assert_eq!(corpus.markets.len(), 6);
        assert!(!corpus.truncated);

        let fed = corpus
            .events
            .iter()
            .find(|e| e.event_ticker == "fed-decision-in-september-762")
            .expect("the Fed book is in the snapshot");
        // The series the curated cross-venue link names.
        assert_eq!(fed.series_ticker, "fed-decision-in");
        assert_eq!(fed.category, "Economy");
    }

    #[tokio::test]
    async fn the_crawl_follows_the_cursor_and_never_counts_a_category_twice() {
        let server = MockServer::start().await;
        let mut first = crawl_fixture();
        first["data"]["categories"]["pageInfo"] =
            json!({ "hasNextPage": true, "endCursor": "page-two" });

        Mock::given(method("GET"))
            .and(path_matcher("/graphql"))
            .and(query_param("variables", r#"{"after":null}"#))
            .respond_with(body(first))
            .expect(1)
            .mount(&server)
            .await;
        // The second page repeats one category and adds none: a venue that pages
        // inconsistently must not double the snapshot.
        Mock::given(method("GET"))
            .and(path_matcher("/graphql"))
            .and(query_param("variables", r#"{"after":"page-two"}"#))
            .respond_with(body(crawl_fixture()))
            .expect(1)
            .mount(&server)
            .await;

        let corpus = corpus_snapshot(&state_for(&server))
            .await
            .expect("the crawl completes");
        assert_eq!(corpus.events.len(), 3);
        assert_eq!(corpus.markets.len(), 6);
    }

    #[tokio::test]
    async fn the_crawl_reports_a_catalogue_it_could_not_finish() {
        // `pagination.first` is clamped to 100 in silence on this connection, so
        // the page cap is the only thing standing between a crawl and a catalogue
        // that grew overnight. A truncated universe says so rather than passing
        // for the whole one.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/graphql"))
            .respond_with(body(json!({ "data": { "categories": {
                "totalCount": 9000,
                "pageInfo": { "hasNextPage": true, "endCursor": "always-more" },
                "edges": []
            }}})))
            .mount(&server)
            .await;

        let corpus = corpus_snapshot(&state_for(&server))
            .await
            .expect("the crawl completes");
        assert!(corpus.truncated);
    }

    #[tokio::test]
    async fn the_snapshot_is_built_once_and_shared() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/graphql"))
            .respond_with(body(crawl_fixture()))
            .expect(1)
            .mount(&server)
            .await;

        let state = state_for(&server);
        let first = corpus_snapshot(&state).await.expect("a snapshot");
        let second = corpus_snapshot(&state).await.expect("a snapshot");
        assert!(Arc::ptr_eq(&first, &second));
    }

    #[tokio::test]
    async fn search_ranks_the_snapshot_rather_than_the_venues_own_search_query() {
        let server = MockServer::start().await;
        mount_all(&server, crawl_fixture()).await;

        let hits = search(&state_for(&server), "fed decision", 10)
            .await
            .expect("a result list");
        assert_eq!(hits.query, "fed decision");
        assert_eq!(hits.scanned, 3);
        assert_eq!(hits.hits.len(), 1);
        assert_eq!(
            hits.hits[0].event.event_ticker,
            "fed-decision-in-september-762"
        );
        // Turnover is summed off the legs, in the dollars this venue meters.
        assert_eq!(hits.hits[0].volume24h, Some(767.4));
    }

    #[tokio::test]
    async fn search_refuses_a_query_with_more_words_than_it_will_match() {
        let server = MockServer::start().await;
        mount_all(&server, crawl_fixture()).await;

        let err = search(&state_for(&server), &"fed ".repeat(20), 10)
            .await
            .expect_err("an overlong query is refused rather than truncated");
        assert_eq!(err.code, codes::BAD_REQUEST);
    }

    #[tokio::test]
    async fn top_markets_ranks_the_three_boards_the_venue_actually_publishes() {
        let server = MockServer::start().await;
        mount_all(&server, crawl_fixture()).await;
        let state = state_for(&server);

        let volume = top_markets(&state, MoverSort::Volume, 3)
            .await
            .expect("a board");
        assert_eq!(volume.len(), 3);
        // Dollars, not contracts — the busiest leg in the fixture is the Bills at
        // $852.60 of 24h turnover.
        assert_eq!(volume[0].ticker, "big-game-champion-2027~26936");
        assert_eq!(volume[0].volume24h, Some(852.6));

        let liquidity = top_markets(&state, MoverSort::Liquidity, 1)
            .await
            .expect("a board");
        assert_eq!(liquidity[0].ticker, "nba-2027-champion~927684");
        assert_eq!(liquidity[0].liquidity, Some(17168.97));

        let open_interest = top_markets(&state, MoverSort::OpenInterest, 1)
            .await
            .expect("a board");
        assert_eq!(
            open_interest[0].ticker,
            "fed-decision-in-september-762~1054369"
        );
        assert_eq!(open_interest[0].open_interest, Some(64753.21300710829));
    }

    #[tokio::test]
    async fn top_markets_leaves_the_movers_boards_empty_because_the_24h_figure_has_no_sign() {
        // The venue's only 24h figure is a magnitude — 0 negative values across
        // 963 live legs — so `Market::change` is unstated and the shared ranker
        // drops every leg rather than ranking an unpublished figure as zero. The
        // venue registry declares the same three boards this returns.
        let server = MockServer::start().await;
        mount_all(&server, crawl_fixture()).await;
        let state = state_for(&server);

        assert!(top_markets(&state, MoverSort::Gainers, 25)
            .await
            .expect("a board")
            .is_empty());
        assert!(top_markets(&state, MoverSort::Losers, 25)
            .await
            .expect("a board")
            .is_empty());
    }
}
