//! P1-016: cookie + bearer dual auth, end to end.
//!
//! Through the real router (`TestApp` oneshot) against the
//! migrated scratch database. The two paths:
//!
//! - **Bearer (the access path):** the access token travels
//!   as `Authorization: Bearer <jwt>` — for the web client
//!   (held in memory) and for API clients alike. It is the
//!   ONLY credential the extractor accepts.
//! - **Cookie (the refresh path):** the `detag_refresh`
//!   cookie holds the REFRESH token, `HttpOnly; Secure;
//!   SameSite=Lax; Path=/v1/auth`. It exists only for
//!   `POST /v1/auth/refresh` (P1-014). Presented as the sole
//!   credential on a protected route it 401s — even when it
//!   is a genuine, live refresh token (that is the point:
//!   a stolen refresh token must not walk straight into the
//!   API as if it were an access token).
//!
//! Precedence: a Bearer token wins over a cookie when both
//! are present — the cookie is then ignored, never validated
//! as an access credential (a forged cookie alongside a
//! valid Bearer token does not fail the request). API keys
//! are E1.4, not this ticket: a `Bearer <key>` 401s.
//!
//! The CSRF shape is proven too: `GET /v1/auth/refresh` is
//! 405, so a top-level navigation (which DOES carry a
//! `SameSite=Lax` cookie) cannot rotate a session — there is
//! no state-changing GET anywhere.

mod common;

use axum::http::{Method, Request, StatusCode};
use common::{TestApp, TestFixtures};
use serde_json::json;
use uuid::Uuid;

/// The request id: a valid UUID, echoed by the middleware.
const REQUEST_ID: &str = "123e4567-e89b-42d3-a456-426614174103";

/// The password every test user registers with.
const PASSWORD: &str = "correct horse battery staple";

/// An API-key-shaped Bearer token (no dots — the extractor
/// classifies it as an API key, not a JWT). API keys are
/// E1.4; until then this must 401.
const API_KEY: &str = "pf_live_01J0000000000000000000000";

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

/// GET /v1/me with the given credential combination.
fn me_req(bearer: Option<&str>, cookie: Option<&str>) -> Request<axum::body::Body> {
    let mut req = Request::builder()
        .method(Method::GET)
        .uri("/v1/me")
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

#[tokio::test]
async fn bearer_only_authenticates() {
    let app = app().await;
    let email = unique_email("user");
    register_user(&app, &email).await;
    let (access, _refresh) = login_tokens(&app, &email).await;

    let res = app.request(me_req(Some(&access), None)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert_eq!(res.json()["email"], email);
}

#[tokio::test]
async fn cookie_alone_is_401_even_with_a_real_refresh_token() {
    let app = app().await;
    let email = unique_email("user");
    register_user(&app, &email).await;
    let (_access, refresh) = login_tokens(&app, &email).await;

    // The refresh token is genuine and live (it rotates fine
    // — P1-014 proves that). As the SOLE credential on a
    // protected route it still 401s: the refresh token is not
    // an access credential.
    let res = app.request(me_req(None, Some(&refresh))).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-1001", "{}", res.text());
}

#[tokio::test]
async fn bearer_and_cookie_authenticates_by_bearer() {
    let app = app().await;
    let email = unique_email("user");
    register_user(&app, &email).await;
    let (access, refresh) = login_tokens(&app, &email).await;

    // Both credentials present: the Bearer token wins, the
    // cookie is ignored — the request authenticates.
    let res = app.request(me_req(Some(&access), Some(&refresh))).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert_eq!(res.json()["email"], email);
}

#[tokio::test]
async fn bearer_with_wrong_cookie_still_authenticates() {
    let app = app().await;
    let email = unique_email("user");
    register_user(&app, &email).await;
    let (access, _refresh) = login_tokens(&app, &email).await;

    // A forged cookie alongside a valid Bearer token does not
    // fail the request: the cookie is never validated as an
    // access credential.
    let res = app
        .request(me_req(Some(&access), Some("forged-token")))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert_eq!(res.json()["email"], email);
}

#[tokio::test]
async fn unknown_api_key_is_401() {
    let app = app().await;
    let email = unique_email("user");
    register_user(&app, &email).await;

    // An API key that matches no live key is not a
    // credential (the extractor answers the generic 401) —
    // a REAL key authenticates now (P1-034, proven in
    // tests/extractor.rs); this fake one never will.
    let res = app.request(me_req(Some(API_KEY), None)).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-1001", "{}", res.text());
}

#[tokio::test]
async fn refresh_is_post_only() {
    let app = app().await;
    register_user(&app, &unique_email("user")).await;

    // A top-level GET navigation carries a SameSite=Lax
    // cookie — but GET /v1/auth/refresh is 405, so a
    // navigated GET cannot rotate a session (there is no
    // state-changing GET anywhere: every mutation is
    // POST/PATCH/DELETE).
    let res = app
        .request(
            Request::builder()
                .method(Method::GET)
                .uri("/v1/auth/refresh")
                .header("x-request-id", REQUEST_ID)
                .header("x-forwarded-for", unique_xff())
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(res.status, StatusCode::METHOD_NOT_ALLOWED, "{}", res.text());
}
