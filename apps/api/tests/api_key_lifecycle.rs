//! P1-039: the API-key lifecycle suite, end to end.
//!
//! The transitions already have their own tests in isolation
//! (create in `api_keys.rs` P1-033, the scope check in
//! `extractor.rs` P1-034, rotation in `api_keys.rs` P1-035,
//! revocation in `api_keys.rs` P1-036) — this suite is the
//! one place that walks a SINGLE key through the whole
//! lifecycle in order, on a frozen clock, and proves the
//! state machine hangs together:
//!
//! create → use → rotate → old-valid-in-window → expired →
//! revoked, with the audit trail (`api_key.created` /
//! `api_key.rotated` / `api_key.revoked`, exactly one of
//! each, no secret or hash in any row) asserted at the end.
//!
//! Plus the two gaps the isolated tests left open:
//! - a DIRECTLY expired key (its `expires_at` set in the
//!   past, not via rotation) is 401 `PF-1001` at the HTTP
//!   layer and frees its cap slot — the repo layer proved
//!   the rule, this proves the extractor honours it;
//! - scope enforcement in the ticket's own phrasing: a
//!   `jobs:read`-only key cannot CREATE (a job, or a key).

mod common;

use axum::http::{Method, Request, StatusCode};
use chrono::{Duration, Utc};
use common::{TestApp, TestFixtures};
use detag_core::checksum::sha256_hex;
use detag_core::clock::{Clock, ManualClock};
use serde_json::json;
use sqlx::Row;
use std::sync::Arc;
use uuid::Uuid;

/// The request id: a valid UUID, echoed by the middleware.
const REQUEST_ID: &str = "123e4567-e89b-42d3-a456-426614174109";

/// The password every test user registers with.
const PASSWORD: &str = "correct horse battery staple";

async fn lifecycle_app() -> TestApp {
    let fixtures = TestFixtures::from_env().await;
    let mut settings = fixtures.settings();
    settings.disposable_email_domains = "mailinator.com".to_string();
    settings.tos_current_version = "v1".to_string();
    TestApp::spawn_with_fixtures_and_settings(fixtures, settings).await
}

/// App on a frozen clock: the caller owns the clock and
/// advances it while the app reads it — the rotation overlap
/// window is proven without waiting 24 h. The clock starts
/// at the real now, so register/login behave exactly as on
/// the wall clock until advanced.
async fn lifecycle_app_with_clock(clock: Arc<ManualClock>) -> TestApp {
    let fixtures = TestFixtures::from_env().await;
    let mut settings = fixtures.settings();
    settings.disposable_email_domains = "mailinator.com".to_string();
    settings.tos_current_version = "v1".to_string();
    let clock: Arc<dyn Clock> = clock;
    TestApp::spawn_with_fixtures_settings_and_clock(fixtures, settings, clock).await
}

fn unique_email() -> String {
    format!("apikey-lifecycle-{}@example.com", Uuid::now_v7())
}

/// A per-test XFF address: each test gets its own IP bucket,
/// so the tests never trip each other's rate limits (the
/// P1-003 lesson).
fn unique_xff() -> String {
    format!("203.0.113.{}", Uuid::now_v7())
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

/// Register + log in; return (user id, email, access token).
/// The user is granted Pro (P1-055): key creation is
/// plan-gated, and this suite is about keys — a Free user
/// could not mint one.
async fn authed_user(app: &TestApp) -> (Uuid, String, String) {
    let email = unique_email();
    let id = register_user(app, &email).await;
    let token = login_token(app, &email).await;
    app.grant_plan(id, "pro").await;
    (id, email, token)
}

fn create_key_req(token: &str, body: serde_json::Value) -> Request<axum::body::Body> {
    Request::builder()
        .method(Method::POST)
        .uri("/v1/api-keys")
        .header("x-request-id", REQUEST_ID)
        .header("x-forwarded-for", unique_xff())
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/json")
        .body(axum::body::Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap()
}

async fn create_key(
    app: &TestApp,
    token: &str,
    name: &str,
    scopes: &[&str],
) -> common::TestResponse {
    let body = json!({ "name": name, "scopes": scopes });
    app.request(create_key_req(token, body)).await
}

async fn rotate_key(app: &TestApp, token: &str, key_id: Uuid) -> common::TestResponse {
    app.request(
        Request::builder()
            .method(Method::POST)
            .uri(format!("/v1/api-keys/{key_id}/rotate"))
            .header("x-request-id", REQUEST_ID)
            .header("x-forwarded-for", unique_xff())
            .header("authorization", format!("Bearer {token}"))
            .body(axum::body::Body::empty())
            .unwrap(),
    )
    .await
}

async fn revoke_key(app: &TestApp, token: &str, key_id: Uuid) -> common::TestResponse {
    app.request(
        Request::builder()
            .method(Method::DELETE)
            .uri(format!("/v1/api-keys/{key_id}"))
            .header("x-request-id", REQUEST_ID)
            .header("x-forwarded-for", unique_xff())
            .header("authorization", format!("Bearer {token}"))
            .body(axum::body::Body::empty())
            .unwrap(),
    )
    .await
}

/// Any request with a Bearer API key (method + uri). A 501
/// from the jobs stubs means the key authenticated and passed
/// the scope check; 401 `PF-1001` means the key is dead
/// (unknown, expired, or revoked); 403 `PF-1002` means the
/// key is live but lacks the route's scope.
async fn key_request(
    app: &TestApp,
    method: Method,
    uri: &str,
    secret: &str,
) -> common::TestResponse {
    app.request(
        Request::builder()
            .method(method)
            .uri(uri)
            .header("x-request-id", REQUEST_ID)
            .header("x-forwarded-for", unique_xff())
            .header("authorization", format!("Bearer {secret}"))
            .body(axum::body::Body::empty())
            .unwrap(),
    )
    .await
}

/// The stored `last_used_at` for a key row (NULL before first use).
async fn last_used_at(app: &TestApp, key_id: Uuid) -> Option<chrono::DateTime<chrono::Utc>> {
    let row = sqlx::query("SELECT last_used_at FROM api_keys WHERE id = $1")
        .bind(key_id)
        .fetch_one(&app.pool)
        .await
        .expect("read key row");
    row.try_get("last_used_at").expect("last_used_at column")
}

/// The audit rows for one user + action, oldest first.
async fn audit_rows(app: &TestApp, user_id: Uuid, action: &str) -> Vec<serde_json::Value> {
    let rows = sqlx::query(
        r#"SELECT after FROM audit_logs WHERE actor_user_id = $1 AND action = $2 ORDER BY created_at"#,
    )
    .bind(user_id)
    .bind(action)
    .fetch_all(&app.pool)
    .await
    .expect("audit rows");
    rows.iter()
        .map(|r| {
            r.try_get::<Option<serde_json::Value>, _>("after")
                .expect("after")
                .unwrap_or(serde_json::Value::Null)
        })
        .collect()
}

#[tokio::test]
async fn lifecycle_one_key_end_to_end() {
    // One key, walked through every state in order:
    // create → use → rotate → old-valid-in-window → expired
    // → revoked. Each transition is proven by the key's next
    // request, and the audit trail is asserted at the end.
    let clock = Arc::new(ManualClock::new(Utc::now()));
    let app = lifecycle_app_with_clock(clock.clone()).await;
    let (user_id, email, token) = authed_user(&app).await;

    // CREATE: 201, the full secret shown once, prefix derived.
    let res = create_key(&app, &token, "CI deploy", &["jobs:write", "jobs:read"]).await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.text());
    let created = res.json();
    let key_id: Uuid = created["id"].as_str().unwrap().parse().unwrap();
    let secret = created["secret"].as_str().unwrap().to_string();
    assert!(secret.starts_with("pf_live_"), "pf_live_ prefix");
    assert_eq!(
        created["prefix"].as_str().unwrap(),
        format!("pf_live_{}", &secret["pf_live_".len()..][..4]),
        "prefix is pf_live_ + first 4 secret chars"
    );
    assert_eq!(last_used_at(&app, key_id).await, None, "fresh key unused");

    // USE: both scopes reach their stubs (501 is the proof
    // auth passed), and the use stamps last_used_at.
    let res = key_request(&app, Method::GET, "/v1/jobs", &secret).await;
    assert_eq!(res.status, StatusCode::NOT_IMPLEMENTED, "{}", res.text());
    let res = key_request(&app, Method::POST, "/v1/jobs", &secret).await;
    assert_eq!(res.status, StatusCode::NOT_IMPLEMENTED, "{}", res.text());
    assert!(
        last_used_at(&app, key_id).await.is_some(),
        "a verified use stamps last_used_at"
    );

    // ROTATE: a replacement row, same name + scopes, new
    // secret; the old row carries the overlap deadline.
    let res = rotate_key(&app, &token, key_id).await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.text());
    let rotated = res.json();
    let new_id: Uuid = rotated["id"].as_str().unwrap().parse().unwrap();
    let new_secret = rotated["secret"].as_str().unwrap().to_string();
    assert_ne!(new_secret, secret, "rotation mints a new secret");
    assert_ne!(new_id, key_id, "the replacement is a new row");
    assert_eq!(rotated["name"].as_str().unwrap(), "CI deploy");
    assert_eq!(rotated["scopes"], json!(["jobs:write", "jobs:read"]));

    // OLD-VALID-IN-WINDOW: both secrets authenticate — at
    // once, and one second before the window closes.
    assert_eq!(
        key_request(&app, Method::GET, "/v1/jobs", &secret)
            .await
            .status,
        StatusCode::NOT_IMPLEMENTED
    );
    assert_eq!(
        key_request(&app, Method::GET, "/v1/jobs", &new_secret)
            .await
            .status,
        StatusCode::NOT_IMPLEMENTED
    );
    clock.advance(Duration::seconds(86_400 - 1));
    assert_eq!(
        key_request(&app, Method::GET, "/v1/jobs", &secret)
            .await
            .status,
        StatusCode::NOT_IMPLEMENTED,
        "the old secret still works 1 s before the deadline"
    );

    // EXPIRED: past the window the old secret is dead — the
    // generic 401, indistinguishable from unknown — while
    // the new secret keeps working.
    clock.advance(Duration::seconds(2));
    let res = key_request(&app, Method::GET, "/v1/jobs", &secret).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());
    assert_eq!(res.json()["code"].as_str().unwrap(), "PF-1001");
    assert_eq!(
        key_request(&app, Method::GET, "/v1/jobs", &new_secret)
            .await
            .status,
        StatusCode::NOT_IMPLEMENTED
    );

    // REVOKED: the replacement dies immediately — no grace,
    // no overlap — and the old secret stays dead.
    // (Re-login: the frozen clock advanced past the first
    // access token's 15-minute life.)
    let token = login_token(&app, &email).await;
    let res = revoke_key(&app, &token, new_id).await;
    assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.text());
    let res = key_request(&app, Method::GET, "/v1/jobs", &new_secret).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());
    assert_eq!(res.json()["code"].as_str().unwrap(), "PF-1001");
    let res = key_request(&app, Method::GET, "/v1/jobs", &secret).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());
    assert_eq!(res.json()["code"].as_str().unwrap(), "PF-1001");

    // The audit trail: exactly one row per transition, in
    // order, and no secret or hash in any of them.
    let created_rows = audit_rows(&app, user_id, "api_key.created").await;
    let rotated_rows = audit_rows(&app, user_id, "api_key.rotated").await;
    let revoked_rows = audit_rows(&app, user_id, "api_key.revoked").await;
    assert_eq!(created_rows.len(), 1, "one created audit");
    assert_eq!(rotated_rows.len(), 1, "one rotated audit");
    assert_eq!(revoked_rows.len(), 1, "one revoked audit");
    assert_eq!(
        rotated_rows[0]["old_api_key_id"].as_str().unwrap(),
        key_id.to_string()
    );
    assert_eq!(
        rotated_rows[0]["new_api_key_id"].as_str().unwrap(),
        new_id.to_string()
    );
    assert_eq!(
        revoked_rows[0]["api_key_id"].as_str().unwrap(),
        new_id.to_string()
    );
    let old_hash = sha256_hex(secret.as_bytes());
    let new_hash = sha256_hex(new_secret.as_bytes());
    for row in created_rows
        .iter()
        .chain(rotated_rows.iter())
        .chain(revoked_rows.iter())
    {
        let text = row.to_string();
        assert!(!text.contains(&secret), "audit has no old secret");
        assert!(!text.contains(&new_secret), "audit has no new secret");
        assert!(!text.contains(&old_hash), "audit has no old hash");
        assert!(!text.contains(&new_hash), "audit has no new hash");
    }
}

#[tokio::test]
async fn expired_key_is_unauthenticated_and_frees_its_cap_slot() {
    // A key whose `expires_at` is in the past — set
    // directly, not via rotation — reads as dead at the HTTP
    // layer (401 `PF-1001`, the generic code) and stops
    // counting toward the 10-live-keys cap, so the slot it
    // occupied frees.
    let app = lifecycle_app().await;
    let (user_id, _, token) = authed_user(&app).await;

    let mut first_id = Uuid::nil();
    let mut first_secret = String::new();
    for n in 1..=10 {
        let res = create_key(&app, &token, &format!("key {n}"), &["jobs:read"]).await;
        assert_eq!(res.status, StatusCode::CREATED, "key {n}: {}", res.text());
        if n == 1 {
            first_id = res.json()["id"].as_str().unwrap().parse().unwrap();
            first_secret = res.json()["secret"].as_str().unwrap().to_string();
        }
    }
    // At the cap: one more is 409.
    let res = create_key(&app, &token, "key 11", &["jobs:read"]).await;
    assert_eq!(res.status, StatusCode::CONFLICT, "{}", res.text());

    // Expire the first key directly (its row is live until
    // `expires_at` passes).
    sqlx::query("UPDATE api_keys SET expires_at = now() - interval '1 hour' WHERE id = $1")
        .bind(first_id)
        .execute(&app.pool)
        .await
        .expect("expire key");

    // Dead at the HTTP layer: the generic 401, exactly like
    // an unknown key.
    let res = key_request(&app, Method::GET, "/v1/jobs", &first_secret).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());
    assert_eq!(res.json()["code"].as_str().unwrap(), "PF-1001");

    // The slot frees: the 11th create is 201 again, and the
    // user is back at 10 live keys.
    let res = create_key(&app, &token, "key 11", &["jobs:read"]).await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.text());
    let live = detag_core::repos::api_keys::count_live_for_user(&app.pool, user_id, Utc::now())
        .await
        .expect("count live keys");
    assert_eq!(live, 10, "10 live keys again after the expiry");
}

#[tokio::test]
async fn read_only_key_cannot_create() {
    // Scope enforcement, in the ticket's phrasing: a
    // `jobs:read`-only key cannot CREATE — neither a job
    // (POST /v1/jobs is 403 `PF-1002`) nor a key (POST
    // /v1/api-keys is 403 `PF-1002`). The positive controls
    // prove the key is live: it reads (GET /v1/jobs is 501),
    // and a write-only key writes but cannot read.
    let app = lifecycle_app().await;
    let (_, _, token) = authed_user(&app).await;

    let res = create_key(&app, &token, "reader", &["jobs:read"]).await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.text());
    let read_id: Uuid = res.json()["id"].as_str().unwrap().parse().unwrap();
    let read_secret = res.json()["secret"].as_str().unwrap().to_string();

    // A read-only key reads fine…
    let res = key_request(&app, Method::GET, "/v1/jobs", &read_secret).await;
    assert_eq!(res.status, StatusCode::NOT_IMPLEMENTED, "{}", res.text());

    // …but cannot create a job.
    let res = key_request(&app, Method::POST, "/v1/jobs", &read_secret).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.text());
    assert_eq!(res.json()["code"].as_str().unwrap(), "PF-1002");

    // …and cannot mint a key (keys cannot mint keys).
    let res = key_request(&app, Method::POST, "/v1/api-keys", &read_secret).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.text());
    assert_eq!(res.json()["code"].as_str().unwrap(), "PF-1002");

    // The mirror: a write-only key writes, but cannot read.
    let res = create_key(&app, &token, "writer", &["jobs:write"]).await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.text());
    let write_secret = res.json()["secret"].as_str().unwrap().to_string();
    let res = key_request(&app, Method::POST, "/v1/jobs", &write_secret).await;
    assert_eq!(res.status, StatusCode::NOT_IMPLEMENTED, "{}", res.text());
    let res = key_request(&app, Method::GET, "/v1/jobs", &write_secret).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.text());
    assert_eq!(res.json()["code"].as_str().unwrap(), "PF-1002");

    // The reader's successful GET stamped last_used_at
    // (the denied-is-not-a-use rule itself is proven in
    // extractor.rs — `last_used_at_is_throttled`).
    assert!(
        last_used_at(&app, read_id).await.is_some(),
        "the reader's successful GET stamped last_used_at"
    );
}
