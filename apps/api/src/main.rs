//! detag-api binary: load settings, build the state, serve the
//! router from the lib on two listeners (P0-063, E0.7).

use detag_api::{build_router, metrics::metrics_router, AppState};
use detag_core::config::{warn_unknown_feature_flags, Settings};
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
    // The state (P0-063): pool + redis pool + storage + settings +
    // clock. Construction is lazy where it can be (a down Postgres
    // or Redis does NOT fail boot — /readyz reports it) and fails
    // fast only on malformed configuration. The error message is a
    // static string per failure — it never echoes a URL or secret.
    let state = AppState::from_settings(&settings).unwrap_or_else(|err| {
        eprintln!("detag-api: cannot build state: {err}");
        std::process::exit(1);
    });
    let pool = state.pool.clone();
    let app = build_router(state, metrics.clone());
    let addr = SocketAddr::from((settings.api_host, settings.api_port));
    tracing::info!(addr = %addr, "listening");
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();

    // Internal /metrics server (P0-026, FR-X-02): its own port on
    // PF_METRICS_HOST, never on the public api port. In compose the
    // metrics port is reachable only on the internal network (E0.9);
    // natively it stays on loopback.
    let metrics_addr = SocketAddr::from((settings.metrics_host, settings.metrics_port));
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
    // Graceful shutdown (P0-023, fixed P0-090): SHUTDOWN_TIMEOUT
    // bounds only the drain AFTER the signal. It used to wrap the
    // whole server run (`timeout(SHUTDOWN_TIMEOUT, join!(...))`),
    // so the api force-exited 30 s after startup even with no
    // signal — the smoke proof caught it (both binaries died
    // mid-smoke; in compose they would restart-loop forever).
    // The servers run in a spawned task so the drain can be
    // awaited with a timeout once the signal arrives; a server
    // erroring on its own still ends the process promptly.
    // The pool closes first (P0-035): in-flight queries finish,
    // idle connections drop, then the servers stop.
    let api_server =
        axum::serve(listener, app).with_graceful_shutdown(wait_shutdown(Arc::clone(&shutdown)));
    let metrics_server = axum::serve(metrics_listener, metrics_router(metrics))
        .with_graceful_shutdown(wait_shutdown(Arc::clone(&shutdown)));
    let mut servers = tokio::spawn(async move { tokio::join!(api_server, metrics_server) });
    enum End {
        Clean,
        ServeFailed(std::io::Result<()>, std::io::Result<()>),
        TaskFailed(tokio::task::JoinError),
        DrainTimeout,
    }
    let end = tokio::select! {
        _ = shutdown.notified() => {
            // Signal arrived: the servers are draining; bound it.
            match tokio::time::timeout(SHUTDOWN_TIMEOUT, servers).await {
                Ok(Ok((Ok(()), Ok(())))) => End::Clean,
                Ok(Ok((api, metrics))) => End::ServeFailed(api, metrics),
                Ok(Err(err)) => End::TaskFailed(err),
                Err(_) => End::DrainTimeout,
            }
        }
        result = &mut servers => {
            // A server exited on its own (serve error) before any
            // signal — report it, don't wait for a signal that may
            // never come.
            match result {
                Ok((Ok(()), Ok(()))) => End::Clean,
                Ok((api, metrics)) => End::ServeFailed(api, metrics),
                Err(err) => End::TaskFailed(err),
            }
        }
    };
    match end {
        End::Clean => {
            pool.close().await;
            shutdown_telemetry();
            tracing::info!("shut down gracefully");
        }
        End::ServeFailed(api, metrics) => {
            pool.close().await;
            if let Err(err) = api {
                eprintln!("detag-api serve error: {err}");
            }
            if let Err(err) = metrics {
                eprintln!("detag-api metrics serve error: {err}");
            }
            std::process::exit(1);
        }
        End::TaskFailed(err) => {
            eprintln!("detag-api server task failed: {err}");
            std::process::exit(1);
        }
        End::DrainTimeout => {
            eprintln!("detag-api shutdown timed out after {SHUTDOWN_TIMEOUT:?}; forcing exit");
            std::process::exit(1);
        }
    }
}
