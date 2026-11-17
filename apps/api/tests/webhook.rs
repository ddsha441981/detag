//! P1-063: the webhook receiver — `POST /v1/webhooks/stripe`.
//!
//! The provider is the REAL `StripeAdapter` (a fake
//! `sk_test_` key + a test webhook secret — construction and
//! signature verification need no network): the suite proves
//! the real HMAC-SHA256 path end to end, through the real
//! HTTP layer — not a fake whose `verify` always says yes.
//! The adapter's own unit tests (P1-060) prove the primitive
//! against a real header; this suite proves the receiver
//! around it. The Stripe HTTP legs are still untested live
//! (no test-mode key on this VM — ASSUMPTION-023): no test
//! here calls the provider.
//!
//! What this suite proves:
//!
//! - `webhook_unconfigured_returns_503` — no provider in the
//!   state (the default spawn) is 503 `PF-5005`.
//! - `webhook_missing_signature_is_400` — a delivery with no
//!   `Stripe-Signature` header is 400 `PF-2013`, and no row
//!   is stored.
//! - `webhook_tamper_rejected` — the AC: a header signed for
//!   one body, sent with another, is 400 `PF-2013`, and no
//!   row is stored.
//! - `webhook_wrong_secret_rejected` — a header built with a
//!   different secret is 400 `PF-2013`.
//! - `webhook_valid_ignored_event_stored` — a verified event
//!   the app ignores (`invoiceitem.created`, the P1-060
//!   shape) is 200 with a receipt, one ledger row, and a
//!   `dispatched_at` stamp; no credential is needed.
//! - `webhook_checkout_completed_dispatched` — a verified
//!   `checkout.session.completed` is 200, stored, and
//!   dispatched. The payload names no user (no
//!   `client_reference_id`), so P1-065's handler logs it and
//!   writes no row — the row-creation proof lives in the
//!   handlers suite (`webhook_handlers.rs`).
//! - `webhook_replay_is_noop` — the AC: the same event id
//!   posted twice is 200 both times, exactly one ledger row,
//!   and the `dispatched_at` stamp does not move.
//! - `webhook_verified_but_unparseable_is_200` — a verified
//!   delivery whose envelope cannot be parsed is 200 with no
//!   row (the "4xx only on bad signature" rule). The real
//!   adapter rejects unparseable envelopes as a bad signature
//!   (its `construct_event` parses while verifying), so this
//!   case runs against an always-accepting test double — the
//!   point is the receiver's rule, not the adapter.

mod common;

use std::sync::Arc;

use async_trait::async_trait;
use axum::http::{Method, Request, StatusCode};
use chrono::{DateTime, Utc};
use common::{DelegatingProvider, TestApp, TestFixtures};
use detag_billing::{
    BillingError, BillingEvent, BillingProvider, CheckoutRequest, CheckoutSession, Customer,
    DomainEvent, DynBillingProvider, Invoice, PortalRequest, PortalSession, ProviderName,
    StripeAdapter, SubscriptionState, PROVIDER_STRIPE,
};
use detag_core::{
    clock::{Clock, ManualClock},
    repos::webhook_events,
};
use hmac::{Hmac, KeyInit, Mac};
use serde_json::json;
use sha2::Sha256;
use uuid::Uuid;

/// The fake Stripe key the adapter is built with. It never
/// leaves this file and never touches the network — the
/// adapter's verify + map are pure.
const TEST_KEY: &str = "sk_test_51FakeForTestsOnly00000000000000";
/// The test webhook secret. A test value, not a real secret —
/// the same constant the billing crate's own tests use.
const WEBHOOK_SECRET: &str = "whsec_test00000000000000000000000000";

/// The request id: a valid UUID, echoed by the middleware.
const REQUEST_ID: &str = "123e4567-e89b-42d3-a456-426614174109";

/// A per-request XFF address: each request gets its own IP
/// bucket, so the tests never trip each other's rate limits
/// (the P1-003 lesson; the full-UUID form, FLAKY-006).
fn unique_xff() -> String {
    format!("203.0.113.{}", Uuid::now_v7())
}

/// Build a real Stripe signature header for `payload`, the way
/// Stripe does: `t={unix_ts},v1={hex(hmac_sha256(secret,
/// "{ts}.{payload}"))}`. `construct_event` enforces the
/// 5-minute timestamp tolerance against the REAL clock, so the
/// header is built at "now" even though the app clock is frozen.
fn stripe_signature_header(payload: &str, secret: &str) -> String {
    let ts = Utc::now().timestamp();
    let signed = format!("{ts}.{payload}");
    let mut mac =
        Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("hmac accepts any key length");
    mac.update(signed.as_bytes());
    format!("t={ts},v1={}", hex::encode(mac.finalize().into_bytes()))
}

/// The app on a frozen clock with the REAL Stripe adapter.
/// Returns (app, clock).
async fn webhook_app(frozen_at: DateTime<Utc>) -> (TestApp, Arc<ManualClock>) {
    let fixtures = TestFixtures::from_env().await;
    let settings = fixtures.settings();
    let clock = Arc::new(ManualClock::new(frozen_at));
    let adapter = StripeAdapter::new(TEST_KEY, WEBHOOK_SECRET, false)
        .expect("test key and secret build the adapter");
    let provider: DynBillingProvider = Arc::new(adapter) as DynBillingProvider;
    let app = TestApp::spawn_with_fixtures_settings_clock_and_provider(
        fixtures,
        settings,
        Arc::clone(&clock) as Arc<dyn Clock>,
        provider,
    )
    .await;
    (app, clock)
}

/// The app on a frozen clock with the DELEGATING provider: the
/// real Stripe adapter for signature verification + mapping,
/// canned `fetch_subscription` (P1-065 — the handler fetches
/// the subscription a checkout completed, and the real adapter
/// would call Stripe live). The checkout tests use this app;
/// the signature / ignore / replay-stamp tests keep the real
/// adapter above, so the HMAC path stays proven against it.
async fn webhook_handler_app(frozen_at: DateTime<Utc>) -> (TestApp, Arc<ManualClock>) {
    let fixtures = TestFixtures::from_env().await;
    let settings = fixtures.settings();
    let clock = Arc::new(ManualClock::new(frozen_at));
    let adapter = StripeAdapter::new(TEST_KEY, WEBHOOK_SECRET, false)
        .expect("test key and secret build the adapter");
    let provider: DynBillingProvider =
        Arc::new(DelegatingProvider::new(adapter)) as DynBillingProvider;
    let app = TestApp::spawn_with_fixtures_settings_clock_and_provider(
        fixtures,
        settings,
        Arc::clone(&clock) as Arc<dyn Clock>,
        provider,
    )
    .await;
    (app, clock)
}

/// App with no provider (the default spawn) — billing is not
/// configured, so the webhook is 503.
async fn unconfigured_app() -> TestApp {
    let fixtures = TestFixtures::from_env().await;
    let settings = fixtures.settings();
    TestApp::spawn_with_fixtures_and_settings(fixtures, settings).await
}

/// A test double whose signature check always passes, so the
/// receiver's post-verification rules (unparseable envelope
/// → 200) are provable without the real adapter, which
/// rejects unparseable envelopes while verifying.
#[derive(Debug)]
struct AlwaysOkProvider;

#[async_trait]
impl BillingProvider for AlwaysOkProvider {
    fn provider_name(&self) -> ProviderName {
        PROVIDER_STRIPE
    }

    async fn create_checkout(
        &self,
        _req: CheckoutRequest,
    ) -> Result<CheckoutSession, BillingError> {
        Err(BillingError::Provider("unused in webhook tests".to_owned()))
    }

    async fn create_portal(&self, _req: PortalRequest) -> Result<PortalSession, BillingError> {
        Err(BillingError::Provider("unused in webhook tests".to_owned()))
    }

    async fn fetch_subscription(
        &self,
        provider_subscription_id: &str,
    ) -> Result<SubscriptionState, BillingError> {
        Err(BillingError::UnknownSubscription(
            provider_subscription_id.to_owned(),
        ))
    }

    async fn list_invoices(
        &self,
        _customer_id: &str,
        _limit: u32,
    ) -> Result<Vec<Invoice>, BillingError> {
        Ok(vec![])
    }

    async fn create_customer(&self, user_id: Uuid, email: &str) -> Result<Customer, BillingError> {
        Ok(Customer {
            provider_customer_id: format!("cus_test_{user_id}"),
            email: email.to_owned(),
        })
    }

    async fn fetch_customer(&self, provider_customer_id: &str) -> Result<Customer, BillingError> {
        Err(BillingError::UnknownCustomer(
            provider_customer_id.to_owned(),
        ))
    }

    fn verify_webhook_signature(
        &self,
        _payload: &[u8],
        _signature: &str,
    ) -> Result<(), BillingError> {
        Ok(())
    }

    fn map_webhook(&self, _event: BillingEvent) -> Result<Option<DomainEvent>, BillingError> {
        Ok(None)
    }
}

/// App on a frozen clock with the always-accepting double.
async fn always_ok_app(frozen_at: DateTime<Utc>) -> TestApp {
    let fixtures = TestFixtures::from_env().await;
    let settings = fixtures.settings();
    let clock = Arc::new(ManualClock::new(frozen_at));
    let provider: DynBillingProvider = Arc::new(AlwaysOkProvider) as DynBillingProvider;
    TestApp::spawn_with_fixtures_settings_clock_and_provider(
        fixtures,
        settings,
        Arc::clone(&clock) as Arc<dyn Clock>,
        provider,
    )
    .await
}

/// POST one delivery to the webhook. `signature` is the raw
/// `Stripe-Signature` header value (`None` = header absent).
/// `credential` is an optional Bearer token — the route takes
/// no credential by design, so it must change nothing.
async fn post_webhook(
    app: &TestApp,
    payload: &str,
    signature: Option<&str>,
    credential: Option<&str>,
) -> common::TestResponse {
    let mut req = Request::builder()
        .method(Method::POST)
        .uri("/v1/webhooks/stripe")
        .header("x-request-id", REQUEST_ID)
        .header("x-forwarded-for", unique_xff())
        .header("content-type", "application/json");
    if let Some(signature) = signature {
        req = req.header("stripe-signature", signature);
    }
    if let Some(credential) = credential {
        req = req.header("authorization", format!("Bearer {credential}"));
    }
    app.request(
        req.body(axum::body::Body::from(payload.as_bytes().to_vec()))
            .unwrap(),
    )
    .await
}

/// A verified-but-ignored event envelope (`invoiceitem.created`,
/// the P1-060 shape — `construct_event` parses the payload into
/// a `stripe::Event`, so the object must be a full invoice item).
fn ignored_event_payload(event_id: &str, now: i64) -> String {
    format!(
        r#"{{"id":"{event_id}","object":"event","api_version":"2024-06-20","created":{now},"data":{{"object":{{"id":"ii_123","object":"invoiceitem","amount":1000,"currency":"usd","customer":"cus_123","date":{now},"description":"Test Invoice Item","discountable":false,"invoice":"in_123","livemode":false,"metadata":{{}},"period":{{"start":{now},"end":{now}}},"proration":false,"quantity":3}}}},"livemode":false,"pending_webhooks":1,"request":{{"id":"req_123","idempotency_key":null}},"type":"invoiceitem.created"}}"#
    )
}

/// A verified checkout-completed envelope. The session object
/// carries the fields `construct_event` requires (id,
/// automatic_tax, created, custom_fields, custom_text,
/// expires_at, livemode, mode, payment_method_types,
/// payment_status, shipping_options) plus the customer and
/// subscription ids `map_webhook` reads.
fn checkout_completed_payload(event_id: &str, now: i64) -> String {
    let expires = now + 1800;
    format!(
        r#"{{"id":"{event_id}","object":"event","api_version":"2024-06-20","created":{now},"data":{{"object":{{"id":"cs_test_123","object":"checkout.session","automatic_tax":{{"enabled":false}},"created":{now},"custom_fields":[],"custom_text":{{}},"customer":"cus_test_123","expires_at":{expires},"livemode":false,"mode":"subscription","payment_method_types":["card"],"payment_status":"paid","shipping_options":[],"subscription":"sub_test_123"}}}},"livemode":false,"pending_webhooks":1,"request":{{"id":"req_123","idempotency_key":null}},"type":"checkout.session.completed"}}"#
    )
}

/// The ledger row for (`stripe`, `event_id`), if one exists.
async fn ledger_row(app: &TestApp, event_id: &str) -> Option<webhook_events::WebhookEventRow> {
    webhook_events::get(&app.pool, PROVIDER_STRIPE, event_id)
        .await
        .expect("ledger read")
}

#[tokio::test]
async fn webhook_unconfigured_returns_503() {
    let app = unconfigured_app().await;
    let payload = ignored_event_payload("evt_unconfigured_1", Utc::now().timestamp());
    let res = post_webhook(&app, &payload, None, None).await;
    assert_eq!(
        res.status,
        StatusCode::SERVICE_UNAVAILABLE,
        "{}",
        res.text()
    );
    assert_eq!(res.json()["code"], "PF-5005");
}

#[tokio::test]
async fn webhook_missing_signature_is_400() {
    let (app, _clock) = webhook_app(Utc::now()).await;
    let event_id = format!("evt_missing_sig_{}", Uuid::now_v7());
    let payload = ignored_event_payload(&event_id, Utc::now().timestamp());
    let res = post_webhook(&app, &payload, None, None).await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-2013");
    assert!(ledger_row(&app, &event_id).await.is_none(), "no row stored");
}

#[tokio::test]
async fn webhook_tamper_rejected() {
    let (app, _clock) = webhook_app(Utc::now()).await;
    let event_id = format!("evt_tamper_{}", Uuid::now_v7());
    let payload = ignored_event_payload(&event_id, Utc::now().timestamp());
    let header = stripe_signature_header(&payload, WEBHOOK_SECRET);
    // Same header, different body: the signature no longer
    // covers it.
    let tampered = ignored_event_payload(
        &format!("evt_tamper_other_{}", Uuid::now_v7()),
        Utc::now().timestamp(),
    );
    let res = post_webhook(&app, &tampered, Some(&header), None).await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-2013");
    assert!(
        ledger_row(&app, &event_id).await.is_none(),
        "the signed-for event id must not be stored"
    );
}

#[tokio::test]
async fn webhook_wrong_secret_rejected() {
    let (app, _clock) = webhook_app(Utc::now()).await;
    let event_id = format!("evt_wrong_secret_{}", Uuid::now_v7());
    let payload = ignored_event_payload(&event_id, Utc::now().timestamp());
    let header = stripe_signature_header(&payload, "whsec_different0000000000000000000000");
    let res = post_webhook(&app, &payload, Some(&header), None).await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-2013");
    assert!(ledger_row(&app, &event_id).await.is_none(), "no row stored");
}

#[tokio::test]
async fn webhook_valid_ignored_event_stored() {
    let frozen = Utc::now();
    let (app, _clock) = webhook_app(frozen).await;
    let event_id = format!("evt_ignored_{}", Uuid::now_v7());
    let payload = ignored_event_payload(&event_id, Utc::now().timestamp());
    let header = stripe_signature_header(&payload, WEBHOOK_SECRET);
    // No credential at all — and a junk Bearer token changes
    // nothing: the route takes no credential by design.
    let res = post_webhook(&app, &payload, Some(&header), None).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert_eq!(res.json()["received"], true);
    let row = ledger_row(&app, &event_id)
        .await
        .expect("the event is stored");
    assert_eq!(row.event_type, "invoiceitem.created");
    assert_eq!(row.payload["id"], event_id);
    // Postgres TIMESTAMPTZ stores microseconds, so the frozen
    // nanosecond clock truncates on write — compare at micros.
    assert_eq!(
        row.dispatched_at.map(|t| t.timestamp_micros()),
        Some(frozen.timestamp_micros()),
        "an ignored event is still dispatched (mapped to nothing)"
    );
    // The same delivery with a junk Bearer token is still 200
    // — but a fresh event id, so the ledger is not polluted by
    // the replay check: this probes the credential handling,
    // not the idempotency.
    let event_id2 = format!("evt_ignored_key_{}", Uuid::now_v7());
    let payload2 = ignored_event_payload(&event_id2, Utc::now().timestamp());
    let header2 = stripe_signature_header(&payload2, WEBHOOK_SECRET);
    let res = post_webhook(
        &app,
        &payload2,
        Some(&header2),
        Some("pf_live_junkkeyfortestingonly"),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert!(ledger_row(&app, &event_id2).await.is_some());
}

#[tokio::test]
async fn webhook_checkout_completed_dispatched() {
    let frozen = Utc::now();
    let (app, _clock) = webhook_handler_app(frozen).await;
    let event_id = format!("evt_checkout_{}", Uuid::now_v7());
    let payload = checkout_completed_payload(&event_id, Utc::now().timestamp());
    let header = stripe_signature_header(&payload, WEBHOOK_SECRET);
    let res = post_webhook(&app, &payload, Some(&header), None).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert_eq!(res.json()["received"], true);
    let row = ledger_row(&app, &event_id)
        .await
        .expect("the event is stored");
    assert_eq!(row.event_type, "checkout.session.completed");
    assert_eq!(
        row.dispatched_at.map(|t| t.timestamp_micros()),
        Some(frozen.timestamp_micros()),
        "a mapped event reaches dispatch"
    );
}

#[tokio::test]
async fn webhook_replay_is_noop() {
    let frozen = Utc::now();
    let (app, _clock) = webhook_handler_app(frozen).await;
    let event_id = format!("evt_replay_{}", Uuid::now_v7());
    let payload = checkout_completed_payload(&event_id, Utc::now().timestamp());
    let header = stripe_signature_header(&payload, WEBHOOK_SECRET);
    let first = post_webhook(&app, &payload, Some(&header), None).await;
    assert_eq!(first.status, StatusCode::OK, "{}", first.text());
    let row = ledger_row(&app, &event_id)
        .await
        .expect("first delivery stored");
    let first_stamp = row.dispatched_at;
    assert!(first_stamp.is_some(), "first delivery dispatched");
    // The identical redelivery: 200, exactly one row, and the
    // stamp does not move.
    let second = post_webhook(&app, &payload, Some(&header), None).await;
    assert_eq!(second.status, StatusCode::OK, "{}", second.text());
    assert_eq!(second.json()["received"], true);
    let row = ledger_row(&app, &event_id)
        .await
        .expect("still exactly one row");
    assert_eq!(
        row.dispatched_at, first_stamp,
        "a replay must not re-dispatch"
    );
    // The count proves it independently of the row read: one
    // row for this event id, not two.
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM webhook_events WHERE provider = 'stripe' AND event_id = $1",
    )
    .bind(&event_id)
    .fetch_one(&app.pool)
    .await
    .expect("count read");
    assert_eq!(count, 1);
}

#[tokio::test]
async fn webhook_verified_but_unparseable_is_200() {
    let app = always_ok_app(Utc::now()).await;
    // A verified delivery whose envelope has no string `id`:
    // the receiver logs it and answers 200 with no row — the
    // "4xx only on bad signature" rule.
    let payload =
        json!({"id": 1, "type": "checkout.session.completed", "data": {"object": {}}}).to_string();
    let res = post_webhook(&app, &payload, Some("t=1,v1=deadbeef"), None).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert_eq!(res.json()["received"], true);
    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM webhook_events WHERE payload ->> 'id' = '1'")
            .fetch_one(&app.pool)
            .await
            .expect("count read");
    assert_eq!(count, 0, "an unparseable envelope stores no row");
}
