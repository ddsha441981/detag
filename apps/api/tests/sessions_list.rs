//! P1-019: session listing + single revocation, end to end.
//!
//! Through the real router (`TestApp` oneshot) against the
//! migrated scratch database:
//!
//! - `GET /v1/me/sessions` lists the user's LIVE sessions
//!   (not revoked, not expired), newest first, each with the
//!   parsed device label (`"Chrome on macOS"` — never the raw
//!   User-Agent), the ip hash, created, last_seen, expiry.
//!   There is NO "current" flag: the access token carries no
//!   session identity (P1-017's logout has the same
//!   limitation), so the handler cannot know which row the
//!   caller is.
//! - `DELETE /v1/me/sessions/{id}` revokes one session
//!   (audited `session.revoked`); unknown id, or an id
//!   belonging to another user → 404 `PF-2010` (no
//!   enumeration); already revoked → no-op 204.
//! - The AC: throttled `last_seen` — presenting a token
//!   advances the presented row's `last_seen` only when the
//!   stored value is older than the 5-minute throttle
//!   (proven at the row level: backdate 10 min → advances;
//!   backdate 1 min → does not).

mod common;

use axum::http::{Method, Request, StatusCode};
use chrono::{Duration, Utc};
use common::{TestApp, TestFixtures};
use detag_core::{
    checksum::sha256_hex,
    repos::{audit_logs, sessions},
};
use serde_json::{json, Value};
use uuid::Uuid;

/// The request id: a valid UUID, echoed by the middleware.
const REQUEST_ID: &str = "123e4567-e89b-42d3-a456-426614174103";

/// The password every test user registers with.
const PASSWORD: &str = "correct horse battery staple";

/// A Chrome-on-macOS User-Agent (the listing must parse it
/// to `"Chrome on macOS"`).
const CHROME_MAC_UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) \
    AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";
/// A Firefox-on-Linux User-Agent.
const FIREFOX_LINUX_UA: &str =
    "Mozilla/5.0 (X11; Linux x86_64; rv:121.0) Gecko/20100101 Firefox/121.0";

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

/// Log in with the given User-Agent, return (access token,
/// refresh token).
async fn login_tokens(app: &TestApp, email: &str, user_agent: &str) -> (String, String) {
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
                .header("user-agent", user_agent)
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

/// GET /v1/me/sessions with the given Bearer token.
fn list_req(access: &str) -> Request<axum::body::Body> {
    Request::builder()
        .method(Method::GET)
        .uri("/v1/me/sessions")
        .header("x-request-id", REQUEST_ID)
        .header("x-forwarded-for", unique_xff())
        .header("authorization", format!("Bearer {access}"))
        .body(axum::body::Body::empty())
        .unwrap()
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

/// The session row behind a refresh token (via the hash).
async fn session_for(app: &TestApp, refresh_token: &str) -> sessions::Session {
    sessions::by_refresh_hash(&app.state.pool, &sha256_hex(refresh_token.as_bytes()))
        .await
        .expect("lookup session")
        .expect("session exists")
}

/// Backdate a session row's `last_seen` by `minutes`
/// (like time passing — the login sets it to now).
async fn backdate_last_seen(app: &TestApp, session_id: Uuid, minutes: i64) {
    sqlx::query(
        "UPDATE sessions SET last_seen = now() - ($1 || ' minutes')::interval WHERE id = $2",
    )
    .bind(minutes.to_string())
    .bind(session_id)
    .execute(&app.state.pool)
    .await
    .expect("backdate last_seen");
}

#[tokio::test]
async fn list_shows_live_sessions_with_parsed_device() {
    let app = app().await;
    let email = unique_email("user");
    register_user(&app, &email).await;
    let (access_a, _refresh_a) = login_tokens(&app, &email, CHROME_MAC_UA).await;
    let (_access_b, _refresh_b) = login_tokens(&app, &email, FIREFOX_LINUX_UA).await;

    let res = app.request(list_req(&access_a)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let items: Vec<Value> = res.json().as_array().expect("list is an array").clone();
    assert_eq!(items.len(), 2, "{items:?}");
    // Newest first: the Firefox login (B) is first.
    assert_eq!(items[0]["device"], "Firefox on Linux", "{items:?}");
    assert_eq!(items[1]["device"], "Chrome on macOS", "{items:?}");
    for item in &items {
        // The raw User-Agent is never echoed back.
        assert!(
            !item["device"].as_str().unwrap().contains("Mozilla"),
            "{item:?}"
        );
        // The ip hash is present (a hash, not PII); created
        // and last_seen are timestamps; expiry is 30 days out.
        assert!(item["ip_hash"].as_str().is_some(), "{item:?}");
        assert!(item["created_at"].as_str().is_some(), "{item:?}");
        assert!(item["last_seen"].as_str().is_some(), "{item:?}");
        assert!(item["expires_at"].as_str().is_some(), "{item:?}");
        assert!(item["id"].as_str().is_some(), "{item:?}");
        // A fresh session's last_seen is its creation (the
        // login that minted it).
        assert_eq!(item["last_seen"], item["created_at"], "{item:?}");
    }
}

#[tokio::test]
async fn list_excludes_revoked_and_expired() {
    let app = app().await;
    let email = unique_email("user");
    let user_id = register_user(&app, &email).await;
    let (access_a, refresh_a) = login_tokens(&app, &email, CHROME_MAC_UA).await;
    let (_access_b, _refresh_b) = login_tokens(&app, &email, FIREFOX_LINUX_UA).await;

    // Revoke A's session (logout with its cookie).
    let res = app
        .request(
            Request::builder()
                .method(Method::POST)
                .uri("/v1/auth/logout")
                .header("x-request-id", REQUEST_ID)
                .header("x-forwarded-for", unique_xff())
                .header("cookie", format!("detag_refresh={refresh_a}"))
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.text());

    // An expired session, seeded directly (no login can
    // produce one).
    let raw = detag_core::tokens::generate_token();
    sessions::create(
        &app.state.pool,
        &sessions::NewSession {
            user_id,
            refresh_hash: sha256_hex(raw.as_bytes()),
            family_id: Uuid::now_v7(),
            device: json!({ "ua": "test" }),
            expires_at: Utc::now() - Duration::hours(1),
        },
    )
    .await
    .expect("seed expired session");

    let res = app.request(list_req(&access_a)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let items: Vec<Value> = res.json().as_array().expect("list is an array").clone();
    // Only B (live) is listed — A is revoked, the seeded row
    // is expired. (A's access token still works for the list
    // itself — the residual 15-minute window, P1-017.)
    assert_eq!(items.len(), 1, "{items:?}");
    assert_eq!(items[0]["device"], "Firefox on Linux", "{items:?}");
}

#[tokio::test]
async fn delete_revokes_one_session() {
    let app = app().await;
    let email = unique_email("user");
    let user_id = register_user(&app, &email).await;
    let (access_a, refresh_a) = login_tokens(&app, &email, CHROME_MAC_UA).await;
    let (_access_b, refresh_b) = login_tokens(&app, &email, FIREFOX_LINUX_UA).await;
    let session_b = session_for(&app, &refresh_b).await;

    let res = app
        .request(
            Request::builder()
                .method(Method::DELETE)
                .uri(format!("/v1/me/sessions/{}", session_b.id))
                .header("x-request-id", REQUEST_ID)
                .header("x-forwarded-for", unique_xff())
                .header("authorization", format!("Bearer {access_a}"))
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.text());

    // B's refresh token is dead; A's still rotates.
    let res = app.request(refresh_req(&refresh_b)).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-1001", "{}", res.text());
    let res = app.request(refresh_req(&refresh_a)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());

    // The revocation is audited.
    let audits = audit_logs::list_for_target(&app.state.pool, "user", &user_id.to_string())
        .await
        .expect("list audits");
    let revoked = audits
        .iter()
        .find(|row| row.action == "session.revoked")
        .expect("session.revoked audit");
    assert_eq!(revoked.actor_user_id, Some(user_id));
    assert_eq!(
        revoked.after.as_ref().unwrap()["session_id"],
        session_b.id.to_string()
    );

    // Deleting the same session again is a no-op 204 (no
    // second audit — P1-008's convention).
    let res = app
        .request(
            Request::builder()
                .method(Method::DELETE)
                .uri(format!("/v1/me/sessions/{}", session_b.id))
                .header("x-request-id", REQUEST_ID)
                .header("x-forwarded-for", unique_xff())
                .header("authorization", format!("Bearer {access_a}"))
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.text());
    let audits = audit_logs::list_for_target(&app.state.pool, "user", &user_id.to_string())
        .await
        .expect("list audits");
    assert_eq!(
        audits
            .iter()
            .filter(|row| row.action == "session.revoked")
            .count(),
        1
    );
}

#[tokio::test]
async fn delete_unknown_or_foreign_session_is_404() {
    let app = app().await;
    let email_a = unique_email("user-a");
    register_user(&app, &email_a).await;
    let (access_a, _refresh_a) = login_tokens(&app, &email_a, CHROME_MAC_UA).await;
    let email_b = unique_email("user-b");
    register_user(&app, &email_b).await;
    let (_access_b, refresh_b) = login_tokens(&app, &email_b, FIREFOX_LINUX_UA).await;
    let session_b = session_for(&app, &refresh_b).await;

    // A random id → 404.
    let res = app
        .request(
            Request::builder()
                .method(Method::DELETE)
                .uri(format!("/v1/me/sessions/{}", Uuid::now_v7()))
                .header("x-request-id", REQUEST_ID)
                .header("x-forwarded-for", unique_xff())
                .header("authorization", format!("Bearer {access_a}"))
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-2010", "{}", res.text());

    // Another user's session id → the same 404 (no
    // enumeration of other users' sessions).
    let res = app
        .request(
            Request::builder()
                .method(Method::DELETE)
                .uri(format!("/v1/me/sessions/{}", session_b.id))
                .header("x-request-id", REQUEST_ID)
                .header("x-forwarded-for", unique_xff())
                .header("authorization", format!("Bearer {access_a}"))
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-2010", "{}", res.text());
}

#[tokio::test]
async fn list_requires_auth() {
    let app = app().await;
    register_user(&app, &unique_email("user")).await;

    let res = app
        .request(
            Request::builder()
                .method(Method::GET)
                .uri("/v1/me/sessions")
                .header("x-request-id", REQUEST_ID)
                .header("x-forwarded-for", unique_xff())
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-1001", "{}", res.text());
}

#[tokio::test]
async fn throttled_last_seen_verified() {
    let app = app().await;
    let email = unique_email("user");
    register_user(&app, &email).await;

    // Case 1: last_seen 10 minutes old (older than the
    // 5-minute throttle) — presenting the token advances it.
    let (_access_a, refresh_a) = login_tokens(&app, &email, CHROME_MAC_UA).await;
    let session_a = session_for(&app, &refresh_a).await;
    backdate_last_seen(&app, session_a.id, 10).await;
    let before = session_for(&app, &refresh_a).await;
    assert!(
        before.last_seen.expect("last_seen set").timestamp() < Utc::now().timestamp() - 500,
        "the backdate must hold: {before:?}"
    );
    let res = app.request(refresh_req(&refresh_a)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let after = session_for(&app, &refresh_a).await;
    assert!(
        after.last_seen.expect("last_seen set").timestamp() >= Utc::now().timestamp() - 5,
        "last_seen must have advanced to ~now: {after:?}"
    );
    assert!(
        after.last_seen.unwrap() > before.last_seen.unwrap(),
        "10 min > 5 min throttle, so the touch must land: {before:?} -> {after:?}"
    );

    // Case 2: last_seen 1 minute old (inside the throttle) —
    // presenting the token does NOT advance it.
    let (_access_b, refresh_b) = login_tokens(&app, &email, FIREFOX_LINUX_UA).await;
    let session_b = session_for(&app, &refresh_b).await;
    backdate_last_seen(&app, session_b.id, 1).await;
    let before = session_for(&app, &refresh_b).await;
    let res = app.request(refresh_req(&refresh_b)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let after = session_for(&app, &refresh_b).await;
    assert_eq!(
        after.last_seen, before.last_seen,
        "1 min < 5 min throttle, so the touch must NOT land: {before:?} -> {after:?}"
    );
}
