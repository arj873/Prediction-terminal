//! The series publishers, as one interface.
//!
//! Eight upstreams answer the same two questions — "chart this id" and "what
//! ids match these words" — and each was written to its own shape. This is
//! where they become interchangeable, so `ECO` and `ECOS` are one command
//! apiece rather than eight, and so adding a ninth publisher is a registry row
//! plus a module with nothing to remember to update in the router, the client
//! or the help text.
//!
//! [`crate::sources::venues`] is the same idea for brokers, and the same rule
//! holds: availability is a *declaration* rather than something a caller
//! discovers by getting an error. `ECOS` has to know it cannot ask the EIA
//! before it fans out, and the `SRC` board exists to show a reader what this
//! deployment can actually serve.

use futures::future::join_all;
use terminal_core::dataset::{
    data_source_info, is_data_source, parse_data_ref_default, series_source_ids, DataSource,
    DataSourceKind, DATA_SOURCES,
};
use terminal_core::types::{
    DataSearchResponse, DataSearchResult, DataSeriesResponse, DataSourceStatus, ProviderSkipped,
    ProviderUnavailable,
};

use crate::app::AppState;
use crate::error::{Result, UpstreamError};
use crate::sources::{bls, cftc, ecb, eia, feddata, fred, imf, oecd};

/// The order `ECOS` reports results in, and it is deliberate: FRED first
/// because it re-publishes most of the others under one naming scheme, then the
/// primary publishers, then the one that needs a key.
///
/// Registry order would do the same job today and is *not* used, because the
/// registry is ordered for a reader picking a prefix and this is ordered for a
/// search board. Tying them together would make one of the two orders change
/// for the other's reasons.
const PROVIDERS: &[DataSource] = &[
    DataSource::Fred,
    DataSource::Bls,
    DataSource::Fed,
    DataSource::Ecb,
    DataSource::Oecd,
    DataSource::Imf,
    DataSource::Cftc,
    DataSource::Eia,
];

/// Every publisher `ECO` can chart, whether or not this deployment can reach it.
pub fn series_provider_ids() -> &'static [DataSource] {
    PROVIDERS
}

/// Why a publisher cannot answer in this deployment, or `None` when it can.
fn unavailable(state: &AppState, source: DataSource) -> Option<String> {
    match source {
        DataSource::Fred => None,
        DataSource::Bls => bls::unavailable(state),
        DataSource::Fed => feddata::unavailable(state),
        DataSource::Ecb => ecb::unavailable(state),
        DataSource::Oecd => oecd::unavailable(state),
        DataSource::Imf => imf::unavailable(state),
        DataSource::Cftc => cftc::unavailable(state),
        DataSource::Eia => eia::unavailable(state),
        // Not a series publisher; `provider_for` refuses before this is asked.
        _ => None,
    }
}

/// Refuse a source `ECO` cannot chart, in terms of what it does publish.
fn not_chartable(source: DataSource) -> UpstreamError {
    let info = data_source_info(source);
    let hint = match info.kind {
        DataSourceKind::Documents => format!(
            "{} publishes documents rather than observations — use its own command.",
            info.label
        ),
        _ => format!(
            "{} feeds price charts (STK, CRY, IMP) rather than ECO.",
            info.label
        ),
    };
    UpstreamError::unsupported(format!("{} does not publish chartable series", info.label))
        .with_hint(hint)
}

fn provider_for(source: DataSource) -> Result<DataSource> {
    if PROVIDERS.contains(&source) {
        Ok(source)
    } else {
        Err(not_chartable(source))
    }
}

async fn ask_series(
    state: &AppState,
    source: DataSource,
    id: &str,
    start: Option<&str>,
    end: Option<&str>,
) -> Result<DataSeriesResponse> {
    match source {
        // FRED caches behind an `Arc` because two panels asking for the same
        // series is the common case; the interface hands out an owned response,
        // so this is the one arm that clones.
        DataSource::Fred => fred::get_series(state, id, start, end)
            .await
            .map(|response| (*response).clone()),
        DataSource::Bls => bls::get_series(state, id, start, end).await,
        DataSource::Fed => feddata::get_series(state, id, start, end).await,
        DataSource::Ecb => ecb::get_series(state, id, start, end).await,
        DataSource::Oecd => oecd::get_series(state, id, start, end).await,
        DataSource::Imf => imf::get_series(state, id, start, end).await,
        DataSource::Cftc => cftc::get_series(state, id, start, end).await,
        DataSource::Eia => eia::get_series(state, id, start, end).await,
        other => Err(not_chartable(other)),
    }
}

async fn ask_search(
    state: &AppState,
    source: DataSource,
    query: &str,
    limit: usize,
) -> Result<Vec<DataSearchResult>> {
    match source {
        DataSource::Fred => fred::search_series(state, query, limit as u32)
            .await
            .map(|response| response.results.clone()),
        DataSource::Bls => bls::search(state, query, limit).await,
        DataSource::Fed => feddata::search(state, query, limit).await,
        DataSource::Ecb => ecb::search(state, query, limit).await,
        DataSource::Oecd => oecd::search(state, query, limit).await,
        DataSource::Imf => imf::search(state, query, limit).await,
        DataSource::Cftc => cftc::search(state, query, limit).await,
        DataSource::Eia => eia::search(state, query, limit).await,
        other => Err(not_chartable(other)),
    }
}

/* ---------------------------------------------------------------- series */

/// Chart `[source:]id`, defaulting to FRED for an unprefixed reference.
pub async fn get_series(
    state: &AppState,
    reference: &str,
    start: Option<&str>,
    end: Option<&str>,
) -> Result<DataSeriesResponse> {
    let reference = parse_data_ref_default(reference).map_err(|err| {
        UpstreamError::bad_request(err.to_string()).with_hint(
            "A reference is `[source:]id`, e.g. UNRATE, bls:LNS14000000, \
             ecb:EXR/D.USD.EUR.SP00.A, fed:H15/RIFLGFCY10_N.B.",
        )
    })?;

    let source = provider_for(reference.source)?;
    if let Some(blocked) = unavailable(state, source) {
        return Err(UpstreamError::not_configured(format!(
            "{} is not configured",
            data_source_info(source).label
        ))
        .with_hint(blocked));
    }

    ask_series(state, source, &reference.id, start, end).await
}

/* ---------------------------------------------------------------- search */

/// Ask every usable publisher at once and merge what comes back.
///
/// One publisher being down must not empty the board, so failures are collected
/// and reported beside the results rather than thrown: a search that found
/// fourteen series and lost the OECD to a throttle is a useful answer, and
/// saying which one is missing is the difference between that and "no match".
pub async fn search_series(
    state: &AppState,
    query: &str,
    sources: &[DataSource],
    limit: usize,
) -> Result<DataSearchResponse> {
    let query = query.trim();
    if query.is_empty() {
        return Err(UpstreamError::bad_request("Search needs at least one word")
            .with_hint("e.g. `ECOS unemployment`, `ECOS oil eia`, `ECOS inflation ecb oecd`."));
    }

    let wanted: Vec<DataSource> = if sources.is_empty() {
        PROVIDERS.to_vec()
    } else {
        PROVIDERS
            .iter()
            .copied()
            .filter(|p| sources.contains(p))
            .collect()
    };

    // Per publisher, ask for a full share of the limit rather than an equal
    // slice: most queries match in one or two catalogues, and pre-dividing
    // would cap a good publisher at five results to reserve room for seven that
    // matched none.
    let per_provider = limit.clamp(5, 100);

    let asked = wanted.iter().copied().map(|source| async move {
        if let Some(hint) = unavailable(state, source) {
            return (source, Err(UpstreamError::not_configured(hint)), true);
        }
        (
            source,
            ask_search(state, source, query, per_provider).await,
            false,
        )
    });

    let mut lanes: Vec<Vec<DataSearchResult>> = Vec::new();
    let mut unavailable_now: Vec<ProviderUnavailable> = Vec::new();
    let mut skipped: Vec<ProviderSkipped> = Vec::new();

    for (source, outcome, was_skipped) in join_all(asked).await {
        match outcome {
            Ok(found) if !found.is_empty() => lanes.push(found),
            Ok(_) => {}
            Err(err) if was_skipped => skipped.push(ProviderSkipped {
                provider: source,
                hint: err.message,
            }),
            Err(err) => unavailable_now.push(ProviderUnavailable {
                provider: source,
                error: err.message,
            }),
        }
    }

    // Interleave rather than concatenate. Straight concatenation would fill the
    // whole board with FRED — it has 800,000 series and always matches — and
    // bury the primary publisher a reader typed `ECOS` in order to discover.
    let mut results: Vec<DataSearchResult> = Vec::new();
    for round in 0.. {
        let before = results.len();
        for lane in &lanes {
            if results.len() >= limit {
                break;
            }
            if let Some(item) = lane.get(round) {
                results.push(item.clone());
            }
        }
        if results.len() >= limit || results.len() == before {
            break;
        }
    }

    Ok(DataSearchResponse {
        query: query.to_owned(),
        results,
        unavailable: unavailable_now,
        skipped,
    })
}

/* --------------------------------------------------------------- statuses */

/// What each publisher's credential does for this deployment.
///
/// The note is what a reader can act on: which arm will answer, what a key
/// would add, or why nothing will answer at all. An empty note means there is
/// nothing to say — no credential is involved.
fn credential_state(state: &AppState, source: DataSource) -> (bool, String) {
    if let Some(blocked) = unavailable(state, source) {
        return (false, blocked);
    }

    let held = state.config().data_source_key(source).is_some();
    match source {
        DataSource::Fred => (
            true,
            if held {
                "Scrape first, official API behind it.".to_owned()
            } else {
                "Scraping only. Set FRED_API_KEY to add the official API as a fallback — \
                 datacentre IPs are often refused by the scrape."
                    .to_owned()
            },
        ),
        DataSource::Bls => (
            true,
            if held {
                "Registered: 500 queries a day, 20 years per call.".to_owned()
            } else {
                "Unregistered: 25 queries a day, 10 years per call. Set BLS_API_KEY to raise both."
                    .to_owned()
            },
        ),
        DataSource::Congress | DataSource::Datagov => {
            let info = data_source_info(source);
            let env = info.key.map(|k| k.env).unwrap_or_default();
            (
                true,
                if held {
                    format!("Using {env}.")
                } else {
                    format!("Using the shared DEMO_KEY — throttled per IP. Set {env} to lift it.")
                },
            )
        }
        DataSource::Polygon => (
            held,
            if held {
                "Leads the STK / CRY / IMP price chain.".to_owned()
            } else {
                "Set POLYGON_API_KEY to put a licensed tape in front of Yahoo and Nasdaq. \
                 Without it, prices still work — this source simply is not used."
                    .to_owned()
            },
        ),
        _ => (
            true,
            if data_source_info(source).key.is_some() {
                String::new()
            } else {
                "No credential needed.".to_owned()
            },
        ),
    }
}

/// Every publisher, and whether this deployment can serve it.
pub fn source_statuses(state: &AppState) -> Vec<DataSourceStatus> {
    DATA_SOURCES
        .iter()
        .map(|info| {
            let (available, note) = credential_state(state, info.id);
            DataSourceStatus {
                id: info.id,
                label: info.label.to_owned(),
                code: info.code.to_owned(),
                prefix: info.prefix.to_owned(),
                kind: info.kind,
                covers: info.covers.to_owned(),
                site: info.site.to_owned(),
                // Shown as a reader would type it, which for everything but the
                // default means with its prefix on.
                id_example: if info.bare_ref {
                    info.id_example.to_owned()
                } else {
                    format!("{}{}", info.prefix, info.id_example)
                },
                available,
                note,
            }
        })
        .collect()
}

/// Read a comma-separated list of source names, rejecting unknown ones.
pub fn parse_source_list(raw: &str) -> Result<Vec<DataSource>> {
    let mut out: Vec<DataSource> = Vec::new();
    for token in raw.split(',').map(str::trim).filter(|t| !t.is_empty()) {
        if !is_data_source(token) {
            let named: Vec<&str> = series_source_ids()
                .into_iter()
                .map(DataSource::as_str)
                .collect();
            return Err(
                UpstreamError::bad_request(format!("\"{token}\" is not a data source"))
                    .with_hint(format!("Series sources are {}.", named.join(", "))),
            );
        }
        let source = token.parse::<DataSource>().expect("checked above");
        if !out.contains(&source) {
            out.push(source);
        }
    }
    Ok(out)
}
