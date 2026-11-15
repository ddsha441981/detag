//! P1-054: overage policy — FR-P-06.
//!
//! An over-quota job create is a 429 that rejects, never a
//! silent queue: the denial carries the clear message (the
//! FR-P-06 upgrade hint in `detail`) PLUS the plan-matrix
//! link (`upgrade_url` = `{PF_PUBLIC_BASE_URL}/plans`), and
//! it leaves nothing behind — no job row, no usage recorded,
//! no slot acquired, nothing enqueued. The four tests:
//!
//! - `image_overage_denies_with_plan_link_and_no_side_effects` —
//!   Free at 10/10 images, image upload → 429 `PF-3001` with
//!   the link, headers `0` / `0`, and zero side effects.
//! - `video_overage_denies_with_plan_link_and_no_side_effects` —
//!   Free grants no video: a 30 s video → 429 `PF-3002` with
//!   the link, and zero side effects (no usage row at all).
//! - `concurrency_overage_denies_with_plan_link_and_no_side_effects` —
//!   Free (1 slot) with one `queued` job → 429 `PF-3008` with
//!   the link; the seeded job stays the only row, usage and
//!   slots untouched.
//! - `allowed_create_carries_no_plan_link` — Free, inside quota,
//!   image upload → the stub's 501 with the headers and NO
//!   `upgrade_url`: the link appears only on denials.
//!
//! The no-side-effects proof is the "no silent queueing" half
//! of the policy: a denial creates no `jobs` row, so the
//! Phase 2 intake — which can only enqueue a job row that
//! exists — can never queue the denied job later.

mod common;

use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
};
use chrono::Utc;
use common::{TestApp, TestFixtures};
use detag_core::{
    clock::SystemClock,
    redis::{create_pool, RedisPoolConfig},
    repos::{jobs, uploads},
    slots::SlotPool,
};
use serde_json::{json, Value};
use std::sync::Arc;
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

/// How many `jobs` rows the user has.
async fn jobs_count(app: &TestApp, user_id: Uuid) -> i64 {
    sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM jobs WHERE user_id = $1")
        .bind(user_id)
        .fetch_one(&app.pool)
        .await
        .expect("count jobs")
}

/// Today's usage row for the user, if one exists.
async fn usage_row(app: &TestApp, user_id: Uuid) -> Option<(i32, i32)> {
    let today = Utc::now().date_naive();
    sqlx::query_as::<_, (i32, i32)>(
        "SELECT images_used, video_seconds_used FROM usage_counters \
         WHERE user_id = $1 AND day = $2",
    )
    .bind(user_id)
    .bind(today)
    .fetch_optional(&app.pool)
    .await
    .expect("read usage")
}

/// How many concurrency slots the user holds in Redis
/// (P1-053 `SlotPool::held` over the app's own Redis).
async fn slots_held(app: &TestApp, user_id: Uuid) -> i64 {
    let pool = create_pool(&app.settings.redis_url, &RedisPoolConfig::default())
        .expect("slots pool builds");
    let slots = SlotPool::new(pool, Arc::new(SystemClock));
    slots.held(user_id).await.expect("slots held reads")
}

/// The plan-matrix link the denial must carry: the app's own
/// public base URL + `/plans` (the setting is the source, so
/// the test pins the derivation, not a hardcoded host).
fn expected_plans_url(app: &TestApp) -> String {
    format!(
        "{}/plans",
        app.settings.public_base_url.trim_end_matches('/')
    )
}

/// Assert the full overage-denial shape: 429 problem+json,
/// the PF code + title + detail (ending in the FR-P-06
/// upgrade hint), the plan-matrix link, and the `X-Quota-*`
/// headers.
fn assert_denial(
    app: &TestApp,
    res: &common::TestResponse,
    code: &str,
    title: &str,
    detail: &str,
    images_remaining: &str,
    video_remaining: &str,
) {
    assert_eq!(
        res.status,
        StatusCode::TOO_MANY_REQUESTS,
        "denial: {}",
        res.text()
    );
    let json = res.json();
    assert_eq!(json["code"], code);
    assert_eq!(json["title"], title);
    assert_eq!(json["detail"], detail);
    assert!(
        json["detail"]
            .as_str()
            .unwrap()
            .ends_with("upgrade your plan for more"),
        "detail must end with the upgrade hint: {}",
        json["detail"]
    );
    assert_eq!(
        json["upgrade_url"],
        expected_plans_url(app),
        "denial must link the plan matrix"
    );
    let images = res
        .headers
        .get("x-quota-images-remaining")
        .expect("images header present")
        .to_str()
        .expect("images header is ascii");
    let video = res
        .headers
        .get("x-quota-video-seconds-remaining")
        .expect("video header present")
        .to_str()
        .expect("video header is ascii");
    assert_eq!(images, images_remaining);
    assert_eq!(video, video_remaining);
}

#[tokio::test]
async fn image_overage_denies_with_plan_link_and_no_side_effects() {
    let app = quota_app().await;
    let (user_id, token) = authed_user(&app, "overage-img").await;
    seed_usage(&app, user_id, 10, 0).await;
    let upload_id = seed_upload(&app, user_id, "image", None).await;

    let res = post_job(&app, &token, json!({ "upload_id": upload_id })).await;
    assert_denial(
        &app,
        &res,
        "PF-3001",
        "quota_images_exceeded",
        "image quota exceeded; upgrade your plan for more",
        "0",
        "0",
    );

    // No silent queueing: nothing was created, recorded, or
    // acquired by the denied request.
    assert_eq!(jobs_count(&app, user_id).await, 0, "denial creates no job");
    assert_eq!(
        usage_row(&app, user_id).await,
        Some((10, 0)),
        "denial records no usage"
    );
    assert_eq!(
        slots_held(&app, user_id).await,
        0,
        "denial acquires no slot"
    );
}

#[tokio::test]
async fn video_overage_denies_with_plan_link_and_no_side_effects() {
    let app = quota_app().await;
    let (user_id, token) = authed_user(&app, "overage-vid").await;
    let upload_id = seed_upload(&app, user_id, "video", Some(30.0)).await;

    let res = post_job(&app, &token, json!({ "upload_id": upload_id })).await;
    assert_denial(
        &app,
        &res,
        "PF-3002",
        "quota_video_seconds_exceeded",
        "video-seconds quota exceeded; upgrade your plan for more",
        "10",
        "0",
    );

    assert_eq!(jobs_count(&app, user_id).await, 0, "denial creates no job");
    assert_eq!(
        usage_row(&app, user_id).await,
        None,
        "denial records no usage"
    );
    assert_eq!(
        slots_held(&app, user_id).await,
        0,
        "denial acquires no slot"
    );
}

#[tokio::test]
async fn concurrency_overage_denies_with_plan_link_and_no_side_effects() {
    let app = quota_app().await;
    let (user_id, token) = authed_user(&app, "overage-conc").await;
    let upload_id = seed_upload(&app, user_id, "image", None).await;
    seed_job(&app, user_id, upload_id, "image", "queued").await;

    let res = post_job(&app, &token, json!({ "upload_id": upload_id })).await;
    assert_denial(
        &app,
        &res,
        "PF-3008",
        "quota_concurrency_exceeded",
        "concurrency limit reached; upgrade your plan for more",
        "10",
        "0",
    );

    // The seeded job stays the only row; nothing new was
    // created, recorded, or acquired.
    assert_eq!(jobs_count(&app, user_id).await, 1, "denial creates no job");
    assert_eq!(
        usage_row(&app, user_id).await,
        None,
        "denial records no usage"
    );
    assert_eq!(
        slots_held(&app, user_id).await,
        0,
        "denial acquires no slot"
    );
}

#[tokio::test]
async fn allowed_create_carries_no_plan_link() {
    let app = quota_app().await;
    let (user_id, token) = authed_user(&app, "overage-ok").await;
    let upload_id = seed_upload(&app, user_id, "image", None).await;

    let res = post_job(&app, &token, json!({ "upload_id": upload_id })).await;
    assert_eq!(
        res.status,
        StatusCode::NOT_IMPLEMENTED,
        "stub: {}",
        res.text()
    );
    let json = res.json();
    assert_eq!(json["code"], "PF-5004");
    assert!(
        json.get("upgrade_url").is_none(),
        "only denials carry the plan link: {}",
        json
    );
    let images = res
        .headers
        .get("x-quota-images-remaining")
        .expect("images header present")
        .to_str()
        .expect("images header is ascii");
    assert_eq!(images, "10");
    assert_eq!(jobs_count(&app, user_id).await, 0, "stub creates no job");
    assert_eq!(slots_held(&app, user_id).await, 0, "stub acquires no slot");
}
