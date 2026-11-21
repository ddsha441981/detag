//! P1-051: quota middleware — the pre-check on job create.
//!
//! `POST /v1/jobs` gates on the plan's daily images /
//! video-seconds and the concurrency slot BEFORE the 501
//! stub: an over-quota request is 429 problem+json with the
//! FR-P-06 upgrade hint (`PF-3001` images, `PF-3002`
//! video-seconds, `PF-3008` concurrency — ADR-0026) and the
//! `X-Quota-*` headers; an inside-quota request reaches the
//! stub's 501 with the same headers. Since P1-094 the allow
//! path also needs the caller's attestation row for the
//! upload (403 `PF-4001` without one — see
//! `attestations.rs`); the allow-path tests attest first via
//! `app.seed_attestation`. The nine tests:
//!
//! - `image_quota_exceeded_is_429_pf3001_with_headers` —
//!   Free at 10/10 images, image upload → 429 `PF-3001`,
//!   problem+json, upgrade hint, headers `0` / `0`.
//! - `video_quota_exceeded_on_free_is_429_pf3002` — Free
//!   grants no video: a 90 s video AND an unknown-duration
//!   video are both 429 `PF-3002` (a video always needs at
//!   least one video-second — ASSUMPTION-017).
//! - `video_seconds_estimate_checked_against_remaining` —
//!   Pro at 3550/3600 video-seconds: a 90 s video is 429
//!   `PF-3002` (90 > 50 remaining), a 30 s video reaches
//!   the stub with headers `500` / `50`.
//! - `under_quota_reaches_stub_with_headers` — Free, no
//!   usage, image upload → 501 `PF-5004`, headers `10` /
//!   `0`.
//! - `concurrency_cap_is_429_pf3008` — Free (1 slot) with
//!   one `queued` job → 429 `PF-3008`; a second user whose
//!   only job is `held` reaches the stub (a held job holds
//!   no slot).
//! - `unknown_or_foreign_upload_is_404` — a random id and
//!   another user's upload are both 404 `PF-2010` (the
//!   P1-045 IDOR rule).
//! - `no_body_or_no_upload_id_still_reaches_stub` — an
//!   empty body (with or without a JSON content-type — the
//!   matrix probe sends the latter), and `{}` with no
//!   `upload_id`, are all the stub's 501 (the probe
//!   shapes, untouched) — and since P1-052 they carry the
//!   caller's quota headers too (every create response
//!   does; the no-headers carve-out is gone).
//! - `malformed_json_body_is_422_pf2002` — a non-empty body
//!   that is not valid JSON is 422 `PF-2002` (the manual
//!   parse keeps the `Json` extractor's problem shape).
//! - `quota_headers_agree_with_profile_snapshot` — the
//!   P1-005 note's requirement: the headers on the
//!   job-create response equal the `/v1/me` quota snapshot
//!   for the same user (same two sources, read once each).

mod common;

use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
};
use chrono::{DateTime, Utc};
use common::{TestApp, TestFixtures};
use detag_core::repos::{jobs, plans, uploads};
use serde_json::{json, Value};
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

/// Seed an upload for the user. `duration_s` is set with an
/// UPDATE — `NewUpload` carries no duration (it is filled
/// in on complete in Phase 3).
async fn seed_upload(app: &TestApp, user_id: Uuid, kind: &str, duration_s: Option<f32>) -> Uuid {
    let (mime, filename) = if kind == "image" {
        ("image/png", "a.png")
    } else {
        ("video/mp4", "a.mp4")
    };
    let upload = uploads::create(
        &app.pool,
        &uploads::NewUpload {
            user_id,
            kind,
            filename,
            bytes: 100,
            sha256: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            mime,
            storage_key: "test/quota-seed",
            parts: 1,
        },
    )
    .await
    .expect("seed upload");
    if let Some(duration) = duration_s {
        sqlx::query("UPDATE uploads SET duration_s = $2 WHERE id = $1")
            .bind(upload.id)
            .bind(duration)
            .execute(&app.pool)
            .await
            .expect("seed upload duration");
    }
    upload.id
}

/// Seed a job row in the given status (a running-slot
/// occupant for the concurrency tests).
async fn seed_job(app: &TestApp, user_id: Uuid, upload_id: Uuid, kind: &str, status: &str) {
    jobs::create(
        &app.pool,
        &jobs::NewJob {
            user_id,
            upload_id: Some(upload_id),
            kind,
            mode: "auto",
            status,
            lane: "standard",
            params: json!({}),
            idempotency_key: None,
            key_id: None,
        },
    )
    .await
    .expect("seed job");
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

/// POST /v1/jobs with an empty body AND a JSON
/// content-type (the api_key_matrix probe's shape — it
/// sets `content-type: application/json` on every POST,
/// even with an empty body). Before the bytes fix this
/// was 422: axum's `Option<Json<T>>` treats an empty body
/// as "no body" only without a JSON content-type.
async fn post_job_empty_json_body(app: &TestApp, token: &str) -> common::TestResponse {
    app.request(
        Request::builder()
            .method(Method::POST)
            .uri("/v1/jobs")
            .header("x-request-id", REQUEST_ID)
            .header("x-forwarded-for", unique_xff())
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "application/json")
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

/// Assert a problem+json body: media type, code, title, and
/// status. A 429 also carries the FR-P-06 upgrade hint in
/// the detail; other statuses do not.
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
    if status == StatusCode::TOO_MANY_REQUESTS {
        assert!(
            body["detail"].as_str().unwrap().contains("upgrade"),
            "429 detail carries the upgrade hint: {}",
            res.text()
        );
    }
}

#[tokio::test]
async fn image_quota_exceeded_is_429_pf3001_with_headers() {
    let app = quota_app().await;
    let (user, token) = authed_user(&app, "quota-img").await;
    seed_usage(&app, user, 10, 0).await;
    let upload = seed_upload(&app, user, "image", None).await;

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
async fn video_quota_exceeded_on_free_is_429_pf3002() {
    let app = quota_app().await;
    let (user, token) = authed_user(&app, "quota-vid").await;
    let known = seed_upload(&app, user, "video", Some(90.0)).await;
    let unknown = seed_upload(&app, user, "video", None).await;

    // Free grants 0 video-seconds: a 90 s video is denied.
    let res = post_job(&app, &token, json!({ "upload_id": known })).await;
    assert_problem(
        &res,
        StatusCode::TOO_MANY_REQUESTS,
        "PF-3002",
        "quota_video_seconds_exceeded",
    );
    assert_eq!(quota_headers(&res), ("10".to_string(), "0".to_string()));

    // An unknown duration is denied too — a video always
    // needs at least one video-second (ASSUMPTION-017).
    let res = post_job(&app, &token, json!({ "upload_id": unknown })).await;
    assert_problem(
        &res,
        StatusCode::TOO_MANY_REQUESTS,
        "PF-3002",
        "quota_video_seconds_exceeded",
    );
}

#[tokio::test]
async fn video_seconds_estimate_checked_against_remaining() {
    let app = quota_app().await;
    let (user, token) = authed_user(&app, "quota-pro").await;
    seed_subscription(&app, user, "pro", "active", Utc::now()).await;
    seed_usage(&app, user, 0, 3550).await;
    let long = seed_upload(&app, user, "video", Some(90.0)).await;
    let short = seed_upload(&app, user, "video", Some(30.0)).await;

    // 90 s needed, 50 s left: denied.
    let res = post_job(&app, &token, json!({ "upload_id": long })).await;
    assert_problem(
        &res,
        StatusCode::TOO_MANY_REQUESTS,
        "PF-3002",
        "quota_video_seconds_exceeded",
    );
    assert_eq!(quota_headers(&res), ("500".to_string(), "50".to_string()));

    // 30 s needed, 50 s left: reaches the stub, headers show
    // the same remaining (the check records nothing).
    app.seed_attestation(user, short).await;
    let res = post_job(&app, &token, json!({ "upload_id": short })).await;
    assert_problem(
        &res,
        StatusCode::NOT_IMPLEMENTED,
        "PF-5004",
        "not_implemented",
    );
    assert_eq!(quota_headers(&res), ("500".to_string(), "50".to_string()));
}

#[tokio::test]
async fn under_quota_reaches_stub_with_headers() {
    let app = quota_app().await;
    let (user, token) = authed_user(&app, "quota-ok").await;
    let upload = seed_upload(&app, user, "image", None).await;
    app.seed_attestation(user, upload).await;

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
async fn concurrency_cap_is_429_pf3008() {
    let app = quota_app().await;
    let (user, token) = authed_user(&app, "quota-conc").await;
    let upload = seed_upload(&app, user, "image", None).await;
    seed_job(&app, user, upload, "image", "queued").await;

    // Free allows 1 concurrent job; one is already queued.
    let res = post_job(&app, &token, json!({ "upload_id": upload })).await;
    assert_problem(
        &res,
        StatusCode::TOO_MANY_REQUESTS,
        "PF-3008",
        "quota_concurrency_exceeded",
    );
    assert_eq!(quota_headers(&res), ("10".to_string(), "0".to_string()));

    // A held job holds no slot: a second user with only a
    // held job reaches the stub.
    let (user_b, token_b) = authed_user(&app, "quota-held").await;
    let upload_b = seed_upload(&app, user_b, "image", None).await;
    seed_job(&app, user_b, upload_b, "image", "held").await;
    app.seed_attestation(user_b, upload_b).await;
    let res = post_job(&app, &token_b, json!({ "upload_id": upload_b })).await;
    assert_problem(
        &res,
        StatusCode::NOT_IMPLEMENTED,
        "PF-5004",
        "not_implemented",
    );
}

#[tokio::test]
async fn unknown_or_foreign_upload_is_404() {
    let app = quota_app().await;
    let (_user, token) = authed_user(&app, "quota-404").await;
    let (other, _) = authed_user(&app, "quota-404b").await;
    let foreign = seed_upload(&app, other, "image", None).await;

    let res = post_job(&app, &token, json!({ "upload_id": Uuid::now_v7() })).await;
    assert_problem(&res, StatusCode::NOT_FOUND, "PF-2010", "not_found");

    let res = post_job(&app, &token, json!({ "upload_id": foreign })).await;
    assert_problem(&res, StatusCode::NOT_FOUND, "PF-2010", "not_found");
}

#[tokio::test]
async fn no_body_or_no_upload_id_still_reaches_stub() {
    let app = quota_app().await;
    let (_user, token) = authed_user(&app, "quota-nobody").await;

    let res = post_job_no_body(&app, &token).await;
    assert_problem(
        &res,
        StatusCode::NOT_IMPLEMENTED,
        "PF-5004",
        "not_implemented",
    );
    // P1-052: every create response carries the headers —
    // Free, no usage: 10 images left, 0 video-seconds.
    assert_eq!(quota_headers(&res), ("10".to_string(), "0".to_string()));

    // The matrix probe's shape: empty body WITH a JSON
    // content-type — the same stub, not a 422 (the P1-051
    // verify went red on exactly this shape before the
    // handler read the body as raw bytes).
    let res = post_job_empty_json_body(&app, &token).await;
    assert_problem(
        &res,
        StatusCode::NOT_IMPLEMENTED,
        "PF-5004",
        "not_implemented",
    );
    assert_eq!(quota_headers(&res), ("10".to_string(), "0".to_string()));

    let res = post_job(&app, &token, json!({})).await;
    assert_problem(
        &res,
        StatusCode::NOT_IMPLEMENTED,
        "PF-5004",
        "not_implemented",
    );
    assert_eq!(quota_headers(&res), ("10".to_string(), "0".to_string()));
}

#[tokio::test]
async fn malformed_json_body_is_422_pf2002() {
    // A non-empty body that is not valid JSON is a client
    // error — the same 422 `PF-2002` the `Json` extractor
    // produced before the handler read the body as raw
    // bytes (the mapping is pinned here, not just the
    // status, so a 500 from the manual parse cannot hide).
    let app = quota_app().await;
    let (_user, token) = authed_user(&app, "quota-malformed").await;

    let res = app
        .request(
            Request::builder()
                .method(Method::POST)
                .uri("/v1/jobs")
                .header("x-request-id", REQUEST_ID)
                .header("x-forwarded-for", unique_xff())
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from("{not json"))
                .unwrap(),
        )
        .await;
    assert_problem(
        &res,
        StatusCode::UNPROCESSABLE_ENTITY,
        "PF-2002",
        "validation_failed",
    );
    assert!(res.headers.get("x-quota-images-remaining").is_none());
}

#[tokio::test]
async fn quota_headers_agree_with_profile_snapshot() {
    let app = quota_app().await;
    let (user, token) = authed_user(&app, "quota-agree").await;
    seed_usage(&app, user, 3, 0).await;
    let upload = seed_upload(&app, user, "image", None).await;
    app.seed_attestation(user, upload).await;

    // The profile snapshot: 10 - 3 = 7 images remaining.
    let res = app
        .request(
            Request::builder()
                .method(Method::GET)
                .uri("/v1/me")
                .header("x-request-id", REQUEST_ID)
                .header("x-forwarded-for", unique_xff())
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let quota = &res.json()["quota"];
    assert_eq!(quota["daily_images"]["remaining"], 7);
    assert_eq!(quota["daily_video_seconds"]["remaining"], 0);

    // The job-create headers agree with that snapshot.
    let res = post_job(&app, &token, json!({ "upload_id": upload })).await;
    assert_problem(
        &res,
        StatusCode::NOT_IMPLEMENTED,
        "PF-5004",
        "not_implemented",
    );
    assert_eq!(quota_headers(&res), ("7".to_string(), "0".to_string()));
}
