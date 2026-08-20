//! Placeholder for the forecastex client.

use std::sync::Arc;

use terminal_core::types::{
    CandleInterval, CandlesResponse, Market, OrderBook, SeriesInfo, TradesResponse, Venue,
    VenueEvent,
};
use terminal_core::venue::MoverSort;

use crate::app::AppState;
use crate::error::{Result, UpstreamError};
use crate::sources::corpus::{Corpus, SearchResponse};

const VENUE: Venue = Venue::ForecastEx;

fn todo<T>() -> Result<T> {
    Err(UpstreamError::unsupported("not implemented yet"))
}

pub async fn get_market(_state: &AppState, _id: &str) -> Result<Market> {
    todo()
}

pub async fn get_event(_state: &AppState, _id: &str) -> Result<VenueEvent> {
    todo()
}

pub async fn get_order_book(_state: &AppState, _id: &str, _depth: usize) -> Result<OrderBook> {
    todo()
}

pub async fn get_trades(_state: &AppState, _id: &str, _limit: usize) -> Result<TradesResponse> {
    todo()
}

pub async fn get_candles(
    _state: &AppState,
    _id: &str,
    _interval: CandleInterval,
    _start_ts: i64,
    _end_ts: i64,
) -> Result<CandlesResponse> {
    todo()
}

pub async fn list_series(_state: &AppState, _category: Option<&str>) -> Result<Vec<SeriesInfo>> {
    todo()
}

pub async fn search(_state: &AppState, _query: &str, _limit: usize) -> Result<SearchResponse> {
    todo()
}

pub async fn top_markets(
    _state: &AppState,
    _sort: MoverSort,
    _limit: usize,
) -> Result<Vec<Market>> {
    todo()
}

pub async fn corpus_snapshot(_state: &AppState) -> Result<Arc<Corpus>> {
    let _ = VENUE;
    todo()
}

pub fn warm_corpus(_state: &AppState) {}
