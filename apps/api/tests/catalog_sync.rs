//! P1-061: the manual catalog-sync trigger — `POST
//! `/v1/admin/billing/catalog-sync`.
//!
//! The sync engine itself is proven in the billing crate
//! (`crates/billing/src/sync.rs`, `FakeCatalog`); this suite
//! proves the endpoint around it:
//!
//! - `catalog_sync_guard` — the guard: anonymous 401, plain
//!   user / support / an admin's own API key 403.
//! - `catalog_sync_unconfigured_returns_503` — the admitted
//!   admin gets 503 `PF-5005` here: the test environment has
//!   no Stripe key configured, so billing is not configured.
//!   No audit row is written for the 503 (the middleware
//!   audits 2xx writes only).
//!
//! A live sync (200 + a drift report) cannot run here: no
//! Stripe test-mode key exists on this VM and the adapter's
//! HTTP legs have never run against Stripe (ASSUMPTION-023
//! precedent) — the first live run is the admin endpoint with
//! a test-mode key, and the report shape is proven by the
//! billing-crate serialization test.

mod common;

use axum::http::{Method, Request, StatusCode};
use common::{TestApp, TestFixtures};
use detag_core::repos::audit_logs;
use serde_json::json;
use uuid::Uuid;

/// The request id: a valid UUID, echoed by the middleware.
const REQUEST_ID: &str = "123e4567-e89b-42d3-a456-426614174109";

/// The password every test user registers with.
const PASSWORD: &str = "correct horse battery staple";

/// A per-request XFF address: each request gets its own IP
/// bucket, so the tests never trip each other's rate limits
/// (the P1-003 lesson).
fn unique_xff() -> String {
    format!("203.0.113.{}", Uuid::now_v7())
}

/// Test settings: the same shape as the authz matrix
/// (generous key bucket — this suite asserts authorization
/// outcomes, not rate limits).
async fn sync_app() -> TestApp {
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

/// Mint a full-scope key for the credential holder; return the
/// secret.
async fn create_key(app: &TestApp, token: &str) -> String {
    let body =
        json!({ "name": "sync probe", "scopes": ["jobs:read", "jobs:write", "webhooks:manage"] });
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

/// One sync probe: POST the endpoint with an optional
/// credential (`None` = anonymous).
async fn probe(app: &TestApp, credential: Option<&str>) -> common::TestResponse {
    let mut req = Request::builder()
        .method(Method::POST)
        .uri("/v1/admin/billing/catalog-sync")
        .header("x-request-id", REQUEST_ID)
        .header("x-forwarded-for", unique_xff())
        .header("content-type", "application/json");
    if let Some(credential) = credential {
        req = req.header("authorization", format!("Bearer {credential}"));
    }
    app.request(req.body(axum::body::Body::empty()).unwrap())
        .await
}

/// The problem `code` of a response body ("" when the body
/// is not a problem — the assertion message then shows the
/// raw body).
fn problem_code(res: &common::TestResponse) -> String {
    res.json()["code"].as_str().unwrap_or("").to_string()
}

#[tokio::test]
async fn catalog_sync_guard() {
    let app = sync_app().await;
    let (_user_id, user_token) = make_role(
        &app,
        &format!("user-{}@example.com", Uuid::now_v7()),
        "user",
    )
    .await;
    let (_support_id, support_token) = make_role(
        &app,
        &format!("support-{}@example.com", Uuid::now_v7()),
        "support",
    )
    .await;
    let (admin_id, admin_token) = make_role(
        &app,
        &format!("admin-{}@example.com", Uuid::now_v7()),
        "admin",
    )
    .await;
    // P1-055: key creation is plan-gated — the admin user
    // needs a Pro grant before it can mint the probe key.
    app.grant_plan(admin_id, "pro").await;
    let admin_key = create_key(&app, &admin_token).await;

    // Anonymous: 401 PF-1001.
    let res = probe(&app, None).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());
    assert_eq!(problem_code(&res), "PF-1001");
    assert_eq!(res.json()["request_id"], REQUEST_ID);

    // Plain user: 403 PF-1002.
    let res = probe(&app, Some(&user_token)).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.text());
    assert_eq!(problem_code(&res), "PF-1002");

    // Support: 403 PF-1002 (a write admits admin only).
    let res = probe(&app, Some(&support_token)).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.text());
    assert_eq!(problem_code(&res), "PF-1002");

    // An admin user's own API key: 403 PF-1002 ("no admin ever").
    let res = probe(&app, Some(&admin_key)).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.text());
    assert_eq!(problem_code(&res), "PF-1002");
}

#[tokio::test]
async fn catalog_sync_unconfigured_returns_503() {
    let app = sync_app().await;
    let (admin_id, admin_token) = make_role(
        &app,
        &format!("admin-{}@example.com", Uuid::now_v7()),
        "admin",
    )
    .await;

    // The admitted admin reaches the handler — and the
    // handler answers 503: no Stripe key is configured in
    // this environment, so billing is not configured.
    let res = probe(&app, Some(&admin_token)).await;
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

    // A non-2xx write audits nothing: the trail records
    // actions, not attempts. Scoped by actor — the harness
    // DB persists across runs.
    let rows = audit_logs::list_for_action(&app.pool, "admin.billing.catalog_synced")
        .await
        .expect("list audit rows");
    let own: Vec<_> = rows
        .iter()
        .filter(|row| row.actor_user_id == Some(admin_id))
        .collect();
    assert_eq!(
        own.len(),
        0,
        "a 503 sync must audit nothing (found {} row(s) by this actor)",
        own.len()
    );
}
