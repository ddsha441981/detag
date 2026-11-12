//! Job routes (P0-064): `/v1/jobs/*`.
//!
//! All handlers are the shared 501 stub until their Phase 2/3
//! tickets land. The attestation route (second explicit consent
//! step for gated provenance operations) lives here because it
//! is job-scoped.

use super::stub;
use crate::AppState;
use axum::{
    routing::{get, post},
    Router,
};

/// The jobs nest: create, list, get, cancel, retry, SSE events,
/// result, report, attestation.
pub fn router() -> Router<AppState> {
    Router::new().nest(
        "/v1/jobs",
        Router::new()
            .route("/", post(stub).get(stub))
            .route("/{id}", get(stub))
            .route("/{id}/cancel", post(stub))
            .route("/{id}/retry", post(stub))
            .route("/{id}/events", get(stub))
            .route("/{id}/result", get(stub))
            .route("/{id}/report", get(stub))
            .route("/{id}/attestation", post(stub)),
    )
}
