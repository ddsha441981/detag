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
//! - an API key still answers 401 `PF-1001` — the key leg is
//!   the E1.4 integration point and its verification lands
//!   with P1-034 ("Key auth extractor").

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

#[tokio::test]
async fn api_key_still_rejected_until_e1_4() {
    // P1-022: the API-key leg is the E1.4 integration point
    // (P1-034 fills it in) — until then a `pf_live_*` key
    // answers the generic 401 PF-1001, the same as no
    // credential.
    let app = app().await;
    let res = get_me(&app, "pf_live_AAAAAAAAAAAAAAAAAAAAAAAA").await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-1001", "{}", res.text());
}
