//! Spotify and YouTube charts — read from kworb.net.
//!
//! Kalshi runs a deep bench of streaming markets: `KXTOPARTIST`, `KXTOPSONGSPOTIFY`
//! and `KXARTISTSTREAMSY` (61 open events, ~1,350 contracts) settle on Spotify
//! figures, and `KXYTVIEWSW`, `KXYTTOPSONGW` and `KXYTDAILYTOPVIDEO` settle on
//! YouTube view counts.
//!
//! Neither platform publishes those numbers in a form a server can read:
//! charts.spotify.com moved behind a login, and charts.youtube.com renders
//! client-side. kworb.net has mirrored both daily for years and is the reference
//! the trading community actually quotes, so it is what this reads — with the
//! honest caveat that it is a third-party mirror, surfaced in the panel.
//!
//! Both sites share one table idiom: a header row naming the columns, a rank, a
//! `P+` movement cell (`=`, `+3`, `-1`, `NEW`), and an `Artist - Title` string.
//! Columns differ between charts — the daily Spotify table has `Days` and `7Day`
//! where the weekly has `Wks` and neither — so the parser resolves every column
//! by its *header text* rather than by position.

use std::sync::LazyLock;
use std::time::Duration;

use regex::Regex;
use terminal_core::types::{StreamChart, StreamChartListItem, StreamEntry};

use crate::app::AppState;
use crate::cache::ttl;
use crate::css;
use crate::error::{Result, UpstreamError};
use crate::http::FetchOptions;
use crate::scrape::{self, Table};

/// One chart kworb publishes: how it is addressed here, and where it lives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChartSpec {
    pub slug: String,
    pub name: String,
    /// `spotify` or `youtube`.
    pub source: String,
    /// Path under the kworb base, leading slash included.
    pub path: String,
}

impl ChartSpec {
    fn new(slug: &str, name: &str, source: &str, path: &str) -> Self {
        Self {
            slug: slug.to_owned(),
            name: name.to_owned(),
            source: source.to_owned(),
            path: path.to_owned(),
        }
    }
}

/// The charts worth a keystroke. `SPOT CHARTS` / `YT CHARTS` list these.
pub static KNOWN_CHARTS: LazyLock<Vec<ChartSpec>> = LazyLock::new(|| {
    vec![
        // Spotify — `<country>_<daily|weekly>`; kworb carries ~70 countries.
        ChartSpec::new(
            "us-daily",
            "Spotify Daily — United States",
            "spotify",
            "/spotify/country/us_daily.html",
        ),
        ChartSpec::new(
            "us-weekly",
            "Spotify Weekly — United States",
            "spotify",
            "/spotify/country/us_weekly.html",
        ),
        ChartSpec::new(
            "global-daily",
            "Spotify Daily — Global",
            "spotify",
            "/spotify/country/global_daily.html",
        ),
        ChartSpec::new(
            "global-weekly",
            "Spotify Weekly — Global",
            "spotify",
            "/spotify/country/global_weekly.html",
        ),
        // YouTube.
        ChartSpec::new(
            "today",
            "YouTube — Today's Most Viewed Music Videos",
            "youtube",
            "/youtube/",
        ),
        ChartSpec::new(
            "alltime",
            "YouTube — Most Viewed of All Time",
            "youtube",
            "/youtube/topvideos.html",
        ),
        ChartSpec::new(
            "trending",
            "YouTube — Trending Worldwide",
            "youtube",
            "/youtube/trending.html",
        ),
    ]
});

#[must_use]
pub fn list_charts(source: Option<&str>) -> Vec<StreamChartListItem> {
    KNOWN_CHARTS
        .iter()
        .filter(|chart| source.is_none_or(|source| chart.source == source))
        .map(|chart| StreamChartListItem {
            slug: chart.slug.clone(),
            name: chart.name.clone(),
            source: chart.source.clone(),
        })
        .collect()
}

/// `us`, `gb`, `global` → the Spotify chart slug for that country.
static COUNTRY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-z]{2}$").expect("COUNTRY is a valid regex"));

/// Resolve a user-typed chart argument.
///
/// `SPOT` → us-daily. `SPOT global` → global-daily. `SPOT gb weekly` →
/// gb-weekly. Anything already in the catalogue is used as-is.
pub fn resolve_spotify_chart(scope: &str, period: &str) -> Result<ChartSpec> {
    let country = scope.trim().to_lowercase();
    let country = if country.is_empty() {
        "us".to_owned()
    } else {
        country
    };
    let cadence = if period.trim().to_lowercase() == "weekly" {
        "weekly"
    } else {
        "daily"
    };

    let slug = format!("{country}-{cadence}");
    if let Some(known) = KNOWN_CHARTS
        .iter()
        .find(|chart| chart.slug == slug && chart.source == "spotify")
    {
        return Ok(known.clone());
    }

    if country != "global" && !COUNTRY.is_match(&country) {
        return Err(
            UpstreamError::bad_request(format!("\"{scope}\" is not a country code"))
                .with_hint("Use a two-letter code or `global`, e.g. `SPOT gb weekly`."),
        );
    }

    Ok(ChartSpec {
        slug,
        name: format!(
            "Spotify {} — {}",
            if cadence == "weekly" {
                "Weekly"
            } else {
                "Daily"
            },
            country.to_uppercase()
        ),
        source: "spotify".to_owned(),
        path: format!("/spotify/country/{country}_{cadence}.html"),
    })
}

pub fn resolve_youtube_chart(view: &str) -> Result<ChartSpec> {
    let slug = view.trim().to_lowercase();
    let slug = if slug.is_empty() {
        "today".to_owned()
    } else {
        slug
    };
    KNOWN_CHARTS
        .iter()
        .find(|chart| chart.slug == slug && chart.source == "youtube")
        .cloned()
        .ok_or_else(|| {
            UpstreamError::bad_request(format!("\"{view}\" is not a YouTube chart"))
                .with_hint("Try `YT`, `YT alltime` or `YT trending`.")
        })
}

/* ------------------------------------------------------------------ parsing */

/// `Number.parseInt(text, 10)`: an optional sign, then every digit up to the
/// first character that is not one. `None` where JavaScript gives `NaN`.
fn js_parse_int(text: &str) -> Option<i64> {
    let bytes = text.as_bytes();
    let mut end = usize::from(matches!(bytes.first(), Some(b'+' | b'-')));
    let digits = end;
    while end < bytes.len() && bytes[end].is_ascii_digit() {
        end += 1;
    }
    (end > digits).then(|| text[..end].parse::<i64>().ok())?
}

/// `"1,466,839"` → 1466839; `"-"`, `""` → null.
fn int_or_null(text: Option<&str>) -> Option<i64> {
    let text = text?;
    if text.is_empty() {
        return None;
    }
    let cleaned: String = text
        .chars()
        .filter(|ch| !(*ch == ',' || *ch == '+' || ch.is_whitespace()))
        .collect();
    if cleaned.is_empty() || cleaned == "-" || cleaned == "--" {
        return None;
    }
    js_parse_int(&cleaned)
}

/// Signed column such as `Streams+`: `"+84,270"` → 84270, `"-116,663"` → -116663.
fn signed_or_null(text: Option<&str>) -> Option<i64> {
    let text = text?;
    if text.is_empty() {
        return None;
    }
    let cleaned: String = text
        .chars()
        .filter(|ch| !(*ch == ',' || ch.is_whitespace()))
        .collect();
    if cleaned.is_empty() || cleaned == "-" || cleaned == "--" {
        return None;
    }
    js_parse_int(&cleaned)
}

/// A figure on the wire is a JavaScript number.
fn as_number(value: Option<i64>) -> Option<f64> {
    value.map(|n| n as f64)
}

/// A count — a rank, a peak, a day tally. Anything that will not fit is no
/// count at all rather than a wrapped one.
fn as_count(value: Option<i64>) -> Option<u32> {
    value.and_then(|n| u32::try_from(n).ok())
}

/// A row's own one-based position, for a table that states no rank of its own.
fn position(rows_so_far: usize) -> u32 {
    u32::try_from(rows_so_far + 1).unwrap_or(u32::MAX)
}

/// What kworb's movement cell says: how far the entry moved, and whether it is
/// on the chart for the first time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Movement {
    pub movement: Option<i32>,
    pub is_new: bool,
}

impl Movement {
    const fn moved(movement: i32) -> Self {
        Self {
            movement: Some(movement),
            is_new: false,
        }
    }

    const UNKNOWN: Self = Self {
        movement: None,
        is_new: false,
    };

    const DEBUT: Self = Self {
        movement: None,
        is_new: true,
    };
}

/// kworb's movement cell.
///
/// `=` held, `+3` climbed three, `-1` slipped one, `NEW` is a debut. Returned in
/// the same convention the Billboard panel uses: positive means moved up.
#[must_use]
pub fn parse_move(text: Option<&str>) -> Movement {
    let value = text.unwrap_or_default().trim().to_uppercase();
    if value == "NEW" || value == "RE" {
        return Movement::DEBUT;
    }
    // No movement column at all (the all-time table has none) is "unknown", which
    // is not the same as "held position" and must not render as `=`.
    if value.is_empty() {
        return Movement::UNKNOWN;
    }
    if value == "=" {
        return Movement::moved(0);
    }
    let digits: String = value
        .chars()
        .filter(|ch| ch.is_ascii_digit() || *ch == '-')
        .collect();
    let Some(n) = js_parse_int(&digits) else {
        return Movement::moved(0);
    };
    let magnitude = i32::try_from(n.saturating_abs()).unwrap_or(i32::MAX);
    Movement::moved(if value.starts_with('-') {
        -magnitude
    } else {
        magnitude
    })
}

/// Split `"Shakira - Dai Dai (w/ Burna Boy)"` into artist and title.
///
/// Only the *first* separator counts: plenty of titles contain a dash of their
/// own, and splitting on the last one would file "Dai Dai (w/ Burna Boy)" under
/// the wrong artist. YouTube rows are frequently just a video name with no
/// separator at all, which stays whole as the title.
#[must_use]
pub fn split_artist_title(text: &str) -> (String, String) {
    let cleaned = scrape::collapse_ws(text);
    match cleaned.find(" - ") {
        None => (String::new(), cleaned),
        Some(index) => (
            cleaned[..index].trim().to_owned(),
            cleaned[index + 3..].trim().to_owned(),
        ),
    }
}

/// The chart date, which kworb states in the page title on the dated charts.
static CHART_DATE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(\d{4}-\d{2}-\d{2})").expect("CHART_DATE is a valid regex"));

pub fn parse_chart_table(html: &str, spec: &ChartSpec, source_url: &str) -> Result<StreamChart> {
    let doc = scrape::parse_document(html);

    // Column index by header name. kworb leaves the rank and movement headers
    // blank on some pages, so those two fall back to their fixed leading
    // positions — a convention that holds across every table on the site.
    let table = Table::require_first_in(&doc, "chart table", source_url)
        .map_err(|err| {
            err.with_hint(
                "kworb.net answered but served no chart table. The chart slug may be wrong.",
            )
        })?
        .with_positional_headers(&["pos", "move"]);

    // `Views` is the period figure on the daily charts but the *cumulative*
    // total on the all-time table, where `Yesterday` carries the daily number.
    let has_yesterday = table.has_column("yesterday");

    let mut entries: Vec<StreamEntry> = Vec::new();

    for row in table.rows() {
        let label = row
            .first_of(&["artistandtitle", "video", "title", "artist"])
            .unwrap_or_default();
        if label.is_empty() {
            continue;
        }

        let (artist, title) = split_artist_title(label);
        let movement = parse_move(row.first_of(&["move", "pplus"]));
        let rank = as_count(int_or_null(row.get("pos"))).unwrap_or_else(|| position(entries.len()));

        let period_streams = if has_yesterday {
            int_or_null(row.get("yesterday"))
        } else {
            int_or_null(row.first_of(&["streams", "views"]))
        };
        let total = if has_yesterday {
            int_or_null(row.get("views"))
        } else {
            int_or_null(row.get("total"))
        };

        entries.push(StreamEntry {
            rank,
            last_rank: movement
                .movement
                .and_then(|moved| u32::try_from(i64::from(rank) + i64::from(moved)).ok()),
            title,
            artist,
            streams: as_number(period_streams),
            streams_change: as_number(signed_or_null(row.get("streamsplus"))),
            total: as_number(total),
            peak: as_count(int_or_null(row.first_of(&["pk", "peak"]))),
            days: as_count(int_or_null(row.first_of(&["days", "wks", "weeks"]))),
            movement: movement.movement,
            is_new: movement.is_new,
        });
    }

    if entries.is_empty() {
        return Err(
            UpstreamError::parse_failed(format!("No chart entries found on {source_url}"))
                .with_hint(
                    "kworb.net returned a page but its rows did not match the expected \
                 table shape. The chart may have been renamed, or the layout changed.",
                ),
        );
    }

    let page_title = scrape::text_of_first(&doc, css!("title")).unwrap_or_default();
    let date = CHART_DATE
        .captures(&page_title)
        .map_or_else(String::new, |caps| caps[1].to_owned());

    Ok(StreamChart {
        source: spec.source.clone(),
        chart: spec.slug.clone(),
        title: if page_title.is_empty() {
            spec.name.clone()
        } else {
            page_title
        },
        date,
        entries,
        source_url: source_url.to_owned(),
    })
}

/// Rows returned by default.
///
/// kworb's chart pages are not all the same length: a Spotify country chart is
/// 200 rows, but the YouTube all-time table runs to several thousand. Handing
/// every one of those to the client means a panel that renders thousands of DOM
/// rows nobody scrolls to, so the tail is trimmed here rather than in the panel.
pub const DEFAULT_LIMIT: u32 = 200;

pub async fn get_chart(state: &AppState, spec: &ChartSpec, limit: u32) -> Result<StreamChart> {
    let source_url = format!("{}{}", state.config().kworb_base, spec.path);
    let key = format!("kworb:{}:{}", spec.source, spec.slug);

    let chart = state
        .cache()
        .cached(&key, ttl::STREAM_CHARTS, || async {
            let html = state
                .http()
                .fetch_text(
                    &source_url,
                    FetchOptions::new()
                        .timeout(Duration::from_secs(30))
                        .retries(2),
                )
                .await?;
            parse_chart_table(&html, spec, &source_url)
        })
        .await?;

    let mut chart = (*chart).clone();
    chart.entries.truncate(limit.max(1) as usize);
    Ok(chart)
}

#[cfg(test)]
mod tests {
    //! kworb parser tests.
    //!
    //! These lean on the cases where a plausible-looking parser reads the wrong
    //! number without ever failing: two columns whose headers normalise to the
    //! same key, and a chart with no movement column at all.

    use super::*;
    use crate::config::Config;
    use crate::error::codes;
    use crate::http::Http;
    use wiremock::matchers::{method, path as path_matcher};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn spotify_spec() -> ChartSpec {
        ChartSpec::new(
            "us-daily",
            "Spotify Daily — US",
            "spotify",
            "/spotify/country/us_daily.html",
        )
    }

    fn youtube_spec() -> ChartSpec {
        ChartSpec::new(
            "alltime",
            "YouTube all time",
            "youtube",
            "/youtube/topvideos.html",
        )
    }

    /// Mirrors kworb's daily table, including the `Streams` / `Streams+` pair.
    const SPOTIFY_HTML: &str = r#"<html><head><title>Spotify Daily Chart - United States</title></head><body>
<table>
  <thead><tr>
    <th>Pos</th><th>P+</th><th>Artist and Title</th><th>Days</th><th>Pk</th>
    <th>(x?)</th><th>Streams</th><th>Streams+</th><th>7Day</th><th>7Day+</th><th>Total</th>
  </tr></thead>
  <tbody>
    <tr><td>1</td><td>=</td><td>Ella Langley - Choosin' Texas</td><td>302</td><td>1</td><td>(x88)</td>
        <td>1,466,839</td><td>+84,270</td><td>10,022,760</td><td>-116,663</td><td>389,670,040</td></tr>
    <tr><td>2</td><td>+3</td><td>Shakira - Dai Dai (w/ Burna Boy)</td><td>12</td><td>2</td><td>(x5)</td>
        <td>1,145,809</td><td>-75,692</td><td>7,596,216</td><td>+231,164</td><td>12,679,364</td></tr>
    <tr><td>3</td><td>NEW</td><td>KATSEYE - Hootie Frutti</td><td>1</td><td>3</td><td>(x1)</td>
        <td>900,000</td><td>-</td><td>900,000</td><td>-</td><td>900,000</td></tr>
  </tbody>
</table></body></html>"#;

    /// The all-time table: no movement column, and `Views` is cumulative.
    const YOUTUBE_HTML: &str = r#"<html><head><title>YouTube - Most Viewed Music Videos of All Time</title></head><body>
<table>
  <thead><tr><th>Video</th><th>Views</th><th>Yesterday</th></tr></thead>
  <tbody>
    <tr><td>Luis Fonsi - Despacito ft. Daddy Yankee</td><td>9,099,389,704</td><td>803,323</td></tr>
    <tr><td>Baby Shark Dance</td><td>15,000,000,000</td><td>1,200,000</td></tr>
  </tbody>
</table></body></html>"#;

    fn spotify_entries() -> Vec<StreamEntry> {
        parse_chart_table(SPOTIFY_HTML, &spotify_spec(), "u")
            .unwrap()
            .entries
    }

    fn youtube_entries() -> Vec<StreamEntry> {
        parse_chart_table(YOUTUBE_HTML, &youtube_spec(), "u")
            .unwrap()
            .entries
    }

    /* ------------------------------------------------- parse_chart_table */

    #[test]
    fn reads_the_value_column_and_its_delta_separately() {
        // `Streams` and `Streams+` both reduce to `streams` unless `+` is preserved,
        // which would silently make the delta unreadable.
        let entries = spotify_entries();
        assert_eq!(entries[0].streams, Some(1_466_839.0));
        assert_eq!(entries[0].streams_change, Some(84_270.0));
        assert_eq!(entries[1].streams_change, Some(-75_692.0));
    }

    #[test]
    fn splits_artist_from_title_on_the_first_separator_only() {
        let entries = spotify_entries();
        assert_eq!(entries[1].artist, "Shakira");
        assert_eq!(entries[1].title, "Dai Dai (w/ Burna Boy)");
    }

    #[test]
    fn reads_peak_days_and_cumulative_total_by_header_name() {
        let entries = spotify_entries();
        assert_eq!(entries[0].peak, Some(1));
        assert_eq!(entries[0].days, Some(302));
        assert_eq!(entries[0].total, Some(389_670_040.0));
    }

    #[test]
    fn derives_last_weeks_rank_from_the_movement_cell() {
        let entries = spotify_entries();
        assert_eq!(entries[0].last_rank, Some(1)); // held at 1
        assert_eq!(entries[1].last_rank, Some(5)); // climbed 3 into 2
    }

    #[test]
    fn marks_a_debut_as_new_with_no_previous_rank() {
        let entries = spotify_entries();
        assert!(entries[2].is_new);
        assert_eq!(entries[2].last_rank, None);
    }

    #[test]
    fn throws_a_diagnosable_error_when_no_rows_match() {
        let error = parse_chart_table(
            "<html><body><table></table></body></html>",
            &spotify_spec(),
            "https://k/",
        )
        .unwrap_err();
        assert!(
            error.message.contains("No chart entries found"),
            "{}",
            error.message
        );
        assert_eq!(error.code, codes::PARSE_FAILED);
    }

    #[test]
    fn throws_when_there_is_no_table_at_all() {
        let error = parse_chart_table(
            "<html><body><p>hi</p></body></html>",
            &spotify_spec(),
            "https://k/",
        )
        .unwrap_err();
        assert!(
            error.message.contains("No chart table found"),
            "{}",
            error.message
        );
        assert_eq!(error.code, codes::PARSE_FAILED);
    }

    /* -------------------------------------- parse_chart_table (all-time) */

    #[test]
    fn treats_views_as_cumulative_and_yesterday_as_the_daily_figure() {
        let entries = youtube_entries();
        assert_eq!(entries[0].total, Some(9_099_389_704.0));
        assert_eq!(entries[0].streams, Some(803_323.0));
    }

    #[test]
    fn leaves_a_video_with_no_separator_whole() {
        let entries = youtube_entries();
        assert_eq!(entries[1].title, "Baby Shark Dance");
        assert_eq!(entries[1].artist, "");
    }

    #[test]
    fn reports_unknown_movement_rather_than_inventing_a_debut_or_a_hold() {
        // This table has no movement column. Reporting `0` would print `=` for every
        // row; reporting a debut would print NEW for a decade-old video.
        let entries = youtube_entries();
        assert_eq!(entries[0].movement, None);
        assert!(!entries[0].is_new);
    }

    #[test]
    fn ranks_by_document_order_when_there_is_no_rank_column() {
        let ranks: Vec<u32> = youtube_entries().iter().map(|entry| entry.rank).collect();
        assert_eq!(ranks, vec![1, 2]);
    }

    /* ------------------------------------------------------- parse_move */

    #[test]
    fn parse_move_reads_the_four_shapes_kworb_emits() {
        assert_eq!(parse_move(Some("=")), Movement::moved(0));
        assert_eq!(parse_move(Some("+3")), Movement::moved(3));
        assert_eq!(parse_move(Some("-1")), Movement::moved(-1));
        assert_eq!(parse_move(Some("NEW")), Movement::DEBUT);
    }

    #[test]
    fn parse_move_treats_a_re_entry_like_a_debut() {
        assert_eq!(parse_move(Some("RE")), Movement::DEBUT);
    }

    #[test]
    fn parse_move_distinguishes_an_absent_column_from_a_held_position() {
        assert_eq!(parse_move(None), Movement::UNKNOWN);
        assert_eq!(parse_move(Some("")), Movement::UNKNOWN);
        assert_ne!(parse_move(Some("")), parse_move(Some("=")));
    }

    /* ------------------------------------------------ split_artist_title */

    #[test]
    fn split_artist_title_splits_on_the_first_separator_not_the_last() {
        assert_eq!(
            split_artist_title("Tyler, The Creator - Sticky - Remix"),
            ("Tyler, The Creator".to_owned(), "Sticky - Remix".to_owned())
        );
    }

    #[test]
    fn split_artist_title_keeps_a_string_with_no_separator_whole() {
        assert_eq!(
            split_artist_title("Baby Shark Dance"),
            (String::new(), "Baby Shark Dance".to_owned())
        );
    }

    #[test]
    fn split_artist_title_does_not_split_on_a_hyphen_without_spaces() {
        assert_eq!(
            split_artist_title("Spider-Man"),
            (String::new(), "Spider-Man".to_owned())
        );
    }

    /* ----------------------------------------------------- chart lookup */

    #[test]
    fn lists_every_chart_or_just_one_platforms() {
        assert_eq!(list_charts(None).len(), 7);
        let spotify = list_charts(Some("spotify"));
        assert_eq!(spotify.len(), 4);
        assert!(spotify.iter().all(|chart| chart.source == "spotify"));
        assert_eq!(list_charts(Some("youtube")).len(), 3);
        assert_eq!(spotify[0].slug, "us-daily");
        assert_eq!(spotify[0].name, "Spotify Daily — United States");
    }

    #[test]
    fn resolves_a_bare_spot_to_the_us_daily_chart() {
        let spec = resolve_spotify_chart("", "").unwrap();
        assert_eq!(spec.slug, "us-daily");
        assert_eq!(spec.path, "/spotify/country/us_daily.html");
    }

    #[test]
    fn resolves_a_country_and_cadence_kworb_carries_but_the_catalogue_does_not() {
        let spec = resolve_spotify_chart(" GB ", "Weekly").unwrap();
        assert_eq!(spec.slug, "gb-weekly");
        assert_eq!(spec.name, "Spotify Weekly — GB");
        assert_eq!(spec.path, "/spotify/country/gb_weekly.html");
        assert_eq!(spec.source, "spotify");

        // `global` is not two letters, and is a real chart all the same.
        assert_eq!(
            resolve_spotify_chart("global", "").unwrap().slug,
            "global-daily"
        );
    }

    #[test]
    fn refuses_a_scope_that_is_not_a_country_code() {
        let error = resolve_spotify_chart("united kingdom", "").unwrap_err();
        assert_eq!(error.code, codes::BAD_REQUEST);
        assert!(error.hint.unwrap().contains("two-letter code"));
    }

    #[test]
    fn resolves_youtube_views_only_from_the_catalogue() {
        assert_eq!(resolve_youtube_chart("").unwrap().slug, "today");
        assert_eq!(resolve_youtube_chart(" ALLTIME ").unwrap().slug, "alltime");
        let error = resolve_youtube_chart("weekly").unwrap_err();
        assert_eq!(error.code, codes::BAD_REQUEST);
        assert!(error.message.contains("not a YouTube chart"));
    }

    /* --------------------------------------------------------- get_chart */

    fn state_for(server: &MockServer) -> AppState {
        AppState::with_http(
            Config {
                kworb_base: server.uri().trim_end_matches('/').to_owned(),
                ..Config::default()
            },
            Http::new(),
        )
    }

    #[tokio::test]
    async fn fetches_the_chart_from_the_path_the_spec_names() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/spotify/country/us_daily.html"))
            .respond_with(ResponseTemplate::new(200).set_body_string(SPOTIFY_HTML))
            .expect(1)
            .mount(&server)
            .await;

        let chart = get_chart(&state_for(&server), &spotify_spec(), DEFAULT_LIMIT)
            .await
            .unwrap();

        assert_eq!(chart.source, "spotify");
        assert_eq!(chart.chart, "us-daily");
        assert_eq!(chart.title, "Spotify Daily Chart - United States");
        assert_eq!(chart.entries.len(), 3);
        assert!(chart.source_url.ends_with("/spotify/country/us_daily.html"));
    }

    #[tokio::test]
    async fn trims_the_tail_the_panel_would_never_scroll_to() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string(SPOTIFY_HTML))
            // One upstream read serves both calls: the second is a cache hit.
            .expect(1)
            .mount(&server)
            .await;

        let state = state_for(&server);
        let spec = spotify_spec();
        assert_eq!(get_chart(&state, &spec, 2).await.unwrap().entries.len(), 2);
        // A zero limit still answers with the leader rather than nothing.
        assert_eq!(get_chart(&state, &spec, 0).await.unwrap().entries.len(), 1);
    }

    #[tokio::test]
    async fn caches_each_chart_under_its_own_key() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/spotify/country/us_daily.html"))
            .respond_with(ResponseTemplate::new(200).set_body_string(SPOTIFY_HTML))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path_matcher("/youtube/topvideos.html"))
            .respond_with(ResponseTemplate::new(200).set_body_string(YOUTUBE_HTML))
            .expect(1)
            .mount(&server)
            .await;

        let state = state_for(&server);
        get_chart(&state, &spotify_spec(), DEFAULT_LIMIT)
            .await
            .unwrap();
        get_chart(&state, &spotify_spec(), DEFAULT_LIMIT)
            .await
            .unwrap();
        get_chart(&state, &youtube_spec(), DEFAULT_LIMIT)
            .await
            .unwrap();

        assert!(state
            .cache()
            .get::<StreamChart>("kworb:spotify:us-daily")
            .await
            .is_some());
        assert!(state
            .cache()
            .get::<StreamChart>("kworb:youtube:alltime")
            .await
            .is_some());
    }

    #[tokio::test]
    async fn reads_the_chart_date_out_of_the_page_title() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                "<html><head><title>Spotify Daily Chart 2026-08-14</title></head><body>\
                 <table><tr><th>Pos</th><th>Artist and Title</th></tr>\
                 <tr><td>1</td><td>Alex Warren - Ordinary</td></tr></table></body></html>",
            ))
            .mount(&server)
            .await;

        let chart = get_chart(&state_for(&server), &spotify_spec(), DEFAULT_LIMIT)
            .await
            .unwrap();
        assert_eq!(chart.date, "2026-08-14");
        assert_eq!(chart.entries[0].artist, "Alex Warren");
    }

    #[tokio::test]
    async fn a_page_with_no_date_in_its_title_reports_none() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string(YOUTUBE_HTML))
            .mount(&server)
            .await;

        let chart = get_chart(&state_for(&server), &youtube_spec(), DEFAULT_LIMIT)
            .await
            .unwrap();
        assert_eq!(chart.date, "");
        assert_eq!(
            chart.title,
            "YouTube - Most Viewed Music Videos of All Time"
        );
    }

    #[tokio::test]
    async fn an_untitled_page_falls_back_to_the_specs_own_name() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                "<html><body><table><tr><th>Video</th><th>Views</th></tr>\
                 <tr><td>Baby Shark Dance</td><td>15,000,000,000</td></tr></table></body></html>",
            ))
            .mount(&server)
            .await;

        let chart = get_chart(&state_for(&server), &youtube_spec(), DEFAULT_LIMIT)
            .await
            .unwrap();
        assert_eq!(chart.title, "YouTube all time");
        assert_eq!(chart.entries[0].streams, Some(15_000_000_000.0));
    }

    #[tokio::test]
    async fn a_failed_parse_is_not_cached() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200).set_body_string("<html><body>nope</body></html>"),
            )
            .expect(2)
            .mount(&server)
            .await;

        let state = state_for(&server);
        let spec = spotify_spec();
        assert_eq!(
            get_chart(&state, &spec, DEFAULT_LIMIT)
                .await
                .unwrap_err()
                .code,
            codes::PARSE_FAILED
        );
        assert!(get_chart(&state, &spec, DEFAULT_LIMIT).await.is_err());
    }
}
