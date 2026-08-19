//! TV schedules — from the TVmaze public API.
//!
//! Kalshi's TV book is episodic and date-driven: `KXBIGBROTHER` and
//! `KXBIGBROTHERELIMINATION` (the second-busiest entertainment series by volume)
//! resolve week by week as episodes air, and `KXDWTS`, `KXSNL` and
//! `KXTVSHOWSCANCELLED` all turn on what is actually broadcast and when.
//!
//! TVmaze is the rare entertainment source that is a real API: JSON, no key, no
//! bot protection, documented rate limits. So this module is a normaliser rather
//! than a parser — the only work is flattening TVmaze's nested show/network
//! objects into flat rows and being careful about the fields it leaves null.

use std::cmp::Ordering;
use std::sync::LazyLock;
use std::time::Duration;

use regex::Regex;
use serde::Deserialize;
use serde_json::Value;
use terminal_core::types::{TvEpisode, TvSchedule};
use time::format_description::BorrowedFormatItem;
use time::{Date, OffsetDateTime};

use crate::app::AppState;
use crate::cache::ttl;
use crate::error::{codes, Result, UpstreamError};
use crate::http::FetchOptions;

static DATE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\d{4}-\d{2}-\d{2}$").expect("DATE is a valid regex"));

static COUNTRY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z]{2}$").expect("COUNTRY is a valid regex"));

/// The shape the regex allows, so `Date::parse` can rule on whether it is a day
/// that exists — the `Number.isNaN(Date.parse(date))` half of the check.
const ISO_DATE: &[BorrowedFormatItem<'_>] =
    time::macros::format_description!("[year]-[month]-[day]");

pub fn assert_date(raw: &str) -> Result<String> {
    let date = raw.trim();
    if !DATE.is_match(date) || Date::parse(date, ISO_DATE).is_err() {
        return Err(
            UpstreamError::bad_request(format!("\"{raw}\" is not a valid schedule date"))
                .with_hint("Dates are YYYY-MM-DD, e.g. `TV 2026-08-20`."),
        );
    }
    Ok(date.to_string())
}

pub fn assert_country(raw: &str) -> Result<String> {
    let country = raw.trim().to_uppercase();
    if !COUNTRY.is_match(&country) {
        return Err(
            UpstreamError::bad_request(format!("\"{raw}\" is not a country code"))
                .with_hint("Use a two-letter code, e.g. `TV GB` or `TV 2026-08-20 US`."),
        );
    }
    Ok(country)
}

/// One entry of TVmaze's `/schedule` array.
///
/// Every field is optional and the numbers arrive as raw [`Value`]s, because
/// TVmaze leaves `season`, `number` and `runtime` null on plenty of rows and
/// occasionally omits them outright; a strict struct would turn that into a 500.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawEpisode {
    #[serde(default)]
    pub airtime: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub season: Value,
    #[serde(default)]
    pub number: Value,
    #[serde(default)]
    pub runtime: Value,
    #[serde(default, rename = "type")]
    pub kind: Option<String>,
    #[serde(default)]
    pub show: Option<RawShow>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawShow {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub network: Option<RawChannel>,
    #[serde(default)]
    pub web_channel: Option<RawChannel>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct RawChannel {
    #[serde(default)]
    pub name: Option<String>,
}

/// `typeof value === 'number' && Number.isFinite(value)`, narrowed to the
/// unsigned counts the wire type carries — a season, an episode number and a
/// runtime in minutes are never negative, and a value that is not a JSON number
/// is not a figure TVmaze published.
fn int_or_null(value: &Value) -> Option<u32> {
    let number = value.as_f64()?;
    if !number.is_finite() || number < 0.0 || number > f64::from(u32::MAX) {
        return None;
    }
    Some(number as u32)
}

pub fn normalise_schedule(
    raw: &[RawEpisode],
    date: &str,
    country: &str,
    source_url: &str,
) -> TvSchedule {
    let mut episodes: Vec<TvEpisode> = raw
        .iter()
        .map(|entry| {
            let show = entry.show.as_ref();
            TvEpisode {
                airtime: entry.airtime.clone().unwrap_or_default(),
                show: show.and_then(|s| s.name.clone()).unwrap_or_default(),
                // Broadcast shows carry a `network`; streaming ones carry a
                // `webChannel` instead, and a row with neither is not worth a
                // blank.
                network: show
                    .and_then(|s| s.network.as_ref())
                    .and_then(|n| n.name.clone())
                    .or_else(|| {
                        show.and_then(|s| s.web_channel.as_ref())
                            .and_then(|c| c.name.clone())
                    })
                    .unwrap_or_default(),
                season: int_or_null(&entry.season),
                episode: int_or_null(&entry.number),
                name: entry.name.clone().unwrap_or_default(),
                runtime: int_or_null(&entry.runtime),
                kind: entry.kind.clone().unwrap_or_default(),
            }
        })
        .filter(|episode| !episode.show.is_empty())
        .collect();

    // TVmaze returns the day roughly in airtime order, but not reliably, and an
    // untimed entry should sort to the end rather than to midnight. The show
    // name breaks a tie, here by code point where the original used the
    // runtime's collation — it only orders two programmes airing in the same
    // minute.
    episodes.sort_by(|a, b| {
        if a.airtime == b.airtime {
            return a.show.cmp(&b.show);
        }
        if a.airtime.is_empty() {
            return Ordering::Greater;
        }
        if b.airtime.is_empty() {
            return Ordering::Less;
        }
        a.airtime.cmp(&b.airtime)
    });

    TvSchedule {
        date: date.to_string(),
        country: country.to_string(),
        episodes,
        source_url: source_url.to_string(),
    }
}

/// `new Date().toISOString().slice(0, 10)` — today in UTC, which is the day
/// TVmaze means when it is not told one.
fn today_iso() -> String {
    let today = OffsetDateTime::now_utc().date();
    format!(
        "{:04}-{:02}-{:02}",
        today.year(),
        u8::from(today.month()),
        today.day()
    )
}

pub async fn get_schedule(
    state: &AppState,
    raw_date: Option<&str>,
    raw_country: Option<&str>,
) -> Result<TvSchedule> {
    let date = match raw_date.filter(|value| !value.is_empty()) {
        Some(value) => assert_date(value)?,
        None => today_iso(),
    };
    let country = match raw_country.filter(|value| !value.is_empty()) {
        Some(value) => assert_country(value)?,
        None => "US".to_string(),
    };
    let source_url = format!(
        "{}/schedule?country={country}&date={date}",
        state.config().tvmaze_api_base
    );

    let schedule = state
        .cache()
        .cached(
            &format!("tvmaze:{country}:{date}"),
            ttl::TV_SCHEDULE,
            || async {
                let raw: Value = state
                    .http()
                    .fetch_json(
                        &source_url,
                        FetchOptions::new()
                            .timeout(Duration::from_secs(25))
                            .retries(2),
                    )
                    .await?;
                let Some(items) = raw.as_array() else {
                    return Err(UpstreamError::new(
                        "TVmaze returned an unexpected schedule payload",
                        codes::BAD_UPSTREAM_BODY,
                    ));
                };
                // Per entry rather than for the array as a whole: one row TVmaze
                // reshapes should cost that row, not the day's schedule.
                let episodes: Vec<RawEpisode> = items
                    .iter()
                    .map(|item| RawEpisode::deserialize(item).unwrap_or_default())
                    .collect();
                Ok(normalise_schedule(&episodes, &date, &country, &source_url))
            },
        )
        .await?;

    Ok((*schedule).clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::http::Http;
    use serde_json::json;
    use wiremock::matchers::{method, path as path_matcher, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// A real `api.tvmaze.com/schedule` answer, trimmed to four rows: a
    /// broadcast episode, a streaming one with a null network, an untimed-ish
    /// early show with no season or episode number, and a row with no show at
    /// all.
    const SCHEDULE_JSON: &str = include_str!("fixtures/tvmaze_schedule.json");

    fn episodes_of(value: &Value) -> Vec<RawEpisode> {
        value
            .as_array()
            .expect("an array")
            .iter()
            .map(|item| RawEpisode::deserialize(item).expect("an episode"))
            .collect()
    }

    /// The three entries the TypeScript test drove `normaliseSchedule` with.
    fn raw() -> Vec<RawEpisode> {
        episodes_of(&json!([
            {
                "airtime": "20:00",
                "name": "Week 6",
                "season": 28,
                "number": 12,
                "runtime": 60,
                "type": "regular",
                "show": { "name": "Big Brother", "network": { "name": "CBS" } }
            },
            {
                "airtime": "",
                "name": "Episode 1",
                "season": 1,
                "number": 1,
                "runtime": null,
                "type": "regular",
                "show": { "name": "Streaming Only", "network": null, "webChannel": { "name": "Netflix" } }
            },
            {
                "airtime": "06:00",
                "name": "Early",
                "season": null,
                "number": null,
                "runtime": 30,
                "type": "regular",
                "show": { "name": "Morning Show", "network": { "name": "NBC" } }
            }
        ]))
    }

    fn schedule() -> TvSchedule {
        normalise_schedule(&raw(), "2026-08-20", "US", "u")
    }

    fn find<'a>(schedule: &'a TvSchedule, show: &str) -> &'a TvEpisode {
        schedule
            .episodes
            .iter()
            .find(|episode| episode.show == show)
            .expect("the show is in the schedule")
    }

    fn state_for(server: &MockServer) -> AppState {
        AppState::with_http(
            Config {
                tvmaze_api_base: server.uri().trim_end_matches('/').to_string(),
                ..Config::default()
            },
            Http::new(),
        )
    }

    /* --------------------------------------------------- normaliseSchedule */

    #[test]
    fn flattens_the_nested_show_and_network_objects() {
        let schedule = schedule();
        let big_brother = find(&schedule, "Big Brother");
        assert_eq!(big_brother.network, "CBS");
        assert_eq!(big_brother.season, Some(28));
        assert_eq!(big_brother.episode, Some(12));
    }

    #[test]
    fn falls_back_to_the_web_channel_for_streaming_only_shows() {
        assert_eq!(find(&schedule(), "Streaming Only").network, "Netflix");
    }

    #[test]
    fn sorts_by_airtime_and_pushes_untimed_entries_to_the_end() {
        let schedule = schedule();
        let order: Vec<&str> = schedule
            .episodes
            .iter()
            .map(|episode| episode.show.as_str())
            .collect();
        assert_eq!(order, ["Morning Show", "Big Brother", "Streaming Only"]);
    }

    #[test]
    fn keeps_a_missing_season_or_episode_as_null_rather_than_0() {
        let schedule = schedule();
        let morning = find(&schedule, "Morning Show");
        assert_eq!(morning.season, None);
        assert_eq!(morning.episode, None);
    }

    #[test]
    fn drops_entries_with_no_show_name() {
        let raw = episodes_of(&json!([{ "airtime": "10:00" }]));
        assert!(normalise_schedule(&raw, "2026-08-20", "US", "u")
            .episodes
            .is_empty());
    }

    #[test]
    fn carries_the_date_country_and_source_url_through() {
        let schedule = normalise_schedule(&raw(), "2026-08-20", "GB", "https://api.tvmaze.com/x");
        assert_eq!(schedule.date, "2026-08-20");
        assert_eq!(schedule.country, "GB");
        assert_eq!(schedule.source_url, "https://api.tvmaze.com/x");
    }

    #[test]
    fn keeps_a_null_runtime_null_and_reads_the_episode_type() {
        let schedule = schedule();
        assert_eq!(find(&schedule, "Streaming Only").runtime, None);
        assert_eq!(find(&schedule, "Big Brother").runtime, Some(60));
        assert_eq!(find(&schedule, "Big Brother").kind, "regular");
    }

    #[test]
    fn ignores_a_figure_that_is_not_a_json_number() {
        // TVmaze has been seen to send `"season": "28"` on a re-run row. The
        // TypeScript probed `typeof value === 'number'`, so a string is not a
        // season — reading `28` out of it would be inventing one.
        let raw = episodes_of(&json!([{
            "airtime": "20:00",
            "season": "28",
            "number": -3,
            "show": { "name": "Odd Row" }
        }]));
        let schedule = normalise_schedule(&raw, "2026-08-20", "US", "u");
        assert_eq!(schedule.episodes[0].season, None);
        assert_eq!(schedule.episodes[0].episode, None);
    }

    /* ------------------------------------------------------- the arguments */

    #[test]
    fn accepts_a_date_that_exists_and_trims_it() {
        assert_eq!(assert_date(" 2026-08-20 ").unwrap(), "2026-08-20");
    }

    #[test]
    fn rejects_a_date_that_is_the_wrong_shape_or_no_day_at_all() {
        // `2026-02-30` matches the shape and is not a day, which is exactly the
        // pair `Date.parse` was there to catch.
        for raw in ["20 Aug 2026", "2026-8-20", "2026-02-30", "2026-13-01", ""] {
            let error = assert_date(raw).expect_err(raw);
            assert_eq!(error.code, codes::BAD_REQUEST, "{raw}");
            assert!(
                error.message.contains("is not a valid schedule date"),
                "{raw}"
            );
            assert!(error.hint.is_some(), "{raw}");
        }
    }

    #[test]
    fn upper_cases_a_country_code() {
        assert_eq!(assert_country(" gb ").unwrap(), "GB");
    }

    #[test]
    fn rejects_anything_that_is_not_two_letters() {
        for raw in ["USA", "u", "12", ""] {
            let error = assert_country(raw).expect_err(raw);
            assert_eq!(error.code, codes::BAD_REQUEST, "{raw}");
            assert!(error.message.contains("is not a country code"), "{raw}");
        }
    }

    /* ------------------------------------------------------------- fetching */

    fn schedule_response() -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_raw(SCHEDULE_JSON, "application/json")
    }

    #[tokio::test]
    async fn asks_tvmaze_for_the_country_and_date_it_was_given() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/schedule"))
            .and(query_param("country", "GB"))
            .and(query_param("date", "2026-08-20"))
            .respond_with(schedule_response())
            .expect(1)
            .mount(&server)
            .await;

        let schedule = get_schedule(&state_for(&server), Some(" 2026-08-20 "), Some("gb"))
            .await
            .unwrap();

        assert_eq!(schedule.date, "2026-08-20");
        assert_eq!(schedule.country, "GB");
        assert!(schedule
            .source_url
            .ends_with("/schedule?country=GB&date=2026-08-20"));
    }

    #[tokio::test]
    async fn normalises_the_live_payload_and_drops_the_show_less_row() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/schedule"))
            .respond_with(schedule_response())
            .mount(&server)
            .await;

        let schedule = get_schedule(&state_for(&server), Some("2026-08-20"), Some("US"))
            .await
            .unwrap();

        let order: Vec<&str> = schedule
            .episodes
            .iter()
            .map(|episode| episode.show.as_str())
            .collect();
        assert_eq!(order, ["Morning Show", "Big Brother", "Streaming Only"]);
        assert_eq!(find(&schedule, "Streaming Only").network, "Netflix");
    }

    #[tokio::test]
    async fn defaults_to_the_us_and_today() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/schedule"))
            .and(query_param("country", "US"))
            .and(query_param("date", today_iso()))
            .respond_with(schedule_response())
            .expect(1)
            .mount(&server)
            .await;

        let schedule = get_schedule(&state_for(&server), None, None).await.unwrap();
        assert_eq!(schedule.country, "US");
        assert_eq!(schedule.date, today_iso());
    }

    #[tokio::test]
    async fn treats_an_empty_argument_as_an_absent_one() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/schedule"))
            .and(query_param("country", "US"))
            .respond_with(schedule_response())
            .mount(&server)
            .await;

        let schedule = get_schedule(&state_for(&server), Some(""), Some(""))
            .await
            .unwrap();
        assert_eq!(schedule.country, "US");
        assert_eq!(schedule.date, today_iso());
    }

    #[tokio::test]
    async fn refuses_a_bad_date_before_asking_tvmaze() {
        // No mock is mounted: a request would 404, so reaching the network at
        // all fails this differently than the validation error it must be.
        let server = MockServer::start().await;
        let error = get_schedule(&state_for(&server), Some("yesterday"), None)
            .await
            .expect_err("a bad request");
        assert_eq!(error.code, codes::BAD_REQUEST);
    }

    #[tokio::test]
    async fn reports_a_payload_that_is_not_an_array() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/schedule"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "message": "nope" })))
            .mount(&server)
            .await;

        let error = get_schedule(&state_for(&server), Some("2026-08-20"), Some("US"))
            .await
            .expect_err("an unexpected payload");
        assert_eq!(error.code, codes::BAD_UPSTREAM_BODY);
        assert_eq!(
            error.message,
            "TVmaze returned an unexpected schedule payload"
        );
    }

    #[tokio::test]
    async fn reads_the_schedule_from_the_cache_key_the_typescript_used() {
        // No mock at all: if the key were spelled differently this would go to
        // the network and 404.
        let server = MockServer::start().await;
        let state = state_for(&server);
        state
            .cache()
            .cached("tvmaze:US:2026-08-20", ttl::TV_SCHEDULE, || async {
                Ok(normalise_schedule(&raw(), "2026-08-20", "US", "cached"))
            })
            .await
            .unwrap();

        let schedule = get_schedule(&state, Some("2026-08-20"), Some("us"))
            .await
            .unwrap();
        assert_eq!(schedule.source_url, "cached");
    }
}
