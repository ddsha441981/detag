//! P1-095: consent versioning, end to end.
//!
//! Through the real router (`TestApp` oneshot) against the
//! migrated scratch database: register requires the AUP
//! version (missing → 422 on `accept_aup_version`); GET
//! /v1/me carries the `consent` section (current at register);
//! a bump — an app spawned with v2 settings and a user seeded
//! with v1/v1 through the repo, exactly what a production
//! config bump produces (config changed, rows didn't) —
//! flips `up_to_date` false; re-accepting a stale version is
//! 422; re-accepting the current versions is 200, clears the
//! banner, updates the row, and writes one
//! `user.consent_accepted` audit row; re-accepting when
//! already current is a no-op 200 with no audit (the PATCH
//! /v1/me precedent). The token comes from a real login
//! (register + login through the router), never minted by
//! hand.

mod common;

use axum::http::{Method, Request, StatusCode};
use common::{TestApp, TestFixtures};
use detag_core::{
    password::{hash_password, Argon2Params},
    repos::{audit_logs, users},
};
use serde_json::json;
use uuid::Uuid;

/// The request id: a valid UUID, echoed by the middleware.
const REQUEST_ID: &str = "123e4567-e89b-42d3-a456-426614174103";

/// The password every test user registers with.
const PASSWORD: &str = "correct horse battery staple";

/// App with both current versions at v1 (the default).
async fn consent_app() -> TestApp {
    let fixtures = TestFixtures::from_env().await;
    let mut settings = fixtures.settings();
    settings.disposable_email_domains = String::new();
    settings.tos_current_version = "v1".to_string();
    settings.aup_current_version = "v1".to_string();
    TestApp::spawn_with_fixtures_and_settings(fixtures, settings).await
}

/// App with both current versions bumped to v2 — the bumped
/// config a re-accept banner is tested against. A user seeded
/// with v1/v1 through the repo is exactly what a production
/// bump leaves behind: config changed, rows didn't.
async fn consent_app_v2() -> TestApp {
    let fixtures = TestFixtures::from_env().await;
    let mut settings = fixtures.settings();
    settings.disposable_email_domains = String::new();
    settings.tos_current_version = "v2".to_string();
    settings.aup_current_version = "v2".to_string();
    TestApp::spawn_with_fixtures_and_settings(fixtures, settings).await
}

fn unique_email() -> String {
    format!("consent-{}@example.com", Uuid::now_v7())
}

/// A per-test XFF address: each test gets its own IP bucket,
/// so the tests never trip each other's rate limits (the
/// P1-003 lesson — the shared "unknown" bucket is one bucket
/// for the whole binary).
fn unique_xff() -> String {
    format!("203.0.113.{}", Uuid::now_v7())
}

/// Register a user, return its id.
async fn register_user(app: &TestApp, email: &str) -> Uuid {
    let body = json!({
        "email": email,
        "password": PASSWORD,
        "accept_tos_version": "v1",
        "accept_aup_version": "v1",
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

/// Seed a user with v1/v1 consent through the repo (a real
/// Argon2id hash, so the test can log in through the router).
/// Used on the v2 app: the row a config bump leaves behind.
async fn seed_v1_user(app: &TestApp, email: &str) -> Uuid {
    let hash = hash_password(PASSWORD, &Argon2Params::defaults()).unwrap();
    users::create_with_consent(&app.pool, email, &hash, Some("v1"), Some("v1"))
        .await
        .expect("seed v1 user")
        .id
}

fn get_me(token: &str) -> Request<axum::body::Body> {
    Request::builder()
        .method(Method::GET)
        .uri("/v1/me")
        .header("x-request-id", REQUEST_ID)
        .header("x-forwarded-for", unique_xff())
        .header("authorization", format!("Bearer {token}"))
        .body(axum::body::Body::empty())
        .unwrap()
}

fn post_consent(token: &str, body: serde_json::Value) -> Request<axum::body::Body> {
    Request::builder()
        .method(Method::POST)
        .uri("/v1/me/consent")
        .header("x-request-id", REQUEST_ID)
        .header("x-forwarded-for", unique_xff())
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {token}"))
        .body(axum::body::Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap()
}

#[tokio::test]
async fn register_requires_aup_version() {
    // A missing `accept_aup_version` never reaches validation:
    // serde rejects the body first, so the answer is the plain
    // 422 (no field_errors) — the malformed-body contract
    // (P0-101), not a `required` field error. The present-but-
    // blank case is the `required` error, covered by the
    // register suite's structural test (4 field errors).
    let app = consent_app().await;
    let body = json!({
        "email": unique_email(),
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
    assert_eq!(
        res.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        res.text()
    );
    let json = res.json();
    assert_eq!(json["code"], "PF-2002", "{json}");
    assert!(
        json.get("field_errors").is_none(),
        "nothing parsed, no field errors: {json}"
    );
}

#[tokio::test]
async fn get_me_reports_consent_current_at_register() {
    let app = consent_app().await;
    let email = unique_email();
    let user_id = register_user(&app, &email).await;
    let token = login_token(&app, &email).await;

    let res = app.request(get_me(&token)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let consent = &res.json()["consent"];
    assert_eq!(consent["tos_version"], "v1", "{consent}");
    assert_eq!(consent["aup_version"], "v1", "{consent}");
    assert_eq!(consent["tos_current_version"], "v1", "{consent}");
    assert_eq!(consent["aup_current_version"], "v1", "{consent}");
    assert_eq!(consent["up_to_date"], true, "{consent}");

    // The row carries both versions (the register test asserts
    // the same; here it proves the profile section reads the row).
    let user = users::by_id(&app.pool, user_id)
        .await
        .unwrap()
        .expect("user row exists");
    assert_eq!(user.tos_version.as_deref(), Some("v1"));
    assert_eq!(user.aup_version.as_deref(), Some("v1"));
}

#[tokio::test]
async fn bump_flips_up_to_date_false() {
    let app = consent_app_v2().await;
    let email = unique_email();
    let user_id = seed_v1_user(&app, &email).await;
    let token = login_token(&app, &email).await;

    let res = app.request(get_me(&token)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let consent = &res.json()["consent"];
    assert_eq!(consent["tos_version"], "v1", "{consent}");
    assert_eq!(consent["aup_version"], "v1", "{consent}");
    assert_eq!(consent["tos_current_version"], "v2", "{consent}");
    assert_eq!(consent["aup_current_version"], "v2", "{consent}");
    assert_eq!(consent["up_to_date"], false, "{consent}");

    // The bump is config-only: the row still says v1.
    let user = users::by_id(&app.pool, user_id)
        .await
        .unwrap()
        .expect("user row exists");
    assert_eq!(user.tos_version.as_deref(), Some("v1"));
    assert_eq!(user.aup_version.as_deref(), Some("v1"));
}

#[tokio::test]
async fn reaccept_with_stale_versions_is_422() {
    let app = consent_app_v2().await;
    let email = unique_email();
    seed_v1_user(&app, &email).await;
    let token = login_token(&app, &email).await;

    let res = app
        .request(post_consent(
            &token,
            json!({"accept_tos_version": "v1", "accept_aup_version": "v1"}),
        ))
        .await;
    assert_eq!(
        res.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        res.text()
    );
    let json = res.json();
    assert_eq!(json["code"], "PF-2002", "{json}");
    let fields = json["field_errors"].as_array().expect("field errors");
    assert_eq!(fields.len(), 2, "{json}");
    assert_eq!(fields[0]["field"], "accept_tos_version");
    assert_eq!(fields[0]["code"], "tos_version_mismatch");
    assert_eq!(fields[1]["field"], "accept_aup_version");
    assert_eq!(fields[1]["code"], "aup_version_mismatch");
}

#[tokio::test]
async fn reaccept_current_clears_banner_and_audits() {
    let app = consent_app_v2().await;
    let email = unique_email();
    let user_id = seed_v1_user(&app, &email).await;
    let token = login_token(&app, &email).await;

    let res = app
        .request(post_consent(
            &token,
            json!({"accept_tos_version": "v2", "accept_aup_version": "v2"}),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let consent = &res.json()["consent"];
    assert_eq!(consent["tos_version"], "v2", "{consent}");
    assert_eq!(consent["aup_version"], "v2", "{consent}");
    assert_eq!(consent["up_to_date"], true, "{consent}");

    // The row now carries v2/v2.
    let user = users::by_id(&app.pool, user_id)
        .await
        .unwrap()
        .expect("user row exists");
    assert_eq!(user.tos_version.as_deref(), Some("v2"));
    assert_eq!(user.aup_version.as_deref(), Some("v2"));

    // One `user.consent_accepted` audit row, actor the caller,
    // target the user, after the new versions.
    let rows = audit_logs::list_for_action(&app.pool, "user.consent_accepted")
        .await
        .expect("list audits");
    let row = rows
        .iter()
        .find(|row| row.target_id.as_deref() == Some(&user_id.to_string()))
        .expect("consent audit exists");
    assert_eq!(row.actor_user_id, Some(user_id));
    assert_eq!(row.target_type, "user");
    let after = row.after.as_ref().expect("audit after exists");
    assert_eq!(after["tos_version"], "v2");
    assert_eq!(after["aup_version"], "v2");
}

#[tokio::test]
async fn reaccept_when_already_current_is_noop_without_audit() {
    let app = consent_app().await;
    let email = unique_email();
    let user_id = register_user(&app, &email).await;
    let token = login_token(&app, &email).await;

    let res = app
        .request(post_consent(
            &token,
            json!({"accept_tos_version": "v1", "accept_aup_version": "v1"}),
        ))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert_eq!(res.json()["consent"]["up_to_date"], true);

    // No `user.consent_accepted` audit row: a no-op writes nothing.
    let rows = audit_logs::list_for_action(&app.pool, "user.consent_accepted")
        .await
        .expect("list audits");
    assert!(
        rows.iter()
            .all(|row| row.target_id.as_deref() != Some(&user_id.to_string())),
        "no-op re-accept must not audit: {rows:?}"
    );
}
