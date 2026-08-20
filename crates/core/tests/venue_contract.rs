//! The venue grammar against its parity contract.
//!
//! `venue.rs` is one of two implementations of the same rules — the other is
//! `client/src/lib/terminal/venue.ts` — so the cases live in data rather than in
//! either language's test file. `contract/venue-cases.json` describes its own
//! schema; this suite is the Rust half reading it, and
//! `client/src/lib/terminal/venue.test.ts` is the TypeScript half reading the
//! same file. A rule that drifts on one side fails on the other instead of
//! surfacing as a 404 the day someone types `pm:` at the prompt.
//!
//! `include_str!` rather than a runtime read on purpose: the path is checked at
//! compile time, so a moved or renamed fixture is a build error rather than a
//! test that quietly asserts nothing, and touching the fixture rebuilds this.

use std::str::FromStr;

use serde::Deserialize;
use terminal_core::venue::{
    format_ref, normalise_id, parse_ref, parse_venue, AliasContext, RefError, Venue, VenueRef,
};

const CONTRACT: &str = include_str!("../../../contract/venue-cases.json");

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
    #[serde(rename = "parseVenue")]
    parse_venue: Vec<ParseVenueCase>,
}

#[derive(Deserialize)]
struct ParseRefCase {
    input: String,
    fallback: String,
    venue: String,
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
    venue: String,
    id: String,
    text: String,
}

#[derive(Deserialize)]
struct NormaliseIdCase {
    venue: String,
    input: String,
    id: String,
}

#[derive(Deserialize)]
struct ParseVenueCase {
    token: String,
    context: String,
    /// `null` means the token names no venue in that context.
    venue: Option<String>,
}

fn contract() -> Contract {
    serde_json::from_str(CONTRACT).expect("contract/venue-cases.json parses")
}

/// The contract names venues by their wire identifier; an unknown one is a
/// broken fixture rather than a failed assertion, so it panics loudly.
fn venue(name: &str) -> Venue {
    Venue::from_str(name).unwrap_or_else(|_| panic!("contract names an unknown venue {name:?}"))
}

fn context(name: &str) -> AliasContext {
    match name {
        "prefix" => AliasContext::Prefix,
        "word" => AliasContext::Word,
        other => panic!("contract names an unknown alias context {other:?}"),
    }
}

/// Which failure a `RefError` reports, in the contract's vocabulary.
///
/// Rust models the two failures as enum variants and TypeScript throws a plain
/// `Error` whose wording it reads back; the contract names the kind so neither
/// side has to know the other's mechanism.
fn error_kind(err: &RefError) -> &'static str {
    match err {
        RefError::UnknownPrefix { .. } => "unknown-prefix",
        RefError::MissingId { .. } => "empty-identifier",
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
    assert!(!cases.parse_venue.is_empty());
}

#[test]
fn parses_every_reference() {
    for c in contract().parse_ref {
        let parsed = parse_ref(&c.input, venue(&c.fallback))
            .unwrap_or_else(|e| panic!("{:?} against {} should parse: {e}", c.input, c.fallback));
        assert_eq!(
            parsed,
            VenueRef {
                venue: venue(&c.venue),
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
        let err = parse_ref(&c.input, venue(&c.fallback))
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
        let reference = VenueRef {
            venue: venue(&c.venue),
            id: c.id.clone(),
        };
        assert_eq!(format_ref(&reference), c.text, "{}:{}", c.venue, c.id);
    }
}

#[test]
fn every_printed_reference_reads_back_as_itself() {
    // A panel id built from a printed reference has to match the market it was
    // cut from, which only holds if the round trip does.
    for c in contract().format_ref {
        let reference = VenueRef {
            venue: venue(&c.venue),
            id: c.id.clone(),
        };
        let printed = format_ref(&reference);
        assert_eq!(
            terminal_core::venue::parse_ref_default(&printed).expect("prints parse"),
            reference,
            "{printed}"
        );
    }
}

#[test]
fn folds_every_identifier() {
    for c in contract().normalise_id {
        assert_eq!(
            normalise_id(venue(&c.venue), &c.input),
            c.id,
            "{:?} at {}",
            c.input,
            c.venue
        );
    }
}

#[test]
fn reads_every_venue_token() {
    for c in contract().parse_venue {
        let expected = c.venue.as_deref().map(venue);
        assert_eq!(
            parse_venue(&c.token, context(&c.context)),
            expected,
            "{:?} in {} context",
            c.token,
            c.context
        );
    }
}
