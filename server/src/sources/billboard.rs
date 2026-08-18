//! Billboard charts — scraped from billboard.com/charts/<slug>[/<date>].
//!
//! Billboard's markup is Tailwind-ish utility soup, but the handful of
//! *semantic* class names carry structure and have been stable for years:
//!
//! ```text
//!   .o-chart-results-list-row-container   one entry (100 per page)
//!     li[0] > .c-label                    rank
//!     img.c-lazy-image__img               artwork
//!     h3.c-title                          title
//!     h3.c-title + .c-label               artist
//!     .c-span (LW | PEAK | WEEKS)         stat labels, value in the next .c-label
//! ```
//!
//! The parser anchors on those and reads stats by their *label text* rather than
//! by position, so Billboard reordering or adding a column does not silently
//! shift "peak" into "weeks on chart".

use std::collections::HashMap;
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use regex::Regex;
use reqwest::Url;
use scraper::ElementRef;
use terminal_core::types::{BillboardChart, BillboardChartListItem, BillboardEntry};

use crate::app::AppState;
use crate::cache::ttl;
use crate::css;
use crate::error::{Result, UpstreamError};
use crate::http::FetchOptions;
use crate::scrape;

/// Charts worth putting one keystroke away. `BB CHARTS` lists these.
pub const KNOWN_CHARTS: [(&str, &str); 15] = [
    ("hot-100", "Billboard Hot 100"),
    ("billboard-200", "Billboard 200 (albums)"),
    ("artist-100", "Billboard Artist 100"),
    ("streaming-songs", "Streaming Songs"),
    ("radio-songs", "Radio Songs"),
    ("digital-song-sales", "Digital Song Sales"),
    ("billboard-global-200", "Billboard Global 200"),
    ("billboard-global-excl-us", "Billboard Global Excl. US"),
    ("country-songs", "Hot Country Songs"),
    ("rock-songs", "Hot Rock Songs"),
    ("r-b-hip-hop-songs", "Hot R&B/Hip-Hop Songs"),
    ("latin-songs", "Hot Latin Songs"),
    ("dance-electronic-songs", "Hot Dance/Electronic Songs"),
    ("pop-songs", "Pop Airplay"),
    ("tiktok-billboard-top-50", "TikTok Billboard Top 50"),
];

/// [`KNOWN_CHARTS`] in the shape the wire wants.
#[must_use]
pub fn known_charts() -> Vec<BillboardChartListItem> {
    KNOWN_CHARTS
        .iter()
        .map(|(slug, name)| BillboardChartListItem {
            slug: (*slug).to_owned(),
            name: (*name).to_owned(),
        })
        .collect()
}

static SLUG: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-z0-9][a-z0-9-]{0,60}$").expect("valid regex"));
static DATE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\d{4}-\d{2}-\d{2}$").expect("valid regex"));
static BILLBOARD_SUFFIX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\s*\|\s*Billboard.*$").expect("valid regex"));
static CHART_SUFFIX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\s+Chart\s*$").expect("valid regex"));

pub fn assert_slug(slug: &str) -> Result<String> {
    let s = slug.trim().to_lowercase();
    if !SLUG.is_match(&s) {
        return Err(UpstreamError::bad_request(format!(
            "\"{slug}\" is not a valid Billboard chart slug"
        ))
        .with_hint("Run `BB CHARTS` to list the charts this terminal knows about."));
    }
    Ok(s)
}

pub fn assert_date(date: &str) -> Result<String> {
    let d = date.trim();
    if !DATE.is_match(d) || !is_real_calendar_date(d) {
        return Err(
            UpstreamError::bad_request(format!("\"{date}\" is not a valid chart date"))
                .with_hint("Chart dates are YYYY-MM-DD, e.g. `BB hot-100 2025-06-14`."),
        );
    }
    Ok(d.to_owned())
}

/// The `Date.parse` half of [`assert_date`].
///
/// A date-only ISO string is validated strictly by `Date.parse`, so `2025-13-45`
/// is `NaN` rather than a rolled-over date. The shape regex alone would accept
/// it, which is why the original ANDs the two checks together.
fn is_real_calendar_date(text: &str) -> bool {
    // The regex has already proved this is ten ASCII bytes in `dddd-dd-dd`.
    let (Ok(year), Ok(month), Ok(day)) = (
        text[0..4].parse::<i32>(),
        text[5..7].parse::<u8>(),
        text[8..10].parse::<u8>(),
    ) else {
        return false;
    };
    let Ok(month) = time::Month::try_from(month) else {
        return false;
    };
    time::Date::from_calendar_date(year, month, day).is_ok()
}

/// `Number.parseInt(text, 10)`: a leading integer, whatever follows it ignored.
///
/// `"42 wks"` is 42 and `"NEW"` is nothing, which is the behaviour the callers
/// below are written against.
fn parse_int(text: &str) -> Option<i64> {
    let bytes = text.as_bytes();
    let mut i = 0;
    let negative = match bytes.first() {
        Some(b'-') => {
            i = 1;
            true
        }
        Some(b'+') => {
            i = 1;
            false
        }
        _ => false,
    };

    let start = i;
    let mut value: i64 = 0;
    while let Some(digit) = bytes.get(i).filter(|b| b.is_ascii_digit()) {
        value = value
            .saturating_mul(10)
            .saturating_add(i64::from(digit - b'0'));
        i += 1;
    }

    if i == start {
        return None;
    }
    Some(if negative { -value } else { value })
}

/// `"3"` → 3; `"-"`, `""`, `"NEW"` → null.
fn int_or_null(text: &str) -> Option<i64> {
    let cleaned = scrape::collapse_ws(text).replace(',', "");
    if cleaned.is_empty() || cleaned == "-" || cleaned == "–" || cleaned == "—" {
        return None;
    }
    parse_int(&cleaned)
}

/// [`int_or_null`] narrowed to the unsigned counts the wire declares.
///
/// Ranks, peaks and week tallies are all `u32` there; anything that parsed to a
/// negative or absurd figure is not one of them and reads as absent.
fn count_or_null(text: &str) -> Option<u32> {
    int_or_null(text).and_then(|n| u32::try_from(n).ok())
}

/// `lastWeek - rank`, in the signed type the wire declares.
///
/// Both operands are chart positions on a hundred-row page, so the subtraction
/// is nowhere near the edges of `i32`; the saturation is only there so a
/// nonsense figure scraped off a broken page cannot panic.
fn movement(last_week: u32, rank: u32) -> i32 {
    let delta = i64::from(last_week) - i64::from(rank);
    i32::try_from(delta).unwrap_or(if delta.is_negative() {
        i32::MIN
    } else {
        i32::MAX
    })
}

/// [`scrape::text_of`] plus the one escape Billboard emits twice.
///
/// A title that reached the page as `&amp;#39;` survives the parser's own decode
/// as a literal `&#39;`, and apostrophes are in roughly every other song title.
fn text(el: ElementRef<'_>) -> String {
    scrape::text_of(el)
        .replace("&#039;", "'")
        .replace("&#39;", "'")
}

/// The collapsed text of an optional element; an absent element reads as `""`,
/// the way cheerio answers `.text()` on an empty set.
fn text_or_empty(el: Option<ElementRef<'_>>) -> String {
    el.map(text).unwrap_or_default()
}

pub fn parse_chart_page(html: &str, chart: &str, source_url: &str) -> Result<BillboardChart> {
    let doc = scrape::parse_document(html);

    // ---- chart-level metadata ---------------------------------------------
    let date = scrape::attr_of_first(&doc, css!(".chart-date-picker"), "data-date")
        .or_else(|| scrape::attr_of_first(&doc, css!("[data-date]"), "data-date"))
        .unwrap_or_default()
        .to_owned();

    let raw_title =
        match scrape::attr_of_first(&doc, css!(r#"meta[property="og:title"]"#), "content") {
            Some(content) => content.to_owned(),
            // `??` only falls through on `undefined`, and cheerio answers
            // `.text()` on an empty set with `""` — so the `<title>` and `chart`
            // arms that follow this one in the original can never run. What
            // actually supplies the fallback is `title || chart` at the end.
            None => scrape::select_one(&doc, css!("h1"))
                .map(scrape::raw_text_of)
                .unwrap_or_default(),
        };
    let title = BILLBOARD_SUFFIX.replace(&raw_title, "");
    let title = CHART_SUFFIX.replace(&title, "");
    let title = scrape::collapse_ws(&title);

    // ---- entries -----------------------------------------------------------
    let mut entries: Vec<BillboardEntry> = Vec::new();

    for row in scrape::select_all(&doc, css!(".o-chart-results-list-row-container")) {
        // Rank lives in the first .c-label of the row — the oversized number in
        // the leading cell. Falling back to ordinal position keeps a row from
        // being dropped entirely if that cell is ever restyled.
        let rank = scrape::select_one(row, css!(".c-label"))
            .and_then(|el| count_or_null(&text(el)))
            .unwrap_or_else(|| u32::try_from(entries.len() + 1).unwrap_or(u32::MAX));

        let title_el = scrape::select_one(row, css!("h3.c-title, .c-title"));
        let song_title = text_or_empty(title_el);

        // The artist sits in the .c-label directly after the title heading. It
        // is a link on most rows and bare text on some (unlinked artists).
        let mut artist = text_or_empty(
            title_el.and_then(|el| scrape::next_sibling_matching(el, css!(".c-label, span"))),
        );
        if artist.is_empty() {
            artist = text_or_empty(
                title_el
                    .and_then(scrape::parent_element)
                    .and_then(|parent| scrape::select_one(parent, css!(".c-label"))),
            );
        }
        if artist.is_empty() {
            artist = text_or_empty(
                title_el
                    .and_then(scrape::parent_element)
                    .and_then(|parent| scrape::select_one(parent, css!(r#"a[href*="/artist/"]"#))),
            );
        }

        // Some chart types (Artist 100) have no separate title/artist split: the
        // heading *is* the artist.
        if song_title.is_empty() && artist.is_empty() {
            continue;
        }

        // ---- stats, keyed by their printed label ----------------------------
        let mut stats: HashMap<String, Option<u32>> = HashMap::new();
        for span in scrape::select_all(row, css!(".c-span")) {
            let label: String = text(span)
                .to_uppercase()
                .chars()
                .filter(char::is_ascii_uppercase)
                .collect();
            if label.is_empty() {
                continue;
            }

            // The value is the next .c-label in document order within this row.
            let value = scrape::parent_element(span)
                .and_then(|scope| scrape::select_one(scope, css!(".c-label")))
                .and_then(|el| count_or_null(&text(el)));
            stats.entry(label).or_insert(value);
        }

        let last_week = stats.get("LW").copied().flatten();
        let peak = stats.get("PEAK").copied().flatten();
        let weeks_on_chart = stats
            .get("WEEKS")
            .copied()
            .flatten()
            .or_else(|| stats.get("WKS").copied().flatten());

        // Rows below the fold are lazy-loaded: `src` is a placeholder GIF and
        // the real artwork is parked in `data-lazy-src`. Prefer the latter.
        let img = scrape::select_one(row, css!("img.c-lazy-image__img"))
            .or_else(|| scrape::select_one(row, css!("img")));
        let image_url = img
            .and_then(|el| scrape::attr(el, "data-lazy-src").or_else(|| scrape::attr(el, "src")));

        // On artist charts (Artist 100) the heading and the label below it are
        // both the artist name; collapse the duplicate rather than printing it
        // twice.
        let is_artist_only =
            song_title.is_empty() || song_title.to_lowercase() == artist.to_lowercase();

        entries.push(BillboardEntry {
            rank,
            title: if song_title.is_empty() {
                artist.clone()
            } else {
                song_title
            },
            artist: if is_artist_only {
                String::new()
            } else {
                artist
            },
            last_week,
            peak,
            weeks_on_chart,
            image_url: image_url
                .filter(|url| url.starts_with("http"))
                .map(str::to_owned),
            movement: last_week.map(|last_week| movement(last_week, rank)),
            is_new: last_week.is_none(),
        });
    }

    if entries.is_empty() {
        return Err(
            UpstreamError::parse_failed(format!("No chart entries found on {source_url}"))
                .with_hint(
                    "Billboard returned a page but its chart rows did not match the expected \
                 markup. The chart slug may be wrong, or the page layout changed.",
                ),
        );
    }

    entries.sort_by_key(|entry| entry.rank);

    Ok(BillboardChart {
        chart: chart.to_owned(),
        title: if title.is_empty() {
            chart.to_owned()
        } else {
            title
        },
        date,
        entries,
        source_url: source_url.to_owned(),
    })
}

pub async fn get_chart(
    state: &AppState,
    raw_chart: &str,
    raw_date: Option<&str>,
) -> Result<Arc<BillboardChart>> {
    let chart = assert_slug(raw_chart)?;
    let date = raw_date.map(assert_date).transpose()?;

    let base = &state.config().billboard_base;
    let source_url = match &date {
        Some(date) => format!("{base}/charts/{chart}/{date}/"),
        None => format!("{base}/charts/{chart}/"),
    };
    let key = format!("billboard:{chart}:{}", date.as_deref().unwrap_or("latest"));

    state
        .cache()
        .cached(&key, ttl::BILLBOARD, || async {
            let html = state
                .http()
                .fetch_text(
                    &source_url,
                    FetchOptions::new()
                        .timeout(Duration::from_secs(40))
                        .retries(2)
                        .max_bytes(24 * 1024 * 1024),
                )
                .await?;
            parse_chart_page(&html, &chart, &source_url)
        })
        .await
}

/* --------------------------------------------------------- artwork proxy */

/// Hosts the artwork proxy will fetch from.
///
/// Exact matches only. A suffix check would accept
/// `charts-static.billboard.com.evil.test`, turning that route into an open SSRF
/// relay — the whole point of the allowlist is that a caller cannot choose the
/// destination.
const ART_HOSTS: [&str; 3] = [
    "charts-static.billboard.com",
    "www.billboard.com",
    "billboard.com",
];

/// The most artwork the proxy will hold. Chart thumbnails are tens of kilobytes.
pub const MAX_ART_BYTES: usize = 3 * 1024 * 1024;

/// Validate an artwork URL before the server will fetch it.
///
/// Exported so the allowlist is covered by tests: this function is the only
/// thing standing between a query parameter and an outbound request, and the
/// failure mode (SSRF against link-local metadata, or localhost) is severe
/// enough to be worth asserting rather than assuming.
pub fn assert_art_url(raw: &str) -> Result<Url> {
    let target = Url::parse(raw)
        .map_err(|_| UpstreamError::bad_request("Artwork URL is not a valid URL"))?;

    // `file:` and the other host-less schemes report no host at all; naming the
    // empty string keeps the message the same shape as every other refusal.
    let host = target.host_str().unwrap_or_default();
    if target.scheme() != "https" || !ART_HOSTS.contains(&host) {
        return Err(
            UpstreamError::bad_request(format!("Refusing to fetch artwork from {host}"))
                .with_hint("Only Billboard chart artwork can be proxied."),
        );
    }
    Ok(target)
}

#[cfg(test)]
mod tests {
    //! Billboard parser tests.
    //!
    //! The fixture mirrors the real billboard.com row structure — the semantic
    //! class names, the `.c-span` stat labels, and the lazy-loaded artwork —
    //! trimmed down from a live capture of the Hot 100.

    use super::*;

    use crate::config::Config;
    use crate::error::codes;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[derive(Default)]
    struct Row<'a> {
        rank: u32,
        title: &'a str,
        artist: Option<&'a str>,
        lw: Option<&'a str>,
        peak: &'a str,
        weeks: &'a str,
        img: Option<&'a str>,
        lazy_img: Option<&'a str>,
    }

    fn row(opts: &Row<'_>) -> String {
        let artist_block = match opts.artist {
            Some(artist) => {
                format!(r#"<span class="c-label"><a href="/artist/x/">{artist}</a></span>"#)
            }
            None => String::new(),
        };
        let img = match opts.lazy_img {
            Some(lazy) => format!(
                r#"<img class="c-lazy-image__img" src="{}" data-lazy-src="{lazy}">"#,
                opts.img.unwrap_or("placeholder.gif")
            ),
            None => format!(
                r#"<img class="c-lazy-image__img" src="{}">"#,
                opts.img.unwrap_or("")
            ),
        };

        format!(
            r#"
  <div class="o-chart-results-list-row-container">
    <ul class="o-chart-results-list-row">
      <li class="o-chart-results-list__item"><span class="c-label">{rank}</span></li>
      <li class="o-chart-results-list__item">{img}</li>
      <li class="lrv-u-width-100p">
        <ul>
          <li class="o-chart-results-list__item">
            <h3 id="title-of-a-story" class="c-title">{title}</h3>
            {artist_block}
          </li>
          <div>
            <span class="c-span">LW</span>
            <li class="o-chart-results-list__item"><span class="c-label">{lw}</span></li>
          </div>
          <div>
            <span class="c-span">PEAK</span>
            <li class="o-chart-results-list__item"><span class="c-label">{peak}</span></li>
          </div>
          <div>
            <span class="c-span">WEEKS</span>
            <li class="o-chart-results-list__item"><span class="c-label">{weeks}</span></li>
          </div>
        </ul>
      </li>
    </ul>
  </div>"#,
            rank = opts.rank,
            title = opts.title,
            lw = opts.lw.unwrap_or("-"),
            peak = opts.peak,
            weeks = opts.weeks,
        )
    }

    fn page() -> String {
        format!(
            r#"<!doctype html><html><head>
<meta property="og:title" content="Hot 100™ | Billboard">
</head><body>
<div class="chart-date-picker" data-date="2026-08-15" data-chart-code="HSI"></div>
<div class="chart-results-list">
  {}
  {}
  {}
</div>
</body></html>"#,
            row(&Row {
                rank: 1,
                title: "Choosin' Texas",
                artist: Some("Ella Langley"),
                lw: Some("1"),
                peak: "1",
                weeks: "42",
                img: Some("https://charts-static.billboard.com/a.jpg"),
                lazy_img: None,
            }),
            row(&Row {
                rank: 3,
                title: "Been By Now",
                artist: Some("Morgan Wallen"),
                lw: Some("2"),
                peak: "2",
                weeks: "2",
                img: Some("https://charts-static.billboard.com/c.jpg"),
                lazy_img: None,
            }),
            row(&Row {
                rank: 2,
                title: "Petal",
                artist: Some("Ariana Grande"),
                lw: None,
                peak: "4",
                weeks: "1",
                img: Some("placeholder.gif"),
                lazy_img: Some("https://charts-static.billboard.com/b.jpg"),
            }),
        )
    }

    fn parsed() -> BillboardChart {
        parse_chart_page(&page(), "hot-100", "u").expect("the fixture parses")
    }

    fn entry<'a>(chart: &'a BillboardChart, title: &str) -> &'a BillboardEntry {
        chart
            .entries
            .iter()
            .find(|entry| entry.title == title)
            .unwrap_or_else(|| panic!("no entry titled {title}"))
    }

    /* -------------------------------------------------- parse_chart_page */

    #[test]
    fn reads_the_chart_week_from_the_date_picker() {
        assert_eq!(parsed().date, "2026-08-15");
    }

    #[test]
    fn strips_the_billboard_suffix_from_the_chart_title() {
        assert_eq!(parsed().title, "Hot 100™");
    }

    #[test]
    fn extracts_rank_title_and_artist_for_every_row() {
        let chart = parsed();
        assert_eq!(chart.entries.len(), 3);
        assert_eq!(chart.entries[0].title, "Choosin' Texas");
        assert_eq!(chart.entries[0].artist, "Ella Langley");
    }

    #[test]
    fn returns_entries_sorted_by_rank_regardless_of_document_order() {
        let chart = parsed();
        let ranks: Vec<u32> = chart.entries.iter().map(|entry| entry.rank).collect();
        assert_eq!(ranks, vec![1, 2, 3]);
    }

    #[test]
    fn keys_stats_by_their_printed_label_not_by_position() {
        let chart = parsed();
        let wallen = entry(&chart, "Been By Now");
        assert_eq!(wallen.last_week, Some(2));
        assert_eq!(wallen.peak, Some(2));
        assert_eq!(wallen.weeks_on_chart, Some(2));
    }

    #[test]
    fn treats_a_missing_last_week_value_as_a_new_entry() {
        let chart = parsed();
        let petal = entry(&chart, "Petal");
        assert_eq!(petal.last_week, None);
        assert!(petal.is_new);
        assert_eq!(petal.movement, None);
    }

    #[test]
    fn computes_movement_as_rank_improvement() {
        let chart = parsed();
        // Ranked 3 this week after 2 last week: down one.
        assert_eq!(entry(&chart, "Been By Now").movement, Some(-1));
        assert_eq!(entry(&chart, "Choosin' Texas").movement, Some(0));
    }

    #[test]
    fn prefers_data_lazy_src_over_the_placeholder_image() {
        let chart = parsed();
        assert_eq!(
            entry(&chart, "Petal").image_url.as_deref(),
            Some("https://charts-static.billboard.com/b.jpg")
        );
    }

    #[test]
    fn collapses_the_duplicated_name_on_artist_charts() {
        let artist_page = format!(
            r#"<html><body><div class="chart-date-picker" data-date="2026-08-15"></div>
      {}
    </body></html>"#,
            row(&Row {
                rank: 1,
                title: "Ariana Grande",
                artist: Some("Ariana Grande"),
                lw: Some("12"),
                peak: "1",
                weeks: "555",
                img: None,
                lazy_img: None,
            })
        );

        let chart = parse_chart_page(&artist_page, "artist-100", "u").expect("the page parses");
        assert_eq!(chart.entries[0].title, "Ariana Grande");
        assert_eq!(chart.entries[0].artist, "");
    }

    #[test]
    fn throws_a_diagnosable_error_when_no_rows_match() {
        let err = parse_chart_page(
            "<html><body><p>nothing here</p></body></html>",
            "hot-100",
            "https://x/",
        )
        .expect_err("a page with no rows is a parse failure");

        assert_eq!(err.code, codes::PARSE_FAILED);
        assert!(err.message.contains("No chart entries found"));
    }

    /* ------------------------------------------------- input validation */

    #[test]
    fn accepts_real_chart_slugs_and_normalises_case() {
        assert_eq!(assert_slug("Hot-100").unwrap(), "hot-100");
        assert_eq!(
            assert_slug("billboard-global-excl-us").unwrap(),
            "billboard-global-excl-us"
        );
    }

    #[test]
    fn rejects_slugs_that_could_escape_the_chart_path() {
        for bad in ["../admin", "hot 100", "hot-100?x=1", "", "/etc/passwd"] {
            let err = assert_slug(bad).expect_err(bad);
            assert_eq!(err.code, codes::BAD_REQUEST, "{bad}");
            assert!(
                err.message.contains("not a valid Billboard chart slug"),
                "{bad}: {}",
                err.message
            );
        }
    }

    #[test]
    fn accepts_iso_chart_dates_and_rejects_anything_else() {
        assert_eq!(assert_date("2025-06-14").unwrap(), "2025-06-14");
        for bad in ["06-14-2025", "2025-13-45", "latest", ""] {
            let err = assert_date(bad).expect_err(bad);
            assert_eq!(err.code, codes::BAD_REQUEST, "{bad}");
            assert!(
                err.message.contains("not a valid chart date"),
                "{bad}: {}",
                err.message
            );
        }
    }

    #[test]
    fn lists_every_chart_worth_one_keystroke() {
        let charts = known_charts();
        assert_eq!(charts.len(), 15);
        assert_eq!(charts[0].slug, "hot-100");
        // Every listed slug has to survive the validator the route runs.
        for chart in &charts {
            assert_eq!(assert_slug(&chart.slug).unwrap(), chart.slug);
        }
    }

    /* --------------------------------------------- artwork proxy allowlist */

    #[test]
    fn accepts_billboard_artwork_over_https() {
        let url = assert_art_url("https://charts-static.billboard.com/img/2025/11/a-180x180.jpg")
            .expect("real Billboard artwork is allowed");
        assert_eq!(url.host_str(), Some("charts-static.billboard.com"));
    }

    #[test]
    fn refuses_hosts_that_merely_look_like_billboard() {
        // A suffix check would let this through and turn the route into an SSRF
        // relay.
        let err = assert_art_url("https://charts-static.billboard.com.evil.test/x.jpg")
            .expect_err("a lookalike host is refused");
        assert!(err.message.contains("Refusing to fetch artwork"));
    }

    #[test]
    fn refuses_link_local_localhost_and_private_targets() {
        for url in [
            "http://169.254.169.254/latest/meta-data/",
            "https://127.0.0.1:8787/api/health",
            "https://10.0.0.5/x.jpg",
            "https://[::1]/x.jpg",
        ] {
            let err = assert_art_url(url).expect_err(url);
            assert!(
                err.message.contains("Refusing to fetch artwork"),
                "{url}: {}",
                err.message
            );
        }
    }

    #[test]
    fn refuses_non_https_schemes_including_the_allowlisted_host_over_http() {
        for url in [
            "file:///etc/passwd",
            "http://charts-static.billboard.com/x.jpg",
        ] {
            let err = assert_art_url(url).expect_err(url);
            assert!(
                err.message.contains("Refusing to fetch artwork"),
                "{url}: {}",
                err.message
            );
        }
    }

    #[test]
    fn refuses_input_that_is_not_a_url_at_all() {
        for url in ["", "not a url", "/relative/path.jpg"] {
            let err = assert_art_url(url).expect_err(url);
            assert!(
                err.message.contains("not a valid URL")
                    || err.message.contains("Refusing to fetch artwork"),
                "{url}: {}",
                err.message
            );
        }
    }

    /* ------------------------------------------------------------ get_chart */

    fn state_for(server: &MockServer) -> AppState {
        AppState::new(Config {
            billboard_base: server.uri(),
            ..Config::default()
        })
    }

    #[tokio::test]
    async fn fetches_the_latest_chart_from_the_configured_base() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/charts/hot-100/"))
            .respond_with(ResponseTemplate::new(200).set_body_string(page()))
            .expect(1)
            .mount(&server)
            .await;

        let state = state_for(&server);
        let chart = get_chart(&state, "Hot-100", None)
            .await
            .expect("the chart resolves");

        assert_eq!(chart.chart, "hot-100");
        assert_eq!(chart.date, "2026-08-15");
        assert_eq!(chart.entries.len(), 3);
        assert!(chart.source_url.ends_with("/charts/hot-100/"));
        // A second call is served from `billboard:hot-100:latest`; the mock's
        // `expect(1)` fails the test if it reaches the wire again.
        get_chart(&state, "hot-100", None)
            .await
            .expect("the second call is cached");
    }

    #[tokio::test]
    async fn a_dated_chart_addresses_and_caches_under_its_week() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/charts/hot-100/2026-08-15/"))
            .respond_with(ResponseTemplate::new(200).set_body_string(page()))
            .expect(1)
            .mount(&server)
            .await;

        let state = state_for(&server);
        let chart = get_chart(&state, "hot-100", Some("2026-08-15"))
            .await
            .expect("the dated chart resolves");
        assert!(chart.source_url.ends_with("/charts/hot-100/2026-08-15/"));

        // The dated request is a different cache key from the undated one.
        assert!(state
            .cache()
            .get::<BillboardChart>("billboard:hot-100:2026-08-15")
            .await
            .is_some());
        assert!(state
            .cache()
            .get::<BillboardChart>("billboard:hot-100:latest")
            .await
            .is_none());
    }

    #[tokio::test]
    async fn rejects_a_bad_slug_before_reaching_the_network() {
        let server = MockServer::start().await;
        // No mock at all: the validator has to refuse before any request.
        let err = get_chart(&state_for(&server), "../admin", None)
            .await
            .expect_err("a traversal slug is refused");
        assert_eq!(err.code, codes::BAD_REQUEST);
    }

    #[tokio::test]
    async fn a_page_without_chart_rows_is_a_parse_failure_not_a_miss() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/charts/hot-100/"))
            .respond_with(
                ResponseTemplate::new(200).set_body_string("<html><body>maintenance</body></html>"),
            )
            .mount(&server)
            .await;

        let err = get_chart(&state_for(&server), "hot-100", None)
            .await
            .expect_err("an empty page is a parse failure");
        assert_eq!(err.code, codes::PARSE_FAILED);
    }
}
