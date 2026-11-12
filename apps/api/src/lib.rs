//! detag-api — Axum HTTP server (bootstrap stub, P0-002).
//! Full router/middleware/state land in E0.7 (P0-063..P0-074).
//!
//! The router lives in the lib target so integration tests
//! (`apps/api/tests/`) can drive it without spawning the binary.

pub mod http_metrics;
pub mod metrics;
pub mod request_id;

use axum::{middleware, routing::get, Extension, Router};
use detag_core::metrics::Metrics;

/// Minimal router: only the process health endpoint exists yet.
/// The request-id middleware (P0-019) wraps every route; the HTTP
/// metrics middleware (P0-028) counts + times every request.
/// Layer order (last added runs first): the `Extension` carrying the
/// metrics must be outermost so the middleware can read it, and the
/// metrics middleware wraps the request-id middleware so its timer
/// covers the whole downstream call.
pub fn build_router(metrics: Metrics) -> Router {
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .layer(middleware::from_fn(request_id::request_id_middleware))
        .layer(middleware::from_fn(http_metrics::http_metrics_middleware))
        .layer(Extension(metrics))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn router_builds() {
        let _app = build_router(Metrics::new());
    }
}
