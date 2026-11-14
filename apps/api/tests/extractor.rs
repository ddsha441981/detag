//! P1-022: the JWT verification extractor, end to end.
//!
//! The extractor is the full `AuthContext` now: it verifies
//! signature + expiry + not-before (leeway 0), loads the
//! `sub` user from the database, and takes the role from the
//! ROW — never from the token's embedded `role` claim. So:
//! - a tampered token (flipped payload, wrong secret, forged
//!   role claim) is rejected or demoted by the database, not
//!   trusted;
//! - a role change in the database takes effect immediately
//!   on the SAME token (no re-login, no new token);
//! - a token whose `nbf` is in the future is immature
//!   (401 `PF-1010`), never expired (401 `PF-1011`);
//! - an API key verifies by hash lookup (P1-034): a live key
//!   with the route's scope reaches the handler (the jobs
//!   stubs answer 501 — auth passed); a key without the scope,
//!   or on a route the scope table does not name, is 403
//!   `PF-1002`; an unknown or revoked key, or a key whose
//!   owner is not `active`, is the generic 401 `PF-1001`; a
//!   verified use re-stamps `last_used_at`, throttled to once
//!   per 300 s.

mod common;

use axum::http::{Method, Request, StatusCode};
use common::{TestApp, TestFixtures};
use detag_core::jwt::{issue_access_token, verify_access_token, ACCESS_TOKEN_TTL_SECS};
use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use serde_json::json;
use uuid::Uuid;

/// The request id: a valid UUID, echoed by the middleware.
const REQUEST_ID: &str = "123e4567-e89b-42d3-a456-426614174106";

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

/// Set a user's role directly in the database (there is no
/// self-serve way to become an admin — the role is set by the
/// owner out of band; the admin_suspend tests do the same).
async fn set_role(app: &TestApp, user_id: Uuid, role: &str) {
    sqlx::query("UPDATE users SET role = $1 WHERE id = $2")
        .bind(role)
        .bind(user_id)
        .execute(&app.pool)
        .await
        .expect("role update");
}

/// `GET /v1/me` with a Bearer token.
async fn get_me(app: &TestApp, token: &str) -> common::TestResponse {
    app.request(
        Request::builder()
            .method(Method::GET)
            .uri("/v1/me")
            .header("x-request-id", REQUEST_ID)
            .header("x-forwarded-for", unique_xff())
            .header("authorization", format!("Bearer {token}"))
            .body(axum::body::Body::empty())
            .unwrap(),
    )
    .await
}

/// `GET /v1/admin/users` with a Bearer token (the
/// `AdminReadContext` route — admin + support in, anyone
/// else 403 `PF-1002`).
async fn admin_list(app: &TestApp, token: &str) -> common::TestResponse {
    app.request(
        Request::builder()
            .method(Method::GET)
            .uri("/v1/admin/users")
            .header("x-request-id", REQUEST_ID)
            .header("x-forwarded-for", unique_xff())
            .header("authorization", format!("Bearer {token}"))
            .body(axum::body::Body::empty())
            .unwrap(),
    )
    .await
}

#[tokio::test]
async fn tampered_token_is_rejected() {
    // P1-022: the signature covers the whole token — any
    // change to the payload breaks it, and the extractor
    // answers 401 PF-1010 (invalid token), never a partial
    // success.
    let app = app().await;
    let email = unique_email("tamper");
    register_user(&app, &email).await;
    let token = login_token(&app, &email).await;

    // Flip one character of the payload segment.
    let mut parts: Vec<&str> = token.split('.').collect();
    assert_eq!(parts.len(), 3);
    let mut payload = parts[1].to_string();
    let first = payload.remove(0);
    let flipped = if first == 'A' { 'B' } else { 'A' };
    payload.insert(0, flipped);
    parts[1] = &payload;
    let tampered = parts.join(".");
    let res = get_me(&app, &tampered).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-1010", "{}", res.text());

    // The same claims signed with a different secret.
    let claims = verify_access_token(&app.settings.jwt_secret, &token).expect("token verifies");
    let forged = encode(
        &Header::new(Algorithm::HS256),
        &json!({
            "sub": claims.sub,
            "role": claims.role,
            "ver": claims.ver,
            "jti": claims.jti,
            "exp": claims.exp,
            "iat": claims.iat,
            "nbf": claims.nbf,
            "iss": claims.iss,
            "aud": claims.aud,
        }),
        &EncodingKey::from_secret(b"a-different-secret-that-is-long-enough"),
    )
    .expect("forged token encodes");
    let res = get_me(&app, &forged).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-1010", "{}", res.text());

    // The untouched token still works (the tampering tests
    // did not poison anything).
    let res = get_me(&app, &token).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
}

#[tokio::test]
async fn forged_role_claim_is_ignored_the_database_decides() {
    // P1-022: the token's embedded `role` is NEVER trusted —
    // the extractor takes the role from the user row. A token
    // minted with `role: "admin"` (as if the attacker edited
    // the claim and re-signed it — which they cannot do
    // without the secret; here the test mints it directly to
    // prove the extractor ignores the claim) for a plain user
    // still acts as that user: the profile works, the admin
    // list is 403 PF-1002.
    let app = app().await;
    let email = unique_email("forged-role");
    let user_id = register_user(&app, &email).await;
    let token = issue_access_token(
        &app.settings.jwt_secret,
        user_id,
        "admin",
        ACCESS_TOKEN_TTL_SECS,
    )
    .expect("forged-role token issues");
    // The claim really says admin (the test built it that way).
    let claims = verify_access_token(&app.settings.jwt_secret, &token).expect("token verifies");
    assert_eq!(claims.role, "admin");

    let res = get_me(&app, &token).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let res = admin_list(&app, &token).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-1002", "{}", res.text());
}

#[tokio::test]
async fn future_nbf_token_is_immature_not_expired() {
    // P1-022: a token whose `nbf` is an hour in the future is
    // not yet valid — 401 PF-1010 (invalid), never 401
    // PF-1011 (expired). And a token with `nbf` in the past
    // verifies normally.
    let app = app().await;
    let email = unique_email("nbf");
    let user_id = register_user(&app, &email).await;
    let now = chrono::Utc::now();
    let base = json!({
        "sub": user_id.to_string(),
        "role": "user",
        "ver": 1,
        "jti": Uuid::now_v7().to_string(),
        "exp": (now + chrono::Duration::seconds(ACCESS_TOKEN_TTL_SECS)).timestamp(),
        "iat": now.timestamp(),
        "iss": "detag",
        "aud": "detag-api",
    });
    let mut future = base.clone();
    future["nbf"] = json!(now.timestamp() + 3600);
    let token = encode(
        &Header::new(Algorithm::HS256),
        &future,
        &EncodingKey::from_secret(app.settings.jwt_secret.as_bytes()),
    )
    .expect("future-nbf token encodes");
    let res = get_me(&app, &token).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-1010", "{}", res.text());

    let mut past = base.clone();
    past["nbf"] = json!(now.timestamp() - 10);
    let token = encode(
        &Header::new(Algorithm::HS256),
        &past,
        &EncodingKey::from_secret(app.settings.jwt_secret.as_bytes()),
    )
    .expect("past-nbf token encodes");
    let res = get_me(&app, &token).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
}

#[tokio::test]
async fn role_refresh_from_database() {
    // P1-022: the role is re-read from the database on EVERY
    // request, so a role change takes effect immediately on
    // the SAME token — no re-login, no new token.
    let app = app().await;
    let email = unique_email("role-refresh");
    let user_id = register_user(&app, &email).await;
    set_role(&app, user_id, "admin").await;
    let token = login_token(&app, &email).await;
    let res = admin_list(&app, &token).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());

    // Demote in the database: the SAME token loses admin
    // access at once, but still works as the user.
    set_role(&app, user_id, "user").await;
    let res = admin_list(&app, &token).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-1002", "{}", res.text());
    let res = get_me(&app, &token).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());

    // Promote back: the SAME token is an admin again.
    set_role(&app, user_id, "admin").await;
    let res = admin_list(&app, &token).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());

    // And the reverse direction: a token minted while the
    // user was a plain user (its claims say role=user) gains
    // admin access the moment the row says admin — the
    // embedded role is never consulted.
    let email_b = unique_email("role-refresh-b");
    let user_b = register_user(&app, &email_b).await;
    let token_b = login_token(&app, &email_b).await;
    let res = admin_list(&app, &token_b).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.text());
    set_role(&app, user_b, "admin").await;
    let res = admin_list(&app, &token_b).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
}

/// Mint an API key for the JWT caller; return (key id, secret).
async fn mint_key(app: &TestApp, token: &str, scopes: &[&str]) -> (Uuid, String) {
    let body = json!({
        "name": "test key",
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
    let json = res.json();
    (
        json["id"].as_str().unwrap().parse().unwrap(),
        json["secret"].as_str().unwrap().to_string(),
    )
}

/// Any request with a Bearer API key (method + uri).
async fn key_request(app: &TestApp, method: Method, uri: &str, key: &str) -> common::TestResponse {
    app.request(
        Request::builder()
            .method(method)
            .uri(uri)
            .header("x-request-id", REQUEST_ID)
            .header("x-forwarded-for", unique_xff())
            .header("authorization", format!("Bearer {key}"))
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
    use sqlx::Row;
    row.try_get("last_used_at").expect("last_used_at column")
}

/// Register + log in + mint a key; return (user id, key id, secret).
async fn key_for(app: &TestApp, scopes: &[&str]) -> (Uuid, Uuid, String) {
    let email = unique_email("key-user");
    let user_id = register_user(app, &email).await;
    let token = login_token(app, &email).await;
    let (key_id, secret) = mint_key(app, &token, scopes).await;
    (user_id, key_id, secret)
}

#[tokio::test]
async fn unknown_api_key_is_unauthenticated() {
    // P1-034: a `pf_live_*` string that matches no live key
    // is the generic 401 PF-1001 — the same as no credential.
    // (The old P1-022 test asserted this for EVERY key; now
    // only unknown keys answer it — a real key authenticates,
    // proven by the tests below.)
    let app = app().await;
    let res = key_request(
        &app,
        Method::GET,
        "/v1/jobs",
        "pf_live_AAAAAAAAAAAAAAAAAAAAAAAA",
    )
    .await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-1001", "{}", res.text());
}

#[tokio::test]
async fn api_key_authenticates_with_scope() {
    // P1-034: a live key with the route's scope reaches the
    // handler — the jobs/webhook stubs answer 501, which is
    // the proof auth passed (an unauthenticated caller gets
    // 401, a scope-denied one 403 — never the stub).
    let app = app().await;
    let (_, _, write_key) = key_for(&app, &["jobs:write"]).await;
    let res = key_request(&app, Method::POST, "/v1/jobs", &write_key).await;
    assert_eq!(res.status, StatusCode::NOT_IMPLEMENTED, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-5004", "{}", res.text());

    let (_, _, read_key) = key_for(&app, &["jobs:read"]).await;
    let res = key_request(&app, Method::GET, "/v1/jobs", &read_key).await;
    assert_eq!(res.status, StatusCode::NOT_IMPLEMENTED, "{}", res.text());

    let (_, _, hook_key) = key_for(&app, &["webhooks:manage"]).await;
    let res = key_request(&app, Method::GET, "/v1/webhook-endpoints", &hook_key).await;
    assert_eq!(res.status, StatusCode::NOT_IMPLEMENTED, "{}", res.text());
}

#[tokio::test]
async fn api_key_scope_check_enforced() {
    // P1-034 (the ticket's AC): a key WITHOUT the route's
    // scope is 403 PF-1002 — scopes are checked independently,
    // `jobs:write` does not imply `jobs:read` and neither
    // implies `webhooks:manage`.
    let app = app().await;
    let (_, _, read_key) = key_for(&app, &["jobs:read"]).await;
    let res = key_request(&app, Method::POST, "/v1/jobs", &read_key).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-1002", "{}", res.text());

    let (_, _, write_key) = key_for(&app, &["jobs:write"]).await;
    let res = key_request(&app, Method::GET, "/v1/jobs", &write_key).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-1002", "{}", res.text());
    let res = key_request(&app, Method::GET, "/v1/webhook-endpoints", &write_key).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.text());

    let res = key_request(
        &app,
        Method::POST,
        "/v1/jobs/019aa1b2-c3d4-7e5f-8000-000000000001/attestation",
        &read_key,
    )
    .await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-1002", "{}", res.text());
}

#[tokio::test]
async fn api_key_denied_on_unmapped_routes() {
    // P1-034: the scope table names jobs + webhook endpoints
    // only — a key carrying ALL THREE scopes still gets 403
    // PF-1002 on /v1/me, on admin routes, and on key
    // management itself (keys cannot mint keys). Fail closed.
    let app = app().await;
    let (_, _, key) = key_for(&app, &["jobs:write", "jobs:read", "webhooks:manage"]).await;
    for (method, uri) in [
        (Method::GET, "/v1/me"),
        (Method::GET, "/v1/admin/users"),
        (Method::POST, "/v1/api-keys"),
    ] {
        let res = key_request(&app, method.clone(), uri, &key).await;
        assert_eq!(
            res.status,
            StatusCode::FORBIDDEN,
            "{method} {uri}: {}",
            res.text()
        );
        assert_eq!(
            res.json()["code"],
            "PF-1002",
            "{method} {uri}: {}",
            res.text()
        );
    }
}

#[tokio::test]
async fn revoked_api_key_is_unauthenticated() {
    // P1-034: a revoked key reads exactly like an unknown one
    // — 401 PF-1001, never a distinct code (the lookup is
    // live-only; P1-036's DELETE flips the row, this test
    // flips it directly).
    let app = app().await;
    let (_, key_id, secret) = key_for(&app, &["jobs:write"]).await;
    let res = key_request(&app, Method::POST, "/v1/jobs", &secret).await;
    assert_eq!(res.status, StatusCode::NOT_IMPLEMENTED, "{}", res.text());

    sqlx::query("UPDATE api_keys SET revoked_at = now() WHERE id = $1")
        .bind(key_id)
        .execute(&app.pool)
        .await
        .expect("revoke key");
    let res = key_request(&app, Method::POST, "/v1/jobs", &secret).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-1001", "{}", res.text());
}

#[tokio::test]
async fn api_key_of_suspended_user_is_unauthenticated() {
    // P1-034 / ADR-0024: suspend revokes sessions but not
    // keys, and a key has no expiry — so the extractor
    // rejects a key whose owner is not `active` (401
    // PF-1001, indistinguishable from an unknown key).
    // Without this, a suspended user's key would work forever.
    let app = app().await;
    let (user_id, _, secret) = key_for(&app, &["jobs:write"]).await;
    let res = key_request(&app, Method::POST, "/v1/jobs", &secret).await;
    assert_eq!(res.status, StatusCode::NOT_IMPLEMENTED, "{}", res.text());

    sqlx::query("UPDATE users SET status = 'suspended' WHERE id = $1")
        .bind(user_id)
        .execute(&app.pool)
        .await
        .expect("suspend user");
    let res = key_request(&app, Method::POST, "/v1/jobs", &secret).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-1001", "{}", res.text());
}

#[tokio::test]
async fn api_key_of_admin_user_cannot_reach_admin_routes() {
    // P1-034: keys never reach admin routes — not even a key
    // minted by an admin user. The scope table names no admin
    // route (403 at the extractor), and the admin guards reject
    // a key-carrying context as a second line.
    let app = app().await;
    let (user_id, _, secret) = key_for(&app, &["jobs:write", "jobs:read", "webhooks:manage"]).await;
    set_role(&app, user_id, "admin").await;
    let res = admin_list(&app, &secret).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-1002", "{}", res.text());
}

#[tokio::test]
async fn last_used_at_is_throttled() {
    // P1-034: a verified use stamps `last_used_at` (NULL
    // before first use), a second use inside the 300 s
    // throttle leaves the stamp untouched, and a use after
    // the stamp goes stale re-stamps it. A denied request is
    // not a use (the scope-denied call below leaves the stamp
    // alone).
    let app = app().await;
    let (_, key_id, secret) = key_for(&app, &["jobs:write"]).await;
    assert_eq!(last_used_at(&app, key_id).await, None, "fresh key unused");

    let res = key_request(&app, Method::POST, "/v1/jobs", &secret).await;
    assert_eq!(res.status, StatusCode::NOT_IMPLEMENTED, "{}", res.text());
    let first = last_used_at(&app, key_id)
        .await
        .expect("stamped on first use");

    // A scope-denied call is not a use.
    let res = key_request(&app, Method::GET, "/v1/jobs", &secret).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.text());
    assert_eq!(last_used_at(&app, key_id).await, Some(first));

    // A second verified use inside the throttle window does
    // not rewrite the stamp.
    let res = key_request(&app, Method::POST, "/v1/jobs", &secret).await;
    assert_eq!(res.status, StatusCode::NOT_IMPLEMENTED, "{}", res.text());
    assert_eq!(last_used_at(&app, key_id).await, Some(first));

    // Age the stamp past the throttle, then use again: it
    // re-stamps to ~now.
    sqlx::query("UPDATE api_keys SET last_used_at = now() - interval '10 minutes' WHERE id = $1")
        .bind(key_id)
        .execute(&app.pool)
        .await
        .expect("age the stamp");
    let aged = last_used_at(&app, key_id).await.expect("aged stamp");
    assert!(aged < first, "the stamp really aged");
    let res = key_request(&app, Method::POST, "/v1/jobs", &secret).await;
    assert_eq!(res.status, StatusCode::NOT_IMPLEMENTED, "{}", res.text());
    let restamped = last_used_at(&app, key_id).await.expect("re-stamped");
    assert!(restamped > aged, "a stale stamp is re-stamped on use");
}
