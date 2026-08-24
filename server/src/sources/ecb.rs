//! European Central Bank — the ECB Data Portal's SDMX service.
//!
//! Keyless, generous, and the publisher of record for everything the euro area
//! settles on: the policy rates themselves, HICP, M3, the AAA yield curve and
//! the daily reference exchange rates that most of the world's EUR conversions
//! are struck at.
//!
//! An id here is `FLOW/KEY` — `EXR/D.USD.EUR.SP00.A` is the daily USD reference
//! rate. The key's dots are dimensions in the order the dataflow declares them,
//! and leaving one empty means "any", which is why
//! [`sdmx::assert_single_series`] refuses a key that ends up selecting more than
//! one series rather than charting their interleaving. The ECB is the one agency
//! of the three that publishes a `KEY` column, so it is also the only one whose
//! refusal can name the series the loose key caught.
//!
//! Everything below this doc comment is configuration: the reader itself lives
//! in [`crate::sources::sdmx`].

use std::sync::LazyLock;

use terminal_core::types::{DataSearchResult, DataSeriesResponse, DataSource};

use crate::app::AppState;
use crate::error::Result;
use crate::sources::sdmx::{self, SdmxAgency, SdmxCatalogueEntry};

/// The euro-area series a reader is most likely to want, each checked against
/// the live service. Any other key still works — this is what a search can offer
/// before you know one, not a limit on what `ECO` will fetch.
static CATALOGUE: LazyLock<Vec<SdmxCatalogueEntry>> = LazyLock::new(|| {
    let rate = |id: &str, title: &str, keywords: &str| {
        SdmxCatalogueEntry::new(id, title)
            .with_units("Percent per annum")
            .with_keywords(keywords)
    };

    vec![
        rate(
            "FM/D.U2.EUR.4F.KR.MRR_FR.LEV",
            "ECB main refinancing operations rate (fixed rate)",
            "policy rate refi mro hike cut",
        )
        .with_frequency("Daily"),
        rate(
            "FM/D.U2.EUR.4F.KR.DFR.LEV",
            "ECB deposit facility rate",
            "policy rate deposit hike cut",
        )
        .with_frequency("Daily"),
        rate(
            "FM/D.U2.EUR.4F.KR.MLFR.LEV",
            "ECB marginal lending facility rate",
            "policy rate lending",
        )
        .with_frequency("Daily"),
        rate(
            "EST/B.EU000A2X2A25.WT",
            "€STR — euro short-term rate, volume-weighted trimmed mean",
            "ester overnight money market benchmark",
        )
        .with_frequency("Daily"),
        rate(
            "ICP/M.U2.N.000000.4.ANR",
            "HICP — euro area headline inflation, annual rate",
            "inflation cpi hicp prices",
        )
        .with_frequency("Monthly"),
        rate(
            "ICP/M.U2.N.XEF000.4.ANR",
            "HICP excluding energy and food — euro area core inflation, annual rate",
            "core inflation cpi hicp prices",
        )
        .with_frequency("Monthly"),
        rate(
            "BSI/M.U2.Y.V.M30.X.I.U2.2300.Z01.A",
            "M3 monetary aggregate — euro area, annual growth rate",
            "money supply monetary aggregate",
        )
        .with_frequency("Monthly"),
        SdmxCatalogueEntry::new(
            "LFSI/M.I9.S.UNEHRT.TOTAL0.15_74.T",
            "Euro area unemployment rate",
        )
        .with_units("Percent of labour force")
        .with_frequency("Monthly")
        .with_keywords("jobless labour market employment"),
        rate(
            "YC/B.U2.EUR.4F.G_N_A.SV_C_YM.SR_10Y",
            "Euro area AAA government bond spot yield, 10 years",
            "yield curve bund govvie rates 10y",
        )
        .with_frequency("Daily"),
        rate(
            "YC/B.U2.EUR.4F.G_N_A.SV_C_YM.SR_2Y",
            "Euro area AAA government bond spot yield, 2 years",
            "yield curve bund govvie rates 2y",
        )
        .with_frequency("Daily"),
        SdmxCatalogueEntry::new(
            "EXR/D.USD.EUR.SP00.A",
            "US dollar / euro reference exchange rate",
        )
        .with_units("USD per EUR")
        .with_frequency("Daily")
        .with_keywords("fx forex eurusd dollar"),
        SdmxCatalogueEntry::new(
            "EXR/D.GBP.EUR.SP00.A",
            "Pound sterling / euro reference exchange rate",
        )
        .with_units("GBP per EUR")
        .with_frequency("Daily")
        .with_keywords("fx forex eurgbp sterling"),
        SdmxCatalogueEntry::new(
            "EXR/D.JPY.EUR.SP00.A",
            "Japanese yen / euro reference exchange rate",
        )
        .with_units("JPY per EUR")
        .with_frequency("Daily")
        .with_keywords("fx forex eurjpy yen"),
        SdmxCatalogueEntry::new(
            "EXR/D.CHF.EUR.SP00.A",
            "Swiss franc / euro reference exchange rate",
        )
        .with_units("CHF per EUR")
        .with_frequency("Daily")
        .with_keywords("fx forex eurchf franc"),
        rate(
            "MIR/M.U2.B.A2C.AM.R.A.2250.EUR.N",
            "Euro area bank lending rate to households for house purchase",
            "mortgage lending rate households credit",
        )
        .with_frequency("Monthly"),
    ]
});

/// The curated euro-area series, for the shared reader and its tests.
pub fn catalogue() -> &'static [SdmxCatalogueEntry] {
    &CATALOGUE
}

pub const ECB: SdmxAgency = SdmxAgency {
    provider: DataSource::Ecb,
    agency: "the ECB",
    // The ECB reads `?format=` and ignores the Accept header; the IMF does the
    // reverse. Both are sent by the shared reader, which is one code path
    // instead of a per-agency branch.
    format: "csvdata",
    accept: "text/csv, application/vnd.sdmx.data+csv;version=1.0.0",
    // The ECB does not throttle a single reader, so one retry covers a dropped
    // connection and nothing more.
    retries: 1,
    base: |state| state.config().ecb_api_base.clone(),
    // `data.ecb.europa.eu` is the Data Portal a person reads; the SDMX service
    // this module fetches from is a different host, and linking to it would send
    // a reader to a CSV download.
    web_url: |_base, flow, key| {
        format!(
            "https://data.ecb.europa.eu/data/datasets/{}/{}",
            urlencoding::encode(flow),
            urlencoding::encode(&format!("{flow}.{key}"))
        )
    },
    catalogue,
};

/// A series and its observations, from the ECB's SDMX service.
pub async fn get_series(
    state: &AppState,
    id: &str,
    start: Option<&str>,
    end: Option<&str>,
) -> Result<DataSeriesResponse> {
    sdmx::series(state, &ECB, id, start, end).await
}

/// Rank the curated euro-area catalogue against a query. The ECB publishes no
/// free-text search endpoint; see [`sdmx::search_catalogue`].
pub async fn search(state: &AppState, query: &str, limit: usize) -> Result<Vec<DataSearchResult>> {
    let _ = state;
    sdmx::search(&ECB, query, limit)
}

/// `None` when this deployment can use the publisher; a string is the
/// operator-facing reason it cannot.
///
/// Always `None` here: the ECB's SDMX service takes no credential and rate-limits
/// nobody, so there is nothing an operator could have failed to configure.
pub fn unavailable(_state: &AppState) -> Option<String> {
    None
}
