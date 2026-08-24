//! International Monetary Fund — the SDMX service at `api.imf.org`.
//!
//! The IMF's value is coverage rather than depth: the same statistic, compiled
//! to one methodology, for nearly every country on earth. That is the feed for a
//! question about an economy no national statistics office publishes in English.
//!
//! An id is `FLOW/KEY`, e.g. `CPI/USA.CPI._T.IX.M`. Two things about this
//! service are worth knowing before typing a key:
//!
//!  - Countries are **ISO alpha-3**. `US` is not a code the IMF knows, and
//!    asking for one returns HTTP 200 with a body containing the dataflow's
//!    description and no observations at all. That is why
//!    [`sdmx::parse_sdmx_csv`] treats an observation-free document as an error
//!    with a hint rather than as an empty series.
//!  - Some dataflows are served only to registered callers and answer 403 to
//!    anyone else. The catalogue below is limited to flows that answer
//!    anonymously, which is what this terminal can actually promise.
//!
//! The IMF also states no title, unit or seasonal adjustment anywhere in its
//! data message — every descriptive column comes back empty — so the catalogue's
//! sentence is not a nicety here, it is the only thing a panel header can show.
//!
//! Everything below this doc comment is configuration: the reader itself lives
//! in [`crate::sources::sdmx`].

use std::sync::LazyLock;

use terminal_core::types::{DataSearchResult, DataSeriesResponse, DataSource};

use crate::app::AppState;
use crate::error::Result;
use crate::sources::sdmx::{self, SdmxAgency, SdmxCatalogueEntry};

/// The economies worth offering by name. The IMF covers nearly two hundred; a
/// search over a list that long stops being a list, and any other alpha-3 code
/// still works when typed directly.
const COUNTRIES: &[(&str, &str)] = &[
    ("USA", "United States"),
    ("GBR", "United Kingdom"),
    ("DEU", "Germany"),
    ("FRA", "France"),
    ("JPN", "Japan"),
    ("CHN", "China"),
    ("IND", "India"),
    ("BRA", "Brazil"),
    ("CAN", "Canada"),
    ("MEX", "Mexico"),
    ("TUR", "Türkiye"),
    ("ARG", "Argentina"),
];

static CATALOGUE: LazyLock<Vec<SdmxCatalogueEntry>> = LazyLock::new(|| {
    let mut entries: Vec<SdmxCatalogueEntry> = COUNTRIES
        .iter()
        .map(|(code, name)| {
            SdmxCatalogueEntry::new(
                format!("CPI/{code}.CPI._T.IX.M"),
                format!("{name} — consumer price index, all items"),
            )
            .with_units("Index")
            .with_frequency("Monthly")
            .with_keywords(format!("inflation cpi prices {code} imf"))
        })
        .collect();

    // No `ER/USA…`: the dollar exchange rate against itself is not a series.
    entries.extend(
        COUNTRIES
            .iter()
            .filter(|(code, _)| *code != "USA")
            .map(|(code, name)| {
                SdmxCatalogueEntry::new(
                    format!("ER/{code}.USD_XDC.PA_RT.M"),
                    format!("{name} — US dollar exchange rate, period average"),
                )
                .with_units("National currency per USD")
                .with_frequency("Monthly")
                .with_keywords(format!("fx forex currency exchange rate {code} imf"))
            }),
    );

    entries
});

/// The curated cross-country series, for the shared reader and its tests.
pub fn catalogue() -> &'static [SdmxCatalogueEntry] {
    &CATALOGUE
}

pub const IMF: SdmxAgency = SdmxAgency {
    provider: DataSource::Imf,
    agency: "the IMF",
    // The IMF reads the Accept header and ignores `?format=`; the ECB and the
    // OECD do the reverse. Sending both is one code path instead of a per-agency
    // branch, and neither service objects to the one it ignores.
    format: "csv",
    accept: "application/vnd.sdmx.data+csv;version=2.0.0",
    retries: 1,
    base: |state| state.config().imf_api_base.clone(),
    // The Data Explorer addresses a dataset by URN and has no per-key
    // permalink, so this lands the reader on the dataflow the series came from.
    web_url: |_base, flow, _key| {
        format!(
            "https://data.imf.org/en/Data-Explorer?datasetUrn=IMF.STA:{}",
            urlencoding::encode(flow)
        )
    },
    catalogue,
};

/// A series and its observations, from the IMF's SDMX service.
pub async fn get_series(
    state: &AppState,
    id: &str,
    start: Option<&str>,
    end: Option<&str>,
) -> Result<DataSeriesResponse> {
    sdmx::series(state, &IMF, id, start, end).await
}

/// Rank the curated cross-country catalogue against a query. The IMF publishes
/// no free-text search endpoint; see [`sdmx::search_catalogue`].
pub async fn search(state: &AppState, query: &str, limit: usize) -> Result<Vec<DataSearchResult>> {
    let _ = state;
    sdmx::search(&IMF, query, limit)
}

/// `None` when this deployment can use the publisher; a string is the
/// operator-facing reason it cannot.
///
/// Always `None`: the flows in the catalogue answer anonymously by construction,
/// so there is no credential an operator could have failed to set.
pub fn unavailable(_state: &AppState) -> Option<String> {
    None
}
