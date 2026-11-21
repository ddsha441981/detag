//! P1-057: the entitlement suite — Free vs Pro, end to end
//! through the HTTP layer (FR-P-01 / FR-P-02 / FR-P-06 /
//! FR-P-07).
//!
//! The quota middleware (P1-051) and the feature gate
//! (P1-055) prove individual denials; this suite proves the
//! whole plan story holds together — the same user, the same
//! endpoints, two different plans:
//!
//! - `free_image_limit_trips_at_ten` — Free's 10-images-a-day
//!   limit trips at exactly 10: 9/10 reaches the stub, 10/10
//!   is 429 `PF-3001` with the upgrade hint.
//! - `pro_image_limit_trips_at_five_hundred` — the same
//!   boundary on Pro: 499/500 reaches the stub, 500/500 is
//!   429 `PF-3001`; the headers carry Pro's remaining
//!   (0 images / 3600 video-seconds).
//! - `utc_midnight_resets_the_free_limit` — the UTC reset
//!   through the gate AND the profile, on a frozen clock: a
//!   Free user at 10/10 on the 15th is denied at 23:59, and
//!   two minutes past midnight the same user is allowed
//!   again with a fresh 10 — while the 15th's row is
//!   untouched (the reset is a new day, not a decrement).
//!   This is the test that forced ADR-0027: the snapshot
//!   used to read `Utc::now()` directly, so a frozen clock
//!   could not move its day.
//! - `pro_concurrency_is_three_slots` — Pro's 3-slot
//!   boundary: 2 queued jobs reach the stub, the 3rd queued
//!   job makes the next create 429 `PF-3008` (Free's 1-slot
//!   boundary is already proven in `quota_middleware.rs`).
//! - `plan_ttl_and_limits_differ_by_plan` — the three plans
//!   differ in exactly the FR-P-01 values, read through the
//!   app's own catalog (the same catalog the middleware
//!   reads): result TTL 6 / 24 / 72 h, concurrency 1 / 3 /
//!   10, daily images 10 / 500 / 5000, video-seconds 0 /
//!   3600 / 18000, max bytes 25 MiB / 1 GiB / 4 GiB — plus
//!   the usage hash's 48 h Redis TTL after a meter record.
//!   Result TTL has no enforcement surface yet (no real
//!   jobs/results exist) — the catalog value is the proof
//!   until retention lands with the Phase 2 intake.
//! - `reconciliation_catches_drift_without_changing_enforcement`
//!   — the meter records 3 images, Redis is corrupted
//!   behind its back (99), and the gate STILL enforces the
//!   DB truth (the create reaches the stub, headers 7/0);
//!   `reconcile_day` then repairs Redis to the DB values
//!   (corrected, `redis_before` 99) and a second reconcile
//!   is a no-op. The meter-level halves of this story
//!   (UTC boundary, reconcile repair, concurrent records)
//!   are already proven in `crates/core/tests/usage_live.rs`;
//!   this test proves the HTTP layer never sees the drift.

mod common;

use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
};
use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use common::{TestApp, TestFixtures};
use deadpool_redis::redis::cmd;
use detag_core::clock::{Clock, ManualClock};
use detag_core::plans::Entitlements;
use detag_core::redis::keys::usage_key;
use detag_core::redis::{create_pool, RedisPool, RedisPoolConfig};
use detag_core::repos::{jobs, uploads, usage_counters};
use detag_core::usage::UsageMeter;
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
async fn entitlement_app() -> TestApp {
    let fixtures = TestFixtures::from_env().await;
    let mut settings = fixtures.settings();
    settings.disposable_email_domains = "mailinator.com".to_string();
    settings.tos_current_version = "v1".to_string();
    settings.api_key_rate_limit_burst = 1000;
    settings.api_key_rate_limit_refill_per_min = 1000;
    TestApp::spawn_with_fixtures_and_settings(fixtures, settings).await
}

/// App on a frozen clock + the Redis URL, so the test can
/// build its own [`UsageMeter`] on the same services and the
/// same clock. Returns (app, clock, redis_url).
async fn entitlement_app_with_clock(
    frozen_at: DateTime<Utc>,
) -> (TestApp, Arc<ManualClock>, String) {
    let fixtures = TestFixtures::from_env().await;
    let redis_url = fixtures.redis_url.clone();
    let mut settings = fixtures.settings();
    settings.disposable_email_domains = "mailinator.com".to_string();
    settings.tos_current_version = "v1".to_string();
    settings.api_key_rate_limit_burst = 1000;
    settings.api_key_rate_limit_refill_per_min = 1000;
    let clock = Arc::new(ManualClock::new(frozen_at));
    let app = TestApp::spawn_with_fixtures_settings_and_clock(
        fixtures,
        settings,
        Arc::clone(&clock) as Arc<dyn Clock>,
    )
    .await;
    (app, clock, redis_url)
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

/// Seed a usage row for the user on `day` directly (the
/// P1-005 helper, day-parameterized — a frozen-clock test
/// seeds the frozen day, not the wall-clock day).
async fn seed_usage_on_day(
    app: &TestApp,
    user_id: Uuid,
    day: NaiveDate,
    images: i32,
    video_seconds: i32,
) {
    sqlx::query(
        "INSERT INTO usage_counters (user_id, day, images_used, video_seconds_used) \
         VALUES ($1, $2, $3, $4)",
    )
    .bind(user_id)
    .bind(day)
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
            storage_key: "test/entitlement-seed",
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

/// A Redis pool on the given URL (the meter / corruption
/// helpers below).
fn redis_pool(redis_url: &str) -> RedisPool {
    create_pool(redis_url, &RedisPoolConfig::default()).expect("pool builds from the test URL")
}

/// The Redis hash for a user-day, as (images, video_seconds)
/// — absent fields read as 0.
async fn redis_totals(pool: &RedisPool, user_id: Uuid, day: NaiveDate) -> (i64, i64) {
    let mut conn = pool.get().await.expect("checkout");
    let got: (Option<i64>, Option<i64>) = cmd("HMGET")
        .arg(usage_key(&user_id, day))
        .arg("images")
        .arg("video_seconds")
        .query_async(&mut conn)
        .await
        .expect("HMGET the day hash");
    (got.0.unwrap_or(0), got.1.unwrap_or(0))
}

async fn delete_usage_key(pool: &RedisPool, user_id: Uuid, day: NaiveDate) {
    let mut conn = pool.get().await.expect("checkout");
    let _: () = cmd("DEL")
        .arg(usage_key(&user_id, day))
        .query_async(&mut conn)
        .await
        .expect("DEL the day hash");
}

#[tokio::test]
async fn free_image_limit_trips_at_ten() {
    let app = entitlement_app().await;
    let today = Utc::now().date_naive();

    // 9/10 images used: one image left — the create reaches
    // the stub with 1 remaining.
    let (user, token) = authed_user(&app, "ent-free-9").await;
    seed_usage_on_day(&app, user, today, 9, 0).await;
    let upload = seed_upload(&app, user, "image", None).await;
    app.seed_attestation(user, upload).await;
    let res = post_job(&app, &token, json!({ "upload_id": upload })).await;
    assert_problem(
        &res,
        StatusCode::NOT_IMPLEMENTED,
        "PF-5004",
        "not_implemented",
    );
    assert_eq!(quota_headers(&res), ("1".to_string(), "0".to_string()));

    // 10/10 images used: the limit trips — 429 PF-3001,
    // headers 0/0, upgrade hint in the detail.
    let (user, token) = authed_user(&app, "ent-free-10").await;
    seed_usage_on_day(&app, user, today, 10, 0).await;
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
async fn pro_image_limit_trips_at_five_hundred() {
    let app = entitlement_app().await;
    let today = Utc::now().date_naive();

    // 499/500 images used: one image left — the create
    // reaches the stub; the headers carry Pro's remaining
    // (1 image / 3600 video-seconds).
    let (user, token) = authed_user(&app, "ent-pro-499").await;
    app.grant_plan(user, "pro").await;
    seed_usage_on_day(&app, user, today, 499, 0).await;
    let upload = seed_upload(&app, user, "image", None).await;
    app.seed_attestation(user, upload).await;
    let res = post_job(&app, &token, json!({ "upload_id": upload })).await;
    assert_problem(
        &res,
        StatusCode::NOT_IMPLEMENTED,
        "PF-5004",
        "not_implemented",
    );
    assert_eq!(quota_headers(&res), ("1".to_string(), "3600".to_string()));

    // 500/500 images used: the limit trips — 429 PF-3001,
    // headers 0/3600.
    let (user, token) = authed_user(&app, "ent-pro-500").await;
    app.grant_plan(user, "pro").await;
    seed_usage_on_day(&app, user, today, 500, 0).await;
    let upload = seed_upload(&app, user, "image", None).await;
    let res = post_job(&app, &token, json!({ "upload_id": upload })).await;
    assert_problem(
        &res,
        StatusCode::TOO_MANY_REQUESTS,
        "PF-3001",
        "quota_images_exceeded",
    );
    assert_eq!(quota_headers(&res), ("0".to_string(), "3600".to_string()));
}

/// The UTC reset through the gate AND the profile, on a
/// frozen clock (ADR-0027): a Free user at 10/10 on the
/// 15th is denied at 23:59, and two minutes past midnight
/// the same user is allowed again with a fresh 10 — while
/// the 15th's row is untouched.
#[tokio::test]
async fn utc_midnight_resets_the_free_limit() {
    let frozen = Utc.with_ymd_and_hms(2026, 11, 15, 23, 59, 0).unwrap();
    let (app, clock, _redis_url) = entitlement_app_with_clock(frozen).await;
    let day_15 = NaiveDate::from_ymd_opt(2026, 11, 15).unwrap();
    let day_16 = NaiveDate::from_ymd_opt(2026, 11, 16).unwrap();

    let (user, token) = authed_user(&app, "ent-utc").await;
    seed_usage_on_day(&app, user, day_15, 10, 0).await;
    let upload = seed_upload(&app, user, "image", None).await;
    // Attest once up front: the first post is denied on quota
    // anyway, and the next-day post (past midnight) needs the
    // row to reach the stub.
    app.seed_attestation(user, upload).await;

    // 23:59 on the 15th, 10/10 used: denied.
    let res = post_job(&app, &token, json!({ "upload_id": upload })).await;
    assert_problem(
        &res,
        StatusCode::TOO_MANY_REQUESTS,
        "PF-3001",
        "quota_images_exceeded",
    );
    assert_eq!(quota_headers(&res), ("0".to_string(), "0".to_string()));

    // Two minutes later — past UTC midnight — the same user
    // has a fresh day: the profile shows 10 remaining and
    // the create reaches the stub.
    clock.advance(chrono::Duration::minutes(2));
    let res = get_me(&app, &token).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let quota = &res.json()["quota"];
    assert_eq!(quota["daily_images"]["limit"], 10);
    assert_eq!(quota["daily_images"]["used"], 0);
    assert_eq!(quota["daily_images"]["remaining"], 10);

    let res = post_job(&app, &token, json!({ "upload_id": upload })).await;
    assert_problem(
        &res,
        StatusCode::NOT_IMPLEMENTED,
        "PF-5004",
        "not_implemented",
    );
    assert_eq!(quota_headers(&res), ("10".to_string(), "0".to_string()));

    // And the 15th's row is untouched by the 16th — the
    // reset is a new day, not a decrement.
    let old_row = usage_counters::for_user_day(&app.pool, user, day_15)
        .await
        .expect("read the 15th's row")
        .expect("the 15th's row exists");
    assert_eq!(old_row.images_used, 10);
    let new_row = usage_counters::for_user_day(&app.pool, user, day_16)
        .await
        .expect("read the 16th's row");
    assert!(new_row.is_none(), "no usage recorded on the 16th yet");
}

/// Pro's 3-slot boundary (Free's 1-slot boundary is already
/// proven in `quota_middleware.rs`): 2 queued jobs reach
/// the stub, the 3rd queued job makes the next create 429
/// `PF-3008`.
#[tokio::test]
async fn pro_concurrency_is_three_slots() {
    let app = entitlement_app().await;
    let (user, token) = authed_user(&app, "ent-pro-conc").await;
    app.grant_plan(user, "pro").await;
    let upload = seed_upload(&app, user, "image", None).await;
    seed_job(&app, user, upload, "image", "queued").await;
    seed_job(&app, user, upload, "image", "queued").await;
    app.seed_attestation(user, upload).await;

    // 2 of 3 slots held: the create reaches the stub.
    let res = post_job(&app, &token, json!({ "upload_id": upload })).await;
    assert_problem(
        &res,
        StatusCode::NOT_IMPLEMENTED,
        "PF-5004",
        "not_implemented",
    );
    assert_eq!(quota_headers(&res), ("500".to_string(), "3600".to_string()));

    // 3 of 3 slots held: the next create is denied.
    seed_job(&app, user, upload, "image", "queued").await;
    let res = post_job(&app, &token, json!({ "upload_id": upload })).await;
    assert_problem(
        &res,
        StatusCode::TOO_MANY_REQUESTS,
        "PF-3008",
        "quota_concurrency_exceeded",
    );
    assert_eq!(quota_headers(&res), ("500".to_string(), "3600".to_string()));
}

/// The three plans differ in exactly the FR-P-01 values,
/// read through the app's own catalog (the same catalog
/// the middleware reads) — result TTL 6 / 24 / 72 h,
/// concurrency 1 / 3 / 10, daily images 10 / 500 / 5000,
/// video-seconds 0 / 3600 / 18000, max bytes 25 MiB / 1 GiB
/// / 4 GiB — plus the usage hash's 48 h Redis TTL after a
/// meter record.
#[tokio::test]
async fn plan_ttl_and_limits_differ_by_plan() {
    let (app, clock, redis_url) =
        entitlement_app_with_clock(Utc.with_ymd_and_hms(2026, 11, 15, 12, 0, 0).unwrap()).await;
    let day = NaiveDate::from_ymd_opt(2026, 11, 15).unwrap();

    let free = app
        .state
        .plans
        .by_code("free")
        .await
        .expect("catalog reads")
        .expect("free is seeded");
    let pro = app
        .state
        .plans
        .by_code("pro")
        .await
        .expect("catalog reads")
        .expect("pro is seeded");
    let business = app
        .state
        .plans
        .by_code("business")
        .await
        .expect("catalog reads")
        .expect("business is seeded");

    let free_ent = Entitlements::from_value(&free.entitlements);
    let pro_ent = Entitlements::from_value(&pro.entitlements);
    let business_ent = Entitlements::from_value(&business.entitlements);

    // Result TTL: 6 h / 24 h / 72 h.
    assert_eq!(free_ent.result_ttl_hours, 6);
    assert_eq!(pro_ent.result_ttl_hours, 24);
    assert_eq!(business_ent.result_ttl_hours, 72);
    // Concurrency: 1 / 3 / 10 slots.
    assert_eq!(free_ent.concurrency, 1);
    assert_eq!(pro_ent.concurrency, 3);
    assert_eq!(business_ent.concurrency, 10);
    // Daily images: 10 / 500 / 5000.
    assert_eq!(free_ent.daily_images, 10);
    assert_eq!(pro_ent.daily_images, 500);
    assert_eq!(business_ent.daily_images, 5000);
    // Daily video-seconds: 0 / 3600 / 18000.
    assert_eq!(free_ent.daily_video_seconds, 0);
    assert_eq!(pro_ent.daily_video_seconds, 3600);
    assert_eq!(business_ent.daily_video_seconds, 18000);
    // Max bytes: 25 MiB / 1 GiB / 4 GiB.
    assert_eq!(free_ent.max_bytes, 25 * 1024 * 1024);
    assert_eq!(pro_ent.max_bytes, 1024 * 1024 * 1024);
    assert_eq!(business_ent.max_bytes, 4 * 1024 * 1024 * 1024);

    // The usage hash's TTL: 48 h from the record (the
    // meter's `TTL_USAGE_COUNTER`) — strictly between 47 h
    // and 48 h, so a missing or wrong TTL cannot pass.
    let redis = redis_pool(&redis_url);
    let (user, _) = authed_user(&app, "ent-ttl").await;
    let meter = UsageMeter::new(
        app.pool.clone(),
        redis.clone(),
        Arc::clone(&clock) as Arc<dyn Clock>,
    );
    meter.record(user, 1, 0).await.expect("record");
    assert_eq!(redis_totals(&redis, user, day).await, (1, 0));
    let mut conn = redis.get().await.expect("checkout");
    let ttl: i64 = cmd("TTL")
        .arg(usage_key(&user, day))
        .query_async(&mut conn)
        .await
        .expect("TTL the day hash");
    assert!(
        ttl > 47 * 3600 && ttl <= 48 * 3600,
        "usage hash TTL must be ~48 h, got {ttl} s"
    );
    drop(conn);

    delete_usage_key(&redis, user, day).await;
}

/// Reconciliation catches drift without changing
/// enforcement: the meter records 3 images, Redis is
/// corrupted behind its back (99), and the gate still
/// enforces the DB truth — the create reaches the stub
/// with 7 remaining. `reconcile_day` then repairs Redis to
/// the DB values, and a second reconcile is a no-op.
#[tokio::test]
async fn reconciliation_catches_drift_without_changing_enforcement() {
    let (app, clock, redis_url) =
        entitlement_app_with_clock(Utc.with_ymd_and_hms(2026, 11, 15, 12, 0, 0).unwrap()).await;
    let day = NaiveDate::from_ymd_opt(2026, 11, 15).unwrap();
    let redis = redis_pool(&redis_url);
    let (user, token) = authed_user(&app, "ent-reconcile").await;
    let meter = UsageMeter::new(
        app.pool.clone(),
        redis.clone(),
        Arc::clone(&clock) as Arc<dyn Clock>,
    );

    // 3 images recorded: Redis and the DB agree.
    let row = meter.record(user, 3, 0).await.expect("record");
    assert_eq!(row.images_used, 3);
    assert_eq!(redis_totals(&redis, user, day).await, (3, 0));

    // Corrupt Redis behind the meter's back (a lost DB
    // write, a crashed process, a manual edit).
    let mut conn = redis.get().await.expect("checkout");
    let _: () = cmd("HSET")
        .arg(usage_key(&user, day))
        .arg("images")
        .arg(99)
        .query_async(&mut conn)
        .await
        .expect("corrupt the hash");
    drop(conn);
    assert_eq!(redis_totals(&redis, user, day).await, (99, 0));

    // The gate still enforces the DB truth: 3 used, 7 left
    // — the drifted hash never leaks into a decision.
    let upload = seed_upload(&app, user, "image", None).await;
    app.seed_attestation(user, upload).await;
    let res = post_job(&app, &token, json!({ "upload_id": upload })).await;
    assert_problem(
        &res,
        StatusCode::NOT_IMPLEMENTED,
        "PF-5004",
        "not_implemented",
    );
    assert_eq!(quota_headers(&res), ("7".to_string(), "0".to_string()));

    // Reconcile: the DB wins, Redis is corrected.
    let outcome = meter.reconcile_day(user, day).await.expect("reconcile");
    assert!(outcome.corrected, "drift must be corrected");
    assert_eq!(outcome.db_images, 3);
    assert_eq!(outcome.db_video_seconds, 0);
    assert_eq!(outcome.redis_images_before, 99);
    assert_eq!(redis_totals(&redis, user, day).await, (3, 0));

    // A second reconcile is a no-op: the stores agree, and
    // the profile agrees with them (7 remaining).
    let outcome = meter
        .reconcile_day(user, day)
        .await
        .expect("reconcile again");
    assert!(!outcome.corrected, "no drift left to correct");
    let res = get_me(&app, &token).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert_eq!(res.json()["quota"]["daily_images"]["remaining"], 7);

    delete_usage_key(&redis, user, day).await;
}
