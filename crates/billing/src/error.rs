//! detag-billing::error — the typed billing error (P1-059, FR-P-05).
//!
//! Every failure a [`BillingProvider`](crate::provider::BillingProvider)
//! can report, as data — no provider payloads, no secrets, no raw
//! webhook bodies in the message. The api maps these to HTTP responses
//! in its own layer (P1-062+); this crate never decides a status code.

use thiserror::Error;

/// What went wrong behind the [`BillingProvider`](crate::provider::BillingProvider) trait.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum BillingError {
    /// The provider's API call failed (network, 4xx/5xx, unexpected body).
    /// The message is the provider's own error text, never the request
    /// payload — it must not carry secrets or customer data.
    #[error("billing provider error: {0}")]
    Provider(String),

    /// The price id the caller asked to charge does not exist in the
    /// provider, or the plan has no price configured (the catalog's
    /// `stripe_price_id` is NULL — see P1-058).
    #[error("unknown or unconfigured price: {0}")]
    InvalidPrice(String),

    /// `fetch_subscription` found no subscription with this provider id.
    #[error("unknown subscription: {0}")]
    UnknownSubscription(String),

    /// `fetch_customer` found no customer with this provider id.
    #[error("unknown customer: {0}")]
    UnknownCustomer(String),

    /// Webhook signature verification failed (FR-P-04). The event is
    /// rejected outright — never processed, never mapped.
    #[error("webhook signature verification failed")]
    SignatureInvalid,

    /// The verified webhook payload is malformed for its event type
    /// (missing fields, wrong shapes). The event is rejected; the
    /// provider will retry if its policy says so.
    #[error("malformed webhook payload: {0}")]
    InvalidPayload(String),

    /// A subscription status string from the provider (or our own DB)
    /// is none of `trialing` / `active` / `past_due` / `canceled`.
    /// Statuses are never coerced — an unknown status fails closed
    /// rather than being read as `active`.
    #[error("unknown subscription status: {0}")]
    UnknownStatus(String),
}
