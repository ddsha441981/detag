//! detag-api binary: load settings, serve the router from the lib.

use detag_api::build_router;
use detag_core::config::{warn_unknown_feature_flags, Settings};
use std::net::SocketAddr;

#[tokio::main]
async fn main() {
    // Fail boot fast on invalid configuration, naming the bad PF_* var.
    let _settings = Settings::load().unwrap_or_else(|err| {
        eprintln!("detag-api: invalid configuration:\n{err}");
        std::process::exit(1);
    });
    // Unknown PF_FEATURE_* flags warn (a typo must not silently do nothing).
    warn_unknown_feature_flags();
    let app = build_router();
    let addr = SocketAddr::from(([127, 0, 0, 1], 8080));
    println!("detag-api listening on {addr}");
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}
