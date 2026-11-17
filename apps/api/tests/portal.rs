//! P1-067: the billing portal endpoint — `POST /v1/billing/portal`.
//!
//! The provider is a fake (`FakeProvider`, below): the adapter's
//! HTTP legs have never run against Stripe (no test-mode key
//! exists on this VM — ASSUMPTION-023), so the live 200 path
//! against Stripe is still unproven here. What this suite
//! proves, through the real HTTP layer against the double:
//!
//! - `portal_guard` — the extractor: anonymous 401, an API
//!   key 403 (no billing for keys, P1-040), a JWT user reaches
//!   the handler.
//! - `portal_unconfigured_returns_503` — no provider in the
//!   state (the default spawn) is 503 `PF-5005`.
//! - `portal_no_customer_is_404` — a user with no subscription
//!   row has no provider customer: 404 `PF-2010`, and the
//!   provider is never called.
//! - `portal_success_returns_url` — the AC: a user with a
//!   (canceled) subscription row gets 200 with the portal
//!   `url`; the provider was called once, with the reused
//!   customer id and the `{PF_PUBLIC_BASE_URL}/billing`
//!   return URL.
//! - `portal_provider_failure_is_500` — a provider failure is
//!   500 `PF-5000`, and no provider message reaches the body.

mod common;

use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex,
};

use async_trait::async_trait;
use axum::http::{Method, Request, StatusCode};
use chrono::{DateTime, Utc};
use common::{TestApp, TestFixtures};
use detag_billing::{
    BillingError, BillingProvider, CheckoutRequest, CheckoutSession, Customer, DynBillingProvider,
    Invoice, PortalRequest, PortalSession, SubscriptionState, PROVIDER_STRIPE,
};
use detag_core::clock::{Clock, ManualClock};
use serde_json::json;
use uuid::Uuid;

/// A test double: canned answers, no I/O. It records every
/// portal call (count + last request) so the tests can prove
/// what the service asked for — and how many times. The
/// `cus_broken` customer fails, mirroring a provider-side
/// failure (an invalid stored customer id, config drift).
#[derive(Debug)]
struct FakeProvider {
    calls: AtomicUsize,
    last_request: Mutex<Option<PortalRequest>>,
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

    fn last_request(&self) -> Option<PortalRequest> {
        self.last_request.lock().expect("fake lock").clone()
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
        if req.customer_id == "cus_broken" {
            return Err(BillingError::Provider("boom".to_owned()));
        }
        let _n = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        *self.last_request.lock().expect("fake lock") = Some(req);
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
async fn portal_app(frozen_at: DateTime<Utc>) -> (TestApp, Arc<ManualClock>, Arc<FakeProvider>) {
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
/// configured, so the portal is 503.
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
    let body =
        json!({ "name": "portal probe", "scopes": ["jobs:read", "jobs:write", "webhooks:manage"] });
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

/// POST /v1/billing/portal with the given bearer token (or
/// none for the anonymous probe).
async fn post_portal(app: &TestApp, token: Option<&str>) -> common::TestResponse {
    let mut req = Request::builder()
        .method(Method::POST)
        .uri("/v1/billing/portal")
        .header("x-request-id", REQUEST_ID)
        .header("x-forwarded-for", unique_xff());
    if let Some(token) = token {
        req = req.header("authorization", format!("Bearer {token}"));
    }
    app.request(req.body(axum::body::Body::empty()).unwrap())
        .await
}

#[tokio::test]
async fn portal_guard() {
    let (app, _clock, _fake) = portal_app(Utc::now()).await;
    // Anonymous: 401 PF-1001.
    let res = post_portal(&app, None).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-1001");
    // A JWT user reaches the handler (404: no customer yet —
    // the extractor let them through).
    let email = format!("portal-guard-{}@example.com", Uuid::now_v7());
    let id = register_user(&app, &email).await;
    let token = login_token(&app, &email).await;
    let res = post_portal(&app, Some(&token)).await;
    assert_eq!(res.status, StatusCode::NOT_FOUND, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-2010");
    // An API key is 403 PF-1002 (no billing for keys).
    // P1-055: key creation is plan-gated — the user needs a
    // Pro grant before it can mint the probe key.
    app.grant_plan(id, "pro").await;
    let secret = create_key(&app, &token).await;
    let res = post_portal(&app, Some(&secret)).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-1002");
}

#[tokio::test]
async fn portal_unconfigured_returns_503() {
    let app = unconfigured_app().await;
    let email = format!("portal-503-{}@example.com", Uuid::now_v7());
    let id = register_user(&app, &email).await;
    let token = login_token(&app, &email).await;
    // The user HAS a customer — but billing is not
    // configured, so the answer is still 503.
    seed_subscription(&app, id, "pro", "cus_test_503", "canceled").await;
    let res = post_portal(&app, Some(&token)).await;
    assert_eq!(
        res.status,
        StatusCode::SERVICE_UNAVAILABLE,
        "{}",
        res.text()
    );
    assert_eq!(res.json()["code"], "PF-5005");
}

#[tokio::test]
async fn portal_no_customer_is_404() {
    let (app, _clock, fake) = portal_app(Utc::now()).await;
    let email = format!("portal-404-{}@example.com", Uuid::now_v7());
    let _id = register_user(&app, &email).await;
    let token = login_token(&app, &email).await;
    let res = post_portal(&app, Some(&token)).await;
    assert_eq!(res.status, StatusCode::NOT_FOUND, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-2010");
    assert_eq!(fake.calls(), 0, "no customer means no provider call");
}

#[tokio::test]
async fn portal_success_returns_url() {
    let (app, _clock, fake) = portal_app(Utc::now()).await;
    let email = format!("portal-ok-{}@example.com", Uuid::now_v7());
    let id = register_user(&app, &email).await;
    let token = login_token(&app, &email).await;
    let customer_id = format!("cus_test_{}", Uuid::now_v7());
    seed_subscription(&app, id, "pro", &customer_id, "canceled").await;
    let res = post_portal(&app, Some(&token)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert_eq!(
        res.json()["url"],
        "https://portal.example/session",
        "the AC: the portal URL is returned"
    );
    assert_eq!(fake.calls(), 1, "exactly one provider call");
    let req = fake.last_request().expect("the fake recorded the call");
    assert_eq!(req.customer_id, customer_id, "the reused customer");
    assert_eq!(
        req.return_url,
        format!(
            "{}/billing",
            app.settings.public_base_url.trim_end_matches('/')
        ),
        "the return URL comes from PF_PUBLIC_BASE_URL"
    );
}

#[tokio::test]
async fn portal_provider_failure_is_500() {
    let (app, _clock, _fake) = portal_app(Utc::now()).await;
    let email = format!("portal-500-{}@example.com", Uuid::now_v7());
    let id = register_user(&app, &email).await;
    let token = login_token(&app, &email).await;
    seed_subscription(&app, id, "pro", "cus_broken", "canceled").await;
    let res = post_portal(&app, Some(&token)).await;
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
