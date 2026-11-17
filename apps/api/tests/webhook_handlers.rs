//! P1-065: the webhook event handlers — `POST /v1/webhooks/stripe`
//! applies the mapped event to the `subscriptions` table.
//!
//! The provider is the DELEGATING double ([`DelegatingProvider`]):
//! the real `StripeAdapter` for signature verification + mapping
//! (so the HMAC path and the mapping table stay real), canned
//! `fetch_subscription` answers (the adapter's HTTP legs are
//! still untested live — no test-mode key on this VM,
//! ASSUMPTION-023). The suite proves the handler through the real
//! HTTP layer, end to end from a signed delivery to the row.
//!
//! What this suite proves (the AC is the mapping table, tested
//! through the handler — the adapter's own unit tests prove the
//! pure mapping in `detag-billing`):
//!
//! - `checkout_completed_creates_subscription` — a verified
//!   checkout with our user (`client_reference_id`) creates the
//!   row at the provider-reported status, with the plan resolved
//!   from the fetched price, plus the `billing.subscription
//!   .created` audit row; the user then resolves to Pro.
//! - `checkout_completed_without_user_is_200` — a session with no
//!   `client_reference_id` is 200 and creates no row.
//! - `checkout_completed_unknown_subscription_is_200` — the
//!   provider no longer knows the subscription: 200, no row.
//! - `checkout_completed_unknown_price_is_200` — the fetched
//!   prices match no plan: 200, no row.
//! - `subscription_updated_moves_status` — `trialing → active`
//!   welcomes, grants Pro, audits.
//! - `subscription_deleted_cancels` — the row goes `canceled`
//!   with the goodbye mail.
//! - `invoice_paid_is_a_noop` — a paid invoice moves nothing.
//! - `invoice_payment_failed_duns` — the linked subscription
//!   goes `past_due` with the dunning mail.
//! - `invoice_payment_failed_one_off_is_200` — a one-off failed
//!   invoice is 200 and touches nothing.
//! - `event_for_unknown_subscription_is_200` — an update for a
//!   row that does not exist is 200 and invents no row.
//! - `payment_failed_for_canceled_row_is_200` — the machine
//!   rejects `canceled → past_due`: 200, row untouched.
//! - `checkout_replay_creates_one_row` — the same checkout
//!   delivered twice creates exactly one row.
//! - `replay_after_failed_dispatch_applies` — a first delivery
//!   whose handler 500s leaves `dispatched_at` NULL, and its
//!   redelivery applies the event (P1-065's re-dispatch rule).

mod common;

use std::sync::Arc;

use axum::http::{Method, Request, StatusCode};
use chrono::{DateTime, Utc};
use common::{DelegatingProvider, TestApp, TestFixtures};
use detag_api::services::entitlements::effective_plan;
use detag_billing::{
    DynBillingProvider, StripeAdapter, SubscriptionState, SubscriptionStatus, PROVIDER_STRIPE,
};
use detag_core::{
    clock::{Clock, ManualClock},
    repos::{audit_logs, email_outbox, subscriptions, webhook_events},
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
const WEBHOOK_SECRET: &str = "whsec_test00000000000000000000000000000000000000000000000000000000";

/// The request id: a valid UUID, echoed by the middleware.
const REQUEST_ID: &str = "123e4567-e89b-42d3-a456-426614174109";

/// The Pro price the canned fetches report, set on the `pro`
/// plan row immediately before each probe (the checkout tests'
/// pattern — the plans table is shared across test binaries).
const PRICE_PRO: &str = "price_p1065_pro";

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

/// The app on a frozen clock with the delegating provider.
/// Returns (app, clock, provider) — the provider is shared with
/// the app, so the test adds canned fetch answers to it.
async fn handler_app(
    frozen_at: DateTime<Utc>,
) -> (TestApp, Arc<ManualClock>, Arc<DelegatingProvider>) {
    let fixtures = TestFixtures::from_env().await;
    let settings = fixtures.settings();
    let clock = Arc::new(ManualClock::new(frozen_at));
    let adapter = StripeAdapter::new(TEST_KEY, WEBHOOK_SECRET, false)
        .expect("test key and secret build the adapter");
    let provider = Arc::new(DelegatingProvider::new(adapter));
    let app = TestApp::spawn_with_fixtures_settings_clock_and_provider(
        fixtures,
        settings,
        Arc::clone(&clock) as Arc<dyn Clock>,
        Arc::clone(&provider) as DynBillingProvider,
    )
    .await;
    (app, clock, provider)
}

/// POST one delivery to the webhook. `signature` is the raw
/// `Stripe-Signature` header value.
async fn post_webhook(app: &TestApp, payload: &str, signature: &str) -> common::TestResponse {
    let req = Request::builder()
        .method(Method::POST)
        .uri("/v1/webhooks/stripe")
        .header("x-request-id", REQUEST_ID)
        .header("x-forwarded-for", unique_xff())
        .header("content-type", "application/json")
        .header("stripe-signature", signature);
    app.request(
        req.body(axum::body::Body::from(payload.as_bytes().to_vec()))
            .unwrap(),
    )
    .await
}

/// A verified checkout-completed envelope. `user_id` is the
/// session's `client_reference_id` (`None` = the session names
/// no user). The session object carries the fields
/// `construct_event` requires (the webhook suite's shape).
fn checkout_completed_payload(
    event_id: &str,
    now: i64,
    user_id: Option<Uuid>,
    subscription_id: &str,
) -> String {
    let expires = now + 1800;
    let client_reference_id = user_id.map(|id| format!(",\"client_reference_id\":\"{id}\""));
    format!(
        r#"{{"id":"{event_id}","object":"event","api_version":"2024-06-20","created":{now},"data":{{"object":{{"id":"cs_test_123","object":"checkout.session","automatic_tax":{{"enabled":false}},"created":{now},"custom_fields":[],"custom_text":{{}},"customer":"cus_test_123","expires_at":{expires},"livemode":false,"mode":"subscription","payment_method_types":["card"],"payment_status":"paid","shipping_options":[],"subscription":"{subscription_id}"{client_reference_id}}}}},"livemode":false,"pending_webhooks":1,"request":{{"id":"req_123","idempotency_key":null}},"type":"checkout.session.completed"}}"#,
        client_reference_id = client_reference_id.unwrap_or_default(),
    )
}

/// A verified subscription event envelope (`created` /
/// `updated` / `deleted`). `construct_event` parses only the
/// envelope; the object is read by `map_webhook`.
fn subscription_event_payload(
    event_id: &str,
    event_type: &str,
    now: i64,
    subscription_id: &str,
    status: Option<&str>,
    current_period_end: Option<i64>,
    cancel_at_period_end: Option<bool>,
) -> String {
    // The object must be a COMPLETE typed `Subscription`: the
    // provider's `construct_event` deserialises `data.object` into
    // the concrete resource for the event type, and the generated
    // `Subscription` struct has required (non-Option) fields — a
    // partial object fails with "missing field `automatic_tax`" and
    // the delivery is 400 `PF-2013`, never reaching the handler.
    // The deleted case carries no status/period in the call, so it
    // defaults to the canceled shape the handler expects.
    let status = status.unwrap_or("canceled");
    let period_end = current_period_end.unwrap_or(now);
    let cancel = cancel_at_period_end.unwrap_or(false);
    let object = format!(
        r#"{{"id":"{subscription_id}","object":"subscription","automatic_tax":{{"enabled":false}},"billing_cycle_anchor":{now},"cancel_at_period_end":{cancel},"created":{now},"currency":"usd","current_period_end":{period_end},"current_period_start":{now},"customer":"cus_test_123","items":{{"data":[],"has_more":false,"url":"/v1/subscription_items"}},"livemode":false,"metadata":{{}},"start_date":{now},"status":"{status}"}}"#
    );
    format!(
        r#"{{"id":"{event_id}","object":"event","api_version":"2024-06-20","created":{now},"data":{{"object":{object}}},"livemode":false,"pending_webhooks":1,"request":{{"id":"req_123","idempotency_key":null}},"type":"{event_type}"}}"#
    )
}

/// A verified invoice event envelope (`invoice.paid` /
/// `invoice.payment_failed`). `subscription_id` is `None` for a
/// one-off invoice.
fn invoice_event_payload(
    event_id: &str,
    event_type: &str,
    now: i64,
    invoice_id: &str,
    subscription_id: Option<&str>,
) -> String {
    let subscription = subscription_id
        .map(|id| format!(r#","subscription":"{id}""#))
        .unwrap_or_default();
    format!(
        r#"{{"id":"{event_id}","object":"event","api_version":"2024-06-20","created":{now},"data":{{"object":{{"id":"{invoice_id}","object":"invoice"{subscription}}}}},"livemode":false,"pending_webhooks":1,"request":{{"id":"req_123","idempotency_key":null}},"type":"{event_type}"}}"#
    )
}

/// The ledger row for (`stripe`, `event_id`), if one exists.
async fn ledger_row(app: &TestApp, event_id: &str) -> Option<webhook_events::WebhookEventRow> {
    webhook_events::get(&app.pool, PROVIDER_STRIPE, event_id)
        .await
        .expect("ledger read")
}

/// The subscription row for a provider subscription id, if one
/// exists.
async fn subscription_row(
    app: &TestApp,
    provider_subscription_id: &str,
) -> Option<subscriptions::Subscription> {
    subscriptions::by_provider_id(&app.pool, PROVIDER_STRIPE, provider_subscription_id)
        .await
        .expect("subscription read")
}

/// Set the `pro` plan's Stripe price immediately before the
/// probe that reads it (the plans table is shared across test
/// binaries — the checkout tests' pattern).
async fn set_pro_price(app: &TestApp) {
    sqlx::query("UPDATE plans SET stripe_price_id = $1 WHERE code = 'pro'")
        .bind(PRICE_PRO)
        .execute(&app.pool)
        .await
        .expect("set pro price");
}

/// Seed a subscription row directly (there is no billing flow
/// to create one through the API except the webhook itself).
/// The provider subscription id is unique per call.
async fn seed_subscription(app: &TestApp, user_id: Uuid, plan_code: &str, status: &str) -> String {
    let plan = detag_core::repos::plans::by_code(&app.pool, plan_code)
        .await
        .expect("load plan")
        .expect("plan is seeded");
    let provider_subscription_id = format!("sub_p1065_{}", Uuid::now_v7());
    sqlx::query(
        "INSERT INTO subscriptions (user_id, plan_id, provider, provider_customer_id, provider_subscription_id, status) \
         VALUES ($1, $2, 'stripe', 'cus_test_123', $3, $4)",
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

/// The audit rows for one subscription row.
async fn audit_rows(app: &TestApp, id: Uuid) -> Vec<audit_logs::AuditLog> {
    audit_logs::list_for_target(&app.pool, "subscription", &id.to_string())
        .await
        .expect("load audit")
}

/// The mails for one user only: the harness DB is shared by
/// every test in this binary (the P1-046 precedent), so a
/// global count would count other tests' mails too.
async fn mails_for_user(
    app: &TestApp,
    template: &str,
    user_id: Uuid,
) -> Vec<email_outbox::OutboxMail> {
    email_outbox::unsent_for_template(&app.pool, template)
        .await
        .expect("load outbox")
        .into_iter()
        .filter(|mail| {
            mail.payload.get("user_id").and_then(|v| v.as_str()) == Some(&user_id.to_string())
        })
        .collect()
}

/// A canned provider answer for a checkout: the subscription
/// is `status` at the fetched price, ending at `period_end`.
fn canned_fetch(
    subscription_id: &str,
    status: SubscriptionStatus,
    period_end: DateTime<Utc>,
) -> SubscriptionState {
    SubscriptionState {
        provider_subscription_id: subscription_id.to_owned(),
        provider_customer_id: Some("cus_test_123".to_owned()),
        status,
        current_period_end: Some(period_end),
        cancel_at_period_end: false,
        price_ids: vec![PRICE_PRO.to_owned()],
    }
}

#[tokio::test]
async fn checkout_completed_creates_subscription() {
    let frozen = Utc::now();
    let (app, _clock, provider) = handler_app(frozen).await;
    let user = app.seed_user_unique().await;
    let subscription_id = format!("sub_p1065_{}", Uuid::now_v7());
    let period_end = DateTime::from_timestamp(1794844800, 0).expect("in range");
    provider.with_subscription(canned_fetch(
        &subscription_id,
        SubscriptionStatus::Active,
        period_end,
    ));
    set_pro_price(&app).await;

    let event_id = format!("evt_checkout_create_{}", Uuid::now_v7());
    let payload = checkout_completed_payload(
        &event_id,
        Utc::now().timestamp(),
        Some(user.id),
        &subscription_id,
    );
    let header = stripe_signature_header(&payload, WEBHOOK_SECRET);
    let res = post_webhook(&app, &payload, &header).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert_eq!(res.json()["received"], true);

    let row = subscription_row(&app, &subscription_id)
        .await
        .expect("the checkout created the row");
    assert_eq!(row.user_id, user.id);
    assert_eq!(row.provider, "stripe");
    assert_eq!(row.provider_customer_id.as_deref(), Some("cus_test_123"));
    assert_eq!(row.status, "active");
    assert_eq!(row.current_period_end, Some(period_end));
    assert!(!row.cancel_at_period_end);
    let plan = detag_core::repos::plans::by_code(&app.pool, "pro")
        .await
        .expect("load plan")
        .expect("plan is seeded");
    assert_eq!(row.plan_id, plan.id);

    let audit = audit_rows(&app, row.id).await;
    assert_eq!(audit.len(), 1, "exactly one audit row");
    assert_eq!(audit[0].action, "billing.subscription.created");
    assert_eq!(audit[0].before, None);
    assert_eq!(
        audit[0].after,
        Some(json!({ "status": "active", "plan_code": "pro" }))
    );
    assert_eq!(audit[0].reason.as_deref(), Some(event_id.as_str()));

    // The user now resolves to Pro.
    let resolved = effective_plan(&app.state.plans, &app.pool, user.id)
        .await
        .expect("resolve plan");
    assert_eq!(resolved.plan.code, "pro");

    let ledger = ledger_row(&app, &event_id).await.expect("stored");
    assert_eq!(
        ledger.dispatched_at.map(|t| t.timestamp_micros()),
        Some(frozen.timestamp_micros()),
        "a created checkout is dispatched"
    );
}

#[tokio::test]
async fn checkout_completed_without_user_is_200() {
    let (app, _clock, _provider) = handler_app(Utc::now()).await;
    let subscription_id = format!("sub_p1065_{}", Uuid::now_v7());
    let event_id = format!("evt_checkout_nouser_{}", Uuid::now_v7());
    let payload =
        checkout_completed_payload(&event_id, Utc::now().timestamp(), None, &subscription_id);
    let header = stripe_signature_header(&payload, WEBHOOK_SECRET);
    let res = post_webhook(&app, &payload, &header).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert!(subscription_row(&app, &subscription_id).await.is_none());
    assert!(ledger_row(&app, &event_id).await.is_some());
}

#[tokio::test]
async fn checkout_completed_unknown_subscription_is_200() {
    let (app, _clock, _provider) = handler_app(Utc::now()).await;
    let user = app.seed_user_unique().await;
    let subscription_id = format!("sub_p1065_{}", Uuid::now_v7());
    // No canned fetch: the provider does not know it.
    let event_id = format!("evt_checkout_unknownsub_{}", Uuid::now_v7());
    let payload = checkout_completed_payload(
        &event_id,
        Utc::now().timestamp(),
        Some(user.id),
        &subscription_id,
    );
    let header = stripe_signature_header(&payload, WEBHOOK_SECRET);
    let res = post_webhook(&app, &payload, &header).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert!(subscription_row(&app, &subscription_id).await.is_none());
}

#[tokio::test]
async fn checkout_completed_unknown_price_is_200() {
    let (app, _clock, provider) = handler_app(Utc::now()).await;
    let user = app.seed_user_unique().await;
    let subscription_id = format!("sub_p1065_{}", Uuid::now_v7());
    let period_end = DateTime::from_timestamp(1794844800, 0).expect("in range");
    let mut fetched = canned_fetch(&subscription_id, SubscriptionStatus::Active, period_end);
    fetched.price_ids = vec!["price_no_such_plan".to_owned()];
    provider.with_subscription(fetched);
    set_pro_price(&app).await;

    let event_id = format!("evt_checkout_unknownprice_{}", Uuid::now_v7());
    let payload = checkout_completed_payload(
        &event_id,
        Utc::now().timestamp(),
        Some(user.id),
        &subscription_id,
    );
    let header = stripe_signature_header(&payload, WEBHOOK_SECRET);
    let res = post_webhook(&app, &payload, &header).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert!(subscription_row(&app, &subscription_id).await.is_none());
}

#[tokio::test]
async fn subscription_updated_moves_status() {
    let frozen = Utc::now();
    let (app, _clock, _provider) = handler_app(frozen).await;
    let user = app.seed_user_unique().await;
    let subscription_id = seed_subscription(&app, user.id, "pro", "trialing").await;

    let event_id = format!("evt_sub_updated_{}", Uuid::now_v7());
    let period_end = 1794844800;
    let payload = subscription_event_payload(
        &event_id,
        "customer.subscription.updated",
        Utc::now().timestamp(),
        &subscription_id,
        Some("active"),
        Some(period_end),
        Some(false),
    );
    let header = stripe_signature_header(&payload, WEBHOOK_SECRET);
    let res = post_webhook(&app, &payload, &header).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());

    let row = subscription_row(&app, &subscription_id)
        .await
        .expect("row still exists");
    assert_eq!(row.status, "active");
    assert_eq!(
        row.current_period_end,
        Some(DateTime::from_timestamp(period_end, 0).expect("in range"))
    );
    let audit = audit_rows(&app, row.id).await;
    assert_eq!(audit.len(), 1, "exactly one audit row");
    assert_eq!(audit[0].action, "billing.subscription.status_changed");
    assert_eq!(
        audit[0].after,
        Some(json!({ "status": "active", "plan_code": "pro" }))
    );
    assert_eq!(audit[0].reason.as_deref(), Some(event_id.as_str()));
    let mails = mails_for_user(&app, "subscription-welcome", user.id).await;
    assert_eq!(mails.len(), 1, "exactly one welcome mail");
    let resolved = effective_plan(&app.state.plans, &app.pool, user.id)
        .await
        .expect("resolve plan");
    assert_eq!(resolved.plan.code, "pro");
}

#[tokio::test]
async fn subscription_deleted_cancels() {
    let (app, _clock, _provider) = handler_app(Utc::now()).await;
    let user = app.seed_user_unique().await;
    let subscription_id = seed_subscription(&app, user.id, "pro", "active").await;

    let event_id = format!("evt_sub_deleted_{}", Uuid::now_v7());
    let payload = subscription_event_payload(
        &event_id,
        "customer.subscription.deleted",
        Utc::now().timestamp(),
        &subscription_id,
        None,
        None,
        None,
    );
    let header = stripe_signature_header(&payload, WEBHOOK_SECRET);
    let res = post_webhook(&app, &payload, &header).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());

    let row = subscription_row(&app, &subscription_id)
        .await
        .expect("row still exists");
    assert_eq!(row.status, "canceled");
    let mails = mails_for_user(&app, "subscription-canceled", user.id).await;
    assert_eq!(mails.len(), 1, "exactly one canceled mail");
    let resolved = effective_plan(&app.state.plans, &app.pool, user.id)
        .await
        .expect("resolve plan");
    assert_eq!(resolved.plan.code, "free", "canceled grants nothing");
}

#[tokio::test]
async fn invoice_paid_is_a_noop() {
    let (app, _clock, _provider) = handler_app(Utc::now()).await;
    let user = app.seed_user_unique().await;
    let subscription_id = seed_subscription(&app, user.id, "pro", "past_due").await;
    let row_id = subscription_row(&app, &subscription_id)
        .await
        .expect("seeded row")
        .id;
    let audit_before = audit_rows(&app, row_id).await.len();

    let event_id = format!("evt_invoice_paid_{}", Uuid::now_v7());
    let payload = invoice_event_payload(
        &event_id,
        "invoice.paid",
        Utc::now().timestamp(),
        &format!("in_p1065_{}", Uuid::now_v7()),
        Some(&subscription_id),
    );
    let header = stripe_signature_header(&payload, WEBHOOK_SECRET);
    let res = post_webhook(&app, &payload, &header).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());

    // Still past_due: recovery arrives as a subscription
    // update, not as an invoice payment (P1-064).
    let row = subscription_row(&app, &subscription_id)
        .await
        .expect("row still exists");
    assert_eq!(row.status, "past_due");
    assert_eq!(
        audit_rows(&app, row_id).await.len(),
        audit_before,
        "no audit row for a no-op"
    );
}

#[tokio::test]
async fn invoice_payment_failed_duns() {
    let (app, _clock, _provider) = handler_app(Utc::now()).await;
    let user = app.seed_user_unique().await;
    let subscription_id = seed_subscription(&app, user.id, "pro", "active").await;

    let event_id = format!("evt_invoice_failed_{}", Uuid::now_v7());
    let payload = invoice_event_payload(
        &event_id,
        "invoice.payment_failed",
        Utc::now().timestamp(),
        &format!("in_p1065_{}", Uuid::now_v7()),
        Some(&subscription_id),
    );
    let header = stripe_signature_header(&payload, WEBHOOK_SECRET);
    let res = post_webhook(&app, &payload, &header).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());

    let row = subscription_row(&app, &subscription_id)
        .await
        .expect("row still exists");
    assert_eq!(row.status, "past_due");
    let mails = mails_for_user(&app, "subscription-dunning", user.id).await;
    assert_eq!(mails.len(), 1, "exactly one dunning mail");
    let resolved = effective_plan(&app.state.plans, &app.pool, user.id)
        .await
        .expect("resolve plan");
    assert_eq!(resolved.plan.code, "free", "past_due grants nothing");
}

#[tokio::test]
async fn invoice_payment_failed_one_off_is_200() {
    let (app, _clock, _provider) = handler_app(Utc::now()).await;
    let event_id = format!("evt_invoice_failed_oneoff_{}", Uuid::now_v7());
    let payload = invoice_event_payload(
        &event_id,
        "invoice.payment_failed",
        Utc::now().timestamp(),
        &format!("in_p1065_{}", Uuid::now_v7()),
        None,
    );
    let header = stripe_signature_header(&payload, WEBHOOK_SECRET);
    let res = post_webhook(&app, &payload, &header).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert!(ledger_row(&app, &event_id).await.is_some());
}

#[tokio::test]
async fn event_for_unknown_subscription_is_200() {
    let (app, _clock, _provider) = handler_app(Utc::now()).await;
    let subscription_id = format!("sub_p1065_{}", Uuid::now_v7());
    let event_id = format!("evt_sub_unknown_{}", Uuid::now_v7());
    let payload = subscription_event_payload(
        &event_id,
        "customer.subscription.updated",
        Utc::now().timestamp(),
        &subscription_id,
        Some("active"),
        Some(1794844800),
        Some(false),
    );
    let header = stripe_signature_header(&payload, WEBHOOK_SECRET);
    let res = post_webhook(&app, &payload, &header).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert!(subscription_row(&app, &subscription_id).await.is_none());
}

#[tokio::test]
async fn payment_failed_for_canceled_row_is_200() {
    let (app, _clock, _provider) = handler_app(Utc::now()).await;
    let user = app.seed_user_unique().await;
    let subscription_id = seed_subscription(&app, user.id, "pro", "canceled").await;
    let row_id = subscription_row(&app, &subscription_id)
        .await
        .expect("seeded row")
        .id;
    let audit_before = audit_rows(&app, row_id).await.len();

    let event_id = format!("evt_invoice_failed_canceled_{}", Uuid::now_v7());
    let payload = invoice_event_payload(
        &event_id,
        "invoice.payment_failed",
        Utc::now().timestamp(),
        &format!("in_p1065_{}", Uuid::now_v7()),
        Some(&subscription_id),
    );
    let header = stripe_signature_header(&payload, WEBHOOK_SECRET);
    let res = post_webhook(&app, &payload, &header).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());

    // Canceled is terminal: the machine rejects the move, the
    // handler logs it, the row is untouched.
    let row = subscription_row(&app, &subscription_id)
        .await
        .expect("row still exists");
    assert_eq!(row.status, "canceled");
    assert_eq!(
        audit_rows(&app, row_id).await.len(),
        audit_before,
        "a rejected move writes nothing"
    );
    assert!(
        mails_for_user(&app, "subscription-dunning", user.id)
            .await
            .is_empty(),
        "no dunning mail for a rejected move"
    );
}

#[tokio::test]
async fn checkout_replay_creates_one_row() {
    let frozen = Utc::now();
    let (app, _clock, provider) = handler_app(frozen).await;
    let user = app.seed_user_unique().await;
    let subscription_id = format!("sub_p1065_{}", Uuid::now_v7());
    let period_end = DateTime::from_timestamp(1794844800, 0).expect("in range");
    provider.with_subscription(canned_fetch(
        &subscription_id,
        SubscriptionStatus::Active,
        period_end,
    ));
    set_pro_price(&app).await;

    let event_id = format!("evt_checkout_replay_{}", Uuid::now_v7());
    let payload = checkout_completed_payload(
        &event_id,
        Utc::now().timestamp(),
        Some(user.id),
        &subscription_id,
    );
    let header = stripe_signature_header(&payload, WEBHOOK_SECRET);
    let first = post_webhook(&app, &payload, &header).await;
    assert_eq!(first.status, StatusCode::OK, "{}", first.text());
    let row = subscription_row(&app, &subscription_id)
        .await
        .expect("first delivery created the row");
    let first_stamp = ledger_row(&app, &event_id)
        .await
        .expect("stored")
        .dispatched_at;

    // The identical redelivery: 200, exactly one row, and the
    // stamp does not move.
    let second = post_webhook(&app, &payload, &header).await;
    assert_eq!(second.status, StatusCode::OK, "{}", second.text());
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM subscriptions WHERE provider_subscription_id = $1",
    )
    .bind(&subscription_id)
    .fetch_one(&app.pool)
    .await
    .expect("count rows");
    assert_eq!(count, 1, "exactly one row for the subscription");
    assert_eq!(
        ledger_row(&app, &event_id)
            .await
            .expect("stored")
            .dispatched_at,
        first_stamp,
        "the replay moves no stamp"
    );
    assert_eq!(
        audit_rows(&app, row.id).await.len(),
        1,
        "exactly one creation audit row"
    );
}

#[tokio::test]
async fn replay_after_failed_dispatch_applies() {
    let frozen = Utc::now();
    let (app, _clock, provider) = handler_app(frozen).await;
    let user = app.seed_user_unique().await;
    let subscription_id = format!("sub_p1065_{}", Uuid::now_v7());
    let period_end = DateTime::from_timestamp(1794844800, 0).expect("in range");
    provider.with_subscription(canned_fetch(
        &subscription_id,
        SubscriptionStatus::Active,
        period_end,
    ));
    set_pro_price(&app).await;
    // The first fetch fails transiently: the handler 500s
    // after the store, so `dispatched_at` stays NULL.
    provider.fail_next_fetch();

    let event_id = format!("evt_checkout_redispatch_{}", Uuid::now_v7());
    let payload = checkout_completed_payload(
        &event_id,
        Utc::now().timestamp(),
        Some(user.id),
        &subscription_id,
    );
    let header = stripe_signature_header(&payload, WEBHOOK_SECRET);
    let first = post_webhook(&app, &payload, &header).await;
    assert_eq!(
        first.status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "{}",
        first.text()
    );
    assert_eq!(first.json()["code"], "PF-5000");
    assert!(subscription_row(&app, &subscription_id).await.is_none());
    assert_eq!(
        ledger_row(&app, &event_id)
            .await
            .expect("stored")
            .dispatched_at,
        None,
        "a failed dispatch stamps nothing"
    );

    // The provider's retry (the same delivery): the ledger
    // sees an undispatched row and dispatches again — the
    // fetch succeeds this time, and the row is created.
    let second = post_webhook(&app, &payload, &header).await;
    assert_eq!(second.status, StatusCode::OK, "{}", second.text());
    assert!(subscription_row(&app, &subscription_id).await.is_some());
    assert_eq!(
        ledger_row(&app, &event_id)
            .await
            .expect("stored")
            .dispatched_at
            .map(|t| t.timestamp_micros()),
        Some(frozen.timestamp_micros()),
        "the redelivery stamps the dispatch"
    );
}

/// P1-066 — reordered delivery: a subscription update that
/// arrives BEFORE its checkout names a row that does not exist
/// yet, so it is a logged 200 and nothing is written. The
/// checkout that follows still creates the row — and the row is
/// built from the provider's FRESH state (`fetch_subscription`),
/// not from the stale update, so the reorder heals itself: the
/// early event is not lost work, it was simply early.
#[tokio::test]
async fn reordered_update_before_checkout_heals_on_create() {
    let frozen = Utc::now();
    let (app, _clock, provider) = handler_app(frozen).await;
    let user = app.seed_user_unique().await;
    let subscription_id = format!("sub_p1066_{}", Uuid::now_v7());
    let period_end = DateTime::from_timestamp(1794844800, 0).expect("in range");
    provider.with_subscription(canned_fetch(
        &subscription_id,
        SubscriptionStatus::Active,
        period_end,
    ));
    set_pro_price(&app).await;

    // The update arrives first: no row exists, so the handler
    // logs it and answers 200 — it does not invent a row.
    let early_id = format!("evt_sub_early_{}", Uuid::now_v7());
    let early = subscription_event_payload(
        &early_id,
        "customer.subscription.updated",
        Utc::now().timestamp(),
        &subscription_id,
        Some("active"),
        Some(1794844800),
        Some(false),
    );
    let early_header = stripe_signature_header(&early, WEBHOOK_SECRET);
    let early_res = post_webhook(&app, &early, &early_header).await;
    assert_eq!(early_res.status, StatusCode::OK, "{}", early_res.text());
    assert!(subscription_row(&app, &subscription_id).await.is_none());

    // The checkout follows: the row is created from the fresh
    // fetch (active), exactly one creation audit row.
    let event_id = format!("evt_checkout_late_{}", Uuid::now_v7());
    let payload = checkout_completed_payload(
        &event_id,
        Utc::now().timestamp(),
        Some(user.id),
        &subscription_id,
    );
    let header = stripe_signature_header(&payload, WEBHOOK_SECRET);
    let res = post_webhook(&app, &payload, &header).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let row = subscription_row(&app, &subscription_id)
        .await
        .expect("the late checkout creates the row");
    assert_eq!(row.status, "active");
    let audit = audit_rows(&app, row.id).await;
    assert_eq!(audit.len(), 1, "exactly one creation audit row");
    assert_eq!(audit[0].action, "billing.subscription.created");
}

/// P1-066 — reordered delivery: a delayed update carrying an
/// OLD state must not move the row backwards. The row is
/// `past_due` (a failed payment already dunned it); the stale
/// `trialing` update arrives late and the state machine rejects
/// the move (no return to `trialing`), so the row stays
/// `past_due`, no audit row is written, and the answer is 200.
#[tokio::test]
async fn reordered_stale_backward_move_rejected() {
    let (app, _clock, _provider) = handler_app(Utc::now()).await;
    let user = app.seed_user_unique().await;
    let subscription_id = seed_subscription(&app, user.id, "pro", "active").await;

    // The payment fails first: active -> past_due (dunning).
    let failed_id = format!("evt_invoice_failed_{}", Uuid::now_v7());
    let failed = invoice_event_payload(
        &failed_id,
        "invoice.payment_failed",
        Utc::now().timestamp(),
        &format!("in_p1066_{}", Uuid::now_v7()),
        Some(&subscription_id),
    );
    let failed_header = stripe_signature_header(&failed, WEBHOOK_SECRET);
    let failed_res = post_webhook(&app, &failed, &failed_header).await;
    assert_eq!(failed_res.status, StatusCode::OK, "{}", failed_res.text());
    let row = subscription_row(&app, &subscription_id)
        .await
        .expect("row still exists");
    assert_eq!(row.status, "past_due");
    let audit_before = audit_rows(&app, row.id).await.len();
    assert_eq!(audit_before, 1, "one dunning audit row");

    // The stale update arrives late, claiming `trialing`.
    let stale_id = format!("evt_sub_stale_{}", Uuid::now_v7());
    let stale = subscription_event_payload(
        &stale_id,
        "customer.subscription.updated",
        Utc::now().timestamp(),
        &subscription_id,
        Some("trialing"),
        Some(1794844800),
        Some(false),
    );
    let stale_header = stripe_signature_header(&stale, WEBHOOK_SECRET);
    let stale_res = post_webhook(&app, &stale, &stale_header).await;
    assert_eq!(stale_res.status, StatusCode::OK, "{}", stale_res.text());
    let row = subscription_row(&app, &subscription_id)
        .await
        .expect("row still exists");
    assert_eq!(row.status, "past_due", "the stale move must not apply");
    assert_eq!(
        audit_rows(&app, row.id).await.len(),
        audit_before,
        "no audit row for a rejected move"
    );
}

/// P1-066 — replayed event, single effect: the same
/// status-changing delivery twice (same event id) moves the row
/// once, writes one audit row, and the replay moves no stamp.
#[tokio::test]
async fn replayed_status_change_has_single_effect() {
    let (app, _clock, _provider) = handler_app(Utc::now()).await;
    let user = app.seed_user_unique().await;
    let subscription_id = seed_subscription(&app, user.id, "pro", "trialing").await;

    let event_id = format!("evt_sub_replay_{}", Uuid::now_v7());
    let payload = subscription_event_payload(
        &event_id,
        "customer.subscription.updated",
        Utc::now().timestamp(),
        &subscription_id,
        Some("active"),
        Some(1794844800),
        Some(false),
    );
    let header = stripe_signature_header(&payload, WEBHOOK_SECRET);
    let first = post_webhook(&app, &payload, &header).await;
    assert_eq!(first.status, StatusCode::OK, "{}", first.text());
    let row = subscription_row(&app, &subscription_id)
        .await
        .expect("row still exists");
    assert_eq!(row.status, "active");
    assert_eq!(
        audit_rows(&app, row.id).await.len(),
        1,
        "one audit row for the first delivery"
    );
    let first_stamp = ledger_row(&app, &event_id)
        .await
        .expect("stored")
        .dispatched_at;

    // The identical redelivery: 200, no second transition, no
    // second audit row, stamp unmoved.
    let second = post_webhook(&app, &payload, &header).await;
    assert_eq!(second.status, StatusCode::OK, "{}", second.text());
    let row = subscription_row(&app, &subscription_id)
        .await
        .expect("row still exists");
    assert_eq!(row.status, "active");
    assert_eq!(
        audit_rows(&app, row.id).await.len(),
        1,
        "the replay writes no second audit row"
    );
    assert_eq!(
        ledger_row(&app, &event_id)
            .await
            .expect("stored")
            .dispatched_at,
        first_stamp,
        "the replay moves no stamp"
    );
    let mails = mails_for_user(&app, "subscription-welcome", user.id).await;
    assert_eq!(mails.len(), 1, "exactly one welcome mail");
}

/// P1-066 — reordered delivery: a delayed update for a
/// CANCELED row must not resurrect it. `canceled` is terminal
/// in the state machine, so the late `active` update is a
/// logged 200 and the row stays canceled.
#[tokio::test]
async fn reordered_update_after_delete_stays_canceled() {
    let (app, _clock, _provider) = handler_app(Utc::now()).await;
    let user = app.seed_user_unique().await;
    let subscription_id = seed_subscription(&app, user.id, "pro", "active").await;

    // The delete arrives first: active -> canceled.
    let deleted_id = format!("evt_sub_deleted_{}", Uuid::now_v7());
    let deleted = subscription_event_payload(
        &deleted_id,
        "customer.subscription.deleted",
        Utc::now().timestamp(),
        &subscription_id,
        None,
        None,
        None,
    );
    let deleted_header = stripe_signature_header(&deleted, WEBHOOK_SECRET);
    let deleted_res = post_webhook(&app, &deleted, &deleted_header).await;
    assert_eq!(deleted_res.status, StatusCode::OK, "{}", deleted_res.text());
    let row = subscription_row(&app, &subscription_id)
        .await
        .expect("row still exists");
    assert_eq!(row.status, "canceled");
    let audit_before = audit_rows(&app, row.id).await.len();
    assert_eq!(audit_before, 1, "one cancel audit row");

    // The delayed update arrives late, claiming `active`.
    let late_id = format!("evt_sub_late_{}", Uuid::now_v7());
    let late = subscription_event_payload(
        &late_id,
        "customer.subscription.updated",
        Utc::now().timestamp(),
        &subscription_id,
        Some("active"),
        Some(1794844800),
        Some(false),
    );
    let late_header = stripe_signature_header(&late, WEBHOOK_SECRET);
    let late_res = post_webhook(&app, &late, &late_header).await;
    assert_eq!(late_res.status, StatusCode::OK, "{}", late_res.text());
    let row = subscription_row(&app, &subscription_id)
        .await
        .expect("row still exists");
    assert_eq!(row.status, "canceled", "canceled is terminal");
    assert_eq!(
        audit_rows(&app, row.id).await.len(),
        audit_before,
        "no audit row for a rejected move"
    );
}
