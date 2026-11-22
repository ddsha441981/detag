//! P1-088: the audit query API — `GET /v1/admin/audit`
//! (filters + cursor pagination, FR-C-03).
//!
//! The trail is append-only and the tests seed rows
//! directly (the P1-081 precedent — no production caller
//! writes audit rows except through the service, and the
//! service stamps `created_at` from the DB clock, so a
//! boundary test needs explicit timestamps). Every seeded
//! row carries a dedicated actor UUID that never registers,
//! so the register flow's own audit rows
//! (`register.attempt` / `register.success`, written for
//! every test user) can never leak into a filtered view —
//! and every list assertion filters by that actor (or by
//! action/target), never by an unfiltered global count:
//! the harness DB is shared and persists across runs.
//!
//! - `admin_audit_guard` — the extractor + matrix guard:
//!   anonymous 401, plain user 403, an admin user's own
//!   API key 403 ("no admin ever"), support 200, admin 200.
//! - `admin_audit_filters_and_pagination` — the AC: the
//!   actor / action / target / from-to filters (each exact,
//!   combined with AND), the cursor round-trip over 5 rows
//!   at limit 2 (2 + 2 + 1, no repeats, no drops, newest
//!   first), the half-open `[from, to)` window (a row
//!   exactly at `to` is excluded, a row exactly at `from`
//!   is included), the tampered cursor 422, the malformed
//!   uuid / from 422s, and the limit clamp.
//! - `admin_audit_empty_page` — an unknown actor id is an
//!   empty page (`items: []`, no `next_cursor`), not a 404.
//! - `admin_audit_read_writes_no_audit_row` — a read
//!   creates nothing durable (P1-046): the row count for
//!   the filtered actor is identical before and after the
//!   GET.

mod common;

use axum::http::{Method, Request, StatusCode};
use chrono::{DateTime, TimeZone, Utc};
use common::{TestApp, TestFixtures};
use detag_core::clock::{Clock, ManualClock};
use detag_core::pagination::Cursor;
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

/// The seed base: 2026-11-19 10:00 UTC, two hours before
/// the frozen now. Five rows, one minute apart.
fn t(minutes: i64) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 11, 19, 10, 0, 0)
        .unwrap()
        .checked_add_signed(chrono::Duration::minutes(minutes))
        .unwrap()
}

/// A per-request XFF address: each request gets its own IP
/// bucket, so the tests never trip each other's rate limits
/// (the P1-003 lesson; the full-UUID form, FLAKY-006).
fn unique_xff() -> String {
    format!("203.0.113.{}", Uuid::now_v7())
}

/// App on the frozen clock. No billing provider: the
/// endpoint never calls one.
async fn admin_audit_app() -> TestApp {
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
        "name": "admin audit probe",
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

/// One seeded audit row: (action, target_type, target_id,
/// created_at). The actor is the caller's dedicated UUID;
/// `before`/`after` carry the suspend shape on the first
/// row only (the verbatim round-trip proof).
async fn seed_rows(app: &TestApp, actor: Uuid, rows: &[(&str, &str, &str, DateTime<Utc>)]) {
    for (i, (action, target_type, target_id, created_at)) in rows.iter().enumerate() {
        let (before, after) = if i == 0 {
            (
                Some(json!({"status": "active"})),
                Some(json!({"status": "suspended"})),
            )
        } else {
            (None, None)
        };
        sqlx::query(
            "INSERT INTO audit_logs (actor_user_id, actor_client, action, target_type, \
             target_id, \"before\", \"after\", reason, created_at) \
             VALUES ($1, 'admin', $2, $3, $4, $5, $6, 'seeded for P1-088', $7)",
        )
        .bind(actor)
        .bind(*action)
        .bind(*target_type)
        .bind(*target_id)
        .bind(before)
        .bind(after)
        .bind(*created_at)
        .execute(&app.pool)
        .await
        .expect("seed audit row");
    }
}

/// How many rows the dedicated actor has (the P1-046
/// read-writes-nothing proof counts this before/after).
async fn count_for_actor(app: &TestApp, actor: Uuid) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM audit_logs WHERE actor_user_id = $1")
        .bind(actor)
        .fetch_one(&app.pool)
        .await
        .expect("count audit rows")
}

/// GET /v1/admin/audit with the raw query string and the
/// given bearer token (or none for the anonymous probe).
async fn get_audit(app: &TestApp, query: &str, token: Option<&str>) -> common::TestResponse {
    let uri = format!("/v1/admin/audit{query}");
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

/// The action names of a page's items, in order.
fn actions(body: &serde_json::Value) -> Vec<String> {
    body["items"]
        .as_array()
        .expect("items is an array")
        .iter()
        .map(|item| item["action"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn admin_audit_guard() {
    let app = admin_audit_app().await;
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

    // Anonymous: 401 PF-1001.
    let res = get_audit(&app, "", None).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-1001");
    // A verified plain user: 403 PF-1002.
    let res = get_audit(&app, "", Some(&user_token)).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-1002");
    // An admin user's own API key: 403 PF-1002 ("no admin
    // ever").
    let res = get_audit(&app, "", Some(&admin_key)).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-1002");
    // Support (read-only role): 200.
    let res = get_audit(&app, "", Some(&support_token)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    // Admin: 200.
    let res = get_audit(&app, "", Some(&admin_token)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
}

#[tokio::test]
async fn admin_audit_filters_and_pagination() {
    let app = admin_audit_app().await;
    let (_admin_id, admin_token) = make_role(
        &app,
        &format!("ac-admin-{}@example.com", Uuid::now_v7()),
        "admin",
    )
    .await;
    // The dedicated actor (never registers — no register
    // rows carry it) + a second actor whose rows must never
    // leak into the first actor's views.
    let actor = Uuid::now_v7();
    let other = Uuid::now_v7();
    // Five rows, one minute apart, newest first t(4)..t(0):
    // two `user.suspended` on user-1/user-2, one
    // `billing.subscription.created`, one `user.restored` on
    // user-1, one more `user.suspended` on user-3.
    seed_rows(
        &app,
        actor,
        &[
            ("user.suspended", "user", "user-1", t(0)),
            (
                "billing.subscription.created",
                "subscription",
                "sub-1",
                t(1),
            ),
            ("user.suspended", "user", "user-2", t(2)),
            ("user.restored", "user", "user-1", t(3)),
            ("user.suspended", "user", "user-3", t(4)),
        ],
    )
    .await;
    seed_rows(
        &app,
        other,
        &[
            ("user.suspended", "user", "user-9", t(1)),
            ("user.suspended", "user", "user-9", t(3)),
        ],
    )
    .await;

    // The actor filter: all 5 rows, newest first, no leak
    // from the other actor (its two `user.suspended` rows
    // would show as a third and fourth if they leaked).
    let res = get_audit(&app, &format!("?actor={actor}"), Some(&admin_token)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let body = res.json();
    assert_eq!(
        actions(&body),
        [
            "user.suspended",
            "user.restored",
            "user.suspended",
            "billing.subscription.created",
            "user.suspended",
        ],
        "newest first, actor-filtered: {body}"
    );
    assert!(body.get("next_cursor").is_none(), "one page: {body}");
    // The first row's payloads round-trip verbatim.
    let first = &body["items"][0];
    assert_eq!(first["target_id"], "user-3");
    let last = &body["items"][4];
    assert_eq!(last["target_id"], "user-1");
    assert_eq!(last["before"], json!({"status": "active"}));
    assert_eq!(last["after"], json!({"status": "suspended"}));
    assert_eq!(last["reason"], "seeded for P1-088");
    assert_eq!(last["actor_client"], "admin");

    // The action filter (combined with the actor filter):
    // the three `user.suspended` rows, newest first.
    let res = get_audit(
        &app,
        &format!("?actor={actor}&action=user.suspended"),
        Some(&admin_token),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let body = res.json();
    assert_eq!(body["items"].as_array().unwrap().len(), 3, "{body}");
    assert_eq!(
        body["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["target_id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["user-3", "user-2", "user-1"],
        "action filter, newest first: {body}"
    );

    // The target filter: both rows targeting user-1 (the
    // suspend at t(0) and the restore at t(3)).
    let res = get_audit(
        &app,
        &format!("?actor={actor}&target=user-1"),
        Some(&admin_token),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let body = res.json();
    assert_eq!(
        actions(&body),
        ["user.restored", "user.suspended"],
        "target filter: {body}"
    );

    // The from/to window, half-open `[from, to)`: from t(1)
    // inclusive, to t(3) exclusive — the t(1) and t(2)
    // rows; the t(3) row is excluded (it belongs to the
    // next window) and the t(0) row is before `from`.
    let res = get_audit(
        &app,
        &format!(
            "?actor={actor}&from={}&to={}",
            t(1).format("%Y-%m-%dT%H:%M:%SZ"),
            t(3).format("%Y-%m-%dT%H:%M:%SZ")
        ),
        Some(&admin_token),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let body = res.json();
    assert_eq!(
        actions(&body),
        ["user.suspended", "billing.subscription.created"],
        "half-open window [t(1), t(3)): {body}"
    );

    // The cursor round-trip at limit 2: 2 + 2 + 1, no
    // repeats, no drops — the three pages concatenate to
    // the full newest-first list.
    let mut seen: Vec<String> = Vec::new();
    let mut cursor: Option<String> = None;
    for page in 0..3 {
        let query = match &cursor {
            Some(c) => format!("?actor={actor}&limit=2&cursor={c}"),
            None => format!("?actor={actor}&limit=2"),
        };
        let res = get_audit(&app, &query, Some(&admin_token)).await;
        assert_eq!(res.status, StatusCode::OK, "page {page}: {}", res.text());
        let body = res.json();
        let items = body["items"].as_array().expect("items is an array");
        let want = if page < 2 { 2 } else { 1 };
        assert_eq!(items.len(), want, "page {page} size: {body}");
        for item in items {
            seen.push(item["id"].as_str().unwrap().to_string());
        }
        let next = body
            .get("next_cursor")
            .and_then(|c| c.as_str().map(String::from));
        if page < 2 {
            cursor = Some(next.expect("pages 1-2 carry a next cursor"));
        } else {
            assert!(next.is_none(), "the last page carries no cursor: {body}");
        }
    }
    assert_eq!(seen.len(), 5, "five rows across three pages");
    assert_eq!(
        seen.iter().collect::<std::collections::HashSet<_>>().len(),
        5,
        "no repeats across pages"
    );
    // The pages, in order, are t(4), t(3), t(2), t(1), t(0).
    let res = get_audit(&app, &format!("?actor={actor}"), Some(&admin_token)).await;
    let full: Vec<String> = res.json()["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(seen, full, "pages concatenate to the full list");

    // A tampered cursor is 422 PF-2002, never a 500 and
    // never a silent restart from the first page.
    let res = get_audit(
        &app,
        &format!("?actor={actor}&cursor=not-a-cursor"),
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
    // A well-formed cursor with a `ts` outside chrono's range
    // decodes fine but must still be 422, never a 500: the
    // repo's `from_timestamp_millis(...).expect(...)` would
    // panic on it, so the service range-checks first (the
    // 121st's adoption-review fix — the same `.expect()`
    // sits in the users/jobs readers, a pre-existing
    // exposure noted for a later ticket).
    let out_of_range = Cursor {
        ts: i64::MAX,
        id: Uuid::now_v7(),
    }
    .encode();
    let res = get_audit(
        &app,
        &format!("?actor={actor}&cursor={out_of_range}"),
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
    // A malformed actor uuid is 422 PF-2002.
    let res = get_audit(&app, "?actor=not-a-uuid", Some(&admin_token)).await;
    assert_eq!(
        res.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        res.text()
    );
    assert_eq!(res.json()["code"], "PF-2002");
    // A malformed `from` is 422 PF-2002.
    let res = get_audit(
        &app,
        &format!("?actor={actor}&from=not-a-time"),
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

    // The limit clamps: 0 means 1 (a page of one, with a
    // next cursor); 500 means 100, so all 5 rows fit on
    // one page with no next cursor.
    let res = get_audit(&app, &format!("?actor={actor}&limit=0"), Some(&admin_token)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let body = res.json();
    assert_eq!(body["items"].as_array().unwrap().len(), 1, "{body}");
    assert!(body.get("next_cursor").is_some(), "limit 0 → 1: {body}");
    let res = get_audit(
        &app,
        &format!("?actor={actor}&limit=500"),
        Some(&admin_token),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let body = res.json();
    assert_eq!(body["items"].as_array().unwrap().len(), 5, "{body}");
    assert!(body.get("next_cursor").is_none(), "limit 500 → all: {body}");
}

#[tokio::test]
async fn admin_audit_empty_page() {
    let app = admin_audit_app().await;
    let (_admin_id, admin_token) = make_role(
        &app,
        &format!("empty-admin-{}@example.com", Uuid::now_v7()),
        "admin",
    )
    .await;
    // An actor with no rows at all: an empty page, not a
    // 404 — a list never 404s on its filter.
    let ghost = Uuid::now_v7();
    let res = get_audit(&app, &format!("?actor={ghost}"), Some(&admin_token)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let body = res.json();
    assert_eq!(body["items"], json!([]), "{body}");
    assert!(body.get("next_cursor").is_none(), "{body}");
}

#[tokio::test]
async fn admin_audit_read_writes_no_audit_row() {
    let app = admin_audit_app().await;
    let (_admin_id, admin_token) = make_role(
        &app,
        &format!("ro-admin-{}@example.com", Uuid::now_v7()),
        "admin",
    )
    .await;
    let actor = Uuid::now_v7();
    seed_rows(
        &app,
        actor,
        &[
            ("user.suspended", "user", "user-1", t(0)),
            ("user.restored", "user", "user-1", t(1)),
        ],
    )
    .await;
    assert_eq!(count_for_actor(&app, actor).await, 2, "seeded two rows");
    // The read itself must not write: the count is
    // identical after the GET (P1-046 — reads are not
    // audited).
    let res = get_audit(&app, &format!("?actor={actor}"), Some(&admin_token)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert_eq!(
        count_for_actor(&app, actor).await,
        2,
        "the read wrote a row"
    );
}
