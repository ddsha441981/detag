//! Meta routes (P0-064): `/healthz`, `/readyz`.
//!
//! Unversioned and unauthenticated by design: orchestrators and
//! load balancers must reach them without credentials. These
//! two are the only real (non-stub) handlers in the router
//! today.

use axum::{extract::State, http::StatusCode, response::Json, routing::get, Router};
use detag_core::db::{db_health, Readiness};
use detag_core::redis::redis_health;

use crate::AppState;

/// Liveness: the process answers. Readiness (below) is the
/// dependency check.
async fn healthz() -> &'static str {
    "ok"
}

/// Readiness (P0-035, FR-X-02; redis component P0-046): 200 when
/// every component is ok, 503 otherwise. The components are the
/// database and redis, read from the state; storage and models
/// join in E0.7 (P0-066), which owns the full `/readyz` shape.
/// The body is always the same JSON — the status code is the
/// signal, the body is the diagnosis.
async fn readyz(State(state): State<AppState>) -> (StatusCode, Json<Readiness>) {
    let report = Readiness::from_components(vec![
        db_health(&state.pool).await,
        redis_health(&state.settings.redis_url).await,
    ]);
    let status = if report.is_ok() {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (status, Json(report))
}

/// The meta nest: unversioned health + readiness at the root.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
}
