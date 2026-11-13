//! P1-007: account deletion, end to end.
//!
//! Through the real router (`TestApp` oneshot) against the
//! migrated scratch database: DELETE /v1/me requests the
//! deletion (204, `status = 'deletion_pending'`, `deleted_at`
//! set, `account.delete_requested` audited, confirmation mail
//! queued — the account is NOT deleted yet), a second DELETE
//! is a no-op (grace runs from the first request, no second
//! mail), and a login during the grace cancels it (`status`
//! back to `active`, `deleted_at` cleared, `account.delete_cancelled`
//! audited). The purge itself is the gc binary's job — see
//! `apps/gc/src/purge.rs` (its tests prove the cascade).

mod common;

use axum::http::{Method, Request, StatusCode};
use common::{TestApp, TestFixtures};
use detag_api::services::deletion::ACCOUNT_DELETION_TEMPLATE;
use detag_core::{
    checksum::sha256_hex,
    repos::{audit_logs, email_outbox, users},
};
use serde_json::json;
use uuid::Uuid;

/// The request id: a valid UUID, echoed by the middleware.
const REQUEST_ID: &str = "123e4567-e89b-42d3-a456-426614174103";

/// The password every test user registers with.
const PASSWORD: &str = "correct horse battery staple";

async fn deletion_app() -> TestApp {
    let fixtures = TestFixtures::from_env().await;
    let mut settings = fixtures.settings();
    settings.disposable_email_domains = "mailinator.com".to_string();
    settings.tos_current_version = "v1".to_string();
    TestApp::spawn_with_fixtures_and_settings(fixtures, settings).await
}

fn unique_email() -> String {
    format!("deletion-{}@example.com", Uuid::now_v7())
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

fn delete_me(token: Option<&str>) -> Request<axum::body::Body> {
    let mut req = Request::builder()
        .method(Method::DELETE)
        .uri("/v1/me")
        .header("x-request-id", REQUEST_ID)
        .header("x-forwarded-for", unique_xff());
    if let Some(token) = token {
        req = req.header("authorization", format!("Bearer {token}"));
    }
    req.body(axum::body::Body::empty()).unwrap()
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

/// The `account.delete_requested` audit rows for a user.
async fn delete_requests(
    app: &TestApp,
    user_id: Uuid,
) -> Vec<detag_core::repos::audit_logs::AuditLog> {
    let rows = audit_logs::list_for_action(&app.state.pool, "account.delete_requested")
        .await
        .expect("list audits");
    rows.into_iter()
        .filter(|a| a.target_id.as_deref() == Some(&user_id.to_string()))
        .collect()
}

/// The `account-deletion` mails for a user (matched on the
/// email hash — the outbox carries no raw address).
async fn deletion_mails(
    app: &TestApp,
    email: &str,
) -> Vec<detag_core::repos::email_outbox::OutboxMail> {
    let mails = email_outbox::unsent_for_template(&app.state.pool, ACCOUNT_DELETION_TEMPLATE)
        .await
        .expect("list outbox");
    let hash = sha256_hex(email.as_bytes());
    mails
        .into_iter()
        .filter(|m| m.to_hash.as_deref() == Some(&hash))
        .collect()
}

#[tokio::test]
async fn delete_me_happy_path() {
    let app = deletion_app().await;
    let email = unique_email();
    let user_id = register_user(&app, &email).await;
    let token = login_token(&app, &email).await;

    let res = app.request(delete_me(Some(&token))).await;
    assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.text());
    assert!(res.text().is_empty(), "204 carries no body");

    // The profile shows the grace, and the account is NOT
    // deleted yet — GET still works.
    let res = app.request(get_me(&token)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let json = res.json();
    assert_eq!(json["status"], "deletion_pending");
    assert!(json["deleted_at"].is_string(), "{json}");

    // The row agrees with the profile.
    let user = users::by_id(&app.state.pool, user_id)
        .await
        .expect("lookup")
        .expect("user still exists");
    assert_eq!(user.status, "deletion_pending");
    assert!(user.deleted_at.is_some());

    // One audit row (actor = the user, after = pending) and
    // one confirmation mail (to the address on record, payload
    // `{user_id}` — the mail drain renders the 30-day notice
    // from the user row).
    let audits = delete_requests(&app, user_id).await;
    assert_eq!(audits.len(), 1, "{audits:?}");
    assert_eq!(audits[0].actor_user_id, Some(user_id));
    assert_eq!(
        audits[0].after.as_ref().unwrap()["status"],
        "deletion_pending"
    );
    let mails = deletion_mails(&app, &email).await;
    assert_eq!(mails.len(), 1, "{mails:?}");
    assert_eq!(mails[0].payload["user_id"], user_id.to_string());
}

#[tokio::test]
async fn delete_me_is_idempotent() {
    let app = deletion_app().await;
    let email = unique_email();
    let user_id = register_user(&app, &email).await;
    let token = login_token(&app, &email).await;

    let res = app.request(delete_me(Some(&token))).await;
    assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.text());
    let first = app.request(get_me(&token)).await.json();
    let first_deleted_at = first["deleted_at"].as_str().unwrap().to_string();

    // A second DELETE is a no-op 204: the grace runs from the
    // FIRST request — no refreshed `deleted_at`, no second
    // audit row, no second mail.
    let res = app.request(delete_me(Some(&token))).await;
    assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.text());
    let second = app.request(get_me(&token)).await.json();
    assert_eq!(second["deleted_at"].as_str().unwrap(), first_deleted_at);
    assert_eq!(delete_requests(&app, user_id).await.len(), 1);
    assert_eq!(deletion_mails(&app, &email).await.len(), 1);
}

#[tokio::test]
async fn delete_me_requires_auth() {
    let app = deletion_app().await;
    let res = app.request(delete_me(None)).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-1001");
}

#[tokio::test]
async fn login_cancels_deletion() {
    let app = deletion_app().await;
    let email = unique_email();
    let user_id = register_user(&app, &email).await;
    let token = login_token(&app, &email).await;

    let res = app.request(delete_me(Some(&token))).await;
    assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.text());
    let pending = app.request(get_me(&token)).await.json();
    assert_eq!(pending["status"], "deletion_pending");

    // A login during the grace cancels the deletion — and the
    // account is usable again (the login itself succeeds: the
    // account was never deleted, only marked).
    let token = login_token(&app, &email).await;
    let res = app.request(get_me(&token)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let json = res.json();
    assert_eq!(json["status"], "active");
    assert!(json["deleted_at"].is_null(), "{json}");

    let user = users::by_id(&app.state.pool, user_id)
        .await
        .expect("lookup")
        .expect("user still exists");
    assert_eq!(user.status, "active");
    assert!(user.deleted_at.is_none());

    // One `account.delete_cancelled` audit row (actor = the
    // user, after = active) — the cancel commits with the new
    // session.
    let rows = audit_logs::list_for_action(&app.state.pool, "account.delete_cancelled")
        .await
        .expect("list audits");
    let cancels: Vec<_> = rows
        .iter()
        .filter(|a| a.target_id.as_deref() == Some(&user_id.to_string()))
        .collect();
    assert_eq!(cancels.len(), 1, "{rows:?}");
    assert_eq!(cancels[0].actor_user_id, Some(user_id));
    assert_eq!(cancels[0].after.as_ref().unwrap()["status"], "active");
}

#[tokio::test]
async fn login_after_cancel_does_not_re_cancel() {
    let app = deletion_app().await;
    let email = unique_email();
    let user_id = register_user(&app, &email).await;
    let token = login_token(&app, &email).await;

    let res = app.request(delete_me(Some(&token))).await;
    assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.text());
    let _token = login_token(&app, &email).await; // cancels

    // A second login after the cancel is a plain login: no
    // second `account.delete_cancelled` row.
    let _token = login_token(&app, &email).await;
    let rows = audit_logs::list_for_action(&app.state.pool, "account.delete_cancelled")
        .await
        .expect("list audits");
    let cancels: Vec<_> = rows
        .iter()
        .filter(|a| a.target_id.as_deref() == Some(&user_id.to_string()))
        .collect();
    assert_eq!(cancels.len(), 1, "{rows:?}");
}
