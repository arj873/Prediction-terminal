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
/// Sweep once the map is bigger than this. Bounded growth matters more than
/// precision — a sweep costs nothing next to an upstream call.
const SWEEP_THRESHOLD: usize = 5_000;
/// What a sweep leaves behind when dropping expired buckets was not enough.
///
/// Sweeping only *expired* buckets is no bound at all against a client varying
/// its key: every bucket is live, nothing is dropped, and the map then runs an
/// O(n) scan on every subsequent request while continuing to grow. Evicting the
/// oldest down to a low-water mark makes the scan amortised — it happens once
/// per `SWEEP_THRESHOLD - LOW_WATER` requests rather than on all of them — and
/// puts a hard ceiling on the memory one caller can cost.
const LOW_WATER: usize = 4_000;

struct Bucket {
    count: u32,
    reset_at: Instant,
}

/// Shared counter state. Cloneable; every clone shares one map.
#[derive(Clone)]
pub struct RateLimiter {
    buckets: Arc<Mutex<HashMap<String, Bucket>>>,
    max_per_window: u32,
    /// Whether a forwarded header may name the client. See [`client_key`].
    trust_proxy: bool,
}

impl RateLimiter {
    pub fn new(max_per_window: u32, trust_proxy: bool) -> Self {
        Self {
            buckets: Arc::new(Mutex::new(HashMap::new())),
            max_per_window,
            trust_proxy,
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

            // Everything still live and still over the mark: the caller is
            // varying its key faster than windows expire. Drop the oldest.
            //
            // Removing exactly `len - LOW_WATER` keys rather than everything
            // older than a cut-off, because the case this exists for creates
            // its buckets in one burst — they share a `reset_at`, and a
            // threshold comparison would empty the map instead of trimming it.
            // Emptying is the wrong failure: it resets the counter of the very
            // caller that forced the sweep.
            if buckets.len() > LOW_WATER {
                let mut by_age: Vec<(Instant, String)> = buckets
                    .iter()
                    .map(|(key, bucket)| (bucket.reset_at, key.clone()))
                    .collect();
                let excess = buckets.len() - LOW_WATER;
                // Oldest first, keyed for a stable order among equal instants.
                by_age.sort_unstable();
                for (_, key) in by_age.into_iter().take(excess) {
                    buckets.remove(&key);
                }
            }
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
/// A forwarded header is a claim by whoever sent it, and only a proxy that
/// rewrites the chain makes it a fact. Where nothing does, reading it hands the
/// caller its own bucket for the asking: vary `X-Forwarded-For` per request and
/// the limit is not a limit. So the peer address — the one thing the client
/// cannot choose — is the default, and the headers are consulted only where the
/// deployment has said it is behind a proxy.
///
/// When it is, the *leftmost* entry is the original client; the rest are hops.
fn client_key(headers: &HeaderMap, peer: Option<SocketAddr>, trust_proxy: bool) -> String {
    if trust_proxy {
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
    let key = client_key(request.headers(), peer, limiter.trust_proxy);

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
        let limiter = RateLimiter::new(3, false);
        assert!(limiter.allow("1.2.3.4"));
        assert!(limiter.allow("1.2.3.4"));
        assert!(limiter.allow("1.2.3.4"));
        assert!(!limiter.allow("1.2.3.4"));
        assert!(!limiter.allow("1.2.3.4"));
    }

    #[test]
    fn counts_each_client_separately() {
        let limiter = RateLimiter::new(1, false);
        assert!(limiter.allow("1.2.3.4"));
        assert!(!limiter.allow("1.2.3.4"));
        // A different client still has its whole allowance.
        assert!(limiter.allow("5.6.7.8"));
    }

    #[test]
    fn a_new_window_restores_the_allowance() {
        let limiter = RateLimiter::new(1, false);
        let start = Instant::now();
        assert!(limiter.allow_at("1.2.3.4", start));
        assert!(!limiter.allow_at("1.2.3.4", start));
        // One second past the window boundary.
        let later = start + WINDOW + Duration::from_secs(1);
        assert!(limiter.allow_at("1.2.3.4", later));
    }

    #[test]
    fn sweeps_expired_buckets_rather_than_growing_without_bound() {
        let limiter = RateLimiter::new(1_000, false);
        let start = Instant::now();
        for i in 0..=SWEEP_THRESHOLD {
            limiter.allow_at(&format!("10.0.{}.{}", i / 256, i % 256), start);
        }
        // Trimmed to the low-water mark rather than left to grow.
        assert_eq!(limiter.buckets.lock().unwrap().len(), LOW_WATER);

        // A later window: once the map next reaches the sweep threshold, every
        // bucket the old window left is expired and goes in one pass.
        let later = start + WINDOW + Duration::from_secs(1);
        for i in 0..=(SWEEP_THRESHOLD - LOW_WATER) {
            limiter.allow_at(&format!("192.168.{}.{}", i / 256, i % 256), later);
        }

        let held = limiter.buckets.lock().unwrap();
        assert!(
            held.len() <= SWEEP_THRESHOLD - LOW_WATER + 1,
            "expired buckets survived the sweep: {} held",
            held.len()
        );
        assert!(
            held.keys().all(|key| key.starts_with("192.168.")),
            "only the live window's buckets should remain"
        );
    }

    /// The bound that has to hold against a caller *choosing* its key.
    ///
    /// Sweeping only expired buckets is no bound at all here: every bucket is
    /// live, so nothing is dropped and the map grows while paying an O(n) scan
    /// on every request past the threshold.
    #[test]
    fn caps_the_map_when_every_bucket_is_still_live() {
        let limiter = RateLimiter::new(1_000, false);
        let now = Instant::now();

        for i in 0..(SWEEP_THRESHOLD * 3) {
            // Every key distinct, every window still open.
            limiter.allow_at(
                &format!("10.{}.{}.{}", i / 65_536, (i / 256) % 256, i % 256),
                now,
            );
        }

        let held = limiter.buckets.lock().unwrap().len();
        assert!(
            held <= SWEEP_THRESHOLD + 1,
            "map grew to {held} against a threshold of {SWEEP_THRESHOLD}"
        );
    }

    #[test]
    fn ignores_a_forwarded_header_unless_the_deployment_trusts_one() {
        // The security-relevant default. Without a proxy in front, this header
        // is a claim by the caller, and honouring it hands them a fresh bucket
        // per request for the asking.
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", HeaderValue::from_static("203.0.113.7"));
        headers.insert("x-real-ip", HeaderValue::from_static("198.51.100.9"));
        let peer: SocketAddr = "192.0.2.44:51234".parse().unwrap();

        assert_eq!(client_key(&headers, Some(peer), false), "192.0.2.44");
    }

    #[test]
    fn a_client_rotating_the_header_still_shares_one_bucket() {
        let limiter = RateLimiter::new(5, false);
        let peer: SocketAddr = "192.0.2.44:51234".parse().unwrap();
        let now = Instant::now();

        let mut allowed = 0;
        for i in 0..12 {
            let mut headers = HeaderMap::new();
            let claimed = format!("203.0.113.{i}");
            headers.insert("x-forwarded-for", HeaderValue::from_str(&claimed).unwrap());
            if limiter.allow_at(&client_key(&headers, Some(peer), false), now) {
                allowed += 1;
            }
        }

        assert_eq!(allowed, 5, "the cap must hold however the header varies");
    }

    #[test]
    fn a_deployment_behind_a_real_proxy_still_buckets_clients_apart() {
        // The other half: turning it on must actually work, or the setting is
        // useless to anyone who genuinely needs it.
        let limiter = RateLimiter::new(5, true);
        let peer: SocketAddr = "10.0.0.1:4000".parse().unwrap();
        let now = Instant::now();

        let key_for = |client: &str| {
            let mut headers = HeaderMap::new();
            headers.insert("x-forwarded-for", HeaderValue::from_str(client).unwrap());
            client_key(&headers, Some(peer), true)
        };

        for _ in 0..5 {
            assert!(limiter.allow_at(&key_for("203.0.113.7"), now));
        }
        // One client is spent; a different one behind the same proxy is not.
        assert!(!limiter.allow_at(&key_for("203.0.113.7"), now));
        assert!(limiter.allow_at(&key_for("198.51.100.9"), now));
    }

    #[test]
    fn reads_the_original_client_from_the_left_of_the_forwarded_chain() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-forwarded-for",
            HeaderValue::from_static("203.0.113.7, 70.41.3.18, 150.172.238.178"),
        );
        assert_eq!(client_key(&headers, None, true), "203.0.113.7");
    }

    #[test]
    fn falls_back_through_real_ip_to_the_socket_peer() {
        let mut headers = HeaderMap::new();
        headers.insert("x-real-ip", HeaderValue::from_static("198.51.100.9"));
        assert_eq!(client_key(&headers, None, true), "198.51.100.9");

        let peer: SocketAddr = "192.0.2.44:51234".parse().unwrap();
        assert_eq!(
            client_key(&HeaderMap::new(), Some(peer), true),
            "192.0.2.44"
        );
        assert_eq!(client_key(&HeaderMap::new(), None, true), "unknown");
    }

    #[test]
    fn an_empty_forwarded_header_does_not_become_the_key() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", HeaderValue::from_static("  "));
        let peer: SocketAddr = "192.0.2.44:51234".parse().unwrap();
        assert_eq!(client_key(&headers, Some(peer), true), "192.0.2.44");
    }

    #[tokio::test]
    async fn refusal_carries_the_rate_limited_code_and_a_hint() {
        use axum::body::to_bytes;

        let limiter = RateLimiter::new(600, false);
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
