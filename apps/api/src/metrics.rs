//! The internal `/metrics` server (P0-026, FR-X-02).
//!
//! A separate axum router served on its own internal port (never on
//! the public api port) — Prometheus scrapes it, nothing else should.
//! It serves exactly one route: `GET /metrics` returns the gathered
//! Prometheus text with the standard content type.

use axum::{extract::State, http::header, response::Response, routing::get, Router};
use detag_core::metrics::Metrics;

/// Router for the metrics server: only `/metrics`.
pub fn metrics_router(metrics: Metrics) -> Router {
    Router::new()
        .route("/metrics", get(metrics_handler))
        .with_state(metrics)
}

async fn metrics_handler(State(metrics): State<Metrics>) -> Response<String> {
    Response::builder()
        .header(
            header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )
        .body(metrics.gather())
        .expect("a /metrics response always builds")
}
