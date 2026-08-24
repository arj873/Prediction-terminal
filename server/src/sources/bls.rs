//! Placeholder for the bls client.

use terminal_core::types::{DataSearchResult, DataSeriesResponse};

use crate::app::AppState;
use crate::error::{Result, UpstreamError};

pub async fn get_series(
    _state: &AppState,
    _id: &str,
    _start: Option<&str>,
    _end: Option<&str>,
) -> Result<DataSeriesResponse> {
    Err(UpstreamError::unsupported("not implemented yet"))
}

pub async fn search(
    _state: &AppState,
    _query: &str,
    _limit: usize,
) -> Result<Vec<DataSearchResult>> {
    Err(UpstreamError::unsupported("not implemented yet"))
}

/// `None` when this deployment can use the publisher; a string is the
/// operator-facing reason it cannot.
pub fn unavailable(_state: &AppState) -> Option<String> {
    None
}
