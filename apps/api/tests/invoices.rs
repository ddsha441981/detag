//! P1-068: the invoices list endpoint — `GET /v1/billing/invoices`.
//!
//! The provider is a fake (`FakeProvider`, below): the adapter's
//! HTTP legs have never run against Stripe (no test-mode key
//! exists on this VM — ASSUMPTION-023), so the live 200 path
//! against Stripe is still unproven here. What this suite
//! proves, through the real HTTP layer against the double:
//!
//! - `invoices_guard` — the extractor: anonymous 401, an API
//!   key 403 (no billing for keys, P1-040), a JWT user reaches
//!   the handler.
//! - `invoices_unconfigured_returns_503` — no provider in the
//!   state (the default spawn) is 503 `PF-5005`.
//! - `invoices_no_customer_is_empty` — a user with no
//!   subscription row has no provider customer: 200 with an
//!   empty list, and the provider is never called.
//! - `invoices_success_shape_contract` — the AC: a user with a
//!   subscription row gets 200 with the invoices in our shape
//!   (id, number, amount, currency, status, created_at,
//!   hosted_url, download_url), newest first; the provider was
//!   called once with the reused customer id and the limit; a
//!   second identical request inside the 5-minute window makes
//!   no second provider call (the Redis cache serves it), and
//!   the cache key carries a TTL inside the window.
//! - `invoices_provider_failure_is_500` — a provider failure is
//!   500 `PF-5000`, and no provider message reaches the body.

mod common;

use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex,
};

use async_trait::async_trait;
use axum::http::{Method, Request, StatusCode};
use chrono::{DateTime, TimeZone, Utc};
use common::{TestApp, TestFixtures};
use deadpool_redis::redis::cmd;
use detag_billing::{
    BillingError, BillingProvider, CheckoutRequest, CheckoutSession, Customer, DynBillingProvider,
    Invoice, PortalRequest, PortalSession, SubscriptionState, PROVIDER_STRIPE,
};
use detag_core::clock::{Clock, ManualClock};
use detag_core::redis::keys::cache_key;
use serde_json::json;
use uuid::Uuid;

/// A test double: canned invoices, no I/O. It records every
/// list call (count + last request) so the tests can prove
/// what the service asked for — and how many times. The
/// `cus_broken` customer fails, mirroring a provider-side
/// failure (an invalid stored customer id, config drift).
#[derive(Debug)]
struct FakeProvider {
    calls: AtomicUsize,
    last_request: Mutex<Option<(String, u32)>>,
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

    fn last_request(&self) -> Option<(String, u32)> {
        self.last_request.lock().expect("fake lock").clone()
    }

    /// The canned list, newest first: a paid invoice with
    /// every field set, then an open one with the optional
    /// fields absent (both shapes must survive the mapping).
    fn canned() -> Vec<Invoice> {
        vec![
            Invoice {
                id: "in_test_2".to_owned(),
                number: Some("INV-2026-002".to_owned()),
                amount: 49900,
                currency: "INR".to_owned(),
                status: "paid".to_owned(),
                created_at: Utc.with_ymd_and_hms(2026, 11, 17, 10, 0, 0).unwrap(),
                hosted_url: Some("https://pay.stripe.com/invoice/acct_1/inv_2".to_owned()),
                download_url: Some("https://pay.stripe.com/invoice/acct_1/inv_2/pdf".to_owned()),
            },
            Invoice {
                id: "in_test_1".to_owned(),
                number: None,
                amount: 14900,
                currency: "INR".to_owned(),
                status: "open".to_owned(),
                created_at: Utc.with_ymd_and_hms(2026, 10, 17, 10, 0, 0).unwrap(),
                hosted_url: None,
                download_url: None,
            },
        ]
    }
}

#[async_trait]
impl BillingProvider for FakeProvider {
    fn provider_name(&self) -> detag_billing::ProviderName {
        PROVIDER_STRIPE
    }

    async fn create_checkout(
        &self,
        _req: CheckoutRequest,
    ) -> Result<CheckoutSession, BillingError> {
        Ok(CheckoutSession {
            id: "cs_test_1".to_owned(),
            url: "https://checkout.example/sess_1".to_owned(),
        })
    }

    async fn create_portal(&self, req: PortalRequest) -> Result<PortalSession, BillingError> {
        Ok(PortalSession {
            url: format!("https://portal.example/{}", req.customer_id),
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
        customer_id: &str,
        limit: u32,
    ) -> Result<Vec<Invoice>, BillingError> {
        if customer_id == "cus_broken" {
            return Err(BillingError::Provider("boom".to_owned()));
        }
        let _n = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        *self.last_request.lock().expect("fake lock") = Some((customer_id.to_owned(), limit));
        Ok(Self::canned())
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
async fn invoices_app(frozen_at: DateTime<Utc>) -> (TestApp, Arc<ManualClock>, Arc<FakeProvider>) {
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
/// configured, so the invoices list is 503.
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

/// Mint a full-scope key for the credential holder; return the
/// secret.
async fn create_key(app: &TestApp, token: &str) -> String {
    let body = json!({
        "name": "invoices probe",
        "scopes": ["jobs:read", "jobs:write", "webhooks:manage"],
    });
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

/// GET /v1/billing/invoices with the given bearer token (or
/// none for the anonymous probe).
async fn get_invoices(app: &TestApp, token: Option<&str>) -> common::TestResponse {
    let mut req = Request::builder()
        .method(Method::GET)
        .uri("/v1/billing/invoices")
        .header("x-request-id", REQUEST_ID)
        .header("x-forwarded-for", unique_xff());
    if let Some(token) = token {
        req = req.header("authorization", format!("Bearer {token}"));
    }
    app.request(req.body(axum::body::Body::empty()).unwrap())
        .await
}

#[tokio::test]
async fn invoices_guard() {
    let (app, _clock, _fake) = invoices_app(Utc::now()).await;
    // Anonymous: 401 PF-1001.
    let res = get_invoices(&app, None).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-1001");
    // A JWT user reaches the handler (200 with an empty list:
    // no customer yet — the extractor let them through).
    let email = format!("invoices-guard-{}@example.com", Uuid::now_v7());
    let id = register_user(&app, &email).await;
    let token = login_token(&app, &email).await;
    let res = get_invoices(&app, Some(&token)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert_eq!(res.json(), serde_json::json!([]));
    // An API key is 403 PF-1002 (no billing for keys).
    // P1-055: key creation is plan-gated — the user needs a
    // Pro grant before it can mint the probe key.
    app.grant_plan(id, "pro").await;
    let secret = create_key(&app, &token).await;
    let res = get_invoices(&app, Some(&secret)).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-1002");
}

#[tokio::test]
async fn invoices_unconfigured_returns_503() {
    let app = unconfigured_app().await;
    let email = format!("invoices-503-{}@example.com", Uuid::now_v7());
    let id = register_user(&app, &email).await;
    let token = login_token(&app, &email).await;
    // The user HAS a customer — but billing is not
    // configured, so the answer is still 503.
    seed_subscription(&app, id, "pro", "cus_test_503", "canceled").await;
    let res = get_invoices(&app, Some(&token)).await;
    assert_eq!(
        res.status,
        StatusCode::SERVICE_UNAVAILABLE,
        "{}",
        res.text()
    );
    assert_eq!(res.json()["code"], "PF-5005");
}

#[tokio::test]
async fn invoices_no_customer_is_empty() {
    let (app, _clock, fake) = invoices_app(Utc::now()).await;
    let email = format!("invoices-404-{}@example.com", Uuid::now_v7());
    let _id = register_user(&app, &email).await;
    let token = login_token(&app, &email).await;
    let res = get_invoices(&app, Some(&token)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert_eq!(res.json(), serde_json::json!([]));
    assert_eq!(fake.calls(), 0, "no customer means no provider call");
}

#[tokio::test]
async fn invoices_success_shape_contract() {
    let (app, _clock, fake) = invoices_app(Utc::now()).await;
    let email = format!("invoices-ok-{}@example.com", Uuid::now_v7());
    let id = register_user(&app, &email).await;
    let token = login_token(&app, &email).await;
    let customer_id = format!("cus_test_{}", Uuid::now_v7());
    seed_subscription(&app, id, "pro", &customer_id, "canceled").await;
    let res = get_invoices(&app, Some(&token)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    // The AC: the shape contract — every field of our shape,
    // newest first, with the Stripe-hosted URLs.
    let body = res.json();
    let list = body.as_array().expect("the list is a bare array");
    assert_eq!(list.len(), 2, "the canned list: {body}");
    let newest = &list[0];
    assert_eq!(newest["id"], "in_test_2");
    assert_eq!(newest["number"], "INV-2026-002");
    assert_eq!(newest["amount"], 49900);
    assert_eq!(newest["currency"], "INR");
    assert_eq!(newest["status"], "paid");
    // chrono's serde for `DateTime<Utc>` writes `Z`, not
    // `+00:00` (`to_rfc3339()` writes the latter — the wire
    // format is what the contract pins).
    assert_eq!(newest["created_at"], "2026-11-17T10:00:00Z");
    assert_eq!(
        newest["hosted_url"],
        "https://pay.stripe.com/invoice/acct_1/inv_2"
    );
    assert_eq!(
        newest["download_url"],
        "https://pay.stripe.com/invoice/acct_1/inv_2/pdf"
    );
    let older = &list[1];
    assert_eq!(older["id"], "in_test_1");
    assert_eq!(older["number"], serde_json::Value::Null);
    assert_eq!(older["amount"], 14900);
    assert_eq!(older["status"], "open");
    assert_eq!(older["created_at"], "2026-10-17T10:00:00Z");
    assert_eq!(older["hosted_url"], serde_json::Value::Null);
    assert_eq!(older["download_url"], serde_json::Value::Null);
    assert_eq!(fake.calls(), 1, "exactly one provider call");
    let (called_customer, called_limit) = fake.last_request().expect("the fake recorded the call");
    assert_eq!(called_customer, customer_id, "the reused customer");
    assert_eq!(
        called_limit, 20,
        "the endpoint asks for one screen of invoices"
    );
    // The 5-minute cache: a second identical request makes no
    // second provider call, and the cache key carries a TTL
    // inside the window.
    let res2 = get_invoices(&app, Some(&token)).await;
    assert_eq!(res2.status, StatusCode::OK, "{}", res2.text());
    assert_eq!(res2.json(), body, "the cached list is identical");
    assert_eq!(
        fake.calls(),
        1,
        "the second request was served from the cache"
    );
    let mut conn = app.state.redis.get().await.expect("redis pool checkout");
    let ttl: i64 = cmd("TTL")
        .arg(cache_key("invoices", &customer_id))
        .query_async(&mut conn)
        .await
        .expect("cache TTL read");
    assert!(
        (1..=300).contains(&ttl),
        "the cache entry lives at most 5 minutes, got TTL {ttl}"
    );
}

#[tokio::test]
async fn invoices_provider_failure_is_500() {
    let (app, _clock, _fake) = invoices_app(Utc::now()).await;
    let email = format!("invoices-500-{}@example.com", Uuid::now_v7());
    let id = register_user(&app, &email).await;
    let token = login_token(&app, &email).await;
    seed_subscription(&app, id, "pro", "cus_broken", "canceled").await;
    let res = get_invoices(&app, Some(&token)).await;
    assert_eq!(
        res.status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "{}",
        res.text()
    );
    assert_eq!(res.json()["code"], "PF-5000");
    assert!(
        !res.text().contains("boom"),
        "no provider message reaches the body: {}",
        res.text()
    );
}
