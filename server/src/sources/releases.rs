//! Release dates — the music calendar, from Apple's iTunes Search API.
//!
//! A whole cluster of Kalshi series asks *when*, not *how well*:
//! `KXALBUMRELEASEDATE*` ("When will Tate McRae release a new album?"),
//! `KXSONGRELEASE*`, `KXNEWTAYLOR`, `KXALBUMRELEASE*`, `KXCATALOGUE`. They
//! settle on `open.spotify.com`, and the terminal's existing music feeds cannot
//! answer them: `BB` and `SPOT` rank what is already out, so a market about an
//! unreleased record has nothing to read.
//!
//! Spotify's own API needs an OAuth client, which would be the first credential
//! in this codebase. Apple's Search API needs nothing, covers the same
//! catalogue, and — the part that matters here — **lists pre-orders with their
//! announced future release date**. An album dated three weeks out is the single
//! most direct piece of evidence a "will they release by X" market has.
//!
//! The endpoint's `sort=recent` parameter is accepted and then ignored; results
//! come back by relevance whatever you pass. Sorting is therefore done here, and
//! the tests pin it, because a "latest release" panel that quietly showed the
//! most *popular* release would be wrong in a way nobody would notice.

use serde::Deserialize;
use terminal_core::types::{Release, ReleaseList};

use crate::app::AppState;
use crate::cache::ttl;
use crate::error::{codes, Result, UpstreamError};
use crate::http::FetchOptions;

/// What to look for. Apple calls these entities; the terminal calls them kinds.
///
/// Music only. Apple's Search API still documents a `movie` entity and still
/// answers `200 OK` for one — with `resultCount: 0` for every title tried,
/// including Avatar and Inception, while the same request for music returns
/// normally. The film catalogue is gone from this endpoint, so `REL` does not
/// offer a film kind: an argument that always resolves to "not found" is worse
/// than not having the argument, and it would make the film release-date markets
/// look covered when they are not.
const KINDS: &[(&str, &str, &str)] = &[("album", "album", "Albums"), ("song", "song", "Songs")];

fn kind_of(name: &str) -> Option<(&'static str, &'static str, &'static str)> {
    KINDS.iter().copied().find(|(key, _, _)| *key == name)
}

pub fn assert_kind(raw: &str) -> Result<&'static str> {
    let lowered = raw.trim().to_lowercase();
    let lowered = if lowered.is_empty() {
        "album".to_string()
    } else {
        lowered
    };
    let normalised = lowered.strip_suffix('s').unwrap_or(&lowered);

    kind_of(normalised).map(|(key, _, _)| key).ok_or_else(|| {
        UpstreamError::bad_request(format!("\"{raw}\" is not a release kind"))
            .with_hint("Kinds are: album, song. Usage: `REL <artist> [album|song]`.")
    })
}

#[derive(Debug, Deserialize)]
struct RawBody {
    results: Option<Vec<RawResult>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawResult {
    collection_name: Option<String>,
    track_name: Option<String>,
    artist_name: Option<String>,
    release_date: Option<String>,
    track_count: Option<u32>,
    primary_genre_name: Option<String>,
    collection_view_url: Option<String>,
    track_view_url: Option<String>,
}

/// Shape one Apple result.
///
/// `collectionName` is the album, `trackName` the song, and a song result
/// carries both — so the title is chosen by what was asked for rather than by
/// whichever field happens to be populated.
fn to_release(raw: &RawResult, kind: &str) -> Option<Release> {
    let title = if kind == "song" {
        raw.track_name.clone()
    } else {
        raw.collection_name
            .clone()
            .or_else(|| raw.track_name.clone())
    }?;
    if title.is_empty() {
        return None;
    }

    let iso = raw.release_date.as_deref().unwrap_or("");
    let date: String = iso.chars().take(10).collect();
    // `YYYY-MM-DD`, or this is not a dated record and cannot answer a "when".
    let dated = date.len() == 10
        && date.as_bytes()[4] == b'-'
        && date.as_bytes()[7] == b'-'
        && date
            .bytes()
            .enumerate()
            .all(|(i, b)| matches!(i, 4 | 7) || b.is_ascii_digit());
    if !dated {
        return None;
    }

    Some(Release {
        title,
        artist: raw.artist_name.clone().unwrap_or_default(),
        date,
        kind: kind.to_string(),
        track_count: raw.track_count,
        genre: raw.primary_genre_name.clone().unwrap_or_default(),
        url: raw
            .collection_view_url
            .clone()
            .or_else(|| raw.track_view_url.clone())
            .unwrap_or_default(),
        // Filled in on the way out: it is a comparison against *now*, and this
        // value goes into a TTL cache. Baking it in at parse time would leave a
        // record that shipped this morning flagged unreleased until it expired.
        upcoming: false,
    })
}

/// Lowercase alphanumerics only, for comparing two spellings of a name.
fn fold(text: &str) -> String {
    text.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// Is this result actually by the artist that was asked for?
///
/// `attribute=artistTerm` narrows the search but does not close it: a live query
/// for "taylor swift" comes back with tribute acts, piano-cover projects and
/// unrelated artists among a hundred results, and because those release
/// constantly they sort to the *top* of a newest-first list. The panel would
/// then answer "what did Taylor Swift release most recently" with a covers
/// compilation.
///
/// Containment either way, so a collaboration credited to "Drake & Future" still
/// counts as a Drake release.
fn by_artist(artist: &str, query: &str) -> bool {
    let a = fold(artist);
    let q = fold(query);
    if a.is_empty() || q.is_empty() {
        return false;
    }
    a.contains(&q) || q.contains(&a)
}

/// Parse a search body into releases, newest first.
///
/// Three behaviours here are load-bearing and none fails loudly when it breaks:
/// the artist filter above, the sort (Apple returns relevance order whatever you
/// pass for `sort`), and the dedupe.
pub fn parse_releases(body: &str, kind: &str, artist: Option<&str>) -> Result<Vec<Release>> {
    let parsed: RawBody = serde_json::from_str(body).map_err(|err| {
        UpstreamError::new(
            format!("Apple's search response did not parse: {err}"),
            codes::BAD_UPSTREAM_BODY,
        )
    })?;

    let mut seen: Vec<String> = Vec::new();
    let mut releases: Vec<Release> = Vec::new();

    for raw in parsed.results.unwrap_or_default() {
        let Some(release) = to_release(&raw, kind) else {
            continue;
        };
        if let Some(name) = artist {
            if !by_artist(&release.artist, name) {
                continue;
            }
        }

        // Exact duplicates only — same title, same day, which is what a record
        // listed across storefronts looks like.
        //
        // Collapsing near-matches folds "1989" together with "1989 (Taylor's
        // Version)", and a re-recording is a separate release that separate
        // markets trade. Two rows that look alike are a cosmetic annoyance; a
        // hidden release is a wrong answer to "has it come out yet".
        let key = format!("{}|{}", release.title.trim().to_lowercase(), release.date);
        if seen.contains(&key) {
            continue;
        }
        seen.push(key);
        releases.push(release);
    }

    releases.sort_by(|a, b| b.date.cmp(&a.date));
    Ok(releases)
}

/// An artist's releases, newest first, with anything still ahead flagged.
pub async fn get_releases(
    state: &AppState,
    query: &str,
    kind: &str,
    limit: usize,
    today: &str,
) -> Result<ReleaseList> {
    let q = query.trim();
    if q.is_empty() {
        return Err(UpstreamError::bad_request("Missing artist")
            .with_hint("Usage: `REL <artist> [album|song]`, e.g. `REL taylor swift`."));
    }

    let kind = assert_kind(kind)?;
    let (_, entity, label) = kind_of(kind).expect("assert_kind returns a known kind");

    // `attribute=artistTerm` is what makes this a discography rather than a
    // keyword search. Without it "taylor swift" matches every covers
    // compilation with her name in the title, and the newest of those — not her
    // newest record — is what a "latest release" panel would show.
    let fetch = (limit * 4).clamp(50, 200);
    let source = format!(
        "{}/search?term={}&entity={entity}&attribute=artistTerm&limit={fetch}&country=US&media=music",
        state.config().itunes_api_base,
        urlencoding::encode(q),
    );

    let key = format!("releases:{kind}:{}", q.to_lowercase());
    let list = state
        .cache()
        .cached(&key, ttl::RELEASES, || async {
            let body = state
                .http()
                .fetch_text(
                    &source,
                    FetchOptions::new()
                        .timeout(std::time::Duration::from_secs(20))
                        .retries(2),
                )
                .await?;
            let releases = parse_releases(&body, kind, Some(q))?;

            if releases.is_empty() {
                return Err(UpstreamError::not_found(format!(
                    "Apple has no {kind} releases credited to \"{q}\""
                ))
                .with_hint(
                    "Check the artist spelling. Results are held to the artist named, so \
                     an album title searched by mistake finds nothing: try `REL <artist>`.",
                ));
            }

            Ok(ReleaseList {
                query: q.to_string(),
                kind: kind.to_string(),
                kind_label: label.to_string(),
                releases,
                source_url: format!(
                    "https://music.apple.com/us/search?term={}",
                    urlencoding::encode(q)
                ),
            })
        })
        .await?;

    let mut list = (*list).clone();
    list.releases.truncate(limit);
    for release in &mut list.releases {
        release.upcoming = release.date.as_str() > today;
    }
    Ok(list)
}

#[cfg(test)]
mod tests {
    //! The search response against a captured live one.

    use super::*;

    const TAYLOR: &str = include_str!("fixtures/itunes_search.json");

    #[test]
    fn sorts_newest_first_because_apple_answers_by_relevance() {
        let releases = parse_releases(TAYLOR, "album", Some("taylor swift")).expect("releases");
        assert!(releases.len() > 5);
        for pair in releases.windows(2) {
            assert!(
                pair[0].date >= pair[1].date,
                "{} came before {}",
                pair[0].date,
                pair[1].date
            );
        }
    }

    #[test]
    fn holds_results_to_the_artist_that_was_asked_for() {
        // `attribute=artistTerm` narrows the search and does not close it. The
        // live fixture carries tribute and cover acts, and they release
        // constantly — so unfiltered they head a newest-first list.
        let all = parse_releases(TAYLOR, "album", None).expect("releases");
        let held = parse_releases(TAYLOR, "album", Some("taylor swift")).expect("releases");

        assert!(
            held.len() < all.len(),
            "the fixture has no foreign artists to filter"
        );
        assert!(held.iter().all(|r| {
            let a = fold(&r.artist);
            a.contains("taylorswift") || "taylorswift".contains(&a)
        }));
    }

    #[test]
    fn counts_a_collaboration_as_the_artists_own_release() {
        assert!(by_artist("Taylor Swift & Chris Lake", "taylor swift"));
        assert!(by_artist("Drake", "Drake & Future"));
        assert!(!by_artist("The Piano Guys", "taylor swift"));
        assert!(!by_artist("", "taylor swift"));
    }

    #[test]
    fn keeps_a_re_recording_apart_from_the_record_it_re_records() {
        // Collapsing "1989" and "1989 (Taylor's Version)" would hide a release
        // that separate markets trade.
        let body = r#"{"results":[
          {"collectionName":"1989","artistName":"Taylor Swift","releaseDate":"2014-10-27T07:00:00Z"},
          {"collectionName":"1989 (Taylor's Version)","artistName":"Taylor Swift","releaseDate":"2023-10-27T07:00:00Z"}
        ]}"#;
        let releases = parse_releases(body, "album", Some("taylor swift")).expect("releases");
        assert_eq!(releases.len(), 2);
    }

    #[test]
    fn drops_one_of_two_identical_listings() {
        let body = r#"{"results":[
          {"collectionName":"Midnights","artistName":"Taylor Swift","releaseDate":"2022-10-21T07:00:00Z"},
          {"collectionName":"midnights","artistName":"Taylor Swift","releaseDate":"2022-10-21T07:00:00Z"}
        ]}"#;
        let releases = parse_releases(body, "album", Some("taylor swift")).expect("releases");
        assert_eq!(releases.len(), 1);
    }

    #[test]
    fn drops_a_result_with_no_usable_date() {
        // A record with no date cannot answer "has it come out yet", which is
        // the only question this feed exists for.
        let body = r#"{"results":[
          {"collectionName":"Undated","artistName":"Taylor Swift"},
          {"collectionName":"Bad","artistName":"Taylor Swift","releaseDate":"soon"},
          {"collectionName":"Good","artistName":"Taylor Swift","releaseDate":"2026-01-02T00:00:00Z"}
        ]}"#;
        let releases = parse_releases(body, "album", Some("taylor swift")).expect("releases");
        assert_eq!(releases.len(), 1);
        assert_eq!(releases[0].date, "2026-01-02");
    }

    #[test]
    fn takes_the_track_name_for_a_song_and_the_collection_for_an_album() {
        let body = r#"{"results":[{
          "collectionName":"Midnights","trackName":"Anti-Hero",
          "artistName":"Taylor Swift","releaseDate":"2022-10-21T07:00:00Z"
        }]}"#;
        let album = parse_releases(body, "album", None).expect("releases");
        let song = parse_releases(body, "song", None).expect("releases");
        assert_eq!(album[0].title, "Midnights");
        assert_eq!(song[0].title, "Anti-Hero");
    }

    #[test]
    fn names_the_kinds_it_takes() {
        assert_eq!(assert_kind("").unwrap(), "album");
        assert_eq!(assert_kind("albums").unwrap(), "album");
        assert_eq!(assert_kind("SONG").unwrap(), "song");
        let err = assert_kind("movie").expect_err("Apple's film catalogue is gone");
        assert_eq!(err.code, codes::BAD_REQUEST);
    }
}
