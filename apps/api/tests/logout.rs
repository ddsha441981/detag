//! P1-017: logout endpoints, end to end.
//!
//! Through the real router (`TestApp` oneshot) against the
//! migrated scratch database:
//!
//! - `POST /v1/auth/logout` revokes the session behind the
//!   `detag_refresh` cookie (the presented session), audits
//!   `session.logged_out`, and clears the cookie (the
//!   `Set-Cookie` comes back with `Max-Age=0`). A second
//!   logout of the same session is a no-op 204 (no second
//!   audit). No cookie, or an unknown token → 401 `PF-1001`.
//! - `POST /v1/auth/logout-all` revokes EVERY session of the
//!   user (the Bearer access token identifies the user, not
//!   the session), audits `session.logged_out_all` with the
//!   count, and clears the cookie. A cookie-only caller gets
//!   401 `PF-1001` (the extractor rejects a bare cookie —
//!   P1-016): the web client sends both, an API client sends
//!   the Bearer.
//! - A revoked refresh token cannot rotate (proven here —
//!   the refresh service answers the generic 401 `PF-1001`
//!   for a revoked row, P1-014). The access token already
//!   issued stays valid until it expires (≤ 15 min — the
//!   documented residual window, asserted, not hidden).

mod common;

use axum::http::{Method, Request, StatusCode};
use common::{TestApp, TestFixtures};
use detag_core::repos::{audit_logs, sessions};
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

/// POST /v1/auth/logout with the given cookie value (or none).
fn logout_req(cookie: Option<&str>) -> Request<axum::body::Body> {
    let mut req = Request::builder()
        .method(Method::POST)
        .uri("/v1/auth/logout")
        .header("x-request-id", REQUEST_ID)
        .header("x-forwarded-for", unique_xff());
    if let Some(token) = cookie {
        req = req.header("cookie", format!("detag_refresh={token}"));
    }
    req.body(axum::body::Body::empty()).unwrap()
}

/// POST /v1/auth/logout-all with the given Bearer token (or
/// cookie, or nothing).
fn logout_all_req(bearer: Option<&str>, cookie: Option<&str>) -> Request<axum::body::Body> {
    let mut req = Request::builder()
        .method(Method::POST)
        .uri("/v1/auth/logout-all")
        .header("x-request-id", REQUEST_ID)
        .header("x-forwarded-for", unique_xff());
    if let Some(token) = bearer {
        req = req.header("authorization", format!("Bearer {token}"));
    }
    if let Some(token) = cookie {
        req = req.header("cookie", format!("detag_refresh={token}"));
    }
    req.body(axum::body::Body::empty()).unwrap()
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

/// The `Set-Cookie` header of a response, if any.
fn set_cookie(res: &common::TestResponse) -> Option<String> {
    res.headers
        .get("set-cookie")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
}

#[tokio::test]
async fn logout_revokes_current_session_and_clears_the_cookie() {
    let app = app().await;
    let email = unique_email("user");
    let user_id = register_user(&app, &email).await;
    let (access_a, refresh_a) = login_tokens(&app, &email).await;

    let res = app.request(logout_req(Some(&refresh_a))).await;
    assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.text());
    let cleared = set_cookie(&res).expect("logout clears the cookie");
    assert!(cleared.starts_with("detag_refresh=;"), "{cleared}");
    assert!(cleared.contains("Max-Age=0"), "{cleared}");

    // The session row is revoked.
    let rows = sessions::list_for_user(&app.state.pool, user_id)
        .await
        .expect("list sessions");
    assert_eq!(rows.len(), 1);
    assert!(rows[0].revoked_at.is_some());

    // The logout is audited.
    let audits = audits_for(&app, user_id, "session.logged_out").await;
    assert_eq!(audits.len(), 1);
    assert_eq!(audits[0].actor_user_id, Some(user_id));
    assert_eq!(
        audits[0].after.as_ref().unwrap()["session_id"],
        rows[0].id.to_string()
    );

    // A revoked refresh token cannot rotate.
    let res = app.request(refresh_req(&refresh_a)).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-1001", "{}", res.text());

    // The access token already issued stays valid until it
    // expires (≤ 15 min — the documented residual window:
    // logout revokes the SESSION, not the stateless token).
    let res = app
        .request(
            Request::builder()
                .method(Method::GET)
                .uri("/v1/me")
                .header("x-request-id", REQUEST_ID)
                .header("x-forwarded-for", unique_xff())
                .header("authorization", format!("Bearer {access_a}"))
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());

    // Another session of the same user is unaffected.
    let (_access_b, refresh_b) = login_tokens(&app, &email).await;
    let res = app.request(refresh_req(&refresh_b)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
}

#[tokio::test]
async fn logout_is_idempotent() {
    let app = app().await;
    let email = unique_email("user");
    let user_id = register_user(&app, &email).await;
    let (_access, refresh) = login_tokens(&app, &email).await;

    let res = app.request(logout_req(Some(&refresh))).await;
    assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.text());
    // Second logout of the same session: still 204, no
    // second audit (P1-008's idempotent-no-op convention).
    let res = app.request(logout_req(Some(&refresh))).await;
    assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.text());
    assert_eq!(
        audits_for(&app, user_id, "session.logged_out").await.len(),
        1
    );
}

#[tokio::test]
async fn logout_without_cookie_is_401() {
    let app = app().await;
    register_user(&app, &unique_email("user")).await;

    let res = app.request(logout_req(None)).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-1001", "{}", res.text());
}

#[tokio::test]
async fn logout_with_unknown_token_is_401() {
    let app = app().await;
    register_user(&app, &unique_email("user")).await;

    let res = app.request(logout_req(Some("never-existed-token"))).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-1001", "{}", res.text());
}

#[tokio::test]
async fn logout_all_revokes_every_session() {
    let app = app().await;
    let email = unique_email("user");
    let user_id = register_user(&app, &email).await;
    let (_access_a, refresh_a) = login_tokens(&app, &email).await;
    let (_access_b, refresh_b) = login_tokens(&app, &email).await;
    // A third session in A's family (rotation): logout-all
    // must kill it too.
    let res = app.request(refresh_req(&refresh_a)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let refresh_c = res
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

    // Logout-all with B's access token (the Bearer identifies
    // the user, not the session).
    let res = app.request(logout_all_req(Some(&_access_b), None)).await;
    assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.text());
    let cleared = set_cookie(&res).expect("logout-all clears the cookie");
    assert!(cleared.contains("Max-Age=0"), "{cleared}");

    // Every session row is revoked (3: A replaced, B, C).
    let rows = sessions::list_for_user(&app.state.pool, user_id)
        .await
        .expect("list sessions");
    assert_eq!(rows.len(), 3);
    assert!(
        rows.iter().all(|row| row.revoked_at.is_some()),
        "every session must be revoked: {rows:?}"
    );

    // The sweep is audited once, with the count of sessions
    // THIS call revoked: 2 (B + C — A was already revoked by
    // the rotation that minted C, so the sweep does not count
    // it twice; `revoke_all_for_user` only touches rows whose
    // `revoked_at` is still NULL).
    let audits = audits_for(&app, user_id, "session.logged_out_all").await;
    assert_eq!(audits.len(), 1);
    assert_eq!(audits[0].actor_user_id, Some(user_id));
    assert_eq!(audits[0].after.as_ref().unwrap()["sessions_revoked"], 2);

    // No refresh token rotates any more — with one
    // distinction the design makes on purpose (P1-014): A
    // is a REPLACED token, so presenting it is still a reuse
    // detection (401 PF-1007 — the row still carries
    // `replaced_by`, and the reuse path runs again); B and C
    // are merely revoked, so they are the generic 401
    // PF-1001. Logout-all revoked the family, but it did not
    // erase the rotation history.
    let res = app.request(refresh_req(&refresh_a)).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-1007", "{}", res.text());
    for token in [&refresh_b, &refresh_c] {
        let res = app.request(refresh_req(token)).await;
        assert_eq!(
            res.status,
            StatusCode::UNAUTHORIZED,
            "{token}: {}",
            res.text()
        );
        assert_eq!(res.json()["code"], "PF-1001", "{token}: {}", res.text());
    }
}

#[tokio::test]
async fn logout_all_with_cookie_only_is_401() {
    let app = app().await;
    let email = unique_email("user");
    register_user(&app, &email).await;
    let (_access, refresh) = login_tokens(&app, &email).await;

    // The extractor rejects a bare cookie (P1-016) — a
    // cookie-only caller uses POST /v1/auth/logout for the
    // single session; logout-all needs the Bearer token.
    let res = app.request(logout_all_req(None, Some(&refresh))).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-1001", "{}", res.text());
}

#[tokio::test]
async fn logout_all_without_credential_is_401() {
    let app = app().await;
    register_user(&app, &unique_email("user")).await;

    let res = app.request(logout_all_req(None, None)).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-1001", "{}", res.text());
}
