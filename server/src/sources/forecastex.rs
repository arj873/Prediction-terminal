//! ForecastEx (forecastex.com) client — the CFTC-regulated exchange behind
//! Interactive Brokers' ForecastTrader.
//!
//! Two public surfaces, both unauthenticated, and the terminal reads both:
//!
//! ```text
//! forecastex.com/api/*        catalogue, products and price samples. Real
//!                             JSON, but undocumented and Lambda-backed, and
//!                             every response is double-wrapped — `body.data`
//!                             is a JSON *string* that has to be parsed again.
//! forecastex-public-data.s3   two years of daily CSVs: the whole-exchange
//!                             print tape and the end-of-session archive.
//! ```
//!
//! What it never publishes is a book. ForecastEx matches by *pairing* — a print
//! creates one YES holder and one NO holder at the same moment — so there is no
//! bid, no ask and no ladder anywhere in public, and [`get_order_book`] says
//! that rather than dressing the last print up as a one-level ladder. For the
//! same reason `last_yes_price` and `last_no_price` are two independent prints
//! (over a full crawl on 2026-08-23 they summed to 1.00 on 4,153 contracts and
//! to 1.01 on another 848), so neither is ever derived from the other.
//!
//! Turnover and the daily move are not in the live catalogue at all: they come
//! from the end-of-session archive, which is published after 16:30 CT. Every
//! figure this module takes from there is therefore a session behind the tape,
//! and each place it lands says so.
//!
//! The identifier trap is the expensive one. `/api/contracts` and
//! `/api/products` are case-insensitive, but `/api/prices` is case-sensitive and
//! fails *silently* — an upper-cased id answers HTTP 200 with an empty series,
//! which draws a blank chart instead of raising anything. Confirmed live:
//! `?contractId=HORC_1126_Republican` returns 502 daily samples and
//! `?contractId=HORC_1126_REPUBLICAN` returns `[]` with `total_days: 0`. The
//! registry folds ids to upper before this module is called, so the canonical
//! spelling is restored from the warm catalogue, or from one case-insensitive
//! contract lookup, before any price call goes out.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use csv::StringRecord;
use regex::Regex;
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::Value;
use terminal_core::types::{
    Candle, CandleInterval, CandlesResponse, Market, MarketStatus, OrderBook, SeriesInfo,
    StrikeType, Trade, TradesResponse, Venue, VenueEvent,
};
use terminal_core::util::round4;
use terminal_core::venue::MoverSort;
use time::format_description::well_known::Rfc3339;
use time::{Date, Month, OffsetDateTime};

use crate::app::AppState;
use crate::cache::ttl;
use crate::error::{codes, Result, UpstreamError};
use crate::http::FetchOptions;
use crate::sources::corpus::{
    rank_markets, refuse_overlong_query, search_corpus, Corpus, SearchResponse,
};

const VENUE: Venue = Venue::ForecastEx;

/* --------------------------------------------------------------- coercion */

/// Read a figure that arrives as a JSON number, a CSV string, or not at all.
///
/// Everything that is not a stated figure has to come back `None`. The hazard is
/// the same one JavaScript's `Number` has and Rust's `parse` mostly avoids: a
/// blank `settlement_price` cell — 54.0% of the archive on 2026-08-23 — must not
/// settle every one of those contracts at zero, which is the difference between
/// "the exchange says nothing" and "the exchange says worthless". Only a number
/// or a non-blank numeric string counts, and a bare `true` or an empty array
/// counts as neither.
pub fn num(value: &Value) -> Option<f64> {
    match value {
        Value::Number(number) => number.as_f64().filter(|n| n.is_finite()),
        Value::String(text) => cell(text),
        _ => None,
    }
}

/// [`num`] over a field the upstream may omit altogether.
fn num_of(value: Option<&Value>) -> Option<f64> {
    value.and_then(num)
}

/// Read one CSV cell as a figure, keeping "blank" and "zero" apart.
///
/// A blank cell is the exchange declining to state a figure, never a statement
/// that the figure is nothing. A cell holding only whitespace is blank too:
/// trimming first and refusing the empty result is what stops `" "` from
/// becoming `0.0` the way `Number(' ')` does.
pub fn cell(text: &str) -> Option<f64> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    trimmed.parse::<f64>().ok().filter(|n| n.is_finite())
}

/* ------------------------------------------------------------- US Central */

/// ForecastEx keeps its clock in Chicago and mostly forgets to say so.
///
/// `expiration_date`, `last_trade_date` and the archive's daily `date` all
/// arrive as bare wall-clock strings with no zone, while the CSVs give the very
/// same instants an explicit `-05:00`/`-06:00`. Left bare they would be read as
/// whatever zone the reader happens to be in — a five-hour error on every close
/// time in London — so the offset is computed and stamped on here.
///
/// Neither `chrono-tz` nor any other zone database is a dependency of this
/// workspace, and one is not added for a single zone: the two US Central offsets
/// and the federal rule that switches between them are written out below, and
/// [`tests`] checks them on both sides of both 2026 transitions.
const CENTRAL_STANDARD: i64 = -6 * 3600;
const CENTRAL_DAYLIGHT: i64 = -5 * 3600;

/// The second Sunday in March at 02:00 CST, as a unix instant.
///
/// 02:00 local standard is 08:00 UTC, because the clock is still six hours
/// behind at the moment it jumps.
fn daylight_begins(year: i32) -> i64 {
    nth_sunday(year, Month::March, 2) + 8 * 3600
}

/// The first Sunday in November at 02:00 CDT, as a unix instant.
///
/// 02:00 local daylight is 07:00 UTC: the clock is five hours behind right up to
/// the moment it falls back.
fn daylight_ends(year: i32) -> i64 {
    nth_sunday(year, Month::November, 1) + 7 * 3600
}

/// Midnight UTC on the `nth` Sunday of a month, as a unix instant.
fn nth_sunday(year: i32, month: Month, nth: u8) -> i64 {
    let Ok(first) = Date::from_calendar_date(year, month, 1) else {
        return 0;
    };
    let skip = (7 - first.weekday().number_days_from_sunday()) % 7;
    let day = 1 + skip + (nth - 1) * 7;
    Date::from_calendar_date(year, month, day)
        .map(|date| date.midnight().assume_utc().unix_timestamp())
        .unwrap_or(0)
}

/// Seconds Central is behind UTC at an instant — negative all year.
///
/// The year is read in UTC rather than in Central. The two disagree only for the
/// six hours either side of 1 January, and Central is on standard time in both
/// years' windows across that whole gap, so the answer is the same either way.
fn central_offset(at: i64) -> i64 {
    let year = OffsetDateTime::from_unix_timestamp(at)
        .map(|moment| moment.year())
        .unwrap_or(1970);

    if at >= daylight_begins(year) && at < daylight_ends(year) {
        CENTRAL_DAYLIGHT
    } else {
        CENTRAL_STANDARD
    }
}

/// A wall clock, as the exchange writes one: `YYYY-MM-DD` with an optional
/// `THH:MM[:SS]` after it.
struct WallClock {
    year: i32,
    month: u8,
    day: u8,
    hour: u8,
    minute: u8,
    second: u8,
}

/// Read `YYYY-MM-DD[T ]HH:MM[:SS]`, ignoring anything trailing.
///
/// Hand-rolled rather than handed to a format description because the time half
/// is optional and the seconds inside it are optional again: the archive's daily
/// `date` column is a bare date where `expiration_date` carries a full clock,
/// and both reach this function.
fn read_wall_clock(local: &str) -> Option<WallClock> {
    let text = local.trim().as_bytes();
    let digits = |from: usize, len: usize| -> Option<u32> {
        let slice = text.get(from..from + len)?;
        if !slice.iter().all(u8::is_ascii_digit) {
            return None;
        }
        std::str::from_utf8(slice).ok()?.parse().ok()
    };

    if text.len() < 10 || text[4] != b'-' || text[7] != b'-' {
        return None;
    }
    let mut clock = WallClock {
        year: digits(0, 4)? as i32,
        month: digits(5, 2)? as u8,
        day: digits(8, 2)? as u8,
        hour: 0,
        minute: 0,
        second: 0,
    };

    if text.len() >= 16 && (text[10] == b'T' || text[10] == b' ') && text[13] == b':' {
        clock.hour = digits(11, 2)? as u8;
        clock.minute = digits(14, 2)? as u8;
        if text.len() >= 19 && text[16] == b':' {
            clock.second = digits(17, 2)? as u8;
        }
    }

    Some(clock)
}

/// The same wall clock read as if it were UTC — the starting point, not an
/// answer.
fn naive_instant(clock: &WallClock) -> Option<i64> {
    let month = Month::try_from(clock.month).ok()?;
    let date = Date::from_calendar_date(clock.year, month, clock.day).ok()?;
    let time = time::Time::from_hms(clock.hour, clock.minute, clock.second).ok()?;
    Some(date.with_time(time).assume_utc().unix_timestamp())
}

/// A Central wall-clock string as a unix instant.
///
/// Two passes, because the offset depends on the instant and the instant depends
/// on the offset: the first pass picks the right side of a DST boundary, the
/// second reads the offset that actually applies there.
pub fn central_instant(local: &str) -> Option<i64> {
    let naive = naive_instant(&read_wall_clock(local)?)?;
    let first = naive - central_offset(naive);
    Some(naive - central_offset(first))
}

/// The same wall clock with its offset spelled out, so the instant is
/// unambiguous. A string this cannot read is returned untouched rather than
/// stamped with an invented zone.
pub fn central_iso(local: &str) -> String {
    let Some(seconds) = central_instant(local) else {
        return local.to_string();
    };
    let minutes = central_offset(seconds) / 60;
    let sign = if minutes <= 0 { '-' } else { '+' };
    let size = minutes.abs();
    format!("{}{sign}{:02}:{:02}", local, size / 60, size % 60)
}

/// The session day an instant falls in, as the archive names its files.
///
/// The exchange's day runs 16:15 CT to 16:14 CT, so a session is filed under the
/// date it *ends* on: prints made on Monday evening belong to Tuesday's tape.
/// Asking for the calendar date instead fetches an empty file every evening.
/// Confirmed live at 18:39 CT on Sunday 2026-08-23, when `pairs_20260824.csv`
/// already held 141 KB of that evening's prints and
/// `prices/daily_prices_20260824.csv` was still a 404.
pub fn session_date(at: i64) -> String {
    let wall = at + central_offset(at);
    let Ok(moment) = OffsetDateTime::from_unix_timestamp(wall) else {
        return String::new();
    };

    let rolled = if moment.hour() > 16 || (moment.hour() == 16 && moment.minute() >= 15) {
        moment.date().next_day().unwrap_or(moment.date())
    } else {
        moment.date()
    };
    stamp(rolled)
}

/// The session before `date` (`YYYYMMDD`), for walking back to a published file.
fn previous_session(date: &str) -> String {
    let Some(clock) = read_wall_clock(&format!(
        "{}-{}-{}",
        date.get(0..4).unwrap_or_default(),
        date.get(4..6).unwrap_or_default(),
        date.get(6..8).unwrap_or_default()
    )) else {
        return date.to_string();
    };
    let Ok(month) = Month::try_from(clock.month) else {
        return date.to_string();
    };
    Date::from_calendar_date(clock.year, month, clock.day)
        .ok()
        .and_then(|day| day.previous_day())
        .map(stamp)
        .unwrap_or_else(|| date.to_string())
}

/// A date as the bucket spells it in a filename: `YYYYMMDD`, no separators.
fn stamp(date: Date) -> String {
    format!(
        "{:04}{:02}{:02}",
        date.year(),
        u8::from(date.month()),
        date.day()
    )
}

/* ------------------------------------------------------------ raw upstream */

/// The Lambda-proxy envelope every `/api/*` response arrives in.
///
/// Two oddities worth knowing before reading [`unwrap`]. `body.data` is a JSON
/// *string*, not an array — the payload is serialised twice. And a rejected
/// request comes back as HTTP 200 with `statusCode: 400` and `body` collapsed to
/// a string holding `{"error": …}`, so the HTTP status is not the one that
/// matters. Both were confirmed live: `?interval=H` answers HTTP 200 carrying
/// `{"statusCode":400,"body":"{\"error\": \"interval must be 'h' or 'd'\"}"}`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RawFexEnvelope {
    pub status_code: Option<i64>,
    /// An object on success and a string on rejection, so it is held loosely and
    /// discriminated in [`unwrap`].
    pub body: Option<Value>,
    /// Present instead of the envelope when the Lambda itself failed.
    pub error_message: Option<String>,
    pub error_type: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct RawFexBody {
    /// A JSON string on every live response. Typed loosely for the day it isn't.
    pub data: Option<Value>,
    pub next_page: Option<i64>,
    pub total_pages: Option<i64>,
    pub page_size: Option<i64>,
    /// Full available depth of a price series, regardless of the window asked
    /// for.
    pub total_days: Option<i64>,
}

/// A catalogue row. These twelve keys are the whole record — there are no
/// others.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct RawFexContract {
    pub contract_id: Option<String>,
    pub product_id: Option<String>,
    pub category: Option<String>,
    pub event_display_name: Option<String>,
    pub question: Option<String>,
    pub expiration_date: Option<String>,
    pub last_trade_date: Option<String>,
    pub payout_date: Option<String>,
    pub open_interest: Option<Value>,
    pub exchange_spec_url: Option<String>,
    /// Independent last prints. `null` means never traded — 6,749 of the 11,750
    /// contracts open on 2026-08-23.
    pub last_yes_price: Option<Value>,
    pub last_no_price: Option<Value>,
}

/// A product row — what every other venue calls a series.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct RawFexProduct {
    pub product_id: Option<String>,
    pub name: Option<String>,
    pub description: Option<String>,
    pub source_agency: Option<String>,
    pub source_agency_url: Option<String>,
    pub category: Option<String>,
    pub calculation_method: Option<String>,
    pub question_template: Option<String>,
    /// `I` `M` `D` `Q` `W` `H`.
    pub frequency_type: Option<String>,
    /// The IBKR/Reuters symbol, e.g. `HORC=MAN`.
    pub external_symbol: Option<String>,
    /// `String` `Decimal` `Percentage` `Integer` `Date` — how the strike segment
    /// reads.
    pub strike_unit: Option<String>,
    pub position_accountability_value: Option<String>,
    pub open_interest: Option<Value>,
}

/// One point of `/api/prices`. A close and a size, never an OHLC bar.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct RawFexPricePoint {
    pub instrument_id: Option<String>,
    pub yes_price: Option<Value>,
    pub no_price: Option<Value>,
    /// ISO with `Z` on the hourly feed; a bare date on the daily one.
    pub interval_date: Option<String>,
    pub volume: Option<Value>,
}

/// One print from `pairs/pairs_YYYYMMDD.csv`, fields as the CSV spells them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RawFexPair {
    pub pair_id: String,
    pub event_contract: String,
    pub expiration_date: String,
    pub quantity: String,
    pub yes_price: String,
    pub no_price: String,
    /// US-Central offset with *nanosecond* fractional seconds.
    pub pair_time: String,
}

/// One row of `prices/daily_prices_YYYYMMDD.csv`, the end-of-session archive.
///
/// `high_price`, `low_price` and `vwap` are carried here and deliberately never
/// read: on an untraded row the file writes all three as `0.00`, which is not a
/// mark, and 7,157 of the 12,593 YES rows in the 2026-08-23 file are untraded.
/// `settlement_price` is likewise unread — it is an empty string on 54.0% of
/// rows, so parsing it unconditionally throws away half the file, and every
/// contract the terminal can reach is unsettled anyway (see [`normalise_market`]
/// on `result`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RawFexArchiveRow {
    pub event_contract: String,
    pub subtype: String,
    pub expiration_date: String,
    pub date: String,
    pub start_price: String,
    pub high_price: String,
    pub low_price: String,
    pub end_price: String,
    pub settlement_price: String,
    pub pair_quantity: String,
    pub open_interest: String,
    pub vwap: String,
}

/// One page of an `/api/*` list, unwrapped.
#[derive(Debug, Clone, Default)]
pub struct ApiPage<T> {
    pub rows: Vec<T>,
    pub next_page: Option<i64>,
}

/// Unwrap the double-wrapped envelope.
///
/// The inner parse is the whole point: the exchange serialises its payload, then
/// serialises the wrapper around it. That is unusual enough that it could be
/// "fixed" in a deploy without notice, so an array arriving where a string is
/// expected is accepted rather than treated as corruption — the shape that
/// breaks is the one nobody would ship on purpose.
pub fn unwrap<T: DeserializeOwned>(envelope: &RawFexEnvelope) -> Result<ApiPage<T>> {
    if let Some(failure) = envelope.error_message.as_deref() {
        return Err(UpstreamError::new(
            format!("ForecastEx failed the request: {failure}"),
            codes::UPSTREAM_ERROR,
        )
        .with_hint(
            "The Lambda behind /api caps its response at 6 MB. Ask for a smaller pageSize \
                 — the catalogue crawl uses 5000, which was 2.9 MB on 2026-08-23.",
        ));
    }

    let status = envelope.status_code.unwrap_or(200);
    let body = envelope.body.as_ref();

    // A rejection collapses `body` to a string holding `{"error": …}` while the
    // HTTP status stays 200, so the envelope's own status is the one to read.
    let collapsed = body.and_then(Value::as_str);
    if collapsed.is_some() || status >= 400 {
        let code = if status == 404 {
            codes::NOT_FOUND
        } else {
            codes::UPSTREAM_STATUS
        };
        let error = UpstreamError::new(
            format!(
                "ForecastEx rejected the request: {}",
                stated_error(collapsed)
            ),
            code,
        );
        return Err(match u16::try_from(status) {
            Ok(status) => error.with_status(status),
            Err(_) => error,
        });
    }

    let parsed: RawFexBody = body
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .map_err(|_| {
            UpstreamError::new(
                "ForecastEx returned an envelope body it does not usually send",
                codes::BAD_UPSTREAM_BODY,
            )
        })?
        .unwrap_or_default();

    let rows = match parsed.data {
        Some(Value::String(text)) => serde_json::from_str::<Value>(&text).map_err(|_| {
            UpstreamError::new(
                "ForecastEx returned a data string that is not JSON",
                codes::BAD_UPSTREAM_BODY,
            )
        })?,
        Some(other) => other,
        None => Value::Null,
    };

    let Value::Array(rows) = rows else {
        return Err(UpstreamError::new(
            "ForecastEx returned an envelope with no data array",
            codes::BAD_UPSTREAM_BODY,
        ));
    };

    Ok(ApiPage {
        // A row the shape has outgrown is dropped rather than costing the page:
        // every field of every raw type here is optional, so this only fires on
        // a row that is not an object at all.
        rows: rows
            .into_iter()
            .filter_map(|row| serde_json::from_value(row).ok())
            .collect(),
        next_page: parsed.next_page,
    })
}

/// The reason inside a collapsed error body, or the body itself when it is not
/// the JSON it usually is.
fn stated_error(body: Option<&str>) -> String {
    let Some(body) = body else {
        return "no reason given".into();
    };
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|parsed| {
            parsed
                .get("error")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_else(|| body.chars().take(200).collect())
}

/* ------------------------------------------------------------ identifiers */

/// The event a contract belongs to: the first two segments of its id.
///
/// Verified against a full crawl on 2026-08-23 — grouping on this prefix and
/// grouping on `(product_id, event_display_name)` both give 1,596 groups. The
/// exchange publishes no event endpoint and no event id, so this prefix *is* the
/// event.
pub fn event_ticker_of(contract_id: &str) -> String {
    contract_id
        .to_uppercase()
        .split('_')
        .take(2)
        .collect::<Vec<_>>()
        .join("_")
}

/// The strike: the *last* segment, not the third.
///
/// Nine Conditional contracts carry four segments —
/// `ZFFCP_091626_E0X0926_3.4` is the Fed-scenario code and then the CPI
/// threshold — and reading index 2 there yields the literal string `E0X0926`,
/// which renders as a strike label and parses as nothing. The last segment is
/// identical to index 2 on every three-segment id, so this is right in both
/// shapes.
pub fn strike_of(contract_id: &str) -> String {
    let parts: Vec<&str> = contract_id.split('_').collect();
    if parts.len() >= 3 {
        parts[parts.len() - 1].to_string()
    } else {
        String::new()
    }
}

/* ------------------------------------------------------------ normalisers */

/// A readable name for a leg whose strike is a code rather than a word.
///
/// Half this exchange's ladders are numeric and label themselves — `Above 80` —
/// but the other half carry codes: the September FOMC book's legs are `E0`,
/// `E25`, `E-25`, `H50` and `M-50`, and a Senate race's are `AH` and `JT`.
/// Printed as they arrive, an event's rungs read as five unrelated tokens, and
/// no cross-venue matcher can pair `E0` with `No change` at the other five
/// brokers — the FOMC ladder, the most compared question here, lined up at every
/// venue except this one.
///
/// The meaning is in the question, which the exchange writes out in full and
/// varies only where the legs differ: *"Will the Fed **leave the rate
/// unchanged** in September 2026?"* against *"Will the Fed **raise the rate
/// 25bps** in September 2026?"*. Removing the words every leg shares, from both
/// ends, leaves exactly the clause that distinguishes it.
///
/// Returns `None` when that leaves any leg with nothing — a single-leg event has
/// no sibling to differ from, and two legs the exchange worded identically (it
/// lists a few races twice, once by party and once by candidate) reduce to a
/// pair of blanks. In both cases the code is a worse label than the question but
/// a better one than nothing.
pub fn distinguishing_clauses(questions: &[String]) -> Option<Vec<String>> {
    if questions.len() < 2 {
        return None;
    }

    let words: Vec<Vec<&str>> = questions
        .iter()
        .map(|q| q.split_whitespace().collect())
        .collect();
    if words.iter().any(|w| w.is_empty()) {
        return None;
    }
    // Compared case-insensitively, so `Will` and `will` count as the same shared
    // word rather than leaving the whole question as its own distinguishing
    // clause.
    let lowered: Vec<Vec<String>> = words
        .iter()
        .map(|w| w.iter().map(|word| word.to_lowercase()).collect())
        .collect();

    let shortest = lowered.iter().map(Vec::len).min().unwrap_or(0);

    let mut head = 0usize;
    while head < shortest && lowered.iter().all(|w| w[head] == lowered[0][head]) {
        head += 1;
    }

    let mut tail = 0usize;
    while head + tail < shortest
        && lowered
            .iter()
            .all(|w| w[w.len() - 1 - tail] == lowered[0][lowered[0].len() - 1 - tail])
    {
        tail += 1;
    }

    let clauses: Vec<String> = words
        .iter()
        .map(|w| {
            w[head..w.len() - tail]
                .join(" ")
                .trim_end_matches([' ', '?', '.', ',', ';', ':'])
                .to_string()
        })
        .collect();

    clauses.iter().all(|c| !c.is_empty()).then_some(clauses)
}

/// The strike as a reader would see it written, e.g. `3.625%`, `91`,
/// `Republican`.
fn strike_label(strike: &str, strike_unit: Option<&str>) -> String {
    if strike.is_empty() {
        return String::new();
    }
    if strike_unit == Some("Percentage") && cell(strike).is_some() {
        format!("{strike}%")
    } else {
        strike.to_string()
    }
}

/// Wording that opens the YES region upward, inclusive of the rung itself.
static AT_LEAST: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\bat least\b").expect("AT_LEAST is a valid regex"));
/// Wording that opens the YES region strictly upward.
static ABOVE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(?:exceeds?|above|greater than|more than|higher than)\b")
        .expect("ABOVE is a valid regex")
});
/// Wording that closes the YES region downward, inclusive of the rung itself.
static AT_MOST: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\bat most\b").expect("AT_MOST is a valid regex"));
/// Wording that closes the YES region strictly downward.
static BELOW: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(?:below|under|less than|lower than|fewer than)\b")
        .expect("BELOW is a valid regex")
});

/// Which way a ladder runs, taken from the question the exchange itself wrote.
///
/// The identifier cannot say. `UHBKF_082326_91` and `ULATL_082326_73` are the
/// same grammar with opposite meanings — one settles YES *above* its strike, the
/// other *below* it — so a rule that reads only the id has to guess, and the
/// obvious guess is that every numeric ladder is a one-sided "exceed". It is
/// not: over the full 2026-08-23 crawl the wording partitions the 9,982 numeric
/// contracts into 9,310 upward ones, **580 downward ones** (`ULATL_082326_73`,
/// "will the lowest temperature in Atlanta be below 73 F"), and none that say
/// both. Calling those 580 upward would draw their ladders inverted and put the
/// YES region on the wrong side of the strike.
///
/// The 92 that state no direction at all — "will exactly one FOMC member
/// dissent", "will Canada's economy enter a recession" — are not ladders, and
/// every strike field stays unstated for them rather than reporting a bound the
/// contract does not have.
fn strike_direction(question: &str) -> Option<StrikeType> {
    if AT_LEAST.is_match(question) {
        return Some(StrikeType::GreaterOrEqual);
    }
    if ABOVE.is_match(question) {
        return Some(StrikeType::Greater);
    }
    if AT_MOST.is_match(question) {
        return Some(StrikeType::LessOrEqual);
    }
    if BELOW.is_match(question) {
        return Some(StrikeType::Less);
    }
    None
}

/// YES and NO leg labels for a ladder rung, in the direction it actually runs.
fn leg_labels(direction: StrikeType, label: &str) -> (String, String) {
    match direction {
        StrikeType::Greater => (format!("Above {label}"), format!("{label} or below")),
        StrikeType::GreaterOrEqual => (format!("{label} or above"), format!("Below {label}")),
        StrikeType::Less => (format!("Below {label}"), format!("{label} or above")),
        StrikeType::LessOrEqual => (format!("{label} or below"), format!("Above {label}")),
        // No `between` ladder exists here — every numeric event found over a full
        // crawl is one-sided — so this arm is unreachable and labels the bound it
        // was given rather than inventing a second one.
        StrikeType::Between => (label.to_string(), "No".into()),
    }
}

/// One contract, quoted from the only two figures the exchange states: the last
/// YES print and the open interest behind it.
///
/// The four book fields are `None` and always will be — there is no book, and a
/// synthesised level would put a ladder on screen that nobody can trade against.
/// `mid` carries the last print instead, which is honest only because the panel
/// labels it as a print; it is not a midpoint of anything.
///
/// The strike fields are filled in only where the last segment parses as a
/// number *and* [`strike_direction`] finds the direction stated in the question.
/// A strike level with no direction behind it is worse than none: it decides
/// which side of the line the YES region sits on, and this venue lists ladders
/// running both ways.
pub fn normalise_market(raw: &RawFexContract, product: Option<&RawFexProduct>) -> Market {
    let contract_id = raw.contract_id.clone().unwrap_or_default();
    // The registry folds ids to upper, so a ticker that round-trips through
    // `get_market` has to be in that case too. The canonical spelling survives in
    // the strike label below, and is restored from the catalogue before any
    // case-sensitive price call.
    let ticker = contract_id.to_uppercase();

    let strike = strike_of(&contract_id);
    let unit = product.and_then(|p| p.strike_unit.as_deref());
    let label = strike_label(&strike, unit);
    let level = if strike.is_empty() {
        None
    } else {
        cell(&strike)
    };
    let question = raw.question.as_deref().unwrap_or_default();
    let direction = level.and_then(|_| strike_direction(question));
    let upward = matches!(
        direction,
        Some(StrikeType::Greater | StrikeType::GreaterOrEqual)
    );
    let (yes_leg, no_leg) = match direction {
        Some(direction) => leg_labels(direction, &label),
        None => (
            if label.is_empty() {
                "Yes".to_string()
            } else {
                label.clone()
            },
            "No".to_string(),
        ),
    };

    let last = num_of(raw.last_yes_price.as_ref());
    let expiry = raw.expiration_date.clone().unwrap_or_default();
    let expires = central_instant(&expiry);
    let now = OffsetDateTime::now_utc().unix_timestamp();

    Market {
        venue: VENUE,
        ticker,
        event_ticker: event_ticker_of(&contract_id),
        series_ticker: raw.product_id.clone().unwrap_or_default(),
        title: raw
            .question
            .clone()
            .or_else(|| raw.event_display_name.clone())
            .unwrap_or_else(|| contract_id.to_uppercase()),
        yes_sub_title: yes_leg,
        no_sub_title: no_leg,
        // Not published. The catalogue holds open contracts only — a settled one
        // leaves it entirely — so this reads `open` on everything reachable, and
        // the expiry check is here for the minutes either side of a close.
        status: match expires {
            Some(expires) if expires <= now => MarketStatus::from("closed"),
            _ => MarketStatus::from("open"),
        },
        market_type: "binary".into(),
        yes_bid: None,
        yes_ask: None,
        no_bid: None,
        no_ask: None,
        mid: last,
        last_price: last,
        // Both come from the end-of-session archive, a session behind the tape.
        // [`apply_session`] fills them in; a market read without it says nothing
        // rather than saying zero.
        previous_price: None,
        change: None,
        // Lifetime turnover is published nowhere. The CSV archive starts
        // 2024-08-01, so summing it would state "since August 2024" under a
        // column header that says "ever".
        volume: None,
        volume24h: None,
        // The one ranking figure the live catalogue does state. A `0` here is the
        // exchange saying zero — 7,047 contracts had no open position at all on
        // 2026-08-23.
        open_interest: num_of(raw.open_interest.as_ref()),
        // Resting depth needs a book, and there is none.
        liquidity: None,
        // No listing timestamp exists anywhere in the API.
        open_time: String::new(),
        close_time: raw
            .last_trade_date
            .as_deref()
            .map(central_iso)
            .unwrap_or_default(),
        expiration_time: if expiry.is_empty() {
            String::new()
        } else {
            central_iso(&expiry)
        },
        // Settled contracts drop out of the catalogue on expiry, so nothing the
        // terminal can fetch has a result yet. The two-year CSV archive does
        // carry settlements, but only for contracts that can no longer be looked
        // up.
        result: String::new(),
        // A link to the contract's terms PDF; the catalogue carries no rules
        // text.
        rules_primary: raw.exchange_spec_url.clone().unwrap_or_default(),
        category: raw.category.clone().filter(|c| !c.is_empty()),
        strike_type: direction,
        floor_strike: if upward { level } else { None },
        // Only one bound is ever stated: every numeric event here is a one-sided
        // ladder, so a rung bounds the YES region below or above it, never both.
        cap_strike: match direction {
            Some(StrikeType::Less | StrikeType::LessOrEqual) => level,
            _ => None,
        },
    }
}

/// Merge the end-of-session archive into a live market.
///
/// This is where turnover and the daily move come from, and both are **a session
/// behind**: the file for the current session is not published until after
/// 16:30 CT, so intraday the previous close is what a reader sees. That lag is
/// the price of having the figures at all — the live catalogue states neither.
///
/// `pair_quantity` is contracts and a genuine `0` — the exchange is saying
/// nothing traded that session, which was true of 11,611 of the 12,593 YES rows
/// on 2026-08-23. The close is not so simple: on 7,157 of those rows the session
/// never traded *and* `start_price` and `end_price` are both `0.00`, which is the
/// file having no mark for that contract rather than a contract worth nothing.
/// Those keep a null previous price, so `change` stays unstated instead of
/// manufacturing a full-dollar move.
pub fn apply_session(market: Market, row: &RawFexArchiveRow) -> Market {
    let traded = cell(&row.pair_quantity);
    let close = cell(&row.end_price);
    let marked = close.is_some_and(|close| close > 0.0 || traded.unwrap_or(0.0) > 0.0);
    let previous = if marked { close } else { None };

    Market {
        previous_price: previous,
        change: match (market.last_price, previous) {
            (Some(last), Some(previous)) => Some(round4(last - previous)),
            _ => None,
        },
        volume24h: traded,
        // Open interest deliberately stays as the catalogue stated it: the
        // archive's column is the figure at that session's close, and the live
        // one is now.
        ..market
    }
}

/// The legs of one event, and whether they exclude each other.
///
/// Exclusivity is claimed only where the venue's own structure states it: a
/// `String` strike unit means the legs are named outcomes of a single field
/// (`{Republican, Democratic}` sums to 1.00), where a numeric unit means a
/// monotone "exceed N" ladder whose legs are all true at once below the mark.
///
/// The Conditional guard is load-bearing and not a nicety. `ZFFCP_091626` has a
/// `String` strike unit and nine legs, and is *not* one winner-take-all field:
/// it is three Fed scenarios crossed with three cumulative CPI thresholds, so
/// within a scenario it is exactly the monotone ladder that must not be flagged.
/// Marking it exclusive would put a fictional Σmid arbitrage on screen across the
/// whole Conditional category.
pub fn normalise_event(
    rows: &[RawFexContract],
    product: Option<&RawFexProduct>,
    sessions: Option<&HashMap<String, RawFexArchiveRow>>,
) -> VenueEvent {
    let first = rows.first().cloned().unwrap_or_default();
    let event_ticker = event_ticker_of(first.contract_id.as_deref().unwrap_or_default());
    let category = first.category.clone().unwrap_or_default();

    let mut markets: Vec<Market> = rows
        .iter()
        .map(|row| {
            let market = normalise_market(row, product);
            let key = row
                .contract_id
                .as_deref()
                .unwrap_or_default()
                .to_uppercase();
            match sessions.and_then(|rows| rows.get(&key)) {
                Some(session) => apply_session(market, session),
                None => market,
            }
        })
        .collect();

    // A numeric ladder already names its own rungs; a coded one does not, and
    // only the sibling questions can say what its codes mean.
    let coded = rows
        .iter()
        .all(|row| cell(&strike_of(row.contract_id.as_deref().unwrap_or_default())).is_none());
    if coded {
        let questions: Vec<String> = rows
            .iter()
            .map(|row| row.question.clone().unwrap_or_default())
            .collect();
        if let Some(clauses) = distinguishing_clauses(&questions) {
            for (market, clause) in markets.iter_mut().zip(clauses) {
                market.yes_sub_title = clause;
            }
        }
    }

    VenueEvent {
        venue: VENUE,
        event_ticker,
        series_ticker: first.product_id.clone().unwrap_or_default(),
        title: first
            .event_display_name
            .clone()
            .unwrap_or_else(|| event_ticker_of(first.contract_id.as_deref().unwrap_or_default())),
        sub_title: product.and_then(|p| p.name.clone()).unwrap_or_default(),
        mutually_exclusive: product.and_then(|p| p.strike_unit.as_deref()) == Some("String")
            && rows.len() > 1
            && category != "Conditional",
        category,
        markets,
    }
}

/// How often the exchange lists a product. `I` is a one-off, not a cadence.
fn frequency_name(code: &str) -> String {
    match code {
        "H" => "hourly".into(),
        "D" => "daily".into(),
        "W" => "weekly".into(),
        "M" => "monthly".into(),
        "Q" => "quarterly".into(),
        "I" => "irregular".into(),
        other => other.to_string(),
    }
}

pub fn normalise_series(raw: &RawFexProduct) -> SeriesInfo {
    let ticker = raw.product_id.clone().unwrap_or_default();
    SeriesInfo {
        venue: VENUE,
        title: raw.name.clone().unwrap_or_else(|| ticker.clone()),
        ticker,
        category: raw.category.clone().unwrap_or_default(),
        frequency: frequency_name(raw.frequency_type.as_deref().unwrap_or_default()),
        // The strike unit is the useful one to carry: it is what says whether a
        // contract id's last segment is a number or the name of an outcome.
        tags: [
            raw.strike_unit.as_deref(),
            raw.external_symbol.as_deref(),
            raw.source_agency.as_deref(),
        ]
        .into_iter()
        .flatten()
        .filter(|tag| !tag.is_empty())
        .map(str::to_owned)
        .collect(),
    }
}

/* ------------------------------------------------------------------- CSVs */

/// A CSV read by its own header rather than by column position.
///
/// Reading by header matters: the archive gained a `vwap` column during its two
/// years on the bucket, and a positional parser would have silently shifted
/// every field after it.
///
/// The `csv` crate is already a workspace dependency and does the quoting
/// correctly, which the naive split this replaces did not. Eight live contracts
/// have a comma inside their identifier — the conditional books,
/// `FEDRO_1128_Senate-R,House-R,President-D` and its siblings — and the exchange
/// quotes those cells, so sixteen lines of every daily archive carry fourteen
/// commas against a twelve-column header. Splitting on every comma shifts their
/// fields two to the left, which puts `House-R` in the `subtype` column, fails
/// the `YES` filter, and drops the contract from the archive entirely: its
/// published turnover and previous close then reach the terminal as `None`,
/// which is the terminal's way of saying the exchange never stated them. It did.
struct Table {
    columns: HashMap<String, usize>,
    rows: Vec<StringRecord>,
}

impl Table {
    fn read(text: &str) -> Table {
        let mut reader = csv::ReaderBuilder::new()
            // A row shorter than the header is a torn read — S3 rewrites the
            // tape every ten minutes and a reader can arrive mid-write — and is
            // dropped below rather than failing the whole file.
            .flexible(true)
            .trim(csv::Trim::All)
            .from_reader(text.as_bytes());

        let columns: HashMap<String, usize> = reader
            .headers()
            .map(|header| {
                header
                    .iter()
                    .enumerate()
                    .map(|(index, name)| (name.to_string(), index))
                    .collect()
            })
            .unwrap_or_default();

        let width = columns.len();
        let rows = reader
            .records()
            .filter_map(std::result::Result::ok)
            .filter(|row| row.len() >= width)
            .collect();

        Table { columns, rows }
    }

    /// One cell by column name, or `""` where the file has no such column.
    fn get<'a>(&self, row: &'a StringRecord, column: &str) -> &'a str {
        self.columns
            .get(column)
            .and_then(|index| row.get(*index))
            .unwrap_or_default()
    }
}

pub fn parse_pairs_csv(text: &str) -> Vec<RawFexPair> {
    let table = Table::read(text);
    table
        .rows
        .iter()
        .filter(|row| !table.get(row, "pair_id").is_empty())
        .map(|row| RawFexPair {
            pair_id: table.get(row, "pair_id").to_string(),
            event_contract: table.get(row, "event_contract").to_string(),
            expiration_date: table.get(row, "expiration_date").to_string(),
            quantity: table.get(row, "quantity").to_string(),
            yes_price: table.get(row, "yes_price").to_string(),
            no_price: table.get(row, "no_price").to_string(),
            pair_time: table.get(row, "pair_time").to_string(),
        })
        .collect()
}

/// The archive, indexed by contract.
///
/// Only the YES rows are kept. Each contract appears twice, YES and NO, and the
/// NO row is the same session with the prices mirrored — `pair_quantity` and
/// `open_interest` are identical on both — so keeping it would double the map
/// for nothing. The key is upper-cased because that is the case the terminal's
/// tickers arrive in.
pub fn parse_archive_csv(text: &str) -> HashMap<String, RawFexArchiveRow> {
    let table = Table::read(text);
    let mut rows = HashMap::new();

    for row in &table.rows {
        let contract = table.get(row, "event_contract");
        if contract.is_empty() || table.get(row, "subtype") != "YES" {
            continue;
        }
        rows.insert(
            contract.to_uppercase(),
            RawFexArchiveRow {
                event_contract: contract.to_string(),
                subtype: table.get(row, "subtype").to_string(),
                expiration_date: table.get(row, "expiration_date").to_string(),
                date: table.get(row, "date").to_string(),
                start_price: table.get(row, "start_price").to_string(),
                high_price: table.get(row, "high_price").to_string(),
                low_price: table.get(row, "low_price").to_string(),
                end_price: table.get(row, "end_price").to_string(),
                settlement_price: table.get(row, "settlement_price").to_string(),
                pair_quantity: table.get(row, "pair_quantity").to_string(),
                open_interest: table.get(row, "open_interest").to_string(),
                vwap: table.get(row, "vwap").to_string(),
            },
        );
    }

    rows
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

/// One `/api/*` page, fetched, unwrapped and cached.
///
/// The unwrap happens inside the cached closure so the parsed rows are what is
/// held: a cache of envelopes would re-parse the doubly-serialised payload on
/// every reader.
async fn api<T>(state: &AppState, path: &str, cache_ttl: Duration) -> Result<Arc<ApiPage<T>>>
where
    T: DeserializeOwned + Send + Sync + 'static,
{
    let url = format!("{}{path}", state.config().forecastex_api_base);
    state
        .cache()
        .cached(&format!("forecastex:{path}"), cache_ttl, || async {
            let envelope: RawFexEnvelope = state
                .http()
                .fetch_json(
                    &url,
                    FetchOptions::new()
                        // A 5,000-row catalogue page was 2.9 MB on 2026-08-23 and
                        // takes the Lambda a second or so to build.
                        .timeout(Duration::from_secs(40))
                        .retries(2),
                )
                .await?;
            unwrap::<T>(&envelope)
        })
        .await
}

/// The not-found a mistyped contract id earns, worded so the reader can retype
/// it.
fn missing_contract(id: &str) -> UpstreamError {
    UpstreamError::not_found(format!("No ForecastEx contract {id}")).with_hint(
        "ForecastEx names a contract PRODUCT_PERIOD_STRIKE, as in `fx:HORC_1126_REPUBLICAN` \
         or `fx:FF_091726_3.625`. Settled contracts leave the catalogue on expiry.",
    )
}

/// One contract by id. `/api/contracts?contractId=` is case-insensitive and
/// answers with the canonical spelling, which is what makes it the recovery path
/// for [`canonical_contract_id`].
async fn contract_row(state: &AppState, id: &str) -> Result<RawFexContract> {
    let path = format!(
        "/api/contracts{}",
        qs(&[
            ("page", "1".into()),
            ("pageSize", "1".into()),
            ("contractId", id.to_string()),
        ])
    );
    let page = api::<RawFexContract>(state, &path, ttl::QUOTE).await?;

    page.rows
        .first()
        .filter(|row| row.contract_id.as_deref().is_some_and(|id| !id.is_empty()))
        .cloned()
        .ok_or_else(|| missing_contract(id))
}

/// Every contract of one product, which is the only way to reach an event.
async fn product_contracts(state: &AppState, product_id: &str) -> Result<Vec<RawFexContract>> {
    let mut rows: Vec<RawFexContract> = Vec::new();

    // The largest product lists ~132 contracts, so one page is almost always the
    // lot; the loop is here for the day a daily product runs long.
    for page in 1..=3 {
        let path = format!(
            "/api/contracts{}",
            qs(&[
                ("page", page.to_string()),
                ("pageSize", "1000".into()),
                ("productId", product_id.to_string()),
                ("sortBy", "contract_id".into()),
                ("sortOrder", "asc".into()),
            ])
        );
        let result = api::<RawFexContract>(state, &path, ttl::QUOTE).await?;
        rows.extend(result.rows.iter().cloned());
        if result.next_page.is_none() {
            break;
        }
    }

    Ok(rows)
}

/// The product registry, keyed by product id.
///
/// All 992 products arrived in a single request on 2026-08-23, and they carry the
/// `strike_unit` that decides how a contract id's last segment reads and the
/// `category` that [`list_series`] filters on — so this is fetched once per
/// catalogue TTL and shared by every path that needs either.
async fn product_index(state: &AppState) -> Result<Arc<HashMap<String, RawFexProduct>>> {
    state
        .cache()
        .cached("forecastex:products", ttl::CATALOGUE, || async {
            let path = format!(
                "/api/products{}",
                qs(&[("page", "1".into()), ("pageSize", "2000".into())])
            );
            let page = api::<RawFexProduct>(state, &path, ttl::CATALOGUE).await?;
            Ok(page
                .rows
                .iter()
                .filter_map(|row| {
                    let id = row.product_id.clone().filter(|id| !id.is_empty())?;
                    Some((id, row.clone()))
                })
                .collect::<HashMap<String, RawFexProduct>>())
        })
        .await
}

/// The registry, or an empty one. Every caller of this treats the product as a
/// decoration — it supplies the strike unit and the series name — and a
/// registry that failed must not cost the reader the quote it decorates.
async fn products_or_empty(state: &AppState) -> Arc<HashMap<String, RawFexProduct>> {
    product_index(state)
        .await
        .unwrap_or_else(|_| Arc::new(HashMap::new()))
}

/* -------------------------------------------------------- session archive */

/// One published end-of-session file, and which session it was.
#[derive(Debug, Clone, Default)]
pub struct SessionArchive {
    /// The session the figures belong to, `YYYYMMDD`.
    pub date: String,
    pub rows: HashMap<String, RawFexArchiveRow>,
}

/// How far back to look for a published archive before giving up on the figures.
const ARCHIVE_LOOKBACK: usize = 5;

/// Fetch one CSV off the public bucket.
///
/// Browser headers are off and `Accept: text/csv` is on: S3 is not bot-protected
/// and the browser set only makes the request larger, while an `Accept` naming
/// the format is what the bucket answers cleanly.
async fn archive_csv(state: &AppState, path: &str) -> Result<String> {
    let url = format!("{}{path}", state.config().forecastex_archive_base);
    state
        .http()
        .fetch_text(
            &url,
            FetchOptions::new()
                .timeout(Duration::from_secs(40))
                .retries(1)
                .browser_headers(false)
                .header("Accept", "text/csv,*/*"),
        )
        .await
}

/// The newest published end-of-session archive.
///
/// The current session's file 404s until after 16:30 CT, and a holiday leaves a
/// gap of several days, so the newest available one is found by walking back a
/// bounded number of sessions rather than assumed. Which session answered is
/// carried on the result, because every figure taken from it is as old as that.
///
/// A failure here is not fatal: turnover, the previous close and the daily move
/// simply stay unstated, which is what they were before the file existed. The
/// `None` is cached alongside a success, deliberately — otherwise every reader
/// during the hour after the roll repeats the same five-deep 404 walk.
async fn session_archive(state: &AppState) -> Arc<Option<SessionArchive>> {
    state
        .cache()
        .cached("forecastex:archive", ttl::CATALOGUE, || async {
            let mut date = session_date(OffsetDateTime::now_utc().unix_timestamp());

            for _ in 0..ARCHIVE_LOOKBACK {
                let path = format!("/prices/daily_prices_{date}.csv");
                match archive_csv(state, &path).await {
                    Ok(csv) => {
                        return Ok(Some(SessionArchive {
                            rows: parse_archive_csv(&csv),
                            date,
                        }))
                    }
                    // A 404 is the file not being published yet, which is the
                    // expected answer for the session in progress. Anything else
                    // is a real failure, and the corpus goes without its ranking
                    // figures rather than hanging on it.
                    Err(err) if err.code == codes::NOT_FOUND => date = previous_session(&date),
                    Err(_) => return Ok(None),
                }
            }

            Ok(None)
        })
        .await
        .unwrap_or_else(|_| Arc::new(None))
}

/* ----------------------------------------------------------------- lookups */

pub async fn get_market(state: &AppState, id: &str) -> Result<Market> {
    let raw = contract_row(state, id).await?;
    let products = products_or_empty(state).await;
    let market = normalise_market(
        &raw,
        products.get(raw.product_id.as_deref().unwrap_or_default()),
    );

    let archive = session_archive(state).await;
    let key = raw
        .contract_id
        .as_deref()
        .unwrap_or_default()
        .to_uppercase();
    match archive.as_ref().as_ref().and_then(|a| a.rows.get(&key)) {
        Some(session) => Ok(apply_session(market, session)),
        None => Ok(market),
    }
}

pub async fn get_event(state: &AppState, id: &str) -> Result<VenueEvent> {
    let event_ticker = event_ticker_of(id);
    let product_id = event_ticker
        .split('_')
        .next()
        .unwrap_or_default()
        .to_string();

    let rows = product_contracts(state, &product_id).await?;
    let products = products_or_empty(state).await;
    let archive = session_archive(state).await;

    let legs: Vec<RawFexContract> = rows
        .into_iter()
        .filter(|row| {
            event_ticker_of(row.contract_id.as_deref().unwrap_or_default()) == event_ticker
        })
        .collect();

    if legs.is_empty() {
        return Err(
            UpstreamError::not_found(format!("No ForecastEx event {id}")).with_hint(
                "A ForecastEx event is the first two segments of a contract id — the product and \
             its period — as in `fx:HORC_1126` or `fx:FFDEC_091626`.",
            ),
        );
    }

    Ok(normalise_event(
        &legs,
        products.get(&product_id),
        archive.as_ref().as_ref().map(|a| &a.rows),
    ))
}

/// Products as series, filtered on the category they state.
///
/// Honest here in a way it is not at either Polymarket: every product row
/// carries its own category, so a filtered list is the real subset rather than
/// whatever the crawl happened to reach.
pub async fn list_series(state: &AppState, category: Option<&str>) -> Result<Vec<SeriesInfo>> {
    let products = product_index(state).await?;
    // A bare `?category=` reaches here as `Some("")`, and treating that as a
    // filter answers with nothing at all rather than with the whole catalogue.
    let want = category
        .map(|c| c.trim().to_lowercase())
        .filter(|want| !want.is_empty());

    // Collected through a map keyed on the ticker so the list comes out in the
    // order a reader scanning `XV` for a family expects to find it.
    let listed: BTreeMap<&str, SeriesInfo> = products
        .values()
        .filter(|row| match want.as_deref() {
            Some(want) => row.category.as_deref().unwrap_or_default().to_lowercase() == want,
            None => true,
        })
        .map(|row| {
            (
                row.product_id.as_deref().unwrap_or_default(),
                normalise_series(row),
            )
        })
        .collect();

    Ok(listed.into_values().collect())
}

/* -------------------------------------------------- what the exchange lacks */

/// There is no order book, and there is no endpoint that would serve one.
///
/// ForecastEx runs a paired auction: a print matches a YES buyer against a NO
/// buyer and creates both positions at once, so what exists afterwards is a
/// print and an open-interest figure, not a bid and an ask. Nothing in the
/// public API — catalogue, prices, tape or archive — carries depth, and the
/// exchange's own contract page renders only "Yes:", "No:" and "Open Interest:".
/// Live quotes exist, but inside IBKR ForecastTrader behind its SSO.
///
/// Standing a one-level book up from the last YES and NO prints would be worse
/// than refusing: those two prints are independent and summed to 1.01 on 848
/// contracts on 2026-08-23, so the ladder would show a crossed market that no one
/// can trade.
///
/// The venue registry declares `book: false` and
/// [`crate::sources::venues::get_order_book`] refuses before this module is
/// reached, so this is a backstop against a future caller that dispatches here
/// directly — it must never become the place a synthetic ladder creeps in.
pub async fn get_order_book(_state: &AppState, _id: &str, _depth: usize) -> Result<OrderBook> {
    Err(
        UpstreamError::unsupported("ForecastEx publishes no order book").with_hint(
            "The exchange matches by pairing a YES buyer with a NO buyer, so a print is all \
             there is — /api/contracts states last_yes_price, last_no_price and open_interest, \
             and no public endpoint carries depth. Live quotes are behind IBKR \
             ForecastTrader\u{2019}s SSO at /portal.proxy/v1/ft, which needs an Interactive \
             Brokers login. DES, TAS and GP work.",
        ),
    )
}

/* ------------------------------------------------------------------- tape */

/// The tape, filtered out of the whole-exchange session file.
///
/// **There is no taker side, and that is structural rather than missing.** Every
/// print creates one YES holder and one NO holder simultaneously, so neither
/// side lifted the other and there is no aggressor to name. The field is emitted
/// empty; filling it in with `yes` would assert an initiative that the exchange's
/// own matching model rules out, and the client draws an unattributed print in
/// neither colour — a guessed side undoes exactly that.
///
/// `pair_time` carries a Central offset and *nanosecond* fractional seconds
/// (`2026-08-23T08:34:34.022716795-05:00`), and the digit count varies row to
/// row — the same file also holds `.90169434`, eight digits. JavaScript's
/// `Date.parse` returns `NaN` on those and the TypeScript this was ported from
/// had to truncate the fraction to milliseconds first; `time`'s RFC 3339 parser
/// reads all nine natively, so the string is parsed as it arrives and the
/// sub-second part is simply discarded by a tape stamped in whole seconds.
pub fn normalise_trades(rows: &[RawFexPair], ticker: &str, limit: usize) -> TradesResponse {
    let want = ticker.to_uppercase();
    let mut trades: Vec<Trade> = Vec::new();

    for row in rows {
        if row.event_contract.to_uppercase() != want {
            continue;
        }

        let seconds = OffsetDateTime::parse(&row.pair_time, &Rfc3339)
            .ok()
            .map(|moment| moment.unix_timestamp());
        let count = cell(&row.quantity);
        let yes_price = cell(&row.yes_price);
        let no_price = cell(&row.no_price);

        // A row the file truncated — S3 rewrites the tape every ten minutes and a
        // reader can arrive mid-write — is dropped rather than published. A
        // `Trade` carries plain numbers, so the alternative is a 1,000-lot at
        // $0.00 dated 1 January 1970 sitting at the top of the tape, which reads
        // as a print.
        let (Some(ts), Some(count), Some(yes_price), Some(no_price)) =
            (seconds, count, yes_price, no_price)
        else {
            continue;
        };

        trades.push(Trade {
            venue: VENUE,
            // An opaque base-32-looking string: an identity, never an amount.
            trade_id: row.pair_id.clone(),
            ticker: want.clone(),
            ts,
            count,
            yes_price,
            no_price,
            taker_side: String::new(),
            // No block or cross flag is published.
            is_block_trade: false,
        });
    }

    // The file is roughly but not strictly time-ordered, so it is sorted rather
    // than reversed.
    trades.sort_by_key(|trade| std::cmp::Reverse(trade.ts));
    trades.truncate(limit);

    // No cursor: the tape is one file per session, so a page is the file. Paging
    // deeper means the previous session's file, not an offset into this one.
    TradesResponse {
        trades,
        cursor: None,
    }
}

/// One session's prints for the whole exchange, ~1.2 MB on 2026-08-23.
///
/// Held for a minute rather than for the three seconds a quote gets: the file is
/// rewritten about every ten minutes, so a shorter TTL would re-download a
/// megabyte to learn nothing, and N panels on one contract share the one copy.
async fn pairs_file(state: &AppState, date: &str) -> Result<Arc<Vec<RawFexPair>>> {
    state
        .cache()
        .cached(&format!("forecastex:pairs:{date}"), ttl::META, || async {
            let csv = archive_csv(state, &format!("/pairs/pairs_{date}.csv")).await?;
            Ok(parse_pairs_csv(&csv))
        })
        .await
}

/// The tape for one contract, out of the two newest session files.
///
/// Most contracts do not print every session — 982 of 11,466 traded on a typical
/// day — so an empty answer from the current file is the norm rather than a
/// fault, and the panel is better served by the last session that did trade.
///
/// What must not happen is the two being confused. An empty tape means "nobody
/// traded this contract"; a missing file means "the exchange has not published
/// the tape yet", which is what the minutes after the 16:15 CT roll look like.
/// If neither session file exists, that is said rather than shown as a market
/// nobody trades.
pub async fn get_trades(state: &AppState, id: &str, limit: usize) -> Result<TradesResponse> {
    let today = session_date(OffsetDateTime::now_utc().unix_timestamp());
    let previous = previous_session(&today);

    let mut answered = false;
    let mut result = TradesResponse {
        trades: Vec::new(),
        cursor: None,
    };

    for date in [today.as_str(), previous.as_str()] {
        let rows = match pairs_file(state, date).await {
            Ok(rows) => rows,
            // A 404 is the file not existing yet, or a holiday. Anything else is
            // a real failure and is worth reporting rather than reading as a
            // quiet tape.
            Err(err) if err.code == codes::NOT_FOUND => continue,
            Err(err) => return Err(err),
        };

        answered = true;
        result = normalise_trades(&rows, id, limit);
        if !result.trades.is_empty() {
            break;
        }
    }

    if !answered {
        let base = &state.config().forecastex_archive_base;
        return Err(UpstreamError::not_found(
            "ForecastEx has not published a tape for this session yet",
        )
        .with_hint(format!(
            "The print tape is one whole-exchange file per session at \
             {base}/pairs/pairs_YYYYMMDD.csv, and neither {today} nor the session before it is \
             there. Today\u{2019}s file appears a few minutes after the 16:15 CT roll."
        )));
    }

    Ok(result)
}

/* ---------------------------------------------------------------- candles */

/// The two buckets `/api/prices` serves. Both are case-sensitive: `H` answers an
/// envelope `statusCode` of 400 saying `interval must be 'h' or 'd'`.
fn interval_name(interval: CandleInterval) -> Option<&'static str> {
    match interval {
        CandleInterval::OneHour => Some("h"),
        CandleInterval::OneDay => Some("d"),
        CandleInterval::OneMinute => None,
    }
}

/// The archive is two years deep, so nothing older can be asked for.
const MAX_DAYS_BACK: i64 = 760;

/// When a sample's period ends. [`Candle::time`] is a period *end* everywhere in
/// the terminal, and `/api/prices` labels its buckets by where they *begin*.
///
/// Both feeds label, and neither says so. Reconciling the buckets against the
/// print tape settles it, and it was re-checked live on 2026-08-23 against
/// `UHLAX_082326_83`: the hourly buckets stamped 10:00Z through 20:00Z carry
/// 172, 408, 995, 568, 509, 8681, 5097, 30531, 28466, 21655 and 22872, and the
/// tape's prints inside each of those eleven hours sum to exactly the same
/// eleven figures. The stamp opens the hour; the bar closes an hour later.
///
/// The daily feed is the same shape one rung up, and it is a **UTC** calendar day
/// rather than the Central session day the exchange files its archive under. Same
/// contract: the daily points for `2026-08-22` and `2026-08-23` report 716 and
/// 173,091, which are exactly the sums of the hourly buckets stamped those dates
/// in UTC, while the archive row for the 8/23 *session* — 16:15 CT the previous
/// day to 16:14 CT — reports 129,763 instead. Closing a daily bar at Central
/// midnight, as the session boundary would suggest, therefore stamps every one of
/// them five hours late.
pub fn sample_end(stamp: &str, interval: CandleInterval) -> Option<i64> {
    if stamp.is_empty() {
        return None;
    }

    // A bare `YYYY-MM-DD` is the daily feed, and its bar closes at the end of
    // that UTC day.
    if stamp.len() == 10 && !stamp.contains('T') {
        let clock = read_wall_clock(stamp)?;
        return naive_instant(&clock).map(|start| start + 86_400);
    }

    // A zoneless wall clock never appears on the hourly feed, but reading one as
    // local time would move the whole series by the server's own offset, so it
    // is read as UTC explicitly.
    let text = if stamp.ends_with('Z') || has_offset(stamp) {
        stamp.to_string()
    } else {
        format!("{stamp}Z")
    };

    OffsetDateTime::parse(&text, &Rfc3339)
        .ok()
        .map(|moment| moment.unix_timestamp() + interval.seconds())
}

/// Whether a timestamp already names its own offset, as `+05:30` or `-06:00`.
fn has_offset(stamp: &str) -> bool {
    let bytes = stamp.as_bytes();
    bytes.len() >= 6
        && matches!(bytes[bytes.len() - 6], b'+' | b'-')
        && bytes[bytes.len() - 3] == b':'
}

/// Samples as bars.
///
/// ForecastEx publishes a close and a size per bucket, never an OHLC bar, so
/// each bar's open, high and low are its close and the response says so. What is
/// *not* invented is a traded flag: the feed carries a sample in every bucket
/// whether or not anything printed, and `volume == 0` is the exchange
/// distinguishing a carried-forward mark from a real one.
///
/// Buckets are merged rather than assumed unique, so a repeated stamp folds into
/// one bar with its sizes added instead of drawing two bars at one instant.
pub fn bucket_samples(points: &[RawFexPricePoint], interval: CandleInterval) -> Vec<Candle> {
    // Ordered by bucket end as it is built, because a chart drawn out of order
    // runs backwards.
    let mut buckets: BTreeMap<i64, Candle> = BTreeMap::new();

    for point in points {
        let price = num_of(point.yes_price.as_ref());
        let end = point
            .interval_date
            .as_deref()
            .and_then(|stamp| sample_end(stamp, interval));
        let (Some(price), Some(end)) = (price, end) else {
            continue;
        };

        let volume = num_of(point.volume.as_ref());
        match buckets.get_mut(&end) {
            Some(bucket) => {
                bucket.high = bucket.high.max(price);
                bucket.low = bucket.low.min(price);
                bucket.close = price;
                if let Some(volume) = volume {
                    bucket.volume = Some(bucket.volume.unwrap_or(0.0) + volume);
                }
            }
            None => {
                buckets.insert(
                    end,
                    Candle {
                        time: end,
                        open: price,
                        high: price,
                        low: price,
                        close: price,
                        volume,
                        // Stated per session in the archive only, never per
                        // bucket here.
                        open_interest: None,
                        traded: false,
                        bid: None,
                        ask: None,
                    },
                );
            }
        }
    }

    buckets
        .into_values()
        .map(|bucket| Candle {
            traded: bucket.volume.unwrap_or(0.0) > 0.0,
            ..bucket
        })
        .collect()
}

pub async fn get_candles(
    state: &AppState,
    id: &str,
    interval: CandleInterval,
    start_ts: i64,
    end_ts: i64,
) -> Result<CandlesResponse> {
    let Some(bucket) = interval_name(interval) else {
        return Err(
            UpstreamError::unsupported("ForecastEx publishes no minute bars").with_hint(
                "GET /api/prices?contractId=…&interval=h|d is the whole history feed, and it \
                 rejects every other value: the finest bucket the exchange publishes is one \
                 hour. A minute grid would have to be rebuilt from the whole-exchange pairs \
                 tape by hand.",
            ),
        );
    };

    // The case trap: /api/prices answers an upper-cased id with HTTP 200 and an
    // empty series, so the canonical spelling has to be recovered first or the
    // chart is silently blank.
    let contract_id = canonical_contract_id(state, id).await?;

    let now = OffsetDateTime::now_utc().unix_timestamp();
    let days = (now - start_ts)
        .div_euclid(86_400)
        .saturating_add(2)
        .clamp(1, MAX_DAYS_BACK);

    let path = format!(
        "/api/prices{}",
        qs(&[
            ("contractId", contract_id.clone()),
            ("daysBack", days.to_string()),
            ("interval", bucket.to_string()),
        ])
    );
    let page = api::<RawFexPricePoint>(state, &path, ttl::CANDLES).await?;

    let period = interval.seconds();
    let candles: Vec<Candle> = bucket_samples(&page.rows, interval)
        .into_iter()
        // The newest bar ends in the future while its session is still open, so
        // the window is generous by one period at the top rather than dropping
        // it.
        .filter(|candle| candle.time >= start_ts && candle.time <= end_ts + period)
        .collect();

    Ok(CandlesResponse {
        venue: VENUE,
        ticker: id.to_uppercase(),
        series_ticker: contract_id
            .split('_')
            .next()
            .unwrap_or_default()
            .to_string(),
        interval,
        candles,
        note: Some(
            "ForecastEx publishes price samples rather than OHLC bars: each bar is one sample, \
             so its high and low are its close. A zero-volume bar is the exchange carrying the \
             previous mark forward. The feed labels each bucket by where it opens, so bars are \
             stamped one period later here; a daily bar is a UTC day, not the exchange\u{2019}s \
             own 16:15 CT session, and does not line up with the end-of-session archive."
                .into(),
        ),
    })
}

/* ----------------------------------------------------------------- corpus */

const CATALOGUE_KEY: &str = "forecastex:catalogue";

/// The snapshot, plus the spelling of every id in it.
struct Catalogue {
    corpus: Arc<Corpus>,
    /// Upper-cased id → the exact spelling the exchange publishes, which is the
    /// only one `/api/prices` answers.
    canonical: HashMap<String, String>,
}

/// Crawled in pages of 5,000, ordered by contract id.
///
/// **The ordering is not a preference.** Crawling the same three pages by
/// `open_interest desc` on 2026-08-23 returned 11,750 rows and only 10,270
/// distinct ids: ties are not broken, so the offset window shifts between
/// requests and 1,480 contracts arrive twice while as many are never served.
/// Ordering by a unique key fixes it, and the rows are deduped anyway — the
/// failure was invisible in the row count, and only a dedupe would have shown it.
///
/// 5,000 is also comfortably inside the Lambda's 6 MB response cap: that page was
/// 2.9 MB, where a 10,000-row page was 5.85 MB and one growth spurt away from
/// failing outright rather than truncating.
const CORPUS_PAGE: usize = 5000;
const CORPUS_MAX_PAGES: usize = 4;

async fn build_catalogue(state: &AppState) -> Result<Catalogue> {
    let mut rows: Vec<RawFexContract> = Vec::new();
    let mut canonical: HashMap<String, String> = HashMap::new();
    let mut truncated = false;

    for page in 1..=CORPUS_MAX_PAGES {
        let path = format!(
            "/api/contracts{}",
            qs(&[
                ("page", page.to_string()),
                ("pageSize", CORPUS_PAGE.to_string()),
                ("sortBy", "contract_id".into()),
                ("sortOrder", "asc".into()),
            ])
        );
        let result = api::<RawFexContract>(state, &path, ttl::CATALOGUE).await?;

        for row in &result.rows {
            let Some(id) = row.contract_id.as_deref().filter(|id| !id.is_empty()) else {
                continue;
            };
            if canonical.contains_key(&id.to_uppercase()) {
                continue;
            }
            canonical.insert(id.to_uppercase(), id.to_string());
            rows.push(row.clone());
        }

        if result.next_page.is_none() {
            break;
        }
        truncated = page == CORPUS_MAX_PAGES;
    }

    let products = products_or_empty(state).await;
    let archive = session_archive(state).await;
    let sessions = archive.as_ref().as_ref().map(|a| &a.rows);

    // Group on the event prefix in crawl order, which is contract id order, so
    // the legs of an event arrive together and a ladder keeps its rungs in order.
    let mut order: Vec<String> = Vec::new();
    let mut grouped: HashMap<String, Vec<RawFexContract>> = HashMap::new();
    for row in rows {
        let key = event_ticker_of(row.contract_id.as_deref().unwrap_or_default());
        grouped
            .entry(key.clone())
            .or_insert_with(|| {
                order.push(key);
                Vec::new()
            })
            .push(row);
    }

    let mut events: Vec<VenueEvent> = Vec::with_capacity(order.len());
    let mut markets: Vec<Market> = Vec::new();
    for key in &order {
        let Some(legs) = grouped.get(key) else {
            continue;
        };
        let product = products.get(legs[0].product_id.as_deref().unwrap_or_default());
        let event = normalise_event(legs, product, sessions);
        markets.extend(event.markets.iter().cloned());
        events.push(event);
    }

    Ok(Catalogue {
        corpus: Arc::new(Corpus::new(VENUE, events, markets, truncated)),
        canonical,
    })
}

async fn catalogue(state: &AppState) -> Result<Arc<Catalogue>> {
    state
        .cache()
        .cached(CATALOGUE_KEY, ttl::CATALOGUE, || async {
            build_catalogue(state).await
        })
        .await
}

/// The exchange's own spelling of an identifier.
///
/// Read from the warm catalogue when there is one, because that costs nothing;
/// otherwise from `/api/contracts?contractId=`, which is case-insensitive and
/// answers with the canonical row. The catalogue is *not* built on demand here —
/// a chart request should not pay for a three-page, 8.7 MB crawl when one
/// 900-byte lookup settles it.
async fn canonical_contract_id(state: &AppState, id: &str) -> Result<String> {
    if let Some(warm) = state.cache().get::<Catalogue>(CATALOGUE_KEY).await {
        if let Some(known) = warm.canonical.get(&id.to_uppercase()) {
            return Ok(known.clone());
        }
    }

    let row = contract_row(state, id).await?;
    Ok(row.contract_id.unwrap_or_else(|| id.to_string()))
}

pub async fn corpus_snapshot(state: &AppState) -> Result<Arc<Corpus>> {
    let result = catalogue(state).await.map(|held| Arc::clone(&held.corpus));
    crate::sources::corpus::record(state, Venue::ForecastEx, &result);
    result
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
                "[forecastex] corpus warm"
            ),
            Err(err) => tracing::warn!(error = %err, "[forecastex] corpus warm failed"),
        }
    });
}

/// Search the snapshot rather than `/api/contracts?search=`.
///
/// The exchange's own search is decent — case-insensitive across id, question
/// and display name — but it ranks by nothing in particular and returns
/// contracts where every other venue here returns events. Ranking the local
/// snapshot instead is what makes a ForecastEx result comparable with a Kalshi
/// one for the same words.
pub async fn search(state: &AppState, query: &str, limit: usize) -> Result<SearchResponse> {
    refuse_overlong_query(query)?;
    let snapshot = corpus_snapshot(state).await?;
    Ok(search_corpus(&snapshot, query, limit))
}

/// Leaderboards over the snapshot.
///
/// Open interest ranks on the live catalogue figure; volume and the movers rank
/// on the end-of-session archive merged in at crawl time, so they are a session
/// behind and read as the last full session's board. Contracts the archive did
/// not cover — anything listed since it was published — are dropped from those
/// boards by [`rank_markets`] rather than ranked as zero.
pub async fn top_markets(state: &AppState, sort: MoverSort, limit: usize) -> Result<Vec<Market>> {
    let snapshot = corpus_snapshot(state).await?;
    Ok(rank_markets(&snapshot.markets, sort, limit))
}

/* ------------------------------------------------------------------ tests */

#[cfg(test)]
mod tests {
    //! ForecastEx normalisation tests.
    //!
    //! Every fixture is a trimmed copy of a real 2026-08-23 response: the
    //! double-wrapped `body.data` string exactly as the Lambda sends it,
    //! catalogue rows with their `null` prices intact, and lines lifted verbatim
    //! out of `pairs_20260823.csv` and `daily_prices_20260823.csv` — including
    //! the rows that trap a careless parser, where an untraded session writes
    //! `0.00` into every price column and leaves `settlement_price` empty, and
    //! the quoted identifier that carries two commas of its own.

    use super::*;
    use serde_json::json;
    use wiremock::matchers::{method, path as path_matcher, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use crate::config::Config;

    /// `/api/contracts?page=1&pageSize=5000&sortBy=contract_id&sortOrder=asc`,
    /// trimmed by hand to sixteen contracts across eight events. Each row is
    /// here for a trap:
    ///
    /// * `HORC_1126_Republican` / `_Democratic` — a `String` strike unit that
    ///   really is winner-take-all, and a coded ladder whose questions differ by
    ///   one word.
    /// * `FFDEC_091626_{E0,E25,E-25,H50,M-50}` — the five September FOMC rungs,
    ///   which is the whole reason [`distinguishing_clauses`] exists.
    /// * `ZFFCP_091626_*` — four-segment ids, a `String` strike unit and the
    ///   `Conditional` category that must *not* be flagged exclusive.
    /// * `UHBKF_082326_85` — never traded, both prints `null`, open interest 0.
    /// * `UHBKF_082326_91` — an "exceed" ladder running upward.
    /// * `ULATL_082326_73` — a "be below" ladder running the other way.
    /// * `HCAB_1226_11` — "at least", which includes its own rung.
    /// * `DISSN_091626_1` — "exactly one", a number with no direction at all.
    /// * `FF_091726_0.875` — a `Percentage` unit, and a live row whose two
    ///   independent prints sum to 1.01.
    const CONTRACTS_JSON: &str = include_str!("fixtures/forecastex_contracts.json");

    /// `/api/products?page=1&pageSize=2000`, trimmed to the eight products the
    /// contracts fixture references. `description`, `question_template`,
    /// `source_agency_url` and `calculation_method` are dropped from each row —
    /// they are hundreds of bytes apiece and nothing in this module reads them.
    const PRODUCTS_JSON: &str = include_str!("fixtures/forecastex_products.json");

    /// Sixteen lines of `prices/daily_prices_20260823.csv`, verbatim. Eight
    /// contracts, each with its YES and NO row so the NO-row filter is exercised:
    /// two HORC legs that traded, `FFDEC_091626_E0` and `FF_091726_0.875` marked
    /// but untraded, `UHBKF_082326_91` and `ULATL_082326_73` with real highs and
    /// lows, `UST_1230_54.41` untraded with every price `0.00` and a blank
    /// settlement, and the quoted `FEDRO_1128_Senate-R,House-R,President-D`.
    const ARCHIVE_CSV: &str = include_str!("fixtures/forecastex_daily_prices.csv");

    /// Seven lines of `pairs/pairs_20260823.csv`, verbatim: three prints on
    /// `UHBKF_082326_91`, two on `ULATL_082326_73` and two on
    /// `HORC_1126_Republican`, whose 2,000 and 2,750 lots are exactly the 4,750
    /// the archive files for that session.
    const PAIRS_CSV: &str = include_str!("fixtures/forecastex_pairs.csv");

    fn envelope_of(text: &str) -> RawFexEnvelope {
        serde_json::from_str(text).expect("the captured envelope parses")
    }

    fn contracts_fixture() -> Vec<RawFexContract> {
        unwrap::<RawFexContract>(&envelope_of(CONTRACTS_JSON))
            .expect("the captured contracts page unwraps")
            .rows
    }

    fn products_fixture() -> HashMap<String, RawFexProduct> {
        unwrap::<RawFexProduct>(&envelope_of(PRODUCTS_JSON))
            .expect("the captured products page unwraps")
            .rows
            .into_iter()
            .filter_map(|row| Some((row.product_id.clone()?, row)))
            .collect()
    }

    fn contract(id: &str) -> RawFexContract {
        contracts_fixture()
            .into_iter()
            .find(|row| row.contract_id.as_deref() == Some(id))
            .unwrap_or_else(|| panic!("the fixture carries {id}"))
    }

    fn contracts(ids: &[&str]) -> Vec<RawFexContract> {
        ids.iter().map(|id| contract(id)).collect()
    }

    fn product(id: &str) -> RawFexProduct {
        products_fixture()
            .remove(id)
            .unwrap_or_else(|| panic!("the fixture carries product {id}"))
    }

    fn archive() -> HashMap<String, RawFexArchiveRow> {
        parse_archive_csv(ARCHIVE_CSV)
    }

    fn session(contract: &str) -> RawFexArchiveRow {
        archive()
            .remove(contract)
            .unwrap_or_else(|| panic!("the fixture is missing {contract}"))
    }

    fn contract_of(value: Value) -> RawFexContract {
        serde_json::from_value(value).expect("the literal parses as a contract")
    }

    fn points_of(value: Value) -> Vec<RawFexPricePoint> {
        serde_json::from_value(value).expect("the literal parses as a price series")
    }

    /* -------------------------------------------------------------- envelope */

    #[test]
    fn unwrap_parses_the_payload_a_second_time_because_body_data_is_a_json_string() {
        let page = unwrap::<RawFexContract>(&envelope_of(CONTRACTS_JSON)).expect("a page");
        assert_eq!(page.rows.len(), 16);
        assert_eq!(
            page.rows[0].contract_id.as_deref(),
            Some("HORC_1126_Republican")
        );
        assert_eq!(num_of(page.rows[0].last_yes_price.as_ref()), Some(0.19));
    }

    #[test]
    fn unwrap_reports_the_next_page_so_a_crawl_knows_when_the_catalogue_ends() {
        assert_eq!(
            unwrap::<RawFexContract>(&envelope_of(CONTRACTS_JSON))
                .expect("a page")
                .next_page,
            None
        );

        let more: RawFexEnvelope = serde_json::from_value(json!({
            "statusCode": 200,
            "body": { "data": "[]", "next_page": 2, "total_pages": 3 }
        }))
        .expect("an envelope");
        assert_eq!(
            unwrap::<RawFexContract>(&more).expect("a page").next_page,
            Some(2)
        );
    }

    #[test]
    fn unwrap_accepts_a_plain_array_in_case_the_double_wrapping_is_ever_fixed() {
        let envelope: RawFexEnvelope = serde_json::from_value(json!({
            "statusCode": 200,
            "body": {
                "data": [{ "contract_id": "HORC_1126_Republican" }],
                "next_page": null
            }
        }))
        .expect("an envelope");

        let page = unwrap::<RawFexContract>(&envelope).expect("a page");
        assert_eq!(
            page.rows[0].contract_id.as_deref(),
            Some("HORC_1126_Republican")
        );
    }

    #[test]
    fn unwrap_reads_the_envelope_status_not_the_http_one_when_a_request_is_rejected() {
        // The real shape of a 400, captured live: HTTP 200 outside, and `body`
        // collapsed to a string.
        let envelope: RawFexEnvelope = serde_json::from_value(json!({
            "statusCode": 400,
            "body": "{\"error\": \"interval must be 'h' or 'd'\"}"
        }))
        .expect("an envelope");

        let err = unwrap::<RawFexContract>(&envelope).expect_err("a rejection");
        assert_eq!(err.code, codes::UPSTREAM_STATUS);
        assert!(err.message.contains("interval must be 'h' or 'd'"));
        assert_eq!(err.status, Some(400));
    }

    #[test]
    fn unwrap_reports_a_collapsed_body_even_when_the_envelope_claims_success() {
        // The status and the body disagree on some rejections; a string body is
        // never a payload, so it is read as the rejection it is.
        let envelope: RawFexEnvelope = serde_json::from_value(json!({
            "statusCode": 200,
            "body": "not json at all"
        }))
        .expect("an envelope");

        let err = unwrap::<RawFexContract>(&envelope).expect_err("a rejection");
        assert!(err.message.contains("not json at all"));
    }

    #[test]
    fn unwrap_names_the_payload_cap_when_the_lambda_itself_fails() {
        let envelope: RawFexEnvelope = serde_json::from_value(json!({
            "errorMessage":
                "Response payload size exceeded maximum allowed payload size (6291556 bytes).",
            "errorType": "Function.ResponseSizeTooLarge"
        }))
        .expect("an envelope");

        let err = unwrap::<RawFexContract>(&envelope).expect_err("a failure");
        assert_eq!(err.code, codes::UPSTREAM_ERROR);
        assert!(err.message.contains("payload size"));
        assert!(err.hint.unwrap().contains("pageSize"));
    }

    #[test]
    fn unwrap_refuses_a_data_string_that_is_not_json_rather_than_returning_nothing() {
        let envelope: RawFexEnvelope =
            serde_json::from_value(json!({ "statusCode": 200, "body": { "data": "<html>" } }))
                .expect("an envelope");

        let err = unwrap::<RawFexContract>(&envelope).expect_err("a bad body");
        assert_eq!(err.code, codes::BAD_UPSTREAM_BODY);
        assert!(err.message.contains("not JSON"));
    }

    #[test]
    fn unwrap_refuses_an_envelope_with_no_data_array_in_it() {
        let envelope: RawFexEnvelope =
            serde_json::from_value(json!({ "statusCode": 200, "body": { "next_page": null } }))
                .expect("an envelope");

        let err = unwrap::<RawFexContract>(&envelope).expect_err("a bad body");
        assert_eq!(err.code, codes::BAD_UPSTREAM_BODY);
    }

    /* -------------------------------------------------------------- coercion */

    #[test]
    fn num_reads_a_json_number_and_a_csv_string_alike() {
        assert_eq!(num(&json!(0.19)), Some(0.19));
        assert_eq!(cell("4750"), Some(4750.0));
        assert_eq!(cell("0.98"), Some(0.98));
    }

    #[test]
    fn num_keeps_a_stated_zero_and_drops_an_unstated_figure() {
        // The difference the whole terminal rests on: 0 is the exchange saying
        // zero, None is the exchange saying nothing.
        assert_eq!(num(&json!(0)), Some(0.0));
        assert_eq!(cell("0"), Some(0.0));
        assert_eq!(num(&Value::Null), None);
        assert_eq!(num_of(None), None);
        assert_eq!(cell(""), None);
    }

    #[test]
    fn num_refuses_every_other_thing_a_generous_coercion_would_call_zero() {
        // A blank archive cell is the venue declining to state a figure, not
        // stating that it is nothing.
        assert_eq!(cell(" "), None);
        assert_eq!(cell("\t"), None);
        assert_eq!(num(&json!(false)), None);
        assert_eq!(num(&json!([])), None);
        assert_eq!(cell("n/a"), None);
    }

    /* ------------------------------------------------------------ identifiers */

    #[test]
    fn event_ticker_takes_the_product_and_its_period_which_is_what_an_event_is_here() {
        assert_eq!(event_ticker_of("HORC_1126_Republican"), "HORC_1126");
        assert_eq!(event_ticker_of("UHBKF_082326_91"), "UHBKF_082326");
    }

    #[test]
    fn event_ticker_is_unchanged_by_the_nine_four_segment_conditional_ids() {
        assert_eq!(event_ticker_of("ZFFCP_091626_E0X0926_3.4"), "ZFFCP_091626");
        assert_eq!(
            event_ticker_of("ZFFCP_091626_E-25X0926_4.2"),
            "ZFFCP_091626"
        );
    }

    #[test]
    fn event_ticker_answers_in_the_case_the_registry_folds_ids_to() {
        assert_eq!(event_ticker_of("horc_1126_republican"), "HORC_1126");
    }

    #[test]
    fn strike_reads_the_last_segment_not_the_third() {
        // Index 2 would yield the scenario code `E0X0926`, which renders as a
        // strike label and parses as nothing.
        assert_eq!(strike_of("ZFFCP_091626_E0X0926_3.4"), "3.4");
        assert_eq!(strike_of("ZFFCP_091626_E-25X0926_4.2"), "4.2");
    }

    #[test]
    fn strike_is_identical_to_the_third_segment_on_a_three_segment_id() {
        assert_eq!(strike_of("HORC_1126_Republican"), "Republican");
        assert_eq!(strike_of("UHBKF_082326_91"), "91");
        assert_eq!(strike_of("ACD_1226_432.0"), "432.0");
    }

    #[test]
    fn strike_states_none_when_the_id_carries_none() {
        assert_eq!(strike_of("HORC_1126"), "");
    }

    /* ----------------------------------------------------------------- market */

    #[test]
    fn market_carries_prices_through_as_dollars_and_the_ticker_in_the_registry_case() {
        let market = normalise_market(&contract("HORC_1126_Republican"), Some(&product("HORC")));
        assert_eq!(market.venue, Venue::ForecastEx);
        // Upper, because that is the case the registry hands `get_market`.
        assert_eq!(market.ticker, "HORC_1126_REPUBLICAN");
        assert_eq!(market.event_ticker, "HORC_1126");
        assert_eq!(market.series_ticker, "HORC");
        assert_eq!(market.last_price, Some(0.19));
        assert_eq!(market.category.as_deref(), Some("Elections"));
        assert_eq!(market.market_type, "binary");
        assert!(market.title.starts_with("Will the Republican Party"));
    }

    #[test]
    fn market_states_no_bid_ask_or_depth_because_the_exchange_has_no_book() {
        let market = normalise_market(&contract("HORC_1126_Republican"), Some(&product("HORC")));
        assert_eq!(market.yes_bid, None);
        assert_eq!(market.yes_ask, None);
        assert_eq!(market.no_bid, None);
        assert_eq!(market.no_ask, None);
        assert_eq!(market.liquidity, None);
    }

    #[test]
    fn market_carries_the_last_print_into_mid_rather_than_deriving_one_from_the_no_print() {
        // `FF_091726_0.875` is a live row whose two prints sum to 1.01, which is
        // what proves they are independent rather than mirrored.
        let raw = contract("FF_091726_0.875");
        assert_eq!(num_of(raw.last_yes_price.as_ref()), Some(0.98));
        assert_eq!(num_of(raw.last_no_price.as_ref()), Some(0.03));

        let market = normalise_market(&raw, Some(&product("FF")));
        assert_eq!(market.mid, Some(0.98));
        assert_eq!(market.mid, market.last_price);
    }

    #[test]
    fn market_reports_a_never_traded_contract_as_unpriced_not_as_worth_nothing() {
        let market = normalise_market(&contract("UHBKF_082326_85"), Some(&product("UHBKF")));
        assert_eq!(market.last_price, None);
        assert_eq!(market.mid, None);
    }

    #[test]
    fn market_keeps_an_open_interest_of_zero_which_the_venue_does_state() {
        assert_eq!(
            normalise_market(&contract("UHBKF_082326_85"), Some(&product("UHBKF"))).open_interest,
            Some(0.0)
        );
        assert_eq!(
            normalise_market(&contract("HORC_1126_Republican"), Some(&product("HORC")))
                .open_interest,
            Some(1_060_526.0)
        );
    }

    #[test]
    fn market_leaves_lifetime_volume_and_turnover_unstated_until_the_archive_answers() {
        let market = normalise_market(&contract("HORC_1126_Republican"), Some(&product("HORC")));
        assert_eq!(market.volume, None);
        assert_eq!(market.volume24h, None);
        assert_eq!(market.previous_price, None);
        assert_eq!(market.change, None);
    }

    #[test]
    fn market_claims_a_greater_strike_only_where_the_question_proves_the_direction() {
        let market = normalise_market(&contract("UHBKF_082326_91"), Some(&product("UHBKF")));
        assert_eq!(market.strike_type, Some(StrikeType::Greater));
        assert_eq!(market.floor_strike, Some(91.0));
        assert_eq!(market.cap_strike, None);
        assert_eq!(market.yes_sub_title, "Above 91");
        assert_eq!(market.no_sub_title, "91 or below");
    }

    #[test]
    fn market_reads_the_four_segment_strike_so_the_conditional_book_is_not_a_label() {
        let market = normalise_market(
            &contract("ZFFCP_091626_E0X0926_3.4"),
            Some(&product("ZFFCP")),
        );
        assert_eq!(market.floor_strike, Some(3.4));
        assert_eq!(market.strike_type, Some(StrikeType::Greater));
        assert_eq!(market.yes_sub_title, "Above 3.4");
    }

    #[test]
    fn market_states_no_strike_where_the_outcome_is_a_name_rather_than_a_number() {
        let market = normalise_market(&contract("HORC_1126_Republican"), Some(&product("HORC")));
        assert_eq!(market.strike_type, None);
        assert_eq!(market.floor_strike, None);
        // The canonical spelling survives in the label even though the ticker
        // folds.
        assert_eq!(market.yes_sub_title, "Republican");
        assert_eq!(market.no_sub_title, "No");
    }

    #[test]
    fn market_puts_a_below_ladders_strike_on_the_cap_not_the_floor() {
        // The id cannot tell these apart from the ones above — `ULATL_082326_73`
        // and `UHBKF_082326_91` are the same grammar — and 580 contracts ran this
        // way on 2026-08-23. Reading the strike as a floor would put the YES
        // region on the wrong side of the line.
        let market = normalise_market(&contract("ULATL_082326_73"), Some(&product("ULATL")));
        assert_eq!(market.strike_type, Some(StrikeType::Less));
        assert_eq!(market.cap_strike, Some(73.0));
        assert_eq!(market.floor_strike, None);
        assert_eq!(market.yes_sub_title, "Below 73");
        assert_eq!(market.no_sub_title, "73 or above");
    }

    #[test]
    fn market_keeps_at_least_inclusive_so_the_rung_itself_settles_yes() {
        let market = normalise_market(&contract("HCAB_1226_11"), Some(&product("HCAB")));
        assert_eq!(market.strike_type, Some(StrikeType::GreaterOrEqual));
        assert_eq!(market.floor_strike, Some(11.0));
        assert_eq!(market.yes_sub_title, "11 or above");
        assert_eq!(market.no_sub_title, "Below 11");
    }

    #[test]
    fn market_states_no_bound_for_a_question_that_names_an_exact_value_not_a_ladder() {
        // "Exactly one dissenter" has a number in its id and no direction at all.
        let market = normalise_market(&contract("DISSN_091626_1"), Some(&product("DISSN")));
        assert_eq!(market.strike_type, None);
        assert_eq!(market.floor_strike, None);
        assert_eq!(market.cap_strike, None);
    }

    #[test]
    fn market_writes_a_percentage_strike_the_way_the_product_says_to_read_it() {
        let market = normalise_market(&contract("FF_091726_0.875"), Some(&product("FF")));
        assert_eq!(product("FF").strike_unit.as_deref(), Some("Percentage"));
        assert_eq!(market.strike_type, Some(StrikeType::Greater));
        assert_eq!(market.floor_strike, Some(0.875));
        assert_eq!(market.yes_sub_title, "Above 0.875%");
    }

    #[test]
    fn market_stamps_the_central_offset_onto_the_zoneless_close_and_expiry() {
        let market = normalise_market(&contract("HORC_1126_Republican"), Some(&product("HORC")));
        // January in Chicago is CST; the exchange's own CSV agrees on -06:00.
        assert_eq!(market.expiration_time, "2027-01-04T15:00:00-06:00");
        assert_eq!(market.close_time, "2027-01-04T15:00:00-06:00");
        // Summer is CDT, and the archive row for the same contract spells it
        // `-05:00` too.
        let denver = normalise_market(&contract("UHBKF_082326_91"), Some(&product("UHBKF")));
        assert_eq!(denver.expiration_time, "2026-08-24T02:00:00-05:00");
        assert!(session("UHBKF_082326_91")
            .expiration_date
            .ends_with("-05:00"));
    }

    #[test]
    fn market_reports_no_settlement_and_no_open_time_which_is_all_the_catalogue_holds() {
        let market = normalise_market(&contract("HORC_1126_Republican"), Some(&product("HORC")));
        // Settled contracts leave the catalogue, so nothing reachable has a
        // result.
        assert_eq!(market.result, "");
        assert_eq!(market.open_time, "");
        assert!(market.rules_primary.ends_with(".pdf"));
    }

    #[test]
    fn market_reads_an_expiry_already_past_as_closed() {
        // The catalogue holds open contracts only, so this only fires in the
        // minutes either side of a close — but a contract whose expiry has gone
        // by must not still read `open`.
        let market = normalise_market(
            &contract_of(json!({
                "contract_id": "HORC_1126_Republican",
                "expiration_date": "2020-01-04T15:00:00"
            })),
            None,
        );
        assert_eq!(market.status, "closed");
        assert_eq!(
            normalise_market(&contract("HORC_1126_Republican"), None).status,
            "open"
        );
    }

    #[test]
    fn market_labels_a_leg_with_no_strike_at_all_as_a_plain_yes() {
        let market = normalise_market(
            &contract_of(json!({ "contract_id": "HORC_1126", "question": "Anything?" })),
            None,
        );
        assert_eq!(market.yes_sub_title, "Yes");
        assert_eq!(market.no_sub_title, "No");
    }

    /* ---------------------------------------------------------------- archive */

    #[test]
    fn archive_keeps_one_row_per_contract_since_the_no_row_is_the_yes_row_mirrored() {
        // Sixteen lines, eight contracts, and only the YES half survives.
        assert_eq!(archive().len(), 8);
        assert_eq!(session("HORC_1126_REPUBLICAN").end_price, "0.19");
        assert_eq!(session("HORC_1126_REPUBLICAN").subtype, "YES");
    }

    #[test]
    fn archive_keys_on_the_folded_id_which_is_the_case_the_terminal_asks_in() {
        assert!(archive().contains_key("FF_091726_0.875"));
        assert!(!archive().contains_key("ff_091726_0.875"));
    }

    #[test]
    fn archive_keeps_an_empty_settlement_price_as_empty_rather_than_parsing_it() {
        // It was blank on 54.0% of rows on 2026-08-23; a parse would produce a
        // zero for every one of them.
        assert_eq!(session("UST_1230_54.41").settlement_price, "");
        assert_eq!(cell(&session("UST_1230_54.41").settlement_price), None);
        assert_eq!(session("HORC_1126_REPUBLICAN").settlement_price, "0.19");
    }

    #[test]
    fn archive_reads_a_quoted_id_containing_commas_which_eight_live_contracts_have() {
        // Verbatim from prices/daily_prices_20260823.csv. The conditional books
        // put commas inside the identifier, so the exchange quotes the cell and
        // the line carries fourteen commas against a twelve-column header. Split
        // naively, the fields shift two left, `subtype` reads `House-R` instead
        // of `YES`, the row is filtered away, and the contract's published
        // turnover and previous close reach the terminal as `None` — the
        // terminal's way of saying the exchange never stated them.
        let row = session("FEDRO_1128_SENATE-R,HOUSE-R,PRESIDENT-D");
        assert_eq!(row.subtype, "YES");
        assert_eq!(row.end_price, "0.09");
        assert_eq!(row.open_interest, "10");
        assert_eq!(
            row.event_contract,
            "FEDRO_1128_Senate-R,House-R,President-D"
        );
    }

    #[test]
    fn archive_reads_a_doubled_quote_inside_a_quoted_cell_as_one_literal_quote() {
        let rows = parse_archive_csv(
            "event_contract,subtype,end_price,pair_quantity,open_interest\n\
             \"A_1_x\"\"y\",YES,0.40,7,9\n",
        );
        assert_eq!(
            rows.get("A_1_X\"Y").map(|r| r.end_price.as_str()),
            Some("0.40")
        );
    }

    #[test]
    fn archive_reads_by_header_so_a_new_column_cannot_shift_the_ones_after_it() {
        // `vwap` was added to this file during its two years on the bucket. A
        // positional reader would have shifted every field past the insertion
        // point without saying so.
        let rows = parse_archive_csv(
            "vwap,open_interest,pair_quantity,end_price,subtype,event_contract\n\
             0.31,33158,129763,0.15,YES,UHLAX_082326_83\n",
        );
        let row = rows.get("UHLAX_082326_83").expect("the row survives");
        assert_eq!(row.end_price, "0.15");
        assert_eq!(row.pair_quantity, "129763");
    }

    #[test]
    fn archive_drops_a_row_the_file_cut_short_rather_than_shifting_its_fields() {
        let rows = parse_archive_csv(
            "event_contract,subtype,expiration_date,date,start_price,high_price,low_price,\
             end_price,settlement_price,pair_quantity,open_interest,vwap\n\
             A_1_2,YES,x,2026-08-23,0.10,0.00,0.00,0.10,0.10,0,5,0.00\n\
             B_1_2,YES,x,2026-08-23\n",
        );
        assert_eq!(rows.len(), 1);
        assert!(rows.contains_key("A_1_2"));
    }

    /* --------------------------------------------------------- session merge */

    #[test]
    fn session_states_the_turnover_and_the_previous_close() {
        let market = apply_session(
            normalise_market(&contract("HORC_1126_Republican"), Some(&product("HORC"))),
            &session("HORC_1126_REPUBLICAN"),
        );
        assert_eq!(market.volume24h, Some(4750.0));
        assert_eq!(market.previous_price, Some(0.19));
        // The catalogue and the archive were captured within the same session, so
        // the live print and the archived close agree.
        assert_eq!(market.change, Some(0.0));
    }

    #[test]
    fn session_takes_the_move_from_the_live_print_against_the_archived_close() {
        // The archive is a session behind the tape, so a print that has moved
        // since the close is exactly what `change` is for.
        let mut raw = contract("HORC_1126_Republican");
        raw.last_yes_price = Some(json!(0.21));
        let market = apply_session(
            normalise_market(&raw, Some(&product("HORC"))),
            &session("HORC_1126_REPUBLICAN"),
        );
        assert_eq!(market.previous_price, Some(0.19));
        assert_eq!(market.change, Some(0.02));
    }

    #[test]
    fn session_keeps_a_traded_quantity_of_zero_which_is_a_session_that_really_was_quiet() {
        // `FFDEC_091626_E0` printed nothing that session and the file still
        // carries a mark for it.
        let market = apply_session(
            normalise_market(&contract("FFDEC_091626_E0"), Some(&product("FFDEC"))),
            &session("FFDEC_091626_E0"),
        );
        assert_eq!(market.volume24h, Some(0.0));
        assert_eq!(market.previous_price, Some(0.68));
    }

    #[test]
    fn session_states_no_previous_close_when_the_row_has_no_price_anywhere_in_it() {
        // 7,157 YES rows were untraded with start and end both 0.00 — the file
        // having no mark, not a contract worth nothing. A 0 here would print a
        // full-dollar move on the movers board.
        let market = apply_session(
            normalise_market(&contract("HORC_1126_Republican"), Some(&product("HORC"))),
            &session("UST_1230_54.41"),
        );
        assert_eq!(market.previous_price, None);
        assert_eq!(market.change, None);
        assert_eq!(market.volume24h, Some(0.0));
    }

    #[test]
    fn session_leaves_open_interest_as_the_live_catalogue_stated_it() {
        let market = apply_session(
            normalise_market(&contract("HORC_1126_Republican"), Some(&product("HORC"))),
            &session("HORC_1126_REPUBLICAN"),
        );
        // The archive's column is that session's close; the catalogue's is now,
        // and the two differ by the overnight session.
        assert_eq!(market.open_interest, Some(1_060_526.0));
        assert_eq!(session("HORC_1126_REPUBLICAN").open_interest, "1060526");
    }

    #[test]
    fn session_never_reads_the_high_low_or_vwap_an_untraded_row_writes_as_zero() {
        let row = session("FF_091726_0.875");
        // Untraded, and the file still writes 0.00 into all three. Reading any of
        // them would mark the contract at nothing.
        assert_eq!(row.high_price, "0.00");
        assert_eq!(row.low_price, "0.00");
        assert_eq!(row.vwap, "0.00");

        let market = apply_session(
            normalise_market(&contract("FF_091726_0.875"), Some(&product("FF"))),
            &row,
        );
        assert_eq!(market.last_price, Some(0.98));
        assert_eq!(market.previous_price, Some(0.98));
    }

    /* ------------------------------------------------------------------ event */

    #[test]
    fn event_groups_the_legs_under_the_product_and_period() {
        let legs = contracts(&["HORC_1126_Republican", "HORC_1126_Democratic"]);
        let event = normalise_event(&legs, Some(&product("HORC")), None);
        assert_eq!(event.venue, Venue::ForecastEx);
        assert_eq!(event.event_ticker, "HORC_1126");
        assert_eq!(event.series_ticker, "HORC");
        assert_eq!(
            event.title,
            "US House of Representatives Control November 2026"
        );
        assert_eq!(event.sub_title, "US House of Representatives Control");
        assert_eq!(event.category, "Elections");
        assert_eq!(event.markets.len(), 2);
        assert_eq!(event.markets[0].event_ticker, "HORC_1126");
    }

    #[test]
    fn event_marks_a_named_outcome_field_exclusive_because_those_legs_really_do_exclude() {
        let legs = contracts(&["HORC_1126_Republican", "HORC_1126_Democratic"]);
        assert!(normalise_event(&legs, Some(&product("HORC")), None).mutually_exclusive);
    }

    #[test]
    fn event_does_not_mark_a_numeric_ladder_exclusive_its_rungs_are_all_true_at_once() {
        let legs = contracts(&["UHBKF_082326_85", "UHBKF_082326_91"]);
        assert!(!normalise_event(&legs, Some(&product("UHBKF")), None).mutually_exclusive);
    }

    #[test]
    fn event_does_not_mark_the_conditional_book_exclusive_despite_its_string_strikes() {
        // `ZFFCP_091626` has a String strike unit and nine legs, and is three Fed
        // scenarios crossed with three cumulative CPI thresholds — a ladder
        // inside each scenario. Flagging it would put a fictional arbitrage on
        // screen across the whole Conditional category.
        let legs = contracts(&[
            "ZFFCP_091626_E0X0926_3.4",
            "ZFFCP_091626_E0X0926_3.8",
            "ZFFCP_091626_E-25X0926_3.4",
        ]);
        let event = normalise_event(&legs, Some(&product("ZFFCP")), None);
        assert_eq!(product("ZFFCP").strike_unit.as_deref(), Some("String"));
        assert_eq!(event.category, "Conditional");
        assert!(!event.mutually_exclusive);
    }

    #[test]
    fn event_claims_no_exclusivity_for_a_single_leg_which_excludes_nothing() {
        let legs = contracts(&["HORC_1126_Republican"]);
        assert!(!normalise_event(&legs, Some(&product("HORC")), None).mutually_exclusive);
    }

    #[test]
    fn event_merges_the_archive_into_the_legs_it_covers_and_leaves_the_rest_unstated() {
        let legs = contracts(&["HORC_1126_Republican", "UHBKF_082326_85"]);
        let rows = archive();
        let event = normalise_event(&legs, Some(&product("HORC")), Some(&rows));
        assert_eq!(event.markets[0].volume24h, Some(4750.0));
        // `UHBKF_082326_85` is not in this trimmed file: unstated, not zero.
        assert_eq!(event.markets[1].volume24h, None);
        assert_eq!(event.markets[1].previous_price, None);
    }

    #[test]
    fn event_relabels_a_coded_ladder_from_the_clause_its_questions_differ_by() {
        // The five September FOMC rungs. Printed as the exchange names them these
        // are `E0`, `E25`, `E-25`, `H50` and `M-50`, which no reader and no
        // cross-venue matcher can pair with `No change` at the other five
        // brokers.
        let legs = contracts(&[
            "FFDEC_091626_E0",
            "FFDEC_091626_E25",
            "FFDEC_091626_E-25",
            "FFDEC_091626_H50",
            "FFDEC_091626_M-50",
        ]);
        let event = normalise_event(&legs, Some(&product("FFDEC")), None);
        let labels: Vec<&str> = event
            .markets
            .iter()
            .map(|m| m.yes_sub_title.as_str())
            .collect();
        assert_eq!(
            labels,
            vec![
                "leave the rate unchanged",
                "raise the rate 25bps",
                "lower the rate 25bps",
                "raise the rate 50bps or more",
                "lower the rate 50bps or more",
            ]
        );
        // A String strike unit outside the Conditional category: these five
        // really are one winner-take-all field.
        assert!(event.mutually_exclusive);
    }

    #[test]
    fn event_leaves_a_numeric_ladder_labelled_by_its_own_rungs() {
        // A numeric ladder names itself, so the sibling diff is not run on it and
        // the strike label survives.
        let legs = contracts(&["UHBKF_082326_85", "UHBKF_082326_91"]);
        let event = normalise_event(&legs, Some(&product("UHBKF")), None);
        assert_eq!(event.markets[0].yes_sub_title, "Above 85");
        assert_eq!(event.markets[1].yes_sub_title, "Above 91");
    }

    /* ------------------------------------------------------ leg labelling */

    #[test]
    fn clauses_leave_the_words_the_legs_differ_by_from_both_ends_of_the_question() {
        let fomc: Vec<String> = [
            "Will the Fed leave the rate unchanged in September 2026?",
            "Will the Fed raise the rate 25bps in September 2026?",
            "Will the Fed lower the rate 25bps in September 2026?",
            "Will the Fed raise the rate 50bps or more in September 2026?",
            "Will the Fed lower the rate 50bps or more in September 2026?",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();

        assert_eq!(
            distinguishing_clauses(&fomc),
            Some(vec![
                "leave the rate unchanged".into(),
                "raise the rate 25bps".into(),
                "lower the rate 25bps".into(),
                "raise the rate 50bps or more".into(),
                "lower the rate 50bps or more".into(),
            ])
        );
    }

    #[test]
    fn clauses_strip_a_shared_tail_of_boilerplate_the_exchange_appends() {
        let senate: Vec<String> = [
            "Will Ashley Hinson win the Iowa general election for US Senate in 2026?",
            "Will Josh Turek win the Iowa general election for US Senate in 2026?",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();

        assert_eq!(
            distinguishing_clauses(&senate),
            Some(vec!["Ashley Hinson".into(), "Josh Turek".into()])
        );
    }

    #[test]
    fn clauses_give_up_on_a_single_leg_which_has_no_sibling_to_differ_from() {
        assert_eq!(
            distinguishing_clauses(&["Will the Fed leave the rate unchanged?".to_string()]),
            None
        );
        assert_eq!(distinguishing_clauses(&[]), None);
    }

    #[test]
    fn clauses_give_up_when_the_exchange_worded_two_legs_identically() {
        // It lists a few races twice, once by party and once by candidate, with
        // the same question on both. Diffing those leaves two blanks, and the
        // strike code is a worse label than the question but a better one than
        // nothing.
        let twice = vec![
            "Will Josh Turek win the Iowa general election?".to_string(),
            "Will Josh Turek win the Iowa general election?".to_string(),
        ];
        assert_eq!(distinguishing_clauses(&twice), None);
    }

    #[test]
    fn clauses_give_up_rather_than_blanking_a_leg_whose_question_prefixes_another() {
        // Every word of the first is shared, so diffing leaves it with nothing at
        // all — and a leg with no label is worse than a leg labelled `E0`.
        let nested = vec![
            "Will the Fed raise the rate".to_string(),
            "Will the Fed raise the rate 25bps".to_string(),
        ];
        assert_eq!(distinguishing_clauses(&nested), None);
    }

    #[test]
    fn clauses_treat_punctuation_as_part_of_the_word_which_keeps_a_trailing_rung_labelled() {
        // `rate?` and `rate` are not the same word, so the shared run stops short
        // and both legs keep a clause. The trailing mark is trimmed afterwards.
        let marked = vec![
            "Will the Fed raise the rate?".to_string(),
            "Will the Fed raise the rate 25bps?".to_string(),
        ];
        assert_eq!(
            distinguishing_clauses(&marked),
            Some(vec!["rate".into(), "rate 25bps".into()])
        );
    }

    #[test]
    fn clauses_ignore_case_when_deciding_which_words_are_shared() {
        let mixed = vec![
            "Will inflation exceed 3%?".to_string(),
            "will inflation exceed 4%?".to_string(),
        ];
        assert_eq!(
            distinguishing_clauses(&mixed),
            Some(vec!["3%".into(), "4%".into()])
        );
    }

    #[test]
    fn clauses_give_up_on_a_leg_with_no_question_at_all() {
        let blank = vec!["Will the Fed cut?".to_string(), String::new()];
        assert_eq!(distinguishing_clauses(&blank), None);
    }

    /* ------------------------------------------------------------------- tape */

    #[test]
    fn pairs_reads_the_whole_exchange_file_which_is_every_contract_at_once() {
        let pairs = parse_pairs_csv(PAIRS_CSV);
        assert_eq!(pairs.len(), 7);
        assert_eq!(pairs[0].pair_id, "C1D6NXZFR07Q");
        assert_eq!(pairs[0].event_contract, "UHBKF_082326_91");
    }

    #[test]
    fn trades_filter_to_the_contract_asked_for_matching_the_folded_ticker() {
        let pairs = parse_pairs_csv(PAIRS_CSV);
        let response = normalise_trades(&pairs, "HORC_1126_REPUBLICAN", 50);
        assert_eq!(response.trades.len(), 2);
        assert_eq!(response.trades[0].ticker, "HORC_1126_REPUBLICAN");
    }

    #[test]
    fn trades_sort_newest_first_because_the_file_is_only_roughly_time_ordered() {
        let pairs = parse_pairs_csv(PAIRS_CSV);
        let response = normalise_trades(&pairs, "UHBKF_082326_91", 50);
        assert_eq!(
            response
                .trades
                .iter()
                .map(|t| t.trade_id.as_str())
                .collect::<Vec<_>>(),
            vec!["C1EY89D6W07Q", "C1DR44TGE07Q", "C1D6NXZFR07Q"]
        );
        for pair in response.trades.windows(2) {
            assert!(pair[0].ts >= pair[1].ts);
        }
    }

    #[test]
    fn trades_read_the_nanosecond_fraction_and_the_central_offset_the_tape_carries() {
        // `2026-08-23T08:34:34.022716795-05:00` — nine fractional digits and an
        // explicit CDT offset. The same file also writes eight
        // (`...03.90169434-05:00`), so the digit count is not fixed.
        let pairs = parse_pairs_csv(PAIRS_CSV);
        let response = normalise_trades(&pairs, "HORC_1126_REPUBLICAN", 50);
        assert_eq!(response.trades[0].ts, 1_787_492_074);
        assert_eq!(response.trades[1].ts, 1_787_492_043);
    }

    #[test]
    fn trades_carry_both_prices_and_the_pair_count_as_the_exchange_states_them() {
        let pairs = parse_pairs_csv(PAIRS_CSV);
        let response = normalise_trades(&pairs, "HORC_1126_REPUBLICAN", 50);
        assert_eq!(response.trades[0].yes_price, 0.19);
        assert_eq!(response.trades[0].no_price, 0.81);
        assert_eq!(response.trades[0].count, 2750.0);
        assert!(!response.trades[0].is_block_trade);
        // 2,000 and 2,750 are exactly the 4,750 the archive files for that
        // session, which is what says the tape and the archive agree.
        let total: f64 = response.trades.iter().map(|t| t.count).sum();
        assert_eq!(
            cell(&session("HORC_1126_REPUBLICAN").pair_quantity),
            Some(total)
        );
    }

    #[test]
    fn trades_name_no_taker_because_a_paired_match_has_no_aggressor() {
        // Every print creates one YES and one NO holder at once. `yes` would
        // assert an initiative the matching model rules out, and the client draws
        // an unattributed print in neither colour.
        let pairs = parse_pairs_csv(PAIRS_CSV);
        let response = normalise_trades(&pairs, "HORC_1126_REPUBLICAN", 50);
        assert!(response.trades.iter().all(|t| t.taker_side.is_empty()));
    }

    #[test]
    fn trades_honour_the_limit_and_offer_no_cursor_since_a_page_is_a_session_file() {
        let pairs = parse_pairs_csv(PAIRS_CSV);
        let one = normalise_trades(&pairs, "HORC_1126_REPUBLICAN", 1);
        assert_eq!(one.trades.len(), 1);
        assert_eq!(one.cursor, None);
    }

    #[test]
    fn trades_return_nothing_for_a_contract_that_did_not_print() {
        let pairs = parse_pairs_csv(PAIRS_CSV);
        assert!(normalise_trades(&pairs, "ACD_1226_432.0", 50)
            .trades
            .is_empty());
    }

    #[test]
    fn trades_drop_a_row_they_cannot_read_rather_than_printing_a_zero_lot_dated_1970() {
        // S3 rewrites the tape every ten minutes, so a reader can arrive
        // mid-write and take a torn last line. Coercing the blanks to numbers
        // would seat a fabricated print at the top of the panel.
        let torn = parse_pairs_csv(
            "pair_id,event_contract,expiration_date,quantity,yes_price,no_price,pair_time\n\
             C1R8PPJYG07Q,HORC_1126_Republican,2027-01-04T15:00:00-06:00,2750,0.19,0.81,\
             2026-08-23T08:34:34.022716795-05:00\n\
             C1R8PPJYH07Q,HORC_1126_Republican,2027-01-04T15:00:00-06:00,,,,\n",
        );
        // The torn row survives the CSV read — it has every cell, just empty ones.
        assert_eq!(torn.len(), 2);

        let response = normalise_trades(&torn, "HORC_1126_REPUBLICAN", 50);
        assert_eq!(response.trades.len(), 1);
        assert_eq!(response.trades[0].trade_id, "C1R8PPJYG07Q");
    }

    /* ---------------------------------------------------------------- candles */

    #[test]
    fn sample_end_closes_an_hourly_bar_an_hour_after_the_stamp_which_opens_it() {
        // Reconciled against the tape on 2026-08-23: the bucket stamped
        // 10:00:00Z on UHLAX_082326_83 carries volume 172, and the prints between
        // 10:00Z and 11:00Z sum to 172. The stamp is the bucket's start, and
        // `Candle::time` is a period end everywhere in the terminal.
        assert_eq!(
            sample_end("2026-08-23T10:00:00Z", CandleInterval::OneHour),
            Some(1_787_482_800)
        );
    }

    #[test]
    fn sample_end_closes_a_daily_bar_at_the_end_of_the_utc_day_not_the_central_session() {
        // The daily point is the sum of that UTC date's hourly buckets — 716 on
        // 2026-08-22 for UHLAX_082326_83, exactly — while the exchange's own
        // archive files the 8/23 session (16:15 CT to 16:14 CT) as 129,763
        // against the daily feed's 173,091. Closing at Central midnight would
        // stamp every bar five hours late.
        assert_eq!(
            sample_end("2026-08-22", CandleInterval::OneDay),
            Some(1_787_443_200)
        );
        // No seasonal offset applies: a UTC day is a UTC day in January too.
        assert_eq!(
            sample_end("2026-01-14", CandleInterval::OneDay),
            Some(1_768_435_200)
        );
    }

    #[test]
    fn sample_end_states_nothing_for_a_missing_or_unreadable_stamp() {
        assert_eq!(sample_end("", CandleInterval::OneHour), None);
        assert_eq!(sample_end("not a date", CandleInterval::OneHour), None);
    }

    #[test]
    fn sample_end_reads_a_zoneless_hourly_stamp_as_utc_rather_than_as_local_time() {
        // The hourly feed always writes `Z`, but reading a bare one as local time
        // would move the whole series by the server's own offset.
        assert_eq!(
            sample_end("2026-08-23T10:00:00", CandleInterval::OneHour),
            sample_end("2026-08-23T10:00:00Z", CandleInterval::OneHour)
        );
    }

    /// Two hourly buckets off `UHLAX_082326_83`, with one figure changed.
    ///
    /// The stamps and both prices are exactly what `/api/prices?interval=h`
    /// returned on 2026-08-23, and the 10:00Z bucket's 172 is the figure that
    /// reconciles against the tape. The 11:00Z bucket really carried 408; it is
    /// written as `0` here because a zero-volume sample is the case worth
    /// pinning — the feed emits one in every bucket whether or not anything
    /// printed, and that zero is the exchange distinguishing a carried-forward
    /// mark from a real one.
    fn hourly() -> Vec<RawFexPricePoint> {
        points_of(json!([
            {
                "instrument_id": "UHLAX_082326_83",
                "yes_price": 0.26, "no_price": 0.74,
                "interval_date": "2026-08-23T10:00:00Z", "volume": 172
            },
            {
                "instrument_id": "UHLAX_082326_83",
                "yes_price": 0.35, "no_price": 0.65,
                "interval_date": "2026-08-23T11:00:00Z", "volume": 0
            }
        ]))
    }

    #[test]
    fn buckets_draw_one_bar_per_sample_its_high_and_low_being_its_close() {
        let candles = bucket_samples(&hourly(), CandleInterval::OneHour);
        assert_eq!(candles.len(), 2);
        assert_eq!(
            candles
                .iter()
                .map(|c| (c.open, c.high, c.low, c.close))
                .collect::<Vec<_>>(),
            vec![(0.26, 0.26, 0.26, 0.26), (0.35, 0.35, 0.35, 0.35)]
        );
    }

    #[test]
    fn buckets_read_a_zero_volume_sample_as_the_mark_carried_forward_and_keep_the_zero() {
        let candles = bucket_samples(&hourly(), CandleInterval::OneHour);
        assert_eq!(candles[0].volume, Some(172.0));
        assert!(candles[0].traded);
        assert_eq!(candles[1].volume, Some(0.0));
        assert!(!candles[1].traded);
    }

    #[test]
    fn buckets_state_no_open_interest_or_quote_which_the_feed_never_carries() {
        let candles = bucket_samples(&hourly(), CandleInterval::OneHour);
        assert_eq!(candles[0].open_interest, None);
        assert_eq!(candles[0].bid, None);
        assert_eq!(candles[0].ask, None);
    }

    #[test]
    fn buckets_come_out_oldest_first_however_the_feed_ordered_them() {
        let candles = bucket_samples(
            &points_of(json!([
                { "yes_price": 0.21, "interval_date": "2026-08-23", "volume": 100 },
                { "yes_price": 0.32, "interval_date": "2025-04-09", "volume": 10000 }
            ])),
            CandleInterval::OneDay,
        );
        assert_eq!(
            candles.iter().map(|c| c.close).collect::<Vec<_>>(),
            vec![0.32, 0.21]
        );
        assert!(candles[0].time < candles[1].time);
    }

    #[test]
    fn buckets_fold_a_repeated_stamp_into_one_bar_instead_of_two_at_one_instant() {
        let candles = bucket_samples(
            &points_of(json!([
                { "yes_price": 0.2, "interval_date": "2026-08-23T10:00:00Z", "volume": 10 },
                { "yes_price": 0.24, "interval_date": "2026-08-23T10:00:00Z", "volume": 5 }
            ])),
            CandleInterval::OneHour,
        );
        assert_eq!(candles.len(), 1);
        assert_eq!(candles[0].high, 0.24);
        assert_eq!(candles[0].low, 0.2);
        assert_eq!(candles[0].close, 0.24);
        assert_eq!(candles[0].volume, Some(15.0));
    }

    #[test]
    fn buckets_drop_a_sample_with_no_price_rather_than_charting_it_as_zero() {
        let candles = bucket_samples(
            &points_of(json!([
                { "yes_price": null, "interval_date": "2026-08-23", "volume": 0 }
            ])),
            CandleInterval::OneDay,
        );
        assert!(candles.is_empty());
    }

    #[test]
    fn intervals_are_named_the_single_letters_the_exchange_accepts() {
        // `H` answers an envelope statusCode of 400 saying `interval must be 'h'
        // or 'd'` — the case matters here as much as it does on the id.
        assert_eq!(interval_name(CandleInterval::OneHour), Some("h"));
        assert_eq!(interval_name(CandleInterval::OneDay), Some("d"));
        assert_eq!(interval_name(CandleInterval::OneMinute), None);
    }

    /* ----------------------------------------------------------------- series */

    #[test]
    fn series_spells_out_the_frequency_code_the_product_registry_states() {
        assert_eq!(normalise_series(&product("HORC")).frequency, "monthly");
        assert_eq!(normalise_series(&product("UHBKF")).frequency, "daily");
        // `I` is a one-off listing rather than a recurring cadence.
        assert_eq!(normalise_series(&product("FF")).frequency, "irregular");
    }

    #[test]
    fn series_carries_the_category_the_product_states_which_is_what_list_series_filters_on() {
        let series = normalise_series(&product("HORC"));
        assert_eq!(series.venue, Venue::ForecastEx);
        assert_eq!(series.ticker, "HORC");
        assert_eq!(series.title, "US House of Representatives Control");
        assert_eq!(series.category, "Elections");
    }

    #[test]
    fn series_tags_the_strike_unit_which_is_what_says_whether_a_strike_is_a_number() {
        assert_eq!(
            normalise_series(&product("HORC")).tags,
            vec!["String", "HORC=MAN", "United States Congress"]
        );
        // Missing fields are dropped rather than tagged as empty strings.
        assert_eq!(
            normalise_series(&RawFexProduct {
                product_id: Some("ZFFCP".into()),
                strike_unit: Some("String".into()),
                ..RawFexProduct::default()
            })
            .tags,
            vec!["String"]
        );
    }

    /* ---------------------------------------------------------------- session */

    #[test]
    fn session_date_files_a_print_made_after_the_16_15_ct_roll_under_the_next_session() {
        // 2026-08-23T21:30Z is 16:30 in Chicago, and the bucket agreed: at 18:39
        // CT that Sunday, pairs_20260824.csv already held that evening's prints.
        assert_eq!(session_date(instant("2026-08-23T21:30:00Z")), "20260824");
    }

    #[test]
    fn session_date_keeps_an_afternoon_print_in_the_session_it_traded_in() {
        assert_eq!(session_date(instant("2026-08-23T20:00:00Z")), "20260823");
        // 16:14 CT is the last minute of the session that is closing.
        assert_eq!(session_date(instant("2026-08-23T21:14:00Z")), "20260823");
    }

    #[test]
    fn session_date_rolls_the_month_and_the_year_with_the_session() {
        assert_eq!(session_date(instant("2026-12-31T23:00:00Z")), "20270101");
        assert_eq!(session_date(instant("2026-08-31T22:00:00Z")), "20260901");
    }

    #[test]
    fn previous_session_walks_back_over_a_month_and_a_year_boundary() {
        assert_eq!(previous_session("20260824"), "20260823");
        assert_eq!(previous_session("20260901"), "20260831");
        assert_eq!(previous_session("20270101"), "20261231");
    }

    /* ------------------------------------------------------------- US Central */

    /// An RFC 3339 instant, for writing a test's clock readably.
    fn instant(text: &str) -> i64 {
        OffsetDateTime::parse(text, &Rfc3339)
            .expect("the test literal is RFC 3339")
            .unix_timestamp()
    }

    #[test]
    fn central_iso_spells_out_the_offset_the_exchange_leaves_off_its_timestamps() {
        assert_eq!(
            central_iso("2026-08-24T02:00:00"),
            "2026-08-24T02:00:00-05:00"
        );
        assert_eq!(
            central_iso("2027-01-04T15:00:00"),
            "2027-01-04T15:00:00-06:00"
        );
    }

    #[test]
    fn central_iso_leaves_a_string_it_cannot_read_alone_rather_than_inventing_an_instant() {
        assert_eq!(central_iso(""), "");
        assert_eq!(central_iso("whenever"), "whenever");
    }

    #[test]
    fn central_offset_switches_on_the_second_sunday_in_march() {
        // 2026's transition is 2026-03-08T08:00:00Z: 01:59 CST becomes 03:00 CDT.
        assert_eq!(
            central_offset(instant("2026-03-08T07:59:00Z")),
            CENTRAL_STANDARD
        );
        assert_eq!(
            central_offset(instant("2026-03-08T08:00:00Z")),
            CENTRAL_DAYLIGHT
        );
    }

    #[test]
    fn central_offset_switches_back_on_the_first_sunday_in_november() {
        // 2026's transition is 2026-11-01T07:00:00Z: 01:59 CDT becomes 01:00 CST.
        assert_eq!(
            central_offset(instant("2026-11-01T06:59:00Z")),
            CENTRAL_DAYLIGHT
        );
        assert_eq!(
            central_offset(instant("2026-11-01T07:00:00Z")),
            CENTRAL_STANDARD
        );
    }

    #[test]
    fn central_instant_lands_on_the_right_side_of_a_spring_boundary() {
        // The two-pass read matters here: taken at face value the naive instant
        // sits on the standard side of the jump, and the offset that actually
        // applies to the answer is the daylight one.
        assert_eq!(
            central_instant("2026-03-08T03:00:00"),
            Some(instant("2026-03-08T08:00:00Z"))
        );
        assert_eq!(
            central_instant("2026-03-08T01:00:00"),
            Some(instant("2026-03-08T07:00:00Z"))
        );
    }

    #[test]
    fn central_instant_lands_on_the_right_side_of_an_autumn_boundary() {
        assert_eq!(
            central_instant("2026-11-01T00:30:00"),
            Some(instant("2026-11-01T05:30:00Z"))
        );
        assert_eq!(
            central_instant("2026-11-01T03:00:00"),
            Some(instant("2026-11-01T09:00:00Z"))
        );
    }

    #[test]
    fn central_instant_reads_a_bare_date_as_midnight_and_refuses_a_non_date() {
        assert_eq!(
            central_instant("2026-08-23"),
            Some(instant("2026-08-23T05:00:00Z"))
        );
        assert_eq!(central_instant("not a date"), None);
        assert_eq!(central_instant(""), None);
    }

    /* --------------------------------------------------------------- the wire */

    /// Both surfaces stand behind one mock: the API paths all begin `/api` and
    /// the bucket's begin `/prices` or `/pairs`, so nothing collides.
    fn state_for(server: &MockServer) -> AppState {
        AppState::new(Config {
            forecastex_api_base: server.uri(),
            forecastex_archive_base: server.uri(),
            ..Config::default()
        })
    }

    fn contracts_body() -> Value {
        serde_json::from_str(CONTRACTS_JSON).expect("the captured contracts page parses")
    }

    fn products_body() -> Value {
        serde_json::from_str(PRODUCTS_JSON).expect("the captured products page parses")
    }

    async fn mount_catalogue(server: &MockServer) {
        Mock::given(method("GET"))
            .and(path_matcher("/api/contracts"))
            .respond_with(ResponseTemplate::new(200).set_body_json(contracts_body()))
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(path_matcher("/api/products"))
            .respond_with(ResponseTemplate::new(200).set_body_json(products_body()))
            .mount(server)
            .await;
    }

    /// The bucket answering 404 for every file, which is what the hour after the
    /// 16:15 CT roll looks like.
    async fn mount_empty_archive(server: &MockServer) {
        Mock::given(method("GET"))
            .and(path_matcher_prefix("/prices/"))
            .respond_with(ResponseTemplate::new(404).set_body_string("NoSuchKey"))
            .mount(server)
            .await;
    }

    /// wiremock matches a path exactly, so a per-file mock is mounted for each
    /// session the walk-back can reach.
    fn path_matcher_prefix(prefix: &'static str) -> impl wiremock::Match {
        struct Prefix(&'static str);
        impl wiremock::Match for Prefix {
            fn matches(&self, request: &wiremock::Request) -> bool {
                request.url.path().starts_with(self.0)
            }
        }
        Prefix(prefix)
    }

    #[tokio::test]
    async fn the_crawl_orders_by_contract_id_because_open_interest_does_not_break_ties() {
        // Crawling the same three pages by `open_interest desc` on 2026-08-23
        // returned 11,750 rows and 10,270 distinct ids: the offset window shifts
        // between requests, so 1,480 contracts arrive twice and as many never do.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/api/contracts"))
            .and(query_param("sortBy", "contract_id"))
            .and(query_param("sortOrder", "asc"))
            .and(query_param("pageSize", "5000"))
            .respond_with(ResponseTemplate::new(200).set_body_json(contracts_body()))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path_matcher("/api/products"))
            .respond_with(ResponseTemplate::new(200).set_body_json(products_body()))
            .mount(&server)
            .await;
        mount_empty_archive(&server).await;

        let corpus = corpus_snapshot(&state_for(&server))
            .await
            .expect("the crawl completes");

        assert_eq!(corpus.venue, Venue::ForecastEx);
        assert_eq!(corpus.markets.len(), 16);
        assert_eq!(corpus.events.len(), 8);
        // `next_page` was null, so the catalogue ran out before the page cap did.
        assert!(!corpus.truncated);
    }

    #[tokio::test]
    async fn the_snapshot_is_built_once_and_shared() {
        let server = MockServer::start().await;
        mount_catalogue(&server).await;
        mount_empty_archive(&server).await;

        let state = state_for(&server);
        let first = corpus_snapshot(&state).await.expect("a snapshot");
        let second = corpus_snapshot(&state).await.expect("a snapshot");
        assert!(Arc::ptr_eq(&first, &second));
    }

    #[tokio::test]
    async fn the_crawl_dedupes_on_the_id_so_a_repeated_row_cannot_double_an_event() {
        let server = MockServer::start().await;
        let mut doubled = contracts_fixture();
        doubled.extend(contracts(&["HORC_1126_Republican", "HORC_1126_Democratic"]));
        Mock::given(method("GET"))
            .and(path_matcher("/api/contracts"))
            .respond_with(ResponseTemplate::new(200).set_body_json(envelope_json(&doubled)))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path_matcher("/api/products"))
            .respond_with(ResponseTemplate::new(200).set_body_json(products_body()))
            .mount(&server)
            .await;
        mount_empty_archive(&server).await;

        let corpus = corpus_snapshot(&state_for(&server))
            .await
            .expect("the crawl completes");
        assert_eq!(corpus.markets.len(), 16);
    }

    /// Re-wrap rows into the envelope the exchange would have sent them in,
    /// `body.data` string and all.
    fn envelope_json(rows: &[RawFexContract]) -> Value {
        let data: Vec<Value> = rows
            .iter()
            .map(|row| {
                json!({
                    "contract_id": row.contract_id,
                    "product_id": row.product_id,
                    "category": row.category,
                    "event_display_name": row.event_display_name,
                    "question": row.question,
                    "expiration_date": row.expiration_date,
                    "last_trade_date": row.last_trade_date,
                    "open_interest": row.open_interest,
                    "last_yes_price": row.last_yes_price,
                    "last_no_price": row.last_no_price,
                })
            })
            .collect();
        json!({
            "statusCode": 200,
            "body": {
                "data": serde_json::to_string(&data).expect("the rows serialise"),
                "next_page": null,
                "total_pages": 1
            }
        })
    }

    #[tokio::test]
    async fn get_market_merges_the_archive_a_session_behind_the_live_print() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/api/contracts"))
            .and(query_param("contractId", "HORC_1126_REPUBLICAN"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(envelope_json(&contracts(&["HORC_1126_Republican"]))),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path_matcher("/api/products"))
            .respond_with(ResponseTemplate::new(200).set_body_json(products_body()))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path_matcher_prefix("/prices/"))
            .respond_with(ResponseTemplate::new(200).set_body_string(ARCHIVE_CSV))
            .mount(&server)
            .await;

        let market = get_market(&state_for(&server), "HORC_1126_REPUBLICAN")
            .await
            .expect("the contract resolves");

        assert_eq!(market.ticker, "HORC_1126_REPUBLICAN");
        // The live catalogue states the print and the open interest.
        assert_eq!(market.last_price, Some(0.19));
        assert_eq!(market.open_interest, Some(1_060_526.0));
        // Turnover and the previous close come from the archive, one session
        // behind.
        assert_eq!(market.volume24h, Some(4750.0));
        assert_eq!(market.previous_price, Some(0.19));
    }

    #[tokio::test]
    async fn get_market_quotes_without_the_archive_rather_than_failing_with_it() {
        // The file 404s for the hour after every roll. Turnover and the daily
        // move stay unstated, which is what they were before the file existed.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/api/contracts"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(envelope_json(&contracts(&["HORC_1126_Republican"]))),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path_matcher("/api/products"))
            .respond_with(ResponseTemplate::new(200).set_body_json(products_body()))
            .mount(&server)
            .await;
        mount_empty_archive(&server).await;

        let market = get_market(&state_for(&server), "HORC_1126_REPUBLICAN")
            .await
            .expect("the contract resolves without the archive");
        assert_eq!(market.last_price, Some(0.19));
        assert_eq!(market.volume24h, None);
        assert_eq!(market.previous_price, None);
        assert_eq!(market.change, None);
    }

    #[tokio::test]
    async fn get_market_quotes_without_the_product_registry_too() {
        // The registry only decorates: it supplies the strike unit and the series
        // name, and a failure there must not cost the reader the quote.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/api/contracts"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(envelope_json(&contracts(&["FF_091726_0.875"]))),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path_matcher("/api/products"))
            .respond_with(ResponseTemplate::new(500).set_body_string("nope"))
            .mount(&server)
            .await;
        mount_empty_archive(&server).await;

        let market = get_market(&state_for(&server), "FF_091726_0.875")
            .await
            .expect("the contract resolves without its product");
        assert_eq!(market.last_price, Some(0.98));
        // Without the `Percentage` unit the strike is still a number and still
        // has a direction; only the `%` sign is lost.
        assert_eq!(market.floor_strike, Some(0.875));
        assert_eq!(market.yes_sub_title, "Above 0.875");
    }

    #[tokio::test]
    async fn get_market_reports_an_unknown_id_as_not_found_with_the_shape_to_retype() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/api/contracts"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "statusCode": 200,
                "body": { "data": "[]", "next_page": null, "total_pages": 0 }
            })))
            .mount(&server)
            .await;

        let err = get_market(&state_for(&server), "NOPE_1126_NOPE")
            .await
            .expect_err("an unlisted id is not found");
        assert_eq!(err.code, codes::NOT_FOUND);
        assert_eq!(err.message, "No ForecastEx contract NOPE_1126_NOPE");
        assert!(err.hint.unwrap().contains("PRODUCT_PERIOD_STRIKE"));
    }

    #[tokio::test]
    async fn get_event_gathers_the_legs_of_one_period_out_of_the_products_contracts() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/api/contracts"))
            .and(query_param("productId", "FFDEC"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(envelope_json(&contracts(&[
                    "FFDEC_091626_E0",
                    "FFDEC_091626_E25",
                    "FFDEC_091626_E-25",
                    "FFDEC_091626_H50",
                    "FFDEC_091626_M-50",
                ]))),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path_matcher("/api/products"))
            .respond_with(ResponseTemplate::new(200).set_body_json(products_body()))
            .mount(&server)
            .await;
        mount_empty_archive(&server).await;

        let event = get_event(&state_for(&server), "FFDEC_091626_E0")
            .await
            .expect("the event resolves");
        assert_eq!(event.event_ticker, "FFDEC_091626");
        assert_eq!(event.series_ticker, "FFDEC");
        assert_eq!(event.markets.len(), 5);
        assert!(event.mutually_exclusive);
        assert_eq!(event.markets[0].yes_sub_title, "leave the rate unchanged");
    }

    #[tokio::test]
    async fn get_event_reports_a_period_the_product_does_not_list_as_not_found() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/api/contracts"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(envelope_json(&contracts(&["FFDEC_091626_E0"]))),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path_matcher("/api/products"))
            .respond_with(ResponseTemplate::new(200).set_body_json(products_body()))
            .mount(&server)
            .await;
        mount_empty_archive(&server).await;

        let err = get_event(&state_for(&server), "FFDEC_991199_E0")
            .await
            .expect_err("a period nobody lists is not found");
        assert_eq!(err.code, codes::NOT_FOUND);
        assert!(err.hint.unwrap().contains("first two segments"));
    }

    #[tokio::test]
    async fn get_candles_restores_the_canonical_case_before_asking_for_prices() {
        // The trap this module exists to avoid: /api/prices answers an
        // upper-cased id with HTTP 200 and an empty series, so a chart drawn from
        // the registry's own folded ticker is silently blank.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/api/contracts"))
            .and(query_param("contractId", "HORC_1126_REPUBLICAN"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(envelope_json(&contracts(&["HORC_1126_Republican"]))),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path_matcher("/api/prices"))
            // The exact spelling the catalogue published, not the folded one.
            .and(query_param("contractId", "HORC_1126_Republican"))
            .and(query_param("interval", "d"))
            // The three daily points `/api/prices` returned for this contract on
            // 2026-08-23, verbatim.
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "statusCode": 200,
                "body": {
                    "data": "[{\"instrument_id\": \"HORC_1126_Republican\", \
                              \"yes_price\": 0.16, \"no_price\": 0.84, \
                              \"interval_date\": \"2026-08-21\", \"volume\": 5299}, \
                             {\"instrument_id\": \"HORC_1126_Republican\", \
                              \"yes_price\": 0.16, \"no_price\": 0.84, \
                              \"interval_date\": \"2026-08-22\", \"volume\": 0}, \
                             {\"instrument_id\": \"HORC_1126_Republican\", \
                              \"yes_price\": 0.19, \"no_price\": 0.81, \
                              \"interval_date\": \"2026-08-23\", \"volume\": 4750}]",
                    "page_size": 3,
                    "total_days": 502
                }
            })))
            .expect(1)
            .mount(&server)
            .await;

        let response = get_candles(
            &state_for(&server),
            "HORC_1126_REPUBLICAN",
            CandleInterval::OneDay,
            1_787_000_000,
            1_787_500_000,
        )
        .await
        .expect("the chart resolves");

        assert_eq!(response.ticker, "HORC_1126_REPUBLICAN");
        assert_eq!(response.series_ticker, "HORC");
        assert_eq!(response.candles.len(), 3);
        assert_eq!(
            response
                .candles
                .iter()
                .map(|c| (c.time, c.close, c.volume, c.traded))
                .collect::<Vec<_>>(),
            vec![
                // Each bar is a UTC day stamped at its end, so the point labelled
                // 2026-08-21 closes at midnight UTC on the 22nd.
                (1_787_356_800, 0.16, Some(5299.0), true),
                (1_787_443_200, 0.16, Some(0.0), false),
                // The newest bar ends past `end_ts` while its session is still
                // open, and the window is generous by one period rather than
                // dropping it.
                (1_787_529_600, 0.19, Some(4750.0), true),
            ]
        );
        // Samples rather than bars, and the response says so.
        assert!(response.note.unwrap().contains("price samples"));
    }

    #[tokio::test]
    async fn get_candles_takes_the_canonical_spelling_from_the_warm_catalogue_for_free() {
        // A chart request must not pay for a three-page crawl, but where the
        // crawl has already run the spelling is there for nothing — and no
        // second contract lookup goes out.
        let server = MockServer::start().await;
        mount_catalogue(&server).await;
        mount_empty_archive(&server).await;
        Mock::given(method("GET"))
            .and(path_matcher("/api/prices"))
            .and(query_param("contractId", "ULATL_082326_73"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "statusCode": 200,
                "body": { "data": "[]", "page_size": 0, "total_days": 0 }
            })))
            .mount(&server)
            .await;

        let state = state_for(&server);
        corpus_snapshot(&state).await.expect("a warm catalogue");

        let response = get_candles(
            &state,
            "ULATL_082326_73",
            CandleInterval::OneHour,
            0,
            1_787_500_000,
        )
        .await
        .expect("the chart resolves");
        assert!(response.candles.is_empty());
        assert_eq!(response.series_ticker, "ULATL");
    }

    #[tokio::test]
    async fn get_candles_refuses_minute_bars_because_api_prices_takes_only_h_and_d() {
        let server = MockServer::start().await;
        // No mock at all: the refusal happens before anything is asked for.
        let err = get_candles(
            &state_for(&server),
            "HORC_1126_REPUBLICAN",
            CandleInterval::OneMinute,
            0,
            0,
        )
        .await
        .expect_err("a minute grid is refused");
        assert_eq!(err.code, codes::UNSUPPORTED);
        assert!(err.hint.unwrap().contains("/api/prices"));
    }

    #[tokio::test]
    async fn get_order_book_refuses_by_naming_the_mechanism_not_by_an_empty_ladder() {
        // An empty book reads as a market with no resting interest. There is no
        // book here at all, and the hint has to say where the quotes actually
        // live.
        let server = MockServer::start().await;
        let err = get_order_book(&state_for(&server), "HORC_1126_REPUBLICAN", 12)
            .await
            .expect_err("there is no book to serve");
        assert_eq!(err.code, codes::UNSUPPORTED);
        let hint = err.hint.unwrap();
        assert!(hint.contains("/api/contracts"));
        assert!(hint.contains("portal.proxy/v1/ft"));
    }

    #[tokio::test]
    async fn get_trades_reads_the_whole_exchange_file_and_filters_to_one_contract() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher_prefix("/pairs/"))
            .respond_with(ResponseTemplate::new(200).set_body_string(PAIRS_CSV))
            .mount(&server)
            .await;

        let response = get_trades(&state_for(&server), "UHBKF_082326_91", 50)
            .await
            .expect("the tape resolves");
        assert_eq!(response.trades.len(), 3);
        assert_eq!(response.cursor, None);
        assert!(response.trades.iter().all(|t| t.taker_side.is_empty()));
    }

    #[tokio::test]
    async fn get_trades_falls_back_to_the_previous_session_when_this_one_is_quiet() {
        // Most contracts do not print every session, so an empty current file is
        // the norm rather than a fault — the panel is better served by the last
        // session that did trade.
        let server = MockServer::start().await;
        let today = session_date(OffsetDateTime::now_utc().unix_timestamp());
        let previous = previous_session(&today);

        Mock::given(method("GET"))
            .and(path_matcher(format!("/pairs/pairs_{today}.csv")))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                "pair_id,event_contract,expiration_date,quantity,yes_price,no_price,pair_time\n",
            ))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path_matcher(format!("/pairs/pairs_{previous}.csv")))
            .respond_with(ResponseTemplate::new(200).set_body_string(PAIRS_CSV))
            .mount(&server)
            .await;

        let response = get_trades(&state_for(&server), "HORC_1126_REPUBLICAN", 50)
            .await
            .expect("the tape resolves");
        assert_eq!(response.trades.len(), 2);
    }

    #[tokio::test]
    async fn get_trades_tells_an_unpublished_tape_apart_from_a_contract_nobody_traded() {
        // An empty tape means "nobody traded this"; a missing file means "the
        // exchange has not published the tape yet", and the two must not be
        // confused into a market that looks dead.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher_prefix("/pairs/"))
            .respond_with(ResponseTemplate::new(404).set_body_string("NoSuchKey"))
            .mount(&server)
            .await;

        let err = get_trades(&state_for(&server), "HORC_1126_REPUBLICAN", 50)
            .await
            .expect_err("no file is not an empty tape");
        assert_eq!(err.code, codes::NOT_FOUND);
        assert!(err.hint.unwrap().contains("16:15 CT roll"));
    }

    #[tokio::test]
    async fn get_trades_answers_empty_for_a_contract_that_simply_did_not_print() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher_prefix("/pairs/"))
            .respond_with(ResponseTemplate::new(200).set_body_string(PAIRS_CSV))
            .mount(&server)
            .await;

        let response = get_trades(&state_for(&server), "ACD_1226_432.0", 50)
            .await
            .expect("a quiet contract is not an error");
        assert!(response.trades.is_empty());
    }

    #[tokio::test]
    async fn the_archive_walks_back_to_the_newest_session_the_bucket_has_published() {
        // The current session's file 404s until after 16:30 CT, and a holiday
        // leaves a gap of several days.
        let server = MockServer::start().await;
        let today = session_date(OffsetDateTime::now_utc().unix_timestamp());
        let previous = previous_session(&today);

        Mock::given(method("GET"))
            .and(path_matcher(format!("/prices/daily_prices_{today}.csv")))
            .respond_with(ResponseTemplate::new(404).set_body_string("NoSuchKey"))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path_matcher(format!("/prices/daily_prices_{previous}.csv")))
            .respond_with(ResponseTemplate::new(200).set_body_string(ARCHIVE_CSV))
            .mount(&server)
            .await;

        let archive = session_archive(&state_for(&server)).await;
        let archive = archive.as_ref().as_ref().expect("a published session");
        // Which session answered is carried, because every figure taken from it
        // is as old as that.
        assert_eq!(archive.date, previous);
        assert_eq!(archive.rows.len(), 8);
    }

    #[tokio::test]
    async fn list_series_lists_the_products_the_registry_states() {
        let server = MockServer::start().await;
        mount_catalogue(&server).await;

        let series = list_series(&state_for(&server), None)
            .await
            .expect("a series list");
        assert_eq!(
            series.iter().map(|s| s.ticker.as_str()).collect::<Vec<_>>(),
            vec!["DISSN", "FF", "FFDEC", "HCAB", "HORC", "UHBKF", "ULATL", "ZFFCP"]
        );
    }

    #[tokio::test]
    async fn an_empty_category_is_no_filter_rather_than_one_nothing_matches() {
        // `GET /api/venue/forecastex/series?category=` reaches here as
        // `Some("")`. Read as a filter it answers with an empty catalogue, which
        // is a statement about the exchange rather than about the query.
        let server = MockServer::start().await;
        mount_catalogue(&server).await;
        let state = state_for(&server);

        let all = list_series(&state, None).await.expect("a series list");
        for blank in ["", "   "] {
            let asked = list_series(&state, Some(blank))
                .await
                .expect("a series list");
            assert_eq!(asked.len(), all.len(), "{blank:?}");
        }
    }

    #[tokio::test]
    async fn list_series_narrows_to_the_category_each_product_states_for_itself() {
        // Honest here in a way it is not at either Polymarket: every product row
        // carries its own category, so the filtered list is the real subset.
        let server = MockServer::start().await;
        mount_catalogue(&server).await;

        let series = list_series(&state_for(&server), Some("  Environmental "))
            .await
            .expect("a series list");
        assert_eq!(
            series.iter().map(|s| s.ticker.as_str()).collect::<Vec<_>>(),
            vec!["HCAB", "UHBKF", "ULATL"]
        );
    }

    #[tokio::test]
    async fn search_ranks_the_snapshot_rather_than_the_exchanges_own_parameter() {
        let server = MockServer::start().await;
        mount_catalogue(&server).await;
        mount_empty_archive(&server).await;

        let response = search(&state_for(&server), "fed decision", 25)
            .await
            .expect("a search");
        assert_eq!(response.query, "fed decision");
        assert!(response
            .hits
            .iter()
            .any(|hit| hit.event.event_ticker == "FFDEC_091626"));
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
    async fn top_markets_ranks_open_interest_on_the_live_catalogue_figure() {
        let server = MockServer::start().await;
        mount_catalogue(&server).await;
        mount_empty_archive(&server).await;

        let board = top_markets(&state_for(&server), MoverSort::OpenInterest, 5)
            .await
            .expect("a board");
        assert_eq!(board[0].ticker, "HORC_1126_REPUBLICAN");
        for pair in board.windows(2) {
            assert!(pair[0].open_interest >= pair[1].open_interest);
        }
    }

    #[tokio::test]
    async fn top_markets_leaves_the_turnover_board_empty_without_the_archive() {
        // Turnover is stated nowhere in the live catalogue. Without the archive
        // every contract reports `None`, and `rank_markets` drops them rather
        // than ranking a venue-wide silence as a row of zeroes.
        let server = MockServer::start().await;
        mount_catalogue(&server).await;
        mount_empty_archive(&server).await;

        let board = top_markets(&state_for(&server), MoverSort::Volume, 5)
            .await
            .expect("a board");
        assert!(board.is_empty());
    }

    #[tokio::test]
    async fn top_markets_ranks_turnover_on_the_archive_a_session_behind() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/api/contracts"))
            .respond_with(ResponseTemplate::new(200).set_body_json(contracts_body()))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path_matcher("/api/products"))
            .respond_with(ResponseTemplate::new(200).set_body_json(products_body()))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path_matcher_prefix("/prices/"))
            .respond_with(ResponseTemplate::new(200).set_body_string(ARCHIVE_CSV))
            .mount(&server)
            .await;

        let board = top_markets(&state_for(&server), MoverSort::Volume, 5)
            .await
            .expect("a board");
        assert_eq!(board[0].ticker, "HORC_1126_REPUBLICAN");
        assert_eq!(board[0].volume24h, Some(4750.0));
        // The two HORC legs and `UHBKF_082326_91` traded; the rest of the
        // archived rows report a genuine zero, which is still a stated figure.
        assert!(board.iter().all(|m| m.volume24h.is_some()));
    }

    #[tokio::test]
    async fn top_markets_leaves_the_liquidity_board_empty_because_there_is_no_book() {
        let server = MockServer::start().await;
        mount_catalogue(&server).await;
        mount_empty_archive(&server).await;

        let board = top_markets(&state_for(&server), MoverSort::Liquidity, 5)
            .await
            .expect("a board");
        assert!(board.is_empty());
    }
}
