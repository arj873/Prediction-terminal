//! Netflix Top 10 — the official weekly publication behind netflix.com/tudum.
//!
//! Netflix publishes the same data its Top 10 site renders as plain TSV, which
//! is what this reads. That matters for a settlement feed: the TSV is the
//! *stated* source, it carries the exact figures (views, hours viewed, weeks in
//! the top 10) that Kalshi's `KXNETFLIXRANK*` and `KXNETFLIXTOPVIEWS*` markets
//! resolve against, and it does not depend on scraping a JavaScript-rendered
//! page that could change shape any week.
//!
//! Two files, because Netflix splits the data differently by scope:
//!
//! ```text
//!   all-weeks-global.tsv      ~1 MB   ranks *and* views/hours, English + non-English
//!   all-weeks-countries.tsv   ~31 MB  ranks only, ~93 countries
//! ```
//!
//! The country file is large enough to be worth handling deliberately: it is
//! fetched at most once per [`ttl::NETFLIX`], reduced immediately to the most
//! recent week for *every* country, and only that reduction is cached. So `NFLX
//! us` and `NFLX gb` share one download, and the 31 MB never lives in the cache.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use csv::StringRecord;
use regex::Regex;
use terminal_core::types::{NetflixEntry, NetflixTop10};

use crate::app::AppState;
use crate::cache::ttl;
use crate::error::{Result, UpstreamError};
use crate::http::FetchOptions;

/// `{netflix_base}/tudum/top10` — the site's own data directory.
fn global_url(state: &AppState) -> String {
    format!(
        "{}/tudum/top10/data/all-weeks-global.tsv",
        state.config().netflix_base
    )
}

fn countries_url(state: &AppState) -> String {
    format!(
        "{}/tudum/top10/data/all-weeks-countries.tsv",
        state.config().netflix_base
    )
}

/// The country file is ~31 MB; give it room without lifting the global ceiling.
const COUNTRIES_MAX_BYTES: usize = 96 * 1024 * 1024;

const GLOBAL_MAX_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetflixCategory {
    Tv,
    Films,
}

impl NetflixCategory {
    /// The value the wire type carries.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Tv => "tv",
            Self::Films => "films",
        }
    }

    /// How the category is written in a heading and in an error.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Tv => "TV",
            Self::Films => "Films",
        }
    }
}

static COUNTRY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z]{2}$").expect("COUNTRY is a valid regex"));

pub fn assert_category(raw: &str) -> Result<NetflixCategory> {
    match raw.trim().to_lowercase().as_str() {
        "tv" | "shows" | "show" | "series" => Ok(NetflixCategory::Tv),
        "films" | "film" | "movies" | "movie" => Ok(NetflixCategory::Films),
        _ => Err(
            UpstreamError::bad_request(format!("\"{raw}\" is not a Netflix category"))
                .with_hint("Categories are `tv` and `films`, e.g. `NFLX tv us`."),
        ),
    }
}

/// `global`, or a two-letter country code.
pub fn assert_scope(raw: &str) -> Result<String> {
    let value = raw.trim().to_lowercase();
    if value.is_empty() || value == "global" || value == "world" {
        return Ok("global".to_string());
    }
    if COUNTRY.is_match(&value) {
        return Ok(value);
    }
    Err(
        UpstreamError::bad_request(format!("\"{raw}\" is not a valid Netflix scope"))
            .with_hint("Use `global` or a two-letter country code, e.g. `NFLX tv us`."),
    )
}

fn num_or_null(value: Option<&str>) -> Option<f64> {
    let value = value?;
    if value.is_empty() {
        return None;
    }
    let cleaned: String = value
        .chars()
        .filter(|ch| *ch != ',' && !ch.is_whitespace())
        .collect();
    if cleaned.is_empty() || cleaned == "N/A" {
        return None;
    }
    cleaned.parse::<f64>().ok().filter(|n| n.is_finite())
}

/// The same reading, narrowed to the unsigned counts the wire type carries.
/// `None` stays `None`: a week Netflix has not published is not week zero.
fn count_or_null(value: Option<&str>) -> Option<u32> {
    let number = num_or_null(value)?;
    if number < 0.0 || number > f64::from(u32::MAX) {
        return None;
    }
    Some(number as u32)
}

/// Netflix writes an absent season as the literal string `N/A`.
fn text_or_empty(value: Option<&str>) -> String {
    let value = value.unwrap_or("").trim();
    if value == "N/A" {
        String::new()
    } else {
        value.to_string()
    }
}

/// A TSV row keyed by the header line.
pub type Row = HashMap<String, String>;

/// A field as the row reports it, distinguishing a column the file does not
/// have (`None`) from one it leaves empty (`Some("")`).
fn field<'a>(row: &'a Row, name: &str) -> Option<&'a str> {
    row.get(name).map(String::as_str)
}

/// One pass over a Netflix TSV.
///
/// The reader hands out borrowed fields and materialises a [`Row`] only when
/// asked. That is the whole reason the 31 MB country file is affordable:
/// building a map for every row would allocate several hundred thousand of them
/// to keep about 1,900.
struct Tsv<'a> {
    reader: csv::Reader<&'a [u8]>,
    columns: Vec<String>,
}

impl<'a> Tsv<'a> {
    fn new(tsv: &'a str) -> Self {
        let mut reader = csv::ReaderBuilder::new()
            .delimiter(b'\t')
            // Netflix does not quote anything: a double quote in a title is a
            // literal one, and letting the reader treat it as an opener would
            // swallow the rest of the file into a single field.
            .quoting(false)
            // A short row is padded and a long one truncated, exactly as reading
            // the line column by column did.
            .flexible(true)
            .trim(csv::Trim::All)
            .from_reader(tsv.as_bytes());
        let columns = reader
            .headers()
            .map(|header| header.iter().map(str::to_owned).collect())
            .unwrap_or_default();
        Self { reader, columns }
    }

    fn index(&self, name: &str) -> Option<usize> {
        self.columns.iter().position(|column| column == name)
    }

    /// The next data row, or `false` at the end of the file. A malformed tail
    /// ends the read rather than failing the feed.
    ///
    /// A line that is blank once trimmed is skipped rather than returned as a
    /// row of empty cells — Netflix's files end with one, and a run of tabs is
    /// as blank as a run of spaces.
    fn read(&mut self, record: &mut StringRecord) -> bool {
        while self.reader.read_record(record).unwrap_or(false) {
            if record.iter().any(|cell| !cell.is_empty()) {
                return true;
            }
        }
        false
    }

    fn row(&self, record: &StringRecord) -> Row {
        self.columns
            .iter()
            .enumerate()
            .map(|(i, name)| (name.clone(), record.get(i).unwrap_or("").to_string()))
            .collect()
    }
}

/// Parse a TSV into rows keyed by the header line.
///
/// Exported so the parsers can be tested against a captured fixture without a
/// network round trip, which is the only way this stays covered on a machine
/// that cannot reach netflix.com.
#[must_use]
pub fn parse_tsv(tsv: &str) -> Vec<Row> {
    let mut reader = Tsv::new(tsv);
    let mut record = StringRecord::new();
    let mut rows = Vec::new();
    while reader.read(&mut record) {
        rows.push(reader.row(&record));
    }
    rows
}

/// The most recent `week` in the file, or `None` when it has no data rows.
///
/// Its own pass, deliberately: the max has to be known before a row can be
/// judged, and reading it off borrowed fields means the 31 MB body is read
/// twice but allocated once.
fn latest_week(tsv: &str) -> Option<String> {
    let mut reader = Tsv::new(tsv);
    let week_at = reader.index("week");
    let mut record = StringRecord::new();
    let mut latest: Option<String> = None;

    while reader.read(&mut record) {
        let week = week_at.and_then(|i| record.get(i)).unwrap_or("");
        match &latest {
            Some(best) if week <= best.as_str() => {}
            _ => latest = Some(week.to_string()),
        }
    }

    latest
}

struct GlobalSnapshot {
    week: String,
    rows: Vec<Row>,
}

async fn global_snapshot(state: &AppState) -> Result<Arc<GlobalSnapshot>> {
    let url = global_url(state);
    state
        .cache()
        .cached("netflix:global", ttl::NETFLIX, || async {
            let tsv = state
                .http()
                .fetch_text(
                    &url,
                    FetchOptions::new()
                        .timeout(Duration::from_secs(60))
                        .retries(2)
                        .max_bytes(GLOBAL_MAX_BYTES),
                )
                .await?;
            let week = latest_week(&tsv).ok_or_else(|| {
                UpstreamError::parse_failed("Netflix returned an empty Top 10 dataset")
            })?;

            let mut reader = Tsv::new(&tsv);
            let week_at = reader.index("week");
            let mut record = StringRecord::new();
            let mut rows = Vec::new();
            while reader.read(&mut record) {
                if week_at.and_then(|i| record.get(i)).unwrap_or("") == week {
                    rows.push(reader.row(&record));
                }
            }

            Ok(GlobalSnapshot { week, rows })
        })
        .await
}

struct CountrySnapshot {
    week: String,
    /// ISO alpha-2 (lowercase) → that country's latest-week rows.
    by_country: HashMap<String, Vec<Row>>,
    names: HashMap<String, String>,
}

/// Latest week for every country, from one download.
///
/// The reduction happens before anything is cached, so the 31 MB body is
/// transient and what survives is ~1,900 rows.
async fn country_snapshot(state: &AppState) -> Result<Arc<CountrySnapshot>> {
    let url = countries_url(state);
    state
        .cache()
        .cached("netflix:countries", ttl::NETFLIX, || async {
            let tsv = state
                .http()
                .fetch_text(
                    &url,
                    FetchOptions::new()
                        .timeout(Duration::from_secs(120))
                        .retries(1)
                        .max_bytes(COUNTRIES_MAX_BYTES),
                )
                .await?;
            let week = latest_week(&tsv).ok_or_else(|| {
                UpstreamError::parse_failed("Netflix returned an empty country dataset")
            })?;

            Ok(reduce_countries(&tsv, week))
        })
        .await
}

fn reduce_countries(tsv: &str, week: String) -> CountrySnapshot {
    let mut reader = Tsv::new(tsv);
    let week_at = reader.index("week");
    let code_at = reader.index("country_iso2");
    let name_at = reader.index("country_name");

    let mut record = StringRecord::new();
    let mut by_country: HashMap<String, Vec<Row>> = HashMap::new();
    let mut names: HashMap<String, String> = HashMap::new();

    while reader.read(&mut record) {
        if week_at.and_then(|i| record.get(i)).unwrap_or("") != week {
            continue;
        }
        let code = code_at
            .and_then(|i| record.get(i))
            .unwrap_or("")
            .to_lowercase();
        if code.is_empty() {
            continue;
        }
        let name = name_at.map_or_else(
            || code.to_uppercase(),
            |i| record.get(i).unwrap_or("").to_string(),
        );
        names.insert(code.clone(), name);
        by_country
            .entry(code)
            .or_default()
            .push(reader.row(&record));
    }

    CountrySnapshot {
        week,
        by_country,
        names,
    }
}

/// Netflix repeats the show name inside `season_title`
/// (`"Wednesday: Season 2"`, and sometimes the bare title again). Showing both
/// columns verbatim prints the name twice in every TV row, so the redundant
/// prefix is trimmed down to just the part that says which season.
fn season_label(show_title: &str, season_title: Option<&str>) -> String {
    let season = text_or_empty(season_title);
    if season.is_empty() || show_title.is_empty() {
        return season;
    }
    if season == show_title {
        return String::new();
    }
    let prefix = format!("{show_title}: ");
    match season.strip_prefix(&prefix) {
        Some(rest) => rest.to_string(),
        None => season,
    }
}

fn to_entry(row: &Row) -> NetflixEntry {
    let title = field(row, "show_title").unwrap_or("").to_string();
    NetflixEntry {
        rank: count_or_null(field(row, "weekly_rank")).unwrap_or(0),
        season: season_label(&title, field(row, "season_title")),
        title,
        views: num_or_null(field(row, "weekly_views")),
        hours_viewed: num_or_null(field(row, "weekly_hours_viewed")),
        runtime: num_or_null(field(row, "runtime")),
        weeks_in_top10: count_or_null(field(row, "cumulative_weeks_in_top_10")),
    }
}

/// The global file splits by language (`TV (English)`, `Films (Non-English)`),
/// which is a distinction the terminal does not need: a Top 10 is a Top 10. Rows
/// are matched on the leading word and re-ranked across both language lists.
fn matches_global_category(category: &str, want: NetflixCategory) -> bool {
    let head = category.trim().to_lowercase();
    match want {
        NetflixCategory::Tv => head.starts_with("tv"),
        NetflixCategory::Films => head.starts_with("film"),
    }
}

pub async fn get_top10(
    state: &AppState,
    category: NetflixCategory,
    scope: &str,
) -> Result<NetflixTop10> {
    let category_label = category.label();

    if scope == "global" {
        let snapshot = global_snapshot(state).await?;
        let mut entries: Vec<NetflixEntry> = snapshot
            .rows
            .iter()
            .filter(|row| matches_global_category(field(row, "category").unwrap_or(""), category))
            .map(to_entry)
            .collect();

        // Both language lists rank 1..10 independently; ordering by views and
        // re-ranking produces the single combined Top 10 people expect.
        entries.sort_by(|a, b| {
            b.views
                .unwrap_or(0.0)
                .partial_cmp(&a.views.unwrap_or(0.0))
                .unwrap_or(Ordering::Equal)
        });
        entries.truncate(10);
        for (i, entry) in entries.iter_mut().enumerate() {
            entry.rank = u32::try_from(i + 1).unwrap_or(u32::MAX);
        }

        if entries.is_empty() {
            return Err(UpstreamError::parse_failed(format!(
                "Netflix published no global {category_label} rows for {}",
                snapshot.week
            )));
        }

        return Ok(NetflixTop10 {
            scope: "global".to_string(),
            scope_label: "Global".to_string(),
            category: category.as_str().to_string(),
            category_label: category_label.to_string(),
            week: snapshot.week.clone(),
            entries,
            source_url: global_url(state),
        });
    }

    let snapshot = country_snapshot(state).await?;
    let Some(rows) = snapshot.by_country.get(scope) else {
        return Err(UpstreamError::not_found(format!(
            "Netflix does not publish a Top 10 for \"{}\"",
            scope.to_uppercase()
        ))
        .with_hint(
            "Netflix reports about 93 countries. Try `NFLX tv us` or `NFLX films global`.",
        ));
    };

    // The country file spells the categories plainly as `TV` and `Films`.
    let want = category.as_str();
    let mut entries: Vec<NetflixEntry> = rows
        .iter()
        .filter(|row| field(row, "category").unwrap_or("").trim().to_lowercase() == want)
        .map(to_entry)
        .collect();
    entries.sort_by(|a, b| a.rank.cmp(&b.rank));
    entries.truncate(10);

    if entries.is_empty() {
        return Err(UpstreamError::parse_failed(format!(
            "Netflix published no {category_label} rows for {} in {}",
            scope.to_uppercase(),
            snapshot.week
        )));
    }

    Ok(NetflixTop10 {
        scope: scope.to_string(),
        scope_label: snapshot
            .names
            .get(scope)
            .cloned()
            .unwrap_or_else(|| scope.to_uppercase()),
        category: category.as_str().to_string(),
        category_label: category_label.to_string(),
        week: snapshot.week.clone(),
        entries,
        source_url: countries_url(state),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::error::codes;
    use crate::http::Http;
    use wiremock::matchers::{method, path as path_matcher};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// The header line and two rows, exactly as the TypeScript test wrote them.
    const TSV: &str = concat!(
        "week\tcategory\tweekly_rank\tshow_title\tseason_title\tweekly_hours_viewed\truntime\tweekly_views\tcumulative_weeks_in_top_10\n",
        "2026-08-09\tFilms (English)\t1\tThe Last House\tN/A\t51400000\t1.8667\t27500000\t1\n",
        "2026-08-09\tTV (English)\t1\tWednesday\tWednesday: Season 2\t71000000\t0.9\t9000000\t3",
    );

    /// A trimmed `all-weeks-global.tsv`: two weeks, and inside the latest one
    /// both language lists ranking from 1 independently.
    const GLOBAL_TSV: &str = include_str!("fixtures/netflix_global.tsv");

    /// A trimmed `all-weeks-countries.tsv`: three weeks for each of three
    /// countries, which is what the reduction has to collapse.
    const COUNTRIES_TSV: &str = include_str!("fixtures/netflix_countries.tsv");

    fn cell<'a>(row: &'a Row, name: &str) -> &'a str {
        field(row, name).unwrap_or("<absent>")
    }

    fn state_for(server: &MockServer) -> AppState {
        AppState::with_http(
            Config {
                netflix_base: server.uri().trim_end_matches('/').to_string(),
                ..Config::default()
            },
            Http::new(),
        )
    }

    async fn serving(global: &str, countries: &str) -> (MockServer, AppState) {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/tudum/top10/data/all-weeks-global.tsv"))
            .respond_with(
                ResponseTemplate::new(200).set_body_raw(global, "text/tab-separated-values"),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path_matcher("/tudum/top10/data/all-weeks-countries.tsv"))
            .respond_with(
                ResponseTemplate::new(200).set_body_raw(countries, "text/tab-separated-values"),
            )
            .mount(&server)
            .await;
        let state = state_for(&server);
        (server, state)
    }

    fn titles(top10: &NetflixTop10) -> Vec<&str> {
        top10
            .entries
            .iter()
            .map(|entry| entry.title.as_str())
            .collect()
    }

    /* ------------------------------------------------------------ parseTsv */

    #[test]
    fn keys_every_row_by_the_header_line() {
        let rows = parse_tsv(TSV);
        assert_eq!(rows.len(), 2);
        assert_eq!(cell(&rows[0], "show_title"), "The Last House");
        assert_eq!(cell(&rows[0], "weekly_views"), "27500000");
        assert_eq!(cell(&rows[1], "category"), "TV (English)");
    }

    #[test]
    fn returns_nothing_for_an_empty_document_rather_than_throwing() {
        assert!(parse_tsv("").is_empty());
        assert!(parse_tsv("week\tcategory").is_empty());
    }

    #[test]
    fn ignores_blank_trailing_lines() {
        assert_eq!(parse_tsv(&format!("{TSV}\n\n")).len(), 2);
    }

    #[test]
    fn drops_a_line_that_is_blank_once_trimmed() {
        // A run of tabs is as blank as a run of spaces, and neither is a row of
        // empty cells to be charted.
        assert_eq!(parse_tsv(&format!("{TSV}\n\t\t\t\n   \n")).len(), 2);
    }

    #[test]
    fn pads_a_short_row_out_to_the_header() {
        // A row Netflix cut early must not shift every later column left by one.
        let rows = parse_tsv("week\tcategory\tshow_title\n2026-08-09\tTV");
        assert_eq!(cell(&rows[0], "category"), "TV");
        assert_eq!(cell(&rows[0], "show_title"), "");
    }

    #[test]
    fn keeps_a_double_quote_in_a_title_literal() {
        // A CSV reader with quoting on reads `"Weird"` as an opening quote and
        // swallows the rest of the file into one field.
        let rows = parse_tsv("week\tshow_title\n2026-08-09\t\"Weird\": The Al Yankovic Story");
        assert_eq!(rows.len(), 1);
        assert_eq!(
            cell(&rows[0], "show_title"),
            "\"Weird\": The Al Yankovic Story"
        );
    }

    /* --------------------------------------------------------- the readings */

    #[test]
    fn reads_a_figure_and_strips_the_thousands_separators() {
        assert_eq!(num_or_null(Some("27,500,000")), Some(27_500_000.0));
        assert_eq!(num_or_null(Some("1.8667")), Some(1.8667));
    }

    #[test]
    fn reads_an_unpublished_figure_as_null_and_never_as_zero() {
        // Netflix reports a figure it does not publish as `N/A`. Reading that as
        // 0 would print "0 views" for a title that has them.
        for raw in [Some("N/A"), Some(""), Some("   "), None] {
            assert_eq!(num_or_null(raw), None, "{raw:?}");
            assert_eq!(count_or_null(raw), None, "{raw:?}");
        }
    }

    #[test]
    fn a_row_that_does_report_zero_keeps_it() {
        assert_eq!(num_or_null(Some("0")), Some(0.0));
        assert_eq!(count_or_null(Some("0")), Some(0));
    }

    #[test]
    fn de_duplicates_the_show_name_out_of_the_season_label() {
        assert_eq!(
            season_label("Wednesday", Some("Wednesday: Season 1")),
            "Season 1"
        );
        // Sometimes the season column is just the title again.
        assert_eq!(season_label("Ziam", Some("Ziam")), "");
        assert_eq!(season_label("The Last House", Some("N/A")), "");
        // A label that does not start with the title is left alone.
        assert_eq!(
            season_label("Berlin", Some("Limited Series")),
            "Limited Series"
        );
    }

    #[test]
    fn matches_a_language_split_category_on_its_leading_word() {
        for raw in ["TV (English)", "TV (Non-English)", "tv"] {
            assert!(matches_global_category(raw, NetflixCategory::Tv), "{raw}");
            assert!(
                !matches_global_category(raw, NetflixCategory::Films),
                "{raw}"
            );
        }
        for raw in ["Films (English)", "Films (Non-English)", "film"] {
            assert!(
                matches_global_category(raw, NetflixCategory::Films),
                "{raw}"
            );
            assert!(!matches_global_category(raw, NetflixCategory::Tv), "{raw}");
        }
    }

    /* ------------------------------------------------------- the arguments */

    #[test]
    fn accepts_every_spelling_of_the_two_categories() {
        for raw in ["tv", "TV", " shows ", "show", "series"] {
            assert_eq!(assert_category(raw).unwrap(), NetflixCategory::Tv, "{raw}");
        }
        for raw in ["films", "Film", " movies ", "movie"] {
            assert_eq!(
                assert_category(raw).unwrap(),
                NetflixCategory::Films,
                "{raw}"
            );
        }
        let error = assert_category("documentaries").expect_err("a bad request");
        assert_eq!(error.code, codes::BAD_REQUEST);
        assert!(error.hint.is_some());
    }

    #[test]
    fn reads_an_empty_or_worldwide_scope_as_global() {
        for raw in ["", "  ", "global", "GLOBAL", "world"] {
            assert_eq!(assert_scope(raw).unwrap(), "global", "{raw}");
        }
    }

    #[test]
    fn lower_cases_a_country_scope_and_rejects_anything_longer() {
        assert_eq!(assert_scope(" GB ").unwrap(), "gb");
        for raw in ["usa", "u", "12"] {
            let error = assert_scope(raw).expect_err(raw);
            assert_eq!(error.code, codes::BAD_REQUEST, "{raw}");
            assert!(
                error.message.contains("is not a valid Netflix scope"),
                "{raw}"
            );
        }
    }

    /* ------------------------------------------------------------- global */

    #[tokio::test]
    async fn re_ranks_the_two_language_lists_into_one_top_ten() {
        // Both lists rank from 1, so concatenating them would report two #1s and
        // put a 12M-view title below a 9M-view one.
        let (_server, state) = serving(GLOBAL_TSV, COUNTRIES_TSV).await;
        let top10 = get_top10(&state, NetflixCategory::Tv, "global")
            .await
            .unwrap();

        assert_eq!(top10.entries.len(), 10);
        assert_eq!(
            titles(&top10)[..3],
            ["Ziam", "Wednesday", "The Hunting Wives"]
        );
        assert_eq!(
            top10.entries.iter().map(|e| e.rank).collect::<Vec<_>>(),
            (1..=10).collect::<Vec<u32>>()
        );
        assert_eq!(top10.entries[0].views, Some(12_000_000.0));
    }

    #[tokio::test]
    async fn reads_only_the_most_recent_week_of_the_global_file() {
        // The week before carries far bigger figures; if the filter went, it
        // would take the whole chart.
        let (_server, state) = serving(GLOBAL_TSV, COUNTRIES_TSV).await;
        let top10 = get_top10(&state, NetflixCategory::Tv, "global")
            .await
            .unwrap();
        assert_eq!(top10.week, "2026-08-09");
        assert!(!titles(&top10).contains(&"Stranger Things"));
    }

    #[tokio::test]
    async fn carries_the_views_hours_and_runtime_the_global_file_publishes() {
        let (_server, state) = serving(GLOBAL_TSV, COUNTRIES_TSV).await;
        let top10 = get_top10(&state, NetflixCategory::Films, "global")
            .await
            .unwrap();

        assert_eq!(top10.scope, "global");
        assert_eq!(top10.scope_label, "Global");
        assert_eq!(top10.category, "films");
        assert_eq!(top10.category_label, "Films");
        assert!(top10
            .source_url
            .ends_with("/tudum/top10/data/all-weeks-global.tsv"));

        let first = &top10.entries[0];
        assert_eq!(first.title, "The Last House");
        assert_eq!(first.views, Some(27_500_000.0));
        assert_eq!(first.hours_viewed, Some(51_400_000.0));
        assert_eq!(first.runtime, Some(1.8667));
        assert_eq!(first.weeks_in_top10, Some(1));
        // `N/A` is not a figure, and it is not zero either.
        let unpublished = top10
            .entries
            .iter()
            .find(|entry| entry.title == "Almost Family")
            .expect("the row with no views");
        assert_eq!(unpublished.views, None);
        assert_eq!(unpublished.runtime, None);
    }

    #[tokio::test]
    async fn trims_the_show_name_off_the_season_it_returns() {
        let (_server, state) = serving(GLOBAL_TSV, COUNTRIES_TSV).await;
        let top10 = get_top10(&state, NetflixCategory::Tv, "global")
            .await
            .unwrap();
        let wednesday = top10
            .entries
            .iter()
            .find(|entry| entry.title == "Wednesday")
            .expect("Wednesday");
        assert_eq!(wednesday.season, "Season 2");
        let ziam = &top10.entries[0];
        assert_eq!(ziam.title, "Ziam");
        assert_eq!(ziam.season, "");
    }

    #[tokio::test]
    async fn reports_a_week_with_no_rows_of_the_category_as_a_parse_failure() {
        let (_server, state) = serving(
            "week\tcategory\tweekly_rank\tshow_title\n2026-08-09\tTV (English)\t1\tWednesday\n",
            COUNTRIES_TSV,
        )
        .await;
        let error = get_top10(&state, NetflixCategory::Films, "global")
            .await
            .expect_err("no film rows");
        assert_eq!(error.code, codes::PARSE_FAILED);
        assert_eq!(
            error.message,
            "Netflix published no global Films rows for 2026-08-09"
        );
    }

    #[tokio::test]
    async fn reports_an_empty_dataset_rather_than_an_empty_chart() {
        let (_server, state) =
            serving("week\tcategory\tweekly_rank\tshow_title\n", COUNTRIES_TSV).await;
        let error = get_top10(&state, NetflixCategory::Tv, "global")
            .await
            .expect_err("an empty dataset");
        assert_eq!(error.code, codes::PARSE_FAILED);
        assert_eq!(error.message, "Netflix returned an empty Top 10 dataset");
    }

    /* ---------------------------------------------------------- countries */

    #[tokio::test]
    async fn reduces_the_country_file_to_the_latest_week_before_caching_it() {
        // The point of the whole module: the 31 MB body is transient, and what
        // reaches the cache is one week per country. The fixture carries three
        // weeks for each of three countries; only the last may survive.
        let (_server, state) = serving(GLOBAL_TSV, COUNTRIES_TSV).await;
        get_top10(&state, NetflixCategory::Tv, "us").await.unwrap();

        let snapshot = state
            .cache()
            .get::<CountrySnapshot>("netflix:countries")
            .await
            .expect("the reduction is cached");

        assert_eq!(snapshot.week, "2026-08-09");
        assert_eq!(snapshot.by_country.len(), 3);
        let kept: usize = snapshot.by_country.values().map(Vec::len).sum();
        assert_eq!(kept, 11, "only the latest week survives the reduction");
        assert!(snapshot
            .by_country
            .values()
            .flatten()
            .all(|row| field(row, "week") == Some("2026-08-09")));
        assert!(!snapshot
            .by_country
            .values()
            .flatten()
            .any(|row| field(row, "show_title") == Some("Last Week US Show")));
    }

    #[tokio::test]
    async fn serves_every_country_from_the_one_download() {
        let (server, state) = serving(GLOBAL_TSV, COUNTRIES_TSV).await;
        get_top10(&state, NetflixCategory::Tv, "us").await.unwrap();
        get_top10(&state, NetflixCategory::Tv, "gb").await.unwrap();
        get_top10(&state, NetflixCategory::Films, "br")
            .await
            .unwrap();

        let downloads = server
            .received_requests()
            .await
            .expect("the request log")
            .iter()
            .filter(|request| request.url.path().ends_with("all-weeks-countries.tsv"))
            .count();
        assert_eq!(downloads, 1);
    }

    #[tokio::test]
    async fn orders_a_country_chart_by_the_rank_netflix_published() {
        let (_server, state) = serving(GLOBAL_TSV, COUNTRIES_TSV).await;
        let top10 = get_top10(&state, NetflixCategory::Tv, "us").await.unwrap();

        assert_eq!(
            titles(&top10),
            ["Wednesday", "The Hunting Wives", "Love Is Blind"]
        );
        assert_eq!(
            top10.entries.iter().map(|e| e.rank).collect::<Vec<_>>(),
            [1, 2, 3]
        );
        assert_eq!(top10.scope, "us");
        assert_eq!(top10.scope_label, "United States");
        assert_eq!(top10.week, "2026-08-09");
        assert!(top10
            .source_url
            .ends_with("/tudum/top10/data/all-weeks-countries.tsv"));
    }

    #[tokio::test]
    async fn a_country_chart_publishes_ranks_but_no_views() {
        let (_server, state) = serving(GLOBAL_TSV, COUNTRIES_TSV).await;
        let top10 = get_top10(&state, NetflixCategory::Films, "gb")
            .await
            .unwrap();
        let first = &top10.entries[0];
        assert_eq!(first.title, "The Last House");
        assert_eq!(first.views, None);
        assert_eq!(first.hours_viewed, None);
        assert_eq!(first.weeks_in_top10, Some(1));
    }

    #[tokio::test]
    async fn reports_a_country_netflix_does_not_publish_as_not_found() {
        let (_server, state) = serving(GLOBAL_TSV, COUNTRIES_TSV).await;
        let error = get_top10(&state, NetflixCategory::Tv, "de")
            .await
            .expect_err("no such country");
        assert_eq!(error.code, codes::NOT_FOUND);
        assert_eq!(
            error.message,
            "Netflix does not publish a Top 10 for \"DE\""
        );
        assert!(error.hint.is_some());
    }

    #[tokio::test]
    async fn reports_a_country_with_no_rows_of_the_category_as_a_parse_failure() {
        // A country Netflix does publish, but not in this category — distinct
        // from a country it does not report at all, which is a `not_found`.
        let (_server, state) = serving(
            GLOBAL_TSV,
            "country_name\tcountry_iso2\tweek\tcategory\tweekly_rank\tshow_title\nFrance\tFR\t2026-08-09\tTV\t1\tLupin\n",
        )
        .await;
        let error = get_top10(&state, NetflixCategory::Films, "fr")
            .await
            .expect_err("no film rows");
        assert_eq!(error.code, codes::PARSE_FAILED);
        assert_eq!(
            error.message,
            "Netflix published no Films rows for FR in 2026-08-09"
        );
    }

    #[tokio::test]
    async fn reports_an_empty_country_dataset() {
        let (_server, state) = serving(
            GLOBAL_TSV,
            "country_name\tcountry_iso2\tweek\tcategory\tweekly_rank\tshow_title\n",
        )
        .await;
        let error = get_top10(&state, NetflixCategory::Tv, "us")
            .await
            .expect_err("an empty dataset");
        assert_eq!(error.code, codes::PARSE_FAILED);
        assert_eq!(error.message, "Netflix returned an empty country dataset");
    }

    /* ---------------------------------------------------------- the cache */

    #[tokio::test]
    async fn reads_both_files_from_the_cache_keys_the_typescript_used() {
        // No mocks at all: if either key were spelled differently the fetch
        // would go to the network and fail.
        let server = MockServer::start().await;
        let state = state_for(&server);
        state
            .cache()
            .cached("netflix:global", ttl::NETFLIX, || async {
                Ok(GlobalSnapshot {
                    week: "2026-08-09".to_string(),
                    rows: parse_tsv(TSV),
                })
            })
            .await
            .unwrap();
        state
            .cache()
            .cached("netflix:countries", ttl::NETFLIX, || async {
                Ok(reduce_countries(COUNTRIES_TSV, "2026-08-09".to_string()))
            })
            .await
            .unwrap();

        let global = get_top10(&state, NetflixCategory::Tv, "global")
            .await
            .unwrap();
        assert_eq!(titles(&global), ["Wednesday"]);
        let country = get_top10(&state, NetflixCategory::Tv, "gb").await.unwrap();
        assert_eq!(country.scope_label, "United Kingdom");
    }
}
