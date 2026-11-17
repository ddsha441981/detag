//! P1-062: the checkout endpoint — `POST /v1/billing/checkout`.
//!
//! The provider is a fake (`FakeProvider`, below): the adapter's
//! HTTP legs have never run against Stripe (no test-mode key
//! exists on this VM — ASSUMPTION-023), so the live 200 path
//! against Stripe is still unproven here. What this suite
//! proves, through the real HTTP layer against the double:
//!
//! - `checkout_guard` — the extractor: anonymous 401, an API
//!   key 403 (no billing for keys, P1-040), a JWT user reaches
//!   the handler.
//! - `checkout_unconfigured_returns_503` — no provider in the
//!   state (the default spawn) is 503 `PF-5005`.
//! - `checkout_validation` — missing / empty `plan_code` is
//!   422, an unknown code is 404 `PF-2010`, Free is 422
//!   (`not_purchasable`), and a plan with no Stripe price is
//!   422 too.
//! - `checkout_success_and_customer_reuse` — a first checkout
//!   sends no customer; a user with a (canceled) subscription
//!   row reuses its `provider_customer_id`. The success/cancel
//!   URLs come from config.
//! - `checkout_idempotent` — the AC: the same key + the same
//!   request replays the stored session with no second
//!   provider call; the same key + a different plan is 409
//!   `PF-2011`; a fresh key is a fresh checkout.
//! - `checkout_idempotency_expires` — past the 24 h TTL the
//!   key is dead and the next call is fresh (frozen clock).
//! - `checkout_provider_invalid_price_is_500` — a configured
//!   price Stripe does not know is 500 `PF-5000` (config
//!   drift), never a client error.

mod common;

use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex,
};

use async_trait::async_trait;
use axum::http::{Method, Request, StatusCode};
use chrono::{DateTime, Duration, Utc};
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
/// what the service asked for — and how many times. The
/// `price_broken` price fails with `InvalidPrice`, mirroring
/// the real adapter's `resource_missing` mapping.
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
        if req.price_id == "price_broken" {
            return Err(BillingError::InvalidPrice(req.price_id));
        }
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
/// scratch DB, `detag_api_harness`), and the checkout tests
/// mutate it (`stripe_price_id` per plan) — so the tests that
/// touch prices serialize on this mutex for their whole run.
/// Without it two tests in this binary race: one sets
/// business's price while the other asserts business has
/// none (the first `checkout_validation` failure). The mutex
/// only orders THIS binary: a concurrent admin_plans run can
/// still interleave — each test therefore sets the exact
/// price it needs immediately before the probe that reads it,
/// and reads through its own app's catalog (cached 5 min, so
/// a later DB change cannot move a price mid-test).
static PRICE_GUARD: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// The request id: a valid UUID, echoed by the middleware.
const REQUEST_ID: &str = "123e4567-e89b-42d3-a456-426614174109";

/// The password every test user registers with.
const PASSWORD: &str = "correct horse battery staple";

/// A per-request XFF address: each request gets its own IP
/// bucket, so the tests never trip each other's rate limits
/// (the P1-003 lesson; the full-UUID form, FLAKY-006).
fn unique_xff() -> String {
    format!("203.0.113.{}", Uuid::now_v7())
}

/// App on a frozen clock with the fake provider. Returns
/// (app, clock, fake) — the fake is shared with the app, so
/// the test reads the call count off it.
async fn checkout_app(frozen_at: DateTime<Utc>) -> (TestApp, Arc<ManualClock>, Arc<FakeProvider>) {
    let fixtures = TestFixtures::from_env().await;
    let mut settings = fixtures.settings();
    settings.disposable_email_domains = "mailinator.com".to_string();
    settings.tos_current_version = "v1".to_string();
    settings.api_key_rate_limit_burst = 1000;
    settings.api_key_rate_limit_refill_per_min = 1000;
    let clock = Arc::new(ManualClock::new(frozen_at));
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

/// App with no provider (the default spawn) — billing is not
/// configured, so checkout is 503.
async fn unconfigured_app() -> TestApp {
    let fixtures = TestFixtures::from_env().await;
    let mut settings = fixtures.settings();
    settings.disposable_email_domains = "mailinator.com".to_string();
    settings.tos_current_version = "v1".to_string();
    settings.api_key_rate_limit_burst = 1000;
    settings.api_key_rate_limit_refill_per_min = 1000;
    TestApp::spawn_with_fixtures_and_settings(fixtures, settings).await
}

/// Register a user, return its id.
async fn register_user(app: &TestApp, email: &str) -> Uuid {
    let body = json!({
        "email": email,
        "password": PASSWORD,
        "accept_tos_version": "v1",
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

/// Mint a full-scope key for the credential holder; return the
/// secret.
async fn create_key(app: &TestApp, token: &str) -> String {
    let body = json!({ "name": "checkout probe", "scopes": ["jobs:read", "jobs:write", "webhooks:manage"] });
    let res = app
        .request(
            Request::builder()
                .method(Method::POST)
                .uri("/v1/api-keys")
                .header("x-request-id", REQUEST_ID)
                .header("x-forwarded-for", unique_xff())
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(axum::body::Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.text());
    res.json()["secret"].as_str().unwrap().to_string()
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

/// Clear a plan's Stripe price (back to the seeded NULL).
async fn clear_price(app: &TestApp, code: &str) {
    sqlx::query("UPDATE plans SET stripe_price_id = NULL WHERE code = $1")
        .bind(code)
        .execute(&app.pool)
        .await
        .expect("clear price");
}

/// Seed a subscription row for the user (a canceled row, so it
/// grants nothing — its `provider_customer_id` is still the
/// customer to reuse).
async fn seed_subscription(
    app: &TestApp,
    user_id: Uuid,
    plan_code: &str,
    customer_id: &str,
    status: &str,
) {
    let plan_id: Uuid = sqlx::query_scalar("SELECT id FROM plans WHERE code = $1")
        .bind(plan_code)
        .fetch_one(&app.pool)
        .await
        .expect("plan id");
    sqlx::query(
        "INSERT INTO subscriptions \
         (user_id, plan_id, provider, provider_customer_id, provider_subscription_id, status) \
         VALUES ($1, $2, 'stripe', $3, 'sub_test_1', $4)",
    )
    .bind(user_id)
    .bind(plan_id)
    .bind(customer_id)
    .bind(status)
    .execute(&app.pool)
    .await
    .expect("seed subscription");
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
async fn checkout_guard() {
    let _guard = PRICE_GUARD.lock().await;
    let (app, _clock, fake) = checkout_app(Utc::now()).await;
    let (admin_id, admin_token) = make_role(
        &app,
        &format!("admin-{}@example.com", Uuid::now_v7()),
        "admin",
    )
    .await;
    let (_user_id, user_token) = make_role(
        &app,
        &format!("user-{}@example.com", Uuid::now_v7()),
        "user",
    )
    .await;
    set_price(&app, "pro", "price_test_pro").await;
    // P1-055: key creation is plan-gated — the admin user
    // needs a Pro grant before it can mint the probe key.
    app.grant_plan(admin_id, "pro").await;
    let admin_key = create_key(&app, &admin_token).await;

    // Anonymous: 401 PF-1001.
    let res = probe(&app, None, json!({"plan_code": "pro"}), None).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());
    assert_eq!(problem_code(&res), "PF-1001");
    assert_eq!(res.json()["request_id"], REQUEST_ID);

    // An admin user's own API key: 403 PF-1002 ("no billing
    // for keys" — the route is not in the extractor's scope
    // table, so the key never reaches the handler).
    let res = probe(&app, Some(&admin_key), json!({"plan_code": "pro"}), None).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.text());
    assert_eq!(problem_code(&res), "PF-1002");

    // A JWT user reaches the handler — 200 with the session.
    let res = probe(&app, Some(&user_token), json!({"plan_code": "pro"}), None).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert_eq!(res.json()["id"], "cs_test_1");
    assert_eq!(res.json()["url"], "https://checkout.example/sess_1");
    assert_eq!(fake.calls(), 1);
}

#[tokio::test]
async fn checkout_unconfigured_returns_503() {
    let app = unconfigured_app().await;
    let (_user_id, user_token) = make_role(
        &app,
        &format!("user-{}@example.com", Uuid::now_v7()),
        "user",
    )
    .await;

    // The admitted user reaches the handler — and the handler
    // answers 503: no Stripe key is configured in this
    // environment, so billing is not configured.
    let res = probe(&app, Some(&user_token), json!({"plan_code": "pro"}), None).await;
    assert_eq!(
        res.status,
        StatusCode::SERVICE_UNAVAILABLE,
        "{}",
        res.text()
    );
    let body = res.json();
    assert_eq!(body["code"], "PF-5005");
    assert_eq!(body["status"], 503);
    assert_eq!(body["title"], "billing_not_configured");
    assert_eq!(body["request_id"], REQUEST_ID);
}

#[tokio::test]
async fn checkout_validation() {
    let _guard = PRICE_GUARD.lock().await;
    let (app, _clock, fake) = checkout_app(Utc::now()).await;
    let (_user_id, user_token) = make_role(
        &app,
        &format!("user-{}@example.com", Uuid::now_v7()),
        "user",
    )
    .await;

    // Missing plan_code: 422 PF-2002.
    let res = probe(&app, Some(&user_token), json!({}), None).await;
    assert_eq!(
        res.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        res.text()
    );
    assert_eq!(problem_code(&res), "PF-2002");

    // Empty plan_code: 422 PF-2002.
    let res = probe(&app, Some(&user_token), json!({"plan_code": ""}), None).await;
    assert_eq!(
        res.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        res.text()
    );
    assert_eq!(problem_code(&res), "PF-2002");

    // Unknown plan code: 404 PF-2010. Business's price is
    // cleared immediately before this probe — it is the first
    // probe that reaches the service, so the catalog caches
    // on it (5 min TTL): a clear after this read would not be
    // seen, and a previous run leaves business priced in the
    // shared DB.
    clear_price(&app, "business").await;
    let res = probe(&app, Some(&user_token), json!({"plan_code": "nope"}), None).await;
    assert_eq!(res.status, StatusCode::NOT_FOUND, "{}", res.text());
    assert_eq!(problem_code(&res), "PF-2010");

    // Free has no checkout: 422 on plan_code.
    let res = probe(&app, Some(&user_token), json!({"plan_code": "free"}), None).await;
    assert_eq!(
        res.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        res.text()
    );
    assert_eq!(problem_code(&res), "PF-2002");
    assert_eq!(
        res.json()["field_errors"][0]["field"],
        "plan_code",
        "{}",
        res.text()
    );
    assert_eq!(
        res.json()["field_errors"][0]["code"],
        "not_purchasable",
        "{}",
        res.text()
    );

    // Business has no Stripe price configured (the seed leaves
    // it NULL, cleared just before the "nope" probe above so
    // the cached catalog holds the NULL): 422, same field error.
    let res = probe(
        &app,
        Some(&user_token),
        json!({"plan_code": "business"}),
        None,
    )
    .await;
    assert_eq!(
        res.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        res.text()
    );
    assert_eq!(problem_code(&res), "PF-2002");
    assert_eq!(
        res.json()["field_errors"][0]["code"],
        "not_purchasable",
        "{}",
        res.text()
    );

    // None of the validation paths called the provider.
    assert_eq!(fake.calls(), 0);
}

#[tokio::test]
async fn checkout_success_and_customer_reuse() {
    let _guard = PRICE_GUARD.lock().await;
    let (app, _clock, fake) = checkout_app(Utc::now()).await;
    let (user_id, user_token) = make_role(
        &app,
        &format!("user-{}@example.com", Uuid::now_v7()),
        "user",
    )
    .await;
    set_price(&app, "pro", "price_test_pro").await;

    // First checkout: no subscription row, so no customer.
    let res = probe(&app, Some(&user_token), json!({"plan_code": "pro"}), None).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert_eq!(res.json()["id"], "cs_test_1");
    let first = fake.last_request().expect("first checkout recorded");
    assert_eq!(first.customer_id, None);
    assert_eq!(first.price_id, "price_test_pro");
    assert_eq!(first.user_id, user_id);
    assert!(
        first
            .success_url
            .ends_with("/billing/success?session_id={CHECKOUT_SESSION_ID}"),
        "success url carries the Stripe session placeholder: {}",
        first.success_url
    );
    assert!(
        first
            .success_url
            .starts_with(app.settings.public_base_url.trim_end_matches('/')),
        "success url comes from PF_PUBLIC_BASE_URL: {}",
        first.success_url
    );
    assert!(
        first.cancel_url.ends_with("/billing/cancel"),
        "cancel url: {}",
        first.cancel_url
    );

    // A canceled subscription row still carries the customer
    // to reuse — the customer survives cancellation.
    seed_subscription(&app, user_id, "pro", "cus_test_1", "canceled").await;
    let res = probe(&app, Some(&user_token), json!({"plan_code": "pro"}), None).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert_eq!(res.json()["id"], "cs_test_2");
    let second = fake.last_request().expect("second checkout recorded");
    assert_eq!(second.customer_id.as_deref(), Some("cus_test_1"));
    assert_eq!(fake.calls(), 2);
}

#[tokio::test]
async fn checkout_idempotent() {
    let _guard = PRICE_GUARD.lock().await;
    let (app, _clock, fake) = checkout_app(Utc::now()).await;
    let (_user_id, user_token) = make_role(
        &app,
        &format!("user-{}@example.com", Uuid::now_v7()),
        "user",
    )
    .await;
    set_price(&app, "pro", "price_test_pro").await;
    set_price(&app, "business", "price_test_business").await;
    // The keys are unique per run: the `idempotency_keys`
    // table persists across runs (shared scratch DB), and a
    // stale row for a fixed key would 409 the next run's
    // first probe (the fingerprint carries the user id).
    let run = Uuid::now_v7().to_string();
    let key_1 = format!("key-1-{run}");
    let key_2 = format!("key-2-{run}");

    // First call with key K: a fresh checkout.
    let res = probe(
        &app,
        Some(&user_token),
        json!({"plan_code": "pro"}),
        Some(&key_1),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert_eq!(res.json()["id"], "cs_test_1");
    assert_eq!(fake.calls(), 1);

    // Same key + same request: the stored session, no second
    // provider call.
    let res = probe(
        &app,
        Some(&user_token),
        json!({"plan_code": "pro"}),
        Some(&key_1),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert_eq!(res.json()["id"], "cs_test_1");
    assert_eq!(res.json()["url"], "https://checkout.example/sess_1");
    assert_eq!(fake.calls(), 1, "a replay must not call the provider");

    // Same key + a different request: 409 PF-2011, and still
    // no provider call.
    let res = probe(
        &app,
        Some(&user_token),
        json!({"plan_code": "business"}),
        Some(&key_1),
    )
    .await;
    assert_eq!(res.status, StatusCode::CONFLICT, "{}", res.text());
    assert_eq!(problem_code(&res), "PF-2011");
    assert_eq!(fake.calls(), 1);

    // A fresh key is a fresh checkout.
    let res = probe(
        &app,
        Some(&user_token),
        json!({"plan_code": "pro"}),
        Some(&key_2),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert_eq!(res.json()["id"], "cs_test_2");
    assert_eq!(fake.calls(), 2);

    // The stored rows are in the table (one row per key).
    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM idempotency_keys WHERE key IN ($1, $2)")
            .bind(&key_1)
            .bind(&key_2)
            .fetch_one(&app.pool)
            .await
            .expect("count idempotency rows");
    assert_eq!(count, 2);
}

#[tokio::test]
async fn checkout_idempotency_expires() {
    let _guard = PRICE_GUARD.lock().await;
    let frozen_at = Utc::now();
    let (app, clock, fake) = checkout_app(frozen_at).await;
    let email = format!("user-{}@example.com", Uuid::now_v7());
    let (_user_id, mut user_token) = make_role(&app, &email, "user").await;
    set_price(&app, "pro", "price_test_pro").await;
    // Unique per run (see checkout_idempotent): a stale row
    // for a fixed key would 409 the next run's first probe.
    let key = format!("key-expire-{}", Uuid::now_v7());

    let res = probe(
        &app,
        Some(&user_token),
        json!({"plan_code": "pro"}),
        Some(&key),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert_eq!(res.json()["id"], "cs_test_1");
    assert_eq!(fake.calls(), 1);

    // 23 h later the key is still live: a replay — but the
    // access token (15 min) is long dead at the new frozen
    // time, so the test logs in again first.
    clock.advance(Duration::hours(23));
    user_token = login_token(&app, &email).await;
    let res = probe(
        &app,
        Some(&user_token),
        json!({"plan_code": "pro"}),
        Some(&key),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert_eq!(res.json()["id"], "cs_test_1");
    assert_eq!(fake.calls(), 1);

    // 25 h after the first call the key is dead: a fresh
    // checkout, and the stale row is replaced (log in again
    // at the new frozen time first).
    clock.advance(Duration::hours(2));
    user_token = login_token(&app, &email).await;
    let res = probe(
        &app,
        Some(&user_token),
        json!({"plan_code": "pro"}),
        Some(&key),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert_eq!(res.json()["id"], "cs_test_2");
    assert_eq!(fake.calls(), 2);
}

#[tokio::test]
async fn checkout_provider_invalid_price_is_500() {
    let _guard = PRICE_GUARD.lock().await;
    let (app, _clock, fake) = checkout_app(Utc::now()).await;
    let (_user_id, user_token) = make_role(
        &app,
        &format!("user-{}@example.com", Uuid::now_v7()),
        "user",
    )
    .await;
    // The plan HAS a price configured — but Stripe does not
    // know it (the fake's `price_broken` leg). That is config
    // drift, not a client error: 500 PF-5000.
    set_price(&app, "pro", "price_broken").await;

    let res = probe(&app, Some(&user_token), json!({"plan_code": "pro"}), None).await;
    assert_eq!(
        res.status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "{}",
        res.text()
    );
    assert_eq!(problem_code(&res), "PF-5000");
    assert_eq!(fake.calls(), 0, "the fake refuses before counting");
}
