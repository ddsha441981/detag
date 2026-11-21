//! P1-094: ownership attestation capture + the job-create gate.
//!
//! The attestation service (`detag_core::attestation`) captures
//! one immutable row per upload (FR-C-01 / ADR-0012), and `POST
//! /v1/jobs` requires the caller's attestation row for the
//! upload before a job may be created from it. The four tests:
//!
//! - `job_without_attestation_rejected_pf4001` — the ticket's
//!   AC: an inside-quota create naming an upload with no
//!   attestation row is 403 `PF-4001` (problem+json, quota
//!   headers present) and creates no job row.
//! - `job_with_attestation_reaches_stub` — the same create
//!   after capturing an attestation reaches the stub's 501
//!   with the same headers (job creation itself is still the
//!   Phase 2 stub — the gate is the ticket, not the intake).
//! - `attestation_gate_runs_after_quota` — check order is
//!   quota first, then attestation: an over-quota user
//!   without an attestation is denied on quota (429
//!   `PF-3001`), not on the attestation.
//! - `api_key_client_attestation_satisfies_gate` — the gate
//!   takes either client: an attestation captured as
//!   `api_key` satisfies a web (JWT) create, and vice versa
//!   — the client is recorded evidence, not a gate input.

mod common;

use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
};
use common::{TestApp, TestFixtures};
use detag_core::attestation::{capture, Client, CURRENT_TEXT_VERSION};
use detag_core::repos::uploads;
use serde_json::{json, Value};
use uuid::Uuid;

/// The request id: a valid UUID, echoed by the middleware.
const REQUEST_ID: &str = "123e4567-e89b-42d3-a456-426614174109";

/// The password every test user registers with.
const PASSWORD: &str = "correct horse battery staple";

/// The client IP the tests attest from (TEST-NET-3).
const TEST_IP: &str = "203.0.113.7";

/// Test settings: the register flow needs a ToS version; the
/// key bucket is generous so the suite asserts attestation
/// outcomes, not rate limits.
async fn attest_app() -> TestApp {
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

/// Seed an upload for the user (the quota_middleware helper —
/// `duration_s` is set with an UPDATE: `NewUpload` carries no
/// duration, it is filled in on complete in Phase 3).
async fn seed_upload(app: &TestApp, user_id: Uuid) -> Uuid {
    uploads::create(
        &app.pool,
        &uploads::NewUpload {
            user_id,
            kind: "image",
            filename: "a.png",
            bytes: 100,
            sha256: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            mime: "image/png",
            storage_key: "test/attest-seed",
            parts: 1,
        },
    )
    .await
    .expect("seed upload")
    .id
}

/// Seed today's usage row directly (the P1-005 helper — the
/// snapshot reads the DB row, so the seed is the DB row).
async fn seed_usage(app: &TestApp, user_id: Uuid, images: i32, video_seconds: i32) {
    let today = chrono::Utc::now().date_naive();
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

/// Capture an attestation for the user's upload through the
/// service (upload-create is still the 501 stub, so the
/// tests capture directly — Phase 2's intake calls this
/// same function with the attestation from the create body).
async fn seed_attestation(app: &TestApp, user_id: Uuid, upload_id: Uuid, client: Client) {
    capture(
        &app.pool,
        user_id,
        upload_id,
        CURRENT_TEXT_VERSION,
        TEST_IP,
        client,
    )
    .await
    .expect("capture attestation");
}

/// POST /v1/jobs with a JSON body as the user.
async fn post_job(app: &TestApp, token: &str, body: Value) -> common::TestResponse {
    app.request(
        Request::builder()
            .method(Method::POST)
            .uri("/v1/jobs")
            .header("x-request-id", REQUEST_ID)
            .header("x-forwarded-for", unique_xff())
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
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

/// Assert a problem+json body: media type, code, title, and
/// status.
fn assert_problem(res: &common::TestResponse, status: StatusCode, code: &str, title: &str) {
    assert_eq!(res.status, status, "{}", res.text());
    let content_type = res
        .headers
        .get("content-type")
        .expect("content-type present")
        .to_str()
        .unwrap();
    assert_eq!(content_type, "application/problem+json");
    let body = res.json();
    assert_eq!(body["code"], code, "{}", res.text());
    assert_eq!(body["title"], title, "{}", res.text());
    assert_eq!(body["status"], status.as_u16(), "{}", res.text());
}

/// How many job rows the user has (the rejection must create
/// none).
async fn jobs_count(app: &TestApp, user_id: Uuid) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM jobs WHERE user_id = $1")
        .bind(user_id)
        .fetch_one(&app.pool)
        .await
        .expect("count jobs")
}

#[tokio::test]
async fn job_without_attestation_rejected_pf4001() {
    let app = attest_app().await;
    let (user, token) = authed_user(&app, "attest-none").await;
    let upload = seed_upload(&app, user).await;

    let res = post_job(&app, &token, json!({ "upload_id": upload })).await;
    assert_problem(
        &res,
        StatusCode::FORBIDDEN,
        "PF-4001",
        "attestation_missing",
    );
    // P1-052: every job-creating response carries the quota
    // headers — Free, no usage: 10 images left, 0 video-seconds.
    assert_eq!(quota_headers(&res), ("10".to_string(), "0".to_string()));
    assert_eq!(
        jobs_count(&app, user).await,
        0,
        "a rejected create creates no job row"
    );
}

#[tokio::test]
async fn job_with_attestation_reaches_stub() {
    let app = attest_app().await;
    let (user, token) = authed_user(&app, "attest-ok").await;
    let upload = seed_upload(&app, user).await;
    seed_attestation(&app, user, upload, Client::Web).await;

    let res = post_job(&app, &token, json!({ "upload_id": upload })).await;
    assert_problem(
        &res,
        StatusCode::NOT_IMPLEMENTED,
        "PF-5004",
        "not_implemented",
    );
    assert_eq!(quota_headers(&res), ("10".to_string(), "0".to_string()));
}

#[tokio::test]
async fn attestation_gate_runs_after_quota() {
    let app = attest_app().await;
    let (user, token) = authed_user(&app, "attest-quota").await;
    seed_usage(&app, user, 10, 0).await;
    let upload = seed_upload(&app, user).await;

    // Free at 10/10 images, no attestation: the quota denial
    // wins — the gate is never consulted.
    let res = post_job(&app, &token, json!({ "upload_id": upload })).await;
    assert_problem(
        &res,
        StatusCode::TOO_MANY_REQUESTS,
        "PF-3001",
        "quota_images_exceeded",
    );
    assert_eq!(quota_headers(&res), ("0".to_string(), "0".to_string()));
}

#[tokio::test]
async fn api_key_client_attestation_satisfies_gate() {
    let app = attest_app().await;
    let (user, token) = authed_user(&app, "attest-key").await;
    let upload = seed_upload(&app, user).await;
    seed_attestation(&app, user, upload, Client::ApiKey).await;

    // The gate takes either client — the client is recorded
    // evidence, not a gate input — so an api_key attestation
    // satisfies a web (JWT) create.
    let res = post_job(&app, &token, json!({ "upload_id": upload })).await;
    assert_problem(
        &res,
        StatusCode::NOT_IMPLEMENTED,
        "PF-5004",
        "not_implemented",
    );
}
