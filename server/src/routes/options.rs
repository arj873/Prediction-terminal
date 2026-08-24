//! Option routes.
//!
//! `{symbol}` picks the venue the way `/api/spot/{class}` picks a price feed,
//! but without making the caller state the asset class: an underlying either has
//! a Deribit board or it does not, and `OPT BTC` should not have to be spelled
//! differently from `OPT AAPL`. Crypto is checked first because the set is small
//! and closed; everything else is routed to the equity providers.
//!
//! Four views over one board — chain, surface, positioning and a single contract
//! — all derived from the same normalised quotes so they cannot disagree. See
//! [`crate::sources::optionboard`] for what "derived" means at each step.

use axum::extract::{Path, RawQuery, State};
use axum::routing::get;
use axum::{Json, Router};
use terminal_core::types::{
    AssetClass, OptionChain, OptionExpiriesResponse, OptionExpiry, OptionPositioning,
    OptionQuoteResponse, OptionSurface, OptionUnderlying, OptionUnderlyingsResponse,
};

use crate::app::AppState;
use crate::error::{Result, UpstreamError};
use crate::routes::helpers::{candle_window, now_seconds, QueryParams};
use crate::sources::deribit;
use crate::sources::equityoptions as equity;
use crate::sources::optionboard::{
    carry_for, chain_for, enrich, expiries_of, iso_date_of, positioning_for, resolve_expiry,
    surface_for, OptionBoard,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/options/underlyings", get(underlyings))
        // Before `/{symbol}/…`, because `contract` would otherwise be read as a
        // symbol.
        .route("/api/options/contract/{contract}", get(contract))
        .route("/api/options/{symbol}/expiries", get(expiries))
        .route("/api/options/{symbol}/chain", get(chain))
        .route("/api/options/{symbol}/surface", get(surface))
        .route("/api/options/{symbol}/positioning", get(positioning))
}

/// Expiries loaded when a view needs the whole term structure.
///
/// Deribit hands over every expiry in one response, so this only bites on the
/// equity path, where Yahoo serves one expiry per request. Eight covers the part
/// of the curve anyone reads — front week through a few months — without turning
/// one panel into twenty upstream calls.
const TERM_EXPIRIES: usize = 8;

/// The carry assumed only when nothing in the market will say what it is.
///
/// Every response derived from it is stamped `forwardSource: "assumed"`, so a
/// panel can tell the reader the Greeks rest on an assumption rather than on a
/// quote.
fn assumed_rate(state: &AppState) -> f64 {
    state.config().options_rate
}

/* ------------------------------------------------------------------ loading */

struct Loaded {
    board: OptionBoard,
    expiries: Vec<OptionExpiry>,
}

/// Load a board, optionally deep enough to see the whole curve.
///
/// The two-pass shape on the equity side is deliberate: the first call is the
/// only way to learn which expiries exist, and it is cached, so the second call
/// pays for the extra expiries alone rather than re-fetching the first.
async fn load_board(state: &AppState, symbol: &str, whole_curve: bool) -> Result<Loaded> {
    let now = now_seconds();
    #[allow(clippy::cast_precision_loss)]
    let now = now as f64;

    if deribit::supports(symbol) {
        let board = deribit::get_board(state, symbol).await?;
        let expiries = expiries_of(&board, now);
        return Ok(Loaded { board, expiries });
    }

    let (first, dates) = equity::get_board(state, symbol, &[]).await?;
    if !whole_curve {
        let expiries = expiries_of(&first, now);
        return Ok(Loaded {
            board: first,
            expiries,
        });
    }

    let wanted: Vec<String> = dates.into_iter().take(TERM_EXPIRIES).collect();
    let (deep, _) = equity::get_board(state, symbol, &wanted).await?;
    let expiries = expiries_of(&deep, now);
    Ok(Loaded {
        board: deep,
        expiries,
    })
}

fn params(raw: Option<String>) -> QueryParams {
    QueryParams::parse(raw.as_deref().unwrap_or(""))
}

/* ------------------------------------------------------------------- routes */

async fn underlyings() -> Json<OptionUnderlyingsResponse> {
    let mut list: Vec<OptionUnderlying> = deribit::underlying_symbols()
        .into_iter()
        .map(|(symbol, name)| OptionUnderlying {
            symbol: symbol.to_owned(),
            name: name.to_owned(),
            asset_class: AssetClass::Crypto,
            venue: "Deribit".to_owned(),
        })
        .collect();
    list.extend(
        equity::underlying_hints()
            .into_iter()
            .map(|(symbol, name)| OptionUnderlying {
                symbol: symbol.to_owned(),
                name: name.to_owned(),
                asset_class: AssetClass::Stock,
                venue: "OPRA".to_owned(),
            }),
    );

    Json(OptionUnderlyingsResponse {
        underlyings: list,
        note: "Crypto boards are the ones Deribit lists. Equity boards exist for most US listed \
               names with options — the list above is a starting point, not a limit."
            .to_owned(),
    })
}

async fn expiries(
    State(state): State<AppState>,
    Path(symbol): Path<String>,
) -> Result<Json<OptionExpiriesResponse>> {
    let symbol = symbol.trim().to_uppercase();
    let loaded = load_board(&state, &symbol, false).await?;
    Ok(Json(OptionExpiriesResponse {
        symbol: loaded.board.symbol.clone(),
        name: loaded.board.name.clone(),
        asset_class: loaded.board.asset_class,
        spot: loaded.board.spot,
        expiries: loaded.expiries,
        venue: loaded.board.venue.clone(),
        source: loaded.board.source.clone(),
    }))
}

async fn chain(
    State(state): State<AppState>,
    Path(symbol): Path<String>,
    RawQuery(raw): RawQuery,
) -> Result<Json<OptionChain>> {
    let symbol = symbol.trim().to_uppercase();
    let query = params(raw);
    let loaded = load_board(&state, &symbol, false).await?;
    #[allow(clippy::cast_precision_loss)]
    let now = now_seconds() as f64;
    let expiry = resolve_expiry(&loaded.expiries, query.get("expiry").unwrap_or(""), now)?;
    Ok(Json(chain_for(
        &loaded.board,
        &expiry,
        &loaded.expiries,
        assumed_rate(&state),
    )))
}

async fn surface(
    State(state): State<AppState>,
    Path(symbol): Path<String>,
    RawQuery(raw): RawQuery,
) -> Result<Json<OptionSurface>> {
    let symbol = symbol.trim().to_uppercase();
    let query = params(raw);
    // The term structure is the point of this view, so it is worth the extra
    // expiries the chain does not need.
    let loaded = load_board(&state, &symbol, true).await?;
    #[allow(clippy::cast_precision_loss)]
    let now = now_seconds() as f64;
    let expiry = resolve_expiry(&loaded.expiries, query.get("expiry").unwrap_or(""), now)?;
    Ok(Json(surface_for(
        &loaded.board,
        &expiry,
        &loaded.expiries,
        assumed_rate(&state),
    )))
}

async fn positioning(
    State(state): State<AppState>,
    Path(symbol): Path<String>,
    RawQuery(raw): RawQuery,
) -> Result<Json<OptionPositioning>> {
    let symbol = symbol.trim().to_uppercase();
    let query = params(raw);
    let loaded = load_board(&state, &symbol, false).await?;
    #[allow(clippy::cast_precision_loss)]
    let now = now_seconds() as f64;
    let expiry = resolve_expiry(&loaded.expiries, query.get("expiry").unwrap_or(""), now)?;
    Ok(Json(positioning_for(&loaded.board, &expiry)))
}

/// One contract, with its pair leg and whatever history the venue publishes.
///
/// Routed on the shape of the identifier rather than on a class parameter: an
/// OCC symbol (`AAPL260918C00300000`) and a Deribit name
/// (`BTC-25DEC26-104000-C`) are not confusable, so a reader can paste either one
/// straight in.
async fn contract(
    State(state): State<AppState>,
    Path(contract): Path<String>,
    RawQuery(raw): RawQuery,
) -> Result<Json<OptionQuoteResponse>> {
    let contract = contract.trim().to_uppercase();
    let query = params(raw);

    let crypto_symbol = deribit::symbol_of_contract(&contract);
    let occ = equity::parse_occ(&contract);

    let Some(symbol) = crypto_symbol
        .map(str::to_owned)
        .or_else(|| occ.as_ref().map(|o| o.symbol.clone()))
    else {
        return Err(UpstreamError::bad_request(format!(
            "\"{contract}\" is not a recognised option contract"
        ))
        .with_hint(
            "Use an OCC symbol for equities (`AAPL260918C00300000`) or a Deribit instrument name \
             for crypto (`BTC-25DEC26-104000-C`). Clicking a row in `OPT` fills either one in for \
             you.",
        ));
    };

    let loaded = load_board(&state, &symbol, false).await?;

    let Some(quote) = loaded
        .board
        .quotes
        .iter()
        .find(|q| q.contract == contract)
        .cloned()
    else {
        return Err(UpstreamError::not_found(format!(
            "{contract} is not on the current {symbol} board"
        ))
        .with_hint(format!(
            "The contract may have expired, or its expiry may not be loaded. Run `OPT {symbol}` \
             and pick the expiry from the strip."
        )));
    };

    #[allow(clippy::cast_precision_loss)]
    let now = now_seconds() as f64;
    let expiry = match loaded.expiries.iter().find(|e| e.expiry == quote.expiry) {
        Some(found) => found.clone(),
        None => resolve_expiry(&loaded.expiries, &iso_date_of(quote.expiry), now)?,
    };

    let slice: Vec<_> = loaded
        .board
        .quotes
        .iter()
        .filter(|q| q.expiry == quote.expiry)
        .cloned()
        .collect();
    let carry = carry_for(
        &slice,
        loaded.board.spot,
        expiry.years_to_expiry,
        assumed_rate(&state),
    );

    let pair = slice
        .iter()
        .find(|q| {
            (q.strike - quote.strike).abs() < f64::EPSILON && q.option_type != quote.option_type
        })
        .map(|q| enrich(q, loaded.board.spot, &carry, expiry.years_to_expiry));

    let interval = query.interval()?;
    let (start, end) = candle_window(&query, interval, now_seconds())?;

    // Only Deribit publishes option price history for free. Saying that plainly
    // beats an empty chart that reads as a loading failure.
    let (history, history_note) = if crypto_symbol.is_some() {
        deribit::get_history(&state, &contract, interval, start, end)
            .await
            .unwrap_or_else(|_| {
                (
                    Vec::new(),
                    "Deribit returned no history for this contract.".to_owned(),
                )
            })
    } else {
        (
            Vec::new(),
            "No free source publishes historical prices for US listed option contracts, so this \
             panel shows the live quote only. Crypto contracts get a chart, because Deribit \
             publishes one."
                .to_owned(),
        )
    };

    Ok(Json(OptionQuoteResponse {
        symbol: loaded.board.symbol.clone(),
        name: loaded.board.name.clone(),
        asset_class: loaded.board.asset_class,
        currency: loaded.board.currency.clone(),
        spot: loaded.board.spot,
        forward: carry.forward,
        forward_source: carry.source,
        rate: carry.rate,
        carry: carry.carry,
        contract_size: loaded.board.contract_size,
        contract: enrich(&quote, loaded.board.spot, &carry, expiry.years_to_expiry),
        pair,
        history,
        history_note,
        venue: loaded.board.venue.clone(),
        source: loaded.board.source.clone(),
        note: loaded.board.note.clone(),
    }))
}
