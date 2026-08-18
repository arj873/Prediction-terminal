//! Entertainment routes.
//!
//! One router for the whole entertainment surface — the Kalshi market browser
//! and the six data feeds those markets settle against — because they are one
//! feature and share a prefix. Each handler is a thin shell: validation lives in
//! the source modules next to the parsers that depend on it.

use axum::extract::{RawQuery, State};
use axum::routing::get;
use axum::{Json, Router};
use terminal_core::types::{
    BoxOfficeDay, EntResponse, NetflixTop10, RtSearchResponse, RtTitle, SteamChart, StreamChart,
    StreamChartsResponse, TvSchedule,
};

use crate::app::AppState;
use crate::error::Result;
use crate::routes::helpers::QueryParams;
use crate::sources::{
    boxoffice, entertainment, netflix, rottentomatoes, steam, streamcharts, tvmaze,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/ent/markets", get(markets))
        .route("/api/ent/rt/search", get(rt_search))
        .route("/api/ent/rt", get(rt_title))
        .route("/api/ent/netflix", get(netflix_top10))
        .route("/api/ent/charts", get(charts))
        .route("/api/ent/spotify", get(spotify))
        .route("/api/ent/youtube", get(youtube))
        .route("/api/ent/boxoffice", get(boxoffice_daily))
        .route("/api/ent/steam", get(steam_chart))
        .route("/api/ent/tv", get(tv))
}

/// Parse the query once, for a handler that only ever reads it.
fn query(raw: Option<String>) -> QueryParams {
    QueryParams::parse(raw.as_deref().unwrap_or_default())
}

/* ------------------------------------------------- kalshi entertainment book */

/// `GET /api/ent/markets?genre&limit`
async fn markets(
    State(state): State<AppState>,
    RawQuery(raw): RawQuery,
) -> Result<Json<EntResponse>> {
    let params = query(raw);

    let genre = entertainment::assert_genre(&params.string("genre"))?;
    let limit = params.int_param("limit", entertainment::DEFAULT_LIMIT as i64, 1, MAX_MARKETS);

    Ok(Json(
        entertainment::browse(&state, genre, limit as usize).await?,
    ))
}

/// More events than fit on a screen, and enough that a client can filter
/// locally. The whole open book is ~545 events, so this is not far off "all".
const MAX_MARKETS: i64 = 250;

/* ------------------------------------------------------------ rotten tomatoes */

/// `GET /api/ent/rt/search?q&limit`
async fn rt_search(
    State(state): State<AppState>,
    RawQuery(raw): RawQuery,
) -> Result<Json<RtSearchResponse>> {
    let params = query(raw);

    let limit = params.int_param("limit", rottentomatoes::DEFAULT_SEARCH_LIMIT as i64, 1, 50);

    Ok(Json(
        rottentomatoes::search(&state, &params.string("q"), limit as usize).await?,
    ))
}

/// `GET /api/ent/rt?q`
async fn rt_title(State(state): State<AppState>, RawQuery(raw): RawQuery) -> Result<Json<RtTitle>> {
    let title = rottentomatoes::get_title(&state, &query(raw).string("q")).await?;
    Ok(Json((*title).clone()))
}

/* -------------------------------------------------------------------- netflix */

/// `GET /api/ent/netflix?category&scope`
///
/// `category` defaults to `tv` twice over — once for a caller that omits it, and
/// once for one that sends it empty — because an empty category is a panel that
/// has not been told which tab it is on, not a request for an error.
async fn netflix_top10(
    State(state): State<AppState>,
    RawQuery(raw): RawQuery,
) -> Result<Json<NetflixTop10>> {
    let params = query(raw);

    let raw_category = params.str_param("category", "tv");
    let category = netflix::assert_category(if raw_category.is_empty() {
        "tv"
    } else {
        &raw_category
    })?;
    let scope = netflix::assert_scope(&params.string("scope"))?;

    Ok(Json(netflix::get_top10(&state, category, &scope).await?))
}

/* --------------------------------------------------------- spotify / youtube */

/// `GET /api/ent/charts?source`
///
/// A constant list, filtered to one source when the caller names a known one.
/// An unrecognised `source` lists everything rather than erroring: this is the
/// endpoint a client calls to find out what exists.
async fn charts(RawQuery(raw): RawQuery) -> Json<StreamChartsResponse> {
    let source = query(raw).string("source").to_lowercase();
    let filter = match source.as_str() {
        "spotify" | "youtube" => Some(source.as_str()),
        _ => None,
    };

    Json(StreamChartsResponse {
        charts: streamcharts::list_charts(filter),
    })
}

/// `GET /api/ent/spotify?scope&period&limit`
async fn spotify(
    State(state): State<AppState>,
    RawQuery(raw): RawQuery,
) -> Result<Json<StreamChart>> {
    let params = query(raw);

    let spec =
        streamcharts::resolve_spotify_chart(&params.string("scope"), &params.string("period"))?;
    let limit = params.int_param(
        "limit",
        i64::from(streamcharts::DEFAULT_LIMIT),
        1,
        MAX_CHART_ROWS,
    );

    Ok(Json(
        streamcharts::get_chart(&state, &spec, limit as u32).await?,
    ))
}

/// `GET /api/ent/youtube?view&limit`
async fn youtube(
    State(state): State<AppState>,
    RawQuery(raw): RawQuery,
) -> Result<Json<StreamChart>> {
    let params = query(raw);

    let spec = streamcharts::resolve_youtube_chart(&params.string("view"))?;
    let limit = params.int_param(
        "limit",
        i64::from(streamcharts::DEFAULT_LIMIT),
        1,
        MAX_CHART_ROWS,
    );

    Ok(Json(
        streamcharts::get_chart(&state, &spec, limit as u32).await?,
    ))
}

/// The YouTube all-time table runs to several thousand rows; past this the
/// client is rendering DOM nobody scrolls to.
const MAX_CHART_ROWS: i64 = 500;

/* ----------------------------------------------------------------- box office */

/// `GET /api/ent/boxoffice?date`
///
/// No date means "the most recent day with grosses", which the source walks
/// back to — yesterday's numbers are not published until the afternoon.
async fn boxoffice_daily(
    State(state): State<AppState>,
    RawQuery(raw): RawQuery,
) -> Result<Json<BoxOfficeDay>> {
    let params = query(raw);
    let date = params.string("date");
    let date = if date.is_empty() { None } else { Some(&date) };

    Ok(Json(
        boxoffice::get_daily(&state, date.map(String::as_str)).await?,
    ))
}

/* ---------------------------------------------------------------------- steam */

/// `GET /api/ent/steam?q&limit`
///
/// No `q` is the leaderboard; a `q` is one game, by name or app id. One endpoint
/// rather than two because the panel is one panel, and `limit` only means
/// anything to the leaderboard.
async fn steam_chart(
    State(state): State<AppState>,
    RawQuery(raw): RawQuery,
) -> Result<Json<SteamChart>> {
    let params = query(raw);
    let game = params.string("q");

    if game.is_empty() {
        let limit = params.int_param("limit", i64::from(steam::DEFAULT_TOP_LIMIT), 1, 100);
        return Ok(Json(steam::get_top(&state, limit as u32).await?));
    }

    Ok(Json(steam::get_game(&state, &game).await?))
}

/* ------------------------------------------------------------------ tv guide */

/// `GET /api/ent/tv?date&country`
async fn tv(State(state): State<AppState>, RawQuery(raw): RawQuery) -> Result<Json<TvSchedule>> {
    let params = query(raw);
    let date = params.string("date");
    let country = params.string("country");

    Ok(Json(
        tvmaze::get_schedule(
            &state,
            if date.is_empty() { None } else { Some(&date) },
            if country.is_empty() {
                None
            } else {
                Some(&country)
            },
        )
        .await?,
    ))
}
