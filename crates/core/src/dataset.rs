//! The reference data sources the terminal reads, and how a series at one of
//! them is named.
//!
//! A prediction market settles against a number somebody else publishes. Twelve
//! publishers are wired in here, and they agree on nothing: the BLS names a
//! series `LNS14000000`, the ECB names one `EXR/D.USD.EUR.SP00.A`, the Federal
//! Reserve names one `H15/RIFLGFCY10_N.B`, FRED names one `UNRATE`. Rather than
//! twelve command sets, every data command takes one *reference* — an optional
//! source prefix and an identifier — and this module is the only place that
//! knows how to read one.
//!
//! ```text
//! UNRATE                       → FRED (the default, so nothing changes)
//! bls:LNS14000000              → Bureau of Labor Statistics
//! ecb:EXR/D.USD.EUR.SP00.A     → European Central Bank
//! fed:H15/RIFLGFCY10_N.B       → Federal Reserve
//! ```
//!
//! This is [`crate::venue`] applied to publishers instead of brokers, and for
//! the same reason: one table drives the parser, the help text, the search
//! filter and the client, so a source added to one and not the others is a
//! compile error rather than a runtime throw. The two share
//! [`crate::venue::AliasContext`] and [`crate::venue::IdCase`] outright, because
//! the rules about ordinary-English aliases and identifier folding are the same
//! rules and should not drift apart.
//!
//! Case is part of the identifier, not decoration. FRED and the BLS shout their
//! ids; the SDMX agencies mix case *inside* a key (`EXR/D.USD.EUR.SP00.A`) and
//! folding either way 404s it, which is what [`IdCase::Keep`] is for.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::venue::{AliasContext, IdCase};

/// What a source is *for*, which decides which command opens it.
///
/// Declared rather than inferred from whether a method happens to exist: `ECOS`
/// has to know which sources it may search before it asks any of them, and a
/// reader picking a prefix has to be told that `sec:` is not something `ECO`
/// will chart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../../../client/src/lib/api/gen/")]
pub enum DataSourceKind {
    /// Publishes observations over time — chartable by `ECO`.
    Series,
    /// Publishes documents or records — bills, filings, dataset metadata.
    Documents,
    /// Publishes market prices — feeds the `STK`/`CRY` provider chain.
    Prices,
}

impl DataSourceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            DataSourceKind::Series => "series",
            DataSourceKind::Documents => "documents",
            DataSourceKind::Prices => "prices",
        }
    }
}

impl fmt::Display for DataSourceKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// How a deployment supplies a source's credential, when it needs one.
#[derive(Debug, Clone, Copy)]
pub struct DataSourceKey {
    /// Environment variable read for the credential.
    pub env: &'static str,
    /// Where a reader gets one.
    pub signup: &'static str,
    /// What the source still does without it.
    ///
    /// Empty means nothing at all — the difference between a degraded feed and
    /// an absent one, which the terminal has to state rather than let a reader
    /// discover through an error.
    pub without_key: &'static str,
}

/// A publisher's row in the registry.
#[derive(Debug, Clone, Copy)]
pub struct DataSourceInfo {
    pub id: DataSource,
    /// Full name, for panel headers and error messages.
    pub label: &'static str,
    /// Short badge for a table column.
    pub code: &'static str,
    /// Canonical command-line prefix, including the colon.
    pub prefix: &'static str,
    pub kind: DataSourceKind,
    /// What this publisher calls an identifier, for usage text.
    pub id_label: &'static str,
    /// A real identifier, shown when a reader gets the shape wrong.
    pub id_example: &'static str,
    pub case: IdCase,
    /// Public documentation, so a panel can link out.
    pub site: &'static str,
    /// One line on what this publisher actually covers.
    pub covers: &'static str,
    /// Names a reader might type. Deliberately generous — the cost of accepting
    /// a synonym is nil and the cost of rejecting one is a command retyped.
    pub aliases: &'static [&'static str],
    /// An unprefixed reference belongs to this source. Exactly one sets it.
    pub is_default: bool,
    /// This source's references print without their prefix. Follows from being
    /// the default.
    pub bare_ref: bool,
    /// The credential this source needs, when it needs one.
    pub key: Option<DataSourceKey>,
}

/// The publishers the terminal reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export, export_to = "../../../client/src/lib/api/gen/")]
pub enum DataSource {
    Fred,
    Bls,
    Ecb,
    Imf,
    Oecd,
    Fed,
    Eia,
    Cftc,
    Congress,
    Sec,
    Datagov,
    Polygon,
}

impl DataSource {
    /// The source's wire identifier — what appears in a URL and in JSON.
    pub fn as_str(self) -> &'static str {
        match self {
            DataSource::Fred => "fred",
            DataSource::Bls => "bls",
            DataSource::Ecb => "ecb",
            DataSource::Imf => "imf",
            DataSource::Oecd => "oecd",
            DataSource::Fed => "fed",
            DataSource::Eia => "eia",
            DataSource::Cftc => "cftc",
            DataSource::Congress => "congress",
            DataSource::Sec => "sec",
            DataSource::Datagov => "datagov",
            DataSource::Polygon => "polygon",
        }
    }

    pub fn info(self) -> &'static DataSourceInfo {
        DATA_SOURCES
            .iter()
            .find(|s| s.id == self)
            .expect("every DataSource variant has a registry row")
    }
}

impl fmt::Display for DataSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for DataSource {
    type Err = ();

    /// Reads the *wire* identifier only. Aliases go through
    /// [`parse_data_source`].
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "fred" => Ok(DataSource::Fred),
            "bls" => Ok(DataSource::Bls),
            "ecb" => Ok(DataSource::Ecb),
            "imf" => Ok(DataSource::Imf),
            "oecd" => Ok(DataSource::Oecd),
            "fed" => Ok(DataSource::Fed),
            "eia" => Ok(DataSource::Eia),
            "cftc" => Ok(DataSource::Cftc),
            "congress" => Ok(DataSource::Congress),
            "sec" => Ok(DataSource::Sec),
            "datagov" => Ok(DataSource::Datagov),
            "polygon" => Ok(DataSource::Polygon),
            _ => Err(()),
        }
    }
}

/// The registry. One row per publisher; every downstream difference reads here.
pub static DATA_SOURCES: &[DataSourceInfo] = &[
    DataSourceInfo {
        id: DataSource::Fred,
        label: "FRED (St. Louis Fed)",
        code: "FRED",
        prefix: "fred:",
        kind: DataSourceKind::Series,
        id_label: "series id",
        id_example: "UNRATE",
        case: IdCase::Upper,
        site: "https://fred.stlouisfed.org",
        covers: "800,000+ US and international series, aggregated from 100+ publishers",
        aliases: &["fred", "stlouisfed", "stl"],
        // Every `FRED <id>` in this terminal's history, help and README predates
        // the other eleven sources; an unprefixed reference has to keep meaning
        // what it always did.
        is_default: true,
        bare_ref: true,
        key: Some(DataSourceKey {
            env: "FRED_API_KEY",
            signup: "https://fred.stlouisfed.org/docs/api/api_key.html",
            without_key: "the fred.stlouisfed.org scrape, which datacentre IPs are often refused",
        }),
    },
    DataSourceInfo {
        id: DataSource::Bls,
        label: "Bureau of Labor Statistics",
        code: "BLS",
        prefix: "bls:",
        kind: DataSourceKind::Series,
        id_label: "series id",
        id_example: "LNS14000000",
        case: IdCase::Upper,
        site: "https://www.bls.gov/developers/",
        covers: "CPI, unemployment, payrolls, earnings, productivity — the US labour statistics \
                 of record",
        aliases: &["bls", "labor", "labour", "bureauoflaborstatistics"],
        is_default: false,
        bare_ref: false,
        key: Some(DataSourceKey {
            env: "BLS_API_KEY",
            signup: "https://data.bls.gov/registrationEngine/",
            without_key: "the v1 API: 25 queries a day and 10 years of history per call",
        }),
    },
    DataSourceInfo {
        id: DataSource::Ecb,
        label: "European Central Bank",
        code: "ECB",
        prefix: "ecb:",
        kind: DataSourceKind::Series,
        id_label: "SDMX key",
        id_example: "EXR/D.USD.EUR.SP00.A",
        case: IdCase::Keep,
        site: "https://data.ecb.europa.eu",
        covers: "euro area rates, exchange rates, HICP inflation, monetary aggregates",
        aliases: &["ecb", "europeancentralbank", "eurozone"],
        is_default: false,
        bare_ref: false,
        key: None,
    },
    DataSourceInfo {
        id: DataSource::Imf,
        label: "International Monetary Fund",
        code: "IMF",
        prefix: "imf:",
        kind: DataSourceKind::Series,
        id_label: "SDMX key",
        id_example: "CPI/USA.CPI._T.IX.M",
        case: IdCase::Keep,
        site: "https://data.imf.org",
        covers: "cross-country CPI, balance of payments, reserves, government finance",
        aliases: &["imf", "fund"],
        is_default: false,
        bare_ref: false,
        key: None,
    },
    DataSourceInfo {
        id: DataSource::Oecd,
        label: "OECD",
        code: "OECD",
        prefix: "oecd:",
        kind: DataSourceKind::Series,
        id_label: "SDMX key",
        id_example: "DSD_KEI@DF_KEI/USA.M.CP.GR._Z._Z.GY",
        case: IdCase::Keep,
        site: "https://data-explorer.oecd.org",
        covers: "member-country growth, inflation, unemployment and leading indicators",
        aliases: &["oecd"],
        is_default: false,
        bare_ref: false,
        key: None,
    },
    DataSourceInfo {
        id: DataSource::Fed,
        label: "Federal Reserve Board",
        code: "FED",
        prefix: "fed:",
        kind: DataSourceKind::Series,
        id_label: "release/series",
        id_example: "H15/RIFLGFCY10_N.B",
        case: IdCase::Keep,
        site: "https://www.federalreserve.gov/datadownload/",
        covers: "H.15 yields, H.4.1 balance sheet, H.6 money stock, G.17 industrial production",
        aliases: &["fed", "federalreserve", "frb", "board", "ddp"],
        is_default: false,
        bare_ref: false,
        key: None,
    },
    DataSourceInfo {
        id: DataSource::Eia,
        label: "US Energy Information Administration",
        code: "EIA",
        prefix: "eia:",
        kind: DataSourceKind::Series,
        id_label: "route/facet path",
        id_example: "petroleum/pri/gnd/EMM_EPMR_PTE_NUS_DPG",
        case: IdCase::Keep,
        site: "https://www.eia.gov/opendata/",
        covers: "crude, gasoline, natural gas, electricity and generation — the feeds energy \
                 markets settle on",
        aliases: &["eia", "energy"],
        is_default: false,
        bare_ref: false,
        key: Some(DataSourceKey {
            env: "EIA_API_KEY",
            signup: "https://www.eia.gov/opendata/register.php",
            // The EIA publishes no anonymous tier at all, so this is the honest
            // answer rather than an omission.
            without_key: "",
        }),
    },
    DataSourceInfo {
        id: DataSource::Cftc,
        label: "CFTC",
        code: "CFTC",
        prefix: "cftc:",
        kind: DataSourceKind::Series,
        id_label: "report/market/field",
        id_example: "legacy/GOLD/noncomm_net",
        case: IdCase::Keep,
        site: "https://publicreporting.cftc.gov",
        covers: "Commitments of Traders — who is long and short each futures market, weekly \
                 since 1986",
        aliases: &["cftc", "cot", "commitments"],
        is_default: false,
        bare_ref: false,
        key: None,
    },
    DataSourceInfo {
        id: DataSource::Congress,
        label: "Congress.gov",
        code: "CONG",
        prefix: "congress:",
        kind: DataSourceKind::Documents,
        id_label: "congress/type/number",
        id_example: "119/hr/1",
        case: IdCase::Lower,
        site: "https://api.congress.gov",
        covers: "bills, amendments, members and roll-call actions back to the 93rd Congress",
        aliases: &["congress", "congressgov", "bills", "gov"],
        is_default: false,
        bare_ref: false,
        key: Some(DataSourceKey {
            env: "CONGRESS_API_KEY",
            signup: "https://api.congress.gov/sign-up/",
            without_key: "api.data.gov's shared DEMO_KEY, which is rate-limited per IP",
        }),
    },
    DataSourceInfo {
        id: DataSource::Sec,
        label: "SEC EDGAR",
        code: "SEC",
        prefix: "sec:",
        kind: DataSourceKind::Documents,
        id_label: "ticker or CIK",
        id_example: "AAPL",
        case: IdCase::Upper,
        site: "https://www.sec.gov/edgar/sec-api-documentation",
        covers: "company filings and every XBRL fact a US issuer has reported since 2009",
        aliases: &["sec", "edgar"],
        is_default: false,
        bare_ref: false,
        key: None,
    },
    DataSourceInfo {
        id: DataSource::Datagov,
        label: "data.gov",
        code: "DGOV",
        prefix: "datagov:",
        kind: DataSourceKind::Documents,
        id_label: "dataset id",
        id_example: "consumer-price-index",
        case: IdCase::Lower,
        site: "https://data.gov/developers/apis/",
        covers: "the US government's dataset catalogue — 300,000+ datasets across every federal \
                 agency",
        aliases: &["datagov", "data-gov", "usgov", "usa"],
        is_default: false,
        bare_ref: false,
        key: Some(DataSourceKey {
            env: "DATAGOV_API_KEY",
            signup: "https://api.data.gov/signup/",
            without_key: "api.data.gov's shared DEMO_KEY, which is rate-limited per IP",
        }),
    },
    DataSourceInfo {
        id: DataSource::Polygon,
        label: "Polygon.io",
        code: "POLY",
        prefix: "polygon:",
        kind: DataSourceKind::Prices,
        id_label: "symbol",
        id_example: "AAPL",
        case: IdCase::Upper,
        site: "https://polygon.io/docs",
        covers: "consolidated US equity, index, FX and crypto bars — the paid tape behind STK, \
                 CRY and IMP",
        aliases: &["polygon", "poly", "polygonio"],
        is_default: false,
        bare_ref: false,
        key: Some(DataSourceKey {
            env: "POLYGON_API_KEY",
            signup: "https://polygon.io/dashboard/api-keys",
            without_key: "",
        }),
    },
];

/// Every source identifier, in registry order.
pub fn data_source_ids() -> Vec<DataSource> {
    DATA_SOURCES.iter().map(|s| s.id).collect()
}

/// The sources `ECO` can chart and `ECOS` can search, in registry order.
pub fn series_source_ids() -> Vec<DataSource> {
    DATA_SOURCES
        .iter()
        .filter(|s| s.kind == DataSourceKind::Series)
        .map(|s| s.id)
        .collect()
}

pub fn data_source_info(source: DataSource) -> &'static DataSourceInfo {
    source.info()
}

/// Whether `value` is a source's wire identifier.
pub fn is_data_source(value: &str) -> bool {
    DataSource::from_str(value).is_ok()
}

/// The source an unprefixed reference belongs to.
pub const DEFAULT_DATA_SOURCE: DataSource = DataSource::Fred;

/// Aliases that are ordinary English words.
///
/// Fine as a prefix — `gov:119/hr/1` is unambiguous — but they must not be
/// claimed out of free text, or `ECOS government spending` quietly becomes a
/// Congress.gov search for one word. Same rule, and same reason, as the venue
/// table's prefix-only aliases.
const PREFIX_ONLY_ALIASES: &[&str] = &[
    "gov", "usa", "energy", "labor", "labour", "fund", "board", "bills", "poly",
];

/// Read a source name or alias. `None` when the token names no source.
pub fn parse_data_source(token: &str, context: AliasContext) -> Option<DataSource> {
    let key = token.trim().to_lowercase();
    if context == AliasContext::Word && PREFIX_ONLY_ALIASES.contains(&key.as_str()) {
        return None;
    }
    DATA_SOURCES
        .iter()
        .find(|s| s.aliases.contains(&key.as_str()))
        .map(|s| s.id)
}

/// A series, filing or dataset at a named publisher.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataRef {
    pub source: DataSource,
    pub id: String,
}

/// Why a reference could not be read. Both variants are shown to the reader.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DataRefError {
    /// A known source prefix with nothing after it.
    MissingId { source: DataSource, raw: String },
    /// `foo:bar` where `foo` names no source.
    UnknownPrefix { prefix: String },
    /// An unprefixed reference that is empty.
    MissingReference,
}

impl fmt::Display for DataRefError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DataRefError::MissingId { source, raw } => write!(
                f,
                "\"{}\" names a source but no {}",
                raw,
                data_source_info(*source).id_label
            ),
            DataRefError::UnknownPrefix { prefix } => {
                // Only the chartable ones are offered: pointing a reader at
                // `sec:` in answer to a mistyped `ECO` prefix sends them to a
                // source that command cannot draw.
                let prefixes: Vec<&str> = DATA_SOURCES
                    .iter()
                    .filter(|s| s.kind == DataSourceKind::Series)
                    .map(|s| s.prefix)
                    .collect();
                write!(
                    f,
                    "Unknown source prefix \"{}\". Try {}",
                    prefix,
                    prefixes.join(", ")
                )
            }
            DataRefError::MissingReference => f.write_str("Missing series id"),
        }
    }
}

impl std::error::Error for DataRefError {}

/// Normalise an identifier to the case its publisher actually answers to.
pub fn normalise_data_id(source: DataSource, id: &str) -> String {
    match data_source_info(source).case {
        IdCase::Upper => id.to_uppercase(),
        IdCase::Lower => id.to_lowercase(),
        IdCase::Keep => id.to_string(),
    }
}

/// Read `[source:]identifier`.
///
/// Only a *known* prefix is treated as one. Several publishers put colons
/// nowhere in an identifier and none put one first, so a bare `foo:bar` with an
/// unrecognised `foo` is far more likely to be a typo than an id — it is
/// reported rather than silently sent to FRED, which would 404 it with a message
/// about FRED.
pub fn parse_data_ref(raw: &str, fallback: DataSource) -> Result<DataRef, DataRefError> {
    let text = raw.trim();

    if let Some(colon) = text.find(':') {
        if colon > 0 {
            let prefix = &text[..colon];
            return match parse_data_source(prefix, AliasContext::Prefix) {
                Some(source) => {
                    let id = text[colon + 1..].trim();
                    if id.is_empty() {
                        return Err(DataRefError::MissingId {
                            source,
                            raw: raw.to_string(),
                        });
                    }
                    Ok(DataRef {
                        source,
                        id: normalise_data_id(source, id),
                    })
                }
                None => Err(DataRefError::UnknownPrefix {
                    prefix: prefix.to_string(),
                }),
            };
        }
    }

    if text.is_empty() {
        return Err(DataRefError::MissingReference);
    }
    Ok(DataRef {
        source: fallback,
        id: normalise_data_id(fallback, text),
    })
}

/// [`parse_data_ref`] against the default source.
pub fn parse_data_ref_default(raw: &str) -> Result<DataRef, DataRefError> {
    parse_data_ref(raw, DEFAULT_DATA_SOURCE)
}

/// The canonical string form of a reference.
///
/// The default source prints bare, because that is what every existing command,
/// example and README line already says.
pub fn format_data_ref(reference: &DataRef) -> String {
    let info = data_source_info(reference.source);
    if info.bare_ref {
        reference.id.clone()
    } else {
        format!("{}{}", info.prefix, reference.id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fred_is_the_default_and_prints_bare() {
        assert_eq!(DEFAULT_DATA_SOURCE, DataSource::Fred);
        let parsed = parse_data_ref_default("UNRATE").unwrap();
        assert_eq!(parsed.source, DataSource::Fred);
        assert_eq!(format_data_ref(&parsed), "UNRATE");
    }

    #[test]
    fn an_sdmx_key_keeps_the_case_it_was_typed_in() {
        // Folding either way 404s it: the case inside a key is meaningful.
        let parsed = parse_data_ref_default("ecb:EXR/D.USD.EUR.SP00.A").unwrap();
        assert_eq!(parsed.source, DataSource::Ecb);
        assert_eq!(parsed.id, "EXR/D.USD.EUR.SP00.A");
        assert_eq!(
            parse_data_ref_default("oecd:DSD_KEI@DF_KEI/USA.M.PRVM.IX...")
                .unwrap()
                .id,
            "DSD_KEI@DF_KEI/USA.M.PRVM.IX..."
        );
    }

    #[test]
    fn only_the_first_colon_separates_so_a_key_may_contain_more() {
        // `fed:H15/RIFLGFCY10_N.B` has no second colon, but an SDMX key can;
        // the rule is the same one the venue grammar uses.
        let parsed = parse_data_ref_default("imf:CPI/US.CPI._Z._Z.M").unwrap();
        assert_eq!(parsed.id, "CPI/US.CPI._Z._Z.M");
        assert_eq!(parse_data_ref_default("bls:a:b").unwrap().id, "A:B");
    }

    #[test]
    fn folds_an_identifier_to_the_case_its_publisher_answers_to() {
        assert_eq!(parse_data_ref_default("unrate").unwrap().id, "UNRATE");
        assert_eq!(
            parse_data_ref_default("bls:lns14000000").unwrap().id,
            "LNS14000000"
        );
        assert_eq!(
            parse_data_ref_default("congress:119/HR/1").unwrap().id,
            "119/hr/1"
        );
    }

    #[test]
    fn rejects_an_unknown_prefix_and_offers_only_the_chartable_ones() {
        let err = parse_data_ref_default("nasdaq:foo").unwrap_err();
        assert!(matches!(err, DataRefError::UnknownPrefix { .. }));
        let message = err.to_string();
        assert!(message.contains("bls:"));
        assert!(message.contains("ecb:"));
        // `sec:` is a documents source; offering it in answer to a bad `ECO`
        // prefix sends the reader somewhere `ECO` cannot draw.
        assert!(!message.contains("sec:"));
    }

    #[test]
    fn rejects_a_prefix_with_no_identifier_using_the_publishers_own_word_for_one() {
        let err = parse_data_ref_default("ecb:").unwrap_err();
        assert!(err.to_string().contains("SDMX key"));
        let err = parse_data_ref_default("bls:  ").unwrap_err();
        assert!(err.to_string().contains("series id"));
    }

    #[test]
    fn an_empty_reference_is_its_own_failure() {
        assert_eq!(
            parse_data_ref_default("   ").unwrap_err(),
            DataRefError::MissingReference
        );
    }

    #[test]
    fn declines_english_word_aliases_read_out_of_free_text() {
        // `ECOS government spending` must not become a Congress.gov search for
        // one word.
        for word in ["gov", "usa", "energy", "labor", "fund", "board", "poly"] {
            assert_eq!(parse_data_source(word, AliasContext::Word), None, "{word}");
        }
        // The same tokens are unambiguous as a prefix.
        assert_eq!(
            parse_data_source("gov", AliasContext::Prefix),
            Some(DataSource::Congress)
        );
        assert_eq!(
            parse_data_source("energy", AliasContext::Prefix),
            Some(DataSource::Eia)
        );
    }

    #[test]
    fn still_claims_unambiguous_aliases_out_of_free_text() {
        assert_eq!(
            parse_data_source("bls", AliasContext::Word),
            Some(DataSource::Bls)
        );
        assert_eq!(
            parse_data_source("oecd", AliasContext::Word),
            Some(DataSource::Oecd)
        );
    }

    #[test]
    fn every_variant_has_exactly_one_registry_row() {
        assert_eq!(DATA_SOURCES.len(), 12);
        for source in data_source_ids() {
            assert_eq!(data_source_info(source).id, source);
        }
        assert_eq!(DATA_SOURCES.iter().filter(|s| s.is_default).count(), 1);
    }

    #[test]
    fn wire_identifiers_round_trip() {
        for source in data_source_ids() {
            assert_eq!(DataSource::from_str(source.as_str()), Ok(source));
            assert!(is_data_source(source.as_str()));
        }
        assert!(!is_data_source("worldbank"));
    }

    #[test]
    fn no_alias_is_claimed_by_two_sources() {
        let mut seen: Vec<&str> = Vec::new();
        for info in DATA_SOURCES {
            for alias in info.aliases {
                assert!(!seen.contains(alias), "alias {alias} is claimed twice");
                seen.push(alias);
            }
        }
    }

    #[test]
    fn a_source_that_serves_nothing_without_a_key_says_so_rather_than_omitting_it() {
        // An empty `without_key` is the honest answer for a publisher with no
        // anonymous tier, and it has to be distinguishable from "no key needed".
        let eia = data_source_info(DataSource::Eia)
            .key
            .expect("the EIA is keyed");
        assert!(eia.without_key.is_empty());
        let bls = data_source_info(DataSource::Bls)
            .key
            .expect("the BLS is keyed");
        assert!(!bls.without_key.is_empty());
        assert!(data_source_info(DataSource::Ecb).key.is_none());
    }

    #[test]
    fn every_advertised_example_is_a_reference_that_parses_back_to_its_own_publisher() {
        // `SRC` prints `id_example` as the thing to type next, so a stale one is
        // not a cosmetic slip: the reader types it, the panel refuses it, and
        // the board has told them a lie about what this deployment serves.
        // Two of them were stale when this test was written — the IMF row named
        // a country `US` where the dataflow spells it `USA`, and the OECD row
        // left three dimensions empty, which selects three series and charts as
        // none. Both were caught by typing them, which is what this automates.
        for info in DATA_SOURCES.iter() {
            let typed = format!("{}{}", info.prefix, info.id_example);
            let parsed = parse_data_ref_default(&typed).unwrap_or_else(|error| {
                panic!(
                    "{} advertises {typed:?}, which does not parse: {error:?}",
                    info.code
                )
            });
            assert_eq!(
                parsed.source, info.id,
                "{} advertises {typed:?}, which routes to {:?}",
                info.code, parsed.source
            );
            assert!(
                !parsed.id.is_empty(),
                "{} advertises {typed:?}, which carries no id",
                info.code
            );
        }
    }

    #[test]
    fn eco_charts_the_series_sources_and_nothing_else() {
        let chartable = series_source_ids();
        assert!(chartable.contains(&DataSource::Fred));
        assert!(chartable.contains(&DataSource::Cftc));
        assert!(!chartable.contains(&DataSource::Sec));
        assert!(!chartable.contains(&DataSource::Polygon));
        assert_eq!(chartable.len(), 8);
    }
}
