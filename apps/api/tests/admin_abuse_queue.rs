//! P1-090: the abuse queue + actions — `GET
//! /v1/admin/abuse-reports` and `POST
//! /v1/admin/abuse-reports/{id}/resolve` (FR-C-06).
//!
//! The queue is global and the harness DB is shared and
//! persists across runs (the P1-088 precedent), so a list
//! assertion can never count the whole queue: every test
//! seeds its own rows and finds them by id. The pagination
//! test seeds its rows at `Utc::now() + 365 days` — one year
//! ahead of real time, so each run's rows are strictly
//! newer than every previous run's (the base moves with the
//! run) and the five seeded rows are the five newest in the
//! queue. The rows sit one second apart, so the 4 s span can
//! never overlap the next run's base (runs are minutes
//! apart). The SLA test freezes its own clock instead (the
//! breach check for an unresolved report reads the app
//! clock), and finds its rows by id across pages.
//!
//! - `admin_abuse_queue_guard` — the extractor + matrix
//!   guard: anonymous 401, plain user 403, an admin user's
//!   own API key 403 ("no admin ever"), support 200 on the
//!   list and 403 on the resolve, admin 200 on both.
//! - `admin_abuse_queue_filters_and_pagination` — the
//!   status filter, the cursor round-trip at limit 2 (the
//!   walk stops once all five seeded ids are seen — the
//!   queue never ends after them, older rows sit below),
//!   the tampered cursor 422, the unknown status 422, and
//!   the limit clamp.
//! - `admin_abuse_queue_sla_fields` — every item carries
//!   `sla_due_at` (`created_at` + 72 h) and `sla_breached`
//!   (open + past due, open + fresh, resolved on time,
//!   resolved late).
//! - `admin_abuse_resolve_dismiss_audited` — the AC:
//!   dismiss resolves the report and writes exactly one
//!   `admin.abuse_reports.resolved` audit row (actor the
//!   admin, `after.action` `dismiss`, the reason); a second
//!   dismiss is a 200 no-op with no second row.
//! - `admin_abuse_resolve_reject_job` — the reported job's
//!   status becomes `rejected`, the report resolves as
//!   `job_rejected`, the audit row carries the job and its
//!   status before.
//! - `admin_abuse_resolve_suspend_user` — the reported user
//!   is suspended through the full P1-008 sequence (queued
//!   jobs held), the report resolves as `user_suspended`,
//!   and BOTH audit rows land: `user.suspended` and the
//!   resolution row.
//! - `admin_abuse_resolve_start_review` — open → reviewing,
//!   audited as `abuse.report_reviewing`; a second pickup
//!   is a 200 no-op with no second row.
//! - `admin_abuse_resolve_errors` — unknown report 404,
//!   `reject_job` on a jobless report 422, `suspend_user`
//!   on a userless report 422, a missing reason 422, an
//!   unknown action 422.

mod common;

use axum::http::{Method, Request, StatusCode};
use chrono::{DateTime, Duration, TimeZone, Utc};
use common::{TestApp, TestFixtures};
use detag_core::clock::{Clock, ManualClock};
use detag_core::pagination::Cursor;
use detag_core::repos::jobs::NewJob;
use detag_core::repos::{abuse_reports, jobs};
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

/// A per-request XFF address: each request gets its own IP
/// bucket, so the tests never trip each other's rate limits
/// (the P1-003 lesson; the full-UUID form, FLAKY-006).
fn unique_xff() -> String {
    format!("203.0.113.{}", Uuid::now_v7())
}

/// App on the frozen clock. No billing provider: the
/// endpoints never call one.
async fn abuse_queue_app() -> TestApp {
    let fixtures = TestFixtures::from_env().await;
    let mut settings = fixtures.settings();
    settings.disposable_email_domains = "mailinator.com".to_string();
    settings.tos_current_version = "v1".to_string();
    settings.api_key_rate_limit_burst = 1000;
    settings.api_key_rate_limit_refill_per_min = 1000;
    let clock = Arc::new(ManualClock::new(frozen_now()));
    TestApp::spawn_with_fixtures_settings_and_clock(
        fixtures,
        settings,
        Arc::clone(&clock) as Arc<dyn Clock>,
    )
    .await
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
        "name": "abuse queue probe",
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

/// Seed one report directly (the P1-088 precedent — no
/// production caller files reports except through the
/// service, and the queue tests need explicit timestamps
/// and statuses). Returns the id.
async fn seed_report(
    app: &TestApp,
    user_id: Option<Uuid>,
    job_id: Option<Uuid>,
    status: &str,
    created_at: DateTime<Utc>,
) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO abuse_reports (user_id, reporter_contact, job_id, reason, status, created_at) \
         VALUES ($1, 'reporter@example.com', $2, 'P1-090 probe report', $3, $4) RETURNING id",
    )
    .bind(user_id)
    .bind(job_id)
    .bind(status)
    .bind(created_at)
    .fetch_one(&app.pool)
    .await
    .expect("seed abuse report")
}

/// Seed one queued image job for a fresh user; return
/// (user id, job id). A job's `upload_id` is nullable, so
/// no upload is needed (the P1-089 precedent).
async fn seed_job(app: &TestApp) -> (Uuid, Uuid) {
    let user = app.seed_user_unique().await;
    let job = jobs::create(
        &app.pool,
        &NewJob {
            user_id: user.id,
            upload_id: None,
            kind: "image",
            mode: "inpaint",
            status: "queued",
            lane: "standard",
            params: json!({}),
            idempotency_key: Some(&format!("p090-{}", Uuid::now_v7())),
            key_id: None,
        },
    )
    .await
    .expect("seed job");
    (user.id, job.id)
}

/// The job's status, read back directly.
async fn job_status(app: &TestApp, job_id: Uuid) -> String {
    sqlx::query_scalar("SELECT status FROM jobs WHERE id = $1")
        .bind(job_id)
        .fetch_one(&app.pool)
        .await
        .expect("job status")
}

/// The user's status, read back directly.
async fn user_status(app: &TestApp, user_id: Uuid) -> String {
    sqlx::query_scalar("SELECT status FROM users WHERE id = $1")
        .bind(user_id)
        .fetch_one(&app.pool)
        .await
        .expect("user status")
}

/// The audit rows for one action on one target, newest first.
async fn audit_rows(app: &TestApp, action: &str, target_id: &str) -> Vec<serde_json::Value> {
    sqlx::query_scalar(
        "SELECT \"after\" FROM audit_logs WHERE action = $1 AND target_id = $2 ORDER BY created_at DESC",
    )
    .bind(action)
    .bind(target_id)
    .fetch_all(&app.pool)
    .await
    .expect("audit rows")
}

/// GET /v1/admin/abuse-reports with the raw query string
/// and the given bearer token (or none for the anonymous
/// probe).
async fn get_queue(app: &TestApp, query: &str, token: Option<&str>) -> common::TestResponse {
    let uri = format!("/v1/admin/abuse-reports{query}");
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

/// POST /v1/admin/abuse-reports/{id}/resolve with the given
/// body and bearer token (or none for the anonymous probe).
async fn post_resolve(
    app: &TestApp,
    report_id: Uuid,
    body: serde_json::Value,
    token: Option<&str>,
) -> common::TestResponse {
    let uri = format!("/v1/admin/abuse-reports/{report_id}/resolve");
    let mut req = Request::builder()
        .method(Method::POST)
        .uri(uri)
        .header("x-request-id", REQUEST_ID)
        .header("x-forwarded-for", unique_xff())
        .header("content-type", "application/json");
    if let Some(token) = token {
        req = req.header("authorization", format!("Bearer {token}"));
    }
    app.request(
        req.body(axum::body::Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap(),
    )
    .await
}

/// The ids of a page's items, in order.
fn item_ids(body: &serde_json::Value) -> Vec<String> {
    body["items"]
        .as_array()
        .expect("items is an array")
        .iter()
        .map(|item| item["id"].as_str().unwrap().to_string())
        .collect()
}

/// Walk the whole queue (limit 100 pages) and return the
/// item with this id, if it is listed at all.
async fn find_in_queue(app: &TestApp, token: &str, id: Uuid) -> Option<serde_json::Value> {
    let mut cursor: Option<String> = None;
    for _ in 0..10 {
        let query = match &cursor {
            Some(c) => format!("?limit=100&cursor={c}"),
            None => "?limit=100".to_string(),
        };
        let res = get_queue(app, &query, Some(token)).await;
        assert_eq!(res.status, StatusCode::OK, "{}", res.text());
        let body = res.json();
        for item in body["items"].as_array().unwrap() {
            if item["id"].as_str() == Some(id.to_string().as_str()) {
                return Some(item.clone());
            }
        }
        cursor = body
            .get("next_cursor")
            .and_then(|c| c.as_str().map(String::from));
        if cursor.is_none() {
            break;
        }
    }
    None
}

#[tokio::test]
async fn admin_abuse_queue_guard() {
    let app = abuse_queue_app().await;
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
    let report_id = seed_report(&app, None, None, "open", frozen_now()).await;

    // The list: anonymous 401, plain user 403, an admin
    // user's own API key 403 ("no admin ever"), support 200,
    // admin 200.
    let res = get_queue(&app, "", None).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-1001");
    let res = get_queue(&app, "", Some(&user_token)).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-1002");
    let res = get_queue(&app, "", Some(&admin_key)).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-1002");
    let res = get_queue(&app, "", Some(&support_token)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let res = get_queue(&app, "", Some(&admin_token)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());

    // The resolve: support is read-only — 403, not a 200
    // that dismisses the report (the report must still be
    // open afterwards).
    let body = json!({ "action": "dismiss", "reason": "guard probe" });
    let res = post_resolve(&app, report_id, body.clone(), Some(&support_token)).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-1002");
    let row = abuse_reports::by_id(&app.pool, report_id)
        .await
        .expect("read back")
        .expect("report exists");
    assert_eq!(row.status, "open", "a denied resolve must not resolve");
    // Anonymous / plain user / key: 401 / 403 / 403.
    let res = post_resolve(&app, report_id, body.clone(), None).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());
    let res = post_resolve(&app, report_id, body.clone(), Some(&user_token)).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.text());
    let res = post_resolve(&app, report_id, body, Some(&admin_key)).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.text());
}

#[tokio::test]
async fn admin_abuse_queue_filters_and_pagination() {
    let app = abuse_queue_app().await;
    let (_admin_id, admin_token) = make_role(
        &app,
        &format!("page-admin-{}@example.com", Uuid::now_v7()),
        "admin",
    )
    .await;
    // Five rows, one second apart, one year ahead of real
    // time — the five newest in the queue, so the pages
    // below hold exactly these rows (see the module docs).
    // Seconds, not minutes: the rows span 4 s, and runs are
    // minutes apart, so a previous run's rows can never
    // interleave with this run's (a 4-minute span overlapped
    // the next run's base — the first version of this test
    // failed exactly on that).
    let base = Utc::now() + Duration::days(365);
    let t = |seconds: i64| base + Duration::seconds(seconds);
    let ids: Vec<Uuid> = vec![
        seed_report(&app, None, None, "open", t(0)).await,
        seed_report(&app, None, None, "reviewing", t(1)).await,
        seed_report(&app, None, None, "resolved", t(2)).await,
        seed_report(&app, None, None, "open", t(3)).await,
        seed_report(&app, None, None, "reviewing", t(4)).await,
    ];
    // The resolved seed needs its stamps for the SLA shape
    // (a resolved row without resolved_at would breach by
    // the clock — the seed below is the honest one: it was
    // resolved on time).
    sqlx::query(
        "UPDATE abuse_reports SET resolution = 'dismissed', resolved_at = $2, resolved_by = $3 WHERE id = $1",
    )
    .bind(ids[2])
    .bind(t(2) + Duration::hours(1))
    .bind(_admin_id)
    .execute(&app.pool)
    .await
    .expect("stamp resolved seed");

    // The unfiltered list at limit 5: exactly the five
    // seeded rows, newest first.
    let res = get_queue(&app, "?limit=5", Some(&admin_token)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let body = res.json();
    assert_eq!(
        item_ids(&body),
        ids.iter()
            .rev()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        "newest first: {body}"
    );
    assert!(
        body.get("next_cursor").is_some(),
        "more rows follow: {body}"
    );
    // The SLA pair rides on every item: due is filing +
    // 72 h, and nothing seeded a year out is breached.
    // (Compare at microsecond precision — Postgres stores
    // TIMESTAMPTZ to the microsecond, so the row's due time
    // drops the test value's nanoseconds on the round-trip.)
    let first = &body["items"][0];
    assert_eq!(first["status"], "reviewing");
    assert_eq!(
        first["sla_due_at"]
            .as_str()
            .expect("sla_due_at is a string")
            .parse::<DateTime<Utc>>()
            .expect("sla_due_at parses as RFC 3339")
            .timestamp_micros(),
        (t(4) + Duration::hours(72)).timestamp_micros(),
        "sla_due_at is filing + 72h: {first}"
    );
    assert_eq!(first["sla_breached"], false, "{first}");

    // The status filter: the two open rows, newest first.
    let res = get_queue(&app, "?status=open&limit=100", Some(&admin_token)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let body = res.json();
    let open: Vec<String> = item_ids(&body)
        .into_iter()
        .filter(|id| ids.contains(&id.parse().unwrap()))
        .collect();
    assert_eq!(
        open,
        [ids[3].to_string(), ids[0].to_string()],
        "open filter, newest first: {body}"
    );

    // The cursor round-trip at limit 2: the pages walk
    // newest-first with no repeats and no drops, and the
    // first five ids seen are exactly the seeded rows.
    // (The harness DB is shared — older rows from other
    // tests and earlier runs sit below the seeded five, so
    // the walk does NOT end after three pages and the last
    // page of mine is not the last page of the queue: the
    // walk stops once all five seeded ids are seen, and no
    // "no cursor on the last page" is asserted.)
    let mut seen: Vec<String> = Vec::new();
    let mut cursor: Option<String> = None;
    for page in 0..6 {
        let query = match &cursor {
            Some(c) => format!("?limit=2&cursor={c}"),
            None => "?limit=2".to_string(),
        };
        let res = get_queue(&app, &query, Some(&admin_token)).await;
        assert_eq!(res.status, StatusCode::OK, "page {page}: {}", res.text());
        let body = res.json();
        let items = body["items"].as_array().expect("items is an array");
        assert_eq!(items.len(), 2, "page {page} size: {body}");
        for item in items {
            let id = item["id"].as_str().unwrap().to_string();
            assert!(!seen.contains(&id), "no repeats across pages: {id}");
            seen.push(id);
        }
        let mine: Vec<&String> = seen
            .iter()
            .filter(|id| ids.iter().any(|seed| &seed.to_string() == *id))
            .collect();
        if mine.len() == 5 {
            break;
        }
        cursor = Some(
            body.get("next_cursor")
                .and_then(|c| c.as_str().map(String::from))
                .expect("a page before the last seeded row carries a next cursor"),
        );
    }
    assert_eq!(
        seen[..5],
        ids.iter()
            .rev()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        "pages concatenate to the five seeded rows"
    );

    // An unknown status is 422 PF-2002, not an empty page.
    let res = get_queue(&app, "?status=archived", Some(&admin_token)).await;
    assert_eq!(
        res.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        res.text()
    );
    assert_eq!(res.json()["code"], "PF-2002");
    // A tampered cursor is 422 PF-2002.
    let res = get_queue(&app, "?cursor=not-a-cursor", Some(&admin_token)).await;
    assert_eq!(
        res.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        res.text()
    );
    assert_eq!(res.json()["code"], "PF-2002");
    // A well-formed cursor with an out-of-range `ts` is
    // 422, never a 500 (the P1-088 lesson).
    let out_of_range = Cursor {
        ts: i64::MAX,
        id: Uuid::now_v7(),
    }
    .encode();
    let res = get_queue(&app, &format!("?cursor={out_of_range}"), Some(&admin_token)).await;
    assert_eq!(
        res.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        res.text()
    );
    assert_eq!(res.json()["code"], "PF-2002");

    // The limit clamps: 0 means 1 (a page of one, with a
    // next cursor); 500 means 100, so all 5 seeded rows fit
    // on the first page.
    let res = get_queue(&app, "?limit=0", Some(&admin_token)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let body = res.json();
    assert_eq!(body["items"].as_array().unwrap().len(), 1, "{body}");
    assert!(body.get("next_cursor").is_some(), "limit 0 → 1: {body}");
    let res = get_queue(&app, "?limit=500", Some(&admin_token)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let body = res.json();
    assert_eq!(
        item_ids(&body)[..5],
        ids.iter()
            .rev()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        "limit 500 → the five seeded rows first: {body}"
    );
}

#[tokio::test]
async fn admin_abuse_queue_sla_fields() {
    let app = abuse_queue_app().await;
    let (admin_id, admin_token) = make_role(
        &app,
        &format!("sla-admin-{}@example.com", Uuid::now_v7()),
        "admin",
    )
    .await;
    let now = frozen_now();
    // Filed 4 days ago, still open: due 1 day ago —
    // breached.
    let old_open = seed_report(&app, None, None, "open", now - Duration::days(4)).await;
    // Filed 1 hour ago, still open: due in 71 h — fresh.
    // Seeded off the REAL clock, not the frozen one: the
    // resolve stamps `resolved_at` from the DB clock (real
    // time), so a frozen seed's due time (frozen + 71 h =
    // 2026-11-22T11:00:00Z) passed and the "resolved on
    // time" assertion below became a deterministic failure
    // — a time bomb, not a flake (attempt 4 died on it).
    // Seeding off real now keeps ~71 h of headroom forever,
    // and the row still sits below the pagination test's
    // year-ahead seeds in the shared harness DB.
    let fresh_open = seed_report(&app, None, None, "open", Utc::now() - Duration::hours(1)).await;

    let item = find_in_queue(&app, &admin_token, old_open)
        .await
        .expect("old open report is listed");
    assert_eq!(
        item["sla_due_at"],
        (now - Duration::days(4) + Duration::hours(72))
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string(),
        "due is filing + 72h: {item}"
    );
    assert_eq!(item["sla_breached"], true, "open + past due: {item}");
    assert!(item["resolved_at"].is_null(), "{item}");

    let item = find_in_queue(&app, &admin_token, fresh_open)
        .await
        .expect("fresh open report is listed");
    assert_eq!(item["sla_breached"], false, "open + fresh: {item}");

    // Resolving the old report now (the DB clock stamps
    // `resolved_at` at real time — past its due time):
    // breached, and the response carries the stamps.
    let res = post_resolve(
        &app,
        old_open,
        json!({ "action": "dismiss", "reason": "SLA probe, late" }),
        Some(&admin_token),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let body = res.json();
    assert_eq!(body["status"], "resolved");
    assert_eq!(body["resolution"], "dismissed");
    assert_eq!(body["sla_breached"], true, "resolved late: {body}");
    assert!(body["resolved_at"].is_string(), "{body}");
    assert_eq!(body["resolved_by"], admin_id.to_string(), "{body}");

    // Resolving the fresh report now: on time.
    let res = post_resolve(
        &app,
        fresh_open,
        json!({ "action": "dismiss", "reason": "SLA probe, on time" }),
        Some(&admin_token),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let body = res.json();
    assert_eq!(body["sla_breached"], false, "resolved on time: {body}");
}

#[tokio::test]
async fn admin_abuse_resolve_dismiss_audited() {
    let app = abuse_queue_app().await;
    let (admin_id, admin_token) = make_role(
        &app,
        &format!("dismiss-admin-{}@example.com", Uuid::now_v7()),
        "admin",
    )
    .await;
    let report_id = seed_report(&app, None, None, "open", frozen_now()).await;

    // The AC: dismiss resolves the report and the action is
    // audited — exactly one resolution row, actor the admin,
    // the action and the reason on it.
    let res = post_resolve(
        &app,
        report_id,
        json!({ "action": "dismiss", "reason": "Not a violation.", "notes": "Checked the job." }),
        Some(&admin_token),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let body = res.json();
    assert_eq!(body["status"], "resolved", "{body}");
    assert_eq!(body["resolution"], "dismissed", "{body}");
    assert_eq!(body["notes"], "Checked the job.", "{body}");
    assert!(body["resolved_at"].is_string(), "{body}");
    assert_eq!(body["resolved_by"], admin_id.to_string(), "{body}");

    let rows = audit_rows(&app, "admin.abuse_reports.resolved", &report_id.to_string()).await;
    assert_eq!(rows.len(), 1, "exactly one resolution row");
    assert_eq!(rows[0]["action"], "dismiss");
    assert_eq!(rows[0]["resolution"], "dismissed");
    // The actor is proven through the audit_logs row
    // itself (the `after` select above carries no actor).
    let actor: Option<Uuid> = sqlx::query_scalar(
        "SELECT actor_user_id FROM audit_logs WHERE action = $1 AND target_id = $2",
    )
    .bind("admin.abuse_reports.resolved")
    .bind(report_id.to_string())
    .fetch_one(&app.pool)
    .await
    .expect("actor");
    assert_eq!(actor, Some(admin_id), "actor is the admin");
    let reason: Option<String> =
        sqlx::query_scalar("SELECT reason FROM audit_logs WHERE action = $1 AND target_id = $2")
            .bind("admin.abuse_reports.resolved")
            .bind(report_id.to_string())
            .fetch_one(&app.pool)
            .await
            .expect("reason");
    assert_eq!(reason.as_deref(), Some("Not a violation."));

    // A second dismiss is a 200 no-op: the report as it
    // stands, no second audit row.
    let res = post_resolve(
        &app,
        report_id,
        json!({ "action": "dismiss", "reason": "Again." }),
        Some(&admin_token),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert_eq!(res.json()["status"], "resolved");
    let rows = audit_rows(&app, "admin.abuse_reports.resolved", &report_id.to_string()).await;
    assert_eq!(rows.len(), 1, "the no-op audits nothing");
}

#[tokio::test]
async fn admin_abuse_resolve_reject_job() {
    let app = abuse_queue_app().await;
    let (_admin_id, admin_token) = make_role(
        &app,
        &format!("reject-admin-{}@example.com", Uuid::now_v7()),
        "admin",
    )
    .await;
    let (_user_id, job_id) = seed_job(&app).await;
    let report_id = seed_report(&app, None, Some(job_id), "reviewing", frozen_now()).await;

    let res = post_resolve(
        &app,
        report_id,
        json!({ "action": "reject_job", "reason": "Confirmed infringement." }),
        Some(&admin_token),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let body = res.json();
    assert_eq!(body["status"], "resolved", "{body}");
    assert_eq!(body["resolution"], "job_rejected", "{body}");
    assert_eq!(
        job_status(&app, job_id).await,
        "rejected",
        "the job is rejected"
    );

    let rows = audit_rows(&app, "admin.abuse_reports.resolved", &report_id.to_string()).await;
    assert_eq!(rows.len(), 1, "exactly one resolution row");
    assert_eq!(rows[0]["action"], "reject_job");
    assert_eq!(rows[0]["job_id"], job_id.to_string());
    assert_eq!(rows[0]["job_status_before"], "queued");
    assert_eq!(rows[0]["job_status"], "rejected");
}

#[tokio::test]
async fn admin_abuse_resolve_suspend_user() {
    let app = abuse_queue_app().await;
    let (_admin_id, admin_token) = make_role(
        &app,
        &format!("suspend-admin-{}@example.com", Uuid::now_v7()),
        "admin",
    )
    .await;
    // The spike's user, with one queued job to hold.
    let (user_id, job_id) = seed_job(&app).await;
    let report_id = seed_report(&app, Some(user_id), None, "open", frozen_now()).await;

    let res = post_resolve(
        &app,
        report_id,
        json!({ "action": "suspend_user", "reason": "Velocity abuse confirmed." }),
        Some(&admin_token),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let body = res.json();
    assert_eq!(body["status"], "resolved", "{body}");
    assert_eq!(body["resolution"], "user_suspended", "{body}");
    assert_eq!(
        user_status(&app, user_id).await,
        "suspended",
        "the user is suspended"
    );
    assert_eq!(
        job_status(&app, job_id).await,
        "held",
        "the queued job is held"
    );

    // BOTH audit rows land: the suspension's own row and
    // the resolution row.
    let rows = audit_rows(&app, "user.suspended", &user_id.to_string()).await;
    assert_eq!(rows.len(), 1, "exactly one suspension row");
    assert_eq!(rows[0]["status"], "suspended");
    assert_eq!(rows[0]["sessions_revoked"], 0);
    assert_eq!(rows[0]["jobs_held"], 1);
    let rows = audit_rows(&app, "admin.abuse_reports.resolved", &report_id.to_string()).await;
    assert_eq!(rows.len(), 1, "exactly one resolution row");
    assert_eq!(rows[0]["action"], "suspend_user");
    assert_eq!(rows[0]["user_id"], user_id.to_string());
    assert_eq!(rows[0]["already_suspended"], false);
    assert_eq!(rows[0]["jobs_held"], 1);
}

#[tokio::test]
async fn admin_abuse_resolve_start_review() {
    let app = abuse_queue_app().await;
    let (_admin_id, admin_token) = make_role(
        &app,
        &format!("review-admin-{}@example.com", Uuid::now_v7()),
        "admin",
    )
    .await;
    let report_id = seed_report(&app, None, None, "open", frozen_now()).await;

    let res = post_resolve(
        &app,
        report_id,
        json!({ "action": "start_review", "reason": "Picking this up." }),
        Some(&admin_token),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let body = res.json();
    assert_eq!(body["status"], "reviewing", "{body}");
    assert!(body["resolution"].is_null(), "not resolved: {body}");
    assert!(body["resolved_at"].is_null(), "not resolved: {body}");

    let rows = audit_rows(&app, "abuse.report_reviewing", &report_id.to_string()).await;
    assert_eq!(rows.len(), 1, "exactly one pickup row");
    assert_eq!(rows[0]["status"], "reviewing");
    // A second pickup is a 200 no-op with no second row.
    let res = post_resolve(
        &app,
        report_id,
        json!({ "action": "start_review", "reason": "Again." }),
        Some(&admin_token),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert_eq!(res.json()["status"], "reviewing");
    let rows = audit_rows(&app, "abuse.report_reviewing", &report_id.to_string()).await;
    assert_eq!(rows.len(), 1, "the no-op audits nothing");
}

#[tokio::test]
async fn admin_abuse_resolve_errors() {
    let app = abuse_queue_app().await;
    let (_admin_id, admin_token) = make_role(
        &app,
        &format!("err-admin-{}@example.com", Uuid::now_v7()),
        "admin",
    )
    .await;
    // An unknown report id is 404 PF-2010.
    let ghost = Uuid::now_v7();
    let res = post_resolve(
        &app,
        ghost,
        json!({ "action": "dismiss", "reason": "Ghost." }),
        Some(&admin_token),
    )
    .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-2010");

    // A report with no job cannot reject a job (422).
    let jobless = seed_report(&app, None, None, "open", frozen_now()).await;
    let res = post_resolve(
        &app,
        jobless,
        json!({ "action": "reject_job", "reason": "No job here." }),
        Some(&admin_token),
    )
    .await;
    assert_eq!(
        res.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        res.text()
    );
    assert_eq!(res.json()["code"], "PF-2002");
    // A report with no user cannot suspend a user (422).
    let res = post_resolve(
        &app,
        jobless,
        json!({ "action": "suspend_user", "reason": "No user here." }),
        Some(&admin_token),
    )
    .await;
    assert_eq!(
        res.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        res.text()
    );
    assert_eq!(res.json()["code"], "PF-2002");
    // A missing reason is 422 with the field list.
    let res = post_resolve(
        &app,
        jobless,
        json!({ "action": "dismiss", "reason": "   " }),
        Some(&admin_token),
    )
    .await;
    assert_eq!(
        res.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        res.text()
    );
    assert_eq!(res.json()["code"], "PF-2002");
    // An unknown action is 422 (serde rejects it before
    // the handler runs).
    let res = post_resolve(
        &app,
        jobless,
        json!({ "action": "nuke", "reason": "Nope." }),
        Some(&admin_token),
    )
    .await;
    assert_eq!(
        res.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        res.text()
    );
    assert_eq!(res.json()["code"], "PF-2002");
    // The report is still open after all four rejections.
    let row = abuse_reports::by_id(&app.pool, jobless)
        .await
        .expect("read back")
        .expect("report exists");
    assert_eq!(row.status, "open");
}
