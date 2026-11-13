//! P1-003: the login endpoint, end to end.
//!
//! Through the real router (`TestApp` oneshot) against the
//! migrated scratch database: the happy path (access token +
//! refresh cookie + session row + audit), the generic 401
//! (unknown email and wrong password answer byte-identically
//! — no enumeration), the per-account rate limit (10 burst /
//! 1 per min by default; the test uses 3/0), the per-IP limit
//! (the middleware's bucket, tripped with a small test burst
//! on a per-test XFF address), and the structural 422s.
//! The access token is verified with the app's own JWT secret
//! (the test settings' `jwt_secret` — `dev-fixed` by
//! default); the refresh token is read back from the
//! `Set-Cookie` header, exactly as a client would read it.

mod common;

use axum::http::{Method, Request, StatusCode};
use common::{TestApp, TestFixtures};
use detag_api::services::login::REFRESH_COOKIE_NAME;
use detag_core::{
    checksum::sha256_hex,
    jwt::verify_access_token,
    password::{hash_password, verify_password, Argon2Params},
    repos::{audit_logs, sessions, users},
};
use serde_json::json;
use uuid::Uuid;

/// The request id: a valid UUID, echoed by the middleware.
const REQUEST_ID: &str = "123e4567-e89b-42d3-a456-426614174103";

/// The password every test user registers with.
const PASSWORD: &str = "correct horse battery staple";

async fn login_app() -> TestApp {
    let fixtures = TestFixtures::from_env().await;
    let mut settings = fixtures.settings();
    settings.disposable_email_domains = String::new();
    settings.tos_current_version = "v1".to_string();
    TestApp::spawn_with_fixtures_and_settings(fixtures, settings).await
}

/// App with custom rate-limit settings (the rate-limit tests
/// shrink one bucket at a time; everything else is default).
async fn login_app_with_limits(
    ip_burst: u64,
    ip_refill: u64,
    account_burst: u64,
    account_refill: u64,
    login_ip_burst: u64,
    login_ip_refill: u64,
) -> TestApp {
    let fixtures = TestFixtures::from_env().await;
    let mut settings = fixtures.settings();
    settings.disposable_email_domains = String::new();
    settings.tos_current_version = "v1".to_string();
    settings.rate_limit_ip_burst = ip_burst;
    settings.rate_limit_ip_refill_per_min = ip_refill;
    settings.rate_limit_login_account_burst = account_burst;
    settings.rate_limit_login_account_refill_per_min = account_refill;
    settings.rate_limit_login_ip_burst = login_ip_burst;
    settings.rate_limit_login_ip_refill_per_min = login_ip_refill;
    TestApp::spawn_with_fixtures_and_settings(fixtures, settings).await
}

fn post_register(email: &str) -> Request<axum::body::Body> {
    let body = json!({
        "email": email,
        "password": PASSWORD,
        "accept_tos_version": "v1",
    });
    Request::builder()
        .method(Method::POST)
        .uri("/v1/auth/register")
        .header("x-request-id", REQUEST_ID)
        .header("content-type", "application/json")
        .body(axum::body::Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap()
}

fn post_login(email: &str, password: &str) -> Request<axum::body::Body> {
    post_login_with_headers(email, password, None, None)
}

/// Login with optional per-request headers: a `User-Agent`
/// (recorded in the session device) and an `X-Forwarded-For`
/// (the middleware's IP bucket key — each rate-limit test uses
/// its own TEST-NET address so parallel tests never share a
/// bucket).
fn post_login_with_headers(
    email: &str,
    password: &str,
    user_agent: Option<&str>,
    xff: Option<&str>,
) -> Request<axum::body::Body> {
    let body = json!({ "email": email, "password": password });
    let mut req = Request::builder()
        .method(Method::POST)
        .uri("/v1/auth/login")
        .header("x-request-id", REQUEST_ID)
        .header("content-type", "application/json");
    if let Some(ua) = user_agent {
        req = req.header("user-agent", ua);
    }
    if let Some(xff) = xff {
        req = req.header("x-forwarded-for", xff);
    }
    req.body(axum::body::Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap()
}

fn unique_email() -> String {
    format!("login-{}@example.com", Uuid::now_v7())
}

/// Register a user, return its id.
async fn register_user(app: &TestApp, email: &str) -> Uuid {
    let res = app.request(post_register(email)).await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.text());
    res.json()["id"].as_str().unwrap().parse().unwrap()
}

/// The refresh token from a login response's `Set-Cookie`
/// header (the cookie value, before the first `;`).
fn refresh_token_from(res: &common::TestResponse) -> String {
    let cookie = res
        .headers
        .get("set-cookie")
        .expect("login sets the refresh cookie")
        .to_str()
        .unwrap()
        .to_string();
    assert!(
        cookie.starts_with(&format!("{REFRESH_COOKIE_NAME}=")),
        "cookie name: {cookie}"
    );
    cookie
        .split(';')
        .next()
        .unwrap()
        .trim_start_matches(&format!("{REFRESH_COOKIE_NAME}="))
        .to_string()
}

/// The full `Set-Cookie` header value.
fn set_cookie_header(res: &common::TestResponse) -> String {
    res.headers
        .get("set-cookie")
        .expect("login sets the refresh cookie")
        .to_str()
        .unwrap()
        .to_string()
}

#[tokio::test]
async fn login_happy_path_returns_token_cookie_session_and_audit() {
    let app = login_app().await;
    let email = unique_email();
    let user_id = register_user(&app, &email).await;

    let res = app
        .request(post_login_with_headers(
            &email,
            PASSWORD,
            Some("login-test/1.0"),
            None,
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let json = res.json();
    assert_eq!(json["token_type"], "Bearer");
    assert_eq!(json["expires_in"], 900);
    let access_token = json["access_token"].as_str().unwrap();
    assert_eq!(access_token.split('.').count(), 3, "a JWT");
    assert!(json.get("refresh_token").is_none(), "never in the body");

    // The access token verifies against the app's secret, with
    // the right subject + role.
    let claims = verify_access_token(&app.settings.jwt_secret, access_token)
        .expect("the issued token verifies");
    assert_eq!(claims.sub, user_id.to_string());
    assert_eq!(claims.role, "user");
    assert_eq!(claims.exp - claims.iat, 900);

    // The cookie: httpOnly refresh, Secure, SameSite=Lax,
    // scoped to /v1/auth, 30d.
    let cookie = set_cookie_header(&res);
    assert!(cookie.contains("; HttpOnly"), "{cookie}");
    assert!(cookie.contains("; Secure"), "{cookie}");
    assert!(cookie.contains("; SameSite=Lax"), "{cookie}");
    assert!(cookie.contains("; Path=/v1/auth"), "{cookie}");
    assert!(cookie.contains("; Max-Age=2592000"), "{cookie}");
    let refresh_token = refresh_token_from(&res);
    assert_eq!(refresh_token.len(), 43, "base64url 32-byte token");

    // The session row: hash of the cookie token, fresh family,
    // 30d expiry, device from the request (oneshot has no
    // socket, so the IP is "unknown").
    let rows = sessions::list_for_user(&app.state.pool, user_id)
        .await
        .unwrap();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].refresh_hash, sha256_hex(refresh_token.as_bytes()));
    assert!(rows[0].revoked_at.is_none());
    assert!(rows[0].replaced_by.is_none());
    assert!(
        rows[0].expires_at > chrono::Utc::now() + chrono::Duration::days(29),
        "expiry is ~30d out"
    );
    assert_eq!(rows[0].device["ua"], json!("login-test/1.0"));
    assert_eq!(
        rows[0].device["ip_hash"],
        json!(sha256_hex("unknown".as_bytes()))
    );

    // Both audit events landed (attempt anonymous, success for
    // the user).
    let attempts = audit_logs::list_for_action(&app.state.pool, "login.attempt")
        .await
        .unwrap();
    assert!(
        attempts
            .iter()
            .any(|log| log.after.as_ref().is_some_and(
                |after| after == &json!({ "email_hash": sha256_hex(email.as_bytes()) })
            )),
        "attempt must be audited with the email hash, not the email"
    );
    let logs = audit_logs::list_for_target(&app.state.pool, "user", &user_id.to_string())
        .await
        .unwrap();
    assert!(
        logs.iter().any(|log| log.action == "login.success"),
        "{logs:?}"
    );
}

#[tokio::test]
async fn login_wrong_password_and_unknown_email_are_identical_401s() {
    let app = login_app().await;
    let email = unique_email();
    let user_id = register_user(&app, &email).await;

    // Wrong password for a real account…
    let wrong = app.request(post_login(&email, "wrong password here")).await;
    assert_eq!(wrong.status, StatusCode::UNAUTHORIZED, "{}", wrong.text());
    // …and an email that was never registered…
    let unknown = app
        .request(post_login(&unique_email(), "wrong password here"))
        .await;
    assert_eq!(
        unknown.status,
        StatusCode::UNAUTHORIZED,
        "{}",
        unknown.text()
    );

    // …answer byte-identically (modulo the request id, which
    // the test fixed to the same value for both — so the whole
    // body must match). That is the no-enumeration proof.
    assert_eq!(
        wrong.text(),
        unknown.text(),
        "wrong-password and unknown-email must be indistinguishable"
    );
    let json = wrong.json();
    assert_eq!(json["code"], "PF-1001", "{json}");
    assert_eq!(json["request_id"], REQUEST_ID, "{json}");

    // The audit log distinguishes (internal only): one failure
    // per reason.
    let failures = audit_logs::list_for_action(&app.state.pool, "login.failure")
        .await
        .unwrap();
    let reasons: Vec<_> = failures
        .iter()
        .filter_map(|log| {
            log.after
                .as_ref()?
                .get("reason")?
                .as_str()
                .map(str::to_string)
        })
        .collect();
    assert!(
        reasons.contains(&"wrong_password".to_string()),
        "{reasons:?}"
    );
    assert!(
        reasons.contains(&"unknown_email".to_string()),
        "{reasons:?}"
    );
    // And neither failure created a session.
    let rows = sessions::list_for_user(&app.state.pool, user_id)
        .await
        .unwrap();
    assert!(rows.is_empty(), "{rows:?}");
}

#[tokio::test]
async fn login_unverified_user_can_log_in() {
    let app = login_app().await;
    let email = unique_email();
    register_user(&app, &email).await;
    // No verification step: the user is unverified, and login
    // still succeeds — verification gates job creation
    // (PF-1009), not authentication.
    let res = app.request(post_login(&email, PASSWORD)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
}

#[tokio::test]
async fn login_second_login_creates_a_new_family() {
    let app = login_app().await;
    let email = unique_email();
    let user_id = register_user(&app, &email).await;
    let first = app.request(post_login(&email, PASSWORD)).await;
    assert_eq!(first.status, StatusCode::OK);
    let second = app.request(post_login(&email, PASSWORD)).await;
    assert_eq!(second.status, StatusCode::OK);

    let rows = sessions::list_for_user(&app.state.pool, user_id)
        .await
        .unwrap();
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert_ne!(rows[0].family_id, rows[1].family_id, "one family per login");
    assert_ne!(
        rows[0].refresh_hash, rows[1].refresh_hash,
        "one refresh token per login"
    );
    assert_ne!(
        refresh_token_from(&first),
        refresh_token_from(&second),
        "the cookies differ too"
    );
}

#[tokio::test]
async fn login_account_rate_limit_trips_before_password_check() {
    // Account bucket: 3 burst, 0 refill — three attempts, then
    // nothing until the key expires. The IP bucket stays at the
    // default (120), so only the account bucket can trip here.
    let app = login_app_with_limits(120, 120, 3, 0, 20, 10).await;
    let email = unique_email();
    let user_id = register_user(&app, &email).await;

    for _ in 0..3 {
        let res = app.request(post_login(&email, "wrong password here")).await;
        assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());
    }
    // The fourth attempt — with the CORRECT password — is
    // denied before the password is ever checked: the limit is
    // pre-auth, not post-auth.
    let res = app.request(post_login(&email, PASSWORD)).await;
    assert_eq!(res.status, StatusCode::TOO_MANY_REQUESTS, "{}", res.text());
    let json = res.json();
    assert_eq!(json["code"], "PF-3007", "{json}");
    assert_eq!(json["request_id"], REQUEST_ID, "{json}");
    assert!(
        res.headers.get("retry-after").is_some(),
        "429 carries Retry-After"
    );
    let limit = res
        .headers
        .get("x-ratelimit-limit")
        .and_then(|v| v.to_str().ok());
    assert_eq!(
        limit,
        Some("3"),
        "the Limit names the account bucket (burst 3) — the middleware must not overwrite a service denial's headers (P1-020)"
    );
    // And no session was created by the denied attempt.
    let rows = sessions::list_for_user(&app.state.pool, user_id)
        .await
        .unwrap();
    assert!(rows.is_empty(), "{rows:?}");
}

#[tokio::test]
async fn login_ip_rate_limit_trips_across_accounts() {
    // P1-020: the per-IP login bucket (3 burst / 0 refill
    // here) bounds a credential-stuffing run from ONE IP
    // against MANY accounts — each account is under its own
    // account bucket (100 burst here, so it cannot trip), and
    // the middleware IP bucket is 120, so only the login-IP
    // bucket can deny. A per-run UUID XFF: a refill-0 bucket
    // never refills, and its Redis key survives across runs
    // (the P1-003 lesson — a fixed address would start the
    // next run already drained).
    let xff = Uuid::now_v7().to_string();
    let app = login_app_with_limits(120, 120, 100, 60_000, 3, 0).await;
    // Three different accounts, one wrong-password login
    // each: each is 401 (the account bucket admits, the IP
    // bucket admits), and no session is created. Three
    // failures from one IP is below the lockout threshold
    // (5), so no lock interferes.
    for _ in 0..3 {
        let email = unique_email();
        register_user(&app, &email).await;
        let res = app
            .request(post_login_with_headers(
                &email,
                "wrong password here",
                None,
                Some(xff.as_str()),
            ))
            .await;
        assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.text());
    }
    // The fourth login — a FOURTH account, with the CORRECT
    // password — is denied by the IP bucket before the
    // password is ever checked.
    let email = unique_email();
    let user_id = register_user(&app, &email).await;
    let res = app
        .request(post_login_with_headers(
            &email,
            PASSWORD,
            None,
            Some(xff.as_str()),
        ))
        .await;
    assert_eq!(res.status, StatusCode::TOO_MANY_REQUESTS, "{}", res.text());
    let json = res.json();
    assert_eq!(json["code"], "PF-3007", "{json}");
    assert_eq!(json["request_id"], REQUEST_ID, "{json}");
    assert!(
        res.headers.get("retry-after").is_some(),
        "429 carries Retry-After"
    );
    let limit = res
        .headers
        .get("x-ratelimit-limit")
        .and_then(|v| v.to_str().ok());
    assert_eq!(
        limit,
        Some("3"),
        "the Limit names the login-IP bucket (burst 3), not the middleware IP bucket (120)"
    );
    let remaining = res
        .headers
        .get("x-ratelimit-remaining")
        .and_then(|v| v.to_str().ok());
    assert_eq!(remaining, Some("0"));
    assert!(
        res.headers.get("x-ratelimit-reset").is_some(),
        "429 carries the X-RateLimit trio"
    );
    // And no session was created by the denied attempt.
    let rows = sessions::list_for_user(&app.state.pool, user_id)
        .await
        .expect("sessions list");
    assert!(rows.is_empty(), "denied login creates no session");
}

#[tokio::test]
async fn login_ip_rate_limit_trips() {
    // IP bucket: 5 burst, 0 refill, on a per-test XFF address
    // (the oneshot "unknown" bucket is shared across tests in
    // this binary — a unique XFF isolates this test). The
    // address is a per-run UUID, not a fixed TEST-NET IP: a
    // refill-0 bucket never refills, and its Redis key survives
    // across test runs — a fixed address would start the next
    // run already drained (caught by `just verify` after the
    // targeted run had passed). The middleware treats the XFF
    // entry as an opaque bucket id, so a UUID is a valid key.
    // The account bucket stays at the default (10), so only the
    // IP bucket can trip here.
    let xff = format!("203.0.113.{}", Uuid::now_v7());
    let app = login_app_with_limits(5, 0, 10, 1, 20, 10).await;
    let email = unique_email();
    register_user(&app, &email).await;
    // The register above did NOT send XFF (it used the shared
    // "unknown" bucket), so this IP's bucket is still full:
    // five logins, then the sixth is denied.
    for _ in 0..5 {
        let res = app
            .request(post_login_with_headers(
                &email,
                PASSWORD,
                None,
                Some(xff.as_str()),
            ))
            .await;
        assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    }
    let res = app
        .request(post_login_with_headers(
            &email,
            PASSWORD,
            None,
            Some(xff.as_str()),
        ))
        .await;
    assert_eq!(res.status, StatusCode::TOO_MANY_REQUESTS, "{}", res.text());
    assert_eq!(res.json()["code"], "PF-3007");
}

#[tokio::test]
async fn login_structural_failures_are_422() {
    let app = login_app().await;
    // Bad email shape.
    let res = app.request(post_login("not-an-email", PASSWORD)).await;
    assert_eq!(
        res.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        res.text()
    );
    assert_eq!(res.json()["code"], "PF-2002");
    // Empty password.
    let res = app.request(post_login(&unique_email(), "")).await;
    assert_eq!(
        res.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        res.text()
    );
    assert_eq!(res.json()["code"], "PF-2002");
    // Malformed JSON.
    let req = Request::builder()
        .method(Method::POST)
        .uri("/v1/auth/login")
        .header("x-request-id", REQUEST_ID)
        .header("content-type", "application/json")
        .body(axum::body::Body::from("{not json"))
        .unwrap();
    let res = app.request(req).await;
    assert_eq!(
        res.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        res.text()
    );
    assert_eq!(res.json()["code"], "PF-2002");
}

#[tokio::test]
async fn login_rehashes_stale_params_hash_on_success() {
    // P1-027: a user whose hash was made under the pre-P1-027
    // defaults (m=19 MiB, t=2, p=1) logs in and leaves with
    // the current policy's hash — the upgrade path.
    let app = login_app().await;
    let email = unique_email();
    let old_hash = hash_password(PASSWORD, &Argon2Params::previous_defaults()).unwrap();
    assert!(old_hash.contains("m=19456,t=2,p=1"), "{old_hash}");
    let user = app.seed_user(&email, &old_hash).await;

    let res = app.request(post_login(&email, PASSWORD)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());

    let updated = users::by_email(&app.state.pool, &email)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(updated.id, user.id);
    assert_ne!(
        updated.password_hash, old_hash,
        "the hash must be replaced on login"
    );
    assert!(
        updated.password_hash.contains("m=65536,t=3,p=4"),
        "upgraded to the P1-027 defaults: {}",
        updated.password_hash
    );
    assert!(
        verify_password(PASSWORD, &updated.password_hash).unwrap(),
        "the upgraded hash still verifies the same password"
    );
}

#[tokio::test]
async fn login_leaves_current_params_hash_untouched() {
    // P1-027: a user already on the current params logs in
    // and the stored hash is byte-identical afterwards — no
    // pointless rehash (and no new salt) on every login.
    let app = login_app().await;
    let email = unique_email();
    let current_hash = hash_password(PASSWORD, &Argon2Params::defaults()).unwrap();
    app.seed_user(&email, &current_hash).await;

    let res = app.request(post_login(&email, PASSWORD)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());

    let after = users::by_email(&app.state.pool, &email)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        after.password_hash, current_hash,
        "a current hash must not be rewritten on login"
    );
}

#[tokio::test]
async fn login_response_never_leaks_the_password_or_hash() {
    let app = login_app().await;
    let email = unique_email();
    register_user(&app, &email).await;
    let res = app.request(post_login(&email, PASSWORD)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let rendered = res.text();
    assert!(!rendered.contains(PASSWORD), "never echo the password");
    let user = users::by_email(&app.state.pool, &email)
        .await
        .unwrap()
        .unwrap();
    assert!(
        !rendered.contains(&user.password_hash),
        "never leak the hash"
    );
}
