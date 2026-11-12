//! Admin routes (P0-064): `/v1/admin/*` (+ the public abuse
//! intake `/v1/abuse`).
//!
//! The abuse intake is public (rate-limited) and its tickets
//! are reviewed in the admin abuse queue, so it lives here.
//! The resolve endpoint is `POST
//! /v1/admin/abuse-reports/{id}/resolve` — the spec says only
//! "POST resolve with action" under the abuse-reports row.
//! All handlers are the shared 501 stub until their Phase 4/5
//! tickets land.

use super::stub;
use crate::AppState;
use axum::{
    routing::{get, post},
    Router,
};

/// The admin nest: dashboard, users (search / suspend / restore /
/// quota-grant / impersonate), jobs (browse / reprocess / cancel),
/// flags, plans, audit, abuse reports, attestations, system —
/// plus the public abuse intake at `/v1/abuse`.
pub fn router() -> Router<AppState> {
    Router::new()
        .nest(
            "/v1/admin",
            Router::new()
                .route("/metrics/dashboard", get(stub))
                .route("/users", get(stub))
                .route("/users/{id}/suspend", post(stub))
                .route("/users/{id}/restore", post(stub))
                .route("/users/{id}/quota-grant", post(stub))
                .route("/users/{id}/impersonate", post(stub))
                .route("/jobs", get(stub))
                .route("/jobs/{id}/reprocess", post(stub))
                .route("/jobs/{id}/cancel", post(stub))
                .route("/flags", get(stub).put(stub))
                .route("/plans", get(stub).put(stub))
                .route("/audit", get(stub))
                .route("/abuse-reports", get(stub))
                .route("/abuse-reports/{id}/resolve", post(stub))
                .route("/attestations", get(stub))
                .route("/system", get(stub)),
        )
        .nest("/v1/abuse", Router::new().route("/", post(stub)))
}
