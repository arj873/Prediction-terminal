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

/// Publishers the registry lists that no verb reads yet.
///
/// `SRC` exists to say what this deployment can *actually* serve, so a row that
/// reports READY for a publisher nothing can open is the exact failure the board
/// was built to prevent — worse than omitting it, because a reader would type
/// the reference and be told to use a command that does not exist. They stay on
/// the board with their coverage, because knowing the terminal knows about
/// EDGAR is worth something, and they say plainly that nothing reads them yet.
const NOT_YET_SERVED: &[DataSource] = &[
    DataSource::Congress,
    DataSource::Sec,
    DataSource::Datagov,
    DataSource::Polygon,
];

/// What each publisher's credential does for this deployment.
///
/// The note is what a reader can act on: which arm will answer, what a key
/// would add, or why nothing will answer at all. An empty note means there is
/// nothing to say — no credential is involved.
fn credential_state(state: &AppState, source: DataSource) -> (bool, String) {
    if let Some(blocked) = unavailable(state, source) {
        return (false, blocked);
    }

    if NOT_YET_SERVED.contains(&source) {
        let info = data_source_info(source);
        return (
            false,
            format!(
                "No command reads {} yet — it is registered so the reference grammar and the \
                 credential are in place, but nothing opens it.",
                info.label
            ),
        );
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

#[cfg(test)]
mod tests {
    //! Dispatch and board tests.
    //!
    //! What matters here is not any publisher's dialect — each module tests its
    //! own — but that the interface tells the truth about which of them this
    //! deployment can reach, and refuses in terms a reader can act on.

    use super::*;

    use crate::config::Config;

    fn state() -> AppState {
        AppState::new(Config::default())
    }

    #[test]
    fn the_board_lists_every_registered_publisher() {
        let board = source_statuses(&state());
        assert_eq!(board.len(), DATA_SOURCES.len());
        for (row, info) in board.iter().zip(DATA_SOURCES) {
            assert_eq!(row.id, info.id);
            assert_eq!(row.label, info.label);
        }
    }

    #[test]
    fn a_publisher_nothing_reads_is_not_reported_ready() {
        // `SRC` exists to say what can actually be served. A READY row for a
        // publisher with no command behind it would send a reader to type a
        // reference and be told to use a command that does not exist.
        let board = source_statuses(&state());
        for source in NOT_YET_SERVED {
            let row = board
                .iter()
                .find(|row| row.id == *source)
                .expect("every registered publisher has a row");
            assert!(!row.available, "{source}");
            assert!(row.note.contains("No command reads"), "{source}");
        }
    }

    #[test]
    fn the_eia_is_unavailable_without_its_key_and_says_what_to_set() {
        // The one publisher with no anonymous tier at all: this is a hard gate
        // rather than a degraded mode, because asking without a key can only
        // ever produce a 403.
        let board = source_statuses(&state());
        let eia = board
            .iter()
            .find(|row| row.id == DataSource::Eia)
            .expect("the EIA has a row");
        assert!(!eia.available);
        assert!(eia.note.contains("EIA_API_KEY"), "{}", eia.note);
    }

    #[test]
    fn a_keyless_publisher_says_so_rather_than_leaving_the_column_blank() {
        let board = source_statuses(&state());
        for source in [DataSource::Ecb, DataSource::Imf, DataSource::Oecd] {
            let row = board.iter().find(|row| row.id == source).expect("a row");
            assert!(row.available, "{source}");
            assert_eq!(row.note, "No credential needed.", "{source}");
        }
    }

    #[test]
    fn a_degraded_publisher_is_ready_and_says_what_a_key_would_add() {
        // Different from both of the above: FRED and the BLS answer without a
        // key, less well. A reader can act on that, so the row says how.
        let board = source_statuses(&state());
        for (source, wanted) in [
            (DataSource::Fred, "FRED_API_KEY"),
            (DataSource::Bls, "BLS_API_KEY"),
        ] {
            let row = board.iter().find(|row| row.id == source).expect("a row");
            assert!(row.available, "{source}");
            assert!(row.note.contains(wanted), "{source}: {}", row.note);
        }
    }

    #[test]
    fn the_reference_on_the_board_is_the_one_a_reader_types() {
        // With the prefix on for everything but the default, because that is
        // what has to be typed for the reference to reach the right publisher.
        let board = source_statuses(&state());
        let fred = board
            .iter()
            .find(|r| r.id == DataSource::Fred)
            .expect("fred");
        assert_eq!(fred.id_example, "UNRATE");
        let ecb = board.iter().find(|r| r.id == DataSource::Ecb).expect("ecb");
        assert_eq!(ecb.id_example, "ecb:EXR/D.USD.EUR.SP00.A");
    }

    #[tokio::test]
    async fn eco_refuses_a_publisher_that_does_not_publish_observations() {
        // In terms of what it *does* publish, rather than as a 404 that sends
        // the reader hunting for a series that was never there.
        let err = get_series(&state(), "sec:AAPL", None, None)
            .await
            .expect_err("EDGAR publishes filings, not observations");
        assert_eq!(err.code, crate::error::codes::UNSUPPORTED);
        assert!(err.message.contains("SEC EDGAR"), "{}", err.message);
        assert!(
            err.hint
                .as_deref()
                .unwrap_or_default()
                .contains("documents"),
            "{:?}",
            err.hint
        );
    }

    #[tokio::test]
    async fn eco_refuses_a_price_feed_by_naming_the_commands_that_use_it() {
        let err = get_series(&state(), "polygon:AAPL", None, None)
            .await
            .expect_err("Polygon feeds the price chain, not ECO");
        assert!(
            err.hint.as_deref().unwrap_or_default().contains("STK"),
            "{:?}",
            err.hint
        );
    }

    #[tokio::test]
    async fn a_bad_reference_is_reported_with_the_shape_it_should_have_had() {
        let err = get_series(&state(), "worldbank:GDP", None, None)
            .await
            .expect_err("no such publisher");
        assert_eq!(err.code, crate::error::codes::BAD_REQUEST);
        assert!(
            err.hint
                .as_deref()
                .unwrap_or_default()
                .contains("[source:]id"),
            "{:?}",
            err.hint
        );
    }

    #[tokio::test]
    async fn ecos_refuses_an_empty_query_before_it_dials_anyone() {
        let err = search_series(&state(), "   ", &[], 40)
            .await
            .expect_err("a search needs a word");
        assert_eq!(err.code, crate::error::codes::BAD_REQUEST);
    }

    #[test]
    fn a_source_filter_reports_the_name_it_could_not_read() {
        let err = parse_source_list("fred,worldbank").expect_err("no such publisher");
        assert!(err.message.contains("worldbank"), "{}", err.message);
        // And offers only the ones `ECOS` could actually have asked.
        let hint = err.hint.unwrap_or_default();
        assert!(hint.contains("fred"), "{hint}");
        assert!(!hint.contains("sec"), "{hint}");
    }

    #[test]
    fn a_source_filter_keeps_its_order_and_drops_repeats() {
        assert_eq!(
            parse_source_list(" ecb , fred ,ecb").expect("a filter"),
            [DataSource::Ecb, DataSource::Fred]
        );
        assert_eq!(parse_source_list("").expect("no filter"), []);
    }
}

#[cfg(test)]
mod advertised_examples {
    use terminal_core::dataset::{data_source_info, DataSource};

    /// The three SDMX publishers state a key that has to be complete.
    ///
    /// [`super::tests`] and the core registry both check that an advertised
    /// example *parses*. That is not enough here, and both defects this test
    /// was written for prove it: `imf:CPI/US.CPI._Z._Z.M` parses perfectly and
    /// fetches nothing, because the dataflow spells the country `USA`; and
    /// `oecd:DSD_KEI@DF_KEI/USA.M.PRVM.IX...` parses perfectly and matches
    /// three series, which is refused rather than charted. A key is only real
    /// if some catalogue entry names it — and being in the catalogue is also
    /// what gives it a readable title, since the OECD states a measure *code*
    /// where the ECB states a sentence.
    #[test]
    fn each_sdmx_publisher_advertises_a_key_from_its_own_catalogue() {
        for (source, catalogue) in [
            (DataSource::Ecb, super::super::ecb::catalogue()),
            (DataSource::Imf, super::super::imf::catalogue()),
            (DataSource::Oecd, super::super::oecd::catalogue()),
        ] {
            let info = data_source_info(source);
            assert!(
                catalogue.iter().any(|entry| entry.id == info.id_example),
                "{} advertises {:?}, which is not in its catalogue — a reader who \
                 types it gets an empty panel or a refusal, not a chart",
                info.code,
                info.id_example
            );
        }
    }
}
