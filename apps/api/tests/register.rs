//! P1-001: the registration endpoint, end to end.
//!
//! Through the real router (`TestApp` oneshot) against the
//! migrated scratch database — the service's transaction (user
//! creation, success audit, and outbox enqueue) is exercised
//! for real, not mocked. The disposable-email denylist and the
//! ToS version come from the test's settings (the harness
//! builds the app from fixtures + explicit settings); the
//! Argon2id params are the OWASP defaults (one real hash per
//! test — ~50 ms each, cheap enough for an integration suite).

mod common;

use axum::http::{Method, Request, StatusCode};
use common::{TestApp, TestFixtures};
use detag_api::services::verification::VERIFY_EMAIL_TEMPLATE;
use detag_core::{
    checksum::sha256_hex,
    config::Settings,
    password::verify_password,
    repos::{audit_logs, email_outbox, users},
};
use serde_json::json;
use uuid::Uuid;

/// The request id: a valid UUID, echoed by the middleware.
const REQUEST_ID: &str = "123e4567-e89b-42d3-a456-426614174101";

/// App with the register test's settings: `mailinator.com` on
/// the disposable denylist, ToS current version "v1".
async fn register_app() -> TestApp {
    let fixtures = TestFixtures::from_env().await;
    let mut settings = fixtures.settings();
    settings.disposable_email_domains = "mailinator.com, tempmail.com".to_string();
    settings.tos_current_version = "v1".to_string();
    TestApp::spawn_with_fixtures_and_settings(fixtures, settings).await
}

/// App with the per-email register bucket at 5 burst / 0
/// refill (P1-020): six attempts for one email trip it
/// exactly. The burst is the production default (5) — the
/// refill is 0 so the trip is deterministic.
async fn register_app_with_email_limit() -> TestApp {
    let fixtures = TestFixtures::from_env().await;
    let mut settings = fixtures.settings();
    settings.disposable_email_domains = "mailinator.com, tempmail.com".to_string();
    settings.tos_current_version = "v1".to_string();
    settings.rate_limit_register_email_burst = 5;
    settings.rate_limit_register_email_refill_per_min = 0;
    TestApp::spawn_with_fixtures_and_settings(fixtures, settings).await
}

fn post_register(email: &str, password: &str, tos: &str) -> Request<axum::body::Body> {
    let body = json!({
        "email": email,
        "password": password,
        "accept_tos_version": tos,
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
    format!("register-{}@example.com", Uuid::now_v7())
}

#[tokio::test]
async fn happy_path_creates_user_audits_and_queues_mail() {
    let app = register_app().await;
    let email = unique_email();
    let res = app
        .request(post_register(&email, "correct horse battery staple", "v1"))
        .await;
    assert_eq!(res.status, StatusCode::CREATED);
    let json = res.json();
    assert_eq!(json["email"], email, "{json}");
    assert_eq!(json["role"], "user");
    assert_eq!(json["status"], "active");
    assert_eq!(json["tos_version"], "v1");
    assert!(json["email_verified_at"].is_null(), "{json}");
    assert!(json.get("password_hash").is_none(), "never leak the hash");
    assert!(json.get("password").is_none(), "never echo the password");
    let user_id: Uuid = json["id"].as_str().unwrap().parse().unwrap();

    // The stored row: normalized email, Argon2id hash, ToS recorded.
    let user = users::by_id(&app.state.pool, user_id)
        .await
        .unwrap()
        .expect("user row exists");
    assert_eq!(user.email, email);
    assert!(user.password_hash.starts_with("$argon2id$v=19$"));
    assert!(verify_password("correct horse battery staple", &user.password_hash).unwrap());
    assert_eq!(user.tos_version.as_deref(), Some("v1"));

    // Both audit events landed (attempt anonymous, success for the user).
    let attempts = audit_logs::list_for_action(&app.state.pool, "register.attempt")
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
    let successes = audit_logs::list_for_target(&app.state.pool, "user", &user_id.to_string())
        .await
        .unwrap();
    assert_eq!(successes.len(), 1, "{successes:?}");
    assert_eq!(successes[0].action, "register.success");
    assert_eq!(successes[0].actor_user_id, Some(user_id));

    // One unsent verification mail, no raw email anywhere in it.
    // The payload carries {user_id, token} (P1-002): the token
    // is a 43-char base64url string, and a token row exists for
    // its hash (unused, ~24h expiry).
    let mail = email_outbox::unsent_for_template(&app.state.pool, VERIFY_EMAIL_TEMPLATE)
        .await
        .unwrap();
    let ours: Vec<_> = mail
        .iter()
        .filter(|row| row.payload["user_id"] == json!(user_id))
        .collect();
    assert_eq!(ours.len(), 1, "{mail:?}");
    assert_eq!(
        ours[0].to_hash.as_deref(),
        Some(sha256_hex(email.as_bytes()).as_str())
    );
    let token = ours[0].payload["token"]
        .as_str()
        .expect("payload carries the raw token");
    assert_eq!(token.len(), 43, "base64url 32-byte token");
    let stored = detag_core::repos::email_verification_tokens::by_hash(
        &app.state.pool,
        &detag_core::tokens::hash_token(token),
    )
    .await
    .unwrap()
    .expect("a token row exists for the queued mail");
    assert_eq!(stored.user_id, user_id);
    assert!(stored.used_at.is_none());
    assert!(
        stored.expires_at > chrono::Utc::now() + chrono::Duration::hours(23),
        "expiry is ~24h out"
    );
    // The row's email-bearing fields carry a hash and an id, never
    // the raw address.
    let rendered = format!(
        "{:?} {} {}",
        ours[0].to_hash, ours[0].payload, stored.token_hash
    );
    assert!(
        !rendered.contains(&email),
        "the outbox row must not contain the raw email"
    );
}

#[tokio::test]
async fn duplicate_email_is_a_generic_409() {
    let app = register_app().await;
    let email = unique_email();
    let res = app
        .request(post_register(&email, "correct horse battery staple", "v1"))
        .await;
    assert_eq!(res.status, StatusCode::CREATED);
    // Same email, different case: citext makes it the same user.
    let res = app
        .request(post_register(
            &email.to_uppercase(),
            "another horse battery staple",
            "v1",
        ))
        .await;
    assert_eq!(res.status, StatusCode::CONFLICT);
    let json = res.json();
    assert_eq!(json["code"], "PF-2011", "{json}");
    assert_eq!(json["title"], "conflict", "{json}");
    assert_eq!(json["request_id"], REQUEST_ID);
    assert!(
        !json["detail"].as_str().unwrap_or_default().contains(&email),
        "the body must not name the email: {json}"
    );
    // Still one user, still one mail, one success audit — but the
    // duplicate attempt was audited too.
    let user = users::by_email(&app.state.pool, &email)
        .await
        .unwrap()
        .expect("one user row");
    assert!(verify_password("correct horse battery staple", &user.password_hash).unwrap());
    let mail = email_outbox::unsent_for_template(&app.state.pool, VERIFY_EMAIL_TEMPLATE)
        .await
        .unwrap();
    assert_eq!(
        mail.iter()
            .filter(|row| row.payload["user_id"] == json!(user.id))
            .count(),
        1
    );
    let successes = audit_logs::list_for_target(&app.state.pool, "user", &user.id.to_string())
        .await
        .unwrap();
    assert_eq!(successes.len(), 1);
}

#[tokio::test]
async fn disposable_domain_is_a_422_field_error() {
    let app = register_app().await;
    let email = format!("someone-{}@mailinator.com", Uuid::now_v7());
    let res = app
        .request(post_register(&email, "correct horse battery staple", "v1"))
        .await;
    assert_eq!(res.status, StatusCode::UNPROCESSABLE_ENTITY);
    let json = res.json();
    assert_eq!(json["code"], "PF-2002", "{json}");
    let fields = json["field_errors"].as_array().expect("field errors");
    assert_eq!(fields.len(), 1);
    assert_eq!(fields[0]["field"], "email");
    assert_eq!(fields[0]["code"], "disposable_email");
    // Nothing was created.
    assert!(users::by_email(&app.state.pool, &email)
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn wrong_tos_version_is_a_422_field_error() {
    let app = register_app().await;
    let res = app
        .request(post_register(
            &unique_email(),
            "correct horse battery staple",
            "v0",
        ))
        .await;
    assert_eq!(res.status, StatusCode::UNPROCESSABLE_ENTITY);
    let json = res.json();
    assert_eq!(json["code"], "PF-2002", "{json}");
    let fields = json["field_errors"].as_array().expect("field errors");
    assert_eq!(fields.len(), 1);
    assert_eq!(fields[0]["field"], "accept_tos_version");
    assert_eq!(fields[0]["code"], "tos_version_mismatch");
}

#[tokio::test]
async fn structural_validation_collects_every_failure() {
    let app = register_app().await;
    let res = app
        .request(post_register("not-an-email", "short", ""))
        .await;
    assert_eq!(res.status, StatusCode::UNPROCESSABLE_ENTITY);
    let json = res.json();
    assert_eq!(json["code"], "PF-2002", "{json}");
    let fields = json["field_errors"].as_array().expect("field errors");
    assert_eq!(fields.len(), 3, "{json}");
    assert_eq!(fields[0]["field"], "email");
    assert_eq!(fields[0]["code"], "invalid_email");
    assert_eq!(fields[1]["field"], "password");
    assert_eq!(fields[1]["code"], "too_short");
    assert_eq!(fields[2]["field"], "accept_tos_version");
    assert_eq!(fields[2]["code"], "required");
}

#[tokio::test]
async fn email_is_normalized_before_storage() {
    let app = register_app().await;
    let local = format!("Alice-{}", Uuid::now_v7());
    let res = app
        .request(post_register(
            &format!("  {local}@Example.COM "),
            "correct horse battery staple",
            "v1",
        ))
        .await;
    assert_eq!(res.status, StatusCode::CREATED);
    let json = res.json();
    assert_eq!(
        json["email"],
        format!("{}@example.com", local.to_lowercase()),
        "{json}"
    );
}

#[tokio::test]
async fn malformed_json_is_a_plain_422() {
    let app = register_app().await;
    let req = Request::builder()
        .method(Method::POST)
        .uri("/v1/auth/register")
        .header("x-request-id", REQUEST_ID)
        .header("content-type", "application/json")
        .body(axum::body::Body::from("{not json"))
        .unwrap();
    let res = app.request(req).await;
    assert_eq!(res.status, StatusCode::UNPROCESSABLE_ENTITY);
    let json = res.json();
    assert_eq!(json["code"], "PF-2002", "{json}");
    assert!(
        json.get("field_errors").is_none(),
        "nothing parsed, no field errors"
    );
}

#[tokio::test]
async fn settings_defaults_are_the_owasp_values() {
    // The register path uses the real defaults unless the test
    // overrides them — prove the defaults are what the config
    // test claims (a drift here would silently weaken hashing).
    let settings = Settings::default();
    assert_eq!(settings.argon2_m_cost_kib, 19_456);
    assert_eq!(settings.argon2_t_cost, 2);
    assert_eq!(settings.argon2_p_cost, 1);
    assert_eq!(settings.tos_current_version, "v1");
}

#[tokio::test]
async fn register_email_rate_limit_trips_with_headers() {
    // P1-020: the per-email bucket (5 burst / 0 refill here)
    // bounds creation spam per address. Six attempts for the
    // same email: the first creates the user (201), the next
    // four are duplicates (409 — a duplicate IS a registration
    // attempt and spends the bucket), and the sixth is denied
    // 429 PF-3007 with Retry-After and the trio, the Limit
    // naming the register-email bucket (5). A denied attempt
    // creates no user — there is only the one from the first
    // attempt.
    let app = register_app_with_email_limit().await;
    let email = unique_email();
    let res = app
        .request(post_register(&email, "correct horse battery staple", "v1"))
        .await;
    assert_eq!(res.status, StatusCode::CREATED, "{}", res.text());
    for i in 1..=4 {
        let res = app
            .request(post_register(&email, "correct horse battery staple", "v1"))
            .await;
        assert_eq!(
            res.status,
            StatusCode::CONFLICT,
            "duplicate {i} must be 409: {}",
            res.text()
        );
    }
    let res = app
        .request(post_register(&email, "correct horse battery staple", "v1"))
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
        Some("5"),
        "the Limit names the register-email bucket (burst 5)"
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
    let users = users::by_email(&app.state.pool, &email)
        .await
        .expect("user lookup");
    assert!(users.is_some(), "the first attempt created the user");
}
