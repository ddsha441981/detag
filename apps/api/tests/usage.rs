//! P1-080: the usage API — `GET /v1/usage`.
//!
//! The endpoint reads the durable `usage_counters` rows
//! (the DB is the source of truth), so the tests seed
//! those rows directly — no production caller records
//! anything yet (job creation is the 501 stub; Phase 2
//! wires `record` at create and `true_up` at completion,
//! P1-079) — and seed `jobs` rows directly for the
//! per-key breakdown (the breakdown counts jobs by
//! `key_id` + `created_at`, via `usage_breakdown_for_user`).
//!
//! - `usage_guard` — the extractor: anonymous 401, a JWT
//!   user reaches the handler, a key without `jobs:read`
//!   is 403 `PF-1002`, a key with `jobs:read` reaches it
//!   (the billing exception to "no billing for keys",
//!   ADR-0044).
//! - `usage_shape_contract` — the AC: a JWT caller gets
//!   today + 30 daily buckets ending today (oldest first,
//!   missing days are zeros, the 31st day back is outside
//!   the window, another user's rows never leak) and no
//!   `by_key` field at all.
//! - `usage_key_caller_gets_breakdown` — a key caller gets
//!   the same buckets plus `by_key`: today's per-key
//!   breakdown (the calling key, a sibling key, and the
//!   unattributed row for web / JWT-created jobs),
//!   yesterday's jobs excluded, `video_seconds` 0 with
//!   the count in `video_jobs` (jobs carry no duration
//!   until Phase 2).
//! - `usage_empty_user_is_zeros` — a user who used nothing
//!   gets zeros, not a 404: today zeros, 30 zero buckets,
//!   and an empty `by_key` for a key caller.

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
/// Today is 2026-11-19; the 30-day window starts
/// 2026-10-21; 2026-10-20 is one day outside it.
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
async fn usage_app() -> (TestApp, Arc<ManualClock>) {
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

/// Mint a key with the given scopes; return the secret.
async fn create_key(app: &TestApp, token: &str, scopes: &[&str]) -> String {
    let body = json!({
        "name": "usage probe",
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

/// The user's keys, oldest first: (id, prefix).
async fn keys_for_user(app: &TestApp, user_id: Uuid) -> Vec<(Uuid, String)> {
    sqlx::query_as::<_, (Uuid, String)>(
        "SELECT id, prefix FROM api_keys WHERE user_id = $1 ORDER BY created_at ASC",
    )
    .bind(user_id)
    .fetch_all(&app.pool)
    .await
    .expect("keys for user")
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

/// Seed `count` job rows for the user, created at
/// `created_at`, attributed to `key_id` (`None` = a web /
/// JWT-created job). `kind` is 'image' or 'video'.
async fn seed_jobs(
    app: &TestApp,
    user_id: Uuid,
    key_id: Option<Uuid>,
    kind: &str,
    count: usize,
    created_at: DateTime<Utc>,
) {
    for _ in 0..count {
        sqlx::query(
            "INSERT INTO jobs (user_id, kind, mode, status, lane, key_id, created_at) \
             VALUES ($1, $2, 'inpaint', 'queued', 'standard', $3, $4)",
        )
        .bind(user_id)
        .bind(kind)
        .bind(key_id)
        .bind(created_at)
        .execute(&app.pool)
        .await
        .expect("seed job");
    }
}

/// GET /v1/usage with the given bearer token (or none for
/// the anonymous probe).
async fn get_usage(app: &TestApp, token: Option<&str>) -> common::TestResponse {
    let mut req = Request::builder()
        .method(Method::GET)
        .uri("/v1/usage")
        .header("x-request-id", REQUEST_ID)
        .header("x-forwarded-for", unique_xff());
    if let Some(token) = token {
        req = req.header("authorization", format!("Bearer {token}"));
    }
    app.request(req.body(axum::body::Body::empty()).unwrap())
        .await
}

#[tokio::test]
async fn usage_guard() {
    let (app, _clock) = usage_app().await;
    // Anonymous: 401 PF-1001.
    let res = get_usage(&app, None).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-1001");
    // A JWT user reaches the handler (200 with zeros: no
    // usage seeded — the extractor let them through).
    let email = format!("usage-guard-{}@example.com", Uuid::now_v7());
    let id = register_user(&app, &email).await;
    let token = login_token(&app, &email).await;
    let res = get_usage(&app, Some(&token)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    // An API key without jobs:read is 403 PF-1002.
    // P1-055: key creation is plan-gated — the user needs a
    // Pro grant before it can mint the probe keys.
    app.grant_plan(id, "pro").await;
    let write_secret = create_key(&app, &token, &["jobs:write"]).await;
    let res = get_usage(&app, Some(&write_secret)).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-1002");
    // A key with jobs:read reaches the handler — and, as a
    // key caller, gets the per-key breakdown (empty: no
    // jobs created today).
    let read_secret = create_key(&app, &token, &["jobs:read"]).await;
    let res = get_usage(&app, Some(&read_secret)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let body = res.json();
    assert_eq!(
        body["by_key"],
        serde_json::json!([]),
        "a key caller with no jobs today gets an empty breakdown: {body}"
    );
}

#[tokio::test]
async fn usage_shape_contract() {
    let (app, _clock) = usage_app().await;
    let email = format!("usage-shape-{}@example.com", Uuid::now_v7());
    let id = register_user(&app, &email).await;
    let token = login_token(&app, &email).await;
    // Today, yesterday, the window's first day, and one
    // day outside the window; plus another user's row
    // today (must never leak into this user's buckets).
    seed_counters(
        &app,
        id,
        &[
            (day(2026, 11, 19), 5, 120),
            (day(2026, 11, 18), 2, 60),
            (day(2026, 10, 21), 1, 30),
            (day(2026, 10, 20), 9, 999),
        ],
    )
    .await;
    let other = register_user(&app, &format!("usage-other-{}@example.com", Uuid::now_v7())).await;
    seed_counters(&app, other, &[(day(2026, 11, 19), 100, 1000)]).await;

    let res = get_usage(&app, Some(&token)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    // The AC: the shape contract — today, 30 buckets
    // ending today, oldest first, no by_key for JWT.
    let body = res.json();
    assert_eq!(body["user_id"], id.to_string());
    assert_eq!(
        body["today"],
        json!({"date": "2026-11-19", "images": 5, "video_seconds": 120}),
        "today's bucket: {body}"
    );
    let days = body["days"].as_array().expect("days is an array");
    assert_eq!(days.len(), 30, "the window is 30 buckets: {body}");
    assert_eq!(days[0]["date"], "2026-10-21", "oldest first: {body}");
    assert_eq!(days[0]["images"], 1);
    assert_eq!(days[0]["video_seconds"], 30);
    assert_eq!(days[29], body["today"], "the last bucket IS today: {body}");
    // A middle day with no row is zeros, not absent.
    let middle = days
        .iter()
        .find(|bucket| bucket["date"] == "2026-11-10")
        .expect("every day in the window has a bucket");
    assert_eq!(middle["images"], 0);
    assert_eq!(middle["video_seconds"], 0);
    // Yesterday's seeded row lands in its bucket.
    let yesterday = days
        .iter()
        .find(|bucket| bucket["date"] == "2026-11-18")
        .expect("yesterday's bucket exists");
    assert_eq!(yesterday["images"], 2);
    assert_eq!(yesterday["video_seconds"], 60);
    // The 31st day back (2026-10-20, 9 images) is outside
    // the window — no bucket carries it.
    assert!(
        days.iter().all(|bucket| bucket["date"] != "2026-10-20"),
        "the day outside the window must not appear: {body}"
    );
    // No other user's usage leaks in (100 images today
    // would show as 100, not 5).
    assert_eq!(
        body["today"]["images"], 5,
        "another user's row leaked: {body}"
    );
    // JWT callers get no per-key breakdown at all.
    assert!(
        body.get("by_key").is_none(),
        "by_key is key-caller-only: {body}"
    );
}

#[tokio::test]
async fn usage_key_caller_gets_breakdown() {
    let (app, _clock) = usage_app().await;
    let email = format!("usage-key-{}@example.com", Uuid::now_v7());
    let id = register_user(&app, &email).await;
    let token = login_token(&app, &email).await;
    app.grant_plan(id, "pro").await;
    // Two keys: the caller (first minted) and a sibling.
    let caller_secret = create_key(&app, &token, &["jobs:read"]).await;
    let _sibling_secret = create_key(&app, &token, &["jobs:read"]).await;
    let keys = keys_for_user(&app, id).await;
    assert_eq!(keys.len(), 2, "two keys minted");
    let (caller_id, caller_prefix) = keys[0].clone();
    let (sibling_id, sibling_prefix) = keys[1].clone();

    // The counters are the truth for the buckets (seeded
    // directly — no production caller records yet).
    seed_counters(&app, id, &[(day(2026, 11, 19), 7, 90)]).await;
    // Today's jobs: caller 3 image + 1 video, sibling 2
    // image, web / JWT-created 1 image — plus one caller
    // image job yesterday (must not appear in today's
    // breakdown).
    let today_10am = Utc.with_ymd_and_hms(2026, 11, 19, 10, 0, 0).unwrap();
    let yesterday_10am = Utc.with_ymd_and_hms(2026, 11, 18, 10, 0, 0).unwrap();
    seed_jobs(&app, id, Some(caller_id), "image", 3, today_10am).await;
    seed_jobs(&app, id, Some(caller_id), "video", 1, today_10am).await;
    seed_jobs(&app, id, Some(sibling_id), "image", 2, today_10am).await;
    seed_jobs(&app, id, None, "image", 1, today_10am).await;
    seed_jobs(&app, id, Some(caller_id), "image", 1, yesterday_10am).await;

    let res = get_usage(&app, Some(&caller_secret)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let body = res.json();
    // The buckets come from the counters, not the jobs.
    assert_eq!(body["today"]["images"], 7);
    assert_eq!(body["today"]["video_seconds"], 90);
    // The breakdown: three rows — calling key, sibling
    // key, unattributed — ordered by images desc.
    let by_key = body["by_key"].as_array().expect("by_key is an array");
    assert_eq!(by_key.len(), 3, "calling + sibling + unattributed: {body}");
    let caller_row = by_key
        .iter()
        .find(|row| row["key_id"] == caller_id.to_string())
        .expect("the calling key's row");
    assert_eq!(caller_row["key_prefix"], caller_prefix);
    assert_eq!(caller_row["images"], 3);
    assert_eq!(caller_row["video_jobs"], 1);
    assert_eq!(
        caller_row["video_seconds"], 0,
        "jobs carry no duration until Phase 2 — the count is in video_jobs: {body}"
    );
    let sibling_row = by_key
        .iter()
        .find(|row| row["key_id"] == sibling_id.to_string())
        .expect("the sibling key's row");
    assert_eq!(sibling_row["key_prefix"], sibling_prefix);
    assert_eq!(sibling_row["images"], 2);
    assert_eq!(sibling_row["video_jobs"], 0);
    // The unattributed row: web / JWT-created jobs.
    let unattributed = by_key
        .iter()
        .find(|row| row["key_id"].is_null())
        .expect("the unattributed row");
    assert!(unattributed["key_prefix"].is_null());
    assert_eq!(unattributed["images"], 1);
    assert_eq!(unattributed["video_jobs"], 0);
    // Yesterday's caller job is not in today's breakdown
    // (the caller row is 3 images, not 4).
    assert_eq!(caller_row["images"], 3, "yesterday's job leaked in: {body}");
}

#[tokio::test]
async fn usage_empty_user_is_zeros() {
    let (app, _clock) = usage_app().await;
    let email = format!("usage-empty-{}@example.com", Uuid::now_v7());
    let id = register_user(&app, &email).await;
    let token = login_token(&app, &email).await;
    // JWT: zeros, not a 404.
    let res = get_usage(&app, Some(&token)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let body = res.json();
    assert_eq!(
        body["today"],
        json!({"date": "2026-11-19", "images": 0, "video_seconds": 0}),
        "today is zeros: {body}"
    );
    let days = body["days"].as_array().expect("days is an array");
    assert_eq!(days.len(), 30);
    assert!(
        days.iter()
            .all(|bucket| bucket["images"] == 0 && bucket["video_seconds"] == 0),
        "every bucket is zeros: {body}"
    );
    assert!(body.get("by_key").is_none());
    // Key caller: the same zeros, with an empty breakdown.
    app.grant_plan(id, "pro").await;
    let secret = create_key(&app, &token, &["jobs:read"]).await;
    let res = get_usage(&app, Some(&secret)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let body = res.json();
    assert_eq!(body["today"]["images"], 0);
    assert_eq!(
        body["by_key"],
        serde_json::json!([]),
        "no jobs today means an empty breakdown: {body}"
    );
}
