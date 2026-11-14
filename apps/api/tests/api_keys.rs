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
//! Key verification itself is P1-034's ticket: these tests
//! assert the stored shape the extractor will read, not that
//! a key authenticates yet.

mod common;

use axum::http::{Method, Request, StatusCode};
use common::{TestApp, TestFixtures};
use detag_core::checksum::sha256_hex;
use serde_json::json;
use sqlx::Row;
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
    let live = detag_core::repos::api_keys::count_live_for_user(&app.pool, user_id)
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
    let live = detag_core::repos::api_keys::count_live_for_user(&app.pool, user_id)
        .await
        .expect("count live keys");
    assert_eq!(
        live, 10,
        "the table holds exactly 10 live keys for this user"
    );
}
