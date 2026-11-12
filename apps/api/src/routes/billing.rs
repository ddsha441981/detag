//! Billing routes (P0-064): `/v1/plans`, `/v1/billing/*`,
//! `/v1/usage`, `/v1/webhooks/*`, `/v1/webhook-endpoints/*`.
//!
//! User webhook endpoints live here: they are gated by plan
//! entitlement, so they belong with billing, not with jobs.
//! All handlers are the shared 501 stub until their Phase 1
//! tickets land.

use super::stub;
use crate::AppState;
use axum::{
    routing::{delete, get, post},
    Router,
};

/// The billing nests: public plans, the subscription / checkout /
/// portal / invoices group, usage, provider webhooks (Stripe,
/// Razorpay), and the user's own webhook endpoints.
pub fn router() -> Router<AppState> {
    Router::new()
        .nest("/v1/plans", Router::new().route("/", get(stub)))
        .nest(
            "/v1/billing",
            Router::new()
                .route("/subscription", get(stub))
                .route("/checkout", post(stub))
                .route("/portal", post(stub))
                .route("/invoices", get(stub)),
        )
        .nest("/v1/usage", Router::new().route("/", get(stub)))
        .nest(
            "/v1/webhooks",
            Router::new()
                .route("/stripe", post(stub))
                .route("/razorpay", post(stub)),
        )
        .nest(
            "/v1/webhook-endpoints",
            Router::new()
                .route("/", get(stub).post(stub))
                .route("/{id}", delete(stub))
                .route("/{id}/deliveries", get(stub)),
        )
}
