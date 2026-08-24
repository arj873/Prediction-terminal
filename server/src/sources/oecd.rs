//! OECD — the SDMX service behind the OECD Data Explorer.
//!
//! The value here is comparability: the same definition of "unemployment rate"
//! or "consumer prices" across 38 member countries plus the euro area, the G7
//! and the G20, which is what a question phrased as "will inflation be higher in
//! the US than the euro area" actually needs.
//!
//! An id is `DATAFLOW/KEY`. The Key Economic Indicators flow — `DSD_KEI@DF_KEI`
//! — orders its dimensions
//! `REF_AREA.FREQ.MEASURE.UNIT_MEASURE.ACTIVITY.ADJUSTMENT.TRANSFORMATION`, so
//! US CPI year-on-year is `USA.M.CP.GR._Z._Z.GY`. Swapping `USA` for `EA20`,
//! `DEU`, `GBR`, `JPN` or `G20` gives the same statistic elsewhere, which is the
//! point of using the OECD for it rather than each country's own publisher.
//!
//! Two OECD-specific facts the shared reader is configured around:
//!
//!  - The dataflow reference contains an `@`, and its agency-qualified form
//!    contains commas. Percent-encoding either is at best pointless and has been
//!    seen to earn an HTTP 500, so [`sdmx::encode_sdmx_flow`] validates the shape
//!    and passes it through untouched.
//!  - A burst of requests is shed with HTTP 500 rather than 429 — the same
//!    throttle wearing a different number. Retried through, and named as a
//!    throttle rather than as an outage when it persists.
//!
//! Everything below this doc comment is configuration: the reader itself lives
//! in [`crate::sources::sdmx`].

use std::sync::LazyLock;

use terminal_core::types::{DataSearchResult, DataSeriesResponse, DataSource};

use crate::app::AppState;
use crate::error::Result;
use crate::sources::sdmx::{self, SdmxAgency, SdmxCatalogueEntry};

/// The Key Economic Indicators dataflow, which every catalogued series is in.
const KEI: &str = "DSD_KEI@DF_KEI";

/// The areas the comparison is usually between.
const AREAS: &[(&str, &str)] = &[
    ("USA", "United States"),
    ("EA20", "Euro area (20 countries)"),
    ("DEU", "Germany"),
    ("GBR", "United Kingdom"),
    ("JPN", "Japan"),
    ("CAN", "Canada"),
    ("FRA", "France"),
    ("G20", "G20"),
];

/// One indicator, as the five dimensions after area and frequency:
/// `MEASURE.UNIT_MEASURE.ACTIVITY.ADJUSTMENT.TRANSFORMATION`.
struct Indicator {
    tail: &'static str,
    /// `M` or `Q` — the frequency dimension, which sits second in the key.
    freq: &'static str,
    title: &'static str,
    units: &'static str,
    keywords: &'static str,
}

const INDICATORS: &[Indicator] = &[
    Indicator {
        tail: "CP.GR._Z._Z.GY",
        freq: "M",
        title: "consumer prices, year on year",
        units: "Percent",
        keywords: "inflation cpi prices",
    },
    Indicator {
        tail: "UNEMP.PT_LF._T.Y._Z",
        freq: "M",
        title: "unemployment rate",
        units: "Percent of labour force",
        keywords: "jobless labour employment",
    },
    Indicator {
        tail: "IRLT.PA._Z._Z._Z",
        freq: "M",
        title: "long-term interest rate (10-year government bond)",
        units: "Percent per annum",
        keywords: "yield rates bond govvie 10y",
    },
    Indicator {
        tail: "IR3TIB.PA._Z._Z._Z",
        freq: "M",
        title: "short-term interest rate (3-month interbank)",
        units: "Percent per annum",
        keywords: "rates money market libor 3m",
    },
    Indicator {
        tail: "LI.IX._T.AA._Z",
        freq: "M",
        title: "composite leading indicator (CLI)",
        units: "Index, long-term average = 100",
        keywords: "cli leading cycle turning point recession",
    },
    Indicator {
        tail: "B1GQ_Q.GR._T.Y.GY",
        freq: "Q",
        title: "GDP volume, year on year",
        units: "Percent",
        keywords: "gdp growth output recession",
    },
    Indicator {
        tail: "PRVM.IX.C.Y._Z",
        freq: "M",
        title: "manufacturing production volume",
        units: "Index",
        keywords: "industrial production manufacturing output",
    },
    Indicator {
        tail: "CCICP.PB._Z.Y._Z",
        freq: "M",
        title: "consumer confidence",
        units: "Normalised index",
        keywords: "sentiment survey confidence",
    },
];

/// The comparable headline indicators, per area — built rather than hand-listed,
/// because the whole point of the OECD is that the key differs only in its first
/// dimension.
static CATALOGUE: LazyLock<Vec<SdmxCatalogueEntry>> = LazyLock::new(|| {
    AREAS
        .iter()
        .flat_map(|(code, name)| {
            INDICATORS.iter().map(move |indicator| {
                SdmxCatalogueEntry::new(
                    format!("{KEI}/{code}.{}.{}", indicator.freq, indicator.tail),
                    format!("{name} — {}", indicator.title),
                )
                .with_units(indicator.units)
                .with_frequency(if indicator.freq == "M" {
                    "Monthly"
                } else {
                    "Quarterly"
                })
                .with_keywords(format!("{} {code} oecd", indicator.keywords))
            })
        })
        .collect()
});

/// The curated comparable indicators, for the shared reader and its tests.
pub fn catalogue() -> &'static [SdmxCatalogueEntry] {
    &CATALOGUE
}

pub const OECD: SdmxAgency = SdmxAgency {
    provider: DataSource::Oecd,
    agency: "the OECD",
    // Labelled CSV: every dimension arrives as a code column and again as a
    // label column, which is where the readable unit string comes from.
    format: "csvfilewithlabels",
    accept: "text/csv, application/vnd.sdmx.data+csv;version=1.0.0",
    // Three attempts after the first. The OECD sheds a burst with HTTP 500
    // rather than 429, and the throttle clears in seconds — failing on the first
    // one would report a busy minute as a broken dataset.
    retries: 3,
    base: |state| state.config().oecd_api_base.clone(),
    // The Data Explorer is a single-page application with no stable per-series
    // permalink, so this links to the query itself — which is the thing a reader
    // would need in order to check the number anyway.
    web_url: |base, flow, key| {
        format!(
            "{}/data/{flow}/{key}?format=csvfilewithlabels",
            base.trim_end_matches('/')
        )
    },
    catalogue,
};

/// A series and its observations, from the OECD's SDMX service.
pub async fn get_series(
    state: &AppState,
    id: &str,
    start: Option<&str>,
    end: Option<&str>,
) -> Result<DataSeriesResponse> {
    sdmx::series(state, &OECD, id, start, end).await
}

/// Rank the curated comparable-indicator catalogue against a query. The OECD's
/// own search is a JavaScript client over a structure API; see
/// [`sdmx::search_catalogue`].
pub async fn search(state: &AppState, query: &str, limit: usize) -> Result<Vec<DataSearchResult>> {
    let _ = state;
    sdmx::search(&OECD, query, limit)
}

/// `None` when this deployment can use the publisher; a string is the
/// operator-facing reason it cannot.
///
/// Always `None`: the public SDMX service takes no credential. Its throttle is a
/// per-request condition rather than a deployment one, and is reported as
/// `rate_limited` on the request that hit it.
pub fn unavailable(_state: &AppState) -> Option<String> {
    None
}
