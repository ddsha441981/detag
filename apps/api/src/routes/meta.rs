//! Meta routes (P0-064): `/healthz`, `/readyz`.
//!
//! Unversioned and unauthenticated by design: orchestrators and
//! load balancers must reach them without credentials. These
//! two are the only real (non-stub) handlers in the router
//! today.

use axum::{extract::State, http::StatusCode, response::Json, routing::get, Router};
use detag_core::db::{db_health, Readiness};
use detag_core::redis::redis_health;
use detag_engine::registry::models_health;
use detag_storage::health::storage_health;

use crate::openapi::openapi_json;
use crate::AppState;

/// Liveness: the process answers. Readiness (below) is the
/// dependency check.
#[utoipa::path(
    get,
    path = "/healthz",
    responses(
        (status = 200, description = "Process is alive", body = String, content_type = "text/plain")
    ),
    tag = "meta"
)]
pub(crate) async fn healthz() -> &'static str {
    "ok"
}

/// Readiness (P0-035 db, P0-046 redis, P0-066 storage + models;
/// FR-X-02): 200 when every component is ok, 503 otherwise. The
/// four probes run concurrently — each is bounded by its own 2 s
/// timeout, so the worst case is ~2 s, not ~8 s. Every component
/// reports its probe latency in `latency_ms` (P0-066). The body is
/// always the same JSON — the status code is the signal, the body
/// is the diagnosis.
#[utoipa::path(
    get,
    path = "/readyz",
    responses(
        (status = 200, description = "All components ok", body = Readiness),
        (status = 503, description = "One or more components down", body = Readiness)
    ),
    tag = "meta"
)]
pub(crate) async fn readyz(State(state): State<AppState>) -> (StatusCode, Json<Readiness>) {
    let (db, redis, storage, models) = tokio::join!(
        db_health(&state.pool),
        redis_health(&state.settings.redis_url),
        storage_health(&state.storage),
        async { models_health() },
    );
    let report = Readiness::from_components(vec![db, redis, storage, models]);
    let status = if report.is_ok() {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (status, Json(report))
}

/// The meta nest: unversioned health + readiness at the root, plus
/// the generated OpenAPI document (P0-067 — always on, including
/// prod; only the interactive UI at `/docs` is gated).
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .route("/openapi.json", get(openapi_json))
}
