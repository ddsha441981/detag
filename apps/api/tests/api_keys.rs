//! P1-033: API key generation, end to end.
//!
//! Through the real router (`TestApp` oneshot) against the
//! migrated scratch database: the create happy path (201, the
//! full `pf_live_` secret shown once, only its sha256 stored),
//! the validation rules (name, scopes), the 401 without a
//! credential, the 10-live-keys cap (the 11th is 409
//! `PF-2011`), and the cap under concurrency (12 concurrent
//! creates admit exactly 10 — the row lock in the service
//! transaction serialises them; the lockout precedent for
//! proving a race at the repo layer applies here too, but
//! this loop has no Argon2id in it — the token is minted once
//! up front — so the HTTP version does not flake).
//!
//! Key verification is P1-034's ticket (hash lookup, scope
//! check, throttled `last_used_at` — proven in
//! tests/extractor.rs): these tests assert the stored shape
//! the extractor reads, not that a key authenticates.
//!
//! P1-035: rotation — `POST /v1/api-keys/{id}/rotate`
//! mints a replacement secret (same name + scopes) and keeps
//! the old secret valid for the overlap window (24 h by
//! default), proven on a frozen clock: the old secret works
//! until the window closes, then 401s, while the new secret
//! keeps working. Rotation is exempt from the cap (it
//! replaces, it does not add).

mod common;

use axum::http::{Method, Request, StatusCode};
use chrono::{Duration, Utc};
use common::{TestApp, TestFixtures};
use detag_api::services::revocation::is_api_key_revoked;
use detag_core::checksum::sha256_hex;
use detag_core::clock::{Clock, ManualClock};
use serde_json::json;
use sqlx::Row;
use std::sync::Arc;
use tower::ServiceExt;
use uuid::Uuid;

/// The request id: a valid UUID, echoed by the middleware.
const REQUEST_ID: &str = "123e4567-e89b-42d3-a456-426614174103";

/// The password every test user registers with.
const PASSWORD: &str = "correct horse battery staple";

async fn keys_app() -> TestApp {
    let fixtures = TestFixtures::from_env().await;
    let mut settings = fixtures.settings();
    settings.disposable_email_domains = "mailinator.com".to_string();
    settings.tos_current_version = "v1".to_string();
    TestApp::spawn_with_fixtures_and_settings(fixtures, settings).await
}

/// App on a frozen clock (P1-035): the caller owns the
/// clock and advances it while the app reads it — the
/// rotation overlap window is proven without waiting 24 h.
/// The clock starts at the real now, so register/login
/// behave exactly as on the wall clock until advanced.
async fn keys_app_with_clock(clock: Arc<ManualClock>) -> TestApp {
    let fixtures = TestFixtures::from_env().await;
    let mut settings = fixtures.settings();
    settings.disposable_email_domains = "mailinator.com".to_string();
    settings.tos_current_version = "v1".to_string();
    let clock: Arc<dyn Clock> = clock;
    TestApp::spawn_with_fixtures_settings_and_clock(fixtures, settings, clock).await
}

fn unique_email() -> String {
    format!("apikeys-{}@example.com", Uuid::now_v7())
}

/// A per-test XFF address: each test gets its own IP bucket,
/// so the tests never trip each other's rate limits (the
/// P1-003 lesson).
fn unique_xff() -> String {
    format!("203.0.113.{}", Uuid::now_v7().as_u128() % 250 + 1)
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

/// Register + log in; return (user id, access token).
async fn authed_user(app: &TestApp) -> (Uuid, String) {
    let email = unique_email();
    let id = register_user(app, &email).await;
    let token = login_token(app, &email).await;
    (id, token)
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

fn rotate_key_req(token: &str, key_id: Uuid) -> Request<axum::body::Body> {
    Request::builder()
        .method(Method::POST)
        .uri(format!("/v1/api-keys/{key_id}/rotate"))
        .header("x-request-id", REQUEST_ID)
        .header("x-forwarded-for", unique_xff())
        .header("authorization", format!("Bearer {token}"))
        .body(axum::body::Body::empty())
        .unwrap()
}

async fn rotate_key(app: &TestApp, token: &str, key_id: Uuid) -> common::TestResponse {
    app.request(rotate_key_req(token, key_id)).await
}

/// Use a key against a scoped stub (`GET /v1/jobs` needs
/// `jobs:read`): 501 means the key authenticated and passed
/// the scope check (the stub runs); 401 `PF-1001` means the
/// key is dead (unknown, revoked, or past its overlap).
async fn probe_key(app: &TestApp, secret: &str) -> common::TestResponse {
    app.request(
        Request::builder()
            .method(Method::GET)
            .uri("/v1/jobs")
            .header("x-request-id", REQUEST_ID)
            .header("x-forwarded-for", unique_xff())
            .header("authorization", format!("Bearer {secret}"))
            .body(axum::body::Body::empty())
            .unwrap(),
    )
    .await
}

#[tokio::test]
async fn create_returns_201_with_full_secret_shown_once() {
    let app = keys_app().await;
    let (user_id, token) = authed_user(&app).await;

    let res = create_key(&app, &token, "CI deploy", &["jobs:write", "jobs:read"]).await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.text());
    let body = res.json();
    let secret = body["secret"].as_str().expect("secret shown");
    assert!(
        secret.starts_with("pf_live_"),
        "secret has the pf_live_ prefix: {secret}"
    );
    let part = &secret["pf_live_".len()..];
    assert_eq!(part.len(), 32, "24 bytes base64url: {secret}");
    assert!(
        part.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
        "secret part is base64url: {secret}"
    );
    assert_eq!(body["name"].as_str().unwrap(), "CI deploy");
    assert_eq!(
        body["prefix"].as_str().unwrap(),
        format!("pf_live_{}", &part[..4]),
        "prefix is pf_live_ + first 4 secret chars"
    );
    assert_eq!(body["scopes"], json!(["jobs:write", "jobs:read"]));
    assert!(body["id"].as_str().is_some(), "key id returned");
    assert!(body["created_at"].as_str().is_some(), "created_at returned");

    // Only the hash is stored — the row carries no usable secret.
    // (Untyped `sqlx::query` + `try_get`: test targets are not
    // covered by `cargo sqlx prepare`, so `query!` macros cannot
    // be used in tests — they have no offline data.)
    let row = sqlx::query(r#"SELECT secret_hash, prefix, scopes FROM api_keys WHERE user_id = $1"#)
        .bind(user_id)
        .fetch_one(&app.pool)
        .await
        .expect("key row stored");
    let stored_hash: String = row.try_get("secret_hash").expect("secret_hash");
    let stored_prefix: String = row.try_get("prefix").expect("prefix");
    let stored_scopes: Vec<String> = row.try_get("scopes").expect("scopes");
    assert_eq!(stored_hash, sha256_hex(secret.as_bytes()));
    assert_ne!(stored_hash, secret, "the hash is not the secret");
    assert_eq!(stored_prefix, body["prefix"].as_str().unwrap());
    assert_eq!(
        stored_scopes,
        vec!["jobs:write".to_string(), "jobs:read".to_string()]
    );

    // The creation is audited — without the secret or its hash.
    let audit = sqlx::query(
        r#"SELECT action, after FROM audit_logs WHERE actor_user_id = $1 AND action = 'api_key.created'"#,
    )
    .bind(user_id)
    .fetch_one(&app.pool)
    .await
    .expect("api_key.created audit row");
    let action: String = audit.try_get("action").expect("action");
    let after: Option<serde_json::Value> = audit.try_get("after").expect("after");
    assert_eq!(action, "api_key.created");
    let after = after.expect("audit after");
    assert_eq!(after["prefix"].as_str().unwrap(), stored_prefix);
    assert!(!after.to_string().contains(secret), "audit has no secret");
    assert!(
        !after.to_string().contains(&stored_hash),
        "audit has no hash either"
    );
}

#[tokio::test]
async fn create_rejects_bad_scopes() {
    let app = keys_app().await;
    let (_, token) = authed_user(&app).await;

    // Unknown scope.
    let res = create_key(&app, &token, "x", &["jobs:delete"]).await;
    assert_eq!(
        res.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        res.text()
    );
    let body = res.json();
    assert_eq!(body["code"].as_str().unwrap(), "PF-2002");
    assert!(
        body["field_errors"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["field"].as_str() == Some("scopes")),
        "scopes field error: {}",
        res.text()
    );

    // No scopes at all.
    let res = create_key(&app, &token, "x", &[]).await;
    assert_eq!(
        res.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        res.text()
    );

    // A duplicate scope.
    let res = create_key(&app, &token, "x", &["jobs:read", "jobs:read"]).await;
    assert_eq!(
        res.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        res.text()
    );
    assert_eq!(
        res.json()["field_errors"][0]["field"].as_str().unwrap(),
        "scopes"
    );
}

#[tokio::test]
async fn create_rejects_bad_name() {
    let app = keys_app().await;
    let (_, token) = authed_user(&app).await;

    // Empty (blank) name.
    let res = create_key(&app, &token, "   ", &["jobs:read"]).await;
    assert_eq!(
        res.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        res.text()
    );
    assert_eq!(res.json()["code"].as_str().unwrap(), "PF-2002");

    // Over the 100-char display-name limit.
    let res = create_key(&app, &token, &"n".repeat(101), &["jobs:read"]).await;
    assert_eq!(
        res.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        res.text()
    );
}

#[tokio::test]
async fn create_requires_auth() {
    let app = keys_app().await;
    let body = json!({ "name": "x", "scopes": ["jobs:read"] });
    let res = app
        .request(
            Request::builder()
                .method(Method::POST)
                .uri("/v1/api-keys")
                .header("x-request-id", REQUEST_ID)
                .header("x-forwarded-for", unique_xff())
                .header("content-type", "application/json")
                .body(axum::body::Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());
    assert_eq!(res.json()["code"].as_str().unwrap(), "PF-1001");
}

#[tokio::test]
async fn tenth_key_succeeds_eleventh_is_409() {
    let app = keys_app().await;
    let (user_id, token) = authed_user(&app).await;

    for n in 1..=10 {
        let res = create_key(&app, &token, &format!("key {n}"), &["jobs:read"]).await;
        assert_eq!(res.status, StatusCode::CREATED, "key {n}: {}", res.text());
    }
    let res = create_key(&app, &token, "key 11", &["jobs:read"]).await;
    assert_eq!(res.status, StatusCode::CONFLICT, "{}", res.text());
    assert_eq!(res.json()["code"].as_str().unwrap(), "PF-2011");

    // The failed 11th create stored nothing.
    let live = detag_core::repos::api_keys::count_live_for_user(&app.pool, user_id, Utc::now())
        .await
        .expect("count live keys");
    assert_eq!(live, 10);
}

#[tokio::test]
async fn concurrent_creates_never_exceed_ten() {
    let app = keys_app().await;
    let (user_id, token) = authed_user(&app).await;

    let mut set = tokio::task::JoinSet::new();
    for n in 0..12 {
        let router = app.router.clone();
        let token = token.clone();
        let body = json!({ "name": format!("race {n}"), "scopes": ["jobs:read"] });
        set.spawn(async move {
            let req = create_key_req(&token, body);
            let res = router.oneshot(req).await.expect("oneshot create failed");
            res.status()
        });
    }
    let mut created = 0;
    let mut conflicted = 0;
    while let Some(joined) = set.join_next().await {
        match joined.expect("create task panicked") {
            StatusCode::CREATED => created += 1,
            StatusCode::CONFLICT => conflicted += 1,
            other => panic!("unexpected status in the race: {other}"),
        }
    }
    assert_eq!(created, 10, "exactly 10 concurrent creates succeed");
    assert_eq!(conflicted, 2, "exactly 2 hit the cap");

    // Per-user count: the binary's tests share one scratch DB,
    // so a global count would see other tests' keys.
    let live = detag_core::repos::api_keys::count_live_for_user(&app.pool, user_id, Utc::now())
        .await
        .expect("count live keys");
    assert_eq!(
        live, 10,
        "the table holds exactly 10 live keys for this user"
    );
}

#[tokio::test]
async fn rotate_returns_201_with_new_secret_and_same_name_scopes() {
    let app = keys_app().await;
    let (user_id, token) = authed_user(&app).await;

    let res = create_key(&app, &token, "CI deploy", &["jobs:write", "jobs:read"]).await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.text());
    let created = res.json();
    let old_id: Uuid = created["id"].as_str().unwrap().parse().unwrap();
    let old_secret = created["secret"].as_str().unwrap().to_string();

    let res = rotate_key(&app, &token, old_id).await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.text());
    let body = res.json();
    let new_secret = body["secret"].as_str().expect("new secret shown");
    assert_ne!(new_secret, old_secret, "rotation mints a new secret");
    assert!(
        new_secret.starts_with("pf_live_"),
        "new secret has the prefix"
    );
    assert_ne!(
        body["id"].as_str().unwrap(),
        old_id.to_string(),
        "the replacement is a new row"
    );
    assert_eq!(body["name"].as_str().unwrap(), "CI deploy");
    assert_eq!(body["scopes"], json!(["jobs:write", "jobs:read"]));
    assert_ne!(
        body["prefix"].as_str().unwrap(),
        created["prefix"].as_str().unwrap(),
        "the new secret has a new prefix"
    );

    // The old row carries the overlap deadline; the new row
    // never expires.
    let rows = sqlx::query(
        r#"SELECT id, expires_at FROM api_keys WHERE user_id = $1 ORDER BY created_at"#,
    )
    .bind(user_id)
    .fetch_all(&app.pool)
    .await
    .expect("key rows");
    assert_eq!(rows.len(), 2, "old + replacement rows");
    let old_expires: Option<chrono::DateTime<Utc>> =
        rows[0].try_get("expires_at").expect("expires_at");
    let new_expires: Option<chrono::DateTime<Utc>> =
        rows[1].try_get("expires_at").expect("expires_at");
    assert!(old_expires.is_some(), "the old row expires");
    assert!(new_expires.is_none(), "the replacement never expires");

    // The rotation is audited — without either secret or hash.
    let audit = sqlx::query(
        r#"SELECT action, after FROM audit_logs WHERE actor_user_id = $1 AND action = 'api_key.rotated'"#,
    )
    .bind(user_id)
    .fetch_one(&app.pool)
    .await
    .expect("api_key.rotated audit row");
    let action: String = audit.try_get("action").expect("action");
    let after: Option<serde_json::Value> = audit.try_get("after").expect("after");
    assert_eq!(action, "api_key.rotated");
    let after = after.expect("audit after");
    assert_eq!(
        after["old_api_key_id"].as_str().unwrap(),
        old_id.to_string()
    );
    assert_eq!(
        after["new_api_key_id"].as_str().unwrap(),
        body["id"].as_str().unwrap()
    );
    assert!(
        !after.to_string().contains(&old_secret),
        "audit has no secret"
    );
    assert!(
        !after.to_string().contains(new_secret),
        "audit has no new secret either"
    );
}

#[tokio::test]
async fn rotate_keeps_old_secret_valid_until_overlap_then_kills_it() {
    let clock = Arc::new(ManualClock::new(Utc::now()));
    let app = keys_app_with_clock(clock.clone()).await;
    let (_user_id, token) = authed_user(&app).await;

    let res = create_key(&app, &token, "CI deploy", &["jobs:read"]).await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.text());
    let created = res.json();
    let old_id: Uuid = created["id"].as_str().unwrap().parse().unwrap();
    let old_secret = created["secret"].as_str().unwrap().to_string();

    let res = rotate_key(&app, &token, old_id).await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.text());
    let new_secret = res.json()["secret"].as_str().unwrap().to_string();

    // Inside the window both secrets authenticate (the jobs
    // stub answers 501 once the key passes the scope check).
    assert_eq!(
        probe_key(&app, &old_secret).await.status,
        StatusCode::NOT_IMPLEMENTED
    );
    assert_eq!(
        probe_key(&app, &new_secret).await.status,
        StatusCode::NOT_IMPLEMENTED
    );

    // One second before the window closes the old secret
    // still works.
    clock.advance(Duration::seconds(86_400 - 1));
    assert_eq!(
        probe_key(&app, &old_secret).await.status,
        StatusCode::NOT_IMPLEMENTED
    );

    // Past the window the old secret is dead — the generic
    // 401, indistinguishable from unknown — while the new
    // secret keeps working.
    clock.advance(Duration::seconds(2));
    let res = probe_key(&app, &old_secret).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());
    assert_eq!(res.json()["code"].as_str().unwrap(), "PF-1001");
    assert_eq!(
        probe_key(&app, &new_secret).await.status,
        StatusCode::NOT_IMPLEMENTED
    );
}

#[tokio::test]
async fn rotate_unknown_or_foreign_id_is_404() {
    let app = keys_app().await;
    let (_user_id, token) = authed_user(&app).await;

    // Unknown id.
    let res = rotate_key(&app, &token, Uuid::now_v7()).await;
    assert_eq!(res.status, StatusCode::NOT_FOUND, "{}", res.text());
    assert_eq!(res.json()["code"].as_str().unwrap(), "PF-2010");

    // Another user's key: the same 404, no enumeration.
    let (_other_id, other_token) = authed_user(&app).await;
    let res = create_key(&app, &other_token, "theirs", &["jobs:read"]).await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.text());
    let foreign_id: Uuid = res.json()["id"].as_str().unwrap().parse().unwrap();
    let res = rotate_key(&app, &token, foreign_id).await;
    assert_eq!(res.status, StatusCode::NOT_FOUND, "{}", res.text());
    assert_eq!(res.json()["code"].as_str().unwrap(), "PF-2010");
}

#[tokio::test]
async fn rotate_revoked_key_is_404() {
    let app = keys_app().await;
    let (user_id, token) = authed_user(&app).await;

    let res = create_key(&app, &token, "CI deploy", &["jobs:read"]).await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.text());
    let key_id: Uuid = res.json()["id"].as_str().unwrap().parse().unwrap();
    // P1-036 owns the revoke path; the row is flipped
    // directly here.
    sqlx::query("UPDATE api_keys SET revoked_at = now() WHERE id = $1")
        .bind(key_id)
        .execute(&app.pool)
        .await
        .expect("revoke key");
    let _ = user_id;

    let res = rotate_key(&app, &token, key_id).await;
    assert_eq!(res.status, StatusCode::NOT_FOUND, "{}", res.text());
    assert_eq!(res.json()["code"].as_str().unwrap(), "PF-2010");
}

#[tokio::test]
async fn rotate_requires_jwt_key_cannot_rotate() {
    let app = keys_app().await;
    let (_user_id, token) = authed_user(&app).await;

    let res = create_key(&app, &token, "CI deploy", &["jobs:read"]).await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.text());
    let key_id: Uuid = res.json()["id"].as_str().unwrap().parse().unwrap();
    let secret = res.json()["secret"].as_str().unwrap().to_string();

    // No credential at all: 401.
    let res = app
        .request(
            Request::builder()
                .method(Method::POST)
                .uri(format!("/v1/api-keys/{key_id}/rotate"))
                .header("x-request-id", REQUEST_ID)
                .header("x-forwarded-for", unique_xff())
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());

    // A live key as the credential: 403 PF-1002 — the rotate
    // route is not in the extractor's scope table, so no key
    // may call it (a key cannot rotate itself or any other).
    let res = rotate_key(&app, &secret, key_id).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.text());
    assert_eq!(res.json()["code"].as_str().unwrap(), "PF-1002");
}

#[tokio::test]
async fn rotate_at_full_cap_succeeds() {
    // Rotation is a replacement, not an addition: a user
    // with a full 10 live keys can still rotate (a create
    // at that point is 409 — proven by
    // tenth_key_succeeds_eleventh_is_409).
    let app = keys_app().await;
    let (_user_id, token) = authed_user(&app).await;

    let mut first_id = Uuid::nil();
    for n in 1..=10 {
        let res = create_key(&app, &token, &format!("key {n}"), &["jobs:read"]).await;
        assert_eq!(res.status, StatusCode::CREATED, "key {n}: {}", res.text());
        if n == 1 {
            first_id = res.json()["id"].as_str().unwrap().parse().unwrap();
        }
    }
    let res = rotate_key(&app, &token, first_id).await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.text());
}

#[tokio::test]
async fn rotate_at_cap_succeeds_and_frees_the_slot_after_overlap() {
    // Nine keys, then a rotation: during the overlap the old
    // row still counts, so the user is at the cap (9 + 1
    // replacement + 1 expiring = 10 counted) and no further
    // create fits. Past the overlap the old row stops
    // counting, so the slot frees. (Rotation itself is exempt
    // from the cap — it replaces, it does not add — which is
    // why the rotate at 9 keys succeeds and why a user with
    // a full 10 can still rotate.)
    let clock = Arc::new(ManualClock::new(Utc::now()));
    let app = keys_app_with_clock(clock.clone()).await;
    let (user_id, mut token) = authed_user(&app).await;
    let email = {
        // The test user was registered with a fresh address
        // inside authed_user; re-login needs that address —
        // read it back from the users row instead of guessing.
        let row = sqlx::query(r#"SELECT email FROM users WHERE id = $1"#)
            .bind(user_id)
            .fetch_one(&app.pool)
            .await
            .expect("user row");
        row.try_get::<String, _>("email").expect("email")
    };

    let mut first_id = Uuid::nil();
    for n in 1..=9 {
        let res = create_key(&app, &token, &format!("key {n}"), &["jobs:read"]).await;
        assert_eq!(res.status, StatusCode::CREATED, "key {n}: {}", res.text());
        if n == 1 {
            first_id = res.json()["id"].as_str().unwrap().parse().unwrap();
        }
    }
    // Rotation succeeds (exempt from the cap) — but the old
    // row still counts until its window closes, so no
    // further create fits yet.
    let res = rotate_key(&app, &token, first_id).await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.text());
    let res = create_key(&app, &token, "key 10", &["jobs:read"]).await;
    assert_eq!(res.status, StatusCode::CONFLICT, "{}", res.text());
    assert_eq!(res.json()["code"].as_str().unwrap(), "PF-2011");

    // Past the overlap the old row stops counting, so the
    // slot frees (re-login: the frozen clock advanced past
    // the first access token's 15-minute life).
    clock.advance(Duration::seconds(86_401));
    token = login_token(&app, &email).await;
    let res = create_key(&app, &token, "key 10", &["jobs:read"]).await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.text());
    // The count reads the app's frozen clock — the wall
    // clock is still inside the overlap window.
    let live = detag_core::repos::api_keys::count_live_for_user(&app.pool, user_id, clock.now())
        .await
        .expect("count live keys");
    assert_eq!(live, 10, "10 live keys again after the slot freed");
}

fn revoke_key_req(token: &str, key_id: Uuid) -> Request<axum::body::Body> {
    Request::builder()
        .method(Method::DELETE)
        .uri(format!("/v1/api-keys/{key_id}"))
        .header("x-request-id", REQUEST_ID)
        .header("x-forwarded-for", unique_xff())
        .header("authorization", format!("Bearer {token}"))
        .body(axum::body::Body::empty())
        .unwrap()
}

async fn revoke_key(app: &TestApp, token: &str, key_id: Uuid) -> common::TestResponse {
    app.request(revoke_key_req(token, key_id)).await
}

#[tokio::test]
async fn revoke_happy_path_kills_key_immediately() {
    let app = keys_app().await;
    let (user_id, token) = authed_user(&app).await;

    let res = create_key(&app, &token, "CI deploy", &["jobs:read"]).await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.text());
    let key_id: Uuid = res.json()["id"].as_str().unwrap().parse().unwrap();
    let secret = res.json()["secret"].as_str().unwrap().to_string();
    let hash = sha256_hex(secret.as_bytes());

    // Live before: the probe runs the stub (501), and the hash
    // is not denylisted yet.
    let res = probe_key(&app, &secret).await;
    assert_eq!(res.status, StatusCode::NOT_IMPLEMENTED, "{}", res.text());
    assert!(
        !is_api_key_revoked(&app.state, &hash)
            .await
            .expect("denylist read"),
        "a fresh key is not denylisted"
    );

    let res = revoke_key(&app, &token, key_id).await;
    assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.text());

    // Dead immediately: the next use is 401 PF-1001 — no
    // grace, no overlap (unlike rotation).
    let res = probe_key(&app, &secret).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());
    assert_eq!(res.json()["code"].as_str().unwrap(), "PF-1001");

    // The row carries revoked_at, and the hash is denylisted.
    let row = sqlx::query(r#"SELECT revoked_at FROM api_keys WHERE id = $1"#)
        .bind(key_id)
        .fetch_one(&app.pool)
        .await
        .expect("key row");
    let revoked_at: Option<chrono::DateTime<chrono::Utc>> =
        row.try_get("revoked_at").expect("revoked_at");
    assert!(revoked_at.is_some(), "revoked_at stamped");
    assert!(
        is_api_key_revoked(&app.state, &hash)
            .await
            .expect("denylist read"),
        "a revoked hash is denylisted"
    );

    // Audited once — without the secret or its hash.
    let audits = sqlx::query(
        r#"SELECT action, after FROM audit_logs WHERE actor_user_id = $1 AND action = 'api_key.revoked'"#,
    )
    .bind(user_id)
    .fetch_all(&app.pool)
    .await
    .expect("api_key.revoked audit rows");
    assert_eq!(audits.len(), 1, "exactly one revoke audit");
    let after: Option<serde_json::Value> = audits[0].try_get("after").expect("after");
    let after = after.expect("audit after");
    assert_eq!(after["api_key_id"].as_str().unwrap(), key_id.to_string());
    assert_eq!(after["name"].as_str().unwrap(), "CI deploy");
    assert_eq!(after["prefix"].as_str().unwrap(), &secret[..12]);
    assert!(!after.to_string().contains(&secret), "audit has no secret");
    assert!(
        !after.to_string().contains(&hash),
        "audit has no hash either"
    );
}

#[tokio::test]
async fn revoke_unknown_or_foreign_id_is_404() {
    let app = keys_app().await;
    let (_user_id, token) = authed_user(&app).await;

    // Unknown id.
    let res = revoke_key(&app, &token, Uuid::now_v7()).await;
    assert_eq!(res.status, StatusCode::NOT_FOUND, "{}", res.text());
    assert_eq!(res.json()["code"].as_str().unwrap(), "PF-2010");

    // Another user's key: the same 404, no enumeration — and
    // the foreign key still works afterwards (not revoked).
    let (_other_id, other_token) = authed_user(&app).await;
    let res = create_key(&app, &other_token, "theirs", &["jobs:read"]).await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.text());
    let foreign_id: Uuid = res.json()["id"].as_str().unwrap().parse().unwrap();
    let foreign_secret = res.json()["secret"].as_str().unwrap().to_string();
    let res = revoke_key(&app, &token, foreign_id).await;
    assert_eq!(res.status, StatusCode::NOT_FOUND, "{}", res.text());
    assert_eq!(res.json()["code"].as_str().unwrap(), "PF-2010");
    let res = probe_key(&app, &foreign_secret).await;
    assert_eq!(res.status, StatusCode::NOT_IMPLEMENTED, "{}", res.text());
}

#[tokio::test]
async fn revoke_already_revoked_is_noop_success() {
    let app = keys_app().await;
    let (user_id, token) = authed_user(&app).await;

    let res = create_key(&app, &token, "CI deploy", &["jobs:read"]).await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.text());
    let key_id: Uuid = res.json()["id"].as_str().unwrap().parse().unwrap();

    // First revoke: 204. Second revoke of the same (dead) key:
    // still 204 — DELETE is idempotent — and no second audit.
    let res = revoke_key(&app, &token, key_id).await;
    assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.text());
    let res = revoke_key(&app, &token, key_id).await;
    assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.text());

    let audits = sqlx::query(
        r#"SELECT action FROM audit_logs WHERE actor_user_id = $1 AND action = 'api_key.revoked'"#,
    )
    .bind(user_id)
    .fetch_all(&app.pool)
    .await
    .expect("api_key.revoked audit rows");
    assert_eq!(audits.len(), 1, "the no-op second revoke audits nothing");
}

#[tokio::test]
async fn revoke_requires_jwt_key_cannot_revoke() {
    let app = keys_app().await;
    let (_user_id, token) = authed_user(&app).await;

    let res = create_key(&app, &token, "CI deploy", &["jobs:read"]).await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.text());
    let key_id: Uuid = res.json()["id"].as_str().unwrap().parse().unwrap();
    let secret = res.json()["secret"].as_str().unwrap().to_string();

    // No credential at all: 401.
    let res = app
        .request(
            Request::builder()
                .method(Method::DELETE)
                .uri(format!("/v1/api-keys/{key_id}"))
                .header("x-request-id", REQUEST_ID)
                .header("x-forwarded-for", unique_xff())
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());

    // A live key as the credential: 403 PF-1002 — the revoke
    // route is not in the extractor's scope table, so no key
    // may call it (a key cannot revoke itself or any other).
    let res = revoke_key(&app, &secret, key_id).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.text());
    assert_eq!(res.json()["code"].as_str().unwrap(), "PF-1002");

    // The key is still live afterwards.
    let res = probe_key(&app, &secret).await;
    assert_eq!(res.status, StatusCode::NOT_IMPLEMENTED, "{}", res.text());
}

#[tokio::test]
async fn revoke_frees_cap_slot() {
    let app = keys_app().await;
    let (user_id, token) = authed_user(&app).await;

    let mut first_id = Uuid::nil();
    for n in 1..=10 {
        let res = create_key(&app, &token, &format!("key {n}"), &["jobs:read"]).await;
        assert_eq!(res.status, StatusCode::CREATED, "key {n}: {}", res.text());
        if n == 1 {
            first_id = res.json()["id"].as_str().unwrap().parse().unwrap();
        }
    }
    // At the cap: one more is 409.
    let res = create_key(&app, &token, "key 11", &["jobs:read"]).await;
    assert_eq!(res.status, StatusCode::CONFLICT, "{}", res.text());

    // Revoking one frees its slot: the next create is 201,
    // and the user is back at 10 live keys.
    let res = revoke_key(&app, &token, first_id).await;
    assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.text());
    let res = create_key(&app, &token, "key 11", &["jobs:read"]).await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.text());
    let live = detag_core::repos::api_keys::count_live_for_user(&app.pool, user_id, Utc::now())
        .await
        .expect("count live keys");
    assert_eq!(live, 10, "10 live keys again after the revoke");
}
