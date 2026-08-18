//! Entry point: read the environment, build the router, serve.
//!
//! The server exists for three reasons the browser cannot handle on its own:
//! none of the upstreams allow cross-origin reads; several of them serve HTML
//! that has to be parsed somewhere; and caching with request coalescing is what
//! turns N polling panels into one upstream call. In production it also serves
//! the built client, so the whole terminal is one process on one port.

use std::net::SocketAddr;

use terminal_server::app::AppState;
use terminal_server::config::Config;
use tokio::net::TcpListener;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();

    let config = Config::from_env();
    let addr = SocketAddr::new(config.host, config.port);
    let state = AppState::new(config);

    let listener = TcpListener::bind(addr).await?;
    let bound = listener.local_addr()?;
    announce(&state, bound);

    terminal_server::sources::warm(state.clone());

    let app =
        terminal_server::build_router(state).into_make_service_with_connect_info::<SocketAddr>();

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    Ok(())
}

fn init_tracing() {
    use tracing_subscriber::{fmt, EnvFilter};

    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("terminal_server=info,warn"));

    fmt().with_env_filter(filter).with_target(false).init();
}

/// The startup banner. Lists what is mounted where, and — more usefully — says
/// plainly which optional feeds are switched off for want of a credential,
/// rather than letting the operator discover it from a 503 later.
fn announce(state: &AppState, addr: SocketAddr) {
    let config = state.config();

    println!("PREDICTION TERMINAL api  http://{addr}");
    println!("  kalshi     /api/kalshi/{{markets,events,search,top,series}}");
    println!(
        "  venues     /api/venue/{{kalshi,polymarket,polymarket-us}}/{{markets,events,search,top}}"
    );
    println!("  xvenue     /api/xv/{{series,compare}}");
    println!("  spot       /api/spot/{{stock,crypto}}/:symbol[/candles]");
    println!("  implied    /api/implied/{{underlyings,candidates,series}}");
    println!("  fred       /api/fred/{{series/:id,search}}");
    println!("  billboard  /api/billboard/{{charts,chart/:slug}}");
    println!("  ent        /api/ent/{{markets,rt,netflix,spotify,youtube,boxoffice,steam,tv}}");
    println!("  news       /api/news?symbols=&limit=&days=");

    if !config.has_fred_key() {
        println!("  note: FRED_API_KEY unset — FRED uses scraping only (no fallback).");
    }
    if !config.has_alpaca_keys() {
        println!("  note: ALPACA_API_KEY_ID/SECRET unset — NEWS is unavailable until they are.");
    }
    match &config.client_dir {
        Some(dir) => println!("  client: {}", dir.display()),
        None => println!("  client: not served (CLIENT_DIR unset — use the Vite dev server)"),
    }
}

/// Stop on Ctrl-C, or on SIGTERM so a container stops promptly rather than
/// waiting out its kill timeout.
async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }

    tracing::info!("shutting down");
}
