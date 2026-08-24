//! Equity, ETF and index option chains.
//!
//! The same two providers as [`crate::sources::stocks`], in the same order and
//! for the same reason. **Yahoo Finance** is primary: `/v7/finance/options` is
//! the only free, keyless source that carries the whole US listed board with
//! bid, ask, volume and open interest per contract. **Nasdaq** is the fallback,
//! because Yahoo's `v7` hosts answer 429 to shared datacentre addresses — and
//! notably harder than `v8/chart` does, so a deployment where `STK AAPL` works
//! can still find `OPT AAPL` blocked. Nasdaq answers from those networks; both
//! behaviours were re-confirmed live from this container.
//!
//! They disagree about almost everything except the numbers:
//!
//! ```text
//!              Yahoo                     Nasdaq
//! Shape        JSON per expiry           one flat table, paged
//! Expiry list  `expirationDates`         inferred from group headers
//! Contract id  OCC symbol                a drill-down URL
//! Numbers      native                    strings, `"--"` for missing
//! ```
//!
//! Both are normalised to an [`OptionBoard`] keyed by **OCC contract symbols**
//! (`AAPL260918C00300000`), which Yahoo already emits and which are
//! reconstructed for Nasdaq. That matters beyond tidiness: it is what lets a row
//! clicked in a chain served by one provider open a contract panel served by the
//! other.
//!
//! No implied volatility is taken from either provider. Yahoo publishes one, but
//! against its own undisclosed carry assumptions — mixing that with a forward
//! this terminal fits from parity would put two disagreeing volatilities on one
//! screen and Greeks that match neither. Every vol here is solved from the book
//! mid against the fitted forward, so price, vol and Greeks are one consistent
//! set. See [`terminal_core::greeks`].

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde::Deserialize;
use terminal_core::greeks::equity_expiry_instant;
use terminal_core::types::{AssetClass, OptionType};

use crate::app::AppState;
use crate::cache::ttl;
use crate::error::UpstreamError;
use crate::http::FetchOptions;
use crate::sources::optionboard::{BoardQuote, OptionBoard};
use crate::sources::stocks::{get_quote, parse_nasdaq_number};

type Result<T> = std::result::Result<T, UpstreamError>;

const BROWSER_UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) \
                          AppleWebKit/537.36 (KHTML, like Gecko) Chrome/127.0.0.0 Safari/537.36";

/// US listed equity options are 100 shares to a contract, without exception.
const CONTRACT_SIZE: f64 = 100.0;

/// Rows per Nasdaq page, and how many pages one crawl will walk.
const NASDAQ_PAGE: usize = 500;
const NASDAQ_MAX_PAGES: usize = 4;

const MONTHS: [&str; 12] = [
    "january",
    "february",
    "march",
    "april",
    "may",
    "june",
    "july",
    "august",
    "september",
    "october",
    "november",
    "december",
];

/* -------------------------------------------------------------- OCC symbols */

/// Build the OCC option symbol the whole terminal keys on.
///
/// `AAPL` + 2026-09-18 + call + 300 → `AAPL260918C00300000`. The strike is in
/// thousandths, zero-padded to eight digits, which is what makes a $0.50 strike
/// and a $500 strike sort and compare correctly as text.
#[must_use]
pub fn occ_symbol(
    symbol: &str,
    iso_date: &str,
    option_type: OptionType,
    strike: f64,
) -> Option<String> {
    let (year, month, day) = split_iso(iso_date)?;
    if !strike.is_finite() || strike <= 0.0 {
        return None;
    }
    let root: String = symbol
        .to_uppercase()
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect();
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let thousandths = (strike * 1000.0).round() as u64;
    let leg = match option_type {
        OptionType::Call => 'C',
        OptionType::Put => 'P',
    };
    Some(format!(
        "{root}{}{month}{day}{leg}{thousandths:08}",
        &year[2..]
    ))
}

fn split_iso(iso_date: &str) -> Option<(String, String, String)> {
    let bytes = iso_date.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return None;
    }
    if !bytes
        .iter()
        .enumerate()
        .all(|(i, b)| i == 4 || i == 7 || b.is_ascii_digit())
    {
        return None;
    }
    Some((
        iso_date[0..4].to_owned(),
        iso_date[5..7].to_owned(),
        iso_date[8..10].to_owned(),
    ))
}

#[derive(Debug, Clone, PartialEq)]
pub struct ParsedOcc {
    pub symbol: String,
    pub date: String,
    pub option_type: OptionType,
    pub strike: f64,
}

/// The inverse of [`occ_symbol`], for `OPD AAPL260918C00300000`.
#[must_use]
pub fn parse_occ(contract: &str) -> Option<ParsedOcc> {
    let upper = contract.trim().to_uppercase();
    let bytes = upper.as_bytes();
    // root (1–6 letters) + YYMMDD + C|P + 8 digits.
    if bytes.len() < 16 || bytes.len() > 21 {
        return None;
    }
    let tail_start = bytes.len() - 15;
    let root = &upper[..tail_start];
    if root.is_empty() || !root.chars().all(|c| c.is_ascii_alphabetic()) {
        return None;
    }
    let tail = &upper[tail_start..];
    let (date, rest) = tail.split_at(6);
    if !date.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let (leg, strike) = rest.split_at(1);
    let option_type = match leg {
        "C" => OptionType::Call,
        "P" => OptionType::Put,
        _ => return None,
    };
    if strike.len() != 8 || !strike.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }

    Some(ParsedOcc {
        symbol: root.to_owned(),
        date: format!("20{}-{}-{}", &date[0..2], &date[2..4], &date[4..6]),
        option_type,
        strike: strike.parse::<f64>().ok()? / 1000.0,
    })
}

/* ------------------------------------------------------------------- yahoo */

#[derive(Debug, Default, Deserialize)]
pub struct YahooOptionsResponse {
    #[serde(default, rename = "optionChain")]
    pub option_chain: Option<YahooOptionChain>,
}

#[derive(Debug, Default, Deserialize)]
pub struct YahooOptionChain {
    #[serde(default)]
    pub result: Vec<YahooResult>,
    #[serde(default)]
    pub error: Option<YahooError>,
}

#[derive(Debug, Deserialize)]
pub struct YahooError {
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct YahooResult {
    #[serde(default, rename = "underlyingSymbol")]
    pub underlying_symbol: Option<String>,
    #[serde(default, rename = "expirationDates")]
    pub expiration_dates: Vec<i64>,
    #[serde(default)]
    pub quote: Option<YahooQuote>,
    #[serde(default)]
    pub options: Vec<YahooSlice>,
}

#[derive(Debug, Default, Deserialize)]
pub struct YahooQuote {
    #[serde(default, rename = "regularMarketPrice")]
    pub regular_market_price: Option<f64>,
    #[serde(default, rename = "shortName")]
    pub short_name: Option<String>,
    #[serde(default, rename = "longName")]
    pub long_name: Option<String>,
    #[serde(default)]
    pub currency: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct YahooSlice {
    #[serde(default, rename = "expirationDate")]
    pub expiration_date: Option<i64>,
    #[serde(default)]
    pub calls: Vec<YahooOptionRow>,
    #[serde(default)]
    pub puts: Vec<YahooOptionRow>,
}

#[derive(Debug, Default, Deserialize)]
pub struct YahooOptionRow {
    #[serde(default, rename = "contractSymbol")]
    pub contract_symbol: Option<String>,
    #[serde(default)]
    pub strike: Option<f64>,
    #[serde(default, rename = "lastPrice")]
    pub last_price: Option<f64>,
    #[serde(default)]
    pub change: Option<f64>,
    #[serde(default)]
    pub bid: Option<f64>,
    #[serde(default)]
    pub ask: Option<f64>,
    #[serde(default)]
    pub volume: Option<f64>,
    #[serde(default, rename = "openInterest")]
    pub open_interest: Option<f64>,
    #[serde(default)]
    pub expiration: Option<i64>,
}

async fn fetch<T>(state: &AppState, url: String, key: String, ttl: Duration) -> Result<Arc<T>>
where
    T: DeserializeOwned + Send + Sync + 'static,
{
    state
        .cache()
        .cached(&key, ttl, || async {
            state
                .http()
                .fetch_json::<T>(
                    &url,
                    FetchOptions::new()
                        .timeout(Duration::from_secs(25))
                        .retries(1)
                        .header("User-Agent", BROWSER_UA),
                )
                .await
        })
        .await
}

fn positive(value: Option<f64>) -> Option<f64> {
    value.filter(|v| v.is_finite() && *v > 0.0)
}

fn finite(value: Option<f64>) -> Option<f64> {
    value.filter(|v| v.is_finite())
}

/// Yahoo's options payload → board quotes.
///
/// Yahoo pads a chain with contracts that have never traded and have no book at
/// all; those are kept rather than dropped, because an open strike with no quote
/// is a real feature of a board and a chain with holes punched in it reads as
/// missing data.
pub fn parse_yahoo_options(
    body: &YahooOptionsResponse,
    symbol: &str,
) -> Result<(OptionBoard, Vec<i64>)> {
    let chain = body.option_chain.as_ref();
    let Some(result) = chain.and_then(|c| c.result.first()) else {
        let described = chain
            .and_then(|c| c.error.as_ref())
            .and_then(|e| e.description.clone())
            .unwrap_or_else(|| format!("Yahoo Finance returned no option board for {symbol}"));
        return Err(UpstreamError::not_found(described).with_hint(
            "Check the symbol. Not every listed name has options — most ETFs and large caps do.",
        ));
    };

    let quote = result.quote.as_ref();
    let mut quotes: Vec<BoardQuote> = Vec::new();

    for slice in &result.options {
        for (rows, option_type) in [
            (&slice.calls, OptionType::Call),
            (&slice.puts, OptionType::Put),
        ] {
            for row in rows {
                let Some(strike) = positive(row.strike) else {
                    continue;
                };
                let Some(expiration) = row.expiration.or(slice.expiration_date) else {
                    continue;
                };

                // Yahoo dates an expiry at UTC midnight; the contract actually
                // stops trading at the close in New York, ~16 hours later. On a
                // same-day chain that difference is the entire remaining life of
                // the option.
                let iso = iso_of(expiration);
                let expiry = equity_expiry_instant(&iso).unwrap_or(expiration);
                let Some(contract) = row
                    .contract_symbol
                    .clone()
                    .or_else(|| occ_symbol(symbol, &iso, option_type, strike))
                else {
                    continue;
                };

                quotes.push(BoardQuote {
                    contract,
                    option_type,
                    strike,
                    expiry,
                    bid: positive(row.bid),
                    ask: positive(row.ask),
                    last: positive(row.last_price),
                    mark: None,
                    change: finite(row.change),
                    volume: finite(row.volume),
                    open_interest: finite(row.open_interest),
                    venue_iv: None,
                    venue_forward: None,
                    venue_discount: None,
                });
            }
        }
    }

    let board = OptionBoard {
        symbol: result
            .underlying_symbol
            .clone()
            .unwrap_or_else(|| symbol.to_owned())
            .to_uppercase(),
        name: quote
            .and_then(|q| q.long_name.clone().or_else(|| q.short_name.clone()))
            .unwrap_or_else(|| symbol.to_uppercase()),
        asset_class: AssetClass::Stock,
        currency: quote
            .and_then(|q| q.currency.clone())
            .unwrap_or_else(|| "USD".to_owned()),
        venue: "OPRA".to_owned(),
        source: "yahoo".to_owned(),
        contract_size: CONTRACT_SIZE,
        spot: positive(quote.and_then(|q| q.regular_market_price)),
        quotes,
        note: String::new(),
    };

    Ok((board, result.expiration_dates.clone()))
}

/* ------------------------------------------------------------------ nasdaq */

#[derive(Debug, Default, Deserialize)]
pub struct NasdaqChainResponse {
    #[serde(default)]
    pub data: Option<NasdaqChainData>,
}

#[derive(Debug, Default, Deserialize)]
pub struct NasdaqChainData {
    #[serde(default, rename = "totalRecord")]
    pub total_record: Option<usize>,
    #[serde(default, rename = "lastTrade")]
    pub last_trade: Option<String>,
    #[serde(default)]
    pub table: Option<NasdaqTable>,
}

#[derive(Debug, Default, Deserialize)]
pub struct NasdaqTable {
    #[serde(default)]
    pub rows: Vec<NasdaqChainRow>,
}

#[derive(Debug, Default, Clone, Deserialize)]
pub struct NasdaqChainRow {
    #[serde(default)]
    pub expirygroup: Option<String>,
    #[serde(default)]
    pub strike: Option<String>,
    #[serde(default, rename = "c_Last")]
    pub c_last: Option<String>,
    #[serde(default, rename = "c_Change")]
    pub c_change: Option<String>,
    #[serde(default, rename = "c_Bid")]
    pub c_bid: Option<String>,
    #[serde(default, rename = "c_Ask")]
    pub c_ask: Option<String>,
    #[serde(default, rename = "c_Volume")]
    pub c_volume: Option<String>,
    #[serde(default, rename = "c_Openinterest")]
    pub c_openinterest: Option<String>,
    #[serde(default, rename = "p_Last")]
    pub p_last: Option<String>,
    #[serde(default, rename = "p_Change")]
    pub p_change: Option<String>,
    #[serde(default, rename = "p_Bid")]
    pub p_bid: Option<String>,
    #[serde(default, rename = "p_Ask")]
    pub p_ask: Option<String>,
    #[serde(default, rename = "p_Volume")]
    pub p_volume: Option<String>,
    #[serde(default, rename = "p_Openinterest")]
    pub p_openinterest: Option<String>,
}

/// `"September 18, 2026"` → `"2026-09-18"`.
#[must_use]
pub fn parse_expiry_group(label: Option<&str>) -> Option<String> {
    let label = label?.trim();
    if label.is_empty() {
        return None;
    }
    let (month, rest) = label.split_once(char::is_whitespace)?;
    let (day, year) = rest.split_once(',')?;
    let index = MONTHS.iter().position(|m| *m == month.to_lowercase())?;
    let day: u32 = day.trim().parse().ok()?;
    let year: u32 = year.trim().parse().ok()?;
    if !(1..=31).contains(&day) {
        return None;
    }
    Some(format!("{year:04}-{:02}-{day:02}", index + 1))
}

/// `"LAST TRADE: $305.93 (AS OF AUG 13, 2026)"` → `305.93`.
#[must_use]
pub fn parse_last_trade(raw: Option<&str>) -> Option<f64> {
    let raw = raw?;
    let after = raw.split_once('$')?.1;
    let number: String = after
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == ',' || *c == '.')
        .collect();
    parse_nasdaq_number(Some(&number))
}

/// One page of Nasdaq's flat table → board quotes.
///
/// The table interleaves group-header rows (`expirygroup` set, everything else
/// null) with data rows that carry only an abbreviated `expiryDate` like
/// `"Sep 18"` — no year. The header is therefore the *only* place the year
/// exists, so parsing statefully is not a shortcut here, it is the only correct
/// reading. A data row before any header is dropped rather than guessed at.
#[must_use]
pub fn parse_nasdaq_chain(
    body: &NasdaqChainResponse,
    symbol: &str,
) -> (Vec<BoardQuote>, Vec<String>, Option<f64>) {
    let rows = body
        .data
        .as_ref()
        .and_then(|d| d.table.as_ref())
        .map(|t| t.rows.as_slice())
        .unwrap_or_default();

    let mut quotes: Vec<BoardQuote> = Vec::new();
    let mut dates: Vec<String> = Vec::new();
    let mut current_date: Option<String> = None;

    for row in rows {
        if let Some(group) = parse_expiry_group(row.expirygroup.as_deref()) {
            if !dates.contains(&group) {
                dates.push(group.clone());
            }
            current_date = Some(group);
            continue;
        }

        let Some(strike) = parse_nasdaq_number(row.strike.as_deref()).filter(|s| *s > 0.0) else {
            continue;
        };
        let Some(date) = current_date.as_deref() else {
            continue;
        };
        let Some(expiry) = equity_expiry_instant(date) else {
            continue;
        };

        for (option_type, last, change, bid, ask, volume, oi) in [
            (
                OptionType::Call,
                &row.c_last,
                &row.c_change,
                &row.c_bid,
                &row.c_ask,
                &row.c_volume,
                &row.c_openinterest,
            ),
            (
                OptionType::Put,
                &row.p_last,
                &row.p_change,
                &row.p_bid,
                &row.p_ask,
                &row.p_volume,
                &row.p_openinterest,
            ),
        ] {
            let Some(contract) = occ_symbol(symbol, date, option_type, strike) else {
                continue;
            };
            quotes.push(BoardQuote {
                contract,
                option_type,
                strike,
                expiry,
                bid: positive(parse_nasdaq_number(bid.as_deref())),
                ask: positive(parse_nasdaq_number(ask.as_deref())),
                last: positive(parse_nasdaq_number(last.as_deref())),
                mark: None,
                change: finite(parse_nasdaq_number(change.as_deref())),
                volume: finite(parse_nasdaq_number(volume.as_deref())),
                open_interest: finite(parse_nasdaq_number(oi.as_deref())),
                venue_iv: None,
                venue_forward: None,
                venue_discount: None,
            });
        }
    }

    let spot = parse_last_trade(body.data.as_ref().and_then(|d| d.last_trade.as_deref()));
    (quotes, dates, spot)
}

fn chain_path(symbol: &str, asset_class: &str, limit: usize, offset: usize) -> String {
    format!(
        "/api/quote/{}/option-chain?assetclass={asset_class}&limit={limit}&offset={offset}\
         &fromdate=all&excode=oprac&callput=callput&money=all&type=all",
        urlencoding::encode(symbol)
    )
}

/// Nasdaq needs the right `assetclass`; there is no lookup, so probe.
///
/// The distinction the loop keeps track of is the one that matters to a reader.
/// Three probes that each came back with a well-formed body listing nothing mean
/// the symbol has no board — `not_found`, and the caller stops there. Three
/// probes that never got an answer mean Nasdaq is down, which is not a statement
/// about the symbol at all: reporting that as "Nasdaq lists no option chain for
/// AAPL" would tell a reader that Apple has no listed options, and would also
/// stop the caller from reporting the outage it actually hit.
async fn nasdaq_asset_class(state: &AppState, symbol: &str) -> Result<&'static str> {
    let mut answered = false;
    let mut last_error: Option<UpstreamError> = None;

    for asset_class in ["stocks", "etf", "index"] {
        let path = chain_path(symbol, asset_class, 10, 0);
        let url = format!(
            "{}{path}",
            state.config().nasdaq_api_base.trim_end_matches('/')
        );
        let body = fetch::<NasdaqChainResponse>(
            state,
            url,
            format!("nasdaq:optprobe:{symbol}:{asset_class}"),
            ttl::META,
        )
        .await;

        match body {
            Ok(body) => {
                answered = true;
                let rows = body
                    .data
                    .as_ref()
                    .and_then(|d| d.table.as_ref())
                    .is_some_and(|t| !t.rows.is_empty());
                if rows {
                    return Ok(asset_class);
                }
            }
            Err(error) => last_error = Some(error),
        }
    }

    if !answered {
        let mut error = last_error.unwrap_or_else(|| {
            UpstreamError::new(
                "Nasdaq did not answer an option-chain probe",
                "upstream_error",
            )
        });
        error.hint = Some(
            "Nasdaq refused every option-chain probe, so this says nothing about whether the \
             symbol has listed options."
                .to_owned(),
        );
        return Err(error);
    }

    Err(
        UpstreamError::not_found(format!("Nasdaq lists no option chain for {symbol}")).with_hint(
            "Nasdaq covers US equities, ETFs and its own indices. Not every name has listed \
             options.",
        ),
    )
}

/// Walk Nasdaq's paged table into one board.
///
/// Nasdaq has no endpoint that lists expiries, so the crawl is how the strip
/// gets built — but it also collects every quote on the way, which means the
/// volatility term structure comes free instead of costing one request per
/// expiry. The page cap is a bound on a very wide name (SPY lists tens of
/// thousands of contracts); when it bites, the response says so rather than
/// presenting a truncated board as the whole one.
async fn nasdaq_board(state: &AppState, symbol: &str) -> Result<OptionBoard> {
    let asset_class = nasdaq_asset_class(state, symbol).await?;

    let mut quotes: Vec<BoardQuote> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut spot: Option<f64> = None;
    let mut truncated = false;
    let mut total = 0usize;

    for page in 0..NASDAQ_MAX_PAGES {
        let path = chain_path(symbol, asset_class, NASDAQ_PAGE, page * NASDAQ_PAGE);
        let url = format!(
            "{}{path}",
            state.config().nasdaq_api_base.trim_end_matches('/')
        );
        let body = fetch::<NasdaqChainResponse>(
            state,
            url,
            format!("nasdaq:optchain:{symbol}:{asset_class}:{page}"),
            ttl::QUOTE,
        )
        .await?;

        let (page_quotes, _, page_spot) = parse_nasdaq_chain(&body, symbol);
        if spot.is_none() {
            spot = page_spot;
        }
        if let Some(reported) = body.data.as_ref().and_then(|d| d.total_record) {
            total = reported;
        }

        for quote in page_quotes {
            if seen.insert(quote.contract.clone()) {
                quotes.push(quote);
            }
        }

        let returned = body
            .data
            .as_ref()
            .and_then(|d| d.table.as_ref())
            .map_or(0, |t| t.rows.len());
        if returned < NASDAQ_PAGE {
            break;
        }
        if page == NASDAQ_MAX_PAGES - 1 && (page + 1) * NASDAQ_PAGE < total {
            truncated = true;
        }
    }

    let note = if truncated {
        format!(
            "Nasdaq paginates the chain; this board is the first {} of {total} rows, so the \
             longest-dated expiries are not shown.",
            NASDAQ_MAX_PAGES * NASDAQ_PAGE
        )
    } else {
        String::new()
    };

    Ok(OptionBoard {
        symbol: symbol.to_uppercase(),
        name: symbol.to_uppercase(),
        asset_class: AssetClass::Stock,
        currency: "USD".to_owned(),
        venue: "OPRA".to_owned(),
        source: "nasdaq".to_owned(),
        contract_size: CONTRACT_SIZE,
        spot,
        quotes,
        note,
    })
}

/* ------------------------------------------------------------------ public */

/// `YYYY-MM-DD` for a unix instant, UTC.
fn iso_of(seconds: i64) -> String {
    crate::sources::optionboard::iso_date_of(seconds)
}

/// The board for one underlying.
///
/// `wanted` names extra expiries to load, as `YYYY-MM-DD`. Yahoo serves one
/// expiry per request, so this bounds the fan-out; Nasdaq's crawl returns
/// everything at once and simply ignores it. An empty list means the front
/// expiry alone.
pub async fn get_board(
    state: &AppState,
    symbol: &str,
    wanted: &[String],
) -> Result<(OptionBoard, Vec<String>)> {
    let upper = symbol.trim().to_uppercase();

    let primary = yahoo_board(state, &upper, wanted).await;
    let primary_error = match primary {
        Ok(found) => return Ok(found),
        Err(error) if error.code == "not_found" => return Err(error),
        Err(error) => error,
    };

    match nasdaq_board(state, &upper).await {
        Ok(mut board) => {
            if board.spot.is_none() {
                board.spot = get_quote(state, &upper).await.ok().and_then(|q| q.price);
            }
            let mut dates: Vec<String> = board
                .quotes
                .iter()
                .map(|q| iso_of(q.expiry))
                .collect::<HashSet<_>>()
                .into_iter()
                .collect();
            dates.sort();
            Ok((board, dates))
        }
        Err(error) if error.code == "not_found" => Err(error),
        Err(_) => {
            let hint = if primary_error.status == Some(429) {
                "Yahoo Finance is rate-limiting this IP — its `v7` option endpoint is blocked \
                 from shared datacentre addresses more aggressively than the price endpoint is — \
                 and the Nasdaq fallback did not answer either. The same request usually succeeds \
                 from a residential connection."
                    .to_owned()
            } else {
                format!(
                    "Yahoo Finance and Nasdaq both refused an option chain for {upper}. Check the \
                     symbol, and note that not every listed name has options."
                )
            };
            Err(UpstreamError::new(
                format!("No provider could serve an option chain for {upper}"),
                "upstream_error",
            )
            .with_hint(hint))
        }
    }
}

async fn yahoo_board(
    state: &AppState,
    symbol: &str,
    wanted: &[String],
) -> Result<(OptionBoard, Vec<String>)> {
    let base = state
        .config()
        .yahoo_api_base
        .trim_end_matches('/')
        .to_owned();
    let encoded = urlencoding::encode(symbol).into_owned();

    // The undated call returns the front expiry *and* the full expiry list, so
    // it is both the cheapest probe and the one that builds the picker.
    let first = fetch::<YahooOptionsResponse>(
        state,
        format!("{base}/v7/finance/options/{encoded}"),
        format!("yahoo:options:{symbol}"),
        ttl::QUOTE,
    )
    .await?;
    let (mut board, expirations) = parse_yahoo_options(&first, symbol)?;

    let by_date: HashMap<String, i64> = expirations.iter().map(|e| (iso_of(*e), *e)).collect();
    let expiry_dates: Vec<String> = expirations.iter().map(|e| iso_of(*e)).collect();
    let loaded: HashSet<String> = board.quotes.iter().map(|q| iso_of(q.expiry)).collect();

    for date in wanted {
        let Some(stamp) = by_date.get(date) else {
            continue;
        };
        if loaded.contains(date) {
            continue;
        }
        let extra = fetch::<YahooOptionsResponse>(
            state,
            format!("{base}/v7/finance/options/{encoded}?date={stamp}"),
            format!("yahoo:options:{symbol}:{stamp}"),
            ttl::QUOTE,
        )
        .await;
        if let Ok(body) = extra {
            if let Ok((slice, _)) = parse_yahoo_options(&body, symbol) {
                board.quotes.extend(slice.quotes);
            }
        }
    }

    if board.spot.is_none() {
        board.spot = get_quote(state, symbol).await.ok().and_then(|q| q.price);
    }

    Ok((board, expiry_dates))
}

/// Equity options exist for far too many names to enumerate; this is a hint
/// list.
#[must_use]
pub fn underlying_hints() -> Vec<(&'static str, &'static str)> {
    vec![
        ("SPY", "SPDR S&P 500 ETF"),
        ("QQQ", "Invesco QQQ Trust"),
        ("IWM", "iShares Russell 2000 ETF"),
        ("AAPL", "Apple"),
        ("NVDA", "NVIDIA"),
        ("TSLA", "Tesla"),
        ("MSFT", "Microsoft"),
        ("AMZN", "Amazon"),
    ]
}

#[cfg(test)]
mod tests {
    //! Equity option parser and wire tests.
    //!
    //! The Nasdaq fixture was captured live from `api.nasdaq.com` for AAPL and
    //! trimmed to the first two expiry groups, which is the smallest slice that
    //! still contains the trap the parser exists for: a group header carrying
    //! the year, followed by data rows that do not.

    use super::*;
    use wiremock::matchers::{method, path as path_matcher};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use crate::config::Config;

    const NASDAQ_CHAIN: &str = include_str!("fixtures/nasdaq_optionchain.json");

    fn chain() -> NasdaqChainResponse {
        serde_json::from_str(NASDAQ_CHAIN).expect("the captured fixture parses")
    }

    fn state_for(server: &MockServer) -> AppState {
        AppState::new(Config {
            nasdaq_api_base: server.uri(),
            // Point Yahoo at the same server, which serves no `/v7` route, so
            // the fallback is what answers — the arrangement this deployment is
            // actually in, since Yahoo 429s datacentre addresses.
            yahoo_api_base: server.uri(),
            ..Config::default()
        })
    }

    /* -------------------------------------------------------------- OCC symbols */

    #[test]
    fn builds_the_occ_symbol_the_whole_terminal_keys_on() {
        assert_eq!(
            occ_symbol("AAPL", "2026-09-18", OptionType::Call, 300.0).as_deref(),
            Some("AAPL260918C00300000")
        );
        // Thousandths, zero-padded to eight, is what makes a $0.50 strike and a
        // $500 strike sort and compare correctly as text.
        assert_eq!(
            occ_symbol("F", "2026-01-16", OptionType::Put, 0.5).as_deref(),
            Some("F260116P00000500")
        );
        assert_eq!(
            occ_symbol("BRK.B", "2026-01-16", OptionType::Call, 500.0).as_deref(),
            Some("BRKB260116C00500000")
        );
    }

    #[test]
    fn round_trips_an_occ_symbol() {
        for (symbol, date, option_type, strike) in [
            ("AAPL", "2026-09-18", OptionType::Call, 300.0),
            ("SPY", "2026-12-31", OptionType::Put, 612.5),
            ("F", "2026-01-16", OptionType::Put, 0.5),
        ] {
            let built = occ_symbol(symbol, date, option_type, strike).expect("builds");
            let parsed = parse_occ(&built).expect("parses back");
            assert_eq!(parsed.symbol, symbol.replace('.', ""));
            assert_eq!(parsed.date, date);
            assert_eq!(parsed.option_type, option_type);
            assert!((parsed.strike - strike).abs() < 1e-9, "{built}");
        }
    }

    #[test]
    fn refuses_something_that_is_not_an_occ_symbol() {
        for bad in [
            "BTC-25DEC26-104000-C",
            "AAPL",
            "AAPL260918X00300000",
            "AAPL260918C0030000",
            "",
        ] {
            assert!(parse_occ(bad).is_none(), "accepted {bad:?}");
        }
    }

    /* ------------------------------------------------------------ the table */

    #[test]
    fn reads_the_year_from_the_group_header_because_the_rows_do_not_carry_one() {
        // The whole reason this parser is stateful. A data row says `"Aug 24"`
        // and nothing else; the year exists only in the header above it.
        assert_eq!(
            parse_expiry_group(Some("August 24, 2026")).as_deref(),
            Some("2026-08-24")
        );
        assert_eq!(
            parse_expiry_group(Some("September 18, 2026")).as_deref(),
            Some("2026-09-18")
        );
        assert_eq!(parse_expiry_group(Some("")), None);
        assert_eq!(parse_expiry_group(Some("Augus 24, 2026")), None);
        assert_eq!(parse_expiry_group(None), None);
    }

    #[test]
    fn drops_a_data_row_that_arrives_before_any_header_rather_than_guessing() {
        let orphan = NasdaqChainResponse {
            data: Some(NasdaqChainData {
                total_record: Some(1),
                last_trade: None,
                table: Some(NasdaqTable {
                    rows: vec![NasdaqChainRow {
                        strike: Some("200.00".to_owned()),
                        c_bid: Some("1.00".to_owned()),
                        ..NasdaqChainRow::default()
                    }],
                }),
            }),
        };
        let (quotes, dates, _) = parse_nasdaq_chain(&orphan, "AAPL");
        assert!(quotes.is_empty(), "a row with no year is not datable");
        assert!(dates.is_empty());
    }

    #[test]
    fn parses_both_legs_off_one_row() {
        let (quotes, dates, spot) = parse_nasdaq_chain(&chain(), "AAPL");
        assert_eq!(dates.len(), 2, "the fixture holds two expiry groups");
        assert!(!quotes.is_empty());
        // Nasdaq puts the call and the put at one strike on a single row, so a
        // parsed board has both legs at every strike.
        let calls = quotes
            .iter()
            .filter(|q| q.option_type == OptionType::Call)
            .count();
        let puts = quotes
            .iter()
            .filter(|q| q.option_type == OptionType::Put)
            .count();
        assert_eq!(calls, puts);
        assert_eq!(spot, Some(309.35), "the last trade line carries the spot");
    }

    #[test]
    fn reads_a_missing_number_as_absent_and_not_as_zero() {
        // Nasdaq writes `"--"` for a figure it has none of. A dash read as zero
        // would put a zero bid on a contract nobody is bidding for, which is a
        // real and very different statement.
        let (quotes, _, _) = parse_nasdaq_chain(&chain(), "AAPL");
        assert!(
            quotes.iter().any(|q| q.bid.is_none()),
            "the fixture should contain a `--` bid"
        );
        assert!(
            quotes.iter().any(|q| q.volume.is_none()),
            "the fixture should contain a `--` volume"
        );
        assert!(
            quotes.iter().all(|q| q.bid != Some(0.0)),
            "no dash should have become a zero"
        );
    }

    #[test]
    fn stamps_an_expiry_at_the_close_in_new_york_rather_than_at_utc_midnight() {
        let (quotes, dates, _) = parse_nasdaq_chain(&chain(), "AAPL");
        let first = &dates[0];
        let expected = equity_expiry_instant(first).expect("the date is real");
        let quote = quotes.first().expect("the fixture has quotes");
        assert_eq!(quote.expiry, expected);
        // 20:00 or 21:00 UTC depending on daylight time — never 00:00, which is
        // what dating an expiry at UTC midnight would give.
        let seconds_into_day = quote.expiry.rem_euclid(86_400);
        assert!(
            seconds_into_day == 20 * 3_600 || seconds_into_day == 21 * 3_600,
            "expiry landed {seconds_into_day}s into the day"
        );
    }

    #[test]
    fn reads_the_spot_out_of_the_last_trade_sentence() {
        assert_eq!(
            parse_last_trade(Some("LAST TRADE: $305.93 (AS OF AUG 13, 2026)")),
            Some(305.93)
        );
        // A thousands separator is the case that breaks a naive parse.
        assert_eq!(
            parse_last_trade(Some("LAST TRADE: $1,234.50 (AS OF AUG 13, 2026)")),
            Some(1234.50)
        );
        assert_eq!(parse_last_trade(Some("LAST TRADE: unavailable")), None);
        assert_eq!(parse_last_trade(None), None);
    }

    #[test]
    fn keys_every_quote_on_an_occ_symbol_so_a_row_opens_in_either_provider() {
        let (quotes, _, _) = parse_nasdaq_chain(&chain(), "AAPL");
        for quote in quotes.iter().take(20) {
            let parsed = parse_occ(&quote.contract)
                .unwrap_or_else(|| panic!("{} is not an OCC symbol", quote.contract));
            assert_eq!(parsed.symbol, "AAPL");
            assert_eq!(parsed.option_type, quote.option_type);
            assert!((parsed.strike - quote.strike).abs() < 1e-9);
        }
    }

    /* ------------------------------------------------------------------ yahoo */

    #[test]
    fn takes_no_implied_volatility_from_a_provider_that_publishes_one() {
        // Yahoo publishes an IV against its own undisclosed carry. Mixing it
        // with a forward fitted from parity here would put two disagreeing
        // volatilities on one screen and Greeks that match neither.
        let body: YahooOptionsResponse = serde_json::from_str(
            r#"{"optionChain":{"result":[{"underlyingSymbol":"AAPL",
                "expirationDates":[1789660800],
                "quote":{"regularMarketPrice":309.35,"shortName":"Apple Inc.","currency":"USD"},
                "options":[{"expirationDate":1789660800,
                  "calls":[{"contractSymbol":"AAPL260918C00300000","strike":300.0,
                            "lastPrice":15.2,"bid":15.0,"ask":15.4,"volume":120,
                            "openInterest":4000,"impliedVolatility":0.31,
                            "expiration":1789660800}],
                  "puts":[]}]}]}}"#,
        )
        .expect("the payload parses");

        let (board, expirations) = parse_yahoo_options(&body, "AAPL").expect("it builds a board");
        assert_eq!(expirations, vec![1_789_660_800]);
        assert_eq!(board.quotes.len(), 1);
        let quote = &board.quotes[0];
        assert_eq!(quote.venue_iv, None, "Yahoo's IV is deliberately unread");
        assert_eq!(quote.contract, "AAPL260918C00300000");
        assert_eq!(quote.bid, Some(15.0));
        assert_eq!(board.spot, Some(309.35));
        assert_eq!(board.name, "Apple Inc.");
    }

    #[test]
    fn refuses_a_yahoo_body_with_no_board_rather_than_returning_an_empty_one() {
        let body: YahooOptionsResponse = serde_json::from_str(
            r#"{"optionChain":{"result":[],"error":{"description":"No data found"}}}"#,
        )
        .expect("parses");
        let error = parse_yahoo_options(&body, "NOPE").expect_err("no board is not an empty board");
        assert_eq!(error.code, "not_found");
        assert!(error.message.contains("No data found"));
    }

    /* ------------------------------------------------------------------- wire */

    #[tokio::test]
    async fn falls_back_to_nasdaq_when_yahoo_will_not_answer() {
        // The live arrangement: Yahoo's `v7` option host 429s shared datacentre
        // addresses, which is exactly what this deployment sits behind.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/v7/finance/options/AAPL"))
            .respond_with(ResponseTemplate::new(429))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path_matcher("/api/quote/AAPL/option-chain"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(NASDAQ_CHAIN, "application/json"))
            .mount(&server)
            .await;

        let (board, dates) = get_board(&state_for(&server), "aapl", &[])
            .await
            .expect("the fallback answers");

        assert_eq!(board.source, "nasdaq");
        assert_eq!(board.symbol, "AAPL");
        assert_eq!(board.contract_size, 100.0);
        assert_eq!(board.spot, Some(309.35));
        assert_eq!(dates.len(), 2);
        assert!(!board.quotes.is_empty());
    }

    #[tokio::test]
    async fn says_which_two_providers_refused_when_neither_answers() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;

        let error = get_board(&state_for(&server), "AAPL", &[])
            .await
            .expect_err("neither provider answered");
        // `not_found` would be a lie — the symbol may be perfectly real.
        assert_eq!(error.code, "upstream_error");
        assert!(error.message.contains("AAPL"));
    }
}
