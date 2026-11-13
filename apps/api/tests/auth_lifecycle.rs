//! P1-023: auth integration suite — the whole lifecycle in
//! one continuous scenario, plus clock-injected expiry and
//! token tampering.
//!
//! Through the real router (`TestApp` oneshot) against the
//! migrated scratch database, on a frozen
//! [`ManualClock`](detag_core::clock::ManualClock) the test
//! advances itself — so expiry is proven by moving time,
//! never by waiting it out or by hand-seeding expired rows:
//!
//! - the lifecycle: register → verify → login → refresh →
//!   rotate → reuse-attack → logout, with the session list
//!   proving each step's effect on the live sessions;
//! - clock-injected expiry: the access token dies 15 minutes
//!   after issue (401 `PF-1011`), the session dies 30 days
//!   after login (refresh → 401 `PF-1001`), the verification
//!   token dies 24 hours after register (401 `PF-1011`);
//! - token tampering: a flipped payload byte, a foreign
//!   secret, garbage, and a future-`nbf` token are all 401
//!   `PF-1010` — and the future-`nbf` token verifies once
//!   the clock reaches its `nbf`.
//!
//! The clock starts at the real now (frozen): DB timestamps
//! (`created_at`, `last_seen` — SQL `now()`) and the frozen
//! clock stay in the same neighbourhood, so lockout windows
//! and rate-limit buckets behave exactly as in production.

mod common;

use axum::http::{Method, Request, StatusCode};
use chrono::{Duration, Utc};
use common::{TestApp, TestFixtures};
use detag_api::services::verification::VERIFY_EMAIL_TEMPLATE;
use detag_core::{
    checksum::sha256_hex,
    clock::{Clock, ManualClock},
    jwt::{issue_access_token, issue_access_token_at, ACCESS_TOKEN_TTL_SECS},
    repos::{audit_logs, email_outbox, sessions},
};
use serde_json::{json, Value};
use std::sync::Arc;
use uuid::Uuid;

/// The request id: a valid UUID, echoed by the middleware.
const REQUEST_ID: &str = "123e4567-e89b-42d3-a456-426614174104";

/// The password every test user registers with.
const PASSWORD: &str = "correct horse battery staple";

/// A different secret from the app's: tokens minted with it
/// must never verify (tampering test).
const OTHER_SECRET: &str = "a-different-test-secret-that-is-long-enough";

/// App on a frozen clock starting at the real now, plus the
/// clock itself (shared `Arc` — the test advances it while
/// the app under test reads it).
async fn lifecycle_app() -> (TestApp, ManualClock) {
    let fixtures = TestFixtures::from_env().await;
    let mut settings = fixtures.settings();
    settings.disposable_email_domains = "mailinator.com".to_string();
    settings.tos_current_version = "v1".to_string();
    let clock = ManualClock::new(Utc::now());
    let app = TestApp::spawn_with_fixtures_settings_and_clock(
        fixtures,
        settings,
        Arc::new(clock.clone()) as Arc<dyn Clock>,
    )
    .await;
    (app, clock)
}

fn unique_email(prefix: &str) -> String {
    format!("{prefix}-{}@example.com", Uuid::now_v7())
}

/// A per-request XFF address: each request gets its own IP
/// bucket, so the tests never trip each other's rate limits
/// (the P1-003 lesson).
fn unique_xff() -> String {
    format!("203.0.113.{}", Uuid::now_v7().as_u128() % 250 + 1)
}

fn post_register(email: &str) -> Request<axum::body::Body> {
    let body = json!({
        "email": email,
        "password": PASSWORD,
        "accept_tos_version": "v1",
    });
    Request::builder()
        .method(Method::POST)
        .uri("/v1/auth/register")
        .header("x-request-id", REQUEST_ID)
        .header("x-forwarded-for", unique_xff())
        .header("content-type", "application/json")
        .body(axum::body::Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap()
}

fn post_verify(token: &str) -> Request<axum::body::Body> {
    let body = json!({ "token": token });
    Request::builder()
        .method(Method::POST)
        .uri("/v1/auth/verify-email")
        .header("x-request-id", REQUEST_ID)
        .header("x-forwarded-for", unique_xff())
        .header("content-type", "application/json")
        .body(axum::body::Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap()
}

fn post_login(email: &str) -> Request<axum::body::Body> {
    let body = json!({ "email": email, "password": PASSWORD });
    Request::builder()
        .method(Method::POST)
        .uri("/v1/auth/login")
        .header("x-request-id", REQUEST_ID)
        .header("x-forwarded-for", unique_xff())
        .header("content-type", "application/json")
        .body(axum::body::Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap()
}

fn post_refresh(cookie: &str) -> Request<axum::body::Body> {
    Request::builder()
        .method(Method::POST)
        .uri("/v1/auth/refresh")
        .header("x-request-id", REQUEST_ID)
        .header("x-forwarded-for", unique_xff())
        .header("cookie", format!("detag_refresh={cookie}"))
        .body(axum::body::Body::empty())
        .unwrap()
}

fn post_logout(cookie: &str) -> Request<axum::body::Body> {
    Request::builder()
        .method(Method::POST)
        .uri("/v1/auth/logout")
        .header("x-request-id", REQUEST_ID)
        .header("x-forwarded-for", unique_xff())
        .header("cookie", format!("detag_refresh={cookie}"))
        .body(axum::body::Body::empty())
        .unwrap()
}

fn get_me(access: &str) -> Request<axum::body::Body> {
    Request::builder()
        .method(Method::GET)
        .uri("/v1/me")
        .header("x-request-id", REQUEST_ID)
        .header("x-forwarded-for", unique_xff())
        .header("authorization", format!("Bearer {access}"))
        .body(axum::body::Body::empty())
        .unwrap()
}

fn get_admin_users(access: &str) -> Request<axum::body::Body> {
    Request::builder()
        .method(Method::GET)
        .uri("/v1/admin/users")
        .header("x-request-id", REQUEST_ID)
        .header("x-forwarded-for", unique_xff())
        .header("authorization", format!("Bearer {access}"))
        .body(axum::body::Body::empty())
        .unwrap()
}

fn get_sessions(access: &str) -> Request<axum::body::Body> {
    Request::builder()
        .method(Method::GET)
        .uri("/v1/me/sessions")
        .header("x-request-id", REQUEST_ID)
        .header("x-forwarded-for", unique_xff())
        .header("authorization", format!("Bearer {access}"))
        .body(axum::body::Body::empty())
        .unwrap()
}

/// Register a user, return its id.
async fn register_user(app: &TestApp, email: &str) -> Uuid {
    let res = app.request(post_register(email)).await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.text());
    res.json()["id"].as_str().unwrap().parse().unwrap()
}

/// The latest unsent verification-mail token for a user —
/// read from the outbox payload, the only place the raw
/// token exists outside the email (the DB holds its hash).
async fn token_for_user(app: &TestApp, user_id: Uuid) -> String {
    let mail = email_outbox::unsent_for_template(&app.state.pool, VERIFY_EMAIL_TEMPLATE)
        .await
        .unwrap();
    let ours: Vec<_> = mail
        .iter()
        .filter(|row| row.payload["user_id"] == json!(user_id))
        .collect();
    assert_eq!(ours.len(), 1, "{mail:?}");
    ours[0].payload["token"].as_str().unwrap().to_string()
}

/// Log in, return (access token, refresh token).
async fn login_tokens(app: &TestApp, email: &str) -> (String, String) {
    let res = app.request(post_login(email)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let access = res.json()["access_token"].as_str().unwrap().to_string();
    let refresh = refresh_token_of(&res);
    (access, refresh)
}

/// The refresh token from a login/refresh response's
/// `set-cookie` header (`detag_refresh=<token>; ...`).
fn refresh_token_of(res: &common::TestResponse) -> String {
    let cookie = res
        .headers
        .get("set-cookie")
        .expect("set-cookie header")
        .to_str()
        .expect("cookie is ascii");
    let first = cookie.split(';').next().expect("cookie has a value");
    let (_name, value) = first.split_once('=').expect("cookie is name=value");
    assert!(!value.is_empty(), "refresh token is non-empty: {cookie}");
    value.to_string()
}

/// Refresh, return (new access token, new refresh token).
async fn refresh_tokens(app: &TestApp, cookie: &str) -> (String, String) {
    let res = app.request(post_refresh(cookie)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let access = res.json()["access_token"].as_str().unwrap().to_string();
    let refresh = refresh_token_of(&res);
    (access, refresh)
}

/// Set a user's role directly in the database (there is no
/// self-serve way to become an admin — the role is set by the
/// operator; the P1-022 pattern).
async fn set_role(app: &TestApp, user_id: Uuid, role: &str) {
    sqlx::query("UPDATE users SET role = $1 WHERE id = $2")
        .bind(role)
        .bind(user_id)
        .execute(&app.pool)
        .await
        .expect("role update");
}

/// The live sessions for the bearer, as a JSON array.
async fn live_sessions(app: &TestApp, access: &str) -> Vec<Value> {
    let res = app.request(get_sessions(access)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    res.json().as_array().expect("list is an array").clone()
}

/// The problem code of an error response (`PF-xxxx`).
fn problem_code(res: &common::TestResponse) -> String {
    res.json()["code"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn full_lifecycle_register_verify_login_refresh_rotate_reuse_logout() {
    let (app, _clock) = lifecycle_app().await;
    let email = unique_email("lifecycle");

    // 1. Register → 201; the user exists, unverified.
    let user_id = register_user(&app, &email).await;
    let token = token_for_user(&app, user_id).await;

    // 2. Verify → 204; the same token is single-use.
    let res = app.request(post_verify(&token)).await;
    assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.text());
    let res = app.request(post_verify(&token)).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());
    assert_eq!(problem_code(&res), "PF-1010", "{}", res.text());

    // 3. Login → 200; the access token opens /v1/me.
    let (access, refresh) = login_tokens(&app, &email).await;
    let res = app.request(get_me(&access)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert_eq!(res.json()["id"].as_str().unwrap(), user_id.to_string());
    assert_eq!(live_sessions(&app, &access).await.len(), 1);

    // 4. Role refresh (P1-022): the role is re-read from the
    // DB on every request, so the SAME token gains admin
    // access when the row is promoted — and loses it again
    // on demotion (the forged `role` claim is ignored).
    set_role(&app, user_id, "admin").await;
    let res = app.request(get_admin_users(&access)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    set_role(&app, user_id, "user").await;
    let res = app.request(get_admin_users(&access)).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.text());
    assert_eq!(problem_code(&res), "PF-1002", "{}", res.text());

    // 5. Refresh → 200: same family, new tokens; the old
    // access token still works (the ≤ 15-minute residual
    // window — documented, not hidden).
    let (access2, refresh2) = refresh_tokens(&app, &refresh).await;
    assert_ne!(access2, access);
    assert_ne!(refresh2, refresh);
    let res = app.request(get_me(&access)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert_eq!(live_sessions(&app, &access2).await.len(), 1);

    // 6. Reuse attack: presenting the already-rotated
    // refresh token is 401 PF-1007, the whole family is
    // revoked (the fresh token dies too), and the security
    // audit + mail are queued — but the access tokens live
    // out their residual window (they are not denylisted
    // by a family revocation).
    let res = app.request(post_refresh(&refresh)).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());
    assert_eq!(problem_code(&res), "PF-1007", "{}", res.text());
    let res = app.request(post_refresh(&refresh2)).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());
    assert_eq!(problem_code(&res), "PF-1001", "{}", res.text());
    let res = app.request(get_me(&access2)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let reuse_audits = audit_logs::list_for_action(&app.state.pool, "session.refresh_reused")
        .await
        .unwrap();
    assert!(
        reuse_audits
            .iter()
            .any(|row| row.target_id.as_deref() == Some(&user_id.to_string())),
        "reuse must be audited for this user"
    );
    let row = sessions::by_refresh_hash(&app.state.pool, &sha256_hex(refresh2.as_bytes()))
        .await
        .unwrap()
        .expect("rotated row still exists");
    assert!(row.revoked_at.is_some(), "family revoked");

    // 7. A fresh login starts a new family; logout revokes
    // it (204, cookie cleared) and the refresh token is
    // dead afterwards — while the access token again lives
    // out its residual window.
    let (access3, refresh3) = login_tokens(&app, &email).await;
    assert_eq!(live_sessions(&app, &access3).await.len(), 1);
    let res = app.request(post_logout(&refresh3)).await;
    assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.text());
    assert_eq!(live_sessions(&app, &access3).await.len(), 0);
    let res = app.request(post_refresh(&refresh3)).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());
    assert_eq!(problem_code(&res), "PF-1001", "{}", res.text());
    let res = app.request(get_me(&access3)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
}

#[tokio::test]
async fn clock_injected_expiry_kills_tokens_and_sessions() {
    let (app, clock) = lifecycle_app().await;
    let email = unique_email("expiry");
    let user_id = register_user(&app, &email).await;
    let token = token_for_user(&app, user_id).await;
    let res = app.request(post_verify(&token)).await;
    assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.text());
    let (access, refresh) = login_tokens(&app, &email).await;

    // Access token: 15-minute TTL. 16 minutes later the
    // extractor answers 401 PF-1011 — proven by moving the
    // clock, not by waiting.
    clock.advance(Duration::minutes(16));
    let res = app.request(get_me(&access)).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());
    assert_eq!(problem_code(&res), "PF-1011", "{}", res.text());

    // The session is still alive (30 days): refresh mints
    // a fresh access token that works.
    let (access2, refresh2) = refresh_tokens(&app, &refresh).await;
    let res = app.request(get_me(&access2)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());

    // Session: 30-day TTL. 31 days after the refresh the
    // session is expired — refresh is the generic 401
    // PF-1001 (an expired session is indistinguishable from
    // an unknown one) and the access token is PF-1011.
    clock.advance(Duration::days(31));
    let res = app.request(post_refresh(&refresh2)).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());
    assert_eq!(problem_code(&res), "PF-1001", "{}", res.text());
    let res = app.request(get_me(&access2)).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());
    assert_eq!(problem_code(&res), "PF-1011", "{}", res.text());

    // Verification token: 24-hour TTL. A token minted now
    // is expired 25 hours later — proven the same way.
    let email2 = unique_email("expiry-verify");
    let user2 = register_user(&app, &email2).await;
    let token2 = token_for_user(&app, user2).await;
    clock.advance(Duration::hours(25));
    let res = app.request(post_verify(&token2)).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());
    assert_eq!(problem_code(&res), "PF-1011", "{}", res.text());
}

#[tokio::test]
async fn token_tampering_is_rejected() {
    let (app, clock) = lifecycle_app().await;
    let email = unique_email("tamper");
    let user_id = register_user(&app, &email).await;
    let token = token_for_user(&app, user_id).await;
    let res = app.request(post_verify(&token)).await;
    assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.text());
    let (access, _refresh) = login_tokens(&app, &email).await;
    let secret = app.settings.jwt_secret.clone();

    // A flipped payload byte breaks the signature.
    let parts: Vec<&str> = access.split('.').collect();
    assert_eq!(parts.len(), 3);
    let mut payload: Vec<char> = parts[1].chars().collect();
    payload[0] = if payload[0] == 'A' { 'B' } else { 'A' };
    let tampered = format!(
        "{}.{}.{}",
        parts[0],
        payload.into_iter().collect::<String>(),
        parts[2]
    );
    let res = app.request(get_me(&tampered)).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());
    assert_eq!(problem_code(&res), "PF-1010", "{}", res.text());

    // A well-formed token from another secret is not ours.
    let foreign = issue_access_token(OTHER_SECRET, user_id, "user", ACCESS_TOKEN_TTL_SECS)
        .expect("foreign token issues");
    let res = app.request(get_me(&foreign)).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());
    assert_eq!(problem_code(&res), "PF-1010", "{}", res.text());

    // Garbage is not a token.
    let res = app.request(get_me("not.a.jwt")).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());
    assert_eq!(problem_code(&res), "PF-1010", "{}", res.text());

    // A token whose `nbf` is an hour in the future is
    // immature (PF-1010, never PF-1011) — and it verifies
    // once the clock reaches its `nbf`.
    let future = issue_access_token_at(
        &secret,
        user_id,
        "user",
        ACCESS_TOKEN_TTL_SECS,
        clock.now() + Duration::hours(1),
    )
    .expect("future token issues");
    let res = app.request(get_me(&future)).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());
    assert_eq!(problem_code(&res), "PF-1010", "{}", res.text());
    clock.advance(Duration::hours(1));
    let res = app.request(get_me(&future)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
}
