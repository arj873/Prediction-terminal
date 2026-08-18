//! A crude per-IP request cap on `/api`.
//!
//! Not a security boundary — just a stop on a runaway client loop turning into
//! an outbound flood at Kalshi and Billboard. One process, one map, fixed
//! windows; nothing here survives a restart and nothing needs to.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::{ConnectInfo, Request};
use axum::http::HeaderMap;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::error::{codes, UpstreamError};

const WINDOW: Duration = Duration::from_secs(60);
/// Sweep expired buckets once the map is bigger than this. Bounded growth
/// matters more than precision — a sweep costs nothing next to an upstream call.
const SWEEP_THRESHOLD: usize = 5_000;

struct Bucket {
    count: u32,
    reset_at: Instant,
}

/// Shared counter state. Cloneable; every clone shares one map.
#[derive(Clone)]
pub struct RateLimiter {
    buckets: Arc<Mutex<HashMap<String, Bucket>>>,
    max_per_window: u32,
}

impl RateLimiter {
    pub fn new(max_per_window: u32) -> Self {
        Self {
            buckets: Arc::new(Mutex::new(HashMap::new())),
            max_per_window,
        }
    }

    /// Count one request from `key`. `false` means it is over the cap.
    fn allow(&self, key: &str) -> bool {
        self.allow_at(key, Instant::now())
    }

    /// [`RateLimiter::allow`] against a caller-supplied clock, so the window
    /// boundary is testable without sleeping through it.
    fn allow_at(&self, key: &str, now: Instant) -> bool {
        let mut buckets = self
            .buckets
            .lock()
            .expect("rate-limit map is never poisoned");

        let allowed = match buckets.get_mut(key) {
            Some(bucket) if bucket.reset_at > now => {
                bucket.count += 1;
                bucket.count <= self.max_per_window
            }
            // No bucket, or the window has rolled over.
            _ => {
                buckets.insert(
                    key.to_string(),
                    Bucket {
                        count: 1,
                        reset_at: now + WINDOW,
                    },
                );
                true
            }
        };

        if buckets.len() > SWEEP_THRESHOLD {
            buckets.retain(|_, bucket| bucket.reset_at > now);
        }

        allowed
    }

    fn too_many(&self) -> Response {
        UpstreamError::new("Too many requests", codes::RATE_LIMITED)
            .with_hint(format!(
                "This client exceeded {} API calls per minute.",
                self.max_per_window
            ))
            .into_response()
    }
}

/// Identify the client.
///
/// The server is expected to sit behind a proxy in any real deployment, so the
/// forwarded chain is trusted the way Express's `trust proxy` did. The
/// *leftmost* entry is the original client; the rest are hops.
fn client_key(headers: &HeaderMap, peer: Option<SocketAddr>) -> String {
    if let Some(forwarded) = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok()) {
        if let Some(first) = forwarded.split(',').next() {
            let trimmed = first.trim();
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }
    }

    if let Some(real_ip) = headers.get("x-real-ip").and_then(|v| v.to_str().ok()) {
        let trimmed = real_ip.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }

    peer.map(|addr| addr.ip().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

/// Middleware entry point. Mounted on `/api` only — the static client is not
/// worth counting.
pub async fn enforce(
    axum::extract::State(limiter): axum::extract::State<RateLimiter>,
    request: Request,
    next: Next,
) -> Response {
    let peer = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ConnectInfo(addr)| *addr);
    let key = client_key(request.headers(), peer);

    if !limiter.allow(&key) {
        return limiter.too_many();
    }

    next.run(request).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::{HeaderMap, HeaderValue};

    #[test]
    fn allows_up_to_the_cap_then_refuses() {
        let limiter = RateLimiter::new(3);
        assert!(limiter.allow("1.2.3.4"));
        assert!(limiter.allow("1.2.3.4"));
        assert!(limiter.allow("1.2.3.4"));
        assert!(!limiter.allow("1.2.3.4"));
        assert!(!limiter.allow("1.2.3.4"));
    }

    #[test]
    fn counts_each_client_separately() {
        let limiter = RateLimiter::new(1);
        assert!(limiter.allow("1.2.3.4"));
        assert!(!limiter.allow("1.2.3.4"));
        // A different client still has its whole allowance.
        assert!(limiter.allow("5.6.7.8"));
    }

    #[test]
    fn a_new_window_restores_the_allowance() {
        let limiter = RateLimiter::new(1);
        let start = Instant::now();
        assert!(limiter.allow_at("1.2.3.4", start));
        assert!(!limiter.allow_at("1.2.3.4", start));
        // One second past the window boundary.
        let later = start + WINDOW + Duration::from_secs(1);
        assert!(limiter.allow_at("1.2.3.4", later));
    }

    #[test]
    fn sweeps_expired_buckets_rather_than_growing_without_bound() {
        let limiter = RateLimiter::new(1_000);
        let start = Instant::now();
        for i in 0..=SWEEP_THRESHOLD {
            limiter.allow_at(&format!("10.0.{}.{}", i / 256, i % 256), start);
        }
        assert!(limiter.buckets.lock().unwrap().len() > SWEEP_THRESHOLD);

        // One request in a later window sweeps everything the old one left.
        let later = start + WINDOW + Duration::from_secs(1);
        limiter.allow_at("192.168.0.1", later);
        assert_eq!(limiter.buckets.lock().unwrap().len(), 1);
    }

    #[test]
    fn reads_the_original_client_from_the_left_of_the_forwarded_chain() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-forwarded-for",
            HeaderValue::from_static("203.0.113.7, 70.41.3.18, 150.172.238.178"),
        );
        assert_eq!(client_key(&headers, None), "203.0.113.7");
    }

    #[test]
    fn falls_back_through_real_ip_to_the_socket_peer() {
        let mut headers = HeaderMap::new();
        headers.insert("x-real-ip", HeaderValue::from_static("198.51.100.9"));
        assert_eq!(client_key(&headers, None), "198.51.100.9");

        let peer: SocketAddr = "192.0.2.44:51234".parse().unwrap();
        assert_eq!(client_key(&HeaderMap::new(), Some(peer)), "192.0.2.44");
        assert_eq!(client_key(&HeaderMap::new(), None), "unknown");
    }

    #[test]
    fn an_empty_forwarded_header_does_not_become_the_key() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", HeaderValue::from_static("  "));
        let peer: SocketAddr = "192.0.2.44:51234".parse().unwrap();
        assert_eq!(client_key(&headers, Some(peer)), "192.0.2.44");
    }

    #[tokio::test]
    async fn refusal_carries_the_rate_limited_code_and_a_hint() {
        use axum::body::to_bytes;

        let limiter = RateLimiter::new(600);
        let response = limiter.too_many();
        assert_eq!(response.status(), axum::http::StatusCode::TOO_MANY_REQUESTS);

        let bytes = to_bytes(response.into_body(), 8 * 1024).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["code"], "rate_limited");
        assert_eq!(body["error"], "Too many requests");
        assert_eq!(
            body["hint"],
            "This client exceeded 600 API calls per minute."
        );
    }
}
