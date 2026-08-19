//! Steam player counts — the live half of Kalshi's video-game book.
//!
//! `KXSTEAM*` (Game of the Year, Labor of Love and the rest of the Steam Awards)
//! and `GAMERANK` settle on Steam itself, and concurrent-player counts are the
//! public number those markets move on. Release-date markets — `KXGTA6`,
//! `KXESVI`, `KXPS6` — trade on the same catalogue.
//!
//! Two upstreams, each used for what it is actually good at:
//!
//!   api.steampowered.com   Valve's own API. Authoritative live player count for
//!                          a given appid, no key required for this endpoint.
//!   steamcharts.com        The most-played leaderboard with *names* attached.
//!
//! Valve publishes a most-played endpoint too, but it returns bare appids, so
//! rendering a leaderboard from it would cost one store lookup per row. The
//! scraped table carries rank, name, current and peak together in one request.

use std::sync::LazyLock;
use std::time::Duration;

use regex::Regex;
use serde::Deserialize;
use serde_json::Value;
use terminal_core::types::{SteamChart, SteamGame};

use crate::app::AppState;
use crate::cache::ttl;
use crate::css;
use crate::error::{Result, UpstreamError};
use crate::http::FetchOptions;
use crate::scrape::{self, Table};

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

fn int_or_null(text: Option<&str>) -> Option<i64> {
    let text = text?;
    if text.is_empty() {
        return None;
    }
    let cleaned: String = text
        .chars()
        .filter(|ch| !(*ch == ',' || ch.is_whitespace()))
        .collect();
    if cleaned.is_empty() || cleaned == "-" {
        return None;
    }
    js_parse_int(&cleaned)
}

/// A player figure. Anything that will not fit is no figure at all rather than
/// a wrapped one.
fn players_or_null(text: Option<&str>) -> Option<u64> {
    int_or_null(text).and_then(|n| u64::try_from(n).ok())
}

/// A row's own one-based position, for a table that states no rank of its own.
fn position(rows_so_far: usize) -> u32 {
    u32::try_from(rows_so_far + 1).unwrap_or(u32::MAX)
}

/// The ceiling Valve's app ids sit well under; a number above it is a typo, not
/// an app.
pub const MAX_APP_ID: u32 = 100_000_000;

pub fn assert_app_id(raw: &str) -> Result<u32> {
    js_parse_int(raw.trim())
        .and_then(|n| u32::try_from(n).ok())
        .filter(|id| *id > 0 && *id <= MAX_APP_ID)
        .ok_or_else(|| {
            UpstreamError::bad_request(format!("\"{raw}\" is not a valid Steam app id"))
                .with_hint("Pass a numeric app id, or a game name: `STEAM counter-strike`.")
        })
}

/* ------------------------------------------------------ most-played table */

/// The app id in a row's own link: `/app/730` → 730.
static APP_HREF: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"/app/(\d+)").expect("APP_HREF is a valid regex"));

/// Parse the most-played table.
///
/// The leading rank column has a blank header, so it is taken positionally;
/// everything else is resolved by header text. The app id comes from the row's
/// own link (`/app/730`), which is what makes each row clickable through to a
/// live count.
pub fn parse_top_page(html: &str, source_url: &str) -> Result<SteamChart> {
    let doc = scrape::parse_document(html);

    let table = Table::require_first_in(&doc, "games table", source_url)
        .map_err(|err| err.with_hint("steamcharts.com answered but served no leaderboard table."))?
        .with_positional_headers(&["rank"]);

    let mut games: Vec<SteamGame> = Vec::new();

    for row in table.rows() {
        let name = row.first_of(&["name", "game"]).unwrap_or_default();
        if name.is_empty() {
            continue;
        }

        let href = scrape::attr_of_first(row.element(), css!("a[href*=\"/app/\"]"), "href")
            .unwrap_or_default();
        let app_id = APP_HREF
            .captures(href)
            .and_then(|caps| int_or_null(Some(&caps[1])))
            .and_then(|id| u32::try_from(id).ok());

        // Ranks render as `1.`; the trailing dot is not part of the number.
        let rank = row.get("rank").unwrap_or_default().replace('.', "");

        games.push(SteamGame {
            app_id: app_id.unwrap_or(0),
            name: name.to_owned(),
            rank: Some(
                int_or_null(Some(&rank))
                    .and_then(|rank| u32::try_from(rank).ok())
                    .unwrap_or_else(|| position(games.len())),
            ),
            current_players: players_or_null(row.get("currentplayers")),
            peak_players: players_or_null(row.get("peakplayers")),
        });
    }

    if games.is_empty() {
        return Err(
            UpstreamError::parse_failed(format!("No games found on {source_url}")).with_hint(
                "steamcharts.com returned a page but its rows did not match the expected \
                 table shape. The layout may have changed.",
            ),
        );
    }

    Ok(SteamChart {
        view: "top".to_owned(),
        games,
        source_url: source_url.to_owned(),
    })
}

/// Rows the leaderboard answers with when the caller names no limit.
pub const DEFAULT_TOP_LIMIT: u32 = 25;

pub async fn get_top(state: &AppState, limit: u32) -> Result<SteamChart> {
    let source_url = format!("{}/top", state.config().steamcharts_base);

    let chart = state
        .cache()
        .cached("steam:top", ttl::STEAM, || async {
            let html = state
                .http()
                .fetch_text(
                    &source_url,
                    FetchOptions::new()
                        .timeout(Duration::from_secs(30))
                        .retries(2),
                )
                .await?;
            parse_top_page(&html, &source_url)
        })
        .await?;

    let mut chart = (*chart).clone();
    chart.games.truncate(limit as usize);
    Ok(chart)
}

/* ---------------------------------------------------------- live counts */

#[derive(Debug, Default, Deserialize)]
struct PlayerCountBody {
    #[serde(default)]
    response: Option<PlayerCountResponse>,
}

#[derive(Debug, Default, Deserialize)]
struct PlayerCountResponse {
    #[serde(default)]
    player_count: Option<Value>,
}

/// Valve's live concurrent-player count for one app.
pub async fn get_player_count(state: &AppState, app_id: u32) -> Result<Option<u64>> {
    let id = assert_app_id(&app_id.to_string())?;
    let url = format!(
        "{}/ISteamUserStats/GetNumberOfCurrentPlayers/v1/?appid={id}",
        state.config().steam_api_base
    );

    let count = state
        .cache()
        .cached(&format!("steam:players:{id}"), ttl::STEAM, || async {
            let body: PlayerCountBody = state
                .http()
                .fetch_json(
                    &url,
                    FetchOptions::new()
                        .timeout(Duration::from_secs(15))
                        .retries(2),
                )
                .await?;
            // Valve answers `result: 1` for a real app and omits the count for one that
            // reports no stats, which is not an error — it is "no live figure".
            let count = body
                .response
                .and_then(|response| response.player_count)
                .as_ref()
                .and_then(Value::as_f64)
                .filter(|count| count.is_finite() && *count >= 0.0)
                .map(|count| count as u64);
            Ok(count)
        })
        .await?;

    Ok(*count)
}

/* --------------------------------------------------------- store search */

#[derive(Debug, Default, Deserialize)]
struct StoreSearchBody {
    #[serde(default)]
    items: Option<Vec<StoreSearchItem>>,
}

#[derive(Debug, Default, Deserialize)]
struct StoreSearchItem {
    #[serde(default)]
    name: Value,
    #[serde(default)]
    id: Value,
}

/// Resolve a game name to apps, most relevant first.
///
/// The store's own search endpoint is used rather than the full app list, which
/// is a ~10 MB document listing every app Valve has ever shipped.
pub async fn search_games(state: &AppState, query: &str, limit: usize) -> Result<Vec<SteamGame>> {
    let q = query.trim();
    if q.is_empty() {
        return Err(UpstreamError::bad_request("Missing game name")
            .with_hint("Usage: `STEAM <game>`, e.g. `STEAM counter-strike`."));
    }

    let url = format!(
        "{}/api/storesearch/?term={}&l=en&cc=US",
        state.config().steam_store_base,
        urlencoding::encode(q)
    );

    let games = state
        .cache()
        .cached(
            &format!("steam:search:{}", q.to_lowercase()),
            ttl::CATALOGUE,
            || async {
                let body: StoreSearchBody = state
                    .http()
                    .fetch_json(
                        &url,
                        FetchOptions::new()
                            .timeout(Duration::from_secs(20))
                            .retries(2),
                    )
                    .await?;
                Ok(body
                    .items
                    .unwrap_or_default()
                    .into_iter()
                    .filter_map(|item| {
                        let id = item.id.as_f64()?;
                        let name = item.name.as_str().filter(|name| !name.is_empty())?;
                        Some(SteamGame {
                            app_id: id as u32,
                            name: name.to_owned(),
                            rank: None,
                            current_players: None,
                            peak_players: None,
                        })
                    })
                    .collect::<Vec<_>>())
            },
        )
        .await?;

    Ok(games.iter().take(limit).cloned().collect())
}

/// An argument that is only digits is an app id rather than a name.
static DIGITS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\d+$").expect("DIGITS is a valid regex"));

/// One game, with its live count attached.
///
/// Accepts either an app id or a name. Peak is filled in from the leaderboard
/// when the game is currently on it — Valve's live endpoint reports only the
/// instantaneous figure, and a peak is what makes it readable.
pub async fn get_game(state: &AppState, input: &str) -> Result<SteamChart> {
    let raw = input.trim();
    if raw.is_empty() {
        return Err(UpstreamError::bad_request("Missing game")
            .with_hint("Usage: `STEAM <game|appid>`, e.g. `STEAM 730`."));
    }

    let mut game = if DIGITS.is_match(raw) {
        SteamGame {
            app_id: assert_app_id(raw)?,
            name: format!("App {raw}"),
            rank: None,
            current_players: None,
            peak_players: None,
        }
    } else {
        search_games(state, raw, 1)
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| {
                UpstreamError::not_found(format!("Steam has no game matching \"{raw}\""))
                    .with_hint("Check the spelling, or pass the numeric app id from the store URL.")
            })?
    };

    let players = get_player_count(state, game.app_id).await?;

    // Best-effort: a game outside the top table simply has no peak to show. The
    // leaderboard is decoration here; the live count is the answer.
    let mut peak: Option<u64> = None;
    let mut rank: Option<u32> = None;
    if let Ok(top) = get_top(state, 100).await {
        if let Some(on_chart) = top.games.iter().find(|entry| entry.app_id == game.app_id) {
            peak = on_chart.peak_players;
            rank = on_chart.rank;
            if game.name.starts_with("App ") && !on_chart.name.is_empty() {
                game.name = on_chart.name.clone();
            }
        }
    }

    let source_url = format!("{}/app/{}/", state.config().steam_store_base, game.app_id);

    Ok(SteamChart {
        view: "game".to_owned(),
        games: vec![SteamGame {
            rank,
            current_players: players,
            peak_players: peak,
            ..game
        }],
        source_url,
    })
}

#[cfg(test)]
mod tests {
    //! Steam tests.
    //!
    //! The leaderboard is the scraped half — a blank rank header, a trailing dot
    //! on the rank, and the app id hidden in the row's link — and the live count
    //! is the half that must distinguish "no stats published" from a failure.

    use super::*;
    use crate::config::Config;
    use crate::error::codes;
    use crate::http::Http;
    use serde_json::json;
    use wiremock::matchers::{method, path as path_matcher, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const STEAM_HTML: &str = r#"<html><body><table>
  <thead><tr><th></th><th>Name</th><th>Current Players</th><th>Last 30 Days</th><th>Peak Players</th><th>Hours Played</th></tr></thead>
  <tbody>
    <tr><td>1.</td><td><a href="/app/730">Counter-Strike 2</a></td><td>1258099</td><td></td><td>1332784</td><td>611619348</td></tr>
    <tr><td>2.</td><td><a href="/app/570">Dota 2</a></td><td>949773</td><td></td><td>949773</td><td>426889899</td></tr>
  </tbody>
</table></body></html>"#;

    /* --------------------------------------------------- parse_top_page */

    #[test]
    fn reads_rank_name_and_both_player_figures() {
        let games = parse_top_page(STEAM_HTML, "u").unwrap().games;
        assert_eq!(games.len(), 2);
        assert_eq!(games[0].rank, Some(1));
        assert_eq!(games[0].name, "Counter-Strike 2");
        assert_eq!(games[0].current_players, Some(1_258_099));
        assert_eq!(games[0].peak_players, Some(1_332_784));
    }

    #[test]
    fn strips_the_trailing_dot_from_the_rank() {
        assert_eq!(
            parse_top_page(STEAM_HTML, "u").unwrap().games[1].rank,
            Some(2)
        );
    }

    #[test]
    fn recovers_the_app_id_from_the_row_link() {
        let games = parse_top_page(STEAM_HTML, "u").unwrap().games;
        assert_eq!(games[0].app_id, 730);
        assert_eq!(games[1].app_id, 570);
    }

    #[test]
    fn throws_a_diagnosable_error_when_the_table_is_empty() {
        let error =
            parse_top_page("<html><body><table></table></body></html>", "https://s/").unwrap_err();
        assert_eq!(error.code, codes::PARSE_FAILED);
        assert!(
            error.message.contains("No games found"),
            "{}",
            error.message
        );
    }

    #[test]
    fn throws_when_there_is_no_table_at_all() {
        let error =
            parse_top_page("<html><body><p>hi</p></body></html>", "https://s/").unwrap_err();
        assert_eq!(error.code, codes::PARSE_FAILED);
        assert!(
            error.message.contains("No games table found"),
            "{}",
            error.message
        );
    }

    #[test]
    fn a_row_with_no_link_keeps_its_figures_and_reports_no_app_id() {
        // The name is the row; a missing link costs the click-through, not the row.
        let games = parse_top_page(
            "<html><body><table><thead><tr><th></th><th>Name</th><th>Current Players</th></tr></thead>\
             <tbody><tr><td>1.</td><td>Mystery Game</td><td>1,234</td></tr></tbody></table></body></html>",
            "u",
        )
        .unwrap()
        .games;
        assert_eq!(games[0].app_id, 0);
        assert_eq!(games[0].current_players, Some(1234));
    }

    /* -------------------------------------------------------- app ids */

    #[test]
    fn assert_app_id_takes_a_positive_id_inside_valves_range() {
        assert_eq!(assert_app_id(" 730 ").unwrap(), 730);
        for bad in ["0", "-1", "abc", "", "100000001"] {
            let error = assert_app_id(bad).unwrap_err();
            assert_eq!(error.code, codes::BAD_REQUEST, "{bad}");
        }
    }

    /* ----------------------------------------------------------- wire */

    fn state_for(server: &MockServer) -> AppState {
        let base = server.uri().trim_end_matches('/').to_owned();
        AppState::with_http(
            Config {
                steamcharts_base: base.clone(),
                steam_api_base: base.clone(),
                steam_store_base: base,
                ..Config::default()
            },
            Http::new(),
        )
    }

    /// The leaderboard, ready to mount. Every `get_game` test wants it there.
    fn top_page() -> Mock {
        Mock::given(method("GET"))
            .and(path_matcher("/top"))
            .respond_with(ResponseTemplate::new(200).set_body_string(STEAM_HTML))
    }

    fn players_body(count: Option<u64>) -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_json(match count {
            Some(count) => json!({ "response": { "player_count": count, "result": 1 } }),
            None => json!({ "response": { "result": 1 } }),
        })
    }

    fn search_body() -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_json(json!({
            "total": 2,
            "items": [
                { "type": "app", "name": "Counter-Strike 2", "id": 730 },
                { "type": "app", "name": "Counter-Strike: Source", "id": 240 },
                // Neither of these is an app the panel can address.
                { "type": "bundle", "name": "CS Bundle" },
                { "type": "app", "name": "", "id": 999 }
            ]
        }))
    }

    #[tokio::test]
    async fn fetches_the_leaderboard_and_trims_it_to_the_limit() {
        let server = MockServer::start().await;
        top_page().expect(1).mount(&server).await;

        let state = state_for(&server);
        let chart = get_top(&state, DEFAULT_TOP_LIMIT).await.unwrap();
        assert_eq!(chart.view, "top");
        assert_eq!(chart.games.len(), 2);
        assert!(chart.source_url.ends_with("/top"));

        // The second call is a cache hit, so the mock still sees one request.
        assert_eq!(get_top(&state, 1).await.unwrap().games.len(), 1);
        assert!(state.cache().get::<SteamChart>("steam:top").await.is_some());
    }

    #[tokio::test]
    async fn reads_the_live_count_from_valve_without_a_key() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher(
                "/ISteamUserStats/GetNumberOfCurrentPlayers/v1/",
            ))
            .and(query_param("appid", "730"))
            .respond_with(players_body(Some(1_258_099)))
            .expect(1)
            .mount(&server)
            .await;

        let state = state_for(&server);
        assert_eq!(
            get_player_count(&state, 730).await.unwrap(),
            Some(1_258_099)
        );
        // Cached under the app id, so a second panel costs nothing.
        get_player_count(&state, 730).await.unwrap();
        assert!(state
            .cache()
            .get::<Option<u64>>("steam:players:730")
            .await
            .is_some());
    }

    #[tokio::test]
    async fn an_app_that_publishes_no_count_is_not_a_failure() {
        // Valve omits `player_count` for an app that reports no stats. That is
        // "no live figure right now", and a panel says so rather than erroring.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher(
                "/ISteamUserStats/GetNumberOfCurrentPlayers/v1/",
            ))
            .respond_with(players_body(None))
            .mount(&server)
            .await;

        assert_eq!(
            get_player_count(&state_for(&server), 730).await.unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn searches_the_store_and_drops_rows_with_no_id_or_no_name() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/api/storesearch/"))
            .and(query_param("term", "counter-strike"))
            .and(query_param("cc", "US"))
            .respond_with(search_body())
            .expect(1)
            .mount(&server)
            .await;

        let state = state_for(&server);
        let hits = search_games(&state, " counter-strike ", 10).await.unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].app_id, 730);
        assert_eq!(hits[0].name, "Counter-Strike 2");
        assert_eq!(hits[0].current_players, None);
        assert_eq!(hits[0].rank, None);

        assert_eq!(
            search_games(&state, "Counter-Strike", 1)
                .await
                .unwrap()
                .len(),
            1,
            "the cache key is the lowercased query"
        );
    }

    #[tokio::test]
    async fn refuses_a_search_with_nothing_to_search_for() {
        let server = MockServer::start().await;
        let error = search_games(&state_for(&server), "   ", 10)
            .await
            .unwrap_err();
        assert_eq!(error.code, codes::BAD_REQUEST);
    }

    #[tokio::test]
    async fn an_app_id_is_named_and_ranked_from_the_leaderboard() {
        let server = MockServer::start().await;
        top_page().mount(&server).await;
        Mock::given(method("GET"))
            .and(path_matcher(
                "/ISteamUserStats/GetNumberOfCurrentPlayers/v1/",
            ))
            .respond_with(players_body(Some(1_300_000)))
            .mount(&server)
            .await;

        let chart = get_game(&state_for(&server), " 730 ").await.unwrap();
        assert_eq!(chart.view, "game");
        assert_eq!(chart.games.len(), 1);
        let game = &chart.games[0];
        assert_eq!(game.app_id, 730);
        // `App 730` until the leaderboard supplies the real name.
        assert_eq!(game.name, "Counter-Strike 2");
        assert_eq!(game.rank, Some(1));
        assert_eq!(game.peak_players, Some(1_332_784));
        // The live count wins over the leaderboard's snapshot.
        assert_eq!(game.current_players, Some(1_300_000));
        assert!(chart.source_url.ends_with("/app/730/"));
    }

    #[tokio::test]
    async fn a_name_is_resolved_through_the_store_before_the_count_is_read() {
        let server = MockServer::start().await;
        top_page().mount(&server).await;
        Mock::given(method("GET"))
            .and(path_matcher("/api/storesearch/"))
            .respond_with(search_body())
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path_matcher(
                "/ISteamUserStats/GetNumberOfCurrentPlayers/v1/",
            ))
            .and(query_param("appid", "730"))
            .respond_with(players_body(Some(1_258_099)))
            .expect(1)
            .mount(&server)
            .await;

        let game = get_game(&state_for(&server), "counter-strike")
            .await
            .unwrap()
            .games
            .remove(0);
        assert_eq!(game.app_id, 730);
        assert_eq!(game.name, "Counter-Strike 2");
        assert_eq!(game.current_players, Some(1_258_099));
    }

    #[tokio::test]
    async fn a_game_off_the_leaderboard_keeps_its_live_count_and_shows_no_peak() {
        let server = MockServer::start().await;
        top_page().mount(&server).await;
        Mock::given(method("GET"))
            .and(path_matcher(
                "/ISteamUserStats/GetNumberOfCurrentPlayers/v1/",
            ))
            .respond_with(players_body(Some(412)))
            .mount(&server)
            .await;

        let game = get_game(&state_for(&server), "1234567")
            .await
            .unwrap()
            .games
            .remove(0);
        assert_eq!(game.name, "App 1234567");
        assert_eq!(game.current_players, Some(412));
        assert_eq!(game.peak_players, None);
        assert_eq!(game.rank, None);
    }

    #[tokio::test]
    async fn a_dead_leaderboard_does_not_cost_the_live_count() {
        // The join is decoration. Nothing serves /top here, and the answer stands.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher(
                "/ISteamUserStats/GetNumberOfCurrentPlayers/v1/",
            ))
            .respond_with(players_body(Some(1_258_099)))
            .mount(&server)
            .await;

        let game = get_game(&state_for(&server), "730")
            .await
            .unwrap()
            .games
            .remove(0);
        assert_eq!(game.current_players, Some(1_258_099));
        assert_eq!(game.name, "App 730");
        assert_eq!(game.rank, None);
    }

    #[tokio::test]
    async fn a_name_the_store_does_not_carry_is_not_found() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/api/storesearch/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "items": [] })))
            .mount(&server)
            .await;

        let error = get_game(&state_for(&server), "nonexistent game")
            .await
            .unwrap_err();
        assert_eq!(error.code, codes::NOT_FOUND);
        assert!(error.message.contains("nonexistent game"));
    }

    #[tokio::test]
    async fn refuses_an_empty_argument() {
        let server = MockServer::start().await;
        let error = get_game(&state_for(&server), "  ").await.unwrap_err();
        assert_eq!(error.code, codes::BAD_REQUEST);
        assert!(error.hint.unwrap().contains("STEAM 730"));
    }
}
