//! P1-074: the Razorpay webhook receiver — `POST
//! /v1/webhooks/razorpay`.
//!
//! The provider is the REAL `RazorpayAdapter` (a fake
//! `rzp_test_` key + a test webhook secret — construction and
//! signature verification need no network): the suite proves
//! the real HMAC-SHA256 path end to end, through the real HTTP
//! layer — not a fake whose `verify` always says yes. The
//! adapter's own unit tests (P1-073) prove the primitive
//! against a real header; this suite proves the receiver
//! around it. The Razorpay HTTP legs are still untested live
//! (no test-mode key on this VM — ASSUMPTION-035): no test
//! here calls Razorpay.
//!
//! What this suite proves:
//!
//! - `razorpay_webhook_unconfigured_returns_503` — flag on,
//!   no provider in the state, is 503 `PF-5005`.
//! - `razorpay_webhook_wrong_provider_returns_503` — flag on
//!   with Stripe as the active provider is 503, not a
//!   misleading 400: there is no Razorpay secret to verify
//!   against.
//! - `razorpay_webhook_missing_signature_is_400` — a delivery
//!   with no `X-Razorpay-Signature` header is 400 `PF-2013`,
//!   and no row is stored.
//! - `razorpay_webhook_tamper_rejected` — the AC: a header
//!   signed for one body, sent with another, is 400
//!   `PF-2013`, and no row is stored.
//! - `razorpay_webhook_wrong_secret_rejected` — a header built
//!   with a different secret is 400 `PF-2013`.
//! - `razorpay_webhook_valid_ignored_event_stored` — a verified
//!   event the app ignores (`subscription.charged`) is 200
//!   with a receipt, one ledger row, and a `dispatched_at`
//!   stamp; no credential is needed.
//! - `razorpay_webhook_authenticated_creates_subscription` — a
//!   verified `subscription.authenticated` is 200, stored,
//!   and dispatched: the subscription row is created at the
//!   provider-reported status with the plan resolved from the
//!   Razorpay plan id (the P1-074 handler change).
//! - `razorpay_webhook_replay_is_noop` — the AC: the same body
//!   posted twice is 200 both times, exactly one ledger row,
//!   the `dispatched_at` stamp does not move, and still
//!   exactly one subscription row.
//! - `razorpay_webhook_status_change_moves_row` — a verified
//!   `subscription.cancelled` moves a seeded Razorpay row to
//!   `canceled` through the state machine.
//! - `razorpay_webhook_verified_but_unparseable_is_200` — a
//!   verified delivery whose envelope cannot be parsed is 200
//!   with no row (the "4xx only on bad signature" rule).
//!   Unlike Stripe, the Razorpay adapter's verify does not
//!   parse the body (HMAC over the raw bytes), so this case
//!   runs against the real adapter.

mod common;

use std::sync::Arc;

use async_trait::async_trait;
use axum::http::{Method, Request, StatusCode};
use chrono::{DateTime, Utc};
use common::{TestApp, TestFixtures};
use detag_billing::{
    BillingError, BillingEvent, BillingProvider, CheckoutRequest, CheckoutSession, Customer,
    DomainEvent, DynBillingProvider, Invoice, PortalRequest, PortalSession, ProviderName,
    RazorpayAdapter, StripeAdapter, SubscriptionState, SubscriptionStatus, PROVIDER_RAZORPAY,
};
use detag_core::{
    checksum::sha256_hex,
    clock::{Clock, ManualClock},
    config::Settings,
    repos::{audit_logs, subscriptions, webhook_events},
};
use hmac::{Hmac, KeyInit, Mac};
use serde_json::json;
use sha2::Sha256;
use uuid::Uuid;

/// The fake Razorpay test key the adapter is built with. It
/// never leaves this file and never touches the network — the
/// adapter's verify + map are pure.
const TEST_KEY_ID: &str = "rzp_test_00000000000000";
/// The fake Razorpay key secret.
const TEST_KEY_SECRET: &str = "test_secret_00000000000000000000";
/// The test webhook secret. A test value, not a real secret —
/// the same constant the billing crate's own tests use.
const WEBHOOK_SECRET: &str = "whsec_test00000000000000000000000000";
/// The Razorpay plan id the `pro` plan is linked to in the
/// dispatch tests (set via SQL, the P1-073 pattern).
const PLAN_RAZORPAY_PRO: &str = "plan_00000000000001";
/// The fake Stripe key for the wrong-provider test.
const STRIPE_TEST_KEY: &str = "sk_test_51FakeForTestsOnly00000000000000";

/// The request id: a valid UUID, echoed by the middleware.
const REQUEST_ID: &str = "123e4567-e89b-42d3-a456-426614174110";

/// A per-request XFF address: each request gets its own IP
/// bucket, so the tests never trip each other's rate limits
/// (the P1-003 lesson; the full-UUID form, FLAKY-006).
fn unique_xff() -> String {
    format!("203.0.113.{}", Uuid::now_v7())
}

/// Build a real Razorpay signature header for `payload`, the
/// way Razorpay does: `hex(hmac_sha256(webhook_secret,
/// body))`.
fn razorpay_signature_header(payload: &[u8], secret: &str) -> String {
    let mut mac =
        Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("hmac accepts any key length");
    mac.update(payload);
    hex::encode(mac.finalize().into_bytes())
}

/// A billing provider for these tests (the P1-065
/// `DelegatingProvider` shape, for Razorpay): it delegates
/// signature verification, webhook mapping, and every call
/// except `fetch_subscription` to a REAL `RazorpayAdapter`
/// (so the HMAC path and the mapping table stay real) and
/// answers `fetch_subscription` from a canned table, so the
/// handler is provable through the real HTTP layer without a
/// Razorpay key (ASSUMPTION-035).
pub struct DelegatingRazorpayProvider {
    inner: RazorpayAdapter,
    subscriptions:
        std::sync::Mutex<std::collections::HashMap<String, detag_billing::SubscriptionState>>,
}

impl DelegatingRazorpayProvider {
    /// Wrap a real adapter (built with the test key + secret).
    pub fn new(inner: RazorpayAdapter) -> Self {
        Self {
            inner,
            subscriptions: std::sync::Mutex::new(std::collections::HashMap::new()),
        }
    }

    /// Answer `fetch_subscription` for this subscription id
    /// with the canned state. Takes `&self` so the test can
    /// add canned answers after the app (and its shared
    /// provider) is spawned.
    pub fn with_subscription(&self, state: detag_billing::SubscriptionState) -> &Self {
        self.subscriptions
            .lock()
            .expect("delegating provider lock")
            .insert(state.provider_subscription_id.clone(), state);
        self
    }
}

#[async_trait]
impl BillingProvider for DelegatingRazorpayProvider {
    fn provider_name(&self) -> ProviderName {
        self.inner.provider_name()
    }

    async fn create_checkout(&self, req: CheckoutRequest) -> Result<CheckoutSession, BillingError> {
        self.inner.create_checkout(req).await
    }

    async fn create_portal(&self, req: PortalRequest) -> Result<PortalSession, BillingError> {
        self.inner.create_portal(req).await
    }

    async fn fetch_subscription(
        &self,
        provider_subscription_id: &str,
    ) -> Result<SubscriptionState, BillingError> {
        self.subscriptions
            .lock()
            .expect("delegating provider lock")
            .get(provider_subscription_id)
            .cloned()
            .ok_or_else(|| BillingError::UnknownSubscription(provider_subscription_id.to_owned()))
    }

    async fn list_invoices(
        &self,
        customer_id: &str,
        limit: u32,
    ) -> Result<Vec<Invoice>, BillingError> {
        self.inner.list_invoices(customer_id, limit).await
    }

    async fn create_customer(&self, user_id: Uuid, email: &str) -> Result<Customer, BillingError> {
        self.inner.create_customer(user_id, email).await
    }

    async fn fetch_customer(&self, provider_customer_id: &str) -> Result<Customer, BillingError> {
        self.inner.fetch_customer(provider_customer_id).await
    }

    fn verify_webhook_signature(
        &self,
        payload: &[u8],
        signature: &str,
    ) -> Result<(), BillingError> {
        self.inner.verify_webhook_signature(payload, signature)
    }

    fn map_webhook(&self, event: BillingEvent) -> Result<Option<DomainEvent>, BillingError> {
        self.inner.map_webhook(event)
    }
}

impl std::fmt::Debug for DelegatingRazorpayProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DelegatingRazorpayProvider")
            .finish_non_exhaustive()
    }
}

/// The app on a frozen clock with the REAL Razorpay adapter
/// and the flag on. Returns (app, clock).
async fn razorpay_app(frozen_at: DateTime<Utc>) -> (TestApp, Arc<ManualClock>) {
    let fixtures = TestFixtures::from_env().await;
    let mut settings = fixtures.settings();
    settings.feature_razorpay = true;
    let clock = Arc::new(ManualClock::new(frozen_at));
    let adapter = RazorpayAdapter::new(TEST_KEY_ID, TEST_KEY_SECRET, WEBHOOK_SECRET, false)
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

/// The app on a frozen clock with the DELEGATING provider:
/// the real Razorpay adapter for signature verification +
/// mapping, canned `fetch_subscription` (the handler fetches
/// the subscription a checkout completed, and the real
/// adapter would call Razorpay live). Returns (app, clock,
/// provider) — the caller adds canned answers to the
/// provider after the spawn.
async fn razorpay_handler_app(
    frozen_at: DateTime<Utc>,
) -> (TestApp, Arc<ManualClock>, Arc<DelegatingRazorpayProvider>) {
    let fixtures = TestFixtures::from_env().await;
    let mut settings = fixtures.settings();
    settings.feature_razorpay = true;
    let clock = Arc::new(ManualClock::new(frozen_at));
    let adapter = RazorpayAdapter::new(TEST_KEY_ID, TEST_KEY_SECRET, WEBHOOK_SECRET, false)
        .expect("test key and secret build the adapter");
    let provider = Arc::new(DelegatingRazorpayProvider::new(adapter));
    let app = TestApp::spawn_with_fixtures_settings_clock_and_provider(
        fixtures,
        settings,
        Arc::clone(&clock) as Arc<dyn Clock>,
        Arc::clone(&provider) as DynBillingProvider,
    )
    .await;
    (app, clock, provider)
}

/// App with the flag on and no provider (the default spawn
/// with the flag flipped) — Razorpay is not configured, so
/// the webhook is 503.
async fn unconfigured_app() -> TestApp {
    let settings = Settings {
        feature_razorpay: true,
        ..Settings::default()
    };
    TestApp::spawn_with(settings).await
}

/// App with the flag on and STRIPE as the active provider —
/// the Razorpay receiver must answer 503, not a misleading
/// 400.
async fn stripe_provider_app(frozen_at: DateTime<Utc>) -> TestApp {
    let fixtures = TestFixtures::from_env().await;
    let mut settings = fixtures.settings();
    settings.feature_razorpay = true;
    let clock = Arc::new(ManualClock::new(frozen_at));
    let adapter = StripeAdapter::new(STRIPE_TEST_KEY, WEBHOOK_SECRET, false)
        .expect("test key and secret build the adapter");
    let provider: DynBillingProvider = Arc::new(adapter) as DynBillingProvider;
    TestApp::spawn_with_fixtures_settings_clock_and_provider(
        fixtures,
        settings,
        Arc::clone(&clock) as Arc<dyn Clock>,
        provider,
    )
    .await
}

/// POST one delivery to the Razorpay webhook. `signature` is
/// the raw `X-Razorpay-Signature` header value (`None` =
/// header absent). `credential` is an optional Bearer token —
/// the route takes no credential by design, so it must change
/// nothing.
async fn post_webhook(
    app: &TestApp,
    payload: &str,
    signature: Option<&str>,
    credential: Option<&str>,
) -> common::TestResponse {
    let mut req = Request::builder()
        .method(Method::POST)
        .uri("/v1/webhooks/razorpay")
        .header("x-request-id", REQUEST_ID)
        .header("x-forwarded-for", unique_xff())
        .header("content-type", "application/json");
    if let Some(signature) = signature {
        req = req.header("x-razorpay-signature", signature);
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

/// A Razorpay subscription-event envelope. The entity is the
/// subscription object the adapter maps; `created_at` is the
/// delivery time (it is what makes two deliveries of the same
/// subscription distinct bodies).
fn subscription_event_payload(event_type: &str, entity: serde_json::Value, now: i64) -> String {
    json!({
        "entity": "event",
        "event": event_type,
        "contains": ["subscription"],
        "payload": { "subscription": { "entity": entity } },
        "created_at": now,
    })
    .to_string()
}

/// A verified-but-ignored subscription entity
/// (`subscription.charged` maps to nothing).
fn charged_entity(subscription_id: &str) -> serde_json::Value {
    json!({
        "id": subscription_id,
        "entity": "subscription",
        "plan_id": PLAN_RAZORPAY_PRO,
        "status": "active",
        "customer_id": "cust_00000000000001",
    })
}

/// A `subscription.authenticated` entity: the first payment
/// made, carrying our user id in `notes.detag_user_id` (the
/// P1-073 checkout writes it there).
fn authenticated_entity(subscription_id: &str, user_id: Uuid) -> serde_json::Value {
    json!({
        "id": subscription_id,
        "entity": "subscription",
        "plan_id": PLAN_RAZORPAY_PRO,
        "status": "authenticated",
        "customer_id": "cust_00000000000001",
        "notes": { "detag_user_id": user_id.to_string() },
    })
}

/// The ledger row for (`razorpay`, the body hash), if one
/// exists. Razorpay sends no event id — the key is the
/// SHA-256 hex of the raw body.
async fn ledger_row(app: &TestApp, payload: &str) -> Option<webhook_events::WebhookEventRow> {
    webhook_events::get(
        &app.pool,
        PROVIDER_RAZORPAY,
        &sha256_hex(payload.as_bytes()),
    )
    .await
    .expect("ledger read")
}

/// The subscription row for a provider subscription id, if
/// one exists.
async fn subscription_row(
    app: &TestApp,
    provider_subscription_id: &str,
) -> Option<subscriptions::Subscription> {
    subscriptions::by_provider_id(&app.pool, PROVIDER_RAZORPAY, provider_subscription_id)
        .await
        .expect("subscription read")
}

/// Link the `pro` plan to the Razorpay plan id immediately
/// before the probe that reads it (the plans table is shared
/// across test binaries — the webhook_handlers pattern).
async fn set_pro_razorpay_plan(app: &TestApp) {
    sqlx::query("UPDATE plans SET razorpay_plan_id = $1 WHERE code = 'pro'")
        .bind(PLAN_RAZORPAY_PRO)
        .execute(&app.pool)
        .await
        .expect("set pro razorpay plan");
}

/// Seed a Razorpay subscription row directly (there is no
/// billing flow to create one through the API except the
/// webhook itself). The provider subscription id is unique
/// per call.
async fn seed_subscription(app: &TestApp, user_id: Uuid, status: &str) -> String {
    let plan = detag_core::repos::plans::by_code(&app.pool, "pro")
        .await
        .expect("load plan")
        .expect("plan is seeded");
    let provider_subscription_id = format!("sub_p1074_{}", Uuid::now_v7());
    sqlx::query(
        "INSERT INTO subscriptions (user_id, plan_id, provider, provider_customer_id, provider_subscription_id, status) \
         VALUES ($1, $2, 'razorpay', 'cust_00000000000001', $3, $4)",
    )
    .bind(user_id)
    .bind(plan.id)
    .bind(&provider_subscription_id)
    .bind(status)
    .execute(&app.pool)
    .await
    .expect("seed subscription");
    provider_subscription_id
}

#[tokio::test]
async fn razorpay_webhook_unconfigured_returns_503() {
    let app = unconfigured_app().await;
    let payload = subscription_event_payload(
        "subscription.charged",
        charged_entity("sub_unconfigured_1"),
        Utc::now().timestamp(),
    );
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
async fn razorpay_webhook_wrong_provider_returns_503() {
    let app = stripe_provider_app(Utc::now()).await;
    let payload = subscription_event_payload(
        "subscription.charged",
        charged_entity("sub_wrong_provider_1"),
        Utc::now().timestamp(),
    );
    // Even a correctly-signed Razorpay delivery is 503 here:
    // Stripe is the active provider, so there is no Razorpay
    // secret to verify against — and the header below is
    // signed with the Razorpay test secret, which Stripe
    // would reject anyway.
    let header = razorpay_signature_header(payload.as_bytes(), WEBHOOK_SECRET);
    let res = post_webhook(&app, &payload, Some(&header), None).await;
    assert_eq!(
        res.status,
        StatusCode::SERVICE_UNAVAILABLE,
        "{}",
        res.text()
    );
    assert_eq!(res.json()["code"], "PF-5005");
}

#[tokio::test]
async fn razorpay_webhook_missing_signature_is_400() {
    let (app, _clock) = razorpay_app(Utc::now()).await;
    let subscription_id = format!("sub_missing_sig_{}", Uuid::now_v7());
    let payload = subscription_event_payload(
        "subscription.charged",
        charged_entity(&subscription_id),
        Utc::now().timestamp(),
    );
    let res = post_webhook(&app, &payload, None, None).await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-2013");
    assert!(ledger_row(&app, &payload).await.is_none(), "no row stored");
}

#[tokio::test]
async fn razorpay_webhook_tamper_rejected() {
    let (app, _clock) = razorpay_app(Utc::now()).await;
    let subscription_id = format!("sub_tamper_{}", Uuid::now_v7());
    let payload = subscription_event_payload(
        "subscription.charged",
        charged_entity(&subscription_id),
        Utc::now().timestamp(),
    );
    let header = razorpay_signature_header(payload.as_bytes(), WEBHOOK_SECRET);
    // Same header, different body: the signature no longer
    // covers it.
    let tampered = subscription_event_payload(
        "subscription.charged",
        charged_entity(&format!("sub_tamper_other_{}", Uuid::now_v7())),
        Utc::now().timestamp(),
    );
    let res = post_webhook(&app, &tampered, Some(&header), None).await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-2013");
    assert!(
        ledger_row(&app, &tampered).await.is_none(),
        "the tampered body must not be stored"
    );
    assert!(
        ledger_row(&app, &payload).await.is_none(),
        "the signed-for body must not be stored either"
    );
}

#[tokio::test]
async fn razorpay_webhook_wrong_secret_rejected() {
    let (app, _clock) = razorpay_app(Utc::now()).await;
    let subscription_id = format!("sub_wrong_secret_{}", Uuid::now_v7());
    let payload = subscription_event_payload(
        "subscription.charged",
        charged_entity(&subscription_id),
        Utc::now().timestamp(),
    );
    let header =
        razorpay_signature_header(payload.as_bytes(), "whsec_different0000000000000000000000");
    let res = post_webhook(&app, &payload, Some(&header), None).await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-2013");
    assert!(ledger_row(&app, &payload).await.is_none(), "no row stored");
}

#[tokio::test]
async fn razorpay_webhook_valid_ignored_event_stored() {
    let frozen = Utc::now();
    let (app, _clock) = razorpay_app(frozen).await;
    let subscription_id = format!("sub_ignored_{}", Uuid::now_v7());
    let payload = subscription_event_payload(
        "subscription.charged",
        charged_entity(&subscription_id),
        Utc::now().timestamp(),
    );
    let header = razorpay_signature_header(payload.as_bytes(), WEBHOOK_SECRET);
    // No credential at all — and a junk Bearer token changes
    // nothing: the route takes no credential by design.
    let res = post_webhook(&app, &payload, Some(&header), None).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert_eq!(res.json()["received"], true);
    let row = ledger_row(&app, &payload)
        .await
        .expect("the event is stored");
    assert_eq!(row.event_type, "subscription.charged");
    assert_eq!(row.payload["event"], "subscription.charged");
    // Postgres TIMESTAMPTZ stores microseconds, so the frozen
    // nanosecond clock truncates on write — compare at micros.
    assert_eq!(
        row.dispatched_at.map(|t| t.timestamp_micros()),
        Some(frozen.timestamp_micros()),
        "an ignored event is still dispatched (mapped to nothing)"
    );
    // The same delivery with a junk Bearer token is still 200
    // — but a fresh body, so the ledger is not polluted by the
    // replay check: this probes the credential handling, not
    // the idempotency.
    let subscription_id2 = format!("sub_ignored_key_{}", Uuid::now_v7());
    let payload2 = subscription_event_payload(
        "subscription.charged",
        charged_entity(&subscription_id2),
        Utc::now().timestamp(),
    );
    let header2 = razorpay_signature_header(payload2.as_bytes(), WEBHOOK_SECRET);
    let res = post_webhook(
        &app,
        &payload2,
        Some(&header2),
        Some("pf_live_junkkeyfortestingonly"),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert!(ledger_row(&app, &payload2).await.is_some());
}

#[tokio::test]
async fn razorpay_webhook_authenticated_creates_subscription() {
    let frozen = Utc::now();
    let (app, _clock, provider) = razorpay_handler_app(frozen).await;
    let user = app.seed_user_unique().await;
    set_pro_razorpay_plan(&app).await;
    let subscription_id = format!("sub_auth_{}", Uuid::now_v7());
    let period_end = frozen + chrono::Duration::days(30);
    provider.with_subscription(SubscriptionState {
        provider_subscription_id: subscription_id.clone(),
        provider_customer_id: Some("cust_00000000000001".to_owned()),
        status: SubscriptionStatus::Active,
        current_period_end: Some(period_end),
        cancel_at_period_end: false,
        price_ids: vec![PLAN_RAZORPAY_PRO.to_owned()],
    });
    let payload = subscription_event_payload(
        "subscription.authenticated",
        authenticated_entity(&subscription_id, user.id),
        Utc::now().timestamp(),
    );
    let header = razorpay_signature_header(payload.as_bytes(), WEBHOOK_SECRET);
    let res = post_webhook(&app, &payload, Some(&header), None).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert_eq!(res.json()["received"], true);
    let row = ledger_row(&app, &payload)
        .await
        .expect("the event is stored");
    assert_eq!(row.event_type, "subscription.authenticated");
    assert_eq!(
        row.dispatched_at.map(|t| t.timestamp_micros()),
        Some(frozen.timestamp_micros()),
        "a mapped event reaches dispatch"
    );
    let created = subscription_row(&app, &subscription_id)
        .await
        .expect("the subscription row is created");
    assert_eq!(created.status, "active");
    assert_eq!(created.provider, "razorpay");
    assert_eq!(created.user_id, user.id);
    // The creation is audited (the P1-065 rule).
    let audits = audit_logs::list_for_target(&app.pool, "subscription", &created.id.to_string())
        .await
        .expect("load audit");
    assert!(
        audits
            .iter()
            .any(|row| row.action == "billing.subscription.created"),
        "creation writes the audit row: {:?}",
        audits.iter().map(|row| &row.action).collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn razorpay_webhook_replay_is_noop() {
    let frozen = Utc::now();
    let (app, _clock, provider) = razorpay_handler_app(frozen).await;
    let user = app.seed_user_unique().await;
    set_pro_razorpay_plan(&app).await;
    let subscription_id = format!("sub_replay_{}", Uuid::now_v7());
    let period_end = frozen + chrono::Duration::days(30);
    provider.with_subscription(SubscriptionState {
        provider_subscription_id: subscription_id.clone(),
        provider_customer_id: Some("cust_00000000000001".to_owned()),
        status: SubscriptionStatus::Active,
        current_period_end: Some(period_end),
        cancel_at_period_end: false,
        price_ids: vec![PLAN_RAZORPAY_PRO.to_owned()],
    });
    let payload = subscription_event_payload(
        "subscription.authenticated",
        authenticated_entity(&subscription_id, user.id),
        Utc::now().timestamp(),
    );
    let header = razorpay_signature_header(payload.as_bytes(), WEBHOOK_SECRET);
    let first = post_webhook(&app, &payload, Some(&header), None).await;
    assert_eq!(first.status, StatusCode::OK, "{}", first.text());
    let row = ledger_row(&app, &payload)
        .await
        .expect("first delivery stored");
    let first_stamp = row.dispatched_at;
    assert!(first_stamp.is_some(), "first delivery dispatched");
    assert!(
        subscription_row(&app, &subscription_id).await.is_some(),
        "first delivery created the row"
    );
    // The identical redelivery: 200, exactly one ledger row,
    // the stamp does not move, and still exactly one
    // subscription row.
    let second = post_webhook(&app, &payload, Some(&header), None).await;
    assert_eq!(second.status, StatusCode::OK, "{}", second.text());
    assert_eq!(second.json()["received"], true);
    let row = ledger_row(&app, &payload)
        .await
        .expect("still exactly one row");
    assert_eq!(
        row.dispatched_at, first_stamp,
        "a replay must not re-dispatch"
    );
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM webhook_events WHERE provider = 'razorpay' AND event_id = $1",
    )
    .bind(sha256_hex(payload.as_bytes()))
    .fetch_one(&app.pool)
    .await
    .expect("count read");
    assert_eq!(count, 1);
    let sub_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM subscriptions WHERE provider_subscription_id = $1",
    )
    .bind(&subscription_id)
    .fetch_one(&app.pool)
    .await
    .expect("count read");
    assert_eq!(sub_count, 1, "a replay must not create a second row");
}

#[tokio::test]
async fn razorpay_webhook_status_change_moves_row() {
    let frozen = Utc::now();
    let (app, _clock) = razorpay_app(frozen).await;
    let user = app.seed_user_unique().await;
    let subscription_id = seed_subscription(&app, user.id, "active").await;
    let payload = subscription_event_payload(
        "subscription.cancelled",
        json!({
            "id": subscription_id,
            "entity": "subscription",
            "plan_id": PLAN_RAZORPAY_PRO,
            "status": "cancelled",
            "customer_id": "cust_00000000000001",
        }),
        Utc::now().timestamp(),
    );
    let header = razorpay_signature_header(payload.as_bytes(), WEBHOOK_SECRET);
    let res = post_webhook(&app, &payload, Some(&header), None).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert_eq!(res.json()["received"], true);
    let row = subscription_row(&app, &subscription_id)
        .await
        .expect("the seeded row is still there");
    assert_eq!(row.status, "canceled", "the terminal event cancels the row");
}

#[tokio::test]
async fn razorpay_webhook_verified_but_unparseable_is_200() {
    let (app, _clock) = razorpay_app(Utc::now()).await;
    // A verified delivery whose envelope has no `event`: the
    // receiver logs it and answers 200 with no row — the
    // "4xx only on bad signature" rule. The Razorpay adapter's
    // verify is HMAC over the raw bytes (it never parses), so
    // this case runs against the real adapter.
    let payload = json!({"payload": {"subscription": {"entity": {}}}}).to_string();
    let header = razorpay_signature_header(payload.as_bytes(), WEBHOOK_SECRET);
    let res = post_webhook(&app, &payload, Some(&header), None).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert_eq!(res.json()["received"], true);
    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM webhook_events WHERE provider = 'razorpay' AND payload ->> 'event' IS NULL")
            .fetch_one(&app.pool)
            .await
            .expect("count read");
    assert_eq!(count, 0, "an unparseable envelope stores no row");
}
