//! P1-081: the admin usage view API — `GET
//! /v1/admin/usage/{id}` (JSON) + `GET
//! /v1/admin/usage/{id}/export` (CSV).
//!
//! The view reads the durable `usage_counters` rows (the DB
//! is the source of truth), so the tests seed those rows
//! directly — no production caller records anything yet
//! (job creation is the 501 stub; Phase 2 wires `record` at
//! create and `true_up` at completion, P1-079) — and seed
//! `jobs` rows directly for the per-key breakdown (the
//! breakdown counts jobs by `key_id` + `created_at`, via
//! `usage_breakdown_for_user`).
//!
//! - `admin_usage_guard` — the extractor + matrix guard:
//!   anonymous 401, plain user 403, an admin user's own
//!   API key 403 ("no admin ever"), support 200, admin 200.
//! - `admin_usage_view_shape` — the JSON shape: today + 30
//!   buckets ending today (oldest first, the 31st day back
//!   outside the window, another user's rows never leak),
//!   the all-time totals (a row 40 days back counts in the
//!   totals but not in the window), and `by_key` always
//!   present (calling key, sibling key, unattributed row;
//!   yesterday's jobs excluded).
//! - `admin_usage_export_parses` — the AC: the CSV export
//!   parses — header `date,images,video_seconds`, 30 rows
//!   oldest first ending today, values matching the seeded
//!   counters, zero rows for missing days, the 31st day out.
//! - `admin_usage_unknown_user_is_404` — an unknown user id
//!   is 404 `PF-2010` on both endpoints, not zeros.
//! - `admin_usage_empty_user_is_zeros` — a user who exists
//!   but used nothing gets zeros (JSON) and a header plus
//!   30 zero rows (CSV), not a 404.

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
/// 2026-10-21; 2026-10-20 is one day outside it;
/// 2026-10-10 is 40 days back (totals only).
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
/// endpoints never call one.
async fn admin_usage_app() -> (TestApp, Arc<ManualClock>) {
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
        "name": "admin usage probe",
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

/// GET /v1/admin/usage/{id} (or /export) with the given
/// bearer <redacted> (or none for the anonymous probe).
async fn get_admin_usage(
    app: &TestApp,
    user_id: Uuid,
    export: bool,
    token: Option<&str>,
) -> common::TestResponse {
    let uri = if export {
        format!("/v1/admin/usage/{user_id}/export")
    } else {
        format!("/v1/admin/usage/{user_id}")
    };
    let mut req = Request::builder()
        .method(Method::GET)
        .uri(uri)
        .header("x-request-id", REQUEST_ID)
        .header("x-forwarded-for", unique_xff());
    if let Some(token) = token {
        req = req.header("authorization", format!("Bearer {token}"));
    }
    app.request(req.body(axum::body::Body::empty()).unwrap())
        .await
}

#[tokio::test]
async fn admin_usage_guard() {
    let (app, _clock) = admin_usage_app().await;
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

    for export in [false, true] {
        // Anonymous: 401 PF-1001.
        let res = get_admin_usage(&app, target, export, None).await;
        assert_eq!(
            res.status,
            StatusCode::UNAUTHORIZED,
            "export={export}: {}",
            res.text()
        );
        assert_eq!(res.json()["code"], "PF-1001");
        // A verified plain user: 403 PF-1002.
        let res = get_admin_usage(&app, target, export, Some(&user_token)).await;
        assert_eq!(
            res.status,
            StatusCode::FORBIDDEN,
            "export={export}: {}",
            res.text()
        );
        assert_eq!(res.json()["code"], "PF-1002");
        // An admin user's own API key: 403 PF-1002 ("no
        // admin ever").
        let res = get_admin_usage(&app, target, export, Some(&admin_key)).await;
        assert_eq!(
            res.status,
            StatusCode::FORBIDDEN,
            "export={export}: {}",
            res.text()
        );
        assert_eq!(res.json()["code"], "PF-1002");
        // Support (read-only role): 200.
        let res = get_admin_usage(&app, target, export, Some(&support_token)).await;
        assert_eq!(
            res.status,
            StatusCode::OK,
            "export={export}: {}",
            res.text()
        );
        // Admin: 200.
        let res = get_admin_usage(&app, target, export, Some(&admin_token)).await;
        assert_eq!(
            res.status,
            StatusCode::OK,
            "export={export}: {}",
            res.text()
        );
    }
}

#[tokio::test]
async fn admin_usage_view_shape() {
    let (app, _clock) = admin_usage_app().await;
    let (_admin_id, admin_token) = make_role(
        &app,
        &format!("shape-admin-{}@example.com", Uuid::now_v7()),
        "admin",
    )
    .await;
    let target_email = format!("shape-target-{}@example.com", Uuid::now_v7());
    let target = register_user(&app, &target_email).await;
    let target_token = login_token(&app, &target_email).await;
    // Today, yesterday, the window's first day, one day
    // outside the window, and one row 40 days back (totals
    // only); plus another user's row today (must never leak).
    seed_counters(
        &app,
        target,
        &[
            (day(2026, 11, 19), 5, 120),
            (day(2026, 11, 18), 2, 60),
            (day(2026, 10, 21), 1, 30),
            (day(2026, 10, 20), 9, 999),
            (day(2026, 10, 10), 3, 45),
        ],
    )
    .await;
    let other = register_user(&app, &format!("shape-other-{}@example.com", Uuid::now_v7())).await;
    seed_counters(&app, other, &[(day(2026, 11, 19), 100, 1000)]).await;
    // Today's per-key breakdown: two keys + the
    // unattributed row; yesterday's jobs are excluded.
    app.grant_plan(target, "pro").await;
    let _key1 = create_key(&app, &target_token, &["jobs:read"]).await;
    let _key2 = create_key(&app, &target_token, &["jobs:read"]).await;
    let keys = keys_for_user(&app, target).await;
    assert_eq!(keys.len(), 2, "two keys minted");
    let (key1_id, key1_prefix) = keys[0].clone();
    let (key2_id, key2_prefix) = keys[1].clone();
    let today_noon = Utc.with_ymd_and_hms(2026, 11, 19, 10, 0, 0).unwrap();
    let yesterday_noon = Utc.with_ymd_and_hms(2026, 11, 18, 10, 0, 0).unwrap();
    seed_jobs(&app, target, Some(key1_id), "image", 2, today_noon).await;
    seed_jobs(&app, target, Some(key2_id), "video", 1, today_noon).await;
    seed_jobs(&app, target, None, "image", 1, today_noon).await;
    seed_jobs(&app, target, Some(key1_id), "image", 7, yesterday_noon).await;

    let res = get_admin_usage(&app, target, false, Some(&admin_token)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let body = res.json();
    assert_eq!(body["user_id"], target.to_string());
    assert_eq!(
        body["today"],
        json!({"date": "2026-11-19", "images": 5, "video_seconds": 120}),
        "today's bucket: {body}"
    );
    let days = body["days"].as_array().expect("days is an array");
    assert_eq!(days.len(), 30, "the window is 30 buckets: {body}");
    assert_eq!(days[0]["date"], "2026-10-21", "oldest first: {body}");
    assert_eq!(days[0]["images"], 1);
    assert_eq!(days[29], body["today"], "the last bucket IS today: {body}");
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
    // The all-time totals: every seeded row counts,
    // including the 40-days-back row (3 images, 45 s) and
    // the 31st-day row (9 images, 999 s) the window drops:
    // images 5+2+1+9+3 = 20, video_seconds
    // 120+60+30+999+45 = 1254.
    assert_eq!(
        body["totals"],
        json!({"images": 20, "video_seconds": 1254}),
        "totals are all-time, not window-only: {body}"
    );
    // by_key is ALWAYS present in the admin view — the
    // breakdown, ordered by images desc: key1 (2 images),
    // unattributed (1 image), key2 (0 images, 1 video).
    let by_key = body["by_key"].as_array().expect("by_key is an array");
    assert_eq!(by_key.len(), 3, "three key rows today: {body}");
    assert_eq!(by_key[0]["key_id"], key1_id.to_string());
    assert_eq!(by_key[0]["key_prefix"], key1_prefix);
    assert_eq!(by_key[0]["images"], 2);
    assert_eq!(by_key[0]["video_jobs"], 0);
    assert_eq!(by_key[0]["video_seconds"], 0);
    assert!(
        by_key[1]["key_id"].is_null(),
        "the unattributed row: {body}"
    );
    assert!(by_key[1]["key_prefix"].is_null());
    assert_eq!(by_key[1]["images"], 1);
    assert_eq!(by_key[2]["key_id"], key2_id.to_string());
    assert_eq!(by_key[2]["key_prefix"], key2_prefix);
    assert_eq!(by_key[2]["images"], 0);
    assert_eq!(by_key[2]["video_jobs"], 1);
}

#[tokio::test]
async fn admin_usage_export_parses() {
    let (app, _clock) = admin_usage_app().await;
    let (_admin_id, admin_token) = make_role(
        &app,
        &format!("export-admin-{}@example.com", Uuid::now_v7()),
        "admin",
    )
    .await;
    let target = register_user(
        &app,
        &format!("export-target-{}@example.com", Uuid::now_v7()),
    )
    .await;
    seed_counters(
        &app,
        target,
        &[
            (day(2026, 11, 19), 5, 120),
            (day(2026, 11, 18), 2, 60),
            (day(2026, 10, 21), 1, 30),
            (day(2026, 10, 20), 9, 999),
        ],
    )
    .await;

    let res = get_admin_usage(&app, target, true, Some(&admin_token)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    // The export is CSV, offered as a download.
    let content_type = res
        .headers
        .get("content-type")
        .expect("content-type header")
        .to_str()
        .expect("content-type is ASCII")
        .to_string();
    assert!(
        content_type.starts_with("text/csv"),
        "content-type must be text/csv, got {content_type}"
    );
    let disposition = res
        .headers
        .get("content-disposition")
        .expect("content-disposition header")
        .to_str()
        .expect("content-disposition is ASCII")
        .to_string();
    assert!(
        disposition.contains("attachment") && disposition.contains(&target.to_string()),
        "attachment with the user id in the filename, got {disposition}"
    );
    // The AC: the export parses. A hand-rolled parse (no
    // new dependency): split lines, split commas, check
    // every field.
    let csv = res.text();
    let lines: Vec<&str> = csv.lines().collect();
    assert_eq!(lines.len(), 31, "header + 30 daily rows: {csv}");
    assert_eq!(lines[0], "date,images,video_seconds", "header: {csv}");
    let mut rows: Vec<(String, i64, i64)> = Vec::with_capacity(30);
    for line in &lines[1..] {
        let fields: Vec<&str> = line.split(',').collect();
        assert_eq!(fields.len(), 3, "three fields per row: {line}");
        let images: i64 = fields[1].parse().expect("images parses as an integer");
        let video_seconds: i64 = fields[2]
            .parse()
            .expect("video_seconds parses as an integer");
        rows.push((fields[0].to_string(), images, video_seconds));
    }
    // Oldest first, ending today; values match the seeded
    // counters; missing days are zero rows.
    assert_eq!(rows[0], ("2026-10-21".to_string(), 1, 30), "oldest first");
    assert_eq!(
        rows[29],
        ("2026-11-19".to_string(), 5, 120),
        "last row is today"
    );
    let yesterday = rows
        .iter()
        .find(|(date, _, _)| date == "2026-11-18")
        .expect("yesterday's row exists");
    assert_eq!(*yesterday, ("2026-11-18".to_string(), 2, 60));
    let middle = rows
        .iter()
        .find(|(date, _, _)| date == "2026-11-10")
        .expect("every day in the window has a row");
    assert_eq!(*middle, ("2026-11-10".to_string(), 0, 0));
    // The 31st day back (2026-10-20, 9 images) is outside
    // the window — no row carries it.
    assert!(
        rows.iter().all(|(date, _, _)| date != "2026-10-20"),
        "the day outside the window must not appear: {csv}"
    );
    // The rows sum to the window totals: images 5+2+1 = 8,
    // video_seconds 120+60+30 = 210.
    assert_eq!(
        rows.iter().map(|(_, images, _)| images).sum::<i64>(),
        8,
        "window image sum: {csv}"
    );
    assert_eq!(
        rows.iter()
            .map(|(_, _, video_seconds)| video_seconds)
            .sum::<i64>(),
        210,
        "window video-seconds sum: {csv}"
    );
}

#[tokio::test]
async fn admin_usage_unknown_user_is_404() {
    let (app, _clock) = admin_usage_app().await;
    let (_admin_id, admin_token) = make_role(
        &app,
        &format!("nf-admin-{}@example.com", Uuid::now_v7()),
        "admin",
    )
    .await;
    let ghost = Uuid::now_v7();
    for export in [false, true] {
        let res = get_admin_usage(&app, ghost, export, Some(&admin_token)).await;
        assert_eq!(
            res.status,
            StatusCode::NOT_FOUND,
            "export={export}: {}",
            res.text()
        );
        assert_eq!(res.json()["code"], "PF-2010");
    }
}

#[tokio::test]
async fn admin_usage_empty_user_is_zeros() {
    let (app, _clock) = admin_usage_app().await;
    let (_admin_id, admin_token) = make_role(
        &app,
        &format!("empty-admin-{}@example.com", Uuid::now_v7()),
        "admin",
    )
    .await;
    let target = register_user(
        &app,
        &format!("empty-target-{}@example.com", Uuid::now_v7()),
    )
    .await;

    // JSON: zeros, not a 404.
    let res = get_admin_usage(&app, target, false, Some(&admin_token)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let body = res.json();
    assert_eq!(body["user_id"], target.to_string());
    assert_eq!(
        body["today"],
        json!({"date": "2026-11-19", "images": 0, "video_seconds": 0}),
        "today is zeros: {body}"
    );
    assert_eq!(body["days"].as_array().expect("days is an array").len(), 30);
    assert!(body["days"]
        .as_array()
        .unwrap()
        .iter()
        .all(|bucket| bucket["images"] == 0 && bucket["video_seconds"] == 0));
    assert_eq!(body["totals"], json!({"images": 0, "video_seconds": 0}));
    assert_eq!(body["by_key"], json!([]), "no jobs today: {body}");

    // CSV: header plus 30 zero rows, not a 404.
    let res = get_admin_usage(&app, target, true, Some(&admin_token)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let csv = res.text();
    let lines: Vec<&str> = csv.lines().collect();
    assert_eq!(lines.len(), 31, "header + 30 daily rows: {csv}");
    assert_eq!(lines[0], "date,images,video_seconds");
    assert!(
        lines[1..].iter().all(|line| line.ends_with(",0,0")),
        "all zero rows: {csv}"
    );
}
