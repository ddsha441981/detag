//! Checkout (P1-062, FR-P-03): `POST /v1/billing/checkout`.
//!
//! The handler's service: take the caller's plan, resolve the
//! price from our own catalog (never from the request — the
//! client names a plan code, the price comes from the `plans`
//! row), reuse the user's existing provider customer when one
//! is on file, and hand the provider a checkout session in
//! subscription mode. Success/cancel URLs come from config
//! (`PF_PUBLIC_BASE_URL` + the console's billing paths — the
//! console pages themselves land in Phase 4; until then the
//! URLs are the contract, not live pages).
//!
//! Rules, in order:
//! - Billing must be configured (`AppState::billing` is
//!   `Some`) — otherwise 503 `PF-5005`. The provider is built
//!   once at boot from the Stripe key; an empty key or the dev
//!   placeholder means "not configured", never a live call
//!   with a key that is not a key (ASSUMPTION-024).
//! - Idempotency first: when the client sends an
//!   `Idempotency-Key`, a stored response for the same key
//!   and the same canonical request is replayed WITHOUT
//!   calling the provider again (the AC); the same key with
//!   a different request is 409 `PF-2011`. The replay check
//!   runs before the plan lookup on purpose — a replay must
//!   return the session that was already created even if the
//!   plan has since become unpurchasable.
//! - The plan must exist (unknown code is 404 `PF-2010`) and
//!   be purchasable: Free (₹0) has no checkout, and a plan
//!   whose `stripe_price_id` is NULL has no price to charge
//!   yet (the catalog sync, P1-061, is what fills it) —
//!   both are 422 `PF-2002` on `plan_code`
//!   (`not_purchasable`).
//! - The customer is reused when the user already has one:
//!   the newest `provider_customer_id` on any of their
//!   subscription rows (live or canceled — the customer
//!   survives cancellation). A first-time buyer has no row,
//!   so the provider mints the customer at checkout and the
//!   webhook records it on the subscription row (P1-063).
//! - A provider `InvalidPrice` (our configured price does not
//!   exist at Stripe) is 500 `PF-5000`: the plan is
//!   configured, the provider disagrees — that is config
//!   drift, not a client error, and the catalog sync is the
//!   repair. Every other provider failure is 500 too, and no
//!   provider message ever reaches the problem body.
//! - No audit row: nothing durable is created in our DB by a
//!   checkout except the idempotency row — the subscription
//!   row (and its audit) lands with the webhook (P1-063/065).
//!   A duplicate-purchase guard (already holding a live
//!   subscription to the same plan) is NOT here: proration
//!   and upgrade/downgrade timing belong to P1-069, and
//!   blocking the purchase there keeps this endpoint's
//!   contract to one thing — start a checkout.
//!
//! Idempotency storage: the `idempotency_keys` table (see
//! `detag_core::repos::idempotency`). The stored envelope is
//! `{"request": <canonical request JSON>, "response": <the
//! checkout session JSON>}`; the canonical request is the
//! endpoint + the user id + the plan code, so a key replayed
//! by another user or for another plan 409s. Entries live
//! 24 h (Stripe's checkout-session lifetime); past that, the
//! key is dead and the next call is fresh. A concurrent
//! same-key race can create two Stripe sessions (the loser's
//! insert loses and it replays the winner's response); a
//! sequential retry — the AC — never does.

use crate::AppState;
use chrono::Duration;
use detag_billing::{CheckoutRequest, CheckoutSession};
use detag_core::{
    error::FieldError,
    repos::{idempotency, subscriptions, users::User},
};
use detag_validator::{Validate, ValidationErrors};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use thiserror::Error;
use utoipa::ToSchema;

/// The route this service serves — the idempotency rows are
/// scoped to it (a key reused on another endpoint hits the
/// same row, and the endpoint inside the stored request makes
/// the fingerprints differ, so it 409s).
const CHECKOUT_ENDPOINT: &str = "/v1/billing/checkout";

/// How long an idempotency entry lives (24 h — Stripe's
/// checkout-session lifetime, ADR-0030).
const IDEMPOTENCY_TTL: Duration = Duration::hours(24);

/// The checkout request body: the plan to buy. The price
/// comes from the catalog, never from the request.
#[derive(Debug, Clone, Deserialize, Serialize, ToSchema)]
pub struct CheckoutRequestBody {
    /// The plan code (`free` / `pro` / `business`).
    pub plan_code: String,
}

impl Validate for CheckoutRequestBody {
    fn validate(&self) -> Result<(), ValidationErrors> {
        let mut errors = ValidationErrors::default();
        if self.plan_code.is_empty() {
            errors.push(FieldError::new(
                "plan_code",
                "required",
                "plan_code is required",
            ));
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}

/// What checkout can fail with. The handler maps each variant
/// to its problem body; the provider's messages never leave
/// this module.
#[derive(Debug, Error)]
pub enum CheckoutError {
    /// No billing provider is configured (no Stripe key):
    /// the handler answers 503 `PF-5005`.
    #[error("billing provider not configured")]
    NotConfigured,
    /// The plan code names no plan: 404 `PF-2010`.
    #[error("unknown plan")]
    UnknownPlan,
    /// The plan cannot be bought (Free, or no Stripe price
    /// configured): 422 `PF-2002` on `plan_code`. The
    /// `reason` is the field error's message — a static
    /// sentence, never a value.
    #[error("plan is not purchasable: {0}")]
    PlanNotPurchasable(&'static str),
    /// The same idempotency key was already used for a
    /// different request: 409 `PF-2011`.
    #[error("idempotency key reused with a different request")]
    IdempotencyConflict,
    /// The provider call failed, or the idempotency row could
    /// not be read/written: 500 `PF-5000`.
    #[error("billing provider error")]
    Provider,
    /// The catalog, the customer lookup, or the idempotency
    /// store failed: 500 `PF-5000`.
    #[error("checkout failed")]
    Internal,
}

/// The canonical request for an idempotency envelope: the
/// endpoint + the user + the plan. Two requests are "the same
/// request" exactly when these are equal.
fn canonical_request(user_id: uuid::Uuid, plan_code: &str) -> Value {
    json!({
        "endpoint": CHECKOUT_ENDPOINT,
        "user_id": user_id.to_string(),
        "plan_code": plan_code,
    })
}

/// Read a stored session out of an idempotency envelope:
/// `None` when the envelope is not a checkout response
/// (a corrupt row — a server bug, never a client error).
fn session_from_envelope(envelope: &Value) -> Option<CheckoutSession> {
    serde_json::from_value(envelope.get("response")?.clone()).ok()
}

/// Start a checkout for `user`'s plan, honoring
/// `idempotency_key` when the client sent one.
pub async fn create_checkout(
    state: &AppState,
    user: &User,
    body: &CheckoutRequestBody,
    idempotency_key: Option<&str>,
) -> Result<CheckoutSession, CheckoutError> {
    let now = state.clock.now();
    let canonical = canonical_request(user.id, &body.plan_code);

    // The replay check runs before anything else: a stored
    // response for this key + this request is the answer,
    // with no provider call and no re-validation — and no
    // provider is needed to return it, so a replay answers
    // even while billing is unconfigured (the provider check
    // below only gates NEW checkouts).
    if let Some(key) = idempotency_key {
        if let Some(row) = idempotency::get(&state.pool, key, now)
            .await
            .map_err(|_| CheckoutError::Internal)?
        {
            let stored_request = row.response.get("request");
            if stored_request == Some(&canonical) {
                return session_from_envelope(&row.response).ok_or_else(|| {
                    tracing::error!("checkout: corrupt idempotency envelope for key");
                    CheckoutError::Internal
                });
            }
            return Err(CheckoutError::IdempotencyConflict);
        }
    }

    // The plan comes from our own catalog (cached 5 min):
    // the client names a code, we supply the price.
    let plan = state
        .plans
        .by_code(&body.plan_code)
        .await
        .map_err(|_| CheckoutError::Internal)?
        .ok_or(CheckoutError::UnknownPlan)?;
    if plan.amount == 0 {
        return Err(CheckoutError::PlanNotPurchasable(
            "the free plan has no checkout",
        ));
    }
    let price_id = plan
        .stripe_price_id
        .ok_or(CheckoutError::PlanNotPurchasable(
            "this plan has no price configured yet",
        ))?;

    // Customer reuse: the newest provider customer id on any
    // of the user's subscription rows (live or canceled).
    let customer_id = subscriptions::latest_customer_id_for_user(&state.pool, user.id)
        .await
        .map_err(|_| CheckoutError::Internal)?;

    let base = state.settings.public_base_url.trim_end_matches('/');
    let success_url = format!("{base}/billing/success?session_id={{CHECKOUT_SESSION_ID}}");
    let cancel_url = format!("{base}/billing/cancel");
    // Billing must be configured for a NEW checkout (the
    // replay above already returned without one).
    let provider = state.billing.as_ref().ok_or(CheckoutError::NotConfigured)?;
    let session = provider
        .create_checkout(CheckoutRequest {
            user_id: user.id,
            customer_id,
            price_id,
            success_url,
            cancel_url,
        })
        .await
        .map_err(|err| {
            // An InvalidPrice here means OUR configured price
            // does not exist at Stripe — config drift, not a
            // client error. Log it (the price id is ours, not
            // a secret) and answer 500 either way; the
            // provider message never reaches the client.
            tracing::error!(error = %err, "checkout: provider call failed");
            CheckoutError::Provider
        })?;

    // Store the response under the key so a retry replays it.
    // A stale row for the same key (expired) is deleted first,
    // so the new response can take the key. Losing the insert
    // race means a concurrent same-key request stored first —
    // re-read and replay the winner's response.
    if let Some(key) = idempotency_key {
        let envelope = json!({
            "request": canonical,
            "response": serde_json::to_value(&session)
                .map_err(|_| CheckoutError::Internal)?,
        });
        let expires_at = now + IDEMPOTENCY_TTL;
        idempotency::delete_expired(&state.pool, key, now)
            .await
            .map_err(|_| CheckoutError::Internal)?;
        let stored = idempotency::insert(
            &state.pool,
            key,
            Some(user.id),
            CHECKOUT_ENDPOINT,
            &envelope,
            expires_at,
        )
        .await
        .map_err(|_| CheckoutError::Internal)?;
        if !stored {
            let row = idempotency::get(&state.pool, key, now)
                .await
                .map_err(|_| CheckoutError::Internal)?
                .ok_or(CheckoutError::Internal)?;
            if row.response.get("request") == Some(&canonical) {
                return session_from_envelope(&row.response).ok_or(CheckoutError::Internal);
            }
            return Err(CheckoutError::IdempotencyConflict);
        }
    }
    Ok(session)
}
