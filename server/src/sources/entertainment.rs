//! Kalshi's entertainment universe, grouped into genres.
//!
//! Kalshi files ~2,500 series under the `Entertainment` category and ~545 of
//! them have open events at any time, but the API offers no sub-category: the
//! `/events` endpoint accepts a `category` parameter and ignores it, so the only
//! discriminator available is the ticker itself. Kalshi's tickers are
//! disciplined — `KXOSCARPIC`, `KXNETFLIXRANKSHOW`, `KXGAMEAWARDS` — so a prefix
//! table classifies them reliably, with title keywords as a backstop for the
//! handful of one-off series that do not follow the convention.
//!
//! Genres are *tags*, not a partition. An Oscar market is both `film` and
//! `awards`, and someone typing `ENT film` expects to see it.
//!
//! The second half of the table is the interesting one: [`FEEDS`] maps a series
//! to the terminal command that shows the data the market settles against. Those
//! pairings are not guesses — they come from each series' own
//! `settlement_sources`, which name the upstream Kalshi resolves against.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use regex::Regex;
use terminal_core::types::{EntEvent, EntFeed, EntGenre, EntGenreFilter, EntResponse, VenueEvent};

use crate::app::AppState;
use crate::error::{Result, UpstreamError};
use crate::sources::corpus::{sum_or_null, Corpus};

const CATEGORY: &str = "entertainment";

/// How many events `ENT` returns when nobody says.
pub const DEFAULT_LIMIT: usize = 60;

use EntGenre::{Awards, Celeb, Film, Games, Music, Tv};

/// Series-ticker prefix → genres.
///
/// Matched longest-prefix-first, so `KXGAMEAWARDS` resolves to games+awards
/// rather than stopping at the shorter `KXGAME` rule.
const PREFIX_GENRES: &[(&str, &[EntGenre])] = &[
    // ---- music -------------------------------------------------------------
    ("KXALBUM", &[Music]),
    ("KXPUREALBUMS", &[Music]),
    ("KXALBUMEQUIV", &[Music]),
    ("KXARTISTSTREAMS", &[Music]),
    ("KXTOPARTIST", &[Music]),
    ("KXTOPSONG", &[Music]),
    ("KXTOPALBUM", &[Music]),
    ("KXTOPMONTHLY", &[Music]),
    ("KX1ALBUM", &[Music]),
    ("KX1SONG", &[Music]),
    ("KX10SONG", &[Music]),
    ("KX20SONG", &[Music]),
    ("KXBILLBOARD", &[Music]),
    ("KXWEEKSNUM1", &[Music]),
    ("KXSONGRELEASE", &[Music]),
    ("KXTOUR", &[Music]),
    ("KXHEADLINE", &[Music]),
    ("KXPERFORMSUPERBOWL", &[Music]),
    ("KXSUPERBOWLHEADLINE", &[Music]),
    ("KXROLEATEVENTCOACHELLA", &[Music]),
    ("KXROLEATEVENT", &[Music]),
    ("KXEUROVISION", &[Music]),
    ("KXYT", &[Music]),
    ("KXRANKLISTSONG", &[Music]),
    ("KXLEAVEGROUP", &[Music]),
    ("KXSPOTIFY", &[Music]),
    ("KXNEWTAYLOR", &[Music]),
    // ---- film --------------------------------------------------------------
    ("KXRT", &[Film]),
    ("KXRTCOMPARE", &[Film]),
    ("KXBOND", &[Film]),
    ("KXMOVIE", &[Film]),
    ("KXMOVIECAST", &[Film]),
    ("KXMOVIEDELAY", &[Film]),
    ("KXMOVIERELEASEDATE", &[Film]),
    ("KXROLEINPRODUCTION", &[Film]),
    ("KXPERFORMROLE", &[Film]),
    ("KXVFILMFESTIVAL", &[Film, Awards]),
    ("KXAVENGERS", &[Film]),
    ("KXIRONMAN", &[Film]),
    ("KXGGBOXOFFICE", &[Film, Awards]),
    // ---- tv ----------------------------------------------------------------
    ("KXBIGBROTHER", &[Tv]),
    ("KXNETFLIX", &[Tv]),
    ("KXDWTS", &[Tv]),
    ("KXSNL", &[Tv]),
    ("KXSUMMERHOUSE", &[Tv]),
    ("KXTVSHOWSCANCELLED", &[Tv]),
    ("KXMEDIAGUEST", &[Tv]),
    ("KXMEDIARELEASE", &[Tv]),
    ("KX60MINUTES", &[Tv]),
    ("KXSPINOFF", &[Tv]),
    ("KXLOVEISLAND", &[Tv]),
    ("KXUPONLY", &[Tv]),
    // ---- games -------------------------------------------------------------
    ("KXGAME", &[Games]),
    ("KXGAMEAWARDS", &[Games, Awards]),
    ("KXGAMERELEASE", &[Games]),
    ("KXGTA", &[Games]),
    ("KXESVI", &[Games]),
    ("KXPS6", &[Games]),
    ("KXSTEAM", &[Games, Awards]),
    ("KXPOKEMON", &[Games]),
    ("KXVIDEOLENGTH", &[Games]),
    ("KXMETACRITIC", &[Games]),
    ("GAMERANK", &[Games]),
    ("KXANIME", &[Games, Awards]),
    // ---- awards ------------------------------------------------------------
    ("KXOSCAR", &[Film, Awards]),
    ("KXEMMY", &[Tv, Awards]),
    ("KXGRAM", &[Music, Awards]),
    ("KXLGRAM", &[Music, Awards]),
    ("KXBAFTA", &[Film, Awards]),
    ("KXGOLDENGLOBE", &[Film, Awards]),
    ("KXCRITICS", &[Film, Awards]),
    ("KXAMA", &[Music, Awards]),
    ("KXACMA", &[Music, Awards]),
    ("KXTIME", &[Awards, Celeb]),
    ("KXSEXYMAN", &[Awards, Celeb]),
    ("KXWORDOFTHEYEAR", &[Awards]),
    // ---- celebrity / culture ------------------------------------------------
    ("KXSWIFT", &[Celeb, Music]),
    ("KXKIMK", &[Celeb]),
    ("KXENGAGEMENT", &[Celeb]),
    ("KXFOLLOWERCOUNT", &[Celeb]),
    ("KXTWITCHSUBS", &[Celeb, Games]),
    ("KXRANKLISTGOOGLESEARCH", &[Celeb]),
    ("KXMEDIACOVER", &[Celeb]),
    ("KXPERSON", &[Celeb]),
    ("KXHERMES", &[Celeb]),
    ("KXART", &[Celeb]),
];

/// Fallback when the ticker is not in the table: match on the event title.
const TITLE_GENRES: &[(&str, &[EntGenre])] = &[
    (
        r"(?i-u)\b(album|song|single|billboard|spotify|streams?|tour|concert|headlin|grammy)\b",
        &[Music],
    ),
    (
        r"(?i-u)\b(box office|rotten tomatoes|film|movie|oscar|cast as|screenplay|director)\b",
        &[Film],
    ),
    (
        r"(?i-u)\b(netflix|episode|season|series|emmy|show|tv|premiere)\b",
        &[Tv],
    ),
    (
        r"(?i-u)\b(game|steam|nintendo|playstation|xbox|console|dlc|speedrun)\b",
        &[Games],
    ),
    (r"(?i-u)\b(award|nominee|winner|nomination)\b", &[Awards]),
];

/// Series → the command that shows what the market settles against.
///
/// Read off each series' `settlement_sources`. `KXRT` settles on
/// rottentomatoes.com, so `RT` is the companion command; `KXNETFLIXRANKSHOW`
/// settles on Netflix's own Top 10 publication, so `NFLX` is. Longest prefix
/// wins, same as the genre table.
const FEEDS: &[(&str, (&str, &str))] = &[
    ("KXRT", ("RT", "Rotten Tomatoes")),
    ("KXRTCOMPARE", ("RT", "Rotten Tomatoes")),
    ("KXNETFLIX", ("NFLX", "Netflix Top 10")),
    ("KXTOPARTIST", ("SPOT", "Spotify charts")),
    ("KXTOPSONGSPOTIFY", ("SPOT", "Spotify charts")),
    ("KXTOPALBUMSPOTIFY", ("SPOT", "Spotify charts")),
    ("KXARTISTSTREAMS", ("SPOT", "Spotify charts")),
    ("KXTOPMONTHLY", ("SPOT", "Spotify charts")),
    ("KXSPOTIFY", ("SPOT", "Spotify charts")),
    ("KXYT", ("YT", "YouTube charts")),
    ("KXTOPSONG", ("BB hot-100", "Billboard Hot 100")),
    ("KXTOPALBUM", ("BB billboard-200", "Billboard 200")),
    ("KXBILLBOARD", ("BB hot-100", "Billboard")),
    ("KX1SONG", ("BB hot-100", "Billboard Hot 100")),
    ("KX10SONG", ("BB hot-100", "Billboard Hot 100")),
    ("KX20SONG", ("BB hot-100", "Billboard Hot 100")),
    ("KX1ALBUM", ("BB billboard-200", "Billboard 200")),
    ("KXWEEKSNUM1", ("BB hot-100", "Billboard Hot 100")),
    ("KXALBUMEQUIV", ("BB billboard-200", "Luminate / Billboard")),
    ("KXPUREALBUMS", ("BB billboard-200", "Luminate / Billboard")),
    ("KXRANKLISTSONG", ("BB hot-100", "Billboard Hot 100")),
    ("KXSTEAM", ("STEAM", "Steam")),
    ("GAMERANK", ("STEAM", "Steam")),
    ("KXGGBOXOFFICE", ("BO", "Box Office Mojo")),
    ("KXBIGBROTHER", ("TV", "TV schedule")),
    ("KXDWTS", ("TV", "TV schedule")),
    ("KXSNL", ("TV", "TV schedule")),
];

/// Longest matching prefix in `table`, or `None`.
fn longest_prefix<'a, T>(ticker: &str, table: &'a [(&str, T)]) -> Option<&'a T> {
    let mut best: Option<(&str, &T)> = None;
    for (key, value) in table {
        if !ticker.starts_with(key) {
            continue;
        }
        if best.is_none_or(|(current, _)| key.len() > current.len()) {
            best = Some((key, value));
        }
    }
    best.map(|(_, value)| value)
}

/// The title patterns, compiled once.
fn title_genres() -> &'static [(Regex, &'static [EntGenre])] {
    static COMPILED: OnceLock<Vec<(Regex, &'static [EntGenre])>> = OnceLock::new();
    COMPILED.get_or_init(|| {
        TITLE_GENRES
            .iter()
            .map(|(pattern, genres)| {
                (
                    Regex::new(pattern).expect("title genre pattern is valid"),
                    *genres,
                )
            })
            .collect()
    })
}

/// Genres for one event.
///
/// Public for the tests: the classification is the whole value of this module,
/// and a silent misclassification (Oscars filed under `music`) is exactly the
/// kind of bug that never surfaces as an error.
pub fn classify(series_ticker: &str, title: &str) -> Vec<EntGenre> {
    let ticker = series_ticker.to_uppercase();
    if let Some(from_prefix) = longest_prefix(&ticker, PREFIX_GENRES) {
        return from_prefix.to_vec();
    }

    let mut tags: Vec<EntGenre> = Vec::new();
    for (pattern, genres) in title_genres() {
        if pattern.is_match(title) {
            for genre in *genres {
                if !tags.contains(genre) {
                    tags.push(*genre);
                }
            }
        }
    }
    // Everything under Kalshi's Entertainment category is *something*; an event
    // that matches nothing is still worth listing rather than silently dropping.
    if tags.is_empty() {
        vec![Celeb]
    } else {
        tags
    }
}

/// The companion data command for a series, if this terminal has one.
pub fn feed_for(series_ticker: &str) -> Option<EntFeed> {
    longest_prefix(&series_ticker.to_uppercase(), FEEDS).map(|(command, source)| EntFeed {
        command: (*command).to_string(),
        source: (*source).to_string(),
    })
}

/// Read a genre argument, or say what the genres are.
pub fn assert_genre(raw: &str) -> Result<EntGenreFilter> {
    let value = raw.trim().to_lowercase();
    if value.is_empty() || value == "all" {
        return Ok(EntGenreFilter::All);
    }
    value.parse::<EntGenre>().map(Into::into).map_err(|()| {
        let names: Vec<&str> = EntGenre::ALL.iter().map(|g| g.as_str()).collect();
        UpstreamError::bad_request(format!("\"{raw}\" is not an entertainment genre"))
            .with_hint(format!("Genres are: {}, or ALL.", names.join(", ")))
    })
}

/// Shape one corpus event into the entertainment view.
pub fn to_ent_event(event: &VenueEvent) -> EntEvent {
    let genres = classify(
        &event.series_ticker,
        &format!("{} {}", event.title, event.sub_title),
    );
    let mut markets = event.markets.clone();
    markets.sort_by(|a, b| {
        b.volume24h
            .unwrap_or(0.0)
            .partial_cmp(&a.volume24h.unwrap_or(0.0))
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    // The soonest close is the one a trader is actually racing; an event whose
    // legs close on different days should show the nearest, not an arbitrary one.
    let mut closes: Vec<&str> = markets
        .iter()
        .map(|m| m.close_time.as_str())
        .filter(|t| !t.is_empty())
        .collect();
    closes.sort_unstable();

    EntEvent {
        event_ticker: event.event_ticker.clone(),
        series_ticker: event.series_ticker.clone(),
        title: event.title.clone(),
        sub_title: event.sub_title.clone(),
        genres,
        // This view is Kalshi-only, and Kalshi publishes both figures on every
        // market, so the fallback is unreachable rather than a silent zero.
        volume24h: sum_or_null(&markets, |m| m.volume24h).unwrap_or(0.0),
        open_interest: sum_or_null(&markets, |m| m.open_interest).unwrap_or(0.0),
        close_time: closes.first().copied().unwrap_or("").to_string(),
        feed: feed_for(&event.series_ticker),
        markets,
    }
}

/// Open entertainment events in a snapshot, optionally narrowed to one genre.
pub fn browse_snapshot(snapshot: &Corpus, genre: EntGenreFilter, limit: usize) -> EntResponse {
    let all: Vec<EntEvent> = snapshot
        .events
        .iter()
        .filter(|e| e.category.to_lowercase() == CATEGORY)
        .map(to_ent_event)
        .collect();

    let mut counts: BTreeMap<String, u32> = BTreeMap::new();
    counts.insert("all".to_string(), all.len() as u32);
    for g in EntGenre::ALL {
        let count = all.iter().filter(|e| e.genres.contains(g)).count();
        counts.insert(g.as_str().to_string(), count as u32);
    }

    let mut events: Vec<EntEvent> = match genre.genre() {
        Some(wanted) => all
            .into_iter()
            .filter(|e| e.genres.contains(&wanted))
            .collect(),
        None => all,
    };
    events.sort_by(|a, b| {
        b.volume24h
            .partial_cmp(&a.volume24h)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                b.open_interest
                    .partial_cmp(&a.open_interest)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    });
    events.truncate(limit);

    EntResponse {
        genre,
        events,
        scanned: snapshot.events.len() as u32,
        snapshot_age_seconds: snapshot.age_seconds(),
        counts,
    }
}

/// Open entertainment events, optionally narrowed to one genre.
///
/// Reads the shared open-event snapshot rather than crawling: `SRCH` and `TOP`
/// already keep it warm, and it is the only view of the universe that excludes
/// Kalshi's auto-generated parlay legs.
pub async fn browse(state: &AppState, genre: EntGenreFilter, limit: usize) -> Result<EntResponse> {
    let snapshot = crate::sources::kalshi::corpus_snapshot(state).await?;
    Ok(browse_snapshot(&snapshot, genre, limit))
}

#[cfg(test)]
mod tests {
    use super::*;

    use terminal_core::types::{Market, Venue};

    fn market(ticker: &str) -> Market {
        Market {
            venue: Venue::Kalshi,
            ticker: ticker.to_string(),
            event_ticker: "E".into(),
            series_ticker: "S".into(),
            title: "t".into(),
            yes_sub_title: String::new(),
            no_sub_title: String::new(),
            status: "open".into(),
            market_type: "binary".into(),
            yes_bid: None,
            yes_ask: None,
            no_bid: None,
            no_ask: None,
            mid: None,
            last_price: None,
            previous_price: None,
            change: None,
            volume: Some(0.0),
            volume24h: Some(0.0),
            open_interest: Some(0.0),
            liquidity: Some(0.0),
            open_time: String::new(),
            close_time: String::new(),
            expiration_time: String::new(),
            result: String::new(),
            rules_primary: String::new(),
            category: None,
            strike_type: None,
            floor_strike: None,
            cap_strike: None,
        }
    }

    fn event() -> VenueEvent {
        VenueEvent {
            venue: Venue::Kalshi,
            event_ticker: "KXRT-DUNE".into(),
            series_ticker: "KXRT".into(),
            title: "Dune: Part Three · Rotten Tomatoes score".into(),
            sub_title: String::new(),
            category: "Entertainment".into(),
            mutually_exclusive: true,
            markets: vec![
                Market {
                    volume24h: Some(10.0),
                    open_interest: Some(5.0),
                    close_time: "2026-12-20T00:00:00Z".into(),
                    ..market("A")
                },
                Market {
                    volume24h: Some(90.0),
                    open_interest: Some(15.0),
                    close_time: "2026-09-01T00:00:00Z".into(),
                    ..market("B")
                },
            ],
        }
    }

    /* -------------------------------------------------------------- classify */

    #[test]
    fn files_the_core_music_series_under_music() {
        for ticker in [
            "KXTOPARTIST",
            "KXARTISTSTREAMSY",
            "KX1ALBUM",
            "KXBILLBOARDRUNNERUPSONG",
        ] {
            assert_eq!(classify(ticker, ""), [Music], "{ticker}");
        }
    }

    #[test]
    fn files_rotten_tomatoes_and_casting_series_under_film() {
        assert_eq!(classify("KXRT", ""), [Film]);
        assert_eq!(classify("KXROLEINPRODUCTIONDOOMSDAY", ""), [Film]);
    }

    #[test]
    fn files_netflix_and_reality_series_under_tv() {
        assert_eq!(classify("KXNETFLIXRANKSHOW", ""), [Tv]);
        assert_eq!(classify("KXBIGBROTHERELIMINATION", ""), [Tv]);
    }

    #[test]
    fn tags_award_series_with_both_their_medium_and_awards() {
        assert_eq!(classify("KXOSCARPIC", ""), [Film, Awards]);
        assert_eq!(classify("KXEMMYCSERIES", ""), [Tv, Awards]);
        assert_eq!(classify("KXGRAMAOTY", ""), [Music, Awards]);
    }

    #[test]
    fn prefers_the_longest_matching_prefix() {
        // `KXGAMEAWARDS` also starts with `KXGAME`; stopping at the shorter rule
        // would drop the awards tag and hide it from `ENT awards`.
        assert_eq!(classify("KXGAMEAWARDS", ""), [Games, Awards]);
        assert_eq!(classify("KXGAMERELEASE", ""), [Games]);
    }

    #[test]
    fn falls_back_to_the_title_when_the_ticker_is_unknown() {
        assert_eq!(
            classify("ZZUNKNOWN", "Who will win Album of the Year"),
            [Music]
        );
        assert!(classify("ZZUNKNOWN", "Rotten Tomatoes score for the film").contains(&Film));
    }

    #[test]
    fn a_non_ascii_title_still_classifies() {
        // Kalshi titles carry `·` separators and accented names, and the title
        // patterns read a word boundary the way JavaScript did — as bytes.
        assert_eq!(
            classify("ZZUNKNOWN", "Amélie · Rotten Tomatoes score"),
            [Film]
        );
    }

    #[test]
    fn never_returns_an_empty_tag_list() {
        // Everything under Kalshi's Entertainment category belongs somewhere; an
        // unclassifiable event must still be listable rather than silently dropped.
        assert_eq!(classify("ZZZ", "something entirely unparseable"), [Celeb]);
    }

    #[test]
    fn is_case_insensitive_on_the_ticker() {
        assert_eq!(classify("kxoscarpic", ""), [Film, Awards]);
    }

    /* --------------------------------------------------------------- feedFor */

    #[test]
    fn points_each_series_at_the_command_that_shows_its_settlement_data() {
        assert_eq!(feed_for("KXRT").map(|f| f.command).as_deref(), Some("RT"));
        assert_eq!(
            feed_for("KXNETFLIXRANKSHOW").map(|f| f.command).as_deref(),
            Some("NFLX")
        );
        assert_eq!(
            feed_for("KXARTISTSTREAMSY").map(|f| f.command).as_deref(),
            Some("SPOT")
        );
        assert_eq!(
            feed_for("KXYTVIEWSW").map(|f| f.command).as_deref(),
            Some("YT")
        );
        assert_eq!(
            feed_for("KXSTEAMGOTY").map(|f| f.command).as_deref(),
            Some("STEAM")
        );
    }

    #[test]
    fn resolves_billboard_series_to_the_right_chart() {
        assert_eq!(
            feed_for("KXTOPSONG").map(|f| f.command).as_deref(),
            Some("BB hot-100")
        );
        assert_eq!(
            feed_for("KXTOPALBUM").map(|f| f.command).as_deref(),
            Some("BB billboard-200")
        );
    }

    #[test]
    fn returns_none_for_a_series_with_no_terminal_feed() {
        assert!(feed_for("KXSEXYMAN").is_none());
    }

    /* ----------------------------------------------------------- assertGenre */

    #[test]
    fn accepts_the_known_genres_and_normalises_case() {
        assert_eq!(assert_genre("MUSIC").unwrap(), EntGenreFilter::Music);
        assert_eq!(assert_genre("games").unwrap(), EntGenreFilter::Games);
    }

    #[test]
    fn treats_empty_and_all_as_everything() {
        assert_eq!(assert_genre("").unwrap(), EntGenreFilter::All);
        assert_eq!(assert_genre("ALL").unwrap(), EntGenreFilter::All);
    }

    #[test]
    fn rejects_anything_else_with_a_usable_hint() {
        let err = assert_genre("sports").unwrap_err();
        assert!(
            err.message.contains("not an entertainment genre"),
            "{err:?}"
        );
        assert!(err.hint.is_some_and(|h| h.contains("music")));
    }

    /* ------------------------------------------------------------ toEntEvent */

    #[test]
    fn sorts_markets_most_liquid_first() {
        let tickers: Vec<String> = to_ent_event(&event())
            .markets
            .into_iter()
            .map(|m| m.ticker)
            .collect();
        assert_eq!(tickers, ["B", "A"]);
    }

    #[test]
    fn sums_volume_and_open_interest_across_the_event() {
        let ent_event = to_ent_event(&event());
        assert_eq!(ent_event.volume24h, 100.0);
        assert_eq!(ent_event.open_interest, 20.0);
    }

    #[test]
    fn reports_the_soonest_close_not_an_arbitrary_one() {
        // The nearest deadline is the one a trader is racing.
        assert_eq!(to_ent_event(&event()).close_time, "2026-09-01T00:00:00Z");
    }

    #[test]
    fn attaches_the_settlement_feed_for_the_series() {
        assert_eq!(
            to_ent_event(&event()).feed.map(|f| f.source).as_deref(),
            Some("Rotten Tomatoes")
        );
    }

    #[test]
    fn omits_the_feed_entirely_when_there_is_none() {
        let orphan = VenueEvent {
            series_ticker: "KXSEXYMAN".into(),
            event_ticker: "KXSEXYMAN-26".into(),
            ..event()
        };
        assert!(to_ent_event(&orphan).feed.is_none());
    }

    #[test]
    fn survives_an_event_with_no_markets() {
        let empty = to_ent_event(&VenueEvent {
            markets: vec![],
            ..event()
        });
        assert_eq!(empty.volume24h, 0.0);
        assert_eq!(empty.close_time, "");
    }

    /* --------------------------------------------------------------- browse */

    fn snapshot() -> Corpus {
        let events = vec![
            VenueEvent {
                event_ticker: "KXOSCARPIC-26".into(),
                series_ticker: "KXOSCARPIC".into(),
                title: "Best Picture".into(),
                markets: vec![Market {
                    volume24h: Some(50.0),
                    open_interest: Some(1.0),
                    ..market("O")
                }],
                ..event()
            },
            VenueEvent {
                event_ticker: "KXTOPSONG-26".into(),
                series_ticker: "KXTOPSONG".into(),
                title: "Hot 100 number one".into(),
                markets: vec![Market {
                    volume24h: Some(500.0),
                    open_interest: Some(1.0),
                    ..market("S")
                }],
                ..event()
            },
            VenueEvent {
                event_ticker: "KXFED-26".into(),
                series_ticker: "KXFED".into(),
                title: "Fed decision".into(),
                category: "Economics".into(),
                markets: vec![],
                ..event()
            },
        ];
        let markets = events.iter().flat_map(|e| e.markets.clone()).collect();
        Corpus::new(Venue::Kalshi, events, markets, false)
    }

    #[test]
    fn browse_keeps_only_the_entertainment_category() {
        let response = browse_snapshot(&snapshot(), EntGenreFilter::All, DEFAULT_LIMIT);
        let tickers: Vec<&str> = response
            .events
            .iter()
            .map(|e| e.event_ticker.as_str())
            .collect();
        assert_eq!(tickers, ["KXTOPSONG-26", "KXOSCARPIC-26"]);
        // Everything in the snapshot was scanned, entertainment or not.
        assert_eq!(response.scanned, 3);
    }

    #[test]
    fn browse_narrows_to_one_genre_but_counts_them_all() {
        let response = browse_snapshot(&snapshot(), EntGenreFilter::Awards, DEFAULT_LIMIT);
        assert_eq!(response.genre, EntGenreFilter::Awards);
        assert_eq!(
            response
                .events
                .iter()
                .map(|e| e.event_ticker.as_str())
                .collect::<Vec<_>>(),
            ["KXOSCARPIC-26"]
        );
        // An Oscars event is both film and awards, so it is counted under both.
        assert_eq!(response.counts["all"], 2);
        assert_eq!(response.counts["awards"], 1);
        assert_eq!(response.counts["film"], 1);
        assert_eq!(response.counts["music"], 1);
        assert_eq!(response.counts["tv"], 0);
    }

    #[test]
    fn browse_is_capped_at_the_limit() {
        assert_eq!(
            browse_snapshot(&snapshot(), EntGenreFilter::All, 1)
                .events
                .len(),
            1
        );
    }
}
