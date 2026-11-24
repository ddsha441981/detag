//! `POST /v1/admin/users/{id}/quota-grant` (P1-107, FR-C-02).
//!
//! An admin grants a user a temporary quota bump — extra daily
//! images and/or video-seconds for `ttl_hours` hours, audited
//! as `admin.users.quota_granted`. The quota snapshot adds the
//! unexpired grants to the plan's daily limits, so the quota
//! middleware honours the grant while it lives and forgets it
//! when it expires (the AC: expiry honoured). Grants stack.
//! The seven tests:
//!
//! - `quota_grant_guard` — anonymous is 401 `PF-1001`, a plain
//!   user and support are 403 `PF-1002`, an admin user's API
//!   key is 403 (no admin ever), an admin is 200.
//! - `quota_grant_shape_and_audit` — the grant shape (amounts,
//!   expiry = frozen now + ttl, reason verbatim, granted_by
//!   the admin) and exactly one `admin.users.quota_granted`
//!   audit row with the right actor, target and after.
//! - `quota_grant_honored_by_precheck` — a Free user at 10/10
//!   images is 429 `PF-3001`; a 5-image grant lets the same
//!   create reach the 501 stub with 5 images remaining.
//! - `quota_grant_video_seconds_unlocks_free_video` — a Free
//!   user (0 video-seconds) is 429 `PF-3002` on a 90 s video;
//!   a 300 s grant lets it reach the stub (the 90 s is the
//!   allow threshold, not a deduction — remaining stays 300).
//! - `quota_grant_expiry_honored` — the AC: a 1 h grant lets
//!   the create through; the frozen clock advanced 2 h past
//!   expiry, the same create is 429 `PF-3001` again.
//! - `quota_grant_stacking` — two grants (5 + 3 images) sum:
//!   the create reaches the stub with 8 images remaining.
//! - `quota_grant_validation` — no amount / zero amount /
//!   ttl 0 / ttl 169 / blank reason are each 422 and write no
//!   grant; an unknown user is 404 `PF-2010` and audits
//!   nothing.
//!
//! The app runs on the frozen clock (Thursday 2026-11-19,
//! 12:00 UTC) and usage is seeded for the FROZEN day — the
//! snapshot reads the clock's day, not the wall clock.

mod common;

use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
};
use chrono::{DateTime, Duration, NaiveDate, Utc};
use common::{TestApp, TestFixtures};
use detag_core::{
    clock::{Clock, ManualClock},
    repos::{audit_logs, uploads},
};
use serde_json::{json, Value};
use std::sync::Arc;
use uuid::Uuid;

/// The request id: a valid UUID, echoed by the middleware.
const REQUEST_ID: &str = "123e4567-e89b-42d3-a456-426614174109";

/// The password every test user registers with.
const PASSWORD: &str = "correct horse battery staple";

/// The frozen "now": Thursday 2026-11-19, 12:00 UTC.
fn frozen_now() -> DateTime<Utc> {
    DateTime::from_naive_utc_and_offset(
        NaiveDate::from_ymd_opt(2026, 11, 19)
            .unwrap()
            .and_hms_opt(12, 0, 0)
            .unwrap(),
        Utc,
    )
}

/// The frozen day: the day the snapshot reads.
fn frozen_day() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 11, 19).unwrap()
}

/// App on the frozen clock. No billing provider: the
/// endpoint never calls one.
async fn quota_grant_app() -> (TestApp, Arc<ManualClock>) {
    let fixtures = TestFixtures::from_env().await;
    let mut settings = fixtures.settings();
    settings.disposable_email_domains = "mailinator.com".to_string();
    settings.tos_current_version = "v1".to_string();
    settings.api_key_rate_limit_burst = 1000;
    settings.api_key_rate_limit_refill_per_min = 1000;
    let clock = Arc::new(ManualClock::new(frozen_now()));
    let app = TestApp::spawn_with_fixtures_settings_and_clock(
        fixtures,
        settings,
        Arc::clone(&clock) as Arc<dyn Clock>,
    )
    .await;
    (app, clock)
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

/// Register a user, set the role in the database, log in,
/// return (id, token). There is no self-serve way to become
/// an admin or support (the role is set by the owner out of
/// band).
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

/// Register + log in; return (user id, token).
async fn authed_user(app: &TestApp, prefix: &str) -> (Uuid, String) {
    let email = unique_email(prefix);
    let id = register_user(app, &email).await;
    let token = login_token(app, &email).await;
    (id, token)
}

/// A per-request XFF address: each request gets its own IP
/// bucket, so the tests never trip each other's rate limits
/// (the P1-003 lesson).
fn unique_xff() -> String {
    format!("203.0.113.{}", Uuid::now_v7())
}

fn unique_email(prefix: &str) -> String {
    format!("{prefix}-{}@example.com", Uuid::now_v7())
}

/// Mint an API key for the user (P1-043 helper): the key is
/// returned once, at creation.
async fn mint_api_key(app: &TestApp, token: &str) -> String {
    let res = app
        .request(
            Request::builder()
                .method(Method::POST)
                .uri("/v1/api-keys")
                .header("x-request-id", REQUEST_ID)
                .header("x-forwarded-for", unique_xff())
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&json!({ "name": "grant probe", "scopes": ["jobs:read", "jobs:write", "webhooks:manage"] })).unwrap(),
                ))
                .unwrap(),
        )
        .await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.text());
    res.json()["secret"].as_str().unwrap().to_string()
}

/// POST the quota-grant endpoint as the given caller.
async fn post_grant(
    app: &TestApp,
    target: Uuid,
    credential: Option<&str>,
    body: Value,
) -> common::TestResponse {
    let mut req = Request::builder()
        .method(Method::POST)
        .uri(format!("/v1/admin/users/{target}/quota-grant"))
        .header("x-request-id", REQUEST_ID)
        .header("x-forwarded-for", unique_xff())
        .header("content-type", "application/json");
    if let Some(token) = credential {
        req = req.header("authorization", format!("Bearer {token}"));
    }
    app.request(
        req.body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap(),
    )
    .await
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

/// Seed the frozen day's usage row directly (the P1-005
/// helper — the snapshot reads the DB row, so the seed is
/// the DB row). The day is the FROZEN day, not the wall
/// clock's: the snapshot reads the clock's day.
async fn seed_usage(app: &TestApp, user_id: Uuid, images: i32, video_seconds: i32) {
    sqlx::query(
        "INSERT INTO usage_counters (user_id, day, images_used, video_seconds_used) \
         VALUES ($1, $2, $3, $4)",
    )
    .bind(user_id)
    .bind(frozen_day())
    .bind(images)
    .bind(video_seconds)
    .execute(&app.pool)
    .await
    .expect("seed usage");
}

/// Seed an upload for the user (the quota_middleware
/// helper). `duration_s` is set with an UPDATE —
/// `NewUpload` carries no duration (it is filled in on
/// complete in Phase 3).
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
            storage_key: "test/quota-grant-seed",
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

/// The quota headers on a response, as (images,
/// video-seconds).
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

/// The problem code in a problem+json body.
fn problem_code(res: &common::TestResponse) -> String {
    res.json()["code"].as_str().unwrap_or("").to_string()
}

/// The `admin.users.quota_granted` audit rows for a user.
async fn grant_audit_rows(app: &TestApp, user_id: Uuid) -> Vec<audit_logs::AuditLog> {
    audit_logs::list_for_target(&app.pool, "user", &user_id.to_string())
        .await
        .expect("list audit rows")
        .into_iter()
        .filter(|row| row.action == "admin.users.quota_granted")
        .collect()
}

/// The unexpired grant totals for a user, at `now` — the
/// caller's clock instant, NOT the frozen start: the expiry
/// test advances the clock past the grant, so totals at the
/// frozen start would still count it.
async fn grant_totals(app: &TestApp, user_id: Uuid, now: DateTime<Utc>) -> (i64, i64) {
    detag_core::repos::quota_grants::unexpired_totals(&app.pool, user_id, now)
        .await
        .expect("grant totals")
}

#[tokio::test]
async fn quota_grant_guard() {
    let (app, _) = quota_grant_app().await;
    let (target, _) = authed_user(&app, "grant-target").await;
    let (user, user_token) = authed_user(&app, "grant-user").await;
    let _ = user;
    let (_, support_token) = make_role(&app, &unique_email("grant-support"), "support").await;
    let (admin_id, admin_token) = make_role(&app, &unique_email("grant-admin"), "admin").await;
    // P1-055: key creation is plan-gated — the admin needs a
    // Pro grant before it can mint the probe key (keys are a
    // Pro/Business feature, FR-P-01; the admin_guard
    // precedent).
    app.grant_plan(admin_id, "pro").await;
    let admin_key = mint_api_key(&app, &admin_token).await;
    let body = json!({ "images": 5, "ttl_hours": 24, "reason": "guard probe" });

    // Anonymous: 401 PF-1001.
    let res = post_grant(&app, target, None, body.clone()).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());
    assert_eq!(problem_code(&res), "PF-1001");

    // Plain user: 403 PF-1002.
    let res = post_grant(&app, target, Some(&user_token), body.clone()).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.text());
    assert_eq!(problem_code(&res), "PF-1002");

    // Support: 403 PF-1002 — the grant is an admin-only
    // write (the P1-042 matrix).
    let res = post_grant(&app, target, Some(&support_token), body.clone()).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.text());
    assert_eq!(problem_code(&res), "PF-1002");

    // An admin user's own API key: 403 PF-1002 (no admin
    // ever).
    let res = post_grant(&app, target, Some(&admin_key), body.clone()).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.text());
    assert_eq!(problem_code(&res), "PF-1002");

    // Admin: 200, and the grant is the one returned.
    let res = post_grant(&app, target, Some(&admin_token), body).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let grant = res.json();
    assert_eq!(grant["user_id"].as_str().unwrap(), target.to_string());
    assert_eq!(grant["images"].as_i64().unwrap(), 5);
    assert!(grant["video_seconds"].is_null());
}

#[tokio::test]
async fn quota_grant_shape_and_audit() {
    let (app, _) = quota_grant_app().await;
    let (target, _) = authed_user(&app, "grant-shape").await;
    let (admin_id, admin_token) = make_role(&app, &unique_email("grant-admin"), "admin").await;
    let before = grant_audit_rows(&app, target).await.len();

    let res = post_grant(
        &app,
        target,
        Some(&admin_token),
        json!({ "images": 5, "video_seconds": 300, "ttl_hours": 48, "reason": "goodwill" }),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let grant = res.json();
    assert_eq!(grant["user_id"].as_str().unwrap(), target.to_string());
    assert_eq!(grant["images"].as_i64().unwrap(), 5);
    assert_eq!(grant["video_seconds"].as_i64().unwrap(), 300);
    // Expiry is frozen now + ttl (48 h → Saturday 2026-11-21
    // 12:00 UTC).
    assert_eq!(
        grant["expires_at"].as_str().unwrap(),
        "2026-11-21T12:00:00Z"
    );
    assert_eq!(grant["reason"].as_str().unwrap(), "goodwill");
    assert_eq!(grant["granted_by"].as_str().unwrap(), admin_id.to_string());
    assert!(grant["id"].as_str().unwrap().parse::<Uuid>().is_ok());
    assert!(grant["created_at"].as_str().is_some());

    // Exactly one new audit row — actor the admin, target
    // the user, after carries the amounts + expiry + grant
    // id, reason verbatim.
    let rows = grant_audit_rows(&app, target).await;
    assert_eq!(rows.len(), before + 1, "one audit row per grant");
    let row = &rows[rows.len() - 1];
    assert_eq!(row.actor_user_id, Some(admin_id), "actor is the admin");
    assert_eq!(row.target_type, "user");
    assert_eq!(row.target_id.as_deref(), Some(target.to_string().as_str()));
    assert_eq!(row.after.as_ref().unwrap()["images"], 5);
    assert_eq!(row.after.as_ref().unwrap()["video_seconds"], 300);
    assert_eq!(
        row.after.as_ref().unwrap()["expires_at"].as_str().unwrap(),
        "2026-11-21T12:00:00Z"
    );
    assert_eq!(
        row.after.as_ref().unwrap()["grant_id"].as_str().unwrap(),
        grant["id"].as_str().unwrap()
    );
    assert_eq!(row.reason.as_deref(), Some("goodwill"));
}

#[tokio::test]
async fn quota_grant_honored_by_precheck() {
    let (app, _) = quota_grant_app().await;
    let (user, token) = authed_user(&app, "grant-honor").await;
    let (admin_id, admin_token) = make_role(&app, &unique_email("grant-admin"), "admin").await;
    let _ = admin_id;
    // Free at 10/10 images: the create is 429 PF-3001.
    seed_usage(&app, user, 10, 0).await;
    let upload = seed_upload(&app, user, "image", None).await;
    let res = post_job(&app, &token, json!({ "upload_id": upload })).await;
    assert_eq!(res.status, StatusCode::TOO_MANY_REQUESTS, "{}", res.text());
    assert_eq!(problem_code(&res), "PF-3001");
    assert_eq!(quota_headers(&res), ("0".to_string(), "0".to_string()));

    // A 5-image grant: the same create reaches the 501 stub
    // with 5 images remaining.
    let res = post_grant(
        &app,
        user,
        Some(&admin_token),
        json!({ "images": 5, "ttl_hours": 24, "reason": "honor probe" }),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    app.seed_attestation(user, upload).await;
    let res = post_job(&app, &token, json!({ "upload_id": upload })).await;
    assert_eq!(res.status, StatusCode::NOT_IMPLEMENTED, "{}", res.text());
    assert_eq!(problem_code(&res), "PF-5004");
    assert_eq!(quota_headers(&res), ("5".to_string(), "0".to_string()));
}

#[tokio::test]
async fn quota_grant_video_seconds_unlocks_free_video() {
    let (app, _) = quota_grant_app().await;
    let (user, token) = authed_user(&app, "grant-video").await;
    let (admin_id, admin_token) = make_role(&app, &unique_email("grant-admin"), "admin").await;
    let _ = admin_id;
    // Free grants 0 video-seconds: a 90 s video is 429
    // PF-3002.
    let upload = seed_upload(&app, user, "video", Some(90.0)).await;
    let res = post_job(&app, &token, json!({ "upload_id": upload })).await;
    assert_eq!(res.status, StatusCode::TOO_MANY_REQUESTS, "{}", res.text());
    assert_eq!(problem_code(&res), "PF-3002");
    assert_eq!(quota_headers(&res), ("10".to_string(), "0".to_string()));

    // A 300 s grant: the same create reaches the stub with
    // 300 s remaining — the 90 s is the ALLOW threshold, not
    // a deduction: the pre-check records nothing (the Phase 2
    // intake owns recording), so the snapshot still reads
    // 300 − 0, not 300 − 90 (ASSUMPTION-068 honesty).
    let res = post_grant(
        &app,
        user,
        Some(&admin_token),
        json!({ "video_seconds": 300, "ttl_hours": 24, "reason": "video probe" }),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    app.seed_attestation(user, upload).await;
    let res = post_job(&app, &token, json!({ "upload_id": upload })).await;
    assert_eq!(res.status, StatusCode::NOT_IMPLEMENTED, "{}", res.text());
    assert_eq!(problem_code(&res), "PF-5004");
    assert_eq!(quota_headers(&res), ("10".to_string(), "300".to_string()));
}

#[tokio::test]
async fn quota_grant_expiry_honored() {
    let (app, clock) = quota_grant_app().await;
    // The email is kept (not `authed_user`) so the test can
    // re-login after the clock advance: the access token
    // lives 15 minutes, and the advance is 2 h (the
    // api_key_lifecycle re-login precedent).
    let email = unique_email("grant-expiry");
    let user = register_user(&app, &email).await;
    let token = login_token(&app, &email).await;
    let (admin_id, admin_token) = make_role(&app, &unique_email("grant-admin"), "admin").await;
    let _ = admin_id;
    seed_usage(&app, user, 10, 0).await;

    // A 1 h grant: the create reaches the stub.
    let res = post_grant(
        &app,
        user,
        Some(&admin_token),
        json!({ "images": 5, "ttl_hours": 1, "reason": "expiry probe" }),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let upload = seed_upload(&app, user, "image", None).await;
    app.seed_attestation(user, upload).await;
    let res = post_job(&app, &token, json!({ "upload_id": upload })).await;
    assert_eq!(res.status, StatusCode::NOT_IMPLEMENTED, "{}", res.text());
    assert_eq!(quota_headers(&res), ("5".to_string(), "0".to_string()));

    // The clock 2 h past the grant's expiry: the grant
    // contributes nothing, and the same create is 429 again
    // (the AC: expiry honoured). Re-login first — the first
    // token died with the 2 h advance (15-minute life).
    clock.advance(Duration::hours(2));
    assert_eq!(grant_totals(&app, user, clock.now()).await, (0, 0));
    let token = login_token(&app, &email).await;
    let upload = seed_upload(&app, user, "image", None).await;
    let res = post_job(&app, &token, json!({ "upload_id": upload })).await;
    assert_eq!(res.status, StatusCode::TOO_MANY_REQUESTS, "{}", res.text());
    assert_eq!(problem_code(&res), "PF-3001");
    assert_eq!(quota_headers(&res), ("0".to_string(), "0".to_string()));
}

#[tokio::test]
async fn quota_grant_stacking() {
    let (app, _) = quota_grant_app().await;
    let (user, token) = authed_user(&app, "grant-stack").await;
    let (admin_id, admin_token) = make_role(&app, &unique_email("grant-admin"), "admin").await;
    let _ = admin_id;
    seed_usage(&app, user, 10, 0).await;

    // Two grants stack: 5 + 3 = 8 extra images.
    for amount in [5, 3] {
        let res = post_grant(
            &app,
            user,
            Some(&admin_token),
            json!({ "images": amount, "ttl_hours": 24, "reason": "stack probe" }),
        )
        .await;
        assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    }
    assert_eq!(grant_totals(&app, user, frozen_now()).await, (8, 0));
    let upload = seed_upload(&app, user, "image", None).await;
    app.seed_attestation(user, upload).await;
    let res = post_job(&app, &token, json!({ "upload_id": upload })).await;
    assert_eq!(res.status, StatusCode::NOT_IMPLEMENTED, "{}", res.text());
    assert_eq!(quota_headers(&res), ("8".to_string(), "0".to_string()));
}

#[tokio::test]
async fn quota_grant_validation() {
    let (app, _) = quota_grant_app().await;
    let (target, _) = authed_user(&app, "grant-valid").await;
    let (admin_id, admin_token) = make_role(&app, &unique_email("grant-admin"), "admin").await;
    let _ = admin_id;
    let unknown = Uuid::now_v7();

    // No amount at all: 422, and no grant was written.
    let res = post_grant(
        &app,
        target,
        Some(&admin_token),
        json!({ "ttl_hours": 24, "reason": "no amount" }),
    )
    .await;
    assert_eq!(
        res.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        res.text()
    );
    assert_eq!(grant_totals(&app, target, frozen_now()).await, (0, 0));

    // A zero amount: 422, nothing written.
    let res = post_grant(
        &app,
        target,
        Some(&admin_token),
        json!({ "images": 0, "ttl_hours": 24, "reason": "zero" }),
    )
    .await;
    assert_eq!(
        res.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        res.text()
    );
    assert_eq!(grant_totals(&app, target, frozen_now()).await, (0, 0));

    // ttl 0 and ttl 169 (past the 168 h cap): 422 each.
    for ttl in [0, 169] {
        let res = post_grant(
            &app,
            target,
            Some(&admin_token),
            json!({ "images": 5, "ttl_hours": ttl, "reason": "ttl probe" }),
        )
        .await;
        assert_eq!(
            res.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "ttl {ttl}: {}",
            res.text()
        );
    }
    assert_eq!(grant_totals(&app, target, frozen_now()).await, (0, 0));

    // A blank reason: 422, nothing written.
    let res = post_grant(
        &app,
        target,
        Some(&admin_token),
        json!({ "images": 5, "ttl_hours": 24, "reason": "   " }),
    )
    .await;
    assert_eq!(
        res.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        res.text()
    );
    assert_eq!(grant_totals(&app, target, frozen_now()).await, (0, 0));

    // An unknown user: 404 PF-2010, and no audit row.
    let before = grant_audit_rows(&app, unknown).await.len();
    let res = post_grant(
        &app,
        unknown,
        Some(&admin_token),
        json!({ "images": 5, "ttl_hours": 24, "reason": "unknown" }),
    )
    .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND, "{}", res.text());
    assert_eq!(problem_code(&res), "PF-2010");
    assert_eq!(
        grant_audit_rows(&app, unknown).await.len(),
        before,
        "a 404 audits nothing"
    );
}
