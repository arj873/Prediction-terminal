//! Outbound HTTP with the things every scraper needs and none of the things it
//! doesn't: a browser-shaped header set, a hard timeout, bounded retries with
//! jittered backoff, and a response-size ceiling so a pathological upstream
//! can't exhaust memory.
//!
//! Everything here funnels into one place so that *how* a failure is described
//! is decided once. A source module calls [`Http::fetch_text`] and gets either a
//! body or an [`UpstreamError`] whose `code` and `hint` are already right for
//! the panel that will display them — it never has to reason about whether a
//! connection reset means "the host is down" or "the host dislikes datacentre
//! IPs", because that judgement lives in [`normalise_transport_error`] and in
//! the 5xx body peek below.

use std::sync::LazyLock;
use std::time::Duration;

use futures::StreamExt;
use rand::Rng;
use regex::Regex;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, CONTENT_TYPE};
use reqwest::{Client, Response, Url};
use serde::de::DeserializeOwned;

use crate::error::{codes, Result, UpstreamError};

/// A recent desktop Chrome on macOS.
///
/// Pinned rather than generated: several of the hosts here vary their response
/// by user agent, and a UA that drifts turns a parser regression into a mystery.
pub const DESKTOP_UA: &str =
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/127.0.0.0 Safari/537.36";

/// Headers a real Chrome sends. Several of these hosts 403 without them.
const BROWSER_HEADERS: &[(&str, &str)] = &[
    ("User-Agent", DESKTOP_UA),
    (
        "Accept",
        "text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,*/*;q=0.8",
    ),
    ("Accept-Language", "en-US,en;q=0.9"),
    ("Cache-Control", "no-cache"),
    ("Pragma", "no-cache"),
    ("Sec-Fetch-Dest", "document"),
    ("Sec-Fetch-Mode", "navigate"),
    ("Sec-Fetch-Site", "none"),
    ("Sec-Fetch-User", "?1"),
    ("Upgrade-Insecure-Requests", "1"),
];

const DEFAULT_MAX_BYTES: usize = 16 * 1024 * 1024;
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(20);
const DEFAULT_RETRIES: u32 = 2;

/// How much of a 5xx body is worth reading to classify it. Enough for an
/// Envoy-style error line, small enough that a huge error page costs nothing.
const PEEK_BYTES: usize = 1024;

/// Body text an egress proxy emits when the *origin* dropped the connection,
/// rather than answering. Envoy and friends phrase it as "upstream connect
/// error or disconnect/reset before headers".
static CONNECT_FAILURE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)(?:upstream connect error|disconnect/reset before headers|connection (?:reset|refused)|no healthy upstream|remote reset)",
    )
    .expect("CONNECT_FAILURE is a valid regex")
});

/// Transport messages that mean "nobody ever answered".
static TIMEOUT_MESSAGE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)(?:timed?\s*out|timeout)").expect("valid regex"));

/// Transport messages that mean "the far end hung up on us".
static BLOCKED_MESSAGE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(?:reset|hang up|EPIPE|ECONNREFUSED|fetch failed|socket)")
        .expect("valid regex")
});

/// Per-request knobs. Every field has a default that is right for a scrape, so
/// the common call is `Http::fetch_text(url, FetchOptions::default())`.
#[derive(Debug, Clone)]
pub struct FetchOptions {
    /// Extra headers, applied *after* the default set so a caller can override
    /// any of it (an API `Accept`, an auth token, a `Referer`).
    pub headers: Vec<(String, String)>,
    /// Timeout for a single attempt — not for the call as a whole. Three
    /// attempts against a dead host take up to three times this.
    pub timeout: Duration,
    /// Additional attempts after the first.
    pub retries: u32,
    /// Send the browser-shaped header set. Off for JSON APIs, which get a UA
    /// and nothing else.
    pub browser_headers: bool,
    /// Reject bodies larger than this.
    pub max_bytes: usize,
}

impl Default for FetchOptions {
    fn default() -> Self {
        Self {
            headers: Vec::new(),
            timeout: DEFAULT_TIMEOUT,
            retries: DEFAULT_RETRIES,
            browser_headers: true,
            max_bytes: DEFAULT_MAX_BYTES,
        }
    }
}

impl FetchOptions {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Add one header. Later additions win, as does anything added here over
    /// the default set.
    #[must_use]
    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    /// Per-attempt timeout.
    #[must_use]
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Attempts *after* the first. `0` means try once and report.
    #[must_use]
    pub fn retries(mut self, retries: u32) -> Self {
        self.retries = retries;
        self
    }

    #[must_use]
    pub fn browser_headers(mut self, on: bool) -> Self {
        self.browser_headers = on;
        self
    }

    #[must_use]
    pub fn max_bytes(mut self, max_bytes: usize) -> Self {
        self.max_bytes = max_bytes;
        self
    }

    /// Resolve the option set into the headers actually sent.
    ///
    /// A header the caller cannot express on the wire is an error rather than a
    /// silent drop: the usual cause is a credential read from the environment
    /// with a stray newline, and losing it quietly produces a baffling 401
    /// three layers away from the mistake.
    fn build_headers(&self) -> Result<HeaderMap> {
        let mut map = HeaderMap::with_capacity(BROWSER_HEADERS.len() + self.headers.len());

        if self.browser_headers {
            for (name, value) in BROWSER_HEADERS {
                insert_header(&mut map, name, value)?;
            }
        } else {
            insert_header(&mut map, "User-Agent", DESKTOP_UA)?;
        }

        for (name, value) in &self.headers {
            insert_header(&mut map, name, value)?;
        }

        Ok(map)
    }
}

fn insert_header(map: &mut HeaderMap, name: &str, value: &str) -> Result<()> {
    let name = HeaderName::try_from(name)
        .map_err(|_| UpstreamError::bad_request(format!("Invalid request header name {name:?}")))?;
    let value = HeaderValue::from_str(value).map_err(|_| {
        UpstreamError::bad_request(format!("Invalid value for request header {name}"))
    })?;
    map.insert(name, value);
    Ok(())
}

/// The outbound HTTP client.
///
/// Holds one [`Client`], and so one connection pool: cloning is cheap and every
/// clone shares the pool. Build it once and pass it down.
#[derive(Debug, Clone)]
pub struct Http {
    client: Client,
}

impl Default for Http {
    fn default() -> Self {
        Self::new()
    }
}

impl Http {
    /// A client configured the way every source here wants one.
    ///
    /// No client-wide timeout: the timeout is applied per attempt at the
    /// request builder, so a retry gets a fresh budget rather than racing the
    /// remains of the first one's.
    ///
    /// # Panics
    ///
    /// If the TLS backend cannot be initialised, which is a broken build rather
    /// than a runtime condition worth threading a `Result` through startup for.
    #[must_use]
    pub fn new() -> Self {
        let client = Client::builder()
            .gzip(true)
            .brotli(true)
            .deflate(true)
            .redirect(reqwest::redirect::Policy::limited(10))
            .build()
            .expect("the HTTP client could not be built");
        Self::with_client(client)
    }

    /// Wrap an already-configured client — a test fixture, or one with a proxy
    /// or a different redirect policy.
    #[must_use]
    pub fn with_client(client: Client) -> Self {
        Self { client }
    }

    /// Fetch `url` as text, retrying transient failures.
    ///
    /// Fails with an [`UpstreamError`] on a non-retryable status, on exhausting
    /// retries, or on a transport failure. A connection reset — how bot-protected
    /// hosts usually say no to a datacentre IP — surfaces as `upstream_blocked`
    /// with a hint, because that is the failure operators actually hit.
    pub async fn fetch_text(&self, url: &str, opts: FetchOptions) -> Result<String> {
        let (bytes, _) = self.fetch_capped(url, &opts).await?;
        // Lossy, matching the `TextDecoder` this replaces: a stray bad byte in
        // an otherwise readable page should not lose the whole page.
        Ok(match String::from_utf8(bytes) {
            Ok(text) => text,
            Err(err) => String::from_utf8_lossy(err.as_bytes()).into_owned(),
        })
    }

    /// JSON convenience wrapper. Sends an API-shaped `Accept` instead of a page
    /// one — the browser header set is for HTML endpoints, and some APIs answer
    /// it with HTML.
    pub async fn fetch_json<T: DeserializeOwned>(
        &self,
        url: &str,
        opts: FetchOptions,
    ) -> Result<T> {
        let mut opts = opts;
        opts.browser_headers = false;
        // Inserted first so a caller's own `Accept` still wins.
        opts.headers
            .insert(0, ("Accept".to_owned(), "application/json".to_owned()));

        let text = self.fetch_text(url, opts).await?;
        serde_json::from_str(&text).map_err(|_| {
            UpstreamError::new(
                format!("{} returned a body that is not valid JSON", host_of(url)),
                codes::BAD_UPSTREAM_BODY,
            )
        })
    }

    /// Fetch `url` as bytes, returning the body and its `Content-Type`.
    ///
    /// Same retry and size-cap machinery as [`Http::fetch_text`]; exists because
    /// the artwork proxy has to pass the upstream's content type through and
    /// must not decode the body as text.
    pub async fn fetch_bytes(
        &self,
        url: &str,
        opts: FetchOptions,
    ) -> Result<(Vec<u8>, Option<String>)> {
        self.fetch_capped(url, &opts).await
    }

    /// The retry loop every public method shares.
    async fn fetch_capped(
        &self,
        url: &str,
        opts: &FetchOptions,
    ) -> Result<(Vec<u8>, Option<String>)> {
        let headers = opts.build_headers()?;
        let mut last_error: Option<UpstreamError> = None;

        for attempt in 0..=opts.retries {
            if attempt > 0 {
                tokio::time::sleep(backoff(attempt)).await;
            }

            let sent = self
                .client
                .get(url)
                .headers(headers.clone())
                .timeout(opts.timeout)
                .send()
                .await;

            let response = match sent {
                Ok(response) => response,
                Err(err) => {
                    last_error = Some(normalise_transport_error(&err, url));
                    if attempt == opts.retries {
                        break;
                    }
                    tracing::debug!(
                        url,
                        attempt,
                        error = %error_chain(&err),
                        "retrying after transport failure"
                    );
                    continue;
                }
            };

            let status = response.status().as_u16();
            if response.status().is_success() {
                return read_capped(response, opts.max_bytes, url).await;
            }

            // A 502/503 can be the origin being down *or* an egress proxy
            // reporting that the origin hung up on it. The body distinguishes
            // them, and the remediation is completely different, so read it
            // before deciding.
            let snippet = if status >= 500 {
                peek(response).await
            } else {
                String::new()
            };

            if status >= 500 && CONNECT_FAILURE.is_match(&snippet) {
                // Not an outage and not worth a second attempt: the origin is
                // refusing this network, and it will refuse the retry too.
                return Err(UpstreamError::blocked(format!(
                    "{} refused the connection",
                    host_of(url)
                ))
                .with_status(status)
                .with_hint(blocked_hint(url)));
            }

            if is_retryable_status(status) && attempt < opts.retries {
                tracing::debug!(url, attempt, status, "retrying after upstream status");
                last_error = Some(status_error(status, url));
                continue;
            }

            // A definitive upstream answer (404, 403, 400) is not worth
            // retrying: asking again gets the same answer more slowly.
            return Err(status_error(status, url));
        }

        let failure = last_error.unwrap_or_else(|| {
            UpstreamError::new(
                format!("Request to {} failed", host_of(url)),
                codes::UPSTREAM_ERROR,
            )
        });
        // Warned, not debugged. This is the last place the failure is a fact
        // rather than a rendered sentence, and on a hosted deployment the log
        // is the only place an operator can see it without a browser.
        tracing::warn!(
            url,
            attempts = opts.retries + 1,
            code = failure.code,
            error = failure.message,
            "upstream fetch failed"
        );
        Err(failure)
    }
}

/// Statuses where trying again might plausibly help.
fn is_retryable_status(status: u16) -> bool {
    status == 408 || status == 425 || status == 429 || (500..=599).contains(&status)
}

/// 400ms, 800ms, 1600ms … plus jitter, so parallel panels don't sync up and
/// hammer a struggling upstream in lockstep.
fn backoff(attempt: u32) -> Duration {
    // Saturating so an absurd `retries` cannot overflow into a panic.
    let base = 400u64.saturating_mul(1u64 << (attempt - 1).min(20));
    let jitter = rand::thread_rng().gen_range(0..250);
    Duration::from_millis(base.saturating_add(jitter))
}

/// The error for a status we are not going to retry.
fn status_error(status: u16, url: &str) -> UpstreamError {
    // A 404 is a statement about the resource, not about the upstream's health,
    // and callers fall through provider chains on `upstream_status` but stop on
    // `not_found`.
    let code = if status == 404 {
        codes::NOT_FOUND
    } else {
        codes::UPSTREAM_STATUS
    };

    let error = UpstreamError::new(format!("{} returned HTTP {status}", host_of(url)), code)
        .with_status(status);

    match describe_status(status, url) {
        Some(hint) => error.with_hint(hint),
        None => error,
    }
}

/// Read a body, refusing to hold more than `max_bytes` of it.
///
/// The cap is enforced while streaming rather than after buffering: measuring
/// afterwards means the pathological response has already been in memory, which
/// is the thing the cap exists to prevent.
async fn read_capped(
    response: Response,
    max_bytes: usize,
    url: &str,
) -> Result<(Vec<u8>, Option<String>)> {
    let status = response.status().as_u16();
    let content_type = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);

    // A declared length over the cap is refused before a byte of body is read.
    // Absent for compressed responses, where the framing length describes the
    // compressed size — the streaming check below is what catches those.
    if let Some(declared) = response.content_length() {
        if declared > u64::try_from(max_bytes).unwrap_or(u64::MAX) {
            return Err(too_large(url, max_bytes, status));
        }
    }

    let mut stream = std::pin::pin!(response.bytes_stream());
    let mut body: Vec<u8> = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|err| normalise_transport_error(&err, url))?;
        if body.len().saturating_add(chunk.len()) > max_bytes {
            // Dropping the stream closes the connection, so the rest is never
            // transferred.
            return Err(too_large(url, max_bytes, status));
        }
        body.extend_from_slice(&chunk);
    }

    Ok((body, content_type))
}

fn too_large(url: &str, max_bytes: usize, status: u16) -> UpstreamError {
    UpstreamError::new(
        format!("Response from {} exceeds {max_bytes} bytes", host_of(url)),
        codes::RESPONSE_TOO_LARGE,
    )
    .with_status(status)
}

/// Read at most 1 KiB of an error body without draining a huge response.
///
/// Errors and early ends are swallowed deliberately: this runs while we are
/// already deciding how to report a failure, and a failure to read the
/// explanation must not replace the failure being explained.
async fn peek(response: Response) -> String {
    let mut stream = std::pin::pin!(response.bytes_stream());
    let mut buffer: Vec<u8> = Vec::with_capacity(PEEK_BYTES);

    while buffer.len() < PEEK_BYTES {
        match stream.next().await {
            Some(Ok(chunk)) => buffer.extend_from_slice(&chunk),
            Some(Err(_)) | None => break,
        }
    }

    buffer.truncate(PEEK_BYTES);
    String::from_utf8_lossy(&buffer).into_owned()
}

/// The host of `url`, or the raw string when it will not parse.
///
/// Mirrors `new URL(u).host`: the port is included when it is not the default
/// for the scheme, so `example.test:8080` and `example.test` are distinguished
/// in messages the reader sees.
#[must_use]
pub fn host_of(url: &str) -> String {
    let Ok(parsed) = Url::parse(url) else {
        return url.to_owned();
    };
    let Some(host) = parsed.host_str() else {
        return String::new();
    };
    match parsed.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_owned(),
    }
}

fn blocked_hint(url: &str) -> String {
    let host = host_of(url);
    format!(
        "{host} reset the connection before sending a response. Hosts behind bot \
         protection commonly do this to datacentre and cloud IPs — the same request \
         usually succeeds from a residential connection."
    )
}

fn describe_status(status: u16, url: &str) -> Option<String> {
    let host = host_of(url);
    match status {
        403 => Some(format!(
            "{host} refused the request (403) — it may be blocking this IP."
        )),
        429 => Some(format!(
            "{host} is rate-limiting this IP. Wait a moment and retry."
        )),
        404 => Some(format!(
            "{host} has no such resource. Check the identifier."
        )),
        500..=599 => Some(format!(
            "{host} is returning {status}. This is an upstream outage."
        )),
        _ => None,
    }
}

/// Turn a transport failure into something a reader can act on.
///
/// The distinction that matters is timeout versus refusal. A timeout means the
/// host accepted the connection and never replied; a reset means it did not
/// want to talk to us at all, which is nearly always bot protection turning
/// away datacentre egress — and is emphatically not a bug in the parser
/// downstream, which is what it looks like if the error says only "request
/// failed".
fn normalise_transport_error(err: &reqwest::Error, url: &str) -> UpstreamError {
    let host = host_of(url);
    let message = error_chain(err);

    // `is_timeout` walks the source chain and is not fooled by wording, but the
    // message check stays: it also catches timeouts reported by an intermediary
    // rather than by our own clock.
    if err.is_timeout() || TIMEOUT_MESSAGE.is_match(&message) {
        return UpstreamError::timeout(format!("{host} did not respond in time")).with_hint(
            format!(
                "{host} accepted the connection but never replied. It may be throttling \
             this IP. The transport said: {message}"
            ),
        );
    }

    // ECONNRESET / EPIPE / "socket hang up" from a host that dislikes
    // datacentre egress.
    if err.is_connect() || BLOCKED_MESSAGE.is_match(&message) {
        // The chain goes in the hint rather than being dropped. `BLOCKED_MESSAGE`
        // matches on the word "socket" among others, so a TLS or proxy fault can
        // land here and be reported as a refusal — and the sentence that would
        // have said which was thrown away with it.
        return UpstreamError::blocked(format!("{host} refused the connection")).with_hint(
            format!("{} The transport said: {message}", blocked_hint(url)),
        );
    }

    UpstreamError::new(
        format!("Request to {host} failed: {message}"),
        codes::UPSTREAM_ERROR,
    )
}

/// Flatten an error and its causes into one string.
///
/// `reqwest`'s own `Display` is usually just "error sending request for url
/// (…)"; the words worth matching on — "connection reset", "timed out" — live
/// in the source chain underneath it.
fn error_chain(err: &dyn std::error::Error) -> String {
    let mut message = err.to_string();
    let mut source = err.source();
    while let Some(cause) = source {
        message.push_str(": ");
        message.push_str(&cause.to_string());
        source = cause.source();
    }
    message
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::net::SocketAddr;

    use serde::Deserialize;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// Fast defaults: the point of a test is the classification, not the wait.
    fn opts() -> FetchOptions {
        FetchOptions::new()
            .retries(0)
            .timeout(Duration::from_secs(5))
    }

    fn http() -> Http {
        Http::new()
    }

    // ---- retry policy -----------------------------------------------------

    #[tokio::test]
    async fn retries_a_503_and_then_succeeds() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/flaky"))
            .respond_with(ResponseTemplate::new(503))
            .up_to_n_times(1)
            .with_priority(1)
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/flaky"))
            .respond_with(ResponseTemplate::new(200).set_body_string("second time lucky"))
            .expect(1)
            .mount(&server)
            .await;

        let body = http()
            .fetch_text(&format!("{}/flaky", server.uri()), opts().retries(1))
            .await
            .expect("the retry should have succeeded");

        assert_eq!(body, "second time lucky");
    }

    #[tokio::test]
    async fn does_not_retry_a_404() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(404))
            .expect(1)
            .mount(&server)
            .await;

        let err = http()
            .fetch_text(&format!("{}/gone", server.uri()), opts().retries(3))
            .await
            .expect_err("a 404 is a definitive answer");

        assert_eq!(err.code, codes::NOT_FOUND);
        // `expect(1)` is verified when the server drops: three more attempts
        // would fail the test there.
    }

    #[tokio::test]
    async fn a_404_carries_not_found_and_a_hint() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let err = http()
            .fetch_text(&format!("{}/missing", server.uri()), opts())
            .await
            .expect_err("404");

        assert_eq!(err.code, codes::NOT_FOUND);
        assert_eq!(err.status, Some(404));
        assert!(
            err.hint
                .as_deref()
                .is_some_and(|hint| hint.contains("no such resource")),
            "expected a hint naming the resource, got {:?}",
            err.hint
        );
    }

    #[tokio::test]
    async fn does_not_retry_a_403() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(403))
            .expect(1)
            .mount(&server)
            .await;

        let err = http()
            .fetch_text(&format!("{}/denied", server.uri()), opts().retries(2))
            .await
            .expect_err("403");

        assert_eq!(err.code, codes::UPSTREAM_STATUS);
        assert_eq!(err.status, Some(403));
        assert!(err.hint.as_deref().is_some_and(|h| h.contains("403")));
    }

    #[test]
    fn only_transient_statuses_are_retryable() {
        for status in [408, 425, 429, 500, 502, 503, 504, 599] {
            assert!(is_retryable_status(status), "{status} should be retryable");
        }
        for status in [400, 401, 403, 404, 410, 418, 422, 451] {
            assert!(!is_retryable_status(status), "{status} should be final");
        }
    }

    // ---- the 5xx body peek ------------------------------------------------

    #[tokio::test]
    async fn a_5xx_connect_failure_is_blocked_and_not_retried() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(503).set_body_string(
                "upstream connect error or disconnect/reset before headers. reset reason: remote reset",
            ))
            .expect(1)
            .mount(&server)
            .await;

        let err = http()
            .fetch_text(&format!("{}/proxied", server.uri()), opts().retries(3))
            .await
            .expect_err("a proxy-reported connect failure");

        // The status alone would say "retryable outage"; the body says the
        // origin hung up, which no number of retries will change.
        assert_eq!(err.code, codes::UPSTREAM_BLOCKED);
        assert_eq!(err.status, Some(503));
        assert!(err
            .hint
            .as_deref()
            .is_some_and(|hint| hint.contains("bot protection")));

        let calls = server.received_requests().await.unwrap_or_default();
        assert_eq!(calls.len(), 1, "a blocked 5xx must not be retried");
    }

    #[tokio::test]
    async fn a_plain_5xx_is_still_retried() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(502).set_body_string("<h1>Bad Gateway</h1>"))
            .expect(2)
            .mount(&server)
            .await;

        let err = http()
            .fetch_text(&format!("{}/down", server.uri()), opts().retries(1))
            .await
            .expect_err("502");

        assert_eq!(err.code, codes::UPSTREAM_STATUS);
        assert_eq!(err.status, Some(502));
    }

    // ---- the size cap -----------------------------------------------------

    #[tokio::test]
    async fn rejects_a_body_whose_declared_length_exceeds_the_cap() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![b'x'; 64 * 1024]))
            .mount(&server)
            .await;

        let err = http()
            .fetch_text(&format!("{}/fat", server.uri()), opts().max_bytes(1024))
            .await
            .expect_err("64 KiB against a 1 KiB cap");

        assert_eq!(err.code, codes::RESPONSE_TOO_LARGE);
        assert!(
            err.message.contains("exceeds 1024 bytes"),
            "{}",
            err.message
        );
    }

    #[tokio::test]
    async fn the_declared_length_is_refused_before_the_body_is_read() {
        // A header promising far more than the cap, in front of a body that
        // would sail under it. Only the `Content-Length` check can fail this —
        // which is the point: an upstream that announces a gigabyte should cost
        // us one round trip, not a gigabyte of transfer.
        let url = mislabelled_length_server().await;

        let err = http()
            .fetch_text(&url, opts().max_bytes(1024))
            .await
            .expect_err("a declared gigabyte against a 1 KiB cap");

        assert_eq!(err.code, codes::RESPONSE_TOO_LARGE);
    }

    #[tokio::test]
    async fn rejects_a_streamed_body_that_exceeds_the_cap() {
        // No `Content-Length` to check, so only the running total can catch
        // this — which is the case the streaming cap exists for.
        let url = endless_chunked_server().await;

        let err = http()
            .fetch_bytes(&url, opts().max_bytes(16 * 1024))
            .await
            .expect_err("an endless body against a 16 KiB cap");

        assert_eq!(err.code, codes::RESPONSE_TOO_LARGE);
    }

    #[tokio::test]
    async fn accepts_a_body_that_fits() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![b'x'; 900]))
            .mount(&server)
            .await;

        let (body, _) = http()
            .fetch_bytes(&format!("{}/ok", server.uri()), opts().max_bytes(1024))
            .await
            .expect("900 bytes fit under a 1 KiB cap");

        assert_eq!(body.len(), 900);
    }

    #[tokio::test]
    async fn fetch_bytes_returns_the_content_type() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(vec![0u8, 1, 2, 3], "image/webp"))
            .mount(&server)
            .await;

        let (body, content_type) = http()
            .fetch_bytes(&format!("{}/art.webp", server.uri()), opts())
            .await
            .expect("bytes");

        assert_eq!(body, vec![0u8, 1, 2, 3]);
        assert_eq!(content_type.as_deref(), Some("image/webp"));
    }

    // ---- JSON -------------------------------------------------------------

    #[derive(Debug, Deserialize, PartialEq, Eq)]
    struct Payload {
        ticker: String,
    }

    #[tokio::test]
    async fn fetch_json_parses_a_json_body() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"ticker":"KXBTC"}"#))
            .mount(&server)
            .await;

        let payload: Payload = http()
            .fetch_json(&format!("{}/api", server.uri()), opts())
            .await
            .expect("json");

        assert_eq!(payload.ticker, "KXBTC");
    }

    #[tokio::test]
    async fn fetch_json_rejects_a_body_that_is_not_json() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string("<!doctype html><html>"))
            .mount(&server)
            .await;

        let err = http()
            .fetch_json::<Payload>(&format!("{}/api", server.uri()), opts())
            .await
            .expect_err("an HTML body where JSON was promised");

        assert_eq!(err.code, codes::BAD_UPSTREAM_BODY);
        assert!(err.message.contains("not valid JSON"), "{}", err.message);
    }

    // ---- headers ----------------------------------------------------------

    #[tokio::test]
    async fn fetch_text_sends_the_browser_header_set() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
            .mount(&server)
            .await;

        http()
            .fetch_text(&format!("{}/page", server.uri()), opts())
            .await
            .expect("ok");

        let sent = server.received_requests().await.expect("recording is on");
        let headers = &sent[0].headers;

        for (name, value) in BROWSER_HEADERS {
            assert_eq!(
                headers.get(*name).and_then(|v| v.to_str().ok()),
                Some(*value),
                "missing or wrong {name}"
            );
        }
    }

    #[tokio::test]
    async fn a_caller_header_overrides_the_browser_set() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
            .mount(&server)
            .await;

        http()
            .fetch_text(
                &format!("{}/page", server.uri()),
                opts().header("Referer", "https://www.billboard.com/"),
            )
            .await
            .expect("ok");

        let sent = server.received_requests().await.expect("recording is on");
        assert_eq!(
            sent[0].headers.get("referer").and_then(|v| v.to_str().ok()),
            Some("https://www.billboard.com/")
        );
    }

    #[tokio::test]
    async fn fetch_json_sends_only_a_user_agent_and_a_json_accept() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
            .mount(&server)
            .await;

        let _: serde_json::Value = http()
            .fetch_json(&format!("{}/api", server.uri()), opts())
            .await
            .expect("json");

        let sent = server.received_requests().await.expect("recording is on");
        let headers = &sent[0].headers;

        assert_eq!(
            headers.get("user-agent").and_then(|v| v.to_str().ok()),
            Some(DESKTOP_UA)
        );
        assert_eq!(
            headers.get("accept").and_then(|v| v.to_str().ok()),
            Some("application/json")
        );

        // The browser set is for HTML endpoints; an API that sees it can decide
        // to answer with a page.
        for absent in [
            "accept-language",
            "cache-control",
            "pragma",
            "sec-fetch-dest",
            "sec-fetch-mode",
            "sec-fetch-site",
            "sec-fetch-user",
            "upgrade-insecure-requests",
        ] {
            assert!(
                headers.get(absent).is_none(),
                "fetch_json should not send {absent}"
            );
        }
    }

    #[tokio::test]
    async fn a_caller_accept_still_wins_over_the_json_default() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
            .mount(&server)
            .await;

        let _: serde_json::Value = http()
            .fetch_json(
                &format!("{}/api", server.uri()),
                opts().header("Accept", "application/vnd.api+json"),
            )
            .await
            .expect("json");

        let sent = server.received_requests().await.expect("recording is on");
        assert_eq!(
            sent[0].headers.get("accept").and_then(|v| v.to_str().ok()),
            Some("application/vnd.api+json")
        );
    }

    #[tokio::test]
    async fn an_unsendable_header_is_reported_rather_than_dropped() {
        let err = http()
            .fetch_text(
                "http://127.0.0.1:1/",
                opts().header("X-Key", "secret\nvalue"),
            )
            .await
            .expect_err("a header value with a newline cannot be sent");

        assert_eq!(err.code, codes::BAD_REQUEST);
    }

    // ---- transport failures -----------------------------------------------

    #[tokio::test]
    async fn a_refused_connection_is_blocked() {
        // Port 1 on loopback: nothing listens, so the connect is refused
        // immediately.
        let err = http()
            .fetch_text("http://127.0.0.1:1/", opts())
            .await
            .expect_err("connection refused");

        assert_eq!(err.code, codes::UPSTREAM_BLOCKED);
        assert!(err
            .hint
            .as_deref()
            .is_some_and(|hint| hint.contains("bot protection")));
    }

    #[tokio::test]
    async fn the_timeout_is_per_attempt_not_per_call() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(30)))
            .mount(&server)
            .await;

        let err = http()
            .fetch_text(
                &format!("{}/slow", server.uri()),
                FetchOptions::new()
                    .timeout(Duration::from_millis(120))
                    .retries(1),
            )
            .await
            .expect_err("a host that never answers");

        assert_eq!(err.code, codes::UPSTREAM_TIMEOUT);
        assert!(err
            .hint
            .as_deref()
            .is_some_and(|hint| hint.contains("never replied")));

        // A whole-call timeout would have aborted after the first attempt.
        let calls = server.received_requests().await.unwrap_or_default();
        assert_eq!(calls.len(), 2, "each attempt gets its own timeout budget");
    }

    // ---- pure helpers -----------------------------------------------------

    #[test]
    fn host_of_extracts_a_host_or_returns_the_input() {
        assert_eq!(
            host_of("https://api.elections.kalshi.com/x?y=1"),
            "api.elections.kalshi.com"
        );
        assert_eq!(host_of("http://127.0.0.1:8080/path"), "127.0.0.1:8080");
        // The default port for the scheme is not part of the host.
        assert_eq!(host_of("https://example.test:443/"), "example.test");
        assert_eq!(host_of("not a url at all"), "not a url at all");
    }

    #[test]
    fn describe_status_speaks_to_the_failure_that_happened() {
        assert!(describe_status(403, "https://a.test/x")
            .unwrap()
            .contains("blocking this IP"));
        assert!(describe_status(429, "https://a.test/x")
            .unwrap()
            .contains("rate-limiting"));
        assert!(describe_status(404, "https://a.test/x")
            .unwrap()
            .contains("no such resource"));
        assert!(describe_status(503, "https://a.test/x")
            .unwrap()
            .contains("upstream outage"));
        assert!(describe_status(418, "https://a.test/x").is_none());
    }

    #[test]
    fn the_connect_failure_pattern_matches_what_proxies_actually_say() {
        for body in [
            "upstream connect error or disconnect/reset before headers",
            "UPSTREAM CONNECT ERROR",
            "reset reason: remote reset",
            "no healthy upstream",
            "connection reset by peer",
            "connection refused",
        ] {
            assert!(CONNECT_FAILURE.is_match(body), "should match: {body}");
        }
        for body in [
            "<h1>503 Service Temporarily Unavailable</h1>",
            "database is starting up",
        ] {
            assert!(!CONNECT_FAILURE.is_match(body), "should not match: {body}");
        }
    }

    #[test]
    fn transport_messages_are_classified_by_wording() {
        for message in ["socket timed out", "operation timeout", "timedout"] {
            assert!(TIMEOUT_MESSAGE.is_match(message), "{message}");
        }
        for message in [
            "connection reset by peer",
            "socket hang up",
            "EPIPE",
            "ECONNREFUSED",
            "fetch failed",
            "socket disconnected",
        ] {
            assert!(BLOCKED_MESSAGE.is_match(message), "{message}");
        }
        // Nothing recognisable falls through to a plain `upstream_error`.
        assert!(!TIMEOUT_MESSAGE.is_match("invalid certificate"));
        assert!(!BLOCKED_MESSAGE.is_match("invalid certificate"));
    }

    // ---- fixtures ---------------------------------------------------------

    /// Serve one hand-written HTTP/1.1 response and hang up.
    ///
    /// `wiremock` derives its own framing headers, so the two halves of the size
    /// cap — a declared length, and a body with no declared length at all — need
    /// a socket we control byte for byte.
    async fn raw_http_server(response: &'static str, then_forever: Option<String>) -> String {
        let listener = TcpListener::bind::<SocketAddr>(([127, 0, 0, 1], 0).into())
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");

        tokio::spawn(async move {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            // Whatever the request says, the answer is the same.
            let mut scratch = [0u8; 1024];
            let _ = socket.read(&mut scratch).await;

            if socket.write_all(response.as_bytes()).await.is_err() {
                return;
            }
            if let Some(chunk) = then_forever {
                // Ends when the client gives up and closes the connection.
                while socket.write_all(chunk.as_bytes()).await.is_ok() {}
            }
        });

        format!("http://{addr}/raw")
    }

    /// A chunked body that never ends: nothing to check up front, so only the
    /// running total can stop it.
    async fn endless_chunked_server() -> String {
        let payload = "a".repeat(8192);
        raw_http_server(
            "HTTP/1.1 200 OK\r\n\
             Content-Type: text/plain\r\n\
             Transfer-Encoding: chunked\r\n\r\n",
            Some(format!("{:x}\r\n{payload}\r\n", payload.len())),
        )
        .await
    }

    /// A body of two bytes behind a `Content-Length` claiming a gigabyte.
    async fn mislabelled_length_server() -> String {
        raw_http_server(
            "HTTP/1.1 200 OK\r\n\
             Content-Type: text/plain\r\n\
             Content-Length: 1073741824\r\n\r\nhi",
            None,
        )
        .await
    }
}
