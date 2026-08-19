//! Dump every venue's crawled catalogue to JSON, for benchmarking and fixtures.
//!
//! Not part of the product: it exists so the matcher can be measured and
//! regression-tested against the catalogues it actually has to cope with,
//! rather than against titles someone made up.
//!
//! `cargo run --release --example dump_corpus -- <out-dir>`

use terminal_core::venue::venue_ids;
use terminal_server::app::AppState;
use terminal_server::config::Config;
use terminal_server::sources::venues::corpus_snapshot;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let out = std::env::args().nth(1).unwrap_or_else(|| ".".to_string());
    let state = AppState::new(Config::from_env());

    for venue in venue_ids() {
        match corpus_snapshot(&state, venue).await {
            Ok(corpus) => {
                let path = format!("{out}/{}.json", venue.as_str());
                std::fs::write(&path, serde_json::to_vec(&corpus.events)?)?;
                eprintln!(
                    "{}: {} events, {} markets, truncated={} -> {path}",
                    venue.as_str(),
                    corpus.events.len(),
                    corpus.markets.len(),
                    corpus.truncated
                );
            }
            Err(err) => eprintln!("{}: unavailable ({err})", venue.as_str()),
        }
    }

    Ok(())
}
