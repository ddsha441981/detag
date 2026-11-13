//! P1-010: admin impersonation, end to end.
//!
//! Through the real router (`TestApp` oneshot) against the
//! migrated scratch database: an admin impersonates a user
//! (200, a 15-minute scoped token whose claims carry
//! `impersonator_id`, `user.impersonated` audited with the
//! reason), the token acts as the target user on user routes
//! (`GET /v1/me` returns the target), and it cannot reach any
//! admin endpoint (403 `PF-1002` — the guards reject any
//! context carrying `impersonator_id`). An expired
//! impersonation token is 401 `PF-1011` (auto-expiry is the
//! JWT `exp`, enforced by the extractor with leeway 0).
//! Support may not impersonate (it is a write).

mod common;

use axum::http::{Method, Request, StatusCode};
use common::{TestApp, TestFixtures};
use detag_core::{
    jwt::{issue_impersonation_token, verify_access_token},
    repos::audit_logs,
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
/// an admin or support (the role is set by the owner out of
/// band; the invite CLI is a DECISIONS.md item).
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

fn impersonate_req(
    token: Option<&str>,
    user_id: Uuid,
    body: serde_json::Value,
) -> Request<axum::body::Body> {
    let mut req = Request::builder()
        .method(Method::POST)
        .uri(format!("/v1/admin/users/{user_id}/impersonate"))
        .header("x-request-id", REQUEST_ID)
        .header("x-forwarded-for", unique_xff())
        .header("content-type", "application/json");
    if let Some(token) = token {
        req = req.header("authorization", format!("Bearer {token}"));
    }
    req.body(axum::body::Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap()
}

fn authed_get(token: &str, path: &str) -> Request<axum::body::Body> {
    Request::builder()
        .method(Method::GET)
        .uri(path)
        .header("x-request-id", REQUEST_ID)
        .header("x-forwarded-for", unique_xff())
        .header("authorization", format!("Bearer {token}"))
        .body(axum::body::Body::empty())
        .unwrap()
}

/// The `user.impersonated` audit rows for a user.
async fn impersonated_audits(
    app: &TestApp,
    user_id: Uuid,
) -> Vec<detag_core::repos::audit_logs::AuditLog> {
    audit_logs::list_for_target(&app.state.pool, "user", &user_id.to_string())
        .await
        .expect("list audits")
        .into_iter()
        .filter(|row| row.action == "user.impersonated")
        .collect()
}

#[tokio::test]
async fn impersonate_happy_path_mints_scoped_token() {
    let app = admin_app().await;
    let (admin_id, admin_token) = make_role(&app, &unique_email("admin"), "admin").await;
    let user_id = register_user(&app, &unique_email("user")).await;

    let res = app
        .request(impersonate_req(
            Some(&admin_token),
            user_id,
            json!({ "reason": "support request #42" }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let body = res.json();
    assert_eq!(body["token_type"], "Bearer", "{body}");
    assert_eq!(body["expires_in"], 900, "{body}");
    assert_eq!(body["impersonator_id"], admin_id.to_string(), "{body}");
    assert_eq!(body["user_id"], user_id.to_string(), "{body}");
    let token = body["access_token"]
        .as_str()
        .expect("access token")
        .to_string();
    assert_eq!(token.split('.').count(), 3, "a JWT has three segments");

    // The token's claims: sub + role are the target's, the
    // impersonator is the admin, and it lives exactly 15 min.
    let claims = verify_access_token(&app.settings.jwt_secret, &token).expect("token verifies");
    assert_eq!(claims.sub, user_id.to_string());
    assert_eq!(claims.role, "user");
    assert_eq!(claims.impersonator_id, Some(admin_id));
    assert_eq!(claims.exp - claims.iat, 900);

    // The impersonation is audited: actor = the admin, target
    // = the user, the reason verbatim, expires_in in `after`.
    let audits = impersonated_audits(&app, user_id).await;
    assert_eq!(audits.len(), 1, "exactly one impersonation audit");
    assert_eq!(audits[0].actor_user_id, Some(admin_id));
    assert_eq!(audits[0].reason.as_deref(), Some("support request #42"));
    assert_eq!(audits[0].after.as_ref().unwrap()["expires_in"], 900);
}

#[tokio::test]
async fn impersonation_token_acts_as_the_user() {
    let app = admin_app().await;
    let (_admin_id, admin_token) = make_role(&app, &unique_email("admin"), "admin").await;
    let user_email = unique_email("user");
    let user_id = register_user(&app, &user_email).await;

    let res = app
        .request(impersonate_req(
            Some(&admin_token),
            user_id,
            json!({ "reason": "debugging their report" }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let token = res.json()["access_token"].as_str().unwrap().to_string();

    // The token acts as the target: GET /v1/me returns the
    // target's profile, not the admin's.
    let res = app.request(authed_get(&token, "/v1/me")).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert_eq!(res.json()["id"], user_id.to_string(), "{}", res.text());
    assert_eq!(res.json()["email"], user_email, "{}", res.text());
}

#[tokio::test]
async fn impersonation_token_cannot_reach_admin_endpoints() {
    let app = admin_app().await;
    let (_admin_id, admin_token) = make_role(&app, &unique_email("admin"), "admin").await;
    let user_id = register_user(&app, &unique_email("user")).await;

    let res = app
        .request(impersonate_req(
            Some(&admin_token),
            user_id,
            json!({ "reason": "debugging" }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let token = res.json()["access_token"].as_str().unwrap().to_string();

    // A write admin route: 403 PF-1002.
    let res = app
        .request(
            Request::builder()
                .method(Method::POST)
                .uri(format!("/v1/admin/users/{user_id}/suspend"))
                .header("x-request-id", REQUEST_ID)
                .header("x-forwarded-for", unique_xff())
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {token}"))
                .body(axum::body::Body::from(
                    serde_json::to_vec(&json!({ "reason": "trying to escalate" })).unwrap(),
                ))
                .unwrap(),
        )
        .await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-1002", "{}", res.text());

    // A read admin route: 403 PF-1002 too (reads included).
    let res = app.request(authed_get(&token, "/v1/admin/users")).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-1002", "{}", res.text());
}

#[tokio::test]
async fn impersonate_support_and_plain_user_forbidden() {
    let app = admin_app().await;
    let (_support_id, support_token) = make_role(&app, &unique_email("support"), "support").await;
    let user_email = unique_email("plainuser");
    let user_id = register_user(&app, &user_email).await;
    let user_token = login_token(&app, &user_email).await;

    // Support may not impersonate: it is a write.
    let res = app
        .request(impersonate_req(
            Some(&support_token),
            user_id,
            json!({ "reason": "support wants to look" }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-1002", "{}", res.text());

    // A plain user may not impersonate either.
    let res = app
        .request(impersonate_req(
            Some(&user_token),
            user_id,
            json!({ "reason": "me" }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-1002", "{}", res.text());

    // No credential: 401.
    let res = app
        .request(impersonate_req(None, user_id, json!({ "reason": "x" })))
        .await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-1001", "{}", res.text());

    // Nothing was audited for the rejections.
    assert_eq!(impersonated_audits(&app, user_id).await.len(), 0);
}

#[tokio::test]
async fn impersonate_unknown_user_not_found() {
    let app = admin_app().await;
    let (_admin_id, admin_token) = make_role(&app, &unique_email("admin"), "admin").await;

    let res = app
        .request(impersonate_req(
            Some(&admin_token),
            Uuid::now_v7(),
            json!({ "reason": "ghost" }),
        ))
        .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-2010", "{}", res.text());
}

#[tokio::test]
async fn impersonate_missing_or_blank_reason_is_422() {
    let app = admin_app().await;
    let (_admin_id, admin_token) = make_role(&app, &unique_email("admin"), "admin").await;
    let user_id = register_user(&app, &unique_email("user")).await;

    let res = app
        .request(impersonate_req(Some(&admin_token), user_id, json!({})))
        .await;
    assert_eq!(
        res.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        res.text()
    );
    assert_eq!(res.json()["code"], "PF-2002", "{}", res.text());

    let res = app
        .request(impersonate_req(
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
}

#[tokio::test]
async fn impersonate_malformed_uuid_is_422() {
    let app = admin_app().await;
    let (_admin_id, admin_token) = make_role(&app, &unique_email("admin"), "admin").await;

    let res = app
        .request(
            Request::builder()
                .method(Method::POST)
                .uri("/v1/admin/users/not-a-uuid/impersonate")
                .header("x-request-id", REQUEST_ID)
                .header("x-forwarded-for", unique_xff())
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {admin_token}"))
                .body(axum::body::Body::from(
                    serde_json::to_vec(&json!({ "reason": "x" })).unwrap(),
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
async fn expired_impersonation_token_is_rejected() {
    let app = admin_app().await;
    let (admin_id, _admin_token) = make_role(&app, &unique_email("admin"), "admin").await;
    let user_id = register_user(&app, &unique_email("user")).await;

    // A token minted 60 s in the past: the extractor rejects
    // it with 401 PF-1011 (leeway 0 — auto-expiry is the JWT
    // `exp`, and the service only ever issues 15-minute
    // tokens, so this is the expiry proof without waiting).
    let expired =
        issue_impersonation_token(&app.settings.jwt_secret, user_id, "user", admin_id, -60)
            .expect("issue succeeds");
    let res = app.request(authed_get(&expired, "/v1/me")).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-1011", "{}", res.text());
}
