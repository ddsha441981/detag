//! Webhook receiver (P1-063, FR-P-04): `POST /v1/webhooks/stripe`.
//!
//! The handler's service: verify the provider's signature over
//! the RAW body (no JSON parse before the check — the signature
//! covers the exact bytes Stripe sent), store the event in the
//! `webhook_events` ledger, then dispatch it. The ledger's
//! (`provider`, `event_id`) UNIQUE key IS the idempotency: a
//! redelivery inserts nothing and is answered 200 without
//! dispatching twice (the AC).
//!
//! Rules, in order:
//! - Billing must be configured (`AppState::billing` is
//!   `Some`) — otherwise 503 `PF-5005`. There is no secret to
//!   verify against without a provider, and a 503 tells Stripe
//!   to retry later rather than dropping the event.
//! - The signature must verify — a missing `Stripe-Signature`
//!   header, a wrong secret, a tampered body, or a stale
//!   timestamp is 400 `PF-2013` (new code, the next free
//!   PF-2xxx slot; Stripe's own guidance is 400 for a bad
//!   signature). This is the ONLY 4xx the endpoint returns:
//!   everything after a verified signature is 200, so the
//!   provider stops retrying.
//! - The envelope must parse (`id`, `type`, `data.object`):
//!   a signature-verified event that does not parse cannot
//!   come from Stripe (its `construct_event` parses the same
//!   envelope while verifying), so an unparseable body is a
//!   bug, not a client error — it is logged and answered 200
//!   with no row stored (there is no event id to key the row
//!   on). The same holds for a verified event `map_webhook`
//!   cannot map (malformed for its type, unknown status):
//!   logged, marked dispatched, answered 200 — no retry storm
//!   (P1-065's rule, applied here too).
//! - A replay (the event id is already in the ledger) is 200
//!   with no dispatch and no stamp moved: the first delivery's
//!   `dispatched_at` stands.
//! - Dispatch (P1-063's seam): the verified event is mapped
//!   (`provider.map_webhook`, pure — P1-060) and the resulting
//!   `DomainEvent` is handed to [`dispatch_domain_event`].
//!   P1-063's dispatcher only records the dispatch (the
//!   `dispatched_at` stamp + an info log): the subscription
//!   writes land in P1-065, which reads the stored envelopes
//!   — the ledger is the durable queue, so no event is lost
//!   by dispatching later. The state machine (P1-064) owns
//!   transition validation, so this ticket applies no status
//!   change itself.
//!
//! No auth extractor runs on this route by design (the matrix
//! row is `Ignored`, not `Allowed`): the provider's signature
//! IS the credential. No secret, key, or raw body ever reaches
//! a log line or a problem body — the 400's detail is the
//! static `WebhookSignatureInvalid` sentence.

use crate::AppState;
use detag_billing::{BillingEvent, DomainEvent, PROVIDER_STRIPE};
use detag_core::repos::webhook_events;
use serde::Serialize;
use serde_json::Value;
use thiserror::Error;
use utoipa::ToSchema;

/// The receipt the endpoint answers with (200, replay or not).
/// Stripe ignores the body; the shape exists so the OpenAPI
/// spec documents a real response instead of an empty one.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct WebhookReceipt {
    /// The event was received (stored, replayed, or ignored —
    /// all three are a receipt, never an error).
    pub received: bool,
}

/// What the receiver did with one delivery. The handler
/// answers 200 for every variant — the only 4xx is a bad
/// signature, which is an error, not an outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WebhookOutcome {
    /// A new event: stored and dispatched (or ignored when
    /// the provider maps it to nothing).
    Accepted {
        /// The provider's event id.
        event_id: String,
        /// Whether `map_webhook` produced a domain event
        /// (`false` = an ignored type — still 200).
        mapped: bool,
    },
    /// The event id is already in the ledger: answered 200,
    /// dispatched nothing, moved no stamp.
    Replayed {
        /// The provider's event id.
        event_id: String,
    },
    /// The signature verified but the envelope does not parse
    /// (`id` / `type` / `data.object` missing or mistyped):
    /// logged, answered 200, no row stored.
    Unparseable,
}

/// What the receiver can fail with. The handler maps each
/// variant to its problem body; provider messages never
/// leave this module.
#[derive(Debug, Error)]
pub enum WebhookError {
    /// No billing provider is configured (no Stripe key):
    /// the handler answers 503 `PF-5005`.
    #[error("billing provider not configured")]
    NotConfigured,
    /// The signature header is missing or does not verify:
    /// the handler answers 400 `PF-2013`.
    #[error("webhook signature verification failed")]
    SignatureInvalid,
    /// The event ledger could not be read/written: 500
    /// `PF-5000`.
    #[error("webhook event store failed")]
    Internal,
}

/// The `Stripe-Signature` header, as Stripe sends it.
const SIGNATURE_HEADER: &str = "stripe-signature";

/// Receive one Stripe webhook delivery: verify, store,
/// dispatch. `signature` is the raw `Stripe-Signature`
/// header value (`None` when the header is absent).
pub async fn receive_stripe_webhook(
    state: &AppState,
    raw_body: &[u8],
    signature: Option<&str>,
) -> Result<WebhookOutcome, WebhookError> {
    let Some(provider) = &state.billing else {
        return Err(WebhookError::NotConfigured);
    };
    let Some(signature) = signature else {
        return Err(WebhookError::SignatureInvalid);
    };
    // The signature covers the exact bytes received — no
    // parse happens before this check, so a tampered body
    // can never reach the mapping.
    if provider
        .verify_webhook_signature(raw_body, signature)
        .is_err()
    {
        return Err(WebhookError::SignatureInvalid);
    }
    let Some((event_id, event_type, object, envelope)) = parse_envelope(raw_body) else {
        tracing::warn!("stripe webhook: signature verified but envelope unparseable");
        return Ok(WebhookOutcome::Unparseable);
    };
    // The ledger insert is the idempotency: the UNIQUE key
    // on (provider, event_id) makes a redelivery lose the
    // race, and the loser answers 200 without dispatching.
    let stored = webhook_events::insert(
        &state.pool,
        PROVIDER_STRIPE,
        &event_id,
        &event_type,
        &envelope,
    )
    .await
    .map_err(|_| WebhookError::Internal)?;
    if !stored {
        return Ok(WebhookOutcome::Replayed { event_id });
    }
    let mapped = dispatch(state, provider, &event_id, &event_type, object).await?;
    Ok(WebhookOutcome::Accepted { event_id, mapped })
}

/// Map the verified event and hand the result to the
/// dispatcher, then stamp the row dispatched. Returns whether
/// `map_webhook` produced a domain event (`false` = ignored
/// or unmappable). A mapping failure (malformed for its type,
/// unknown status) is logged and still stamped: the event was
/// processed, the provider must not retry it, and the stored
/// envelope stays for P1-065 to inspect.
async fn dispatch(
    state: &AppState,
    provider: &detag_billing::DynBillingProvider,
    event_id: &str,
    event_type: &str,
    object: Value,
) -> Result<bool, WebhookError> {
    let event = BillingEvent {
        provider: PROVIDER_STRIPE.to_owned(),
        event_id: event_id.to_owned(),
        event_type: event_type.to_owned(),
        payload: object,
    };
    let mapped = match provider.map_webhook(event) {
        Ok(Some(domain_event)) => {
            dispatch_domain_event(&domain_event);
            true
        }
        Ok(None) => {
            tracing::info!(
                event_id = event_id,
                event_type = event_type,
                "stripe webhook: event type ignored"
            );
            false
        }
        Err(_) => {
            // The error text is never logged: a mapping
            // failure names payload fields, and the envelope
            // is already stored for inspection.
            tracing::warn!(
                event_id = event_id,
                event_type = event_type,
                "stripe webhook: verified event could not be mapped"
            );
            false
        }
    };
    webhook_events::mark_dispatched(&state.pool, PROVIDER_STRIPE, event_id, state.clock.now())
        .await
        .map_err(|_| WebhookError::Internal)?;
    Ok(mapped)
}

/// The dispatch seam (P1-063): hand a mapped domain event to
/// its handler. This ticket's dispatcher only records the
/// dispatch — the subscription writes land in P1-065 (which
/// reads the stored envelopes) behind P1-064's state machine.
/// Logging the variant only: no ids, no payload.
fn dispatch_domain_event(event: &DomainEvent) {
    let variant = match event {
        DomainEvent::CheckoutCompleted { .. } => "checkout_completed",
        DomainEvent::SubscriptionStatusChanged { .. } => "subscription_status_changed",
        DomainEvent::SubscriptionDeleted { .. } => "subscription_deleted",
        DomainEvent::InvoicePaid { .. } => "invoice_paid",
    };
    tracing::info!(
        domain_event = variant,
        "stripe webhook: domain event dispatched (application lands in P1-065)"
    );
}

/// Parse a verified envelope into (`id`, `type`,
/// `data.object`, the whole envelope). `None` when the body
/// is not JSON, or any of the three is missing or mistyped.
fn parse_envelope(raw_body: &[u8]) -> Option<(String, String, Value, Value)> {
    let envelope: Value = serde_json::from_slice(raw_body).ok()?;
    let event_id = envelope.get("id")?.as_str()?.to_owned();
    let event_type = envelope.get("type")?.as_str()?.to_owned();
    let object = envelope.get("data")?.get("object")?.clone();
    Some((event_id, event_type, object, envelope))
}

/// The signature header name, for the handler.
pub fn signature_header() -> &'static str {
    SIGNATURE_HEADER
}
