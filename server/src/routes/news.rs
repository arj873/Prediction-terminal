//! News routes.
//!
//! One endpoint, kept off `/api/ent` because a market-news wire is not an
//! entertainment feed. Validation lives in the source module next to the
//! normaliser that depends on it, so this is a shell.

use axum::extract::{RawQuery, State};
use axum::routing::get;
use axum::{Json, Router};
use terminal_core::types::NewsFeed;

use crate::app::AppState;
use crate::error::Result;
use crate::routes::helpers::QueryParams;
use crate::sources::alpaca;

pub fn router() -> Router<AppState> {
    Router::new().route("/api/news", get(news))
}

/// `GET /api/news?symbols&limit&days`
///
/// `symbols` is comma- or space-separated and may be empty, which means the
/// whole wire rather than nothing.
async fn news(State(state): State<AppState>, RawQuery(raw): RawQuery) -> Result<Json<NewsFeed>> {
    let params = QueryParams::parse(raw.as_deref().unwrap_or_default());

    let symbols = alpaca::assert_symbols(&params.string("symbols"))?;
    let limit = params.int_param(
        "limit",
        i64::from(alpaca::DEFAULT_LIMIT),
        1,
        i64::from(alpaca::MAX_LIMIT),
    );
    let days = params.int_param(
        "days",
        i64::from(alpaca::DEFAULT_DAYS),
        1,
        i64::from(alpaca::MAX_DAYS),
    );

    let feed = alpaca::get_news(&state, &symbols, Some(limit), Some(days)).await?;
    Ok(Json(feed))
}
