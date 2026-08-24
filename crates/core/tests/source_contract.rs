//! The data-source grammar against its parity contract.
//!
//! `dataset.rs` is one of two implementations of the same rules — the other is
//! `client/src/lib/terminal/dataset.ts` — so the cases live in data rather than
//! in either language's test file. `contract/source-cases.json` describes its
//! own schema; this suite is the Rust half reading it, and
//! `client/src/lib/terminal/dataset.test.ts` is the TypeScript half reading the
//! same file. A rule that drifts on one side fails on the other instead of
//! surfacing as a 404 the day someone types `ecb:` at the prompt.
//!
//! The same shape, and the same reasoning, as `venue_contract.rs`. The two are
//! deliberately separate files: the case lists are unrelated data and a shared
//! harness would only couple them.

use std::str::FromStr;

use serde::Deserialize;
use terminal_core::dataset::{
    format_data_ref, normalise_data_id, parse_data_ref, parse_data_source, DataRef, DataRefError,
    DataSource,
};
use terminal_core::venue::AliasContext;

const CONTRACT: &str = include_str!("../../../contract/source-cases.json");

#[derive(Deserialize)]
struct Contract {
    #[serde(rename = "parseRef")]
    parse_ref: Vec<ParseRefCase>,
    #[serde(rename = "parseRefErrors")]
    parse_ref_errors: Vec<ParseRefErrorCase>,
    #[serde(rename = "formatRef")]
    format_ref: Vec<FormatRefCase>,
    #[serde(rename = "normaliseId")]
    normalise_id: Vec<NormaliseIdCase>,
    #[serde(rename = "parseSource")]
    parse_source: Vec<ParseSourceCase>,
}

#[derive(Deserialize)]
struct ParseRefCase {
    input: String,
    fallback: String,
    source: String,
    id: String,
}

#[derive(Deserialize)]
struct ParseRefErrorCase {
    input: String,
    fallback: String,
    error: String,
    #[serde(rename = "messageContains")]
    message_contains: Vec<String>,
}

#[derive(Deserialize)]
struct FormatRefCase {
    source: String,
    id: String,
    text: String,
}

#[derive(Deserialize)]
struct NormaliseIdCase {
    source: String,
    input: String,
    id: String,
}

#[derive(Deserialize)]
struct ParseSourceCase {
    token: String,
    context: String,
    /// `null` means the token names no source in that context.
    source: Option<String>,
}

fn contract() -> Contract {
    serde_json::from_str(CONTRACT).expect("contract/source-cases.json parses")
}

/// The contract names sources by their wire identifier; an unknown one is a
/// broken fixture rather than a failed assertion, so it panics loudly.
fn source(name: &str) -> DataSource {
    DataSource::from_str(name)
        .unwrap_or_else(|_| panic!("contract names an unknown source {name:?}"))
}

fn context(name: &str) -> AliasContext {
    match name {
        "prefix" => AliasContext::Prefix,
        "word" => AliasContext::Word,
        other => panic!("contract names an unknown alias context {other:?}"),
    }
}

/// Which failure a `DataRefError` reports, in the contract's vocabulary.
fn error_kind(err: &DataRefError) -> &'static str {
    match err {
        DataRefError::UnknownPrefix { .. } => "unknown-prefix",
        DataRefError::MissingId { .. } => "empty-identifier",
        DataRefError::MissingReference => "missing-reference",
    }
}

#[test]
fn covers_every_case_list() {
    // A truncated or renamed file must fail here rather than silently passing
    // zero assertions below.
    let cases = contract();
    assert!(!cases.parse_ref.is_empty());
    assert!(!cases.parse_ref_errors.is_empty());
    assert!(!cases.format_ref.is_empty());
    assert!(!cases.normalise_id.is_empty());
    assert!(!cases.parse_source.is_empty());
}

#[test]
fn parses_every_reference() {
    for c in contract().parse_ref {
        let parsed = parse_data_ref(&c.input, source(&c.fallback))
            .unwrap_or_else(|e| panic!("{:?} against {} should parse: {e}", c.input, c.fallback));
        assert_eq!(
            parsed,
            DataRef {
                source: source(&c.source),
                id: c.id.clone(),
            },
            "{:?} against {}",
            c.input,
            c.fallback
        );
    }
}

#[test]
fn rejects_every_bad_reference() {
    for c in contract().parse_ref_errors {
        let err = parse_data_ref(&c.input, source(&c.fallback))
            .expect_err(&format!("{:?} should not parse", c.input));
        assert_eq!(error_kind(&err), c.error, "{:?}", c.input);

        let message = err.to_string();
        for fragment in &c.message_contains {
            assert!(
                message.contains(fragment.as_str()),
                "{:?} reported {message:?}, which is missing {fragment:?}",
                c.input
            );
        }
    }
}

#[test]
fn formats_every_reference() {
    for c in contract().format_ref {
        let reference = DataRef {
            source: source(&c.source),
            id: c.id.clone(),
        };
        assert_eq!(format_data_ref(&reference), c.text, "{}:{}", c.source, c.id);
    }
}

#[test]
fn every_printed_reference_reads_back_as_itself() {
    // A panel id built from a printed reference has to match the series it was
    // cut from, which only holds if the round trip does.
    for c in contract().format_ref {
        let reference = DataRef {
            source: source(&c.source),
            id: c.id.clone(),
        };
        let printed = format_data_ref(&reference);
        assert_eq!(
            terminal_core::dataset::parse_data_ref_default(&printed).expect("prints parse"),
            reference,
            "{printed}"
        );
    }
}

#[test]
fn folds_every_identifier() {
    for c in contract().normalise_id {
        assert_eq!(
            normalise_data_id(source(&c.source), &c.input),
            c.id,
            "{:?} at {}",
            c.input,
            c.source
        );
    }
}

#[test]
fn reads_every_source_token() {
    for c in contract().parse_source {
        let expected = c.source.as_deref().map(source);
        assert_eq!(
            parse_data_source(&c.token, context(&c.context)),
            expected,
            "{:?} in {} context",
            c.token,
            c.context
        );
    }
}
