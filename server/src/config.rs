//! Everything the server reads from its environment.
//!
//! All of it is optional — the terminal runs with nothing set. Two variables
//! hold credentials (FRED's optional API fallback, and the one news feed that
//! needs a key); the rest name upstream hosts, and exist mainly so a test can
//! point a source at a fixture server instead of the internet.
//!
//! [`Config`] is a plain value, built once at startup and passed down through
//! [`crate::app::AppState`]. Nothing below this module reads `std::env`, which
//! is what lets an integration test construct a config directly rather than
//! mutating process-global state and hoping the tests do not interleave.

use std::env;
use std::net::{IpAddr, Ipv4Addr};
use std::path::PathBuf;

/// The Alpaca credential pair. Present or absent together — a half-set pair is
/// treated as unset, because it fails at the wire with a confusing 401.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlpacaKeys {
    pub key_id: String,
    pub secret_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub host: IpAddr,
    pub port: u16,
    /// API calls allowed per client IP per minute.
    pub rate_limit: u32,
    /// Whether `X-Forwarded-For` and `X-Real-IP` may name the client.
    ///
    /// Off by default, and that default is the security-relevant half: with it
    /// on and nothing in front of the server, a caller picks its own rate-limit
    /// bucket by varying the header, which is the whole of the limit defeated.
    /// Turn it on only where a proxy you control actually rewrites the chain.
    pub trust_proxy: bool,

    /// Enables the FRED API arm of the provider chain. The scrape is always
    /// tried first; this is the fallback for hosts the scrape cannot reach.
    pub fred_api_key: Option<String>,
    /// The only feed here that requires a credential.
    pub alpaca: Option<AlpacaKeys>,

    // Upstream bases. Overridable so a test can serve a fixture instead.
    pub kalshi_api_base: String,
    pub polymarket_gamma_base: String,
    pub polymarket_clob_base: String,
    pub polymarket_data_base: String,
    pub polymarket_us_api_base: String,
    pub fred_web_base: String,
    pub fred_api_base: String,
    pub alpaca_data_base: String,
    pub yahoo_api_base: String,
    pub nasdaq_api_base: String,
    pub coinbase_api_base: String,
    pub billboard_base: String,
    pub boxoffice_base: String,
    pub netflix_base: String,
    pub rottentomatoes_base: String,
    pub kworb_base: String,
    pub steamcharts_base: String,
    pub steam_api_base: String,
    pub steam_store_base: String,
    pub tvmaze_api_base: String,
    pub wikidata_sparql_base: String,
    pub google_trends_base: String,
    pub itunes_api_base: String,
    pub apple_rss_base: String,

    /// The built client, served as static files with an SPA fallback. Unset in
    /// development, where Vite serves the client and proxies `/api` here.
    pub client_dir: Option<PathBuf>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            host: IpAddr::V4(Ipv4Addr::LOCALHOST),
            port: 8787,
            rate_limit: 600,
            trust_proxy: false,
            fred_api_key: None,
            alpaca: None,
            kalshi_api_base: defaults::KALSHI.into(),
            polymarket_gamma_base: defaults::POLYMARKET_GAMMA.into(),
            polymarket_clob_base: defaults::POLYMARKET_CLOB.into(),
            polymarket_data_base: defaults::POLYMARKET_DATA.into(),
            polymarket_us_api_base: defaults::POLYMARKET_US.into(),
            fred_web_base: defaults::FRED_WEB.into(),
            fred_api_base: defaults::FRED_API.into(),
            alpaca_data_base: defaults::ALPACA_DATA.into(),
            yahoo_api_base: defaults::YAHOO.into(),
            nasdaq_api_base: defaults::NASDAQ.into(),
            coinbase_api_base: defaults::COINBASE.into(),
            billboard_base: defaults::BILLBOARD.into(),
            boxoffice_base: defaults::BOXOFFICE.into(),
            netflix_base: defaults::NETFLIX.into(),
            rottentomatoes_base: defaults::ROTTEN_TOMATOES.into(),
            kworb_base: defaults::KWORB.into(),
            steamcharts_base: defaults::STEAMCHARTS.into(),
            steam_api_base: defaults::STEAM_API.into(),
            steam_store_base: defaults::STEAM_STORE.into(),
            tvmaze_api_base: defaults::TVMAZE.into(),
            wikidata_sparql_base: defaults::WIKIDATA_SPARQL.into(),
            google_trends_base: defaults::GOOGLE_TRENDS.into(),
            itunes_api_base: defaults::ITUNES.into(),
            apple_rss_base: defaults::APPLE_RSS.into(),
            client_dir: None,
        }
    }
}

/// What counts as "on" in an environment variable.
///
/// Deliberately generous about spelling and deliberately strict about default:
/// anything unrecognised reads as off, because the one setting this gates is
/// only safe when someone meant it.
fn truthy(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

impl Config {
    /// Read the environment, falling back to the defaults for anything unset or
    /// unparseable. A malformed `PORT` is not worth refusing to start over.
    pub fn from_env() -> Self {
        let defaults = Config::default();

        Self {
            host: var("HOST")
                .and_then(|v| v.parse().ok())
                .unwrap_or(defaults.host),
            port: var("PORT")
                .and_then(|v| v.parse().ok())
                .unwrap_or(defaults.port),
            rate_limit: var("RATE_LIMIT")
                .and_then(|v| v.parse().ok())
                .unwrap_or(defaults.rate_limit),
            trust_proxy: var("TRUST_PROXY")
                .map(|v| truthy(&v))
                .unwrap_or(defaults.trust_proxy),

            fred_api_key: var("FRED_API_KEY"),
            alpaca: alpaca_from_env(),

            kalshi_api_base: base("KALSHI_API_BASE", defaults::KALSHI),
            polymarket_gamma_base: base("POLYMARKET_GAMMA_BASE", defaults::POLYMARKET_GAMMA),
            polymarket_clob_base: base("POLYMARKET_CLOB_BASE", defaults::POLYMARKET_CLOB),
            polymarket_data_base: base("POLYMARKET_DATA_BASE", defaults::POLYMARKET_DATA),
            polymarket_us_api_base: base("POLYMARKET_US_API_BASE", defaults::POLYMARKET_US),
            fred_web_base: base("FRED_WEB_BASE", defaults::FRED_WEB),
            fred_api_base: base("FRED_API_BASE", defaults::FRED_API),
            alpaca_data_base: base("ALPACA_DATA_BASE", defaults::ALPACA_DATA),
            yahoo_api_base: base("YAHOO_API_BASE", defaults::YAHOO),
            nasdaq_api_base: base("NASDAQ_API_BASE", defaults::NASDAQ),
            coinbase_api_base: base("COINBASE_API_BASE", defaults::COINBASE),
            billboard_base: base("BILLBOARD_BASE", defaults::BILLBOARD),
            boxoffice_base: base("BOXOFFICE_BASE", defaults::BOXOFFICE),
            netflix_base: base("NETFLIX_BASE", defaults::NETFLIX),
            rottentomatoes_base: base("ROTTENTOMATOES_BASE", defaults::ROTTEN_TOMATOES),
            kworb_base: base("KWORB_BASE", defaults::KWORB),
            steamcharts_base: base("STEAMCHARTS_BASE", defaults::STEAMCHARTS),
            steam_api_base: base("STEAM_API_BASE", defaults::STEAM_API),
            steam_store_base: base("STEAM_STORE_BASE", defaults::STEAM_STORE),
            tvmaze_api_base: base("TVMAZE_API_BASE", defaults::TVMAZE),
            wikidata_sparql_base: base("WIKIDATA_SPARQL_BASE", defaults::WIKIDATA_SPARQL),
            google_trends_base: base("GOOGLE_TRENDS_BASE", defaults::GOOGLE_TRENDS),
            itunes_api_base: base("ITUNES_API_BASE", defaults::ITUNES),
            apple_rss_base: base("APPLE_RSS_BASE", defaults::APPLE_RSS),

            client_dir: var("CLIENT_DIR").map(PathBuf::from),
        }
    }

    pub fn has_fred_key(&self) -> bool {
        self.fred_api_key.is_some()
    }

    pub fn has_alpaca_keys(&self) -> bool {
        self.alpaca.is_some()
    }
}

/// A set, non-empty environment variable.
fn var(name: &str) -> Option<String> {
    match env::var(name) {
        Ok(value) if !value.trim().is_empty() => Some(value.trim().to_string()),
        _ => None,
    }
}

/// A base URL, with any trailing slash removed so callers can always write
/// `format!("{base}/path")`.
fn base(name: &str, fallback: &str) -> String {
    let raw = var(name).unwrap_or_else(|| fallback.to_string());
    raw.trim_end_matches('/').to_string()
}

/// Alpaca's own SDK names are read too, so an environment already set up for
/// their tooling works unchanged.
fn alpaca_from_env() -> Option<AlpacaKeys> {
    let key_id = var("ALPACA_API_KEY_ID").or_else(|| var("APCA_API_KEY_ID"))?;
    let secret_key = var("ALPACA_API_SECRET_KEY").or_else(|| var("APCA_API_SECRET_KEY"))?;
    Some(AlpacaKeys { key_id, secret_key })
}

/// The public hosts each source reads, when nothing overrides them.
pub mod defaults {
    pub const KALSHI: &str = "https://api.elections.kalshi.com/trade-api/v2";
    pub const POLYMARKET_GAMMA: &str = "https://gamma-api.polymarket.com";
    pub const POLYMARKET_CLOB: &str = "https://clob.polymarket.com";
    pub const POLYMARKET_DATA: &str = "https://data-api.polymarket.com";
    pub const POLYMARKET_US: &str = "https://gateway.polymarket.us";
    pub const FRED_WEB: &str = "https://fred.stlouisfed.org";
    pub const FRED_API: &str = "https://api.stlouisfed.org/fred";
    pub const ALPACA_DATA: &str = "https://data.alpaca.markets/v1beta1";
    pub const YAHOO: &str = "https://query1.finance.yahoo.com";
    pub const NASDAQ: &str = "https://api.nasdaq.com";
    pub const COINBASE: &str = "https://api.exchange.coinbase.com";
    pub const BILLBOARD: &str = "https://www.billboard.com";
    pub const BOXOFFICE: &str = "https://www.boxofficemojo.com";
    pub const NETFLIX: &str = "https://www.netflix.com";
    pub const ROTTEN_TOMATOES: &str = "https://www.rottentomatoes.com";
    pub const KWORB: &str = "https://kworb.net";
    pub const STEAMCHARTS: &str = "https://steamcharts.com";
    pub const STEAM_API: &str = "https://api.steampowered.com";
    pub const STEAM_STORE: &str = "https://store.steampowered.com";
    pub const TVMAZE: &str = "https://api.tvmaze.com";

    /// Wikidata's SPARQL endpoint, not `wikidata.org/w/api.php`: the MediaWiki
    /// API rate-limits shared egress hard enough to be unusable from a
    /// container. Entity search still runs against MediaWiki, but server-side
    /// through WDQS's own `wikibase:mwapi` service.
    pub const WIKIDATA_SPARQL: &str = "https://query.wikidata.org/sparql";
    /// The RSS surface. The JSON behind trends.google.com is a batchexecute RPC
    /// needing a session token; the feed is a stable public document.
    pub const GOOGLE_TRENDS: &str = "https://trends.google.com";
    pub const ITUNES: &str = "https://itunes.apple.com";
    pub const APPLE_RSS: &str = "https://rss.marketingtools.apple.com/api/v2";
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_need_no_environment_at_all() {
        let config = Config::default();
        assert_eq!(config.port, 8787);
        assert_eq!(config.host, IpAddr::V4(Ipv4Addr::LOCALHOST));
        assert_eq!(config.rate_limit, 600);
        assert!(!config.has_fred_key());
        assert!(!config.has_alpaca_keys());
        assert!(config.client_dir.is_none());
    }

    #[test]
    fn every_base_is_stored_without_a_trailing_slash() {
        // Sources build URLs as `{base}/path`; a stored trailing slash would
        // produce `//path`, which some upstreams 404.
        let config = Config::default();
        for base in [
            &config.kalshi_api_base,
            &config.polymarket_gamma_base,
            &config.fred_web_base,
            &config.alpaca_data_base,
            &config.tvmaze_api_base,
        ] {
            assert!(!base.ends_with('/'), "{base}");
        }
    }

    #[test]
    fn trims_a_trailing_slash_off_an_override() {
        // `base()` is what a fixture server's URI goes through, and wiremock's
        // `uri()` has no trailing slash — but a hand-set env var often does.
        assert_eq!(
            super::base("DEFINITELY_UNSET_VARIABLE_NAME", "https://example.com/"),
            "https://example.com"
        );
    }

    #[test]
    fn a_half_set_alpaca_pair_counts_as_unset() {
        // One key without the other fails at the wire as a confusing 401; the
        // useful answer is "not configured", with the setup hint.
        let config = Config {
            alpaca: None,
            ..Config::default()
        };
        assert!(!config.has_alpaca_keys());
    }

    #[test]
    fn a_test_can_point_a_source_at_a_fixture_without_touching_the_environment() {
        let config = Config {
            fred_web_base: "http://127.0.0.1:9999".into(),
            ..Config::default()
        };
        assert_eq!(config.fred_web_base, "http://127.0.0.1:9999");
        // Everything else keeps its real default.
        assert_eq!(config.kalshi_api_base, defaults::KALSHI);
    }
}
