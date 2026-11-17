//! Billing routes (P0-064): `/v1/plans`, `/v1/billing/*`,
//! `/v1/usage`, `/v1/webhooks/*`, `/v1/webhook-endpoints/*`.
//!
//! User webhook endpoints live here: they are gated by plan
//! entitlement, so they belong with billing, not with jobs.
//! The checkout endpoint is live (P1-062) and the Stripe
//! provider webhook is live (P1-063); the rest are the
//! shared 501 stub until their Phase 1 tickets land — except
//! the webhook-endpoints nest, which uses the authenticated
//! stub (`stub_authed`, P1-034): an API key needs
//! `webhooks:manage` there, a caller without a credential is
//! 401. P1-055: `POST /v1/webhook-endpoints` runs the
//! `webhooks` feature gate first ([`create_webhook_endpoint`])
//! — a Free user is 403 `PF-3003`; an entitled caller still
//! reaches the 501 stub until the real handler lands.

use super::{
    extractors::{RequestId, ValidatedJson},
    stub, stub_authed,
};
use crate::{
    auth::AuthContext,
    problem::ProblemResponse,
    services::{
        checkout::{
            create_checkout as create_checkout_service, CheckoutError, CheckoutRequestBody,
        },
        entitlements::require_feature,
        webhook::{
            receive_stripe_webhook as receive_stripe_webhook_service, signature_header,
            WebhookError, WebhookReceipt,
        },
    },
    AppState,
};
use axum::{
    body::Bytes,
    extract::{Extension, State},
    http::{header::HeaderMap, StatusCode},
    response::{IntoResponse, Json, Response},
    routing::{delete, get, post},
    Router,
};
use detag_billing::CheckoutSession;
use detag_core::error::{AppError, FieldError, Problem, Profile};
use detag_validator::ValidationErrors;

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

/// Create a checkout session (P1-062, FR-P-03): the buyer
/// is redirected to the provider-hosted page at the returned
/// `url`. JWT only — the route is not in the extractor's
/// scope table, so an API key is 403 `PF-1002` before this
/// handler runs (P1-040's "no billing for keys"); no
/// credential is 401 `PF-1001` from the extractor. The
/// `Idempotency-Key` header is honored (a replay returns the
/// stored session, a reuse with a different request is 409
/// `PF-2011`); without the header every call is a fresh
/// checkout. Billing not configured is 503 `PF-5005`.
#[utoipa::path(
    post,
    path = "/v1/billing/checkout",
    request_body = CheckoutRequestBody,
    responses(
        (status = 200, description = "The hosted checkout session — redirect the buyer to `url`", body = CheckoutSession),
        (status = 401, description = "Unauthenticated (no credential, bad or expired token)", body = Problem),
        (status = 403, description = "An API key cannot check out (PF-1002)", body = Problem),
        (status = 404, description = "Unknown plan code (PF-2010)", body = Problem),
        (status = 409, description = "Idempotency key reused with a different request (PF-2011)", body = Problem),
        (status = 422, description = "Invalid body, or the plan cannot be bought (PF-2002)", body = Problem),
        (status = 500, description = "The provider call failed", body = Problem),
        (status = 503, description = "Billing is not configured (no Stripe secret key)", body = Problem)
    ),
    tag = "billing",
    security(("bearer_auth" = []))
)]
pub async fn create_checkout(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    auth: AuthContext,
    headers: HeaderMap,
    ValidatedJson(body): ValidatedJson<CheckoutRequestBody>,
) -> Response {
    let request_id = Some(id.0.as_str());
    let Some(ref user) = auth.user else {
        return ProblemResponse(AppError::Unauthenticated.to_problem(
            Profile::Production,
            request_id,
            None,
        ))
        .into_response();
    };
    // An empty header value counts as no key (a client that
    // sends the header with nothing in it gets a fresh
    // checkout, not a stored row under the empty string).
    let idempotency_key = headers
        .get("idempotency-key")
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.is_empty());
    match create_checkout_service(&state, user, &body, idempotency_key).await {
        Ok(session) => (StatusCode::OK, Json(session)).into_response(),
        Err(CheckoutError::NotConfigured) => ProblemResponse(
            AppError::BillingNotConfigured.to_problem(Profile::Production, request_id, None),
        )
        .into_response(),
        Err(CheckoutError::UnknownPlan) => {
            ProblemResponse(AppError::NotFound.to_problem(Profile::Production, request_id, None))
                .into_response()
        }
        Err(CheckoutError::PlanNotPurchasable(reason)) => {
            let mut errors = ValidationErrors::default();
            errors.push(FieldError::new("plan_code", "not_purchasable", reason));
            ProblemResponse(errors.into_problem(Profile::Production, request_id)).into_response()
        }
        Err(CheckoutError::IdempotencyConflict) => {
            ProblemResponse(AppError::Conflict.to_problem(Profile::Production, request_id, None))
                .into_response()
        }
        Err(CheckoutError::Provider | CheckoutError::Internal) => {
            ProblemResponse(AppError::Internal.to_problem(Profile::Production, request_id, None))
                .into_response()
        }
    }
}

/// Receive a Stripe webhook delivery (P1-063, FR-P-04).
/// No auth extractor runs here by design — the provider's
/// signature IS the credential (the matrix row is `Ignored`).
/// The raw body is read as bytes: the signature covers the
/// exact bytes Stripe sent, and any JSON parse before the
/// check would break it. A bad signature is 400 `PF-2013`
/// (the endpoint's only 4xx); billing unconfigured is 503
/// `PF-5005`; every verified delivery — stored, replayed, or
/// ignored — is 200 with a receipt.
#[utoipa::path(
    post,
    path = "/v1/webhooks/stripe",
    request_body(
        content = String,
        description = "The raw Stripe event envelope — the signature covers these exact bytes",
        content_type = "application/json"
    ),
    responses(
        (status = 200, description = "Received (stored, replayed, or ignored)", body = WebhookReceipt),
        (status = 400, description = "The webhook signature does not verify (PF-2013)", body = Problem),
        (status = 503, description = "Billing is not configured (no Stripe secret key)", body = Problem)
    ),
    tag = "billing"
)]
pub async fn receive_stripe_webhook(
    State(state): State<AppState>,
    Extension(id): Extension<RequestId>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let request_id = Some(id.0.as_str());
    let signature = headers
        .get(signature_header())
        .and_then(|value| value.to_str().ok());
    match receive_stripe_webhook_service(&state, &body, signature).await {
        Ok(_) => (StatusCode::OK, Json(WebhookReceipt { received: true })).into_response(),
        Err(WebhookError::NotConfigured) => ProblemResponse(
            AppError::BillingNotConfigured.to_problem(Profile::Production, request_id, None),
        )
        .into_response(),
        Err(WebhookError::SignatureInvalid) => ProblemResponse(
            AppError::WebhookSignatureInvalid.to_problem(Profile::Production, request_id, None),
        )
        .into_response(),
        Err(WebhookError::Internal) => {
            ProblemResponse(AppError::Internal.to_problem(Profile::Production, request_id, None))
                .into_response()
        }
    }
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
                .route("/checkout", post(create_checkout))
                .route("/portal", post(stub))
                .route("/invoices", get(stub)),
        )
        .nest("/v1/usage", Router::new().route("/", get(stub)))
        .nest(
            "/v1/webhooks",
            Router::new()
                .route("/stripe", post(receive_stripe_webhook))
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
