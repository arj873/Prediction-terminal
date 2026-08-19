//! Ordered provider chains: several upstreams behind one answer.
//!
//! Four places in this server already fall back from one source to another —
//! FRED scrapes then tries the official API, equities try Yahoo then Nasdaq,
//! the box office walks back through earlier dates, Rotten Tomatoes guesses a
//! slug then searches. All four were written independently. Two of them were
//! even called `with_fallback`, with different signatures, different
//! both-failed behaviour, and no shared code; the rule that a `not_found` is a
//! real answer rather than a reason to try the next source was hand-copied into
//! all four.
//!
//! A chain states the things those copies had in common and parameterises the
//! things they did not:
//!
//!   - the order, as a list rather than a hardcoded pair;
//!   - whether a provider is usable at all in this deployment (an unset key);
//!   - which error codes are a definitive answer and end the chain;
//!   - what error to raise when every provider has failed;
//!   - and which provider actually answered, returned rather than tracked by a
//!     mutated closure variable or a string literal at each construction site.
//!
//! That last one is why this returns [`Attributed`] rather than a bare value: a
//! reader deciding how much to trust a number wants to know whether it came
//! from the source of record or the fallback, and four of the response types
//! that have a fallback behind them could not say.
//!
//! ```no_run
//! # use terminal_server::error::Result;
//! # use terminal_server::providers::{Chain, Provider};
//! # async fn scrape() -> Result<u8> { Ok(1) }
//! # async fn via_api() -> Result<u8> { Ok(2) }
//! # async fn example(has_key: bool) -> Result<()> {
//! let answer = Chain::new(format!("series {}", "GDP"))
//!     .log_prefix("fred")
//!     .provider(Provider::new("scrape", "fred.stlouisfed.org", scrape()))
//!     .provider(
//!         Provider::new("api", "the FRED API", via_api())
//!             .available(has_key)
//!             .skipped_hint(|cause| {
//!                 (cause.code == "upstream_blocked")
//!                     .then(|| "Set FRED_API_KEY to use the official API instead.".to_string())
//!             }),
//!     )
//!     .first_answer()
//!     .await?;
//! assert_eq!(answer.source, "scrape");
//! # Ok(())
//! # }
//! ```

use std::future::Future;

use futures::future::BoxFuture;
use futures::FutureExt;

use crate::error::{codes, Result, UpstreamError};

/// Builds the hint a skipped provider contributes, given the failure that will
/// otherwise be reported.
///
/// Unlike the TypeScript, the cause is never absent: the hook is only consulted
/// when some other arm actually failed, because a hint that rewrites nothing
/// has nothing to rewrite.
type SkippedHint<'a> = Box<dyn Fn(&UpstreamError) -> Option<String> + Send + 'a>;

/// Builds the error raised when every available provider failed.
type OnExhausted<'a> = Box<dyn FnOnce(&[ProviderFailure]) -> UpstreamError + Send + 'a>;

/// One arm of a chain.
///
/// `run` is a future rather than a closure returning one. Rust futures do
/// nothing until polled, so an arm that is never reached costs only the
/// allocation that boxing it took — and boxing is what lets arms of unrelated
/// concrete types sit in one `Vec`.
pub struct Provider<'a, T> {
    id: &'static str,
    label: &'static str,
    available: bool,
    skipped_hint: Option<SkippedHint<'a>>,
    run: BoxFuture<'a, Result<T>>,
}

impl<'a, T> Provider<'a, T> {
    /// `id` is the stable name surfaced to the client as the answering source;
    /// `label` is the human name used in error text and logs.
    pub fn new(
        id: &'static str,
        label: &'static str,
        run: impl Future<Output = Result<T>> + Send + 'a,
    ) -> Self {
        Self {
            id,
            label,
            available: true,
            skipped_hint: None,
            run: run.boxed(),
        }
    }

    /// Whether this deployment can use the provider at all — an unset API key,
    /// say. An unavailable provider is skipped without being counted as a
    /// failure.
    #[must_use]
    pub fn available(mut self, available: bool) -> Self {
        self.available = available;
        self
    }

    /// A hint to attach when this provider was skipped and nothing else
    /// answered. Lets FRED tell the operator that setting a key would have
    /// covered the gap.
    #[must_use]
    pub fn skipped_hint(
        mut self,
        hint: impl Fn(&UpstreamError) -> Option<String> + Send + 'a,
    ) -> Self {
        self.skipped_hint = Some(Box::new(hint));
        self
    }

    #[must_use]
    pub fn id(&self) -> &'static str {
        self.id
    }

    #[must_use]
    pub fn label(&self) -> &'static str {
        self.label
    }
}

/// A value and the provider that produced it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attributed<T> {
    pub value: T,
    /// The answering provider's `id`.
    pub source: &'static str,
}

/// One arm's refusal, kept for the exhausted-chain error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderFailure {
    pub id: &'static str,
    pub label: &'static str,
    pub error: UpstreamError,
}

/// Codes that propagate untouched instead of being folded into the
/// exhausted-chain error — a capability gap the caller should see as itself.
const DEFAULT_PASS_THROUGH: &[&str] = &[codes::UNSUPPORTED];

/// An ordered chain of providers, run until one answers.
///
/// Skips providers this deployment cannot use, stops on a definitive answer,
/// and raises a single described error when nothing worked.
pub struct Chain<'a, T> {
    what: String,
    providers: Vec<Provider<'a, T>>,
    pass_through: Vec<&'static str>,
    on_exhausted: Option<OnExhausted<'a>>,
    log_prefix: Option<&'static str>,
}

impl<'a, T> Chain<'a, T> {
    /// `what` names what was being fetched, for the exhausted-chain error —
    /// e.g. `a quote for AAPL`.
    pub fn new(what: impl Into<String>) -> Self {
        Self {
            what: what.into(),
            providers: Vec::new(),
            pass_through: DEFAULT_PASS_THROUGH.to_vec(),
            on_exhausted: None,
            log_prefix: None,
        }
    }

    /// Append one arm. Order is the order they are tried in.
    #[must_use]
    pub fn provider(mut self, provider: Provider<'a, T>) -> Self {
        self.providers.push(provider);
        self
    }

    /// Append several arms at once, for a helper that builds the whole list.
    #[must_use]
    pub fn providers(mut self, providers: Vec<Provider<'a, T>>) -> Self {
        self.providers.extend(providers);
        self
    }

    /// Replace the codes that propagate verbatim rather than being folded into
    /// the exhausted-chain error. Defaults to `unsupported`.
    #[must_use]
    pub fn pass_through_codes(mut self, codes: impl Into<Vec<&'static str>>) -> Self {
        self.pass_through = codes.into();
        self
    }

    /// Build the error raised when every available provider failed.
    ///
    /// This is how Yahoo→Nasdaq distinguishes "Yahoo is rate-limiting this
    /// datacentre IP" from "that symbol does not exist".
    #[must_use]
    pub fn on_exhausted(
        mut self,
        build: impl FnOnce(&[ProviderFailure]) -> UpstreamError + Send + 'a,
    ) -> Self {
        self.on_exhausted = Some(Box::new(build));
        self
    }

    /// Name this chain in the warning logged when more than one arm failed.
    #[must_use]
    pub fn log_prefix(mut self, prefix: &'static str) -> Self {
        self.log_prefix = Some(prefix);
        self
    }

    /// Run the providers in order and return the first answer.
    pub async fn first_answer(self) -> Result<Attributed<T>> {
        let Chain {
            what,
            providers,
            pass_through,
            on_exhausted,
            log_prefix,
        } = self;

        let labels: Vec<&'static str> = providers.iter().map(Provider::label).collect();
        let mut failures: Vec<ProviderFailure> = Vec::new();
        let mut skipped: Vec<SkippedHint<'a>> = Vec::new();

        for provider in providers {
            let Provider {
                id,
                label,
                available,
                skipped_hint,
                run,
            } = provider;

            if !available {
                if let Some(hint) = skipped_hint {
                    skipped.push(hint);
                }
                continue;
            }

            match run.await {
                Ok(value) => return Ok(Attributed { value, source: id }),
                Err(error) => {
                    // A definitive answer from any provider is the answer.
                    // Asking the next one cannot turn "no such series" into a
                    // series. Which codes those are lives on the error type, so
                    // every chain agrees on it.
                    if error.is_terminal() {
                        return Err(error);
                    }
                    failures.push(ProviderFailure { id, label, error });
                }
            }
        }

        // Nothing answered. If a provider was skipped for want of
        // configuration, and it could have covered this, say so on the way out.
        if let Some(cause) = failures.first().map(|failure| &failure.error) {
            for hint in &skipped {
                if let Some(hint) = hint(cause) {
                    let mut rewritten =
                        UpstreamError::new(cause.message.clone(), cause.code.clone())
                            .with_hint(hint);
                    // The upstream's own status is still a fact about what
                    // happened; only the hint is being replaced.
                    rewritten.status = cause.status;
                    return Err(rewritten);
                }
            }
        }

        if failures.is_empty() {
            return Err(UpstreamError::not_configured(format!(
                "No source is configured to serve {what}"
            ))
            .with_hint(format!(
                "{} are all unavailable in this deployment.",
                labels.join(", ")
            )));
        }

        if failures.len() > 1 {
            if let Some(prefix) = log_prefix {
                let detail = failures
                    .iter()
                    .map(|failure| format!("{}: {}", failure.id, failure.error))
                    .collect::<Vec<_>>()
                    .join("; ");
                tracing::warn!(
                    chain = prefix,
                    "every provider failed for {what} — {detail}"
                );
            }
        }

        // A capability gap is worth surfacing as itself rather than as a
        // generic "nothing could answer" — it is a fact about the provider, not
        // an outage.
        for failure in &failures {
            if pass_through.contains(&failure.error.code.as_str()) {
                return Err(failure.error.clone());
            }
        }

        if let Some(build) = on_exhausted {
            return Err(build(&failures));
        }

        // Default: the first failure is the most informative, because the first
        // provider is the source of record and its error says why it declined.
        Err(failures.remove(0).error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    async fn ok(value: &str) -> Result<String> {
        Ok(value.to_string())
    }

    async fn fail(error: UpstreamError) -> Result<String> {
        Err(error)
    }

    #[tokio::test]
    async fn the_first_arm_that_answers_wins_and_says_so() {
        let answer = Chain::new("a quote for AAPL")
            .provider(Provider::new("yahoo", "Yahoo Finance", ok("yahoo said so")))
            .provider(Provider::new("nasdaq", "Nasdaq", ok("nasdaq said so")))
            .first_answer()
            .await
            .unwrap();

        assert_eq!(answer.value, "yahoo said so");
        assert_eq!(answer.source, "yahoo");
    }

    #[tokio::test]
    async fn falls_through_to_the_next_arm_after_a_failure() {
        let answer = Chain::new("a quote for AAPL")
            .provider(Provider::new(
                "yahoo",
                "Yahoo Finance",
                fail(UpstreamError::blocked("429 from a datacentre IP").with_status(429)),
            ))
            .provider(Provider::new("nasdaq", "Nasdaq", ok("nasdaq said so")))
            .first_answer()
            .await
            .unwrap();

        assert_eq!(answer.value, "nasdaq said so");
        assert_eq!(answer.source, "nasdaq");
    }

    #[tokio::test]
    async fn an_unavailable_arm_is_skipped_and_never_run() {
        let ran = AtomicBool::new(false);
        let answer = Chain::new("series GDP")
            .provider(
                Provider::new("api", "the FRED API", async {
                    ran.store(true, Ordering::SeqCst);
                    Ok("api".to_string())
                })
                .available(false),
            )
            .provider(Provider::new("scrape", "fred.stlouisfed.org", ok("scrape")))
            .first_answer()
            .await
            .unwrap();

        assert_eq!(answer.source, "scrape");
        assert!(!ran.load(Ordering::SeqCst), "a skipped arm was polled");
    }

    #[tokio::test]
    async fn a_skipped_arm_is_not_a_failure() {
        // The reported error is the one available arm's, not a chain-exhausted
        // wrapper counting the skip.
        let error = Chain::new("series GDP")
            .provider(Provider::new("api", "the FRED API", ok("api")).available(false))
            .provider(Provider::new(
                "scrape",
                "fred.stlouisfed.org",
                fail(UpstreamError::blocked("connection reset")),
            ))
            .first_answer()
            .await
            .unwrap_err();

        assert_eq!(error.code, "upstream_blocked");
        assert_eq!(error.message, "connection reset");
        assert_eq!(error.hint, None);
    }

    #[tokio::test]
    async fn a_terminal_code_ends_the_chain() {
        let ran = AtomicBool::new(false);
        let error = Chain::new("series NOPE")
            .provider(Provider::new(
                "scrape",
                "fred.stlouisfed.org",
                fail(UpstreamError::not_found("No series NOPE")),
            ))
            .provider(Provider::new("api", "the FRED API", async {
                ran.store(true, Ordering::SeqCst);
                Ok("api".to_string())
            }))
            .first_answer()
            .await
            .unwrap_err();

        assert_eq!(error.code, "not_found");
        assert_eq!(error.message, "No series NOPE");
        assert!(
            !ran.load(Ordering::SeqCst),
            "the chain kept going past a definitive answer"
        );
    }

    #[tokio::test]
    async fn every_arm_skipped_is_not_configured() {
        let error = Chain::new("a quote for AAPL")
            .providers(vec![
                Provider::new("alpaca", "Alpaca", ok("a")).available(false),
                Provider::new("yahoo", "Yahoo Finance", ok("y")).available(false),
            ])
            .first_answer()
            .await
            .unwrap_err();

        assert_eq!(error.code, "not_configured");
        assert_eq!(
            error.message,
            "No source is configured to serve a quote for AAPL"
        );
        assert_eq!(
            error.hint.as_deref(),
            Some("Alpaca, Yahoo Finance are all unavailable in this deployment.")
        );
    }

    #[tokio::test]
    async fn a_skipped_arms_hint_rewrites_the_failure() {
        let error = Chain::new("series GDP")
            .provider(Provider::new(
                "scrape",
                "fred.stlouisfed.org",
                fail(
                    UpstreamError::blocked("fred.stlouisfed.org refused the connection")
                        .with_status(503),
                ),
            ))
            .provider(
                Provider::new("api", "the FRED API", ok("api"))
                    .available(false)
                    .skipped_hint(|cause| {
                        (cause.code == codes::UPSTREAM_BLOCKED).then(|| {
                            "Set FRED_API_KEY to use the official API instead.".to_string()
                        })
                    }),
            )
            .first_answer()
            .await
            .unwrap_err();

        // Same failure, now carrying the operator's way out of it.
        assert_eq!(error.code, "upstream_blocked");
        assert_eq!(error.message, "fred.stlouisfed.org refused the connection");
        assert_eq!(
            error.hint.as_deref(),
            Some("Set FRED_API_KEY to use the official API instead.")
        );
        assert_eq!(error.status, Some(503));
    }

    #[tokio::test]
    async fn a_hint_that_declines_leaves_the_failure_alone() {
        let error = Chain::new("series GDP")
            .provider(Provider::new(
                "scrape",
                "fred.stlouisfed.org",
                fail(UpstreamError::timeout("no reply in 8s")),
            ))
            .provider(
                Provider::new("api", "the FRED API", ok("api"))
                    .available(false)
                    // Only a block is worth blaming on the missing key.
                    .skipped_hint(|cause| {
                        (cause.code == codes::UPSTREAM_BLOCKED).then(|| "set a key".to_string())
                    }),
            )
            .first_answer()
            .await
            .unwrap_err();

        assert_eq!(error.code, "upstream_timeout");
        assert_eq!(error.hint, None);
    }

    #[tokio::test]
    async fn a_pass_through_code_propagates_verbatim() {
        let error = Chain::new("candles for a Polymarket US market")
            .pass_through_codes(vec![codes::RATE_LIMITED])
            .provider(Provider::new(
                "primary",
                "Primary",
                fail(UpstreamError::blocked("blocked")),
            ))
            .provider(Provider::new(
                "secondary",
                "Secondary",
                fail(
                    UpstreamError::new("slow down", codes::RATE_LIMITED)
                        .with_hint("try again in a minute"),
                ),
            ))
            .on_exhausted(|_| UpstreamError::new("nothing answered", codes::UPSTREAM_ERROR))
            .first_answer()
            .await
            .unwrap_err();

        // The capability/limit answer beats both the first failure and the
        // exhausted-chain override.
        assert_eq!(error.code, codes::RATE_LIMITED);
        assert_eq!(error.hint.as_deref(), Some("try again in a minute"));
    }

    #[tokio::test]
    async fn on_exhausted_replaces_the_first_failure() {
        let error = Chain::new("a price for ^GSPC")
            .log_prefix("stocks")
            .provider(Provider::new(
                "yahoo",
                "Yahoo Finance",
                fail(UpstreamError::blocked("rate limited").with_status(429)),
            ))
            .provider(Provider::new(
                "nasdaq",
                "Nasdaq",
                fail(UpstreamError::not_configured(
                    "no coverage for cash indices",
                )),
            ))
            .on_exhausted(|failures| {
                let yahoo = failures.iter().find(|f| f.id == "yahoo");
                let hint = if yahoo.and_then(|f| f.error.status) == Some(429) {
                    "Yahoo Finance is rate-limiting this IP, and Nasdaq does not cover ^GSPC."
                } else {
                    "Check the symbol: cash indices need a caret, e.g. `^GSPC`."
                };
                UpstreamError::new("No price source could quote ^GSPC", codes::UPSTREAM_ERROR)
                    .with_hint(hint)
            })
            .first_answer()
            .await
            .unwrap_err();

        assert_eq!(error.code, codes::UPSTREAM_ERROR);
        assert_eq!(error.message, "No price source could quote ^GSPC");
        assert!(error
            .hint
            .unwrap()
            .starts_with("Yahoo Finance is rate-limiting"));
    }

    #[tokio::test]
    async fn the_first_failure_is_what_surfaces_by_default() {
        let error = Chain::new("series GDP")
            .provider(Provider::new(
                "scrape",
                "fred.stlouisfed.org",
                fail(UpstreamError::blocked("connection reset").with_hint("scrape hint")),
            ))
            .provider(Provider::new(
                "api",
                "the FRED API",
                fail(UpstreamError::new("bad key", codes::BAD_CREDENTIALS)),
            ))
            .first_answer()
            .await
            .unwrap_err();

        assert_eq!(error.code, "upstream_blocked");
        assert_eq!(error.hint.as_deref(), Some("scrape hint"));
    }

    #[tokio::test]
    async fn arms_are_tried_in_order() {
        let order = std::sync::Mutex::new(Vec::new());
        let answer = Chain::new("something")
            .provider(Provider::new("first", "First", {
                let order = &order;
                async move {
                    order.lock().unwrap().push("first");
                    Err(UpstreamError::timeout("nope"))
                }
            }))
            .provider(Provider::new("second", "Second", {
                let order = &order;
                async move {
                    order.lock().unwrap().push("second");
                    Ok("answer".to_string())
                }
            }))
            .first_answer()
            .await
            .unwrap();

        assert_eq!(answer.source, "second");
        assert_eq!(*order.lock().unwrap(), ["first", "second"]);
    }
}
