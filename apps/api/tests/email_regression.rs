//! P1-104: the email regression suite — one file proving the
//! email link tokens are single-use, for all three token kinds.
//!
//! Through the real router (`TestApp` oneshot) against the
//! migrated scratch database. The three token kinds are the
//! `verify-email` token (register), the `password-reset` token
//! (forgot), and the `email-change-verify` token (PATCH /v1/me)
//! — all three live in the one `email_verification_tokens`
//! table and all three consume through the same atomic
//! `UPDATE ... WHERE used_at IS NULL ... RETURNING` (the
//! repo fn `email_verification_tokens::consume`).
//!
//! The sequential proofs are deliberate re-proofs (the P1-099
//! / ADR-0062 precedent): each kind already proves single-use
//! in its own file (`verify_token_is_single_use`,
//! `reset_token_is_single_use`, `confirm_used_token_is_401`),
//! and this suite restates them as one contract so a future
//! change must keep the whole property green in one place.
//! The concurrency proofs are NEW: the sequential tests
//! cannot prove the consume is atomic, so each kind also gets
//! a double-consume race — two requests in flight on the same
//! token, exactly one 204 and one 401 PF-1010, and the
//! winner's effect landed exactly once. The verify race is
//! NOT duplicated here: it already lives in verification.rs
//! (`verify_concurrent_double_verify_only_one_succeeds`).
//! Tokens are read back from the outbox payload — the only
//! place the raw token exists outside the email (the DB holds
//! its hash), exactly as the mail drain reads it.

mod common;

use axum::http::{Method, Request, StatusCode};
use common::{TestApp, TestFixtures};
use detag_api::services::password_reset::PASSWORD_RESET_TEMPLATE;
use detag_api::services::verification::{EMAIL_CHANGE_VERIFY_TEMPLATE, VERIFY_EMAIL_TEMPLATE};
use detag_core::repos::email_outbox;
use serde_json::{json, Value};
use uuid::Uuid;

/// The request id: a valid UUID, echoed by the middleware.
const REQUEST_ID: &str = "123e4567-e89b-42d3-a456-426614174104";

/// The password every test user registers with.
const PASSWORD: &str = "correct horse battery staple";
const NEW_PASSWORD: &str = "a completely different horse";

async fn regression_app() -> TestApp {
    let fixtures = TestFixtures::from_env().await;
    let mut settings = fixtures.settings();
    settings.disposable_email_domains = String::new();
    settings.tos_current_version = "v1".to_string();
    TestApp::spawn_with_fixtures_and_settings(fixtures, settings).await
}

fn unique_email(prefix: &str) -> String {
    format!("{prefix}-{}@example.com", Uuid::now_v7())
}

/// A per-request XFF address: each request gets its own IP
/// bucket, so the tests never trip each other's rate limits
/// (the P1-003 lesson) — and the two legs of a race never
/// share a bucket either.
fn unique_xff() -> String {
    format!("203.0.113.{}", Uuid::now_v7())
}

fn json_request(method: Method, uri: &str, body: Value) -> Request<axum::body::Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("x-request-id", REQUEST_ID)
        .header("x-forwarded-for", unique_xff())
        .header("content-type", "application/json")
        .body(axum::body::Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap()
}

fn post_register(email: &str) -> Request<axum::body::Body> {
    json_request(
        Method::POST,
        "/v1/auth/register",
        json!({"email": email, "password": PASSWORD, "accept_tos_version": "v1", "accept_aup_version": "v1"}),
    )
}

fn post_verify(token: &str) -> Request<axum::body::Body> {
    json_request(
        Method::POST,
        "/v1/auth/verify-email",
        json!({ "token": token }),
    )
}

fn post_forgot(email: &str) -> Request<axum::body::Body> {
    json_request(
        Method::POST,
        "/v1/auth/forgot-password",
        json!({ "email": email }),
    )
}

fn post_reset(token: &str, new_password: &str) -> Request<axum::body::Body> {
    json_request(
        Method::POST,
        "/v1/auth/reset-password",
        json!({ "token": token, "new_password": new_password }),
    )
}

fn post_login(email: &str, password: &str) -> Request<axum::body::Body> {
    json_request(
        Method::POST,
        "/v1/auth/login",
        json!({ "email": email, "password": password }),
    )
}

fn post_confirm(raw_token: &str) -> Request<axum::body::Body> {
    json_request(
        Method::POST,
        "/v1/auth/confirm-email-change",
        json!({ "token": raw_token }),
    )
}

fn patch_me(token: &str, body: Value) -> Request<axum::body::Body> {
    Request::builder()
        .method(Method::PATCH)
        .uri("/v1/me")
        .header("x-request-id", REQUEST_ID)
        .header("x-forwarded-for", unique_xff())
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {token}"))
        .body(axum::body::Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap()
}

fn get_me(token: &str) -> Request<axum::body::Body> {
    Request::builder()
        .method(Method::GET)
        .uri("/v1/me")
        .header("x-request-id", REQUEST_ID)
        .header("x-forwarded-for", unique_xff())
        .header("authorization", format!("Bearer {token}"))
        .body(axum::body::Body::empty())
        .unwrap()
}

/// Register a user, return its id.
async fn register_user(app: &TestApp, email: &str) -> Uuid {
    let res = app.request(post_register(email)).await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.text());
    res.json()["id"].as_str().unwrap().parse().unwrap()
}

/// Log in, return the access token.
async fn login_token(app: &TestApp, email: &str, password: &str) -> String {
    let res = app.request(post_login(email, password)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    res.json()["access_token"].as_str().unwrap().to_string()
}

/// The unsent verification-mail token for a user.
async fn verify_token_for(app: &TestApp, user_id: Uuid) -> String {
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

/// The LATEST unsent reset-mail token for a user. Latest,
/// not exactly-one: a second forgot queues a second mail
/// while killing the first token.
async fn reset_token_for(app: &TestApp, user_id: Uuid) -> String {
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

/// The change token for `user_id`, read from the LATEST
/// `email-change-verify` outbox mail. The outbox accumulates
/// one mail per mint, so the latest mail is the live one.
async fn change_token_for(app: &TestApp, user_id: Uuid) -> String {
    let mail = email_outbox::unsent_for_template(&app.state.pool, EMAIL_CHANGE_VERIFY_TEMPLATE)
        .await
        .unwrap();
    let ours: Vec<_> = mail
        .iter()
        .filter(|row| row.payload["user_id"] == json!(user_id))
        .collect();
    assert!(!ours.is_empty(), "no change mail for user {user_id}");
    ours.last().unwrap().payload["token"]
        .as_str()
        .unwrap()
        .to_string()
}

/// Exactly one of the two responses is the winner's status
/// and the other is the loser's — the shape every
/// double-consume race must have.
fn assert_one_winner_one_loser(
    statuses: [StatusCode; 2],
    winner: StatusCode,
    loser: StatusCode,
    what: &str,
) {
    assert_eq!(
        statuses.iter().filter(|s| **s == winner).count(),
        1,
        "exactly one {winner} in the {what} race, got {statuses:?}"
    );
    assert_eq!(
        statuses.iter().filter(|s| **s == loser).count(),
        1,
        "exactly one {loser} in the {what} race, got {statuses:?}"
    );
}

#[tokio::test]
async fn verify_token_is_single_use() {
    let app = regression_app().await;
    let email = unique_email("regression-verify");
    let user_id = register_user(&app, &email).await;
    let token = verify_token_for(&app, user_id).await;

    let first = app.request(post_verify(&token)).await;
    assert_eq!(first.status, StatusCode::NO_CONTENT, "{}", first.text());
    let second = app.request(post_verify(&token)).await;
    assert_eq!(second.status, StatusCode::UNAUTHORIZED, "{}", second.text());
    assert_eq!(second.json()["code"], "PF-1010", "{}", second.text());
}

#[tokio::test]
async fn reset_token_is_single_use() {
    let app = regression_app().await;
    let email = unique_email("regression-reset");
    let user_id = register_user(&app, &email).await;

    let res = app.request(post_forgot(&email)).await;
    assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.text());
    let token = reset_token_for(&app, user_id).await;

    let first = app.request(post_reset(&token, NEW_PASSWORD)).await;
    assert_eq!(first.status, StatusCode::NO_CONTENT, "{}", first.text());

    // The same token again: consumed, so invalid.
    let second = app
        .request(post_reset(&token, "yet another password"))
        .await;
    assert_eq!(second.status, StatusCode::UNAUTHORIZED, "{}", second.text());
    assert_eq!(second.json()["code"], "PF-1010", "{}", second.text());

    // The winner's write is the one that landed.
    let res = app.request(post_login(&email, NEW_PASSWORD)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
}

#[tokio::test]
async fn email_change_token_is_single_use() {
    let app = regression_app().await;
    let email = unique_email("regression-change");
    let user_id = register_user(&app, &email).await;
    let access = login_token(&app, &email, PASSWORD).await;
    let pending = unique_email("regression-change-pending");

    let res = app
        .request(patch_me(&access, json!({ "email": pending })))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let token = change_token_for(&app, user_id).await;

    let first = app.request(post_confirm(&token)).await;
    assert_eq!(first.status, StatusCode::NO_CONTENT, "{}", first.text());
    let second = app.request(post_confirm(&token)).await;
    assert_eq!(second.status, StatusCode::UNAUTHORIZED, "{}", second.text());
    assert_eq!(second.json()["code"], "PF-1010", "{}", second.text());

    // The winner's swap is the one that landed.
    let res = app.request(get_me(&access)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert_eq!(res.json()["email"], pending, "{}", res.text());
}

#[tokio::test]
async fn reset_token_concurrent_double_consume_succeeds_once() {
    let app = regression_app().await;
    let email = unique_email("regression-reset-race");
    let user_id = register_user(&app, &email).await;

    let res = app.request(post_forgot(&email)).await;
    assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.text());
    let token = reset_token_for(&app, user_id).await;

    // Two requests in flight on the same token: the atomic
    // consume UPDATE admits exactly one.
    let (first, second) = tokio::join!(
        app.request(post_reset(&token, NEW_PASSWORD)),
        app.request(post_reset(&token, NEW_PASSWORD)),
    );
    assert_one_winner_one_loser(
        [first.status, second.status],
        StatusCode::NO_CONTENT,
        StatusCode::UNAUTHORIZED,
        "reset-token",
    );
    for res in [&first, &second] {
        if res.status == StatusCode::UNAUTHORIZED {
            assert_eq!(res.json()["code"], "PF-1010", "{}", res.text());
        }
    }

    // The winner's write landed exactly once: the new
    // password logs in, the old one does not.
    let res = app.request(post_login(&email, NEW_PASSWORD)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let res = app.request(post_login(&email, PASSWORD)).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());
}

#[tokio::test]
async fn email_change_token_concurrent_double_consume_succeeds_once() {
    let app = regression_app().await;
    let email = unique_email("regression-chg-race");
    let user_id = register_user(&app, &email).await;
    let access = login_token(&app, &email, PASSWORD).await;
    let pending = unique_email("regression-chg-pend");

    let res = app
        .request(patch_me(&access, json!({ "email": pending })))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let token = change_token_for(&app, user_id).await;

    // Two requests in flight on the same token: the atomic
    // consume UPDATE admits exactly one.
    let (first, second) = tokio::join!(
        app.request(post_confirm(&token)),
        app.request(post_confirm(&token)),
    );
    assert_one_winner_one_loser(
        [first.status, second.status],
        StatusCode::NO_CONTENT,
        StatusCode::UNAUTHORIZED,
        "change-token",
    );
    for res in [&first, &second] {
        if res.status == StatusCode::UNAUTHORIZED {
            assert_eq!(res.json()["code"], "PF-1010", "{}", res.text());
        }
    }

    // The winner's swap landed exactly once: the address on
    // record is the pending one (the same access token still
    // works — the account is the same, only its email moved).
    let res = app.request(get_me(&access)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert_eq!(res.json()["email"], pending, "{}", res.text());
}
