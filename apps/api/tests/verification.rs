//! P1-002: email verification, end to end.
//!
//! Through the real router (`TestApp` oneshot) against the
//! migrated scratch database: the single-use token (minted at
//! register, stored as sha256), the verify endpoint (204 on
//! success, PF-1010/PF-1011 on bad/expired tokens), the resend
//! endpoint (204-always, 3/hour), and the `require_verified`
//! gate (PF-1009) the Phase 2 job-create handler will call.
//! The raw token is read back from the outbox payload — the
//! only place it exists outside the email (the DB holds its
//! hash), exactly as the mail drain will read it.

mod common;

use axum::http::{Method, Request, StatusCode};
use common::{TestApp, TestFixtures};
use detag_api::services::verification::{require_verified, VERIFY_EMAIL_TEMPLATE};
use detag_core::error::Profile;
use detag_core::{
    checksum::sha256_hex,
    repos::{audit_logs, email_outbox, email_verification_tokens, users},
    tokens::{generate_token, hash_token},
};
use serde_json::json;
use uuid::Uuid;

/// The request id: a valid UUID, echoed by the middleware.
const REQUEST_ID: &str = "123e4567-e89b-42d3-a456-426614174102";

async fn verify_app() -> TestApp {
    let fixtures = TestFixtures::from_env().await;
    let mut settings = fixtures.settings();
    settings.disposable_email_domains = String::new();
    settings.tos_current_version = "v1".to_string();
    TestApp::spawn_with_fixtures_and_settings(fixtures, settings).await
}

fn post_register(email: &str) -> Request<axum::body::Body> {
    let body = json!({
        "email": email,
        "password": "correct horse battery staple",
        "accept_tos_version": "v1",
    });
    Request::builder()
        .method(Method::POST)
        .uri("/v1/auth/register")
        .header("x-request-id", REQUEST_ID)
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
        .header("content-type", "application/json")
        .body(axum::body::Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap()
}

fn post_resend(email: &str) -> Request<axum::body::Body> {
    let body = json!({ "email": email });
    Request::builder()
        .method(Method::POST)
        .uri("/v1/auth/resend-verification")
        .header("x-request-id", REQUEST_ID)
        .header("content-type", "application/json")
        .body(axum::body::Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap()
}

fn unique_email() -> String {
    format!("verify-{}@example.com", Uuid::now_v7())
}

/// Register a user, return (user id, the raw token from the
/// queued verification mail). The token is read from the
/// outbox payload — the same place the mail drain reads it.
async fn register_and_token(app: &TestApp) -> (Uuid, String) {
    let email = unique_email();
    let res = app.request(post_register(&email)).await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.text());
    let user_id: Uuid = res.json()["id"].as_str().unwrap().parse().unwrap();
    let token = token_for_user(app, user_id).await;
    (user_id, token)
}

/// The latest unsent verification-mail token for a user.
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

/// How many unsent verification mails a user has queued.
async fn mail_count_for_user(app: &TestApp, user_id: Uuid) -> usize {
    let mail = email_outbox::unsent_for_template(&app.state.pool, VERIFY_EMAIL_TEMPLATE)
        .await
        .unwrap();
    mail.iter()
        .filter(|row| row.payload["user_id"] == json!(user_id))
        .count()
}

#[tokio::test]
async fn verify_happy_path_sets_verified_at_and_audits() {
    let app = verify_app().await;
    let (user_id, token) = register_and_token(&app).await;

    let res = app.request(post_verify(&token)).await;
    assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.text());
    assert!(res.text().is_empty(), "204 has no body");

    let user = users::by_id(&app.state.pool, user_id)
        .await
        .unwrap()
        .expect("user row exists");
    assert!(user.email_verified_at.is_some(), "verified_at is set");

    // The token row is consumed (used_at set), not deleted.
    let row = email_verification_tokens::by_hash(&app.state.pool, &hash_token(&token))
        .await
        .unwrap()
        .expect("token row still exists as audit trail");
    assert!(row.used_at.is_some());

    // And the success is audited.
    let logs = audit_logs::list_for_target(&app.state.pool, "user", &user_id.to_string())
        .await
        .unwrap();
    assert!(
        logs.iter().any(|log| log.action == "verify-email.success"),
        "{logs:?}"
    );
}

#[tokio::test]
async fn verify_token_is_single_use() {
    let app = verify_app().await;
    let (_user_id, token) = register_and_token(&app).await;

    let first = app.request(post_verify(&token)).await;
    assert_eq!(first.status, StatusCode::NO_CONTENT);
    let second = app.request(post_verify(&token)).await;
    assert_eq!(second.status, StatusCode::UNAUTHORIZED, "{}", second.text());
    let json = second.json();
    assert_eq!(json["code"], "PF-1010", "{json}");
    assert_eq!(json["request_id"], REQUEST_ID, "{json}");
}

#[tokio::test]
async fn verify_unknown_token_is_401_pf1010() {
    let app = verify_app().await;
    let res = app.request(post_verify(&generate_token())).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-1010");
}

#[tokio::test]
async fn verify_expired_token_is_401_pf1011_and_not_consumed() {
    let app = verify_app().await;
    let (user_id, _token) = register_and_token(&app).await;
    // Mint a second token that is already expired (the repo
    // takes expires_at as a parameter — no clock to freeze).
    let raw = generate_token();
    let past = chrono::Utc::now() - chrono::Duration::hours(1);
    email_verification_tokens::mint(
        &app.state.pool,
        user_id,
        &hash_token(&raw),
        email_verification_tokens::PURPOSE_VERIFICATION,
        past,
    )
    .await
    .unwrap();

    let res = app.request(post_verify(&raw)).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-1011", "{}", res.text());

    // Expired is not consumed: the row stays unused (a later
    // resend mints fresh; nothing silently marks it used).
    let row = email_verification_tokens::by_hash(&app.state.pool, &hash_token(&raw))
        .await
        .unwrap()
        .expect("expired row still exists");
    assert!(row.used_at.is_none());
    // And the user is still unverified.
    let user = users::by_id(&app.state.pool, user_id)
        .await
        .unwrap()
        .unwrap();
    assert!(user.email_verified_at.is_none());
}

#[tokio::test]
async fn verify_concurrent_double_verify_only_one_succeeds() {
    let app = verify_app().await;
    let (_user_id, token) = register_and_token(&app).await;

    // Two requests in flight on the same token: the atomic
    // consume UPDATE admits exactly one.
    let (first, second) = tokio::join!(
        app.request(post_verify(&token)),
        app.request(post_verify(&token)),
    );
    let statuses = [first.status, second.status];
    assert_eq!(
        statuses
            .iter()
            .filter(|s| **s == StatusCode::NO_CONTENT)
            .count(),
        1,
        "exactly one 204, got {statuses:?}"
    );
    assert_eq!(
        statuses
            .iter()
            .filter(|s| **s == StatusCode::UNAUTHORIZED)
            .count(),
        1,
        "exactly one 401, got {statuses:?}"
    );
}

#[tokio::test]
async fn resend_sends_a_fresh_token_and_invalidates_the_old_one() {
    let app = verify_app().await;
    let (user_id, old_token) = register_and_token(&app).await;

    let res = app
        .request(post_resend(&unique_email_for(&app, user_id).await))
        .await;
    assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.text());
    assert!(res.text().is_empty(), "204 has no body");

    // A second mail is queued; the old token no longer works.
    assert_eq!(mail_count_for_user(&app, user_id).await, 2);
    let new_token = latest_token_for_user(&app, user_id).await;
    assert_ne!(new_token, old_token);
    let old = app.request(post_verify(&old_token)).await;
    assert_eq!(old.status, StatusCode::UNAUTHORIZED, "{}", old.text());
    assert_eq!(old.json()["code"], "PF-1010");
    // The fresh token verifies.
    let fresh = app.request(post_verify(&new_token)).await;
    assert_eq!(fresh.status, StatusCode::NO_CONTENT);

    // The resend is audited.
    let logs = audit_logs::list_for_target(&app.state.pool, "user", &user_id.to_string())
        .await
        .unwrap();
    assert!(
        logs.iter().any(|log| log.action == "verify-email.resend"),
        "{logs:?}"
    );
}

/// The email a user registered with, looked up from the row
/// (the register helper generates it internally, so resend
/// tests read it back from the stored row).
async fn unique_email_for(app: &TestApp, user_id: Uuid) -> String {
    users::by_id(&app.state.pool, user_id)
        .await
        .unwrap()
        .unwrap()
        .email
}

/// The newest unsent verification-mail token for a user (the
/// outbox rows are ordered by creation; the last one wins).
async fn latest_token_for_user(app: &TestApp, user_id: Uuid) -> String {
    let mail = email_outbox::unsent_for_template(&app.state.pool, VERIFY_EMAIL_TEMPLATE)
        .await
        .unwrap();
    let ours: Vec<_> = mail
        .iter()
        .filter(|row| row.payload["user_id"] == json!(user_id))
        .collect();
    ours.last().unwrap().payload["token"]
        .as_str()
        .unwrap()
        .to_string()
}

#[tokio::test]
async fn resend_rate_limited_at_three_per_hour() {
    let app = verify_app().await;
    let (user_id, _token) = register_and_token(&app).await;
    let email = unique_email_for(&app, user_id).await;

    // Register minted 1; two resends mint 2 and 3.
    for _ in 0..2 {
        let res = app.request(post_resend(&email)).await;
        assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.text());
    }
    assert_eq!(mail_count_for_user(&app, user_id).await, 3);

    // The third resend is still 204 (no enumeration) — but no
    // fourth mail is queued.
    let res = app.request(post_resend(&email)).await;
    assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.text());
    assert_eq!(mail_count_for_user(&app, user_id).await, 3);

    // The latest (third) token still verifies.
    let token = latest_token_for_user(&app, user_id).await;
    let res = app.request(post_verify(&token)).await;
    assert_eq!(res.status, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn resend_unknown_email_is_204_and_sends_nothing() {
    let app = verify_app().await;
    // The scratch DB is shared across tests in this binary, so
    // "sends nothing" must be scoped to this email's hash —
    // never a global outbox-emptiness assertion.
    let email = unique_email();
    let res = app.request(post_resend(&email)).await;
    assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.text());
    assert!(res.text().is_empty());
    let mail = email_outbox::unsent_for_template(&app.state.pool, VERIFY_EMAIL_TEMPLATE)
        .await
        .unwrap();
    assert!(
        mail.iter()
            .all(|row| row.to_hash.as_deref() != Some(sha256_hex(email.as_bytes()).as_str())),
        "no mail was queued for the unknown email: {mail:?}"
    );
}

#[tokio::test]
async fn resend_verified_user_is_204_and_sends_nothing() {
    let app = verify_app().await;
    let (user_id, token) = register_and_token(&app).await;
    let email = unique_email_for(&app, user_id).await;
    let res = app.request(post_verify(&token)).await;
    assert_eq!(res.status, StatusCode::NO_CONTENT);

    let res = app.request(post_resend(&email)).await;
    assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.text());
    assert_eq!(mail_count_for_user(&app, user_id).await, 1, "no new mail");
}

#[tokio::test]
async fn resend_malformed_email_is_422() {
    let app = verify_app().await;
    let res = app.request(post_resend("not-an-email")).await;
    assert_eq!(
        res.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        res.text()
    );
    assert_eq!(res.json()["code"], "PF-2002");
}

#[tokio::test]
async fn verify_empty_token_is_422() {
    let app = verify_app().await;
    let res = app.request(post_verify("   ")).await;
    assert_eq!(
        res.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        res.text()
    );
    assert_eq!(res.json()["code"], "PF-2002");
}

#[tokio::test]
async fn verify_malformed_json_is_422() {
    let app = verify_app().await;
    let req = Request::builder()
        .method(Method::POST)
        .uri("/v1/auth/verify-email")
        .header("x-request-id", REQUEST_ID)
        .header("content-type", "application/json")
        .body(axum::body::Body::from("{not json"))
        .unwrap();
    let res = app.request(req).await;
    assert_eq!(
        res.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        res.text()
    );
    assert_eq!(res.json()["code"], "PF-2002");
}

/// The job-creation gate (PF-1009): pure check on the loaded
/// user — unverified 403s, verified passes. The Phase 2
/// job-create handler calls `require_verified` first; until
/// then the guard is proven here, not by a live job endpoint.
#[test]
fn require_verified_gate_is_pf1009_403_for_unverified_users() {
    let mut user = users::User {
        id: Uuid::now_v7(),
        email: "gate@example.com".to_string(),
        password_hash: String::new(),
        email_verified_at: None,
        role: "user".to_string(),
        status: "active".to_string(),
        tos_version: Some("v1".to_string()),
        deleted_at: None,
        revoked_before: None,
        display_name: None,
        pending_email: None,
        settings: json!({}),
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    };
    let err = require_verified(&user).expect_err("unverified must fail");
    let problem = err.to_problem(Profile::Production, Some(REQUEST_ID), None);
    assert_eq!(problem.code, "PF-1009");
    assert_eq!(problem.status, 403);
    assert_eq!(problem.title, "email_not_verified");

    user.email_verified_at = Some(chrono::Utc::now());
    assert!(require_verified(&user).is_ok());
}
