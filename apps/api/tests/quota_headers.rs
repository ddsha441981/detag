//! P1-052: quota headers everywhere — the `X-Quota-*`
//! headers on every job-creating route, not just the gated
//! `POST /v1/jobs` path P1-051 built.
//!
//! The routes, and what this suite pins:
//!
//! - `POST /v1/jobs` with no body, or no `upload_id`,
//!   reaches the stub — and now carries the headers too
//!   (P1-051 left them off: "there is nothing to check
//!   yet" — but a client creating a job always wants to
//!   see what is left; the numbers come from the same
//!   durable snapshot as the gate, so they agree with the
//!   gated path by construction).
//! - `POST /v1/jobs/{id}/retry` carries the CALLER's
//!   headers on its 501 stub. The handler reads NO job
//!   row — it stamps the caller's snapshot and stops —
//!   so an unknown id still 501s: the 404 ownership
//!   proof (P1-045) lands with the real retry handler
//!   in Phase 2, when the handler first reads the job
//!   (see ASSUMPTION-018). Nothing leaks meanwhile: no
//!   data, no state change, same 501 as before.
//! - `POST /v1/admin/jobs/{id}/reprocess` is NOT in this
//!   ticket: the quota subject for a reprocess is the job
//!   OWNER, not the admin, and the owner's remaining quota
//!   is already visible in the admin jobs browse view
//!   (P1-038 usage breakdown) — the owner's headers land
//!   when reprocess becomes a real handler
//!   (ASSUMPTION-018).
//! - The stub-path headers agree with the `/v1/me`
//!   snapshot for the same user (the gated path's
//!   agreement was proven in `quota_middleware.rs`;
//!   this suite proves the stub path too).
//! - An API key with `jobs:write` is a creator too: its
//!   no-body create carries the owner's headers (the
//!   extractor loads the key's owner into the context,
//!   P1-034 — no separate resolution needed).
//!
//! The UI contract these headers implement is documented
//! in `docs/06-DELIVERY/08-quota-ui-contract.md`
//! (FR-P-02: remaining quota surfaces in headers AND UI).

mod common;

use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
};
use chrono::{DateTime, Utc};
use common::{TestApp, TestFixtures};
use detag_core::repos::plans;
use serde_json::json;
use uuid::Uuid;

/// The request id: a valid UUID, echoed by the middleware.
const REQUEST_ID: &str = "123e4567-e89b-42d3-a456-426614174109";

/// The password every test user registers with.
const PASSWORD: &str = "correct horse battery staple";

/// Test settings: the register flow needs a ToS version; the
/// key bucket is generous so the suite asserts quota
/// outcomes, not rate limits.
async fn quota_app() -> TestApp {
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
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
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
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    res.json()["access_token"].as_str().unwrap().to_string()
}

/// Register + log in; return (user id, token).
async fn authed_user(app: &TestApp, prefix: &str) -> (Uuid, String) {
    let email = unique_email(prefix);
    let id = register_user(app, &email).await;
    let token = login_token(app, &email).await;
    (id, token)
}

/// Insert a subscription row directly — there is no billing
/// flow yet to create one through the API (the P1-049
/// helper).
async fn seed_subscription(
    app: &TestApp,
    user_id: Uuid,
    plan_code: &str,
    status: &str,
    created_at: DateTime<Utc>,
) {
    let plan = plans::by_code(&app.pool, plan_code)
        .await
        .expect("load plan")
        .expect("plan is seeded");
    sqlx::query(
        "INSERT INTO subscriptions (user_id, plan_id, provider, status, created_at) \
         VALUES ($1, $2, 'stripe', $3, $4)",
    )
    .bind(user_id)
    .bind(plan.id)
    .bind(status)
    .bind(created_at)
    .execute(&app.pool)
    .await
    .expect("seed subscription");
}

/// Seed today's usage row directly (the P1-005 helper — the
/// snapshot reads the DB row, so the seed is the DB row).
async fn seed_usage(app: &TestApp, user_id: Uuid, images: i32, video_seconds: i32) {
    let today = Utc::now().date_naive();
    sqlx::query(
        "INSERT INTO usage_counters (user_id, day, images_used, video_seconds_used) \
         VALUES ($1, $2, $3, $4)",
    )
    .bind(user_id)
    .bind(today)
    .bind(images)
    .bind(video_seconds)
    .execute(&app.pool)
    .await
    .expect("seed usage");
}

/// Mint a key for the JWT holder; return the full secret.
async fn create_key(app: &TestApp, token: &str, scopes: &[&str]) -> String {
    let body = json!({ "name": "headers", "scopes": scopes });
    let res = app
        .request(
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
        .await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.text());
    res.json()["secret"].as_str().unwrap().to_string()
}

/// POST /v1/jobs with an empty body as the user (the
/// route-table probe's shape).
async fn post_job_no_body(app: &TestApp, token: &str) -> common::TestResponse {
    app.request(
        Request::builder()
            .method(Method::POST)
            .uri("/v1/jobs")
            .header("x-request-id", REQUEST_ID)
            .header("x-forwarded-for", unique_xff())
            .header("authorization", format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await
}

/// POST /v1/jobs with `{}` (no `upload_id`) as the user.
async fn post_job_empty_object(app: &TestApp, token: &str) -> common::TestResponse {
    app.request(
        Request::builder()
            .method(Method::POST)
            .uri("/v1/jobs")
            .header("x-request-id", REQUEST_ID)
            .header("x-forwarded-for", unique_xff())
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "application/json")
            .body(Body::from("{}"))
            .unwrap(),
    )
    .await
}

/// POST /v1/jobs with an empty body as an API-key caller.
async fn post_job_no_body_with_key(app: &TestApp, secret: &str) -> common::TestResponse {
    app.request(
        Request::builder()
            .method(Method::POST)
            .uri("/v1/jobs")
            .header("x-request-id", REQUEST_ID)
            .header("x-forwarded-for", unique_xff())
            .header("authorization", format!("Bearer {secret}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await
}

/// POST /v1/jobs/{id}/retry as the user.
async fn post_retry(app: &TestApp, token: &str, job_id: Uuid) -> common::TestResponse {
    app.request(
        Request::builder()
            .method(Method::POST)
            .uri(format!("/v1/jobs/{job_id}/retry"))
            .header("x-request-id", REQUEST_ID)
            .header("x-forwarded-for", unique_xff())
            .header("authorization", format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await
}

/// GET /v1/me as the user.
async fn get_me(app: &TestApp, token: &str) -> common::TestResponse {
    app.request(
        Request::builder()
            .method(Method::GET)
            .uri("/v1/me")
            .header("x-request-id", REQUEST_ID)
            .header("x-forwarded-for", unique_xff())
            .header("authorization", format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await
}

/// The quota headers on a response, as (images, video-seconds).
fn quota_headers(res: &common::TestResponse) -> (String, String) {
    let images = res
        .headers
        .get("x-quota-images-remaining")
        .expect("images header present")
        .to_str()
        .unwrap()
        .to_string();
    let video = res
        .headers
        .get("x-quota-video-seconds-remaining")
        .expect("video-seconds header present")
        .to_str()
        .unwrap()
        .to_string();
    (images, video)
}

#[tokio::test]
async fn create_no_body_carries_quota_headers() {
    let app = quota_app().await;
    let (_id, token) = authed_user(&app, "hdr-nobody").await;
    let res = post_job_no_body(&app, &token).await;
    assert_eq!(res.status, StatusCode::NOT_IMPLEMENTED, "{}", res.text());
    // Free, no usage: 10 images left, 0 video-seconds.
    assert_eq!(quota_headers(&res), ("10".to_string(), "0".to_string()));
}

#[tokio::test]
async fn create_no_upload_id_carries_quota_headers() {
    let app = quota_app().await;
    let (_id, token) = authed_user(&app, "hdr-noupload").await;
    let res = post_job_empty_object(&app, &token).await;
    assert_eq!(res.status, StatusCode::NOT_IMPLEMENTED, "{}", res.text());
    assert_eq!(quota_headers(&res), ("10".to_string(), "0".to_string()));
}

#[tokio::test]
async fn create_stub_headers_follow_seeded_usage() {
    let app = quota_app().await;
    let (id, token) = authed_user(&app, "hdr-seeded").await;
    seed_usage(&app, id, 3, 0).await;
    let res = post_job_no_body(&app, &token).await;
    assert_eq!(res.status, StatusCode::NOT_IMPLEMENTED, "{}", res.text());
    // 10 - 3 images used, video untouched.
    assert_eq!(quota_headers(&res), ("7".to_string(), "0".to_string()));
}

#[tokio::test]
async fn retry_carries_quota_headers() {
    let app = quota_app().await;
    let (_id, token) = authed_user(&app, "hdr-retry").await;
    // No job row is read (ASSUMPTION-018): an unknown id
    // still reaches the stub — the 404 lands with the
    // real retry handler in Phase 2.
    let res = post_retry(&app, &token, Uuid::now_v7()).await;
    assert_eq!(res.status, StatusCode::NOT_IMPLEMENTED, "{}", res.text());
    assert_eq!(quota_headers(&res), ("10".to_string(), "0".to_string()));
}

#[tokio::test]
async fn retry_headers_follow_seeded_usage() {
    let app = quota_app().await;
    let (id, token) = authed_user(&app, "hdr-retry-pro").await;
    seed_subscription(&app, id, "pro", "active", Utc::now()).await;
    seed_usage(&app, id, 100, 0).await;
    let res = post_retry(&app, &token, Uuid::now_v7()).await;
    assert_eq!(res.status, StatusCode::NOT_IMPLEMENTED, "{}", res.text());
    // Pro: 500 images, 3600 video-seconds; 100 used.
    assert_eq!(quota_headers(&res), ("400".to_string(), "3600".to_string()));
}

#[tokio::test]
async fn retry_anonymous_is_401() {
    let app = quota_app().await;
    let res = app
        .request(
            Request::builder()
                .method(Method::POST)
                .uri(format!("/v1/jobs/{}/retry", Uuid::now_v7()))
                .header("x-request-id", REQUEST_ID)
                .header("x-forwarded-for", unique_xff())
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-1001");
}

#[tokio::test]
async fn stub_headers_agree_with_profile_snapshot() {
    let app = quota_app().await;
    let (id, token) = authed_user(&app, "hdr-agree").await;
    seed_usage(&app, id, 3, 0).await;
    let me = get_me(&app, &token).await;
    assert_eq!(me.status, StatusCode::OK, "{}", me.text());
    let quota = &me.json()["quota"];
    assert_eq!(quota["daily_images"]["remaining"], 7);
    assert_eq!(quota["daily_video_seconds"]["remaining"], 0);
    // The stub path's headers read the same snapshot.
    let res = post_job_no_body(&app, &token).await;
    assert_eq!(res.status, StatusCode::NOT_IMPLEMENTED, "{}", res.text());
    assert_eq!(quota_headers(&res), ("7".to_string(), "0".to_string()));
}

#[tokio::test]
async fn key_caller_create_carries_headers() {
    let app = quota_app().await;
    let (id, token) = authed_user(&app, "hdr-key").await;
    // P1-055: key creation is plan-gated — the user needs a
    // Pro grant to mint the key, so the headers now read the
    // Pro limits (500 images / 3600 video-seconds) instead of
    // the Free ones (10 / 0). The test still proves the point:
    // a key caller's create carries the headers.
    app.grant_plan(id, "pro").await;
    let secret = create_key(&app, &token, &["jobs:write"]).await;
    let res = post_job_no_body_with_key(&app, &secret).await;
    assert_eq!(res.status, StatusCode::NOT_IMPLEMENTED, "{}", res.text());
    assert_eq!(quota_headers(&res), ("500".to_string(), "3600".to_string()));
}
