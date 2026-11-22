//! P1-069: upgrade/downgrade proration — `POST /v1/billing/checkout`.
//!
//! The provider is a fake (the same shape as `checkout.rs`'s):
//! the adapter's HTTP legs have never run against Stripe (no
//! test-mode key exists on this VM — ASSUMPTION-023), so the
//! live 200 path against Stripe is still unproven here. What
//! this suite proves, through the real HTTP layer against the
//! double:
//!
//! - `upgrade_returns_session_with_preview` — the AC's upgrade
//!   leg: a live Pro subscription buying Business returns 200
//!   with the session AND the proration preview (half the
//!   period left: half the old plan credited, half the new
//!   plan charged), and the provider request carries the live
//!   subscription id to supersede.
//! - `same_plan_is_422` — buying the plan already held is 422
//!   `not_purchasable`, with no provider call.
//! - `downgrade_schedules_at_period_end` — the AC's downgrade
//!   leg: a live Business subscription buying Pro returns 202
//!   with the scheduled change (effective at the period end,
//!   proration all zero), no provider call, and the durable
//!   row is in `scheduled_plan_changes`.
//! - `downgrade_to_free_schedules` — a downgrade to Free is a
//!   schedule too (not the first-purchase 422): the buyer
//!   keeps Pro until the period end.
//! - `downgrade_twice_conflicts_and_replay_returns_202` — one
//!   pending change per subscription: a fresh key conflicts
//!   (409 `PF-2011`), while the same key replays the stored
//!   202 with no second row.
//! - `upgrade_without_period_end_has_zero_preview` — a live
//!   row with no period end has no period to prorate: the
//!   upgrade still returns 200, with a zero preview.
//! - `upgrade_idempotent_replay` — the same key + the same
//!   upgrade replays the stored session with no second
//!   provider call.

mod common;

use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex,
};

use async_trait::async_trait;
use axum::http::{Method, Request, StatusCode};
use chrono::{DateTime, TimeZone, Utc};
use common::{TestApp, TestFixtures};
use detag_billing::{
    BillingError, BillingProvider, CheckoutRequest, CheckoutSession, Customer, DynBillingProvider,
    Invoice, PortalRequest, PortalSession, SubscriptionState, PROVIDER_STRIPE,
};
use detag_core::clock::{Clock, ManualClock};
use serde_json::json;
use uuid::Uuid;

/// A test double: canned answers, no I/O. It records every
/// checkout call (count + last request) so the tests can prove
/// what the service asked for — and how many times.
#[derive(Debug)]
struct FakeProvider {
    calls: AtomicUsize,
    last_request: Mutex<Option<CheckoutRequest>>,
}

impl FakeProvider {
    fn new() -> Self {
        Self {
            calls: AtomicUsize::new(0),
            last_request: Mutex::new(None),
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    fn last_request(&self) -> Option<CheckoutRequest> {
        self.last_request.lock().expect("fake lock").clone()
    }
}

#[async_trait]
impl BillingProvider for FakeProvider {
    fn provider_name(&self) -> detag_billing::ProviderName {
        PROVIDER_STRIPE
    }

    async fn create_checkout(&self, req: CheckoutRequest) -> Result<CheckoutSession, BillingError> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        *self.last_request.lock().expect("fake lock") = Some(req);
        Ok(CheckoutSession {
            id: format!("cs_test_{n}"),
            url: format!("https://checkout.example/sess_{n}"),
        })
    }

    async fn create_portal(&self, _req: PortalRequest) -> Result<PortalSession, BillingError> {
        Ok(PortalSession {
            url: "https://portal.example/session".to_owned(),
        })
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

    fn map_webhook(
        &self,
        _event: detag_billing::BillingEvent,
    ) -> Result<Option<detag_billing::DomainEvent>, BillingError> {
        Ok(None)
    }
}

/// The plans table is shared by every test binary (one
/// scratch DB, `detag_api_harness`), and these tests mutate
/// it (`stripe_price_id` per plan) — so the tests that touch
/// prices serialize on this mutex for their whole run (the
/// checkout tests' pattern; the mutex only orders THIS
/// binary, so each test sets the exact price it needs
/// immediately before the probe that reads it).
static PRICE_GUARD: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// The request id: a valid UUID, echoed by the middleware.
const REQUEST_ID: &str = "123e4567-e89b-42d3-a456-426614174109";

/// The password every test user registers with.
const PASSWORD: &str = "correct horse battery staple";

/// The frozen clock: 2026-11-18T00:00:00Z. The seeded period
/// ends 2026-12-03T00:00:00Z — exactly halfway through the
/// 30-day period that started 2026-11-03 — so the upgrade
/// preview is exactly half of each leg.
fn frozen_now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 11, 18, 0, 0, 0).unwrap()
}

/// The seeded period end: 2026-12-03T00:00:00Z.
fn period_end() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 12, 3, 0, 0, 0).unwrap()
}

/// A per-request XFF address: each request gets its own IP
/// bucket, so the tests never trip each other's rate limits
/// (the P1-003 lesson; the full-UUID form, FLAKY-006).
fn unique_xff() -> String {
    format!("203.0.113.{}", Uuid::now_v7())
}

/// App on a frozen clock with the fake provider. Returns
/// (app, clock, fake) — the fake is shared with the app, so
/// the test reads the call count off it.
async fn proration_app() -> (TestApp, Arc<ManualClock>, Arc<FakeProvider>) {
    let fixtures = TestFixtures::from_env().await;
    let mut settings = fixtures.settings();
    settings.disposable_email_domains = "mailinator.com".to_string();
    settings.tos_current_version = "v1".to_string();
    settings.api_key_rate_limit_burst = 1000;
    settings.api_key_rate_limit_refill_per_min = 1000;
    let clock = Arc::new(ManualClock::new(frozen_now()));
    let fake = Arc::new(FakeProvider::new());
    let provider: DynBillingProvider = Arc::clone(&fake) as DynBillingProvider;
    let app = TestApp::spawn_with_fixtures_settings_clock_and_provider(
        fixtures,
        settings,
        Arc::clone(&clock) as Arc<dyn Clock>,
        provider,
    )
    .await;
    (app, clock, fake)
}

/// Register a user, return its id.
async fn register_user(app: &TestApp, email: &str) -> Uuid {
    let body = json!({
        "email": email,
        "password": PASSWORD,
        "accept_tos_version": "v1",
        "accept_aup_version": "v1",
    });
    let res = app
        .request(
            Request::builder()
                .method(Method::POST)
                .uri("/v1/auth/register")
                .header("x-request-id", REQUEST_ID)
                .header("x-forwarded-for", unique_xff())
                .header("content-type", "application/json")
                .body(axum::body::Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.text());
    res.json()["id"].as_str().unwrap().parse().unwrap()
}

/// Log in, return the access token.
async fn login_token(app: &TestApp, email: &str) -> String {
    let body = json!({
        "email": email,
        "password": PASSWORD,
    });
    let res = app
        .request(
            Request::builder()
                .method(Method::POST)
                .uri("/v1/auth/login")
                .header("x-request-id", REQUEST_ID)
                .header("x-forwarded-for", unique_xff())
                .header("content-type", "application/json")
                .body(axum::body::Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    res.json()["access_token"].as_str().unwrap().to_string()
}

/// Register a user, set its role in the database, log in,
/// return (id, token). The role is set by a direct UPDATE —
/// there is no self-serve way to become an admin or support
/// (the role is set by the owner out of band).
async fn make_role(app: &TestApp, email: &str, role: &str) -> (Uuid, String) {
    let id = register_user(app, email).await;
    sqlx::query("UPDATE users SET role = $1 WHERE id = $2")
        .bind(role)
        .bind(id)
        .execute(&app.pool)
        .await
        .expect("set role");
    let token = login_token(app, email).await;
    (id, token)
}

/// Give a plan a Stripe price (the seed leaves
/// `stripe_price_id` NULL until the catalog sync fills it).
async fn set_price(app: &TestApp, code: &str, price_id: &str) {
    sqlx::query("UPDATE plans SET stripe_price_id = $1 WHERE code = $2")
        .bind(price_id)
        .bind(code)
        .execute(&app.pool)
        .await
        .expect("set price");
}

/// Seed a LIVE subscription row for the user (the provider
/// subscription id is unique per call) and return it. The
/// period end is `Some` for the proration tests, `None` for
/// the no-period test.
async fn seed_live_subscription(
    app: &TestApp,
    user_id: Uuid,
    plan_code: &str,
    customer_id: &str,
    period_end: Option<DateTime<Utc>>,
) -> String {
    let plan_id: Uuid = sqlx::query_scalar("SELECT id FROM plans WHERE code = $1")
        .bind(plan_code)
        .fetch_one(&app.pool)
        .await
        .expect("plan id");
    let provider_subscription_id = format!("sub_p1069_{}", Uuid::now_v7());
    sqlx::query(
        "INSERT INTO subscriptions \
         (user_id, plan_id, provider, provider_customer_id, provider_subscription_id, status, current_period_end) \
         VALUES ($1, $2, 'stripe', $3, $4, 'active', $5)",
    )
    .bind(user_id)
    .bind(plan_id)
    .bind(customer_id)
    .bind(&provider_subscription_id)
    .bind(period_end)
    .execute(&app.pool)
    .await
    .expect("seed subscription");
    provider_subscription_id
}

/// The pending scheduled change for a provider subscription,
/// if one exists.
async fn pending_change(
    app: &TestApp,
    provider_subscription_id: &str,
) -> Option<(String, String, DateTime<Utc>)> {
    sqlx::query_as::<_, (String, String, DateTime<Utc>)>(
        "SELECT spc.status, p.code, spc.effective_at \
         FROM scheduled_plan_changes spc \
         JOIN subscriptions s ON s.id = spc.subscription_id \
         JOIN plans p ON p.id = spc.to_plan_id \
         WHERE s.provider_subscription_id = $1",
    )
    .bind(provider_subscription_id)
    .fetch_optional(&app.pool)
    .await
    .expect("read scheduled change")
}

/// One checkout probe: POST the endpoint with a JSON body, an
/// optional credential (`None` = anonymous) and an optional
/// `Idempotency-Key`.
async fn probe(
    app: &TestApp,
    credential: Option<&str>,
    body: serde_json::Value,
    idempotency_key: Option<&str>,
) -> common::TestResponse {
    let mut req = Request::builder()
        .method(Method::POST)
        .uri("/v1/billing/checkout")
        .header("x-request-id", REQUEST_ID)
        .header("x-forwarded-for", unique_xff())
        .header("content-type", "application/json");
    if let Some(credential) = credential {
        req = req.header("authorization", format!("Bearer {credential}"));
    }
    if let Some(key) = idempotency_key {
        req = req.header("idempotency-key", key);
    }
    app.request(
        req.body(axum::body::Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap(),
    )
    .await
}

/// The problem `code` of a response body ("" when the body
/// is not a problem — the assertion message then shows the
/// raw body).
fn problem_code(res: &common::TestResponse) -> String {
    res.json()["code"].as_str().unwrap_or("").to_string()
}

#[tokio::test]
async fn upgrade_returns_session_with_preview() {
    let _guard = PRICE_GUARD.lock().await;
    let (app, _clock, fake) = proration_app().await;
    let (user_id, user_token) = make_role(
        &app,
        &format!("user-{}@example.com", Uuid::now_v7()),
        "user",
    )
    .await;
    set_price(&app, "business", "price_test_business").await;
    let provider_sub =
        seed_live_subscription(&app, user_id, "pro", "cus_test_upgrade", Some(period_end())).await;

    // Half the 30-day period is left: half the old plan
    // credited (49900 / 2), half the new plan charged
    // (199900 / 2), net 75000 — effective now.
    let res = probe(
        &app,
        Some(&user_token),
        json!({"plan_code": "business"}),
        None,
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let body = res.json();
    assert_eq!(body["id"], "cs_test_1");
    assert_eq!(body["url"], "https://checkout.example/sess_1");
    let proration = &body["proration"];
    assert_eq!(proration["kind"], "upgrade", "{}", res.text());
    assert_eq!(proration["credit_paise"], 24_950, "{}", res.text());
    assert_eq!(proration["charge_paise"], 99_950, "{}", res.text());
    assert_eq!(proration["net_paise"], 75_000, "{}", res.text());
    assert_eq!(
        proration["effective_at"],
        "2026-11-18T00:00:00Z",
        "{}",
        res.text()
    );

    // Exactly one provider call, for the Business price, with
    // the live subscription id to supersede and the reused
    // customer.
    assert_eq!(fake.calls(), 1);
    let last = fake.last_request().expect("upgrade checkout recorded");
    assert_eq!(last.price_id, "price_test_business");
    assert_eq!(
        last.upgrade_from_provider_subscription_id.as_deref(),
        Some(provider_sub.as_str())
    );
    assert_eq!(last.customer_id.as_deref(), Some("cus_test_upgrade"));
    assert_eq!(last.user_id, user_id);
}

#[tokio::test]
async fn same_plan_is_422() {
    let _guard = PRICE_GUARD.lock().await;
    let (app, _clock, fake) = proration_app().await;
    let (user_id, user_token) = make_role(
        &app,
        &format!("user-{}@example.com", Uuid::now_v7()),
        "user",
    )
    .await;
    set_price(&app, "pro", "price_test_pro").await;
    seed_live_subscription(&app, user_id, "pro", "cus_test_same", Some(period_end())).await;

    let res = probe(&app, Some(&user_token), json!({"plan_code": "pro"}), None).await;
    assert_eq!(
        res.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        res.text()
    );
    assert_eq!(problem_code(&res), "PF-2002");
    assert_eq!(res.json()["field_errors"][0]["field"], "plan_code");
    assert_eq!(
        res.json()["field_errors"][0]["code"],
        "not_purchasable",
        "{}",
        res.text()
    );
    assert_eq!(fake.calls(), 0, "a same-plan buy never calls the provider");
}

#[tokio::test]
async fn downgrade_schedules_at_period_end() {
    let _guard = PRICE_GUARD.lock().await;
    let (app, _clock, fake) = proration_app().await;
    let (user_id, user_token) = make_role(
        &app,
        &format!("user-{}@example.com", Uuid::now_v7()),
        "user",
    )
    .await;
    set_price(&app, "pro", "price_test_pro").await;
    let provider_sub = seed_live_subscription(
        &app,
        user_id,
        "business",
        "cus_test_down",
        Some(period_end()),
    )
    .await;

    // 202, not 200: no session — the change is scheduled at
    // the period end, with a zero proration (nothing moves
    // now), and the buyer keeps Business until then.
    let res = probe(&app, Some(&user_token), json!({"plan_code": "pro"}), None).await;
    assert_eq!(res.status, StatusCode::ACCEPTED, "{}", res.text());
    let body = res.json();
    assert_eq!(body["scheduled"], true, "{}", res.text());
    assert_eq!(body["plan_code"], "pro", "{}", res.text());
    assert_eq!(body["from_plan_code"], "business", "{}", res.text());
    assert_eq!(
        body["effective_at"],
        "2026-12-03T00:00:00Z",
        "{}",
        res.text()
    );
    let proration = &body["proration"];
    assert_eq!(proration["kind"], "downgrade", "{}", res.text());
    assert_eq!(proration["credit_paise"], 0, "{}", res.text());
    assert_eq!(proration["charge_paise"], 0, "{}", res.text());
    assert_eq!(proration["net_paise"], 0, "{}", res.text());
    assert_eq!(
        proration["effective_at"],
        "2026-12-03T00:00:00Z",
        "{}",
        res.text()
    );
    assert!(
        body.get("id").is_none(),
        "a downgrade has no session: {}",
        res.text()
    );
    assert_eq!(fake.calls(), 0, "a downgrade never calls the provider");

    // The durable row: scheduled, to Pro, at the period end.
    let (status, to_code, effective_at) = pending_change(&app, &provider_sub)
        .await
        .expect("a scheduled change row exists");
    assert_eq!(status, "scheduled");
    assert_eq!(to_code, "pro");
    assert_eq!(effective_at, period_end());
}

#[tokio::test]
async fn downgrade_to_free_schedules() {
    let _guard = PRICE_GUARD.lock().await;
    let (app, _clock, fake) = proration_app().await;
    let (user_id, user_token) = make_role(
        &app,
        &format!("user-{}@example.com", Uuid::now_v7()),
        "user",
    )
    .await;
    let provider_sub =
        seed_live_subscription(&app, user_id, "pro", "cus_test_free", Some(period_end())).await;

    // Free has no checkout for a first purchase — but a live
    // Pro buyer asking for Free is a downgrade, so it
    // schedules (202), it does not 422.
    let res = probe(&app, Some(&user_token), json!({"plan_code": "free"}), None).await;
    assert_eq!(res.status, StatusCode::ACCEPTED, "{}", res.text());
    assert_eq!(res.json()["plan_code"], "free", "{}", res.text());
    assert_eq!(
        res.json()["effective_at"],
        "2026-12-03T00:00:00Z",
        "{}",
        res.text()
    );
    assert_eq!(fake.calls(), 0);
    let (status, to_code, _) = pending_change(&app, &provider_sub)
        .await
        .expect("a scheduled change row exists");
    assert_eq!(status, "scheduled");
    assert_eq!(to_code, "free");
}

#[tokio::test]
async fn downgrade_twice_conflicts_and_replay_returns_202() {
    let _guard = PRICE_GUARD.lock().await;
    let (app, _clock, fake) = proration_app().await;
    let (user_id, user_token) = make_role(
        &app,
        &format!("user-{}@example.com", Uuid::now_v7()),
        "user",
    )
    .await;
    set_price(&app, "pro", "price_test_pro").await;
    let provider_sub = seed_live_subscription(
        &app,
        user_id,
        "business",
        "cus_test_twice",
        Some(period_end()),
    )
    .await;
    let run = Uuid::now_v7().to_string();
    let key_1 = format!("key-down-1-{run}");
    let key_2 = format!("key-down-2-{run}");

    // First call with key K: the 202, and one row.
    let res = probe(
        &app,
        Some(&user_token),
        json!({"plan_code": "pro"}),
        Some(&key_1),
    )
    .await;
    assert_eq!(res.status, StatusCode::ACCEPTED, "{}", res.text());
    assert!(pending_change(&app, &provider_sub).await.is_some());

    // Same key + same request: the stored 202, no second row.
    let res = probe(
        &app,
        Some(&user_token),
        json!({"plan_code": "pro"}),
        Some(&key_1),
    )
    .await;
    assert_eq!(res.status, StatusCode::ACCEPTED, "{}", res.text());
    assert_eq!(res.json()["plan_code"], "pro", "{}", res.text());
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM scheduled_plan_changes spc \
         JOIN subscriptions s ON s.id = spc.subscription_id \
         WHERE s.provider_subscription_id = $1 AND spc.status = 'scheduled'",
    )
    .bind(&provider_sub)
    .fetch_one(&app.pool)
    .await
    .expect("count scheduled changes");
    assert_eq!(count, 1, "a replay must not schedule twice");

    // A fresh key for the same downgrade: 409 — one pending
    // change per subscription.
    let res = probe(
        &app,
        Some(&user_token),
        json!({"plan_code": "pro"}),
        Some(&key_2),
    )
    .await;
    assert_eq!(res.status, StatusCode::CONFLICT, "{}", res.text());
    assert_eq!(problem_code(&res), "PF-2011");
    assert_eq!(fake.calls(), 0);
}

#[tokio::test]
async fn upgrade_without_period_end_has_zero_preview() {
    let _guard = PRICE_GUARD.lock().await;
    let (app, _clock, fake) = proration_app().await;
    let (user_id, user_token) = make_role(
        &app,
        &format!("user-{}@example.com", Uuid::now_v7()),
        "user",
    )
    .await;
    set_price(&app, "business", "price_test_business").await;
    seed_live_subscription(&app, user_id, "pro", "cus_test_noperiod", None).await;

    // No period end on the row: no period to prorate, so the
    // preview is zero — but the upgrade still returns 200
    // with a session, effective now.
    let res = probe(
        &app,
        Some(&user_token),
        json!({"plan_code": "business"}),
        None,
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let proration = &res.json()["proration"];
    assert_eq!(proration["kind"], "upgrade", "{}", res.text());
    assert_eq!(proration["credit_paise"], 0, "{}", res.text());
    assert_eq!(proration["charge_paise"], 0, "{}", res.text());
    assert_eq!(proration["net_paise"], 0, "{}", res.text());
    assert_eq!(
        proration["effective_at"],
        "2026-11-18T00:00:00Z",
        "{}",
        res.text()
    );
    assert_eq!(fake.calls(), 1);
}

#[tokio::test]
async fn upgrade_idempotent_replay() {
    let _guard = PRICE_GUARD.lock().await;
    let (app, _clock, fake) = proration_app().await;
    let (user_id, user_token) = make_role(
        &app,
        &format!("user-{}@example.com", Uuid::now_v7()),
        "user",
    )
    .await;
    set_price(&app, "business", "price_test_business").await;
    seed_live_subscription(&app, user_id, "pro", "cus_test_replay", Some(period_end())).await;
    let key = format!("key-up-replay-{}", Uuid::now_v7());

    let res = probe(
        &app,
        Some(&user_token),
        json!({"plan_code": "business"}),
        Some(&key),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert_eq!(res.json()["id"], "cs_test_1");
    assert_eq!(res.json()["proration"]["kind"], "upgrade", "{}", res.text());
    assert_eq!(fake.calls(), 1);

    // Same key + same request: the stored session (with its
    // preview), no second provider call.
    let res = probe(
        &app,
        Some(&user_token),
        json!({"plan_code": "business"}),
        Some(&key),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert_eq!(res.json()["id"], "cs_test_1");
    assert_eq!(
        res.json()["proration"]["credit_paise"],
        24_950,
        "{}",
        res.text()
    );
    assert_eq!(fake.calls(), 1, "a replay must not call the provider");
}
