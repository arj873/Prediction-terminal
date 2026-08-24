//! The Prediction Terminal API server.
//!
//! Three layers, each of which knows nothing about the one above it:
//!
//! - `sources/` speak to the outside world and normalise what comes back into
//!   the wire types from `terminal_core`. Each upstream's dialect — Kalshi's
//!   decimal strings, Polymarket's JSON-inside-JSON, Polymarket US's
//!   `{value, currency}` money objects — is unpicked here and nowhere else.
//! - `routes/` map HTTP onto those sources: read the query, clamp it, call, and
//!   hand back the result. They contain no upstream knowledge.
//! - `app.rs` assembles the two into a router, which both [`main`] and the
//!   integration tests build the same way.
//!
//! [`build_router`](app::build_router) is the seam: a test constructs a
//! [`Config`](config::Config) pointing at a fixture server and gets a real
//! router back, without any process-global state.

pub mod app;
pub mod cache;
pub mod config;
pub mod error;
pub mod http;
pub mod period;
pub mod providers;
pub mod rate_limit;
pub mod routes;
pub mod scrape;
pub mod sources;
pub mod static_files;

pub use app::{build_router, AppState};
pub use config::Config;
pub use error::{codes, Result, UpstreamError};
