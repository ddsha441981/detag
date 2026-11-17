//! Billing portal (P1-067, FR-P-03): `POST /v1/billing/portal`.
//!
//! The handler's service: open the provider-hosted billing
//! portal for the caller's own customer — the page where they
//! manage payment methods, see invoices, and cancel. The
//! portal session is a short-lived URL; the user is sent back
//! to `{PF_PUBLIC_BASE_URL}/billing` when they leave it (the
//! console billing page lands in Phase 4; until then the URL
//! is the contract, not a live page).
//!
//! Rules, in order:
//! - The customer comes from our own rows: the newest
//!   `provider_customer_id` on any of the user's
//!   subscription rows (live or canceled — the customer
//!   survives cancellation, the P1-062 reuse rule). A user
//!   with no subscription row has no provider customer, so
//!   there is no portal to open: 404 `PF-2010`.
//! - Billing must be configured (`AppState::billing` is
//!   `Some`) — otherwise 503 `PF-5005`. The customer check
//!   runs first on purpose (the P1-062 ordering: local-data
//!   answers before the provider gate), so a user with no
//!   customer is 404 even while billing is unconfigured.
//! - A provider failure is 500 `PF-5000`, and no provider
//!   message ever reaches the problem body. An invalid
//!   stored customer id is config drift (our row disagrees
//!   with the provider) — 500 either way.
//! - No idempotency: every call mints a fresh portal
//!   session (the provider's own behavior — a portal session
//!   is a short-lived link, not a purchase), so a retry is
//!   simply a new link; there is nothing to replay.
//! - No audit row: nothing durable is created in our DB by
//!   opening the portal — the subscription changes the user
//!   makes inside it land with the webhook (P1-063/065).
//! - The portal's own configuration (which invoices show,
//!   whether cancel is at period end or immediate) is
//!   Stripe-dashboard-side: the adapter passes
//!   `configuration: None`, so the account's default portal
//!   configuration applies (see the pricing-config doc).

use crate::AppState;
use detag_billing::{PortalRequest, PortalSession};
use detag_core::repos::{subscriptions, users::User};
use thiserror::Error;

/// What opening the portal can fail with. The handler maps
/// each variant to its problem body; the provider's messages
/// never leave this module.
#[derive(Debug, Error)]
pub enum PortalError {
    /// No billing provider is configured (no Stripe key):
    /// the handler answers 503 `PF-5005`.
    #[error("billing provider not configured")]
    NotConfigured,
    /// The user has no provider customer on file (no
    /// subscription row): 404 `PF-2010`.
    #[error("no billing customer on file")]
    NoCustomer,
    /// The provider call failed: 500 `PF-5000`.
    #[error("billing provider error")]
    Provider,
    /// The customer lookup failed: 500 `PF-5000`.
    #[error("portal failed")]
    Internal,
}

/// Open the billing portal for `user`: return the hosted
/// portal URL.
pub async fn create_portal(state: &AppState, user: &User) -> Result<PortalSession, PortalError> {
    // Customer reuse: the newest provider customer id on any
    // of the user's subscription rows (live or canceled).
    let customer_id = subscriptions::latest_customer_id_for_user(&state.pool, user.id)
        .await
        .map_err(|_| PortalError::Internal)?
        .ok_or(PortalError::NoCustomer)?;

    // Billing must be configured to call the provider.
    let provider = state.billing.as_ref().ok_or(PortalError::NotConfigured)?;

    let base = state.settings.public_base_url.trim_end_matches('/');
    let return_url = format!("{base}/billing");
    provider
        .create_portal(PortalRequest {
            customer_id,
            return_url,
        })
        .await
        .map_err(|err| {
            tracing::error!(error = %err, "portal: provider call failed");
            PortalError::Provider
        })
}
