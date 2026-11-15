//! P1-038: key-scoped usage attribution, end to end.
//!
//! Through the real router (`TestApp` oneshot) against the
//! migrated scratch database: jobs record the API key that
//! created them (`jobs.key_id`); `GET /v1/admin/jobs` shows
//! that key's display prefix per row and filters by
//! `?key_id=` / `?user_id=`; the usage-attribution service
//! breaks one user's day down per key (plus the unattributed
//! row for web / JWT-created jobs).
//!
//! Job creation itself is still the Phase 2 stub, so the
//! tests seed job rows through the repo directly (the same
//! pattern as `admin_suspend.rs`) — the key is minted through
//! the real API, so the prefix the admin view shows is the
//! prefix the API returned at mint time. The harness DB is
//! shared across the tests in this binary (and persists
//! across runs), so every list assertion filters by the
//! test's own `user_id` / `key_id` — an unfiltered list can
//! never assert an exact count.

mod common;

use axum::http::{Method, Request, StatusCode};
use chrono::Utc;
use common::{TestApp, TestFixtures};
use detag_api::services::usage_attribution::{
    usage_breakdown_for_user, usage_for_key, UsageAttributionError,
};
use detag_core::repos::jobs::{self, NewJob};
use serde_json::json;
use uuid::Uuid;

/// The request id: a valid UUID, echoed by the middleware.
const REQUEST_ID: &str = "123e4567-e89b-42d3-a456-426614174108";

/// The password every test user registers with.
const PASSWORD: &str = "correct horse battery staple";

async fn app() -> TestApp {
    let fixtures = TestFixtures::from_env().await;
    let mut settings = fixtures.settings();
    settings.disposable_email_domains = "mailinator.com".to_string();
    settings.tos_current_version = "v1".to_string();
    TestApp::spawn_with_fixtures_and_settings(fixtures, settings).await
}

fn unique_email(prefix: &str) -> String {
    format!("{prefix}-{}@example.com", Uuid::now_v7())
}

/// A per-test XFF address: each test gets its own IP bucket,
/// so the tests never trip each other's rate limits (the
/// P1-003 lesson — the shared "unknown" bucket is one bucket
/// for the whole binary).
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

/// Mint a key through the real API; return (key id, prefix).
async fn mint_key(app: &TestApp, token: &str, scopes: &[&str]) -> (Uuid, String) {
    let body = json!({ "name": "attribution-key", "scopes": scopes });
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
    let body = res.json();
    (
        body["id"].as_str().unwrap().parse().unwrap(),
        body["prefix"].as_str().unwrap().to_string(),
    )
}

/// Seed a job row for a user, attributed to `key_id` when
/// `Some` (a key-created job) or unattributed when `None` (a
/// web / JWT-created job). Job creation is still the Phase 2
/// stub, so the repo is the seeder — the attribution column
/// is what this ticket proves.
async fn seed_job(app: &TestApp, user_id: Uuid, kind: &str, key_id: Option<Uuid>) -> Uuid {
    let idem = format!("idem-p138-{}", Uuid::now_v7());
    jobs::create(
        &app.pool,
        &NewJob {
            user_id,
            upload_id: None,
            kind,
            mode: "auto",
            status: "queued",
            lane: "standard",
            params: json!({}),
            idempotency_key: Some(&idem),
            key_id,
        },
    )
    .await
    .expect("seed job")
    .id
}

fn jobs_req(token: Option<&str>, query: &str) -> Request<axum::body::Body> {
    let mut req = Request::builder()
        .method(Method::GET)
        .uri(format!("/v1/admin/jobs{query}"))
        .header("x-request-id", REQUEST_ID)
        .header("x-forwarded-for", unique_xff());
    if let Some(token) = token {
        req = req.header("authorization", format!("Bearer {token}"));
    }
    req.body(axum::body::Body::empty()).unwrap()
}

/// The ids in an admin-jobs response body, in order.
fn item_ids(body: &serde_json::Value) -> Vec<String> {
    body["items"]
        .as_array()
        .expect("items is an array")
        .iter()
        .map(|item| item["id"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn admin_jobs_list_shows_key_prefix() {
    // P1-038 (the ticket's AC): the admin view shows which
    // key created each job — the minted key's prefix on the
    // key-created job, no prefix on the web-created one.
    let app = app().await;
    let (_admin_id, admin_token) = make_role(&app, &unique_email("admin"), "admin").await;
    let email = unique_email("attr-user");
    let user_id = register_user(&app, &email).await;
    let token = login_token(&app, &email).await;
    let (key_id, prefix) = mint_key(&app, &token, &["jobs:write"]).await;
    let key_job = seed_job(&app, user_id, "image", Some(key_id)).await;
    let web_job = seed_job(&app, user_id, "image", None).await;
    // Another user's job must not leak into the filter.
    let other_id = register_user(&app, &unique_email("attr-other")).await;
    seed_job(&app, other_id, "image", None).await;

    let res = app
        .request(jobs_req(Some(&admin_token), &format!("?user_id={user_id}")))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let body = res.json();
    assert_eq!(
        item_ids(&body),
        vec![web_job.to_string(), key_job.to_string()],
        "newest first: {body}"
    );
    assert!(body.get("next_cursor").is_none(), "{body}");
    let items = body["items"].as_array().unwrap();
    let key_item = items
        .iter()
        .find(|item| item["id"] == key_job.to_string())
        .expect("key job listed");
    assert_eq!(key_item["key_id"], key_id.to_string(), "{key_item}");
    assert_eq!(key_item["key_prefix"], prefix, "{key_item}");
    assert_eq!(key_item["kind"], "image", "{key_item}");
    assert_eq!(key_item["status"], "queued", "{key_item}");
    let web_item = items
        .iter()
        .find(|item| item["id"] == web_job.to_string())
        .expect("web job listed");
    assert!(web_item.get("key_id").is_none(), "{web_item}");
    assert!(web_item.get("key_prefix").is_none(), "{web_item}");
}

#[tokio::test]
async fn admin_jobs_list_filters_by_key() {
    // P1-038: `?key_id=` narrows the admin view to one key's
    // jobs — the "usage endpoints filter by key" read.
    let app = app().await;
    let (_admin_id, admin_token) = make_role(&app, &unique_email("admin"), "admin").await;
    let email = unique_email("attr-filter");
    let user_id = register_user(&app, &email).await;
    let token = login_token(&app, &email).await;
    let (key_a, _) = mint_key(&app, &token, &["jobs:write"]).await;
    let (key_b, _) = mint_key(&app, &token, &["jobs:write"]).await;
    let job_a = seed_job(&app, user_id, "image", Some(key_a)).await;
    seed_job(&app, user_id, "video", Some(key_b)).await;
    seed_job(&app, user_id, "image", None).await;

    let res = app
        .request(jobs_req(Some(&admin_token), &format!("?key_id={key_a}")))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let body = res.json();
    assert_eq!(item_ids(&body), vec![job_a.to_string()], "{body}");

    // An unknown key filters to empty, not an error.
    let res = app
        .request(jobs_req(
            Some(&admin_token),
            &format!("?key_id={}", Uuid::now_v7()),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert!(res.json()["items"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn admin_jobs_list_paginates_without_duplicates() {
    // P1-038: the keyset pagination walks the filtered list
    // without repeating or skipping a row.
    let app = app().await;
    let (_admin_id, admin_token) = make_role(&app, &unique_email("admin"), "admin").await;
    let user_id = register_user(&app, &unique_email("attr-page")).await;
    let first = seed_job(&app, user_id, "image", None).await;
    let second = seed_job(&app, user_id, "image", None).await;
    let third = seed_job(&app, user_id, "image", None).await;

    let res = app
        .request(jobs_req(
            Some(&admin_token),
            &format!("?user_id={user_id}&limit=2"),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let page1 = res.json();
    assert_eq!(page1["items"].as_array().unwrap().len(), 2, "{page1}");
    let cursor = page1["next_cursor"].as_str().expect("page 1 has a cursor");
    let res = app
        .request(jobs_req(
            Some(&admin_token),
            &format!("?user_id={user_id}&limit=2&cursor={cursor}"),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let page2 = res.json();
    assert_eq!(page2["items"].as_array().unwrap().len(), 1, "{page2}");
    assert!(page2.get("next_cursor").is_none(), "{page2}");
    let mut seen = item_ids(&page1);
    seen.extend(item_ids(&page2));
    seen.sort();
    let mut expected = vec![first.to_string(), second.to_string(), third.to_string()];
    expected.sort();
    assert_eq!(seen, expected, "pagination must visit every row once");
}

#[tokio::test]
async fn admin_jobs_list_auth_rules() {
    // P1-038: admin + support read the jobs view; a plain
    // user is 403 PF-1002, no credential is 401 PF-1001, a
    // tampered cursor is 422 PF-2002.
    let app = app().await;
    let (_admin_id, admin_token) = make_role(&app, &unique_email("admin"), "admin").await;
    let (_support_id, support_token) = make_role(&app, &unique_email("support"), "support").await;
    let plain_email = unique_email("attr-auth");
    let _plain_id = register_user(&app, &plain_email).await;
    let user_token = login_token(&app, &plain_email).await;

    let res = app.request(jobs_req(Some(&support_token), "")).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());

    let res = app.request(jobs_req(Some(&user_token), "")).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-1002", "{}", res.text());

    let res = app.request(jobs_req(None, "")).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-1001", "{}", res.text());

    let res = app
        .request(jobs_req(Some(&admin_token), "?cursor=not-a-cursor"))
        .await;
    assert_eq!(
        res.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        res.text()
    );
    assert_eq!(res.json()["code"], "PF-2002", "{}", res.text());

    let res = app
        .request(jobs_req(Some(&admin_token), "?key_id=not-a-uuid"))
        .await;
    assert_eq!(
        res.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        res.text()
    );
    assert_eq!(res.json()["code"], "PF-2002", "{}", res.text());
}

#[tokio::test]
async fn usage_breakdown_attributes_per_key() {
    // P1-038: the attribution service breaks one user's day
    // down per key — counts + prefix — with the unattributed
    // row for web / JWT-created jobs; an unknown key is an
    // error, not an empty result.
    let app = app().await;
    let email = unique_email("attr-usage");
    let user_id = register_user(&app, &email).await;
    let token = login_token(&app, &email).await;
    let (key_a, prefix_a) = mint_key(&app, &token, &["jobs:write"]).await;
    let (key_b, prefix_b) = mint_key(&app, &token, &["jobs:write"]).await;
    seed_job(&app, user_id, "image", Some(key_a)).await;
    seed_job(&app, user_id, "image", Some(key_a)).await;
    seed_job(&app, user_id, "video", Some(key_a)).await;
    seed_job(&app, user_id, "image", Some(key_b)).await;
    seed_job(&app, user_id, "image", None).await;

    let today = Utc::now().date_naive();
    let rows = usage_breakdown_for_user(&app.pool, user_id, today)
        .await
        .expect("breakdown");
    assert_eq!(rows.len(), 3, "key A + key B + unattributed: {rows:?}");
    let row_a = rows
        .iter()
        .find(|r| r.key_id == Some(key_a))
        .expect("row A");
    assert_eq!(row_a.key_prefix.as_deref(), Some(prefix_a.as_str()));
    assert_eq!((row_a.images, row_a.video_jobs), (2, 1));
    assert_eq!(row_a.video_seconds, 0, "seconds wait on P1-079");
    let row_b = rows
        .iter()
        .find(|r| r.key_id == Some(key_b))
        .expect("row B");
    assert_eq!(row_b.key_prefix.as_deref(), Some(prefix_b.as_str()));
    assert_eq!((row_b.images, row_b.video_jobs), (1, 0));
    let row_web = rows
        .iter()
        .find(|r| r.key_id.is_none())
        .expect("unattributed row");
    assert_eq!(row_web.key_prefix, None);
    assert_eq!((row_web.images, row_web.video_jobs), (1, 0));

    let usage = usage_for_key(&app.pool, key_a, today)
        .await
        .expect("usage for key A");
    assert_eq!(usage.key_id, Some(key_a));
    assert_eq!(usage.key_prefix.as_deref(), Some(prefix_a.as_str()));
    assert_eq!((usage.images, usage.video_jobs), (2, 1));

    let err = usage_for_key(&app.pool, Uuid::now_v7(), today)
        .await
        .expect_err("unknown key must fail");
    assert!(
        matches!(err, UsageAttributionError::UnknownKey),
        "expected UnknownKey, got {err:?}"
    );

    // A user with no jobs today breaks down to nothing.
    let empty_user = register_user(&app, &unique_email("attr-empty")).await;
    let rows = usage_breakdown_for_user(&app.pool, empty_user, today)
        .await
        .expect("empty breakdown");
    assert!(rows.is_empty(), "{rows:?}");
}
