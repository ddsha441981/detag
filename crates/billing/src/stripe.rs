//! detag-billing::stripe — the Stripe adapter (P1-060, FR-P-05).
//!
//! [`StripeAdapter`] implements [`BillingProvider`](crate::provider::BillingProvider)
//! over `async-stripe` 0.41 (`runtime-tokio-hyper-rustls-webpki`:
//! tokio + hyper + rustls, no OpenSSL — the same TLS stack the rest
//! of the workspace uses). Domain types only cross the trait — no
//! `stripe::` type ever leaves this module.
//!
//! # Test mode is enforced at construction
//!
//! `StripeAdapter::new` accepts only `sk_test_...` keys, plus
//! `sk_live_...` keys when `allow_live` is true. A live key without
//! the flag is [`BillingError::Configuration`] — the adapter refuses
//! to exist, so no code path can charge a real card by accident.
//! `allow_live` is the owner flag (owner decision #3, "live payment
//! keys — TEST MODE only", still pending): it must come from an
//! explicit owner decision, and it defaults to false wherever the
//! adapter is constructed. Restricted keys (`rk_...`) are rejected
//! too — they start with neither prefix.
//!
//! # Webhook shape (ASSUMPTION-023)
//!
//! `map_webhook` takes the Stripe event's `data.object` as
//! [`BillingEvent::payload`](crate::provider::BillingEvent::payload) —
//! the resource object, not the whole event envelope. The handler
//! (P1-063) extracts `data.object` after
//! [`BillingProvider::verify_webhook_signature`](crate::provider::BillingProvider::verify_webhook_signature)
//! passes; the `event_id` / `event_type` fields on the envelope
//! already carry the id and the type. A `checkout.session.completed`
//! without a `subscription` id is ignored (`None`) — it is not one
//! of our subscription checkouts, and ignoring it answers 200 so
//! Stripe stops retrying. A subscription event whose `status` is none
//! of the four DB values (`incomplete`, `unpaid`, `paused`, ...) fails
//! closed with [`BillingError::UnknownStatus`] — the provider never
//! tells us what a user is entitled to, and a status we cannot store
//! must not be read as `active`.
//!
//! # What is (and is not) tested here
//!
//! The live-key guard, the webhook signature verification (against a
//! real HMAC-SHA256 header built in the test), and the whole
//! `map_webhook` mapping are unit tests in this module — they need no
//! network. The HTTP legs (`create_checkout`, `create_portal`,
//! `fetch_subscription`, `list_invoices`, `create_customer`,
//! `fetch_customer`) are implemented against the real client but have
//! NO live test here: there is no Stripe test-mode key on this VM and
//! no network path to `api.stripe.com` was exercised. Their first live
//! run is the P1-062 checkout endpoint's integration test, with a
//! test-mode key. A 404 from Stripe maps to the typed
//! `UnknownSubscription` / `UnknownCustomer` errors; a `resource_missing`
//! on checkout maps to `InvalidPrice`; anything else is
//! `BillingError::Provider` with Stripe's own message (never a secret
//! or a request payload).

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::Value;
use uuid::Uuid;

use crate::error::BillingError;
use crate::provider::{
    BillingEvent, BillingProvider, CheckoutRequest, CheckoutSession, Customer, DomainEvent,
    Invoice, PortalRequest, PortalSession, ProviderName, SubscriptionState, SubscriptionStatus,
    PROVIDER_STRIPE,
};

/// The Stripe implementation of [`BillingProvider`].
///
/// Built only through [`StripeAdapter::new`], which enforces test
/// mode (see the module docs). Holds the `async-stripe` client and
/// the webhook secret; neither is ever logged.
pub struct StripeAdapter {
    client: stripe::Client,
    webhook_secret: String,
}

// Manual `Debug`: the derived form would print the client (which
// holds the secret key) and the webhook secret. Nothing secret is
// ever printed.
impl std::fmt::Debug for StripeAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StripeAdapter").finish_non_exhaustive()
    }
}

impl StripeAdapter {
    /// Build the adapter. `secret_key` must start with `sk_test_` —
    /// or with `sk_live_` when `allow_live` is true (the owner flag,
    /// owner decision #3). Anything else (empty, `rk_...`, a
    /// placeholder with no `sk_` prefix) is
    /// [`BillingError::Configuration`]. `webhook_secret` must be
    /// non-empty: signature verification can never pass without it.
    pub fn new(
        secret_key: &str,
        webhook_secret: &str,
        allow_live: bool,
    ) -> Result<Self, BillingError> {
        if secret_key.is_empty() {
            return Err(BillingError::Configuration(
                "stripe secret key is empty".to_owned(),
            ));
        }
        if webhook_secret.is_empty() {
            return Err(BillingError::Configuration(
                "stripe webhook secret is empty".to_owned(),
            ));
        }
        let live = secret_key.starts_with("sk_live_");
        let test = secret_key.starts_with("sk_test_");
        if !live && !test {
            return Err(BillingError::Configuration(
                "stripe secret key must start with sk_test_ or sk_live_".to_owned(),
            ));
        }
        if live && !allow_live {
            return Err(BillingError::Configuration(
                "live stripe key rejected: test mode only (owner decision #3)".to_owned(),
            ));
        }
        Ok(Self {
            client: stripe::Client::new(secret_key),
            webhook_secret: webhook_secret.to_owned(),
        })
    }
}

/// A 404 from Stripe — the id does not exist there.
fn is_not_found(err: &stripe::StripeError) -> bool {
    matches!(err, stripe::StripeError::Stripe(req) if req.http_status == 404)
}

/// Stripe's `resource_missing` — the named object does not exist
/// (a checkout against a price Stripe has never seen answers this
/// way, with a 400, not a 404).
fn is_resource_missing(err: &stripe::StripeError) -> bool {
    matches!(
        err,
        stripe::StripeError::Stripe(req)
            if req.code == Some(stripe::ErrorCode::ResourceMissing)
    )
}

/// Any other Stripe failure, with Stripe's own message. The message
/// is the provider's error text — it never carries a secret or a
/// request payload.
fn provider_error(err: stripe::StripeError) -> BillingError {
    BillingError::Provider(err.to_string())
}

/// A required string field of a webhook payload, or
/// [`BillingError::InvalidPayload`] naming the missing field.
fn required_str<'a>(payload: &'a Value, field: &str) -> Result<&'a str, BillingError> {
    payload
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| BillingError::InvalidPayload(format!("missing {field}")))
}

/// A unix-seconds timestamp field of a webhook payload, or
/// [`BillingError::InvalidPayload`] when it is missing or out of
/// range.
fn required_timestamp(payload: &Value, field: &str) -> Result<DateTime<Utc>, BillingError> {
    let secs = payload
        .get(field)
        .and_then(Value::as_i64)
        .ok_or_else(|| BillingError::InvalidPayload(format!("missing {field}")))?;
    DateTime::from_timestamp(secs, 0)
        .ok_or_else(|| BillingError::InvalidPayload(format!("{field} out of range")))
}

/// Map a Stripe subscription object (`data.object` of a
/// `customer.subscription.*` event) to its domain state. The
/// `subscription_id` is the object's own `id`.
fn subscription_state_from_object(
    object: &Value,
) -> Result<(String, SubscriptionState), BillingError> {
    let subscription_id = required_str(object, "id")?.to_owned();
    let status = SubscriptionStatus::from_db_str(required_str(object, "status")?)?;
    let current_period_end = required_timestamp(object, "current_period_end")?;
    let cancel_at_period_end = object
        .get("cancel_at_period_end")
        .and_then(Value::as_bool)
        .ok_or_else(|| BillingError::InvalidPayload("missing cancel_at_period_end".to_owned()))?;
    Ok((
        subscription_id.clone(),
        SubscriptionState {
            provider_subscription_id: subscription_id,
            provider_customer_id: object
                .get("customer")
                .and_then(Value::as_str)
                .map(str::to_owned),
            status,
            current_period_end: Some(current_period_end),
            cancel_at_period_end,
        },
    ))
}

#[async_trait]
impl BillingProvider for StripeAdapter {
    fn provider_name(&self) -> ProviderName {
        PROVIDER_STRIPE
    }

    async fn create_checkout(&self, req: CheckoutRequest) -> Result<CheckoutSession, BillingError> {
        if req.price_id.is_empty() {
            return Err(BillingError::InvalidPrice(req.price_id));
        }
        // `client_reference_id` ties the session back to our user for
        // the webhook (P1-063 reads it off the completed session).
        let user_id = req.user_id.to_string();
        let customer_id = match &req.customer_id {
            Some(id) => Some(id.parse::<stripe::CustomerId>().map_err(|_| {
                BillingError::Provider(format!("invalid stored customer id: {id}"))
            })?),
            None => None,
        };
        let session = stripe::CheckoutSession::create(
            &self.client,
            stripe::CreateCheckoutSession {
                mode: Some(stripe::CheckoutSessionMode::Subscription),
                line_items: Some(vec![stripe::CreateCheckoutSessionLineItems {
                    price: Some(req.price_id.clone()),
                    quantity: Some(1),
                    ..Default::default()
                }]),
                success_url: Some(req.success_url.as_str()),
                cancel_url: Some(req.cancel_url.as_str()),
                customer: customer_id,
                client_reference_id: Some(user_id.as_str()),
                ..Default::default()
            },
        )
        .await
        .map_err(|err| {
            if is_resource_missing(&err) {
                BillingError::InvalidPrice(req.price_id.clone())
            } else {
                provider_error(err)
            }
        })?;
        Ok(CheckoutSession {
            id: session.id.as_str().to_owned(),
            url: session.url.ok_or_else(|| {
                BillingError::Provider("stripe checkout session has no url".to_owned())
            })?,
        })
    }

    async fn create_portal(&self, req: PortalRequest) -> Result<PortalSession, BillingError> {
        let customer_id = req.customer_id.parse::<stripe::CustomerId>().map_err(|_| {
            BillingError::Provider(format!("invalid stored customer id: {}", req.customer_id))
        })?;
        // `CreateBillingPortalSession` has no `Default` (its
        // `customer` is required), so every field is spelled out.
        let session = stripe::BillingPortalSession::create(
            &self.client,
            stripe::CreateBillingPortalSession {
                customer: customer_id,
                return_url: Some(req.return_url.as_str()),
                configuration: None,
                expand: &[],
                flow_data: None,
                locale: None,
                on_behalf_of: None,
            },
        )
        .await
        .map_err(provider_error)?;
        Ok(PortalSession { url: session.url })
    }

    async fn fetch_subscription(
        &self,
        provider_subscription_id: &str,
    ) -> Result<SubscriptionState, BillingError> {
        let id = provider_subscription_id
            .parse::<stripe::SubscriptionId>()
            .map_err(|_| BillingError::UnknownSubscription(provider_subscription_id.to_owned()))?;
        let sub = stripe::Subscription::retrieve(&self.client, &id, &[])
            .await
            .map_err(|err| {
                if is_not_found(&err) {
                    BillingError::UnknownSubscription(provider_subscription_id.to_owned())
                } else {
                    provider_error(err)
                }
            })?;
        Ok(SubscriptionState {
            provider_subscription_id: sub.id.as_str().to_owned(),
            provider_customer_id: Some(sub.customer.id().as_str().to_owned()),
            status: SubscriptionStatus::from_db_str(sub.status.as_str())?,
            current_period_end: Some(
                DateTime::from_timestamp(sub.current_period_end, 0).ok_or_else(|| {
                    BillingError::Provider(
                        "stripe subscription has an out-of-range current_period_end".to_owned(),
                    )
                })?,
            ),
            cancel_at_period_end: sub.cancel_at_period_end,
        })
    }

    async fn list_invoices(
        &self,
        customer_id: &str,
        limit: u32,
    ) -> Result<Vec<Invoice>, BillingError> {
        let customer = customer_id.parse::<stripe::CustomerId>().map_err(|_| {
            BillingError::Provider(format!("invalid stored customer id: {customer_id}"))
        })?;
        let list = stripe::Invoice::list(
            &self.client,
            &stripe::ListInvoices {
                customer: Some(customer),
                limit: Some(u64::from(limit)),
                ..Default::default()
            },
        )
        .await
        .map_err(provider_error)?;
        let mut out = Vec::with_capacity(list.data.len());
        for inv in list.data {
            let id = inv.id.as_str().to_owned();
            out.push(Invoice {
                id,
                number: inv.number,
                amount: inv.total.ok_or_else(|| {
                    BillingError::Provider("stripe invoice has no total".to_owned())
                })?,
                currency: {
                    let currency = inv.currency.ok_or_else(|| {
                        BillingError::Provider("stripe invoice has no currency".to_owned())
                    })?;
                    // `Currency` serializes to its lowercase code
                    // (`"inr"`); our contract spells it uppercase.
                    match serde_json::to_value(currency).map_err(|err| {
                        BillingError::Provider(format!(
                            "stripe invoice currency is not a code: {err}"
                        ))
                    })? {
                        Value::String(code) => code.to_ascii_uppercase(),
                        _ => {
                            return Err(BillingError::Provider(
                                "stripe invoice currency is not a code".to_owned(),
                            ));
                        }
                    }
                },
                status: inv
                    .status
                    .map(|status| status.as_str().to_owned())
                    .ok_or_else(|| {
                        BillingError::Provider("stripe invoice has no status".to_owned())
                    })?,
                created_at: {
                    let created = inv.created.ok_or_else(|| {
                        BillingError::Provider("stripe invoice has no created timestamp".to_owned())
                    })?;
                    DateTime::from_timestamp(created, 0).ok_or_else(|| {
                        BillingError::Provider(
                            "stripe invoice has an out-of-range created timestamp".to_owned(),
                        )
                    })?
                },
                hosted_url: inv.hosted_invoice_url,
            });
        }
        Ok(out)
    }

    async fn create_customer(&self, user_id: Uuid, email: &str) -> Result<Customer, BillingError> {
        // Our user id rides as metadata so support can trace a
        // Stripe customer back to our user (the subscription row is
        // the authoritative link; this is only a breadcrumb).
        let customer = stripe::Customer::create(
            &self.client,
            stripe::CreateCustomer {
                email: Some(email),
                metadata: Some(
                    [("detag_user_id".to_owned(), user_id.to_string())]
                        .into_iter()
                        .collect(),
                ),
                ..Default::default()
            },
        )
        .await
        .map_err(provider_error)?;
        Ok(Customer {
            provider_customer_id: customer.id.as_str().to_owned(),
            email: customer
                .email
                .ok_or_else(|| BillingError::Provider("stripe customer has no email".to_owned()))?,
        })
    }

    async fn fetch_customer(&self, provider_customer_id: &str) -> Result<Customer, BillingError> {
        let id = provider_customer_id
            .parse::<stripe::CustomerId>()
            .map_err(|_| BillingError::UnknownCustomer(provider_customer_id.to_owned()))?;
        let customer = stripe::Customer::retrieve(&self.client, &id, &[])
            .await
            .map_err(|err| {
                if is_not_found(&err) {
                    BillingError::UnknownCustomer(provider_customer_id.to_owned())
                } else {
                    provider_error(err)
                }
            })?;
        Ok(Customer {
            provider_customer_id: customer.id.as_str().to_owned(),
            email: customer
                .email
                .ok_or_else(|| BillingError::Provider("stripe customer has no email".to_owned()))?,
        })
    }

    fn verify_webhook_signature(
        &self,
        payload: &[u8],
        signature: &str,
    ) -> Result<(), BillingError> {
        let payload_str =
            std::str::from_utf8(payload).map_err(|_| BillingError::SignatureInvalid)?;
        stripe::Webhook::construct_event(payload_str, signature, &self.webhook_secret)
            .map(|_| ())
            .map_err(|_| BillingError::SignatureInvalid)
    }

    fn map_webhook(&self, event: BillingEvent) -> Result<Option<DomainEvent>, BillingError> {
        if event.provider != PROVIDER_STRIPE {
            return Err(BillingError::InvalidPayload(
                "event is not a stripe event".to_owned(),
            ));
        }
        match event.event_type.as_str() {
            "checkout.session.completed" => {
                // Only a subscription checkout maps: without a
                // subscription id this is someone else's session —
                // ignore it (200) rather than erroring (retry storm).
                match (
                    event.payload.get("customer").and_then(Value::as_str),
                    event.payload.get("subscription").and_then(Value::as_str),
                ) {
                    (Some(customer_id), Some(subscription_id)) => {
                        Ok(Some(DomainEvent::CheckoutCompleted {
                            customer_id: customer_id.to_owned(),
                            subscription_id: subscription_id.to_owned(),
                        }))
                    }
                    _ => Ok(None),
                }
            }
            "customer.subscription.created" | "customer.subscription.updated" => {
                let (subscription_id, state) = subscription_state_from_object(&event.payload)?;
                Ok(Some(DomainEvent::SubscriptionStatusChanged {
                    subscription_id,
                    status: state.status,
                    current_period_end: state.current_period_end,
                    cancel_at_period_end: state.cancel_at_period_end,
                }))
            }
            "customer.subscription.deleted" => Ok(Some(DomainEvent::SubscriptionDeleted {
                subscription_id: required_str(&event.payload, "id")?.to_owned(),
            })),
            "invoice.paid" => Ok(Some(DomainEvent::InvoicePaid {
                invoice_id: required_str(&event.payload, "id")?.to_owned(),
                subscription_id: event
                    .payload
                    .get("subscription")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            })),
            _ => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const TEST_KEY: &str = "sk_test_51FakeForTestsOnly00000000000000";
    const WEBHOOK_SECRET: &str = "whsec_test00000000000000000000000000";

    fn adapter() -> StripeAdapter {
        StripeAdapter::new(TEST_KEY, WEBHOOK_SECRET, false).expect("test key is valid")
    }

    /// Build a real Stripe signature header for `payload`, the way
    /// Stripe does: `t={unix_ts},v1={hex(hmac_sha256(secret,
    /// "{ts}.{payload}"))}`. `construct_event` enforces the 5-minute
    /// timestamp tolerance, so the header is built at "now".
    fn stripe_signature_header(payload: &str) -> String {
        use hmac::{Hmac, Mac};
        use sha2::Sha256;
        let ts = Utc::now().timestamp();
        let signed = format!("{ts}.{payload}");
        let mut mac = Hmac::<Sha256>::new_from_slice(WEBHOOK_SECRET.as_bytes())
            .expect("hmac accepts any key length");
        mac.update(signed.as_bytes());
        format!("t={ts},v1={}", hex::encode(mac.finalize().into_bytes()))
    }

    #[test]
    fn provider_name_is_stripe() {
        assert_eq!(adapter().provider_name(), PROVIDER_STRIPE);
    }

    #[test]
    fn live_key_rejected_without_owner_flag() {
        let err = StripeAdapter::new("sk_live_51FakeLive0000000000000000", WEBHOOK_SECRET, false)
            .expect_err("live key without the flag must fail");
        assert_eq!(
            err,
            BillingError::Configuration(
                "live stripe key rejected: test mode only (owner decision #3)".to_owned()
            )
        );
        // The error never carries the key value.
        assert!(!err.to_string().contains("sk_live_"));
    }

    #[test]
    fn live_key_accepted_with_owner_flag() {
        assert!(
            StripeAdapter::new("sk_live_51FakeLive0000000000000000", WEBHOOK_SECRET, true).is_ok()
        );
    }

    #[test]
    fn test_key_accepted_and_malformed_keys_rejected() {
        assert!(StripeAdapter::new(TEST_KEY, WEBHOOK_SECRET, false).is_ok());
        for bad in [
            "",
            "not-a-key",
            "rk_live_51FakeRestricted000000000",
            "pk_test_51Fake",
        ] {
            let err = StripeAdapter::new(bad, WEBHOOK_SECRET, false)
                .expect_err("malformed key must fail");
            assert!(
                matches!(err, BillingError::Configuration(_)),
                "bad key {bad:?} gave {err:?}"
            );
            // The error never leaks the key value (the empty key has
            // nothing to leak; `str::contains("")` is always true).
            assert!(
                bad.is_empty() || !err.to_string().contains(bad),
                "error leaks the key"
            );
        }
        let err =
            StripeAdapter::new(TEST_KEY, "", false).expect_err("empty webhook secret must fail");
        assert!(matches!(err, BillingError::Configuration(_)));
    }

    #[test]
    fn signature_verification_accepts_a_real_stripe_header() {
        // A full event envelope: `construct_event` parses the payload
        // into a `stripe::Event` after verifying the signature, so a
        // bare `{"id": ...}` object is not enough — the shape below
        // mirrors async-stripe's own webhook test (an
        // `invoiceitem.created` event, whose `data.object` parses).
        // The signature timestamp is "now" (`construct_event`
        // enforces the 5-minute tolerance against `Utc::now()`).
        let now = Utc::now().timestamp();
        let payload = format!(
            r#"{{"id":"evt_1","object":"event","api_version":"2024-06-20","created":{now},"data":{{"object":{{"id":"ii_123","object":"invoiceitem","amount":1000,"currency":"usd","customer":"cus_123","date":{now},"description":"Test Invoice Item","discountable":false,"invoice":"in_123","livemode":false,"metadata":{{}},"period":{{"start":{now},"end":{now}}},"proration":false,"quantity":3}}}},"livemode":false,"pending_webhooks":1,"request":{{"id":"req_123","idempotency_key":null}},"type":"invoiceitem.created"}}"#
        );
        let header = stripe_signature_header(&payload);
        assert!(adapter()
            .verify_webhook_signature(payload.as_bytes(), &header)
            .is_ok());
    }

    #[test]
    fn signature_verification_rejects_tampered_payload_wrong_secret_and_garbage() {
        let payload = r#"{"id":"evt_1","object":"event","type":"checkout.session.completed"}"#;
        let header = stripe_signature_header(payload);
        let adapter = adapter();
        // Same header, different body: the signature no longer covers it.
        assert_eq!(
            adapter
                .verify_webhook_signature(b"{}", &header)
                .expect_err("tampered payload must fail"),
            BillingError::SignatureInvalid
        );
        // A header built for a different secret.
        let other = StripeAdapter::new(TEST_KEY, "whsec_different0000000000000000000000", false)
            .expect("second adapter builds");
        assert_eq!(
            other
                .verify_webhook_signature(payload.as_bytes(), &header)
                .expect_err("wrong secret must fail"),
            BillingError::SignatureInvalid
        );
        // Garbage header, empty header, non-UTF8 body.
        assert_eq!(
            adapter
                .verify_webhook_signature(payload.as_bytes(), "t=1,v1=deadbeef")
                .expect_err("garbage signature must fail"),
            BillingError::SignatureInvalid
        );
        assert_eq!(
            adapter
                .verify_webhook_signature(payload.as_bytes(), "")
                .expect_err("empty signature must fail"),
            BillingError::SignatureInvalid
        );
        assert_eq!(
            adapter
                .verify_webhook_signature(&[0xff, 0xfe], &header)
                .expect_err("non-UTF8 body must fail"),
            BillingError::SignatureInvalid
        );
    }

    fn stripe_event(event_type: &str, payload: Value) -> BillingEvent {
        BillingEvent {
            provider: PROVIDER_STRIPE.to_owned(),
            event_id: "evt_test".to_owned(),
            event_type: event_type.to_owned(),
            payload,
        }
    }

    #[test]
    fn webhook_maps_real_stripe_event_shapes() {
        let adapter = adapter();
        // checkout.session.completed → CheckoutCompleted.
        assert_eq!(
            adapter
                .map_webhook(stripe_event(
                    "checkout.session.completed",
                    json!({"customer": "cus_123", "subscription": "sub_123"}),
                ))
                .expect("checkout maps"),
            Some(DomainEvent::CheckoutCompleted {
                customer_id: "cus_123".to_owned(),
                subscription_id: "sub_123".to_owned(),
            })
        );
        // A checkout with no subscription is not ours — ignored, not an error.
        assert_eq!(
            adapter
                .map_webhook(stripe_event(
                    "checkout.session.completed",
                    json!({"customer": "cus_123", "subscription": null}),
                ))
                .expect("non-subscription checkout is ignored"),
            None
        );
        // Subscription created/updated → status change, with the
        // period end converted from unix seconds.
        assert_eq!(
            adapter
                .map_webhook(stripe_event(
                    "customer.subscription.updated",
                    json!({"id": "sub_123", "customer": "cus_123", "status": "past_due", "current_period_end": 1794844800, "cancel_at_period_end": true}),
                ))
                .expect("subscription maps"),
            Some(DomainEvent::SubscriptionStatusChanged {
                subscription_id: "sub_123".to_owned(),
                status: SubscriptionStatus::PastDue,
                current_period_end: Some(
                    DateTime::from_timestamp(1794844800, 0).expect("in range")
                ),
                cancel_at_period_end: true,
            })
        );
        // Deleted → canceled (the row is never deleted).
        assert_eq!(
            adapter
                .map_webhook(stripe_event(
                    "customer.subscription.deleted",
                    json!({"id": "sub_123"}),
                ))
                .expect("delete maps"),
            Some(DomainEvent::SubscriptionDeleted {
                subscription_id: "sub_123".to_owned(),
            })
        );
        // Invoice paid, with and without a subscription link.
        assert_eq!(
            adapter
                .map_webhook(stripe_event(
                    "invoice.paid",
                    json!({"id": "in_123", "subscription": "sub_123"}),
                ))
                .expect("invoice maps"),
            Some(DomainEvent::InvoicePaid {
                invoice_id: "in_123".to_owned(),
                subscription_id: Some("sub_123".to_owned()),
            })
        );
        assert_eq!(
            adapter
                .map_webhook(stripe_event("invoice.paid", json!({"id": "in_124"})))
                .expect("one-off invoice maps"),
            Some(DomainEvent::InvoicePaid {
                invoice_id: "in_124".to_owned(),
                subscription_id: None,
            })
        );
        // Unknown-but-well-formed types are ignored, not errors.
        assert_eq!(
            adapter
                .map_webhook(stripe_event("invoice.finalized", json!({"id": "in_1"})))
                .expect("ignoring a type never errors"),
            None
        );
        assert_eq!(
            adapter
                .map_webhook(stripe_event("payment_intent.succeeded", json!({})))
                .expect("ignoring a type never errors"),
            None
        );
    }

    #[test]
    fn webhook_mapping_fails_closed_on_malformed_and_unknown_status() {
        let adapter = adapter();
        // Missing status on a subscription event.
        assert_eq!(
            adapter
                .map_webhook(stripe_event(
                    "customer.subscription.updated",
                    json!({"id": "sub_123", "current_period_end": 1794844800, "cancel_at_period_end": false}),
                ))
                .expect_err("missing status must fail"),
            BillingError::InvalidPayload("missing status".to_owned())
        );
        // A status Stripe sends but our DB cannot store is never
        // coerced to active.
        assert_eq!(
            adapter
                .map_webhook(stripe_event(
                    "customer.subscription.created",
                    json!({"id": "sub_123", "status": "incomplete", "current_period_end": 1794844800, "cancel_at_period_end": false}),
                ))
                .expect_err("incomplete must fail closed"),
            BillingError::UnknownStatus("incomplete".to_owned())
        );
        // An event from another provider is not mappable here.
        assert_eq!(
            adapter
                .map_webhook(BillingEvent {
                    provider: "razorpay".to_owned(),
                    event_id: "evt_r".to_owned(),
                    event_type: "subscription.updated".to_owned(),
                    payload: json!({}),
                })
                .expect_err("wrong provider must fail"),
            BillingError::InvalidPayload("event is not a stripe event".to_owned())
        );
    }
}
