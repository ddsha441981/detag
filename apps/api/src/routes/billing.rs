//! Billing routes (P0-064): `/v1/plans`, `/v1/billing/*`,
//! `/v1/usage`, `/v1/webhooks/*`, `/v1/webhook-endpoints/*`.
//!
//! User webhook endpoints live here: they are gated by plan
//! entitlement, so they belong with billing, not with jobs.
//! All handlers are the shared 501 stub until their Phase 1
//! tickets land — except the webhook-endpoints nest, which
//! uses the authenticated stub (`stub_authed`, P1-034): an API
//! key needs `webhooks:manage` there, a caller without a
//! credential is 401. P1-055: `POST /v1/webhook-endpoints`
//! runs the `webhooks` feature gate first
//! ([`create_webhook_endpoint`]) — a Free user is 403
//! `PF-3003`; an entitled caller still reaches the 501 stub
//! until the real handler lands.

use super::{stub, stub_authed};
use crate::{
    auth::AuthContext, problem::ProblemResponse, services::entitlements::require_feature, AppState,
};
use axum::{
    extract::{Extension, State},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
    Router,
};
use detag_core::error::{AppError, Profile};

use super::extractors::RequestId;

/// Create a webhook endpoint (P1-055, FR-P-01): the plan
/// gate, then the 501 stub — the real handler has not landed
/// yet, but the gate already decides who may create one. A
/// JWT caller is gated on their own plan (Free → 403
/// `PF-3003`); a key caller has no user in the context (the
/// verified key carries no owner id — `VerifiedApiKey` holds
/// id + scopes + tier only), so keys pass through to the stub
/// unchanged: the key itself could only be minted by an
/// entitled owner (P1-055 gates key creation), and the stub
/// grants nothing either way.
async fn create_webhook_endpoint(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    auth: AuthContext,
) -> Response {
    let request_id = Some(id.0.as_str());
    if let Some(user) = &auth.user {
        if let Err(err) = require_feature(&state.plans, &state.pool, user.id, "webhooks").await {
            let problem = match err {
                AppError::FeatureNotEntitled => {
                    err.to_problem(Profile::Production, request_id, None)
                }
                // A plan-store failure must not create the
                // endpoint it cannot gate — but there is no
                // endpoint to create yet, so the stub's 501
                // is the safe answer either way; fail closed
                // to the same 501 the stub returns.
                _ => AppError::NotImplemented.to_problem(Profile::Production, request_id, None),
            };
            return ProblemResponse(problem).into_response();
        }
    }
    ProblemResponse(AppError::NotImplemented.to_problem(Profile::Production, request_id, None))
        .into_response()
}

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
                .route("/", get(stub_authed).post(create_webhook_endpoint))
                .route("/{id}", delete(stub_authed))
                .route("/{id}/deliveries", get(stub_authed)),
        )
}
