//! Where the outside world is turned into the wire contract.
//!
//! One module per upstream. Each owns exactly one dialect — Kalshi's
//! fixed-point decimal strings, Polymarket's JSON arrays shipped as JSON
//! strings, Polymarket US's `{value, currency}` money objects, Coinbase's
//! positional candle arrays, Mojo's header-keyed HTML tables — and none of that
//! is visible above this directory. What comes out is [`terminal_core::types`].
//!
//! Three of these modules read no network at all. `corpus` searches and ranks a
//! catalogue snapshot, `entertainment` classifies one, and `crossvenue` pairs
//! series across three of them; they are here because they are upstream
//! knowledge, not because they fetch.

use crate::app::AppState;

/// Crawl every catalogue, then pair the series up, in the background.
///
/// Fired once at startup and never awaited: the first `SRCH` or `XV` would
/// otherwise pay a multi-page crawl of six brokers while someone waits at the
/// prompt. Everything it fills is cached, so a failure here costs nothing more
/// than a cold first request.
pub fn warm(state: AppState) {
    tokio::spawn(async move {
        venues::warm_all(&state).await;
        crossvenue::warm_indexes(&state).await;
    });
}

pub mod alpaca;
pub mod awards;
pub mod billboard;
pub mod bls;
pub mod boxoffice;
pub mod cftc;
pub mod congress;
pub mod corpus;
pub mod crossvenue;
pub mod crypto;
pub mod datagov;
pub mod datasources;
pub mod deribit;
pub mod ecb;
pub mod eia;
pub mod entertainment;
pub mod equityoptions;
pub mod feddata;
pub mod forecastex;
pub mod fred;
pub mod gemini;
pub mod imf;
pub mod implied;
pub mod kalshi;
pub mod netflix;
pub mod oecd;
pub mod optionboard;
pub mod podcasts;
pub mod polygon;
pub mod polymarket;
pub mod polymarketus;
pub mod predictfun;
pub mod releases;
pub mod rottentomatoes;
pub mod sdmx;
pub mod secedgar;
pub mod steam;
pub mod stocks;
pub mod streamcharts;
pub mod trends;
pub mod tvmaze;
pub mod venues;
