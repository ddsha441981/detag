//! P1-055: plan feature flags — FR-P-01.
//!
//! The plan's `features` jsonb (plus the `lane` for
//! `priority_lane`) is checked by the services, and a
//! caller whose plan does not grant the feature is denied
//! with 403 `PF-3003` whose detail carries the FR-P-06
//! upgrade hint. The gated call sites that exist today:
//!
//! - `POST /v1/api-keys` — the `api_keys` feature (Free
//!   grants nothing, Pro and Business grant it).
//! - `POST /v1/webhook-endpoints` — the `webhooks` feature
//!   (same split; an entitled caller still reaches the 501
//!   stub until the real handler lands).
//!
//! `batch` and `priority_lane` have no HTTP surface yet —
//! no batch endpoint exists, and job create is still the
//! 501 stub with no lane selection — so their mapping is
//! proven by the `grants()` unit tests in
//! `crates/core/src/plans.rs` (Business grants both, Pro
//! grants neither) and their call sites land with the batch
//! endpoint and the Phase 2 intake (ASSUMPTION-021). A key
//! caller has no user in the context, so the webhook gate
//! passes keys through to the stub: the key could only be
//! minted by an entitled owner (key creation is gated too).

mod common;

use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
};
use common::{TestApp, TestFixtures};
use serde_json::{json, Value};
use uuid::Uuid;

/// The request id: a valid UUID, echoed by the middleware.
const REQUEST_ID: &str = "123e4567-e89b-42d3-a456-426614174109";

/// The password every test user registers with.
const PASSWORD: &str = "correct horse battery staple";

/// Test settings: the register flow needs a ToS version; the
/// key bucket is generous so the suite asserts feature
/// outcomes, not rate limits.
async fn flags_app() -> TestApp {
    let fixtures = TestFixtures::from_env().await;
    let mut settings = fixtures.settings();
    settings.disposable_email_domains = "mailinator.com".to_string();
    settings.tos_current_version = "v1".to_string();
    settings.api_key_rate_limit_burst = 1000;
    settings.api_key_rate_limit_refill_per_min = 1000;
    TestApp::spawn_with_fixtures_and_settings(fixtures, settings).await
}

fn unique_email(prefix: &str) -> String {
    format!("{prefix}-{}@example.com", Uuid::now_v7())
}

/// A per-test client identity (TEST-NET-3; the middleware and
/// the lockout treat the XFF first entry as an opaque key, so
/// a UUID suffix is a valid distinct address here — FLAKY-006:
/// the old `% 250` pool let one test's lockout 423 another
/// test's login, so every test draws its own bucket).
fn unique_xff() -> String {
    format!("203.0.113.{}", Uuid::now_v7())
}

/// Register a user, return its id.
async fn register_user(app: &TestApp, email: &str) -> Uuid {
    let res = app
        .request(
            Request::builder()
                .method(Method::POST)
                .uri("/v1/auth/register")
                .header("x-request-id", REQUEST_ID)
                .header("x-forwarded-for", unique_xff())
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&json!({
                        "email": email,
                        "password": PASSWORD,
                        "accept_tos_version": "v1",
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await;
    assert_eq!(res.status, StatusCode::CREATED, "register: {}", res.text());
    res.json()["id"].as_str().unwrap().parse().unwrap()
}

/// Log the user in, return the JWT.
async fn login_token(app: &TestApp, email: &str) -> String {
    let res = app
        .request(
            Request::builder()
                .method(Method::POST)
                .uri("/v1/auth/login")
                .header("x-request-id", REQUEST_ID)
                .header("x-forwarded-for", unique_xff())
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&json!({
                        "email": email,
                        "password": PASSWORD,
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await;
    assert_eq!(res.status, StatusCode::OK, "login: {}", res.text());
    res.json()["access_token"]
        .as_str()
        .expect("login returns an access token")
        .to_string()
}

async fn authed_user(app: &TestApp, prefix: &str) -> (Uuid, String) {
    let email = unique_email(prefix);
    let id = register_user(app, &email).await;
    let token = login_token(app, &email).await;
    (id, token)
}

/// POST /v1/api-keys as the JWT holder.
async fn create_key(app: &TestApp, token: &str, body: Value) -> common::TestResponse {
    app.request(
        Request::builder()
            .method(Method::POST)
            .uri("/v1/api-keys")
            .header("x-request-id", REQUEST_ID)
            .header("x-forwarded-for", unique_xff())
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap(),
    )
    .await
}

/// POST /v1/webhook-endpoints as the JWT holder (empty body
/// — the endpoint takes no payload yet).
async fn create_webhook_endpoint_jwt(app: &TestApp, token: &str) -> common::TestResponse {
    app.request(
        Request::builder()
            .method(Method::POST)
            .uri("/v1/webhook-endpoints")
            .header("x-request-id", REQUEST_ID)
            .header("x-forwarded-for", unique_xff())
            .header("authorization", format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await
}

/// POST /v1/webhook-endpoints as an API key.
async fn create_webhook_endpoint_key(app: &TestApp, secret: &str) -> common::TestResponse {
    app.request(
        Request::builder()
            .method(Method::POST)
            .uri("/v1/webhook-endpoints")
            .header("x-request-id", REQUEST_ID)
            .header("x-forwarded-for", unique_xff())
            .header("authorization", format!("Bearer {secret}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await
}

/// How many live api_keys rows the user has.
async fn live_key_count(app: &TestApp, user_id: Uuid) -> i64 {
    sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM api_keys WHERE user_id = $1")
        .bind(user_id)
        .fetch_one(&app.pool)
        .await
        .expect("count keys")
}

/// Assert the problem body is the feature denial: 403,
/// `PF-3003`, and the detail carries the upgrade hint.
fn assert_feature_denial(res: &common::TestResponse) {
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.text());
    let body = res.json();
    assert_eq!(body["code"], "PF-3003", "{}", res.text());
    assert!(
        body["detail"]
            .as_str()
            .unwrap_or_default()
            .contains("upgrade"),
        "detail must carry the upgrade hint: {}",
        res.text()
    );
}

#[tokio::test]
async fn free_user_cannot_create_api_key() {
    let app = flags_app().await;
    let (id, token) = authed_user(&app, "flags-free-key").await;
    let res = create_key(&app, &token, json!({"name": "t", "scopes": ["jobs:read"]})).await;
    assert_feature_denial(&res);
    assert_eq!(
        live_key_count(&app, id).await,
        0,
        "a denied create mints no row"
    );
}

#[tokio::test]
async fn pro_user_can_create_api_key() {
    let app = flags_app().await;
    let (id, token) = authed_user(&app, "flags-pro-key").await;
    app.grant_plan(id, "pro").await;
    let res = create_key(&app, &token, json!({"name": "t", "scopes": ["jobs:read"]})).await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.text());
    assert!(
        res.json()["secret"]
            .as_str()
            .unwrap_or_default()
            .starts_with("pf_live_"),
        "the full secret is shown once: {}",
        res.text()
    );
    assert_eq!(live_key_count(&app, id).await, 1);
}

#[tokio::test]
async fn business_user_can_create_api_key() {
    let app = flags_app().await;
    let (id, token) = authed_user(&app, "flags-biz-key").await;
    app.grant_plan(id, "business").await;
    let res = create_key(&app, &token, json!({"name": "t", "scopes": ["jobs:read"]})).await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.text());
    assert_eq!(live_key_count(&app, id).await, 1);
}

#[tokio::test]
async fn free_user_key_create_validation_still_422() {
    let app = flags_app().await;
    let (_, token) = authed_user(&app, "flags-free-422").await;
    // An empty name is invalid — the gate runs after
    // validation, so even a Free user sees the 422, not a 403.
    let res = create_key(&app, &token, json!({"name": "", "scopes": ["jobs:read"]})).await;
    assert_eq!(
        res.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        res.text()
    );
    assert_eq!(res.json()["code"], "PF-2002", "{}", res.text());
}

#[tokio::test]
async fn free_user_cannot_create_webhook_endpoint() {
    let app = flags_app().await;
    let (_, token) = authed_user(&app, "flags-free-wh").await;
    let res = create_webhook_endpoint_jwt(&app, &token).await;
    assert_feature_denial(&res);
}

#[tokio::test]
async fn pro_user_reaches_webhook_endpoint_stub() {
    let app = flags_app().await;
    let (id, token) = authed_user(&app, "flags-pro-wh").await;
    app.grant_plan(id, "pro").await;
    let res = create_webhook_endpoint_jwt(&app, &token).await;
    assert_eq!(
        res.status,
        StatusCode::NOT_IMPLEMENTED,
        "an entitled caller reaches the stub: {}",
        res.text()
    );
    assert_eq!(res.json()["code"], "PF-5004", "{}", res.text());
}

#[tokio::test]
async fn key_caller_reaches_webhook_endpoint_stub() {
    let app = flags_app().await;
    let (id, token) = authed_user(&app, "flags-key-wh").await;
    app.grant_plan(id, "pro").await;
    let res = create_key(
        &app,
        &token,
        json!({"name": "t", "scopes": ["webhooks:manage"]}),
    )
    .await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.text());
    let secret = res.json()["secret"].as_str().unwrap().to_string();
    // A key has no user in the context, so the gate passes it
    // through: the stub's 501, not a 403 and not a success.
    let res = create_webhook_endpoint_key(&app, &secret).await;
    assert_eq!(
        res.status,
        StatusCode::NOT_IMPLEMENTED,
        "a key caller passes the gate to the stub: {}",
        res.text()
    );
    assert_eq!(res.json()["code"], "PF-5004", "{}", res.text());
}
