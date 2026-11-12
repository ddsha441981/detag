//! detag-api binary: load settings, serve the router from the lib.

use detag_api::build_router;
use detag_core::config::{warn_unknown_feature_flags, Settings};
use detag_core::shutdown::{shutdown_signal, SHUTDOWN_TIMEOUT};
use std::net::SocketAddr;

#[tokio::main]
async fn main() {
    // Fail boot fast on invalid configuration, naming the bad PF_* var.
    let settings = Settings::load().unwrap_or_else(|err| {
        eprintln!("detag-api: invalid configuration:\n{err}");
        std::process::exit(1);
    });
    // Unknown PF_FEATURE_* flags warn (a typo must not silently do nothing).
    warn_unknown_feature_flags();
    let app = build_router();
    let addr = SocketAddr::from(([127, 0, 0, 1], settings.api_port));
    println!("detag-api listening on {addr}");
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    // Graceful shutdown (P0-023): on SIGTERM/SIGINT stop accepting and
    // drain in-flight requests. A stuck drain is cut off after
    // SHUTDOWN_TIMEOUT and the process exits non-zero rather than
    // hanging a deploy. Pool closes hook in here when pools land (E0.4).
    let server = axum::serve(listener, app).with_graceful_shutdown(shutdown_signal());
    match tokio::time::timeout(SHUTDOWN_TIMEOUT, server).await {
        Ok(Ok(())) => println!("detag-api shut down gracefully"),
        Ok(Err(err)) => {
            eprintln!("detag-api serve error: {err}");
            std::process::exit(1);
        }
        Err(_) => {
            eprintln!("detag-api shutdown timed out after {SHUTDOWN_TIMEOUT:?}; forcing exit");
            std::process::exit(1);
        }
    }
}
