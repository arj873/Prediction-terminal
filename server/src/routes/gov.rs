//! Congress.gov and data.gov routes.
//!
//! Both publish records rather than observations, so neither is reachable
//! through `ECO` — a bill has no series to chart. They get their own routes for
//! the same reason they get their own panels: the answer is a document, and a
//! document does not have a value at a date.

use axum::extract::{Path, RawQuery, State};
use axum::routing::get;
use axum::{Json, Router};
use terminal_core::types::{BillDetail, BillSearchResponse, DataGovSearchResponse};

use crate::app::AppState;
use crate::error::{Result, UpstreamError};
use crate::routes::helpers::QueryParams;
use crate::sources::{congress, datagov};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/congress/bills", get(bills))
        .route("/api/congress/bill/{congress}/{kind}/{number}", get(bill))
        .route("/api/datagov/search", get(datasets))
}

fn params(raw: Option<String>) -> QueryParams {
    QueryParams::parse(raw.as_deref().unwrap_or(""))
}

async fn bills(
    State(state): State<AppState>,
    RawQuery(raw): RawQuery,
) -> Result<Json<BillSearchResponse>> {
    let query = params(raw);
    let q = query.str_param("q", "");
    #[allow(clippy::cast_sign_loss)]
    let limit = query.int_param("limit", 40, 1, 250) as usize;
    // An absent Congress means the one sitting now, which is arithmetic rather
    // than a table — so it stays right without anyone updating it.
    let congress = query.get("congress").and_then(|raw| raw.parse::<i64>().ok());

    Ok(Json(
        congress::search_bills(&state, &q, congress, limit).await?,
    ))
}

async fn bill(
    State(state): State<AppState>,
    Path((congress_number, kind, number)): Path<(String, String, String)>,
) -> Result<Json<BillDetail>> {
    let congress_number: i64 = congress_number.parse().map_err(|_| {
        UpstreamError::bad_request(format!("\"{congress_number}\" is not a Congress number"))
            .with_hint(format!(
                "The current Congress is {}.",
                congress::current_congress()
            ))
    })?;

    Ok(Json(
        congress::get_bill(&state, congress_number, &kind, &number).await?,
    ))
}

async fn datasets(
    State(state): State<AppState>,
    RawQuery(raw): RawQuery,
) -> Result<Json<DataGovSearchResponse>> {
    let query = params(raw);
    let q = query.str_param("q", "");
    #[allow(clippy::cast_sign_loss)]
    let limit = query.int_param("limit", 30, 1, 100) as usize;
    let cursor = query.get("cursor").filter(|c| !c.is_empty());

    Ok(Json(
        datagov::search_datasets(&state, &q, limit, cursor).await?,
    ))
}
