//! P1-091: enforcement workflow (FR-C-02), end to end.
//!
//! Through the real router (`TestApp` oneshot) against the
//! migrated scratch database: an admin walks a user down
//! the ladder — warn (200, status unchanged, `user.warned`
//! audited, the warning mail enqueued) → suspend (the
//! P1-008 endpoint: status `suspended`, sessions revoked,
//! jobs held, the suspension mail enqueued) → terminate
//! (200, status `deletion_pending`, the 30-day grace,
//! `user.terminated` audited, the termination mail
//! enqueued) — and the terminated account cannot log in
//! (403 `PF-1004` after a correct password, the generic
//! 401 after a wrong one), while a self-requested deletion
//! still cancels on login. The wrong rung is 422, and a
//! suspension through the P1-090 queue mails the user too.

mod common;

use axum::http::{Method, Request, StatusCode};
use chrono::{Duration, Utc};
use common::{TestApp, TestFixtures};
use detag_core::{
    checksum::sha256_hex,
    repos::{audit_logs, email_outbox, jobs, jobs::NewJob, sessions, sessions::NewSession, users},
};
use detag_mailer::TemplateEngine;
use serde_json::json;
use uuid::Uuid;

/// The request id: a valid UUID, echoed by the middleware.
const REQUEST_ID: &str = "123e4567-e89b-42d3-a456-426614174104";

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
    format!("203.0.113.{}", Uuid::now_v7())
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
/// (the terminated-login tests assert on its status).
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

fn enforce_req(
    token: Option<&str>,
    user_id: Uuid,
    rung: &str,
    body: serde_json::Value,
) -> Request<axum::body::Body> {
    let mut req = Request::builder()
        .method(Method::POST)
        .uri(format!("/v1/admin/users/{user_id}/{rung}"))
        .header("x-request-id", REQUEST_ID)
        .header("x-forwarded-for", unique_xff())
        .header("content-type", "application/json");
    if let Some(token) = token {
        req = req.header("authorization", format!("Bearer {token}"));
    }
    req.body(axum::body::Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap()
}

fn suspend_req(
    token: Option<&str>,
    user_id: Uuid,
    body: serde_json::Value,
) -> Request<axum::body::Body> {
    enforce_req(token, user_id, "suspend", body)
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
            key_id: None,
        },
    )
    .await
    .expect("seed queued job")
    .id
}

/// Seed one open report naming `user_id` directly (the
/// P1-090 precedent — the queue tests need explicit
/// statuses, and no production caller files reports
/// except through the service).
async fn seed_report(app: &TestApp, user_id: Uuid) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO abuse_reports (user_id, reporter_contact, job_id, reason, status, created_at) \
         VALUES ($1, 'reporter@example.com', NULL, 'P1-091 probe report', 'open', $2) RETURNING id",
    )
    .bind(user_id)
    .bind(Utc::now())
    .fetch_one(&app.pool)
    .await
    .expect("seed abuse report")
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

/// The audit rows for one user + one action.
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

/// The `login.failure` audit rows for a user with reason
/// `terminated`.
async fn login_failures(app: &TestApp, user_id: Uuid) -> Vec<audit_logs::AuditLog> {
    audits_for(app, user_id, "login.failure")
        .await
        .into_iter()
        .filter(|row| row.reason.as_deref() == Some("terminated"))
        .collect()
}

/// The unsent mails of one template addressed to `email`
/// (matched on the sha256 — the outbox carries no raw
/// address).
async fn mails_for(app: &TestApp, email: &str, template: &str) -> Vec<email_outbox::OutboxMail> {
    let mails = email_outbox::unsent_for_template(&app.state.pool, template)
        .await
        .expect("list outbox");
    let hash = sha256_hex(email.as_bytes());
    mails
        .into_iter()
        .filter(|m| m.to_hash.as_deref() == Some(&hash))
        .collect()
}

/// Render one enqueued row exactly as the drain would: the
/// row's payload plus the drain's own `base_url` / `app_name`
/// (drain.rs) — nothing else. A missing template variable
/// fails loudly here, the same way it would park the row.
fn render_as_drain(row: &email_outbox::OutboxMail) -> detag_mailer::RenderedMail {
    let engine = TemplateEngine::new().expect("templates compile");
    let mut ctx = row.payload.clone();
    let obj = ctx.as_object_mut().expect("payload is an object");
    obj.insert("base_url".to_string(), json!("https://example.invalid"));
    obj.insert("app_name".to_string(), json!("Detag"));
    engine
        .render(&row.template, &ctx)
        .expect("the enqueued payload renders its template")
}

async fn user_status(app: &TestApp, user_id: Uuid) -> String {
    users::by_id(&app.state.pool, user_id)
        .await
        .expect("load user")
        .expect("user exists")
        .status
}

/// The AC: the ladder end to end — warn → suspend →
/// terminate — with the audit row, the mail, and the
/// effect asserted at every rung, and the terminated
/// account refused at login.
#[tokio::test]
async fn enforcement_ladder_e2e() {
    let app = admin_app().await;
    let (admin_id, admin_token) = make_admin(&app, &unique_email("admin")).await;
    let email = unique_email("user");
    let user_id = register_user(&app, &email).await;
    seed_session(&app, user_id).await;
    let job_id = seed_queued_job(&app, user_id).await;

    // Rung 1 — warn: the status does not change, but the
    // warning is audited and mailed.
    let res = app
        .request(enforce_req(
            Some(&admin_token),
            user_id,
            "warn",
            json!({ "reason": "stock-agency template matches" }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert_eq!(res.json()["step"], "warn", "{}", res.text());
    assert_eq!(res.json()["already"], false, "{}", res.text());
    assert_eq!(user_status(&app, user_id).await, "active");

    let warned = audits_for(&app, user_id, "user.warned").await;
    assert_eq!(warned.len(), 1, "exactly one warning audit");
    assert_eq!(
        warned[0].actor_user_id,
        Some(admin_id),
        "actor is the admin"
    );
    assert_eq!(
        warned[0].reason.as_deref(),
        Some("stock-agency template matches")
    );
    assert_eq!(warned[0].after.as_ref().unwrap()["warnings"], 1);

    let mails = mails_for(&app, &email, "enforcement-warning").await;
    assert_eq!(mails.len(), 1, "exactly one warning mail");
    assert_eq!(mails[0].payload["reason"], "stock-agency template matches");
    let rendered = render_as_drain(&mails[0]);
    assert!(
        rendered.text_body.contains("stock-agency template matches"),
        "warning mail carries the reason: {}",
        rendered.text_body
    );

    // Rung 2 — suspend (the P1-008 endpoint): sessions
    // revoked, the job held, the suspension mailed.
    let res = app
        .request(suspend_req(
            Some(&admin_token),
            user_id,
            json!({ "reason": "second violation" }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert_eq!(user_status(&app, user_id).await, "suspended");

    let live_sessions = sessions::list_for_user(&app.state.pool, user_id)
        .await
        .expect("list sessions");
    assert_eq!(live_sessions.len(), 1);
    assert!(
        live_sessions[0].revoked_at.is_some(),
        "the session is revoked: {live_sessions:?}"
    );
    let job = jobs::by_id(&app.state.pool, job_id)
        .await
        .expect("load job")
        .expect("job exists");
    assert_eq!(job.status, "held");

    let mails = mails_for(&app, &email, "enforcement-suspension").await;
    assert_eq!(mails.len(), 1, "exactly one suspension mail");
    assert_eq!(mails[0].payload["reason"], "second violation");
    let rendered = render_as_drain(&mails[0]);
    assert!(
        rendered.text_body.contains("second violation"),
        "suspension mail carries the reason: {}",
        rendered.text_body
    );

    // Rung 3 — terminate: the 30-day grace, the audit, the
    // mail. The session and job are already revoked/held,
    // so the counts are 0 — the terminate effects are
    // proven on fresh rows by
    // `enforcement_terminate_effects_revoke_and_hold`.
    let res = app
        .request(enforce_req(
            Some(&admin_token),
            user_id,
            "terminate",
            json!({ "reason": "third violation" }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert_eq!(res.json()["step"], "terminate", "{}", res.text());
    assert_eq!(res.json()["already"], false, "{}", res.text());
    assert_eq!(res.json()["sessions_revoked"], 0, "{}", res.text());
    assert_eq!(res.json()["jobs_held"], 0, "{}", res.text());

    let user = users::by_id(&app.state.pool, user_id)
        .await
        .expect("load user")
        .expect("user exists");
    assert_eq!(user.status, "deletion_pending");
    assert!(user.deleted_at.is_some(), "the grace starts now");

    let terminated = audits_for(&app, user_id, "user.terminated").await;
    assert_eq!(terminated.len(), 1, "exactly one termination audit");
    assert_eq!(terminated[0].actor_user_id, Some(admin_id));
    assert_eq!(terminated[0].reason.as_deref(), Some("third violation"));
    assert_eq!(
        terminated[0].after.as_ref().unwrap()["status"],
        "deletion_pending"
    );

    let mails = mails_for(&app, &email, "enforcement-termination").await;
    assert_eq!(mails.len(), 1, "exactly one termination mail");
    assert_eq!(mails[0].payload["reason"], "third violation");
    let rendered = render_as_drain(&mails[0]);
    assert!(
        rendered.text_body.contains("third violation"),
        "termination mail carries the reason: {}",
        rendered.text_body
    );

    // The terminated account cannot log in: 403 PF-1004
    // after a correct password, the generic 401 after a
    // wrong one — and the attempt is audited.
    let res = login_raw(&app, &email, PASSWORD).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-1004", "{}", res.text());
    let res = login_raw(&app, &email, "wrong password").await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());
    let failures = login_failures(&app, user_id).await;
    assert_eq!(failures.len(), 1, "one terminated-login failure");
    assert_eq!(failures[0].reason.as_deref(), Some("terminated"));

    // The ladder ends here: warning a terminated account
    // is 422, and terminating it again is a 200 no-op —
    // no second audit, no second mail.
    let res = app
        .request(enforce_req(
            Some(&admin_token),
            user_id,
            "warn",
            json!({ "reason": "too late" }),
        ))
        .await;
    assert_eq!(
        res.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        res.text()
    );
    assert_eq!(res.json()["code"], "PF-2002", "{}", res.text());
    let res = app
        .request(enforce_req(
            Some(&admin_token),
            user_id,
            "terminate",
            json!({ "reason": "third violation" }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert_eq!(res.json()["already"], true, "{}", res.text());
    assert_eq!(audits_for(&app, user_id, "user.terminated").await.len(), 1);
    assert_eq!(
        mails_for(&app, &email, "enforcement-termination")
            .await
            .len(),
        1
    );
}

/// The ladder is sequential: terminating an active account
/// and warning a suspended one are both 422, and neither
/// writes an audit row or a mail.
#[tokio::test]
async fn enforcement_wrong_rung_is_422() {
    let app = admin_app().await;
    let (_admin_id, admin_token) = make_admin(&app, &unique_email("admin")).await;
    let email = unique_email("user");
    let user_id = register_user(&app, &email).await;

    // Terminate an active account: 422, untouched.
    let res = app
        .request(enforce_req(
            Some(&admin_token),
            user_id,
            "terminate",
            json!({ "reason": "skipping the ladder" }),
        ))
        .await;
    assert_eq!(
        res.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        res.text()
    );
    assert_eq!(res.json()["code"], "PF-2002", "{}", res.text());
    assert_eq!(user_status(&app, user_id).await, "active");
    assert_eq!(audits_for(&app, user_id, "user.terminated").await.len(), 0);
    assert_eq!(
        mails_for(&app, &email, "enforcement-termination")
            .await
            .len(),
        0
    );

    // Suspend, then warn: 422, no warning recorded.
    let res = app
        .request(suspend_req(
            Some(&admin_token),
            user_id,
            json!({ "reason": "first" }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let res = app
        .request(enforce_req(
            Some(&admin_token),
            user_id,
            "warn",
            json!({ "reason": "too late to warn" }),
        ))
        .await;
    assert_eq!(
        res.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        res.text()
    );
    assert_eq!(audits_for(&app, user_id, "user.warned").await.len(), 0);
    assert_eq!(
        mails_for(&app, &email, "enforcement-warning").await.len(),
        0
    );

    // Unknown users are 404; a missing or blank reason is 422.
    let unknown = Uuid::now_v7();
    let res = app
        .request(enforce_req(
            Some(&admin_token),
            unknown,
            "warn",
            json!({ "reason": "nobody" }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-2010", "{}", res.text());
    let res = app
        .request(enforce_req(
            Some(&admin_token),
            unknown,
            "terminate",
            json!({ "reason": "nobody" }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND, "{}", res.text());
    let res = app
        .request(enforce_req(Some(&admin_token), user_id, "warn", json!({})))
        .await;
    assert_eq!(
        res.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        res.text()
    );
    let res = app
        .request(enforce_req(
            Some(&admin_token),
            user_id,
            "terminate",
            json!({ "reason": "   " }),
        ))
        .await;
    assert_eq!(
        res.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        res.text()
    );
}

/// Terminate's own effects: on a suspended account with a
/// live session and a queued job, the termination revokes
/// the session and holds the job (the counts in the
/// response name them).
#[tokio::test]
async fn enforcement_terminate_effects_revoke_and_hold() {
    let app = admin_app().await;
    let (_admin_id, admin_token) = make_admin(&app, &unique_email("admin")).await;
    let user_id = register_user(&app, &unique_email("user")).await;
    let res = app
        .request(suspend_req(
            Some(&admin_token),
            user_id,
            json!({ "reason": "suspend first" }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());

    // Fresh rows after the suspension: the terminate must
    // revoke and hold these.
    seed_session(&app, user_id).await;
    let job_id = seed_queued_job(&app, user_id).await;

    let res = app
        .request(enforce_req(
            Some(&admin_token),
            user_id,
            "terminate",
            json!({ "reason": "terminate now" }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert_eq!(res.json()["sessions_revoked"], 1, "{}", res.text());
    assert_eq!(res.json()["jobs_held"], 1, "{}", res.text());

    let live_sessions = sessions::list_for_user(&app.state.pool, user_id)
        .await
        .expect("list sessions");
    assert!(live_sessions.iter().all(|s| s.revoked_at.is_some()));
    let job = jobs::by_id(&app.state.pool, job_id)
        .await
        .expect("load job")
        .expect("job exists");
    assert_eq!(job.status, "held");
    assert_eq!(user_status(&app, user_id).await, "deletion_pending");
}

/// A suspension through the P1-090 queue mails the user
/// too — the suspension mail belongs to the rung, not to
/// the endpoint that ran it.
#[tokio::test]
async fn enforcement_queue_suspend_mails_the_user() {
    let app = admin_app().await;
    let (_admin_id, admin_token) = make_admin(&app, &unique_email("admin")).await;
    let email = unique_email("user");
    let user_id = register_user(&app, &email).await;
    let report_id = seed_report(&app, user_id).await;

    let res = app
        .request(
            Request::builder()
                .method(Method::POST)
                .uri(format!("/v1/admin/abuse-reports/{report_id}/resolve"))
                .header("x-request-id", REQUEST_ID)
                .header("x-forwarded-for", unique_xff())
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {admin_token}"))
                .body(axum::body::Body::from(
                    serde_json::to_vec(&json!({
                        "action": "suspend_user",
                        "reason": "queue suspension",
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert_eq!(user_status(&app, user_id).await, "suspended");

    let mails = mails_for(&app, &email, "enforcement-suspension").await;
    assert_eq!(mails.len(), 1, "exactly one suspension mail");
    assert_eq!(mails[0].payload["reason"], "queue suspension");
    let rendered = render_as_drain(&mails[0]);
    assert!(
        rendered.text_body.contains("queue suspension"),
        "queue suspension mail carries the reason: {}",
        rendered.text_body
    );
}

/// A self-requested deletion still cancels on login
/// (P1-007): only an admin termination is sticky.
#[tokio::test]
async fn enforcement_self_requested_deletion_still_cancels_on_login() {
    let app = admin_app().await;
    let email = unique_email("user");
    let user_id = register_user(&app, &email).await;
    let token = login_token(&app, &email).await;

    let res = app.request(delete_me(Some(&token))).await;
    assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.text());
    assert_eq!(user_status(&app, user_id).await, "deletion_pending");

    // The login cancels the deletion: 200, active again.
    let res = login_raw(&app, &email, PASSWORD).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert_eq!(user_status(&app, user_id).await, "active");
}
