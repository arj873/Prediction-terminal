//! Assembling the server.
//!
//! [`build_router`] is the seam the whole test suite hangs off: `main` calls it
//! with a config read from the environment, and an integration test calls it
//! with a config pointing at a fixture server. Neither path touches process
//! globals, so tests can run in parallel without fighting over `std::env`.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use axum::Router;
use terminal_core::types::{CorpusState, VenueCorpusHealth};
use terminal_core::util::round_to;
use terminal_core::venue::{venue_ids, Venue};

use crate::cache::TtlCache;
use crate::config::Config;
use crate::http::Http;

/// What a venue's catalogue crawl last did.
///
/// Kept beside the cache rather than in it, because a *failure* is deliberately
/// never cached (see [`crate::cache`]) and the failure is the thing an operator
/// curling `/api/health` needs to see.
pub enum CorpusRecord {
    Ok {
        events: u32,
        markets: u32,
        truncated: bool,
        built_at: Instant,
    },
    Failed {
        error: String,
        code: String,
        hint: Option<String>,
    },
}

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
    /// Last crawl outcome per venue. A plain mutex: it is written once per
    /// catalogue TTL and read only by `/api/health`.
    venue_corpus: Mutex<BTreeMap<Venue, CorpusRecord>>,
}

impl AppState {
    pub fn new(config: Config) -> Self {
        Self(Arc::new(StateInner {
            config,
            http: Http::new(),
            cache: TtlCache::new(),
            started: Instant::now(),
            venue_corpus: Mutex::new(BTreeMap::new()),
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
            venue_corpus: Mutex::new(BTreeMap::new()),
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

    /// Note what a venue's catalogue crawl just did.
    ///
    /// The guard is taken and dropped around the insert alone — never held
    /// across an await, which is both a deadlock risk and a clippy denial.
    pub fn record_corpus(&self, venue: Venue, record: CorpusRecord) {
        if let Ok(mut records) = self.0.venue_corpus.lock() {
            records.insert(venue, record);
        }
    }

    /// Every venue's catalogue state, in registry order.
    ///
    /// A venue that has never been crawled reports [`CorpusState::Never`]
    /// rather than being left out: "not warm yet" and "broken" are different
    /// answers, and an absent row would read as neither.
    pub fn venue_corpus_health(&self) -> Vec<VenueCorpusHealth> {
        let records = self.0.venue_corpus.lock().ok();

        venue_ids()
            .into_iter()
            .map(|venue| {
                let blank = VenueCorpusHealth {
                    venue,
                    state: CorpusState::Never,
                    events: 0,
                    markets: 0,
                    truncated: false,
                    age_seconds: None,
                    error: None,
                    code: None,
                    hint: None,
                };

                match records.as_ref().and_then(|held| held.get(&venue)) {
                    Some(CorpusRecord::Ok {
                        events,
                        markets,
                        truncated,
                        built_at,
                    }) => VenueCorpusHealth {
                        state: CorpusState::Ok,
                        events: *events,
                        markets: *markets,
                        truncated: *truncated,
                        age_seconds: Some(round_to(built_at.elapsed().as_secs_f64(), 0)),
                        ..blank
                    },
                    Some(CorpusRecord::Failed { error, code, hint }) => VenueCorpusHealth {
                        state: CorpusState::Failed,
                        error: Some(error.clone()),
                        code: Some(code.clone()),
                        hint: hint.clone(),
                        ..blank
                    },
                    None => blank,
                }
            })
            .collect()
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
