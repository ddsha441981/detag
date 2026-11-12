//! The worker's internal `/metrics` server (P0-080) — mirrors
//! `apps/api/src/metrics.rs`.
//!
//! A separate axum router served on its own internal port
//! (PF_WORKER_METRICS_PORT, default 9092 — 9090 is the api's
//! metrics port, 9091 is Prometheus itself in compose), bound to
//! loopback, never on a public port. Prometheus scrapes it; the
//! queue dashboard's worker panel reads it. It serves exactly one
//! route: `GET /metrics` returns the gathered Prometheus text
//! with the standard content type.

use axum::{extract::State, http::header, response::Response, routing::get, Router};
use detag_core::metrics::Metrics;
use std::sync::Arc;
use tokio::sync::Notify;

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

/// Serve `/metrics` on `listener` until `shutdown` fires.
pub async fn serve_metrics(
    metrics: Metrics,
    listener: tokio::net::TcpListener,
    shutdown: Arc<Notify>,
) -> std::io::Result<()> {
    axum::serve(listener, metrics_router(metrics))
        .with_graceful_shutdown(async move {
            shutdown.notified().await;
        })
        .await
}
