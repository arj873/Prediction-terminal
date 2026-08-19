//! Google Trends — what is actually being searched, right now.
//!
//! `KXRANKLISTGOOGLESEARCH` ("#1 searched person on Google in 2026"), its top-5
//! and #2 variants and `KXGOOGLESEARCH` carry ~568k open interest between them,
//! and every one of them names `trends.google.com` as its settlement source. The
//! terminal had no reading of it at all.
//!
//! Google publishes the trending list as RSS — `/trending/rss?geo=US` — which is
//! the one Trends surface that needs neither a key nor a rendered browser. The
//! JSON endpoints behind trends.google.com are batchexecute RPCs that require a
//! session token; the RSS feed is a stable public document, so that is what this
//! reads.
//!
//! **This is the daily list, not the annual one those markets settle on.**
//! Google publishes "Year in Search" once, in December. What the feed shows is
//! who is being searched today, which is the evidence a trader has in August for
//! a market that resolves in December — the same relationship `BO` has to a
//! total-gross market. The panel says so rather than implying it is the ranking.
//!
//! Each item carries an approximate traffic figure (`500+`, `2M+`) and the news
//! stories Google attributes the spike to. Both are kept: the headline is
//! usually the whole explanation for why a name appeared.

use quick_xml::events::Event;
use quick_xml::Reader;
use terminal_core::types::{TrendEntry, TrendList};

use crate::app::AppState;
use crate::cache::ttl;
use crate::error::{codes, Result, UpstreamError};
use crate::http::FetchOptions;

/// Geographies worth naming. Google accepts any ISO-3166 alpha-2 and
/// [`assert_geo`] lets any of them through — this is the menu, not the limit.
pub const GEOS: &[(&str, &str)] = &[
    ("US", "United States"),
    ("GB", "United Kingdom"),
    ("CA", "Canada"),
    ("AU", "Australia"),
    ("IN", "India"),
    ("JP", "Japan"),
    ("DE", "Germany"),
    ("FR", "France"),
    ("BR", "Brazil"),
    ("MX", "Mexico"),
];

pub fn assert_geo(raw: &str) -> Result<String> {
    let geo = raw.trim().to_uppercase();
    let geo = if geo.is_empty() {
        "US".to_string()
    } else {
        geo
    };

    if geo.len() != 2 || !geo.chars().all(|c| c.is_ascii_uppercase()) {
        return Err(
            UpstreamError::bad_request(format!("\"{raw}\" is not a country code"))
                .with_hint("Pass a two-letter ISO country code, e.g. `TRND US` or `TRND GB`."),
        );
    }
    Ok(geo)
}

/// `500+` → 500, `2M+` → 2000000, `20K+` → 20000.
///
/// Google states these as lower bounds and never as exact counts, which is why
/// the field is named `traffic_floor` downstream. An unparseable value is `None`
/// rather than 0 — "Google did not say" and "nobody searched it" are different
/// facts, and only one of them is ever true here.
pub fn parse_traffic(raw: &str) -> Option<f64> {
    let text: String = raw
        .chars()
        .filter(|c| !matches!(c, '+' | ',' | ' ' | '\t' | '\n' | '\r'))
        .collect();
    if text.is_empty() {
        return None;
    }

    let (digits, scale) = match text.chars().last() {
        Some(last) if last.is_ascii_alphabetic() => {
            let factor = match last.to_ascii_uppercase() {
                'K' => 1e3,
                'M' => 1e6,
                'B' => 1e9,
                _ => return None,
            };
            (&text[..text.len() - 1], factor)
        }
        _ => (text.as_str(), 1.0),
    };

    let value: f64 = digits.parse().ok()?;
    if !value.is_finite() {
        return None;
    }
    Some((value * scale).round())
}

/// The fields one `<item>` contributes, gathered as the reader walks it.
#[derive(Default)]
struct Item {
    title: String,
    traffic: String,
    published: String,
    headline: String,
    headline_source: String,
    headline_url: String,
    articles: u32,
}

/// Parse the trending RSS.
///
/// A real XML reader rather than the HTML one the scraped feeds use: the
/// payload lives in namespaced elements (`ht:approx_traffic`,
/// `ht:news_item_title`), and html5ever mangles the prefixes so that
/// `ht:picture` and `ht:picture_source` collapse onto one lookup.
///
/// Read as a stream with a small amount of state rather than into a tree,
/// because the shape is flat and only the *first* news item's fields are
/// wanted — a tree would be built only to take its head.
pub fn parse_trends_rss(xml: &str, geo: &str, source_url: &str) -> Result<TrendList> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    let mut entries: Vec<TrendEntry> = Vec::new();
    let mut item: Option<Item> = None;
    let mut path: Vec<String> = Vec::new();
    let mut items_seen = 0usize;
    // Google repeats `<ht:news_item>` per story; only the first is shown, but
    // all of them are counted.
    let mut in_first_news = false;

    loop {
        match reader.read_event() {
            Ok(Event::Start(tag)) => {
                let name = String::from_utf8_lossy(tag.name().as_ref()).to_string();
                match name.as_str() {
                    "item" => {
                        item = Some(Item::default());
                        items_seen += 1;
                        in_first_news = false;
                    }
                    "ht:news_item" => {
                        if let Some(current) = item.as_mut() {
                            in_first_news = current.articles == 0;
                            current.articles += 1;
                        }
                    }
                    _ => {}
                }
                path.push(name);
            }
            Ok(Event::End(tag)) => {
                let name = String::from_utf8_lossy(tag.name().as_ref()).to_string();
                path.pop();
                if name == "item" {
                    if let Some(done) = item.take() {
                        if !done.title.is_empty() {
                            entries.push(TrendEntry {
                                // The feed is ordered but unnumbered; position
                                // in the document *is* the rank.
                                rank: u32::try_from(entries.len() + 1).unwrap_or(u32::MAX),
                                query: done.title,
                                traffic_floor: parse_traffic(&done.traffic),
                                started_at: done.published,
                                headline: done.headline,
                                headline_source: done.headline_source,
                                headline_url: done.headline_url,
                                articles: done.articles,
                            });
                        }
                    }
                }
            }
            Ok(Event::Text(text)) if item.is_some() => {
                let value = match reader.decoder().decode(text.as_ref()) {
                    Ok(decoded) => decoded.to_string(),
                    Err(_) => continue,
                };
                let value = quick_xml::escape::unescape(&value)
                    .map(|v| v.to_string())
                    .unwrap_or(value);
                let Some(current) = item.as_mut() else {
                    continue;
                };
                let Some(tag) = path.last().map(String::as_str) else {
                    continue;
                };

                match tag {
                    // `<title>` also appears on the channel, but `item` is only
                    // Some once inside one.
                    "title" if current.title.is_empty() => current.title = value,
                    "ht:approx_traffic" => current.traffic = value,
                    "pubDate" => current.published = value,
                    "ht:news_item_title" if in_first_news => current.headline = value,
                    "ht:news_item_source" if in_first_news => current.headline_source = value,
                    "ht:news_item_url" if in_first_news => current.headline_url = value,
                    _ => {}
                }
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(err) => {
                return Err(UpstreamError::new(
                    format!("Google's trending feed did not parse: {err}"),
                    codes::BAD_UPSTREAM_BODY,
                ));
            }
        }
    }

    if items_seen == 0 {
        return Err(UpstreamError::new(
            format!("No trending items in the feed for {geo}"),
            codes::BAD_UPSTREAM_BODY,
        )
        .with_hint(
            "Google answered but the feed carried no <item> elements. Either the \
             geography has no trending list, or the feed format changed.",
        ));
    }

    if entries.is_empty() {
        return Err(UpstreamError::new(
            format!("No trending searches found for {geo}"),
            codes::BAD_UPSTREAM_BODY,
        )
        .with_hint("The feed parsed but every item was missing a title."));
    }

    Ok(TrendList {
        geo: geo.to_string(),
        geo_label: GEOS
            .iter()
            .find(|(code, _)| *code == geo)
            .map(|(_, name)| (*name).to_string())
            .unwrap_or_else(|| geo.to_string()),
        entries,
        source_url: source_url.to_string(),
    })
}

pub async fn get_trending(state: &AppState, geo: &str, limit: usize) -> Result<TrendList> {
    let geo = assert_geo(geo)?;
    let source_url = format!(
        "{}/trending/rss?geo={geo}",
        state.config().google_trends_base
    );

    let key = format!("trends:{geo}");
    let list = state
        .cache()
        .cached(&key, ttl::TRENDS, || async {
            let xml = state
                .http()
                .fetch_text(
                    &source_url,
                    FetchOptions::new()
                        .timeout(std::time::Duration::from_secs(20))
                        .retries(2),
                )
                .await?;
            parse_trends_rss(&xml, &geo, &source_url)
        })
        .await?;

    let mut list = (*list).clone();
    list.entries.truncate(limit);
    Ok(list)
}

#[cfg(test)]
mod tests {
    //! The trending feed against a captured live response.

    use super::*;

    const FEED: &str = include_str!("fixtures/google_trends.xml");

    #[test]
    fn reads_the_rank_off_the_order_google_ships() {
        let list = parse_trends_rss(FEED, "US", "https://example.test").expect("a list");

        assert_eq!(list.geo_label, "United States");
        assert!(list.entries.len() >= 5);
        for (index, entry) in list.entries.iter().enumerate() {
            assert_eq!(entry.rank as usize, index + 1);
            assert!(!entry.query.is_empty());
        }
    }

    #[test]
    fn reaches_into_the_namespaced_elements_the_payload_lives_in() {
        let list = parse_trends_rss(FEED, "US", "https://example.test").expect("a list");
        let first = &list.entries[0];

        assert!(
            first.traffic_floor.is_some(),
            "ht:approx_traffic was not read"
        );
        assert!(
            !first.headline.is_empty(),
            "ht:news_item_title was not read"
        );
        assert!(
            !first.headline_source.is_empty(),
            "ht:news_item_source was not read"
        );
        assert!(first.articles >= 1);
        assert!(!first.started_at.is_empty(), "pubDate was not read");
    }

    #[test]
    fn takes_the_first_story_and_counts_the_rest() {
        // Google attributes a spike to several stories. The panel shows one and
        // says how many there were; taking the last would caption the row with
        // whichever outlet Google happened to list last.
        let list = parse_trends_rss(FEED, "US", "https://example.test").expect("a list");
        let busiest = list
            .entries
            .iter()
            .max_by_key(|e| e.articles)
            .expect("an entry");
        assert!(busiest.articles > 1, "the fixture has no multi-story row");
        assert!(!busiest.headline.is_empty());
    }

    #[test]
    fn reads_googles_lower_bounds_as_numbers() {
        assert_eq!(parse_traffic("500+"), Some(500.0));
        assert_eq!(parse_traffic("2M+"), Some(2_000_000.0));
        assert_eq!(parse_traffic("20K+"), Some(20_000.0));
        assert_eq!(parse_traffic("1,000+"), Some(1000.0));
        assert_eq!(parse_traffic(" 5B "), Some(5_000_000_000.0));
    }

    #[test]
    fn says_nothing_rather_than_zero_when_google_did_not() {
        // Zero would mean "nobody searched it", which is never true here.
        assert_eq!(parse_traffic(""), None);
        assert_eq!(parse_traffic("lots"), None);
        assert_eq!(parse_traffic("12Q"), None);
    }

    #[test]
    fn refuses_a_feed_with_no_items() {
        let empty = r#"<?xml version="1.0"?><rss><channel><title>x</title></channel></rss>"#;
        let err = parse_trends_rss(empty, "ZZ", "https://example.test")
            .expect_err("an empty feed is not a list");
        assert_eq!(err.code, codes::BAD_UPSTREAM_BODY);
    }

    #[test]
    fn holds_the_geography_to_two_letters() {
        assert_eq!(assert_geo("").unwrap(), "US");
        assert_eq!(assert_geo("gb").unwrap(), "GB");
        assert!(assert_geo("United Kingdom").is_err());
    }
}
