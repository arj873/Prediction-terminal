//! CFTC — Commitments of Traders, from the Commission's public reporting
//! portal.
//!
//! Every Friday afternoon the CFTC publishes who is long and who is short each
//! US futures market, broken out by what kind of trader they are. It is the
//! only public, mandatory census of positioning that exists, and it is the
//! reason a "will gold be above X" market and a "speculators have capitulated"
//! thesis can be checked against each other rather than argued about.
//!
//! Three reports cover different market families, and they name their trader
//! categories differently because the categories genuinely differ:
//!
//! ```text
//! legacy         commercial vs non-commercial. Every market, weekly since 1986.
//! disaggregated  producers, swap dealers, managed money, other reportables.
//!                Physical commodities, since 2006.
//! financial      dealers, asset managers, leveraged funds (the TFF report).
//!                Rates, equity indices and currencies, since 2010.
//! ```
//!
//! The portal is a Socrata instance, public and keyless — verified live from
//! this container against all three datasets — so the whole history is
//! queryable and a position series is a real chart rather than a single week's
//! snapshot. What that costs is Socrata's own grammar: `$select`, `$where`,
//! `$group`, `$order` and `$limit`, with SoQL string literals single-quoted.
//!
//! The failures worth naming are the quiet ones.
//!
//! **A query that matches nothing is an HTTP 200 and an empty array.** A market
//! name misremembered by one word, a report that does not cover the market, a
//! date range before the report existed — all of them come back as `[]` with a
//! success status. Nothing here treats that as a series with no points in it.
//!
//! **A market name can contain an apostrophe, and SoQL doubles it.** The
//! Commission publishes `CRUDE OIL, LIGHT 'SWEET' - NEW YORK MERCANTILE
//! EXCHANGE`. Pasting that into a `$where` unescaped ends the literal mid-name;
//! live, that is:
//!
//! ```text
//! {"code":"query.compiler.malformed","error":true,
//!  "message":"Could not parse SoQL query \"select * where
//!             market_and_exchange_names='CRUDE OIL, LIGHT 'SWEET' - NEW YORK
//!             MERCANTILE EXCHANGE' limit 1\" at line 1 character 61 …"}
//! ```
//!
//! and doubling the quotes returns that market's rows. A syntax error is the
//! lucky outcome: a name whose stray quote still parses returns a *different*
//! market's numbers under the name the reader asked for. [`sql_literal`] is
//! SoQL's own escape, verified live against that exact market.
//!
//! **Every figure arrives as a JSON string**, including the ones that are
//! plainly numbers, and a column the report does not carry is simply absent
//! from the row. Both read as `None` and never as `0.0`: a net position of zero
//! is a market in perfect balance, which is a thing a reader would act on and
//! is not what an absent column means.
//!
//! **The Commission's own field names carry typos**, and correcting them
//! silently stops matching the column. `noncomm_postions_spread_all` in the
//! legacy report and `swap__positions_short_all` — two underscores — in the
//! disaggregated one are spelled here exactly as the portal spells them; both
//! were checked against the live column list.

use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Map, Value};
use terminal_core::dataset::DataSource;
use terminal_core::types::{DataObservation, DataSearchResult, DataSeries, DataSeriesResponse};

use crate::app::AppState;
use crate::cache::ttl;
use crate::error::{Result, UpstreamError};
use crate::http::FetchOptions;

/// A single attempt's budget. A full history query is a grouped scan over two
/// million rows and the portal is not quick about it.
const TIMEOUT: Duration = Duration::from_secs(30);

/// Attempts after the first.
const RETRIES: u32 = 1;

/// Rows one series request asks for. Every market's full weekly history since
/// 1986 is under 2,100 rows, so this is a ceiling rather than a page.
const HISTORY_LIMIT: usize = 5_000;

/// Socrata's own ceiling on a single response. Asking for more is refused
/// rather than truncated.
const MAX_LIMIT: usize = 200;

/// The Commission's page for the report, so a reader can check a figure against
/// the published tables.
const SOURCE_URL: &str = "https://www.cftc.gov/MarketReports/CommitmentsofTraders/index.htm";

/// The column every report dates its rows with. A Socrata floating timestamp:
/// `2026-08-18T00:00:00.000`, never a bare date.
const DATE_COLUMN: &str = "report_date_as_yyyy_mm_dd";

/// The column every report names its market with, exchange and all.
const NAME_COLUMN: &str = "market_and_exchange_names";

/// The Commission's stable identity for a market, behind the name.
const CODE_COLUMN: &str = "cftc_contract_market_code";

const OPEN_INTEREST_COLUMN: &str = "open_interest_all";

/* ----------------------------------------------------------------- report */

/// One trader category, as the columns that describe it in a report's rows.
///
/// A `None` field is a figure that report does not publish for that category —
/// the legacy report breaks out spreading for non-commercials only, and states
/// no trader count for the non-reportable bucket, because a non-reportable
/// trader is by definition not counted.
#[derive(Debug, Clone, Copy)]
pub struct CategorySpec {
    /// Short key used in a series id, e.g. `noncomm`.
    pub key: &'static str,
    pub name: &'static str,
    pub long: &'static str,
    pub short: &'static str,
    pub spread: Option<&'static str>,
    pub change_long: Option<&'static str>,
    pub change_short: Option<&'static str>,
    pub pct_long: Option<&'static str>,
    pub pct_short: Option<&'static str>,
    pub traders: Option<&'static str>,
}

/// One of the three reports.
#[derive(Debug, Clone, Copy)]
pub struct ReportSpec {
    pub id: &'static str,
    pub label: &'static str,
    /// Socrata dataset identifier — the four-and-four handle in the URL.
    pub dataset: &'static str,
    pub covers: &'static str,
    pub categories: &'static [CategorySpec],
}

const LEGACY_CATEGORIES: &[CategorySpec] = &[
    CategorySpec {
        key: "noncomm",
        name: "Non-commercial",
        long: "noncomm_positions_long_all",
        short: "noncomm_positions_short_all",
        // The Commission's own column name carries this typo — `postions`.
        // Correcting it here would simply stop matching the column, and a
        // column that does not match reads as a category with no spreading.
        spread: Some("noncomm_postions_spread_all"),
        change_long: Some("change_in_noncomm_long_all"),
        change_short: Some("change_in_noncomm_short_all"),
        pct_long: Some("pct_of_oi_noncomm_long_all"),
        pct_short: Some("pct_of_oi_noncomm_short_all"),
        traders: Some("traders_noncomm_long_all"),
    },
    CategorySpec {
        key: "comm",
        name: "Commercial",
        long: "comm_positions_long_all",
        short: "comm_positions_short_all",
        spread: None,
        change_long: Some("change_in_comm_long_all"),
        change_short: Some("change_in_comm_short_all"),
        pct_long: Some("pct_of_oi_comm_long_all"),
        pct_short: Some("pct_of_oi_comm_short_all"),
        traders: Some("traders_comm_long_all"),
    },
    CategorySpec {
        key: "nonrept",
        name: "Non-reportable",
        long: "nonrept_positions_long_all",
        short: "nonrept_positions_short_all",
        spread: None,
        change_long: Some("change_in_nonrept_long_all"),
        change_short: Some("change_in_nonrept_short_all"),
        pct_long: Some("pct_of_oi_nonrept_long_all"),
        pct_short: Some("pct_of_oi_nonrept_short_all"),
        // A non-reportable trader is one below the reporting threshold, so the
        // Commission does not count them.
        traders: None,
    },
];

const DISAGGREGATED_CATEGORIES: &[CategorySpec] = &[
    CategorySpec {
        key: "prod",
        name: "Producer / merchant / processor / user",
        long: "prod_merc_positions_long",
        short: "prod_merc_positions_short",
        // A producer hedging its own output has nothing to spread.
        spread: None,
        change_long: Some("change_in_prod_merc_long"),
        change_short: Some("change_in_prod_merc_short"),
        pct_long: Some("pct_of_oi_prod_merc_long"),
        pct_short: Some("pct_of_oi_prod_merc_short"),
        traders: Some("traders_prod_merc_long_all"),
    },
    CategorySpec {
        key: "swap",
        name: "Swap dealer",
        long: "swap_positions_long_all",
        // Two underscores, in the Commission's own schema, on the short and
        // spread columns but not the long one.
        short: "swap__positions_short_all",
        spread: Some("swap__positions_spread_all"),
        change_long: Some("change_in_swap_long_all"),
        change_short: Some("change_in_swap_short_all"),
        pct_long: Some("pct_of_oi_swap_long_all"),
        pct_short: Some("pct_of_oi_swap_short_all"),
        traders: Some("traders_swap_long_all"),
    },
    CategorySpec {
        key: "mmoney",
        name: "Managed money",
        long: "m_money_positions_long_all",
        short: "m_money_positions_short_all",
        spread: Some("m_money_positions_spread"),
        change_long: Some("change_in_m_money_long_all"),
        change_short: Some("change_in_m_money_short_all"),
        pct_long: Some("pct_of_oi_m_money_long_all"),
        pct_short: Some("pct_of_oi_m_money_short_all"),
        traders: Some("traders_m_money_long_all"),
    },
    CategorySpec {
        key: "other",
        name: "Other reportable",
        long: "other_rept_positions_long",
        short: "other_rept_positions_short",
        spread: Some("other_rept_positions_spread"),
        change_long: Some("change_in_other_rept_long"),
        change_short: Some("change_in_other_rept_short"),
        pct_long: Some("pct_of_oi_other_rept_long"),
        pct_short: Some("pct_of_oi_other_rept_short"),
        traders: Some("traders_other_rept_long_all"),
    },
    CategorySpec {
        key: "nonrept",
        name: "Non-reportable",
        long: "nonrept_positions_long_all",
        short: "nonrept_positions_short_all",
        spread: None,
        change_long: Some("change_in_nonrept_long_all"),
        change_short: Some("change_in_nonrept_short_all"),
        pct_long: Some("pct_of_oi_nonrept_long_all"),
        pct_short: Some("pct_of_oi_nonrept_short_all"),
        traders: None,
    },
];

const FINANCIAL_CATEGORIES: &[CategorySpec] = &[
    CategorySpec {
        key: "dealer",
        name: "Dealer / intermediary",
        long: "dealer_positions_long_all",
        short: "dealer_positions_short_all",
        spread: Some("dealer_positions_spread_all"),
        change_long: Some("change_in_dealer_long_all"),
        change_short: Some("change_in_dealer_short_all"),
        pct_long: Some("pct_of_oi_dealer_long_all"),
        pct_short: Some("pct_of_oi_dealer_short_all"),
        traders: Some("traders_dealer_long_all"),
    },
    CategorySpec {
        key: "assetmgr",
        name: "Asset manager / institutional",
        long: "asset_mgr_positions_long",
        short: "asset_mgr_positions_short",
        spread: Some("asset_mgr_positions_spread"),
        change_long: Some("change_in_asset_mgr_long"),
        change_short: Some("change_in_asset_mgr_short"),
        pct_long: Some("pct_of_oi_asset_mgr_long"),
        pct_short: Some("pct_of_oi_asset_mgr_short"),
        traders: Some("traders_asset_mgr_long_all"),
    },
    CategorySpec {
        key: "levfund",
        name: "Leveraged funds",
        long: "lev_money_positions_long",
        short: "lev_money_positions_short",
        spread: Some("lev_money_positions_spread"),
        change_long: Some("change_in_lev_money_long"),
        change_short: Some("change_in_lev_money_short"),
        pct_long: Some("pct_of_oi_lev_money_long"),
        pct_short: Some("pct_of_oi_lev_money_short"),
        traders: Some("traders_lev_money_long_all"),
    },
    CategorySpec {
        key: "other",
        name: "Other reportable",
        long: "other_rept_positions_long",
        short: "other_rept_positions_short",
        spread: Some("other_rept_positions_spread"),
        change_long: Some("change_in_other_rept_long"),
        change_short: Some("change_in_other_rept_short"),
        pct_long: Some("pct_of_oi_other_rept_long"),
        pct_short: Some("pct_of_oi_other_rept_short"),
        traders: Some("traders_other_rept_long_all"),
    },
    CategorySpec {
        key: "nonrept",
        name: "Non-reportable",
        long: "nonrept_positions_long_all",
        short: "nonrept_positions_short_all",
        spread: None,
        change_long: Some("change_in_nonrept_long_all"),
        change_short: Some("change_in_nonrept_short_all"),
        pct_long: Some("pct_of_oi_nonrept_long_all"),
        pct_short: Some("pct_of_oi_nonrept_short_all"),
        traders: None,
    },
];

/// The three reports. Every column name below was checked against the live
/// schema of its dataset; the typos are the Commission's.
pub const REPORTS: &[ReportSpec] = &[
    ReportSpec {
        id: "legacy",
        label: "Legacy — commercial vs non-commercial",
        dataset: "6dca-aqww",
        covers: "every futures market, weekly since 1986",
        categories: LEGACY_CATEGORIES,
    },
    ReportSpec {
        id: "disaggregated",
        label: "Disaggregated — producers, swaps, managed money",
        dataset: "72hh-3qpy",
        covers: "physical commodities, weekly since 2006",
        categories: DISAGGREGATED_CATEGORIES,
    },
    ReportSpec {
        id: "financial",
        label: "Traders in Financial Futures — dealers, asset managers, leveraged funds",
        dataset: "gpe5-46if",
        covers: "rates, equity indices and currencies, weekly since 2010",
        categories: FINANCIAL_CATEGORIES,
    },
];

/// What an id that names no report gets, which is the legacy one: it is the
/// only report that covers every market and the only one with history before
/// 2006.
const DEFAULT_REPORT: &str = "legacy";

/// The report `id` names, by its own name or by what a reader would call it.
///
/// `tff` and `disagg` are what the Commission's own publications call two of
/// them, so a reader who has read one of those will type them.
pub fn report_spec(id: &str) -> Result<&'static ReportSpec> {
    let key = id.trim().to_lowercase();
    let found = REPORTS.iter().find(|report| {
        report.id == key
            || (key == "tff" && report.id == "financial")
            || (key == "disagg" && report.id == "disaggregated")
    });

    found.ok_or_else(|| {
        UpstreamError::bad_request(format!("\"{id}\" is not a Commitments of Traders report"))
            .with_hint(format!("Reports are {}.", report_ids()))
    })
}

fn report_ids() -> String {
    REPORTS
        .iter()
        .map(|report| report.id)
        .collect::<Vec<_>>()
        .join(", ")
}

/// The reports and what each covers, for a board that lists them.
pub fn list_reports() -> Vec<(&'static str, &'static str, &'static str)> {
    REPORTS
        .iter()
        .map(|report| (report.id, report.label, report.covers))
        .collect()
}

/* -------------------------------------------------------------- the wire */

/// One Socrata row.
///
/// Kept as a map rather than a struct because the columns differ per report and
/// per `$select`, and because the whole point of the category specs above is
/// that the column a figure lives in is data, not a field name.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(transparent)]
struct RawCftcRow(Map<String, Value>);

impl RawCftcRow {
    fn text(&self, field: &str) -> &str {
        self.0.get(field).and_then(Value::as_str).unwrap_or("")
    }

    /// A figure, or `None` where the report does not state one.
    ///
    /// Socrata sends every column of these datasets as a JSON string, including
    /// the counts; a number is accepted too, in case a column is ever retyped.
    /// An absent column, an empty cell and anything unparseable are all `None`
    /// rather than `0.0` — a net position of zero is a market in perfect
    /// balance, and that is not what a missing column means.
    fn number(&self, field: Option<&str>) -> Option<f64> {
        match self.0.get(field?)? {
            Value::String(text) => text
                .trim()
                .parse::<f64>()
                .ok()
                .filter(|value| value.is_finite()),
            Value::Number(number) => number.as_f64().filter(|value| value.is_finite()),
            _ => None,
        }
    }

    /// The report date as `YYYY-MM-DD`.
    ///
    /// The column is a floating timestamp — `2026-08-18T00:00:00.000` — and the
    /// midnight is noise: the reading is as of the close of that Tuesday.
    fn report_date(&self) -> Option<String> {
        let raw = self.text(DATE_COLUMN);
        (raw.len() >= 10).then(|| raw[..10].to_owned())
    }
}

/// A SoQL string literal.
///
/// Single-quoted, with an embedded quote doubled — SoQL's own escape. The
/// Commission publishes `CRUDE OIL, LIGHT 'SWEET' - NEW YORK MERCANTILE
/// EXCHANGE`, so this is load-bearing rather than defensive: unescaped, that
/// name ends its literal after `LIGHT ` and the rest of it is read as SoQL.
fn sql_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

/// The request URL for one Socrata query.
///
/// Parameter *names* are passed through unencoded, because Socrata's are
/// `$select`, `$where` and friends, and every one of them is a literal written
/// in this module rather than anything a reader supplies. Values are encoded,
/// because they carry market names, spaces and quotes.
fn socrata_url(base: &str, dataset: &str, params: &[(&str, String)]) -> String {
    let query = params
        .iter()
        .map(|(key, value)| format!("{key}={}", urlencoding::encode(value)))
        .collect::<Vec<_>>()
        .join("&");
    format!("{}/{dataset}.json?{query}", base.trim_end_matches('/'))
}

/// Ask the portal, through the cache.
///
/// Keyed on the whole query rather than on a caller-shaped summary of it: two
/// panels asking the same question is the common case, and two panels asking
/// subtly different ones must not share an answer.
async fn query(
    state: &AppState,
    dataset: &str,
    params: &[(&str, String)],
    ttl: Duration,
) -> Result<Arc<Vec<RawCftcRow>>> {
    let url = socrata_url(&state.config().cftc_api_base, dataset, params);
    let cache_key = format!("cftc:{dataset}:{}", params_key(params));

    state
        .cache()
        .cached(&cache_key, ttl, || async {
            state
                .http()
                .fetch_json::<Vec<RawCftcRow>>(
                    &url,
                    FetchOptions::new().timeout(TIMEOUT).retries(RETRIES),
                )
                .await
        })
        .await
}

fn params_key(params: &[(&str, String)]) -> String {
    params
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join("&")
}

/* --------------------------------------------------------------- markets */

/// One market, as the portal's own group-by describes it.
#[derive(Debug, Clone, PartialEq)]
pub struct CotMarket {
    /// The Commission's stable identity for the contract. Six upper-case
    /// alphanumerics, always carrying at least one digit — `088691`, `0063A1`,
    /// `ZB9105`.
    pub contract_code: String,
    /// The name exactly as the Commission publishes it, exchange and all. This
    /// is the only string a `$where` on the market will match.
    pub name: String,
    /// The name with the exchange split off, for a reader.
    pub market: String,
    pub exchange: String,
    /// The most recent report date for this market. A market that stopped
    /// trading in 2007 still has rows, and this is how a reader tells.
    pub latest: String,
    /// The highest open interest this market has ever reported — a `max()` over
    /// the whole group, not the current week's figure. Useful for ranking a
    /// search, misleading if read as today's.
    pub peak_open_interest: Option<f64>,
}

/// Split `GOLD - COMMODITY EXCHANGE INC.` into market and exchange.
///
/// At the *last* ` - `, not the first. Forty of the legacy report's 1,327
/// market names carry more than one — `COLORADO INTERSTATE - MAINLINE (BASIS) -
/// ICE FUTURES ENERGY DIV` — and the exchange is the trailing segment in every
/// one of them. Splitting at the first, as the TypeScript this replaces did,
/// files that market on an exchange called `MAINLINE (BASIS) - ICE FUTURES
/// ENERGY DIV`. A name with no separator at all is all market and no exchange;
/// the legacy report has one of those too.
fn split_name(full: &str) -> (String, String) {
    match full.rfind(" - ") {
        Some(at) => (
            full[..at].trim().to_owned(),
            full[at + 3..].trim().to_owned(),
        ),
        None => (full.trim().to_owned(), String::new()),
    }
}

/// A contract market code, as opposed to a name to search for.
///
/// Every code in all three datasets is exactly six upper-case alphanumerics and
/// every one carries at least one digit — checked against the full distinct
/// list of all three. The digit is what keeps `SILVER` a name to search for
/// while `088691` and `0063A1` are identities to look up. The TypeScript this
/// replaces required all six characters to be digits, which meant the ids its
/// own search produced for a market like `GOLD -1 TROY OUNCE - COINBASE
/// DERIVATIVES, LLC` (code `088LM1`) could not then be resolved.
fn is_contract_code(value: &str) -> bool {
    value.len() == 6
        && value.chars().all(|c| c.is_ascii_alphanumeric())
        && value.chars().any(|c| c.is_ascii_digit())
}

/// Markets whose name contains every word of `search`.
///
/// Matching on all words rather than the whole phrase is what makes `COT wheat
/// chicago` work: the Commission writes the name as `WHEAT-SRW - CHICAGO BOARD
/// OF TRADE`, which contains both words and not the phrase. Verified live —
/// `like '%WHEAT CHICAGO%'` matches nothing at all.
///
/// Ordered by the latest report date, so a market that still trades outranks
/// one that stopped in 2007 no matter how well the name matches.
pub async fn find_markets(
    state: &AppState,
    search: &str,
    report: &str,
    limit: usize,
) -> Result<Vec<CotMarket>> {
    let spec = report_spec(report)?;
    let where_clause = name_filter(search);

    let mut params: Vec<(&str, String)> = vec![
        (
            "$select",
            format!(
                "{NAME_COLUMN},{CODE_COLUMN},max({DATE_COLUMN}) as latest,\
                 max({OPEN_INTEREST_COLUMN}) as oi"
            ),
        ),
        ("$group", format!("{NAME_COLUMN},{CODE_COLUMN}")),
        ("$order", "latest DESC".to_owned()),
        ("$limit", limit.clamp(1, MAX_LIMIT).to_string()),
    ];
    if !where_clause.is_empty() {
        params.push(("$where", where_clause));
    }

    let rows = query(state, spec.dataset, &params, ttl::CATALOGUE).await?;

    Ok(rows
        .iter()
        .map(|row| {
            let name = row.text(NAME_COLUMN).to_owned();
            let (market, exchange) = split_name(&name);
            CotMarket {
                contract_code: row.text(CODE_COLUMN).trim().to_owned(),
                name,
                market,
                exchange,
                latest: row.text("latest").get(..10).unwrap_or_default().to_owned(),
                peak_open_interest: row.number(Some("oi")),
            }
        })
        .collect())
}

/// The `$where` matching every word of `search`, or `""` for no words.
fn name_filter(search: &str) -> String {
    search
        .split_whitespace()
        .map(|word| {
            format!(
                "upper({NAME_COLUMN}) like {}",
                sql_literal(&format!("%{}%", word.to_uppercase()))
            )
        })
        .collect::<Vec<_>>()
        .join(" AND ")
}

/// Resolve what a reader typed to one market's full published name.
///
/// A contract market code is unambiguous and is looked up directly; anything
/// else is a name search whose best hit is the most recently reported market
/// that matched.
async fn resolve_market(state: &AppState, spec: &ReportSpec, market: &str) -> Result<String> {
    let trimmed = market.trim();
    if trimmed.is_empty() {
        return Err(
            UpstreamError::bad_request("A CFTC series id has to name a market").with_hint(
                "Ids are report/market/field, e.g. legacy/GOLD/noncomm_net or \
                 legacy/088691/oi.",
            ),
        );
    }

    let upper = trimmed.to_uppercase();
    if is_contract_code(&upper) {
        let rows = query(
            state,
            spec.dataset,
            &[
                ("$select", NAME_COLUMN.to_owned()),
                ("$where", format!("{CODE_COLUMN}={}", sql_literal(&upper))),
                ("$order", format!("{DATE_COLUMN} DESC")),
                ("$limit", "1".to_owned()),
            ],
            ttl::CATALOGUE,
        )
        .await?;

        if let Some(name) = rows.first().map(|row| row.text(NAME_COLUMN)) {
            if !name.is_empty() {
                return Ok(name.to_owned());
            }
        }
        // Falls through to a name search rather than failing: a six-character
        // string with a digit in it can also be somebody typing a ticker.
    }

    let markets = find_markets(state, trimmed, spec.id, 1).await?;
    markets
        .into_iter()
        .next()
        .map(|best| best.name)
        .ok_or_else(|| {
            UpstreamError::not_found(format!("No {} COT market matches \"{market}\"", spec.id))
                .with_hint(format!(
                    "Try a shorter name — `COT gold`, `COT e-mini s&p`, `COT crude` — or another \
                 report ({}).",
                    report_ids()
                ))
        })
}

/* ---------------------------------------------------------------- report */

/// One trader category's line in a published report.
#[derive(Debug, Clone, PartialEq)]
pub struct CotCategory {
    pub name: &'static str,
    pub long: Option<f64>,
    pub short: Option<f64>,
    /// Spreading contracts, where the report breaks them out.
    pub spreading: Option<f64>,
    /// `long - short`, and `None` unless both sides are stated.
    pub net: Option<f64>,
    /// Week-on-week change in [`CotCategory::net`].
    pub net_change: Option<f64>,
    /// Share of open interest held long, 0..100.
    pub percent_long: Option<f64>,
    pub percent_short: Option<f64>,
    pub trader_count: Option<f64>,
}

/// The latest published report for one market.
#[derive(Debug, Clone, PartialEq)]
pub struct CotReport {
    pub report: &'static str,
    pub report_label: &'static str,
    pub market: String,
    pub exchange: String,
    pub contract_code: String,
    /// The report Tuesday, `YYYY-MM-DD`.
    pub date: String,
    pub open_interest: Option<f64>,
    pub open_interest_change: Option<f64>,
    pub categories: Vec<CotCategory>,
    pub source_url: &'static str,
}

/// Read one category out of a report row.
fn read_category(row: &RawCftcRow, spec: &CategorySpec) -> CotCategory {
    let long = row.number(Some(spec.long));
    let short = row.number(Some(spec.short));
    let change_long = row.number(spec.change_long);
    let change_short = row.number(spec.change_short);

    CotCategory {
        name: spec.name,
        long,
        short,
        spreading: row.number(spec.spread),
        // Both sides or neither: a net computed from one stated side and one
        // missing one would be that side's gross, dressed as a net.
        net: long.zip(short).map(|(long, short)| long - short),
        net_change: change_long
            .zip(change_short)
            .map(|(long, short)| long - short),
        percent_long: row.number(spec.pct_long),
        percent_short: row.number(spec.pct_short),
        trader_count: row.number(spec.traders),
    }
}

/// The latest published report for a market, with every category broken out.
pub async fn get_report(state: &AppState, market: &str, report: &str) -> Result<CotReport> {
    let spec = report_spec(report)?;
    let name = resolve_market(state, spec, market).await?;

    let rows = query(
        state,
        spec.dataset,
        &[
            ("$where", format!("{NAME_COLUMN}={}", sql_literal(&name))),
            ("$order", format!("{DATE_COLUMN} DESC")),
            ("$limit", "1".to_owned()),
        ],
        ttl::FRED,
    )
    .await?;

    let row = rows.first().ok_or_else(|| {
        UpstreamError::not_found(format!(
            "The CFTC has published no {} report for {name}",
            spec.id
        ))
        .with_hint(format!(
            "The market exists but this report does not cover it. Reports are {}.",
            report_ids()
        ))
    })?;

    let (market_name, exchange) = split_name(&name);

    Ok(CotReport {
        report: spec.id,
        report_label: spec.label,
        market: market_name,
        exchange,
        contract_code: row.text(CODE_COLUMN).trim().to_owned(),
        date: row.report_date().unwrap_or_default(),
        open_interest: row.number(Some(OPEN_INTEREST_COLUMN)),
        open_interest_change: row.number(Some("change_in_open_interest_all")),
        categories: spec
            .categories
            .iter()
            .map(|category| read_category(row, category))
            .collect(),
        source_url: SOURCE_URL,
    })
}

/* ---------------------------------------------------------------- series */

/// Which figure of a category a series id asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Measure {
    Net,
    Long,
    Short,
}

impl Measure {
    fn label(self) -> &'static str {
        match self {
            Self::Net => "net position",
            Self::Long => "long",
            Self::Short => "short",
        }
    }
}

/// `noncomm_net` → the category and which figure of it.
///
/// A field with no recognised suffix is that category's net, because the net is
/// what a positioning series means when nobody says otherwise.
fn split_field(field: &str) -> (String, Measure) {
    let lower = field.to_lowercase();
    match lower.rsplit_once('_') {
        Some((category, "net")) => (category.to_owned(), Measure::Net),
        Some((category, "long")) => (category.to_owned(), Measure::Long),
        Some((category, "short")) => (category.to_owned(), Measure::Short),
        _ => (lower, Measure::Net),
    }
}

/// Whether a field names open interest rather than a category.
fn is_open_interest(field: &str) -> bool {
    matches!(field, "oi" | "open_interest")
}

/// The category `key` names, by its short key or by the start of its name.
fn find_category(spec: &ReportSpec, key: &str) -> Option<&'static CategorySpec> {
    spec.categories
        .iter()
        .find(|category| category.key == key || category.name.to_lowercase().starts_with(key))
}

/// The parts of `report/market/field`, with the market's own slashes kept.
fn split_id(raw_id: &str) -> Vec<&str> {
    raw_id
        .split('/')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect()
}

fn bad_id(raw_id: &str) -> UpstreamError {
    UpstreamError::bad_request(format!("\"{raw_id}\" does not name a CFTC series")).with_hint(
        "Ids are report/market/field, e.g. legacy/GOLD/noncomm_net, \
         financial/E-MINI S&P 500/levfund_net, or legacy/CRUDE OIL/oi.",
    )
}

/// Positioning over time: `report/market/field`.
///
/// `legacy/GOLD/noncomm_net` is the classic speculative-positioning series. The
/// open interest itself is `report/market/oi`.
pub async fn get_series(
    state: &AppState,
    raw_id: &str,
    start: Option<&str>,
    end: Option<&str>,
) -> Result<DataSeriesResponse> {
    let parts = split_id(raw_id);
    // Three parts, not two: the TypeScript this replaces read `legacy/GOLD` as
    // a field called GOLD against an unnamed market, and asked the portal for
    // whichever market had reported most recently. A missing market is a typo,
    // and is cheaper to say so than to answer with a different market's numbers.
    if parts.len() < 3 {
        return Err(bad_id(raw_id));
    }

    let spec = report_spec(parts[0])?;
    let field = parts[parts.len() - 1].to_lowercase();
    let market = parts[1..parts.len() - 1].join("/");

    let open_interest = is_open_interest(&field);
    let (category_key, measure) = split_field(&field);
    let category = if open_interest {
        None
    } else {
        // Checked before the market is resolved, so a mistyped field costs no
        // request: which categories a report has is knowable here.
        Some(find_category(spec, &category_key).ok_or_else(|| {
            UpstreamError::bad_request(format!(
                "The {} report has no \"{category_key}\" category",
                spec.id
            ))
            .with_hint(format!(
                "Categories are {}, each with _net, _long or _short — plus `oi` for open \
                 interest.",
                spec.categories
                    .iter()
                    .map(|category| category.key)
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
        })?)
    };

    let name = resolve_market(state, spec, &market).await?;

    let mut clauses = vec![format!("{NAME_COLUMN}={}", sql_literal(&name))];
    if let Some(start) = start {
        clauses.push(format!(
            "{DATE_COLUMN} >= {}",
            sql_literal(&format!("{start}T00:00:00"))
        ));
    }
    if let Some(end) = end {
        clauses.push(format!(
            "{DATE_COLUMN} <= {}",
            sql_literal(&format!("{end}T23:59:59"))
        ));
    }

    let select = match category {
        Some(category) => format!("{DATE_COLUMN},{},{}", category.long, category.short),
        None => format!("{DATE_COLUMN},{OPEN_INTEREST_COLUMN}"),
    };

    let rows = query(
        state,
        spec.dataset,
        &[
            ("$select", select),
            ("$where", clauses.join(" AND ")),
            ("$order", format!("{DATE_COLUMN} ASC")),
            ("$limit", HISTORY_LIMIT.to_string()),
        ],
        ttl::FRED,
    )
    .await?;

    let observations: Vec<DataObservation> = rows
        .iter()
        .filter_map(|row| {
            // A row the terminal cannot date is dropped rather than dated to
            // the empty string, which would sort ahead of 1986 and draw a point
            // off the left edge of every chart.
            let date = row.report_date()?;
            let value = match category {
                None => row.number(Some(OPEN_INTEREST_COLUMN)),
                Some(category) => {
                    let long = row.number(Some(category.long));
                    let short = row.number(Some(category.short));
                    match measure {
                        Measure::Long => long,
                        Measure::Short => short,
                        Measure::Net => long.zip(short).map(|(long, short)| long - short),
                    }
                }
            };
            Some(DataObservation { date, value })
        })
        .collect();

    if observations.is_empty() {
        // An empty array with an HTTP 200 is what a market this report does not
        // cover, and a range before the report existed, both look like.
        return Err(UpstreamError::not_found(format!(
            "The CFTC has no {} history for {name}",
            spec.id
        ))
        .with_hint(format!(
            "The {} report covers {}. Try another report ({}).",
            spec.id,
            spec.covers,
            report_ids()
        )));
    }

    let label = match category {
        None => "open interest".to_owned(),
        Some(category) => format!("{} {}", category.name, measure.label()),
    };

    Ok(DataSeriesResponse {
        series: DataSeries {
            provider: DataSource::Cftc,
            id: raw_id.to_owned(),
            title: format!("{name} — {label}"),
            units: "Contracts".to_owned(),
            units_short: "Contracts".to_owned(),
            frequency: "Weekly (Tuesday)".to_owned(),
            seasonal_adjustment: String::new(),
            last_updated: observations
                .last()
                .map(|point| point.date.clone())
                .unwrap_or_default(),
            observation_start: observations
                .first()
                .map(|point| point.date.clone())
                .unwrap_or_default(),
            observation_end: observations
                .last()
                .map(|point| point.date.clone())
                .unwrap_or_default(),
            notes: format!(
                "{}. Positions are as of the report Tuesday and published the following Friday.",
                spec.label
            ),
            // Which of the three reports answered. The TypeScript reported the
            // Socrata dataset handle here; `6dca-aqww` in a panel header tells
            // a reader nothing, and the report name is the arm that answered.
            source: spec.id.to_owned(),
            source_url: SOURCE_URL.to_owned(),
        },
        observations,
    })
}

/* ---------------------------------------------------------------- search */

/// The positioning series people actually chart, per matching market.
///
/// Unlike the other publishers this one *is* a live search — the portal indexes
/// every market name — so the rows are generated from the reader's own query
/// rather than curated in advance. Two per market, because the net speculative
/// position and the open interest are what a reader wants from a market they
/// have just found, and the id of each is a thing they can paste into `ECO`.
pub async fn search(state: &AppState, query: &str, limit: usize) -> Result<Vec<DataSearchResult>> {
    let query = query.trim();
    if query.is_empty() || limit == 0 {
        return Ok(Vec::new());
    }

    let wanted = limit.div_ceil(2);
    let markets = find_markets(state, query, DEFAULT_REPORT, wanted).await?;

    Ok(markets
        .into_iter()
        .take(wanted)
        .flat_map(|market| {
            // Keyed by contract code rather than by name: the code is stable,
            // unambiguous and short, where a name carries commas, quotes and
            // the exchange.
            let label = if market.exchange.is_empty() {
                market.market.clone()
            } else {
                format!("{} ({})", market.market, market.exchange)
            };
            [
                DataSearchResult {
                    provider: DataSource::Cftc,
                    source: Some(DEFAULT_REPORT.to_owned()),
                    id: format!("legacy/{}/noncomm_net", market.contract_code),
                    title: format!("{label} — non-commercial net position"),
                    units: Some("Contracts".to_owned()),
                    frequency: Some("Weekly".to_owned()),
                    seasonal_adjustment: None,
                    observation_range: (!market.latest.is_empty())
                        .then(|| format!("to {}", market.latest)),
                },
                DataSearchResult {
                    provider: DataSource::Cftc,
                    source: Some(DEFAULT_REPORT.to_owned()),
                    id: format!("legacy/{}/oi", market.contract_code),
                    title: format!("{label} — open interest"),
                    units: Some("Contracts".to_owned()),
                    frequency: Some("Weekly".to_owned()),
                    seasonal_adjustment: None,
                    observation_range: (!market.latest.is_empty())
                        .then(|| format!("to {}", market.latest)),
                },
            ]
        })
        .collect())
}

/// `None` when this deployment can use the publisher; a string is the
/// operator-facing reason it cannot.
///
/// Always `None`. The portal is public and keyless — verified live against all
/// three datasets from this container, with no credential of any kind.
pub fn unavailable(_state: &AppState) -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    //! CFTC parser and wire tests.
    //!
    //! Every fixture here was captured live from `publicreporting.cftc.gov`
    //! with curl, and each was fetched with an explicit `$select` — the same
    //! columns this module asks for — which is why a report row has 26 fields
    //! rather than the 130 the dataset holds. That is a projection of a real
    //! answer, not a hand-built one, and every figure asserted below is read
    //! off the file that ships beside this module.

    use super::*;
    use serde_json::json;
    use wiremock::matchers::{method, path as path_matcher, query_param};
    use wiremock::{Mock, MockServer, Request, ResponseTemplate};

    use crate::config::Config;

    /// The latest legacy report for `GOLD - COMMODITY EXCHANGE INC.`, one row,
    /// as of the report Tuesday 2026-08-18. Kept because it is the report a
    /// reader is most likely to ask for and because it carries the Commission's
    /// `noncomm_postions_spread_all` typo in a real payload.
    const GOLD_REPORT_JSON: &str = include_str!("fixtures/cftc_legacy_gold.json");

    /// Five weekly rows of gold non-commercial longs and shorts, 2026-07-21 to
    /// 2026-08-18 — the tail of a series that goes back to 1986, trimmed to
    /// what a test can assert in full.
    const GOLD_HISTORY_JSON: &str = include_str!("fixtures/cftc_gold_history.json");

    /// The grouped market search for `wheat chicago`, three rows. Kept for the
    /// ordering: two markets that still report and one that stopped in 2022,
    /// which is what `$order=latest DESC` exists to separate.
    const WHEAT_MARKETS_JSON: &str = include_str!("fixtures/cftc_markets_wheat.json");

    const LEGACY: &str = "/6dca-aqww.json";
    const GOLD: &str = "GOLD - COMMODITY EXCHANGE INC.";

    fn state_for(server: &MockServer) -> AppState {
        AppState::new(Config {
            cftc_api_base: server.uri(),
            ..Config::default()
        })
    }

    fn fixture(text: &str) -> serde_json::Value {
        serde_json::from_str(text).expect("the captured fixture parses")
    }

    /// `(date, value)` pairs, since the wire type carries no `PartialEq`.
    fn rows(observations: &[DataObservation]) -> Vec<(&str, Option<f64>)> {
        observations
            .iter()
            .map(|observation| (observation.date.as_str(), observation.value))
            .collect()
    }

    fn where_of(request: &Request) -> String {
        request
            .url
            .query_pairs()
            .find(|(key, _)| key == "$where")
            .map(|(_, value)| value.into_owned())
            .unwrap_or_default()
    }

    /// A one-row answer naming the market, which is what a code lookup gets.
    fn name_only(name: &str) -> serde_json::Value {
        json!([{ "market_and_exchange_names": name }])
    }

    /* ---------------------------------------------------------- SoQL literals */

    #[test]
    fn a_quote_in_a_market_name_is_doubled_rather_than_ending_the_literal() {
        // `CRUDE OIL, LIGHT 'SWEET' - NEW YORK MERCANTILE EXCHANGE` is a real
        // market. Unescaped, its literal ends after `LIGHT ` and the rest is
        // read as SoQL.
        assert_eq!(
            sql_literal("CRUDE OIL, LIGHT 'SWEET' - NEW YORK MERCANTILE EXCHANGE"),
            "'CRUDE OIL, LIGHT ''SWEET'' - NEW YORK MERCANTILE EXCHANGE'"
        );
        assert_eq!(sql_literal("GOLD"), "'GOLD'");
        assert_eq!(sql_literal(""), "''");
        // And a reader who types nothing but quotes gets a literal, not a query.
        assert_eq!(sql_literal("' OR 1=1 --"), "''' OR 1=1 --'");
    }

    #[test]
    fn every_word_becomes_its_own_like_so_the_words_can_be_apart_in_the_name() {
        // `%WHEAT CHICAGO%` matches nothing live; two `like`s match
        // `WHEAT-SRW - CHICAGO BOARD OF TRADE`.
        assert_eq!(
            name_filter("wheat chicago"),
            "upper(market_and_exchange_names) like '%WHEAT%' AND \
             upper(market_and_exchange_names) like '%CHICAGO%'"
        );
        assert_eq!(name_filter("  "), "");
    }

    #[test]
    fn a_socrata_url_encodes_its_values_and_leaves_its_parameter_names_alone() {
        let url = socrata_url(
            "https://cftc.test/resource",
            "6dca-aqww",
            &[("$where", format!("{NAME_COLUMN}={}", sql_literal(GOLD)))],
        );
        assert!(url.starts_with("https://cftc.test/resource/6dca-aqww.json?$where="));
        assert!(
            url.contains("%27GOLD%20-%20COMMODITY%20EXCHANGE%20INC.%27"),
            "{url}"
        );
    }

    /* ----------------------------------------------------------- the id shape */

    #[test]
    fn reads_a_report_by_its_name_or_by_what_a_reader_calls_it() {
        assert_eq!(
            report_spec("legacy").expect("a report").dataset,
            "6dca-aqww"
        );
        assert_eq!(report_spec(" LEGACY ").expect("a report").id, "legacy");
        assert_eq!(report_spec("tff").expect("a report").id, "financial");
        assert_eq!(report_spec("disagg").expect("a report").id, "disaggregated");
    }

    #[test]
    fn refuses_a_report_nobody_publishes_and_names_the_three_that_exist() {
        let err = report_spec("supplemental").expect_err("not a report");
        assert_eq!(err.code, crate::error::codes::BAD_REQUEST);
        let hint = err.hint.expect("a hint");
        assert!(hint.contains("legacy"));
        assert!(hint.contains("disaggregated"));
        assert!(hint.contains("financial"));
    }

    #[test]
    fn splits_the_exchange_off_the_end_of_a_name_not_the_front() {
        assert_eq!(
            split_name(GOLD),
            ("GOLD".to_owned(), "COMMODITY EXCHANGE INC.".to_owned())
        );
        // Forty legacy markets carry more than one separator, and the exchange
        // is the trailing segment in every one.
        assert_eq!(
            split_name("COLORADO INTERSTATE - MAINLINE (BASIS) - ICE FUTURES ENERGY DIV"),
            (
                "COLORADO INTERSTATE - MAINLINE (BASIS)".to_owned(),
                "ICE FUTURES ENERGY DIV".to_owned()
            )
        );
        // And one market name has no separator at all.
        assert_eq!(
            split_name("- CHICAGO MERCANTILE EXCHANGE"),
            ("- CHICAGO MERCANTILE EXCHANGE".to_owned(), String::new())
        );
    }

    #[test]
    fn tells_a_contract_code_from_a_name_by_the_digit_in_it() {
        assert!(is_contract_code("088691"));
        // Codes with letters are real: `GOLD -1 TROY OUNCE - COINBASE
        // DERIVATIVES, LLC` is `088LM1`, and one code starts with a letter.
        assert!(is_contract_code("088LM1"));
        assert!(is_contract_code("0063A1"));
        assert!(is_contract_code("ZB9105"));
        // Six letters with no digit is a market a reader named.
        assert!(!is_contract_code("SILVER"));
        assert!(!is_contract_code("GOLD"));
        assert!(!is_contract_code("0886910"));
        assert!(!is_contract_code("088-91"));
        assert!(!is_contract_code(""));
    }

    #[test]
    fn reads_a_field_as_a_category_and_a_measure() {
        assert_eq!(
            split_field("noncomm_net"),
            ("noncomm".to_owned(), Measure::Net)
        );
        assert_eq!(
            split_field("m_money_long"),
            ("m_money".to_owned(), Measure::Long)
        );
        assert_eq!(
            split_field("LEVFUND_SHORT"),
            ("levfund".to_owned(), Measure::Short)
        );
        // A bare category is its net, because that is what a positioning series
        // means when nobody says otherwise.
        assert_eq!(split_field("noncomm"), ("noncomm".to_owned(), Measure::Net));
        assert_eq!(
            split_field("noncomm_gross"),
            ("noncomm_gross".to_owned(), Measure::Net)
        );
    }

    #[test]
    fn finds_a_category_by_its_key_or_by_the_start_of_its_name() {
        let legacy = report_spec("legacy").expect("a report");
        assert_eq!(
            find_category(legacy, "noncomm").expect("a category").key,
            "noncomm"
        );
        assert_eq!(
            find_category(legacy, "commercial").expect("a category").key,
            "comm"
        );
        assert!(find_category(legacy, "mmoney").is_none());

        let financial = report_spec("financial").expect("a report");
        assert_eq!(
            find_category(financial, "levfund").expect("a category").key,
            "levfund"
        );
        // Each report has only its own categories: the whole reason there are
        // three of them.
        assert!(find_category(financial, "comm").is_none());
    }

    #[test]
    fn every_category_key_is_unique_within_its_report() {
        for report in REPORTS {
            for category in report.categories {
                let matched = find_category(report, category.key).expect("its own key");
                assert_eq!(
                    matched.name, category.name,
                    "{} in {}",
                    category.key, report.id
                );
            }
        }
    }

    /* -------------------------------------------------------------- markets */

    #[tokio::test]
    async fn finds_markets_by_every_word_and_says_which_still_report() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher(LEGACY))
            .respond_with(ResponseTemplate::new(200).set_body_json(fixture(WHEAT_MARKETS_JSON)))
            .mount(&server)
            .await;

        let markets = find_markets(&state_for(&server), "wheat chicago", "legacy", 3)
            .await
            .expect("the portal answers");

        assert_eq!(markets.len(), 3);
        assert_eq!(markets[0].contract_code, "001612");
        assert_eq!(markets[0].market, "WHEAT-HRW");
        assert_eq!(markets[0].exchange, "CHICAGO BOARD OF TRADE");
        assert_eq!(markets[0].name, "WHEAT-HRW - CHICAGO BOARD OF TRADE");
        assert_eq!(markets[0].latest, "2026-08-18");
        // A `max()` over the whole group: the highest open interest this market
        // has ever reported, not this week's.
        assert_eq!(markets[0].peak_open_interest, Some(345_241.0));
        // The market that stopped reporting in 2022 sorts last, however well
        // its name matches.
        assert_eq!(markets[2].latest, "2022-02-22");
        assert_eq!(markets[2].contract_code, "00160F");

        let sent = server.received_requests().await.expect("one request");
        assert!(where_of(&sent[0]).contains("like '%WHEAT%'"));
        assert!(where_of(&sent[0]).contains("like '%CHICAGO%'"));
    }

    #[tokio::test]
    async fn a_search_with_no_words_asks_for_no_filter_at_all() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher(LEGACY))
            .respond_with(ResponseTemplate::new(200).set_body_json(fixture(WHEAT_MARKETS_JSON)))
            .mount(&server)
            .await;

        find_markets(&state_for(&server), "   ", "legacy", 3)
            .await
            .expect("the portal answers");

        let sent = server.received_requests().await.expect("one request");
        assert_eq!(where_of(&sent[0]), "", "an empty filter is no filter");
    }

    #[tokio::test]
    async fn a_limit_is_clamped_to_what_socrata_will_serve() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher(LEGACY))
            .and(query_param("$limit", "200"))
            .respond_with(ResponseTemplate::new(200).set_body_json(fixture(WHEAT_MARKETS_JSON)))
            .expect(1)
            .mount(&server)
            .await;

        find_markets(&state_for(&server), "wheat", "legacy", 10_000)
            .await
            .expect("the portal answers");
    }

    /* --------------------------------------------------------------- series */

    async fn mount_gold_history(server: &MockServer) {
        // The market lookup and the history come from the same path and are
        // told apart by what they select.
        Mock::given(method("GET"))
            .and(path_matcher(LEGACY))
            .and(query_param("$select", "market_and_exchange_names"))
            .respond_with(ResponseTemplate::new(200).set_body_json(name_only(GOLD)))
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(path_matcher(LEGACY))
            .and(query_param(
                "$select",
                "report_date_as_yyyy_mm_dd,noncomm_positions_long_all,\
                 noncomm_positions_short_all",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(fixture(GOLD_HISTORY_JSON)))
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn charts_a_net_position_as_long_minus_short() {
        let server = MockServer::start().await;
        mount_gold_history(&server).await;

        let answer = get_series(&state_for(&server), "legacy/088691/noncomm_net", None, None)
            .await
            .expect("the series resolves");

        // 224785-40875, 219622-37552, 227013-29379, 250936-32996, 256902-34713.
        assert_eq!(
            rows(&answer.observations),
            [
                ("2026-07-21", Some(183_910.0)),
                ("2026-07-28", Some(182_070.0)),
                ("2026-08-04", Some(197_634.0)),
                ("2026-08-11", Some(217_940.0)),
                ("2026-08-18", Some(222_189.0)),
            ]
        );
        assert_eq!(answer.series.provider, DataSource::Cftc);
        assert_eq!(answer.series.id, "legacy/088691/noncomm_net");
        assert_eq!(
            answer.series.title,
            "GOLD - COMMODITY EXCHANGE INC. — Non-commercial net position"
        );
        assert_eq!(answer.series.units, "Contracts");
        assert_eq!(answer.series.units_short, "Contracts");
        assert_eq!(answer.series.frequency, "Weekly (Tuesday)");
        assert_eq!(answer.series.source, "legacy");
        assert_eq!(answer.series.source_url, SOURCE_URL);
        assert_eq!(answer.series.observation_start, "2026-07-21");
        assert_eq!(answer.series.observation_end, "2026-08-18");
        assert_eq!(answer.series.last_updated, "2026-08-18");
        assert!(answer.series.notes.contains("report Tuesday"));
    }

    #[tokio::test]
    async fn charts_one_side_of_a_category_when_asked_for_it() {
        let server = MockServer::start().await;
        mount_gold_history(&server).await;
        let state = state_for(&server);

        let long = get_series(&state, "legacy/088691/noncomm_long", None, None)
            .await
            .expect("the series resolves");
        assert_eq!(long.observations[0].value, Some(224_785.0));
        assert!(long.series.title.ends_with("Non-commercial long"));

        let short = get_series(&state, "legacy/088691/noncomm_short", None, None)
            .await
            .expect("the series resolves");
        assert_eq!(short.observations[0].value, Some(40_875.0));
        assert!(short.series.title.ends_with("Non-commercial short"));
    }

    #[tokio::test]
    async fn charts_open_interest_from_its_own_column() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher(LEGACY))
            .and(query_param("$select", "market_and_exchange_names"))
            .respond_with(ResponseTemplate::new(200).set_body_json(name_only(GOLD)))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path_matcher(LEGACY))
            .and(query_param(
                "$select",
                "report_date_as_yyyy_mm_dd,open_interest_all",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                { "report_date_as_yyyy_mm_dd": "2026-08-18T00:00:00.000",
                  "open_interest_all": "406260" }
            ])))
            .mount(&server)
            .await;

        for field in ["oi", "open_interest"] {
            let answer = get_series(
                &state_for(&server),
                &format!("legacy/088691/{field}"),
                None,
                None,
            )
            .await
            .expect("the series resolves");
            assert_eq!(
                rows(&answer.observations),
                [("2026-08-18", Some(406_260.0))]
            );
            assert!(answer.series.title.ends_with("— open interest"));
        }
    }

    #[tokio::test]
    async fn a_market_named_in_words_is_resolved_before_it_is_charted() {
        let server = MockServer::start().await;
        // Not a contract code, so the name search answers first.
        Mock::given(method("GET"))
            .and(path_matcher(LEGACY))
            .and(query_param(
                "$group",
                "market_and_exchange_names,cftc_contract_market_code",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                { "market_and_exchange_names": GOLD,
                  "cftc_contract_market_code": "088691",
                  "latest": "2026-08-18T00:00:00.000",
                  "oi": "796883" }
            ])))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path_matcher(LEGACY))
            .and(query_param(
                "$select",
                "report_date_as_yyyy_mm_dd,noncomm_positions_long_all,\
                 noncomm_positions_short_all",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(fixture(GOLD_HISTORY_JSON)))
            .mount(&server)
            .await;

        let answer = get_series(&state_for(&server), "legacy/gold/noncomm_net", None, None)
            .await
            .expect("the series resolves");

        // The series is titled with the Commission's own name for the market,
        // not with what the reader typed.
        assert!(answer.series.title.starts_with(GOLD));
        // And the id stays as the reader wrote it, so the panel can be reopened.
        assert_eq!(answer.series.id, "legacy/gold/noncomm_net");
    }

    #[tokio::test]
    async fn a_market_name_with_a_quote_in_it_reaches_the_portal_intact() {
        let quoted = "CRUDE OIL, LIGHT 'SWEET' - NEW YORK MERCANTILE EXCHANGE";
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher(LEGACY))
            .and(query_param(
                "$group",
                "market_and_exchange_names,cftc_contract_market_code",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                { "market_and_exchange_names": quoted,
                  "cftc_contract_market_code": "067651",
                  "latest": "2026-08-18T00:00:00.000",
                  "oi": "502126" }
            ])))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path_matcher(LEGACY))
            .and(query_param(
                "$select",
                "report_date_as_yyyy_mm_dd,noncomm_positions_long_all,\
                 noncomm_positions_short_all",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                { "report_date_as_yyyy_mm_dd": "2026-08-18T00:00:00.000",
                  "noncomm_positions_long_all": "300000",
                  "noncomm_positions_short_all": "100000" }
            ])))
            .mount(&server)
            .await;

        let answer = get_series(
            &state_for(&server),
            "legacy/crude sweet/noncomm_net",
            None,
            None,
        )
        .await
        .expect("the series resolves");
        assert_eq!(
            rows(&answer.observations),
            [("2026-08-18", Some(200_000.0))]
        );

        let sent = server.received_requests().await.expect("two requests");
        let history = sent
            .iter()
            .find(|request| where_of(request).contains("CRUDE OIL"))
            .expect("the history query names the market");
        assert!(
            where_of(history).contains("LIGHT ''SWEET''"),
            "{}",
            where_of(history)
        );
    }

    #[tokio::test]
    async fn a_range_becomes_a_where_on_the_report_date() {
        let server = MockServer::start().await;
        mount_gold_history(&server).await;

        get_series(
            &state_for(&server),
            "legacy/088691/noncomm_net",
            Some("2026-07-01"),
            Some("2026-08-31"),
        )
        .await
        .expect("the series resolves");

        let sent = server.received_requests().await.expect("two requests");
        let history = sent
            .iter()
            .find(|request| where_of(request).contains(">="))
            .expect("the history query carries the range");
        let clause = where_of(history);
        // Whole days at both ends: the column is a timestamp, so `<= '…'` on a
        // bare date would exclude the last report in the range.
        assert!(clause.contains("report_date_as_yyyy_mm_dd >= '2026-07-01T00:00:00'"));
        assert!(clause.contains("report_date_as_yyyy_mm_dd <= '2026-08-31T23:59:59'"));
    }

    #[tokio::test]
    async fn an_empty_array_is_not_a_series_with_no_points_in_it() {
        // HTTP 200 and `[]` is how the portal answers a market this report does
        // not cover, and a range before the report existed.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/gpe5-46if.json"))
            .and(query_param("$select", "market_and_exchange_names"))
            .respond_with(ResponseTemplate::new(200).set_body_json(name_only(GOLD)))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path_matcher("/gpe5-46if.json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
            .mount(&server)
            .await;

        let err = get_series(
            &state_for(&server),
            "financial/088691/levfund_net",
            None,
            None,
        )
        .await
        .expect_err("no rows is not a series");

        assert_eq!(err.code, crate::error::codes::NOT_FOUND);
        assert!(err.message.contains("financial"));
        assert!(err.hint.expect("a hint").contains("rates, equity indices"));
    }

    #[tokio::test]
    async fn a_market_nobody_publishes_is_reported_before_a_series_is_asked_for() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher(LEGACY))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
            .mount(&server)
            .await;

        let err = get_series(&state_for(&server), "legacy/tulips/noncomm_net", None, None)
            .await
            .expect_err("no market is no series");

        assert_eq!(err.code, crate::error::codes::NOT_FOUND);
        assert!(err.message.contains("tulips"));
        assert!(err.hint.expect("a hint").contains("COT gold"));
    }

    #[tokio::test]
    async fn a_figure_the_report_does_not_state_is_not_a_zero() {
        // A net of zero is a market in perfect balance. An absent column is not
        // that, and must never be drawn as it.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher(LEGACY))
            .and(query_param("$select", "market_and_exchange_names"))
            .respond_with(ResponseTemplate::new(200).set_body_json(name_only(GOLD)))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path_matcher(LEGACY))
            .and(query_param(
                "$select",
                "report_date_as_yyyy_mm_dd,noncomm_positions_long_all,\
                 noncomm_positions_short_all",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                { "report_date_as_yyyy_mm_dd": "2026-08-04T00:00:00.000",
                  "noncomm_positions_long_all": "227013",
                  "noncomm_positions_short_all": "29379" },
                // One side stated and one missing: not a net of 250,000.
                { "report_date_as_yyyy_mm_dd": "2026-08-11T00:00:00.000",
                  "noncomm_positions_long_all": "250936" },
                { "report_date_as_yyyy_mm_dd": "2026-08-18T00:00:00.000",
                  "noncomm_positions_long_all": "",
                  "noncomm_positions_short_all": "34713" },
                // A row with no date at all is dropped rather than dated to "".
                { "noncomm_positions_long_all": "1", "noncomm_positions_short_all": "2" }
            ])))
            .mount(&server)
            .await;

        let answer = get_series(&state_for(&server), "legacy/088691/noncomm_net", None, None)
            .await
            .expect("the readable rows still chart");

        assert_eq!(
            rows(&answer.observations),
            [
                ("2026-08-04", Some(197_634.0)),
                ("2026-08-11", None),
                ("2026-08-18", None),
            ]
        );
    }

    #[tokio::test]
    async fn refuses_a_malformed_id_before_spending_a_request_on_it() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
            .expect(0)
            .mount(&server)
            .await;
        let state = state_for(&server);

        for id in ["", "legacy", "legacy/GOLD", "/", "legacy//"] {
            let err = get_series(&state, id, None, None)
                .await
                .expect_err("not a series id");
            assert_eq!(err.code, crate::error::codes::BAD_REQUEST, "{id:?}");
            assert!(err.hint.expect("a hint").contains("report/market/field"));
        }
    }

    #[tokio::test]
    async fn refuses_a_category_the_report_does_not_have_before_resolving_a_market() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
            .expect(0)
            .mount(&server)
            .await;

        // Managed money is a disaggregated category; the legacy report has
        // commercials and non-commercials and nothing else.
        let err = get_series(&state_for(&server), "legacy/088691/mmoney_net", None, None)
            .await
            .expect_err("not a legacy category");

        assert_eq!(err.code, crate::error::codes::BAD_REQUEST);
        assert!(err.message.contains("mmoney"));
        let hint = err.hint.expect("a hint");
        assert!(hint.contains("noncomm, comm, nonrept"));
        assert!(hint.contains("_net"));
    }

    #[tokio::test]
    async fn refuses_a_report_nobody_publishes_before_spending_a_request() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
            .expect(0)
            .mount(&server)
            .await;

        let err = get_series(&state_for(&server), "supplemental/088691/oi", None, None)
            .await
            .expect_err("not a report");
        assert_eq!(err.code, crate::error::codes::BAD_REQUEST);
    }

    #[tokio::test]
    async fn a_series_is_cached_rather_than_re_asked() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher(LEGACY))
            .and(query_param("$select", "market_and_exchange_names"))
            .respond_with(ResponseTemplate::new(200).set_body_json(name_only(GOLD)))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path_matcher(LEGACY))
            .and(query_param(
                "$select",
                "report_date_as_yyyy_mm_dd,noncomm_positions_long_all,\
                 noncomm_positions_short_all",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(fixture(GOLD_HISTORY_JSON)))
            .expect(1)
            .mount(&server)
            .await;

        let state = state_for(&server);
        for _ in 0..3 {
            get_series(&state, "legacy/088691/noncomm_net", None, None)
                .await
                .expect("the series resolves");
        }
    }

    /* --------------------------------------------------------------- report */

    #[tokio::test]
    async fn reads_every_category_out_of_a_captured_report_row() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher(LEGACY))
            .and(query_param("$select", "market_and_exchange_names"))
            .respond_with(ResponseTemplate::new(200).set_body_json(name_only(GOLD)))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path_matcher(LEGACY))
            .and(query_param("$limit", "1"))
            .and(query_param(
                "$where",
                "market_and_exchange_names='GOLD - COMMODITY EXCHANGE INC.'",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(fixture(GOLD_REPORT_JSON)))
            .mount(&server)
            .await;

        let report = get_report(&state_for(&server), "088691", "legacy")
            .await
            .expect("the report resolves");

        assert_eq!(report.report, "legacy");
        assert_eq!(report.market, "GOLD");
        assert_eq!(report.exchange, "COMMODITY EXCHANGE INC.");
        assert_eq!(report.contract_code, "088691");
        assert_eq!(report.date, "2026-08-18");
        assert_eq!(report.open_interest, Some(406_260.0));
        assert_eq!(report.open_interest_change, Some(5_951.0));
        assert_eq!(report.categories.len(), 3);

        let noncomm = &report.categories[0];
        assert_eq!(noncomm.name, "Non-commercial");
        assert_eq!(noncomm.long, Some(256_902.0));
        assert_eq!(noncomm.short, Some(34_713.0));
        // Read out of the Commission's own misspelt column.
        assert_eq!(noncomm.spreading, Some(28_961.0));
        assert_eq!(noncomm.net, Some(222_189.0));
        assert_eq!(noncomm.net_change, Some(4_249.0));
        assert_eq!(noncomm.percent_long, Some(63.2));
        assert_eq!(noncomm.percent_short, Some(8.5));
        assert_eq!(noncomm.trader_count, Some(172.0));

        let comm = &report.categories[1];
        assert_eq!(comm.net, Some(-258_418.0));
        assert_eq!(comm.net_change, Some(-5_778.0));
        // The legacy report breaks out spreading for non-commercials only.
        assert_eq!(comm.spreading, None);
        assert_eq!(comm.trader_count, Some(51.0));

        let nonrept = &report.categories[2];
        assert_eq!(nonrept.net, Some(36_229.0));
        assert_eq!(nonrept.net_change, Some(1_529.0));
        // A non-reportable trader is by definition not counted.
        assert_eq!(nonrept.trader_count, None);
    }

    #[tokio::test]
    async fn a_market_this_report_does_not_cover_is_named_as_that() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/72hh-3qpy.json"))
            .and(query_param("$select", "market_and_exchange_names"))
            .respond_with(ResponseTemplate::new(200).set_body_json(name_only(GOLD)))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path_matcher("/72hh-3qpy.json"))
            .and(query_param("$limit", "1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
            .mount(&server)
            .await;

        let err = get_report(&state_for(&server), "088691", "disagg")
            .await
            .expect_err("no row is no report");
        assert_eq!(err.code, crate::error::codes::NOT_FOUND);
        assert!(err.message.contains("disaggregated"));
    }

    #[test]
    fn the_reports_are_listed_with_what_each_covers() {
        let listed = list_reports();
        assert_eq!(listed.len(), 3);
        assert_eq!(listed[0].0, "legacy");
        assert!(listed[0].2.contains("1986"));
    }

    /* --------------------------------------------------------------- search */

    #[tokio::test]
    async fn search_offers_the_two_series_a_reader_charts_per_market() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher(LEGACY))
            .respond_with(ResponseTemplate::new(200).set_body_json(fixture(WHEAT_MARKETS_JSON)))
            .mount(&server)
            .await;

        let results = search(&state_for(&server), "wheat chicago", 4)
            .await
            .expect("the portal answers");

        assert_eq!(results.len(), 4);
        assert_eq!(results[0].provider, DataSource::Cftc);
        // Keyed by contract code, which is stable where a name is not.
        assert_eq!(results[0].id, "legacy/001612/noncomm_net");
        assert_eq!(
            results[0].title,
            "WHEAT-HRW (CHICAGO BOARD OF TRADE) — non-commercial net position"
        );
        assert_eq!(results[0].units.as_deref(), Some("Contracts"));
        assert_eq!(results[0].frequency.as_deref(), Some("Weekly"));
        // Which of the three reports answered.
        assert_eq!(results[0].source.as_deref(), Some("legacy"));
        assert_eq!(
            results[0].observation_range.as_deref(),
            Some("to 2026-08-18")
        );
        assert_eq!(results[1].id, "legacy/001612/oi");
        assert_eq!(results[2].id, "legacy/001602/noncomm_net");
    }

    #[tokio::test]
    async fn search_asks_for_half_the_rows_it_returns() {
        // Two series per market, so a board of ten rows wants five markets.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher(LEGACY))
            .and(query_param("$limit", "5"))
            .respond_with(ResponseTemplate::new(200).set_body_json(fixture(WHEAT_MARKETS_JSON)))
            .expect(1)
            .mount(&server)
            .await;

        let results = search(&state_for(&server), "wheat", 10)
            .await
            .expect("the portal answers");
        // Three markets came back, so six rows, which is under the limit.
        assert_eq!(results.len(), 6);
    }

    #[tokio::test]
    async fn search_for_nothing_asks_the_portal_nothing() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
            .expect(0)
            .mount(&server)
            .await;
        let state = state_for(&server);

        assert!(search(&state, "", 10).await.expect("no query").is_empty());
        assert!(search(&state, "   ", 10)
            .await
            .expect("no query")
            .is_empty());
        assert!(search(&state, "gold", 0).await.expect("no room").is_empty());
    }

    #[test]
    fn the_publisher_needs_no_credential_at_all() {
        // Verified live from this container against all three datasets.
        assert_eq!(unavailable(&AppState::new(Config::default())), None);
    }
}
