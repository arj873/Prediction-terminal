//! Assembling the server.
//!
//! [`build_router`] is the seam the whole test suite hangs off: `main` calls it
//! with a config read from the environment, and an integration test calls it
//! with a config pointing at a fixture server. Neither path touches process
//! globals, so tests can run in parallel without fighting over `std::env`.

use std::sync::Arc;
use std::time::Instant;

use axum::Router;

use crate::cache::TtlCache;
use crate::config::Config;
use crate::http::Http;

/// Everything a request handler or a source module needs.
///
/// Cloned per request — the clone is one `Arc` bump. Sources take `&AppState`
/// rather than reaching for a global, which is what makes them testable against
/// a fixture and what keeps the config overridable.
#[derive(Clone)]
pub struct AppState(Arc<StateInner>);

pub struct StateInner {
    pub config: Config,
    pub http: Http,
    pub cache: TtlCache,
    pub started: Instant,
}

impl AppState {
    pub fn new(config: Config) -> Self {
        Self(Arc::new(StateInner {
            config,
            http: Http::new(),
            cache: TtlCache::new(),
            started: Instant::now(),
        }))
    }

    /// Build a state with a pre-made HTTP client — a test can hand in one with
    /// no retries so a fixture assertion is not muddied by a second attempt.
    pub fn with_http(config: Config, http: Http) -> Self {
        Self(Arc::new(StateInner {
            config,
            http,
            cache: TtlCache::new(),
            started: Instant::now(),
        }))
    }

    pub fn config(&self) -> &Config {
        &self.0.config
    }

    pub fn http(&self) -> &Http {
        &self.0.http
    }

    pub fn cache(&self) -> &TtlCache {
        &self.0.cache
    }

    pub fn uptime_seconds(&self) -> f64 {
        self.0.started.elapsed().as_secs_f64()
    }
}

impl std::ops::Deref for AppState {
    type Target = StateInner;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// The complete application: API routes, the rate limit, and the built client
/// when one is present.
pub fn build_router(state: AppState) -> Router {
    let client_dir = crate::static_files::resolve(state.config().client_dir.as_deref());
    let router = crate::routes::api_router(state.clone());
    crate::static_files::attach(router, client_dir.as_deref())
}
