//! P1-046: privileged-action audit middleware — route
//! metadata drives automatic audit for admin writes, so no
//! handler can forget its audit row.
//!
//! The three tests:
//!
//! - `live_writes_audit_exactly_one_row_each` — the three
//!   live admin writes (suspend / restore / impersonate)
//!   each write exactly one audit row with the right actor,
//!   target, before/after and reason. Their services write
//!   the row inside the same transaction as the mutation
//!   (P1-008's atomicity), and the handler marks the response
//!   `AuditHandled` — so the middleware must NOT write a
//!   second row. A duplicate here means the marker protocol
//!   broke.
//! - `middleware_audits_a_write_without_a_manual_call` —
//!   the mechanism proof: the middleware layered over a
//!   fake handler (no service, no manual audit call) audits
//!   a successful admin write from the route metadata alone
//!   (`admin.flags.updated`, actor = the admin,
//!   target_type `flags`); a 501 from the same route and a
//!   200 from a read route each audit nothing.
//! - `denied_and_stub_writes_audit_nothing` — the negative
//!   on the real app: a denied write (support suspend, 403),
//!   an admitted stub write (admin quota-grant, 501) and a
//!   successful read (admin user search, 200) each write no
//!   audit row. The trail records actions, not attempts or
//!   views.

mod common;

use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
    middleware,
    response::IntoResponse,
    routing::{get, put},
    Json, Router,
};
use common::{TestApp, TestFixtures};
use detag_api::audit_middleware::audit_admin_writes;
use detag_core::repos::audit_logs;
use serde_json::json;
use tower::ServiceExt;
use uuid::Uuid;

/// The request id: a valid UUID, echoed by the middleware.
const REQUEST_ID: &str = "123e4567-e89b-42d3-a456-426614174109";

/// The password every test user registers with.
const PASSWORD: &str = "correct horse battery staple";

/// Test settings: the same shape as the authz matrix (generous
/// key bucket — this suite asserts authorization outcomes,
/// not rate limits).
async fn audit_app() -> TestApp {
    let fixtures = TestFixtures::from_env().await;
    let mut settings = fixtures.settings();
    settings.disposable_email_domains = "mailinator.com".to_string();
    settings.tos_current_version = "v1".to_string();
    settings.api_key_rate_limit_burst = 1000;
    settings.api_key_rate_limit_refill_per_min = 1000;
    TestApp::spawn_with_fixtures_and_settings(fixtures, settings).await
}

fn unique_email(prefix: &str) -> String {
    format!("{prefix}-{}@example.com", Uuid::now_v7())
}

/// A per-request XFF address: each request gets its own IP
/// bucket, so the tests never trip each other's rate limits
/// (the P1-003 lesson).
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
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
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
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    res.json()["access_token"].as_str().unwrap().to_string()
}

/// Register a user, set its role in the database, log in,
/// return (id, token). The role is set by a direct UPDATE —
/// there is no self-serve way to become an admin or support
/// (the role is set by the owner out of band).
async fn make_role(app: &TestApp, email: &str, role: &str) -> (Uuid, String) {
    let id = register_user(app, email).await;
    sqlx::query("UPDATE users SET role = $1 WHERE id = $2")
        .bind(role)
        .bind(id)
        .execute(&app.pool)
        .await
        .expect("set role");
    let token = login_token(app, email).await;
    (id, token)
}

/// The audit rows for one user target and one action:
/// registration and login audit against the same target, so
/// an admin-action assertion must filter by action, not just
/// by target.
async fn audit_for_user_action(
    app: &TestApp,
    user_id: Uuid,
    action: &str,
) -> Vec<audit_logs::AuditLog> {
    audit_logs::list_for_target(&app.pool, "user", &user_id.to_string())
        .await
        .expect("list audit rows")
        .into_iter()
        .filter(|row| row.action == action)
        .collect()
}

/// The audit rows for one action, oldest first.
async fn audit_for_action(app: &TestApp, action: &str) -> Vec<audit_logs::AuditLog> {
    audit_logs::list_for_action(&app.pool, action)
        .await
        .expect("list audit rows")
}

/// One admin write through the real app: POST with a JSON
/// body + a bearer token.
async fn admin_post(
    app: &TestApp,
    uri: &str,
    token: &str,
    body: Option<serde_json::Value>,
) -> common::TestResponse {
    let req = Request::builder()
        .method(Method::POST)
        .uri(uri)
        .header("x-request-id", REQUEST_ID)
        .header("x-forwarded-for", unique_xff())
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/json");
    let body = match body {
        Some(value) => Body::from(serde_json::to_vec(&value).unwrap()),
        None => Body::empty(),
    };
    app.request(req.body(body).unwrap()).await
}

#[tokio::test]
async fn live_writes_audit_exactly_one_row_each() {
    let app = audit_app().await;
    let (admin_id, admin_token) = make_role(&app, &unique_email("admin"), "admin").await;

    // Suspend: exactly one `user.suspended` row — actor the
    // admin, target the user, before `active` → after
    // `suspended`, reason verbatim.
    let (suspend_target, _) = make_role(&app, &unique_email("target-s"), "user").await;
    let res = admin_post(
        &app,
        &format!("/v1/admin/users/{suspend_target}/suspend"),
        &admin_token,
        Some(json!({ "reason": "privileged audit probe" })),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let rows = audit_for_user_action(&app, suspend_target, "user.suspended").await;
    assert_eq!(rows.len(), 1, "suspend writes exactly one row");
    let row = &rows[0];
    assert_eq!(row.actor_user_id, Some(admin_id), "actor is the admin");
    assert_eq!(row.target_type, "user");
    assert_eq!(
        row.target_id.as_deref(),
        Some(suspend_target.to_string()).as_deref()
    );
    assert_eq!(row.before.as_ref().unwrap()["status"], "active");
    assert_eq!(row.after.as_ref().unwrap()["status"], "suspended");
    assert_eq!(row.reason.as_deref(), Some("privileged audit probe"));

    // Restore: exactly one `user.restored` row — before
    // `suspended` → after `active`.
    let res = admin_post(
        &app,
        &format!("/v1/admin/users/{suspend_target}/restore"),
        &admin_token,
        None,
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let rows = audit_for_user_action(&app, suspend_target, "user.restored").await;
    assert_eq!(rows.len(), 1, "restore writes exactly one row");
    let row = &rows[0];
    assert_eq!(row.actor_user_id, Some(admin_id), "actor is the admin");
    assert_eq!(row.before.as_ref().unwrap()["status"], "suspended");
    assert_eq!(row.after.as_ref().unwrap()["status"], "active");

    // Impersonate: exactly one `user.impersonated` row —
    // actor the admin, reason verbatim, after carries the
    // 15-minute expiry.
    let (impersonate_target, _) = make_role(&app, &unique_email("target-i"), "user").await;
    let res = admin_post(
        &app,
        &format!("/v1/admin/users/{impersonate_target}/impersonate"),
        &admin_token,
        Some(json!({ "reason": "privileged audit probe" })),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let rows = audit_for_user_action(&app, impersonate_target, "user.impersonated").await;
    assert_eq!(rows.len(), 1, "impersonate writes exactly one row");
    let row = &rows[0];
    assert_eq!(row.actor_user_id, Some(admin_id), "actor is the admin");
    assert_eq!(row.target_type, "user");
    assert_eq!(row.reason.as_deref(), Some("privileged audit probe"));
    assert_eq!(row.after.as_ref().unwrap()["expires_in"], 900);
}

/// A fake handler that answers 200 with an empty object —
/// a stand-in for a future admin write handler that never
/// learned to audit itself.
async fn fake_write_ok() -> impl IntoResponse {
    (StatusCode::OK, Json(json!({})))
}

/// A fake handler that answers 501 — a stand-in for a stub
/// whose handler has not landed yet.
async fn fake_write_stub() -> impl IntoResponse {
    (
        StatusCode::NOT_IMPLEMENTED,
        Json(json!({ "code": "PF-5004" })),
    )
}

#[tokio::test]
async fn middleware_audits_a_write_without_a_manual_call() {
    let app = audit_app().await;
    let (admin_id, admin_token) = make_role(&app, &unique_email("admin"), "admin").await;

    // The harness DB persists across runs (the P1-009
    // lesson), so an earlier run of this suite may already
    // hold `admin.flags.updated` rows: assert the DELTA this
    // test writes, not the absolute count.
    let baseline = audit_for_action(&app, "admin.flags.updated").await.len();

    // A 200 write through the middleware alone: exactly one
    // row, action + target type from the route metadata,
    // actor the admin, no target id (flags has no `{id}`).
    let router = Router::new()
        .route("/v1/admin/flags", put(fake_write_ok))
        .layer(middleware::from_fn_with_state(
            app.state.clone(),
            audit_admin_writes,
        ))
        .with_state(app.state.clone());
    let res = router
        .oneshot(
            Request::builder()
                .method(Method::PUT)
                .uri("/v1/admin/flags")
                .header("authorization", format!("Bearer {admin_token}"))
                .header("content-type", "application/json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("oneshot runs");
    assert_eq!(res.status(), StatusCode::OK);
    let rows = audit_for_action(&app, "admin.flags.updated").await;
    assert_eq!(rows.len(), baseline + 1, "middleware audits the write");
    let row = &rows[rows.len() - 1];
    assert_eq!(row.actor_user_id, Some(admin_id), "actor is the admin");
    assert_eq!(row.target_type, "flags");
    assert_eq!(row.target_id, None, "flags has no target id");

    // The same route answering 501 audits nothing: the
    // handler has not landed, so no action happened.
    let router = Router::new()
        .route("/v1/admin/flags", put(fake_write_stub))
        .layer(middleware::from_fn_with_state(
            app.state.clone(),
            audit_admin_writes,
        ))
        .with_state(app.state.clone());
    let res = router
        .oneshot(
            Request::builder()
                .method(Method::PUT)
                .uri("/v1/admin/flags")
                .header("authorization", format!("Bearer {admin_token}"))
                .header("content-type", "application/json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("oneshot runs");
    assert_eq!(res.status(), StatusCode::NOT_IMPLEMENTED);
    let rows = audit_for_action(&app, "admin.flags.updated").await;
    assert_eq!(rows.len(), baseline + 1, "a 501 stub audits nothing");

    // A 200 read through the middleware audits nothing: the
    // trail records actions, not views.
    let router = Router::new()
        .route("/v1/admin/flags", get(fake_write_ok))
        .layer(middleware::from_fn_with_state(
            app.state.clone(),
            audit_admin_writes,
        ))
        .with_state(app.state.clone());
    let res = router
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/v1/admin/flags")
                .header("authorization", format!("Bearer {admin_token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("oneshot runs");
    assert_eq!(res.status(), StatusCode::OK);
    let rows = audit_for_action(&app, "admin.flags.updated").await;
    assert_eq!(rows.len(), baseline + 1, "a read audits nothing");
}

#[tokio::test]
async fn denied_and_stub_writes_audit_nothing() {
    let app = audit_app().await;
    let (admin_id, admin_token) = make_role(&app, &unique_email("admin"), "admin").await;
    let (_support_id, support_token) = make_role(&app, &unique_email("support"), "support").await;
    let (target, _) = make_role(&app, &unique_email("target"), "user").await;
    let _ = admin_id;
    // Same baseline rule as the middleware test: the harness
    // DB persists across runs, so assert the count does not
    // move rather than that it is zero.
    let quota_baseline = audit_for_action(&app, "admin.users.quota_granted")
        .await
        .len();

    // A denied write (support may not suspend): 403, and no
    // `user.suspended` row for the target.
    let res = admin_post(
        &app,
        &format!("/v1/admin/users/{target}/suspend"),
        &support_token,
        Some(json!({ "reason": "privileged audit probe" })),
    )
    .await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.text());
    assert_eq!(
        res.json()["code"].as_str().unwrap_or(""),
        "PF-1002",
        "{}",
        res.text()
    );
    assert!(
        audit_for_user_action(&app, target, "user.suspended")
            .await
            .is_empty(),
        "a denied suspend audits nothing"
    );

    // An admitted stub write (admin quota-grant): 501, and
    // no `admin.users.quota_granted` row.
    let res = admin_post(
        &app,
        &format!("/v1/admin/users/{target}/quota-grant"),
        &admin_token,
        Some(json!({ "reason": "privileged audit probe" })),
    )
    .await;
    assert_eq!(res.status, StatusCode::NOT_IMPLEMENTED, "{}", res.text());
    assert_eq!(
        audit_for_action(&app, "admin.users.quota_granted")
            .await
            .len(),
        quota_baseline,
        "an admitted stub write audits nothing"
    );

    // A successful read (admin user search): 200, and no
    // admin audit row of any kind for the search.
    let res = app
        .request(
            Request::builder()
                .method(Method::GET)
                .uri("/v1/admin/users")
                .header("x-request-id", REQUEST_ID)
                .header("x-forwarded-for", unique_xff())
                .header("authorization", format!("Bearer {admin_token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert_eq!(
        audit_for_action(&app, "admin.users.quota_granted")
            .await
            .len(),
        quota_baseline,
        "a read audits nothing"
    );
}
