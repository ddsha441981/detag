//! P1-008: admin suspend / restore, end to end.
//!
//! Through the real router (`TestApp` oneshot) against the
//! migrated scratch database: an admin suspends a user (200,
//! status `suspended`, every live session revoked, every
//! queued job held, `user.suspended` audited with the reason
//! and the counts), a restore brings them back (status
//! `active`, held jobs released, sessions stay revoked,
//! `user.restored` audited), and both are idempotent no-ops
//! when the account is already in the target state. A
//! suspended user cannot log in (403 `PF-1003` after a
//! correct password, the generic 401 after a wrong one), and
//! a suspend cancels a pending deletion (the grace does not
//! survive admin intervention). Non-admins get 403 `PF-1002`;
//! unauthenticated callers get 401 `PF-1001`.

mod common;

use axum::http::{Method, Request, StatusCode};
use chrono::{Duration, Utc};
use common::{TestApp, TestFixtures};
use detag_core::{
    checksum::sha256_hex,
    repos::{audit_logs, jobs, jobs::NewJob, sessions, sessions::NewSession, users},
};
use serde_json::json;
use uuid::Uuid;

/// The request id: a valid UUID, echoed by the middleware.
const REQUEST_ID: &str = "123e4567-e89b-42d3-a456-426614174103";

/// The password every test user registers with.
const PASSWORD: &str = "correct horse battery staple";

async fn admin_app() -> TestApp {
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

/// Log in with the given password, return the raw response
/// (the suspended-login tests assert on its status).
async fn login_raw(app: &TestApp, email: &str, password: &str) -> common::TestResponse {
    let body = json!({
        "email": email,
        "password": password,
    });
    app.request(
        Request::builder()
            .method(Method::POST)
            .uri("/v1/auth/login")
            .header("x-request-id", REQUEST_ID)
            .header("x-forwarded-for", unique_xff())
            .header("content-type", "application/json")
            .body(axum::body::Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap(),
    )
    .await
}

/// Log in, return the access token.
async fn login_token(app: &TestApp, email: &str) -> String {
    let res = login_raw(app, email, PASSWORD).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    res.json()["access_token"].as_str().unwrap().to_string()
}

/// Register a user, promote them to admin in the database,
/// log in, return (id, token). The promotion is a direct
/// UPDATE — there is no self-serve way to become an admin
/// (the role is set by the owner out of band; P1-009's search
/// is the read side, the invite CLI is a DECISIONS.md item).
async fn make_admin(app: &TestApp, email: &str) -> (Uuid, String) {
    let id = register_user(app, email).await;
    sqlx::query("UPDATE users SET role = 'admin' WHERE id = $1")
        .bind(id)
        .execute(&app.pool)
        .await
        .expect("promote to admin");
    let token = login_token(app, email).await;
    (id, token)
}

fn suspend_req(
    token: Option<&str>,
    user_id: Uuid,
    body: serde_json::Value,
) -> Request<axum::body::Body> {
    let mut req = Request::builder()
        .method(Method::POST)
        .uri(format!("/v1/admin/users/{user_id}/suspend"))
        .header("x-request-id", REQUEST_ID)
        .header("x-forwarded-for", unique_xff())
        .header("content-type", "application/json");
    if let Some(token) = token {
        req = req.header("authorization", format!("Bearer {token}"));
    }
    req.body(axum::body::Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap()
}

fn restore_req(token: Option<&str>, user_id: Uuid) -> Request<axum::body::Body> {
    let mut req = Request::builder()
        .method(Method::POST)
        .uri(format!("/v1/admin/users/{user_id}/restore"))
        .header("x-request-id", REQUEST_ID)
        .header("x-forwarded-for", unique_xff());
    if let Some(token) = token {
        req = req.header("authorization", format!("Bearer {token}"));
    }
    req.body(axum::body::Body::empty()).unwrap()
}

/// Seed a live session for a user (the gc-test pattern).
async fn seed_session(app: &TestApp, user_id: Uuid) {
    sessions::create(
        &app.state.pool,
        &NewSession {
            user_id,
            refresh_hash: sha256_hex(format!("{user_id}-{}", Uuid::now_v7()).as_bytes()),
            family_id: Uuid::now_v7(),
            device: json!({ "ua": "test" }),
            expires_at: Utc::now() + Duration::days(30),
        },
    )
    .await
    .expect("seed session");
}

/// Seed a queued job for a user (no upload — the job is a
/// bare status row; the upload row is Phase 2's intake).
async fn seed_queued_job(app: &TestApp, user_id: Uuid) -> Uuid {
    jobs::create(
        &app.state.pool,
        &NewJob {
            user_id,
            upload_id: None,
            kind: "image",
            mode: "auto",
            status: "queued",
            lane: "standard",
            params: json!({}),
            idempotency_key: None,
        },
    )
    .await
    .expect("seed queued job")
    .id
}

/// The `user.suspended` audit rows for a user.
async fn suspended_audits(
    app: &TestApp,
    user_id: Uuid,
) -> Vec<detag_core::repos::audit_logs::AuditLog> {
    audit_logs::list_for_target(&app.state.pool, "user", &user_id.to_string())
        .await
        .expect("list audits")
        .into_iter()
        .filter(|row| row.action == "user.suspended")
        .collect()
}

/// The `user.restored` audit rows for a user.
async fn restored_audits(
    app: &TestApp,
    user_id: Uuid,
) -> Vec<detag_core::repos::audit_logs::AuditLog> {
    audit_logs::list_for_target(&app.state.pool, "user", &user_id.to_string())
        .await
        .expect("list audits")
        .into_iter()
        .filter(|row| row.action == "user.restored")
        .collect()
}

#[tokio::test]
async fn suspend_happy_path_revokes_sessions_and_holds_jobs() {
    let app = admin_app().await;
    let (_admin_id, admin_token) = make_admin(&app, &unique_email("admin")).await;
    let user_id = register_user(&app, &unique_email("user")).await;
    seed_session(&app, user_id).await;
    seed_session(&app, user_id).await;
    let job_id = seed_queued_job(&app, user_id).await;

    let res = app
        .request(suspend_req(
            Some(&admin_token),
            user_id,
            json!({ "reason": "abuse report #12" }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert_eq!(res.json()["status"], "suspended", "{}", res.text());
    assert_eq!(res.json()["id"], user_id.to_string(), "{}", res.text());

    let user = users::by_id(&app.state.pool, user_id)
        .await
        .expect("load user")
        .expect("user exists");
    assert_eq!(user.status, "suspended");

    let sessions = sessions::list_for_user(&app.state.pool, user_id)
        .await
        .expect("list sessions");
    assert_eq!(sessions.len(), 2);
    assert!(
        sessions.iter().all(|s| s.revoked_at.is_some()),
        "both sessions must be revoked: {sessions:?}"
    );

    let job = jobs::by_id(&app.state.pool, job_id)
        .await
        .expect("load job")
        .expect("job exists");
    assert_eq!(job.status, "held");

    let audits = suspended_audits(&app, user_id).await;
    assert_eq!(audits.len(), 1, "exactly one suspend audit");
    assert_eq!(audits[0].reason.as_deref(), Some("abuse report #12"));
    assert_eq!(audits[0].after.as_ref().unwrap()["sessions_revoked"], 2);
    assert_eq!(audits[0].after.as_ref().unwrap()["jobs_held"], 1);
    assert_eq!(audits[0].after.as_ref().unwrap()["status"], "suspended");
}

#[tokio::test]
async fn suspend_malformed_uuid_is_422() {
    let app = admin_app().await;
    let (_admin_id, admin_token) = make_admin(&app, &unique_email("admin")).await;

    // A non-UUID path segment: axum's Path<Uuid> extractor
    // rejects with 400, and the error mapper rewrites it to
    // the 422 PF-2002 problem+json envelope (the same shape
    // every other validation failure takes).
    let res = app
        .request(
            Request::builder()
                .method(Method::POST)
                .uri("/v1/admin/users/not-a-uuid/suspend")
                .header("x-request-id", REQUEST_ID)
                .header("x-forwarded-for", unique_xff())
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {admin_token}"))
                .body(axum::body::Body::from(
                    serde_json::to_vec(&json!({ "reason": "abuse" })).unwrap(),
                ))
                .unwrap(),
        )
        .await;
    assert_eq!(
        res.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        res.text()
    );
    assert_eq!(res.json()["code"], "PF-2002", "{}", res.text());
}

#[tokio::test]
async fn suspend_non_admin_forbidden() {
    let app = admin_app().await;
    let user_email = unique_email("user");
    let user_id = register_user(&app, &user_email).await;
    let user_token = login_token(&app, &user_email).await;

    let res = app
        .request(suspend_req(
            Some(&user_token),
            user_id,
            json!({ "reason": "abuse" }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-1002", "{}", res.text());
    // The target was not touched.
    let user = users::by_id(&app.state.pool, user_id)
        .await
        .expect("load user")
        .expect("user exists");
    assert_eq!(user.status, "active");
}

#[tokio::test]
async fn suspend_unauthenticated() {
    let app = admin_app().await;
    let user_id = register_user(&app, &unique_email("user")).await;

    let res = app
        .request(suspend_req(None, user_id, json!({ "reason": "abuse" })))
        .await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-1001", "{}", res.text());
}

#[tokio::test]
async fn suspend_unknown_user_not_found() {
    let app = admin_app().await;
    let (_admin_id, admin_token) = make_admin(&app, &unique_email("admin")).await;

    let res = app
        .request(suspend_req(
            Some(&admin_token),
            Uuid::now_v7(),
            json!({ "reason": "abuse" }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-2010", "{}", res.text());
}

#[tokio::test]
async fn suspend_missing_or_blank_reason_is_422() {
    let app = admin_app().await;
    let (_admin_id, admin_token) = make_admin(&app, &unique_email("admin")).await;
    let user_id = register_user(&app, &unique_email("user")).await;

    // No reason field at all.
    let res = app
        .request(suspend_req(Some(&admin_token), user_id, json!({})))
        .await;
    assert_eq!(
        res.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        res.text()
    );
    assert_eq!(res.json()["code"], "PF-2002", "{}", res.text());

    // Blank reason (whitespace only).
    let res = app
        .request(suspend_req(
            Some(&admin_token),
            user_id,
            json!({ "reason": "   " }),
        ))
        .await;
    assert_eq!(
        res.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        res.text()
    );
    assert_eq!(res.json()["code"], "PF-2002", "{}", res.text());
    assert_eq!(
        res.json()["field_errors"][0]["field"],
        "reason",
        "{}",
        res.text()
    );

    // The target was not touched by either rejection.
    let user = users::by_id(&app.state.pool, user_id)
        .await
        .expect("load user")
        .expect("user exists");
    assert_eq!(user.status, "active");
}

#[tokio::test]
async fn suspend_already_suspended_is_noop() {
    let app = admin_app().await;
    let (_admin_id, admin_token) = make_admin(&app, &unique_email("admin")).await;
    let user_id = register_user(&app, &unique_email("user")).await;

    let res = app
        .request(suspend_req(
            Some(&admin_token),
            user_id,
            json!({ "reason": "first" }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());

    let res = app
        .request(suspend_req(
            Some(&admin_token),
            user_id,
            json!({ "reason": "second, ignored" }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert_eq!(res.json()["status"], "suspended", "{}", res.text());

    // No second audit — the no-op writes nothing.
    assert_eq!(suspended_audits(&app, user_id).await.len(), 1);
}

#[tokio::test]
async fn restore_happy_path_releases_jobs_and_keeps_sessions_revoked() {
    let app = admin_app().await;
    let (_admin_id, admin_token) = make_admin(&app, &unique_email("admin")).await;
    let user_id = register_user(&app, &unique_email("user")).await;
    seed_session(&app, user_id).await;
    let job_id = seed_queued_job(&app, user_id).await;

    let res = app
        .request(suspend_req(
            Some(&admin_token),
            user_id,
            json!({ "reason": "abuse" }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());

    let res = app.request(restore_req(Some(&admin_token), user_id)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert_eq!(res.json()["status"], "active", "{}", res.text());

    let user = users::by_id(&app.state.pool, user_id)
        .await
        .expect("load user")
        .expect("user exists");
    assert_eq!(user.status, "active");

    // The held job is back in the queue.
    let job = jobs::by_id(&app.state.pool, job_id)
        .await
        .expect("load job")
        .expect("job exists");
    assert_eq!(job.status, "queued");

    // The session stays revoked — revocation is permanent,
    // the user logs in again.
    let sessions = sessions::list_for_user(&app.state.pool, user_id)
        .await
        .expect("list sessions");
    assert_eq!(sessions.len(), 1);
    assert!(sessions[0].revoked_at.is_some());

    let audits = restored_audits(&app, user_id).await;
    assert_eq!(audits.len(), 1, "exactly one restore audit");
    assert_eq!(audits[0].after.as_ref().unwrap()["jobs_released"], 1);
    assert_eq!(audits[0].after.as_ref().unwrap()["status"], "active");
}

#[tokio::test]
async fn restore_active_user_is_noop() {
    let app = admin_app().await;
    let (_admin_id, admin_token) = make_admin(&app, &unique_email("admin")).await;
    let user_id = register_user(&app, &unique_email("user")).await;

    let res = app.request(restore_req(Some(&admin_token), user_id)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert_eq!(res.json()["status"], "active", "{}", res.text());
    assert_eq!(restored_audits(&app, user_id).await.len(), 0);
}

#[tokio::test]
async fn restore_non_admin_and_unauthenticated() {
    let app = admin_app().await;
    let user_email = unique_email("user");
    let user_id = register_user(&app, &user_email).await;
    let user_token = login_token(&app, &user_email).await;

    let res = app.request(restore_req(Some(&user_token), user_id)).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-1002", "{}", res.text());

    let res = app.request(restore_req(None, user_id)).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-1001", "{}", res.text());
}

#[tokio::test]
async fn restore_unknown_user_not_found() {
    let app = admin_app().await;
    let (_admin_id, admin_token) = make_admin(&app, &unique_email("admin")).await;

    let res = app
        .request(restore_req(Some(&admin_token), Uuid::now_v7()))
        .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-2010", "{}", res.text());
}

#[tokio::test]
async fn suspend_cancels_pending_deletion() {
    let app = admin_app().await;
    let (_admin_id, admin_token) = make_admin(&app, &unique_email("admin")).await;
    let user_email = unique_email("user");
    let user_id = register_user(&app, &user_email).await;
    let user_token = login_token(&app, &user_email).await;

    // Request the deletion (30-day grace).
    let res = app
        .request(
            Request::builder()
                .method(Method::DELETE)
                .uri("/v1/me")
                .header("x-request-id", REQUEST_ID)
                .header("x-forwarded-for", unique_xff())
                .header("authorization", format!("Bearer {user_token}"))
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.text());
    let user = users::by_id(&app.state.pool, user_id)
        .await
        .expect("load user")
        .expect("user exists");
    assert_eq!(user.status, "deletion_pending");
    assert!(user.deleted_at.is_some());

    // An admin suspends them: the grace is cancelled.
    let res = app
        .request(suspend_req(
            Some(&admin_token),
            user_id,
            json!({ "reason": "abuse confirmed" }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let user = users::by_id(&app.state.pool, user_id)
        .await
        .expect("load user")
        .expect("user exists");
    assert_eq!(user.status, "suspended");
    assert!(
        user.deleted_at.is_none(),
        "the grace must not survive the suspend"
    );
}

#[tokio::test]
async fn suspended_user_login_blocked_after_correct_password() {
    let app = admin_app().await;
    let (_admin_id, admin_token) = make_admin(&app, &unique_email("admin")).await;
    let user_email = unique_email("user");
    let user_id = register_user(&app, &user_email).await;

    let res = app
        .request(suspend_req(
            Some(&admin_token),
            user_id,
            json!({ "reason": "abuse" }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());

    // A correct password gets 403 PF-1003 — the account is
    // suspended, and the attempt is audited as a failure.
    let res = login_raw(&app, &user_email, PASSWORD).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-1003", "{}", res.text());

    // A wrong password is still the generic 401 — probing an
    // address learns nothing about whether it is suspended.
    let res = login_raw(&app, &user_email, "wrong password here").await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-1001", "{}", res.text());

    // The suspended attempt was audited (reason `suspended`).
    let rows = audit_logs::list_for_target(&app.state.pool, "user", &user_id.to_string())
        .await
        .expect("list audits");
    assert!(
        rows.iter()
            .any(|row| row.action == "login.failure" && row.reason.as_deref() == Some("suspended")),
        "a suspended login must audit login.failure with reason suspended: {rows:?}"
    );
}

#[tokio::test]
async fn suspended_user_can_log_in_after_restore() {
    let app = admin_app().await;
    let (_admin_id, admin_token) = make_admin(&app, &unique_email("admin")).await;
    let user_email = unique_email("user");
    let user_id = register_user(&app, &user_email).await;

    let res = app
        .request(suspend_req(
            Some(&admin_token),
            user_id,
            json!({ "reason": "abuse" }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let res = app.request(restore_req(Some(&admin_token), user_id)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());

    let res = login_raw(&app, &user_email, PASSWORD).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert!(res.json()["access_token"].is_string(), "{}", res.text());
}
