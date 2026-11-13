//! P1-025: forgot / reset password flow, end to end.
//!
//! Through the real router (`TestApp` oneshot) against the
//! migrated scratch database: the forgot endpoint (204-always,
//! no enumeration — an unknown email answers exactly like a
//! known one), the reset endpoint (single-use 30-minute hashed
//! token, purpose `password_reset`), the reset's side effects
//! (password changed, EVERY session revoked + epoch bumped,
//! lockout counter cleared, digest mail queued), and the
//! reset-account rate limit (10 burst / 1 per min, P1-020).
//! The raw token is read back from the outbox payload — the
//! only place it exists outside the email (the DB holds its
//! hash), exactly as the mail drain will read it.

mod common;

use std::sync::Arc;

use axum::http::{Method, Request, StatusCode};
use chrono::{Duration, Utc};
use common::{TestApp, TestFixtures};
use detag_api::services::password::NEW_LOGIN_DIGEST_TEMPLATE;
use detag_api::services::password_reset::{PASSWORD_RESET_TEMPLATE, RESET_TOKEN_TTL_SECS};
use detag_api::services::verification::VERIFY_EMAIL_TEMPLATE;
use detag_core::clock::{Clock, ManualClock};
use detag_core::{
    checksum::sha256_hex,
    repos::{audit_logs, email_outbox, email_verification_tokens, users},
    tokens::{generate_token, hash_token},
};
use serde_json::{json, Value};
use uuid::Uuid;

/// The request id: a valid UUID, echoed by the middleware.
const REQUEST_ID: &str = "123e4567-e89b-42d3-a456-426614174102";
const PASSWORD: &str = "correct horse battery staple";
const NEW_PASSWORD: &str = "another correct horse battery";

async fn reset_app() -> TestApp {
    let fixtures = TestFixtures::from_env().await;
    let mut settings = fixtures.settings();
    settings.disposable_email_domains = String::new();
    settings.tos_current_version = "v1".to_string();
    TestApp::spawn_with_fixtures_and_settings(fixtures, settings).await
}

/// App on a frozen clock (P1-023's pattern): the reset token's
/// 30-minute TTL is testable without waiting it out.
async fn reset_app_with_clock() -> (TestApp, ManualClock) {
    let fixtures = TestFixtures::from_env().await;
    let mut settings = fixtures.settings();
    settings.disposable_email_domains = String::new();
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

fn post_forgot(email: &str) -> Request<axum::body::Body> {
    let body = json!({ "email": email });
    Request::builder()
        .method(Method::POST)
        .uri("/v1/auth/forgot-password")
        .header("x-request-id", REQUEST_ID)
        .header("x-forwarded-for", unique_xff())
        .header("content-type", "application/json")
        .body(axum::body::Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap()
}

fn post_reset(token: &str, new_password: &str) -> Request<axum::body::Body> {
    let body = json!({ "token": token, "new_password": new_password });
    Request::builder()
        .method(Method::POST)
        .uri("/v1/auth/reset-password")
        .header("x-request-id", REQUEST_ID)
        .header("x-forwarded-for", unique_xff())
        .header("content-type", "application/json")
        .body(axum::body::Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap()
}

fn post_login(email: &str, password: &str) -> Request<axum::body::Body> {
    let body = json!({ "email": email, "password": password });
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

/// The LATEST unsent reset-mail token for a user — read from
/// the outbox payload, the only place the raw token exists
/// outside the email (the DB holds its hash). Latest, not
/// exactly-one: a second forgot queues a second mail (the
/// first mail row stays unsent) while killing the first
/// token — the caller that wants the count uses
/// [`reset_mail_count_for_user`].
async fn reset_token_for_user(app: &TestApp, user_id: Uuid) -> String {
    let mail = email_outbox::unsent_for_template(&app.state.pool, PASSWORD_RESET_TEMPLATE)
        .await
        .unwrap();
    let ours: Vec<_> = mail
        .iter()
        .filter(|row| row.payload["user_id"] == json!(user_id))
        .collect();
    assert!(
        !ours.is_empty(),
        "no reset mail for user {user_id}: {mail:?}"
    );
    let latest = ours
        .iter()
        .max_by_key(|row| row.created_at)
        .expect("non-empty");
    latest.payload["token"].as_str().unwrap().to_string()
}

/// How many unsent reset mails a user has queued.
async fn reset_mail_count_for_user(app: &TestApp, user_id: Uuid) -> usize {
    let mail = email_outbox::unsent_for_template(&app.state.pool, PASSWORD_RESET_TEMPLATE)
        .await
        .unwrap();
    mail.iter()
        .filter(|row| row.payload["user_id"] == json!(user_id))
        .count()
}

/// The latest unsent verification-mail token for a user.
async fn verify_token_for_user(app: &TestApp, user_id: Uuid) -> String {
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
async fn login_tokens(app: &TestApp, email: &str, password: &str) -> (String, String) {
    let res = app.request(post_login(email, password)).await;
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

/// The problem code of an error response (`PF-xxxx`).
fn problem_code(res: &common::TestResponse) -> String {
    res.json()["code"].as_str().unwrap().to_string()
}

/// The live sessions for the bearer, as a JSON array.
async fn live_sessions(app: &TestApp, access: &str) -> Vec<Value> {
    let res = app.request(get_sessions(access)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    res.json().as_array().expect("list is an array").clone()
}

#[tokio::test]
async fn forgot_unknown_email_is_204_with_no_mail() {
    let app = reset_app().await;
    let email = unique_email("ghost");

    let res = app.request(post_forgot(&email)).await;
    assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.text());
    assert!(res.body.is_empty(), "204 carries no body");

    // No reset mail for that address (the outbox row would
    // carry its sha256 as to_hash — no user row, no payload
    // user_id, so the hash is the only handle).
    let mail = email_outbox::unsent_for_template(&app.state.pool, PASSWORD_RESET_TEMPLATE)
        .await
        .unwrap();
    assert!(
        mail.iter()
            .all(|row| row.to_hash.as_deref() != Some(sha256_hex(email.as_bytes()).as_str())),
        "no reset mail for an unknown email: {mail:?}"
    );
    // And no token row was minted for it either (no user to
    // mint for — the audit below is the only trace).
    let logs = audit_logs::list_for_action(&app.state.pool, "password.reset_requested")
        .await
        .unwrap();
    assert!(
        logs.iter().any(|log| {
            log.after
                .as_ref()
                .is_some_and(|after| after["email_hash"] == json!(sha256_hex(email.as_bytes())))
        }),
        "the unknown-email request is audited with the hash only: {logs:?}"
    );
}

#[tokio::test]
async fn forgot_known_email_queues_mail_and_second_forgot_kills_first_token() {
    let app = reset_app().await;
    let email = unique_email("reset");
    let user_id = register_user(&app, &email).await;

    let res = app.request(post_forgot(&email)).await;
    assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.text());
    let first = reset_token_for_user(&app, user_id).await;

    // A second forgot mints a fresh token (the outbox row for
    // the first mail stays — it is unsent mail, not a token —
    // but the first TOKEN row is deleted: one live token per
    // user, the P1-006 rule).
    let res = app.request(post_forgot(&email)).await;
    assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.text());
    assert_eq!(reset_mail_count_for_user(&app, user_id).await, 2);
    let second = reset_token_for_user(&app, user_id).await;
    assert_ne!(first, second, "the second forgot minted a fresh token");

    // The first token is dead: the second mint deleted it.
    let res = app.request(post_reset(&first, NEW_PASSWORD)).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());
    assert_eq!(problem_code(&res), "PF-1010", "{}", res.text());

    // The second token still works.
    let res = app.request(post_reset(&second, NEW_PASSWORD)).await;
    assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.text());
}

#[tokio::test]
async fn reset_happy_path_changes_password_revokes_sessions_and_audits() {
    let app = reset_app().await;
    let email = unique_email("happy");
    let user_id = register_user(&app, &email).await;
    let (access, refresh) = login_tokens(&app, &email, PASSWORD).await;
    assert_eq!(live_sessions(&app, &access).await.len(), 1);

    let res = app.request(post_forgot(&email)).await;
    assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.text());
    let token = reset_token_for_user(&app, user_id).await;

    let res = app.request(post_reset(&token, NEW_PASSWORD)).await;
    assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.text());

    // The old password is dead, the new one works.
    let res = app.request(post_login(&email, PASSWORD)).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());
    let (new_access, _new_refresh) = login_tokens(&app, &email, NEW_PASSWORD).await;

    // EVERY session is revoked: the old refresh token dies…
    let res = app.request(post_refresh(&refresh)).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());
    // …and the old access token dies at the extractor (the
    // epoch bump, P1-021).
    let res = app.request(get_me(&access)).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());
    assert_eq!(problem_code(&res), "PF-1010", "{}", res.text());
    // The fresh login's own session is the only live one.
    assert_eq!(live_sessions(&app, &new_access).await.len(), 1);

    // The audit trail: request + reset, on the user.
    let logs = audit_logs::list_for_target(&app.state.pool, "user", &user_id.to_string())
        .await
        .unwrap();
    assert!(
        logs.iter()
            .any(|log| log.action == "password.reset_requested"),
        "{logs:?}"
    );
    assert!(
        logs.iter().any(|log| log.action == "password.reset"),
        "{logs:?}"
    );
    assert!(
        logs.iter()
            .any(|log| log.action == "session.revoked_on_password_change"),
        "{logs:?}"
    );
    // And the new-login digest mail was queued (P1-018).
    let mail = email_outbox::unsent_for_template(&app.state.pool, NEW_LOGIN_DIGEST_TEMPLATE)
        .await
        .unwrap();
    assert!(
        mail.iter()
            .any(|row| row.payload["user_id"] == json!(user_id)),
        "digest mail queued: {mail:?}"
    );
}

#[tokio::test]
async fn reset_token_is_single_use() {
    let app = reset_app().await;
    let email = unique_email("single");
    let user_id = register_user(&app, &email).await;

    let res = app.request(post_forgot(&email)).await;
    assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.text());
    let token = reset_token_for_user(&app, user_id).await;

    let res = app.request(post_reset(&token, NEW_PASSWORD)).await;
    assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.text());

    // The same token again: consumed, so invalid.
    let res = app
        .request(post_reset(&token, "yet another password"))
        .await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());
    assert_eq!(problem_code(&res), "PF-1010", "{}", res.text());
}

#[tokio::test]
async fn reset_expired_token_is_401_pf1011() {
    let app = reset_app().await;
    let email = unique_email("expired");
    let user_id = register_user(&app, &email).await;

    // Seed an expired reset token directly (the repo mint
    // takes any expiry — the service's 30-minute TTL is the
    // production path; this is the expired-row fixture).
    let raw = generate_token();
    email_verification_tokens::mint(
        &app.pool,
        user_id,
        &hash_token(&raw),
        "password_reset",
        Utc::now() - Duration::hours(1),
    )
    .await
    .unwrap();

    let res = app.request(post_reset(&raw, NEW_PASSWORD)).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());
    assert_eq!(problem_code(&res), "PF-1011", "{}", res.text());
}

#[tokio::test]
async fn reset_with_verification_token_is_401_pf1010_and_token_survives() {
    let app = reset_app().await;
    let email = unique_email("purpose");
    let user_id = register_user(&app, &email).await;
    let verify_token = verify_token_for_user(&app, user_id).await;

    // A registration token is not a reset token (purpose
    // separation, the P1-006 lesson).
    let res = app.request(post_reset(&verify_token, NEW_PASSWORD)).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());
    assert_eq!(problem_code(&res), "PF-1010", "{}", res.text());

    // And the rejected attempt did NOT consume it: it still
    // verifies the email afterwards.
    let body = json!({ "token": verify_token });
    let req = Request::builder()
        .method(Method::POST)
        .uri("/v1/auth/verify-email")
        .header("x-request-id", REQUEST_ID)
        .header("x-forwarded-for", unique_xff())
        .header("content-type", "application/json")
        .body(axum::body::Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();
    let res = app.request(req).await;
    assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.text());
}

#[tokio::test]
async fn reset_unknown_token_is_401_pf1010() {
    let app = reset_app().await;
    let res = app
        .request(post_reset(&generate_token(), NEW_PASSWORD))
        .await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());
    assert_eq!(problem_code(&res), "PF-1010", "{}", res.text());
}

#[tokio::test]
async fn reset_short_password_is_422() {
    let app = reset_app().await;
    let email = unique_email("short");
    let user_id = register_user(&app, &email).await;

    let res = app.request(post_forgot(&email)).await;
    assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.text());
    let token = reset_token_for_user(&app, user_id).await;

    let res = app.request(post_reset(&token, "short")).await;
    assert_eq!(
        res.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        res.text()
    );
    assert_eq!(problem_code(&res), "PF-2002", "{}", res.text());
}

#[tokio::test]
async fn forgot_rate_limit_trips() {
    let app = reset_app().await;
    // An unknown email: the bucket trips identically for
    // known and unknown addresses, and nothing is minted.
    let email = unique_email("limit");

    // The reset-account bucket: 10 burst, 1 per minute.
    for _ in 0..10 {
        let res = app.request(post_forgot(&email)).await;
        assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.text());
    }
    let res = app.request(post_forgot(&email)).await;
    assert_eq!(res.status, StatusCode::TOO_MANY_REQUESTS, "{}", res.text());
    assert_eq!(problem_code(&res), "PF-3007", "{}", res.text());
    assert!(
        res.headers.get("retry-after").is_some(),
        "429 carries Retry-After"
    );
    let limit = res
        .headers
        .get("x-ratelimit-limit")
        .and_then(|v| v.to_str().ok());
    assert_eq!(
        limit,
        Some("10"),
        "the Limit names the reset-account bucket (burst 10)"
    );
}

#[tokio::test]
async fn reset_clears_the_lockout_counter() {
    let app = reset_app().await;
    let email = unique_email("lockout");
    let user_id = register_user(&app, &email).await;

    // Five failed logins lock the account (P1-004).
    for _ in 0..5 {
        let res = app.request(post_login(&email, "wrong password here")).await;
        assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());
    }
    // The correct password is now refused: 423 PF-1008.
    let res = app.request(post_login(&email, PASSWORD)).await;
    assert_eq!(res.status, StatusCode::LOCKED, "{}", res.text());
    assert_eq!(problem_code(&res), "PF-1008", "{}", res.text());

    // A password reset un-locks the account (P1-004's hook).
    let res = app.request(post_forgot(&email)).await;
    assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.text());
    let token = reset_token_for_user(&app, user_id).await;
    let res = app.request(post_reset(&token, NEW_PASSWORD)).await;
    assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.text());

    let (_access, _refresh) = login_tokens(&app, &email, NEW_PASSWORD).await;
}

#[tokio::test]
async fn reset_token_expires_after_30_minutes() {
    let (app, clock) = reset_app_with_clock().await;
    let email = unique_email("ttl");
    let user_id = register_user(&app, &email).await;

    // At T0 + 29 minutes the token is still alive.
    let res = app.request(post_forgot(&email)).await;
    assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.text());
    let first = reset_token_for_user(&app, user_id).await;
    clock.advance(Duration::minutes(29));
    let res = app.request(post_reset(&first, NEW_PASSWORD)).await;
    assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.text());

    // A fresh token, then 31 minutes past its mint: dead.
    let res = app.request(post_forgot(&email)).await;
    assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.text());
    let second = reset_token_for_user(&app, user_id).await;
    clock.advance(Duration::minutes(31));
    let res = app
        .request(post_reset(&second, "a third password here"))
        .await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());
    assert_eq!(problem_code(&res), "PF-1011", "{}", res.text());

    // The TTL constant is the 30 minutes the spec promises.
    assert_eq!(RESET_TOKEN_TTL_SECS, 30 * 60);
}

/// The reset flow needs no verified email: an unverified
/// user can reset (the login gate is not the verification
/// gate — the P1-002 test proves login works unverified).
#[tokio::test]
async fn reset_works_for_unverified_user() {
    let app = reset_app().await;
    let email = unique_email("unverified");
    let user_id = register_user(&app, &email).await;
    let user = users::by_id(&app.state.pool, user_id)
        .await
        .unwrap()
        .unwrap();
    assert!(user.email_verified_at.is_none(), "fresh user is unverified");

    let res = app.request(post_forgot(&email)).await;
    assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.text());
    let token = reset_token_for_user(&app, user_id).await;
    let res = app.request(post_reset(&token, NEW_PASSWORD)).await;
    assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.text());
    let (_access, _refresh) = login_tokens(&app, &email, NEW_PASSWORD).await;
}
