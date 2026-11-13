//! P1-018: revoke on password change, end to end.
//!
//! The password-change flows do not exist yet as HTTP
//! endpoints (the reset flow is P1-025; no change-password
//! route exists) — so these tests call the service directly
//! (`revoke_sessions_on_password_change`) with the `TestApp`'s
//! state, plus the real refresh endpoint to prove the kept
//! session is genuinely alive:
//!
//! - change-password case (`Some(keep)`): every live session
//!   except the current one is revoked; the audit
//!   `session.revoked_on_password_change` carries the count;
//!   the digest mail (`new-login-digest`) is queued.
//! - reset case (`None`): every session is revoked (a reset
//!   happens logged-out — there is no current session).
//! - no sessions: no-op — no audit, but the digest mail is
//!   still queued (the password DID change; the mail
//!   notifies about the change, the audit records the
//!   revocation — two different events).
//! - the kept session still rotates (proven through the real
//!   `POST /v1/auth/refresh`); a revoked one answers the
//!   generic 401 `PF-1001`.

mod common;

use axum::http::{Method, Request, StatusCode};
use common::{TestApp, TestFixtures};
use detag_api::services::password::{
    revoke_sessions_on_password_change, NEW_LOGIN_DIGEST_TEMPLATE,
};
use detag_core::{
    checksum::sha256_hex,
    repos::{audit_logs, email_outbox, sessions},
};
use serde_json::json;
use uuid::Uuid;

/// The request id: a valid UUID, echoed by the middleware.
const REQUEST_ID: &str = "123e4567-e89b-42d3-a456-426614174103";

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

/// Log in, return (access token, refresh token).
async fn login_tokens(app: &TestApp, email: &str) -> (String, String) {
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
    let access = res.json()["access_token"].as_str().unwrap().to_string();
    let refresh = res
        .headers
        .get("set-cookie")
        .expect("set-cookie header")
        .to_str()
        .expect("cookie is ascii")
        .split(';')
        .next()
        .expect("cookie has a value")
        .split_once('=')
        .expect("cookie is name=value")
        .1
        .to_string();
    assert!(!refresh.is_empty(), "refresh token is non-empty");
    (access, refresh)
}

/// POST /v1/auth/refresh with the given cookie value.
fn refresh_req(cookie: &str) -> Request<axum::body::Body> {
    Request::builder()
        .method(Method::POST)
        .uri("/v1/auth/refresh")
        .header("x-request-id", REQUEST_ID)
        .header("x-forwarded-for", unique_xff())
        .header("cookie", format!("detag_refresh={cookie}"))
        .body(axum::body::Body::empty())
        .unwrap()
}

/// The session id behind a refresh token (via the hash).
async fn session_id_for(app: &TestApp, refresh_token: &str) -> Uuid {
    sessions::by_refresh_hash(&app.state.pool, &sha256_hex(refresh_token.as_bytes()))
        .await
        .expect("lookup session")
        .expect("session exists")
        .id
}

/// The audit rows of one action for a user.
async fn audits_for(
    app: &TestApp,
    user_id: Uuid,
    action: &str,
) -> Vec<detag_core::repos::audit_logs::AuditLog> {
    audit_logs::list_for_target(&app.state.pool, "user", &user_id.to_string())
        .await
        .expect("list audits")
        .into_iter()
        .filter(|row| row.action == action)
        .collect()
}

/// The digest mails queued for a user (by email hash).
async fn digest_mails(
    app: &TestApp,
    email: &str,
) -> Vec<detag_core::repos::email_outbox::OutboxMail> {
    let hash = sha256_hex(email.as_bytes());
    email_outbox::unsent_for_template(&app.state.pool, NEW_LOGIN_DIGEST_TEMPLATE)
        .await
        .expect("list outbox")
        .into_iter()
        .filter(|mail| mail.to_hash.as_deref() == Some(hash.as_str()))
        .collect()
}

#[tokio::test]
async fn revokes_all_except_current() {
    let app = app().await;
    let email = unique_email("user");
    let user_id = register_user(&app, &email).await;
    let (_access_a, refresh_a) = login_tokens(&app, &email).await;
    let (_access_b, refresh_b) = login_tokens(&app, &email).await;
    let (_access_c, refresh_c) = login_tokens(&app, &email).await;
    // A fourth session in A's family (rotation): A is
    // already revoked, D is the current one to keep.
    let res = app.request(refresh_req(&refresh_a)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let refresh_d = res
        .headers
        .get("set-cookie")
        .expect("set-cookie header")
        .to_str()
        .expect("cookie is ascii")
        .split(';')
        .next()
        .expect("cookie has a value")
        .split_once('=')
        .expect("cookie is name=value")
        .1
        .to_string();
    let keep = session_id_for(&app, &refresh_d).await;

    let revoked = revoke_sessions_on_password_change(&app.state, user_id, Some(keep))
        .await
        .expect("revoke");
    // B + C (A was already revoked by the rotation and is not
    // counted twice; D is kept).
    assert_eq!(revoked, 2);

    // D is live, B and C are revoked, A is still revoked.
    let rows = sessions::list_for_user(&app.state.pool, user_id)
        .await
        .expect("list sessions");
    assert_eq!(rows.len(), 4);
    for row in &rows {
        if row.id == keep {
            assert!(row.revoked_at.is_none(), "the kept session must stay live");
        } else {
            assert!(
                row.revoked_at.is_some(),
                "every other session must be revoked"
            );
        }
    }

    // The revocation is audited once, with the count + the
    // kept session.
    let audits = audits_for(&app, user_id, "session.revoked_on_password_change").await;
    assert_eq!(audits.len(), 1);
    assert_eq!(audits[0].actor_user_id, Some(user_id));
    assert_eq!(audits[0].after.as_ref().unwrap()["sessions_revoked"], 2);
    assert_eq!(
        audits[0].after.as_ref().unwrap()["kept_session_id"],
        keep.to_string()
    );

    // The digest mail is queued for the user.
    let mails = digest_mails(&app, &email).await;
    assert_eq!(mails.len(), 1);
    assert_eq!(mails[0].payload["user_id"], user_id.to_string());
    assert_eq!(mails[0].payload["sessions_revoked"], 2);

    // The kept session still rotates (proven through the real
    // refresh endpoint); a revoked one answers the generic 401.
    let res = app.request(refresh_req(&refresh_d)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let res = app.request(refresh_req(&refresh_b)).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-1001", "{}", res.text());
    let _ = refresh_c;
}

#[tokio::test]
async fn revokes_all_when_no_current_session() {
    let app = app().await;
    let email = unique_email("user");
    let user_id = register_user(&app, &email).await;
    let (_access_a, _refresh_a) = login_tokens(&app, &email).await;
    let (_access_b, _refresh_b) = login_tokens(&app, &email).await;

    // The reset case: no current session — everything goes.
    let revoked = revoke_sessions_on_password_change(&app.state, user_id, None)
        .await
        .expect("revoke");
    assert_eq!(revoked, 2);

    let rows = sessions::list_for_user(&app.state.pool, user_id)
        .await
        .expect("list sessions");
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|row| row.revoked_at.is_some()));

    let audits = audits_for(&app, user_id, "session.revoked_on_password_change").await;
    assert_eq!(audits.len(), 1);
    assert_eq!(audits[0].after.as_ref().unwrap()["sessions_revoked"], 2);
    assert!(audits[0].after.as_ref().unwrap()["kept_session_id"].is_null());
    assert_eq!(digest_mails(&app, &email).await.len(), 1);
}

#[tokio::test]
async fn no_sessions_is_noop_no_audit_but_still_mails() {
    let app = app().await;
    let email = unique_email("user");
    let user_id = register_user(&app, &email).await;

    // No sessions at all: nothing to revoke, so no audit —
    // but the digest mail is still queued, because the
    // password DID change (the mail notifies about the
    // change; the audit records the revocation).
    let revoked = revoke_sessions_on_password_change(&app.state, user_id, None)
        .await
        .expect("revoke");
    assert_eq!(revoked, 0);
    assert_eq!(
        audits_for(&app, user_id, "session.revoked_on_password_change")
            .await
            .len(),
        0
    );
    let mails = digest_mails(&app, &email).await;
    assert_eq!(mails.len(), 1);
    assert_eq!(mails[0].payload["sessions_revoked"], 0);
}
