//! The one error type every route and source speaks, and how it becomes a
//! response.
//!
//! An error carries a machine-readable `code` and, where there is something the
//! reader can actually do, a `hint` written for a person — the terminal prints
//! it verbatim in the panel that failed. The HTTP status is *derived* from the
//! code at the boundary rather than chosen at the throw site, so a source module
//! never has to know it is being served over HTTP.

use std::fmt;

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use terminal_core::types::ApiError;

/// A failure worth showing to the reader.
///
/// `code` is an open set of strings rather than an enum: the client switches on
/// a handful of them and displays the rest, and a new upstream failure mode
/// should be expressible without touching a shared enum. The codes that carry
/// meaning are listed on [`UpstreamError::status`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpstreamError {
    pub message: String,
    pub code: String,
    pub hint: Option<String>,
    /// The upstream's HTTP status, when the failure came from a remote host.
    /// Echoed to the client; it does *not* decide our own status.
    pub status: Option<u16>,
}

impl UpstreamError {
    pub fn new(message: impl Into<String>, code: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            code: code.into(),
            hint: None,
            status: None,
        }
    }

    /// Attach a remediation hint. Written for a person: it is shown verbatim.
    #[must_use]
    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    /// Attach the upstream's own HTTP status.
    #[must_use]
    pub fn with_status(mut self, status: u16) -> Self {
        self.status = Some(status);
        self
    }

    /// The caller asked for something malformed.
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(message, codes::BAD_REQUEST)
    }

    /// The thing asked for does not exist.
    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(message, codes::NOT_FOUND)
    }

    /// This venue does not serve this at all — a stated capability gap, not an
    /// outage. Polymarket US's candles and prints are the whole reason this
    /// exists, and the hint names what would be needed.
    pub fn unsupported(message: impl Into<String>) -> Self {
        Self::new(message, codes::UNSUPPORTED)
    }

    /// A credential this feed needs was never set.
    pub fn not_configured(message: impl Into<String>) -> Self {
        Self::new(message, codes::NOT_CONFIGURED)
    }

    /// The upstream accepted the connection and never replied.
    pub fn timeout(message: impl Into<String>) -> Self {
        Self::new(message, codes::UPSTREAM_TIMEOUT)
    }

    /// The upstream refused us — typically bot protection turning away a
    /// datacentre IP.
    pub fn blocked(message: impl Into<String>) -> Self {
        Self::new(message, codes::UPSTREAM_BLOCKED)
    }

    /// A page or feed parsed into nothing usable. Distinct from `not_found`:
    /// the host answered, we could not read it, and that is our bug to fix.
    pub fn parse_failed(message: impl Into<String>) -> Self {
        Self::new(message, codes::PARSE_FAILED)
    }

    /// Whether this code ends a provider chain rather than falling through to
    /// the next arm. A missing series is missing at every provider.
    pub fn is_terminal(&self) -> bool {
        matches!(
            self.code.as_str(),
            codes::NOT_FOUND | codes::BAD_REQUEST | codes::UNSUPPORTED
        )
    }

    /// The HTTP status this failure is served as.
    ///
    /// Everything that is not one of the named cases is a 502: the terminal
    /// asked an upstream a question and did not get a usable answer, which is a
    /// bad *gateway* rather than a bad request or a broken server.
    pub fn http_status(&self) -> StatusCode {
        match self.code.as_str() {
            codes::BAD_REQUEST => StatusCode::BAD_REQUEST,
            codes::NOT_FOUND => StatusCode::NOT_FOUND,
            codes::UNSUPPORTED => StatusCode::NOT_IMPLEMENTED,
            codes::NOT_CONFIGURED => StatusCode::SERVICE_UNAVAILABLE,
            codes::UPSTREAM_TIMEOUT => StatusCode::GATEWAY_TIMEOUT,
            codes::RATE_LIMITED => StatusCode::TOO_MANY_REQUESTS,
            codes::INTERNAL => StatusCode::INTERNAL_SERVER_ERROR,
            _ => StatusCode::BAD_GATEWAY,
        }
    }

    pub fn body(&self) -> ApiError {
        ApiError {
            error: self.message.clone(),
            code: self.code.clone(),
            hint: self.hint.clone(),
            status: self.status,
        }
    }
}

impl fmt::Display for UpstreamError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({})", self.message, self.code)
    }
}

impl std::error::Error for UpstreamError {}

impl IntoResponse for UpstreamError {
    fn into_response(self) -> Response {
        (self.http_status(), Json(self.body())).into_response()
    }
}

/// The `code` values the client and the sources agree on.
pub mod codes {
    /// The request itself was malformed. → 400
    pub const BAD_REQUEST: &str = "bad_request";
    /// No such market, series or page. → 404
    pub const NOT_FOUND: &str = "not_found";
    /// This venue does not publish this, by design. → 501
    pub const UNSUPPORTED: &str = "unsupported";
    /// A required credential is unset. → 503
    pub const NOT_CONFIGURED: &str = "not_configured";
    /// The upstream never replied. → 504
    pub const UPSTREAM_TIMEOUT: &str = "upstream_timeout";
    /// This client is over its own rate limit. → 429
    pub const RATE_LIMITED: &str = "rate_limited";
    /// A bug on our side. → 500
    pub const INTERNAL: &str = "internal_error";

    // Everything below is served as 502.

    /// The upstream answered with an unhelpful status.
    pub const UPSTREAM_STATUS: &str = "upstream_status";
    /// The upstream hung up — usually bot protection.
    pub const UPSTREAM_BLOCKED: &str = "upstream_blocked";
    /// The transport failed in some other way.
    pub const UPSTREAM_ERROR: &str = "upstream_error";
    /// The body was not the JSON it claimed to be.
    pub const BAD_UPSTREAM_BODY: &str = "bad_upstream_body";
    /// A page answered but did not parse into anything usable.
    pub const PARSE_FAILED: &str = "parse_failed";
    /// The credentials we hold were rejected.
    pub const BAD_CREDENTIALS: &str = "bad_credentials";
    /// The upstream returned nothing where it must return something.
    pub const EMPTY_UPSTREAM: &str = "empty_upstream";
    /// The body exceeded the size ceiling.
    pub const RESPONSE_TOO_LARGE: &str = "response_too_large";
}

/// Convenience alias for a handler or source result.
pub type Result<T> = std::result::Result<T, UpstreamError>;

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;

    async fn body_of(err: UpstreamError) -> (StatusCode, serde_json::Value) {
        let response = err.into_response();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    #[test]
    fn maps_each_named_code_to_its_status() {
        for (code, expected) in [
            (codes::BAD_REQUEST, StatusCode::BAD_REQUEST),
            (codes::NOT_FOUND, StatusCode::NOT_FOUND),
            (codes::UNSUPPORTED, StatusCode::NOT_IMPLEMENTED),
            (codes::NOT_CONFIGURED, StatusCode::SERVICE_UNAVAILABLE),
            (codes::UPSTREAM_TIMEOUT, StatusCode::GATEWAY_TIMEOUT),
            (codes::RATE_LIMITED, StatusCode::TOO_MANY_REQUESTS),
            (codes::INTERNAL, StatusCode::INTERNAL_SERVER_ERROR),
        ] {
            assert_eq!(
                UpstreamError::new("x", code).http_status(),
                expected,
                "{code}"
            );
        }
    }

    #[test]
    fn serves_every_other_upstream_failure_as_a_bad_gateway() {
        for code in [
            codes::UPSTREAM_STATUS,
            codes::UPSTREAM_BLOCKED,
            codes::UPSTREAM_ERROR,
            codes::BAD_UPSTREAM_BODY,
            codes::PARSE_FAILED,
            codes::BAD_CREDENTIALS,
            codes::EMPTY_UPSTREAM,
            codes::RESPONSE_TOO_LARGE,
            "something_nobody_has_seen_yet",
        ] {
            assert_eq!(
                UpstreamError::new("x", code).http_status(),
                StatusCode::BAD_GATEWAY,
                "{code}"
            );
        }
    }

    #[tokio::test]
    async fn a_hint_reaches_the_client_verbatim() {
        let (status, body) = body_of(
            UpstreamError::blocked("fred.stlouisfed.org refused the connection")
                .with_hint("Set FRED_API_KEY to use the official API instead.")
                .with_status(503),
        )
        .await;

        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert_eq!(body["error"], "fred.stlouisfed.org refused the connection");
        assert_eq!(body["code"], "upstream_blocked");
        assert_eq!(
            body["hint"],
            "Set FRED_API_KEY to use the official API instead."
        );
        // The upstream's status is reported, not adopted.
        assert_eq!(body["status"], 503);
    }

    #[tokio::test]
    async fn omits_hint_and_status_when_there_are_none() {
        let (_, body) = body_of(UpstreamError::not_found("No such market")).await;
        assert_eq!(
            body,
            serde_json::json!({ "error": "No such market", "code": "not_found" })
        );
    }

    #[test]
    fn a_missing_thing_is_missing_at_every_provider() {
        assert!(UpstreamError::not_found("x").is_terminal());
        assert!(UpstreamError::bad_request("x").is_terminal());
        assert!(UpstreamError::unsupported("x").is_terminal());
        // A blocked or timed-out arm should let the next provider try.
        assert!(!UpstreamError::blocked("x").is_terminal());
        assert!(!UpstreamError::timeout("x").is_terminal());
        assert!(!UpstreamError::not_configured("x").is_terminal());
    }
}
