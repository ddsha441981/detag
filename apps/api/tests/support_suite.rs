//! `P1-111 Support suite` (FR-C-02 / ADR-0070 / ASSUMPTION-072).
//!
//! The cross-ticket scenario suite over the P1-106–P1-110
//! support surface — the family AS A STORY, which no single
//! ticket's file proves. Test-only: no new route, service,
//! migration, table, config, or worker kind, so every
//! registry count stays (route table 81, OpenAPI 49 paths,
//! authz 29 rows, API-key matrix 79 rows, audit_abuse_suite
//! live writes 13). The six tests:
//!
//! - `support_suite_role_matrix` — role enforcement across
//!   the family in one place: 6 endpoints × 5 caller
//!   classes (anon, plain user, support, admin, admin API
//!   key). The three reads admit support (200); the three
//!   writes deny support (403 `PF-1002`); anon is 401 with
//!   code `PF-1001`, a plain user / an API key is 403 on
//!   all six; admin is 200 on all six.
//! - `support_suite_full_workflow` — the support story: a
//!   Free user at 10/10 images is 429 `PF-3001`; support
//!   looks the user up by email (200, one audit row);
//!   support opens the view (plan free, usage 10/10, no
//!   secrets); an admin grants 5 images (one audit row);
//!   the user's create reaches the 501 stub with 5
//!   remaining; the admin appends a note (one audit row);
//!   support re-opens the view and reads the note
//!   verbatim. The audit trail for the user is exactly
//!   [lookup, quota_granted, note_added], in order.
//! - `support_suite_quota_grant_expiry_honored` — the AC
//!   at suite level: a 1 h grant lets the create through;
//!   the frozen clock advanced 2 h past expiry, the same
//!   create is 429 `PF-3001` again; the grant row still
//!   exists (expiry is a read-time rule, not a delete)
//!   and no new audit row appears on expiry.
//! - `support_suite_audit_trail` — a lookup hit audits, a
//!   lookup miss 404s AND audits (target null), a view
//!   read audits nothing, a grant + a note + a refund
//!   audit exactly one row each with the right
//!   actor/target, and a denied support write audits
//!   nothing.
//! - `support_suite_refund_guides_and_changes_nothing` —
//!   the refund returns the checklist (dashboard deep-link
//!   with a note, four steps all `done: false`; the required
//!   fields carry values where known and null for `amount`
//!   and `invoice_or_payment_id`); the subscription row is
//!   byte-identical afterwards, the refund adds no mail,
//!   exactly one `billing.refund.requested` row exists,
//!   and a second call writes a second row.
//! - `support_suite_attestations_visible_to_support` —
//!   eleven seeded attestations, one second apart:
//!   support lists them and reads the newest row flagged
//!   (`velocity_count` 11), the rest 10..1 unflagged, and
//!   a `?flagged=true` query returns only the flagged row
//!   (for this user).
//!
//! The app runs on the frozen clock (Thursday 2026-11-19,
//! 12:00 UTC) and usage is seeded for the FROZEN day — the
//! snapshot reads the clock's day, not the wall clock.
//! The suite deliberately re-proves per-ticket ACs at the
//! story level (P1-107 expiry, P1-110 note visibility,
//! P1-108 checklist, P1-098 flag) — the per-ticket files
//! stay the unit of record; this file is the regression
//! net that fails first when the family drifts.

mod common;

use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
};
use chrono::{DateTime, Duration, NaiveDate, Utc};
use common::{TestApp, TestFixtures};
use detag_core::{
    checksum::sha256_hex,
    clock::{Clock, ManualClock},
    repos::{audit_logs, plans, subscriptions, uploads, users},
};
use serde_json::{json, Value};
use std::sync::Arc;
use uuid::Uuid;

/// The request id: a valid UUID, echoed by the middleware.
const REQUEST_ID: &str = "123e4567-e89b-42d3-a456-426614174113";

/// The password every test user registers with.
const PASSWORD: &str = "correct horse battery staple";

/// The test address attestations are seeded from (its hash
/// is stored, never the address — NFR-42).
const TEST_IP: &str = "203.0.113.7";

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

/// App on the frozen clock. No billing provider: no
/// endpoint here calls one.
async fn support_suite_app() -> (TestApp, Arc<ManualClock>) {
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
                    serde_json::to_vec(&json!({ "name": "suite probe", "scopes": ["jobs:read", "jobs:write", "webhooks:manage"] })).unwrap(),
                ))
                .unwrap(),
        )
        .await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.text());
    res.json()["secret"].as_str().unwrap().to_string()
}

/// Send one request through the router as the given caller
/// (`None` = anonymous), with an optional JSON body.
async fn call(
    app: &TestApp,
    method: &str,
    uri: &str,
    credential: Option<&str>,
    body: Option<Value>,
) -> common::TestResponse {
    let mut req = Request::builder()
        .method(Method::from_bytes(method.as_bytes()).unwrap())
        .uri(uri)
        .header("x-request-id", REQUEST_ID)
        .header("x-forwarded-for", unique_xff());
    if let Some(token) = credential {
        req = req.header("authorization", format!("Bearer {token}"));
    }
    if let Some(body) = body {
        req = req.header("content-type", "application/json");
        app.request(
            req.body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
    } else {
        app.request(req.body(Body::empty()).unwrap()).await
    }
}

/// POST /v1/jobs with a JSON body as the user.
async fn post_job(app: &TestApp, token: &str, body: Value) -> common::TestResponse {
    call(app, "POST", "/v1/jobs", Some(token), Some(body)).await
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
            storage_key: "test/support-suite-seed",
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

/// Seed a subscription row directly — there is no billing
/// flow yet to create one through the API (P1-065 owns
/// creation). Returns the row id.
async fn seed_subscription(app: &TestApp, user_id: Uuid, plan_code: &str) -> Uuid {
    let plan = plans::by_code(&app.pool, plan_code)
        .await
        .expect("load plan")
        .expect("plan is seeded");
    let row = sqlx::query(
        "INSERT INTO subscriptions (user_id, plan_id, provider, provider_customer_id, provider_subscription_id, status, current_period_end) \
         VALUES ($1, $2, 'stripe', $3, $4, 'active', $5) RETURNING id",
    )
    .bind(user_id)
    .bind(plan.id)
    .bind(format!("cus_p111_{}", Uuid::now_v7()))
    .bind(format!("sub_p111_{}", Uuid::now_v7()))
    .bind(Utc::now() + Duration::days(30))
    .fetch_one(&app.pool)
    .await
    .expect("seed subscription");
    use sqlx::Row;
    row.try_get::<Uuid, _>("id").expect("seed returns id")
}

/// Seed one attestation row directly (the P1-098
/// precedent — the list under test needs explicit
/// `created_at` values, and the capture service stamps
/// real time). The stored `ip_hash` is the sha256 of the
/// test address, never the address (NFR-42).
async fn seed_attestation_at(
    app: &TestApp,
    user_id: Uuid,
    upload_id: Uuid,
    created_at: DateTime<Utc>,
) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO attestations (user_id, upload_id, text_version, ip_hash, client, created_at) \
         VALUES ($1, $2, 'v1', $3, 'web', $4) RETURNING id",
    )
    .bind(user_id)
    .bind(upload_id)
    .bind(sha256_hex(TEST_IP.as_bytes()))
    .bind(created_at)
    .fetch_one(&app.pool)
    .await
    .expect("seed attestation")
}

/// Seed eleven attestations for the user, one second apart,
/// newest first: row `i` sits at `base - i` seconds, so its
/// 24 h velocity count is `11 - i` — exactly the newest
/// row is flagged. The base is a year in the future (the
/// P1-098 precedent), so the burst can never overlap
/// another run's rows; the test filters every assertion by
/// this user, so parallel binaries seeding their own
/// bursts cannot pollute it either.
async fn seed_velocity_burst(app: &TestApp, user_id: Uuid) -> Vec<Uuid> {
    let base = Utc::now() + Duration::days(365);
    let mut ids = Vec::with_capacity(11);
    for i in 0..11 {
        let upload = seed_upload(app, user_id, "image", None).await;
        let id = seed_attestation_at(app, user_id, upload, base - Duration::seconds(i)).await;
        ids.push(id);
    }
    ids
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

/// All audit rows for one target and one action, in
/// `list_for_target` order (oldest first).
async fn audit_for_action(
    app: &TestApp,
    target_type: &str,
    target_id: &str,
    action: &str,
) -> Vec<audit_logs::AuditLog> {
    audit_logs::list_for_target(&app.pool, target_type, target_id)
        .await
        .expect("list audit rows")
        .into_iter()
        .filter(|row| row.action == action)
        .collect()
}

/// All audit rows for one user target, every action, in
/// order — the trail a privacy review reads.
async fn audit_trail(app: &TestApp, user_id: Uuid) -> Vec<audit_logs::AuditLog> {
    audit_logs::list_for_target(&app.pool, "user", &user_id.to_string())
        .await
        .expect("list audit rows")
}

/// The user's ticket notes, oldest first — read straight
/// from the column, so the denial proof does not depend
/// on the view.
async fn user_notes(app: &TestApp, user_id: Uuid) -> Vec<Value> {
    users::support_notes(&app.pool, user_id)
        .await
        .expect("read notes")
        .expect("user exists")
}

/// How many outbox rows name this user — the refund adds
/// no mail, so the count does not move through the refund
/// test (registration already enqueued the welcome mail).
async fn mails_for_user(app: &TestApp, user_id: Uuid) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM email_outbox WHERE payload ->> 'user_id' = $1")
        .bind(user_id.to_string())
        .fetch_one(&app.pool)
        .await
        .expect("count outbox rows")
}

/// How many quota-grant rows exist for the user (expired
/// or not) — expiry deletes nothing.
async fn grant_row_count(app: &TestApp, user_id: Uuid) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM quota_grants WHERE user_id = $1")
        .bind(user_id)
        .fetch_one(&app.pool)
        .await
        .expect("count grant rows")
}

#[tokio::test]
async fn support_suite_role_matrix() {
    let (app, _) = support_suite_app().await;
    let target_email = unique_email("suite-matrix-target");
    let target = register_user(&app, &target_email).await;
    let (_, user_token) = authed_user(&app, "suite-matrix-user").await;
    let (_, support_token) =
        make_role(&app, &unique_email("suite-matrix-support"), "support").await;
    let (admin_id, admin_token) =
        make_role(&app, &unique_email("suite-matrix-admin"), "admin").await;
    // P1-055: key creation is plan-gated — the admin needs a
    // Pro grant before it can mint the probe key.
    app.grant_plan(admin_id, "pro").await;
    let admin_key = mint_api_key(&app, &admin_token).await;
    let subscription = seed_subscription(&app, target, "pro").await;

    // One row per endpoint: the method, the path, the body
    // a valid caller sends, and whether support is admitted
    // (a read) or denied (a write). The write bodies are
    // valid, so a 403 is the guard, not validation.
    struct Row {
        method: &'static str,
        path: String,
        body: Option<Value>,
        support_reads: bool,
    }
    let rows = [
        Row {
            method: "GET",
            path: format!("/v1/admin/users/{target}"),
            body: None,
            support_reads: true,
        },
        Row {
            method: "GET",
            path: format!("/v1/admin/users/lookup?email={target_email}"),
            body: None,
            support_reads: true,
        },
        Row {
            method: "GET",
            path: "/v1/admin/attestations".to_string(),
            body: None,
            support_reads: true,
        },
        Row {
            method: "POST",
            path: format!("/v1/admin/users/{target}/notes"),
            body: Some(json!({ "body": "matrix probe" })),
            support_reads: false,
        },
        Row {
            method: "POST",
            path: format!("/v1/admin/users/{target}/quota-grant"),
            body: Some(json!({ "images": 5, "ttl_hours": 24, "reason": "matrix probe" })),
            support_reads: false,
        },
        Row {
            method: "POST",
            path: format!("/v1/admin/billing/subscriptions/{subscription}/refund"),
            body: Some(json!({ "reason": "matrix probe" })),
            support_reads: false,
        },
    ];

    for row in &rows {
        // Anonymous: 401 PF-1001 on every row.
        let res = call(&app, row.method, &row.path, None, row.body.clone()).await;
        assert_eq!(
            res.status,
            StatusCode::UNAUTHORIZED,
            "{} {} (anon): {}",
            row.method,
            row.path,
            res.text()
        );
        assert_eq!(problem_code(&res), "PF-1001", "{} {}", row.method, row.path);

        // Plain user: 403 PF-1002 on every row.
        let res = call(
            &app,
            row.method,
            &row.path,
            Some(&user_token),
            row.body.clone(),
        )
        .await;
        assert_eq!(
            res.status,
            StatusCode::FORBIDDEN,
            "{} {} (plain user): {}",
            row.method,
            row.path,
            res.text()
        );
        assert_eq!(problem_code(&res), "PF-1002", "{} {}", row.method, row.path);

        // Support: 200 on the reads, 403 PF-1002 on the
        // writes — read-only per FR-A-07.
        let res = call(
            &app,
            row.method,
            &row.path,
            Some(&support_token),
            row.body.clone(),
        )
        .await;
        if row.support_reads {
            assert_eq!(
                res.status,
                StatusCode::OK,
                "{} {} (support read): {}",
                row.method,
                row.path,
                res.text()
            );
        } else {
            assert_eq!(
                res.status,
                StatusCode::FORBIDDEN,
                "{} {} (support write): {}",
                row.method,
                row.path,
                res.text()
            );
            assert_eq!(problem_code(&res), "PF-1002", "{} {}", row.method, row.path);
        }

        // An admin user's own API key: 403 PF-1002 — no
        // admin ever.
        let res = call(
            &app,
            row.method,
            &row.path,
            Some(&admin_key),
            row.body.clone(),
        )
        .await;
        assert_eq!(
            res.status,
            StatusCode::FORBIDDEN,
            "{} {} (api key): {}",
            row.method,
            row.path,
            res.text()
        );
        assert_eq!(problem_code(&res), "PF-1002", "{} {}", row.method, row.path);

        // Admin: 200 on every row.
        let res = call(
            &app,
            row.method,
            &row.path,
            Some(&admin_token),
            row.body.clone(),
        )
        .await;
        assert_eq!(
            res.status,
            StatusCode::OK,
            "{} {} (admin): {}",
            row.method,
            row.path,
            res.text()
        );
    }
}

#[tokio::test]
async fn support_suite_full_workflow() {
    let (app, _) = support_suite_app().await;
    let user_email = unique_email("suite-flow-user");
    let user = register_user(&app, &user_email).await;
    let user_token = login_token(&app, &user_email).await;
    let (_, support_token) = make_role(&app, &unique_email("suite-flow-support"), "support").await;
    let (admin_id, admin_token) = make_role(&app, &unique_email("suite-flow-admin"), "admin").await;
    let _ = admin_id;
    // Free at 10/10 images: the create is 429 PF-3001.
    seed_usage(&app, user, 10, 0).await;
    let upload = seed_upload(&app, user, "image", None).await;
    let res = post_job(&app, &user_token, json!({ "upload_id": upload })).await;
    assert_eq!(res.status, StatusCode::TOO_MANY_REQUESTS, "{}", res.text());
    assert_eq!(problem_code(&res), "PF-3001");
    assert_eq!(quota_headers(&res), ("0".to_string(), "0".to_string()));

    // Support looks the user up by email: 200, and the
    // lookup is audited (actor the support caller).
    let res = call(
        &app,
        "GET",
        &format!("/v1/admin/users/lookup?email={user_email}"),
        Some(&support_token),
        None,
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert_eq!(res.json()["user_id"].as_str().unwrap(), user.to_string());
    let lookups = audit_for_action(&app, "user", &user.to_string(), "admin.users.lookup").await;
    assert_eq!(lookups.len(), 1, "one lookup row");
    assert_eq!(lookups[0].after.as_ref().unwrap()["lookup_kind"], "email");
    assert_eq!(lookups[0].after.as_ref().unwrap()["found"], true);

    // Support opens the view: plan free, usage 10/10 today,
    // no secrets anywhere in the body.
    let res = call(
        &app,
        "GET",
        &format!("/v1/admin/users/{user}"),
        Some(&support_token),
        None,
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let view = res.json();
    assert_eq!(view["profile"]["email"], user_email);
    assert_eq!(view["profile"]["role"], "user");
    assert_eq!(view["plan"]["code"], "free");
    assert_eq!(view["usage"]["today"]["date"], "2026-11-19");
    assert_eq!(view["usage"]["today"]["images"], 10);
    assert_eq!(view["usage"]["today"]["video_seconds"], 0);
    assert_eq!(view["recent_jobs"].as_array().unwrap().len(), 0);
    assert_eq!(view["sessions"].as_array().unwrap().len(), 1);
    assert_eq!(view["support_notes"].as_array().unwrap().len(), 0);
    assert!(
        !res.text().contains("password_hash") && !res.text().contains("refresh_hash"),
        "the user view must never carry secrets: {}",
        res.text()
    );

    // An admin grants 5 images: the grant is audited, and
    // the user's create now reaches the 501 stub with 5
    // remaining.
    let res = call(
        &app,
        "POST",
        &format!("/v1/admin/users/{user}/quota-grant"),
        Some(&admin_token),
        Some(json!({ "images": 5, "ttl_hours": 24, "reason": "goodwill, quota wall" })),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let grants =
        audit_for_action(&app, "user", &user.to_string(), "admin.users.quota_granted").await;
    assert_eq!(grants.len(), 1, "one grant row");
    app.seed_attestation(user, upload).await;
    let res = post_job(&app, &user_token, json!({ "upload_id": upload })).await;
    assert_eq!(res.status, StatusCode::NOT_IMPLEMENTED, "{}", res.text());
    assert_eq!(problem_code(&res), "PF-5004");
    assert_eq!(quota_headers(&res), ("5".to_string(), "0".to_string()));

    // The admin appends a note: audited, and support reads
    // it back through the view — verbatim, oldest first.
    let res = call(
        &app,
        "POST",
        &format!("/v1/admin/users/{user}/notes"),
        Some(&admin_token),
        Some(json!({ "body": "Called the user, they will re-upload on Monday." })),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let notes = audit_for_action(&app, "user", &user.to_string(), "admin.users.note_added").await;
    assert_eq!(notes.len(), 1, "one note row");
    let res = call(
        &app,
        "GET",
        &format!("/v1/admin/users/{user}"),
        Some(&support_token),
        None,
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let view_notes = res.json()["support_notes"].as_array().unwrap().clone();
    assert_eq!(view_notes.len(), 1, "support reads the one note");
    assert_eq!(
        view_notes[0]["body"],
        "Called the user, they will re-upload on Monday."
    );
    assert_eq!(view_notes[0]["author_role"], "admin");

    // The trail for the user holds exactly the three
    // support actions, in the order they happened (the
    // register/login rows are the auth trail, not the
    // support trail — they sit before the lookup).
    let trail = audit_trail(&app, user).await;
    let actions: Vec<&str> = trail
        .iter()
        .map(|row| row.action.as_str())
        .filter(|action| action.starts_with("admin.users."))
        .collect();
    assert_eq!(
        actions,
        vec![
            "admin.users.lookup",
            "admin.users.quota_granted",
            "admin.users.note_added"
        ],
        "the workflow audits exactly its three actions, in order"
    );
}

#[tokio::test]
async fn support_suite_quota_grant_expiry_honored() {
    let (app, clock) = support_suite_app().await;
    // The email is kept (not `authed_user`) so the test can
    // re-login after the clock advance: the access token
    // lives 15 minutes, and the advance is 2 h (the
    // api_key_lifecycle re-login precedent).
    let email = unique_email("suite-expiry");
    let user = register_user(&app, &email).await;
    let token = login_token(&app, &email).await;
    let (admin_id, admin_token) =
        make_role(&app, &unique_email("suite-expiry-admin"), "admin").await;
    let _ = admin_id;
    seed_usage(&app, user, 10, 0).await;

    // A 1 h grant: the create reaches the stub.
    let res = call(
        &app,
        "POST",
        &format!("/v1/admin/users/{user}/quota-grant"),
        Some(&admin_token),
        Some(json!({ "images": 5, "ttl_hours": 1, "reason": "expiry probe" })),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let upload = seed_upload(&app, user, "image", None).await;
    app.seed_attestation(user, upload).await;
    let res = post_job(&app, &token, json!({ "upload_id": upload })).await;
    assert_eq!(res.status, StatusCode::NOT_IMPLEMENTED, "{}", res.text());
    assert_eq!(quota_headers(&res), ("5".to_string(), "0".to_string()));
    let grants_before =
        audit_for_action(&app, "user", &user.to_string(), "admin.users.quota_granted").await;
    assert_eq!(grants_before.len(), 1);

    // The clock 2 h past the grant's expiry: the grant
    // contributes nothing, and the same create is 429
    // again (the AC: expiry honoured). Re-login first —
    // the first token died with the 2 h advance.
    clock.advance(Duration::hours(2));
    assert_eq!(
        detag_core::repos::quota_grants::unexpired_totals(&app.pool, user, clock.now())
            .await
            .expect("grant totals"),
        (0, 0),
        "no unexpired grant at the advanced now"
    );
    assert_eq!(
        grant_row_count(&app, user).await,
        1,
        "expiry deletes nothing — the row stays, the read ignores it"
    );
    let token = login_token(&app, &email).await;
    let upload = seed_upload(&app, user, "image", None).await;
    let res = post_job(&app, &token, json!({ "upload_id": upload })).await;
    assert_eq!(res.status, StatusCode::TOO_MANY_REQUESTS, "{}", res.text());
    assert_eq!(problem_code(&res), "PF-3001");
    assert_eq!(quota_headers(&res), ("0".to_string(), "0".to_string()));
    // Expiry is not an event: no new audit row appears.
    let grants_after =
        audit_for_action(&app, "user", &user.to_string(), "admin.users.quota_granted").await;
    assert_eq!(
        grants_after.len(),
        1,
        "expiry writes no audit row — the trail still holds exactly the one grant"
    );
}

#[tokio::test]
async fn support_suite_audit_trail() {
    let (app, _) = support_suite_app().await;
    let user_email = unique_email("suite-audit-user");
    let user = register_user(&app, &user_email).await;
    let (support_id, support_token) =
        make_role(&app, &unique_email("suite-audit-support"), "support").await;
    let (admin_id, admin_token) =
        make_role(&app, &unique_email("suite-audit-admin"), "admin").await;
    let subscription = seed_subscription(&app, user, "pro").await;

    // A lookup hit audits (actor the support caller,
    // target the user, found true).
    let res = call(
        &app,
        "GET",
        &format!("/v1/admin/users/lookup?email={user_email}"),
        Some(&support_token),
        None,
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let lookups = audit_for_action(&app, "user", &user.to_string(), "admin.users.lookup").await;
    assert_eq!(lookups.len(), 1, "one row for the hit");
    assert_eq!(lookups[0].actor_user_id, Some(support_id));
    assert_eq!(lookups[0].after.as_ref().unwrap()["found"], true);

    // A lookup miss 404s AND audits (target null, found
    // false) — the access log shows the probe too. The
    // miss row has no user target, so it is counted by
    // actor: hit + miss is two lookup rows for this
    // support caller.
    let res = call(
        &app,
        "GET",
        "/v1/admin/users/lookup?email=nobody-here@example.com",
        Some(&support_token),
        None,
    )
    .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND, "{}", res.text());
    assert_eq!(problem_code(&res), "PF-2010");
    let total_lookups: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_logs WHERE action = 'admin.users.lookup' \
         AND actor_user_id = $1",
    )
    .bind(support_id)
    .fetch_one(&app.pool)
    .await
    .expect("count lookup rows");
    assert_eq!(
        total_lookups, 2,
        "hit + miss: two lookup rows for this support caller"
    );

    // A view read audits nothing: the trail for the user
    // is unchanged by opening the view twice.
    let before = audit_trail(&app, user).await.len();
    for _ in 0..2 {
        let res = call(
            &app,
            "GET",
            &format!("/v1/admin/users/{user}"),
            Some(&support_token),
            None,
        )
        .await;
        assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    }
    assert_eq!(
        audit_trail(&app, user).await.len(),
        before,
        "reads audit nothing"
    );

    // A grant + a note + a refund audit exactly one row
    // each, with the right actor and target.
    let res = call(
        &app,
        "POST",
        &format!("/v1/admin/users/{user}/quota-grant"),
        Some(&admin_token),
        Some(json!({ "images": 5, "ttl_hours": 24, "reason": "audit probe" })),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let res = call(
        &app,
        "POST",
        &format!("/v1/admin/users/{user}/notes"),
        Some(&admin_token),
        Some(json!({ "body": "audit probe note" })),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let res = call(
        &app,
        "POST",
        &format!("/v1/admin/billing/subscriptions/{subscription}/refund"),
        Some(&admin_token),
        Some(json!({ "reason": "audit probe refund" })),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let grants =
        audit_for_action(&app, "user", &user.to_string(), "admin.users.quota_granted").await;
    assert_eq!(grants.len(), 1);
    assert_eq!(grants[0].actor_user_id, Some(admin_id));
    let notes = audit_for_action(&app, "user", &user.to_string(), "admin.users.note_added").await;
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0].actor_user_id, Some(admin_id));
    let refunds = audit_for_action(
        &app,
        "subscription",
        &subscription.to_string(),
        "billing.refund.requested",
    )
    .await;
    assert_eq!(refunds.len(), 1);
    assert_eq!(refunds[0].actor_user_id, Some(admin_id));

    // A denied support write audits nothing and writes
    // nothing: the note list is unchanged.
    let res = call(
        &app,
        "POST",
        &format!("/v1/admin/users/{user}/notes"),
        Some(&support_token),
        Some(json!({ "body": "support must not write" })),
    )
    .await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.text());
    assert_eq!(
        audit_for_action(&app, "user", &user.to_string(), "admin.users.note_added")
            .await
            .len(),
        1,
        "a denied note append audits nothing"
    );
    assert_eq!(user_notes(&app, user).await.len(), 1, "and writes nothing");
}

#[tokio::test]
async fn support_suite_refund_guides_and_changes_nothing() {
    let (app, _) = support_suite_app().await;
    let (user, _) = authed_user(&app, "suite-refund-user").await;
    let (admin_id, admin_token) =
        make_role(&app, &unique_email("suite-refund-admin"), "admin").await;
    let _ = admin_id;
    let subscription = seed_subscription(&app, user, "pro").await;
    let before = subscriptions::by_id(&app.pool, subscription)
        .await
        .expect("load row")
        .expect("row exists");
    // Registration already enqueued the welcome mail, so
    // the refund's proof is that the count does NOT move.
    let mails_before = mails_for_user(&app, user).await;
    assert_eq!(mails_before, 1, "the welcome mail is the only mail so far");

    let res = call(
        &app,
        "POST",
        &format!("/v1/admin/billing/subscriptions/{subscription}/refund"),
        Some(&admin_token),
        Some(json!({ "reason": "duplicate charge, refund requested" })),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let body = res.json();
    assert_eq!(body["subscription_id"], subscription.to_string());
    assert_eq!(body["user_id"], user.to_string());
    assert_eq!(body["status"], "active");
    assert_eq!(body["plan_code"], "pro");
    // The guidance names the Stripe subscription page for
    // this row (the provider id is in the URL) and carries
    // the note verbatim.
    let url = body["refund_guidance"]["dashboard_url"].as_str().unwrap();
    assert!(
        url.starts_with("https://dashboard.stripe.com/subscriptions/sub_p111_"),
        "dashboard url names this subscription: {url}"
    );
    assert!(
        !body["refund_guidance"]["note"].as_str().unwrap().is_empty(),
        "the guidance carries the note"
    );
    // Four steps, all `done: false` — the API cannot know
    // what a human did in the dashboard.
    let checklist = body["checklist"].as_array().unwrap().clone();
    assert_eq!(checklist.len(), 4, "four guided steps");
    assert!(
        checklist.iter().all(|step| step["done"] == false),
        "no step claims to be done: {checklist:?}"
    );
    // Required fields: values where known, null where only
    // support knows.
    let fields = body["required_fields"].as_array().unwrap().clone();
    let field = |name: &str| {
        fields
            .iter()
            .find(|f| f["field"].as_str() == Some(name))
            .unwrap_or_else(|| panic!("field {name} present: {fields:?}"))
            .clone()
    };
    assert_eq!(field("subscription_id")["value"], subscription.to_string());
    assert_eq!(
        field("reason")["value"],
        "duplicate charge, refund requested"
    );
    assert!(
        field("amount")["value"].is_null(),
        "amount is null: {fields:?}"
    );
    assert!(
        field("invoice_or_payment_id")["value"].is_null(),
        "invoice id is null: {fields:?}"
    );

    // State after the refund: the row is byte-identical
    // (status still active), no mail was enqueued, exactly
    // one audit row exists.
    let after = subscriptions::by_id(&app.pool, subscription)
        .await
        .expect("load row")
        .expect("row exists");
    assert_eq!(after.status, before.status);
    assert_eq!(after.provider_customer_id, before.provider_customer_id);
    assert_eq!(
        after.provider_subscription_id,
        before.provider_subscription_id
    );
    assert_eq!(after.current_period_end, before.current_period_end);
    assert_eq!(
        mails_for_user(&app, user).await,
        mails_before,
        "the refund sends no mail"
    );
    let refunds = audit_for_action(
        &app,
        "subscription",
        &subscription.to_string(),
        "billing.refund.requested",
    )
    .await;
    assert_eq!(refunds.len(), 1, "one audit row per refund call");
    assert_eq!(
        refunds[0].reason.as_deref(),
        Some("duplicate charge, refund requested")
    );

    // A second call writes a second row — a note per
    // request, not idempotent.
    let res = call(
        &app,
        "POST",
        &format!("/v1/admin/billing/subscriptions/{subscription}/refund"),
        Some(&admin_token),
        Some(json!({ "reason": "duplicate charge, refund requested" })),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert_eq!(
        audit_for_action(
            &app,
            "subscription",
            &subscription.to_string(),
            "billing.refund.requested",
        )
        .await
        .len(),
        2,
        "a second refund writes a second row"
    );
}

#[tokio::test]
async fn support_suite_attestations_visible_to_support() {
    let (app, _) = support_suite_app().await;
    let (user, _) = authed_user(&app, "suite-burst-user").await;
    let (_, support_token) = make_role(&app, &unique_email("suite-burst-support"), "support").await;
    let burst = seed_velocity_burst(&app, user).await;

    // Support lists the attestations: the newest burst row
    // is flagged (velocity_count 11), the rest read 10..1,
    // unflagged. Rows are filtered by this user, so
    // parallel binaries cannot pollute the assertion.
    let res = call(
        &app,
        "GET",
        "/v1/admin/attestations?limit=100",
        Some(&support_token),
        None,
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let mine: Vec<Value> = res.json()["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| item["user_id"].as_str() == Some(&user.to_string()))
        .cloned()
        .collect();
    assert_eq!(mine.len(), 11, "the whole burst, for this user");
    assert_eq!(mine[0]["id"], burst[0].to_string(), "newest first");
    assert_eq!(mine[0]["velocity_count"], 11);
    assert_eq!(mine[0]["flagged"], true);
    assert!(
        !mine[0]["flag_reasons"].as_array().unwrap().is_empty(),
        "the flagged row names its reason"
    );
    assert_eq!(
        mine[0]["abuse_queue_url"],
        format!("/v1/admin/abuse-reports?user_id={user}")
    );
    for (i, item) in mine.iter().enumerate().skip(1) {
        assert_eq!(item["velocity_count"], 11 - i as i64, "burst row {}", i + 1);
        assert_eq!(item["flagged"], false, "burst row {}: {}", i + 1, item);
    }

    // `?flagged=true` returns only the flagged row for
    // this user — the abuse signal, visible to support.
    let res = call(
        &app,
        "GET",
        "/v1/admin/attestations?flagged=true&limit=100",
        Some(&support_token),
        None,
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let flagged_body = res.json();
    let flagged: Vec<&Value> = flagged_body["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| item["user_id"].as_str() == Some(&user.to_string()))
        .collect();
    assert_eq!(flagged.len(), 1, "exactly one flagged row for this user");
    assert_eq!(flagged[0]["id"], burst[0].to_string());
    assert!(flagged_body["items"]
        .as_array()
        .unwrap()
        .iter()
        .all(|item| item["flagged"] == true));
}
