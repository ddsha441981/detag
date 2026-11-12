//! detag-api binary: load settings, serve the router from the lib.

use detag_api::{build_router, metrics::metrics_router};
use detag_core::config::{warn_unknown_feature_flags, Settings};
use detag_core::db::{connect_lazy, PoolConfig};
use detag_core::metrics::Metrics;
use detag_core::shutdown::{shutdown_signal, SHUTDOWN_TIMEOUT};
use detag_core::telemetry::{init_tracing, shutdown_telemetry};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::Notify;

#[tokio::main]
async fn main() {
    // Fail boot fast on invalid configuration, naming the bad PF_* var.
    let settings = Settings::load().unwrap_or_else(|err| {
        eprintln!("detag-api: invalid configuration:\n{err}");
        std::process::exit(1);
    });
    // Tracing first, so the warn below (and everything after) is logged.
    init_tracing(&settings, "detag-api");
    // Unknown PF_FEATURE_* flags warn (a typo must not silently do nothing).
    warn_unknown_feature_flags();
    let metrics = Metrics::new();
    // Postgres pool (P0-035, E0.4): lazy, so boot never fails on a
    // temporarily-down database — /readyz reports it down instead.
    // A malformed PF_DATABASE_URL still fails boot here.
    let pool = connect_lazy(&settings.database_url, &PoolConfig::default()).unwrap_or_else(|err| {
        eprintln!("detag-api: invalid PF_DATABASE_URL: {err}");
        std::process::exit(1);
    });
    let app = build_router(metrics.clone(), pool.clone());
    let addr = SocketAddr::from(([127, 0, 0, 1], settings.api_port));
    tracing::info!(addr = %addr, "listening");
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();

    // Internal /metrics server (P0-026, FR-X-02): its own port on
    // loopback, never on the public api port. Compose network isolation
    // (internal network only, port not published) lands with E0.9.
    let metrics_addr = SocketAddr::from(([127, 0, 0, 1], settings.metrics_port));
    tracing::info!(addr = %metrics_addr, "metrics listening");
    let metrics_listener = tokio::net::TcpListener::bind(metrics_addr).await.unwrap();

    // One signal listener, shared by both servers: on SIGTERM/SIGINT
    // both stop accepting and drain in-flight requests.
    let shutdown = Arc::new(Notify::new());
    {
        let shutdown = Arc::clone(&shutdown);
        tokio::spawn(async move {
            shutdown_signal().await;
            shutdown.notify_waiters();
        });
    }
    let wait_shutdown = |shutdown: Arc<Notify>| async move {
        shutdown.notified().await;
    };
    // Graceful shutdown (P0-023): a stuck drain is cut off after
    // SHUTDOWN_TIMEOUT and the process exits non-zero rather than
    // hanging a deploy. The pool closes first (P0-035): in-flight
    // queries finish, idle connections drop, then the servers stop.
    let api_server =
        axum::serve(listener, app).with_graceful_shutdown(wait_shutdown(Arc::clone(&shutdown)));
    let metrics_server = axum::serve(metrics_listener, metrics_router(metrics))
        .with_graceful_shutdown(wait_shutdown(Arc::clone(&shutdown)));
    let drained = tokio::time::timeout(SHUTDOWN_TIMEOUT, async {
        tokio::join!(api_server, metrics_server)
    })
    .await;
    match drained {
        Ok((Ok(()), Ok(()))) => {
            pool.close().await;
            shutdown_telemetry();
            tracing::info!("shut down gracefully");
        }
        Ok((api, metrics)) => {
            pool.close().await;
            if let Err(err) = api {
                eprintln!("detag-api serve error: {err}");
            }
            if let Err(err) = metrics {
                eprintln!("detag-api metrics serve error: {err}");
            }
            std::process::exit(1);
        }
        Err(_) => {
            eprintln!("detag-api shutdown timed out after {SHUTDOWN_TIMEOUT:?}; forcing exit");
            std::process::exit(1);
        }
    }
}
