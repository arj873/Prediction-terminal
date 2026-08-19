//! Reading a query string, the way every route needs it read.
//!
//! Nothing here is clever; it exists so that clamping, defaulting and the
//! "a repeated key arrives as a list" problem are solved once. The old server
//! hand-inlined these and got the repeated-key case wrong, so `?q=a&q=b`
//! reached the source layer as an empty string and silently lost the query
//! instead of erroring.

use std::collections::HashMap;

use terminal_core::types::CandleInterval;

use crate::error::{Result, UpstreamError};

/// A parsed query string.
///
/// Keeps the *first* value for a repeated key, which is what every route here
/// wants and what the old server intended. `axum`'s own `Query<HashMap<_, _>>`
/// keeps the last, so this is not just a convenience wrapper.
#[derive(Debug, Default, Clone)]
pub struct QueryParams(HashMap<String, String>);

impl QueryParams {
    /// Parse a raw query string (without the leading `?`).
    pub fn parse(raw: &str) -> Self {
        let mut map = HashMap::new();
        for (key, value) in form_urlencoded_pairs(raw) {
            map.entry(key).or_insert(value);
        }
        Self(map)
    }

    /// The raw value for `name`, if present.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.0.get(name).map(String::as_str)
    }

    /// A trimmed string, or `fallback` when the key is absent.
    pub fn str_param(&self, name: &str, fallback: &str) -> String {
        match self.0.get(name) {
            Some(value) => value.trim().to_string(),
            None => fallback.to_string(),
        }
    }

    /// A trimmed string, or the empty string.
    pub fn string(&self, name: &str) -> String {
        self.str_param(name, "")
    }

    /// A required, non-empty parameter.
    pub fn required(&self, name: &str) -> Result<String> {
        let value = self.string(name);
        if value.is_empty() {
            return Err(UpstreamError::bad_request(format!("`{name}` is required")));
        }
        Ok(value)
    }

    /// An integer, truncated toward zero and clamped into `[min, max]`.
    ///
    /// A value that is absent, empty or unparseable falls back rather than
    /// failing: a panel sending `limit=` on a fresh load should get the default
    /// page, not a 400.
    pub fn int_param(&self, name: &str, fallback: i64, min: i64, max: i64) -> i64 {
        let Some(raw) = self.0.get(name).map(|v| v.trim()) else {
            return fallback;
        };
        if raw.is_empty() {
            return fallback;
        }
        // Parsed as f64 first so `limit=30.7` truncates the way `Number()` +
        // `Math.trunc` did, rather than failing an integer parse.
        match raw.parse::<f64>() {
            Ok(value) if value.is_finite() => (value.trunc() as i64).clamp(min, max),
            _ => fallback,
        }
    }

    /// A unix-seconds timestamp, if the key holds one.
    pub fn timestamp(&self, name: &str) -> Option<i64> {
        let raw = self.0.get(name)?.trim();
        if raw.is_empty() {
            return None;
        }
        raw.parse::<f64>()
            .ok()
            .filter(|v| v.is_finite())
            .map(|v| v.trunc() as i64)
    }

    /// A candle interval, defaulting to one hour.
    ///
    /// Rejects anything outside the three buckets rather than rounding to the
    /// nearest, because a chart drawn on a grid the venue does not serve is
    /// worse than an error that says which grids exist.
    pub fn interval(&self) -> Result<CandleInterval> {
        let Some(raw) = self.0.get("interval").map(|v| v.trim()) else {
            return Ok(CandleInterval::OneHour);
        };
        if raw.is_empty() {
            return Ok(CandleInterval::OneHour);
        }

        raw.parse::<u16>()
            .ok()
            .and_then(|minutes| CandleInterval::try_from(minutes).ok())
            .ok_or_else(|| {
                UpstreamError::bad_request(format!("Unsupported interval `{raw}`"))
                    .with_hint(INTERVAL_HINT)
            })
    }
}

/// Split a query string into decoded key/value pairs, in order.
fn form_urlencoded_pairs(raw: &str) -> impl Iterator<Item = (String, String)> + '_ {
    raw.split('&').filter(|part| !part.is_empty()).map(|part| {
        let (key, value) = match part.split_once('=') {
            Some((k, v)) => (k, v),
            None => (part, ""),
        };
        (decode(key), decode(value))
    })
}

/// Percent-decoding, with `+` meaning a space as in a form body.
fn decode(raw: &str) -> String {
    let replaced = raw.replace('+', " ");
    urlencoding::decode(&replaced)
        .map(|decoded| decoded.into_owned())
        .unwrap_or(replaced)
}

/// Why those three buckets and no others.
pub const INTERVAL_HINT: &str =
    "Intervals are 1 (1m), 60 (1h) and 1440 (1d) minutes — the same buckets \
     Kalshi uses, so an implied overlay lines up with the price.";

/// How far back a candle request reaches when it does not say.
///
/// Scaled to the bucket: a minute chart with a year of default window would ask
/// for half a million bars.
pub fn default_lookback(interval: CandleInterval) -> i64 {
    match interval {
        CandleInterval::OneMinute => 6 * 3_600,
        CandleInterval::OneHour => 30 * 86_400,
        CandleInterval::OneDay => 365 * 86_400,
    }
}

/// Resolve a `[start, end]` candle window from the query.
///
/// `end` is clamped to a day ahead of now — a clock-skewed client asking for
/// tomorrow should get today's bars rather than an empty chart — and a window
/// that ends before it starts is the caller's mistake, so it is a 400.
pub fn candle_window(
    params: &QueryParams,
    interval: CandleInterval,
    now: i64,
) -> Result<(i64, i64)> {
    let end = params.timestamp("end").unwrap_or(now).min(now + 86_400);
    let start = params
        .timestamp("start")
        .unwrap_or_else(|| end - default_lookback(interval));

    if start >= end {
        return Err(UpstreamError::bad_request("`start` must be before `end`"));
    }

    Ok((start, end))
}

/// Snap a timestamp down onto a 15-second grid.
///
/// Candle and implied-series responses are cached, and panels poll them on
/// their own timers. Without this the `now` in every request differs, so the
/// cache key differs, so nothing ever hits and the TTL might as well not exist.
pub fn snap_to_grid(seconds: i64) -> i64 {
    const GRID: i64 = 15;
    seconds - seconds.rem_euclid(GRID)
}

/// Now, in unix seconds.
pub fn now_seconds() -> i64 {
    time::OffsetDateTime::now_utc().unix_timestamp()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(raw: &str) -> QueryParams {
        QueryParams::parse(raw)
    }

    #[test]
    fn keeps_the_first_value_of_a_repeated_key() {
        // `?q=a&q=b` must not silently lose the query, which is what the
        // hand-inlined readers this replaces used to do.
        let p = params("q=alpha&q=beta");
        assert_eq!(p.string("q"), "alpha");
    }

    #[test]
    fn decodes_percent_escapes_and_plus_as_space() {
        let p = params("q=fed%20decision&r=world+series&s=%26%3D");
        assert_eq!(p.string("q"), "fed decision");
        assert_eq!(p.string("r"), "world series");
        assert_eq!(p.string("s"), "&=");
    }

    #[test]
    fn trims_and_defaults_a_string() {
        let p = params("q=%20%20spaced%20%20&empty=");
        assert_eq!(p.string("q"), "spaced");
        assert_eq!(p.string("empty"), "");
        assert_eq!(p.str_param("absent", "fallback"), "fallback");
    }

    #[test]
    fn a_required_parameter_that_is_missing_or_blank_is_a_bad_request() {
        let p = params("q=%20");
        let err = p.required("q").unwrap_err();
        assert_eq!(err.code, crate::error::codes::BAD_REQUEST);
        assert!(err.message.contains("`q`"));
        assert!(params("").required("q").is_err());
        assert_eq!(params("q=btc").required("q").unwrap(), "btc");
    }

    #[test]
    fn clamps_an_integer_into_range() {
        let p = params("a=5&b=-99&c=99999&d=30.7&e=abc&f=");
        assert_eq!(p.int_param("a", 25, 1, 100), 5);
        assert_eq!(p.int_param("b", 25, 1, 100), 1);
        assert_eq!(p.int_param("c", 25, 1, 100), 100);
        // Truncated toward zero, as `Math.trunc` did.
        assert_eq!(p.int_param("d", 25, 1, 100), 30);
        // Unparseable and empty both fall back rather than failing.
        assert_eq!(p.int_param("e", 25, 1, 100), 25);
        assert_eq!(p.int_param("f", 25, 1, 100), 25);
        assert_eq!(p.int_param("absent", 25, 1, 100), 25);
    }

    #[test]
    fn reads_only_the_three_candle_buckets() {
        assert_eq!(params("").interval().unwrap(), CandleInterval::OneHour);
        assert_eq!(
            params("interval=1").interval().unwrap(),
            CandleInterval::OneMinute
        );
        assert_eq!(
            params("interval=1440").interval().unwrap(),
            CandleInterval::OneDay
        );

        let err = params("interval=5").interval().unwrap_err();
        assert_eq!(err.code, crate::error::codes::BAD_REQUEST);
        // The hint names the grids that do exist.
        assert!(err.hint.unwrap().contains("1440"));
    }

    #[test]
    fn the_default_window_is_scaled_to_the_bucket() {
        // A minute chart with a year of default window would ask for half a
        // million bars.
        assert_eq!(default_lookback(CandleInterval::OneMinute), 6 * 3_600);
        assert_eq!(default_lookback(CandleInterval::OneHour), 30 * 86_400);
        assert_eq!(default_lookback(CandleInterval::OneDay), 365 * 86_400);
    }

    #[test]
    fn a_window_with_no_bounds_reaches_back_from_now() {
        let now = 1_700_000_000;
        let (start, end) = candle_window(&params(""), CandleInterval::OneHour, now).unwrap();
        assert_eq!(end, now);
        assert_eq!(start, now - 30 * 86_400);
    }

    #[test]
    fn clamps_an_end_in_the_future_to_a_day_ahead() {
        let now = 1_700_000_000;
        let far = now + 400 * 86_400;
        let (_, end) =
            candle_window(&params(&format!("end={far}")), CandleInterval::OneHour, now).unwrap();
        assert_eq!(end, now + 86_400);
    }

    #[test]
    fn a_window_that_ends_before_it_starts_is_the_callers_mistake() {
        let now = 1_700_000_000;
        let err = candle_window(
            &params("start=1700000000&end=1600000000"),
            CandleInterval::OneHour,
            now,
        )
        .unwrap_err();
        assert_eq!(err.code, crate::error::codes::BAD_REQUEST);

        // Equal bounds are empty, and equally a mistake.
        assert!(candle_window(
            &params("start=1700000000&end=1700000000"),
            CandleInterval::OneHour,
            now,
        )
        .is_err());
    }

    #[test]
    fn snapping_makes_a_polling_panel_share_one_cache_key() {
        // Every instant inside one 15-second cell must produce the same key, or
        // the cache never hits and the TTL might as well not exist.
        let cell = snap_to_grid(1_700_000_000);
        for offset in 0..15 {
            assert_eq!(snap_to_grid(cell + offset), cell, "offset {offset}");
        }
        // The next second starts a new cell.
        assert_eq!(snap_to_grid(cell + 15), cell + 15);
    }

    #[test]
    fn a_snapped_timestamp_is_always_on_the_grid_and_never_ahead_of_the_input() {
        for raw in [0, 1, 14, 15, 1_700_000_007, 1_699_999_995, i64::MAX - 100] {
            let snapped = snap_to_grid(raw);
            assert_eq!(snapped % 15, 0, "{raw} did not land on the grid");
            assert!(snapped <= raw, "{raw} snapped forward to {snapped}");
            assert!(raw - snapped < 15, "{raw} snapped back too far");
        }
    }

    #[test]
    fn snapping_a_pre_epoch_timestamp_still_rounds_down() {
        // `%` in Rust keeps the sign; `rem_euclid` is what makes this floor
        // rather than truncate toward zero.
        assert_eq!(snap_to_grid(-7), -15);
        assert_eq!(snap_to_grid(-15), -15);
    }

    #[test]
    fn a_bare_key_reads_as_empty_rather_than_being_dropped() {
        let p = params("nested&q=x");
        assert_eq!(p.get("nested"), Some(""));
        assert_eq!(p.string("q"), "x");
    }
}
