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
//!   `dispatched_at` stands — unless the first delivery never
//!   finished dispatching (its `dispatched_at` is still NULL:
//!   the handler failed after the store). Then the redelivery
//!   IS the retry that applies the event (P1-065): dispatching
//!   again is safe because the handler is idempotent, and
//!   without it a transient failure would lose a paid
//!   subscription forever — the provider's retry would be
//!   answered as a no-op.
//! - Dispatch (P1-065): the verified event is mapped
//!   (`provider.map_webhook`, pure — P1-060) and the resulting
//!   `DomainEvent` is handed to
//!   [`crate::services::webhook_handlers::handle_domain_event`],
//!   which applies it to the `subscriptions` table through
//!   P1-064's state machine. The seam P1-063 left (log-only)
//!   is gone: the subscription writes live here now.
//!
//! No auth extractor runs on this route by design (the matrix
//! row is `Ignored`, not `Allowed`): the provider's signature
//! IS the credential. No secret, key, or raw body ever reaches
//! a log line or a problem body — the 400's detail is the
//! static `WebhookSignatureInvalid` sentence.

use crate::AppState;
use detag_billing::{BillingEvent, PROVIDER_RAZORPAY, PROVIDER_STRIPE};
use detag_core::{checksum::sha256_hex, repos::webhook_events};
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
    /// No billing provider is configured (no provider key, or
    /// the wrong provider is active for this receiver):
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
    store_and_dispatch(
        state,
        provider,
        PROVIDER_STRIPE,
        &event_id,
        &event_type,
        object,
        envelope,
    )
    .await
}

/// Receive one Razorpay webhook delivery: verify, store,
/// dispatch (P1-074, FR-P-04). The mirror of
/// [`receive_stripe_webhook`], over the Razorpay envelope
/// shape — the differences are all in the envelope, not the
/// flow:
///
/// - The header is `X-Razorpay-Signature` and the signature
///   is `hex(hmac_sha256(webhook_secret, raw_body))` — no
///   timestamp, no tolerance window: Razorpay signs the body
///   alone, and the adapter proves the primitive (P1-073).
/// - The envelope is `{"event": ..., "payload":
///   {"subscription": {"entity": {...}}}}` — the object the
///   adapter maps is `payload.subscription.entity`, not
///   `data.object`.
/// - Razorpay sends NO event id, so the ledger key is the
///   SHA-256 hex of the raw body: a redelivery sends the
///   identical bytes, so it hashes to the same key and is a
///   replay; two distinct events differ (their `created_at`
///   differs at least), so they never collide in practice —
///   and a collision would only ever swallow a duplicate.
/// - The receiver only runs when Razorpay is the ACTIVE
///   provider (`provider_name() == "razorpay"`): with Stripe
///   configured instead, the answer is 503 `PF-5005`
///   ("Razorpay is not configured"), not a misleading 400 —
///   there is no Razorpay secret to verify against, and a
///   503 tells Razorpay to retry later rather than dropping
///   the event.
///
/// The error contract is the Stripe one: a bad signature is
/// 400 `PF-2013` (the endpoint's ONLY 4xx); billing
/// unconfigured (or the wrong provider active) is 503
/// `PF-5005`; every verified delivery is 200 with a receipt.
pub async fn receive_razorpay_webhook(
    state: &AppState,
    raw_body: &[u8],
    signature: Option<&str>,
) -> Result<WebhookOutcome, WebhookError> {
    let Some(provider) = &state.billing else {
        return Err(WebhookError::NotConfigured);
    };
    if provider.provider_name() != PROVIDER_RAZORPAY {
        return Err(WebhookError::NotConfigured);
    }
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
    let Some((event_id, event_type, object, envelope)) = parse_razorpay_envelope(raw_body) else {
        tracing::warn!("razorpay webhook: signature verified but envelope unparseable");
        return Ok(WebhookOutcome::Unparseable);
    };
    store_and_dispatch(
        state,
        provider,
        PROVIDER_RAZORPAY,
        &event_id,
        &event_type,
        object,
        envelope,
    )
    .await
}

/// The store + replay + dispatch flow both receivers share
/// (P1-074): insert into the ledger (the UNIQUE key on
/// (`provider`, `event_id`) IS the idempotency), dispatch a
/// new event, answer a replay 200 without dispatching —
/// unless the first delivery never finished dispatching
/// (`dispatched_at` still NULL), in which case the redelivery
/// IS the retry that applies the event (P1-065).
async fn store_and_dispatch(
    state: &AppState,
    provider: &detag_billing::DynBillingProvider,
    provider_name: &str,
    event_id: &str,
    event_type: &str,
    object: Value,
    envelope: Value,
) -> Result<WebhookOutcome, WebhookError> {
    // The ledger insert is the idempotency: the UNIQUE key
    // on (provider, event_id) makes a redelivery lose the
    // race, and the loser answers 200 without dispatching.
    let stored =
        webhook_events::insert(&state.pool, provider_name, event_id, event_type, &envelope)
            .await
            .map_err(|_| WebhookError::Internal)?;
    if !stored {
        // A replay is a no-op — unless the first delivery
        // never finished dispatching (its `dispatched_at` is
        // still NULL: the handler failed after the store).
        // Then this redelivery is the retry that applies the
        // event (P1-065): the handler is idempotent, so
        // dispatching again cannot double-apply, and without
        // it a transient failure would lose the event — the
        // provider's retry would be answered as a no-op.
        let row = webhook_events::get(&state.pool, provider_name, event_id)
            .await
            .map_err(|_| WebhookError::Internal)?;
        if row.as_ref().and_then(|row| row.dispatched_at).is_some() {
            return Ok(WebhookOutcome::Replayed {
                event_id: event_id.to_owned(),
            });
        }
        let Some(object) = row.as_ref().and_then(|row| {
            if provider_name == PROVIDER_RAZORPAY {
                parse_razorpay_envelope_value(&row.payload).map(|(_, obj, _)| obj)
            } else {
                parse_envelope_value(&row.payload).map(|(_, _, obj, _)| obj)
            }
        }) else {
            tracing::warn!(
                event_id = event_id,
                "{provider_name} webhook: stored envelope no longer parses; replay answered without dispatch"
            );
            return Ok(WebhookOutcome::Replayed {
                event_id: event_id.to_owned(),
            });
        };
        let mapped = dispatch(state, provider, provider_name, event_id, event_type, object).await?;
        return Ok(WebhookOutcome::Accepted {
            event_id: event_id.to_owned(),
            mapped,
        });
    }
    let mapped = dispatch(state, provider, provider_name, event_id, event_type, object).await?;
    Ok(WebhookOutcome::Accepted {
        event_id: event_id.to_owned(),
        mapped,
    })
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
    provider_name: &str,
    event_id: &str,
    event_type: &str,
    object: Value,
) -> Result<bool, WebhookError> {
    let event = BillingEvent {
        provider: provider_name.to_owned(),
        event_id: event_id.to_owned(),
        event_type: event_type.to_owned(),
        payload: object,
    };
    let mapped = match provider.map_webhook(event) {
        Ok(Some(domain_event)) => {
            // P1-065: apply the event to the subscriptions
            // table. A handler outcome that is about the event
            // (unresolvable, stale, already recorded) is a
            // Noop — logged inside the handler, still 200,
            // still stamped. Only a genuine failure (the DB, a
            // provider call) is an error, and the receiver
            // answers it 500 so the provider retries — the
            // redelivery then re-dispatches (see the replay
            // path above).
            crate::services::webhook_handlers::handle_domain_event(
                state,
                provider,
                &domain_event,
                event_id,
            )
            .await
            .map_err(|_| WebhookError::Internal)?;
            true
        }
        Ok(None) => {
            tracing::info!(
                event_id = event_id,
                event_type = event_type,
                "{provider_name} webhook: event type ignored"
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
                "{provider_name} webhook: verified event could not be mapped"
            );
            false
        }
    };
    webhook_events::mark_dispatched(&state.pool, provider_name, event_id, state.clock.now())
        .await
        .map_err(|_| WebhookError::Internal)?;
    Ok(mapped)
}

/// Parse a verified envelope into (`id`, `type`,
/// `data.object`, the whole envelope). `None` when the body
/// is not JSON, or any of the three is missing or mistyped.
fn parse_envelope(raw_body: &[u8]) -> Option<(String, String, Value, Value)> {
    let envelope: Value = serde_json::from_slice(raw_body).ok()?;
    parse_envelope_value(&envelope)
}

/// The same parse over an already-decoded envelope (a stored
/// ledger row, for the replay re-dispatch).
fn parse_envelope_value(envelope: &Value) -> Option<(String, String, Value, Value)> {
    let event_id = envelope.get("id")?.as_str()?.to_owned();
    let event_type = envelope.get("type")?.as_str()?.to_owned();
    let object = envelope.get("data")?.get("object")?.clone();
    Some((event_id, event_type, object, envelope.clone()))
}

/// Parse a verified Razorpay envelope into (ledger event
/// id, event type, the subscription entity, the whole
/// envelope). Razorpay sends no event id, so the id is the
/// SHA-256 hex of the raw body (see
/// [`receive_razorpay_webhook`]). `None` when the body is not
/// JSON, or `event` / `payload.subscription.entity` is missing
/// or mistyped.
fn parse_razorpay_envelope(raw_body: &[u8]) -> Option<(String, String, Value, Value)> {
    let envelope: Value = serde_json::from_slice(raw_body).ok()?;
    let (event_type, object, envelope) = parse_razorpay_envelope_value(&envelope)?;
    Some((sha256_hex(raw_body), event_type, object, envelope))
}

/// The same parse over an already-decoded envelope (a stored
/// ledger row, for the replay re-dispatch): the event type,
/// the subscription entity, and the whole envelope. The ledger
/// key is the body hash, which cannot be re-derived from the
/// stored envelope — the caller already holds it.
fn parse_razorpay_envelope_value(envelope: &Value) -> Option<(String, Value, Value)> {
    let event_type = envelope.get("event")?.as_str()?.to_owned();
    let object = envelope
        .get("payload")?
        .get("subscription")?
        .get("entity")?
        .clone();
    Some((event_type, object, envelope.clone()))
}

/// The signature header name, for the handler.
pub fn signature_header() -> &'static str {
    SIGNATURE_HEADER
}

/// The `X-Razorpay-Signature` header, as Razorpay sends it.
const RAZORPAY_SIGNATURE_HEADER: &str = "x-razorpay-signature";

/// The Razorpay signature header name, for the handler.
pub fn razorpay_signature_header() -> &'static str {
    RAZORPAY_SIGNATURE_HEADER
}
