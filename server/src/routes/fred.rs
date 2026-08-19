//! FRED routes.
//!
//! Two endpoints over one source: a catalogue search, and a series with its
//! observations. The date bounds are checked here rather than in the source
//! because they are a property of the query string — the source takes them as
//! already-valid strings and passes them straight to whichever arm answers.

use std::sync::LazyLock;

use axum::extract::{Path, RawQuery, State};
use axum::routing::get;
use axum::{Json, Router};
use regex::Regex;
use terminal_core::types::{FredSearchResponse, FredSeriesResponse};

use crate::app::AppState;
use crate::error::{Result, UpstreamError};
use crate::routes::helpers::QueryParams;
use crate::sources::fred;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/fred/search", get(search))
        .route("/api/fred/series/{id}", get(series))
}

/// The only shape a FRED date bound takes.
static DATE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\d{4}-\d{2}-\d{2}$").expect("DATE is a valid regex"));

/// Read one date bound.
///
/// Absent or empty means "whatever FRED's own default is". A malformed one is
/// the caller's mistake and is reported: a typo'd start date that quietly
/// returned the whole history would read as the terminal ignoring the argument.
fn date_param(raw: Option<&str>, label: &str) -> Result<Option<String>> {
    let Some(raw) = raw.filter(|value| !value.is_empty()) else {
        return Ok(None);
    };

    if !DATE.is_match(raw) {
        return Err(
            UpstreamError::bad_request(format!("\"{raw}\" is not a valid {label} date"))
                .with_hint("Dates are YYYY-MM-DD, e.g. `FRED UNRATE 2015-01-01`."),
        );
    }
    Ok(Some(raw.to_owned()))
}

/// `GET /api/fred/search?q&limit`
async fn search(
    State(state): State<AppState>,
    RawQuery(raw): RawQuery,
) -> Result<Json<FredSearchResponse>> {
    let params = QueryParams::parse(raw.as_deref().unwrap_or_default());

    let query = params.string("q");
    let limit = params.int_param("limit", i64::from(fred::DEFAULT_SEARCH_LIMIT), 1, 100);
    let limit = u32::try_from(limit).unwrap_or(fred::DEFAULT_SEARCH_LIMIT);

    let found = fred::search_series(&state, &query, limit).await?;
    Ok(Json((*found).clone()))
}

/// `GET /api/fred/series/{id}?start&end`
async fn series(
    State(state): State<AppState>,
    Path(id): Path<String>,
    RawQuery(raw): RawQuery,
) -> Result<Json<FredSeriesResponse>> {
    let params = QueryParams::parse(raw.as_deref().unwrap_or_default());

    let start = date_param(params.get("start"), "start")?;
    let end = date_param(params.get("end"), "end")?;

    let series = fred::get_series(&state, &id, start.as_deref(), end.as_deref()).await?;
    Ok(Json((*series).clone()))
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::error::codes;

    #[test]
    fn an_absent_or_empty_bound_is_no_bound_at_all() {
        assert_eq!(date_param(None, "start").unwrap(), None);
        assert_eq!(date_param(Some(""), "start").unwrap(), None);
    }

    #[test]
    fn accepts_only_yyyy_mm_dd() {
        assert_eq!(
            date_param(Some("2015-01-01"), "start").unwrap(),
            Some("2015-01-01".to_owned())
        );

        for bad in [
            "2015-1-1",
            "01/01/2015",
            "2015-01-01T00:00:00Z",
            "yesterday",
        ] {
            let err = date_param(Some(bad), "end").expect_err(bad);
            assert_eq!(err.code, codes::BAD_REQUEST);
            assert!(err.message.contains(bad), "{}", err.message);
            // The label names which bound the reader got wrong.
            assert!(err.message.contains("end"), "{}", err.message);
            assert!(err.hint.unwrap().contains("YYYY-MM-DD"));
        }
    }
}
