//! detag-api — Axum HTTP server (bootstrap stub, P0-002).
//! Full router/middleware/state land in E0.7 (P0-063..P0-074).

use axum::{routing::get, Router};
use detag_core::config::Settings;
use std::net::SocketAddr;

#[tokio::main]
async fn main() {
    // Fail boot fast on invalid configuration, naming the bad PF_* var.
    let _settings = Settings::load().unwrap_or_else(|err| {
        eprintln!("detag-api: invalid configuration:\n{err}");
        std::process::exit(1);
    });
    let app = build_router();
    let addr = SocketAddr::from(([127, 0, 0, 1], 8080));
    println!("detag-api listening on {addr}");
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

/// Minimal router: only the process health endpoint exists yet.
fn build_router() -> Router {
    Router::new().route("/healthz", get(|| async { "ok" }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn router_builds() {
        let _app = build_router();
    }
}
