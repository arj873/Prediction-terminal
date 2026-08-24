//! Data-publisher routes.
//!
//! Three endpoints over twelve publishers: a series with its observations, a
//! search that fans out across all of them, and the board that says which ones
//! this deployment can actually serve.
//!
//! The reference is parsed in [`crate::sources::datasources`] rather than here,
//! because the identifier is the publisher's own and the path segment is only
//! the transport for it. What is checked here is the query string — the date
//! bounds and the source filter — which is a property of the request rather
//! than of any publisher.

use std::sync::LazyLock;

use axum::extract::{Path, RawQuery, State};
use axum::routing::get;
use axum::{Json, Router};
use regex::Regex;
use terminal_core::types::{DataSearchResponse, DataSeriesResponse, DataSourcesResponse};

use crate::app::AppState;
use crate::error::{Result, UpstreamError};
use crate::routes::helpers::QueryParams;
use crate::sources::datasources;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/data/sources", get(sources))
        .route("/api/data/search", get(search))
        // A reference contains slashes at half the publishers here — an SDMX
        // key is `EXR/D.USD.EUR.SP00.A` and an EIA id is a route path — so the
        // segment is a wildcard rather than one component.
        .route("/api/data/series/{*reference}", get(series))
}

/// The only shape a date bound takes, at every publisher that accepts one.
static DATE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\d{4}-\d{2}-\d{2}$").expect("DATE is a valid regex"));

/// Read one date bound.
///
/// Absent or empty means "whatever the publisher's own default is". A malformed
/// one is the caller's mistake and is reported: a typo'd start date that quietly
/// returned the whole history would read as the terminal ignoring the argument.
fn date_param(raw: Option<&str>, label: &str) -> Result<Option<String>> {
    let Some(raw) = raw.filter(|value| !value.is_empty()) else {
        return Ok(None);
    };

    if !DATE.is_match(raw) {
        return Err(
            UpstreamError::bad_request(format!("\"{raw}\" is not a valid {label} date"))
                .with_hint("Dates are YYYY-MM-DD, e.g. `ECO UNRATE 2015-01-01`."),
        );
    }
    Ok(Some(raw.to_owned()))
}

/// `GET /api/data/sources`
///
/// Takes no arguments on purpose: what it reports is a fact about this process's
/// environment, and a caller cannot ask about another one.
async fn sources(State(state): State<AppState>) -> Json<DataSourcesResponse> {
    Json(DataSourcesResponse {
        sources: datasources::source_statuses(&state),
    })
}

/// `GET /api/data/search?q&sources&limit`
async fn search(
    State(state): State<AppState>,
    RawQuery(raw): RawQuery,
) -> Result<Json<DataSearchResponse>> {
    let params = QueryParams::parse(raw.as_deref().unwrap_or_default());

    let query = params.string("q");
    let wanted = datasources::parse_source_list(params.get("sources").unwrap_or_default())?;
    let limit = params.int_param("limit", 40, 1, 200) as usize;

    Ok(Json(
        datasources::search_series(&state, &query, &wanted, limit).await?,
    ))
}

/// `GET /api/data/series/{reference}?start&end`
async fn series(
    State(state): State<AppState>,
    Path(reference): Path<String>,
    RawQuery(raw): RawQuery,
) -> Result<Json<DataSeriesResponse>> {
    let params = QueryParams::parse(raw.as_deref().unwrap_or_default());

    let start = date_param(params.get("start"), "start")?;
    let end = date_param(params.get("end"), "end")?;

    Ok(Json(
        datasources::get_series(&state, &reference, start.as_deref(), end.as_deref()).await?,
    ))
}
