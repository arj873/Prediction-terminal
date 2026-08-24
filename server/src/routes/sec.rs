//! SEC EDGAR routes.
//!
//! Three endpoints, because EDGAR answers three different questions and
//! flattening them into one would mean guessing which was meant: what has this
//! filer filed, what has it reported for one XBRL concept, and which filer did
//! you mean.

use axum::extract::{Path, RawQuery, State};
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;
use terminal_core::types::{SecConceptResponse, SecFilingsResponse, SecSearchResult};

use crate::app::AppState;
use crate::error::{Result, UpstreamError};
use crate::routes::helpers::QueryParams;
use crate::sources::secedgar;

pub fn router() -> Router<AppState> {
    Router::new()
        // Before `/{company}/…`, or `search` would be read as a company.
        .route("/api/sec/search", get(search))
        .route("/api/sec/{company}/filings", get(filings))
        .route("/api/sec/{company}/concept/{tag}", get(concept))
}

fn params(raw: Option<String>) -> QueryParams {
    QueryParams::parse(raw.as_deref().unwrap_or(""))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SearchResponse {
    query: String,
    results: Vec<SecSearchResult>,
}

async fn search(
    State(state): State<AppState>,
    RawQuery(raw): RawQuery,
) -> Result<Json<SearchResponse>> {
    let query = params(raw);
    let q = query.str_param("q", "");
    if q.trim().is_empty() {
        return Err(UpstreamError::bad_request("Missing `q`")
            .with_hint("Search EDGAR by ticker or by words from a company name."));
    }
    #[allow(clippy::cast_sign_loss)]
    let limit = query.int_param("limit", 25, 1, 100) as usize;
    let results = secedgar::search(&state, &q, limit).await?;
    Ok(Json(SearchResponse { query: q, results }))
}

async fn filings(
    State(state): State<AppState>,
    Path(company): Path<String>,
    RawQuery(raw): RawQuery,
) -> Result<Json<SecFilingsResponse>> {
    let query = params(raw);
    #[allow(clippy::cast_sign_loss)]
    let limit = query.int_param("limit", 40, 1, 250) as usize;
    let form = query.str_param("form", "");
    let form = if form.trim().is_empty() {
        None
    } else {
        Some(form)
    };
    Ok(Json(
        secedgar::get_filings(&state, &company, limit, form.as_deref()).await?,
    ))
}

async fn concept(
    State(state): State<AppState>,
    Path((company, tag)): Path<(String, String)>,
    RawQuery(raw): RawQuery,
) -> Result<Json<SecConceptResponse>> {
    let query = params(raw);
    // `us-gaap` covers almost everything; `dei` carries the entity facts and
    // `ifrs-full` the foreign private issuers, so the taxonomy is a parameter
    // rather than a constant.
    let taxonomy = query.str_param("taxonomy", "us-gaap");
    Ok(Json(
        secedgar::get_concept(&state, &company, &tag, &taxonomy).await?,
    ))
}
