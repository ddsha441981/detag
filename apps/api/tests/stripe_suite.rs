//! P1-072: the Stripe suite — the consolidation pass over the
//! Stripe-side behaviour built in P1-059–P1-071.
//!
//! The earlier tickets each proved one mechanism in its own
//! file (the adapter in `detag-billing`, the receiver in
//! `webhook.rs`, the handlers in `webhook_handlers.rs`, the
//! checkout in `checkout.rs`, the proration math in the
//! billing crate). This suite proves the mechanisms TOGETHER,
//! through the real HTTP layer, the way Stripe drives them in
//! production — one subscription's whole life, the signature
//! edge cases, and the two idempotency layers (the ledger and
//! the state machine) — so a later refactor that breaks the
//! wiring fails here first. The provider is the DELEGATING
//! double ([`DelegatingProvider`]): the real `StripeAdapter`
//! for signature verification + mapping, canned
//! `fetch_subscription` answers (the adapter's HTTP legs are
//! still untested live — no test-mode key on this VM,
//! ASSUMPTION-023). No test here calls the provider.
//!
//! What this suite proves:
//!
//! - `stripe_lifecycle_trial_to_active_to_past_due_to_canceled`
//!   — the AC: one subscription walks the whole life through
//!   signed webhook deliveries on a frozen test clock — a
//!   trial checkout creates the row, a subscription update
//!   activates it (welcome mail, Pro granted), a failed
//!   invoice duns it (day-0 mail, Pro revoked), a deletion
//!   cancels it (goodbye mail), and a replay of the deletion
//!   changes nothing. The audit trail carries the four
//!   statuses in order, one row per step.
//! - `stripe_suite_expired_and_future_timestamps_rejected` —
//!   a correctly-signed delivery whose timestamp is 10
//!   minutes old, or 10 minutes in the future, is 400
//!   `PF-2013` with no row: the 5-minute tolerance bites
//!   both ways (the unit tests prove the primitive; this
//!   proves the receiver around it).
//! - `stripe_suite_malformed_signature_rejected` — a header
//!   with no timestamp, or with no `v1`, is 400 `PF-2013`
//!   with no row.
//! - `stripe_suite_replay_with_different_body_is_noop` — the
//!   same event id redelivered with a DIFFERENT body (a
//!   different subscription) is 200, exactly one ledger row,
//!   the ORIGINAL payload kept, and no second subscription
//!   row: the ledger dedups on the event id before dispatch,
//!   not on payload equality.
//! - `stripe_suite_duplicate_payment_failed_mails_once` —
//!   two DISTINCT `invoice.payment_failed` events for the
//!   same subscription mail the buyer exactly once: the
//!   ledger sees two events, the state machine's same-state
//!   no-op makes the domain effect idempotent (the
//!   replay-with-the-same-id case lives in
//!   `webhook_handlers.rs`; this is the second idempotency
//!   layer).

mod common;

use std::sync::Arc;

use axum::http::{Method, Request, StatusCode};
use chrono::{DateTime, Duration, Utc};
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
/// plan row immediately before each probe. The SAME value the
/// handlers suite uses (`price_p1065_pro`): the plans table is
/// shared across test binaries, and an identical value makes
/// a concurrent overwrite unobservable (the P1-065 pattern).
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
    stripe_signature_header_at(payload, secret, Utc::now().timestamp())
}

/// The same header with an explicit timestamp — the
/// expired/future cases sign a real HMAC over a stale or
/// ahead timestamp, so only the tolerance can reject them.
fn stripe_signature_header_at(payload: &str, secret: &str, ts: i64) -> String {
    let signed = format!("{ts}.{payload}");
    let mut mac =
        Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("hmac accepts any key length");
    mac.update(signed.as_bytes());
    format!("t={ts},v1={}", hex::encode(mac.finalize().into_bytes()))
}

/// The app on a frozen clock with the delegating provider.
/// Returns (app, clock, provider) — the provider is shared with
/// the app, so the test adds canned fetch answers to it.
async fn suite_app(
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
/// session's `client_reference_id`. The session object carries
/// the fields `construct_event` requires (the webhook suite's
/// shape).
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
/// envelope; the object is read by `map_webhook`. The object
/// must be a COMPLETE typed `Subscription` (the handlers
/// suite's shape — a partial object fails deserialisation and
/// the delivery is 400 `PF-2013`, never reaching the handler).
fn subscription_event_payload(
    event_id: &str,
    event_type: &str,
    now: i64,
    subscription_id: &str,
    status: Option<&str>,
    current_period_end: Option<i64>,
    cancel_at_period_end: Option<bool>,
) -> String {
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
/// `invoice.payment_failed`).
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

/// A verified-but-ignored event envelope
/// (`invoiceitem.created`, the P1-060 shape).
fn ignored_event_payload(event_id: &str, now: i64) -> String {
    format!(
        r#"{{"id":"{event_id}","object":"event","api_version":"2024-06-20","created":{now},"data":{{"object":{{"id":"ii_123","object":"invoiceitem","amount":1000,"currency":"usd","customer":"cus_123","date":{now},"description":"Test Invoice Item","discountable":false,"invoice":"in_123","livemode":false,"metadata":{{}},"period":{{"start":{now},"end":{now}}},"proration":false,"quantity":3}}}},"livemode":false,"pending_webhooks":1,"request":{{"id":"req_123","idempotency_key":null}},"type":"invoiceitem.created"}}"#
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
    let provider_subscription_id = format!("sub_p1072_{}", Uuid::now_v7());
    // The dunning CHECK (P1-070) requires a `past_due` row to
    // carry `past_due_since`: stamp it from the app clock.
    let since = if status == "past_due" {
        Some(app.state.clock.now())
    } else {
        None
    };
    sqlx::query(
        "INSERT INTO subscriptions (user_id, plan_id, provider, provider_customer_id, provider_subscription_id, status, past_due_since) \
         VALUES ($1, $2, 'stripe', 'cus_test_123', $3, $4, $5)",
    )
    .bind(user_id)
    .bind(plan.id)
    .bind(&provider_subscription_id)
    .bind(status)
    .bind(since)
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

/// The plan the user resolves to right now, by code.
async fn resolved_plan(app: &TestApp, user_id: Uuid) -> String {
    effective_plan(&app.state.plans, &app.pool, user_id)
        .await
        .expect("resolve plan")
        .plan
        .code
        .to_string()
}

#[tokio::test]
async fn stripe_lifecycle_trial_to_active_to_past_due_to_canceled() {
    let frozen = Utc::now();
    let (app, clock, provider) = suite_app(frozen).await;
    let user = app.seed_user_unique().await;
    let subscription_id = format!("sub_p1072_{}", Uuid::now_v7());
    // Whole seconds: the subscription.updated payload carries
    // `current_period_end` as epoch seconds, so a sub-second
    // period end could never round-trip through step 2.
    let period_end =
        DateTime::from_timestamp(frozen.timestamp() + 30 * 86_400, 0).expect("in range");
    provider.with_subscription(canned_fetch(
        &subscription_id,
        SubscriptionStatus::Trialing,
        period_end,
    ));
    set_pro_price(&app).await;

    // 1. The trial checkout creates the row — trialing, no
    // mail yet. A trial IS the Pro plan: `active_for_user`
    // matches `trialing` too, so the trial grants Pro from
    // the first day (the P1-064 live-status rule).
    let checkout_id = format!("evt_p1072_life_checkout_{}", Uuid::now_v7());
    let payload = checkout_completed_payload(
        &checkout_id,
        Utc::now().timestamp(),
        Some(user.id),
        &subscription_id,
    );
    let header = stripe_signature_header(&payload, WEBHOOK_SECRET);
    let res = post_webhook(&app, &payload, &header).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let row = subscription_row(&app, &subscription_id)
        .await
        .expect("the checkout created the row");
    assert_eq!(row.status, "trialing");
    // Postgres TIMESTAMPTZ stores microseconds, so the frozen
    // nanosecond clock truncates on write — compare at micros
    // (the webhook suite's pattern).
    assert_eq!(
        row.current_period_end.map(|t| t.timestamp_micros()),
        Some(period_end.timestamp_micros())
    );
    assert_eq!(
        resolved_plan(&app, user.id).await,
        "pro",
        "the trial grants Pro from day one"
    );
    assert!(mails_for_user(&app, "subscription-welcome", user.id)
        .await
        .is_empty());
    assert!(mails_for_user(&app, "subscription-dunning", user.id)
        .await
        .is_empty());
    assert!(mails_for_user(&app, "subscription-canceled", user.id)
        .await
        .is_empty());
    assert_eq!(
        ledger_row(&app, &checkout_id)
            .await
            .expect("stored")
            .dispatched_at
            .map(|t| t.timestamp_micros()),
        Some(frozen.timestamp_micros()),
        "the checkout is dispatched at the frozen clock"
    );

    // 2. Seven days in, the trial converts: the welcome mail
    // goes out and Pro stays granted, with one status-change
    // audit row.
    clock.advance(Duration::days(7));
    let activated_id = format!("evt_p1072_life_activated_{}", Uuid::now_v7());
    let payload = subscription_event_payload(
        &activated_id,
        "customer.subscription.updated",
        Utc::now().timestamp(),
        &subscription_id,
        Some("active"),
        Some(period_end.timestamp()),
        Some(false),
    );
    let header = stripe_signature_header(&payload, WEBHOOK_SECRET);
    let res = post_webhook(&app, &payload, &header).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let row = subscription_row(&app, &subscription_id)
        .await
        .expect("row still exists");
    assert_eq!(row.status, "active");
    // Postgres TIMESTAMPTZ stores microseconds, so the frozen
    // nanosecond clock truncates on write — compare at micros
    // (the webhook suite's pattern).
    assert_eq!(
        row.current_period_end.map(|t| t.timestamp_micros()),
        Some(period_end.timestamp_micros())
    );
    assert_eq!(resolved_plan(&app, user.id).await, "pro");
    assert_eq!(
        mails_for_user(&app, "subscription-welcome", user.id)
            .await
            .len(),
        1
    );

    // 3. Twenty days later the renewal payment fails: the row
    // goes past_due, the day-0 dunning mail goes out, Pro is
    // revoked.
    clock.advance(Duration::days(20));
    let failed_id = format!("evt_p1072_life_failed_{}", Uuid::now_v7());
    let payload = invoice_event_payload(
        &failed_id,
        "invoice.payment_failed",
        Utc::now().timestamp(),
        &format!("in_p1072_{}", Uuid::now_v7()),
        Some(&subscription_id),
    );
    let header = stripe_signature_header(&payload, WEBHOOK_SECRET);
    let res = post_webhook(&app, &payload, &header).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let row = subscription_row(&app, &subscription_id)
        .await
        .expect("row still exists");
    assert_eq!(row.status, "past_due");
    assert_eq!(
        row.past_due_since.map(|t| t.timestamp_micros()),
        Some((frozen + Duration::days(27)).timestamp_micros()),
        "the dunning clock starts at the failed payment"
    );
    assert_eq!(row.dunning_step, 0);
    assert_eq!(
        resolved_plan(&app, user.id).await,
        "free",
        "past_due grants nothing"
    );
    assert_eq!(
        mails_for_user(&app, "subscription-dunning", user.id)
            .await
            .len(),
        1
    );

    // 4. One day later Stripe deletes the subscription: the
    // row cancels, the goodbye mail goes out.
    clock.advance(Duration::days(1));
    let deleted_id = format!("evt_p1072_life_deleted_{}", Uuid::now_v7());
    let payload = subscription_event_payload(
        &deleted_id,
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
    assert_eq!(resolved_plan(&app, user.id).await, "free");
    assert_eq!(
        mails_for_user(&app, "subscription-canceled", user.id)
            .await
            .len(),
        1
    );
    assert_eq!(
        ledger_row(&app, &deleted_id)
            .await
            .expect("stored")
            .dispatched_at
            .map(|t| t.timestamp_micros()),
        Some((frozen + Duration::days(28)).timestamp_micros()),
        "the deletion is dispatched at the advanced clock"
    );

    // The audit trail is the whole life, in order, one row
    // per step — and a replay of the terminal deletion changes
    // nothing: canceled is terminal.
    let audit = audit_rows(&app, row.id).await;
    assert_eq!(audit.len(), 4, "one audit row per life step");
    let statuses: Vec<&str> = audit
        .iter()
        .map(|a| {
            a.after
                .as_ref()
                .and_then(|v| v.get("status"))
                .and_then(|v| v.as_str())
                .unwrap_or("?")
        })
        .collect();
    assert_eq!(statuses, ["trialing", "active", "past_due", "canceled"]);
    assert_eq!(audit[0].action, "billing.subscription.created");
    assert!(audit[1..]
        .iter()
        .all(|a| a.action == "billing.subscription.status_changed"));
    let replay = post_webhook(&app, &payload, &header).await;
    assert_eq!(replay.status, StatusCode::OK, "{}", replay.text());
    let row = subscription_row(&app, &subscription_id)
        .await
        .expect("row still exists");
    assert_eq!(row.status, "canceled");
    assert_eq!(
        audit_rows(&app, row.id).await.len(),
        4,
        "no second audit row"
    );
    assert_eq!(
        mails_for_user(&app, "subscription-canceled", user.id)
            .await
            .len(),
        1
    );
}

#[tokio::test]
async fn stripe_suite_expired_and_future_timestamps_rejected() {
    let (app, _clock, _provider) = suite_app(Utc::now()).await;
    for (label, ts) in [
        ("expired", Utc::now().timestamp() - 600),
        ("future", Utc::now().timestamp() + 600),
    ] {
        let event_id = format!("evt_p1072_{label}_{}", Uuid::now_v7());
        let payload = ignored_event_payload(&event_id, Utc::now().timestamp());
        // A real HMAC over a stale/ahead timestamp: only the
        // 5-minute tolerance can reject this.
        let header = stripe_signature_header_at(&payload, WEBHOOK_SECRET, ts);
        let res = post_webhook(&app, &payload, &header).await;
        assert_eq!(
            res.status,
            StatusCode::BAD_REQUEST,
            "{label}: {}",
            res.text()
        );
        assert_eq!(res.json()["code"], "PF-2013");
        assert!(
            ledger_row(&app, &event_id).await.is_none(),
            "{label}: no row stored"
        );
    }
}

#[tokio::test]
async fn stripe_suite_malformed_signature_rejected() {
    let (app, _clock, _provider) = suite_app(Utc::now()).await;
    // A header with a v1 but no timestamp, and one with a
    // timestamp but no v1: `construct_event` cannot verify
    // either, so both are 400 with no row.
    let now = Utc::now().timestamp();
    let event_id = format!("evt_p1072_no_ts_{}", Uuid::now_v7());
    let payload = ignored_event_payload(&event_id, now);
    let mut mac =
        Hmac::<Sha256>::new_from_slice(WEBHOOK_SECRET.as_bytes()).expect("hmac accepts any key");
    mac.update(format!("{now}.{payload}").as_bytes());
    let v1_only = format!("v1={}", hex::encode(mac.finalize().into_bytes()));
    let res = post_webhook(&app, &payload, &v1_only).await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-2013");
    assert!(ledger_row(&app, &event_id).await.is_none(), "no row stored");

    let event_id = format!("evt_p1072_no_v1_{}", Uuid::now_v7());
    let payload = ignored_event_payload(&event_id, now);
    let t_only = format!("t={now}");
    let res = post_webhook(&app, &payload, &t_only).await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-2013");
    assert!(ledger_row(&app, &event_id).await.is_none(), "no row stored");
}

#[tokio::test]
async fn stripe_suite_replay_with_different_body_is_noop() {
    let frozen = Utc::now();
    let (app, _clock, provider) = suite_app(frozen).await;
    let user = app.seed_user_unique().await;
    let first_sub = format!("sub_p1072_{}", Uuid::now_v7());
    let second_sub = format!("sub_p1072_{}", Uuid::now_v7());
    let period_end = frozen + Duration::days(30);
    // The second subscription's fetch is registered too: if
    // the handler wrongly re-ran, the row WOULD be created and
    // this test would fail — the fetch existing is what makes
    // the noop strict.
    provider.with_subscription(canned_fetch(
        &first_sub,
        SubscriptionStatus::Active,
        period_end,
    ));
    provider.with_subscription(canned_fetch(
        &second_sub,
        SubscriptionStatus::Active,
        period_end,
    ));
    set_pro_price(&app).await;

    let event_id = format!("evt_p1072_replay_body_{}", Uuid::now_v7());
    let payload =
        checkout_completed_payload(&event_id, Utc::now().timestamp(), Some(user.id), &first_sub);
    let header = stripe_signature_header(&payload, WEBHOOK_SECRET);
    let first = post_webhook(&app, &payload, &header).await;
    assert_eq!(first.status, StatusCode::OK, "{}", first.text());
    assert!(subscription_row(&app, &first_sub).await.is_some());

    // The same event id, a different body (a different
    // subscription), signed correctly for that body.
    let changed = checkout_completed_payload(
        &event_id,
        Utc::now().timestamp(),
        Some(user.id),
        &second_sub,
    );
    let changed_header = stripe_signature_header(&changed, WEBHOOK_SECRET);
    let second = post_webhook(&app, &changed, &changed_header).await;
    assert_eq!(second.status, StatusCode::OK, "{}", second.text());
    assert_eq!(second.json()["received"], true);
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM webhook_events WHERE provider = 'stripe' AND event_id = $1",
    )
    .bind(&event_id)
    .fetch_one(&app.pool)
    .await
    .expect("count read");
    assert_eq!(count, 1, "exactly one ledger row for the event id");
    let stored = ledger_row(&app, &event_id).await.expect("stored");
    assert_eq!(
        stored.payload["data"]["object"]["subscription"], first_sub,
        "the ORIGINAL payload is kept, not the replay's"
    );
    assert!(
        subscription_row(&app, &second_sub).await.is_none(),
        "the replay created no second subscription"
    );
    let user_rows: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM subscriptions WHERE user_id = $1")
            .bind(user.id)
            .fetch_one(&app.pool)
            .await
            .expect("count read");
    assert_eq!(user_rows, 1, "exactly one subscription row for the user");
}

#[tokio::test]
async fn stripe_suite_duplicate_payment_failed_mails_once() {
    let (app, _clock, _provider) = suite_app(Utc::now()).await;
    let user = app.seed_user_unique().await;
    let subscription_id = seed_subscription(&app, user.id, "pro", "active").await;

    // Two DISTINCT payment-failed events (different event ids,
    // different invoices) for the same subscription.
    for (label, invoice) in [("first", Uuid::now_v7()), ("second", Uuid::now_v7())] {
        let event_id = format!("evt_p1072_dup_failed_{label}_{}", Uuid::now_v7());
        let payload = invoice_event_payload(
            &event_id,
            "invoice.payment_failed",
            Utc::now().timestamp(),
            &format!("in_p1072_{invoice}"),
            Some(&subscription_id),
        );
        let header = stripe_signature_header(&payload, WEBHOOK_SECRET);
        let res = post_webhook(&app, &payload, &header).await;
        assert_eq!(res.status, StatusCode::OK, "{label}: {}", res.text());
        let row = subscription_row(&app, &subscription_id)
            .await
            .expect("row still exists");
        assert_eq!(row.status, "past_due");
    }

    // The ledger saw two events — but the domain effect ran
    // once: one dunning mail, one status-change audit row.
    let ledger_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM webhook_events WHERE event_type = 'invoice.payment_failed' AND payload -> 'data' -> 'object' ->> 'subscription' = $1",
    )
    .bind(&subscription_id)
    .fetch_one(&app.pool)
    .await
    .expect("count read");
    assert_eq!(ledger_count, 2, "both deliveries are stored");
    assert_eq!(
        mails_for_user(&app, "subscription-dunning", user.id)
            .await
            .len(),
        1,
        "exactly one day-0 dunning mail"
    );
    let row = subscription_row(&app, &subscription_id)
        .await
        .expect("row still exists");
    let audit = audit_rows(&app, row.id).await;
    assert_eq!(audit.len(), 1, "exactly one status-change audit row");
    assert_eq!(audit[0].action, "billing.subscription.status_changed");
}
