//! P1-014: refresh rotation, end to end.
//!
//! Through the real router (`TestApp` oneshot) against the
//! migrated scratch database: presenting a live refresh
//! token (in the `detag_refresh` cookie) mints a new access
//! token + a new refresh token in the SAME family and marks
//! the old row replaced (`revoked_at` + `replaced_by`);
//! presenting an already-replaced token is a reuse attack —
//! 401 `PF-1007`, the whole family revoked, a
//! `session.refresh_reused` audit, and the security email
//! queued. Unknown / revoked / expired tokens are the
//! generic 401 `PF-1001` (the hash lookup must not
//! distinguish "never existed" from "dead").

mod common;

use axum::http::{Method, Request, StatusCode};
use chrono::{Duration, Utc};
use common::{TestApp, TestFixtures};
use detag_core::{
    checksum::sha256_hex,
    jwt::verify_access_token,
    repos::{
        audit_logs, email_outbox,
        sessions::{self, NewSession},
    },
    tokens::generate_token,
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

/// Log in, return the raw response (the tests read the
/// `set-cookie` header off it).
async fn login_raw(app: &TestApp, email: &str) -> common::TestResponse {
    let body = json!({
        "email": email,
        "password": PASSWORD,
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

/// Log in, return (access token, refresh token).
async fn login_tokens(app: &TestApp, email: &str) -> (String, String) {
    let res = login_raw(app, email).await;
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

/// POST /v1/auth/refresh with the given cookie value (or none).
fn refresh_req(cookie: Option<&str>) -> Request<axum::body::Body> {
    let mut req = Request::builder()
        .method(Method::POST)
        .uri("/v1/auth/refresh")
        .header("x-request-id", REQUEST_ID)
        .header("x-forwarded-for", unique_xff());
    if let Some(token) = cookie {
        req = req.header("cookie", format!("detag_refresh={token}"));
    }
    req.body(axum::body::Body::empty()).unwrap()
}

/// Register a user, set the role in the database, log in,
/// return (id, token). There is no self-serve way to become
/// an admin (the role is set by the owner out of band; the
/// invite CLI is a DECISIONS.md item).
async fn make_admin(app: &TestApp, email: &str) -> (Uuid, String) {
    let id = register_user(app, email).await;
    sqlx::query("UPDATE users SET role = 'admin' WHERE id = $1")
        .bind(id)
        .execute(&app.pool)
        .await
        .expect("promote to admin");
    let body = json!({ "email": email, "password": PASSWORD });
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
    (id, res.json()["access_token"].as_str().unwrap().to_string())
}

/// The `session.refresh_reused` audit rows for a user.
async fn reused_audits(
    app: &TestApp,
    user_id: Uuid,
) -> Vec<detag_core::repos::audit_logs::AuditLog> {
    audit_logs::list_for_target(&app.state.pool, "user", &user_id.to_string())
        .await
        .expect("list audits")
        .into_iter()
        .filter(|row| row.action == "session.refresh_reused")
        .collect()
}

/// The `session.refreshed` audit rows for a user.
async fn refreshed_audits(
    app: &TestApp,
    user_id: Uuid,
) -> Vec<detag_core::repos::audit_logs::AuditLog> {
    audit_logs::list_for_target(&app.state.pool, "user", &user_id.to_string())
        .await
        .expect("list audits")
        .into_iter()
        .filter(|row| row.action == "session.refreshed")
        .collect()
}

#[tokio::test]
async fn refresh_happy_path_rotates_in_the_same_family() {
    let app = admin_app().await;
    let user_email = unique_email("user");
    let user_id = register_user(&app, &user_email).await;
    let (_access, refresh_a) = login_tokens(&app, &user_email).await;

    let res = app.request(refresh_req(Some(&refresh_a))).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let body = res.json();
    assert_eq!(body["token_type"], "Bearer", "{body}");
    assert_eq!(body["expires_in"], 900, "{body}");
    let refresh_b = refresh_token_of(&res);
    assert_ne!(refresh_a, refresh_b, "the refresh token must change");
    let access_b = body["access_token"].as_str().unwrap().to_string();
    let claims =
        verify_access_token(&app.settings.jwt_secret, &access_b).expect("new token verifies");
    assert_eq!(claims.sub, user_id.to_string());

    // Two rows, one family: the old row is replaced (both
    // marks), the new row is live.
    let rows = sessions::list_for_user(&app.state.pool, user_id)
        .await
        .expect("list sessions");
    assert_eq!(rows.len(), 2, "login + one rotation");
    let (old, new) = (&rows[1], &rows[0]);
    assert_eq!(old.family_id, new.family_id, "rotation stays in the family");
    assert!(old.revoked_at.is_some(), "old row is revoked");
    assert_eq!(
        old.replaced_by,
        Some(new.id),
        "old row points at the new row"
    );
    assert!(new.revoked_at.is_none(), "new row is live");
    assert!(new.replaced_by.is_none(), "new row is not replaced");

    // The rotation is audited.
    let audits = refreshed_audits(&app, user_id).await;
    assert_eq!(audits.len(), 1);
    assert_eq!(audits[0].actor_user_id, Some(user_id));
    assert_eq!(
        audits[0].after.as_ref().unwrap()["family_id"],
        old.family_id.to_string()
    );

    // The new access token works.
    let res = app
        .request(
            Request::builder()
                .method(Method::GET)
                .uri("/v1/me")
                .header("x-request-id", REQUEST_ID)
                .header("x-forwarded-for", unique_xff())
                .header("authorization", format!("Bearer {access_b}"))
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
}

#[tokio::test]
async fn refresh_chains_across_rotations() {
    let app = admin_app().await;
    let user_email = unique_email("user");
    let user_id = register_user(&app, &user_email).await;
    let (_access, refresh_a) = login_tokens(&app, &user_email).await;

    let res = app.request(refresh_req(Some(&refresh_a))).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let refresh_b = refresh_token_of(&res);

    let res = app.request(refresh_req(Some(&refresh_b))).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let refresh_c = refresh_token_of(&res);
    assert_ne!(refresh_b, refresh_c);

    // Three rows, one family, a chain: A → B → C.
    let rows = sessions::list_for_user(&app.state.pool, user_id)
        .await
        .expect("list sessions");
    assert_eq!(rows.len(), 3);
    assert!(rows.iter().all(|row| row.family_id == rows[0].family_id));
    let (a, b, c) = (&rows[2], &rows[1], &rows[0]);
    assert_eq!(a.replaced_by, Some(b.id));
    assert_eq!(b.replaced_by, Some(c.id));
    assert!(c.replaced_by.is_none());
    assert_eq!(refreshed_audits(&app, user_id).await.len(), 2);
}

#[tokio::test]
async fn reuse_attack_revokes_the_whole_family() {
    let app = admin_app().await;
    let user_email = unique_email("user");
    let user_id = register_user(&app, &user_email).await;
    let (_access, refresh_a) = login_tokens(&app, &user_email).await;

    // Rotate once: A is now replaced by B.
    let res = app.request(refresh_req(Some(&refresh_a))).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let refresh_b = refresh_token_of(&res);

    // Present A again: reuse — 401 PF-1007.
    let res = app.request(refresh_req(Some(&refresh_a))).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-1007", "{}", res.text());

    // The whole family is revoked — including B, the live row.
    let rows = sessions::list_for_user(&app.state.pool, user_id)
        .await
        .expect("list sessions");
    assert_eq!(rows.len(), 2);
    assert!(
        rows.iter().all(|row| row.revoked_at.is_some()),
        "every session in the family must be revoked: {rows:?}"
    );

    // The reuse is audited + the security email is queued.
    let audits = reused_audits(&app, user_id).await;
    assert_eq!(audits.len(), 1, "exactly one reuse audit");
    assert_eq!(audits[0].actor_user_id, Some(user_id));
    assert_eq!(audits[0].after.as_ref().unwrap()["sessions_revoked"], 1);
    let mails = email_outbox::unsent_for_template(&app.state.pool, "refresh-reuse-alert")
        .await
        .expect("list outbox");
    assert!(
        mails
            .iter()
            .any(|mail| mail.to_hash.as_deref() == Some(sha256_hex(user_email.as_bytes()).as_str())),
        "the security email must be queued for the user: {mails:?}"
    );

    // B is dead too now — revoked, not replaced, so it is the
    // generic 401, not a second PF-1007.
    let res = app.request(refresh_req(Some(&refresh_b))).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-1001", "{}", res.text());
    // No second reuse audit (B was revoked, not rotated).
    assert_eq!(reused_audits(&app, user_id).await.len(), 1);
}

#[tokio::test]
async fn unknown_refresh_token_is_401() {
    let app = admin_app().await;
    let user_email = unique_email("user");
    let user_id = register_user(&app, &user_email).await;

    let res = app.request(refresh_req(Some("never-existed-token"))).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-1001", "{}", res.text());
    assert_eq!(reused_audits(&app, user_id).await.len(), 0);
    assert_eq!(refreshed_audits(&app, user_id).await.len(), 0);
}

#[tokio::test]
async fn missing_cookie_is_401() {
    let app = admin_app().await;
    register_user(&app, &unique_email("user")).await;

    let res = app.request(refresh_req(None)).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-1001", "{}", res.text());
}

#[tokio::test]
async fn expired_session_is_401() {
    let app = admin_app().await;
    let user_email = unique_email("user");
    let user_id = register_user(&app, &user_email).await;

    // A session whose 30 days are already over, seeded
    // directly (no login can produce one).
    let raw = generate_token();
    sessions::create(
        &app.state.pool,
        &NewSession {
            user_id,
            refresh_hash: sha256_hex(raw.as_bytes()),
            family_id: Uuid::now_v7(),
            device: json!({ "ua": "test" }),
            expires_at: Utc::now() - Duration::hours(1),
        },
    )
    .await
    .expect("seed expired session");

    let res = app.request(refresh_req(Some(&raw))).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-1001", "{}", res.text());
    assert_eq!(reused_audits(&app, user_id).await.len(), 0);
}

#[tokio::test]
async fn revoked_session_is_401_not_1007() {
    let app = admin_app().await;
    let (_admin_id, admin_token) = make_admin(&app, &unique_email("admin")).await;
    let user_email = unique_email("user");
    let user_id = register_user(&app, &user_email).await;
    let (_access, refresh) = login_tokens(&app, &user_email).await;

    // Suspend the user: every session is revoked (but none
    // is replaced — revocation is not rotation).
    let res = app
        .request(
            Request::builder()
                .method(Method::POST)
                .uri(format!("/v1/admin/users/{user_id}/suspend"))
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
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());

    let res = app.request(refresh_req(Some(&refresh))).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-1001", "{}", res.text());
    assert_eq!(reused_audits(&app, user_id).await.len(), 0);
}

#[tokio::test]
async fn impersonation_token_cannot_refresh() {
    let app = admin_app().await;
    let (_admin_id, admin_token) = make_admin(&app, &unique_email("admin")).await;
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
                    serde_json::to_vec(&json!({ "reason": "support" })).unwrap(),
                ))
                .unwrap(),
        )
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let impersonation_token = res.json()["access_token"].as_str().unwrap().to_string();

    // An impersonation token is an access token, not a refresh
    // token: there is no session row behind it, so the hash
    // lookup fails with the generic 401 (P1-010's promise).
    let res = app.request(refresh_req(Some(&impersonation_token))).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-1001", "{}", res.text());
}
