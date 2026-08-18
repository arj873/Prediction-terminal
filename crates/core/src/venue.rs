//! The venues the terminal quotes: who they are, what they can answer, and how
//! a contract at one of them is named.
//!
//! Three brokers list the same questions under three naming schemes. Kalshi has
//! upper-case tickers (`KXFEDDECISION-26SEP-T3.75`), both Polymarkets have
//! lower-case slugs (`will-the-fed-decrease-interest-rates-…`,
//! `tec-mlb-nlchamp-2026-09-27-lad`). Rather than three parallel command sets,
//! every command takes one *reference* — an optional venue prefix and an
//! identifier — and this module is the only place that knows how to read one.
//!
//! ```text
//! KXFEDDECISION-26SEP-T3.75      → kalshi (the default, so nothing changes)
//! pm:fed-decision-in-september   → Polymarket International
//! pmus:fed-decision-2026-09-16   → Polymarket US
//! ```
//!
//! Case is part of the identifier, not decoration: Kalshi 404s a lower-case
//! ticker and Polymarket 404s an upper-case slug, so folding happens here, once,
//! per venue — never at the call site.
//!
//! One table drives all of it. Everything a venue differs by — its aliases,
//! whether it prints bare, whether it is the default for an unprefixed
//! reference, and which endpoints it actually serves — is a field here rather
//! than a `== Venue::Kalshi` somewhere downstream.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// How `TOP` ranks a book. Defined here because every venue declares which it
/// can serve.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../../../client/src/lib/api/gen/")]
pub enum MoverSort {
    Volume,
    Gainers,
    Losers,
    OpenInterest,
    Liquidity,
}

impl MoverSort {
    pub const ALL: &'static [MoverSort] = &[
        MoverSort::Volume,
        MoverSort::Gainers,
        MoverSort::Losers,
        MoverSort::OpenInterest,
        MoverSort::Liquidity,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            MoverSort::Volume => "volume",
            MoverSort::Gainers => "gainers",
            MoverSort::Losers => "losers",
            MoverSort::OpenInterest => "open_interest",
            MoverSort::Liquidity => "liquidity",
        }
    }
}

impl FromStr for MoverSort {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "volume" => Ok(MoverSort::Volume),
            "gainers" => Ok(MoverSort::Gainers),
            "losers" => Ok(MoverSort::Losers),
            "open_interest" => Ok(MoverSort::OpenInterest),
            "liquidity" => Ok(MoverSort::Liquidity),
            _ => Err(()),
        }
    }
}

impl fmt::Display for MoverSort {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Identifier case. Kalshi shouts; the Polymarkets do not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdCase {
    Upper,
    Lower,
}

/// What a venue's public API actually offers.
///
/// Declared rather than discovered. The alternative — call the endpoint and see
/// whether it errors — means the UI cannot know until after it has offered the
/// reader a button, and it cannot tell "this venue publishes no volume" apart
/// from "this venue is down". Both of those were live bugs.
#[derive(Debug, Clone, Copy)]
pub struct VenueCapabilities {
    /// Publishes price history the terminal can chart (`GP`).
    pub candles: bool,
    /// Publishes a public print tape (`TAS`).
    pub trades: bool,
    /// `list_series` honours a category filter.
    pub series_category_filter: bool,
    /// The rankings this venue populates. `TOP` will not ask it for the others.
    pub sorts: &'static [MoverSort],
    /// Why a missing capability is missing, shown to the reader in place of a
    /// dead button. Empty when the venue serves everything.
    pub note: &'static str,
}

/// A venue's row in the registry.
#[derive(Debug, Clone, Copy)]
pub struct VenueInfo {
    pub id: Venue,
    /// Full name, for panel headers and error messages.
    pub label: &'static str,
    /// Short badge for a table column.
    pub code: &'static str,
    /// Canonical command-line prefix, including the colon.
    pub prefix: &'static str,
    /// What this venue calls a contract identifier, for usage text.
    pub id_label: &'static str,
    /// Identifier case.
    pub case: IdCase,
    /// Public web page for a market, so a panel can link out.
    pub site: &'static str,
    /// Names a trader might type. Deliberately generous — the cost of accepting
    /// a synonym is nil and the cost of rejecting one is a command retyped.
    pub aliases: &'static [&'static str],
    /// An unprefixed reference belongs to this venue. Exactly one venue sets it.
    pub is_default: bool,
    /// This venue's references print without their prefix. Follows from being
    /// the default.
    pub bare_ref: bool,
    pub capabilities: VenueCapabilities,
}

/// The brokers the terminal quotes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../client/src/lib/api/gen/")]
pub enum Venue {
    #[serde(rename = "kalshi")]
    #[ts(rename = "kalshi")]
    Kalshi,
    #[serde(rename = "polymarket")]
    #[ts(rename = "polymarket")]
    Polymarket,
    #[serde(rename = "polymarket-us")]
    #[ts(rename = "polymarket-us")]
    PolymarketUs,
}

impl Venue {
    /// The venue's wire identifier — what appears in a URL and in JSON.
    pub fn as_str(self) -> &'static str {
        match self {
            Venue::Kalshi => "kalshi",
            Venue::Polymarket => "polymarket",
            Venue::PolymarketUs => "polymarket-us",
        }
    }

    pub fn info(self) -> &'static VenueInfo {
        VENUES
            .iter()
            .find(|v| v.id == self)
            .expect("every Venue variant has a registry row")
    }
}

impl fmt::Display for Venue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Venue {
    type Err = ();

    /// Reads the *wire* identifier only. Aliases go through [`parse_venue`].
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "kalshi" => Ok(Venue::Kalshi),
            "polymarket" => Ok(Venue::Polymarket),
            "polymarket-us" => Ok(Venue::PolymarketUs),
            _ => Err(()),
        }
    }
}

const ALL_SORTS: &[MoverSort] = MoverSort::ALL;

/// The registry. One row per broker; every downstream difference reads from here.
pub static VENUES: &[VenueInfo] = &[
    VenueInfo {
        id: Venue::Kalshi,
        label: "Kalshi",
        code: "KAL",
        prefix: "kx:",
        id_label: "ticker",
        case: IdCase::Upper,
        site: "https://kalshi.com",
        aliases: &["kalshi", "kal", "kx", "k"],
        // Every command, example and habit in this terminal predates the other
        // two venues; an unprefixed reference has to keep meaning what it always
        // did.
        is_default: true,
        bare_ref: true,
        capabilities: VenueCapabilities {
            candles: true,
            trades: true,
            series_category_filter: true,
            sorts: ALL_SORTS,
            note: "",
        },
    },
    VenueInfo {
        id: Venue::Polymarket,
        label: "Polymarket International",
        code: "PM",
        prefix: "pm:",
        id_label: "slug",
        case: IdCase::Lower,
        site: "https://polymarket.com",
        aliases: &[
            "polymarket",
            "poly",
            "pm",
            "intl",
            "international",
            "polymarket-intl",
        ],
        is_default: false,
        bare_ref: false,
        capabilities: VenueCapabilities {
            candles: true,
            trades: true,
            series_category_filter: false,
            // The catalogue carries no open interest, so an OI board would rank
            // it last on a figure it never published rather than on a small one.
            sorts: &[
                MoverSort::Volume,
                MoverSort::Gainers,
                MoverSort::Losers,
                MoverSort::Liquidity,
            ],
            note: "Polymarket publishes price samples rather than OHLC bars, and no open interest.",
        },
    },
    VenueInfo {
        id: Venue::PolymarketUs,
        label: "Polymarket US",
        code: "PMUS",
        prefix: "pmus:",
        id_label: "slug",
        case: IdCase::Lower,
        site: "https://polymarket.us",
        aliases: &[
            "polymarketus",
            "polymarket-us",
            "polyus",
            "pmus",
            "pm-us",
            "us",
        ],
        is_default: false,
        bare_ref: false,
        capabilities: VenueCapabilities {
            candles: false,
            trades: false,
            series_category_filter: false,
            // Its public catalogue carries no volume, change or open interest at
            // all, so it can be ranked on nothing.
            sorts: &[],
            note: "Polymarket US serves price history, prints and ranking figures only to an \
                   authenticated caller. The terminal reads public endpoints only, so quote \
                   and book (DES, OB) work and the rest say so.",
        },
    },
];

/// Every venue identifier, in registry order.
pub fn venue_ids() -> Vec<Venue> {
    VENUES.iter().map(|v| v.id).collect()
}

pub fn venue_info(venue: Venue) -> &'static VenueInfo {
    venue.info()
}

/// Whether `value` is a venue's wire identifier.
pub fn is_venue(value: &str) -> bool {
    Venue::from_str(value).is_ok()
}

/// The venue an unprefixed reference belongs to.
pub const DEFAULT_VENUE: Venue = Venue::Kalshi;

/// Where a venue token was read from.
///
/// A `Prefix` reading accepts every alias; a `Word` reading — one token among
/// the words of a search — declines the ones that are also ordinary English.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AliasContext {
    Prefix,
    Word,
}

/// Aliases that are ordinary English words.
///
/// These are fine as a prefix — `us:` is unambiguous — but must not be claimed
/// out of free text, or `SRCH us election` quietly becomes "search Polymarket US
/// for *election*" and `TV The Office US` searches the wrong book. A venue
/// filter is a convenience; silently changing which exchange was searched is not.
const PREFIX_ONLY_ALIASES: &[&str] = &["us", "k", "intl", "international"];

/// Read a venue name or alias. `None` when the token names no venue.
pub fn parse_venue(token: &str, context: AliasContext) -> Option<Venue> {
    let key = token.trim().to_lowercase();
    if context == AliasContext::Word && PREFIX_ONLY_ALIASES.contains(&key.as_str()) {
        return None;
    }
    VENUES
        .iter()
        .find(|v| v.aliases.contains(&key.as_str()))
        .map(|v| v.id)
}

/// A contract, event or series at a named venue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VenueRef {
    pub venue: Venue,
    pub id: String,
}

/// Why a reference could not be read. Both variants are shown to the reader.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefError {
    /// A known venue prefix with nothing after it.
    MissingId { venue: Venue, raw: String },
    /// `foo:bar` where `foo` names no venue.
    UnknownPrefix { prefix: String },
}

impl fmt::Display for RefError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RefError::MissingId { venue, raw } => write!(
                f,
                "\"{}\" names a venue but no {}",
                raw,
                venue_info(*venue).id_label
            ),
            RefError::UnknownPrefix { prefix } => {
                let prefixes: Vec<&str> = VENUES.iter().map(|v| v.prefix).collect();
                write!(
                    f,
                    "Unknown venue prefix \"{}\". Try {}",
                    prefix,
                    prefixes.join(", ")
                )
            }
        }
    }
}

impl std::error::Error for RefError {}

/// Normalise an identifier to the case its venue actually answers to.
pub fn normalise_id(venue: Venue, id: &str) -> String {
    match venue_info(venue).case {
        IdCase::Upper => id.to_uppercase(),
        IdCase::Lower => id.to_lowercase(),
    }
}

/// Read `[venue:]identifier`.
///
/// Only a *known* prefix is treated as one. Polymarket slugs are full of
/// colons' cousins but not colons, and Kalshi tickers have none, so a bare
/// `foo:bar` with an unrecognised `foo` is far more likely to be a typo than an
/// identifier — it is reported rather than silently mangled.
pub fn parse_ref(raw: &str, fallback: Venue) -> Result<VenueRef, RefError> {
    let text = raw.trim();

    if let Some(colon) = text.find(':') {
        if colon > 0 {
            let prefix = &text[..colon];
            return match parse_venue(prefix, AliasContext::Prefix) {
                Some(venue) => {
                    let id = text[colon + 1..].trim();
                    if id.is_empty() {
                        return Err(RefError::MissingId {
                            venue,
                            raw: raw.to_string(),
                        });
                    }
                    Ok(VenueRef {
                        venue,
                        id: normalise_id(venue, id),
                    })
                }
                None => Err(RefError::UnknownPrefix {
                    prefix: prefix.to_string(),
                }),
            };
        }
    }

    Ok(VenueRef {
        venue: fallback,
        id: normalise_id(fallback, text),
    })
}

/// [`parse_ref`] against the default venue.
pub fn parse_ref_default(raw: &str) -> Result<VenueRef, RefError> {
    parse_ref(raw, DEFAULT_VENUE)
}

/// The canonical string form of a reference.
///
/// The default venue prints bare, because that is what every existing command,
/// example and README line already says.
pub fn format_ref(reference: &VenueRef) -> String {
    let info = venue_info(reference.venue);
    if info.bare_ref {
        reference.id.clone()
    } else {
        format!("{}{}", info.prefix, reference.id)
    }
}

/// A capability a UI might want to offer a button for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Capability {
    Candles,
    Trades,
}

/// Whether a venue serves a given capability, for a UI deciding what to offer.
pub fn supports(venue: Venue, capability: Capability) -> bool {
    let caps = venue_info(venue).capabilities;
    match capability {
        Capability::Candles => caps.candles,
        Capability::Trades => caps.trades,
    }
}

/// Whether `TOP` can rank this venue by `sort`.
pub fn supports_sort(venue: Venue, sort: MoverSort) -> bool {
    venue_info(venue).capabilities.sorts.contains(&sort)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kalshi_is_the_default_and_prints_bare() {
        assert_eq!(DEFAULT_VENUE, Venue::Kalshi);
        let parsed = parse_ref_default("KXFEDDECISION-26SEP-T3.75").unwrap();
        assert_eq!(parsed.venue, Venue::Kalshi);
        assert_eq!(format_ref(&parsed), "KXFEDDECISION-26SEP-T3.75");
    }

    #[test]
    fn folds_an_identifier_to_the_case_its_venue_answers_to() {
        // Kalshi 404s a lower-case ticker; Polymarket 404s an upper-case slug.
        assert_eq!(
            parse_ref_default("kxfeddecision-26sep").unwrap().id,
            "KXFEDDECISION-26SEP"
        );
        assert_eq!(
            parse_ref("pm:Fed-Decision", Venue::Kalshi).unwrap().id,
            "fed-decision"
        );
        assert_eq!(
            parse_ref("pmus:US-CPI", Venue::Kalshi).unwrap().id,
            "us-cpi"
        );
    }

    #[test]
    fn reads_every_venue_prefix() {
        for (token, expected) in [
            ("kx:ABC", Venue::Kalshi),
            ("kalshi:ABC", Venue::Kalshi),
            ("k:ABC", Venue::Kalshi),
            ("pm:abc", Venue::Polymarket),
            ("poly:abc", Venue::Polymarket),
            ("intl:abc", Venue::Polymarket),
            ("pmus:abc", Venue::PolymarketUs),
            ("polyus:abc", Venue::PolymarketUs),
            ("us:abc", Venue::PolymarketUs),
        ] {
            assert_eq!(parse_ref_default(token).unwrap().venue, expected, "{token}");
        }
    }

    #[test]
    fn a_non_default_venue_prints_with_its_prefix() {
        let pm = parse_ref_default("pm:fed-decision").unwrap();
        assert_eq!(format_ref(&pm), "pm:fed-decision");
        let pmus = parse_ref_default("pmus:fed-decision").unwrap();
        assert_eq!(format_ref(&pmus), "pmus:fed-decision");
    }

    #[test]
    fn rejects_an_unknown_prefix_rather_than_mangling_it() {
        let err = parse_ref_default("foo:bar").unwrap_err();
        assert!(matches!(err, RefError::UnknownPrefix { .. }));
        assert!(err.to_string().contains("kx:"));
    }

    #[test]
    fn rejects_a_venue_prefix_with_no_identifier() {
        let err = parse_ref_default("pm:").unwrap_err();
        assert!(matches!(err, RefError::MissingId { .. }));
        assert!(err.to_string().contains("slug"));
    }

    #[test]
    fn declines_english_word_aliases_read_out_of_free_text() {
        // `SRCH us election` must not quietly become a Polymarket US search.
        assert_eq!(parse_venue("us", AliasContext::Word), None);
        assert_eq!(parse_venue("k", AliasContext::Word), None);
        assert_eq!(parse_venue("intl", AliasContext::Word), None);
        // The same tokens are unambiguous as a prefix.
        assert_eq!(
            parse_venue("us", AliasContext::Prefix),
            Some(Venue::PolymarketUs)
        );
        assert_eq!(parse_venue("k", AliasContext::Prefix), Some(Venue::Kalshi));
    }

    #[test]
    fn still_claims_unambiguous_aliases_out_of_free_text() {
        assert_eq!(
            parse_venue("polymarket", AliasContext::Word),
            Some(Venue::Polymarket)
        );
        assert_eq!(
            parse_venue("kalshi", AliasContext::Word),
            Some(Venue::Kalshi)
        );
    }

    #[test]
    fn capabilities_are_declared_not_discovered() {
        assert!(supports(Venue::Kalshi, Capability::Candles));
        assert!(supports(Venue::Polymarket, Capability::Trades));
        // Polymarket US serves history only to an authenticated caller.
        assert!(!supports(Venue::PolymarketUs, Capability::Candles));
        assert!(!supports(Venue::PolymarketUs, Capability::Trades));
        assert!(!venue_info(Venue::PolymarketUs).capabilities.note.is_empty());
    }

    #[test]
    fn top_only_ranks_a_venue_on_a_figure_it_publishes() {
        assert!(supports_sort(Venue::Kalshi, MoverSort::OpenInterest));
        // Polymarket's catalogue carries no open interest.
        assert!(!supports_sort(Venue::Polymarket, MoverSort::OpenInterest));
        assert!(supports_sort(Venue::Polymarket, MoverSort::Volume));
        // Polymarket US can be ranked on nothing at all.
        for sort in MoverSort::ALL {
            assert!(!supports_sort(Venue::PolymarketUs, *sort), "{sort}");
        }
    }

    #[test]
    fn every_variant_has_exactly_one_registry_row() {
        assert_eq!(VENUES.len(), 3);
        for venue in venue_ids() {
            assert_eq!(venue_info(venue).id, venue);
        }
        assert_eq!(VENUES.iter().filter(|v| v.is_default).count(), 1);
    }

    #[test]
    fn wire_identifiers_round_trip() {
        for venue in venue_ids() {
            assert_eq!(Venue::from_str(venue.as_str()), Ok(venue));
            assert!(is_venue(venue.as_str()));
        }
        assert!(!is_venue("nyse"));
    }

    #[test]
    fn no_alias_is_claimed_by_two_venues() {
        let mut seen: Vec<&str> = Vec::new();
        for info in VENUES {
            for alias in info.aliases {
                assert!(!seen.contains(alias), "alias {alias} is claimed twice");
                seen.push(alias);
            }
        }
    }
}
