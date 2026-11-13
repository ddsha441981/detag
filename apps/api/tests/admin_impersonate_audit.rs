//! P1-011: impersonation audit tests, end to end.
//!
//! Through the real router (`TestApp` oneshot) against the
//! migrated scratch database: every action taken under
//! impersonation is audited with BOTH ids — the audit row's
//! `actor_user_id` is the impersonator (the human who acted)
//! and its `target_id` is the user acted-as. The actions that
//! exist today are the profile update and the deletion
//! request; the `user.impersonated` row itself is checked
//! field by field. The admin block is proven on every live
//! admin route (not just the two P1-010 covered), and expiry
//! is enforced before the guards run (an expired token is 401
//! `PF-1011`, not 403).

mod common;

use axum::http::{Method, Request, StatusCode};
use common::{TestApp, TestFixtures};
use detag_core::{
    jwt::issue_impersonation_token,
    repos::{audit_logs, users},
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

/// Register a user, set the role in the database, log in,
/// return (id, token). There is no self-serve way to become
/// an admin (the role is set by the owner out of band; the
/// invite CLI is a DECISIONS.md item).
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

/// Impersonate a user, return the scoped token.
async fn impersonate_token(app: &TestApp, admin_token: &str, user_id: Uuid) -> String {
    let res = app
        .request(
            Request::builder()
                .method(Method::POST)
                .uri(format!("/v1/admin/users/{user_id}/impersonate"))
                .header("x-request-id", REQUEST_ID)
                .header("x-forwarded-for", unique_xff())
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {admin_token}"))
                .body(axum::body::Body::from(
                    serde_json::to_vec(&json!({ "reason": "support case" })).unwrap(),
                ))
                .unwrap(),
        )
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    res.json()["access_token"].as_str().unwrap().to_string()
}

fn authed_req(
    token: &str,
    method: Method,
    path: String,
    body: Option<serde_json::Value>,
) -> Request<axum::body::Body> {
    let mut req = Request::builder()
        .method(method)
        .uri(path)
        .header("x-request-id", REQUEST_ID)
        .header("x-forwarded-for", unique_xff())
        .header("authorization", format!("Bearer {token}"));
    if let Some(body) = body {
        req = req.header("content-type", "application/json");
        return req
            .body(axum::body::Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap();
    }
    req.body(axum::body::Body::empty()).unwrap()
}

/// The audit rows for a user with the given action.
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

#[tokio::test]
async fn impersonated_profile_update_audits_both_ids() {
    let app = admin_app().await;
    let (admin_id, admin_token) = make_role(&app, &unique_email("admin"), "admin").await;
    let user_id = register_user(&app, &unique_email("user")).await;
    let token = impersonate_token(&app, &admin_token, user_id).await;

    // The impersonator sets the target's display name — the
    // action works as the user.
    let res = app
        .request(authed_req(
            &token,
            Method::PATCH,
            "/v1/me".to_string(),
            Some(json!({ "display_name": "Impersonated Name" })),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert_eq!(
        res.json()["display_name"],
        "Impersonated Name",
        "{}",
        res.text()
    );

    // The audit row carries BOTH ids: the actor is the admin
    // (the human who acted), the target is the user acted-as.
    let audits = audits_for(&app, user_id, "profile.update").await;
    assert_eq!(audits.len(), 1, "exactly one profile.update audit");
    assert_eq!(
        audits[0].actor_user_id,
        Some(admin_id),
        "actor must be the impersonator"
    );
    assert_eq!(
        audits[0].target_id.as_deref(),
        Some(user_id.to_string()).as_deref()
    );
    assert_eq!(audits[0].target_type, "user");
    assert_eq!(
        audits[0].after.as_ref().unwrap()["display_name"],
        "Impersonated Name"
    );
}

#[tokio::test]
async fn normal_profile_update_audits_the_user_themself() {
    let app = admin_app().await;
    let user_email = unique_email("user");
    let user_id = register_user(&app, &user_email).await;
    let user_token = login_token(&app, &user_email).await;

    let res = app
        .request(authed_req(
            &user_token,
            Method::PATCH,
            "/v1/me".to_string(),
            Some(json!({ "display_name": "My Own Name" })),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());

    // No impersonation: actor == target == the user.
    let audits = audits_for(&app, user_id, "profile.update").await;
    assert_eq!(audits.len(), 1);
    assert_eq!(audits[0].actor_user_id, Some(user_id));
    assert_eq!(
        audits[0].target_id.as_deref(),
        Some(user_id.to_string()).as_deref()
    );
}

#[tokio::test]
async fn impersonated_deletion_request_audits_both_ids() {
    let app = admin_app().await;
    let (admin_id, admin_token) = make_role(&app, &unique_email("admin"), "admin").await;
    let user_id = register_user(&app, &unique_email("user")).await;
    let token = impersonate_token(&app, &admin_token, user_id).await;

    let res = app
        .request(authed_req(
            &token,
            Method::DELETE,
            "/v1/me".to_string(),
            None,
        ))
        .await;
    assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.text());

    // The account is deletion-pending (the impersonator CAN
    // request deletion — it is the admin's explicit, audited
    // action), and the audit names both ids.
    let user = users::by_id(&app.state.pool, user_id)
        .await
        .expect("load user")
        .expect("user exists");
    assert_eq!(user.status, "deletion_pending");
    let audits = audits_for(&app, user_id, "account.delete_requested").await;
    assert_eq!(audits.len(), 1);
    assert_eq!(
        audits[0].actor_user_id,
        Some(admin_id),
        "actor must be the impersonator"
    );
    assert_eq!(
        audits[0].target_id.as_deref(),
        Some(user_id.to_string()).as_deref()
    );
}

#[tokio::test]
async fn impersonation_audit_row_is_complete() {
    let app = admin_app().await;
    let (admin_id, admin_token) = make_role(&app, &unique_email("admin"), "admin").await;
    let user_id = register_user(&app, &unique_email("user")).await;

    let res = app
        .request(
            Request::builder()
                .method(Method::POST)
                .uri(format!("/v1/admin/users/{user_id}/impersonate"))
                .header("x-request-id", REQUEST_ID)
                .header("x-forwarded-for", unique_xff())
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {admin_token}"))
                .body(axum::body::Body::from(
                    serde_json::to_vec(&json!({ "reason": "fraud review" })).unwrap(),
                ))
                .unwrap(),
        )
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());

    let audits = audits_for(&app, user_id, "user.impersonated").await;
    assert_eq!(audits.len(), 1, "exactly one impersonation audit");
    let row = &audits[0];
    assert_eq!(row.action, "user.impersonated");
    assert_eq!(row.actor_user_id, Some(admin_id));
    assert_eq!(row.target_type, "user");
    assert_eq!(
        row.target_id.as_deref(),
        Some(user_id.to_string()).as_deref()
    );
    assert!(row.before.is_none(), "no before on a mint: {row:?}");
    assert_eq!(row.after.as_ref().unwrap()["expires_in"], 900);
    assert_eq!(row.reason.as_deref(), Some("fraud review"));
}

#[tokio::test]
async fn admin_endpoints_blocked_while_impersonating() {
    let app = admin_app().await;
    let (_admin_id, admin_token) = make_role(&app, &unique_email("admin"), "admin").await;
    let user_id = register_user(&app, &unique_email("user")).await;
    let other_id = register_user(&app, &unique_email("other")).await;
    let token = impersonate_token(&app, &admin_token, user_id).await;

    // Every live admin route rejects the impersonation token
    // with 403 PF-1002 — the guards, not the handlers, enforce
    // it. (P1-010 proved suspend + the user list; this proves
    // restore + impersonate, the remaining live admin routes.
    // The stub admin routes carry no guard yet — their tickets
    // add one with the real handler.)
    for (method, path) in [
        (Method::POST, format!("/v1/admin/users/{user_id}/restore")),
        (
            Method::POST,
            format!("/v1/admin/users/{other_id}/impersonate"),
        ),
    ] {
        let body = if path.ends_with("/impersonate") {
            Some(json!({ "reason": "trying to chain" }))
        } else {
            None
        };
        let res = app.request(authed_req(&token, method, path, body)).await;
        assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.text());
        assert_eq!(res.json()["code"], "PF-1002", "{}", res.text());
    }
    let res = app
        .request(authed_req(
            &token,
            Method::GET,
            "/v1/admin/users".to_string(),
            None,
        ))
        .await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-1002", "{}", res.text());
}

#[tokio::test]
async fn expired_impersonation_token_cannot_reach_admin_endpoints() {
    let app = admin_app().await;
    let (admin_id, _admin_token) = make_role(&app, &unique_email("admin"), "admin").await;
    let user_id = register_user(&app, &unique_email("user")).await;

    // Expired before the guards run: the extractor rejects
    // with 401 PF-1011, not 403 — expiry is checked first.
    let expired =
        issue_impersonation_token(&app.settings.jwt_secret, user_id, "user", admin_id, -60)
            .expect("issue succeeds");
    let res = app
        .request(authed_req(
            &expired,
            Method::POST,
            format!("/v1/admin/users/{user_id}/suspend"),
            Some(json!({ "reason": "x" })),
        ))
        .await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-1011", "{}", res.text());
    let res = app
        .request(authed_req(
            &expired,
            Method::GET,
            "/v1/admin/users".to_string(),
            None,
        ))
        .await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-1011", "{}", res.text());
}

#[tokio::test]
async fn impersonating_an_admin_target_is_still_blocked() {
    let app = admin_app().await;
    let (_admin_id, admin_token) = make_role(&app, &unique_email("admin"), "admin").await;
    let (target_admin_id, _target_token) =
        make_role(&app, &unique_email("targetadmin"), "admin").await;
    let token = impersonate_token(&app, &admin_token, target_admin_id).await;

    // The token's role is "admin" (the target's role), but the
    // block is on the impersonator marker, not the role: admin
    // endpoints still reject it.
    let res = app
        .request(authed_req(
            &token,
            Method::GET,
            "/v1/admin/users".to_string(),
            None,
        ))
        .await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-1002", "{}", res.text());

    // User routes still work as the target admin.
    let res = app
        .request(authed_req(&token, Method::GET, "/v1/me".to_string(), None))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert_eq!(
        res.json()["id"],
        target_admin_id.to_string(),
        "{}",
        res.text()
    );
}
