//! P1-106: the support user view API — `GET
//! /v1/admin/users/{id}` (profile, plan, usage, recent jobs,
//! session meta).
//!
//! The view composes reads the API already owns, so the
//! tests seed rows directly — a subscription via
//! `grant_plan`, `usage_counters` rows, `jobs` rows, and one
//! login (which mints the live session) — and assert the
//! composite, not the parts (the parts are proven by their
//! own suites: P1-080 usage, P1-038 jobs, P1-019 sessions).
//!
//! - `admin_user_view_guard` — the extractor + matrix guard:
//!   anonymous 401, plain user 403, an admin user's own
//!   API key 403 ("no admin ever"), support 200, admin 200.
//! - `admin_user_view_shape` — the composite: the profile
//!   (email, role, status, verified), the plan (the granted
//!   Pro subscription, status `active`), the usage (today +
//!   all-time totals, another user's rows never leak), the
//!   10 newest jobs (12 seeded — the 2 oldest drop, newest
//!   first), and the 1 live session (device label + ip hash,
//!   no refresh hash) — and the raw body carries no secret
//!   field name anywhere.
//! - `admin_user_view_unknown_user_is_404` — an unknown user
//!   id is 404 `PF-2010`, not an empty view.
//! - `admin_user_view_empty_user_is_free_zeros_and_empty_arrays`
//!   — a user who exists but has nothing gets Free, zeros,
//!   and empty arrays, not a 404.

mod common;

use axum::http::{Method, Request, StatusCode};
use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use common::{TestApp, TestFixtures};
use detag_core::clock::{Clock, ManualClock};
use serde_json::json;
use std::sync::Arc;
use uuid::Uuid;

/// The request id: a valid UUID, echoed by the middleware.
const REQUEST_ID: &str = "123e4567-e89b-42d3-a456-426614174109";

/// The password every test user registers with.
const PASSWORD: &str = "correct horse battery staple";

/// The frozen "now": Thursday 2026-11-19, 12:00 UTC.
fn frozen_now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 11, 19, 12, 0, 0).unwrap()
}

fn day(y: i32, m: u32, d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, d).unwrap()
}

/// A per-request XFF address: each request gets its own IP
/// bucket, so the tests never trip each other's rate limits
/// (the P1-003 lesson; the full-UUID form, FLAKY-006).
fn unique_xff() -> String {
    format!("203.0.113.{}", Uuid::now_v7())
}

/// App on the frozen clock. No billing provider: the
/// endpoint never calls one.
async fn admin_user_view_app() -> (TestApp, Arc<ManualClock>) {
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

/// Register a user, set the role in the database, log in,
/// return (id, token). There is no self-serve way to become
/// an admin or support (the role is set by the owner out of
/// band; P1-009's search is the read side, the invite CLI is
/// a DECISIONS.md item).
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

/// Mint a key with the given scopes; return the secret.
async fn create_key(app: &TestApp, token: &str, scopes: &[&str]) -> String {
    let body = json!({
        "name": "admin user view probe",
        "scopes": scopes,
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

/// Seed `usage_counters` rows for the user: (day, images,
/// video_seconds).
async fn seed_counters(app: &TestApp, user_id: Uuid, rows: &[(NaiveDate, i32, i32)]) {
    for (day, images, video_seconds) in rows {
        sqlx::query(
            "INSERT INTO usage_counters (user_id, day, images_used, video_seconds_used) \
             VALUES ($1, $2, $3, $4)",
        )
        .bind(user_id)
        .bind(*day)
        .bind(*images)
        .bind(*video_seconds)
        .execute(&app.pool)
        .await
        .expect("seed usage counter");
    }
}

/// Seed one job row for the user, created at `created_at`.
/// `kind` is 'image' or 'video'; `status` is the job
/// status the view reports.
async fn seed_job(
    app: &TestApp,
    user_id: Uuid,
    kind: &str,
    status: &str,
    created_at: DateTime<Utc>,
) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO jobs (id, user_id, kind, mode, status, lane, created_at) \
         VALUES ($1, $2, $3, 'inpaint', $4, 'standard', $5)",
    )
    .bind(id)
    .bind(user_id)
    .bind(kind)
    .bind(status)
    .bind(created_at)
    .execute(&app.pool)
    .await
    .expect("seed job");
    id
}

/// GET /v1/admin/users/{id} with the given bearer token (or
/// none for the anonymous probe).
async fn get_admin_user_view(
    app: &TestApp,
    user_id: Uuid,
    token: Option<&str>,
) -> common::TestResponse {
    let mut req = Request::builder()
        .method(Method::GET)
        .uri(format!("/v1/admin/users/{user_id}"))
        .header("x-request-id", REQUEST_ID)
        .header("x-forwarded-for", unique_xff());
    if let Some(token) = token {
        req = req.header("authorization", format!("Bearer {token}"));
    }
    app.request(req.body(axum::body::Body::empty()).unwrap())
        .await
}

#[tokio::test]
async fn admin_user_view_guard() {
    let (app, _clock) = admin_user_view_app().await;
    let (admin_id, admin_token) = make_role(
        &app,
        &format!("guard-admin-{}@example.com", Uuid::now_v7()),
        "admin",
    )
    .await;
    let (_support_id, support_token) = make_role(
        &app,
        &format!("guard-support-{}@example.com", Uuid::now_v7()),
        "support",
    )
    .await;
    let (_user_id, user_token) = make_role(
        &app,
        &format!("guard-user-{}@example.com", Uuid::now_v7()),
        "user",
    )
    .await;
    // P1-055: key creation is plan-gated — the admin user
    // needs a Pro grant before it can mint the probe key.
    app.grant_plan(admin_id, "pro").await;
    let admin_key = create_key(
        &app,
        &admin_token,
        &["jobs:read", "jobs:write", "webhooks:manage"],
    )
    .await;
    let target = register_user(
        &app,
        &format!("guard-target-{}@example.com", Uuid::now_v7()),
    )
    .await;

    // Anonymous: 401 PF-1001.
    let res = get_admin_user_view(&app, target, None).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-1001");
    // A verified plain user: 403 PF-1002.
    let res = get_admin_user_view(&app, target, Some(&user_token)).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-1002");
    // An admin user's own API key: 403 PF-1002 ("no admin
    // ever").
    let res = get_admin_user_view(&app, target, Some(&admin_key)).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-1002");
    // Support (read-only role): 200.
    let res = get_admin_user_view(&app, target, Some(&support_token)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    // Admin: 200.
    let res = get_admin_user_view(&app, target, Some(&admin_token)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
}

#[tokio::test]
async fn admin_user_view_shape() {
    let (app, _clock) = admin_user_view_app().await;
    let (_admin_id, admin_token) = make_role(
        &app,
        &format!("shape-admin-{}@example.com", Uuid::now_v7()),
        "admin",
    )
    .await;
    let target_email = format!("shape-target-{}@example.com", Uuid::now_v7());
    let target = register_user(&app, &target_email).await;
    // The one login (the file header's design): register mints
    // NO session, so the target logs in once — that login is
    // the single live session the view must carry. (The 170th:
    // the shape test failed before this line existed —
    // `sessions` was `[]`, 0 vs the asserted 1 — because the
    // login call was missing; the empty-user test, same
    // register-only setup, correctly expects `[]`.)
    login_token(&app, &target_email).await;
    // The plan: a Pro grant (a live `active` subscription).
    app.grant_plan(target, "pro").await;
    // The usage: today (5 images, 120 s) + a row 40 days
    // back (totals only); plus another user's row today
    // (must never leak).
    seed_counters(
        &app,
        target,
        &[(day(2026, 11, 19), 5, 120), (day(2026, 10, 10), 3, 45)],
    )
    .await;
    let other = register_user(&app, &format!("shape-other-{}@example.com", Uuid::now_v7())).await;
    seed_counters(&app, other, &[(day(2026, 11, 19), 100, 1000)]).await;
    // The jobs: 12 rows, one minute apart (10:00–10:11),
    // alternating kind, the first failed, the rest queued —
    // the view carries the 10 newest, newest first, so the
    // 10:00 + 10:01 rows drop.
    let base = Utc.with_ymd_and_hms(2026, 11, 19, 10, 0, 0).unwrap();
    let mut seeded = Vec::new();
    for i in 0..12 {
        let created = base + chrono::Duration::minutes(i);
        let kind = if i % 2 == 0 { "image" } else { "video" };
        let status = if i == 0 { "failed" } else { "queued" };
        seeded.push(seed_job(&app, target, kind, status, created).await);
    }

    let res = get_admin_user_view(&app, target, Some(&admin_token)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let raw = res.text();
    // No secret field name anywhere in the body — the AC
    // ("no secrets") proven on the wire, not just in the
    // unit tests (the P1-104 PII-subject precedent: grep the
    // rendered output).
    for forbidden in [
        "password_hash",
        "refresh_hash",
        "family_id",
        "provider_customer_id",
        "provider_subscription_id",
    ] {
        assert!(
            !raw.contains(forbidden),
            "view body must not contain {forbidden}: {raw}"
        );
    }
    let body = res.json();
    assert_eq!(body["user_id"], target.to_string());
    // The profile: the account, as support sees it.
    assert_eq!(body["profile"]["email"], target_email);
    assert_eq!(body["profile"]["role"], "user");
    assert_eq!(body["profile"]["status"], "active");
    // Registration alone does not verify the email (the
    // verify-email step is separate) — the field is present
    // as null, not absent.
    assert!(body["profile"]["email_verified_at"].is_null());
    assert_eq!(body["profile"]["id"], target.to_string());
    // The plan: the granted Pro subscription.
    assert_eq!(body["plan"]["code"], "pro");
    assert_eq!(body["plan"]["status"], "active");
    assert!(body["plan"]["current_period_end"].is_null());
    assert_eq!(body["plan"]["cancel_at_period_end"], false);
    // The usage: today + the all-time totals (5+3 images,
    // 120+45 video seconds — the other user's 100 never
    // leaks in).
    assert_eq!(
        body["usage"]["today"],
        json!({"date": "2026-11-19", "images": 5, "video_seconds": 120}),
        "today's bucket: {body}"
    );
    assert_eq!(
        body["usage"]["totals"],
        json!({"images": 8, "video_seconds": 165}),
        "totals are all-time, not window-only: {body}"
    );
    // The jobs: the 10 newest, newest first — seeded[11]
    // (10:11) first, seeded[2] (10:02) last; the 10:00 +
    // 10:01 rows are gone.
    let jobs = body["recent_jobs"]
        .as_array()
        .expect("recent_jobs is an array");
    assert_eq!(jobs.len(), 10, "the view carries 10 recent jobs: {body}");
    assert_eq!(jobs[0]["id"], seeded[11].to_string());
    assert_eq!(jobs[0]["kind"], "video");
    assert_eq!(jobs[9]["id"], seeded[2].to_string());
    assert_eq!(jobs[9]["kind"], "image");
    assert!(
        jobs.iter().all(|job| job.get("params").is_none()),
        "job rows carry no params: {body}"
    );
    // The sessions: exactly the one login — a device label
    // + ip hash, never the refresh hash or family id.
    let sessions = body["sessions"].as_array().expect("sessions is an array");
    assert_eq!(sessions.len(), 1, "one live session (the login): {body}");
    assert!(sessions[0]["device"].is_string());
    assert!(sessions[0]["ip_hash"].is_string());
    assert!(sessions[0].get("refresh_hash").is_none());
    assert!(sessions[0].get("family_id").is_none());
}

#[tokio::test]
async fn admin_user_view_unknown_user_is_404() {
    let (app, _clock) = admin_user_view_app().await;
    let (_admin_id, admin_token) = make_role(
        &app,
        &format!("nf-admin-{}@example.com", Uuid::now_v7()),
        "admin",
    )
    .await;
    let res = get_admin_user_view(&app, Uuid::now_v7(), Some(&admin_token)).await;
    assert_eq!(res.status, StatusCode::NOT_FOUND, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-2010");
}

#[tokio::test]
async fn admin_user_view_empty_user_is_free_zeros_and_empty_arrays() {
    let (app, _clock) = admin_user_view_app().await;
    let (_admin_id, admin_token) = make_role(
        &app,
        &format!("empty-admin-{}@example.com", Uuid::now_v7()),
        "admin",
    )
    .await;
    let target_email = format!("empty-target-{}@example.com", Uuid::now_v7());
    let target = register_user(&app, &target_email).await;

    let res = get_admin_user_view(&app, target, Some(&admin_token)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let body = res.json();
    assert_eq!(body["profile"]["email"], target_email);
    assert_eq!(body["plan"]["code"], "free");
    assert!(body["plan"]["status"].is_null());
    assert!(body["plan"]["current_period_end"].is_null());
    assert_eq!(body["plan"]["cancel_at_period_end"], false);
    assert_eq!(
        body["usage"]["today"],
        json!({"date": "2026-11-19", "images": 0, "video_seconds": 0}),
        "an unused user gets zeros: {body}"
    );
    assert_eq!(
        body["usage"]["totals"],
        json!({"images": 0, "video_seconds": 0}),
        "an unused user gets zero totals: {body}"
    );
    assert_eq!(body["recent_jobs"], json!([]), "no jobs: {body}");
    assert_eq!(body["sessions"], json!([]), "no sessions: {body}");
}
