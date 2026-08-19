//! HTTP in front of the sources.
//!
//! A route reads the query, clamps it into range, calls one source, and hands
//! back what it returns. It holds no knowledge of any upstream — that all lives
//! in `sources/` — and it raises no errors of its own beyond `bad_request` for
//! a query it cannot make sense of.
//!
//! Every module here exposes `router() -> Router<AppState>` carrying its own
//! absolute paths, so the mount table below is the whole map of the API.

pub mod billboard;
pub mod crossvenue;
pub mod entertainment;
pub mod fred;
pub mod helpers;
pub mod implied;
pub mod kalshi;
pub mod news;
pub mod spot;
pub mod venue;

use axum::routing::any;
use axum::Router;

use crate::app::AppState;

/// Everything under `/api`, with the per-IP rate limit in front of it.
pub fn api_router(state: AppState) -> Router {
    let limiter = crate::rate_limit::RateLimiter::new(state.config().rate_limit);

    Router::new()
        // The venue-agnostic surface every command actually uses.
        .merge(venue::router())
        // Kalshi's own mount predates the other two venues and is kept so that
        // anything written against the original API still works.
        .merge(kalshi::router())
        .merge(crossvenue::router())
        .merge(spot::router())
        .merge(implied::router())
        .merge(fred::router())
        .merge(billboard::router())
        .merge(entertainment::router())
        .merge(news::router())
        .merge(health::router())
        // A wildcard *route* rather than a fallback, because the client's SPA
        // fallback is attached to this same router afterwards and would
        // otherwise swallow it — answering a mistyped fetch with an HTML page
        // that fails to parse somewhere else entirely. `matchit` prefers the
        // literal routes above to this, so it only catches what nothing claimed.
        .route("/api", any(unknown_api_route))
        .route("/api/", any(unknown_api_route))
        .route("/api/{*rest}", any(unknown_api_route))
        .with_state(state)
        .layer(axum::middleware::from_fn_with_state(
            limiter,
            crate::rate_limit::enforce,
        ))
}

/// The catch-all for a path under `/api` that names no route.
///
/// Answered as JSON rather than falling through to the client's SPA fallback:
/// a typo in a fetch should read as a missing route, not as an HTML page that
/// fails to parse somewhere else entirely.
async fn unknown_api_route() -> crate::error::UpstreamError {
    crate::error::UpstreamError::not_found("No such API route")
}

mod health {
    use axum::extract::State;
    use axum::routing::get;
    use axum::{Json, Router};
    use terminal_core::types::HealthResponse;

    use crate::app::AppState;

    pub fn router() -> Router<AppState> {
        Router::new().route("/api/health", get(health))
    }

    /// Liveness plus the two facts an operator most often needs: whether the
    /// optional credentials are set, and whether the cache is doing its job.
    async fn health(State(state): State<AppState>) -> Json<HealthResponse> {
        Json(HealthResponse {
            ok: true,
            uptime_seconds: state.uptime_seconds().round(),
            cache: state.cache().stats(),
            fred_api_key: state.config().has_fred_key(),
            alpaca_keys: state.config().has_alpaca_keys(),
            time: now_iso8601(),
        })
    }

    fn now_iso8601() -> String {
        time::OffsetDateTime::now_utc()
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_default()
    }
}
