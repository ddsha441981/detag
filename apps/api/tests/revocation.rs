//! P1-021: the token revocation cache, end to end.
//!
//! Two mechanisms kill a live access token before its 15-minute
//! expiry, and the extractor consults both:
//! - the epoch (`users.revoked_before`): a token whose `iat` is
//!   earlier than it is dead — bumped by a password change
//!   (P1-018's service, same tx as the session revocation);
//! - the jti denylist (Redis `pf:revocation:jti:{jti}`, TTL =
//!   the token's remaining life): a listed `jti` is dead —
//!   written by `POST /v1/auth/logout-all` for the presented
//!   token.
//!
//! Both rejections are 401 `PF-1010` (invalid token) — a revoked
//! token is not distinguishable from a forged one.

mod common;

use axum::http::{Method, Request, StatusCode};
use common::{TestApp, TestFixtures};
use detag_api::services::{password::revoke_sessions_on_password_change, revocation::revoke_jti};
use detag_core::{
    jwt::{verify_access_token, ACCESS_TOKEN_TTL_SECS},
    repos::users,
};
use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use serde_json::json;
use uuid::Uuid;

/// The request id: a valid UUID, echoed by the middleware.
const REQUEST_ID: &str = "123e4567-e89b-42d3-a456-426614174104";

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
        .trim_start_matches("detag_refresh=")
        .to_string();
    (access, refresh)
}

/// `GET /v1/me` with a Bearer token.
async fn get_me(app: &TestApp, token: &str) -> common::TestResponse {
    app.request(
        Request::builder()
            .method(Method::GET)
            .uri("/v1/me")
            .header("x-request-id", REQUEST_ID)
            .header("x-forwarded-for", unique_xff())
            .header("authorization", format!("Bearer {token}"))
            .body(axum::body::Body::empty())
            .unwrap(),
    )
    .await
}

/// `POST /v1/auth/logout-all` with a Bearer token.
async fn logout_all(app: &TestApp, token: &str) -> common::TestResponse {
    app.request(
        Request::builder()
            .method(Method::POST)
            .uri("/v1/auth/logout-all")
            .header("x-request-id", REQUEST_ID)
            .header("x-forwarded-for", unique_xff())
            .header("authorization", format!("Bearer {token}"))
            .body(axum::body::Body::empty())
            .unwrap(),
    )
    .await
}

#[tokio::test]
async fn epoch_bump_kills_pre_change_tokens() {
    // P1-021: a password change bumps the epoch, and every
    // access token issued before it dies at the extractor —
    // the session revocation alone cannot kill a stateless
    // token. The reset case (`None`) revokes every session;
    // the token is dead by the epoch, not by the session.
    let app = app().await;
    let email = unique_email("epoch");
    let user_id = register_user(&app, &email).await;
    let (access, _refresh) = login_tokens(&app, &email).await;
    let res = get_me(&app, &access).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());

    revoke_sessions_on_password_change(&app.state, user_id, None)
        .await
        .expect("password-change revocation succeeds");

    // The pre-change token is dead: 401 PF-1010, the same code
    // as a forged token — a revoked token is not
    // distinguishable from an invalid one.
    let res = get_me(&app, &access).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-1010", "{}", res.text());

    // A fresh login mints a token with a NEW `iat` (after the
    // bump), and it works. The 1.1 s sleep is load-bearing:
    // the epoch is microsecond-precise while `iat` is whole
    // seconds — a token minted in the same second AFTER the
    // bump has an `iat` earlier than the bump instant and is
    // denied too (it self-heals via refresh, but this test
    // asserts the clean path, so it waits for the next
    // second boundary before minting).
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    let (fresh, _refresh) = login_tokens(&app, &email).await;
    let res = get_me(&app, &fresh).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
}

#[tokio::test]
async fn jti_denylist_kills_the_listed_token_only() {
    // P1-021: denylisting one `jti` kills exactly that token —
    // another token of the same user (a different issuance,
    // a different `jti`) is untouched.
    let app = app().await;
    let email = unique_email("jti");
    register_user(&app, &email).await;
    let (access_a, _refresh_a) = login_tokens(&app, &email).await;
    let (access_b, _refresh_b) = login_tokens(&app, &email).await;
    let claims_a =
        verify_access_token(&app.settings.jwt_secret, &access_a).expect("token A verifies");
    let jti_a = claims_a.jti.expect("post-P1-013 tokens carry a jti");

    revoke_jti(&app.state, &jti_a, claims_a.exp)
        .await
        .expect("denylist write succeeds");

    let res = get_me(&app, &access_a).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-1010", "{}", res.text());
    let res = get_me(&app, &access_b).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
}

#[tokio::test]
async fn logout_all_denylists_the_presented_token() {
    // P1-021: logout-all kills the PRESENTED access token
    // immediately (its `jti` is denylisted) — not just the
    // sessions. Another device's token (a different `jti`,
    // never presented) lives until its expiry: only the epoch,
    // on a password change, kills those.
    let app = app().await;
    let email = unique_email("logout-all");
    register_user(&app, &email).await;
    let (access_a, _refresh_a) = login_tokens(&app, &email).await;
    let (access_b, _refresh_b) = login_tokens(&app, &email).await;

    let res = logout_all(&app, &access_b).await;
    assert_eq!(res.status, StatusCode::NO_CONTENT, "{}", res.text());

    let res = get_me(&app, &access_b).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-1010", "{}", res.text());
    let res = get_me(&app, &access_a).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
}

#[tokio::test]
async fn legacy_token_without_jti_skips_denylist_but_epoch_applies() {
    // P1-021: a pre-P1-013 token (claims {sub, role, exp,
    // iat} only — no `jti`) has nothing to denylist, so the
    // jti check skips it and the token works — but the epoch
    // still covers it via `iat`: after a password change it
    // dies like any other pre-change token.
    let app = app().await;
    let email = unique_email("legacy");
    let user_id = register_user(&app, &email).await;
    let now = chrono::Utc::now();
    let legacy = json!({
        "sub": user_id.to_string(),
        "role": "user",
        "exp": (now + chrono::Duration::seconds(ACCESS_TOKEN_TTL_SECS)).timestamp(),
        "iat": now.timestamp(),
    });
    let token = encode(
        &Header::new(Algorithm::HS256),
        &legacy,
        &EncodingKey::from_secret(app.settings.jwt_secret.as_bytes()),
    )
    .expect("legacy token encodes");
    // The verifier accepts it (P1-013 backward compat) and
    // its `jti` is None.
    let claims =
        verify_access_token(&app.settings.jwt_secret, &token).expect("legacy token verifies");
    assert_eq!(claims.jti, None);

    let res = get_me(&app, &token).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());

    revoke_sessions_on_password_change(&app.state, user_id, None)
        .await
        .expect("password-change revocation succeeds");
    let res = get_me(&app, &token).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-1010", "{}", res.text());

    // The epoch bump is visible on the user row (P1-021's
    // other half: the password service bumps it in the same
    // tx as the session revocation).
    let user = users::by_id(&app.state.pool, user_id)
        .await
        .expect("user loads")
        .expect("user exists");
    assert!(
        user.revoked_before.is_some(),
        "the epoch is set after a password change"
    );
}
