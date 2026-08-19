//! Market news — from Alpaca's news API.
//!
//! The one feed here that needs a credential, for the simple reason that no
//! unauthenticated equity news feed exists that is legal to read and stable
//! enough to parse. Alpaca resells Benzinga's wire as JSON at
//! `data.alpaca.markets/v1beta1/news`, keyed to an Alpaca account; a free paper
//! account is enough, and the key never leaves this process — it is sent as a
//! request header to one host and is not echoed into any response.
//!
//! Two upstream defaults are deliberately overridden:
//!
//!  - **`start` defaults to the beginning of the current day.** For the whole
//!    tape that is fine; for `NEWS NVDA` it means a quiet morning, a weekend, or
//!    a thinly-covered ticker answers `200 OK` with an empty list, which reads
//!    as "no news exists" rather than "none today". The window is set
//!    explicitly, and the panel says how wide it is.
//!  - **`exclude_contentless` stays off.** A headline with no article body is
//!    still the information — halt notices and 8-K flags arrive that way — so
//!    dropping them would quietly hide the fastest-moving items on the wire.
//!
//! `include_content` stays off in the other direction: the article body is never
//! rendered and asking for it multiplies the payload for nothing.

use std::sync::LazyLock;
use std::time::Duration;

use regex::Regex;
use reqwest::Url;
use serde::{Deserialize, Deserializer};
use serde_json::Value;
use terminal_core::types::{NewsArticle, NewsFeed};
use time::format_description::BorrowedFormatItem;
use time::OffsetDateTime;

use crate::app::AppState;
use crate::cache::ttl;
use crate::config::{AlpacaKeys, Config};
use crate::error::{codes, Result, UpstreamError};
use crate::http::FetchOptions;
use crate::scrape;

/// Alpaca caps a page at 50 articles; asking for more is a 400, not a longer page.
pub const MAX_LIMIT: u32 = 50;
pub const MAX_SYMBOLS: usize = 20;
pub const MAX_DAYS: u32 = 90;
pub const DEFAULT_DAYS: u32 = 7;

/// The page size a caller that names none gets. The TypeScript spelled this as a
/// default argument *and* as the clamp's fallback; Rust has neither, so it is a
/// constant both call sites read.
pub const DEFAULT_LIMIT: u32 = 30;

/* ------------------------------------------------------------ credentials */

/// Whether the key pair this feed needs is set.
///
/// Alpaca's own SDKs read `APCA_API_KEY_ID` / `APCA_API_SECRET_KEY`, so anyone
/// who already uses Alpaca has those exported. The `ALPACA_`-prefixed names are
/// what this terminal documents; both are accepted, prefixed names first — and
/// that reading happens once, in [`Config`], rather than here.
#[must_use]
pub fn has_credentials(config: &Config) -> bool {
    config.alpaca.is_some()
}

const SETUP_HINT: &str = "Set ALPACA_API_KEY_ID and ALPACA_API_SECRET_KEY and restart the \
     server. Keys are free from alpaca.markets — a paper-trading account issues a pair, and \
     this feed only ever reads news with them.";

fn credentials(config: &Config) -> Result<&AlpacaKeys> {
    config.alpaca.as_ref().ok_or_else(|| {
        UpstreamError::not_configured("The news feed needs Alpaca API credentials")
            .with_hint(SETUP_HINT)
    })
}

/* -------------------------------------------------------------- arguments */

/// Symbols Alpaca will filter news by.
///
/// Equities can carry a dot (`BRK.B`) and crypto arrives as a pair (`BTCUSD`,
/// `BTC/USD`), so this is looser than a Kalshi ticker — but it is still a
/// whitelist, because these tokens are interpolated into an outbound query
/// string.
static SYMBOL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Z][A-Z0-9.\-/]{0,14}$").expect("SYMBOL is a valid regex"));

pub fn assert_symbols(raw: &str) -> Result<Vec<String>> {
    let mut out: Vec<String> = Vec::new();

    for token in raw.split(|ch: char| ch == ',' || ch.is_whitespace()) {
        let symbol = token.trim().to_uppercase();
        if symbol.is_empty() {
            continue;
        }

        if !SYMBOL.is_match(&symbol) {
            return Err(UpstreamError::bad_request(format!(
                "\"{token}\" is not a symbol the news feed can filter by"
            ))
            .with_hint(
                "Symbols look like NVDA, BRK.B or BTCUSD. `NEWS` with no symbol shows the \
                 whole wire.",
            ));
        }
        if out.contains(&symbol) {
            continue;
        }
        if out.len() == MAX_SYMBOLS {
            return Err(UpstreamError::bad_request(format!(
                "Too many symbols — the news feed takes at most {MAX_SYMBOLS}"
            )));
        }
        out.push(symbol);
    }

    Ok(out)
}

/// `clamp(Number(raw), min, max, fallback)`.
///
/// `None` is the original's non-finite reading — an absent query parameter, or
/// one that is not a number at all — and takes the fallback rather than the
/// minimum, so `?limit=abc` still shows the usual thirty headlines.
fn clamp(value: Option<i64>, min: u32, max: u32, fallback: u32) -> u32 {
    let Some(value) = value else {
        return fallback;
    };
    // Bounded into `[min, max]` first, so the narrowing cannot fail.
    u32::try_from(value.clamp(i64::from(min), i64::from(max))).unwrap_or(fallback)
}

/* ------------------------------------------------------------ normalising */

/// The fields this feed reads off one Alpaca article.
///
/// Every one is a [`Value`] because the original probes them rather than
/// trusting them (`typeof raw !== 'string'`): `id` arrives as an integer on one
/// row and a string on the next, `url` is `null` for a link-less item, and a
/// field the upstream renames or retypes should cost one column rather than the
/// whole request.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct RawArticle {
    pub id: Value,
    pub headline: Value,
    pub summary: Value,
    pub author: Value,
    pub source: Value,
    pub url: Value,
    pub created_at: Value,
    pub updated_at: Value,
    pub symbols: Value,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct RawNewsResponse {
    /// `None` when `news` was not an array — the one payload shape that ends the
    /// request rather than producing an empty feed.
    #[serde(deserialize_with = "array_or_nothing")]
    pub news: Option<Vec<RawArticle>>,
    pub next_page_token: Option<String>,
}

/// `Array.isArray(raw.news) ? raw.news : undefined`, as a deserialiser.
///
/// A row that is not an object at all becomes a default article, which the
/// empty-headline filter then drops — one malformed entry must not cost the
/// other forty-nine.
fn array_or_nothing<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<Vec<RawArticle>>, D::Error>
where
    D: Deserializer<'de>,
{
    let Value::Array(items) = Value::deserialize(deserializer)? else {
        return Ok(None);
    };
    Ok(Some(
        items
            .into_iter()
            .map(|item| serde_json::from_value(item).unwrap_or_default())
            .collect(),
    ))
}

/// Upstream text → plain text.
///
/// Wire copy arrives HTML-escaped (`AT&amp;T`, `Q3 &#39;26`) and a summary is cut
/// from the article body, so it can bring markup with it. Every panel renders
/// through `textContent`, which would print `AT&amp;T` literally — the escape has
/// to be undone somewhere, and doing it here means it is done once, on the
/// server, for every consumer of the feed.
#[must_use]
pub fn plain_text(raw: &str) -> String {
    scrape::strip_markup(raw)
}

/// [`plain_text`] of a field the wire may have omitted, nulled, or shipped as
/// something that is not a string.
#[must_use]
pub fn plain_text_of(raw: &Value) -> String {
    raw.as_str().map(plain_text).unwrap_or_default()
}

/// Article links, filtered to schemes a browser may safely open.
///
/// The headline is rendered as an anchor, and the href comes from an upstream
/// feed. A `javascript:` url in that position is one click from being a script,
/// so anything that is not http(s) is dropped and the headline renders as plain
/// text instead.
#[must_use]
pub fn safe_url(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    match Url::parse(trimmed) {
        Ok(url) if matches!(url.scheme(), "http" | "https") => url.to_string(),
        _ => String::new(),
    }
}

/// [`safe_url`] of a field that is routinely `null`.
#[must_use]
pub fn safe_url_of(raw: &Value) -> String {
    raw.as_str().map(safe_url).unwrap_or_default()
}

/// RFC-3339 → unix seconds. `0` for anything unparseable, never `NaN`.
#[must_use]
pub fn to_unix(raw: &str) -> i64 {
    if raw.is_empty() {
        return 0;
    }
    OffsetDateTime::parse(raw, &time::format_description::well_known::Rfc3339)
        .map_or(0, |at| at.unix_timestamp())
}

/// [`to_unix`] of a field that may be absent or may not be a string at all.
#[must_use]
pub fn to_unix_of(raw: &Value) -> i64 {
    raw.as_str().map_or(0, to_unix)
}

fn symbol_list(raw: &Value) -> Vec<String> {
    let Some(entries) = raw.as_array() else {
        return Vec::new();
    };
    let mut out: Vec<String> = Vec::new();
    for entry in entries {
        let Some(entry) = entry.as_str() else {
            continue;
        };
        let symbol = entry.trim().to_uppercase();
        if !symbol.is_empty() && !out.contains(&symbol) {
            out.push(symbol);
        }
    }
    out
}

/// `String(raw.id)`, with `undefined` and `null` reading as empty.
///
/// Alpaca's ids are integers wide enough to be worth keeping as text: they are
/// an identity, never an amount, and nothing here does arithmetic on one.
fn id_text(raw: &Value) -> String {
    match raw {
        Value::Null => String::new(),
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

#[must_use]
pub fn normalise_article(raw: &RawArticle) -> NewsArticle {
    let time = to_unix_of(&raw.created_at);
    let updated = to_unix_of(&raw.updated_at);

    NewsArticle {
        id: id_text(&raw.id),
        headline: plain_text_of(&raw.headline),
        summary: plain_text_of(&raw.summary),
        author: plain_text_of(&raw.author),
        publisher: plain_text_of(&raw.source),
        url: safe_url_of(&raw.url),
        time,
        // An unedited item still carries `updated_at`; falling back to `time`
        // keeps the column meaningful when the upstream omits it entirely.
        updated: if updated == 0 { time } else { updated },
        symbols: symbol_list(&raw.symbols),
    }
}

#[must_use]
pub fn normalise_feed(
    raw: &RawNewsResponse,
    symbols: &[String],
    days: u32,
    source_url: &str,
) -> NewsFeed {
    let mut articles: Vec<NewsArticle> = raw
        .news
        .as_deref()
        .unwrap_or_default()
        .iter()
        .map(normalise_article)
        // A row with no headline is not a story — it is a parse that went wrong,
        // and an empty line in a news tape is worse than a missing one.
        .filter(|article| !article.headline.is_empty())
        .collect();

    // Alpaca sorts by *edit* time, so a story revised an hour after publication
    // outranks one published since. Re-sort on publication so the top of the
    // panel is the newest news rather than the newest correction. The headline
    // breaks a tie, here by code point where the original used the runtime's
    // collation — it only decides between two stories filed in the same second.
    articles.sort_by(|a, b| {
        b.time
            .cmp(&a.time)
            .then_with(|| a.headline.cmp(&b.headline))
    });

    NewsFeed {
        symbols: symbols.to_vec(),
        days,
        articles,
        source: "Alpaca / Benzinga".to_string(),
        source_url: source_url.to_string(),
    }
}

/* ----------------------------------------------------------------- fetch */

/// `new Date(...).toISOString()`: always UTC, always three subsecond digits.
const ISO_8601: &[BorrowedFormatItem<'_>] = time::macros::format_description!(
    "[year]-[month]-[day]T[hour]:[minute]:[second].[subsecond digits:3]Z"
);

fn start_of(days: u32) -> String {
    let start = OffsetDateTime::now_utc() - Duration::from_secs(u64::from(days) * 86_400);
    start.format(ISO_8601).unwrap_or_default()
}

/// The outbound URL, with every parameter named rather than defaulted.
fn news_url(base: &str, symbols: &str, limit: u32, days: u32) -> Result<String> {
    let mut url = Url::parse(&format!("{base}/news")).map_err(|_| {
        UpstreamError::new(
            format!("`{base}` is not a usable Alpaca data base URL"),
            codes::INTERNAL,
        )
    })?;

    {
        let mut query = url.query_pairs_mut();
        if !symbols.is_empty() {
            query.append_pair("symbols", symbols);
        }
        query.append_pair("limit", &limit.to_string());
        query.append_pair("sort", "desc");
        query.append_pair("start", &start_of(days));
        query.append_pair("include_content", "false");
        query.append_pair("exclude_contentless", "false");
    }

    Ok(url.to_string())
}

/// A 401/403 here is a wrong key, not a blocked IP.
///
/// The shared fetch hints "this host may be blocking your IP" for a 403, which
/// is the right guess for the scraped feeds and exactly the wrong one for a
/// keyed API — it would send someone hunting a network problem they do not have.
fn explain_auth_failure(err: UpstreamError) -> UpstreamError {
    match err.status {
        Some(status @ (401 | 403)) => UpstreamError::new(
            "Alpaca rejected the API credentials",
            codes::BAD_CREDENTIALS,
        )
        .with_status(status)
        .with_hint(
            "The key pair was sent but not accepted. Check ALPACA_API_KEY_ID and \
                     ALPACA_API_SECRET_KEY are a matching pair, and that they are live keys \
                     rather than a paper key with the secret from another account.",
        ),
        _ => err,
    }
}

/// The query a snapshot is filed under.
///
/// Deliberately *not* keyed on the window's start instant. That moves every
/// call, and keying on it would mean never hitting the cache at all — so within
/// a TTL two callers a second apart share one snapshot, which is what the TTL is
/// for.
pub(crate) fn cache_key(symbols: &str, limit: u32, days: u32) -> String {
    format!("alpaca:news:{symbols}:{limit}:{days}")
}

/// The latest headlines, newest first.
///
/// `symbols` empty means the whole wire. `raw_limit` and `raw_days` are `None`
/// for a caller that named neither, which is the original's default argument.
pub async fn get_news(
    state: &AppState,
    symbols: &[String],
    raw_limit: Option<i64>,
    raw_days: Option<i64>,
) -> Result<NewsFeed> {
    let limit = clamp(raw_limit, 1, MAX_LIMIT, DEFAULT_LIMIT);
    let days = clamp(raw_days, 1, MAX_DAYS, DEFAULT_DAYS);
    let keys = credentials(state.config())?;

    let joined = symbols.join(",");
    let key = cache_key(&joined, limit, days);

    let feed = state
        .cache()
        .cached(&key, ttl::NEWS, || {
            fetch_news(state, symbols, &joined, limit, days, keys)
        })
        .await?;

    Ok((*feed).clone())
}

async fn fetch_news(
    state: &AppState,
    symbols: &[String],
    joined: &str,
    limit: u32,
    days: u32,
    keys: &AlpacaKeys,
) -> Result<NewsFeed> {
    let url = news_url(&state.config().alpaca_data_base, joined, limit, days)?;

    let payload: Value = state
        .http()
        .fetch_json(
            &url,
            FetchOptions::new()
                .timeout(Duration::from_secs(20))
                .retries(2)
                // Headers, never the query string: a secret in a query string
                // ends up in every access log between here and the origin.
                .header("APCA-API-KEY-ID", keys.key_id.clone())
                .header("APCA-API-SECRET-KEY", keys.secret_key.clone()),
        )
        .await
        .map_err(explain_auth_failure)?;

    let raw: RawNewsResponse = serde_json::from_value(payload).unwrap_or_default();
    if raw.news.is_none() {
        return Err(UpstreamError::new(
            "Alpaca returned an unexpected news payload",
            codes::BAD_UPSTREAM_BODY,
        ));
    }

    Ok(normalise_feed(&raw, symbols, days, &url))
}

#[cfg(test)]
mod tests {
    //! News feed normalisation tests.
    //!
    //! The fixture is a trimmed copy of a real `data.alpaca.markets/v1beta1/news`
    //! response, so the quirks under test are the wire's actual quirks: copy that
    //! arrives HTML-escaped, a summary cut out of the article body with its markup
    //! still attached, an item with no link at all, and — the one that silently
    //! misorders a news panel — a story published in the morning and *edited* in
    //! the evening, which the upstream's own sort ranks above everything published
    //! since.

    use super::*;
    use serde_json::json;

    /// The second row is filed in the morning and corrected at 21:10. Alpaca
    /// sorts on the edit, so it arrives *first* despite being seven hours older
    /// than the story above it.
    const FEED_JSON: &str = include_str!("fixtures/alpaca_news.json");

    fn feed_fixture() -> RawNewsResponse {
        serde_json::from_str(FEED_JSON).expect("the news fixture is valid JSON")
    }

    fn raw(value: Value) -> RawArticle {
        serde_json::from_value(value).expect("a raw article")
    }

    fn symbols(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_string()).collect()
    }

    /* ---------------------------------------------------------- plain text */

    #[test]
    fn plain_text_decodes_the_escapes_the_wire_ships_headlines_in() {
        // Rendered with textContent, an undecoded `&amp;` prints as five characters.
        assert_eq!(plain_text("AT&amp;T Names New CFO"), "AT&T Names New CFO");
        assert_eq!(plain_text("Tops Street&#39;s View"), "Tops Street's View");
    }

    #[test]
    fn plain_text_drops_markup_a_summary_brings_from_the_article_body() {
        assert_eq!(
            plain_text("<p>Shares of <b>NVIDIA</b> rose  in after-hours trade.</p>"),
            "Shares of NVIDIA rose in after-hours trade."
        );
    }

    #[test]
    fn plain_text_leaves_a_bare_ampersand_alone() {
        // `&G` is not an entity; a decoder that guesses would eat the character.
        assert_eq!(plain_text("P&G posts Q4 beat"), "P&G posts Q4 beat");
    }

    #[test]
    fn plain_text_does_not_leak_script_or_style_text_into_the_headline() {
        assert_eq!(plain_text("<script>alert(1)</script>Halted"), "Halted");
    }

    #[test]
    fn plain_text_answers_for_the_empty_and_the_absent() {
        assert_eq!(plain_text(""), "");
        assert_eq!(plain_text_of(&Value::Null), "");
        assert_eq!(plain_text_of(&json!(42)), "");
        // A field the upstream never sent at all.
        assert_eq!(plain_text_of(&raw(json!({})).headline), "");
    }

    /* ---------------------------------------------------------------- urls */

    #[test]
    fn safe_url_keeps_http_and_https_links() {
        assert_eq!(
            safe_url("https://www.benzinga.com/a"),
            "https://www.benzinga.com/a"
        );
        assert_eq!(safe_url("http://example.test/a"), "http://example.test/a");
    }

    #[test]
    fn safe_url_drops_anything_a_click_could_execute() {
        // The headline renders as an anchor, so this href is one click from running.
        assert_eq!(safe_url("javascript:alert(1)"), "");
        assert_eq!(safe_url("data:text/html,<script>alert(1)</script>"), "");
        assert_eq!(safe_url("file:///etc/passwd"), "");
    }

    #[test]
    fn safe_url_drops_what_is_not_a_url_at_all() {
        assert_eq!(safe_url_of(&Value::Null), "");
        assert_eq!(safe_url(""), "");
        assert_eq!(safe_url("   "), "");
        assert_eq!(safe_url("benzinga.com/a"), "");
    }

    /* ---------------------------------------------------------------- time */

    #[test]
    fn to_unix_reads_rfc_3339_into_unix_seconds() {
        assert_eq!(to_unix("2026-08-14T20:31:07Z"), 1_786_739_467);
        assert_eq!(to_unix("2026-08-14T20:31:07.482Z"), 1_786_739_467);
    }

    #[test]
    fn to_unix_returns_0_rather_than_nan_for_junk() {
        assert_eq!(to_unix("not a date"), 0);
        assert_eq!(to_unix_of(&Value::Null), 0);
        // A number is not a string, however much it looks like an instant.
        assert_eq!(to_unix_of(&json!(1_786_825_867)), 0);
    }

    /* ------------------------------------------------------------- symbols */

    #[test]
    fn assert_symbols_splits_on_commas_and_whitespace_and_upper_cases() {
        assert_eq!(
            assert_symbols("nvda,amd msft").unwrap(),
            symbols(&["NVDA", "AMD", "MSFT"])
        );
    }

    #[test]
    fn assert_symbols_accepts_the_shapes_the_wire_actually_tags() {
        assert_eq!(
            assert_symbols("BRK.B BTCUSD BTC/USD").unwrap(),
            symbols(&["BRK.B", "BTCUSD", "BTC/USD"])
        );
    }

    #[test]
    fn assert_symbols_de_duplicates() {
        assert_eq!(
            assert_symbols("AAPL aapl AAPL").unwrap(),
            symbols(&["AAPL"])
        );
    }

    #[test]
    fn assert_symbols_reads_no_symbols_as_the_whole_wire_not_as_an_error() {
        assert_eq!(assert_symbols("").unwrap(), Vec::<String>::new());
        assert_eq!(assert_symbols("  ,  ").unwrap(), Vec::<String>::new());
    }

    #[test]
    fn assert_symbols_refuses_anything_that_is_not_a_symbol() {
        // These tokens end up in an outbound query string, so the whitelist is
        // the boundary — not a formatting preference.
        for bad in ["AAPL&limit=9999", "../../etc", "9NVDA"] {
            let err = assert_symbols(bad).unwrap_err();
            assert_eq!(err.code, codes::BAD_REQUEST, "{bad}");
            assert!(err.message.contains("not a symbol"), "{}", err.message);
        }
    }

    #[test]
    fn assert_symbols_caps_how_many_symbols_one_request_may_filter_on() {
        let many: Vec<String> = (0..=MAX_SYMBOLS).map(|i| format!("SYM{i}")).collect();
        let err = assert_symbols(&many.join(",")).unwrap_err();
        assert_eq!(err.code, codes::BAD_REQUEST);
        assert!(err.message.contains("at most"), "{}", err.message);
    }

    /* --------------------------------------------------------- normalising */

    #[test]
    fn normalise_article_maps_one_story_onto_the_wire_contract() {
        let fixture = feed_fixture();
        let article = normalise_article(&fixture.news.as_ref().unwrap()[0]);

        assert_eq!(
            serde_json::to_value(&article).unwrap(),
            json!({
                "id": "47374123",
                "headline": "Nvidia Q3 Beat: Data Center Revenue Tops Street's $30B View",
                "summary": "Shares of NVIDIA rose in after-hours trade.",
                "author": "Benzinga Newsdesk",
                "publisher": "benzinga",
                "url": "https://www.benzinga.com/news/26/08/47374123/nvidia-q3",
                "time": 1_786_739_467,
                "updated": 1_786_739_621,
                "symbols": ["NVDA", "AMD"],
            })
        );
    }

    #[test]
    fn normalise_article_keeps_a_numeric_id_as_text() {
        // 47374123 is small, but the ids are wide enough to be worth never doing
        // arithmetic on, and the client compares them as strings.
        assert_eq!(
            normalise_article(&raw(json!({ "id": 47374123 }))).id,
            "47374123"
        );
        assert_eq!(
            normalise_article(&raw(json!({ "id": "47370001" }))).id,
            "47370001"
        );
        assert_eq!(normalise_article(&raw(json!({}))).id, "");
    }

    #[test]
    fn normalise_article_falls_back_to_the_publication_time_when_there_is_no_edit_time() {
        let article = normalise_article(&raw(json!({
            "headline": "x",
            "created_at": "2026-08-14T13:02:00Z",
        })));
        assert_eq!(article.updated, article.time);
        assert_eq!(article.time, 1_786_712_520);
    }

    #[test]
    fn normalise_article_leaves_a_link_less_item_with_an_empty_url_rather_than_a_broken_anchor() {
        let fixture = feed_fixture();
        assert_eq!(
            normalise_article(&fixture.news.as_ref().unwrap()[1]).url,
            ""
        );
    }

    #[test]
    fn normalise_feed_orders_on_publication_not_on_the_last_edit() {
        // Alpaca sorts by `updated_at`, which would put a seven-hour-old story
        // corrected at 21:10 above one published at 20:31.
        let feed = normalise_feed(
            &feed_fixture(),
            &symbols(&["NVDA"]),
            7,
            "https://data.alpaca.markets/v1beta1/news",
        );

        assert_eq!(
            feed.articles
                .iter()
                .map(|a| a.headline.as_str())
                .collect::<Vec<_>>(),
            vec![
                "Nvidia Q3 Beat: Data Center Revenue Tops Street's $30B View",
                "AT&T Names New CFO",
            ]
        );
    }

    #[test]
    fn normalise_feed_drops_a_row_with_no_headline_instead_of_printing_a_blank_line() {
        let feed = normalise_feed(&feed_fixture(), &symbols(&["NVDA"]), 7, "u");
        assert_eq!(feed.articles.len(), 2);
        assert!(feed.articles.iter().all(|a| !a.headline.is_empty()));
    }

    #[test]
    fn normalise_feed_carries_the_query_back_so_the_panel_can_state_its_own_scope() {
        let feed = normalise_feed(
            &feed_fixture(),
            &symbols(&["NVDA"]),
            7,
            "https://data.alpaca.markets/v1beta1/news",
        );
        assert_eq!(feed.symbols, symbols(&["NVDA"]));
        assert_eq!(feed.days, 7);
        assert_eq!(feed.source, "Alpaca / Benzinga");
        assert_eq!(feed.source_url, "https://data.alpaca.markets/v1beta1/news");
    }

    #[test]
    fn normalise_feed_reads_a_payload_with_no_stories_as_an_empty_wire_not_a_failure() {
        let empty: RawNewsResponse = serde_json::from_value(json!({ "news": [] })).unwrap();
        assert!(normalise_feed(&empty, &[], 7, "u").articles.is_empty());
    }

    #[test]
    fn a_news_field_that_is_not_an_array_is_the_one_shape_that_ends_the_request() {
        // Everything else degrades; this cannot, because there is nothing to read.
        let raw: RawNewsResponse = serde_json::from_value(json!({ "news": "soon" })).unwrap();
        assert!(raw.news.is_none());
        let missing: RawNewsResponse = serde_json::from_value(json!({})).unwrap();
        assert!(missing.news.is_none());
    }

    /* ------------------------------------------------------------ the call */

    #[test]
    fn clamp_takes_the_fallback_for_a_figure_that_is_not_one() {
        assert_eq!(clamp(None, 1, MAX_LIMIT, DEFAULT_LIMIT), 30);
        assert_eq!(clamp(Some(5000), 1, MAX_LIMIT, DEFAULT_LIMIT), 50);
        assert_eq!(clamp(Some(-4), 1, MAX_LIMIT, DEFAULT_LIMIT), 1);
        assert_eq!(clamp(Some(40), 1, MAX_LIMIT, DEFAULT_LIMIT), 40);
        assert_eq!(clamp(Some(400), 1, MAX_DAYS, DEFAULT_DAYS), 90);
    }

    #[test]
    fn the_start_instant_is_the_iso_string_the_upstream_reads() {
        let start = start_of(DEFAULT_DAYS);
        let parsed = OffsetDateTime::parse(&start, &time::format_description::well_known::Rfc3339)
            .expect("start is RFC-3339");
        let expected = OffsetDateTime::now_utc().unix_timestamp() - 7 * 86_400;
        assert!(
            (parsed.unix_timestamp() - expected).abs() < 60,
            "start was {start}"
        );
        assert!(start.ends_with('Z'), "start was {start}");
    }

    #[test]
    fn the_query_names_every_parameter_rather_than_defaulting_it() {
        let url = news_url("http://127.0.0.1:9/v1beta1", "NVDA,AMD", 40, 14).unwrap();
        let parsed = Url::parse(&url).unwrap();
        let query: Vec<(String, String)> = parsed
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();

        assert_eq!(parsed.path(), "/v1beta1/news");
        assert!(query.contains(&("symbols".into(), "NVDA,AMD".into())));
        assert!(query.contains(&("limit".into(), "40".into())));
        assert!(query.contains(&("sort".into(), "desc".into())));
        assert!(query.contains(&("include_content".into(), "false".into())));
        assert!(query.contains(&("exclude_contentless".into(), "false".into())));

        // No symbol at all is the whole wire, not an empty filter.
        let all = Url::parse(&news_url("http://127.0.0.1:9/v1beta1", "", 30, 7).unwrap()).unwrap();
        assert!(all.query_pairs().all(|(k, _)| k != "symbols"));
    }

    #[test]
    fn the_cache_key_leaves_the_moving_start_instant_out() {
        // Keying on `start` would move it every call, and nothing would ever hit.
        assert_eq!(cache_key("NVDA,AMD", 30, 7), "alpaca:news:NVDA,AMD:30:7");
        assert_eq!(cache_key("", 50, 90), "alpaca:news::50:90");
        // The three things that do change the answer each change the key.
        assert_ne!(cache_key("NVDA", 30, 7), cache_key("AMD", 30, 7));
        assert_ne!(cache_key("NVDA", 30, 7), cache_key("NVDA", 40, 7));
        assert_ne!(cache_key("NVDA", 30, 7), cache_key("NVDA", 30, 14));
    }

    #[test]
    fn has_credentials_reads_the_pair_config_resolved() {
        let mut config = Config::default();
        assert!(!has_credentials(&config));

        let err = credentials(&config).unwrap_err();
        assert_eq!(err.code, codes::NOT_CONFIGURED);
        // The hint is shown verbatim in the panel, so it has to name the fix.
        assert!(err.hint.unwrap().contains("ALPACA_API_KEY_ID"));

        config.alpaca = Some(AlpacaKeys {
            key_id: "PKTESTKEYID".into(),
            secret_key: "testsecret".into(),
        });
        assert!(has_credentials(&config));
        assert_eq!(credentials(&config).unwrap().key_id, "PKTESTKEYID");
    }

    #[test]
    fn a_rejected_key_is_a_rejected_key_not_a_blocked_ip() {
        for status in [401, 403] {
            let err = explain_auth_failure(
                UpstreamError::new(
                    "data.alpaca.markets returned HTTP 401",
                    codes::UPSTREAM_STATUS,
                )
                .with_status(status)
                .with_hint("it may be blocking this IP."),
            );
            assert_eq!(err.code, codes::BAD_CREDENTIALS);
            assert_eq!(err.status, Some(status));
            assert!(err.hint.unwrap().contains("ALPACA_API_SECRET_KEY"));
        }

        // Everything else is passed through untouched — a 500 is not a key problem.
        let other = UpstreamError::timeout("data.alpaca.markets did not respond in time");
        assert_eq!(explain_auth_failure(other.clone()), other);
    }
}
