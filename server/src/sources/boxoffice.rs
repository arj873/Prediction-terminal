//! Domestic daily box office — scraped from boxofficemojo.com.
//!
//! The film side of Kalshi's entertainment book trades on how a release performs
//! — `KXGGBOXOFFICE` settles on box office achievement outright, and the opening
//! weekend is the strongest public read on the award and Rotten Tomatoes markets
//! that sit around a title.
//!
//! Mojo serves a plain server-rendered table, which makes this the least fragile
//! of the scraped sources: one table, a labelled header row, one row per release.
//! Columns are resolved by header text so that Mojo adding a column does not
//! silently shift "theaters" into "days in release".

use std::sync::LazyLock;
use std::time::Duration;

use regex::Regex;
use terminal_core::types::{BoxOfficeDay, BoxOfficeEntry};
use time::format_description::BorrowedFormatItem;
use time::{Date, OffsetDateTime};

use crate::app::AppState;
use crate::cache::ttl;
use crate::css;
use crate::error::{codes, Result, UpstreamError};
use crate::http::FetchOptions;
use crate::scrape::{self, Table};

static DATE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\d{4}-\d{2}-\d{2}$").expect("DATE is a valid regex"));

/// `YYYY-MM-DD`, the only shape either the route or Mojo's URLs accept.
const ISO_DATE: &[BorrowedFormatItem<'_>] =
    time::macros::format_description!("[year]-[month]-[day]");

pub fn assert_date(raw: &str) -> Result<String> {
    let date = raw.trim();
    if !DATE.is_match(date) || Date::parse(date, ISO_DATE).is_err() {
        return Err(UpstreamError::bad_request(format!(
            "\"{raw}\" is not a valid box office date"
        ))
        .with_hint("Dates are YYYY-MM-DD, e.g. `BO 2026-08-14`."));
    }
    Ok(date.to_owned())
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

/// `Number.parseFloat(text)`, which reads the longest numeric prefix and
/// shrugs at whatever follows it.
fn js_parse_float(text: &str) -> Option<f64> {
    let bytes = text.as_bytes();
    let mut end = usize::from(matches!(bytes.first(), Some(b'+' | b'-')));
    let mut digits = false;

    while end < bytes.len() && bytes[end].is_ascii_digit() {
        end += 1;
        digits = true;
    }
    if bytes.get(end) == Some(&b'.') {
        end += 1;
        while end < bytes.len() && bytes[end].is_ascii_digit() {
            end += 1;
            digits = true;
        }
    }
    if !digits {
        return None;
    }

    // An exponent counts only when it is complete; `1e` is `1`, as in JavaScript.
    if matches!(bytes.get(end), Some(b'e' | b'E')) {
        let mut cursor = end + 1;
        if matches!(bytes.get(cursor), Some(b'+' | b'-')) {
            cursor += 1;
        }
        let exponent = cursor;
        while cursor < bytes.len() && bytes[cursor].is_ascii_digit() {
            cursor += 1;
        }
        if cursor > exponent {
            end = cursor;
        }
    }

    text[..end].parse::<f64>().ok()
}

/// Strip the characters a figure is printed with, then apply the `-`/`--`
/// convention Mojo uses for "no comparison to make".
fn cleaned(text: Option<&str>, strip: &[char]) -> Option<String> {
    let text = text?;
    if text.is_empty() {
        return None;
    }
    let cleaned: String = text
        .chars()
        .filter(|ch| !(strip.contains(ch) || ch.is_whitespace()))
        .collect();
    if cleaned.is_empty() || cleaned == "-" || cleaned == "--" {
        return None;
    }
    Some(cleaned)
}

/// `"$19,000,000"` → 19000000; `"-"`, `""` → null.
fn money_or_null(text: Option<&str>) -> Option<f64> {
    cleaned(text, &['$', ','])
        .and_then(|text| js_parse_int(&text))
        .map(|n| n as f64)
}

fn int_or_null(text: Option<&str>) -> Option<i64> {
    cleaned(text, &[',']).and_then(|text| js_parse_int(&text))
}

/// A count — a rank, a theatre tally, days in release. Anything that will not
/// fit is no count at all rather than a wrapped one.
fn count_or_null(text: Option<&str>) -> Option<u32> {
    int_or_null(text).and_then(|n| u32::try_from(n).ok())
}

/// A row's own one-based position, for a table that states no rank of its own.
fn position(rows_so_far: usize) -> u32 {
    u32::try_from(rows_so_far + 1).unwrap_or(u32::MAX)
}

/// `"+68.9%"` → 68.9; `"-55.6%"` → -55.6; a bare `"-"` (no comparison) → null.
fn percent_or_null(text: Option<&str>) -> Option<f64> {
    cleaned(text, &['%', ',']).and_then(|text| js_parse_float(&text))
}

pub fn parse_daily_page(html: &str, date: &str, source_url: &str) -> Result<BoxOfficeDay> {
    let doc = scrape::parse_document(html);

    let table = match Table::first_in(&doc) {
        Some(table) => table,
        None => {
            // Mojo serves a perfectly valid page for a date it has not posted grosses
            // for yet, carrying a "No data available" notice instead of a table. That is
            // an empty day, not a broken parser, and saying so is the difference between
            // "wait for tomorrow" and "go fix the scraper".
            let body = scrape::select_one(&doc, css!("body"))
                .map(scrape::text_of)
                .unwrap_or_default();
            if body.to_lowercase().contains("no data available") {
                return Err(UpstreamError::not_found(format!(
                    "Box Office Mojo has no grosses for {date} yet"
                ))
                .with_hint(
                    "Daily grosses are usually posted the following afternoon. Try an \
                     earlier date, e.g. `BO 2026-08-14`.",
                ));
            }

            return Err(UpstreamError::parse_failed(format!(
                "No box office table found on {source_url}"
            ))
            .with_hint("Box Office Mojo answered but served no results table for that date."));
        }
    };

    let mut entries: Vec<BoxOfficeEntry> = Vec::new();

    for row in table.rows() {
        let title = row.first_of(&["release", "title"]).unwrap_or_default();
        if title.is_empty() {
            continue;
        }

        let rank =
            count_or_null(row.first_of(&["td", "rank"])).unwrap_or_else(|| position(entries.len()));
        let last_rank = count_or_null(row.get("yd"));

        entries.push(BoxOfficeEntry {
            rank,
            last_rank,
            title: title.to_owned(),
            gross: money_or_null(row.first_of(&["daily", "gross"])),
            change_day: percent_or_null(row.get("pctyd")),
            change_week: percent_or_null(row.get("pctlw")),
            theaters: count_or_null(row.get("theaters")),
            average: money_or_null(row.get("avg")),
            total_gross: money_or_null(row.get("todate")),
            days_in_release: count_or_null(row.get("days")),
            distributor: row.get("distributor").unwrap_or_default().to_owned(),
            // A release with no yesterday rank opened today.
            movement: last_rank
                .map(|last| i64::from(last) - i64::from(rank))
                .map(|moved| i32::try_from(moved).unwrap_or(0)),
            is_new: last_rank.is_none(),
        });
    }

    if entries.is_empty() {
        return Err(UpstreamError::parse_failed(format!(
            "No box office entries found on {source_url}"
        ))
        .with_hint(
            "Box Office Mojo returned a page but its rows did not match the expected \
                     table shape. That date may predate their daily coverage.",
        ));
    }

    entries.sort_by_key(|entry| entry.rank);

    let heading = scrape::text_of_first(&doc, css!("h1"));

    Ok(BoxOfficeDay {
        date: date.to_owned(),
        title: heading.unwrap_or_else(|| format!("Domestic Box Office for {date}")),
        total_gross: entries.iter().map(|entry| entry.gross.unwrap_or(0.0)).sum(),
        entries,
        source_url: source_url.to_owned(),
    })
}

/// `n` days before today, as `YYYY-MM-DD`.
fn days_ago_iso(n: u32) -> String {
    let date =
        (OffsetDateTime::now_utc() - Duration::from_secs(u64::from(n) * 24 * 60 * 60)).date();
    date.format(ISO_DATE).unwrap_or_default()
}

/// How far back `BO` will look for the most recent posted chart.
///
/// Grosses land the afternoon after the day they cover, so "today" is always
/// empty and yesterday usually is too. Holidays and reporting gaps can stretch
/// that, so the walk-back covers a few days rather than assuming a fixed lag —
/// and stops rather than crawling indefinitely into a genuine outage.
pub const MAX_LOOKBACK_DAYS: u32 = 5;

async fn fetch_day(state: &AppState, date: &str) -> Result<BoxOfficeDay> {
    let source_url = format!("{}/date/{date}/", state.config().boxoffice_base);
    let day = state
        .cache()
        .cached(&format!("boxoffice:{date}"), ttl::BOX_OFFICE, || async {
            let html = state
                .http()
                .fetch_text(
                    &source_url,
                    FetchOptions::new()
                        .timeout(Duration::from_secs(30))
                        .retries(2),
                )
                .await?;
            parse_daily_page(&html, date, &source_url)
        })
        .await?;
    Ok((*day).clone())
}

pub async fn get_daily(state: &AppState, raw_date: Option<&str>) -> Result<BoxOfficeDay> {
    // An explicit date is answered exactly — including with "no grosses yet",
    // which is the honest answer and not something to paper over.
    if let Some(raw) = raw_date.filter(|raw| !raw.is_empty()) {
        return fetch_day(state, &assert_date(raw)?).await;
    }

    let mut last_error: Option<UpstreamError> = None;
    for back in 1..=MAX_LOOKBACK_DAYS {
        match fetch_day(state, &days_ago_iso(back)).await {
            Ok(day) => return Ok(day),
            Err(err) => {
                // Only an unposted day is worth stepping back from; a block or a timeout
                // would repeat five times over and bury the real cause.
                if err.code != codes::NOT_FOUND {
                    return Err(err);
                }
                last_error = Some(err);
            }
        }
    }

    Err(last_error.unwrap_or_else(|| {
        UpstreamError::not_found("Box Office Mojo has posted no recent daily grosses")
    }))
}

#[cfg(test)]
mod tests {
    //! Box Office Mojo parser tests.
    //!
    //! The two that matter carry the whole module: `YD` beside `%± YD`, which a
    //! percent-stripping header key reads as a rank; and an upstream that answers
    //! `200 OK` with "no data" instead of an error.

    use super::*;
    use crate::config::Config;
    use crate::http::Http;
    use wiremock::matchers::{method, path as path_matcher};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const BO_HTML: &str = r#"<html><body><h1>Domestic Box Office For Aug 14, 2026</h1>
<table>
  <tr><th>TD</th><th>YD</th><th>Release</th><th>Daily</th><th>%± YD</th><th>%± LW</th>
      <th>Theaters</th><th>Avg</th><th>To Date</th><th>Days</th><th>Distributor</th></tr>
  <tr><td>1</td><td>3</td><td>Spider-Man: Brand New Day</td><td>$19,000,000</td><td>+68.9%</td><td>-55.6%</td>
      <td>4,539</td><td>$4,185</td><td>$734,831,670</td><td>15</td><td>Sony Pictures Releasing</td></tr>
  <tr><td>2</td><td>-</td><td>The End of Oak Street</td><td>$8,050,000</td><td>-</td><td>-</td>
      <td>3,446</td><td>$2,336</td><td>$8,050,000</td><td>1</td><td>Warner Bros.</td></tr>
</table></body></html>"#;

    /// Mojo's answer for a date it has not posted grosses for: `200 OK`, a
    /// heading, and a notice where the table belongs.
    const BO_EMPTY: &str = "<html><body><h1>Domestic Box Office For Aug 15, 2026</h1>\
                            <p>No data available.</p></body></html>";

    fn entries() -> Vec<BoxOfficeEntry> {
        parse_daily_page(BO_HTML, "2026-08-14", "u")
            .unwrap()
            .entries
    }

    /* -------------------------------------------------- parse_daily_page */

    #[test]
    fn does_not_confuse_the_yesterday_rank_with_the_day_over_day_percentage() {
        // `YD` and `%± YD` both normalise to `yd` unless the percent sign is kept,
        // which would report a rank of 3 as a +3% change.
        let entries = entries();
        assert_eq!(entries[0].last_rank, Some(3));
        assert_eq!(entries[0].change_day, Some(68.9));
        assert_eq!(entries[0].change_week, Some(-55.6));
    }

    #[test]
    fn parses_money_into_whole_dollars() {
        let entries = entries();
        assert_eq!(entries[0].gross, Some(19_000_000.0));
        assert_eq!(entries[0].total_gross, Some(734_831_670.0));
        assert_eq!(entries[0].average, Some(4185.0));
    }

    #[test]
    fn computes_movement_as_rank_improvement() {
        assert_eq!(entries()[0].movement, Some(2)); // 3 → 1
    }

    #[test]
    fn treats_a_release_with_no_yesterday_rank_as_an_opener() {
        let entries = entries();
        assert!(entries[1].is_new);
        assert_eq!(entries[1].change_day, None);
        assert_eq!(entries[1].days_in_release, Some(1));
        assert_eq!(entries[1].movement, None);
    }

    #[test]
    fn sums_the_days_gross() {
        assert_eq!(
            parse_daily_page(BO_HTML, "2026-08-14", "u")
                .unwrap()
                .total_gross,
            27_050_000.0
        );
    }

    #[test]
    fn reports_an_unposted_day_as_not_found_rather_than_a_parse_failure() {
        // Mojo serves 200 OK with this notice for a date it has not posted yet.
        // Calling that a parse failure sends someone to debug a working scraper.
        let error = parse_daily_page(BO_EMPTY, "2026-08-15", "u").unwrap_err();
        assert_eq!(error.code, codes::NOT_FOUND);
        assert!(
            error.message.contains("has no grosses for 2026-08-15 yet"),
            "{}",
            error.message
        );
    }

    #[test]
    fn still_reports_a_genuine_layout_change_as_a_parse_failure() {
        let error = parse_daily_page(
            "<html><body><p>surprise</p></body></html>",
            "2026-08-14",
            "u",
        )
        .unwrap_err();
        assert_eq!(error.code, codes::PARSE_FAILED);
        assert!(
            error.message.contains("No box office table found"),
            "{}",
            error.message
        );
    }

    #[test]
    fn reads_the_rest_of_the_row_by_header_name() {
        let entries = entries();
        assert_eq!(entries[0].rank, 1);
        assert_eq!(entries[0].title, "Spider-Man: Brand New Day");
        assert_eq!(entries[0].theaters, Some(4539));
        assert_eq!(entries[0].days_in_release, Some(15));
        assert_eq!(entries[0].distributor, "Sony Pictures Releasing");
        assert_eq!(
            parse_daily_page(BO_HTML, "2026-08-14", "u").unwrap().title,
            "Domestic Box Office For Aug 14, 2026"
        );
    }

    #[test]
    fn a_table_of_headers_and_nothing_else_is_a_parse_failure() {
        let error = parse_daily_page(
            "<html><body><table><tr><th>TD</th><th>Release</th></tr></table></body></html>",
            "2026-08-14",
            "https://m/",
        )
        .unwrap_err();
        assert_eq!(error.code, codes::PARSE_FAILED);
        assert!(
            error.message.contains("No box office entries found"),
            "{}",
            error.message
        );
    }

    #[test]
    fn orders_the_chart_by_rank_whatever_order_the_rows_arrive_in() {
        let day = parse_daily_page(
            "<html><body><table><tr><th>TD</th><th>Release</th></tr>\
             <tr><td>3</td><td>Third</td></tr><tr><td>1</td><td>First</td></tr></table></body></html>",
            "2026-08-14",
            "u",
        )
        .unwrap();
        assert_eq!(
            day.entries.iter().map(|e| e.rank).collect::<Vec<_>>(),
            vec![1, 3]
        );
        // No h1 on this page, so the date names it.
        assert_eq!(day.title, "Domestic Box Office for 2026-08-14");
    }

    /* --------------------------------------------------------- assert_date */

    #[test]
    fn assert_date_accepts_a_real_iso_date_and_nothing_else() {
        assert_eq!(assert_date(" 2026-08-14 ").unwrap(), "2026-08-14");
        for bad in ["2026-8-14", "14/08/2026", "yesterday", "", "2026-02-30"] {
            let error = assert_date(bad).unwrap_err();
            assert_eq!(error.code, codes::BAD_REQUEST, "{bad}");
            assert!(error.hint.unwrap().contains("YYYY-MM-DD"));
        }
    }

    /* ----------------------------------------------------------- get_daily */

    fn state_for(server: &MockServer) -> AppState {
        AppState::with_http(
            Config {
                boxoffice_base: server.uri().trim_end_matches('/').to_owned(),
                ..Config::default()
            },
            Http::new(),
        )
    }

    #[tokio::test]
    async fn fetches_and_caches_the_date_it_was_asked_for() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/date/2026-08-14/"))
            .respond_with(ResponseTemplate::new(200).set_body_string(BO_HTML))
            .expect(1)
            .mount(&server)
            .await;

        let state = state_for(&server);
        let day = get_daily(&state, Some("2026-08-14")).await.unwrap();
        assert_eq!(day.date, "2026-08-14");
        assert_eq!(day.entries.len(), 2);
        assert!(day.source_url.ends_with("/date/2026-08-14/"));

        get_daily(&state, Some("2026-08-14")).await.unwrap();
        assert!(state
            .cache()
            .get::<BoxOfficeDay>("boxoffice:2026-08-14")
            .await
            .is_some());
    }

    #[tokio::test]
    async fn an_explicit_date_with_no_grosses_is_answered_as_such() {
        // The walk-back is for "whatever is newest"; a named date gets the truth.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/date/2026-08-15/"))
            .respond_with(ResponseTemplate::new(200).set_body_string(BO_EMPTY))
            .expect(1)
            .mount(&server)
            .await;

        let error = get_daily(&state_for(&server), Some("2026-08-15"))
            .await
            .unwrap_err();
        assert_eq!(error.code, codes::NOT_FOUND);
    }

    #[tokio::test]
    async fn walks_back_from_yesterday_over_days_mojo_has_not_posted() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher(format!("/date/{}/", days_ago_iso(1))))
            .respond_with(ResponseTemplate::new(200).set_body_string(BO_EMPTY))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path_matcher(format!("/date/{}/", days_ago_iso(2))))
            .respond_with(ResponseTemplate::new(200).set_body_string(BO_HTML))
            .expect(1)
            .mount(&server)
            .await;

        let day = get_daily(&state_for(&server), None).await.unwrap();
        assert_eq!(day.date, days_ago_iso(2));
        assert_eq!(day.entries.len(), 2);
    }

    #[tokio::test]
    async fn gives_up_after_five_unposted_days_rather_than_crawling_backwards() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string(BO_EMPTY))
            .expect(u64::from(MAX_LOOKBACK_DAYS))
            .mount(&server)
            .await;

        let error = get_daily(&state_for(&server), None).await.unwrap_err();
        assert_eq!(error.code, codes::NOT_FOUND);
    }

    #[tokio::test]
    async fn a_block_stops_the_walk_back_instead_of_repeating_five_times() {
        // Five 403s in a row would bury the real cause under a "no grosses" answer.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(403))
            .expect(1)
            .mount(&server)
            .await;

        let error = get_daily(&state_for(&server), None).await.unwrap_err();
        assert_ne!(error.code, codes::NOT_FOUND);
    }

    #[tokio::test]
    async fn an_empty_date_walks_back_rather_than_failing_the_format_check() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher(format!("/date/{}/", days_ago_iso(1))))
            .respond_with(ResponseTemplate::new(200).set_body_string(BO_HTML))
            .expect(1)
            .mount(&server)
            .await;

        let day = get_daily(&state_for(&server), Some("")).await.unwrap();
        assert_eq!(day.date, days_ago_iso(1));
    }
}
