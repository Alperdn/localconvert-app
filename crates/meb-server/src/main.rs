//! `meb-server` - local development entry point.
//!
//! ```text
//! cargo run --manifest-path crates/meb-server/Cargo.toml
//! ```
//! Environment: `MEB_BIND` (loopback only, default 127.0.0.1:8787),
//! `MEB_DATA_ROOT`, `MEB_MAX_UPLOAD_MB`, `MEB_SECURE_COOKIE`, `RUST_LOG`.
//! The frontend runs with `npm run dev:web`; Vite proxies `/api` here so the
//! browser sees a single origin.

use meb_server::{janitor, App, Config};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("meb_server=info")),
        )
        .init();

    let config = match Config::from_env() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("configuration error: {e}");
            std::process::exit(2);
        }
    };
    let bind = config.bind;

    let app = match App::new(config) {
        Ok(app) => app,
        Err(e) => {
            eprintln!("startup error: {e}");
            std::process::exit(1);
        }
    };
    tracing::info!(data_root = %app.data_root().display(), "storage ready");

    let janitor = janitor::spawn(app.state.clone());
    let listener = match tokio::net::TcpListener::bind(bind).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("could not bind {bind}: {e}");
            std::process::exit(1);
        }
    };
    tracing::info!(%bind, "meb-server listening (development mode, no authentication)");

    let served = axum::serve(listener, app.router())
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await;
    if let Err(e) = served {
        tracing::error!(error = %e, "server error");
    }

    tracing::info!("shutting down: cancelling active jobs");
    app.cancel_all();
    janitor.abort();
    // Give cancelled engines a moment to reach their next checkpoint. Any
    // workspace left behind is purged on the next start (storage::open).
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
}
