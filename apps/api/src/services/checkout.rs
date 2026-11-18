//! Checkout (P1-062, FR-P-03; proration P1-069): `POST
//! /v1/billing/checkout`.
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
//!   plan has since become unpurchasable. A replay returns
//!   the stored outcome whole: a session replay is 200, a
//!   scheduled-downgrade replay is 202.
//! - The plan must exist (unknown code is 404 `PF-2010`).
//! - The caller's live subscription decides what the
//!   checkout IS (P1-069, `detag_billing::proration`):
//!   * No live subscription — a first purchase: the P1-062
//!     path. Free (₹0) has no checkout, and a plan whose
//!     `stripe_price_id` is NULL has no price to charge
//!     yet (the catalog sync, P1-061, is what fills it) —
//!     both are 422 `PF-2002` on `plan_code`
//!     (`not_purchasable`). The response is the session with
//!     no proration (`proration: null` — there is no old
//!     plan to prorate from).
//!   * The same plan (equal amount) — 422 `PF-2002`
//!     (`not_purchasable`, "already subscribed to this
//!     plan"). Buying what you already hold is not a
//!     checkout; no provider call, no row.
//!   * An upgrade (new amount > old) — a checkout with a
//!     proration preview: the session plus `proration`
//!     (credit for the unused old plan, charge for the new
//!     plan's remainder, `effective_at` = now). The live
//!     provider subscription id rides on the provider
//!     request (`upgrade_from_provider_subscription_id`) so
//!     the adapter can write it to the session's
//!     subscription metadata — the webhook reads it when the
//!     new checkout completes (ASSUMPTION-032; the webhook
//!     does not read it yet).
//!   * A downgrade (new amount < old, including to Free) —
//!     NO checkout, NO provider call: the change is
//!     scheduled in `scheduled_plan_changes` and the answer
//!     is 202 with the scheduled change
//!     (`effective_at` = the current period end, proration
//!     all zero — nothing is charged now). A second pending
//!     change for the same subscription is 409 `PF-2011`.
//!     The entitlement switch lands with the applier
//!     (`super::plan_changes`) when the provider's period
//!     rolls past `effective_at`. A downgrade needs no
//!     provider, so it schedules even while billing is
//!     unconfigured — it applies on the next period-roll
//!     event, whenever billing is configured again.
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
//!   A downgrade is the one exception: its scheduled row IS
//!   durable (that is the point — the promise must survive
//!   a restart), but it carries no audit row either; the
//!   audit lands when the applier applies it
//!   (`billing.subscription.plan_changed`).
//!
//! Idempotency storage: the `idempotency_keys` table (see
//! `detag_core::repos::idempotency`). The stored envelope is
//! `{"request": <canonical request JSON>, "response": <the
//! wire body>}` — the wire body is the session JSON for a
//! purchase/upgrade and the scheduled-change JSON for a
//! downgrade, so a replay returns the exact body (and
//! status) the first call returned; the canonical request is
//! the endpoint + the user id + the plan code, so a key
//! replayed by another user or for another plan 409s.
//! Entries live 24 h (Stripe's checkout-session lifetime);
//! past that, the key is dead and the next call is fresh. A
//! concurrent same-key race can create two Stripe sessions
//! (the loser's insert loses and it replays the winner's
//! response); a sequential retry — the AC — never does. A
//! concurrent same-key downgrade race can likewise insert
//! twice — the loser's insert hits the one-pending-change
//! UNIQUE index and 409s, and its retry (same key) replays
//! the winner's 202.

use crate::AppState;
use chrono::{DateTime, Utc};
use detag_billing::{
    classify, period_start_from_end, preview, preview_no_period, ChangeKind, CheckoutRequest,
    CheckoutSession, ProrationPreview,
};
use detag_core::{
    error::FieldError,
    repos::{idempotency, plan_changes, subscriptions, users::User, RepoError},
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
const IDEMPOTENCY_TTL: chrono::Duration = chrono::Duration::hours(24);

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

/// The 200 body for a purchase or an upgrade: the hosted
/// session, plus the proration preview when the buyer already
/// holds a plan (`null` for a first purchase — there is no
/// old plan to prorate from).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CheckoutResponse {
    /// The provider's checkout session id.
    pub id: String,
    /// The hosted checkout URL.
    pub url: String,
    /// The upgrade's proration preview (`null` for a first
    /// purchase). Old idempotency rows (P1-062) carry no key
    /// — it defaults to `null` on read.
    #[serde(default)]
    pub proration: Option<ProrationPreview>,
}

/// The 202 body for a downgrade: no session — the change is
/// scheduled, and the buyer keeps the old plan until
/// `effective_at`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ScheduledChangeResponse {
    /// Always `true`: this body is a scheduled change, not a
    /// session. The flag is what the idempotency replay keys
    /// on to tell the two wire bodies apart.
    pub scheduled: bool,
    /// The plan the subscription moves to at `effective_at`.
    pub plan_code: String,
    /// The plan the subscription moves from.
    pub from_plan_code: String,
    /// When the new plan takes effect (the current period
    /// end, or now when the row carries no period end).
    pub effective_at: DateTime<Utc>,
    /// The downgrade's proration preview: all legs zero
    /// (nothing is charged now), `kind: downgrade`.
    pub proration: ProrationPreview,
}

/// What a checkout call returns: a session to redirect the
/// buyer to (200), or a scheduled downgrade (202).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckoutOutcome {
    /// A purchase or an upgrade: redirect to `url`.
    Session(CheckoutResponse),
    /// A downgrade: scheduled, no session.
    Scheduled(ScheduledChangeResponse),
}

impl CheckoutOutcome {
    /// The exact JSON the route returns for this outcome —
    /// the body the idempotency envelope stores.
    fn wire_body(&self) -> Value {
        match self {
            CheckoutOutcome::Session(response) => {
                serde_json::to_value(response).expect("a checkout response serializes")
            }
            CheckoutOutcome::Scheduled(response) => {
                serde_json::to_value(response).expect("a scheduled change serializes")
            }
        }
    }
}

/// Read a stored outcome out of an idempotency envelope:
/// `None` when the envelope is not a checkout response (a
/// corrupt row — a server bug, never a client error). The
/// `scheduled` flag tells the two wire bodies apart; a
/// session body without a `proration` key is a P1-062 row
/// and reads as a first purchase.
fn outcome_from_envelope(envelope: &Value) -> Option<CheckoutOutcome> {
    let body = envelope.get("response")?;
    if body.get("scheduled").and_then(Value::as_bool) == Some(true) {
        let response: ScheduledChangeResponse = serde_json::from_value(body.clone()).ok()?;
        Some(CheckoutOutcome::Scheduled(response))
    } else {
        let response: CheckoutResponse = serde_json::from_value(body.clone()).ok()?;
        Some(CheckoutOutcome::Session(response))
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
    /// configured), or is already held (same plan): 422
    /// `PF-2002` on `plan_code`. The `reason` is the field
    /// error's message — a static sentence, never a value.
    #[error("plan is not purchasable: {0}")]
    PlanNotPurchasable(&'static str),
    /// The same idempotency key was already used for a
    /// different request: 409 `PF-2011`.
    #[error("idempotency key reused with a different request")]
    IdempotencyConflict,
    /// The subscription already has a pending scheduled
    /// change: 409 `PF-2011` (one pending change per
    /// subscription — the partial UNIQUE index is the guard).
    #[error("a plan change is already scheduled for this subscription")]
    ChangeAlreadyScheduled,
    /// The provider call failed, or the idempotency row could
    /// not be read/written: 500 `PF-5000`.
    #[error("billing provider error")]
    Provider,
    /// The catalog, the customer lookup, the subscription
    /// lookup, or the scheduled-change insert failed:
    /// 500 `PF-5000`.
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

/// Start a checkout for `user`'s plan, honoring
/// `idempotency_key` when the client sent one. Returns the
/// session (200) for a purchase or an upgrade, or the
/// scheduled change (202) for a downgrade.
pub async fn create_checkout(
    state: &AppState,
    user: &User,
    body: &CheckoutRequestBody,
    idempotency_key: Option<&str>,
) -> Result<CheckoutOutcome, CheckoutError> {
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
                return outcome_from_envelope(&row.response).ok_or_else(|| {
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

    // The caller's live subscription decides what this
    // checkout IS (P1-069): first purchase, same-plan 422,
    // upgrade with a preview, or scheduled downgrade.
    let live = subscriptions::active_for_user(&state.pool, user.id)
        .await
        .map_err(|_| CheckoutError::Internal)?;
    let outcome = match live {
        Some(sub) => {
            let current = subscriptions::plan_for_subscription(&state.pool, sub.plan_id)
                .await
                .map_err(|_| CheckoutError::Internal)?
                .ok_or(CheckoutError::Internal)?;
            match classify(current.amount, plan.amount) {
                ChangeKind::Same => {
                    return Err(CheckoutError::PlanNotPurchasable(
                        "already subscribed to this plan",
                    ));
                }
                ChangeKind::Upgrade => {
                    let price_id =
                        plan.stripe_price_id
                            .ok_or(CheckoutError::PlanNotPurchasable(
                                "this plan has no price configured yet",
                            ))?;
                    let preview = match sub.current_period_end {
                        Some(end) => preview(
                            current.amount,
                            plan.amount,
                            period_start_from_end(end),
                            end,
                            now,
                        ),
                        None => preview_no_period(ChangeKind::Upgrade, now),
                    };
                    let outcome = checkout_with_provider(
                        state,
                        user,
                        &price_id,
                        sub.provider_subscription_id.as_deref(),
                    )
                    .await?;
                    CheckoutOutcome::Session(CheckoutResponse {
                        id: outcome.id,
                        url: outcome.url,
                        proration: Some(preview),
                    })
                }
                ChangeKind::Downgrade => {
                    let preview = match sub.current_period_end {
                        Some(end) => preview(
                            current.amount,
                            plan.amount,
                            period_start_from_end(end),
                            end,
                            now,
                        ),
                        None => preview_no_period(ChangeKind::Downgrade, now),
                    };
                    let effective_at = preview.effective_at;
                    plan_changes::insert(
                        &state.pool,
                        user.id,
                        sub.id,
                        current.id,
                        plan.id,
                        effective_at,
                    )
                    .await
                    .map_err(|err| match err {
                        RepoError::UniqueViolation { .. } => CheckoutError::ChangeAlreadyScheduled,
                        _ => CheckoutError::Internal,
                    })?;
                    CheckoutOutcome::Scheduled(ScheduledChangeResponse {
                        scheduled: true,
                        plan_code: plan.code.clone(),
                        from_plan_code: current.code.clone(),
                        effective_at,
                        proration: preview,
                    })
                }
            }
        }
        None => {
            // First purchase — the P1-062 path unchanged.
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
            let session = checkout_with_provider(state, user, &price_id, None).await?;
            CheckoutOutcome::Session(CheckoutResponse {
                id: session.id,
                url: session.url,
                proration: None,
            })
        }
    };

    // Store the response under the key so a retry replays it.
    // A stale row for the same key (expired) is deleted first,
    // so the new response can take the key. Losing the insert
    // race means a concurrent same-key request stored first —
    // re-read and replay the winner's response.
    if let Some(key) = idempotency_key {
        let envelope = json!({
            "request": canonical,
            "response": outcome.wire_body(),
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
                return outcome_from_envelope(&row.response).ok_or(CheckoutError::Internal);
            }
            return Err(CheckoutError::IdempotencyConflict);
        }
    }
    Ok(outcome)
}

/// The provider leg of a purchase/upgrade: reuse the
/// customer's provider id, build the success/cancel URLs
/// from config, and ask the provider for a session. Billing
/// must be configured (`state.billing` is `Some`) — the
/// caller (first purchase or upgrade) only reaches here for
/// a NEW checkout. `upgrade_from` is the live provider
/// subscription id an upgrade replaces (`None` for a first
/// purchase).
async fn checkout_with_provider(
    state: &AppState,
    user: &User,
    price_id: &str,
    upgrade_from: Option<&str>,
) -> Result<CheckoutSession, CheckoutError> {
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
    provider
        .create_checkout(CheckoutRequest {
            user_id: user.id,
            customer_id,
            price_id: price_id.to_string(),
            success_url,
            cancel_url,
            upgrade_from_provider_subscription_id: upgrade_from.map(str::to_owned),
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
        })
}
