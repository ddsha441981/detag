//! Job routes (P0-064): `/v1/jobs/*`.
//!
//! All handlers are the authenticated 501 stub
//! (`stub_authed`, P1-034) until their Phase 2/3 tickets
//! land: a caller without a credential is 401 `PF-1001`, an
//! API key without the route's scope is 403 `PF-1002`, and a
//! verified caller reaches the stub's 501. The attestation
//! route (second explicit consent step for gated provenance
//! operations) lives here because it is job-scoped.

use super::stub_authed;
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
            .route("/", post(stub_authed).get(stub_authed))
            .route("/{id}", get(stub_authed))
            .route("/{id}/cancel", post(stub_authed))
            .route("/{id}/retry", post(stub_authed))
            .route("/{id}/events", get(stub_authed))
            .route("/{id}/result", get(stub_authed))
            .route("/{id}/report", get(stub_authed))
            .route("/{id}/attestation", post(stub_authed)),
    )
}
