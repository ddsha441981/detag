//! P1-026: the HIBP pwned check, end to end (flag on).
//!
//! Through the real router (`TestApp` oneshot) with the flag on
//! and the HIBP URL pointed at a local stub server (never the real
//! API — tests must not depend on network). The stub serves one
//! canned range body; the test builds the body from the candidate
//! password's own SHA-1 suffix, so the pwned/clean cases are
//! exact, not hand-copied vectors.

mod common;

use axum::http::{Method, Request, StatusCode};
use common::{TestApp, TestFixtures};
use detag_testkit::serve_one_http_response;
use detag_validator::hibp_sha1_prefix_suffix;
use serde_json::json;
use uuid::Uuid;

/// The request id: a valid UUID, echoed by the middleware.
const REQUEST_ID: &str = "123e4567-e89b-42d3-a456-426614174101";

/// App with the pwned check on, pointed at `hibp_url`.
async fn pwned_app(hibp_url: &str) -> TestApp {
    let fixtures = TestFixtures::from_env().await;
    let mut settings = fixtures.settings();
    settings.password_pwned_check = true;
    settings.password_pwned_url = hibp_url.to_string();
    TestApp::spawn_with_fixtures_and_settings(fixtures, settings).await
}

fn post_register(email: &str, password: &str) -> Request<axum::body::Body> {
    let body = json!({
        "email": email,
        "password": password,
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

fn unique_email() -> String {
    format!("pwned-{}@example.com", Uuid::now_v7())
}

/// A range body that reports `password` as breached: its suffix,
/// computed here (not hand-copied), plus one unrelated suffix.
fn breached_body(password: &str) -> String {
    let (_, suffix) = hibp_sha1_prefix_suffix(password);
    format!("{suffix}:9543776\n0018A45C4D1DEF81644B54AB7F969B88D65:1")
}

#[tokio::test]
async fn breached_password_is_a_422_field_error() {
    // "letmein2024!!" passes the local policy (zxcvbn 3), so the
    // only thing standing between it and a 201 is the pwned
    // check — and the stub reports it breached.
    let password = "letmein2024!!";
    let (url, server) = serve_one_http_response(200, breached_body(password)).await;
    let app = pwned_app(&url).await;
    let res = app.request(post_register(&unique_email(), password)).await;
    assert_eq!(
        res.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        res.text()
    );
    let json = res.json();
    assert_eq!(json["code"], "PF-2002", "{json}");
    let fields = json["field_errors"].as_array().expect("field_errors");
    assert!(
        fields
            .iter()
            .any(|f| f["field"] == "password" && f["code"] == "pwned_password"),
        "password/pwned_password among {fields:?}"
    );
    server.abort();
}

#[tokio::test]
async fn clean_password_registers_with_flag_on() {
    // Same password, but the stub's range does not contain its
    // suffix — the check passes and registration proceeds.
    let password = "letmein2024!!";
    let (url, server) = serve_one_http_response(
        200,
        "0018A45C4D1DEF81644B54AB7F969B88D65:1\n01330C689E5DC05884B7E4379AF1E0A7C:2".to_string(),
    )
    .await;
    let app = pwned_app(&url).await;
    let res = app.request(post_register(&unique_email(), password)).await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.text());
    server.abort();
}

#[tokio::test]
async fn hibp_outage_fails_open_at_register() {
    // The flag is on but the HIBP URL points at a closed port:
    // the lookup errors, the policy fails open (warn logged),
    // and registration proceeds — a down HIBP must never block
    // signups.
    let fixtures = TestFixtures::from_env().await;
    let mut settings = fixtures.settings();
    settings.password_pwned_check = true;
    settings.password_pwned_url = "http://127.0.0.1:1".to_string();
    let app = TestApp::spawn_with_fixtures_and_settings(fixtures, settings).await;
    let res = app
        .request(post_register(&unique_email(), "letmein2024!!"))
        .await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.text());
}
