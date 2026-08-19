//! Podcast charts — Apple's own ranking.
//!
//! `KXTOPPOD`, `KXROGANGUEST`, `KXPODCASTGUESTCALLHERDADDY`,
//! `KXCALLHERDADDYCANCELED` and the rest of Kalshi's podcast book had no
//! companion feed at all — it was the one entertainment genre the terminal was
//! silent on, despite `ENT` happily listing the markets.
//!
//! Apple publishes the chart itself, as JSON, at `rss.marketingtools.apple.com`.
//! That is a first-party publication rather than a scrape, which puts it in the
//! same category as the Netflix TSVs: no markup to break, no mirror to caveat.
//!
//! Two charts, because they answer different questions. `top` is Apple's ranking
//! of shows overall — what `KXTOPPOD` is about. `episodes` is what is charting
//! right now, which is where a guest booking shows up, and a guest booking is
//! what most of these markets actually trade.

use serde::Deserialize;
use terminal_core::types::{PodcastChart, PodcastEntry};

use crate::app::AppState;
use crate::cache::ttl;
use crate::error::{codes, Result, UpstreamError};
use crate::http::FetchOptions;

/// Apple serves fixed sizes and 100 is the largest, so one upstream response
/// satisfies any panel's row count.
const FETCH_SIZE: u32 = 100;

/// The two charts, keyed by the *resource* name.
///
/// Apple's URL is `/{country}/podcasts/top/{size}/{resource}.json`, and shows
/// and episodes differ only in the last segment — both live under
/// `podcasts/top`. The obvious guess, `podcast-episodes/top/…`, 404s.
const VIEWS: &[(&str, &str, &str)] = &[
    ("top", "podcasts", "Top Shows"),
    ("episodes", "podcast-episodes", "Trending Episodes"),
];

fn view_of(name: &str) -> Option<(&'static str, &'static str, &'static str)> {
    VIEWS.iter().copied().find(|(key, _, _)| *key == name)
}

/// Read a view name, forgiving the singular and the obvious synonym.
pub fn assert_view(raw: &str) -> Result<&'static str> {
    let lowered = raw.trim().to_lowercase();
    let normalised = match lowered.as_str() {
        "" => "top",
        "show" | "shows" => "top",
        "episode" => "episodes",
        other => other,
    };

    view_of(normalised).map(|(key, _, _)| key).ok_or_else(|| {
        UpstreamError::bad_request(format!("\"{raw}\" is not a podcast chart"))
            .with_hint("Views are: top, episodes. Usage: `POD [top|episodes] [country]`.")
    })
}

pub fn assert_country(raw: &str) -> Result<String> {
    let country = raw.trim().to_lowercase();
    let country = if country.is_empty() {
        "us".to_string()
    } else {
        country
    };

    if country.len() != 2 || !country.chars().all(|c| c.is_ascii_lowercase()) {
        return Err(
            UpstreamError::bad_request(format!("\"{raw}\" is not a country code"))
                .with_hint("Pass a two-letter ISO country code, e.g. `POD top gb`."),
        );
    }
    Ok(country)
}

#[derive(Debug, Deserialize)]
struct RawFeed {
    feed: Option<RawInner>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawInner {
    title: Option<String>,
    updated: Option<String>,
    results: Option<Vec<RawEntry>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawEntry {
    name: Option<String>,
    artist_name: Option<String>,
    release_date: Option<String>,
    url: Option<String>,
    genres: Option<Vec<RawGenre>>,
    content_advisory_rating: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawGenre {
    name: Option<String>,
}

/// Apple's own advisory rating, which Apple spells wrong.
///
/// The live chart returns `"Explict"` — missing the second `i` — for every
/// explicit row, on both views. An `== "explicit"` test therefore matches
/// nothing and silently reports a chart of exclusively clean content, which is
/// wrong without ever looking broken. The prefix is matched so a third spelling
/// does not reintroduce it.
fn is_explicit(rating: Option<&str>) -> bool {
    rating
        .map(|r| r.trim().to_lowercase().starts_with("expl"))
        .unwrap_or(false)
}

/// Shape Apple's feed into ranked entries.
///
/// The rank is positional — Apple ships the chart in order and never numbers it
/// — and the genre list is flattened to its first entry, because a table column
/// has room for one and Apple orders them by relevance.
pub fn parse_feed(body: &str, view: &str, country: &str, source_url: &str) -> Result<PodcastChart> {
    let parsed: RawFeed = serde_json::from_str(body).map_err(|err| {
        UpstreamError::new(
            format!("Apple's podcast chart did not parse: {err}"),
            codes::BAD_UPSTREAM_BODY,
        )
    })?;

    let inner = parsed.feed;
    let results = inner.as_ref().and_then(|f| f.results.as_ref());
    let rows = results.map(Vec::as_slice).unwrap_or_default();

    if rows.is_empty() {
        return Err(UpstreamError::not_found(format!(
            "Apple returned an empty {view} chart for {}",
            country.to_uppercase()
        ))
        .with_hint(
            "Apple answered but the chart had no entries. Not every country publishes \
             every chart — try `POD top us`.",
        ));
    }

    let mut entries: Vec<PodcastEntry> = Vec::new();
    for raw in rows {
        let Some(name) = raw.name.as_deref().map(str::trim).filter(|n| !n.is_empty()) else {
            continue;
        };

        entries.push(PodcastEntry {
            rank: u32::try_from(entries.len() + 1).unwrap_or(u32::MAX),
            name: name.to_string(),
            // On the episode chart this is the show; on the show chart it is the
            // publisher. Same field, and both are what you want beside a title.
            publisher: raw.artist_name.as_deref().unwrap_or("").trim().to_string(),
            genre: raw
                .genres
                .as_ref()
                .and_then(|g| g.first())
                .and_then(|g| g.name.as_deref())
                .unwrap_or("")
                .trim()
                .to_string(),
            // Neither chart currently carries one; the panel shows the column
            // only when the rows do.
            released: raw
                .release_date
                .as_deref()
                .unwrap_or("")
                .chars()
                .take(10)
                .collect(),
            explicit: is_explicit(raw.content_advisory_rating.as_deref()),
            url: raw.url.clone().unwrap_or_default(),
        });
    }

    if entries.is_empty() {
        return Err(UpstreamError::new(
            format!("No named entries in Apple's {view} chart"),
            codes::BAD_UPSTREAM_BODY,
        )
        .with_hint("The feed parsed but every entry was missing a name."));
    }

    let (_, _, label) = view_of(view).unwrap_or(("top", "podcasts", "Top Shows"));

    Ok(PodcastChart {
        view: view.to_string(),
        view_label: label.to_string(),
        country: country.to_uppercase(),
        title: inner
            .as_ref()
            .and_then(|f| f.title.clone())
            .unwrap_or_default(),
        updated: inner.and_then(|f| f.updated).unwrap_or_default(),
        entries,
        source_url: source_url.to_string(),
    })
}

pub async fn get_chart(
    state: &AppState,
    view: &str,
    country: &str,
    limit: usize,
) -> Result<PodcastChart> {
    let view = assert_view(view)?;
    let country = assert_country(country)?;
    let (_, resource, _) = view_of(view).expect("assert_view returns a known view");

    let source_url = format!(
        "{}/{country}/podcasts/top/{FETCH_SIZE}/{resource}.json",
        state.config().apple_rss_base
    );

    let key = format!("podcasts:{view}:{country}");
    let chart = state
        .cache()
        .cached(&key, ttl::PODCASTS, || async {
            let body = state
                .http()
                .fetch_text(
                    &source_url,
                    FetchOptions::new()
                        .timeout(std::time::Duration::from_secs(20))
                        .retries(2),
                )
                .await?;
            parse_feed(&body, view, &country, &source_url)
        })
        .await?;

    let mut chart = (*chart).clone();
    chart.entries.truncate(limit);
    Ok(chart)
}

#[cfg(test)]
mod tests {
    //! Apple's chart against a captured live response.

    use super::*;

    const SHOWS: &str = include_str!("fixtures/apple_podcasts.json");

    #[test]
    fn ranks_the_chart_by_the_order_apple_ships_it_in() {
        let chart = parse_feed(SHOWS, "top", "us", "https://example.test").expect("a chart");

        assert_eq!(chart.view_label, "Top Shows");
        assert_eq!(chart.country, "US");
        assert_eq!(chart.entries[0].rank, 1);
        assert_eq!(chart.entries[0].name, "The Daily");
        assert_eq!(chart.entries[0].publisher, "The New York Times");
        // Positional, and contiguous even where a nameless row was skipped.
        for (index, entry) in chart.entries.iter().enumerate() {
            assert_eq!(entry.rank as usize, index + 1);
        }
    }

    #[test]
    fn reads_apples_own_misspelling_of_explicit() {
        // The live feed says "Explict". An equality test against "explicit"
        // matches nothing and reports an all-clean chart, which is wrong
        // without ever looking broken.
        assert!(is_explicit(Some("Explict")));
        assert!(is_explicit(Some("explicit")));
        assert!(is_explicit(Some("  EXPLICIT  ")));
        assert!(!is_explicit(Some("Clean")));
        assert!(!is_explicit(None));

        let chart = parse_feed(SHOWS, "top", "us", "https://example.test").expect("a chart");
        assert!(
            chart.entries.iter().any(|e| e.explicit),
            "the captured chart carries explicit rows and none were read as such"
        );
    }

    #[test]
    fn takes_the_first_genre_because_a_column_has_room_for_one() {
        let chart = parse_feed(SHOWS, "top", "us", "https://example.test").expect("a chart");
        assert!(chart.entries.iter().all(|e| !e.genre.contains(',')));
        assert!(chart.entries.iter().any(|e| !e.genre.is_empty()));
    }

    #[test]
    fn refuses_an_empty_chart_rather_than_reporting_no_podcasts() {
        let empty = r#"{"feed":{"title":"Top Shows","results":[]}}"#;
        let err = parse_feed(empty, "top", "gb", "https://example.test")
            .expect_err("an empty chart is not an answer");
        assert_eq!(err.code, codes::NOT_FOUND);
    }

    #[test]
    fn names_the_views_it_takes_and_forgives_the_singular() {
        assert_eq!(assert_view("").unwrap(), "top");
        assert_eq!(assert_view("shows").unwrap(), "top");
        assert_eq!(assert_view("SHOW").unwrap(), "top");
        assert_eq!(assert_view("episode").unwrap(), "episodes");
        assert_eq!(assert_view("episodes").unwrap(), "episodes");

        let err = assert_view("charts").expect_err("not a view");
        assert_eq!(err.code, codes::BAD_REQUEST);
        assert!(err.hint.unwrap_or_default().contains("top, episodes"));
    }

    #[test]
    fn holds_the_country_to_two_letters() {
        assert_eq!(assert_country("").unwrap(), "us");
        assert_eq!(assert_country("GB").unwrap(), "gb");
        assert!(assert_country("united kingdom").is_err());
        assert!(assert_country("u").is_err());
    }
}
