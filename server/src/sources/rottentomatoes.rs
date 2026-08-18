//! Rotten Tomatoes — scraped from rottentomatoes.com.
//!
//! This is the feed behind Kalshi's single busiest entertainment series. `KXRT`
//! lists a strike ladder per film ("Dune: Part Three ≥ 85%") and settles on the
//! Tomatometer, so the score and — just as importantly — *whether a score exists
//! yet* is the number those markets trade around.
//!
//! The page embeds its own state as JSON rather than only rendering it:
//!
//! ```text
//!   #media-scorecard-json   { criticsScore, audienceScore, description }
//!   ld+json                 { name, dateCreated, @type }
//! ```
//!
//! Reading those beats scraping the rendered score badges, which are web
//! components whose shadow DOM never appears in the served HTML. A film with no
//! Tomatometer yet returns `score: None` rather than 0 — the distinction is the
//! whole point when the market is "will it score above 85".

use std::sync::{Arc, LazyLock};
use std::time::Duration;

use regex::Regex;
use scraper::Html;
use serde_json::{Map, Value};
use terminal_core::types::{RtScore, RtSearchResponse, RtSearchResult, RtTitle};
use terminal_core::util::fold_diacritics;

use crate::app::AppState;
use crate::cache::ttl;
use crate::css;
use crate::error::{codes, Result, UpstreamError};
use crate::http::FetchOptions;
use crate::scrape;

/// How many hits `RT <title>` shows when it has to fall back to search.
pub const DEFAULT_SEARCH_LIMIT: usize = 20;

/// RT slugs are `[a-z0-9_]` with the occasional hyphen, e.g. `dune_part_two`.
static SLUG: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-z0-9][a-z0-9_-]{0,120}$").expect("valid regex"));
/// The shape a typed input has to have before it is treated as a slug already.
static SLUG_SHAPED: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(tv/)?[a-z0-9][a-z0-9_-]*$").expect("valid regex"));
static RT_SUFFIX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\s*\|\s*Rotten Tomatoes\s*$").expect("valid regex"));
static LEADING_YEAR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(\d{4})").expect("valid regex"));
static SLUG_IN_HREF: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)rottentomatoes\.com/(m|tv)/([a-z0-9_-]+)").expect("valid regex")
});

/// Turn a free-text title into RT's slug form.
///
/// `Dune: Part Two` → `dune_part_two`. RT disambiguates same-named titles with a
/// year suffix (`dune_2021`), which this cannot know about — that is what the
/// search fallback in [`get_title`] is for.
#[must_use]
pub fn slugify(input: &str) -> String {
    // The original normalises NFKD and lets the `[^a-z0-9]+` pass turn the
    // freed-up combining marks into separators, so `Amélie` becomes `ame_lie`.
    // `fold_diacritics` drops the marks instead, which is the slug RT actually
    // serves.
    let folded = fold_diacritics(input).to_lowercase();
    let expanded = folded.replace(['\'', '\u{2019}'], "").replace('&', " and ");

    let mut out = String::with_capacity(expanded.len());
    for ch in expanded.chars() {
        if ch.is_ascii_lowercase() || ch.is_ascii_digit() {
            out.push(ch);
        } else if !out.ends_with('_') {
            out.push('_');
        }
    }

    let trimmed = out.trim_matches('_');
    // Every byte left is ASCII, so a byte cut is the character cut `.slice` made.
    trimmed[..trimmed.len().min(120)].to_owned()
}

pub fn assert_slug(raw: &str) -> Result<String> {
    let slug = raw.trim().to_lowercase();
    if !SLUG.is_match(&slug) {
        return Err(UpstreamError::bad_request(format!(
            "\"{raw}\" is not a valid Rotten Tomatoes slug"
        ))
        .with_hint("Try a title instead, e.g. `RT dune part three`."));
    }
    Ok(slug)
}

/// `Number.parseInt(text, 10)`: a leading integer, whatever follows it ignored.
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

/// The wire declares scores and review counts as unsigned integers. RT sends
/// them as strings anyway, so a JSON number here is already whole.
fn count_from_f64(n: f64) -> Option<u32> {
    (n.is_finite() && n >= 0.0 && n <= f64::from(u32::MAX)).then_some(n as u32)
}

/// `"92"` → 92; `""`, `"--"`, absent → None. A film awaiting reviews has none.
fn score_from_text(value: &str) -> Option<u32> {
    let cleaned: String = value
        .chars()
        .filter(|ch| *ch != '%' && !ch.is_whitespace())
        .collect();
    if cleaned.is_empty() {
        return None;
    }
    parse_int(&cleaned).and_then(|n| u32::try_from(n).ok())
}

fn score_or_null(value: Option<&Value>) -> Option<u32> {
    match value {
        Some(Value::Number(n)) => n.as_f64().and_then(count_from_f64),
        Some(Value::String(text)) => score_from_text(text),
        _ => None,
    }
}

fn int_or_null(value: Option<&Value>) -> Option<u32> {
    match value {
        Some(Value::Number(n)) => n.as_f64().and_then(count_from_f64),
        Some(Value::String(text)) => {
            let cleaned: String = text
                .chars()
                .filter(|ch| *ch != ',' && !ch.is_whitespace())
                .collect();
            parse_int(&cleaned).and_then(|n| u32::try_from(n).ok())
        }
        _ => None,
    }
}

/// Normalise one of RT's two score blocks.
///
/// The critic and audience payloads have the same shape but different
/// vocabularies — critics carry `certified`, the audience carries
/// `certifiedFresh: "certified"` — so both spellings are folded into one flag.
fn normalise_score(raw: Option<&Value>, certified_label: &str) -> RtScore {
    // An absent block short-circuits. An *empty* one is truthy upstream and
    // takes the long way round to the same answer, which is why this only
    // checks for nothing at all.
    let Some(raw) = raw.filter(|value| !value.is_null()) else {
        return RtScore {
            score: None,
            average_rating: String::new(),
            review_count: None,
            state: "not yet scored".to_owned(),
            certified: false,
        };
    };

    let score = score_or_null(raw.get("score"));
    let certified = raw.get("certified") == Some(&Value::Bool(true))
        || raw.get("certifiedFresh").and_then(Value::as_str) == Some("certified");

    let state = if score.is_none() {
        "not yet scored".to_owned()
    } else if certified {
        certified_label.to_owned()
    } else if let Some(sentiment) = raw
        .get("sentiment")
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
    {
        sentiment.to_lowercase()
    } else if score.is_some_and(|score| score >= 60) {
        "positive".to_owned()
    } else {
        "negative".to_owned()
    };

    RtScore {
        score,
        average_rating: raw
            .get("averageRating")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        review_count: int_or_null(raw.get("reviewCount"))
            .or_else(|| int_or_null(raw.get("ratingCount"))),
        state,
        certified,
    }
}

/// Pull the first ld+json block that describes the title itself.
fn read_ld_json(doc: &Html) -> Map<String, Value> {
    let mut found = Map::new();
    for node in scrape::select_all(doc, css!(r#"script[type="application/ld+json"]"#)) {
        if found
            .get("name")
            .and_then(Value::as_str)
            .is_some_and(|name| !name.is_empty())
        {
            break;
        }
        // A malformed block is not fatal — og: tags cover the same ground.
        // `raw_text_of`, not `text_of`: collapsing runs of whitespace inside a
        // JSON document rewrites any string value that contains two of them.
        if let Ok(Value::Object(parsed)) = serde_json::from_str::<Value>(&scrape::raw_text_of(node))
        {
            if parsed.contains_key("name") {
                found = parsed;
            }
        }
    }
    found
}

pub fn parse_title_page(html: &str, slug: &str, source_url: &str) -> Result<RtTitle> {
    let doc = scrape::parse_document(html);

    let scorecard_raw = scrape::select_one(&doc, css!("#media-scorecard-json"))
        .map(|el| scrape::raw_text_of(el).trim().to_owned())
        .unwrap_or_default();
    if scorecard_raw.is_empty() {
        return Err(
            UpstreamError::parse_failed(format!("No score data found on {source_url}")).with_hint(
                "Rotten Tomatoes served a page without its score payload. The slug may \
                 point at something other than a film or show — try `RT <title>` to search.",
            ),
        );
    }

    let scorecard: Value = serde_json::from_str(&scorecard_raw).map_err(|_| {
        UpstreamError::parse_failed(format!("Score payload on {source_url} is not valid JSON"))
    })?;

    let ld = read_ld_json(&doc);

    let og_title =
        scrape::attr_of_first(&doc, css!(r#"meta[property="og:title"]"#), "content").unwrap_or("");
    // `??` only falls through on `undefined`, and `.replace().trim()` always
    // answers with a string — so the `slug.replace(/_/g, ' ')` arm that follows
    // this one in the original can never run. What supplies the fallback is
    // `title || slug` at the end.
    let title = match ld.get("name").and_then(Value::as_str) {
        Some(name) => name.to_owned(),
        None => RT_SUFFIX.replace(og_title, "").trim().to_owned(),
    };

    // `dateCreated` is a full ISO date; the year is all a terminal column needs.
    let year = ld
        .get("dateCreated")
        .and_then(Value::as_str)
        .and_then(|date| LEADING_YEAR.captures(date))
        .map(|caps| caps[1].to_owned())
        .unwrap_or_default();

    let og_type =
        scrape::attr_of_first(&doc, css!(r#"meta[property="og:type"]"#), "content").unwrap_or("");
    let media_type = if ld.get("@type").and_then(Value::as_str) == Some("TVSeries")
        || og_type.contains("tv_show")
        || slug.starts_with("tv/")
    {
        "TV"
    } else {
        "Movie"
    };

    let synopsis = scorecard
        .get("description")
        .and_then(Value::as_str)
        .or_else(|| ld.get("description").and_then(Value::as_str))
        .unwrap_or_default();

    Ok(RtTitle {
        slug: slug.to_owned(),
        title: if title.is_empty() {
            slug.to_owned()
        } else {
            title
        },
        year,
        media_type: media_type.to_owned(),
        critics: normalise_score(scorecard.get("criticsScore"), "certified fresh"),
        audience: normalise_score(scorecard.get("audienceScore"), "verified hot"),
        synopsis: scrape::collapse_ws(synopsis),
        source_url: source_url.to_owned(),
    })
}

#[must_use]
pub fn parse_search_page(html: &str, query: &str) -> RtSearchResponse {
    let doc = scrape::parse_document(html);
    let mut results: Vec<RtSearchResult> = Vec::new();

    // Each hit is a <search-page-media-row> custom element. The scores and years
    // live in its *attributes*, which survive in the served HTML even though the
    // element's own rendering does not.
    for row in scrape::select_all(&doc, css!("search-page-media-row")) {
        let href = scrape::attr_of_first(row, css!(r#"a[data-qa="thumbnail-link"]"#), "href")
            .or_else(|| scrape::attr_of_first(row, css!("a"), "href"))
            .unwrap_or("");
        let Some(caps) = SLUG_IN_HREF.captures(href) else {
            continue;
        };
        // The href match is case-insensitive but this comparison is not, so a
        // `/TV/` path reads as a film. Carried across as written.
        let is_tv = &caps[1] == "tv";
        let slug = caps[2].to_owned();

        let name = scrape::select_one(row, css!(r#"[data-qa="info-name"]"#))
            .map(|el| scrape::raw_text_of(el).trim().to_owned())
            .filter(|name| !name.is_empty())
            .or_else(|| scrape::attr_of_first(row, css!("img"), "alt").map(str::to_owned))
            .filter(|name| !name.is_empty())
            .unwrap_or_default();
        if name.is_empty() {
            continue;
        }

        results.push(RtSearchResult {
            slug: if is_tv { format!("tv/{slug}") } else { slug },
            title: name,
            year: scrape::attr(row, "release-year")
                .or_else(|| scrape::attr(row, "start-year"))
                .unwrap_or("")
                .to_owned(),
            media_type: if is_tv { "TV" } else { "Movie" }.to_owned(),
            critics_score: score_from_text(scrape::attr(row, "tomatometer-score").unwrap_or("")),
        });
    }

    RtSearchResponse {
        query: query.to_owned(),
        results,
    }
}

pub async fn search(state: &AppState, query: &str, limit: usize) -> Result<RtSearchResponse> {
    let q = query.trim();
    if q.is_empty() {
        return Err(UpstreamError::bad_request("Missing search text")
            .with_hint("Usage: `RT <title>`, e.g. `RT dune part three`."));
    }

    let base = &state.config().rottentomatoes_base;
    let source_url = format!("{base}/search?search={}", urlencoding::encode(q));
    let key = format!("rt:search:{}", q.to_lowercase());

    let found = state
        .cache()
        .cached(&key, ttl::ROTTEN_TOMATOES, || async {
            let html = state
                .http()
                .fetch_text(
                    &source_url,
                    FetchOptions::new()
                        .timeout(Duration::from_secs(30))
                        .retries(2),
                )
                .await?;
            Ok(parse_search_page(&html, q))
        })
        .await?;

    Ok(RtSearchResponse {
        query: found.query.clone(),
        results: found.results.iter().take(limit).cloned().collect(),
    })
}

/// Fetch a title by slug, falling back to search when the guessed slug 404s.
///
/// A user types `RT dune part three`, not `RT dune_part_three`, and RT's slugs
/// carry disambiguating suffixes (`dune_2021`) that no slugifier can predict. So
/// the direct guess is tried first because it is one request and usually right,
/// and search is the backstop that makes the natural phrasing work.
pub async fn get_title(state: &AppState, input: &str) -> Result<Arc<RtTitle>> {
    let raw = input.trim();
    if raw.is_empty() {
        return Err(UpstreamError::bad_request("Missing title")
            .with_hint("Usage: `RT <title>`, e.g. `RT dune part three`."));
    }

    let lowered = raw.to_lowercase();
    let looks_like_slug = SLUG_SHAPED.is_match(&lowered) && raw.contains('_');
    let guess = if looks_like_slug {
        lowered
    } else {
        slugify(raw)
    };

    match fetch_title(state, &guess).await {
        Ok(title) => return Ok(title),
        // Only a miss is worth a second request; a block or a timeout would just
        // fail again, more slowly.
        Err(err) if err.code != codes::NOT_FOUND => return Err(err),
        Err(_) => {}
    }

    let found = search(state, raw, 5).await?;
    let Some(hit) = found.results.first() else {
        return Err(UpstreamError::not_found(format!(
            "Rotten Tomatoes has no title matching \"{raw}\""
        ))
        .with_hint(
            "Check the spelling, or open rottentomatoes.com and use the slug from the URL.",
        ));
    };
    fetch_title(state, &hit.slug).await
}

async fn fetch_title(state: &AppState, slug_or_path: &str) -> Result<Arc<RtTitle>> {
    // `tv/<slug>` addresses a series; everything else is a film under `/m/`.
    let is_tv = slug_or_path.starts_with("tv/");
    let slug = assert_slug(if is_tv {
        &slug_or_path[3..]
    } else {
        slug_or_path
    })?;
    let path = if is_tv {
        format!("tv/{slug}")
    } else {
        format!("m/{slug}")
    };
    let base = &state.config().rottentomatoes_base;
    let source_url = format!("{base}/{path}");
    let page_slug = if is_tv {
        format!("tv/{slug}")
    } else {
        slug.clone()
    };

    state
        .cache()
        .cached(
            &format!("rt:title:{path}"),
            ttl::ROTTEN_TOMATOES,
            || async {
                let html = state
                    .http()
                    .fetch_text(
                        &source_url,
                        FetchOptions::new()
                            .timeout(Duration::from_secs(30))
                            .retries(2),
                    )
                    .await?;
                parse_title_page(&html, &page_slug, &source_url)
            },
        )
        .await
}

#[cfg(test)]
mod tests {
    //! Rotten Tomatoes parser tests.
    //!
    //! The fixtures mirror the real page structure: the score payload sits in a
    //! `<script id="media-scorecard-json">` whose attributes are split across
    //! lines, and search hits are `<search-page-media-row>` custom elements
    //! carrying their data in attributes. Both are trimmed from live captures.
    //!
    //! The case that matters most is a film with no Tomatometer yet — which is
    //! exactly when a Kalshi KXRT ladder has something to price, and where
    //! returning `0` instead of `None` would read as "universally panned".

    use super::*;

    use crate::config::Config;
    use serde_json::json;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn page_with(scorecard: &Value, title: &str, date: &str, og_type: &str) -> String {
        let ld = json!({
            "@type": if og_type == "video.tv_show" { "TVSeries" } else { "Movie" },
            "name": title,
            "dateCreated": date,
        });

        format!(
            r#"<!doctype html><html><head>
<meta property="og:title" content="{title} | Rotten Tomatoes">
<meta property="og:type" content="{og_type}">
<script type="application/ld+json">{ld}</script>
</head><body>
  <script
      id="media-scorecard-json"
      data-json="mediaScorecard"
      type="application/json"
  >
  {scorecard}
  </script>
</body></html>"#
        )
    }

    fn page(scorecard: &Value) -> String {
        page_with(scorecard, "Dune: Part Two", "2024-03-01", "video.movie")
    }

    fn scored() -> Value {
        json!({
            "criticsScore": {
                "score": "92",
                "averageRating": "8.40",
                "reviewCount": 466,
                "sentiment": "POSITIVE",
                "certified": true,
            },
            "audienceScore": {
                "score": "95",
                "averageRating": "4.7",
                "reviewCount": 2246,
                "sentiment": "POSITIVE",
                "certifiedFresh": "certified",
            },
            "description": "Paul Atreides unites with the Fremen.",
        })
    }

    fn dune() -> RtTitle {
        parse_title_page(&page(&scored()), "dune_part_two", "u").expect("the fixture parses")
    }

    /* -------------------------------------------------- parse_title_page */

    #[test]
    fn reads_both_scores_as_numbers() {
        let title = dune();
        assert_eq!(title.critics.score, Some(92));
        assert_eq!(title.audience.score, Some(95));
    }

    #[test]
    fn reads_the_title_year_and_type_from_the_ld_json_block() {
        let title = dune();
        assert_eq!(title.title, "Dune: Part Two");
        assert_eq!(title.year, "2024");
        assert_eq!(title.media_type, "Movie");
    }

    #[test]
    fn carries_review_counts_and_average_ratings_through() {
        let title = dune();
        assert_eq!(title.critics.review_count, Some(466));
        assert_eq!(title.critics.average_rating, "8.40");
        assert_eq!(title.audience.average_rating, "4.7");
    }

    #[test]
    fn recognises_certification_from_either_spelling() {
        // Critics say `certified: true`; the audience says `certifiedFresh:
        // "certified"`. Both mean the same thing and must fold to one flag.
        let title = dune();
        assert!(title.critics.certified);
        assert_eq!(title.critics.state, "certified fresh");
        assert!(title.audience.certified);
    }

    #[test]
    fn reports_an_unreleased_film_as_null_not_zero() {
        let html = page_with(
            &json!({ "criticsScore": { "score": "" }, "audienceScore": {} }),
            "Dune: Part Three",
            "2026-12-18",
            "video.movie",
        );
        let unscored =
            parse_title_page(&html, "dune_part_three", "u").expect("an unscored page parses");

        assert_eq!(unscored.critics.score, None);
        assert_eq!(unscored.audience.score, None);
        assert_eq!(unscored.critics.state, "not yet scored");
    }

    #[test]
    fn detects_a_tv_series() {
        let html = page_with(&scored(), "The Last of Us", "2024-03-01", "video.tv_show");
        let show = parse_title_page(&html, "tv/the_last_of_us", "u").expect("a series page parses");
        assert_eq!(show.media_type, "TV");
    }

    #[test]
    fn derives_a_rotten_sentiment_when_the_payload_omits_one() {
        let rotten = parse_title_page(
            &page(&json!({ "criticsScore": { "score": "31" } })),
            "x",
            "u",
        )
        .expect("the page parses");
        assert_eq!(rotten.critics.score, Some(31));
        assert_eq!(rotten.critics.state, "negative");
    }

    #[test]
    fn throws_a_diagnosable_error_when_the_score_payload_is_missing() {
        let err = parse_title_page(
            "<html><body><p>nothing</p></body></html>",
            "x",
            "https://rt/",
        )
        .expect_err("a page with no scorecard is a parse failure");
        assert_eq!(err.code, codes::PARSE_FAILED);
        assert!(err.message.contains("No score data found"));
    }

    #[test]
    fn throws_when_the_score_payload_is_not_valid_json() {
        let broken = "<html><body><script id=\"media-scorecard-json\" \
                      type=\"application/json\">{not json</script></body></html>";
        let err = parse_title_page(broken, "x", "https://rt/")
            .expect_err("a broken payload is a parse failure");
        assert_eq!(err.code, codes::PARSE_FAILED);
        assert!(err.message.contains("not valid JSON"));
    }

    /* ------------------------------------------------- parse_search_page */

    const SEARCH: &str = r#"<html><body>
<search-page-media-row release-year="2024" tomatometer-score="92" tomatometer-is-certified="true">
  <a href="https://www.rottentomatoes.com/m/dune_part_two" data-qa="thumbnail-link"><img alt="Dune: Part Two"></a>
  <span data-qa="info-name">Dune: Part Two</span>
</search-page-media-row>
<search-page-media-row release-year="2026" tomatometer-score="">
  <a href="https://www.rottentomatoes.com/m/dune_part_three" data-qa="thumbnail-link"><img alt="Dune: Part Three"></a>
  <span data-qa="info-name">Dune: Part Three</span>
</search-page-media-row>
<search-page-media-row start-year="2023" tomatometer-score="96">
  <a href="https://www.rottentomatoes.com/tv/the_last_of_us" data-qa="thumbnail-link"><img alt="The Last of Us"></a>
  <span data-qa="info-name">The Last of Us</span>
</search-page-media-row>
</body></html>"#;

    #[test]
    fn extracts_every_hit_with_its_slug_and_score() {
        let found = parse_search_page(SEARCH, "dune");
        assert_eq!(found.results.len(), 3);
        assert_eq!(found.results[0].slug, "dune_part_two");
        assert_eq!(found.results[0].critics_score, Some(92));
        assert_eq!(found.results[0].year, "2024");
    }

    #[test]
    fn reports_an_unscored_title_as_null_rather_than_0() {
        let found = parse_search_page(SEARCH, "dune");
        assert_eq!(found.results[1].critics_score, None);
    }

    #[test]
    fn keeps_the_tv_prefix_so_the_slug_addresses_the_right_page() {
        let found = parse_search_page(SEARCH, "dune");
        assert_eq!(found.results[2].slug, "tv/the_last_of_us");
        assert_eq!(found.results[2].media_type, "TV");
        // TV rows carry `start-year` instead of `release-year`.
        assert_eq!(found.results[2].year, "2023");
    }

    #[test]
    fn returns_no_results_rather_than_throwing_on_an_empty_page() {
        assert!(parse_search_page("<html><body></body></html>", "x")
            .results
            .is_empty());
    }

    /* --------------------------------------------------------- slugify */

    #[test]
    fn turns_a_typed_title_into_rt_slug_form() {
        assert_eq!(slugify("Dune: Part Two"), "dune_part_two");
        assert_eq!(
            slugify("Spider-Man: Brand New Day"),
            "spider_man_brand_new_day"
        );
    }

    #[test]
    fn drops_apostrophes_rather_than_turning_them_into_separators() {
        assert_eq!(slugify("Don't Look Up"), "dont_look_up");
        assert_eq!(slugify("Don\u{2019}t Look Up"), "dont_look_up");
    }

    #[test]
    fn spells_out_an_ampersand() {
        assert_eq!(slugify("Fire & Ice"), "fire_and_ice");
    }

    #[test]
    fn never_leaves_leading_or_trailing_separators() {
        assert_eq!(slugify("  ...Wicked!  "), "wicked");
    }

    /* ------------------------------------------------- get_title / search */

    fn state_for(server: &MockServer) -> AppState {
        AppState::new(Config {
            rottentomatoes_base: server.uri(),
            ..Config::default()
        })
    }

    async fn mount_title(server: &MockServer, at: &str, body: String) {
        Mock::given(method("GET"))
            .and(path(at.to_owned()))
            .respond_with(ResponseTemplate::new(200).set_body_string(body))
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn fetches_a_guessed_slug_from_the_configured_base() {
        let server = MockServer::start().await;
        mount_title(&server, "/m/dune_part_two", page(&scored())).await;

        let state = state_for(&server);
        let title = get_title(&state, "Dune: Part Two")
            .await
            .expect("the guessed slug resolves");

        assert_eq!(title.slug, "dune_part_two");
        assert_eq!(title.critics.score, Some(92));
        assert!(title.source_url.ends_with("/m/dune_part_two"));
        assert!(state
            .cache()
            .get::<RtTitle>("rt:title:m/dune_part_two")
            .await
            .is_some());
    }

    #[tokio::test]
    async fn falls_back_to_search_when_the_guessed_slug_is_a_miss() {
        let server = MockServer::start().await;
        // The guess misses…
        Mock::given(method("GET"))
            .and(path("/m/the_last_of_us"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        // …search names the real page, which is a series under `tv/`…
        Mock::given(method("GET"))
            .and(path("/search"))
            .and(query_param("search", "The Last of Us"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"<html><body>
<search-page-media-row start-year="2023" tomatometer-score="96">
  <a href="https://www.rottentomatoes.com/tv/the_last_of_us" data-qa="thumbnail-link"><img alt="The Last of Us"></a>
  <span data-qa="info-name">The Last of Us</span>
</search-page-media-row>
</body></html>"#,
            ))
            .mount(&server)
            .await;
        // …and the fallback re-fetches under the slug search handed back.
        mount_title(
            &server,
            "/tv/the_last_of_us",
            page_with(&scored(), "The Last of Us", "2023-01-15", "video.tv_show"),
        )
        .await;

        let title = get_title(&state_for(&server), "The Last of Us")
            .await
            .expect("the search fallback resolves");

        assert_eq!(title.slug, "tv/the_last_of_us");
        assert_eq!(title.media_type, "TV");
    }

    #[tokio::test]
    async fn a_slug_that_addresses_a_series_is_fetched_under_tv() {
        let server = MockServer::start().await;
        mount_title(
            &server,
            "/tv/the_last_of_us",
            page_with(&scored(), "The Last of Us", "2023-01-15", "video.tv_show"),
        )
        .await;

        let state = state_for(&server);
        let title = get_title(&state, "tv/the_last_of_us")
            .await
            .expect("the series resolves");

        assert_eq!(title.slug, "tv/the_last_of_us");
        assert_eq!(title.media_type, "TV");
        assert!(state
            .cache()
            .get::<RtTitle>("rt:title:tv/the_last_of_us")
            .await
            .is_some());
    }

    #[tokio::test]
    async fn does_not_search_after_a_failure_that_is_not_a_miss() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/m/dune_part_two"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;
        // No `/search` mock: reaching for it would 404 and change the code.

        let err = get_title(&state_for(&server), "Dune: Part Two")
            .await
            .expect_err("a block is not a miss");
        assert_eq!(err.code, codes::UPSTREAM_STATUS);
    }

    #[tokio::test]
    async fn reports_a_title_search_cannot_place_as_not_found() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/m/nonesuch_film"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/search"))
            .respond_with(ResponseTemplate::new(200).set_body_string("<html><body></body></html>"))
            .mount(&server)
            .await;

        let err = get_title(&state_for(&server), "Nonesuch Film")
            .await
            .expect_err("nothing matches");
        assert_eq!(err.code, codes::NOT_FOUND);
        assert!(err.message.contains("Nonesuch Film"));
    }

    #[tokio::test]
    async fn search_caches_by_query_and_trims_to_the_limit() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/search"))
            // The URL carries the query as typed; only the cache key is folded.
            .and(query_param("search", "Dune"))
            .respond_with(ResponseTemplate::new(200).set_body_string(SEARCH))
            .expect(1)
            .mount(&server)
            .await;

        let state = state_for(&server);
        let found = search(&state, "Dune", 2)
            .await
            .expect("the search resolves");
        assert_eq!(found.query, "Dune");
        assert_eq!(found.results.len(), 2);

        // Cached under the lower-cased query; the mock's `expect(1)` catches a
        // second trip to the wire.
        assert!(state
            .cache()
            .get::<RtSearchResponse>("rt:search:dune")
            .await
            .is_some());
        let again = search(&state, "Dune", DEFAULT_SEARCH_LIMIT)
            .await
            .expect("the second search is cached");
        assert_eq!(again.results.len(), 3);
    }

    #[tokio::test]
    async fn refuses_empty_input_before_reaching_the_network() {
        let server = MockServer::start().await;
        let state = state_for(&server);

        let err = search(&state, "   ", DEFAULT_SEARCH_LIMIT)
            .await
            .expect_err("empty search text is refused");
        assert_eq!(err.code, codes::BAD_REQUEST);

        let err = get_title(&state, "  ")
            .await
            .expect_err("an empty title is refused");
        assert_eq!(err.code, codes::BAD_REQUEST);
    }
}
