//! detag-api — Axum HTTP server (bootstrap stub, P0-002).
//! Full router/middleware/state land in E0.7 (P0-063..P0-074).
//!
//! The router lives in the lib target so integration tests
//! (`apps/api/tests/`) can drive it without spawning the binary.

pub mod http_metrics;
pub mod metrics;
pub mod request_id;

use axum::{
    extract::Extension, http::StatusCode, middleware, response::Json, routing::get, Router,
};
use detag_core::db::{db_health, DbPool, Readiness};
use detag_core::metrics::Metrics;

/// Readiness (P0-035, FR-X-02): 200 when every component is ok, 503
/// otherwise. Today the only component is the database; redis,
/// storage and models join in E0.7 (P0-063), which owns the full
/// `/readyz` shape. The body is always the same JSON — the status
/// code is the signal, the body is the diagnosis.
async fn readyz(Extension(pool): Extension<DbPool>) -> (StatusCode, Json<Readiness>) {
    let report = Readiness::from_components(vec![db_health(&pool).await]);
    let status = if report.is_ok() {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (status, Json(report))
}

/// Minimal router: the process health endpoint, plus readiness.
/// The request-id middleware (P0-019) wraps every route; the HTTP
/// metrics middleware (P0-028) counts + times every request.
/// Layer order (last added runs first): the `Extension`s carrying the
/// metrics and the pool must be outermost so the middleware and the
/// handlers can read them, and the metrics middleware wraps the
/// request-id middleware so its timer covers the whole downstream call.
pub fn build_router(metrics: Metrics, pool: DbPool) -> Router {
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/readyz", get(readyz))
        .layer(middleware::from_fn(request_id::request_id_middleware))
        .layer(middleware::from_fn(http_metrics::http_metrics_middleware))
        .layer(Extension(metrics))
        .layer(Extension(pool))
}

#[cfg(test)]
mod tests {
    use super::*;
    use detag_core::config::Settings;
    use detag_core::db::{connect_lazy, PoolConfig};

    #[tokio::test]
    async fn router_builds() {
        // Lazy pool: no connection is attempted, so the router builds
        // even with no database behind the URL.
        let pool = connect_lazy(&Settings::default().database_url, &PoolConfig::default())
            .expect("dev-default URL is well-formed");
        let _app = build_router(Metrics::new(), pool);
    }
}
