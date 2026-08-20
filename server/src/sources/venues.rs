//! One interface over the six brokers.
//!
//! Every venue module already exports the same handful of verbs; this states
//! that as one function per verb, each taking the [`Venue`] and handing the call
//! to the module that serves it, so the routes and the cross-venue code are
//! written once instead of once per broker.
//!
//! The interface is deliberately the union of what the venues *can* answer, not
//! the intersection — Polymarket US has no public candles or tape, ForecastEx
//! has no book at all, and asking
//! for one fails with a described `unsupported` error rather than the surface
//! pretending the verb is absent. A terminal that silently omitted a chart would
//! be worse than one that says why there isn't one.
//!
//! Where a gap is already declared, that is where the refusal comes from.
//! [`terminal_core::venue`]'s capability table states which venue publishes
//! candles, which publishes prints, and which `TOP` boards each can be ranked
//! on, so this module declines before it dials rather than calling through and
//! translating whatever the upstream's own failure turned into. Inferring the
//! answer from an empty result made an outage indistinguishable from a fact
//! about the venue's API, and the panel said the wrong one out loud.

use std::sync::Arc;

use terminal_core::types::{
    CandleInterval, CandlesResponse, Market, OrderBook, SeriesInfo, TradesResponse, VenueEvent,
};
use terminal_core::venue::{
    supports, supports_sort, venue_ids, venue_info, Capability, MoverSort, Venue,
};

use crate::app::AppState;
use crate::error::{Result, UpstreamError};
use crate::sources::corpus::{Corpus, SearchResponse};
use crate::sources::{forecastex, gemini, kalshi, polymarket, polymarketus, predictfun};

/* ------------------------------------------------------- declared refusals */

/// "Kalshi (kx:) and Polymarket International (pm:)" — the venues a reader can
/// retype the command against. `None` when there are none, so a hint promising
/// somewhere to go is never printed without one.
fn venue_list(venues: &[Venue]) -> Option<String> {
    let named: Vec<String> = venues
        .iter()
        .map(|venue| {
            let info = venue_info(*venue);
            format!("{} ({})", info.label, info.prefix)
        })
        .collect();

    match named.split_last() {
        None => None,
        Some((last, [])) => Some(last.clone()),
        Some((last, rest)) => Some(format!("{} and {last}", rest.join(", "))),
    }
}

/// Refuse a call the registry already says this venue cannot serve.
///
/// The message is the venue's own `capabilities.note`, which is the sentence
/// written to be read here — it explains what the venue does publish and what
/// the missing thing would cost, which a translated upstream 404 cannot. The
/// hint points at a venue that can answer, because retyping the reference with
/// another prefix is the one thing the reader can actually do about it.
fn declined(venue: Venue, what: &str, elsewhere: &[Venue]) -> UpstreamError {
    let info = venue_info(venue);
    let error = if info.capabilities.note.is_empty() {
        // No row currently declares a gap without a note. If one ever does,
        // naming the venue and the verb beats an empty panel.
        UpstreamError::unsupported(format!("{} does not serve {what}", info.label))
    } else {
        UpstreamError::unsupported(info.capabilities.note)
    };

    match venue_list(elsewhere) {
        Some(list) => error.with_hint(format!("Ask {list} for {what} instead.")),
        None => error,
    }
}

/// The venues that do serve `capability`, in registry order.
fn serving(capability: Capability) -> Vec<Venue> {
    venue_ids()
        .into_iter()
        .filter(|venue| supports(*venue, capability))
        .collect()
}

/// The venues `TOP` can rank on `sort`, in registry order.
fn ranking(sort: MoverSort) -> Vec<Venue> {
    venue_ids()
        .into_iter()
        .filter(|venue| supports_sort(*venue, sort))
        .collect()
}

/* -------------------------------------------------------------- the verbs */

/// One contract, quoted.
pub async fn get_market(state: &AppState, venue: Venue, id: &str) -> Result<Market> {
    match venue {
        Venue::Kalshi => kalshi::get_market(state, id).await,
        Venue::Polymarket => polymarket::get_market(state, id).await,
        Venue::PolymarketUs => polymarketus::get_market(state, id).await,
        Venue::Gemini => gemini::get_market(state, id).await,
        Venue::PredictFun => predictfun::get_market(state, id).await,
        Venue::ForecastEx => forecastex::get_market(state, id).await,
    }
}

/// The resting book, `depth` levels a side.
///
/// ForecastEx has a live market and no book: it matches by pairing a YES buyer
/// with a NO buyer, so a print is the whole of what exists and there is no bid,
/// no ask and no ladder anywhere in its public API. Refusing here rather than
/// dressing the last print up as a one-level ladder is the difference between
/// the panel saying how the exchange works and the panel inventing a quote.
pub async fn get_order_book(
    state: &AppState,
    venue: Venue,
    id: &str,
    depth: usize,
) -> Result<OrderBook> {
    if !supports(venue, Capability::Book) {
        return Err(declined(venue, "an order book", &serving(Capability::Book)));
    }

    match venue {
        Venue::Kalshi => kalshi::get_order_book(state, id, depth).await,
        Venue::Polymarket => polymarket::get_order_book(state, id, depth).await,
        Venue::PolymarketUs => polymarketus::get_order_book(state, id, depth).await,
        Venue::Gemini => gemini::get_order_book(state, id, depth).await,
        Venue::PredictFun => predictfun::get_order_book(state, id, depth).await,
        Venue::ForecastEx => forecastex::get_order_book(state, id, depth).await,
    }
}

/// The public print tape.
///
/// Kalshi's own tape pages with a cursor, which neither Polymarket exposes; the
/// shared verb takes a limit only, and the Kalshi route reaches for
/// [`kalshi::get_trades`] directly when it needs the next page.
pub async fn get_trades(
    state: &AppState,
    venue: Venue,
    id: &str,
    limit: usize,
) -> Result<TradesResponse> {
    if !supports(venue, Capability::Trades) {
        return Err(declined(
            venue,
            "a print tape",
            &serving(Capability::Trades),
        ));
    }

    match venue {
        Venue::Kalshi => kalshi::get_trades(state, id, limit, None).await,
        Venue::Polymarket => polymarket::get_trades(state, id, limit).await,
        Venue::PolymarketUs => polymarketus::get_trades(state, id, limit).await,
        Venue::Gemini => gemini::get_trades(state, id, limit).await,
        Venue::PredictFun => predictfun::get_trades(state, id, limit).await,
        Venue::ForecastEx => forecastex::get_trades(state, id, limit).await,
    }
}

/// Price history over `[start_ts, end_ts]`, in `interval` buckets.
pub async fn get_candles(
    state: &AppState,
    venue: Venue,
    id: &str,
    interval: CandleInterval,
    start_ts: i64,
    end_ts: i64,
) -> Result<CandlesResponse> {
    if !supports(venue, Capability::Candles) {
        return Err(declined(venue, "candles", &serving(Capability::Candles)));
    }

    match venue {
        Venue::Kalshi => kalshi::get_candles(state, id, interval, start_ts, end_ts, None).await,
        Venue::Polymarket => polymarket::get_candles(state, id, interval, start_ts, end_ts).await,
        Venue::PolymarketUs => {
            polymarketus::get_candles(state, id, interval, start_ts, end_ts).await
        }
        Venue::Gemini => gemini::get_candles(state, id, interval, start_ts, end_ts).await,
        Venue::PredictFun => predictfun::get_candles(state, id, interval, start_ts, end_ts).await,
        Venue::ForecastEx => forecastex::get_candles(state, id, interval, start_ts, end_ts).await,
    }
}

/// One event and the ladder that resolves it.
pub async fn get_event(state: &AppState, venue: Venue, id: &str) -> Result<VenueEvent> {
    match venue {
        Venue::Kalshi => kalshi::get_event(state, id).await,
        Venue::Polymarket => polymarket::get_event(state, id).await,
        Venue::PolymarketUs => polymarketus::get_event(state, id).await,
        Venue::Gemini => gemini::get_event(state, id).await,
        Venue::PredictFun => predictfun::get_event(state, id).await,
        Venue::ForecastEx => forecastex::get_event(state, id).await,
    }
}

/// The venue's series, optionally narrowed to a category.
///
/// Only Kalshi's catalogue endpoint honours the filter, and the registry says
/// so. A category is therefore dropped rather than passed to a venue that would
/// answer the same unfiltered list to any value of it — reading which venues
/// those are off `series_category_filter` keeps the fact in the table with the
/// rest of them instead of restating it here.
pub async fn list_series(
    state: &AppState,
    venue: Venue,
    category: Option<&str>,
) -> Result<Vec<SeriesInfo>> {
    let category = category.filter(|_| venue_info(venue).capabilities.series_category_filter);

    match venue {
        Venue::Kalshi => kalshi::list_series(state, category).await,
        Venue::Polymarket => polymarket::list_series(state).await,
        Venue::PolymarketUs => polymarketus::list_series(state).await,
        Venue::Gemini => gemini::list_series(state, category).await,
        Venue::PredictFun => predictfun::list_series(state, category).await,
        Venue::ForecastEx => forecastex::list_series(state, category).await,
    }
}

/// Rank the venue's open events against a free-text query.
pub async fn search(
    state: &AppState,
    venue: Venue,
    query: &str,
    limit: usize,
) -> Result<SearchResponse> {
    match venue {
        Venue::Kalshi => kalshi::search(state, query, limit).await,
        Venue::Polymarket => polymarket::search(state, query, limit).await,
        Venue::PolymarketUs => polymarketus::search(state, query, limit).await,
        Venue::Gemini => gemini::search(state, query, limit).await,
        Venue::PredictFun => predictfun::search(state, query, limit).await,
        Venue::ForecastEx => forecastex::search(state, query, limit).await,
    }
}

/// The leaderboard behind `TOP`.
///
/// A venue that publishes nothing to rank on is refused here rather than asked:
/// its board would come back empty, which reads as "nothing trades" rather than
/// "this venue does not publish the figure".
pub async fn top_markets(
    state: &AppState,
    venue: Venue,
    sort: MoverSort,
    limit: usize,
) -> Result<Vec<Market>> {
    if !supports_sort(venue, sort) {
        return Err(declined(
            venue,
            &format!("the {sort} board"),
            &ranking(sort),
        ));
    }

    match venue {
        Venue::Kalshi => kalshi::top_markets(state, sort, limit).await,
        Venue::Polymarket => polymarket::top_markets(state, sort, limit).await,
        Venue::PolymarketUs => polymarketus::top_markets(state, sort, limit).await,
        Venue::Gemini => gemini::top_markets(state, sort, limit).await,
        Venue::PredictFun => predictfun::top_markets(state, sort, limit).await,
        Venue::ForecastEx => forecastex::top_markets(state, sort, limit).await,
    }
}

/// The open-event snapshot `search` and `top_markets` read, for callers that
/// slice it differently.
pub async fn corpus_snapshot(state: &AppState, venue: Venue) -> Result<Arc<Corpus>> {
    match venue {
        Venue::Kalshi => kalshi::corpus_snapshot(state).await,
        Venue::Polymarket => polymarket::corpus_snapshot(state).await,
        Venue::PolymarketUs => polymarketus::corpus_snapshot(state).await,
        Venue::Gemini => gemini::corpus_snapshot(state).await,
        Venue::PredictFun => predictfun::corpus_snapshot(state).await,
        Venue::ForecastEx => forecastex::corpus_snapshot(state).await,
    }
}

/// Kick one venue's crawl off in the background, without waiting for it.
pub fn warm_corpus(state: &AppState, venue: Venue) {
    match venue {
        Venue::Kalshi => kalshi::warm_corpus(state),
        Venue::Polymarket => polymarket::warm_corpus(state),
        Venue::PolymarketUs => polymarketus::warm_corpus(state),
        Venue::Gemini => gemini::warm_corpus(state),
        Venue::PredictFun => predictfun::warm_corpus(state),
        Venue::ForecastEx => forecastex::warm_corpus(state),
    }
}

/// Crawl every venue's catalogue into the cache, so the first search pays for
/// none of it.
///
/// Awaited rather than spawned per venue, because what runs after it —
/// [`crate::sources::crossvenue::warm_indexes`] — reads these same snapshots,
/// and would otherwise start a crawl of its own per venue alongside these.
///
/// A venue that is down is logged and skipped. This runs at startup with nobody
/// to report to, and one broker refusing a datacentre IP must not cost the
/// others their warm catalogue: the failure is not cached, so the next reader
/// retries it anyway.
pub async fn warm_all(state: &AppState) {
    let crawls = venue_ids()
        .into_iter()
        .map(|venue| async move { (venue, corpus_snapshot(state, venue).await) });

    for (venue, result) in futures::future::join_all(crawls).await {
        match result {
            Ok(corpus) => tracing::info!(
                venue = %venue,
                events = corpus.events.len(),
                markets = corpus.markets.len(),
                "corpus warm"
            ),
            Err(err) => tracing::warn!(venue = %venue, error = %err, "corpus warm failed"),
        }
    }
}

#[cfg(test)]
mod tests {
    //! Dispatch tests.
    //!
    //! Every venue's base points at its own fixture server, and each verb is
    //! asked for all three venues at once: routing to the wrong module reaches a
    //! server with no matching mount, and the `venue` field on what comes back
    //! says which module normalised it. The refusal tests assert the fixture
    //! server was never dialled at all, which is the difference between failing
    //! from the registry and failing from the upstream.

    use super::*;
    use serde_json::{json, Value};
    use wiremock::matchers::{method, path, query_param, query_param_is_missing};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use crate::config::Config;
    use crate::error::codes;

    /// The four fixture servers the three venues read between them.
    struct Venues {
        kalshi: MockServer,
        gamma: MockServer,
        clob: MockServer,
        data: MockServer,
        us: MockServer,
    }

    impl Venues {
        async fn start() -> Self {
            Self {
                kalshi: MockServer::start().await,
                gamma: MockServer::start().await,
                clob: MockServer::start().await,
                data: MockServer::start().await,
                us: MockServer::start().await,
            }
        }

        fn state(&self) -> AppState {
            AppState::new(Config {
                kalshi_api_base: self.kalshi.uri(),
                polymarket_gamma_base: self.gamma.uri(),
                polymarket_clob_base: self.clob.uri(),
                polymarket_data_base: self.data.uri(),
                polymarket_us_api_base: self.us.uri(),
                ..Config::default()
            })
        }

        /// Every request any of the five servers saw.
        async fn requests(&self) -> usize {
            let mut total = 0;
            for server in [&self.kalshi, &self.gamma, &self.clob, &self.data, &self.us] {
                total += seen(server).await;
            }
            total
        }
    }

    /// How many requests one fixture server has answered.
    async fn seen(server: &MockServer) -> usize {
        server.received_requests().await.map_or(0, |r| r.len())
    }

    async fn mount(server: &MockServer, at: &str, body: Value) {
        Mock::given(method("GET"))
            .and(path(at))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(server)
            .await;
    }

    const KALSHI_ID: &str = "KXFED-26SEP-T3.75";
    const SLUG: &str = "fed-decision-september";

    /// One open event per venue, as each venue's catalogue crawl sees it. All
    /// three are titled the same so a single query matches whichever was asked.
    async fn catalogues(venues: &Venues) {
        mount(
            &venues.kalshi,
            "/events",
            json!({
                "events": [{
                    "event_ticker": "KXFED-26SEP",
                    "series_ticker": "KXFED",
                    "title": "Fed decision",
                    "markets": [{ "ticker": KALSHI_ID, "volume_24h_fp": "10.00" }],
                }],
                "cursor": "",
            }),
        )
        .await;

        mount(
            &venues.gamma,
            "/events",
            json!([{
                "slug": SLUG,
                "title": "Fed decision",
                "seriesSlug": "fomc",
                "markets": [{ "slug": SLUG, "question": "Fed decision", "volume24hr": "10" }],
            }]),
        )
        .await;

        mount(
            &venues.us,
            "/v1/events",
            json!({
                "events": [{
                    "slug": SLUG,
                    "title": "Fed decision",
                    "category": "macro",
                    "markets": [{ "slug": SLUG, "title": "Fed decision" }],
                }],
            }),
        )
        .await;
    }

    /* --------------------------------------------------------- routing */

    #[tokio::test]
    async fn a_market_is_read_by_the_venue_it_was_asked_of() {
        let venues = Venues::start().await;
        mount(
            &venues.kalshi,
            &format!("/markets/{KALSHI_ID}"),
            json!({ "market": { "ticker": KALSHI_ID } }),
        )
        .await;
        mount(&venues.gamma, "/markets", json!([{ "slug": SLUG }])).await;
        mount(
            &venues.us,
            &format!("/v1/market/slug/{SLUG}"),
            json!({ "market": { "slug": SLUG } }),
        )
        .await;
        mount(&venues.us, &format!("/v1/markets/{SLUG}/bbo"), json!({})).await;
        // Polymarket US recovers the parent event from its own snapshot.
        catalogues(&venues).await;

        let state = venues.state();
        for (venue, id) in [
            (Venue::Kalshi, KALSHI_ID),
            (Venue::Polymarket, SLUG),
            (Venue::PolymarketUs, SLUG),
        ] {
            let market = get_market(&state, venue, id).await.expect("a quote");
            assert_eq!(market.venue, venue);
            assert_eq!(market.ticker, id);
        }
    }

    #[tokio::test]
    async fn an_event_is_read_by_the_venue_it_was_asked_of() {
        let venues = Venues::start().await;
        mount(
            &venues.kalshi,
            "/events/KXFED-26SEP",
            json!({ "event": { "event_ticker": "KXFED-26SEP", "title": "Fed decision" } }),
        )
        .await;
        mount(
            &venues.gamma,
            "/events",
            json!([{ "slug": SLUG, "title": "Fed decision" }]),
        )
        .await;
        mount(
            &venues.us,
            &format!("/v1/events/slug/{SLUG}"),
            json!({ "event": { "slug": SLUG, "title": "Fed decision" } }),
        )
        .await;

        let state = venues.state();
        for (venue, id) in [
            (Venue::Kalshi, "KXFED-26SEP"),
            (Venue::Polymarket, SLUG),
            (Venue::PolymarketUs, SLUG),
        ] {
            let event = get_event(&state, venue, id).await.expect("an event");
            assert_eq!(event.venue, venue);
            assert_eq!(event.title, "Fed decision");
        }
    }

    #[tokio::test]
    async fn a_book_is_read_by_the_venue_it_was_asked_of() {
        let venues = Venues::start().await;
        mount(
            &venues.kalshi,
            &format!("/markets/{KALSHI_ID}/orderbook"),
            json!({ "orderbook_fp": { "yes": [["0.6900", "10.00"]], "no": [] } }),
        )
        .await;
        // The CLOB is keyed by token id, which the catalogue row states.
        mount(
            &venues.gamma,
            "/markets",
            json!([{ "slug": SLUG, "clobTokenIds": "[\"561528276\"]" }]),
        )
        .await;
        mount(
            &venues.clob,
            "/book",
            json!({ "bids": [{ "price": "0.73", "size": "100" }], "asks": [] }),
        )
        .await;
        mount(
            &venues.us,
            &format!("/v1/markets/{SLUG}/book"),
            json!({ "marketData": { "bids": [{ "px": "0.46", "qty": "5" }], "asks": [] } }),
        )
        .await;

        let state = venues.state();
        for (venue, id) in [
            (Venue::Kalshi, KALSHI_ID),
            (Venue::Polymarket, SLUG),
            (Venue::PolymarketUs, SLUG),
        ] {
            let book = get_order_book(&state, venue, id, 12).await.expect("a book");
            assert_eq!(book.venue, venue);
            assert_eq!(book.ticker, id);
        }
    }

    #[tokio::test]
    async fn a_tape_is_read_by_the_venue_it_was_asked_of() {
        let venues = Venues::start().await;
        mount(
            &venues.kalshi,
            "/markets/trades",
            json!({
                "trades": [{
                    "trade_id": "t1",
                    "ticker": KALSHI_ID,
                    "created_time": "2026-09-16T18:00:00Z",
                    "count": 5,
                    "yes_price_dollars": "0.70",
                    "no_price_dollars": "0.30",
                    "taker_side": "yes",
                }],
            }),
        )
        .await;
        mount(
            &venues.gamma,
            "/markets",
            json!([{ "slug": SLUG, "conditionId": "0xabc" }]),
        )
        .await;
        mount(
            &venues.data,
            "/trades",
            json!([{
                "transactionHash": "0xdead",
                "timestamp": 1_758_045_600u64,
                "price": "0.73",
                "size": "12",
                "side": "BUY",
                "outcomeIndex": 0,
            }]),
        )
        .await;

        let state = venues.state();
        for (venue, id) in [(Venue::Kalshi, KALSHI_ID), (Venue::Polymarket, SLUG)] {
            let tape = get_trades(&state, venue, id, 50).await.expect("a tape");
            assert_eq!(tape.trades.first().expect("one print").venue, venue);
        }
    }

    #[tokio::test]
    async fn candles_are_read_by_the_venue_they_were_asked_of() {
        let venues = Venues::start().await;
        mount(
            &venues.kalshi,
            &format!("/markets/{KALSHI_ID}"),
            json!({ "market": { "ticker": KALSHI_ID, "event_ticker": "KXFED-26SEP" } }),
        )
        .await;
        mount(
            &venues.kalshi,
            &format!("/series/KXFED/markets/{KALSHI_ID}/candlesticks"),
            json!({
                "candlesticks": [{
                    "end_period_ts": 1_758_045_600u64,
                    "price": { "open_dollars": "0.70", "close_dollars": "0.71",
                               "high_dollars": "0.72", "low_dollars": "0.69" },
                }],
            }),
        )
        .await;
        mount(
            &venues.gamma,
            "/markets",
            json!([{ "slug": SLUG, "clobTokenIds": "[\"561528276\"]", "seriesSlug": "fomc" }]),
        )
        .await;
        mount(
            &venues.clob,
            "/prices-history",
            json!({ "history": [{ "t": 1_758_045_600u64, "p": 0.73 }] }),
        )
        .await;

        let state = venues.state();
        for (venue, id) in [(Venue::Kalshi, KALSHI_ID), (Venue::Polymarket, SLUG)] {
            let chart = get_candles(
                &state,
                venue,
                id,
                CandleInterval::OneHour,
                1_758_042_000,
                1_758_049_200,
            )
            .await
            .expect("a chart");
            assert_eq!(chart.venue, venue);
            assert_eq!(chart.ticker, id);
            assert!(!chart.candles.is_empty());
        }
    }

    #[tokio::test]
    async fn series_are_listed_by_the_venue_they_were_asked_of() {
        let venues = Venues::start().await;
        mount(
            &venues.kalshi,
            "/series/",
            json!({ "series": [{ "ticker": "KXFED", "title": "Fed decision" }] }),
        )
        .await;
        mount(&venues.gamma, "/series", json!([{ "slug": "fomc" }])).await;
        mount(
            &venues.us,
            "/v1/series",
            json!({ "series": [{ "slug": "fed-decision" }] }),
        )
        .await;

        let state = venues.state();
        for venue in venue_ids() {
            let series = list_series(&state, venue, None).await.expect("a catalogue");
            assert_eq!(series.first().expect("one series").venue, venue);
        }
    }

    #[tokio::test]
    async fn search_is_answered_by_the_venue_it_was_asked_of() {
        let venues = Venues::start().await;
        catalogues(&venues).await;

        let state = venues.state();
        for venue in venue_ids() {
            let found = search(&state, venue, "fed", 25).await.expect("a result");
            assert_eq!(found.query, "fed");
            assert_eq!(found.hits.first().expect("one hit").event.venue, venue);
        }
    }

    #[tokio::test]
    async fn a_board_is_ranked_by_the_venue_it_was_asked_of() {
        let venues = Venues::start().await;
        catalogues(&venues).await;

        let state = venues.state();
        // Volume is the one board Polymarket US cannot serve, so it is asked for
        // the two that can and refused separately below.
        for venue in [Venue::Kalshi, Venue::Polymarket] {
            let board = top_markets(&state, venue, MoverSort::Volume, 25)
                .await
                .expect("a board");
            assert_eq!(board.first().expect("one row").venue, venue);
        }
    }

    #[tokio::test]
    async fn a_snapshot_is_taken_from_the_venue_it_was_asked_of() {
        let venues = Venues::start().await;
        catalogues(&venues).await;

        let state = venues.state();
        for venue in venue_ids() {
            let snapshot = corpus_snapshot(&state, venue).await.expect("a snapshot");
            assert_eq!(snapshot.venue, venue);
            assert_eq!(snapshot.events.len(), 1);
        }
    }

    /* ------------------------------------------------------- the category */

    #[tokio::test]
    async fn only_the_venue_that_honours_a_category_is_sent_one() {
        let venues = Venues::start().await;
        Mock::given(method("GET"))
            .and(path("/series/"))
            .and(query_param("category", "Economics"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "series": [] })))
            .expect(1)
            .mount(&venues.kalshi)
            .await;
        // Gamma answers the same list whatever it is asked, so being sent a
        // filter it drops would report a narrowed catalogue that is not one.
        Mock::given(method("GET"))
            .and(path("/series"))
            .and(query_param_is_missing("category"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
            .expect(1)
            .mount(&venues.gamma)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/series"))
            .and(query_param_is_missing("category"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "series": [] })))
            .expect(1)
            .mount(&venues.us)
            .await;

        let state = venues.state();
        for venue in venue_ids() {
            list_series(&state, venue, Some("Economics"))
                .await
                .expect("a catalogue");
        }
    }

    #[test]
    fn only_kalshi_declares_a_series_category_filter() {
        // Which arms are handed the category is a fact about the registry, not
        // about this module: the day a venue starts honouring one, this fires
        // and points at the `list_series` arm that still drops it.
        for venue in venue_ids() {
            assert_eq!(
                venue_info(venue).capabilities.series_category_filter,
                venue == Venue::Kalshi,
                "{venue}"
            );
        }
    }

    /* ---------------------------------------------------- declared refusals */

    #[tokio::test]
    async fn candles_are_refused_from_the_registry_not_from_the_upstream() {
        let venues = Venues::start().await;
        let state = venues.state();

        let err = get_candles(
            &state,
            Venue::PolymarketUs,
            SLUG,
            CandleInterval::OneHour,
            0,
            3600,
        )
        .await
        .expect_err("Polymarket US publishes no public candles");

        assert_eq!(err.code, codes::UNSUPPORTED);
        assert_eq!(
            err.message,
            venue_info(Venue::PolymarketUs).capabilities.note
        );
        // Nothing was dialled: the table answered.
        assert_eq!(venues.requests().await, 0);
    }

    #[tokio::test]
    async fn a_tape_is_refused_from_the_registry_not_from_the_upstream() {
        let venues = Venues::start().await;
        let state = venues.state();

        let err = get_trades(&state, Venue::PolymarketUs, SLUG, 50)
            .await
            .expect_err("Polymarket US publishes no public tape");

        assert_eq!(err.code, codes::UNSUPPORTED);
        assert_eq!(
            err.message,
            venue_info(Venue::PolymarketUs).capabilities.note
        );
        assert_eq!(venues.requests().await, 0);
    }

    #[tokio::test]
    async fn a_refusal_names_a_venue_that_can_answer() {
        let venues = Venues::start().await;
        let state = venues.state();

        let err = get_trades(&state, Venue::PolymarketUs, SLUG, 50)
            .await
            .expect_err("no public tape");
        let hint = err.hint.expect("somewhere to go");
        assert!(hint.contains("kx:"), "{hint}");
        assert!(hint.contains("pm:"), "{hint}");
        assert!(!hint.contains("pmus:"), "{hint}");
    }

    #[tokio::test]
    async fn a_board_the_venue_publishes_nothing_for_is_refused() {
        let venues = Venues::start().await;
        let state = venues.state();

        // Polymarket's catalogue carries no open interest, so the board would
        // come back empty rather than short.
        let err = top_markets(&state, Venue::Polymarket, MoverSort::OpenInterest, 25)
            .await
            .expect_err("no open interest published");
        assert_eq!(err.code, codes::UNSUPPORTED);
        assert_eq!(err.message, venue_info(Venue::Polymarket).capabilities.note);
        assert!(err.hint.expect("a hint").contains("open_interest"));

        // Polymarket US can be ranked on nothing at all.
        for sort in MoverSort::ALL {
            let err = top_markets(&state, Venue::PolymarketUs, *sort, 25)
                .await
                .expect_err("nothing to rank on");
            assert_eq!(err.code, codes::UNSUPPORTED, "{sort}");
        }

        assert_eq!(venues.requests().await, 0);
    }

    #[test]
    fn a_refusal_with_nowhere_to_go_promises_nothing() {
        // Kalshi is the only venue with an open interest board; refusing it
        // would leave the reader no alternative to be pointed at.
        let err = declined(Venue::PolymarketUs, "the open_interest board", &[]);
        assert_eq!(err.code, codes::UNSUPPORTED);
        assert_eq!(err.hint, None);
    }

    #[test]
    fn a_venue_with_no_note_still_says_which_verb_it_declined() {
        // Kalshi declares no gap, so it has no note. Nothing routes here today;
        // the fallback exists so a future registry row cannot print an empty
        // error into a panel.
        let err = declined(Venue::Kalshi, "candles", &[]);
        assert_eq!(err.message, "Kalshi does not serve candles");
    }

    #[test]
    fn a_refusal_lists_every_venue_that_can_answer() {
        assert_eq!(venue_list(&[]), None);
        assert_eq!(
            venue_list(&[Venue::Kalshi]).as_deref(),
            Some("Kalshi (kx:)")
        );
        assert_eq!(
            venue_list(&[Venue::Kalshi, Venue::Polymarket]).as_deref(),
            Some("Kalshi (kx:) and Polymarket International (pm:)")
        );
        assert_eq!(
            venue_list(&venue_ids()).as_deref(),
            Some("Kalshi (kx:), Polymarket International (pm:) and Polymarket US (pmus:)")
        );
    }

    #[test]
    fn the_refusals_read_the_same_table_the_client_does() {
        assert_eq!(
            serving(Capability::Candles),
            [Venue::Kalshi, Venue::Polymarket]
        );
        assert_eq!(
            serving(Capability::Trades),
            [Venue::Kalshi, Venue::Polymarket]
        );
        assert_eq!(ranking(MoverSort::OpenInterest), [Venue::Kalshi]);
        assert_eq!(
            ranking(MoverSort::Volume),
            [Venue::Kalshi, Venue::Polymarket]
        );
    }

    /* ------------------------------------------------------------ warm_all */

    #[tokio::test]
    async fn warming_every_catalogue_survives_a_venue_that_is_down() {
        let venues = Venues::start().await;
        catalogues(&venues).await;
        // Kalshi's crawl is left unmounted, so its fixture server 404s it.
        venues.kalshi.reset().await;

        let state = venues.state();
        warm_all(&state).await;

        // The two that answered are cached — a snapshot now costs no request.
        let before = venues.requests().await;
        for venue in [Venue::Polymarket, Venue::PolymarketUs] {
            let snapshot = corpus_snapshot(&state, venue).await.expect("warm");
            assert_eq!(snapshot.events.len(), 1);
        }
        assert_eq!(venues.requests().await, before);

        // The one that failed was not cached, so the next reader retries it.
        let err = corpus_snapshot(&state, Venue::Kalshi)
            .await
            .expect_err("still down");
        assert_eq!(err.code, codes::NOT_FOUND);
    }

    #[tokio::test]
    async fn warming_every_catalogue_asks_every_venue() {
        let venues = Venues::start().await;
        catalogues(&venues).await;

        let state = venues.state();
        warm_all(&state).await;

        for venue in venue_ids() {
            let snapshot = corpus_snapshot(&state, venue).await.expect("warm");
            assert_eq!(snapshot.venue, venue);
            assert_eq!(snapshot.events.len(), 1);
        }
    }

    #[tokio::test]
    async fn warming_one_venue_leaves_the_others_alone() {
        let venues = Venues::start().await;
        catalogues(&venues).await;
        let state = venues.state();

        warm_corpus(&state, Venue::Polymarket);
        // The cache is single-flight, so this either joins the crawl the spawn
        // started or starts the one it joins: the catalogue is read once
        // whichever of the two got there first.
        let snapshot = corpus_snapshot(&state, Venue::Polymarket)
            .await
            .expect("warm");

        assert_eq!(snapshot.venue, Venue::Polymarket);
        assert_eq!(seen(&venues.gamma).await, 1);
        assert_eq!(seen(&venues.kalshi).await, 0);
        assert_eq!(seen(&venues.us).await, 0);
    }

    #[tokio::test]
    async fn warming_survives_every_venue_being_down() {
        // Nothing is mounted at all: the crawl fails everywhere and `warm_all`
        // still returns, because startup has nobody to report a failure to.
        let venues = Venues::start().await;
        warm_all(&venues.state()).await;
    }
}
