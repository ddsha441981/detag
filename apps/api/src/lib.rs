//! detag-api — Axum HTTP server (bootstrap stub, P0-002).
//! Full router/middleware/state land in E0.7 (P0-063..P0-074).
//!
//! The router lives in the lib target so integration tests
//! (`apps/api/tests/`) can drive it without spawning the binary.

pub mod metrics;
pub mod request_id;

use axum::{middleware, routing::get, Router};

/// Minimal router: only the process health endpoint exists yet.
/// The request-id middleware (P0-019) wraps every route.
pub fn build_router() -> Router {
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .layer(middleware::from_fn(request_id::request_id_middleware))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn router_builds() {
        let _app = build_router();
    }
}
